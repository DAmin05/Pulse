//! Approximate nearest-neighbor search over L2-normalized vectors.
//!
//! [`Hnsw`] is a deterministic Hierarchical Navigable Small World graph
//! (Malkov & Yashunin, 2016): level assignment uses a seeded RNG, and every
//! priority queue orders by `(distance, key)`, so the same inserts in the same
//! order always build the same graph and return the same results. That is what
//! lets the Story Processor replay a log and reproduce its output exactly.
//! [`BruteForce`] is the exact oracle used to measure recall.

use std::cmp::{Ordering, Reverse};
use std::collections::{BinaryHeap, HashSet};

pub trait VectorIndex {
    /// Adds a vector under a caller-chosen key. Keys must be unique.
    fn insert(&mut self, key: u32, vector: &[f32]);
    /// Up to `k` nearest keys with cosine similarity, most similar first.
    fn search(&self, query: &[f32], k: usize) -> Vec<(u32, f32)>;
    fn vector(&self, key: u32) -> Option<&[f32]>;
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[inline]
pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Cosine distance for unit vectors, with a total order (ties broken by key).
#[derive(Clone, Copy, PartialEq)]
struct Scored {
    dist: f32,
    node: u32,
}

impl Eq for Scored {}

impl Ord for Scored {
    fn cmp(&self, other: &Self) -> Ordering {
        self.dist
            .total_cmp(&other.dist)
            .then(self.node.cmp(&other.node))
    }
}

impl PartialOrd for Scored {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

// ---------------------------------------------------------------------------

/// Exact search by scanning every vector.
pub struct BruteForce {
    dim: usize,
    keys: Vec<u32>,
    vectors: Vec<f32>,
}

impl BruteForce {
    pub fn new(dim: usize) -> Self {
        Self {
            dim,
            keys: Vec::new(),
            vectors: Vec::new(),
        }
    }
}

impl VectorIndex for BruteForce {
    fn insert(&mut self, key: u32, vector: &[f32]) {
        assert_eq!(vector.len(), self.dim);
        self.keys.push(key);
        self.vectors.extend_from_slice(vector);
    }

    fn search(&self, query: &[f32], k: usize) -> Vec<(u32, f32)> {
        let mut scored: Vec<Scored> = self
            .vectors
            .chunks_exact(self.dim)
            .zip(&self.keys)
            .map(|(v, &key)| Scored {
                dist: 1.0 - dot(query, v),
                node: key,
            })
            .collect();
        scored.sort_unstable();
        scored
            .into_iter()
            .take(k)
            .map(|s| (s.node, 1.0 - s.dist))
            .collect()
    }

    fn vector(&self, key: u32) -> Option<&[f32]> {
        let i = self.keys.iter().position(|&k| k == key)?;
        Some(&self.vectors[i * self.dim..(i + 1) * self.dim])
    }

    fn len(&self) -> usize {
        self.keys.len()
    }
}

// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct HnswParams {
    /// Links per node on upper layers.
    pub m: usize,
    /// Links per node on layer 0 (conventionally 2·M).
    pub m0: usize,
    pub ef_construction: usize,
    pub ef_search: usize,
    pub seed: u64,
}

impl Default for HnswParams {
    fn default() -> Self {
        Self {
            m: 16,
            m0: 32,
            ef_construction: 128,
            ef_search: 64,
            seed: 0x4853_574e_5055_4c53, // "HNSWPULS"
        }
    }
}

const MAX_LEVEL: usize = 16;

pub struct Hnsw {
    dim: usize,
    params: HnswParams,
    /// External key per node; nodes are dense `0..n` in insertion order.
    keys: Vec<u32>,
    key_to_node: std::collections::HashMap<u32, u32>,
    vectors: Vec<f32>,
    /// links[node][layer] = neighbor nodes.
    links: Vec<Vec<Vec<u32>>>,
    entry: Option<u32>,
    max_level: usize,
    rng: u64,
}

impl Hnsw {
    pub fn new(dim: usize, params: HnswParams) -> Self {
        let rng = params.seed | 1;
        Self {
            dim,
            params,
            keys: Vec::new(),
            key_to_node: std::collections::HashMap::new(),
            vectors: Vec::new(),
            links: Vec::new(),
            entry: None,
            max_level: 0,
            rng,
        }
    }

    pub fn params(&self) -> &HnswParams {
        &self.params
    }

    /// A fresh index holding only the keys `keep` accepts, inserted in their
    /// original order. HNSW has no cheap delete, so eviction rebuilds.
    pub fn rebuilt(&self, mut keep: impl FnMut(u32) -> bool) -> Self {
        let mut next = Self::new(self.dim, self.params.clone());
        for (node, &key) in self.keys.iter().enumerate() {
            if keep(key) {
                next.insert(key, self.node_vector(node as u32));
            }
        }
        next
    }

    fn node_vector(&self, node: u32) -> &[f32] {
        let i = node as usize * self.dim;
        &self.vectors[i..i + self.dim]
    }

    fn dist(&self, query: &[f32], node: u32) -> f32 {
        1.0 - dot(query, self.node_vector(node))
    }

    fn random_level(&mut self) -> usize {
        // xorshift64*, then the standard geometric level distribution.
        self.rng ^= self.rng >> 12;
        self.rng ^= self.rng << 25;
        self.rng ^= self.rng >> 27;
        let bits = self.rng.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11;
        let u = (bits as f64 + 1.0) / (1u64 << 53) as f64; // (0, 1]
        let ml = 1.0 / (self.params.m as f64).ln();
        ((-u.ln() * ml) as usize).min(MAX_LEVEL)
    }

    /// Beam search on one layer. Returns up to `ef` nodes, nearest first.
    fn search_layer(&self, query: &[f32], entry: &[u32], ef: usize, layer: usize) -> Vec<Scored> {
        let mut visited: HashSet<u32> = entry.iter().copied().collect();
        let mut candidates: BinaryHeap<Reverse<Scored>> = BinaryHeap::new();
        let mut results: BinaryHeap<Scored> = BinaryHeap::new();
        for &node in entry {
            let s = Scored {
                dist: self.dist(query, node),
                node,
            };
            candidates.push(Reverse(s));
            results.push(s);
        }
        while results.len() > ef {
            results.pop();
        }

        while let Some(Reverse(current)) = candidates.pop() {
            let worst = *results.peek().expect("results never empty here");
            if current > worst && results.len() >= ef {
                break;
            }
            for &neighbor in &self.links[current.node as usize][layer] {
                if !visited.insert(neighbor) {
                    continue;
                }
                let s = Scored {
                    dist: self.dist(query, neighbor),
                    node: neighbor,
                };
                if results.len() < ef || s < *results.peek().expect("non-empty") {
                    candidates.push(Reverse(s));
                    results.push(s);
                    if results.len() > ef {
                        results.pop();
                    }
                }
            }
        }
        results.into_sorted_vec()
    }

    /// Neighbor selection heuristic (paper, Algorithm 4): keep a candidate only
    /// if it is closer to the base than to any already-kept neighbor, so links
    /// spread across clusters instead of piling into one. Pruned candidates fill
    /// any remaining slots (keepPrunedConnections).
    fn select_neighbors(&self, candidates: &[Scored], m: usize) -> Vec<u32> {
        let mut selected: Vec<Scored> = Vec::with_capacity(m);
        let mut pruned = Vec::new();
        for &c in candidates {
            if selected.len() >= m {
                break;
            }
            let c_vec = self.node_vector(c.node);
            let diverse = selected
                .iter()
                .all(|s| 1.0 - dot(c_vec, self.node_vector(s.node)) > c.dist);
            if diverse {
                selected.push(c);
            } else {
                pruned.push(c);
            }
        }
        for c in pruned {
            if selected.len() >= m {
                break;
            }
            selected.push(c);
        }
        selected.into_iter().map(|s| s.node).collect()
    }

    fn max_links(&self, layer: usize) -> usize {
        if layer == 0 {
            self.params.m0
        } else {
            self.params.m
        }
    }
}

impl VectorIndex for Hnsw {
    fn insert(&mut self, key: u32, vector: &[f32]) {
        assert_eq!(vector.len(), self.dim, "dimension mismatch");
        assert!(!self.key_to_node.contains_key(&key), "duplicate key {key}");
        let node = self.keys.len() as u32;
        let level = self.random_level();
        self.keys.push(key);
        self.key_to_node.insert(key, node);
        self.vectors.extend_from_slice(vector);
        self.links.push(vec![Vec::new(); level + 1]);

        let Some(entry) = self.entry else {
            self.entry = Some(node);
            self.max_level = level;
            return;
        };

        // Greedy descent through layers above the new node's level.
        let mut entry_points = vec![entry];
        for layer in (level + 1..=self.max_level).rev() {
            entry_points = vec![self.search_layer(vector, &entry_points, 1, layer)[0].node];
        }

        for layer in (0..=level.min(self.max_level)).rev() {
            let candidates =
                self.search_layer(vector, &entry_points, self.params.ef_construction, layer);
            let neighbors = self.select_neighbors(&candidates, self.params.m);
            self.links[node as usize][layer] = neighbors.clone();

            for neighbor in neighbors {
                self.links[neighbor as usize][layer].push(node);
                if self.links[neighbor as usize][layer].len() > self.max_links(layer) {
                    // Shrink the neighbor's list with the same heuristic.
                    let base = self.node_vector(neighbor).to_vec();
                    let mut scored: Vec<Scored> = self.links[neighbor as usize][layer]
                        .iter()
                        .map(|&n| Scored {
                            dist: self.dist(&base, n),
                            node: n,
                        })
                        .collect();
                    scored.sort_unstable();
                    let kept = self.select_neighbors(&scored, self.max_links(layer));
                    self.links[neighbor as usize][layer] = kept;
                }
            }
            entry_points = candidates.iter().map(|s| s.node).collect();
        }

        if level > self.max_level {
            self.entry = Some(node);
            self.max_level = level;
        }
    }

    fn search(&self, query: &[f32], k: usize) -> Vec<(u32, f32)> {
        let Some(entry) = self.entry else {
            return Vec::new();
        };
        let mut entry_points = vec![entry];
        for layer in (1..=self.max_level).rev() {
            entry_points = vec![self.search_layer(query, &entry_points, 1, layer)[0].node];
        }
        self.search_layer(query, &entry_points, self.params.ef_search.max(k), 0)
            .into_iter()
            .take(k)
            .map(|s| (self.keys[s.node as usize], 1.0 - s.dist))
            .collect()
    }

    fn vector(&self, key: u32) -> Option<&[f32]> {
        self.key_to_node.get(&key).map(|&n| self.node_vector(n))
    }

    fn len(&self) -> usize {
        self.keys.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Clustered unit vectors, like news embeddings: many tight groups.
    fn dataset(n: usize, dim: usize, clusters: usize, seed: u64) -> Vec<Vec<f32>> {
        let mut s = seed;
        let mut rand = move || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 11) as f32 / (1u64 << 53) as f32 - 0.5
        };
        let centers: Vec<Vec<f32>> = (0..clusters)
            .map(|_| (0..dim).map(|_| rand()).collect())
            .collect();
        (0..n)
            .map(|i| {
                let c = &centers[i % clusters];
                let mut v: Vec<f32> = c.iter().map(|x| x + 0.3 * rand()).collect();
                let norm = dot(&v, &v).sqrt();
                v.iter_mut().for_each(|x| *x /= norm);
                v
            })
            .collect()
    }

    fn recall(
        index: &impl VectorIndex,
        oracle: &BruteForce,
        queries: &[Vec<f32>],
        k: usize,
    ) -> f32 {
        let mut hits = 0;
        for q in queries {
            let truth: HashSet<u32> = oracle.search(q, k).into_iter().map(|(id, _)| id).collect();
            hits += index
                .search(q, k)
                .into_iter()
                .filter(|(id, _)| truth.contains(id))
                .count();
        }
        hits as f32 / (queries.len() * k) as f32
    }

    #[test]
    fn high_recall_on_clustered_data() {
        let data = dataset(3000, 64, 150, 7);
        let (indexed, queries) = data.split_at(2800);
        let mut hnsw = Hnsw::new(64, HnswParams::default());
        let mut oracle = BruteForce::new(64);
        for (i, v) in indexed.iter().enumerate() {
            hnsw.insert(i as u32, v);
            oracle.insert(i as u32, v);
        }
        let r = recall(&hnsw, &oracle, queries, 10);
        assert!(r >= 0.95, "recall@10 = {r}");
    }

    #[test]
    fn deterministic_build_and_search() {
        let data = dataset(800, 32, 40, 3);
        let build = || {
            let mut h = Hnsw::new(32, HnswParams::default());
            for (i, v) in data.iter().enumerate() {
                h.insert(i as u32 * 7, v); // arbitrary keys
            }
            h
        };
        let (a, b) = (build(), build());
        assert_eq!(a.links, b.links);
        for q in data.iter().step_by(37) {
            assert_eq!(a.search(q, 10), b.search(q, 10));
        }
    }

    #[test]
    fn self_query_and_rebuild() {
        let data = dataset(500, 16, 20, 11);
        let mut h = Hnsw::new(16, HnswParams::default());
        for (i, v) in data.iter().enumerate() {
            h.insert(i as u32, v);
        }
        assert_eq!(h.search(&data[42], 1)[0].0, 42);
        assert_eq!(h.vector(42), Some(data[42].as_slice()));

        let even = h.rebuilt(|k| k % 2 == 0);
        assert_eq!(even.len(), 250);
        assert!(even.search(&data[43], 5).iter().all(|(k, _)| k % 2 == 0));
        assert_eq!(even.search(&data[42], 1)[0].0, 42);
    }

    #[test]
    fn empty_index() {
        let h = Hnsw::new(4, HnswParams::default());
        assert!(h.search(&[1.0, 0.0, 0.0, 0.0], 3).is_empty());
    }
}

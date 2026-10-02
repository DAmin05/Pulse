//! Story lineage: detecting when one story becomes several narratives (split)
//! or several stories turn out to be one (merge).
//!
//! Runs every `lineage_every_inputs` inputs, on stories that changed since the
//! last check ("dirty"); unchanged candidates keep their standing, since their
//! evidence is unchanged.
//!
//! - **Split:** deterministic 2-means over a story's indexed members (seeded
//!   with the member farthest from the centroid, then the one farthest from
//!   that). If both halves have ≥ `split_min_component` members and their
//!   centroids are less similar than `split_max_similarity`, the story is a
//!   split candidate.
//! - **Merge:** two established stories whose centroids are at least
//!   `merge_min_similarity` alike are a merge candidate. The smaller story
//!   joins the larger.
//!
//! Anti-flapping: a candidate must qualify on `lineage_confirm_checks`
//! consecutive checks; the merge threshold sits well above the split threshold
//! (a merged pair can't immediately qualify to split); and stories touched by a
//! split or merge sit out `lineage_cooldown_checks` checks.

use std::collections::{BTreeMap, BTreeSet};

use pulse_core::proto::v1::{
    CloseReason, SplitChild, StoryClosed, StoryCreated, StoryMerged, StorySplit, story_event::Kind,
};
use sha2::{Digest, Sha256};

use crate::ann::{VectorIndex, dot};
use crate::engine::{Emitter, Engine, Story};

const KMEANS_ITERATIONS: usize = 12;

/// A proposed split: two groups of indexed member keys and how alike they are.
#[derive(Debug, Clone)]
pub struct SplitPlan {
    pub groups: [Vec<u32>; 2],
    pub similarity: f32,
}

fn normalize(mut v: Vec<f32>) -> Vec<f32> {
    let norm = dot(&v, &v).sqrt().max(1e-12);
    v.iter_mut().for_each(|x| *x /= norm);
    v
}

impl Engine {
    fn vector_of(&self, key: u32) -> &[f32] {
        self.s
            .ann
            .as_ref()
            .and_then(|a| a.vector(key))
            .expect("open stories' indexed members are in the index")
    }

    fn mean_direction(&self, keys: &[u32]) -> Vec<f32> {
        let dim = self.s.dim.unwrap_or(0);
        let mut sum = vec![0.0; dim];
        for &k in keys {
            sum.iter_mut()
                .zip(self.vector_of(k))
                .for_each(|(s, x)| *s += x);
        }
        normalize(sum)
    }

    /// Deterministic spherical 2-means over the story's indexed members.
    pub fn split_plan(&self, story: &Story) -> Option<SplitPlan> {
        let keys = &story.originals;
        if keys.len() < 2 * self.cfg.split_min_component {
            return None;
        }
        // Seeds: the member least like the centroid, then the member least like it.
        // `min_by` with (similarity, key) keeps ties deterministic.
        let farthest_from = |target: &[f32], skip: Option<u32>| {
            keys.iter()
                .copied()
                .filter(|&k| Some(k) != skip)
                .min_by(|&a, &b| {
                    dot(self.vector_of(a), target)
                        .total_cmp(&dot(self.vector_of(b), target))
                        .then(a.cmp(&b))
                })
                .expect("non-empty")
        };
        let seed_a = farthest_from(&story.centroid(), None);
        let seed_b = farthest_from(self.vector_of(seed_a), Some(seed_a));
        let mut centers = [
            self.vector_of(seed_a).to_vec(),
            self.vector_of(seed_b).to_vec(),
        ];

        let mut assignment: Vec<usize> = Vec::new();
        for _ in 0..KMEANS_ITERATIONS {
            let next: Vec<usize> = keys
                .iter()
                .map(|&k| {
                    let v = self.vector_of(k);
                    usize::from(dot(v, &centers[1]) > dot(v, &centers[0]))
                })
                .collect();
            if next == assignment {
                break;
            }
            assignment = next;
            for (g, center) in centers.iter_mut().enumerate() {
                let members: Vec<u32> = keys
                    .iter()
                    .zip(&assignment)
                    .filter(|(_, a)| **a == g)
                    .map(|(&k, _)| k)
                    .collect();
                if members.is_empty() {
                    return None;
                }
                *center = self.mean_direction(&members);
            }
        }

        let mut groups: [Vec<u32>; 2] = [Vec::new(), Vec::new()];
        for (&k, &g) in keys.iter().zip(&assignment) {
            groups[g].push(k);
        }
        if groups
            .iter()
            .any(|g| g.len() < self.cfg.split_min_component)
        {
            return None;
        }
        Some(SplitPlan {
            similarity: dot(&centers[0], &centers[1]),
            groups,
        })
    }

    pub(crate) fn lineage(&mut self, out: &mut Emitter) {
        self.s.lineage_checks += 1;
        let check = self.s.lineage_checks;
        let dirty = std::mem::take(&mut self.s.dirty);
        self.detect_splits(check, &dirty, out);
        self.detect_merges(check, &dirty, out);
    }

    fn split_eligible(&self, key: u32, check: u64) -> bool {
        self.s.stories.get(&key).is_some_and(|s| {
            s.originals.len() >= self.cfg.split_min_size && s.cooldown_until <= check
        })
    }

    fn merge_eligible(&self, key: u32, check: u64) -> bool {
        self.s.stories.get(&key).is_some_and(|s| {
            s.originals.len() >= self.cfg.merge_min_size && s.cooldown_until <= check
        })
    }

    fn detect_splits(&mut self, check: u64, dirty: &BTreeSet<u32>, out: &mut Emitter) {
        let old = std::mem::take(&mut self.s.pending_splits);
        let mut pending: BTreeMap<u32, u32> = BTreeMap::new();
        // Unchanged candidates keep qualifying: their evidence hasn't changed.
        for (&key, &n) in &old {
            if !dirty.contains(&key) && self.split_eligible(key, check) {
                pending.insert(key, n + 1);
            }
        }
        for &key in dirty {
            if !self.split_eligible(key, check) {
                continue;
            }
            let qualifies = self
                .split_plan(&self.s.stories[&key])
                .is_some_and(|p| p.similarity < self.cfg.split_max_similarity);
            if qualifies {
                pending.insert(key, old.get(&key).copied().unwrap_or(0) + 1);
            }
        }

        let confirmed: Vec<u32> = pending
            .iter()
            .filter(|&(_, &n)| n >= self.cfg.lineage_confirm_checks)
            .map(|(&k, _)| k)
            .collect();
        for key in &confirmed {
            pending.remove(key);
        }
        self.s.pending_splits = pending;
        for key in confirmed {
            // Recomputed here; identical to the plan that qualified (same state).
            if let Some(plan) = self.split_plan(&self.s.stories[&key]) {
                self.apply_split(key, plan, check, out);
            }
        }
    }

    /// Title word → number of the story's members whose title contains it.
    fn word_counts(&self, story: &Story) -> BTreeMap<u64, u32> {
        let mut counts = BTreeMap::new();
        for m in &story.members {
            for w in crate::engine::title_words(&self.s.articles[m].title) {
                *counts.entry(w).or_insert(0) += 1;
            }
        }
        counts
    }

    /// True when the stories share a title word concentrated in them: at least
    /// `anchor_min_concentration` of live articles using it belong to the pair.
    pub fn share_anchor(&self, a: &Story, b: &Story) -> bool {
        let (ca, cb) = (self.word_counts(a), self.word_counts(b));
        ca.iter().any(|(w, &na)| {
            cb.get(w).is_some_and(|&nb| {
                let df = self.s.word_df.get(w).copied().unwrap_or(u32::MAX);
                (na + nb) as f32 >= self.cfg.anchor_min_concentration * df as f32
            })
        })
    }

    /// Whether a pair at `similarity` qualifies: the anchored threshold if they
    /// share an anchor word, otherwise the stricter unanchored one.
    fn merge_qualifies(&self, a: u32, b: u32, similarity: f32) -> bool {
        if similarity >= self.cfg.merge_min_similarity_unanchored {
            return true;
        }
        similarity >= self.cfg.merge_min_similarity
            && self.share_anchor(&self.s.stories[&a], &self.s.stories[&b])
    }

    fn detect_merges(&mut self, check: u64, dirty: &BTreeSet<u32>, out: &mut Emitter) {
        let eligible: Vec<u32> = self
            .s
            .stories
            .keys()
            .copied()
            .filter(|&k| self.merge_eligible(k, check))
            .collect();
        let centroids: BTreeMap<u32, Vec<f32>> = eligible
            .iter()
            .map(|&k| (k, self.s.stories[&k].centroid()))
            .collect();

        let old = std::mem::take(&mut self.s.pending_merges);
        let mut pending: BTreeMap<(u32, u32), u32> = BTreeMap::new();
        for (&(a, b), &n) in &old {
            if !dirty.contains(&a)
                && !dirty.contains(&b)
                && centroids.contains_key(&a)
                && centroids.contains_key(&b)
            {
                pending.insert((a, b), n + 1);
            }
        }
        for &d in dirty.iter().filter(|d| centroids.contains_key(d)) {
            for &other in &eligible {
                let pair = (d.min(other), d.max(other));
                if other == d || pending.contains_key(&pair) {
                    continue;
                }
                let similarity = dot(&centroids[&d], &centroids[&other]);
                if similarity >= self.cfg.merge_min_similarity
                    && self.merge_qualifies(pair.0, pair.1, similarity)
                {
                    pending.insert(pair, old.get(&pair).copied().unwrap_or(0) + 1);
                }
            }
        }

        let mut confirmed: Vec<((u32, u32), f32)> = pending
            .iter()
            .filter(|&(_, &n)| n >= self.cfg.lineage_confirm_checks)
            .map(|(&(a, b), _)| ((a, b), dot(&centroids[&a], &centroids[&b])))
            .collect();
        for (pair, _) in &confirmed {
            pending.remove(pair);
        }
        self.s.pending_merges = pending;

        // Most similar first; each story takes part in at most one merge per check.
        confirmed.sort_by(|x, y| y.1.total_cmp(&x.1).then(x.0.cmp(&y.0)));
        let mut touched = BTreeSet::new();
        for ((a, b), _) in confirmed {
            if touched.contains(&a) || touched.contains(&b) {
                continue;
            }
            touched.extend([a, b]);
            self.apply_merge(a, b, check, out);
        }
    }

    /// Follows `duplicate_of` links to the indexed article a copy derives from.
    fn root_original(&self, mut key: u32) -> u32 {
        while let Some(original) = self.s.articles.get(&key).and_then(|a| a.duplicate_of) {
            key = original;
        }
        key
    }

    /// Builds a story from members, recomputing every derived field.
    fn build_story(
        &self,
        id: String,
        mut members: Vec<u32>,
        mut originals: Vec<u32>,
        parents: Vec<String>,
        merged_from: Vec<String>,
        cooldown_until: u64,
    ) -> Story {
        members.sort_unstable();
        originals.sort_unstable();
        let dim = self.s.dim.unwrap_or(0);
        let mut centroid_sum = vec![0.0; dim];
        for &k in &originals {
            centroid_sum
                .iter_mut()
                .zip(self.vector_of(k))
                .for_each(|(s, x)| *s += x);
        }
        let centroid = normalize(centroid_sum.clone());
        let fits: Vec<(u32, f32)> = originals
            .iter()
            .map(|&k| (k, dot(self.vector_of(k), &centroid)))
            .collect();
        let headline = fits
            .iter()
            .fold(None, |best: Option<(u32, f32)>, &(k, s)| match best {
                Some((_, bs)) if bs >= s => best,
                _ => Some((k, s)),
            })
            .map_or(members[0], |(k, _)| k);
        let articles = members.iter().map(|m| &self.s.articles[m]);
        Story {
            id,
            sources: articles.clone().map(|a| a.source_id.clone()).collect(),
            langs: articles.clone().map(|a| a.lang.clone()).collect(),
            first_event_ms: articles.clone().map(|a| a.event_time_ms).min().unwrap_or(0),
            last_event_ms: articles.map(|a| a.event_time_ms).max().unwrap_or(0),
            cohesion_sum: fits.iter().map(|(_, s)| s).sum(),
            cohesion_n: fits.len() as u32,
            members,
            originals,
            centroid_sum,
            headline,
            parents,
            merged_from,
            cooldown_until,
        }
    }

    fn apply_split(&mut self, parent_key: u32, plan: SplitPlan, check: u64, out: &mut Emitter) {
        let parent = self
            .s
            .stories
            .remove(&parent_key)
            .expect("candidate exists");
        let group_of: BTreeMap<u32, usize> = plan
            .groups
            .iter()
            .enumerate()
            .flat_map(|(g, keys)| keys.iter().map(move |&k| (k, g)))
            .collect();
        // Duplicates follow the original they copy.
        let mut members: [Vec<u32>; 2] = [Vec::new(), Vec::new()];
        for &m in &parent.members {
            let g = group_of.get(&self.root_original(m)).copied().unwrap_or(0);
            members[g].push(m);
        }
        let mut groups: Vec<(Vec<u32>, Vec<u32>)> = members.into_iter().zip(plan.groups).collect();
        groups.sort_by_key(|(m, _)| m.iter().min().copied());

        let cooldown = check + self.cfg.lineage_cooldown_checks;
        let mut children = Vec::new();
        for (members, originals) in groups {
            let child_key = *members.iter().min().expect("non-empty group");
            let seed = &self.s.articles[&child_key];
            let mut h = Sha256::new();
            h.update(parent.id.as_bytes());
            h.update(b":");
            h.update(seed.id.as_bytes());
            let id = format!("st_{}", hex::encode(&h.finalize()[..8]));
            let story = self.build_story(
                id.clone(),
                members,
                originals,
                vec![parent.id.clone()],
                Vec::new(),
                cooldown,
            );
            for m in &story.members {
                self.s.articles.get_mut(m).expect("member").story = child_key;
            }
            children.push((child_key, story));
        }

        out.emit(Kind::Split(StorySplit {
            parent_story_id: parent.id.clone(),
            children: children
                .iter()
                .map(|(_, child)| SplitChild {
                    story_id: child.id.clone(),
                    article_ids: child
                        .members
                        .iter()
                        .map(|m| self.s.articles[m].id.clone())
                        .collect(),
                })
                .collect(),
        }));
        for (child_key, child) in children {
            let headline = &self.s.articles[&child.headline];
            out.emit(Kind::Created(StoryCreated {
                story_id: child.id.clone(),
                seed_article_id: self.s.articles[&child_key].id.clone(),
                headline: headline.title.clone(),
                lang: headline.lang.clone(),
                parent_story_ids: child.parents.clone(),
            }));
            self.s.stories.insert(child_key, child);
            out.emit(self.story_updated(child_key));
        }
        out.emit(Kind::Closed(StoryClosed {
            story_id: parent.id,
            reason: CloseReason::Split as i32,
        }));
        self.s
            .pending_merges
            .retain(|&(a, b), _| a != parent_key && b != parent_key);
    }

    fn apply_merge(&mut self, a: u32, b: u32, check: u64, out: &mut Emitter) {
        let size = |k: u32| self.s.stories[&k].originals.len();
        // The larger story absorbs the smaller; ties to the older (lower key).
        let (target_key, source_key) = if size(a) >= size(b) { (a, b) } else { (b, a) };
        let source = self
            .s
            .stories
            .remove(&source_key)
            .expect("candidate exists");
        let target = self
            .s
            .stories
            .remove(&target_key)
            .expect("candidate exists");
        for m in &source.members {
            self.s.articles.get_mut(m).expect("member").story = target_key;
        }
        let mut merged_from = target.merged_from;
        merged_from.push(source.id.clone());
        let story = self.build_story(
            target.id.clone(),
            [target.members, source.members].concat(),
            [target.originals, source.originals].concat(),
            target.parents,
            merged_from,
            check + self.cfg.lineage_cooldown_checks,
        );
        self.s.stories.insert(target_key, story);

        out.emit(Kind::Merged(StoryMerged {
            source_story_ids: vec![source.id.clone()],
            target_story_id: target.id,
        }));
        out.emit(Kind::Closed(StoryClosed {
            story_id: source.id,
            reason: CloseReason::Merged as i32,
        }));
        out.emit(self.story_updated(target_key));
        self.s.pending_splits.remove(&source_key);
        self.s.pending_splits.remove(&target_key);
        self.s
            .pending_merges
            .retain(|&(x, y), _| x != source_key && y != source_key);
    }
}

#[cfg(test)]
mod tests {
    use crate::ann::dot;
    use crate::engine::{Config, Engine, Outcome};
    use pulse_core::proto::v1::story_event::Kind;
    use pulse_core::proto::v1::{Article, CloseReason, EmbeddedArticle, StoryEvent};

    fn input(i: usize, v: &[f32]) -> EmbeddedArticle {
        let n = dot(v, v).sqrt();
        EmbeddedArticle {
            article: Some(Article {
                id: format!("a{i}"),
                // Distinct words so MinHash never treats them as copies.
                title: format!("headline {i} {}", "xyzw".repeat(i % 7 + 1)),
                source_id: format!("s{}", i % 3),
                lang: "en".into(),
                published_at_ms: 1_000 + i as i64,
                ..Default::default()
            }),
            vector: v.iter().map(|x| x / n).collect(),
            model_version: "t".into(),
        }
    }

    /// Two narratives: around axis 0 and axis 1, with small jitter.
    fn narrative(axis: usize, i: usize) -> Vec<f32> {
        let mut v = vec![0.0; 4];
        v[axis] = 1.0;
        v[2] = 0.05 * (i % 3) as f32;
        v[3] = 0.05 * (i % 2) as f32;
        v
    }

    fn kinds(events: &[StoryEvent]) -> Vec<&'static str> {
        events
            .iter()
            .filter_map(|e| match e.kind.as_ref()? {
                Kind::Split(_) => Some("split"),
                Kind::Merged(_) => Some("merged"),
                Kind::Closed(c) if c.reason == CloseReason::Split as i32 => Some("closed:split"),
                Kind::Closed(c) if c.reason == CloseReason::Merged as i32 => Some("closed:merged"),
                Kind::Created(c) if !c.parent_story_ids.is_empty() => Some("created:child"),
                _ => None,
            })
            .collect()
    }

    /// Everything joins one story (permissive thresholds), then lineage notices
    /// it holds two narratives and splits it, after the confirmation check.
    #[test]
    fn splits_a_story_holding_two_narratives() {
        let mut e = Engine::new(Config {
            neighbor_similarity: -1.0,
            centroid_similarity: -1.0,
            min_votes_established: 1,
            cohesion_margin: None,
            lineage_every_inputs: 1,
            split_min_size: 8,
            ..Config::default()
        });
        let mut lineage = Vec::new();
        for i in 0..10 {
            let p = e.process(&input(i, &narrative(i % 2, i)), i as i64);
            assert_ne!(p.outcome, Outcome::Invalid);
            lineage.extend(p.events);
        }
        assert_eq!(
            kinds(&lineage),
            vec!["split", "created:child", "created:child", "closed:split"]
        );
        let split = lineage
            .iter()
            .find_map(|e| match &e.kind {
                Some(Kind::Split(s)) => Some(s.clone()),
                _ => None,
            })
            .unwrap();
        // Each child holds exactly one narrative (even vs odd article numbers).
        for child in &split.children {
            let parity: Vec<usize> = child
                .article_ids
                .iter()
                .map(|id| id[1..].parse::<usize>().unwrap() % 2)
                .collect();
            assert!(parity.windows(2).all(|w| w[0] == w[1]), "{parity:?}");
        }
        assert_eq!(e.stories().len(), 2);
        assert!(
            e.stories()
                .values()
                .all(|s| s.parents == vec![split.parent_story_id.clone()])
        );
        // Articles point at their child story.
        for (key, story) in e.stories() {
            assert!(story.members.iter().all(|m| e.articles()[m].story == *key));
        }
    }

    /// Two stories form separately (strict join thresholds) but are one event
    /// by centroid: they merge once confirmed, and cooldown prevents a split back.
    #[test]
    fn merges_stories_that_are_one_event_without_flapping() {
        let mut e = Engine::new(Config {
            neighbor_similarity: 0.9999,
            centroid_similarity: 0.9999,
            lineage_every_inputs: 1,
            merge_min_size: 1,
            merge_min_similarity: 0.95,
            merge_min_similarity_unanchored: 0.95,
            split_min_size: 2,
            split_min_component: 1,
            split_max_similarity: 0.999,
            ..Config::default()
        });
        let mut events = Vec::new();
        events.extend(e.process(&input(0, &[1.0, 0.10, 0.0, 0.0]), 0).events);
        events.extend(e.process(&input(1, &[1.0, 0.13, 0.0, 0.0]), 1).events);
        assert!(
            kinds(&events).is_empty(),
            "first check only marks the pair pending"
        );
        // A third, unrelated input triggers the confirming check.
        events.extend(e.process(&input(2, &[0.0, 0.0, 1.0, 0.0]), 2).events);
        assert_eq!(kinds(&events), vec!["merged", "closed:merged"]);
        let merged = e.stories().values().find(|s| s.members.len() == 2).unwrap();
        assert_eq!(merged.merged_from.len(), 1);

        // The merged story is on cooldown: no split for the next checks even
        // though the (deliberately loose) split threshold would qualify it.
        let mut later = Vec::new();
        for i in 3..6 {
            later.extend(e.process(&input(i, &[0.0, 0.0, 0.0, 1.0]), i as i64).events);
        }
        assert!(!kinds(&later).contains(&"split"));
    }

    #[test]
    fn single_narrative_never_splits() {
        let mut e = Engine::new(Config {
            neighbor_similarity: -1.0,
            centroid_similarity: -1.0,
            min_votes_established: 1,
            cohesion_margin: None,
            lineage_every_inputs: 1,
            ..Config::default()
        });
        let mut events = Vec::new();
        for i in 0..30 {
            events.extend(e.process(&input(i, &narrative(0, i)), i as i64).events);
        }
        assert!(kinds(&events).is_empty());
        assert_eq!(e.stories().len(), 1);
    }
}

//! Near-duplicate detection with MinHash + LSH banding.
//!
//! Text → normalized character 5-gram shingles → 128 MinHash values →
//! 16 bands of 8 rows. Two texts become candidates when any band matches, which
//! happens with high probability once their Jaccard similarity exceeds ~0.7.
//! Candidates are confirmed by the signature-estimated Jaccard (standard error
//! about 0.04 at 128 hashes).
//!
//! Character shingles work for languages written without spaces (zh, ja, th).
//! Everything is seeded and order-independent, so results are deterministic.

use std::collections::{BTreeMap, BTreeSet};

use xxhash_rust::xxh3::xxh3_64_with_seed;

pub const NUM_HASHES: usize = 128;
pub const BANDS: usize = 16;
pub const ROWS: usize = NUM_HASHES / BANDS;
const SHINGLE_CHARS: usize = 5;
const SHINGLE_SEED: u64 = 0x5055_4c53_455f_4d48; // "PULSE_MH"
/// Mersenne prime 2^61 - 1 for universal hashing.
const PRIME: u64 = (1 << 61) - 1;

pub type Signature = [u64; NUM_HASHES];

/// The 128 hash functions `h_i(x) = (a_i * x + b_i) mod p`.
pub struct MinHasher {
    a: [u64; NUM_HASHES],
    b: [u64; NUM_HASHES],
}

impl Default for MinHasher {
    fn default() -> Self {
        // SplitMix64 with a fixed seed: identical coefficients on every run.
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = || {
            state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        };
        let mut a = [0; NUM_HASHES];
        let mut b = [0; NUM_HASHES];
        for i in 0..NUM_HASHES {
            a[i] = next() % (PRIME - 1) + 1;
            b[i] = next() % PRIME;
        }
        Self { a, b }
    }
}

/// Lowercase, keep letters/digits, collapse everything else to single spaces.
pub fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = true;
    for c in text.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            out.push(c);
            space = false;
        } else if !space {
            out.push(' ');
            space = true;
        }
    }
    out.trim_end().to_owned()
}

/// Hashed character shingles. Texts shorter than one shingle hash as a whole.
pub fn shingles(text: &str) -> BTreeSet<u64> {
    let chars: Vec<char> = normalize(text).chars().collect();
    let mut set = BTreeSet::new();
    if chars.is_empty() {
        return set;
    }
    let mut buf = String::new();
    for window in chars.windows(SHINGLE_CHARS.min(chars.len())) {
        buf.clear();
        buf.extend(window);
        set.insert(xxh3_64_with_seed(buf.as_bytes(), SHINGLE_SEED));
    }
    set
}

impl MinHasher {
    /// `None` for texts with no shingles (nothing to compare).
    pub fn signature(&self, text: &str) -> Option<Signature> {
        let set = shingles(text);
        if set.is_empty() {
            return None;
        }
        let mut sig = [u64::MAX; NUM_HASHES];
        for &x in &set {
            let x = x % PRIME;
            for ((min, a), b) in sig.iter_mut().zip(&self.a).zip(&self.b) {
                let h = ((*a as u128 * x as u128 + *b as u128) % PRIME as u128) as u64;
                *min = (*min).min(h);
            }
        }
        Some(sig)
    }
}

/// Fraction of matching MinHash values: an unbiased estimate of Jaccard similarity.
pub fn estimate_jaccard(a: &Signature, b: &Signature) -> f32 {
    let equal = a.iter().zip(b).filter(|(x, y)| x == y).count();
    equal as f32 / NUM_HASHES as f32
}

fn band_key(sig: &Signature, band: usize) -> u64 {
    let rows = &sig[band * ROWS..(band + 1) * ROWS];
    let mut bytes = [0u8; ROWS * 8];
    for (i, v) in rows.iter().enumerate() {
        bytes[i * 8..(i + 1) * 8].copy_from_slice(&v.to_le_bytes());
    }
    xxh3_64_with_seed(&bytes, band as u64)
}

/// LSH index over signatures, keyed by caller-chosen ids (`u32`).
#[derive(Default)]
pub struct LshIndex {
    /// (band, key) → ids in insertion order.
    buckets: BTreeMap<(u8, u64), Vec<u32>>,
    signatures: BTreeMap<u32, Signature>,
}

impl LshIndex {
    pub fn insert(&mut self, id: u32, sig: Signature) {
        for band in 0..BANDS {
            self.buckets
                .entry((band as u8, band_key(&sig, band)))
                .or_default()
                .push(id);
        }
        self.signatures.insert(id, sig);
    }

    /// Best match with estimated Jaccard ≥ `threshold`. Ties go to the lowest id
    /// (the earliest article), which keeps the result deterministic.
    pub fn find_duplicate(&self, sig: &Signature, threshold: f32) -> Option<(u32, f32)> {
        let mut candidates = BTreeSet::new();
        for band in 0..BANDS {
            if let Some(ids) = self.buckets.get(&(band as u8, band_key(sig, band))) {
                candidates.extend(ids.iter().copied());
            }
        }
        candidates
            .into_iter()
            .map(|id| (id, estimate_jaccard(sig, &self.signatures[&id])))
            .filter(|&(_, j)| j >= threshold)
            .fold(None, |best: Option<(u32, f32)>, c| match best {
                Some(b) if b.1 >= c.1 => Some(b),
                _ => Some(c),
            })
    }

    /// Drops every id for which `keep` is false.
    pub fn retain(&mut self, mut keep: impl FnMut(u32) -> bool) {
        self.signatures.retain(|id, _| keep(*id));
        let live = &self.signatures;
        self.buckets.retain(|_, ids| {
            ids.retain(|id| live.contains_key(id));
            !ids.is_empty()
        });
    }

    pub fn len(&self) -> usize {
        self.signatures.len()
    }

    pub fn is_empty(&self) -> bool {
        self.signatures.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exact_jaccard(a: &str, b: &str) -> f32 {
        let (a, b) = (shingles(a), shingles(b));
        a.intersection(&b).count() as f32 / a.union(&b).count() as f32
    }

    #[test]
    fn normalization() {
        assert_eq!(
            normalize("  Breaking: PM Resigns!! "),
            "breaking pm resigns"
        );
        assert_eq!(normalize("東京で地震、津波警報"), "東京で地震 津波警報");
    }

    #[test]
    fn estimate_tracks_exact_jaccard() {
        let base = "Strong earthquake hits northern Japan, tsunami warning issued for the coast";
        let pairs = [
            (
                base,
                "Strong earthquake hits northern Japan; tsunami warning issued for coast",
            ),
            (
                base,
                "Strong quake strikes northern Japan, tsunami alert for the coastline",
            ),
            (
                base,
                "Central bank raises interest rates to fight stubborn inflation",
            ),
        ];
        let mh = MinHasher::default();
        for (a, b) in pairs {
            let est = estimate_jaccard(&mh.signature(a).unwrap(), &mh.signature(b).unwrap());
            let exact = exact_jaccard(a, b);
            assert!((est - exact).abs() < 0.12, "est {est} vs exact {exact}");
        }
    }

    #[test]
    fn finds_syndicated_copy_but_not_related_story() {
        let mh = MinHasher::default();
        let mut index = LshIndex::default();
        let wire = "Strong earthquake hits northern Japan, tsunami warning issued. \
                    The quake struck off the coast of Hokkaido at a depth of 10km.";
        index.insert(1, mh.signature(wire).unwrap());
        index.insert(
            2,
            mh.signature("Central bank raises interest rates again")
                .unwrap(),
        );

        let copy = "Strong earthquake hits northern Japan, tsunami warning issued — \
                    the quake struck off the coast of Hokkaido at a depth of 10 km.";
        let (id, j) = index
            .find_duplicate(&mh.signature(copy).unwrap(), 0.8)
            .unwrap();
        assert_eq!(id, 1);
        assert!(j >= 0.8);

        let related = "Tsunami warning lifted after Hokkaido quake; no damage reported";
        assert!(
            index
                .find_duplicate(&mh.signature(related).unwrap(), 0.8)
                .is_none()
        );
    }

    #[test]
    fn deterministic_and_retain_works() {
        let a = MinHasher::default().signature("same text").unwrap();
        let b = MinHasher::default().signature("same text").unwrap();
        assert_eq!(a, b);

        let mut index = LshIndex::default();
        index.insert(1, a);
        index.insert(2, b);
        index.retain(|id| id != 1);
        assert_eq!(index.len(), 1);
        assert_eq!(index.find_duplicate(&a, 0.9).map(|(id, _)| id), Some(2));
    }

    #[test]
    fn empty_text_has_no_signature() {
        assert!(MinHasher::default().signature(" ¡¿ ").is_none());
    }
}

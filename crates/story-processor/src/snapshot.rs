//! Engine snapshots and where they are kept.
//!
//! A snapshot is the complete [`State`] after processing every input before
//! `next_offset`. Because the engine is deterministic, the Kafka log after that
//! offset acts as the write-ahead log: recovery loads the newest snapshot at or
//! before the committed offset and silently re-processes the gap. So snapshots
//! can be infrequent (minutes) while transactions commit every few seconds.
//!
//! File layout: `PULSESP1` · u32 header length · bincode([`Header`]) ·
//! zstd(bincode([`State`])).

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};

use crate::engine::{Config, Engine, State};

const MAGIC: &[u8; 8] = b"PULSESP1";
const FORMAT_VERSION: u32 = 1;
const ZSTD_LEVEL: i32 = 3;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Header {
    pub format_version: u32,
    /// [`Config::fingerprint`] of the engine that produced the state.
    pub fingerprint: String,
    /// The first input offset *not* reflected in the state.
    pub next_offset: i64,
    pub watermark_ms: i64,
    pub articles: usize,
    pub stories: usize,
}

pub fn encode(engine: &Engine, next_offset: i64) -> Result<Vec<u8>> {
    let header = Header {
        format_version: FORMAT_VERSION,
        fingerprint: engine.config().fingerprint(),
        next_offset,
        watermark_ms: engine.watermark_ms(),
        articles: engine.articles().len(),
        stories: engine.stories().len(),
    };
    let header_bytes = bincode::serialize(&header)?;
    let state = zstd::encode_all(bincode::serialize(engine.state())?.as_slice(), ZSTD_LEVEL)?;
    let mut out = Vec::with_capacity(12 + header_bytes.len() + state.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(header_bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(&header_bytes);
    out.extend_from_slice(&state);
    Ok(out)
}

pub fn read_header(bytes: &[u8]) -> Result<(Header, &[u8])> {
    let rest = bytes
        .strip_prefix(MAGIC)
        .context("not a story-processor snapshot")?;
    ensure!(rest.len() >= 4, "truncated snapshot");
    let len = u32::from_le_bytes(rest[..4].try_into().expect("4 bytes")) as usize;
    ensure!(rest.len() >= 4 + len, "truncated snapshot header");
    let header: Header = bincode::deserialize(&rest[4..4 + len])?;
    ensure!(
        header.format_version == FORMAT_VERSION,
        "snapshot format {} unsupported (expected {FORMAT_VERSION})",
        header.format_version
    );
    Ok((header, &rest[4 + len..]))
}

/// Restores an engine, refusing state built under a different configuration.
pub fn decode(bytes: &[u8], cfg: Config) -> Result<(Header, Engine)> {
    let (header, body) = read_header(bytes)?;
    let expected = cfg.fingerprint();
    if header.fingerprint != expected {
        bail!(
            "snapshot was built with config {} but the engine is configured as {expected}; \
             resuming would diverge (delete the snapshots or restore the old config)",
            header.fingerprint
        );
    }
    let state: State = bincode::deserialize(&zstd::decode_all(body)?)?;
    Ok((header, Engine::from_state(cfg, state)))
}

/// Snapshot files in a directory, named by zero-padded `next_offset`.
pub struct LocalStore {
    dir: PathBuf,
}

impl LocalStore {
    pub fn open(dir: impl Into<PathBuf>) -> Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        Ok(Self { dir })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self, next_offset: i64) -> PathBuf {
        self.dir.join(format!("{next_offset:020}.snap"))
    }

    /// Writes atomically: temp file, fsync, rename, fsync directory.
    pub fn put(&self, next_offset: i64, bytes: &[u8]) -> Result<()> {
        let tmp = self.dir.join(format!(".{next_offset:020}.tmp"));
        {
            let mut f = File::create(&tmp)?;
            f.write_all(bytes)?;
            f.sync_all()?;
        }
        fs::rename(&tmp, self.path(next_offset))?;
        File::open(&self.dir)?.sync_all()?;
        Ok(())
    }

    /// Offsets of stored snapshots, ascending.
    pub fn list(&self) -> Result<Vec<i64>> {
        let mut offsets: Vec<i64> = fs::read_dir(&self.dir)?
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let name = e.file_name().into_string().ok()?;
                name.strip_suffix(".snap")?.parse().ok()
            })
            .collect();
        offsets.sort_unstable();
        Ok(offsets)
    }

    /// The newest snapshot whose `next_offset` ≤ `offset`.
    pub fn latest_at_or_before(&self, offset: i64) -> Result<Option<(i64, Vec<u8>)>> {
        match self.list()?.into_iter().rev().find(|&o| o <= offset) {
            Some(o) => Ok(Some((o, fs::read(self.path(o))?))),
            None => Ok(None),
        }
    }

    /// Deletes snapshots past `offset` (never committed) and all but the newest `keep`.
    pub fn prune(&self, keep: usize, beyond: Option<i64>) -> Result<usize> {
        let offsets = self.list()?;
        let mut removed = 0;
        let cut = offsets.len().saturating_sub(keep);
        for (i, &o) in offsets.iter().enumerate() {
            if i < cut || beyond.is_some_and(|b| o > b) {
                fs::remove_file(self.path(o))?;
                removed += 1;
            }
        }
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ann::dot;
    use proptest::prelude::*;
    use pulse_core::proto::v1::{Article, EmbeddedArticle, StoryEvent};

    /// Synthetic stream: topical clusters, duplicates, out-of-order and very
    /// late event times, and enough event-time span to close stories.
    fn stream(n: usize, seed: u64) -> Vec<EmbeddedArticle> {
        let mut s = seed | 1;
        let mut rand = move || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        };
        let hour = 3_600_000i64;
        (0..n)
            .map(|i| {
                // Time moves ~30 min per article; 1 in 8 arrives up to 40h late.
                let jitter = if rand() % 8 == 0 {
                    (rand() % 40) as i64 * hour
                } else {
                    0
                };
                // Topics drift: each lives ~2 windows of 50 articles (~17h each),
                // so early stories go idle and close within the stream. Late
                // arrivals get any topic, so some match no open story.
                let topic = if jitter > 0 {
                    (rand() % 16) as usize
                } else {
                    (i / 50 + (rand() % 2) as usize) % 16
                };
                let mut v: Vec<f32> = (0..16)
                    .map(|d| {
                        if d == topic {
                            1.0
                        } else {
                            (rand() % 100) as f32 / 400.0
                        }
                    })
                    .collect();
                let norm = dot(&v, &v).sqrt();
                v.iter_mut().for_each(|x| *x /= norm);
                let dup = i > 0 && rand() % 10 == 0;
                let id = if dup {
                    format!("a{}", i - 1)
                } else {
                    format!("a{i}")
                };
                EmbeddedArticle {
                    article: Some(Article {
                        id,
                        title: format!("topic {topic} report {}", rand() % 1000),
                        source_id: format!("src{}", rand() % 5),
                        lang: ["en", "es", "de"][(rand() % 3) as usize].into(),
                        published_at_ms: i as i64 * hour / 2 - jitter,
                        ..Default::default()
                    }),
                    vector: v,
                    model_version: "test".into(),
                }
            })
            .collect()
    }

    fn cfg() -> Config {
        Config {
            neighbor_similarity: 0.8,
            centroid_similarity: 0.8,
            ..Config::default()
        }
    }

    fn run(engine: &mut Engine, inputs: &[EmbeddedArticle], from: usize) -> Vec<StoryEvent> {
        inputs[from..]
            .iter()
            .enumerate()
            .flat_map(|(i, input)| engine.process(input, (from + i) as i64).events)
            .collect()
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(12))]

        /// The core guarantee: snapshot at any point, restore, continue — the
        /// events and final state are identical to never having stopped.
        #[test]
        fn restore_anywhere_is_indistinguishable(seed in 1u64..u64::MAX, cut_pct in 0usize..100) {
            let inputs = stream(400, seed);
            let cut = inputs.len() * cut_pct / 100;

            let mut straight = Engine::new(cfg());
            let expected = run(&mut straight, &inputs, 0);

            let mut first = Engine::new(cfg());
            let mut events = run(&mut first, &inputs[..cut], 0);
            let bytes = encode(&first, cut as i64).unwrap();
            let (header, mut resumed) = decode(&bytes, cfg()).unwrap();
            prop_assert_eq!(header.next_offset, cut as i64);
            events.extend(run(&mut resumed, &inputs, cut));

            prop_assert_eq!(&events, &expected);
            prop_assert_eq!(
                encode(&resumed, 400).unwrap(),
                encode(&straight, 400).unwrap(),
                "final state bytes differ"
            );
        }
    }

    /// Guards the property test above: the stream must actually hit the
    /// interesting paths, or "restore is indistinguishable" proves little.
    #[test]
    fn stream_exercises_lateness_and_closing() {
        use crate::engine::Outcome;
        use pulse_core::proto::v1::story_event::Kind;

        let mut e = Engine::new(cfg());
        let (mut late, mut dropped, mut closed, mut dups) = (0, 0, 0, 0);
        for (i, input) in stream(400, 42).iter().enumerate() {
            let p = e.process(input, i as i64);
            late += usize::from(p.late);
            dropped += usize::from(p.outcome == Outcome::LateDropped);
            dups += usize::from(p.outcome == Outcome::ExactDuplicate);
            closed += p
                .events
                .iter()
                .filter(|ev| matches!(ev.kind, Some(Kind::Closed(_))))
                .count();
        }
        assert!(
            late > 10 && dropped > 0 && closed > 0 && dups > 0,
            "late {late}, dropped {dropped}, closed {closed}, dups {dups}"
        );
        assert!(e.tombstones() < e.index_len() || e.index_len() == 0);
    }

    #[test]
    fn refuses_other_config() {
        let mut e = Engine::new(cfg());
        run(&mut e, &stream(50, 7), 0);
        let bytes = encode(&e, 50).unwrap();
        let other = Config { k: 11, ..cfg() };
        let err = decode(&bytes, other)
            .err()
            .expect("must refuse")
            .to_string();
        assert!(err.contains("diverge"), "{err}");
    }

    #[test]
    fn local_store_round_trip_and_prune() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalStore::open(dir.path()).unwrap();
        for o in [10, 20, 30, 40] {
            store.put(o, format!("snap{o}").as_bytes()).unwrap();
        }
        assert_eq!(store.list().unwrap(), vec![10, 20, 30, 40]);
        let (o, bytes) = store.latest_at_or_before(35).unwrap().unwrap();
        assert_eq!((o, bytes.as_slice()), (30, b"snap30".as_slice()));
        assert!(store.latest_at_or_before(5).unwrap().is_none());

        // Keep 2 newest, and drop anything past the committed offset 35.
        assert_eq!(store.prune(2, Some(35)).unwrap(), 3);
        assert_eq!(store.list().unwrap(), vec![30]);
    }
}

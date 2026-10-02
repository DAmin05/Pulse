//! Replay: re-drive any window of the input log and prove the output matches.
//!
//! 1. **Warm up:** restore the newest live snapshot at or before `from` (or
//!    start fresh at the log's beginning, which is always valid) and silently
//!    process inputs up to `from`.
//! 2. **Re-drive** `[from, to)`, collecting every story event and late article,
//!    optionally writing the events to an isolated `replay.<id>.stories` topic.
//! 3. **Diff** against what the live processor committed for the same inputs:
//!    `stories.events` (events whose `input_offset` is in range) and
//!    `articles.late`. Equal order-sensitive hashes mean byte-identical output.
//!
//! `to` is clamped to the live processor's committed offset, so a replay never
//! compares against output that hasn't been produced yet. Nothing here writes
//! to live topics or commits offsets.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use prost::Message as _;
use pulse_core::kafka;
use pulse_core::proto::v1::{EmbeddedArticle, StoryEvent};
use rdkafka::admin::{AdminClient, AdminOptions, NewTopic, TopicReplication};
use rdkafka::client::DefaultClientContext;
use rdkafka::consumer::{BaseConsumer, Consumer};
use rdkafka::message::{Header, OwnedHeaders};
use rdkafka::producer::{BaseProducer, BaseRecord, Producer};
use rdkafka::{ClientConfig, Message, Offset, TopicPartitionList};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::engine::{Config, Engine, Outcome};
use crate::snapshot::{self, LocalStore};

const PARTITION: i32 = 0;
const TIMEOUT: Duration = Duration::from_secs(10);
/// Replay output topics are scratch data.
const OUTPUT_RETENTION_MS: &str = "86400000";

#[derive(Debug, Clone)]
pub struct ReplayRequest {
    pub brokers: String,
    pub input_topic: String,
    pub events_topic: String,
    pub late_topic: String,
    /// Live processor's consumer group: its committed offset bounds `to`.
    pub processor_group: String,
    /// Live snapshots to start from (read only). `None`: always start fresh.
    pub snapshot_dir: Option<PathBuf>,
    pub from: i64,
    /// Exclusive. `None`: up to the processor's committed offset.
    pub to: Option<i64>,
    pub output_topic: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Divergence {
    /// Index in the in-range event sequence where the two sides first differ.
    pub position: usize,
    pub original: Option<String>,
    pub replayed: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct EventDiff {
    pub original: usize,
    pub replayed: usize,
    /// Positions where both sides hold the same bytes.
    pub matched: usize,
    pub original_hash: String,
    pub replayed_hash: String,
    pub identical: bool,
    pub first_divergence: Option<Divergence>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LateDiff {
    pub original: usize,
    pub replayed: usize,
    pub identical: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReplayReport {
    pub from: i64,
    pub to: i64,
    /// `to` was lowered to what the live processor has committed.
    pub clamped: bool,
    pub snapshot_offset: Option<i64>,
    pub fingerprint: String,
    pub warmup_inputs: usize,
    pub inputs: usize,
    pub events: EventDiff,
    pub late: LateDiff,
    pub identical: bool,
    pub output_topic: Option<String>,
    pub seconds: f64,
}

fn hash_all<'a>(items: impl Iterator<Item = &'a [u8]>) -> String {
    let mut h = Sha256::new();
    for bytes in items {
        h.update((bytes.len() as u64).to_le_bytes());
        h.update(bytes);
    }
    hex::encode(h.finalize())
}

/// Position and payload of an event, without the id/time fields both sides
/// share (centroids truncated: they make events unreadably long).
fn describe(e: &StoryEvent) -> String {
    let mut s = format!("@{}:{} {:?}", e.input_offset, e.seq, e.kind);
    if s.len() > 400 {
        let cut = (0..=400)
            .rev()
            .find(|&i| s.is_char_boundary(i))
            .unwrap_or(0);
        s.truncate(cut);
        s.push('…');
    }
    s
}

/// Compares two in-order event sequences byte for byte.
pub fn diff(original: &[StoryEvent], replayed: &[StoryEvent]) -> EventDiff {
    let a: Vec<Vec<u8>> = original.iter().map(|e| e.encode_to_vec()).collect();
    let b: Vec<Vec<u8>> = replayed.iter().map(|e| e.encode_to_vec()).collect();
    let matched = a.iter().zip(&b).filter(|(x, y)| x == y).count();
    let first = (0..a.len().max(b.len())).find(|&i| a.get(i) != b.get(i));
    let original_hash = hash_all(a.iter().map(Vec::as_slice));
    let replayed_hash = hash_all(b.iter().map(Vec::as_slice));
    EventDiff {
        original: a.len(),
        replayed: b.len(),
        matched,
        identical: original_hash == replayed_hash,
        first_divergence: first.map(|i| Divergence {
            position: i,
            original: original.get(i).map(describe),
            replayed: replayed.get(i).map(describe),
        }),
        original_hash,
        replayed_hash,
    }
}

/// First input offset whose Kafka timestamp (pipeline time) is ≥ `time_ms`.
/// Returns the end offset when nothing is that recent.
pub fn offset_for_time(brokers: &str, topic: &str, time_ms: i64) -> Result<i64> {
    let consumer: BaseConsumer = ClientConfig::new()
        .set("bootstrap.servers", brokers)
        .create()?;
    let mut tpl = TopicPartitionList::new();
    tpl.add_partition_offset(topic, PARTITION, Offset::Offset(time_ms))?;
    let found = consumer.offsets_for_times(tpl, TIMEOUT)?;
    match found.elements().first().map(|e| e.offset()) {
        Some(Offset::Offset(o)) => Ok(o),
        _ => Ok(consumer.fetch_watermarks(topic, PARTITION, TIMEOUT)?.1),
    }
}

fn committed_offset(brokers: &str, group: &str, topic: &str) -> Result<Option<i64>> {
    let consumer: BaseConsumer = ClientConfig::new()
        .set("bootstrap.servers", brokers)
        .set("group.id", group)
        .set("enable.auto.commit", "false")
        .create()?;
    let mut tpl = TopicPartitionList::new();
    tpl.add_partition(topic, PARTITION);
    let committed = consumer.committed_offsets(tpl, TIMEOUT)?;
    Ok(committed.elements().first().and_then(|e| match e.offset() {
        Offset::Offset(o) => Some(o),
        _ => None,
    }))
}

/// Runs a replay. Blocking (Kafka scans and CPU-bound processing); call from
/// `spawn_blocking` in async code. `config_for` builds the engine config from
/// the input's model version, exactly as the live processor does.
pub fn run(
    req: &ReplayRequest,
    config_for: impl FnOnce(&str) -> Result<Config>,
) -> Result<ReplayReport> {
    let started = Instant::now();
    let probe: BaseConsumer = ClientConfig::new()
        .set("bootstrap.servers", &req.brokers)
        .create()?;
    let (low, _) = probe.fetch_watermarks(&req.input_topic, PARTITION, TIMEOUT)?;
    let committed = committed_offset(&req.brokers, &req.processor_group, &req.input_topic)?
        .context("the live processor has not committed anything yet: nothing to compare against")?;
    let requested_to = req.to.unwrap_or(committed);
    let to = requested_to.min(committed);
    let from = req.from.max(low);
    if from >= to {
        bail!("empty range [{from}, {to}) (live processor committed up to {committed})");
    }

    // Model version from the first input, so the engine is built like live's.
    let mut model_version = None;
    kafka::scan_range(
        &req.brokers,
        &req.input_topic,
        PARTITION,
        low,
        (low + 16).min(to),
        |m| {
            if model_version.is_none() {
                model_version = m
                    .payload()
                    .and_then(|p| EmbeddedArticle::decode(p).ok())
                    .map(|e| e.model_version);
            }
        },
    )?;
    let cfg = config_for(&model_version.context("no decodable input")?)?;
    let fingerprint = cfg.fingerprint();

    // Warm-up start: newest compatible snapshot at or before `from`.
    let restored = match &req.snapshot_dir {
        Some(dir) if dir.exists() => {
            let store = LocalStore::open(dir)?;
            match store.latest_at_or_before(from)? {
                Some((offset, bytes)) => match snapshot::decode(&bytes, cfg.clone()) {
                    Ok((_, engine)) => Some((offset, engine)),
                    Err(e) => {
                        tracing::warn!(offset, error = %e, "snapshot unusable; starting fresh");
                        None
                    }
                },
                None => None,
            }
        }
        _ => None,
    };
    let (snapshot_offset, mut engine) = match restored {
        Some((o, e)) => (Some(o), e),
        None => (None, Engine::new(cfg)),
    };
    let start = snapshot_offset.unwrap_or(low);

    let mut warmup = 0usize;
    let mut inputs = 0usize;
    let mut replayed: Vec<StoryEvent> = Vec::new();
    let mut replayed_late: Vec<String> = Vec::new();
    let mut range_ids: HashSet<String> = HashSet::new();
    kafka::scan_range(&req.brokers, &req.input_topic, PARTITION, start, to, |m| {
        let Some(Ok(input)) = m.payload().map(EmbeddedArticle::decode) else {
            return; // the live processor skips undecodable inputs too
        };
        let processed = engine.process(&input, m.offset());
        if m.offset() < from {
            warmup += 1;
            return;
        }
        inputs += 1;
        let id = input
            .article
            .as_ref()
            .map(|a| a.id.clone())
            .unwrap_or_default();
        if processed.outcome == Outcome::LateDropped {
            replayed_late.push(id.clone());
        }
        range_ids.insert(id);
        replayed.extend(processed.events);
    })
    .context("re-driving input")?;

    // What the live processor committed for the same inputs.
    let mut original = Vec::new();
    kafka::scan_since(&req.brokers, &req.events_topic, 0, |m| {
        if let Some(Ok(e)) = m.payload().map(StoryEvent::decode) {
            if (from..to).contains(&e.input_offset) {
                original.push(e);
            }
        }
    })
    .context("reading original events")?;
    let mut original_late = Vec::new();
    kafka::scan_since(&req.brokers, &req.late_topic, 0, |m| {
        if let Some(Ok(a)) = m.payload().map(EmbeddedArticle::decode) {
            let id = a.article.map(|a| a.id).unwrap_or_default();
            if range_ids.contains(&id) {
                original_late.push(id);
            }
        }
    })
    .context("reading original late articles")?;

    if let Some(topic) = &req.output_topic {
        write_output(&req.brokers, topic, &replayed)?;
    }

    let events = diff(&original, &replayed);
    let late = LateDiff {
        original: original_late.len(),
        replayed: replayed_late.len(),
        identical: original_late == replayed_late,
    };
    Ok(ReplayReport {
        from,
        to,
        clamped: requested_to > committed,
        snapshot_offset,
        fingerprint,
        warmup_inputs: warmup,
        inputs,
        identical: events.identical && late.identical,
        events,
        late,
        output_topic: req.output_topic.clone(),
        seconds: started.elapsed().as_secs_f64(),
    })
}

fn write_output(brokers: &str, topic: &str, events: &[StoryEvent]) -> Result<()> {
    let admin: AdminClient<DefaultClientContext> = ClientConfig::new()
        .set("bootstrap.servers", brokers)
        .create()?;
    let new = NewTopic::new(topic, 1, TopicReplication::Fixed(1))
        .set("retention.ms", OUTPUT_RETENTION_MS);
    let results = futures::executor::block_on(admin.create_topics([&new], &AdminOptions::new()))?;
    for r in results {
        if let Err((name, code)) = r {
            if code != rdkafka::types::RDKafkaErrorCode::TopicAlreadyExists {
                bail!("creating {name}: {code}");
            }
        }
    }

    let producer: BaseProducer = kafka::idempotent_producer(brokers, "story-replay").create()?;
    for event in events {
        let payload = event.encode_to_vec();
        let mut record = BaseRecord::to(topic)
            .key(crate::live::story_id(event))
            .payload(&payload)
            // Same shape as live output, so the same tools read both.
            .headers(OwnedHeaders::new().insert(Header {
                key: "content-type",
                value: Some(crate::live::EVENT_CONTENT_TYPE),
            }));
        loop {
            match producer.send(record) {
                Ok(()) => break,
                Err((
                    rdkafka::error::KafkaError::MessageProduction(
                        rdkafka::types::RDKafkaErrorCode::QueueFull,
                    ),
                    r,
                )) => {
                    record = r;
                    producer.poll(Duration::from_millis(50));
                }
                Err((e, _)) => return Err(e.into()),
            }
        }
    }
    producer.flush(Duration::from_secs(60))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pulse_core::proto::v1::{StoryClosed, story_event::Kind};

    fn ev(offset: i64, seq: u32, story: &str) -> StoryEvent {
        StoryEvent {
            event_id: format!("{offset}-{seq}"),
            input_offset: offset,
            seq,
            kind: Some(Kind::Closed(StoryClosed {
                story_id: story.into(),
                reason: 1,
            })),
            ..Default::default()
        }
    }

    #[test]
    fn identical_sequences() {
        let a = vec![ev(1, 0, "s1"), ev(2, 0, "s2")];
        let d = diff(&a, &a.clone());
        assert!(d.identical);
        assert_eq!((d.original, d.replayed, d.matched), (2, 2, 2));
        assert!(d.first_divergence.is_none());
    }

    #[test]
    fn finds_the_first_difference() {
        let a = vec![ev(1, 0, "s1"), ev(2, 0, "s2"), ev(3, 0, "s3")];
        let mut b = a.clone();
        b[1] = ev(2, 0, "other");
        let d = diff(&a, &b);
        assert!(!d.identical);
        assert_eq!(d.matched, 2);
        let first = d.first_divergence.unwrap();
        assert_eq!(first.position, 1);
        assert!(first.original.unwrap().contains("s2"));
        assert!(first.replayed.unwrap().contains("other"));
    }

    #[test]
    fn order_and_length_matter() {
        let a = vec![ev(1, 0, "s1"), ev(2, 0, "s2")];
        let swapped = vec![a[1].clone(), a[0].clone()];
        assert!(!diff(&a, &swapped).identical);

        let shorter = vec![a[0].clone()];
        let d = diff(&a, &shorter);
        assert!(!d.identical);
        let first = d.first_divergence.unwrap();
        assert_eq!(first.position, 1);
        assert!(first.replayed.is_none(), "missing on the replay side");
    }
}

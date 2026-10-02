//! `pulse topic check|hash`: verify and fingerprint committed topic contents.

use std::collections::{BTreeMap, HashMap};

use anyhow::{Context, Result};
use prost::Message as _;
use pulse_core::proto::v1::StoryEvent;
use pulse_core::{config::Settings, kafka};
use rdkafka::Message as _;
use rdkafka::message::Headers;
use sha2::{Digest, Sha256};

/// Records whose identity is inside the payload (keys are story ids, shared by
/// many events) are recognized by their content-type header.
fn identity(msg: &rdkafka::message::BorrowedMessage<'_>) -> Vec<u8> {
    let is_story_event = msg.headers().is_some_and(|h| {
        h.iter().any(|header| {
            header.key == "content-type"
                && header
                    .value
                    .is_some_and(|v| v.windows(19).any(|w| w == b"pulse.v1.StoryEvent"))
        })
    });
    if is_story_event {
        if let Some(Ok(event)) = msg.payload().map(StoryEvent::decode) {
            return event.event_id.into_bytes();
        }
    }
    msg.key().unwrap_or_default().to_vec()
}

/// Counts committed records and duplicate identities (message key, or
/// `event_id` for story events). Returns false when duplicates exist.
pub async fn check(settings: &Settings, topic: &str) -> Result<bool> {
    let brokers = settings.kafka_brokers.clone();
    let name = topic.to_owned();
    let (records, ids, min_ts, max_ts) = tokio::task::spawn_blocking(move || {
        let mut ids: HashMap<Vec<u8>, u32> = HashMap::new();
        let (mut min_ts, mut max_ts) = (i64::MAX, i64::MIN);
        // read_committed: aborted transactional records and commit markers are skipped.
        let records = kafka::scan_since(&brokers, &name, 0, |msg| {
            *ids.entry(identity(msg)).or_default() += 1;
            if let Some(ts) = msg.timestamp().to_millis() {
                min_ts = min_ts.min(ts);
                max_ts = max_ts.max(ts);
            }
        })
        .with_context(|| format!("reading {name}"))?;
        anyhow::Ok((records, ids, min_ts, max_ts))
    })
    .await??;

    let duplicates: Vec<_> = ids.iter().filter(|(_, n)| **n > 1).collect();
    println!("topic          {topic}");
    println!("records        {records} (committed)");
    println!("unique ids     {}", ids.len());
    println!("duplicate ids  {}", duplicates.len());
    if records > 0 {
        println!(
            "span           {:.1} min",
            (max_ts - min_ts) as f64 / 60_000.0
        );
    }
    for (id, n) in duplicates.iter().take(5) {
        println!("  {} ×{n}", String::from_utf8_lossy(id));
    }
    Ok(duplicates.is_empty())
}

/// Order-sensitive fingerprint of committed contents: sha256 over each
/// partition's (key, value) sequence, partitions in id order. Two topics with
/// the same hash hold the same records in the same order.
pub async fn hash(settings: &Settings, topic: &str) -> Result<()> {
    let brokers = settings.kafka_brokers.clone();
    let name = topic.to_owned();
    let (records, digest) = tokio::task::spawn_blocking(move || {
        let mut partitions: BTreeMap<i32, Sha256> = BTreeMap::new();
        let records = kafka::scan_since(&brokers, &name, 0, |msg| {
            let h = partitions.entry(msg.partition()).or_default();
            for part in [
                msg.key().unwrap_or_default(),
                msg.payload().unwrap_or_default(),
            ] {
                h.update((part.len() as u64).to_le_bytes());
                h.update(part);
            }
        })
        .with_context(|| format!("reading {name}"))?;
        let mut all = Sha256::new();
        for (partition, h) in partitions {
            all.update(partition.to_le_bytes());
            all.update(h.finalize());
        }
        anyhow::Ok((records, hex::encode(all.finalize())))
    })
    .await??;
    println!("{records} {digest}");
    Ok(())
}

//! `pulse topic check`: count committed records and duplicate keys in a topic.

use std::collections::HashMap;

use anyhow::{Context, Result};
use pulse_core::{config::Settings, kafka};
use rdkafka::Message as _;

pub async fn check(settings: &Settings, topic: &str) -> Result<bool> {
    let brokers = settings.kafka_brokers.clone();
    let name = topic.to_owned();
    let (records, keys, min_ts, max_ts) = tokio::task::spawn_blocking(move || {
        let mut keys: HashMap<Vec<u8>, u32> = HashMap::new();
        let (mut min_ts, mut max_ts) = (i64::MAX, i64::MIN);
        // read_committed: aborted transactional records and commit markers are skipped.
        let records = kafka::scan_since(&brokers, &name, 0, |msg| {
            *keys
                .entry(msg.key().unwrap_or_default().to_vec())
                .or_default() += 1;
            if let Some(ts) = msg.timestamp().to_millis() {
                min_ts = min_ts.min(ts);
                max_ts = max_ts.max(ts);
            }
        })
        .with_context(|| format!("reading {name}"))?;
        anyhow::Ok((records, keys, min_ts, max_ts))
    })
    .await??;

    let duplicates: Vec<_> = keys.iter().filter(|(_, n)| **n > 1).collect();
    println!("topic          {topic}");
    println!("records        {records} (committed)");
    println!("unique keys    {}", keys.len());
    println!("duplicate keys {}", duplicates.len());
    if records > 0 {
        println!(
            "span           {:.1} min",
            (max_ts - min_ts) as f64 / 60_000.0
        );
    }
    for (key, n) in duplicates.iter().take(5) {
        println!("  {} ×{n}", String::from_utf8_lossy(key));
    }
    Ok(duplicates.is_empty())
}

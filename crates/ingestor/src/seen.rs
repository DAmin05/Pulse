//! Which article ids were already published, so re-polling a feed doesn't
//! republish its whole item list.
//!
//! The set lives in memory and is rebuilt on startup from `articles.raw`
//! itself: the log is the state, so there's nothing extra to persist.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result};
use pulse_core::kafka;
use rdkafka::Message;

pub struct SeenSet {
    /// id → time it was first seen (ms).
    entries: HashMap<String, i64>,
    ttl_ms: i64,
}

impl SeenSet {
    pub fn new(ttl: Duration) -> Self {
        Self {
            entries: HashMap::new(),
            ttl_ms: ttl.as_millis() as i64,
        }
    }

    /// Returns true if `id` was not seen before and marks it seen.
    pub fn claim(&mut self, id: &str, now_ms: i64) -> bool {
        if self.entries.contains_key(id) {
            return false;
        }
        self.entries.insert(id.to_owned(), now_ms);
        true
    }

    /// Undoes a claim whose publish failed, so the next poll retries it.
    pub fn release(&mut self, id: &str) {
        self.entries.remove(id);
    }

    pub fn insert(&mut self, id: String, seen_at_ms: i64) {
        self.entries.entry(id).or_insert(seen_at_ms);
    }

    /// Forgets ids older than the TTL (they're also past `max_age`, so the
    /// normalizer drops them before they reach the set again).
    pub fn prune(&mut self, now_ms: i64) {
        let cutoff = now_ms - self.ttl_ms;
        self.entries.retain(|_, t| *t >= cutoff);
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

/// Reads `(key, timestamp)` for every message in `topic` newer than `since_ms`.
/// Blocking; call from `spawn_blocking`.
pub fn load_from_kafka(brokers: &str, topic: &str, since_ms: i64) -> Result<Vec<(String, i64)>> {
    let mut out = Vec::new();
    kafka::scan_since(brokers, topic, since_ms, |msg| {
        if let (Some(key), Some(ts)) = (msg.key(), msg.timestamp().to_millis()) {
            out.push((String::from_utf8_lossy(key).into_owned(), ts));
        }
    })
    .with_context(|| format!("rebuilding seen set from {topic}"))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claim_release_prune() {
        let mut s = SeenSet::new(Duration::from_secs(60));
        assert!(s.claim("a", 0));
        assert!(!s.claim("a", 1));
        s.release("a");
        assert!(s.claim("a", 2));
        s.insert("b".into(), 50_000);
        s.prune(61_000);
        assert_eq!(s.len(), 1); // "a" (t=2) expired, "b" kept
        assert!(!s.claim("b", 61_000));
    }
}

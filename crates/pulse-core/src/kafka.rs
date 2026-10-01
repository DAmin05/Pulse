//! Kafka client configurations for the delivery guarantees Pulse relies on.
//!
//! Services build their clients from these so the guarantees are set in one place.

use std::collections::HashMap;
use std::time::Duration;

use rdkafka::consumer::{BaseConsumer, Consumer};
use rdkafka::error::KafkaError;
use rdkafka::message::BorrowedMessage;
use rdkafka::{ClientConfig, Offset, TopicPartitionList};

#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    #[error(transparent)]
    Kafka(#[from] KafkaError),
    #[error("timed out reading {0}")]
    Timeout(String),
}

/// Calls `f` for every message in `topic` with a timestamp at or after
/// `since_ms`, up to the end of each partition as of the call. Partitions are
/// read concurrently, so order is only guaranteed within a partition.
///
/// Blocking: call from `spawn_blocking` in async code.
pub fn scan_since(
    brokers: &str,
    topic: &str,
    since_ms: i64,
    mut f: impl FnMut(&BorrowedMessage<'_>),
) -> Result<usize, ScanError> {
    const TIMEOUT: Duration = Duration::from_secs(10);
    let consumer: BaseConsumer = base(brokers, "pulse-scan")
        .set("group.id", "pulse-scan")
        .set("enable.auto.commit", "false")
        .set("isolation.level", "read_committed")
        .create()?;

    let metadata = consumer.fetch_metadata(Some(topic), TIMEOUT)?;
    let mut by_time = TopicPartitionList::new();
    for p in metadata.topics().iter().flat_map(|t| t.partitions()) {
        by_time.add_partition_offset(topic, p.id(), Offset::Offset(since_ms))?;
    }
    let starts = consumer.offsets_for_times(by_time, TIMEOUT)?;

    // Assign only partitions with data in range; remember where each ends now.
    let mut assignment = TopicPartitionList::new();
    let mut ends: HashMap<i32, i64> = HashMap::new();
    for elem in starts.elements() {
        let Offset::Offset(start) = elem.offset() else {
            continue; // nothing at or after since_ms
        };
        let (_, high) = consumer.fetch_watermarks(topic, elem.partition(), TIMEOUT)?;
        if start < high {
            assignment.add_partition_offset(topic, elem.partition(), Offset::Offset(start))?;
            ends.insert(elem.partition(), high);
        }
    }
    if ends.is_empty() {
        return Ok(0);
    }
    consumer.assign(&assignment)?;

    // On transactional topics the last offsets can be commit markers, which are
    // never delivered. So a partition is done when the consumer's *position*
    // (which skips markers and aborted records) reaches the end, not when we see
    // a message at end-1.
    let mut count = 0;
    let mut last_progress = std::time::Instant::now();
    while !ends.is_empty() {
        if let Some(msg) = consumer.poll(Duration::from_millis(200)) {
            let msg = msg?;
            use rdkafka::Message as _;
            if ends
                .get(&msg.partition())
                .is_some_and(|&end| msg.offset() < end)
            {
                f(&msg);
                count += 1;
            }
            last_progress = std::time::Instant::now();
        }
        for elem in consumer.position()?.elements_for_topic(topic) {
            if let Offset::Offset(pos) = elem.offset() {
                if ends.get(&elem.partition()).is_some_and(|&end| pos >= end) {
                    ends.remove(&elem.partition());
                }
            }
        }
        if last_progress.elapsed() > TIMEOUT {
            return Err(ScanError::Timeout(topic.to_owned()));
        }
    }
    Ok(count)
}

fn base(brokers: &str, client_id: &str) -> ClientConfig {
    let mut cfg = ClientConfig::new();
    cfg.set("bootstrap.servers", brokers)
        .set("client.id", client_id);
    cfg
}

/// Producer that never writes duplicates on retry and never reorders within a
/// partition. Used by the Ingestor and the embed loop.
pub fn idempotent_producer(brokers: &str, client_id: &str) -> ClientConfig {
    let mut cfg = base(brokers, client_id);
    cfg.set("enable.idempotence", "true")
        .set("acks", "all")
        .set("compression.type", "lz4")
        .set("linger.ms", "5");
    cfg
}

/// Transactional producer for consume-transform-produce. The transactional id
/// must be stable across restarts so a restarted instance fences the old one.
pub fn transactional_producer(brokers: &str, transactional_id: &str) -> ClientConfig {
    let mut cfg = idempotent_producer(brokers, transactional_id);
    cfg.set("transactional.id", transactional_id)
        .set("transaction.timeout.ms", "60000");
    cfg
}

/// Consumer that only sees committed transactional output and never commits
/// offsets on its own: offsets are committed inside the producer transaction
/// (Story Processor) or after a successful write (Story Sink).
pub fn exactly_once_consumer(brokers: &str, group_id: &str) -> ClientConfig {
    let mut cfg = base(brokers, group_id);
    cfg.set("group.id", group_id)
        .set("isolation.level", "read_committed")
        .set("enable.auto.commit", "false")
        .set("enable.auto.offset.store", "false")
        .set("auto.offset.reset", "earliest")
        // A crashed member holds its partitions until its session expires; keep
        // that short so a restarted instance resumes quickly.
        .set("session.timeout.ms", "10000")
        .set("heartbeat.interval.ms", "2000");
    cfg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transactional_producer_is_idempotent() {
        let cfg = transactional_producer("b:9092", "story-processor-0");
        assert_eq!(cfg.get("enable.idempotence"), Some("true"));
        assert_eq!(cfg.get("acks"), Some("all"));
        assert_eq!(cfg.get("transactional.id"), Some("story-processor-0"));
    }

    #[test]
    fn consumer_reads_committed_only() {
        let cfg = exactly_once_consumer("b:9092", "story-sink");
        assert_eq!(cfg.get("isolation.level"), Some("read_committed"));
        assert_eq!(cfg.get("enable.auto.commit"), Some("false"));
    }
}

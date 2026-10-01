//! Kafka client configurations for the delivery guarantees Pulse relies on.
//!
//! Services build their clients from these so the guarantees are set in one place.

use rdkafka::ClientConfig;

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
        .set("auto.offset.reset", "earliest");
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

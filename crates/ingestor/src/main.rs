//! Polls RSS/API sources and publishes raw articles to Kafka.

fn main() -> anyhow::Result<()> {
    pulse_core::telemetry::init("ingestor");
    tracing::warn!("not implemented yet (phase 1)");
    Ok(())
}

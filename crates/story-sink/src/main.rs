//! Writes committed story events to Postgres idempotently.

fn main() -> anyhow::Result<()> {
    pulse_core::telemetry::init("story-sink");
    tracing::warn!("not implemented yet (phase 6)");
    Ok(())
}

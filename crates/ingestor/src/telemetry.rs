//! Prometheus metrics, served at `:PULSE_INGESTOR_METRICS_PORT/metrics`.
//!
//! - `pulse_ingestor_polls_total{source,outcome}` — ok | not_modified | http_error | network_error | parse_error
//! - `pulse_ingestor_poll_duration_seconds{kind}`
//! - `pulse_ingestor_items_total{source,result}` — published | seen | invalid_url | missing_title | too_old | publish_error
//! - `pulse_ingestor_published_total{lang}`
//! - `pulse_ingestor_event_time_corrections_total{reason}` — missing | future
//! - `pulse_ingestor_last_success_timestamp_seconds{source}`
//! - `pulse_ingestor_seen_set_size`
//! - `pulse_ingestor_gdelt_slots_skipped_total`

use std::net::{Ipv4Addr, SocketAddr};

use anyhow::Result;
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder};

pub fn install(port: u16) -> Result<()> {
    PrometheusBuilder::new()
        .with_http_listener(SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)))
        .set_buckets_for_metric(
            Matcher::Suffix("duration_seconds".into()),
            &[0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0],
        )?
        .install()?;
    tracing::info!(port, "metrics listening on /metrics");
    Ok(())
}

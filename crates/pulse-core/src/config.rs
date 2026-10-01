//! Environment-driven settings shared across services.
//!
//! Defaults match `deploy/docker-compose.yml` so services run locally with no
//! configuration. See `.env.example` for the full list.

use std::env;

pub fn env_or(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_owned())
}

#[derive(Debug, Clone)]
pub struct Settings {
    pub kafka_brokers: String,
    pub database_url: String,
    pub s3_endpoint: String,
    pub prometheus_url: String,
    pub grafana_url: String,
}

impl Settings {
    pub fn from_env() -> Self {
        Self {
            kafka_brokers: env_or("PULSE_KAFKA_BROKERS", "localhost:19092"),
            database_url: env_or(
                "PULSE_DATABASE_URL",
                "postgres://pulse:pulse@localhost:5432/pulse",
            ),
            s3_endpoint: env_or("PULSE_S3_ENDPOINT", "http://localhost:8333"),
            prometheus_url: env_or("PULSE_PROMETHEUS_URL", "http://localhost:9090"),
            grafana_url: env_or("PULSE_GRAFANA_URL", "http://localhost:3000"),
        }
    }
}

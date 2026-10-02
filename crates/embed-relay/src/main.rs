//! articles.raw → Embedder (gRPC) → articles.embedded, exactly once.

mod embedder;
mod offline;
mod relay;

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use anyhow::Result;
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder};
use pulse_core::config::{Settings, env_or};
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() -> Result<()> {
    pulse_core::telemetry::init("embed-relay");
    let settings = Settings::from_env();

    let args: Vec<String> = std::env::args().skip(1).collect();
    if let [cmd, input, output] = args.as_slice() {
        if cmd == "embed-fixture" {
            let embedder = embedder::Embedder::connect_lazy(
                &env_or("PULSE_EMBEDDER_URL", "http://localhost:50061"),
                env_or("PULSE_RELAY_REQUEST_SIZE", "16").parse()?,
                env_or("PULSE_RELAY_CONCURRENCY", "8").parse()?,
            )?;
            return offline::embed_fixture(&embedder, input.as_ref(), output.as_ref()).await;
        }
    }
    anyhow::ensure!(
        args.is_empty(),
        "usage: embed-relay                         (relay articles.raw → articles.embedded)\n       \
         embed-relay embed-fixture IN.pulsefx OUT.pulseem"
    );

    let metrics_port: u16 = env_or("PULSE_RELAY_METRICS_PORT", "9106").parse()?;
    PrometheusBuilder::new()
        .with_http_listener(SocketAddr::from((Ipv4Addr::UNSPECIFIED, metrics_port)))
        .set_buckets_for_metric(
            Matcher::Suffix("seconds".into()),
            &[
                0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0,
            ],
        )?
        .set_buckets_for_metric(
            Matcher::Full("pulse_relay_batch_size".into()),
            &[1.0, 4.0, 16.0, 32.0, 64.0, 128.0, 256.0, 512.0],
        )?
        .install()?;

    let embedder = embedder::Embedder::connect_lazy(
        &env_or("PULSE_EMBEDDER_URL", "http://localhost:50061"),
        env_or("PULSE_RELAY_REQUEST_SIZE", "16").parse()?,
        env_or("PULSE_RELAY_CONCURRENCY", "8").parse()?,
    )?;
    let info = embedder.model_info().await?;
    tracing::info!(
        model = info.model_version,
        dims = info.dimensions,
        server_max_batch = info.max_batch,
        server_max_wait_ms = info.max_wait_ms,
        "embedder reachable"
    );

    let shutdown = CancellationToken::new();
    let on_signal = shutdown.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            tracing::info!("shutting down after the current batch");
            on_signal.cancel();
        }
    });

    let cfg = relay::Config {
        brokers: settings.kafka_brokers,
        group_id: env_or("PULSE_RELAY_GROUP", "embed-relay"),
        output_topic: env_or(
            "PULSE_RELAY_OUTPUT_TOPIC",
            pulse_core::topics::ARTICLES_EMBEDDED,
        ),
        max_batch: env_or("PULSE_RELAY_MAX_BATCH", "256").parse()?,
        linger: Duration::from_millis(env_or("PULSE_RELAY_LINGER_MS", "250").parse()?),
    };
    relay::run(cfg, embedder, shutdown).await
}

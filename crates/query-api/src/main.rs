//! Query API: REST + SSE over the Postgres read model.
//!
//! - `GET /api/stories`, `/api/stories/{id}`, `/api/graph`: current state or any
//!   past position (`?at=<offset>` or `?as_of=<RFC 3339 time>`).
//! - `GET /api/search?q=`: cross-lingual semantic search (Embedder + pgvector).
//! - `GET /api/timeline`, `/api/stats`, `/api/sources`: slider, panels, filters.
//! - `GET /api/pipeline`: per-stage throughput/latency (Prometheus) and lag (Kafka).
//! - `GET /api/stream`: live story events (SSE), resumable with `Last-Event-ID`.
//!   Fed by the sink's Postgres NOTIFY, so every streamed event is already
//!   queryable through the endpoints above.
//! - `POST /api/replays`, `GET /api/replays[/{id}]`: re-drive a window of the
//!   input log and diff it against the live output (one run at a time).
//! - `GET /api/listen`, `POST /api/stories/{id}/briefing`, `GET /api/audio/{key}`:
//!   a story briefing translated and read aloud (DeepL + ElevenLabs, cached,
//!   budgeted; the browser's own voice when unavailable).
//! - `GET /metrics`: Prometheus.

mod listen;
mod live;
mod pipeline;
mod replays;
mod routes;

use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use axum::Router;
use axum::extract::{MatchedPath, Request};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::{get, post};
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder, PrometheusHandle};
use pulse_core::config::{Settings, env_or};
use pulse_core::proto::v1::embedder_service_client::EmbedderServiceClient;
use tonic::transport::{Channel, Endpoint};
use tower_http::compression::CompressionLayer;
use tower_http::compression::predicate::{DefaultPredicate, NotForContentType, Predicate};
use tower_http::cors::CorsLayer;

pub struct AppState {
    pub pool: pulse_store::deadpool_postgres::Pool,
    pub embedder: EmbedderServiceClient<Channel>,
    pub brokers: String,
    pub topics: Vec<String>,
    pub sources_path: PathBuf,
    pub live: live::Live,
    pub replays: replays::Runner,
    pub metrics: PrometheusHandle,
    /// `/api/stats` pipeline section, cached briefly (it queries Kafka).
    pub pipeline_cache: tokio::sync::Mutex<Option<(Instant, serde_json::Value)>>,
    /// `/api/pipeline` response, cached briefly (Prometheus + Kafka).
    pub pipeline_panel_cache: tokio::sync::Mutex<Option<(Instant, serde_json::Value)>>,
    pub prometheus_url: String,
    pub listen: listen::Listen,
}

pub type Shared = Arc<AppState>;

#[tokio::main]
async fn main() -> Result<()> {
    pulse_core::telemetry::init("query-api");
    let settings = Settings::from_env();

    let metrics = PrometheusBuilder::new()
        .set_buckets_for_metric(
            Matcher::Suffix("seconds".into()),
            &[
                0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5,
            ],
        )?
        .install_recorder()?;

    // Ensure the schema exists even if the API starts before the sink.
    let mut db = pulse_store::connect(&settings.database_url).await?;
    pulse_store::migrate(&mut db).await?;
    let interrupted = pulse_store::replays::fail_interrupted(&db).await?;
    if interrupted > 0 {
        tracing::warn!(
            interrupted,
            "marked replays interrupted by the last shutdown as failed"
        );
    }
    drop(db);

    let pool = pulse_store::pool(&settings.database_url, 16)?;
    let embedder = EmbedderServiceClient::new(
        Endpoint::from_shared(env_or("PULSE_EMBEDDER_URL", "http://localhost:50061"))?
            .connect_timeout(Duration::from_secs(2))
            .connect_lazy(),
    );
    let live = live::Live::start(settings.database_url.clone(), pool.clone()).await?;

    let state: Shared = Arc::new(AppState {
        pool,
        embedder,
        brokers: settings.kafka_brokers.clone(),
        topics: vec![
            pulse_core::topics::ARTICLES_RAW.into(),
            pulse_core::topics::ARTICLES_EMBEDDED.into(),
            pulse_core::topics::STORIES_EVENTS.into(),
            pulse_core::topics::ARTICLES_LATE.into(),
        ],
        sources_path: env_or("PULSE_SOURCES", "config/sources.toml").into(),
        live,
        replays: replays::Runner::new(
            settings.kafka_brokers.clone(),
            env_or(
                "PULSE_PROCESSOR_INPUT_TOPIC",
                pulse_core::topics::ARTICLES_EMBEDDED,
            ),
            env_or(
                "PULSE_PROCESSOR_OUTPUT_TOPIC",
                pulse_core::topics::STORIES_EVENTS,
            ),
            env_or(
                "PULSE_PROCESSOR_LATE_TOPIC",
                pulse_core::topics::ARTICLES_LATE,
            ),
            env_or("PULSE_PROCESSOR_GROUP", "story-processor"),
            env_or(
                "PULSE_PROCESSOR_SNAPSHOT_DIR",
                "data/checkpoints/story-processor",
            )
            .into(),
        ),
        metrics,
        pipeline_cache: tokio::sync::Mutex::new(None),
        pipeline_panel_cache: tokio::sync::Mutex::new(None),
        prometheus_url: settings.prometheus_url.clone(),
        listen: listen::Listen::from_env(&settings)?,
    });

    let app = Router::new()
        .route("/api/health", get(routes::health))
        .route("/api/stats", get(routes::stats))
        .route("/api/stories", get(routes::stories))
        .route("/api/stories/{id}", get(routes::story))
        .route("/api/graph", get(routes::graph))
        .route("/api/search", get(routes::search))
        .route("/api/timeline", get(routes::timeline))
        .route("/api/sources", get(routes::sources))
        .route("/api/pipeline", get(pipeline::pipeline))
        .route("/api/stream", get(live::stream))
        .route("/api/replays", get(replays::list).post(replays::create))
        .route("/api/replays/{id}", get(replays::get))
        .route("/api/listen", get(listen::capabilities))
        .route("/api/stories/{id}/briefing", post(listen::briefing))
        .route("/api/audio/{key}", get(listen::audio))
        .route("/metrics", get(routes::metrics))
        .layer(middleware::from_fn(track))
        // MP3 is already compressed.
        .layer(
            CompressionLayer::new()
                .compress_when(DefaultPredicate::new().and(NotForContentType::const_new("audio/"))),
        )
        .layer(CorsLayer::permissive())
        .with_state(state);

    let port: u16 = env_or("PULSE_API_PORT", "9105").parse()?;
    let listener =
        tokio::net::TcpListener::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, port))).await?;
    tracing::info!(port, "listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
            tracing::info!("shutting down");
        })
        .await?;
    Ok(())
}

/// Request count and latency per route.
async fn track(request: Request, next: Next) -> Response {
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map_or_else(|| "unmatched".to_owned(), |p| p.as_str().to_owned());
    let started = Instant::now();
    let response = next.run(request).await;
    if route != "/api/stream" {
        metrics::histogram!("pulse_api_request_seconds", "route" => route.clone())
            .record(started.elapsed().as_secs_f64());
    }
    metrics::counter!("pulse_api_requests_total", "route" => route, "status" => response.status().as_u16().to_string())
        .increment(1);
    response
}

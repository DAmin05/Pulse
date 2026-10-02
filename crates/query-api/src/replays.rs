//! Replay Service: re-drive a window of the input log and diff it against the
//! live output (see `story_processor::replay`). Runs one at a time in the
//! background; requests return immediately and are polled by id.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use pulse_store::{reader, replays};
use serde::Deserialize;
use serde_json::{Value, json};
use story_processor::replay::{self, ReplayRequest};
use tokio::sync::Semaphore;

use crate::Shared;
use crate::routes::{ApiError, ApiResult, db};

pub struct Runner {
    /// One replay at a time: they are CPU-bound and scan whole topics.
    slots: Arc<Semaphore>,
    pub brokers: String,
    pub input_topic: String,
    pub events_topic: String,
    pub late_topic: String,
    pub processor_group: String,
    pub snapshot_dir: PathBuf,
}

impl Runner {
    pub fn new(
        brokers: String,
        input_topic: String,
        events_topic: String,
        late_topic: String,
        processor_group: String,
        snapshot_dir: PathBuf,
    ) -> Self {
        Self {
            slots: Arc::new(Semaphore::new(1)),
            brokers,
            input_topic,
            events_topic,
            late_topic,
            processor_group,
            snapshot_dir,
        }
    }
}

#[derive(Deserialize, Default)]
pub struct CreateReplay {
    /// First input offset (inclusive); or `from_time`. Default: the beginning.
    from: Option<i64>,
    /// Last input offset (exclusive); or `to_time`. Default: the processor's
    /// committed offset.
    to: Option<i64>,
    from_time: Option<DateTime<Utc>>,
    to_time: Option<DateTime<Utc>>,
    /// Also write the replayed events to `replay.<id>.stories`.
    #[serde(default)]
    write_output: bool,
}

fn new_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("rp_{:x}{n:02x}", Utc::now().timestamp_millis())
}

pub async fn create(
    State(state): State<Shared>,
    body: Option<Json<CreateReplay>>,
) -> Result<(StatusCode, Json<replays::Replay>), ApiError> {
    let body = body.map(|Json(b)| b).unwrap_or_default();
    let c = db(&state).await?;
    let from = match (body.from, body.from_time) {
        (Some(_), Some(_)) => {
            return Err(ApiError::bad_request("use either `from` or `from_time`"));
        }
        (Some(o), None) => o,
        (None, Some(t)) => reader::offset_from(&**c, t).await?,
        (None, None) => 0,
    };
    let to = match (body.to, body.to_time) {
        (Some(_), Some(_)) => return Err(ApiError::bad_request("use either `to` or `to_time`")),
        (Some(o), None) => Some(o),
        (None, Some(t)) => Some(reader::offset_at(&**c, t).await? + 1),
        (None, None) => None,
    };
    if from < 0 || to.is_some_and(|t| t <= from) {
        return Err(ApiError::bad_request("need 0 <= from < to"));
    }
    let id = new_id();
    let output_topic = body.write_output.then(|| format!("replay.{id}.stories"));
    let row = replays::create(&**c, &id, from, to, output_topic.as_deref()).await?;
    drop(c);

    let runner = &state.replays;
    let request = ReplayRequest {
        brokers: runner.brokers.clone(),
        input_topic: runner.input_topic.clone(),
        events_topic: runner.events_topic.clone(),
        late_topic: runner.late_topic.clone(),
        processor_group: runner.processor_group.clone(),
        snapshot_dir: Some(runner.snapshot_dir.clone()),
        from,
        to,
        output_topic,
    };
    tokio::spawn(execute(state.clone(), id, request));
    Ok((StatusCode::ACCEPTED, Json(row)))
}

async fn execute(state: Shared, id: String, request: ReplayRequest) {
    let _slot = state
        .replays
        .slots
        .clone()
        .acquire_owned()
        .await
        .expect("never closed");
    let record = |outcome| {
        let (state, id) = (state.clone(), id.clone());
        async move {
            match state.pool.get().await {
                Ok(c) => {
                    if let Err(e) = replays::finish(&**c, &id, outcome).await {
                        tracing::error!(replay = id, error = %e, "recording replay result");
                    }
                }
                Err(e) => tracing::error!(replay = id, error = %e, "database unavailable"),
            }
        }
    };
    if let Ok(c) = state.pool.get().await {
        let _ = replays::start(&**c, &id).await;
    }
    tracing::info!(replay = id, from = request.from, to = ?request.to, "replay started");

    let outcome = tokio::task::spawn_blocking(move || {
        replay::run(&request, story_processor::live::production_config)
    })
    .await;
    let outcome = match outcome {
        Ok(Ok(report)) => {
            tracing::info!(
                replay = id,
                identical = report.identical,
                events = report.events.replayed,
                seconds = format!("{:.2}", report.seconds),
                "replay finished"
            );
            metrics::counter!("pulse_api_replays_total",
                "result" => if report.identical { "identical" } else { "different" })
            .increment(1);
            let identical = report.identical;
            serde_json::to_value(&report)
                .map(|v| (v, identical))
                .map_err(|e| e.to_string())
        }
        Ok(Err(e)) => Err(format!("{e:#}")),
        Err(e) => Err(format!("replay task panicked: {e}")),
    };
    if let Err(e) = &outcome {
        tracing::warn!(replay = id, error = e, "replay failed");
        metrics::counter!("pulse_api_replays_total", "result" => "failed").increment(1);
    }
    record(outcome).await;
}

pub async fn get(
    State(state): State<Shared>,
    Path(id): Path<String>,
) -> ApiResult<replays::Replay> {
    let c = db(&state).await?;
    replays::get(&**c, &id)
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::bad_request(format!("no replay {id}")))
}

pub async fn list(State(state): State<Shared>) -> ApiResult<Value> {
    let c = db(&state).await?;
    Ok(Json(json!({ "replays": replays::list(&**c, 20).await? })))
}

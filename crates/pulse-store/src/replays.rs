//! Replay run bookkeeping: queued → running → done | failed.

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::Serialize;
use tokio_postgres::GenericClient;

#[derive(Debug, Clone, Serialize)]
pub struct Replay {
    pub id: String,
    pub status: String,
    pub from_offset: i64,
    pub to_offset: Option<i64>,
    pub output_topic: Option<String>,
    pub requested_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub identical: Option<bool>,
    pub report: Option<serde_json::Value>,
    pub error: Option<String>,
}

const COLUMNS: &str = "id, status, from_offset, to_offset, output_topic, requested_at, started_at,
                       finished_at, identical, report, error";

fn row(r: &tokio_postgres::Row) -> Replay {
    Replay {
        id: r.get(0),
        status: r.get(1),
        from_offset: r.get(2),
        to_offset: r.get(3),
        output_topic: r.get(4),
        requested_at: r.get(5),
        started_at: r.get(6),
        finished_at: r.get(7),
        identical: r.get(8),
        report: r.get(9),
        error: r.get(10),
    }
}

pub async fn create(
    c: &impl GenericClient,
    id: &str,
    from: i64,
    to: Option<i64>,
    output_topic: Option<&str>,
) -> Result<Replay> {
    let r = c
        .query_one(
            &format!(
                "INSERT INTO replays (id, status, from_offset, to_offset, output_topic)
                 VALUES ($1, 'queued', $2, $3, $4) RETURNING {COLUMNS}"
            ),
            &[&id, &from, &to, &output_topic],
        )
        .await?;
    Ok(row(&r))
}

pub async fn start(c: &impl GenericClient, id: &str) -> Result<()> {
    c.execute(
        "UPDATE replays SET status = 'running', started_at = now() WHERE id = $1",
        &[&id],
    )
    .await?;
    Ok(())
}

pub async fn finish(
    c: &impl GenericClient,
    id: &str,
    outcome: std::result::Result<(serde_json::Value, bool), String>,
) -> Result<()> {
    let (status, report, identical, error) = match outcome {
        Ok((report, identical)) => ("done", Some(report), Some(identical), None),
        Err(e) => ("failed", None, None, Some(e)),
    };
    c.execute(
        "UPDATE replays SET status = $2, finished_at = now(), report = $3, identical = $4, error = $5
         WHERE id = $1",
        &[&id, &status, &report, &identical, &error],
    )
    .await?;
    Ok(())
}

pub async fn get(c: &impl GenericClient, id: &str) -> Result<Option<Replay>> {
    Ok(c.query_opt(
        &format!("SELECT {COLUMNS} FROM replays WHERE id = $1"),
        &[&id],
    )
    .await?
    .as_ref()
    .map(row))
}

pub async fn list(c: &impl GenericClient, limit: i64) -> Result<Vec<Replay>> {
    Ok(c.query(
        &format!("SELECT {COLUMNS} FROM replays ORDER BY requested_at DESC LIMIT $1"),
        &[&limit],
    )
    .await?
    .iter()
    .map(row)
    .collect())
}

/// Runs interrupted by a restart will never finish; mark them failed.
pub async fn fail_interrupted(c: &impl GenericClient) -> Result<u64> {
    Ok(c.execute(
        "UPDATE replays SET status = 'failed', finished_at = now(),
                    error = 'interrupted: the API restarted during the run'
             WHERE status IN ('queued', 'running')",
        &[],
    )
    .await?)
}

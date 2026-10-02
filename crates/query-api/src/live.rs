//! Live story events over Server-Sent Events.
//!
//! One background task LISTENs on the sink's Postgres channel. Each
//! notification means a batch has *committed*, so it reads the new rows from
//! `story_events` (in processing order) and fans them out to every connected
//! client. A periodic poll covers any missed notification, and the listener
//! reconnects with backoff.
//!
//! Each SSE event carries `id: <offset>:<seq>`. A reconnecting client sends it
//! back as `Last-Event-ID` (or `?after=`), and the stream first replays what it
//! missed from Postgres, then continues live without gaps or repeats.

use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures::{Stream, StreamExt};
use pulse_store::deadpool_postgres::Pool;
use pulse_store::reader;
use pulse_store::tokio_postgres::{AsyncMessage, NoTls};
use serde::Deserialize;
use tokio::sync::broadcast;

use crate::Shared;
use crate::routes::ApiError;

const CATCH_UP_LIMIT: i64 = 5_000;
const PAGE: i64 = 500;
const POLL_EVERY: Duration = Duration::from_secs(5);

pub type Pos = (i64, i32);

#[derive(Clone)]
pub struct LiveEvent {
    pub pos: Pos,
    pub json: Arc<String>,
}

pub struct Live {
    tx: broadcast::Sender<LiveEvent>,
    /// Last broadcast position, packed as offset << 20 | seq.
    last: Arc<AtomicI64>,
}

fn pack(p: Pos) -> i64 {
    (p.0 << 20) | i64::from(p.1)
}

fn unpack(v: i64) -> Pos {
    (v >> 20, (v & 0xFFFFF) as i32)
}

fn parse_pos(s: &str) -> Option<Pos> {
    let (o, q) = s.split_once(':')?;
    Some((o.parse().ok()?, q.parse().ok()?))
}

async fn latest_event(pool: &Pool) -> anyhow::Result<Pos> {
    let c = pool.get().await?;
    let row = c
        .query_one(
            "SELECT COALESCE(MAX(input_offset), -1), COALESCE(MAX(seq) FILTER (
                 WHERE input_offset = (SELECT MAX(input_offset) FROM story_events)), -1)
             FROM story_events",
            &[],
        )
        .await?;
    Ok((row.get(0), row.get(1)))
}

impl Live {
    pub async fn start(database_url: String, pool: Pool) -> anyhow::Result<Self> {
        let (tx, _) = broadcast::channel(4096);
        let last = Arc::new(AtomicI64::new(pack(latest_event(&pool).await?)));
        tokio::spawn(listen(database_url, pool, tx.clone(), last.clone()));
        Ok(Self { tx, last })
    }

    pub fn position(&self) -> Pos {
        unpack(self.last.load(Ordering::Acquire))
    }
}

/// Forwards committed events to subscribers forever, reconnecting on failure.
async fn listen(url: String, pool: Pool, tx: broadcast::Sender<LiveEvent>, last: Arc<AtomicI64>) {
    let mut backoff = Duration::from_millis(500);
    loop {
        match listen_once(&url, &pool, &tx, &last).await {
            Ok(()) => backoff = Duration::from_millis(500),
            Err(e) => {
                tracing::warn!(error = format!("{e:#}"), retry_in = ?backoff, "live listener failed");
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(15));
            }
        }
    }
}

async fn listen_once(
    url: &str,
    pool: &Pool,
    tx: &broadcast::Sender<LiveEvent>,
    last: &AtomicI64,
) -> anyhow::Result<()> {
    let (client, mut connection) = pulse_store::tokio_postgres::connect(url, NoTls).await?;
    let (notify_tx, mut notify_rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut messages = futures::stream::poll_fn(move |cx| connection.poll_message(cx));
        while let Some(message) = messages.next().await {
            match message {
                Ok(AsyncMessage::Notification(_)) => {
                    let _ = notify_tx.send(());
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
    });
    client
        .batch_execute(&format!("LISTEN {}", pulse_store::EVENTS_CHANNEL))
        .await?;
    tracing::info!("listening for committed story events");

    let mut poll = tokio::time::interval(POLL_EVERY);
    loop {
        tokio::select! {
            n = notify_rx.recv() => if n.is_none() { anyhow::bail!("notification connection closed") },
            _ = poll.tick() => {}
        }
        // Drain everything committed since the last broadcast.
        loop {
            let from = unpack(last.load(Ordering::Acquire));
            let c = pool.get().await?;
            let page = reader::events_after(&**c, from.0, from.1, PAGE).await?;
            drop(c);
            let n = page.len();
            for (pos, payload) in page {
                // No receivers is fine: nobody is watching.
                let _ = tx.send(LiveEvent {
                    pos,
                    json: Arc::new(payload.to_string()),
                });
                last.store(pack(pos), Ordering::Release);
            }
            metrics::counter!("pulse_api_live_events_total").increment(n as u64);
            if (n as i64) < PAGE {
                break;
            }
        }
    }
}

#[derive(Deserialize)]
pub struct StreamParams {
    /// Resume after this position (`<offset>:<seq>`); same as `Last-Event-ID`.
    after: Option<String>,
}

fn sse_event(e: &LiveEvent) -> Event {
    Event::default()
        .id(format!("{}:{}", e.pos.0, e.pos.1))
        .event("story")
        .data(e.json.as_str())
}

pub async fn stream(
    State(state): State<Shared>,
    headers: HeaderMap,
    Query(p): Query<StreamParams>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let resume = match p
        .after
        .as_deref()
        .or_else(|| headers.get("last-event-id").and_then(|v| v.to_str().ok()))
    {
        Some(s) => Some(
            parse_pos(s).ok_or_else(|| ApiError::bad_request("after must be <offset>:<seq>"))?,
        ),
        None => None,
    };
    // Subscribe before catching up so nothing falls in between.
    let mut rx = state.live.tx.subscribe();
    let start = resume.unwrap_or_else(|| state.live.position());
    metrics::gauge!("pulse_api_live_clients").increment(1.0);

    let stream = async_stream::stream! {
        let _guard = ClientGuard;
        let mut last = start;
        yield Ok(Event::default().event("hello").data(
            serde_json::json!({ "position": format!("{}:{}", start.0, start.1) }).to_string(),
        ));

        if resume.is_some() {
            let mut sent = 0;
            loop {
                let page = match state.pool.get().await {
                    Ok(c) => reader::events_after(&**c, last.0, last.1, PAGE).await.unwrap_or_default(),
                    Err(_) => Vec::new(),
                };
                let n = page.len() as i64;
                for (pos, payload) in page {
                    last = pos;
                    sent += 1;
                    yield Ok(Event::default()
                        .id(format!("{}:{}", pos.0, pos.1))
                        .event("story")
                        .data(payload.to_string()));
                }
                if n < PAGE {
                    break;
                }
                if sent >= CATCH_UP_LIMIT {
                    // Too far behind to replay: tell the client to refetch state.
                    yield Ok(Event::default().event("resync").data("{\"reason\":\"too_far_behind\"}"));
                    last = state.live.position();
                    break;
                }
            }
        }

        loop {
            match rx.recv().await {
                Ok(e) if e.pos > last => {
                    last = e.pos;
                    yield Ok(sse_event(&e));
                }
                Ok(_) => {} // already sent during catch-up
                Err(broadcast::error::RecvError::Lagged(missed)) => {
                    yield Ok(Event::default().event("resync").data(
                        serde_json::json!({ "reason": "lagged", "missed": missed }).to_string(),
                    ));
                    last = state.live.position();
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    };
    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}

struct ClientGuard;

impl Drop for ClientGuard {
    fn drop(&mut self) {
        metrics::gauge!("pulse_api_live_clients").decrement(1.0);
    }
}

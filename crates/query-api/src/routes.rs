//! REST handlers.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use pulse_core::proto::v1::{EmbedKind, EmbedRequest};
use pulse_store::reader::{self, Sort, StoryCard, StoryQuery};
use pulse_store::tokio_postgres::GenericClient;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::Shared;

// ---------------------------------------------------------------------------
// Errors

pub struct ApiError(StatusCode, String);

impl ApiError {
    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self(StatusCode::BAD_REQUEST, msg.into())
    }
    fn not_found(msg: impl Into<String>) -> Self {
        Self(StatusCode::NOT_FOUND, msg.into())
    }
    fn unavailable(msg: impl Into<String>) -> Self {
        Self(StatusCode::SERVICE_UNAVAILABLE, msg.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

impl<E: Into<anyhow::Error>> From<E> for ApiError {
    fn from(e: E) -> Self {
        let e = e.into();
        tracing::error!(error = format!("{e:#}"), "request failed");
        Self(StatusCode::INTERNAL_SERVER_ERROR, "internal error".into())
    }
}

pub type ApiResult<T> = Result<Json<T>, ApiError>;

pub async fn db(state: &Shared) -> Result<pulse_store::deadpool_postgres::Object, ApiError> {
    state
        .pool
        .get()
        .await
        .map_err(|e| ApiError::unavailable(format!("database unavailable: {e}")))
}

// ---------------------------------------------------------------------------
// Position: `?at=<offset>` or `?as_of=<RFC 3339>`, default latest.

#[derive(Deserialize, Default)]
pub struct At {
    #[serde(default, deserialize_with = "opt_i64")]
    at: Option<i64>,
    as_of: Option<DateTime<Utc>>,
}

/// Query strings are untyped, and `#[serde(flatten)]` hands numbers over as
/// strings, so accept either form.
fn opt_i64<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<i64>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Int(i64),
        Str(String),
    }
    match Option::<Raw>::deserialize(d)? {
        None => Ok(None),
        Some(Raw::Int(n)) => Ok(Some(n)),
        Some(Raw::Str(s)) => s.trim().parse().map(Some).map_err(serde::de::Error::custom),
    }
}

#[derive(Serialize, Clone, Copy)]
pub struct Position {
    /// Offset in articles.embedded the view reflects.
    pub at: i64,
    /// Pipeline time at that offset (when its input was fetched).
    pub time: Option<DateTime<Utc>>,
    /// The newest position available.
    pub latest: i64,
    pub live: bool,
}

async fn position(c: &impl GenericClient, at: &At) -> Result<Position, ApiError> {
    let latest = reader::latest_offset(c).await?;
    let offset = match (at.at, at.as_of) {
        (Some(_), Some(_)) => return Err(ApiError::bad_request("use either `at` or `as_of`")),
        (Some(o), None) => o.min(latest),
        (None, Some(t)) => reader::offset_at(c, t).await?,
        (None, None) => latest,
    };
    Ok(Position {
        at: offset,
        time: reader::time_at(c, offset).await?,
        latest,
        live: offset >= latest,
    })
}

// ---------------------------------------------------------------------------

pub async fn health(State(state): State<Shared>) -> ApiResult<Value> {
    let c = db(&state).await?;
    let latest = reader::latest_offset(&**c).await?;
    Ok(Json(json!({ "ok": true, "latest_offset": latest })))
}

pub async fn metrics(State(state): State<Shared>) -> String {
    state.metrics.render()
}

#[derive(Deserialize)]
pub struct StoriesParams {
    #[serde(flatten)]
    at: At,
    lang: Option<String>,
    #[serde(default)]
    min_sources: Option<i64>,
    #[serde(default)]
    sort: Sort,
    limit: Option<i64>,
}

#[derive(Serialize)]
pub struct StoriesResponse {
    #[serde(flatten)]
    position: Position,
    stories: Vec<StoryCard>,
}

pub async fn stories(
    State(state): State<Shared>,
    Query(p): Query<StoriesParams>,
) -> ApiResult<StoriesResponse> {
    let c = db(&state).await?;
    let position = position(&**c, &p.at).await?;
    let stories = reader::stories(
        &**c,
        &StoryQuery {
            at: position.at,
            lang: p.lang.filter(|l| !l.is_empty()),
            min_sources: p.min_sources.unwrap_or(1).max(1),
            sort: p.sort,
            limit: p.limit.unwrap_or(50).clamp(1, 500),
            ids: None,
        },
    )
    .await?;
    Ok(Json(StoriesResponse { position, stories }))
}

#[derive(Serialize)]
pub struct StoryResponse {
    #[serde(flatten)]
    position: Position,
    story: reader::StoryDetail,
}

pub async fn story(
    State(state): State<Shared>,
    Path(id): Path<String>,
    Query(at): Query<At>,
) -> ApiResult<StoryResponse> {
    let c = db(&state).await?;
    let position = position(&**c, &at).await?;
    let story = reader::story(&**c, &id, position.at)
        .await?
        .ok_or_else(|| {
            ApiError::not_found(format!(
                "story {id} does not exist at offset {}",
                position.at
            ))
        })?;
    Ok(Json(StoryResponse { position, story }))
}

#[derive(Deserialize)]
pub struct GraphParams {
    #[serde(flatten)]
    at: At,
    limit: Option<i64>,
    min_sources: Option<i64>,
    /// Similarity edges need at least this centroid cosine (centered space).
    min_similarity: Option<f32>,
    edges_per_node: Option<usize>,
}

#[derive(Serialize)]
struct Edge {
    source: String,
    target: String,
    kind: &'static str,
    similarity: Option<f32>,
}

/// Stories as nodes; similarity and lineage as edges; recent splits/merges.
pub async fn graph(State(state): State<Shared>, Query(p): Query<GraphParams>) -> ApiResult<Value> {
    let c = db(&state).await?;
    let position = position(&**c, &p.at).await?;
    let nodes = reader::stories(
        &**c,
        &StoryQuery {
            at: position.at,
            min_sources: p.min_sources.unwrap_or(2).max(1),
            sort: Sort::Sources,
            limit: p.limit.unwrap_or(120).clamp(1, 400),
            ..StoryQuery::default()
        },
    )
    .await?;
    let ids: Vec<String> = nodes.iter().map(|n| n.id.clone()).collect();
    let centroids = reader::centroids(&**c, &ids).await?;

    let min_similarity = p.min_similarity.unwrap_or(0.35);
    let per_node = p.edges_per_node.unwrap_or(3).min(10);
    let mut edges: BTreeMap<(String, String), Edge> = BTreeMap::new();
    for (i, (a, ca)) in centroids.iter().enumerate() {
        let mut nearest: Vec<(f32, &String)> = centroids
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != i)
            .map(|(_, (b, cb))| (ca.iter().zip(cb).map(|(x, y)| x * y).sum::<f32>(), b))
            .filter(|(s, _)| *s >= min_similarity)
            .collect();
        nearest.sort_by(|x, y| y.0.total_cmp(&x.0));
        for (similarity, b) in nearest.into_iter().take(per_node) {
            let key = if a < b {
                (a.clone(), b.clone())
            } else {
                (b.clone(), a.clone())
            };
            edges.entry(key.clone()).or_insert(Edge {
                source: key.0,
                target: key.1,
                kind: "similar",
                similarity: Some(similarity),
            });
        }
    }
    let present: std::collections::HashSet<&str> = ids.iter().map(String::as_str).collect();
    for n in &nodes {
        for parent in &n.parent_ids {
            if present.contains(parent.as_str()) {
                edges.insert(
                    (parent.clone(), n.id.clone()),
                    Edge {
                        source: parent.clone(),
                        target: n.id.clone(),
                        kind: "split",
                        similarity: None,
                    },
                );
            }
        }
    }
    let lineage = reader::lineage_events(&**c, position.at, 30).await?;
    Ok(Json(json!({
        "at": position.at,
        "time": position.time,
        "latest": position.latest,
        "live": position.live,
        "nodes": nodes,
        "edges": edges.into_values().collect::<Vec<_>>(),
        "lineage": lineage,
    })))
}

const STRONG_MATCH: f32 = 0.86;

#[derive(Deserialize)]
pub struct SearchParams {
    q: String,
    limit: Option<i64>,
}

/// Cross-lingual semantic search: articles nearest the query, grouped by story.
pub async fn search(
    State(state): State<Shared>,
    Query(p): Query<SearchParams>,
) -> ApiResult<Value> {
    let q = p.q.trim();
    if q.is_empty() || q.chars().count() > 500 {
        return Err(ApiError::bad_request("q must be 1–500 characters"));
    }
    let mut request = tonic::Request::new(EmbedRequest {
        texts: vec![q.to_owned()],
        kind: EmbedKind::Query as i32,
    });
    request.set_timeout(Duration::from_secs(5));
    let response = state
        .embedder
        .clone()
        .embed(request)
        .await
        .map_err(|e| ApiError::unavailable(format!("embedder unavailable: {}", e.message())))?
        .into_inner();
    let vector = response
        .vectors
        .into_iter()
        .next()
        .map(|v| v.values)
        .ok_or_else(|| ApiError::unavailable("embedder returned no vector"))?;

    let c = db(&state).await?;
    let hits = reader::search(&**c, vector, p.limit.unwrap_or(60).clamp(1, 200)).await?;

    let mut by_story: Vec<(String, Vec<&reader::SearchHit>)> = Vec::new();
    let mut unassigned = Vec::new();
    for hit in &hits {
        match &hit.story_id {
            Some(id) => match by_story.iter_mut().find(|(s, _)| s == id) {
                Some((_, v)) => v.push(hit),
                None => by_story.push((id.clone(), vec![hit])),
            },
            None => unassigned.push(hit),
        }
    }
    let latest = reader::latest_offset(&**c).await?;
    let cards: HashMap<String, StoryCard> = reader::stories(
        &**c,
        &StoryQuery {
            at: latest,
            limit: by_story.len().max(1) as i64,
            ids: Some(by_story.iter().map(|(id, _)| id.clone()).collect()),
            ..StoryQuery::default()
        },
    )
    .await?
    .into_iter()
    .map(|s| (s.id.clone(), s))
    .collect();

    let results: Vec<Value> = by_story
        .into_iter()
        .filter_map(|(id, hits)| {
            let story = cards.get(&id)?;
            let best = hits.first().map_or(0.0, |h| h.similarity);
            Some(json!({
                "story": story,
                "best_similarity": best,
                // e5 query–passage scores are compressed: unrelated texts still
                // score ~0.83–0.85. Below this, show results as "not a close match".
                "strong": best >= STRONG_MATCH,
                "hits": hits,
            }))
        })
        .collect();
    Ok(Json(json!({
        "query": q,
        "model_version": response.model_version,
        "results": results,
        "unassigned": unassigned,
    })))
}

#[derive(Deserialize)]
pub struct TimelineParams {
    buckets: Option<i32>,
}

pub async fn timeline(
    State(state): State<Shared>,
    Query(p): Query<TimelineParams>,
) -> ApiResult<Value> {
    let c = db(&state).await?;
    let buckets = reader::timeline(&**c, p.buckets.unwrap_or(96).clamp(4, 500)).await?;
    Ok(Json(json!({ "buckets": buckets })))
}

/// Totals plus pipeline health: how far each consumer is behind its input.
pub async fn stats(State(state): State<Shared>) -> ApiResult<Value> {
    let c = db(&state).await?;
    let totals = reader::totals(&**c).await?;
    let sink = pulse_store::writer::read_offsets(&c).await?;
    drop(c);

    let pipeline = {
        let mut cache = state.pipeline_cache.lock().await;
        match &*cache {
            Some((at, v)) if at.elapsed() < Duration::from_secs(2) => v.clone(),
            _ => {
                let v = kafka_ends(&state.brokers, &state.topics).await;
                *cache = Some((Instant::now(), v.clone()));
                v
            }
        }
    };
    let sink: Vec<Value> = sink
        .into_iter()
        .map(|((topic, partition), next)| {
            // Transactional topics end with a commit marker the consumer never
            // reads, so a caught-up consumer sits one below the end offset.
            let marker = i64::from(TRANSACTIONAL.contains(&topic.as_str()));
            let end = pipeline[&topic].as_i64();
            json!({ "topic": topic, "partition": partition, "next_offset": next,
                    "lag": end.map(|e| (e - next - marker).max(0)) })
        })
        .collect();
    Ok(Json(
        json!({ "totals": totals, "topics": pipeline, "sink": sink }),
    ))
}

/// Topics written only inside Kafka transactions.
const TRANSACTIONAL: &[&str] = &[
    pulse_core::topics::ARTICLES_EMBEDDED,
    pulse_core::topics::STORIES_EVENTS,
    pulse_core::topics::ARTICLES_LATE,
];

/// Total end offset per topic (sum over partitions). `null` when Kafka is down.
async fn kafka_ends(brokers: &str, topics: &[String]) -> Value {
    let (brokers, topics) = (brokers.to_owned(), topics.to_vec());
    tokio::task::spawn_blocking(move || {
        use rdkafka::consumer::{BaseConsumer, Consumer};
        let timeout = Duration::from_secs(2);
        let consumer: Option<BaseConsumer> = rdkafka::ClientConfig::new()
            .set("bootstrap.servers", &brokers)
            .create()
            .ok();
        let mut out = serde_json::Map::new();
        for topic in topics {
            let end = consumer.as_ref().and_then(|c| {
                let meta = c.fetch_metadata(Some(&topic), timeout).ok()?;
                let partitions: Vec<i32> = meta
                    .topics()
                    .first()?
                    .partitions()
                    .iter()
                    .map(|p| p.id())
                    .collect();
                partitions
                    .into_iter()
                    .map(|p| {
                        c.fetch_watermarks(&topic, p, timeout)
                            .ok()
                            .map(|(_, high)| high)
                    })
                    .sum::<Option<i64>>()
            });
            out.insert(topic, json!(end));
        }
        Value::Object(out)
    })
    .await
    .unwrap_or(Value::Null)
}

/// The feed catalog with how much each source contributed.
pub async fn sources(State(state): State<Shared>) -> ApiResult<Value> {
    let text = tokio::fs::read_to_string(&state.sources_path)
        .await
        .map_err(|e| {
            ApiError::unavailable(format!("reading {}: {e}", state.sources_path.display()))
        })?;
    let catalog: toml::Table = text
        .parse()
        .map_err(|e| ApiError::unavailable(format!("parsing sources: {e}")))?;

    let c = db(&state).await?;
    let counts: HashMap<String, (i64, Option<DateTime<Utc>>)> = c
        .query(
            "SELECT split_part(source_id, ':', 1), COUNT(*), MAX(fetched_at) FROM articles GROUP BY 1",
            &[],
        )
        .await?
        .iter()
        .map(|r| (r.get(0), (r.get(1), r.get(2))))
        .collect();

    let field = |s: &toml::Table, k: &str| s.get(k).and_then(|v| v.as_str()).map(str::to_owned);
    let sources: Vec<Value> = catalog
        .get("source")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_table())
        .map(|s| {
            let id = field(s, "id").unwrap_or_default();
            let (articles, last) = counts.get(&id).copied().unwrap_or((0, None));
            json!({
                "id": id,
                "kind": field(s, "kind").unwrap_or_else(|| "rss".into()),
                "url": field(s, "url"),
                "lang": field(s, "lang"),
                "category": field(s, "category"),
                "region": field(s, "region"),
                "enabled": s.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true),
                "articles": articles,
                "last_fetched_at": last,
            })
        })
        .collect();
    Ok(Json(json!({ "sources": sources })))
}

#[cfg(test)]
mod tests {
    use axum::extract::Query;
    use axum::http::Uri;

    use super::*;

    #[test]
    fn flattened_position_parses_from_query_string() {
        let uri: Uri = "/api/stories?at=5036&limit=40&min_sources=2"
            .parse()
            .unwrap();
        let Query(p) = Query::<StoriesParams>::try_from_uri(&uri).unwrap();
        assert_eq!(p.at.at, Some(5036));
        assert_eq!(p.limit, Some(40));

        let uri: Uri = "/api/graph?as_of=2026-10-01T12:00:00Z".parse().unwrap();
        let Query(p) = Query::<StoriesParams>::try_from_uri(&uri).unwrap();
        assert!(p.at.at.is_none() && p.at.as_of.is_some());

        let uri: Uri = "/api/stories?at=abc".parse().unwrap();
        assert!(Query::<StoriesParams>::try_from_uri(&uri).is_err());
    }
}

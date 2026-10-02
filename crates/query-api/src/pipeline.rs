//! `GET /api/pipeline`: health of every stage, for the metrics panel.
//!
//! Throughputs and latencies come from Prometheus (server-side, since browsers
//! can't query it cross-origin), each as a latest value plus a 30-minute series
//! for sparklines. Consumer lag per stage comes straight from Kafka: each
//! consumer group's committed offsets against its input topic's end.

use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::State;
use serde::Serialize;
use serde_json::{Value, json};

use crate::Shared;
use crate::routes::{ApiResult, db};

const WINDOW: Duration = Duration::from_secs(30 * 60);
const STEP_SECONDS: u64 = 30;
const CACHE_FOR: Duration = Duration::from_secs(5);

struct Query {
    key: &'static str,
    label: &'static str,
    unit: &'static str,
    promql: &'static str,
}

const QUERIES: &[Query] = &[
    Query {
        key: "ingest_rate",
        label: "Articles ingested",
        unit: "/min",
        promql: "sum(rate(pulse_ingestor_published_total[5m])) * 60",
    },
    Query {
        key: "embed_rate",
        label: "Embedding throughput",
        unit: "texts/s",
        promql: "sum(rate(pulse_embedder_texts_total[2m]))",
    },
    Query {
        key: "embed_batch",
        label: "Mean batch size",
        unit: "texts",
        promql: "sum(rate(pulse_embedder_batch_size_sum[5m])) / sum(rate(pulse_embedder_batch_size_count[5m]))",
    },
    Query {
        key: "embed_p95",
        label: "Inference p95",
        unit: "ms",
        promql: "histogram_quantile(0.95, sum by (le) (rate(pulse_embedder_inference_seconds_bucket[5m]))) * 1000",
    },
    Query {
        key: "pipeline_latency",
        label: "Fetch → embedded p50",
        unit: "s",
        promql: "histogram_quantile(0.5, sum by (le) (rate(pulse_relay_ingest_to_embedded_seconds_bucket[5m])))",
    },
    Query {
        key: "process_rate",
        label: "Articles clustered",
        unit: "/min",
        promql: "sum(rate(pulse_processor_inputs_total[5m])) * 60",
    },
    Query {
        key: "events_rate",
        label: "Story events",
        unit: "/min",
        promql: "sum(rate(pulse_processor_events_total[5m])) * 60",
    },
    Query {
        key: "dedup_rate",
        label: "Near-duplicate rate",
        unit: "%",
        promql: "100 * sum(rate(pulse_processor_inputs_total{outcome=\"near_duplicate\"}[30m])) / sum(rate(pulse_processor_inputs_total[30m]))",
    },
    Query {
        key: "late_rate",
        label: "Late arrivals",
        unit: "%",
        promql: "100 * sum(rate(pulse_processor_late_total[30m])) / sum(rate(pulse_processor_inputs_total[30m]))",
    },
    Query {
        key: "watermark_lag",
        label: "Watermark behind now",
        unit: "h",
        promql: "max(pulse_processor_watermark_lag_seconds) / 3600",
    },
];

#[derive(Serialize)]
struct Metric {
    key: &'static str,
    label: &'static str,
    unit: &'static str,
    value: Option<f64>,
    /// `[unix seconds, value]`, oldest first.
    series: Vec<(f64, f64)>,
}

async fn query_range(http: &reqwest::Client, base: &str, promql: &str) -> Option<Vec<(f64, f64)>> {
    let end = chrono::Utc::now().timestamp() as u64;
    let start = end - WINDOW.as_secs();
    let body: Value = http
        .get(format!("{}/api/v1/query_range", base.trim_end_matches('/')))
        .query(&[
            ("query", promql.to_owned()),
            ("start", start.to_string()),
            ("end", end.to_string()),
            ("step", STEP_SECONDS.to_string()),
        ])
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    let values = body["data"]["result"].get(0)?["values"].as_array()?.clone();
    Some(
        values
            .iter()
            .filter_map(|p| {
                let t = p.get(0)?.as_f64()?;
                let v: f64 = p.get(1)?.as_str()?.parse().ok()?;
                v.is_finite().then_some((t, v))
            })
            .collect(),
    )
}

/// Committed offsets of `group` on `topic` (summed over partitions) and the
/// topic's end, or `None` when Kafka is unreachable.
fn group_position(brokers: &str, group: &str, topic: &str) -> Option<(i64, i64)> {
    use rdkafka::consumer::{BaseConsumer, Consumer};
    use rdkafka::{Offset, TopicPartitionList};
    let timeout = Duration::from_secs(2);
    let consumer: BaseConsumer = rdkafka::ClientConfig::new()
        .set("bootstrap.servers", brokers)
        .set("group.id", group)
        .set("enable.auto.commit", "false")
        .create()
        .ok()?;
    let meta = consumer.fetch_metadata(Some(topic), timeout).ok()?;
    let partitions: Vec<i32> = meta
        .topics()
        .first()?
        .partitions()
        .iter()
        .map(|p| p.id())
        .collect();
    let mut tpl = TopicPartitionList::new();
    for p in &partitions {
        tpl.add_partition(topic, *p);
    }
    let committed = consumer.committed_offsets(tpl, timeout).ok()?;
    let position: i64 = committed
        .elements()
        .iter()
        .map(|e| match e.offset() {
            Offset::Offset(o) => o,
            _ => 0,
        })
        .sum();
    let end: i64 = partitions
        .iter()
        .map(|&p| {
            consumer
                .fetch_watermarks(topic, p, timeout)
                .ok()
                .map(|(_, h)| h)
        })
        .sum::<Option<i64>>()?;
    Some((position, end))
}

pub async fn pipeline(State(state): State<Shared>) -> ApiResult<Value> {
    {
        let cache = state.pipeline_panel_cache.lock().await;
        if let Some((at, v)) = &*cache {
            if at.elapsed() < CACHE_FOR {
                return Ok(Json(v.clone()));
            }
        }
    }

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .map_err(anyhow::Error::from)?;
    let series = futures::future::join_all(
        QUERIES
            .iter()
            .map(|q| query_range(&http, &state.prometheus_url, q.promql)),
    )
    .await;
    let metrics: Vec<Metric> = QUERIES
        .iter()
        .zip(series)
        .map(|(q, s)| {
            let series = s.unwrap_or_default();
            Metric {
                key: q.key,
                label: q.label,
                unit: q.unit,
                value: series.last().map(|p| p.1),
                series,
            }
        })
        .collect();

    // Stage lags. Transactional inputs end with a commit marker the consumer
    // never reads, so a caught-up consumer sits one below the end.
    let brokers = state.brokers.clone();
    let stages = tokio::task::spawn_blocking(move || {
        let stage = |name: &str, group: &str, topic: &str, marker: i64| {
            let pos = group_position(&brokers, group, topic);
            json!({
                "stage": name,
                "group": group,
                "topic": topic,
                "position": pos.map(|p| p.0),
                "end": pos.map(|p| p.1),
                "lag": pos.map(|(p, e)| (e - p - marker).max(0)),
            })
        };
        vec![
            stage(
                "Embed relay",
                "embed-relay",
                pulse_core::topics::ARTICLES_RAW,
                0,
            ),
            stage(
                "Story processor",
                "story-processor",
                pulse_core::topics::ARTICLES_EMBEDDED,
                1,
            ),
        ]
    })
    .await
    .unwrap_or_default();

    let c = db(&state).await?;
    let sink = pulse_store::writer::read_offsets(&c).await?;
    drop(c);
    let ends = {
        let (brokers, topics) = (state.brokers.clone(), state.topics.clone());
        tokio::task::spawn_blocking(move || {
            use rdkafka::consumer::{BaseConsumer, Consumer};
            let consumer: Option<BaseConsumer> = rdkafka::ClientConfig::new()
                .set("bootstrap.servers", &brokers)
                .create()
                .ok();
            topics
                .into_iter()
                .map(|t| {
                    let end = consumer
                        .as_ref()
                        .and_then(|c| c.fetch_watermarks(&t, 0, Duration::from_secs(2)).ok())
                        .map(|w| w.1);
                    (t, end)
                })
                .collect::<std::collections::HashMap<_, _>>()
        })
        .await
        .unwrap_or_default()
    };
    let mut stages = stages;
    let sink_lag: Option<i64> = sink
        .iter()
        .map(|((topic, _), next)| {
            ends.get(topic)
                .copied()
                .flatten()
                .map(|e| (e - next - 1).max(0))
        })
        .sum();
    stages.push(json!({ "stage": "Story sink", "group": "postgres", "topic": "articles.embedded + stories.events",
                        "lag": sink_lag }));

    let value = json!({
        "generated_at": chrono::Utc::now(),
        "window_seconds": WINDOW.as_secs(),
        "metrics": metrics,
        "stages": stages,
    });
    *state.pipeline_panel_cache.lock().await = Some((Instant::now(), value.clone()));
    Ok(Json(value))
}

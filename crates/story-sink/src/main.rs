//! Writes committed story events and articles to Postgres, exactly once.
//!
//! Reads `articles.embedded` and `stories.events` (read_committed) from the
//! offsets stored in Postgres, and applies each batch together with the new
//! offsets in one database transaction (see `pulse_store::writer`). No Kafka
//! consumer commits: the database is the source of truth for progress. On any
//! error it exits; restarting resumes from the last committed batch.
//!
//! `story-sink reset` empties the read model so it rebuilds from the start.

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use metrics::{counter, gauge, histogram};
use prost::Message as _;
use pulse_core::config::{Settings, env_or};
use pulse_core::proto::v1::{EmbeddedArticle, StoryEvent};
use pulse_core::{kafka, topics};
use pulse_store::writer::{self, Batch};
use rdkafka::consumer::{Consumer, StreamConsumer};
use rdkafka::{Message, Offset, TopicPartitionList};
use tokio_util::sync::CancellationToken;

const META_TIMEOUT: Duration = Duration::from_secs(10);

struct Config {
    brokers: String,
    database_url: String,
    articles_topic: String,
    events_topic: String,
    max_batch: usize,
    linger: Duration,
}

#[tokio::main]
async fn main() -> Result<()> {
    pulse_core::telemetry::init("story-sink");
    let settings = Settings::from_env();
    let cfg = Config {
        brokers: settings.kafka_brokers,
        database_url: settings.database_url,
        articles_topic: env_or("PULSE_SINK_ARTICLES_TOPIC", topics::ARTICLES_EMBEDDED),
        events_topic: env_or("PULSE_SINK_EVENTS_TOPIC", topics::STORIES_EVENTS),
        max_batch: env_or("PULSE_SINK_MAX_BATCH", "1000").parse()?,
        linger: Duration::from_millis(env_or("PULSE_SINK_LINGER_MS", "100").parse()?),
    };

    let mut db = pulse_store::connect(&cfg.database_url).await?;
    let applied = pulse_store::migrate(&mut db).await?;
    if !applied.is_empty() {
        tracing::info!(?applied, "migrations applied");
    }

    match std::env::args().nth(1).as_deref() {
        Some("reset") => {
            writer::reset(&db).await?;
            tracing::info!("read model emptied; the next run rebuilds it from the start");
            return Ok(());
        }
        Some(other) => bail!("unknown command `{other}` (usage: story-sink [reset])"),
        None => {}
    }

    let port: u16 = env_or("PULSE_SINK_METRICS_PORT", "9104").parse()?;
    metrics_exporter_prometheus::PrometheusBuilder::new()
        .with_http_listener(SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)))
        .install()?;

    let shutdown = CancellationToken::new();
    let on_signal = shutdown.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            tracing::info!("shutting down after the current batch");
            on_signal.cancel();
        }
    });
    run(&cfg, &mut db, shutdown).await
}

async fn run(
    cfg: &Config,
    db: &mut pulse_store::tokio_postgres::Client,
    shutdown: CancellationToken,
) -> Result<()> {
    // The group id only labels the client; offsets live in Postgres.
    let consumer: StreamConsumer =
        kafka::exactly_once_consumer(&cfg.brokers, "story-sink").create()?;
    let stored = writer::read_offsets(db).await?;

    let mut assignment = TopicPartitionList::new();
    for topic in [&cfg.articles_topic, &cfg.events_topic] {
        let metadata =
            tokio::task::block_in_place(|| consumer.fetch_metadata(Some(topic), META_TIMEOUT))?;
        let partitions: Vec<i32> = metadata
            .topics()
            .iter()
            .flat_map(|t| t.partitions().iter().map(|p| p.id()))
            .collect();
        if partitions.is_empty() {
            bail!("topic {topic} not found");
        }
        for p in partitions {
            let offset = match stored.get(&(topic.clone(), p)) {
                Some(&o) => Offset::Offset(o),
                None => Offset::Beginning,
            };
            tracing::info!(topic, partition = p, ?offset, "resuming");
            assignment.add_partition_offset(topic, p, offset)?;
        }
    }
    consumer.assign(&assignment)?;

    let lag_brokers = cfg.brokers.clone();
    let lag_topics = [cfg.articles_topic.clone(), cfg.events_topic.clone()];
    let mut last_lag = Instant::now() - Duration::from_secs(60);

    loop {
        let messages = tokio::select! {
            _ = shutdown.cancelled() => break,
            m = collect(&consumer, cfg) => m?,
        };
        let mut batch = Batch::default();
        for msg in &messages {
            let key = (msg.topic().to_owned(), msg.partition());
            batch.offsets.insert(key, msg.offset() + 1);
            let payload = msg.payload().unwrap_or_default();
            if msg.topic() == cfg.articles_topic {
                match EmbeddedArticle::decode(payload) {
                    Ok(a) => batch.articles.push((msg.offset(), a)),
                    Err(_) => counter!("pulse_sink_undecodable_total").increment(1),
                }
            } else {
                match StoryEvent::decode(payload) {
                    Ok(e) => batch.events.push(e),
                    Err(_) => counter!("pulse_sink_undecodable_total").increment(1),
                }
            }
        }

        let started = Instant::now();
        writer::apply(db, &batch).await.context("applying batch")?;
        histogram!("pulse_sink_batch_seconds").record(started.elapsed().as_secs_f64());
        counter!("pulse_sink_articles_total").increment(batch.articles.len() as u64);
        counter!("pulse_sink_events_total").increment(batch.events.len() as u64);
        tracing::debug!(
            articles = batch.articles.len(),
            events = batch.events.len(),
            "batch applied"
        );

        if last_lag.elapsed() >= Duration::from_secs(5) {
            last_lag = Instant::now();
            report_lag(&lag_brokers, &lag_topics, &batch.offsets).await;
        }
    }
    tracing::info!("stopped");
    Ok(())
}

async fn collect(
    consumer: &StreamConsumer,
    cfg: &Config,
) -> Result<Vec<rdkafka::message::OwnedMessage>> {
    let first = consumer.recv().await?.detach();
    let deadline = tokio::time::Instant::now() + cfg.linger;
    let mut batch = vec![first];
    while batch.len() < cfg.max_batch {
        match tokio::time::timeout_at(deadline, consumer.recv()).await {
            Ok(msg) => batch.push(msg?.detach()),
            Err(_) => break,
        }
    }
    Ok(batch)
}

/// Records end − position per topic (records not yet in Postgres).
async fn report_lag(brokers: &str, topics: &[String; 2], positions: &BTreeMap<(String, i32), i64>) {
    let (brokers, topics, positions) = (brokers.to_owned(), topics.clone(), positions.clone());
    let _ = tokio::task::spawn_blocking(move || -> Result<()> {
        let consumer: rdkafka::consumer::BaseConsumer = rdkafka::ClientConfig::new()
            .set("bootstrap.servers", &brokers)
            .create()?;
        for topic in &topics {
            if let Some(&pos) = positions.get(&(topic.clone(), 0)) {
                let (_, high) = consumer.fetch_watermarks(topic, 0, META_TIMEOUT)?;
                gauge!("pulse_sink_lag_records", "topic" => topic.clone())
                    .set((high - pos).max(0) as f64);
            }
        }
        Ok(())
    })
    .await;
}

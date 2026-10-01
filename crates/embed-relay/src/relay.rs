//! Consume-transform-produce, exactly once.
//!
//! Each batch of `articles.raw` messages is embedded, then written to
//! `articles.embedded` together with the consumed offsets in one Kafka
//! transaction. Either both the outputs and the offsets commit, or neither does,
//! so a crash at any point neither loses nor duplicates articles. On any error
//! the transaction is aborted and the process exits; restarting resumes from
//! the last committed offsets.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use metrics::{counter, histogram};
use prost::Message as _;
use pulse_core::proto::v1::{Article, EmbeddedArticle};
use pulse_core::{kafka, topics};
use rdkafka::consumer::{Consumer, StreamConsumer};
use rdkafka::message::{Header, OwnedHeaders, OwnedMessage};
use rdkafka::producer::{FutureProducer, FutureRecord, Producer};
use rdkafka::{Message, Offset, TopicPartitionList};
use tokio::task::block_in_place;
use tokio_util::sync::CancellationToken;

use crate::embedder::Embedder;

const TXN_TIMEOUT: Duration = Duration::from_secs(30);
pub const CONTENT_TYPE: &str = "application/x-protobuf; messageType=pulse.v1.EmbeddedArticle";

pub struct Config {
    pub brokers: String,
    /// Consumer group; also the base of the transactional id.
    pub group_id: String,
    pub output_topic: String,
    pub max_batch: usize,
    pub linger: Duration,
}

/// Text sent to the model for an article.
pub fn embedding_text(a: &Article) -> String {
    if a.summary.is_empty() {
        a.title.clone()
    } else {
        format!("{}\n{}", a.title, a.summary)
    }
}

pub async fn run(cfg: Config, embedder: Embedder, shutdown: CancellationToken) -> Result<()> {
    let consumer: StreamConsumer =
        kafka::exactly_once_consumer(&cfg.brokers, &cfg.group_id).create()?;
    consumer.subscribe(&[topics::ARTICLES_RAW])?;
    // Stable across restarts so a restarted relay fences off a zombie instance.
    let transactional_id = format!("{}-0", cfg.group_id);
    let producer: FutureProducer =
        kafka::transactional_producer(&cfg.brokers, &transactional_id).create()?;
    block_in_place(|| producer.init_transactions(TXN_TIMEOUT)).context("init transactions")?;
    tracing::info!(
        "relaying {} → {} (max_batch={}, linger={:?})",
        topics::ARTICLES_RAW,
        cfg.output_topic,
        cfg.max_batch,
        cfg.linger
    );

    loop {
        let batch = tokio::select! {
            _ = shutdown.cancelled() => break,
            batch = collect(&consumer, &cfg) => batch?,
        };
        let started = Instant::now();
        if let Err(e) = relay_batch(&consumer, &producer, &embedder, &cfg, &batch).await {
            // Outputs and offsets were never committed; restart replays this batch.
            let _ = block_in_place(|| producer.abort_transaction(TXN_TIMEOUT));
            return Err(e);
        }
        histogram!("pulse_relay_batch_seconds").record(started.elapsed().as_secs_f64());
    }
    tracing::info!("stopped");
    Ok(())
}

/// Waits for one message, then gathers more until `max_batch` or `linger` elapses.
async fn collect(consumer: &StreamConsumer, cfg: &Config) -> Result<Vec<OwnedMessage>> {
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

async fn relay_batch(
    consumer: &StreamConsumer,
    producer: &FutureProducer,
    embedder: &Embedder,
    cfg: &Config,
    batch: &[OwnedMessage],
) -> Result<()> {
    // Next offset to consume per partition = what we commit.
    let mut next_offsets: BTreeMap<i32, i64> = BTreeMap::new();
    let mut articles = Vec::with_capacity(batch.len());
    for msg in batch {
        next_offsets
            .entry(msg.partition())
            .and_modify(|o| *o = (*o).max(msg.offset() + 1))
            .or_insert(msg.offset() + 1);
        match msg.payload().map(Article::decode) {
            Some(Ok(a)) => articles.push(a),
            _ => {
                // Skipped but still committed: a poison message must not block the pipeline.
                counter!("pulse_relay_undecodable_total").increment(1);
                tracing::warn!(
                    partition = msg.partition(),
                    offset = msg.offset(),
                    "undecodable message skipped"
                );
            }
        }
    }

    let embed_started = Instant::now();
    let embedded = embedder
        .embed_passages(articles.iter().map(embedding_text).collect())
        .await?;
    histogram!("pulse_relay_embed_seconds").record(embed_started.elapsed().as_secs_f64());

    let commit_started = Instant::now();
    block_in_place(|| producer.begin_transaction())?;
    let now_ms = chrono::Utc::now().timestamp_millis();
    for (article, vector) in articles.into_iter().zip(embedded.vectors) {
        let id = article.id.clone();
        let fetched_at_ms = article.fetched_at_ms;
        let payload = EmbeddedArticle {
            article: Some(article),
            vector,
            model_version: embedded.model_version.clone(),
        }
        .encode_to_vec();
        let record = FutureRecord::to(&cfg.output_topic)
            .key(&id)
            .payload(&payload)
            .headers(OwnedHeaders::new().insert(Header {
                key: "content-type",
                value: Some(CONTENT_TYPE),
            }));
        // Delivery is confirmed by commit_transaction, which fails if any send failed.
        drop(producer.send_result(record).map_err(|(e, _)| e)?);
        histogram!("pulse_relay_ingest_to_embedded_seconds")
            .record((now_ms - fetched_at_ms).max(0) as f64 / 1000.0);
    }

    let mut offsets = TopicPartitionList::new();
    for (partition, offset) in &next_offsets {
        offsets.add_partition_offset(topics::ARTICLES_RAW, *partition, Offset::Offset(*offset))?;
    }
    let group = consumer
        .group_metadata()
        .context("consumer has no group metadata")?;
    block_in_place(|| {
        producer.send_offsets_to_transaction(&offsets, &group, TXN_TIMEOUT)?;
        producer.commit_transaction(TXN_TIMEOUT)
    })
    .context("committing transaction")?;
    histogram!("pulse_relay_commit_seconds").record(commit_started.elapsed().as_secs_f64());

    counter!("pulse_relay_batches_total").increment(1);
    counter!("pulse_relay_articles_total").increment(batch.len() as u64);
    histogram!("pulse_relay_batch_size").record(batch.len() as f64);
    tracing::debug!(articles = batch.len(), "batch committed");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedding_text_joins_title_and_summary() {
        let mut a = Article {
            title: "Headline".into(),
            ..Default::default()
        };
        assert_eq!(embedding_text(&a), "Headline");
        a.summary = "Details".into();
        assert_eq!(embedding_text(&a), "Headline\nDetails");
    }
}

//! Live mode: articles.embedded → engine → stories.events, exactly once.
//!
//! Startup:
//! 1. Read the consumer group's committed offset `C` (where outputs end).
//! 2. Load the newest snapshot with `next_offset ≤ C`, at offset `S`.
//! 3. **Silently replay** `[S, C)`: re-process those inputs to rebuild state,
//!    discarding outputs, which were already committed. Determinism makes the
//!    rebuilt state identical to the one that crashed.
//! 4. Consume from `C`.
//!
//! Each epoch (a batch of inputs) produces its story events, late articles and
//! the next input offset in **one Kafka transaction**. Snapshots are written
//! only after a commit, so a snapshot never contains uncommitted progress. On
//! any error the transaction is aborted and the process exits; restarting
//! recovers as above.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use metrics::{counter, gauge, histogram};
use prost::Message as _;
use pulse_core::kafka;
use pulse_core::proto::v1::{EmbeddedArticle, StoryEvent, story_event::Kind};
use rdkafka::consumer::{Consumer, StreamConsumer};
use rdkafka::message::{Header, OwnedHeaders, OwnedMessage};
use rdkafka::producer::{FutureProducer, FutureRecord, Producer};
use rdkafka::{Message, Offset, TopicPartitionList};
use tokio::task::block_in_place;
use tokio_util::sync::CancellationToken;

use crate::engine::{Config, Engine, Outcome};
use crate::snapshot::{self, LocalStore};

const TXN_TIMEOUT: Duration = Duration::from_secs(30);
const META_TIMEOUT: Duration = Duration::from_secs(10);
const PARTITION: i32 = 0;
pub const EVENT_CONTENT_TYPE: &str = "application/x-protobuf; messageType=pulse.v1.StoryEvent";

pub struct LiveConfig {
    pub brokers: String,
    /// Consumer group; also the base of the transactional id.
    pub group_id: String,
    pub input_topic: String,
    pub output_topic: String,
    pub late_topic: String,
    pub snapshot_dir: PathBuf,
    pub snapshot_every_messages: u64,
    pub snapshot_every: Duration,
    pub keep_snapshots: usize,
    pub max_batch: usize,
    pub linger: Duration,
}

/// Builds the engine config once the input's model version is known.
pub type ConfigFor = Box<dyn FnOnce(&str) -> Result<Config> + Send>;

/// The engine configuration the live processor runs with: defaults plus the
/// frozen centering for the input's model (`PULSE_CENTERING` overrides the
/// path). Replay builds its engine the same way, so fingerprints match.
pub fn production_config(model_version: &str) -> Result<Config> {
    let path = std::env::var("PULSE_CENTERING")
        .map(PathBuf::from)
        .unwrap_or_else(|_| crate::centering::default_path(model_version));
    let centering = if path.exists() {
        Some(std::sync::Arc::new(crate::centering::Centering::load(
            &path,
        )?))
    } else {
        tracing::warn!(
            "no centering file at {}; clustering raw vectors",
            path.display()
        );
        None
    };
    Ok(Config {
        centering,
        ..Config::default()
    })
}

pub async fn run(
    cfg: LiveConfig,
    config_for: ConfigFor,
    shutdown: CancellationToken,
) -> Result<()> {
    let consumer: StreamConsumer =
        kafka::exactly_once_consumer(&cfg.brokers, &cfg.group_id).create()?;
    let metadata =
        block_in_place(|| consumer.fetch_metadata(Some(&cfg.input_topic), META_TIMEOUT))?;
    let partitions = metadata
        .topics()
        .first()
        .map_or(0, |t| t.partitions().len());
    if partitions != 1 {
        bail!(
            "{} has {partitions} partitions; the Story Processor needs exactly 1 \
             (log order is processing order, which makes output deterministic)",
            cfg.input_topic
        );
    }

    let transactional_id = format!("{}-0", cfg.group_id);
    let producer: FutureProducer =
        kafka::transactional_producer(&cfg.brokers, &transactional_id).create()?;
    // Also aborts any transaction a crashed predecessor left open (fencing).
    block_in_place(|| producer.init_transactions(TXN_TIMEOUT)).context("init transactions")?;

    let (low, _) =
        block_in_place(|| consumer.fetch_watermarks(&cfg.input_topic, PARTITION, META_TIMEOUT))?;
    let committed = committed_offset(&consumer, &cfg.input_topic)?.unwrap_or(low);

    let Some(model_version) = first_model_version(&cfg, low, &shutdown).await? else {
        return Ok(()); // shut down while waiting for input
    };
    let engine_cfg = config_for(&model_version)?;
    tracing::info!(
        model = model_version,
        fingerprint = engine_cfg.fingerprint(),
        centering = engine_cfg.centering.is_some(),
        "engine configured"
    );

    let store = LocalStore::open(&cfg.snapshot_dir)?;
    let removed = store.prune(cfg.keep_snapshots, Some(committed))?;
    if removed > 0 {
        tracing::info!(removed, "pruned snapshots");
    }

    // Restore + silent replay.
    let restore_started = Instant::now();
    let (engine, snapshot_offset) = match store.latest_at_or_before(committed)? {
        Some((offset, bytes)) => {
            let (header, engine) = snapshot::decode(&bytes, engine_cfg)
                .with_context(|| format!("restoring snapshot at offset {offset}"))?;
            tracing::info!(
                offset,
                articles = header.articles,
                stories = header.stories,
                bytes = bytes.len(),
                "snapshot loaded"
            );
            (engine, offset)
        }
        None => (Engine::new(engine_cfg), low),
    };
    let (mut engine, replayed) = replay(&cfg, engine, snapshot_offset, committed).await?;
    let restore_seconds = restore_started.elapsed().as_secs_f64();
    gauge!("pulse_processor_restore_seconds").set(restore_seconds);
    gauge!("pulse_processor_replayed_messages").set(replayed as f64);
    tracing::info!(
        snapshot_offset,
        committed,
        replayed,
        seconds = format!("{restore_seconds:.2}"),
        "state recovered; consuming from committed offset"
    );

    let mut assignment = TopicPartitionList::new();
    assignment.add_partition_offset(&cfg.input_topic, PARTITION, Offset::Offset(committed))?;
    consumer.assign(&assignment)?;

    let mut since_snapshot = 0u64;
    let mut last_snapshot = Instant::now();
    let mut next_offset = committed;
    loop {
        let batch = tokio::select! {
            _ = shutdown.cancelled() => break,
            batch = collect(&consumer, &cfg) => batch?,
        };
        let started = Instant::now();
        match process_epoch(&cfg, &consumer, &producer, &mut engine, &batch) {
            Ok(next) => next_offset = next,
            Err(e) => {
                // In-memory state is now ahead of what committed; never continue.
                let _ = block_in_place(|| producer.abort_transaction(TXN_TIMEOUT));
                return Err(e);
            }
        }
        histogram!("pulse_processor_epoch_seconds").record(started.elapsed().as_secs_f64());
        report_state(&engine, next_offset);

        since_snapshot += batch.len() as u64;
        if since_snapshot >= cfg.snapshot_every_messages
            || last_snapshot.elapsed() >= cfg.snapshot_every
        {
            take_snapshot(&store, &engine, next_offset, cfg.keep_snapshots)?;
            since_snapshot = 0;
            last_snapshot = Instant::now();
        }
    }

    if since_snapshot > 0 {
        take_snapshot(&store, &engine, next_offset, cfg.keep_snapshots)?;
    }
    tracing::info!(next_offset, "stopped");
    Ok(())
}

fn committed_offset(consumer: &StreamConsumer, topic: &str) -> Result<Option<i64>> {
    let mut tpl = TopicPartitionList::new();
    tpl.add_partition(topic, PARTITION);
    let committed = block_in_place(|| consumer.committed_offsets(tpl, META_TIMEOUT))?;
    Ok(committed.elements().first().and_then(|e| match e.offset() {
        Offset::Offset(o) => Some(o),
        _ => None,
    }))
}

/// Model version of the first input, waiting for one to exist.
async fn first_model_version(
    cfg: &LiveConfig,
    low: i64,
    shutdown: &CancellationToken,
) -> Result<Option<String>> {
    loop {
        let (brokers, topic) = (cfg.brokers.clone(), cfg.input_topic.clone());
        let found = tokio::task::spawn_blocking(move || -> Result<Option<String>> {
            let consumer: rdkafka::consumer::BaseConsumer = rdkafka::ClientConfig::new()
                .set("bootstrap.servers", &brokers)
                .create()?;
            let (_, high) = consumer.fetch_watermarks(&topic, PARTITION, META_TIMEOUT)?;
            let mut version = None;
            // A few records in, in case the first ones are undecodable.
            kafka::scan_range(&brokers, &topic, PARTITION, low, high.min(low + 16), |m| {
                if version.is_none() {
                    version = m
                        .payload()
                        .and_then(|p| EmbeddedArticle::decode(p).ok())
                        .map(|e| e.model_version);
                }
            })?;
            Ok(version)
        })
        .await??;
        if found.is_some() {
            return Ok(found);
        }
        tracing::info!("waiting for the first input on {}", cfg.input_topic);
        tokio::select! {
            _ = shutdown.cancelled() => return Ok(None),
            _ = tokio::time::sleep(Duration::from_secs(2)) => {}
        }
    }
}

/// Re-processes `[from, to)` without emitting anything.
async fn replay(
    cfg: &LiveConfig,
    mut engine: Engine,
    from: i64,
    to: i64,
) -> Result<(Engine, usize)> {
    if from >= to {
        return Ok((engine, 0));
    }
    tracing::info!(from, to, "replaying uncheckpointed inputs");
    let (brokers, topic) = (cfg.brokers.clone(), cfg.input_topic.clone());
    tokio::task::spawn_blocking(move || {
        let count = kafka::scan_range(&brokers, &topic, PARTITION, from, to, |m| {
            if let Some(Ok(input)) = m.payload().map(EmbeddedArticle::decode) {
                // Outputs for these inputs were committed before the crash.
                let _ = engine.process(&input, m.offset());
            }
        })
        .context("replaying input")?;
        anyhow::Ok((engine, count))
    })
    .await?
}

/// Waits for one message, then gathers more until `max_batch` or `linger`.
async fn collect(consumer: &StreamConsumer, cfg: &LiveConfig) -> Result<Vec<OwnedMessage>> {
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

/// Processes one batch inside a transaction. Returns the next input offset.
fn process_epoch(
    cfg: &LiveConfig,
    consumer: &StreamConsumer,
    producer: &FutureProducer,
    engine: &mut Engine,
    batch: &[OwnedMessage],
) -> Result<i64> {
    block_in_place(|| producer.begin_transaction())?;
    let mut next_offset = 0;
    for msg in batch {
        next_offset = msg.offset() + 1;
        let Some(Ok(input)) = msg.payload().map(EmbeddedArticle::decode) else {
            counter!("pulse_processor_inputs_total", "outcome" => "undecodable").increment(1);
            tracing::warn!(offset = msg.offset(), "undecodable input skipped");
            continue;
        };
        let processed = engine.process(&input, msg.offset());
        counter!("pulse_processor_inputs_total", "outcome" => outcome_label(processed.outcome))
            .increment(1);
        if processed.late {
            counter!("pulse_processor_late_total").increment(1);
        }

        for event in &processed.events {
            let payload = event.encode_to_vec();
            let record = FutureRecord::to(&cfg.output_topic)
                .key(story_id(event))
                .payload(&payload)
                .headers(OwnedHeaders::new().insert(Header {
                    key: "content-type",
                    value: Some(EVENT_CONTENT_TYPE),
                }));
            // Delivery is confirmed by commit_transaction.
            drop(producer.send_result(record).map_err(|(e, _)| e)?);
            counter!("pulse_processor_events_total", "kind" => event_kind(event)).increment(1);
        }
        if processed.outcome == Outcome::LateDropped {
            let record = FutureRecord::to(&cfg.late_topic)
                .key(input.article.as_ref().map_or("", |a| a.id.as_str()))
                .payload(msg.payload().unwrap_or_default());
            drop(producer.send_result(record).map_err(|(e, _)| e)?);
        }
    }

    let mut offsets = TopicPartitionList::new();
    offsets.add_partition_offset(&cfg.input_topic, PARTITION, Offset::Offset(next_offset))?;
    let group = consumer
        .group_metadata()
        .context("consumer has no group metadata")?;
    let commit_started = Instant::now();
    block_in_place(|| {
        producer.send_offsets_to_transaction(&offsets, &group, TXN_TIMEOUT)?;
        producer.commit_transaction(TXN_TIMEOUT)
    })
    .context("committing transaction")?;
    histogram!("pulse_processor_commit_seconds").record(commit_started.elapsed().as_secs_f64());
    Ok(next_offset)
}

fn take_snapshot(store: &LocalStore, engine: &Engine, next_offset: i64, keep: usize) -> Result<()> {
    let started = Instant::now();
    let bytes = snapshot::encode(engine, next_offset)?;
    store.put(next_offset, &bytes)?;
    store.prune(keep, None)?;
    let seconds = started.elapsed().as_secs_f64();
    histogram!("pulse_processor_snapshot_seconds").record(seconds);
    gauge!("pulse_processor_snapshot_bytes").set(bytes.len() as f64);
    gauge!("pulse_processor_snapshot_offset").set(next_offset as f64);
    tracing::info!(
        next_offset,
        bytes = bytes.len(),
        ms = (seconds * 1000.0) as u64,
        "snapshot written"
    );
    Ok(())
}

fn report_state(engine: &Engine, next_offset: i64) {
    gauge!("pulse_processor_input_offset").set(next_offset as f64);
    gauge!("pulse_processor_open_stories").set(engine.stories().len() as f64);
    gauge!("pulse_processor_articles").set(engine.articles().len() as f64);
    gauge!("pulse_processor_index_size").set(engine.index_len() as f64);
    gauge!("pulse_processor_index_tombstones").set(engine.tombstones() as f64);
    let watermark = engine.watermark_ms();
    if watermark > i64::MIN {
        gauge!("pulse_processor_watermark_seconds").set(watermark as f64 / 1000.0);
        let lag = chrono::Utc::now().timestamp_millis() - watermark;
        gauge!("pulse_processor_watermark_lag_seconds").set(lag as f64 / 1000.0);
    }
}

pub fn outcome_label(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Invalid => "invalid",
        Outcome::ExactDuplicate => "exact_duplicate",
        Outcome::NearDuplicate => "near_duplicate",
        Outcome::Joined => "joined",
        Outcome::Created => "created",
        Outcome::LateDropped => "late_dropped",
    }
}

fn event_kind(event: &StoryEvent) -> &'static str {
    match &event.kind {
        Some(Kind::Created(_)) => "created",
        Some(Kind::ArticleAdded(_)) => "article_added",
        Some(Kind::Updated(_)) => "updated",
        Some(Kind::Split(_)) => "split",
        Some(Kind::Merged(_)) => "merged",
        Some(Kind::Closed(_)) => "closed",
        None => "none",
    }
}

/// Kafka key for an event: the story it concerns.
pub fn story_id(event: &StoryEvent) -> &str {
    match &event.kind {
        Some(Kind::Created(c)) => &c.story_id,
        Some(Kind::ArticleAdded(a)) => &a.story_id,
        Some(Kind::Updated(u)) => &u.story_id,
        Some(Kind::Split(s)) => &s.parent_story_id,
        Some(Kind::Merged(m)) => &m.target_story_id,
        Some(Kind::Closed(c)) => &c.story_id,
        None => "",
    }
}

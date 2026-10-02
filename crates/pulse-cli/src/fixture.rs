//! `pulse fixture record|stats`: capture a topic into a golden fixture file and
//! summarize fixture contents.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use prost::Message as _;
use pulse_core::fixture::{self, FixtureRecord, FixtureWriter};
use pulse_core::proto::v1::{Article, EmbeddedArticle};
use pulse_core::{config::Settings, kafka, topics};
use rdkafka::Message as _;

pub async fn record(settings: &Settings, topic: &str, since: Duration, out: &Path) -> Result<()> {
    match topic {
        topics::ARTICLES_RAW => record_raw(settings, since, out).await,
        topics::ARTICLES_EMBEDDED => record_embedded(settings, since, out).await,
        other => bail!("cannot record {other}: expected articles.raw or articles.embedded"),
    }
}

/// Reads and decodes every record in `topic` since `since`, in partition order.
async fn scan<M: FixtureRecord + Send + 'static>(
    settings: &Settings,
    topic: &'static str,
    since: Duration,
) -> Result<(Vec<M>, usize)> {
    let since_ms = Utc::now().timestamp_millis() - since.as_millis() as i64;
    let brokers = settings.kafka_brokers.clone();
    tokio::task::spawn_blocking(move || {
        let mut records = Vec::new();
        let mut undecodable = 0usize;
        kafka::scan_since(&brokers, topic, since_ms, |msg| {
            match msg.payload().map(M::decode) {
                Some(Ok(r)) => records.push(r),
                _ => undecodable += 1,
            }
        })
        .with_context(|| format!("reading {topic}"))?;
        anyhow::Ok((records, undecodable))
    })
    .await?
}

async fn record_raw(settings: &Settings, since: Duration, out: &Path) -> Result<()> {
    let (articles, undecodable) = scan::<Article>(settings, topics::ARTICLES_RAW, since).await?;
    // articles.raw has several partitions; impose one total order for replay.
    let total = articles.len();
    let mut seen = HashSet::new();
    let mut articles: Vec<Article> = articles
        .into_iter()
        .filter(|a| seen.insert(a.id.clone()))
        .collect();
    articles.sort_by(|a, b| (a.fetched_at_ms, &a.id).cmp(&(b.fetched_at_ms, &b.id)));
    let written = write(out, &articles)?;
    println!(
        "wrote {written} articles to {} (skipped {undecodable} undecodable, {} duplicate ids)",
        out.display(),
        total - written
    );
    Ok(())
}

async fn record_embedded(settings: &Settings, since: Duration, out: &Path) -> Result<()> {
    // Single partition: log order is already the processing order. Keep it.
    let (records, undecodable) =
        scan::<EmbeddedArticle>(settings, topics::ARTICLES_EMBEDDED, since).await?;
    let written = write(out, &records)?;
    println!(
        "wrote {written} embedded articles to {} in log order (skipped {undecodable} undecodable)",
        out.display()
    );
    Ok(())
}

fn write<M: FixtureRecord>(out: &Path, records: &[M]) -> Result<usize> {
    let mut writer = FixtureWriter::<M>::create(out)?;
    for r in records {
        writer.write(r)?;
    }
    Ok(writer.finish()?)
}

pub fn stats(path: &Path) -> Result<()> {
    let magic = fixture::magic(path).with_context(|| format!("reading {}", path.display()))?;
    let (articles, embedded): (Vec<Article>, Option<Vec<EmbeddedArticle>>) =
        if &magic == EmbeddedArticle::MAGIC {
            let records: Vec<EmbeddedArticle> = fixture::read_all(path)?;
            let articles = records.iter().filter_map(|r| r.article.clone()).collect();
            (articles, Some(records))
        } else {
            (fixture::read_all(path)?, None)
        };
    if articles.is_empty() {
        println!("{}: empty", path.display());
        return Ok(());
    }

    let ts = |ms: i64| {
        DateTime::<Utc>::from_timestamp_millis(ms).map_or_else(
            || ms.to_string(),
            |t| t.format("%Y-%m-%d %H:%M UTC").to_string(),
        )
    };
    let range = |f: fn(&Article) -> i64| {
        let (min, max) = articles
            .iter()
            .map(f)
            .fold((i64::MAX, i64::MIN), |(lo, hi), t| (lo.min(t), hi.max(t)));
        format!("{} → {}", ts(min), ts(max))
    };

    let mut by_lang: BTreeMap<&str, usize> = BTreeMap::new();
    let mut by_source: BTreeMap<&str, usize> = BTreeMap::new();
    for a in &articles {
        *by_lang.entry(&a.lang).or_default() += 1;
        *by_source.entry(&a.source_id).or_default() += 1;
    }
    let corrected = articles.iter().filter(|a| a.event_time_corrected).count();
    let no_summary = articles.iter().filter(|a| a.summary.is_empty()).count();
    // Out-of-order arrivals: event time earlier than the latest event time seen so far.
    let mut max_event = i64::MIN;
    let out_of_order = articles
        .iter()
        .filter(|a| {
            let late = a.published_at_ms < max_event;
            max_event = max_event.max(a.published_at_ms);
            late
        })
        .count();

    let top = |m: &BTreeMap<&str, usize>, n: usize| {
        let mut v: Vec<_> = m.iter().collect();
        v.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        v.into_iter()
            .take(n)
            .map(|(k, c)| format!("{k}:{c}"))
            .collect::<Vec<_>>()
            .join("  ")
    };
    let pct = |n: usize| 100.0 * n as f64 / articles.len() as f64;

    println!("file           {}", path.display());
    println!("articles       {}", articles.len());
    if let Some(records) = &embedded {
        let mut models: BTreeMap<&str, usize> = BTreeMap::new();
        for r in records {
            *models.entry(&r.model_version).or_default() += 1;
        }
        let dims: HashSet<usize> = records.iter().map(|r| r.vector.len()).collect();
        println!("models         {}", top(&models, 3));
        println!("dimensions     {dims:?}");
    }
    println!("fetched        {}", range(|a| a.fetched_at_ms));
    println!("event time     {}", range(|a| a.published_at_ms));
    println!("languages      {} — {}", by_lang.len(), top(&by_lang, 12));
    println!(
        "sources        {} — top: {}",
        by_source.len(),
        top(&by_source, 8)
    );
    println!("out of order   {out_of_order} ({:.1}%)", pct(out_of_order));
    println!("time corrected {corrected} ({:.1}%)", pct(corrected));
    println!("no summary     {no_summary} ({:.1}%)", pct(no_summary));
    Ok(())
}

/// Publishes a fixture into a topic in file order (idempotent producer, keyed
/// by article id). Lets any recorded or synthetic stream drive the pipeline.
pub async fn publish(settings: &Settings, path: &Path, topic: &str) -> Result<()> {
    use rdkafka::producer::{FutureProducer, FutureRecord, Producer};
    use rdkafka::util::Timeout;

    let magic = fixture::magic(path).with_context(|| format!("reading {}", path.display()))?;
    let records: Vec<(String, Vec<u8>)> = if &magic == EmbeddedArticle::MAGIC {
        fixture::read_all::<EmbeddedArticle>(path)?
            .into_iter()
            .map(|r| {
                let id = r.article.as_ref().map(|a| a.id.clone()).unwrap_or_default();
                (id, r.encode_to_vec())
            })
            .collect()
    } else {
        fixture::read_all::<Article>(path)?
            .into_iter()
            .map(|a| (a.id.clone(), a.encode_to_vec()))
            .collect()
    };

    let producer: FutureProducer =
        kafka::idempotent_producer(&settings.kafka_brokers, "pulse-fixture-publish").create()?;
    for (key, payload) in &records {
        // Idempotence keeps per-partition order even with retries.
        let mut record = FutureRecord::to(topic).key(key).payload(payload);
        loop {
            match producer.send_result(record) {
                Ok(delivery) => {
                    drop(delivery);
                    break;
                }
                Err((
                    rdkafka::error::KafkaError::MessageProduction(
                        rdkafka::types::RDKafkaErrorCode::QueueFull,
                    ),
                    r,
                )) => {
                    record = r;
                    producer.poll(Duration::from_millis(50));
                }
                Err((e, _)) => return Err(e.into()),
            }
        }
    }
    producer.flush(Timeout::After(Duration::from_secs(60)))?;
    println!(
        "published {} records from {} to {topic}",
        records.len(),
        path.display()
    );
    Ok(())
}

/// A deterministic synthetic embedded stream for tests and CI (real fixtures
/// contain publishers' text and stay out of the repo). Covers drifting topics,
/// exact and near duplicates, several sources and languages, and late arrivals
/// beyond the processor's allowed lateness.
pub fn synth(out: &Path, count: usize, seed: u64) -> Result<()> {
    const DIM: usize = 64;
    const TOPICS: usize = 48;
    const WORDS: &[&str] = &[
        "minister", "storm", "election", "market", "court", "strike", "summit", "flood", "budget",
        "vote", "protest", "rescue", "trade", "border", "energy", "health", "school", "bank",
        "rocket", "festival", "verdict", "airport", "talks", "fire", "harbor", "museum", "tariff",
        "launch", "outage", "treaty", "drought", "merger",
    ];
    let hour = 3_600_000i64;
    let start = 1_767_225_600_000i64; // 2026-01-01T00:00Z
    let mut s = seed | 1;
    let mut rand = move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };

    let mut writer = FixtureWriter::<EmbeddedArticle>::create(out)?;
    let mut last_title = String::new();
    for i in 0..count {
        // ~6 min per article; 1 in 10 is 1–60h late.
        let late_by = if rand() % 10 == 0 {
            (1 + rand() % 60) as i64 * hour
        } else {
            0
        };
        // Topics drift every ~100 articles; late arrivals pick any topic.
        let topic = if late_by > 0 {
            (rand() % TOPICS as u64) as usize
        } else {
            (i / 100 + (rand() % 3) as usize) % TOPICS
        };
        let mut v: Vec<f32> = (0..DIM)
            .map(|d| {
                let noise = (rand() % 1000) as f32 / 4000.0;
                if d == topic { 1.0 + noise } else { noise }
            })
            .collect();
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.iter_mut().for_each(|x| *x /= norm);

        let roll = rand() % 20;
        let (id, title) = if i > 0 && roll == 0 {
            (format!("syn{}", i - 1), last_title.clone()) // exact duplicate (re-poll)
        } else if i > 0 && roll == 1 {
            (format!("syn{i}"), format!("{last_title} (updated)")) // near duplicate
        } else {
            let words: Vec<&str> = (0..8)
                .map(|_| WORDS[(rand() % WORDS.len() as u64) as usize])
                .collect();
            (
                format!("syn{i}"),
                format!("{} topic{topic}", words.join(" ")),
            )
        };
        last_title.clone_from(&title);
        writer.write(&EmbeddedArticle {
            article: Some(Article {
                id,
                source_id: format!("synth-{}", rand() % 12),
                url: format!("https://synthetic.example/{i}"),
                title,
                lang: ["en", "es", "fr", "de"][(rand() % 4) as usize].into(),
                published_at_ms: start + i as i64 * hour / 10 - late_by,
                fetched_at_ms: start + i as i64 * hour / 10,
                ..Default::default()
            }),
            vector: v,
            model_version: "synthetic".into(),
        })?;
    }
    let written = writer.finish()?;
    println!(
        "wrote {written} synthetic embedded articles to {}",
        out.display()
    );
    Ok(())
}

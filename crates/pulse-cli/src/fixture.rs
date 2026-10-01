//! `pulse fixture record|stats`: capture `articles.raw` into a golden fixture
//! file and summarize fixture contents.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use prost::Message as _;
use pulse_core::fixture::{self, FixtureWriter};
use pulse_core::proto::v1::Article;
use pulse_core::{config::Settings, kafka, topics};
use rdkafka::Message as _;

pub async fn record(settings: &Settings, since: Duration, out: &Path) -> Result<()> {
    let since_ms = Utc::now().timestamp_millis() - since.as_millis() as i64;
    let brokers = settings.kafka_brokers.clone();

    let (articles, undecodable) = tokio::task::spawn_blocking(move || {
        let mut articles = Vec::new();
        let mut undecodable = 0usize;
        kafka::scan_since(&brokers, topics::ARTICLES_RAW, since_ms, |msg| {
            match msg.payload().map(Article::decode) {
                Some(Ok(a)) => articles.push(a),
                _ => undecodable += 1,
            }
        })
        .context("reading articles.raw")?;
        anyhow::Ok((articles, undecodable))
    })
    .await??;

    // articles.raw has several partitions; impose one total order for replay.
    let total = articles.len();
    let mut seen = HashSet::new();
    let mut articles: Vec<Article> = articles
        .into_iter()
        .filter(|a| seen.insert(a.id.clone()))
        .collect();
    articles.sort_by(|a, b| (a.fetched_at_ms, &a.id).cmp(&(b.fetched_at_ms, &b.id)));

    let mut writer = FixtureWriter::create(out)?;
    for a in &articles {
        writer.write(a)?;
    }
    let written = writer.finish()?;
    println!(
        "wrote {written} articles to {} (skipped {undecodable} undecodable, {} duplicate ids)",
        out.display(),
        total - written
    );
    Ok(())
}

pub fn stats(path: &Path) -> Result<()> {
    let articles =
        fixture::read_all(path).with_context(|| format!("reading {}", path.display()))?;
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

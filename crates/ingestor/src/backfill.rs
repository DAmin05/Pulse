//! `ingestor backfill-gdelt`: download a past time range of GDELT GKG slots
//! into a fixture file for load tests and replay experiments.
//!
//! Output is deterministic for a given range and filter: each article's
//! `fetched_at_ms` is its slot's end time, and records are ordered by
//! `(fetched_at_ms, id)`.

use std::collections::HashSet;
use std::path::Path;

use anyhow::Result;
use chrono::{DateTime, TimeDelta, Utc};
use pulse_core::fixture::FixtureWriter;

use crate::gdelt::{self, Filter, SlotFetch};
use crate::normalize::Normalizer;
use crate::runner::http_client;
use crate::source::PollError;
use crate::sources::{Defaults, SourceConfig};

const ATTEMPTS: u32 = 3;

pub async fn run(
    cfg: &SourceConfig,
    defaults: &Defaults,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    out: &Path,
) -> Result<()> {
    anyhow::ensure!(from < to, "--from must be before --to");
    anyhow::ensure!(
        cfg.sample > 0.0 && cfg.sample <= 1.0,
        "--sample must be in (0, 1]"
    );
    let stream = cfg.stream.expect("set by caller");
    let http = http_client(defaults)?;
    let filter = Filter::new(cfg);
    let normalizer = Normalizer {
        max_age: None,
        max_summary_chars: defaults.max_summary_chars,
    };

    let mut seen = HashSet::new();
    let mut articles = Vec::new();
    let (mut missing, mut rejected) = (0, 0);
    let slots: Vec<_> = gdelt::slots(from, to).collect();

    for (i, &slot) in slots.iter().enumerate() {
        let fetched = fetch_with_retry(&http, stream, slot).await?;
        let SlotFetch::Ready(items) = fetched else {
            missing += 1;
            tracing::warn!(%slot, "slot not available; skipping");
            continue;
        };
        let fetched_at = (slot + TimeDelta::minutes(15)).timestamp_millis();
        let before = articles.len();
        for c in items.into_iter().filter(|c| filter.keep(c)) {
            match normalizer.normalize(cfg, c, fetched_at) {
                Ok(n) if seen.insert(n.article.id.clone()) => articles.push(n.article),
                Ok(_) => {}
                Err(_) => rejected += 1,
            }
        }
        tracing::info!(
            "[{}/{}] {slot}: +{} articles",
            i + 1,
            slots.len(),
            articles.len() - before
        );
        tokio::time::sleep(gdelt::BACKFILL_PAUSE).await;
    }

    articles.sort_by(|a, b| (a.fetched_at_ms, &a.id).cmp(&(b.fetched_at_ms, &b.id)));
    let mut writer = FixtureWriter::create(out)?;
    for a in &articles {
        writer.write(a)?;
    }
    let written = writer.finish()?;
    tracing::info!(
        written,
        slots = slots.len(),
        missing,
        rejected,
        out = %out.display(),
        "backfill complete"
    );
    Ok(())
}

async fn fetch_with_retry(
    http: &reqwest::Client,
    stream: crate::sources::GdeltStream,
    slot: DateTime<Utc>,
) -> Result<SlotFetch> {
    let mut attempt = 0;
    loop {
        attempt += 1;
        match gdelt::fetch_slot(http, stream, slot).await {
            Ok(f) => return Ok(f),
            Err(e @ (PollError::Network(_) | PollError::Http { .. })) if attempt < ATTEMPTS => {
                let wait = e
                    .retry_after()
                    .unwrap_or(std::time::Duration::from_secs(2 << attempt));
                tracing::warn!(%slot, error = %e, ?wait, "retrying");
                tokio::time::sleep(wait).await;
            }
            Err(e) => return Err(e.into()),
        }
    }
}

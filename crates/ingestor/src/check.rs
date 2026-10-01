//! `ingestor check`: poll each source once and report, without publishing.

use std::collections::BTreeMap;

use anyhow::Result;
use tokio::task::JoinSet;

use crate::normalize::{Normalizer, Rejection};
use crate::runner::{Limits, http_client, now_ms};
use crate::source::AnySource;
use crate::sources::{SourceConfig, SourcesFile};

struct Report {
    id: String,
    error: Option<String>,
    items: usize,
    valid: usize,
    too_old: usize,
    invalid: usize,
    corrected: usize,
    langs: BTreeMap<String, usize>,
    newest_age_min: Option<i64>,
}

/// Returns true when every checked source succeeded and produced valid items.
pub async fn run(file: &SourcesFile, only: &[String], include_disabled: bool) -> Result<bool> {
    let selected: Vec<SourceConfig> = file
        .sources
        .iter()
        .filter(|s| include_disabled || s.enabled)
        .filter(|s| only.is_empty() || only.contains(&s.id))
        .cloned()
        .collect();
    anyhow::ensure!(!selected.is_empty(), "no sources selected");

    let defaults = file.defaults.clone();
    let http = http_client(&defaults)?;
    let limits = Limits::new(&selected, &defaults);
    let mut tasks = JoinSet::new();
    for cfg in selected {
        let (http, limits, defaults) = (http.clone(), limits.clone(), defaults.clone());
        tasks.spawn(async move {
            let mut source = AnySource::new(&cfg, &defaults);
            let result = {
                let _permits = limits.acquire(&cfg.host()).await;
                source.poll(&http).await
            };
            let normalizer = Normalizer {
                max_age: Some(defaults.max_age),
                max_summary_chars: defaults.max_summary_chars,
            };
            let mut r = Report {
                id: cfg.id.clone(),
                error: None,
                items: 0,
                valid: 0,
                too_old: 0,
                invalid: 0,
                corrected: 0,
                langs: BTreeMap::new(),
                newest_age_min: None,
            };
            match result {
                Err(e) => r.error = Some(e.to_string()),
                Ok(poll) => {
                    let now = now_ms();
                    r.items = poll.items.len();
                    for c in poll.items {
                        match normalizer.normalize(&cfg, c, now) {
                            Ok(n) => {
                                r.valid += 1;
                                r.corrected += usize::from(n.correction.is_some());
                                *r.langs.entry(n.article.lang).or_default() += 1;
                                let age = (now - n.article.published_at_ms) / 60_000;
                                r.newest_age_min =
                                    Some(r.newest_age_min.map_or(age, |a: i64| a.min(age)));
                            }
                            Err(Rejection::TooOld) => r.too_old += 1,
                            Err(_) => r.invalid += 1,
                        }
                    }
                }
            }
            r
        });
    }

    let mut reports = Vec::new();
    while let Some(r) = tasks.join_next().await {
        reports.push(r?);
    }
    reports.sort_by(|a, b| a.id.cmp(&b.id));

    println!(
        "{:<6} {:<28} {:>5} {:>5} {:>4} {:>4} {:>4}  {:<16} {:>8}  ERROR",
        "STATUS", "SOURCE", "ITEMS", "VALID", "OLD", "BAD", "FIX", "LANGS", "NEWEST"
    );
    let mut failed = 0;
    for r in &reports {
        let ok = r.error.is_none() && r.valid > 0;
        failed += usize::from(!ok);
        let mut langs: Vec<_> = r.langs.iter().collect();
        langs.sort_by(|a, b| b.1.cmp(a.1));
        let langs = langs
            .iter()
            .take(3)
            .map(|(l, n)| format!("{l}:{n}"))
            .collect::<Vec<_>>()
            .join(",");
        let newest = r.newest_age_min.map_or("-".into(), format_age);
        let status = match (&r.error, r.valid) {
            (Some(_), _) => "FAIL",
            (None, 0) => "EMPTY",
            _ => "ok",
        };
        println!(
            "{status:<6} {:<28} {:>5} {:>5} {:>4} {:>4} {:>4}  {langs:<16} {newest:>8}  {}",
            r.id,
            r.items,
            r.valid,
            r.too_old,
            r.invalid,
            r.corrected,
            r.error.as_deref().unwrap_or("")
        );
    }
    let total_valid: usize = reports.iter().map(|r| r.valid).sum();
    println!(
        "\n{} sources, {} ok, {failed} failing/empty, {total_valid} valid articles",
        reports.len(),
        reports.len() - failed
    );
    Ok(failed == 0)
}

fn format_age(minutes: i64) -> String {
    match minutes {
        m if m < 60 => format!("{m}m"),
        m if m < 48 * 60 => format!("{}h", m / 60),
        m => format!("{}d", m / (24 * 60)),
    }
}

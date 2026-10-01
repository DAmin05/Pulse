//! GDELT 2.0 Global Knowledge Graph (GKG) 15-minute files.
//!
//! Each slot `YYYYMMDDHHMMSS` has a zipped TSV at
//! `https://data.gdeltproject.org/gdeltv2/{slot}[.translation].gkg.csv.zip`.
//! We use: column 1 (slot date), 4 (document URL), 25 (translation info,
//! `srclc:xxx`) and 26 (extras XML containing `<PAGE_TITLE>`). GKG has no
//! summaries, so GDELT articles are headline-only.

use std::io::{Cursor, Read};
use std::time::Duration;

use chrono::{DateTime, NaiveDateTime, TimeDelta, Timelike, Utc};
use reqwest::StatusCode;
use sha2::{Digest, Sha256};

use crate::normalize::Candidate;
use crate::source::{PollError, PollResult, check_status, read_body};
use crate::sources::{Defaults, GdeltStream, SourceConfig};

pub const HOST: &str = "data.gdeltproject.org";
const SLOT: TimeDelta = TimeDelta::minutes(15);
/// Missing slots older than this are skipped instead of retried.
const GIVE_UP_AFTER: TimeDelta = TimeDelta::hours(3);
/// Upper bound on slots fetched in one poll when catching up.
const MAX_SLOTS_PER_POLL: usize = 4;

pub fn floor_slot(t: DateTime<Utc>) -> DateTime<Utc> {
    let minute = t.minute() - t.minute() % 15;
    t.with_minute(minute)
        .and_then(|t| t.with_second(0))
        .and_then(|t| t.with_nanosecond(0))
        .expect("valid time")
}

pub fn slot_url(stream: GdeltStream, slot: DateTime<Utc>) -> String {
    let infix = match stream {
        GdeltStream::English => "",
        GdeltStream::Translingual => ".translation",
    };
    format!(
        "https://{HOST}/gdeltv2/{}{infix}.gkg.csv.zip",
        slot.format("%Y%m%d%H%M%S")
    )
}

pub enum SlotFetch {
    Ready(Vec<Candidate>),
    /// 404: not published yet (or never will be).
    Missing,
}

pub async fn fetch_slot(
    http: &reqwest::Client,
    stream: GdeltStream,
    slot: DateTime<Utc>,
) -> Result<SlotFetch, PollError> {
    let resp = http.get(slot_url(stream, slot)).send().await?;
    if resp.status() == StatusCode::NOT_FOUND {
        return Ok(SlotFetch::Missing);
    }
    check_status(&resp)?;
    let zipped = read_body(resp).await?;
    let csv = unzip_single(&zipped)?;
    Ok(SlotFetch::Ready(parse_gkg(&csv, stream)))
}

fn unzip_single(zipped: &[u8]) -> Result<Vec<u8>, PollError> {
    let mut archive =
        zip::ZipArchive::new(Cursor::new(zipped)).map_err(|e| PollError::Parse(e.to_string()))?;
    let mut file = archive
        .by_index(0)
        .map_err(|e| PollError::Parse(e.to_string()))?;
    let mut out = Vec::with_capacity(file.size() as usize);
    // Reading to the end verifies the CRC.
    file.read_to_end(&mut out)
        .map_err(|e| PollError::Parse(e.to_string()))?;
    Ok(out)
}

pub fn parse_gkg(csv: &[u8], stream: GdeltStream) -> Vec<Candidate> {
    String::from_utf8_lossy(csv)
        .lines()
        .filter_map(|line| {
            let cols: Vec<&str> = line.split('\t').collect();
            if cols.len() < 27 {
                return None;
            }
            let url = cols[4];
            if !url.starts_with("http") {
                return None;
            }
            let title = between(cols[26], "<PAGE_TITLE>", "</PAGE_TITLE>")?;
            let published_at_ms = NaiveDateTime::parse_from_str(cols[1], "%Y%m%d%H%M%S")
                .ok()
                .map(|t| t.and_utc().timestamp_millis());
            let lang_hint = match stream {
                GdeltStream::English => Some("en"),
                GdeltStream::Translingual => {
                    between(cols[25], "srclc:", ";").or_else(|| cols[25].strip_prefix("srclc:"))
                }
            }
            .map(str::to_owned);
            Some(Candidate {
                url: url.to_owned(),
                title: title.to_owned(),
                summary: String::new(),
                published_at_ms,
                lang_hint,
            })
        })
        .collect()
}

fn between<'a>(s: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let from = s.find(start)? + start.len();
    let to = s[from..].find(end)? + from;
    Some(&s[from..to])
}

/// Deterministic URL-hash sampling plus an optional domain allowlist.
pub struct Filter {
    sample: f64,
    domains: Vec<String>,
}

impl Filter {
    pub fn new(cfg: &SourceConfig) -> Self {
        Self {
            sample: cfg.sample,
            domains: cfg.domains.iter().map(|d| d.to_ascii_lowercase()).collect(),
        }
    }

    pub fn keep(&self, c: &Candidate) -> bool {
        if !self.domains.is_empty() {
            let Some(host) = url::Url::parse(&c.url)
                .ok()
                .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
            else {
                return false;
            };
            let allowed = self
                .domains
                .iter()
                .any(|d| host == *d || host.ends_with(&format!(".{d}")));
            if !allowed {
                return false;
            }
        }
        if self.sample >= 1.0 {
            return true;
        }
        let digest = Sha256::digest(c.url.as_bytes());
        let bucket = u64::from_be_bytes(digest[..8].try_into().expect("8 bytes"));
        (bucket as f64 / u64::MAX as f64) < self.sample
    }
}

/// Live GDELT source: walks slots in order, waiting on slots that are not
/// published yet and skipping ones that never appear.
pub struct GdeltSource {
    stream: GdeltStream,
    filter: Filter,
    next_slot: DateTime<Utc>,
}

impl GdeltSource {
    pub fn live(cfg: &SourceConfig, defaults: &Defaults) -> Self {
        // Start an hour back so the translingual stream (which lags) has data,
        // but never further back than max_age.
        let lookback = TimeDelta::hours(1)
            .min(TimeDelta::from_std(defaults.max_age).unwrap_or(TimeDelta::hours(1)));
        Self {
            stream: cfg.stream.expect("validated: gdelt sources have a stream"),
            filter: Filter::new(cfg),
            next_slot: floor_slot(Utc::now() - lookback),
        }
    }

    pub async fn poll(&mut self, http: &reqwest::Client) -> Result<PollResult, PollError> {
        let mut result = PollResult::default();
        let now = Utc::now();
        for _ in 0..MAX_SLOTS_PER_POLL {
            if self.next_slot > now {
                break;
            }
            match fetch_slot(http, self.stream, self.next_slot).await? {
                SlotFetch::Ready(items) => {
                    result
                        .items
                        .extend(items.into_iter().filter(|c| self.filter.keep(c)));
                }
                SlotFetch::Missing if now - self.next_slot > GIVE_UP_AFTER => {
                    tracing::warn!(slot = %self.next_slot, stream = ?self.stream, "gdelt slot never published; skipping");
                    metrics::counter!("pulse_ingestor_gdelt_slots_skipped_total").increment(1);
                }
                SlotFetch::Missing => break, // not yet published; retry next poll
            }
            self.next_slot += SLOT;
        }
        result.not_modified = result.items.is_empty();
        Ok(result)
    }
}

/// Slots in `[from, to)`.
pub fn slots(from: DateTime<Utc>, to: DateTime<Utc>) -> impl Iterator<Item = DateTime<Utc>> {
    let mut next = floor_slot(from);
    std::iter::from_fn(move || {
        (next < to).then(|| {
            let slot = next;
            next += SLOT;
            slot
        })
    })
}

/// Polite pause between sequential slot downloads during backfill.
pub const BACKFILL_PAUSE: Duration = Duration::from_millis(500);

#[cfg(test)]
mod tests {
    use super::*;

    fn row(date: &str, url: &str, translation: &str, extras: &str) -> String {
        let mut cols = vec![""; 27];
        cols[1] = date;
        cols[4] = url;
        cols[25] = translation;
        cols[26] = extras;
        cols.join("\t")
    }

    #[test]
    fn parses_gkg_rows() {
        let csv = [
            row(
                "20261001210000",
                "https://em.com.br/x",
                "srclc:por;eng:GT-POR 1.0",
                "<PAGE_TITLE>Vota&#xE7;&#xE3;o</PAGE_TITLE><PAGE_AUTHORS>A</PAGE_AUTHORS>",
            ),
            row(
                "20261001210000",
                "https://no-title.com/x",
                "",
                "<PAGE_LINKS>x</PAGE_LINKS>",
            ),
            "short\trow".to_owned(),
        ]
        .join("\n");
        let items = parse_gkg(csv.as_bytes(), GdeltStream::Translingual);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "Vota&#xE7;&#xE3;o"); // decoded later by normalize
        assert_eq!(items[0].lang_hint.as_deref(), Some("por"));
        assert_eq!(items[0].published_at_ms, Some(1_790_888_400_000));
    }

    #[test]
    fn slot_math() {
        let t = DateTime::parse_from_rfc3339("2026-10-01T21:37:12Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(floor_slot(t).to_rfc3339(), "2026-10-01T21:30:00+00:00");
        assert_eq!(
            slot_url(GdeltStream::Translingual, floor_slot(t)),
            "https://data.gdeltproject.org/gdeltv2/20261001213000.translation.gkg.csv.zip"
        );
        let to = t + TimeDelta::hours(1);
        assert_eq!(slots(t, to).count(), 5); // 21:30 .. 22:30 inclusive of start
    }

    #[test]
    fn filter_is_deterministic_and_respects_domains() {
        let cfg: SourceConfig = toml::from_str(
            "id = \"g\"\nkind = \"gdelt\"\nstream = \"english\"\nsample = 0.5\ndomains = [\"bbc.co.uk\"]",
        )
        .unwrap();
        let f = Filter::new(&cfg);
        let other = Candidate {
            url: "https://example.com/a".into(),
            ..Default::default()
        };
        assert!(!f.keep(&other));

        let kept = (0..1000)
            .filter(|i| {
                f.keep(&Candidate {
                    url: format!("https://www.bbc.co.uk/news/{i}"),
                    ..Default::default()
                })
            })
            .count();
        assert!((400..600).contains(&kept), "kept {kept}");
        let again = (0..1000)
            .filter(|i| {
                f.keep(&Candidate {
                    url: format!("https://www.bbc.co.uk/news/{i}"),
                    ..Default::default()
                })
            })
            .count();
        assert_eq!(kept, again);
    }
}

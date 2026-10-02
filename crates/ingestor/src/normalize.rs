//! Raw feed items → `pulse.v1.Article`, including event-time correction.

use std::time::Duration;

use pulse_core::ids;
use pulse_core::proto::v1::{Article, SourceKind};

use crate::lang;
use crate::sources::{Kind, SourceConfig};
use crate::text;

/// Publisher clocks may run slightly ahead; beyond this an event time is "future".
const FUTURE_SKEW_MS: i64 = 5 * 60 * 1000;

/// An item as extracted from a source, before validation.
#[derive(Debug, Clone, Default)]
pub struct Candidate {
    pub url: String,
    /// May contain HTML and entities.
    pub title: String,
    /// May contain HTML and entities.
    pub summary: String,
    pub published_at_ms: Option<i64>,
    /// Language declared by the feed or upstream source.
    pub lang_hint: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    InvalidUrl,
    MissingTitle,
    TooOld,
}

impl Rejection {
    pub fn label(self) -> &'static str {
        match self {
            Self::InvalidUrl => "invalid_url",
            Self::MissingTitle => "missing_title",
            Self::TooOld => "too_old",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Correction {
    /// No publisher timestamp; fetched time used.
    Missing,
    /// Publisher timestamp in the future; clamped to fetched time.
    Future,
}

impl Correction {
    pub fn label(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Future => "future",
        }
    }
}

#[derive(Debug)]
pub struct Normalized {
    pub article: Article,
    pub correction: Option<Correction>,
}

pub struct Normalizer {
    /// `None` disables the age filter (backfill).
    pub max_age: Option<Duration>,
    pub max_summary_chars: usize,
}

impl Normalizer {
    pub fn normalize(
        &self,
        source: &SourceConfig,
        c: Candidate,
        fetched_at_ms: i64,
    ) -> Result<Normalized, Rejection> {
        let url = ids::canonicalize_url(&c.url).map_err(|_| Rejection::InvalidUrl)?;
        let title = text::clean(&c.title);
        if title.is_empty() {
            return Err(Rejection::MissingTitle);
        }
        let mut summary = text::truncate(&text::clean(&c.summary), self.max_summary_chars);
        if summary == title {
            summary.clear();
        }

        let (published_at_ms, correction) = match c.published_at_ms {
            None => (fetched_at_ms, Some(Correction::Missing)),
            Some(t) if t > fetched_at_ms + FUTURE_SKEW_MS => {
                (fetched_at_ms, Some(Correction::Future))
            }
            Some(t) => (t, None),
        };
        if let Some(max_age) = self.max_age {
            if fetched_at_ms - published_at_ms > max_age.as_millis() as i64 {
                return Err(Rejection::TooOld);
            }
        }

        let lang = resolve_lang(source, c.lang_hint.as_deref(), &title, &summary);
        let source_id = publisher_id(source, &url);

        Ok(Normalized {
            article: Article {
                id: ids::article_id(&url),
                source_id,
                source_kind: source_kind(source.kind) as i32,
                url,
                title,
                summary,
                published_at_ms,
                fetched_at_ms,
                lang,
                event_time_corrected: correction.is_some(),
            },
            correction,
        })
    }
}

/// RSS feeds are one publisher each, so the source id names them. Aggregated
/// sources (GDELT) carry many publishers; attribute those to the article's
/// domain (`gdelt-english:lemonde.fr`) so per-story source counts stay meaningful.
fn publisher_id(source: &SourceConfig, canonical_url: &str) -> String {
    match source.kind {
        Kind::Rss => source.id.clone(),
        Kind::Gdelt => {
            let host = url::Url::parse(canonical_url)
                .ok()
                .and_then(|u| u.host_str().map(str::to_owned))
                .unwrap_or_default();
            format!("{}:{host}", source.id)
        }
    }
}

/// Config language wins (curated), then the feed's own declaration, then
/// detection. Sources marked `detect_lang` are mixed-language, so detection wins.
fn resolve_lang(source: &SourceConfig, hint: Option<&str>, title: &str, summary: &str) -> String {
    let configured = || source.lang.as_deref().and_then(lang::normalize);
    let hinted = || hint.and_then(lang::normalize);
    let detected = || lang::detect(&format!("{title}. {summary}"));

    let resolved = if source.detect_lang {
        detected().or_else(hinted).or_else(configured)
    } else {
        configured().or_else(hinted).or_else(detected)
    };
    resolved.unwrap_or_else(|| lang::UNDETERMINED.to_owned())
}

fn source_kind(kind: Kind) -> SourceKind {
    match kind {
        Kind::Rss => SourceKind::Rss,
        Kind::Gdelt => SourceKind::Gdelt,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_790_000_000_000;
    const HOUR: i64 = 3_600_000;

    fn source(lang: Option<&str>, detect: bool) -> SourceConfig {
        toml::from_str(&format!(
            "id = \"t\"\nurl = \"https://x.y/feed\"\ndetect_lang = {detect}\n{}",
            lang.map(|l| format!("lang = \"{l}\"")).unwrap_or_default()
        ))
        .unwrap()
    }

    fn normalizer() -> Normalizer {
        Normalizer {
            max_age: Some(Duration::from_secs(72 * 3600)),
            max_summary_chars: 600,
        }
    }

    fn candidate(published: Option<i64>) -> Candidate {
        Candidate {
            url: "https://www.example.com/a?utm_source=rss".into(),
            title: "  Big <b>news</b> &amp; more ".into(),
            summary: "<p>Details here</p>".into(),
            published_at_ms: published,
            lang_hint: Some("en-GB".into()),
        }
    }

    #[test]
    fn builds_clean_article() {
        let n = normalizer()
            .normalize(&source(None, false), candidate(Some(NOW - HOUR)), NOW)
            .unwrap();
        let a = n.article;
        assert_eq!(a.url, "https://example.com/a");
        assert_eq!(a.id, ids::article_id("https://example.com/a"));
        assert_eq!(a.title, "Big news & more");
        assert_eq!(a.summary, "Details here");
        assert_eq!(a.lang, "en");
        assert_eq!(a.published_at_ms, NOW - HOUR);
        assert!(!a.event_time_corrected);
        assert_eq!(n.correction, None);
    }

    #[test]
    fn corrects_event_time() {
        let n = normalizer();
        let missing = n
            .normalize(&source(None, false), candidate(None), NOW)
            .unwrap();
        assert_eq!(missing.article.published_at_ms, NOW);
        assert_eq!(missing.correction, Some(Correction::Missing));

        let future = n
            .normalize(&source(None, false), candidate(Some(NOW + 2 * HOUR)), NOW)
            .unwrap();
        assert_eq!(future.article.published_at_ms, NOW);
        assert!(future.article.event_time_corrected);

        let skew_ok = n
            .normalize(&source(None, false), candidate(Some(NOW + 60_000)), NOW)
            .unwrap();
        assert_eq!(skew_ok.correction, None);
    }

    #[test]
    fn rejects_bad_items() {
        let n = normalizer();
        let src = source(None, false);
        let old = n.normalize(&src, candidate(Some(NOW - 100 * HOUR)), NOW);
        assert_eq!(old.unwrap_err(), Rejection::TooOld);

        let mut no_title = candidate(Some(NOW));
        no_title.title = "<img src=x>".into();
        assert_eq!(
            n.normalize(&src, no_title, NOW).unwrap_err(),
            Rejection::MissingTitle
        );

        let mut bad_url = candidate(Some(NOW));
        bad_url.url = "not a url".into();
        assert_eq!(
            n.normalize(&src, bad_url, NOW).unwrap_err(),
            Rejection::InvalidUrl
        );
    }

    #[test]
    fn gdelt_articles_are_attributed_to_their_publisher() {
        let gdelt: SourceConfig =
            toml::from_str("id = \"gdelt-english\"\nkind = \"gdelt\"\nstream = \"english\"")
                .unwrap();
        let mut c = candidate(Some(NOW));
        c.url = "https://www.lemonde.fr/article/1".into();
        let a = normalizer().normalize(&gdelt, c, NOW).unwrap().article;
        assert_eq!(a.source_id, "gdelt-english:lemonde.fr");
        let rss = normalizer()
            .normalize(&source(None, false), candidate(Some(NOW)), NOW)
            .unwrap()
            .article;
        assert_eq!(rss.source_id, "t");
    }

    #[test]
    fn language_priority() {
        let n = normalizer();
        let mut c = candidate(Some(NOW));
        c.title = "El gobierno anunció nuevas medidas económicas contra la inflación".into();
        c.lang_hint = Some("en".into());

        // Configured language beats the feed hint.
        let a = n
            .normalize(&source(Some("es"), false), c.clone(), NOW)
            .unwrap();
        assert_eq!(a.article.lang, "es");
        // Feed hint beats detection when nothing is configured.
        let a = n.normalize(&source(None, false), c.clone(), NOW).unwrap();
        assert_eq!(a.article.lang, "en");
        // Mixed-language sources trust detection.
        let a = n.normalize(&source(Some("en"), true), c, NOW).unwrap();
        assert_eq!(a.article.lang, "es");
    }
}

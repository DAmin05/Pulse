//! `sources.toml`: what to poll and how.

use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;

/// Never poll a feed more often than this.
const MIN_RSS_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourcesFile {
    #[serde(default)]
    pub defaults: Defaults,
    #[serde(rename = "source", default)]
    pub sources: Vec<SourceConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Defaults {
    #[serde(with = "humantime_serde")]
    pub poll_interval: Duration,
    /// Items older than this (by event time) are skipped.
    #[serde(with = "humantime_serde")]
    pub max_age: Duration,
    #[serde(with = "humantime_serde")]
    pub timeout: Duration,
    pub user_agent: String,
    pub max_concurrent: usize,
    pub max_concurrent_per_host: usize,
    /// Summaries longer than this many characters are truncated.
    pub max_summary_chars: usize,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(180),
            max_age: Duration::from_secs(72 * 3600),
            timeout: Duration::from_secs(20),
            user_agent: "PulseNewsBot/0.1 (news aggregation research project)".into(),
            max_concurrent: 16,
            max_concurrent_per_host: 2,
            max_summary_chars: 600,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Rss,
    Gdelt,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum GdeltStream {
    /// English-language GKG.
    English,
    /// Machine-translated GKG covering ~65 source languages (titles stay in the
    /// original language). Lags the English stream by roughly an hour.
    Translingual,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceConfig {
    /// Stable identifier, `[a-z0-9-]+`. Recorded on every article.
    pub id: String,
    #[serde(default = "default_kind")]
    pub kind: Kind,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Feed URL (RSS/Atom/JSON Feed).
    pub url: Option<String>,
    /// Language of the source. Trusted over feed metadata and detection.
    pub lang: Option<String>,
    /// Detect language per article instead of trusting `lang` (mixed-language feeds).
    #[serde(default)]
    pub detect_lang: bool,
    /// Catalog metadata, surfaced by the Query API (phase 6).
    #[allow(dead_code)]
    pub category: Option<String>,
    #[allow(dead_code)]
    pub region: Option<String>,
    #[serde(default, with = "humantime_serde")]
    pub poll_interval: Option<Duration>,

    /// GDELT: which GKG stream.
    pub stream: Option<GdeltStream>,
    /// GDELT: fraction of articles to keep, chosen deterministically by URL.
    #[serde(default = "default_sample")]
    pub sample: f64,
    /// GDELT: only keep articles whose host is (a subdomain of) one of these.
    #[serde(default)]
    pub domains: Vec<String>,
}

fn default_kind() -> Kind {
    Kind::Rss
}
fn default_true() -> bool {
    true
}
fn default_sample() -> f64 {
    1.0
}

impl SourceConfig {
    pub fn interval(&self, defaults: &Defaults) -> Duration {
        self.poll_interval.unwrap_or(defaults.poll_interval)
    }

    /// Host used for per-host politeness limits.
    pub fn host(&self) -> String {
        match self.kind {
            Kind::Gdelt => crate::gdelt::HOST.to_owned(),
            Kind::Rss => self
                .url
                .as_deref()
                .and_then(|u| url::Url::parse(u).ok())
                .and_then(|u| u.host_str().map(str::to_owned))
                .unwrap_or_default(),
        }
    }
}

impl SourcesFile {
    pub fn load(path: &Path) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let file: Self =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        file.validate()?;
        Ok(file)
    }

    pub fn enabled(&self) -> impl Iterator<Item = &SourceConfig> {
        self.sources.iter().filter(|s| s.enabled)
    }

    fn validate(&self) -> Result<()> {
        let mut ids = HashSet::new();
        for s in &self.sources {
            let ctx = || format!("source `{}`", s.id);
            ensure!(
                !s.id.is_empty()
                    && s.id
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "{}: id must match [a-z0-9-]+",
                ctx()
            );
            ensure!(ids.insert(&s.id), "{}: duplicate id", ctx());
            if let Some(lang) = &s.lang {
                ensure!(
                    crate::lang::normalize(lang).is_some(),
                    "{}: unknown lang `{lang}`",
                    ctx()
                );
            }
            match s.kind {
                Kind::Rss => {
                    let Some(url) = &s.url else {
                        bail!("{}: rss sources need `url`", ctx());
                    };
                    let parsed = url::Url::parse(url).with_context(ctx)?;
                    ensure!(
                        matches!(parsed.scheme(), "http" | "https"),
                        "{}: url must be http(s)",
                        ctx()
                    );
                    ensure!(
                        s.interval(&self.defaults) >= MIN_RSS_INTERVAL,
                        "{}: poll_interval below {MIN_RSS_INTERVAL:?}",
                        ctx()
                    );
                }
                Kind::Gdelt => {
                    ensure!(s.stream.is_some(), "{}: gdelt sources need `stream`", ctx());
                    ensure!(
                        s.sample > 0.0 && s.sample <= 1.0,
                        "{}: sample must be in (0, 1]",
                        ctx()
                    );
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(toml_text: &str) -> Result<SourcesFile> {
        let f: SourcesFile = toml::from_str(toml_text)?;
        f.validate()?;
        Ok(f)
    }

    #[test]
    fn parses_rss_and_gdelt() {
        let f = parse(
            r#"
            [defaults]
            poll_interval = "5m"

            [[source]]
            id = "bbc-mundo"
            url = "https://feeds.bbci.co.uk/mundo/rss.xml"
            lang = "es"

            [[source]]
            id = "gdelt-en"
            kind = "gdelt"
            stream = "english"
            enabled = false
            sample = 0.1
            "#,
        )
        .unwrap();
        assert_eq!(f.sources.len(), 2);
        assert_eq!(f.enabled().count(), 1);
        assert_eq!(f.sources[0].interval(&f.defaults), Duration::from_secs(300));
        assert_eq!(f.sources[0].host(), "feeds.bbci.co.uk");
    }

    #[test]
    fn rejects_bad_configs() {
        assert!(parse("[[source]]\nid = \"Bad Id\"\nurl = \"https://x.y\"").is_err());
        assert!(parse("[[source]]\nid = \"a\"").is_err());
        assert!(
            parse("[[source]]\nid = \"a\"\nurl = \"https://x.y\"\npoll_interval = \"10s\"")
                .is_err()
        );
        assert!(
            parse("[[source]]\nid = \"a\"\nurl = \"https://x.y\"\n[[source]]\nid = \"a\"\nurl = \"https://x.z\"")
                .is_err()
        );
        assert!(parse("[[source]]\nid = \"g\"\nkind = \"gdelt\"").is_err());
    }
}

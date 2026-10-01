//! The polling interface shared by all source kinds.

use std::time::Duration;

use reqwest::{Response, StatusCode, header};

use crate::gdelt::GdeltSource;
use crate::normalize::Candidate;
use crate::rss::RssSource;
use crate::sources::{Defaults, Kind, SourceConfig};

/// Feeds larger than this are rejected rather than buffered.
pub const MAX_BODY_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug, Default)]
pub struct PollResult {
    pub items: Vec<Candidate>,
    /// The server answered 304 / nothing new was available.
    pub not_modified: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum PollError {
    #[error("HTTP {status}")]
    Http {
        status: StatusCode,
        retry_after: Option<Duration>,
    },
    #[error("network: {0}")]
    Network(#[from] reqwest::Error),
    #[error("parse: {0}")]
    Parse(String),
    #[error("body larger than {MAX_BODY_BYTES} bytes")]
    TooLarge,
}

impl PollError {
    /// Metric label for the poll outcome.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Http { .. } => "http_error",
            Self::Network(_) => "network_error",
            Self::Parse(_) | Self::TooLarge => "parse_error",
        }
    }

    /// Server-requested delay (429/503 with `Retry-After` seconds).
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::Http { retry_after, .. } => *retry_after,
            _ => None,
        }
    }
}

/// Turns a non-success response into `PollError::Http`.
pub fn check_status(resp: &Response) -> Result<(), PollError> {
    let status = resp.status();
    if status.is_success() || status == StatusCode::NOT_MODIFIED {
        return Ok(());
    }
    let retry_after = resp
        .headers()
        .get(header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_secs);
    Err(PollError::Http {
        status,
        retry_after,
    })
}

/// Reads a body, refusing anything over `MAX_BODY_BYTES`.
pub async fn read_body(resp: Response) -> Result<Vec<u8>, PollError> {
    if resp
        .content_length()
        .is_some_and(|n| n as usize > MAX_BODY_BYTES)
    {
        return Err(PollError::TooLarge);
    }
    let bytes = resp.bytes().await?;
    if bytes.len() > MAX_BODY_BYTES {
        return Err(PollError::TooLarge);
    }
    Ok(bytes.to_vec())
}

pub enum AnySource {
    Rss(RssSource),
    Gdelt(GdeltSource),
}

impl AnySource {
    pub fn new(cfg: &SourceConfig, defaults: &Defaults) -> Self {
        match cfg.kind {
            Kind::Rss => Self::Rss(RssSource::new(cfg)),
            Kind::Gdelt => Self::Gdelt(GdeltSource::live(cfg, defaults)),
        }
    }

    pub async fn poll(&mut self, http: &reqwest::Client) -> Result<PollResult, PollError> {
        match self {
            Self::Rss(s) => s.poll(http).await,
            Self::Gdelt(s) => s.poll(http).await,
        }
    }
}

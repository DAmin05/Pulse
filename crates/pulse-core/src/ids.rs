//! Stable article identity.
//!
//! The same article is often reachable through many URLs (tracking parameters,
//! `http` vs `https`, `www.`, trailing slashes). Canonicalizing before hashing
//! means a re-poll or a share link maps to the same article id.

use sha2::{Digest, Sha256};
use url::Url;

#[derive(Debug, thiserror::Error)]
pub enum IdError {
    #[error("invalid url: {0}")]
    InvalidUrl(#[from] url::ParseError),
    #[error("unsupported url scheme: {0}")]
    UnsupportedScheme(String),
}

/// Query parameters that never identify content.
fn is_tracking_param(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key.starts_with("utm_")
        || matches!(
            key.as_str(),
            "fbclid"
                | "gclid"
                | "dclid"
                | "msclkid"
                | "mc_cid"
                | "mc_eid"
                | "igshid"
                | "ref"
                | "ref_src"
                | "cmpid"
                | "at_medium"
                | "at_campaign"
                | "ocid"
                | "smid"
                | "smtyp"
                | "partner"
                | "rss"
                | "cid"
        )
}

pub fn canonicalize_url(raw: &str) -> Result<String, IdError> {
    let mut url = Url::parse(raw.trim())?;
    match url.scheme() {
        "http" | "https" => {}
        other => return Err(IdError::UnsupportedScheme(other.to_owned())),
    }
    // `set_scheme` only fails for special/non-special mismatches; http→https is fine.
    let _ = url.set_scheme("https");
    let _ = url.set_port(None);
    url.set_fragment(None);

    if let Some(host) = url.host_str().map(str::to_owned) {
        if let Some(stripped) = host.strip_prefix("www.") {
            url.set_host(Some(stripped))?;
        }
    }

    let mut params: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(k, _)| !is_tracking_param(k))
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    params.sort();
    if params.is_empty() {
        url.set_query(None);
    } else {
        url.query_pairs_mut().clear().extend_pairs(params);
    }

    let path = url.path().to_owned();
    if path.len() > 1 && path.ends_with('/') {
        url.set_path(path.trim_end_matches('/'));
    }

    Ok(url.into())
}

/// 128-bit hex id of the canonical URL.
pub fn article_id(canonical_url: &str) -> String {
    let digest = Sha256::digest(canonical_url.as_bytes());
    hex::encode(&digest[..16])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variants_of_one_article_share_an_id() {
        let variants = [
            "https://www.bbc.com/news/world-123?utm_source=rss&utm_medium=feed",
            "http://bbc.com/news/world-123/",
            "https://bbc.com:443/news/world-123#comments",
            "https://BBC.com/news/world-123?at_medium=RSS",
        ];
        let ids: Vec<_> = variants
            .iter()
            .map(|u| article_id(&canonicalize_url(u).unwrap()))
            .collect();
        assert!(ids.windows(2).all(|w| w[0] == w[1]), "{ids:?}");
        assert_eq!(ids[0].len(), 32);
    }

    #[test]
    fn content_params_are_kept_and_sorted() {
        let a = canonicalize_url("https://example.com/a?b=2&a=1&utm_campaign=x").unwrap();
        assert_eq!(a, "https://example.com/a?a=1&b=2");
    }

    #[test]
    fn different_articles_differ() {
        let a = article_id(&canonicalize_url("https://example.com/story?id=1").unwrap());
        let b = article_id(&canonicalize_url("https://example.com/story?id=2").unwrap());
        assert_ne!(a, b);
    }

    #[test]
    fn rejects_non_http() {
        assert!(matches!(
            canonicalize_url("ftp://example.com/x"),
            Err(IdError::UnsupportedScheme(_))
        ));
    }
}

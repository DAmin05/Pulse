//! RSS / Atom / JSON Feed polling with conditional GET.

use reqwest::{StatusCode, header};

use crate::normalize::Candidate;
use crate::source::{PollError, PollResult, check_status, read_body};
use crate::sources::SourceConfig;

pub struct RssSource {
    url: String,
    etag: Option<String>,
    last_modified: Option<String>,
}

impl RssSource {
    pub fn new(cfg: &SourceConfig) -> Self {
        Self {
            url: cfg.url.clone().expect("validated: rss sources have a url"),
            etag: None,
            last_modified: None,
        }
    }

    pub async fn poll(&mut self, http: &reqwest::Client) -> Result<PollResult, PollError> {
        let mut req = http.get(&self.url);
        if let Some(etag) = &self.etag {
            req = req.header(header::IF_NONE_MATCH, etag);
        }
        if let Some(lm) = &self.last_modified {
            req = req.header(header::IF_MODIFIED_SINCE, lm);
        }
        let resp = req.send().await?;
        check_status(&resp)?;
        if resp.status() == StatusCode::NOT_MODIFIED {
            return Ok(PollResult {
                items: Vec::new(),
                not_modified: true,
            });
        }

        let header_value = |name| {
            resp.headers()
                .get(name)
                .and_then(|v: &header::HeaderValue| v.to_str().ok())
                .map(str::to_owned)
        };
        let etag = header_value(header::ETAG);
        let last_modified = header_value(header::LAST_MODIFIED);
        let body = read_body(resp).await?;
        let items = parse(&body, &self.url)?;

        // Only remember validators once the body parsed, so a bad response
        // isn't cached as "not modified" next time.
        self.etag = etag;
        self.last_modified = last_modified;
        Ok(PollResult {
            items,
            not_modified: false,
        })
    }
}

/// Media RSS elements feed-rs requires to contain text, but which some
/// publishers emit empty (`<media:title></media:title>`, `<media:description/>`).
const MEDIA_TEXT_ELEMENTS: &[&str] = &[
    "title",
    "text",
    "credit",
    "description",
    "keywords",
    "category",
    "copyright",
];

fn strip_empty_media_elements(body: &[u8]) -> Vec<u8> {
    let mut text = String::from_utf8_lossy(body).into_owned();
    for name in MEDIA_TEXT_ELEMENTS {
        for empty in [
            format!("<media:{name}></media:{name}>"),
            format!("<media:{name}/>"),
            format!("<media:{name} />"),
        ] {
            text = text.replace(&empty, "");
        }
    }
    text.into_bytes()
}

pub fn parse(body: &[u8], feed_url: &str) -> Result<Vec<Candidate>, PollError> {
    // A feed-rs parser keeps state after a failed parse, so retry with a fresh one.
    let parser = || {
        feed_rs::parser::Builder::new()
            .base_uri(Some(feed_url))
            .build()
    };
    let feed = parser()
        .parse(body)
        .or_else(|_| parser().parse(strip_empty_media_elements(body).as_slice()))
        .map_err(|e| PollError::Parse(e.to_string()))?;
    let feed_lang = feed.language.clone();

    Ok(feed
        .entries
        .into_iter()
        .filter_map(|entry| {
            let url = entry
                .links
                .iter()
                .find(|l| l.rel.as_deref().is_none_or(|r| r == "alternate"))
                .or_else(|| entry.links.first())
                .map(|l| l.href.clone())
                .or_else(|| entry.id.starts_with("http").then(|| entry.id.clone()))?;
            let summary = entry
                .summary
                .map(|t| t.content)
                .or_else(|| entry.content.and_then(|c| c.body))
                .or_else(|| {
                    entry
                        .media
                        .into_iter()
                        .find_map(|m| m.description.map(|d| d.content))
                })
                .unwrap_or_default();
            Some(Candidate {
                url,
                title: entry.title.map(|t| t.content).unwrap_or_default(),
                summary,
                published_at_ms: entry
                    .published
                    .or(entry.updated)
                    .map(|t| t.timestamp_millis()),
                lang_hint: feed_lang.clone(),
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rss2() {
        let xml = br#"<?xml version="1.0"?>
        <rss version="2.0"><channel>
          <title>T</title><language>es-ES</language>
          <item>
            <title>Sismo en M&#233;xico</title>
            <link>https://example.com/a?utm_source=rss</link>
            <description><![CDATA[<p>Detalles</p>]]></description>
            <pubDate>Thu, 01 Oct 2026 12:00:00 GMT</pubDate>
          </item>
          <item><title>No date</title><link>/relative/b</link></item>
        </channel></rss>"#;
        let items = parse(xml, "https://example.com/feed.xml").unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].title, "Sismo en México");
        assert_eq!(items[0].summary, "<p>Detalles</p>");
        assert_eq!(items[0].lang_hint.as_deref(), Some("es-es"));
        assert_eq!(items[0].published_at_ms, Some(1_790_856_000_000));
        assert_eq!(items[1].url, "https://example.com/relative/b");
        assert_eq!(items[1].published_at_ms, None);
    }

    #[test]
    fn parses_atom() {
        let xml = br#"<?xml version="1.0" encoding="utf-8"?>
        <feed xmlns="http://www.w3.org/2005/Atom" xml:lang="de">
          <title>T</title><id>urn:x</id><updated>2026-10-01T10:00:00Z</updated>
          <entry>
            <title>Schlagzeile</title><id>urn:1</id>
            <link rel="alternate" href="https://example.de/1"/>
            <updated>2026-10-01T10:00:00Z</updated>
            <summary>Kurz</summary>
          </entry>
        </feed>"#;
        let items = parse(xml, "https://example.de/atom").unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].url, "https://example.de/1");
        assert_eq!(items[0].summary, "Kurz");
    }

    #[test]
    fn tolerates_empty_media_elements() {
        let xml = br#"<?xml version="1.0"?>
        <rss version="2.0" xmlns:media="http://search.yahoo.com/mrss/"><channel><title>T</title>
          <item>
            <title>Haber</title><link>https://example.com.tr/1</link>
            <media:content url="https://example.com.tr/a.jpg">
              <media:title></media:title><media:description/><media:credit></media:credit>
            </media:content>
          </item>
        </channel></rss>"#;
        let items = parse(xml, "https://example.com.tr/rss").unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "Haber");
    }

    #[test]
    fn rejects_garbage() {
        assert!(matches!(
            parse(b"<html>nope</html>", "https://x.y"),
            Err(PollError::Parse(_))
        ));
    }
}

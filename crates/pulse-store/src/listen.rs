//! Caches and usage accounting for "Listen in any language".

use std::collections::HashMap;

use anyhow::Result;
use chrono::{DateTime, NaiveDate, Utc};
use serde::Serialize;
use tokio_postgres::GenericClient;

/// Cached translations of `hashes` into `target`, keyed by source hash.
pub async fn translations(
    c: &impl GenericClient,
    hashes: &[String],
    target: &str,
) -> Result<HashMap<String, String>> {
    if hashes.is_empty() {
        return Ok(HashMap::new());
    }
    Ok(c.query(
        "SELECT source_hash, text FROM translations WHERE source_hash = ANY($1) AND target = $2",
        &[&hashes, &target],
    )
    .await?
    .iter()
    .map(|r| (r.get(0), r.get(1)))
    .collect())
}

pub async fn put_translation(
    c: &impl GenericClient,
    source_hash: &str,
    target: &str,
    provider: &str,
    text: &str,
) -> Result<()> {
    c.execute(
        "INSERT INTO translations (source_hash, target, provider, text) VALUES ($1, $2, $3, $4)
         ON CONFLICT (source_hash, target) DO NOTHING",
        &[&source_hash, &target, &provider, &text],
    )
    .await?;
    Ok(())
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Audio {
    pub key: String,
    pub story_id: String,
    pub lang: String,
    pub provider: String,
    pub model: String,
    pub voice: String,
    pub text: String,
    pub characters: i32,
    pub bytes: i32,
    pub content_type: String,
    pub words: serde_json::Value,
    pub duration: f32,
    pub created_at: DateTime<Utc>,
}

/// A cached briefing's audio metadata; counts the play.
pub async fn audio(c: &impl GenericClient, key: &str) -> Result<Option<Audio>> {
    Ok(c.query_opt(
        "UPDATE briefing_audio SET plays = plays + 1 WHERE key = $1
             RETURNING key, story_id, lang, provider, model, voice, text, characters, bytes,
                       content_type, words, duration, created_at",
        &[&key],
    )
    .await?
    .map(|r| Audio {
        key: r.get(0),
        story_id: r.get(1),
        lang: r.get(2),
        provider: r.get(3),
        model: r.get(4),
        voice: r.get(5),
        text: r.get(6),
        characters: r.get(7),
        bytes: r.get(8),
        content_type: r.get(9),
        words: r.get(10),
        duration: r.get(11),
        created_at: r.get(12),
    }))
}

/// Content type of stored audio, if `key` exists (for serving it).
pub async fn audio_content_type(c: &impl GenericClient, key: &str) -> Result<Option<String>> {
    Ok(c.query_opt(
        "SELECT content_type FROM briefing_audio WHERE key = $1",
        &[&key],
    )
    .await?
    .map(|r| r.get(0)))
}

pub async fn put_audio(c: &impl GenericClient, a: &Audio) -> Result<()> {
    c.execute(
        "INSERT INTO briefing_audio (key, story_id, lang, provider, model, voice, text, characters,
                                     bytes, content_type, words, duration, created_at, plays)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, 1)
         ON CONFLICT (key) DO NOTHING",
        &[
            &a.key,
            &a.story_id,
            &a.lang,
            &a.provider,
            &a.model,
            &a.voice,
            &a.text,
            &a.characters,
            &a.bytes,
            &a.content_type,
            &a.words,
            &a.duration,
            &a.created_at,
        ],
    )
    .await?;
    Ok(())
}

/// Records characters sent to a provider on `day`.
pub async fn add_usage(
    c: &impl GenericClient,
    day: NaiveDate,
    provider: &str,
    characters: i64,
) -> Result<()> {
    c.execute(
        "INSERT INTO listen_usage (day, provider, characters, requests) VALUES ($1, $2, $3, 1)
         ON CONFLICT (day, provider)
         DO UPDATE SET characters = listen_usage.characters + $3, requests = listen_usage.requests + 1",
        &[&day, &provider, &characters],
    )
    .await?;
    Ok(())
}

/// Characters sent to `provider` on days in `[from, to]`.
pub async fn usage(
    c: &impl GenericClient,
    provider: &str,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<i64> {
    Ok(c.query_one(
        "SELECT COALESCE(SUM(characters), 0)::bigint FROM listen_usage
             WHERE provider = $1 AND day BETWEEN $2 AND $3",
        &[&provider, &from, &to],
    )
    .await?
    .get(0))
}

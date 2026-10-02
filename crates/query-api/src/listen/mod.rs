//! "Listen in any language": a story briefing, translated and read aloud.
//!
//! - `GET /api/listen`: languages, which providers are configured, budgets.
//! - `POST /api/stories/{id}/briefing` `{lang, voice?, speech?}`: composes the
//!   briefing (see [`briefing`]), translates the parts not already in `lang`,
//!   and synthesizes it with ElevenLabs, returning the text, per-word timings
//!   and an audio URL. When speech isn't available (no key, budget spent,
//!   provider error) it returns the text with a `fallback` reason, and the
//!   browser reads it with the Web Speech API instead.
//! - `GET /api/audio/{key}`: the synthesized MP3.
//!
//! Both caches are content-addressed: a translation by its source text, audio
//! by (provider, model, voice, language, final text). Asking again for an
//! unchanged briefing costs no characters from either provider.

pub mod briefing;
pub mod langs;
pub mod speech;
pub mod store;
pub mod translate;

#[cfg(test)]
mod providers_test;

use std::fmt;

use anyhow::Result;
use axum::Json;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, header};
use axum::response::Response;
use chrono::{Datelike, NaiveDate, Utc};
use pulse_core::config::{Settings, env_or};
use pulse_store::listen as cache;
use pulse_store::reader;
use pulse_store::tokio_postgres::GenericClient;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::Shared;
use crate::routes::{ApiError, ApiResult, db};
use briefing::{Segment, Story};
use langs::Language;
use speech::ElevenLabs;
use store::AudioStore;
use translate::Translator;

pub struct Listen {
    pub translator: Option<Translator>,
    pub speech: Option<ElevenLabs>,
    pub store: AudioStore,
    /// Briefing length cap, in characters.
    pub max_chars: usize,
    /// Speech characters per UTC day.
    pub speech_daily_chars: i64,
    /// Translation characters per calendar month (UTC).
    pub translation_monthly_chars: i64,
    /// One synthesis at a time, so concurrent requests for the same briefing
    /// pay once (the second finds it cached).
    synth: tokio::sync::Mutex<()>,
}

impl Listen {
    pub fn from_env(settings: &Settings) -> Result<Self> {
        let listen = Self {
            translator: Translator::from_env(),
            speech: ElevenLabs::from_env(),
            store: AudioStore::from_env(settings)?,
            max_chars: env_or("PULSE_BRIEFING_MAX_CHARS", "520").parse()?,
            speech_daily_chars: env_or("PULSE_TTS_DAILY_CHARS", "3000").parse()?,
            translation_monthly_chars: env_or("PULSE_TRANSLATE_MONTHLY_CHARS", "400000").parse()?,
            synth: tokio::sync::Mutex::new(()),
        };
        tracing::info!(
            translation = listen.translator.as_ref().map_or("none", Translator::name),
            speech = if listen.speech.is_some() {
                "elevenlabs"
            } else {
                "browser fallback"
            },
            audio_store = listen.store.description,
            "listen configured"
        );
        Ok(listen)
    }
}

/// Why a provider call didn't produce a result.
#[derive(Debug)]
pub enum ProviderError {
    Unauthorized,
    Quota,
    RateLimited,
    Network(String),
    Other(String),
}

impl ProviderError {
    fn network(e: reqwest::Error) -> Self {
        Self::Network(e.without_url().to_string())
    }

    /// Passes successful responses through; maps failures to a kind.
    async fn check(r: reqwest::Response) -> Result<reqwest::Response, Self> {
        let status = r.status();
        if status.is_success() {
            return Ok(r);
        }
        let body: String = r
            .text()
            .await
            .unwrap_or_default()
            .chars()
            .take(300)
            .collect();
        // DeepL signals an exhausted quota with 456; ElevenLabs with a body status.
        Err(
            if status.as_u16() == 456 || body.contains("quota_exceeded") {
                Self::Quota
            } else if status.as_u16() == 401 || status.as_u16() == 403 {
                Self::Unauthorized
            } else if status.as_u16() == 429 {
                Self::RateLimited
            } else {
                Self::Other(format!("{status}: {body}"))
            },
        )
    }

    fn reason(&self) -> &'static str {
        match self {
            Self::Unauthorized => "unauthorized",
            Self::Quota => "quota",
            Self::RateLimited => "rate_limited",
            Self::Network(_) | Self::Other(_) => "provider_error",
        }
    }
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unauthorized => f.write_str("the API key was rejected"),
            Self::Quota => f.write_str("the provider's quota is used up"),
            Self::RateLimited => f.write_str("rate limited by the provider"),
            Self::Network(e) | Self::Other(e) => f.write_str(e),
        }
    }
}

fn sha256(parts: &[&str]) -> String {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p.as_bytes());
        h.update([0]);
    }
    hex::encode(h.finalize())
}

fn month_start(today: NaiveDate) -> NaiveDate {
    today.with_day(1).unwrap_or(today)
}

// ---------------------------------------------------------------------------

pub async fn capabilities(
    axum::extract::State(state): axum::extract::State<Shared>,
) -> ApiResult<Value> {
    let listen = &state.listen;
    let c = db(&state).await?;
    let today = Utc::now().date_naive();
    let speech = match &listen.speech {
        Some(s) => {
            let used = cache::usage(&**c, "elevenlabs", today, today).await?;
            json!({
                "provider": "elevenlabs",
                "model": s.model,
                "default_voice": s.default_voice,
                "voices": s.voices().await,
                "budget": {
                    "used_today": used,
                    "daily_limit": listen.speech_daily_chars,
                    "account": s.subscription().await,
                },
            })
        }
        None => Value::Null,
    };
    let translation = match &listen.translator {
        Some(t) => json!({
            "provider": t.name(),
            "budget": {
                "used_this_month": cache::usage(&**c, t.name(), month_start(today), today).await?,
                "monthly_limit": listen.translation_monthly_chars,
            },
        }),
        None => Value::Null,
    };
    Ok(Json(json!({
        "languages": langs::LANGUAGES.iter().map(|l| json!({
            "code": l.code, "name": l.name, "native": l.native, "bcp47": l.bcp47,
        })).collect::<Vec<_>>(),
        "translation": translation,
        "speech": speech,
        "max_chars": listen.max_chars,
    })))
}

#[derive(Deserialize)]
pub struct BriefingRequest {
    lang: String,
    voice: Option<String>,
    /// `false`: text only (the client reads it with its own voice).
    speech: Option<bool>,
}

pub async fn briefing(
    State(state): State<Shared>,
    Path(id): Path<String>,
    Json(req): Json<BriefingRequest>,
) -> ApiResult<Value> {
    let lang = langs::find(&req.lang)
        .ok_or_else(|| ApiError::bad_request(format!("unsupported language {:?}", req.lang)))?;
    let listen = &state.listen;
    let c = db(&state).await?;
    let latest = reader::latest_offset(&**c).await?;
    let detail = reader::story(&**c, &id, latest)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("story {id} not found")))?;
    let card = &detail.card;
    let headline_lang = detail
        .articles
        .iter()
        .find(|a| a.id == card.headline_article_id)
        .map_or(card.lang.as_str(), |a| a.lang.as_str());
    let story = Story {
        headline: &card.headline,
        headline_lang,
        source_count: card.source_count,
        languages: card.langs.len(),
        articles: &detail.articles,
    };

    // 1. Script, translated where needed.
    let mut notice: Option<String> = None;
    let (segments, translation) = match &listen.translator {
        Some(translator) => {
            let segments =
                briefing::compose(&story, lang.code, true, listen.max_chars).unwrap_or_default();
            match translate(&**c, listen, translator, segments, lang).await {
                Ok(done) => done,
                Err(e) => {
                    // Fall back to what's already in the language, if anything.
                    tracing::warn!(error = %e, lang = lang.code, "translation failed");
                    metrics::counter!("pulse_listen_translation_failures_total", "reason" => e.reason())
                        .increment(1);
                    notice = Some(format!(
                        "Translation unavailable ({e}); using {} sources only.",
                        lang.name
                    ));
                    (native_only(&story, lang, listen.max_chars)?, None)
                }
            }
        }
        None => (native_only(&story, lang, listen.max_chars)?, None),
    };
    let text = segments
        .iter()
        .map(|s| s.text.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");

    // 2. Speech, cached by content.
    let (audio, fallback) = if req.speech == Some(false) {
        (
            None,
            Some(("not_requested", "Speech wasn't requested.".to_owned())),
        )
    } else {
        match &listen.speech {
            None => (
                None,
                Some((
                    "not_configured",
                    "No ElevenLabs key is configured, so your browser's voice reads it.".to_owned(),
                )),
            ),
            Some(tts) => {
                match speak(&**c, listen, tts, &id, lang, req.voice.as_deref(), &text).await {
                    Ok(audio) => (Some(audio), None),
                    Err(SpeakError::Budget(message)) => (None, Some(("budget", message))),
                    Err(SpeakError::Provider(e)) => {
                        tracing::warn!(error = %e, "speech synthesis failed");
                        (
                            None,
                            Some((
                                e.reason(),
                                format!("ElevenLabs: {e}. Your browser's voice reads it instead."),
                            )),
                        )
                    }
                    Err(SpeakError::Internal(e)) => return Err(e.into()),
                }
            }
        }
    };
    let outcome = match (&audio, &fallback) {
        (Some(a), _) if a["cached"] == true => "cached",
        (Some(_), _) => "synthesized",
        _ => "fallback",
    };
    metrics::counter!("pulse_listen_briefings_total", "outcome" => outcome).increment(1);

    Ok(Json(json!({
        "story_id": id,
        "lang": lang.code,
        "bcp47": lang.bcp47,
        "text": text,
        "segments": segments.iter().map(|s| json!({
            "kind": s.text.kind,
            "text": s.text.text,
            "original_lang": s.original_lang,
            "translated": s.translated,
            "source": s.text.source,
        })).collect::<Vec<_>>(),
        "translation": translation,
        "audio": audio,
        "fallback": fallback.map(|(reason, message)| json!({ "reason": reason, "message": message })),
        "notice": notice,
    })))
}

/// A briefing segment after translation.
struct Translated {
    text: Segment,
    original_lang: String,
    translated: bool,
}

fn native_only(
    story: &Story,
    lang: &Language,
    max_chars: usize,
) -> Result<Vec<Translated>, ApiError> {
    let segments = briefing::compose(story, lang.code, false, max_chars).ok_or_else(|| {
        ApiError::unprocessable(format!(
            "No {} coverage of this story, and translation isn't configured (set DEEPL_API_KEY).",
            lang.name
        ))
    })?;
    Ok(segments
        .into_iter()
        .map(|s| Translated {
            original_lang: s.lang.clone(),
            text: s,
            translated: false,
        })
        .collect())
}

/// Translates the segments not already in `lang`, through the cache.
async fn translate(
    c: &impl GenericClient,
    listen: &Listen,
    translator: &Translator,
    segments: Vec<Segment>,
    lang: &Language,
) -> Result<(Vec<Translated>, Option<Value>), ProviderError> {
    let foreign: Vec<usize> = (0..segments.len())
        .filter(|&i| !langs::same(&segments[i].lang, lang.code))
        .collect();
    let hashes: Vec<String> = foreign
        .iter()
        .map(|&i| sha256(&[&segments[i].text]))
        .collect();
    let cached = cache::translations(c, &hashes, lang.code)
        .await
        .map_err(|e| ProviderError::Other(format!("{e:#}")))?;
    let missing: Vec<usize> = (0..foreign.len())
        .filter(|&j| !cached.contains_key(&hashes[j]))
        .collect();
    let texts: Vec<String> = missing
        .iter()
        .map(|&j| segments[foreign[j]].text.clone())
        .collect();
    let characters: i64 = texts.iter().map(|t| t.chars().count() as i64).sum();

    let mut fresh = std::collections::HashMap::new();
    if !texts.is_empty() {
        let today = Utc::now().date_naive();
        let used = cache::usage(c, translator.name(), month_start(today), today)
            .await
            .map_err(|e| ProviderError::Other(format!("{e:#}")))?;
        if used + characters > listen.translation_monthly_chars {
            return Err(ProviderError::Quota);
        }
        let out = translator.translate(&texts, lang).await?;
        let record = async {
            for (&j, t) in missing.iter().zip(&out) {
                cache::put_translation(c, &hashes[j], lang.code, translator.name(), t).await?;
            }
            cache::add_usage(c, today, translator.name(), characters).await
        };
        record
            .await
            .map_err(|e| ProviderError::Other(format!("{e:#}")))?;
        metrics::counter!("pulse_listen_characters_total", "provider" => translator.name())
            .increment(characters as u64);
        for (&j, t) in missing.iter().zip(out) {
            fresh.insert(hashes[j].clone(), t);
        }
    }

    let mut result = Vec::with_capacity(segments.len());
    for (i, mut segment) in segments.into_iter().enumerate() {
        let original_lang = segment.lang.clone();
        let translated = match foreign.iter().position(|&f| f == i) {
            Some(j) => {
                let hash = &hashes[j];
                segment.text = fresh
                    .get(hash)
                    .or_else(|| cached.get(hash))
                    .cloned()
                    .unwrap_or(segment.text);
                segment.lang = lang.code.to_owned();
                true
            }
            None => false,
        };
        result.push(Translated {
            text: segment,
            original_lang,
            translated,
        });
    }
    let summary = (!foreign.is_empty()).then(|| {
        json!({
            "provider": translator.name(),
            "segments": foreign.len(),
            "cached_segments": foreign.len() - missing.len(),
            "characters": characters,
        })
    });
    Ok((result, summary))
}

enum SpeakError {
    Budget(String),
    Provider(ProviderError),
    Internal(anyhow::Error),
}

impl From<anyhow::Error> for SpeakError {
    fn from(e: anyhow::Error) -> Self {
        Self::Internal(e)
    }
}

/// Audio for `text`: from the cache, else synthesized within budget.
async fn speak(
    c: &impl GenericClient,
    listen: &Listen,
    tts: &ElevenLabs,
    story_id: &str,
    lang: &Language,
    voice: Option<&str>,
    text: &str,
) -> Result<Value, SpeakError> {
    let voices = tts.voices().await;
    let voice = voice
        .filter(|v| voices.iter().any(|x| x.id == *v))
        .unwrap_or(&tts.default_voice)
        .to_owned();
    let voice_name = voices
        .iter()
        .find(|v| v.id == voice)
        .map(|v| v.name.clone());
    let key = sha256(&["elevenlabs", &tts.model, &voice, lang.code, text]);
    let describe = |a: &cache::Audio, cached: bool| {
        json!({
            "url": format!("/api/audio/{}", a.key),
            "voice": a.voice,
            "voice_name": voice_name,
            "model": a.model,
            "words": a.words,
            "duration": a.duration,
            "characters": a.characters,
            "cached": cached,
        })
    };
    if let Some(a) = cache::audio(c, &key).await? {
        return Ok(describe(&a, true));
    }

    let _one_at_a_time = listen.synth.lock().await;
    if let Some(a) = cache::audio(c, &key).await? {
        return Ok(describe(&a, true));
    }
    let characters = text.chars().count() as i64;
    let today = Utc::now().date_naive();
    let used = cache::usage(c, "elevenlabs", today, today).await?;
    if used + characters > listen.speech_daily_chars {
        return Err(SpeakError::Budget(format!(
            "Today's speech budget ({} characters) is used up; your browser's voice reads it instead.",
            listen.speech_daily_chars
        )));
    }
    if let Some(s) = tts.subscription().await
        && s.limit - s.used < characters
    {
        return Err(SpeakError::Budget(
            "The ElevenLabs account's character quota is used up; your browser's voice reads it instead.".into(),
        ));
    }

    let speech = tts
        .synthesize(text, &voice, lang)
        .await
        .map_err(SpeakError::Provider)?;
    // Count the characters as soon as they're spent, even if storing fails.
    cache::add_usage(c, today, "elevenlabs", characters).await?;
    metrics::counter!("pulse_listen_characters_total", "provider" => "elevenlabs")
        .increment(characters as u64);
    let audio = cache::Audio {
        key: key.clone(),
        story_id: story_id.to_owned(),
        lang: lang.code.to_owned(),
        provider: "elevenlabs".into(),
        model: tts.model.clone(),
        voice,
        text: text.to_owned(),
        characters: characters as i32,
        bytes: speech.audio.len() as i32,
        content_type: speech.content_type.into(),
        words: json!(speech.words),
        duration: speech.duration,
        created_at: Utc::now(),
    };
    listen.store.put(&key, speech.audio).await?;
    cache::put_audio(c, &audio).await?;
    Ok(describe(&audio, false))
}

pub async fn audio(
    State(state): State<Shared>,
    Path(key): Path<String>,
) -> Result<Response, ApiError> {
    if key.len() != 64 || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(ApiError::not_found("no such audio"));
    }
    let c = db(&state).await?;
    let content_type = cache::audio_content_type(&**c, &key)
        .await?
        .ok_or_else(|| ApiError::not_found("no such audio"))?;
    drop(c);
    let bytes = state.listen.store.get(&key).await?;
    let mut response = Response::new(Body::from(bytes));
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&content_type).unwrap_or(HeaderValue::from_static("audio/mpeg")),
    );
    // Content-addressed: the bytes behind a key never change.
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=31536000, immutable"),
    );
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_keys_separate_their_parts() {
        assert_ne!(sha256(&["ab", "c"]), sha256(&["a", "bc"]));
        assert_eq!(sha256(&["x"]).len(), 64);
    }

    #[test]
    fn month_start_is_the_first() {
        let d = NaiveDate::from_ymd_opt(2026, 10, 17).unwrap();
        assert_eq!(
            month_start(d),
            NaiveDate::from_ymd_opt(2026, 10, 1).unwrap()
        );
    }
}

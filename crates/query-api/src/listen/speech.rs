//! Text to speech via ElevenLabs, with character timings for the transcript.

use std::time::{Duration, Instant};

use base64::Engine;
use bytes::Bytes;
use pulse_core::config::env_or;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::Mutex;

use super::ProviderError;
use super::langs::Language;
use super::translate::non_empty;

/// A premade voice that reads every supported language.
pub const DEFAULT_VOICE: &str = "JBFqnCBsd6RMkjVDRZzb";
const VOICES_TTL: Duration = Duration::from_secs(3600);
const SUBSCRIPTION_TTL: Duration = Duration::from_secs(300);
const MAX_VOICES: usize = 12;

#[derive(Debug, Clone, Serialize)]
pub struct Voice {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct Subscription {
    pub used: i64,
    pub limit: i64,
    pub resets_at: Option<i64>,
}

pub struct Speech {
    pub audio: Bytes,
    pub content_type: &'static str,
    /// Word starts: (UTF-16 offset into the text, seconds).
    pub words: Vec<(usize, f32)>,
    pub duration: f32,
}

pub struct ElevenLabs {
    http: reqwest::Client,
    url: String,
    key: String,
    pub model: String,
    pub default_voice: String,
    voices: Mutex<Option<(Instant, Vec<Voice>)>>,
    subscription: Mutex<Option<(Instant, Option<Subscription>)>>,
}

impl ElevenLabs {
    /// Configured when `ELEVENLABS_API_KEY` is set.
    pub fn from_env() -> Option<Self> {
        Some(Self::new(
            env_or("PULSE_ELEVENLABS_URL", "https://api.elevenlabs.io"),
            non_empty("ELEVENLABS_API_KEY")?,
            // Flash v2.5: 32 languages, low latency, half the credits of Multilingual v2.
            env_or("PULSE_TTS_MODEL", "eleven_flash_v2_5"),
            env_or("PULSE_TTS_VOICE", DEFAULT_VOICE),
        ))
    }

    pub fn new(url: String, key: String, model: String, default_voice: String) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(60))
                .build()
                .unwrap_or_default(),
            url,
            key,
            model,
            default_voice,
            voices: Mutex::new(None),
            subscription: Mutex::new(None),
        }
    }

    pub async fn synthesize(
        &self,
        text: &str,
        voice: &str,
        lang: &Language,
    ) -> Result<Speech, ProviderError> {
        #[derive(Deserialize)]
        struct Response {
            audio_base64: String,
            alignment: Option<Alignment>,
        }
        let mut body = json!({ "text": text, "model_id": self.model });
        // Only the v2.5 models accept a language hint; others infer it from the text.
        if self.model.ends_with("v2_5") {
            body["language_code"] = json!(lang.code);
        }
        let response = self
            .http
            .post(format!(
                "{}/v1/text-to-speech/{voice}/with-timestamps?output_format=mp3_44100_128",
                self.url
            ))
            .header("xi-api-key", &self.key)
            .json(&body)
            .send()
            .await
            .map_err(ProviderError::network)?;
        let r: Response = ProviderError::check(response)
            .await?
            .json()
            .await
            .map_err(ProviderError::network)?;
        let audio = base64::engine::general_purpose::STANDARD
            .decode(r.audio_base64)
            .map_err(|e| ProviderError::Other(format!("bad audio encoding: {e}")))?;
        if audio.is_empty() {
            return Err(ProviderError::Other("empty audio".into()));
        }
        let (words, duration) = r.alignment.map(|a| a.word_starts(text)).unwrap_or_default();
        // Invalidate the cached quota: this request used some.
        *self.subscription.lock().await = None;
        Ok(Speech {
            audio: audio.into(),
            content_type: "audio/mpeg",
            words,
            duration,
        })
    }

    /// Voices to offer: the account's premade voices (cached), or the default alone.
    pub async fn voices(&self) -> Vec<Voice> {
        let mut cache = self.voices.lock().await;
        if let Some((at, voices)) = cache.as_ref()
            && at.elapsed() < VOICES_TTL
        {
            return voices.clone();
        }
        let voices = match self.fetch_voices().await {
            Ok(v) if !v.is_empty() => v,
            Ok(_) => vec![self.fallback_voice()],
            Err(e) => {
                tracing::warn!(error = %e, "listing ElevenLabs voices failed; offering the default");
                vec![self.fallback_voice()]
            }
        };
        *cache = Some((Instant::now(), voices.clone()));
        voices
    }

    fn fallback_voice(&self) -> Voice {
        Voice {
            id: self.default_voice.clone(),
            name: "Default".into(),
            description: None,
        }
    }

    async fn fetch_voices(&self) -> Result<Vec<Voice>, ProviderError> {
        #[derive(Deserialize)]
        struct Response {
            voices: Vec<Item>,
        }
        #[derive(Deserialize)]
        struct Item {
            voice_id: String,
            name: String,
            category: Option<String>,
            #[serde(default)]
            labels: serde_json::Map<String, serde_json::Value>,
        }
        let response = self
            .http
            .get(format!("{}/v1/voices", self.url))
            .header("xi-api-key", &self.key)
            .send()
            .await
            .map_err(ProviderError::network)?;
        let r: Response = ProviderError::check(response)
            .await?
            .json()
            .await
            .map_err(ProviderError::network)?;
        let mut items = r.voices;
        // The default first, then premade voices (available on every plan), by name.
        items.sort_by_key(|v| {
            (
                v.voice_id != self.default_voice,
                v.category.as_deref() != Some("premade"),
                v.name.clone(),
            )
        });
        Ok(items
            .into_iter()
            .take(MAX_VOICES)
            .map(|v| {
                let label = |k: &str| v.labels.get(k).and_then(|x| x.as_str()).map(str::to_owned);
                let description = [
                    label("accent"),
                    label("gender"),
                    label("descriptive").or(label("description")),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(", ");
                Voice {
                    id: v.voice_id,
                    // "George - Warm, Captivating Storyteller" → "George".
                    name: v
                        .name
                        .split(" - ")
                        .next()
                        .unwrap_or(&v.name)
                        .trim()
                        .to_owned(),
                    description: (!description.is_empty()).then_some(description),
                }
            })
            .collect())
    }

    /// Characters used and allowed this billing period, if the key may read it.
    pub async fn subscription(&self) -> Option<Subscription> {
        let mut cache = self.subscription.lock().await;
        if let Some((at, s)) = cache.as_ref()
            && at.elapsed() < SUBSCRIPTION_TTL
        {
            return *s;
        }
        #[derive(Deserialize)]
        struct Response {
            character_count: i64,
            character_limit: i64,
            next_character_count_reset_unix: Option<i64>,
        }
        let fetched = async {
            let response = self
                .http
                .get(format!("{}/v1/user/subscription", self.url))
                .header("xi-api-key", &self.key)
                .send()
                .await
                .map_err(ProviderError::network)?;
            ProviderError::check(response)
                .await?
                .json::<Response>()
                .await
                .map_err(ProviderError::network)
        }
        .await;
        let s = match fetched {
            Ok(r) => Some(Subscription {
                used: r.character_count,
                limit: r.character_limit,
                resets_at: r.next_character_count_reset_unix,
            }),
            Err(e) => {
                // Keys can be scoped without `user_read`; budgets still apply.
                tracing::debug!(error = %e, "reading ElevenLabs subscription failed");
                None
            }
        };
        *cache = Some((Instant::now(), s));
        s
    }
}

#[derive(Deserialize)]
struct Alignment {
    characters: Vec<String>,
    character_start_times_seconds: Vec<f32>,
    character_end_times_seconds: Vec<f32>,
}

impl Alignment {
    /// Start time of each word, by UTF-16 offset into `text` (what the browser
    /// indexes strings by). Empty if the alignment doesn't spell out `text`.
    fn word_starts(&self, text: &str) -> (Vec<(usize, f32)>, f32) {
        let duration = self
            .character_end_times_seconds
            .last()
            .copied()
            .unwrap_or(0.0);
        if self.characters.concat() != text
            || self.character_start_times_seconds.len() != self.characters.len()
        {
            return (Vec::new(), duration);
        }
        let mut words = Vec::new();
        let mut offset = 0;
        let mut previous_space = true;
        for (ch, &start) in self
            .characters
            .iter()
            .zip(&self.character_start_times_seconds)
        {
            let space = ch.chars().all(char::is_whitespace);
            if !space && previous_space {
                words.push((offset, start));
            }
            previous_space = space;
            offset += ch.encode_utf16().count();
        }
        (words, duration)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_starts_use_utf16_offsets() {
        let text = "Ça va 😀 bien";
        let a = Alignment {
            characters: text.chars().map(String::from).collect(),
            character_start_times_seconds: (0..text.chars().count())
                .map(|i| i as f32 * 0.1)
                .collect(),
            character_end_times_seconds: (1..=text.chars().count())
                .map(|i| i as f32 * 0.1)
                .collect(),
        };
        let (words, duration) = a.word_starts(text);
        let offsets: Vec<_> = words.iter().map(|w| w.0).collect();
        // "😀" is two UTF-16 units, so "bien" starts at 9, not 8.
        assert_eq!(offsets, [0, 3, 6, 9]);
        assert!((duration - 1.2).abs() < 1e-5);
        assert!((words[3].1 - 0.8).abs() < 1e-5);

        let (none, _) = a.word_starts("something else");
        assert!(none.is_empty());
    }
}

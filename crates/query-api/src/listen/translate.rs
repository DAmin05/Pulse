//! Machine translation: DeepL API (Free or Pro) or a LibreTranslate server.

use std::time::Duration;

use pulse_core::config::env_or;
use serde::Deserialize;
use serde_json::json;

use super::ProviderError;
use super::langs::Language;

pub enum Translator {
    DeepL {
        http: reqwest::Client,
        url: String,
        key: String,
    },
    Libre {
        http: reqwest::Client,
        url: String,
        key: Option<String>,
    },
}

impl Translator {
    /// DeepL when `DEEPL_API_KEY` is set, else LibreTranslate when
    /// `PULSE_LIBRETRANSLATE_URL` is, else none.
    pub fn from_env() -> Option<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .ok()?;
        if let Some(key) = non_empty("DEEPL_API_KEY") {
            // Free-plan keys end in ":fx" and have their own host.
            let default = if key.ends_with(":fx") {
                "https://api-free.deepl.com"
            } else {
                "https://api.deepl.com"
            };
            let url = env_or("PULSE_DEEPL_URL", default);
            return Some(Self::DeepL { http, url, key });
        }
        non_empty("PULSE_LIBRETRANSLATE_URL").map(|url| Self::Libre {
            http,
            url,
            key: non_empty("PULSE_LIBRETRANSLATE_KEY"),
        })
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::DeepL { .. } => "deepl",
            Self::Libre { .. } => "libretranslate",
        }
    }

    /// Translates each text into `target` (source language auto-detected), in order.
    pub async fn translate(
        &self,
        texts: &[String],
        target: &Language,
    ) -> Result<Vec<String>, ProviderError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let out = match self {
            Self::DeepL { http, url, key } => {
                #[derive(Deserialize)]
                struct Response {
                    translations: Vec<Item>,
                }
                #[derive(Deserialize)]
                struct Item {
                    text: String,
                }
                let response = http
                    .post(format!("{url}/v2/translate"))
                    .header("Authorization", format!("DeepL-Auth-Key {key}"))
                    .json(&json!({ "text": texts, "target_lang": target.deepl }))
                    .send()
                    .await
                    .map_err(ProviderError::network)?;
                let r: Response = ProviderError::check(response)
                    .await?
                    .json()
                    .await
                    .map_err(ProviderError::network)?;
                r.translations
                    .into_iter()
                    .map(|t| t.text)
                    .collect::<Vec<_>>()
            }
            Self::Libre { http, url, key } => {
                #[derive(Deserialize)]
                struct Response {
                    #[serde(rename = "translatedText")]
                    translated: Vec<String>,
                }
                let response = http
                    .post(format!("{url}/translate"))
                    .json(&json!({
                        "q": texts,
                        "source": "auto",
                        "target": target.code,
                        "format": "text",
                        "api_key": key,
                    }))
                    .send()
                    .await
                    .map_err(ProviderError::network)?;
                let r: Response = ProviderError::check(response)
                    .await?
                    .json()
                    .await
                    .map_err(ProviderError::network)?;
                r.translated
            }
        };
        if out.len() != texts.len() {
            return Err(ProviderError::Other(format!(
                "{} returned {} translations for {} texts",
                self.name(),
                out.len(),
                texts.len()
            )));
        }
        Ok(out)
    }
}

/// An env var's value, treating empty as unset (`.env` has `KEY=` placeholders).
pub fn non_empty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

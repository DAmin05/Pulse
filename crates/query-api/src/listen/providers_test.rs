//! Provider clients against an in-process mock of the DeepL and ElevenLabs APIs.

use axum::Router;
use axum::extract::Path;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use base64::Engine;
use serde_json::{Value, json};

use super::ProviderError;
use super::langs;
use super::speech::ElevenLabs;
use super::translate::Translator;

const KEY: &str = "test-key";
const FLASH: &str = "eleven_flash_v2_5";

fn client(url: &str, key: &str) -> ElevenLabs {
    ElevenLabs::new(
        url.into(),
        key.into(),
        FLASH.into(),
        "eleven_v3".into(),
        "v-default".into(),
    )
}

async fn deepl(headers: HeaderMap, body: axum::Json<Value>) -> (StatusCode, axum::Json<Value>) {
    if headers["authorization"] != format!("DeepL-Auth-Key {KEY}:fx") {
        return (StatusCode::FORBIDDEN, axum::Json(json!({})));
    }
    let texts = body["text"].as_array().cloned().unwrap_or_default();
    if texts.iter().any(|t| t.as_str() == Some("QUOTA")) {
        return (
            StatusCode::from_u16(456).unwrap(),
            axum::Json(json!({ "message": "Quota exceeded" })),
        );
    }
    let target = body["target_lang"].as_str().unwrap_or("?").to_owned();
    let translations: Vec<Value> = texts
        .iter()
        .map(|t| json!({ "detected_source_language": "EN", "text": format!("[{target}] {}", t.as_str().unwrap_or("")) }))
        .collect();
    (
        StatusCode::OK,
        axum::Json(json!({ "translations": translations })),
    )
}

async fn tts(
    Path(voice): Path<String>,
    headers: HeaderMap,
    body: axum::Json<Value>,
) -> (StatusCode, axum::Json<Value>) {
    if headers["xi-api-key"] != KEY {
        return (
            StatusCode::UNAUTHORIZED,
            axum::Json(json!({ "detail": { "status": "invalid_api_key" } })),
        );
    }
    let text = body["text"].as_str().unwrap_or("");
    if text == "QUOTA" {
        return (
            StatusCode::UNAUTHORIZED,
            axum::Json(
                json!({ "detail": { "status": "quota_exceeded", "message": "no credits" } }),
            ),
        );
    }
    // Models without character alignment refuse this endpoint.
    if body["model_id"] == "eleven_v3" {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            axum::Json(json!({ "detail": "alignment is not available for this model" })),
        );
    }
    assert_eq!(body["model_id"], "eleven_flash_v2_5");
    assert_eq!(body["language_code"], "fr", "the language hint is sent");
    let chars: Vec<String> = text.chars().map(String::from).collect();
    let starts: Vec<f32> = (0..chars.len()).map(|i| i as f32 * 0.05).collect();
    let ends: Vec<f32> = (1..=chars.len()).map(|i| i as f32 * 0.05).collect();
    let audio = base64::engine::general_purpose::STANDARD.encode(format!("ID3:{voice}"));
    (
        StatusCode::OK,
        axum::Json(json!({
            "audio_base64": audio,
            "alignment": {
                "characters": chars,
                "character_start_times_seconds": starts,
                "character_end_times_seconds": ends,
            },
        })),
    )
}

/// Plain audio, no timings.
async fn tts_plain(Path(voice): Path<String>, body: axum::Json<Value>) -> Vec<u8> {
    format!(
        "MP3:{voice}:{}:{}",
        body["model_id"].as_str().unwrap_or(""),
        body["language_code"].as_str().unwrap_or("")
    )
    .into_bytes()
}

async fn models(headers: HeaderMap) -> (StatusCode, axum::Json<Value>) {
    if headers["xi-api-key"] != KEY {
        return (StatusCode::UNAUTHORIZED, axum::Json(json!({})));
    }
    let langs = |codes: &[&str]| -> Vec<Value> {
        codes
            .iter()
            .map(|c| json!({ "language_id": c, "name": c }))
            .collect()
    };
    (
        StatusCode::OK,
        axum::Json(json!([
            { "model_id": "eleven_flash_v2_5", "languages": langs(&["en", "fr", "fil"]) },
            { "model_id": "eleven_v3", "languages": langs(&["en", "fr", "sw", "fil"]) },
        ])),
    )
}

async fn voices() -> axum::Json<Value> {
    axum::Json(json!({ "voices": [
        { "voice_id": "zz-custom", "name": "Mine", "category": "cloned", "labels": {} },
        { "voice_id": "v-aria", "name": "Aria - Clear and Bright", "category": "premade",
          "labels": { "accent": "american", "gender": "female" } },
        { "voice_id": "v-default", "name": "George", "category": "premade", "labels": {} },
    ]}))
}

async fn mock() -> String {
    let app = Router::new()
        .route("/v2/translate", post(deepl))
        .route("/v1/text-to-speech/{voice}/with-timestamps", post(tts))
        .route("/v1/text-to-speech/{voice}", post(tts_plain))
        .route("/v1/models", get(models))
        .route("/v1/voices", get(voices))
        .route(
            "/v1/user/subscription",
            get(|| async { (StatusCode::UNAUTHORIZED, "missing user_read permission") }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    url
}

#[tokio::test]
async fn deepl_translates_in_order_and_reports_quota() {
    let url = mock().await;
    let t = Translator::DeepL {
        http: reqwest::Client::new(),
        url: url.clone(),
        key: format!("{KEY}:fx"),
    };
    let de = langs::find("de").unwrap();
    let out = t
        .translate(&["one".into(), "two".into()], de)
        .await
        .unwrap();
    assert_eq!(out, ["[DE] one", "[DE] two"]);
    let pt = langs::find("pt").unwrap();
    assert_eq!(
        t.translate(&["x".into()], pt).await.unwrap(),
        ["[PT-BR] x"],
        "regional target code"
    );
    assert!(matches!(
        t.translate(&["QUOTA".into()], de).await,
        Err(ProviderError::Quota)
    ));

    let bad = Translator::DeepL {
        http: reqwest::Client::new(),
        url,
        key: "wrong".into(),
    };
    assert!(matches!(
        bad.translate(&["x".into()], de).await,
        Err(ProviderError::Unauthorized)
    ));
}

#[tokio::test]
async fn elevenlabs_synthesizes_with_word_timings() {
    let url = mock().await;
    let tts = ElevenLabs::new(
        url.clone(),
        KEY.into(),
        "eleven_flash_v2_5".into(),
        "eleven_v3".into(),
        "v-default".into(),
    );
    let fr = langs::find("fr").unwrap();
    let speech = tts
        .synthesize("Bonjour à tous", "v-aria", fr, FLASH)
        .await
        .unwrap();
    assert_eq!(&speech.audio[..], b"ID3:v-aria");
    assert_eq!(
        speech.words.iter().map(|w| w.0).collect::<Vec<_>>(),
        [0, 8, 10]
    );
    assert!((speech.duration - 0.7).abs() < 1e-4);

    assert!(matches!(
        tts.synthesize("QUOTA", "v-aria", fr, FLASH).await,
        Err(ProviderError::Quota)
    ));
    let bad = ElevenLabs::new(
        url,
        "wrong".into(),
        "eleven_flash_v2_5".into(),
        "eleven_v3".into(),
        "v-default".into(),
    );
    assert!(matches!(
        bad.synthesize("x", "v-aria", fr, FLASH).await,
        Err(ProviderError::Unauthorized)
    ));
}

#[tokio::test]
async fn elevenlabs_lists_default_then_premade_voices() {
    let url = mock().await;
    let tts = ElevenLabs::new(
        url,
        KEY.into(),
        "eleven_flash_v2_5".into(),
        "eleven_v3".into(),
        "v-default".into(),
    );
    let voices = tts.voices().await;
    let ids: Vec<_> = voices.iter().map(|v| v.id.as_str()).collect();
    assert_eq!(ids, ["v-default", "v-aria", "zz-custom"]);
    assert_eq!(voices[1].name, "Aria");
    assert_eq!(voices[1].description.as_deref(), Some("american, female"));
    // A key without `user_read` still works; budgets just can't see the account.
    assert!(tts.subscription().await.is_none());
}

#[tokio::test]
async fn each_language_gets_the_cheapest_model_that_speaks_it() {
    let url = mock().await;
    let tts = client(&url, KEY);
    let model = |code: &str| {
        let tts = &tts;
        let lang = langs::find(code).unwrap();
        async move { tts.model_for(lang).await }
    };
    assert_eq!(model("fr").await.as_deref(), Some(FLASH));
    assert_eq!(
        model("tl").await.as_deref(),
        Some(FLASH),
        "Filipino is listed as fil"
    );
    assert_eq!(model("sw").await.as_deref(), Some("eleven_v3"));
    assert_eq!(
        model("mt").await,
        None,
        "no ElevenLabs voice: the browser reads it"
    );

    // Without the account's model list, the published lists decide.
    let tts = client(&url, "no-models-permission");
    let lang = |c| langs::find(c).unwrap();
    assert_eq!(tts.model_for(lang("hu")).await.as_deref(), Some(FLASH));
    assert_eq!(
        tts.model_for(lang("sw")).await.as_deref(),
        Some("eleven_v3")
    );
    assert_eq!(tts.model_for(lang("mt")).await, None);
}

#[tokio::test]
async fn models_without_timings_fall_back_to_plain_audio() {
    let url = mock().await;
    let tts = client(&url, KEY);
    let sw = langs::find("sw").unwrap();
    let speech = tts
        .synthesize("Habari za leo", "v-aria", sw, "eleven_v3")
        .await
        .unwrap();
    assert_eq!(&speech.audio[..], b"MP3:v-aria:eleven_v3:sw");
    assert!(
        speech.words.is_empty(),
        "no timings: the transcript doesn't highlight"
    );
    assert_eq!(
        speech.duration, 0.0,
        "the player reads the length from the audio"
    );
}

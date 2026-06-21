//! Client for the local Kokoro TTS sidecar (a small FastAPI service that wraps
//! the Kokoro Python pipeline). The Rust backend POSTs a transcript + voice
//! config and receives an MP3 path back.
use crate::error::{AppError, AppResult};
use crate::model::VoiceConfig;
use serde::{Deserialize, Serialize};
use std::time::Duration;

pub const SIDECAR_BASE: &str = "http://127.0.0.1:8765";

#[derive(Serialize)]
struct TtsReq<'a> {
    text_path: &'a str,
    out_path: &'a str,
    voice: &'a str,
    lang: &'a str,
    speed: f32,
}

#[derive(Deserialize)]
struct TtsResp {
    out_path: String,
    audio_seconds: f64,
}

/// Poll the sidecar /health until it responds or the timeout elapses.
pub async fn wait_until_ready(timeout: Duration) -> AppResult<()> {
    let client = reqwest::Client::new();
    let url = format!("{SIDECAR_BASE}/health");
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Ok(resp) = client.get(&url).timeout(Duration::from_secs(2)).send().await {
            if resp.status().is_success() {
                return Ok(());
            }
        }
        if std::time::Instant::now() >= deadline {
            return Err(AppError::Sidecar(
                "Kokoro sidecar did not become ready in time".into(),
            ));
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
}

/// Synthesize one chapter transcript file to an MP3. Returns audio length (s).
pub async fn synthesize(
    text_path: &str,
    out_path: &str,
    voice: &VoiceConfig,
) -> AppResult<f64> {
    let client = reqwest::Client::new();
    let url = format!("{SIDECAR_BASE}/tts");
    let req = TtsReq {
        text_path,
        out_path,
        voice: &voice.voice,
        lang: &voice.lang,
        speed: voice.speed,
    };
    let resp = client
        .post(&url)
        .json(&req)
        // A full chapter can take minutes; allow a generous ceiling.
        .timeout(Duration::from_secs(60 * 90))
        .send()
        .await?;
    if !resp.status().is_success() {
        let txt = resp.text().await.unwrap_or_default();
        return Err(AppError::Sidecar(format!("/tts failed: {txt}")));
    }
    let parsed: TtsResp = resp.json().await?;
    let _ = parsed.out_path;
    Ok(parsed.audio_seconds)
}

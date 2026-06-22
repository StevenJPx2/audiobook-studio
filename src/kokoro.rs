//! Native Kokoro TTS: turn a chapter transcript into an MP3, entirely in-process.
//!
//! Replaces the old FastAPI sidecar client. The flow is:
//!   transcript text
//!     -> g2p::phonemize          (misaki G2P sidecar -> phoneme chunks)
//!     -> tts::generate           (MLX Kokoro inference -> f32 @ 24kHz)
//!     -> assemble + silence gaps (cadence parity with the old pipeline)
//!     -> WAV -> ffmpeg           (encode to MP3)
//!
//! `synthesize` keeps its original signature so `pipeline.rs` is unchanged.

use crate::error::{AppError, AppResult};
use crate::model::VoiceConfig;
use crate::tts;
use std::process::Command;
use std::time::Duration;

const SR: u32 = tts::SR; // 24000
const PARA_GAP: f32 = 0.40; // silence between paragraphs (s)
const HEAD_GAP: f32 = 0.80; // silence after the title block / heading (s)
const MP3_BITRATE: &str = "128k";

/// Ensure the G2P sidecar is up and warm the TTS model. Replaces the old HTTP
/// /health poll. (Kept name + async signature for pipeline.rs compatibility.)
pub async fn wait_until_ready(_timeout: Duration) -> AppResult<()> {
    let voice = VoiceConfig::default();
    tokio::task::spawn_blocking(move || -> AppResult<()> {
        crate::g2p::ensure_ready()?;
        // Best-effort model warm; ignore on non-macOS (returns Err there).
        let _ = tts::warm(tts::MODEL_REPO, &voice.voice);
        Ok(())
    })
    .await
    .map_err(|e| AppError::Other(e.to_string()))?
}

/// Synthesize one chapter transcript file to an MP3. Returns audio length (s).
pub async fn synthesize(text_path: &str, out_path: &str, voice: &VoiceConfig) -> AppResult<f64> {
    let text = std::fs::read_to_string(text_path)?;
    let out_path = out_path.to_string();
    let voice = voice.clone();

    // All of this is blocking (file IO, sidecar pipe, MLX, ffmpeg) — run off the
    // async runtime so we don't stall other tasks.
    tokio::task::spawn_blocking(move || synth_blocking(&text, &out_path, &voice))
        .await
        .map_err(|e| AppError::Other(e.to_string()))?
}

fn synth_blocking(text: &str, out_path: &str, voice: &VoiceConfig) -> AppResult<f64> {
    // Split into paragraphs (blank-line separated) for cadence, mirroring the
    // old sidecar. Each paragraph is phonemized into sentence chunks.
    let paragraphs: Vec<&str> = text
        .split("\n\n")
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .collect();
    if paragraphs.is_empty() {
        return Err(AppError::Tts("empty transcript".into()));
    }

    let sil_para = vec![0.0f32; (SR as f32 * PARA_GAP) as usize];
    let sil_head = vec![0.0f32; (SR as f32 * HEAD_GAP) as usize];

    let mut samples: Vec<f32> = Vec::new();
    for (pi, para) in paragraphs.iter().enumerate() {
        let chunks = crate::g2p::phonemize(para)?;
        for ph in &chunks {
            let audio = tts::generate(tts::MODEL_REPO, &voice.voice, ph, voice.speed)?;
            samples.extend_from_slice(&audio);
        }
        // Bigger gap after the first two blocks (title/heading), smaller between
        // body paragraphs — matches the previous pipeline's feel.
        let gap = if pi <= 1 { &sil_head } else { &sil_para };
        samples.extend_from_slice(gap);
    }

    let audio_seconds = samples.len() as f64 / SR as f64;

    // Write a temp WAV, then ffmpeg -> MP3 (we keep ffmpeg for encode + m4b).
    let tmp_wav = std::env::temp_dir().join(format!("abs_tts_{}.wav", std::process::id()));
    write_wav(&samples, &tmp_wav)?;

    let status = Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-i"])
        .arg(&tmp_wav)
        .args(["-b:a", MP3_BITRATE])
        .arg(out_path)
        .status()
        .map_err(|e| AppError::Ffmpeg(format!("spawn ffmpeg: {e}")))?;
    let _ = std::fs::remove_file(&tmp_wav);
    if !status.success() {
        return Err(AppError::Ffmpeg(format!("ffmpeg exited with {status}")));
    }

    Ok(audio_seconds)
}

/// Write mono f32 samples to a 24 kHz WAV.
fn write_wav(samples: &[f32], path: &std::path::Path) -> AppResult<()> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: SR,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut w = hound::WavWriter::create(path, spec)
        .map_err(|e| AppError::Io(std::io::Error::other(e)))?;
    for &s in samples {
        w.write_sample(s)
            .map_err(|e| AppError::Io(std::io::Error::other(e)))?;
    }
    w.finalize()
        .map_err(|e| AppError::Io(std::io::Error::other(e)))?;
    Ok(())
}

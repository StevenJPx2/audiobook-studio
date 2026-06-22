//! Native Kokoro TTS inference via MLX (`voice-tts`), Apple-Silicon only.
//!
//! Loads the Kokoro-82M model + the configured voice once (cached for the
//! process lifetime — model load is the slow part) and synthesizes a phoneme
//! string into 24 kHz mono f32 samples. Grapheme→phoneme conversion is done
//! upstream by the misaki G2P sidecar (see `g2p.rs`); this module only runs the
//! model on already-computed phonemes.
//!
//! On non-macOS targets every entry point returns an error so the rest of the
//! crate still compiles (TTS is macOS-only by design — MLX requirement).

use crate::error::{AppError, AppResult};

/// Kokoro output sample rate.
pub const SR: u32 = 24000;

/// Hard token-sequence limit of the Kokoro model (voice-tts panics above this).
pub const MAX_MODEL_TOKENS: usize = 512;

/// Default HuggingFace repo for the MLX Kokoro weights.
pub const MODEL_REPO: &str = "prince-canuma/Kokoro-82M";

#[cfg(target_os = "macos")]
mod imp {
    use super::*;
    use std::sync::Mutex;
    use voice_tts::{generate as vt_generate, load_model, load_voice, Array, KokoroModel};

    // The model holds MLX state and is not Sync; guard it behind a Mutex and
    // initialize lazily on first use. We key the cache on (repo, voice) so a
    // voice change reloads.
    struct Loaded {
        repo: String,
        voice_name: String,
        model: KokoroModel,
        voice: Array,
    }

    static LOADED: Mutex<Option<Loaded>> = Mutex::new(None);

    fn ensure_loaded(repo: &str, voice_name: &str) -> AppResult<()> {
        let mut guard = LOADED.lock().map_err(|_| AppError::Tts("tts lock poisoned".into()))?;
        let reload = match guard.as_ref() {
            Some(l) => l.repo != repo || l.voice_name != voice_name,
            None => true,
        };
        if reload {
            let model = load_model(repo).map_err(|e| AppError::Tts(format!("load_model({repo}): {e}")))?;
            let voice = load_voice(voice_name, None)
                .map_err(|e| AppError::Tts(format!("load_voice({voice_name}): {e}")))?;
            *guard = Some(Loaded {
                repo: repo.to_string(),
                voice_name: voice_name.to_string(),
                model,
                voice,
            });
        }
        Ok(())
    }

    /// Synthesize one phoneme chunk to 24 kHz mono f32 samples.
    pub fn generate(repo: &str, voice_name: &str, phonemes: &str, speed: f32) -> AppResult<Vec<f32>> {
        if phonemes.trim().is_empty() {
            return Ok(Vec::new());
        }
        // voice-tts PANICS (not Err) if the token count exceeds the model's hard
        // limit. Callers should pre-chunk (see g2p::cap_token_len), but guard
        // here too so a stray long chunk degrades gracefully instead of crashing
        // the worker thread. Truncate on a space boundary as a last resort.
        const HARD_CAP: usize = super::MAX_MODEL_TOKENS - 2; // room for start/end
        let phonemes = if phonemes.chars().count() > HARD_CAP {
            let truncated: String = phonemes.chars().take(HARD_CAP).collect();
            match truncated.rfind(' ') {
                Some(i) => truncated[..i].to_string(),
                None => truncated,
            }
        } else {
            phonemes.to_string()
        };
        let phonemes = phonemes.as_str();
        ensure_loaded(repo, voice_name)?;
        let mut guard = LOADED.lock().map_err(|_| AppError::Tts("tts lock poisoned".into()))?;
        let loaded = guard.as_mut().ok_or_else(|| AppError::Tts("model not loaded".into()))?;
        let audio = vt_generate(&mut loaded.model, phonemes, &loaded.voice, speed)
            .map_err(|e| AppError::Tts(format!("generate: {e}")))?;
        audio.eval().map_err(|e| AppError::Tts(format!("eval: {e}")))?;
        Ok(audio.as_slice().to_vec())
    }

    /// Warm the model so the first real synth isn't slow. Best-effort.
    pub fn warm(repo: &str, voice_name: &str) -> AppResult<()> {
        ensure_loaded(repo, voice_name)
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::*;
    pub fn generate(_repo: &str, _voice: &str, _phonemes: &str, _speed: f32) -> AppResult<Vec<f32>> {
        Err(AppError::Tts(
            "native MLX TTS is only available on macOS (Apple Silicon)".into(),
        ))
    }
    pub fn warm(_repo: &str, _voice: &str) -> AppResult<()> {
        Err(AppError::Tts(
            "native MLX TTS is only available on macOS (Apple Silicon)".into(),
        ))
    }
}

/// Synthesize one phoneme chunk to 24 kHz mono f32 samples.
pub fn generate(repo: &str, voice_name: &str, phonemes: &str, speed: f32) -> AppResult<Vec<f32>> {
    imp::generate(repo, voice_name, phonemes, speed)
}

/// Preload the model + voice (model load is the slow part). Best-effort.
/// On a fresh machine this triggers the one-time HuggingFace download of the
/// MLX weights (~312 MB), so callers should run it off the UI thread.
pub fn warm(repo: &str, voice_name: &str) -> AppResult<()> {
    imp::warm(repo, voice_name)
}

/// Is the MLX model for `repo` already cached locally (no download needed)?
/// Used by the GUI to decide whether to show a first-run "downloading model"
/// setup screen. Honors `HF_HOME` (set by `bundle_env` in a packaged .app);
/// falls back to the default `~/.cache/huggingface` HF cache layout.
pub fn model_present(repo: &str) -> bool {
    // hf-hub cache dir: <hf_home>/hub/models--<org>--<name>/snapshots/<rev>/...
    let hub = hf_hub_dir();
    let dirname = format!("models--{}", repo.replace('/', "--"));
    let snapshots = hub.join(&dirname).join("snapshots");
    // Present if any snapshot dir exists with at least one entry.
    std::fs::read_dir(&snapshots)
        .ok()
        .map(|mut it| it.any(|e| e.is_ok()))
        .unwrap_or(false)
}

fn hf_hub_dir() -> std::path::PathBuf {
    if let Some(h) = std::env::var_os("HF_HOME") {
        return std::path::PathBuf::from(h).join("hub");
    }
    if let Some(h) = std::env::var_os("HF_HUB_CACHE") {
        return std::path::PathBuf::from(h);
    }
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_default();
    home.join(".cache/huggingface/hub")
}

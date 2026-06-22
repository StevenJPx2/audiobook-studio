//! Runtime environment setup for the packaged `.app`.
//!
//! When the binary runs from inside `Audiobook Studio.app`, point the
//! HuggingFace cache at the model bundled in `Contents/Resources/hf-cache` and
//! force offline mode, so MLX Kokoro loads the embedded weights without ever
//! touching the network. A no-op in development (no bundle detected, or the
//! vars already set by the caller).

use std::path::PathBuf;

/// Detect the `.app` bundle and wire up offline model loading. Call once at
/// process start, before any TTS load. Safe to call from both binaries.
pub fn init() {
    let Some(resources) = bundle_resources_dir() else {
        return; // dev / not bundled
    };
    let hf_cache = resources.join("hf-cache");
    if hf_cache.is_dir() {
        // SAFETY: called once at startup, before threads touch these vars.
        unsafe {
            if std::env::var_os("HF_HOME").is_none() {
                std::env::set_var("HF_HOME", &hf_cache);
            }
            if std::env::var_os("HF_HUB_OFFLINE").is_none() {
                std::env::set_var("HF_HUB_OFFLINE", "1");
            }
        }
    }
}

/// `…/Audiobook Studio.app/Contents/Resources` when running inside a bundle.
/// The executable lives in `Contents/MacOS/`, so Resources is a sibling.
fn bundle_resources_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let macos_dir = exe.parent()?; // Contents/MacOS
    if macos_dir.file_name()?.to_str()? != "MacOS" {
        return None;
    }
    let contents = macos_dir.parent()?; // Contents
    if contents.file_name()?.to_str()? != "Contents" {
        return None;
    }
    let resources = contents.join("Resources");
    resources.is_dir().then_some(resources)
}

//! Shared data structures passed between the pipeline stages and the frontend.
use serde::{Deserialize, Serialize};

/// A detected chapter boundary. Pages are 1-indexed and inclusive.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chapter {
    pub order: usize,
    pub title: String,
    pub start_page: usize,
    pub end_page: usize,
}

/// What the LLM returns for a single boundary (page where a chapter starts).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Boundary {
    pub title: String,
    pub start_page: usize,
}

/// Quick facts about a dropped PDF, shown in the UI before processing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BookInfo {
    pub path: String,
    pub file_name: String,
    pub page_count: usize,
    pub size_mb: f64,
    /// Embedded PDF outline (table of contents), if any.
    pub outline: Vec<OutlineItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutlineItem {
    pub level: usize,
    pub title: String,
    pub page: usize,
}

/// Voice options exposed to the UI; mirrors the Kokoro sidecar config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceConfig {
    pub voice: String,
    pub lang: String,
    pub speed: f32,
}

impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            voice: "bm_george".into(),
            lang: "b".into(),
            speed: 1.0,
        }
    }
}

/// Full job request from the frontend "Generate" action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerateRequest {
    pub pdf_path: String,
    pub out_dir: String,
    pub chapters: Vec<Chapter>,
    pub voice: VoiceConfig,
    pub book_title: String,
    pub author: String,
}

/// Progress event payload emitted to the frontend over a Tauri channel/event.
#[derive(Debug, Clone, Serialize)]
pub struct Progress {
    pub stage: String,   // "extract" | "boundaries" | "split" | "tts" | "bundle" | "done"
    pub message: String,
    pub current: u32,
    pub total: u32,
    pub pct: f32,
}

impl Progress {
    pub fn new(stage: &str, message: impl Into<String>, current: u32, total: u32) -> Self {
        let pct = if total > 0 {
            (current as f32 / total as f32) * 100.0
        } else {
            0.0
        };
        Self {
            stage: stage.into(),
            message: message.into(),
            current,
            total,
            pct,
        }
    }
}

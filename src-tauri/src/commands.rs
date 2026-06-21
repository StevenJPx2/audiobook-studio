//! Tauri commands exposed to the frontend, plus the end-to-end pipeline that
//! streams `audiobook://progress` events as it runs.
use crate::agent;
use crate::bundle;
use crate::error::{AppError, AppResult};
use crate::kokoro;
use crate::model::{
    BookInfo, Boundary, Chapter, GenerateRequest, Progress, VoiceConfig,
};
use crate::pdf;
use crate::split;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tauri::{Emitter, Window};

fn emit(window: &Window, p: Progress) {
    let _ = window.emit("audiobook://progress", p);
}

/// Inspect a dropped PDF: page count, size, embedded outline.
#[tauri::command]
pub async fn inspect_pdf(path: String) -> AppResult<BookInfo> {
    tokio::task::spawn_blocking(move || pdf::info(&path))
        .await
        .map_err(|e| AppError::Other(e.to_string()))?
}

/// List locally available Ollama models (tags) for the model picker.
#[tauri::command]
pub async fn list_models() -> AppResult<Vec<String>> {
    let base = std::env::var("OLLAMA_HOST")
        .unwrap_or_else(|_| "http://localhost:11434".to_string());
    let url = format!("{}/api/tags", base.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let resp = client
        .get(&url)
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .map_err(|e| AppError::Llm(format!("Ollama not reachable: {e}")))?;
    let json: serde_json::Value = resp.json().await.map_err(|e| AppError::Llm(e.to_string()))?;
    let models = json["models"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m["name"].as_str().map(String::from))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(models)
}

/// Detect chapter boundaries with the local LLM. Returns chapters the user can
/// review/edit before generating audio.
#[tauri::command]
pub async fn detect_chapters(
    window: Window,
    path: String,
    model: String,
) -> AppResult<Vec<Chapter>> {
    emit(&window, Progress::new("extract", "Reading PDF…", 0, 1));
    let path2 = path.clone();
    let pages = tokio::task::spawn_blocking(move || pdf::pages(&path2))
        .await
        .map_err(|e| AppError::Other(e.to_string()))??;
    let page_count = pages.len();
    emit(
        &window,
        Progress::new("extract", format!("Extracted {page_count} pages"), 1, 1),
    );

    // Outline if present, else heuristic page openings.
    let outline = pdf::read_outline(&path).unwrap_or_default();
    let page_heads: Vec<(usize, String)> = pages
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let head = p
                .lines()
                .map(|l| l.trim())
                .find(|l| !l.is_empty())
                .unwrap_or("")
                .chars()
                .take(80)
                .collect::<String>();
            (i + 1, head)
        })
        .collect();

    emit(
        &window,
        Progress::new("boundaries", format!("Asking {model} to find chapters…"), 0, 1),
    );
    let candidates = agent::build_candidates(&outline, &page_heads);
    let boundaries = agent::detect_boundaries(&model, &candidates).await?;

    let boundaries = if boundaries.is_empty() {
        // Last-resort fallback: treat the whole book as one chapter.
        vec![Boundary {
            title: "Full Book".to_string(),
            start_page: 1,
        }]
    } else {
        boundaries
    };

    let chapters = split::boundaries_to_chapters(&boundaries, page_count);
    emit(
        &window,
        Progress::new("boundaries", format!("Found {} chapters", chapters.len()), 1, 1),
    );
    Ok(chapters)
}

/// Full generation: transcripts -> Kokoro TTS (per chapter) -> .m4b bundle.
#[tauri::command]
pub async fn generate_audiobook(
    window: Window,
    req: GenerateRequest,
) -> AppResult<String> {
    let GenerateRequest {
        pdf_path,
        out_dir,
        chapters,
        voice,
        book_title,
        author,
    } = req;

    std::fs::create_dir_all(&out_dir)?;

    // 1) Extract pages + write transcripts.
    emit(&window, Progress::new("split", "Building transcripts…", 0, chapters.len() as u32));
    let pdf_path2 = pdf_path.clone();
    let pages = tokio::task::spawn_blocking(move || pdf::pages(&pdf_path2))
        .await
        .map_err(|e| AppError::Other(e.to_string()))??;

    let transcripts = split::write_transcripts(
        &pages, &chapters, &out_dir, &book_title, &author,
    )?;
    emit(
        &window,
        Progress::new("split", "Transcripts ready", chapters.len() as u32, chapters.len() as u32),
    );

    // 2) Ensure the Kokoro sidecar is up.
    emit(&window, Progress::new("tts", "Waiting for Kokoro sidecar…", 0, 1));
    kokoro::wait_until_ready(Duration::from_secs(120)).await?;

    // 3) Synthesize each chapter.
    let total = transcripts.len() as u32;
    let mut mp3s: Vec<PathBuf> = Vec::with_capacity(transcripts.len());
    for (i, (ch, txt_path)) in transcripts.iter().enumerate() {
        let mp3 = Path::new(&out_dir).join(format!("{}.mp3", split::file_stem(ch)));
        emit(
            &window,
            Progress::new(
                "tts",
                format!("Narrating: {}", ch.title),
                i as u32,
                total,
            ),
        );
        // Resumable: skip chapters already rendered.
        if !(mp3.exists() && std::fs::metadata(&mp3).map(|m| m.len() > 1000).unwrap_or(false)) {
            kokoro::synthesize(
                &txt_path.to_string_lossy(),
                &mp3.to_string_lossy(),
                &voice,
            )
            .await?;
        }
        mp3s.push(mp3);
    }
    emit(&window, Progress::new("tts", "All chapters narrated", total, total));

    // 3.5) Cover art: render PDF page 1 unless the user already dropped a cover.
    let has_cover = ["cover.jpg", "cover.jpeg", "cover.png"]
        .iter()
        .any(|n| Path::new(&out_dir).join(n).exists());
    if !has_cover {
        emit(&window, Progress::new("bundle", "Rendering cover…", 0, 1));
        let cover_path = Path::new(&out_dir).join("cover.jpg");
        match kokoro::generate_cover(&pdf_path, &cover_path.to_string_lossy(), 1).await {
            Ok(()) => {}
            Err(e) => eprintln!("[cover] skipped ({e})"), // non-fatal
        }
    }

    // 4) Bundle into .m4b.
    emit(&window, Progress::new("bundle", "Bundling .m4b…", 0, 1));
    let safe_title = book_title.replace('/', "-");
    let out_file = Path::new(&out_dir).join(format!("{safe_title} (Audiobook).m4b"));
    let chapters_for_bundle = chapters.clone();
    let out_dir_b = out_dir.clone();
    let title_b = book_title.clone();
    let author_b = author.clone();
    let out_file_b = out_file.clone();
    tokio::task::spawn_blocking(move || {
        bundle::build_m4b(
            &mp3s,
            &chapters_for_bundle,
            &out_dir_b,
            &out_file_b,
            &title_b,
            &author_b,
            "64k",
        )
    })
    .await
    .map_err(|e| AppError::Other(e.to_string()))??;

    emit(&window, Progress::new("done", "Audiobook ready", 1, 1));
    Ok(out_file.to_string_lossy().to_string())
}

/// Reveal a finished file in the OS file manager.
#[tauri::command]
pub async fn reveal(path: String) -> AppResult<()> {
    crate::reveal_in_os(&path)
}

/// Default voice config for the UI.
#[tauri::command]
pub fn default_voice() -> VoiceConfig {
    VoiceConfig::default()
}

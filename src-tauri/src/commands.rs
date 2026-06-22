//! Tauri commands exposed to the frontend, plus the end-to-end pipeline that
//! streams `audiobook://progress` events as it runs.
use crate::agent;
use crate::bundle;
use crate::error::{AppError, AppResult};
use crate::kokoro;
use crate::model::{BookInfo, Boundary, Chapter, GenerateRequest, Progress, VoiceConfig};
use crate::pdf;
use crate::split;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tauri::{Emitter, Window};

fn emit(window: &Window, p: Progress) {
    let _ = window.emit("audiobook://progress", p);
}

/// Quick check that the local Ollama server is up, so the (default-on) polish
/// pass can be skipped cleanly when it isn't. Short timeout; never errors.
async fn ollama_reachable() -> bool {
    let base =
        std::env::var("OLLAMA_HOST").unwrap_or_else(|_| "http://localhost:11434".to_string());
    let url = format!("{}/api/tags", base.trim_end_matches('/'));
    reqwest::Client::new()
        .get(&url)
        .timeout(Duration::from_secs(3))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
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
    let base =
        std::env::var("OLLAMA_HOST").unwrap_or_else(|_| "http://localhost:11434".to_string());
    let url = format!("{}/api/tags", base.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let resp = client
        .get(&url)
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .map_err(|e| AppError::Llm(format!("Ollama not reachable: {e}")))?;
    let json: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| AppError::Llm(e.to_string()))?;
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

    // Candidate signals, strongest first: embedded outline, deterministically
    // detected heading lines, and (only as a last resort) page openings.
    let outline = pdf::read_outline(&path).unwrap_or_default();
    // Deterministic heading scan, with table-of-contents duplicates removed so
    // both the LLM and the fallback see clean candidates (one entry per title).
    let headings = pdf::dedupe_headings(&pdf::chapter_headings(&pages));
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
        Progress::new(
            "boundaries",
            format!("Asking {model} to find chapters…"),
            0,
            1,
        ),
    );
    let candidates = agent::build_candidates(&outline, &headings, &page_heads);
    let boundaries = agent::detect_boundaries(&model, &candidates).await?;

    let boundaries = if !boundaries.is_empty() {
        boundaries
    } else if !headings.is_empty() {
        // LLM gave nothing usable: fall back to the (already TOC-deduped)
        // deterministic headings directly, so a model hiccup never collapses a
        // structured book into one giant chapter.
        eprintln!(
            "[boundaries] LLM empty; using {} detected headings",
            headings.len()
        );
        headings
            .iter()
            .map(|(pg, title)| Boundary {
                title: title.trim().to_string(),
                start_page: *pg,
            })
            .collect()
    } else {
        // No signal at all: treat the whole book as one chapter.
        vec![Boundary {
            title: "Full Book".to_string(),
            start_page: 1,
        }]
    };

    let chapters = split::boundaries_to_chapters(&boundaries, page_count);
    emit(
        &window,
        Progress::new(
            "boundaries",
            format!("Found {} chapters", chapters.len()),
            1,
            1,
        ),
    );
    Ok(chapters)
}

/// Full generation: transcripts -> Kokoro TTS (per chapter) -> .m4b bundle.
#[tauri::command]
pub async fn generate_audiobook(window: Window, req: GenerateRequest) -> AppResult<String> {
    let GenerateRequest {
        pdf_path,
        out_dir,
        chapters,
        voice,
        book_title,
        author,
        polish,
        polish_model,
    } = req;

    std::fs::create_dir_all(&out_dir)?;

    // 1) Extract pages + write transcripts.
    emit(
        &window,
        Progress::new("split", "Building transcripts…", 0, chapters.len() as u32),
    );
    let pdf_path2 = pdf_path.clone();
    let pages = tokio::task::spawn_blocking(move || pdf::pages(&pdf_path2))
        .await
        .map_err(|e| AppError::Other(e.to_string()))??;

    let transcripts = split::write_transcripts(&pages, &chapters, &out_dir, &book_title, &author)?;
    emit(
        &window,
        Progress::new(
            "split",
            "Transcripts ready",
            chapters.len() as u32,
            chapters.len() as u32,
        ),
    );

    // 1.5) LLM polish pass over each transcript (opt-out; default on). Skipped
    // entirely when Ollama is unreachable so the deterministic transcript is
    // used without wasted time or confusing progress. Non-fatal and per-chapter:
    // a failed or low-confidence polish keeps the algorithmic transcript, so
    // generation always proceeds.
    if polish && ollama_reachable().await {
        let model = polish_model
            .clone()
            .unwrap_or_else(|| "gemma4:e2b".to_string());
        let total = transcripts.len() as u32;
        for (i, (ch, txt_path)) in transcripts.iter().enumerate() {
            emit(
                &window,
                Progress::new(
                    "polish",
                    format!("Polishing transcript: {}", ch.title),
                    i as u32,
                    total,
                ),
            );
            if let Err(e) = polish_transcript_file(txt_path, &model).await {
                eprintln!("[polish] {} skipped ({e})", ch.title); // non-fatal
            }
        }
        emit(
            &window,
            Progress::new("polish", "Transcripts polished", total, total),
        );
    }

    // 2) Ensure the Kokoro sidecar is up.
    emit(
        &window,
        Progress::new("tts", "Waiting for Kokoro sidecar…", 0, 1),
    );
    kokoro::wait_until_ready(Duration::from_secs(120)).await?;

    // 3) Synthesize each chapter.
    let total = transcripts.len() as u32;
    let mut mp3s: Vec<PathBuf> = Vec::with_capacity(transcripts.len());
    for (i, (ch, txt_path)) in transcripts.iter().enumerate() {
        let mp3 = Path::new(&out_dir).join(format!("{}.mp3", split::file_stem(ch)));
        emit(
            &window,
            Progress::new("tts", format!("Narrating: {}", ch.title), i as u32, total),
        );
        // Resumable: skip chapters already rendered.
        if !(mp3.exists()
            && std::fs::metadata(&mp3)
                .map(|m| m.len() > 1000)
                .unwrap_or(false))
        {
            kokoro::synthesize(&txt_path.to_string_lossy(), &mp3.to_string_lossy(), &voice).await?;
        }
        mp3s.push(mp3);
    }
    emit(
        &window,
        Progress::new("tts", "All chapters narrated", total, total),
    );

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

/// Polish one transcript file in place. The file layout written by
/// `split::write_transcripts` is:
///   "<book_title>\n<author>\n\n<chapter_title>\n\n<body...>\n"
/// We keep that 4-line header verbatim and only run the LLM polish over the
/// body, then rewrite the file. On any error the original file is untouched.
async fn polish_transcript_file(path: &Path, model: &str) -> AppResult<()> {
    let full = std::fs::read_to_string(path)?;
    // Header = first three logical paragraphs joined by blank lines: title,
    // author, chapter title. Body is everything after the third blank-line gap.
    let mut parts = full.splitn(3, "\n\n");
    let p0 = parts.next().unwrap_or("");
    let p1 = parts.next().unwrap_or("");
    let body = parts.next().unwrap_or("");
    if body.trim().is_empty() {
        return Ok(()); // nothing to polish
    }
    let cleaned = agent::polish_transcript(model, body).await?;
    let rebuilt = format!("{p0}\n\n{p1}\n\n{}\n", cleaned.trim());
    std::fs::write(path, rebuilt)?;
    Ok(())
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

//! The end-to-end audiobook pipeline, decoupled from any GUI framework.
//!
//! Each long-running step reports progress through a `Progress` callback so the
//! egui layer can render it. Everything here is plain async Rust; the UI runs it
//! on a background thread with its own Tokio runtime.
use crate::agent;
use crate::bundle;
use crate::error::{AppError, AppResult};
use crate::kokoro;
use crate::model::{BookInfo, Boundary, Chapter, GenerateRequest, Progress};
use crate::pdf;
use crate::split;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A progress sink. The GUI passes a closure that forwards to its channel.
pub type ProgressFn<'a> = dyn Fn(Progress) + Send + Sync + 'a;

/// Inspect a PDF: page count, size, embedded outline. Blocking (call off-thread).
pub fn inspect_pdf(path: &str) -> AppResult<BookInfo> {
    pdf::info(path)
}

/// List locally available Ollama model tags for the picker.
pub async fn list_models() -> AppResult<Vec<String>> {
    let base =
        std::env::var("OLLAMA_HOST").unwrap_or_else(|_| "http://localhost:11434".to_string());
    let url = format!("{}/api/tags", base.trim_end_matches('/'));
    let resp = reqwest::Client::new()
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

/// Detect chapter boundaries with the local LLM, returning chapters the user can
/// review/edit before generating audio.
pub async fn detect_chapters(
    path: &str,
    model: &str,
    progress: &ProgressFn<'_>,
) -> AppResult<Vec<Chapter>> {
    progress(Progress::new("extract", "Reading PDF…", 0, 1));
    let path_owned = path.to_string();
    let pages = tokio::task::spawn_blocking(move || pdf::pages(&path_owned))
        .await
        .map_err(|e| AppError::Other(e.to_string()))??;
    let page_count = pages.len();
    progress(Progress::new(
        "extract",
        format!("Extracted {page_count} pages"),
        1,
        1,
    ));

    // Candidate signals, strongest first: embedded outline, deterministically
    // detected (and TOC-deduped) headings, and page openings as a last resort.
    let outline = pdf::read_outline(path).unwrap_or_default();
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

    progress(Progress::new(
        "boundaries",
        format!("Asking {model} to find chapters…"),
        0,
        1,
    ));
    let candidates = agent::build_candidates(&outline, &headings, &page_heads);
    let boundaries = agent::detect_boundaries(model, &candidates).await?;

    let boundaries = if !boundaries.is_empty() {
        boundaries
    } else if !headings.is_empty() {
        // LLM gave nothing usable: fall back to the deterministic headings so a
        // model hiccup never collapses a structured book into one giant chapter.
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
        vec![Boundary {
            title: "Full Book".to_string(),
            start_page: 1,
        }]
    };

    let chapters = split::boundaries_to_chapters(&boundaries, page_count);
    progress(Progress::new(
        "boundaries",
        format!("Found {} chapters", chapters.len()),
        1,
        1,
    ));
    Ok(chapters)
}

/// Full generation: transcripts -> (optional LLM polish) -> Kokoro TTS per
/// chapter -> cover -> .m4b bundle. Returns the output file path.
pub async fn generate_audiobook(
    req: GenerateRequest,
    progress: &ProgressFn<'_>,
) -> AppResult<String> {
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
    progress(Progress::new(
        "split",
        "Building transcripts…",
        0,
        chapters.len() as u32,
    ));
    let pdf_path2 = pdf_path.clone();
    let pages = tokio::task::spawn_blocking(move || pdf::pages(&pdf_path2))
        .await
        .map_err(|e| AppError::Other(e.to_string()))??;

    let transcripts = split::write_transcripts(&pages, &chapters, &out_dir, &book_title, &author)?;
    progress(Progress::new(
        "split",
        "Transcripts ready",
        chapters.len() as u32,
        chapters.len() as u32,
    ));

    // 1.5) LLM polish pass over each transcript (opt-out; default on). Skipped
    // when Ollama is unreachable. Non-fatal and per-chapter: a failed or
    // low-confidence polish keeps the algorithmic transcript.
    if polish && ollama_reachable().await {
        let model = polish_model
            .clone()
            .unwrap_or_else(|| "gemma4:e2b".to_string());
        let total = transcripts.len() as u32;
        for (i, (ch, txt_path)) in transcripts.iter().enumerate() {
            progress(Progress::new(
                "polish",
                format!("Polishing transcript: {}", ch.title),
                i as u32,
                total,
            ));
            if let Err(e) = polish_transcript_file(txt_path, &model).await {
                eprintln!("[polish] {} skipped ({e})", ch.title); // non-fatal
            }
        }
        progress(Progress::new(
            "polish",
            "Transcripts polished",
            total,
            total,
        ));
    }

    // 2) Ensure the Kokoro sidecar is up.
    progress(Progress::new("tts", "Waiting for Kokoro sidecar…", 0, 1));
    kokoro::wait_until_ready(Duration::from_secs(120)).await?;

    // 3) Synthesize each chapter (resumable: skip already-rendered MP3s).
    let total = transcripts.len() as u32;
    let mut mp3s: Vec<PathBuf> = Vec::with_capacity(transcripts.len());
    for (i, (ch, txt_path)) in transcripts.iter().enumerate() {
        let mp3 = Path::new(&out_dir).join(format!("{}.mp3", split::file_stem(ch)));
        progress(Progress::new(
            "tts",
            format!("Narrating: {}", ch.title),
            i as u32,
            total,
        ));
        if !(mp3.exists()
            && std::fs::metadata(&mp3)
                .map(|m| m.len() > 1000)
                .unwrap_or(false))
        {
            kokoro::synthesize(&txt_path.to_string_lossy(), &mp3.to_string_lossy(), &voice).await?;
        }
        mp3s.push(mp3);
    }
    progress(Progress::new("tts", "All chapters narrated", total, total));

    // 3.5) Cover art: render PDF page 1 unless a cover was already supplied.
    let has_cover = ["cover.jpg", "cover.jpeg", "cover.png"]
        .iter()
        .any(|n| Path::new(&out_dir).join(n).exists());
    if !has_cover {
        progress(Progress::new("bundle", "Rendering cover…", 0, 1));
        let cover_path = Path::new(&out_dir).join("cover.jpg");
        if let Err(e) = kokoro::generate_cover(&pdf_path, &cover_path.to_string_lossy(), 1).await {
            eprintln!("[cover] skipped ({e})"); // non-fatal
        }
    }

    // 4) Bundle into .m4b.
    progress(Progress::new("bundle", "Bundling .m4b…", 0, 1));
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

    progress(Progress::new("done", "Audiobook ready", 1, 1));
    Ok(out_file.to_string_lossy().to_string())
}

/// Polish one transcript file in place, preserving the
/// "<title>\n<author>\n\n<chapter>\n\n<body>" header and rewriting only the body.
async fn polish_transcript_file(path: &Path, model: &str) -> AppResult<()> {
    let full = std::fs::read_to_string(path)?;
    let mut parts = full.splitn(3, "\n\n");
    let p0 = parts.next().unwrap_or("");
    let p1 = parts.next().unwrap_or("");
    let body = parts.next().unwrap_or("");
    if body.trim().is_empty() {
        return Ok(());
    }
    let cleaned = agent::polish_transcript(model, body).await?;
    let rebuilt = format!("{p0}\n\n{p1}\n\n{}\n", cleaned.trim());
    std::fs::write(path, rebuilt)?;
    Ok(())
}

/// Open the OS file manager with `path` selected.
pub fn reveal_in_os(path: &str) -> AppResult<()> {
    #[cfg(target_os = "macos")]
    let res = std::process::Command::new("open")
        .arg("-R")
        .arg(path)
        .spawn();
    #[cfg(target_os = "windows")]
    let res = std::process::Command::new("explorer")
        .arg("/select,")
        .arg(path)
        .spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let res = std::process::Command::new("xdg-open")
        .arg(
            std::path::Path::new(path)
                .parent()
                .unwrap_or(std::path::Path::new(".")),
        )
        .spawn();
    res.map(|_| ()).map_err(|e| AppError::Other(e.to_string()))
}

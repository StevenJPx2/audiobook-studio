//! Chapter-boundary detection with a local LLM (Ollama) via the Rig agent
//! framework, with a direct-HTTP fallback so the app runs even if Rig's
//! provider surface shifts between versions.
//!
//! Input: candidate headings (from the PDF outline when available, otherwise a
//! heuristic scan of page openings). Output: an ordered list of chapter
//! boundaries `{ title, start_page }`.
use crate::error::{AppError, AppResult};
use crate::model::{Boundary, OutlineItem};
use rig_core::client::CompletionClient;
use rig_core::completion::Prompt;
use rig_core::providers::ollama;
use serde::Deserialize;

const SYSTEM: &str = "You are a meticulous book-structure analyst. \
Given a list of candidate headings with their page numbers from a book PDF, \
identify the real top-level sections a listener would want as audiobook chapters: \
front matter (Introduction/Preface), each numbered Chapter, and back matter \
(Conclusion/Epilogue). Ignore sub-headings, figure captions, running headers, \
and bibliography sections. Return STRICT JSON only — no prose, no markdown — \
matching: {\"chapters\":[{\"title\":\"...\",\"start_page\":N}, ...]}. \
Titles must be clean and human-readable. Order by start_page ascending.";

#[derive(Debug, Deserialize)]
struct LlmChapters {
    chapters: Vec<Boundary>,
}

/// Build the candidate-heading prompt body the model reasons over.
pub fn build_candidates(outline: &[OutlineItem], page_heads: &[(usize, String)]) -> String {
    if !outline.is_empty() {
        let mut s = String::from("PDF OUTLINE (level: title @ page):\n");
        for it in outline {
            s.push_str(&format!("{}: {} @ {}\n", it.level, it.title, it.page));
        }
        s
    } else {
        let mut s = String::from("PAGE OPENINGS (page: first line):\n");
        for (pg, head) in page_heads {
            s.push_str(&format!("{pg}: {head}\n"));
        }
        s
    }
}

/// Run boundary detection. `model` is an Ollama model tag (e.g. "gemma4:e2b").
pub async fn detect_boundaries(
    model: &str,
    candidates: &str,
) -> AppResult<Vec<Boundary>> {
    let prompt = format!(
        "{candidates}\n\nReturn the chapter boundaries as STRICT JSON now."
    );

    // Primary path: Rig agent over Ollama.
    match rig_detect(model, &prompt).await {
        Ok(b) if !b.is_empty() => return Ok(b),
        Ok(_) => { /* empty -> try fallback */ }
        Err(e) => {
            eprintln!("[agent] rig path failed ({e}); trying direct Ollama");
        }
    }

    // Fallback: direct Ollama /api/chat with JSON format.
    http_detect(model, &prompt).await
}

async fn rig_detect(model: &str, prompt: &str) -> AppResult<Vec<Boundary>> {
    let client = ollama::Client::new(rig_core::client::Nothing)
        .map_err(|e| AppError::Llm(format!("ollama client: {e}")))?;
    let agent = client.agent(model).preamble(SYSTEM).temperature(0.0).build();
    let raw = agent
        .prompt(prompt)
        .await
        .map_err(|e| AppError::Llm(e.to_string()))?;
    parse_boundaries(&raw)
}

#[derive(serde::Serialize)]
struct OllamaChatReq<'a> {
    model: &'a str,
    messages: Vec<OllamaMsg<'a>>,
    stream: bool,
    format: &'a str,
    options: OllamaOpts,
}
#[derive(serde::Serialize)]
struct OllamaMsg<'a> {
    role: &'a str,
    content: &'a str,
}
#[derive(serde::Serialize)]
struct OllamaOpts {
    temperature: f32,
}
#[derive(Deserialize)]
struct OllamaChatResp {
    message: OllamaRespMsg,
}
#[derive(Deserialize)]
struct OllamaRespMsg {
    content: String,
}

async fn http_detect(model: &str, prompt: &str) -> AppResult<Vec<Boundary>> {
    let base = std::env::var("OLLAMA_HOST")
        .unwrap_or_else(|_| "http://localhost:11434".to_string());
    let url = format!("{}/api/chat", base.trim_end_matches('/'));
    let body = OllamaChatReq {
        model,
        messages: vec![
            OllamaMsg { role: "system", content: SYSTEM },
            OllamaMsg { role: "user", content: prompt },
        ],
        stream: false,
        format: "json",
        options: OllamaOpts { temperature: 0.0 },
    };
    let client = reqwest::Client::new();
    let resp: OllamaChatResp = client
        .post(&url)
        .json(&body)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    parse_boundaries(&resp.message.content)
}

/// Pull the JSON object out of a possibly-chatty response and parse it.
fn parse_boundaries(raw: &str) -> AppResult<Vec<Boundary>> {
    let json = extract_json_object(raw)
        .ok_or_else(|| AppError::Llm(format!("no JSON object in LLM output: {raw:.200}")))?;
    let parsed: LlmChapters = serde_json::from_str(&json)
        .map_err(|e| AppError::Llm(format!("parse boundaries: {e}; raw={json:.200}")))?;
    let mut chapters = parsed.chapters;
    chapters.retain(|c| c.start_page > 0 && !c.title.trim().is_empty());
    chapters.sort_by_key(|c| c.start_page);
    chapters.dedup_by_key(|c| c.start_page);
    Ok(chapters)
}

/// Find the first balanced top-level `{ ... }` in a string.
fn extract_json_object(s: &str) -> Option<String> {
    let start = s.find('{')?;
    let bytes = s.as_bytes();
    let mut depth = 0i32;
    let mut in_str = false;
    let mut esc = false;
    for i in start..bytes.len() {
        let c = bytes[i] as char;
        if in_str {
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(s[start..=i].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

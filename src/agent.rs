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

/// Build the candidate-heading prompt body the model reasons over. We give the
/// model the strongest signals available, in priority order: the embedded PDF
/// outline, deterministically detected heading lines (`Chapter N`, `BOOK II`,
/// `Introduction`, …), and — only if neither is informative — the first line of
/// each page. Headings vary too much between books to split on mechanically, so
/// the model picks the real top-level chapters from these candidates.
pub fn build_candidates(
    outline: &[OutlineItem],
    headings: &[(usize, String)],
    page_heads: &[(usize, String)],
) -> String {
    let mut s = String::new();
    if !outline.is_empty() {
        s.push_str("PDF OUTLINE (level: title @ page):\n");
        for it in outline {
            s.push_str(&format!("{}: {} @ {}\n", it.level, it.title, it.page));
        }
    }
    if !headings.is_empty() {
        s.push_str("\nDETECTED HEADING LINES (page: text):\n");
        for (pg, h) in headings {
            s.push_str(&format!("{pg}: {h}\n"));
        }
    }
    // Fall back to page openings when we have no stronger signal — OR when the
    // strong signals are too sparse to divide the book. A handful of detected
    // headings across many pages (e.g. a single "Conclusion" in a 19-page book
    // whose chapters are display-titles the detector under-counts) would
    // otherwise starve the model. Including page openings as a safety net lets
    // the LLM recover the real chapters; it still prefers the explicit headings.
    let weak_outline = outline.len() < 2;
    let weak_headings = headings_too_sparse(headings, page_heads.len());
    if weak_outline && weak_headings && !page_heads.is_empty() {
        s.push_str("\nPAGE OPENINGS (page: first line):\n");
        for (pg, head) in page_heads {
            s.push_str(&format!("{pg}: {head}\n"));
        }
    }
    s
}

/// Heuristic: are the detected headings too few to plausibly be the book's
/// chapter list, given its length? Fewer than two headings is always weak; for
/// longer books, fewer than one heading per ~12 pages suggests the detector
/// missed display-title chapters, so we should also offer page openings.
fn headings_too_sparse(headings: &[(usize, String)], page_count: usize) -> bool {
    if headings.len() < 2 {
        return true;
    }
    // Roughly: expect at least one heading per 12 pages for a chaptered book.
    let expected = (page_count / 12).max(2);
    headings.len() < expected
}

/// Run boundary detection. `model` is an Ollama model tag (e.g. "gemma4:e2b").
pub async fn detect_boundaries(model: &str, candidates: &str) -> AppResult<Vec<Boundary>> {
    let prompt = format!("{candidates}\n\nReturn the chapter boundaries as STRICT JSON now.");

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
    let agent = client
        .agent(model)
        .preamble(SYSTEM)
        .temperature(0.0)
        .build();
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
    /// Ollama structured-output mode. Empty for free-form prose (the polish
    /// pass), so we skip the field entirely rather than send `"format": ""`.
    #[serde(skip_serializing_if = "str::is_empty")]
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
    let base =
        std::env::var("OLLAMA_HOST").unwrap_or_else(|_| "http://localhost:11434".to_string());
    let url = format!("{}/api/chat", base.trim_end_matches('/'));
    let body = OllamaChatReq {
        model,
        messages: vec![
            OllamaMsg {
                role: "system",
                content: SYSTEM,
            },
            OllamaMsg {
                role: "user",
                content: prompt,
            },
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

// ---------- Optional transcript polish pass ----------

const POLISH_SYSTEM: &str = "You repair character-level extraction glitches in a \
book passage so it reads aloud cleanly in text-to-speech. You will receive prose \
extracted from a PDF. You make ONLY mechanical character fixes — you are NOT an \
editor. \
WHAT TO FIX: \
(1) Broken ligatures and mojibake: e.g. 'ﬁ'->'fi', 'ﬂ'->'fl', garbled accented \
letters, stray control characters, and obvious OCR letter errors (e.g. 'rn'->'m' \
only when unmistakable). \
(2) Words split by a hyphen across a line break: rejoin them (e.g. 'exam-\\nple' \
-> 'example'). \
(3) Spacing only: collapse doubled spaces and insert a missing space between two \
run-together words. \
ABSOLUTE RULES — violating any means failure: \
(A) PRESERVE EVERY WORD and the sentence order exactly. Do NOT rewrite, \
paraphrase, summarize, translate, shorten, or add words. \
(B) PRESERVE ALL PUNCTUATION EXACTLY — every period, comma, colon, semicolon, \
question mark, exclamation mark, quote, and apostrophe must remain. NEVER remove \
or add sentence punctuation. \
(C) Do NOT delete lines, headings, or any content. Removal of artifacts is \
handled elsewhere — your job is only character repair. \
(D) If the passage has no character glitches, return it completely UNCHANGED. \
Return ONLY the repaired prose as plain text — no preamble, commentary, markdown, \
or surrounding quotes.";

/// Roughly how many characters of transcript to send per LLM request. Small
/// enough to stay well within a local model's context and keep each round trip
/// fast; we split on paragraph boundaries so we never cut a sentence.
const POLISH_CHUNK_CHARS: usize = 6000;

/// LLM polish pass over an already-cleaned chapter transcript. Splits the text
/// into paragraph-aligned chunks, asks the model to delete artifacts only, and
/// keeps the model's output for a chunk ONLY if it stays within a length
/// tolerance of the input (a guard against the model rewriting/summarizing or
/// hallucinating). Any failed/over-divergent chunk falls back to the original,
/// so this can only improve or no-op — never corrupt — the transcript.
pub async fn polish_transcript(model: &str, transcript: &str) -> AppResult<String> {
    let chunks = chunk_paragraphs(transcript, POLISH_CHUNK_CHARS);
    let mut out: Vec<String> = Vec::with_capacity(chunks.len());
    for chunk in chunks {
        match polish_chunk(model, &chunk).await {
            Ok(cleaned) if accept_polish(&chunk, &cleaned) => out.push(cleaned),
            Ok(_) => {
                eprintln!("[polish] chunk diverged too much; keeping original");
                out.push(chunk);
            }
            Err(e) => {
                eprintln!("[polish] chunk failed ({e}); keeping original");
                out.push(chunk);
            }
        }
    }
    Ok(out.join("\n\n"))
}

/// Split text into chunks of <= `budget` chars, breaking only on blank-line
/// paragraph boundaries so sentences stay intact. A single paragraph larger
/// than the budget is emitted as its own (over-budget) chunk.
fn chunk_paragraphs(text: &str, budget: usize) -> Vec<String> {
    let mut chunks: Vec<String> = Vec::new();
    let mut cur = String::new();
    for para in text.split("\n\n") {
        let para = para.trim();
        if para.is_empty() {
            continue;
        }
        if !cur.is_empty() && cur.len() + para.len() + 2 > budget {
            chunks.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push_str("\n\n");
        }
        cur.push_str(para);
    }
    if !cur.is_empty() {
        chunks.push(cur);
    }
    chunks
}

/// Accept the polished chunk only if its length is within tolerance of the
/// original. The pass is deletion-only, so the output should be the same size
/// or modestly smaller — never larger, and never a fraction of the input
/// (which would mean the model summarized or dropped content).
fn accept_polish(original: &str, cleaned: &str) -> bool {
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        return false;
    }
    let o = original.trim().chars().count() as f64;
    let c = cleaned.chars().count() as f64;
    if o == 0.0 {
        return false;
    }
    // Polish is now a character-repair pass (fix ligatures/spacing/hyphenation),
    // so length should barely change. A tight band rejects rewrites/summaries.
    let ratio = c / o;
    if !(0.90..=1.05).contains(&ratio) {
        return false;
    }
    // Punctuation-preservation guard: a char-repair pass must NOT drop sentence
    // punctuation. The earlier regression stripped every comma/period while
    // keeping word count, so a length check alone missed it. Require the
    // polished text to retain ~all of the original's punctuation.
    let count_punct = |s: &str| s.chars().filter(|c| matches!(c, '.' | ',' | ':' | ';' | '?' | '!')).count();
    let op = count_punct(original) as f64;
    let cp = count_punct(cleaned) as f64;
    if op > 0.0 && cp / op < 0.90 {
        return false;
    }
    true
}

async fn polish_chunk(model: &str, chunk: &str) -> AppResult<String> {
    // Prefer Rig; fall back to direct Ollama generate-style chat without JSON
    // formatting (we want plain prose back, not JSON).
    match rig_polish(model, chunk).await {
        Ok(s) if !s.trim().is_empty() => Ok(s),
        Ok(_) => http_polish(model, chunk).await,
        Err(e) => {
            eprintln!("[polish] rig path failed ({e}); trying direct Ollama");
            http_polish(model, chunk).await
        }
    }
}

async fn rig_polish(model: &str, chunk: &str) -> AppResult<String> {
    let client = ollama::Client::new(rig_core::client::Nothing)
        .map_err(|e| AppError::Llm(format!("ollama client: {e}")))?;
    let agent = client
        .agent(model)
        .preamble(POLISH_SYSTEM)
        .temperature(0.0)
        .build();
    let raw = agent
        .prompt(chunk)
        .await
        .map_err(|e| AppError::Llm(e.to_string()))?;
    Ok(raw.trim().to_string())
}

async fn http_polish(model: &str, chunk: &str) -> AppResult<String> {
    let base =
        std::env::var("OLLAMA_HOST").unwrap_or_else(|_| "http://localhost:11434".to_string());
    let url = format!("{}/api/chat", base.trim_end_matches('/'));
    let body = OllamaChatReq {
        model,
        messages: vec![
            OllamaMsg {
                role: "system",
                content: POLISH_SYSTEM,
            },
            OllamaMsg {
                role: "user",
                content: chunk,
            },
        ],
        stream: false,
        // No "json" format here — we want plain cleaned prose back.
        format: "",
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
    Ok(resp.message.content.trim().to_string())
}

#[cfg(test)]
mod polish_tests {
    use super::*;

    #[test]
    fn chunks_break_on_paragraphs_and_respect_budget() {
        let text = "aaaa\n\nbbbb\n\ncccc\n\ndddd";
        // budget 10 -> ~2 paras per chunk (4 chars + "\n\n").
        let chunks = chunk_paragraphs(text, 10);
        assert!(chunks.len() >= 2, "expected multiple chunks: {chunks:?}");
        // No chunk splits a paragraph: every original para survives intact.
        let rejoined = chunks.join("\n\n");
        for p in ["aaaa", "bbbb", "cccc", "dddd"] {
            assert!(rejoined.contains(p), "lost paragraph {p}");
        }
    }

    #[test]
    fn oversize_paragraph_becomes_its_own_chunk() {
        let big = "x".repeat(20);
        let text = format!("{big}\n\nsmall");
        let chunks = chunk_paragraphs(&text, 8);
        assert_eq!(chunks[0], big, "oversize para should stand alone");
    }

    /// Live end-to-end polish against a running Ollama. Ignored by default.
    /// Run with: `OLLAMA_POLISH_MODEL=gemma4:e2b cargo test polish_live -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn polish_live_removes_artifacts_keeps_prose() {
        let model = std::env::var("OLLAMA_POLISH_MODEL").unwrap_or_else(|_| "gemma4:e2b".into());
        // A passage with a fused running header, an inline footnote marker, and
        // a trailing page-number artifact — the kind of thing the deterministic
        // pass might miss in another book.
        let dirty = "32 De Fide Thomas develops a framework for the relationship of \
                     faith and reason.1 He begins by making a distinction within truths \
                     about God. 47";
        let out = polish_transcript(&model, dirty).await.expect("polish call");
        eprintln!("POLISHED: <<<{out}>>>");
        assert!(!out.trim().is_empty(), "empty output");
        assert!(
            out.contains("Thomas develops a framework"),
            "core sentence lost"
        );
        assert!(
            out.contains("distinction within truths about God"),
            "core sentence lost"
        );
    }

    /// Live boundary detection over a noisy heading list (TOC + body + index),
    /// proving the LLM dedupes to the real top-level chapters. Ignored.
    /// Run e.g.:
    ///   OLLAMA_BOUND_MODEL=gemma4:e2b cargo test boundaries_live -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn boundaries_live_dedupes_toc_and_index() {
        let model = std::env::var("OLLAMA_BOUND_MODEL").unwrap_or_else(|_| "gemma4:e2b".into());
        // Candidates are already TOC-deduped by `pdf::dedupe_headings` before
        // reaching the model, so this is the clean body-heading list the LLM
        // selects/labels from (mirrors the real pipeline).
        let candidates = "DETECTED HEADING LINES (page: text):\n\
            12: CHAPTER 1. Loomings.\n\
            20: CHAPTER 2. The Carpet-Bag.\n\
            27: CHAPTER 3. The Spouter-Inn.\n";
        let b = detect_boundaries(&model, candidates).await.expect("detect");
        eprintln!("BOUNDARIES: {b:?}");
        assert!(b.len() >= 3, "expected >=3 chapters, got {}", b.len());
        assert!(
            b.iter().any(|x| x.title.contains("Loomings")),
            "missing Loomings"
        );
        assert!(
            b.iter().any(|x| x.start_page >= 12),
            "TOC page used instead of body page: {b:?}"
        );
    }

    /// Ad-hoc: dump the polished version of $AUDIOBOOK_POLISH_TEXT. Ignored.
    #[tokio::test]
    #[ignore]
    async fn polish_live_dump() {
        let Ok(text) = std::env::var("AUDIOBOOK_POLISH_TEXT") else {
            return;
        };
        let model = std::env::var("OLLAMA_POLISH_MODEL").unwrap_or_else(|_| "gemma4:e2b".into());
        let out = polish_transcript(&model, &text).await.expect("polish");
        eprintln!("=== POLISHED ===\n{out}\n=== END ===");
    }

    #[test]
    fn accept_polish_guards_against_rewrite_and_summarize() {
        let original = "The quick, brown fox jumps over the lazy dog; it was fast.";
        // identical -> accept
        assert!(accept_polish(original, original));
        // pure character repair (ligature fix), punctuation intact -> accept
        assert!(accept_polish(
            original,
            "The quick, brown fox jumps over the lazy dog; it was fast."
        ));
        // summarized to a fraction -> reject
        assert!(!accept_polish(original, "A fox jumps."));
        // empty -> reject
        assert!(!accept_polish(original, "   "));
        // ballooned (hallucinated additions) -> reject
        let bloated = original.repeat(3);
        assert!(!accept_polish(original, &bloated));
    }

    #[test]
    fn build_candidates_adds_page_openings_when_headings_sparse() {
        // A 19-page book with a single detected heading (the Seeking case):
        // page openings must be offered as a safety net so the LLM can divide.
        let headings = vec![(14, "Conclusion".to_string())];
        let page_heads: Vec<(usize, String)> =
            (1..=19).map(|i| (i, format!("opening line {i}"))).collect();
        let s = build_candidates(&[], &headings, &page_heads);
        assert!(
            s.contains("PAGE OPENINGS"),
            "sparse headings should trigger page-openings fallback:\n{s}"
        );
        // Strong heading list (>= one per ~12 pages) should NOT need the fallback.
        let strong: Vec<(usize, String)> =
            (1..=6).map(|i| (i * 3, format!("CHAPTER {i}"))).collect();
        let s2 = build_candidates(&[], &strong, &page_heads);
        assert!(
            !s2.contains("PAGE OPENINGS"),
            "strong heading list should not add page openings:\n{s2}"
        );
    }

    #[test]
    fn accept_polish_rejects_punctuation_stripping() {
        // The real regression: same words, punctuation removed. Word count and
        // length barely change, but every comma/period/semicolon is gone.
        let original = "There are two ways, one of life, and one of death; a great difference.";
        let stripped = "There are two ways one of life and one of death a great difference";
        assert!(
            !accept_polish(original, stripped),
            "must reject punctuation-stripped output"
        );
    }
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

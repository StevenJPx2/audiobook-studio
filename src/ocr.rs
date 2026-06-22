//! OCR fallback for PDFs whose pages have no extractable text layer (scanned
//! books). We render each empty page to an image with pdfium and recognize its
//! text with a local Ollama vision model (`glm-ocr`), reusing the same Ollama
//! HTTP surface the chapter/polish passes already use — so OCR adds no new
//! bundled binaries or models to sign; the user just `ollama pull`s glm-ocr.
//!
//! Design (book-agnostic, non-fatal): callers detect which pages are empty and
//! OCR only those, so hybrid PDFs (mostly real text, a few scanned pages) cost
//! nothing extra. A failed page yields an empty string and the pipeline carries
//! on with whatever text exists.

use crate::error::{AppError, AppResult};
use base64::Engine;
use serde::{Deserialize, Serialize};

/// Default Ollama vision model for OCR. Quantized 0.9B GLM-OCR: small + fast,
/// good for OCR-ing many pages of a scanned book on-device. Override with
/// `AUDIOBOOK_OCR_MODEL`.
pub const DEFAULT_OCR_MODEL: &str = "glm-ocr:q8_0";

/// A page is considered to lack a usable text layer when it has fewer than this
/// many non-whitespace characters. Catches blank/scanned pages while leaving
/// genuinely (even sparsely) texted pages untouched.
pub const EMPTY_PAGE_CHAR_THRESHOLD: usize = 20;

/// Resolve the OCR model tag (env override or default).
pub fn model() -> String {
    std::env::var("AUDIOBOOK_OCR_MODEL").unwrap_or_else(|_| DEFAULT_OCR_MODEL.to_string())
}

/// True if a page's extracted text is too sparse to be a real text layer.
pub fn is_empty_page(text: &str) -> bool {
    text.chars().filter(|c| !c.is_whitespace()).count() < EMPTY_PAGE_CHAR_THRESHOLD
}

/// Page indices (0-based) whose text layer is empty/near-empty.
pub fn empty_page_indices(pages: &[String]) -> Vec<usize> {
    pages
        .iter()
        .enumerate()
        .filter(|(_, t)| is_empty_page(t))
        .map(|(i, _)| i)
        .collect()
}

#[derive(Serialize)]
struct OcrChatReq<'a> {
    model: &'a str,
    messages: Vec<OcrMsg<'a>>,
    stream: bool,
    options: OcrOpts,
}
#[derive(Serialize)]
struct OcrMsg<'a> {
    role: &'a str,
    content: &'a str,
    /// Base64-encoded image bytes (no data: prefix), per Ollama's chat API.
    images: Vec<String>,
}
#[derive(Serialize)]
struct OcrOpts {
    temperature: f32,
}
#[derive(Deserialize)]
struct OcrChatResp {
    message: OcrRespMsg,
}
#[derive(Deserialize)]
struct OcrRespMsg {
    content: String,
}

/// Ollama base URL (shared convention with the agent module).
fn ollama_base() -> String {
    std::env::var("OLLAMA_HOST").unwrap_or_else(|_| "http://localhost:11434".to_string())
}

/// Is the OCR model present in the local Ollama? Used to skip OCR gracefully
/// (offline, or model not pulled) rather than erroring the whole job.
pub async fn available(model: &str) -> bool {
    let url = format!("{}/api/tags", ollama_base().trim_end_matches('/'));
    let Ok(resp) = reqwest::Client::new().get(&url).send().await else {
        return false;
    };
    let Ok(body) = resp.text().await else {
        return false;
    };
    // Match on the family tag prefix so "glm-ocr:q8_0" matches a listed
    // "glm-ocr:q8_0" or a bare "glm-ocr".
    let family = model.split(':').next().unwrap_or(model);
    body.contains(model) || body.contains(family)
}

/// Recognize the text of one rendered page image (PNG/JPEG bytes) via glm-ocr.
/// Returns the recognized plain text (trimmed). Non-fatal upstream: callers may
/// log and substitute an empty string on `Err`.
pub async fn ocr_image(model: &str, image_bytes: &[u8]) -> AppResult<String> {
    let b64 = base64::engine::general_purpose::STANDARD.encode(image_bytes);
    let url = format!("{}/api/chat", ollama_base().trim_end_matches('/'));
    let body = OcrChatReq {
        model,
        messages: vec![OcrMsg {
            role: "user",
            // glm-ocr's documented text-recognition trigger.
            content: "Text Recognition:",
            images: vec![b64],
        }],
        stream: false,
        options: OcrOpts { temperature: 0.0 },
    };
    let resp: OcrChatResp = reqwest::Client::new()
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| AppError::Ocr(e.to_string()))?
        .error_for_status()
        .map_err(|e| AppError::Ocr(e.to_string()))?
        .json()
        .await
        .map_err(|e| AppError::Ocr(e.to_string()))?;
    Ok(resp.message.content.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_page_detection() {
        assert!(is_empty_page(""));
        assert!(is_empty_page("   \n  \n "));
        assert!(is_empty_page("12")); // bare folio on a scanned page
        assert!(is_empty_page("  page  3  ")); // 6 non-ws chars < 20
        assert!(!is_empty_page(
            "This page has a genuine and sufficiently long text layer."
        ));
    }

    #[test]
    fn empty_indices_selects_only_sparse_pages() {
        let pages = vec![
            "Real prose with plenty of extractable characters here.".to_string(),
            "".to_string(),
            "7".to_string(),
            "Another solid page of actual text content for the reader.".to_string(),
        ];
        assert_eq!(empty_page_indices(&pages), vec![1, 2]);
    }

    #[test]
    fn model_default_and_override() {
        // SAFETY: single-threaded test; we set then clear the env var.
        unsafe { std::env::remove_var("AUDIOBOOK_OCR_MODEL") };
        assert_eq!(model(), DEFAULT_OCR_MODEL);
    }
}

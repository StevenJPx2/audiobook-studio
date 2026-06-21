//! PDF text extraction + light TTS cleaning.
//!
//! We extract text *per page* so the chapter splitter can map LLM-detected
//! start pages onto real page ranges, then clean each chapter into flowing,
//! TTS-friendly paragraphs (ligatures normalized, hyphenation repaired,
//! running headers/folios dropped).
use crate::error::{AppError, AppResult};
use crate::model::{BookInfo, OutlineItem};
use once_cell::sync::Lazy;
use regex::Regex;
use std::path::Path;
use unicode_normalization::UnicodeNormalization;

/// Extract the text of every page. Index 0 == page 1.
pub fn pages(path: &str) -> AppResult<Vec<String>> {
    let pages = pdf_extract::extract_text_by_pages(path)
        .map_err(|e| AppError::Pdf(format!("extract_text_by_pages: {e}")))?;
    Ok(pages)
}

/// Build a quick BookInfo for the UI: page count, size, and outline (if any).
pub fn info(path: &str) -> AppResult<BookInfo> {
    let p = Path::new(path);
    let meta = std::fs::metadata(path)?;
    let pages = pages(path)?;
    let outline = read_outline(path).unwrap_or_default();
    Ok(BookInfo {
        path: path.to_string(),
        file_name: p
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default(),
        page_count: pages.len(),
        size_mb: meta.len() as f64 / 1_048_576.0,
        outline,
    })
}

/// Read the embedded PDF outline / bookmarks via lopdf (re-exported by rig? no —
/// pdf-extract pulls lopdf transitively, but we read with our own minimal pass).
/// If the document has no outline this returns an empty vec.
pub fn read_outline(path: &str) -> AppResult<Vec<OutlineItem>> {
    // pdf-extract depends on lopdf; we use it directly for the outline.
    let doc = lopdf::Document::load(path).map_err(|e| AppError::Pdf(e.to_string()))?;
    let mut items = Vec::new();
    // Map page object-id -> page number (1-indexed).
    let page_numbers: std::collections::HashMap<(u32, u16), usize> = doc
        .get_pages()
        .into_iter()
        .map(|(num, id)| (id, num as usize))
        .collect();

    if let Ok(catalog) = doc.catalog() {
        if let Ok(outlines_ref) = catalog.get(b"Outlines") {
            if let Ok(outlines_id) = outlines_ref.as_reference() {
                if let Ok(outlines) = doc.get_dictionary(outlines_id) {
                    if let Ok(first) = outlines.get(b"First").and_then(|o| o.as_reference()) {
                        walk_outline(&doc, first, 0, &page_numbers, &mut items);
                    }
                }
            }
        }
    }
    Ok(items)
}

fn walk_outline(
    doc: &lopdf::Document,
    node_id: (u32, u16),
    level: usize,
    page_numbers: &std::collections::HashMap<(u32, u16), usize>,
    out: &mut Vec<OutlineItem>,
) {
    let mut cur = Some(node_id);
    while let Some(id) = cur {
        let Ok(dict) = doc.get_dictionary(id) else { break };
        let title = dict
            .get(b"Title")
            .ok()
            .and_then(|o| o.as_str().ok())
            .map(|b| decode_pdf_string(b))
            .unwrap_or_default();
        let page = resolve_dest_page(doc, dict, page_numbers).unwrap_or(0);
        if !title.is_empty() {
            out.push(OutlineItem { level, title, page });
        }
        if let Ok(first) = dict.get(b"First").and_then(|o| o.as_reference()) {
            walk_outline(doc, first, level + 1, page_numbers, out);
        }
        cur = dict.get(b"Next").and_then(|o| o.as_reference()).ok();
    }
}

fn resolve_dest_page(
    doc: &lopdf::Document,
    dict: &lopdf::Dictionary,
    page_numbers: &std::collections::HashMap<(u32, u16), usize>,
) -> Option<usize> {
    // Dest may be an array [pageRef /XYZ ...] or under /A /D.
    let dest = dict
        .get(b"Dest")
        .ok()
        .cloned()
        .or_else(|| {
            dict.get(b"A")
                .ok()
                .and_then(|a| a.as_reference().ok())
                .and_then(|aid| doc.get_dictionary(aid).ok())
                .and_then(|ad| ad.get(b"D").ok().cloned())
        })?;
    let arr = dest.as_array().ok()?;
    let page_ref = arr.first()?.as_reference().ok()?;
    page_numbers.get(&page_ref).copied()
}

fn decode_pdf_string(b: &[u8]) -> String {
    // UTF-16BE with BOM, else Latin-1-ish fallback.
    if b.len() >= 2 && b[0] == 0xFE && b[1] == 0xFF {
        let u16s: Vec<u16> = b[2..]
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&u16s)
    } else {
        b.iter().map(|&c| c as char).collect()
    }
}

// ---------- TTS cleaning ----------

static FOLIO: Lazy<Regex> = Lazy::new(|| Regex::new(r"^\s*\d{1,4}\s*$").unwrap());
static MULTISPACE: Lazy<Regex> = Lazy::new(|| Regex::new(r"[ \t]{2,}").unwrap());
static SPACE_PUNCT: Lazy<Regex> = Lazy::new(|| Regex::new(r"\s+([,.;:!?])").unwrap());

/// Clean a slice of raw page texts into flowing paragraphs for TTS.
pub fn clean_pages_to_transcript(raw_pages: &[String]) -> String {
    let mut paragraphs: Vec<String> = Vec::new();

    for page in raw_pages {
        // Split a page into blank-line-separated blocks.
        for block in page.split("\n\n") {
            let joined = join_lines(block);
            let t = joined.trim();
            if t.is_empty() || FOLIO.is_match(t) {
                continue;
            }
            paragraphs.push(t.to_string());
        }
    }

    let merged = stitch_across_breaks(paragraphs);
    merged
        .into_iter()
        .map(|p| normalize(&p))
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Join the visual lines of one block, repairing end-of-line hyphenation.
fn join_lines(block: &str) -> String {
    let mut out = String::new();
    for line in block.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if out.is_empty() {
            out.push_str(line);
        } else if out.ends_with('-') {
            let prev = &out[..out.len() - 1];
            let prev_lower = prev.chars().last().map(|c| c.is_lowercase()).unwrap_or(false);
            let next_lower = line.chars().next().map(|c| c.is_lowercase()).unwrap_or(false);
            if prev_lower && next_lower {
                out = format!("{prev}{line}"); // soft hyphen: glue
            } else {
                out = format!("{prev}-{line}"); // real hyphen: keep
            }
        } else {
            out.push(' ');
            out.push_str(line);
        }
    }
    out
}

/// Stitch paragraphs that were split across page/column breaks.
fn stitch_across_breaks(paras: Vec<String>) -> Vec<String> {
    let term = ['.', '!', '?', ':', '”', '"', '’', '\'', ')', ']'];
    let mut merged: Vec<String> = Vec::new();
    for p in paras {
        if let Some(prev) = merged.last_mut() {
            if prev.ends_with('-') {
                let base = prev[..prev.len() - 1].to_string();
                let prev_lower = base.chars().last().map(|c| c.is_lowercase()).unwrap_or(false);
                let next_lower = p.chars().next().map(|c| c.is_lowercase()).unwrap_or(false);
                *prev = if prev_lower && next_lower {
                    format!("{base}{p}")
                } else {
                    format!("{base}-{p}")
                };
                continue;
            }
            let prev_trim = prev.trim_end();
            let ends_sentence = prev_trim.chars().last().map(|c| term.contains(&c)).unwrap_or(false);
            let starts_lower = p.chars().next().map(|c| c.is_lowercase()).unwrap_or(false);
            if !ends_sentence && starts_lower {
                *prev = format!("{prev_trim} {p}");
                continue;
            }
        }
        merged.push(p);
    }
    merged
}

fn normalize(t: &str) -> String {
    let nfkc: String = t.nfkc().collect();
    let nfkc = SPACE_PUNCT.replace_all(&nfkc, "$1");
    let nfkc = MULTISPACE.replace_all(&nfkc, " ");
    nfkc.trim().to_string()
}

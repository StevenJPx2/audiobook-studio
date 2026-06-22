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
        let Ok(dict) = doc.get_dictionary(id) else {
            break;
        };
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
    let dest = dict.get(b"Dest").ok().cloned().or_else(|| {
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
/// A line that is just a folio glued to the running head, e.g. "32" or "31".
/// Used to peel a leading/trailing page number off a header line.
static LEAD_FOLIO: Lazy<Regex> = Lazy::new(|| Regex::new(r"^\s*\d{1,4}\s*").unwrap());
static TRAIL_FOLIO: Lazy<Regex> = Lazy::new(|| Regex::new(r"\s*\d{1,4}\s*$").unwrap());
/// Footnote / endnote apparatus: a block that begins with a note marker like
/// "1. " or "12. " followed by a capitalized citation. These are page-bottom
/// notes that read as gibberish in narration.
static FOOTNOTE_BLOCK: Lazy<Regex> = Lazy::new(|| Regex::new(r"^\s*\d{1,3}\.\s+\p{Lu}").unwrap());
/// A chapter-end bibliography entry begins with a capitalized author token or a
/// repeated-author em-dash (`———.`). This is deliberately broad — it's the
/// publication-signature gate below that actually classifies the block, so we
/// never drop ordinary prose that merely starts with a capital letter.
static BIBLIO_START: Lazy<Regex> = Lazy::new(|| Regex::new(r"^\s*(—{2,}\.|\p{Lu})").unwrap());
/// Publication metadata that marks a block as a reference list, not prose:
/// "Translated by", "trans. ", "edited by", "ed. ", or a
/// "City: Publisher, 1999" colophon. These effectively never occur in narration
/// prose, so requiring one makes bibliography removal safe.
static BIBLIO_SIGNATURE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(Translated by |\btrans\.\s\p{Lu}|edited by |\bed\.\s\p{Lu}\.|\b\p{Lu}[\w.]*:\s+\p{Lu}[\w&. ]+,\s+\d{4})",
    )
    .unwrap()
});
/// Inline superscript footnote reference glued to the end of a word/clause,
/// e.g. `Church.”1`, `absolutely.2`, `know. 3`, `telescope. 12`. We only strip
/// a 1-2 digit run that directly follows sentence-ending punctuation or a
/// closing quote, and is itself followed by whitespace/end or the start of the
/// next sentence — never another digit, ":" or "," (so "Isaiah 7:9", "(1689)",
/// "1 Cor. 2:14" are left intact). The `regex` crate has no lookahead, so the
/// trailing boundary is captured in $2 and re-emitted.
static INLINE_NOTE_REF: Lazy<Regex> = Lazy::new(|| {
    // $1 = the punctuation the marker is glued to; $2 = the trailing boundary
    // (a space, the start of the next sentence, or empty at end-of-string).
    Regex::new(r#"([.!?”’"'])\s?\d{1,2}(\s|[A-Z“'(]|$)"#).unwrap()
});
/// A chapter/part/section heading line, e.g. "CHAPTER 1. Loomings.",
/// "Chapter I.", "CHAPTER IV. NATURAL SELECTION.", "BOOK II", "PART THIRD",
/// "Introduction". Roman or arabic numerals with an optional inline title. This
/// is a general structural pattern (no book-specific text); used as the
/// deterministic source of chapter divisions, with the LLM refining/labeling.
static HEADING: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?ix)
        ^\s*
        (
          (chapter|book|part|section|canto|letter)     # structural keyword
          \s+
          (                                            # …followed by a NUMBER:
              [0-9]{1,3}                               #   arabic (1, 42)
            | [ivxlcdm]{1,7}                           #   roman (I, IV, XII)
            | (one|two|three|four|five|six|seven|eight|nine|ten|eleven|twelve|
               thirteen|fourteen|fifteen|sixteen|seventeen|eighteen|nineteen|twenty|
               first|second|third|fourth|fifth|sixth|seventh|eighth|ninth|tenth|
               last|final)                             #   spelled-out cardinal/ordinal
          )
          \b
          |
          (introduction|prologue|epilogue|preface|foreword|conclusion|prelude|afterword)
        )
        [.\):\s]*                                       # optional separators
        (.{0,80})?                                      # optional inline title
        $
        ",
    )
    .unwrap()
});

/// Scan pages for chapter/part/section headings deterministically. Returns
/// `(page_number_1indexed, heading_text)` for every heading line found, in page
/// order. This is the reliable, model-free source of chapter divisions; the LLM
/// refines/labels these rather than discovering them from scratch.
///
/// Two complementary signals are collected:
/// 1. Keyword/numeral headings anywhere on a page (`Chapter 4`, `BOOK II`,
///    `Introduction`), matched by `HEADING`.
/// 2. Display-title headings: a page that *opens* with one to three consecutive
///    ALL-CAPS lines (the chapter title set in large caps, often wrapped across
///    lines), which carry no structural keyword or numeral. Common in trade
///    non-fiction whose chapters are bare titles. Detected book-agnostically by
///    shape (caps + page-top position), not by any specific text.
pub fn chapter_headings(pages: &[String]) -> Vec<(usize, String)> {
    let mut out: Vec<(usize, String)> = Vec::new();
    for (i, page) in pages.iter().enumerate() {
        // (1) Keyword/numeral headings anywhere on the page.
        for line in page.lines() {
            let line = line.trim();
            if line.is_empty() || line.chars().count() > 90 {
                continue;
            }
            if HEADING.is_match(line) {
                out.push((i + 1, line.to_string()));
            }
        }
        // (2) Display-title heading at the very top of the page.
        if let Some(title) = caps_title_at_top(page) {
            // Avoid a duplicate if the same line was already caught by HEADING
            // (e.g. an all-caps "INTRODUCTION").
            if !out.iter().any(|(pg, t)| *pg == i + 1 && t.eq_ignore_ascii_case(&title)) {
                out.push((i + 1, title));
            }
        }
    }
    out
}

/// Detect a large-caps display title at the top of a page: the first one to
/// three non-empty lines are each predominantly UPPERCASE letters (a wrapped
/// title like `WHAT IS A` / `WORLDVIEW?`), and the run is followed by a line
/// that is NOT all-caps (the body, or a recurring series marker). Returns the
/// joined title (`"WHAT IS A WORLDVIEW?"`) or `None`.
///
/// Book-agnostic: keys only on letter case and page-top position, never on
/// specific words. Continuation pages (which begin mid-sentence in mixed case)
/// produce no match, so only true chapter openers are flagged.
fn caps_title_at_top(page: &str) -> Option<String> {
    let lines: Vec<&str> = page
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();
    let mut title_lines: Vec<&str> = Vec::new();
    for line in lines.iter().take(3) {
        if is_caps_line(line) {
            title_lines.push(line);
        } else {
            break;
        }
    }
    if title_lines.is_empty() {
        return None;
    }
    let title = title_lines.join(" ");
    let collapsed = MULTISPACE.replace_all(title.trim(), " ").to_string();
    // A real title has at least one letter and isn't absurdly long.
    if collapsed.chars().any(|c| c.is_alphabetic()) && collapsed.chars().count() <= 90 {
        Some(collapsed)
    } else {
        None
    }
}

/// True if a line reads as part of a caps display title: it contains letters,
/// every cased letter is uppercase, and it is short (a heading, not a shouted
/// sentence). Digits, punctuation, and spaces are allowed (`SO,`, `WHAT NEXT?`).
fn is_caps_line(line: &str) -> bool {
    let letters: Vec<char> = line.chars().filter(|c| c.is_alphabetic()).collect();
    if letters.len() < 2 {
        return false;
    }
    // Every alphabetic char must be uppercase (no lowercase letters at all).
    if line.chars().filter(|c| c.is_alphabetic()).any(|c| c.is_lowercase()) {
        return false;
    }
    // Keep it heading-sized: at most ~9 words and 60 chars, so an all-caps
    // emphatic sentence in body prose is never mistaken for a title.
    let words = line.split_whitespace().count();
    words <= 9 && line.chars().count() <= 60
}

/// Remove table-of-contents duplicates from a heading list. A book's TOC lists
/// the same heading text that later appears at the real chapter start, so a
/// normalized title occurring more than once is deduped to its LAST occurrence
/// (the body heading; the TOC copy comes first). General across books — no
/// dependence on specific titles. Returns headings sorted by page.
pub fn dedupe_headings(headings: &[(usize, String)]) -> Vec<(usize, String)> {
    use std::collections::BTreeMap;
    let mut by_title: BTreeMap<String, (usize, String)> = BTreeMap::new();
    for (pg, title) in headings {
        let key = title
            .trim()
            .trim_end_matches([']', ')', '.'])
            .to_lowercase();
        // insert() overwrites, so iterating in page order keeps the last page.
        by_title.insert(key, (*pg, title.trim().to_string()));
    }
    let mut out: Vec<(usize, String)> = by_title.into_values().collect();
    out.sort_by_key(|(pg, _)| *pg);
    out
}

/// Clean a slice of raw page texts into flowing paragraphs for TTS.
pub fn clean_pages_to_transcript(raw_pages: &[String]) -> String {
    // Detect repeating running headers/footers (book/chapter titles printed at
    // the top or bottom of every page) so we can drop them book-agnostically.
    let headers = detect_running_heads(raw_pages);

    let mut paragraphs: Vec<String> = Vec::new();

    for page in raw_pages {
        // Split a page into blank-line-separated blocks.
        for block in page.split("\n\n") {
            let joined = join_lines(block);
            // Peel a running header off the front/back of the block (headers
            // are frequently fused onto adjacent body text by the extractor),
            // then drop the block entirely if nothing meaningful remains.
            let stripped = strip_running_heads(&joined, &headers);
            let t = stripped.trim();
            if t.is_empty() || FOLIO.is_match(t) {
                continue;
            }
            // Drop footnote / endnote apparatus blocks.
            if FOOTNOTE_BLOCK.is_match(t) {
                continue;
            }
            // Drop chapter-end bibliography entries: must look like a reference
            // entry AND carry a publication signature (so prose that merely
            // opens with "Surname, First name…" is never dropped).
            if BIBLIO_START.is_match(t) && BIBLIO_SIGNATURE.is_match(t) {
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

/// Normalize a candidate header line for frequency comparison: drop a leading
/// and trailing folio number and collapse whitespace. `"32 De Fide"` and
/// `"34 De Fide"` both normalize to `"De Fide"`; `"31How Do I Know…"` to
/// `"How Do I Know…"`.
fn header_key(line: &str) -> String {
    let s = LEAD_FOLIO.replace(line.trim(), "");
    let s = TRAIL_FOLIO.replace(&s, "");
    MULTISPACE.replace_all(s.trim(), " ").to_string()
}

/// Find lines that repeat as running heads across the page slice. A running
/// head is a short line (≤ 8 words) whose folio-stripped form appears at the
/// top or bottom of many pages. Returns the set of such keys.
fn detect_running_heads(raw_pages: &[String]) -> std::collections::HashSet<String> {
    use std::collections::HashMap;
    if raw_pages.len() < 4 {
        // Too few pages to tell a repeating header from real prose.
        return std::collections::HashSet::new();
    }
    let mut counts: HashMap<String, usize> = HashMap::new();
    for page in raw_pages {
        // Consider the first two and last two non-empty lines of each page —
        // headers/footers live at the page edges, not buried in body text.
        let lines: Vec<&str> = page
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .collect();
        let mut edges: Vec<&str> = Vec::new();
        edges.extend(lines.iter().take(2));
        edges.extend(lines.iter().rev().take(2));
        let mut seen_on_page = std::collections::HashSet::new();
        for line in edges {
            let key = header_key(line);
            // Header heuristic: non-empty, short, and not itself a bare folio.
            if key.is_empty() || key.split_whitespace().count() > 8 {
                continue;
            }
            if FOLIO.is_match(&key) {
                continue;
            }
            // Count each distinct header once per page.
            if seen_on_page.insert(key.clone()) {
                *counts.entry(key).or_insert(0) += 1;
            }
        }
    }
    // A real running head recurs on a large fraction of pages. Require it on at
    // least a quarter of pages (and at least twice) to avoid nuking a phrase
    // that merely opens a couple of paragraphs.
    let threshold = (raw_pages.len() as f64 * 0.25).ceil() as usize;
    let threshold = threshold.max(2);
    counts
        .into_iter()
        .filter(|(_, c)| *c >= threshold)
        .map(|(k, _)| k)
        .collect()
}

/// Strip a detected running head where it appears at the start or end of a
/// block — including the common case where the extractor fused it onto the
/// adjacent body text, e.g. `"30 De Fide ground for faith was…"`.
fn strip_running_heads(block: &str, headers: &std::collections::HashSet<String>) -> String {
    if headers.is_empty() {
        return block.to_string();
    }
    let mut s = block.trim().to_string();
    // Peel from the front: optional leading folio, then the header text.
    // Repeat once in case a folio+header pair leads the block.
    for _ in 0..2 {
        let head = LEAD_FOLIO.replace(&s, "");
        let mut matched = false;
        for h in headers {
            if let Some(rest) = head.strip_prefix(h.as_str()) {
                // Only treat as a header if what follows is a boundary (start of
                // a sentence/word), not the middle of a real word.
                if rest.is_empty() || rest.starts_with(|c: char| c.is_whitespace() || c == '.') {
                    s = rest.trim_start_matches(['.', ' ', '\t']).to_string();
                    matched = true;
                    break;
                }
            }
        }
        if !matched {
            break;
        }
    }
    // Peel a trailing header (+ optional folio) off the end.
    let trimmed = TRAIL_FOLIO.replace(s.trim_end(), "").to_string();
    for h in headers {
        if let Some(rest) = trimmed.strip_suffix(h.as_str()) {
            if rest.is_empty() || rest.ends_with(|c: char| c.is_whitespace()) {
                s = rest.trim_end().to_string();
                break;
            }
        }
    }
    s
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
            let prev_lower = prev
                .chars()
                .last()
                .map(|c| c.is_lowercase())
                .unwrap_or(false);
            let next_lower = line
                .chars()
                .next()
                .map(|c| c.is_lowercase())
                .unwrap_or(false);
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
                let prev_lower = base
                    .chars()
                    .last()
                    .map(|c| c.is_lowercase())
                    .unwrap_or(false);
                let next_lower = p.chars().next().map(|c| c.is_lowercase()).unwrap_or(false);
                *prev = if prev_lower && next_lower {
                    format!("{base}{p}")
                } else {
                    format!("{base}-{p}")
                };
                continue;
            }
            let prev_trim = prev.trim_end();
            let ends_sentence = prev_trim
                .chars()
                .last()
                .map(|c| term.contains(&c))
                .unwrap_or(false);
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
    // Drop inline superscript footnote references (e.g. `Church.”1`, `know. 3`),
    // keeping the punctuation they were glued to and the following boundary.
    let nfkc = INLINE_NOTE_REF.replace_all(&nfkc, "$1$2");
    let nfkc = SPACE_PUNCT.replace_all(&nfkc, "$1");
    let nfkc = MULTISPACE.replace_all(&nfkc, " ");
    nfkc.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a page slice that mirrors the real "Reasonable Faith" extraction:
    /// running heads (standalone + fused to body), a footnote apparatus block,
    /// and inline superscript footnote markers. Enough pages that the running
    /// head clears the frequency threshold.
    fn sample_pages() -> Vec<String> {
        vec![
            // Recto: header fused onto the start of the body paragraph; body has
            // inline footnote markers (Church.”1, absolutely.2, know. 3).
            "31How Do I Know Christianity Is True?\n\n\
             30 De Fide ground for faith was divine authority. Augustine confessed, \
             “I should not believe the Gospel except as moved by the authority of the \
             Catholic Church.”1 The Scriptures are to be believed absolutely.2 He asserts \
             that one must first believe before he can know. 3"
                .to_string(),
            // Verso: standalone header line, then a footnote block at the bottom.
            "32 De Fide\n\n\
             Their approaches were determinative for the Middle Ages.\n\n\
             1. Augustine, Against the Epistle of Manichaeus 5.6. 2. Augustine, Letters 82.3. \
             3. Augustine, On Free Will 2.1.6."
                .to_string(),
            "33How Do I Know Christianity Is True?\n\n\
             Thomas develops a framework for the relationship of faith and reason."
                .to_string(),
            "34 De Fide\n\n\
             Because these doctrines surpass reason, they are properly objects of faith."
                .to_string(),
            "35How Do I Know Christianity Is True?\n\n\
             The Enlightenment is also known as the Age of Reason."
                .to_string(),
        ]
    }

    #[test]
    fn drops_running_heads_standalone_and_fused() {
        let out = clean_pages_to_transcript(&sample_pages());
        assert!(
            !out.contains("De Fide"),
            "running head 'De Fide' leaked:\n{out}"
        );
        assert!(
            !out.contains("How Do I Know Christianity Is True?"),
            "running head (chapter title) leaked:\n{out}"
        );
        // The body that was fused after the header must survive, header removed.
        assert!(
            out.contains("ground for faith was divine authority"),
            "body lost:\n{out}"
        );
        assert!(
            !out.contains("30 De Fide ground"),
            "fused header not peeled:\n{out}"
        );
    }

    #[test]
    fn drops_footnote_apparatus_block() {
        let out = clean_pages_to_transcript(&sample_pages());
        assert!(
            !out.contains("Against the Epistle of Manichaeus"),
            "footnote block leaked into narration:\n{out}"
        );
        // Real prose on the same page is kept.
        assert!(
            out.contains("determinative for the Middle Ages"),
            "real prose dropped:\n{out}"
        );
    }

    #[test]
    fn strips_inline_footnote_markers() {
        let out = clean_pages_to_transcript(&sample_pages());
        assert!(
            out.contains("Catholic Church.”"),
            "closing quote lost:\n{out}"
        );
        assert!(
            !out.contains("Church.”1"),
            "inline marker after quote not stripped:\n{out}"
        );
        assert!(
            out.contains("believed absolutely."),
            "marker tail not stripped:\n{out}"
        );
        assert!(
            !out.contains("absolutely.2"),
            "inline marker '.2' not stripped:\n{out}"
        );
        assert!(
            out.contains("before he can know."),
            "spaced marker not stripped:\n{out}"
        );
        assert!(
            !out.contains("know. 3"),
            "spaced inline marker not stripped:\n{out}"
        );
    }

    #[test]
    fn keeps_legitimate_numbers_and_scripture_refs() {
        // Numbers that are NOT footnote markers must be preserved.
        let pages = vec![
            "Isaiah 7:9 in the Septuagint. The year 1689 mattered. See 1 Cor. 2:14 \
                          and Rom. 8:15 too. The sum equals 42 here."
                .to_string(),
        ];
        let out = clean_pages_to_transcript(&pages);
        assert!(out.contains("Isaiah 7:9"), "scripture ref mangled:\n{out}");
        assert!(out.contains("1689"), "year dropped:\n{out}");
        assert!(out.contains("1 Cor. 2:14"), "citation dropped:\n{out}");
        assert!(out.contains("Rom. 8:15"), "citation dropped:\n{out}");
        assert!(
            out.contains("equals 42 here"),
            "trailing number dropped:\n{out}"
        );
    }

    #[test]
    fn drops_bibliography_but_keeps_author_led_prose() {
        let pages = vec![
            "Barth, Karl. Dogmatics in Outline. Translated by G. J. Thomson. New York: \
             Philosophical Library, 1947.\n\n\
             ———. The Knowledge of God. London: Hodder, 1939.\n\n\
             Augustine famously argued that faith seeks understanding, and that we must \
             believe in order to know."
                .to_string(),
        ];
        let out = clean_pages_to_transcript(&pages);
        assert!(
            !out.contains("Translated by"),
            "bibliography entry leaked:\n{out}"
        );
        assert!(!out.contains("———"), "continuation entry leaked:\n{out}");
        // Prose that merely opens with a name (no pub signature) must survive.
        assert!(
            out.contains("Augustine famously argued"),
            "author-led prose dropped:\n{out}"
        );
    }

    #[test]
    fn repairs_hyphenation_and_keeps_real_hyphens() {
        let pages = vec!["under-\nstanding the well-\nknown self-\nevident truth".to_string()];
        let out = clean_pages_to_transcript(&pages);
        assert!(
            out.contains("understanding"),
            "soft hyphen not glued:\n{out}"
        );
        assert!(
            out.contains("well-known") || out.contains("wellknown"),
            "{out}"
        );
    }

    /// Real-PDF smoke check. Ignored by default (depends on a local file); run
    /// with: `AUDIOBOOK_TEST_PDF="/path/book.pdf" cargo test real_pdf -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_pdf_has_no_running_heads_or_footnotes() {
        let Ok(path) = std::env::var("AUDIOBOOK_TEST_PDF") else {
            eprintln!("set AUDIOBOOK_TEST_PDF to run");
            return;
        };
        let pages = pages(&path).expect("extract pages");
        let env_usize = |k: &str, d: usize| {
            std::env::var(k)
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(d)
        };
        let lo = env_usize("AUDIOBOOK_TEST_LO", 29).min(pages.len());
        let hi = env_usize("AUDIOBOOK_TEST_HI", 63).min(pages.len());
        let out = clean_pages_to_transcript(&pages[lo..hi]);
        if std::env::var("AUDIOBOOK_TEST_DUMP").is_ok() {
            // Dump-only mode for ad-hoc inspection of any book; skip the
            // Reasonable-Faith-specific assertions below.
            eprintln!("--- cleaned pages {lo}..{hi} ---\n{out}");
            return;
        }
        // numbered footnote markers like "1. Augustine, ..." should be gone:
        let numbered = out
            .split("\n\n")
            .filter(|p| FOOTNOTE_BLOCK.is_match(p.trim()))
            .count();
        eprintln!("remaining numbered-footnote paras: {numbered}");
        // No paragraph should still look like a bibliography entry (author-led
        // start AND a publication signature). Em-dash runs alone are NOT a
        // signal — the extractor renders math fraction bars as "————".
        let biblio_left = out
            .split("\n\n")
            .filter(|p| BIBLIO_START.is_match(p.trim()) && BIBLIO_SIGNATURE.is_match(p.trim()))
            .count();
        assert_eq!(biblio_left, 0, "bibliography entry leaked");
        assert!(
            !out.contains("Translated by"),
            "bibliography signature leaked"
        );
        assert!(!out.contains("De Fide"), "running head leaked");
        assert!(
            !out.contains("How Do I Know Christianity Is True?"),
            "chapter head leaked"
        );
        assert_eq!(numbered, 0, "numbered footnote block leaked");
    }

    #[test]
    fn detects_varied_heading_styles() {
        let pages = vec![
            "Front matter line\n\nCHAPTER 1. Loomings.\n\nCall me Ishmael.".to_string(),
            "Chapter II.\n\nIt is a truth universally acknowledged...".to_string(),
            "CHAPTER IV. NATURAL SELECTION.\n\nHow will the struggle for existence act?"
                .to_string(),
            "INTRODUCTION.\n\nWhen on board H.M.S. Beagle...".to_string(),
            "BOOK SECOND\n\nThe family was in mourning.".to_string(),
        ];
        let heads = chapter_headings(&pages);
        let texts: Vec<&str> = heads.iter().map(|(_, t)| t.as_str()).collect();
        assert!(
            texts.iter().any(|t| t.starts_with("CHAPTER 1. Loomings")),
            "{texts:?}"
        );
        assert!(
            texts.iter().any(|t| t.starts_with("Chapter II")),
            "{texts:?}"
        );
        assert!(
            texts.iter().any(|t| t.contains("NATURAL SELECTION")),
            "{texts:?}"
        );
        assert!(
            texts.iter().any(|t| t.starts_with("INTRODUCTION")),
            "{texts:?}"
        );
        assert!(
            texts.iter().any(|t| t.starts_with("BOOK SECOND")),
            "{texts:?}"
        );
        // Pages are 1-indexed; "CHAPTER 1" is on page 1.
        assert_eq!(heads[0].0, 1);
    }

    #[test]
    fn dedupes_toc_keeping_body_heading() {
        // Same titles listed early (TOC, pages 1) then again at real starts.
        let headings = vec![
            (1, "CHAPTER 1. Loomings.".to_string()),
            (1, "CHAPTER 2. The Carpet-Bag.".to_string()),
            (12, "CHAPTER 1. Loomings.".to_string()),
            (20, "CHAPTER 2. The Carpet-Bag.".to_string()),
        ];
        let deduped = dedupe_headings(&headings);
        assert_eq!(deduped.len(), 2, "TOC not deduped: {deduped:?}");
        // Body pages (12, 20) kept, not the TOC page (1).
        assert_eq!(deduped[0].0, 12);
        assert_eq!(deduped[1].0, 20);
    }

    #[test]
    fn heading_detector_ignores_prose_sentences() {
        // Sentences that merely *mention* a chapter/part must not be flagged.
        let pages = vec![
            "In chapter three the author argues at length about the nature of selection \
             and the struggle for life among species across the world."
                .to_string(),
            "Part of the difficulty is that we cannot see the past.".to_string(),
        ];
        let heads = chapter_headings(&pages);
        assert!(heads.is_empty(), "false-positive headings: {heads:?}");
    }

    #[test]
    fn detects_multiline_caps_display_titles() {
        // Mirrors "Seeking Answers, Finding Truth": each chapter opens a page
        // with a wrapped ALL-CAPS title, then a recurring series marker, then
        // body prose. No structural keyword or numeral anywhere.
        let pages = vec![
            "WHAT IS A \nWORLDVIEW?\n\nSEEKiNG\n\nI woke up in the middle of the night."
                .to_string(),
            // Continuation page: opens mid-prose, mixed case -> NOT a heading.
            "coherently from a set of axioms (or intermediate propositions). The third \
             theory follows."
                .to_string(),
            "WHO IS \nJESUS?\n\nSEEKiNG\n\nThe very core of Christianity is the person of Jesus."
                .to_string(),
            "SO, \nWHAT NEXT?\n\nSEEKiNG\n\nWe have surveyed the evidence."
                .to_string(),
        ];
        let heads = chapter_headings(&pages);
        let texts: Vec<&str> = heads.iter().map(|(_, t)| t.as_str()).collect();
        assert!(
            texts.iter().any(|t| *t == "WHAT IS A WORLDVIEW?"),
            "wrapped caps title not joined/detected: {texts:?}"
        );
        assert!(
            texts.iter().any(|t| *t == "WHO IS JESUS?"),
            "caps title missed: {texts:?}"
        );
        assert!(
            texts.iter().any(|t| *t == "SO, WHAT NEXT?"),
            "caps title with punctuation missed: {texts:?}"
        );
        // The continuation page (page 2) must produce no heading.
        assert!(
            !heads.iter().any(|(pg, _)| *pg == 2),
            "continuation page falsely flagged: {heads:?}"
        );
    }

    #[test]
    fn caps_detector_ignores_allcaps_body_and_short_markers() {
        // A long shouted sentence in body prose is not a title; a bare 1-letter
        // or recurring short marker alone should not register as a chapter.
        let pages = vec![
            "THIS IS A VERY LONG ALL CAPS SENTENCE THAT GOES ON AND ON BEYOND ANY \
             REASONABLE HEADING LENGTH AND MUST NOT BE TREATED AS A CHAPTER TITLE AT ALL."
                .to_string(),
            "Ordinary opening prose that begins a continuation page in mixed case here."
                .to_string(),
        ];
        let heads = chapter_headings(&pages);
        assert!(
            heads.is_empty(),
            "all-caps body sentence falsely detected as heading: {heads:?}"
        );
    }

    #[test]
    fn caps_title_does_not_double_count_keyword_heading() {
        // An all-caps "INTRODUCTION." is caught by HEADING; the caps detector
        // must not add a duplicate for the same page.
        let pages = vec!["INTRODUCTION.\n\nWhen on board the Beagle...".to_string()];
        let heads = chapter_headings(&pages);
        let on_p1 = heads.iter().filter(|(pg, _)| *pg == 1).count();
        assert_eq!(on_p1, 1, "duplicate heading emitted: {heads:?}");
    }

    /// Ignored corpus harness: scan a plain-text book (split into synthetic
    /// pages on form-feeds or blank-line groups) and print detected headings.
    /// Run e.g.:
    ///   AUDIOBOOK_TEST_TXT=/tmp/gutenberg/2701.txt \
    ///   cargo test heading_corpus -- --ignored --nocapture
    #[test]
    #[ignore]
    fn heading_corpus_dump() {
        let Ok(path) = std::env::var("AUDIOBOOK_TEST_TXT") else {
            return;
        };
        let text = std::fs::read_to_string(&path).expect("read txt");
        // Approximate PDF pages: ~45 lines per page.
        let lines: Vec<&str> = text.lines().collect();
        let pages: Vec<String> = lines.chunks(45).map(|c| c.join("\n")).collect();
        let heads = chapter_headings(&pages);
        eprintln!("=== {} detected headings in {path} ===", heads.len());
        for (pg, h) in &heads {
            eprintln!("p{pg}: {h}");
        }
    }
}



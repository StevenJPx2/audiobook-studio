//! Turn detected boundaries into concrete, contiguous chapter page ranges,
//! and render each chapter's cleaned transcript to a .txt file.
use crate::error::AppResult;
use crate::model::{Boundary, Chapter};
use crate::pdf;
use std::path::{Path, PathBuf};

/// Convert sorted boundaries + total page count into inclusive page ranges.
/// Each chapter runs from its start page up to the page before the next one.
pub fn boundaries_to_chapters(boundaries: &[Boundary], page_count: usize) -> Vec<Chapter> {
    let mut chapters = Vec::new();
    for (i, b) in boundaries.iter().enumerate() {
        // Clamp start into [1, page_count] — the LLM can hallucinate pages past
        // the end of the book (e.g. start_page 16 on a 12-page PDF).
        let start = b.start_page.clamp(1, page_count.max(1));
        let end = if i + 1 < boundaries.len() {
            boundaries[i + 1].start_page.saturating_sub(1)
        } else {
            page_count
        };
        // Always keep end within the book and >= start so the range is valid.
        let end = end.min(page_count).max(start);
        chapters.push(Chapter {
            order: i + 1,
            title: b.title.trim().to_string(),
            start_page: start,
            end_page: end,
        });
    }
    chapters
}

/// Sanitize a chapter title into a safe file stem with an order prefix.
pub fn file_stem(ch: &Chapter) -> String {
    let mut t = ch.title.replace('?', "");
    for c in ['\\', '/', ':', '*', '"', '<', '>', '|'] {
        t = t.replace(c, "");
    }
    let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
    format!("{:02} - {}", ch.order, t)
}

/// Write each chapter's transcript to `<out_dir>/<NN - Title>.txt`.
/// Returns the list of (chapter, txt_path) in order.
pub fn write_transcripts(
    pages: &[String],
    chapters: &[Chapter],
    out_dir: &str,
    book_title: &str,
    author: &str,
) -> AppResult<Vec<(Chapter, PathBuf)>> {
    std::fs::create_dir_all(out_dir)?;
    let mut result = Vec::new();
    for ch in chapters {
        // Defensive: chapter page ranges come from LLM/boundary detection and
        // can be inverted or out of bounds (e.g. start>end, start>page_count).
        // Clamp both ends and ensure lo<=hi so we never slice out of range.
        let lo = ch.start_page.saturating_sub(1).min(pages.len());
        let hi = ch.end_page.min(pages.len()).max(lo);
        let slice = &pages[lo..hi];
        let body = pdf::clean_pages_to_transcript(slice);
        let full = format!("{book_title}\n{author}\n\n{}\n\n{body}\n", ch.title);
        let stem = file_stem(ch);
        let path = Path::new(out_dir).join(format!("{stem}.txt"));
        std::fs::write(&path, full)?;
        result.push((ch.clone(), path));
    }
    Ok(result)
}

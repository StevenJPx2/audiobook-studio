//! Native PDF cover rendering via `pdfium-render` (replaces the PyMuPDF sidecar
//! `/cover` endpoint). Renders one page of a PDF to a JPEG. Best-effort: callers
//! should treat a failure as "no cover" rather than aborting the job.

use crate::error::{AppError, AppResult};
use pdfium_render::prelude::*;
use std::path::Path;

/// Resolve a Pdfium instance, trying a bundled/system libpdfium then the
/// library search path. Returns a readable error if none is found.
fn pdfium() -> AppResult<Pdfium> {
    // Try, in order: next to the executable, the current dir, then the system.
    // exe_dir() canonicalizes symlinks so a Homebrew bin->libexec link resolves
    // to the dir that actually holds libpdfium.dylib.
    let exe_dir = crate::bundle_env::exe_dir();
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Some(d) = &exe_dir {
        candidates.push(Pdfium::pdfium_platform_library_name_at_path(d));
    }
    candidates.push(Pdfium::pdfium_platform_library_name_at_path("./"));

    for path in candidates {
        if let Ok(b) = Pdfium::bind_to_library(&path) {
            return Ok(Pdfium::new(b));
        }
    }
    // Fall back to the system-installed library.
    Pdfium::bind_to_system_library()
        .map(Pdfium::new)
        .map_err(|e| AppError::Other(format!("libpdfium not found: {e}")))
}

/// Render `page` (1-indexed) of `pdf_path` to an in-memory RGB image at
/// `target_width` (height scales to aspect). Shared by cover export and OCR.
pub fn render_page_image(
    pdf_path: &str,
    page: u16,
    target_width: u16,
) -> AppResult<image::RgbImage> {
    let pdfium = pdfium()?;
    let doc = pdfium
        .load_pdf_from_file(pdf_path, None)
        .map_err(|e| AppError::Other(format!("open pdf: {e}")))?;

    let count = doc.pages().len();
    let idx = page.saturating_sub(1).min(count.saturating_sub(1));
    let page = doc
        .pages()
        .get(idx)
        .map_err(|e| AppError::Other(format!("get page: {e}")))?;

    let cfg = PdfRenderConfig::new().set_target_width(target_width as i32);
    let bitmap = page
        .render_with_config(&cfg)
        .map_err(|e| AppError::Other(format!("render: {e}")))?;
    Ok(bitmap.as_image().into_rgb8())
}

/// Render `page` (1-indexed) of `pdf_path` to a JPEG at `out_path`.
/// `target_width` controls render resolution (height scales to aspect).
pub fn render(pdf_path: &str, out_path: &str, page: u16, target_width: u16) -> AppResult<()> {
    let image = render_page_image(pdf_path, page, target_width)?;
    if let Some(parent) = Path::new(out_path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    image
        .save(out_path)
        .map_err(|e| AppError::Other(format!("save cover: {e}")))?;
    Ok(())
}

/// Render `page` (1-indexed) to PNG bytes in memory — used to feed OCR without
/// touching disk. A higher `target_width` improves OCR accuracy on dense text.
pub fn render_page_png_bytes(pdf_path: &str, page: u16, target_width: u16) -> AppResult<Vec<u8>> {
    let image = render_page_image(pdf_path, page, target_width)?;
    let mut buf: Vec<u8> = Vec::new();
    image
        .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
        .map_err(|e| AppError::Other(format!("encode png: {e}")))?;
    Ok(buf)
}

//! Bundle per-chapter MP3s into a single chaptered .m4b with metadata and an
//! optional cover (page 1 of the source PDF, rendered to JPEG by the sidecar
//! or supplied as cover.jpg in the output dir). Pure ffmpeg + stdlib.
use crate::error::{AppError, AppResult};
use crate::model::Chapter;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Probe an audio file's duration in milliseconds via ffprobe.
fn duration_ms(path: &Path) -> AppResult<u64> {
    let out = Command::new("ffprobe")
        .args([
            "-v", "error",
            "-show_entries", "format=duration",
            "-of", "csv=p=0",
        ])
        .arg(path)
        .output()
        .map_err(|e| AppError::Ffmpeg(format!("ffprobe spawn: {e}")))?;
    if !out.status.success() {
        return Err(AppError::Ffmpeg(format!(
            "ffprobe failed: {}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    let s = String::from_utf8_lossy(&out.stdout);
    let secs: f64 = s.trim().parse().unwrap_or(0.0);
    Ok((secs * 1000.0).round() as u64)
}

fn find_cover(out_dir: &Path) -> Option<PathBuf> {
    for name in ["cover.jpg", "cover.jpeg", "cover.png"] {
        let p = out_dir.join(name);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

/// Build the .m4b. `mp3s` must be in chapter order and align with `chapters`.
pub fn build_m4b(
    mp3s: &[PathBuf],
    chapters: &[Chapter],
    out_dir: &str,
    out_file: &Path,
    title: &str,
    author: &str,
    bitrate: &str,
) -> AppResult<()> {
    if mp3s.is_empty() {
        return Err(AppError::Ffmpeg("no MP3s to bundle".into()));
    }
    let out_dir_p = Path::new(out_dir);

    // ffmetadata with chapter markers.
    let mut meta = String::from(";FFMETADATA1\n");
    meta.push_str(&format!("title={title}\n"));
    meta.push_str(&format!("artist={author}\n"));
    meta.push_str(&format!("album={title}\n"));
    meta.push_str(&format!("album_artist={author}\n"));
    meta.push_str("genre=Audiobook\n");
    meta.push_str("media_type=2\n");

    let mut concat = String::new();
    let mut start: u64 = 0;
    for (i, mp3) in mp3s.iter().enumerate() {
        let dur = duration_ms(mp3)?;
        let end = start + dur;
        let title = chapters
            .get(i)
            .map(|c| c.title.clone())
            .unwrap_or_else(|| format!("Chapter {}", i + 1));
        meta.push_str("\n[CHAPTER]\nTIMEBASE=1/1000\n");
        meta.push_str(&format!("START={start}\nEND={end}\ntitle={title}\n"));
        // ffconcat: escape single quotes in paths.
        let safe = mp3.to_string_lossy().replace('\'', r"'\''");
        concat.push_str(&format!("file '{safe}'\n"));
        start = end;
    }

    let meta_path = out_dir_p.join(".meta.ffmeta");
    let list_path = out_dir_p.join(".concat.txt");
    std::fs::write(&meta_path, meta)?;
    std::fs::write(&list_path, concat)?;

    let cover = find_cover(out_dir_p);

    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-y", "-loglevel", "error"]);
    cmd.args(["-f", "concat", "-safe", "0", "-i"]).arg(&list_path);
    cmd.arg("-i").arg(&meta_path);
    if let Some(ref c) = cover {
        cmd.arg("-i").arg(c);
    }
    cmd.args(["-map", "0:a", "-map_metadata", "1"]);
    if cover.is_some() {
        cmd.args(["-map", "2:v", "-c:v", "copy", "-disposition:v:0", "attached_pic"]);
    }
    cmd.args(["-c:a", "aac", "-b:a", bitrate, "-movflags", "+faststart"]);
    cmd.arg(out_file);

    let status = cmd
        .status()
        .map_err(|e| AppError::Ffmpeg(format!("ffmpeg spawn: {e}")))?;

    let _ = std::fs::remove_file(&meta_path);
    let _ = std::fs::remove_file(&list_path);

    if !status.success() {
        return Err(AppError::Ffmpeg("ffmpeg m4b build failed".into()));
    }
    Ok(())
}

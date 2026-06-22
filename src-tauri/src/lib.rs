//! Audiobook Studio — Tauri backend.
//! Pipeline: PDF -> (local LLM) chapter boundaries -> split + transcripts ->
//! Kokoro TTS (Python sidecar) -> chaptered .m4b.
mod agent;
mod bundle;
mod commands;
mod error;
mod kokoro;
mod model;
mod pdf;
mod split;

use error::{AppError, AppResult};

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

/// Is a healthy Kokoro sidecar already listening on the sidecar port? A short,
/// dependency-free blocking probe (raw HTTP/1.0 GET over std TCP) so we don't
/// spawn a second process that fights for the port (EADDRINUSE / Errno 48).
/// `spawn_sidecar` runs before the async runtime, so this stays synchronous.
fn sidecar_already_healthy() -> bool {
    use std::io::{Read, Write};
    use std::net::{TcpStream, ToSocketAddrs};
    use std::time::Duration;

    // SIDECAR_BASE is "http://host:port"; pull off the authority for TcpStream.
    let authority = kokoro::SIDECAR_BASE
        .strip_prefix("http://")
        .unwrap_or("127.0.0.1:8765");
    let Ok(mut addrs) = authority.to_socket_addrs() else {
        return false;
    };
    let Some(addr) = addrs.next() else {
        return false;
    };

    let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(600)) else {
        // Nothing listening — the common, healthy "fresh start" case.
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(800)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(600)));
    let req = format!("GET /health HTTP/1.0\r\nHost: {authority}\r\nConnection: close\r\n\r\n");
    if stream.write_all(req.as_bytes()).is_err() {
        return false;
    }
    let mut resp = String::new();
    let _ = stream.read_to_string(&mut resp);
    // Must be a 2xx AND look like our health payload, so we don't mistake some
    // unrelated service squatting on the port for our sidecar.
    let ok_status = resp.starts_with("HTTP/1.") && resp.contains(" 200 ");
    ok_status && resp.contains("\"status\"") && resp.contains("\"ok\"")
}

/// Try to start the Kokoro Python sidecar from a local venv, unless one is
/// already listening. Non-fatal: if this fails the user can start it manually
/// and the app will pick it up via the /health poll.
fn spawn_sidecar() {
    // If a healthy sidecar already owns the port, reuse it. This makes startup
    // idempotent across `tauri dev` reloads and avoids the second process
    // failing to bind with `[Errno 48] address already in use`.
    if sidecar_already_healthy() {
        eprintln!("[sidecar] already running and healthy; reusing it");
        return;
    }

    // Resolve the sidecar dir relative to the app: ../sidecar from src-tauri in
    // dev, or alongside the binary in a bundle. We try a few candidates.
    let candidates = [
        std::env::var("AUDIOBOOK_SIDECAR_DIR")
            .ok()
            .map(std::path::PathBuf::from),
        std::env::current_dir().ok().map(|d| d.join("sidecar")),
        std::env::current_dir().ok().map(|d| d.join("../sidecar")),
    ];
    let sidecar_dir = candidates
        .into_iter()
        .flatten()
        .find(|p| p.join("kokoro_server.py").exists());

    let Some(dir) = sidecar_dir else {
        eprintln!("[sidecar] kokoro_server.py not found; start it manually if needed");
        return;
    };

    // Preferred launcher: `uv run` auto-syncs the env from pyproject.toml/uv.lock
    // and is move-proof (no baked-in absolute paths). Fall back to a synced
    // .venv, then a system python, so the app still starts without uv.
    let script_rel = "kokoro_server.py";
    let mut cmd = if std::process::Command::new("uv")
        .arg("--version")
        .output()
        .is_ok()
    {
        let mut c = std::process::Command::new("uv");
        c.args(["run", script_rel, "--warm"]);
        eprintln!("[sidecar] launching via `uv run`");
        c
    } else {
        let venv_py = dir.join(".venv/bin/python");
        let python = if venv_py.exists() {
            venv_py.to_string_lossy().to_string()
        } else if std::process::Command::new("python3.12")
            .arg("--version")
            .output()
            .is_ok()
        {
            "python3.12".to_string()
        } else {
            "python3".to_string()
        };
        eprintln!("[sidecar] uv not found; launching via {python}");
        let mut c = std::process::Command::new(python);
        c.arg(script_rel).arg("--warm");
        c
    };

    match cmd.current_dir(&dir).spawn() {
        Ok(_) => eprintln!("[sidecar] started in {}", dir.display()),
        Err(e) => eprintln!("[sidecar] failed to launch ({e}); start it manually"),
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    spawn_sidecar();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            commands::inspect_pdf,
            commands::list_models,
            commands::detect_chapters,
            commands::generate_audiobook,
            commands::reveal,
            commands::default_voice,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Audiobook Studio");
}

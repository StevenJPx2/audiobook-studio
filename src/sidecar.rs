//! Spawns and reuses the Kokoro Python sidecar (FastAPI). Pure std + process
//! management — no GUI dependency. Called once at startup.
use crate::kokoro;

/// Is a healthy Kokoro sidecar already listening on the sidecar port? A short,
/// dependency-free blocking probe (raw HTTP/1.0 GET over std TCP) so we don't
/// spawn a second process that fights for the port (EADDRINUSE / Errno 48).
fn sidecar_already_healthy() -> bool {
    use std::io::{Read, Write};
    use std::net::{TcpStream, ToSocketAddrs};
    use std::time::Duration;

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
        return false; // nothing listening — the common fresh-start case
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(800)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(600)));
    let req = format!("GET /health HTTP/1.0\r\nHost: {authority}\r\nConnection: close\r\n\r\n");
    if stream.write_all(req.as_bytes()).is_err() {
        return false;
    }
    let mut resp = String::new();
    let _ = stream.read_to_string(&mut resp);
    let ok_status = resp.starts_with("HTTP/1.") && resp.contains(" 200 ");
    ok_status && resp.contains("\"status\"") && resp.contains("\"ok\"")
}

/// Try to start the Kokoro Python sidecar, unless a healthy one is already
/// listening. Non-fatal: on failure the user can start it manually and the app
/// will pick it up via the /health poll before TTS.
pub fn spawn_sidecar() {
    if sidecar_already_healthy() {
        eprintln!("[sidecar] already running and healthy; reusing it");
        return;
    }

    // Resolve the sidecar dir: $AUDIOBOOK_SIDECAR_DIR, ./sidecar, or ../sidecar.
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

    // Preferred launcher: `uv run` auto-syncs the env and is move-proof. Fall
    // back to a synced .venv, then a system python.
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

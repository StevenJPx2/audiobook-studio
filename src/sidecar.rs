//! Early startup warm-up for the G2P sidecar.
//!
//! The G2P sidecar (`sidecar/g2p_server.py`) is a persistent misaki process that
//! `g2p.rs` spawns and owns on first use. Spawning it eagerly at app startup —
//! on a background thread — means it's loaded and warm by the time the user
//! kicks off a job, hiding the ~4s model-load (and any first-run spaCy model
//! download) behind the time spent choosing a PDF and reviewing chapters.
//!
//! Non-fatal: if warm-up fails here, `g2p.rs` will retry (and surface a real
//! error) when synthesis actually needs it.

/// Kick off G2P sidecar warm-up in the background. Returns immediately.
pub fn spawn_sidecar() {
    std::thread::spawn(|| match crate::g2p::ensure_ready() {
        Ok(()) => eprintln!("[g2p] sidecar ready"),
        Err(e) => eprintln!("[g2p] warm-up deferred ({e}); will retry on first use"),
    });
}

//! Client for the slim misaki G2P sidecar (`sidecar/g2p_server.py`).
//!
//! Spawns a persistent Python process (torch-free misaki, British, perceptron
//! POS + espeak fallback) and talks to it line-by-line over stdin/stdout:
//! send one line of text, read one line of Kokoro phonemes back.
//!
//! All phoneme *shaping* lives here (not in Python) so it's unit-testable:
//!   * abbreviation-aware sentence splitting (keeps "Dr. Smith" as one chunk),
//!   * ZWJ stripping (misaki glues diphthongs with U+200D, not in Kokoro vocab),
//!   * whitespace collapse (hyphen-compounds emit runs of spaces),
//!   * spurious mid-chunk period stripping (espeak's OOV fallback appends '.'),
//!   * pronunciation overrides for abbreviation-homographs (est=EST -> ɛst).
//!
//! Each chunk is kept well under Kokoro's ~510-token limit by splitting on
//! sentence boundaries before phonemizing.

use crate::error::{AppError, AppResult};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

/// Abbreviations ending in '.' that do NOT end a sentence.
const ABBREVS: &[&str] = &[
    "dr", "mr", "mrs", "ms", "prof", "st", "rev", "hon", "sr", "jr", "vs", "etc", "eg", "ie", "no",
    "vol", "fig", "p", "pp", "ch",
];

/// Pronunciation overrides applied to whole input words before phonemizing is
/// not possible (the dict maps them); instead we patch the *server's* lexicon.
/// For abbreviation-homographs we post-fix the phoneme of the standalone token.
/// Kept here as documentation; the server uses misaki's dict. The only override
/// we currently need ("est") is handled by sending a hint — see `phonemize`.
const OVERRIDES: &[(&str, &str)] = &[("est", "ˈɛst")];

struct Server {
    /// Held to keep the child process alive for the lifetime of the server;
    /// also used by `shutdown()` to reap it.
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

static SERVER: Mutex<Option<Server>> = Mutex::new(None);

/// Locate the sidecar directory containing `g2p_server.py` (dev/source path).
fn find_sidecar_dir() -> Option<PathBuf> {
    let candidates = [
        std::env::var("AUDIOBOOK_SIDECAR_DIR")
            .ok()
            .map(PathBuf::from),
        crate::bundle_env::exe_dir().map(|d| d.join("sidecar")),
        std::env::current_dir().ok().map(|d| d.join("sidecar")),
        std::env::current_dir().ok().map(|d| d.join("../sidecar")),
    ];
    candidates
        .into_iter()
        .flatten()
        .find(|p| p.join("g2p_server.py").exists())
}

/// Locate a frozen, standalone sidecar binary (`g2p_server`) — the form shipped
/// in the distributable `.app`, which needs no Python/uv at runtime. Checked
/// before the dev Python path. Resolution order:
///   1. `$AUDIOBOOK_SIDECAR_BIN` explicit override
///   2. `<exe>/../Resources/sidecar/g2p_server` (the .app bundle layout)
///   3. `<exe_dir>/sidecar/g2p_server` (binary-adjacent)
///   4. `<repo>/sidecar/dist/g2p_server` (local freeze output, for testing)
fn find_frozen_sidecar() -> Option<PathBuf> {
    let exe_dir = crate::bundle_env::exe_dir();
    let candidates = [
        std::env::var("AUDIOBOOK_SIDECAR_BIN").ok().map(PathBuf::from),
        exe_dir
            .as_ref()
            .map(|d| d.join("../Resources/sidecar/g2p_server")),
        exe_dir.as_ref().map(|d| d.join("sidecar/g2p_server")),
        std::env::current_dir()
            .ok()
            .map(|d| d.join("sidecar/dist/g2p_server")),
    ];
    candidates
        .into_iter()
        .flatten()
        .find(|p| p.is_file())
}

/// What `spawn_server` would launch, for diagnostics (e.g. `abs doctor`).
/// Mirrors the real resolution order so the report can't drift from behavior.
pub enum SidecarResolution {
    /// Frozen standalone binary (production .app form).
    Frozen(PathBuf),
    /// Dev Python source dir containing `g2p_server.py`.
    Dev(PathBuf),
    /// Nothing found.
    None,
}

/// Resolve the sidecar the same way `spawn_server` does, without launching it.
pub fn resolve_sidecar() -> SidecarResolution {
    if let Some(bin) = find_frozen_sidecar() {
        SidecarResolution::Frozen(bin)
    } else if let Some(dir) = find_sidecar_dir() {
        SidecarResolution::Dev(dir)
    } else {
        SidecarResolution::None
    }
}

fn spawn_server() -> AppResult<Server> {
    // Prefer the frozen standalone binary (production .app); fall back to the
    // dev Python path (`uv run` / `.venv`) so `cargo run` works unfrozen.
    let (mut cmd, work_dir) = if let Some(bin) = find_frozen_sidecar() {
        let work_dir = bin
            .parent()
            .map(|d| d.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        (Command::new(bin), work_dir)
    } else {
        let dir = find_sidecar_dir()
            .ok_or_else(|| AppError::Sidecar("g2p_server not found (no frozen binary or g2p_server.py)".into()))?;
        // Prefer `uv run` (auto-syncs the env); fall back to the synced .venv python.
        let c = if Command::new("uv").arg("--version").output().is_ok() {
            let mut c = Command::new("uv");
            c.args(["run", "g2p_server.py"]);
            c
        } else {
            let venv_py = dir.join(".venv/bin/python");
            let py = if venv_py.exists() {
                venv_py.to_string_lossy().to_string()
            } else {
                "python3".to_string()
            };
            let mut c = Command::new(py);
            c.arg("g2p_server.py");
            c
        };
        (c, dir)
    };

    let mut child = cmd
        .current_dir(&work_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| AppError::Sidecar(format!("spawn g2p_server: {e}")))?;

    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| AppError::Sidecar("no stdin".into()))?;
    let stdout = BufReader::new(
        child
            .stdout
            .take()
            .ok_or_else(|| AppError::Sidecar("no stdout".into()))?,
    );

    // Wait for the `__READY__` handshake on stderr (model load takes ~4s, plus a
    // possible first-run spaCy model download). Read on a thread with a deadline.
    if let Some(stderr) = child.stderr.take() {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let r = BufReader::new(stderr);
            for line in r.lines().map_while(Result::ok) {
                if line.starts_with("__READY__") {
                    let _ = tx.send(());
                    break;
                }
            }
        });
        // Generous: first run may download en_core_web_sm.
        rx.recv_timeout(Duration::from_secs(180))
            .map_err(|_| AppError::Sidecar("g2p_server did not become ready in time".into()))?;
    }

    Ok(Server { child, stdin, stdout })
}

/// Send one line of text to the server and read one phoneme line back.
fn server_call(srv: &mut Server, text: &str) -> AppResult<String> {
    let line = text.replace('\n', " ");
    srv.stdin
        .write_all(line.as_bytes())
        .and_then(|_| srv.stdin.write_all(b"\n"))
        .and_then(|_| srv.stdin.flush())
        .map_err(|e| AppError::Sidecar(format!("g2p write: {e}")))?;

    // Read lines, skipping the spaCy first-run banner that lands on stdout.
    loop {
        let mut out = String::new();
        let n = srv
            .stdout
            .read_line(&mut out)
            .map_err(|e| AppError::Sidecar(format!("g2p read: {e}")))?;
        if n == 0 {
            return Err(AppError::Sidecar("g2p_server closed unexpectedly".into()));
        }
        let out = out.trim_end_matches('\n');
        if out.starts_with('✔') || out.starts_with("You can now load") {
            continue; // spaCy model-download banner
        }
        return Ok(out.to_string());
    }
}

/// Ready the G2P server (spawn + warm). Call once before synthesis.
pub fn ensure_ready() -> AppResult<()> {
    let mut guard = SERVER.lock().map_err(|_| AppError::Sidecar("g2p lock poisoned".into()))?;
    if guard.is_none() {
        *guard = Some(spawn_server()?);
    }
    Ok(())
}

/// Convert text into a list of Kokoro phoneme chunks (one per sentence),
/// cleaned and ready to feed to the TTS model.
pub fn phonemize(text: &str) -> AppResult<Vec<String>> {
    ensure_ready()?;
    let mut guard = SERVER.lock().map_err(|_| AppError::Sidecar("g2p lock poisoned".into()))?;
    let srv = guard.as_mut().ok_or_else(|| AppError::Sidecar("g2p not ready".into()))?;

    let mut chunks = Vec::new();
    for sentence in split_sentences(text) {
        let raw = server_call(srv, &sentence)?;
        let cleaned = clean_phonemes(&raw);
        let cleaned = apply_overrides(&sentence, cleaned);
        // Kokoro caps the token sequence (~512 incl. start/end). A single long
        // "sentence" (verse, list, or punctuation-light prose) can exceed it, so
        // sub-split any over-budget phoneme string on word boundaries.
        for piece in cap_token_len(&cleaned, MAX_PHONEME_CHARS) {
            if !piece.trim().is_empty() {
                chunks.push(piece);
            }
        }
    }
    Ok(chunks)
}

/// Safe phoneme-character budget per chunk. The model tokenizes per character
/// and wraps with start/end ($) tokens; stay well under the 512 hard cap.
const MAX_PHONEME_CHARS: usize = 480;

/// Split a phoneme string into pieces no longer than `max` characters, breaking
/// on spaces (word boundaries) so we never cut mid-phoneme. A single "word"
/// longer than `max` (rare) is hard-split as a last resort.
fn cap_token_len(phonemes: &str, max: usize) -> Vec<String> {
    if phonemes.chars().count() <= max {
        return vec![phonemes.to_string()];
    }
    let mut out = Vec::new();
    let mut cur = String::new();
    for word in phonemes.split(' ') {
        let wlen = word.chars().count();
        let curlen = cur.chars().count();
        if !cur.is_empty() && curlen + 1 + wlen > max {
            out.push(std::mem::take(&mut cur));
        }
        if wlen > max {
            // pathological single token: hard-split by chars
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            let chars: Vec<char> = word.chars().collect();
            for ch in chars.chunks(max) {
                out.push(ch.iter().collect());
            }
            continue;
        }
        if cur.is_empty() {
            cur.push_str(word);
        } else {
            cur.push(' ');
            cur.push_str(word);
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Shut the server down cleanly (best-effort).
pub fn shutdown() {
    if let Ok(mut guard) = SERVER.lock() {
        if let Some(mut srv) = guard.take() {
            let _ = srv.stdin.write_all(b"__QUIT__\n");
            let _ = srv.stdin.flush();
            let _ = srv.child.wait();
        }
    }
}

// ---------------------------------------------------------------------------
// Pure helpers (unit-tested) — ported from the validated spike.
// ---------------------------------------------------------------------------

/// Split text into sentences on .?! while keeping abbreviations
/// ("Dr. Smith") intact and absorbing trailing closing quotes/parens.
fn split_sentences(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().map(|c| if c == '\n' { ' ' } else { c }).collect();
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        cur.push(ch);
        if matches!(ch, '.' | '?' | '!') {
            // Ellipsis ("..."): a '.' followed by another '.' is not a break.
            // (Also skip if the previous char was '.', i.e. mid-ellipsis.)
            if chars.get(i + 1) == Some(&'.') || (ch == '.' && i > 0 && chars[i - 1] == '.') {
                i += 1;
                continue;
            }
            // Last alphanumeric word ending at this '.' — is it an abbreviation?
            let last_word: String = cur
                .trim_end_matches(|c: char| !c.is_alphanumeric())
                .chars()
                .rev()
                .take_while(|c| c.is_alphanumeric())
                .collect::<String>()
                .chars()
                .rev()
                .collect();
            if ch == '.' && ABBREVS.contains(&last_word.to_lowercase().as_str()) {
                i += 1;
                continue; // keep "Dr. Smith" together; leave following space intact
            }
            // Sentence end: absorb only immediately-following closing quotes/parens.
            let mut j = i + 1;
            while j < chars.len() && matches!(chars[j], '"' | '\u{201d}' | ')' | '\'') {
                cur.push(chars[j]);
                j += 1;
            }
            let s = cur.trim();
            if !s.is_empty() {
                out.push(s.to_string());
            }
            cur.clear();
            i = j;
            continue;
        }
        i += 1;
    }
    let s = cur.trim();
    if !s.is_empty() {
        out.push(s.to_string());
    }
    out
}

/// Clean one raw phoneme string: strip ZWJ, collapse whitespace, and remove
/// spurious mid-chunk sentence periods (espeak OOV fallback appends them).
fn clean_phonemes(ipa: &str) -> String {
    let ipa = ipa.replace('\u{200d}', "");
    let ipa: String = ipa.split_whitespace().collect::<Vec<_>>().join(" ");
    // strip mid-chunk '.' (keep only a trailing terminator)
    let chars: Vec<char> = ipa.chars().collect();
    let mut kept = String::with_capacity(chars.len());
    for (k, &c) in chars.iter().enumerate() {
        if c == '.' {
            let rest_blank = chars[k + 1..].iter().all(|c| c.is_whitespace());
            if !rest_blank {
                continue;
            }
        }
        kept.push(c);
    }
    kept.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Apply abbreviation-homograph overrides. The misaki dict renders e.g. "est"
/// as the initialism "E-S-T"; if the source word is present and the dict
/// letter-spelled it, substitute the intended phoneme.
fn apply_overrides(source: &str, phonemes: String) -> String {
    let mut out = phonemes;
    let lower = source.to_lowercase();
    for (word, ph) in OVERRIDES {
        // crude word-presence check; only the "est=E-S-T" letter-spelling case
        if lower.split(|c: char| !c.is_alphanumeric()).any(|w| w == *word) {
            // the letter-spelled form for "est" is "ˌiːˌɛstˈiː"
            out = out.replace("ˌiːˌɛstˈiː", ph);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abbrev_keeps_dr_with_name() {
        let s = split_sentences("Dr. Eleanor Vance paused. Next sentence.");
        assert_eq!(s.len(), 2);
        assert!(s[0].starts_with("Dr. Eleanor"));
    }

    #[test]
    fn absorbs_closing_quote() {
        let s = split_sentences("\"...is lost.\" The next.");
        assert_eq!(s.len(), 2);
        assert!(!s[1].trim_start().starts_with('"'));
    }

    #[test]
    fn clean_strips_zwj_and_collapses_ws() {
        // "wɔ<ZWJ>təɹ   stɑnd" -> ZWJ removed, runs of spaces collapsed
        let got = clean_phonemes("wɔ\u{200d}təɹ   stɑnd");
        assert!(!got.contains('\u{200d}'));
        assert!(!got.contains("  "));
    }

    #[test]
    fn clean_strips_midchunk_period_keeps_trailing() {
        let got = clean_phonemes("abc. def.");
        assert_eq!(got, "abc def.");
    }

    #[test]
    fn override_fixes_est_spelling() {
        let got = apply_overrides("nemo sine vitio est.", "vˈɪtɪˌəʊ ˌiːˌɛstˈiː".into());
        assert_eq!(got, "vˈɪtɪˌəʊ ˈɛst");
    }

    #[test]
    fn cap_token_len_under_budget_is_unchanged() {
        let s = "a b c d";
        assert_eq!(cap_token_len(s, 480), vec![s.to_string()]);
    }

    #[test]
    fn cap_token_len_splits_on_word_boundaries() {
        // 100 "ab" words (~299 chars) capped at 50 -> several pieces, each <=50,
        // none cutting a word.
        let words = vec!["ab"; 100].join(" ");
        let pieces = cap_token_len(&words, 50);
        assert!(pieces.len() > 1);
        for p in &pieces {
            assert!(p.chars().count() <= 50, "piece over budget: {}", p.len());
            assert!(!p.starts_with(' ') && !p.ends_with(' '));
        }
        // round-trips back to the same words
        assert_eq!(pieces.join(" "), words);
    }
}

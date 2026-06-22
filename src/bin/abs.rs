//! `abs` — the headless Audiobook Studio CLI.
//!
//! A thin, scriptable/agent-friendly front end over the same pipeline the GUI
//! uses (`audiobook_studio::pipeline`). Conventions:
//!   * results go to STDOUT (plain text, or JSON with `--json`);
//!   * progress + diagnostics go to STDERR (so STDOUT stays pipeable);
//!   * non-interactive — every input is a flag, nothing prompts;
//!   * a non-zero exit code on failure, with the error on STDERR.
//!
//! Commands: `detect`, `generate`, `list-models`, `doctor`.

use audiobook_studio::model::{Chapter, GenerateRequest, Progress, VoiceConfig};
use audiobook_studio::{g2p, ocr, pipeline, sidecar};
use clap::{Parser, Subcommand};
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "abs",
    about = "Audiobook Studio CLI — PDF to chaptered .m4b with a local LLM + Kokoro TTS.",
    version
)]
struct Cli {
    /// Emit machine-readable JSON on stdout where applicable.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Detect chapter boundaries and print them (review/edit before generating).
    Detect {
        /// Path to the input PDF.
        #[arg(long)]
        pdf: String,
        /// Ollama model tag for boundary detection.
        #[arg(long, default_value = "gemma4:e2b-mlx")]
        model: String,
    },
    /// Generate a chaptered .m4b audiobook from a PDF.
    Generate {
        /// Path to the input PDF.
        #[arg(long)]
        pdf: String,
        /// Output directory for the final .m4b.
        #[arg(long)]
        out: String,
        /// Pre-detected/edited chapters JSON (from `abs detect --json`). When
        /// omitted, chapters are detected automatically first.
        #[arg(long)]
        chapters: Option<String>,
        /// Kokoro voice id.
        #[arg(long, default_value = "bm_george")]
        voice: String,
        /// Voice language code (e.g. b = British, a = American).
        #[arg(long, default_value = "b")]
        lang: String,
        /// Narration speed multiplier.
        #[arg(long, default_value_t = 1.0)]
        speed: f32,
        /// Book title (defaults to the PDF file stem).
        #[arg(long)]
        title: Option<String>,
        /// Author metadata.
        #[arg(long, default_value = "")]
        author: String,
        /// Ollama model for detection + polish.
        #[arg(long, default_value = "gemma4:e2b-mlx")]
        model: String,
        /// Disable the LLM transcript-polish pass.
        #[arg(long)]
        no_polish: bool,
    },
    /// List locally available Ollama models.
    ListModels,
    /// Check prerequisites (Ollama, models, ffmpeg, libpdfium, sidecar).
    Doctor,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    // Warm the G2P sidecar early only for commands that need TTS.
    if matches!(cli.command, Command::Generate { .. }) {
        sidecar::spawn_sidecar();
    }
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: failed to start runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    let code = rt.block_on(run(cli));
    // Clean teardown of the persistent G2P child, if we started it.
    g2p::shutdown();
    code
}

async fn run(cli: Cli) -> ExitCode {
    let json = cli.json;
    let result: Result<(), String> = match cli.command {
        Command::Detect { pdf, model } => cmd_detect(&pdf, &model, json).await,
        Command::Generate {
            pdf,
            out,
            chapters,
            voice,
            lang,
            speed,
            title,
            author,
            model,
            no_polish,
        } => {
            cmd_generate(GenerateArgs {
                pdf,
                out,
                chapters,
                voice,
                lang,
                speed,
                title,
                author,
                model,
                polish: !no_polish,
                json,
            })
            .await
        }
        Command::ListModels => cmd_list_models(json).await,
        Command::Doctor => cmd_doctor(json).await,
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Progress sink: human-readable lines to STDERR so STDOUT stays clean.
fn stderr_progress(p: Progress) {
    if p.total > 1 {
        eprintln!("[{}] {} ({}/{})", p.stage, p.message, p.current, p.total);
    } else {
        eprintln!("[{}] {}", p.stage, p.message);
    }
}

async fn cmd_detect(pdf: &str, model: &str, json: bool) -> Result<(), String> {
    let chapters = pipeline::detect_chapters(pdf, model, &stderr_progress)
        .await
        .map_err(|e| e.to_string())?;
    print_chapters(&chapters, json);
    Ok(())
}

struct GenerateArgs {
    pdf: String,
    out: String,
    chapters: Option<String>,
    voice: String,
    lang: String,
    speed: f32,
    title: Option<String>,
    author: String,
    model: String,
    polish: bool,
    json: bool,
}

async fn cmd_generate(a: GenerateArgs) -> Result<(), String> {
    let title = a.title.clone().unwrap_or_else(|| title_from_pdf(&a.pdf));

    // Chapters: load from JSON if provided, otherwise detect.
    let chapters = match &a.chapters {
        Some(path) => {
            let raw = std::fs::read_to_string(path)
                .map_err(|e| format!("read chapters {path}: {e}"))?;
            serde_json::from_str::<Vec<Chapter>>(&raw)
                .map_err(|e| format!("parse chapters {path}: {e}"))?
        }
        None => pipeline::detect_chapters(&a.pdf, &a.model, &stderr_progress)
            .await
            .map_err(|e| e.to_string())?,
    };
    if chapters.is_empty() {
        return Err("no chapters to generate".into());
    }

    let req = GenerateRequest {
        pdf_path: a.pdf.clone(),
        out_dir: a.out.clone(),
        chapters,
        voice: VoiceConfig {
            voice: a.voice.clone(),
            lang: a.lang.clone(),
            speed: a.speed,
        },
        book_title: title,
        author: a.author.clone(),
        polish: a.polish,
        polish_model: Some(a.model.clone()),
    };

    let out_path = pipeline::generate_audiobook(req, &stderr_progress)
        .await
        .map_err(|e| e.to_string())?;

    if a.json {
        println!("{}", serde_json::json!({ "output": out_path }));
    } else {
        println!("{out_path}");
    }
    Ok(())
}

async fn cmd_list_models(json: bool) -> Result<(), String> {
    let models = pipeline::list_models().await.map_err(|e| e.to_string())?;
    if json {
        println!(
            "{}",
            serde_json::to_string(&models).map_err(|e| e.to_string())?
        );
    } else if models.is_empty() {
        println!("(no models — is Ollama running?)");
    } else {
        for m in &models {
            println!("{m}");
        }
    }
    Ok(())
}

/// A single prerequisite check result.
struct Check {
    name: &'static str,
    ok: bool,
    detail: String,
}

async fn cmd_doctor(json: bool) -> Result<(), String> {
    let mut checks: Vec<Check> = Vec::new();

    // Ollama + models.
    let models = pipeline::list_models().await.unwrap_or_default();
    let ollama_up = !models.is_empty();
    checks.push(Check {
        name: "ollama",
        ok: ollama_up,
        detail: if ollama_up {
            format!("{} model(s) available", models.len())
        } else {
            "not reachable (start `ollama serve`)".into()
        },
    });
    let has = |needle: &str| models.iter().any(|m| m.contains(needle));
    checks.push(Check {
        name: "llm-model",
        ok: has("gemma4:e2b-mlx") || has("gemma4:e2b"),
        detail: if has("gemma4:e2b-mlx") {
            "gemma4:e2b-mlx present".into()
        } else if has("gemma4:e2b") {
            "gemma4:e2b present (mlx build recommended)".into()
        } else {
            "missing — `ollama pull gemma4:e2b-mlx`".into()
        },
    });
    let ocr_model = ocr::model();
    let ocr_ok = ocr::available(&ocr_model).await;
    checks.push(Check {
        name: "ocr-model",
        ok: ocr_ok,
        detail: if ocr_ok {
            format!("{ocr_model} present")
        } else {
            format!("missing (scanned-PDF OCR off) — `ollama pull {ocr_model}`")
        },
    });

    // ffmpeg on PATH.
    let ffmpeg_ok = which_exists("ffmpeg");
    checks.push(Check {
        name: "ffmpeg",
        ok: ffmpeg_ok,
        detail: if ffmpeg_ok {
            "found on PATH".into()
        } else {
            "missing — install ffmpeg".into()
        },
    });

    // libpdfium next to the executable, in cwd, or system.
    let pdfium_ok = libpdfium_present();
    checks.push(Check {
        name: "libpdfium",
        ok: pdfium_ok,
        detail: if pdfium_ok {
            "found".into()
        } else {
            "not found (cover/OCR render off) — place libpdfium next to the binary".into()
        },
    });

    // Sidecar dir (g2p_server.py) resolvable.
    let sidecar_ok = sidecar_dir_present();
    checks.push(Check {
        name: "g2p-sidecar",
        ok: sidecar_ok,
        detail: if sidecar_ok {
            "g2p_server.py found".into()
        } else {
            "sidecar not found — set AUDIOBOOK_SIDECAR_DIR".into()
        },
    });

    let all_ok = checks.iter().all(|c| c.ok);
    if json {
        let arr: Vec<_> = checks
            .iter()
            .map(|c| serde_json::json!({ "name": c.name, "ok": c.ok, "detail": c.detail }))
            .collect();
        println!(
            "{}",
            serde_json::json!({ "ok": all_ok, "checks": arr })
        );
    } else {
        for c in &checks {
            let mark = if c.ok { "ok  " } else { "FAIL" };
            println!("[{mark}] {:<12} {}", c.name, c.detail);
        }
    }
    // doctor reports status but is not itself a failure unless a hard
    // prerequisite (ffmpeg) is missing — that blocks all generation.
    if checks.iter().any(|c| c.name == "ffmpeg" && !c.ok) {
        return Err("ffmpeg is required for audio output".into());
    }
    Ok(())
}

fn print_chapters(chapters: &[Chapter], json: bool) {
    if json {
        match serde_json::to_string_pretty(chapters) {
            Ok(s) => println!("{s}"),
            Err(e) => eprintln!("error: serialize chapters: {e}"),
        }
    } else {
        println!("{:>3}  {:<48}  pages", "#", "title");
        for c in chapters {
            let title: String = c.title.chars().take(48).collect();
            println!("{:>3}  {:<48}  {}-{}", c.order, title, c.start_page, c.end_page);
        }
    }
}

fn title_from_pdf(pdf: &str) -> String {
    std::path::Path::new(pdf)
        .file_stem()
        .map(|s| s.to_string_lossy().replace(['_', '-'], " "))
        .unwrap_or_else(|| "Audiobook".into())
}

fn which_exists(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|dir| {
                let p = dir.join(bin);
                p.is_file()
            })
        })
        .unwrap_or(false)
}

fn libpdfium_present() -> bool {
    let name = if cfg!(target_os = "macos") {
        "libpdfium.dylib"
    } else if cfg!(target_os = "windows") {
        "pdfium.dll"
    } else {
        "libpdfium.so"
    };
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()));
    if let Some(d) = exe_dir {
        if d.join(name).exists() {
            return true;
        }
    }
    std::path::Path::new(name).exists()
}

fn sidecar_dir_present() -> bool {
    // Mirror the sidecar resolution order: env override, exe-relative, cwd.
    if let Some(dir) = std::env::var_os("AUDIOBOOK_SIDECAR_DIR") {
        if std::path::Path::new(&dir).join("g2p_server.py").exists() {
            return true;
        }
    }
    let candidates = ["sidecar/g2p_server.py", "../sidecar/g2p_server.py"];
    candidates.iter().any(|c| std::path::Path::new(c).exists())
}

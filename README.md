# Audiobook Studio

[![CI](https://github.com/StevenJPx2/audiobook-studio/actions/workflows/ci.yml/badge.svg)](https://github.com/StevenJPx2/audiobook-studio/actions/workflows/ci.yml)

Drop a book PDF → a **local LLM** finds the chapters → the PDF is split and
cleaned into transcripts → **Kokoro** narrates each chapter locally → you get a
single chaptered **`.m4b`** with cover art. Entirely local, **$0**.

- **UI:** native [egui/eframe](https://github.com/emilk/egui) — a single Rust binary, no web layer
- **Agent:** [Rig](https://github.com/0xPlaygrounds/rig) (`rig-core`) talking to **Ollama**
- **TTS:** Kokoro via a small Python **sidecar** (FastAPI)

---

## How it works

```
 PDF ──▶ extract pages (pdf-extract) ──▶ candidate headings (outline + text scan, TOC-deduped)
     ──▶ Rig + Ollama → chapter boundaries (JSON)  ──▶ contiguous page ranges
     ──▶ clean transcripts (.txt) ──▶ optional LLM polish ──▶ Kokoro sidecar → per-chapter MP3
     ──▶ ffmpeg → chaptered .m4b (+ cover, metadata)
```

The pipeline (`src/pipeline.rs`) runs on a background thread and reports
progress to the egui UI over a channel. Chapter detection is
reviewable/editable in the app before any audio is generated.

## Prerequisites

macOS / Apple Silicon only (MLX requirement).

| Tool | Why | Install (macOS) |
|------|-----|-----------------|
| **Rust** ≥ 1.77 | building the app/CLI | `brew install rust` or rustup |
| **Ollama** | local LLM (chapters, OCR, polish) | `brew install ollama`, then `ollama pull gemma4:e2b-mlx` (+ `glm-ocr:q8_0` for scanned PDFs) |
| **uv** | Python env for the G2P sidecar (dev only) | `brew install uv` |
| **ffmpeg** | audio assembly + .m4b encode | `brew install ffmpeg` |

> uv manages its own Python 3.12 toolchain — no system Python needed.
> espeak-ng is **not** required: the G2P sidecar bundles its own libespeak-ng via
> `espeakng_loader`. In a packaged build the sidecar is frozen to a standalone
> binary (no Python/uv at runtime).

## Install (Apple Silicon, via Homebrew)

GUI app:

```bash
brew install --cask StevenJPx2/audiobook-studio/audiobook-studio-app
```

CLI (`abs`):

```bash
brew install StevenJPx2/audiobook-studio/audiobook-studio
abs doctor        # checks Ollama, models, ffmpeg, libpdfium, sidecar
```

No codesigning prompt — Homebrew installs aren't quarantined. On first launch
the GUI downloads the MLX voice model (~312 MB) from HuggingFace, shown as a
setup step (the slim app ships without it).

Or from source (installs to `~/.cargo/bin`, with the frozen sidecar + libpdfium
placed beside it):

```bash
./scripts/install-cli.sh
```

CLI usage:

```bash
abs detect --pdf book.pdf --json > ch.json     # detect chapters (review/edit)
abs generate --pdf book.pdf --out ./out --chapters ch.json
```

## Setup

```bash
# 1) Kokoro sidecar env (one time) — uses uv
./scripts/setup-sidecar.sh        # runs `uv sync` from sidecar/pyproject.toml

# 2) Make sure Ollama is running with a model
ollama serve &                    # if not already running
ollama pull gemma4:e2b-mlx        # Metal-accelerated; default detection/polish model
```

## Run (dev)

```bash
cargo run
```

The app **auto-launches the G2P sidecar** via `uv run` on startup (it preloads
the British misaki pipeline, ~4 s) and reuses a running one. uv keeps the env in
sync from `uv.lock`, so a moved or freshly-cloned project just works. You can
also run the sidecar manually:

```bash
cd sidecar && uv run g2p_server.py
```

## Build (release)

```bash
cargo build --release    # GUI: target/release/audiobook-studio · CLI: target/release/abs
```

## Package a distributable `.app` / CLI

```bash
./scripts/build-app.sh                 # slim .app (model downloads on first run)
SLIM=0 ./scripts/build-app.sh          # full self-contained .app (embeds the 312M model)
./scripts/package-app-zip.sh           # slim .app zip + sha256 (for the Homebrew cask)
./scripts/package-cli-tarball.sh       # CLI tarball + sha256 (for the Homebrew formula)
```

See **BUNDLING.md** for the asset map, the frozen-sidecar approach, offline
model embedding, and the deferred codesign/notarize steps.

## Releasing

Tag-driven, automated to Homebrew. Bump `version` in `Cargo.toml`, then:

```bash
git tag v0.1.0 && git push --tags
```

A macOS-arm64 CI job builds the CLI tarball, publishes a GitHub Release, and
bumps the Homebrew tap formula. Commits follow **Conventional Commits** (see
CONTRIBUTING.md); the release flow + required `HOMEBREW_TAP_TOKEN` secret are
documented in BUNDLING.md.

## Cover art

By default the app **renders page 1 of the PDF** to `cover.jpg` in the output
folder (via the sidecar's `/cover` endpoint, PyMuPDF) and embeds it in the
`.m4b`. To override, drop your own `cover.jpg` / `.png` into the output folder
before generating — an existing cover is never overwritten.

## Transcript cleanup

Extracted PDF text is cleaned into TTS-friendly prose by a fast, deterministic
pass that:

- drops repeating **running heads/footers** (detected by frequency, so it's
  book-agnostic), including ones the extractor fuses onto body text;
- removes **footnote/endnote apparatus** and chapter-end **bibliography**
  entries;
- strips inline **superscript footnote markers** (e.g. `Church.”12`) while
  leaving real numbers (`Isaiah 7:9`, `1 Cor. 2:14`, years) intact;
- repairs hyphenation split across line breaks.

A second **LLM polish pass** then removes the artifacts that vary too much from
book to book to catch with rules — front-matter, cover/title-page boilerplate,
credits, epigraphs, and stray heading fragments. It is **on by default
(opt-out)**: untick **“Polish transcripts with the local model”** to skip it.
The pass is **deletion-only and verified** — each section is kept only if its
length stays within tolerance of the original (guarding against
rewrites/summaries), any failed or low-confidence section falls back to the
deterministic transcript, and the whole pass is skipped automatically when
Ollama is unreachable.

## Notes & knobs

- **Model pick:** the UI prefers `gemma4:e2b` for speed; any chat model works.
  Boundary detection asks for strict JSON and parses defensively.
- **Voices:** Kokoro `bm_george` (British) at 1× is the default. Switch voice
  and speed in the Review step.
- **Resumable:** if generation is interrupted, re-running skips chapters whose
  MP3 already exists in the output folder.
- **Sidecar port:** `127.0.0.1:8765`. Override the model host with
  `OLLAMA_HOST`, and the sidecar dir with `AUDIOBOOK_SIDECAR_DIR`.

## Layout

```
audiobook-studio/
├─ Cargo.toml           # single Rust binary (eframe)
├─ src/
│  ├─ main.rs           # eframe entry; spawns sidecar
│  ├─ app.rs            # egui UI + screen state machine
│  ├─ pipeline.rs       # GUI-agnostic pipeline (progress via callback)
│  ├─ sidecar.rs        # Kokoro sidecar launcher / health probe
│  ├─ agent.rs          # Rig + Ollama boundary detection + transcript polish
│  ├─ pdf.rs            # text extraction, heading scan, TTS cleaning
│  ├─ split.rs          # boundaries → page ranges → transcripts
│  ├─ kokoro.rs         # sidecar HTTP client
│  ├─ bundle.rs         # ffmpeg .m4b builder
│  └─ model.rs          # shared types
└─ sidecar/
   ├─ kokoro_server.py  # FastAPI TTS service
   ├─ pyproject.toml    # deps (managed by uv)
   └─ uv.lock           # pinned, reproducible env
```

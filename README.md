# Audiobook Studio

[![CI](https://github.com/StevenJPx2/audiobook-studio/actions/workflows/ci.yml/badge.svg)](https://github.com/StevenJPx2/audiobook-studio/actions/workflows/ci.yml)

Drop a book PDF → a **local LLM** finds the chapters → the PDF is split and
cleaned into transcripts → **Kokoro** narrates each chapter locally → you get a
single chaptered **`.m4b`** with cover art. Entirely local, **$0**.

- **Shell:** Tauri 2 (Rust backend + TypeScript frontend)
- **Agent:** [Rig](https://github.com/0xPlaygrounds/rig) (`rig-core`) talking to **Ollama**
- **TTS:** Kokoro via a small Python **sidecar** (FastAPI)
- **Design:** Vercel's [Geist](https://vercel.com/design.md) design system (light + dark)

---

## How it works

```
 PDF ──▶ extract pages (pdf-extract) ──▶ candidate headings (outline or text scan)
     ──▶ Rig + Ollama → chapter boundaries (JSON)  ──▶ contiguous page ranges
     ──▶ clean transcripts (.txt, TTS-friendly)    ──▶ Kokoro sidecar → per-chapter MP3
     ──▶ ffmpeg → chaptered .m4b (+ cover, metadata)
```

The Rust backend orchestrates the pipeline and streams progress to the UI over
the `audiobook://progress` event. Chapter detection is reviewable/editable in
the UI before any audio is generated.

## Prerequisites

| Tool | Why | Install (macOS) |
|------|-----|-----------------|
| **Rust** ≥ 1.77 | Tauri backend | `brew install rust` or rustup |
| **Node** ≥ 18 | frontend build | `brew install node` |
| **Ollama** | local LLM | `brew install ollama` then `ollama pull gemma4:e2b` |
| **uv** | Python env for sidecar | `brew install uv` |
| **ffmpeg + espeak-ng** | audio + phonemes | `brew install ffmpeg espeak-ng` |

> uv manages its own Python 3.12 toolchain, so you don't need a system Python.

## Setup

```bash
# 1) Frontend deps
npm install

# 2) Kokoro sidecar env (one time) — uses uv
./scripts/setup-sidecar.sh        # runs `uv sync` from sidecar/pyproject.toml

# 3) Make sure Ollama is running with a model
ollama serve &                    # if not already running
ollama pull gemma4:e2b            # fast; or use gemma4:latest for higher quality
```

## Run (dev)

```bash
npm run tauri dev
```

The app **auto-launches the Kokoro sidecar** via `uv run` on startup (it
preloads the British pipeline, so the first launch takes ~20–40 s). uv keeps
the env in sync from `uv.lock`, so a moved or freshly-cloned project just works.
You can also run it manually:

```bash
cd sidecar && uv run kokoro_server.py --warm
```

## Build (release)

```bash
npm run tauri build
```

## Cover art

By default the app **renders page 1 of the PDF** to `cover.jpg` in the output
folder (via the sidecar's `/cover` endpoint, PyMuPDF) and embeds it in the
`.m4b`. To override, drop your own `cover.jpg` / `.png` into the output folder
before generating — an existing cover is never overwritten.

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
├─ src/                 # TypeScript frontend (Geist UI)
│  ├─ main.ts           # state machine + views
│  ├─ api.ts            # typed bridge to Rust commands
│  └─ styles.css        # Geist design tokens
├─ src-tauri/
│  └─ src/
│     ├─ lib.rs         # app builder + sidecar launcher
│     ├─ commands.rs    # Tauri commands + pipeline orchestration
│     ├─ agent.rs       # Rig + Ollama boundary detection
│     ├─ pdf.rs         # text extraction + TTS cleaning
│     ├─ split.rs       # boundaries → page ranges → transcripts
│     ├─ kokoro.rs      # sidecar HTTP client
│     ├─ bundle.rs      # ffmpeg .m4b builder
│     └─ model.rs       # shared types
└─ sidecar/
   ├─ kokoro_server.py  # FastAPI TTS service
   ├─ pyproject.toml    # deps (managed by uv)
   └─ uv.lock           # pinned, reproducible env
```

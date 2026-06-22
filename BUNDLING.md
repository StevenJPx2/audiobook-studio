# Bundling Audiobook Studio into a distributable macOS `.app`

Target: a self-contained, signable `Audiobook Studio.app` for Apple Silicon
(MLX). No Python/uv/Homebrew/Ollama-model assumptions at runtime except the
user's own Ollama install (LLM/OCR are optional and degrade gracefully).

Status: **scaffolding stage** — produce an *unsigned* `.app` that launches
locally. Codesign + notarize are deferred (need a Developer ID identity).

## Runtime asset map

| Asset | ~Size | Dev location | Runtime lookup (code) | Bundle destination |
|-------|------:|--------------|------------------------|--------------------|
| `audiobook-studio` (GUI) | — | `target/release/` | — | `Contents/MacOS/audiobook-studio` |
| `abs` (CLI) | — | `target/release/` | — | `Contents/MacOS/abs` |
| `libpdfium.dylib` | 7.4M | next to exe / cwd | `cover.rs`: `current_exe()/` then `./` | `Contents/MacOS/libpdfium.dylib` |
| G2P sidecar (frozen) | ~150M | `sidecar/dist/g2p_server` | `g2p.rs`: `current_exe()/../sidecar` etc. | `Contents/Resources/sidecar/g2p_server` |
| MLX Kokoro model | 312M | `~/.cache/huggingface/hub/models--prince-canuma--Kokoro-82M` | `voice-tts` HF download | `Contents/Resources/hf-cache/` + `HF_HUB_OFFLINE=1` |

`en_core_web_sm` (spaCy) and espeak-ng are **inside** the frozen sidecar (see
below), not separate bundle items.

## Sidecar: freeze to a standalone binary (no Python at runtime)

The signed app can't use `uv run` (no uv on users' machines, can't re-sync) and
a relocated venv is painful to deep-sign. Instead we freeze `g2p_server.py`
into one executable with **PyInstaller** (`scripts/freeze-sidecar.sh`).

Data/libs PyInstaller must collect (verified present in the dev venv):
- `espeakng_loader` — **bundles its own `libespeak-ng` + espeak-ng-data**
  (misaki calls `espeakng_loader.get_library_path()/get_data_path()`), so the
  frozen binary needs NO system espeak-ng. `--collect-all espeakng_loader`.
- `misaki` — lexicon/dictionary data. `--collect-all misaki`.
- `en_core_web_sm` + `spacy` — model package + spaCy data.
  `--collect-all en_core_web_sm --collect-all spacy`.
- `phonemizer` (phonemizer_fork) — `--collect-data phonemizer`.

Output: `sidecar/dist/g2p_server` (single binary). The line protocol is
unchanged, so `g2p.rs` just execs it instead of `python g2p_server.py`.

### `g2p.rs` change (pending)
Prefer, in order: `$AUDIOBOOK_SIDECAR_BIN` → `current_exe()/../Resources/sidecar/g2p_server`
→ frozen binary next to a dev build → existing dev path (`uv run` / `.venv`).
Keep the dev path so `cargo run` still works without freezing.

## MLX model offline

`voice-tts` downloads `prince-canuma/Kokoro-82M` from HuggingFace on first use.
For a bundle:
1. Pre-populate an HF cache under `Contents/Resources/hf-cache/`.
2. At startup set `HF_HOME=<bundle>/Resources/hf-cache` and `HF_HUB_OFFLINE=1`
   (in `main.rs`/`abs.rs` before any TTS call) so it never hits the network.

## `.app` layout

```
Audiobook Studio.app/
└─ Contents/
   ├─ Info.plist
   ├─ MacOS/
   │  ├─ audiobook-studio        # GUI (CFBundleExecutable)
   │  ├─ abs                     # CLI
   │  └─ libpdfium.dylib
   └─ Resources/
      ├─ AppIcon.icns
      ├─ sidecar/g2p_server      # frozen sidecar
      └─ hf-cache/...            # pre-cached MLX model
```

`scripts/build-app.sh` (pending): `cargo build --release`, freeze the sidecar,
assemble the tree above, write `Info.plist`, copy the pre-cached model. Produces
an **unsigned** `.app`.

## Codesign + notarize (DEFERRED — needs Developer ID)

When ready, with a "Developer ID Application: <name> (<TEAMID>)" identity:
1. `codesign --deep --force --options runtime --timestamp` the nested binaries
   (sidecar, libpdfium, abs) then the app, with a hardened-runtime
   entitlements plist (allow-jit / allow-unsigned-executable-memory may be
   needed for MLX/Python-frozen code).
2. `xcrun notarytool submit` (app-specific password or App Store Connect API
   key) → `xcrun stapler staple`.
3. Verify: `spctl -a -vv` and `codesign --verify --deep --strict`.

These steps need secrets, so they live outside the repo and are run manually.

## Known risks / open items

- PyInstaller + spaCy/misaki data files sometimes need explicit hidden-imports;
  validate the frozen binary prints `__READY__` and phonemizes a test line.
- Frozen-binary first-run is slower (unpacks to a temp dir); acceptable since
  the sidecar is warmed at startup.
- Hardened runtime may block the frozen Python/MLX JIT — entitlements TBD at
  signing time.
- Two binaries (GUI + abs) must both be signed if both ship in the app.

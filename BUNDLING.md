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

`scripts/build-app.sh`: `cargo build --release`, freeze the sidecar, assemble
the tree above, write `Info.plist`, (slim) skip or (SLIM=0) copy the model,
then **ad-hoc codesign the whole bundle** (`codesign --force --deep --sign -`).

The ad-hoc re-sign is required, not cosmetic: the Rust executable ships with a
linker/ad-hoc signature that seals only itself. Adding the sibling files
(`abs`, `libpdfium.dylib`, `Resources/sidecar`, model) invalidates that seal, so
macOS reports the app as **"damaged and can't be opened."** Re-signing the
assembled bundle ad-hoc (no Developer ID required) seals the final contents.
Homebrew-cask installs aren't quarantined, so ad-hoc is enough to launch.

## Release process (tag → CI → Homebrew)

Releases are tag-driven (`.github/workflows/release.yml`), no crates.io publish.

1. Bump `version` in `Cargo.toml`, commit (Conventional Commits — see
   CONTRIBUTING.md), and tag: `git tag v0.1.0 && git push --tags`.
2. On the tag, a `macos-14` (arm64) runner:
   - fetches `libpdfium` (bblanchon arm64), runs
     `scripts/package-cli-tarball.sh` (release `abs` + frozen sidecar),
   - creates the GitHub Release and uploads
     `abs-<ver>-macos-arm64.tar.gz` + `.sha256`,
   - bumps the Homebrew tap formula (`url` + `sha256`) via
     `bump-homebrew-formula-action`.
3. Users `brew upgrade audiobook-studio`.

**Required secret:** `HOMEBREW_TAP_TOKEN` — a PAT with `contents:write` on
`StevenJPx2/homebrew-audiobook-studio`. If absent, the build + GitHub Release
still run; only the formula bump is skipped. The canonical formula lives in the
tap at `Formula/audiobook-studio.rb`; `packaging/homebrew/audiobook-studio.rb`
here is the seed copy.

## Distribution & codesigning

Both the CLI and the GUI ship through the Homebrew tap
(`StevenJPx2/homebrew-audiobook-studio`):

- **CLI** — `brew install StevenJPx2/audiobook-studio/audiobook-studio`
  (formula, prebuilt tarball).
- **GUI** — `brew install --cask StevenJPx2/audiobook-studio/audiobook-studio-app`
  (cask, slim `.app` zip; the model downloads on first launch).

**Codesigning is NOT required.** Homebrew installs via curl, which does not set
the `com.apple.quarantine` flag, so unsigned binaries/apps launch without a
Gatekeeper prompt for `brew install` / `brew install --cask` users. This is why
we distribute via the tap rather than a browser download (a browser-downloaded
unsigned `.app` *would* be quarantined).

Optional, only if you later distribute the `.app` as a direct download (DMG/zip
from a website): sign + notarize with a Developer ID identity using
`packaging/entitlements.plist` (hardened runtime; allow-jit / unsigned-exec
memory for MLX + the frozen Python sidecar), then `xcrun notarytool submit` →
`xcrun stapler staple`. Not needed for the Homebrew path.

## App icon

`packaging/icon/AppIcon.svg` → `scripts/make-icon.sh` → `packaging/AppIcon.icns`
(all 10 macOS sizes) + `packaging/icon/window-512.png`. `build-app.sh` copies
the icns into `Contents/Resources` (Info.plist `CFBundleIconFile = AppIcon`);
`main.rs` sets the running window/dock icon via `ViewportBuilder::with_icon`.

> If a *reinstalled/replaced* app shows the old or a generic icon, that's the
> macOS icon cache, not a bundle problem. Refresh with:
> `lsregister -f "/Applications/Audiobook Studio.app"` then `killall Dock Finder`
> (or log out/in). Fresh installs are unaffected.

## Known risks / open items

- PyInstaller + spaCy/misaki data files sometimes need explicit hidden-imports;
  validate the frozen binary prints `__READY__` and phonemizes a test line.
- Frozen-binary first-run is slower (unpacks to a temp dir); acceptable since
  the sidecar is warmed at startup.
- Hardened runtime may block the frozen Python/MLX JIT — entitlements TBD at
  signing time.
- Two binaries (GUI + abs) must both be signed if both ship in the app.

#!/usr/bin/env bash
# Assemble an (UNSIGNED) "Audiobook Studio.app" for Apple Silicon. See BUNDLING.md.
#
# Produces dist-app/Audiobook Studio.app that launches locally. Codesign +
# notarize are a separate, deferred step (needs a Developer ID identity).
#
# Steps: release build -> freeze sidecar (if needed) -> assemble bundle tree
# (binaries, libpdfium, frozen sidecar, Info.plist, pre-cached MLX model).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

APP="dist-app/Audiobook Studio.app"
MACOS="$APP/Contents/MacOS"
RES="$APP/Contents/Resources"
PDFIUM_SRC="${PDFIUM_SRC:-$ROOT/vendor/libpdfium.dylib}"
MODEL_REPO_DIR="models--prince-canuma--Kokoro-82M"
HF_HUB_SRC="${HF_HUB_SRC:-$HOME/.cache/huggingface/hub}"

echo "==> 1/5  Release build (GUI + abs)"
cargo build --release --bin audiobook-studio --bin abs

echo "==> 2/5  Freeze G2P sidecar (if missing)"
if [[ ! -x "$ROOT/sidecar/dist/g2p_server" ]]; then
  "$ROOT/scripts/freeze-sidecar.sh"
else
  echo "    using existing sidecar/dist/g2p_server"
fi

echo "==> 3/5  Assemble bundle tree"
rm -rf "$APP"
mkdir -p "$MACOS" "$RES/sidecar"
cp "target/release/audiobook-studio" "$MACOS/"
cp "target/release/abs" "$MACOS/"
cp "packaging/Info.plist" "$APP/Contents/Info.plist"
cp "$ROOT/sidecar/dist/g2p_server" "$RES/sidecar/g2p_server"

# libpdfium next to the executable (cover.rs / OCR look here).
if [[ -f "$PDFIUM_SRC" ]]; then
  cp "$PDFIUM_SRC" "$MACOS/libpdfium.dylib"
else
  echo "    WARN: libpdfium not found at $PDFIUM_SRC (cover/OCR render will be off)" >&2
fi

# App icon (optional).
if [[ -f "packaging/AppIcon.icns" ]]; then
  cp "packaging/AppIcon.icns" "$RES/AppIcon.icns"
else
  echo "    note: packaging/AppIcon.icns absent (no custom icon)"
fi

# SLIM=1 (default) ships WITHOUT the 312M model — the app downloads it from
# HuggingFace on first run, showing a setup screen. SLIM=0 embeds it for a
# fully-offline, larger .app.
SLIM="${SLIM:-1}"
if [[ "$SLIM" == "1" ]]; then
  echo "==> 4/5  Slim build — model NOT embedded (downloads on first run)"
else
  echo "==> 4/5  Embed MLX Kokoro model (offline, SLIM=0)"
  SRC_MODEL="$HF_HUB_SRC/$MODEL_REPO_DIR"
  if [[ -d "$SRC_MODEL" ]]; then
    mkdir -p "$RES/hf-cache/hub"
    cp -R "$SRC_MODEL" "$RES/hf-cache/hub/"
    echo "    embedded $(du -sh "$RES/hf-cache" | cut -f1) model cache"
  else
    echo "    WARN: model cache not found at $SRC_MODEL." >&2
    echo "          Run a generate once to populate ~/.cache/huggingface, or set HF_HUB_SRC." >&2
  fi
fi

echo "==> 5/6  Ad-hoc codesign (bundle consistency)"
# The Rust binary ships with a stale ad-hoc/linker signature that seals ONLY
# the executable. After adding sibling files (abs, libpdfium, sidecar, model)
# that signature no longer matches the bundle, so macOS reports the app as
# "damaged". Re-sign the WHOLE bundle ad-hoc (no Developer ID needed) so the
# signature seals the final contents. Homebrew-cask installs aren't quarantined,
# so an ad-hoc signature is sufficient to launch without a Gatekeeper prompt.
codesign --force --deep --sign - "$APP"
codesign --verify --deep --strict "$APP"
echo "    signed + verified (ad-hoc)"

echo "==> 6/6  Done (ad-hoc signed, unnotarized)"
SIZE="$(du -sh "$APP" | cut -f1)"
echo "    $APP  ($SIZE)"
echo
echo "Launch:   open \"$APP\""
echo "CLI:      \"$MACOS/abs\" doctor"
echo
echo "Optional (only for direct .app download, NOT for the Homebrew cask) — see"
echo "BUNDLING.md: Developer ID codesign + notarytool + stapler."

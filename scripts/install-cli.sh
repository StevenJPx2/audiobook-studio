#!/usr/bin/env bash
# Install the `abs` CLI to ~/.cargo/bin so it works from any directory.
#
# `abs generate` needs the frozen G2P sidecar and libpdfium findable relative to
# the executable (see g2p.rs / cover.rs resolution). So besides installing the
# binary we place those assets next to it:
#   ~/.cargo/bin/abs
#   ~/.cargo/bin/libpdfium.dylib
#   ~/.cargo/bin/sidecar/g2p_server      (matches g2p.rs exe-adjacent lookup)
#
# The MLX Kokoro model is used from the normal ~/.cache/huggingface (not bundled
# here — this is a dev-machine install, not the distributable .app).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

BIN_DIR="${CARGO_HOME:-$HOME/.cargo}/bin"
PDFIUM_SRC="${PDFIUM_SRC:-$ROOT/vendor/libpdfium.dylib}"
SIDECAR_SRC="$ROOT/sidecar/dist/g2p_server"

echo "==> Installing abs (release) to $BIN_DIR"
cargo install --path . --bin abs --force

echo "==> Placing libpdfium next to abs"
if [[ -f "$PDFIUM_SRC" ]]; then
  cp "$PDFIUM_SRC" "$BIN_DIR/libpdfium.dylib"
else
  echo "    WARN: $PDFIUM_SRC missing (cover/OCR render will be off)" >&2
fi

echo "==> Placing frozen sidecar next to abs"
if [[ ! -x "$SIDECAR_SRC" ]]; then
  echo "    sidecar not frozen yet; running freeze-sidecar.sh"
  "$ROOT/scripts/freeze-sidecar.sh"
fi
mkdir -p "$BIN_DIR/sidecar"
cp "$SIDECAR_SRC" "$BIN_DIR/sidecar/g2p_server"

echo
echo "Installed. Verify with:  abs doctor"

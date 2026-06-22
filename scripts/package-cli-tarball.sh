#!/usr/bin/env bash
# Build a self-contained `abs` CLI tarball for distribution (Homebrew formula
# source). Contents:
#   abs                     release binary
#   libpdfium.dylib         native PDF render (cover/OCR)
#   sidecar/g2p_server      PyInstaller-frozen G2P sidecar
#
# The MLX Kokoro model is NOT bundled (312M) — it downloads from HuggingFace on
# first `abs generate` (online). Everything else is offline/self-contained.
#
# Output: dist-cli/abs-<version>-macos-arm64.tar.gz  (+ .sha256 for the formula)
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

VERSION="${1:-$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')}"
ARCH="macos-arm64"
NAME="abs-${VERSION}-${ARCH}"
STAGE="dist-cli/$NAME"
PDFIUM_SRC="${PDFIUM_SRC:-$ROOT/vendor/libpdfium.dylib}"
SIDECAR_SRC="$ROOT/sidecar/dist/g2p_server"

echo "==> Release build (abs)"
cargo build --release --bin abs

echo "==> Freeze sidecar (if missing)"
[[ -x "$SIDECAR_SRC" ]] || "$ROOT/scripts/freeze-sidecar.sh"

echo "==> Stage $STAGE"
rm -rf "$STAGE"
mkdir -p "$STAGE/sidecar"
cp "target/release/abs" "$STAGE/abs"
cp "$SIDECAR_SRC" "$STAGE/sidecar/g2p_server"
if [[ -f "$PDFIUM_SRC" ]]; then
  cp "$PDFIUM_SRC" "$STAGE/libpdfium.dylib"
else
  echo "    WARN: $PDFIUM_SRC missing (cover/OCR render off in tarball)" >&2
fi

echo "==> Tar + checksum"
TARBALL="dist-cli/${NAME}.tar.gz"
( cd dist-cli && tar -czf "${NAME}.tar.gz" "$NAME" )
shasum -a 256 "$TARBALL" | tee "${TARBALL}.sha256"

echo
echo "Tarball: $TARBALL"
echo "Upload it to a GitHub Release tag v${VERSION}, then set url+sha256 in the"
echo "Homebrew formula (packaging/homebrew/audiobook-studio.rb)."

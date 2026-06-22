#!/usr/bin/env bash
# Build the slim "Audiobook Studio.app" and zip it for Homebrew Cask
# distribution. The MLX voice model is NOT embedded; the app downloads it from
# HuggingFace on first run (showing a setup screen).
#
# Output: dist-cli/AudiobookStudio-<version>-macos-arm64.zip (+ .sha256)
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

VERSION="${1:-$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')}"
APP="dist-app/Audiobook Studio.app"
OUT="dist-cli/AudiobookStudio-${VERSION}-macos-arm64.zip"

echo "==> Build slim .app (no embedded model)"
SLIM=1 "$ROOT/scripts/build-app.sh" "$VERSION"

echo "==> Zip .app (ditto preserves bundle attrs)"
mkdir -p dist-cli
rm -f "$OUT"
( cd dist-app && ditto -c -k --sequesterRsrc --keepParent "Audiobook Studio.app" "../$OUT" )

echo "==> Checksum"
shasum -a 256 "$OUT" | tee "${OUT}.sha256"
echo
echo "Zip: $OUT ($(du -h "$OUT" | cut -f1))"
echo "Upload to the GitHub Release; the Cask url+sha256 are bumped by CI."

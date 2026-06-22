#!/usr/bin/env bash
# Render packaging/icon/AppIcon.svg into:
#   packaging/AppIcon.icns        macOS app icon (build-app.sh copies this)
#   packaging/icon/window-512.png window/dock icon for eframe (with_icon)
#
# Requires rsvg-convert + iconutil (macOS).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SVG="$ROOT/packaging/icon/AppIcon.svg"
ICONSET="$(mktemp -d)/AppIcon.iconset"
mkdir -p "$ICONSET"

for t in rsvg-convert iconutil; do
  command -v "$t" >/dev/null 2>&1 || { echo "missing: $t" >&2; exit 1; }
done

# macOS iconset requires these exact names/sizes (1x + 2x from 16..512).
render() { rsvg-convert -w "$2" -h "$2" "$SVG" -o "$ICONSET/$1"; }
render icon_16x16.png        16
render icon_16x16@2x.png     32
render icon_32x32.png        32
render icon_32x32@2x.png     64
render icon_128x128.png      128
render icon_128x128@2x.png   256
render icon_256x256.png      256
render icon_256x256@2x.png   512
render icon_512x512.png      512
render icon_512x512@2x.png   1024

iconutil -c icns "$ICONSET" -o "$ROOT/packaging/AppIcon.icns"
rsvg-convert -w 512 -h 512 "$SVG" -o "$ROOT/packaging/icon/window-512.png"

echo "Wrote packaging/AppIcon.icns ($(du -h "$ROOT/packaging/AppIcon.icns" | cut -f1))"
echo "Wrote packaging/icon/window-512.png"

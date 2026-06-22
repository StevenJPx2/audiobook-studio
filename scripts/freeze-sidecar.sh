#!/usr/bin/env bash
# Freeze the G2P sidecar (g2p_server.py) into a single standalone binary so the
# distributable .app needs no Python / uv / venv at runtime. See BUNDLING.md.
#
# Output: sidecar/dist/g2p_server  (the .app build script copies this into
# Contents/Resources/sidecar/).
#
# Requires: uv. Installs the build-only `freeze` group (PyInstaller) on demand.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SIDE="$ROOT/sidecar"
cd "$SIDE"

if ! command -v uv >/dev/null 2>&1; then
  echo "uv not found. Install it: brew install uv" >&2
  exit 1
fi

echo "Syncing sidecar env (incl. freeze group)…"
uv sync --group freeze

echo "Freezing g2p_server.py with PyInstaller…"
# Data/lib collection (verified against the dev venv):
#   espeakng_loader — bundles libespeak-ng + espeak-ng-data (no system espeak)
#   misaki          — lexicon/dictionary data
#   en_core_web_sm  — spaCy English model package
#   spacy           — spaCy runtime data
#   phonemizer      — fork data files
#   language_tags / csvw / segments — transitive JSON data files pulled in via
#     phonemizer -> segments -> csvw -> language_tags (data, not just code).
uv run pyinstaller \
  --onefile \
  --name g2p_server \
  --console \
  --noconfirm \
  --clean \
  --collect-all espeakng_loader \
  --collect-all misaki \
  --collect-all en_core_web_sm \
  --collect-all spacy \
  --collect-data phonemizer \
  --collect-data language_tags \
  --collect-data csvw \
  --collect-data segments \
  --copy-metadata en_core_web_sm \
  --copy-metadata spacy \
  g2p_server.py

BIN="$SIDE/dist/g2p_server"
if [[ ! -x "$BIN" ]]; then
  echo "freeze failed: $BIN not produced" >&2
  exit 1
fi

echo
echo "Frozen sidecar: $BIN"
echo "Smoke-test:  printf 'Hello world.\n__QUIT__\n' | '$BIN'"
echo "(expect a phoneme line on stdout and '__READY__ …' on stderr)"

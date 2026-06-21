#!/usr/bin/env bash
# Create the Kokoro sidecar venv and install dependencies.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SIDE="$ROOT/sidecar"
VENV="$SIDE/.venv"

PY="${PYTHON:-python3.12}"
if ! command -v "$PY" >/dev/null 2>&1; then
  echo "python3.12 not found. Install it (brew install python@3.12) or set PYTHON=..." >&2
  exit 1
fi

echo "Creating venv at $VENV"
"$PY" -m venv "$VENV"
"$VENV/bin/pip" install --upgrade pip
"$VENV/bin/pip" install -r "$SIDE/requirements.txt"

echo
echo "Done. Sidecar venv ready at $VENV"
echo "System deps still required on PATH: ffmpeg, espeak-ng"

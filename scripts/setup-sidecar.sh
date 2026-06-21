#!/usr/bin/env bash
# Set up the Kokoro sidecar environment with uv.
#
# uv reads sidecar/pyproject.toml + uv.lock and creates a reproducible,
# self-contained env. It is move-proof: if the project is relocated or freshly
# cloned, `uv sync` (or `uv run`) just re-creates the env — no broken venv.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SIDE="$ROOT/sidecar"

if ! command -v uv >/dev/null 2>&1; then
  echo "uv not found. Install it: brew install uv  (or https://docs.astral.sh/uv/)" >&2
  exit 1
fi

echo "Syncing sidecar env from $SIDE/pyproject.toml"
( cd "$SIDE" && uv sync )

echo
echo "Done. Run the sidecar with:  cd sidecar && uv run kokoro_server.py --warm"
echo "(The app launches it automatically via uv.)"
echo "System deps still required on PATH: ffmpeg, espeak-ng"

#!/usr/bin/env bash
# Set up the G2P sidecar environment with uv (development use).
#
# uv reads sidecar/pyproject.toml + uv.lock and creates a reproducible,
# torch-free env (slim misaki, no transformers). It is move-proof: if the
# project is relocated or freshly cloned, `uv sync` (or `uv run`) just
# re-creates the env — no broken venv.
#
# NOTE: this is the DEV path. For a distributable .app the sidecar is frozen to
# a standalone binary (no Python/uv at runtime) — see scripts/freeze-sidecar.sh
# and BUNDLING.md.
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
echo "Done. Run the sidecar with:  cd sidecar && uv run g2p_server.py"
echo "(The app launches it automatically via uv in dev.)"
echo "System deps still required on PATH: ffmpeg, espeak-ng"

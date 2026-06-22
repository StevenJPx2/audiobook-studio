#!/usr/bin/env python3
"""Slim grapheme-to-phoneme (G2P) sidecar for Audiobook Studio.

Replaces the old Kokoro TTS sidecar. TTS inference now runs natively in Rust
(MLX voice-tts); this process does *only* G2P — turning text into the Kokoro
phoneme string the Rust side feeds to the model.

Engine: misaki English G2P, British, perceptron POS (trf=False) + espeak-ng
fallback for out-of-vocabulary words. No torch, no transformer model — the
perceptron tagger + full dictionary already handle homographs
(read/wind/record/present/close/live/produce) correctly.

Protocol (persistent, line-based, UTF-8):
  startup:  prints `__READY__ startup=<s> rss=<MB>` to STDERR once loaded
  request:  one line of text on STDIN
  response: one line of phonemes on STDOUT (space-joined; may be empty)
  shutdown: the line `__QUIT__` on STDIN

All higher-level shaping (sentence splitting, whitespace/ZWJ cleanup, spurious
clause-period stripping, pronunciation overrides) is done on the Rust side so it
stays unit-testable; this server is intentionally minimal.
"""
from __future__ import annotations

import sys
import time
import resource


def _rss_mb() -> float:
    # macOS reports ru_maxrss in bytes; Linux in kilobytes. Normalize to MB.
    raw = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    return raw / (1024 * 1024) if sys.platform == "darwin" else raw / 1024


def main() -> None:
    t0 = time.time()
    from misaki import en, espeak

    fallback = espeak.EspeakFallback(british=True)
    g2p = en.G2P(trf=False, british=True, fallback=fallback)
    # Warm the pipeline so the first real request isn't slow.
    _ = g2p("warm up the pipeline.")

    print(
        f"__READY__ startup={time.time() - t0:.3f}s rss={_rss_mb():.0f}MB",
        file=sys.stderr,
        flush=True,
    )

    for line in sys.stdin:
        line = line.rstrip("\n")
        if line == "__QUIT__":
            break
        if not line.strip():
            print("", flush=True)
            continue
        try:
            phonemes, _tokens = g2p(line)
        except Exception as exc:  # never crash the long-lived server on one line
            print("", flush=True)
            print(f"__ERR__ {exc!r}", file=sys.stderr, flush=True)
            continue
        # Single line out. The Rust client does all post-processing.
        print(phonemes.replace("\n", " "), flush=True)


if __name__ == "__main__":
    main()

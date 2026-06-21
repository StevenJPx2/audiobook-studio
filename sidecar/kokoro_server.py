#!/usr/bin/env python3
"""
Kokoro TTS sidecar for Audiobook Studio.

A tiny local FastAPI service the Tauri/Rust backend calls to synthesize a
chapter transcript into an MP3. The Kokoro pipeline is loaded once at startup
and reused across requests (model load is the slow part).

Endpoints
  GET  /health                 -> {"status":"ok","voice_loaded":bool}
  POST /tts {text_path,out_path,voice,lang,speed} -> {out_path,audio_seconds}

Run (managed by the app, or standalone for dev):
  uv run kokoro_server.py --warm            # preferred (auto-syncs env)
  uv run kokoro_server.py --host 127.0.0.1 --port 8765
"""
from __future__ import annotations

import argparse
import os
import re
import subprocess
import tempfile
import time
from pathlib import Path

import numpy as np
import soundfile as sf
from fastapi import FastAPI, HTTPException
from pydantic import BaseModel
from kokoro import KPipeline

SR = 24000               # Kokoro output sample rate
PARA_GAP = 0.40          # silence between paragraphs (s)
HEAD_GAP = 0.80          # silence after the title block / heading (s)
MP3_BITRATE = "128k"

app = FastAPI(title="Kokoro Sidecar", version="0.1.0")

# Pipelines are keyed by language code ('b' British, 'a' American). Loaded lazily.
_pipelines: dict[str, KPipeline] = {}


def get_pipeline(lang: str) -> KPipeline:
    if lang not in _pipelines:
        _pipelines[lang] = KPipeline(lang_code=lang)
    return _pipelines[lang]


def paragraphs(text: str) -> list[str]:
    parts = re.split(r"\n\s*\n", text.strip())
    return [p.strip() for p in parts if p.strip()]


class TtsRequest(BaseModel):
    text_path: str
    out_path: str
    voice: str = "bm_george"
    lang: str = "b"
    speed: float = 1.0


class TtsResponse(BaseModel):
    out_path: str
    audio_seconds: float


class CoverRequest(BaseModel):
    pdf_path: str
    out_path: str
    page: int = 1        # 1-indexed source page to render
    zoom: float = 2.0    # render scale (2.0 ≈ 144 dpi)


class CoverResponse(BaseModel):
    out_path: str
    width: int
    height: int


@app.get("/health")
def health() -> dict:
    return {"status": "ok", "voice_loaded": bool(_pipelines)}


@app.post("/tts", response_model=TtsResponse)
def tts(req: TtsRequest) -> TtsResponse:
    src = Path(req.text_path)
    if not src.exists():
        raise HTTPException(status_code=400, detail=f"text_path not found: {src}")

    out = Path(req.out_path)
    out.parent.mkdir(parents=True, exist_ok=True)

    text = src.read_text(encoding="utf-8")
    paras = paragraphs(text)
    if not paras:
        raise HTTPException(status_code=400, detail="empty transcript")

    pipeline = get_pipeline(req.lang)
    sil_para = np.zeros(int(SR * PARA_GAP), dtype=np.float32)
    sil_head = np.zeros(int(SR * HEAD_GAP), dtype=np.float32)

    t0 = time.time()
    nsamp = 0
    with tempfile.NamedTemporaryFile(suffix=".wav", delete=False) as tf:
        tmp_wav = tf.name
    try:
        with sf.SoundFile(tmp_wav, "w", samplerate=SR, channels=1, subtype="PCM_16") as snd:
            for pi, para in enumerate(paras):
                for _, _, audio in pipeline(para, voice=req.voice, speed=req.speed):
                    if audio is None:
                        continue
                    a = audio.detach().cpu().numpy() if hasattr(audio, "detach") else np.asarray(audio)
                    snd.write(a.astype(np.float32))
                    nsamp += len(a)
                gap = sil_head if pi <= 1 else sil_para
                snd.write(gap)
                nsamp += len(gap)

        subprocess.run(
            ["ffmpeg", "-y", "-loglevel", "error", "-i", tmp_wav,
             "-b:a", MP3_BITRATE, str(out)],
            check=True,
        )
    finally:
        if os.path.exists(tmp_wav):
            os.remove(tmp_wav)

    audio_seconds = nsamp / SR
    gen = time.time() - t0
    print(f"[tts] {out.name}: {audio_seconds/60:.1f} min audio in {gen/60:.1f} min "
          f"({audio_seconds/gen:.1f}x realtime)", flush=True)
    return TtsResponse(out_path=str(out), audio_seconds=audio_seconds)


@app.post("/cover", response_model=CoverResponse)
def cover(req: CoverRequest) -> CoverResponse:
    """Render one PDF page to a JPEG cover (defaults to page 1)."""
    try:
        import fitz  # PyMuPDF
    except ImportError as e:
        raise HTTPException(status_code=501, detail=f"PyMuPDF not installed: {e}")

    src = Path(req.pdf_path)
    if not src.exists():
        raise HTTPException(status_code=400, detail=f"pdf_path not found: {src}")

    out = Path(req.out_path)
    out.parent.mkdir(parents=True, exist_ok=True)

    doc = fitz.open(str(src))
    try:
        idx = max(0, min(req.page - 1, doc.page_count - 1))
        page = doc[idx]
        pix = page.get_pixmap(matrix=fitz.Matrix(req.zoom, req.zoom), alpha=False)
        pix.save(str(out), jpg_quality=90) if out.suffix.lower() in (".jpg", ".jpeg") \
            else pix.save(str(out))
        w, h = pix.width, pix.height
    finally:
        doc.close()

    print(f"[cover] {src.name} p{req.page} -> {out.name} ({w}x{h})", flush=True)
    return CoverResponse(out_path=str(out), width=w, height=h)


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, default=8765)
    ap.add_argument("--warm", action="store_true", help="preload the British pipeline at startup")
    args = ap.parse_args()

    if args.warm:
        print("Warming Kokoro (lang=b)…", flush=True)
        get_pipeline("b")

    import uvicorn
    uvicorn.run(app, host=args.host, port=args.port, log_level="warning")


if __name__ == "__main__":
    main()

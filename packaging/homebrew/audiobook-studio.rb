# Homebrew formula for the Audiobook Studio CLI (`abs`).
#
# Distribution: a prebuilt, self-contained tarball (abs + frozen G2P sidecar +
# libpdfium) produced by scripts/package-cli-tarball.sh and attached to a
# GitHub Release. The MLX Kokoro model is NOT bundled; it downloads from
# HuggingFace on first `abs generate`.
#
# Publish to a tap (StevenJPx2/homebrew-audiobook-studio):
#   brew install StevenJPx2/audiobook-studio/audiobook-studio
#
# Apple Silicon only (MLX requirement).
class AudiobookStudio < Formula
  desc "PDF to chaptered .m4b audiobook with a local LLM + Kokoro TTS (CLI)"
  homepage "https://github.com/StevenJPx2/audiobook-studio"
  version "0.1.0"
  license "MIT"

  # Apple Silicon prebuilt tarball. Fill in url + sha256 from
  # scripts/package-cli-tarball.sh output at release time.
  url "https://github.com/StevenJPx2/audiobook-studio/releases/download/v0.1.0/abs-0.1.0-macos-arm64.tar.gz"
  sha256 "REPLACE_WITH_SHA256_FROM_package-cli-tarball.sh"

  depends_on :macos
  depends_on arch: :arm64
  depends_on "ffmpeg" # required: .m4b encoding + audio assembly

  def install
    # Self-contained payload (abs + libpdfium.dylib + sidecar/g2p_server) lives
    # together in libexec so abs's exe-relative lookups (cover.rs / g2p.rs)
    # resolve the sidecar and libpdfium next to the binary.
    libexec.install Dir["*"]
    bin.install_symlink libexec/"abs"
  end

  def caveats
    <<~EOS
      Audiobook Studio uses a local Ollama for chapter detection, OCR, and the
      optional polish pass. Install and start it, then pull the models:

        brew install --cask ollama   # or https://ollama.com
        ollama pull gemma4:e2b-mlx   # chapter detection + polish
        ollama pull glm-ocr:q8_0     # OCR for scanned PDFs (optional)

      Verify everything with:  abs doctor

      The MLX Kokoro voice model (~312 MB) downloads from HuggingFace on the
      first `abs generate`.
    EOS
  end

  test do
    # `doctor` runs without network/models and exits 0 unless ffmpeg is missing
    # (which is a hard dependency here), so it's a safe smoke test.
    assert_match "g2p-sidecar", shell_output("#{bin}/abs doctor 2>&1", 0)
  end
end

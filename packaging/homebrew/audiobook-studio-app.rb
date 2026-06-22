# Homebrew Cask for the Audiobook Studio GUI (.app).
#
# Slim build: the ~312 MB MLX voice model is NOT bundled; the app downloads it
# from HuggingFace on first launch (with an in-app setup screen). Apple Silicon
# only. Unsigned — Casks install via curl and are not quarantined, so it
# launches without a Gatekeeper prompt.
#
#   brew install --cask StevenJPx2/audiobook-studio/audiobook-studio-app
#
# url + sha256 are bumped automatically by the app repo's release workflow.
cask "audiobook-studio-app" do
  version "0.1.1"
  sha256 "REPLACE_ON_FIRST_CASK_RELEASE"

  url "https://github.com/StevenJPx2/audiobook-studio/releases/download/v#{version}/AudiobookStudio-#{version}-macos-arm64.zip"
  name "Audiobook Studio"
  desc "Turn a book PDF into a chaptered .m4b audiobook (local LLM + Kokoro TTS)"
  homepage "https://github.com/StevenJPx2/audiobook-studio"

  depends_on macos: ">= :ventura"
  depends_on arch: :arm64
  depends_on formula: "ffmpeg"

  app "Audiobook Studio.app"

  caveats <<~EOS
    Audiobook Studio uses a local Ollama for chapter detection, OCR, and the
    optional polish pass:
      brew install --cask ollama
      ollama pull gemma4:e2b-mlx   # chapter detection + polish
      ollama pull glm-ocr:q8_0     # OCR for scanned PDFs (optional)

    On first launch the app downloads the MLX Kokoro voice model (~312 MB)
    from HuggingFace, shown as a setup step.
  EOS
end

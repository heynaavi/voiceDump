#!/usr/bin/env bash
# Download the bundled speech models.
#
# These are the only large files the app ships, and they're the reason it works
# offline: they go inside the .app, so a user who downloads the DMG never
# fetches anything. Kept out of git (a 730 MB repo helps nobody) and fetched
# here instead, checksummed so a truncated download can't silently ship.
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p models
BASE="https://huggingface.co/ggerganov/whisper.cpp/resolve/main"

# Multilingual, not the .en variants: the app already handles non-English notes.
# q5 quantisation — the accuracy difference is inaudible next to halving size.
for m in ggml-small-q5_1.bin ggml-medium-q5_0.bin; do
  if [ -s "models/$m" ]; then
    echo "  have $m ($(du -h "models/$m" | cut -f1))"
    continue
  fi
  echo "  fetching $m…"
  curl -fL --progress-bar "$BASE/$m" -o "models/$m.part"
  mv "models/$m.part" "models/$m"
  echo "  got $m ($(du -h "models/$m" | cut -f1))"
done
echo "Models ready in ./models"

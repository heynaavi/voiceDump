#!/usr/bin/env bash
# Download the speech models for development.
#
# Ships nothing. The app fetches these itself on first run and keeps them in
# its data directory (see `src-tauri/src/models.rs`), which is why a release is
# 4.6 MB rather than 720 MB. This script exists so a checkout can run `cargo
# test` and `tauri dev` against local weights without going through the setup
# screen every time — `engine::model_path` looks in `./models` last.
#
# Same files, same source as the app uses, so a dev machine and a user's
# machine are running identical weights. Kept out of git: a 730 MB repo helps
# nobody.
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

#!/usr/bin/env bash
# Measure dictation latency on this machine.
#
# Everything the README claims about speed comes from here, so it is a script in
# the repo rather than a number somebody typed: anyone can run it and get their
# own figures on their own hardware.
#
# It builds in release. Debug numbers would be unfairly slow and are not what
# ships.
#
# Usage:
#   scripts/bench.sh                 # 12s of synthesised speech
#   scripts/bench.sh path/to.wav     # your own audio
#   VOICEDUMPS_MODEL_SIZE=small scripts/bench.sh
set -euo pipefail
cd "$(dirname "$0")/.."

SAMPLE="${1:-}"

if [ -z "$SAMPLE" ]; then
  SAMPLE="/tmp/vd-bench/dictation.wav"
  if [ ! -s "$SAMPLE" ]; then
    command -v ffmpeg >/dev/null || { echo "need ffmpeg to build the sample, or pass your own wav"; exit 1; }
    mkdir -p /tmp/vd-bench
    echo "synthesising a speech sample with \`say\`…"
    say -v Samantha -o /tmp/vd-bench/raw.aiff \
      "Ship the build tonight. I have reviewed the deck and the numbers hold up, so let us go on Thursday. \
Retention is the story here: churn dropped for the third month running, and the pricing page should land before the board call."
    ffmpeg -v error -i /tmp/vd-bench/raw.aiff -ac 1 -ar 16000 -c:a pcm_s16le "$SAMPLE" -y
  fi
fi

[ -s "models/ggml-small-q5_1.bin" ] || { echo "models missing — run scripts/fetch-models.sh"; exit 1; }

echo "sample: $SAMPLE"
echo "machine: $(sysctl -n machdep.cpu.brand_string), $(sysctl -n hw.ncpu) cores, \
$(sysctl -n hw.memsize | awk '{printf "%.0f GB", $1/1024/1024/1024}'), macOS $(sw_vers -productVersion)"
echo

TEST_AUDIO="$(cd "$(dirname "$SAMPLE")" && pwd)/$(basename "$SAMPLE")" \
VOICEDUMPS_MODEL_DIR="$PWD/models" \
  cargo test --manifest-path src-tauri/Cargo.toml --release \
    --no-default-features benchmark_latency -- --ignored --nocapture

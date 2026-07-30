#!/usr/bin/env bash
# Regenerate every app icon asset from scripts/make-icon.py.
#
# Each size is rendered natively rather than downscaled from one master: the
# mark is a grid of hard-edged squares, and resampling a 1024px render down to
# 16px turns those crisp edges into grey mush. Rendering at the target size
# keeps the cells aligned to the pixel grid.
#
# Usage: ./scripts/make-icons.sh [python]
set -euo pipefail

cd "$(dirname "$0")/.."
PY="${1:-python3}"
ICONS=src-tauri/icons
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

render() { "$PY" scripts/make-icon.py "$@"; }

echo "Rendering icon set…"

# -- macOS .icns --------------------------------------------------------------
SET="$WORK/icon.iconset"
mkdir -p "$SET"
render "$SET/icon_16x16.png"        16
render "$SET/icon_16x16@2x.png"     32
render "$SET/icon_32x32.png"        32
render "$SET/icon_32x32@2x.png"     64
render "$SET/icon_128x128.png"     128
render "$SET/icon_128x128@2x.png"  256
render "$SET/icon_256x256.png"     256
render "$SET/icon_256x256@2x.png"  512
render "$SET/icon_512x512.png"     512
render "$SET/icon_512x512@2x.png" 1024
iconutil -c icns "$SET" -o "$ICONS/icon.icns"
echo "  icon.icns  $(du -h "$ICONS/icon.icns" | cut -f1)"

# -- the PNGs tauri.conf.json names ------------------------------------------
render "$ICONS/32x32.png"        32
render "$ICONS/128x128.png"     128
render "$ICONS/128x128@2x.png"  256
render "$ICONS/icon.png"       1024

# -- macOS menu-bar template --------------------------------------------------
# Not the app icon: a template image is redrawn from its alpha alone, so an
# opaque tile would show up as a solid blob. See render_tray().
render "$ICONS/tray.png" 44 --tray

# -- Windows Store tiles (unused on macOS, kept so the set stays coherent) ----
for spec in 30 44 71 89 107 142 150 284 310; do
  render "$ICONS/Square${spec}x${spec}Logo.png" "$spec"
done
render "$ICONS/StoreLogo.png" 50

# -- .ico ---------------------------------------------------------------------
# Referenced by tauri.conf.json. A PNG-payload ICO (Vista+) is enough and keeps
# this dependency-free; nothing on macOS reads it.
for s in 16 32 48 64 128 256; do render "$WORK/ico-$s.png" "$s"; done
"$PY" - "$WORK" "$ICONS/icon.ico" <<'PY'
import struct, sys
from pathlib import Path

work, out = Path(sys.argv[1]), Path(sys.argv[2])
sizes = [16, 32, 48, 64, 128, 256]
blobs = [(s, (work / f"ico-{s}.png").read_bytes()) for s in sizes]

header = struct.pack("<HHH", 0, 1, len(blobs))
offset = 6 + 16 * len(blobs)
entries, payload = b"", b""
for s, blob in blobs:
    entries += struct.pack(
        "<BBBBHHII", 0 if s >= 256 else s, 0 if s >= 256 else s, 0, 0, 1, 32, len(blob), offset
    )
    payload += blob
    offset += len(blob)
out.write_bytes(header + entries + payload)
print(f"  icon.ico  {out.stat().st_size / 1024:.0f} KB")
PY

echo "Done — $ICONS"

#!/usr/bin/env bash
# Strip the volume icon out of a built .dmg.
#
# Tauri's DMG bundler drops a `.VolumeIcon.icns` at the root of the disk image
# to give the mounted volume a custom icon. Finder draws it as a second document
# next to the app, so the installer window opens with two VoiceDumps logos in it
# and no way to tell which one to drag — a bad first thirty seconds for an app
# nobody has heard of.
#
# Setting UF_HIDDEN on it does not help. Anyone with invisible items shown
# (⌘⇧. — a per-window toggle that leaves no preference behind, so you cannot
# detect it or rely on it being off) still sees the file: Finder special-cases
# `.DS_Store` and shows everything else regardless of the flag. Since we cannot
# control the viewer's Finder, the file itself has to go.
#
# There is no other place to put it. A volume icon has to be exactly
# `/.VolumeIcon.icns` at the root, and the older mechanism — a resource-fork
# `Icon\r` file — is a visible file too, with a stranger name. So the mounted
# volume gets the generic disk icon, which costs one icon in the sidebar for the
# few seconds the DMG is mounted, and buys an installer window that is
# unambiguous on every Mac. The app's own icon is untouched.
#
# Usage: ./scripts/finish-dmg.sh [path/to/App.dmg | bundle/dmg dir]
#
# With no argument it finds the newest .dmg under the release bundle, so the
# version number in the filename is never spelled out in a build script.
set -euo pipefail

cd "$(dirname "$0")/.."
TARGET="${1:-src-tauri/target/release/bundle/dmg}"

if [ -d "$TARGET" ]; then
  DMG="$(ls -t "$TARGET"/*.dmg 2>/dev/null | head -1)"
else
  DMG="$TARGET"
fi
[ -n "${DMG:-}" ] && [ -f "$DMG" ] || { echo "no dmg found at: $TARGET" >&2; exit 1; }

WORK="$(mktemp -d)"
MNT="$WORK/mnt"
cleanup() {
  hdiutil detach "$MNT" -quiet -force 2>/dev/null || true
  rm -rf "$WORK"
}
trap cleanup EXIT
mkdir -p "$MNT"

# A shipped DMG is compressed and read-only, so it has to be round-tripped
# through a read-write image to change anything inside it.
hdiutil convert "$DMG" -format UDRW -o "$WORK/rw.dmg" -quiet
hdiutil attach "$WORK/rw.dmg" -mountpoint "$MNT" -nobrowse -noautoopen -quiet

removed=0
if [ -e "$MNT/.VolumeIcon.icns" ]; then
  rm -f "$MNT/.VolumeIcon.icns"
  # Clear the root's "has a custom icon" Finder flag as well, so Finder stops
  # looking for the file we just deleted and falls straight back to the default.
  xattr -d com.apple.FinderInfo "$MNT" 2>/dev/null || true
  removed=1
fi

# Anything else the bundler leaves behind is genuinely internal, and hiding it
# is enough — none of it is a second copy of the app icon.
for stray in .fseventsd .Trashes .background; do
  [ -e "$MNT/$stray" ] && chflags -h hidden "$MNT/$stray"
done

hdiutil detach "$MNT" -quiet
rm -f "$DMG"
hdiutil convert "$WORK/rw.dmg" -format UDZO -imagekey zlib-level=9 -o "$DMG" -quiet

echo "  $(basename "$DMG") — removed $removed volume icon, $(du -h "$DMG" | cut -f1)"

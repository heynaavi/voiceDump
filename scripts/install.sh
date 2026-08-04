#!/usr/bin/env bash
# Install the latest VoiceDumps release.
#
# This exists for one reason: the app is signed ad-hoc rather than notarized,
# because notarization requires Apple's paid Developer ID. macOS therefore
# refuses to open it after a browser download, with a dialog whose default
# button is "Move to Bin" — see the README. Installing from a script rather
# than the Finder means the quarantine flag can be cleared as part of the
# install, so nobody meets that dialog at all.
#
# Read it before you run it. It is deliberately short and does nothing clever:
# download the DMG the project publishes, check its signature, copy the app to
# /Applications, drop the quarantine flag on the copy, unmount. No sudo, no
# background agent, nothing written outside /Applications.

set -euo pipefail

REPO="heynaavi/voiceDump"
APP="VoiceDumps.app"
DEST="/Applications/$APP"
# The identifier the bundle must claim. macOS keys an app's data directory off
# this, and it is what makes the downloaded thing *this* app rather than
# something that merely arrived from the same URL.
WANT_ID="dev.heynaavi.voicedump"

say() { printf '\033[1m==>\033[0m %s\n' "$*"; }
die() { printf '\033[1;31mError:\033[0m %s\n' "$*" >&2; exit 1; }

[ "$(uname -s)" = "Darwin" ] || die "VoiceDumps is macOS only."
[ "$(uname -m)" = "arm64" ] || die "VoiceDumps needs Apple Silicon."

if pgrep -x VoiceDumps >/dev/null 2>&1; then
  die "VoiceDumps is running. Quit it first, then run this again."
fi

say "Finding the latest release…"
URL=$(/usr/bin/curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" \
  | /usr/bin/grep -o '"browser_download_url": *"[^"]*\.dmg"' \
  | /usr/bin/sed 's/.*"\(https[^"]*\)"/\1/' \
  | /usr/bin/head -1)
[ -n "$URL" ] || die "Could not find a .dmg in the latest release."
say "Downloading $(basename "$URL")"

TMP=$(/usr/bin/mktemp -d)
MOUNT=""
cleanup() {
  [ -n "$MOUNT" ] && /usr/bin/hdiutil detach "$MOUNT" -quiet 2>/dev/null || true
  /bin/rm -rf "$TMP"
}
trap cleanup EXIT

DMG="$TMP/voicedumps.dmg"
/usr/bin/curl -fL --progress-bar "$URL" -o "$DMG"

say "Mounting…"
# stderr is dropped because recent macOS prints a deprecation notice for these
# flags while still honouring them; `diskutil image attach` is not on macOS 11,
# which this still supports.
MOUNT=$(/usr/bin/hdiutil attach "$DMG" -nobrowse -readonly 2>/dev/null \
  | /usr/bin/grep -o '/Volumes/.*' | /usr/bin/head -1)
[ -n "$MOUNT" ] && [ -d "$MOUNT/$APP" ] || die "The disk image did not contain $APP."

# Ad-hoc signatures are not a chain of trust — nobody vouches for them — but
# they do seal the bundle, so this catches a download that arrived corrupted or
# was altered after it was built. It is the strongest check available without
# a paid certificate, and it is worth making rather than skipping.
say "Checking the signature…"
/usr/bin/codesign --verify --deep --strict "$MOUNT/$APP" 2>/dev/null \
  || die "The downloaded app failed its own signature check. Not installing it."
GOT_ID=$(/usr/bin/codesign -dv "$MOUNT/$APP" 2>&1 | /usr/bin/sed -n 's/^Identifier=//p')
[ "$GOT_ID" = "$WANT_ID" ] \
  || die "The app identifies as '$GOT_ID', not '$WANT_ID'. Not installing it."

if [ -d "$DEST" ]; then
  OLD=$(/usr/libexec/PlistBuddy -c 'Print CFBundleShortVersionString' \
    "$DEST/Contents/Info.plist" 2>/dev/null || echo "?")
  say "Replacing the copy already installed ($OLD)…"
  /bin/rm -rf "$DEST"
fi

say "Copying to $(dirname "$DEST")…"
/bin/cp -R "$MOUNT/$APP" "$DEST"

# The point of the whole script. Without this the copy inherits the download's
# quarantine flag and macOS refuses to open it.
say "Clearing the quarantine flag…"
/usr/bin/xattr -dr com.apple.quarantine "$DEST" 2>/dev/null || true

VER=$(/usr/libexec/PlistBuddy -c 'Print CFBundleShortVersionString' \
  "$DEST/Contents/Info.plist" 2>/dev/null || echo "?")
say "VoiceDumps $VER is in $(dirname "$DEST"). Open it from Spotlight."
echo
echo "  First launch downloads the speech models — 729 MB on a Mac with 16 GB"
echo "  or more, 190 MB below that. They are kept beside your notes and survive"
echo "  every update, so this happens once."

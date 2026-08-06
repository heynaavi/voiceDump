#!/usr/bin/env bash
# Publish the public build to github.com/heynaavi/voiceDump.
#
# Two products, one tree. This is the filter between them, and it is a script
# rather than a habit because the failure mode is not "the release is late" —
# it is a private file in a public repository, which cannot be taken back once
# it has been cloned.
#
# WHAT MAKES THIS NECESSARY. The public repository has its own history,
# unrelated to this one: it was created as an export and every release since has
# been another export. So there is no branch to merge and no remote to push, and
# the temptation is to copy files by hand. The list below is what a hand-copy
# would have to remember, every time, forever.
#
# WHAT MUST NEVER GO. Beyond the obvious integration modules, one file is worth
# naming out loud: `cross-repo-graph.json` is a knowledge graph built across all
# three repositories, and it names the private website and the internal sidecar
# more than a thousand times each. It is a build artifact, it is tracked here on
# purpose, and it would be the single worst thing in this tree to publish.
#
# Usage:
#   scripts/publish-lite.sh            # stage into a temp clone and show the diff
#   scripts/publish-lite.sh --push     # ...and push it, after the diff is shown
#
# Nothing is pushed without --push, and even then the diff is printed first.

set -euo pipefail

PUBLIC="https://github.com/heynaavi/voiceDump.git"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Everything below stays private. Paths are matched against the repository root.
#
# Adding to this list is cheap and removing from it is not, so when in doubt
# about a new file, add it: a missing file in a release is a follow-up commit, a
# leaked one is a rotated credential and an apology.
PRIVATE=(
  "sidecar/"                        # the internal Python service, whole
  "scripts/setup-sidecar.sh"
  "src-tauri/src/slack.rs"
  "src-tauri/src/discord.rs"
  "src-tauri/src/knowledge.rs"
  "src-tauri/src/sidecar.rs"
  "src-tauri/tauri.qwee.conf.json"  # the private build's identifier and name
  "cross-repo-graph.json"           # spans all three repos — see above
  "docs/chat-harness.md"            # internal design notes, quotes a real library
  ".claude/"                        # worktrees, local settings, agent scratch

  # The READMEs are deliberately different and this one is not the public one.
  # Ours opens with an AWS Bedrock badge and a table of what gets sent to it —
  # true of the private build, and a description of features the public app does
  # not have and cannot be given. Copying it over would advertise Slack, Discord
  # and a cloud round-trip to people who chose this app because it has none of
  # those.
  "README.md"
  # The public one, which is written here and installed as README.md below. It
  # is kept in this repository so it can be reviewed in a diff alongside the
  # features it describes, rather than edited on GitHub after the fact and
  # slowly drifting from what the app does.
  "docs/public-README.md"
)

is_private() {
  local path="$1"
  for deny in "${PRIVATE[@]}"; do
    case "$deny" in
      */) [[ "$path" == "$deny"* ]] && return 0 ;;
      *)  [[ "$path" == "$deny" ]] && return 0 ;;
    esac
  done
  return 1
}

version="$(python3 -c 'import json;print(json.load(open("src-tauri/tauri.conf.json"))["version"])')"
echo "[publish] preparing VoiceDumps $version for $PUBLIC"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# Never wait at a prompt. With no terminal attached, a credential request does
# not fail — it hangs, silently, forever, and the first run of this script sat
# for six minutes having used 0.09 seconds of CPU before anyone thought to look.
export GIT_TERMINAL_PROMPT=0

# Blobless and shallow. The published repository carries about 8 MB of video in
# docs/media, every byte of which is about to be overwritten from this tree, so
# downloading it is pure latency. The two files that *are* read out of the clone
# (install.sh, LICENSE) are fetched on demand by the filter.
echo "[publish] fetching the published tree…"
git clone --quiet --depth 1 --filter=blob:none --single-branch "$PUBLIC" "$work/public" || {
  echo "[publish] could not reach $PUBLIC — nothing was pushed."
  exit 1
}

# Wipe the tracked tree rather than copying over it. Without this a file deleted
# here stays in the public repository forever, which is how a repo slowly fills
# with sources nobody builds any more.
( cd "$work/public" && git ls-files -z | xargs -0 rm -f )

published=0
withheld=0
while IFS= read -r path; do
  if is_private "$path"; then
    withheld=$((withheld + 1))
    continue
  fi
  mkdir -p "$work/public/$(dirname "$path")"
  git -C "$HERE" show "HEAD:$path" > "$work/public/$path"
  published=$((published + 1))
done < <(git -C "$HERE" ls-tree -r --name-only HEAD)

# The public README, written here under a different name so it can never be
# confused with ours by a careless copy, and installed under the name GitHub
# renders.
if [ -f "$HERE/docs/public-README.md" ]; then
  git -C "$HERE" show "HEAD:docs/public-README.md" > "$work/public/README.md"
  published=$((published + 1))
else
  echo "[publish] REFUSING: docs/public-README.md is missing"
  exit 1
fi

# Anything else the public repository owns and this tree has no counterpart for.
for kept in scripts/install.sh LICENSE; do
  if [ ! -f "$work/public/$kept" ]; then
    git -C "$work/public" show "HEAD:$kept" > "$work/public/$kept" 2>/dev/null || true
  fi
done
chmod +x "$work/public/scripts/install.sh" 2>/dev/null || true

echo "[publish] $published file(s) to publish, $withheld withheld"

# The proof, before anything leaves the machine: no private path survived.
#
# README.md is the one path that is deliberately written from a private source
# under a public name, so the name alone cannot decide it. It is checked by
# content instead, immediately below, which is the stronger test anyway — it
# catches our README arriving there by any route, not just by being copied.
leaked=0
while IFS= read -r path; do
  [ "$path" = "README.md" ] && continue
  if is_private "$path"; then
    echo "[publish] REFUSING: $path is private and would have been published"
    leaked=1
  fi
done < <(cd "$work/public" && git add -A >/dev/null 2>&1 && git diff --cached --name-only)

# The published README must be the public one, byte for byte.
if ! git -C "$HERE" show "HEAD:docs/public-README.md" | cmp -s - "$work/public/README.md"; then
  echo "[publish] REFUSING: README.md is not docs/public-README.md"
  leaked=1
fi
# And must not be ours, whatever else it is.
if git -C "$HERE" show "HEAD:README.md" | cmp -s - "$work/public/README.md"; then
  echo "[publish] REFUSING: the private README reached the public repository"
  leaked=1
fi

[ "$leaked" -eq 0 ] || { echo "[publish] nothing was pushed."; exit 1; }

echo
echo "===== what would change in the public repository ====="
git -C "$work/public" diff --cached --stat | tail -40
echo "====================================================="
echo

if [ "${1:-}" != "--push" ]; then
  echo "[publish] dry run. Re-run with --push to publish $version."
  exit 0
fi

git -C "$work/public" -c user.name="$(git -C "$HERE" config user.name)" \
  -c user.email="$(git -C "$HERE" config user.email)" \
  commit --quiet -m "Release $version"
git -C "$work/public" tag "v$version"
git -C "$work/public" push --quiet origin HEAD:main
git -C "$work/public" push --quiet origin "v$version"
echo "[publish] pushed $version and tagged v$version"

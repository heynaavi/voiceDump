#!/usr/bin/env bash
# Tell the website a release exists.
#
# The site bakes the version, size and download URL into its HTML at build time,
# which is what makes it work with JavaScript off. The cost of that choice is
# that a release published after the last deploy is invisible until something
# rebuilds — and publishing a release changes nothing in the website's own
# repository, so nothing would.
#
# This closes that. It fires the `release-published` repository_dispatch that
# voiceDumpWeb's ci.yml already listens for; that workflow rebuilds, runs the
# no-JS and accessibility checks, and then posts to a Vercel Deploy Hook to
# republish.
#
# **No token is stored anywhere for this.** `gh` is already signed in as
# somebody who can write to both repositories, which is the same authority that
# just cut the release. The one secret in the chain is the deploy hook, and it
# lives in the website's repository where it is used.
#
# Safe to run twice: a dispatch is an event, not a state, and a second rebuild
# of the same release produces the same page.
#
#   scripts/announce-release.sh            # after `gh release create`
#
set -euo pipefail

WEB_REPO="${VOICEDUMPS_WEB_REPO:-heynaavi/voiceDumpWeb}"

if ! command -v gh >/dev/null 2>&1; then
  echo "[announce] gh is not installed — the site will pick this up within a day." >&2
  exit 0
fi

# Never fail a release over this. The daily schedule in ci.yml and freshen.ts in
# the browser both cover the same gap more slowly, so a failed announcement
# costs freshness, not correctness — and a release that is already published
# should not report failure because a website did not rebuild.
if gh api -X POST "repos/${WEB_REPO}/dispatches" \
     -f event_type=release-published >/dev/null 2>&1; then
  echo "[announce] told ${WEB_REPO} to rebuild"
else
  echo "[announce] could not reach ${WEB_REPO}; it rebuilds daily anyway." >&2
fi

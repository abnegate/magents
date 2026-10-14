#!/usr/bin/env bash
# Commit the baked product-version bump to the default branch when RELEASE_SHA
# is still that branch's tip. Keep this file list in sync with
# set-release-version.sh and tests/release_version.rs.
# Usage: commit-release-version.sh
# Required env: VERSION, DEFAULT_BRANCH, RELEASE_SHA
set -euo pipefail

: "${VERSION:?set VERSION}"
: "${DEFAULT_BRANCH:?set DEFAULT_BRANCH}"
: "${RELEASE_SHA:?set RELEASE_SHA}"

files=(
  Cargo.toml
  Cargo.lock
  gemini-extension.json
  server.json
  plugins/claude/.claude-plugin/plugin.json
  plugins/codex/.codex-plugin/plugin.json
  plugins/cursor/.cursor-plugin/plugin.json
  .cursor-plugin/marketplace.json
)

if git diff --quiet -- "${files[@]}"; then
  echo "product versions already ${VERSION}"
  exit 0
fi

git fetch origin "${DEFAULT_BRANCH}"
tip="$(git rev-parse "origin/${DEFAULT_BRANCH}")"
if [ "${RELEASE_SHA}" != "$tip" ]; then
  echo "release commit is not ${DEFAULT_BRANCH} tip; skipping version commit"
  exit 0
fi

git config user.name "github-actions[bot]"
git config user.email "github-actions[bot]@users.noreply.github.com"
git switch -C "${DEFAULT_BRANCH}"
git add -- "${files[@]}"
git commit -m "(chore): set product versions to ${VERSION}"
git push origin "HEAD:${DEFAULT_BRANCH}"

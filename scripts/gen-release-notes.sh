#!/usr/bin/env bash
# Generate Release Notes from conventional-commit history.
#
# Usage:
#   scripts/gen-release-notes.sh [from_ref] [to_ref]
#
# Defaults:
#   from_ref = the previous tag (git describe on HEAD^), or the root commit
#   to_ref   = HEAD
#
# Output: markdown on stdout — grouped by commit type, with the change
# summary and sha range. Used by the release workflow as the --notes-file.

set -euo pipefail

TO="${2:-HEAD}"
if [ -n "${1:-}" ]; then
  FROM="$1"
else
  FROM="$(git describe --tags --abbrev=0 "HEAD^" 2>/dev/null || true)"
  if [ -z "$FROM" ]; then
    # No previous tag: start from the root commit.
    FROM="$(git rev-list --max-parents=0 HEAD)"
  fi
fi

if ! git rev-parse --verify "$FROM" >/dev/null 2>&1; then
  echo "error: unknown from_ref '$FROM'" >&2
  exit 1
fi

RANGE="$FROM..$TO"
COUNT="$(git rev-list --count "$RANGE")"
if [ "$COUNT" -eq 0 ]; then
  echo "error: empty range $RANGE (no commits between refs)" >&2
  exit 1
fi

short() { git rev-parse --short "$1"; }

echo "## What's Changed"
echo ""
echo "**$(short "$FROM")..$(short "$TO")** — $COUNT commits"
echo ""

# Group conventional-commit prefixes. Order matters: first match wins.
group() {
  local prefix="$1" title="$2"
  local items
  items="$(git log --pretty=format:'- %s (`%h`)' "$RANGE" | grep -E "^- ${prefix}" || true)"
  if [ -n "$items" ]; then
    echo "### $title"
    echo ""
    echo "$items"
    echo ""
  fi
}

group "feat" "Features"
group "fix" "Bug Fixes"
group "perf" "Performance"
group "refactor" "Refactoring"
group "build" "Build & Dependencies"
group "test" "Testing"
group "ci" "CI / Release"
group "docs" "Documentation"
group "style" "Style"

# Anything without a recognized prefix lands here.
OTHER="$(git log --pretty=format:'- %s (`%h`)' "$RANGE" | grep -Ev "^- (feat|fix|perf|refactor|build|test|ci|docs|style)" || true)"
if [ -n "$OTHER" ]; then
  echo "### Other"
  echo ""
  echo "$OTHER"
  echo ""
fi

echo "**Full Changelog**: \`$FROM..$TO\`"

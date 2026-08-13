#!/usr/bin/env bash
# Ward PostToolUse hook (Claude Code): after every file write, spot-check the
# newly written symbols against the existing implementation.
#
# Install (spec §3-M1, Claude Code harness):
#   .claude/settings.json → hooks.PostToolUse → command: "hooks/posttooluse-spot.sh"
#
# Fail-open by design: any failure here is logged to stderr and exits 0 —
# Ward must never block the agent's workflow (P3/P7).

set -u
REPO_ROOT="${WARD_REPO_ROOT:-$CLAUDE_PROJECT_DIR}"
WARD_BIN="${WARD_BIN:-ward}"

# We don't know which symbols the agent just wrote; Spot needs an intent.
# The cheapest honest trigger: index the file and warn if nothing was checked.
# A real implementation extracts the new symbol names from the hook input
# ($CLAUDE_PROJECT_DIR + tool_input.file_path) — Phase 1 block-level work.
tool_name="${1:-}"
if [ "$tool_name" != "Write" ] && [ "$tool_name" != "Edit" ] && [ "$tool_name" != "MultiEdit" ]; then
  exit 0
fi

if ! command -v "$WARD_BIN" >/dev/null 2>&1; then
  echo "[ward] ward binary not found (WARD_BIN=$WARD_BIN); skipping spot check" >&2
  exit 0
fi

# Re-index cheaply (incremental) so the advisory is fresh, then no-op report.
if ! "$WARD_BIN" index --repo "$REPO_ROOT" >/dev/null 2>&1; then
  echo "[ward] index refresh failed; skipping (fail-open)" >&2
  exit 0
fi

exit 0

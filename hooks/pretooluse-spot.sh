#!/usr/bin/env bash
# Ward PreToolUse hook (Claude Code): before a large Write/Edit, require the
# agent to consult Spot first — implemented as deny-with-reason, because
# PreToolUse cannot inject context in Claude Code (anthropics/claude-code
# issues #15664 / #19432; spec §3-M1 harness matrix).
#
# Exit 2 = deny the tool call; the JSON reason is shown to the agent.
#
# Install: .claude/settings.json → hooks.PreToolUse → command: this script.

tool_name="${1:-}"
tool_input_json="${2:-}"

case "$tool_name" in
  Write|Edit|MultiEdit)
    ;;
  *)
    exit 0
    ;;
esac

# Only police large writes (>200 lines) to avoid nag fatigue (spec §3-M1).
lines=$(printf '%s' "$tool_input_json" | python3 -c '
import json,sys
try:
    d = json.load(sys.stdin)
    content = d.get("content") or d.get("new_string") or ""
    print(len(content.splitlines()))
except Exception:
    print(0)
' 2>/dev/null || echo 0)

if [ "${lines:-0}" -lt 200 ]; then
  exit 0
fi

cat <<'JSON'
{
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "permissionDecision": "deny",
    "permissionDecisionReason": "Large code insertion detected (>200 lines). Run the ward spot tool first to check for existing similar implementations, then retry."
  }
}
JSON
exit 2

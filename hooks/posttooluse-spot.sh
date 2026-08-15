#!/usr/bin/env bash
# Ward PostToolUse hook (Claude Code): after every file write, spot-check the
# symbols the write introduced/changed against the existing implementation,
# and inject strong hits back as additionalContext.
#
# Protocol (Claude Code PostToolUse):
#   $1      — tool name (Write / Edit / MultiEdit)
#   $2      — tool_input JSON
#   stdout  — {"additionalContext": "..."} when strong hits exist, else empty
#
# Install (spec §3-M1, Claude Code harness):
#   .claude/settings.json → hooks.PostToolUse → command: "hooks/posttooluse-spot.sh"
#
# Fail-open by design: any failure here is logged to stderr and exits 0 —
# Ward must never block the agent's workflow (P3/P7).

set -u
REPO_ROOT="${WARD_REPO_ROOT:-$CLAUDE_PROJECT_DIR}"
WARD_BIN="${WARD_BIN:-ward}"

tool_name="${1:-}"
tool_input_json="${2:-}"

if [ "$tool_name" != "Write" ] && [ "$tool_name" != "Edit" ] && [ "$tool_name" != "MultiEdit" ]; then
  exit 0
fi

if ! command -v "$WARD_BIN" >/dev/null 2>&1; then
  echo "[ward] ward binary not found (WARD_BIN=$WARD_BIN); skipping spot check" >&2
  exit 0
fi

if [ ! -f "$REPO_ROOT/.ward/index.db" ]; then
  # First run ever: nothing to compare against yet. Build the index once so
  # the next writes get real checks; do NOT check this write (fail-open).
  "$WARD_BIN" index --repo "$REPO_ROOT" >/dev/null 2>&1 || true
  exit 0
fi

if ! command -v python3 >/dev/null 2>&1; then
  echo "[ward] python3 not found; skipping spot check (fail-open)" >&2
  exit 0
fi

# The store still holds the pre-write state (spot-file runs before the index
# refresh below). Collect strong hits (≥0.92) per written file into a
# temp file; python assembles the additionalContext envelope.
hits_file="$(mktemp)"
python3 - "$tool_input_json" "$WARD_BIN" "$REPO_ROOT" "$hits_file" <<'PYEOF'
import json, os, subprocess, sys
tool_input_json, ward_bin, repo_root, hits_file = sys.argv[1:5]
try:
    data = json.loads(tool_input_json)
except Exception:
    sys.exit(0)
paths = []
if isinstance(data.get("file_path"), str):
    paths.append(data["file_path"])
for edit in data.get("edits") or []:
    p = edit.get("file_path")
    if isinstance(p, str):
        paths.append(p)
SRC_EXT = (".rs", ".kt", ".kts", ".swift", ".java", ".m", ".mm", ".h")
hits = []
for p in list(dict.fromkeys(paths))[:5]:
    if not p.endswith(SRC_EXT):
        continue
    rel = os.path.relpath(p, repo_root) if os.path.isabs(p) else p
    try:
        out = subprocess.run(
            [ward_bin, "spot-file", "--repo", repo_root, "--path", rel, "--json"],
            capture_output=True, text=True, timeout=30)
        if out.returncode != 0:
            continue
        report = json.loads(out.stdout)
    except Exception:
        continue
    for adv in report.get("advisories") or []:
        for m in adv.get("matches") or []:
            if m.get("similarity", 0) >= 0.92:
                hits.append(
                    f'{rel} → {m["path"]}:{m["lines"]} {m["symbol"]}'
                    f' [{m["kind"]} {m["similarity"]:.2f}]'
                )
if hits:
    with open(hits_file, "w", encoding="utf-8") as f:
        json.dump(hits, f)
PYEOF

if [ -s "$hits_file" ]; then
  python3 - "$hits_file" <<'PYEOF'
import json, sys
hits = json.load(open(sys.argv[1], encoding="utf-8"))
ctx = ("Ward Spot：本次写入与现有实现强相似（≥0.92），优先复用/扩展现有实现"
       "（AGENTS.md 规则 2）：\n" + "\n".join(f"- {h}" for h in hits[:8]))
print(json.dumps({"additionalContext": ctx}, ensure_ascii=False))
PYEOF
fi
rm -f "$hits_file"

# Refresh the index so the NEXT write diffs against this state.
"$WARD_BIN" index --repo "$REPO_ROOT" >/dev/null 2>&1 || true
exit 0

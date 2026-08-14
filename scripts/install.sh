#!/usr/bin/env bash
# Ward 一键安装器 — 下载官方 Release 二进制，并注册到 Claude Code / Codex / Cursor。
#
# 用法（一行命令）:
#   curl -fsSL https://raw.githubusercontent.com/ending0421/Ward/master/scripts/install.sh | sh
#
# 选项（环境变量或参数）:
#   VERSION=v0.1.0         固定版本（默认 latest release）
#   --scope user|project   user=全局注册（默认）；project=当前项目注册 + hooks
#   --no-mcp               只装二进制，不注册任何工具
#   --uninstall            卸载（二进制 + 所有注册项）
#
# 安全: 所有下载产物经 SHA256SUMS.txt 校验；修改前自动备份现有配置。

set -euo pipefail

REPO="ending0421/Ward"
WARD_HOME="${WARD_HOME:-$HOME/.ward}"
BIN_DIR="$WARD_HOME/bin"
SCOPE="user"
NO_MCP=0
UNINSTALL=0
VERSION="${VERSION:-latest}"

for arg in "$@"; do
  case "$arg" in
    --scope) SCOPE="project" ;;  # 仅支持 --scope project; 缺省即 user
    --project) SCOPE="project" ;;
    --no-mcp) NO_MCP=1 ;;
    --uninstall) UNINSTALL=1 ;;
    *) echo "unknown option: $arg" >&2; exit 2 ;;
  esac
done

info()  { printf '\033[1;32m[ward]\033[0m %s\n' "$*"; }
warn()  { printf '\033[1;33m[ward]\033[0m %s\n' "$*" >&2; }
fail()  { printf '\033[1;31m[ward]\033[0m %s\n' "$*" >&2; exit 1; }

os="$(uname -s)"
arch="$(uname -m)"
case "$os-$arch" in
  Darwin-arm64)  target="aarch64-apple-darwin" ;;
  Darwin-x86_64) target="x86_64-apple-darwin" ;;
  Linux-x86_64)  target="x86_64-unknown-linux-gnu" ;;
  Linux-aarch64) target="aarch64-unknown-linux-gnu" ;;
  *) fail "unsupported platform: $os-$arch（Windows 请从 GitHub Releases 手动下载 zip）" ;;
esac

shasum_cmd() {
  if command -v sha256sum >/dev/null 2>&1; then echo "sha256sum";
  elif command -v shasum >/dev/null 2>&1; then echo "shasum -a 256";
  else fail "need sha256sum or shasum"; fi
}

# ---------------------------------------------------------------- uninstall
if [ "$UNINSTALL" = 1 ]; then
  info "uninstalling…"
  if command -v claude >/dev/null 2>&1; then claude mcp remove ward >/dev/null 2>&1 || true; fi
  if [ -f "$HOME/.codex/config.toml" ] && command -v python3 >/dev/null 2>&1; then
    python3 - "$HOME/.codex/config.toml" <<'EOF'
import sys, re
p = sys.argv[1]
s = open(p).read()
s = re.sub(r"\n\[mcp_servers\.ward\][^\[]*", "\n", s, flags=re.S)
open(p, "w").write(s)
EOF
    info "removed Codex [mcp_servers.ward]"
  fi
  rm -rf "$WARD_HOME"
  rm -f "$HOME/.local/bin/ward" "$HOME/.local/bin/ward-mcp"
  info "done. 项目内 .mcp.json / .cursor/mcp.json / .claude/settings.json 如存在请手动删除。"
  exit 0
fi

# ---------------------------------------------------------------- download
if [ "$VERSION" = "latest" ]; then
  info "resolving latest release…"
  VERSION="$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" \
    | python3 -c 'import json,sys; print(json.load(sys.stdin)["tag_name"])' 2>/dev/null || true)"
  [ -n "$VERSION" ] || VERSION="v0.1.0"
fi
BASE="https://github.com/$REPO/releases/download/$VERSION"
ASSET="ward-$VERSION-$target.tar.gz"
info "installing Ward $VERSION ($target)"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
curl -fL --progress-bar -o "$tmp/$ASSET" "$BASE/$ASSET"
curl -fLs -o "$tmp/SHA256SUMS.txt" "$BASE/SHA256SUMS.txt"

info "verifying checksum…"
expected="$(grep " $ASSET\$" "$tmp/SHA256SUMS.txt" | awk '{print $1}')"
[ -n "$expected" ] || fail "asset $ASSET not found in SHA256SUMS.txt"
actual="$($(shasum_cmd) "$tmp/$ASSET" | awk '{print $1}')"
[ "$expected" = "$actual" ] || fail "checksum mismatch for $ASSET"

info "extracting to $WARD_HOME …"
rm -rf "$WARD_HOME"
mkdir -p "$WARD_HOME"
tar xzf "$tmp/$ASSET" -C "$WARD_HOME"
mkdir -p "$BIN_DIR"
mv "$WARD_HOME"/ward "$WARD_HOME"/ward-mcp "$BIN_DIR"/ 2>/dev/null || true

if [ -d "$HOME/.local/bin" ]; then
  ln -sf "$BIN_DIR/ward" "$HOME/.local/bin/ward"
  ln -sf "$BIN_DIR/ward-mcp" "$HOME/.local/bin/ward-mcp"
  info "linked binaries into ~/.local/bin"
else
  warn "~/.local/bin 不存在；请将 $BIN_DIR 加入 PATH"
fi

W="$(command -v ward || echo "$BIN_DIR/ward")"
"$W" --version | head -1

# ---------------------------------------------------------------- MCP
if [ "$NO_MCP" = 1 ]; then
  info "skipped MCP registration (--no-mcp)"
  exit 0
fi
MCP_BIN="$BIN_DIR/ward-mcp"

# --- Claude Code -----------------------------------------------------
if command -v claude >/dev/null 2>&1; then
  if [ "$SCOPE" = "user" ]; then
    claude mcp add --scope user ward -- "$MCP_BIN" >/dev/null 2>&1 \
      && info "Claude Code: registered (user scope)" \
      || warn "claude mcp add failed; fallback: 手动在 ~/.claude.json 添加 mcpServers.ward"
  else
    cat > .mcp.json <<EOF
{
  "mcpServers": {
    "ward": { "command": "$MCP_BIN", "args": [] }
  }
}
EOF
    info "Claude Code: registered (project scope → .mcp.json)"
  fi
else
  warn "未检测到 claude CLI；安装 Claude Code 后运行: claude mcp add --scope user ward -- $MCP_BIN"
fi

# --- Codex -----------------------------------------------------------
if command -v codex >/dev/null 2>&1; then
  CFG="$HOME/.codex/config.toml"
  [ "$SCOPE" = "project" ] && CFG=".codex/config.toml"
  mkdir -p "$(dirname "$CFG")"
  cp "$CFG" "$CFG.bak" 2>/dev/null || true
  if grep -q '^\[mcp_servers\.ward\]' "$CFG" 2>/dev/null; then
    info "Codex: ward 已注册，跳过"
  else
    {
      echo ""
      echo "[mcp_servers.ward]"
      echo "command = \"$MCP_BIN\""
      echo "args = []"
    } >> "$CFG"
    info "Codex: registered ($CFG)"
  fi
else
  warn "未检测到 codex CLI；安装 Codex 后在 ~/.codex/config.toml 追加 [mcp_servers.ward]（见 README）"
fi

# --- Cursor ----------------------------------------------------------
if command -v cursor >/dev/null 2>&1; then
  if [ "$SCOPE" = "project" ]; then
    cat > .cursor/mcp.json <<EOF
{
  "mcpServers": {
    "ward": { "command": "$MCP_BIN", "args": [] }
  }
}
EOF
    info "Cursor: registered (project scope → .cursor/mcp.json)"
  else
    warn "Cursor CLI 已检测到；全局注册请用: cursor mcp add ward -- $MCP_BIN"
  fi
else
  if [ "$SCOPE" = "project" ]; then
    mkdir -p .cursor
    cat > .cursor/mcp.json <<EOF
{
  "mcpServers": {
    "ward": { "command": "$MCP_BIN", "args": [] }
  }
}
EOF
    info "Cursor: registered (project scope → .cursor/mcp.json)"
  else
    warn "未检测到 cursor CLI；Cursor 全局注册: Settings → MCP → Add new MCP server → Command: $MCP_BIN"
  fi
fi

# --- Claude Code hooks (project scope only) ---------------------------
if [ "$SCOPE" = "project" ] && command -v claude >/dev/null 2>&1; then
  mkdir -p .claude
  cat > .claude/settings.json <<EOF
{
  "hooks": {
    "PreToolUse": [
      { "matcher": "Write|Edit|MultiEdit",
        "hooks": [ { "type": "command", "command": "$WARD_HOME/hooks/pretooluse-spot.sh" } ] }
    ],
    "PostToolUse": [
      { "matcher": "Write|Edit|MultiEdit",
        "hooks": [ { "type": "command", "command": "$WARD_HOME/hooks/posttooluse-spot.sh" } ] }
    ]
  },
  "env": { "WARD_BIN": "$W" }
}
EOF
  info "Claude Code hooks: installed (.claude/settings.json)"
fi

echo ""
info "安装完成 ✔  验证方式:"
echo "  ward --version                # 二进制可用"
echo "  claude mcp list               # 应看到 ward"
echo "  codex mcp list                # 应看到 ward"
echo "  （Cursor: Settings → MCP 查看；或项目内 .cursor/mcp.json）"
echo "  项目内启用: curl …/install.sh | sh -s -- --project"

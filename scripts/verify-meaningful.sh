#!/usr/bin/env bash
# Ward 意义验证：已知案例召回/精度实验 + 对抗性 fail-open/fail-closed 检查。
#
# 用法（装好 ward 后）:
#   scripts/verify-meaningful.sh [ward_bin]
#
# 它构造一个真实 git 仓库，埋入四类已知案例，用真实二进制跑完整管线，
# 逐项断言预期行为。任何一项不符 → 红色退出（fail-closed 自检）。
#
# 注意：这验证的是"引擎行为正确且不撒谎"（第 1-2 层）。
# "真正有意义"（第 3 层）还需要真实工作流数据——见 README §9 指标与本文末尾说明。

set -euo pipefail

WARD="${1:-ward}"
case "$WARD" in
  /*) ;;
  *) WARD="$(cd "$(dirname "$WARD")" && pwd)/$(basename "$WARD")" ;;
esac
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
cd "$TMP"
git init -q -b master
git config user.name t && git config user.email t@e.c

pass=0; fail=0
check() { # check <name> <expected_behavior>
  local name="$1" got="$2" want="$3"
  if [ "$got" = "$want" ]; then
    printf '  \033[1;32mPASS\033[0m %s\n' "$name"; pass=$((pass+1))
  else
    printf '  \033[1;31mFAIL\033[0m %s（期望 %s，实际 %s）\n' "$name" "$want" "$got"; fail=$((fail+1))
  fi
}

echo "== 准备：埋入四类已知案例 =="
mkdir -p src
cat > Cargo.toml <<'TOML'
[package]
name = "ward-harness-fixture"
version = "0.1.0"
TOML
# 案例 1: 原实现
cat > src/original.rs <<'RS'
pub fn debounce(f: &dyn Fn(u64), ms: u64) -> u8 { f(ms); 0 }
RS
# 案例 2: 精确克隆（copy-paste）
cat > src/clone.rs <<'RS'
pub fn debounce_clone(f: &dyn Fn(u64), ms: u64) -> u8 { f(ms); 0 }
RS
# 案例 3: copy-then-modify（改一个字面量）
cat > src/near.rs <<'RS'
pub fn debounce_near(f: &dyn Fn(u64), ms: u64) -> u8 { f(ms); 1 }
RS
# 案例 4: 无关实现（精度对照组）
cat > src/unrelated.rs <<'RS'
pub fn quicksort(v: &mut [i32]) { if v.len() <= 1 { return } let p = v[0]; quicksort(&mut v[1..]) }
RS
git add -A && git commit -q -m base

echo ""
echo "== 第 1 层：功能可用（真实二进制全命令） =="
"$WARD" index --repo . >/tmp/ward-index.log 2>&1 && check "index 全量成功" 0 0 || check "index 全量成功" 1 0
"$WARD" catch-run --repo . >/tmp/ward-catch.log 2>&1; check "catch-run 有裁决输出" "$(grep -c 'catch_run:' /tmp/ward-catch.log)" "1"

echo ""
echo "== 第 2 层：已知案例召回/精度（引擎行为正确） =="
# 2a: 精确克隆 → L1 structural, sim 1.0
"$WARD" spot --repo . --intent "防抖" \
  --signature "pub fn debounce(f: &dyn Fn(u64), ms: u64) -> u8 { f(ms); 0 }" --json > /tmp/s1.json
HITS=$(python3 - <<'PY'
import json
r = json.load(open("/tmp/s1.json"))
m = [x for x in r["data"]["matches"] if x["path"] == "src/clone.rs"]
print("yes" if m and m[0]["kind"] == "structural" and m[0]["similarity"] == 1.0 else "no")
PY
)
check "精确克隆被 L1 命中（kind=structural, sim=1.0）" "$HITS" "yes"

# 2b: copy-then-modify → near, sim ≥ 0.8
"$WARD" spot --repo . --intent "防抖" \
  --signature "pub fn debounce(f: &dyn Fn(u64), ms: u64) -> u8 { f(ms); 0 }" --json > /tmp/s2.json
HITS=$(python3 - <<'PY'
import json
r = json.load(open("/tmp/s2.json"))
m = [x for x in r["data"]["matches"] if x["path"] == "src/near.rs"]
print("yes" if m and m[0]["similarity"] >= 0.8 else "no")
PY
)
check "copy-then-modify 被 L2 命中（sim ≥ 0.8）" "$HITS" "yes"

# 2c: 无关函数不得误报（精度）
"$WARD" spot --repo . --intent "快排" \
  --signature "pub fn quicksort(v: &mut [i32])" --json > /tmp/s3.json
HITS=$(python3 -c "
import json
r = json.load(open('/tmp/s3.json'))
print('no' if all('debounce' not in x['symbol'] for x in r['data']['matches']) else 'yes')
")
check "无关函数不产生 debounce 误报" "$HITS" "no"

echo ""
echo "== 第 3 层前置：对抗性语义（不撒谎、该硬则硬该软则软） =="
# 3a: F3 — 坏文件整体跳过，其余正常
echo 'fn broken( {' > src/broken.rs
"$WARD" index --repo . >/tmp/ward-index2.log 2>&1
check "坏文件被 F3 跳过（unparsable=1）且索引仍成功" "$(grep -c '1 unparsable' /tmp/ward-index2.log)" "1"
rm src/broken.rs
# 3b: per-file 新鲜度 — 未提交修改 → stale
echo '// tweak' >> src/original.rs
"$WARD" spot --repo . --intent "防抖" --signature "pub fn debounce(f: &dyn Fn(u64), ms: u64) -> u8 { f(ms); 0 }" --json > /tmp/s4.json
check "未提交修改 → advisory stale=true（绝不假装新鲜）" "$(python3 -c "import json; print(json.load(open('/tmp/s4.json'))['data']['stale'])")" "True"
git checkout -q src/original.rs
# 3c: 外环裁决一致性不变量 —— 沙箱可用 ⇔ 真裁决(pass/fail)；沙箱不可用 ⇔ unknown
"$WARD" verify --full --repo . --json > /tmp/v.json 2>&1 || true
V=$(python3 -c "import json; print(json.load(open('/tmp/v.json'))['data']['verdict'])" 2>/dev/null || echo missing)
if docker info >/dev/null 2>&1; then
  check "有 Docker：verify --full 给出真实裁决（pass/fail，非 unknown、非 fake green）" "$([ "$V" = "pass" ] || [ "$V" = "fail" ] && echo ok || echo bad)" "ok"
else
  check "无 Docker：verify --full = unknown（绝不 fake green）" "$V" "unknown"
fi
# 3d: M4 语义 — must_pass 必须 deferred，api_compat 必须 unknown
mkdir -p specs
printf '# s\n```yaml\nassertions:\n  - kind: must_pass\n  - kind: api_compat\n```\n' > specs/t.md
git add -A && git commit -q -m "add spec"   # 第二个提交，让 base/head 差值有真实内容
"$WARD" index --repo . >/dev/null 2>&1
"$WARD" form-check --repo . --spec specs/t.md >/tmp/fc.log 2>&1
check "must_pass → deferred（不伪造测试通过）" "$(grep -c '\[deferred\] must_pass' /tmp/fc.log)" "1"
check "api_compat → unknown（无工具无裁决）" "$(grep -c '\[unknown\] api_compat' /tmp/fc.log)" "1"
# 3e: 无 LLM → 诚实"未执行"
"$WARD" intent-check --repo . --requirement "实现防抖" --json > /tmp/ic.json 2>&1
check "无 LLM provider → intent-check executed=false（不伪造判断）" "$(python3 -c "import json; print(json.load(open('/tmp/ic.json'))['data']['executed'])")" "False"

echo ""
echo "======================================"
if [ "$fail" -eq 0 ]; then
  printf '\033[1;32m全部 %d 项通过 ✔\033[0m\n' "$pass"
  echo "第 1-2 层验证完成：引擎对已知案例的召回/精度正确、fail-open/fail-closed 语义不撒谎。"
  exit 0
else
  printf '\033[1;31m%d 项失败 ✘\033[0m（共 %d 项）\n' "$fail" "$((pass+fail))"
  exit 1
fi

# AGENTS.md — Ward 自举工作规则

> 本文件被 DeepSeek Harness / Claude Code / Codex 自动加载。
> Ward 在此仓库内**校验自己的开发过程**（dogfood）：写 Ward 代码时，
> 每个 Agent 必须走 Ward 自己的护栏。

## 写代码前

1. 实现任何新函数前，先调用 `spot`（`ward spot --repo . --intent "<意图>" --signature "<拟写签名>"`）。
2. `structural` 或 `similarity >= 0.92` 的 `near` 命中 → **必须复用/扩展现有实现**，
   并在回复中说明为什么不复用（如无理由而重复造轮子，视为违反本规则）。
3. 新函数涉及多语句体时，附 `--body` 触发块级查重。

## 写代码后

4. 每次实现完成，运行 `ward catch-run --repo .`；失败必须修复或说明原因。
5. 每次改动后提交前，运行 `ward index --repo .` 保持索引新鲜
   （否则后续 advisory 会标 stale）。
6. 收到任何 advisory 后，用 `ward action <advisory_id> accepted|ignored|dismissed`
   回写处置——不回写会污染黄金集校准。

## 提交纪律

7. Commit message 遵循 conventional commits（feat/fix/refactor/test/docs/ci/style/build）。
8. 涉及规格任务的改动引用条款号：`[spec:<条款>]`。
9. 需求/断言有变 → 改 `specs/` 走 PR 评审，不绕过断言（F12）。

## 自校验（每周或发布前）

10. `scripts/verify-meaningful.sh $(command -v ward)` — 11 项召回/精度与对抗语义检查，必须全绿。
11. `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`。
12. `ward clusters --repo .` 检查存量重复是否在增长。

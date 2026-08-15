# Release vX.Y.Z

<!--
  Ward Release Notes 规范模板。
  每份 notes 发布前必须人工审校（与代码同 PR 评审，AGENTS.md 规则 9）。

  写作要求（"比通用做法更进一步"）：
  1. Highlights 是"本版本的叙事"——3-5 条，回答"这一版为什么值得升级"；
  2. Breaking Changes 必须含**迁移指引**（用户看完能自己升级）；
  3. 每个功能条目写"是什么 + 怎么用"，给命令/配置示例；
  4. Verification 数字必须来自发布时实测（测试数/覆盖率/意义验证项数），
     不写没测过的数字；
  5. Upgrade 段必须写 schema 版本与索引/配置兼容性（F1 自动重建等）；
  6. Full Changelog 由流水线自动拼入（gen-release-notes.sh），人工勿改。
-->

## Highlights

- （本版本最重要的 3-5 件事，一句话一条）

## Breaking Changes

- 无。（或逐条列出 + 迁移指引）

## Features

### （模块名）

- 描述 + 用法示例。

## Fixes

- 描述 + 修复前症状。

## Internal / Refactoring

- 纯内部改动。

## Testing / CI

- 新增的测试与门禁。

## Docs

- 文档变更。

## Verification

| 指标 | 值 |
| :--- | :--- |
| 测试 | N passed / 0 failed（发布 tag 实测） |
| clippy | `-D warnings` 零告警 |
| rustfmt | 干净 |
| 覆盖率 | workspace 行覆盖 X%（llvm-cov，门禁 ≥85%） |
| 意义验证 | scripts/verify-meaningful.sh 11/11 |

## Artifacts & Checksums

预构建二进制（5 平台）与 SHA256SUMS.txt 见本 Release 资产；
安装：`curl -fsSL https://raw.githubusercontent.com/ending0421/Ward/master/scripts/install.sh | sh`

## Upgrade

- 从 vX.Y.Z 升级：`ward` 单二进制替换即可；索引 schema vN → 首次打开自动重建（F1），仅损失速度；
- 配置 `.ward/config.toml` 无破坏性变更（如有变更在此说明）。

## Contributors

- （git shortlog 统计）

## Full Changelog

（流水线自动拼入：gen-release-notes.sh 分组输出）

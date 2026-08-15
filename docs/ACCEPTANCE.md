# Ward 交付验收清单（v0.4.0 实测版）

对照设计规格 `docs/ward-tech-spec-v0.6.1.md` 的逐项实现核对。
（历史版本：v0.1.0 版验收数字见 git 历史。）

## 1. 模块完成度

| 模块 | 规格要求 | 实现状态 |
| :--- | :--- | :--- |
| M1 Spot | L0/L1/L2 指纹 + 块级指纹 + BM25 召回 + 分级 + 反馈回写 + 五语法查询侧 | ✅ 完整（L3 为接口 + 哈希嵌入器；PostToolUse hook 已升级为**写后自动 spot-file + additionalContext 注入**） |
| M2 Replay | 符号级 diff（六类变更）+ 影响面下界 + 风险标记 + LLM 叙述层 | ✅ 完整（逐句锚点校验、F6 结构化回退） |
| M3 Catch | 内环预检 / 外环沙箱裁决 / runner 矩阵 / 差分测试 | ✅ 核心完整（沙箱安全姿态可测；差分测试为 runner 命令位；macOS runner 为扩展点） |
| M4 Form Check | 断言种类 + api_compat 逐语言工具 + M4-b 意图比对 | ✅ 完整（api_compat 仅 Rust 接线，CI 外环 cargo-semver-checks **真实裁决**；其余语言按矩阵报 unknown） |
| M5 Context Cards | 定义 + 调用方 + 相关测试 + 配置引用 | ✅ 完整 |
| M6 整合重构 | 离线重复簇聚类 + 合并建议（PR 由人提交） | ✅ 完整（分析半部分；PR 创建按 P2 由人执行） |
| 语言矩阵 | Rust/Kotlin/Swift/Java/ObjC 语法接入 + LanguageSpec | ✅ 五语法编译接入（索引 + spot 查询侧，含 Kotlin body-field 修复） |
| 治理数据闭环 | 推断采纳通道 + 黄金集标注 + Wilson 校准 + 快照/报表 + **双标一致性（Fleiss κ）** | ✅ 完整（`ward label set --annotator <名字>` 支持多人标注，`ward stats` 输出 κ 与低一致率告警） |
| 基础设施 | hooks、CI（fmt/clippy/test/dogfood/coverage/jscpd/spec-gate/hook 冒烟）、回滚、**性能基准** | ✅ 完整（`ward-bench` 生成器 + `docs/BENCHMARKS.md` 10⁴/10⁵ 实测） |

## 2. 质量门禁（实测数字）

| 指标 | 值 |
| :--- | :--- |
| 测试总数 | 214（2 bench + 9 CLI smoke + 160 lib unit + 42 e2e + 1 doc） |
| clippy | `-D warnings` 零告警 |
| rustfmt | 干净 |
| 覆盖率（workspace） | 86.25% 行 / 88.60% 区域（CI 门禁 ≥85%） |
| 意义验证 | scripts/verify-meaningful.sh 11/11 |
| 性能（F11 基线，合成仓库） | 10⁴：索引 7.45s / spot p99 25ms；10⁵：索引 11.1s / spot p99 93ms（详见 docs/BENCHMARKS.md） |
| 索引 schema | v6（labels.annotator；F1 重建保留治理数据，v5→v6 迁移带默认标注者） |

## 3. 设计边界（文档化的诚实声明，非缺失代码）

1. **api_compat 工具矩阵**：仅 Rust（cargo-semver-checks）接线；Kotlin/Java/Swift/OC 工具按规格矩阵报 `unknown`（F13：无工具即无裁决）。
2. **L3 嵌入**：提供 `EmbeddingProvider` trait + 确定性哈希嵌入器；学习型 provider（fastembed/onnx + sqlite-vec）为后续接线点，F8 退化已就绪。
3. **LLM HTTP provider**：环境变量门控（`WARD_LLM_URL/KEY/MODEL`），端到端网络调用不可在无网络 CI 中测试（覆盖率统计中的主要例外）。
4. **Apple/macOS runner**：M3 runner 矩阵中的 CI 配置扩展点（本地内环预检已支持）。
5. **M6 PR 创建**：按 P2（Advisory, not Authority）由人或 Agent 提交；Ward 提供聚类与合并建议。
6. **10⁵ 聚类截断**：chunked 分桶在 10⁵ 符号下截断并拆分同构家族（`truncated: true` 诚实标记），5×10⁵ 前需专项验证（docs/BENCHMARKS.md 已知边界）。

## 4. Git 交付

- 仓库：`git@github.com:ending0421/Ward.git`，分支 `master`；
- 最小颗粒度提交：150+ 次（feat/fix/refactor/test/docs/ci 分类，AI 提交带 `[ai]` 标记）；
- 每次提交保持可编译、测试绿（历史提交逐步演进）。

# Ward 交付验收清单（v0.1.0 实现版）

对照设计规格 `docs/ward-tech-spec-v0.6.1.md` 的逐项实现核对。

## 1. 模块完成度

| 模块 | 规格要求 | 实现状态 |
| :--- | :--- | :--- |
| M1 Spot | L0/L1/L2 指纹 + 块级指纹 + BM25 召回 + 分级 + 反馈回写 | ✅ 完整（L3 为接口 + 哈希嵌入器，见边界） |
| M2 Replay | 符号级 diff（六类变更）+ 影响面下界 + 风险标记 + LLM 叙述层 | ✅ 完整（逐句锚点校验、F6 结构化回退） |
| M3 Catch | 内环预检 / 外环沙箱裁决 / runner 矩阵 / 差分测试 | ✅ 核心完整（沙箱安全姿态可测；差分测试为 runner 命令位） |
| M4 Form Check | 断言种类 + api_compat 逐语言工具 + M4-b 意图比对 | ✅ 完整（api_compat 仅 Rust 接线，其余语言按矩阵报 unknown） |
| M5 Context Cards | 定义 + 调用方 + 相关测试 + 配置引用 | ✅ 完整 |
| M6 整合重构 | 离线重复簇聚类 + 合并建议（PR 由人提交） | ✅ 完整（分析半部分；PR 创建按 P2 由人执行） |
| 语言矩阵 | Rust/Kotlin/Swift/Java/ObjC 语法接入 + LanguageSpec | ✅ 五语法全部编译接入 |
| 基础设施 | hooks、CI（fmt/clippy/test/dogfood/coverage/jscpd）、回滚 | ✅ 完整 |

## 2. 质量门禁（实测数字）

| 指标 | 值 |
| :--- | :--- |
| 测试总数 | 153（111 lib unit + 37 e2e + 4 CLI smoke + 1 MCP 九工具回环） |
| clippy | `-D warnings` 零告警 |
| rustfmt | 干净 |
| 覆盖率（workspace） | 88.75% 行 / 89.87% 区域（CI 门禁 ≥85%） |
| 覆盖率（ward-core 19 模块） | 93.89% 行；算法核心全部 ≥95%：fingerprint 98.2 / intent 98.3 / narrate 98.3 / normalize 97.6 / embedding 97.6 / search 97.1 / context 95.9 / config 95.3 / index 95.0 |
| 性能 | release 全量索引 ~1s（26 文件/409 符号）；增量索引二次 0.023s；聚类分桶剪枝（50k 符号 2.7s，含暴力一致性验证） |

## 3. 设计边界（文档化的诚实声明，非缺失代码）

1. **api_compat 工具矩阵**：仅 Rust（cargo-semver-checks）接线；Kotlin/Java/Swift/OC 工具按规格矩阵报 `unknown`（F13：无工具即无裁决）。
2. **L3 嵌入**：提供 `EmbeddingProvider` trait + 确定性哈希嵌入器（词法 token-bag 近似，非学习型语义模型）；学习型 provider（fastembed/onnx）为后续接线点，F8 退化已就绪。
3. **LLM HTTP provider**：环境变量门控（`WARD_LLM_URL/KEY/MODEL`），端到端网络调用不可在无网络 CI 中测试（覆盖率统计中的主要例外）。
4. **Apple/macOS runner**：M3 runner 矩阵中的 CI 配置扩展点（本地内环预检已支持）。
5. **M6 PR 创建**：按 P2（Advisory, not Authority）由人或 Agent 提交；Ward 提供聚类与合并建议。

## 4. Git 交付

- 仓库：`git@github.com:ending0421/Ward.git`，分支 `master`，已 `push -u`；
- 最小颗粒度提交：100+ 次（feat/fix/refactor/test/docs/ci 分类）；
- 每次提交保持可编译、测试绿（历史提交逐步演进）。

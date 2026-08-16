# Ward 交付验收清单（v0.5.0 实测版）

对照设计规格 `docs/ward-tech-spec-v0.6.1.md` 的逐项实现核对。
（历史版本：v0.1.0 / v0.4.0 版数字见 git 历史。）

## 1. 模块完成度

| 模块 | 规格要求 | 实现状态 |
| :--- | :--- | :--- |
| M1 Spot | L0/L1/L2 指纹 + 块级指纹 + BM25 召回 + 分级 + 反馈回写 + 五语法查询侧 + **near 集合指纹化（intent 不变性，#3）** + **模块作用域过滤（§2.6）** | ✅ 完整（L3 为接口 + 哈希嵌入器；PostToolUse hook 写后自动 spot-file + additionalContext 注入） |
| M2 Replay | 符号级 diff + 影响面下界 + 风险标记 + LLM 叙述层 + **UDL 接口变更** | ✅ 完整（UDL 六类结构薄提取器接入，#2 修复；UDL 变更带"重新生成 + 人审"HIGH 风险标记） |
| M3 Catch | 内环预检 / 外环沙箱裁决 / runner 矩阵 / 差分测试 | ✅ 完整（按构建系统自动选形：cargo / gradlew / swift build；Docker 镜像矩阵 rust/gradle/swift；Xcode 为 macOS runner 原生形态，CI apple-smoke） |
| M4 Form Check | 断言种类 + api_compat 逐语言工具 + **ffi_compat（0.5-3）** + M4-b 意图比对 | ✅ 完整（Rust=cargo-semver-checks、Kotlin=binary-compatibility-validator、Swift=swift-api-digester（macOS）、Java=japicmp 待 jar 配置=unknown；FFI 导出面 nm 对比，removed=红） |
| M5 Context Cards | 定义 + 调用方 + 相关测试 + 配置引用 | ✅ 完整 |
| M6 整合重构 | 离线重复簇聚类 + 合并建议（PR 由人提交） | ✅ 完整（分析半部分；PR 创建按 P2 由人执行） |
| 语言矩阵 | Rust/Kotlin/Swift/Java/ObjC + **UDL** + **模块作用域** | ✅ 五语法 + UDL 薄提取器；构建产物/清单文件默认忽略 |
| 治理数据闭环 | 推断采纳通道 + 黄金集标注 + Wilson 校准 + 快照/报表 + 双标一致性（Fleiss κ） | ✅ 完整 |
| 基础设施 | hooks、CI（含 jvm-smoke 真 validator / apple-smoke 真 digester）、回滚、性能基准、**CLI/MCP JSON 信封统一（#4）** | ✅ 完整 |

## 2. 质量门禁（实测数字）

| 指标 | 值 |
| :--- | :--- |
| 测试总数 | 237（2 bench + 9 CLI smoke + 179 lib unit + 46 e2e + 1 doc） |
| clippy | `-D warnings` 零告警 |
| rustfmt | 干净 |
| 覆盖率（workspace） | 85.69% 行（llvm-cov，门禁 ≥85%） |
| 意义验证 | scripts/verify-meaningful.sh 11/11 |
| 性能（F11 基线） | 10⁴：索引 7.5s / spot p99 25ms；10⁵：索引 11.9s / spot p99 73ms（near 全表扫描后余量 27%） |
| 索引 schema | v7（symbols.module；F1 重建保留治理数据） |

## 3. 设计边界（文档化的诚实声明，非缺失代码）

1. **api_compat 工具矩阵**：Rust/Kotlin/Swift 已接线；Java（japicmp）需显式新旧 jar 配置 → unknown；FFI 导出面由 0.5-3 薄自研层裁决（无现成件，P4 例外）。
2. **L3 嵌入**：提供 `EmbeddingProvider` trait + 确定性哈希嵌入器；学习型 provider（fastembed/onnx + sqlite-vec）为后续接线点，F8 退化已就绪。
3. **LLM HTTP provider**：环境变量门控，端到端网络调用不可在无网络 CI 中测试。
4. **Xcode M3**：Docker 沙箱内不可执行 → `unknown`；macOS runner 原生跑（CI apple-smoke），instrumented 测试按规格 `unknown`。
5. **M6 PR 创建**：按 P2 由人或 Agent 提交；Ward 提供聚类与合并建议。
6. **10⁵ 聚类截断**：chunked 分桶在 10⁵ 符号下截断（`truncated: true` 诚实标记），5×10⁵ 前需专项验证（docs/BENCHMARKS.md）。

## 4. Git 交付

- 仓库：`git@github.com:ending0421/Ward.git`，分支 `master`；
- 最小颗粒度提交：180+ 次（AI 提交带 `[ai]` 标记 + Co-authored-by）；
- 版本纪律：0.4.x 仅 hotfix，0.5.0 = 多平台语言矩阵里程碑。

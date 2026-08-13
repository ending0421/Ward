# Ward 技术方案

> **Ward off AI slop.**
>
> **文档版本**：v0.6.1（V6.1，Probe 竞品深度分析稿）
> **状态**：Draft / 可落地实施方案（已吸收三轮评审意见与产品定位决策）
> **目标读者**：AI 工程师、平台工程师、工程效能团队
> **定位**：AI Agent Coding 的**护栏与验证层**——不替换 Git，不持有真理，只做三件小事并把它们做对。

**v0.6.1 变更记录**（Probe 竞品深度分析，基于一手调研：README 与 ARCHITECTURE.md 全文核验）：

1. **新增 §11.5 竞品对比：Ward vs Probe（probelabs/probe）**。一手调研结论：Probe 定位"零索引的 AST 结构检索 + 内建理解型 Agent"；架构为 **Rust 检索内核 + Node.js SDK/CLI/MCP 包装层**（分发第一路径 `npx`，Node 是硬依赖）；四 MCP 工具 search/query/extract/symbols；Elasticsearch 式布尔查询 + BM25/TF-IDF/hybrid 排序 + SIMD 加速；确定性无状态（同查询同结果、无过期索引）；可选 BERT rerank；Agent 层多 provider、四种 persona、`--allow-edit`。
2. **三点关键差异化发现**：(a) **Probe 零索引即无指纹**——它是检索不是查重，Ward 的四层指纹 + 采纳/拒绝闭环属不同问题域；(b) **语言盲区**——Probe 语言列表含 Java/Swift 但**无 Kotlin/Objective-C**，恰是 Ward 首选矩阵的差异点；(c) **分发形态**——Probe 的 MCP/Agent 层运行在 Node.js，移动工程师环境（无 Node 工具链）是其真实摩擦，Ward 单静态二进制是差异化优势。
3. **竞合策略落位**：互补 > 竞争。**集成点 = M5**（若启动，检索后端进程级集成 Probe CLI 而非自研，§3-M5 强化；Probe 无 Kotlin/OC，这两门语言 M5 需自行桥接或降级）；**不集成点 = M1**（指纹需持久索引，Probe 零索引模型不适用）。**威胁监控**：Probe 正向理解型 Agent 演进（--allow-edit/personas），若横向扩展到查重/治理/验证域将直接撞上 M1/M2/M3——与 Vet、平台方内建并列为三个季度跟踪向量；反制策略是继续押注指纹-度量-验证数据闭环，不跟随其 agent 化。
4. §3.0、§3-M5、§11.4、§12 相应更新。

**版本沿革**：v0.3.0（命名 + 四道结界 + Vet 竞品对比）→ v0.4.0（六 P0 修复：四层指纹、内外环拆分、P7 两环分治、行为级断言、指标操作化）→ v0.5.0（因果方向修正、可移植性声明、计划与度量补齐）→ v0.6.0（Rust 全栈 + 五语言首选矩阵）→ v0.6.1（本稿：Probe 深度竞品对比）。

---

## 0. 本方案与 S-VCS 的关系

本方案是对《S-VCS 架构与技术规格书 v1.0.0》的重写。评审确认 S-VCS 的痛点嗅觉基本真实，但其核心架构（AST 真理源、Bi-Sync 双向同步、WASM 行为证明、无人值守重构）违反已知工程约束。本方案：

- **保留**两个经调研验证的真空白：生成阶段查重拦截、语义化变更摘要；
- **补上**S-VCS 漏掉的两个更痛的痛点：意图/规格漂移、验证缺失；
- **砍掉**全部"真理源 / 证明 / 替代"叙事，反转为"索引 / 测试 / 建议"；
- **补齐**一份规格书应有的内容：数据模型、一致性协议、失败模式目录、安全模型、MVP 里程碑、评估基准与毕业门槛。

### 0.1 设计原则（七条铁律）

| # | 原则 | 含义 |
| :--- | :--- | :--- |
| P1 | **文本与 Git 永远是唯一真理源** | Ward 的一切产物（索引、摘要、报告）均可从 `git + 工作区文件` 重建；删掉整个 `.ward/` 目录，系统功能不损失正确性，只损失速度 |
| P2 | **Advisory, not Authority** | Ward 只建议、只度量、只报告；所有写操作经由 git commit / PR，由人或 Agent 显式执行。**对代码内容永远 fail-open**（永不拦截"写了什么"）；对流程条件的约束（"写之前要做什么"）仅以 CI 断言形式存在，且必须有**人书写并审阅的 spec** 授权 |
| P3 | **Fail-open（内环）** | 索引过期、解析失败、服务宕机时，内环所有检查降级为"跳过并记录"，绝不阻塞开发流。外环 CI 断言的失败姿态遵循 P7 |
| P4 | **复用优先** | 检索、索引、MCP 服务层优先复用成熟开源件（tree-sitter、rusqlite、sqlite-vec、notify、jscpd、cargo-semver-checks、japicmp、binary-compatibility-validator、swift-api-digester、LSP/Serena 生态），自研集中在两个重投入差异化模块（M1/M2）+ 若干薄自研层（M3 差分运行器、M4 断言执行器、M2 确定性 diff） |
| P5 | **度量先行** | 每个模块带可测指标与毕业门槛，达不到门槛即下线或降级，不允许"愿景当规格" |
| P6 | **确定性兜底** | 意图验证用测试/lint/类型/契约断言在 CI 中确定性执行；LLM 只做叙述与排序，不做裁决 |
| P7 | **两环分治** | **内环 fail-open，外环 fail-closed**。内环 advisory 永不阻塞开发流；外环 CI 断言失败即红、`unknown` 不绿灯（安全方向是红）。两者同二进制、同索引格式，但失败姿态分治，互不妥协 |

---

## 1. 痛点重排与目标矩阵

基于 2024–2026 行业数据（GitClear 2.11 亿→6.23 亿行变更分析、DORA 2024/2025、Agentic SE 调查，来源见 §12），痛点按真实严重程度重排。与 S-VCS 的关键差异：**新增第 1、2 优先级**（S-VCS 未覆盖），**降级上下文问题**（已有轻量方案）。

| 优先级 | 痛点 | 证据 | Ward 解法 | 闭环定义（怎么算解决） |
| :--- | :--- | :--- | :--- | :--- |
| **P0-a** | Review/验证瓶颈 | 68%+ Agent 生成 PR 长期无人审；AI 高采纳团队 PR review 时间 +91%、PR 体积 +154% | **M2 Replay（语义变更摘要）** + **M3 Catch（验证闭环）** | 审阅者在不看全文 Diff 的情况下，依据符号级变更清单+测试报告做出 merge 决定；A/B 实测（§9 分层匹配设计）审阅时间下降且缺陷逃逸率不升 |
| **P0-b** | 代码重复/膨胀 | GitClear：克隆代码 4 倍增长、≥5 行重复块一年增 8 倍、重构占比 24%→3.8% | **M1 Spot（生成前查重拦截）** | Agent 在写代码前收到"已有相似实现"提示并复用/扩展现有实现；**重复拒绝率（推断通道）与 jscpd 独立口径重复率**在 dogfood 仓库中干预期 vs 基线期可测下降（附人类提交同期对照） |
| **P1-a** | 意图/规格漂移 | 结构约束断言通过率随任务推进下降约 30 个百分点；业界公认 agentic coding 头号瓶颈是意图对齐 | **M4 Form Check（规格漂移守护）** | 任务验收条件以机器可检查的断言形式入库（**含行为级 `behavior_diff` 断言**），CI 确定性校验；Agent 每次提交可追溯到规格条款 |
| **P1-b** | 验证缺失 | 多数团队无 Agent 产出物的自动验证门槛 | **M3 Catch**（与 P0-a 共用模块） | Agent 提交前**外环 CI 必须在沙箱跑通**测试/lint/类型检查，报告随提交归档；内环预检仅作秒级提示 |
| **P2** | 上下文浪费 | Context Rot 研究；但 Aider repo-map、Cursor indexing 已大幅缓解 | **M5 Context Cards（上下文卡片）**（复用现成件，不自研引擎） | 仅在 M1–M4 数据表明上下文仍是瓶颈时启动 |
| **P2** | 存量重复债务 | GitClear churn 3.1%→5.7% | **M6 建议制整合重构** | 系统产出带测试报告的整合建议 PR，人审后合并；永不自动落盘 |

### 1.1 非目标（Non-Goals）——明确不做什么

1. **不做新 VCS**。不引入私有真理源，不做 AST 级存储与 AST 级合并。Git 的文本模型是鲁棒性、可审计性、生态兼容性的工程胜利，不动它。
2. **不做双向同步**。不存在"语义库 ↔ 文件"的 Bi-Sync；只有"文件 → 索引"的单向、最终一致、可重建的数据流。
3. **不做"行为证明"**。行为等价不可判定（Rice 定理）。Ward 只做**差分测试**（仅在有旧版本可比的场景，如重构任务）与**契约断言检查**，且只在 CI/沙箱中作为参考信号。
4. **不做无人值守自动重构**。所有代码变更以 PR 形式由人或 Agent 显式提交。
5. **不自研检索引擎/MCP 框架**。能用 Serena/LSP/tree-sitter 生态解决的，不重写。

---

## 2. 总体架构

```text
+------------------------------------------------------------------+
|                        AI Agent / IDE                            |
|        (Claude Code / Cursor / 自研 Agent / 人类开发者)          |
+------------------------------------------------------------------+
        │  ▲ MCP tools                    ▲ │ git commit / PR
        │  │ (advisory, 内环 fail-open)     │ │ (唯一写入路径)
        │  │ PreToolUse/PostToolUse hook ───┘ │ (写文件→自动触发 spot,
        ▼  │                                  │  harness 能力矩阵 §3-M1)
+------------------------------------------------------------------+
|                     Ward Daemon (本地/CI 同构)                    |
|          Rust 单静态二进制；同版本同行为，无运行时差异            |
|                                                                  |
|  +-----------------+  +------------------+  +-----------------+  |
|  | MCP Server      |  | Indexer          |  | Verifier        |  |
|  | - spot          |  | (tree-sitter     |  | 内环: lint/type |  |
|  | - replay        |  |  增量解析, 单向)  |  |  预检(轻量)     |  |
|  | - catch_run     |  +------------------+  | 外环: CI 沙箱全  |  |
|  | - form_check    |          │            |  量+差分(裁决)   |  |
|  +-----------------+          │            +-----------------+  |
|  +---------------------------v----------------------------+      |
|  |  The Rack (.ward/, gitignored, 可整体删除重建)          |      |
|  |  - SQLite(rusqlite): symbols / blocks / edges /         |      |
|  |    advisories / contracts                              |      |
|  |  - sqlite-vec: embeddings    - CAS cache: body_hash    |      |
|  +--------------------------------------------------------+      |
+------------------------------------------------------------------+
        │ 只读                              ▲ git hook / CI 触发
        ▼                                   │
+---------------------+   git(唯一真理源)   +----------------------+
|  工作区文件 (src/)   +-------------------->+  Git 仓库 + CI 流水线  |
+---------------------+                     +----------------------+
```

### 2.1 架构决策要点

1. **单向数据流**：文件/git → Indexer → The Rack（索引层）→ MCP advisory。反向永远不存在。这从架构上消灭了 S-VCS 的 Bi-Sync 双写一致性问题与 view-update 问题。
2. **可丢弃索引**：`.ward/` 整体 gitignored。索引损坏、过期、版本不匹配时的标准处理是**删除重建**。每个索引条目携带 `commit_sha`，advisory 输出携带 `as_of` 与 **per-file 新鲜度**（见 §5），消费者可自行判断新鲜度。
3. **写入路径唯一**：所有代码变更经 git。Ward 与 Agent、人类开发者、多 Agent 并发之间**不存在锁竞争**——git 本身就是协调层（worktree/branch/PR），这是它被验证了 20 年的能力。
4. **本地与 CI 同构、姿态分治**：同一个 Rust 二进制，本地以 MCP daemon 形态跑（内环，毫秒级 advisory，fail-open），CI 以 CLI 形态跑（外环，确定性校验与报告归档，fail-closed）。两环共享索引格式但各自独立构建；**M3 在两环形态不同**：内环只跑 lint/type 预检（轻量），外环跑全量测试 + 差分（Docker 沙箱，裁决权只在外环）。**Apple 平台的"同构"如实降级**：Swift/OC 集成测试需 macOS runner（§3-M3 runner 矩阵），同构承诺仅覆盖 Linux 可验证集。
5. **技术选型：Rust 全栈**。理由按权重排序：(a) **分发形态（决定性）**——目标用户是移动工程师（Rust/Kotlin/Swift/Java/OC），其环境普遍无 Node 工具链，单静态二进制（`curl \| sh` / brew / 二进制下载）是唯一正确分发形态，安装必须做到一行命令；(b) **免重写**——五语言目标仓可达 10⁵–10⁶ 符号量级，若 TS 起步则 F11 触发的语言下沉几乎必然发生，Rust 起步即消除该风险；(c) 长驻 daemon 的内存与启动开销（对开发者机器友好）；(d) 领域生态风向——tree-sitter CLI、jscpd、竞品 probe 的检索内核均已 Rust 化，官方 MCP Rust SDK（[modelcontextprotocol/rust-sdk](https://github.com/modelcontextprotocol/rust-sdk)）活跃维护、sqlite-vec 有[官方 Rust 集成](https://github.com/asg017/sqlite-vec/blob/563a3e60/site/using/rust.md)。选型清单：tree-sitter + tree-sitter-{rust,kotlin,swift,java,objc} 语法、rusqlite、sqlite-vec、notify（文件监听）、bollard（Docker）、fastembed-rs/onnxruntime-rs（本地 embedding）。
6. **语言支持矩阵（首选五语言）**：**Rust / Kotlin / Swift / Java / Objective-C**，逐语言能力差异见 §3.0。rollout 顺序：Rust 自举（Ward 索引 Ward 自身仓库，解析与测试工具链最简）→ Kotlin/Java（JVM 可在 Linux Docker 全量验证，Android 主力）→ Swift（纯 Swift 包 Linux `swift test` 可验证；Apple 框架集成测试需 macOS runner）→ Objective-C（macOS-only，最后）。TS/JS 不进入首选矩阵。monorepo 以 package/module 边界为符号作用域划分，M3 测试套件按受影响模块递归选择。
7. **性能预算与语言选择（诚实声明）**：tree-sitter 与 SQLite 的热路径是 C，**Rust 不加速解析与存储**；Rust 买的是哈希/simhash 等计算层 CPU 常数（2–5×）、daemon 内存与启动、以及分发。性能预算以 F11 数值为唯一门槛，不因换语言而放宽；"极致高性能"不进入对外叙事——预算是门槛，不是口号。

---

## 3. 模块详设（四道结界 + 两个扩展）

### 3.0 语言支持矩阵

首选五语言的逐语言能力表；**每门语言的接入 = 一行表内的五项工作**，rollout 即按行推进：

| 语言 | tree-sitter 语法 | 指纹归一化要点 | `api_compat` 确定性工具（M4） | M3 验证形态 | embedding 覆盖 | rollout |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| **Rust** | tree-sitter-rust（一等公民） | 宏调用节点归一化（宏展开前处理） | **cargo-semver-checks**（cargo 生态标准，确定性 API 兼容检查） | Linux Docker `cargo test` 全量 ✓ | 中上 | **Phase 0 自举** |
| **Kotlin** | tree-sitter-kotlin（fwcd，成熟） | 属性访问器/扩展函数归一化 | **binary-compatibility-validator**（JetBrains 官方） | Linux Docker Gradle JVM 单测 ✓；instrumented 测试需模拟器 → `unknown` | 中 | Phase 1 |
| **Java** | tree-sitter-java（成熟） | 匿名类/lambda 归一化 | **japicmp / Revapi** | Linux Docker Gradle/Maven ✓ | 中 | Phase 1 |
| **Swift** | tree-sitter-swift（alex-pinkus，成熟） | 属性包装器/结果构建器归一化 | **swift-api-digester**（Apple 工具链自带） | 纯 Swift 包：Linux `swift test` ✓；**Apple 框架集成测试需 macOS runner** | 中下 | Phase 2 |
| **Objective-C** | tree-sitter-objc | 消息发送/协议归一化 | swift-api-digester 可消化 ObjC 头（clang importer） | macOS-only（Linux 不可用）→ macOS runner | 下 | Phase 3 |

**跨语言通用声明**：
- **embedding 弱点**：代码 embedding 模型对移动语言的覆盖度普遍弱于 JS/Python。对策：L2 simhash 层在移动语言上承担主要近重复职责，L3 仅作召回补充（与 §3-M1 一致）；**每语言建独立标注评估集，L3 在该语言评估集不达标即关闭该语言 L3**（F8 精神，fail-open）。
- **M2 影响面下界估计对移动语言更关键**：Kotlin/Java 接口、Swift protocol witness、ObjC runtime 的动态派发密度高于静态语言直觉，edges 漏边更普遍——"至少 N 处"措辞全语言强制。
- **jscpd 覆盖**：223 种格式，五语言全含，独立度量口径无需自研。
- **竞品盲区（v0.6.1 补充）**：Probe（§11.5）语言列表无 Kotlin/Objective-C——Ward 首选矩阵恰好覆盖其盲区。此为事实差异点而非竞争手段（其扩展成本低，勿据此定价）。

### M1 Spot：生成前查重拦截 —— P0，差异化核心 #1

**问题**：Agent 生成代码时不检索已有实现，导致重复逻辑持续涌入（GitClear 证实的行业级痛点）。**竞品判断（软化表述）**：截至 2026-08 检索，未发现以代码查重为核心卖点、在生成循环**前置**介入的独立产品——Cursor codebase-aware 模型与 Aider repo-map 提供上下文但不做查重拦截，Probe 是检索而非查重（§11.5），CodeRabbit 等均为事后 review。此判断按 §11 尽调纪律持续监控，发现反例即修订。

**指纹体系**——单一哈希抓不了近重复，四层分工：

| 层 | 指纹 | 抓什么 | 不抓什么 |
| :--- | :--- | :--- | :--- |
| L0 | `body_hash`（原文 CAS） | 未修改的克隆 | — |
| L1 | `struct_hash`（归一化全树哈希） | **归一化后全等**：克隆+纯改名+字面量替换 | **不承诺近重复**（哈希只等价于精确匹配） |
| L2 | **AST 子树特征 simhash**（**受 DECKARD 特征向量化思路启发的变体**——原论文 ICSE 2007 用子树特征向量 + LSH 欧氏聚类，非 simhash；本方案取"子树特征多重集 → 64-bit simhash"，相似度 ≈ 子树多重集 Jaccard） | **结构近重复**（copy-then-modify：差一个 null 检查/一条语句） | 语义同构（改名换实现） |
| L3 | embedding（sqlite-vec） | 语义同构、命名不同的重复 | 仅作**召回补充**，不单独定阈值；**移动语言覆盖弱，按语言评估集准入**（§3.0） |

**L2 归一化设计要点**（每语言一份，Phase 0 先定稿 Rust 版并写死进测试用例）：标识符→占位符归一化、字面量归一化、节点类型集合、n-gram 窗口大小、语言特构节点处理（§3.0 归一化要点列）——全部作为版本化配置，变更需随测试基线一起走。这是多语言接入的主要隐性工作量，已在 §8 人力估算中计入。

**检索流程**：L0/L1 等值索引 + BM25+dense RRF 召回候选集 → **在应用层对候选集计算 L2 simhash 距离**（Hamming/XOR 近距）→ 合并排序输出。L2 距离计算放在应用层而非引入 LSH 专用组件，保持 P4（单仓符号量级候选集 ≤10³，开销可忽略；超量级再评估）。

**块级重复（粒度错配修复）**：函数级指纹对**函数内部的块级重复**不可见（40 行重复块藏进 200 行函数即漏检），而 GitClear 指标是块级口径。两条腿走路：
- **度量腿（Phase 0 起）**：§9 的"增量重复率"指标一律用独立工具 **jscpd** 度量（第三方口径，不自我裁判；token 级 CPD 与 GitClear 行块级口径**趋势可比、绝对值不可比**，见 §9）；
- **机制腿（Phase 1 起）**：新增块级指纹表（滑窗语句块 → simhash），spot 结果增加 `kind: block`，拦截能力逐步向块级指标收敛。Phase 0 只做符号级，不宣称块级能力。

**强制抓手**——"生成前"不能只靠约定：
1. **PostToolUse hook**：Agent 每次写文件后自动触发 spot，对新增符号做查重，advisory 以 tool result 评论形式注入（最接近"拦截"的合法表面）；
2. **PreToolUse hook**：大改动（>N 行新增）前提示先查 spot（实际形态按 harness 能力矩阵）；
3. rules 文件/系统提示约定保留为辅助兜底；
4. **spot 调用率** = 触发 spot 的文件写入事件占比，作为 M1 **第一指标**（调用率上不去，采纳率无从谈起），**按 harness 分层统计**（§9）。

**harness 能力矩阵（独立核验 2026-08）**——hook 强制抓手对 agent harness 有依赖，按实情声明分级支持与降级路径：

| Harness | hook 现状（2026-08 核验） | Ward 适配形态 | 承诺 |
| :--- | :--- | :--- | :--- |
| **Claude Code** | PreToolUse/PostToolUse 完整；PreToolUse 可 deny（exit 2 阻止写文件）；PostToolUse 可注入 additionalContext；**PreToolUse 不支持注入 context**（[#15664](https://github.com/anthropics/claude-code/issues/15664)、[#19432](https://github.com/anthropics/claude-code/issues/19432)） | 写前提示用 **deny-with-reason**：拒绝写入并在 reason 里要求先跑 spot（比温和提示更硬）；写后注入用 PostToolUse additionalContext | **Phase 0 唯一承诺** |
| **Cursor** | preToolUse/postToolUse 已上线，但 **postToolUse 的 additional_context 存在未修复缺陷、不注入模型上下文**（[社区论坛多帖](https://forum.cursor.com/t/posttooluse-hooks-additional-context-not-injected-into-agent-model-context/158168)，2026-08） | 注入式设计当前失效 → 降级为 rules 约定（软）；调用率分层报告、暂不设达标线 | 跟踪上游修复 |
| **自研/其他** | 无标准 hook 机制 | 自实现写文件 middleware 拦截层 | Phase 2+ |

hook 不可用时不视为 Ward 故障（fail-open，F9 精神）；调用率指标分层统计，不跨 harness 混算。

**MCP 接口**：

```jsonc
// spot
{ "intent": "防抖函数，支持 leading/trailing 选项",
  "proposed_signature": "fn debounce(f: impl Fn(), ms: u64, opts: ...) -> impl Fn()",
  "top_k": 5 }
// 返回
{ "as_of": "a1b2c3d", "stale": false,   // stale 判定含 per-file 新鲜度（§5）
  "matches": [
    { "path": "src/utils/timing.rs", "lines": "14-41", "symbol": "debounce",
      "similarity": 0.94, "kind": "structural",   // structural(simhash) / semantic / block / exact
      "note": "已支持 leading/trailing，测试覆盖 93%" } ],
  "advisory_id": "adv_01J..." }
```

**分级与校准**：阈值 `≥0.92 强提示 / 0.80–0.92 弱提示 / <0.80 不返回`为**初始值**（声明为拍脑袋起点），按**人工黄金集**（与 M2 抽检共用管线）周级校准。Advisory 全部 fail-open：服务不可用即跳过。

**采纳/拒绝双通道（v0.5.0 修正因果方向）**：Agent 自报（`agent_action` 回写）**不可单独采信**——"谎称跑了测试"正是 Agent 头号劣迹。结果推断通道必须问对因果：spot 命中 top-1 意味着"已有实现 X"，**期望的采纳行为 = 复用/扩展 X**，其可观测证据是**没有**引入与 X 相似的符号、且新增对 X 的调用/引用边；而"引入相似新符号"恰是**拒绝**建议、重复造轮子的证据。语义定义：

```
inferred_action =
  accepted   := 下一 commit 未引入相似新符号 ∧ 新增到 top-1 符号的调用/引用边
  reused-ish := 未引入相似新符号 ∧ 无调用边（采纳或需求转向，无法区分，单列）
  rejected   := 引入了与 top-1 高度相似的新符号
  unknown    := 任务中途放弃 / 无后续 commit
```

§9 主指标由"采纳率"改为**重复拒绝率**（rejected 占比），比采纳率更直接对齐 GitClear 痛点；自报通道仅作辅助分布，两通道背离 >10pp 报警（防自报失真）。

**误报与 alarm fatigue**：每仓库 `.ward/config.toml` 可抑制路径/模式；**误报率与驳回率分离**——误报率以黄金集标注为据（"Agent 驳回"≠"建议错误"）。

**冷启动**：首次索引全仓在 CI 预热并归档产物，本地 clone 后下载展开；**索引包必须签名**（minisign/age），验签失败即删除重建、不阻塞（§7）。

### M2 Replay：语义变更摘要 —— P0，差异化核心 #2

**问题**：审阅者的瓶颈不是读 Diff 慢，而是建立"这次改了什么、影响什么、风险在哪"的心智模型慢。LLM 直接审 Diff 已存在（CodeRabbit 等），但其摘要缺乏确定性锚点，会幻觉。

**机制（确定性为骨，LLM 为肉）**：
1. `git diff base..head` 输入后，**确定性层**先产出符号级变更清单：tree-sitter 解析新旧两版，对齐符号，分类为 `added / removed / signature_changed / body_changed / moved / doc_only`；再沿调用图做 1-hop 影响面分析（哪些现存调用方受签名变更影响）。**影响面为下界估计**：edges 由静态解析构建，接口/回调/事件等动态派发会漏边——移动语言动态派发密度更高（§3.0），"被 N+ 处调用"的报告措辞一律为"至少 N 处"，不冒充精确值。
2. 从变更清单**确定性推导风险标记**：公共 API 签名变更、被 N+ 处调用的符号被改、测试文件未同步变更、新增未引用的导出符号（疑似死代码）、与 M1 索引中既有符号高度相似（疑似重复引入）。风险标记体系可定制（Phase 2 引入 guides.toml，借鉴 Vet issue codes 思路）。
3. **LLM 层只做叙述**（两条实现路径）：
   - **默认路径：结构化槽位生成**——LLM 只在固定模板槽位内填空（变更意图解读/风险说明/审阅清单），每个槽绑定确定性清单条目 id，**锚点在生成结构上保证存在**；
   - **可选路径：自由叙述 + 逐句锚点校验器**——第二遍规则校验每句是否可回溯到清单条目，未命中即删句，不返工重写。
   - 铁律不变：摘要中每一条事实性陈述必须可点击回溯到符号+行号；LLM 不得引入清单之外的事实主张。
4. 输出形态：PR 评论（外环 CI 产出）+ 本地 `replay` MCP tool（内环，Agent 自描述其产出，供人快速过目）。

**为什么不是"行为证明替代 Diff"**：Diff 与测试仍是裁决依据；本模块攻击的真实瓶颈是"建立心智模型"的时间，而非"阅读"的时间。不承诺"30min→2min"这类数字——目标值由 A/B 实测定义（§9）。

**指标**：审阅时长 A/B（分层匹配 + 交叉设计，§9）、摘要事实错误率（人工抽检，连续两周 >5% 触发 F6 回退）、缺陷逃逸率（非劣效边际，§9）。

### M3 Catch：验证闭环 —— P1

**问题**：Agent 产出物缺少确定性验证门槛；"让它看起来对"与"它对"之间缺一层机器检查。

**机制（内外环拆分 + runner 矩阵）**：
1. **内环 `catch_run` = 预检**：Agent 完成实现后调用，本地执行 lint/type 检查（轻量，避免全量构建）：Rust `cargo check`、Kotlin/Java Gradle 编译检查、Swift `swiftc -typecheck`。报告 verdict：`pass / fail / deferred / unknown`。`must_pass` 全量测试与 `behavior_diff` 标记为 `deferred`，并附提示"裁决以 CI 外环为准"。**本地沙箱不可用（F13）→ verdict `unknown` + 显式提示"仅 CI 可裁决"，绝不静默 pass**。
2. **外环 CI = 裁决**：在沙箱中执行项目既有的测试/lint/类型检查（**复用项目自身工具链**，Ward 不发明新检查），fail-closed（P7）：失败即红、`unknown` 不绿灯。
3. **runner 矩阵**：

   | 验证目标 | runner | 状态 |
   | :--- | :--- | :--- |
   | Rust `cargo test` | Linux Docker | ✓ 默认 |
   | Kotlin/Java JVM 单测（Gradle/Maven） | Linux Docker | ✓ 默认 |
   | Swift 纯包 `swift test` | Linux Docker | ✓ 默认 |
   | **Apple 框架集成测试（UIKit/AppKit/XCTest）** | **macOS runner** | CI 配置扩展点，Phase 2 评估；本地内环预检在 macOS 原生执行 |
   | **Android instrumented 测试** | 模拟器/真机农场 | 标记 `unknown`，v1.0 前不承诺 |

4. **差分测试（仅重构类任务）**：同一测试集对 old/new 两版运行，比较结果与（抽样式）快照输出。old 版本 = 任务开始时的 commit 归档（含 lockfile/依赖版本固定）。快照输出**归一化**：剔除时间戳/随机种子/绝对路径后才比对；声明为抽样对比而非全量等价。这取代 S-VCS 的"WASM 行为证明"——承认它只是差分测试，只在其有效的场景使用。
5. **受控服务依赖**：真实项目的测试常依赖数据库/中间件，一刀切禁 Docker socket 会让这些测试全部 `unknown` → 全红。机制：外环 CI 以 **sidecar 容器（compose）** 启动项目依赖服务，沙箱经内部网络访问；**仍禁 Docker socket 挂载与主机写挂载**；依赖服务缺失/不可达 → 相关测试 `unknown`（红，P7），绝不静默 pass。服务依赖清单走 F7 同款人审通道配置。
6. 对纯函数密集的新代码，可选生成 property-based 测试草案（proptest/quickcheck/kotest），以 PR 评论形式建议，人审后入库。
7. 验证报告（通过/失败/flaky/覆盖率增量）随提交归档，M2 摘要中引用。

**边界**：沙箱内测试不可信时的裁决（环境依赖、外部服务）→ 标记为 `unknown` 而非 `pass`，绝不用绿灯掩盖未验证。Flaky 测试自动重试并隔离上报（隔离清单 + SLA + 人审豁免，见 F7）。

### M4 Form Check：规格漂移守护 —— P1，补 S-VCS 最大盲区

**问题**：长任务中 Agent 逐渐偏离意图（约束衰减实测约 30 个百分点）——就像健身做组做到后面动作变形。这是当前 agentic coding 的头号痛点，S-VCS 完全未覆盖。

**机制（轻量版 spec-driven）**：
1. 任务开始时，在仓库内落一份 `specs/<task-id>.md`（入库、可 review），含**机器可检查断言段**，覆盖行为层：

   ```yaml
   # specs/2026-0813-debounce.md 片段
   assertions:
     - kind: no_new_dependency            # 不新增第三方依赖
     - kind: api_compat                   # 公开 API 类型/ABI 级兼容（逐语言工具，见下）
     - kind: must_pass, suite: "tests/utils/**"   # 指定测试必须通过
     - kind: behavior_diff, suite: "tests/golden/**"  # 行为级：golden/差分测试必须通过
     - kind: max_files_changed, value: 6
   ```

2. **`api_compat` 的逐语言确定性工具矩阵（v0.6.0 泛化）**：放弃任何单一自研实现（如 TS 版的 tsc 导出表面 diff），改用每语言生态内**已存在的确定性工具**，Ward 只做调用编排与四档判定：

   | 语言 | 工具 | 判定口径 |
   | :--- | :--- | :--- |
   | Rust | cargo-semver-checks | 公开 API 兼容（cargo 生态标准） |
   | Kotlin | binary-compatibility-validator（JetBrains 官方） | 二进制兼容 |
   | Java | japicmp / Revapi | 字节码级 API/ABI 兼容 |
   | Swift | swift-api-digester（工具链自带） | 模块接口兼容 |
   | Objective-C | swift-api-digester（clang importer 消化 ObjC 头） | 头文件接口兼容 |

   通用四档判定：`pass`（兼容）→ `fail`（不兼容）→ `unknown`（工具盲区，如跨语言 ABI 细节）→ `deferred`（仅外环可判）。内环 `unknown/deferred` → 降级提示；**外环 `unknown` → 红**（P7，安全方向是红）。
3. CI（外环）对每次 PR **确定性执行断言**：依赖清单 diff、逐语言 api_compat 工具、指定测试套件运行（含 golden）、变更面约束。全部失败即红，无 LLM 裁决。
4. Agent 每次 commit message 引用规格条款号（`[spec:a2]`），形成意图→提交的可追溯链；M2 摘要中展示"规格覆盖度"（几条断言已验证/未验证）。
5. **断言由人编写**——spec 草案可由 Agent 起草，但 merge 前必须人审（"考生自判"补丁）。**spec 修订走 PR、人审留痕**（F12）：任务中途需求合法变更时，正确的解法是修订 spec 的 PR，而非代码绕行；CI 红允许以 spec 修订 PR 解决，但修订与代码同评审。spec 人审时长与驳回率纳入 §9 监测。
6. **M4-b 意图比对层（默认组成，带指标与降级触发）**：对未编写显式 spec 的任务（或作为断言层兜底），用 LLM 比对"用户原始需求描述 ↔ 最终 diff"做**软性意图偏离提示**——只提示、不拦截；明确标注为 LLM 判断，与确定性断言在报告里**分区呈现**，永远不能把 `unknown` 断言"说成"pass。**默认开启即必须可度量**：§9 定义人工确认率（≥60%，周抽样 10 条）与成本帽（纳入 M2 预算统一计量）；连续两周确认率 <40% → 自动降级为可选，直至数据支持重新开启。

**内环接口（form_check schema）**：

```jsonc
// form_check（内环预检，提交前，轻量、非裁决）
{ "spec_path": "specs/2026-0813-debounce.md", "base": "main" }
// 返回
{ "as_of": "...", "stale": false,
  "results": [
    { "assertion": "no_new_dependency", "verdict": "pass", "detail": "依赖清单无新增" },
    { "assertion": "api_compat", "verdict": "unknown", "detail": "二进制级判定需 CI 外环" },
    { "assertion": "must_pass", "verdict": "deferred", "detail": "全量测试仅 CI 裁决" }
  ],
  "note": "本预检非裁决；CI 外环结果为准" }
```

**与业界关系**：与 Kiro/Spec Kit 的 spec-driven 方向一致，但只取"机器可检查断言 + CI 确定性执行"这一最小内核，不引入整套方法论迁移成本。

### M5 Context Cards：上下文卡片 —— P2，复用优先

**不做**：自研 2-hop 依赖子图引擎、自研向量检索栈、**自研检索后端**。
**做**：在 M1 索引之上暴露一个薄封装 `context_card(symbol)`，输出"定义 + 调用方 Top-N + 相关测试 + 配置引用"的一页卡片。
**检索后端集成策略（v0.6.1 落位）**：若启动，检索后端**进程级集成 probelabs/probe CLI**（§11.5）而非自研，Ward 只做卡片组装与治理数据（采纳/风险/测试报告）注入——这是 P4 复用优先的最优解；**Probe 无 Kotlin/OC**，这两门语言的 M5 需自行桥接或降级为符号索引输出（计入 M5 启动评估成本）。
**启动前提**：M1–M4 上线后，任务失败复盘数据显示"上下文缺失"仍是 Top-3 失败原因，否则不建；启动评估须将 probe 纳入对照基线（其已商品化此方向）。

### M6 建议制整合重构（Consolidation PR Bot）—— P2

**问题**：存量重复债务需要清理，但无人值守自动改写不可接受。
**机制**：M1 索引离线聚类出重复簇（符号级 + 块级）→ LLM 生成"提取公共实现 + 各调用点迁移"的整合方案 → **以普通 PR 形式提交**，PR 内含 M3 差分测试报告与 M2 语义摘要 → 人审人并。频率限制（每周 ≤N 个）避免 PR 轰炸。与 S-VCS"后台 GC 引擎"的本质区别：**每一步有人，每一个字可回滚，产出物是 PR 而不是落盘事实**。

---

## 4. 数据模型（The Rack）

SQLite，单文件 `.ward/index.db`，schema 版本化（`schema_version` 表），版本不匹配即整库重建：

```sql
-- 符号表：tree-sitter 解析产物
CREATE TABLE symbols (
  id INTEGER PRIMARY KEY,
  file_path TEXT NOT NULL,
  language TEXT NOT NULL,                      -- rust/kotlin/swift/java/objc（多语言仓库必需）
  name TEXT NOT NULL, kind TEXT NOT NULL,      -- function/class/method/...（kind 按语言映射）
  start_byte INTEGER, end_byte INTEGER,
  body_hash TEXT NOT NULL,                     -- L0 原文内容哈希（CAS，精确克隆）
  struct_hash TEXT NOT NULL,                   -- L1 归一化全树哈希（归一化后全等）
  simhash TEXT NOT NULL,                       -- L2 子树特征 simhash（结构近重复，DECKARD 变体）
  commit_sha TEXT NOT NULL                     -- 索引依据的提交
);
CREATE INDEX idx_struct ON symbols(struct_hash);
CREATE INDEX idx_lang ON symbols(language);

-- 块级指纹表（Phase 1 起）：滑窗语句块，抓函数内部重复
CREATE TABLE blocks (
  id INTEGER PRIMARY KEY,
  file_path TEXT NOT NULL,
  parent_symbol_id INTEGER,
  start_byte INTEGER, end_byte INTEGER,
  simhash TEXT NOT NULL,
  kind TEXT,                                   -- statement_block / expr_block
  commit_sha TEXT NOT NULL
);

-- 依赖边（调用/引用），用于 M2 影响面分析（下界估计）
CREATE TABLE edges (
  src_id INTEGER, dst_id INTEGER, kind TEXT,
  PRIMARY KEY (src_id, dst_id, kind)
);

-- embedding 由 sqlite-vec 虚表承载，与 symbols.id 关联（按语言准入，§3.0）

-- advisory 反馈闭环：M1 采纳/拒绝度量的数据基础
CREATE TABLE advisories (
  id TEXT PRIMARY KEY, tool TEXT NOT NULL, ts INTEGER NOT NULL,
  query_hash TEXT, result_json TEXT,
  agent_action TEXT,                           -- 自报通道：accepted/ignored/dismissed/unknown
  inferred_action TEXT,                        -- 推断通道：accepted/reused-ish/rejected/unknown
  inferred_commit_sha TEXT
);

-- 规格断言执行记录
CREATE TABLE contract_runs (
  spec_path TEXT, commit_sha TEXT, ts INTEGER,
  assertion TEXT, verdict TEXT,                -- pass/fail/unknown/deferred
  detail TEXT
);
```

**取舍说明**：不用图数据库（Redb/Neo4j）。调用图查询深度 ≤2，SQLite 递归 CTE 足够；少一个组件就少一类运维与腐烂。不用 HNSW 专用库，sqlite-vec 的暴力/IVF 在单仓符号量级（10⁴–10⁵）毫秒级可达。**性能口径（与 F11 统一）**：符号量 ≤5×10⁵ 时全量索引 <10min、查询 P99 <100ms **必须达标**；超 5×10⁵ 只进入监控（F11），达 10⁶ 触发 scale-out 评估，届时 schema 与接口不变。

---

## 5. 一致性与同步协议（单向）

1. **触发**：文件 watcher（notify，编辑器高频区，去抖 500ms）+ **写文件 hook 事件**（PostToolUse，作为高频信号）+ git ref 变化检测（post-checkout/merge hook 轮询 HEAD）+ CI 全量。
2. **增量**：按文件 `mtime+size+hash` 三级判断，只重解析变更文件；符号表按 `file_path` 整行替换。
3. **新鲜度协议（per-file）**：每次索引写入记录当时 HEAD 与**每个文件的 indexed_hash**；所有 advisory 输出带 `as_of` 与
   `stale = (index_sha != HEAD) ∨ (任一命中文件 indexed_hash ≠ 当前文件 hash)`。
   这覆盖最常见的过期场景：Agent 改到一半、未提交时调用 advisory（watcher 去抖窗口内或 watcher 失败）。`stale=true` 时强提示降级为弱提示并标注，绝不假装新鲜。
4. **冲突**：不存在。索引是派生物，任何不一致以文件/git 为准，重建解决。
5. **合并冲突中的仓库**：冲突标记文件解析失败 → 该文件符号标记 `unparsable`，相关 advisory 跳过该文件（fail-open），其余文件正常工作。git 的三方合并仍在文本层进行——这是特性不是缺陷：文本合并久经考验，Ward 不发明 AST 合并。

---

## 6. 失败模式目录（S-VCS 原文档为零，本节为强制要求）

| # | 失败模式 | 检测 | 行为 |
| :--- | :--- | :--- | :--- |
| F1 | 索引损坏/版本不匹配 | 打开时校验 schema_version + 完整性 pragma | 自动删除重建；重建期间 advisory 降级跳过 |
| F2 | 索引过期（HEAD 已移动或工作区有未落盘改动） | as_of + **per-file 新鲜度协议**（§5） | 降级提示强度并标注 stale |
| F3 | tree-sitter 解析失败（语法错误/冲突标记/语法版本/语法未支持的新语法） | 解析错误节点 | 该文件标记 unparsable，跳过，不影响全局；**移动语言语法演进快（Swift/Kotlin 尤甚），语法包版本随 Ward 发布节奏季度跟进** |
| F4 | 查重误报（语义同构误判） | 黄金集人工标注（误报率与驳回率分离，**按语言分层**） | 阈值周级校准；config 支持路径/模式抑制 |
| F5 | 查重漏报（跨语言/跨抽象层/函数内块级重复） | jscpd 独立度量 + 块级指纹覆盖率监控 | 承认边界：本模块只承诺降低增量，不承诺清零；块级能力 Phase 1 起逐步收敛 |
| F6 | M2 摘要幻觉 | 逐句锚点校验器（未命中清单条目即删句）+ 人工抽检 | **连续两周抽检错误率 >5% → 回退纯结构化清单（无 LLM 叙述）**；抽检不达标期间结构化槽位路径仍可用 |
| F7 | 沙箱测试 flaky/环境依赖 | 重试方差检测 | 裁决输出 `unknown`，绝不绿灯；**flaky 隔离清单 + 修复 SLA（5 工作日）+ 人审临时豁免（记录审计）**——隔离清单内的 flaky 失败标记 `flaky-exempt`（定期人审复核），其余 `unknown` 一律红（P7）；flaky 清单上报 |
| F8 | embedding 服务不可用 | 健康检查 | 退化为 L0/L1 等值 + L2 simhash + BM25（去掉 L3 语义层），不阻塞；**某语言 L3 评估集不达标 → 该语言关闭 L3**（§3.0） |
| F9 | MCP daemon 宕机 | 客户端超时 | 内环全部 fail-open，Agent 工作流不受任何影响（P7） |
| F10 | 多 Agent/人并发写代码 | 不适用 | git 原生协调（branch/worktree/PR），Ward 只读，无锁 |
| F11 | 大仓性能劣化 | 索引耗时与查询 P99 监控 | 口径：≤5×10⁵ 符号必须达标（索引 <10min、P99 <100ms）；5×10⁵–10⁶ 仅监控不做预研；达 10⁶ 触发 **scale-out 评估**（远程索引/分片/分布式构建缓存——架构扩展而非语言重写，Rust 起步已消除语言下沉问题） |
| F12 | spec 自身漂移/过时 | spec 修订走 PR + 人审留痕；CI 红时先查 spec 修订历史 | 任务中途合法变更需求 → 修订 spec 的 PR 是合法解法；修订与代码同评审；spec 人审时长/驳回率纳入 §9 监测。杜绝"红了就改断言"的无审路径 |
| F13 | 本地以 CLI 跑外环流程（`ward verify --full`）但沙箱环境缺失 | 环境探测 | verdict=`unknown` + 显式提示"仅 CI 可裁决"；绝不静默 pass。内环预检本就不需要 Docker，此条仅覆盖本地模拟外环的场景 |

---

## 7. 安全与隐私

- 索引与 advisory 日志全部本地；embedding 默认本地小模型（fastembed-rs/onnxruntime-rs），配置允许时才走云端且仅发送符号签名片段（不含整文件）。
- **沙箱（M3）细则**：默认断网、**永不挂载 Docker socket**、无主机写挂载（仓库只读 + 独立 tmpfs 工作区）、cap-drop ALL + 最小 cap 白名单 + 默认 seccomp profile、沙箱镜像按 digest 固定并做供应链校验。M3 本质是把不可信代码（Agent 生成物）放进执行环境，上述项是准入前提而非可选加固。**项目依赖服务走受控 sidecar（compose），沙箱仅经内部网络访问**（§3-M3-5）。验证报告不含源码全文，只含测试结果与 diff 统计。
- **macOS runner 的隔离声明**：Apple 平台集成测试无法用 Linux 容器隔离，macOS runner 采用进程级 sandbox-exec + 网络受控 + 一次性 runner 实例（不落盘凭据），安全级别如实标注为"弱于 Linux 容器、强于本地裸跑"。
- **specs/ 解析安全**：CI 解析 specs YAML 用严格解析器，禁锚点/别名、限嵌套深度与文件大小（防 alias bomb），超限即 fail-closed。
- **索引产物分发**：CI 预热归档的索引包必须签名（minisign/age），本地解包前验签；验签失败即删除重建、不阻塞。索引是派生物，其破坏面限于 advisory 错误（fail-open 兜底），但签名仍为必备卫生。
- `specs/` 与 CI 断言是入库文本，走正常代码评审——意图层的变更因此天然留有审计痕迹。

---

## 8. 落地路线图（2 名工程师起步）

| 阶段 | 周期 | 内容 | 退出条件（毕业门槛，全部数值化、标注为校准前临时门槛） |
| :--- | :--- | :--- | :--- |
| **Phase 0 原型** | 2 周 | M1 Spot 单体原型（**Rust**）：tree-sitter-rust + L0/L1 + BM25 检索 + MCP server（官方 rust-sdk）+ PostToolUse/PreToolUse hook（**限定 Claude Code**），不做 L2 simhash/embedding/块级。**dogfood 仓库 = Ward 自身仓库（Rust 自举）**。**第 1 周即在候选 dogfood 仓库启动 jscpd 周度基线采集**（与开发并行，干预前基线必须前置） | 自仓跑通"写文件 → hook 触发 spot → 反馈回写"端到端闭环；黄金集起步：人工标注 ≥50 条 advisory；jscpd 基线采集正常运行；**单二进制安装一行命令可用** |
| **Phase 1 dogfood** | 4 周 | M1 完整（L2 simhash + L3 embedding + 双通道采纳/拒绝回写 + 块级指纹起步）+ **Kotlin/Java 语法接入（§3.0 行内五项工作）** + M2 确定性层（符号 diff+风险标记，无 LLM）| 5 个真实仓库 dogfood（**含 ≥2 个 JVM 仓库**，Ward 自仓持续在内）；标注集 ≥200；强提示 precision ≥60%（分语言报告）、误报率 ≤20%、spot 调用率 ≥50%（Claude Code 口径，按 harness 分层）、重复拒绝率干预期 vs 基线期下降；jscpd 基线数据 ≥4 周就绪 |
| **Phase 2 验证与意图** | 6 周 | M3 内外环（Linux Docker 默认 + macOS runner 评估）+ M4 断言与 CI 集成（逐语言 api_compat 工具接入）+ **Swift 接入** + M2 LLM 叙述层（结构化槽位路径）+ M4-b（带指标） | 见 §9 指标门槛 |
| **Phase 3 按需扩展** | 视数据 | **Objective-C 接入**；M5/M6 仅在 Phase 2 复盘数据支持时启动（M5 评估含 probe 对照与集成成本） | — |

**前提假设声明**：全稿按**团队具备 Rust 生产能力**编写。若实际不具备，Phase 0 周期 ×1.5、Phase 1 周期 ×1.3 是保守修正——这是当前方案的最大单一风险，先于一切技术风险。

**人力与复用清单**：tree-sitter（解析，含五语言语法）、rusqlite/sqlite-vec（存储）、官方 MCP Rust SDK（协议）、bollard（沙箱）、jscpd（重复度量）、notify（监听）、fastembed-rs（本地 embedding）、逐语言 api_compat 工具（M4，四工具均为现成）、probe CLI（M5 检索后端，进程级集成）、Serena/LSP（M5 桥接备选）。自研代码预估：Phase 0–1 约 **8–10k 行**（Rust 起步 + Rust/Kotlin/Java 三语言归一化 + L2 simhash、块级指纹、双通道推断、hook 适配——多语言归一化是主要增量）。

**标注人力（单列）**：黄金集标注 + M2 周抽检 + M4-b 确认率抽样合计约 **0.2–0.5 人周/月**，属长期投入而非一次性成本。由两名工程师之一明确兼任并计入容量；标注一致率做交叉抽检，防标注者疲劳导致阈值校准失真（标注腐烂 → 全指标失真，是单点风险）。

### 8.5 回滚计划（一行卖点）

整产品回滚 = 停用 daemon + 移除 CI step + 删除 `.ward/`。零残留、零锁死（P1/P3/P7 保证）。唯一残留物是已入库的 `specs/` 与已合并的 PR 评论——两者均为**人审产物**，保留无副作用。

---

## 9. 评估基准（全部先定义，后建设）

**归因纪律**：所有趋势指标采用**基线期（4 周）vs 干预期**设计，并附**同期人类提交对照序列**——Agent 模型与团队行为在快速变化，无对照的"环比下降"无法归因于 Ward。**基线必须在干预前采集**（Phase 0 第 1 周启动，§8）。判定规则：连续两个周期不达标 → 降级或下线（P5）。

**指标语义走查纪律**：每条指标上桌前必须回答两个问题——(1) **它测量的因果方向对吗？**（v0.4.0 推断采纳通道方向反向即为反例，已修正）；(2) **被刷该指标会产生什么扭曲激励？** 走查结论随指标一起评审，与人类对照组纪律一脉相承。

**语言分层纪律**：M1 的 precision/误报率/重复拒绝率按语言分层报告——embedding 弱语言（Swift/OC）与强语言（Rust/JVM）的 L3 表现差异必须可见，不得用跨语言平均掩盖弱语言劣化；弱语言 L3 不达标即关闭（F8）。

| 模块 | 指标 | 目标（均标注为校准前临时门槛，Phase 1 第 2 周按黄金集数据修订） | 测量方式 |
| :--- | :--- | :--- | :--- |
| M1 | **spot 调用率**（触发 spot 的写文件事件占比） | Phase 1 ≥50%；Phase 2 ≥70%（Claude Code 口径）；**按 harness 分层统计**，Cursor 分层报告、暂不设达标线（待上游修复） | hook 事件计数 vs PostToolUse 写文件事件计数，分 harness 记账 |
| M1 | **重复拒绝率（主指标）** | 干预期 vs 基线期下降 ≥10pp；**分语言报告** | 推断通道 `rejected` 占比；基线期在 Phase 0 采集 |
| M1 | 自报采纳率（辅助） | ≥30%；与推断通道背离 >10pp 报警 | advisories.agent_action 分布 |
| M1 | 强提示 precision / 误报率（黄金集） | precision ≥60%；误报率 ≤20%；**分语言报告，弱语言单独设门槛** | 人工标注（与 M2 抽检共用管线，周级）；"Agent 驳回"单独报告为驳回率，不计入误报率 |
| M1 | 增量重复率 | 干预期 vs 基线期环比下降；人类提交对照不劣化 | **jscpd 独立口径**（token 级 CPD；与 GitClear 行块级口径**趋势可比、绝对值不可比**，禁止对标绝对数），月度 |
| M2 | 审阅时长 | A/B 下降 ≥25% | 分层匹配（diff 行数 ±25% 桶内随机分组）+ 交叉设计（同一 reviewer 交替两种形态）+ 窄口径计时（首次审阅动作 → approve，剔除等待时间） |
| M2 | 摘要事实错误率 | ≤5%（连续两周，每周抽检 20 条） | 抽检记录；超阈值触发 F6 回退 |
| M2 | 缺陷逃逸率 | ≤ 基线 ×1.2（非劣效边际） | 上线后 7 日内 revert/hotfix 归因，固定周度抽样 |
| M2 | LLM 成本 | 每 PR 中位数 <$0.05；周预算帽，超帽自动降级结构化槽位路径 | token 计量（**M4-b 的 LLM 成本纳入同一预算帽统一计量**） |
| M3 | Agent 提交首过 CI 率 | 提升 ≥20 个百分点（绝对值）；人类提交同期对照 | CI 历史对比（**按 runner 分层：Linux Docker 集与 macOS runner 集分开报告**） |
| M4 | 规格断言覆盖率（有断言的 Agent 任务占比） | dogfood 团队 ≥60% | CI 统计 |
| M4 | 约束衰减 | 长任务（>10 commits）断言通过率降幅 <10pp | contract_runs 纵向分析 |
| M4 | spec 审阅卫生 | 监测 spec 人审时长与驳回率 | Phase 2 定标，先监测 |
| M4-b | 意图偏离提示人工确认率 | ≥60%（每周抽样 10 条）；**连续两周 <40% → 自动降级为可选** | 人工抽检（与 M2 抽检共用管线） |
| M5/M6 | 启动条件 | 复盘数据驱动（§3） | — |

**门槛纪律**：任一模块连续两个周期达不到门槛 → 降级（如 M2 去掉 LLM 层）或下线。指标写进设计文档而非事后补，这是对 S-VCS"愿景当规格"的直接纠正。

---

## 10. 与 S-VCS 主张的映射（保留了什么、砍了什么、为什么）

| S-VCS 主张 | 本方案处置 | 理由 |
| :--- | :--- | :--- |
| AST 真理源 / Git 为物化视图 | **砍**。反转为 git 唯一真理源 + 可重建索引 | 双写一致性与 view-update 无通用解；历史先例（Unison/MPS/SemanticMerge）全不利 |
| Bi-Sync + Tx Locks | **砍**。单向数据流 + as_of/per-file 新鲜度协议 | fs notify 不可靠、锁不提供原子性；git 本身就是并发协调层 |
| 2-Hop 子图上下文引擎 | **降级为 M5（P2，复用现成件，数据驱动启动）** | Aider/Serena/Cursor 已商品化；自研无差异化 |
| WASM 行为证明替代 Review | **砍**。改为 M2 Replay（确定性符号 diff + 锚定式 LLM 叙述）+ M3 差分测试（仅重构场景） | 行为等价不可判定；"证明"答非所问（证明不了意图）；LLM 必须锚定确定性事实 |
| 后台 WASM GC 重构引擎 | **改造为 M6 建议制 PR（P2）** | 无人值守改写生产代码不可接受；PR 形态有人、可回滚、可审计 |
| HNSW + AST Hash 生成阶段查重 | **保留为 M1 Spot（P0 核心），技术栈修正** | 业界真空白；但单一 AST 哈希抓不了近重复，改为四层指纹（L0/L1/L2/L3 + 块级）；sqlite-vec 在单仓量级足够，不引专用向量库 |
| MCP <10ms 内环 | **保留但不作为卖点** | 红海能力，SQLite 点查自然达标；差异化在查重与摘要的数据闭环 |
| （缺失）意图/规格漂移 | **新增 M4 Form Check（P1）** | 行业公认头号痛点，S-VCS 盲区；断言覆盖行为层（behavior_diff）+ 逐语言 api_compat 确定性工具 |
| （缺失）验证闭环 | **新增 M3 Catch（P1）** | 确定性验证是意图对齐的唯一机器锚点；内环预检/外环裁决分治 + runner 矩阵 |
| （缺失）失败模式/基准/MVP | **新增 §6/§8/§9** | 规格书基本修养 |

---

## 11. 竞品对比

### 11.1 Ward vs Vet（imbue-ai/vet）

> 调研对象：imbue-ai/vet（2026-01 发布，AGPL-3.0，HN 首页讨论 448+ points）。Vet 定位"standalone verification tool for code changes and coding agent behavior"——**这是目前与我们功能域重叠最多的产品之一，值得逐条对比。**

**Vet 是什么（一手调研结论）**：
- **核心机制**：快照 repo + diff（可选附加任务目标与 **Agent 对话历史**），跑一组 **LLM 检查**（issue codes：logic_error、insecure_code 等，带 severity/confidence 评分），过滤去重后输出问题清单；自带可定制的 issue 指南（guides.toml）与团队配置 profiles。
- **两个审查对象**：(1) **代码变更的正确性**（LLM 审 diff）；(2) **Agent 行为与用户意图的对齐**——读 Agent 对话历史，抓"Agent 说测试过了但其实没跑""需求要 X 却悄悄塞了假数据"这类行为级问题。
- **分发形态**：CLI（`pip install verify-everything`）、Agent Skill（装进 `.claude/.codex/.agents` 技能目录，Agent 写完代码后自查）、GitHub Action（PR 自动 review）。BYOM（自带 API key）或复用 Claude/Codex 订阅，零基础设施。
- **明确不做**：不执行测试、不做确定性分析、不做重复代码检测、无任何事前（生成前）介入。

**逐维度对比**：

| 维度 | Vet | Ward | 判定 |
| :--- | :--- | :--- | :--- |
| **介入时机** | 全部**事后**（写完后自查 / PR 时） | **事前**（M1 生成前查重，hook 强制触发）+ 事中（内环 advisory）+ 事后（M2/M3/M4） | Ward 覆盖全周期；Vet 只在损失造成后介入 |
| **重复代码治理** | 无 | M1 前置拦截（四层指纹 + 块级）+ M6 存量整合 + jscpd 独立趋势度量 | Ward 独占，GitClear 数据证实的行业第一梯队痛点 |
| **正确性验证方式** | 纯 LLM 判断 | M3 外环在沙箱**真实执行**项目测试/lint/类型 + 重构场景差分测试 | Ward 是"裁决"，Vet 是"意见"；LLM 无法证明"测试真的跑过"，Ward 能 |
| **Review 摘要** | LLM 审 diff 直出问题清单 | M2 确定性符号 diff 为骨、结构化槽位生成 + 逐句锚点校验器，每条事实锚定行号 | Ward 抗幻觉有**生成结构保证**；Vet 的问题清单无确定性锚点 |
| **意图对齐** | **读 Agent 对话历史**做行为-意图比对（零配置） | M4 机器可检查断言（含 behavior_diff + 逐语言 api_compat）+ CI 确定性执行 + M4-b 对话级比对兜底（带指标与降级触发） | **各有千秋**：Vet 零配置但是 LLM 软判断；Ward 有配置成本但是确定性硬约束 |
| **反馈与度量** | 单次 confidence 评分 | 双通道采纳/拒绝回写、黄金集校准、毕业门槛、jscpd 趋势 | Ward 有纵向数据闭环，Vet 是单次无状态检查 |
| **基础设施** | 零（pip 安装即用） | 本地 daemon + 索引（但可删除重建、fail-open、产物可签名分发）；单二进制安装一行命令 | Vet 更轻；Ward 用重量换事前能力与确定性 |
| **成本/延迟** | 每次 review 一次 LLM 调用（分钟级、按 token 计费） | 内环 advisory 毫秒级、零 LLM 成本；LLM 仅用于 M2 叙述层与 M4-b（统一预算帽） | 高频场景 Ward 边际成本低一个数量级 |
| **语言覆盖** | 模型通吃（LLM 审 diff） | 确定性层按语言逐行接入（§3.0） | Vet 零成本覆盖任意语言；Ward 的确定性优势以语言接入成本为代价——**移动语言恰是 LLM 幻觉高发区，确定性层价值更大** |
| **许可证** | **AGPL-3.0**（企业商用有合规摩擦） | 建议 MIT/Apache-2.0 | Ward 商业友好 |
| **成熟度** | 已发布、HN 验证、issue 分类法成熟 | 设计阶段 | Vet 先发优势真实存在 |

**结论：Ward 的差异化优势与应对**：

**一句话定位差异**：**Vet 是"事后 LLM 终审官"，Ward 是"全周期护栏"**——Vet 回答"这次写得对不对"，Ward 还要回答"写之前有没有重复、跑没跑真测试、偏离规格没有、趋势在变好还是变坏"。

**Ward 的四条护城河**：
1. **时机**：唯一在生成前介入（M1），把问题消灭在成本最低点——Vet 架构上无法前移到生成前（它没有索引）。
2. **确定性**：真实执行测试 + 机器可检查断言（M3/M4），而非"LLM 觉得对"。Vet 自己都把"Agent 谎称测试通过"列为头号案例——但 LLM 检查同样可能谎称，只有沙箱执行是裁决。
3. **治理而非审查**：重复拒绝率与重复率趋势（jscpd）、双通道闭环、毕业门槛（§9）——Ward 产出的是工程效能数据，Vet 产出的是单次问题清单。
4. **许可证与商业模式**：MIT vs AGPL。

**诚实承认 Vet 的长处（并已吸收）**：对话级意图对齐是聪明的零配置设计——已吸收为 M4-b；其 issue codes 分类法（可定制 guides）值得 M2 风险标记体系借鉴；零基础设施体验提醒我们 Phase 0 必须把安装做到一行命令（Rust 单二进制恰好天然满足）。

**动态跟踪**：Vet 母公司 Imbue 2026 年公开转向自主企业 Agent 平台（$200M Series B，方向性核验，§12），vet 作为单一工具的维护节奏存在不确定性；星数等第三方统计口径未逐条核实。§11 各对比表"成熟度"行需**每季度复核**，不随 v0.6.1 定稿冻结。

**竞合关系判断**：两者更多是互补而非替代——Ward 甚至可以在 M2/M3 报告管道中**调用 Vet CLI 作为 LLM 审查员之一**（进程级调用不触发 AGPL 传导），让 Vet 的 LLM 判断与 Ward 的确定性证据在同一份报告里互相印证。真正的威胁不是 Vet，而是平台方内建（Anthropic 已推出多 Agent 代码评审，见 §12）——这要求 Ward 的索引层与度量闭环做到平台不愿做的深度。

### 11.2 Ward vs Probe（probelabs/probe）—— v0.6.1 新增

> 调研对象：probelabs/probe（[仓库](https://github.com/probelabs/probe)），"code and markdown context engine, with a built-in agent, made to work on enterprise-scale codebases"。2026-08 一手核验：README 与 [ARCHITECTURE.md](https://github.com/probelabs/probe/blob/main/ARCHITECTURE.md) 全文。**这是与 Ward 技术栈最接近的产品（Rust 检索内核 + tree-sitter + BM25 + MCP + 本地优先），但问题域不同——值得最细粒度对比。**

**Probe 是什么（一手调研结论）**：
- **定位**："读代码比写代码多 10 倍"的**上下文引擎**：把代码当代码（AST 结构）而非文本，一次调用给 Agent 完整、低噪的上下文；自带"理解型 Agent"（code-explorer / engineer / code-review / architect 四种 persona，支持 `--allow-edit` 改代码）。
- **技术路线（第三条路）**：不用 grep（纯文本）也不用 embedding（需索引+向量库），而是 **AST 感知结构检索 + 零索引**：Elasticsearch 式布尔查询（AND/OR/+required/-excluded/"短语"/ext:rs/lang:python）→ BM25/TF-IDF/hybrid 排序（SIMD 加速）→ tree-sitter 提取**完整代码块**（函数/类整体，而非切碎文本块）。确定性：同查询同结果，无模型方差、无过期索引。本地优先、token 预算、会话级去重、可选 BERT rerank。
- **架构与分发**：**Rust 检索内核**（搜索/提取/排序，`src/`）+ **Node.js SDK/CLI/MCP 包装层**（ProbeAgent 循环、MCP server 在 `npm/src/agent`）——**MCP 与 Agent 层运行在 Node.js，分发第一路径是 `npx -y @probelabs/probe@latest`**。安全边界：allowedFolders + 路径穿越校验。
- **语言覆盖**：Rust、Python、JS/TS、Go、C/C++、Java、Ruby、PHP、**Swift**、Solidity、Crystal、C# 等——**无 Kotlin、无 Objective-C**。
- **明确没有**：无持久索引（每次现扫）、无查重指纹、无采纳反馈、无验证执行、无 diff 摘要、无规格断言——它不回答"该不该写"，只回答"在哪里、是什么"。

**逐维度对比**：

| 维度 | Probe | Ward | 判定 |
| :--- | :--- | :--- | :--- |
| **定位** | 上下文引擎（读懂代码） | 护栏与验证层（治理产出） | 问题域不同：检索 vs 治理 |
| **数据形态** | **零索引**：每次现解析现排序，无状态、无过期问题 | **持久化可重建索引**：指纹需要落盘 | Probe 冷启动无敌；Ward 的索引是为指纹/度量服务，不是为搜索速度 |
| **查重能力** | 无（检索≠查重；无任何指纹概念） | **核心**：四层指纹 + 块级 + 双通道采纳/拒绝度量 | Ward 独占，这是产品边界而非技术差距 |
| **介入时机** | 被动调用（Agent 决定要不要查） | hook 强制触发（生成前拦截）+ 事中 advisory + 事后 CI | Ward 全周期 |
| **验证/意图/摘要** | 均无（不跑测试、无断言、无 diff 摘要） | M3 沙箱裁决 + M4 断言 + M2 锚定摘要 | Ward 独占（P0/P1 痛点域） |
| **度量与治理** | 无（确定性无状态工具） | 采纳/拒绝回写、毕业门槛、趋势度量 | Ward 独占 |
| **语言覆盖** | 无 **Kotlin/Objective-C**（Java/Swift 有） | 首选五语言**含 Kotlin/OC**（§3.0 逐行接入） | **Ward 矩阵恰好覆盖 Probe 盲区**——移动语言体系的事实差异点 |
| **分发形态** | Rust 内核 + **Node.js MCP/Agent 层**，第一路径 npx | **单静态 Rust 二进制** | 移动工程师无 Node 工具链——Probe 的 Node 依赖是其移动场景摩擦，Ward 分发占优 |
| **冷启动/规模** | 任意仓库即刻可用（企业级 codebase 定位） | 需索引预热（CI 归档缓解）；F11 预算 ≤5×10⁵ 达标 | 超大仓即时性 Probe 占优；Ward 用索引换治理能力 |
| **Agent 能力** | 内建理解型 Agent（personas、--allow-edit、委派） | 不内建 Agent（Ward 是被调用的护栏） | Probe 正向"理解型 agent"演进；Ward 刻意不做 Agent（P2） |
| **安全** | allowedFolders + 路径校验 | 沙箱隔离细则 + 索引签名 + YAML 硬化（§7） | 各自对应各自风险面 |

**结论：竞合定位与集成策略**：

**一句话定位差异**：**Probe 回答"代码在哪里、是什么"，Ward 回答"要不要写、写得对不对、趋势好不好"——Probe 是 Agent 的眼睛，Ward 是 Agent 的护栏。**技术栈同源（Rust 内核 + tree-sitter + BM25 + MCP）意味着它是最像我们的产品，但问题域正交。

**集成机会（P4 复用优先的最优解）**：
- **M5 Context Cards 若启动，检索后端直接进程级集成 Probe CLI，不自研**——Ward 只做卡片组装与治理数据（采纳/风险/测试报告）注入（§3-M5 已落位）；
- **M1 不集成**：指纹层需要持久索引与定制归一化，Probe 零索引模型不适用；M1 的 BM25 召回继续走自建索引（Phase 0 已含）；
- **不集成的语言**：Probe 无 Kotlin/OC，M5 对这两门语言需自行桥接或降级为符号索引输出——这是 §3.0 接入矩阵之外对 M5 的附加成本，计入 M5 启动评估。

**威胁监控（与 Vet 同纪律）**：Probe 的演进方向是**理解型 Agent**（--allow-edit、personas、委派），当前不与 Ward 冲突；但它离"写代码"最近，若横向扩展到查重/治理/验证域，将直接撞上 M1/M2/M3——与平台方内建、Vet 并列为**三个季度跟踪向量**。**反制：我们的壁垒不在检索（Probe 已商品化），而在指纹-度量-验证的数据闭环——继续把资源押在差异化模块，不跟随其 agent 化。**

### 11.3 与其他工具的关系

- **事后 review 工具（CodeRabbit 等）**：互补而非替代——Ward 供事前拦截与确定性证据，CodeRabbit 供事后 LLM 审阅。客户已部署 CodeRabbit 时，M2 叙述层可降级为纯结构化清单（跳过 LLM 层），避免双摘要噪音——此配置作为 `.ward/config.toml` 选项。
- **通用代码检索（probe 之外的同类，如 grepai/Octocode 等 embedding 检索）**：probe 的"零索引 AST 检索"路线已证明检索层被商品化（§11.2），Ward 不自研检索的决策因此是稳定的；后续所有"要不要自研检索"的提议一律先对照 probe。

---

## 12. 证据与引用

以数据驱动的方案必须附源，否则等于"愿景当规格"的变体。核验状态：✓ = 已独立核验；✓方向 = 方向性核验（具体时点/数字未逐条核实）；待补 = 作者须在对外发布前补链接。

| 主张 | 来源 | 核验状态 |
| :--- | :--- | :--- |
| imbue-ai/vet 存在、AGPL-3.0、pip 包 verify-everything | [GitHub: imbue-ai/vet](https://github.com/imbue-ai/vet)；[PyPI: verify-everything](https://pypi.org/project/verify-everything/0.2.13/) | ✓ |
| Anthropic 多 Agent 代码评审（平台方威胁） | [Claude Code Code Review 文档](https://code.claude.com/docs/en/code-review)；[InfoWorld 报道](https://www.infoworld.com/article/4143297/claude-code-adds-code-reviews.html) | ✓ |
| Claude Code PreToolUse 不支持注入 additionalContext | [issue #15664](https://github.com/anthropics/claude-code/issues/15664)；[issue #19432](https://github.com/anthropics/claude-code/issues/19432) | ✓ |
| Cursor postToolUse additional_context 不注入模型上下文（未修复） | [Cursor 论坛 bug 帖](https://forum.cursor.com/t/posttooluse-hooks-additional-context-not-injected-into-agent-model-context/158168) | ✓ |
| jscpd 活跃维护、Rust 重写、自带 MCP server | [GitHub: kucherenko/jscpd](https://github.com/kucherenko/jscpd)；[lib.rs crate](https://lib.rs/crates/jscpd)；[性能对比（v5.0.12）](https://github.com/kucherenko/jscpd/blob/v5.0.12/docs/performance-comparison.md) | ✓ |
| DECKARD：子树特征向量 + LSH，非 simhash | [ACM: ICSE 2007](https://dl.acm.org/doi/abs/10.1109/ICSE.2007.30)；[Semantic Scholar](https://www.semanticscholar.org/paper/DECKARD%3A-Scalable-and-Accurate-Tree-Based-Detection-Jiang-Misherghi/2d3efc22854e07d2b84c92446ef5e8cdd2c6b965) | ✓ |
| probelabs/probe：Rust 检索内核 + Node.js MCP/Agent 层、零索引 AST 检索 + BM25、四 MCP 工具、无 Kotlin/OC | [GitHub: probelabs/probe](https://github.com/probelabs/probe)；[ARCHITECTURE.md](https://github.com/probelabs/probe/blob/main/ARCHITECTURE.md)；[官方文档](https://probelabs.com/docs/probe) | ✓ |
| Imbue 战略转向自主企业 Agent 平台（Vet 维护节奏不确定性） | [Imbue $200M Series B 报道](https://aimenta.ai/news/imbue-singapore-series-b-autonomous-apac-enterprise-agents-2026) | ✓方向 |
| 官方 MCP Rust SDK 活跃维护（SEP 扩展落地中） | [modelcontextprotocol/rust-sdk](https://github.com/modelcontextprotocol/rust-sdk) | ✓ |
| sqlite-vec 官方 Rust 集成 | [sqlite-vec Rust 集成文档](https://github.com/asg017/sqlite-vec/blob/563a3e60/site/using/rust.md) | ✓ |
| cargo-semver-checks / binary-compatibility-validator / japicmp / swift-api-digester 四工具存在且为各生态标准 | 各项目仓库（链接待补，作者发布前补齐） | 待补 |
| GitClear 2.11 亿→6.23 亿行、克隆 4 倍、≥5 行重复块 8 倍、重构占比 24%→3.8%、churn 3.1%→5.7% | GitClear 年度代码质量报告（年份与链接） | 待补 |
| DORA 2024/2025 指标 | DORA 报告（链接） | 待补 |
| 68%+ Agent PR 长期无人审；+91% review 时间；+154% PR 体积 | Agentic SE 调查原文（链接） | 待补 |
| 约束衰减约 30pp | 业界 agentic coding 研究报告（链接） | 待补 |
| Context Rot | 研究原文（链接） | 待补 |
| Kiro / Spec Kit、Serena/LSP、tree-sitter 五语言语法成熟度 | 各项目仓库（链接） | 待补 |

---

## 附：一句话版本

**Ward = 一个 Rust 单二进制，在 Agent 写代码前查一次重（Spot，hook 强制触发、以重复拒绝率度量成效）、在 PR 上贴一份每条事实可回溯的复盘（Replay）、在 CI 里跑确定性验证与规格断言（Catch + Form Check，外环 fail-closed）——首选服务 Rust/Kotlin/Swift/Java/OC 五门移动语言，检索层交给 Probe（Agent 的眼睛），治理层交给 Ward（Agent 的护栏），git 仍是唯一的神，我们只做一个可删除、可重建、会认错的索引层，内环永远不挡路。**

*Ward off AI slop.*

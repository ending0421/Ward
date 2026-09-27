# Task Spec — M1 召回保真与治理闭环（v0.8.0）

> 任务 ID：`2026-09-m1-recall-fidelity`
> 上游依据：GitHub issues #12–#16（同一份 97,743 符号部署的现场数据）
> 版本：目标 v0.8.0（指纹空间变更 + schema v9 + 契约扩展，按 0.x 语义需 minor bump）

```yaml
assertions:
  - kind: no_new_dependency
  - kind: api_compat
  - kind: must_pass
    suite: "crates/**"
  - kind: max_files_changed
    value: 15
```

## 1. 背景与已核实根因

### 1.1 #16 裸类型参数被丢弃（已定位，代码级复核确认）

`specificity.rs::parameter_types` 的过滤器

```rust
.filter(|c| !c.kind().contains("identifier") || c.child_count() > 0)
```

意图跳过"参数名"，实际把**所有无子节点的 identifier 类节点**都删掉。tree-sitter-rust 中
裸类型是 `type_identifier`（kind 含 "identifier" 且无子节点）→ 类型被删；
包装类型（`&T` → `reference_type`、`Vec<T>` → `generic_type`）非 identifier 类 → 存活。
后果：`fn g4(x: Canvas)` 特异度 0.0（应为 1.0）→ 误判 `low_confidence` → gate 跳过；
混合签名分母缩水（`0.50` 应为 `0.67`）。2 个月 gate 语料中 8% 签名命中。

### 1.2 #15 逐字复制不自匹配（本 spec 作者实测复现，归因修正）

| 查询形态 | 实测（v0.7.0） |
| :--- | :--- |
| 纯签名 | 原件 `near 0.891`（**应为 1.0**） |
| 完整函数当签名 | `structural 1.0` ✅ |
| 签名 + 精确 `--body` | `near 0.891`（body 只进块层，指纹未用） |

根因（两处，均为代码可验证）：

1. `fingerprint::subtree_features_excluding(node, excluded)` 只跳过 `excluded` 子树，
   **父节点的 children 列表特征里仍保留 body 子节点**；查询侧（无 body）与索引侧
   （body 被排除但仍在父特征里）因此特征不等 → 自匹配被系统性压低到 ~0.89。
   在大仓库中同形兄弟（0.94）会挤掉 top_k=5，报告者的"完全不可见"即由此而来。
2. `--body` 提供时，L1/L2 指纹仍只用无 body 的签名；而"签名+完整 body"本可精确
   复现索引侧的全节点 struct_hash（实测形态 B 证明该路径可用）。

### 1.3 #14 MCP 缺 ack 工具（工具面缺口，已核对 10 工具清单）

`ward-mcp` 暴露 10 个工具，无 `ack`/`infer`；MCP-only 集成（无 shell）能读 spot
结果却无法登记 ack，也无法触发客观通道推断。#9 的"零 Agent 配合"故事在 MCP 面断裂。

### 1.4 #13 缺自有转化漏斗（治理闭环缺口）

deny→ack→retry→landing 的每个事件都已在 store 里（advisories / acks / infer 的
inferred_action），但 `ward stats` 不输出漏斗：维护者看不到"gate 是否改变行为"。
外部实测 0/84 reuse，工具自己一天就能打印出来。

### 1.5 #12 未合并 worktree 符号不可见（最大盲区）

索引只含主 checkout 的已跟踪文件；两个 worktree 各自写的近似重复在合并前互不可见。
v0.7.0 的 worktree **索引共享**解决"查对索引"，未解决"worktree 新符号在索引里"。

## 2. 交付项与断言

### S1（#16）裸类型计入特异度

- `parameter_types` 只跳过参数的**第一个** identifier 子节点（参数名）；
  优先使用字段访问器（`child_by_field_name("type")`，Rust/Java/Swift 有；Kotlin 无字段，
  回退"跳过首个 identifier 子节点"）。
- 断言行（全部必须通过，来自 issue 的判别矩阵）：

  | 签名 | 期望特异度 |
  | :--- | ---: |
  | `fn g5(x: &Canvas)` | 1.0 |
  | `fn g2(a: u64, b: &ItemId)` | 0.5 |
  | `fn f3(a: &Frame, b: f32)` | 0.5 |
  | `fn g1(a: Frame, b: f32)` | 0.5 |
  | `fn g4(x: Canvas)` | 1.0 |
  | `fn f2(a: u64, b: ItemId)` | 0.5 |
  | `fn g3(a: u64, b: ItemId, c: &Frame)` | 0.67 |
  | `fn m1(&mut self, id: ItemId) -> Status` | 1.0 |

- 包装不变性不变量测试：把每个裸类型包成 `&T` 后，域类型/总数比值不变。

### S2（#15）逐字复制必须自匹配

- `subtree_features_excluding`：被排除子树必须同时从**父节点 children 列表特征**中移除。
- spot：当 `--body` 提供且形如完整声明（解析出的首节点是 symbol-kind 且非
  signature-alias）时，用 `签名 + body` 作为指纹查询文本（L1/L2 一致），保留
  仅签名路径的既有行为。
- 断言：
  1. 纯签名查询 → 原件 `kind ∈ {structural, near}` 且 `similarity ≥ 0.99`；
  2. 完整函数当签名 → `structural` 且 `similarity == 1.0`（现状保持）；
  3. 签名 + 精确 body → 原件 `structural` 且 `similarity == 1.0`；
  4. 逐字复制查询的**首位**命中即原件（不被同形兄弟挤出）。

### S3（#14）MCP 补 `ack` 与 `infer` 工具

- 新增 MCP 工具：`ack`（against/kind/reason/repo）、`infer`（repo）。
- 断言：`tools/list` 返回 12 个工具且含 `ack`/`infer`；`ack` 调用后 registry
  可查（与 CLI 同一 store）；README MCP 工具表同步为 12。

### S4（#13）自有转化漏斗

- 新增 `ward stats --funnel`（JSON 亦可）：对每条 advisory 分桶
  `acked | reused | rewritten | abandoned`，给出计数与转化率 + 周序列。

  分桶规则（全部只读既有数据）：
  - `acked`：acks 表存在该 advisory 任一命中符号的登记；
  - `reused`：`inferred_action == accepted`（调用边信号）；
  - `rejected`／`rewritten`：`inferred_action == rejected`，或同一 family
    出现后续 advisory 且最终落地（用 family key = (query_hash, top-1 命中符号)）；
  - `abandoned`：无 ack、无后续同 family advisory、无推断动作，且 advisory
    年龄 > 7 天。

- 断言：e2e 构造四类样本 → 分桶计数与转化率精确匹配；`--json` 输出稳定。

### S5（#12）worktree 新符号可见（未合并来源可标注）

- 配置 `[index] include_worktrees = false`（默认关，显式开启）+ CLI
  `ward index --include-worktrees`。
- 开启时：对每个 linked worktree，只索引**相对 merge-base 新增/变更**的文件
  （`git diff --merge-base --name-only` + 存在性过滤），符号带 provenance。
- schema v9：`symbols.worktree` + `symbols.worktree_branch`（均
  `TEXT NOT NULL DEFAULT ''`）；契约新增 `SpotMatch.worktree`（worktree 绝对根，
  坐标与内容哈希的解析基准）、`SpotMatch.worktree_branch`（分支名，人读标签）
  与 `SpotResult.worktree_symbols`（计数），全部 serde 默认。
- 命中坐标与逐文件新鲜度按 provenance 解析：行号在 worktree 根读取，文件哈希
  以 `<worktree>\u{1}<path>` 作用域键比对（主 checkout 仍用裸路径）——
  未合并内容不会把每条 advisory 拖成 stale。
- 主 checkout 自身不算 worktree（`git worktree list` 首项被排除）：本地未提交
  编辑不得被误标为"未合并来源"。
- 块级指纹不索引 worktree（非目标）；`worktree` 命中的 note 标注
  `未合并来源（worktree <目录>@<分支>）（#12）`。
- 清理：索引结束时，worktree 已不存在的条目整批删除；关闭开关的后一次索引
  清空全部 worktree provenance（开启才收集，谓词可预期）。hook（PostToolUse）
  始终以 `--include-worktrees` 刷新，故静默路径无需人工开启。
- 断言：e2e —— 主 checkout 索引后，worktree 写新函数（已提交）+ 未跟踪文件 →
  `--include-worktrees` 索引 → 主 checkout 查询命中该符号、`worktree` 根与
  `worktree_branch` 标注正确、`stale=false`；关闭开关后命中消失；
  worktree 删除后条目消失。

## 3. 验收

| 项 | 门禁 |
| :--- | :--- |
| 单元/集成测试 | 新增断言全部通过；既有 251 项不回归 |
| clippy / rustfmt | `-D warnings` 零告警、干净 |
| 覆盖率 | workspace 行覆盖 ≥85% |
| 意义验证 | `scripts/verify-meaningful.sh` 11/11（含 L1 精确克隆检查） |
| 本 spec | `ward form-check --spec specs/2026-09-m1-recall-fidelity.md --ci` |
| 发布 | tag v0.8.0 + curated notes（含指纹空间变更的校准影响声明） |

## 4. 非目标

- 不实现 #12 的"轻量备选"（`--against-worktrees` 按查询现扫）——持久化方案已足够且可复用；
- 不做 #15 建议 3 的"same-domain structural 是否阻断"的阈值校准（需黄金集数据，另行开展）；
- 不引入 specificity 加权指纹（#5 建议 3 的遗留项，仍待标注数据）。

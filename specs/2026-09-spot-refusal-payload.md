# Task Spec — 拒绝作答的载荷不得自证成功（v0.9.0）

> 任务 ID：`2026-09-spot-refusal-payload`
> 上游依据：GitHub issue #17（KarlLyu0421，v0.8.0 现场：等价语料消费方 143/200 条误读）
> 版本：目标 v0.9.0（契约字段类型变更 + 新增字段 = 破坏性；0.x 语义需 minor bump）

```yaml
assertions:
  - kind: no_new_dependency
  - kind: api_compat
  - kind: must_pass
    suite: "crates/**"
  - kind: max_files_changed
    value: 15
```

## 1. 已核实根因

### 1.1 exit 3 的载荷是模板默认值（代码级复核确认）

`search.rs` 的 `missing(state)` 模板对"无索引/空索引"这两条**无法作答**的路径填：

```rust
query_specificity: 0.0,
low_confidence: false,   // ← 从未计算过，却以"已计算且不低"出现
```

CLI 的 `print_json` 一律 `Envelope::ok(value)` ⇒ **`ok: true`**；MCP 的 `tool_ok` 同理
（两侧共用 `ward_core::envelope::Envelope`）。于是"没查成"与"查了没重复"在 JSON 层面
**字节级同形**——只有 exit code 或 `index_state` 能区分。现场代价：消费方 143/200 条误读。

### 1.2 同类漏洞：`spot-file` 的 fail-open 报告

CLI 在 `Store::open_for_query` 失败时返回 `FileSpotReport { changed_symbols: [], checked: 0,
advisories: [] }`，与"这次写入没有新增/变更符号"**完全同形**；hook 消费方无法区分
"无法检查"与"检查过、没发现"。`FileSpotReport` 目前没有任何索引状态字段。

### 1.3 不属于本 spec 的同类项（非目标有理由）

`form-check`/`catch-run --full`/`compat-check` 在 exit 1/2 时 `ok: true`：它们的载荷
**携带 verdict 字段本身**（fail/unknown 是计算结果，不是模板默认值），`ok` 表示
"工具跑完了并给出裁决"。本 spec 不改变这层语义，但要求文档写清楚消费方式。

## 2. 交付

### S1（#17）拒绝作答的载荷不再携带伪造默认值

- `SpotResult.query_specificity: Option<f64>`、`low_confidence: Option<bool>`；
  `#[serde(default, skip_serializing_if = "Option::is_none")]` ⇒ **无法作答时字段缺失**
  （不是 `false`/`0.0`），可作答时与今日一致（`true/false`、具体数值）。
- `SpotResult.cannot_answer: bool`（serde 默认 false）：载荷级自描述标记。
  判定与 CLI exit 3 规则**同源**：`index_state ∈ {missing, empty}` ∨ `stale_severe`。
- `impl SpotResult { pub fn refusal_reason(&self) -> String }`：拒绝原因的人类可读文本，
  由 CLI/MCP 放进信封的 `error`（避免两处各写一份判断）。
- 快速路径（`--quick`，`index_state: "unchecked"` + 低特异度）是**诚实作答**：
  `low_confidence: Some(true)`、`cannot_answer: false`。

### S2（#17）信封不再对拒绝作答宣示成功

- `envelope::Envelope::refused(data, reason)`：`ok: false` + `error: Some(reason)`
  **且保留 data**（载荷本身是解释：`index_state` / `cannot_answer`）。
- CLI `spot --json`：`cannot_answer` ⇒ `refused` 信封；退出码仍为 3（不变）。
- MCP `spot`：同一信封（MCP 无退出码，`ok: false` 是其唯一机器信号）。
- 两者共用 `ward_core::envelope::Envelope`，不新增第二套形状。

### S3（#17 同类）`spot-file` 报告声明索引状态

- `FileSpotReport.index_state: String`（serde 默认 `"unchecked"`，旧 payload 兼容）：
  `missing`（无索引）/`empty`（0 符号）/`fresh`/`stale`（有索引时的新鲜度）/
  `unchecked`（非源文件、不可读、解析失败——本报告未向索引求证）。
- CLI fail-open 分支填 `missing`；正常路径由核心按 store 计算。
- hook 退出码语义不变（fail-open，P3）：载荷可自证"没查成"，静默不等于撒谎。

### S4 文档与断言

- README「Consumer boundary contract」新增一行：**exit 3 / `cannot_answer: true` 时，
  `ok: false` 且 `low_confidence`/`query_specificity` 缺失——门禁必须读 `cannot_answer`
  或 exit code，绝不能读 `matches`/`low_confidence`**；并说明 verdict 类命令（form-check/
  catch-run/compat-check）的 `ok` 与退出码关系（S1.3）。
- `docs/USAGE.md` 边界契约速查同步；`docs/release-notes/v0.9.0.md` 声明破坏性变更与迁移方式。
- 断言：e2e 覆盖 missing/empty/quick/严重滞后四态 × (`cannot_answer`, `low_confidence`,
  `query_specificity`, 序列化键存在性)；CLI smoke 断言 `ok:false` + 缺字段 + exit 3；
  `spot-file` 断言 `index_state` 取值。

## 3. 验收

| 项 | 门禁 |
| :--- | :--- |
| 单元/集成测试 | 新增断言全部通过；既有 258 项不回归 |
| clippy / rustfmt | `-D warnings` 零告警、干净 |
| 覆盖率 | workspace 行覆盖 ≥85% |
| 意义验证 | `scripts/verify-meaningful.sh` 11/11 |
| 本 spec | `ward form-check --spec specs/2026-09-spot-refusal-payload.md --ci` |
| 发布 | tag v0.9.0 + curated notes（含契约迁移说明） |

## 4. 非目标

- 不改 verdict 类命令（form-check/catch-run/compat-check）的 `ok` 语义（见 §1.3）；
- 不为 `spot` 增加"自动刷新索引后重试"（静默路径已由 hook 刷新；拒绝仍然要显式）；
- 不改变 exit 3 的触发条件（本 spec 只让载荷与退出码说同一句话）；
- 不改索引 schema（v9 不变）与指纹空间。

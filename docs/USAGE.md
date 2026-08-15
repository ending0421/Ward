# Ward 使用指南

> 正确使用 Ward 的关键不是记命令，而是理解它的两环哲学：
> **内环（本地/MCP）只建议、永不挡路；外环（CI）确定性裁决、失败即红。**
> 它不阻止你写任何代码，只保证"你写之前知道有没有重复、合并之前知道
> 有没有真跑过测试、偏离规格时有人被拦住"。

## 0. 心智模型（先读这个，再动命令）

| 观念 | 正确理解 | 错误理解 |
| :--- | :--- | :--- |
| Ward 是什么 | 护栏与验证层：索引、度量、建议、CI 裁决 | 第二个 Git / 代码审查员 / 拦路器 |
| 谁会失败 | 外环 CI 失败（fail-closed，红 = 安全方向） | 内环 daemon 永不失败（fail-open） |
| 索引 | `.ward/` 是可丢弃缓存，删除重建只损失速度 | 需要备份/迁移的数据库 |
| 建议 | 采纳/忽略由你决定，但**请回写 action** 校准阈值 | 必须遵守 |
| LLM | 只叙述确定性事实（句句锚定行号），无 provider 自动回退结构化清单 | 裁决者 |
| unknown | "没有证据"——在外环等于红 | 一种可以通过的绿色 |

## 1. 安装（一行）

```bash
curl -fsSL https://raw.githubusercontent.com/ending0421/Ward/master/scripts/install.sh | sh
# 项目级（团队共享 + Claude Code hooks）:
cd <项目> && curl -fsSL https://raw.githubusercontent.com/ending0421/Ward/master/scripts/install.sh | sh -s -- --project
```

装完：`ward --version`、`claude mcp list`（应显示 `ward ✔ Connected`）。

## 2. 日常工作流（人类开发者）

```bash
cd <项目>
ward init --repo .        # 生成 .ward/config.toml（可调阈值/抑制路径/lint 命令）
ward index --repo .       # 首次索引；此后增量（未变文件 0.02s 跳过）
```

**改代码前查重（Spot）**——签名写得越接近真实声明，证据越强：

```bash
ward spot --repo . \
  --intent "防抖函数，支持 leading/trailing" \
  --signature "pub fn debounce(f: &dyn Fn(u64), ms: u64) -> u8"
```

解读输出：

| 字段 | 含义 | 行动 |
| :--- | :--- | :--- |
| `kind: structural` | 归一化后结构全等（克隆/纯改名） | 强证据：直接复用或扩展 |
| `kind: near` | 结构近重复（copy-then-modify） | 相似度 ≥0.92 强提示 / ≥0.80 弱提示 |
| `kind: block` | 函数内语句窗口相似（需 `--body "..."`） | 检查函数内部是否抄了段落 |
| `kind: textual` | 只有文本证据（**永不强提示**） | 仅供参考 |
| `stale: true` | 索引过期（HEAD 变了或有未提交修改） | 按弱证据对待，先 `ward index` |

**用完整函数源码做签名**可命中 L1 精确层；**写完的函数体用 `--body`** 可触发块级检查。

**采纳后必须回写**（这是阈值校准的燃料，不写会污染你的黄金集）：

```bash
ward action <advisory_id> accepted   # 或 ignored / dismissed
```

**Agent 写文件后自动查重（PostToolUse hook）**：`--project` 安装时已配置。
hook 会对比写入前后索引，对**新增/变更符号**逐个跑结构查重，把 ≥0.92 的
强命中以 additionalContext 注入 Agent 上下文（≥0.92 时提示"优先复用"）。
手动触发同款检查：`ward spot-file --repo . --path src/x.rs`。

**提交前验证**：

```bash
ward catch-run --repo .              # 内环 lint/type 预检（秒级、无 Docker）
ward replay HEAD~3 HEAD --repo .     # 审自己的 diff：符号级变更 + 影响面 + 风险
ward clusters --repo .               # 存量重复盘点（M6 合并建议）
ward card debounce --repo .          # 一键上下文卡片
```

**LLM 增强（可选）**：设置 `WARD_LLM_URL`（OpenAI 兼容 `/chat/completions`）、`WARD_LLM_KEY`、`WARD_LLM_MODEL` 后：

```bash
ward replay HEAD~3 HEAD --narrate --repo .          # 摘要叙述（句句锚定，F6 回退）
ward intent-check --repo . --requirement "实现防抖" # 需求 vs diff 软性比对
```

## 3. Spec 驱动的任务（M4 的正确姿势）

任务开始时落一份 `specs/<task-id>.md`（**人审后入库**，别让写代码的 Agent 自定自判）：

```yaml
assertions:
  - kind: no_new_dependency
  - kind: api_compat
  - kind: must_pass
    suite: "tests/utils/**"
  - kind: behavior_diff
    suite: "tests/golden/**"
  - kind: max_files_changed
    value: 6
```

- 每个 commit message 引用条款号：`fix debounce edge case [spec:a2]`；
- 内环自检：`ward form-check --repo . --spec specs/<task>.md`（deferred/unknown 是**诚实的**，不是失败）；
- **CI 外环裁决**：`ward form-check --spec … --ci`（fail→exit 1，unknown→exit 2）；
- 需求中途合法变更？**改 spec 走 PR**（F12），不是绕过断言。

## 4. CI 集成（外环 fail-closed 的最小配置）

```yaml
- name: Ward 外环裁决
  run: |
    ward index --repo .
    ward form-check --spec specs/$SPEC --ci          # 失败=1，未知=2
    ward verify --full --repo . || exit $?           # 沙箱真跑测试（无沙箱=2）
    ward compat-check --base main --repo . || exit $? # API 兼容（无工具=2）
```

退出码约定：`1` = 裁决失败（红），`2` = 证据不足（unknown 不绿灯，也是红）。

## 5. 与 Claude Code / Codex 协作

装好 MCP 后，Agent 会获得 10 个工具。**给 Agent 的正确指令**（写进项目规则文件）：

```text
- 写任何新函数前：调用 ward spot（intent + 拟写签名），
  有 structural/near 命中且 similarity >= 0.92 时复用现有实现；
- 实现完成后：调用 ward catch_run；全量测试留给 CI 外环；
- 每个 commit message 引用 [spec:<条款号>]；
- 收到 advisory 后调用 ward spot_action 回写 accepted/ignored/dismissed。
```

hooks（`--project` 安装时已配）：PreToolUse 大改动 deny-with-reason、PostToolUse 写后自动索引。

## 6. 校准纪律（长期正确的关键）

- 阈值 0.92/0.80 是**初始值**；每周从 `advisories` 表抽样人工标注（黄金集），precision <60% 或误报 >20% 就调；
- `ward action` 的回写率掉下来 → 阈值失真 → 全指标失真，这是单点风险；
- **双标一致性护栏（spec §8）**：两个标注者各自对同一批 match 打标，
  `ward stats` 输出 Fleiss κ；κ < 0.4 说明标注标准在漂移，先对齐标准再校准：
  ```bash
  ward label next --annotator alice --repo .      # alice 的待标队列
  ward label set <advisory> <match> y --annotator alice
  ward label next --annotator bob --repo .        # bob 的独立队列
  ward stats --repo .                             # 看"标注一致性"行
  ```
- 每季度复核 `ward clusters` 趋势与 spec 衰减（`contract_runs` 纵向分析）。

## 7. 反模式清单（别这么用）

1. ❌ 把 spot 当搜索工具用（那是 probe 的活）——不写 intent/signature 只能拿到弱证据；
2. ❌ 指望内环"拦截"Agent——它是 advisory，拦截只存在于外环 CI；
3. ❌ 忽略 `stale: true` 仍采信强提示；
4. ❌ 让生成代码的同一个 Agent 自己写 spec 自己审（考生出卷）；
5. ❌ 把 `unknown` 当通过（外环它会红，别在本地自我安慰）；
6. ❌ 不回写 action（阈值校准失明）；
7. ❌ 修改 `.ward/index.db`（F1 会重建，改了白改）。

## 8. 快速参考

```bash
# 核心：init index spot spot-file replay catch-run verify form-check compat-check
#        intent-check card clusters action
# 治理：infer setup-hooks label calibrate snapshot stats
# 运维：daemon service doctor report issue
ward <子命令> --help
```

- 配置：`.ward/config.toml`（thresholds / top_k / suppress / languages / lint / sandbox）
- 索引：`.ward/index.db`（可随时 `rm -rf .ward` 重建）
- 规格：`specs/<task-id>.md`（入库、人审）
- 回滚：卸载 `scripts/install.sh --uninstall`；工程回滚 = 移除 CI step + 删 `.ward/`，零残留

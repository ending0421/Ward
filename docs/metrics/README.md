# Ward 指标基线（docs/metrics/）

按 spec §9 归因纪律：**基线期数据 + 干预期数据 + 人类提交对照**，
全部由 `ward daemon`（无感模式）自动采集到 `.ward/metrics.jsonl`，
关键数字在此人工登记留档。

## 当前基线（2026-08-15 首测）

| 指标 | 值 | 来源 |
| :--- | :--- | :--- |
| jscpd 重复率（token 级） | **2.60%**（376/14483 行） | jscpd --min-lines 5 首测 |
| 符号数 | 491 | ward stats |
| 重复簇（测试豁免前） | 53 | ward clusters |
| 黄金集标注 | 2 | ward stats |
| 测试 | 176 passed | cargo test |

## 采集节奏（daemon 自动）

- 文件变更 → 增量索引；HEAD 变更 → 推断采纳；
- 每日 → 快照；每周 → jscpd + 聚类 + 校准建议，追加 `.ward/metrics.jsonl`；
- **唯一人工环节**：每周 `ward label next --count 20` 标注（黄金集的天性，
  无法自动化），daemon 会在报表中提醒待标注数。

## 判读纪律

- 重复率/采纳率要**环比**看趋势，不看单点；
- 对照：人类提交 vs `[ai]` 标记提交分桶；
- 连续两个周期不达标 → 降级或下线（spec P5）。

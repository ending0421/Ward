# Ward 性能基准（F11）

> 合成仓库基准：确定性生成器 + 真实引擎计时。合成数据保证可复现、不依赖
> 任何专有代码；真实仓库验证是独立的长周期活动（spec §9）。
>
> F11 硬承诺：**符号量 ≤5×10⁵ 时全量索引 <10min、spot 查询 P99 <100ms
> 必须达标**；超 5×10⁵ 进入监控，达 10⁶ 触发 scale-out 评估。

## 基线数字（2026-08-15 实测）

环境：macOS aarch64（Apple M 系列）、`cargo build --release`、ward v0.4.0+
（含本批 F11 修复）。生成参数：`--languages rust,kotlin,swift
--cluster-ratio 0.3 --seed 42`（30% 符号为同模板改字面量的 copy-then-modify）。

| 指标 | 10⁴ 符号（51 文件） | 10⁵ 符号（501 文件） | 规格门槛（@5×10⁵） |
| :--- | :--- | :--- | :--- |
| 全量索引（冷） | 7.45 s | 11.1 s | <600 s ✅ |
| 增量索引（无变更） | 0.019 s | 0.11 s | — |
| spot p50 | 16.7 ms | 86.5 ms | — |
| spot p99 | 24.9 ms | 92.8 ms | <100 ms ✅（余量 7%） |
| spot max | 33.8 ms | 170.7 ms（首次查询预热） | — |
| 重复聚类（0.92） | 0.53 s | 21.2 s | — |
| 数据库体积 | 5.7 MiB | 66.4 MiB | — |

注意：10⁵ 时聚类分桶截断（`truncated: true`，工具如实标记，一个同构家族被
拆分）——10⁵ 级聚类正确性需要后续专项验证，见"已知边界"。

## 复现

```bash
cargo build --release -p ward-bench
./target/release/ward-bench gen --out /tmp/ward-bench-100k \
  --symbols 100000 --languages rust,kotlin,swift --cluster-ratio 0.3 --seed 42
./target/release/ward-bench run --repo /tmp/ward-bench-100k --queries 100
# 冷索引复测：rm -rf /tmp/ward-bench-100k/.ward 后重新 run
```

## 本批基准逼出来的修复

1. **L1 命中洪泛（正确性 + 延迟）**：`search::spot` 的 L1 结构全等扫描
   没有 `top_k` 截断，且每个命中都读一次文件算行号——10⁴ 仓库上一条
   查询命中 3332 个同构符号，单查 118ms 且 advisory 塞满 3332 条
   matches（top_k 形同虚设）。修复后 L1 扫描先截断再物化行号：
   **10⁴ spot p99 118ms → 25ms**，advisory 恢复 top_k 语义。
2. **mentions 表缺 `symbol_id` 索引（O(N²)）**：`set_mentions` 的
   DELETE 全表扫描，10⁵ 全量索引 **584s（9.7min）→ 11.1s**（52×）。
   `blocks.file_path` 同型问题一并补索引。
3. **BM25 每次查询重建**：daemon/hook 进程内多次 spot 时按 store 实例
   缓存（`replace_file` 失效）；单进程 100 查询 p99 收益明显，CLI
   单次调用不受影响（进程启动摊销）。

## 已知边界（后续工作）

- **10⁵ 聚类截断**：chunked 分桶在 10⁵ 符号下截断并拆分同构家族
  （`truncated: true` 诚实标记）；5×10⁵ 规模前需专项验证聚类正确性。
- **spot p99 余量**：10⁵ 时 92.8ms，距 100ms 门槛余量 7%；5×10⁵ 未实测，
  预计需要把 L1 等值扫描下推到 SQL（`WHERE struct_hash = ?`）并做
  BM25 增量维护才能守住。
- **聚类时长**：10⁵ 时 21s，5×10⁵ 预计 3-5min——可接受但需监控。
- 生成器目前每语言单模板；多模板变体会让聚类/查重基准更接近真实分布。

## 维护纪律

- 每次改动影响 index/spot/cluster 路径的 PR，跑一遍 10⁵ 基线对比
  （CI 外环可选 job，防止 F11 退化）；
- 数字更新时同步修改本文件与对应 release notes。

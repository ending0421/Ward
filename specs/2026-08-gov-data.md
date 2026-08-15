# 任务规格：治理数据与校准（v0.2.0 迭代）

> 本文件按 AGENTS.md 规则 8-9 管理：需求/断言变更走 PR 评审（F12）。
> 内环自检：`ward form-check --spec specs/2026-08-gov-data.md`
> 外环裁决：CI 中以 `--ci` 执行（fail=1, unknown=2）。

```yaml
assertions:
  - kind: no_new_dependency
  - kind: api_compat
  - kind: must_pass
    suite: "crates/**"
  - kind: max_files_changed
    value: 15
```

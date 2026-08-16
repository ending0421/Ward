# 主仓库冒烟 Runbook（0.5.0 工程验证）

> 在真实多平台仓库上跑通 Ward 全链路（工程冒烟，**不采指标**）——
> 指标验证是长周期采样工作，另有安排。本 runbook 只回答"工具在每个
> 构建形态上都能用、都诚实"。

## 前提

- ward ≥ 0.5.0（`ward --version`）；
- 仓库形态：Rust 核心（Cargo.toml，含 `extern "C"` 导出 + UniFFI UDL）
  + Android 上层（Gradle/Kotlin，UniFFI 生成绑定）+ iOS 上层
  （Swift/UniFFI 生成绑定 + Xcode 工程）；
- 一个入库的 **C 声明头文件**（FFI 导出面清单）。

## 0. 配置（一次）

```toml
# .ward/config.toml
languages = ["rust", "kotlin", "swift", "java", "objc"]

[ffi]
manifest = "ffi/exports.h"        # 你的固定导出面清单（0.5-3）
artifact_glob = "target/*/release/lib*.so"

[lint]
command = "cargo check --quiet"   # 保持出厂默认 = 自动按构建系统选形
```

## 1. 索引与模块作用域

```bash
ward index --repo .
ward doctor --repo . --json | jq '.data.store.languages'
# 期望：rust/kotlin/swift 符号都在；core/android/ios 的 module 归属正确
ward spot --repo . --intent "x" --signature "pub fn push_fill_quad(...)" --json \
  | jq '.data.matches[].scope'
# 期望：只有 core 模块的命中（跨模块不串味）
```

## 2. 写文件自动查重（hook）

```bash
ward setup-hooks --repo .        # 或安装器的 --project
# 写一个与既有实现相似的新符号 → hook 输出 additionalContext（≥0.92）
```

## 3. 外层裁决

```bash
# Rust API 兼容（全 workspace）
ward compat-check --repo . --base HEAD^ || echo "exit=$?"

# FFI 导出面（构建产物 vs 清单）
cargo build --release
ward compat-check --repo . --base HEAD^ --ffi || echo "exit=$?"
# 期望：removed 为空 = pass；产物新增而清单没改 = pass + 漂移警告

# JVM 侧（在 android 模块目录执行，模块级裁决）
ward compat-check --repo android --base HEAD^ || echo "exit=$?"
# 期望：gradlew apiCheck 真实裁决；任务不存在 = unknown（诚实）

# Apple 侧（macOS）
ward compat-check --repo ios --base HEAD^ || echo "exit=$?"
# 期望：swift-api-digester dump/diagnose 真实裁决
```

## 4. spec 门禁（CI 用）

```yaml
- name: Ward 外环裁决
  run: |
    ward index --repo .
    ward form-check --spec specs/$SPEC --ci     # fail=1 / unknown=2
```

spec 断言增加 FFI 面（0.5-3）：

```yaml
assertions:
  - kind: no_new_dependency
  - kind: api_compat
  - kind: ffi_compat          # nm 对比导出面；removed = 红
  - kind: max_files_changed
    value: 30
```

## 5. UDL 变更人审

```bash
ward replay HEAD~3 HEAD --repo . | grep -A3 UDL
# 期望：UDL 变更带 HIGH 风险标记 + 符号级变更清单
```

## 期望之外的诚实降级（都是设计内行为）

| 场景 | 结果 |
| :--- | :--- |
| Android instrumented 测试 | `unknown`（需模拟器，规格明确） |
| Xcode 工程的 Docker 沙箱 | `unknown`（macOS runner 原生跑，见 CI apple-smoke） |
| 清单/产物缺失时的 FFI 检查 | `unknown` + 修复指引 |
| validator 插件未接入 | `unknown`（不是 fail——接线缺口 ≠ API 破坏） |

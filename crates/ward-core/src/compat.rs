//! M4 `api_compat` — per-language API/ABI compatibility adjudication
//! (spec §3-M4.2). Ward orchestrates each language ecosystem's existing
//! deterministic tool; it never re-implements compatibility checking.
//!
//! Tool matrix (spec §3-M4): Rust → cargo-semver-checks, Kotlin →
//! binary-compatibility-validator, Java → japicmp, Swift/ObjC →
//! swift-api-digester. Only the Rust tool is wired so far; the others
//! report `unknown` honestly (F13 spirit: no tool, no verdict).

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Compatibility verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CompatVerdict {
    Pass,
    Fail,
    Unknown,
}

impl CompatVerdict {
    pub fn as_str(self) -> &'static str {
        match self {
            CompatVerdict::Pass => "pass",
            CompatVerdict::Fail => "fail",
            CompatVerdict::Unknown => "unknown",
        }
    }
}

/// One compatibility check result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompatReport {
    pub verdict: CompatVerdict,
    pub tool: String,
    pub detail: String,
    pub duration_ms: u64,
}

fn read_pipe<R: std::io::Read + Send + 'static>(
    pipe: Option<R>,
) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut text = String::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_string(&mut text);
        }
        text
    })
}

fn run(cmd: &mut Command, timeout: Duration) -> Result<std::process::Output, String> {
    let start = Instant::now();
    // Drain both pipes on reader threads from spawn time: reading only after
    // the child exits deadlocks once it fills the OS pipe buffer (~64KiB on
    // macOS) — a real pass would be misreported as a timeout.
    let mut child = match cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return Err(format!("spawn failed: {e}")),
    };
    let out_pipe = read_pipe(child.stdout.take());
    let err_pipe = read_pipe(child.stderr.take());
    let deadline = start + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // The child is gone: both write ends are closed, the readers
                // hit EOF — joining cannot hang.
                let out = out_pipe.join().unwrap_or_default();
                let err = err_pipe.join().unwrap_or_default();
                return Ok(std::process::Output {
                    status,
                    stdout: out.into_bytes(),
                    stderr: err.into_bytes(),
                });
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    // kill+wait closes the write ends; join so the readers
                    // never leak.
                    let _ = out_pipe.join();
                    let _ = err_pipe.join();
                    return Err("timeout".into());
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            // try_wait failing leaves the child's liveness unknown; joining
            // here could hang on a live writer, so return without the pipes.
            Err(e) => return Err(format!("wait failed: {e}")),
        }
    }
}

/// Run the API compatibility check for the repository's language, selected
/// by build-system detection (spec §3.0 matrix):
///
/// * Rust → `cargo semver-checks check-release --baseline-rev <base>`;
/// * Gradle → `./gradlew apiCheck` (binary-compatibility-validator; the
///   task missing = tool not wired = unknown, never a fake fail);
/// * pom.xml-only Java → japicmp needs explicit old/new jars — unknown;
/// * SwiftPM/Xcode → swift-api-digester: baseline dump from a detached
///   worktree at `base`, current dump from the working tree, then
///   `-diagnose-sdk` (macOS toolchain only; elsewhere honestly unknown).
///
/// A missing tool is `unknown`, never a fake pass (F13).
pub fn api_compat_check(repo: &Path, base: &str) -> CompatReport {
    match crate::project::detect(repo) {
        crate::project::ProjectKind::Rust => run_compat("cargo", repo, base),
        crate::project::ProjectKind::Gradle => gradle_api_check_with("./gradlew", repo),
        crate::project::ProjectKind::SwiftPm | crate::project::ProjectKind::Xcode => {
            swift_digester_check_with("xcrun", repo, base)
        }
        crate::project::ProjectKind::Unknown => {
            if repo.join("pom.xml").is_file() {
                let start = Instant::now();
                CompatReport {
                    verdict: CompatVerdict::Unknown,
                    tool: "japicmp".into(),
                    detail:
                        "Maven/Java 项目：japicmp 需显式新旧 jar 配置（spec §3-M4 矩阵，未接线）"
                            .into(),
                    duration_ms: start.elapsed().as_millis() as u64,
                }
            } else {
                let start = Instant::now();
                CompatReport {
                    verdict: CompatVerdict::Unknown,
                    tool: "unknown".into(),
                    detail: "无法识别的构建系统；无工具即无裁决（F13）".into(),
                    duration_ms: start.elapsed().as_millis() as u64,
                }
            }
        }
    }
}

/// Swift module name from Package.swift (first product/target name).
fn swift_module_name(repo: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(repo.join("Package.swift")).ok()?;
    for marker in ["name: \"", "name:\""] {
        if let Some(start) = raw.find(marker) {
            let rest = &raw[start + marker.len()..];
            let end = rest.find('"').unwrap_or(rest.len());
            let name = rest[..end].trim().to_string();
            if !name.is_empty() {
                return Some(name);
            }
        }
    }
    None
}

/// swift-api-digester (spec §3-M4 matrix, 0.5-5): dump the SDK surface of
/// `base` (detached worktree) and of the working tree, then diagnose the
/// diff. macOS toolchain only — elsewhere honestly `unknown` (F13).
pub fn swift_digester_check_with(xcrun_bin: &str, repo: &Path, base: &str) -> CompatReport {
    let start = Instant::now();
    if !cfg!(target_os = "macos") {
        return CompatReport {
            verdict: CompatVerdict::Unknown,
            tool: "swift-api-digester".into(),
            detail: "swift-api-digester 需要 macOS 工具链；本平台无裁决（F13）".into(),
            duration_ms: start.elapsed().as_millis() as u64,
        };
    }
    let Some(module) = swift_module_name(repo) else {
        return CompatReport {
            verdict: CompatVerdict::Unknown,
            tool: "swift-api-digester".into(),
            detail: "Package.swift 未提供模块名（digester 需要 -module）".into(),
            duration_ms: start.elapsed().as_millis() as u64,
        };
    };
    let tmp = match tempfile::tempdir() {
        Ok(t) => t,
        Err(e) => {
            return CompatReport {
                verdict: CompatVerdict::Unknown,
                tool: "swift-api-digester".into(),
                detail: format!("无法创建临时目录：{e}"),
                duration_ms: start.elapsed().as_millis() as u64,
            };
        }
    };
    let baseline_dir = tmp.path().join("baseline");
    // Detached worktree at `base` for the baseline dump.
    let wt = std::process::Command::new("git")
        .args(["worktree", "add", "--detach"])
        .arg(&baseline_dir)
        .arg(base)
        .current_dir(repo)
        .output();
    if !matches!(wt.as_ref(), Ok(o) if o.status.success()) {
        return CompatReport {
            verdict: CompatVerdict::Unknown,
            tool: "swift-api-digester".into(),
            detail: format!(
                "基线 worktree 创建失败（git worktree add --detach {base}）：{}",
                wt.as_ref()
                    .map(|o| String::from_utf8_lossy(&o.stderr).trim().to_string())
                    .unwrap_or_default()
            ),
            duration_ms: start.elapsed().as_millis() as u64,
        };
    }
    let baseline_json = tmp.path().join("baseline.json");
    let current_json = tmp.path().join("current.json");
    for (dir, out_path) in [
        (&baseline_dir, &baseline_json),
        (&repo.to_path_buf(), &current_json),
    ] {
        let out = std::process::Command::new(xcrun_bin)
            .args(["swift-api-digester", "-dump-sdk", "-module", &module, "-o"])
            .arg(out_path)
            .current_dir(dir)
            .output();
        let ok = out
            .as_ref()
            .map(|o| o.status.success() && out_path.exists())
            .unwrap_or(false);
        if !ok {
            let _ = std::process::Command::new("git")
                .args(["worktree", "remove", "--force"])
                .arg(&baseline_dir)
                .current_dir(repo)
                .output();
            return CompatReport {
                verdict: CompatVerdict::Unknown,
                tool: "swift-api-digester".into(),
                detail: format!(
                    "SDK dump 失败（{}）：{}",
                    dir.display(),
                    out.as_ref()
                        .map(|o| String::from_utf8_lossy(&o.stderr).trim().to_string())
                        .unwrap_or_default()
                ),
                duration_ms: start.elapsed().as_millis() as u64,
            };
        }
    }
    let _ = std::process::Command::new("git")
        .args(["worktree", "remove", "--force"])
        .arg(&baseline_dir)
        .current_dir(repo)
        .output();
    let out = std::process::Command::new(xcrun_bin)
        .args(["swift-api-digester", "-diagnose-sdk", "-input-paths"])
        .arg(&baseline_json)
        .arg("-input-paths")
        .arg(&current_json)
        .current_dir(repo)
        .output();
    match out {
        Ok(o) if o.status.success() => CompatReport {
            verdict: CompatVerdict::Pass,
            tool: "swift-api-digester".into(),
            detail: format!(
                "模块 {module} 的 SDK 接口 base..HEAD 兼容：{}",
                String::from_utf8_lossy(&o.stdout).lines().count()
            ),
            duration_ms: start.elapsed().as_millis() as u64,
        },
        Ok(o) => CompatReport {
            verdict: CompatVerdict::Fail,
            tool: "swift-api-digester".into(),
            detail: format!(
                "模块 {module} 的 SDK 接口发生破坏性变更：{}",
                String::from_utf8_lossy(&o.stderr).trim()
            ),
            duration_ms: start.elapsed().as_millis() as u64,
        },
        Err(e) => CompatReport {
            verdict: CompatVerdict::Unknown,
            tool: "swift-api-digester".into(),
            detail: format!("swift-api-digester 不可用：{e}"),
            duration_ms: start.elapsed().as_millis() as u64,
        },
    }
}

/// Gradle binary-compatibility-validator: `./gradlew apiCheck` compares the
/// public ABI against the checked-in `api/*.api` dumps. The task missing
/// means the validator is not wired — `unknown`, never a fake fail.
pub fn gradle_api_check_with(gradlew_bin: &str, repo: &Path) -> CompatReport {
    let start = Instant::now();
    if !repo.join("gradlew").exists() && gradlew_bin == "./gradlew" {
        return CompatReport {
            verdict: CompatVerdict::Unknown,
            tool: "binary-compatibility-validator".into(),
            detail: "未检测到 gradlew；validator 需要 Gradle 项目（spec §3-M4 矩阵）".into(),
            duration_ms: start.elapsed().as_millis() as u64,
        };
    }
    let mut cmd = Command::new(gradlew_bin);
    cmd.arg("apiCheck").current_dir(repo);
    let out = match run(&mut cmd, Duration::from_secs(600)) {
        Ok(o) => o,
        Err(e) => {
            let unknown = e.contains("spawn failed") || e == "timeout";
            return CompatReport {
                verdict: if unknown {
                    CompatVerdict::Unknown
                } else {
                    CompatVerdict::Fail
                },
                tool: "binary-compatibility-validator".into(),
                detail: format!("gradlew apiCheck {e}"),
                duration_ms: start.elapsed().as_millis() as u64,
            };
        }
    };
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let detail = combined
        .lines()
        .rev()
        .take(6)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
    // The apiCheck task only exists when the validator plugin is applied —
    // its absence is a wiring gap, not an API break.
    let task_missing = combined.contains("Task 'apiCheck' not found")
        || combined.contains("Unknown task")
        || combined.contains("no such task");
    if task_missing {
        return CompatReport {
            verdict: CompatVerdict::Unknown,
            tool: "binary-compatibility-validator".into(),
            detail: format!("validator 未接线（apiCheck 任务不存在）：{detail}"),
            duration_ms: start.elapsed().as_millis() as u64,
        };
    }
    if out.status.success() {
        CompatReport {
            verdict: CompatVerdict::Pass,
            tool: "binary-compatibility-validator".into(),
            detail,
            duration_ms: start.elapsed().as_millis() as u64,
        }
    } else {
        CompatReport {
            verdict: CompatVerdict::Fail,
            tool: "binary-compatibility-validator".into(),
            detail,
            duration_ms: start.elapsed().as_millis() as u64,
        }
    }
}

/// The actual runner, with the cargo binary injectable (tests pass a shim
/// path; no environment mutation, parallel-test safe).
pub fn run_compat(cargo_bin: &str, repo: &Path, base: &str) -> CompatReport {
    let start = Instant::now();
    if !repo.join("Cargo.toml").exists() {
        return CompatReport {
            verdict: CompatVerdict::Unknown,
            tool: "cargo-semver-checks".into(),
            detail: "非 Rust 仓库（无 Cargo.toml）；其他语言工具未接线（spec §3-M4 矩阵）".into(),
            duration_ms: start.elapsed().as_millis() as u64,
        };
    }
    let mut cmd = Command::new(cargo_bin);
    cmd.args(["semver-checks", "check-release", "--baseline-rev", base])
        .current_dir(repo);
    let out = match run(&mut cmd, Duration::from_secs(300)) {
        Ok(o) => o,
        Err(e) => {
            let unknown = e.contains("spawn failed") || e == "timeout";
            return CompatReport {
                verdict: if unknown {
                    CompatVerdict::Unknown
                } else {
                    CompatVerdict::Fail
                },
                tool: "cargo-semver-checks".into(),
                detail: format!(
                    "cargo-semver-checks {e}（工具不可用/超时 → unknown，仅 CI 外环可裁决）"
                ),
                duration_ms: start.elapsed().as_millis() as u64,
            };
        }
    };
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let detail = combined
        .lines()
        .rev()
        .take(6)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
    // A missing tool is `unknown` (F13 spirit), never a fake fail.
    let tool_missing = combined.contains("no such command")
        || combined.contains("could not find")
        || combined.contains("not installed");
    if tool_missing {
        return CompatReport {
            verdict: CompatVerdict::Unknown,
            tool: "cargo-semver-checks".into(),
            detail: format!("工具不可用：{detail}"),
            duration_ms: start.elapsed().as_millis() as u64,
        };
    }
    if out.status.success() {
        CompatReport {
            verdict: CompatVerdict::Pass,
            tool: "cargo-semver-checks".into(),
            detail,
            duration_ms: start.elapsed().as_millis() as u64,
        }
    } else {
        CompatReport {
            verdict: CompatVerdict::Fail,
            tool: "cargo-semver-checks".into(),
            detail,
            duration_ms: start.elapsed().as_millis() as u64,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rust_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"x\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        dir
    }

    fn shim(script: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("cargo");
        std::fs::write(&bin, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        dir
    }

    #[test]
    fn public_wrapper_delegates_with_unknown_for_non_rust() {
        // Covers the api_compat_check wrapper path (non-Rust repo).
        let dir = tempfile::tempdir().unwrap();
        let r = api_compat_check(dir.path(), "HEAD^");
        assert_eq!(r.verdict, CompatVerdict::Unknown);
        assert!(
            r.detail.contains("无法识别") || r.detail.contains("非 Rust"),
            "honest unknown expected: {}",
            r.detail
        );
    }

    #[test]
    fn missing_cargo_toml_is_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let r = api_compat_check(dir.path(), "HEAD^");
        assert_eq!(r.verdict, CompatVerdict::Unknown);
    }

    #[test]
    fn missing_tool_is_unknown() {
        let repo = rust_repo();
        let r = run_compat("/nonexistent/cargo", repo.path(), "HEAD^");
        assert_eq!(
            r.verdict,
            CompatVerdict::Unknown,
            "no tool ⇒ unknown: {r:?}"
        );
    }

    #[test]
    fn shim_pass_and_fail_map_to_verdicts() {
        let repo = rust_repo();
        let pass = shim("#!/bin/sh\nexit 0\n");
        let r = run_compat(
            pass.path().join("cargo").to_str().unwrap(),
            repo.path(),
            "HEAD^",
        );
        assert_eq!(r.verdict, CompatVerdict::Pass, "{}", r.detail);

        let fail = shim("#!/bin/sh\necho incompatible >&2\nexit 1\n");
        let r = run_compat(
            fail.path().join("cargo").to_str().unwrap(),
            repo.path(),
            "HEAD^",
        );
        assert_eq!(r.verdict, CompatVerdict::Fail);
        assert!(r.detail.contains("incompatible"), "detail: {}", r.detail);
    }

    #[test]
    fn gradle_api_check_maps_pass_missing_task_and_fail() {
        let repo = rust_repo();
        let pass = shim("#!/bin/sh\nexit 0\n");
        let r = gradle_api_check_with(pass.path().join("cargo").to_str().unwrap(), repo.path());
        assert_eq!(r.verdict, CompatVerdict::Pass, "{}", r.detail);

        let missing = shim("#!/bin/sh\necho \"Task 'apiCheck' not found\" >&2\nexit 1\n");
        let r = gradle_api_check_with(missing.path().join("cargo").to_str().unwrap(), repo.path());
        assert_eq!(
            r.verdict,
            CompatVerdict::Unknown,
            "task missing is a wiring gap, not a break: {}",
            r.detail
        );

        let broke = shim("#!/bin/sh\necho \"API check failed for :app\" >&2\nexit 1\n");
        let r = gradle_api_check_with(broke.path().join("cargo").to_str().unwrap(), repo.path());
        assert_eq!(r.verdict, CompatVerdict::Fail, "{}", r.detail);
    }

    #[test]
    fn api_compat_detects_gradle_via_project_kind() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("app");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("build.gradle.kts"), "// android module\n").unwrap();
        std::fs::write(repo.join("gradlew"), "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(repo.join("gradlew"), std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }
        let r = api_compat_check(&repo, "HEAD^");
        assert_eq!(r.tool, "binary-compatibility-validator");
        assert_eq!(r.verdict, CompatVerdict::Pass, "{}", r.detail);
    }

    #[test]
    fn swift_module_name_parses_package_swift() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Package.swift"),
            "import PackageDescription\nlet package = Package(name: \"CalcKit\", targets: [.target(name: \"CalcKit\")])\n",
        )
        .unwrap();
        assert_eq!(swift_module_name(dir.path()).as_deref(), Some("CalcKit"));
        std::fs::write(dir.path().join("Package.swift"), "// nothing").unwrap();
        assert_eq!(swift_module_name(dir.path()), None);
    }

    #[test]
    fn gradle_without_gradlew_is_unknown() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("build.gradle.kts"), "// no wrapper\n").unwrap();
        let r = gradle_api_check_with("./gradlew", dir.path());
        assert_eq!(r.verdict, CompatVerdict::Unknown);
        assert!(r.detail.contains("gradlew"), "{}", r.detail);
    }

    #[test]
    fn maven_without_jars_is_unknown() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("pom.xml"), "<project/>").unwrap();
        let r = api_compat_check(dir.path(), "HEAD^");
        assert_eq!(r.verdict, CompatVerdict::Unknown);
        assert!(r.detail.contains("japicmp"), "{}", r.detail);
    }

    #[test]
    fn large_tool_output_is_not_a_timeout() {
        // A tool emitting ~1MiB (far beyond the OS pipe buffer) and exiting
        // 0 must be a pass, not a bogus timeout: the reader threads drain
        // both pipes from spawn time.
        let repo = rust_repo();
        let loud =
            shim("#!/bin/sh\nyes '0123456789012345678901234567890123456789' | head -c 1048576\n");
        let r = run_compat(
            loud.path().join("cargo").to_str().unwrap(),
            repo.path(),
            "HEAD^",
        );
        assert_eq!(r.verdict, CompatVerdict::Pass, "{}", r.detail);
    }
}

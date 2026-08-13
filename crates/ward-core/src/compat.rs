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

fn read_all(child: std::process::Child) -> (String, String) {
    let mut child = child;
    use std::io::Read;
    let mut out = String::new();
    let mut err = String::new();
    if let Some(mut o) = child.stdout.take() {
        let _ = o.read_to_string(&mut out);
    }
    if let Some(mut e) = child.stderr.take() {
        let _ = e.read_to_string(&mut err);
    }
    (out, err)
}

fn run(cmd: &mut Command, timeout: Duration) -> Result<std::process::Output, String> {
    let start = Instant::now();
    let mut child = match cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return Err(format!("spawn failed: {e}")),
    };
    let deadline = start + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // Drain pipes BEFORE assembling: on some platforms
                // `wait_with_output` after an observed exit loses data.
                let (out, err) = read_all(child);
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
                    return Err("timeout".into());
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(format!("wait failed: {e}")),
        }
    }
}

/// Run the API compatibility check for the repository's language.
///
/// Rust: `cargo semver-checks check-release --baseline-rev <base>`.
/// A missing tool or a missing Cargo.toml is `unknown`, never a fake pass.
pub fn api_compat_check(repo: &Path, base: &str) -> CompatReport {
    run_compat("cargo", repo, base)
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
}

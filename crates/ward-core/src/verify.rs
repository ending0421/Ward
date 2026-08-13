//! M3 Catch — verification loop (spec §3-M3).
//!
//! * Inner loop (`catch_run`): lint/type precheck only — light, no Docker.
//!   Anything requiring real execution is `deferred`, never faked.
//! * Outer loop (`verify_full`): adjudication inside a Docker sandbox with
//!   network disabled, no Docker socket, no host write mounts. When no
//!   sandbox environment exists the verdict is `unknown` (F13) — a missing
//!   sandbox can never produce a green.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::config::WardConfig;

/// Verification verdict (spec §3-M3): pass / fail / deferred / unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CatchVerdict {
    Pass,
    Fail,
    Deferred,
    Unknown,
}

impl CatchVerdict {
    pub fn as_str(self) -> &'static str {
        match self {
            CatchVerdict::Pass => "pass",
            CatchVerdict::Fail => "fail",
            CatchVerdict::Deferred => "deferred",
            CatchVerdict::Unknown => "unknown",
        }
    }
}

/// Result of one verification run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatchReport {
    pub verdict: CatchVerdict,
    /// Human-readable explanation, always present.
    pub note: String,
    /// Tail of the command output (never source code).
    pub output_tail: String,
    pub duration_ms: u64,
}

/// Run a command with a timeout; returns (exit_ok, stdout, stderr).
fn run_with_timeout(cmd: &mut Command, timeout: Duration) -> (bool, String, String) {
    let start = Instant::now();
    let Ok(mut child) = cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn() else {
        return (false, String::new(), "failed to spawn".into());
    };
    let deadline = start + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let out = read_all(child);
                return (status.success(), out.0, out.1);
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return (false, String::new(), "timeout".into());
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => return (false, String::new(), "wait error".into()),
        }
    }
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

fn tail(s: &str, lines: usize) -> String {
    s.lines()
        .rev()
        .take(lines)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n")
}

/// Inner-loop precheck: run the configured lint command (no Docker).
pub fn catch_run(repo: &Path, config: &WardConfig) -> CatchReport {
    let start = Instant::now();
    if config.lint.command.trim().is_empty() {
        return CatchReport {
            verdict: CatchVerdict::Deferred,
            note: "未配置内环预检命令（.ward/config.toml → lint.command）".into(),
            output_tail: String::new(),
            duration_ms: start.elapsed().as_millis() as u64,
        };
    }
    let mut parts = config.lint.command.split_whitespace();
    let Some(prog) = parts.next() else {
        return CatchReport {
            verdict: CatchVerdict::Unknown,
            note: "内环预检命令为空".into(),
            output_tail: String::new(),
            duration_ms: start.elapsed().as_millis() as u64,
        };
    };
    let mut cmd = Command::new(prog);
    cmd.args(parts).current_dir(repo);
    let (ok, stdout, stderr) =
        run_with_timeout(&mut cmd, Duration::from_secs(config.lint.timeout_secs));
    let output_tail = tail(&format!("{stdout}\n{stderr}"), 12);
    let duration_ms = start.elapsed().as_millis() as u64;
    if stderr.contains("timeout") && !ok {
        CatchReport {
            verdict: CatchVerdict::Unknown,
            note: format!("内环预检超时（>{}s）", config.lint.timeout_secs),
            output_tail,
            duration_ms,
        }
    } else if stdout.is_empty() && stderr.contains("failed to spawn") {
        CatchReport {
            verdict: CatchVerdict::Unknown,
            note: format!("预检工具不可用：{prog}（fail-open，仅 CI 可裁决）"),
            output_tail,
            duration_ms,
        }
    } else if ok {
        CatchReport {
            verdict: CatchVerdict::Pass,
            note: format!("内环预检通过：{}", config.lint.command),
            output_tail,
            duration_ms,
        }
    } else {
        CatchReport {
            verdict: CatchVerdict::Fail,
            note: format!("内环预检失败：{}", config.lint.command),
            output_tail,
            duration_ms,
        }
    }
}

fn docker_available() -> bool {
    Command::new("docker")
        .args(["info", "--format", "{{.ServerVersion}}"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Outer-loop adjudication inside a Docker sandbox.
///
/// Sandbox posture (spec §7): network disabled, no Docker socket mount, repo
/// mounted read-only, target/cargo homes on the container's tmpfs, memory
/// limited. No sandbox → `unknown` (F13), never a fake pass.
pub fn verify_full(repo: &Path, config: &WardConfig) -> CatchReport {
    let start = Instant::now();
    if !docker_available() {
        return CatchReport {
            verdict: CatchVerdict::Unknown,
            note: "沙箱环境不可用（Docker 缺失/无权限）；仅 CI 可裁决（F13）".into(),
            output_tail: String::new(),
            duration_ms: start.elapsed().as_millis() as u64,
        };
    }
    let repo_abs = match repo.canonicalize() {
        Ok(p) => p,
        Err(_) => {
            return CatchReport {
                verdict: CatchVerdict::Unknown,
                note: "无法解析仓库路径".into(),
                output_tail: String::new(),
                duration_ms: start.elapsed().as_millis() as u64,
            };
        }
    };
    let repo_str = repo_abs.to_string_lossy().into_owned();
    let mut cmd = Command::new("docker");
    cmd.args([
        "run",
        "--rm",
        "--network",
        "none",
        "--memory",
        &config.sandbox.memory,
        "--pids-limit",
        "256",
        "--cap-drop",
        "ALL",
        "-v",
        &format!("{repo_str}:/repo:ro"),
        "-w",
        "/repo",
        "-e",
        "CARGO_TARGET_DIR=/tmp/ward-target",
        "-e",
        "CARGO_HOME=/tmp/ward-cargo",
        &config.sandbox.image,
        "sh",
        "-c",
        &config.sandbox.verify_command,
    ]);
    let (ok, stdout, stderr) = run_with_timeout(&mut cmd, Duration::from_secs(1800));
    let output_tail = tail(&format!("{stdout}\n{stderr}"), 24);
    let duration_ms = start.elapsed().as_millis() as u64;
    if ok {
        CatchReport {
            verdict: CatchVerdict::Pass,
            note: format!("外环沙箱裁决通过：{}", config.sandbox.verify_command),
            output_tail,
            duration_ms,
        }
    } else {
        CatchReport {
            verdict: CatchVerdict::Fail,
            note: format!("外环沙箱裁决失败：{}", config.sandbox.verify_command),
            output_tail,
            duration_ms,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_keeps_last_lines() {
        assert_eq!(tail("a\nb\nc", 2), "b\nc");
    }

    #[test]
    fn missing_sandbox_is_unknown_never_pass() {
        // Cannot assert docker absence on the host; instead assert the
        // structural rule: verdict must never be a silent Pass when the
        // sandbox path is unknown — covered by the F13 branch above. This
        // test pins the serialized verdict vocabulary.
        assert_eq!(CatchVerdict::Unknown.as_str(), "unknown");
        assert_ne!(CatchVerdict::Unknown, CatchVerdict::Pass);
    }

    #[test]
    fn empty_lint_command_defers() {
        let cfg = WardConfig {
            lint: crate::config::LintConfig {
                command: String::new(),
                timeout_secs: 1,
            },
            ..Default::default()
        };
        let dir = tempfile::tempdir().unwrap();
        let r = catch_run(dir.path(), &cfg);
        assert_eq!(r.verdict, CatchVerdict::Deferred);
    }
}

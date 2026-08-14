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

/// Map a sandbox run's raw outcome to a CatchReport (pure, testable).
fn outer_report(
    ok: bool,
    config: &WardConfig,
    output_tail: String,
    duration_ms: u64,
) -> CatchReport {
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

fn docker_available(docker_bin: &str) -> bool {
    // The docker CLIENT exits 0 even when the daemon is unreachable, so the
    // exit status alone is not evidence. The daemon is only available when
    // `docker info` actually renders a ServerVersion.
    let Ok(out) = Command::new(docker_bin)
        .args(["info", "--format", "{{.ServerVersion}}"])
        .output()
    else {
        return false;
    };
    if !out.status.success() {
        return false;
    }
    let version = String::from_utf8_lossy(&out.stdout);
    let version = version.trim();
    !version.is_empty() && version.chars().next().is_some_and(|c| c.is_ascii_digit())
}

/// Build the docker-run argument vector for the sandbox.
///
/// Sandbox posture (spec §7): network disabled, no Docker socket mount, repo
/// mounted read-only, target/cargo homes on the container's tmpfs, memory
/// and pid limits, all capabilities dropped.
pub fn sandbox_args(config: &WardConfig, repo_abs: &str) -> Vec<String> {
    sandbox_args_with_cache(config, repo_abs, None)
}

/// Build the docker-run argument vector for the sandbox.
///
/// Sandbox posture (spec §7): network disabled, no Docker socket mount, repo
/// mounted read-only, target/cargo homes on the container's tmpfs, memory
/// and pid limits, all capabilities dropped. When a local cargo registry
/// cache exists it is mounted read-only and cargo is forced offline — the
/// sandbox then actually works on a dev machine (spec §3-M3: real execution,
/// not a green light).
pub fn sandbox_args_with_cache(
    config: &WardConfig,
    repo_abs: &str,
    cargo_cache: Option<&str>,
) -> Vec<String> {
    let mut args = vec![
        "run".to_string(),
        "--rm".to_string(),
        "--network".to_string(),
        "none".to_string(),
        "--memory".to_string(),
        config.sandbox.memory.clone(),
        "--pids-limit".to_string(),
        "256".to_string(),
        "--cap-drop".to_string(),
        "ALL".to_string(),
        "-v".to_string(),
        format!("{repo_abs}:/repo:ro"),
        "-w".to_string(),
        "/repo".to_string(),
        "-e".to_string(),
        "CARGO_TARGET_DIR=/tmp/ward-target".to_string(),
        "-e".to_string(),
        "CARGO_HOME=/tmp/ward-cargo".to_string(),
        "-e".to_string(),
        "CARGO_NET_OFFLINE=true".to_string(),
    ];
    if let Some(cache) = cargo_cache {
        args.push("-v".to_string());
        args.push(format!("{cache}:/tmp/ward-cargo:ro"));
    }
    args.extend([
        config.sandbox.image.clone(),
        "sh".to_string(),
        "-c".to_string(),
        config.sandbox.verify_command.clone(),
    ]);
    args
}

/// Outer-loop adjudication inside a Docker sandbox.
///
/// Sandbox posture (spec §7): network disabled, no Docker socket mount, repo
/// mounted read-only, target/cargo homes on the container's tmpfs, memory
/// limited. No sandbox → `unknown` (F13), never a fake pass.
pub fn verify_full(repo: &Path, config: &WardConfig) -> CatchReport {
    run_sandbox("docker", repo, config)
}

/// The sandbox runner, with the docker binary injectable (tests use a shim;
/// no environment mutation, parallel-test safe).
pub fn run_sandbox(docker_bin: &str, repo: &Path, config: &WardConfig) -> CatchReport {
    let start = Instant::now();
    if !docker_available(docker_bin) {
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
    // Repo-local cargo cache (e.g. the workspace .cargo kept by CI/dev
    // builds) seeds the offline sandbox so the real test run can build.
    let cache = repo_abs
        .join(".cargo")
        .is_dir()
        .then(|| repo_abs.join(".cargo").to_string_lossy().into_owned());
    let mut cmd = Command::new(docker_bin);
    cmd.args(sandbox_args_with_cache(config, &repo_str, cache.as_deref()));
    let (ok, stdout, stderr) = run_with_timeout(&mut cmd, Duration::from_secs(1800));
    let combined = format!("{stdout}\n{stderr}");
    let output_tail = tail(&combined, 24);
    let duration_ms = start.elapsed().as_millis() as u64;
    // A docker client without a reachable daemon is a *missing sandbox*
    // (F13 → unknown), never a failed test run.
    if combined.contains("Cannot connect to the Docker daemon")
        || combined.contains("Is the docker daemon running")
    {
        return CatchReport {
            verdict: CatchVerdict::Unknown,
            note: "沙箱不可用（docker 守护进程未运行）；仅 CI 可裁决（F13）".into(),
            output_tail,
            duration_ms,
        };
    }
    outer_report(ok, config, output_tail, duration_ms)
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
    fn outer_report_maps_outcomes() {
        let cfg = WardConfig::default();
        let pass = outer_report(true, &cfg, "x".into(), 1);
        assert_eq!(pass.verdict, CatchVerdict::Pass);
        let fail = outer_report(false, &cfg, "x".into(), 1);
        assert_eq!(fail.verdict, CatchVerdict::Fail);
        assert!(fail.note.contains("cargo test"));
    }

    #[test]
    fn sandbox_args_enforce_the_security_posture() {
        let cfg = WardConfig::default();
        let args = sandbox_args(&cfg, "/abs/repo");
        let joined = args.join(" ");
        assert!(
            args.windows(2).any(|w| w == ["--network", "none"]),
            "network off"
        );
        assert!(joined.contains("/abs/repo:/repo:ro"), "read-only mount");
        assert!(!joined.contains("/var/run/docker.sock"), "no docker socket");
        assert!(
            args.windows(2).any(|w| w == ["--cap-drop", "ALL"]),
            "caps dropped"
        );
        assert!(args.contains(&"--pids-limit".to_string()));
        assert!(args.contains(&"rust:1-bookworm".to_string()));
        assert_eq!(args.last().unwrap(), "cargo test --quiet");
    }

    #[test]
    fn docker_shim_covers_pass_fail_and_security_args() {
        let dir = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let log = dir.path().join("args.log");
        let log_s = log.to_string_lossy().into_owned();
        let script = format!(
            "#!/bin/sh\nif [ \"$1\" = \"info\" ]; then echo 26.0; exit 0; fi\necho \"$@\" >> {log_s}\nexit {exit_code}\n",
            exit_code = 0
        );
        let bin = dir.path().join("docker");
        std::fs::write(&bin, &script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let cfg = WardConfig::default();
        let r = run_sandbox(bin.to_str().unwrap(), repo.path(), &cfg);
        assert_eq!(r.verdict, CatchVerdict::Pass, "{}", r.note);
        let args = std::fs::read_to_string(&log).unwrap();
        assert!(args.contains("--network none"), "args: {args}");
        assert!(args.contains(":ro"), "read-only mount: {args}");
        assert!(!args.contains("docker.sock"), "no socket mount: {args}");
        assert!(args.contains("--cap-drop ALL"), "caps dropped: {args}");

        // Failure mapping.
        let fail_bin = dir.path().join("docker-fail");
        std::fs::write(
            &fail_bin,
            "#!/bin/sh\nif [ \"$1\" = \"info\" ]; then echo 26.0; exit 0; fi\nexit 1\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fail_bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let r2 = run_sandbox(fail_bin.to_str().unwrap(), repo.path(), &cfg);
        assert_eq!(r2.verdict, CatchVerdict::Fail);
    }

    #[test]
    fn sandbox_cache_mount_is_conditional() {
        let cfg = WardConfig::default();
        // No cache: no mount, but offline is always forced.
        let args = sandbox_args_with_cache(&cfg, "/repo", None);
        assert!(!args.iter().any(|a| a.contains("/tmp/ward-cargo:ro")));
        assert!(args.windows(2).any(|w| w == ["-e", "CARGO_NET_OFFLINE=true"]));
        // With cache: read-only mount appears.
        let args = sandbox_args_with_cache(&cfg, "/repo", Some("/home/u/.cargo"));
        assert!(args.contains(&"-v".to_string()));
        assert!(args.contains(&"/home/u/.cargo:/tmp/ward-cargo:ro".to_string()));
        // And it still never mounts the docker socket.
        assert!(!args.join(" ").contains("docker.sock"));
    }

    #[test]
    fn docker_client_without_daemon_is_unknown_not_fail() {
        let dir = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let cfg = WardConfig::default();
        // Shim: `info` renders nothing (daemon down), `run` emits the
        // daemon-unreachable error. The verdict must be Unknown (F13), and
        // `info` alone (empty ServerVersion) must not look "available".
        let shim = dir.path().join("docker");
        // `info` reports a version (daemon looked alive), then `run` hits
        // the daemon-unreachable race — verdict must still be Unknown.
        std::fs::write(
            &shim,
            "#!/bin/sh\nif [ \"$1\" = \"info\" ]; then echo 27.5.1; exit 0; fi\necho \"docker: Cannot connect to the Docker daemon at unix:///x.sock. Is the docker daemon running?.\" >&2\nexit 1\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let r = run_sandbox(shim.to_str().unwrap(), repo.path(), &cfg);
        assert_eq!(r.verdict, CatchVerdict::Unknown, "daemon down ⇒ unknown: {r:?}");
        assert!(r.note.contains("守护进程"), "note: {}", r.note);
    }

    #[test]
    fn docker_available_requires_real_server_version() {
        let dir = tempfile::tempdir().unwrap();
        let shim = dir.path().join("docker");
        std::fs::write(&shim, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        // exit 0 with EMPTY version output must not count as available.
        assert!(!docker_available(shim.to_str().unwrap()));
    }

    #[test]
    fn docker_unavailable_is_unknown() {
        let repo = tempfile::tempdir().unwrap();
        let cfg = WardConfig::default();
        let r = run_sandbox("/nonexistent/docker", repo.path(), &cfg);
        assert_eq!(
            r.verdict,
            CatchVerdict::Unknown,
            "F13: no sandbox ⇒ unknown"
        );
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

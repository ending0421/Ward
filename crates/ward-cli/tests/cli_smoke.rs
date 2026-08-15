//! CLI smoke tests: run the real `ward` binary against a temp repository.

use std::process::Command;

fn repo_with_rust() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(args)
            .output()
            .expect("git");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "-b", "master"]);
    git(&["config", "user.name", "t"]);
    git(&["config", "user.email", "t@e.c"]);
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("src/lib.rs"),
        "pub fn debounce() { let a = 1; let b = a + 1; let c = b + 1; }\n",
    )
    .unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "c1"]);
    dir
}

fn ward(args: &[&str], cwd: &std::path::Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_ward"))
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("ward runs")
}

#[test]
fn init_index_spot_roundtrip() {
    let repo = repo_with_rust();
    let out = ward(&["init", "--repo", "."], repo.path());
    assert!(out.status.success());
    assert!(repo.path().join(".ward/config.toml").exists());

    let out = ward(&["index", "--repo", "."], repo.path());
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("1 files"), "index report: {stdout}");

    let out = ward(
        &[
            "spot",
            "--repo",
            ".",
            "--intent",
            "防抖",
            "--signature",
            "pub fn debounce() -> u8",
            "--json",
        ],
        repo.path(),
    );
    assert!(out.status.success());
    let json: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    assert!(json["matches"].is_array());
    assert_eq!(json["stale"], false);
    let id = json["advisory_id"].as_str().unwrap().to_string();

    // feedback loop: self-reported action roundtrip
    let out = ward(&["action", "--repo", ".", &id, "accepted"], repo.path());
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // invalid action must be rejected (argument validation)
    let out = ward(&["action", "--repo", ".", &id, "bogus"], repo.path());
    assert!(!out.status.success());
}

#[test]
fn form_check_evaluates_spec() {
    let repo = repo_with_rust();
    std::fs::create_dir_all(repo.path().join("specs")).unwrap();
    std::fs::write(
        repo.path().join("specs/task.md"),
        "```yaml\nassertions:\n  - kind: no_new_dependency\n  - kind: must_pass\n```\n",
    )
    .unwrap();
    let out = ward(
        &["form-check", "--repo", ".", "--spec", "specs/task.md"],
        repo.path(),
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("[pass] no_new_dependency"), "{stdout}");
    assert!(stdout.contains("[deferred] must_pass"), "{stdout}");
}

#[test]
fn card_clusters_replay_and_intent_roundtrip() {
    let repo = repo_with_rust();
    let out = ward(&["index", "--repo", "."], repo.path());
    assert!(out.status.success());

    let out = ward(&["card", "--repo", ".", "debounce"], repo.path());
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("debounce (function_item)"),
        "card: {stdout}"
    );
    assert!(stdout.contains("callers"), "card callers section");

    let out = ward(&["clusters", "--repo", ".", "--json"], repo.path());
    assert!(out.status.success());
    let json: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).unwrap();
    assert!(json["clusters"].is_array());
    assert_eq!(json["truncated"], false);

    // Replay needs two commits.
    std::fs::write(
        repo.path().join("src/lib.rs"),
        "pub fn debounce() { let x = 1; let y = x + 1; let z = y + 1; }\n",
    )
    .unwrap();
    let git = std::process::Command::new("git")
        .arg("-C")
        .arg(repo.path())
        .args(["add", "-A"])
        .output()
        .unwrap();
    assert!(git.status.success());
    let git = std::process::Command::new("git")
        .arg("-C")
        .arg(repo.path())
        .args(["commit", "-q", "-m", "c2"])
        .output()
        .unwrap();
    assert!(git.status.success());
    let out = ward(&["index", "--repo", "."], repo.path());
    assert!(out.status.success());
    let out = ward(&["replay", "HEAD^", "HEAD", "--repo", "."], repo.path());
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("Replay"));

    // intent-check without a provider: honest "not executed" JSON.
    let out = ward(
        &[
            "intent-check",
            "--repo",
            ".",
            "--requirement",
            "实现防抖",
            "--json",
        ],
        repo.path(),
    );
    assert!(out.status.success());
    let json: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).unwrap();
    assert_eq!(json["executed"], false);
    assert_eq!(json["partition"], "llm_soft");
}

#[test]
fn form_check_ci_posture_is_fail_closed() {
    let repo = repo_with_rust();
    std::fs::create_dir_all(repo.path().join("specs")).unwrap();
    std::fs::write(
        repo.path().join("specs/task.md"),
        "```yaml\nassertions:\n  - kind: no_new_dependency\n```\n",
    )
    .unwrap();
    // Add a dependency change → no_new_dependency fails.
    std::fs::write(repo.path().join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
    let git = std::process::Command::new("git")
        .arg("-C")
        .arg(repo.path())
        .args(["add", "-A"])
        .output()
        .unwrap();
    assert!(git.status.success());
    let git = std::process::Command::new("git")
        .arg("-C")
        .arg(repo.path())
        .args(["commit", "-q", "-m", "c2"])
        .output()
        .unwrap();
    assert!(git.status.success());
    let out = ward(
        &[
            "form-check",
            "--repo",
            ".",
            "--spec",
            "specs/task.md",
            "--ci",
        ],
        repo.path(),
    );
    assert_eq!(
        out.status.code(),
        Some(1),
        "--ci must exit 1 on fail: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Advisory mode (no --ci) must not exit nonzero.
    let out = ward(
        &["form-check", "--repo", ".", "--spec", "specs/task.md"],
        repo.path(),
    );
    assert!(out.status.success(), "advisory mode stays fail-open");
}

#[test]
fn catch_run_reports_pass_for_true_command() {
    let repo = repo_with_rust();
    // Override the lint command to something deterministic.
    std::fs::create_dir_all(repo.path().join(".ward")).unwrap();
    std::fs::write(
        repo.path().join(".ward/config.toml"),
        "[lint]\ncommand = \"true\"\ntimeout_secs = 5\n",
    )
    .unwrap();
    let out = ward(&["catch-run", "--repo", "."], repo.path());
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("pass"));
}

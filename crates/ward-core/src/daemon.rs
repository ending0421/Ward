//! The unattended background worker (无感模式): watch → index, poll HEAD →
//! infer, daily snapshot, weekly jscpd/clusters/calibration. Every tick is a
//! pure function so the daemon loop stays thin and everything is testable
//! without long-running processes.

use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::config::WardConfig;
use crate::store::Store;
use crate::{index, infer, stats};

/// What one weekly unattended pass produced.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeeklyReport {
    /// jscpd one-line summary (independent duplication metric), None when
    /// jscpd is unavailable (fail-open).
    pub jscpd: Option<String>,
    pub clusters: i64,
    pub labels: i64,
    /// Calibration note (sample-size honest).
    pub calibration_note: String,
}

/// Incremental index tick (file watcher trigger).
pub fn run_index_tick(repo: &Path, config: &WardConfig) -> Result<index::IndexReport> {
    index::index_repo(repo, config)
}

/// Inference tick (HEAD poll trigger).
pub fn run_infer_tick(repo: &Path, config: &WardConfig) -> Result<infer::InferReport> {
    let store = Store::open(&Store::default_path(repo))?;
    infer::infer_pending(repo, &store, config)
}

/// Daily tick: idempotent snapshot.
pub fn run_daily_tick(repo: &Path, config: &WardConfig) -> Result<crate::store::Snapshot> {
    let store = Store::open(&Store::default_path(repo))?;
    stats::snapshot_now(repo, &store, config)
}

/// Weekly tick: independent duplication metric + clusters + calibration note,
/// appended to the unattended metrics log.
pub fn run_weekly_tick(repo: &Path, config: &WardConfig) -> Result<WeeklyReport> {
    let store = Store::open(&Store::default_path(repo))?;
    let jscpd = jscpd_summary(repo);
    let clusters = crate::cluster::cluster_duplicates_with(
        &store,
        config.thresholds.strong,
        config.clusters.exclude_tests,
    )?
    .clusters
    .len() as i64;
    let labels = store.label_count()?;
    let calibration = crate::calibrate::calibrate(&store)?;
    let report = WeeklyReport {
        jscpd,
        clusters,
        labels,
        calibration_note: calibration.note.clone(),
    };
    append_metrics(
        repo,
        &serde_json::json!({
            "event": "weekly",
            "clusters": clusters,
            "labels": labels,
            "jscpd": report.jscpd,
            "calibration_note": calibration.note,
            "calibration_total": calibration.total_verdicts,
        }),
    )?;
    Ok(report)
}

/// jscpd one-line duplication summary (token-level CPD, independent metric).
fn jscpd_summary(repo: &Path) -> Option<String> {
    let out = std::process::Command::new("npx")
        .args([
            "--yes",
            "jscpd",
            "--min-lines",
            "5",
            "--reporters",
            "console",
            ".",
        ])
        .current_dir(repo)
        .output()
        .ok()?;
    if !out.status.success() {
        return None; // fail-open: no jscpd, no metric
    }
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .find(|l| l.contains("Total:"))
        .map(|l| l.trim().to_string())
}

/// Append one JSON line to `.ward/metrics.jsonl` (unattended metrics log).
pub fn append_metrics(repo: &Path, value: &serde_json::Value) -> Result<()> {
    let mut line = serde_json::to_string(value).unwrap_or_default();
    line.push('\n');
    let path = repo.join(".ward/metrics.jsonl");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    f.write_all(line.as_bytes())?;
    Ok(())
}

/// Read the metrics log (newest last).
pub fn read_metrics(repo: &Path) -> Vec<serde_json::Value> {
    let Ok(text) = std::fs::read_to_string(repo.join(".ward/metrics.jsonl")) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Label;

    fn rust_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), "pub fn f() {}\n").unwrap();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success());
        };
        git(&["init", "-b", "master"]);
        git(&["config", "user.name", "t"]);
        git(&["config", "user.email", "t@e.c"]);
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "c1"]);
        dir
    }

    #[test]
    fn index_tick_builds_the_index() {
        let dir = rust_repo();
        let report = run_index_tick(dir.path(), &WardConfig::default()).unwrap();
        assert_eq!(report.files_indexed, 1);
        assert_eq!(report.files_unchanged, 0);
        let again = run_index_tick(dir.path(), &WardConfig::default()).unwrap();
        assert_eq!(again.files_unchanged, 1, "incremental skip in daemon mode");
    }

    #[test]
    fn infer_tick_runs_on_pending_advisories() {
        let dir = rust_repo();
        let store = Store::open(&Store::default_path(dir.path())).unwrap();
        store
            .record_advisory(&crate::store::Advisory {
                id: "a".into(),
                tool: "spot".into(),
                ts: 1,
                query_hash: "q".into(),
                result_json: "[]".into(),
                ..Default::default()
            })
            .unwrap();
        let report = run_infer_tick(dir.path(), &WardConfig::default()).unwrap();
        assert_eq!(report.considered, 1, "{report:?}");
    }

    #[test]
    fn weekly_tick_appends_metrics_and_is_honest_about_samples() {
        let dir = rust_repo();
        let store = Store::open(&Store::default_path(dir.path())).unwrap();
        store
            .record_label(&Label {
                id: None,
                advisory_id: "a".into(),
                match_index: 0,
                annotator: "human".into(),
                query_hash: None,
                language: None,
                kind: Some("near".into()),
                similarity: Some(0.93),
                verdict: "y".into(),
                ts: 1,
            })
            .unwrap();
        let report = run_weekly_tick(dir.path(), &WardConfig::default()).unwrap();
        assert_eq!(report.labels, 1);
        assert!(report.calibration_note.contains("样本量不足"));
        let metrics = read_metrics(dir.path());
        assert_eq!(metrics.len(), 1);
        assert_eq!(metrics[0]["event"], "weekly");
        assert_eq!(metrics[0]["labels"], 1);
        // Second run appends (no overwrite).
        run_weekly_tick(dir.path(), &WardConfig::default()).unwrap();
        assert_eq!(read_metrics(dir.path()).len(), 2);
    }

    #[test]
    fn metrics_log_survives_missing_dir_and_bad_lines() {
        let dir = tempfile::tempdir().unwrap();
        append_metrics(dir.path(), &serde_json::json!({"event": "daily"})).unwrap();
        assert_eq!(read_metrics(dir.path()).len(), 1);
        std::fs::write(
            dir.path().join(".ward/metrics.jsonl"),
            "not-json\n{\"event\":\"ok\"}\n",
        )
        .unwrap();
        assert_eq!(read_metrics(dir.path()).len(), 1, "bad lines are skipped");
    }
}

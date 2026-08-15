//! Remote diagnostics & feedback (Ward 报告自己的问题): environment and
//! health probe, privacy-redacted bundles, and GitHub issue composition.
//!
//! Privacy discipline (§7): everything stays local by default; the bundle
//! contains no source snippets and no absolute paths unless explicitly
//! opted in; uploading anywhere is always an explicit user command.

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::WardConfig;
use crate::store::Store;
use crate::{index, search};

/// Redaction / opt-in switches.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DoctorOpts {
    pub include_snippets: bool,
    pub include_config: bool,
    pub include_paths: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreHealth {
    pub symbols: usize,
    pub languages: Vec<(String, usize)>,
    pub schema_version: i64,
    pub integrity: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerfProbe {
    pub index_secs: f64,
    pub spot_runs: usize,
    pub spot_mean_ms: f64,
    pub spot_max_ms: f64,
}

/// The doctor report — everything needed to triage a remote problem.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorReport {
    pub ward_version: String,
    pub platform: String,
    pub repo_name: String,
    /// Absolute repo path — redacted (`None`) unless `include_paths`.
    pub repo_path: Option<String>,
    pub store: StoreHealth,
    /// Safe numeric config summary; full config only with `include_config`.
    pub config_summary: serde_json::Value,
    pub perf: PerfProbe,
    pub metrics_tail: Vec<serde_json::Value>,
    /// Tail of the daemon log with repo/home prefixes redacted to `<repo>`/`~`.
    pub daemon_log_tail: Vec<String>,
    pub redacted: bool,
    pub generated_at: i64,
}

/// Language histogram from the index.
fn language_histogram(store: &Store) -> Vec<(String, usize)> {
    let Ok(symbols) = store.all_symbols() else {
        return Vec::new();
    };
    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for s in symbols {
        *counts.entry(s.language).or_default() += 1;
    }
    counts.into_iter().collect()
}

fn integrity(_store: &Store) -> String {
    "ok".to_string() // Store::open already ran quick_check; a broken store
    // would have errored before this report exists.
}

/// Run the doctor probe.
pub fn doctor(repo: &Path, opts: &DoctorOpts) -> Result<DoctorReport> {
    let cfg = WardConfig::load_or_default(&crate::config::default_path(repo)).0;
    let store = Store::open(&Store::default_path(repo))?;

    // Performance probe: time an incremental index, then 5 spot calls.
    let index_start = Instant::now();
    index::index_repo(repo, &cfg)?;
    let index_secs = index_start.elapsed().as_secs_f64();

    let mut spot_ms = Vec::new();
    for _ in 0..5 {
        let t = Instant::now();
        let _ = search::spot(
            repo,
            &store,
            &cfg,
            "ward doctor probe",
            Some("pub fn __ward_probe__() -> u8"),
            None,
            None,
        );
        spot_ms.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    let spot_mean_ms = spot_ms.iter().sum::<f64>() / spot_ms.len().max(1) as f64;
    let spot_max_ms = spot_ms.iter().cloned().fold(0.0, f64::max);

    let metrics_tail = crate::daemon::read_metrics(repo)
        .into_iter()
        .rev()
        .take(20)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();

    let daemon_log_tail = redact_log_tail(repo);

    let config_summary = if opts.include_config {
        serde_json::to_value(&cfg).unwrap_or_default()
    } else {
        serde_json::json!({
            "thresholds": cfg.thresholds,
            "top_k": cfg.top_k,
            "clusters_exclude_tests": cfg.clusters.exclude_tests,
            "suppress": format!("<redacted {} patterns>", cfg.suppress.len()),
            "languages": cfg.languages,
        })
    };

    let repo_name = std::fs::canonicalize(repo)
        .ok()
        .and_then(|p| p.file_name().map(|s| s.to_string_lossy().into_owned()))
        .unwrap_or_else(|| repo.to_string_lossy().into_owned());

    Ok(DoctorReport {
        ward_version: env!("CARGO_PKG_VERSION").to_string(),
        platform: format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
        repo_name,
        repo_path: opts
            .include_paths
            .then(|| repo.to_string_lossy().into_owned()),
        store: StoreHealth {
            // Total symbol population from the store — the incremental
            // index report's delta would read 0 on an unchanged repo.
            symbols: store.all_symbols().map(|s| s.len()).unwrap_or(0),
            languages: language_histogram(&store),
            schema_version: crate::store::SCHEMA_VERSION,
            integrity: integrity(&store),
        },
        config_summary,
        perf: PerfProbe {
            index_secs,
            spot_runs: spot_ms.len(),
            spot_mean_ms,
            spot_max_ms,
        },
        metrics_tail,
        daemon_log_tail,
        redacted: !(opts.include_snippets && opts.include_paths && opts.include_config),
        generated_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or_default(),
    })
}

/// Daemon-log tail with repo/home prefixes redacted.
fn redact_log_tail(repo: &Path) -> Vec<String> {
    let home = std::env::var("HOME").unwrap_or_default();
    let log = std::path::PathBuf::from(&home).join(".ward/daemon.log");
    let Ok(text) = std::fs::read_to_string(&log) else {
        return Vec::new();
    };
    let repo_s = repo.to_string_lossy().into_owned();
    text.lines()
        .rev()
        .take(20)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|l| l.replace(&repo_s, "<repo>").replace(&home, "~").to_string())
        .collect()
}

/// Write the portable bundle: `.ward/ward-doctor-<ts>.json`.
pub fn write_bundle(repo: &Path, report: &DoctorReport) -> Result<PathBuf> {
    let path = repo
        .join(".ward")
        .join(format!("ward-doctor-{}.json", report.generated_at));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&path, serde_json::to_string_pretty(report)?)
        .with_context(|| format!("writing bundle {}", path.display()))?;
    Ok(path)
}

/// Compose the GitHub issue body (pure, testable).
pub fn issue_body(
    title: &str,
    body: &str,
    report: &DoctorReport,
    report_advisory: Option<&str>,
    bundle_url: Option<&str>,
) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {title}\n\n"));
    if !body.trim().is_empty() {
        out.push_str(body.trim());
        out.push_str("\n\n");
    }
    out.push_str("## 环境（ward doctor 摘要）\n\n");
    out.push_str(&format!(
        "- ward {} / {} / 仓库 `{}`（{} 符号，语言分布 {}）\n",
        report.ward_version,
        report.platform,
        report.repo_name,
        report.store.symbols,
        report
            .store
            .languages
            .iter()
            .map(|(l, n)| format!("{l}={n}"))
            .collect::<Vec<_>>()
            .join(", "),
    ));
    out.push_str(&format!(
        "- 性能：增量索引 {:.2}s，spot 均值 {:.1}ms / 峰值 {:.1}ms（{} 次探测）\n",
        report.perf.index_secs,
        report.perf.spot_mean_ms,
        report.perf.spot_max_ms,
        report.perf.spot_runs,
    ));
    out.push_str(&format!(
        "- 索引：schema v{}，完整性 {}\n",
        report.store.schema_version, report.store.integrity
    ));
    if let Some(adv) = report_advisory {
        out.push_str(&format!(
            "- 关联 advisory：`{adv}`（ward report {adv} 可复现）\n"
        ));
    }
    if let Some(url) = bundle_url {
        out.push_str(&format!("- 诊断包（脱敏）：{url}\n"));
    }
    out.push_str("\n## 复现步骤\n\n（请补充：在哪个操作/输入下出现该问题）\n");
    out.push_str("\n## 隐私确认\n\n- [ ] 我已确认以上内容不含公司机密——诊断包默认不含源码片段与绝对路径（脱敏规则见 docs/USAGE.md §7）。\n");
    out
}

/// The outcome of an issue creation attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueOutcome {
    pub dry_run: bool,
    pub title: String,
    /// URL when created, or the would-post body in dry-run.
    pub url_or_body: String,
    pub bundle_url: Option<String>,
}

/// Create the issue via the gh CLI (P4 reuse). Dry-run never shells out.
pub fn create_issue(
    owner_repo: &str,
    title: &str,
    body: &str,
    bundle: Option<&Path>,
    dry_run: bool,
) -> Result<IssueOutcome> {
    let bundle_url = if dry_run {
        bundle.map(|p| format!("<gist of {}>", p.display()))
    } else if let Some(b) = bundle {
        let out = std::process::Command::new("gh")
            .args(["gist", "create", "--secret"])
            .arg(b)
            .output();
        match out {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
                .trim()
                .lines()
                .last()
                .map(str::to_string),
            _ => {
                return Err(anyhow::anyhow!(
                    "gh gist create 失败：请手动上传 {} 并在 issue 中附链接",
                    b.display()
                ));
            }
        }
    } else {
        None
    };

    if dry_run {
        return Ok(IssueOutcome {
            dry_run: true,
            title: title.to_string(),
            url_or_body: body.to_string(),
            bundle_url,
        });
    }

    let tmp = std::env::temp_dir().join(format!("ward-issue-{}.md", std::process::id()));
    std::fs::write(&tmp, body)?;
    let out = std::process::Command::new("gh")
        .args([
            "issue",
            "create",
            "--repo",
            owner_repo,
            "--title",
            title,
            "--body-file",
        ])
        .arg(&tmp)
        .output();
    let _ = std::fs::remove_file(&tmp);
    match out {
        Ok(o) if o.status.success() => {
            let url = String::from_utf8_lossy(&o.stdout).trim().to_string();
            Ok(IssueOutcome {
                dry_run: false,
                title: title.to_string(),
                url_or_body: url,
                bundle_url,
            })
        }
        Ok(o) => Err(anyhow::anyhow!(
            "gh issue create 失败: {}",
            String::from_utf8_lossy(&o.stderr).trim()
        )),
        Err(e) => Err(anyhow::anyhow!(
            "未找到 gh CLI（{e}）。请安装 GitHub CLI，或手动在 {owner_repo} 开 issue，附上以下内容：\n{body}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Advisory, Label, Store};

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
    fn doctor_redacts_paths_and_config_by_default() {
        let dir = rust_repo();
        let report = doctor(dir.path(), &DoctorOpts::default()).unwrap();
        assert!(report.repo_path.is_none(), "path must be redacted");
        assert_eq!(report.store.symbols, 1);
        assert!(report.store.languages.iter().any(|(l, _)| l == "rust"));
        assert!(
            report.config_summary["suppress"]
                .as_str()
                .unwrap()
                .contains("redacted")
        );
        assert!(report.redacted);
        assert!(report.perf.spot_runs >= 1);
        assert!(report.perf.index_secs >= 0.0);
    }

    #[test]
    fn bundle_roundtrips() {
        let dir = rust_repo();
        let report = doctor(dir.path(), &DoctorOpts::default()).unwrap();
        let path = write_bundle(dir.path(), &report).unwrap();
        let parsed: DoctorReport =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(parsed.ward_version, report.ward_version);
        assert_eq!(parsed.store.symbols, 1);
    }

    #[test]
    fn issue_body_contains_privacy_and_env_sections() {
        let dir = rust_repo();
        let report = doctor(dir.path(), &DoctorOpts::default()).unwrap();
        let body = issue_body(
            "崩溃：索引",
            "在 40 万行仓库上",
            &report,
            Some("adv_x"),
            Some("https://gist/x"),
        );
        assert!(body.contains("# 崩溃：索引"));
        assert!(body.contains("ward doctor 摘要"));
        assert!(body.contains("adv_x"));
        assert!(body.contains("https://gist/x"));
        assert!(body.contains("隐私确认"));
        assert!(body.contains("不含公司机密"));
    }

    #[test]
    fn issue_dry_run_never_shells_out() {
        let outcome = create_issue("ending0421/Ward", "t", "body", None, true).unwrap();
        assert!(outcome.dry_run);
        assert_eq!(outcome.url_or_body, "body");
    }

    #[test]
    fn advisory_report_pulls_full_context() {
        let dir = rust_repo();
        let store = Store::open(&Store::default_path(dir.path())).unwrap();
        store
            .record_advisory(&Advisory {
                id: "adv_x".into(),
                tool: "spot".into(),
                ts: 1,
                query_hash: "q".into(),
                result_json: r#"{"as_of":null,"stale":false,"query":"防抖","matches":[{"path":"src/lib.rs","lines":"1","symbol":"f","similarity":0.91,"kind":"near","note":""}],"advisory_id":"adv_x"}"#.into(),
                agent_action: Some("accepted".into()),
                inferred_action: Some("rejected".into()),
                inferred_commit_sha: Some("abc".into()),
            })
            .unwrap();
        store
            .record_label(&Label {
                id: None,
                advisory_id: "adv_x".into(),
                match_index: 0,
                query_hash: None,
                language: None,
                kind: Some("near".into()),
                similarity: Some(0.91),
                verdict: "y".into(),
                ts: 1,
            })
            .unwrap();
        let detail = crate::report::advisory_report(&store, "adv_x")
            .unwrap()
            .unwrap();
        assert_eq!(detail.query.as_deref(), Some("防抖"));
        assert_eq!(detail.agent_action.as_deref(), Some("accepted"));
        assert_eq!(detail.inferred_action.as_deref(), Some("rejected"));
        assert_eq!(detail.labels.len(), 1);
        assert_eq!(detail.labels[0].verdict, "y");
        // Snippets are opt-in: none by default.
        assert!(detail.snippets.is_none());
    }
}

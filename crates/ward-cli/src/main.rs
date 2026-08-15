//! The `ward` command-line interface — the outer-loop form of the same
//! binary that runs as an MCP daemon locally (spec §2.1.4: one binary, two
//! postures).
//!
//! Every subcommand is fail-open: Ward never blocks a developer workflow.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use ward_core::config::{self, WardConfig};
use ward_core::store::Store;
use ward_core::verify::{CatchVerdict, catch_run, verify_full};
use ward_core::{diff, index, search, spec};

#[derive(Parser)]
#[command(
    name = "ward",
    version,
    about = "Ward off AI slop — guardrails and verification for AI agent coding",
    long_about = None
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum ServiceAction {
    /// Install the daemon as a background service for this repo
    Install {
        #[arg(default_value = ".", long)]
        repo: PathBuf,
        /// Print the service unit instead of installing (inspect/test)
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove the background service
    Uninstall,
}

#[derive(Subcommand)]
enum LabelAction {
    /// Show the next unlabeled match with its code context
    Next {
        #[arg(long, default_value_t = 1)]
        count: usize,
        #[arg(long)]
        json: bool,
        #[arg(default_value = ".", long)]
        repo: PathBuf,
    },
    /// Record a verdict: y (relevant) or n (not relevant)
    Set {
        advisory_id: String,
        match_index: i64,
        #[arg(value_parser = ["y", "n"])]
        verdict: String,
        #[arg(default_value = ".", long)]
        repo: PathBuf,
    },
}

#[derive(Subcommand)]
enum Cmd {
    /// Write a starter .ward/config.toml
    Init {
        #[arg(default_value = ".", long)]
        repo: PathBuf,
    },
    /// Build/refresh the local index (The Rack)
    Index {
        #[arg(default_value = ".", long)]
        repo: PathBuf,
    },
    /// Pre-generation duplicate check (M1 Spot)
    Spot {
        #[arg(long)]
        intent: String,
        #[arg(long)]
        signature: Option<String>,
        #[arg(long)]
        body: Option<String>,
        #[arg(default_value = ".", long)]
        repo: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Deterministic semantic change summary (M2 Replay)
    Replay {
        base: String,
        head: String,
        #[arg(default_value = ".", long)]
        repo: PathBuf,
        #[arg(long)]
        json: bool,
        /// Add an LLM narration section (requires WARD_LLM_URL; every
        /// sentence is anchor-validated, failures fall back to the
        /// structured list).
        #[arg(long)]
        narrate: bool,
    },
    /// Inner-loop lint/type precheck (M3, no Docker)
    CatchRun {
        #[arg(default_value = ".", long)]
        repo: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Outer-loop adjudication in a Docker sandbox (M3)
    Verify {
        #[arg(long)]
        full: bool,
        #[arg(default_value = ".", long)]
        repo: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Evaluate a task spec's assertions (M4, inner-loop semantics)
    FormCheck {
        #[arg(long)]
        spec: PathBuf,
        #[arg(long)]
        base: Option<String>,
        #[arg(default_value = ".", long)]
        repo: PathBuf,
        #[arg(long)]
        json: bool,
        /// Outer-loop posture (P7): exit 1 on any `fail`, exit 2 on any
        /// `unknown` — for CI. Without it, form-check is advisory.
        #[arg(long)]
        ci: bool,
    },
    /// API/ABI compatibility adjudication against a base rev (M4, outer
    /// loop: cargo-semver-checks for Rust, unknown for other languages)
    CompatCheck {
        #[arg(long, default_value = "HEAD^")]
        base: String,
        #[arg(default_value = ".", long)]
        repo: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Infer adoption outcomes for pending advisories from the next commit
    /// (spec §3-M1 objective channel)
    Infer {
        #[arg(default_value = ".", long)]
        repo: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Golden-set labeling: show the next unlabeled match (or record a
    /// verdict for one)
    Label {
        #[command(subcommand)]
        action: LabelAction,
    },
    /// Threshold calibration from golden-set labels (Wilson intervals)
    Calibrate {
        #[arg(default_value = ".", long)]
        repo: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Record an idempotent daily trend snapshot
    Snapshot {
        #[arg(default_value = ".", long)]
        repo: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Governance report: adoption, clusters, constraint decay (CLI + JSON)
    Stats {
        #[arg(default_value = ".", long)]
        repo: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Background unattended worker: watch→index, poll→infer, daily
    /// snapshot, weekly metrics (无感模式)
    Daemon {
        #[arg(default_value = ".", long)]
        repo: PathBuf,
        #[arg(long, default_value_t = 300)]
        interval_secs: u64,
    },
    /// Environment & health probe with a privacy-redacted portable bundle
    Doctor {
        #[arg(default_value = ".", long)]
        repo: PathBuf,
        #[arg(long)]
        json: bool,
        /// Write the portable bundle (.ward/ward-doctor-<ts>.json)
        #[arg(long)]
        bundle: bool,
        /// Opt-in: include source snippets in the bundle
        #[arg(long)]
        include_snippets: bool,
        /// Opt-in: include the full config
        #[arg(long)]
        include_config: bool,
        /// Opt-in: include the absolute repo path
        #[arg(long)]
        include_paths: bool,
    },
    /// Full context dump for one advisory (reproduction input)
    Report {
        advisory_id: String,
        #[arg(default_value = ".", long)]
        repo: PathBuf,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        include_snippets: bool,
    },
    /// Compose and (optionally) open a GitHub issue with diagnostics
    Issue {
        #[arg(long)]
        title: String,
        #[arg(long, default_value = "")]
        body: String,
        #[arg(long)]
        report: Option<String>,
        #[arg(long)]
        bundle: Option<PathBuf>,
        #[arg(long, default_value = "ending0421/Ward")]
        repo_owner: String,
        /// Default: preview only. Without --yes, nothing is posted.
        #[arg(long)]
        yes: bool,
    },
    /// Install (or remove) the background service (launchd / systemd)
    Service {
        #[command(subcommand)]
        action: ServiceAction,
    },
    /// Install (or remove) the git post-commit hook that auto-runs `ward infer`
    SetupHooks {
        #[arg(default_value = ".", long)]
        repo: PathBuf,
        #[arg(long)]
        remove: bool,
    },
    /// Soft intent-drift comparison: original requirement vs change facts
    /// (M4-b, LLM partition; requires WARD_LLM_URL, else "not executed")
    IntentCheck {
        #[arg(long)]
        requirement: String,
        #[arg(long, default_value = "HEAD^")]
        base: String,
        #[arg(long, default_value = "HEAD")]
        head: String,
        #[arg(default_value = ".", long)]
        repo: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// One-page context card for a symbol (M5: definition, callers, tests,
    /// config references)
    Card {
        /// Symbol name or path:line
        query: String,
        #[arg(default_value = ".", long)]
        repo: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Offline duplicate clustering for consolidation (M6)
    Clusters {
        #[arg(long, default_value_t = 0.92)]
        threshold: f64,
        #[arg(default_value = ".", long)]
        repo: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Record the agent's self-reported action for an advisory (M1 feedback)
    Action {
        advisory: String,
        /// accepted | ignored | dismissed
        action: String,
        #[arg(default_value = ".", long)]
        repo: PathBuf,
    },
}

fn load_config(repo: &std::path::Path) -> WardConfig {
    let path = config::default_path(repo);
    let (cfg, warn) = WardConfig::load_or_default(&path);
    if let Some(w) = warn {
        eprintln!("ward: warning: {w}");
    }
    cfg
}

fn open_store(repo: &std::path::Path) -> Result<Store> {
    Store::open(&Store::default_path(repo))
        .with_context(|| format!("opening index for {}", repo.display()))
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_target(false)
        .init();

    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Init { repo } => {
            let path = config::default_path(&repo);
            config::write_starter_config(&path)?;
            println!("wrote {}", path.display());
        }
        Cmd::Index { repo } => {
            let cfg = load_config(&repo);
            let report = index::index_repo(&repo, &cfg)?;
            println!(
                "indexed {} files / {} symbols ({} unchanged, {} skipped-language, {} unparsable, {} suppressed) at {:?}",
                report.files_indexed,
                report.symbols_indexed,
                report.files_unchanged,
                report.files_skipped_language,
                report.files_unparsable,
                report.files_suppressed,
                report.commit_sha.as_deref().unwrap_or("uncommitted")
            );
        }
        Cmd::Spot {
            intent,
            signature,
            body,
            repo,
            json,
        } => {
            let cfg = load_config(&repo);
            let store = open_store(&repo)?;
            let result = search::spot(
                &repo,
                &store,
                &cfg,
                &intent,
                signature.as_deref(),
                body.as_deref(),
            )?;
            if json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                println!(
                    "spot advisory {} (as_of={:?}, stale={})",
                    result.advisory_id, result.as_of, result.stale
                );
                for m in &result.matches {
                    println!(
                        "  [{} {:0.2}] {}:{} ({}) — {}",
                        m.kind, m.similarity, m.path, m.lines, m.symbol, m.note
                    );
                }
                if result.matches.is_empty() {
                    println!("  (no matches above threshold)");
                }
                if result.stale {
                    println!("  warning: index is stale; treat matches as weak evidence");
                }
            }
        }
        Cmd::Replay {
            base,
            head,
            repo,
            json,
            narrate,
        } => {
            let cfg = load_config(&repo);
            let store = open_store(&repo)?;
            let report = diff::replay(&repo, &store, &cfg, &base, &head)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else if narrate {
                let provider = ward_core::llm::http_llm_from_env();
                let out = ward_core::narrate::narrate(&report, provider.as_deref());
                print!("{out}");
            } else {
                print!("{}", diff::render_markdown(&report));
            }
        }
        Cmd::CatchRun { repo, json } => {
            let cfg = load_config(&repo);
            let report = catch_run(&repo, &cfg);
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!(
                    "catch_run: {} — {} ({:?})",
                    report.verdict.as_str(),
                    report.note,
                    std::time::Duration::from_millis(report.duration_ms)
                );
                if !report.output_tail.is_empty() {
                    println!("---\n{}", report.output_tail);
                }
            }
            // Outer-loop posture: a failed inner check does not block
            // (fail-open); only CI adjudicates.
            if report.verdict == CatchVerdict::Fail {
                eprintln!("note: inner-loop verdict is advisory; CI outer loop adjudicates");
            }
        }
        Cmd::Verify { full, repo, json } => {
            let cfg = load_config(&repo);
            let report = if full {
                verify_full(&repo, &cfg)
            } else {
                // Without --full, verify is the inner precheck (same as
                // catch-run) — spelled out so scripts stay unambiguous.
                catch_run(&repo, &cfg)
            };
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!(
                    "verify: {} — {} ({:?})",
                    report.verdict.as_str(),
                    report.note,
                    std::time::Duration::from_millis(report.duration_ms)
                );
                if !report.output_tail.is_empty() {
                    println!("---\n{}", report.output_tail);
                }
            }
            if report.verdict == CatchVerdict::Fail && full {
                std::process::exit(1); // outer loop is fail-closed (P7)
            }
            if report.verdict == CatchVerdict::Unknown && full {
                // P7: unknown is never green in the outer loop.
                std::process::exit(2);
            }
        }
        Cmd::FormCheck {
            spec: spec_path,
            base,
            repo,
            json,
            ci,
        } => {
            let cfg = load_config(&repo);
            let store = open_store(&repo)?;
            let parsed = spec::parse_spec_file(&spec_path)
                .with_context(|| format!("parsing {}", spec_path.display()))?;
            let head =
                ward_core::git::head_sha(&repo)?.unwrap_or_else(|| "uncommitted".to_string());
            let base = base.unwrap_or_else(|| "HEAD^".to_string());
            let results = spec::evaluate(&repo, &parsed, &base, &head)?;
            for r in &results {
                store.record_contract_run(&ward_core::store::ContractRun {
                    spec_path: spec_path.to_string_lossy().into_owned(),
                    commit_sha: head.clone(),
                    ts: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or_default(),
                    assertion: r.assertion.clone(),
                    verdict: r.verdict.as_str().to_string(),
                    detail: r.detail.clone(),
                })?;
            }
            if json {
                println!("{}", serde_json::to_string_pretty(&results)?);
            } else {
                println!(
                    "form-check {} ({}..{}):",
                    spec_path.display(),
                    short(&base),
                    short(&head)
                );
                for r in &results {
                    println!("  [{}] {} — {}", r.verdict.as_str(), r.assertion, r.detail);
                }
                for issue in &parsed.issues {
                    println!("  [issue] {issue}");
                }
                println!("note: 本预检非裁决；CI 外环结果为准");
            }
            if ci {
                use ward_core::spec::Verdict;
                let any_fail = results.iter().any(|r| r.verdict == Verdict::Fail);
                let any_unknown = results.iter().any(|r| r.verdict == Verdict::Unknown);
                if any_fail {
                    std::process::exit(1); // 外环 fail-closed（P7）
                }
                if any_unknown {
                    std::process::exit(2); // unknown 不绿灯（P7）
                }
            }
            let _ = cfg;
        }
        Cmd::CompatCheck { base, repo, json } => {
            let report = ward_core::compat::api_compat_check(&repo, &base);
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!(
                    "compat-check [{}] {} — {} ({:?})",
                    report.tool,
                    report.verdict.as_str(),
                    report.detail,
                    std::time::Duration::from_millis(report.duration_ms)
                );
            }
            // Outer loop: unknown is never green (P7).
            if report.verdict == ward_core::compat::CompatVerdict::Fail {
                std::process::exit(1);
            }
            if report.verdict == ward_core::compat::CompatVerdict::Unknown {
                std::process::exit(2);
            }
        }
        Cmd::Infer { repo, json } => {
            let store = open_store(&repo)?;
            let cfg = load_config(&repo);
            let report = ward_core::infer::infer_pending(&repo, &store, &cfg)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!(
                    "inferred {} advisories: {} accepted / {} reused-ish / {} rejected / {} unknown",
                    report.considered,
                    report.accepted,
                    report.reused_ish,
                    report.rejected,
                    report.unknown
                );
            }
        }
        Cmd::Label { action } => match action {
            LabelAction::Next { count, json, repo } => {
                let store = open_store(&repo)?;
                let cands = ward_core::label::candidates(&store, &repo, count)?;
                if json {
                    println!("{}", serde_json::to_string_pretty(&cands)?);
                } else if cands.is_empty() {
                    println!("没有未标注的 match。跑几次 spot 后再来。");
                } else {
                    for c in cands {
                        println!(
                            "advisory={} match={} [{} {:.2}] {}:{} ({})\n  查询: {}\n{}",
                            c.advisory_id,
                            c.match_index,
                            c.kind,
                            c.similarity,
                            c.path,
                            c.lines,
                            c.symbol,
                            c.query.as_deref().unwrap_or("-"),
                            c.snippet.as_deref().unwrap_or("(snippet unavailable)")
                        );
                        println!(
                            "  标注: ward label set {} {} y|n",
                            c.advisory_id, c.match_index
                        );
                        println!();
                    }
                }
            }
            LabelAction::Set {
                advisory_id,
                match_index,
                verdict,
                repo,
            } => {
                let store = open_store(&repo)?;
                ward_core::label::label_match(&store, &advisory_id, match_index, &verdict)?;
                println!("labeled {advisory_id}#{match_index} = {verdict}");
            }
        },
        Cmd::Calibrate { repo, json } => {
            let store = open_store(&repo)?;
            let report = ward_core::calibrate::calibrate(&store)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!("标注总数: {} | {}", report.total_verdicts, report.note);
                for r in &report.rows {
                    if r.yes + r.no == 0 {
                        continue;
                    }
                    println!(
                        "  >= {:.2}: {}/{} = {:.0}% (95% CI {:.0}%-{:.0}%){}",
                        r.threshold,
                        r.yes,
                        r.yes + r.no,
                        r.precision * 100.0,
                        r.ci_low * 100.0,
                        r.ci_high * 100.0,
                        if r.sufficient_sample {
                            ""
                        } else {
                            " [样本不足]"
                        }
                    );
                }
                match report.suggested_strong {
                    Some(t) => println!("建议 strong 阈值: >= {t}（--apply 手动写入 config）"),
                    None => println!("暂无可靠建议阈值"),
                }
            }
        }
        Cmd::Snapshot { repo, json } => {
            let store = open_store(&repo)?;
            let cfg = load_config(&repo);
            let snap = ward_core::stats::snapshot_now(&repo, &store, &cfg)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&snap)?);
            } else {
                println!(
                    "snapshot day={} symbols={} clusters={} advisories={} labels={}",
                    snap.ts, snap.symbols, snap.clusters, snap.advisories, snap.labels
                );
            }
        }
        Cmd::Stats { repo, json } => {
            let store = open_store(&repo)?;
            let cfg = load_config(&repo);
            let report = ward_core::stats::stats(&repo, &store, &cfg)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print!("{}", ward_core::stats::render_table(&report));
            }
        }
        Cmd::Doctor {
            repo,
            json,
            bundle,
            include_snippets,
            include_config,
            include_paths,
        } => {
            let opts = ward_core::doctor::DoctorOpts {
                include_snippets,
                include_config,
                include_paths,
            };
            let report = ward_core::doctor::doctor(&repo, &opts)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!(
                    "ward {} / {} / 仓库 {}（{} 符号）",
                    report.ward_version, report.platform, report.repo_name, report.store.symbols
                );
                println!(
                    "  语言: {}",
                    report
                        .store
                        .languages
                        .iter()
                        .map(|(l, n)| format!("{l}={n}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                println!(
                    "  性能: 索引 {:.2}s, spot 均值 {:.1}ms / 峰值 {:.1}ms",
                    report.perf.index_secs, report.perf.spot_mean_ms, report.perf.spot_max_ms
                );
                println!(
                    "  脱敏: {} | 指标行: {} | daemon 日志: {} 行",
                    report.redacted,
                    report.metrics_tail.len(),
                    report.daemon_log_tail.len()
                );
            }
            if bundle {
                let path = ward_core::doctor::write_bundle(&repo, &report)?;
                println!("bundle written: {}", path.display());
            }
        }
        Cmd::Report {
            advisory_id,
            repo,
            json,
            include_snippets,
        } => {
            let store = open_store(&repo)?;
            match ward_core::report::advisory_report(&store, &advisory_id)? {
                None => anyhow::bail!("unknown advisory id {advisory_id}"),
                Some(mut detail) => {
                    if include_snippets {
                        detail = ward_core::report::with_snippets(detail, &repo);
                    }
                    if json {
                        println!("{}", serde_json::to_string_pretty(&detail)?);
                    } else {
                        println!("advisory {} (ts {})", detail.id, detail.ts);
                        println!("  查询: {}", detail.query.as_deref().unwrap_or("-"));
                        println!(
                            "  处置: 自报 {:?} / 推断 {:?} (@ {:?})",
                            detail.agent_action, detail.inferred_action, detail.inferred_commit_sha
                        );
                        println!(
                            "  标注: {}",
                            detail
                                .labels
                                .iter()
                                .map(|l| format!("#{}={}", l.match_index, l.verdict))
                                .collect::<Vec<_>>()
                                .join(", ")
                        );
                        for (i, m) in detail.matches.iter().enumerate() {
                            println!(
                                "  match#{i} [{} {:.2}] {}:{} ({})",
                                m.kind, m.similarity, m.path, m.lines, m.symbol
                            );
                            if let Some(snips) = &detail.snippets {
                                if let Some(Some(sn)) = snips.get(i) {
                                    println!("{sn}");
                                }
                            }
                        }
                    }
                }
            }
        }
        Cmd::Issue {
            title,
            body,
            report,
            bundle,
            repo_owner,
            yes,
        } => {
            let repo = PathBuf::from(".");
            let doctor_report =
                ward_core::doctor::doctor(&repo, &ward_core::doctor::DoctorOpts::default())?;
            let final_body = ward_core::doctor::issue_body(
                &title,
                &body,
                &doctor_report,
                report.as_deref(),
                None,
            );
            let dry_run = !yes;
            let outcome = ward_core::doctor::create_issue(
                &repo_owner,
                &title,
                &final_body,
                bundle.as_deref(),
                dry_run,
            )?;
            if outcome.dry_run {
                println!("【预览（默认 dry-run）】以下内容将提交到 {repo_owner}：\n");
                println!("{}", outcome.url_or_body);
                if let Some(u) = outcome.bundle_url {
                    println!("诊断包: {u}");
                }
                println!("\n确认无误后加 --yes 真正提交。");
            } else {
                println!("issue created: {}", outcome.url_or_body);
                if let Some(u) = outcome.bundle_url {
                    println!("诊断包 gist: {u}");
                }
            }
        }
        Cmd::Daemon {
            repo,
            interval_secs,
        } => {
            use notify::Watcher;
            let (tx, rx) = std::sync::mpsc::channel();
            let mut watcher = notify::recommended_watcher(tx)?;
            watcher.watch(&repo, notify::RecursiveMode::Recursive)?;
            let interval = std::time::Duration::from_secs(interval_secs);
            let mut last_head = ward_core::git::head_sha(&repo)?;
            let mut last_day: Option<i64> = None;
            let mut last_week: Option<u32> = None;
            let mut dirty_since: Option<std::time::Instant> = None;
            println!(
                "ward daemon: watching {} (interval {interval_secs}s)",
                repo.display()
            );
            loop {
                let deadline = std::time::Instant::now() + interval;
                while std::time::Instant::now() < deadline {
                    if rx
                        .recv_timeout(std::time::Duration::from_millis(500))
                        .is_ok()
                    {
                        dirty_since = Some(std::time::Instant::now());
                    }
                }
                let cfg = load_config(&repo);
                if dirty_since.take().is_some() {
                    match ward_core::daemon::run_index_tick(&repo, &cfg) {
                        Ok(r) => println!(
                            "[index] {}/{} symbols ({} unchanged)",
                            r.files_indexed, r.symbols_indexed, r.files_unchanged
                        ),
                        Err(e) => eprintln!("[index] skipped (fail-open): {e}"),
                    }
                }
                if let Ok(Some(head)) = ward_core::git::head_sha(&repo) {
                    if last_head.as_deref() != Some(head.as_str()) {
                        match ward_core::daemon::run_infer_tick(&repo, &cfg) {
                            Ok(r) => println!(
                                "[infer] {} considered / {} accepted / {} rejected",
                                r.considered, r.accepted, r.rejected
                            ),
                            Err(e) => eprintln!("[infer] skipped (fail-open): {e}"),
                        }
                        last_head = Some(head);
                    }
                }
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or_default();
                let day = now / 86400;
                let week = (day / 7) as u32;
                if last_day != Some(day) {
                    match ward_core::daemon::run_daily_tick(&repo, &cfg) {
                        Ok(s) => println!(
                            "[daily] snapshot symbols={} clusters={}",
                            s.symbols, s.clusters
                        ),
                        Err(e) => eprintln!("[daily] skipped (fail-open): {e}"),
                    }
                    last_day = Some(day);
                }
                if last_week != Some(week) {
                    match ward_core::daemon::run_weekly_tick(&repo, &cfg) {
                        Ok(r) => println!(
                            "[weekly] clusters={} labels={} jscpd={:?} note={}",
                            r.clusters, r.labels, r.jscpd, r.calibration_note
                        ),
                        Err(e) => eprintln!("[weekly] skipped (fail-open): {e}"),
                    }
                    last_week = Some(week);
                }
            }
        }
        Cmd::Service { action } => match action {
            ServiceAction::Install { repo, dry_run } => {
                let repo_abs = std::fs::canonicalize(&repo)?;
                let bin = std::env::current_exe()?;
                let unit = launchd_plist(&bin.to_string_lossy(), &repo_abs.to_string_lossy());
                if dry_run {
                    println!("{unit}");
                } else {
                    #[cfg(target_os = "macos")]
                    {
                        let dir = dirs_launch_agents_dir()?;
                        std::fs::create_dir_all(&dir).ok();
                        let path = dir.join("com.ward.daemon.plist");
                        std::fs::write(&path, &unit)?;
                        let status = std::process::Command::new("launchctl")
                            .args(["load", "-w"])
                            .arg(&path)
                            .status()?;
                        anyhow::ensure!(status.success(), "launchctl load failed");
                        println!("installed {} (launchd)", path.display());
                        println!("日志: ~/.ward/daemon.log；卸载: ward service uninstall");
                    }
                    #[cfg(target_os = "linux")]
                    {
                        let dir = std::path::PathBuf::from(
                            std::env::var("XDG_CONFIG_HOME").unwrap_or_else(|_| {
                                format!("{}/.config", std::env::var("HOME").unwrap_or_default())
                            }),
                        )
                        .join("systemd/user");
                        std::fs::create_dir_all(&dir).ok();
                        let path = dir.join("ward-daemon.service");
                        let unit =
                            systemd_unit(&bin.to_string_lossy(), &repo_abs.to_string_lossy());
                        std::fs::write(&path, &unit)?;
                        let status = std::process::Command::new("systemctl")
                            .args(["--user", "daemon-reload"])
                            .status()?;
                        anyhow::ensure!(status.success(), "systemctl daemon-reload failed");
                        let status = std::process::Command::new("systemctl")
                            .args(["--user", "enable", "--now", "ward-daemon.service"])
                            .status()?;
                        anyhow::ensure!(status.success(), "systemctl enable --now failed");
                        println!("installed {} (systemd user unit)", path.display());
                    }
                    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
                    anyhow::bail!("service install 仅支持 macOS (launchd) 与 Linux (systemd)");
                }
            }
            ServiceAction::Uninstall => {
                #[cfg(target_os = "macos")]
                {
                    let dir = dirs_launch_agents_dir()?;
                    let path = dir.join("com.ward.daemon.plist");
                    if path.exists() {
                        let _ = std::process::Command::new("launchctl")
                            .args(["unload", "-w"])
                            .arg(&path)
                            .status();
                        std::fs::remove_file(&path)?;
                        println!("removed {}", path.display());
                    } else {
                        println!("no service installed");
                    }
                }
                #[cfg(target_os = "linux")]
                {
                    let _ = std::process::Command::new("systemctl")
                        .args(["--user", "disable", "--now", "ward-daemon.service"])
                        .status();
                    let dir =
                        std::path::PathBuf::from(std::env::var("XDG_CONFIG_HOME").unwrap_or_else(
                            |_| format!("{}/.config", std::env::var("HOME").unwrap_or_default()),
                        ))
                        .join("systemd/user/ward-daemon.service");
                    if dir.exists() {
                        std::fs::remove_file(&dir)?;
                    }
                    println!("removed systemd user unit");
                }
                #[cfg(not(any(target_os = "macos", target_os = "linux")))]
                println!("此平台无服务可卸载");
            }
        },
        Cmd::SetupHooks { repo, remove } => {
            let hook_path = repo.join(".git/hooks/post-commit");
            if remove {
                match std::fs::remove_file(&hook_path) {
                    Ok(()) => println!("removed {}", hook_path.display()),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        println!("no hook at {}", hook_path.display());
                    }
                    Err(e) => anyhow::bail!("removing hook: {e}"),
                }
            } else {
                if !repo.join(".git").is_dir() {
                    anyhow::bail!("{} is not a git repository", repo.display());
                }
                let script = "#!/bin/sh\n# Ward post-commit: infer adoption outcomes (fail-open, never blocks).\nexec ward infer --repo \"$(git rev-parse --show-toplevel)\"\n";
                std::fs::write(&hook_path, script)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&hook_path, std::fs::Permissions::from_mode(0o755))?;
                }
                println!("installed {}", hook_path.display());
            }
        }
        Cmd::IntentCheck {
            requirement,
            base,
            head,
            repo,
            json,
        } => {
            let cfg = load_config(&repo);
            let store = open_store(&repo)?;
            let provider = ward_core::llm::http_llm_from_env();
            let hint = match ward_core::intent::intent_drift_check(
                &repo,
                &store,
                &cfg,
                &requirement,
                &base,
                &head,
                provider.as_deref(),
            ) {
                Ok(h) => h,
                // Fail-open: bad refs / unindexed repo degrade to a honest
                // "not executed" hint instead of a hard error.
                Err(e) => ward_core::intent::DriftHint {
                    executed: false,
                    partition: "llm_soft".into(),
                    hints: Vec::new(),
                    note: format!("M4-b 未执行：{e}"),
                },
            };
            if json {
                println!("{}", serde_json::to_string_pretty(&hint)?);
            } else {
                println!(
                    "intent-check [{}] executed={}:",
                    hint.partition, hint.executed
                );
                for h in &hint.hints {
                    println!("  - {h}");
                }
                println!("note: {}", hint.note);
            }
        }
        Cmd::Card { query, repo, json } => {
            let cfg = load_config(&repo);
            let store = open_store(&repo)?;
            let card = ward_core::context::context_card(&repo, &store, &cfg, &query)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&card)?);
            } else {
                println!(
                    "{} ({}) {}:{}",
                    card.symbol, card.kind, card.path, card.lines
                );
                println!("  callers (at least {}):", card.callers.len());
                for c in card.callers.iter().take(10) {
                    println!("    {} {}", c.path, c.symbol);
                }
                if card.callers.len() > 10 {
                    println!("    … and {} more", card.callers.len() - 10);
                }
                println!("  related tests: {}", card.tests.len());
                for t in card.tests.iter().take(5) {
                    println!("    {} {}", t.path, t.symbol);
                }
                println!("  config refs: {}", card.config_refs.len());
                for r in card.config_refs.iter().take(5) {
                    println!("    {}:{}", r.path, r.line);
                }
            }
        }
        Cmd::Clusters {
            threshold,
            repo,
            json,
        } => {
            let store = open_store(&repo)?;
            let report = ward_core::cluster::cluster_duplicates(&store, threshold)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!(
                    "{} clusters ({} pairwise checks, truncated={})",
                    report.clusters.len(),
                    report.pairs_checked,
                    report.truncated
                );
                for c in &report.clusters {
                    println!("  [{}x] {}", c.members.len(), c.suggestion);
                    for m in c.members.iter().take(4) {
                        println!("      {} {} ({:.2})", m.path, m.symbol, m.similarity);
                    }
                }
            }
        }
        Cmd::Action {
            advisory,
            action,
            repo,
        } => {
            let store = open_store(&repo)?;
            if !matches!(action.as_str(), "accepted" | "ignored" | "dismissed") {
                anyhow::bail!("action must be one of accepted|ignored|dismissed");
            }
            store.set_agent_action(&advisory, &action)?;
            println!("recorded agent_action={action} for {advisory}");
        }
    }
    Ok(())
}

/// The launchd LaunchAgent plist for the unattended daemon (pure, testable).
fn launchd_plist(bin: &str, repo: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>com.ward.daemon</string>
    <key>ProgramArguments</key>
    <array>
        <string>{bin}</string>
        <string>daemon</string>
        <string>--repo</string>
        <string>{repo}</string>
    </array>
    <key>RunAtLoad</key><true/>
    <key>KeepAlive</key><true/>
    <key>StandardOutPath</key><string>{home}/.ward/daemon.log</string>
    <key>StandardErrorPath</key><string>{home}/.ward/daemon.log</string>
</dict>
</plist>
"#,
        bin = bin,
        repo = repo,
        home = std::env::var("HOME").unwrap_or_default(),
    )
}

#[cfg(target_os = "linux")]
/// The systemd user unit (Linux).
fn systemd_unit(bin: &str, repo: &str) -> String {
    format!(
        "[Unit]\nDescription=Ward unattended daemon\n\n[Service]\nExecStart={bin} daemon --repo {repo}\nRestart=on-failure\n\n[Install]\nWantedBy=default.target\n"
    )
}

#[cfg(target_os = "macos")]
fn dirs_launch_agents_dir() -> anyhow::Result<std::path::PathBuf> {
    Ok(
        std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default())
            .join("Library/LaunchAgents"),
    )
}

fn short(sha: &str) -> String {
    sha.chars().take(8).collect()
}

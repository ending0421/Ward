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

fn short(sha: &str) -> String {
    sha.chars().take(8).collect()
}

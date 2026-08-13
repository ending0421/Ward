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
use ward_core::verify::{catch_run, verify_full, CatchVerdict};
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
                "indexed {} files / {} symbols ({} skipped-language, {} unparsable, {} suppressed) at {:?}",
                report.files_indexed,
                report.symbols_indexed,
                report.files_skipped_language,
                report.files_unparsable,
                report.files_suppressed,
                report.commit_sha.as_deref().unwrap_or("uncommitted")
            );
        }
        Cmd::Spot {
            intent,
            signature,
            repo,
            json,
        } => {
            let cfg = load_config(&repo);
            let store = open_store(&repo)?;
            let result = search::spot(&repo, &store, &cfg, &intent, signature.as_deref())?;
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
        } => {
            let cfg = load_config(&repo);
            let store = open_store(&repo)?;
            let report = diff::replay(&repo, &store, &cfg, &base, &head)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
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
        } => {
            let cfg = load_config(&repo);
            let store = open_store(&repo)?;
            let parsed = spec::parse_spec_file(&spec_path)
                .with_context(|| format!("parsing {}", spec_path.display()))?;
            let head = ward_core::git::head_sha(&repo)?
                .unwrap_or_else(|| "uncommitted".to_string());
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
            let _ = cfg;
        }
        Cmd::Action { advisory, action, repo } => {
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

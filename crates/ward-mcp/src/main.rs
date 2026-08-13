//! Ward MCP daemon — the inner-loop form of the Ward binary (spec §2.1.4).
//!
//! Exposes the four advisory tools over stdio MCP:
//! `spot` / `replay` / `catch_run` / `form_check`, plus `spot_action` for the
//! M1 feedback loop. Everything here is advisory and fail-open: a failure is
//! reported as structured output, never a broken session.

use std::path::PathBuf;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::{ServiceExt, schemars, tool, tool_router, transport::stdio};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ward_core::config::{self, WardConfig};
use ward_core::store::Store;
use ward_core::verify::{catch_run, verify_full};

/// Common request wrapper: which repository to operate on.
fn resolve_repo(repo: Option<String>) -> PathBuf {
    repo.map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

fn load_config(repo: &std::path::Path) -> WardConfig {
    let (cfg, warn) = WardConfig::load_or_default(&config::default_path(repo));
    if let Some(w) = warn {
        eprintln!("ward: warning: {w}");
    }
    cfg
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct SpotParams {
    /// What the agent intends to implement, in natural language.
    pub intent: String,
    /// Optional proposed signature (Rust). Fingerprint evidence requires it.
    pub proposed_signature: Option<String>,
    /// Optional already-written body — enables block-level fingerprint
    /// matches (the PostToolUse flow).
    pub proposed_body: Option<String>,
    /// Repository root; defaults to the daemon's working directory.
    pub repo: Option<String>,
    /// Number of matches to return.
    pub top_k: Option<usize>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ReplayParams {
    pub base: String,
    pub head: String,
    pub repo: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct CatchRunParams {
    pub repo: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct FormCheckParams {
    /// Path to a `specs/<task-id>.md` file.
    pub spec_path: String,
    /// Base ref; defaults to HEAD^.
    pub base: Option<String>,
    pub repo: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct SpotActionParams {
    pub advisory_id: String,
    /// accepted | ignored | dismissed
    pub action: String,
    pub repo: Option<String>,
}

/// Serialized wrapper that makes tool failures explicit instead of fatal.
#[derive(Serialize)]
struct ToolEnvelope<T: Serialize> {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<T>,
}

impl<T: Serialize> ToolEnvelope<T> {
    fn ok(data: T) -> String {
        serde_json::to_string_pretty(&ToolEnvelope {
            ok: true,
            error: None,
            data: Some(data),
        })
        .unwrap_or_else(|_| r#"{"ok":false,"error":"serialize failed"}"#.into())
    }
}

/// Non-generic error envelope (fail-open tools report errors as structured
/// output instead of dying).
fn tool_err(e: impl std::fmt::Display) -> String {
    serde_json::to_string(&ToolEnvelope {
        ok: false,
        error: Some(e.to_string()),
        data: None::<serde_json::Value>,
    })
    .unwrap_or_else(|_| r#"{"ok":false,"error":"serialize failed"}"#.into())
}

#[derive(Clone)]
struct WardMcp;

#[tool_router(server_handler)]
impl WardMcp {
    /// Pre-generation duplicate check: find existing similar implementations
    /// before writing new code. Fail-open advisory; never blocks.
    #[tool(
        description = "Pre-generation duplicate check (M1 Spot). Pass an intent and, ideally, a proposed signature; returns structurally similar existing symbols with path:line anchors."
    )]
    fn spot(&self, Parameters(p): Parameters<SpotParams>) -> String {
        let repo = resolve_repo(p.repo);
        let cfg = load_config(&repo);
        let mut cfg = cfg;
        if let Some(k) = p.top_k {
            cfg.top_k = k;
        }
        let result = (|| -> anyhow::Result<_> {
            let store = Store::open(&Store::default_path(&repo))?;
            ward_core::search::spot(
                &repo,
                &store,
                &cfg,
                &p.intent,
                p.proposed_signature.as_deref(),
                p.proposed_body.as_deref(),
            )
        })();
        match result {
            Ok(r) => ToolEnvelope::ok(r),
            Err(e) => tool_err(format!("spot failed (fail-open): {e}")),
        }
    }

    /// Deterministic symbol-level change summary between two commits.
    /// Structured facts only; the LLM narration layer is not part of this
    /// tool.
    #[tool(
        description = "Deterministic semantic change summary (M2 Replay) between two commits, with path:line anchors, lower-bound impact and risk markers."
    )]
    fn replay(&self, Parameters(p): Parameters<ReplayParams>) -> String {
        let repo = resolve_repo(p.repo);
        let cfg = load_config(&repo);
        let result = (|| -> anyhow::Result<_> {
            let store = Store::open(&Store::default_path(&repo))?;
            ward_core::diff::replay(&repo, &store, &cfg, &p.base, &p.head)
        })();
        match result {
            Ok(r) => ToolEnvelope::ok(r),
            Err(e) => tool_err(format!("replay failed (fail-open): {e}")),
        }
    }

    /// Inner-loop lint/type precheck. No Docker; full test suites are
    /// deferred to the CI outer loop.
    #[tool(
        description = "Inner-loop lint/type precheck (M3 Catch, no Docker). Verdict pass/fail/deferred/unknown; full test suites are deferred to CI."
    )]
    fn catch_run(&self, Parameters(p): Parameters<CatchRunParams>) -> String {
        let repo = resolve_repo(p.repo);
        let cfg = load_config(&repo);
        let report = if cfg.lint.command.trim().is_empty() {
            ward_core::verify::catch_run(&repo, &cfg)
        } else {
            catch_run(&repo, &cfg)
        };
        ToolEnvelope::ok(report)
    }

    /// Outer-loop sandbox adjudication — only meaningful on machines with
    /// Docker; otherwise verdict is `unknown` (F13), never a fake pass.
    #[tool(
        description = "Outer-loop sandbox adjudication (M3, Docker required). Without a sandbox the verdict is 'unknown' — never a fake green."
    )]
    fn verify_full(&self, Parameters(p): Parameters<CatchRunParams>) -> String {
        let repo = resolve_repo(p.repo);
        let cfg = load_config(&repo);
        ToolEnvelope::ok(verify_full(&repo, &cfg))
    }

    /// Evaluate a task spec's assertions against base..head (inner-loop
    /// semantics: deferred/unknown for CI-only assertions).
    #[tool(
        description = "Evaluate a task spec's machine-checkable assertions (M4 Form Check, inner-loop semantics). CI-only assertions are deferred, never faked."
    )]
    fn form_check(&self, Parameters(p): Parameters<FormCheckParams>) -> String {
        let repo = resolve_repo(p.repo);
        let result = (|| -> anyhow::Result<_> {
            let spec = ward_core::spec::parse_spec_file(std::path::Path::new(&p.spec_path))?;
            let head = ward_core::git::head_sha(&repo)?.unwrap_or_else(|| "uncommitted".into());
            let base = p.base.unwrap_or_else(|| "HEAD^".into());
            let results = ward_core::spec::evaluate(&repo, &spec, &base, &head)?;
            let store = Store::open(&Store::default_path(&repo))?;
            for r in &results {
                store.record_contract_run(&ward_core::store::ContractRun {
                    spec_path: p.spec_path.clone(),
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
            Ok(serde_json::json!({
                "results": results,
                "note": "本预检非裁决；CI 外环结果为准"
            }))
        })();
        match result {
            Ok(r) => ToolEnvelope::ok(r),
            Err(e) => tool_err(format!("form_check failed (fail-open): {e}")),
        }
    }

    /// Record the agent's self-reported action for an advisory (M1 feedback
    /// loop: accepted/ignored/dismissed).
    #[tool(
        description = "Record the agent's self-reported action for an advisory (M1 feedback loop): accepted | ignored | dismissed."
    )]
    fn spot_action(&self, Parameters(p): Parameters<SpotActionParams>) -> String {
        let repo = resolve_repo(p.repo);
        let result = (|| -> anyhow::Result<()> {
            let store = Store::open(&Store::default_path(&repo))?;
            store.set_agent_action(&p.advisory_id, &p.action)?;
            Ok(())
        })();
        match result {
            Ok(()) => ToolEnvelope::ok(serde_json::json!({"recorded": true})),
            Err(e) => tool_err(format!("spot_action failed: {e}")),
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_target(false)
        .init();

    let service = WardMcp.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}

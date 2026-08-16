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
use serde::Deserialize;
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
    /// Optional proposed signature in any supported language (Rust, Kotlin,
    /// Swift, Java, ObjC). Fingerprint evidence requires it.
    pub proposed_signature: Option<String>,
    /// Optional already-written body — enables block-level fingerprint
    /// matches (the PostToolUse flow).
    pub proposed_body: Option<String>,
    /// Signature language override ("rust|kotlin|swift|java|objc");
    /// auto-detected from the snippet when omitted.
    pub language: Option<String>,
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
    /// Add an anchor-validated LLM narration section (requires
    /// WARD_LLM_URL; failures fall back to the structured list).
    pub narrate: Option<bool>,
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
pub struct CompatCheckParams {
    /// Baseline revision (default HEAD^).
    pub base: Option<String>,
    pub repo: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct IntentCheckParams {
    /// The original user requirement, in natural language.
    pub requirement: String,
    pub base: Option<String>,
    pub head: Option<String>,
    pub repo: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct CardParams {
    /// Symbol name or path:line.
    pub query: String,
    pub repo: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ClustersParams {
    /// Similarity threshold (default 0.92).
    pub threshold: Option<f64>,
    pub repo: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct SpotActionParams {
    pub advisory_id: String,
    /// accepted | ignored | dismissed
    pub action: String,
    pub repo: Option<String>,
}

/// Serialized wrapper shared with the CLI (issue #4): both surfaces emit
/// the same `{ok, data}` envelope.
type ToolEnvelope<T> = ward_core::envelope::Envelope<T>;

fn tool_ok<T: serde::Serialize>(data: T) -> String {
    let envelope = ToolEnvelope::ok(data);
    serde_json::to_string_pretty(&envelope)
        .unwrap_or_else(|_| r#"{"ok":false,"error":"serialize failed"}"#.into())
}

fn tool_err(e: impl std::fmt::Display) -> String {
    serde_json::to_string(&ToolEnvelope::<serde_json::Value>::err(e))
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
            let lang = p
                .language
                .as_deref()
                .and_then(ward_core::lang::Language::from_name);
            ward_core::search::spot(
                &repo,
                &store,
                &cfg,
                &p.intent,
                p.proposed_signature.as_deref(),
                p.proposed_body.as_deref(),
                lang,
            )
        })();
        match result {
            Ok(r) => tool_ok(r),
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
            let report = ward_core::diff::replay(&repo, &store, &cfg, &p.base, &p.head)?;
            if p.narrate.unwrap_or(false) {
                let provider = ward_core::llm::http_llm_from_env();
                Ok(serde_json::json!({
                    "report": report,
                    "narrative": ward_core::narrate::narrate(&report, provider.as_deref()),
                }))
            } else {
                Ok(serde_json::json!(report))
            }
        })();
        match result {
            Ok(r) => tool_ok(r),
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
        tool_ok(report)
    }

    /// Outer-loop sandbox adjudication — only meaningful on machines with
    /// Docker; otherwise verdict is `unknown` (F13), never a fake pass.
    #[tool(
        description = "Outer-loop sandbox adjudication (M3, Docker required). Without a sandbox the verdict is 'unknown' — never a fake green."
    )]
    fn verify_full(&self, Parameters(p): Parameters<CatchRunParams>) -> String {
        let repo = resolve_repo(p.repo);
        let cfg = load_config(&repo);
        tool_ok(verify_full(&repo, &cfg))
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
            Ok(r) => tool_ok(r),
            Err(e) => tool_err(format!("form_check failed (fail-open): {e}")),
        }
    }

    /// API/ABI compatibility adjudication (M4 outer loop). Rust uses
    /// cargo-semver-checks; other languages report unknown honestly.
    #[tool(
        description = "API/ABI compatibility adjudication against a base rev (M4). Rust uses cargo-semver-checks; other languages report unknown."
    )]
    fn compat_check(&self, Parameters(p): Parameters<CompatCheckParams>) -> String {
        let repo = resolve_repo(p.repo);
        tool_ok(ward_core::compat::api_compat_check(
            &repo,
            &p.base.unwrap_or_else(|| "HEAD^".into()),
        ))
    }

    /// Soft intent-drift comparison (M4-b, LLM partition; not executed
    /// without WARD_LLM_URL).
    #[tool(
        description = "Soft intent-drift comparison (M4-b): original requirement vs deterministic change facts. LLM judgment, advisory only; reports 'not executed' without a provider."
    )]
    fn intent_check(&self, Parameters(p): Parameters<IntentCheckParams>) -> String {
        let repo = resolve_repo(p.repo);
        let cfg = load_config(&repo);
        let provider = ward_core::llm::http_llm_from_env();
        let result = (|| -> anyhow::Result<_> {
            let store = Store::open(&Store::default_path(&repo))?;
            let head = p
                .head
                .clone()
                .or_else(|| ward_core::git::head_sha(&repo).ok().flatten())
                .unwrap_or_else(|| "uncommitted".into());
            ward_core::intent::intent_drift_check(
                &repo,
                &store,
                &cfg,
                &p.requirement,
                &p.base.clone().unwrap_or_else(|| "HEAD^".into()),
                &head,
                provider.as_deref(),
            )
        })();
        match result {
            Ok(r) => tool_ok(r),
            Err(e) => tool_err(format!("intent_check failed (fail-open): {e}")),
        }
    }

    /// One-page context card for a symbol (M5): definition, callers (lower
    /// bound), related tests and config references.
    #[tool(
        description = "One-page context card for a symbol (M5): definition, callers, related tests, config references."
    )]
    fn context_card(&self, Parameters(p): Parameters<CardParams>) -> String {
        let repo = resolve_repo(p.repo);
        let cfg = load_config(&repo);
        let result = (|| -> anyhow::Result<_> {
            let store = Store::open(&Store::default_path(&repo))?;
            ward_core::context::context_card(&repo, &store, &cfg, &p.query)
        })();
        match result {
            Ok(r) => tool_ok(r),
            Err(e) => tool_err(format!("context_card failed (fail-open): {e}")),
        }
    }

    /// Offline duplicate clustering for the consolidation workflow (M6).
    #[tool(
        description = "Offline duplicate clustering (M6): union-find over simhash similarity with chunked bucketing; returns clusters and consolidation suggestions."
    )]
    fn clusters(&self, Parameters(p): Parameters<ClustersParams>) -> String {
        let repo = resolve_repo(p.repo);
        let result = (|| -> anyhow::Result<_> {
            let store = Store::open(&Store::default_path(&repo))?;
            ward_core::cluster::cluster_duplicates(&store, p.threshold.unwrap_or(0.92))
        })();
        match result {
            Ok(r) => tool_ok(r),
            Err(e) => tool_err(format!("clusters failed (fail-open): {e}")),
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
            Ok(()) => tool_ok(serde_json::json!({"recorded": true})),
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

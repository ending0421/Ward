//! M4-b — soft intent-drift comparison (spec §3-M4.6): the original user
//! requirement vs the deterministic change facts.
//!
//! Partition discipline: this is ALWAYS an LLM judgment — soft, advisory,
//! never intercepting — and is labeled as such in every output. Without a
//! provider the check honestly reports "not executed"; it never fabricates
//! a hint from nothing.

use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::config::WardConfig;
use crate::diff;
use crate::narrate::LlmProvider;
use crate::store::Store;

/// The soft drift hint (LLM partition, spec §3-M4.6).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriftHint {
    /// True when an LLM provider actually ran.
    pub executed: bool,
    /// The partition label — always `llm_soft` here; deterministic assertions
    /// are reported separately by `form_check` (spec: partitioned display).
    pub partition: String,
    /// Soft hints; empty when not executed or nothing to say.
    pub hints: Vec<String>,
    pub note: String,
}

/// Compare the original requirement with the change facts.
pub fn intent_drift_check(
    repo: &Path,
    store: &Store,
    config: &WardConfig,
    requirement: &str,
    base: &str,
    head: &str,
    provider: Option<&dyn LlmProvider>,
) -> Result<DriftHint> {
    let report = diff::replay(repo, store, config, base, head)?;
    let facts = serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".into());
    let Some(p) = provider else {
        return Ok(DriftHint {
            executed: false,
            partition: "llm_soft".into(),
            hints: Vec::new(),
            note: "未配置 LLM provider（WARD_LLM_URL）；M4-b 未执行，仅确定性断言生效".into(),
        });
    };

    let prompt = format!(
        "你是意图漂移检查器。下面是用户原始需求，以及本次变更的确定性事实（JSON）。\n\n\
         原始需求：\n{requirement}\n\n\
         变更事实：\n{facts}\n\n\
         请输出不超过 5 条软性意图偏离提示（每条一行，以 - 开头）。规则：只提示、不拦截；\
         不得声称测试通过或行为正确；若需求与变更一致，输出空。"
    );
    match p.complete(&prompt) {
        Ok(raw) => {
            let hints: Vec<String> = raw
                .lines()
                .map(|l| l.trim().trim_start_matches('-').trim().to_string())
                .filter(|l| !l.is_empty())
                .collect();
            Ok(DriftHint {
                executed: true,
                partition: "llm_soft".into(),
                hints,
                note: "LLM 软性判断（M4-b），只提示不拦截；确定性断言以 form_check 为准".into(),
            })
        }
        Err(e) => Ok(DriftHint {
            executed: false,
            partition: "llm_soft".into(),
            hints: Vec::new(),
            note: format!("LLM provider 失败（{e}）；M4-b 未执行"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::narrate::ScriptedProvider;

    fn setup() -> (tempfile::TempDir, Store, String, String) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), "pub fn f() { 1 }\n").unwrap();
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
        let base = String::from_utf8(
            std::process::Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();
        std::fs::write(dir.path().join("src/lib.rs"), "pub fn f() { 2 }\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "c2"]);
        let head = String::from_utf8(
            std::process::Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();
        let store = Store::open(&Store::default_path(dir.path())).unwrap();
        (dir, store, base, head)
    }

    #[test]
    fn no_provider_reports_not_executed() {
        let (dir, store, base, head) = setup();
        let hint = intent_drift_check(
            dir.path(),
            &store,
            &WardConfig::default(),
            "实现防抖",
            &base,
            &head,
            None,
        )
        .unwrap();
        assert!(!hint.executed);
        assert_eq!(hint.partition, "llm_soft");
        assert!(hint.hints.is_empty());
    }

    #[test]
    fn provider_hints_are_labeled_soft() {
        let (dir, store, base, head) = setup();
        let p = ScriptedProvider {
            reply: "- 变更与需求一致\n- 未发现偏离\n".into(),
        };
        let hint = intent_drift_check(
            dir.path(),
            &store,
            &WardConfig::default(),
            "实现防抖",
            &base,
            &head,
            Some(&p),
        )
        .unwrap();
        assert!(hint.executed);
        assert_eq!(hint.hints, vec!["变更与需求一致", "未发现偏离"]);
    }

    #[test]
    fn failing_provider_reports_not_executed() {
        struct Fail;
        impl LlmProvider for Fail {
            fn complete(&self, _p: &str) -> anyhow::Result<String> {
                anyhow::bail!("down")
            }
        }
        let (dir, store, base, head) = setup();
        let hint = intent_drift_check(
            dir.path(),
            &store,
            &WardConfig::default(),
            "x",
            &base,
            &head,
            Some(&Fail),
        )
        .unwrap();
        assert!(!hint.executed);
        assert!(hint.note.contains("down"));
    }
}

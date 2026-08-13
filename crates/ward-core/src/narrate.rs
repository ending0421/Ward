//! M2 narration layer (spec §3-M2): LLM text is allowed to *narrate* facts,
//! never to invent them.
//!
//! Two hard rules implemented here:
//! 1. **Slot binding** — the LLM only fills slots driven by deterministic
//!    facts (change list, risk markers); it never receives free rein.
//! 2. **Per-sentence anchor validation** — every sentence the LLM produces
//!    must contain a real `path:line` anchor from the report; sentences that
//!    don't are deleted, not rewritten (spec §3-M2 / F6).
//!
//! With no provider configured — the default — output is the pure structured
//! fallback (F6), which is the honest anti-hallucination posture.

use serde::{Deserialize, Serialize};

use crate::diff::ReplayReport;

/// Minimal LLM provider interface (an HTTP provider is wired by the CLI/MCP
/// layer from environment configuration; tests use a mock).
pub trait LlmProvider {
    fn complete(&self, prompt: &str) -> anyhow::Result<String>;
}

/// Sentences = split on Chinese full stops, newlines, and English periods
/// that are *followed by whitespace or end-of-text*. A period inside a
/// `path.rs`-style anchor never terminates a sentence.
pub fn split_sentences(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut cur = String::new();
    for (i, &c) in chars.iter().enumerate() {
        let next = chars.get(i + 1).copied();
        let ends_sentence =
            c == '。' || c == '\n' || (c == '.' && next.is_none_or(|n| n.is_whitespace()));
        if ends_sentence {
            let t = cur.trim();
            if !t.is_empty() {
                out.push(t.to_string());
            }
            cur.clear();
        } else {
            cur.push(c);
        }
    }
    let t = cur.trim();
    if !t.is_empty() {
        out.push(t.to_string());
    }
    out
}

/// Keep only sentences containing at least one real anchor (`path:line`
/// present in the report). This is the spec's per-sentence anchor validator:
/// misses are deleted, never rewritten.
pub fn validate_anchors(text: &str, anchors: &[String]) -> Vec<String> {
    split_sentences(text)
        .into_iter()
        .filter(|sentence| {
            anchors
                .iter()
                .any(|a| !a.is_empty() && sentence.contains(a.as_str()))
        })
        .collect()
}

/// Every `path:line` anchor present in a report.
pub fn report_anchors(report: &ReplayReport) -> Vec<String> {
    let mut out: Vec<String> = report
        .changes
        .iter()
        .map(|c| format!("{}:{}", c.path, c.lines))
        .collect();
    out.extend(report.risks.iter().flat_map(|r| r.anchors.iter().cloned()));
    out.sort();
    out.dedup();
    out
}

/// The narration prompt: facts only, slots only.
pub fn build_prompt(report: &ReplayReport) -> String {
    let facts = serde_json::to_string(&serde_json::json!({
        "changes": report.changes,
        "risks": report.risks,
        "impact": report.impact,
    }))
    .unwrap_or_else(|_| "{}".into());
    format!(
        "以下是本次变更的确定性事实（JSON）。请用中文为审阅者写一段不超过 5 句话的摘要。\
         硬性规则：每一句话必须包含事实中出现的 path:line 锚点（例如 src/lib.rs:10）；\
         不得陈述事实之外的内容；不得声称测试通过或行为等价。\n\n{facts}"
    )
}

/// Render the final summary: deterministic sections always; LLM narration
/// only when a provider is configured and its output survives anchor
/// validation. Provider failure degrades to the structured fallback (F6).
pub fn narrate(report: &ReplayReport, provider: Option<&dyn LlmProvider>) -> String {
    let mut out = crate::diff::render_markdown(report);
    let Some(p) = provider else {
        out.push_str("\n> 叙述层未配置（结构化回退，F6）\n");
        return out;
    };
    let anchors = report_anchors(report);
    match p.complete(&build_prompt(report)) {
        Ok(raw) => {
            let kept = validate_anchors(&raw, &anchors);
            out.push_str("\n## 摘要叙述（LLM，锚定校验后）\n\n");
            if kept.is_empty() {
                out.push_str("> LLM 叙述未通过锚点校验，已回退为纯结构化清单（F6）。\n");
            } else {
                for sentence in kept {
                    out.push_str(&format!("- {sentence}\n"));
                }
            }
        }
        Err(e) => {
            out.push_str(&format!(
                "\n> LLM 叙述失败（{e}），已回退为纯结构化清单（F6）。\n"
            ));
        }
    }
    out
}

/// A mock provider for tests and local demos (deterministic).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptedProvider {
    pub reply: String,
}

impl LlmProvider for ScriptedProvider {
    fn complete(&self, _prompt: &str) -> anyhow::Result<String> {
        Ok(self.reply.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::{ChangeKind, RiskMarker, SymbolChange};

    fn report() -> ReplayReport {
        ReplayReport {
            base: "a".into(),
            head: "b".into(),
            changes: vec![SymbolChange {
                path: "src/lib.rs".into(),
                lines: "10-14".into(),
                name: "debounce".into(),
                kind_of: "function_item".into(),
                change: ChangeKind::SignatureChanged,
                moved_from: None,
                public: true,
            }],
            risks: vec![RiskMarker {
                severity: "high".into(),
                description: "公共 API 签名变更".into(),
                anchors: vec!["src/lib.rs:10-14".into()],
            }],
            impact: Default::default(),
        }
    }

    #[test]
    fn sentence_splitter_handles_mixed_punctuation() {
        let sentences = split_sentences("第一句。second sentence.\n第三句");
        assert_eq!(sentences.len(), 3);
    }

    #[test]
    fn anchor_validator_keeps_only_anchored_sentences() {
        let anchors = vec!["src/lib.rs:10-14".to_string()];
        let kept = validate_anchors(
            "这句话有锚点 src/lib.rs:10-14 是合法的。\n这句话没有锚点，必须删除。",
            &anchors,
        );
        assert_eq!(kept.len(), 1);
        assert!(kept[0].contains("src/lib.rs:10-14"));
    }

    #[test]
    fn anchor_validator_handles_empty_anchors() {
        // No anchors → no sentence may survive (nothing to be anchored to).
        let kept = validate_anchors("没有任何锚点的话 src/x.rs:9。", &[]);
        assert!(kept.is_empty());
    }

    #[test]
    fn no_provider_uses_structured_fallback() {
        let out = narrate(&report(), None);
        assert!(out.contains("结构化回退"));
        assert!(out.contains("签名变更"));
    }

    #[test]
    fn provider_output_is_filtered_by_anchors() {
        let p = ScriptedProvider {
            reply: "改了 src/lib.rs:10-14 的签名。\n这句没有锚点会被删。".into(),
        };
        let out = narrate(&report(), Some(&p));
        assert!(out.contains("改了 src/lib.rs:10-14 的签名"));
        assert!(!out.contains("这句没有锚点会被删"));
    }

    #[test]
    fn failing_provider_falls_back() {
        struct Fail;
        impl LlmProvider for Fail {
            fn complete(&self, _p: &str) -> anyhow::Result<String> {
                anyhow::bail!("llm down")
            }
        }
        let out = narrate(&report(), Some(&Fail));
        assert!(out.contains("回退"));
        assert!(out.contains("llm down"));
    }

    #[test]
    fn prompt_contains_only_facts_and_rules() {
        let prompt = build_prompt(&report());
        assert!(prompt.contains("debounce"));
        assert!(prompt.contains("path:line 锚点"));
        // The prompt forbids claim fabrication explicitly.
        assert!(prompt.contains("不得声称测试通过"));
    }
}

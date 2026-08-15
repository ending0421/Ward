//! Commit attribution (spec §9 归因纪律): is a commit AI-authored or not?
//!
//! Dual-marker convention (AGENTS.md rule 7a):
//! * `[ai]` subject prefix (case-insensitive), or
//! * a `Co-authored-by:` trailer whose name hints at an AI tool
//!   (ai/claude/codex/deepseek/gpt/copilot/cursor/agent, case-insensitive).
//!
//! No marker ⇒ `unknown` — the governance report never guesses.

/// AI commit detection. Marker-based only, never heuristic.
pub fn is_ai_commit(message: &str) -> bool {
    let subject = message
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if subject.starts_with("[ai]") || subject.starts_with("[ai ") {
        return true;
    }
    const AI_HINTS: &[&str] = &[
        "claude",
        "codex",
        "deepseek",
        "gpt",
        "copilot",
        "cursor",
        "agent",
        "anthropic",
        "openai",
        "noreply",
        "assistant",
    ];
    message.lines().any(|line| {
        let lower = line.to_ascii_lowercase();
        if !lower.starts_with("co-authored-by:") {
            return false;
        }
        // "ai" matches only as a standalone word — "gmail" must not fire.
        let words: std::collections::HashSet<&str> = lower
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .collect();
        words.contains("ai") || AI_HINTS.iter().any(|hint| lower.contains(hint))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ai_prefix_detected_case_insensitive() {
        assert!(is_ai_commit("[ai] feat: add infer"));
        assert!(is_ai_commit("[AI] fix: typo"));
        assert!(is_ai_commit("[ai]fix without space"));
    }

    #[test]
    fn ai_trailer_detected() {
        assert!(is_ai_commit("feat: x\n\nCo-authored-by: AI <ai@ward.dev>"));
        assert!(is_ai_commit(
            "feat: x\nCo-authored-by: Claude <noreply@anthropic.com>"
        ));
        assert!(is_ai_commit(
            "feat: x\nCo-authored-by: OpenAI Codex <codex@openai.com>"
        ));
    }

    #[test]
    fn human_commits_are_not_ai() {
        assert!(!is_ai_commit("fix: manual edit by karl"));
        assert!(!is_ai_commit(
            "feat: x\nCo-authored-by: Karl.Lyu <karl.lv.0421@gmail.com>"
        ));
        assert!(!is_ai_commit(""));
    }

    #[test]
    fn trailer_with_unrelated_name_is_not_ai() {
        assert!(!is_ai_commit(
            "feat: x\nCo-authored-by: Alice <alice@corp.com>"
        ));
    }

    #[test]
    fn prefix_wins_even_with_human_trailer() {
        assert!(is_ai_commit(
            "[ai] feat: x\nCo-authored-by: Bob <bob@corp.com>"
        ));
    }
}

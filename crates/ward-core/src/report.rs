//! Single-advisory context dump (`ward report <id>`) — the reproduction
//! half of the remote-feedback story: everything the fixer needs about one
//! advisory, with source snippets strictly opt-in.

use serde::{Deserialize, Serialize};

use crate::store::{Label, Store};

/// Full context for one advisory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdvisoryDetail {
    pub id: String,
    pub ts: i64,
    pub query: Option<String>,
    pub matches: Vec<crate::search::SpotMatch>,
    pub agent_action: Option<String>,
    pub inferred_action: Option<String>,
    pub inferred_commit_sha: Option<String>,
    pub labels: Vec<Label>,
    /// Source snippets per match — only present when explicitly requested.
    pub snippets: Option<Vec<Option<String>>>,
}

/// Assemble the advisory detail. Snippets are opt-in.
pub fn advisory_report(store: &Store, id: &str) -> anyhow::Result<Option<AdvisoryDetail>> {
    let Some(row) = store.advisory_row(id)? else {
        return Ok(None);
    };
    let (ts, _query_hash, result_json, agent_action, inferred_action, inferred_sha) = row;
    let parsed = crate::search::parse_spot_payload(&result_json);
    let query = parsed.as_ref().and_then(|p| p.query.clone());
    let matches = parsed.map(|p| p.matches).unwrap_or_default();
    let labels = store.labels_for_advisory(id)?;
    Ok(Some(AdvisoryDetail {
        id: id.to_string(),
        ts,
        query,
        matches,
        agent_action,
        inferred_action,
        inferred_commit_sha: inferred_sha,
        labels,
        snippets: None,
    }))
}

/// Attach opt-in snippets to a detail (the `--include-snippets` path).
pub fn with_snippets(mut detail: AdvisoryDetail, repo: &std::path::Path) -> AdvisoryDetail {
    let snippets = detail
        .matches
        .iter()
        .map(|m| crate::label::snippet_of(repo, &m.path, &m.lines))
        .collect();
    detail.snippets = Some(snippets);
    detail
}

#[cfg(test)]
mod tests {

    #[test]
    fn snippet_of_reads_around_the_hit_from_repo_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/lib.rs"),
            "fn a() {}\nfn b() {}\nfn hit() {}\nfn c() {}\n",
        )
        .unwrap();
        // Resolved against the repo root, NOT the process cwd — the report
        // must be correct even when `--repo` points elsewhere.
        let snip = crate::label::snippet_of(dir.path(), "src/lib.rs", "3").unwrap();
        assert!(snip.contains("fn hit"), "snippet: {snip}");
    }
}

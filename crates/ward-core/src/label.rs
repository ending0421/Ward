//! Golden-set labeling (spec §9): match-level human verdicts that feed
//! threshold calibration. Labels are the ONLY data calibration trusts —
//! agent dismissals are reported separately and never treated as truth.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::search::{SpotMatch, SpotResult};
use crate::store::{Label, Store};

/// One labelable match, resolved from an advisory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LabelCandidate {
    pub advisory_id: String,
    pub match_index: i64,
    pub query: Option<String>,
    pub language: Option<String>,
    pub kind: String,
    pub similarity: f64,
    pub path: String,
    pub lines: String,
    pub symbol: String,
    /// The matched code snippet from the current working tree (best effort).
    pub snippet: Option<String>,
}

/// Decode advisory matches into label candidates.
pub fn candidates(store: &Store, limit: usize) -> Result<Vec<LabelCandidate>> {
    let mut out = Vec::new();
    let pending = store.pending_inferences()?;
    // Also include already-inferred advisories: inference is about adoption,
    // labeling is about match quality — both apply to the same matches.
    let all: Vec<(String, i64, String)> = {
        let mut rows = store.advisory_payloads()?;
        // pending_inferences is a subset of payloads; extend only unseen ids.
        {
            let seen: std::collections::HashSet<String> =
                rows.iter().map(|r| r.0.clone()).collect();
            rows.extend(pending.into_iter().filter(|r| !seen.contains(&r.0)));
        }
        rows
    };
    for (id, _ts, result_json) in all {
        let Some(parsed) = crate::search::parse_spot_payload(&result_json) else {
            continue;
        };
        for (i, m) in parsed.matches.iter().enumerate() {
            if store.is_labeled(&id, i as i64)? {
                continue;
            }
            out.push(candidate_of(&id, i as i64, &parsed, m));
            if out.len() >= limit {
                return Ok(out);
            }
        }
    }
    Ok(out)
}

fn candidate_of(advisory_id: &str, idx: i64, parsed: &SpotResult, m: &SpotMatch) -> LabelCandidate {
    LabelCandidate {
        advisory_id: advisory_id.to_string(),
        match_index: idx,
        query: parsed.query.clone(),
        language: None,
        kind: m.kind.clone(),
        similarity: m.similarity,
        path: m.path.clone(),
        lines: m.lines.clone(),
        symbol: m.symbol.clone(),
        snippet: snippet_of(&m.path, &m.lines),
    }
}

/// Best-effort snippet: read the file and cut ±6 lines around the hit.
fn snippet_of(path: &str, lines: &str) -> Option<String> {
    let start: usize = lines.split('-').next()?.parse().ok()?;
    let content = std::fs::read_to_string(path).ok()?;
    let all: Vec<&str> = content.lines().collect();
    let lo = start.saturating_sub(7);
    let hi = (start + 5).min(all.len());
    if lo >= hi {
        return None;
    }
    Some(
        all[lo..hi]
            .iter()
            .enumerate()
            .map(|(i, l)| format!("{:>4} {}", lo + i + 1, l))
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

/// Record a verdict for one match. The match's (kind, similarity) are
/// resolved from the advisory payload so calibration can bucket verdicts.
pub fn label_match(
    store: &Store,
    advisory_id: &str,
    match_index: i64,
    verdict: &str,
) -> Result<()> {
    anyhow::ensure!(
        matches!(verdict, "y" | "n"),
        "verdict must be y or n, got {verdict}"
    );
    let (kind, similarity) = store
        .advisory_payloads()?
        .into_iter()
        .find(|(id, _, _)| id == advisory_id)
        .and_then(|(_, _, json)| {
            let parsed = crate::search::parse_spot_payload(&json)?;
            parsed
                .matches
                .get(match_index as usize)
                .map(|m| (Some(m.kind.clone()), Some(m.similarity)))
        })
        .unwrap_or((None, None));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default();
    store
        .record_label(&Label {
            id: None,
            advisory_id: advisory_id.to_string(),
            match_index,
            query_hash: None,
            language: None,
            kind,
            similarity,
            verdict: verdict.to_string(),
            ts: now,
        })
        .with_context(|| format!("recording label for {advisory_id}#{match_index}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Advisory;

    fn seed(store: &Store) {
        store
            .record_advisory(&Advisory {
                id: "adv_1".into(),
                tool: "spot".into(),
                ts: 1,
                query_hash: "q".into(),
                result_json: r#"{"as_of":null,"stale":false,"query":"防抖函数","matches":[{"path":"src/lib.rs","lines":"10-14","symbol":"debounce","similarity":0.94,"kind":"near","note":""}],"advisory_id":"adv_1"}"#.into(),
                ..Default::default()
            })
            .unwrap();
    }

    #[test]
    fn candidates_expose_unlabeled_matches_with_query() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("index.db")).unwrap();
        seed(&store);
        let cands = candidates(&store, 10).unwrap();
        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0].query.as_deref(), Some("防抖函数"));
        assert_eq!(cands[0].kind, "near");
        assert_eq!(cands[0].similarity, 0.94);
    }

    #[test]
    fn labeling_a_match_excludes_it_from_candidates() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("index.db")).unwrap();
        seed(&store);
        label_match(&store, "adv_1", 0, "y").unwrap();
        assert!(candidates(&store, 10).unwrap().is_empty());
        assert_eq!(store.label_count().unwrap(), 1);
    }

    #[test]
    fn invalid_verdict_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("index.db")).unwrap();
        seed(&store);
        assert!(label_match(&store, "adv_1", 0, "maybe").is_err());
        assert!(label_match(&store, "adv_1", 0, "n").is_ok());
    }
}

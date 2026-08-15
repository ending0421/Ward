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
pub fn candidates(
    store: &Store,
    repo: &std::path::Path,
    limit: usize,
) -> Result<Vec<LabelCandidate>> {
    candidates_by(store, repo, limit, "human")
}

/// Same as [`candidates`], but only matches the given annotator has NOT
/// labeled yet — double-annotation means each annotator sees the full
/// unlabeled-for-them queue.
pub fn candidates_by(
    store: &Store,
    repo: &std::path::Path,
    limit: usize,
    annotator: &str,
) -> Result<Vec<LabelCandidate>> {
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
            if store.is_labeled_by(&id, i as i64, annotator)? {
                continue;
            }
            out.push(candidate_of(&id, i as i64, &parsed, m, repo));
            if out.len() >= limit {
                return Ok(out);
            }
        }
    }
    Ok(out)
}

fn candidate_of(
    advisory_id: &str,
    idx: i64,
    parsed: &SpotResult,
    m: &SpotMatch,
    repo: &std::path::Path,
) -> LabelCandidate {
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
        snippet: snippet_of(repo, &m.path, &m.lines),
    }
}

/// Best-effort snippet: read the file and cut ±6 lines around the hit.
pub fn snippet_of(repo: &std::path::Path, path: &str, lines: &str) -> Option<String> {
    let start: usize = lines.split('-').next()?.parse().ok()?;
    let content = std::fs::read_to_string(repo.join(path)).ok()?;
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
    label_match_by(store, advisory_id, match_index, verdict, "human")
}

/// [`label_match`] with an explicit annotator (double-annotation, spec §8).
pub fn label_match_by(
    store: &Store,
    advisory_id: &str,
    match_index: i64,
    verdict: &str,
    annotator: &str,
) -> Result<()> {
    anyhow::ensure!(
        matches!(verdict, "y" | "n"),
        "verdict must be y or n, got {verdict}"
    );
    anyhow::ensure!(
        !annotator.trim().is_empty(),
        "annotator must be a non-empty name"
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
            annotator: annotator.to_string(),
            query_hash: None,
            language: None,
            kind,
            similarity,
            verdict: verdict.to_string(),
            ts: now,
        })
        .with_context(|| format!("recording label for {advisory_id}#{match_index} by {annotator}"))
}

/// Inter-annotator agreement over the golden set (spec §8 标注腐烂护栏).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgreementReport {
    /// Matches carrying ≥2 annotator labels (the agreement sample).
    pub double_labeled: usize,
    /// Fleiss' kappa over y/n verdicts. `None` when the sample is too small
    /// or degenerate (all raters agree on everything → kappa undefined).
    pub fleiss_kappa: Option<f64>,
    /// Label counts per annotator.
    pub per_annotator: Vec<(String, usize)>,
}

/// Fleiss' kappa for binary y/n verdicts over matches with ≥2 raters.
pub fn annotator_agreement(store: &Store) -> Result<AgreementReport> {
    let labels = store.labels_all()?;
    let mut per_annotator: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();
    let mut per_match: std::collections::BTreeMap<(String, i64), (usize, usize)> =
        std::collections::BTreeMap::new();
    for l in &labels {
        *per_annotator.entry(l.annotator.clone()).or_insert(0) += 1;
        let key = (l.advisory_id.clone(), l.match_index);
        let entry = per_match.entry(key).or_insert((0, 0));
        if l.verdict == "y" {
            entry.0 += 1;
        } else if l.verdict == "n" {
            entry.1 += 1;
        }
    }
    let double: Vec<(usize, usize)> = per_match
        .values()
        .filter(|(ny, nn)| ny + nn >= 2)
        .copied()
        .collect();
    let kappa = fleiss_kappa(&double);
    Ok(AgreementReport {
        double_labeled: double.len(),
        fleiss_kappa: kappa,
        per_annotator: per_annotator.into_iter().collect(),
    })
}

/// Fleiss' kappa for binary ratings. Each row is (n_yes, n_no) per match.
/// Returns `None` for empty/degenerate samples (P_e == 1).
fn fleiss_kappa(rows: &[(usize, usize)]) -> Option<f64> {
    let n_total: usize = rows.iter().map(|(y, n)| y + n).sum();
    if n_total == 0 {
        return None;
    }
    let mut p_bar = 0.0;
    for &(ny, nn) in rows {
        let n = ny + nn;
        if n < 2 {
            continue; // single-rater rows cannot contribute agreement
        }
        p_bar += ((ny * ny.saturating_sub(1)) + (nn * nn.saturating_sub(1))) as f64
            / (n * (n - 1)) as f64;
    }
    let m = rows.iter().filter(|(y, n)| y + n >= 2).count();
    if m == 0 {
        return None;
    }
    p_bar /= m as f64;
    let n_yes: usize = rows.iter().map(|(y, _)| y).sum();
    let p_yes = n_yes as f64 / n_total as f64;
    let p_no = 1.0 - p_yes;
    let p_e = p_yes * p_yes + p_no * p_no;
    if p_e >= 1.0 {
        return None; // degenerate: everyone says the same thing
    }
    Some((p_bar - p_e) / (1.0 - p_e))
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
        let cands = candidates(&store, dir.path(), 10).unwrap();
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
        assert!(candidates(&store, dir.path(), 10).unwrap().is_empty());
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

    #[test]
    fn annotators_see_their_own_unlabeled_queue() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("index.db")).unwrap();
        seed(&store);
        label_match_by(&store, "adv_1", 0, "y", "alice").unwrap();
        // Alice labeled it → alice's queue is empty, bob's still has it.
        assert!(
            candidates_by(&store, dir.path(), 10, "alice")
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            candidates_by(&store, dir.path(), 10, "bob").unwrap().len(),
            1
        );
        // The default annotator ("human") is distinct from both.
        assert_eq!(candidates(&store, dir.path(), 10).unwrap().len(), 1);
    }

    #[test]
    fn empty_annotator_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("index.db")).unwrap();
        seed(&store);
        assert!(label_match_by(&store, "adv_1", 0, "y", "  ").is_err());
    }

    #[test]
    fn fleiss_kappa_handles_agreement_disagreement_and_degenerate() {
        // Perfect agreement → kappa 1.0.
        assert!((fleiss_kappa(&[(2, 0), (2, 0), (0, 2)]).unwrap() - 1.0).abs() < 1e-9);
        // Total disagreement → kappa −1.
        assert!((fleiss_kappa(&[(1, 1), (1, 1)]).unwrap() + 1.0).abs() < 1e-9);
        // Mixed sample (computed by hand: kappa = 7/15 ≈ 0.46667).
        let mixed = fleiss_kappa(&[(2, 0), (2, 0), (0, 2), (1, 1)]).unwrap();
        assert!(
            (mixed - 7.0 / 15.0).abs() < 1e-9,
            "expected 7/15, got {mixed}"
        );
        // Single-rater rows carry no agreement signal.
        assert!(fleiss_kappa(&[(1, 0), (0, 1)]).is_none());
        // Everyone says y everywhere: degenerate → None (no denominator).
        assert!(fleiss_kappa(&[(2, 0)]).is_none());
    }

    #[test]
    fn agreement_report_counts_double_labeled_and_annotators() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("index.db")).unwrap();
        seed(&store);
        label_match_by(&store, "adv_1", 0, "y", "alice").unwrap();
        label_match_by(&store, "adv_1", 0, "n", "bob").unwrap();
        let r = annotator_agreement(&store).unwrap();
        assert_eq!(r.double_labeled, 1);
        assert!(r.fleiss_kappa.is_some());
        assert!(
            (r.fleiss_kappa.unwrap() + 1.0).abs() < 1e-9,
            "y vs n → kappa −1"
        );
        let counts: std::collections::BTreeMap<&str, usize> = r
            .per_annotator
            .iter()
            .map(|(k, v)| (k.as_str(), *v))
            .collect();
        assert_eq!(counts.get("alice"), Some(&1));
        assert_eq!(counts.get("bob"), Some(&1));
    }
}

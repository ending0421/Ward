//! The outcome-inference channel (spec §3-M1): observe what the agent
//! actually did in the next commit, instead of trusting its self-report.
//!
//! Causal direction (spec §3-M1, fixed in v0.5.0):
//! ```
//! inferred_action =
//!   accepted   := next commit did NOT introduce a symbol similar to top-1
//!                 AND did add a call/mention edge to the top-1 symbol
//!   reused-ish := no similar symbol AND no call edge (adopt or pivot,
//!                 indistinguishable — reported separately)
//!   rejected   := next commit introduced a symbol similar to top-1
//!   unknown    := no later commit, or top-1 no longer resolvable
//! ```
//!
//! This is the objective adoption signal the governance report ranks first;
//! the self-reported channel is shown beside it with a divergence alarm.

use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::config::WardConfig;
use crate::fingerprint;
use crate::git;
use crate::index;
use crate::lang::{Language, RUST};
use crate::search::SpotResult;
use crate::store::Store;

/// Similarity above which a newly introduced symbol counts as "similar to
/// the top-1 match" (rejection evidence). Conservative: only near-identical
/// re-implementations count as rejections.
pub const REJECT_SIMILARITY: f64 = 0.80;

/// What one inference pass did.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct InferReport {
    pub considered: usize,
    pub accepted: usize,
    pub reused_ish: usize,
    pub rejected: usize,
    pub unknown: usize,
}

/// First commit whose committer timestamp is >= `ts` (unix seconds).
///
/// Inclusive on purpose: `git log --after` is strictly-after, which races
/// same-second commits; reading `%ct` and comparing ourselves is
/// deterministic. NOTE: commit timestamps have 1-second granularity — a
/// commit made in the same second as the advisory is indistinguishable from
/// a pre-advisory commit. Real workflows have minutes between query and
/// commit; tests must sleep >1s to model that.
fn first_commit_after(repo: &Path, ts: i64) -> Result<Option<String>> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["log", "--reverse", "--format=%H %ct"])
        .output()?;
    if !out.status.success() {
        return Ok(None);
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|line| {
            let mut parts = line.split_whitespace();
            let sha = parts.next()?;
            let ct: i64 = parts.next()?.parse().ok()?;
            (ct >= ts).then(|| sha.to_string())
        }))
}

/// Symbols introduced by `parent..commit` in Rust files.
fn added_symbols(repo: &Path, commit: &str) -> Vec<index::Extracted> {
    let parent = format!("{commit}^");
    let changed = git::diff_names(repo, &parent, commit).unwrap_or_default();
    let mut added = Vec::new();
    for path in changed {
        if Language::from_path(Path::new(&path)) != Some(Language::Rust) {
            continue;
        }
        let (Some(old_src), Some(new_src)) = (
            git::show_file(repo, &parent, &path).unwrap_or(None),
            git::show_file(repo, commit, &path).unwrap_or(None),
        ) else {
            continue;
        };
        let Some(old_tree) = fingerprint::parse_rust(&old_src) else {
            continue;
        };
        let Some(new_tree) = fingerprint::parse_rust(&new_src) else {
            continue;
        };
        let old_names: std::collections::BTreeSet<String> =
            index::extract_symbols(&old_tree, &old_src, &RUST)
                .into_iter()
                .map(|e| e.symbol.name)
                .collect();
        let new_syms = index::extract_symbols(&new_tree, &new_src, &RUST);
        for e in new_syms {
            if !old_names.contains(&e.symbol.name) {
                added.push(e);
            }
        }
    }
    added
}

/// Resolve the top-1 match of an advisory into the stored symbol (by
/// path+name at query time). `None` when the index no longer holds it.
fn top1_symbol(store: &Store, result_json: &str) -> Option<crate::store::Symbol> {
    let parsed: SpotResult = serde_json::from_str(result_json).ok()?;
    let top = parsed.matches.first()?.clone();
    store
        .all_symbols()
        .ok()?
        .into_iter()
        .find(|s| s.file_path == top.path && s.name == top.symbol)
}

/// Run inference for every advisory whose `inferred_action` is still NULL.
pub fn infer_pending(repo: &Path, store: &Store, config: &WardConfig) -> Result<InferReport> {
    let mut report = InferReport::default();
    let pending = store.pending_inferences()?;
    for (id, ts, result_json) in pending {
        report.considered += 1;
        let Some(commit) = first_commit_after(repo, ts)? else {
            report.unknown += 1;
            continue;
        };
        let Some(top1) = top1_symbol(store, &result_json) else {
            // Top-1 no longer in the index: cannot adjudicate objectively.
            store.set_inferred_action(&id, "unknown", &commit)?;
            report.unknown += 1;
            continue;
        };
        let added = added_symbols(repo, &commit);
        if added.is_empty() {
            // Nothing added at all: clearly no duplicate was introduced, and
            // a reuse would normally touch some file — treat as no-evidence.
            store.set_inferred_action(&id, "unknown", &commit)?;
            report.unknown += 1;
            continue;
        }
        let introduced_similar = added.iter().any(|e| {
            !config.is_suppressed(&e.symbol.file_path)
                && fingerprint::simhash_similarity(e.symbol.simhash, top1.simhash)
                    >= REJECT_SIMILARITY
        });
        if introduced_similar {
            store.set_inferred_action(&id, "rejected", &commit)?;
            report.rejected += 1;
            continue;
        }
        let called_top1 = added
            .iter()
            .any(|e| e.mentions.iter().any(|m| m == &top1.name));
        if called_top1 {
            store.set_inferred_action(&id, "accepted", &commit)?;
            report.accepted += 1;
        } else {
            store.set_inferred_action(&id, "reused-ish", &commit)?;
            report.reused_ish += 1;
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Advisory;

    fn repo_with_advice() -> (tempfile::TempDir, Store, String) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), "pub fn debounce() {}\n").unwrap();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}");
        };
        git(&["init", "-b", "master"]);
        git(&["config", "user.name", "t"]);
        git(&["config", "user.email", "t@e.c"]);
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "c1"]);
        let store = Store::open(&Store::default_path(dir.path())).unwrap();
        // The index must hold the advised symbols (top-1 resolution).
        crate::index::index_repo(dir.path(), &WardConfig::default()).unwrap();
        let sha = String::from_utf8(
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
        (dir, store, sha)
    }

    fn now_ts() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }

    fn seed_advisory(store: &Store, id: &str, ts: i64, result_json: &str) {
        store
            .record_advisory(&Advisory {
                id: id.into(),
                tool: "spot".into(),
                ts,
                query_hash: "q".into(),
                result_json: result_json.into(),
                ..Default::default()
            })
            .unwrap();
    }

    #[test]
    fn rejected_when_similar_symbol_introduced() {
        let (dir, store, _) = repo_with_advice();
        // Cross the 1-second timestamp boundary: the advisory must be
        // strictly after c1 (see first_commit_after docs).
        std::thread::sleep(std::time::Duration::from_millis(1100));
        seed_advisory(
            &store,
            "adv_1",
            now_ts(),
            r#"{"as_of":null,"stale":false,"matches":[{"path":"src/lib.rs","lines":"1","symbol":"debounce","similarity":0.94,"kind":"near","note":""}],"advisory_id":"adv_1"}"#,
        );
        std::fs::write(
            dir.path().join("src/lib.rs"),
            "pub fn debounce() {}\npub fn debounce_copy() {}\n",
        )
        .unwrap();
        // Commit timestamps are second-granular; cross into the next second
        // so c2 is strictly after the advisory (see first_commit_after docs).
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let git = std::process::Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["add", "-A"])
            .output()
            .unwrap();
        assert!(git.status.success());
        let git = std::process::Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["commit", "-q", "-m", "c2"])
            .output()
            .unwrap();
        assert!(git.status.success());
        let report = infer_pending(dir.path(), &store, &WardConfig::default()).unwrap();
        assert_eq!(report.rejected, 1, "{report:?}");
    }

    #[test]
    fn accepted_when_call_edge_added_without_duplicate() {
        let (dir, store, _) = repo_with_advice();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        seed_advisory(
            &store,
            "adv_2",
            now_ts(),
            r#"{"as_of":null,"stale":false,"matches":[{"path":"src/lib.rs","lines":"1","symbol":"debounce","similarity":0.94,"kind":"near","note":""}],"advisory_id":"adv_2"}"#,
        );
        std::fs::write(
            dir.path().join("src/lib.rs"),
            "pub fn debounce() {}\npub fn user() { debounce(); }\n",
        )
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let git = std::process::Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["add", "-A"])
            .output()
            .unwrap();
        assert!(git.status.success());
        let git = std::process::Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["commit", "-q", "-m", "c2"])
            .output()
            .unwrap();
        assert!(git.status.success());
        let report = infer_pending(dir.path(), &store, &WardConfig::default()).unwrap();
        assert_eq!(report.accepted, 1, "{report:?}");
    }

    #[test]
    fn no_later_commit_is_unknown() {
        let (dir, store, _) = repo_with_advice();
        seed_advisory(
            &store,
            "adv_3",
            1,
            r#"{"as_of":null,"stale":false,"matches":[{"path":"src/lib.rs","lines":"1","symbol":"debounce","similarity":0.94,"kind":"near","note":""}],"advisory_id":"adv_3"}"#,
        );
        let report = infer_pending(dir.path(), &store, &WardConfig::default()).unwrap();
        assert_eq!(report.unknown, 1, "{report:?}");
    }
}

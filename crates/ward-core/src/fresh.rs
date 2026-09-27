//! Per-file freshness protocol (spec §5).
//!
//! `stale = (index_sha != HEAD) ∨ (any hit file's indexed hash ≠ its current
//! hash)`. The second term is what catches the most common stale case: the
//! agent has uncommitted edits when the advisory is requested.

use std::path::Path;

use anyhow::Result;

use crate::git;
use crate::store::Store;

/// Freshness of an advisory relative to git and the working tree.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Freshness {
    /// The commit the index was built from (`None` when uncommitted).
    pub as_of: Option<String>,
    /// True when the index is stale against HEAD *or* any hit file.
    pub stale: bool,
}

/// Compute freshness for a set of hit file paths (main checkout only).
pub fn check(repo: &Path, store: &Store, hit_files: &[String]) -> Result<Freshness> {
    let scoped: Vec<(&str, &str)> = hit_files.iter().map(|f| (f.as_str(), "")).collect();
    check_scoped(repo, store, &scoped)
}

/// [`check`] with worktree provenance (issue #12): a hit stored from a linked
/// worktree is hashed at *its* root, and its freshness key is the scoped one —
/// otherwise the main checkout's copy of the same path would be compared
/// against worktree content and every advisory would look stale.
pub fn check_scoped(repo: &Path, store: &Store, hits: &[(&str, &str)]) -> Result<Freshness> {
    let as_of = store.last_indexed_sha()?;
    let head = git::head_sha(repo)?;
    let mut stale = match (&as_of, &head) {
        (Some(a), Some(h)) => a != h,
        _ => true, // never indexed, or no commits: nothing to be fresh against
    };
    if !stale {
        for (f, worktree) in hits {
            let root = if worktree.is_empty() {
                repo.to_path_buf()
            } else {
                std::path::PathBuf::from(worktree)
            };
            let current = git::file_hash(&root.join(f));
            let indexed = store.get_file_hash_scoped(worktree, f)?;
            if current != indexed {
                stale = true;
                break;
            }
        }
    }
    Ok(Freshness { as_of, stale })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn freshness_serializes_for_mcp() {
        let f = Freshness {
            as_of: Some("abc".into()),
            stale: false,
        };
        let json = serde_json::to_string(&f).unwrap();
        assert!(json.contains("\"stale\":false"));
    }
}

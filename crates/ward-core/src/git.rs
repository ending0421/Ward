//! Thin git plumbing — Ward shells out to the real git (law P1: git is the
//! only source of truth, so Ward *reads* it rather than reimplementing it).

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};

fn git(repo: &Path, args: &[&str]) -> Result<std::process::Output> {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .with_context(|| format!("git {}", args.join(" ")))
}

/// The current HEAD sha, if the repository has commits.
pub fn head_sha(repo: &Path) -> Result<Option<String>> {
    let out = git(repo, &["rev-parse", "--verify", "HEAD"])?;
    if out.status.success() {
        Ok(Some(
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
        ))
    } else {
        Ok(None)
    }
}

/// The content of `path` at `commit`, or `None` when it does not exist there.
pub fn show_file(repo: &Path, commit: &str, path: &str) -> Result<Option<String>> {
    let spec = format!("{commit}:{path}");
    let out = git(repo, &["show", &spec])?;
    if !out.status.success() {
        return Ok(None);
    }
    // `git show` writes the blob verbatim (no munging), so from_utf8_lossy is
    // acceptable for source files.
    Ok(Some(String::from_utf8_lossy(&out.stdout).into_owned()))
}

/// Paths that differ between two commits (exactly the files Replay cares
/// about).
pub fn diff_names(repo: &Path, base: &str, head: &str) -> Result<Vec<String>> {
    let out = git(repo, &["diff", "--name-only", base, head])?;
    if !out.status.success() {
        anyhow::bail!(
            "git diff {} {} failed: {}",
            base,
            head,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect())
}

/// blake3 of a file's content — the per-file freshness key (spec §5).
/// True when `repo` is a linked git worktree (or a separated-git-dir
/// checkout): its `.git` is a FILE containing `gitdir: …`.
pub fn is_linked_worktree(repo: &Path) -> bool {
    let dot_git = repo.join(".git");
    let Ok(meta) = std::fs::metadata(&dot_git) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    std::fs::read_to_string(&dot_git)
        .map(|c| c.trim_start().starts_with("gitdir:"))
        .unwrap_or(false)
}

/// The main checkout's worktree path for a linked worktree (the first
/// entry of `git worktree list --porcelain`). `None` for a normal repo.
pub fn main_worktree(repo: &Path) -> Option<PathBuf> {
    let out = std::process::Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(repo)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            let p = PathBuf::from(path);
            if p != repo {
                return Some(p);
            }
        }
    }
    None
}

/// All linked worktrees except `repo` itself: `(root, branch)` pairs from
/// `git worktree list --porcelain` (issue #12).
pub fn list_worktrees(repo: &Path) -> Result<Vec<(PathBuf, Option<String>)>> {
    let out = std::process::Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(repo)
        .output()
        .context("git worktree list")?;
    if !out.status.success() {
        return Ok(Vec::new());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let repo_canon = repo.canonicalize().unwrap_or_else(|_| repo.to_path_buf());
    let mut out_list = Vec::new();
    let (mut root, mut branch): (Option<PathBuf>, Option<String>) = (None, None);
    let mut flush = |root: &mut Option<PathBuf>, branch: &mut Option<String>| {
        if let Some(r) = root.take() {
            let canon = r.canonicalize().unwrap_or_else(|_| r.clone());
            if canon != repo_canon {
                out_list.push((r, branch.take()));
            }
        }
        *branch = None;
    };
    // Entries are separated by a blank line and the LAST one has no trailing
    // separator, so both the blank line and the end of output must flush.
    for line in text.lines() {
        if line.is_empty() {
            flush(&mut root, &mut branch);
        } else if let Some(p) = line.strip_prefix("worktree ") {
            flush(&mut root, &mut branch);
            root = Some(PathBuf::from(p));
        } else if let Some(b) = line.strip_prefix("branch ") {
            branch = Some(b.trim_start_matches("refs/heads/").to_string());
        }
    }
    flush(&mut root, &mut branch);
    Ok(out_list)
}

/// The merge base of `repo`'s HEAD and `other_sha`, if any.
pub fn merge_base(repo: &Path, other_sha: &str) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["merge-base", "HEAD"])
        .arg(other_sha)
        .current_dir(repo)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!sha.is_empty()).then_some(sha)
}

/// Paths with uncommitted changes (tracked modified/deleted + untracked).
pub fn status_paths(repo: &Path) -> Vec<String> {
    let Ok(out) = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(repo)
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.get(3..).map(|p| p.trim().trim_matches('"').to_string()))
        .filter(|p| !p.is_empty())
        .collect()
}

/// Commits between `sha` (exclusive) and HEAD: how far behind the index
/// is. `None` when `sha` is unresolvable (e.g. shallow/detached noise).
pub fn commits_behind(repo: &Path, sha: &str) -> Option<u64> {
    let out = std::process::Command::new("git")
        .args(["rev-list", "--count"])
        .arg(format!("{sha}..HEAD"))
        .current_dir(repo)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

/// Timestamp of `sha` (unix seconds) — for age-based staleness floors.
pub fn commit_timestamp(repo: &Path, sha: &str) -> Option<i64> {
    let out = std::process::Command::new("git")
        .args(["show", "-s", "--format=%ct"])
        .arg(sha)
        .current_dir(repo)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

/// True when `path` exists in the HEAD commit (false = deleted since
/// `as_of` — coordinates in stale advisories are historical).
pub fn exists_at_head(repo: &Path, path: &str) -> bool {
    std::process::Command::new("git")
        .args(["cat-file", "-e"])
        .arg(format!("HEAD:{path}"))
        .current_dir(repo)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(true) // fail-open: unknown ⇒ don't claim deletion
}

pub fn file_hash(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let mut h = blake3::Hasher::new();
    h.update(&bytes);
    Some(h.finalize().to_hex().to_string())
}

/// Byte offset → 1-based line number within `source`.
pub fn line_of(source: &str, byte: usize) -> usize {
    source[..byte.min(source.len())]
        .bytes()
        .filter(|b| *b == b'\n')
        .count()
        + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_of_counts_lines() {
        // "fn a() {}\nfn b() {}\n" — newline bytes belong to the following
        // row, matching tree-sitter's row semantics.
        let src = "fn a() {}\nfn b() {}\n";
        assert_eq!(line_of(src, 0), 1);
        assert_eq!(line_of(src, 10), 2);
        assert_eq!(line_of(src, 11), 2);
        assert_eq!(line_of(src, src.len()), 3);
    }

    #[test]
    fn line_of_is_total_for_out_of_range() {
        assert_eq!(line_of("abc", 999), 1);
    }
}

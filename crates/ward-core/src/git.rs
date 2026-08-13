//! Thin git plumbing — Ward shells out to the real git (law P1: git is the
//! only source of truth, so Ward *reads* it rather than reimplementing it).

use std::path::Path;
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

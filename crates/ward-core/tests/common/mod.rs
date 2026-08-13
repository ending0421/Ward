//! Shared test infrastructure: a real temporary git repository so engine
//! tests exercise the actual git plumbing (head/diff/show) end to end.

use std::path::{Path, PathBuf};
use std::process::Command;

pub struct TestRepo {
    pub dir: tempfile::TempDir,
}

impl TestRepo {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = Self { dir };
        repo.git(["init", "-b", "master"]);
        repo.git(["config", "user.name", "test"]);
        repo.git(["config", "user.email", "test@example.com"]);
        repo.git(["config", "commit.gpgsign", "false"]);
        repo
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    pub fn git(&self, args: impl IntoIterator<Item = impl AsRef<std::ffi::OsStr>>) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(self.path())
            .args(args)
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// Write a file (creating parent dirs) and return its repo-relative path.
    pub fn write(&self, rel: &str, content: &str) -> PathBuf {
        let abs = self.path().join(rel);
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent).expect("create dirs");
        }
        std::fs::write(&abs, content).expect("write file");
        PathBuf::from(rel)
    }

    /// Stage everything and commit; returns the new HEAD sha.
    pub fn commit_all(&self, message: &str) -> String {
        self.git(["add", "-A"]);
        self.git(["commit", "-q", "-m", message]);
        self.git(["rev-parse", "HEAD"]).trim().to_string()
    }

    pub fn head(&self) -> String {
        self.git(["rev-parse", "HEAD"]).trim().to_string()
    }
}

//! `.ward/config.toml` — per-repository Ward configuration.
//!
//! Loading is fail-open by design (law P3): a missing or malformed config
//! file degrades to defaults plus a warning, never an error.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Default config file location inside a repository.
pub fn default_path(repo_root: &Path) -> PathBuf {
    repo_root.join(".ward").join("config.toml")
}

/// Similarity thresholds for Spot advisories.
///
/// These are deliberately declared as *initial values* — the design mandates
/// weekly recalibration against a human-labeled golden set (spec §3-M1).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Thresholds {
    /// Strong suggestion: "reuse or extend the existing implementation".
    pub strong: f64,
    /// Weak suggestion: listed for reference only.
    pub weak: f64,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            strong: 0.92,
            weak: 0.80,
        }
    }
}

/// Local lint/type precheck command (inner loop, no Docker).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LintConfig {
    /// Command run by `catch_run` inner-loop precheck. Empty disables it.
    pub command: String,
    /// Timeout in seconds.
    pub timeout_secs: u64,
}

impl Default for LintConfig {
    fn default() -> Self {
        Self {
            command: String::from("cargo check --quiet"),
            timeout_secs: 120,
        }
    }
}

/// Outer-loop sandbox settings (CI adjudication).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SandboxConfig {
    /// Full verification command executed inside the sandbox.
    pub verify_command: String,
    /// Docker image used for the sandbox.
    pub image: String,
    /// Memory limit (docker --memory).
    pub memory: String,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            verify_command: String::from("cargo test --quiet"),
            image: String::from("rust:1-bookworm"),
            memory: String::from("2g"),
        }
    }
}

/// Full repository configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WardConfig {
    pub thresholds: Thresholds,
    /// Path globs to suppress from Spot advisories.
    pub suppress: Vec<String>,
    /// Number of matches returned per advisory.
    pub top_k: usize,
    /// Enabled languages (currently only `rust` is wired; unknown entries are
    /// accepted and ignored until their grammars are added — fail-open).
    pub languages: Vec<String>,
    pub lint: LintConfig,
    pub sandbox: SandboxConfig,
}

impl Default for WardConfig {
    fn default() -> Self {
        Self {
            thresholds: Thresholds::default(),
            suppress: Vec::new(),
            top_k: 5,
            languages: vec!["rust".to_string()],
            lint: LintConfig::default(),
            sandbox: SandboxConfig::default(),
        }
    }
}

impl WardConfig {
    /// Load config from `path`, falling back to defaults on any failure.
    ///
    /// Returns the config and whether a fallback happened (so callers can
    /// record it instead of failing).
    pub fn load_or_default(path: &Path) -> (Self, Option<String>) {
        match std::fs::read_to_string(path) {
            Ok(raw) => match toml::from_str::<WardConfig>(&raw) {
                Ok(cfg) => (cfg, None),
                Err(e) => {
                    let warn = format!("malformed {} ({e}); using defaults", path.display());
                    tracing::warn!("{warn}");
                    (WardConfig::default(), Some(warn))
                }
            },
            Err(_) => (WardConfig::default(), None),
        }
    }

    /// Is the given (repo-relative) path suppressed?
    pub fn is_suppressed(&self, path: &str) -> bool {
        self.suppress.iter().any(|pat| {
            if let Some(stripped) = pat.strip_suffix('/') {
                path.starts_with(stripped)
            } else {
                path.contains(pat)
            }
        })
    }
}

/// Write a starter config file (used by `ward init`).
pub fn write_starter_config(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let cfg = WardConfig::default();
    let raw = toml::to_string_pretty(&cfg).context("serializing starter config")?;
    let header = "# Ward configuration (see docs/ward-tech-spec-v0.6.1.md §3, §7)\n";
    std::fs::write(path, format!("{header}{raw}"))
        .with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_the_spec_initial_thresholds() {
        let cfg = WardConfig::default();
        assert_eq!(cfg.thresholds.strong, 0.92);
        assert_eq!(cfg.thresholds.weak, 0.80);
        assert_eq!(cfg.top_k, 5);
        assert_eq!(cfg.languages, vec!["rust"]);
    }

    #[test]
    fn valid_config_loads_without_warning() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "top_k = 3\n[thresholds]\nstrong = 0.95\nweak = 0.85\n",
        )
        .unwrap();
        let (cfg, warn) = WardConfig::load_or_default(&path);
        assert!(warn.is_none());
        assert_eq!(cfg.thresholds.strong, 0.95);
        assert_eq!(cfg.thresholds.weak, 0.85);
        assert_eq!(cfg.top_k, 3);
    }

    #[test]
    fn starter_config_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write_starter_config(&path).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(
            raw.contains("thresholds"),
            "starter config documents sections"
        );
        let (cfg, warn) = WardConfig::load_or_default(&path);
        assert!(warn.is_none(), "starter config must parse: {warn:?}");
        assert_eq!(cfg.thresholds.strong, 0.92);
    }

    #[test]
    fn suppression_treats_bare_pattern_as_contains() {
        let cfg = WardConfig {
            suppress: vec!["generated".to_string()],
            ..Default::default()
        };
        assert!(cfg.is_suppressed("src/generated.rs"));
        assert!(!cfg.is_suppressed("src/clean.rs"));
    }

    #[test]
    fn malformed_config_falls_back_with_warning() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "this is [not valid toml").unwrap();
        let (cfg, warn) = WardConfig::load_or_default(&path);
        assert!(warn.is_some());
        assert_eq!(cfg.thresholds.strong, 0.92);
    }

    #[test]
    fn missing_config_is_silent_default() {
        let (cfg, warn) = WardConfig::load_or_default(Path::new("/nonexistent/config.toml"));
        assert!(warn.is_none());
        assert_eq!(cfg.thresholds.weak, 0.80);
    }

    #[test]
    fn suppression_matches_paths() {
        let cfg = WardConfig {
            suppress: vec!["vendor/".to_string(), "generated".to_string()],
            ..Default::default()
        };
        assert!(cfg.is_suppressed("vendor/foo/bar.rs"));
        assert!(cfg.is_suppressed("src/generated_code.rs"));
        assert!(!cfg.is_suppressed("src/lib.rs"));
    }
}

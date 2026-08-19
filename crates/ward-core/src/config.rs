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
    /// Signature specificity floor (issue #5): queries whose signature is
    /// mostly basic/std types (specificity below this) return matches but
    /// never grade above Weak — automated gates must ignore them.
    pub specificity_floor: f64,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            strong: 0.92,
            weak: 0.80,
            specificity_floor: 0.5,
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

/// FFI export-face options (0.5-3): the expected export face (a checked-in
/// declaration header) and the built artifact to inspect.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FfiConfig {
    /// Repo-relative path to the checked-in C declaration header. Empty =
    /// auto-detect headers under ffi//include/.
    pub manifest: Option<String>,
    /// File-name glob for the built artifact (`target/*/lib*.so`). Empty =
    /// no artifact search → honest unknown.
    pub artifact_glob: String,
}

impl Default for FfiConfig {
    fn default() -> Self {
        Self {
            manifest: None,
            artifact_glob: "target/*/lib*.so".into(),
        }
    }
}

/// Duplicate-clustering options (M6).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ClustersConfig {
    /// Exclude symbols inside `#[cfg(test)] mod tests` and files under
    /// `tests/` — test functions are structurally near-duplicates BY DESIGN
    /// and would drown the real consolidation signal.
    pub exclude_tests: bool,
}

impl Default for ClustersConfig {
    fn default() -> Self {
        Self {
            exclude_tests: true,
        }
    }
}

/// Full repository configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WardConfig {
    pub thresholds: Thresholds,
    pub clusters: ClustersConfig,
    /// Path globs to suppress from Spot advisories.
    pub suppress: Vec<String>,
    /// Number of matches returned per advisory.
    pub top_k: usize,
    /// Enabled languages. All five grammars are compiled in; this list
    /// *restricts* which of them Ward indexes and matches. Names accept
    /// any casing (`"Kotlin"` ≡ `"kotlin"`); unknown names are ignored
    /// with a warning — fail-open, they never error a run. Defaults to
    /// all five.
    pub languages: Vec<String>,
    pub lint: LintConfig,
    pub sandbox: SandboxConfig,
    pub ffi: FfiConfig,
}

impl Default for WardConfig {
    fn default() -> Self {
        Self {
            thresholds: Thresholds::default(),
            clusters: ClustersConfig::default(),
            suppress: Vec::new(),
            top_k: 5,
            languages: crate::lang::Language::ALL
                .iter()
                .map(|l| l.as_str().to_string())
                .collect(),
            lint: LintConfig::default(),
            sandbox: SandboxConfig::default(),
            ffi: FfiConfig::default(),
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

    /// Is the given language enabled for indexing/matching?
    pub fn is_language_enabled(&self, lang: crate::lang::Language) -> bool {
        self.languages
            .iter()
            .any(|l| l.trim().eq_ignore_ascii_case(lang.as_str()))
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
        assert_eq!(
            cfg.languages,
            vec!["rust", "kotlin", "java", "swift", "objc"],
            "all five grammars enabled by default"
        );
    }

    #[test]
    fn language_gate_is_case_insensitive_and_fail_open() {
        let cfg = WardConfig {
            languages: vec!["Rust".into(), "Kotlin".into(), "bogus".into()],
            ..Default::default()
        };
        assert!(cfg.is_language_enabled(crate::lang::Language::Rust));
        assert!(cfg.is_language_enabled(crate::lang::Language::Kotlin));
        assert!(!cfg.is_language_enabled(crate::lang::Language::Swift));
        assert!(!cfg.is_language_enabled(crate::lang::Language::Java));
        assert!(!cfg.is_language_enabled(crate::lang::Language::ObjC));
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

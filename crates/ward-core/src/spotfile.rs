//! PostToolUse hook engine (spec §3-M1 强制抓手 #1): spot-check the symbols
//! an agent just wrote. The hook runs *before* the index refresh, so the
//! store still holds the pre-write state — new/changed symbols are exactly
//! the diff between the working tree and the store.

use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::config::WardConfig;
use crate::lang::Language;
use crate::search::{self, SpotResult};
use crate::store::Store;

/// Context budget: at most this many symbols get a spot query per write
/// (the hook must stay cheap and its injected context small).
pub const MAX_SYMBOLS_PER_FILE: usize = 5;

/// What one `spot-file` invocation found and checked.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSpotReport {
    pub path: String,
    /// Names of symbols introduced or changed by the write.
    pub changed_symbols: Vec<String>,
    /// How many of them actually got a spot query (capped).
    pub checked: usize,
    /// One advisory per checked symbol (in the same order).
    pub advisories: Vec<SpotResult>,
    /// State of the index backing this report (issue #17):
    /// `missing` (no index) / `empty` (0 symbols) / `fresh` / `stale`,
    /// or `unchecked` when the report never consulted the index (not a
    /// source file, unreadable, unparsable). Without it, a fail-open report
    /// and "this write introduced nothing new" are byte-identical — the
    /// same silence-as-absence hole #17 found in `spot`.
    #[serde(default = "default_index_state")]
    pub index_state: String,
}

fn default_index_state() -> String {
    "unchecked".to_string()
}

impl FileSpotReport {
    fn empty(path: &str) -> Self {
        Self {
            path: path.to_string(),
            changed_symbols: Vec::new(),
            checked: 0,
            advisories: Vec::new(),
            index_state: default_index_state(),
        }
    }
}

/// Compare the symbols of `path` between the store (pre-write) and the
/// working tree (post-write), then run a structural spot query per new or
/// changed symbol. Fail-open throughout: unreadable files, unsupported
/// languages, broken parses (F3) and store errors degrade to an empty
/// report — the hook must never block the agent (P3/P7).
pub fn spot_new_symbols(
    repo: &Path,
    store: &Store,
    config: &WardConfig,
    path: &str,
) -> Result<FileSpotReport> {
    spot_new_symbols_scoped(repo, store, config, path, None)
}

/// [`spot_new_symbols`] with a monorepo scope filter (spec §2.6): queries
/// are constrained to the written file's package/module.
pub fn spot_new_symbols_scoped(
    repo: &Path,
    store: &Store,
    config: &WardConfig,
    path: &str,
    scope: Option<&str>,
) -> Result<FileSpotReport> {
    let full = repo.join(path);
    let Some(lang) = Language::from_path(&full) else {
        return Ok(FileSpotReport::empty(path)); // not a source file
    };
    let Ok(source) = std::fs::read_to_string(&full) else {
        return Ok(FileSpotReport::empty(path)); // unreadable → skip
    };
    let Some(tree) = crate::fingerprint::parse(lang, &source) else {
        return Ok(FileSpotReport::empty(path));
    };
    if tree.root_node().has_error() {
        return Ok(FileSpotReport::empty(path)); // F3: broken file, no evidence
    }
    let extracted = crate::index::extract_symbols(&tree, &source, lang.spec());

    // Pre-write state: name → body_hash for this file only. The full symbol
    // list doubles as the index-state probe (issue #17).
    let all = store.all_symbols()?;
    let index_state = if all.is_empty() {
        "empty"
    } else if crate::fresh::check(repo, store, &[])?.stale {
        "stale"
    } else {
        "fresh"
    };
    let old: HashMap<String, String> = all
        .into_iter()
        .filter(|s| s.file_path == path)
        .map(|s| (s.name, s.body_hash))
        .collect();
    let mut changed: Vec<&crate::index::Extracted> = Vec::new();
    for e in &extracted {
        if e.symbol.in_test || path.split('/').any(|seg| seg == "tests") {
            continue;
        }
        if old.get(&e.symbol.name) != Some(&e.symbol.body_hash) {
            changed.push(e);
        }
    }

    let checked = changed.len().min(MAX_SYMBOLS_PER_FILE);
    let mut advisories = Vec::with_capacity(checked);
    for e in changed.iter().take(MAX_SYMBOLS_PER_FILE) {
        let sig = source
            .get(e.symbol.start_byte as usize..e.symbol.end_byte as usize)
            .unwrap_or("")
            .to_string();
        let intent = format!("新增/变更符号 {}（{}）", e.symbol.name, path);
        let result = search::spot(
            repo,
            config,
            &intent,
            Some(&sig),
            None,
            Some(lang),
            &search::SpotOptions {
                scope: scope.map(String::from),
                ..Default::default()
            },
        )?;
        advisories.push(result);
    }
    Ok(FileSpotReport {
        path: path.to_string(),
        changed_symbols: changed.iter().map(|e| e.symbol.name.clone()).collect(),
        checked,
        advisories,
        index_state: index_state.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_source_path_is_an_empty_report() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("index.db")).unwrap();
        std::fs::write(dir.path().join("README.md"), "x").unwrap();
        let r = spot_new_symbols(dir.path(), &store, &WardConfig::default(), "README.md").unwrap();
        assert!(r.changed_symbols.is_empty());
        assert_eq!(r.checked, 0);
        assert_eq!(r.index_state, "unchecked", "never consulted the index");
    }

    #[test]
    fn unreadable_path_is_an_empty_report() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("index.db")).unwrap();
        let r =
            spot_new_symbols(dir.path(), &store, &WardConfig::default(), "src/missing.rs").unwrap();
        assert!(r.changed_symbols.is_empty());
        assert_eq!(r.checked, 0);
    }

    #[test]
    fn broken_source_is_f3_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("index.db")).unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/broken.rs"), "fn broken( {").unwrap();
        let r =
            spot_new_symbols(dir.path(), &store, &WardConfig::default(), "src/broken.rs").unwrap();
        assert!(r.changed_symbols.is_empty());
        assert_eq!(r.checked, 0);
    }
}

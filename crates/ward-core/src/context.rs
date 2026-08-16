//! M5 Context Cards — one page per symbol: definition, callers (lower
//! bound), related tests, and configuration references (spec §3-M5).
//!
//! This is a thin wrapper over the M1 index: no self-built 2-hop graph, no
//! self-built search stack (P4). The retrieval backend could later be
//! swapped for Probe CLI integration (spec §11.2) without changing this
//! module's API.

use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::config::WardConfig;
use crate::store::Store;

/// One caller entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallerRef {
    pub path: String,
    pub symbol: String,
}

/// A related test file that mentions the symbol.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestRef {
    pub path: String,
    pub symbol: String,
}

/// Configuration files that mention the symbol name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigRef {
    pub path: String,
    pub line: usize,
}

/// The one-page context card.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextCard {
    /// The query as resolved to a symbol.
    pub symbol: String,
    pub kind: String,
    pub language: String,
    pub path: String,
    pub lines: String,
    /// Callers, lower bound ("at least N" semantics, spec §3-M2).
    pub callers: Vec<CallerRef>,
    /// Tests mentioning the symbol.
    pub tests: Vec<TestRef>,
    /// Configuration files mentioning the symbol.
    pub config_refs: Vec<ConfigRef>,
}

/// Config-file extensions scanned for symbol mentions.
const CONFIG_EXTS: &[&str] = &[
    "toml", "yaml", "yml", "json", "gradle", "kts", "xml", "plist", "lock",
];

/// Resolve a query (exact symbol name, or `path:line`) and build its card.
///
/// Fail-open: an unknown query returns a card with the query echoed back and
/// everything else empty — never an error.
pub fn context_card(
    repo: &Path,
    store: &Store,
    config: &WardConfig,
    query: &str,
) -> Result<ContextCard> {
    let symbols = store.all_symbols()?;
    let symbol = resolve(&symbols, query, repo);

    let empty = |name: String| ContextCard {
        symbol: name,
        kind: String::new(),
        language: String::new(),
        path: String::new(),
        lines: String::new(),
        callers: Vec::new(),
        tests: Vec::new(),
        config_refs: Vec::new(),
    };
    let Some(sym) = symbol else {
        return Ok(empty(query.to_string()));
    };

    let callers: Vec<CallerRef> = store
        .mentioners(&sym.name)?
        .into_iter()
        .filter(|(p, _)| !config.is_suppressed(p))
        .map(|(path, symbol)| CallerRef { path, symbol })
        .collect();
    let tests: Vec<TestRef> = callers
        .iter()
        .filter(|c| c.path.contains("tests/") || c.path.contains("/test/"))
        .map(|c| TestRef {
            path: c.path.clone(),
            symbol: c.symbol.clone(),
        })
        .collect();
    let config_refs = scan_config_refs(repo, &sym.name)?;

    let lines = {
        let path = repo.join(&sym.file_path);
        let Ok(source) = std::fs::read_to_string(path) else {
            return Ok(empty(sym.name));
        };
        let a = crate::git::line_of(&source, sym.start_byte.max(0) as usize);
        let b = crate::git::line_of(&source, sym.end_byte.max(0) as usize);
        if a == b {
            format!("{a}")
        } else {
            format!("{a}-{b}")
        }
    };

    Ok(ContextCard {
        symbol: sym.name.clone(),
        kind: sym.kind.clone(),
        language: sym.language.clone(),
        path: sym.file_path.clone(),
        lines,
        callers,
        tests,
        config_refs,
    })
}

/// Exact name match first; `path:line` fallback.
fn resolve(
    symbols: &[crate::store::Symbol],
    query: &str,
    repo: &Path,
) -> Option<crate::store::Symbol> {
    if let Some(sym) = symbols.iter().find(|s| s.name == query) {
        return Some(sym.clone());
    }
    // `path:line` form.
    if let Some((path_part, line_part)) = query.rsplit_once(':') {
        if let Ok(line) = line_part.parse::<usize>() {
            let source = std::fs::read_to_string(repo.join(path_part)).ok()?;
            // Find the byte offset of that line.
            let mut byte = 0usize;
            for (i, l) in source.lines().enumerate() {
                if i + 1 == line {
                    break;
                }
                byte += l.len() + 1;
            }
            if let Some(sym) = symbols.iter().find(|s| {
                s.file_path == path_part
                    && (s.start_byte as usize) <= byte
                    && byte <= (s.end_byte as usize)
            }) {
                return Some(sym.clone());
            }
        }
    }
    None
}

/// Scan config-shaped files for lines mentioning the symbol name.
fn scan_config_refs(repo: &Path, name: &str) -> Result<Vec<ConfigRef>> {
    let mut out = Vec::new();
    for entry in walk(repo) {
        let ext = entry
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default();
        if !CONFIG_EXTS.contains(&ext) {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&entry) else {
            continue;
        };
        for (i, line) in content.lines().enumerate() {
            if line.contains(name) {
                let rel = entry
                    .strip_prefix(repo)
                    .unwrap_or(&entry)
                    .to_string_lossy()
                    .into_owned();
                out.push(ConfigRef {
                    path: rel,
                    line: i + 1,
                });
            }
        }
    }
    Ok(out)
}

fn walk(repo: &Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![repo.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') || name == "target" {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                stack.push(path);
            } else if meta.is_file() {
                files.push(path);
            }
        }
    }
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_exact_name_then_path_line() {
        let symbols = vec![crate::store::Symbol {
            id: None,
            file_path: "src/lib.rs".into(),
            module: String::new(),
            language: "rust".into(),
            name: "debounce".into(),
            kind: "function_item".into(),
            start_byte: 10,
            end_byte: 60,
            body_hash: "b".into(),
            struct_hash: "s".into(),
            simhash: 1,
            sig_simhash: 1,
            in_test: false,
            commit_sha: "c".into(),
        }];
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/lib.rs"),
            "xxxxxxxxxx\npub fn debounce() {}\n",
        )
        .unwrap();
        // path:line 2 resolves into the symbol at bytes 10..60.
        let sym = resolve(&symbols, "src/lib.rs:2", dir.path()).unwrap();
        assert_eq!(sym.name, "debounce");
        let none = resolve(&symbols, "src/lib.rs:1", dir.path());
        assert!(none.is_none(), "line 1 is outside the symbol");
        let exact = resolve(&symbols, "debounce", dir.path()).unwrap();
        assert_eq!(exact.name, "debounce");
        assert!(resolve(&symbols, "missing", dir.path()).is_none());
    }

    #[test]
    fn unknown_query_fails_open_to_empty_card() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("index.db")).unwrap();
        let card = context_card(dir.path(), &store, &WardConfig::default(), "nope").unwrap();
        assert_eq!(card.symbol, "nope");
        assert!(card.callers.is_empty());
        assert!(card.path.is_empty());
    }

    #[test]
    fn config_scan_finds_mentions() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ward")).unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"debounce-utils\"\n",
        )
        .unwrap();
        let refs = scan_config_refs(dir.path(), "debounce").unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].path, "Cargo.toml");
        assert_eq!(refs[0].line, 2);
    }
}

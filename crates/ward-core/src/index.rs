//! The Indexer — single-direction data flow: files → tree-sitter → The Rack
//! (spec §2.1.1). There is no reverse direction, ever.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tree_sitter::Node;

use crate::config::WardConfig;
use crate::fingerprint;
use crate::lang::Language;
use crate::store::{Store, Symbol};

/// What a full index pass produced.
#[derive(Debug, Default)]
pub struct IndexReport {
    pub files_indexed: usize,
    pub symbols_indexed: usize,
    /// Files skipped because their language grammar is not compiled in yet
    /// (documented fail-open behavior, spec §3.0).
    pub files_skipped_language: usize,
    /// Files that failed to parse (F3: marked unparsable, everything else
    /// keeps working).
    pub files_unparsable: usize,
    /// Files suppressed via `.ward/config.toml`.
    pub files_suppressed: usize,
    pub commit_sha: Option<String>,
}

/// One extracted symbol plus the identifier mentions inside its own body —
/// the raw input for the static mention edges (a *lower bound* on callers;
/// dynamic dispatch is invisible to it, spec §3-M2).
#[derive(Debug)]
pub struct Extracted {
    pub symbol: Symbol,
    pub mentions: Vec<String>,
}

/// Kinds of tree-sitter-rust nodes we index as symbols.
fn is_symbol_kind(kind: &str) -> bool {
    matches!(
        kind,
        "function_item"
            | "struct_item"
            | "enum_item"
            | "trait_item"
            | "type_item"
            | "const_item"
            | "static_item"
            | "macro_definition"
    )
}

/// Extract indexable symbols (plus their in-body identifier mentions) from a
/// parsed Rust tree.
pub fn extract_symbols(tree: &tree_sitter::Tree, source: &str) -> Vec<Extracted> {
    let mut out = Vec::new();
    let mut cursor = tree.walk();
    for node in tree.root_node().named_children(&mut cursor) {
        walk_for_symbols(&node, source, &mut out);
    }
    out
}

fn walk_for_symbols(node: &Node, source: &str, out: &mut Vec<Extracted>) {
    if is_symbol_kind(node.kind()) {
        if let Some(sym) = symbol_from_node(node, source) {
            let mentions = identifier_mentions_in(node, source)
                .into_iter()
                .filter(|m| m != &sym.name)
                .collect();
            out.push(Extracted {
                symbol: sym,
                mentions,
            });
        }
        // Function bodies are not searched for further "symbols".
        return;
    }
    // `impl` blocks host methods; modules and declaration lists contain more
    // top-level symbols.
    match node.kind() {
        "impl_item" | "mod_item" | "declaration_list" | "source_file" | "block" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                walk_for_symbols(&child, source, out);
            }
        }
        _ => {}
    }
}

fn symbol_from_node(node: &Node, source: &str) -> Option<Symbol> {
    let name_node = node.child_by_field_name("name")?;
    let name = name_node.utf8_text(source.as_bytes()).ok()?.to_string();
    let body = node.utf8_text(source.as_bytes()).ok()?.to_string();
    let features = fingerprint::subtree_features_of(node);
    let struct_form = crate::normalize::canonical_form_of(node);
    let mut h = blake3::Hasher::new();
    h.update(struct_form.as_bytes());
    Some(Symbol {
        id: None,
        file_path: String::new(), // filled in by the caller
        language: Language::Rust.as_str().to_string(),
        name,
        kind: node.kind().to_string(),
        start_byte: node.start_byte() as i64,
        end_byte: node.end_byte() as i64,
        body_hash: fingerprint::body_hash(&body),
        struct_hash: h.finalize().to_hex().to_string(),
        simhash: fingerprint::simhash(&features),
        commit_sha: String::new(), // filled in by the caller
    })
}

/// Every identifier mention inside a node's subtree.
pub fn identifier_mentions_in(node: &Node, source: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    collect_identifiers(node, source, &mut out);
    out
}

fn collect_identifiers(node: &Node, source: &str, out: &mut BTreeSet<String>) {
    if node.kind() == "identifier" {
        if let Ok(text) = node.utf8_text(source.as_bytes()) {
            out.insert(text.to_string());
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_identifiers(&child, source, out);
    }
}

/// Repository-wide indexer.
pub struct Indexer<'a> {
    pub repo_root: &'a Path,
    pub store: &'a mut Store,
    pub config: &'a WardConfig,
}

impl Indexer<'_> {
    /// Walk the repository and index every supported file. Fail-open at every
    /// step: a broken file never stops the pass.
    pub fn index_all(&mut self) -> Result<IndexReport> {
        let mut report = IndexReport::default();
        let commit = crate::git::head_sha(self.repo_root)?;
        report.commit_sha = commit.clone();
        let sha = commit.unwrap_or_else(|| "uncommitted".to_string());

        let files = self.collect_files()?;
        for path in files {
            let rel = path
                .strip_prefix(self.repo_root)
                .unwrap_or(&path)
                .to_string_lossy()
                .into_owned();
            if self.config.is_suppressed(&rel) {
                report.files_suppressed += 1;
                continue;
            }
            let Some(lang) = Language::from_path(&path) else {
                continue;
            };
            if lang.ts_language().is_none() {
                report.files_skipped_language += 1;
                continue;
            }
            let source = match std::fs::read_to_string(&path) {
                Ok(s) => s,
                Err(_) => continue, // unreadable file: skip, fail-open
            };
            let Some(tree) = fingerprint::parse_rust(&source) else {
                report.files_unparsable += 1;
                continue; // F3
            };

            let extracted = extract_symbols(&tree, &source);
            let mut symbols: Vec<Symbol> = Vec::with_capacity(extracted.len());
            for e in &extracted {
                let mut sym = e.symbol.clone();
                sym.file_path = rel.clone();
                sym.commit_sha = sha.clone();
                symbols.push(sym);
            }
            let ids = self.store.replace_file(&rel, &symbols)?;
            for (id, e) in ids.iter().zip(&extracted) {
                if !e.mentions.is_empty() {
                    self.store.set_mentions(*id, &e.mentions)?;
                }
            }

            if let Some(h) = crate::git::file_hash(&path) {
                self.store.set_file_hash(&rel, &h)?;
            }
            report.files_indexed += 1;
            report.symbols_indexed += symbols.len();
        }

        if let Some(sha) = &report.commit_sha {
            self.store.set_last_indexed_sha(sha)?;
        }
        Ok(report)
    }

    fn collect_files(&self) -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();
        let mut stack = vec![self.repo_root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let entries = match std::fs::read_dir(&dir) {
                Ok(e) => e,
                Err(_) => continue,
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name == ".git"
                    || name == ".ward"
                    || name == "target"
                    || name == "node_modules"
                {
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
        Ok(files)
    }
}

/// Convenience: index a repository into its default store location.
pub fn index_repo(repo: &Path, config: &WardConfig) -> Result<IndexReport> {
    let mut store = Store::open(&Store::default_path(repo))
        .with_context(|| format!("opening index for {}", repo.display()))?;
    let mut indexer = Indexer {
        repo_root: repo,
        store: &mut store,
        config,
    };
    indexer.index_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_functions_structs_and_impl_methods() {
        let src = r#"
            pub fn debounce(f: Fn(u64), ms: u64) -> Fn(u64) { f(ms) }
            pub struct Config { pub ms: u64 }
            impl Config { pub fn new() -> Self { Config { ms: 0 } } }
            fn private_helper() -> u64 { 42 }
        "#;
        let tree = fingerprint::parse_rust(src).unwrap();
        let syms = extract_symbols(&tree, src);
        let names: Vec<&str> = syms.iter().map(|s| s.symbol.name.as_str()).collect();
        assert!(names.contains(&"debounce"));
        assert!(names.contains(&"Config"));
        assert!(names.contains(&"new"));
        assert!(names.contains(&"private_helper"));
    }

    #[test]
    fn mentions_are_per_symbol_and_exclude_self() {
        let src = "fn outer() { debounce(); throttle() }\nfn debounce() { }";
        let tree = fingerprint::parse_rust(src).unwrap();
        let syms = extract_symbols(&tree, src);
        let outer = syms
            .iter()
            .find(|s| s.symbol.name == "outer")
            .expect("outer exists");
        assert!(outer.mentions.contains(&"debounce".to_string()));
        assert!(outer.mentions.contains(&"throttle".to_string()));
        let debounce = syms
            .iter()
            .find(|s| s.symbol.name == "debounce")
            .expect("debounce exists");
        assert!(!debounce.mentions.contains(&"debounce".to_string()));
    }
}

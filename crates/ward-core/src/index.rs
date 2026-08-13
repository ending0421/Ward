//! The Indexer — single-direction data flow: files → tree-sitter → The Rack
//! (spec §2.1.1). There is no reverse direction, ever.
//!
//! Extraction is table-driven by [`LanguageSpec`] (spec §3.0): symbol kinds,
//! container kinds, the name field and identifier taxonomy all come from the
//! spec, never from hardcoded Rust assumptions. Block-level fingerprints
//! (spec §3-M1, granularity-mismatch fix) are extracted as sliding windows
//! over statement children of block nodes.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tree_sitter::Node;

use crate::config::WardConfig;
use crate::fingerprint;
use crate::lang::{Language, LanguageSpec};
use crate::store::{Block, Store, Symbol};

/// What a full index pass produced.
#[derive(Debug, Default)]
pub struct IndexReport {
    pub files_indexed: usize,
    pub symbols_indexed: usize,
    pub blocks_indexed: usize,
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

/// Sliding-window size for block fingerprints.
pub const BLOCK_WINDOW: usize = 3;

/// Extract indexable symbols (plus their in-body identifier mentions) from a
/// parsed tree, according to the language spec.
pub fn extract_symbols(
    tree: &tree_sitter::Tree,
    source: &str,
    spec: &LanguageSpec,
) -> Vec<Extracted> {
    let mut out = Vec::new();
    let mut cursor = tree.walk();
    for node in tree.root_node().named_children(&mut cursor) {
        walk_for_symbols(&node, source, spec, &mut out);
    }
    out
}

fn walk_for_symbols(node: &Node, source: &str, spec: &LanguageSpec, out: &mut Vec<Extracted>) {
    if spec.is_symbol_kind(node.kind()) {
        if let Some(sym) = symbol_from_node(node, source, spec) {
            let mentions = identifier_mentions_in(node, source, spec)
                .into_iter()
                .filter(|m| m != &sym.name)
                .collect();
            out.push(Extracted {
                symbol: sym,
                mentions,
            });
        }
    }
    // Always descend: container-like symbols (classes/interfaces) host
    // nested symbols in their bodies, and nested declarations inside
    // function bodies are legal in several of Ward's languages.
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        walk_for_symbols(&child, source, spec, out);
    }
}

/// Resolve a declaration's name node: prefer the spec's name field, then
/// fall back to the first identifier-kind named child (grammars differ in
/// field wiring — Kotlin/ObjC declare the name without a `name` field).
fn name_node_of<'a>(node: &'a Node, spec: &LanguageSpec) -> Option<Node<'a>> {
    if let Some(n) = node.child_by_field_name(spec.name_field) {
        return Some(n);
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find(|c| spec.is_identifier_kind(c.kind()))
}

fn symbol_from_node(node: &Node, source: &str, spec: &LanguageSpec) -> Option<Symbol> {
    let name_node = name_node_of(node, spec)?;
    let name = name_node.utf8_text(source.as_bytes()).ok()?.to_string();
    let body = node.utf8_text(source.as_bytes()).ok()?.to_string();
    let features = fingerprint::subtree_features_of(node);
    let sig_features = node
        .child_by_field_name("body")
        .map(|body| fingerprint::subtree_features_excluding(node, body))
        .unwrap_or_else(|| features.clone());
    let struct_form = crate::normalize::canonical_form_of(node, spec);
    let mut h = blake3::Hasher::new();
    h.update(struct_form.as_bytes());
    Some(Symbol {
        id: None,
        file_path: String::new(), // filled in by the caller
        language: spec.lang.as_str().to_string(),
        name,
        kind: node.kind().to_string(),
        start_byte: node.start_byte() as i64,
        end_byte: node.end_byte() as i64,
        body_hash: fingerprint::body_hash(&body),
        struct_hash: h.finalize().to_hex().to_string(),
        simhash: fingerprint::simhash(&features),
        sig_simhash: fingerprint::simhash(&sig_features),
        commit_sha: String::new(), // filled in by the caller
    })
}

/// Every identifier mention inside a node's subtree (per-spec identifier
/// taxonomy).
pub fn identifier_mentions_in(node: &Node, source: &str, spec: &LanguageSpec) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    collect_identifiers(node, source, spec, &mut out);
    out
}

fn collect_identifiers(node: &Node, source: &str, spec: &LanguageSpec, out: &mut BTreeSet<String>) {
    if spec.is_mention_kind(node.kind()) {
        if let Ok(text) = node.utf8_text(source.as_bytes()) {
            out.insert(text.to_string());
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_identifiers(&child, source, spec, out);
    }
}

/// Extract block-level fingerprints: sliding windows of `BLOCK_WINDOW`
/// consecutive statements over every block node's direct named children.
///
/// Function-level fingerprints cannot see duplication *inside* a large
/// function (spec §3-M1 granularity fix); these windows are the mechanism
/// leg of that fix.
pub fn extract_blocks(tree: &tree_sitter::Tree, source: &str) -> Vec<Block> {
    let mut out = Vec::new();
    let mut cursor = tree.walk();
    for node in tree.root_node().named_children(&mut cursor) {
        collect_blocks(&node, source, &mut out);
    }
    out
}

fn collect_blocks(node: &Node, _source: &str, out: &mut Vec<Block>) {
    if matches!(
        node.kind(),
        "block" | "compound_statement" | "class_body" | "statements"
    ) {
        let mut cursor = node.walk();
        let stmts: Vec<Node> = node.named_children(&mut cursor).collect();
        if stmts.len() >= 2 {
            for win in stmts.windows(BLOCK_WINDOW.min(stmts.len())) {
                let features: Vec<u64> = win
                    .iter()
                    .flat_map(fingerprint::subtree_features_of)
                    .collect();
                out.push(Block {
                    id: None,
                    file_path: String::new(), // filled in by the caller
                    parent_symbol_id: None,
                    start_byte: win.first().unwrap().start_byte() as i64,
                    end_byte: win.last().unwrap().end_byte() as i64,
                    simhash: fingerprint::simhash(&features),
                    kind: "statement_block".to_string(),
                    commit_sha: String::new(),
                });
            }
        }
        return; // do not descend into nested blocks for windows
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_blocks(&child, _source, out);
    }
}

/// Block windows for a query body: wrap it in a synthetic function and reuse
/// the same extraction as indexing, so query windows are comparable to
/// indexed windows.
pub fn block_windows_of_body(body: &str) -> Vec<u64> {
    let wrapped = format!("fn __ward_query__() {{ {body} }}");
    let Some(tree) = fingerprint::parse_rust(&wrapped) else {
        return Vec::new();
    };
    // Tolerant parses of garbage still yield trees; windows from an
    // error-ridden tree would be noise, so reject them (F3 spirit).
    if tree.root_node().has_error() {
        return Vec::new();
    }
    let blocks = extract_blocks(&tree, &wrapped);
    blocks.into_iter().map(|b| b.simhash).collect()
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
            let Some(grammar) = lang.ts_language() else {
                report.files_skipped_language += 1;
                continue;
            };
            let source = match std::fs::read_to_string(&path) {
                Ok(s) => s,
                Err(_) => continue, // unreadable file: skip, fail-open
            };
            let spec = lang.spec();
            let mut parser = tree_sitter::Parser::new();
            if parser.set_language(&grammar).is_err() {
                report.files_skipped_language += 1;
                continue;
            }
            let Some(tree) = parser.parse(&source, None) else {
                report.files_unparsable += 1;
                continue; // F3
            };
            // Tolerant parsers return trees even for broken files; a tree
            // with syntax errors would feed garbage symbols, so F3 treats it
            // as unparsable (full-file skip, everything else keeps working).
            if tree.root_node().has_error() {
                report.files_unparsable += 1;
                continue;
            }

            let extracted = extract_symbols(&tree, &source, spec);
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

            let mut blocks = extract_blocks(&tree, &source);
            for b in &mut blocks {
                b.file_path = rel.clone();
                b.commit_sha = sha.clone();
            }
            report.blocks_indexed += blocks.len();
            self.store.replace_blocks(&rel, &blocks)?;

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
                // Skip VCS/artifact/workspace dot-directories. Hidden dirs
                // in general carry no first-party source (.git, .cargo,
                // .ward, .github, …).
                if name.starts_with('.') || name == "target" || name == "node_modules" {
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
    use crate::lang::{Language, RUST};

    #[test]
    fn extracts_functions_structs_and_impl_methods() {
        let src = r#"
            pub fn debounce(f: Fn(u64), ms: u64) -> Fn(u64) { f(ms) }
            pub struct Config { pub ms: u64 }
            impl Config { pub fn new() -> Self { Config { ms: 0 } } }
            fn private_helper() -> u64 { 42 }
        "#;
        let tree = fingerprint::parse_rust(src).unwrap();
        let syms = extract_symbols(&tree, src, &RUST);
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
        let syms = extract_symbols(&tree, src, &RUST);
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

    #[test]
    fn java_symbols_extracted_spec_driven() {
        let spec = Language::Java.spec();
        let src = "class Foo { int bar() { return 1; } void baz() { } }";
        let tree = fingerprint::parse(Language::Java, src).unwrap();
        let syms = extract_symbols(&tree, src, spec);
        let names: Vec<&str> = syms.iter().map(|s| s.symbol.name.as_str()).collect();
        assert!(names.contains(&"Foo"), "got {names:?}");
        assert!(names.contains(&"bar"));
        assert!(names.contains(&"baz"));
    }

    #[test]
    fn kotlin_symbols_extracted_spec_driven() {
        let spec = Language::Kotlin.spec();
        let src = "class Foo { fun bar(): Int { return 1 } }";
        let tree = fingerprint::parse(Language::Kotlin, src).unwrap();
        let syms = extract_symbols(&tree, src, spec);
        let names: Vec<&str> = syms.iter().map(|s| s.symbol.name.as_str()).collect();
        assert!(names.contains(&"Foo"), "got {names:?}");
        assert!(names.contains(&"bar"));
    }

    #[test]
    fn swift_symbols_extracted_spec_driven() {
        let spec = Language::Swift.spec();
        let src = "class Foo { func bar() -> Int { return 1 } }";
        let tree = fingerprint::parse(Language::Swift, src).unwrap();
        let syms = extract_symbols(&tree, src, spec);
        let names: Vec<&str> = syms.iter().map(|s| s.symbol.name.as_str()).collect();
        assert!(names.contains(&"Foo"), "got {names:?}");
        assert!(names.contains(&"bar"));
    }

    #[test]
    fn objc_symbols_extracted_spec_driven() {
        let spec = Language::ObjC.spec();
        let src = "@interface Foo\n- (int)bar;\n@end\n@implementation Foo\n- (int)bar { return 1; }\n@end";
        let tree = fingerprint::parse(Language::ObjC, src).unwrap();
        let syms = extract_symbols(&tree, src, spec);
        let names: Vec<&str> = syms.iter().map(|s| s.symbol.name.as_str()).collect();
        assert!(names.contains(&"Foo"), "got {names:?}");
        assert!(names.contains(&"bar"));
    }

    #[test]
    fn block_windows_extracted_for_function_bodies() {
        let src = r#"
            fn f() {
                let a = 1;
                let b = a + 1;
                let c = b + 1;
                let d = c + 1;
                let e = d + 1;
            }
        "#;
        let tree = fingerprint::parse_rust(src).unwrap();
        let blocks = extract_blocks(&tree, src);
        assert!(!blocks.is_empty(), "5 statements yield 3 windows");
        assert_eq!(blocks.len(), 3);
        for b in &blocks {
            assert_eq!(b.kind, "statement_block");
        }
    }

    #[test]
    fn query_body_windows_match_indexed_windows() {
        let body = "let a = 1; let b = a + 1; let c = b + 1; let d = c + 1;";
        let src = format!("fn f() {{ {body} }}");
        let tree = fingerprint::parse_rust(&src).unwrap();
        let indexed: Vec<u64> = extract_blocks(&tree, &src)
            .iter()
            .map(|b| b.simhash)
            .collect();
        let query: Vec<u64> = block_windows_of_body(body);
        assert_eq!(indexed.len(), 2);
        assert_eq!(query.len(), 2);
        for (a, b) in indexed.iter().zip(&query) {
            assert_eq!(a, b, "query windows must align with indexed windows");
        }
    }

    #[test]
    fn unparseable_query_body_yields_no_windows() {
        assert!(block_windows_of_body("this is not rust }{").is_empty());
    }

    #[test]
    fn short_bodies_yield_no_windows() {
        assert!(block_windows_of_body("let x = 1;").is_empty());
    }
}

//! M2 Replay — the deterministic layer (spec §3-M2): symbol-level change
//! classification with every fact anchored to `path:line`, plus a lower-bound
//! impact analysis and risk markers.
//!
//! Discipline enforced here:
//! * All facts come from tree-sitter + git; the LLM narration layer (not in
//!   this crate) may only fill slots bound to these facts.
//! * Caller counts are **lower bounds** — dynamic dispatch is invisible to a
//!   static parser, so reports say "at least N" (spec §3-M2).
//! * Doc-only edits are detected by `struct_hash` equality (comments are
//!   excluded from the canonical form).

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tree_sitter::Node;

use crate::config::WardConfig;
use crate::fingerprint;
use crate::git;
use crate::lang::{Language, LanguageSpec};
use crate::store::Store;

/// Classification of one symbol's change between base and head.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Added,
    Removed,
    SignatureChanged,
    BodyChanged,
    /// Renamed/pure-literal/doc edit with identical structure.
    DocOnly,
    Moved,
}

/// One symbol-level change, anchored to file+line.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolChange {
    pub path: String,
    pub lines: String,
    pub name: String,
    pub kind_of: String,
    pub change: ChangeKind,
    /// For `Moved`: the path it came from.
    pub moved_from: Option<String>,
    /// True when the symbol is publicly visible (`pub` / exported).
    pub public: bool,
}

/// A risk marker derived deterministically from the change list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskMarker {
    pub severity: String,
    pub description: String,
    /// Anchors, each `path:line` — clickable back-references.
    pub anchors: Vec<String>,
}

/// The full deterministic replay report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayReport {
    pub base: String,
    pub head: String,
    pub changes: Vec<SymbolChange>,
    pub risks: Vec<RiskMarker>,
    /// Caller counts per changed symbol name ("at least N" semantics).
    pub impact: BTreeMap<String, i64>,
}

/// The parsed symbol surface of one file version.
#[derive(Debug, Clone)]
struct DiffSymbol {
    name: String,
    kind_of: String,
    body_hash: String,
    sig_hash: Option<String>,
    public: bool,
    start_byte: i64,
    end_byte: i64,
}

/// Public-visibility heuristic, cross-language lower bound:
/// * Rust: a `visibility_modifier` named child;
/// * Java/Swift/Kotlin: declaration text starts with `pub`/`public`.
///
/// Kotlin's *implicit* public visibility is not detected — reported impact
/// is a lower bound, consistent with spec §3-M2.
fn is_public(node: &Node, source: &str) -> bool {
    let has_visibility_child = {
        let mut cursor = node.walk();
        node.named_children(&mut cursor)
            .any(|c| c.kind().contains("visibility"))
    };
    let text = node.utf8_text(source.as_bytes()).unwrap_or("");
    let trimmed = text.trim_start();
    has_visibility_child || trimmed.starts_with("pub")
}

/// The signature form of a symbol: the whole declaration minus its body
/// (functions/methods) or the whole declaration (structs/enums/traits).
fn signature_hash(node: &Node, spec: &LanguageSpec) -> Option<String> {
    let body = node.child_by_field_name("body");
    let form = match body {
        Some(b) => crate::normalize::canonical_form_excluding(node, b, spec),
        None => crate::normalize::canonical_form_of(node, spec),
    };
    let mut h = blake3::Hasher::new();
    h.update(form.as_bytes());
    Some(h.finalize().to_hex().to_string())
}

/// Extract the symbol surface of one source file.
fn surface(source: &str, spec: &LanguageSpec) -> BTreeMap<(String, String), DiffSymbol> {
    let mut out = BTreeMap::new();
    let mut parser = tree_sitter::Parser::new();
    if parser
        .set_language(&spec.lang.ts_language().unwrap())
        .is_err()
    {
        return out;
    }
    let Some(tree) = parser.parse(source, None) else {
        return out;
    };
    let mut cursor = tree.walk();
    for node in tree.root_node().named_children(&mut cursor) {
        collect_surface(&node, source, spec, &mut out);
    }
    out
}

fn collect_surface(
    node: &Node,
    source: &str,
    spec: &LanguageSpec,
    out: &mut BTreeMap<(String, String), DiffSymbol>,
) {
    if spec.is_symbol_kind(node.kind()) {
        let name_node = node.child_by_field_name(spec.name_field).or_else(|| {
            let mut cursor = node.walk();
            node.named_children(&mut cursor)
                .find(|c| spec.is_identifier_kind(c.kind()))
        });
        if let Some(name_node) = name_node {
            if let Ok(name) = name_node.utf8_text(source.as_bytes()) {
                let body = node.utf8_text(source.as_bytes()).unwrap_or("");
                out.insert(
                    (name.to_string(), node.kind().to_string()),
                    DiffSymbol {
                        name: name.to_string(),
                        kind_of: node.kind().to_string(),
                        body_hash: fingerprint::body_hash(body),
                        sig_hash: signature_hash(node, spec),
                        public: is_public(node, source),
                        start_byte: node.start_byte() as i64,
                        end_byte: node.end_byte() as i64,
                    },
                );
            }
        }
        if !spec.is_container_kind(node.kind()) {
            return;
        }
    }
    if spec.is_container_kind(node.kind()) {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            collect_surface(&child, source, spec, out);
        }
    }
}

/// Source text with every comment node removed (byte-range deletion).
/// Doc comments are *siblings* of declarations in most grammars (verified
/// for tree-sitter-rust), so symbol-level hashes cannot see doc edits; this
/// file-level view is how Replay detects doc-only changes.
fn collect_comment_ranges(node: &Node, spec: &LanguageSpec, ranges: &mut Vec<(usize, usize)>) {
    if spec.is_comment_kind(node.kind()) {
        ranges.push((node.start_byte(), node.end_byte()));
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_comment_ranges(&child, spec, ranges);
    }
}

fn strip_comments(source: &str, spec: &LanguageSpec) -> String {
    let mut parser = tree_sitter::Parser::new();
    if parser
        .set_language(&spec.lang.ts_language().unwrap())
        .is_err()
    {
        return source.to_string();
    }
    let Some(tree) = parser.parse(source, None) else {
        return source.to_string();
    };
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    {
        let mut cursor = tree.walk();
        for node in tree.root_node().named_children(&mut cursor) {
            collect_comment_ranges(&node, spec, &mut ranges);
        }
    }
    ranges.sort_unstable();
    let bytes = source.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut pos = 0usize;
    for (start, end) in ranges {
        if start < pos {
            continue; // nested/overlapping (should not happen)
        }
        out.extend_from_slice(&bytes[pos..start.min(bytes.len())]);
        pos = end.min(bytes.len());
    }
    out.extend_from_slice(&bytes[pos..]);
    String::from_utf8_lossy(&out).into_owned()
}

fn lines_of(source: &str, start: i64, end: i64) -> String {
    let a = git::line_of(source, start.max(0) as usize);
    let b = git::line_of(source, end.max(0) as usize);
    if a == b {
        format!("{a}")
    } else {
        format!("{a}-{b}")
    }
}

/// Run the deterministic replay between two commits.
///
/// `store` supplies the caller-count lower bounds (static mention edges of
/// the current index) and the near-duplicate check for added symbols.
pub fn replay(
    repo: &Path,
    store: &Store,
    config: &WardConfig,
    base: &str,
    head: &str,
) -> Result<ReplayReport> {
    let changed_files = git::diff_names(repo, base, head)?
        .into_iter()
        .filter(|p| p.ends_with(".rs"))
        .collect::<Vec<_>>();

    let mut changes = Vec::new();

    for path in &changed_files {
        if config.is_suppressed(path) {
            continue;
        }
        let old_src = git::show_file(repo, base, path)?;
        let new_src = git::show_file(repo, head, path)?;
        // Files without a compiled grammar are skipped (fail-open).
        let Some(lang) = Language::from_path(std::path::Path::new(path)) else {
            continue;
        };
        if lang.ts_language().is_none() {
            continue;
        }
        let spec = lang.spec();

        let (old_lines, new_lines) = match (&old_src, &new_src) {
            (Some(o), Some(n)) => (o.clone(), n.clone()),
            (None, Some(n)) => {
                // Brand-new file: every symbol is Added.
                for (_, s) in surface(n, spec) {
                    changes.push(SymbolChange {
                        path: path.clone(),
                        lines: lines_of(n, s.start_byte, s.end_byte),
                        name: s.name,
                        kind_of: s.kind_of,
                        change: ChangeKind::Added,
                        moved_from: None,
                        public: s.public,
                    });
                }
                continue;
            }
            (Some(o), None) => {
                // Deleted file: every symbol is Removed.
                for (_, s) in surface(o, spec) {
                    changes.push(SymbolChange {
                        path: path.clone(),
                        lines: "-".into(),
                        name: s.name,
                        kind_of: s.kind_of,
                        change: ChangeKind::Removed,
                        moved_from: None,
                        public: s.public,
                    });
                }
                continue;
            }
            (None, None) => continue,
        };

        let old = surface(&old_lines, spec);
        let new = surface(&new_lines, spec);

        // Doc-only edit at file level: the versions differ only in comments
        // (doc comments are siblings of declarations — symbol hashes are
        // blind to them, spec §3-M2 doc_only detection).
        if strip_comments(&old_lines, spec) == strip_comments(&new_lines, spec) {
            for s in new.values() {
                changes.push(SymbolChange {
                    path: path.clone(),
                    lines: lines_of(&new_lines, s.start_byte, s.end_byte),
                    name: s.name.clone(),
                    kind_of: s.kind_of.clone(),
                    change: ChangeKind::DocOnly,
                    moved_from: None,
                    public: s.public,
                });
            }
            continue;
        }

        // Removed: in old, not in new.
        for (key, s) in &old {
            if !new.contains_key(key) {
                changes.push(SymbolChange {
                    path: path.clone(),
                    lines: "-".into(),
                    name: s.name.clone(),
                    kind_of: s.kind_of.clone(),
                    change: ChangeKind::Removed,
                    moved_from: None,
                    public: s.public,
                });
            }
        }
        // Added / changed.
        for (key, s) in &new {
            let lines = lines_of(&new_lines, s.start_byte, s.end_byte);
            match old.get(key) {
                None => {
                    changes.push(SymbolChange {
                        path: path.clone(),
                        lines,
                        name: s.name.clone(),
                        kind_of: s.kind_of.clone(),
                        change: ChangeKind::Added,
                        moved_from: None,
                        public: s.public,
                    });
                }
                Some(o) => {
                    if o.body_hash == s.body_hash {
                        continue; // unchanged
                    }
                    let change = if o.sig_hash == s.sig_hash {
                        // Identical structure (modulo renames/literals, which
                        // the canonical form collapses) but a different body:
                        // the only possible edits are literal-only changes.
                        // Doc-only edits never reach this path — they are
                        // handled at file level above.
                        ChangeKind::BodyChanged
                    } else {
                        ChangeKind::SignatureChanged
                    };
                    changes.push(SymbolChange {
                        path: path.clone(),
                        lines,
                        name: s.name.clone(),
                        kind_of: s.kind_of.clone(),
                        change,
                        moved_from: None,
                        public: s.public,
                    });
                }
            }
        }
    }

    // Moved detection: collapse a remove+add pair with the same name in
    // different files into a single `Moved` on both sides.
    let mut move_pairs: Vec<(usize, usize)> = Vec::new();
    for (i, c) in changes
        .iter()
        .enumerate()
        .filter(|(_, c)| c.change == ChangeKind::Removed)
    {
        if let Some(j) = changes
            .iter()
            .position(|a| a.change == ChangeKind::Added && a.name == c.name && a.path != c.path)
        {
            move_pairs.push((i, j));
        }
    }
    for (i, j) in move_pairs {
        let from = changes[i].path.clone();
        changes[i].change = ChangeKind::Moved;
        changes[i].moved_from = Some(changes[j].path.clone());
        changes[j].change = ChangeKind::Moved;
        changes[j].moved_from = Some(from);
    }

    // Impact: lower-bound caller counts per changed name (static mentions).
    let mut impact = BTreeMap::new();
    for c in &changes {
        let n = store.mention_count(&c.name).unwrap_or(0);
        impact.insert(c.name.clone(), n);
    }

    // Risk markers (deterministic).
    let mut risks = Vec::new();
    for c in &changes {
        match c.change {
            ChangeKind::SignatureChanged if c.public => risks.push(RiskMarker {
                severity: "high".into(),
                description: format!(
                    "公共 API 签名变更：`{}`（至少 {} 处引用）",
                    c.name,
                    impact.get(&c.name).copied().unwrap_or(0)
                ),
                anchors: vec![format!("{}:{}", c.path, c.lines)],
            }),
            ChangeKind::SignatureChanged | ChangeKind::BodyChanged
                if impact.get(&c.name).copied().unwrap_or(0) >= 5 =>
            {
                risks.push(RiskMarker {
                    severity: "medium".into(),
                    description: format!(
                        "高扇入符号被修改：`{}`（至少 {} 处调用）",
                        c.name,
                        impact.get(&c.name).copied().unwrap_or(0)
                    ),
                    anchors: vec![format!("{}:{}", c.path, c.lines)],
                });
            }
            _ => {}
        }
    }

    // Tests not updated?
    let src_changed = changed_files
        .iter()
        .any(|p| !p.contains("tests/") && !p.contains("/test/"));
    let tests_changed = changed_files.iter().any(|p| p.contains("tests/"));
    if src_changed && !tests_changed {
        risks.push(RiskMarker {
            severity: "low".into(),
            description: "源码变更但测试文件未同步变更".into(),
            anchors: changed_files.iter().map(|p| format!("{p}:1")).collect(),
        });
    }

    // Near-duplicate introduction for added symbols (M1 cross-check): a new
    // symbol sharing its name with an existing implementation elsewhere.
    if let Ok(all) = store.all_symbols() {
        for c in changes.iter().filter(|c| c.change == ChangeKind::Added) {
            if let Some(existing) = all
                .iter()
                .find(|s| s.name == c.name && s.file_path != c.path)
            {
                risks.push(RiskMarker {
                    severity: "medium".into(),
                    description: format!(
                        "新增符号 `{}` 与既有实现 `{}` 同名（疑似重复引入，建议 Spot 复核）",
                        c.name, existing.file_path
                    ),
                    anchors: vec![format!("{}:{}", c.path, c.lines)],
                });
            }
        }
    }

    Ok(ReplayReport {
        base: base.to_string(),
        head: head.to_string(),
        changes,
        risks,
        impact,
    })
}

/// Render the deterministic report as reviewer-friendly markdown.
///
/// This is the *structured fallback* (F6) — the LLM narration layer fills
/// slots in a template built from exactly these facts, never inventing new
/// ones. Every line carries its `path:line` anchor.
pub fn render_markdown(report: &ReplayReport) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# Replay：{}..{}\n\n",
        short(&report.base),
        short(&report.head)
    ));
    out.push_str("## 变更清单\n\n");
    for c in &report.changes {
        let anchor = format!("`{}:{}`", c.path, c.lines);
        let moved = c
            .moved_from
            .as_ref()
            .map(|f| format!("（自 `{f}` 移入）"))
            .unwrap_or_default();
        out.push_str(&format!(
            "- **{}** `{}` ({}) {}{}\n",
            change_label(c.change),
            c.name,
            c.kind_of,
            anchor,
            moved
        ));
    }
    if report.changes.is_empty() {
        out.push_str("- 无符号级变更\n");
    }
    out.push_str("\n## 影响面（下界估计，至少 N 处）\n\n");
    for (name, n) in &report.impact {
        if *n > 0 {
            out.push_str(&format!("- `{name}`：至少 {n} 处引用\n"));
        }
    }
    out.push_str("\n## 风险标记\n\n");
    for r in &report.risks {
        let anchors = r.anchors.join("、");
        out.push_str(&format!(
            "- **[{}]** {}（锚点：{}）\n",
            r.severity, r.description, anchors
        ));
    }
    if report.risks.is_empty() {
        out.push_str("- 无\n");
    }
    out
}

fn short(sha: &str) -> String {
    sha.chars().take(8).collect()
}

fn change_label(kind: ChangeKind) -> &'static str {
    match kind {
        ChangeKind::Added => "新增",
        ChangeKind::Removed => "移除",
        ChangeKind::SignatureChanged => "签名变更",
        ChangeKind::BodyChanged => "实现变更",
        ChangeKind::DocOnly => "仅文档",
        ChangeKind::Moved => "移动",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lang::RUST;

    #[test]
    fn surface_extracts_and_hashes_signatures() {
        let src = "pub fn debounce(f: Fn(u64), ms: u64) -> Fn(u64) { call(f, ms) }";
        let s = surface(src, &RUST);
        let (_, sym) = s.iter().next().unwrap();
        assert!(sym.public);
        assert!(sym.sig_hash.is_some());
    }

    #[test]
    fn signature_hash_ignores_body_but_not_params() {
        let a = surface("fn f(x: u64) -> u64 { x + 1 }", &RUST);
        let b = surface("fn f(x: u64) -> u64 { x * 999 }", &RUST);
        let c = surface("fn f(x: u64, y: u64) -> u64 { x + 1 }", &RUST);
        assert_eq!(
            a[&("f".to_string(), "function_item".to_string())].sig_hash,
            b[&("f".to_string(), "function_item".to_string())].sig_hash,
            "body edits must not change the signature hash"
        );
        assert_ne!(
            a[&("f".to_string(), "function_item".to_string())].sig_hash,
            c[&("f".to_string(), "function_item".to_string())].sig_hash,
            "parameter edits must change the signature hash"
        );
    }

    #[test]
    fn render_contains_anchors() {
        let report = ReplayReport {
            base: "aaaa1111".into(),
            head: "bbbb2222".into(),
            changes: vec![SymbolChange {
                path: "src/lib.rs".into(),
                lines: "10-14".into(),
                name: "debounce".into(),
                kind_of: "function_item".into(),
                change: ChangeKind::SignatureChanged,
                moved_from: None,
                public: true,
            }],
            risks: vec![],
            impact: BTreeMap::new(),
        };
        let md = render_markdown(&report);
        assert!(md.contains("src/lib.rs:10-14"));
        assert!(md.contains("签名变更"));
    }
}

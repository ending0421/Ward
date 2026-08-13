//! AST canonicalization for structural fingerprinting.
//!
//! The canonical form collapses every identifier to `ID` and every literal
//! to `LIT`, keeps all other node kinds, and drops comments. Two trees with
//! equal canonical forms are *structurally identical modulo renaming and
//! literals* — this is exactly what L1 `struct_hash` promises (spec §3-M1):
//! exact structural equality, no near-duplicate claims.

use tree_sitter::{Node, Tree};

/// Kinds treated as identifiers (collapsed to `ID`).
fn is_identifier_kind(kind: &str) -> bool {
    matches!(
        kind,
        "identifier"
            | "type_identifier"
            | "field_identifier"
            | "constant_identifier"
            | "scoped_identifier"
            | "scoped_type_identifier"
            | "self"
            | "super"
            | "crate"
    )
}

/// Kinds treated as literals (collapsed to `LIT`).
fn is_literal_kind(kind: &str) -> bool {
    kind.ends_with("_literal")
        || matches!(
            kind,
            "boolean_literal"
                | "integer_literal"
                | "float_literal"
                | "char_literal"
                | "string_literal"
                | "raw_string_literal"
                | "byte_string_literal"
        )
}

/// Kinds excluded from the canonical form entirely.
///
/// Comments are excluded so that doc-only edits leave `struct_hash`
/// unchanged — the basis of M2's `doc_only` change classification.
fn is_excluded(kind: &str) -> bool {
    kind == "line_comment" || kind == "block_comment" || kind == "comment"
}

fn write_node(node: &Node, out: &mut String) {
    let kind = node.kind();
    if is_excluded(kind) {
        return;
    }
    if is_identifier_kind(kind) {
        out.push_str("(ID)");
        return;
    }
    if is_literal_kind(kind) {
        out.push_str("(LIT)");
        return;
    }
    out.push('(');
    out.push_str(kind);
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        write_node(&child, out);
    }
    out.push(')');
}

/// Canonical structural form of a whole tree.
pub fn canonical_form(tree: &Tree) -> String {
    let mut out = String::with_capacity(1024);
    write_node(&tree.root_node(), &mut out);
    out
}

/// Canonical structural form of a single subtree (e.g. a signature without
/// its body — used by M2 to distinguish `signature_changed` from
/// `body_changed`).
pub fn canonical_form_of(node: &Node) -> String {
    let mut out = String::new();
    write_node(&node, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fingerprint::parse_rust;

    fn canon(src: &str) -> String {
        let tree = parse_rust(src).expect("parse");
        canonical_form(&tree)
    }

    #[test]
    fn renaming_does_not_change_form() {
        let a = canon("fn debounce(f: Fn, ms: u64) -> Fn { call(f, ms) }");
        let b = canon("fn throttle(g: Func, secs: u32) -> Func { call(g, secs) }");
        assert_eq!(a, b);
    }

    #[test]
    fn literal_changes_do_not_change_form() {
        let a = canon("fn limit(x: u64) -> u64 { x.min(100) }");
        let b = canon("fn limit(x: u64) -> u64 { x.min(999) }");
        assert_eq!(a, b);
    }

    #[test]
    fn comment_changes_do_not_change_form() {
        let a = canon("fn f() { let x = 1; x }");
        let b = canon("/// Documented!\nfn f() { let x = 1; x }");
        assert_eq!(a, b);
    }

    #[test]
    fn structural_changes_do_change_form() {
        let a = canon("fn f() { let x = 1; x }");
        let b = canon("fn f() { let x = 1; x + 1 }");
        assert_ne!(a, b);
    }
}

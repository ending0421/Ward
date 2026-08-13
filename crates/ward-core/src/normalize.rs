//! AST canonicalization for structural fingerprinting.
//!
//! The canonical form collapses every identifier to `ID` and every literal
//! to `LIT`, keeps all other node kinds, and drops comments. Two trees with
//! equal canonical forms are *structurally identical modulo renaming and
//! literals* — this is exactly what L1 `struct_hash` promises (spec §3-M1):
//! exact structural equality, no near-duplicate claims.
//!
//! All decisions are table-driven by the [`LanguageSpec`] (spec §3.0):
//! identifier kinds, literal heuristic and comment kinds per language.

use crate::lang::LanguageSpec;
use tree_sitter::{Node, Tree};

fn write_node(node: &Node, spec: &LanguageSpec, out: &mut String) {
    let kind = node.kind();
    if spec.is_comment_kind(kind) {
        return;
    }
    if spec.is_identifier_kind(kind) {
        out.push_str("(ID)");
        return;
    }
    if spec.is_literal_kind(kind) {
        out.push_str("(LIT)");
        return;
    }
    out.push('(');
    out.push_str(kind);
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        write_node(&child, spec, out);
    }
    out.push(')');
}

/// Canonical structural form of a whole tree.
pub fn canonical_form(tree: &Tree, spec: &LanguageSpec) -> String {
    let mut out = String::with_capacity(1024);
    write_node(&tree.root_node(), spec, &mut out);
    out
}

/// Canonical structural form of a single subtree (e.g. a signature without
/// its body — used by M2 to distinguish `signature_changed` from
/// `body_changed`).
pub fn canonical_form_of(node: &Node, spec: &LanguageSpec) -> String {
    let mut out = String::new();
    write_node(node, spec, &mut out);
    out
}

/// Canonical form of a node *excluding* one child subtree (by node id) —
/// the signature form: the whole declaration minus its `body` field.
pub fn canonical_form_excluding(node: &Node, excluded: Node, spec: &LanguageSpec) -> String {
    let mut out = String::new();
    write_node_excluding(node, excluded, spec, &mut out);
    out
}

fn write_node_excluding(node: &Node, excluded: Node, spec: &LanguageSpec, out: &mut String) {
    if node.id() == excluded.id() {
        return;
    }
    let kind = node.kind();
    if spec.is_comment_kind(kind) {
        return;
    }
    if spec.is_identifier_kind(kind) {
        out.push_str("(ID)");
        return;
    }
    if spec.is_literal_kind(kind) {
        out.push_str("(LIT)");
        return;
    }
    out.push('(');
    out.push_str(kind);
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        write_node_excluding(&child, excluded, spec, out);
    }
    out.push(')');
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fingerprint::parse;
    use crate::lang::{Language, RUST};

    fn canon_rust(src: &str) -> String {
        let tree = parse(Language::Rust, src).expect("parse");
        canonical_form(&tree, &RUST)
    }

    #[test]
    fn renaming_does_not_change_form() {
        let a = canon_rust("fn debounce(f: Fn, ms: u64) -> Fn { call(f, ms) }");
        let b = canon_rust("fn throttle(g: Func, secs: u32) -> Func { call(g, secs) }");
        assert_eq!(a, b);
    }

    #[test]
    fn literal_changes_do_not_change_form() {
        let a = canon_rust("fn limit(x: u64) -> u64 { x.min(100) }");
        let b = canon_rust("fn limit(x: u64) -> u64 { x.min(999) }");
        assert_eq!(a, b);
    }

    #[test]
    fn comment_changes_do_not_change_form() {
        let a = canon_rust("fn f() { let x = 1; x }");
        let b = canon_rust("/// Documented!\nfn f() { let x = 1; x }");
        assert_eq!(a, b);
    }

    #[test]
    fn structural_changes_do_change_form() {
        let a = canon_rust("fn f() { let x = 1; x }");
        let b = canon_rust("fn f() { let x = 1; x + 1 }");
        assert_ne!(a, b);
    }

    #[test]
    fn java_comments_and_literals_normalize() {
        let spec = Language::Java.spec();
        let a = parse(Language::Java, "class Foo { int bar() { return 1; } }").unwrap();
        let b = parse(
            Language::Java,
            "class Renamed { int renamedFn() { return 999; } }",
        )
        .unwrap();
        assert_eq!(canonical_form(&a, spec), canonical_form(&b, spec));
    }

    #[test]
    fn swift_literals_normalize() {
        let spec = Language::Swift.spec();
        let a = parse(Language::Swift, "func f() -> Int { return 1 }").unwrap();
        let b = parse(Language::Swift, "func f() -> Int { return 2 }").unwrap();
        assert_eq!(canonical_form(&a, spec), canonical_form(&b, spec));
    }
}

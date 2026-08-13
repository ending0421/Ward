//! The four-layer fingerprint system behind Spot (spec §3-M1).
//!
//! | Layer | Fingerprint | Captures |
//! | :--- | :--- | :--- |
//! | L0 | `body_hash` — blake3 of the raw source text | exact clones |
//! | L1 | `struct_hash` — blake3 of the canonical form | clones + pure rename + literal substitution |
//! | L2 | `simhash` — 64-bit Charikar simhash over subtree features (a DECKARD-*inspired* variant; DECKARD itself used subtree characteristic vectors + LSH with Euclidean distance, ICSE 2007) | structural near-duplicates (copy-then-modify) |
//! | L3 | embedding (sqlite-vec, later) | semantic clones — recall only, never thresholds |
//!
//! A hash only expresses *equality*; near-duplicates are the job of the
//! L2 simhash, where similarity ≈ Jaccard of subtree-feature multisets via
//! Hamming distance.

use blake3::Hasher;
use tree_sitter::{Node, Tree};

/// Parse Rust source with the tree-sitter-rust grammar.
pub fn parse_rust(source: &str) -> Option<Tree> {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .ok()?;
    parser.parse(source, None)
}

/// L0: CAS hash of the raw source text.
pub fn body_hash(source: &str) -> String {
    let mut h = Hasher::new();
    h.update(source.as_bytes());
    h.finalize().to_hex().to_string()
}

/// L1: hash of the canonical structural form.
pub fn struct_hash(tree: &Tree) -> String {
    let form = crate::normalize::canonical_form(tree);
    let mut h = Hasher::new();
    h.update(form.as_bytes());
    h.finalize().to_hex().to_string()
}

/// One subtree feature: `parent_kind|kind|child_kinds...`.
///
/// Context-window 1 (DECKARD-style characteristic vector per node position).
/// Kinds only — identifiers are deliberately absent so renaming leaves the
/// feature multiset unchanged.
fn node_feature(node: &Node) -> u64 {
    let parent = node
        .parent()
        .map(|p| p.kind().to_string())
        .unwrap_or_default();
    let mut f = String::with_capacity(64);
    f.push_str(&parent);
    f.push('|');
    f.push_str(node.kind());
    f.push('|');
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        f.push_str(child.kind());
        f.push(',');
    }
    feature_hash(&f)
}

fn feature_hash(feature: &str) -> u64 {
    let mut h = Hasher::new();
    h.update(feature.as_bytes());
    let out = h.finalize();
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&out.as_bytes()[..8]);
    u64::from_le_bytes(bytes)
}

/// Collect the subtree-feature multiset of a tree.
pub fn subtree_features(tree: &Tree) -> Vec<u64> {
    let mut features = Vec::new();
    let mut cursor = tree.walk();
    for node in tree.root_node().named_children(&mut cursor) {
        collect_features(&node, &mut features);
    }
    features
}

/// Collect the subtree-feature multiset of a single node (used for per-symbol
/// simhashes during indexing).
pub fn subtree_features_of(node: &Node) -> Vec<u64> {
    let mut features = Vec::new();
    collect_features(node, &mut features);
    features
}

/// Subtree-feature multiset of a node excluding one child subtree (the
/// signature form: declaration minus body). Signature-shaped Spot queries
/// compare against the signature simhash, not the full-body one.
pub fn subtree_features_excluding(node: &Node, excluded: Node) -> Vec<u64> {
    let mut features = Vec::new();
    collect_features_excluding(node, excluded, &mut features);
    features
}

fn collect_features_excluding(node: &Node, excluded: Node, out: &mut Vec<u64>) {
    if node.id() == excluded.id() {
        return;
    }
    out.push(node_feature(node));
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_features_excluding(&child, excluded, out);
    }
}

fn collect_features(node: &Node, out: &mut Vec<u64>) {
    out.push(node_feature(node));
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_features(&child, out);
    }
}

/// L2: Charikar simhash over the feature multiset (multiplicity-weighted).
pub fn simhash(features: &[u64]) -> u64 {
    let mut v = [0i64; 64];
    for &f in features {
        for (i, slot) in v.iter_mut().enumerate() {
            if (f >> i) & 1 == 1 {
                *slot += 1;
            } else {
                *slot -= 1;
            }
        }
    }
    let mut out = 0u64;
    for (i, &slot) in v.iter().enumerate() {
        if slot >= 0 {
            out |= 1 << i;
        }
    }
    out
}

/// Signature simhash for a *query* tree: features of the first symbol with
/// its body excluded.
///
/// Two normalization steps make signature-shaped queries comparable to
/// indexed symbols:
/// * tolerant parses of signature-only snippets produce a
///   `function_signature_item` (with `has_error`) — aliased to
///   `function_item`;
/// * the root's parent kind is pinned to `source_file`, matching the parent
///   of indexed top-level symbols.
pub fn signature_simhash(tree: &Tree) -> Option<u64> {
    let root = tree.root_node();
    let mut cursor = root.walk();
    let node = root.named_children(&mut cursor).next()?;
    let alias = match node.kind() {
        "function_signature_item" => Some("function_item"),
        _ => None,
    };
    let body = node.child_by_field_name("body");
    let mut features = Vec::new();
    collect_features_alias(&node, Some("source_file"), alias, body, &mut features);
    Some(simhash(&features))
}

fn collect_features_alias(
    node: &Node,
    parent_kind: Option<&str>,
    self_alias: Option<&str>,
    excluded: Option<Node>,
    out: &mut Vec<u64>,
) {
    if Some(node.id()) == excluded.map(|e| e.id()) {
        return;
    }
    let kind = self_alias.unwrap_or_else(|| node.kind());
    out.push(feature_with_parent(parent_kind, kind, node));
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_features_alias(&child, Some(kind), None, excluded, out);
    }
}

fn feature_with_parent(parent_kind: Option<&str>, kind: &str, node: &Node) -> u64 {
    let mut f = String::with_capacity(64);
    f.push_str(parent_kind.unwrap_or(""));
    f.push('|');
    f.push_str(kind);
    f.push('|');
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        f.push_str(child.kind());
        f.push(',');
    }
    feature_hash(&f)
}

/// Hamming distance between two simhashes (0..=64).
pub fn simhash_distance(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

/// Similarity in [0, 1] derived from the Hamming distance.
///
/// This is an *initial-value* mapping (spec: thresholds are declared
/// calibration starting points, recalibrated weekly against a golden set).
pub fn simhash_similarity(a: u64, b: u64) -> f64 {
    1.0 - (simhash_distance(a, b) as f64 / 64.0)
}

/// Similarity between two source snippets, computed end-to-end.
pub fn similarity(source_a: &str, source_b: &str) -> Option<f64> {
    let tree_a = parse_rust(source_a)?;
    let tree_b = parse_rust(source_b)?;
    let a = simhash(&subtree_features(&tree_a));
    let b = simhash(&subtree_features(&tree_b));
    Some(simhash_similarity(a, b))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FN_A: &str = "fn debounce(f: Fn(u64), ms: u64) -> Fn(u64) { call(f, ms) }";
    const FN_A_RENAMED: &str = "fn throttle(g: Fn(u32), secs: u32) -> Fn(u32) { call(g, secs) }";
    const FN_B: &str = "fn quicksort(v: &mut [i32]) { if v.len() <= 1 { return } let p = v[0]; quicksort(&mut v[1..]) }";
    const FN_A_MODIFIED: &str =
        "fn debounce(f: Fn(u64), ms: u64) -> Fn(u64) { let t = ms + 1; call(f, t) }";

    #[test]
    fn l0_body_hash_is_exact() {
        assert_eq!(body_hash(FN_A), body_hash(FN_A));
        assert_ne!(body_hash(FN_A), body_hash(FN_A_RENAMED));
    }

    #[test]
    fn l1_struct_hash_ignores_rename() {
        let a = parse_rust(FN_A).unwrap();
        let b = parse_rust(FN_A_RENAMED).unwrap();
        assert_eq!(struct_hash(&a), struct_hash(&b));
    }

    #[test]
    fn l1_struct_hash_differs_on_structure() {
        let a = parse_rust(FN_A).unwrap();
        let b = parse_rust(FN_B).unwrap();
        assert_ne!(struct_hash(&a), struct_hash(&b));
    }

    #[test]
    fn l2_simhash_high_for_rename() {
        let s = similarity(FN_A, FN_A_RENAMED).unwrap();
        assert!(s > 0.95, "rename similarity should be ~1.0, got {s}");
    }

    #[test]
    fn l2_simhash_high_for_copy_then_modify() {
        let s = similarity(FN_A, FN_A_MODIFIED).unwrap();
        assert!(
            s > 0.75,
            "near-duplicate similarity should be high, got {s}"
        );
    }

    #[test]
    fn l2_simhash_low_for_unrelated() {
        let s = similarity(FN_A, FN_B).unwrap();
        assert!(s < 0.8, "unrelated functions should score low, got {s}");
    }

    #[test]
    fn simhash_is_symmetric_and_bounded() {
        let a = simhash(&subtree_features(&parse_rust(FN_A).unwrap()));
        let b = simhash(&subtree_features(&parse_rust(FN_B).unwrap()));
        assert_eq!(simhash_distance(a, b), simhash_distance(b, a));
        assert!(simhash_similarity(a, a) > 0.999);
    }

    #[test]
    fn signature_query_matches_symbol_signature() {
        // A signature-only query snippet (which tolerant parsing turns into
        // a `function_signature_item`) must align with the signature simhash
        // of the full function it came from.
        let full = "pub fn simhash(features: &[u64]) -> u64 { let mut v = [0i64; 64]; v[0] = 1; let mut o = 0u64; o }";
        let sig = "pub fn simhash(features: &[u64]) -> u64";
        let t_full = parse_rust(full).unwrap();
        let t_sig = parse_rust(sig).unwrap();
        let sym_node = t_full.root_node().named_child(0).unwrap();
        let body = sym_node.child_by_field_name("body").unwrap();
        let sym_sig = simhash(&subtree_features_excluding(&sym_node, body));
        let q_sim = signature_simhash(&t_sig).unwrap();
        assert!(
            simhash_similarity(sym_sig, q_sim) > 0.9,
            "signature query must align with symbol signature (dist={})",
            simhash_distance(sym_sig, q_sim)
        );
    }
}

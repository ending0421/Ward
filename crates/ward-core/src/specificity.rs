//! Signature specificity (issue #5): the fraction of a signature's
//! parameter types that are DOMAIN types (project-local named types) as
//! opposed to basic/std types.
//!
//! Kind-only structural fingerprints (DECKARD-style) deliberately collapse
//! type NAMES — `&ModelRect` and `&BufferPool` produce the same feature.
//! For signatures composed entirely of basic types ("function with N scalar
//! params") the fingerprint degenerates to a shape that matches hundreds of
//! unrelated helpers: 34/34 false positives in the issue's golden set, at
//! every similarity threshold. Specificity makes that degeneracy visible so
//! the grading layer can cap low-specificity queries at Weak (automated
//! gates ignore them; humans still see them).

use crate::lang::Language;
use tree_sitter::Node;

/// Basic/std type names per language (lowercased for matching). Anything
/// NOT in this table (and not a container/pointer wrapper around table
/// entries) counts as a domain type.
const BASIC_RUST: &[&str] = &[
    "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64", "i128", "isize", "f32",
    "f64", "bool", "char", "str", "string", "fn", "fnmut", "fnonce", "vec", "option", "result",
    "box", "arc", "rc", "cell", "refcell", "hashmap", "btreemap", "hashset", "btreeset",
    "vecdeque", "path", "pathbuf", "osstring", "osstr", "cow",
];
const BASIC_KOTLIN: &[&str] = &[
    "int",
    "long",
    "short",
    "byte",
    "double",
    "float",
    "boolean",
    "char",
    "string",
    "unit",
    "list",
    "map",
    "set",
    "mutablelist",
    "mutablemap",
    "mutableset",
    "pair",
    "triple",
    "sequence",
    "array",
];
const BASIC_SWIFT: &[&str] = &[
    "int",
    "int64",
    "int32",
    "int16",
    "int8",
    "uint",
    "uint64",
    "uint32",
    "uint16",
    "uint8",
    "double",
    "float",
    "bool",
    "string",
    "character",
    "substring",
    "array",
    "dictionary",
    "set",
    "optional",
];
const BASIC_JAVA: &[&str] = &[
    "int",
    "long",
    "short",
    "byte",
    "double",
    "float",
    "boolean",
    "char",
    "string",
    "integer",
    "long",
    "short",
    "byte",
    "double",
    "float",
    "boolean",
    "character",
    "list",
    "map",
    "set",
    "optional",
    "object",
    "void",
];
const BASIC_OBJC: &[&str] = &[
    "int",
    "long",
    "nsinteger",
    "nsuinteger",
    "cgfloat",
    "bool",
    "nsstring",
    "nsnumber",
    "nsarray",
    "nsdictionary",
    "nsdata",
    "id",
];

fn basic_table(lang: Language) -> &'static [&'static str] {
    match lang {
        Language::Rust => BASIC_RUST,
        Language::Kotlin => BASIC_KOTLIN,
        Language::Swift => BASIC_SWIFT,
        Language::Java => BASIC_JAVA,
        Language::ObjC => BASIC_OBJC,
    }
}

/// Is this type node a basic/std type? Recursive: pointer/container nodes
/// (references, generics, arrays, optionals, function types) classify by
/// their contents; primitive kinds always count as basic; named types
/// classify by name against the table.
fn is_basic_type(node: &Node, source: &str, lang: Language) -> bool {
    let kind = node.kind();
    if kind.contains("primitive") || kind.contains("integral") || kind.contains("floating") {
        return true;
    }
    if kind == "function_type" {
        // Closures/fn pointers: basic unless a domain type hides inside.
        return named_children_of(node, source)
            .all(|c| c.kind() == "identifier" || is_basic_type(&c, source, lang));
    }
    // Named type leaves: classify by text.
    let text = node.utf8_text(source.as_bytes()).unwrap_or("");
    let name = text
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .rfind(|s| !s.is_empty())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !name.is_empty() && basic_table(lang).contains(&name.as_str()) {
        return true;
    }
    // Containers/pointers: basic iff every named child is basic (recursion
    // decides whether a domain type hides inside `Vec<ModelRect>`).
    let children: Vec<Node> = named_children_of(node, source).collect();
    !children.is_empty() && children.iter().all(|c| is_basic_type(c, source, lang))
}

fn named_children_of<'a>(node: &'a Node, source: &str) -> impl Iterator<Item = Node<'a>> {
    let mut cursor = node.walk();
    let children: Vec<Node> = node.named_children(&mut cursor).collect();
    // Silence unused-source warnings on grammars with fields (source is
    // used by is_basic_type's utf8_text).
    let _ = source;
    children.into_iter()
}

/// The signature's parameter type nodes (language-heuristic: each
/// parameter's named children minus its leading name identifier).
fn parameter_types<'a>(symbol: &Node<'a>, source: &str) -> Vec<Node<'a>> {
    let mut params: Vec<Node> = Vec::new();
    let mut cursor = symbol.walk();
    for child in symbol.named_children(&mut cursor) {
        // tree-sitter-swift wraps body-less declarations in ERROR with the
        // parameter nodes attached to the wrapper itself.
        if child.kind() == "parameter" || child.kind() == "formal_parameter" {
            params.push(child);
            continue;
        }
        if child.kind() == "parameters" || child.kind() == "function_value_parameters" {
            let mut pc = child.walk();
            for p in child.named_children(&mut pc) {
                if p.kind() == "parameter" || p.kind() == "formal_parameter" {
                    let mut tc = p.walk();
                    let types: Vec<Node> = p
                        .named_children(&mut tc)
                        .filter(|c| {
                            // skip the parameter NAME (first identifier-ish
                            // child); everything else is part of the type
                            !c.kind().contains("identifier") || c.child_count() > 0
                        })
                        .collect();
                    if !types.is_empty() {
                        params.extend(types);
                    }
                }
            }
        }
    }
    let _ = source;
    params
}

/// Specificity of a signature: domain-typed params / total params, in
/// [0,1]. Zero-param signatures are 0.0 — a shape-only query.
pub fn signature_specificity(lang: Language, sig: &str) -> Option<f64> {
    let tree = crate::fingerprint::parse(lang, sig)?;
    let root = tree.root_node();
    let mut cursor = root.walk();
    let first = root.named_children(&mut cursor).next()?;
    let was_error_wrapped = first.kind() == "ERROR";
    // ERROR-wrapped tolerant parses keep their declaration pieces (name +
    // parameters) directly on the wrapper — use it as the container.
    let symbol = first;
    let types = parameter_types(&symbol, sig);
    if types.is_empty() {
        // Tolerant parses of signature-only snippets (Swift body-less
        // top-level funcs) carry ERROR wrappers even for valid
        // declarations; a wrapped, parameterless node is garbage instead.
        if was_error_wrapped && tree.root_node().has_error() {
            return None;
        }
        return Some(0.0);
    }
    let domain = types
        .iter()
        .filter(|t| !is_basic_type(t, sig, lang))
        .count();
    Some(domain as f64 / types.len() as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_basic_signatures_are_zero() {
        assert_eq!(
            signature_specificity(
                Language::Rust,
                "pub fn debounce(f: &dyn Fn(u64), ms: u64) -> u8"
            ),
            Some(0.0)
        );
        assert_eq!(
            signature_specificity(Language::Kotlin, "fun add(a: Long, b: Long): Long"),
            Some(0.0)
        );
        assert_eq!(
            signature_specificity(Language::Swift, "func add(_ a: Int, _ b: Int) -> Int"),
            Some(0.0)
        );
    }

    #[test]
    fn domain_types_raise_specificity() {
        assert_eq!(
            signature_specificity(
                Language::Rust,
                "pub fn push_fill(rect: &ModelRect, out: &mut Vec<PathFillVertex>) -> u8"
            ),
            Some(1.0)
        );
        // Mixed: one domain, one basic.
        assert!(
            (signature_specificity(
                Language::Rust,
                "pub fn paint(rect: &ModelRect, color: u32) -> u8"
            )
            .unwrap()
                - 0.5)
                .abs()
                < 1e-9
        );
    }

    #[test]
    fn basic_containers_of_domain_types_count_as_domain() {
        assert_eq!(
            signature_specificity(Language::Rust, "pub fn batch(v: Vec<ModelRect>) -> u8"),
            Some(1.0)
        );
        assert_eq!(
            signature_specificity(Language::Rust, "pub fn raw(v: Vec<u8>) -> u8"),
            Some(0.0)
        );
    }

    #[test]
    fn zero_param_signatures_are_zero() {
        assert_eq!(
            signature_specificity(Language::Rust, "pub fn render() -> u8"),
            Some(0.0)
        );
    }

    #[test]
    fn garbage_is_none() {
        assert_eq!(signature_specificity(Language::Rust, "fn broken( {"), None);
    }
}

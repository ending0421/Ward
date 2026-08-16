//! UniFFI UDL extractor (spec §3.0 interface contract, 0.5-2).
//!
//! UDL (`.udl`) files define the cross-language interface surface: the
//! Rust core exports it, UniFFI generates the Kotlin/Swift bindings from
//! it. It is NOT parsed by tree-sitter (no grammar exists — verified
//! 2026-08), so a thin hand-rolled extractor covers the ~6 UDL constructs.
//! The interface declarations become index symbols (`language: "udl"`),
//! making UDL changes visible to Replay and to the FFI guard.
//!
//! Scope discipline (P4): we parse structure only — braces, keywords and
//! declaration text. Semantic resolution stays with UniFFI itself.

use serde::{Deserialize, Serialize};

/// One extracted UDL definition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UdlSymbol {
    /// Interface/record/dictionary/enum/typedef/function name.
    pub name: String,
    /// `udl_interface` | `udl_record` | `udl_dictionary` | `udl_enum` |
    /// `udl_function` | `udl_typedef`.
    pub kind: String,
    pub start_byte: usize,
    pub end_byte: usize,
    /// The raw declaration text (comment/whitespace-normalized source).
    pub raw: String,
}

/// Comment-stripped, whitespace-normalized UDL source. Type names survive —
/// the interface CONTRACT includes types; identifier collapsing would
/// hide breaking changes (renaming a field breaks generated bindings).
fn normalized(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut chars = source.chars().peekable();
    let mut pending_space = false;
    while let Some(c) = chars.next() {
        match c {
            '/' if chars.peek() == Some(&'/') => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
                pending_space = true;
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut prev = '\0';
                for c in chars.by_ref() {
                    if prev == '*' && c == '/' {
                        break;
                    }
                    prev = c;
                }
                pending_space = true;
            }
            c if c.is_whitespace() => pending_space = true,
            c => {
                if pending_space && !out.is_empty() && !out.ends_with(' ') {
                    out.push(' ');
                }
                pending_space = false;
                out.push(c);
            }
        }
    }
    for (a, b) in [
        ("( ", "("),
        (" )", ")"),
        (") ", ")"),
        (" ,", ","),
        (", ", ","),
        (" ;", ";"),
        ("; ", ";"),
        (" {", "{"),
        ("} ", "}"),
        (" =", "="),
        ("= ", "="),
    ] {
        out = out.replace(a, b);
    }
    out
}

/// Extract every UDL definition. Nested interface methods are folded into
/// their interface (the interface is the change unit Replay reports).
pub fn extract(source: &str) -> Vec<UdlSymbol> {
    let src = normalized(source);
    let bytes = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        let rest = &src[i..];
        // Skip structural keywords (namespace/callback) and attribute
        // annotations; `{`/`}`/`;` delimiters are skipped inline.
        if rest.starts_with("namespace ") || rest.starts_with("callback interface ") {
            let keyword_end = rest.find(' ').unwrap_or(rest.len());
            i += keyword_end + 1;
            i = skip_word(&src, i);
            while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'{') {
                i += 1;
            }
            continue;
        }
        if bytes[i] == b'[' {
            while i < bytes.len() && bytes[i] != b']' {
                i += 1;
            }
            i = (i + 1).min(bytes.len());
            while i < bytes.len() && bytes[i] == b' ' {
                i += 1;
            }
            continue;
        }
        if bytes[i] == b'{' || bytes[i] == b'}' || bytes[i] == b';' {
            i += 1;
            while i < bytes.len() && bytes[i] == b' ' {
                i += 1;
            }
            continue;
        }
        let Some(kind) = ["interface ", "record ", "dictionary ", "enum ", "typedef "]
            .iter()
            .find(|k| src[i..].starts_with(**k))
            .map(|k| (*k).trim_end())
        else {
            // Top-level function inside a namespace:
            // `Type name(args);` — capture as udl_function.
            if let Some((name, end)) = top_level_function(&src, i) {
                let raw = src[i..end].trim().to_string();
                out.push(UdlSymbol {
                    name,
                    kind: "udl_function".into(),
                    start_byte: i,
                    end_byte: end,
                    raw,
                });
                i = end;
                continue;
            }
            i += 1;
            continue;
        };
        let kind_key = match kind {
            "interface" => "udl_interface",
            "record" => "udl_record",
            "dictionary" => "udl_dictionary",
            "enum" => "udl_enum",
            _ => "udl_typedef",
        };
        let name_start = i + kind.len() + 1;
        let tail = &src[name_start..];
        let name_len = tail
            .find(|c: char| !(c.is_alphanumeric() || c == '_'))
            .unwrap_or(tail.len());
        let name = &src[name_start..name_start + name_len];
        if name.is_empty() || !name.chars().next().is_some_and(|c| c.is_alphabetic()) {
            i = name_start + name_len;
            continue;
        }
        let end = definition_end(&src, name_start + name_len);
        let raw = src[i..end].trim().to_string();
        out.push(UdlSymbol {
            name: name.to_string(),
            kind: kind_key.into(),
            start_byte: i,
            end_byte: end,
            raw,
        });
        i = end;
    }
    out
}

/// Index just past the next identifier-ish word starting at `i`.
fn skip_word(src: &str, i: usize) -> usize {
    src[i..]
        .char_indices()
        .find(|(_, c)| !(c.is_alphanumeric() || *c == '_' || *c == '.'))
        .map(|(p, _)| i + p)
        .unwrap_or(src.len())
}

/// The end byte of a braced or semicolon-terminated definition starting at
/// the position just after its name.
fn definition_end(src: &str, from: usize) -> usize {
    let bytes = src.as_bytes();
    let mut i = from;
    // skip to '{' or ';' (typedef) or '(' (function)
    while i < bytes.len() && bytes[i] != b'{' && bytes[i] != b';' && bytes[i] != b'(' {
        i += 1;
    }
    if i >= bytes.len() {
        return src.len();
    }
    if bytes[i] == b';' {
        return i + 1;
    }
    if bytes[i] == b'(' {
        while i < bytes.len() && bytes[i] != b';' {
            i += 1;
        }
        return (i + 1).min(src.len());
    }
    // '{' — match braces (no nested braces inside UDL definitions, but
    // string literals may contain them; the comment stripper already
    // removed comments, so a simple depth scan is safe).
    let mut depth = 0i32;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    src.len()
}

/// A top-level namespace function: `Type name(args);` at depth 0.
fn top_level_function(src: &str, i: usize) -> Option<(String, usize)> {
    let rest = &src[i..];
    let semi = rest.find(';')?;
    let head = &rest[..semi];
    let paren = head.find('(')?;
    let args_end = head.rfind(')')?;
    if paren >= args_end {
        return None;
    }
    let before = head[..paren].trim();
    let name = before
        .split_whitespace()
        .last()?
        .trim_end_matches(|c: char| !c.is_alphanumeric());
    if name.is_empty() {
        return None;
    }
    Some((name.to_string(), i + semi + 1))
}

/// blake3 hash of the declaration (the UDL contract fingerprint).
pub fn declaration_hash(sym: &UdlSymbol) -> String {
    let mut h = blake3::Hasher::new();
    h.update(sym.raw.as_bytes());
    h.finalize().to_hex().to_string()
}

/// Simhash over the declaration's tokens (near-duplicate detection).
pub fn declaration_simhash(sym: &UdlSymbol) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let tokens: Vec<u64> = sym
        .raw
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|t| !t.is_empty())
        .map(|t| {
            let mut h = DefaultHasher::new();
            t.hash(&mut h);
            h.finish()
        })
        .collect();
    let mut v = [0i64; 64];
    for t in &tokens {
        for (i, slot) in v.iter_mut().enumerate() {
            if (t >> i) & 1 == 1 {
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

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
namespace calculator {
    [Throws=CalculatorError]
    interface Calculator {
        [Throws=CalculatorError]
        u32 add(u32 a, u32 b);
        string describe();
    };

    dictionary Config {
        u32 precision = 2;
        boolean verbose;
    };

    record Point {
        f64 x;
        f64 y;
    };

    enum Operation {
        "Add",
        "Subtract",
    };

    typedef string Alias;

    [Throws=CalculatorError]
    u32 free_add(u32 a, u32 b);
};
"#;

    #[test]
    fn extracts_all_six_constructs() {
        let syms = extract(SAMPLE);
        let kinds: Vec<&str> = syms.iter().map(|s| s.kind.as_str()).collect();
        for expected in [
            "udl_interface",
            "udl_dictionary",
            "udl_record",
            "udl_enum",
            "udl_typedef",
            "udl_function",
        ] {
            assert!(kinds.contains(&expected), "missing {expected}: {kinds:?}");
        }
        let iface = syms.iter().find(|s| s.name == "Calculator").unwrap();
        assert!(iface.raw.contains("u32 add"));
        assert!(iface.raw.contains("string describe"));
        let free = syms.iter().find(|s| s.kind == "udl_function").unwrap();
        assert_eq!(free.name, "free_add");
    }

    #[test]
    fn comments_and_whitespace_do_not_change_the_fingerprint() {
        let a = extract(SAMPLE)
            .into_iter()
            .find(|s| s.name == "Calculator")
            .unwrap();
        // Same contract, different comments and whitespace. (Attributes are
        // part of the contract and survive in the fingerprint.)
        let b = extract(
            "namespace n {\n\n interface Calculator {\n [Throws=CalculatorError] u32 add( u32 a, u32 b ) ; // add\n string describe();\n }; };",
        )
        .into_iter()
        .find(|s| s.name == "Calculator")
        .unwrap();
        assert_eq!(declaration_hash(&a), declaration_hash(&b));
    }

    #[test]
    fn field_rename_changes_the_fingerprint() {
        let a = extract("namespace n { record R { f64 x; }; };")
            .into_iter()
            .find(|s| s.name == "R")
            .unwrap();
        let b = extract("namespace n { record R { f64 y; }; };")
            .into_iter()
            .find(|s| s.name == "R")
            .unwrap();
        assert_ne!(declaration_hash(&a), declaration_hash(&b));
    }
}

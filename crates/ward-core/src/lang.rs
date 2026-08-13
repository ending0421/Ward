//! Language registry — the five first-class Ward languages (spec §3.0).
//!
//! Each language is described by a [`LanguageSpec`]: the grammar, the symbol
//! kinds it indexes, the container kinds it descends into, its identifier /
//! comment taxonomy and (for query alignment) a kind alias. All extraction,
//! normalization and fingerprinting is table-driven off these specs, so
//! adding a language means adding a grammar crate and one spec row — never
//! touching engine logic.
//!
//! Grammars that are not compiled in resolve to `None` and files of that
//! language are skipped fail-open during indexing.

use std::path::Path;

/// Ward's first-class language set (spec §3.0 rollout order).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    Rust,
    Kotlin,
    Swift,
    Java,
    ObjC,
}

impl Language {
    /// Language key as stored in the index (`symbols.language`).
    pub fn as_str(self) -> &'static str {
        match self {
            Language::Rust => "rust",
            Language::Kotlin => "kotlin",
            Language::Swift => "swift",
            Language::Java => "java",
            Language::ObjC => "objc",
        }
    }

    /// Detect the language for a file path.
    pub fn from_path(path: &Path) -> Option<Self> {
        match path.extension()?.to_str()? {
            "rs" => Some(Language::Rust),
            "kt" | "kts" => Some(Language::Kotlin),
            "swift" => Some(Language::Swift),
            "java" => Some(Language::Java),
            "m" | "h" | "mm" => Some(Language::ObjC),
            _ => None,
        }
    }

    /// The tree-sitter grammar for this language, when compiled in.
    pub fn ts_language(self) -> Option<tree_sitter::Language> {
        match self {
            Language::Rust => Some(tree_sitter_rust::LANGUAGE.into()),
            Language::Java => Some(tree_sitter_java::LANGUAGE.into()),
            Language::Kotlin => Some(tree_sitter_kotlin::LANGUAGE.into()),
            Language::Swift => Some(tree_sitter_swift::LANGUAGE.into()),
            Language::ObjC => Some(tree_sitter_objc::LANGUAGE.into()),
        }
    }

    /// The spec for this language.
    pub fn spec(self) -> &'static LanguageSpec {
        match self {
            Language::Rust => &RUST,
            Language::Java => &JAVA,
            Language::Kotlin => &KOTLIN,
            Language::Swift => &SWIFT,
            Language::ObjC => &OBJC,
        }
    }

    /// All languages in rollout order.
    pub const ALL: [Language; 5] = [
        Language::Rust,
        Language::Kotlin,
        Language::Java,
        Language::Swift,
        Language::ObjC,
    ];
}

/// Static description of one language's syntax surface.
#[derive(Debug)]
pub struct LanguageSpec {
    pub lang: Language,
    /// Node kinds indexed as symbols.
    pub symbol_kinds: &'static [&'static str],
    /// Node kinds descended into while hunting symbols (impl/class bodies,
    /// modules, the root, blocks).
    pub container_kinds: &'static [&'static str],
    /// Field name holding a symbol's name (almost always "name").
    pub name_field: &'static str,
    /// Node kinds collapsed to `ID` during canonicalization.
    pub identifier_kinds: &'static [&'static str],
    /// Query-only kind alias: tolerant parses of signature-only snippets
    /// produce this kind, which must compare as the indexed kind.
    pub query_alias: Option<(&'static str, &'static str)>,
}

impl LanguageSpec {
    pub fn is_symbol_kind(&self, kind: &str) -> bool {
        self.symbol_kinds.contains(&kind)
    }

    pub fn is_container_kind(&self, kind: &str) -> bool {
        self.container_kinds.contains(&kind)
    }

    pub fn is_identifier_kind(&self, kind: &str) -> bool {
        self.identifier_kinds.contains(&kind)
    }

    /// Literals are collapsed to `LIT`. Universal heuristic: any kind
    /// containing "literal" (covers `string_literal`, `integer_literal`,
    /// `line_string_literal`, `real_literal`, `character_literal`, …).
    pub fn is_literal_kind(&self, kind: &str) -> bool {
        kind.contains("literal")
    }

    /// Comments are excluded from canonical forms entirely (doc-only edits
    /// must not change `struct_hash`, spec §3-M2).
    pub fn is_comment_kind(&self, kind: &str) -> bool {
        kind == "comment" || kind.ends_with("_comment")
    }

    /// Kinds counted as identifier mentions (static edge lower bound).
    pub fn is_mention_kind(&self, kind: &str) -> bool {
        self.identifier_kinds.contains(&kind)
    }
}

pub static RUST: LanguageSpec = LanguageSpec {
    lang: Language::Rust,
    symbol_kinds: &[
        "function_item",
        "struct_item",
        "enum_item",
        "trait_item",
        "type_item",
        "const_item",
        "static_item",
        "macro_definition",
    ],
    container_kinds: &[
        "source_file",
        "mod_item",
        "declaration_list",
        "impl_item",
        "block",
    ],
    name_field: "name",
    identifier_kinds: &[
        "identifier",
        "type_identifier",
        "field_identifier",
        "constant_identifier",
        "scoped_identifier",
        "scoped_type_identifier",
        "self",
        "super",
        "crate",
    ],
    // Tolerant parses of signature-only snippets yield
    // `function_signature_item`; indexed symbols are `function_item`.
    query_alias: Some(("function_signature_item", "function_item")),
};

pub static JAVA: LanguageSpec = LanguageSpec {
    lang: Language::Java,
    symbol_kinds: &[
        "method_declaration",
        "constructor_declaration",
        "class_declaration",
        "interface_declaration",
        "enum_declaration",
        "record_declaration",
        "annotation_type_declaration",
    ],
    container_kinds: &[
        "program",
        "class_body",
        "interface_body",
        "enum_body",
        "record_body",
        "block",
    ],
    name_field: "name",
    identifier_kinds: &["identifier", "type_identifier", "scoped_identifier"],
    query_alias: None,
};

pub static KOTLIN: LanguageSpec = LanguageSpec {
    lang: Language::Kotlin,
    symbol_kinds: &[
        "function_declaration",
        "class_declaration",
        "object_declaration",
        "interface_declaration",
        "type_alias",
        "property_declaration",
    ],
    container_kinds: &["source_file", "class_body", "block"],
    name_field: "name",
    identifier_kinds: &["simple_identifier", "type_identifier"],
    query_alias: None,
};

pub static SWIFT: LanguageSpec = LanguageSpec {
    lang: Language::Swift,
    symbol_kinds: &[
        "function_declaration",
        "class_declaration",
        "struct_declaration",
        "enum_declaration",
        "protocol_declaration",
        "extension_declaration",
        "typealias_declaration",
        "actor_declaration",
    ],
    container_kinds: &[
        "source_file",
        "class_body",
        "protocol_body",
        "statements",
        "declarations",
    ],
    name_field: "name",
    identifier_kinds: &["simple_identifier", "type_identifier"],
    query_alias: None,
};

pub static OBJC: LanguageSpec = LanguageSpec {
    lang: Language::ObjC,
    symbol_kinds: &[
        "method_definition",
        "method_declaration",
        "class_interface",
        "class_implementation",
        "protocol_declaration",
        "function_definition",
        "property_declaration",
    ],
    container_kinds: &[
        "translation_unit",
        "class_interface",
        "class_implementation",
        "protocol_declaration",
        "implementation_definition",
        "compound_statement",
    ],
    name_field: "name",
    identifier_kinds: &["identifier", "type_identifier"],
    query_alias: None,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_rust() {
        assert_eq!(
            Language::from_path(Path::new("src/lib.rs")),
            Some(Language::Rust)
        );
    }

    #[test]
    fn detects_mobile_languages() {
        assert_eq!(
            Language::from_path(Path::new("app/Main.kt")),
            Some(Language::Kotlin)
        );
        assert_eq!(
            Language::from_path(Path::new("ios/App.swift")),
            Some(Language::Swift)
        );
        assert_eq!(
            Language::from_path(Path::new("core/Foo.java")),
            Some(Language::Java)
        );
        assert_eq!(
            Language::from_path(Path::new("ios/Legacy.m")),
            Some(Language::ObjC)
        );
    }

    #[test]
    fn unknown_extensions_are_none() {
        assert_eq!(Language::from_path(Path::new("README.md")), None);
    }

    #[test]
    fn all_five_grammars_compile_in() {
        for lang in Language::ALL {
            assert!(
                lang.ts_language().is_some(),
                "{} grammar must be compiled in",
                lang.as_str()
            );
        }
    }

    #[test]
    fn specs_cover_all_languages() {
        for lang in Language::ALL {
            let spec = lang.spec();
            assert_eq!(spec.lang, lang);
            assert!(
                !spec.symbol_kinds.is_empty(),
                "{} needs symbol kinds",
                lang.as_str()
            );
            assert!(!spec.identifier_kinds.is_empty());
        }
    }

    #[test]
    fn literal_heuristic_is_universal() {
        for lang in Language::ALL {
            let spec = lang.spec();
            assert!(spec.is_literal_kind("string_literal"));
            assert!(spec.is_literal_kind("line_string_literal"));
            assert!(spec.is_literal_kind("real_literal"));
            assert!(!spec.is_literal_kind("function_item"));
        }
    }

    #[test]
    fn comment_heuristic_is_universal() {
        for lang in Language::ALL {
            let spec = lang.spec();
            assert!(spec.is_comment_kind("comment"));
            assert!(spec.is_comment_kind("line_comment"));
            assert!(spec.is_comment_kind("multiline_comment"));
            assert!(!spec.is_comment_kind("identifier"));
        }
    }

    #[test]
    fn rust_query_alias_configured() {
        assert_eq!(
            RUST.query_alias,
            Some(("function_signature_item", "function_item"))
        );
    }
}

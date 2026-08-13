//! Language registry — the five first-class Ward languages (spec §3.0).
//!
//! Only grammars that are actually compiled in resolve to a
//! `tree_sitter::Language`; the rest report `None` and are skipped
//! fail-open during indexing. Adding a language = adding its grammar crate
//! and one match arm.

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
            // Rollout stages Phase 1+ (spec §3.0): grammars added as crates
            // land; until then files of these languages are skipped, which is
            // the documented fail-open behavior.
            Language::Kotlin | Language::Swift | Language::Java | Language::ObjC => None,
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
    fn rust_grammar_resolves_others_fail_open() {
        assert!(Language::Rust.ts_language().is_some());
        assert!(Language::Kotlin.ts_language().is_none());
    }
}

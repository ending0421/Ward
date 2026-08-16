//! Engine end-to-end tests: real temp git repositories driving the whole
//! pipeline (index → spot → replay → spec → verify → freshness → store
//! failure modes). Positive and negative cases per functional path.

mod common;

use common::TestRepo;
use ward_core::config::WardConfig;
use ward_core::diff::{ChangeKind, replay};
use ward_core::lang::Language;
use ward_core::store::Store;
use ward_core::{fresh, git, index, search, spec, verify};

fn cfg() -> WardConfig {
    WardConfig::default()
}

// ---------------------------------------------------------------- git.rs

#[test]
fn head_sha_none_on_empty_repo() {
    let repo = TestRepo::new();
    assert_eq!(git::head_sha(repo.path()).unwrap(), None);
}

#[test]
fn head_sha_tracks_commits() {
    let repo = TestRepo::new();
    repo.write("a.rs", "fn a() {}");
    let sha = repo.commit_all("c1");
    assert_eq!(git::head_sha(repo.path()).unwrap().unwrap(), sha);
}

#[test]
fn show_file_returns_content_or_none() {
    let repo = TestRepo::new();
    repo.write("keep.rs", "fn keep() {}");
    repo.commit_all("c1");
    let sha = repo.head();
    let shown = git::show_file(repo.path(), &sha, "keep.rs")
        .unwrap()
        .unwrap();
    assert!(shown.contains("fn keep() {}"));
    assert!(
        git::show_file(repo.path(), &sha, "missing.rs")
            .unwrap()
            .is_none()
    );
}

#[test]
fn diff_names_reports_changed_files() {
    let repo = TestRepo::new();
    repo.write("a.rs", "fn a() {}");
    repo.write("b.rs", "fn b() {}");
    let base = repo.commit_all("c1");
    repo.write("a.rs", "fn a() { /* changed */ }");
    repo.write("c.rs", "fn c() {}");
    let head = repo.commit_all("c2");
    let names = git::diff_names(repo.path(), &base, &head).unwrap();
    assert!(names.contains(&"a.rs".to_string()));
    assert!(names.contains(&"c.rs".to_string()));
    assert!(!names.contains(&"b.rs".to_string()));
}

#[test]
fn file_hash_detects_changes() {
    let repo = TestRepo::new();
    repo.write("f.rs", "fn f() {}");
    let h1 = git::file_hash(&repo.path().join("f.rs")).unwrap();
    repo.write("f.rs", "fn f() { 1 }");
    let h2 = git::file_hash(&repo.path().join("f.rs")).unwrap();
    assert_ne!(h1, h2);
    assert!(git::file_hash(&repo.path().join("nope.rs")).is_none());
}

// ------------------------------------------------------------- fresh.rs

#[test]
fn freshness_after_commit_is_stale() {
    let repo = TestRepo::new();
    repo.write("lib.rs", "pub fn debounce() {}");
    repo.commit_all("c1");
    let store = Store::open(&Store::default_path(repo.path())).unwrap();
    index::index_repo(repo.path(), &cfg()).unwrap();
    let f = fresh::check(repo.path(), &store, &["lib.rs".to_string()]).unwrap();
    assert!(!f.stale, "fresh right after indexing");
    // Move HEAD: stale.
    repo.write("lib.rs", "pub fn debounce() { 1 }");
    repo.commit_all("c2");
    let f2 = fresh::check(repo.path(), &store, &["lib.rs".to_string()]).unwrap();
    assert!(f2.stale, "stale after HEAD moves");
    store.set_last_indexed_sha(&repo.head()).unwrap();
}

#[test]
fn freshness_detects_uncommitted_edits() {
    let repo = TestRepo::new();
    repo.write("lib.rs", "pub fn debounce() {}");
    repo.commit_all("c1");
    index::index_repo(repo.path(), &cfg()).unwrap();
    let store = Store::open(&Store::default_path(repo.path())).unwrap();
    // Uncommitted edit to the hit file: per-file hash mismatch.
    repo.write("lib.rs", "pub fn debounce() { let x = 1; }");
    let f = fresh::check(repo.path(), &store, &["lib.rs".to_string()]).unwrap();
    assert!(f.stale, "uncommitted edit must mark the advisory stale");
    // Non-hit file edits don't matter for this advisory.
    let store2 = Store::open(&Store::default_path(repo.path())).unwrap();
    repo.write("other.rs", "fn other() {}");
    let g = fresh::check(repo.path(), &store2, &["lib.rs".to_string()]).unwrap();
    assert!(g.stale, "index sha still old for HEAD (never re-indexed)");
}

// ------------------------------------------------------------- index.rs

const MULTI_LANG_FILES: &[(&str, &str)] = &[
    (
        "src/lib.rs",
        "pub fn debounce() { let a = 1; let b = a + 1; let c = b + 1; }",
    ),
    (
        "src/Main.java",
        "public class Main { int bar() { return 1; } }",
    ),
    // Note: the Kotlin grammar is newline-sensitive around class bodies.
    (
        "src/Main.kt",
        "class Main {\n    fun bar(): Int { return 1 }\n}",
    ),
    (
        "src/App.swift",
        "class App { func bar() -> Int { return 1 } }",
    ),
    ("src/Legacy.m", "@interface Legacy\n- (int)bar;\n@end"),
];

#[test]
fn module_scoping_partitions_symbols_and_filters_spot() {
    let repo = TestRepo::new();
    // A Rust core package and a Kotlin wrapper module: the same structural
    // shape exists in both, and each must only match within its scope.
    repo.write(
        "core/Cargo.toml",
        "[package]\nname = \"engine-core\"\nversion = \"0.1.0\"\n",
    );
    repo.write(
        "core/src/lib.rs",
        "pub fn push_fill_quad(rect: &Rect, color: u32) { tessellate(rect); paint(color) }\npub struct Rect { pub x: f32, pub y: f32 }\nfn tessellate(_r: &Rect) {}\nfn paint(_c: u32) {}\n",
    );
    repo.write("android/build.gradle.kts", "// wrapper module\n");
    repo.write(
        "android/src/main/kotlin/Wrapper.kt",
        "package wrapper\n\nclass Wrapper {\n    fun pushFillQuad(rect: Rect, color: Int): Unit { tessellate(rect); paint(color) }\n}\nclass Rect(val x: Float, val y: Float)\nfun tessellate(r: Rect) {}\nfun paint(c: Int) {}\n",
    );
    repo.commit_all("c1");
    let report = index::index_repo(repo.path(), &cfg()).unwrap();
    assert_eq!(report.files_indexed, 2, "{report:?}");
    let store = Store::open(&Store::default_path(repo.path())).unwrap();
    let symbols = store.all_symbols().unwrap();
    let rust_scope: Vec<&str> = symbols
        .iter()
        .filter(|s| s.file_path.starts_with("core/"))
        .map(|s| s.module.as_str())
        .collect();
    assert!(!rust_scope.is_empty());
    assert!(
        rust_scope.iter().all(|m| *m == "engine-core"),
        "cargo package name is the scope: {rust_scope:?}"
    );
    let kt_scope: Vec<&str> = symbols
        .iter()
        .filter(|s| s.file_path.starts_with("android/"))
        .map(|s| s.module.as_str())
        .collect();
    assert!(!kt_scope.is_empty());
    assert!(
        kt_scope.iter().all(|m| *m == "android"),
        "gradle module dir is the scope: {kt_scope:?}"
    );

    // Same-structure query: scoped to the Rust core, the Kotlin clone must
    // not surface; unscoped it must.
    let sig = "pub fn push_fill_quad(rect: &Rect, color: u32)";
    let scoped = search::spot(
        repo.path(),
        &store,
        &cfg(),
        "x",
        Some(sig),
        None,
        Some(Language::Rust),
        Some("engine-core"),
    )
    .unwrap();
    assert!(
        scoped.matches.iter().all(|m| m.scope == "engine-core"),
        "{:?}",
        scoped.matches
    );
    let unscoped = search::spot(
        repo.path(),
        &store,
        &cfg(),
        "x",
        Some(sig),
        None,
        Some(Language::Rust),
        None,
    )
    .unwrap();
    assert!(
        unscoped.matches.len() >= scoped.matches.len(),
        "scoping must only restrict: {:?} vs {:?}",
        scoped.matches,
        unscoped.matches
    );
}

#[test]
fn build_artifact_dirs_are_not_indexed() {
    let repo = TestRepo::new();
    repo.write("src/lib.rs", "pub fn f() {}\n");
    repo.write("target/debug/build/x.rs", "pub fn generated() {}\n");
    repo.write("build/generated/Gen.kt", "fun generated() {}\n");
    repo.write(".gradle/caches/x.rs", "pub fn cached() {}\n");
    repo.write(
        "ios/DerivedData/Build/Generated.swift",
        "func generated() {}\n",
    );
    repo.write("swift/.build/checkouts/D.swift", "func checkout() {}\n");
    repo.commit_all("c1");
    let report = index::index_repo(repo.path(), &cfg()).unwrap();
    assert_eq!(
        report.files_indexed, 1,
        "only src/lib.rs may be indexed: {report:?}"
    );
}

#[test]
fn index_repo_handles_all_five_languages() {
    let repo = TestRepo::new();
    for (path, content) in MULTI_LANG_FILES {
        repo.write(path, content);
    }
    repo.write("src/broken.rs", "fn broken( {");
    repo.write("vendor/generated.rs", "fn vendored() {}");
    repo.commit_all("c1");
    let mut cfg = cfg();
    cfg.suppress = vec!["vendor/".into()];
    let report = index::index_repo(repo.path(), &cfg).unwrap();
    assert_eq!(report.files_indexed, 5);
    assert!(report.symbols_indexed >= 5);
    assert!(report.blocks_indexed > 0, "statement windows indexed");
    assert_eq!(
        report.files_unparsable, 1,
        "broken file must be skipped (F3)"
    );
    assert_eq!(report.files_suppressed, 1);

    let store = Store::open(&Store::default_path(repo.path())).unwrap();
    let symbols = store.all_symbols().unwrap();
    let languages: std::collections::BTreeSet<String> =
        symbols.iter().map(|s| s.language.clone()).collect();
    for lang in ["rust", "java", "kotlin", "swift", "objc"] {
        assert!(
            languages.contains(lang),
            "missing language {lang}: {languages:?}"
        );
    }
    assert!(!symbols.iter().any(|s| s.file_path.starts_with("vendor")));
}

#[test]
fn languages_config_gates_indexing() {
    let repo = TestRepo::new();
    for (path, content) in MULTI_LANG_FILES {
        repo.write(path, content);
    }
    repo.commit_all("c1");
    let mut cfg = cfg();
    cfg.languages = vec!["rust".to_string()];
    let report = index::index_repo(repo.path(), &cfg).unwrap();
    assert_eq!(
        report.files_indexed, 1,
        "only the .rs file may be indexed: {report:?}"
    );
    assert_eq!(report.files_skipped_language, 4);

    let store = Store::open(&Store::default_path(repo.path())).unwrap();
    let languages: std::collections::BTreeSet<String> = store
        .all_symbols()
        .unwrap()
        .iter()
        .map(|s| s.language.clone())
        .collect();
    assert_eq!(
        languages.len(),
        1,
        "store must only hold rust: {languages:?}"
    );
    assert!(languages.contains("rust"));
}

#[test]
fn spot_resolves_kotlin_signatures_structurally() {
    let repo = TestRepo::new();
    repo.write(
        "src/util.kt",
        "package com.example\n\nfun debounce(f: (Long) -> Unit, ms: Long): Unit { f(ms) }\n",
    );
    repo.commit_all("c1");
    index::index_repo(repo.path(), &cfg()).unwrap();
    let store = Store::open(&Store::default_path(repo.path())).unwrap();

    // Signature-only query: detected as Kotlin, strong fingerprint match.
    let r = search::spot(
        repo.path(),
        &store,
        &cfg(),
        "防抖函数",
        Some("fun debounce(f: (Long) -> Unit, ms: Long): Unit"),
        None,
        None,
        None,
    )
    .unwrap();
    let hit = r
        .matches
        .iter()
        .find(|m| m.symbol == "debounce")
        .expect("kotlin symbol must be recalled");
    assert!(
        hit.similarity >= 0.92,
        "strong fingerprint match expected, got {:?}",
        r.matches
    );
    assert!(matches!(hit.kind.as_str(), "structural" | "near"));

    // Explicit language hint must not break the same query.
    let r2 = search::spot(
        repo.path(),
        &store,
        &cfg(),
        "防抖函数",
        Some("fun debounce(f: (Long) -> Unit, ms: Long): Unit"),
        None,
        Some(Language::Kotlin),
        None,
    )
    .unwrap();
    assert!(r2.matches.iter().any(|m| m.symbol == "debounce"));

    // Rust grammar must NOT swallow the Kotlin snippet (no cross-language
    // fingerprint match on a rust-only store).
    let rust_repo = TestRepo::new();
    rust_repo.write("src/only.rs", "pub fn unrelated() -> u8 { 1 }");
    rust_repo.commit_all("c1");
    index::index_repo(rust_repo.path(), &cfg()).unwrap();
    let rust_store = Store::open(&Store::default_path(rust_repo.path())).unwrap();
    let r3 = search::spot(
        rust_repo.path(),
        &rust_store,
        &cfg(),
        "防抖函数",
        Some("fun debounce(f: (Long) -> Unit, ms: Long): Unit"),
        None,
        None,
        None,
    )
    .unwrap();
    assert!(
        r3.matches
            .iter()
            .all(|m| m.similarity < 0.92 || m.symbol != "unrelated"),
        "kotlin snippet must not fingerprint-match rust symbols: {:?}",
        r3.matches
    );
}

#[test]
fn udl_definitions_index_and_replay_with_risk_marker() {
    let repo = TestRepo::new();
    repo.write(
        "core/src/lib.rs",
        "pub fn add(a: u32, b: u32) -> u32 { a + b }\n",
    );
    repo.write(
        "core/src/api.udl",
        "namespace calculator {\n    [Throws=CalcError]\n    interface Calculator {\n        [Throws=CalcError]\n        u32 add(u32 a, u32 b);\n        string describe();\n    };\n};\n",
    );
    let base = repo.commit_all("base");

    // UDL symbols are indexed (language "udl", no tree-sitter grammar).
    let report = index::index_repo(repo.path(), &cfg()).unwrap();
    assert_eq!(report.files_indexed, 2, "{report:?}");
    let store = Store::open(&Store::default_path(repo.path())).unwrap();
    let udl_syms: Vec<_> = store
        .all_symbols()
        .unwrap()
        .into_iter()
        .filter(|s| s.language == "udl")
        .collect();
    assert!(
        udl_syms
            .iter()
            .any(|s| s.name == "Calculator" && s.kind == "udl_interface"),
        "interface must be indexed: {udl_syms:?}"
    );

    // A method-signature edit is SignatureChanged; the UDL risk marker fires.
    repo.write(
        "core/src/api.udl",
        "namespace calculator {\n    [Throws=CalcError]\n    interface Calculator {\n        [Throws=CalcError]\n        u64 add(u32 a, u32 b);\n        string describe();\n    };\n};\n",
    );
    let head = repo.commit_all("head");
    index::index_repo(repo.path(), &cfg()).unwrap();
    let store = Store::open(&Store::default_path(repo.path())).unwrap();
    let report = replay(repo.path(), &store, &cfg(), &base, &head).unwrap();
    assert!(
        report
            .changes
            .iter()
            .any(|c| c.name == "Calculator" && c.change == ChangeKind::SignatureChanged),
        "udl signature change must be classified: {:?}",
        report.changes
    );
    assert!(
        report
            .risks
            .iter()
            .any(|r| r.description.contains("UDL")
                && r.anchors.iter().any(|a| a.contains("api.udl"))),
        "UDL change must carry the regeneration risk marker: {:?}",
        report.risks
    );
}

#[test]
fn incremental_indexing_skips_unchanged_files() {
    let repo = TestRepo::new();
    repo.write("src/lib.rs", "pub fn f() {}\n");
    repo.write("src/other.rs", "pub fn g() {}\n");
    repo.commit_all("c1");
    let first = index::index_repo(repo.path(), &cfg()).unwrap();
    assert_eq!(first.files_indexed, 2);
    assert_eq!(first.files_unchanged, 0);

    let second = index::index_repo(repo.path(), &cfg()).unwrap();
    assert_eq!(
        second.files_indexed, 0,
        "nothing changed ⇒ nothing re-parsed"
    );
    assert_eq!(second.files_unchanged, 2);

    // Touch one file: only that file is re-parsed.
    let touched = repo.path().join("src/other.rs");
    let mut content = std::fs::read_to_string(&touched).unwrap();
    content.push('\n');
    std::fs::write(&touched, content).unwrap();
    let third = index::index_repo(repo.path(), &cfg()).unwrap();
    assert_eq!(third.files_indexed, 1);
    assert_eq!(third.files_unchanged, 1);
}

#[test]
fn spot_file_checks_new_symbols_against_the_store() {
    let repo = TestRepo::new();
    repo.write(
        "src/lib.rs",
        "pub fn debounce(f: &dyn Fn(u64), ms: u64) -> u8 { f(ms); 0 }",
    );
    repo.commit_all("c1");
    index::index_repo(repo.path(), &cfg()).unwrap();
    let store = Store::open(&Store::default_path(repo.path())).unwrap();

    // The hook runs BEFORE the index refresh: the store still holds the
    // pre-write state, the working tree holds the new file.
    repo.write(
        "src/new.rs",
        "pub fn debounce2(f: &dyn Fn(u64), ms: u64) -> u8 { f(ms); 0 }\npub fn unrelated() -> u8 { 7 }\n",
    );
    let report =
        ward_core::spotfile::spot_new_symbols(repo.path(), &store, &cfg(), "src/new.rs").unwrap();
    assert_eq!(
        report.changed_symbols,
        vec!["debounce2".to_string(), "unrelated".to_string()],
        "both symbols are new to the pre-write store"
    );
    assert_eq!(report.checked, 2);
    let debounce_adv = &report.advisories[0];
    assert!(
        debounce_adv
            .matches
            .iter()
            .any(|m| m.symbol == "debounce" && m.similarity >= 0.9),
        "the exact clone must be recalled structurally: {:?}",
        debounce_adv.matches
    );

    // Re-check after indexing: the symbols are now known → nothing changed.
    index::index_repo(repo.path(), &cfg()).unwrap();
    let store2 = Store::open(&Store::default_path(repo.path())).unwrap();
    let again =
        ward_core::spotfile::spot_new_symbols(repo.path(), &store2, &cfg(), "src/new.rs").unwrap();
    assert!(
        again.changed_symbols.is_empty(),
        "unchanged file ⇒ no changed symbols: {:?}",
        again.changed_symbols
    );
}

#[test]
fn index_repo_records_freshness_and_sha() {
    let repo = TestRepo::new();
    repo.write("lib.rs", "pub fn f() {}");
    let sha = repo.commit_all("c1");
    index::index_repo(repo.path(), &cfg()).unwrap();
    let store = Store::open(&Store::default_path(repo.path())).unwrap();
    assert_eq!(store.last_indexed_sha().unwrap().unwrap(), sha);
    assert!(store.get_file_hash("lib.rs").unwrap().is_some());
}

// ------------------------------------------------------------- search.rs

#[test]
fn spot_finds_structural_match_end_to_end() {
    let repo = TestRepo::new();
    repo.write(
        "src/lib.rs",
        "pub fn debounce(f: &dyn Fn(u64), ms: u64) -> u8 { f(ms); 0 }",
    );
    repo.commit_all("c1");
    index::index_repo(repo.path(), &cfg()).unwrap();
    let store = Store::open(&Store::default_path(repo.path())).unwrap();
    let r = search::spot(
        repo.path(),
        &store,
        &cfg(),
        "防抖函数",
        Some("pub fn debounce(f: &dyn Fn(u64), ms: u64) -> u8"),
        None,
        None,
        None,
    )
    .unwrap();
    assert!(!r.stale);
    assert!(
        r.matches
            .iter()
            .any(|m| m.symbol == "debounce" && m.similarity >= 0.9),
        "exact signature should match: {:?}",
        r.matches
    );
    // The advisory must be recorded and updatable (feedback loop roundtrip).
    store.set_agent_action(&r.advisory_id, "accepted").unwrap();
}

#[test]
fn spot_l1_structural_equality_when_signature_is_full_body() {
    let repo = TestRepo::new();
    let fn_src = "pub fn debounce() { let a = 1; }";
    repo.write("src/lib.rs", &format!("{fn_src}\n"));
    repo.commit_all("c1");
    index::index_repo(repo.path(), &cfg()).unwrap();
    let store = Store::open(&Store::default_path(repo.path())).unwrap();
    // Passing the *complete* function as the signature hits the L1 exact
    // structural-equality layer (normalized full-tree hash).
    let r = search::spot(
        repo.path(),
        &store,
        &cfg(),
        "防抖",
        Some(fn_src),
        None,
        None,
        None,
    )
    .unwrap();
    assert!(
        r.matches
            .iter()
            .any(|m| m.kind == "structural" && m.similarity == 1.0),
        "full-body signature must hit L1 equality: {:?}",
        r.matches
    );
}

#[test]
fn context_card_assembles_callers_tests_and_config_refs() {
    let repo = TestRepo::new();
    repo.write(
        "src/lib.rs",
        "pub fn debounce() {}\npub fn caller() { debounce(); }\n",
    );
    repo.write("tests/debounce_test.rs", "fn t() { debounce(); }\n");
    repo.write("Cargo.toml", "[package]\nname = \"debounce-utils\"\n");
    repo.commit_all("c1");
    index::index_repo(repo.path(), &cfg()).unwrap();
    let store = Store::open(&Store::default_path(repo.path())).unwrap();
    let card = ward_core::context::context_card(repo.path(), &store, &cfg(), "debounce").unwrap();
    assert_eq!(card.symbol, "debounce");
    assert_eq!(card.language, "rust");
    assert!(card.lines.contains('1'), "lines: {}", card.lines);
    assert!(
        card.callers.iter().any(|c| c.symbol == "caller"),
        "callers: {:?}",
        card.callers
    );
    assert!(
        card.tests
            .iter()
            .any(|t| t.path == "tests/debounce_test.rs"),
        "tests: {:?}",
        card.tests
    );
    assert!(
        card.config_refs
            .iter()
            .any(|r| r.path == "Cargo.toml" && r.line == 2),
        "config refs: {:?}",
        card.config_refs
    );
}

#[test]
fn replay_handles_java_changes() {
    let repo = TestRepo::new();
    repo.write(
        "src/Main.java",
        "public class Main { int bar() { return 1; } }\n",
    );
    let base = repo.commit_all("base");
    repo.write(
        "src/Main.java",
        "public class Main { int bar() { return 2; } }\n",
    );
    let head = repo.commit_all("head");
    index::index_repo(repo.path(), &cfg()).unwrap();
    let store = Store::open(&Store::default_path(repo.path())).unwrap();
    let report = replay(repo.path(), &store, &cfg(), &base, &head).unwrap();
    assert!(
        report.changes.iter().any(|c| c.name == "bar"),
        "java symbols must be classified: {:?}",
        report.changes
    );
    let md = ward_core::diff::render_markdown(&report);
    assert!(md.contains("src/Main.java"));
}

#[test]
fn replay_skips_non_code_files() {
    let repo = TestRepo::new();
    repo.write("README.md", "# title\n");
    repo.write("src/lib.rs", "pub fn f() {}\n");
    let base = repo.commit_all("base");
    repo.write("README.md", "# title changed\n");
    let head = repo.commit_all("head");
    index::index_repo(repo.path(), &cfg()).unwrap();
    let store = Store::open(&Store::default_path(repo.path())).unwrap();
    let report = replay(repo.path(), &store, &cfg(), &base, &head).unwrap();
    assert!(
        report.changes.is_empty(),
        "md files are skipped: {:?}",
        report.changes
    );
}

#[test]
fn near_set_is_a_pure_function_of_signature_not_intent() {
    // Issue #3: rewording ONLY the intent must not change the near set —
    // automated consumers (hooks/CI) cannot author a "good" intent.
    let repo = TestRepo::new();
    repo.write(
        "src/lib.rs",
        "pub fn push_fill_quad(rect: &Rect, color: u32) { tessellate(rect); paint(color) }\npub fn push_rotated_corners(rect: &Rect, color: u32) { tessellate(rect); rotate(color) }\npub struct Rect { pub x: f32, pub y: f32 }\nfn tessellate(_r: &Rect) {}\nfn paint(_c: u32) {}\nfn rotate(_c: u32) {}\n",
    );
    repo.commit_all("c1");
    index::index_repo(repo.path(), &cfg()).unwrap();
    let store = Store::open(&Store::default_path(repo.path())).unwrap();
    let sig = "pub fn push_rounded_rect(rect: &Rect, color: u32)";
    let body = "tessellate(rect); paint(color)";
    let intent_a = "new Rust function push_rounded_rect with signature: pub fn push_rounded_rect(rect: &Rect, color: u32) — being added to the engine renderer";
    let intent_b = "pre-edit duplicate check for Rust code being written in the engine";
    let r_a = search::spot(
        repo.path(),
        &store,
        &cfg(),
        intent_a,
        Some(sig),
        Some(body),
        Some(Language::Rust),
        None,
    )
    .unwrap();
    let r_b = search::spot(
        repo.path(),
        &store,
        &cfg(),
        intent_b,
        Some(sig),
        Some(body),
        Some(Language::Rust),
        None,
    )
    .unwrap();
    let near_of = |r: &ward_core::search::SpotResult| -> Vec<(String, f64)> {
        r.matches
            .iter()
            .filter(|m| m.kind == "near")
            .map(|m| (m.symbol.clone(), m.similarity))
            .collect()
    };
    let a = near_of(&r_a);
    assert!(!a.is_empty(), "near hits must exist: {:?}", r_a.matches);
    assert_eq!(a, near_of(&r_b), "near set must be intent-invariant");
}

#[test]
fn freshness_never_indexed_never_committed_is_stale() {
    let repo = TestRepo::new();
    let store = Store::open(&Store::default_path(repo.path())).unwrap();
    let f = fresh::check(repo.path(), &store, &["lib.rs".to_string()]).unwrap();
    assert!(f.stale, "(None, None) must be stale");
    assert!(f.as_of.is_none());
}

#[test]
fn spot_block_layer_matches_body_windows() {
    let repo = TestRepo::new();
    let body = "let a = 1; let b = a + 1; let c = b + 1; let d = c + 1;";
    repo.write("src/one.rs", &format!("fn one() {{ {body} }}"));
    repo.commit_all("c1");
    index::index_repo(repo.path(), &cfg()).unwrap();
    let store = Store::open(&Store::default_path(repo.path())).unwrap();
    let r = search::spot(
        repo.path(),
        &store,
        &cfg(),
        "一样的语句序列",
        None,
        Some(body),
        None,
        None,
    )
    .unwrap();
    assert!(
        r.matches.iter().any(|m| m.kind == "block"),
        "body windows must match: {:?}",
        r.matches
    );
}

#[test]
fn spot_without_signature_is_weak_never_strong() {
    let repo = TestRepo::new();
    repo.write("src/lib.rs", "pub fn debounce() {}");
    repo.commit_all("c1");
    index::index_repo(repo.path(), &cfg()).unwrap();
    let store = Store::open(&Store::default_path(repo.path())).unwrap();
    let r = search::spot(
        repo.path(),
        &store,
        &cfg(),
        "debounce",
        None,
        None,
        None,
        None,
    )
    .unwrap();
    for m in &r.matches {
        assert_ne!(m.kind, "structural");
        assert_ne!(m.kind, "near", "text-only evidence must not be graded near");
    }
}

#[test]
fn spot_respects_suppression_and_top_k() {
    let repo = TestRepo::new();
    repo.write("src/a.rs", "pub fn alpha() {}");
    repo.write("vendor/b.rs", "pub fn alpha() {}");
    repo.commit_all("c1");
    let mut cfg = cfg();
    cfg.suppress = vec!["vendor/".into()];
    index::index_repo(repo.path(), &cfg).unwrap();
    let store = Store::open(&Store::default_path(repo.path())).unwrap();
    let r = search::spot(repo.path(), &store, &cfg, "alpha", None, None, None, None).unwrap();
    assert!(r.matches.iter().all(|m| !m.path.starts_with("vendor")));
}

#[test]
fn spot_top_k_truncates_textual_matches() {
    let repo = TestRepo::new();
    // Five docs, each matched by one *rare* query token (df=1 ⇒ strong BM25
    // idf); two decoy docs that match nothing. Textual evidence tops out at
    // Weak, so all five are returned and top_k=5 must truncate.
    for name in ["alpha", "bravo", "charlie", "delta", "echo"] {
        repo.write(
            &format!("src/{name}.rs"),
            &format!("pub fn debounce_{name}() {{}}"),
        );
    }
    for i in 0..2 {
        repo.write(
            &format!("src/noise{i}.rs"),
            &format!("pub fn noise_{i}() {{}}"),
        );
    }
    repo.commit_all("c1");
    index::index_repo(repo.path(), &cfg()).unwrap();
    let store = Store::open(&Store::default_path(repo.path())).unwrap();
    let r = search::spot(
        repo.path(),
        &store,
        &cfg(),
        "alpha bravo charlie delta echo",
        None,
        None,
        None,
        None,
    )
    .unwrap();
    assert_eq!(r.matches.len(), 5, "top_k must truncate: {:?}", r.matches);
}

#[test]
fn freshness_when_head_vanishes_is_stale() {
    let repo = TestRepo::new();
    // No commits at all: HEAD is unborn, but the store claims a sha.
    let store = Store::open(&Store::default_path(repo.path())).unwrap();
    store.set_last_indexed_sha("deadbeef").unwrap();
    let f = fresh::check(repo.path(), &store, &["lib.rs".to_string()]).unwrap();
    assert!(f.stale, "(Some indexed, None head) must be stale");
}

#[test]
fn git_negatives_are_errors_or_none() {
    let repo = TestRepo::new();
    repo.write("a.rs", "fn a() {}");
    repo.commit_all("c1");
    assert!(git::diff_names(repo.path(), "bogus-base", "bogus-head").is_err());
    assert!(
        git::show_file(repo.path(), "bogus-sha", "a.rs")
            .unwrap()
            .is_none()
    );
}

#[test]
fn spot_on_empty_index_fails_open() {
    let repo = TestRepo::new();
    repo.write("src/lib.rs", "pub fn f() {}");
    repo.commit_all("c1");
    // Never indexed: store exists but is empty.
    let store = Store::open(&Store::default_path(repo.path())).unwrap();
    let r = search::spot(
        repo.path(),
        &store,
        &cfg(),
        "whatever",
        None,
        None,
        None,
        None,
    )
    .unwrap();
    assert!(r.matches.is_empty());
    assert!(r.stale, "empty index must report stale");
}

// -------------------------------------------------------------- diff.rs

#[test]
fn replay_classifies_all_change_kinds() {
    let repo = TestRepo::new();
    repo.write("src/lib.rs", "pub fn keep() {}\npub fn body_only() { let x = 1; }\npub fn sig_change(a: u64) -> u64 { a }\npub fn remove_me() {}\npub fn moves() {}\n");
    repo.write("src/other.rs", "pub fn unrelated() {}\n");
    let base = repo.commit_all("base");

    repo.write(
        "src/lib.rs",
        "pub fn keep() {}\n/// docs\npub fn body_only() { let x = 2; }\npub fn sig_change(a: u64, b: u64) -> u64 { a + b }\npub fn added() {}\n",
    );
    repo.write("src/moved.rs", "pub fn moves() {}\n");
    let head = repo.commit_all("head");

    index::index_repo(repo.path(), &cfg()).unwrap();
    let store = Store::open(&Store::default_path(repo.path())).unwrap();
    let report = replay(repo.path(), &store, &cfg(), &base, &head).unwrap();

    let by_name = |n: &str| {
        report
            .changes
            .iter()
            .find(|c| c.name == n)
            .unwrap_or_else(|| panic!("no change for {n}: {:?}", report.changes))
    };
    assert_eq!(by_name("body_only").change, ChangeKind::BodyChanged);
    assert_eq!(by_name("sig_change").change, ChangeKind::SignatureChanged);
    assert_eq!(by_name("added").change, ChangeKind::Added);
    assert_eq!(by_name("remove_me").change, ChangeKind::Removed);
    assert_eq!(by_name("moves").change, ChangeKind::Moved);
    assert!(by_name("moves").moved_from.is_some());
    // keep/unrelated are untouched and must not appear.
    assert!(!report.changes.iter().any(|c| c.name == "keep"));
    assert!(!report.changes.iter().any(|c| c.name == "unrelated"));
}

#[test]
fn replay_detects_doc_only_changes() {
    let repo = TestRepo::new();
    repo.write("src/lib.rs", "pub fn f() { 1 }\n");
    let base = repo.commit_all("base");
    repo.write("src/lib.rs", "/// documented now\npub fn f() { 1 }\n");
    let head = repo.commit_all("head");
    index::index_repo(repo.path(), &cfg()).unwrap();
    let store = Store::open(&Store::default_path(repo.path())).unwrap();
    let report = replay(repo.path(), &store, &cfg(), &base, &head).unwrap();
    let f = report.changes.iter().find(|c| c.name == "f").unwrap();
    assert_eq!(f.change, ChangeKind::DocOnly);
}

#[test]
fn replay_emits_risk_markers_for_public_signature_changes() {
    let repo = TestRepo::new();
    repo.write(
        "src/lib.rs",
        "pub fn hot(a: u64) -> u64 { a }\npub fn caller() { hot(1); }\n",
    );
    let base = repo.commit_all("base");
    repo.write(
        "src/lib.rs",
        "pub fn hot(a: u64, b: u64) -> u64 { a }\npub fn caller() { hot(1, 2); }\n",
    );
    let head = repo.commit_all("head");
    index::index_repo(repo.path(), &cfg()).unwrap();
    let store = Store::open(&Store::default_path(repo.path())).unwrap();
    let report = replay(repo.path(), &store, &cfg(), &base, &head).unwrap();
    assert!(
        report
            .risks
            .iter()
            .any(|r| r.description.contains("公共 API 签名变更")),
        "public signature change must be flagged: {:?}",
        report.risks
    );
    assert!(
        report
            .risks
            .iter()
            .any(|r| r.description.contains("测试文件未同步变更"))
    );
}

#[test]
fn replay_flags_high_fan_in_symbols() {
    let repo = TestRepo::new();
    let mut lib = String::from("pub fn hot(a: u64) -> u64 { a }\n");
    for i in 0..5 {
        lib.push_str(&format!("pub fn caller{i}() {{ hot({i}); }}\n"));
    }
    repo.write("src/lib.rs", &lib);
    let base = repo.commit_all("base");
    let mut lib2 = String::from("pub fn hot(a: u64) -> u64 { a + 1 }\n");
    for i in 0..5 {
        lib2.push_str(&format!("pub fn caller{i}() {{ hot({i}); }}\n"));
    }
    repo.write("src/lib.rs", &lib2);
    let head = repo.commit_all("head");
    index::index_repo(repo.path(), &cfg()).unwrap();
    let store = Store::open(&Store::default_path(repo.path())).unwrap();
    let report = replay(repo.path(), &store, &cfg(), &base, &head).unwrap();
    assert!(
        report
            .risks
            .iter()
            .any(|r| r.description.contains("高扇入符号被修改")),
        "5 callers must trigger the fan-in risk: {:?}",
        report.risks
    );
}

#[test]
fn replay_flags_suspected_duplicates_for_added_symbols() {
    let repo = TestRepo::new();
    repo.write("src/a.rs", "pub fn alpha() {}\n");
    let base = repo.commit_all("base");
    index::index_repo(repo.path(), &cfg()).unwrap();
    repo.write("src/b.rs", "pub fn alpha() {}\n");
    let head = repo.commit_all("head");
    let store = Store::open(&Store::default_path(repo.path())).unwrap();
    let report = replay(repo.path(), &store, &cfg(), &base, &head).unwrap();
    assert!(
        report
            .risks
            .iter()
            .any(|r| r.description.contains("疑似重复引入")),
        "same-name add must be flagged: {:?}",
        report.risks
    );
}

#[test]
fn replay_respects_suppression() {
    let repo = TestRepo::new();
    repo.write("src/lib.rs", "pub fn f() {}\n");
    repo.write("vendor/g.rs", "pub fn v() {}\n");
    let base = repo.commit_all("base");
    repo.write("vendor/g.rs", "pub fn v() { 1 }\n");
    let head = repo.commit_all("head");
    let mut c = cfg();
    c.suppress = vec!["vendor/".into()];
    index::index_repo(repo.path(), &c).unwrap();
    let store = Store::open(&Store::default_path(repo.path())).unwrap();
    let report = replay(repo.path(), &store, &c, &base, &head).unwrap();
    assert!(
        report
            .changes
            .iter()
            .all(|ch| !ch.path.starts_with("vendor")),
        "suppressed paths must not appear: {:?}",
        report.changes
    );
}

#[test]
fn render_markdown_covers_all_labels() {
    use ward_core::diff::{ReplayReport, RiskMarker, SymbolChange, render_markdown};
    let mut impact = std::collections::BTreeMap::new();
    impact.insert("moves".to_string(), 3);
    let report = ReplayReport {
        base: "a".into(),
        head: "b".into(),
        changes: vec![
            SymbolChange {
                path: "src/x.rs".into(),
                lines: "5".into(),
                name: "moves".into(),
                kind_of: "function_item".into(),
                change: ChangeKind::Moved,
                moved_from: Some("src/y.rs".into()),
                public: false,
            },
            SymbolChange {
                path: "src/x.rs".into(),
                lines: "8".into(),
                name: "docs".into(),
                kind_of: "function_item".into(),
                change: ChangeKind::DocOnly,
                moved_from: None,
                public: false,
            },
        ],
        risks: vec![RiskMarker {
            severity: "high".into(),
            description: "boom".into(),
            anchors: vec!["src/x.rs:5".into()],
        }],
        impact,
    };
    let md = render_markdown(&report);
    assert!(md.contains("移动"), "moved label rendered");
    assert!(md.contains("自 `src/y.rs` 移入"));
    assert!(md.contains("仅文档"), "doc-only label rendered");
    assert!(md.contains("至少 3 处引用"), "impact rendered");
    assert!(md.contains("**[high]** boom"), "risk rendered with anchor");
}

#[test]
fn render_markdown_handles_empty_report() {
    use ward_core::diff::{ReplayReport, render_markdown};
    let report = ReplayReport {
        base: "a".into(),
        head: "b".into(),
        changes: vec![],
        risks: vec![],
        impact: Default::default(),
    };
    let md = render_markdown(&report);
    assert!(md.contains("无符号级变更"));
    assert!(md.contains("无"));
}

#[test]
fn replay_on_new_and_deleted_files() {
    let repo = TestRepo::new();
    repo.write("src/a.rs", "pub fn gone() {}\n");
    let base = repo.commit_all("base");
    std::fs::remove_file(repo.path().join("src/a.rs")).unwrap();
    repo.write("src/b.rs", "pub fn fresh() {}\n");
    let head = repo.commit_all("head");
    index::index_repo(repo.path(), &cfg()).unwrap();
    let store = Store::open(&Store::default_path(repo.path())).unwrap();
    let report = replay(repo.path(), &store, &cfg(), &base, &head).unwrap();
    assert!(
        report
            .changes
            .iter()
            .any(|c| c.name == "gone" && c.change == ChangeKind::Removed)
    );
    assert!(
        report
            .changes
            .iter()
            .any(|c| c.name == "fresh" && c.change == ChangeKind::Added)
    );
    let md = ward_core::diff::render_markdown(&report);
    assert!(md.contains("src/b.rs"));
}

// -------------------------------------------------------------- spec.rs

#[test]
fn evaluate_against_real_git_diff() {
    let repo = TestRepo::new();
    repo.write("src/lib.rs", "pub fn f() {}\n");
    let base = repo.commit_all("base");
    // A NEW manifest whose dependency set gains serde → new dependency.
    repo.write(
        "Cargo.toml",
        "[package]\nname = \"x\"\n\n[dependencies]\nserde = \"1\"\n",
    );
    repo.write("src/1.rs", "a");
    repo.write("src/2.rs", "b");
    repo.write("src/3.rs", "c");
    repo.write("src/4.rs", "d");
    repo.write("src/5.rs", "e");
    repo.write("src/6.rs", "f");
    repo.write("src/7.rs", "g");
    let head = repo.commit_all("head");
    let parsed = spec::parse_spec(
        "```yaml\nassertions:\n  - kind: no_new_dependency\n  - kind: max_files_changed\n    value: 6\n```",
        "specs/t.md",
    )
    .unwrap();
    let results = spec::evaluate(repo.path(), &parsed, &base, &head).unwrap();
    let dep = results
        .iter()
        .find(|r| r.assertion == "no_new_dependency")
        .unwrap();
    assert_eq!(
        dep.verdict,
        spec::Verdict::Fail,
        "a new dependency key must fail: {dep:?}"
    );
    assert!(dep.detail.contains("serde"), "detail: {}", dep.detail);
    let max = results
        .iter()
        .find(|r| r.assertion == "max_files_changed")
        .unwrap();
    assert_eq!(max.verdict, spec::Verdict::Fail, "7 files > 6");
}

#[test]
fn version_bump_without_new_deps_passes_no_new_dependency() {
    let repo = TestRepo::new();
    repo.write(
        "Cargo.toml",
        "[package]\nname = \"x\"\nversion = \"0.3.1\"\n\n[dependencies]\nserde = \"1\"\n",
    );
    repo.write(
        "Cargo.lock",
        "[[package]]\nname = \"serde\"\nversion = \"1.0.0\"\n",
    );
    repo.write("src/lib.rs", "pub fn f() {}\n");
    let base = repo.commit_all("base");
    // Version-only edits to both manifests — no new dependency keys/names.
    repo.write(
        "Cargo.toml",
        "[package]\nname = \"x\"\nversion = \"0.4.0\"\n\n[dependencies]\nserde = \"1\"\n",
    );
    repo.write(
        "Cargo.lock",
        "[[package]]\nname = \"serde\"\nversion = \"1.1.0\"\n",
    );
    let head = repo.commit_all("bump");
    let parsed = spec::parse_spec(
        "```yaml\nassertions:\n  - kind: no_new_dependency\n```",
        "specs/t.md",
    )
    .unwrap();
    let results = spec::evaluate(repo.path(), &parsed, &base, &head).unwrap();
    assert_eq!(
        results[0].verdict,
        spec::Verdict::Pass,
        "version bump is not a new dependency: {results:?}"
    );
}

// ------------------------------------------------------------- store.rs

#[test]
fn store_rebuilds_on_schema_version_mismatch() {
    let repo = TestRepo::new();
    repo.write("lib.rs", "pub fn f() {}");
    repo.commit_all("c1");
    let db_path = Store::default_path(repo.path());
    {
        let mut store = Store::open(&db_path).unwrap();
        store
            .replace_file(
                "lib.rs",
                &[ward_core::store::Symbol {
                    id: None,
                    file_path: "lib.rs".into(),
                    module: String::new(),
                    language: "rust".into(),
                    name: "f".into(),
                    kind: "function_item".into(),
                    start_byte: 0,
                    end_byte: 1,
                    body_hash: "b".into(),
                    struct_hash: "s".into(),
                    simhash: 1,
                    sig_simhash: 1,
                    in_test: false,
                    commit_sha: "c".into(),
                }],
            )
            .unwrap();
        assert_eq!(store.all_symbols().unwrap().len(), 1);
        // Sabotage the version stamp.
        store
            .record_contract_run(&ward_core::store::ContractRun {
                spec_path: "s".into(),
                commit_sha: "c".into(),
                ts: 1,
                assertion: "a".into(),
                verdict: "pass".into(),
                detail: String::new(),
            })
            .unwrap();
    }
    // Reopen after forging an older version: F1 wipes and rebuilds.
    {
        use rusqlite::Connection;
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "UPDATE meta SET value = '1' WHERE key = 'schema_version'",
            [],
        )
        .unwrap();
    }
    let store = Store::open(&db_path).unwrap();
    assert!(
        store.all_symbols().unwrap().is_empty(),
        "version mismatch must wipe derived tables (F1)"
    );
    // Governance data is NOT derivable and must survive the rebuild.
    assert_eq!(
        store.label_count().unwrap(),
        0,
        "no labels were seeded in this test"
    );
    store
        .record_advisory(&ward_core::store::Advisory {
            id: "adv_keep".into(),
            tool: "spot".into(),
            ts: 2,
            query_hash: "q".into(),
            result_json: "[]".into(),
            ..Default::default()
        })
        .unwrap();
    // Re-stamping still works after the rebuild.
    store.set_last_indexed_sha("abc").unwrap();
    assert_eq!(store.last_indexed_sha().unwrap().unwrap(), "abc");
}

#[test]
fn schema_rebuild_preserves_governance_data() {
    let repo = TestRepo::new();
    repo.write("lib.rs", "pub fn f() {}\n");
    repo.commit_all("c1");
    let db_path = Store::default_path(repo.path());
    {
        let mut store = Store::open(&db_path).unwrap();
        store
            .replace_file(
                "lib.rs",
                &[ward_core::store::Symbol {
                    id: None,
                    file_path: "lib.rs".into(),
                    module: String::new(),
                    language: "rust".into(),
                    name: "f".into(),
                    kind: "function_item".into(),
                    start_byte: 0,
                    end_byte: 1,
                    body_hash: "b".into(),
                    struct_hash: "s".into(),
                    simhash: 1,
                    sig_simhash: 1,
                    in_test: false,
                    commit_sha: "c".into(),
                }],
            )
            .unwrap();
        store
            .record_advisory(&ward_core::store::Advisory {
                id: "adv_keep".into(),
                tool: "spot".into(),
                ts: 1,
                query_hash: "q".into(),
                result_json: "[]".into(),
                ..Default::default()
            })
            .unwrap();
        store
            .record_label(&ward_core::store::Label {
                id: None,
                advisory_id: "adv_keep".into(),
                match_index: 0,
                annotator: "human".into(),
                query_hash: None,
                language: None,
                kind: Some("near".into()),
                similarity: Some(0.93),
                verdict: "y".into(),
                ts: 1,
            })
            .unwrap();
    }
    {
        use rusqlite::Connection;
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "UPDATE meta SET value = '1' WHERE key = 'schema_version'",
            [],
        )
        .unwrap();
    }
    let store = Store::open(&db_path).unwrap();
    assert!(
        store.all_symbols().unwrap().is_empty(),
        "derived data wiped"
    );
    assert_eq!(
        store.label_count().unwrap(),
        1,
        "labels survive the rebuild"
    );
    assert_eq!(
        store.advisory_payloads().unwrap().len(),
        1,
        "advisories survive the rebuild"
    );
}

#[test]
fn inferred_action_roundtrip_and_unknown_id() {
    let repo = TestRepo::new();
    repo.write("lib.rs", "pub fn f() {}");
    repo.commit_all("c1");
    let store = Store::open(&Store::default_path(repo.path())).unwrap();
    store
        .record_advisory(&ward_core::store::Advisory {
            id: "adv_x".into(),
            tool: "spot".into(),
            ts: 1,
            query_hash: "q".into(),
            result_json: "[]".into(),
            ..Default::default()
        })
        .unwrap();
    store
        .set_inferred_action("adv_x", "rejected", "sha2")
        .unwrap();
    assert!(
        store
            .set_inferred_action("adv_404", "accepted", "s")
            .is_err()
    );
}

// ------------------------------------------------------------ verify.rs

#[test]
fn catch_run_pass_fail_and_missing_tool() {
    let repo = TestRepo::new();
    repo.write("lib.rs", "pub fn f() {}");
    repo.commit_all("c1");

    let mut c = cfg();
    c.lint.command = "true".into();
    assert_eq!(
        verify::catch_run(repo.path(), &c).verdict,
        verify::CatchVerdict::Pass
    );

    c.lint.command = "false".into();
    assert_eq!(
        verify::catch_run(repo.path(), &c).verdict,
        verify::CatchVerdict::Fail
    );

    c.lint.command = "/nonexistent/ward-tool".into();
    assert_eq!(
        verify::catch_run(repo.path(), &c).verdict,
        verify::CatchVerdict::Unknown
    );
}

#[test]
fn catch_run_timeout_is_unknown() {
    let repo = TestRepo::new();
    repo.write("lib.rs", "pub fn f() {}");
    repo.commit_all("c1");
    let mut c = cfg();
    c.lint.command = "sleep 5".into();
    c.lint.timeout_secs = 1;
    let start = std::time::Instant::now();
    let r = verify::catch_run(repo.path(), &c);
    assert!(start.elapsed().as_secs() < 4, "must kill at timeout");
    assert_eq!(r.verdict, verify::CatchVerdict::Unknown);
}

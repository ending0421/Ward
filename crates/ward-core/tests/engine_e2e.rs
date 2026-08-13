//! Engine end-to-end tests: real temp git repositories driving the whole
//! pipeline (index → spot → replay → spec → verify → freshness → store
//! failure modes). Positive and negative cases per functional path.

mod common;

use common::TestRepo;
use ward_core::config::WardConfig;
use ward_core::diff::{ChangeKind, replay};
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
    let r = search::spot(repo.path(), &store, &cfg(), "防抖", Some(fn_src), None).unwrap();
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
    let r = search::spot(repo.path(), &store, &cfg(), "debounce", None, None).unwrap();
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
    let r = search::spot(repo.path(), &store, &cfg, "alpha", None, None).unwrap();
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
    let r = search::spot(repo.path(), &store, &cfg(), "whatever", None, None).unwrap();
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
    repo.write("Cargo.toml", "[package]\nname = \"x\"\n");
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
    assert_eq!(dep.verdict, spec::Verdict::Fail, "Cargo.toml changed");
    let max = results
        .iter()
        .find(|r| r.assertion == "max_files_changed")
        .unwrap();
    assert_eq!(max.verdict, spec::Verdict::Fail, "7 files > 6");
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
                    language: "rust".into(),
                    name: "f".into(),
                    kind: "function_item".into(),
                    start_byte: 0,
                    end_byte: 1,
                    body_hash: "b".into(),
                    struct_hash: "s".into(),
                    simhash: 1,
                    sig_simhash: 1,
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
        "version mismatch must wipe the store (F1)"
    );
    // Re-stamping still works after the rebuild.
    store.set_last_indexed_sha("abc").unwrap();
    assert_eq!(store.last_indexed_sha().unwrap().unwrap(), "abc");
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

//! FFI export-face adjudication (0.5-3): the Rust core's `extern "C"`
//! surface is the contract the generated bindings consume — cargo-semver-
//! checks, binary-compatibility-validator and swift-api-digester each see
//! only ONE side of it. This thin self-built layer (P4 exception: no
//! reusable component exists) compares the EXPECTED face (a checked-in
//! declaration header — shape-agnostic: configured, auto-detected, or
//! degraded to the base artifact) against the ACTUAL face (`nm` on the
//! built library):
//!
//! * `removed` — in the manifest but not the artifact: **breaking**;
//! * `added` — in the artifact but not the manifest: warning (new API, or
//!   manifest drift that must be fixed);
//! * unresolvable manifest/artifact → `unknown`, never a fake pass (F13).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::compat::CompatVerdict;
use crate::config::FfiConfig;

/// One FFI adjudication.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FfiReport {
    pub verdict: CompatVerdict,
    /// Manifest-derived expected export names.
    pub expected: Vec<String>,
    /// Artifact-derived actual export names.
    pub actual: Vec<String>,
    pub removed: Vec<String>,
    pub added: Vec<String>,
    pub manifest_path: Option<String>,
    pub artifact: Option<String>,
    pub detail: String,
}

/// Parse C declaration syntax for function names: `type name(args)`.
/// Comments and preprocessor lines are skipped; typedefs/functions without
/// parentheses are ignored.
pub fn parse_c_declarations(source: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in source.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') || t.starts_with("//") || t.starts_with('*') {
            continue;
        }
        let t = t.split("//").next().unwrap_or("").trim();
        if t.starts_with("static ") || t.starts_with("extern \"C\"") {
            continue;
        }
        if let Some(paren) = t.find('(') {
            let head = &t[..paren];
            let mut name = head
                .split(|c: char| !(c.is_alphanumeric() || c == '_'))
                .rfind(|s| !s.is_empty())
                .unwrap_or("")
                .to_string();
            if !name.is_empty() && !head.trim().starts_with("typedef") {
                // Skip `if/for/while/switch/return` control-flow lookalikes.
                if matches!(
                    name.as_str(),
                    "if" | "for" | "while" | "switch" | "return" | "sizeof"
                ) {
                    continue;
                }
                if name.starts_with('_') {
                    name = name.trim_start_matches('_').to_string();
                }
                out.push(name);
            }
        }
    }
    out
}

/// `nm -g --defined-only` output → exported symbol names (leading
/// underscores and version suffixes stripped, duplicates removed).
pub fn parse_nm_output(raw: &str) -> Vec<String> {
    let mut out: Vec<String> = raw
        .lines()
        .filter_map(|line| {
            // `nm` formats: "addr T name" (BSD) / "addr T name@@VER" (GNU).
            let mut it = line.split_whitespace();
            let _addr = it.next()?;
            let _kind = it.next()?;
            let mut name = it.next()?.to_string();
            if let Some(ver) = name.find("@@") {
                name.truncate(ver);
            }
            while name.starts_with('_') {
                name.remove(0);
            }
            Some(name)
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Locate the manifest: explicit config wins, then a declaration header in
/// the repo root / `ffi/` / `include/`.
fn resolve_manifest(repo: &Path, config: &FfiConfig) -> Option<(PathBuf, Vec<String>)> {
    let candidates: Vec<PathBuf> = match &config.manifest {
        Some(m) => vec![repo.join(m)],
        None => {
            let mut dirs = vec![repo.to_path_buf()];
            for sub in ["ffi", "include", "exports"] {
                let d = repo.join(sub);
                if d.is_dir() {
                    dirs.push(d);
                }
            }
            let mut files = Vec::new();
            for d in dirs {
                if let Ok(rd) = std::fs::read_dir(d) {
                    for e in rd.flatten() {
                        let p = e.path();
                        if p.extension().is_some_and(|x| x == "h") {
                            files.push(p);
                        }
                    }
                }
            }
            files
        }
    };
    for path in candidates {
        if let Ok(source) = std::fs::read_to_string(&path) {
            let names = parse_c_declarations(&source);
            if !names.is_empty() {
                return Some((path, names));
            }
        }
    }
    None
}

/// Find the built artifact matching the config glob (first match by walk
/// order — deterministic enough for a single-target CI job).
fn resolve_artifact(repo: &Path, config: &FfiConfig) -> Option<PathBuf> {
    if config.artifact_glob.trim().is_empty() {
        return None;
    }
    let pattern = config.artifact_glob.as_str();
    let mut stack = vec![repo.to_path_buf()];
    let mut hits = Vec::new();
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name();
            let name = name.to_string_lossy();
            if p.is_dir() {
                if name.starts_with('.') {
                    continue;
                }
                stack.push(p);
                continue;
            }
            let rel = p
                .strip_prefix(repo)
                .map(|r| r.to_string_lossy().into_owned())
                .unwrap_or_else(|_| name.to_string());
            if glob_like(pattern, &rel) {
                hits.push(p);
            }
        }
    }
    hits.sort_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
    hits.pop()
}

/// A tiny `*`/`?` glob (path-relative, `*` crosses `/`).
fn glob_like(pattern: &str, text: &str) -> bool {
    fn rec(p: &[char], t: &[char]) -> bool {
        match p.first() {
            None => t.is_empty(),
            Some('*') => (0..=t.len()).any(|i| rec(&p[1..], &t[i..])),
            Some(c) => t
                .first()
                .is_some_and(|tc| (*c == '?' || c == tc) && rec(&p[1..], &t[1..])),
        }
    }
    rec(
        &pattern.chars().collect::<Vec<_>>(),
        &text.chars().collect::<Vec<_>>(),
    )
}

/// The real runner, with the nm binary injectable (tests pass a shim;
/// parallel-test safe, no environment mutation).
pub fn ffi_check_with(nm_bin: &str, repo: &Path, config: &FfiConfig) -> FfiReport {
    let Some((manifest_path, expected)) = resolve_manifest(repo, config) else {
        return FfiReport {
            verdict: CompatVerdict::Unknown,
            expected: vec![],
            actual: vec![],
            removed: vec![],
            added: vec![],
            manifest_path: None,
            artifact: None,
            detail: "FFI 导出面清单缺失：请在 .ward/config.toml 配置 ffi.manifest（或在 ffi//include/ 放置声明头文件）".into(),
        };
    };
    let Some(artifact) = resolve_artifact(repo, config) else {
        return FfiReport {
            verdict: CompatVerdict::Unknown,
            expected: expected.clone(),
            actual: vec![],
            removed: vec![],
            added: vec![],
            manifest_path: Some(manifest_path.display().to_string()),
            artifact: None,
            detail: format!(
                "未找到构建产物（ffi.artifact_glob = {}）；先构建再裁决（F13：无产物即无裁决）",
                config.artifact_glob
            ),
        };
    };
    let out = std::process::Command::new(nm_bin)
        .args(["-g", "--defined-only"])
        .arg(&artifact)
        .output();
    let Ok(out) = out else {
        return FfiReport {
            verdict: CompatVerdict::Unknown,
            expected: expected.clone(),
            actual: vec![],
            removed: vec![],
            added: vec![],
            manifest_path: Some(manifest_path.display().to_string()),
            artifact: Some(artifact.display().to_string()),
            detail: format!("nm 不可用（{nm_bin}）；LLVM 工具链缺失 → unknown"),
        };
    };
    if !out.status.success() {
        return FfiReport {
            verdict: CompatVerdict::Unknown,
            expected: expected.clone(),
            actual: vec![],
            removed: vec![],
            added: vec![],
            manifest_path: Some(manifest_path.display().to_string()),
            artifact: Some(artifact.display().to_string()),
            detail: format!("nm 失败：{}", String::from_utf8_lossy(&out.stderr).trim()),
        };
    }
    let actual = parse_nm_output(&String::from_utf8_lossy(&out.stdout));
    let mut removed: Vec<String> = expected
        .iter()
        .filter(|n| !actual.contains(n))
        .cloned()
        .collect();
    let mut added: Vec<String> = actual
        .iter()
        .filter(|n| !expected.contains(n))
        .cloned()
        .collect();
    removed.sort();
    removed.dedup();
    added.sort();
    added.dedup();
    let verdict = if removed.is_empty() {
        CompatVerdict::Pass
    } else {
        CompatVerdict::Fail
    };
    let detail = if removed.is_empty() && added.is_empty() {
        "FFI 导出面与清单一致".to_string()
    } else if removed.is_empty() {
        format!(
            "FFI 导出面兼容；清单漂移（产物新增、清单未更新）：{}",
            added.join(", ")
        )
    } else {
        format!(
            "FFI 导出面破坏性变更（removed）：{}；新增：{}",
            removed.join(", "),
            added.join(", ")
        )
    };
    FfiReport {
        verdict,
        expected: expected.clone(),
        actual,
        removed,
        added,
        manifest_path: Some(manifest_path.display().to_string()),
        artifact: Some(artifact.display().to_string()),
        detail,
    }
}

/// Public wrapper: `nm` from PATH (llvm-nm fallback).
pub fn ffi_check(repo: &Path, config: &FfiConfig) -> FfiReport {
    let nm = if std::process::Command::new("nm")
        .arg("--version")
        .output()
        .is_ok()
    {
        "nm"
    } else {
        "llvm-nm"
    };
    ffi_check_with(nm, repo, config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_declarations_and_skips_noise() {
        let src = r#"
// comment
#include <stdint.h>
#define EXPORT __attribute__((visibility("default")))

EXPORT int32_t uniffi_calc_add(int32_t a, int32_t b);
void uniffi_calc_free(void* ptr);
typedef struct { int x; } Foo;
static void helper(void);
"#;
        let names = parse_c_declarations(src);
        assert!(names.contains(&"uniffi_calc_add".to_string()));
        assert!(names.contains(&"uniffi_calc_free".to_string()));
        assert!(
            !names.contains(&"helper".to_string()),
            "static helper: {names:?}"
        );
    }

    #[test]
    fn parses_nm_output_and_strips_decorations() {
        let raw = "0000000000001234 T uniffi_calc_add\n0000000000005678 T _uniffi_calc_free@@VER_1\n0000000000000000 t internal\n";
        let names = parse_nm_output(raw);
        assert_eq!(
            names,
            vec![
                "internal".to_string(),
                "uniffi_calc_add".to_string(),
                "uniffi_calc_free".to_string()
            ]
        );
    }

    #[test]
    fn removed_is_breaking_added_is_drift() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join("ffi")).unwrap();
        std::fs::create_dir_all(repo.join("target/release")).unwrap();
        std::fs::write(
            repo.join("ffi/exports.h"),
            "int uniffi_calc_add(int a);\nint uniffi_calc_free(void* p);\n",
        )
        .unwrap();
        std::fs::write(repo.join("target/release/libcalc.so"), "artifact").unwrap();
        let shim = dir.path().join("nm");
        std::fs::write(
            &shim,
            "#!/bin/sh\necho '0000 T uniffi_calc_add'\necho '0000 T uniffi_calc_new'\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let cfg = FfiConfig {
            manifest: Some("ffi/exports.h".into()),
            artifact_glob: "target/*/lib*.so".into(),
        };
        let r = ffi_check_with(shim.to_str().unwrap(), &repo, &cfg);
        assert_eq!(r.verdict, CompatVerdict::Fail);
        assert_eq!(r.removed, vec!["uniffi_calc_free".to_string()]);
        assert_eq!(r.added, vec!["uniffi_calc_new".to_string()]);
    }

    #[test]
    fn missing_manifest_or_artifact_is_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = FfiConfig::default();
        let r = ffi_check_with("nm", dir.path(), &cfg);
        assert_eq!(r.verdict, CompatVerdict::Unknown);
        assert!(r.detail.contains("清单缺失"), "{}", r.detail);
    }
}

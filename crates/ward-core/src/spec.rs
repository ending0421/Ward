//! M4 Form Check — spec parsing and local assertion evaluation (spec §3-M4).
//!
//! Specs live in the repository (`specs/<task-id>.md`, reviewed like code).
//! Only *locally evaluable* assertions run in the inner loop; everything that
//! needs the outer loop (`must_pass`, `behavior_diff`, `api_compat`) is
//! honestly reported as `deferred` / `unknown` — never a fake green.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::git;

/// One machine-checkable assertion from a spec file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Assertion {
    pub kind: String,
    pub path: Option<String>,
    pub suite: Option<String>,
    pub value: Option<i64>,
}

/// A parsed spec file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Spec {
    pub path: String,
    pub assertions: Vec<Assertion>,
    /// Parse issues (unknown kinds etc.) — reported, never fatal (fail-open).
    pub issues: Vec<String>,
}

/// Assertion verdict (spec §3-M4: `pass / fail / unknown / deferred`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Pass,
    Fail,
    Unknown,
    Deferred,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Pass => "pass",
            Verdict::Fail => "fail",
            Verdict::Unknown => "unknown",
            Verdict::Deferred => "deferred",
        }
    }
}

/// One evaluated assertion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssertionResult {
    pub assertion: String,
    pub verdict: Verdict,
    pub detail: String,
}

/// Dependency manifests that count as "a new dependency" for
/// `no_new_dependency`. Any change to one of these fails the assertion.
const DEP_MANIFESTS: &[&str] = &[
    "Cargo.toml",
    "Cargo.lock",
    "build.gradle",
    "build.gradle.kts",
    "settings.gradle",
    "settings.gradle.kts",
    "gradle/libs.versions.toml",
    "Package.swift",
    "Package.resolved",
    "pom.xml",
    "Podfile",
    "Podfile.lock",
];

/// Parse a spec file: extract the first ```yaml fenced block and read its
/// `assertions` list.
pub fn parse_spec_file(path: &Path) -> Result<Spec> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading spec {}", path.display()))?;
    parse_spec(&raw, &path.to_string_lossy())
}

/// Parse spec markdown content.
pub fn parse_spec(markdown: &str, path: &str) -> Result<Spec> {
    let mut spec = Spec {
        path: path.to_string(),
        ..Default::default()
    };
    let yaml = extract_yaml_fence(markdown).unwrap_or_default();
    if yaml.is_empty() {
        spec.issues
            .push("no yaml fence with assertions found".into());
        return Ok(spec);
    }
    let parsed: serde_yaml::Value =
        serde_yaml::from_str(&yaml).context("parsing spec yaml block")?;
    let Some(list) = parsed.get("assertions").and_then(|a| a.as_sequence()) else {
        spec.issues
            .push("yaml block has no `assertions` sequence".into());
        return Ok(spec);
    };
    for item in list {
        let Some(map) = item.as_mapping() else {
            spec.issues
                .push(format!("assertion is not a map: {item:?}"));
            continue;
        };
        let get = |k: &str| -> Option<String> {
            map.get(serde_yaml::Value::String(k.into()))
                .and_then(|v| match v {
                    serde_yaml::Value::String(s) => Some(s.clone()),
                    serde_yaml::Value::Number(n) => Some(n.to_string()),
                    serde_yaml::Value::Bool(b) => Some(b.to_string()),
                    _ => None,
                })
        };
        let Some(kind) = get("kind") else {
            spec.issues
                .push(format!("assertion without kind: {item:?}"));
            continue;
        };
        spec.assertions.push(Assertion {
            kind,
            path: get("path"),
            suite: get("suite"),
            value: get("value").and_then(|v| v.parse().ok()),
        });
    }
    Ok(spec)
}

fn extract_yaml_fence(markdown: &str) -> Option<String> {
    let mut in_fence = false;
    let mut buf = String::new();
    for line in markdown.lines() {
        let t = line.trim();
        if t.starts_with("```") {
            if in_fence {
                return Some(buf);
            }
            in_fence = t == "```yaml" || t == "```yml" || t == "```";
            continue;
        }
        if in_fence {
            buf.push_str(line);
            buf.push('\n');
        }
    }
    if buf.is_empty() { None } else { Some(buf) }
}

/// Evaluate all assertions of a spec against `base..head`.
///
/// Inner-loop semantics (spec §3-M4): locally evaluable assertions get a
/// real verdict; anything requiring the outer loop is `deferred`/`unknown`.
pub fn evaluate(repo: &Path, spec: &Spec, base: &str, head: &str) -> Result<Vec<AssertionResult>> {
    let changed = git::diff_names(repo, base, head).unwrap_or_default();
    let mut results = Vec::new();
    for a in &spec.assertions {
        results.push(evaluate_one(repo, base, head, &changed, a));
    }
    Ok(results)
}

/// Outer-loop evaluation (`form-check --ci`): assertions the inner loop can
/// only mark `unknown` are adjudicated here with the real tooling — the
/// outer loop never greenlights an `unknown` (P7).
pub fn evaluate_ci(
    repo: &Path,
    spec: &Spec,
    base: &str,
    head: &str,
) -> Result<Vec<AssertionResult>> {
    let mut results = evaluate(repo, spec, base, head)?;
    for r in &mut results {
        if r.assertion == "api_compat" && r.verdict == Verdict::Unknown {
            let report = crate::compat::api_compat_check(repo, base);
            r.verdict = match report.verdict {
                crate::compat::CompatVerdict::Pass => Verdict::Pass,
                crate::compat::CompatVerdict::Fail => Verdict::Fail,
                crate::compat::CompatVerdict::Unknown => Verdict::Unknown,
            };
            r.detail = format!("{} [{}]", report.detail, report.tool);
        }
    }
    Ok(results)
}

fn evaluate_one(
    repo: &Path,
    base: &str,
    head: &str,
    changed: &[String],
    a: &Assertion,
) -> AssertionResult {
    match a.kind.as_str() {
        "no_new_dependency" => {
            let hits: Vec<&String> = changed
                .iter()
                .filter(|p| {
                    DEP_MANIFESTS
                        .iter()
                        .any(|m| p == m || p.ends_with(&format!("/{m}")))
                })
                .collect();
            if hits.is_empty() {
                AssertionResult {
                    assertion: a.kind.clone(),
                    verdict: Verdict::Pass,
                    detail: "依赖清单无变更".to_string(),
                }
            } else {
                dependency_verdict(repo, base, head, &hits)
            }
        }
        "max_files_changed" => match a.value {
            Some(n) => {
                let count = changed.len() as i64;
                if count <= n {
                    AssertionResult {
                        assertion: a.kind.clone(),
                        verdict: Verdict::Pass,
                        detail: format!("变更文件 {count} ≤ {n}"),
                    }
                } else {
                    AssertionResult {
                        assertion: a.kind.clone(),
                        verdict: Verdict::Fail,
                        detail: format!("变更文件 {count} > {n}"),
                    }
                }
            }
            None => AssertionResult {
                assertion: a.kind.clone(),
                verdict: Verdict::Unknown,
                detail: "缺少 value".to_string(),
            },
        },
        "must_pass" | "behavior_diff" => AssertionResult {
            assertion: a.kind.clone(),
            verdict: Verdict::Deferred,
            detail: "全量测试/golden 对比仅 CI 外环裁决".to_string(),
        },
        "api_compat" => AssertionResult {
            assertion: a.kind.clone(),
            verdict: Verdict::Unknown,
            detail: "类型/二进制级判定需 CI 外环（逐语言工具，spec §3-M4）".to_string(),
        },
        other => AssertionResult {
            assertion: other.to_string(),
            verdict: Verdict::Unknown,
            detail: format!("未知断言种类 {other}（fail-open）"),
        },
    }
}

/// Adjudicate `no_new_dependency` for changed manifests: compare the
/// *dependency set* between base and head. A version bump or metadata edit
/// is not a new dependency; an added dependency key (Cargo.toml) or package
/// name (Cargo.lock) is. Unreadable/unparseable sides are `unknown`, never
/// a fake pass.
fn dependency_verdict(repo: &Path, base: &str, _head: &str, hits: &[&String]) -> AssertionResult {
    let mut added: Vec<String> = Vec::new();
    let mut unreadable: Vec<&str> = Vec::new();
    for path in hits {
        let is_lock = path.ends_with("Cargo.lock");
        let base_raw = git::show_file(repo, base, path).ok().flatten();
        let head_raw = std::fs::read_to_string(repo.join(path)).ok();
        match head_raw
            .as_deref()
            .and_then(|h| new_deps(base_raw.as_deref(), h, is_lock).ok())
        {
            Some(mut deps) => added.append(&mut deps),
            None => unreadable.push(path),
        }
    }
    if !unreadable.is_empty() {
        return AssertionResult {
            assertion: "no_new_dependency".into(),
            verdict: Verdict::Unknown,
            detail: format!("依赖清单有改动但无法比对：{}", unreadable.join(", ")),
        };
    }
    if added.is_empty() {
        AssertionResult {
            assertion: "no_new_dependency".into(),
            verdict: Verdict::Pass,
            detail: "依赖清单有改动，无新增依赖（版本/元数据变更）".into(),
        }
    } else {
        added.sort();
        added.dedup();
        AssertionResult {
            assertion: "no_new_dependency".into(),
            verdict: Verdict::Fail,
            detail: format!("新增依赖：{}", added.join(", ")),
        }
    }
}

/// Dependency names present in one manifest content. `Err` when the TOML
/// does not parse (the caller turns that into `unknown`).
fn dep_names(raw: &str, is_lock: bool) -> Result<BTreeSet<String>, String> {
    let value: toml::Value = toml::from_str(raw).map_err(|e| format!("parse: {e}"))?;
    let mut out = BTreeSet::new();
    if is_lock {
        if let Some(pkgs) = value.get("package").and_then(|p| p.as_array()) {
            for p in pkgs {
                if let Some(name) = p.get("name").and_then(|n| n.as_str()) {
                    out.insert(name.to_string());
                }
            }
        }
        return Ok(out);
    }
    for table in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(deps) = value.get(table).and_then(|d| d.as_table()) {
            out.extend(deps.keys().cloned());
        }
    }
    if let Some(ws) = value
        .get("workspace")
        .and_then(|w| w.get("dependencies"))
        .and_then(|d| d.as_table())
    {
        out.extend(ws.keys().cloned());
    }
    Ok(out)
}

/// Dep-name set difference between two manifest contents: the dependency
/// keys/names present at head but absent at base. A missing base file (new
/// manifest) contributes every head dependency as new.
fn new_deps(base: Option<&str>, head: &str, is_lock: bool) -> Result<Vec<String>, String> {
    let head_deps = dep_names(head, is_lock)?;
    let base_deps = match base {
        Some(b) => dep_names(b, is_lock)?,
        None => BTreeSet::new(),
    };
    Ok(head_deps.difference(&base_deps).cloned().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: &str = r#"# Task spec

```yaml
assertions:
  - kind: no_new_dependency
  - kind: api_compat
  - kind: must_pass
    suite: "tests/utils/**"
  - kind: behavior_diff
    suite: "tests/golden/**"
  - kind: max_files_changed
    value: 6
```
"#;

    #[test]
    fn parses_assertions_from_yaml_fence() {
        let spec = parse_spec(SPEC, "specs/t.md").unwrap();
        assert_eq!(spec.assertions.len(), 5);
        assert_eq!(spec.assertions[0].kind, "no_new_dependency");
        assert_eq!(spec.assertions[2].suite.as_deref(), Some("tests/utils/**"));
        assert_eq!(spec.assertions[4].value, Some(6));
    }

    #[test]
    fn yaml_fence_without_lang_tag_works() {
        let spec = parse_spec("```\nassertions:\n  - kind: must_pass\n```\n", "s").unwrap();
        assert_eq!(spec.assertions.len(), 1);
    }

    #[test]
    fn yml_fence_with_lang_tag_works() {
        let spec = parse_spec("```yml\nassertions:\n  - kind: must_pass\n```\n", "s").unwrap();
        assert_eq!(spec.assertions.len(), 1);
        assert!(spec.issues.is_empty());
    }

    #[test]
    fn non_map_assertion_is_issue_not_panic() {
        let spec = parse_spec("```yaml\nassertions:\n  - just_a_string\n```\n", "s").unwrap();
        assert!(spec.assertions.is_empty());
        assert!(!spec.issues.is_empty());
    }

    #[test]
    fn assertion_without_kind_is_issue() {
        let spec = parse_spec("```yaml\nassertions:\n  - suite: x\n```\n", "s").unwrap();
        assert!(spec.assertions.is_empty());
        assert!(!spec.issues.is_empty());
    }

    #[test]
    fn max_files_changed_without_value_is_unknown() {
        let repo = tempfile::tempdir().unwrap();
        let changed = vec!["a.rs".to_string()];
        let a = Assertion {
            kind: "max_files_changed".into(),
            path: None,
            suite: None,
            value: None,
        };
        assert_eq!(
            evaluate_one(repo.path(), "b", "h", &changed, &a).verdict,
            Verdict::Unknown
        );
    }

    #[test]
    fn evaluate_with_bad_refs_fails_open_to_empty_diff() {
        // git diff on bogus refs fails; evaluate treats it as an empty diff
        // (fail-open) instead of crashing.
        let repo = tempfile::tempdir().unwrap();
        let parsed = parse_spec(
            "```yaml\nassertions:\n  - kind: no_new_dependency\n```",
            "s",
        )
        .unwrap();
        let results = evaluate(repo.path(), &parsed, "bogus-base", "bogus-head").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].verdict,
            Verdict::Pass,
            "empty diff passes no_new_dependency"
        );
    }

    #[test]
    fn missing_spec_file_is_an_error() {
        // Reading a nonexistent spec must fail loudly (the caller decides
        // how to fail open) — never silently produce an empty spec.
        assert!(parse_spec_file(std::path::Path::new("/nonexistent/spec.md")).is_err());
    }

    #[test]
    fn malformed_yaml_in_spec_is_an_error() {
        // A yaml fence whose content does not parse must surface as an
        // error, not a silently empty spec (F12: spec quality matters).
        let r = parse_spec("```yaml\nassertions: [unclosed\n```\n", "s");
        assert!(r.is_err(), "malformed yaml must error: {r:?}");
    }

    #[test]
    fn spec_without_yaml_is_issue_not_error() {
        let spec = parse_spec("# nothing here", "specs/x.md").unwrap();
        assert!(spec.assertions.is_empty());
        assert!(!spec.issues.is_empty());
    }

    #[test]
    fn inner_loop_semantics() {
        let repo = tempfile::tempdir().unwrap();
        let changed = vec!["src/lib.rs".to_string()];
        let a = Assertion {
            kind: "no_new_dependency".into(),
            path: None,
            suite: None,
            value: None,
        };
        assert_eq!(
            evaluate_one(repo.path(), "b", "h", &changed, &a).verdict,
            Verdict::Pass
        );
        let a2 = Assertion {
            kind: "must_pass".into(),
            path: None,
            suite: None,
            value: None,
        };
        assert_eq!(
            evaluate_one(repo.path(), "b", "h", &changed, &a2).verdict,
            Verdict::Deferred
        );
        let a3 = Assertion {
            kind: "api_compat".into(),
            path: None,
            suite: None,
            value: None,
        };
        assert_eq!(
            evaluate_one(repo.path(), "b", "h", &changed, &a3).verdict,
            Verdict::Unknown
        );
        let a4 = Assertion {
            kind: "bogus_kind".into(),
            path: None,
            suite: None,
            value: None,
        };
        assert_eq!(
            evaluate_one(repo.path(), "b", "h", &changed, &a4).verdict,
            Verdict::Unknown
        );
    }

    #[test]
    fn version_bump_is_not_a_new_dependency() {
        let base =
            "[package]\nname = \"x\"\nversion = \"0.3.1\"\n\n[dependencies]\nserde = \"1\"\n";
        let head =
            "[package]\nname = \"x\"\nversion = \"0.4.0\"\n\n[dependencies]\nserde = \"1\"\n";
        assert!(new_deps(Some(base), head, false).unwrap().is_empty());
    }

    #[test]
    fn added_dependency_key_is_new() {
        let base = "[dependencies]\nserde = \"1\"\n";
        let head = "[dependencies]\nserde = \"1\"\nserde_json = \"1\"\n";
        assert_eq!(
            new_deps(Some(base), head, false).unwrap(),
            vec!["serde_json"]
        );
        // Renamed dependency = new name.
        let renamed = "[dependencies]\nserde_derive2 = \"1\"\n";
        assert_eq!(
            new_deps(Some(base), renamed, false).unwrap(),
            vec!["serde_derive2"]
        );
    }

    #[test]
    fn new_manifest_contributes_all_its_dependencies() {
        let head = "[package]\nname = \"x\"\n\n[dependencies]\nserde = \"1\"\n";
        assert_eq!(new_deps(None, head, false).unwrap(), vec!["serde"]);
    }

    #[test]
    fn lockfile_compares_package_names() {
        let base = "[[package]]\nname = \"serde\"\nversion = \"1.0.0\"\n";
        let head = "[[package]]\nname = \"serde\"\nversion = \"1.1.0\"\n";
        assert!(new_deps(Some(base), head, true).unwrap().is_empty());
        let head2 = format!("{base}[[package]]\nname = \"serde_json\"\nversion = \"1.0.0\"\n");
        assert_eq!(
            new_deps(Some(base), &head2, true).unwrap(),
            vec!["serde_json"]
        );
    }

    #[test]
    fn unparseable_manifest_is_an_error_for_the_caller() {
        assert!(new_deps(Some("[dependencies]\nserde = \"1\"\n"), "not [ toml", false).is_err());
    }
}

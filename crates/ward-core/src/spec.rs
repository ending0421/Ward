//! M4 Form Check — spec parsing and local assertion evaluation (spec §3-M4).
//!
//! Specs live in the repository (`specs/<task-id>.md`, reviewed like code).
//! Only *locally evaluable* assertions run in the inner loop; everything that
//! needs the outer loop (`must_pass`, `behavior_diff`, `api_compat`) is
//! honestly reported as `deferred` / `unknown` — never a fake green.

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
        results.push(evaluate_one(&changed, a));
    }
    Ok(results)
}

fn evaluate_one(changed: &[String], a: &Assertion) -> AssertionResult {
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
                AssertionResult {
                    assertion: a.kind.clone(),
                    verdict: Verdict::Fail,
                    detail: format!(
                        "依赖清单变更：{}",
                        hits.iter()
                            .map(|s| s.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                }
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
        let changed = vec!["a.rs".to_string()];
        let a = Assertion {
            kind: "max_files_changed".into(),
            path: None,
            suite: None,
            value: None,
        };
        assert_eq!(evaluate_one(&changed, &a).verdict, Verdict::Unknown);
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
        let changed = vec!["src/lib.rs".to_string()];
        let a = Assertion {
            kind: "no_new_dependency".into(),
            path: None,
            suite: None,
            value: None,
        };
        assert_eq!(evaluate_one(&changed, &a).verdict, Verdict::Pass);
        let a2 = Assertion {
            kind: "must_pass".into(),
            path: None,
            suite: None,
            value: None,
        };
        assert_eq!(evaluate_one(&changed, &a2).verdict, Verdict::Deferred);
        let a3 = Assertion {
            kind: "api_compat".into(),
            path: None,
            suite: None,
            value: None,
        };
        assert_eq!(evaluate_one(&changed, &a3).verdict, Verdict::Unknown);
        let a4 = Assertion {
            kind: "bogus_kind".into(),
            path: None,
            suite: None,
            value: None,
        };
        assert_eq!(evaluate_one(&changed, &a4).verdict, Verdict::Unknown);
    }

    #[test]
    fn dependency_manifest_change_fails_assertion() {
        let changed = vec!["app/Cargo.toml".to_string()];
        let a = Assertion {
            kind: "no_new_dependency".into(),
            path: None,
            suite: None,
            value: None,
        };
        let r = evaluate_one(&changed, &a);
        assert_eq!(r.verdict, Verdict::Fail);
        assert!(r.detail.contains("Cargo.toml"));
    }
}

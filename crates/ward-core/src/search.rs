//! The Spot search pipeline (spec §3-M1): L1 exact structural equality →
//! BM25 recall → L2 simhash ranking → thresholded advisory grades.
//!
//! Discipline enforced here:
//! * **Strong grades require fingerprint evidence.** Text-only matches
//!   (no parseable proposed signature) are never graded strong — fail-open
//!   conservatism, not false confidence.
//! * Thresholds come from `.ward/config.toml` and are *initial values* by
//!   design (weekly golden-set recalibration, spec §9).
//! * Every advisory is recorded into the feedback loop with both action
//!   channels (`agent_action`, `inferred_action`) left for later update.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::config::WardConfig;
use crate::fingerprint;
use crate::store::{Advisory, Store, Symbol};

/// One advisory match.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpotMatch {
    pub path: String,
    /// 1-based inclusive line range, `"start-end"`.
    pub lines: String,
    pub symbol: String,
    pub similarity: f64,
    /// `exact` (L0) | `structural` (L1) | `near` (L2 simhash) | `textual`
    /// (BM25 only — never graded strong).
    pub kind: String,
    pub note: String,
}

/// The advisory payload returned by `spot`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpotResult {
    pub as_of: Option<String>,
    pub stale: bool,
    pub matches: Vec<SpotMatch>,
    pub advisory_id: String,
}

/// Advisory grade, with the discipline that text-only evidence can never be
/// graded strong (spec §3-M1: strong claims require fingerprint evidence).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grade {
    Strong,
    Weak,
    /// Below the weak threshold — not returned at all.
    Filtered,
}

/// Grade a (kind, similarity) pair against the configured thresholds.
pub fn grade(kind: &str, similarity: f64, config: &WardConfig) -> Grade {
    match kind {
        // Text-only matches (no parseable signature → no fingerprint) are
        // capped at Weak by construction.
        "textual" => {
            if similarity >= config.thresholds.weak {
                Grade::Weak
            } else {
                Grade::Filtered
            }
        }
        _ => {
            if similarity >= config.thresholds.strong {
                Grade::Strong
            } else if similarity >= config.thresholds.weak {
                Grade::Weak
            } else {
                Grade::Filtered
            }
        }
    }
}

/// Split an identifier-ish string into search tokens.
///
/// Handles snake_case, camelCase and acronym runs (`debounceFnMS` →
/// `debounce, fn, ms`).
pub fn tokenize(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut current = String::new();
    let mut prev_upper = false;
    for (i, &ch) in chars.iter().enumerate() {
        if ch.is_alphanumeric() {
            if ch.is_uppercase() && !current.is_empty() {
                let prev_lower = !prev_upper;
                let next_lower = chars.get(i + 1).is_some_and(|c| c.is_lowercase());
                // Word boundary: "fooBar" → foo|Bar, "URLParser" → URL|Parser,
                // "FnMS" → Fn|MS (acronym run stays together).
                if prev_lower || (prev_upper && next_lower) {
                    out.push(std::mem::take(&mut current));
                }
            }
            current.push(ch.to_ascii_lowercase());
            prev_upper = ch.is_uppercase();
        } else if !current.is_empty() {
            out.push(std::mem::take(&mut current));
            prev_upper = false;
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// BM25 over symbol names/kinds. Kept deliberately small: the index holds
/// 10⁴–10⁵ symbols, an in-memory pass is milliseconds (spec §4).
pub struct Bm25 {
    docs: Vec<usize>,           // indices into `symbols`
    doc_tokens: Vec<Vec<String>>,
    df: HashMap<String, usize>, // document frequency per token
    n: usize,
    avg_len: f64,
}

impl Bm25 {
    pub fn build(symbols: &[Symbol]) -> Self {
        let mut docs = Vec::new();
        let mut doc_tokens = Vec::new();
        let mut df: HashMap<String, usize> = HashMap::new();
        for (i, s) in symbols.iter().enumerate() {
            let mut toks = tokenize(&s.name);
            toks.push(s.kind.to_lowercase());
            // uniqueness within a doc for df counting
            let mut seen = HashSet::new();
            for t in &toks {
                if seen.insert(t.clone()) {
                    *df.entry(t.clone()).or_default() += 1;
                }
            }
            docs.push(i);
            doc_tokens.push(toks);
        }
        let n = docs.len();
        let avg_len = if n == 0 {
            1.0
        } else {
            doc_tokens.iter().map(|d| d.len()).sum::<usize>() as f64 / n as f64
        };
        Self {
            docs,
            doc_tokens,
            df,
            n,
            avg_len,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    /// BM25 score for a query against doc `i` (k1=1.2, b=0.75).
    fn score(&self, query: &[String], i: usize) -> f64 {
        let doc = &self.doc_tokens[i];
        let dl = doc.len() as f64;
        let mut score = 0.0;
        for q in query {
            let Some(&df) = self.df.get(q) else { continue };
            let tf = doc.iter().filter(|t| *t == q).count() as f64;
            let idf = ((self.n as f64 - df as f64 + 0.5) / (df as f64 + 0.5) + 1.0).ln();
            let denom = tf + 1.2 * (1.0 - 0.75 + 0.75 * dl / self.avg_len);
            score += idf * (tf * 2.2) / denom;
        }
        score
    }

    /// Top `k` doc indices by BM25 score.
    pub fn recall(&self, query: &[String], k: usize) -> Vec<(usize, f64)> {
        let mut scored: Vec<(usize, f64)> = self
            .docs
            .iter()
            .map(|&i| (i, self.score(query, i)))
            .filter(|(_, s)| *s > 0.0)
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(k);
        scored
    }
}

fn line_range(path: &Path, start_byte: i64, end_byte: i64) -> String {
    let Ok(source) = std::fs::read_to_string(path) else {
        return "?".into();
    };
    let start = crate::git::line_of(&source, start_byte.max(0) as usize);
    let end = crate::git::line_of(&source, end_byte.max(0) as usize);
    if start == end {
        format!("{start}")
    } else {
        format!("{start}-{end}")
    }
}

/// Run the full Spot pipeline and record the advisory.
pub fn spot(
    repo: &Path,
    store: &Store,
    config: &WardConfig,
    intent: &str,
    proposed_signature: Option<&str>,
) -> Result<SpotResult> {
    let symbols = store.all_symbols()?;
    let bm25 = Bm25::build(&symbols);

    let mut query = tokenize(intent);
    if let Some(sig) = proposed_signature {
        query.extend(tokenize(sig));
    }

    let mut matches: Vec<SpotMatch> = Vec::new();
    let mut seen: HashSet<usize> = HashSet::new();

    // Layer 1: exact structural equality (L1) when the signature parses.
    let parsed = proposed_signature.and_then(fingerprint::parse_rust);
    let query_struct = parsed.as_ref().map(|t| fingerprint::struct_hash(t));
    let query_sim = parsed
        .as_ref()
        .map(|t| fingerprint::simhash(&fingerprint::subtree_features(t)));

    if let Some(qs) = &query_struct {
        for sym in symbols
            .iter()
            .filter(|s| &s.struct_hash == qs)
            .filter(|s| !config.is_suppressed(&s.file_path))
        {
            matches.push(SpotMatch {
                path: sym.file_path.clone(),
                lines: line_range(&repo.join(&sym.file_path), sym.start_byte, sym.end_byte),
                symbol: sym.name.clone(),
                similarity: 1.0,
                kind: "structural".into(),
                note: "结构全等（归一化后）：克隆/纯改名/字面量替换".into(),
            });
        }
    }

    // Layers 2+3: BM25 recall, then simhash ranking over candidates.
    let candidates = bm25.recall(&query, 50);
    let mut ranked: Vec<(SpotMatch, f64)> = Vec::new();
    let mut claimed: HashSet<(String, String)> = matches
        .iter()
        .map(|m| (m.path.clone(), m.symbol.clone()))
        .collect();
    for (idx, bm25_score) in candidates {
        let sym = &symbols[idx];
        if config.is_suppressed(&sym.file_path) {
            continue;
        }
        if !claimed.insert((sym.file_path.clone(), sym.name.clone())) {
            continue;
        }
        let (kind, sim) = match query_sim {
            Some(q) => ("near", fingerprint::simhash_similarity(q, sym.simhash)),
            // No fingerprint evidence: BM25-only, normalized to [0,1).
            None => ("textual", bm25_score / (bm25_score + 1.0)),
        };
        if seen.insert(idx) {
            ranked.push((
                SpotMatch {
                    path: sym.file_path.clone(),
                    lines: line_range(&repo.join(&sym.file_path), sym.start_byte, sym.end_byte),
                    symbol: sym.name.clone(),
                    similarity: sim,
                    kind: kind.into(),
                    note: String::new(),
                },
                sim,
            ));
        }
    }
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    for (m, sim) in ranked {
        if grade(&m.kind, sim, config) != Grade::Filtered {
            matches.push(m);
        }
        if matches.len() >= config.top_k {
            break;
        }
    }

    let fresh = crate::fresh::check(
        repo,
        store,
        &matches.iter().map(|m| m.path.clone()).collect::<Vec<_>>(),
    )?;

    let query_hash = {
        let mut h = blake3::Hasher::new();
        h.update(intent.as_bytes());
        h.finalize().to_hex().to_string()
    };
    let advisory_id = format!(
        "adv_{}_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or_default(),
        &query_hash[..8]
    );
    store.record_advisory(&Advisory {
        id: advisory_id.clone(),
        tool: "spot".into(),
        ts: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or_default(),
        query_hash,
        result_json: serde_json::to_string(&matches).unwrap_or_else(|_| "[]".into()),
        ..Default::default()
    })?;

    Ok(SpotResult {
        as_of: fresh.as_of,
        stale: fresh.stale,
        matches,
        advisory_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_splits_snake_and_camel() {
        let t = tokenize("debounceFnMS");
        assert_eq!(t, vec!["debounce", "fn", "ms"]);
        let t2 = tokenize("fn debounce(fn: Fn, ms: u64)");
        assert!(t2.contains(&"debounce".to_string()));
        assert_eq!(tokenize("URLParser"), vec!["url", "parser"]);
    }

    #[test]
    fn bm25_ranks_matching_names_first() {
        let syms = vec![
            Symbol {
                id: None,
                file_path: "a.rs".into(),
                language: "rust".into(),
                name: "debounce".into(),
                kind: "function_item".into(),
                start_byte: 0,
                end_byte: 1,
                body_hash: "b".into(),
                struct_hash: "s".into(),
                simhash: 1,
                commit_sha: "c".into(),
            },
            Symbol {
                id: None,
                file_path: "b.rs".into(),
                language: "rust".into(),
                name: "quicksort".into(),
                kind: "function_item".into(),
                start_byte: 0,
                end_byte: 1,
                body_hash: "b2".into(),
                struct_hash: "s2".into(),
                simhash: 2,
                commit_sha: "c".into(),
            },
        ];
        let bm = Bm25::build(&syms);
        let top = bm.recall(&tokenize("debounce leading trailing"), 5);
        assert_eq!(top[0].0, 0);
    }

    #[test]
    fn text_only_matches_are_never_strong() {
        let cfg = WardConfig::default();
        // Any similarity, even 1.0, must grade Weak for text-only evidence.
        assert_eq!(grade("textual", 1.0, &cfg), Grade::Weak);
        assert_eq!(grade("textual", 0.5, &cfg), Grade::Filtered);
        // Fingerprint evidence may grade strong.
        assert_eq!(grade("near", 0.95, &cfg), Grade::Strong);
        assert_eq!(grade("structural", 1.0, &cfg), Grade::Strong);
    }
}

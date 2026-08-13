//! M6 — duplicate clustering for the consolidation workflow (spec §3-M6).
//!
//! Offline union-find over simhash similarities. The output is the *analysis*
//! half of the Consolidation PR bot: clusters plus a deterministic
//! consolidation suggestion template. The actual PR is created by a human or
//! agent (P2: Ward never lands code unattended).
//!
//! Complexity is O(n²) Hamming distances — acceptable for the documented
//! single-repo scale (10⁴–10⁵ symbols) and capped with a fail-open warning
//! beyond it (F11 discipline: monitor, don't pre-optimize).

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::fingerprint;
use crate::store::Store;

/// One cluster member.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterMember {
    pub path: String,
    pub symbol: String,
    pub kind: String,
    /// Similarity to the cluster seed.
    pub similarity: f64,
}

/// A duplicate cluster with a deterministic consolidation suggestion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cluster {
    pub members: Vec<ClusterMember>,
    /// Most frequent member name.
    pub common_name: String,
    /// Structured suggestion (template — no LLM in this layer).
    pub suggestion: String,
}

/// Cluster analysis report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterReport {
    pub clusters: Vec<Cluster>,
    pub pairs_checked: usize,
    /// True when the symbol count exceeded the cap and the analysis was
    /// truncated (fail-open: partial clusters, never a wrong answer).
    pub truncated: bool,
}

/// Hard cap for the O(n²) pass (F11: monitor beyond this, don't grind).
pub const MAX_SYMBOLS: usize = 50_000;

/// Cluster symbols whose full-body simhash similarity is ≥ `threshold`.
pub fn cluster_duplicates(store: &Store, threshold: f64) -> Result<ClusterReport> {
    let symbols = store.all_symbols()?;
    let mut truncated = false;
    let symbols: Vec<_> = if symbols.len() > MAX_SYMBOLS {
        truncated = true;
        symbols.into_iter().take(MAX_SYMBOLS).collect()
    } else {
        symbols
    };

    let n = symbols.len();
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(parent: &mut [usize], x: usize) -> usize {
        if parent[x] != x {
            parent[x] = find(parent, parent[x]);
        }
        parent[x]
    }
    fn union(parent: &mut [usize], a: usize, b: usize) {
        let ra = find(parent, a);
        let rb = find(parent, b);
        if ra != rb {
            parent[ra] = rb;
        }
    }

    // Chunked bucketing with a pigeonhole recall guarantee: two simhashes
    // at Hamming distance ≤ d share at least one of (d+1) equal chunks that
    // partition the 64 bits. Comparing only within buckets therefore misses
    // nothing below the threshold — and turns the O(n²) pass into ~O(n)
    // on real symbol populations.
    let dist_max = ((1.0 - threshold) * 64.0).floor() as u32;
    let n_chunks = (dist_max + 1) as usize;
    let mut pairs_checked = 0usize;
    if n_chunks <= 8 {
        let chunk_bits = 64usize.div_ceil(n_chunks);
        for c in 0..n_chunks {
            let shift = c * chunk_bits;
            let len = (chunk_bits).min(64 - shift);
            if len == 0 {
                continue;
            }
            let mask = if len == 64 {
                u64::MAX
            } else {
                (1u64 << len) - 1
            };
            let mut buckets: std::collections::HashMap<u64, Vec<usize>> =
                std::collections::HashMap::new();
            for (i, sym) in symbols.iter().enumerate() {
                let key = (sym.simhash >> shift) & mask;
                buckets.entry(key).or_default().push(i);
            }
            for members in buckets.values() {
                for (ai, &i) in members.iter().enumerate() {
                    for &j in &members[ai + 1..] {
                        pairs_checked += 1;
                        let sim =
                            fingerprint::simhash_similarity(symbols[i].simhash, symbols[j].simhash);
                        if sim >= threshold {
                            union(&mut parent, i, j);
                        }
                    }
                }
            }
        }
    } else {
        // Extremely low threshold: fall back to the exact O(n²) pass
        // (F11: this band is not expected at production thresholds).
        for i in 0..n {
            for j in (i + 1)..n {
                pairs_checked += 1;
                let sim = fingerprint::simhash_similarity(symbols[i].simhash, symbols[j].simhash);
                if sim >= threshold {
                    union(&mut parent, i, j);
                }
            }
        }
    }

    // Group by root.
    let mut groups: std::collections::BTreeMap<usize, Vec<usize>> =
        std::collections::BTreeMap::new();
    for i in 0..n {
        groups.entry(find(&mut parent, i)).or_default().push(i);
    }

    let mut clusters = Vec::new();
    for (_, members) in groups {
        if members.len() < 2 {
            continue; // singletons are not duplicates
        }
        let seed = symbols[members[0]].simhash;
        let cluster_members: Vec<ClusterMember> = members
            .iter()
            .map(|&i| {
                let s = &symbols[i];
                ClusterMember {
                    path: s.file_path.clone(),
                    symbol: s.name.clone(),
                    kind: s.kind.clone(),
                    similarity: fingerprint::simhash_similarity(seed, s.simhash),
                }
            })
            .collect();
        let common_name = most_common(
            cluster_members
                .iter()
                .map(|m| m.symbol.clone())
                .collect::<Vec<_>>(),
        );
        clusters.push(Cluster {
            suggestion: format!(
                "提取公共实现 `{common_name}`（{} 处重复，建议以普通 PR 提交：提取公共函数 + 迁移调用点 + 附带 M3 差分测试报告，人审合并）",
                cluster_members.len()
            ),
            common_name,
            members: cluster_members,
        });
    }
    // Largest clusters first (biggest consolidation wins).
    clusters.sort_by_key(|c| std::cmp::Reverse(c.members.len()));

    Ok(ClusterReport {
        clusters,
        pairs_checked,
        truncated,
    })
}

fn most_common(names: Vec<String>) -> String {
    let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for n in &names {
        *counts.entry(n.as_str()).or_default() += 1;
    }
    counts
        .into_iter()
        .max_by_key(|(_, c)| *c)
        .map(|(n, _)| n.to_string())
        .unwrap_or_else(|| names.first().cloned().unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn symbol(name: &str, path: &str, simhash: u64) -> crate::store::Symbol {
        crate::store::Symbol {
            id: None,
            file_path: path.into(),
            language: "rust".into(),
            name: name.into(),
            kind: "function_item".into(),
            start_byte: 0,
            end_byte: 1,
            body_hash: format!("b-{name}"),
            struct_hash: format!("s-{name}"),
            simhash,
            sig_simhash: simhash,
            commit_sha: "c".into(),
        }
    }

    #[test]
    fn identical_simhashes_form_one_cluster() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("index.db")).unwrap();
        store
            .replace_file(
                "a.rs",
                &[
                    symbol("f", "a.rs", 0xAAAA),
                    symbol("g", "a.rs", 0xAAAA),
                    symbol("h", "a.rs", 0xBBBB),
                ],
            )
            .unwrap();
        let report = cluster_duplicates(&store, 0.95).unwrap();
        assert_eq!(report.clusters.len(), 1);
        assert_eq!(report.clusters[0].members.len(), 2);
        assert!(!report.truncated);
    }

    #[test]
    fn distant_simhashes_do_not_cluster() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("index.db")).unwrap();
        store
            .replace_file(
                "a.rs",
                &[
                    symbol("f", "a.rs", 0x0000_0000_0000_0000),
                    symbol("g", "a.rs", 0xFFFF_FFFF_FFFF_FFFF),
                ],
            )
            .unwrap();
        let report = cluster_duplicates(&store, 0.95).unwrap();
        assert!(report.clusters.is_empty());
        // Bucketing prunes far pairs entirely — that is the point.
        assert_eq!(report.pairs_checked, 0);
    }

    #[test]
    fn threshold_boundary_is_inclusive() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("index.db")).unwrap();
        // One bit apart → similarity 63/64 = 0.984.
        store
            .replace_file(
                "a.rs",
                &[symbol("f", "a.rs", 0x1), symbol("g", "a.rs", 0x3)],
            )
            .unwrap();
        assert_eq!(cluster_duplicates(&store, 0.98).unwrap().clusters.len(), 1);
        assert!(
            cluster_duplicates(&store, 0.99)
                .unwrap()
                .clusters
                .is_empty()
        );
    }

    #[test]
    fn truncation_flags_oversized_input() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("index.db")).unwrap();
        // Well-distributed hashes (LCG): sequential values would be
        // adversarial to bucketing (all share the low chunks).
        let mut seed = 7u64;
        let mut rand = || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            seed
        };
        let many: Vec<_> = (0..(MAX_SYMBOLS + 10))
            .map(|i| symbol(&format!("f{i}"), "a.rs", rand()))
            .collect();
        store.replace_file("a.rs", &many).unwrap();
        let report = cluster_duplicates(&store, 0.95).unwrap();
        assert!(report.truncated);
    }

    #[test]
    fn bucketed_clusters_match_brute_force() {
        // Property-style consistency: on a small random population, the
        // chunked result must contain exactly the same clusters as a direct
        // O(n²) comparison (the pigeonhole argument says it cannot miss
        // pairs below the threshold).
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("index.db")).unwrap();
        let mut seed = 42u64;
        let mut rand = || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            seed
        };
        let syms: Vec<_> = (0..150)
            .map(|i| symbol(&format!("f{i}"), "a.rs", rand()))
            .collect();
        store.replace_file("a.rs", &syms).unwrap();
        let report = cluster_duplicates(&store, 0.92).unwrap();

        // Brute force with the same threshold.
        let n = syms.len();
        let mut parent: Vec<usize> = (0..n).collect();
        fn find(parent: &mut [usize], x: usize) -> usize {
            if parent[x] != x {
                parent[x] = find(parent, parent[x]);
            }
            parent[x]
        }
        for i in 0..n {
            for j in (i + 1)..n {
                if fingerprint::simhash_similarity(syms[i].simhash, syms[j].simhash) >= 0.92 {
                    let (ra, rb) = (find(&mut parent, i), find(&mut parent, j));
                    if ra != rb {
                        parent[ra] = rb;
                    }
                }
            }
        }
        // Every non-singleton brute-force group must appear as a cluster
        // with the same member count (singletons are excluded by both).
        let mut group_sizes = std::collections::BTreeMap::new();
        for i in 0..n {
            let r = find(&mut parent, i);
            *group_sizes.entry(r).or_insert(0usize) += 1;
        }
        let mut expected_sizes: Vec<usize> =
            group_sizes.values().filter(|s| **s >= 2).cloned().collect();
        let mut got_sizes: Vec<usize> = report.clusters.iter().map(|c| c.members.len()).collect();
        expected_sizes.sort_unstable();
        got_sizes.sort_unstable();
        assert_eq!(
            got_sizes, expected_sizes,
            "chunked clusters must match brute force"
        );
    }

    #[test]
    fn suggestion_mentions_pr_and_differential_tests() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("index.db")).unwrap();
        store
            .replace_file(
                "a.rs",
                &[symbol("parse", "a.rs", 7), symbol("parse", "b.rs", 7)],
            )
            .unwrap();
        let report = cluster_duplicates(&store, 0.95).unwrap();
        let s = &report.clusters[0].suggestion;
        assert!(s.contains("parse"));
        assert!(s.contains("PR"));
        assert!(s.contains("差分测试"));
    }
}

//! The deny→outcome conversion funnel (issue #13): Ward records every
//! event (advisory, ack, inferred adoption) but never reported its own
//! conversion. `ward stats --funnel` answers "is the gate changing
//! behaviour, or just collecting acks?" from Ward's own store.

use std::collections::HashSet;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::store::Store;

/// One funnel cohort (a week of advisories).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FunnelRow {
    /// Cohort label: `week-<n>` (epoch weeks) — stable and timezone-free.
    pub cohort: String,
    pub total: i64,
    pub acked: i64,
    pub reused: i64,
    pub rewritten: i64,
    pub pending: i64,
    pub abandoned: i64,
}

/// Conversion rates over the whole window (None when a denominator is 0).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FunnelConversion {
    pub acked: Option<f64>,
    pub reused: Option<f64>,
    pub rewritten: Option<f64>,
    pub pending: Option<f64>,
    pub abandoned: Option<f64>,
}

/// The full funnel report.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FunnelReport {
    pub total: i64,
    pub acked: i64,
    pub reused: i64,
    pub rewritten: i64,
    pub pending: i64,
    pub abandoned: i64,
    pub conversion: FunnelConversion,
    /// Weekly cohorts, oldest first.
    pub weekly: Vec<FunnelRow>,
    /// Coarse buckets for the period. Bucketing reads only existing data.
    pub note: String,
}

const WEEK_SECS: i64 = 7 * 86_400;
/// An advisory younger than this is `pending`, not `abandoned`.
pub const PENDING_WINDOW_SECS: i64 = 7 * 86_400;

/// One advisory's classification inputs.
struct AdvisoryFacts {
    ts: i64,
    /// Hit symbols recorded in the payload.
    hit_symbols: Vec<String>,
    /// The objective channel's verdict, when inference ran.
    inferred_action: Option<String>,
}

fn facts_of(result_json: &str) -> Vec<String> {
    crate::search::parse_spot_payload(result_json)
        .map(|r| r.matches.into_iter().map(|m| m.symbol).collect())
        .unwrap_or_default()
}

/// Classify every recorded advisory into the funnel.
///
/// Rules (deterministic, from existing tables only):
/// * `acked`     — an ack-registry entry names one of the advisory's hits;
/// * `reused`    — the objective channel inferred `accepted` (call-edge to
///   the top hit);
/// * `rewritten` — the objective channel inferred `rejected`;
/// * `pending`   — no decision yet and younger than [`PENDING_WINDOW_SECS`];
/// * `abandoned` — nothing landed within the window.
pub fn funnel_report(store: &Store, now: i64) -> Result<FunnelReport> {
    let acked_hits: HashSet<String> = store.acked_hits()?;
    // (id, ts, result_json, inferred_action)
    let rows = store.advisory_facts()?;
    let mut report = FunnelReport::default();
    let mut weekly: std::collections::BTreeMap<i64, FunnelRow> = std::collections::BTreeMap::new();

    for (_, ts, result_json, inferred) in rows {
        let facts = AdvisoryFacts {
            ts,
            hit_symbols: facts_of(&result_json),
            inferred_action: inferred,
        };
        let bucket = if facts.hit_symbols.iter().any(|s| acked_hits.contains(s)) {
            "acked"
        } else {
            match facts.inferred_action.as_deref() {
                Some("accepted") => "reused",
                Some("rejected") => "rewritten",
                _ => {
                    if now - facts.ts < PENDING_WINDOW_SECS {
                        "pending"
                    } else {
                        "abandoned"
                    }
                }
            }
        };
        report.total += 1;
        let row = weekly
            .entry(facts.ts / WEEK_SECS)
            .or_insert_with(|| FunnelRow {
                cohort: format!("week-{}", facts.ts / WEEK_SECS),
                ..Default::default()
            });
        row.total += 1;
        match bucket {
            "acked" => {
                report.acked += 1;
                row.acked += 1;
            }
            "reused" => {
                report.reused += 1;
                row.reused += 1;
            }
            "rewritten" => {
                report.rewritten += 1;
                row.rewritten += 1;
            }
            "pending" => {
                report.pending += 1;
                row.pending += 1;
            }
            _ => {
                report.abandoned += 1;
                row.abandoned += 1;
            }
        }
    }
    let rate = |n: i64| (report.total > 0).then(|| n as f64 / report.total as f64);
    report.conversion = FunnelConversion {
        acked: rate(report.acked),
        reused: rate(report.reused),
        rewritten: rate(report.rewritten),
        pending: rate(report.pending),
        abandoned: rate(report.abandoned),
    };
    report.weekly = weekly.into_values().collect();
    report.note = "分桶规则：acked=登记处命中；reused/rewritten=客观通道推断；pending=<7天未决；abandoned=超窗未决。"
        .to_string();
    Ok(report)
}

/// Human-readable funnel block for `ward stats --funnel`.
pub fn render_funnel(report: &FunnelReport) -> String {
    let pct = |v: Option<f64>| {
        v.map(|x| format!("{:.0}%", x * 100.0))
            .unwrap_or_else(|| "-".into())
    };
    let mut out = format!(
        "M1 转化漏斗（共 {} 条 advisory）\n\
         acked: {} ({}) | reused: {} ({}) | rewritten: {} ({})\n\
         pending: {} ({}) | abandoned: {} ({})\n\
         {}",
        report.total,
        report.acked,
        pct(report.conversion.acked),
        report.reused,
        pct(report.conversion.reused),
        report.rewritten,
        pct(report.conversion.rewritten),
        report.pending,
        pct(report.conversion.pending),
        report.abandoned,
        pct(report.conversion.abandoned),
        report.note,
    );
    if !report.weekly.is_empty() {
        out.push_str("\n周序列（cohort: total/acked/reused/rewritten/pending/abandoned）");
        for row in &report.weekly {
            out.push_str(&format!(
                "\n  {}: {}/{}/{}/{}/{}/{}",
                row.cohort,
                row.total,
                row.acked,
                row.reused,
                row.rewritten,
                row.pending,
                row.abandoned
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Advisory, Store};

    fn seed(store: &Store, id: &str, ts: i64, symbols: &[&str], inferred: Option<&str>) {
        let matches: Vec<String> = symbols
            .iter()
            .map(|s| {
                format!(
                    r#"{{"path":"src/x.rs","lines":"1","symbol":"{s}","similarity":0.95,"kind":"near","note":""}}"#
                )
            })
            .collect();
        let payload = format!(
            r#"{{"as_of":null,"stale":false,"matches":[{}],"advisory_id":"{id}","query":"q"}}"#,
            matches.join(",")
        );
        store
            .record_advisory(&Advisory {
                id: id.into(),
                tool: "spot".into(),
                ts,
                query_hash: "qh".into(),
                result_json: payload,
                ..Default::default()
            })
            .unwrap();
        if let Some(action) = inferred {
            store.set_inferred_action(id, action, "sha").unwrap();
        }
    }

    #[test]
    fn classifies_all_five_buckets() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("index.db")).unwrap();
        let now = 1_700_000_000i64;
        seed(&store, "a", now - 100 * 86_400, &["hit_a"], None);
        seed(
            &store,
            "b",
            now - 100 * 86_400,
            &["hit_b"],
            Some("accepted"),
        );
        seed(
            &store,
            "c",
            now - 100 * 86_400,
            &["hit_c"],
            Some("rejected"),
        );
        seed(&store, "d", now - 100 * 86_400, &["hit_d"], None);
        seed(&store, "e", now - 86_400, &["hit_e"], None); // fresh ⇒ pending
        store
            .record_ack(&crate::store::Ack {
                id: None,
                new_symbol: "new".into(),
                hit_symbol: "hit_a".into(),
                hit_path: None,
                reason: "r".into(),
                advisory_id: Some("a".into()),
                kind: "ack".into(),
                ts: now,
            })
            .unwrap();

        let report = funnel_report(&store, now).unwrap();
        assert_eq!(report.total, 5);
        assert_eq!(report.acked, 1);
        assert_eq!(report.reused, 1);
        assert_eq!(report.rewritten, 1);
        assert_eq!(report.pending, 1);
        assert_eq!(report.abandoned, 1);
        assert!((report.conversion.reused.unwrap() - 0.2).abs() < 1e-9);
        assert!(!report.weekly.is_empty());
        let text = render_funnel(&report);
        assert!(text.contains("M1 转化漏斗"), "{text}");
    }

    #[test]
    fn empty_store_is_a_zero_funnel_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("index.db")).unwrap();
        let report = funnel_report(&store, 1_700_000_000).unwrap();
        assert_eq!(report.total, 0);
        assert!(report.conversion.acked.is_none());
        assert!(report.weekly.is_empty());
    }
}

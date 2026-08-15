//! Threshold calibration (spec §9): precision per threshold band with
//! Wilson confidence intervals — intervals, not point estimates, because
//! weekly golden sets are small samples. Calibration suggests; it never
//! rewrites config on its own (P2: `--apply` is the caller's decision).

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::store::Store;

/// Wilson 95% interval for a binomial proportion.
pub fn wilson(yes: i64, no: i64, z: f64) -> (f64, f64) {
    let n = (yes + no) as f64;
    if n == 0.0 {
        return (0.0, 1.0);
    }
    let p = yes as f64 / n;
    let z2 = z * z;
    let denom = 1.0 + z2 / n;
    let centre = (p + z2 / (2.0 * n)) / denom;
    let margin = z * (p * (1.0 - p) / n + z2 / (4.0 * n * n)).sqrt() / denom;
    ((centre - margin).max(0.0), (centre + margin).min(1.0))
}

/// One threshold band's verdicts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BandRow {
    pub threshold: f64,
    pub yes: i64,
    pub no: i64,
    pub precision: f64,
    pub ci_low: f64,
    pub ci_high: f64,
    /// True when the sample is large enough to trust (>= 20 verdicts).
    pub sufficient_sample: bool,
}

/// The calibration report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibrationReport {
    pub total_verdicts: i64,
    pub rows: Vec<BandRow>,
    /// Suggested strong threshold (highest with ci_low >= 0.60), if any.
    pub suggested_strong: Option<f64>,
    pub note: String,
}

/// Verdict counts above each threshold, scanning 0.80..=0.98.
pub fn calibrate(store: &Store) -> Result<CalibrationReport> {
    let labels = store.labels_with_similarity()?;
    let total = labels.len() as i64;
    let mut rows = Vec::new();
    let mut suggested = None;
    let mut threshold = 0.80f64;
    while threshold <= 0.98 + 1e-9 {
        let (mut yes, mut no) = (0i64, 0i64);
        for (sim, verdict) in &labels {
            if *sim >= threshold {
                if verdict == "y" {
                    yes += 1;
                } else {
                    no += 1;
                }
            }
        }
        let (lo, hi) = wilson(yes, no, 1.96);
        let sufficient = yes + no >= 20;
        let row = BandRow {
            threshold: (threshold * 100.0).round() / 100.0,
            yes,
            no,
            precision: if yes + no == 0 {
                0.0
            } else {
                yes as f64 / (yes + no) as f64
            },
            ci_low: lo,
            ci_high: hi,
            sufficient_sample: sufficient,
        };
        if sufficient && lo >= 0.60 {
            suggested = Some(row.threshold);
        }
        rows.push(row);
        threshold += 0.02;
    }
    let note = if total < 20 {
        format!("样本量不足：仅 {total} 条标注（可信需要 ≥20）；当前建议不可靠，继续按周标注")
    } else {
        "样本量达标；建议阈值为 ci_low ≥ 0.60 的最高档位".to_string()
    };
    Ok(CalibrationReport {
        total_verdicts: total,
        rows,
        suggested_strong: suggested,
        note,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Label, Store};

    fn seed(store: &Store, sims: &[(f64, &str)]) {
        for (i, (sim, verdict)) in sims.iter().enumerate() {
            store
                .record_label(&Label {
                    id: None,
                    advisory_id: format!("a{i}"),
                    match_index: 0,
                    annotator: "human".into(),
                    query_hash: None,
                    language: None,
                    kind: Some("near".into()),
                    similarity: Some(*sim),
                    verdict: verdict.to_string(),
                    ts: 1,
                })
                .unwrap();
        }
    }

    #[test]
    fn wilson_is_bounded_and_sane() {
        let (lo, hi) = wilson(8, 2, 1.96);
        assert!(lo <= 0.8 && 0.8 <= hi, "({lo},{hi})");
        assert!(
            (lo - 0.490).abs() < 0.05,
            "8/10 95% CI low ≈ 0.49, got {lo}"
        );
        assert_eq!(wilson(0, 0, 1.96), (0.0, 1.0));
        assert_eq!(wilson(10, 0, 1.96).1, 1.0);
    }

    #[test]
    fn calibration_suggests_highest_trusted_threshold() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("index.db")).unwrap();
        let mut sims = Vec::new();
        // 22 high-similarity matches: 18 yes, 4 no → precision ~0.82.
        for i in 0..22 {
            let v = if i < 18 { "y" } else { "n" };
            sims.push((0.95, v));
        }
        seed(&store, &sims);
        let report = calibrate(&store).unwrap();
        assert_eq!(report.total_verdicts, 22);
        assert!(report.rows.iter().any(|r| r.threshold == 0.92));
        let r95 = report.rows.iter().find(|r| r.threshold == 0.94).unwrap();
        assert!(r95.sufficient_sample && r95.yes == 18 && r95.no == 4);
        assert!(report.suggested_strong.is_some());
        assert!(!report.note.contains("样本量不足"));
    }

    #[test]
    fn small_sample_warns_and_suggests_nothing_reliable() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("index.db")).unwrap();
        seed(&store, &[(0.93, "y"), (0.95, "n")]);
        let report = calibrate(&store).unwrap();
        assert_eq!(report.total_verdicts, 2);
        assert!(report.note.contains("样本量不足"));
        assert!(report.suggested_strong.is_none());
    }

    #[test]
    fn verdicts_below_threshold_do_not_count() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("index.db")).unwrap();
        seed(&store, &[(0.79, "y"), (0.95, "n")]);
        let report = calibrate(&store).unwrap();
        let r95 = report.rows.iter().find(|r| r.threshold == 0.94).unwrap();
        assert_eq!(r95.yes + r95.no, 1, "0.79 sits below the 0.94 band");
    }
}

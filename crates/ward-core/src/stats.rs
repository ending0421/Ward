//! The governance report (spec §9): the consumer of everything Ward
//! records. Dual-channel adoption, duplicate-cluster trend, constraint
//! decay, calibration progress — one command, one JSON shape.

use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::config::WardConfig;
use crate::store::{Snapshot, Store};

/// Adoption counts across both channels (spec §3-M1 dual channel).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Adoption {
    pub inferred_total: i64,
    pub inferred_accepted: i64,
    pub inferred_rejected: i64,
    pub self_reported_total: i64,
    pub self_reported_accepted: i64,
    /// |inferred rate - self-reported rate| — a large gap means the agent's
    /// self-reports are drifting from what its commits actually do.
    pub divergence: Option<f64>,
}

/// Instant values for the CLI table.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CurrentStats {
    pub adoption: Adoption,
    pub clusters: i64,
    pub symbols: i64,
    pub labels: i64,
    pub contract_runs: i64,
    pub contract_pass_rate: Option<f64>,
    /// Constraint decay: pass rate of the most recent 10 runs vs the first
    /// 10 (spec §9 longitudinal analysis, coarse approximation).
    pub decay_hint: Option<f64>,
}

/// One time-series point.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeriesPoint {
    pub ts: i64,
    pub value: f64,
}

/// One metric series.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Series {
    pub metric: String,
    pub points: Vec<SeriesPoint>,
}

/// The full governance report (JSON shape stable enough for dashboards).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GovReport {
    pub repo: String,
    pub generated_at: i64,
    pub schema_version: u32,
    pub stability: String,
    pub current: CurrentStats,
    pub series: Vec<Series>,
    /// Inter-annotator agreement over the golden set (spec §8 标注腐烂护栏).
    pub agreement: crate::label::AgreementReport,
}

fn rate(ok: i64, total: i64) -> Option<f64> {
    (total > 0).then(|| ok as f64 / total as f64)
}

/// Take an idempotent daily snapshot (same day → overwrite, not append).
pub fn snapshot_now(_repo: &Path, store: &Store, config: &WardConfig) -> Result<Snapshot> {
    let day = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| (d.as_secs() / 86400) as i64)
        .unwrap_or_default();
    let (symbols, advisories, runs, pass) = store.counts()?;
    let clusters = crate::cluster::cluster_duplicates_with(
        store,
        config.thresholds.strong,
        config.clusters.exclude_tests,
    )?
    .clusters
    .len() as i64;
    let labels = store.label_count()?;
    let snap = Snapshot {
        ts: day,
        symbols,
        clusters,
        advisories,
        labels,
        contract_runs: runs,
        contract_pass: pass,
    };
    store.record_snapshot(&snap)?;
    Ok(snap)
}

/// Assemble the governance report (snapshots first, then aggregate).
pub fn stats(repo: &Path, store: &Store, config: &WardConfig) -> Result<GovReport> {
    snapshot_now(repo, store, config)?;
    let snaps = store.snapshots()?;

    let (symbols, _advisories, runs, pass) = store.counts()?;
    let clusters = crate::cluster::cluster_duplicates(store, config.thresholds.strong)?
        .clusters
        .len() as i64;
    let labels = store.label_count()?;
    let (it, ia, ir, st, sa) = store.adoption_counts()?;
    let adoption = Adoption {
        inferred_total: it,
        inferred_accepted: ia,
        inferred_rejected: ir,
        self_reported_total: st,
        self_reported_accepted: sa,
        divergence: match (rate(ia, it), rate(sa, st)) {
            (Some(a), Some(b)) => Some((a - b).abs()),
            _ => None,
        },
    };

    let pass_rate = rate(pass, runs);
    let decay_hint = store.constraint_decay_hint()?;

    let series = vec![
        Series {
            metric: "symbols".into(),
            points: snaps
                .iter()
                .map(|s| SeriesPoint {
                    ts: s.ts,
                    value: s.symbols as f64,
                })
                .collect(),
        },
        Series {
            metric: "duplicate_clusters".into(),
            points: snaps
                .iter()
                .map(|s| SeriesPoint {
                    ts: s.ts,
                    value: s.clusters as f64,
                })
                .collect(),
        },
        Series {
            metric: "advisories".into(),
            points: snaps
                .iter()
                .map(|s| SeriesPoint {
                    ts: s.ts,
                    value: s.advisories as f64,
                })
                .collect(),
        },
        Series {
            metric: "labels".into(),
            points: snaps
                .iter()
                .map(|s| SeriesPoint {
                    ts: s.ts,
                    value: s.labels as f64,
                })
                .collect(),
        },
    ];

    Ok(GovReport {
        repo: repo.to_string_lossy().into_owned(),
        generated_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or_default(),
        schema_version: 1,
        stability: "unstable".into(),
        current: CurrentStats {
            adoption,
            clusters,
            symbols,
            labels,
            contract_runs: runs,
            contract_pass_rate: pass_rate,
            decay_hint,
        },
        series,
        agreement: crate::label::annotator_agreement(store)?,
    })
}

/// Human-readable CLI table.
pub fn render_table(report: &GovReport) -> String {
    let a = &report.current.adoption;
    let divergence = a
        .divergence
        .map(|d| format!("{d:.2}"))
        .unwrap_or_else(|| "-".into());
    let kappa = report
        .agreement
        .fleiss_kappa
        .map(|k| format!("{k:+.2}"))
        .unwrap_or_else(|| "-".into());
    let mut out = format!(
        "Ward 治理报表 {}\n\
         采纳（推断通道）: {}/{} = {:.0}% | 拒绝: {}\n\
         采纳（自报通道）: {}/{} = {:.0}% | 背离: {}\n\
         符号: {} | 重复簇: {} | 黄金集标注: {}\n\
         断言执行: {} | 通过率: {} | 衰减提示: {}\n\
         标注一致性: 双标 match {} 个 | Fleiss κ = {} | {}",
        report.repo,
        a.inferred_accepted,
        a.inferred_total,
        rate(a.inferred_accepted, a.inferred_total).unwrap_or(0.0) * 100.0,
        a.inferred_rejected,
        a.self_reported_accepted,
        a.self_reported_total,
        rate(a.self_reported_accepted, a.self_reported_total).unwrap_or(0.0) * 100.0,
        divergence,
        report.current.symbols,
        report.current.clusters,
        report.current.labels,
        report.current.contract_runs,
        report
            .current
            .contract_pass_rate
            .map(|r| format!("{:.0}%", r * 100.0))
            .unwrap_or_else(|| "-".into()),
        report
            .current
            .decay_hint
            .map(|d| format!("{:+.2}", d))
            .unwrap_or_else(|| "-".into()),
        report.agreement.double_labeled,
        kappa,
        report
            .agreement
            .per_annotator
            .iter()
            .map(|(n, c)| format!("{n}={c}"))
            .collect::<Vec<_>>()
            .join(", "),
    );
    if let Some(k) = report.agreement.fleiss_kappa {
        if k < 0.4 {
            out.push_str("\n  ⚠ 标注一致率低（Fleiss κ < 0.4）：阈值校准失真风险（spec §8 标注腐烂护栏），请交叉抽检标注流程");
        }
    }
    out.push_str(&format!(
        "\n 快照点数: {}（系列见 --json）",
        report.series.first().map(|s| s.points.len()).unwrap_or(0),
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Advisory, Label, Store};

    fn seeded() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("index.db")).unwrap();
        for i in 0..5 {
            store
                .record_advisory(&Advisory {
                    id: format!("a{i}"),
                    tool: "spot".into(),
                    ts: 1,
                    query_hash: "q".into(),
                    result_json: "[]".into(),
                    agent_action: Some("accepted".into()),
                    ..Default::default()
                })
                .unwrap();
        }
        for i in 0..3 {
            store
                .record_label(&Label {
                    id: None,
                    advisory_id: format!("a{i}"),
                    match_index: 0,
                    annotator: "human".into(),
                    query_hash: None,
                    language: None,
                    kind: Some("near".into()),
                    similarity: Some(0.93),
                    verdict: if i < 2 { "y" } else { "n" }.into(),
                    ts: 1,
                })
                .unwrap();
        }
        (dir, store)
    }

    #[test]
    fn snapshot_is_idempotent_per_day() {
        let (dir, store) = seeded();
        let s1 = snapshot_now(dir.path(), &store, &WardConfig::default()).unwrap();
        let s2 = snapshot_now(dir.path(), &store, &WardConfig::default()).unwrap();
        assert_eq!(s1.ts, s2.ts);
        assert_eq!(
            store.snapshots().unwrap().len(),
            1,
            "same day must overwrite"
        );
        assert_eq!(s1.advisories, 5);
        assert_eq!(s1.labels, 3);
    }

    #[test]
    fn report_aggregates_adoption_and_series() {
        let (dir, store) = seeded();
        let report = stats(dir.path(), &store, &WardConfig::default()).unwrap();
        assert_eq!(report.current.adoption.self_reported_accepted, 5);
        assert_eq!(report.current.labels, 3);
        assert_eq!(report.series.len(), 4);
        assert_eq!(report.series[0].metric, "symbols");
        assert_eq!(report.series[0].points.len(), 1);
        assert_eq!(report.schema_version, 1);
        assert_eq!(report.stability, "unstable");
        let table = render_table(&report);
        assert!(table.contains("采纳（推断通道）"));
        assert!(table.contains("黄金集标注: 3"));
    }
}

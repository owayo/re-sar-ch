//! 絶対水準 — 意味が確立している固定条件による検出。
//!
//! 見るのは「**文脈なしで判断できる絶対値**」だけである。
//! `%idle` が 5% を下回る、`pswpin/s` が 0 でない、`%fsused` が 95% 以上。
//! どれも搭載量・CPU 数・ワークロードに依存せず意味が定まる。
//!
//! CPU 数に依存する `runq-sz` や搭載量に依存する `kbavail` には
//! 固定条件を置かない ([`crate::analyze::metric_catalog`] の
//! `context_dependent_metrics_have_no_fixed_condition` テストで固定してある)。
//! **固定条件が無いことは「検出しない」ではない** — その系列は
//! [`crate::detect::robust`] と [`crate::detect::level_shift`] が見る。
//!
//! # 他の経路の実行条件にならない
//!
//! この経路が 0 件でも他の 2 経路はそのまま走る。逆もそうである。
//! ここで計算する [`flagged_share`] / [`satisfies_any_fixed`] は
//! **比較基準の説明のために**逸脱経路へ渡すが、実行の可否には使わない。

use crate::analyze::assessment::{NotEvaluated, RouteStatus};
use crate::analyze::metric_catalog::{CatalogEntry, FixedCondition};

use super::{
    Baseline, DETECTOR_VERSION, DecisionBasis, DecisionEvidence, DetectOptions, Detection,
    Observation, PreparedSeries, SeriesKey, group_runs,
};

/// 値が固定条件のいずれかを満たすか。
///
/// 比較基準の中央値がすでに異常側にあるかを調べるのに使う
/// (`BaselineEvidence::median_within_fixed_condition`)。
pub fn satisfies_any_fixed(entry: &CatalogEntry, value: f64) -> bool {
    entry
        .fixed
        .iter()
        .any(|f| f.comparison.holds(value, f.value))
}

/// 材料のうち固定条件を満たした値の割合 (0.0〜1.0)。
///
/// **「異常が入力の大半を占めている」ことの指標。**
/// 1.0 に近ければ、入力自身から作った基準はその状態に寄っている。
/// 固定条件を持たない系列では常に 0.0 を返す (割合の意味が無い)。
pub fn flagged_share(entry: &CatalogEntry, values: &[f64]) -> f64 {
    if entry.fixed.is_empty() || values.is_empty() {
        return 0.0;
    }
    let hit = values
        .iter()
        .filter(|v| satisfies_any_fixed(entry, **v))
        .count();
    hit as f64 / values.len() as f64
}

/// 固定条件で検出する。
pub fn detect(
    series: &PreparedSeries,
    entry: &'static CatalogEntry,
    baseline: &Baseline,
    _opts: &DetectOptions,
) -> (Vec<Detection>, RouteStatus) {
    if entry.fixed.is_empty() {
        return (
            Vec::new(),
            RouteStatus::NotEvaluated {
                reason: NotEvaluated::NoFixedConditionDeclared,
            },
        );
    }
    if series.is_empty() {
        return (
            Vec::new(),
            RouteStatus::NotEvaluated {
                reason: NotEvaluated::NoUsableObservation,
            },
        );
    }

    let mut out = Vec::new();
    for condition in entry.fixed {
        // 連続区間ごとに見る。**不連続を跨いで「続いた」と数えない。**
        for segment in series.iter_segments() {
            for (a, b) in group_runs(segment, |o| {
                condition.comparison.holds(o.value, condition.value)
            }) {
                let hit = &segment[a..b];
                if (hit.len() as u32) < condition.min_samples {
                    continue;
                }
                out.push(build(series, entry, condition, baseline, hit));
            }
        }
    }

    let status = if out.is_empty() {
        RouteStatus::Evaluated
    } else {
        RouteStatus::Detected {
            count: out.len(),
            blind_edge_points: 0,
        }
    };
    (out, status)
}

fn build(
    series: &PreparedSeries,
    entry: &'static CatalogEntry,
    condition: &FixedCondition,
    baseline: &Baseline,
    hit: &[Observation],
) -> Detection {
    let basis = DecisionBasis::FixedCondition {
        condition_id: condition.id,
        comparison: condition.comparison,
        threshold: condition.value,
        min_samples: condition.min_samples,
        rationale: condition.rationale,
    };
    Detection {
        detector_version: DETECTOR_VERSION,
        series: SeriesKey::from_metric(&series.key),
        metric_label: entry.label,
        unit: series.unit,
        kind: series.kind,
        origin: series.origin,
        pattern: condition.pattern,
        support: series.support_of(hit),
        baseline: baseline.evidence.clone(),
        decision: DecisionEvidence::new(basis, hit),
        base_priority: condition.priority,
        possible_interpretations: entry.interpretations,
        not_established: entry.not_established,
    }
}

#[cfg(test)]
mod tests {
    use super::super::testing::*;
    use super::super::*;
    use super::*;

    fn run(t: crate::analyze::timeline::MetricTimeline) -> (Vec<Detection>, RouteStatus) {
        let entry = entry_for(&t);
        let series = PreparedSeries::from_timeline(&t);
        let opts = DetectOptions::default();
        let material = series.observations.clone();
        let baseline = build_baseline(&series, entry, material, &opts);
        super::detect(&series, entry, &baseline, &opts)
    }

    /// `%idle` が 5% 以下で続いた区間を拾う。
    ///
    /// **形と優先度はカタログの宣言をそのまま運ぶ。**
    /// この経路が独自に「飽和」と解釈してはいけない
    /// (`%idle ≤ 5` が何を意味するかはカタログ側の判断である)。
    #[test]
    fn cpu_idle_exhaustion_is_detected() {
        let mut v = vec![80.0; 20];
        v[10] = 1.0;
        v[11] = 2.0;
        v[12] = 0.5;
        let t = cpu_idle(&vals(&v));
        let declared = entry_for(&t)
            .fixed
            .iter()
            .find(|f| f.id == "cpu-idle-exhausted")
            .expect("カタログの固定条件");
        let (found, status) = run(t);
        assert_eq!(found.len(), 1);
        let d = &found[0];
        assert_eq!(d.pattern, declared.pattern, "宣言された形を運ぶ");
        assert_eq!(d.base_priority, declared.priority, "宣言された優先度を運ぶ");
        assert_eq!(d.support.samples, 3);
        assert_eq!(d.route(), DetectRoute::FixedCondition);
        assert!(matches!(status, RouteStatus::Detected { count: 1, .. }));
        let DecisionBasis::FixedCondition { condition_id, .. } = d.decision.basis else {
            panic!("固定条件の根拠が入っているべき");
        };
        assert_eq!(condition_id, "cpu-idle-exhausted");
    }

    /// `min_samples` 未満の単発は拾わない。
    #[test]
    fn a_single_sample_below_min_samples_is_not_reported() {
        let mut v = vec![80.0; 20];
        v[10] = 1.0;
        let (found, status) = run(cpu_idle(&vals(&v)));
        assert!(found.is_empty(), "1 回だけでは報告しない");
        assert!(matches!(status, RouteStatus::Evaluated));
    }

    /// 発生自体が事象の系列は 1 回でも拾う。
    #[test]
    fn occurrence_metrics_are_reported_from_a_single_sample() {
        let mut v = vec![0.0; 20];
        v[7] = 12.0;
        let (found, _) = run(pswpin(&vals(&v)));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].pattern, Pattern::Emergence);
        assert_eq!(found[0].support.samples, 1);
    }

    /// 不連続を跨いで「続いた」と数えない。
    #[test]
    fn a_run_does_not_span_a_discontinuity() {
        // 5% 以下が RESTART の前後に 1 回ずつ。跨いで 2 回続いたとは数えない。
        let mut points = vals(&[80.0; 20]);
        points[9] = P::V(1.0);
        points[10] = P::Restart;
        points[11] = P::V(1.0);
        let (found, _) = run(cpu_idle(&points));
        assert!(
            found.is_empty(),
            "RESTART を跨いで min_samples を満たしたと数えてはいけない"
        );
    }

    /// 固定条件を持たない系列はこの経路では評価しない (が、他経路は動く)。
    #[test]
    fn metrics_without_a_fixed_condition_decline_this_route() {
        let (found, status) = run(runq(&vals(&[1.0; 20])));
        assert!(found.is_empty());
        assert!(matches!(
            status,
            RouteStatus::NotEvaluated {
                reason: NotEvaluated::NoFixedConditionDeclared
            }
        ));
    }

    #[test]
    fn flagged_share_counts_the_material_inside_the_condition() {
        let t = cpu_idle(&vals(&[1.0, 1.0, 1.0, 80.0]));
        let entry = entry_for(&t);
        assert!((flagged_share(entry, &[1.0, 1.0, 1.0, 80.0]) - 0.75).abs() < 1e-9);
        // 固定条件を持たない系列では割合の意味が無い
        let q = runq(&vals(&[1.0]));
        assert_eq!(flagged_share(entry_for(&q), &[1.0]), 0.0);
    }

    #[test]
    fn satisfies_any_fixed_follows_the_declared_comparison() {
        let t = cpu_idle(&vals(&[1.0]));
        let entry = entry_for(&t);
        assert!(satisfies_any_fixed(entry, 4.0), "%idle 4% は 5% 以下");
        assert!(!satisfies_any_fixed(entry, 50.0));
    }
}

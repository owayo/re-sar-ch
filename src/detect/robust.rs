//! 参照分布からの逸脱 — median と MAD からの偏りによる検出。
//!
//! 入力自身から作った比較基準 ([`crate::detect::BaselineEvidence`]) に対して
//! 「この入力の中で明らかに外れている点」を拾う。
//! 平均と標準偏差ではなく median と MAD を使うのは、
//! 外れ値そのものが基準を引きずらないようにするためである。
//!
//! # `MAD == 0` を ε で割らない
//!
//! 値がほぼ一定の系列では MAD が 0 になる。そこを `1e-9` のような ε で割ると
//! スコアが天文学的な値になり、無害な 1 ビットの揺れが最重大の検出になる。
//! `MAD == 0` は [`crate::detect::Dispersion::NotMeasurable`] という
//! **別の状態**として扱い、この経路は何も出さずに降りる。
//! その系列は固定条件経路と水準変化経路が見る。
//!
//! MAD は偏差の中央値なので、**過半数の値が中央値と一致すれば厳密に 0** になる。
//! 「0 ではないが極端に小さい」状態は、同値がちょうど半数前後を占めるときに
//! 起こり得る。そこは [`crate::detect::Dispersion::TooSparse`] で降りる。
//!
//! # スコアを `z` と呼ばない
//!
//! `(x - median) / (1.4826 × MAD)` と書くと「σ 何個ぶん」に見えるが、
//! レート系列は右に裾が長く正規分布ではないので、その読み方は成立しない。
//! ここでは**生の MAD に対する比**を出し、閾値もその比で宣言する
//! ([`crate::detect::DetectThresholds::deviation_ratio`])。
//!
//! # 絶対差の下限も併せて要求する
//!
//! MAD が測れていても極端に小さいことはある。比だけを見ると
//! 「中央値 48.00 に対して 48.03」が巨大な比になる。
//! そこで**指標ごとの最小有意変化量**
//! ([`crate::analyze::metric_catalog::ShiftMagnitude`]) を絶対差の下限として
//! 併せて要求する。宣言が無い系列 (普段 0 の事象カウンタ) は
//! 下限なしとし、固定条件経路に任せる。

use crate::analyze::assessment::{NotEvaluated, RouteStatus};
use crate::analyze::metric_catalog::CatalogEntry;

use super::{
    Baseline, DETECTOR_VERSION, DecisionBasis, DecisionEvidence, DetectOptions, Detection,
    Dispersion, Observation, Pattern, PreparedSeries, SeriesKey, ShiftDirection, group_runs,
    magnitude_floor,
};

/// ロバスト逸脱で検出する。
pub fn detect(
    series: &PreparedSeries,
    entry: &'static CatalogEntry,
    baseline: &Baseline,
    opts: &DetectOptions,
) -> (Vec<Detection>, RouteStatus) {
    if entry.deviation.is_none() {
        return (
            Vec::new(),
            RouteStatus::NotEvaluated {
                reason: NotEvaluated::NoDeviationInterestDeclared,
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

    // **散らばりが測れないときは何も出さない。** ε で割らない。
    let Some((center, mad)) = baseline.usable_mad() else {
        let reason = match baseline.evidence.dispersion {
            Dispersion::NotMeasurable => NotEvaluated::DispersionNotMeasurable,
            Dispersion::TooSparse { .. } => NotEvaluated::DispersionTooSparse,
            _ => NotEvaluated::TooFewBaselineSamples,
        };
        return (Vec::new(), RouteStatus::NotEvaluated { reason });
    };

    let ratio_threshold = opts.thresholds.deviation_ratio;
    // 絶対差の下限。宣言が無ければ 0 (比だけで判定する)
    let floor = magnitude_floor(entry.shift, center).unwrap_or(0.0);

    let deviates = |o: &Observation| {
        let diff = o.value - center;
        let abs = diff.abs();
        entry.deviation.accepts(ShiftDirection::of(diff))
            && abs / mad >= ratio_threshold
            && abs >= floor
    };

    let mut out = Vec::new();
    for segment in series.iter_segments() {
        for (a, b) in group_runs(segment, deviates) {
            let hit = &segment[a..b];
            // 逸脱の向きは区間の中で最も外れた点で決める
            let peak = hit
                .iter()
                .max_by(|x, y| {
                    (x.value - center)
                        .abs()
                        .partial_cmp(&(y.value - center).abs())
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .expect("非空");
            let diff = peak.value - center;
            let direction = ShiftDirection::of(diff);
            let basis = DecisionBasis::RobustDeviation {
                median: center,
                mad,
                peak_mad_ratio: diff.abs() / mad,
                ratio_threshold,
                min_absolute_deviation: floor,
                peak_absolute_deviation: diff.abs(),
                direction,
            };
            out.push(Detection {
                detector_version: DETECTOR_VERSION,
                series: SeriesKey::from_metric(&series.key),
                metric_label: entry.label,
                unit: series.unit,
                kind: series.kind,
                origin: series.origin,
                pattern: match direction {
                    ShiftDirection::Rise => Pattern::Spike,
                    ShiftDirection::Fall => Pattern::Dip,
                },
                support: series.support_of(hit),
                baseline: baseline.evidence.clone(),
                decision: DecisionEvidence::new(basis, hit),
                // 逸脱だけでは「このホストでは珍しい」までしか言えない。
                // 絶対水準の裏付けが無いので単独では Watch を超えない。
                base_priority: crate::analyze::assessment::Priority::Watch,
                possible_interpretations: entry.interpretations,
                not_established: entry.not_established,
            });
        }
    }

    let status = if out.is_empty() {
        RouteStatus::Evaluated
    } else {
        RouteStatus::Detected { count: out.len() }
    };
    (out, status)
}

#[cfg(test)]
mod tests {
    use super::super::testing::*;
    use super::super::*;
    use super::*;

    fn run_with(
        t: crate::analyze::timeline::MetricTimeline,
        opts: &DetectOptions,
    ) -> (Vec<Detection>, RouteStatus, Baseline) {
        let entry = entry_for(&t);
        let series = PreparedSeries::from_timeline(&t);
        let material = series.observations.clone();
        let baseline = build_baseline(&series, entry, material, opts);
        let (d, s) = super::detect(&series, entry, &baseline, opts);
        (d, s, baseline)
    }

    fn run(t: crate::analyze::timeline::MetricTimeline) -> (Vec<Detection>, RouteStatus, Baseline) {
        run_with(t, &DetectOptions::default())
    }

    /// **`MAD == 0` の系列で巨大スコアが出ない。**
    ///
    /// ε で割る実装では、`50.0` が 1 点だけ `50.01` になったときに
    /// スコアが 1e9 規模になり最重大の検出として報告される。
    #[test]
    fn a_constant_series_yields_no_deviation_score() {
        let mut v = vec![50.0; 40];
        // 1 ビットぶんの揺れ
        v[20] = 50.01;
        let (found, status, baseline) = run(runq(&vals(&v)));

        assert_eq!(baseline.evidence.mad, Some(0.0));
        assert_eq!(baseline.evidence.dispersion, Dispersion::NotMeasurable);
        assert!(
            found.is_empty(),
            "MAD が 0 の系列から逸脱を出してはいけない"
        );
        assert!(matches!(
            status,
            RouteStatus::NotEvaluated {
                reason: NotEvaluated::DispersionNotMeasurable
            }
        ));
        assert!(
            baseline
                .evidence
                .caveats
                .iter()
                .any(|c| c.contains("MAD が 0")),
            "測れなかったことを出力に残す"
        );
    }

    /// 完全に一定の系列でも同じ (0 除算が起きない)。
    #[test]
    fn a_perfectly_flat_series_is_safe() {
        let (found, _, baseline) = run(runq(&vals(&[3.0; 30])));
        assert!(found.is_empty());
        assert_eq!(baseline.evidence.dispersion, Dispersion::NotMeasurable);
    }

    /// 中央値と同じ値が大半を占める系列も降りる。
    ///
    /// `MAD` は偏差の中央値なので、同値が過半数なら厳密に 0 になる。
    /// 半数前後では小さな正値になり得るため、別の状態として受ける。
    #[test]
    fn a_series_dominated_by_one_value_declines() {
        // 21 点が 0、19 点が散らばる → MAD は 0 になる (同値が過半数)
        let mut v = vec![0.0; 21];
        v.extend((1..=19).map(|i| f64::from(i) * 10.0));
        let (found, status, baseline) = run(runq(&vals(&v)));
        assert!(found.is_empty());
        assert!(
            matches!(
                status,
                RouteStatus::NotEvaluated {
                    reason: NotEvaluated::DispersionNotMeasurable
                        | NotEvaluated::DispersionTooSparse
                }
            ),
            "{status:?}"
        );
        assert!(!baseline.evidence.dispersion.is_measured());
    }

    /// 散らばりがある系列では逸脱を拾う。
    #[test]
    fn a_clear_outlier_is_detected() {
        // 2〜6 の間で揺れる系列に 200 が 1 点
        let mut v: Vec<f64> = (0..40).map(|i| 2.0 + f64::from(i % 5)).collect();
        v[20] = 200.0;
        let (found, status, baseline) = run(runq(&vals(&v)));
        assert!(baseline.evidence.dispersion.is_measured());
        assert_eq!(found.len(), 1, "{found:#?}");
        assert_eq!(found[0].pattern, Pattern::Spike);
        assert_eq!(found[0].route(), DetectRoute::RobustDeviation);
        assert!(matches!(status, RouteStatus::Detected { count: 1 }));
        let DecisionBasis::RobustDeviation {
            peak_mad_ratio,
            ratio_threshold,
            ..
        } = found[0].decision.basis
        else {
            panic!("逸脱の根拠");
        };
        assert!(peak_mad_ratio >= ratio_threshold);
        assert!(
            peak_mad_ratio.is_finite(),
            "スコアが無限大になってはいけない"
        );
    }

    /// 宣言した向きだけを見る (`%idle` は下方向のみ)。
    #[test]
    fn only_the_declared_direction_is_reported() {
        // idle は DeviationInterest::Lower。上振れは拾わない
        let mut v: Vec<f64> = (0..40).map(|i| 40.0 + f64::from(i % 5)).collect();
        v[20] = 99.0;
        let (found, _, _) = run(cpu_idle(&vals(&v)));
        assert!(found.is_empty(), "上振れは %idle では異変ではない");

        let mut v2: Vec<f64> = (0..40).map(|i| 40.0 + f64::from(i % 5)).collect();
        v2[20] = 0.5;
        let (found2, _, _) = run(cpu_idle(&vals(&v2)));
        assert_eq!(found2.len(), 1);
        assert_eq!(found2[0].pattern, Pattern::Dip);
    }

    /// サンプルが少なければ基準を作らない。
    #[test]
    fn too_few_samples_decline_the_route() {
        let (found, status, _) = run(runq(&vals(&[1.0, 9.0, 1.0, 1.0, 1.0])));
        assert!(found.is_empty());
        assert!(matches!(
            status,
            RouteStatus::NotEvaluated {
                reason: NotEvaluated::TooFewBaselineSamples
            }
        ));
    }

    /// 絶対差の下限に届かない「比だけ大きい」逸脱は出さない。
    #[test]
    fn a_large_ratio_with_a_tiny_absolute_difference_is_rejected() {
        // 中央値 48、MAD 0.01 相当の微小な揺れに 48.5 が 1 点。
        // 比は 50 倍でも %idle の最小有意変化量 (20 ポイント) に届かない。
        let mut v: Vec<f64> = (0..40).map(|i| 48.0 + f64::from(i % 2) * 0.02).collect();
        v[20] = 30.0;
        let (found, _, baseline) = run(cpu_idle(&vals(&v)));
        assert!(baseline.evidence.dispersion.is_measured());
        assert!(
            found.is_empty(),
            "18 ポイントの差は %idle の最小有意変化量 (20) に届かない"
        );
    }

    /// 逸脱が入力の大半を占めると、基準がそちらへ寄ることが型に出る。
    #[test]
    fn the_basis_admits_when_the_anomaly_dominates_the_input() {
        // 40 点のうち 34 点が %idle 2% (= 固定条件を満たす)
        let mut v = vec![2.0; 34];
        v.extend(vec![80.0; 6]);
        let (_, _, baseline) = run(cpu_idle(&vals(&v)));
        assert!(baseline.evidence.median_within_fixed_condition);
        assert!(baseline.evidence.flagged_share > 0.8);
        assert!(baseline.evidence.may_reflect_the_anomaly());
        assert!(
            baseline
                .evidence
                .caveats
                .iter()
                .any(|c| c.contains("基準が異常側へ寄っている"))
        );
    }
}

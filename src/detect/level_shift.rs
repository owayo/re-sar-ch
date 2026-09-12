//! 時間的変化 — 前後の窓の水準差による検出。
//!
//! 「ある時刻から高止まりした」という変化は、逸脱検出では拾えない。
//! 変化後の値が長時間を占めていれば、それが比較基準そのものになるためである。
//! この経路は**時刻を挟んだ前後の窓を比べる**ので、基準が変化後へ寄っていても
//! 変化が起きた時刻を指せる。
//!
//! # 窓はサンプル数ではなく時間幅で決める
//!
//! 10 秒採取と 10 分採取で同じサンプル数の窓を使うと、前者は 50 秒、
//! 後者は 50 分を比べてしまう。窓幅は
//! [`crate::detect::DetectThresholds::shift_window_secs`] (既定 30 分) を
//! 採取間隔で割って決め、サンプル数の下限
//! ([`crate::detect::DetectThresholds::shift_window_min_samples`]) で底を打つ。
//! 窓が小さいと単発のスパイクが窓の中央値を動かしてしまうため。
//!
//! # 判定は 2 条件 + 持続性
//!
//! | # | 条件 | 理由 |
//! |---|---|---|
//! | a | 絶対差が指標ごとの最小有意変化量以上 | 単位を持つ差でないと読み手が判断できない |
//! | b | 散らばりが測れたときのみ、正規化差も閾値以上 | 揺れの大きい系列で小さな段差を拾わない |
//! | c | 後窓の 7 割以上が同方向へ最小変化量以上動いている | 単発の値が窓の中央値を押し出すのを防ぐ |
//!
//! **b を無条件の AND にしない。** 一定値から一定値へ動いた明瞭な段差では
//! 散らばりが 0 になるので、b を必須にすると最も分かりやすい変化を落とす。
//! 散らばりが測れないときは b を課さず、「正規化できなかった」ことを記録する
//! (**ε で割らない**)。
//!
//! # 正規化の尺度は窓ごとに中心化してから束ねる
//!
//! 前後の窓を合わせたまま MAD を取ると、**尺度が段差自身で膨らむ**ので
//! 大きな段差ほど正規化差が小さくなる。各窓を自身の中央値で中心化した
//! 残差を束ねてから MAD を取る。
//!
//! # 不連続を跨がない
//!
//! 連続区間の内側でしか窓を取らない。RESTART を挟んだ前後を比べると
//! 「再起動で水準が変わった」を異変として報告してしまう。

use crate::analyze::assessment::{NotEvaluated, RouteStatus};
use crate::analyze::metric_catalog::CatalogEntry;

use super::{
    Baseline, DETECTOR_VERSION, DecisionBasis, DecisionEvidence, DetectOptions, Detection,
    MAD_SCALE, Observation, Pattern, PreparedSeries, SeriesKey, ShiftDirection, mad,
    magnitude_floor, median,
};

/// 1 つの分割候補の評価結果。
#[derive(Debug, Clone, Copy)]
struct Split {
    /// 後窓の開始位置 (連続区間内の索引)。
    at: usize,
    before_median: f64,
    after_median: f64,
    shift: f64,
    min_shift: f64,
    pooled_mad: Option<f64>,
    normalized: Option<f64>,
    persistence: f64,
    /// 順位付けに使う大きさ。正規化できたならそれを、できなければ
    /// 最小変化量で割った絶対差を使う (単位の違う系列を混ぜないため)。
    rank: f64,
}

/// 水準変化で検出する。
pub fn detect(
    series: &PreparedSeries,
    entry: &'static CatalogEntry,
    baseline: &Baseline,
    opts: &DetectOptions,
) -> (Vec<Detection>, RouteStatus) {
    if matches!(
        entry.shift,
        crate::analyze::metric_catalog::ShiftMagnitude::NotEvaluated
    ) {
        return (
            Vec::new(),
            RouteStatus::NotEvaluated {
                reason: NotEvaluated::LevelShiftNotDeclared,
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

    let th = &opts.thresholds;
    let mut out = Vec::new();
    let mut any_window = false;

    for segment in series.iter_segments() {
        let Some(w) = window_size(segment, th.shift_window_secs, th.shift_window_min_samples)
        else {
            continue;
        };
        if segment.len() < 2 * w {
            continue;
        }
        any_window = true;

        // 分割候補を順に評価する
        let mut candidates: Vec<Split> = Vec::new();
        for at in w..=segment.len() - w {
            let before = &segment[at - w..at];
            let after = &segment[at..at + w];
            let Some(mut split) = evaluate(before, after, entry) else {
                continue;
            };
            split.at = at;
            let direction = ShiftDirection::of(split.shift);
            if !accepts(entry, direction) {
                continue;
            }
            if split.shift.abs() < split.min_shift {
                continue;
            }
            if split.persistence < th.shift_persistence_share {
                continue;
            }
            // 散らばりが測れたときだけ正規化差を課す (**ε で割らない**)
            if let Some(n) = split.normalized
                && n < th.shift_normalized
            {
                continue;
            }
            candidates.push(split);
        }

        // 隣接する候補は 1 件へ畳む。**同じ段差を窓ごとに何度も報告しない。**
        for best in collapse(&candidates) {
            let after = &segment[best.at..best.at + w];
            let before = series.support_of(&segment[best.at - w..best.at]);
            out.push(build(series, entry, baseline, best, before, after));
        }
    }

    // 閾値は根拠にそのまま載せる (読み手が判定を再現できるように)
    stamp_thresholds(&mut out, opts);

    if out.is_empty() {
        if !any_window {
            // 前後窓を取れる連続区間が無い。**「検出なし」ではない。**
            return (
                Vec::new(),
                RouteStatus::NotEvaluated {
                    reason: NotEvaluated::NoWindowLongEnough,
                },
            );
        }
        return (out, RouteStatus::Evaluated);
    }
    let count = out.len();
    (out, RouteStatus::Detected { count })
}

/// 窓のサンプル数。
///
/// 採取間隔が取れない (区間長が全て 0) 場合は評価しない。
fn window_size(segment: &[Observation], window_secs: u64, min_samples: usize) -> Option<usize> {
    let interval = representative_interval(segment)?;
    let by_time = window_secs.div_ceil(interval.max(1)) as usize;
    Some(by_time.max(min_samples))
}

/// 連続区間の採取間隔の代表値 (秒)。
fn representative_interval(segment: &[Observation]) -> Option<u64> {
    let mut gaps: Vec<u64> = segment
        .iter()
        .map(Observation::interval_secs)
        .filter(|g| *g > 0)
        .collect();
    if gaps.is_empty() {
        return None;
    }
    gaps.sort_unstable();
    Some(gaps[gaps.len() / 2])
}

/// カタログの関心方向に合うか。
///
/// 方向の宣言が無い系列は両方向を見る。
fn accepts(entry: &CatalogEntry, direction: ShiftDirection) -> bool {
    entry.deviation.is_none() || entry.deviation.accepts(direction)
}

fn evaluate(before: &[Observation], after: &[Observation], entry: &CatalogEntry) -> Option<Split> {
    let bv: Vec<f64> = before.iter().map(|o| o.value).collect();
    let av: Vec<f64> = after.iter().map(|o| o.value).collect();
    let before_median = median(&bv)?;
    let after_median = median(&av)?;
    // 相対で宣言された指標は、**入力全体ではなく前窓の水準**を基準にする。
    // 局所的な水準に対する変化として読めるようにするため。
    let min_shift = magnitude_floor(entry.shift, before_median)?;
    let shift = after_median - before_median;

    // 各窓を自身の中央値で中心化した残差を束ねる。
    // 段差を含めたまま束ねると尺度が段差自身で膨らむ。
    let mut residuals: Vec<f64> = bv.iter().map(|v| v - before_median).collect();
    residuals.extend(av.iter().map(|v| v - after_median));
    let pooled = mad(&residuals, 0.0).filter(|m| *m > 0.0);
    let normalized = pooled.map(|m| shift.abs() / (MAD_SCALE * m));

    // 持続性: 後窓のうち前窓の水準から同方向へ最小変化量以上離れた割合
    let direction = ShiftDirection::of(shift);
    let moved = av
        .iter()
        .filter(|v| {
            let d = *v - before_median;
            ShiftDirection::of(d) == direction && d.abs() >= min_shift
        })
        .count();
    let persistence = moved as f64 / av.len() as f64;

    let rank = match normalized {
        Some(n) => n,
        // 正規化できない場合は単位を消すため最小変化量で割る
        None if min_shift > 0.0 => shift.abs() / min_shift,
        None => shift.abs(),
    };

    Some(Split {
        at: 0,
        before_median,
        after_median,
        shift,
        min_shift,
        pooled_mad: pooled,
        normalized,
        persistence,
        rank,
    })
}

/// 分割候補の優劣。
///
/// 大きさが同じなら**持続性が高い方**を採る。一定値から一定値への段差では
/// 大きさ (正規化できないので絶対差 ÷ 最小変化量) が窓をずらしても
/// 同じ値になり、そのままでは段差にまたがった窓が先に採られてしまう。
/// 持続性は段差にぴったり合った窓で最大になるので、境界が正しく決まる。
fn rank_key(s: &Split) -> (f64, f64) {
    (s.rank, s.persistence)
}

/// 隣接する分割候補を 1 件へ畳む。
///
/// 同じ段差は窓をずらすたびに条件を満たすので、連続する候補の中から
/// 最も良いものだけを残す。**完全に同じなら早い時刻を残す** (決定的にする)。
fn collapse(candidates: &[Split]) -> Vec<Split> {
    let mut out: Vec<Split> = Vec::new();
    let mut best: Option<Split> = None;
    let mut prev_at: Option<usize> = None;
    for c in candidates {
        let adjacent = prev_at.is_some_and(|p| c.at == p + 1);
        if !adjacent && let Some(b) = best.take() {
            out.push(b);
        }
        best = match best {
            Some(b) if rank_key(&b) >= rank_key(c) => Some(b),
            _ => Some(*c),
        };
        prev_at = Some(c.at);
    }
    if let Some(b) = best {
        out.push(b);
    }
    out
}

fn build(
    series: &PreparedSeries,
    entry: &'static CatalogEntry,
    baseline: &Baseline,
    split: Split,
    before: super::TemporalSupport,
    after: &[Observation],
) -> Detection {
    let after_support = series.support_of(after);
    let direction = ShiftDirection::of(split.shift);
    let basis = DecisionBasis::LevelShift {
        before_median: split.before_median,
        after_median: split.after_median,
        shift: split.shift,
        min_shift: split.min_shift,
        pooled_mad: split.pooled_mad,
        normalized_shift: split.normalized,
        normalized_threshold: 0.0,
        persistence_share: split.persistence,
        persistence_threshold: 0.0,
        before,
        after: after_support,
    };
    Detection {
        detector_version: DETECTOR_VERSION,
        series: SeriesKey::from_metric(&series.key),
        metric_label: entry.label,
        unit: series.unit,
        kind: series.kind,
        origin: series.origin,
        pattern: Pattern::LevelShift { direction },
        support: after_support,
        baseline: baseline.evidence.clone(),
        decision: DecisionEvidence::new(basis, after),
        // 水準変化は「いつ変わったか」を足すが、絶対水準の裏付けは無い
        base_priority: crate::analyze::assessment::Priority::Watch,
        possible_interpretations: entry.interpretations,
        not_established: entry.not_established,
    }
}

/// 出力に載せる閾値を設定から埋める。
///
/// [`build`] は閾値を知らないので、組み立て後に差し込む。
fn stamp_thresholds(detections: &mut [Detection], opts: &DetectOptions) {
    for d in detections {
        if let DecisionBasis::LevelShift {
            normalized_threshold,
            persistence_threshold,
            ..
        } = &mut d.decision.basis
        {
            *normalized_threshold = opts.thresholds.shift_normalized;
            *persistence_threshold = opts.thresholds.shift_persistence_share;
        }
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

    /// 一定値から一定値への明瞭な段差を拾う。
    ///
    /// 散らばりが 0 なので正規化はできない。**そこで落とさない**ことが要点。
    #[test]
    fn a_clean_step_is_detected_without_normalization() {
        let mut v = vec![90.0; 15];
        v.extend(vec![30.0; 15]);
        let (found, status) = run(cpu_idle(&vals(&v)));
        assert_eq!(found.len(), 1, "{found:#?}");
        let d = &found[0];
        assert_eq!(
            d.pattern,
            Pattern::LevelShift {
                direction: ShiftDirection::Fall
            }
        );
        assert!(matches!(status, RouteStatus::Detected { count: 1 }));
        let DecisionBasis::LevelShift {
            shift,
            normalized_shift,
            pooled_mad,
            persistence_share,
            ..
        } = d.decision.basis
        else {
            panic!("水準変化の根拠");
        };
        assert!((shift + 60.0).abs() < 1e-9);
        assert_eq!(pooled_mad, None, "一定値なので散らばりは測れない");
        assert_eq!(
            normalized_shift, None,
            "散らばりが測れないときは ε で割らず None"
        );
        assert!((persistence_share - 1.0).abs() < 1e-9);
    }

    /// 段差の時刻を指せる (変化後の窓が裏付けになる)。
    #[test]
    fn the_detection_points_at_the_time_of_the_change() {
        let mut v = vec![90.0; 12];
        v.extend(vec![20.0; 12]);
        let (found, _) = run(cpu_idle(&vals(&v)));
        assert_eq!(found.len(), 1);
        // 13 点目 (索引 12) から変化した
        assert_eq!(found[0].support.start_ust, T0 + 12 * STEP_SECS);
    }

    /// 最小有意変化量に届かない小さな段差は出さない。
    #[test]
    fn a_small_step_below_the_declared_magnitude_is_rejected() {
        // %idle の最小有意変化量は 20 ポイント
        let mut v = vec![90.0; 15];
        v.extend(vec![80.0; 15]);
        let (found, _) = run(cpu_idle(&vals(&v)));
        assert!(found.is_empty(), "10 ポイントの段差は報告しない");
    }

    /// 揺れの大きい系列では正規化差の条件が効く。
    ///
    /// 窓幅 (5 点) と同じ周期の揺れにするのは、窓の中央値が揺れに
    /// 引きずられないようにするため。周期 2 の矩形波では窓の中央値自体が
    /// 交互に動いてしまい、手法の限界を試すだけのテストになる。
    #[test]
    fn a_step_buried_in_noise_is_rejected() {
        // 中央値 50 の周りを ±40 揺れる系列に 25 の段差を足す
        const NOISE: [f64; 5] = [10.0, 90.0, 50.0, 30.0, 70.0];
        let mut v: Vec<f64> = (0..20).map(|i| NOISE[i % 5]).collect();
        v.extend((0..20).map(|i| NOISE[i % 5] + 25.0));
        let (found, _) = run(runq(&vals(&v)));
        assert!(
            found.is_empty(),
            "揺れに埋もれた段差は水準変化として報告しない: {found:#?}"
        );
    }

    /// 同じ揺れでも段差が十分大きければ拾う (上の裏返し)。
    #[test]
    fn a_step_larger_than_the_noise_is_detected() {
        const NOISE: [f64; 5] = [10.0, 90.0, 50.0, 30.0, 70.0];
        let mut v: Vec<f64> = (0..20).map(|i| NOISE[i % 5]).collect();
        v.extend((0..20).map(|i| NOISE[i % 5] + 300.0));
        let (found, _) = run(runq(&vals(&v)));
        assert_eq!(found.len(), 1, "{found:#?}");
        let DecisionBasis::LevelShift {
            normalized_shift, ..
        } = found[0].decision.basis
        else {
            panic!();
        };
        assert!(normalized_shift.is_some_and(|n| n >= 3.0));
    }

    /// 単発のスパイクは水準変化にしない (持続性の条件)。
    #[test]
    fn a_single_spike_is_not_a_level_shift() {
        let mut v = vec![5.0; 40];
        v[20] = 500.0;
        let (found, _) = run(runq(&vals(&v)));
        assert!(found.is_empty(), "1 点だけの跳ねは水準変化ではない");
    }

    /// 不連続 (RESTART) を挟んだ前後は比べない。
    #[test]
    fn a_step_across_a_restart_is_not_compared() {
        let mut points = vals(&[90.0; 15]);
        points.push(P::Restart);
        points.extend(vals(&[20.0; 15]));
        let (found, status) = run(cpu_idle(&points));
        assert!(
            found.is_empty(),
            "再起動を挟んだ水準差を異変として報告してはいけない: {found:#?}"
        );
        // 連続区間が 15 点しかなく 2 窓 (各 5 点以上) を取れない場合もある
        assert!(matches!(
            status,
            RouteStatus::Evaluated | RouteStatus::NotEvaluated { .. }
        ));
    }

    /// 宣言した向きだけを見る。
    #[test]
    fn only_the_declared_direction_is_reported() {
        // idle は下方向のみ。回復 (上昇) は報告しない
        let mut v = vec![20.0; 15];
        v.extend(vec![90.0; 15]);
        let (found, _) = run(cpu_idle(&vals(&v)));
        assert!(found.is_empty(), "%idle の回復は異変ではない");
    }

    /// 窓が取れない短い系列は評価しない。
    #[test]
    fn a_short_series_declines_the_route() {
        let (found, status) = run(cpu_idle(&vals(&[90.0, 90.0, 20.0, 20.0])));
        assert!(found.is_empty());
        assert!(matches!(
            status,
            RouteStatus::NotEvaluated {
                reason: NotEvaluated::NoWindowLongEnough
            }
        ));
    }

    /// 窓幅は採取間隔から決まる (サンプル数固定ではない)。
    #[test]
    fn the_window_is_sized_by_time_not_by_sample_count() {
        let obs: Vec<Observation> = (0..100)
            .map(|i| Observation {
                start_ust: T0 + i * 10,
                end_ust: T0 + (i + 1) * 10,
                elapsed_cs: 1000,
                value: 1.0,
                origin: ObservationOrigin::InstantGauge,
            })
            .collect();
        // 10 秒採取で 30 分窓 → 180 点
        assert_eq!(window_size(&obs, 1800, 5), Some(180));

        let obs10m: Vec<Observation> = (0..20)
            .map(|i| Observation {
                start_ust: T0 + i * 600,
                end_ust: T0 + (i + 1) * 600,
                elapsed_cs: 60000,
                value: 1.0,
                origin: ObservationOrigin::InstantGauge,
            })
            .collect();
        // 10 分採取で 30 分窓 → 3 点だが下限 5 で底を打つ
        assert_eq!(window_size(&obs10m, 1800, 5), Some(5));
    }

    /// 同じ段差を窓ごとに何度も報告しない。
    #[test]
    fn adjacent_candidates_collapse_into_one() {
        let mut v = vec![90.0; 25];
        v.extend(vec![20.0; 25]);
        let (found, _) = run(cpu_idle(&vals(&v)));
        assert_eq!(found.len(), 1, "1 つの段差は 1 件: {found:#?}");
    }

    /// 閾値が出力に載る。
    #[test]
    fn thresholds_are_recorded_in_the_output() {
        let mut v = vec![90.0; 15];
        v.extend(vec![30.0; 15]);
        let (found, _) = run(cpu_idle(&vals(&v)));
        let DecisionBasis::LevelShift {
            normalized_threshold,
            persistence_threshold,
            ..
        } = found[0].decision.basis
        else {
            panic!();
        };
        assert_eq!(normalized_threshold, 3.0);
        assert!((persistence_threshold - 0.7).abs() < 1e-9);
    }
}

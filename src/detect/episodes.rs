//! 検出のまとまり (エピソード) 化。
//!
//! 「いつ・何に異変があったか」を答えるには、系列ごとにばらばらの検出ではなく
//! **時刻でまとめた塊**が要る。同じ時刻帯に CPU・ディスク・キューの検出が
//! 並んでいれば、読み手はそれを 1 つの出来事として読む。
//!
//! # 近接の判定は採取間隔から作る
//!
//! ギャップ許容量を固定秒 (例 10 分) にすると、10 秒採取のログでは
//! 無関係な事象が融合し、1 時間採取のログでは同じ出来事が分断される。
//! **採取間隔の代表値 × 係数**で決め、絶対上限で頭を押さえる
//! ([`crate::detect::DetectThresholds::episode_gap_factor`] /
//! [`crate::detect::DetectThresholds::episode_gap_cap_secs`])。
//!
//! 係数 2 の意味は「検出と検出の間に**欠測 1 個ぶん**を許す」である。
//! 代表値に中央値ではなく 90 パーセンタイルを使うのは、`sadc` の
//! 起動ずれによる数十秒のジッタを吸収するため
//! ([`crate::detect::PreparedSeries::interval_p90_secs`])。
//!
//! # 不連続を跨がない
//!
//! 「正常な観測 1 個を橋渡しすること」と「観測不能区間を橋渡しすること」は
//! 別の概念である。前者はギャップ許容量で扱うが、後者は許容量に関係なく切る。
//! RESTART を挟んだ前後の検出を 1 つのエピソードにすると、
//! 「再起動をまたいで続いた異変」という観測していない主張になる。

use serde::Serialize;

use super::{DetectRoute, Detection, SeriesKey, TemporalSupport};

/// 近接する検出のまとまり。
///
/// **解釈 (優先度) は持たない。** 優先度は
/// [`crate::analyze::assessment::AssessedEpisode`] が付ける。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Episode {
    /// 通し番号 (時刻順、0 起点)。
    pub index: usize,
    /// エピソード全体の時間範囲。
    ///
    /// **`samples` は検出をまたいだ合計ではなく最大値。**
    /// 同じ時刻の別系列を足すと採取回数が実際より多く見えるため
    /// ([`TemporalSupport::merge`])。「何回の採取で見えたか」は
    /// [`Episode::longest_detection`] か各検出の裏付けを見る。
    pub support: TemporalSupport,
    /// 含まれる検出。
    pub detections: Vec<Detection>,
    /// 関わった系列 (重複なし)。
    pub series: Vec<SeriesKey>,
    /// 当たった観点 (重複なし)。
    ///
    /// **「独立な裏付けが N 個」ではない。** 3 経路は相関する。
    pub viewpoints: Vec<DetectRoute>,
    /// 1 つの系列に当たった観点の最大数。
    pub max_viewpoints_on_one_series: usize,
    /// 最も長く続いた検出の裏付け (持続性の判断に使う)。
    pub longest_detection: TemporalSupport,
}

impl Episode {
    /// 代表となる検出 (優先度の下地が最も高く、次に裏付けが長いもの)。
    pub fn lead(&self) -> &Detection {
        self.detections
            .iter()
            .max_by(|a, b| {
                a.base_priority
                    .cmp(&b.base_priority)
                    .then_with(|| a.support.samples.cmp(&b.support.samples))
                    .then_with(|| a.support.span_secs().cmp(&b.support.span_secs()))
                    // 決定的にするため最後は系列名で決める
                    .then_with(|| b.series.cmp(&a.series))
            })
            .expect("エピソードは空でない")
    }

    /// 同じ系列に水準変化と他の観点が当たっているか。
    ///
    /// 「絶対水準が高い」と「参照分布から外れている」はほぼ言い換えだが、
    /// そこへ「**いつ変わったか**」が加わるのは情報が増えている。
    /// ただし独立な裏付けではない。
    pub fn has_change_point_for_a_flagged_series(&self) -> bool {
        self.series.iter().any(|s| {
            let routes: Vec<DetectRoute> = self
                .detections
                .iter()
                .filter(|d| &d.series == s)
                .map(Detection::route)
                .collect();
            routes.contains(&DetectRoute::LevelShift)
                && routes.iter().any(|r| *r != DetectRoute::LevelShift)
        })
    }
}

/// ギャップ許容量を決める。
///
/// 採取間隔が取れない場合は絶対上限を使う (それ以上融合させない)。
pub fn gap_secs(interval_p90_secs: Option<u64>, factor: u64, cap_secs: u64) -> u64 {
    match interval_p90_secs {
        Some(i) => (i.saturating_mul(factor)).min(cap_secs),
        None => cap_secs,
    }
}

/// 検出を時刻でまとめる。
///
/// `detections` は時刻昇順に並んでいること (`crate::detect::detect` が並べる)。
pub fn group(
    detections: Vec<Detection>,
    gap_secs: u64,
    discontinuity_marks: &[u64],
) -> Vec<Episode> {
    let mut episodes: Vec<Episode> = Vec::new();
    let mut current: Vec<Detection> = Vec::new();
    let mut current_end: u64 = 0;

    for d in detections {
        let start = d.support.start_ust;
        let joins = !current.is_empty()
            && start <= current_end.saturating_add(gap_secs)
            && !crosses_discontinuity(current_end, start, discontinuity_marks);
        if !joins && !current.is_empty() {
            episodes.push(finish(episodes.len(), std::mem::take(&mut current)));
            current_end = 0;
        }
        current_end = current_end.max(d.support.end_ust);
        current.push(d);
    }
    if !current.is_empty() {
        episodes.push(finish(episodes.len(), current));
    }
    episodes
}

/// 2 つの時刻の間に不連続があるか。
fn crosses_discontinuity(from_ust: u64, to_ust: u64, marks: &[u64]) -> bool {
    marks.iter().any(|m| *m > from_ust && *m <= to_ust)
}

fn finish(index: usize, detections: Vec<Detection>) -> Episode {
    let mut support = TemporalSupport::default();
    let mut series: Vec<SeriesKey> = Vec::new();
    let mut viewpoints: Vec<DetectRoute> = Vec::new();
    let mut longest = TemporalSupport::default();

    for d in &detections {
        support = support.merge(&d.support);
        if !series.contains(&d.series) {
            series.push(d.series.clone());
        }
        let route = d.route();
        if !viewpoints.contains(&route) {
            viewpoints.push(route);
        }
        // 「最も長く続いた」は採取回数を先に見る (時間範囲だけでは
        // 採取間隔の長いログが有利になる)
        if (d.support.samples, d.support.span_secs()) > (longest.samples, longest.span_secs()) {
            longest = d.support;
        }
    }
    series.sort();
    viewpoints.sort_unstable();

    let max_viewpoints = series
        .iter()
        .map(|s| {
            let mut rs: Vec<DetectRoute> = detections
                .iter()
                .filter(|d| &d.series == s)
                .map(Detection::route)
                .collect();
            rs.sort_unstable();
            rs.dedup();
            rs.len()
        })
        .max()
        .unwrap_or(0);

    Episode {
        index,
        support,
        detections,
        series,
        viewpoints,
        max_viewpoints_on_one_series: max_viewpoints,
        longest_detection: longest,
    }
}

#[cfg(test)]
mod tests {
    use super::super::testing::*;
    use super::super::*;
    use super::*;
    use crate::analyze::assessment::Priority;
    use crate::model::{ActivityId, Unit, ValueKind};

    fn detection(start: u64, end: u64, column: &str, route: DetectRoute) -> Detection {
        let basis = match route {
            DetectRoute::FixedCondition => DecisionBasis::FixedCondition {
                condition_id: "test",
                comparison: FixedComparison::AtLeast,
                threshold: 1.0,
                min_samples: 1,
                rationale: "テスト",
            },
            DetectRoute::RobustDeviation => DecisionBasis::RobustDeviation {
                median: 1.0,
                mad: 1.0,
                peak_mad_ratio: 9.0,
                ratio_threshold: 8.0,
                min_absolute_deviation: 0.0,
                peak_absolute_deviation: 9.0,
                direction: ShiftDirection::Rise,
            },
            DetectRoute::LevelShift => DecisionBasis::LevelShift {
                before_median: 1.0,
                after_median: 9.0,
                shift: 8.0,
                min_shift: 1.0,
                pooled_mad: None,
                normalized_shift: None,
                normalized_threshold: 3.0,
                persistence_share: 1.0,
                persistence_threshold: 0.7,
                before: TemporalSupport::default(),
                after: TemporalSupport::default(),
            },
        };
        let support = TemporalSupport {
            start_ust: start,
            end_ust: end,
            samples: 1,
            ..Default::default()
        };
        Detection {
            detector_version: DETECTOR_VERSION,
            series: SeriesKey::from_metric(&crate::analyze::timeline::MetricKey::new(
                ActivityId::CPU,
                "all",
                column,
            )),
            metric_label: "テスト指標",
            unit: Unit::Percent,
            kind: ValueKind::Counter,
            origin: ObservationOrigin::IntervalRate,
            pattern: Pattern::Sustained,
            support,
            baseline: BaselineEvidence {
                basis: BasisOrigin::InputItself,
                samples: 20,
                support: TemporalSupport::default(),
                median: Some(1.0),
                mad: Some(1.0),
                dispersion: Dispersion::Measured,
                median_within_fixed_condition: false,
                flagged_share: 0.0,
                caveats: Vec::new(),
            },
            decision: DecisionEvidence::new(basis, &[]),
            base_priority: Priority::Watch,
            possible_interpretations: &[],
            not_established: &[],
        }
    }

    #[test]
    fn gap_is_derived_from_the_sampling_interval() {
        // 10 分採取 → 20 分 (上限 30 分に収まる)
        assert_eq!(gap_secs(Some(600), 2, 1800), 1200);
        // 10 秒採取 → 20 秒
        assert_eq!(gap_secs(Some(10), 2, 1800), 20);
        // 1 時間採取 → 上限で頭を押さえる
        assert_eq!(gap_secs(Some(3600), 2, 1800), 1800);
        // 間隔が分からなければ上限
        assert_eq!(gap_secs(None, 2, 1800), 1800);
    }

    #[test]
    fn nearby_detections_join_one_episode() {
        let ds = vec![
            detection(T0, T0 + 600, "idle", DetectRoute::FixedCondition),
            detection(T0 + 600, T0 + 1200, "iowait", DetectRoute::FixedCondition),
        ];
        let eps = group(ds, 1200, &[]);
        assert_eq!(eps.len(), 1);
        assert_eq!(eps[0].series.len(), 2);
        assert_eq!(eps[0].support.start_ust, T0);
        assert_eq!(eps[0].support.end_ust, T0 + 1200);
    }

    #[test]
    fn distant_detections_form_separate_episodes() {
        let ds = vec![
            detection(T0, T0 + 600, "idle", DetectRoute::FixedCondition),
            detection(
                T0 + 20_000,
                T0 + 20_600,
                "idle",
                DetectRoute::FixedCondition,
            ),
        ];
        let eps = group(ds, 1200, &[]);
        assert_eq!(eps.len(), 2);
        assert_eq!(eps[1].index, 1);
    }

    /// 不連続を挟んだ検出は、時間的に近くても別のエピソードにする。
    ///
    /// 「正常な観測 1 個を橋渡しすること」と「観測不能区間を橋渡しすること」は
    /// 別の概念である。後者はギャップ許容量に関係なく切る。
    #[test]
    fn a_discontinuity_splits_episodes_regardless_of_the_gap() {
        let ds = vec![
            detection(T0, T0 + 600, "idle", DetectRoute::FixedCondition),
            // RESTART の区間 [T0+600, T0+1200] を挟んで再開
            detection(T0 + 1200, T0 + 1800, "idle", DetectRoute::FixedCondition),
        ];
        // ギャップ許容量 (1200 秒) の内側だが、不連続を跨ぐので繋がない
        let eps = group(ds, 1200, &[T0 + 1200]);
        assert_eq!(eps.len(), 2, "再起動をまたいで続いた異変にしてはいけない");

        // 不連続が無ければ同じ間隔で繋がる
        let ds2 = vec![
            detection(T0, T0 + 600, "idle", DetectRoute::FixedCondition),
            detection(T0 + 1200, T0 + 1800, "idle", DetectRoute::FixedCondition),
        ];
        assert_eq!(group(ds2, 1200, &[]).len(), 1);
    }

    #[test]
    fn viewpoints_are_counted_per_series() {
        let ds = vec![
            detection(T0, T0 + 600, "idle", DetectRoute::FixedCondition),
            detection(T0, T0 + 600, "idle", DetectRoute::LevelShift),
            detection(T0, T0 + 600, "iowait", DetectRoute::RobustDeviation),
        ];
        let eps = group(ds, 1200, &[]);
        assert_eq!(eps.len(), 1);
        assert_eq!(eps[0].viewpoints.len(), 3);
        assert_eq!(eps[0].max_viewpoints_on_one_series, 2);
        assert!(eps[0].has_change_point_for_a_flagged_series());
    }

    #[test]
    fn a_series_with_only_a_level_shift_is_not_a_combined_viewpoint() {
        let ds = vec![detection(T0, T0 + 600, "idle", DetectRoute::LevelShift)];
        let eps = group(ds, 1200, &[]);
        assert!(!eps[0].has_change_point_for_a_flagged_series());
    }

    #[test]
    fn empty_input_yields_no_episode() {
        assert!(group(Vec::new(), 1200, &[]).is_empty());
    }
}

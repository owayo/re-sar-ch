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
//! 係数 2 の意味は「検出の始まりと次の始まりの間に**欠測 1 個ぶん**を許す」である。
//! 代表値に中央値ではなく 90 パーセンタイルを使うのは、`sadc` の
//! 起動ずれによる数十秒のジッタを吸収するため
//! ([`crate::detect::PreparedSeries::interval_p90_secs`])。
//!
//! # 結合の軸は検出の「始まり」
//!
//! この機能が答えるのは「**いつ**・何に異変があったか」である。したがって
//! エピソードは**異変が始まった時刻のまとまり**として作る。
//!
//! 終了時刻の最大値でエピソードを伸ばしてはいけない。スワップ使用率のように
//! 一日中条件を満たす検出が 1 件あると、その 1 件が入力全体を覆い、
//! 午前の CPU 異変と夜の通信エラーまで同じエピソードへ融合する。
//! ギャップ許容量は**重なっている検出**には効かない (`start < current_end` なら
//! どんな許容量でも結合が成立する) ので、上限を足しても防げない。
//!
//! 始まりで束ねれば、長く続く検出は「それが始まった時刻」のエピソードに属し、
//! その後に始まった別の事象は自分の時刻のエピソードになる。
//! 長い検出の時間範囲そのものは [`Episode::support`] に残る。
//!
//! **各経路の `support.start_ust` は所見の始まりを指している。**
//! 固定条件と逸脱は「条件を満たした連続区間の先頭」、水準変化は
//! 「水準が違う 2 つの時間帯の境目 (後窓の先頭)」であり、
//! どれも前窓や基準の材料の先頭ではない。
//! これが成り立たない経路を足すときは、束ねる時刻を別に持たせること。
//!
//! この規則の副作用として、40 分続いた低下とその復帰は
//! **2 つのエピソード**になる (低下の始まりと復帰の始まりがギャップより離れるため)。
//! どちらも観測された時刻として正しく、「いつ何が起きたか」としては
//! そのほうが正確である。復帰を同じ箱へ戻すために結合範囲を広げると、
//! その間の無関係な検出を巻き込む問題が戻る。
//!
//! エピソードの範囲は互いに重なる (入れ子になる) ことがある。
//! **範囲の包含は同一事象を意味しない。**
//!
//! 始まりの広がりにも上限を置く ([`GroupRules::onset_span_cap_secs`])。
//! 採取間隔ごとに次の異変が始まる入力では、始まりの連鎖だけでも
//! 入力全体へ広がってしまうためである。
//!
//! **系列や変化の向きの一致は結合条件に入れない。** 1 つの事象は系列間で
//! 逆向きの変化を生む (`%idle` は下がり `runq-sz` は上がる) ので、
//! 向きの一致を要求すると 1 つの事象が割れる。
//!
//! # 恒常的な状態は背景の所見として分ける
//!
//! 入力のほぼ全体を占める検出は「いつ」の手がかりを持たない
//! (入力のどこを切っても成立する)。それを軸にエピソードを作ると
//! 時刻の情報が消えるので、[`split_standing`] で別枠へ出す。
//!
//! **水準変化は対象外。** 前後窓が入力の大半を占めても、水準変化の中身は
//! 「いつ変わったか」そのものなので、時間的な手がかりを持っている。
//!
//! # 不連続を跨がない
//!
//! 「正常な観測 1 個を橋渡しすること」と「観測不能区間を橋渡しすること」は
//! 別の概念である。前者はギャップ許容量で扱うが、後者は許容量に関係なく切る。
//! RESTART を挟んだ前後の検出を 1 つのエピソードにすると、
//! 「再起動をまたいで続いた異変」という観測していない主張になる。
//!
//! 検査はギャップ部分ではなく**結合後のエピソード全範囲**で行う。
//! ギャップ部分だけを見ると、A 系列の長い検出が B 系列の item 入れ替え時刻を
//! 覆っているとき、B の入れ替え前後の検出が同じエピソードへ入ってしまう
//! (重なっている検出では `start < covered_end` となり、ギャップ部分の検査を
//! 満たす mark が存在できない)。

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
    /// エピソード内で**最後に始まった**検出の開始時刻。
    ///
    /// エピソードは始まりの近接でまとめるので、
    /// **検出の始まりのまとまり**は `[support.start_ust, last_onset_ust]`、
    /// **根拠が及ぶ範囲**は [`Episode::support`] になる。2 つは別物であり、
    /// 後者は長く続く検出の終端まで伸びる。
    ///
    /// この 2 つを区別せずに support だけを出すと、
    /// 「その期間まるごとが 1 つの事象で、内側のエピソードはその一部」と
    /// 読まれてしまう。**範囲の包含は同一事象を意味しない。**
    pub last_onset_ust: u64,
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
    /// 最も長く続いた検出の裏付け。
    ///
    /// **持続性による優先度の昇格には使わない。** 昇格はその検出自身の
    /// 裏付けで決める ([`crate::analyze::assessment::AssessedDetection`])。
    /// ここは「このエピソードで何が最も長く続いたか」を併記するためにある。
    pub longest_detection: TemporalSupport,
}

impl Episode {
    /// 最も長く続いた検出。
    ///
    /// 採取回数を先に見る (時間範囲だけでは採取間隔の長いログが有利になる)。
    pub fn longest(&self) -> &Detection {
        self.detections
            .iter()
            .max_by(|a, b| {
                a.support
                    .samples
                    .cmp(&b.support.samples)
                    .then_with(|| a.support.span_secs().cmp(&b.support.span_secs()))
                    // 決定的にするため最後は系列名で決める
                    .then_with(|| b.series.cmp(&a.series))
            })
            .expect("エピソードは空でない")
    }

    /// 水準変化と他の観点が**同じ系列・重なった時刻**で当たった系列。
    ///
    /// 「絶対水準が高い」と「参照分布から外れている」はほぼ言い換えだが、
    /// そこへ「**いつ変わったか**」が加わるのは情報が増えている。
    ///
    /// **ただし独立な裏付けではない (規律 2′)。** 3 経路は相関するので、
    /// これを理由に優先度を上げてはいけない。示すだけにする。
    ///
    /// 同じ系列であることだけでは足りない。水準変化が朝で逸脱が夜なら
    /// 「変化した時刻が分かった」ことにならないので、
    /// **時間範囲が重なっていること**も確かめる。
    pub fn series_with_corroborating_viewpoints(&self) -> Vec<SeriesKey> {
        let mut out: Vec<SeriesKey> = Vec::new();
        for s in &self.series {
            let same_series = self.detections.iter().filter(|d| &d.series == s);
            let shifts: Vec<&Detection> = same_series
                .clone()
                .filter(|d| d.route() == DetectRoute::LevelShift)
                .collect();
            let others: Vec<&Detection> = same_series
                .filter(|d| d.route() != DetectRoute::LevelShift)
                .collect();
            let corroborated = shifts
                .iter()
                .any(|l| others.iter().any(|o| overlaps(&l.support, &o.support)));
            if corroborated {
                out.push(s.clone());
            }
        }
        out
    }
}

/// 2 つの裏付けの時間範囲が重なるか。
///
/// 端点の一致も重なりとみなす。水準変化の後窓はちょうど変化点から始まるので、
/// 変化点で立った別の観点の検出とは端点で接する。
fn overlaps(a: &TemporalSupport, b: &TemporalSupport) -> bool {
    a.start_ust <= b.end_ust && b.start_ust <= a.end_ust
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

/// 恒常的な状態とみなす、入力全体に対する時間範囲の割合 (百分率)。
///
/// 入力の 9 割を覆う検出は、入力のどこを切ってもほぼ成立する。
/// 「いつ」の手がかりが無いという意味で、局所的な異変とは別のものである。
pub const STANDING_SPAN_PERCENT: u64 = 90;

/// 「始まりの広がり」の上限を入力長から作るときの分母。
///
/// エピソードが入力の大半を覆うと「いつ」の手がかりが消えるので、
/// 入力長の 1/4 を上限にする。
const ONSET_SPAN_CAP_DIVISOR: u64 = 4;

/// 始まりの広がりの上限の下限 (ギャップ許容量の倍数)。
///
/// 短い入力で上限がギャップ許容量を下回ると、隣接する検出すら
/// 結合できなくなる。ギャップ許容量の 4 倍を下回らせない。
const ONSET_SPAN_CAP_FLOOR_FACTOR: u64 = 4;

/// エピソードの結合規則。
///
/// **どちらも出力に載せる** ([`crate::analyze::assessment::Assessment`])。
/// 読み手が結合の結果を再現できるようにするため。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GroupRules {
    /// 検出の始まりがこれだけ離れていても同じエピソードに入れる (秒)。
    pub gap_secs: u64,
    /// 1 つのエピソードに入る「始まり」の広がりの上限 (秒)。
    pub onset_span_cap_secs: u64,
}

impl GroupRules {
    /// 入力全体の時間範囲とギャップ許容量から作る。
    pub fn new(gap_secs: u64, input_span_secs: Option<u64>) -> Self {
        let floor = gap_secs.saturating_mul(ONSET_SPAN_CAP_FLOOR_FACTOR);
        let cap = match input_span_secs {
            Some(span) if span > 0 => (span / ONSET_SPAN_CAP_DIVISOR).max(floor),
            // 入力長が分からないときは始まりの広がりを抑えない
            // (分母が無いのに上限を決めると根拠のない分割になる)
            _ => u64::MAX,
        };
        Self {
            gap_secs,
            onset_span_cap_secs: cap,
        }
    }
}

/// 裏付けの時間範囲が入力全体に占める割合 (百分率、切り捨て)。
///
/// 入力全体の時間範囲が分からなければ `None`。
pub fn share_of_input_percent(
    support: &TemporalSupport,
    input_span_secs: Option<u64>,
) -> Option<u64> {
    match input_span_secs {
        Some(span) if span > 0 => Some(support.span_secs().saturating_mul(100) / span),
        _ => None,
    }
}

/// その検出が「入力のほぼ全体を占める」か。
///
/// **水準変化は常に `false`。** 前後窓が入力の大半を占めても、
/// 水準変化の中身は「いつ変わったか」なので時間的な手がかりを持っている。
///
/// 判定に**採取回数の割合ではなく時間範囲の割合**を使うのは、
/// 採取間隔が一定でない入力で回数割合が密に採取した時間帯へ偏るためである。
/// 「有効な観測の大半で条件を満たした」という別の問いには
/// [`crate::detect::BaselineEvidence::flagged_share`] が答える。
///
/// 時間範囲を分子に使えるのは、検出の裏付けが
/// **条件を満たした連続区間**だから (`crate::detect::group_runs` が
/// 時刻の繋がらない点で区切る)。「最初の当たりから最後の当たりまで」に
/// 正常区間や欠測を含む裏付けを作る経路を足すときは、この前提が崩れる。
pub fn is_standing(d: &Detection, input_span_secs: Option<u64>, percent: u64) -> bool {
    if d.route() == DetectRoute::LevelShift {
        return false;
    }
    share_of_input_percent(&d.support, input_span_secs).is_some_and(|s| s >= percent)
}

/// 恒常的な状態を局所的な異変から分ける。
///
/// 返り値は `(恒常的な状態, 局所的な異変)`。どちらも入力の並び順を保つ。
///
/// **入力全体の時間範囲が分からなければ何も分けない。** 「ほぼ全体」を
/// 判定する分母が無いのに背景へ回すと、局所的な異変を背景と呼んでしまう
/// (規律 7: 評価できなかったことを断定しない)。
pub fn split_standing(
    detections: Vec<Detection>,
    input_span_secs: Option<u64>,
    standing_span_percent: u64,
) -> (Vec<Detection>, Vec<Detection>) {
    let mut standing: Vec<Detection> = Vec::new();
    let mut local: Vec<Detection> = Vec::new();
    for d in detections {
        if is_standing(&d, input_span_secs, standing_span_percent) {
            standing.push(d);
        } else {
            local.push(d);
        }
    }
    (standing, local)
}

/// 検出を「始まった時刻」でまとめる。
///
/// `detections` は開始時刻の昇順に並んでいること (`crate::detect::detect` が並べる)。
///
/// 結合の条件は 3 つ。
///
/// 1. 直前に始まった検出からギャップ許容量の内側で始まった
/// 2. エピソード先頭の始まりから [`GroupRules::onset_span_cap_secs`] の内側で始まった
/// 3. **結合後のエピソード全範囲**に不連続が無い
pub fn group(
    detections: Vec<Detection>,
    rules: GroupRules,
    discontinuity_marks: &[u64],
) -> Vec<Episode> {
    let mut episodes: Vec<Episode> = Vec::new();
    let mut current: Vec<Detection> = Vec::new();
    // エピソード先頭の始まり / 直前の検出の始まり / 結合済みの終端
    let mut first_onset: u64 = 0;
    let mut last_onset: u64 = 0;
    let mut covered_end: u64 = 0;

    for d in detections {
        let onset = d.support.start_ust;
        let end = d.support.end_ust;
        let joins = !current.is_empty()
            && onset <= last_onset.saturating_add(rules.gap_secs)
            && onset.saturating_sub(first_onset) <= rules.onset_span_cap_secs
            && !crosses_discontinuity(first_onset, covered_end.max(end), discontinuity_marks);
        if !joins && !current.is_empty() {
            episodes.push(finish(episodes.len(), std::mem::take(&mut current)));
        }
        if current.is_empty() {
            first_onset = onset;
            covered_end = end;
        } else {
            covered_end = covered_end.max(end);
        }
        last_onset = onset;
        current.push(d);
    }
    if !current.is_empty() {
        episodes.push(finish(episodes.len(), current));
    }
    episodes
}

/// `[from_ust, to_ust]` の内側に不連続があるか。
///
/// 始点そのものは含めない (そこから観測が再開したのなら、
/// その不連続をエピソードが跨いだことにはならない)。
fn crosses_discontinuity(from_ust: u64, to_ust: u64, marks: &[u64]) -> bool {
    marks.iter().any(|m| *m > from_ust && *m <= to_ust)
}

fn finish(index: usize, detections: Vec<Detection>) -> Episode {
    let mut support = TemporalSupport::default();
    let mut series: Vec<SeriesKey> = Vec::new();
    let mut viewpoints: Vec<DetectRoute> = Vec::new();
    let mut last_onset: u64 = 0;

    for d in &detections {
        support = support.merge(&d.support);
        last_onset = last_onset.max(d.support.start_ust);
        if !series.contains(&d.series) {
            series.push(d.series.clone());
        }
        let route = d.route();
        if !viewpoints.contains(&route) {
            viewpoints.push(route);
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

    let mut episode = Episode {
        index,
        support,
        last_onset_ust: last_onset,
        detections,
        series,
        viewpoints,
        max_viewpoints_on_one_series: max_viewpoints,
        longest_detection: TemporalSupport::default(),
    };
    // 判定の重複を避けるため、選び方は `Episode::longest` の 1 箇所に置く
    episode.longest_detection = episode.longest().support;
    episode
}

#[cfg(test)]
mod tests {
    use super::super::testing::*;
    use super::super::*;
    use super::*;
    use crate::analyze::assessment::Priority;
    use crate::model::{ActivityId, Unit, ValueKind};

    fn detection(start: u64, end: u64, column: &str, route: DetectRoute) -> Detection {
        detection_of(start, end, 1, column, route)
    }

    fn detection_of(
        start: u64,
        end: u64,
        samples: u64,
        column: &str,
        route: DetectRoute,
    ) -> Detection {
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
                trend_explained_shift: 0.0,
                step_shift: 8.0,
                min_shift: 1.0,
                pooled_mad: None,
                normalized_shift: None,
                normalized_threshold: 3.0,
                persistence_share: 1.0,
                persistence_threshold: 0.7,
                window_samples: 5,
                window_requested_secs: 1800,
                window_secs: 3000,
                before: TemporalSupport::default(),
                after: TemporalSupport::default(),
            },
        };
        let support = TemporalSupport {
            start_ust: start,
            end_ust: end,
            samples: samples.max(1),
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

    /// 上限を課さない結合規則 (結合そのものを試すテスト用)。
    fn rules(gap: u64) -> GroupRules {
        GroupRules {
            gap_secs: gap,
            onset_span_cap_secs: u64::MAX,
        }
    }

    #[test]
    fn the_onset_span_cap_comes_from_the_input_length() {
        // 24 時間の入力 → 1/4 の 6 時間
        assert_eq!(
            GroupRules::new(1200, Some(86_400)).onset_span_cap_secs,
            21_600
        );
        // 短い入力ではギャップ許容量の 4 倍を下回らせない
        assert_eq!(GroupRules::new(1200, Some(3600)).onset_span_cap_secs, 4800);
        // 入力長が分からなければ抑えない (根拠のない分割をしない)
        assert_eq!(GroupRules::new(1200, None).onset_span_cap_secs, u64::MAX);
    }

    #[test]
    fn nearby_detections_join_one_episode() {
        let ds = vec![
            detection(T0, T0 + 600, "idle", DetectRoute::FixedCondition),
            detection(T0 + 600, T0 + 1200, "iowait", DetectRoute::FixedCondition),
        ];
        let eps = group(ds, rules(1200), &[]);
        assert_eq!(eps.len(), 1);
        assert_eq!(eps[0].series.len(), 2);
        assert_eq!(eps[0].support.start_ust, T0);
        assert_eq!(eps[0].support.end_ust, T0 + 1200);
        assert_eq!(eps[0].last_onset_ust, T0 + 600, "始まりの広がりも残す");
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
        let eps = group(ds, rules(1200), &[]);
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
        let eps = group(ds, rules(1200), &[T0 + 1200]);
        assert_eq!(eps.len(), 2, "再起動をまたいで続いた異変にしてはいけない");

        // 不連続が無ければ同じ間隔で繋がる
        let ds2 = vec![
            detection(T0, T0 + 600, "idle", DetectRoute::FixedCondition),
            detection(T0 + 1200, T0 + 1800, "idle", DetectRoute::FixedCondition),
        ];
        assert_eq!(group(ds2, rules(1200), &[]).len(), 1);
    }

    /// 重なる検出を経由して不連続を跨がない (Issue #5 ⑧)。
    ///
    /// A 系列の長い検出が B 系列の item 入れ替え時刻を覆っていると、
    /// ギャップ部分だけを検査する実装では B の入れ替え前後の検出が
    /// 同じエピソードへ入ってしまう (重なっているので `start < covered_end` になり、
    /// ギャップ部分に mark が存在できない)。
    #[test]
    fn an_overlapping_detection_cannot_bridge_a_discontinuity() {
        let mark = T0 + 2400;
        let ds = vec![
            // A 系列: 不連続時刻を覆う長い検出
            detection_of(T0, T0 + 4800, 8, "idle", DetectRoute::FixedCondition),
            // B 系列: 入れ替えの前
            detection(T0 + 600, T0 + 1200, "iowait", DetectRoute::FixedCondition),
            // B 系列: 入れ替えの後
            detection(mark, mark + 600, "iowait", DetectRoute::FixedCondition),
        ];
        let eps = group(ds, rules(1200), &[mark]);
        assert!(
            eps.iter().all(|e| !crosses_discontinuity(
                e.support.start_ust,
                e.support.end_ust,
                &[mark]
            ) || e.detections.len() == 1),
            "不連続を跨ぐエピソードは、それ自体が 1 件の検出である場合だけ許される"
        );
        let joined = eps
            .iter()
            .find(|e| e.detections.iter().any(|d| d.support.start_ust == mark))
            .expect("入れ替え後の検出を含むエピソード");
        assert!(
            joined
                .detections
                .iter()
                .all(|d| d.support.start_ust >= mark),
            "入れ替え前の検出と同じエピソードにしてはいけない"
        );
    }

    /// 一日続く条件を軸にしてエピソードを作らない (Issue #5 ⑨)。
    ///
    /// スワップ使用率が一日中 1% 超なら、その検出は入力全体を覆う。
    /// 終了時刻でエピソードを伸ばすと、午前の異変と夜の異変が 1 件に融合し
    /// 「いつ」の手がかりが消える。
    #[test]
    fn a_condition_that_holds_all_day_does_not_fuse_unrelated_detections() {
        let day = 86_400;
        let all_day = detection_of(
            T0,
            T0 + day,
            144,
            "swpused_pct",
            DetectRoute::FixedCondition,
        );
        let morning = detection(
            T0 + 10_800,
            T0 + 11_400,
            "idle",
            DetectRoute::FixedCondition,
        );
        let night = detection(
            T0 + 72_000,
            T0 + 72_600,
            "rxdrop",
            DetectRoute::FixedCondition,
        );

        // 恒常的な状態は背景の所見として分ける
        let (standing, local) = split_standing(
            vec![all_day, morning, night],
            Some(day),
            STANDING_SPAN_PERCENT,
        );
        assert_eq!(standing.len(), 1, "一日続く条件は背景へ");
        assert_eq!(standing[0].series.column, "swpused_pct");
        assert_eq!(local.len(), 2);

        // 残りは時刻が離れているので別のエピソードになる
        let eps = group(local, GroupRules::new(1200, Some(day)), &[]);
        assert_eq!(eps.len(), 2, "午前の異変と夜の異変を 1 件に融合しない");
        assert_eq!(eps[0].support.start_ust, T0 + 10_800);
        assert_eq!(eps[1].support.start_ust, T0 + 72_000);
    }

    /// 長く続く検出は「始まった時刻」のエピソードに属し、後続を吸い込まない。
    #[test]
    fn a_long_detection_does_not_absorb_later_onsets() {
        let long = detection_of(T0, T0 + 40_000, 66, "pgscand", DetectRoute::FixedCondition);
        let later = detection(
            T0 + 20_000,
            T0 + 20_600,
            "idle",
            DetectRoute::FixedCondition,
        );
        let eps = group(vec![long, later], GroupRules::new(1200, Some(86_400)), &[]);
        assert_eq!(eps.len(), 2, "終了時刻で伸ばすと 1 件に融合してしまう");
        // 長い検出の時間範囲そのものは残る
        assert_eq!(eps[0].support.end_ust, T0 + 40_000);
        assert_eq!(eps[1].support.start_ust, T0 + 20_000);
    }

    /// 始まりの連鎖だけでも入力全体へ広がらないよう上限を課す。
    #[test]
    fn the_onset_span_cap_stops_a_chain_of_onsets() {
        // 1200 秒ごとに次の異変が始まる 12 件
        let ds: Vec<Detection> = (0..12)
            .map(|i| {
                detection(
                    T0 + i * 1200,
                    T0 + i * 1200 + 600,
                    "idle",
                    DetectRoute::FixedCondition,
                )
            })
            .collect();
        // 上限なしなら 1 件に繋がる
        assert_eq!(group(ds.clone(), rules(1200), &[]).len(), 1);
        // 上限 (4800 秒) を課すと始まりの広がりで切れる
        let capped = GroupRules {
            gap_secs: 1200,
            onset_span_cap_secs: 4800,
        };
        let eps = group(ds, capped, &[]);
        assert!(eps.len() > 1);
        assert!(
            eps.iter()
                .all(|e| e.last_onset_ust - e.support.start_ust <= 4800)
        );
    }

    /// 水準変化は「入力のほぼ全体」でも背景に回さない。
    ///
    /// 前後窓が入力を覆っても、水準変化の中身は「いつ変わったか」であり
    /// 時間的な手がかりを持っている。
    #[test]
    fn a_level_shift_is_never_treated_as_background() {
        let span = 3600;
        let shift = detection_of(T0, T0 + span, 6, "idle", DetectRoute::LevelShift);
        assert!(!is_standing(&shift, Some(span), STANDING_SPAN_PERCENT));
        let fixed = detection_of(T0, T0 + span, 6, "idle", DetectRoute::FixedCondition);
        assert!(is_standing(&fixed, Some(span), STANDING_SPAN_PERCENT));
    }

    /// 入力全体の時間範囲が分からなければ背景へ回さない。
    #[test]
    fn without_an_input_span_nothing_is_called_background() {
        let d = detection_of(T0, T0 + 3600, 6, "idle", DetectRoute::FixedCondition);
        assert!(!is_standing(&d, None, STANDING_SPAN_PERCENT));
        assert_eq!(share_of_input_percent(&d.support, None), None);
        assert_eq!(share_of_input_percent(&d.support, Some(7200)), Some(50));
    }

    #[test]
    fn viewpoints_are_counted_per_series() {
        let ds = vec![
            detection(T0, T0 + 600, "idle", DetectRoute::FixedCondition),
            detection(T0, T0 + 600, "idle", DetectRoute::LevelShift),
            detection(T0, T0 + 600, "iowait", DetectRoute::RobustDeviation),
        ];
        let eps = group(ds, rules(1200), &[]);
        assert_eq!(eps.len(), 1);
        assert_eq!(eps[0].viewpoints.len(), 3);
        assert_eq!(eps[0].max_viewpoints_on_one_series, 2);
        assert_eq!(eps[0].series_with_corroborating_viewpoints().len(), 1);
    }

    #[test]
    fn a_series_with_only_a_level_shift_is_not_a_combined_viewpoint() {
        let ds = vec![detection(T0, T0 + 600, "idle", DetectRoute::LevelShift)];
        let eps = group(ds, rules(1200), &[]);
        assert!(eps[0].series_with_corroborating_viewpoints().is_empty());
    }

    /// 同じ系列でも時刻が対応していなければ「変化した時刻が分かった」ではない。
    #[test]
    fn a_corroborating_viewpoint_must_overlap_in_time() {
        let ds = vec![
            detection(T0, T0 + 600, "idle", DetectRoute::LevelShift),
            // 同じ系列だが 1 時間後の逸脱
            detection(T0 + 3600, T0 + 4200, "idle", DetectRoute::RobustDeviation),
        ];
        let eps = group(ds, rules(7200), &[]);
        assert_eq!(eps.len(), 1, "同じエピソードには入る");
        assert!(
            eps[0].series_with_corroborating_viewpoints().is_empty(),
            "時刻が重なっていないので裏付けとして示さない"
        );
    }

    #[test]
    fn the_longest_detection_is_chosen_by_sample_count() {
        let ds = vec![
            detection_of(T0, T0 + 600, 1, "idle", DetectRoute::FixedCondition),
            detection_of(
                T0 + 600,
                T0 + 3000,
                4,
                "iowait",
                DetectRoute::FixedCondition,
            ),
        ];
        let eps = group(ds, rules(1200), &[]);
        assert_eq!(eps[0].longest().series.column, "iowait");
        assert_eq!(eps[0].longest_detection.samples, 4);
    }

    #[test]
    fn empty_input_yields_no_episode() {
        assert!(group(Vec::new(), rules(1200), &[]).is_empty());
    }
}

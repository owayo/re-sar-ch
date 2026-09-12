//! 検出結果の解釈・提示用の型 — **観測と解釈を分ける**。
//!
//! [`crate::detect`] が出すのは観測 ([`crate::detect::Detection`]) と
//! その根拠だけである。この層はそれを読んで
//!
//! - **調査優先度** ([`Priority`]) — 順序尺度
//! - **根拠の充足度** ([`EvidenceSufficiency`]) — 何サンプル使えたか
//! - **評価の網羅度** ([`EvaluationCoverage`]) — 何を評価でき、何を評価できなかったか
//!
//! を付ける。
//!
//! # 確率を出さない
//!
//! 「確信度 92%」のような値は出さない。単一ホストの短い時系列から
//! 較正された確率は作れず、数字があると読み手はそれを信じてしまう。
//! 優先度は**順序尺度**であり、充足度は**別のフィールド**である。
//! 両者を 1 つのスコアへ潰さない (潰した瞬間に確率のように見える)。
//!
//! # 優先度は「経路の数」で上げない
//!
//! 3 経路は統計的に独立ではない。`%idle` が 5% を下回れば
//! 固定条件も逸脱も水準変化も同時に鳴りやすいので、
//! ヒット数で加点すると相関した根拠を「重なった」と誤って数える。
//!
//! 代わりに**持続性**で 1 段上げる。
//!
//! | 段 | 条件 |
//! |---|---|
//! | 下地 | 各検出が宣言する [`crate::detect::Detection::base_priority`] の最大 |
//! | +1 | 同方向の状態が [`crate::detect::DetectThresholds::persistence_samples`] 回以上かつ [`crate::detect::DetectThresholds::persistence_secs`] 以上続いた |
//! | +1 | 同じ系列に水準変化と他の観点が当たった (「いつ変わったか」が増えた) |
//! | −1 | 根拠の充足度が [`SufficiencyLevel::Thin`] |
//!
//! **加点は合計 1 段まで。** `Investigate` が上限で、`Informational` が下限。
//! 適用した理由は [`AssessedEpisode::priority_reasons`] に残す。
//!
//! # 「評価できなかった」を「検出なし」と混同しない
//!
//! 欠落で基準が作れなかった系列は「異変なし」ではない。
//! [`EvaluationCoverage`] に理由つきで記録し、報告に出す
//! (`docs/design.md` 4 章の「欠落とゼロを混同しない」の延長)。

use serde::Serialize;

use crate::analyze::metric_catalog::{CATALOG, CATALOG_VERSION, CatalogEntry};
use crate::analyze::summary::{NativePeriodSummary, PeriodBounds, SummarySource};
use crate::detect::episodes::{self, Episode};
use crate::detect::{
    BaselineEvidence, BasisOrigin, DETECT_SCHEMA_VERSION, DETECTOR_VERSION, DetectOptions,
    DetectOutcome, DetectRoute, DetectThresholds, Detection, Dispersion, PreparedSeries, SeriesKey,
};

/// 所見の種別。`summarize` の出力と混同されないよう明示する。
pub const ASSESSMENT_KIND: &str = "resarch_detect_assessment";

// ===========================================================================
// 順序尺度
// ===========================================================================

/// 調査優先度。**確率ではなく順序尺度。**
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    /// 記録しておく価値はあるが、単独では調査の理由にならない。
    Informational,
    /// 他の情報と併せて見る。
    Watch,
    /// 先に見る。
    Investigate,
}

impl Priority {
    pub const fn label(self) -> &'static str {
        match self {
            Priority::Informational => "参考",
            Priority::Watch => "注視",
            Priority::Investigate => "調査",
        }
    }

    /// 出力の先頭に付ける印。
    pub const fn mark(self) -> &'static str {
        match self {
            Priority::Informational => "..",
            Priority::Watch => "!.",
            Priority::Investigate => "!!",
        }
    }

    /// CLI の `--min-priority` で使う名前。
    pub const fn as_str(self) -> &'static str {
        match self {
            Priority::Informational => "informational",
            Priority::Watch => "watch",
            Priority::Investigate => "investigate",
        }
    }

    fn up(self) -> Self {
        match self {
            Priority::Informational => Priority::Watch,
            Priority::Watch | Priority::Investigate => Priority::Investigate,
        }
    }

    fn down(self) -> Self {
        match self {
            Priority::Investigate => Priority::Watch,
            Priority::Watch | Priority::Informational => Priority::Informational,
        }
    }
}

/// 根拠の充足度。**優先度とは別のフィールド。**
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SufficiencyLevel {
    /// 基準を作る材料が乏しい。判断を控える理由になる。
    Thin,
    /// 基準は作れたが余裕は無い。
    Moderate,
    /// 基準を作るのに十分な採取があった。
    Adequate,
}

impl SufficiencyLevel {
    pub const fn label(self) -> &'static str {
        match self {
            SufficiencyLevel::Thin => "乏しい",
            SufficiencyLevel::Moderate => "最低限",
            SufficiencyLevel::Adequate => "十分",
        }
    }
}

/// 根拠がどれだけ揃っていたか。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct EvidenceSufficiency {
    pub level: SufficiencyLevel,
    /// 比較基準を作るのに使えた採取回数。
    pub baseline_samples: u64,
    /// 検出を裏付けた採取回数。
    pub detected_samples: u64,
    /// 同じ系列で値が得られなかった採取回数。
    pub missing_samples: u64,
    /// 同じ系列で不連続として捨てた区間数。
    pub discontinuities: u64,
    /// **比較基準が異変側へ寄っている疑いがあるか。**
    pub basis_may_reflect_the_anomaly: bool,
}

impl EvidenceSufficiency {
    fn of(episode: &Episode, th: &DetectThresholds) -> Self {
        let baseline_samples = episode
            .detections
            .iter()
            .map(|d| d.baseline.samples)
            .max()
            .unwrap_or(0);
        let detected_samples = episode.longest_detection.samples;
        let missing = episode
            .detections
            .iter()
            .map(|d| d.support.missing_samples)
            .max()
            .unwrap_or(0);
        let breaks = episode
            .detections
            .iter()
            .map(|d| d.support.discontinuities)
            .max()
            .unwrap_or(0);
        let leaning = episode
            .detections
            .iter()
            .any(|d| d.baseline.may_reflect_the_anomaly());

        let total = baseline_samples + missing;
        let missing_share = if total == 0 {
            0.0
        } else {
            missing as f64 / total as f64
        };
        let level = if baseline_samples < th.min_baseline_samples {
            SufficiencyLevel::Thin
        } else if baseline_samples >= th.min_baseline_samples * 3 && missing_share < 0.1 {
            SufficiencyLevel::Adequate
        } else {
            SufficiencyLevel::Moderate
        };

        Self {
            level,
            baseline_samples,
            detected_samples,
            missing_samples: missing,
            discontinuities: breaks,
            basis_may_reflect_the_anomaly: leaning,
        }
    }
}

// ===========================================================================
// 評価の網羅度
// ===========================================================================

/// 経路を評価できなかった理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NotEvaluated {
    /// 系列がこの入力に無い (その世代のファイルに列が無い / activity 未収集)。
    SeriesAbsentFromSource,
    /// 値が得られた採取が 1 つも無い。
    NoUsableObservation,
    /// この系列には固定条件を宣言していない。
    NoFixedConditionDeclared,
    /// この系列では逸脱の向きを宣言していない。
    NoDeviationInterestDeclared,
    /// この系列では水準変化を見ないと宣言している。
    LevelShiftNotDeclared,
    /// `MAD == 0`。散らばりが測れないので逸脱スコアを出さない。
    DispersionNotMeasurable,
    /// 中央値と同じ値が大半を占める。散らばりの推定材料にならない。
    DispersionTooSparse,
    /// 基準を作るサンプルが足りない。
    TooFewBaselineSamples,
    /// 前後の窓を取れる長さの連続区間が無い。
    NoWindowLongEnough,
}

impl NotEvaluated {
    pub const fn label(self) -> &'static str {
        match self {
            NotEvaluated::SeriesAbsentFromSource => "この入力に系列が無い",
            NotEvaluated::NoUsableObservation => "有効な観測が無い",
            NotEvaluated::NoFixedConditionDeclared => "固定条件を宣言していない",
            NotEvaluated::NoDeviationInterestDeclared => "逸脱の向きを宣言していない",
            NotEvaluated::LevelShiftNotDeclared => "水準変化を見ない指標",
            NotEvaluated::DispersionNotMeasurable => "MAD が 0 で散らばりが測れない",
            NotEvaluated::DispersionTooSparse => "同値が大半で散らばりが測れない",
            NotEvaluated::TooFewBaselineSamples => "基準を作るサンプルが足りない",
            NotEvaluated::NoWindowLongEnough => "前後窓を取れる連続区間が無い",
        }
    }

    /// 「見ないと宣言している」のか「見たかったが見られなかった」のか。
    ///
    /// 前者は設計上の選択、後者はデータの制約。報告で混ぜない。
    pub const fn is_by_design(self) -> bool {
        matches!(
            self,
            NotEvaluated::NoFixedConditionDeclared
                | NotEvaluated::NoDeviationInterestDeclared
                | NotEvaluated::LevelShiftNotDeclared
        )
    }
}

/// 1 経路の評価結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RouteStatus {
    /// 検出あり。
    Detected { count: usize },
    /// 評価できたが検出なし。
    Evaluated,
    /// 評価できなかった。**「検出なし」とは別。**
    NotEvaluated { reason: NotEvaluated },
}

impl RouteStatus {
    pub const fn detections(self) -> usize {
        match self {
            RouteStatus::Detected { count } => count,
            _ => 0,
        }
    }

    pub const fn was_evaluated(self) -> bool {
        matches!(self, RouteStatus::Detected { .. } | RouteStatus::Evaluated)
    }

    pub fn label(self) -> String {
        match self {
            RouteStatus::Detected { count } => format!("検出 {count} 件"),
            RouteStatus::Evaluated => "検出なし".to_string(),
            RouteStatus::NotEvaluated { reason } => format!("評価不能 ({})", reason.label()),
        }
    }
}

/// 1 系列の評価結果。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SeriesEvaluation {
    pub series: SeriesKey,
    pub metric_label: &'static str,
    /// この入力に系列があったか。
    pub present: bool,
    /// 値が得られた採取回数。
    pub observed_samples: u64,
    /// 値が得られなかった採取回数。
    pub missing_samples: u64,
    /// 不連続として捨てた区間数。
    pub discontinuities: u64,
    /// 比較基準を作るのに使えた採取回数。
    pub baseline_samples: u64,
    /// 基準の出所。系列が無い場合は `None`。
    pub baseline_basis: Option<BasisOrigin>,
    /// 散らばりが測れたか。系列が無い場合は `None`。
    pub dispersion: Option<Dispersion>,
    /// 基準が異変側へ寄っている疑いがあるか。
    pub basis_may_reflect_the_anomaly: bool,
    pub fixed_condition: RouteStatus,
    pub robust_deviation: RouteStatus,
    pub level_shift: RouteStatus,
}

impl SeriesEvaluation {
    /// 入力に存在した系列の評価。
    pub fn observed(
        series: &PreparedSeries,
        entry: &'static CatalogEntry,
        baseline: &BaselineEvidence,
    ) -> Self {
        Self {
            series: SeriesKey::from_metric(&series.key),
            metric_label: entry.label,
            present: true,
            observed_samples: series.observations.len() as u64,
            missing_samples: series.missing,
            discontinuities: series.discontinuities,
            baseline_samples: baseline.samples,
            baseline_basis: Some(baseline.basis),
            dispersion: Some(baseline.dispersion),
            basis_may_reflect_the_anomaly: baseline.may_reflect_the_anomaly(),
            fixed_condition: RouteStatus::Evaluated,
            robust_deviation: RouteStatus::Evaluated,
            level_shift: RouteStatus::Evaluated,
        }
    }

    /// 入力に無かった系列の評価。**黙って落とさない。**
    pub fn absent(entry: &'static CatalogEntry) -> Self {
        let absent = RouteStatus::NotEvaluated {
            reason: NotEvaluated::SeriesAbsentFromSource,
        };
        Self {
            series: SeriesKey::from_metric(&entry.placeholder_key()),
            metric_label: entry.label,
            present: false,
            observed_samples: 0,
            missing_samples: 0,
            discontinuities: 0,
            baseline_samples: 0,
            baseline_basis: None,
            dispersion: None,
            basis_may_reflect_the_anomaly: false,
            fixed_condition: absent,
            robust_deviation: absent,
            level_shift: absent,
        }
    }

    /// いずれかの経路で評価できたか。
    pub fn was_evaluated(&self) -> bool {
        self.fixed_condition.was_evaluated()
            || self.robust_deviation.was_evaluated()
            || self.level_shift.was_evaluated()
    }

    /// 検出件数の合計。
    pub fn detections(&self) -> usize {
        self.fixed_condition.detections()
            + self.robust_deviation.detections()
            + self.level_shift.detections()
    }

    fn status(&self, route: DetectRoute) -> RouteStatus {
        match route {
            DetectRoute::FixedCondition => self.fixed_condition,
            DetectRoute::RobustDeviation => self.robust_deviation,
            DetectRoute::LevelShift => self.level_shift,
        }
    }
}

/// 1 経路の集計。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct RouteTally {
    pub detected_series: usize,
    pub detections: usize,
    pub evaluated_series: usize,
    /// 宣言により見ていない系列数 (設計上の選択)。
    pub not_applicable_series: usize,
    /// 見たかったが見られなかった系列数 (データの制約)。
    pub blocked_series: usize,
}

impl RouteTally {
    fn add(&mut self, status: RouteStatus) {
        match status {
            RouteStatus::Detected { count } => {
                self.detected_series += 1;
                self.evaluated_series += 1;
                self.detections += count;
            }
            RouteStatus::Evaluated => self.evaluated_series += 1,
            RouteStatus::NotEvaluated { reason } => {
                if reason.is_by_design() {
                    self.not_applicable_series += 1;
                } else {
                    self.blocked_series += 1;
                }
            }
        }
    }
}

/// 何を評価でき、何を評価できなかったか。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EvaluationCoverage {
    pub catalog_version: &'static str,
    /// カタログが宣言する系列パターン数。
    pub patterns_in_catalog: usize,
    /// 入力に存在した系列数 (item 展開後)。
    pub series_present: usize,
    /// いずれかの経路で評価できた系列数。
    pub series_evaluated: usize,
    /// どの経路でも評価できなかった系列数。
    pub series_not_evaluated: usize,
    pub fixed_condition: RouteTally,
    pub robust_deviation: RouteTally,
    pub level_shift: RouteTally,
    /// 系列ごとの内訳。
    pub series: Vec<SeriesEvaluation>,
}

impl EvaluationCoverage {
    fn of(evaluations: Vec<SeriesEvaluation>) -> Self {
        let mut c = Self {
            catalog_version: CATALOG_VERSION,
            patterns_in_catalog: CATALOG.len(),
            series_present: 0,
            series_evaluated: 0,
            series_not_evaluated: 0,
            fixed_condition: RouteTally::default(),
            robust_deviation: RouteTally::default(),
            level_shift: RouteTally::default(),
            series: evaluations,
        };
        for e in &c.series {
            if e.present {
                c.series_present += 1;
            }
            if e.was_evaluated() {
                c.series_evaluated += 1;
            } else {
                c.series_not_evaluated += 1;
            }
            c.fixed_condition.add(e.fixed_condition);
            c.robust_deviation.add(e.robust_deviation);
            c.level_shift.add(e.level_shift);
        }
        c
    }

    /// 評価できなかった系列のうち、データの制約によるもの。
    ///
    /// 「見ないと宣言している」ものは除く (設計上の選択なので報告の必要が薄い)。
    pub fn blocked(&self) -> impl Iterator<Item = &SeriesEvaluation> {
        self.series.iter().filter(|e| {
            e.present
                && [
                    DetectRoute::FixedCondition,
                    DetectRoute::RobustDeviation,
                    DetectRoute::LevelShift,
                ]
                .iter()
                .any(|r| match e.status(*r) {
                    RouteStatus::NotEvaluated { reason } => !reason.is_by_design(),
                    _ => false,
                })
        })
    }
}

// ===========================================================================
// 解釈されたエピソード
// ===========================================================================

/// エピソードに優先度と充足度を付けたもの。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AssessedEpisode {
    pub episode: Episode,
    /// 調査優先度 (**順序尺度。確率ではない**)。
    pub priority: Priority,
    /// 各検出が宣言する下地の最大。
    pub base_priority: Priority,
    /// 優先度をその値にした理由 (昇降の根拠)。
    pub priority_reasons: Vec<&'static str>,
    /// 根拠の充足度 (**優先度とは別**)。
    pub sufficiency: EvidenceSufficiency,
    /// 1 行の見出し。
    pub headline: String,
    /// 当たった観点の名前。
    pub viewpoints: Vec<&'static str>,
    /// 考えられる解釈 (重複なし)。
    pub possible_interpretations: Vec<&'static str>,
    /// この所見では確かめていないこと (重複なし)。
    pub not_established: Vec<&'static str>,
}

impl AssessedEpisode {
    fn of(episode: Episode, th: &DetectThresholds) -> Self {
        let sufficiency = EvidenceSufficiency::of(&episode, th);
        let base = episode
            .detections
            .iter()
            .map(|d| d.base_priority)
            .max()
            .unwrap_or(Priority::Informational);

        let mut reasons: Vec<&'static str> = Vec::new();
        let mut priority = base;

        // 加点は合計 1 段まで。**経路の数では上げない** (3 経路は相関する)。
        let persisted = episode.detections.iter().any(|d| {
            d.support.samples >= th.persistence_samples
                && d.support.span_secs() >= th.persistence_secs
        });
        if persisted {
            priority = priority.up();
            reasons.push("同じ状態が複数回の採取にわたって続いた (+1 段)");
        } else if episode.has_change_point_for_a_flagged_series() {
            priority = priority.up();
            reasons.push("同じ系列に水準変化が重なり、変化した時刻が分かった (+1 段)");
        }
        if sufficiency.level == SufficiencyLevel::Thin {
            priority = priority.down();
            reasons.push("比較基準の材料が乏しいので判断を控えた (−1 段)");
        }
        if reasons.is_empty() {
            reasons.push("昇降なし (各検出が宣言する下地のまま)");
        }

        let lead = episode.lead();
        let headline = format!(
            "{} [{}] {} — {}",
            lead.metric_label,
            lead.series.display(),
            lead.pattern.label(),
            lead.support.describe_span()
        );

        let viewpoints = episode
            .viewpoints
            .iter()
            .map(|r| r.label())
            .collect::<Vec<_>>();
        let mut interpretations: Vec<&'static str> = Vec::new();
        let mut not_established: Vec<&'static str> = Vec::new();
        for d in &episode.detections {
            for i in d.possible_interpretations {
                if !interpretations.contains(i) {
                    interpretations.push(i);
                }
            }
            for n in d.not_established {
                if !not_established.contains(n) {
                    not_established.push(n);
                }
            }
        }

        Self {
            episode,
            priority,
            base_priority: base,
            priority_reasons: reasons,
            sufficiency,
            headline,
            viewpoints,
            possible_interpretations: interpretations,
            not_established,
        }
    }
}

// ===========================================================================
// 所見
// ===========================================================================

/// 読み手に必ず伝える前提。
const STANDING_NOTES: &[&str] = &[
    "比較基準はこの入力自身から作ったものであり、外部の正常値ではない。\
     異変が入力の大半を占めていれば基準もその状態に寄る",
    "確率や確信度は出さない。優先度は順序尺度で、根拠の充足度は別のフィールドである",
    "3 つの観点 (絶対水準 / 参照分布からの逸脱 / 時間的変化) は統計的に独立ではない。\
     複数の観点が当たったことを独立な裏付けの数として数えていない",
    "sar のデータは離散的な採取である。採取と採取の間に何が起きていたかは観測されていない",
];

/// ファイル (群) 全体の所見。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Assessment {
    pub schema_version: &'static str,
    /// 種別。`summarize` の出力と混同しないための印。
    pub assessment_kind: &'static str,
    pub detector_version: &'static str,
    /// 適用した閾値 (読み手が判定を再現できるように全部載せる)。
    pub thresholds: DetectThresholds,
    /// 比較基準の出所。
    pub baseline_basis: BasisOrigin,
    /// エピソードをまとめたギャップ許容量 (秒)。
    pub episode_gap_secs: u64,
    /// 採取間隔の代表値 (秒)。
    pub interval_p90_secs: Option<u64>,
    pub source: SummarySource,
    pub period: PeriodBounds,
    /// 所見 (時刻順)。
    pub episodes: Vec<AssessedEpisode>,
    /// 何を評価でき、何を評価できなかったか。
    pub coverage: EvaluationCoverage,
    pub notes: &'static [&'static str],
}

impl Assessment {
    /// 優先度の下限で絞る。
    pub fn filter_priority(&mut self, min: Priority) {
        self.episodes.retain(|e| e.priority >= min);
    }

    /// 最も高い優先度。
    pub fn top_priority(&self) -> Option<Priority> {
        self.episodes.iter().map(|e| e.priority).max()
    }

    /// 検出件数の合計。
    pub fn detection_count(&self) -> usize {
        self.episodes
            .iter()
            .map(|e| e.episode.detections.len())
            .sum()
    }
}

/// 検出結果を所見へまとめる。
pub fn assess(
    outcome: DetectOutcome,
    source: SummarySource,
    period: PeriodBounds,
    opts: &DetectOptions,
) -> Assessment {
    let th = opts.thresholds;
    let gap = episodes::gap_secs(
        outcome.interval_p90_secs,
        th.episode_gap_factor,
        th.episode_gap_cap_secs,
    );
    let grouped = episodes::group(outcome.detections, gap, &outcome.discontinuity_marks);
    let episodes: Vec<AssessedEpisode> = grouped
        .into_iter()
        .map(|e| AssessedEpisode::of(e, &th))
        .collect();

    Assessment {
        schema_version: DETECT_SCHEMA_VERSION,
        assessment_kind: ASSESSMENT_KIND,
        detector_version: DETECTOR_VERSION,
        thresholds: th,
        baseline_basis: basis_of(opts),
        episode_gap_secs: gap,
        interval_p90_secs: outcome.interval_p90_secs,
        source,
        period,
        episodes,
        coverage: EvaluationCoverage::of(outcome.evaluations),
        notes: STANDING_NOTES,
    }
}

/// サマリから所見を作る (1 起動区間ぶん)。
pub fn assess_summary(summary: &NativePeriodSummary, opts: &DetectOptions) -> Assessment {
    let outcome = crate::detect::detect(&summary.timelines, opts);
    assess(outcome, summary.source.clone(), summary.period, opts)
}

fn basis_of(opts: &DetectOptions) -> BasisOrigin {
    match opts.baseline_scope {
        crate::detect::BaselineScope::Input => BasisOrigin::InputItself,
        crate::detect::BaselineScope::Window => BasisOrigin::ReportWindowOnly,
    }
}

/// 検出 1 件を 1 行の文へ。
///
/// **採取回数と時間範囲を必ず併記する** (`crate::detect::TemporalSupport` の方針)。
/// 時刻そのものは出力層が付ける (エポック秒の書式はこの層の責務ではない)。
pub fn describe_detection(d: &Detection) -> String {
    use crate::detect::DecisionBasis;
    let unit = d.unit.suffix();
    match &d.decision.basis {
        DecisionBasis::FixedCondition {
            comparison,
            threshold,
            ..
        } => format!(
            "{} が {}{unit} {}の状態で {} (最小 {:.2} / 最大 {:.2})",
            d.metric_label,
            threshold,
            comparison.label(),
            d.support.describe_span(),
            d.decision.min,
            d.decision.max
        ),
        DecisionBasis::RobustDeviation {
            median,
            mad,
            peak_mad_ratio,
            direction,
            ..
        } => format!(
            "{} が比較基準 (中央値 {:.2}{unit}、MAD {:.2}) から{}側へ MAD の {:.1} 倍離れた \
             ({}、最小 {:.2} / 最大 {:.2})",
            d.metric_label,
            median,
            mad,
            direction.as_str(),
            peak_mad_ratio,
            d.support.describe_span(),
            d.decision.min,
            d.decision.max
        ),
        DecisionBasis::LevelShift {
            before_median,
            after_median,
            shift,
            normalized_shift,
            before,
            after,
            ..
        } => {
            let norm = match normalized_shift {
                Some(n) => format!("散らばりの {n:.1} 倍"),
                None => "散らばりが測れないため正規化なし".to_string(),
            };
            format!(
                "{} の水準が {:.2}{unit} から {:.2}{unit} へ {:+.2}{unit} 動いた ({norm}。\
                 前: {} / 後: {})",
                d.metric_label,
                before_median,
                after_median,
                shift,
                before.describe_span(),
                after.describe_span()
            )
        }
    }
}

/// 所見全体を 1 行で要約する。
pub fn describe_assessment(a: &Assessment) -> String {
    match a.top_priority() {
        None => format!(
            "エピソードなし (評価できた系列 {} / 入力にあった系列 {})",
            a.coverage.series_evaluated, a.coverage.series_present
        ),
        Some(p) => format!(
            "エピソード {} 件 (最高優先度: {})、検出 {} 件、評価できた系列 {} / 入力にあった系列 {}",
            a.episodes.len(),
            p.label(),
            a.detection_count(),
            a.coverage.series_evaluated,
            a.coverage.series_present
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze::timeline::Timelines;
    use crate::detect::testing::*;
    use crate::detect::{BaselineScope, ReportBound};

    fn assess_timelines(ts: &Timelines, opts: &DetectOptions) -> Assessment {
        let outcome = crate::detect::detect(ts, opts);
        assess(
            outcome,
            SummarySource::default(),
            PeriodBounds::default(),
            opts,
        )
    }

    #[test]
    fn priority_is_an_ordinal_scale() {
        assert!(Priority::Investigate > Priority::Watch);
        assert!(Priority::Watch > Priority::Informational);
        assert_eq!(Priority::Informational.up(), Priority::Watch);
        assert_eq!(Priority::Investigate.up(), Priority::Investigate);
        assert_eq!(Priority::Informational.down(), Priority::Informational);
    }

    /// 3 経路のうち 1 つが 0 件でも他の経路は動く。
    #[test]
    fn the_routes_do_not_gate_one_another() {
        // runq_sz は固定条件を持たないが、逸脱と水準変化で拾える
        let mut v: Vec<f64> = (0..20).map(|i| 2.0 + f64::from(i % 3)).collect();
        v.extend((0..20).map(|i| 40.0 + f64::from(i % 3)));
        let ts = single(runq(&vals(&v)));
        let a = assess_timelines(&ts, &DetectOptions::default());

        let ev = a
            .coverage
            .series
            .iter()
            .find(|e| e.series.column == "runq_sz")
            .expect("評価行");
        assert!(
            matches!(
                ev.fixed_condition,
                RouteStatus::NotEvaluated {
                    reason: NotEvaluated::NoFixedConditionDeclared
                }
            ),
            "固定条件は宣言していない: {:?}",
            ev.fixed_condition
        );
        assert!(
            ev.level_shift.detections() > 0,
            "固定条件が 0 件でも水準変化は動く: {:?}",
            ev.level_shift
        );
        assert!(!a.episodes.is_empty());
    }

    /// 逆に、逸脱が評価不能でも固定条件は動く。
    #[test]
    fn a_fixed_condition_fires_even_when_dispersion_cannot_be_measured() {
        // %idle が終始 2% (= 一定値なので MAD は 0)
        let ts = single(cpu_idle(&vals(&[2.0; 30])));
        let a = assess_timelines(&ts, &DetectOptions::default());
        let ev = a
            .coverage
            .series
            .iter()
            .find(|e| e.series.column == "idle")
            .expect("評価行");
        assert!(matches!(
            ev.robust_deviation,
            RouteStatus::NotEvaluated {
                reason: NotEvaluated::DispersionNotMeasurable
            }
        ));
        assert!(
            ev.fixed_condition.detections() > 0,
            "逸脱が評価不能でも固定条件は動く"
        );
        assert!(!a.episodes.is_empty());
    }

    /// 異変が入力の大半を占めるとき、基準がそちらへ寄ることを型と出力が明示する。
    #[test]
    fn the_output_admits_when_the_basis_reflects_the_anomaly() {
        // 30 点すべて %idle 2%。中央値そのものが固定条件の内側
        let ts = single(cpu_idle(&vals(&[2.0; 30])));
        let a = assess_timelines(&ts, &DetectOptions::default());

        let ev = a
            .coverage
            .series
            .iter()
            .find(|e| e.series.column == "idle")
            .expect("評価行");
        assert!(
            ev.basis_may_reflect_the_anomaly,
            "評価行に「基準が異変側へ寄っている」ことが出る"
        );

        let ep = a.episodes.first().expect("エピソード");
        assert!(
            ep.sufficiency.basis_may_reflect_the_anomaly,
            "充足度にも出る"
        );
        let d = &ep.episode.detections[0];
        assert!(d.baseline.median_within_fixed_condition);
        assert!((d.baseline.flagged_share - 1.0).abs() < 1e-9);
        assert!(
            d.baseline
                .caveats
                .iter()
                .any(|c| c.contains("基準が異常側へ寄っている")),
            "留保が本文に載る"
        );
        // 「正常値」という語を出力に出さない
        let json = serde_json::to_string(&a).expect("JSON");
        assert!(!json.contains("baseline_normal"));
        assert!(json.contains("input_itself"));
    }

    /// 優先度は経路の数では上がらない (持続性で上がる)。
    #[test]
    fn priority_escalates_on_persistence_not_on_route_count() {
        // %idle が 3 回 (30 分) 続けて 2% → 持続性で 1 段上がる
        let mut v = vec![80.0; 20];
        v[10] = 2.0;
        v[11] = 2.0;
        v[12] = 2.0;
        v[13] = 2.0;
        let ts = single(cpu_idle(&vals(&v)));
        let a = assess_timelines(&ts, &DetectOptions::default());
        let ep = a.episodes.first().expect("エピソード");
        assert_eq!(ep.base_priority, Priority::Investigate);
        assert_eq!(ep.priority, Priority::Investigate, "上限で止まる");
        assert!(
            ep.priority_reasons
                .iter()
                .any(|r| r.contains("複数回の採取にわたって続いた"))
        );
    }

    /// 材料が乏しければ 1 段下げる。
    #[test]
    fn a_thin_basis_demotes_the_priority() {
        // 6 点だけ。基準を作るサンプル (既定 12) に届かない
        let ts = single(cpu_idle(&vals(&[80.0, 80.0, 2.0, 2.0, 80.0, 80.0])));
        let a = assess_timelines(&ts, &DetectOptions::default());
        let ep = a.episodes.first().expect("エピソード");
        assert_eq!(ep.sufficiency.level, SufficiencyLevel::Thin);
        assert_eq!(ep.base_priority, Priority::Investigate);
        assert!(ep.priority < ep.base_priority, "材料が乏しいので下げる");
        assert!(
            ep.priority_reasons
                .iter()
                .any(|r| r.contains("材料が乏しい"))
        );
    }

    /// 「評価できなかった」を「検出なし」と混同しない。
    #[test]
    fn coverage_separates_not_evaluated_from_no_detection() {
        let ts = single(cpu_idle(&vals(&[50.0; 20])));
        let a = assess_timelines(&ts, &DetectOptions::default());
        assert!(a.coverage.patterns_in_catalog > 1);
        assert_eq!(a.coverage.series_present, 1);
        assert!(
            a.coverage.series_not_evaluated > 0,
            "入力に無い系列は評価不能として残る"
        );
        // 固定条件は評価できて検出 0 件
        let ev = &a.coverage.series.iter().find(|e| e.present).unwrap();
        assert_eq!(ev.fixed_condition, RouteStatus::Evaluated);
        assert_eq!(ev.detections(), 0);
    }

    #[test]
    fn filter_priority_drops_lower_episodes() {
        let mut v = vec![0.0; 30];
        v[10] = 5.0;
        let ts = single(timeline(
            crate::analyze::timeline::MetricKey::new(
                crate::model::ActivityId::PAGE,
                "-",
                "pgscank",
            ),
            crate::model::Unit::CountPerSec,
            crate::model::ValueKind::Counter,
            &vals(&v),
        ));
        let mut a = assess_timelines(&ts, &DetectOptions::default());
        assert_eq!(a.episodes.len(), 1);
        // pgscank の発生は Informational 相当 (材料が乏しければ更に下がる)
        assert!(a.episodes[0].priority <= Priority::Watch);
        a.filter_priority(Priority::Investigate);
        assert!(a.episodes.is_empty());
    }

    #[test]
    fn standing_notes_state_the_limits() {
        let ts = single(cpu_idle(&vals(&[50.0; 20])));
        let a = assess_timelines(&ts, &DetectOptions::default());
        assert!(a.notes.iter().any(|n| n.contains("外部の正常値ではない")));
        assert!(a.notes.iter().any(|n| n.contains("確率や確信度は出さない")));
        assert!(a.notes.iter().any(|n| n.contains("独立ではない")));
        assert!(a.notes.iter().any(|n| n.contains("離散的な採取")));
    }

    #[test]
    fn baseline_scope_is_recorded_in_the_assessment() {
        let ts = single(cpu_idle(&vals(&[50.0; 20])));
        let a = assess_timelines(&ts, &DetectOptions::default());
        assert_eq!(a.baseline_basis, BasisOrigin::InputItself);

        let opts = DetectOptions {
            baseline_scope: BaselineScope::Window,
            report_from: ReportBound::Epoch(T0),
            ..Default::default()
        };
        let b = assess_timelines(&ts, &opts);
        assert_eq!(b.baseline_basis, BasisOrigin::ReportWindowOnly);
    }

    /// 検出の説明文は採取回数と時間範囲を併記する。
    #[test]
    fn the_description_never_claims_continuous_time() {
        let mut v = vec![80.0; 20];
        v[10] = 2.0;
        v[11] = 2.0;
        let ts = single(cpu_idle(&vals(&v)));
        let a = assess_timelines(&ts, &DetectOptions::default());
        let d = &a.episodes[0].episode.detections[0];
        let text = describe_detection(d);
        assert!(text.contains("回の採取"), "{text}");
        assert!(!text.contains("分間"), "{text}");
    }
}

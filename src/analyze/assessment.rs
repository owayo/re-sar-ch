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
//! # 優先度は検出 1 件ごとに決める
//!
//! **エピソードは複数の検出の入れ物であり、優先度の単位ではない。**
//! 下地をエピソード内の最大から採り、昇格条件を「どれか 1 つの検出が持続した」
//! にすると、単発の direct reclaim と長時間の低優先度所見が同じエピソードに
//! 入っただけで direct reclaim が持続したかのように昇格する。
//!
//! したがって [`AssessedDetection`] を検出 1 件ごとに作り、
//! エピソードの優先度はその**最大値**とする。見出しもその検出に揃える
//! (優先度を出した検出と見出しの検出が違うと、読み手は別の検出の根拠を読む)。
//!
//! | 段 | 条件 |
//! |---|---|
//! | 下地 | **その検出**が宣言する [`crate::detect::Detection::base_priority`] |
//! | +1 | **その検出自身**が [`crate::detect::DetectThresholds::persistence_samples`] 回以上かつ [`crate::detect::DetectThresholds::persistence_secs`] 以上続いた |
//! | −1 | **その経路が依存する**根拠が [`SufficiencyLevel::Thin`] ([`SufficiencyBasis`]) |
//!
//! **加点は 1 段まで。** `Investigate` が上限で、`Informational` が下限。
//! 適用した理由は [`AssessedDetection::priority_reasons`] に残す。
//!
//! 「同じ系列に水準変化と他の観点が当たった」ことによる昇格は**しない**。
//! 3 経路は相関するので、ヒット数を独立な裏付けとして数えないという規律 2′ と
//! 一致しないためである。時刻の対応を検証したうえで
//! [`AssessedEpisode::corroborating_series`] に示すだけにする。
//!
//! # 根拠の乏しさは「その判断が依存する根拠」だけに効かせる
//!
//! 固定条件 (`threshold::detect`) は比較基準を判定に使っていない。
//! 短い入力で direct reclaim を観測したとき、基準の材料が 12 点未満だからといって
//! その観測が疑わしくなるわけではない。分布に依存する判断
//! (逸脱・水準変化) だけに標本不足を効かせる ([`SufficiencyBasis`])。
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
    BaselineEvidence, BasisOrigin, DETECT_SCHEMA_VERSION, DETECTOR_VERSION, DecisionBasis,
    DetectOptions, DetectOutcome, DetectRoute, DetectThresholds, Detection, Dispersion,
    PreparedSeries, ReportBound, SeriesKey, TemporalSupport,
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

/// 充足度が**何に対する**充足度か。
///
/// 経路ごとに必要な根拠が違う。固定条件の成立に比較基準は使っていないので、
/// 基準の標本不足を固定条件の判断へ持ち込んではいけない。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SufficiencyBasis {
    /// 条件を満たした採取そのもの (固定条件)。**入力の分布に依存しない。**
    ObservedSamplesOnly,
    /// 入力全体から作った比較基準の材料 (参照分布からの逸脱)。
    ComparisonBasis,
    /// 水準変化の前後窓 (時間的変化)。**入力全体の点数ではない。**
    LocalWindows,
}

impl SufficiencyBasis {
    /// 経路から決める。
    pub const fn of(route: DetectRoute) -> Self {
        match route {
            DetectRoute::FixedCondition => SufficiencyBasis::ObservedSamplesOnly,
            DetectRoute::RobustDeviation => SufficiencyBasis::ComparisonBasis,
            DetectRoute::LevelShift => SufficiencyBasis::LocalWindows,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            SufficiencyBasis::ObservedSamplesOnly => "条件を満たした採取",
            SufficiencyBasis::ComparisonBasis => "比較基準の材料",
            SufficiencyBasis::LocalWindows => "水準変化の前後窓",
        }
    }

    /// その判断が入力の分布に依存するか。
    ///
    /// `false` の経路 (固定条件) は、比較基準の標本が乏しくても判定は成立している。
    /// **標本不足を優先度へ移し替えてよいのは `true` の経路だけ。**
    pub const fn depends_on_the_input_distribution(self) -> bool {
        !matches!(self, SufficiencyBasis::ObservedSamplesOnly)
    }
}

/// 根拠がどれだけ揃っていたか。**検出 1 件ごとに作る。**
///
/// エピソード内の最大値を採ってはいけない。重大な系列が 2 点しかなくても
/// 別系列に 144 点あれば「十分」になり、欠測率の分子と分母が
/// 別の系列から来ることもある。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct EvidenceSufficiency {
    pub level: SufficiencyLevel,
    /// 何に対する充足度か。
    pub basis: SufficiencyBasis,
    /// [`EvidenceSufficiency::basis`] が指す採取回数。
    pub material_samples: u64,
    /// その経路が要求する採取回数の下限。
    pub required_samples: u64,
    /// 比較基準を作るのに使えた採取回数 (**この検出の系列**)。
    pub baseline_samples: u64,
    /// 検出を裏付けた採取回数。
    pub detected_samples: u64,
    /// **この検出の系列で**値が得られなかった採取回数。
    pub missing_samples: u64,
    /// **この検出の系列で**不連続として捨てた区間数。
    pub discontinuities: u64,
    /// **比較基準が異変側へ寄っている疑いがあるか。**
    pub basis_may_reflect_the_anomaly: bool,
}

impl EvidenceSufficiency {
    /// 検出 1 件の充足度。
    fn of(d: &Detection, th: &DetectThresholds) -> Self {
        let basis = SufficiencyBasis::of(d.route());
        let (material, required) = match &d.decision.basis {
            // 固定条件: 連続して条件を満たした採取が要求回数に届いたか。
            // 比較基準は判定に使っていないので分母に置かない
            DecisionBasis::FixedCondition { min_samples, .. } => {
                (d.support.samples, u64::from(*min_samples).max(1))
            }
            // 逸脱: 入力全体から作った基準の材料。
            // 絶対差だけで判断した場合も中央値 (基準) を参照点に使っている
            DecisionBasis::RobustDeviation { .. } | DecisionBasis::AbsoluteDeparture { .. } => {
                (d.baseline.samples, th.min_baseline_samples)
            }
            // 水準変化: **局所窓の点数**。入力全体の点数ではない
            DecisionBasis::LevelShift { window_samples, .. } => {
                (*window_samples, (th.shift_window_min_samples as u64).max(1))
            }
        };

        let missing = d.support.missing_samples;
        let total = d.support.series_observed_samples.saturating_add(missing);
        let missing_share = if total == 0 {
            0.0
        } else {
            missing as f64 / total as f64
        };
        let level = if material < required {
            SufficiencyLevel::Thin
        } else if material >= required.saturating_mul(3) && missing_share < 0.1 {
            SufficiencyLevel::Adequate
        } else {
            SufficiencyLevel::Moderate
        };

        Self {
            level,
            basis,
            material_samples: material,
            required_samples: required,
            baseline_samples: d.baseline.samples,
            detected_samples: d.support.samples,
            missing_samples: missing,
            discontinuities: d.support.discontinuities,
            basis_may_reflect_the_anomaly: d.baseline.may_reflect_the_anomaly(),
        }
    }
}

/// エピソード内で充足度がどれだけばらついているか。
///
/// **代表 1 件だけを出すと「十分」に見える。** 重大な系列が 2 点しかないのに
/// 別系列に 144 点あるときの誤読を止めるため、範囲と混在を併記する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SufficiencySpread {
    /// 優先度を決めた検出の水準。
    pub representative: SufficiencyLevel,
    /// エピソード内の最低。
    pub lowest: SufficiencyLevel,
    /// エピソード内の最高。
    pub highest: SufficiencyLevel,
    /// 水準が揃っていないか。
    pub mixed: bool,
}

impl SufficiencySpread {
    fn of(representative: SufficiencyLevel, levels: &[SufficiencyLevel]) -> Self {
        let lowest = levels.iter().copied().min().unwrap_or(representative);
        let highest = levels.iter().copied().max().unwrap_or(representative);
        Self {
            representative,
            lowest,
            highest,
            mixed: lowest != highest,
        }
    }

    /// 1 行の表記。混在しているときは範囲も示す。
    pub fn label(&self) -> String {
        if self.mixed {
            format!(
                "{} (優先度を決めた検出) / エピソード内は {}〜{} の混在",
                self.representative.label(),
                self.lowest.label(),
                self.highest.label()
            )
        } else {
            self.representative.label().to_string()
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
    /// 利用者が activity 選択から除外した。
    ExcludedBySelection,
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
    /// 前後の窓を取れる**長さ**の連続区間が無い。
    ///
    /// 入力が短い / 連続区間が短いという構造の問題。採取を増やすしかない。
    NoWindowLongEnough,
    /// 長さは足りるが、前後の窓が**欠測を挟む**ので連続した窓が取れない。
    ///
    /// 「取れる窓が無い」という結果は [`NotEvaluated::NoWindowLongEnough`] と
    /// 同じだが、読み手が次に打つ手が違う (採取を増やすのではなく、
    /// 欠測の原因を見る)。
    ///
    /// **どちらを報告するかは水準変化の経路が決める**
    /// ([`crate::detect::level_shift`])。この層は語彙と表示だけを持つ。
    WindowSpansMissingSamples,
}

impl NotEvaluated {
    pub const fn label(self) -> &'static str {
        match self {
            NotEvaluated::SeriesAbsentFromSource => "この入力に系列が無い",
            NotEvaluated::ExcludedBySelection => "activity 選択で除外された",
            NotEvaluated::NoUsableObservation => "有効な観測が無い",
            NotEvaluated::NoFixedConditionDeclared => "固定条件を宣言していない",
            NotEvaluated::NoDeviationInterestDeclared => "逸脱の向きを宣言していない",
            NotEvaluated::LevelShiftNotDeclared => "水準変化を見ない指標",
            NotEvaluated::DispersionNotMeasurable => "MAD が 0 で散らばりが測れない",
            NotEvaluated::DispersionTooSparse => "同値が大半で散らばりが測れない",
            NotEvaluated::TooFewBaselineSamples => "基準を作るサンプルが足りない",
            NotEvaluated::NoWindowLongEnough => "前後窓を取れる長さの連続区間が無い",
            NotEvaluated::WindowSpansMissingSamples => "前後窓が欠測を挟むので連続した窓が取れない",
        }
    }

    /// 「見ないと宣言している」のか「見たかったが見られなかった」のか。
    ///
    /// 前者は設計上の選択、後者はデータの制約。報告で混ぜない。
    pub const fn is_by_design(self) -> bool {
        matches!(
            self,
            NotEvaluated::ExcludedBySelection
                | NotEvaluated::NoFixedConditionDeclared
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
    Detected {
        count: usize,
        /// 検出があっても前後窓を取れない端は未評価。
        blind_edge_points: usize,
    },
    /// 評価できたが検出なし。
    Evaluated,
    /// 評価できたが、**構造的に見ていない範囲がある**。
    ///
    /// 水準変化は前後の窓を必要とするので、連続区間の先頭と末尾の
    /// `窓の点数` ぶんは分割候補になれない。そこは
    /// 「変化が無かった」のではなく「**見ていない**」である。
    /// 検出なしと同一視すると、ファイル端で起きた変化を
    /// 「無かった」と読ませてしまう (規律 7)。
    EvaluatedWithBlindEdges {
        /// 候補になれなかった端の点数 (全連続区間の合計)。
        edge_points: usize,
    },
    /// 評価できなかった。**「検出なし」とは別。**
    NotEvaluated { reason: NotEvaluated },
}

impl RouteStatus {
    pub const fn detections(self) -> usize {
        match self {
            RouteStatus::Detected { count, .. } => count,
            _ => 0,
        }
    }

    pub const fn was_evaluated(self) -> bool {
        matches!(
            self,
            RouteStatus::Detected { .. }
                | RouteStatus::Evaluated
                | RouteStatus::EvaluatedWithBlindEdges { .. }
        )
    }

    pub fn label(self) -> String {
        match self {
            RouteStatus::Detected {
                count,
                blind_edge_points,
            } => {
                if blind_edge_points == 0 {
                    format!("検出 {count} 件")
                } else {
                    format!("検出 {count} 件 (窓を取れない端 {blind_edge_points} 採取は見ていない)")
                }
            }
            RouteStatus::Evaluated => "検出なし".to_string(),
            RouteStatus::EvaluatedWithBlindEdges { edge_points } => {
                format!("検出なし (窓を取れない端 {edge_points} 採取は見ていない)")
            }
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

    /// activity 選択から除外された系列。入力の欠落とは区別する。
    pub fn excluded(entry: &'static CatalogEntry) -> Self {
        let mut evaluation = Self::absent(entry);
        let status = RouteStatus::NotEvaluated {
            reason: NotEvaluated::ExcludedBySelection,
        };
        evaluation.fixed_condition = status;
        evaluation.robust_deviation = status;
        evaluation.level_shift = status;
        evaluation
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
    /// 評価はできたが、**構造的に見ていない端がある**系列数。
    ///
    /// 水準変化は前後の窓を要するので、連続区間の先頭と末尾は
    /// 分割候補になれない。「検出なし」に数えつつ、
    /// 見ていない範囲があることを別に数える (規律 7)。
    pub series_with_blind_edges: usize,
    /// 見ていない端の採取回数 (全系列・全連続区間の合計)。
    pub blind_edge_samples: usize,
}

impl RouteTally {
    fn add(&mut self, status: RouteStatus) {
        match status {
            RouteStatus::Detected {
                count,
                blind_edge_points,
            } => {
                if blind_edge_points > 0 {
                    self.series_with_blind_edges += 1;
                    self.blind_edge_samples += blind_edge_points;
                }
                self.detected_series += 1;
                self.evaluated_series += 1;
                self.detections += count;
            }
            RouteStatus::Evaluated => self.evaluated_series += 1,
            RouteStatus::EvaluatedWithBlindEdges { edge_points } => {
                self.evaluated_series += 1;
                self.series_with_blind_edges += 1;
                self.blind_edge_samples += edge_points;
            }
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
    /// **比較基準が異変側へ寄っている疑いがある系列数。**
    ///
    /// 検出が 1 件も立たなかった系列にも起こる。`%idle` が 4 と 6 を
    /// 交互に取れば中央値 5 は固定条件の内側だが、連続 2 回を満たさないので
    /// 固定条件の検出は無く、逸脱も水準変化も絶対差の下限に届かない。
    /// **エピソードの有無にかかわらず報告する** (規律 3)。
    pub series_with_basis_leaning: usize,
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
            series_with_basis_leaning: 0,
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
            if e.basis_may_reflect_the_anomaly {
                c.series_with_basis_leaning += 1;
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

    /// 比較基準が異変側へ寄っている疑いがある系列。
    ///
    /// **検出の有無で絞らない。** 検出が立たなかった系列の警告を落とすと、
    /// 「基準そのものが固定条件の内側にある」という留保が
    /// JSON にだけ残って text から消える (形式間で伝播が変わる)。
    pub fn basis_leaning(&self) -> impl Iterator<Item = &SeriesEvaluation> {
        self.series
            .iter()
            .filter(|e| e.present && e.basis_may_reflect_the_anomaly)
    }
}

// ===========================================================================
// 解釈されたエピソード
// ===========================================================================

/// 検出 1 件の解釈。**優先度と充足度の単位はここ。**
///
/// `AssessedEpisode::detections` は `episode.detections` と同じ順序で並ぶ
/// (読み手が観測と解釈を突き合わせられるようにするため)。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AssessedDetection {
    pub series: SeriesKey,
    pub route: DetectRoute,
    pub metric_label: &'static str,
    /// この検出の時間的な裏付け (どの検出を指しているかの同定にも使う)。
    pub support: TemporalSupport,
    /// 調査優先度 (**順序尺度。確率ではない**)。
    pub priority: Priority,
    /// この検出が宣言する下地。
    pub base_priority: Priority,
    /// 優先度をその値にした理由 (昇降の根拠)。
    pub priority_reasons: Vec<&'static str>,
    /// 根拠の充足度 (**優先度とは別**)。
    pub sufficiency: EvidenceSufficiency,
    /// 1 行の見出し。
    pub headline: String,
}

impl AssessedDetection {
    /// 検出 1 件に優先度と充足度を付ける。
    fn of(d: &Detection, th: &DetectThresholds) -> Self {
        let sufficiency = EvidenceSufficiency::of(d, th);
        let mut reasons: Vec<&'static str> = Vec::new();
        let mut priority = d.base_priority;

        // **その検出自身**が持続したときだけ上げる。
        // 別の検出の持続性を借りてはいけない。
        // 観測範囲の解釈は `TemporalSupport` に任せる (瞬時値と区間レートで
        // 範囲の意味が違うため、ここで秒数を組み立て直さない)。
        // 水準変化の後窓は検出の必要条件であり、追加の持続根拠ではない。
        if d.route() != DetectRoute::LevelShift
            && d.support.samples >= th.persistence_samples
            && d.support.span_secs() >= th.persistence_secs
        {
            priority = priority.up();
            reasons.push("この検出自身が複数回の採取にわたって続いた (+1 段)");
        }
        // 標本不足は**その判断が依存する根拠**にだけ効かせる。
        // 固定条件は比較基準を判定に使っていないので降格しない。
        if sufficiency.level == SufficiencyLevel::Thin
            && sufficiency.basis.depends_on_the_input_distribution()
        {
            priority = priority.down();
            reasons.push("この判断が依存する根拠の材料が乏しいので判断を控えた (−1 段)");
        }
        if reasons.is_empty() {
            reasons.push("昇降なし (この検出が宣言する下地のまま)");
        }

        Self {
            series: d.series.clone(),
            route: d.route(),
            metric_label: d.metric_label,
            support: d.support,
            priority,
            base_priority: d.base_priority,
            priority_reasons: reasons,
            sufficiency,
            headline: headline_of(d),
        }
    }
}

/// 検出 1 件の見出し。
fn headline_of(d: &Detection) -> String {
    format!(
        "{} [{}] {} — {}",
        d.metric_label,
        d.series.display(),
        d.pattern.label(),
        d.support.describe_span()
    )
}

/// エピソードに優先度と充足度を付けたもの。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AssessedEpisode {
    pub episode: Episode,
    /// 調査優先度 (**順序尺度。確率ではない**)。
    ///
    /// **検出ごとの優先度の最大値。** エピソードの中で最も先に見るべき
    /// 検出がどれか、という意味しか持たない。
    pub priority: Priority,
    /// 優先度を決めた検出が宣言する下地。
    pub base_priority: Priority,
    /// 優先度をその値にした理由 (昇降の根拠)。
    pub priority_reasons: Vec<&'static str>,
    /// 根拠の充足度 (**優先度とは別**)。優先度を決めた検出のもの。
    pub sufficiency: EvidenceSufficiency,
    /// エピソード内の充足度のばらつき。
    pub sufficiency_spread: SufficiencySpread,
    /// 1 行の見出し。**優先度を決めた検出**の見出し。
    pub headline: String,
    /// 最も長く続いた検出の見出し。見出しの検出と違うときだけ入る。
    pub longest_running_headline: Option<String>,
    /// 検出ごとの解釈 (`episode.detections` と同じ順序)。
    pub detections: Vec<AssessedDetection>,
    /// 当たった観点の名前。
    pub viewpoints: Vec<&'static str>,
    /// 水準変化と他の観点が**同じ系列・重なった時刻**で当たった系列。
    ///
    /// **優先度は上げない (規律 2′)。** 3 経路は相関するので、
    /// これを独立な裏付けとして数えない。観点が重なったことを示すだけ。
    pub corroborating_series: Vec<String>,
    /// 考えられる解釈 (重複なし)。
    pub possible_interpretations: Vec<&'static str>,
    /// この所見では確かめていないこと (重複なし)。
    pub not_established: Vec<&'static str>,
}

impl AssessedEpisode {
    fn of(episode: Episode, th: &DetectThresholds) -> Self {
        let assessed: Vec<AssessedDetection> = episode
            .detections
            .iter()
            .map(|d| AssessedDetection::of(d, th))
            .collect();

        // 優先度はエピソード内の最大。**見出しもその検出に揃える**
        // (優先度を出した検出と見出しの検出が違うと、読み手は別の根拠を読む)。
        let lead = assessed
            .iter()
            .max_by(|a, b| {
                a.priority
                    .cmp(&b.priority)
                    .then_with(|| a.base_priority.cmp(&b.base_priority))
                    .then_with(|| a.support.samples.cmp(&b.support.samples))
                    .then_with(|| a.support.span_secs().cmp(&b.support.span_secs()))
                    // 決定的にするため最後は系列名で決める
                    .then_with(|| b.series.cmp(&a.series))
            })
            .expect("エピソードは空でない");

        let levels: Vec<SufficiencyLevel> = assessed.iter().map(|a| a.sufficiency.level).collect();
        let spread = SufficiencySpread::of(lead.sufficiency.level, &levels);

        // 「最も先に見るべき検出」と「最も長く続いた検出」は別物になり得る。
        // 単発の検出が見出しになったとき、何が長く続いていたかを併記する。
        let longest = episode.longest();
        let longest_running_headline = (longest.series != lead.series
            || longest.route() != lead.route
            || longest.support != lead.support)
            .then(|| headline_of(longest));

        let priority = lead.priority;
        let base_priority = lead.base_priority;
        let priority_reasons = lead.priority_reasons.clone();
        let sufficiency = lead.sufficiency;
        let headline = lead.headline.clone();

        let viewpoints = episode
            .viewpoints
            .iter()
            .map(|r| r.label())
            .collect::<Vec<_>>();
        let corroborating_series = episode
            .series_with_corroborating_viewpoints()
            .iter()
            .map(SeriesKey::display)
            .collect();
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
            base_priority,
            priority_reasons,
            sufficiency,
            sufficiency_spread: spread,
            headline,
            longest_running_headline,
            detections: assessed,
            viewpoints,
            corroborating_series,
            possible_interpretations: interpretations,
            not_established,
        }
    }
}

/// 背景の所見 — 入力のほぼ全体を占め、「いつ」の手がかりを持たない検出。
///
/// 一日中スワップが使われている状態は異変ではあるが、
/// **入力のどこを切っても成立する**ので時刻を絞る材料にならない。
/// これをエピソードの軸にすると、午前の CPU 異変と夜の通信エラーが
/// 1 件へ融合して「いつ何が起きたか」が埋もれる
/// ([`crate::detect::episodes::split_standing`])。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BackgroundFinding {
    /// 検出 1 件の解釈 (優先度・充足度はエピソードと同じ規則で付ける)。
    pub finding: AssessedDetection,
    /// 入力全体の時間範囲に対する割合 (百分率)。
    pub share_of_input_percent: Option<u64>,
    /// 検出の全内容 (根拠つき)。
    pub detection: Detection,
}

impl BackgroundFinding {
    fn of(d: Detection, th: &DetectThresholds, input_span_secs: Option<u64>) -> Self {
        Self {
            finding: AssessedDetection::of(&d, th),
            share_of_input_percent: episodes::share_of_input_percent(&d.support, input_span_secs),
            detection: d,
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
    "エピソードは検出が**始まった時刻**でまとめている。長く続く検出は始まった時刻の\
     エピソードに 1 度だけ現れるので、後の時刻のエピソードを読むときは\
     それ以前から続いている所見も併せて見る必要がある",
    "エピソードの「根拠が及ぶ範囲」は互いに重なることがある。\
     範囲の包含は同一事象を意味しない",
    "入力のほぼ全体を占める検出は「いつ」の手がかりを持たないので、\
     エピソードではなく背景の所見として分けている。\
     **重要でないという意味ではない**",
];

/// 報告範囲の境界 (出力用の表記)。
///
/// [`crate::detect::ReportBound`] をそのまま載せない。
/// 検出層の設定型を出力契約に混ぜると、設定の追加が契約の変更になる。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReportBoundary {
    /// 指定なし。
    Unbounded,
    /// エポック秒。
    Epoch { ust: u64 },
    /// 毎日の時刻 (UTC)。
    TimeOfDay { hour: u8, min: u8, sec: u8 },
}

impl ReportBoundary {
    fn of(bound: ReportBound) -> Self {
        match bound {
            ReportBound::None => ReportBoundary::Unbounded,
            ReportBound::Epoch(ust) => ReportBoundary::Epoch { ust },
            ReportBound::TimeOfDay { hour, min, sec } => {
                ReportBoundary::TimeOfDay { hour, min, sec }
            }
        }
    }

    /// 1 行の表記。
    pub fn label(self) -> String {
        match self {
            ReportBoundary::Unbounded => "指定なし".to_string(),
            ReportBoundary::Epoch { ust } => format!("epoch {ust}"),
            ReportBoundary::TimeOfDay { hour, min, sec } => {
                format!("{hour:02}:{min:02}:{sec:02}")
            }
        }
    }
}

/// 何を報告対象にし、何を落としたか。
///
/// **入力全体の評価と報告対象の件数を区別する。** これが無いと
/// 「検出件数はあるのに `episodes` が空」の理由を結果単体で判断できない
/// (報告時間帯で落ちたのか、優先度の下限で落ちたのか、背景へ回ったのか)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ReportScope {
    /// 報告範囲の始点 (`--from`)。**基準の材料は絞らない。**
    pub from: ReportBoundary,
    /// 報告範囲の終点 (`--to`)。
    pub to: ReportBoundary,
    /// 適用した優先度の下限 (`--min-priority`)。
    pub min_priority: Priority,
    /// 入力全体で立った検出件数 (**報告範囲で絞る前**)。
    pub detections_in_input: usize,
    /// 報告範囲に入った検出件数。
    pub detections_in_report_window: usize,
    /// 背景の所見として別枠にした検出件数。
    pub background_findings: usize,
    /// まとめたエピソード件数 (優先度で絞る前)。
    pub episodes_before_priority_filter: usize,
    /// 優先度の下限で落としたエピソード件数。
    pub episodes_excluded_by_priority: usize,
    /// 優先度の下限で落とした背景の所見の件数。
    pub background_excluded_by_priority: usize,
}

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
    /// 1 つのエピソードに入る「始まり」の広がりの上限 (秒)。
    pub episode_onset_span_cap_secs: u64,
    /// 背景の所見へ回す、入力全体に対する時間範囲の割合 (百分率)。
    pub standing_span_percent: u64,
    /// 採取間隔の代表値 (秒)。
    pub interval_p90_secs: Option<u64>,
    pub source: SummarySource,
    pub period: PeriodBounds,
    /// 何を報告対象にし、何を落としたか。
    pub report_scope: ReportScope,
    /// 所見 (時刻順)。
    pub episodes: Vec<AssessedEpisode>,
    /// 背景の所見 (入力のほぼ全体を占め、「いつ」の手がかりを持たないもの)。
    pub background: Vec<BackgroundFinding>,
    /// 何を評価でき、何を評価できなかったか。
    pub coverage: EvaluationCoverage,
    pub notes: &'static [&'static str],
}

impl Assessment {
    /// 優先度の下限で絞る。
    ///
    /// **落とした件数と適用した下限を残す。** 結果だけを見て
    /// 「エピソードが空なのは絞ったからか、検出が無かったからか」を
    /// 判断できるようにするため。
    pub fn filter_priority(&mut self, min: Priority) {
        let before = self.episodes.len();
        self.episodes.retain(|e| e.priority >= min);
        let background_before = self.background.len();
        self.background.retain(|b| b.finding.priority >= min);
        self.report_scope.min_priority = min;
        self.report_scope.episodes_excluded_by_priority = before - self.episodes.len();
        self.report_scope.background_excluded_by_priority =
            background_before - self.background.len();
    }

    /// 最も高い優先度。
    pub fn top_priority(&self) -> Option<Priority> {
        self.episodes
            .iter()
            .map(|e| e.priority)
            .chain(self.background.iter().map(|b| b.finding.priority))
            .max()
    }

    /// 報告対象のエピソードに入っている検出件数。
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
    // 入力全体の時間範囲。「入力のほぼ全体を占める」の分母になる
    let input_span = match (period.first_ust, period.last_ust) {
        (Some(a), Some(b)) if b > a => Some(b - a),
        _ => None,
    };
    let rules = episodes::GroupRules::new(gap, input_span);
    let coverage = EvaluationCoverage::of(outcome.evaluations);

    let detections_in_report_window = outcome.detections.len();
    // 恒常的な状態を先に分ける。**それを軸にエピソードを作らない**
    let (standing, local) = episodes::split_standing(
        outcome.detections,
        input_span,
        episodes::STANDING_SPAN_PERCENT,
    );
    let background: Vec<BackgroundFinding> = standing
        .into_iter()
        .map(|d| BackgroundFinding::of(d, &th, input_span))
        .collect();
    let grouped = episodes::group(local, rules, &outcome.discontinuity_marks);
    let episodes: Vec<AssessedEpisode> = grouped
        .into_iter()
        .map(|e| AssessedEpisode::of(e, &th))
        .collect();

    let report_scope = ReportScope {
        from: ReportBoundary::of(opts.report_from),
        to: ReportBoundary::of(opts.report_to),
        // `filter_priority` が呼ばれるまでは何も絞っていない
        min_priority: Priority::Informational,
        detections_in_input: coverage.fixed_condition.detections
            + coverage.robust_deviation.detections
            + coverage.level_shift.detections,
        detections_in_report_window,
        background_findings: background.len(),
        episodes_before_priority_filter: episodes.len(),
        episodes_excluded_by_priority: 0,
        background_excluded_by_priority: 0,
    };

    Assessment {
        schema_version: DETECT_SCHEMA_VERSION,
        assessment_kind: ASSESSMENT_KIND,
        detector_version: DETECTOR_VERSION,
        thresholds: th,
        baseline_basis: basis_of(opts),
        episode_gap_secs: gap,
        episode_onset_span_cap_secs: rules.onset_span_cap_secs,
        standing_span_percent: episodes::STANDING_SPAN_PERCENT,
        interval_p90_secs: outcome.interval_p90_secs,
        source,
        period,
        report_scope,
        episodes,
        background,
        coverage,
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
        // **「MAD の N 倍」と書かない。** 散らばりが測れなかったので
        // 倍数は存在しない (`DecisionBasis::AbsoluteDeparture` の方針)。
        DecisionBasis::AbsoluteDeparture {
            reference,
            dispersion,
            min_absolute_deviation,
            peak_absolute_deviation,
            direction,
            ..
        } => format!(
            "{} が比較基準 (中央値 {:.2}{unit}) から{}側へ {:.2}{unit} 離れた \
             (散らばりが測れないため絶対差で判断した: {}、要 {:.2}{unit} 以上。{}、\
             最小 {:.2} / 最大 {:.2})",
            d.metric_label,
            reference,
            direction.as_str(),
            peak_absolute_deviation,
            dispersion.label(),
            min_absolute_deviation,
            d.support.describe_span(),
            d.decision.min,
            d.decision.max
        ),
        DecisionBasis::LevelShift {
            before_median,
            after_median,
            shift,
            trend_explained_shift,
            step_shift,
            normalized_shift,
            before,
            after,
            ..
        } => {
            // **正規化しているのは段差 (`step_shift`) であって観測差ではない。**
            let norm = match normalized_shift {
                Some(n) => format!("段差は散らばりの {n:.1} 倍"),
                None => "散らばりが測れないため正規化なし".to_string(),
            };
            format!(
                "{} の水準が前後の窓で {:.2}{unit} から {:.2}{unit} へ {:+.2}{unit} 違う \
                 (うち窓内の傾向で説明できる差 {:+.2}{unit} / 残る段差 {:+.2}{unit}、{norm}。\
                 前: {} / 後: {})",
                d.metric_label,
                before_median,
                after_median,
                shift,
                trend_explained_shift,
                step_shift,
                before.describe_span(),
                after.describe_span()
            )
        }
    }
}

/// 所見全体を 1 行で要約する。
///
/// **絞り込みで空になったのか、検出が無かったのかを混ぜない。**
/// 件数は [`Assessment::report_scope`] から採る。
pub fn describe_assessment(a: &Assessment) -> String {
    let s = &a.report_scope;
    let mut text = match a.top_priority() {
        None => format!(
            "エピソードなし (入力全体の検出 {} 件 / 報告範囲の検出 {} 件 / \
             優先度 {} 未満で除外したエピソード {} 件)",
            s.detections_in_input,
            s.detections_in_report_window,
            s.min_priority.label(),
            s.episodes_excluded_by_priority
        ),
        Some(p) => format!(
            "エピソード {} 件 (最高優先度: {})、検出 {} 件、背景の所見 {} 件、\
             評価できた系列 {} / 入力にあった系列 {}",
            a.episodes.len(),
            p.label(),
            a.detection_count(),
            a.background.len(),
            a.coverage.series_evaluated,
            a.coverage.series_present
        ),
    };
    if s.episodes_excluded_by_priority > 0 || s.background_excluded_by_priority > 0 {
        text.push_str(&format!(
            "。優先度 {} 未満で除外: エピソード {} 件 / 背景の所見 {} 件",
            s.min_priority.label(),
            s.episodes_excluded_by_priority,
            s.background_excluded_by_priority
        ));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze::timeline::{MetricKey, Timelines};
    use crate::detect::testing::*;
    use crate::detect::{
        BaselineScope, DecisionEvidence, FixedComparison, ObservationOrigin, Pattern, ReportBound,
        ShiftDirection,
    };
    use crate::model::{ActivityId, Unit, ValueKind};

    /// 手で組んだ検出。
    ///
    /// **カタログの下地や閾値に依存させない。** 優先度の算出単位
    /// (Issue #5 ⑩⑪⑫) を確かめるテストは、カタログの値が変わっても
    /// 壊れてはいけない。
    #[allow(clippy::too_many_arguments)]
    fn detection(
        column: &str,
        route: DetectRoute,
        start: u64,
        end: u64,
        samples: u64,
        baseline_samples: u64,
        base_priority: Priority,
        leaning: bool,
    ) -> Detection {
        let basis = match route {
            DetectRoute::FixedCondition => DecisionBasis::FixedCondition {
                condition_id: "test",
                comparison: FixedComparison::Above,
                threshold: 0.0,
                min_samples: 1,
                rationale: "テスト用の固定条件",
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
                pooled_mad: Some(1.0),
                normalized_shift: Some(5.0),
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
        Detection {
            detector_version: DETECTOR_VERSION,
            series: SeriesKey::from_metric(&MetricKey::new(ActivityId::CPU, "all", column)),
            metric_label: "テスト指標",
            unit: Unit::Percent,
            kind: ValueKind::Counter,
            origin: ObservationOrigin::IntervalRate,
            pattern: Pattern::Sustained,
            support: TemporalSupport {
                start_ust: start,
                end_ust: end,
                samples,
                ..Default::default()
            },
            baseline: BaselineEvidence {
                basis: BasisOrigin::InputItself,
                samples: baseline_samples,
                support: TemporalSupport::default(),
                median: Some(1.0),
                mad: Some(1.0),
                dispersion: Dispersion::Measured,
                median_within_fixed_condition: leaning,
                flagged_share: if leaning { 1.0 } else { 0.0 },
                caveats: Vec::new(),
            },
            decision: DecisionEvidence::new(basis, &[]),
            base_priority,
            possible_interpretations: &[],
            not_established: &[],
        }
    }

    /// 手で組んだ検出から所見を作る。
    fn assess_detections(detections: Vec<Detection>, period: PeriodBounds) -> Assessment {
        let outcome = DetectOutcome {
            detections,
            evaluations: Vec::new(),
            interval_p90_secs: Some(600),
            discontinuity_marks: Vec::new(),
        };
        assess(
            outcome,
            SummarySource::default(),
            period,
            &DetectOptions::default(),
        )
    }

    /// 1 日ぶんの期間 (背景の切り出しの分母になる)。
    fn one_day() -> PeriodBounds {
        PeriodBounds {
            first_ust: Some(T0),
            last_ust: Some(T0 + 86_400),
            samples: 144,
            ..Default::default()
        }
    }

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
        // %idle が 4 回 (40 分) 続けて 2% → 持続性で 1 段上がる
        let mut v = vec![80.0; 20];
        v[10] = 2.0;
        v[11] = 2.0;
        v[12] = 2.0;
        v[13] = 2.0;
        let ts = single(cpu_idle(&vals(&v)));
        let a = assess_timelines(&ts, &DetectOptions::default());
        let ep = a.episodes.first().expect("エピソード");
        // 下地の絶対値はカタログの宣言なのでここでは固定しない。
        // **昇格が起きたこと**と、その理由が「この検出自身の持続」であることを見る
        assert_eq!(ep.priority, ep.base_priority.up());
        assert!(
            ep.priority_reasons
                .iter()
                .any(|r| r.contains("この検出自身が複数回の採取にわたって続いた"))
        );
        // 経路の数で上げていない: 3 経路当たっても昇格は 1 段まで
        assert!(ep.episode.viewpoints.len() > 1);
        assert!(ep.priority <= ep.base_priority.up());
    }

    /// 別の検出の持続性を借りて昇格しない (Issue #5 ⑩)。
    ///
    /// 単発の検出と長時間続いた低優先度の所見が同じエピソードに入っても、
    /// 単発の検出が「持続した」ことにはならない。
    #[test]
    fn a_detection_never_borrows_another_detections_persistence() {
        let th = DetectThresholds::default();
        // 単発 (1 採取) の高めの下地
        let one_shot = detection(
            "pgscand",
            DetectRoute::FixedCondition,
            T0,
            T0 + 600,
            1,
            144,
            Priority::Watch,
            false,
        );
        // 同じ時刻から長時間続く低い下地
        let long = detection(
            "idle",
            DetectRoute::FixedCondition,
            T0,
            T0 + 86_400,
            144,
            144,
            Priority::Informational,
            false,
        );
        assert!(long.support.samples >= th.persistence_samples);
        assert!(long.support.span_secs() >= th.persistence_secs);

        // 入力長を渡さなければ背景へは回らない (結合の挙動だけを見る)
        let a = assess_detections(vec![one_shot, long], PeriodBounds::default());
        let ep = a.episodes.first().expect("エピソード");
        let shot = ep
            .detections
            .iter()
            .find(|d| d.series.column == "pgscand")
            .expect("単発の検出");
        assert_eq!(
            shot.priority,
            Priority::Watch,
            "単発の検出は昇格しない: {:?}",
            shot.priority_reasons
        );
        let long = ep
            .detections
            .iter()
            .find(|d| d.series.column == "idle")
            .expect("長い検出");
        assert_eq!(long.priority, Priority::Watch, "長い検出自身は昇格する");
        // エピソードの優先度は検出ごとの最大で、見出しはその検出に揃う。
        // 同じ優先度なら下地の高い方 (= 借り物で上がっていない方) が代表になる
        assert_eq!(ep.priority, Priority::Watch);
        assert_eq!(ep.base_priority, Priority::Watch);
        assert!(
            ep.headline.contains("pgscand"),
            "見出しは優先度を決めた検出のもの: {}",
            ep.headline
        );
        // 最も長く続いた検出が見出しと違うなら併記する
        assert!(ep.longest_running_headline.is_some());
    }

    /// 第 2 経路 (同じ系列の水準変化) では昇格しない (Issue #5 ⑩ / 規律 2′)。
    #[test]
    fn a_second_viewpoint_is_shown_but_never_escalates() {
        let shift = detection(
            "idle",
            DetectRoute::LevelShift,
            T0,
            T0 + 600,
            1,
            144,
            Priority::Watch,
            false,
        );
        let dev = detection(
            "idle",
            DetectRoute::RobustDeviation,
            T0,
            T0 + 600,
            1,
            144,
            Priority::Watch,
            false,
        );
        let a = assess_detections(vec![shift, dev], PeriodBounds::default());
        let ep = a.episodes.first().expect("エピソード");
        assert_eq!(
            ep.priority,
            Priority::Watch,
            "観点が重なっても上げない: {:?}",
            ep.priority_reasons
        );
        assert_eq!(
            ep.corroborating_series,
            vec!["A_CPU/all/idle".to_string()],
            "重なったことは示す"
        );
        assert!(
            ep.priority_reasons.iter().all(|r| !r.contains("水準変化")),
            "昇格理由に第 2 経路を書かない: {:?}",
            ep.priority_reasons
        );
    }

    /// 固定条件は比較基準の標本不足では降格しない (Issue #5 ⑪)。
    ///
    /// `threshold::detect` は baseline を判定に使っていないので、
    /// 基準の材料が乏しいことはその観測の確かさと関係がない。
    #[test]
    fn a_fixed_condition_is_not_demoted_by_a_thin_comparison_basis() {
        // 6 点だけ。基準を作るサンプル (既定 12) に届かない
        let ts = single(cpu_idle(&vals(&[80.0, 80.0, 2.0, 2.0, 80.0, 80.0])));
        let a = assess_timelines(&ts, &DetectOptions::default());
        let ep = a.episodes.first().expect("エピソード");
        let fixed = ep
            .detections
            .iter()
            .find(|d| d.route == DetectRoute::FixedCondition)
            .expect("固定条件の検出");
        assert_eq!(
            fixed.sufficiency.basis,
            SufficiencyBasis::ObservedSamplesOnly
        );
        assert!(!fixed.sufficiency.basis.depends_on_the_input_distribution());
        assert_eq!(
            fixed.priority, fixed.base_priority,
            "分布に依存しない判断を分布の標本不足で下げない: {:?}",
            fixed.priority_reasons
        );
        // 基準の材料が乏しいことは充足度の別フィールドに残る
        assert!(fixed.sufficiency.baseline_samples < a.thresholds.min_baseline_samples);
    }

    /// 分布に依存する判断は材料が乏しければ 1 段下げる (Issue #5 ⑪)。
    #[test]
    fn a_distribution_dependent_route_is_demoted_when_its_basis_is_thin() {
        let dev = detection(
            "idle",
            DetectRoute::RobustDeviation,
            T0,
            T0 + 600,
            1,
            // 基準の材料が既定の下限 (12) に届かない
            2,
            Priority::Watch,
            false,
        );
        let a = assess_detections(vec![dev], PeriodBounds::default());
        let d = &a.episodes[0].detections[0];
        assert_eq!(d.sufficiency.level, SufficiencyLevel::Thin);
        assert_eq!(d.sufficiency.basis, SufficiencyBasis::ComparisonBasis);
        assert_eq!(d.priority, Priority::Informational);
        assert!(
            d.priority_reasons
                .iter()
                .any(|r| r.contains("この判断が依存する根拠の材料が乏しい"))
        );
    }

    /// 充足度は検出・系列・経路ごとに保持する (Issue #5 ⑫)。
    ///
    /// 別系列に 144 点あっても、2 点しかない系列の充足度は「乏しい」。
    #[test]
    fn sufficiency_is_kept_per_detection_not_maxed_over_the_episode() {
        let thin = detection(
            "iowait",
            DetectRoute::RobustDeviation,
            T0,
            T0 + 600,
            1,
            2,
            Priority::Watch,
            false,
        );
        let rich = detection(
            "idle",
            DetectRoute::RobustDeviation,
            T0,
            T0 + 600,
            1,
            144,
            Priority::Watch,
            false,
        );
        let a = assess_detections(vec![thin, rich], PeriodBounds::default());
        let ep = a.episodes.first().expect("エピソード");
        let thin = ep
            .detections
            .iter()
            .find(|d| d.series.column == "iowait")
            .expect("材料の乏しい検出");
        assert_eq!(
            thin.sufficiency.level,
            SufficiencyLevel::Thin,
            "別系列の点数で「十分」にしない"
        );
        assert_eq!(thin.sufficiency.baseline_samples, 2, "分母も同じ系列のもの");
        let rich = ep
            .detections
            .iter()
            .find(|d| d.series.column == "idle")
            .expect("材料の十分な検出");
        assert_eq!(rich.sufficiency.level, SufficiencyLevel::Adequate);
        // エピソードは代表を出しつつ混在を示す
        assert!(ep.sufficiency_spread.mixed);
        assert_eq!(ep.sufficiency_spread.lowest, SufficiencyLevel::Thin);
        assert_eq!(ep.sufficiency_spread.highest, SufficiencyLevel::Adequate);
        assert!(ep.sufficiency_spread.label().contains("混在"));
    }

    /// 水準変化の充足度は局所窓の点数で測る (Issue #5 ⑫)。
    #[test]
    fn a_level_shift_is_judged_by_its_local_windows() {
        // 入力全体は 40 点あるが、窓は既定の下限 5 点
        let mut v: Vec<f64> = vec![2.0; 20];
        v.extend(vec![40.0; 20]);
        let ts = single(runq(&vals(&v)));
        let a = assess_timelines(&ts, &DetectOptions::default());
        let shift = a
            .episodes
            .iter()
            .flat_map(|e| e.detections.iter())
            .find(|d| d.route == DetectRoute::LevelShift)
            .expect("水準変化の検出");
        assert_eq!(shift.sufficiency.basis, SufficiencyBasis::LocalWindows);
        assert_eq!(
            shift.sufficiency.material_samples, a.thresholds.shift_window_min_samples as u64,
            "入力全体の点数ではなく窓の点数"
        );
        assert!(shift.sufficiency.material_samples < shift.sufficiency.baseline_samples);
        assert_eq!(shift.sufficiency.level, SufficiencyLevel::Moderate);
    }

    #[test]
    fn a_level_shift_window_does_not_escalate_priority_at_any_sampling_interval() {
        for interval in [60, 600] {
            let mut v = vec![90.0; 100];
            v.extend(vec![60.0; 100]);
            let mut timeline = cpu_idle(&vals(&v));
            for (i, point) in timeline.points.iter_mut().enumerate() {
                point.start_ust = T0 + i as u64 * interval;
                point.end_ust = T0 + (i as u64 + 1) * interval;
                point.elapsed_cs = interval * 100;
            }
            let a = assess_timelines(&single(timeline), &DetectOptions::default());
            let shift = a
                .episodes
                .iter()
                .flat_map(|e| &e.detections)
                .find(|d| d.route == DetectRoute::LevelShift)
                .expect("段差");
            assert_eq!(shift.priority, Priority::Watch, "採取間隔 {interval}");
            assert!(a.coverage.level_shift.blind_edge_samples > 0);
            assert!(a.coverage.level_shift.series_with_blind_edges > 0);
        }
    }

    #[test]
    fn missing_share_uses_the_series_observations_for_every_route() {
        let mut points = vec![P::V(90.0); 144];
        points[10] = P::Missing;
        for point in &mut points[60..66] {
            *point = P::V(1.0);
        }
        let a = assess_timelines(&single(cpu_idle(&points)), &DetectOptions::default());
        let fixed = a
            .episodes
            .iter()
            .flat_map(|e| &e.detections)
            .find(|d| d.route == DetectRoute::FixedCondition)
            .expect("固定条件");
        assert_eq!(fixed.support.series_observed_samples, 143);
        assert_eq!(fixed.sufficiency.missing_samples, 1);
        assert_eq!(fixed.sufficiency.material_samples, 6);
        assert_eq!(fixed.sufficiency.level, SufficiencyLevel::Adequate);
    }

    /// 一日続く条件を背景の所見として分ける (Issue #5 ⑨)。
    #[test]
    fn a_standing_condition_becomes_a_background_finding() {
        let all_day = detection(
            "swpused_pct",
            DetectRoute::FixedCondition,
            T0,
            T0 + 86_400,
            144,
            144,
            Priority::Watch,
            true,
        );
        let morning = detection(
            "idle",
            DetectRoute::FixedCondition,
            T0 + 10_800,
            T0 + 11_400,
            1,
            144,
            Priority::Watch,
            false,
        );
        let night = detection(
            "iowait",
            DetectRoute::FixedCondition,
            T0 + 72_000,
            T0 + 72_600,
            1,
            144,
            Priority::Watch,
            false,
        );
        let a = assess_detections(vec![all_day, morning, night], one_day());
        assert_eq!(a.background.len(), 1, "一日続く条件は背景へ");
        assert_eq!(a.background[0].share_of_input_percent, Some(100));
        assert!(a.background[0].finding.headline.contains("swpused_pct"));
        assert_eq!(
            a.episodes.len(),
            2,
            "午前と夜の異変を 1 件に融合しない: {:?}",
            a.episodes.iter().map(|e| &e.headline).collect::<Vec<_>>()
        );
        // 背景の所見も優先度・充足度を持つ (黙って落とさない)。
        // 判定の規則はエピソードと同じ。一日続いた検出は「その検出自身が持続した」
        // ので 1 段上がる (背景だからといって別の規則を持ち込まない)
        assert_eq!(a.background[0].finding.priority, Priority::Investigate);
        assert!(
            a.background[0]
                .finding
                .priority_reasons
                .iter()
                .any(|r| r.contains("この検出自身"))
        );
        assert!(
            a.background[0]
                .finding
                .sufficiency
                .basis_may_reflect_the_anomaly
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

    /// 何を絞ったかを結果に残す (Issue #5 ㉑)。
    ///
    /// 「検出件数はあるのに episodes が空」の理由を、結果単体で判断できること。
    #[test]
    fn the_report_scope_records_what_was_filtered_out() {
        let mut v = vec![0.0; 30];
        v[10] = 5.0;
        let ts = single(timeline(
            MetricKey::new(ActivityId::PAGE, "-", "pgscank"),
            Unit::CountPerSec,
            ValueKind::Counter,
            &vals(&v),
        ));
        // T0 は 00:00:00 UTC。検出を落とさない境界を指定する
        let opts = DetectOptions {
            report_from: ReportBound::TimeOfDay {
                hour: 0,
                min: 0,
                sec: 0,
            },
            report_to: ReportBound::Epoch(T0 + 86_400),
            ..Default::default()
        };
        let mut a = assess_timelines(&ts, &opts);
        let s = a.report_scope;
        // 報告時間帯が結果に載る
        assert_eq!(
            s.from,
            ReportBoundary::TimeOfDay {
                hour: 0,
                min: 0,
                sec: 0
            }
        );
        assert_eq!(s.to, ReportBoundary::Epoch { ust: T0 + 86_400 });
        // 入力全体の評価と報告対象の件数を区別する
        assert!(s.detections_in_input >= s.detections_in_report_window);
        assert_eq!(s.min_priority, Priority::Informational, "まだ絞っていない");
        assert_eq!(s.episodes_excluded_by_priority, 0);
        let before = a.episodes.len();
        assert!(before > 0);

        a.filter_priority(Priority::Investigate);
        assert!(a.episodes.is_empty());
        assert_eq!(a.report_scope.min_priority, Priority::Investigate);
        assert_eq!(a.report_scope.episodes_excluded_by_priority, before);
        assert_eq!(a.report_scope.episodes_before_priority_filter, before);
        // 1 行の要約も「絞って空になった」と書く
        let text = describe_assessment(&a);
        assert!(text.contains("優先度"), "{text}");
        assert!(text.contains("除外"), "{text}");
    }

    /// 報告範囲で落ちた検出と、優先度で落ちたエピソードを混ぜない (Issue #5 ㉑)。
    #[test]
    fn detections_outside_the_report_window_are_counted_separately() {
        let mut v = vec![0.0; 30];
        v[2] = 5.0;
        let ts = single(timeline(
            MetricKey::new(ActivityId::PAGE, "-", "pgscank"),
            Unit::CountPerSec,
            ValueKind::Counter,
            &vals(&v),
        ));
        // 検出の時刻より後ろだけを報告範囲にする
        let opts = DetectOptions {
            report_from: ReportBound::Epoch(T0 + 20 * 600),
            ..Default::default()
        };
        let a = assess_timelines(&ts, &opts);
        let s = a.report_scope;
        assert!(s.detections_in_input > 0, "入力全体では検出があった");
        assert_eq!(s.detections_in_report_window, 0, "報告範囲には無い");
        assert!(a.episodes.is_empty());
        assert_eq!(
            s.episodes_excluded_by_priority, 0,
            "優先度で落ちたのではない"
        );
    }

    #[test]
    fn standing_notes_state_the_limits() {
        let ts = single(cpu_idle(&vals(&[50.0; 20])));
        let a = assess_timelines(&ts, &DetectOptions::default());
        assert!(a.notes.iter().any(|n| n.contains("外部の正常値ではない")));
        assert!(a.notes.iter().any(|n| n.contains("確率や確信度は出さない")));
        assert!(a.notes.iter().any(|n| n.contains("独立ではない")));
        assert!(a.notes.iter().any(|n| n.contains("離散的な採取")));
        // まとめ方の前提も前提として出す (読み方が変わるため)
        assert!(a.notes.iter().any(|n| n.contains("始まった時刻")));
        assert!(
            a.notes
                .iter()
                .any(|n| n.contains("範囲の包含は同一事象を意味しない"))
        );
        assert!(
            a.notes
                .iter()
                .any(|n| n.contains("重要でないという意味ではない"))
        );
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

//! 異変検出 — 「**いつ・何に**異変があったか当たりを付ける」層。
//!
//! `analyze::rules` が「あらかじめ決めた症状が観測されたか」を判定するのに対し、
//! この層は**症状の一覧を持たずに**入力の中で目立つ点を拾う。
//! `sa` ファイルを 1 本渡されただけで当たりを付けたい、という用途に応える。
//!
//! # 確率を出さない
//!
//! 「確信度 92%」のような数字は**出さない**。単一ホストの短い時系列
//! (既定の採取間隔なら 1 日 144 点) から較正された確率は作れず、
//! 数字があると読み手はそれを信じてしまう。代わりに
//!
//! - **調査優先度** ([`crate::analyze::assessment::Priority`]) — 順序尺度
//! - **根拠の充足度** ([`crate::analyze::assessment::EvidenceSufficiency`]) — 何サンプル使えたか
//!
//! を**別のフィールド**として持つ。両者を 1 つのスコアへ潰さない。
//!
//! # 3 つの観点を併用し、互いを実行条件にしない
//!
//! | 観点 | 経路 | 何を見るか |
//! |---|---|---|
//! | 絶対水準 | [`threshold`] | 意味が確立している絶対値 |
//! | 参照分布からの逸脱 | [`robust`] | median と MAD からの偏り |
//! | 時間的変化 | [`level_shift`] | 前後の窓の水準差 |
//!
//! **片方の経路が他方の実行条件になってはいけない。** 固定条件を満たさないと
//! 逸脱検出が走らない作りにすると、閾値の外側にある異変を取りこぼす。
//! 3 経路は独立に走らせ、[`episodes`] で近接するものをまとめて提示する。
//!
//! **3 経路は統計的に独立ではない。** `%idle` が 5% を下回れば
//! 固定条件も逸脱も水準変化も同時に鳴りやすい。したがって
//! 「複数経路がヒットしたから確からしい」とは扱わない
//! (`analyze::assessment` の優先度は**持続性**で 1 段上げる)。
//! 出力では「独立」「直交」という語を使わず、3 つの**観点**として並べるだけにする。
//!
//! ```mermaid
//! flowchart LR
//!     TL["Timelines<br/>(analyze::summary が作る区間値)"]
//!     PREP["PreparedSeries<br/>不連続で切った連続区間"]
//!     F["threshold<br/>絶対水準"]
//!     R["robust<br/>参照分布からの逸脱"]
//!     L["level_shift<br/>時間的変化"]
//!     D["Detection"]
//!     E["Episode<br/>(episodes)"]
//!     A["Assessment<br/>(analyze::assessment)"]
//!     TL --> PREP
//!     PREP --> F --> D
//!     PREP --> R --> D
//!     PREP --> L --> D
//!     D --> E --> A
//! ```
//!
//! # 基準を「正常値」と呼ばない
//!
//! 同一ファイルから median / MAD を作ると、**異常が長時間を占めていれば
//! 基準もその状態に寄る**。したがってこれを `normal` / `baseline_normal` と
//! 名付けない。**比較基準** ([`BaselineEvidence`]) と呼び、
//! [`BasisOrigin`] で「この基準は入力自身から作った」ことを型に明示する。
//! 基準そのものが固定条件を満たしている場合は
//! [`BaselineEvidence::median_within_fixed_condition`] が立つ。
//!
//! # 採取間隔を偽らない
//!
//! Gauge が 3 回高かったことを「20 分間高止まり」と書いてはいけない。
//! `sar` のデータは**離散的な採取**である。[`TemporalSupport`] は
//! 時間範囲と採取回数の**両方**を持ち、[`TemporalSupport::describe_span`] は
//! 「20 分にわたる 3 回の採取」の形の文を返す。

pub mod episodes;
pub mod level_shift;
pub mod robust;
pub mod threshold;

use serde::Serialize;

use crate::analyze::assessment::SeriesEvaluation;
use crate::analyze::metric_catalog::{self, CatalogEntry, ShiftMagnitude};
use crate::analyze::timeline::{MetricKey, MetricTimeline, Timelines};
use crate::model::{ActivityId, Unit, ValueKind};

/// 検出出力のスキーマ版。出力契約として固定する (`docs/design.md` §11)。
pub const DETECT_SCHEMA_VERSION: &str = "1";

/// 検出器の版。閾値や手順を変えたら上げる。
pub const DETECTOR_VERSION: &str = "resarch-detect/1";

/// MAD を正規分布の標準偏差に合わせる係数。
///
/// **使うのは水準変化の正規化だけ。** 逸脱検出では使わない。
///
/// 逸脱スコアを `(x - median) / (1.4826 × MAD)` と書くと
/// 「σ 何個ぶん」に見えてしまうが、レート系列は右に裾が長く
/// 正規分布ではないので、その読み方は成立しない。
/// 逸脱は**生の MAD に対する比** ([`DetectThresholds::deviation_ratio`]) で表し、
/// `z` という記号も使わない。
///
/// 水準変化では 2 つの窓の残差を束ねた MAD を尺度にするため、
/// 「散らばり 1 つぶんに対して段差が何倍か」という読み方ができる。
/// そこでのみ係数を掛けて、閾値を散らばりの倍数として表す。
pub const MAD_SCALE: f64 = 1.4826;

// ===========================================================================
// 系列の同定
// ===========================================================================

/// 何の系列か (activity / item / column)。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct SeriesKey {
    pub activity: ActivityId,
    /// `A_CPU` のようなシンボル名 (未知 activity は数値表記)。
    pub activity_name: String,
    /// item のラベル (`all` / `dev8-0` / `eth0` / `-`)。
    pub item: String,
    /// 独自出力での列名。
    pub column: String,
}

impl SeriesKey {
    pub fn from_metric(key: &MetricKey) -> Self {
        Self {
            activity: key.activity,
            activity_name: key.activity.display_name(),
            item: key.item.clone(),
            column: key.column.clone(),
        }
    }

    /// 診断・出力用の表記 (`A_CPU/all/idle` の形)。
    pub fn display(&self) -> String {
        format!("{}/{}/{}", self.activity_name, self.item, self.column)
    }
}

// ===========================================================================
// 時間的な裏付け
// ===========================================================================

/// 時間範囲 + 採取回数 + 欠測数。
///
/// **採取回数と時間範囲を必ず一緒に持つ。** 片方だけを出すと
/// 「3 回高かった」が「20 分間高止まりした」に化ける。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct TemporalSupport {
    /// 最初の区間の始点 (エポック秒)。
    pub start_ust: u64,
    /// 最後の区間の終点 (エポック秒)。
    pub end_ust: u64,
    /// 値が得られた採取回数。
    pub samples: u64,
    /// 系列全体で値が得られなかった採取回数。
    pub missing_samples: u64,
    /// 系列全体で不連続として捨てた区間数。
    pub discontinuities: u64,
    /// 値が得られた区間の長さの合計 (1/100 秒)。
    pub observed_cs: u64,
}

impl TemporalSupport {
    /// 時間範囲の長さ (秒)。**採取が連続していたことを意味しない。**
    pub fn span_secs(&self) -> u64 {
        self.end_ust.saturating_sub(self.start_ust)
    }

    /// 「20 分にわたる 3 回の採取」の形の文。
    ///
    /// **「20 分間」とは書かない。** `sar` のデータは離散的な採取なので、
    /// 採取と採取の間に何が起きていたかは観測されていない。
    pub fn describe_span(&self) -> String {
        let secs = self.span_secs();
        if secs == 0 {
            return format!("1 時点の {} 回の採取", self.samples);
        }
        format!(
            "{}にわたる {} 回の採取",
            describe_duration(secs),
            self.samples
        )
    }

    /// 2 つの裏付けを合併する。
    ///
    /// 欠測・不連続は系列全体の値なので**大きい方を採る** (足すと二重計上になる)。
    pub fn merge(&self, other: &TemporalSupport) -> TemporalSupport {
        if self.samples == 0 && self.observed_cs == 0 {
            return *other;
        }
        if other.samples == 0 && other.observed_cs == 0 {
            return *self;
        }
        TemporalSupport {
            start_ust: self.start_ust.min(other.start_ust),
            end_ust: self.end_ust.max(other.end_ust),
            samples: self.samples.max(other.samples),
            missing_samples: self.missing_samples.max(other.missing_samples),
            discontinuities: self.discontinuities.max(other.discontinuities),
            observed_cs: self.observed_cs.max(other.observed_cs),
        }
    }
}

/// 秒数を日本語の期間表記へ。
///
/// 書式化だが、文の意味 (採取回数との併記) を壊されないよう
/// [`TemporalSupport::describe_span`] と同じ場所に置く。
pub fn describe_duration(secs: u64) -> String {
    if secs < 60 {
        return format!("{secs} 秒");
    }
    if secs < 3600 {
        return format!("{} 分", secs / 60);
    }
    if secs.is_multiple_of(3600) {
        return format!("{} 時間", secs / 3600);
    }
    format!("{} 時間 {} 分", secs / 3600, (secs % 3600) / 60)
}

// ===========================================================================
// 観測
// ===========================================================================

/// 値の由来。Counter 由来のレートと Gauge の瞬時値を混ぜない。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationOrigin {
    /// 2 サンプル間の差分を経過時間で割ったレート (Counter 由来)。
    ///
    /// 値は区間を代表する。区間の内側でどう変動したかは観測されていない。
    IntervalRate,
    /// その時点の瞬時値 (Gauge 由来)。
    ///
    /// 前後の採取の間の値は観測されていない。
    InstantGauge,
}

impl ObservationOrigin {
    /// 列の性質から決める。
    pub const fn of(kind: ValueKind) -> Self {
        match kind {
            ValueKind::Counter => ObservationOrigin::IntervalRate,
            // Identity 列は検出対象にしない (カタログのテストで固定してある)
            ValueKind::Gauge | ValueKind::Identity => ObservationOrigin::InstantGauge,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            ObservationOrigin::IntervalRate => "区間レート (Counter 由来)",
            ObservationOrigin::InstantGauge => "採取時点の値 (Gauge)",
        }
    }
}

/// 1 点の観測 (時刻 + 値 + 由来)。
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Observation {
    /// 区間の始点 (エポック秒)。Gauge では前サンプルの時刻。
    pub start_ust: u64,
    /// 区間の終点 (エポック秒)。値を観測した時刻。
    pub end_ust: u64,
    /// 区間長 (1/100 秒)。**0 は「連続した区間として扱えない」を意味する。**
    pub elapsed_cs: u64,
    pub value: f64,
    pub origin: ObservationOrigin,
}

impl Observation {
    /// 区間長 (秒)。
    pub fn interval_secs(&self) -> u64 {
        self.end_ust.saturating_sub(self.start_ust)
    }
}

// ===========================================================================
// 検出の形
// ===========================================================================

/// 水準が動いた向き。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ShiftDirection {
    /// 上がった。
    Rise,
    /// 下がった。
    Fall,
}

impl ShiftDirection {
    pub const fn as_str(self) -> &'static str {
        match self {
            ShiftDirection::Rise => "上昇",
            ShiftDirection::Fall => "低下",
        }
    }

    /// 差の符号から決める。
    pub fn of(delta: f64) -> Self {
        if delta >= 0.0 {
            ShiftDirection::Rise
        } else {
            ShiftDirection::Fall
        }
    }
}

/// 検出の形。**原因ではなく、観測された形を表す。**
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "shape", rename_all = "snake_case")]
pub enum Pattern {
    /// 周囲より大きく上へ外れた。
    Spike,
    /// 周囲より大きく下へ外れた。
    Dip,
    /// ある時刻から水準が移った。
    LevelShift { direction: ShiftDirection },
    /// 上限に張り付いた (`%util` が 100 付近、`%idle` が 0 付近)。
    Saturation,
    /// 普段は起きていない事象が発生した (スワップ、直接回収、エラーカウンタ)。
    Emergence,
    /// 余裕が尽きかけている (空き容量・空きメモリの減少)。
    Depletion,
    /// 閾値を超えた状態が続いた。
    Sustained,
}

impl Pattern {
    pub const fn label(self) -> &'static str {
        match self {
            Pattern::Spike => "上方への逸脱",
            Pattern::Dip => "下方への逸脱",
            Pattern::LevelShift {
                direction: ShiftDirection::Rise,
            } => "水準の上昇",
            Pattern::LevelShift {
                direction: ShiftDirection::Fall,
            } => "水準の低下",
            Pattern::Saturation => "飽和",
            Pattern::Emergence => "事象の発生",
            Pattern::Depletion => "余裕の減少",
            Pattern::Sustained => "継続した閾値超過",
        }
    }
}

/// どの経路が検出したか。
///
/// 経路は 3 つの**観点**であり、統計的に独立ではない。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DetectRoute {
    /// 絶対水準 (固定条件)。
    FixedCondition,
    /// 参照分布からの逸脱 (median + MAD)。
    RobustDeviation,
    /// 時間的変化 (前後窓の median 差)。
    LevelShift,
}

impl DetectRoute {
    /// 観点の名前。**「独立」「直交」と書かない。**
    pub const fn label(self) -> &'static str {
        match self {
            DetectRoute::FixedCondition => "絶対水準",
            DetectRoute::RobustDeviation => "参照分布からの逸脱",
            DetectRoute::LevelShift => "時間的変化",
        }
    }
}

// ===========================================================================
// 比較基準 (**正常値ではない**)
// ===========================================================================

/// 比較基準をどこから作ったか。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BasisOrigin {
    /// **入力自身**から作った。外部の正常値ではない。
    InputItself,
    /// 入力のうち報告範囲 (`--from` / `--to`) だけから作った。
    ReportWindowOnly,
}

impl BasisOrigin {
    pub const fn label(self) -> &'static str {
        match self {
            BasisOrigin::InputItself => "入力全体 (この入力自身が材料)",
            BasisOrigin::ReportWindowOnly => "報告範囲のみ (この入力自身が材料)",
        }
    }
}

/// 散らばりが測れたか。
///
/// **`MAD == 0` を ε で割ってはいけない。** 値がほぼ一定の系列で
/// `1e-9` のような ε で割ると、無害な 1 ビットの揺れが最重大の検出になる。
/// ここでは「散らばりが測れない」という**別の状態**として扱い、
/// 逸脱スコアを出さずに固定条件経路と水準変化経路へ任せる。
///
/// `MAD` は偏差の中央値なので、**過半数の値が中央値と一致すれば厳密に 0 になる**。
/// 「0 ではないが極端に小さい」状態は、ゼロがちょうど半数前後を占めるときに
/// 起こり得る。そこは [`Dispersion::TooSparse`] で受ける。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Dispersion {
    /// MAD が正で、逸脱スコアを出せる。
    Measured,
    /// MAD が 0。値がほぼ一定なので散らばりを測れない。
    NotMeasurable,
    /// 中央値と異なる値が少なすぎて散らばりの推定材料にならない。
    TooSparse { off_center: u64, required: u64 },
    /// 基準を作るサンプルが足りない。
    InsufficientSamples { required: u64 },
}

impl Dispersion {
    pub const fn is_measured(self) -> bool {
        matches!(self, Dispersion::Measured)
    }

    pub const fn label(self) -> &'static str {
        match self {
            Dispersion::Measured => "測定できた",
            Dispersion::NotMeasurable => "MAD が 0 (値がほぼ一定) のため測れない",
            Dispersion::TooSparse { .. } => "中央値から離れた値が少なすぎて測れない",
            Dispersion::InsufficientSamples { .. } => "サンプル数が足りない",
        }
    }
}

/// 比較基準の中身。
///
/// **`normal` / `baseline_normal` と名付けてはいけない。**
/// 入力自身から作った基準は、異常が長時間を占めていればその状態へ寄る。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BaselineEvidence {
    /// 基準の出所。
    pub basis: BasisOrigin,
    /// 基準を作るのに使った採取回数。
    pub samples: u64,
    /// 基準の材料の時間範囲と採取回数。
    pub support: TemporalSupport,
    /// 中央値。材料が無ければ `None`。
    pub median: Option<f64>,
    /// 中央絶対偏差 (生の値。係数は掛けていない)。
    pub mad: Option<f64>,
    pub dispersion: Dispersion,
    /// **基準そのものが固定条件を満たしているか。**
    ///
    /// `true` なら「この系列の中央値がすでに異常側にある」ことを意味する。
    /// 基準が異常側へ寄っているので、逸脱検出は当てにならない。
    pub median_within_fixed_condition: bool,
    /// 基準の材料のうち固定条件を満たした採取の割合 (0.0〜1.0)。
    ///
    /// 1.0 に近いほど「異常が入力の大半を占めている」ことを示す。
    pub flagged_share: f64,
    /// 読み手に伝える留保。
    pub caveats: Vec<&'static str>,
}

impl BaselineEvidence {
    /// 基準が異常側へ寄っている疑いがあるか。
    ///
    /// 中央値そのものが固定条件を満たす、または材料の半分以上が
    /// 固定条件を満たしている場合。
    pub fn may_reflect_the_anomaly(&self) -> bool {
        self.median_within_fixed_condition || self.flagged_share >= 0.5
    }
}

// ===========================================================================
// 判断の根拠
// ===========================================================================

/// 固定条件の比較方向。
///
/// `analyze::rules::Comparison` とは別に定義する。
/// 「0 より大きい (発生した)」を表す必要があり、`AtLeast(0.0)` では
/// 常に真になってしまうため。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FixedComparison {
    /// 閾値以上。
    AtLeast,
    /// 閾値以下。
    AtMost,
    /// 閾値より大きい (`Above(0.0)` = 発生した)。
    Above,
    /// 閾値より小さい。
    Below,
}

impl FixedComparison {
    pub fn holds(self, value: f64, threshold: f64) -> bool {
        match self {
            FixedComparison::AtLeast => value >= threshold,
            FixedComparison::AtMost => value <= threshold,
            FixedComparison::Above => value > threshold,
            FixedComparison::Below => value < threshold,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            FixedComparison::AtLeast => "以上",
            FixedComparison::AtMost => "以下",
            FixedComparison::Above => "超",
            FixedComparison::Below => "未満",
        }
    }
}

/// 経路ごとの判定内容。**何を見てそう言ったか**を丸ごと残す。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "route", rename_all = "snake_case")]
pub enum DecisionBasis {
    /// 絶対水準。
    FixedCondition {
        /// 安定した条件 ID (出力契約の一部)。
        condition_id: &'static str,
        comparison: FixedComparison,
        threshold: f64,
        /// 条件を満たした採取が連続して何回必要か。
        min_samples: u32,
        /// この絶対値に意味がある根拠。
        rationale: &'static str,
    },
    /// 参照分布からの逸脱。
    ///
    /// **正規化しない。** 生の MAD に対する比で表す
    /// ([`MAD_SCALE`] の doc を参照)。
    RobustDeviation {
        median: f64,
        /// 生の MAD。
        mad: f64,
        /// 最も外れた点の `|x - median| / MAD`。
        peak_mad_ratio: f64,
        ratio_threshold: f64,
        /// 併せて要求した絶対差の下限 (指標ごとの最小有意変化量)。
        min_absolute_deviation: f64,
        /// 最も外れた点の `|x - median|`。
        peak_absolute_deviation: f64,
        direction: ShiftDirection,
    },
    /// 時間的変化。
    LevelShift {
        before_median: f64,
        after_median: f64,
        /// `after_median - before_median`。
        shift: f64,
        /// 絶対差の下限 (この指標の単位)。
        min_shift: f64,
        /// 前後の窓を各々の中央値で中心化した残差を束ねた MAD。
        ///
        /// **段差を含めたまま束ねると尺度が段差自身で膨らむ**ので、
        /// 窓ごとに中心化してから束ねる。
        pooled_mad: Option<f64>,
        /// `|shift| / (MAD_SCALE × pooled_mad)`。
        /// 散らばりが測れないときは `None` (**ε で割らない**)。
        normalized_shift: Option<f64>,
        normalized_threshold: f64,
        /// 後窓のうち前窓の中央値から同方向へ最小変化量以上離れた割合。
        persistence_share: f64,
        persistence_threshold: f64,
        before: TemporalSupport,
        after: TemporalSupport,
    },
}

impl DecisionBasis {
    pub const fn route(&self) -> DetectRoute {
        match self {
            DecisionBasis::FixedCondition { .. } => DetectRoute::FixedCondition,
            DecisionBasis::RobustDeviation { .. } => DetectRoute::RobustDeviation,
            DecisionBasis::LevelShift { .. } => DetectRoute::LevelShift,
        }
    }
}

/// 根拠として保持する観測の上限。
///
/// 全点を持つと 1 日分の検出で出力が肥大する。**打ち切ったことは
/// [`DecisionEvidence::observations_truncated`] で明示する。**
pub const MAX_KEPT_OBSERVATIONS: usize = 8;

/// なぜそう判断したか。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DecisionEvidence {
    pub route: DetectRoute,
    pub basis: DecisionBasis,
    /// 判定に効いた観測 (先頭から [`MAX_KEPT_OBSERVATIONS`] 点)。
    pub observations: Vec<Observation>,
    /// 観測を打ち切ったか (打ち切ったことを黙らない)。
    pub observations_truncated: bool,
    pub min: f64,
    pub max: f64,
    /// 時間加重平均。区間長が取れない場合は標本平均。
    pub mean: f64,
}

impl DecisionEvidence {
    /// 観測列から根拠を組み立てる。
    pub fn new(basis: DecisionBasis, observations: &[Observation]) -> Self {
        let mut min = f64::INFINITY;
        let mut max = f64::NEG_INFINITY;
        let mut weighted = 0.0;
        let mut weight = 0.0;
        let mut plain = 0.0;
        for o in observations {
            min = min.min(o.value);
            max = max.max(o.value);
            weighted += o.value * o.elapsed_cs as f64;
            weight += o.elapsed_cs as f64;
            plain += o.value;
        }
        let mean = if weight > 0.0 {
            weighted / weight
        } else if observations.is_empty() {
            0.0
        } else {
            plain / observations.len() as f64
        };
        Self {
            route: basis.route(),
            basis,
            observations: observations
                .iter()
                .take(MAX_KEPT_OBSERVATIONS)
                .copied()
                .collect(),
            observations_truncated: observations.len() > MAX_KEPT_OBSERVATIONS,
            min: if observations.is_empty() { 0.0 } else { min },
            max: if observations.is_empty() { 0.0 } else { max },
            mean,
        }
    }
}

// ===========================================================================
// 検出 1 件
// ===========================================================================

/// 1 件の検出。
///
/// 観測とその根拠だけを持つ。**優先度 (解釈) は持たない。**
/// 優先度は [`crate::analyze::assessment`] がエピソード単位で付ける。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Detection {
    pub detector_version: &'static str,
    pub series: SeriesKey,
    /// 人間向けの指標名 (`CPU の空き時間` など)。
    pub metric_label: &'static str,
    pub unit: Unit,
    pub kind: ValueKind,
    pub origin: ObservationOrigin,
    pub pattern: Pattern,
    pub support: TemporalSupport,
    /// 比較基準 (**正常値ではない**)。
    pub baseline: BaselineEvidence,
    /// 判断の根拠。
    pub decision: DecisionEvidence,
    /// この検出が単独で立ったときの調査優先度の下地。
    ///
    /// **確率ではなく順序尺度。** 最終的な優先度は
    /// [`crate::analyze::assessment`] が持続性と充足度を見て決める。
    pub base_priority: crate::analyze::assessment::Priority,
    /// 考えられる解釈 (複数。どれとも断定しない)。
    pub possible_interpretations: &'static [&'static str],
    /// この検出では確かめていないこと。
    pub not_established: &'static [&'static str],
}

impl Detection {
    pub fn route(&self) -> DetectRoute {
        self.decision.route
    }
}

// ===========================================================================
// 設定
// ===========================================================================

/// 比較基準の材料をどこから取るか。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BaselineScope {
    /// **入力全体** (既定)。`--from` / `--to` は報告範囲を絞るだけで、
    /// 基準の材料は絞らない。狭い調査範囲の外から比較材料を取れるようにするため。
    #[default]
    Input,
    /// 報告範囲のみ。
    Window,
}

/// 報告範囲の境界。
///
/// `output::time_filter::TimeBound` と同じ形だが、分析層が出力層へ
/// 依存しないよう独立に定義する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReportBound {
    #[default]
    None,
    /// エポック秒。
    Epoch(u64),
    /// 時刻 (UTC)。入力の最初の採取日に当てて解釈する。
    TimeOfDay { hour: u8, min: u8, sec: u8 },
}

impl ReportBound {
    /// エポック秒の境界。時刻指定では `None`。
    fn epoch(self) -> Option<u64> {
        match self {
            ReportBound::Epoch(e) => Some(e),
            _ => None,
        }
    }

    /// 00:00:00 からの秒数。エポック指定では `None`。
    fn seconds_of_day(self) -> Option<u64> {
        match self {
            ReportBound::TimeOfDay { hour, min, sec } => {
                Some(u64::from(hour) * 3600 + u64::from(min) * 60 + u64::from(sec))
            }
            _ => None,
        }
    }

    pub fn is_none(self) -> bool {
        matches!(self, ReportBound::None)
    }
}

/// 報告範囲。
///
/// **時刻 (`hh:mm[:ss]`) は絶対時刻へ解かず、時刻として比べる。**
/// 1 ファイルが日付を跨ぐ (15:00 開始で翌日 14:50 まで) のは通常なので、
/// 「最初の採取日の 03:00」へ解くと、利用者が意図した翌日 03:00 を外す。
/// `sar -s` / `-e` と同じく**毎日の時刻**として扱う。
///
/// `--from` が `--to` より後ろの場合は日付を跨ぐ指定として解釈する
/// (`--from 22:00 --to 02:00` は深夜帯)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReportWindow {
    pub from: ReportBound,
    pub to: ReportBound,
}

impl ReportWindow {
    pub fn is_unbounded(&self) -> bool {
        self.from.is_none() && self.to.is_none()
    }

    /// 区間 `[start_ust, end_ust]` が報告範囲に重なるか。
    pub fn admits(&self, start_ust: u64, end_ust: u64) -> bool {
        if let Some(s) = self.from.epoch()
            && end_ust < s
        {
            return false;
        }
        if let Some(e) = self.to.epoch()
            && start_ust > e
        {
            return false;
        }
        if self.from.seconds_of_day().is_none() && self.to.seconds_of_day().is_none() {
            return true;
        }
        let lo = self.from.seconds_of_day().unwrap_or(0);
        let hi = self.to.seconds_of_day().unwrap_or(86_399);
        // 24 時間以上に及ぶ区間はどの時刻も含む
        if end_ust.saturating_sub(start_ust) >= 86_400 {
            return true;
        }
        in_time_window(start_ust, lo, hi) || in_time_window(end_ust, lo, hi)
    }
}

/// エポック秒の時刻部分が `[lo, hi]` に入るか (`lo > hi` は日跨ぎ)。
fn in_time_window(ust: u64, lo: u64, hi: u64) -> bool {
    let tod = ust % 86_400;
    if lo <= hi {
        tod >= lo && tod <= hi
    } else {
        tod >= lo || tod <= hi
    }
}

/// 検出の閾値。
///
/// **出力にそのまま載せる**ので、既定値を変えたら [`DETECTOR_VERSION`] を上げる。
/// どの値も「理論から導いた値」ではなく**感度を控えめに振った設計値**である。
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct DetectThresholds {
    /// 逸脱とみなす `|x - median| / MAD` の下限。
    ///
    /// よく使われる 3.5 (modified Z-score の目安) は、この条件
    /// (144 点前後・自己相関が強い・右に裾が長い) では鳴りすぎる。
    /// 逆に 20 では取りこぼす。**誤検出率を保証する値ではない。**
    pub deviation_ratio: f64,
    /// 基準を作るのに必要な最小サンプル数。
    pub min_baseline_samples: u64,
    /// 散らばりの推定に必要な「中央値から離れた値」の最小割合。
    ///
    /// 144 点のうち 120 点が同じ値の系列では、MAD が 0 でなくても
    /// 散らばりの推定材料になっていない。
    pub min_off_center_share: f64,
    /// 水準変化の窓幅 (秒)。
    ///
    /// **サンプル数ではなく時間幅で決める。** 10 秒採取と 10 分採取で
    /// 同じサンプル数の窓を使うと、前者は 50 秒、後者は 50 分を比べてしまう。
    pub shift_window_secs: u64,
    /// 水準変化の窓のサンプル数の下限。
    ///
    /// 窓が小さいと単発のスパイクが窓の中央値を動かしてしまう。
    pub shift_window_min_samples: usize,
    /// 正規化した水準差の下限 (散らばりが測れたときのみ適用)。
    pub shift_normalized: f64,
    /// 後窓のうち同方向へ動いていることを要求する割合。
    ///
    /// 小さな窓では単発の値が中央値を押し出せるので、持続性を別に要求する。
    pub shift_persistence_share: f64,
    /// エピソードをまとめるときのギャップ許容量 (採取間隔の倍数)。
    ///
    /// 「検出点の間に欠測 1 個ぶんを許す」という意味で 2 を既定にする。
    pub episode_gap_factor: u64,
    /// ギャップ許容量の絶対上限 (秒)。
    ///
    /// 採取間隔が長いログで、無関係な事象が 1 つのエピソードへ融合するのを防ぐ。
    pub episode_gap_cap_secs: u64,
    /// 優先度を 1 段上げる持続性の下限 (採取回数)。
    pub persistence_samples: u64,
    /// 優先度を 1 段上げる持続性の下限 (秒)。
    pub persistence_secs: u64,
}

impl Default for DetectThresholds {
    fn default() -> Self {
        Self {
            deviation_ratio: 8.0,
            // 10 分間隔なら 2 時間ぶん。これ未満の median / MAD は基準にしない
            min_baseline_samples: 12,
            min_off_center_share: 0.20,
            shift_window_secs: 1800,
            shift_window_min_samples: 5,
            shift_normalized: 3.0,
            shift_persistence_share: 0.7,
            episode_gap_factor: 2,
            episode_gap_cap_secs: 1800,
            persistence_samples: 3,
            persistence_secs: 1800,
        }
    }
}

/// 検出の設定。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DetectOptions {
    pub baseline_scope: BaselineScope,
    pub report_from: ReportBound,
    pub report_to: ReportBound,
    pub thresholds: DetectThresholds,
}

impl DetectOptions {
    fn basis_origin(&self) -> BasisOrigin {
        match self.baseline_scope {
            BaselineScope::Input => BasisOrigin::InputItself,
            BaselineScope::Window => BasisOrigin::ReportWindowOnly,
        }
    }
}

// ===========================================================================
// 系列の準備
// ===========================================================================

/// 検出にかけられる形へ整えた 1 系列。
///
/// **不連続を挟んだ区間は基準にも検出にも使わない** (`docs/design.md` §5)。
/// RESTART / item 入れ替え / 非正の経過時間で連続区間を切り、
/// 切れ目を跨ぐ比較 (水準変化) を成立させない。
#[derive(Debug, Clone)]
pub struct PreparedSeries {
    pub key: MetricKey,
    pub unit: Unit,
    pub kind: ValueKind,
    pub origin: ObservationOrigin,
    /// 値が得られた観測 (時刻昇順)。
    pub observations: Vec<Observation>,
    /// 連続区間 (`observations` の半開区間)。
    pub segments: Vec<(usize, usize)>,
    /// 値が得られなかった採取回数。
    pub missing: u64,
    /// 不連続として捨てた区間数。
    pub discontinuities: u64,
    /// 不連続が起きた時刻 (区間の終点)。エピソードを跨がせないために使う。
    pub discontinuity_marks: Vec<u64>,
}

impl PreparedSeries {
    /// 時系列から整える。
    pub fn from_timeline(t: &MetricTimeline) -> Self {
        let origin = ObservationOrigin::of(t.kind);
        let mut observations: Vec<Observation> = Vec::new();
        let mut segments: Vec<(usize, usize)> = Vec::new();
        let mut marks: Vec<u64> = Vec::new();
        let mut open: Option<usize> = None;
        let mut missing = 0u64;
        let mut discontinuities = 0u64;
        let mut prev_end: Option<u64> = None;

        for p in &t.points {
            // 連続区間を切る条件。
            // - 時刻が繋がらない (別ファイル・採取停止)
            // - 区間長 0 = 前サンプルが無い / RESTART を挟んだ
            //   (`analyze::summary` が weight_cs を 0 にしている)
            // - 欠損の理由が不連続 (item 入れ替え・カウンタ逆行など)
            let adjacent = prev_end.is_none_or(|e| e == p.start_ust);
            let discontinuous =
                p.elapsed_cs == 0 || p.reason.is_some_and(|r| r.is_discontinuity()) || !adjacent;
            prev_end = Some(p.end_ust);

            if discontinuous {
                discontinuities += 1;
                marks.push(p.end_ust);
                if let Some(start) = open.take() {
                    segments.push((start, observations.len()));
                }
                if p.value.is_none() {
                    missing += 1;
                }
                // 不連続な区間の値は基準にも検出にも使わない
                continue;
            }

            match p.value {
                Some(v) if v.is_finite() => {
                    if open.is_none() {
                        open = Some(observations.len());
                    }
                    observations.push(Observation {
                        start_ust: p.start_ust,
                        end_ust: p.end_ust,
                        elapsed_cs: p.elapsed_cs,
                        value: v,
                        origin,
                    });
                }
                _ => {
                    // 欠損は 0 ではない。連続区間は切らないが観測にも数えない。
                    missing += 1;
                }
            }
        }
        if let Some(start) = open.take() {
            segments.push((start, observations.len()));
        }

        Self {
            key: t.key.clone(),
            unit: t.unit,
            kind: t.kind,
            origin,
            observations,
            segments,
            missing,
            discontinuities,
            discontinuity_marks: marks,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.observations.is_empty()
    }

    /// 連続区間の観測列。
    pub fn segment(&self, i: usize) -> &[Observation] {
        let (a, b) = self.segments[i];
        &self.observations[a..b]
    }

    /// 連続区間を順に返す。
    pub fn iter_segments(&self) -> impl Iterator<Item = &[Observation]> {
        self.segments
            .iter()
            .map(|(a, b)| &self.observations[*a..*b])
    }

    /// 採取間隔の代表値 (秒)。
    ///
    /// **中央値ではなく 90 パーセンタイルを使う。** 採取間隔は
    /// `sadc` の起動ずれで数十秒ジッタする。中央値で閾値を作ると
    /// ジッタ側の区間を「離れている」と判定してしまう。
    pub fn interval_p90_secs(&self) -> Option<u64> {
        let mut gaps: Vec<u64> = self
            .observations
            .iter()
            .filter(|o| o.elapsed_cs > 0)
            .map(Observation::interval_secs)
            .filter(|g| *g > 0)
            .collect();
        if gaps.is_empty() {
            return None;
        }
        gaps.sort_unstable();
        let idx = ((gaps.len() as f64 * 0.9).ceil() as usize).saturating_sub(1);
        Some(gaps[idx.min(gaps.len() - 1)])
    }

    /// 観測列から時間的な裏付けを作る。
    ///
    /// 欠測・不連続は**系列全体の値**を載せる。検出範囲だけを切り出すと
    /// 「この範囲では欠測が無かった」に見えてしまう。
    pub fn support_of(&self, observations: &[Observation]) -> TemporalSupport {
        let Some(first) = observations.first() else {
            return TemporalSupport::default();
        };
        let last = observations.last().expect("非空");
        TemporalSupport {
            start_ust: first.start_ust,
            end_ust: last.end_ust,
            samples: observations.len() as u64,
            missing_samples: self.missing,
            discontinuities: self.discontinuities,
            observed_cs: observations.iter().map(|o| o.elapsed_cs).sum(),
        }
    }
}

// ===========================================================================
// 基準の材料
// ===========================================================================

/// 比較基準の材料と、そこから作った統計量。
#[derive(Debug, Clone)]
pub struct Baseline {
    pub evidence: BaselineEvidence,
    /// 材料に使った観測。
    pub material: Vec<Observation>,
}

impl Baseline {
    /// 逸脱スコアに使える尺度 (生の MAD)。測れなければ `None`。
    pub fn usable_mad(&self) -> Option<(f64, f64)> {
        if !self.evidence.dispersion.is_measured() {
            return None;
        }
        match (self.evidence.median, self.evidence.mad) {
            (Some(c), Some(m)) if m > 0.0 => Some((c, m)),
            _ => None,
        }
    }
}

/// 中央値。**材料を破壊しないため複製してから並べ替える。**
pub fn median(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut v: Vec<f64> = values.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = v.len();
    if n % 2 == 1 {
        Some(v[n / 2])
    } else {
        Some((v[n / 2 - 1] + v[n / 2]) / 2.0)
    }
}

/// 中央絶対偏差。
pub fn mad(values: &[f64], center: f64) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let dev: Vec<f64> = values.iter().map(|v| (v - center).abs()).collect();
    median(&dev)
}

/// 指標ごとの最小有意変化量を実際の値へ解く。
///
/// 相対宣言は**中央値に対する割合と絶対下限の大きい方**を採る。
/// 中央値が 0 付近の系列で割合だけを使うと、`aqu-sz` が 0.0002 から
/// 0.01 になっただけで「中央値の 50 倍」という報告が出る。
/// 絶対下限がその報告を止める
/// ([`crate::analyze::metric_catalog::ShiftMagnitude::Relative`])。
pub fn magnitude_floor(shift: ShiftMagnitude, center: f64) -> Option<f64> {
    match shift {
        ShiftMagnitude::Absolute(v) => Some(v),
        ShiftMagnitude::Relative { fraction, floor } => Some((center.abs() * fraction).max(floor)),
        // 普段 0 の事象カウンタ。下限を課さず固定条件経路に任せる
        ShiftMagnitude::NotEvaluated => None,
    }
}

/// 比較基準を作る。
///
/// **基準は入力自身から作る。** 外部の正常値は持っていないし、
/// 持っているふりもしない ([`BasisOrigin`] で明示する)。
pub fn build_baseline(
    series: &PreparedSeries,
    entry: &CatalogEntry,
    material: Vec<Observation>,
    opts: &DetectOptions,
) -> Baseline {
    let values: Vec<f64> = material.iter().map(|o| o.value).collect();
    let samples = values.len() as u64;
    let support = series.support_of(&material);
    let med = median(&values);
    let m = med.and_then(|c| mad(&values, c));

    let off_center = med.map_or(0, |c| values.iter().filter(|v| **v != c).count() as u64);
    let required_off_center =
        ((samples as f64) * opts.thresholds.min_off_center_share).ceil() as u64;

    let dispersion = if samples < opts.thresholds.min_baseline_samples {
        Dispersion::InsufficientSamples {
            required: opts.thresholds.min_baseline_samples,
        }
    } else {
        match m {
            // MAD == 0 は ε で割らず「測れない」として扱う
            Some(v) if v > 0.0 => {
                if off_center < required_off_center {
                    Dispersion::TooSparse {
                        off_center,
                        required: required_off_center,
                    }
                } else {
                    Dispersion::Measured
                }
            }
            Some(_) => Dispersion::NotMeasurable,
            None => Dispersion::InsufficientSamples {
                required: opts.thresholds.min_baseline_samples,
            },
        }
    };

    let flagged = threshold::flagged_share(entry, &values);
    let median_flagged = med.is_some_and(|c| threshold::satisfies_any_fixed(entry, c));

    let mut caveats: Vec<&'static str> =
        vec!["この基準は入力自身から作ったものであり、外部の正常値ではない"];
    if median_flagged {
        caveats.push(
            "中央値そのものが固定条件を満たしている。基準が異常側へ寄っているため、\
             この系列の逸脱検出は当てにならない",
        );
    } else if flagged >= 0.5 {
        caveats
            .push("材料の半分以上が固定条件を満たしている。基準が異常側へ寄っている可能性がある");
    }
    match dispersion {
        Dispersion::NotMeasurable => {
            caveats.push("MAD が 0 なので逸脱スコアは出さない (絶対水準と時間的変化に任せる)");
        }
        Dispersion::TooSparse { .. } => {
            caveats.push(
                "中央値と同じ値が大半を占める。散らばりの推定材料にならないので\
                 逸脱スコアは出さない",
            );
        }
        _ => {}
    }

    Baseline {
        evidence: BaselineEvidence {
            basis: opts.basis_origin(),
            samples,
            support,
            median: med,
            mad: m,
            dispersion,
            median_within_fixed_condition: median_flagged,
            flagged_share: flagged,
            caveats,
        },
        material,
    }
}

// ===========================================================================
// 検出の実行
// ===========================================================================

/// 検出の結果。
#[derive(Debug, Clone, Default)]
pub struct DetectOutcome {
    pub detections: Vec<Detection>,
    /// 系列ごとに何を評価でき、何を評価できなかったか。
    pub evaluations: Vec<SeriesEvaluation>,
    /// 採取間隔の代表値 (秒)。エピソードの近接判定に使う。
    pub interval_p90_secs: Option<u64>,
    /// 不連続が起きた時刻。**エピソードはここを跨がない。**
    pub discontinuity_marks: Vec<u64>,
}

/// 時系列に 3 経路を独立に走らせる。
///
/// **経路の実行順は結果に影響しない。** 1 つの経路が 0 件でも、
/// 他の経路はそのまま走る。
pub fn detect(timelines: &Timelines, opts: &DetectOptions) -> DetectOutcome {
    let mut out = DetectOutcome::default();

    let window = ReportWindow {
        from: opts.report_from,
        to: opts.report_to,
    };

    let mut seen: Vec<&'static CatalogEntry> = Vec::new();
    let mut intervals: Vec<u64> = Vec::new();

    for timeline in timelines.iter() {
        let Some(entry) = metric_catalog::lookup(&timeline.key) else {
            continue;
        };
        if !seen.iter().any(|e| std::ptr::eq(*e, entry)) {
            seen.push(entry);
        }

        let series = PreparedSeries::from_timeline(timeline);
        if let Some(i) = series.interval_p90_secs() {
            intervals.push(i);
        }
        for m in &series.discontinuity_marks {
            if !out.discontinuity_marks.contains(m) {
                out.discontinuity_marks.push(*m);
            }
        }

        let material = baseline_material(&series, opts, window);
        let baseline = build_baseline(&series, entry, material, opts);

        let mut eval = SeriesEvaluation::observed(&series, entry, &baseline.evidence);

        // --- 3 経路。互いを実行条件にしない ---
        let (fixed, fixed_status) = threshold::detect(&series, entry, &baseline, opts);
        let (dev, dev_status) = robust::detect(&series, entry, &baseline, opts);
        let (shift, shift_status) = level_shift::detect(&series, entry, &baseline, opts);

        eval.fixed_condition = fixed_status;
        eval.robust_deviation = dev_status;
        eval.level_shift = shift_status;
        out.evaluations.push(eval);

        out.detections
            .extend(fixed.into_iter().chain(dev).chain(shift));
    }

    // カタログにあるのに入力に無い系列も「評価できなかった」として残す。
    // **黙って落とすと「検出なし」と区別が付かない。**
    for entry in metric_catalog::CATALOG {
        if !seen.iter().any(|e| std::ptr::eq(*e, entry)) {
            out.evaluations.push(SeriesEvaluation::absent(entry));
        }
    }

    // 報告範囲で検出を絞る (**基準の材料は絞らない**)
    out.detections
        .retain(|d| window.admits(d.support.start_ust, d.support.end_ust));

    // 出力を決定的にする
    out.detections.sort_by(|a, b| {
        a.support
            .start_ust
            .cmp(&b.support.start_ust)
            .then_with(|| a.series.cmp(&b.series))
            .then_with(|| a.route().cmp(&b.route()))
    });
    out.evaluations.sort_by(|a, b| a.series.cmp(&b.series));
    out.discontinuity_marks.sort_unstable();

    intervals.sort_unstable();
    out.interval_p90_secs = intervals.get(intervals.len() / 2).copied();
    out
}

/// 基準の材料を選ぶ。
///
/// **既定 ([`BaselineScope::Input`]) では報告範囲で絞らない。**
/// 狭い調査範囲の外から比較材料を取れるようにするため。
fn baseline_material(
    series: &PreparedSeries,
    opts: &DetectOptions,
    window: ReportWindow,
) -> Vec<Observation> {
    match opts.baseline_scope {
        BaselineScope::Input => series.observations.clone(),
        BaselineScope::Window => series
            .observations
            .iter()
            .filter(|o| window.admits(o.start_ust, o.end_ust))
            .copied()
            .collect(),
    }
}

/// 連続して条件を満たした観測を区切る。
///
/// 絶対水準と参照分布からの逸脱が共通で使う。
/// **観測が連続していない (時刻が繋がらない) 場合は区切る。**
pub(crate) fn group_runs<F>(observations: &[Observation], mut hit: F) -> Vec<(usize, usize)>
where
    F: FnMut(&Observation) -> bool,
{
    let mut runs = Vec::new();
    let mut open: Option<usize> = None;
    for (i, o) in observations.iter().enumerate() {
        let adjacent = i == 0 || observations[i - 1].end_ust == o.start_ust;
        if !hit(o) || !adjacent {
            if let Some(s) = open.take() {
                runs.push((s, i));
            }
            if hit(o) && !adjacent {
                open = Some(i);
            }
            continue;
        }
        if open.is_none() {
            open = Some(i);
        }
    }
    if let Some(s) = open {
        runs.push((s, observations.len()));
    }
    runs
}

#[cfg(test)]
pub(crate) mod testing {
    //! 検出のテストで使う時系列の組み立て。

    use super::*;
    use crate::analyze::timeline::{ExclusionReason, MetricPoint};

    /// 1 区間 = 600 秒 (sysstat の既定採取間隔)。
    pub const STEP_SECS: u64 = 600;
    pub const STEP_CS: u64 = STEP_SECS * 100;
    /// 基準時刻 (2026-01-01 00:00:00 UTC)。
    pub const T0: u64 = 1_767_225_600;

    /// 1 点の指定。
    #[derive(Debug, Clone, Copy)]
    pub enum P {
        /// 値が得られた。
        V(f64),
        /// 値が無い (不連続ではない欠損)。
        Missing,
        /// RESTART を挟んだ (区間長 0 で不連続)。
        Restart,
    }

    pub fn timeline(key: MetricKey, unit: Unit, kind: ValueKind, points: &[P]) -> MetricTimeline {
        let mut t = MetricTimeline::new(key, unit, kind);
        for (i, p) in points.iter().enumerate() {
            let start = T0 + i as u64 * STEP_SECS;
            let end = start + STEP_SECS;
            t.push(match p {
                P::V(v) => MetricPoint::observed(start, end, STEP_CS, *v),
                P::Missing => {
                    MetricPoint::missing(start, end, STEP_CS, ExclusionReason::MissingInSample)
                }
                // 不連続な区間は区間長 0 で来る (`analyze::summary` の weight_cs)
                P::Restart => MetricPoint::missing(start, end, 0, ExclusionReason::Restart),
            });
        }
        t
    }

    /// `A_CPU/all/idle` (Counter / %) の時系列。
    pub fn cpu_idle(points: &[P]) -> MetricTimeline {
        timeline(
            MetricKey::new(ActivityId::CPU, "all", "idle"),
            Unit::Percent,
            ValueKind::Counter,
            points,
        )
    }

    /// `A_QUEUE/-/runq_sz` (Gauge / 固定条件なし) の時系列。
    pub fn runq(points: &[P]) -> MetricTimeline {
        timeline(
            MetricKey::new(ActivityId::QUEUE, "-", "runq_sz"),
            Unit::None,
            ValueKind::Gauge,
            points,
        )
    }

    /// `A_SWAP/-/pswpin` (Counter / 発生自体が事象) の時系列。
    pub fn pswpin(points: &[P]) -> MetricTimeline {
        timeline(
            MetricKey::new(ActivityId::SWAP, "-", "pswpin"),
            Unit::CountPerSec,
            ValueKind::Counter,
            points,
        )
    }

    /// 値の並びを `P::V` へ。
    pub fn vals(values: &[f64]) -> Vec<P> {
        values.iter().map(|v| P::V(*v)).collect()
    }

    pub fn collect(timelines: &[MetricTimeline]) -> Timelines {
        let mut ts = Timelines::new();
        for t in timelines {
            let dst = ts.entry(t.key.clone(), t.unit, t.kind);
            for p in &t.points {
                dst.push(*p);
            }
        }
        ts
    }

    pub fn single(t: MetricTimeline) -> Timelines {
        collect(std::slice::from_ref(&t))
    }

    /// カタログ項目を引く (テスト用)。
    pub fn entry_for(t: &MetricTimeline) -> &'static CatalogEntry {
        metric_catalog::lookup(&t.key).expect("カタログ項目")
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;

    #[test]
    fn describe_span_states_both_duration_and_sample_count() {
        let s = TemporalSupport {
            start_ust: 0,
            end_ust: 1200,
            samples: 3,
            ..Default::default()
        };
        let text = s.describe_span();
        assert!(text.contains("20 分"), "{text}");
        assert!(text.contains("3 回の採取"), "{text}");
        // 「20 分間」と書いてはいけない (採取の間は観測していない)
        assert!(!text.contains("分間"), "{text}");
    }

    #[test]
    fn duration_is_rendered_in_readable_units() {
        assert_eq!(describe_duration(45), "45 秒");
        assert_eq!(describe_duration(600), "10 分");
        assert_eq!(describe_duration(7200), "2 時間");
        assert_eq!(describe_duration(5400), "1 時間 30 分");
    }

    /// 不連続 (RESTART) を挟んだ区間は観測に数えず、連続区間を切る。
    #[test]
    fn restart_splits_segments_and_is_not_an_observation() {
        let t = cpu_idle(&[
            P::V(90.0),
            P::V(91.0),
            P::Restart,
            P::V(92.0),
            P::V(93.0),
            P::V(94.0),
        ]);
        let s = PreparedSeries::from_timeline(&t);
        assert_eq!(s.observations.len(), 5, "RESTART の区間は観測に入らない");
        assert_eq!(s.segments.len(), 2);
        assert_eq!(s.segment(0).len(), 2);
        assert_eq!(s.segment(1).len(), 3);
        assert_eq!(s.discontinuities, 1);
        assert_eq!(s.discontinuity_marks.len(), 1);
    }

    /// 欠損は 0 ではない。観測に数えないが連続区間は切らない。
    #[test]
    fn missing_is_not_zero_and_does_not_split() {
        let t = cpu_idle(&[P::V(90.0), P::Missing, P::V(92.0)]);
        let s = PreparedSeries::from_timeline(&t);
        assert_eq!(s.observations.len(), 2);
        assert_eq!(s.missing, 1);
        assert_eq!(s.segments.len(), 1, "欠損では区間を切らない");
        assert!(
            !s.observations.iter().any(|o| o.value == 0.0),
            "欠損を 0 として観測に混ぜてはいけない"
        );
    }

    #[test]
    fn median_and_mad_are_computed_without_mutating_input() {
        let v = [5.0, 1.0, 3.0];
        assert_eq!(median(&v), Some(3.0));
        assert_eq!(v[0], 5.0, "入力を並べ替えてはいけない");
        assert_eq!(mad(&v, 3.0), Some(2.0));
        assert_eq!(median(&[] as &[f64]), None);
        assert_eq!(median(&[1.0, 2.0, 3.0, 4.0]), Some(2.5));
    }

    #[test]
    fn observation_origin_separates_counter_from_gauge() {
        assert_eq!(
            ObservationOrigin::of(ValueKind::Counter),
            ObservationOrigin::IntervalRate
        );
        assert_eq!(
            ObservationOrigin::of(ValueKind::Gauge),
            ObservationOrigin::InstantGauge
        );
    }

    #[test]
    fn fixed_comparison_above_zero_means_occurrence() {
        assert!(!FixedComparison::Above.holds(0.0, 0.0));
        assert!(FixedComparison::Above.holds(0.1, 0.0));
        assert!(FixedComparison::AtLeast.holds(0.0, 0.0));
    }

    #[test]
    fn group_runs_breaks_on_non_adjacent_observations() {
        let obs: Vec<Observation> = [(0u64, 600u64), (7200, 7800), (7800, 8400)]
            .iter()
            .map(|(a, b)| Observation {
                start_ust: *a,
                end_ust: *b,
                elapsed_cs: 60000,
                value: 9.0,
                origin: ObservationOrigin::InstantGauge,
            })
            .collect();
        let runs = group_runs(&obs, |o| o.value > 1.0);
        assert_eq!(runs, vec![(0, 1), (1, 3)]);
    }

    /// 採取間隔の代表値はジッタを吸収する側 (p90) を採る。
    ///
    /// 中央値では `sadc` の起動ずれで伸びた区間を「離れている」と
    /// 判定してしまうので、上側の分位点を使う。
    #[test]
    fn interval_representative_absorbs_jitter() {
        let mut t = cpu_idle(&vals(&[1.0; 10]));
        // 10 区間のうち 2 区間が 30 秒伸びた状況を作る
        let mut shift = 0u64;
        for i in 0..t.points.len() {
            t.points[i].start_ust += shift;
            if i == 3 || i == 7 {
                shift += 30;
            }
            t.points[i].end_ust += shift;
        }
        let s = PreparedSeries::from_timeline(&t);
        assert_eq!(
            s.interval_p90_secs(),
            Some(630),
            "上側の分位点はジッタ側を採る"
        );
    }

    /// 相対宣言は絶対下限で底を打つ。
    ///
    /// 中央値が 0 付近の系列で割合だけを使うと、無害な揺れが
    /// 「中央値の 50 倍」として報告される。
    #[test]
    fn a_relative_magnitude_is_floored_in_absolute_terms() {
        use crate::analyze::metric_catalog::ShiftMagnitude;
        assert_eq!(
            magnitude_floor(ShiftMagnitude::Absolute(5.0), 0.0),
            Some(5.0)
        );
        // 中央値が大きければ割合が効く
        assert_eq!(
            magnitude_floor(
                ShiftMagnitude::Relative {
                    fraction: 0.5,
                    floor: 1.0
                },
                100.0
            ),
            Some(50.0)
        );
        // 中央値が 0 付近なら絶対下限が効く
        assert_eq!(
            magnitude_floor(
                ShiftMagnitude::Relative {
                    fraction: 1.0,
                    floor: 1.0
                },
                0.0002
            ),
            Some(1.0)
        );
        // 事象カウンタは下限を課さない (固定条件経路が見る)
        assert_eq!(magnitude_floor(ShiftMagnitude::NotEvaluated, 1.0), None);
    }

    /// 時刻指定は絶対時刻へ解かず「毎日の時刻」として比べる。
    ///
    /// 1 ファイルが日付を跨ぐのは通常なので、最初の採取日へ解くと
    /// 利用者が意図した翌日の時刻を外す。
    #[test]
    fn a_time_of_day_bound_is_not_resolved_against_the_first_day() {
        let w = ReportWindow {
            from: ReportBound::TimeOfDay {
                hour: 3,
                min: 0,
                sec: 0,
            },
            to: ReportBound::TimeOfDay {
                hour: 6,
                min: 0,
                sec: 0,
            },
        };
        // T0 は 00:00:00 UTC。翌日の 04:00 も範囲に入る
        let next_day_0400 = T0 + 86_400 + 4 * 3600;
        assert!(w.admits(next_day_0400, next_day_0400 + 600));
        // 同じ日の 15:00 は範囲外
        let same_day_1500 = T0 + 15 * 3600;
        assert!(!w.admits(same_day_1500, same_day_1500 + 600));
    }

    /// `--from` が `--to` より後ろなら日跨ぎとして扱う。
    #[test]
    fn an_overnight_window_wraps_around_midnight() {
        let w = ReportWindow {
            from: ReportBound::TimeOfDay {
                hour: 22,
                min: 0,
                sec: 0,
            },
            to: ReportBound::TimeOfDay {
                hour: 2,
                min: 0,
                sec: 0,
            },
        };
        let t2300 = T0 + 23 * 3600;
        let t0100 = T0 + 3600;
        let t1200 = T0 + 12 * 3600;
        assert!(w.admits(t2300, t2300 + 600));
        assert!(w.admits(t0100, t0100 + 600));
        assert!(!w.admits(t1200, t1200 + 600));
    }

    #[test]
    fn an_unbounded_window_admits_everything() {
        let w = ReportWindow::default();
        assert!(w.is_unbounded());
        assert!(w.admits(0, 0));
        assert!(w.admits(T0, T0 + 86_400));
    }

    /// エポック指定は絶対時刻で比べる。
    #[test]
    fn an_epoch_bound_is_compared_absolutely() {
        let w = ReportWindow {
            from: ReportBound::Epoch(T0 + 3600),
            to: ReportBound::None,
        };
        assert!(!w.admits(T0, T0 + 600));
        assert!(w.admits(T0 + 3600, T0 + 4200));
    }

    /// 報告範囲は検出を絞るだけで、基準の材料は絞らない (既定)。
    #[test]
    fn report_window_does_not_narrow_the_baseline_material() {
        let t = cpu_idle(&vals(&[10.0; 40]));
        let ts = single(t);

        let opts = DetectOptions {
            report_from: ReportBound::Epoch(T0 + 34 * STEP_SECS),
            ..Default::default()
        };
        let out = detect(&ts, &opts);
        let ev = out
            .evaluations
            .iter()
            .find(|e| e.series.column == "idle")
            .expect("A_CPU/all/idle の評価");
        assert_eq!(
            ev.baseline_samples, 40,
            "既定では報告範囲の外も基準の材料に入る"
        );
        assert_eq!(ev.baseline_basis, Some(BasisOrigin::InputItself));
    }

    /// `--baseline-scope window` では材料を報告範囲だけに絞る。
    #[test]
    fn baseline_scope_window_narrows_the_material() {
        let t = cpu_idle(&vals(&[10.0; 40]));
        let ts = single(t);
        let opts = DetectOptions {
            baseline_scope: BaselineScope::Window,
            report_from: ReportBound::Epoch(T0 + 30 * STEP_SECS),
            ..Default::default()
        };
        let out = detect(&ts, &opts);
        let ev = out
            .evaluations
            .iter()
            .find(|e| e.series.column == "idle")
            .expect("評価");
        assert!(ev.baseline_samples < 40);
        assert_eq!(ev.baseline_basis, Some(BasisOrigin::ReportWindowOnly));
    }

    /// カタログにあるのに入力に無い系列は「評価できなかった」として残る。
    #[test]
    fn absent_series_are_reported_as_not_evaluated() {
        let ts = single(cpu_idle(&vals(&[50.0; 20])));
        let out = detect(&ts, &DetectOptions::default());
        let absent = out.evaluations.iter().filter(|e| !e.present).count();
        assert!(absent > 0, "入力に無い系列を黙って落としてはいけない");
        let disk = out
            .evaluations
            .iter()
            .find(|e| e.series.activity == ActivityId::DISK)
            .expect("A_DISK の行");
        assert!(!disk.present);
    }
}

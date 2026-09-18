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
//!
//! 時間範囲の**両端も観測の意味で決める**。瞬時値に前サンプルの時刻を
//! 含めると、10:10 / 10:20 / 10:30 の 3 回の高値が「30 分にわたる 3 回の採取」
//! になり、採取時点の広がり (20 分) を 1 採取間隔ぶん長く報告する。
//! この範囲は headline・報告範囲フィルタ・優先度の昇格判定が共用するので、
//! 偽ると既定の昇格条件 (1800 秒) へ Gauge だけが 1 採取早く到達する。
//!
//! # 保存形式の種別と観測の意味を分ける
//!
//! [`crate::model::ValueKind`] は「ディスク上の値が累積カウンタか時点の量か」
//! という**保存・差分処理上の種別**である。検出で必要なのは
//! 「**その値が何を代表しているか**」で、両者は一致しない。
//! カタログの `await` は `ValueKind::Gauge` だが、実体は区間のカウンタ差分から
//! 計算した値 (`Δticks / ΔI/O 数`) である。
//! [`ObservationOrigin`] が観測の意味を持ち、時間範囲の両端と
//! 平均の取り方 ([`MeanBasis`]) をそこから決める。

pub mod episodes;
pub mod level_shift;
pub mod robust;
pub mod threshold;

use serde::Serialize;

use crate::analyze::assessment::SeriesEvaluation;
use crate::analyze::metric_catalog::{self, CatalogEntry, ShiftMagnitude};
use crate::analyze::timeline::{MetricKey, MetricTimeline, Timelines};
use crate::model::{ActivityId, DisplayTz, Lang, Text, Unit, ValueKind};
use crate::text;

/// 検出出力のスキーマ版。出力契約として固定する (`docs/design.md` §11)。
pub const DETECT_SCHEMA_VERSION: &str = crate::model::NATIVE_SCHEMA_VERSION;

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
///
/// # 範囲の両端は観測の意味で決まる
///
/// 区間値 (Counter の差分から計算したレート・比率) は区間を代表するので、
/// 範囲は**最初の区間の始点から最後の区間の終点**である。
/// 瞬時値 ([`ObservationOrigin::InstantGauge`]) は採取時点の値なので、
/// 範囲は**最初の採取時刻から最後の採取時刻**である
/// ([`PreparedSeries::support_of`] が origin を見て決める)。
///
/// 瞬時値に前サンプルの時刻を含めると、10 分採取で 3 回続いた高値が
/// 「30 分にわたる 3 回の採取」になる。実際に採取が広がっているのは 20 分で、
/// 残りの 10 分は**最初の採取より前**の時間である。
/// この範囲は headline・報告範囲フィルタ・優先度の昇格判定
/// ([`crate::analyze::assessment`]) が共用するので、偽ると既定の昇格条件
/// (`persistence_secs` = 1800 秒) へ Gauge だけが 1 採取早く到達する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct TemporalSupport {
    /// 範囲の始点 (エポック秒)。
    ///
    /// 区間値では最初の区間の始点、瞬時値では**最初の採取時刻**。
    pub start_ust: u64,
    /// 範囲の終点 (エポック秒)。最後の値を観測した時刻。
    pub end_ust: u64,
    /// 値が得られた採取回数。
    pub samples: u64,
    /// 系列全体で値が得られた採取回数 (欠測割合の分母)。
    pub series_observed_samples: u64,
    /// 系列全体で値が得られなかった採取回数。
    pub missing_samples: u64,
    /// 系列全体で不連続として捨てた区間数。
    pub discontinuities: u64,
    /// 観測された区間の長さの合計 (1/100 秒)。
    ///
    /// **瞬時値では 0。** 採取時点の値しか観測していないので、
    /// 長さを持つ区間を観測していない。
    pub observed_cs: u64,
}

impl TemporalSupport {
    /// 時間範囲の長さ (秒)。**採取が連続していたことを意味しない。**
    ///
    /// 瞬時値では「最初の採取から最後の採取まで」なので、
    /// 3 回の採取が 10 分間隔なら 20 分である (30 分ではない)。
    pub fn span_secs(&self) -> u64 {
        self.end_ust.saturating_sub(self.start_ust)
    }

    /// 「20 分にわたる 3 回の採取」の形の文。
    ///
    /// **「20 分間」とは書かない。** `sar` のデータは離散的な採取なので、
    /// 採取と採取の間に何が起きていたかは観測されていない。
    pub fn describe_span(&self, lang: Lang) -> String {
        let secs = self.span_secs();
        let n = self.samples;
        if secs == 0 {
            return match lang {
                Lang::Ja => format!("1 時点の {n} 回の採取"),
                Lang::En => format!("{n} samples at a single point in time"),
            };
        }
        let duration = describe_duration(secs, lang);
        match lang {
            Lang::Ja => format!("{duration}にわたる {n} 回の採取"),
            Lang::En => format!("{n} samples spanning {duration}"),
        }
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
            series_observed_samples: self
                .series_observed_samples
                .max(other.series_observed_samples),
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
pub fn describe_duration(secs: u64, lang: Lang) -> String {
    let (m, h) = (secs / 60, secs / 3600);
    match (secs, lang) {
        (s, Lang::Ja) if s < 60 => format!("{s} 秒"),
        (s, Lang::En) if s < 60 => format!("{s} seconds"),
        (s, Lang::Ja) if s < 3600 => format!("{m} 分"),
        (s, Lang::En) if s < 3600 => format!("{m} minutes"),
        (s, Lang::Ja) if s.is_multiple_of(3600) => format!("{h} 時間"),
        (s, Lang::En) if s.is_multiple_of(3600) => format!("{h} hours"),
        (_, Lang::Ja) => format!("{h} 時間 {} 分", (secs % 3600) / 60),
        (_, Lang::En) => format!("{h} hours {} minutes", (secs % 3600) / 60),
    }
}

// ===========================================================================
// 観測
// ===========================================================================

/// 観測の意味 — **その値が何を代表しているか**。
///
/// [`ValueKind`] (保存・差分処理上の種別) とは別の宣言である。
/// `ValueKind::Gauge` で保存されていても、カタログの `await` と `%ifutil` は
/// **区間のカウンタ差分から計算した値**であり、採取時点の量ではない
/// (`await = Δticks / Δ完了 I/O 数`、`%ifutil` は区間の通信量 ÷ リンク速度。
/// どちらも `series::compute` が前サンプルとの差分から作る)。
///
/// | 由来 | 何を代表するか | 時間範囲の始点 | 複数観測の束ね方 |
/// |---|---|---|---|
/// | [`IntervalRate`](ObservationOrigin::IntervalRate) | 区間全体 | 区間の始点 | 区間長で重み付けした平均 |
/// | [`InstantGauge`](ObservationOrigin::InstantGauge) | その瞬間 | 最初の採取時刻 | 採取ごとの単純平均 |
/// | [`PerRequestAverage`](ObservationOrigin::PerRequestAverage) | 区間の 1 要求あたり | 区間の始点 | **要求数で重み付けした平均** |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationOrigin {
    /// 区間を代表する値 (Counter の差分から計算したレート・比率)。
    ///
    /// 区間の内側でどう変動したかは観測されていない。
    /// 区間長を重みにした平均で束ねられる。
    IntervalRate,
    /// その時点の瞬時値 (Gauge 由来)。
    ///
    /// 前後の採取の間の値は観測されていない。
    /// **区間長は重みにならない** — 採取と採取の間隔が長いことは、
    /// その値が長く続いたことを意味しない。
    InstantGauge,
    /// 区間のカウンタ差分から「1 要求あたり」として計算した値 (`await`)。
    ///
    /// 区間を代表するが、複数区間を束ねるときの重みは区間長ではなく
    /// **要求数**である (`Σ(Δticks) / Σ(Δ要求数)`)。
    /// 要求が 1 件しか無かった区間と 1 万件あった区間を同じ重みで平均すると、
    /// 静かな区間の外れ値が全体の平均を動かす。
    PerRequestAverage,
}

impl ObservationOrigin {
    /// 系列の同定と列の性質から決める。
    ///
    /// **[`ValueKind`] だけでは決まらない。** 派生した区間値は
    /// `ValueKind::Gauge` で保存されるが瞬時値ではないので、
    /// 系列ごとの例外を [`derived_origin`] が持つ。
    pub fn of(key: &MetricKey, kind: ValueKind) -> Self {
        if let Some(origin) = derived_origin(key) {
            return origin;
        }
        match kind {
            ValueKind::Counter => ObservationOrigin::IntervalRate,
            // Identity 列は検出対象にしない (カタログのテストで固定してある)
            ValueKind::Gauge | ValueKind::Identity => ObservationOrigin::InstantGauge,
        }
    }

    /// 採取時点の値か (長さを持つ区間を代表しないか)。
    pub const fn is_instant(self) -> bool {
        matches!(self, ObservationOrigin::InstantGauge)
    }

    /// 複数観測を束ねるときの平均の取り方。
    pub const fn mean_basis(self) -> MeanBasis {
        match self {
            ObservationOrigin::IntervalRate => MeanBasis::TimeWeighted,
            ObservationOrigin::InstantGauge => MeanBasis::PerSample,
            ObservationOrigin::PerRequestAverage => MeanBasis::UnweightedPerRequest,
        }
    }

    pub const fn label(self) -> Text {
        match self {
            ObservationOrigin::IntervalRate => {
                text!(ja: "区間を代表する値 (差分から計算)", en: "a value representing the interval (computed from deltas)")
            }
            ObservationOrigin::InstantGauge => {
                text!(ja: "採取時点の値 (Gauge)", en: "the value at the moment of sampling (gauge)")
            }
            ObservationOrigin::PerRequestAverage => {
                text!(ja: "区間の 1 要求あたりの平均 (差分から計算)", en: "a per-request average over the interval (computed from deltas)")
            }
        }
    }
}

/// 派生した区間値の例外表。
///
/// `ValueKind::Gauge` で保存されているが採取時点の量ではない系列を挙げる。
/// **本来はカタログ ([`crate::analyze::metric_catalog::CatalogEntry`]) が
/// 系列ごとに観測の意味を宣言すべき情報である。** ここに置いているのは
/// カタログ側の宣言が入るまでの暫定で、Issue #5 の 3 で追跡している。
fn derived_origin(key: &MetricKey) -> Option<ObservationOrigin> {
    match (key.activity, key.column.as_str()) {
        // await = Σ(Δticks) / Δ完了 I/O 数 (`series::compute` の disk_derived)。
        // 区間の 1 要求あたりの平均なので、区間長では重み付けできない
        (ActivityId::DISK, "await") => Some(ObservationOrigin::PerRequestAverage),
        // %ifutil = 区間の通信量 (バイト毎秒) ÷ リンク速度 (`compute::ifutil`)。
        // 区間を代表する比率なので時間加重平均でよい
        (ActivityId::NET_DEV, "ifutil_pct") => Some(ObservationOrigin::IntervalRate),
        _ => None,
    }
}

/// 平均をどう取ったか。**取り方を偽らない。**
///
/// [`DecisionEvidence::mean`] の意味がこれで変わる。
/// 「時間加重平均」と書いた数字が実は単純平均だった、という取り違えを
/// 型で止める。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MeanBasis {
    /// 区間長で重み付けした平均。区間値はこれが正しい。
    TimeWeighted,
    /// 採取ごとの単純平均。
    ///
    /// 瞬時値は区間長を重みにできない (採取間隔はその値が続いた長さではない)。
    PerSample,
    /// 単純平均だが、**本来の重み (要求数) で重み付けしたものではない**。
    ///
    /// `await` の複数区間の平均は `Σ(Δticks) / Σ(Δ要求数)` である。
    /// 検出層には区間ごとの要求数が渡ってきていないので、
    /// 単純平均で代用していることをここで宣言する。
    /// 要求の少ない区間の値を過大に評価する側へ寄る。
    UnweightedPerRequest,
}

impl MeanBasis {
    pub const fn label(self) -> Text {
        match self {
            MeanBasis::TimeWeighted => {
                text!(ja: "区間長で重み付けした平均", en: "mean weighted by interval length")
            }
            MeanBasis::PerSample => {
                text!(ja: "採取ごとの単純平均", en: "plain mean over the samples")
            }
            MeanBasis::UnweightedPerRequest => {
                text!(ja: "単純平均 (本来必要な要求数の重みが無い)", en: "plain mean (without the request-count weighting it needs)")
            }
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
    ///
    /// 瞬時値では**前の採取からの間隔**であり、その値が続いた長さではない。
    pub fn interval_secs(&self) -> u64 {
        self.end_ust.saturating_sub(self.start_ust)
    }

    /// 報告する時刻の始点。
    ///
    /// 瞬時値は採取時点の値なので、前サンプルの時刻を範囲へ含めない
    /// ([`TemporalSupport`] の doc を参照)。
    pub fn reported_start_ust(&self) -> u64 {
        if self.origin.is_instant() {
            self.end_ust
        } else {
            self.start_ust
        }
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
    /// 向きを表す語。**文脈は呼び手が付ける** (「上昇側へ」「moved upward」)。
    pub const fn as_str(self, lang: Lang) -> &'static str {
        match self {
            ShiftDirection::Rise => text!(ja: "上昇", en: "upward").get(lang),
            ShiftDirection::Fall => text!(ja: "低下", en: "downward").get(lang),
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
    /// **境目の前後で水準が違う。**
    ///
    /// 「その時刻に変わった」ではない。指せるのは採用した前後窓の
    /// 分割時刻であって、採取と採取の間のどこで動いたかは観測されていない
    /// ([`level_shift`] の doc を参照)。
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
    pub const fn label(self) -> Text {
        match self {
            Pattern::Spike => text!(ja: "上方への逸脱", en: "deviation upward"),
            Pattern::Dip => text!(ja: "下方への逸脱", en: "deviation downward"),
            Pattern::LevelShift {
                direction: ShiftDirection::Rise,
            } => text!(ja: "水準の上昇", en: "a rise in level"),
            Pattern::LevelShift {
                direction: ShiftDirection::Fall,
            } => text!(ja: "水準の低下", en: "a fall in level"),
            Pattern::Saturation => text!(ja: "飽和", en: "saturation"),
            Pattern::Emergence => text!(ja: "事象の発生", en: "an event occurring"),
            Pattern::Depletion => text!(ja: "余裕の減少", en: "headroom shrinking"),
            Pattern::Sustained => {
                text!(ja: "継続した閾値超過", en: "a sustained breach of the threshold")
            }
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
    pub const fn label(self) -> Text {
        match self {
            DetectRoute::FixedCondition => text!(ja: "絶対水準", en: "absolute level"),
            DetectRoute::RobustDeviation => {
                text!(ja: "参照分布からの逸脱", en: "deviation from the reference distribution")
            }
            DetectRoute::LevelShift => text!(ja: "時間的変化", en: "change over time"),
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
    pub const fn label(self) -> Text {
        match self {
            BasisOrigin::InputItself => {
                text!(ja: "入力全体 (この入力自身が材料)", en: "the whole input (this input itself is the material)")
            }
            BasisOrigin::ReportWindowOnly => {
                text!(ja: "報告範囲のみ (この入力自身が材料)", en: "the report window only (this input itself is the material)")
            }
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
    ///
    /// **既定の設定では到達しない。** `MAD` は偏差の中央値なので、
    /// `MAD > 0` なら中央値と異なる値が少なくとも半数ある。
    /// [`DetectThresholds::min_off_center_share`] が 0.5 以下である限り
    /// 割合の条件は必ず満たされる。
    /// 「0 ではないが極端に小さい MAD」を別に扱いたければ、
    /// 割合ではない尺度の条件が必要である (割合の閾値を上げて代用しない)。
    TooSparse { off_center: u64, required: u64 },
    /// 基準を作るサンプルが足りない。
    InsufficientSamples { required: u64 },
}

impl Dispersion {
    pub const fn is_measured(self) -> bool {
        matches!(self, Dispersion::Measured)
    }

    pub const fn label(self) -> Text {
        match self {
            Dispersion::Measured => text!(ja: "測定できた", en: "measured"),
            Dispersion::NotMeasurable => {
                text!(ja: "MAD が 0 (値がほぼ一定) のため測れない", en: "MAD is 0 (the value barely moves), so it cannot be measured")
            }
            Dispersion::TooSparse { .. } => {
                text!(ja: "中央値から離れた値が少なすぎて測れない", en: "too few values sit away from the median to measure it")
            }
            Dispersion::InsufficientSamples { .. } => {
                text!(ja: "サンプル数が足りない", en: "too few samples")
            }
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
    /// 読み手に伝える留保。**[`DetectOptions::lang`] で解決済み。**
    ///
    /// **先頭は必ず [`BASELINE_CAVEAT_SELF_SOURCED`]** で、これは比較基準の
    /// 出所そのものを言い直したものである。2 件目以降がその系列に固有の留保
    /// (中央値が固定条件の内側、`MAD = 0`、要求当たりの平均) になる。
    pub caveats: Vec<&'static str>,
}

/// すべての比較基準に付く一般的な留保。
///
/// **この 1 件だけは系列を選ばない。** 比較基準は常に入力自身から作るので、
/// 検出ごとに繰り返しても読み手が得る情報は増えない。`text` の要約では
/// 出力層がこれを外し、報告の冒頭と末尾で 1 度ずつ言う
/// (規律 3 は「出所を必ず明示する」であって「毎検出で繰り返す」ではない)。
pub const BASELINE_CAVEAT_SELF_SOURCED: Text = text!(
    ja: "この基準は入力自身から作ったものであり、外部の正常値ではない",
    en: "this basis is built from the input itself; it is not an external notion of normal",
);

/// 散らばりが測れないときの留保 ([`Dispersion::NotMeasurable`])。
///
/// **検出の説明文が同じことを言う** ので、`text` の要約では出力層が外す。
/// 「散らばりが測れないため絶対差で判断した」と書いたうえで同じ留保を並べても、
/// 読み手が得る情報は増えない。
pub const BASELINE_CAVEAT_MAD_ZERO: Text = text!(
    ja: "MAD が 0 なので正規化した逸脱評価はできない。\
         宣言された最小有意変化量を超える差は絶対差として別に報告する",
    en: "MAD is 0, so no normalised deviation can be assessed. A difference beyond the declared \
         minimum significant change is reported separately, as an absolute difference",
);

/// 中央値と同じ値が大半を占めるときの留保 ([`Dispersion::TooSparse`])。
///
/// [`BASELINE_CAVEAT_MAD_ZERO`] と同じ理由で、要約では外す。
pub const BASELINE_CAVEAT_TOO_SPARSE: Text = text!(
    ja: "中央値と同じ値が大半を占める。\
         散らばりの推定材料にならないので正規化した逸脱評価はできない。\
         絶対差による観測は別に報告する",
    en: "most values equal the median, which leaves nothing to estimate spread from, so no \
         normalised deviation can be assessed. Observations by absolute difference are \
         reported separately",
);

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

    pub const fn label(self) -> Text {
        match self {
            FixedComparison::AtLeast => text!(ja: "以上", en: "at or above"),
            FixedComparison::AtMost => text!(ja: "以下", en: "at or below"),
            FixedComparison::Above => text!(ja: "超", en: "above"),
            FixedComparison::Below => text!(ja: "未満", en: "below"),
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
    /// 散らばりが測れないまま、**絶対差が大きかった**観測。
    ///
    /// [`RobustDeviation`](DecisionBasis::RobustDeviation) と**同じ観点**
    /// ([`DetectRoute::RobustDeviation`]) の別の状態である。
    /// `MAD == 0` を ε で割った結果ではない (規律 4 は守る) — 逸脱スコアを
    /// 出さない代わりに、「散らばりは測れないが、指標ごとの最小有意変化量を
    /// 超える差があった」という事実だけを報告する。
    ///
    /// これが無いと、値がほぼ一定の整数 Gauge で**大きな偽陰性**が出る。
    /// `runq-sz` が 144 点中 141 点 0 で 3 点だけ 100 の入力では、
    /// 固定条件が無く (CPU 数に依存するので置けない)、MAD が 0 で逸脱も出せず、
    /// 後窓の持続率は最大 3/5 で水準変化にも届かないため検出 0 件になる。
    ///
    /// **出力では「MAD の N 倍」と書かない。**
    /// 「散らばりが測れないため絶対差で判断した」と書き分ける。
    AbsoluteDeparture {
        /// 比較基準の中央値。
        reference: f64,
        /// なぜ正規化しなかったか (散らばりが測れない理由)。
        dispersion: Dispersion,
        /// 要求した絶対差の下限 (指標ごとの最小有意変化量)。
        min_absolute_deviation: f64,
        /// 最も離れた点の `|x - reference|`。
        peak_absolute_deviation: f64,
        direction: ShiftDirection,
    },
    /// 時間的変化。
    LevelShift {
        before_median: f64,
        after_median: f64,
        /// `after_median - before_median`。**観測された差**。
        shift: f64,
        /// `shift` のうち窓内の傾きで説明できる量。
        ///
        /// 滑らかな増加では前後窓の中央値差がそのまま傾向で説明できる。
        /// 段差ではないので、引いた残り (`step_shift`) で判定する。
        trend_explained_shift: f64,
        /// 傾向を引いた残りの段差。**判定はこちらで行う。**
        ///
        /// 符号は `shift` と同じで、大きさは `|shift|` を超えない
        /// (傾きの推定誤差で検出を増やさないため)。
        step_shift: f64,
        /// 絶対差の下限 (この指標の単位)。
        min_shift: f64,
        /// 前後の窓を各々の中央値で中心化した残差を束ねた MAD。
        ///
        /// **段差を含めたまま束ねると尺度が段差自身で膨らむ**ので、
        /// 窓ごとに中心化してから束ねる。
        pooled_mad: Option<f64>,
        /// `|step_shift| / (MAD_SCALE × pooled_mad)`。
        /// 散らばりが測れないときは `None` (**ε で割らない**)。
        normalized_shift: Option<f64>,
        normalized_threshold: f64,
        /// 後窓のうち前窓の中央値から同方向へ最小変化量以上離れた割合。
        persistence_share: f64,
        persistence_threshold: f64,
        /// 前後の窓の点数 (前後で同じ)。
        window_samples: u64,
        /// 設定が要求した窓幅 (秒)。**実際の窓幅ではない。**
        window_requested_secs: u64,
        /// 実際に使った窓幅 (秒) = 点数 × 採取間隔。
        ///
        /// 要求幅が採取間隔で割り切れない場合と、点数の下限
        /// ([`DetectThresholds::shift_window_min_samples`]) で底を打った場合に
        /// 要求幅より**長くなる**。既定の 600 秒採取では
        /// `max(ceil(1800/600), 5) = 5` 点なので、区間値では各窓 3000 秒になる。
        /// 「窓 1800 秒で判定した」と出さないためにこの値を渡す。
        window_secs: u64,
        before: TemporalSupport,
        after: TemporalSupport,
    },
}

impl DecisionBasis {
    pub const fn route(&self) -> DetectRoute {
        match self {
            DecisionBasis::FixedCondition { .. } => DetectRoute::FixedCondition,
            // 絶対差だけで判断した観測も「参照分布からの逸脱」の観点である。
            // **経路を 4 つ目にしない** (3 経路の枠組みを崩さない)
            DecisionBasis::RobustDeviation { .. } | DecisionBasis::AbsoluteDeparture { .. } => {
                DetectRoute::RobustDeviation
            }
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
    /// 平均。取り方は [`DecisionEvidence::mean_basis`] が宣言する。
    pub mean: f64,
    /// 平均の取り方。**観測の意味 ([`ObservationOrigin`]) から決まる。**
    ///
    /// すべてを時間加重平均にすると、瞬時値では「採取間隔が長い点」が
    /// 重くなり、`await` では「要求が 1 件しか無かった区間」が
    /// 要求 1 万件の区間と同じ重みになる。
    pub mean_basis: MeanBasis,
}

impl DecisionEvidence {
    /// 観測列から根拠を組み立てる。
    ///
    /// 平均の取り方は観測の意味から決める (**一律の時間加重平均にしない**)。
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
        let mean_basis = observations
            .first()
            .map_or(MeanBasis::PerSample, |o| o.origin.mean_basis());
        let mean = match mean_basis {
            // 区間長が取れない (全て 0) 場合は標本平均へ落とす
            MeanBasis::TimeWeighted if weight > 0.0 => weighted / weight,
            _ if observations.is_empty() => 0.0,
            _ => plain / observations.len() as f64,
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
            mean_basis,
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
    /// 人間向けの指標名 (**[`DetectOptions::lang`] で解決済み**)。
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
    pub possible_interpretations: Vec<&'static str>,
    /// この検出では確かめていないこと。
    pub not_established: Vec<&'static str>,
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
    /// 時刻。**毎日の壁時計**として比較する ([`ReportWindow::tz`] の基準)。
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
    /// `hh:mm[:ss]` をどのタイムゾーンの壁時計として読むか。
    ///
    /// **レポートの表示と同じ基準にする。** 表示を JST にしたまま
    /// 日内境界を UTC で切ると、`--from 09:00` が画面の 09:00 と 9 時間ずれる。
    /// `epoch` 指定の境界はタイムゾーンによらない。
    pub tz: DisplayTz,
}

impl ReportWindow {
    pub fn is_unbounded(&self) -> bool {
        self.from.is_none() && self.to.is_none()
    }

    /// 区間 `[start_ust, end_ust]` が報告範囲に**重なるか**。
    ///
    /// **両端が範囲内かではなく、重なりで判定する。**
    /// 端だけを見ると、報告範囲を**包含する**検出が捨てられる。
    /// 08:00〜12:00 に及ぶ検出へ `--from 09:00 --to 10:00` を指定すると
    /// 両端 (08:00 と 12:00) が範囲外なので消え、同じ範囲をエポック秒で
    /// 指定した場合 (こちらは重なりで判定していた) と結果が食い違う。
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
        // 検出区間が占める「時刻」も日跨ぎし得る弧なので、弧同士の重なりを見る。
        // 日内秒は表示と同じタイムゾーンで取る (`% 86_400` は UTC 固定になる)。
        arcs_overlap(
            self.tz.seconds_of_day(start_ust),
            self.tz.seconds_of_day(end_ust),
            lo,
            hi,
        )
    }
}

/// 時刻 `tod` が `[lo, hi]` に入るか (`lo > hi` は日跨ぎ)。
fn tod_in_window(tod: u64, lo: u64, hi: u64) -> bool {
    if lo <= hi {
        tod >= lo && tod <= hi
    } else {
        tod >= lo || tod <= hi
    }
}

/// 1 日を円と見たときの 2 つの弧が重なるか。
///
/// 弧 `[a1, a2]` と `[b1, b2]` は、どちらか一方の端がもう一方に入っていれば
/// 重なり、入っていなければ重ならない (どちらかが他方を包含する場合も、
/// 包含される側の端が相手の中に入る)。**端の一致だけを見ないための判定。**
fn arcs_overlap(a1: u64, a2: u64, b1: u64, b2: u64) -> bool {
    tod_in_window(a1, b1, b2)
        || tod_in_window(a2, b1, b2)
        || tod_in_window(b1, a1, a2)
        || tod_in_window(b2, a1, a2)
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
    /// None は全 activity。選択外のカタログ系列は入力欠落と区別して報告する。
    pub selected_activities: Option<Vec<ActivityId>>,
    pub baseline_scope: BaselineScope,
    pub report_from: ReportBound,
    pub report_to: ReportBound,
    /// `report_from` / `report_to` の `hh:mm[:ss]` を読むタイムゾーン。
    ///
    /// レポート出力に使うものと同じ値を入れる (CLI は `--timezone` で 1 つに決める)。
    pub tz: DisplayTz,
    /// 所見の文を組み立てる言語。
    ///
    /// **分析層が文を作る**ので、言語はここまで届く必要がある
    /// (`docs/design.md` §2: 出力層で文を作ると text と JSON で食い違う)。
    /// 出力層は [`crate::analyze::assessment::Assessment::lang`] を見る。
    pub lang: Lang,
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
    /// 不連続が起きた時刻。エピソードを跨がせないために使う。
    pub discontinuity_marks: Vec<DiscontinuityMark>,
}

/// 不連続が起きた 1 点。
///
/// **及ぶ範囲を持つ。** 再起動はその時刻をまたぐ全系列に効くが、
/// item の入れ替えはその系列だけの話である。区別しないと、
/// あるデバイスの着脱が無関係な系列の所見まで分断する
/// (エピソードの過剰分割)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiscontinuityMark {
    /// 不連続が起きた時刻 (区間の終点)。
    pub at_ust: u64,
    /// 全系列に及ぶか (再起動・採取の中断・ファイルの切り替え)。
    pub all_series: bool,
    /// 観測した系列。`all_series` が真でも「どこで気づいたか」として持つ。
    pub series: SeriesKey,
}

impl DiscontinuityMark {
    /// この不連続が `series` の所見を分断するか。
    pub fn blocks(&self, series: &[SeriesKey]) -> bool {
        self.all_series || series.contains(&self.series)
    }
}

impl PreparedSeries {
    /// 時系列から整える。
    pub fn from_timeline(t: &MetricTimeline) -> Self {
        let origin = ObservationOrigin::of(&t.key, t.kind);
        let mut observations: Vec<Observation> = Vec::new();
        let mut segments: Vec<(usize, usize)> = Vec::new();
        let mut marks: Vec<DiscontinuityMark> = Vec::new();
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
                // 及ぶ範囲を判定する。時刻が繋がらない / 区間長 0 は
                // レコード列の性質なので全系列に効く。理由が付いている場合は
                // その分類に従う (item 入れ替えはその系列だけ)。
                let all_series = p.elapsed_cs == 0
                    || !adjacent
                    || p.reason.is_some_and(|r| r.affects_all_series());
                marks.push(DiscontinuityMark {
                    at_ust: p.end_ust,
                    all_series,
                    series: SeriesKey::from_metric(&t.key),
                });
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
    ///
    /// 範囲の始点は**観測の意味で決める**。瞬時値に前サンプルの時刻を
    /// 含めると採取の広がりを 1 採取間隔ぶん長く報告する
    /// ([`TemporalSupport`] の doc を参照)。
    pub fn support_of(&self, observations: &[Observation]) -> TemporalSupport {
        let Some(first) = observations.first() else {
            return TemporalSupport::default();
        };
        let last = observations.last().expect("非空");
        TemporalSupport {
            start_ust: first.reported_start_ust(),
            end_ust: last.end_ust,
            samples: observations.len() as u64,
            series_observed_samples: self.observations.len() as u64,
            missing_samples: self.missing,
            discontinuities: self.discontinuities,
            // 瞬時値は長さを持つ区間を観測していない
            observed_cs: if self.origin.is_instant() {
                0
            } else {
                observations.iter().map(|o| o.elapsed_cs).sum()
            },
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

    let mut caveats: Vec<Text> = vec![BASELINE_CAVEAT_SELF_SOURCED];
    if median_flagged {
        caveats.push(text!(
            ja: "中央値そのものが固定条件を満たしている。基準が異常側へ寄っているため、\
                 この系列の逸脱検出は当てにならない",
            en: "the median itself meets a fixed condition. The basis leans toward the anomalous \
                 side, so deviation detection for this series cannot be trusted",
        ));
    } else if flagged >= 0.5 {
        caveats.push(text!(
            ja: "材料の半分以上が固定条件を満たしている。基準が異常側へ寄っている可能性がある",
            en: "more than half the material meets a fixed condition, so the basis may lean \
                 toward the anomalous side",
        ));
    }
    match dispersion {
        Dispersion::NotMeasurable => caveats.push(BASELINE_CAVEAT_MAD_ZERO),
        Dispersion::TooSparse { .. } => caveats.push(BASELINE_CAVEAT_TOO_SPARSE),
        _ => {}
    }
    // **要求当たりの平均は、要求数の重みが無いと期間全体へ合算できない。**
    // `await` の正しい合算は Σ(Δticks) / Σ(Δ要求数) であり、中央値も
    // 「要求当たり」ではなく「区間ごとの値の中央値」である
    if series.origin == ObservationOrigin::PerRequestAverage {
        caveats.push(text!(
            ja: "区間の 1 要求あたりの値なので、この基準は「要求当たりの平均」ではなく\
                 「区間ごとの値の分布」である。要求数の重みが検出層へ渡っていない",
            en: "the value is a per-request average over the interval, so this basis is the \
                 distribution of per-interval values, not a per-request mean. The request-count \
                 weighting does not reach the detection layer",
        ));
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
            caveats: caveats.into_iter().map(|c| c.get(opts.lang)).collect(),
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
    ///
    /// 及ぶ範囲を持つので、無関係な系列の item 入れ替えで
    /// エピソードが分断されることはない ([`DiscontinuityMark::blocks`])。
    pub discontinuity_marks: Vec<DiscontinuityMark>,
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
        tz: opts.tz,
    };

    let mut seen: Vec<&'static CatalogEntry> = Vec::new();
    let mut intervals: Vec<u64> = Vec::new();

    for timeline in timelines.iter() {
        if opts
            .selected_activities
            .as_ref()
            .is_some_and(|ids| !ids.contains(&timeline.key.activity))
        {
            continue;
        }
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
                out.discontinuity_marks.push(m.clone());
            }
        }

        let material = baseline_material(&series, opts, window);
        let baseline = build_baseline(&series, entry, material, opts);

        let mut eval = SeriesEvaluation::observed(&series, entry, &baseline.evidence, opts.lang);

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
            let excluded = opts
                .selected_activities
                .as_ref()
                .is_some_and(|ids| !ids.contains(&entry.activity));
            out.evaluations.push(if excluded {
                SeriesEvaluation::excluded(entry, opts.lang)
            } else {
                SeriesEvaluation::absent(entry, opts.lang)
            });
        }
    }

    // 報告範囲で検出を絞る (**基準の材料は絞らない**)。
    // 判定は**重なり**である。報告範囲を包含する検出を捨てない
    // ([`ReportWindow::admits`] の doc を参照)
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
    out.discontinuity_marks.sort_by(|a, b| {
        a.at_ust
            .cmp(&b.at_ust)
            .then_with(|| a.series.cmp(&b.series))
    });

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
        let text = s.describe_span(Lang::Ja);
        assert!(text.contains("20 分"), "{text}");
        assert!(text.contains("3 回の採取"), "{text}");
        // 「20 分間」と書いてはいけない (採取の間は観測していない)
        assert!(!text.contains("分間"), "{text}");
    }

    #[test]
    fn duration_is_rendered_in_readable_units() {
        assert_eq!(describe_duration(45, Lang::Ja), "45 秒");
        assert_eq!(describe_duration(600, Lang::Ja), "10 分");
        assert_eq!(describe_duration(7200, Lang::Ja), "2 時間");
        assert_eq!(describe_duration(5400, Lang::Ja), "1 時間 30 分");
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
        let idle = MetricKey::new(ActivityId::CPU, "all", "idle");
        assert_eq!(
            ObservationOrigin::of(&idle, ValueKind::Counter),
            ObservationOrigin::IntervalRate
        );
        let runq = MetricKey::new(ActivityId::QUEUE, "-", "runq_sz");
        assert_eq!(
            ObservationOrigin::of(&runq, ValueKind::Gauge),
            ObservationOrigin::InstantGauge
        );
    }

    /// 派生した区間値を「採取時点の値」に分類しない。
    ///
    /// `await` は `Δticks / Δ完了 I/O 数`、`%ifutil` は区間の通信量から
    /// 計算した値で、どちらも `ValueKind::Gauge` で保存されているが
    /// 採取時点の量ではない (`series::compute` の `disk_derived` / `ifutil`)。
    #[test]
    fn a_derived_interval_value_is_not_classified_as_an_instant() {
        let await_key = MetricKey::new(ActivityId::DISK, "dev8-0", "await");
        assert_eq!(
            ObservationOrigin::of(&await_key, ValueKind::Gauge),
            ObservationOrigin::PerRequestAverage,
            "await は区間の 1 要求あたりの平均"
        );
        let ifutil = MetricKey::new(ActivityId::NET_DEV, "eth0", "ifutil_pct");
        assert_eq!(
            ObservationOrigin::of(&ifutil, ValueKind::Gauge),
            ObservationOrigin::IntervalRate,
            "%ifutil は区間を代表する比率"
        );
        // 本物の瞬時値は瞬時値のまま
        let ldavg = MetricKey::new(ActivityId::QUEUE, "-", "ldavg_1");
        assert_eq!(
            ObservationOrigin::of(&ldavg, ValueKind::Gauge),
            ObservationOrigin::InstantGauge
        );
    }

    /// 平均の取り方は観測の意味で決まる。**一律の時間加重平均にしない。**
    #[test]
    fn the_mean_is_weighted_according_to_the_observation_origin() {
        let basis = || DecisionBasis::FixedCondition {
            condition_id: "test",
            comparison: FixedComparison::AtLeast,
            threshold: 0.0,
            min_samples: 1,
            rationale: "テスト",
        };
        // 区間長 600 秒の点 (値 10) と 2400 秒の点 (値 20)。
        // 時間加重平均は 18.0、単純平均は 15.0 になる並び
        let obs = |origin| {
            vec![
                Observation {
                    start_ust: T0,
                    end_ust: T0 + 600,
                    elapsed_cs: 60_000,
                    value: 10.0,
                    origin,
                },
                Observation {
                    start_ust: T0 + 600,
                    end_ust: T0 + 3000,
                    elapsed_cs: 240_000,
                    value: 20.0,
                    origin,
                },
            ]
        };

        let rate = DecisionEvidence::new(basis(), &obs(ObservationOrigin::IntervalRate));
        assert_eq!(rate.mean_basis, MeanBasis::TimeWeighted);
        assert!((rate.mean - 18.0).abs() < 1e-9, "{}", rate.mean);

        // 瞬時値では採取間隔が重みにならない (間隔が長いことはその値が
        // 長く続いたことを意味しない)
        let gauge = DecisionEvidence::new(basis(), &obs(ObservationOrigin::InstantGauge));
        assert_eq!(gauge.mean_basis, MeanBasis::PerSample);
        assert!((gauge.mean - 15.0).abs() < 1e-9, "{}", gauge.mean);

        // await は要求数の重みが必要。重みが無いことを宣言する
        let per_req = DecisionEvidence::new(basis(), &obs(ObservationOrigin::PerRequestAverage));
        assert_eq!(per_req.mean_basis, MeanBasis::UnweightedPerRequest);
        assert!((per_req.mean - 15.0).abs() < 1e-9, "{}", per_req.mean);
    }

    /// 瞬時値の時間範囲は**採取時点の広がり**である。
    ///
    /// 10 分採取で 3 回続いた高値は「20 分にわたる 3 回の採取」であり、
    /// 30 分ではない。この範囲は優先度の昇格判定
    /// (`persistence_secs` = 1800 秒) と共用されている。
    #[test]
    fn an_instant_gauge_reports_only_the_spread_of_its_samples() {
        let t = runq(&vals(&[0.0, 0.0, 9.0, 9.0, 9.0, 0.0]));
        let s = PreparedSeries::from_timeline(&t);
        let support = s.support_of(&s.observations[2..5]);

        assert_eq!(
            support.start_ust,
            T0 + 3 * STEP_SECS,
            "始点は最初の採取時刻 (前サンプルの時刻ではない)"
        );
        assert_eq!(support.end_ust, T0 + 5 * STEP_SECS);
        assert_eq!(support.span_secs(), 2 * STEP_SECS, "3 回の採取の広がり");
        assert!(support.describe_span(Lang::Ja).contains("20 分"));
        assert!(support.describe_span(Lang::Ja).contains("3 回の採取"));
        assert!(
            support.span_secs() < DetectThresholds::default().persistence_secs,
            "3 回の採取で昇格条件 (1800 秒) へ到達してはいけない"
        );
        assert_eq!(
            support.observed_cs, 0,
            "瞬時値は長さを持つ区間を観測していない"
        );
    }

    /// 区間値の時間範囲は最初の区間の始点から。
    #[test]
    fn an_interval_value_keeps_the_start_of_its_first_interval() {
        let t = cpu_idle(&vals(&[50.0; 6]));
        let s = PreparedSeries::from_timeline(&t);
        let support = s.support_of(&s.observations[2..5]);

        assert_eq!(support.start_ust, T0 + 2 * STEP_SECS);
        assert_eq!(support.span_secs(), 3 * STEP_SECS, "3 区間ぶん");
        assert!(
            support.span_secs() >= DetectThresholds::default().persistence_secs,
            "区間値は 3 区間で 1800 秒に達する"
        );
        assert_eq!(support.observed_cs, 3 * STEP_CS);
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
            // T0 は UTC 基準に置いた値なので、日内境界も UTC で見る
            tz: DisplayTz::Utc,
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

    /// 日内境界は**表示と同じタイムゾーン**で切る。
    ///
    /// `epoch % 86_400` で日内秒を出すと UTC 固定になり、
    /// 画面が JST でも `--from 09:00` だけが UTC の 09:00 を指してしまう。
    #[test]
    fn the_report_window_cuts_the_day_in_the_display_timezone() {
        let window = |tz| ReportWindow {
            tz,
            from: ReportBound::TimeOfDay {
                hour: 9,
                min: 0,
                sec: 0,
            },
            to: ReportBound::TimeOfDay {
                hour: 10,
                min: 0,
                sec: 0,
            },
        };
        // T0 = 2026-01-01 00:00:00 UTC = 同日 09:00 JST。
        // UTC 基準では 00:00 なので外れ、JST 基準では 09:00 なので当たる。
        let jst = DisplayTz::parse("Asia/Tokyo").expect("Asia/Tokyo");
        assert!(!window(DisplayTz::Utc).admits(T0, T0 + 600));
        assert!(window(jst).admits(T0, T0 + 600));

        // 逆向きも見る。UTC の 09:00 は JST では 18:00 なので範囲外。
        let utc_0900 = T0 + 9 * 3600;
        assert!(window(DisplayTz::Utc).admits(utc_0900, utc_0900 + 600));
        assert!(!window(jst).admits(utc_0900, utc_0900 + 600));
    }

    /// 報告範囲を**包含する**検出を捨てない。
    ///
    /// 08:00〜12:00 に及ぶ検出へ `--from 09:00 --to 10:00` を指定すると、
    /// 区間の両端 (08:00 と 12:00) はどちらも範囲外である。
    /// 端だけを見る判定ではこの検出が消え、**同じ範囲をエポック秒で
    /// 指定した場合と結果が食い違う**。
    #[test]
    fn a_detection_that_contains_the_report_window_is_kept() {
        let by_time = ReportWindow {
            // T0 は UTC 基準に置いた値なので、日内境界も UTC で見る
            tz: DisplayTz::Utc,
            from: ReportBound::TimeOfDay {
                hour: 9,
                min: 0,
                sec: 0,
            },
            to: ReportBound::TimeOfDay {
                hour: 10,
                min: 0,
                sec: 0,
            },
        };
        let by_epoch = ReportWindow {
            // T0 は UTC 基準に置いた値なので、日内境界も UTC で見る
            tz: DisplayTz::Utc,
            from: ReportBound::Epoch(T0 + 9 * 3600),
            to: ReportBound::Epoch(T0 + 10 * 3600),
        };
        let start = T0 + 8 * 3600;
        let end = T0 + 12 * 3600;

        assert!(
            by_time.admits(start, end),
            "報告範囲を包含する検出を捨ててはいけない"
        );
        assert_eq!(
            by_time.admits(start, end),
            by_epoch.admits(start, end),
            "時刻指定とエポック秒指定で結果が変わってはいけない"
        );
        // 重ならない検出は捨てる
        assert!(!by_time.admits(T0 + 12 * 3600, T0 + 13 * 3600));
        assert!(!by_time.admits(T0 + 7 * 3600, T0 + 8 * 3600));
        // 端が 1 点だけ触れる場合は重なりとして扱う
        assert!(by_time.admits(T0 + 7 * 3600, T0 + 9 * 3600));
    }

    /// 日跨ぎの検出区間と日跨ぎの報告範囲でも重なりで判定する。
    #[test]
    fn overlap_is_detected_across_midnight_on_both_sides() {
        let overnight = ReportWindow {
            // T0 は UTC 基準に置いた値なので、日内境界も UTC で見る
            tz: DisplayTz::Utc,
            from: ReportBound::TimeOfDay {
                hour: 0,
                min: 30,
                sec: 0,
            },
            to: ReportBound::TimeOfDay {
                hour: 1,
                min: 0,
                sec: 0,
            },
        };
        // 23:00 から翌 03:00 までの検出は 00:30〜01:00 を含む
        let start = T0 + 23 * 3600;
        assert!(overnight.admits(start, start + 4 * 3600));

        let window = ReportWindow {
            // T0 は UTC 基準に置いた値なので、日内境界も UTC で見る
            tz: DisplayTz::Utc,
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
        // 21:00〜23:00 の検出は 22:00 以降と重なる
        assert!(window.admits(T0 + 21 * 3600, T0 + 23 * 3600));
        // 19:00〜20:00 はどこにも重ならない
        assert!(!window.admits(T0 + 19 * 3600, T0 + 20 * 3600));
    }

    /// `--from` が `--to` より後ろなら日跨ぎとして扱う。
    #[test]
    fn an_overnight_window_wraps_around_midnight() {
        let w = ReportWindow {
            // T0 は UTC 基準に置いた値なので、日内境界も UTC で見る
            tz: DisplayTz::Utc,
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
            // T0 は UTC 基準に置いた値なので、日内境界も UTC で見る
            tz: DisplayTz::Utc,
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
            lang: Lang::Ja,
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
            lang: Lang::Ja,
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
    fn activity_selection_is_not_reported_as_source_absence() {
        use crate::analyze::assessment::{NotEvaluated, RouteStatus};
        let opts = DetectOptions {
            lang: Lang::Ja,
            selected_activities: Some(vec![ActivityId::CPU]),
            ..Default::default()
        };
        let out = detect(&single(cpu_idle(&vals(&[90.0; 20]))), &opts);
        for eval in out.evaluations {
            if eval.series.activity == ActivityId::CPU {
                continue;
            }
            assert_eq!(
                eval.fixed_condition,
                RouteStatus::NotEvaluated {
                    reason: NotEvaluated::ExcludedBySelection
                }
            );
            assert_eq!(eval.robust_deviation, eval.fixed_condition);
            assert_eq!(eval.level_shift, eval.fixed_condition);
        }
    }

    #[test]
    fn absent_series_are_reported_as_not_evaluated() {
        let ts = single(cpu_idle(&vals(&[50.0; 20])));
        let out = detect(
            &ts,
            &DetectOptions {
                lang: Lang::Ja,
                ..Default::default()
            },
        );
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

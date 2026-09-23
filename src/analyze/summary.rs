//! 期間集計 — **reSARch 独自のサマリ**。
//!
//! # 本家の `Average:` 行とは別物
//!
//! sysstat の `Average:` 行には性質の異なる方式が混在する
//! (`docs/format/03-output-format.md` §1.6)。
//!
//! | 本家の方式 | 対象 | 計算 |
//! |---|---|---|
//! | A: 差分方式 | カウンタ型 | **最初のサンプルと最後のサンプルの差分**を全期間 itv で割る (中間サンプルを一切使わない) |
//! | B: 累積平均方式 | ゲージ型 | 表示したサンプル値を加算し、`合計 / 表示サンプル数` |
//! | ハイブリッド | `A_PSI_*` | 移動平均列は B、合計由来の列は A |
//!
//! 本家方式 A は**途中の RESTART やカウンタのラップを無視する**。
//! 再起動を挟むと「最後 − 最初」が負になり、符号なし減算で巨大な値が出る。
//!
//! このモジュールが出す [`NativePeriodSummary`] は**それとは別の独自集計**である。
//!
//! - 区間ごとに差分を取り、**不連続な区間を除外**してから合算する
//! - 除外した区間数と理由を必ず報告する ([`ColumnSummary::exclusions`])
//! - 最大 / 最小 / 平均 / p95 を出し、**どの方式で集計したか**を
//!   [`ColumnSummary::method`] と [`SummarySpec`] に明示する
//!
//! 本家互換の `Average:` 行は `output` 層 (互換出力) の担当であり、
//! ここで作る値をそのまま流用してはならない。型名 (`Native*`) と
//! [`SUMMARY_KIND`] で区別できるようにしてある。
//!
//! # 値の由来
//!
//! **区間値は `series::compute` から取る。** ゲージの単位変換
//! (`ldavg-1` の 1/100 固定小数、`double` として保存された温度・電圧など) や
//! activity 固有の補正をこの層で再実装すると、同じ指標が出力形式ごとに
//! 違う値になる。集計層が独自に計算するのは
//! 「レート集計の分子となる差分」と「その分母」だけである。

use serde::Serialize;

use crate::error::Result;
use crate::format::file::SaFile;
use crate::layout::registry::{ColumnMeta, ItemShape, lookup};
use crate::model::{ActivityId, Aggregation, Availability, Unit, ValueKind};
use crate::series::compute::{
    ComputeContext, RateSample, column_value_strict, prepare_item, rate_from_totals, rate_sample,
    raw_column, tick_total,
};
use crate::series::snapshot::{
    ActivitySnapshot, IntervalView, ItemSnapshot, Selection, WalkItem, walk_items,
};

use super::percentile::{PercentileResult, PercentileSpec, PercentileUnavailable, WeightedSamples};
use super::timeline::{ExclusionReason, MetricKey, MetricPoint, SINGLE_ITEM, Timelines};

/// 独自サマリのスキーマ版。出力契約として固定する (`docs/design.md` §11)。
pub const SUMMARY_SCHEMA_VERSION: &str = crate::model::NATIVE_SCHEMA_VERSION;

/// このサマリの種別。本家 `Average:` 行と混同されないよう出力に含める。
pub const SUMMARY_KIND: &str = "resarch_native_summary";

// ===========================================================================
// 集計方式の宣言
// ===========================================================================

/// 実際に適用した集計方式。
///
/// [`Aggregation`] は「列がどう集計されるべきか」の宣言だが、
/// 派生列のようにそのとおりに集計できない列がある。
/// **宣言ではなく実際に適用した方式**を出力に載せる。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AggregationMethod {
    /// 有効区間の差分合計 ÷ 有効区間の分母合計 × 100。
    ///
    /// 分母は [`RateDenominator`] で示す。不連続な区間は分子にも分母にも入れない。
    RateOverValidIntervals,
    /// 区間値の標本平均 (1 サンプル = 重み 1)。本家方式 B と同じ重み付け。
    SampleMean,
    /// 区間値の時間加重平均。
    ///
    /// 各サンプル値は「前サンプルから当サンプルまでの区間」を代表するものとして
    /// 区間長で重み付けする。前サンプルが無い / 連続でない区間は重み 0 とし、
    /// 平均に寄与させない (区間長そのものが信頼できないため)。
    TimeWeightedMean,
    /// 最後の有効観測値。
    LastValid,
    /// 有効区間の差分合計。
    DeltaSum,
    /// 区間値の標本平均 (派生列)。
    ///
    /// 派生列は複数フィールドから計算するため単一の差分が存在せず、
    /// レートの期間集計 ([`AggregationMethod::RateOverValidIntervals`]) に還元できない。
    /// **宣言が `RateOverPeriod` でも期間レートにはならない**ので別の方式として示す。
    DerivedIntervalSampleMean,
    /// 集計しない (識別子列など)。
    NotAggregated,
}

/// レート集計の分母。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RateDenominator {
    /// 区間長 (1/100 秒)。結果は毎秒あたりの値になる。
    ElapsedCentiseconds,
    /// その item の CPU tick 合計。結果は百分率になる。
    CpuTicks,
}

/// ゲージ列の平均方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GaugeMeanMode {
    /// 標本平均 (既定)。本家方式 B と同じ重み付けなので比較しやすい。
    Sample,
    /// 時間加重平均。採取間隔が不均一なログで実時間の平均を出したいとき。
    TimeWeighted,
}

/// どの指標の時系列を保持するか。
///
/// 時系列は判定 (継続時間の検査) に必要だが、全列を保持するとメモリを食う。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum RetainTimelines {
    /// 組み込みルールが必要とする指標だけ (既定)。
    #[default]
    RuleInputs,
    /// 指定した指標だけ。
    Only(Vec<MetricKey>),
    /// すべての列。
    All,
    /// 保持しない。
    None,
}

/// 集計の設定。
#[derive(Debug, Clone)]
pub struct SummaryOptions {
    pub percentile: PercentileSpec,
    pub gauge_mean: GaugeMeanMode,
    pub retain: RetainTimelines,
}

impl Default for SummaryOptions {
    fn default() -> Self {
        Self {
            percentile: PercentileSpec::p95_sample_weighted(),
            gauge_mean: GaugeMeanMode::Sample,
            retain: RetainTimelines::default(),
        }
    }
}

/// 集計方式の宣言。出力メタデータとしてそのまま載せる。
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct SummarySpec {
    /// 分位点の宣言 (重み付け・アルゴリズム・補間)。
    pub percentile: PercentileSpec,
    pub gauge_mean: GaugeMeanMode,
    /// カウンタ列のレート集計方式 (常に有効区間のみ)。
    pub rate_method: AggregationMethod,
}

impl SummarySpec {
    fn from_options(o: &SummaryOptions) -> Self {
        Self {
            percentile: o.percentile,
            gauge_mean: o.gauge_mean,
            rate_method: AggregationMethod::RateOverValidIntervals,
        }
    }
}

// ===========================================================================
// 出力型
// ===========================================================================

/// 集計対象の出自。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SummarySource {
    /// 表示用のラベル (ホスト名や `--host` で付けた別名)。
    pub label: String,
    pub nodename: Option<String>,
    pub release: Option<String>,
    pub machine: Option<String>,
    pub cpu_nr: Option<u32>,
    pub tzname: Option<String>,
    /// 集計に使ったファイル (指定順)。
    pub files: Vec<String>,
    /// 起動区間の索引 (複数ファイル横断時)。
    pub boot_segment: Option<usize>,
}

/// 期間の範囲。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct PeriodBounds {
    /// 集計期間の始点 (エポック秒) = 最初に採った区間の始点。
    ///
    /// 系列の先頭サンプルは区間を持たないのでそのサンプルの時刻になる。
    /// 時刻フィルタで絞った場合は、範囲に最初に合致したサンプル
    /// (差分の基準として消費したもの) の時刻になる。
    pub first_ust: Option<u64>,
    /// 最後のサンプルの時刻 (エポック秒)。
    pub last_ust: Option<u64>,
    /// 集計に入ったサンプル数。
    pub samples: u64,
    /// 連続と判定できた区間数。
    pub continuous_intervals: u64,
    /// 不連続として扱った区間数 (サンプル境界の数)。
    pub broken_intervals: u64,
    /// 連続区間の長さの合計 (1/100 秒)。
    pub covered_cs: u64,
}

/// 除外理由ごとの件数。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ExclusionCount {
    pub reason: ExclusionReason,
    pub intervals: u64,
}

/// 極値とその観測時刻。
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Extremum {
    pub value: f64,
    /// 観測区間の始点 (エポック秒)。
    pub start_ust: u64,
    /// 観測区間の終点 (エポック秒)。
    pub end_ust: u64,
}

/// p95 の算出結果。出せなかった場合は理由を持つ。
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PercentileOutcome {
    Computed(PercentileResult),
    Unavailable { reason: PercentileUnavailable },
}

/// 1 列の集計結果。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ColumnSummary {
    /// 独自出力での列名 (`ColumnMeta::public_name`)。
    pub column: &'static str,
    /// `sar` のヘッダ名 (無い列は空文字)。
    pub sar_header: &'static str,
    pub unit: Unit,
    pub kind: ValueKind,
    /// 列メタデータが宣言する集計方法。
    pub declared_aggregation: Aggregation,
    /// 実際に適用した方式。
    pub method: AggregationMethod,
    /// 宣言どおりに集計できたか。派生列では `false` になる。
    pub matches_declared: bool,
    /// レート集計の分母 (レート以外は `None`)。
    pub rate_denominator: Option<RateDenominator>,
    pub max: Option<Extremum>,
    pub min: Option<Extremum>,
    /// 期間平均。方式は [`ColumnSummary::method`] のとおり。
    pub mean: Option<f64>,
    pub p95: PercentileOutcome,
    /// 差分の合計 (レート / 合計集計のみ)。十進文字列で出す
    /// (`u64` の生値は JavaScript 系で精度が落ちるため。`docs/design.md` §11)。
    pub delta_total: Option<String>,
    /// 分母の合計 (レート集計のみ)。
    pub denominator_total: Option<String>,
    /// 集計に入った区間数。
    pub intervals: u64,
    /// うちカウンタのラップを復元して採用した区間数。
    pub wrapped_intervals: u64,
    /// 除外した区間数の合計。
    pub excluded_intervals: u64,
    /// 除外の内訳。
    pub exclusions: Vec<ExclusionCount>,
}

impl ColumnSummary {
    /// 有効な観測があったか。
    pub fn has_observation(&self) -> bool {
        self.intervals > 0
    }
}

/// 1 item の集計結果。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ItemSummary {
    /// item のラベル (`all` / `cpu0` / `sda` / `-`)。
    pub item: String,
    /// ファイル上の識別子 (デバイス名など)。持たない activity は `None`。
    pub key: Option<String>,
    pub columns: Vec<ColumnSummary>,
}

/// 1 activity の集計結果。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ActivitySummary {
    pub activity: ActivityId,
    pub activity_name: String,
    pub items: Vec<ItemSummary>,
}

/// 独自の期間サマリ。**本家 `Average:` 行とは別物**。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NativePeriodSummary {
    pub schema_version: &'static str,
    /// 種別。常に [`SUMMARY_KIND`]。
    pub summary_kind: &'static str,
    /// 集計方式の宣言。
    pub spec: SummarySpec,
    pub source: SummarySource,
    pub period: PeriodBounds,
    pub activities: Vec<ActivitySummary>,
    /// 全列を合わせた除外の内訳。
    pub exclusions: Vec<ExclusionCount>,
    /// 判定に使う指標の時系列 (保持設定に従う)。
    pub timelines: Timelines,
}

impl NativePeriodSummary {
    pub fn activity(&self, id: ActivityId) -> Option<&ActivitySummary> {
        self.activities.iter().find(|a| a.activity == id)
    }

    /// activity / item / 列で 1 列の集計を引く。
    pub fn column(&self, id: ActivityId, item: &str, column: &str) -> Option<&ColumnSummary> {
        self.activity(id)?
            .items
            .iter()
            .find(|i| i.item == item)?
            .columns
            .iter()
            .find(|c| c.column == column)
    }
}

// ===========================================================================
// 蓄積器
// ===========================================================================

#[derive(Debug)]
struct ColumnAccum {
    /// どの activity の何列目か。
    ///
    /// 「差分合計 ÷ 分母合計」から表示単位のレートを出すには
    /// [`rate_from_totals`] に列を渡す必要がある (列ごとに保存値のスケールが
    /// 違う: `rkB/s` は 1/2、`aqu-sz` は 1/1000、`%util` は 1/10)。
    id: ActivityId,
    column: usize,
    meta: ColumnMeta,
    method: AggregationMethod,
    rate_denominator: Option<RateDenominator>,
    max: Option<Extremum>,
    min: Option<Extremum>,
    delta_total: u128,
    denom_total: u128,
    /// 区間値の単純合計 (標本平均用)。
    value_sum: f64,
    /// 区間値 × 区間長 の合計 (時間加重平均用)。
    weighted_sum: f64,
    /// 区間長の合計 (時間加重平均の分母)。
    weight_sum: f64,
    intervals: u64,
    wrapped: u64,
    last: Option<f64>,
    pct: WeightedSamples,
    exclusions: Vec<ExclusionCount>,
}

/// 蓄積器の生成に必要な設定だけを写したもの。
///
/// `&self.opts` を引き回すと `&mut self` と借用が衝突するため、
/// `Copy` な最小限の設定として切り出す。
#[derive(Debug, Clone, Copy)]
struct AccumConfig {
    percentile: PercentileSpec,
    gauge_mean: GaugeMeanMode,
    /// レートの分母が CPU tick 合計か (`A_CPU`)。区間長との違いを出力に残す。
    cpu_ticks: bool,
}

impl AccumConfig {
    fn of(opts: &SummaryOptions, cpu_ticks: bool) -> Self {
        Self {
            percentile: opts.percentile,
            gauge_mean: opts.gauge_mean,
            cpu_ticks,
        }
    }
}

impl ColumnAccum {
    fn new(id: ActivityId, column: usize, meta: ColumnMeta, cfg: AccumConfig) -> Self {
        let (method, denom) = plan_method(&meta, cfg);
        Self {
            id,
            column,
            meta,
            method,
            rate_denominator: denom,
            max: None,
            min: None,
            delta_total: 0,
            denom_total: 0,
            value_sum: 0.0,
            weighted_sum: 0.0,
            weight_sum: 0.0,
            intervals: 0,
            wrapped: 0,
            last: None,
            pct: WeightedSamples::new(cfg.percentile),
            exclusions: Vec::new(),
        }
    }

    fn exclude(&mut self, reason: ExclusionReason) {
        match self.exclusions.iter_mut().find(|e| e.reason == reason) {
            Some(e) => e.intervals += 1,
            None => self.exclusions.push(ExclusionCount {
                reason,
                intervals: 1,
            }),
        }
    }

    /// 区間値を 1 つ受け取る。
    ///
    /// `weight_cs` は時間重みに使う区間長。前サンプルが無い / 連続でない場合は 0。
    fn observe(&mut self, value: f64, weight_cs: u64, start_ust: u64, end_ust: u64) {
        if !value.is_finite() {
            self.exclude(ExclusionReason::NotFinite);
            return;
        }
        self.intervals += 1;
        let ex = Extremum {
            value,
            start_ust,
            end_ust,
        };
        if self.max.is_none_or(|m| value > m.value) {
            self.max = Some(ex);
        }
        if self.min.is_none_or(|m| value < m.value) {
            self.min = Some(ex);
        }
        self.value_sum += value;
        self.weighted_sum += value * weight_cs as f64;
        self.weight_sum += weight_cs as f64;
        self.last = Some(value);
        self.pct.push(value, weight_cs);
    }

    fn mean(&self) -> Option<f64> {
        match self.method {
            // **保存値 → 表示単位の換算を計算層に委ねる (指摘 2)。**
            //
            // ここで `delta_total / denom_total * 100.0` を直接返すと、
            // 生の差分の単位がそのまま出てしまい、瞬時値から作る
            // 最大 / 最小 / p95 と平均の単位が食い違う
            // (`rkB/s` が 2 倍、`aqu-sz` が 1,000 倍、`%util` が 10 倍、
            // PSI の圧力割合が 10,000 倍)。列ごとの除数は計算層が
            // [`rate_divisor`] に持っているので、換算はそこへ一本化する。
            AggregationMethod::RateOverValidIntervals => {
                rate_from_totals(self.id, self.column, self.delta_total, self.denom_total)
            }
            // 差分の総量そのもの (`read_ticks` などの内部フィールド)。
            // レートではないので `rate_divisor` では割らない。割るべき列が
            // `Aggregation::Sum` に現れないことは
            // `sum_columns_need_no_rate_divisor` が機械的に確認する。
            AggregationMethod::DeltaSum => {
                if self.intervals == 0 {
                    return None;
                }
                Some(self.delta_total as f64)
            }
            AggregationMethod::LastValid => self.last,
            AggregationMethod::TimeWeightedMean => {
                if self.weight_sum > 0.0 {
                    Some(self.weighted_sum / self.weight_sum)
                } else {
                    None
                }
            }
            AggregationMethod::SampleMean | AggregationMethod::DerivedIntervalSampleMean => {
                if self.intervals == 0 {
                    return None;
                }
                // 標本平均は 1 区間 = 重み 1。時間加重とは別の合計を使う。
                Some(self.value_sum / self.intervals as f64)
            }
            AggregationMethod::NotAggregated => None,
        }
    }

    fn finish(mut self) -> ColumnSummary {
        let mean = self.mean();
        let p95 = match self.pct.quantile() {
            Ok(r) => PercentileOutcome::Computed(r),
            Err(reason) => PercentileOutcome::Unavailable { reason },
        };
        self.exclusions.sort_by_key(|e| e.reason);
        let excluded_intervals = self.exclusions.iter().map(|e| e.intervals).sum();
        let is_rate = matches!(self.method, AggregationMethod::RateOverValidIntervals);
        let has_delta = is_rate || matches!(self.method, AggregationMethod::DeltaSum);

        ColumnSummary {
            column: self.meta.public_name,
            sar_header: self.meta.sar_header,
            unit: self.meta.unit,
            kind: self.meta.kind,
            declared_aggregation: self.meta.aggregation,
            method: self.method,
            matches_declared: !matches!(self.method, AggregationMethod::DerivedIntervalSampleMean),
            rate_denominator: self.rate_denominator,
            max: self.max,
            min: self.min,
            mean,
            p95,
            delta_total: has_delta.then(|| self.delta_total.to_string()),
            denominator_total: is_rate.then(|| self.denom_total.to_string()),
            intervals: self.intervals,
            wrapped_intervals: self.wrapped,
            excluded_intervals,
            exclusions: self.exclusions,
        }
    }
}

/// 列メタデータと設定から、適用する方式と分母を決める。
fn plan_method(
    meta: &ColumnMeta,
    cfg: AccumConfig,
) -> (AggregationMethod, Option<RateDenominator>) {
    if !meta.is_direct() {
        // 派生列は単一の差分を持たないため、期間レートには還元できない
        return (AggregationMethod::DerivedIntervalSampleMean, None);
    }
    let denominator = if cfg.cpu_ticks {
        RateDenominator::CpuTicks
    } else {
        RateDenominator::ElapsedCentiseconds
    };
    match meta.aggregation {
        Aggregation::RateOverPeriod => {
            (AggregationMethod::RateOverValidIntervals, Some(denominator))
        }
        Aggregation::Sum => (AggregationMethod::DeltaSum, None),
        Aggregation::Last => (AggregationMethod::LastValid, None),
        Aggregation::Mean => match cfg.gauge_mean {
            GaugeMeanMode::Sample => (AggregationMethod::SampleMean, None),
            GaugeMeanMode::TimeWeighted => (AggregationMethod::TimeWeightedMean, None),
        },
        Aggregation::NotAggregated => (AggregationMethod::NotAggregated, None),
    }
}

#[derive(Debug)]
struct ItemAccum {
    label: String,
    key: Option<String>,
    columns: Vec<ColumnAccum>,
}

#[derive(Debug)]
struct ActivityAccum {
    id: ActivityId,
    items: Vec<ItemAccum>,
}

// ===========================================================================
// 集計器
// ===========================================================================

/// 区間を順に受け取り、独自サマリを組み立てる。
///
/// 複数ファイル横断では、ファイル境界をまたいだ区間も
/// [`NativeSummaryBuilder::observe`] へ渡すことで 1 つの系列として集計できる
/// (`src/multi.rs` がその繋ぎを担う)。**差分をファイル単位で完結させない**。
///
/// # 期間を絞るのは呼び出し側
///
/// `--from` / `--to` による絞り込みは、**渡す区間を選ぶ**ことで行う
/// ([`crate::multi::MultiOptions::time_filter`])。この集計器は受け取った区間を
/// すべて数えるので、範囲外のサンプルが平均や p95 の重みに混ざる余地が無い。
/// 集計器側で時刻を見る作りにすると、「範囲に最初に合致したレコードを
/// 差分の起点としてだけ使う」`sar -s` の意味論や、ファイルごとに
/// フィルタを引き直す必要 (日ごとの時間帯指定) を持ち込むことになる。
#[derive(Debug)]
pub struct NativeSummaryBuilder {
    opts: SummaryOptions,
    activities: Vec<ActivityAccum>,
    timelines: Timelines,
    period: PeriodBounds,
}

impl NativeSummaryBuilder {
    pub fn new(opts: SummaryOptions) -> Self {
        Self {
            opts,
            activities: Vec::new(),
            timelines: Timelines::new(),
            period: PeriodBounds::default(),
        }
    }

    /// 1 区間 (前サンプルと当サンプルの対) を集計へ加える。
    pub fn observe(&mut self, view: &IntervalView<'_>) {
        if !view.curr.valid {
            return;
        }
        self.period.samples += 1;
        let end_ust = view.curr.ust_time;
        let start_ust = if view.has_prev {
            view.prev.ust_time
        } else {
            end_ust
        };
        // 期間の始点は**最初に採った区間の始点**。
        //
        // 当サンプルの時刻を入れると、時刻フィルタ (`--from`) で絞ったときに
        // 「基準サンプル → 最初に数えた区間」の長さが `covered_cs` に入るのに
        // `first_ust` には現れず、`covered_cs` と `last_ust − first_ust` が
        // 食い違う (レートの分母を読む側が期間を取り違える)。
        // 系列の先頭サンプルは前サンプルが無く `start_ust == end_ust` なので、
        // 絞らないときの値は変わらない。
        if self.period.first_ust.is_none() {
            self.period.first_ust = Some(start_ust);
        }
        self.period.last_ust = Some(end_ust);

        // 区間長を重みに使えるのは「前サンプルがあり、かつ連続」な場合だけ。
        // 再起動を挟むと uptime 差分が意味を失うため、重み 0 として扱う。
        let weight_cs = if view.has_prev && view.continuous {
            view.itv_cs
        } else {
            0
        };
        if weight_cs > 0 {
            self.period.continuous_intervals += 1;
            self.period.covered_cs = self.period.covered_cs.saturating_add(weight_cs);
        } else {
            self.period.broken_intervals += 1;
        }

        let timing = Timing {
            start_ust,
            end_ust,
            weight_cs,
            itv_cs: view.itv_cs,
        };

        for snap in &view.curr.activities {
            self.observe_activity(view, snap, timing);
        }
    }

    fn observe_activity(
        &mut self,
        view: &IntervalView<'_>,
        snap: &ActivitySnapshot,
        timing: Timing,
    ) {
        let Some(def) = lookup(snap.id) else { return };
        let Some(plan) = view.plan_for(snap.id) else {
            return;
        };
        let prev_snap = view.prev.activity(snap.id);
        let normalize_by_ticks = normalizes_by_cpu_ticks(snap.id);
        let cfg = AccumConfig::of(&self.opts, normalize_by_ticks);

        let irq_transposed =
            snap.id == ActivityId::IRQ && (snap.nr2 > 1 || plan.text_index("irq_name").is_some());
        let item_count = if irq_transposed {
            snap.nr2 as usize
        } else {
            snap.items.len()
        };
        for (index, item) in snap.items.iter().take(item_count).enumerate() {
            let label = if snap.id == ActivityId::DISK {
                let field = |name| {
                    def.columns
                        .iter()
                        .position(|c| c.public_name == name)
                        .and_then(|c| raw_column(plan, item, c).ok())
                };
                match (field("major"), field("minor")) {
                    (Some(major), Some(minor)) => format!("dev{major}-{minor}"),
                    _ => format!("dev{index}"),
                }
            } else {
                item_label(snap.id, def.shape, index, item.key.as_deref())
            };
            // 前サンプルの同一 item を探す。**位置ではなく識別子で対応付ける**。
            let (prev_item, same_item) = match (prev_snap, item.key.as_deref()) {
                (Some(p), Some(k)) => match p.item_by_key(k) {
                    Some(pi) => (Some(pi), true),
                    // 同じ名前の item が前サンプルに無い = 着脱または名前の再利用
                    None => (None, false),
                },
                (Some(p), None) => match p.items.get(index) {
                    Some(pi) => (Some(pi), true),
                    None => (None, false),
                },
                (None, _) => (None, false),
            };

            let ctx = ComputeContext {
                itv_cs: timing.itv_cs,
                tick_total: match (normalize_by_ticks, prev_item) {
                    // guest / guest_nice を分母に入れないため、列を特定する plan が必要
                    (true, Some(p)) => Some(tick_total(plan, p, item)),
                    (true, None) => None,
                    (false, _) => None,
                },
                continuous: view.continuous && same_item,
                has_prev: view.has_prev && prev_item.is_some(),
                // CPU の `all` 行 / `A_IRQ` の合計列は集約 item として扱う
                aggregate_item: label == "all" || label == "sum",
            };

            let prepared = prepare_item(
                snap.id,
                plan,
                index,
                prev_snap.map_or(&[], |s| s.items.as_slice()),
                &snap.items,
                ctx,
            )
            .expect("現在の item は存在する");
            let prev_item = prev_item.map(|_| &prepared.prev);
            let item = &prepared.curr;
            let ctx = prepared.ctx;

            let setup = DeltaSetup {
                normalize_by_ticks,
                missing_prev: if view.has_prev {
                    // 前サンプルはあるのに同一 item が無い
                    ExclusionReason::ItemReplaced
                } else {
                    ExclusionReason::FirstSample
                },
            };

            let ai = self.activity_index(snap.id);
            let ii = self.item_index(ai, &label, item.key.as_deref(), def.columns, cfg);

            for column in 0..def.columns.len() {
                let meta = def.columns[column];
                if matches!(meta.kind, ValueKind::Identity)
                    || matches!(meta.aggregation, Aggregation::NotAggregated)
                {
                    continue;
                }

                // レート列の分子・分母は厳密な不連続判定付きで別に求める。
                // `compute` 層の表示値は本家の符号なし減算をそのまま再現するため、
                // カウンタ逆行時に巨大な値になり得る。集計ではそれを採らない。
                let strict = (meta.is_direct() && matches!(meta.kind, ValueKind::Counter))
                    .then(|| counter_delta(snap.id, plan, column, prev_item, item, &ctx, setup));

                let outcome = match strict {
                    _ if prepared.offline && view.has_prev => {
                        Outcome::Excluded(if normalize_by_ticks && ctx.tick_total == Some(0) {
                            ExclusionReason::ZeroDenominator
                        } else {
                            ExclusionReason::ItemReplaced
                        })
                    }
                    _ if prepared.replaced && !matches!(meta.kind, ValueKind::Gauge) => {
                        Outcome::Excluded(ExclusionReason::ItemReplaced)
                    }
                    Some(Err(reason)) => Outcome::Excluded(reason),
                    strict => {
                        // 区間値は `compute` 層から取る。
                        // ゲージの単位変換 (ldavg の 1/100、`double` 保存フィールドなど) や
                        // activity 固有の補正をここで再実装すると、
                        // 同じ指標が出力形式ごとに違う値になる。
                        let prev_for_compute = prev_item.unwrap_or(&EMPTY_ITEM);
                        // **厳密モードを使う。**
                        // 互換出力は本家の代替規則で欠落を埋める (旧世代の
                        // `%memused` を `frmkb` から出す等) が、集計でそれをやると
                        // 「その世代のファイルには無い値」が有効な観測として
                        // 平均や p95 に混ざる。欠落は欠落として除外する。
                        match column_value_strict(
                            snap.id,
                            column,
                            &meta,
                            plan,
                            prev_for_compute,
                            item,
                            &ctx,
                        ) {
                            Ok(value) => {
                                let (delta, denom, wrapped) = match strict {
                                    Some(Ok(d)) => (Some(d.delta), Some(d.denominator), d.wrapped),
                                    _ => (None, None, false),
                                };
                                Outcome::Value {
                                    value,
                                    delta,
                                    denom,
                                    wrapped,
                                }
                            }
                            Err(issue) => Outcome::Excluded(ExclusionReason::from(issue)),
                        }
                    }
                };

                let acc = &mut self.activities[ai].items[ii].columns[column];
                match outcome {
                    Outcome::Value {
                        value,
                        delta,
                        denom,
                        wrapped,
                    } => {
                        // ゲージの時間加重は「前サンプルから当サンプルまで」を重みにする。
                        // 重み 0 の場合でも最大 / 最小 / 標本平均には寄与させる。
                        acc.observe(value, timing.weight_cs, timing.start_ust, timing.end_ust);
                        if let (Some(d), Some(n)) = (delta, denom) {
                            acc.delta_total += u128::from(d);
                            acc.denom_total += u128::from(n);
                        } else if let Some(d) = delta {
                            acc.delta_total += u128::from(d);
                        }
                        if wrapped {
                            acc.wrapped += 1;
                        }
                    }
                    Outcome::Excluded(reason) => acc.exclude(reason),
                }

                let point = match outcome {
                    Outcome::Value { value, .. } => MetricPoint::observed(
                        timing.start_ust,
                        timing.end_ust,
                        timing.weight_cs,
                        value,
                    ),
                    Outcome::Excluded(reason) => MetricPoint::missing(
                        timing.start_ust,
                        timing.end_ust,
                        timing.weight_cs,
                        reason,
                    ),
                };
                self.retain_point(snap.id, &label, &meta, point);
            }
        }
    }

    fn retain_point(&mut self, id: ActivityId, label: &str, meta: &ColumnMeta, point: MetricPoint) {
        let key = MetricKey::new(id, label, meta.public_name);
        let keep = match &self.opts.retain {
            RetainTimelines::None => false,
            RetainTimelines::All => true,
            RetainTimelines::Only(keys) => keys.contains(&key),
            RetainTimelines::RuleInputs => super::rules::is_rule_input(&key),
        };
        if !keep {
            return;
        }
        self.timelines.entry(key, meta.unit, meta.kind).push(point);
    }

    fn activity_index(&mut self, id: ActivityId) -> usize {
        if let Some(i) = self.activities.iter().position(|a| a.id == id) {
            return i;
        }
        self.activities.push(ActivityAccum {
            id,
            items: Vec::new(),
        });
        self.activities.len() - 1
    }

    fn item_index(
        &mut self,
        activity: usize,
        label: &str,
        key: Option<&str>,
        columns: &'static [ColumnMeta],
        cfg: AccumConfig,
    ) -> usize {
        let id = self.activities[activity].id;
        let items = &mut self.activities[activity].items;
        if let Some(i) = items.iter().position(|it| it.label == label) {
            return i;
        }
        items.push(ItemAccum {
            label: label.to_string(),
            key: key.map(|k| k.to_string()),
            columns: columns
                .iter()
                .enumerate()
                .map(|(column, m)| ColumnAccum::new(id, column, *m, cfg))
                .collect(),
        });
        items.len() - 1
    }

    /// 集計を確定する。
    pub fn finish(mut self, source: SummarySource) -> NativePeriodSummary {
        // 出力順を決定的にする (activity ID 昇順 → item ラベル昇順)
        self.activities.sort_by_key(|a| a.id);
        self.timelines.sort();

        let mut total: Vec<ExclusionCount> = Vec::new();
        let mut activities = Vec::with_capacity(self.activities.len());
        for mut a in self.activities {
            a.items.sort_by_key(|x| item_order(&x.label));
            let mut items = Vec::with_capacity(a.items.len());
            for it in a.items {
                let columns: Vec<ColumnSummary> =
                    it.columns.into_iter().map(ColumnAccum::finish).collect();
                for c in &columns {
                    for e in &c.exclusions {
                        match total.iter_mut().find(|t| t.reason == e.reason) {
                            Some(t) => t.intervals += e.intervals,
                            None => total.push(*e),
                        }
                    }
                }
                items.push(ItemSummary {
                    item: it.label,
                    key: it.key,
                    columns,
                });
            }
            activities.push(ActivitySummary {
                activity: a.id,
                activity_name: a.id.display_name(),
                items,
            });
        }
        total.sort_by_key(|e| e.reason);

        NativePeriodSummary {
            schema_version: SUMMARY_SCHEMA_VERSION,
            summary_kind: SUMMARY_KIND,
            spec: SummarySpec::from_options(&self.opts),
            source,
            period: self.period,
            activities,
            exclusions: total,
            timelines: self.timelines,
        }
    }

    /// 保持している時系列 (判定の入力)。
    pub fn timelines(&self) -> &Timelines {
        &self.timelines
    }

    /// 期間の範囲。
    pub fn period(&self) -> PeriodBounds {
        self.period
    }
}

/// 区間の時刻情報。
#[derive(Debug, Clone, Copy)]
struct Timing {
    start_ust: u64,
    end_ust: u64,
    /// 時間重みに使う区間長 (連続でない場合は 0)。
    weight_cs: u64,
    /// レート計算に使う区間長 (本家と同じ `itv`)。
    itv_cs: u64,
}

/// 1 列 1 区間の結果。
#[derive(Debug, Clone, Copy)]
enum Outcome {
    Value {
        value: f64,
        /// レート集計の分子 (カウンタ列のみ)。
        delta: Option<u64>,
        /// レート集計の分母 (カウンタ列のみ)。
        denom: Option<u64>,
        wrapped: bool,
    },
    Excluded(ExclusionReason),
}

/// 前サンプルが無いときに渡す空 item (派生列の計算は前値の欠落を自分で判定する)。
static EMPTY_ITEM: ItemSnapshot = ItemSnapshot {
    key: None,
    texts: Vec::new(),
    values: Vec::new(),
};

/// 差分計算の前提。
#[derive(Debug, Clone, Copy)]
struct DeltaSetup {
    /// CPU tick 合計で正規化するか。
    normalize_by_ticks: bool,
    /// 前サンプルの同一 item が無いときの除外理由。
    ///
    /// 系列の先頭なら `FirstSample`、前サンプルはあるのに同一 item が
    /// 見つからないなら `ItemReplaced` (デバイスの着脱・名前の再利用)。
    missing_prev: ExclusionReason,
}

/// カウンタ列の差分と分母を、厳密な不連続判定付きで求める。
///
/// 本体は計算層の [`rate_sample`] で、ここは**除外理由を集計側の語彙へ
/// 翻訳するだけ**の薄い層である。差分の採り方 (減少を一律ラップと解釈しない、
/// 本家が 0 にクランプする列は区間の寄与を 0 にする) を集計側で書き直すと、
/// 同じ区間の瞬時値と集計が食い違う。
///
/// 計算層が持たない情報だけを先に判定する。
///
/// | 先に見る理由 | 集計側の語彙 |
/// |---|---|
/// | この世代のファイルにその列が無い | `UnsupportedBySource` (系列の先頭でも「列が無い」と言う) |
/// | 前サンプルの同一 item が無い | `FirstSample` / `ItemReplaced` の区別 (計算層は前者しか知らない) |
/// | CPU の tick 合計が 0 | `ZeroDenominator` (区間長 0 の `NonPositiveElapsed` と区別する) |
fn counter_delta(
    id: ActivityId,
    plan: &crate::layout::plan::DecodePlan,
    column: usize,
    prev_item: Option<&ItemSnapshot>,
    curr_item: &ItemSnapshot,
    ctx: &ComputeContext,
    setup: DeltaSetup,
) -> std::result::Result<RateSample, ExclusionReason> {
    // 「列そのものが無い」は前サンプルの有無より先に報告する。
    // 差分が取れないのは列が無いからであって、系列の先頭だからではない
    // (先頭サンプルだけ `first_sample` と報告されると理由が揺れる)。
    match plan.column_value(&curr_item.values, column) {
        Availability::Present(_) => {}
        Availability::UnsupportedBySource => return Err(ExclusionReason::UnsupportedBySource),
        Availability::MissingInSample => return Err(ExclusionReason::MissingInSample),
    }
    // 前サンプルの同一 item が無い理由は呼び出し側が知っている
    // (系列の先頭なのか、item が入れ替わったのか)
    let Some(prev_item) = prev_item else {
        return Err(setup.missing_prev);
    };
    // CPU 割合の分母はその item の tick 合計。0 = その CPU は動いていないので
    // 0% と報告しない。区間長が 0 の場合と理由を分けるためここで判定する。
    if setup.normalize_by_ticks && !matches!(ctx.tick_total, Some(t) if t > 0) {
        return Err(ExclusionReason::ZeroDenominator);
    }

    rate_sample(plan, id, column, prev_item, curr_item, ctx).map_err(ExclusionReason::from)
}

/// CPU tick 合計で正規化する activity か。
///
/// `A_CPU` の割合列はグローバルな区間長ではなく、その CPU の tick 合計を分母にする
/// (`docs/format/03-output-format.md` §1.4)。
fn normalizes_by_cpu_ticks(id: ActivityId) -> bool {
    id == ActivityId::CPU
}

/// item のラベルを決める。
///
/// 識別子を持つ activity はそれを使う。持たないものは並び位置から決める。
/// `A_CPU` だけは位置 0 が全 CPU の合計なので `all` とする。
pub fn item_label(id: ActivityId, shape: ItemShape, index: usize, key: Option<&str>) -> String {
    if let Some(k) = key {
        return k.to_string();
    }
    match shape {
        ItemShape::Single => SINGLE_ITEM.to_string(),
        _ if matches!(id, ActivityId::CPU | ActivityId::NET_SOFT) => {
            if index == 0 {
                "all".to_string()
            } else {
                format!("cpu{}", index - 1)
            }
        }
        _ => index.to_string(),
    }
}

/// item ラベルの並び順。`all` を先頭、数字付きは数値順にする。
fn item_order(label: &str) -> (u8, u64, String) {
    if label == "all" {
        return (0, 0, String::new());
    }
    if let Some(rest) = label.strip_prefix("cpu")
        && let Ok(n) = rest.parse::<u64>()
    {
        return (1, n, String::new());
    }
    if let Ok(n) = label.parse::<u64>() {
        return (1, n, String::new());
    }
    (2, 0, label.to_string())
}

// ===========================================================================
// 便利関数
// ===========================================================================

/// 1 ファイルを集計する。
///
/// 複数ファイルを 1 つの系列として集計する場合は `crate::multi` を使う
/// (ファイル境界で差分を切らないため)。
pub fn summarize_file(
    file: &SaFile,
    selection: &Selection,
    opts: SummaryOptions,
) -> Result<NativePeriodSummary> {
    let mut builder = NativeSummaryBuilder::new(opts);
    walk_items(file, selection, |item| {
        // 集計は統計レコードだけを見る (不連続は `view.continuous` で判定できる)。
        if let WalkItem::Sample(view) = item {
            builder.observe(view);
        }
        Ok(crate::format::file::ScanControl::Continue)
    })?;
    let h = file.header();
    let source = SummarySource {
        label: h.nodename.clone(),
        nodename: Some(h.nodename.clone()),
        release: Some(h.release.clone()),
        machine: Some(h.machine.clone()),
        cpu_nr: h.real_cpu_count(),
        tzname: h.tzname.clone(),
        files: vec![file.path().display().to_string()],
        boot_segment: None,
    };
    Ok(builder.finish(source))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::abi::{Endian, LayoutAbi, SourceEncoding};
    use crate::layout::plan::DecodePlan;
    use crate::series::snapshot::{ActivityPlan, Snapshot};

    /// 指定 activity の最新 revision からデコード計画を作る。
    ///
    /// 値の並びは `plan.fields` の順なので、テストの入力も同じ順で組む。
    fn plan_for(id: ActivityId) -> DecodePlan {
        let def = lookup(id).expect("既知 activity");
        let rev = def.latest().expect("revision");
        let enc = SourceEncoding::new(Endian::Little, LayoutAbi::LP64);
        DecodePlan::build(def, rev, rev.size_lp64, 1, 1, &enc).expect("計画")
    }

    fn plans(id: ActivityId) -> Vec<ActivityPlan> {
        vec![ActivityPlan {
            index: 0,
            id,
            plan: plan_for(id),
        }]
    }

    /// wire フィールド名 → 値 の指定から item を組む。指定の無いフィールドは 0。
    fn item_of(plan: &DecodePlan, values: &[(&str, u64)]) -> ItemSnapshot {
        let mut v = vec![Availability::Present(0u64); plan.fields.len()];
        for (name, value) in values {
            let i = plan
                .fields
                .iter()
                .position(|f| f.name == *name)
                .unwrap_or_else(|| panic!("フィールド {name} が無い"));
            v[i] = Availability::Present(*value);
        }
        ItemSnapshot {
            key: None,
            texts: Vec::new(),
            values: v,
        }
    }

    fn snapshot(id: ActivityId, ust: u64, uptime_cs: u64, items: Vec<ItemSnapshot>) -> Snapshot {
        Snapshot {
            valid: true,
            kind: None,
            ust_time: ust,
            uptime_cs,
            hour: 0,
            minute: 0,
            second: 0,
            activities: vec![ActivitySnapshot {
                id,
                index: 0,
                nr: items.len() as u32,
                nr2: 1,
                items,
            }],
        }
    }

    struct Feed {
        builder: NativeSummaryBuilder,
        plans: Vec<ActivityPlan>,
        prev: Option<Snapshot>,
        empty: Snapshot,
    }

    impl Feed {
        fn new(id: ActivityId, opts: SummaryOptions) -> Self {
            Self {
                builder: NativeSummaryBuilder::new(opts),
                plans: plans(id),
                prev: None,
                empty: Snapshot::default(),
            }
        }

        /// サンプルを 1 つ流す。`continuous == false` は RESTART を挟んだ状態。
        fn push(&mut self, snap: Snapshot, continuous: bool) {
            let has_prev = self.prev.is_some();
            let itv_cs = match &self.prev {
                Some(p) if continuous => {
                    crate::series::delta::interval_cs(p.uptime_cs, snap.uptime_cs)
                }
                // 再起動後は前サンプルの uptime と繋がらない
                Some(_) => snap.uptime_cs.max(1),
                None => snap.uptime_cs.max(1),
            };
            let view = IntervalView {
                prev: self.prev.as_ref().unwrap_or(&self.empty),
                curr: &snap,
                itv_cs,
                has_prev,
                continuous: has_prev && continuous,
                events: &[],
                plans: &self.plans,
            };
            self.builder.observe(&view);
            self.prev = Some(snap);
        }

        fn finish(self) -> NativePeriodSummary {
            self.builder.finish(SummarySource::default())
        }
    }

    fn opts_all() -> SummaryOptions {
        SummaryOptions {
            retain: RetainTimelines::All,
            ..Default::default()
        }
    }

    /// 全フィールドに別々の値を入れた item を作る。
    ///
    /// 列の取り違え (隣の列の差分を読んでいる) を検出できるようにするため、
    /// 一様な値にはしない。
    fn graded_item(plan: &DecodePlan, base: u64, step: u64) -> ItemSnapshot {
        ItemSnapshot {
            key: None,
            texts: Vec::new(),
            values: (0..plan.fields.len())
                .map(|i| Availability::Present(base + step * i as u64))
                .collect(),
        }
    }

    /// その activity の最新 revision で 1 item のデコード計画を作る
    /// (revision を持たない / 計画が作れない activity は `None`)。
    fn latest_plan(def: &'static crate::layout::registry::ActivityDef) -> Option<DecodePlan> {
        let rev = def.latest()?;
        let enc = SourceEncoding::new(Endian::Little, LayoutAbi::LP64);
        DecodePlan::build(def, rev, rev.size_lp64, 1, 1, &enc).ok()
    }

    // -----------------------------------------------------------------------
    // 保存値 → 表示単位のスケーリング (指摘 2)
    // -----------------------------------------------------------------------

    /// **回帰テスト (指摘 2)**: 単一区間の平均は、その区間の瞬時値と一致する。
    ///
    /// 期間平均は「差分合計 ÷ 分母合計」から作るため、保存値 → 表示単位の
    /// スケーリング ([`crate::series::compute::rate_divisor`]) で割り忘れると、
    /// 瞬時値から作る最大 / 最小 / p95 とだけ単位が食い違う
    /// (`rkB/s` が 2 倍、`aqu-sz` が 1,000 倍、`%util` が 10 倍、
    /// PSI の圧力割合が 10,000 倍)。
    ///
    /// 区間が 1 本だけの集計は定義上その区間の瞬時値に等しいので、
    /// 全 activity の全レート列で `平均 == 最大` を確認すれば漏れが機械的に出る。
    /// 増加する区間と逆行する区間の両方を見る (本家が 0 にクランプする列は
    /// 逆行区間でも値が出るので、そこでも一致しなければならない)。
    #[test]
    fn single_interval_mean_matches_instant_value() {
        let mut mismatched: Vec<String> = Vec::new();
        let mut checked = 0usize;

        for increasing in [true, false] {
            for def in crate::layout::registry::all() {
                let Some(plan) = latest_plan(def) else {
                    continue;
                };
                let lo = graded_item(&plan, 100, 7);
                let hi = graded_item(&plan, 1_000, 37);
                let (first, second) = if increasing { (lo, hi) } else { (hi, lo) };

                let mut f = Feed::new(def.id, opts_all());
                f.push(snapshot(def.id, 1_000, 100_000, vec![first]), true);
                f.push(snapshot(def.id, 1_010, 101_000, vec![second]), true);
                let s = f.finish();

                let Some(item) = s.activities.first().and_then(|a| a.items.first()) else {
                    continue;
                };
                for c in &item.columns {
                    if c.method != AggregationMethod::RateOverValidIntervals || c.intervals == 0 {
                        continue;
                    }
                    // 集計が値を作らなかった区間は比較対象外。
                    // 「値を出したなら瞬時値と一致する」が守りたい不変量。
                    let (Some(mean), Some(max)) = (c.mean, c.max) else {
                        continue;
                    };
                    checked += 1;
                    if (mean - max.value).abs() > max.value.abs() * 1e-9 + 1e-12 {
                        mismatched.push(format!(
                            "{} {} (increasing={increasing}): 平均 {mean} vs 瞬時 {}",
                            def.id, c.column, max.value
                        ));
                    }
                }
            }
        }

        assert!(
            mismatched.is_empty(),
            "1 区間の平均と瞬時値が食い違う列がある (スケーリング漏れ): {mismatched:?}"
        );
        assert!(checked > 50, "検査した列が少なすぎる: {checked}");
    }

    /// スケールが必要な代表列の平均が、表示単位で出ること。
    ///
    /// 機械的なテストが「たまたま全部 1.0 倍」で通っていないことの確認も兼ねる。
    /// 期待値の根拠は `docs/format/03-output-format.md` §id=11 (ディスク列) と
    /// §1.5.3 (PSI)。
    #[test]
    fn scaled_rate_columns_are_averaged_in_display_units() {
        // --- A_DISK: 10 秒で 1,000 セクタ / 1,000 ms ---
        let id = ActivityId::DISK;
        let plan = plan_for(id);
        let mut f = Feed::new(id, opts_all());
        let fields = |sect: u64, ticks: u64| {
            item_of(
                &plan,
                &[
                    ("major", 8),
                    ("minor", 0),
                    ("rd_sect", sect),
                    ("rq_ticks", ticks),
                    ("tot_ticks", ticks),
                ],
            )
        };
        f.push(snapshot(id, 1_000, 100_000, vec![fields(0, 0)]), true);
        f.push(
            snapshot(id, 1_010, 101_000, vec![fields(1_000, 1_000)]),
            true,
        );
        let s = f.finish();
        let disk = s.activities.first().and_then(|a| a.items.first()).unwrap();
        let col = |name: &str| disk.columns.iter().find(|c| c.column == name).unwrap();

        // 100 セクタ/秒 = 50 kB/s (セクタは 512 B なので 2 倍にならない)
        assert_eq!(col("read_kb_per_sec").mean, Some(50.0));
        // rq_ticks 1,000 ms / 10 秒 → 平均キュー長 0.1 (1,000 倍にならない)
        assert_eq!(col("avg_queue_size").mean, Some(0.1));
        // tot_ticks 1,000 ms / 10 秒 = 10% ビジー (10 倍にならない)
        assert_eq!(col("util_pct").mean, Some(10.0));
        // 平均は瞬時値と一致する (区間は 1 本だけ)
        assert_eq!(col("read_kb_per_sec").max.unwrap().value, 50.0);
        assert_eq!(col("avg_queue_size").max.unwrap().value, 0.1);
        assert_eq!(col("util_pct").max.unwrap().value, 10.0);

        // --- A_PSI_CPU: 10 秒のうち 1 秒 (1e6 µs) 停止 → 10% ---
        let id = ActivityId::PSI_CPU;
        let plan = plan_for(id);
        let mut f = Feed::new(id, opts_all());
        f.push(
            snapshot(
                id,
                1_000,
                100_000,
                vec![item_of(&plan, &[("some_cpu_total", 0)])],
            ),
            true,
        );
        f.push(
            snapshot(
                id,
                1_010,
                101_000,
                vec![item_of(&plan, &[("some_cpu_total", 1_000_000)])],
            ),
            true,
        );
        let s = f.finish();
        let psi = s.activities.first().and_then(|a| a.items.first()).unwrap();
        let scpu = psi.columns.iter().find(|c| c.column == "scpu").unwrap();
        assert_eq!(scpu.mean, Some(10.0), "10,000 倍にならない");
        assert_eq!(scpu.max.unwrap().value, 10.0);
    }

    /// `Aggregation::Sum` の列に保存値スケールが必要なものは無い。
    ///
    /// [`AggregationMethod::DeltaSum`] は差分の総量をそのまま平均欄に出すため、
    /// レートの除数で割らない。スケールが必要な列が `Sum` で宣言されたら
    /// 単位が壊れるので、その組み合わせが現れないことを機械的に固定する。
    #[test]
    fn sum_columns_need_no_rate_divisor() {
        use crate::series::compute::rate_divisor;
        for def in crate::layout::registry::all() {
            for (column, meta) in def.columns.iter().enumerate() {
                if meta.aggregation != Aggregation::Sum {
                    continue;
                }
                assert_eq!(
                    rate_divisor(def.id, column),
                    1.0,
                    "{} {} は Sum 宣言だがレートのスケールを持つ",
                    def.id,
                    meta.public_name
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // レート集計
    // -----------------------------------------------------------------------

    /// レート列は「有効区間の差分合計 ÷ 有効区間の時間合計」で集計する。
    #[test]
    fn rate_column_aggregates_delta_over_elapsed() {
        let id = ActivityId::SWAP;
        let plan = plan_for(id);
        let mut f = Feed::new(id, opts_all());
        // 10 秒ごとに pswpin が +100 / +300 → 期間レートは 400/20s = 20/s
        f.push(
            snapshot(id, 1000, 100_000, vec![item_of(&plan, &[("pswpin", 0)])]),
            true,
        );
        f.push(
            snapshot(id, 1010, 101_000, vec![item_of(&plan, &[("pswpin", 100)])]),
            true,
        );
        f.push(
            snapshot(id, 1020, 102_000, vec![item_of(&plan, &[("pswpin", 400)])]),
            true,
        );
        let s = f.finish();

        let c = s.column(id, SINGLE_ITEM, "pswpin").expect("列");
        assert_eq!(c.method, AggregationMethod::RateOverValidIntervals);
        assert_eq!(c.intervals, 2, "先頭サンプルは差分が取れないので除外");
        assert_eq!(c.delta_total.as_deref(), Some("400"));
        assert_eq!(c.denominator_total.as_deref(), Some("2000"));
        assert_eq!(c.mean, Some(20.0));
        assert_eq!(c.max.unwrap().value, 30.0, "2 区間目は 300/10s");
        assert_eq!(c.min.unwrap().value, 10.0);
        // 先頭サンプルは first_sample として除外されている
        assert!(
            c.exclusions
                .iter()
                .any(|e| e.reason == ExclusionReason::FirstSample && e.intervals == 1)
        );
    }

    /// **不連続を跨いだ区間は集計に入らない。**
    ///
    /// 再起動でカウンタが 0 に戻ったあとの区間を混ぜると、
    /// 「最後 − 最初」方式では負の差分が符号なし減算で巨大な値に化ける。
    /// ここでは再起動後の最初の区間が除外され、平均が汚染されないことを固定する。
    #[test]
    fn discontinuity_does_not_contaminate_aggregate() {
        let id = ActivityId::SWAP;
        let plan = plan_for(id);
        let mut f = Feed::new(id, opts_all());
        f.push(
            snapshot(
                id,
                1000,
                100_000,
                vec![item_of(&plan, &[("pswpin", 1_000)])],
            ),
            true,
        );
        f.push(
            snapshot(
                id,
                1010,
                101_000,
                vec![item_of(&plan, &[("pswpin", 1_100)])],
            ),
            true,
        );
        // 再起動: uptime も カウンタも巻き戻る
        f.push(
            snapshot(id, 1020, 1_000, vec![item_of(&plan, &[("pswpin", 5)])]),
            false,
        );
        f.push(
            snapshot(id, 1030, 2_000, vec![item_of(&plan, &[("pswpin", 105)])]),
            true,
        );
        let s = f.finish();

        let c = s.column(id, SINGLE_ITEM, "pswpin").expect("列");
        assert_eq!(c.intervals, 2, "有効な区間は再起動前後の 1 本ずつ");
        assert_eq!(c.delta_total.as_deref(), Some("200"), "100 + 100");
        assert_eq!(c.denominator_total.as_deref(), Some("2000"));
        assert_eq!(c.mean, Some(10.0), "再起動区間を含めると桁が跳ねる");
        assert_eq!(c.max.unwrap().value, 10.0);
        assert!(
            c.exclusions
                .iter()
                .any(|e| e.reason == ExclusionReason::Restart && e.intervals == 1),
            "除外理由に restart が記録される: {:?}",
            c.exclusions
        );
        assert_eq!(c.excluded_intervals, 2, "first_sample と restart");
        assert_eq!(s.period.broken_intervals, 2);
    }

    /// 減少が曖昧な場合 (64bit カウンタの逆行) も除外する。
    #[test]
    fn ambiguous_decrease_is_excluded_not_wrapped() {
        let id = ActivityId::SWAP;
        let plan = plan_for(id);
        let mut f = Feed::new(id, opts_all());
        f.push(
            snapshot(
                id,
                1000,
                100_000,
                vec![item_of(&plan, &[("pswpin", 1_000_000)])],
            ),
            true,
        );
        f.push(
            snapshot(id, 1010, 101_000, vec![item_of(&plan, &[("pswpin", 10)])]),
            true,
        );
        let s = f.finish();

        let c = s.column(id, SINGLE_ITEM, "pswpin").expect("列");
        assert_eq!(c.intervals, 0);
        assert!(c.mean.is_none(), "巨大な値を出さない");
        assert!(
            c.exclusions
                .iter()
                .any(|e| e.reason == ExclusionReason::AmbiguousDecrease)
        );
    }

    // -----------------------------------------------------------------------
    // 欠損の扱い
    // -----------------------------------------------------------------------

    /// **欠損を 0 と見なさない。**
    ///
    /// `MissingInSample` の区間は平均・最小・p95 のいずれにも 0 として入らない。
    #[test]
    fn missing_value_is_never_treated_as_zero() {
        let id = ActivityId::MEMORY;
        let plan = plan_for(id);
        let mut f = Feed::new(id, opts_all());

        let mut present = item_of(&plan, &[("frmkb", 1_000)]);
        f.push(snapshot(id, 1000, 100_000, vec![present.clone()]), true);
        // frmkb だけ欠落させる
        let i = plan.fields.iter().position(|x| x.name == "frmkb").unwrap();
        present.values[i] = Availability::MissingInSample;
        f.push(snapshot(id, 1010, 101_000, vec![present]), true);
        f.push(
            snapshot(id, 1020, 102_000, vec![item_of(&plan, &[("frmkb", 1_000)])]),
            true,
        );
        let s = f.finish();

        let c = s.column(id, SINGLE_ITEM, "kbmemfree").expect("列");
        assert_eq!(c.intervals, 2, "欠損区間は集計対象外");
        assert_eq!(c.mean, Some(1_000.0), "0 を混ぜると 666.7 になる");
        assert_eq!(c.min.unwrap().value, 1_000.0, "最小が 0 に落ちない");
        assert!(
            c.exclusions
                .iter()
                .any(|e| e.reason == ExclusionReason::MissingInSample && e.intervals == 1)
        );
    }

    /// その世代に無いフィールドも 0 として集計しない。
    #[test]
    fn unsupported_field_is_reported_not_zeroed() {
        let id = ActivityId::MEMORY;
        let plan = plan_for(id);
        let mut f = Feed::new(id, opts_all());
        let mut it = item_of(&plan, &[("frmkb", 10)]);
        let i = plan.fields.iter().position(|x| x.name == "frmkb").unwrap();
        it.values[i] = Availability::UnsupportedBySource;
        f.push(snapshot(id, 1000, 100_000, vec![it]), true);
        let s = f.finish();

        let c = s.column(id, SINGLE_ITEM, "kbmemfree").expect("列");
        assert_eq!(c.intervals, 0);
        assert!(c.mean.is_none());
        assert_eq!(
            c.exclusions,
            vec![ExclusionCount {
                reason: ExclusionReason::UnsupportedBySource,
                intervals: 1
            }]
        );
    }

    /// 派生列は未実装でも 0 を返さない。
    #[test]
    fn derived_column_is_marked_not_matching_declaration() {
        let id = ActivityId::MEMORY;
        let plan = plan_for(id);
        let mut f = Feed::new(id, opts_all());
        f.push(
            snapshot(
                id,
                1000,
                100_000,
                vec![item_of(&plan, &[("tlmkb", 100), ("frmkb", 10)])],
            ),
            true,
        );
        let s = f.finish();

        let c = s.column(id, SINGLE_ITEM, "memused_pct").expect("列");
        assert_eq!(c.method, AggregationMethod::DerivedIntervalSampleMean);
        assert!(!c.matches_declared, "宣言 (Mean) と方式が違うことを示す");
        assert!(c.mean.is_none() || c.intervals > 0);
    }

    // -----------------------------------------------------------------------
    // ゲージの平均方式
    // -----------------------------------------------------------------------

    /// 既定は標本平均。宣言と結果が一致する。
    #[test]
    fn gauge_default_is_sample_mean() {
        let id = ActivityId::MEMORY;
        let plan = plan_for(id);
        let mut f = Feed::new(id, opts_all());
        f.push(
            snapshot(id, 0, 100, vec![item_of(&plan, &[("frmkb", 100)])]),
            true,
        );
        f.push(
            snapshot(id, 10, 1_100, vec![item_of(&plan, &[("frmkb", 200)])]),
            true,
        );
        // 長い区間 (100 秒) に 0
        f.push(
            snapshot(id, 110, 11_100, vec![item_of(&plan, &[("frmkb", 0)])]),
            true,
        );
        let s = f.finish();

        let c = s.column(id, SINGLE_ITEM, "kbmemfree").expect("列");
        assert_eq!(c.method, AggregationMethod::SampleMean);
        assert_eq!(c.mean, Some(100.0), "(100 + 200 + 0) / 3");
    }

    /// 時間加重を選ぶと区間長で重み付けされる。
    #[test]
    fn gauge_time_weighted_mean_uses_interval_length() {
        let id = ActivityId::MEMORY;
        let plan = plan_for(id);
        let mut f = Feed::new(
            id,
            SummaryOptions {
                gauge_mean: GaugeMeanMode::TimeWeighted,
                retain: RetainTimelines::All,
                ..Default::default()
            },
        );
        // 先頭サンプルは重み 0 (区間長が信頼できない)
        f.push(
            snapshot(id, 0, 100, vec![item_of(&plan, &[("frmkb", 999)])]),
            true,
        );
        // 10 秒間の 200
        f.push(
            snapshot(id, 10, 1_100, vec![item_of(&plan, &[("frmkb", 200)])]),
            true,
        );
        // 100 秒間の 0
        f.push(
            snapshot(id, 110, 11_100, vec![item_of(&plan, &[("frmkb", 0)])]),
            true,
        );
        let s = f.finish();

        let c = s.column(id, SINGLE_ITEM, "kbmemfree").expect("列");
        assert_eq!(c.method, AggregationMethod::TimeWeightedMean);
        let mean = c.mean.unwrap();
        // (200×1000 + 0×10000) / 11000
        assert!((mean - 18.1818).abs() < 0.001, "mean={mean}");
        assert_eq!(c.max.unwrap().value, 999.0, "重み 0 でも極値には入る");
    }

    // -----------------------------------------------------------------------
    // p95
    // -----------------------------------------------------------------------

    /// p95 の重み付けは宣言どおりで、メタデータにも記録される。
    #[test]
    fn p95_weighting_is_declared_and_recorded() {
        let id = ActivityId::MEMORY;
        let plan = plan_for(id);

        let build = |spec: PercentileSpec| {
            let mut f = Feed::new(
                id,
                SummaryOptions {
                    percentile: spec,
                    retain: RetainTimelines::None,
                    ..Default::default()
                },
            );
            let mut uptime = 100u64;
            let mut ust = 0u64;
            // 1 秒区間の 10 を 99 本
            for _ in 0..99 {
                uptime += 100;
                ust += 1;
                f.push(
                    snapshot(id, ust, uptime, vec![item_of(&plan, &[("frmkb", 10)])]),
                    true,
                );
            }
            // 1000 秒区間の 900 を 1 本
            uptime += 100_000;
            ust += 1_000;
            f.push(
                snapshot(id, ust, uptime, vec![item_of(&plan, &[("frmkb", 900)])]),
                true,
            );
            f.finish()
        };

        let by_sample = build(PercentileSpec::p95_sample_weighted());
        let c = by_sample.column(id, SINGLE_ITEM, "kbmemfree").unwrap();
        let r = match c.p95 {
            PercentileOutcome::Computed(r) => r,
            other => panic!("p95 が出ていない: {other:?}"),
        };
        assert_eq!(
            r.spec.weighting,
            super::super::percentile::PercentileWeighting::Sample
        );
        assert_eq!(r.value, 10.0, "標本重みでは 1 本の外れ値は p95 に出ない");

        let by_time = build(PercentileSpec::p95_time_weighted());
        let c = by_time.column(id, SINGLE_ITEM, "kbmemfree").unwrap();
        let r = match c.p95 {
            PercentileOutcome::Computed(r) => r,
            other => panic!("p95 が出ていない: {other:?}"),
        };
        assert_eq!(
            r.spec.weighting,
            super::super::percentile::PercentileWeighting::Time
        );
        assert_eq!(r.value, 900.0, "時間重みでは長時間続いた値が p95 を取る");

        // 宣言はサマリのメタデータにも載る
        assert_eq!(
            by_time.spec.percentile.weighting,
            super::super::percentile::PercentileWeighting::Time
        );
    }

    // -----------------------------------------------------------------------
    // item の同一性
    // -----------------------------------------------------------------------

    /// item が入れ替わったら差分を作らない。
    #[test]
    fn replaced_item_breaks_the_delta() {
        let id = ActivityId::NET_DEV;
        let plan = plan_for(id);
        let mut f = Feed::new(id, opts_all());

        let named = |name: &str, rx: u64| {
            let mut it = item_of(&plan, &[("rx_packets", rx)]);
            it.key = Some(name.into());
            it
        };
        f.push(
            snapshot(id, 1000, 100_000, vec![named("if-a", 1_000)]),
            true,
        );
        // 同名 item が消え、別名で現れる
        f.push(
            snapshot(id, 1010, 101_000, vec![named("if-b", 5_000)]),
            true,
        );
        let s = f.finish();

        let a = s.activity(id).unwrap();
        let b = a.items.iter().find(|i| i.item == "if-b").unwrap();
        let c = b
            .columns
            .iter()
            .find(|c| c.column == "rxpck_per_sec")
            .expect("列");
        assert_eq!(c.intervals, 0, "別 item の値と差分を作らない");
        assert!(
            c.exclusions
                .iter()
                .any(|e| e.reason == ExclusionReason::ItemReplaced)
        );
    }

    // -----------------------------------------------------------------------
    // CPU の tick 正規化
    // -----------------------------------------------------------------------

    /// CPU 割合は区間長ではなく tick 合計で正規化する。
    #[test]
    fn cpu_percent_is_normalized_by_tick_total() {
        let id = ActivityId::CPU;
        let plan = plan_for(id);
        let mut f = Feed::new(id, opts_all());
        f.push(
            snapshot(
                id,
                1000,
                100_000,
                vec![item_of(&plan, &[("cpu_user", 0), ("cpu_idle", 0)])],
            ),
            true,
        );
        // tick 合計 1000 のうち user が 100 → 10%
        f.push(
            snapshot(
                id,
                1010,
                101_000,
                vec![item_of(&plan, &[("cpu_user", 100), ("cpu_idle", 900)])],
            ),
            true,
        );
        let s = f.finish();

        let c = s.column(id, "all", "user").expect("列");
        assert_eq!(
            c.rate_denominator,
            Some(RateDenominator::CpuTicks),
            "分母が区間長でないことを出力に明示する"
        );
        assert_eq!(c.max.unwrap().value, 10.0);
        assert_eq!(
            c.denominator_total.as_deref(),
            Some("1000"),
            "分母は tick 合計"
        );
        assert_eq!(c.mean, Some(10.0));
    }

    /// オフライン CPU (tick 合計 0) は 0% ではなく除外する。
    #[test]
    fn offline_cpu_is_excluded_not_zero() {
        let id = ActivityId::CPU;
        let plan = plan_for(id);
        let mut f = Feed::new(id, opts_all());
        let zeros = || item_of(&plan, &[]);
        f.push(snapshot(id, 1000, 100_000, vec![zeros(), zeros()]), true);
        f.push(snapshot(id, 1010, 101_000, vec![zeros(), zeros()]), true);
        let s = f.finish();

        let c = s.column(id, "cpu0", "user").expect("列");
        assert_eq!(c.intervals, 0);
        assert!(c.mean.is_none(), "動いていない CPU を 0% と報告しない");
        assert!(
            c.exclusions
                .iter()
                .any(|e| e.reason == ExclusionReason::ZeroDenominator)
        );
    }

    // -----------------------------------------------------------------------
    // 出力形
    // -----------------------------------------------------------------------

    /// 独自サマリであることが出力に現れる。
    #[test]
    fn output_declares_that_it_is_not_the_sar_average() {
        let id = ActivityId::SWAP;
        let plan = plan_for(id);
        let mut f = Feed::new(id, opts_all());
        f.push(
            snapshot(id, 1000, 100_000, vec![item_of(&plan, &[("pswpin", 0)])]),
            true,
        );
        f.push(
            snapshot(id, 1010, 101_000, vec![item_of(&plan, &[("pswpin", 100)])]),
            true,
        );
        let s = f.finish();
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains(SUMMARY_KIND), "{json}");
        assert!(json.contains(r#""schema_version":"1.0""#));
        assert!(json.contains("rate_over_valid_intervals"));
        // u64 の生値は十進文字列
        assert!(json.contains(r#""delta_total":"100""#), "{json}");
    }

    #[test]
    fn disk_labels_use_source_device_numbers() {
        let id = ActivityId::DISK;
        let plan = plan_for(id);
        let mut feed = Feed::new(id, opts_all());
        for (at, ios) in [(1000, 100), (1010, 110)] {
            feed.push(
                snapshot(
                    id,
                    at,
                    at * 100,
                    vec![item_of(
                        &plan,
                        &[("major", 8), ("minor", 16), ("nr_ios", ios)],
                    )],
                ),
                true,
            );
        }
        let summary = feed.finish();
        assert_eq!(summary.activity(id).unwrap().items[0].item, "dev8-16");
        assert!(summary.timelines.iter().all(|t| t.key.item == "dev8-16"));
    }

    #[test]
    fn irq_summary_keeps_one_named_aggregate_per_interrupt() {
        let id = ActivityId::IRQ;
        let mut feed = Feed::new(id, opts_all());
        feed.plans[0].plan.nr = 3;
        feed.plans[0].plan.nr2 = 2;
        for (at, count) in [(1000, 100), (1010, 200)] {
            let plan = &feed.plans[0].plan;
            let mut items: Vec<_> = (0..6).map(|_| graded_item(plan, count, 0)).collect();
            items[0].key = Some("sum".into());
            items[1].key = Some("timer".into());
            let mut sample = snapshot(id, at, at * 100, items);
            sample.activities[0].nr = 3;
            sample.activities[0].nr2 = 2;
            feed.push(sample, true);
        }
        let summary = feed.finish();
        let items = &summary.activity(id).unwrap().items;
        assert_eq!(items.len(), 2, "CPU の平坦な添字を独立 item にしない");
        assert!(items.iter().any(|i| i.item == "sum"));
        assert!(items.iter().any(|i| i.item == "timer"));
    }

    #[test]
    fn item_labels_follow_sar_convention() {
        assert_eq!(item_label(ActivityId::CPU, ItemShape::List, 0, None), "all");
        assert_eq!(
            item_label(ActivityId::CPU, ItemShape::List, 3, None),
            "cpu2"
        );
        assert_eq!(
            item_label(ActivityId::MEMORY, ItemShape::Single, 0, None),
            SINGLE_ITEM
        );
        assert_eq!(
            item_label(ActivityId::NET_DEV, ItemShape::List, 1, Some("if-a")),
            "if-a"
        );
    }

    /// 集計の出力順は決定的 (activity ID 昇順・item は all → 番号順)。
    #[test]
    fn output_order_is_deterministic() {
        let id = ActivityId::CPU;
        let plan = plan_for(id);
        let mut f = Feed::new(
            id,
            SummaryOptions {
                retain: RetainTimelines::None,
                ..Default::default()
            },
        );
        let it = || item_of(&plan, &[("cpu_user", 1), ("cpu_idle", 1)]);
        f.push(snapshot(id, 1000, 100_000, vec![it(), it(), it()]), true);
        let s = f.finish();
        let labels: Vec<&str> = s
            .activity(id)
            .unwrap()
            .items
            .iter()
            .map(|i| i.item.as_str())
            .collect();
        assert_eq!(labels, vec!["all", "cpu0", "cpu1"]);
    }
}

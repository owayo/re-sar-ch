//! 集計と判定。
//!
//! 判定は断定ではなく、観測根拠とルール ID を添えた形で返す。
//!
//! | モジュール | 役割 |
//! |---|---|
//! | [`summary`] | 期間集計 (最大 / 最小 / 平均 / p95)。**本家 `Average:` 行とは別物の独自サマリ** |
//! | [`percentile`] | 重み付きパーセンタイル。重み付けとアルゴリズムを出力に記録する |
//! | [`timeline`] | 判定に使う指標の区間値の列。欠損を 0 と見なさない |
//! | [`rules`] | ルール ID 付きのボトルネック判定。閾値・継続時間・必要指標を出力に保存する |
//!
//! この層は**書式化しない**。結果を表す型に `serde::Serialize` を付けてあるので、
//! `output` 層がそれを書式化する (`docs/design.md` §2)。
//!
//! # 使い方
//!
//! ```no_run
//! use re_sar_ch::analyze::{RuleContext, SummaryOptions, evaluate, summarize_file};
//! use re_sar_ch::format::SaFile;
//! use re_sar_ch::series::Selection;
//!
//! let file = SaFile::open("sa01")?;
//! let summary = summarize_file(&file, &Selection::All, SummaryOptions::default())?;
//! let findings = evaluate(&summary, &RuleContext::default());
//! # Ok::<(), re_sar_ch::Error>(())
//! ```

pub mod percentile;
pub mod rules;
pub mod summary;
pub mod timeline;

pub use percentile::{
    PercentileAlgorithm, PercentileInterpolation, PercentileResult, PercentileSpec,
    PercentileUnavailable, PercentileWeighting,
};
pub use rules::{
    Comparison, Evidence, Finding, MissingPolicy, RULESET_VERSION, RuleContext, RuleDef,
    ThresholdRecord, Verdict, VerdictReason, evaluate, evaluate_timelines, rule_inputs,
};
pub use summary::{
    ActivitySummary, AggregationMethod, ColumnSummary, ExclusionCount, Extremum, GaugeMeanMode,
    ItemSummary, NativePeriodSummary, NativeSummaryBuilder, PercentileOutcome, PeriodBounds,
    RateDenominator, RetainTimelines, SUMMARY_KIND, SUMMARY_SCHEMA_VERSION, SummaryOptions,
    SummarySource, SummarySpec, summarize_file,
};
pub use timeline::{
    Coverage, ExclusionReason, MetricKey, MetricPoint, MetricRef, MetricTimeline, Run, Timelines,
};

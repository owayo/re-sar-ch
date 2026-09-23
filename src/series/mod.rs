//! 時系列処理層。
//!
//! パーサが返す生の累積値から、差分・レート・不連続の判定を行う。
//! **出力層では値を再計算しない。** 同じ指標を sar 互換テキストと JSON で出したときに
//! 形式ごとに計算がずれる事故を、層の分離で防ぐ。

pub mod compute;
pub mod delta;
pub mod el7;
pub mod snapshot;

pub use compute::{ComputeContext, ComputeIssue, Computed, column_value, tick_total};
pub use delta::{
    Delta, DeltaContext, Discontinuity, compute_delta, interval_cs, ll_sp_value, s_value,
};
pub use snapshot::{
    ActivitySnapshot, IntervalView, ItemSnapshot, RecordEvent, RecordRange, Selection, Snapshot,
    WalkItem, walk, walk_items, walk_items_in,
};

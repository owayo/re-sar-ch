//! 出力層。
//!
//! **この層では値を計算しない。** `series` 層が計算した結果を書式化するだけ。
//! 同じ指標を `sar` 互換テキストと JSON で出したときに、
//! 形式ごとに計算がずれる事故を層の分離で防ぐ。

pub mod csv;
pub mod json;
pub mod ndjson;
pub mod sadf;
pub mod sar_text;
pub mod table;
pub mod time_filter;

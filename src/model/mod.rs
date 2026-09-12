//! 統一ドメインモデル。
//!
//! パース結果はここで定義する型へ写す。フォーマット世代や生成元 ABI の差は
//! `format` 層で吸収済みであり、このモデルには現れない。
//!
//! 設計上の要点は「**欠落とゼロを混同しない**」こと。
//! その世代のファイルに存在しないフィールドを 0 として扱うと、
//! 平均・p95・ボトルネック判定が静かに誤る。

pub mod activity;
pub mod value;

pub use activity::{ActivityId, KNOWN_ACTIVITIES, MAX_NR_ACT, NR_ACT};
pub use value::{Aggregation, Availability, Counter, CounterBits, Unit, ValueKind};

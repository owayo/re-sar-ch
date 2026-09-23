//! 統一ドメインモデル。
//!
//! パース結果はここで定義する型へ写す。フォーマット世代や生成元 ABI の差は
//! `format` 層で吸収済みであり、このモデルには現れない。
//!
//! 設計上の要点は「**欠落とゼロを混同しない**」こと。
//! その世代のファイルに存在しないフィールドを 0 として扱うと、
//! 平均・p95・ボトルネック判定が静かに誤る。

pub mod activity;
pub mod lang;
pub mod sar_profile;
pub mod sysstat_env;
pub mod text;
pub mod timezone;
pub mod value;

pub use activity::{ActivityId, KNOWN_ACTIVITIES, MAX_NR_ACT, NR_ACT};
pub use lang::{Lang, LangSource, ResolvedLang};
pub use sar_profile::{PageSize, SarProfile};
pub use sysstat_env::{
    CompatDateFormat, DEFAULT_ROWS, HeaderRows, header_rows_el7, header_rows_from_env,
};
pub use text::{Text, count_en};
pub use timezone::DisplayTz;
pub use value::{Aggregation, Availability, Counter, CounterBits, Unit, ValueKind};

/// 独自出力で共通の公開スキーマ版。
pub const NATIVE_SCHEMA_VERSION: &str = "1.0";

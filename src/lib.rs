//! reSARch — sysstat の `sa` バイナリを単体で解析するライブラリ。
//!
//! 設計は `docs/design.md`、フォーマット仕様は `docs/format/` を参照。

pub mod analyze;
pub mod cli;
pub mod convert;
pub mod detect;
pub mod error;
pub mod format;
pub mod layout;
pub mod model;
pub mod multi;
pub mod output;
pub mod series;

pub use error::{Error, Result};

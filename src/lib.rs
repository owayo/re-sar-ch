//! reSARch — sysstat の `sa` バイナリを単体で解析するライブラリ。
//!
//! 設計は `docs/design.md`、フォーマット仕様は `docs/format/` を参照。

pub mod cli;
pub mod error;
pub mod format;
pub mod layout;
pub mod model;
pub mod series;

pub use error::{Error, Result};

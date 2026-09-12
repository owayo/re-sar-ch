//! activity 定義の実体。
//!
//! activity の種類ごとにファイルを分けている。各ファイルは
//! `pub const DEFS: &[ActivityDef]` を公開し、ここで束ねる。
//!
//! 定義の正しさは `registry` の自己整合性テストが機械的に検査する
//! (レイアウト記述から導出したサイズ・型別個数が、宣言値と一致すること)。

use super::registry::ActivityDef;

pub mod net;
pub mod power;
pub mod storage;
pub mod system;

/// 全 activity 定義のグループ。
pub const GROUPS: &[&[ActivityDef]] = &[system::DEFS, storage::DEFS, net::DEFS, power::DEFS];

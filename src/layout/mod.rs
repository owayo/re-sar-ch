//! activity レイアウト層。
//!
//! 43 種の activity について「wire 上のフィールド並び」と「列メタデータ」を
//! **単一の定義**として持つ。世代差は `revisions` の差分として表現し、
//! デコーダは 1 つで済ませる。
//!
//! ## 設計の要点
//!
//! - 1 item のストライドは**常に `file_activity.size`** を使う。構造体定義からの
//!   推測は禁止 (旧版の `A_HUGE` は `sizeof(stats_memory)` が書かれており、
//!   定義から計算した値と一致しない)。
//! - フィールドのオフセットはレイアウト記述から導出する。導出結果は
//!   `file_activity.size` と `types_nr` の申告値、および独立に作った fixture と突合する。
//! - 値は `FieldId` で索引する配列に入れる。ホットパスに文字列キーの map を置かない。

pub mod activities;
pub mod plan;
pub mod registry;

pub use plan::{DecodePlan, FieldId, ItemValues};
pub use registry::{ActivityDef, ColumnMeta, ItemShape, WireRevision, all, lookup};

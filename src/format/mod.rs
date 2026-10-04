//! バイナリフォーマット層。
//!
//! 世代 (`format_magic`) と生成元 ABI の差を吸収し、
//! レコード境界を確定するところまでを担う。統計値の意味付けは上位層の責務。

pub mod abi;
pub mod file;
pub mod layouts;
pub(crate) mod legacy;
pub(crate) mod legacy_layouts;
pub(crate) mod packed_legacy;
pub mod reader;
pub mod registry;
pub mod selfdesc;
pub mod wire;
pub mod writer;

pub use abi::{Endian, LayoutAbi, SourceEncoding};
pub use file::{MmapPolicy, OpenOptions, RawRecord, SaFile, ScanControl, ScanSummary, Tolerance};
pub use reader::{Cursor, OutOfBounds, ReadResult};
pub use registry::{FormatSpec, RecordKind, RestartPayload};
pub use wire::{
    AlignSpec, FieldTy, LayoutExpectation, PlacedField, ResolvedLayout, WireField, WireLayout,
    align_up,
};
pub use writer::WriteCursor;

/// 旧形式の jiffies を 1/100 秒へ換算する。
/// 中間積だけがあふれる場合も値を保ち、未指定の HZ と最終結果の桁あふれは区別せず欠落にする。
pub(crate) fn uptime_centiseconds(jiffies: u64, hz: u64) -> Option<u64> {
    let cs = (u128::from(jiffies) * 100).checked_div(u128::from(hz))?;
    u64::try_from(cs).ok()
}

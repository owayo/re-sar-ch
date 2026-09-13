//! バイナリフォーマット層。
//!
//! 世代 (`format_magic`) と生成元 ABI の差を吸収し、
//! レコード境界を確定するところまでを担う。統計値の意味付けは上位層の責務。

pub mod abi;
pub mod file;
pub mod layouts;
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

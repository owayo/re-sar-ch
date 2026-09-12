//! wire レイアウト記述と、その配置解決エンジン。
//!
//! C 構造体のバイト配置は
//!
//! ```text
//! field_offset = align_up(previous_end, effective_field_alignment)
//! field_end    = field_offset + field_storage_size
//! struct_size  = align_up(last_field_end, effective_struct_alignment)
//! ```
//!
//! という規則で決まる。reSARch はこの規則から配置を導出し、
//! 別経路で得た期待値 (本家 C ヘッダの `offsetof` / `sizeof`、実データのバイト列) と
//! 突合して固定する。人手の明示オフセットは通常の定義には使わない。

use super::abi::{LayoutAbi, SourceEncoding};
use crate::error::LayoutError;

/// `unsigned long` フィールドがファイル上で占めるスロット幅。
///
/// sysstat の統計構造体は `unsigned long` に `aligned(8)` を付けているため、
/// 32bit ライタが書いたファイルでも構造体サイズが 64bit と一致する。
/// 本家の `UL_ALIGNMENT_WIDTH` に対応する。
pub const UL_SLOT_WIDTH: usize = 8;

/// wire 上のフィールド型。
///
/// `CULong` / `CLong` は「スロット幅 8 バイト固定、有効バイト数だけが
/// [`LayoutAbi::long_bytes`] に依存する」という特殊な型である。
/// これが世代差ではなく ABI 差を担う唯一の型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldTy {
    U8,
    U16,
    U32,
    U64,
    I8,
    I16,
    I32,
    I64,
    /// `unsigned long`
    CULong,
    /// `long`
    CLong,
    /// `char[n]` / `unsigned char[n]`
    Bytes(u16),
}

impl FieldTy {
    /// ファイル上の占有幅 (スロット幅)。
    ///
    /// `unsigned long` は `aligned(8)` により **常に 8 バイトのスロット**を占める。
    /// 32bit ライタが書いた場合も構造体サイズは変わらず、
    /// スロットの後半 4 バイトがゼロパディングになる。
    /// このため 32bit/64bit で `sizeof(struct stats_*)` が一致する。
    #[inline]
    pub const fn width(self, _abi: &LayoutAbi) -> usize {
        match self {
            FieldTy::U8 | FieldTy::I8 => 1,
            FieldTy::U16 | FieldTy::I16 => 2,
            FieldTy::U32 | FieldTy::I32 => 4,
            FieldTy::U64 | FieldTy::I64 => 8,
            FieldTy::CULong | FieldTy::CLong => UL_SLOT_WIDTH,
            FieldTy::Bytes(n) => n as usize,
        }
    }

    /// 値として意味のあるバイト数。スロットの**先頭側**から数える。
    ///
    /// `unsigned long` のみ占有幅と異なり、`sa_sizeof_long` (4 か 8) に従う。
    /// 先頭 `value_width` バイトをファイルのバイト順で読んでゼロ拡張すれば、
    /// LE/BE × 32/64bit の 4 通りすべてで正しい値になる。
    #[inline]
    pub const fn value_width(self, abi: &LayoutAbi) -> usize {
        match self {
            FieldTy::CULong | FieldTy::CLong => abi.long_bytes as usize,
            other => other.width(abi),
        }
    }

    /// この ABI 上での自然アラインメント。
    #[inline]
    pub const fn natural_align(self, abi: &LayoutAbi) -> usize {
        match self {
            FieldTy::U8 | FieldTy::I8 => 1,
            FieldTy::U16 | FieldTy::I16 => 2,
            FieldTy::U32 | FieldTy::I32 => 4,
            FieldTy::U64 | FieldTy::I64 => abi.u64_align as usize,
            // ul スロットは 8 バイト幅・8 バイト境界として扱う (本家の UL_ALIGNMENT_WIDTH)
            FieldTy::CULong | FieldTy::CLong => UL_SLOT_WIDTH,
            // 配列のアラインメントは要素型 (char) のもの
            FieldTy::Bytes(_) => 1,
        }
    }

    /// 符号付きか。
    #[inline]
    pub const fn is_signed(self) -> bool {
        matches!(
            self,
            FieldTy::I8 | FieldTy::I16 | FieldTy::I32 | FieldTy::I64 | FieldTy::CLong
        )
    }
}

/// アラインメント指定。
///
/// C の `__attribute__((aligned(n)))` / `__attribute__((packed))` に対応する。
/// `packed` を一律にアラインメント 1 と読み替えるのではなく、
/// 指定の種類を保ったまま解決時に組み合わせる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlignSpec {
    /// 型の自然アラインメントに従う。
    Natural,
    /// `aligned(n)`: 自然アラインメントと n の大きい方。
    AtLeast(u16),
    /// `packed`: パディングを入れない。
    Packed,
}

/// wire 上の 1 フィールド。
#[derive(Debug, Clone, Copy)]
pub struct WireField {
    pub name: &'static str,
    pub ty: FieldTy,
    pub align: AlignSpec,
}

impl WireField {
    pub const fn new(name: &'static str, ty: FieldTy, align: AlignSpec) -> Self {
        Self { name, ty, align }
    }

    pub const fn natural(name: &'static str, ty: FieldTy) -> Self {
        Self::new(name, ty, AlignSpec::Natural)
    }

    pub const fn aligned(name: &'static str, ty: FieldTy, n: u16) -> Self {
        Self::new(name, ty, AlignSpec::AtLeast(n))
    }

    pub const fn packed(name: &'static str, ty: FieldTy) -> Self {
        Self::new(name, ty, AlignSpec::Packed)
    }
}

/// 1 つの C 構造体に対応する wire レイアウト記述。
#[derive(Debug, Clone, Copy)]
pub struct WireLayout {
    pub name: &'static str,
    pub fields: &'static [WireField],
    /// 構造体全体への明示アラインメント。`Natural` ならメンバの最大値を使う。
    pub struct_align: AlignSpec,
}

impl WireLayout {
    pub const fn new(name: &'static str, fields: &'static [WireField]) -> Self {
        Self {
            name,
            fields,
            struct_align: AlignSpec::Natural,
        }
    }

    pub const fn with_struct_align(mut self, spec: AlignSpec) -> Self {
        self.struct_align = spec;
        self
    }

    /// 配置を解決する。
    pub fn resolve(&self, enc: &SourceEncoding) -> Result<ResolvedLayout, LayoutError> {
        resolve_fields(self.name, self.fields, self.struct_align, enc)
    }

    /// 配置を解決し、独立して得た期待値と突合する。
    ///
    /// 総サイズだけでは構造体内部の穴の誤りを見逃すため、
    /// 主要フィールドのオフセットも併せて検査する。
    pub fn resolve_verified(
        &self,
        enc: &SourceEncoding,
        expect: &LayoutExpectation,
    ) -> Result<ResolvedLayout, LayoutError> {
        let resolved = self.resolve(enc)?;

        if resolved.size != expect.size {
            return Err(LayoutError::SizeMismatch {
                layout: self.name,
                computed: resolved.size,
                expected: expect.size,
                abi: enc.abi.name,
                endian: enc.endian.as_str(),
            });
        }

        for (field, expected_offset) in expect.offsets {
            let found = resolved
                .field(field)
                .ok_or(LayoutError::OffsetMismatch {
                    layout: self.name,
                    field,
                    computed: usize::MAX,
                    expected: *expected_offset,
                })?
                .offset;
            if found != *expected_offset {
                return Err(LayoutError::OffsetMismatch {
                    layout: self.name,
                    field,
                    computed: found,
                    expected: *expected_offset,
                });
            }
        }

        Ok(resolved)
    }
}

/// 別経路で得た配置の期待値。
///
/// 出所は本家 C ヘッダから対象 ABI で得た `offsetof` / `sizeof`、
/// あるいは実データのバイト列である。本体のレイアウト記述から導出してはならない。
#[derive(Debug, Clone, Copy)]
pub struct LayoutExpectation {
    pub size: usize,
    pub offsets: &'static [(&'static str, usize)],
}

/// 解決済みの 1 フィールド。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlacedField {
    pub name: &'static str,
    pub offset: usize,
    /// ファイル上の占有幅 (次フィールドの位置を決める)。
    pub width: usize,
    /// 値として意味のあるバイト数 (スロット先頭から)。`unsigned long` のみ `width` と異なる。
    pub value_width: usize,
    pub ty: FieldTy,
}

/// 解決済みの構造体配置。
#[derive(Debug, Clone)]
pub struct ResolvedLayout {
    pub name: &'static str,
    /// 構造体の実サイズ (末尾パディングを含む)。可変個 item のストライドでもある。
    pub size: usize,
    pub align: usize,
    pub fields: Box<[PlacedField]>,
}

impl ResolvedLayout {
    /// 名前でフィールドを引く。**ホットパスでは使わない** (計画構築時のみ)。
    pub fn field(&self, name: &str) -> Option<&PlacedField> {
        self.fields.iter().find(|f| f.name == name)
    }

    /// 宣言順の位置でフィールドを引く。
    #[inline]
    pub fn at(&self, index: usize) -> Option<&PlacedField> {
        self.fields.get(index)
    }
}

/// フィールド列の配置を解決する。
///
/// [`WireLayout`] (静的宣言) と、自己記述形式で実行時に組み立てた
/// フィールド列の両方から呼ばれる共通エンジン。
pub fn resolve_fields(
    name: &'static str,
    fields: &[WireField],
    struct_align_spec: AlignSpec,
    enc: &SourceEncoding,
) -> Result<ResolvedLayout, LayoutError> {
    let abi = &enc.abi;
    let mut placed: Vec<PlacedField> = Vec::with_capacity(fields.len());
    let mut cursor: usize = 0;
    let mut max_align: usize = 1;

    for f in fields {
        let natural = f.ty.natural_align(abi);
        let effective = match f.align {
            AlignSpec::Natural => natural,
            AlignSpec::AtLeast(n) => {
                let n = n as usize;
                if !n.is_power_of_two() {
                    return Err(LayoutError::AlignNotPowerOfTwo {
                        layout: name,
                        field: f.name,
                        align: n as u16,
                    });
                }
                natural.max(n)
            }
            AlignSpec::Packed => 1,
        };

        max_align = max_align.max(effective);

        let offset = align_up(cursor, effective).ok_or(LayoutError::SizeOverflow {
            layout: name,
            field: f.name,
        })?;
        let width = f.ty.width(abi);
        let end = offset.checked_add(width).ok_or(LayoutError::SizeOverflow {
            layout: name,
            field: f.name,
        })?;

        placed.push(PlacedField {
            name: f.name,
            offset,
            width,
            value_width: f.ty.value_width(abi),
            ty: f.ty,
        });
        cursor = end;
    }

    let struct_align = match struct_align_spec {
        AlignSpec::Natural => max_align,
        AlignSpec::AtLeast(n) => {
            let n = n as usize;
            if !n.is_power_of_two() {
                return Err(LayoutError::StructAlignNotPowerOfTwo {
                    layout: name,
                    align: n as u16,
                });
            }
            max_align.max(n)
        }
        AlignSpec::Packed => 1,
    };

    let size = align_up(cursor, struct_align).ok_or(LayoutError::SizeOverflow {
        layout: name,
        field: "<struct>",
    })?;

    Ok(ResolvedLayout {
        name,
        size,
        align: struct_align,
        fields: placed.into_boxed_slice(),
    })
}

/// `offset` を `align` 境界へ切り上げる。桁溢れする場合は `None`。
#[inline]
pub fn align_up(offset: usize, align: usize) -> Option<usize> {
    debug_assert!(align.is_power_of_two(), "align must be a power of two");
    if align <= 1 {
        return Some(offset);
    }
    offset.checked_add(align - 1).map(|v| v & !(align - 1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::abi::{Endian, LayoutAbi};

    fn lp64() -> SourceEncoding {
        SourceEncoding::new(Endian::Little, LayoutAbi::LP64)
    }

    fn i386() -> SourceEncoding {
        SourceEncoding::new(Endian::Little, LayoutAbi::I386)
    }

    /// sysstat 10.1.5 の `record_header`。
    ///
    /// 期待値の出所は実データのバイト列 (レコードが 48 バイト間隔で並び、
    /// ファイル末尾までぴったり一致することを確認済み)。
    const RECORD_HEADER_2171: WireLayout = WireLayout::new(
        "record_header@2171",
        &[
            WireField::aligned("uptime", FieldTy::U64, 16),
            WireField::aligned("uptime0", FieldTy::U64, 16),
            WireField::aligned("ust_time", FieldTy::CULong, 16),
            WireField::aligned("record_type", FieldTy::U8, 8),
            WireField::natural("hour", FieldTy::U8),
            WireField::natural("minute", FieldTy::U8),
            WireField::natural("second", FieldTy::U8),
        ],
    );

    const RECORD_HEADER_2171_EXPECT: LayoutExpectation = LayoutExpectation {
        size: 48,
        offsets: &[
            ("uptime", 0),
            ("uptime0", 16),
            ("ust_time", 32),
            ("record_type", 40),
            ("hour", 41),
            ("minute", 42),
            ("second", 43),
        ],
    };

    #[test]
    fn aligned16_holes_are_reproduced_on_lp64() {
        let r = RECORD_HEADER_2171
            .resolve_verified(&lp64(), &RECORD_HEADER_2171_EXPECT)
            .expect("実データ由来の期待値と一致すること");
        assert_eq!(r.size, 48);
        assert_eq!(r.align, 16);
    }

    #[test]
    fn same_offsets_but_narrower_long_on_ilp32() {
        // 32bit では ust_time の読み幅が 4 になるが、後続フィールドの
        // オフセットは aligned(8)/aligned(16) に支配されて変わらない。
        let r = RECORD_HEADER_2171
            .resolve_verified(&i386(), &RECORD_HEADER_2171_EXPECT)
            .expect("32bit でも同じオフセットになること");
        // スロットは 8 バイトのまま。有効バイト数だけが 4 になる。
        assert_eq!(r.field("ust_time").unwrap().width, 8);
        assert_eq!(r.field("ust_time").unwrap().value_width, 4);
        assert_eq!(r.field("record_type").unwrap().offset, 40);
        assert_eq!(r.size, 48);
    }

    /// sysstat 10.1.5 の `file_header`。期待値の出所は実データ。
    #[test]
    fn file_header_2171_is_280_bytes_on_lp64() {
        const UTSNAME_LEN: u16 = 65;
        const FIELDS: &[WireField] = &[
            WireField::aligned("sa_ust_time", FieldTy::CULong, 8),
            WireField::aligned("sa_nr_act", FieldTy::U32, 8),
            WireField::natural("sa_day", FieldTy::U8),
            WireField::natural("sa_month", FieldTy::U8),
            WireField::natural("sa_year", FieldTy::U8),
            WireField::natural("sa_sizeof_long", FieldTy::I8),
            WireField::natural("sa_sysname", FieldTy::Bytes(UTSNAME_LEN)),
            WireField::natural("sa_nodename", FieldTy::Bytes(UTSNAME_LEN)),
            WireField::natural("sa_release", FieldTy::Bytes(UTSNAME_LEN)),
            WireField::natural("sa_machine", FieldTy::Bytes(UTSNAME_LEN)),
        ];
        let layout = WireLayout::new("file_header@2171", FIELDS);
        let expect = LayoutExpectation {
            size: 280,
            offsets: &[
                ("sa_ust_time", 0),
                ("sa_nr_act", 8),
                ("sa_day", 12),
                ("sa_sizeof_long", 15),
                ("sa_sysname", 16),
                ("sa_nodename", 81),
                ("sa_release", 146),
                ("sa_machine", 211),
            ],
        };
        let r = layout
            .resolve_verified(&lp64(), &expect)
            .expect("実データ由来の期待値と一致すること");
        assert_eq!(r.size, 280);
    }

    /// sysstat 10.1.5 の `file_activity` は全メンバ 4 バイトで 20 バイト。
    #[test]
    fn file_activity_2171_is_20_bytes() {
        const FIELDS: &[WireField] = &[
            WireField::aligned("id", FieldTy::U32, 4),
            WireField::packed("magic", FieldTy::U32),
            WireField::packed("nr", FieldTy::I32),
            WireField::packed("nr2", FieldTy::I32),
            WireField::packed("size", FieldTy::I32),
        ];
        let layout = WireLayout::new("file_activity@2171", FIELDS);
        let r = layout.resolve(&lp64()).unwrap();
        assert_eq!(r.size, 20);
        assert_eq!(r.field("size").unwrap().offset, 16);
    }

    #[test]
    fn rejects_non_power_of_two_alignment() {
        const FIELDS: &[WireField] = &[WireField::aligned("x", FieldTy::U32, 3)];
        let layout = WireLayout::new("bad", FIELDS);
        assert!(matches!(
            layout.resolve(&lp64()),
            Err(LayoutError::AlignNotPowerOfTwo { .. })
        ));
    }

    #[test]
    fn size_mismatch_is_reported() {
        let expect = LayoutExpectation {
            size: 99,
            offsets: &[],
        };
        assert!(matches!(
            RECORD_HEADER_2171.resolve_verified(&lp64(), &expect),
            Err(LayoutError::SizeMismatch { .. })
        ));
    }

    #[test]
    fn align_up_is_checked() {
        assert_eq!(align_up(0, 8), Some(0));
        assert_eq!(align_up(1, 8), Some(8));
        assert_eq!(align_up(8, 8), Some(8));
        assert_eq!(align_up(9, 16), Some(16));
        assert_eq!(align_up(usize::MAX, 8), None);
    }
}

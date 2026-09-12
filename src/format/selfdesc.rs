//! 自己記述形式 (`format_magic` = `0x2175`) のレイアウト構築。
//!
//! この世代のファイルは、各構造体について
//! 「`unsigned long long` / `unsigned long` / `int` の個数」と「構造体の実サイズ」を
//! 自分で申告する。個数が既知フィールド数より少なければ後半のフィールドが未提供、
//! 多ければ未知フィールドが増えたことを意味する。
//!
//! ## 実測で確認したバリアント
//!
//! | sysstat | header_size | hdr_types_nr | rec_types_nr | 備考 |
//! |---|---:|---|---|---|
//! | 12.0.0 | 328 | (1, 1, 11) | (2, 0, 0) | `extra_next` なし |
//! | 12.1.7 | 328 | (1, 1, 12) | (2, 0, 1) | `extra_next` 追加 |
//! | 12.5.6 | 336 | (1, 1, 12) | (2, 0, 1) | `sa_tzname` 追加 |
//! | 12.7.1 | 336 | (1, 1, 12) | (2, 0, 1) | 同上 |
//!
//! **12.0.0 と 12.1.7 は `header_size` が同じ 328 でありながら中身が違う。**
//! したがって申告サイズだけでは判別できず、型別個数の参照が不可欠である。

use super::abi::SourceEncoding;
use super::layouts::{TZNAME_LEN, UTSNAME_LEN};
use super::wire::{AlignSpec, FieldTy, ResolvedLayout, WireField, resolve_fields};
use crate::error::LayoutError;

/// 既知フィールド数を超えた分に割り当てる名前。値は読まずにスキップする。
const RESERVED: &str = "__reserved";

/// `file_header` の `unsigned long long` グループ。
const FH_ULL: &[WireField] = &[WireField::natural("sa_ust_time", FieldTy::U64)];

/// `file_header` の `unsigned long` グループ。
const FH_UL: &[WireField] = &[WireField::aligned("sa_hz", FieldTy::CULong, 8)];

/// `file_header` の `int` グループ。宣言順がそのまま wire 順。
const FH_INT: &[WireField] = &[
    WireField::aligned("sa_cpu_nr", FieldTy::U32, 8),
    WireField::natural("sa_act_nr", FieldTy::U32),
    WireField::natural("sa_year", FieldTy::I32),
    WireField::natural("act_types_nr_0", FieldTy::U32),
    WireField::natural("act_types_nr_1", FieldTy::U32),
    WireField::natural("act_types_nr_2", FieldTy::U32),
    WireField::natural("rec_types_nr_0", FieldTy::U32),
    WireField::natural("rec_types_nr_1", FieldTy::U32),
    WireField::natural("rec_types_nr_2", FieldTy::U32),
    WireField::natural("act_size", FieldTy::U32),
    WireField::natural("rec_size", FieldTy::U32),
    WireField::natural("extra_next", FieldTy::U32),
];

/// `file_header` の型グループより後ろに続く固定部分。
const FH_TAIL: &[WireField] = &[
    WireField::natural("sa_day", FieldTy::U8),
    WireField::natural("sa_month", FieldTy::U8),
    WireField::natural("sa_sizeof_long", FieldTy::I8),
    WireField::natural("sa_sysname", FieldTy::Bytes(UTSNAME_LEN)),
    WireField::natural("sa_nodename", FieldTy::Bytes(UTSNAME_LEN)),
    WireField::natural("sa_release", FieldTy::Bytes(UTSNAME_LEN)),
    WireField::natural("sa_machine", FieldTy::Bytes(UTSNAME_LEN)),
];

/// v12.5 以降で末尾に加わる。申告サイズとの差で在否を判定する。
const FH_TZNAME: WireField = WireField::natural("sa_tzname", FieldTy::Bytes(TZNAME_LEN));

/// `file_activity` の `int` グループ (ull / ul グループは空)。
const FA_INT: &[WireField] = &[
    WireField::natural("id", FieldTy::U32),
    WireField::natural("magic", FieldTy::U32),
    WireField::natural("nr", FieldTy::I32),
    WireField::natural("nr2", FieldTy::I32),
    WireField::natural("has_nr", FieldTy::I32),
    WireField::natural("size", FieldTy::I32),
    WireField::natural("types_nr_0", FieldTy::U32),
    WireField::natural("types_nr_1", FieldTy::U32),
    WireField::natural("types_nr_2", FieldTy::U32),
];

/// `record_header` の `unsigned long long` グループ。
const RH_ULL: &[WireField] = &[
    WireField::natural("uptime_cs", FieldTy::U64),
    WireField::natural("ust_time", FieldTy::U64),
];

/// `record_header` の `int` グループ。
const RH_INT: &[WireField] = &[WireField::natural("extra_next", FieldTy::U32)];

/// `record_header` の固定部分。
const RH_TAIL: &[WireField] = &[
    WireField::natural("record_type", FieldTy::U8),
    WireField::natural("hour", FieldTy::U8),
    WireField::natural("minute", FieldTy::U8),
    WireField::natural("second", FieldTy::U8),
];

/// `extra_desc` の `int` グループ。
const XD_INT: &[WireField] = &[
    WireField::natural("extra_nr", FieldTy::U32),
    WireField::natural("extra_size", FieldTy::U32),
    WireField::natural("extra_next", FieldTy::U32),
    WireField::natural("extra_types_nr_0", FieldTy::U32),
    WireField::natural("extra_types_nr_1", FieldTy::U32),
    WireField::natural("extra_types_nr_2", FieldTy::U32),
];

/// 型別フィールド数の申告値。`[unsigned long long, unsigned long, int]` の順。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TypesNr(pub [u32; 3]);

impl TypesNr {
    pub fn ull(&self) -> u32 {
        self.0[0]
    }
    pub fn ul(&self) -> u32 {
        self.0[1]
    }
    pub fn int(&self) -> u32 {
        self.0[2]
    }

    /// 上限を超える申告を弾く。壊れたファイルで巨大な確保をしないため。
    pub fn within(&self, limit: u32) -> bool {
        self.0.iter().all(|&n| n <= limit)
    }

    /// 数えられているフィールドが占める先頭部分のバイト数。
    ///
    /// `unsigned long long` と `unsigned long` は 8 バイトスロット、
    /// `int` は 4 バイト。末尾の文字列フィールドやパディングは含まない。
    ///
    /// したがって **`map_size() <= 申告サイズ`** が常に成り立たなければならない
    /// (等号ではない: 末尾に文字列やパディングがある構造体では申告サイズの方が大きい)。
    pub fn map_size(&self) -> u64 {
        self.0[0] as u64 * 8 + self.0[1] as u64 * 8 + self.0[2] as u64 * 4
    }
}

/// 1 グループ分のフィールドを積む。
///
/// - 申告数が既知フィールド数**以下**なら、先頭から申告数だけ使う (残りは未提供)。
/// - 申告数が既知フィールド数を**超える**なら、超過分を予備フィールドとして積み、
///   バイト位置だけを正しく進める。値は読まない。
fn push_group(out: &mut Vec<WireField>, known: &[WireField], count: u32, filler: FieldTy) {
    let count = count as usize;
    let take = count.min(known.len());
    out.extend_from_slice(&known[..take]);
    for _ in take..count {
        out.push(WireField::natural(RESERVED, filler));
    }
}

/// 自己記述形式の `file_header` を構築して解決する。
///
/// `declared_size` は `file_magic.header_size` の申告値。
/// 型グループと既知の固定部分を置いた残りが `sa_tzname` 1 本分あれば、それを加える。
pub fn resolve_file_header(
    types_nr: TypesNr,
    declared_size: usize,
    enc: &SourceEncoding,
) -> Result<ResolvedLayout, LayoutError> {
    let mut fields: Vec<WireField> = Vec::with_capacity(24);
    push_group(&mut fields, FH_ULL, types_nr.ull(), FieldTy::U64);
    push_group(&mut fields, FH_UL, types_nr.ul(), FieldTy::CULong);
    push_group(&mut fields, FH_INT, types_nr.int(), FieldTy::U32);
    fields.extend_from_slice(FH_TAIL);

    let without_tz = resolve_fields("file_header@2175", &fields, AlignSpec::Natural, enc)?;
    if declared_size >= without_tz.size + TZNAME_LEN as usize {
        fields.push(FH_TZNAME);
        return resolve_fields("file_header@2175", &fields, AlignSpec::Natural, enc);
    }
    Ok(without_tz)
}

/// 自己記述形式の `file_activity` を構築して解決する。
pub fn resolve_file_activity(
    types_nr: TypesNr,
    enc: &SourceEncoding,
) -> Result<ResolvedLayout, LayoutError> {
    let mut fields: Vec<WireField> = Vec::with_capacity(12);
    push_group(&mut fields, &[], types_nr.ull(), FieldTy::U64);
    push_group(&mut fields, &[], types_nr.ul(), FieldTy::CULong);
    push_group(&mut fields, FA_INT, types_nr.int(), FieldTy::U32);
    resolve_fields("file_activity@2175", &fields, AlignSpec::Natural, enc)
}

/// 自己記述形式の `record_header` を構築して解決する。
pub fn resolve_record_header(
    types_nr: TypesNr,
    enc: &SourceEncoding,
) -> Result<ResolvedLayout, LayoutError> {
    let mut fields: Vec<WireField> = Vec::with_capacity(8);
    push_group(&mut fields, RH_ULL, types_nr.ull(), FieldTy::U64);
    push_group(&mut fields, &[], types_nr.ul(), FieldTy::CULong);
    push_group(&mut fields, RH_INT, types_nr.int(), FieldTy::U32);
    fields.extend_from_slice(RH_TAIL);
    resolve_fields("record_header@2175", &fields, AlignSpec::Natural, enc)
}

/// `extra_desc` を構築して解決する。
pub fn resolve_extra_desc(
    types_nr: TypesNr,
    enc: &SourceEncoding,
) -> Result<ResolvedLayout, LayoutError> {
    let mut fields: Vec<WireField> = Vec::with_capacity(8);
    push_group(&mut fields, &[], types_nr.ull(), FieldTy::U64);
    push_group(&mut fields, &[], types_nr.ul(), FieldTy::CULong);
    push_group(&mut fields, XD_INT, types_nr.int(), FieldTy::U32);
    resolve_fields("extra_desc@2175", &fields, AlignSpec::Natural, enc)
}

/// 統計構造体を、型グループの申告だけから解決する。
///
/// activity 固有のフィールド定義が未登録でも、`file_activity.size` と
/// `types_nr` からバイト境界だけは確定できる。既知 activity のデコードには
/// `layout` 層のフィールド定義を使う。
pub fn resolve_opaque_stats(
    types_nr: TypesNr,
    enc: &SourceEncoding,
) -> Result<ResolvedLayout, LayoutError> {
    let mut fields: Vec<WireField> = Vec::new();
    push_group(&mut fields, &[], types_nr.ull(), FieldTy::U64);
    push_group(&mut fields, &[], types_nr.ul(), FieldTy::CULong);
    push_group(&mut fields, &[], types_nr.int(), FieldTy::U32);
    resolve_fields("stats@opaque", &fields, AlignSpec::Natural, enc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::abi::{Endian, LayoutAbi};

    fn lp64() -> SourceEncoding {
        SourceEncoding::new(Endian::Little, LayoutAbi::LP64)
    }

    /// v12.0.0 の実データ: hdr_types_nr=(1,1,11), header_size=328。
    #[test]
    fn file_header_v1200() {
        let r = resolve_file_header(TypesNr([1, 1, 11]), 328, &lp64()).unwrap();
        assert_eq!(r.size, 328, "申告された header_size と一致すること");
        assert_eq!(r.field("sa_ust_time").unwrap().offset, 0);
        assert_eq!(r.field("sa_hz").unwrap().offset, 8);
        assert_eq!(r.field("sa_cpu_nr").unwrap().offset, 16);
        assert_eq!(r.field("rec_size").unwrap().offset, 56);
        assert_eq!(r.field("sa_day").unwrap().offset, 60);
        assert_eq!(r.field("sa_sizeof_long").unwrap().offset, 62);
        assert_eq!(r.field("sa_sysname").unwrap().offset, 63);
        // extra_next は int が 11 個しかないので未提供
        assert!(r.field("extra_next").is_none());
        assert!(r.field("sa_tzname").is_none());
    }

    /// v12.1.7 の実データ: hdr_types_nr=(1,1,12), header_size=328 (tzname なし)。
    ///
    /// v12.0.0 と申告サイズが同じでも中身が違うことの回帰テスト。
    #[test]
    fn file_header_v1217_same_size_different_content() {
        let r = resolve_file_header(TypesNr([1, 1, 12]), 328, &lp64()).unwrap();
        assert_eq!(r.size, 328);
        assert_eq!(r.field("extra_next").unwrap().offset, 60);
        assert_eq!(r.field("sa_day").unwrap().offset, 64);
        assert_eq!(r.field("sa_sizeof_long").unwrap().offset, 66);
        assert!(r.field("sa_tzname").is_none(), "この版には tzname が無い");
    }

    /// v12.5.6 / v12.7.1 の実データ: header_size=336 で tzname あり。
    #[test]
    fn file_header_v1256_has_tzname() {
        let r = resolve_file_header(TypesNr([1, 1, 12]), 336, &lp64()).unwrap();
        assert_eq!(r.size, 336);
        assert_eq!(r.field("sa_day").unwrap().offset, 64);
        assert_eq!(r.field("sa_tzname").unwrap().offset, 327);
    }

    /// 将来 int が増えた場合も、既知フィールドの位置は保たれる。
    #[test]
    fn unknown_future_ints_are_skipped_not_misread() {
        let r = resolve_file_header(TypesNr([1, 1, 14]), 344, &lp64()).unwrap();
        // 既知の 12 個は同じ位置
        assert_eq!(r.field("extra_next").unwrap().offset, 60);
        // 未知の 2 個分 (8 バイト) だけ後続がずれる
        assert_eq!(r.field("sa_day").unwrap().offset, 72);
    }

    #[test]
    fn file_activity_is_36_bytes() {
        let r = resolve_file_activity(TypesNr([0, 0, 9]), &lp64()).unwrap();
        assert_eq!(r.size, 36, "act_size の申告値と一致");
        assert_eq!(r.field("id").unwrap().offset, 0);
        assert_eq!(r.field("size").unwrap().offset, 20);
        assert_eq!(r.field("types_nr_0").unwrap().offset, 24);
    }

    /// v12.0.0: rec_types_nr=(2,0,0) — extra_next なし。
    #[test]
    fn record_header_v1200() {
        let r = resolve_record_header(TypesNr([2, 0, 0]), &lp64()).unwrap();
        assert_eq!(r.size, 24, "rec_size の申告値と一致");
        assert_eq!(r.field("uptime_cs").unwrap().offset, 0);
        assert_eq!(r.field("ust_time").unwrap().offset, 8);
        assert_eq!(r.field("record_type").unwrap().offset, 16);
        assert!(r.field("extra_next").is_none());
    }

    /// v12.1.7 以降: rec_types_nr=(2,0,1) — サイズは同じ 24 だが record_type の位置が動く。
    #[test]
    fn record_header_current() {
        let r = resolve_record_header(TypesNr([2, 0, 1]), &lp64()).unwrap();
        assert_eq!(r.size, 24);
        assert_eq!(r.field("extra_next").unwrap().offset, 16);
        assert_eq!(r.field("record_type").unwrap().offset, 20);
        assert_eq!(r.field("second").unwrap().offset, 23);
    }

    #[test]
    fn opaque_stats_size_follows_types_nr() {
        let r = resolve_opaque_stats(TypesNr([2, 1, 3]), &lp64()).unwrap();
        // 8*2 + 8*1 + 4*3 = 36 -> align 8 -> 40
        assert_eq!(r.size, 40);
    }

    #[test]
    fn limit_check_rejects_absurd_counts() {
        assert!(TypesNr([1, 1, 12]).within(256));
        assert!(!TypesNr([1, 1, 100_000]).within(256));
    }
}

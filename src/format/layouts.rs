//! 世代別の wire レイアウト定義。
//!
//! 各定義の期待値 (サイズ・オフセット) は、本家 C ヘッダの構造体宣言と
//! 実データのバイト列という **2 つの独立した出所**で裏取りしている。
//! 特に `file_magic` の `header_size` フィールドは、そのファイル自身が
//! `file_header` の実サイズを申告しているため、計算結果の検算に使える。
//!
//! | format_magic | 使用バージョン | file_magic | file_header | file_activity | record_header |
//! |---|---|---:|---:|---:|---:|
//! | `0x2170` | 〜9.1.5 | 8 | 280 | 12 | 48 |
//! | `0x2171` | 9.1.6〜10.2 | 8 | 280 | 20 | 48 |
//! | `0x2173` | 10.3〜11.6 | 76 | 288 | 20 | 48 |
//! | `0x2175` | 11.7〜12.8 | 76 | 328〜336 | 36 | 24 |
//!
//! `0x2175` 世代の `file_header` / `file_activity` / `record_header` は
//! サイズが固定されておらず、ファイル自身が申告する型別フィールド数に従って
//! 読み替える (自己記述形式)。ここに置く定義は**現行版の基準レイアウト**であり、
//! 読み替えの基準として使う。

use super::wire::{AlignSpec, FieldTy, LayoutExpectation, WireField, WireLayout};

/// `UTSNAME_LEN`: uname 由来文字列の配列長。全世代で 65。
pub const UTSNAME_LEN: u16 = 65;
/// `TZNAME_LEN`: タイムゾーン名の配列長 (v12.5 以降)。
pub const TZNAME_LEN: u16 = 8;

// ===========================================================================
// file_magic
// ===========================================================================

/// `0x2170` / `0x2171`: バージョン情報だけの 8 バイト。
pub const FILE_MAGIC_G1: WireLayout = WireLayout::new(
    "file_magic@2170+2171",
    &[
        WireField::natural("sysstat_magic", FieldTy::U16),
        WireField::natural("format_magic", FieldTy::U16),
        WireField::natural("sysstat_version", FieldTy::U8),
        WireField::natural("sysstat_patchlevel", FieldTy::U8),
        WireField::natural("sysstat_sublevel", FieldTy::U8),
        WireField::natural("sysstat_extraversion", FieldTy::U8),
    ],
);

/// `0x2173`: `header_size` と 64 バイトの予備領域が付く。
///
/// v10.3 時点では予備領域は全てパディングだったが、v11.x が先頭 1 バイトを
/// `upgraded` として使い始めた。旧版のファイルではその位置は 0 なので、
/// 1 つの定義で両方を読める。
pub const FILE_MAGIC_G2: WireLayout = WireLayout::new(
    "file_magic@2173",
    &[
        WireField::natural("sysstat_magic", FieldTy::U16),
        WireField::natural("format_magic", FieldTy::U16),
        WireField::natural("sysstat_version", FieldTy::U8),
        WireField::natural("sysstat_patchlevel", FieldTy::U8),
        WireField::natural("sysstat_sublevel", FieldTy::U8),
        WireField::natural("sysstat_extraversion", FieldTy::U8),
        WireField::natural("header_size", FieldTy::U32),
        WireField::natural("upgraded", FieldTy::U8),
        WireField::natural("pad", FieldTy::Bytes(63)),
    ],
);

/// `0x2175`: `upgraded` が 32bit になり、`hdr_types_nr[3]` が加わる。
pub const FILE_MAGIC_G3: WireLayout = WireLayout::new(
    "file_magic@2175",
    &[
        WireField::natural("sysstat_magic", FieldTy::U16),
        WireField::natural("format_magic", FieldTy::U16),
        WireField::natural("sysstat_version", FieldTy::U8),
        WireField::natural("sysstat_patchlevel", FieldTy::U8),
        WireField::natural("sysstat_sublevel", FieldTy::U8),
        WireField::natural("sysstat_extraversion", FieldTy::U8),
        WireField::natural("header_size", FieldTy::U32),
        WireField::natural("upgraded", FieldTy::U32),
        WireField::natural("hdr_types_nr_0", FieldTy::U32),
        WireField::natural("hdr_types_nr_1", FieldTy::U32),
        WireField::natural("hdr_types_nr_2", FieldTy::U32),
        WireField::natural("pad", FieldTy::Bytes(48)),
    ],
);

// ===========================================================================
// file_header
// ===========================================================================

/// `0x2170` / `0x2171`: activity 数と uname 情報のみ。
pub const FILE_HEADER_G1: WireLayout = WireLayout::new(
    "file_header@2170+2171",
    &[
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
    ],
);

pub const FILE_HEADER_G1_EXPECT_LP64: LayoutExpectation = LayoutExpectation {
    size: 280,
    offsets: &[
        ("sa_ust_time", 0),
        ("sa_nr_act", 8),
        ("sa_day", 12),
        ("sa_month", 13),
        ("sa_year", 14),
        ("sa_sizeof_long", 15),
        ("sa_sysname", 16),
        ("sa_nodename", 81),
        ("sa_release", 146),
        ("sa_machine", 211),
    ],
};

/// `0x2173`: CPU 数と volatile activity 数が加わる。
pub const FILE_HEADER_G2: WireLayout = WireLayout::new(
    "file_header@2173",
    &[
        WireField::aligned("sa_ust_time", FieldTy::CULong, 8),
        WireField::aligned("sa_last_cpu_nr", FieldTy::U32, 8),
        WireField::natural("sa_act_nr", FieldTy::U32),
        WireField::natural("sa_vol_act_nr", FieldTy::U32),
        WireField::natural("sa_day", FieldTy::U8),
        WireField::natural("sa_month", FieldTy::U8),
        WireField::natural("sa_year", FieldTy::U8),
        WireField::natural("sa_sizeof_long", FieldTy::I8),
        WireField::natural("sa_sysname", FieldTy::Bytes(UTSNAME_LEN)),
        WireField::natural("sa_nodename", FieldTy::Bytes(UTSNAME_LEN)),
        WireField::natural("sa_release", FieldTy::Bytes(UTSNAME_LEN)),
        WireField::natural("sa_machine", FieldTy::Bytes(UTSNAME_LEN)),
    ],
);

/// 期待値の出所: v10.3.1 / v11.6.5 のデータが申告する `header_size` = 288。
pub const FILE_HEADER_G2_EXPECT_LP64: LayoutExpectation = LayoutExpectation {
    size: 288,
    offsets: &[
        ("sa_ust_time", 0),
        ("sa_last_cpu_nr", 8),
        ("sa_act_nr", 12),
        ("sa_vol_act_nr", 16),
        ("sa_day", 20),
        ("sa_sizeof_long", 23),
        ("sa_sysname", 24),
        ("sa_nodename", 89),
        ("sa_release", 154),
        ("sa_machine", 219),
    ],
};

/// `0x2175` の初出レイアウト (v12.0 相当)。
///
/// `hdr_types_nr = [1, 1, 11]` — `unsigned long long` 1 個、`unsigned long` 1 個、
/// `int` 11 個 (`sa_cpu_nr`, `sa_act_nr`, `sa_year`, `act_types_nr[3]`,
/// `rec_types_nr[3]`, `act_size`, `rec_size`)。
pub const FILE_HEADER_G3_V120: WireLayout = WireLayout::new(
    "file_header@2175/v12.0",
    &[
        WireField::natural("sa_ust_time", FieldTy::U64),
        WireField::aligned("sa_hz", FieldTy::CULong, 8),
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
        WireField::natural("sa_day", FieldTy::U8),
        WireField::natural("sa_month", FieldTy::U8),
        WireField::natural("sa_sizeof_long", FieldTy::I8),
        WireField::natural("sa_sysname", FieldTy::Bytes(UTSNAME_LEN)),
        WireField::natural("sa_nodename", FieldTy::Bytes(UTSNAME_LEN)),
        WireField::natural("sa_release", FieldTy::Bytes(UTSNAME_LEN)),
        WireField::natural("sa_machine", FieldTy::Bytes(UTSNAME_LEN)),
    ],
);

/// 期待値の出所: v12.0.0 / v12.1.7 のデータが申告する `header_size` = 328。
pub const FILE_HEADER_G3_V120_EXPECT_LP64: LayoutExpectation = LayoutExpectation {
    size: 328,
    offsets: &[
        ("sa_ust_time", 0),
        ("sa_hz", 8),
        ("sa_cpu_nr", 16),
        ("sa_act_nr", 20),
        ("sa_year", 24),
        ("act_types_nr_0", 28),
        ("rec_types_nr_0", 40),
        ("act_size", 52),
        ("rec_size", 56),
        ("sa_day", 60),
        ("sa_sizeof_long", 62),
        ("sa_sysname", 63),
    ],
};

/// `0x2175` の現行レイアウト (v12.5 以降)。`extra_next` と `sa_tzname` が加わる。
///
/// `int` グループが 12 個になり、その分だけ後続の固定部分がずれる。
/// この差を吸収するのが自己記述の読み替え。
pub const FILE_HEADER_G3_CURRENT: WireLayout = WireLayout::new(
    "file_header@2175/current",
    &[
        WireField::natural("sa_ust_time", FieldTy::U64),
        WireField::aligned("sa_hz", FieldTy::CULong, 8),
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
        WireField::natural("sa_day", FieldTy::U8),
        WireField::natural("sa_month", FieldTy::U8),
        WireField::natural("sa_sizeof_long", FieldTy::I8),
        WireField::natural("sa_sysname", FieldTy::Bytes(UTSNAME_LEN)),
        WireField::natural("sa_nodename", FieldTy::Bytes(UTSNAME_LEN)),
        WireField::natural("sa_release", FieldTy::Bytes(UTSNAME_LEN)),
        WireField::natural("sa_machine", FieldTy::Bytes(UTSNAME_LEN)),
        WireField::natural("sa_tzname", FieldTy::Bytes(TZNAME_LEN)),
    ],
);

/// 期待値の出所: v12.5.6 / v12.7.1 のデータが申告する `header_size` = 336。
pub const FILE_HEADER_G3_CURRENT_EXPECT_LP64: LayoutExpectation = LayoutExpectation {
    size: 336,
    offsets: &[
        ("sa_ust_time", 0),
        ("sa_hz", 8),
        ("sa_cpu_nr", 16),
        ("extra_next", 60),
        ("sa_day", 64),
        ("sa_sizeof_long", 66),
        ("sa_sysname", 67),
        ("sa_tzname", 327),
    ],
};

// ===========================================================================
// file_activity
// ===========================================================================

/// `0x2170`: activity magic と nr2 がまだ無い。
pub const FILE_ACTIVITY_G0: WireLayout = WireLayout::new(
    "file_activity@2170",
    &[
        WireField::aligned("id", FieldTy::U32, 4),
        WireField::packed("nr", FieldTy::I32),
        WireField::packed("size", FieldTy::I32),
    ],
);

/// `0x2171` / `0x2173`: magic と nr2 が加わる。
pub const FILE_ACTIVITY_G1: WireLayout = WireLayout::new(
    "file_activity@2171+2173",
    &[
        WireField::aligned("id", FieldTy::U32, 4),
        WireField::packed("magic", FieldTy::U32),
        WireField::packed("nr", FieldTy::I32),
        WireField::packed("nr2", FieldTy::I32),
        WireField::packed("size", FieldTy::I32),
    ],
);

/// `0x2175`: `has_nr` と `types_nr[3]` が加わる (自己記述)。
pub const FILE_ACTIVITY_G3: WireLayout = WireLayout::new(
    "file_activity@2175",
    &[
        WireField::natural("id", FieldTy::U32),
        WireField::natural("magic", FieldTy::U32),
        WireField::natural("nr", FieldTy::I32),
        WireField::natural("nr2", FieldTy::I32),
        WireField::natural("has_nr", FieldTy::I32),
        WireField::natural("size", FieldTy::I32),
        WireField::natural("types_nr_0", FieldTy::U32),
        WireField::natural("types_nr_1", FieldTy::U32),
        WireField::natural("types_nr_2", FieldTy::U32),
    ],
);

// ===========================================================================
// record_header
// ===========================================================================

/// `0x2170`〜`0x2173`: `aligned(16)` による大きな穴を持つ 48 バイト。
pub const RECORD_HEADER_G1: WireLayout = WireLayout::new(
    "record_header@2170..2173",
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

/// 期待値の出所: 実データのレコードが 48 バイト間隔で並び、
/// ファイル末尾まで残余 0 で一致することを確認済み。
pub const RECORD_HEADER_G1_EXPECT: LayoutExpectation = LayoutExpectation {
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

/// `0x2175` の初出レイアウト (v12.0 相当)。24 バイトへ縮小され、
/// `uptime` は 1/100 秒単位の `uptime_cs` に変わった。
pub const RECORD_HEADER_G3_V120: WireLayout = WireLayout::new(
    "record_header@2175/v12.0",
    &[
        WireField::natural("uptime_cs", FieldTy::U64),
        WireField::natural("ust_time", FieldTy::U64),
        WireField::natural("record_type", FieldTy::U8),
        WireField::natural("hour", FieldTy::U8),
        WireField::natural("minute", FieldTy::U8),
        WireField::natural("second", FieldTy::U8),
    ],
);

/// `0x2175` の現行レイアウト。`extra_next` が加わる。
pub const RECORD_HEADER_G3_CURRENT: WireLayout = WireLayout::new(
    "record_header@2175/current",
    &[
        WireField::natural("uptime_cs", FieldTy::U64),
        WireField::natural("ust_time", FieldTy::U64),
        WireField::natural("extra_next", FieldTy::U32),
        WireField::natural("record_type", FieldTy::U8),
        WireField::natural("hour", FieldTy::U8),
        WireField::natural("minute", FieldTy::U8),
        WireField::natural("second", FieldTy::U8),
    ],
);

/// `extra_desc` (v12.5 以降、`extra_next` が非 0 のときに続く)。
pub const EXTRA_DESC: WireLayout = WireLayout::new(
    "extra_desc",
    &[
        WireField::natural("extra_nr", FieldTy::U32),
        WireField::natural("extra_size", FieldTy::U32),
        WireField::natural("extra_next", FieldTy::U32),
        WireField::natural("extra_types_nr_0", FieldTy::U32),
        WireField::natural("extra_types_nr_1", FieldTy::U32),
        WireField::natural("extra_types_nr_2", FieldTy::U32),
    ],
);

/// 構造体アラインメント指定を明示したい場合に使う補助。
pub const fn packed_struct(layout: WireLayout) -> WireLayout {
    layout.with_struct_align(AlignSpec::Packed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::abi::{Endian, LayoutAbi, SourceEncoding};

    fn lp64() -> SourceEncoding {
        SourceEncoding::new(Endian::Little, LayoutAbi::LP64)
    }

    #[test]
    fn file_magic_sizes_match_upstream() {
        assert_eq!(FILE_MAGIC_G1.resolve(&lp64()).unwrap().size, 8);
        assert_eq!(FILE_MAGIC_G2.resolve(&lp64()).unwrap().size, 76);
        assert_eq!(FILE_MAGIC_G3.resolve(&lp64()).unwrap().size, 76);
    }

    #[test]
    fn file_header_sizes_match_header_size_field() {
        // いずれも、そのファイル自身が申告する header_size と一致すること
        FILE_HEADER_G1
            .resolve_verified(&lp64(), &FILE_HEADER_G1_EXPECT_LP64)
            .expect("0x2170/0x2171 は 280 バイト");
        FILE_HEADER_G2
            .resolve_verified(&lp64(), &FILE_HEADER_G2_EXPECT_LP64)
            .expect("0x2173 は 288 バイト");
        FILE_HEADER_G3_V120
            .resolve_verified(&lp64(), &FILE_HEADER_G3_V120_EXPECT_LP64)
            .expect("0x2175 初出は 328 バイト");
        FILE_HEADER_G3_CURRENT
            .resolve_verified(&lp64(), &FILE_HEADER_G3_CURRENT_EXPECT_LP64)
            .expect("0x2175 現行は 336 バイト");
    }

    #[test]
    fn file_activity_sizes() {
        assert_eq!(FILE_ACTIVITY_G0.resolve(&lp64()).unwrap().size, 12);
        assert_eq!(FILE_ACTIVITY_G1.resolve(&lp64()).unwrap().size, 20);
        assert_eq!(FILE_ACTIVITY_G3.resolve(&lp64()).unwrap().size, 36);
    }

    #[test]
    fn record_header_sizes() {
        RECORD_HEADER_G1
            .resolve_verified(&lp64(), &RECORD_HEADER_G1_EXPECT)
            .expect("旧世代は 48 バイト");
        assert_eq!(RECORD_HEADER_G3_V120.resolve(&lp64()).unwrap().size, 24);
        assert_eq!(RECORD_HEADER_G3_CURRENT.resolve(&lp64()).unwrap().size, 24);
    }

    /// 自己記述形式の型別個数が、レイアウト定義と整合していること。
    ///
    /// v12.0.0 の実データは `hdr_types_nr = [1, 1, 11]` を申告する。
    #[test]
    fn hdr_types_nr_matches_v120_layout() {
        let r = FILE_HEADER_G3_V120.resolve(&lp64()).unwrap();
        let ull = r
            .fields
            .iter()
            .filter(|f| f.ty == FieldTy::U64 || f.ty == FieldTy::I64)
            .count();
        let ul = r
            .fields
            .iter()
            .filter(|f| f.ty == FieldTy::CULong || f.ty == FieldTy::CLong)
            .count();
        let int = r
            .fields
            .iter()
            .filter(|f| f.ty == FieldTy::U32 || f.ty == FieldTy::I32)
            .count();
        assert_eq!((ull, ul, int), (1, 1, 11), "v12.0.0 の hdr_types_nr と一致");
    }

    /// 現行版は int が 1 つ増える (extra_next)。
    #[test]
    fn current_layout_has_one_more_int() {
        let r = FILE_HEADER_G3_CURRENT.resolve(&lp64()).unwrap();
        let int = r
            .fields
            .iter()
            .filter(|f| f.ty == FieldTy::U32 || f.ty == FieldTy::I32)
            .count();
        assert_eq!(int, 12);
    }
}

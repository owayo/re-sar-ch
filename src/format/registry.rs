//! フォーマット世代のレジストリ。
//!
//! 判定は `format_magic` を鍵に行う。バージョン番号の大小比較で分岐しない
//! (同じ `format_magic` が複数のバージョン範囲に跨り、逆に同じバージョンでも
//! レイアウトが違う例があるため)。
//!
//! | magic | 使用バージョン | 特徴 |
//! |---|---|---|
//! | `0x2170` | 〜9.1.5 | `file_activity` に magic / nr2 が無い。本家も変換対象外 |
//! | `0x2171` | 9.1.6〜10.2 | `file_magic` 8 バイト。RESTART はペイロードなし |
//! | `0x2173` | 10.3〜11.6 | `header_size` 付き。**RESTART の後に volatile activity リスト**が並ぶ |
//! | `0x2175` | 11.7〜12.8 | 自己記述形式。`has_nr` / `extra_desc` を持つ |
//!
//! `0x2172` / `0x2174` は欠番で実在しない。

use super::layouts;
use super::wire::WireLayout;

/// レコード種別。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordKind {
    /// 統計レコード。
    Stats,
    /// 再起動マーカー (`LINUX RESTART`)。
    Restart,
    /// 日付が変わる直前の最終統計レコード。統計として扱う。
    LastStats,
    /// コメント (`sadc -C`)。
    Comment,
    /// 拡張レコード (`0x2175` の 5〜15)。統計を伴わず、`extra_desc` チェーンのみを持つ。
    Extra(u8),
    /// 統計レコードとして扱う未知の種別 (旧世代の挙動)。
    UnknownStats(u8),
    /// 無効な種別 (`0x2175` の 16 以上)。
    Invalid(u8),
}

impl RecordKind {
    pub const R_STATS: u8 = 1;
    pub const R_RESTART: u8 = 2;
    pub const R_LAST_STATS: u8 = 3;
    pub const R_COMMENT: u8 = 4;
    pub const R_EXTRA_MIN: u8 = 5;
    pub const R_EXTRA_MAX: u8 = 15;

    /// 世代を踏まえて種別を判定する。
    ///
    /// 旧世代 (`0x2170`〜`0x2173`) には拡張レコードが存在しないため、
    /// RESTART / COMMENT 以外はすべて統計レコードとして扱う (本家と同じ)。
    /// `0x2175` では 5〜15 が拡張レコード、16 以上は無効。
    pub fn from_raw(v: u8, self_describing: bool) -> Self {
        match v {
            Self::R_STATS => RecordKind::Stats,
            Self::R_RESTART => RecordKind::Restart,
            Self::R_LAST_STATS => RecordKind::LastStats,
            Self::R_COMMENT => RecordKind::Comment,
            other if self_describing => {
                if (Self::R_EXTRA_MIN..=Self::R_EXTRA_MAX).contains(&other) {
                    RecordKind::Extra(other)
                } else {
                    RecordKind::Invalid(other)
                }
            }
            other => RecordKind::UnknownStats(other),
        }
    }

    /// 統計データを伴うレコードか。
    #[inline]
    pub fn carries_stats(self) -> bool {
        matches!(
            self,
            RecordKind::Stats | RecordKind::LastStats | RecordKind::UnknownStats(_)
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            RecordKind::Stats => "stats",
            RecordKind::Restart => "restart",
            RecordKind::LastStats => "last_stats",
            RecordKind::Comment => "comment",
            RecordKind::Extra(_) => "extra",
            RecordKind::UnknownStats(_) => "unknown_stats",
            RecordKind::Invalid(_) => "invalid",
        }
    }
}

/// コメントレコードのペイロード長。全世代で 64。
pub const MAX_COMMENT_LEN: usize = 64;

/// `extra_desc` 構造体のサイズ。「将来も変えない」と規定されているため固定値。
pub const EXTRA_DESC_SIZE: usize = 24;

/// `extra_desc.extra_nr` の上限。
pub const MAX_EXTRA_NR: u32 = 8192;

/// `extra_desc.extra_size` の上限。
pub const MAX_EXTRA_SIZE: u32 = 1024;

/// `file_activity.size` (1 item のサイズ) の上限。
pub const MAX_ITEM_STRUCT_SIZE: u32 = 1024;

/// 未知 activity の item 数上限 (`65536 × 4096`)。
pub const NR_MAX: u32 = 268_435_456;

/// sub-item 数の上限。
pub const NR2_MAX: u32 = 4096;

/// CPU 数の上限。`A_CPU` などの `nr_max` は `NR_CPUS + 1`。
pub const NR_CPUS: u32 = 8192;

/// 構造体レイアウトの供給元。
#[derive(Debug, Clone, Copy)]
pub enum StructSource {
    /// 固定レイアウト世代。
    Fixed {
        file_header: WireLayout,
        file_activity: WireLayout,
        record_header: WireLayout,
        /// `file_header` 内での `sa_sizeof_long` のオフセット。
        ///
        /// このフィールドは `unsigned long` の幅に依存しない位置にあるため、
        /// ABI を決める前に読める (bootstrap に使う)。
        sizeof_long_offset: usize,
        /// `file_header` の実サイズ (LP64)。この世代は 32bit でも同じ。
        file_header_size: usize,
    },
    /// 自己記述世代。レイアウトはファイル申告値から組み立てる。
    SelfDescribing,
}

/// 1 世代分の仕様。
#[derive(Debug, Clone, Copy)]
pub struct FormatSpec {
    pub magic: u16,
    /// 診断表示用のラベル。
    pub label: &'static str,
    /// この世代を使う sysstat バージョン範囲 (診断用)。
    pub versions: &'static str,
    pub file_magic: WireLayout,
    pub structs: StructSource,
    /// RESTART レコードのペイロード。
    pub restart_payload: RestartPayload,
}

/// RESTART レコードに続くデータ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartPayload {
    /// ペイロードなし (`0x2170` / `0x2171`)。
    None,
    /// `sa_vol_act_nr` 個の `file_activity` が並ぶ (`0x2173`)。
    ///
    /// **各エントリの `nr` で以降のレコードの item 数が変わる。**
    /// 単純にスキップすると、以降のレコード位置がすべてずれる。
    VolatileActivityList,
    /// 新しい CPU 数 1 個 (`__nr_t` = 4 バイト、`0x2175`)。
    CpuCount,
}

/// 既知の全世代。
pub const FORMATS: &[FormatSpec] = &[
    FormatSpec {
        magic: 0x2170,
        label: "2170",
        versions: "〜9.1.5",
        file_magic: layouts::FILE_MAGIC_G1,
        structs: StructSource::Fixed {
            file_header: layouts::FILE_HEADER_G1,
            file_activity: layouts::FILE_ACTIVITY_G0,
            record_header: layouts::RECORD_HEADER_G1,
            sizeof_long_offset: 15,
            file_header_size: 280,
        },
        restart_payload: RestartPayload::None,
    },
    FormatSpec {
        magic: 0x2171,
        label: "2171",
        versions: "9.1.6〜10.2.x",
        file_magic: layouts::FILE_MAGIC_G1,
        structs: StructSource::Fixed {
            file_header: layouts::FILE_HEADER_G1,
            file_activity: layouts::FILE_ACTIVITY_G1,
            record_header: layouts::RECORD_HEADER_G1,
            sizeof_long_offset: 15,
            file_header_size: 280,
        },
        restart_payload: RestartPayload::None,
    },
    FormatSpec {
        magic: 0x2173,
        label: "2173",
        versions: "10.3.1〜11.6.x",
        file_magic: layouts::FILE_MAGIC_G2,
        structs: StructSource::Fixed {
            file_header: layouts::FILE_HEADER_G2,
            file_activity: layouts::FILE_ACTIVITY_G1,
            record_header: layouts::RECORD_HEADER_G1,
            sizeof_long_offset: 23,
            file_header_size: 288,
        },
        restart_payload: RestartPayload::VolatileActivityList,
    },
    FormatSpec {
        magic: 0x2175,
        label: "2175",
        versions: "11.7.x〜12.8.x",
        file_magic: layouts::FILE_MAGIC_G3,
        structs: StructSource::SelfDescribing,
        restart_payload: RestartPayload::CpuCount,
    },
];

/// `format_magic` から世代を引く。
pub fn lookup(format_magic: u16) -> Option<&'static FormatSpec> {
    FORMATS.iter().find(|f| f.magic == format_magic)
}

/// 自己記述形式か。
#[inline]
pub fn is_self_describing(spec: &FormatSpec) -> bool {
    matches!(spec.structs, StructSource::SelfDescribing)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_four_generations_are_registered() {
        assert_eq!(FORMATS.len(), 4);
        for m in [0x2170u16, 0x2171, 0x2173, 0x2175] {
            assert!(lookup(m).is_some(), "0x{m:04x} が未登録");
        }
    }

    /// 0x2172 / 0x2174 は実在しない欠番。
    #[test]
    fn gap_magics_are_not_registered() {
        assert!(lookup(0x2172).is_none());
        assert!(lookup(0x2174).is_none());
        assert!(lookup(0x2176).is_none());
    }

    #[test]
    fn only_2173_has_volatile_activity_list_after_restart() {
        assert_eq!(
            lookup(0x2173).unwrap().restart_payload,
            RestartPayload::VolatileActivityList
        );
        assert_eq!(
            lookup(0x2171).unwrap().restart_payload,
            RestartPayload::None
        );
        assert_eq!(
            lookup(0x2175).unwrap().restart_payload,
            RestartPayload::CpuCount
        );
    }

    #[test]
    fn record_kinds_from_raw() {
        for sd in [false, true] {
            assert_eq!(RecordKind::from_raw(1, sd), RecordKind::Stats);
            assert_eq!(RecordKind::from_raw(2, sd), RecordKind::Restart);
            assert_eq!(RecordKind::from_raw(3, sd), RecordKind::LastStats);
            assert_eq!(RecordKind::from_raw(4, sd), RecordKind::Comment);
        }
    }

    /// 旧世代に拡張レコードは存在しないので、5 以上も統計レコードとして扱う。
    #[test]
    fn legacy_treats_unknown_kinds_as_stats() {
        assert_eq!(RecordKind::from_raw(9, false), RecordKind::UnknownStats(9));
        assert!(RecordKind::from_raw(9, false).carries_stats());
        assert_eq!(
            RecordKind::from_raw(200, false),
            RecordKind::UnknownStats(200)
        );
    }

    /// 自己記述世代では 5〜15 が拡張レコード、16 以上は無効。
    #[test]
    fn self_describing_has_extra_record_range() {
        assert_eq!(RecordKind::from_raw(5, true), RecordKind::Extra(5));
        assert_eq!(RecordKind::from_raw(15, true), RecordKind::Extra(15));
        assert_eq!(RecordKind::from_raw(16, true), RecordKind::Invalid(16));
        assert_eq!(RecordKind::from_raw(0, true), RecordKind::Invalid(0));
        // 拡張レコードは統計を伴わない
        assert!(!RecordKind::Extra(5).carries_stats());
        assert!(RecordKind::LastStats.carries_stats());
        assert!(!RecordKind::Restart.carries_stats());
        assert!(!RecordKind::Comment.carries_stats());
    }

    /// sizeof_long の読み取り位置は `unsigned long` の幅に依存しない。
    #[test]
    fn sizeof_long_offsets_are_abi_independent() {
        use crate::format::abi::{Endian, LayoutAbi, SourceEncoding};

        for (magic, expected) in [(0x2171u16, 15usize), (0x2173, 23)] {
            let spec = lookup(magic).unwrap();
            let StructSource::Fixed {
                file_header,
                sizeof_long_offset,
                ..
            } = spec.structs
            else {
                panic!("固定レイアウト世代のはず");
            };
            assert_eq!(sizeof_long_offset, expected);

            for abi in [LayoutAbi::LP64, LayoutAbi::I386, LayoutAbi::ILP32_ALIGN8] {
                let enc = SourceEncoding::new(Endian::Little, abi);
                let r = file_header.resolve(&enc).unwrap();
                assert_eq!(
                    r.field("sa_sizeof_long").unwrap().offset,
                    sizeof_long_offset,
                    "magic=0x{magic:04x} abi={} で位置が変わった",
                    abi.name
                );
            }
        }
    }
}

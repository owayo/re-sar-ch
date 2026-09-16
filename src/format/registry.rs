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
//! `0x215c` / `0x2172` / `0x2174` は欠番で、どのリリースでも使われていない。
//! `0x1170` は本家の採番外で、RHEL / CentOS 6.5 以降が使うベンダー派生 (下記)。

use super::abi::Endian;
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

/// `file_header.act_size` (`file_activity` 構造体のサイズ) の上限。
pub const MAX_FILE_ACTIVITY_SIZE: u32 = 1024;

/// `file_header.rec_size` (`record_header` のサイズ) の上限。
pub const MAX_RECORD_HEADER_SIZE: u32 = 512;

/// 未知 activity の item 数上限 (`65536 × 4096`)。
pub const NR_MAX: u32 = 268_435_456;

/// sub-item 数の上限。
pub const NR2_MAX: u32 = 4096;

/// CPU 数の上限。`A_CPU` などの `nr_max` は `NR_CPUS + 1`。
pub const NR_CPUS: u32 = 8192;

/// `format_magic` がファイルのどこに書かれているか。
///
/// **「先頭 2 バイトが `0xd596`、次の 2 バイトが `format_magic`」は全世代の規則ではない。**
/// `struct file_magic` が導入されたのは 8.1.1 (`0x216f`) で、それ以前は magic が
/// `file_hdr` 構造体の中に埋まっており、しかもその位置が世代で 3 回動く。
#[derive(Debug, Clone, Copy)]
pub enum MagicLocation {
    /// ファイル先頭の `struct file_magic` が持つ (8.1.1 = `0x216f` 以降)。
    ///
    /// オフセット 0 に `SYSSTAT_MAGIC` (`0xd596`)、オフセット 2 に `format_magic`。
    FileMagic(WireLayout),
    /// `file_hdr` 構造体の中に埋まっている (8.0.4 = `0x216e` 以前)。
    ///
    /// **この世代のファイルには `SYSSTAT_MAGIC` がどこにも無い。**
    /// 先頭 2 バイトは世代によって `sa_actflag` だったり `sa_ust_time` だったりする。
    Embedded {
        /// ファイル先頭からの magic のオフセット (実測値: 4 / 36 / 32)。
        offset: usize,
    },
}

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
    /// 旧モノリシック世代 (`0x216f` 以前)。
    ///
    /// 現行世代と違い、**`file_activity[]` の配列を持たない**。
    /// 記録されている統計は `file_hdr.sa_actflag` の 32bit ビットマスクで表され、
    /// item 数は `sa_proc` / `sa_serial` / `sa_iface` / `sa_irqcpu` / `sa_nr_disk` に散っている。
    /// レコードは固定長 `file_stats` (サイズは `file_hdr.sa_st_size` の申告値) で始まり、
    /// 統計レコードのときだけ可変長ブロック群が続く。
    ///
    /// **現状は識別まで。ヘッダ解釈と統計のデコードは未実装** (`docs/format/01-file-format.md` §2.8)。
    Legacy {
        /// 最初のレコードが始まるファイルオフセット (= 書き込まれるヘッダのバイト数)。
        header_size: usize,
    },
    /// magic は判明しているが、構造体レイアウトを実測できていない世代。
    ///
    /// 配布アーカイブから当該バージョンのソースを入手できなかったもの。
    /// 隣接世代から推定して読むことはしない — **値を静かに誤るくらいなら、
    /// 識別だけして明示的に拒否する**。
    Unverified,
}

/// 1 世代分の仕様。
#[derive(Debug, Clone, Copy)]
pub struct FormatSpec {
    pub magic: u16,
    /// 診断表示用のラベル。
    pub label: &'static str,
    /// この世代を使う sysstat バージョン範囲 (診断用)。
    pub versions: &'static str,
    /// `format_magic` がファイルのどこにあるか。
    pub magic_at: MagicLocation,
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
    // ===================================================================
    // 旧モノリシック世代 (sysstat 3.2.4 〜 8.1.2)
    // ===================================================================
    //
    // `struct file_magic` が無く、magic は `file_hdr` 構造体の中にある。
    // **しかもその位置が世代で 3 回動く (4 → 36 → 32)。**
    // 「先頭 2 バイトが 0xd596」はこの世代には当てはまらない。
    //
    // レイアウトを実測できた世代だけ `Legacy` で登録する。配布アーカイブから
    // ソースを入手できなかった版 (`0x215e` / `0x215f` / `0x2161` / `0x2162` / `0x2164`) は
    // **登録しない**。推定で読むより「知らない」と言う方が安全である。
    FormatSpec {
        magic: 0x115a,
        label: "115a",
        versions: "〜3.2.4",
        magic_at: MagicLocation::Embedded { offset: 4 },
        structs: StructSource::Unverified,
        restart_payload: RestartPayload::None,
    },
    FormatSpec {
        magic: 0x215a,
        label: "215a",
        versions: "3.3.2",
        magic_at: MagicLocation::Embedded { offset: 4 },
        structs: StructSource::Unverified,
        restart_payload: RestartPayload::None,
    },
    FormatSpec {
        magic: 0x215b,
        label: "215b",
        versions: "3.3.3〜3.3.5",
        magic_at: MagicLocation::Embedded { offset: 4 },
        structs: StructSource::Unverified,
        restart_payload: RestartPayload::None,
    },
    FormatSpec {
        magic: 0x215d,
        label: "215d",
        versions: "3.3.6〜4.0.7",
        magic_at: MagicLocation::Embedded { offset: 4 },
        // sizeof は 232 だが、書き出されるのは 229 バイト。
        structs: StructSource::Legacy { header_size: 229 },
        restart_payload: RestartPayload::None,
    },
    FormatSpec {
        magic: 0x2160,
        label: "2160",
        versions: "4.1.4",
        magic_at: MagicLocation::Embedded { offset: 4 },
        structs: StructSource::Unverified,
        restart_payload: RestartPayload::None,
    },
    FormatSpec {
        magic: 0x2163,
        label: "2163",
        versions: "4.1.7〜5.0.6",
        magic_at: MagicLocation::Embedded { offset: 4 },
        structs: StructSource::Legacy { header_size: 240 },
        restart_payload: RestartPayload::None,
    },
    FormatSpec {
        magic: 0x2165,
        label: "2165",
        versions: "5.1.3",
        magic_at: MagicLocation::Embedded { offset: 36 },
        structs: StructSource::Legacy { header_size: 240 },
        restart_payload: RestartPayload::None,
    },
    FormatSpec {
        magic: 0x2166,
        label: "2166",
        versions: "5.1.4〜5.1.5",
        magic_at: MagicLocation::Embedded { offset: 36 },
        structs: StructSource::Legacy { header_size: 240 },
        restart_payload: RestartPayload::None,
    },
    FormatSpec {
        magic: 0x2167,
        label: "2167",
        versions: "6.0.0〜6.0.2",
        magic_at: MagicLocation::Embedded { offset: 36 },
        structs: StructSource::Legacy { header_size: 240 },
        restart_payload: RestartPayload::None,
    },
    FormatSpec {
        magic: 0x2168,
        label: "2168",
        versions: "6.1.1〜6.1.2",
        magic_at: MagicLocation::Embedded { offset: 36 },
        structs: StructSource::Legacy { header_size: 240 },
        restart_payload: RestartPayload::None,
    },
    FormatSpec {
        magic: 0x2169,
        label: "2169",
        versions: "6.1.3〜7.0.4",
        magic_at: MagicLocation::Embedded { offset: 36 },
        structs: StructSource::Legacy { header_size: 240 },
        restart_payload: RestartPayload::None,
    },
    FormatSpec {
        magic: 0x216a,
        label: "216a",
        versions: "7.1.2",
        magic_at: MagicLocation::Embedded { offset: 36 },
        structs: StructSource::Legacy { header_size: 240 },
        restart_payload: RestartPayload::None,
    },
    FormatSpec {
        magic: 0x216b,
        label: "216b",
        versions: "7.1.3〜7.1.4",
        magic_at: MagicLocation::Embedded { offset: 36 },
        structs: StructSource::Legacy { header_size: 240 },
        restart_payload: RestartPayload::None,
    },
    FormatSpec {
        magic: 0x216c,
        label: "216c",
        versions: "7.1.5",
        magic_at: MagicLocation::Embedded { offset: 36 },
        structs: StructSource::Legacy { header_size: 240 },
        restart_payload: RestartPayload::None,
    },
    FormatSpec {
        magic: 0x216d,
        label: "216d",
        versions: "7.1.6",
        magic_at: MagicLocation::Embedded { offset: 32 },
        structs: StructSource::Legacy { header_size: 240 },
        restart_payload: RestartPayload::None,
    },
    FormatSpec {
        magic: 0x216e,
        label: "216e",
        versions: "8.0.0〜8.0.4",
        magic_at: MagicLocation::Embedded { offset: 32 },
        structs: StructSource::Legacy { header_size: 240 },
        restart_payload: RestartPayload::None,
    },
    // `file_magic` が頭に付いた最初の世代。ただし**本体は旧形式のまま**で、
    // `file_activity[]` への転換は次の `0x2170` から。ここを取り違えると
    // 「先頭が 0xd596 なら現行形式」という誤った一般化になる。
    FormatSpec {
        magic: 0x216f,
        label: "216f",
        versions: "8.1.1〜8.1.2",
        magic_at: MagicLocation::FileMagic(layouts::FILE_MAGIC_G1),
        structs: StructSource::Legacy {
            header_size: 8 + 304,
        },
        restart_payload: RestartPayload::None,
    },
    // ===================================================================
    // activity リスト世代 (sysstat 8.1.3 以降)
    // ===================================================================
    //
    // RHEL / CentOS 6.5 以降が使うベンダー派生。**本家はどのリリースでもこの値を
    // 使っていない**ので、本家のソースをいくら追っても出てこない。
    // (本家の採番は `0x115a` → `0x215a`〜`0x2175` と連番で進む。§2.3)
    //
    // 経緯: Red Hat は 9.0.4-19 (RHEL 6.3) で `/proc/diskstats` の読み方を直した際に
    // `stats_io` のフィールド型を変えたが、`format_magic` を据え置いた。そのため
    // 新しい sar が旧形式のファイルを黙って読んでしまい、値が誤って表示される事故に
    // なった (RHBZ #967386)。その修正として 9.0.4-22 (RHEL 6.5) で magic だけを
    // `0x1170` へ振り直し、旧ファイルの読み込みを拒否するようにした
    // (読みたい場合は本家 sar の `--legacy` オプション)。
    //
    // ヘッダ 4 構造体は `0x2170` と完全に同一。差は `A_IO` のレイアウトのみで、
    // upstream の 20 バイトに対しこちらは 80 バイト (`layout::activities::system`)。
    FormatSpec {
        magic: 0x1170,
        label: "1170",
        versions: "9.0.4 (RHEL/CentOS 6.5 以降)",
        magic_at: MagicLocation::FileMagic(layouts::FILE_MAGIC_G1),
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
        magic: 0x2170,
        label: "2170",
        versions: "〜9.1.5",
        magic_at: MagicLocation::FileMagic(layouts::FILE_MAGIC_G1),
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
        magic_at: MagicLocation::FileMagic(layouts::FILE_MAGIC_G1),
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
        magic_at: MagicLocation::FileMagic(layouts::FILE_MAGIC_G2),
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
        magic_at: MagicLocation::FileMagic(layouts::FILE_MAGIC_G3),
        structs: StructSource::SelfDescribing,
        restart_payload: RestartPayload::CpuCount,
    },
];

impl FormatSpec {
    /// この世代の `file_activity` が activity magic を持つか。
    ///
    /// `0x2170` 系 (upstream の `0x2170` と RHEL 派生の `0x1170`) の `file_activity` は
    /// `id` / `nr` / `size` の 3 フィールドしかなく、activity magic を書かない。
    /// デコード結果は 0 になるだけなので、**「magic が 0」と「magic を持たない」を
    /// 区別できないと、既知の revision がどれも一致せず全 activity を読み飛ばす。**
    ///
    /// この判定を `format_magic` の値比較で書いてはいけない。同じ構造を持つ世代を
    /// 足したときに必ず書き漏らす (実際 `0x1170` の追加で踏んだ)。
    /// 判定はレイアウト記述そのものから導く。
    pub fn has_activity_magic(&self) -> bool {
        match self.structs {
            StructSource::SelfDescribing => true,
            StructSource::Fixed { file_activity, .. } => file_activity.has_field("magic"),
            // 旧世代は `file_activity[]` 自体が無い (activity は `sa_actflag` のビット)。
            // レイアウト未実測の世代も「持たない」に倒す (安全側)。
            StructSource::Legacy { .. } | StructSource::Unverified => false,
        }
    }

    /// ファイル先頭の `struct file_magic` のレイアウト。
    ///
    /// **持たない世代がある** (`0x216e` 以前)。その世代では magic が `file_hdr` の
    /// 中にあるので、ここは `None` を返す。
    pub fn file_magic_layout(&self) -> Option<WireLayout> {
        match self.magic_at {
            MagicLocation::FileMagic(layout) => Some(layout),
            MagicLocation::Embedded { .. } => None,
        }
    }

    /// この世代のファイルを実際に読めるか。
    ///
    /// 識別できることと読めることは別である。旧世代は magic から世代を特定できても、
    /// ヘッダ解釈・レコード境界・統計デコードが未実装なら読めない。
    /// **「読めない」を「壊れている」と混同して報告しないために分けている。**
    pub fn is_readable(&self) -> bool {
        matches!(
            self.structs,
            StructSource::Fixed { .. } | StructSource::SelfDescribing
        )
    }
}

/// `format_magic` から世代を引く。
pub fn lookup(format_magic: u16) -> Option<&'static FormatSpec> {
    FORMATS.iter().find(|f| f.magic == format_magic)
}

/// 世代同定の結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Probe {
    /// ちょうど 1 つの世代が成立した。
    Identified { magic: u16, endian: Endian },
    /// 複数の世代が成立した。**どれを選んでも根拠がないので確定しない。**
    Ambiguous { first: u16, second: u16 },
    /// どの世代とも一致しなかった。
    Unknown,
}

/// ファイル先頭のバイト列から世代を同定する。
///
/// # なぜ「順に試して最初に当たったもの」にしないか
///
/// 旧世代は magic の位置が世代ごとに違う (オフセット 4 / 36 / 32)。
/// 「4 を見て、外れたら 36、それも外れたら 32」と書くと、
/// **判定の順序を入れ替えただけで結果が変わる**実装になる。
/// どの順序が正しいかを裏付ける根拠は仕様のどこにもない。
///
/// そこで全候補を列挙し、**成立したものがちょうど 1 つのときだけ確定する**。
/// これは `docs/design.md` §3.3 の「複数候補が成立したら曖昧として扱う。
/// 都合のよい候補を選ばない」を、ABI 判定だけでなく形式判定にも適用したものである。
pub fn probe(bytes: &[u8]) -> Probe {
    let mut first: Option<(u16, Endian)> = None;
    let mut second: Option<u16> = None;

    for spec in FORMATS {
        for endian in [Endian::Little, Endian::Big] {
            if !magic_matches(bytes, spec, endian) {
                continue;
            }
            match first {
                None => first = Some((spec.magic, endian)),
                Some((m, _)) if m == spec.magic && second.is_none() => {
                    // 同じ magic が両エンディアンで成立する = バイト反転しても
                    // 同じ値になる回文。区別できないので曖昧として扱う。
                    second = Some(spec.magic);
                }
                Some(_) if second.is_none() => second = Some(spec.magic),
                _ => {}
            }
        }
    }

    match (first, second) {
        (Some((magic, endian)), None) => Probe::Identified { magic, endian },
        (Some((f, _)), Some(s)) => Probe::Ambiguous {
            first: f,
            second: s,
        },
        (None, _) => Probe::Unknown,
    }
}

/// この世代の規則で magic が一致するか。
fn magic_matches(bytes: &[u8], spec: &FormatSpec, endian: Endian) -> bool {
    match spec.magic_at {
        // 先頭に `file_magic` がある世代。`sysstat_magic` の一致も併せて要求する
        // (`format_magic` だけでは 2 バイトしか根拠が無く、偶然の一致に弱い)。
        MagicLocation::FileMagic(_) => {
            read_u16(bytes, 0, endian) == Some(super::file::SYSSTAT_MAGIC)
                && read_u16(bytes, 2, endian) == Some(spec.magic)
        }
        // magic が `file_hdr` の内側にある世代。この世代のファイルには
        // `sysstat_magic` がそもそも書かれていない。
        MagicLocation::Embedded { offset } => read_u16(bytes, offset, endian) == Some(spec.magic),
    }
}

fn read_u16(bytes: &[u8], at: usize, endian: Endian) -> Option<u16> {
    let end = at.checked_add(2)?;
    let s = bytes.get(at..end)?;
    Some(endian.u16_from([s[0], s[1]]))
}

/// 自己記述形式か。
#[inline]
pub fn is_self_describing(spec: &FormatSpec) -> bool {
    matches!(spec.structs, StructSource::SelfDescribing)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 現行 (activity リスト) 世代 — 識別でき、かつ読める。
    const READABLE_MAGICS: &[u16] = &[0x1170, 0x2170, 0x2171, 0x2173, 0x2175];

    /// 旧モノリシック世代 — 識別はできるが、読み取りは未実装。
    const LEGACY_MAGICS: &[u16] = &[
        0x115a, 0x215a, 0x215b, 0x215d, 0x2160, 0x2163, 0x2165, 0x2166, 0x2167, 0x2168, 0x2169,
        0x216a, 0x216b, 0x216c, 0x216d, 0x216e, 0x216f,
    ];

    #[test]
    fn all_known_generations_are_registered() {
        for &m in READABLE_MAGICS {
            let spec = lookup(m).unwrap_or_else(|| panic!("0x{m:04x} が未登録"));
            assert!(spec.is_readable(), "0x{m:04x} は読めるはず");
        }
        for &m in LEGACY_MAGICS {
            let spec = lookup(m).unwrap_or_else(|| panic!("0x{m:04x} が未登録"));
            assert!(
                !spec.is_readable(),
                "0x{m:04x} の読み取りはまだ未実装のはず"
            );
        }
        assert_eq!(
            FORMATS.len(),
            READABLE_MAGICS.len() + LEGACY_MAGICS.len(),
            "この表に載っていない世代が登録されている"
        );
    }

    /// 同じ magic を 2 回登録すると `lookup` が先勝ちで黙って一方を隠す。
    #[test]
    fn no_magic_is_registered_twice() {
        let mut seen = std::collections::BTreeSet::new();
        for f in FORMATS {
            assert!(
                seen.insert(f.magic),
                "0x{:04x} ({}) が重複登録されている",
                f.magic,
                f.label
            );
        }
    }

    /// magic の在処は世代で 3 回動く。ここを取り違えると別世代として誤認する。
    #[test]
    fn legacy_generations_carry_the_magic_inside_the_header() {
        use MagicLocation::*;
        for (m, want) in [
            (0x115au16, 4usize),
            (0x215a, 4),
            (0x215b, 4),
            (0x215d, 4),
            (0x2160, 4),
            (0x2163, 4),
            (0x2165, 36),
            (0x216c, 36),
            (0x216d, 32),
            (0x216e, 32),
        ] {
            let Embedded { offset } = lookup(m).unwrap().magic_at else {
                panic!("0x{m:04x} は file_hdr の内側に magic を持つはず");
            };
            assert_eq!(offset, want, "0x{m:04x} の magic の位置");
        }
        // `0x216f` だけは file_magic を持ちながら本体は旧形式、という中間世代。
        assert!(matches!(lookup(0x216f).unwrap().magic_at, FileMagic(_)));
        assert!(!lookup(0x216f).unwrap().is_readable());
    }

    /// 旧世代は `file_hdr` の内側に magic を持つ。
    /// 「先頭 2 バイトが `0xd596`」を前提にすると、これらは
    /// 「sysstat のファイルではない」という**誤った診断**になる。
    #[test]
    fn legacy_generations_are_identified_by_their_embedded_magic() {
        for (magic, offset) in [(0x215du16, 4usize), (0x2163, 4), (0x2169, 36), (0x216e, 32)] {
            let mut bytes = vec![0u8; 64];
            bytes[offset..offset + 2].copy_from_slice(&magic.to_le_bytes());
            assert_eq!(
                probe(&bytes),
                Probe::Identified {
                    magic,
                    endian: Endian::Little
                },
                "0x{magic:04x} を offset {offset} から見つけられること"
            );
        }
    }

    /// big-endian で書かれた旧世代も同じ規則で同定できる。
    #[test]
    fn legacy_generations_are_identified_in_big_endian_too() {
        let mut bytes = vec![0u8; 64];
        bytes[36..38].copy_from_slice(&0x2169u16.to_be_bytes());
        assert_eq!(
            probe(&bytes),
            Probe::Identified {
                magic: 0x2169,
                endian: Endian::Big
            }
        );
    }

    /// 現行世代は `sysstat_magic` の一致まで要求する。
    /// `format_magic` の 2 バイトだけでは偶然の一致に弱い。
    #[test]
    fn modern_generations_require_the_sysstat_magic_too() {
        let mut bytes = vec![0u8; 64];
        // `format_magic` だけ置いて `sysstat_magic` を欠くと成立しない。
        bytes[2..4].copy_from_slice(&0x2175u16.to_le_bytes());
        assert_eq!(probe(&bytes), Probe::Unknown);

        bytes[0..2].copy_from_slice(&super::super::file::SYSSTAT_MAGIC.to_le_bytes());
        assert_eq!(
            probe(&bytes),
            Probe::Identified {
                magic: 0x2175,
                endian: Endian::Little
            }
        );
    }

    /// 別々の位置で別々の世代が同時に成立したら、**都合のよい方を選ばずに拒否する**。
    /// どちらが正しいかを裏付ける根拠が仕様のどこにも無いため。
    #[test]
    fn two_simultaneous_matches_are_reported_as_ambiguous() {
        let mut bytes = vec![0u8; 64];
        bytes[4..6].copy_from_slice(&0x215du16.to_le_bytes()); // 旧世代 A
        bytes[36..38].copy_from_slice(&0x2169u16.to_le_bytes()); // 旧世代 B
        assert!(
            matches!(probe(&bytes), Probe::Ambiguous { .. }),
            "同時成立は曖昧として拒否すること"
        );
    }

    /// 短すぎる入力で読み出しがはみ出さないこと。
    #[test]
    fn probe_does_not_read_past_the_end() {
        for len in 0..40usize {
            let bytes = vec![0u8; len];
            // パニックしないこと。ゼロ埋めはどの magic とも一致しない。
            assert_eq!(probe(&bytes), Probe::Unknown, "len={len}");
        }
    }

    /// RHEL 派生 (`0x1170`) はヘッダ構造が `0x2170` と完全に同一でなければならない。
    /// 差は `A_IO` のレイアウトだけであり、そこが崩れると前提が壊れる。
    #[test]
    fn rhel_variant_shares_header_layout_with_2170() {
        let rhel = lookup(0x1170).unwrap();
        let upstream = lookup(0x2170).unwrap();

        assert_eq!(rhel.restart_payload, upstream.restart_payload);

        let (StructSource::Fixed { .. }, StructSource::Fixed { .. }) =
            (rhel.structs, upstream.structs)
        else {
            panic!("どちらも固定レイアウト世代のはず");
        };
        assert_eq!(
            format!("{:?}", rhel.structs),
            format!("{:?}", upstream.structs),
            "0x1170 のヘッダ構造は 0x2170 と同一でなければならない"
        );
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

    /// **回帰テスト**: activity magic の有無は世代ごとに正しく判定されなければならない。
    ///
    /// かつて `series::snapshot` がこれを `format_magic != 0x2170` という値比較で
    /// 判定していたため、同じ構造を持つ `0x1170` を足したときに
    /// 「magic を持つ世代」と誤認し、**全 activity が無言で読み飛ばされた**
    /// (ヘッダ表示は通り、終了コードも 0 なので気付きにくい)。
    /// 世代を足したらこの表も必ず増やすこと。
    #[test]
    fn activity_magic_presence_matches_the_file_activity_layout() {
        for magic in [0x1170u16, 0x2170] {
            assert!(
                !lookup(magic).unwrap().has_activity_magic(),
                "0x{magic:04x} の file_activity は activity magic を持たない"
            );
        }
        for magic in [0x2171u16, 0x2173, 0x2175] {
            assert!(
                lookup(magic).unwrap().has_activity_magic(),
                "0x{magic:04x} の file_activity は activity magic を持つ"
            );
        }
    }

    /// 上の判定が全世代について漏れなく決まること (新世代を足したら必ず通る)。
    #[test]
    fn every_generation_answers_whether_it_has_activity_magic() {
        for spec in FORMATS {
            let has = spec.has_activity_magic();
            match spec.structs {
                StructSource::SelfDescribing => assert!(has, "{}", spec.label),
                StructSource::Fixed { file_activity, .. } => assert_eq!(
                    has,
                    file_activity.fields.iter().any(|f| f.name == "magic"),
                    "{}: レイアウト記述と判定が食い違っている",
                    spec.label
                ),
                // 旧世代は `file_activity[]` 自体を持たない。
                StructSource::Legacy { .. } | StructSource::Unverified => {
                    assert!(!has, "{}: 旧世代に activity magic は無い", spec.label)
                }
            }
        }
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

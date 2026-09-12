//! 自作 `sa` fixture 生成器。
//!
//! # 位置づけ
//!
//! 本家 sysstat のテストデータは GPL-2.0-or-later なので同梱できない
//! (`docs/format/04-test-data.md` §6)。そこで**自分で組み立てた最小の `sa` ファイル**を
//! バイト列として生成する。自作物なので MIT で同梱できる。
//!
//! # 設計上の最重要の約束
//!
//! **このモジュールは `re_sar_ch::format::layouts` / `selfdesc` のレイアウト定義を使わない。**
//! 本体と同じ定義から fixture を作ると、定義自体の誤りが往復して通ってしまい、
//! 配置の正しさを検証できなくなる (`docs/design.md` §8)。
//! ここでは `docs/format/01-file-format.md` §3 のオフセット表を見ながら
//! **バイト位置を独立に書き下ろす**。各定数には根拠となる節番号をコメントで添える。
//!
//! 本体側と共有するのは「バイト順」と「`sizeof(long)`」という ABI パラメータの概念だけで、
//! それも [`FixtureAbi`] として独自に定義している。
//!
//! # 対応範囲
//!
//! | 世代 | `format_magic` | `file_header` | 自己記述値 |
//! |---|---|---:|---|
//! | [`Generation::G2170`] | `0x2170` | 280 | (なし) |
//! | [`Generation::G2171`] | `0x2171` | 280 | (なし) |
//! | [`Generation::G2173`] | `0x2173` | 288 | (なし) |
//! | [`Generation::G2175V120`] | `0x2175` | 328 | `hdr=(1,1,11)` / `rec=(2,0,0)` |
//! | [`Generation::G2175V1217`] | `0x2175` | 328 | `hdr=(1,1,12)` / `rec=(2,0,1)` |
//! | [`Generation::G2175Current`] | `0x2175` | 336 | `hdr=(1,1,12)` / `rec=(2,0,1)` |
//!
//! ABI は 64bit LE / 64bit BE / 32bit LE / 32bit BE の 4 通り ([`FixtureAbi`])。
//!
//! 異常系は [`Corruption`] で生成する。本家 `data-12.6.0-*-err` 14 本と同じ壊し方を、
//! 自作の基準ファイルに対して適用する。

// 生成器は「今は使わないが仕様上必要な出口」を多く持つ。
// 複数のテストバイナリから include されるため、片方で未使用になるものがある。
#![allow(dead_code)]

// ===========================================================================
// 全世代共通の定数 (docs/format/01-file-format.md §3.0 / §10.1)
// ===========================================================================

/// `SYSSTAT_MAGIC`。ファイル先頭の 2 バイト (§2.1)。
pub const SYSSTAT_MAGIC: u16 = 0xd596;

/// `UTSNAME_LEN`。uname 由来文字列の配列長。全世代で 65 (§3.0)。
pub const UTSNAME_LEN: usize = 65;

/// `TZNAME_LEN`。v12.2.0 で導入 (§3.0)。
pub const TZNAME_LEN: usize = 8;

/// `MAX_COMMENT_LEN`。R_COMMENT のペイロードは長さフィールド無しの固定長 (§6.4)。
pub const MAX_COMMENT_LEN: usize = 64;

/// `extra_desc` のサイズ。「将来も変えない」と規定されている (§3.5)。
pub const EXTRA_DESC_SIZE: usize = 24;

/// `unsigned long` がファイル上で占めるスロット幅。32bit ファイルでも 8 (§3.0)。
pub const UL_SLOT_WIDTH: usize = 8;

/// `R_STATS` (§6.1)。
pub const R_STATS: u8 = 1;
/// `R_RESTART` (§6.1)。
pub const R_RESTART: u8 = 2;
/// `R_COMMENT` (§6.1)。
pub const R_COMMENT: u8 = 4;
/// `R_EXTRA_MIN` (§6.1)。
pub const R_EXTRA_MIN: u8 = 5;

/// `MAX_NR_ACT` (§10.1)。
pub const MAX_NR_ACT: u32 = 256;
/// `NR_MAX` (§10.1)。
pub const NR_MAX: i32 = 268_435_456;
/// `NR2_MAX` (§10.1)。
pub const NR2_MAX: i32 = 4096;
/// `MAX_ITEM_STRUCT_SIZE` (§10.1)。
pub const MAX_ITEM_STRUCT_SIZE: i32 = 1024;
/// `MAX_FILE_ACTIVITY_SIZE` (§10.1)。
pub const MAX_FILE_ACTIVITY_SIZE: u32 = 1024;
/// `MAX_RECORD_HEADER_SIZE` (§10.1)。
pub const MAX_RECORD_HEADER_SIZE: u32 = 512;
/// `NR_CPUS + 1`。CPU 系 activity の `nr_max` (§10.1)。
pub const CPU_NR_MAX: i32 = 8193;

/// `MAP_SIZE(types_nr)` (§4.2)。ull と ul がスロット 8 バイト、int が 4 バイト。
pub fn map_size(types_nr: [u32; 3]) -> u32 {
    types_nr[0] * 8 + types_nr[1] * 8 + types_nr[2] * 4
}

// ===========================================================================
// ABI
// ===========================================================================

/// fixture を書き出す ABI。**本体の `LayoutAbi` とは独立した定義**。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixtureAbi {
    /// 64bit little endian (x86_64 など)。
    Le64,
    /// 64bit big endian (s390x など)。
    Be64,
    /// 32bit little endian (i686 など)。`unsigned long` の有効バイト数が 4。
    Le32,
    /// 32bit big endian (ppc など)。本家 `data-ppc-11.7.2` と同じ条件。
    Be32,
}

impl FixtureAbi {
    /// 全 4 通り。
    pub const ALL: [FixtureAbi; 4] = [
        FixtureAbi::Le64,
        FixtureAbi::Be64,
        FixtureAbi::Le32,
        FixtureAbi::Be32,
    ];

    /// big endian か。
    pub fn is_big_endian(self) -> bool {
        matches!(self, FixtureAbi::Be64 | FixtureAbi::Be32)
    }

    /// `sa_sizeof_long` に書く値 (= `unsigned long` の有効バイト数)。
    pub fn long_bytes(self) -> usize {
        match self {
            FixtureAbi::Le64 | FixtureAbi::Be64 => 8,
            FixtureAbi::Le32 | FixtureAbi::Be32 => 4,
        }
    }

    /// 8 バイト整数 (`long long`) の自然アラインメント。
    ///
    /// **32bit ABI 同士でも違う。** i386 System V は `long long` を 4 境界に置くが、
    /// ARM EABI / PowerPC は 8 境界に置く。
    /// `aligned` 属性の付いていない構造体のサイズがここで変わる
    /// (該当するのは G3 の `record_header` だけ。[`GenFacts::record_header_size`] 参照)。
    pub fn u64_align(self) -> usize {
        match self {
            FixtureAbi::Le64 | FixtureAbi::Be64 => 8,
            // i686 = i386 System V
            FixtureAbi::Le32 => 4,
            // ppc (32bit)
            FixtureAbi::Be32 => 8,
        }
    }

    /// `uname -m` に相当する値。本体の ABI 推定 (`LayoutAbi::infer`) を通すために使う。
    ///
    /// 32bit の 2 つは意図的に別アーキテクチャにしてある
    /// (i686 は 8 バイト整数を 4 境界に、ppc は 8 境界に置く ABI)。
    pub fn machine(self) -> &'static str {
        match self {
            FixtureAbi::Le64 => "x86_64",
            FixtureAbi::Be64 => "s390x",
            FixtureAbi::Le32 => "i686",
            FixtureAbi::Be32 => "ppc",
        }
    }

    /// 診断・テスト名用の短い名前。
    pub fn name(self) -> &'static str {
        match self {
            FixtureAbi::Le64 => "le64",
            FixtureAbi::Be64 => "be64",
            FixtureAbi::Le32 => "le32",
            FixtureAbi::Be32 => "be32",
        }
    }
}

// ===========================================================================
// バイト列ライタ
// ===========================================================================

/// オフセット直書き用のバッファ。
///
/// すべての書き込みは「絶対オフセット + 値」で行う。
/// 構造体定義を持たないのは意図的で、オフセットを式ではなく**リテラル**で書くため。
struct Bytes {
    buf: Vec<u8>,
    big: bool,
    long_bytes: usize,
}

impl Bytes {
    fn new(abi: FixtureAbi) -> Self {
        Self {
            buf: Vec::new(),
            big: abi.is_big_endian(),
            long_bytes: abi.long_bytes(),
        }
    }

    fn len(&self) -> usize {
        self.buf.len()
    }

    /// `end` バイトまでゼロで伸ばす (パディングは必ず 0)。
    fn grow_to(&mut self, end: usize) {
        if self.buf.len() < end {
            self.buf.resize(end, 0);
        }
    }

    fn put(&mut self, off: usize, src: &[u8]) {
        self.grow_to(off + src.len());
        self.buf[off..off + src.len()].copy_from_slice(src);
    }

    fn u8(&mut self, off: usize, v: u8) {
        self.put(off, &[v]);
    }

    fn u16(&mut self, off: usize, v: u16) {
        let b = if self.big {
            v.to_be_bytes()
        } else {
            v.to_le_bytes()
        };
        self.put(off, &b);
    }

    fn u32(&mut self, off: usize, v: u32) {
        let b = if self.big {
            v.to_be_bytes()
        } else {
            v.to_le_bytes()
        };
        self.put(off, &b);
    }

    fn i32(&mut self, off: usize, v: i32) {
        self.u32(off, v as u32);
    }

    fn u64(&mut self, off: usize, v: u64) {
        let b = if self.big {
            v.to_be_bytes()
        } else {
            v.to_le_bytes()
        };
        self.put(off, &b);
    }

    /// `unsigned long` を 8 バイトのスロットへ書く。
    ///
    /// スロット幅は ABI に関わらず 8 (§3.0 の `UL_ALIGNMENT_WIDTH`)。
    /// 有効バイト数だけが `sa_sizeof_long` に従い、値は**スロットの先頭側**に入る。
    /// 32bit big endian でも先頭 4 バイトに BE の u32 が入る
    /// (本家 `data-ppc-11.7.2` の `sa_hz` = `00 00 00 64 00 00 00 00` で実証済み)。
    fn ul(&mut self, off: usize, v: u64) {
        self.grow_to(off + UL_SLOT_WIDTH);
        for b in &mut self.buf[off..off + UL_SLOT_WIDTH] {
            *b = 0;
        }
        if self.long_bytes == 8 {
            self.u64(off, v);
        } else {
            self.u32(off, v as u32);
        }
    }

    /// NUL 終端の文字列フィールド。`cap` バイトを必ず占め、余りは 0。
    fn cstr(&mut self, off: usize, cap: usize, s: &str) {
        self.grow_to(off + cap);
        for b in &mut self.buf[off..off + cap] {
            *b = 0;
        }
        let src = s.as_bytes();
        let n = src.len().min(cap.saturating_sub(1));
        self.buf[off..off + n].copy_from_slice(&src[..n]);
    }
}

// ===========================================================================
// 世代
// ===========================================================================

/// fixture の世代。`format_magic` と自己記述値の組で区別する。
///
/// **`header_size` だけでは G2175V120 と G2175V1217 を区別できない** (どちらも 328)。
/// 区別できるのは `hdr_types_nr` のみ (§3.1 の「最重要の落とし穴」)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Generation {
    /// `0x2170` (sysstat 8.1.3〜9.1.5)。`file_activity` に `magic` / `nr2` が無い。
    G2170,
    /// `0x2171` (9.1.6〜10.2.1)。
    G2171,
    /// `0x2173` (10.3.1〜11.6.6)。RESTART の後に volatile activity リストが付く。
    G2173,
    /// `0x2175` の初出形 (11.7.1〜12.1.6)。`hdr_types_nr = (1,1,11)`、`header_size = 328`。
    G2175V120,
    /// `0x2175` (12.1.7)。`extra_next` が入り `hdr_types_nr = (1,1,12)`。`header_size` は 328 のまま。
    G2175V1217,
    /// `0x2175` の現行形 (12.2.0〜)。`sa_tzname` が付き `header_size = 336`。
    G2175Current,
}

impl Generation {
    /// 全 6 バリアント。
    pub const ALL: [Generation; 6] = [
        Generation::G2170,
        Generation::G2171,
        Generation::G2173,
        Generation::G2175V120,
        Generation::G2175V1217,
        Generation::G2175Current,
    ];

    /// 自己記述形式 (`0x2175`) の 3 バリアント。
    pub const SELF_DESCRIBED: [Generation; 3] = [
        Generation::G2175V120,
        Generation::G2175V1217,
        Generation::G2175Current,
    ];

    pub fn name(self) -> &'static str {
        self.facts().name
    }

    /// 独立に書き下ろした配置の事実。
    pub fn facts(self) -> &'static GenFacts {
        match self {
            Generation::G2170 => &FACTS_2170,
            Generation::G2171 => &FACTS_2171,
            Generation::G2173 => &FACTS_2173,
            Generation::G2175V120 => &FACTS_2175_V120,
            Generation::G2175V1217 => &FACTS_2175_V1217,
            Generation::G2175Current => &FACTS_2175_CURRENT,
        }
    }

    fn is_self_described(self) -> bool {
        matches!(
            self,
            Generation::G2175V120 | Generation::G2175V1217 | Generation::G2175Current
        )
    }
}

/// 世代ごとの配置の事実。**本体のレイアウト定義と突合するための独立した期待値**。
///
/// フィールド名は本体 (`src/format/`) の `WireField` 名に合わせてある。
/// 名前が食い違えば突合テストが「フィールドが無い」で落ちるので、それも検出になる。
pub struct GenFacts {
    pub name: &'static str,
    pub format_magic: u16,
    pub file_magic_size: usize,
    pub file_header_size: usize,
    pub file_activity_size: usize,
    /// 8 バイト整数を 8 境界に置く ABI での `record_header` サイズ。
    ///
    /// 直接は使わず [`GenFacts::record_header_size`] を通す。
    pub record_header_size_align8: usize,
    /// `file_magic.hdr_types_nr` (自己記述世代のみ)。
    pub hdr_types_nr: Option<[u32; 3]>,
    /// `file_header.act_types_nr` (自己記述世代のみ)。
    pub act_types_nr: Option<[u32; 3]>,
    /// `file_header.rec_types_nr` (自己記述世代のみ)。
    pub rec_types_nr: Option<[u32; 3]>,
    pub file_magic_offsets: &'static [(&'static str, usize)],
    pub file_header_offsets: &'static [(&'static str, usize)],
    pub file_activity_offsets: &'static [(&'static str, usize)],
    pub record_header_offsets: &'static [(&'static str, usize)],
}

impl GenFacts {
    /// この ABI での `record_header` サイズ (= `file_header.rec_size` に書かれる値)。
    ///
    /// **全構造体のうち、サイズが ABI で変わるのはこれだけ**である。
    /// 他の構造体は `__attribute__((aligned(8)))` / `aligned(16)` が付いているため
    /// 32bit / 64bit でサイズもオフセットも一致する (`01-file-format.md` §3.0)。
    ///
    /// G3 (`rec_types_nr = (2,0,0)`) の `record_header` には alignment 属性が無く、
    /// メンバの終端が 20 バイト目になる。構造体アラインメントは 8 バイト整数の
    /// 自然アラインメントで決まるので
    ///
    /// - `long long` が 8 境界の ABI (LP64 / ARM EABI / PowerPC) → 20 を 8 に丸めて **24**
    /// - `long long` が 4 境界の i386 System V → **20**
    ///
    /// となる。実データでの裏取り: 本家 `data-ppc-11.7.2` は 32bit big endian の
    /// G3 ファイルだが `rec_size = 24` を申告している (PowerPC は 8 境界)。
    ///
    /// G4 / G5 は `extra_next` が末尾の穴を埋めるため、どちらの ABI でも 24 になる。
    pub fn record_header_size(&self, abi: FixtureAbi) -> usize {
        let members_end: usize = match self.rec_types_nr {
            // G3: ull 2 個 + 時刻 4 バイト = 20
            Some([2, 0, 0]) => 20,
            _ => return self.record_header_size_align8,
        };
        let align: usize = abi.u64_align();
        members_end.div_ceil(align) * align
    }
}

// --- file_magic のオフセット (§3.2) ---

/// G0 / G1: 8 バイト (§3.2.1)。
const MAGIC_OFF_G01: &[(&str, usize)] = &[
    ("sysstat_magic", 0),
    ("format_magic", 2),
    ("sysstat_version", 4),
    ("sysstat_patchlevel", 5),
    ("sysstat_sublevel", 6),
    ("sysstat_extraversion", 7),
];

/// G2b: 76 バイト。`upgraded` は 1 バイト (§3.2.3)。
const MAGIC_OFF_G2: &[(&str, usize)] = &[
    ("sysstat_magic", 0),
    ("format_magic", 2),
    ("sysstat_version", 4),
    ("sysstat_patchlevel", 5),
    ("sysstat_sublevel", 6),
    ("sysstat_extraversion", 7),
    ("header_size", 8),
    ("upgraded", 12),
    ("pad", 13),
];

/// G3〜G5: 76 バイト。`upgraded` が 4 バイトになり `hdr_types_nr[3]` が付く (§3.2.4)。
const MAGIC_OFF_G3: &[(&str, usize)] = &[
    ("sysstat_magic", 0),
    ("format_magic", 2),
    ("sysstat_version", 4),
    ("sysstat_patchlevel", 5),
    ("sysstat_sublevel", 6),
    ("sysstat_extraversion", 7),
    ("header_size", 8),
    ("upgraded", 12),
    ("hdr_types_nr_0", 16),
    ("hdr_types_nr_1", 20),
    ("hdr_types_nr_2", 24),
    ("pad", 28),
];

// --- file_header のオフセット (§3.3) ---

/// G0 / G1: 280 バイト (§3.3.1)。`sa_ust_time` は `unsigned long`。
const FH_OFF_G01: &[(&str, usize)] = &[
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
];

/// G2: 288 バイト (§3.3.2)。`sa_vol_act_nr` が付く。
const FH_OFF_G2: &[(&str, usize)] = &[
    ("sa_ust_time", 0),
    ("sa_last_cpu_nr", 8),
    ("sa_act_nr", 12),
    ("sa_vol_act_nr", 16),
    ("sa_day", 20),
    ("sa_month", 21),
    ("sa_year", 22),
    ("sa_sizeof_long", 23),
    ("sa_sysname", 24),
    ("sa_nodename", 89),
    ("sa_release", 154),
    ("sa_machine", 219),
];

/// G3: 328 バイト / `hdr_types_nr = (1,1,11)` (§3.3.3)。`extra_next` は**無い**。
const FH_OFF_G3: &[(&str, usize)] = &[
    ("sa_ust_time", 0),
    ("sa_hz", 8),
    ("sa_cpu_nr", 16),
    ("sa_act_nr", 20),
    ("sa_year", 24),
    ("act_types_nr_0", 28),
    ("act_types_nr_1", 32),
    ("act_types_nr_2", 36),
    ("rec_types_nr_0", 40),
    ("rec_types_nr_1", 44),
    ("rec_types_nr_2", 48),
    ("act_size", 52),
    ("rec_size", 56),
    ("sa_day", 60),
    ("sa_month", 61),
    ("sa_sizeof_long", 62),
    ("sa_sysname", 63),
    ("sa_nodename", 128),
    ("sa_release", 193),
    ("sa_machine", 258),
];

/// G4: 328 バイト / `(1,1,12)` (§3.3.4)。オフセット 60 に `extra_next` が挿入され後続が 4 バイト後退。
const FH_OFF_G4: &[(&str, usize)] = &[
    ("sa_ust_time", 0),
    ("sa_hz", 8),
    ("sa_cpu_nr", 16),
    ("sa_act_nr", 20),
    ("sa_year", 24),
    ("act_types_nr_0", 28),
    ("act_types_nr_1", 32),
    ("act_types_nr_2", 36),
    ("rec_types_nr_0", 40),
    ("rec_types_nr_1", 44),
    ("rec_types_nr_2", 48),
    ("act_size", 52),
    ("rec_size", 56),
    ("extra_next", 60),
    ("sa_day", 64),
    ("sa_month", 65),
    ("sa_sizeof_long", 66),
    ("sa_sysname", 67),
    ("sa_nodename", 132),
    ("sa_release", 197),
    ("sa_machine", 262),
];

/// G5: 336 バイト / `(1,1,12)` (§3.3.5)。G4 の末尾に `sa_tzname[8]` が付く。
const FH_OFF_G5: &[(&str, usize)] = &[
    ("sa_ust_time", 0),
    ("sa_hz", 8),
    ("sa_cpu_nr", 16),
    ("sa_act_nr", 20),
    ("sa_year", 24),
    ("act_types_nr_0", 28),
    ("act_types_nr_1", 32),
    ("act_types_nr_2", 36),
    ("rec_types_nr_0", 40),
    ("rec_types_nr_1", 44),
    ("rec_types_nr_2", 48),
    ("act_size", 52),
    ("rec_size", 56),
    ("extra_next", 60),
    ("sa_day", 64),
    ("sa_month", 65),
    ("sa_sizeof_long", 66),
    ("sa_sysname", 67),
    ("sa_nodename", 132),
    ("sa_release", 197),
    ("sa_machine", 262),
    ("sa_tzname", 327),
];

// --- file_activity のオフセット (§3.4) ---

/// G0: 12 バイト (§3.4.1)。`magic` / `nr2` が無いので activity 単位の形式判別ができない。
const FA_OFF_G0: &[(&str, usize)] = &[("id", 0), ("nr", 4), ("size", 8)];

/// G1 / G2: 20 バイト (§3.4.2)。
const FA_OFF_G12: &[(&str, usize)] = &[
    ("id", 0),
    ("magic", 4),
    ("nr", 8),
    ("nr2", 12),
    ("size", 16),
];

/// G3〜G5: 36 バイト / `act_types_nr = (0,0,9)` (§3.4.3)。
const FA_OFF_G3: &[(&str, usize)] = &[
    ("id", 0),
    ("magic", 4),
    ("nr", 8),
    ("nr2", 12),
    ("has_nr", 16),
    ("size", 20),
    ("types_nr_0", 24),
    ("types_nr_1", 28),
    ("types_nr_2", 32),
];

// --- record_header のオフセット (§3.6) ---

/// G0〜G2: 48 バイト。`aligned(16)` により 8 バイトの穴が 2 箇所 (§3.6.1)。
const RH_OFF_OLD: &[(&str, usize)] = &[
    ("uptime", 0),
    ("uptime0", 16),
    ("ust_time", 32),
    ("record_type", 40),
    ("hour", 41),
    ("minute", 42),
    ("second", 43),
];

/// G3: 24 バイト / `rec_types_nr = (2,0,0)` (§3.6.2)。`extra_next` は無い。
const RH_OFF_G3: &[(&str, usize)] = &[
    ("uptime_cs", 0),
    ("ust_time", 8),
    ("record_type", 16),
    ("hour", 17),
    ("minute", 18),
    ("second", 19),
];

/// G4 / G5: 24 バイト / `rec_types_nr = (2,0,1)` (§3.6.3)。
/// サイズは同じままオフセット 16 に `extra_next` が入り、時刻が 4 バイト後退する。
const RH_OFF_G45: &[(&str, usize)] = &[
    ("uptime_cs", 0),
    ("ust_time", 8),
    ("extra_next", 16),
    ("record_type", 20),
    ("hour", 21),
    ("minute", 22),
    ("second", 23),
];

const FACTS_2170: GenFacts = GenFacts {
    name: "0x2170",
    format_magic: 0x2170,
    file_magic_size: 8,
    file_header_size: 280,
    file_activity_size: 12,
    record_header_size_align8: 48,
    hdr_types_nr: None,
    act_types_nr: None,
    rec_types_nr: None,
    file_magic_offsets: MAGIC_OFF_G01,
    file_header_offsets: FH_OFF_G01,
    file_activity_offsets: FA_OFF_G0,
    record_header_offsets: RH_OFF_OLD,
};

const FACTS_2171: GenFacts = GenFacts {
    name: "0x2171",
    format_magic: 0x2171,
    file_magic_size: 8,
    file_header_size: 280,
    file_activity_size: 20,
    record_header_size_align8: 48,
    hdr_types_nr: None,
    act_types_nr: None,
    rec_types_nr: None,
    file_magic_offsets: MAGIC_OFF_G01,
    file_header_offsets: FH_OFF_G01,
    file_activity_offsets: FA_OFF_G12,
    record_header_offsets: RH_OFF_OLD,
};

const FACTS_2173: GenFacts = GenFacts {
    name: "0x2173",
    format_magic: 0x2173,
    file_magic_size: 76,
    file_header_size: 288,
    file_activity_size: 20,
    record_header_size_align8: 48,
    hdr_types_nr: None,
    act_types_nr: None,
    rec_types_nr: None,
    file_magic_offsets: MAGIC_OFF_G2,
    file_header_offsets: FH_OFF_G2,
    file_activity_offsets: FA_OFF_G12,
    record_header_offsets: RH_OFF_OLD,
};

const FACTS_2175_V120: GenFacts = GenFacts {
    name: "0x2175/328/(1,1,11)",
    format_magic: 0x2175,
    file_magic_size: 76,
    file_header_size: 328,
    file_activity_size: 36,
    record_header_size_align8: 24,
    hdr_types_nr: Some([1, 1, 11]),
    act_types_nr: Some([0, 0, 9]),
    rec_types_nr: Some([2, 0, 0]),
    file_magic_offsets: MAGIC_OFF_G3,
    file_header_offsets: FH_OFF_G3,
    file_activity_offsets: FA_OFF_G3,
    record_header_offsets: RH_OFF_G3,
};

const FACTS_2175_V1217: GenFacts = GenFacts {
    name: "0x2175/328/(1,1,12)",
    format_magic: 0x2175,
    file_magic_size: 76,
    file_header_size: 328,
    file_activity_size: 36,
    record_header_size_align8: 24,
    hdr_types_nr: Some([1, 1, 12]),
    act_types_nr: Some([0, 0, 9]),
    rec_types_nr: Some([2, 0, 1]),
    file_magic_offsets: MAGIC_OFF_G3,
    file_header_offsets: FH_OFF_G4,
    file_activity_offsets: FA_OFF_G3,
    record_header_offsets: RH_OFF_G45,
};

const FACTS_2175_CURRENT: GenFacts = GenFacts {
    name: "0x2175/336/(1,1,12)",
    format_magic: 0x2175,
    file_magic_size: 76,
    file_header_size: 336,
    file_activity_size: 36,
    record_header_size_align8: 24,
    hdr_types_nr: Some([1, 1, 12]),
    act_types_nr: Some([0, 0, 9]),
    rec_types_nr: Some([2, 0, 1]),
    file_magic_offsets: MAGIC_OFF_G3,
    file_header_offsets: FH_OFF_G5,
    file_activity_offsets: FA_OFF_G3,
    record_header_offsets: RH_OFF_G45,
};

// ===========================================================================
// 仕様 (論理値)
// ===========================================================================

/// 1 件の activity。
///
/// 値は `docs/format/01-file-format.md` §4.6 の一覧に合わせる
/// (`nr_max` や `magic` の検査に通るようにするため)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivitySpec {
    pub id: u32,
    pub magic: u32,
    pub nr: i32,
    pub nr2: i32,
    /// 真なら統計ブロックの直前に `__nr_t` (4 バイト) が毎レコード入る。
    pub has_nr: bool,
    pub size: i32,
    pub types_nr: [u32; 3],
}

impl ActivitySpec {
    /// `A_CPU` (id=1, magic=0x8b, `types_nr = (10,0,0)`, `size = 80`)。
    pub fn a_cpu(nr: i32) -> Self {
        Self {
            id: 1,
            magic: 0x8b,
            nr,
            nr2: 1,
            has_nr: true,
            size: 80,
            types_nr: [10, 0, 0],
        }
    }

    /// `A_PCSW` (id=2, magic=0x8b, `types_nr = (1,1,0)`, `size = 16`)。
    ///
    /// `ull` 1 個 + `ul` 1 個なので、**`unsigned long` の幅差を統計側でも踏む**唯一の最小 activity。
    pub fn a_pcsw() -> Self {
        Self {
            id: 2,
            magic: 0x8b,
            nr: 1,
            nr2: 1,
            has_nr: false,
            size: 16,
            types_nr: [1, 1, 0],
        }
    }

    /// `A_QUEUE` (id=9, magic=0x8c, `types_nr = (3,0,3)`, `size = 40`)。
    pub fn a_queue() -> Self {
        Self {
            id: 9,
            magic: 0x8c,
            nr: 1,
            nr2: 1,
            has_nr: false,
            size: 40,
            types_nr: [3, 0, 3],
        }
    }

    /// 全サニティチェックを上限ぴったりで通り、`nr × nr2 × size` だけが u32 を溢れる `A_IRQ`。
    ///
    /// 本家 `data-12.7.1-A_IRQ_overflow` と同じ値: 8193 × 4096 × 1024 = 34,359,738,368。
    pub fn a_irq_overflow() -> Self {
        Self {
            id: 3,
            magic: 0x8c,
            nr: CPU_NR_MAX,
            nr2: NR2_MAX,
            has_nr: true,
            size: MAX_ITEM_STRUCT_SIZE,
            types_nr: [0, 0, 1],
        }
    }

    /// 未知 ID の activity (本家 `data-ukwn` の `id=255` 相当)。
    pub fn unknown_id() -> Self {
        Self {
            id: 255,
            magic: 0x8a,
            nr: 1,
            nr2: 1,
            has_nr: false,
            size: 8,
            types_nr: [1, 0, 0],
        }
    }

    /// 既知 ID だが未知 magic (本家 `data-ukwn` の `A_PCSW[0xff]` 相当)。
    pub fn known_id_unknown_magic() -> Self {
        Self {
            magic: 0xff,
            ..Self::a_pcsw()
        }
    }

    /// この activity の 1 レコード分のアイテム数。
    pub fn item_count(&self, record_count: Option<i32>) -> i64 {
        let n = match record_count {
            Some(c) if self.has_nr => c,
            _ => self.nr,
        };
        n as i64 * self.nr2 as i64
    }
}

/// `extra_desc` 連鎖 1 段の仕様 (§3.5)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtraSpec {
    pub nr: u32,
    pub size: u32,
    pub types_nr: [u32; 3],
}

impl ExtraSpec {
    /// 本家 `data-extra-12.1.7` と同じ形 (`nr=2`, `size=32`, `types_nr=(1,0,1)`)。
    pub fn sample() -> Self {
        Self {
            nr: 2,
            size: 32,
            types_nr: [1, 0, 1],
        }
    }

    /// この段がファイル上で占めるバイト数。
    pub fn byte_len(&self) -> usize {
        EXTRA_DESC_SIZE + self.nr as usize * self.size as usize
    }
}

/// レコードの種別とペイロード。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordKind {
    /// `R_STATS`。`counts` は activity 配列と同じ長さで、`has_nr` が真な activity の件数。
    Stats { counts: Vec<i32> },
    /// `R_RESTART`。`volatile` は `0x2173` 世代でのみ書かれる (§5.7)。
    Restart {
        cpu_nr: i32,
        volatile: Vec<ActivitySpec>,
    },
    /// `R_COMMENT`。固定 64 バイト。
    Comment { text: String },
    /// `R_EXTRA_MIN`〜`R_EXTRA_MAX` (5〜15)。統計を持たない (§6.1)。
    Extra { record_type: u8 },
}

/// 1 レコード。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordSpec {
    pub kind: RecordKind,
    /// `0x2175` では `uptime_cs` (センチ秒)、旧世代では `uptime` / `uptime0` (jiffies)。
    pub uptime: u64,
    /// epoch 秒。`ust_time >= 1_000_000_000` でないと本家の検査に落ちる (§6.1)。
    pub ust_time: u64,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    /// `record_header.extra_next` を立てて置く extra 連鎖 (自己記述世代のみ)。
    pub extra: Vec<ExtraSpec>,
}

impl RecordSpec {
    fn new(kind: RecordKind, ust_time: u64, hour: u8, minute: u8, second: u8) -> Self {
        Self {
            kind,
            uptime: 100_000,
            ust_time,
            hour,
            minute,
            second,
            extra: Vec::new(),
        }
    }

    pub fn stats(counts: Vec<i32>, ust_time: u64, hour: u8, minute: u8, second: u8) -> Self {
        Self::new(RecordKind::Stats { counts }, ust_time, hour, minute, second)
    }

    pub fn restart(cpu_nr: i32, ust_time: u64, hour: u8, minute: u8, second: u8) -> Self {
        Self::new(
            RecordKind::Restart {
                cpu_nr,
                volatile: Vec::new(),
            },
            ust_time,
            hour,
            minute,
            second,
        )
    }

    pub fn comment(text: &str, ust_time: u64, hour: u8, minute: u8, second: u8) -> Self {
        Self::new(
            RecordKind::Comment {
                text: text.to_string(),
            },
            ust_time,
            hour,
            minute,
            second,
        )
    }

    /// `R_EXTRA_MIN`〜`R_EXTRA_MAX` のレコード。統計を持たないので読み側は黙って次へ進む。
    ///
    /// 本家 `data-extra-12.1.7` が `record_type = 8` のこのレコードを含む。
    pub fn extra_record(record_type: u8, ust_time: u64, hour: u8, minute: u8, second: u8) -> Self {
        debug_assert!((R_EXTRA_MIN..=15).contains(&record_type));
        Self::new(
            RecordKind::Extra { record_type },
            ust_time,
            hour,
            minute,
            second,
        )
    }

    pub fn with_volatile(mut self, volatile: Vec<ActivitySpec>) -> Self {
        if let RecordKind::Restart { volatile: v, .. } = &mut self.kind {
            *v = volatile;
        }
        self
    }

    pub fn with_extra(mut self, extra: Vec<ExtraSpec>) -> Self {
        self.extra = extra;
        self
    }

    fn record_type(&self) -> u8 {
        match &self.kind {
            RecordKind::Stats { .. } => R_STATS,
            RecordKind::Restart { .. } => R_RESTART,
            RecordKind::Comment { .. } => R_COMMENT,
            RecordKind::Extra { record_type } => *record_type,
        }
    }
}

/// ファイル全体の仕様。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixtureSpec {
    pub generation: Generation,
    pub abi: FixtureAbi,
    /// `file_magic` の版数 (version, patchlevel, sublevel, extraversion)。
    pub version: (u8, u8, u8, u8),
    /// 非 0 なら `sadf -c` による変換済み (§2.6)。
    pub upgraded: u32,
    pub ust_time: u64,
    /// `sa_hz`。自己記述世代のみ持つ (§3.3.3)。
    pub hz: u64,
    /// `sa_cpu_nr` / `sa_last_cpu_nr` (CPU "all" を含む数)。
    pub cpu_nr: u32,
    pub day: u8,
    /// 0 起点 (`tm_mon`)。
    pub month: u8,
    /// 1900 起点 (`tm_year`)。
    pub year: i32,
    pub sysname: String,
    pub nodename: String,
    pub release: String,
    pub machine: String,
    pub tzname: String,
    pub activities: Vec<ActivitySpec>,
    pub records: Vec<RecordSpec>,
    /// `file_header.extra_next` を立てて `file_activity[]` の直後に置く extra 連鎖。
    pub file_extra: Vec<ExtraSpec>,
}

impl FixtureSpec {
    /// 最小構成の雛形 (activity / レコードは空)。
    ///
    /// 論理値は世代・ABI に依らず同一にしてある。これが
    /// 「LE / BE / 32bit / 64bit で同じ正規化結果になる」性質検証の前提になる。
    pub fn skeleton(generation: Generation, abi: FixtureAbi) -> Self {
        Self {
            generation,
            abi,
            version: (12, 8, 0, 0),
            upgraded: 0,
            // 2020-09-13T12:26:40Z。§6.1 の `ust_time >= 1_000_000_000` を満たす。
            ust_time: 1_600_000_000,
            hz: 100,
            cpu_nr: 3,
            day: 13,
            month: 8,  // 0 起点なので 9 月
            year: 120, // 1900 + 120 = 2020
            sysname: "Linux".to_string(),
            // 実ホスト名を絶対に持ち込まない。一般的な固定値を使う。
            nodename: "testhost".to_string(),
            release: "0.0.0-resarch".to_string(),
            machine: abi.machine().to_string(),
            tzname: "UTC".to_string(),
            activities: Vec::new(),
            records: Vec::new(),
            file_extra: Vec::new(),
        }
    }

    /// `A_CPU` + `A_PCSW`、RESTART / STATS / COMMENT / STATS の 4 レコードを持つ標準構成。
    pub fn minimal(generation: Generation, abi: FixtureAbi) -> Self {
        let mut spec = Self::skeleton(generation, abi);
        let cpu_nr = spec.cpu_nr as i32;
        spec.activities = vec![ActivitySpec::a_cpu(cpu_nr), ActivitySpec::a_pcsw()];

        let restart = RecordSpec::restart(cpu_nr, 1_600_000_001, 12, 26, 41)
            // 0x2173 は RESTART の後に volatile activity リストが並ぶ (§5.7)
            .with_volatile(vec![ActivitySpec::a_cpu(cpu_nr)]);
        spec.records = vec![
            restart,
            RecordSpec::stats(vec![cpu_nr, 0], 1_600_000_011, 12, 26, 51),
            RecordSpec::comment("resarch fixture", 1_600_000_021, 12, 27, 1),
            RecordSpec::stats(vec![cpu_nr, 0], 1_600_000_031, 12, 27, 11),
        ];
        spec
    }

    /// `sa_vol_act_nr` (`0x2173` のみ意味を持つ)。
    ///
    /// RESTART レコードに並ぶ volatile activity の件数と一致しなければ、
    /// 以降のレコード位置が全部ずれる (§5.7)。
    fn vol_act_nr(&self) -> u32 {
        self.records
            .iter()
            .filter_map(|r| match &r.kind {
                RecordKind::Restart { volatile, .. } => Some(volatile.len() as u32),
                _ => None,
            })
            .max()
            .unwrap_or(0)
    }
}

// ===========================================================================
// 統計値
// ===========================================================================

/// 統計フィールドに入れる決定的な値。
///
/// - ABI・世代に依存しない
/// - `u32` に収まる (int フィールドと 32bit の `unsigned long` でも同じ値が入る)
///
/// この 2 条件があるので「LE と BE の fixture から同じ値が読める」ことを
/// そのまま期待値として書ける。
pub fn stat_value(act_id: u32, record_seq: usize, item: usize, field: usize) -> u64 {
    debug_assert!(act_id <= 0xff && record_seq <= 0xff && item <= 0xff && field < 0xff);
    ((act_id as u64) << 24)
        | ((record_seq as u64) << 16)
        | ((item as u64) << 8)
        | (field as u64 + 1)
}

// ===========================================================================
// 生成された fixture
// ===========================================================================

/// 生成された fixture。
#[derive(Debug, Clone)]
pub struct Fixture {
    /// ファイルのバイト列そのもの。
    pub bytes: Vec<u8>,
    /// 生成に使った論理値 (期待値の出所)。
    pub spec: FixtureSpec,
    /// `file_header` の開始位置 (= `file_magic` のサイズ)。
    pub file_header_off: usize,
    /// `file_activity[]` の開始位置。
    pub file_activity_off: usize,
    /// `file_header.extra_next` 由来の extra 連鎖の開始位置。
    ///
    /// **`file_activity[]` の直後**である。本家 `check_file_actlst()` が
    /// activity リストを読み切ってから `skip_extra_struct()` を呼ぶため
    /// (`docs/format/04-test-data.md` §1.3 / 実データ `data-extra-12.1.7` で確認)。
    pub extra_chain_off: usize,
    /// 最初のレコードの開始位置。
    pub first_record_off: usize,
    /// 各レコードの `(開始位置, バイト長)`。走査テストの期待値として使う。
    pub record_offsets: Vec<(usize, usize)>,
}

impl Fixture {
    pub fn generation(&self) -> Generation {
        self.spec.generation
    }

    pub fn abi(&self) -> FixtureAbi {
        self.spec.abi
    }

    pub fn facts(&self) -> &'static GenFacts {
        self.spec.generation.facts()
    }

    /// この fixture の `record_header` サイズ (ABI 依存)。
    pub fn record_header_size(&self) -> usize {
        self.facts().record_header_size(self.abi())
    }

    /// テスト名に使える識別子。
    pub fn label(&self) -> String {
        format!("{}/{}", self.facts().name, self.abi().name())
    }
}

/// 標準構成の fixture を生成する。
pub fn minimal(generation: Generation, abi: FixtureAbi) -> Fixture {
    build(FixtureSpec::minimal(generation, abi))
}

/// 4 世代 × 4 ABI の全組み合わせ。
pub fn all_minimal() -> Vec<Fixture> {
    let mut out = Vec::new();
    for generation in Generation::ALL {
        for abi in FixtureAbi::ALL {
            out.push(minimal(generation, abi));
        }
    }
    out
}

/// extra 連鎖を 3 か所 (ファイルヘッダ / RESTART / STATS) に持つ fixture。
///
/// `extra_next` を持つのは `0x2175` の `(1,1,12)` 系のみなので、それ以外の世代では
/// 連鎖を置かずに標準構成と同じものを返す。
pub fn with_extra_chains(generation: Generation, abi: FixtureAbi) -> Fixture {
    let mut spec = FixtureSpec::minimal(generation, abi);
    let has_extra_field = matches!(
        generation,
        Generation::G2175V1217 | Generation::G2175Current
    );
    if has_extra_field {
        // ファイルヘッダ由来の連鎖は 2 段にして extra_next の連結も踏む
        spec.file_extra = vec![ExtraSpec::sample(), ExtraSpec::sample()];
        for rec in &mut spec.records {
            rec.extra = vec![ExtraSpec::sample()];
        }
        // R_EXTRA レコード (統計なし) も混ぜる。本家 data-extra-12.1.7 と同じ record_type。
        spec.records.push(
            RecordSpec::extra_record(8, 1_600_000_041, 12, 27, 21)
                .with_extra(vec![ExtraSpec::sample()]),
        );
    }
    build(spec)
}

// ===========================================================================
// 生成本体
// ===========================================================================

/// 仕様からバイト列を組み立てる。
///
/// 各書き込みの第 1 引数が `docs/format/01-file-format.md` のオフセット表の値。
pub fn build(spec: FixtureSpec) -> Fixture {
    let facts = spec.generation.facts();
    let mut b = Bytes::new(spec.abi);

    // -------------------------------------------------------------------
    // file_magic (§3.2)
    // -------------------------------------------------------------------
    b.u16(0, SYSSTAT_MAGIC);
    b.u16(2, facts.format_magic);
    b.u8(4, spec.version.0);
    b.u8(5, spec.version.1);
    b.u8(6, spec.version.2);
    b.u8(7, spec.version.3);

    match spec.generation {
        Generation::G2170 | Generation::G2171 => {
            // 8 バイトで終わり。header_size も hdr_types_nr も無い (§3.2.1)。
        }
        Generation::G2173 => {
            b.u32(8, facts.file_header_size as u32); // header_size
            b.u8(12, spec.upgraded as u8); // upgraded はこの世代だけ 1 バイト (§3.2.3)
            // 13..76 は pad (ゼロ)
        }
        Generation::G2175V120 | Generation::G2175V1217 | Generation::G2175Current => {
            b.u32(8, facts.file_header_size as u32); // header_size
            b.u32(12, spec.upgraded); // upgraded (§3.2.4)
            let t = facts
                .hdr_types_nr
                .expect("自己記述世代は hdr_types_nr を持つ");
            b.u32(16, t[0]);
            b.u32(20, t[1]);
            b.u32(24, t[2]);
            // 28..76 は pad (ゼロ)
        }
    }
    b.grow_to(facts.file_magic_size);

    // -------------------------------------------------------------------
    // file_header (§3.3)
    // -------------------------------------------------------------------
    let fh = facts.file_magic_size;
    let act_nr = spec.activities.len() as u32;

    match spec.generation {
        Generation::G2170 | Generation::G2171 => {
            // §3.3.1 (280 バイト)
            b.ul(fh, spec.ust_time); // sa_ust_time: unsigned long, aligned(8)
            b.u32(fh + 8, act_nr); // sa_nr_act
            b.u8(fh + 12, spec.day); // sa_day
            b.u8(fh + 13, spec.month); // sa_month (0 起点)
            b.u8(fh + 14, spec.year as u8); // sa_year: この世代は unsigned char
            b.u8(fh + 15, spec.abi.long_bytes() as u8); // sa_sizeof_long
            b.cstr(fh + 16, UTSNAME_LEN, &spec.sysname);
            b.cstr(fh + 81, UTSNAME_LEN, &spec.nodename);
            b.cstr(fh + 146, UTSNAME_LEN, &spec.release);
            b.cstr(fh + 211, UTSNAME_LEN, &spec.machine);
        }
        Generation::G2173 => {
            // §3.3.2 (288 バイト)
            b.ul(fh, spec.ust_time); // sa_ust_time
            b.u32(fh + 8, spec.cpu_nr); // sa_last_cpu_nr
            b.u32(fh + 12, act_nr); // sa_act_nr
            b.u32(fh + 16, spec.vol_act_nr()); // sa_vol_act_nr
            b.u8(fh + 20, spec.day);
            b.u8(fh + 21, spec.month);
            b.u8(fh + 22, spec.year as u8);
            b.u8(fh + 23, spec.abi.long_bytes() as u8);
            b.cstr(fh + 24, UTSNAME_LEN, &spec.sysname);
            b.cstr(fh + 89, UTSNAME_LEN, &spec.nodename);
            b.cstr(fh + 154, UTSNAME_LEN, &spec.release);
            b.cstr(fh + 219, UTSNAME_LEN, &spec.machine);
        }
        Generation::G2175V120 | Generation::G2175V1217 | Generation::G2175Current => {
            // §3.3.3〜§3.3.5。共通部分 (オフセット 0〜59) は 3 バリアントで同一。
            b.u64(fh, spec.ust_time); // sa_ust_time: unsigned long long
            b.ul(fh + 8, spec.hz); // sa_hz: unsigned long, aligned(8)
            b.u32(fh + 16, spec.cpu_nr); // sa_cpu_nr: aligned(8)
            b.u32(fh + 20, act_nr); // sa_act_nr
            b.i32(fh + 24, spec.year); // sa_year: この世代は int
            let at = facts
                .act_types_nr
                .expect("自己記述世代は act_types_nr を持つ");
            b.u32(fh + 28, at[0]);
            b.u32(fh + 32, at[1]);
            b.u32(fh + 36, at[2]);
            let rt = facts
                .rec_types_nr
                .expect("自己記述世代は rec_types_nr を持つ");
            b.u32(fh + 40, rt[0]);
            b.u32(fh + 44, rt[1]);
            b.u32(fh + 48, rt[2]);
            b.u32(fh + 52, facts.file_activity_size as u32); // act_size
            b.u32(fh + 56, facts.record_header_size(spec.abi) as u32); // rec_size

            if spec.generation == Generation::G2175V120 {
                // int が 11 個しかない世代には extra_next が無く、時刻が 4 バイト手前 (§3.3.3)
                b.u8(fh + 60, spec.day);
                b.u8(fh + 61, spec.month);
                b.u8(fh + 62, spec.abi.long_bytes() as u8);
                b.cstr(fh + 63, UTSNAME_LEN, &spec.sysname);
                b.cstr(fh + 128, UTSNAME_LEN, &spec.nodename);
                b.cstr(fh + 193, UTSNAME_LEN, &spec.release);
                b.cstr(fh + 258, UTSNAME_LEN, &spec.machine);
            } else {
                // §3.3.4 / §3.3.5
                b.u32(fh + 60, u32::from(!spec.file_extra.is_empty())); // extra_next
                b.u8(fh + 64, spec.day);
                b.u8(fh + 65, spec.month);
                b.u8(fh + 66, spec.abi.long_bytes() as u8);
                b.cstr(fh + 67, UTSNAME_LEN, &spec.sysname);
                b.cstr(fh + 132, UTSNAME_LEN, &spec.nodename);
                b.cstr(fh + 197, UTSNAME_LEN, &spec.release);
                b.cstr(fh + 262, UTSNAME_LEN, &spec.machine);
                if spec.generation == Generation::G2175Current {
                    b.cstr(fh + 327, TZNAME_LEN, &spec.tzname); // sa_tzname (§3.3.5)
                }
            }
        }
    }
    b.grow_to(fh + facts.file_header_size);

    // -------------------------------------------------------------------
    // file_activity[] (§3.4)
    // -------------------------------------------------------------------
    let fa_off = fh + facts.file_header_size;
    for (i, act) in spec.activities.iter().enumerate() {
        let o = fa_off + i * facts.file_activity_size;
        write_file_activity(&mut b, o, spec.generation, act);
    }
    let after_activities = fa_off + spec.activities.len() * facts.file_activity_size;
    b.grow_to(after_activities);

    // -------------------------------------------------------------------
    // file_header.extra_next 由来の extra 連鎖 (§3.5)
    // activity リストの直後に置かれる
    // -------------------------------------------------------------------
    let mut cur = after_activities;
    if !spec.file_extra.is_empty() {
        cur = write_extra_chain(&mut b, cur, &spec.file_extra);
    }
    let first_record_off = cur;

    // -------------------------------------------------------------------
    // レコード列 (§6)
    // -------------------------------------------------------------------
    let mut record_offsets = Vec::with_capacity(spec.records.len());
    for (seq, rec) in spec.records.iter().enumerate() {
        let start = cur;
        cur = write_record(&mut b, cur, &spec, facts, seq, rec);
        record_offsets.push((start, cur - start));
    }
    b.grow_to(cur);

    Fixture {
        bytes: std::mem::take(&mut b.buf),
        spec,
        file_header_off: fh,
        file_activity_off: fa_off,
        extra_chain_off: after_activities,
        first_record_off,
        record_offsets,
    }
}

fn write_file_activity(b: &mut Bytes, o: usize, generation: Generation, act: &ActivitySpec) {
    match generation {
        Generation::G2170 => {
            // §3.4.1 (12 バイト): magic / nr2 / has_nr / types_nr を持たない
            b.u32(o, act.id);
            b.i32(o + 4, act.nr);
            b.i32(o + 8, act.size);
        }
        Generation::G2171 | Generation::G2173 => {
            // §3.4.2 (20 バイト)
            b.u32(o, act.id);
            b.u32(o + 4, act.magic);
            b.i32(o + 8, act.nr);
            b.i32(o + 12, act.nr2);
            b.i32(o + 16, act.size);
        }
        Generation::G2175V120 | Generation::G2175V1217 | Generation::G2175Current => {
            // §3.4.3 (36 バイト)
            b.u32(o, act.id);
            b.u32(o + 4, act.magic);
            b.i32(o + 8, act.nr);
            b.i32(o + 12, act.nr2);
            b.i32(o + 16, i32::from(act.has_nr));
            b.i32(o + 20, act.size);
            b.u32(o + 24, act.types_nr[0]);
            b.u32(o + 28, act.types_nr[1]);
            b.u32(o + 32, act.types_nr[2]);
        }
    }
}

/// `extra_desc` 連鎖を書く。最終段以外は `extra_next = 1` にする (§3.5)。
fn write_extra_chain(b: &mut Bytes, start: usize, chain: &[ExtraSpec]) -> usize {
    let mut cur = start;
    for (i, x) in chain.iter().enumerate() {
        let more = i + 1 < chain.len();
        b.u32(cur, x.nr); // extra_nr
        b.u32(cur + 4, x.size); // extra_size
        b.u32(cur + 8, u32::from(more)); // extra_next
        b.u32(cur + 12, x.types_nr[0]);
        b.u32(cur + 16, x.types_nr[1]);
        b.u32(cur + 20, x.types_nr[2]);
        cur += EXTRA_DESC_SIZE;
        for item in 0..x.nr as usize {
            // 本体の中身は未知拡張なので、読み側はスキップするだけ。
            // ここでは types_nr に沿った決定的な値を置く。
            write_typed_item(b, cur, x.types_nr, x.size as usize, 0, 0, item);
            cur += x.size as usize;
        }
    }
    cur
}

/// `types_nr` の並び (ull → ul → int) に沿って 1 アイテム分の値を書く (§4.1)。
fn write_typed_item(
    b: &mut Bytes,
    base: usize,
    types_nr: [u32; 3],
    size: usize,
    act_id: u32,
    record_seq: usize,
    item: usize,
) {
    b.grow_to(base + size);
    let mut o = base;
    let mut field = 0usize;
    for _ in 0..types_nr[0] {
        b.u64(o, stat_value(act_id, record_seq, item, field));
        o += 8;
        field += 1;
    }
    for _ in 0..types_nr[1] {
        // unsigned long は 8 バイトスロット・有効バイト数のみ ABI 依存
        b.ul(o, stat_value(act_id, record_seq, item, field));
        o += UL_SLOT_WIDTH;
        field += 1;
    }
    for _ in 0..types_nr[2] {
        b.u32(o, stat_value(act_id, record_seq, item, field) as u32);
        o += 4;
        field += 1;
    }
    debug_assert!(
        o <= base + size,
        "MAP_SIZE(types_nr) が size を超えている: {} > {}",
        o - base,
        size
    );
}

/// 1 レコードを書き、次のレコードの開始位置を返す。
fn write_record(
    b: &mut Bytes,
    start: usize,
    spec: &FixtureSpec,
    facts: &GenFacts,
    seq: usize,
    rec: &RecordSpec,
) -> usize {
    let rt = rec.record_type();
    let has_extra_field = matches!(
        spec.generation,
        Generation::G2175V1217 | Generation::G2175Current
    );
    let extra = if has_extra_field {
        &rec.extra[..]
    } else {
        &[][..]
    };

    // --- record_header (§3.6) ---
    match spec.generation {
        Generation::G2170 | Generation::G2171 | Generation::G2173 => {
            // §3.6.1 (48 バイト)。uptime / uptime0 は jiffies。
            b.u64(start, rec.uptime); // uptime, aligned(16)
            b.u64(start + 16, rec.uptime); // uptime0, aligned(16)
            b.ul(start + 32, rec.ust_time); // ust_time: unsigned long, aligned(16)
            b.u8(start + 40, rt); // record_type, aligned(8)
            b.u8(start + 41, rec.hour);
            b.u8(start + 42, rec.minute);
            b.u8(start + 43, rec.second);
        }
        Generation::G2175V120 => {
            // §3.6.2 (24 バイト)。extra_next を持たない。
            b.u64(start, rec.uptime); // uptime_cs
            b.u64(start + 8, rec.ust_time);
            b.u8(start + 16, rt);
            b.u8(start + 17, rec.hour);
            b.u8(start + 18, rec.minute);
            b.u8(start + 19, rec.second);
        }
        Generation::G2175V1217 | Generation::G2175Current => {
            // §3.6.3 (24 バイト)。extra_next が入り時刻が 4 バイト後退する。
            b.u64(start, rec.uptime); // uptime_cs
            b.u64(start + 8, rec.ust_time);
            b.u32(start + 16, u32::from(!extra.is_empty())); // extra_next
            b.u8(start + 20, rt);
            b.u8(start + 21, rec.hour);
            b.u8(start + 22, rec.minute);
            b.u8(start + 23, rec.second);
        }
    }
    let mut cur = start + facts.record_header_size(spec.abi);
    b.grow_to(cur);

    // --- extra 連鎖の位置はレコード種別で違う (§1.1) ---
    // R_STATS / R_EXTRA* は record_header の直後、R_RESTART は CPU 数の後、
    // R_COMMENT はコメント 64 バイトの後。
    let extra_after_header = !matches!(
        rec.kind,
        RecordKind::Restart { .. } | RecordKind::Comment { .. }
    );
    if extra_after_header && !extra.is_empty() {
        cur = write_extra_chain(b, cur, extra);
    }

    // --- ペイロード (§6.2〜§6.4) ---
    match &rec.kind {
        RecordKind::Stats { counts } => {
            for (i, act) in spec.activities.iter().enumerate() {
                let declared = counts.get(i).copied();
                let count = if spec.generation.is_self_described() && act.has_nr {
                    // has_nr が真なら統計の直前に __nr_t が毎レコード入る (§6.2)
                    let c = declared.unwrap_or(act.nr);
                    b.i32(cur, c);
                    cur += 4;
                    c
                } else {
                    // 旧世代には has_nr もレコード内件数も存在しない。件数は常に nr (§5.14)
                    act.nr
                };
                for item in 0..(count as i64 * act.nr2 as i64) as usize {
                    write_typed_item(b, cur, act.types_nr, act.size as usize, act.id, seq, item);
                    cur += act.size as usize;
                }
                b.grow_to(cur);
            }
        }
        RecordKind::Restart { cpu_nr, volatile } => {
            match spec.generation {
                Generation::G2170 | Generation::G2171 => {
                    // ペイロードなし (§5.7)
                }
                Generation::G2173 => {
                    // sa_vol_act_nr 個の old_file_activity (各 20 バイト) が並ぶ (§5.7)
                    for (i, act) in volatile.iter().enumerate() {
                        write_file_activity(
                            b,
                            cur + i * facts.file_activity_size,
                            spec.generation,
                            act,
                        );
                    }
                    cur += volatile.len() * facts.file_activity_size;
                    b.grow_to(cur);
                }
                _ => {
                    // __nr_t (4 バイト) の新しい CPU 数のみ (§6.3)
                    b.i32(cur, *cpu_nr);
                    cur += 4;
                }
            }
            if !extra.is_empty() {
                cur = write_extra_chain(b, cur, extra);
            }
        }
        RecordKind::Comment { text } => {
            // 固定 64 バイト。長さフィールドは無く NUL 終端も保証されない (§6.4)
            b.cstr(cur, MAX_COMMENT_LEN, text);
            cur += MAX_COMMENT_LEN;
            if !extra.is_empty() {
                cur = write_extra_chain(b, cur, extra);
            }
        }
        RecordKind::Extra { .. } => {
            // 統計を持たない (§6.1)
        }
    }

    b.grow_to(cur);
    cur
}

// ===========================================================================
// 異常系
// ===========================================================================

/// 異常系の壊し方。
///
/// `Hdr*` / `Act*` の 14 種は本家 `data-12.6.0-*` と**同じフィールドを同じ値に**壊す
/// (`docs/format/04-test-data.md` §2.2 / `01-file-format.md` §10.5)。
/// 本家は 448 バイトの正常ファイルの 1 フィールドだけを書き換えて作っており、
/// ここでも同じ手口を自作の基準ファイルに適用する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Corruption {
    /// `sa_act_nr = 257` > `MAX_NR_ACT (256)`。
    HdrSaActNr,
    /// `act_types_nr = (1,1,10)` → `MAP_SIZE = 56 > act_size = 36`。
    HdrMapSizeActTypesNr,
    /// `rec_types_nr = (3,1,2)` → `MAP_SIZE = 40 > rec_size = 24`。
    HdrMapSizeRecTypesNr,
    /// `act_size = 1025` > `MAX_FILE_ACTIVITY_SIZE (1024)`。
    HdrActSize,
    /// `rec_size = 513` > `MAX_RECORD_HEADER_SIZE (512)`。
    HdrRecSize,
    /// `file_activity.nr = 0` (`nr < 1`)。
    ActNrZero,
    /// `file_activity.nr = 268435457` > `NR_MAX`。
    ActNrHuge,
    /// `A_PCSW` の `nr = 2` > activity 個別の `nr_max (1)`。
    ActNrOverNrMax,
    /// `file_activity.nr2 = 0`。
    ActNr2Zero,
    /// `file_activity.nr2 = 4097` > `NR2_MAX`。
    ActNr2Huge,
    /// `file_activity.size = 0`。
    ActSizeZero,
    /// `file_activity.size = 1025` > `MAX_ITEM_STRUCT_SIZE`。
    ActSizeHuge,
    /// `types_nr = (2,2,0)` → `MAP_SIZE = 32 > size = 16`。
    ActMapSizeTypesNr,
    /// `types_nr = (0,2,0)`。`MAP_SIZE = 16 == size` なので単調性違反だけが残る。
    /// 本家 `-SARerr` と同じく**ヘッダ表示は成功すべき**境界。
    ActTypesNrNonMonotonic,
    /// `nr × nr2 × size` が u32 を溢れる (`data-12.7.1-A_IRQ_overflow` 相当)。
    IrqOverflow,
    /// 未知の `format_magic` (実在しない世代)。
    UnknownFormatMagic,
    /// `sysstat_magic` が違う (sysstat のファイルではない)。
    BadSysstatMagic,
    /// レコードヘッダの途中で EOF (`data-trunc` 相当)。
    TruncatedRecordHeader,
    /// 統計ブロックの途中で EOF。
    TruncatedStats,
    /// `file_header` の途中で EOF。
    TruncatedFileHeader,
    /// `file_activity[]` の途中で EOF。
    TruncatedActivityList,
}

impl Corruption {
    /// 全件。
    pub const ALL: [Corruption; 21] = [
        Corruption::HdrSaActNr,
        Corruption::HdrMapSizeActTypesNr,
        Corruption::HdrMapSizeRecTypesNr,
        Corruption::HdrActSize,
        Corruption::HdrRecSize,
        Corruption::ActNrZero,
        Corruption::ActNrHuge,
        Corruption::ActNrOverNrMax,
        Corruption::ActNr2Zero,
        Corruption::ActNr2Huge,
        Corruption::ActSizeZero,
        Corruption::ActSizeHuge,
        Corruption::ActMapSizeTypesNr,
        Corruption::ActTypesNrNonMonotonic,
        Corruption::IrqOverflow,
        Corruption::UnknownFormatMagic,
        Corruption::BadSysstatMagic,
        Corruption::TruncatedRecordHeader,
        Corruption::TruncatedStats,
        Corruption::TruncatedFileHeader,
        Corruption::TruncatedActivityList,
    ];

    /// 本家 `tests/` の対応ファイル名 (対応物がある場合)。
    pub fn upstream_name(self) -> Option<&'static str> {
        Some(match self {
            Corruption::HdrSaActNr => "data-12.6.0-file_hdr-sa_act_nr-err",
            Corruption::HdrMapSizeActTypesNr => "data-12.6.0-file_hdr-MAP_SIZE_act_types_nr-err",
            Corruption::HdrMapSizeRecTypesNr => "data-12.6.0-file_hdr-MAP_SIZE_rec_types_nr-err",
            Corruption::HdrActSize => "data-12.6.0-file_hdr-act_size-err",
            Corruption::HdrRecSize => "data-12.6.0-file_hdr-rec_size-err",
            Corruption::ActNrZero => "data-12.6.0-file_act-nr-0-err",
            Corruption::ActNrHuge => "data-12.6.0-file_act-nr-err",
            Corruption::ActNrOverNrMax => "data-12.6.0-file_act-nr-nr_max-err",
            Corruption::ActNr2Zero => "data-12.6.0-file_act-nr2-0-err",
            Corruption::ActNr2Huge => "data-12.6.0-file_act-nr2-err",
            Corruption::ActSizeZero => "data-12.6.0-file_act-size-0-err",
            Corruption::ActSizeHuge => "data-12.6.0-file_act-size-err",
            Corruption::ActMapSizeTypesNr => "data-12.6.0-file_act-MAP_SIZE_types_nr-err",
            Corruption::ActTypesNrNonMonotonic => "data-12.6.0-file_act-types_nr-SARerr",
            Corruption::IrqOverflow => "data-12.7.1-A_IRQ_overflow",
            Corruption::TruncatedRecordHeader => "data-trunc",
            Corruption::UnknownFormatMagic
            | Corruption::BadSysstatMagic
            | Corruption::TruncatedStats
            | Corruption::TruncatedFileHeader
            | Corruption::TruncatedActivityList => return None,
        })
    }

    /// 何を検出すべきか (人間向け)。
    pub fn detects(self) -> &'static str {
        match self {
            Corruption::HdrSaActNr => "sa_act_nr > MAX_NR_ACT (256)",
            Corruption::HdrMapSizeActTypesNr => "MAP_SIZE(act_types_nr) > act_size",
            Corruption::HdrMapSizeRecTypesNr => "MAP_SIZE(rec_types_nr) > rec_size",
            Corruption::HdrActSize => "act_size > MAX_FILE_ACTIVITY_SIZE (1024)",
            Corruption::HdrRecSize => "rec_size > MAX_RECORD_HEADER_SIZE (512)",
            Corruption::ActNrZero => "file_activity.nr < 1",
            Corruption::ActNrHuge => "file_activity.nr > NR_MAX",
            Corruption::ActNrOverNrMax => "file_activity.nr > activity 個別の nr_max",
            Corruption::ActNr2Zero => "file_activity.nr2 < 1",
            Corruption::ActNr2Huge => "file_activity.nr2 > NR2_MAX",
            Corruption::ActSizeZero => "file_activity.size <= 0",
            Corruption::ActSizeHuge => "file_activity.size > MAX_ITEM_STRUCT_SIZE (1024)",
            Corruption::ActMapSizeTypesNr => "MAP_SIZE(types_nr) > file_activity.size",
            Corruption::ActTypesNrNonMonotonic => {
                "types_nr の単調性違反 (ヘッダ表示は成功し統計読みで失敗)"
            }
            Corruption::IrqOverflow => "nr × nr2 × size が u32 を溢れる",
            Corruption::UnknownFormatMagic => "未知の format_magic",
            Corruption::BadSysstatMagic => "sysstat のデータファイルではない",
            Corruption::TruncatedRecordHeader => "レコードヘッダ途中で EOF",
            Corruption::TruncatedStats => "統計ブロック途中で EOF",
            Corruption::TruncatedFileHeader => "file_header 途中で EOF",
            Corruption::TruncatedActivityList => "file_activity[] 途中で EOF",
        }
    }

    /// ヘッダ表示 (`sadf -H` 相当) でも失敗すべきか。
    ///
    /// `ActTypesNrNonMonotonic` と `IrqOverflow` はヘッダ表示は成功しなければならない
    /// (`04-test-data.md` §5.2 / 本家テスト 00732 / 00734)。
    pub fn fails_header_only_mode(self) -> bool {
        !matches!(
            self,
            Corruption::ActTypesNrNonMonotonic | Corruption::IrqOverflow
        )
    }
}

/// 壊した fixture。
#[derive(Debug, Clone)]
pub struct Corrupted {
    /// 壊した後のバイト列。
    pub bytes: Vec<u8>,
    /// 壊す前のバイト列 (差分検査用)。
    pub base: Vec<u8>,
    /// 書き換えたバイト範囲 `(offset, len)`。切り詰めの場合は空。
    pub patched: Vec<(usize, usize)>,
    pub corruption: Corruption,
}

impl Corrupted {
    /// 基準ファイルとの差分バイト位置。
    pub fn diff_offsets(&self) -> Vec<usize> {
        let n = self.bytes.len().min(self.base.len());
        let mut out: Vec<usize> = (0..n).filter(|&i| self.bytes[i] != self.base[i]).collect();
        out.extend(n..self.base.len().max(self.bytes.len()));
        out
    }
}

/// 異常系の基準となる正常ファイル。
///
/// 本家 `data-12.6.0-*` と同じ構成 —
/// `file_magic` 76 + `file_header` 336 + `file_activity` 36 × 1 = **448 バイト**、
/// activity は `A_PCSW` 1 件のみ、レコード 0 件。
pub fn err_base(abi: FixtureAbi) -> Fixture {
    let mut spec = FixtureSpec::skeleton(Generation::G2175Current, abi);
    spec.activities = vec![ActivitySpec::a_pcsw()];
    build(spec)
}

/// 指定の壊し方で異常系 fixture を作る。
pub fn corrupted(abi: FixtureAbi, corruption: Corruption) -> Corrupted {
    use Corruption as C;

    // 切り詰め系と、別構成が必要なものは基準ファイルを差し替える。
    match corruption {
        C::IrqOverflow => {
            // 基準は「上限ぴったりだが溢れない」A_IRQ (nr2 = 1)。
            // そこから nr2 だけを NR2_MAX へ上げると、全サニティチェックを通りながら
            // nr × nr2 × size が u32 を溢れる (本家 data-12.7.1-A_IRQ_overflow と同じ性質)。
            let mut spec = FixtureSpec::skeleton(Generation::G2175Current, abi);
            spec.activities = vec![ActivitySpec {
                nr2: 1,
                ..ActivitySpec::a_irq_overflow()
            }];
            let base_fixture = build(spec);
            let fa = base_fixture.file_activity_off;
            let mut b = Bytes::new(abi);
            b.buf = base_fixture.bytes.clone();
            b.i32(fa + 12, NR2_MAX); // file_activity.nr2 (§3.4.3)
            return Corrupted {
                bytes: std::mem::take(&mut b.buf),
                base: base_fixture.bytes,
                patched: vec![(fa + 12, 4)],
                corruption,
            };
        }
        C::TruncatedRecordHeader | C::TruncatedStats => {
            let full = minimal(Generation::G2175Current, abi);
            let facts = full.facts();
            // 最終レコード (STATS) の途中で切る。
            let (last_off, _) = *full
                .record_offsets
                .last()
                .expect("標準構成はレコードを持つ");
            let cut = match corruption {
                // レコードヘッダの一部だけ残す
                C::TruncatedRecordHeader => last_off + 8,
                // ヘッダは完全、統計の途中で切る
                _ => last_off + facts.record_header_size(abi) + 8,
            };
            let mut bytes = full.bytes.clone();
            bytes.truncate(cut);
            return Corrupted {
                bytes,
                base: full.bytes,
                patched: Vec::new(),
                corruption,
            };
        }
        C::TruncatedFileHeader => {
            let full = minimal(Generation::G2175Current, abi);
            let cut = full.file_header_off + full.facts().file_header_size / 2;
            let mut bytes = full.bytes.clone();
            bytes.truncate(cut);
            return Corrupted {
                bytes,
                base: full.bytes,
                patched: Vec::new(),
                corruption,
            };
        }
        C::TruncatedActivityList => {
            let full = minimal(Generation::G2175Current, abi);
            let cut = full.file_activity_off + full.facts().file_activity_size + 4;
            let mut bytes = full.bytes.clone();
            bytes.truncate(cut);
            return Corrupted {
                bytes,
                base: full.bytes,
                patched: Vec::new(),
                corruption,
            };
        }
        _ => {}
    }

    // ここから先は「1 フィールドだけ書き換える」系。
    let base_fixture = err_base(abi);
    let base = base_fixture.bytes.clone();
    let fh = base_fixture.file_header_off;
    let fa = base_fixture.file_activity_off;

    let mut b = Bytes::new(abi);
    b.buf = base.clone();

    let mut patched = Vec::new();
    let patch_u32 = |b: &mut Bytes, patched: &mut Vec<(usize, usize)>, off: usize, v: u32| {
        b.u32(off, v);
        patched.push((off, 4));
    };

    match corruption {
        // --- file_header 系 (オフセットは §3.3.5 の表より) ---
        C::HdrSaActNr => patch_u32(&mut b, &mut patched, fh + 20, MAX_NR_ACT + 1),
        C::HdrMapSizeActTypesNr => {
            patch_u32(&mut b, &mut patched, fh + 28, 1);
            patch_u32(&mut b, &mut patched, fh + 32, 1);
            patch_u32(&mut b, &mut patched, fh + 36, 10);
        }
        C::HdrMapSizeRecTypesNr => {
            patch_u32(&mut b, &mut patched, fh + 40, 3);
            patch_u32(&mut b, &mut patched, fh + 44, 1);
            patch_u32(&mut b, &mut patched, fh + 48, 2);
        }
        C::HdrActSize => patch_u32(&mut b, &mut patched, fh + 52, MAX_FILE_ACTIVITY_SIZE + 1),
        C::HdrRecSize => patch_u32(&mut b, &mut patched, fh + 56, MAX_RECORD_HEADER_SIZE + 1),

        // --- file_activity 系 (オフセットは §3.4.3 の表より) ---
        C::ActNrZero => patch_u32(&mut b, &mut patched, fa + 8, 0),
        C::ActNrHuge => patch_u32(&mut b, &mut patched, fa + 8, NR_MAX as u32 + 1),
        C::ActNrOverNrMax => patch_u32(&mut b, &mut patched, fa + 8, 2),
        C::ActNr2Zero => patch_u32(&mut b, &mut patched, fa + 12, 0),
        C::ActNr2Huge => patch_u32(&mut b, &mut patched, fa + 12, NR2_MAX as u32 + 1),
        C::ActSizeZero => patch_u32(&mut b, &mut patched, fa + 20, 0),
        C::ActSizeHuge => patch_u32(
            &mut b,
            &mut patched,
            fa + 20,
            MAX_ITEM_STRUCT_SIZE as u32 + 1,
        ),
        C::ActMapSizeTypesNr => {
            patch_u32(&mut b, &mut patched, fa + 24, 2);
            patch_u32(&mut b, &mut patched, fa + 28, 2);
        }
        C::ActTypesNrNonMonotonic => {
            patch_u32(&mut b, &mut patched, fa + 24, 0);
            patch_u32(&mut b, &mut patched, fa + 28, 2);
        }

        // --- file_magic 系 (オフセットは §3.2.4 の表より) ---
        C::UnknownFormatMagic => {
            // 0x2172 / 0x2174 は実在せず、0x2177 も未来の未知世代 (§2.5)
            b.u16(2, 0x2177);
            patched.push((2, 2));
        }
        C::BadSysstatMagic => {
            b.u16(0, 0x1234);
            patched.push((0, 2));
        }

        C::IrqOverflow
        | C::TruncatedRecordHeader
        | C::TruncatedStats
        | C::TruncatedFileHeader
        | C::TruncatedActivityList => unreachable!("上の match で処理済み"),
    }

    Corrupted {
        bytes: std::mem::take(&mut b.buf),
        base,
        patched,
        corruption,
    }
}

/// 正常ファイルを 1 バイト刻みで切り詰めた列。
///
/// 「切り詰め入力から、完全な元レコードと異なる『正常に見える値』を生成しない」
/// という性質検証 (`docs/design.md` §8) に使う。
pub fn truncation_series(generation: Generation, abi: FixtureAbi, step: usize) -> Vec<Vec<u8>> {
    let full = minimal(generation, abi);
    let mut out = Vec::new();
    let mut len = full.bytes.len();
    let step = step.max(1);
    while len > 0 {
        len = len.saturating_sub(step);
        out.push(full.bytes[..len].to_vec());
    }
    out
}

//! `sadf` 互換出力のフィールド表。
//!
//! 本家は同じ情報を 5 箇所 (`activity.c` の `hdr_line`、`rndr_stats.c`、
//! `json_stats.c`、`xml_stats.c`、`raw_stats.c`) に**別々に手で書いて**おり、
//! 整合性は保証されていない (`docs/format/03-output-format.md` §11.2)。
//! ここでは 1 つの表に集約しつつ、**食い違いは食い違いのまま**保持する。
//! 本家のバグ 2 件 (A_PWR_USB のヘッダ列順、A_MEMORY の `kbshmem`/`kbshared`) は
//! 再現対象なので、`hdr_line` を文字列リテラルとして持ち、データ順とは独立にした。
//!
//! 出典: `docs/format/03-output-format.md`
//! §0.6 (`hdr_line` 一覧) / §4.5 (raw) / §9.5 (JSON) / §10.5 (XML) / §11 (`-d`/`-p`)。

use crate::model::ActivityId;

// ===========================================================================
// 値の書式
// ===========================================================================

/// 値の printf 相当書式。
///
/// 本家は `-d`/`-p` では `PT_*` フラグ、`-j`/`-x` では直接 `printf` で書式を選ぶ。
/// 同じフィールドで両者が違う例がある (A_PWR_FAN の `rpm` は `-d` が `%.2f`、
/// `-j` が `%llu`) ため、形式ごとに別の書式を持たせる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fmt {
    /// `%.2f` (`PT_NOFLAG`)。レート・パーセントの既定。
    R2,
    /// `%.0f` (`PT_USERND`)。四捨五入して小数点なし。A_FS の `MBfs*` のみ。
    R0,
    /// `%llu` (`PT_USEINT`)。整数 (切り捨て)。
    Int,
    /// `%s` (`PT_USESTR`)。文字列。
    Str,
    /// `%x` を引用符で囲む (JSON) / 素の `%x` (`-d`/`-p`/`-x`)。
    Hex,
    /// アイテム識別子を文字列として出す (`"all"` / `"sda"`)。
    ItemKeyStr,
    /// アイテム識別子を数値として出す (`1` / `0`)。
    ItemKeyNum,
    /// この形式では出さない。
    Skip,
}

// ===========================================================================
// アイテム
// ===========================================================================

/// アイテム識別子の作り方。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    /// アイテムを持たない。`-p` では位置にリテラル `-` が入る。
    None,
    /// CPU。添字 0 が集約行。
    /// `-p`: `all` / `cpu<N>`、`-d`: `-1` / `<N>`、`-j`/`-x`: `"all"` / `"<N>"`。
    Cpu,
    /// 文字列キー (インターフェース名・FS 名・FC ホスト名)。
    Name,
    /// 添字由来の番号。`base` が開始値 (`A_PWR_IN` は 0、`A_PWR_FAN`/`TEMP` は 1)。
    Index { prefix: &'static str, base: u32 },
    /// 列の値を番号として使う (`A_SERIAL` の `line`、`A_PWR_USB` の `BUS`、
    /// `A_PWR_BAT` の `bat_id`)。
    Column {
        col: &'static str,
        prefix: &'static str,
    },
    /// ブロックデバイス。名前はファイルに入っていないため `major`/`minor` から作る
    /// (本家の `get_devname()` のフォールバックと同じ `dev<major>-<minor>`)。
    Disk,
    /// `A_IRQ` (割り込み × CPU の行列)。専用経路で扱う。
    Irq,
}

// ===========================================================================
// セクション (AO_MULTIPLE_OUTPUTS)
// ===========================================================================

/// セクションが有効になる条件。
///
/// `AO_MULTIPLE_OUTPUTS` を持つ 3 activity (A_CPU / A_MEMORY / A_FS) は
/// `opt_flags` のビットごとに別ブロックとして出力される (§0.3)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectionGate {
    Always,
    /// `-u` (既定)。
    CpuDef,
    /// `-u ALL`。
    CpuAll,
    /// `-r` (メモリ部)。
    Memory,
    /// `-S` (スワップ部)。
    Swap,
    /// `-F` (デバイス名)。
    FsName,
    /// `-F MOUNT` (マウントポイント)。
    FsMount,
}

/// フィールドが有効になる条件。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldGate {
    Always,
    /// `-r ALL` (`AO_F_MEM_ALL`)。`hdr_line` の `&` 以降に相当する。
    MemAll,
}

// ===========================================================================
// フィールド
// ===========================================================================

/// 1 フィールドの出力定義。
#[derive(Debug, Clone, Copy)]
pub struct Field {
    /// `layout::registry` の `ColumnMeta::public_name`。
    /// 空文字は「計算対象の列を持たない」(アイテム識別子の再掲など)。
    pub col: &'static str,
    /// `-p` が毎行に出すフィールド名。空文字なら `-d`/`-p` では出さない。
    ///
    /// `-d` のフィールド名は [`Section::hdr_line`] 側から出るので、
    /// ここが `hdr_line` と食い違っていてもよい (A_MEMORY の `kbshared`)。
    pub pp: &'static str,
    /// `-d` / `-p` の値書式。
    pub dp_fmt: Fmt,
    /// `-j` のキー名。空文字なら JSON では出さない。
    pub key: &'static str,
    /// `-x` の属性名 (テキスト内容型の activity では子要素名)。
    pub attr: &'static str,
    /// `-j` / `-x` の値書式。
    pub jx_fmt: Fmt,
    pub gate: FieldGate,
}

/// raw (`-r`) の値の出し方。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawStyle {
    /// `名前; 前値; 現値;` — `pval()` 相当。
    Pval,
    /// 複数列の合計を `pval` で出す (A_CPU の `%system`)。
    PvalSum(&'static [&'static str]),
    /// 2 列の差を `pval` で出す (A_CPU `ALL` の `%usr` / `%nice`)。
    PvalDiff(&'static str, &'static str),
    /// `名前; 値;` (`%llu`) — 瞬時値。
    Int,
    /// `名前; 値;` (`%f` = 小数 6 桁)。
    ///
    /// センサ値はファイル上で IEEE-754 の `double` なので、u64 のビット列を
    /// `f64::from_bits` で解釈する (**計算ではなくデコード**)。
    Sensor,
    /// `名前; 値;` (`%s`)。
    Text,
    /// `名前; "値";` (`%s` をダブルクォートで囲む)。
    QuotedText,
    /// `名前; 値;` (`%x`)。
    Hex,
}

/// raw の 1 フィールド。
#[derive(Debug, Clone, Copy)]
pub struct RawField {
    /// `ColumnMeta::public_name`。`PvalSum` / `PvalDiff` では未使用。
    pub col: &'static str,
    /// 出力される名前。`hdr_line` 由来のものと C ソース直書きのものが混在する。
    pub name: &'static str,
    pub style: RawStyle,
}

/// raw のフィールド構成。
#[derive(Debug, Clone, Copy)]
pub enum RawSpec {
    /// `-d`/`-p` の全フィールドを `pval` で出す (アイテムを持たないカウンタ系)。
    AllPval,
    /// 明示リスト。
    Fields(&'static [RawField]),
}

/// 1 セクション (= `-d` の 1 ブロック)。
#[derive(Debug, Clone, Copy)]
pub struct Section {
    pub gate: SectionGate,
    /// アイテム識別子に使う列 (`ColumnMeta::public_name`)。空なら
    /// [`ActivitySpec::item`] の規則に従う。
    ///
    /// `A_FS` だけはセクションで切り替わる: `-F` は `fs_name`、
    /// `-F MOUNT` は `mountp` を表示する (§2.8.1)。
    pub item_col: &'static str,
    /// `activity.c` の `hdr_line` セグメント。**そのままの文字列**。
    ///
    /// `-d` のフィールド名一覧行の出典。`&` は「`-r ALL` 指定時のみ現れる境界」。
    pub hdr_line: &'static str,
    /// データの出力順 (`-d` / `-p` / `-j` / `-x` 共通)。
    pub fields: &'static [Field],
    /// `-j` / `-x` での並べ替え (`fields` の添字列)。空なら `fields` 順。
    pub jx_order: &'static [usize],
    pub raw: RawSpec,
}

// ===========================================================================
// activity
// ===========================================================================

/// XML / JSON のグループラッパ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    None,
    Network,
    PowerManagement,
    Psi,
}

impl Group {
    /// XML / JSON のラッパ名。
    pub const fn tag(self) -> &'static str {
        match self {
            Group::None => "",
            Group::Network => "network",
            Group::PowerManagement => "power-management",
            Group::Psi => "psi",
        }
    }

    /// XML のラッパに付く属性 (`per="second"` など)。
    pub const fn xml_attrs(self) -> &'static str {
        match self {
            Group::Network | Group::Psi => " per=\"second\"",
            _ => "",
        }
    }
}

/// JSON / XML での形。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// 1 サンプルに 1 個のオブジェクト / 自己終了要素。
    Object,
    /// アイテムごとの配列 / 子要素の列。
    Array,
    /// 値をテキスト内容の子要素として出す (A_MEMORY / A_HUGE)。
    TextChildren,
    /// 専用実装 (A_IRQ / A_IO)。
    Custom,
}

/// 1 activity の出力定義。
#[derive(Debug, Clone, Copy)]
pub struct ActivitySpec {
    pub id: ActivityId,
    /// `-r` の `-O debug` と `-H` に出る本家のシンボル名。
    pub name: &'static str,
    /// `-H` / 診断に出る説明文 (`act[].desc`)。
    pub desc: &'static str,
    /// `-j` のトップレベルキー。
    pub json_key: &'static str,
    /// `-x` の要素名 (ラッパを持つ場合はラッパ名)。
    pub xml_elem: &'static str,
    /// `-x` の配列要素名 (`Shape::Array` のとき)。
    pub xml_child: &'static str,
    /// `-x` のラッパ属性 (`per="second"` / `unit="kB"` など)。
    pub xml_wrapper_attrs: &'static str,
    pub group: Group,
    /// `AO_CLOSE_MARKUP` — 選択されていなくてもグループの閉じタグを出す担当 (§0.5)。
    pub closes_group: bool,
    pub item: ItemKind,
    pub shape: Shape,
    /// `-j` / `-x` で全セクションを 1 つのオブジェクトに連結するか (A_MEMORY)。
    pub merge_sections: bool,
    pub sections: &'static [Section],
}

impl ActivitySpec {
    /// 設定に合わせて有効なセクションを返す。
    pub fn active_sections(&self, cfg: &SectionConfig) -> impl Iterator<Item = &Section> {
        self.sections.iter().filter(move |s| cfg.allows(s.gate))
    }
}

/// セクション選択の設定 (`opt_flags` 相当)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SectionConfig {
    /// `-u ALL`。
    pub cpu_all: bool,
    /// `-r` (メモリ部を出す)。
    pub memory: bool,
    /// `-S` (スワップ部を出す)。
    pub swap: bool,
    /// `-r ALL`。
    pub mem_all: bool,
    /// `-F MOUNT`。
    pub fs_mount: bool,
}

impl Default for SectionConfig {
    fn default() -> Self {
        // `sadf ... -- -A` 相当 (= -u ALL / -r ALL / メモリもスワップも出す)。
        Self {
            cpu_all: true,
            memory: true,
            swap: true,
            mem_all: true,
            fs_mount: false,
        }
    }
}

impl SectionConfig {
    fn allows(&self, gate: SectionGate) -> bool {
        match gate {
            SectionGate::Always => true,
            SectionGate::CpuDef => !self.cpu_all,
            SectionGate::CpuAll => self.cpu_all,
            SectionGate::Memory => self.memory,
            SectionGate::Swap => self.swap,
            SectionGate::FsName => !self.fs_mount,
            SectionGate::FsMount => self.fs_mount,
        }
    }

    /// フィールドの有効判定。
    pub fn allows_field(&self, gate: FieldGate) -> bool {
        match gate {
            FieldGate::Always => true,
            FieldGate::MemAll => self.mem_all,
        }
    }

    /// `hdr_line` の `&` を展開する (`-r ALL` 相当なら `;` に、そうでなければ打ち切り)。
    pub fn expand_hdr_line(&self, hdr: &str) -> String {
        match hdr.find('&') {
            None => hdr.to_string(),
            Some(pos) if self.mem_all => {
                let mut s = String::with_capacity(hdr.len());
                s.push_str(&hdr[..pos]);
                s.push(';');
                s.push_str(&hdr[pos + 1..]);
                s
            }
            Some(pos) => hdr[..pos].to_string(),
        }
    }
}

// ===========================================================================
// 表の記述を短くするマクロ
// ===========================================================================

macro_rules! f {
    ($col:literal, $pp:literal, $dp:ident, $key:literal, $attr:literal, $jx:ident) => {
        Field {
            col: $col,
            pp: $pp,
            dp_fmt: Fmt::$dp,
            key: $key,
            attr: $attr,
            jx_fmt: Fmt::$jx,
            gate: FieldGate::Always,
        }
    };
    ($col:literal, $pp:literal, $dp:ident, $key:literal, $attr:literal, $jx:ident, $g:ident) => {
        Field {
            col: $col,
            pp: $pp,
            dp_fmt: Fmt::$dp,
            key: $key,
            attr: $attr,
            jx_fmt: Fmt::$jx,
            gate: FieldGate::$g,
        }
    };
}

/// レート列 1 本 (`-d`/`-p`/`-j`/`-x` すべて 2 桁小数)。
macro_rules! rate {
    ($col:literal, $pp:literal, $key:literal) => {
        f!($col, $pp, R2, $key, $key, R2)
    };
}

macro_rules! rawf {
    ($col:literal, $name:literal, $style:expr) => {
        RawField {
            col: $col,
            name: $name,
            style: $style,
        }
    };
}

/// アイテム識別子をそのまま値として出すフィールド (`-d`/`-p` には出ない)。
macro_rules! item_key {
    ($key:literal, $attr:literal, $jx:ident) => {
        f!("", "", Skip, $key, $attr, $jx)
    };
}

// ===========================================================================
// A_CPU (1)
// ===========================================================================

const CPU_DEF_FIELDS: &[Field] = &[
    item_key!("cpu", "number", ItemKeyStr),
    rate!("user", "%user", "user"),
    rate!("nice", "%nice", "nice"),
    rate!("system", "%system", "system"),
    rate!("iowait", "%iowait", "iowait"),
    rate!("steal", "%steal", "steal"),
    rate!("idle", "%idle", "idle"),
];

const CPU_DEF_RAW: &[RawField] = &[
    rawf!("user", "%user", RawStyle::Pval),
    rawf!("nice", "%nice", RawStyle::Pval),
    // %system には cpu_sys + cpu_hardirq + cpu_softirq の合算を渡す (§4.5)
    rawf!("", "%system", RawStyle::PvalSum(&["sys", "irq", "soft"])),
    rawf!("iowait", "%iowait", RawStyle::Pval),
    rawf!("steal", "%steal", RawStyle::Pval),
    rawf!("idle", "%idle", RawStyle::Pval),
];

const CPU_ALL_FIELDS: &[Field] = &[
    item_key!("cpu", "number", ItemKeyStr),
    rate!("usr", "%usr", "usr"),
    rate!("nice_excl_gnice", "%nice", "nice"),
    rate!("sys", "%sys", "sys"),
    rate!("iowait", "%iowait", "iowait"),
    rate!("steal", "%steal", "steal"),
    rate!("irq", "%irq", "irq"),
    rate!("soft", "%soft", "soft"),
    rate!("guest", "%guest", "guest"),
    rate!("gnice", "%gnice", "gnice"),
    rate!("idle", "%idle", "idle"),
];

const CPU_ALL_RAW: &[RawField] = &[
    rawf!("", "%usr", RawStyle::PvalDiff("user", "guest")),
    rawf!("", "%nice", RawStyle::PvalDiff("nice", "gnice")),
    rawf!("sys", "%sys", RawStyle::Pval),
    rawf!("iowait", "%iowait", RawStyle::Pval),
    rawf!("steal", "%steal", RawStyle::Pval),
    rawf!("irq", "%irq", RawStyle::Pval),
    rawf!("soft", "%soft", RawStyle::Pval),
    rawf!("guest", "%guest", RawStyle::Pval),
    rawf!("gnice", "%gnice", RawStyle::Pval),
    rawf!("idle", "%idle", RawStyle::Pval),
];

// ===========================================================================
// A_PCSW (2) / A_SWAP (4) / A_PAGE (5) / A_IO (6)
// ===========================================================================

const PCSW_FIELDS: &[Field] = &[
    rate!("proc", "proc/s", "proc"),
    rate!("cswch", "cswch/s", "cswch"),
];

const SWAP_FIELDS: &[Field] = &[
    rate!("pswpin", "pswpin/s", "pswpin"),
    rate!("pswpout", "pswpout/s", "pswpout"),
];

const PAGE_FIELDS: &[Field] = &[
    rate!("pgpgin", "pgpgin/s", "pgpgin"),
    rate!("pgpgout", "pgpgout/s", "pgpgout"),
    rate!("fault", "fault/s", "fault"),
    rate!("majflt", "majflt/s", "majflt"),
    rate!("pgfree", "pgfree/s", "pgfree"),
    rate!("pgscank", "pgscank/s", "pgscank"),
    rate!("pgscand", "pgscand/s", "pgscand"),
    rate!("pgsteal", "pgsteal/s", "pgsteal"),
    rate!("pgprom", "pgprom/s", "pgprom"),
    rate!("pgdem", "pgdem/s", "pgdem"),
];

const IO_FIELDS: &[Field] = &[
    rate!("tps", "tps", "tps"),
    rate!("rtps", "rtps", "rtps"),
    rate!("wtps", "wtps", "wtps"),
    rate!("dtps", "dtps", "dtps"),
    rate!("bread", "bread/s", "bread"),
    rate!("bwrtn", "bwrtn/s", "bwrtn"),
    rate!("bdscd", "bdscd/s", "bdscd"),
];

// ===========================================================================
// A_MEMORY (7) / A_HUGE (34)
// ===========================================================================

/// メモリ部。`kbshared` は `-p` のフィールド名で、`hdr_line` 側は `kbshmem`。
/// **この食い違いは本家の実装どおり** (§11.2 (a))。
const MEMORY_FIELDS: &[Field] = &[
    f!("kbmemfree", "kbmemfree", Int, "memfree", "memfree", Int),
    f!("kbavail", "kbavail", Int, "avail", "avail", Int),
    f!("kbmemused", "kbmemused", Int, "memused", "memused", Int),
    f!(
        "memused_pct",
        "%memused",
        R2,
        "memused-percent",
        "memused-percent",
        R2
    ),
    f!("kbbuffers", "kbbuffers", Int, "buffers", "buffers", Int),
    f!("kbcached", "kbcached", Int, "cached", "cached", Int),
    f!("kbcommit", "kbcommit", Int, "commit", "commit", Int),
    f!(
        "commit_pct",
        "%commit",
        R2,
        "commit-percent",
        "commit-percent",
        R2
    ),
    f!("kbactive", "kbactive", Int, "active", "active", Int),
    f!("kbinact", "kbinact", Int, "inactive", "inactive", Int),
    f!("kbdirty", "kbdirty", Int, "dirty", "dirty", Int),
    f!("kbshmem", "kbshared", Int, "shared", "shared", Int),
    f!("kbanonpg", "kbanonpg", Int, "anonpg", "anonpg", Int, MemAll),
    f!("kbslab", "kbslab", Int, "slab", "slab", Int, MemAll),
    f!("kbkstack", "kbkstack", Int, "kstack", "kstack", Int, MemAll),
    f!("kbpgtbl", "kbpgtbl", Int, "pgtbl", "pgtbl", Int, MemAll),
    f!("kbvmused", "kbvmused", Int, "vmused", "vmused", Int, MemAll),
];

/// メモリ行の raw。派生値 (`kbmemused` / `%memused` / `%commit`) は出さず、
/// 代わりに `hdr_line` に無い直書き名 `kbttlmem` が入る (§4.5)。
const MEMORY_RAW: &[RawField] = &[
    rawf!("kbmemfree", "kbmemfree", RawStyle::Int),
    rawf!("kbavail", "kbavail", RawStyle::Int),
    rawf!("kbmemtotal", "kbttlmem", RawStyle::Int),
    rawf!("kbbuffers", "kbbuffers", RawStyle::Int),
    rawf!("kbcached", "kbcached", RawStyle::Int),
    rawf!("kbcommit", "kbcommit", RawStyle::Int),
    rawf!("kbactive", "kbactive", RawStyle::Int),
    rawf!("kbinact", "kbinact", RawStyle::Int),
    rawf!("kbdirty", "kbdirty", RawStyle::Int),
    rawf!("kbshmem", "kbshmem", RawStyle::Int),
    rawf!("kbanonpg", "kbanonpg", RawStyle::Int),
    rawf!("kbslab", "kbslab", RawStyle::Int),
    rawf!("kbkstack", "kbkstack", RawStyle::Int),
    rawf!("kbpgtbl", "kbpgtbl", RawStyle::Int),
    rawf!("kbvmused", "kbvmused", RawStyle::Int),
];

const SWAP_MEM_FIELDS: &[Field] = &[
    f!("kbswpfree", "kbswpfree", Int, "swpfree", "swpfree", Int),
    f!("kbswpused", "kbswpused", Int, "swpused", "swpused", Int),
    f!(
        "swpused_pct",
        "%swpused",
        R2,
        "swpused-percent",
        "swpused-percent",
        R2
    ),
    f!("kbswpcad", "kbswpcad", Int, "swpcad", "swpcad", Int),
    f!(
        "swpcad_pct",
        "%swpcad",
        R2,
        "swpcad-percent",
        "swpcad-percent",
        R2
    ),
];

const SWAP_MEM_RAW: &[RawField] = &[
    rawf!("kbswpfree", "kbswpfree", RawStyle::Int),
    rawf!("kbswptotal", "kbttlswp", RawStyle::Int),
    rawf!("kbswpcad", "kbswpcad", RawStyle::Int),
];

const HUGE_FIELDS: &[Field] = &[
    f!("kbhugfree", "kbhugfree", Int, "hugfree", "hugfree", Int),
    f!("kbhugused", "kbhugused", Int, "hugused", "hugused", Int),
    f!(
        "hugused_pct",
        "%hugused",
        R2,
        "hugused-percent",
        "hugused-percent",
        R2
    ),
    f!("kbhugrsvd", "kbhugrsvd", Int, "hugrsvd", "hugrsvd", Int),
    f!("kbhugsurp", "kbhugsurp", Int, "hugsurp", "hugsurp", Int),
];

const HUGE_RAW: &[RawField] = &[
    rawf!("kbhugfree", "kbhugfree", RawStyle::Int),
    rawf!("kbhugtotal", "hugtotal", RawStyle::Int),
    rawf!("kbhugrsvd", "kbhugrsvd", RawStyle::Int),
    rawf!("kbhugsurp", "kbhugsurp", RawStyle::Int),
];

// ===========================================================================
// A_KTABLES (8) / A_QUEUE (9) / A_SERIAL (10)
// ===========================================================================

const KTABLES_FIELDS: &[Field] = &[
    f!("dentunusd", "dentunusd", Int, "dentunusd", "dentunusd", Int),
    f!("file_nr", "file-nr", Int, "file-nr", "file-nr", Int),
    f!("inode_nr", "inode-nr", Int, "inode-nr", "inode-nr", Int),
    f!("pty_nr", "pty-nr", Int, "pty-nr", "pty-nr", Int),
];

const KTABLES_RAW: &[RawField] = &[
    rawf!("dentunusd", "dentunusd", RawStyle::Int),
    rawf!("file_nr", "file-nr", RawStyle::Int),
    rawf!("inode_nr", "inode-nr", RawStyle::Int),
    rawf!("pty_nr", "pty-nr", RawStyle::Int),
];

const QUEUE_FIELDS: &[Field] = &[
    f!("runq_sz", "runq-sz", Int, "runq-sz", "runq-sz", Int),
    f!("plist_sz", "plist-sz", Int, "plist-sz", "plist-sz", Int),
    rate!("ldavg_1", "ldavg-1", "ldavg-1"),
    rate!("ldavg_5", "ldavg-5", "ldavg-5"),
    rate!("ldavg_15", "ldavg-15", "ldavg-15"),
    f!("blocked", "blocked", Int, "blocked", "blocked", Int),
];

/// `ldavg-*` は 100 倍された整数の生値のまま出る (§4.5)。
const QUEUE_RAW: &[RawField] = &[
    rawf!("runq_sz", "runq-sz", RawStyle::Int),
    rawf!("plist_sz", "plist-sz", RawStyle::Int),
    rawf!("ldavg_1", "ldavg-1", RawStyle::Int),
    rawf!("ldavg_5", "ldavg-5", RawStyle::Int),
    rawf!("ldavg_15", "ldavg-15", RawStyle::Int),
    rawf!("blocked", "blocked", RawStyle::Int),
];

const SERIAL_FIELDS: &[Field] = &[
    item_key!("line", "line", ItemKeyNum),
    rate!("rcvin", "rcvin/s", "rcvin"),
    rate!("xmtin", "xmtin/s", "xmtin"),
    rate!("framerr", "framerr/s", "framerr"),
    rate!("prtyerr", "prtyerr/s", "prtyerr"),
    rate!("brk", "brk/s", "brk"),
    rate!("ovrun", "ovrun/s", "ovrun"),
];

const SERIAL_RAW: &[RawField] = &[
    rawf!("rcvin", "rcvin/s", RawStyle::Pval),
    rawf!("xmtin", "xmtin/s", RawStyle::Pval),
    rawf!("framerr", "framerr/s", RawStyle::Pval),
    rawf!("prtyerr", "prtyerr/s", RawStyle::Pval),
    rawf!("brk", "brk/s", RawStyle::Pval),
    rawf!("ovrun", "ovrun/s", RawStyle::Pval),
];

// ===========================================================================
// A_DISK (11)
// ===========================================================================

/// `rd_sec` / `wr_sec` / `dc_sec` / `avgrq-sz` は JSON / XML にしか無いセクタ単位の
/// 別表現。`series` 層がまだセクタ換算列を持たないため `col` は空にしてある
/// (0 を代入せず「未対応」として出す)。
const DISK_FIELDS: &[Field] = &[
    item_key!("disk-device", "dev", ItemKeyStr),
    rate!("tps", "tps", "tps"),
    f!("", "", Skip, "rd_sec", "rd_sec", R2),
    f!("", "", Skip, "wr_sec", "wr_sec", R2),
    f!("", "", Skip, "dc_sec", "dc_sec", R2),
    rate!("read_kb_per_sec", "rkB/s", "rkB"),
    rate!("write_kb_per_sec", "wkB/s", "wkB"),
    rate!("discard_kb_per_sec", "dkB/s", "dkB"),
    f!("", "", Skip, "avgrq-sz", "avgrq-sz", R2),
    rate!("avg_request_size", "areq-sz", "areq-sz"),
    // avgqu-sz と aqu-sz は本家でも同じ式を 2 回評価しているだけ (§9.6-10)
    f!("avg_queue_size", "", Skip, "avgqu-sz", "avgqu-sz", R2),
    rate!("avg_queue_size", "aqu-sz", "aqu-sz"),
    rate!("await", "await", "await"),
    f!("util_pct", "%util", R2, "util-percent", "util-percent", R2),
];

/// 行頭に直書きの `major` / `minor` が来て、`areq-sz` は消費されない (§4.5)。
const DISK_RAW: &[RawField] = &[
    rawf!("major", "major", RawStyle::Int),
    rawf!("minor", "minor", RawStyle::Int),
    rawf!("tps", "tps", RawStyle::Pval),
    rawf!("read_kb_per_sec", "rkB/s", RawStyle::Pval),
    rawf!("write_kb_per_sec", "wkB/s", RawStyle::Pval),
    rawf!("discard_kb_per_sec", "dkB/s", RawStyle::Pval),
    rawf!("read_ticks", "rd_ticks", RawStyle::Pval),
    rawf!("write_ticks", "wr_ticks", RawStyle::Pval),
    rawf!("discard_ticks", "dc_ticks", RawStyle::Pval),
    rawf!("util_pct", "tot_ticks", RawStyle::Pval),
    rawf!("avg_queue_size", "aqu-sz", RawStyle::Pval),
];

// ===========================================================================
// A_NET_DEV (12) / A_NET_EDEV (13)
// ===========================================================================

const NET_DEV_FIELDS: &[Field] = &[
    item_key!("iface", "iface", ItemKeyStr),
    rate!("rxpck_per_sec", "rxpck/s", "rxpck"),
    rate!("txpck_per_sec", "txpck/s", "txpck"),
    rate!("rx_bytes_per_sec", "rxkB/s", "rxkB"),
    rate!("tx_bytes_per_sec", "txkB/s", "txkB"),
    rate!("rxcmp_per_sec", "rxcmp/s", "rxcmp"),
    rate!("txcmp_per_sec", "txcmp/s", "txcmp"),
    rate!("rxmcst_per_sec", "rxmcst/s", "rxmcst"),
    f!(
        "ifutil_pct",
        "%ifutil",
        R2,
        "ifutil-percent",
        "ifutil-percent",
        R2
    ),
];

/// `%ifutil` の代わりに直書きの `speed` / `duplex` が付く (§4.5)。
const NET_DEV_RAW: &[RawField] = &[
    rawf!("rxpck_per_sec", "rxpck/s", RawStyle::Pval),
    rawf!("txpck_per_sec", "txpck/s", RawStyle::Pval),
    rawf!("rx_bytes_per_sec", "rxkB/s", RawStyle::Pval),
    rawf!("tx_bytes_per_sec", "txkB/s", RawStyle::Pval),
    rawf!("rxcmp_per_sec", "rxcmp/s", RawStyle::Pval),
    rawf!("txcmp_per_sec", "txcmp/s", RawStyle::Pval),
    rawf!("rxmcst_per_sec", "rxmcst/s", RawStyle::Pval),
    rawf!("speed", "speed", RawStyle::Int),
    rawf!("duplex", "duplex", RawStyle::Int),
];

const NET_EDEV_FIELDS: &[Field] = &[
    item_key!("iface", "iface", ItemKeyStr),
    rate!("rxerr_per_sec", "rxerr/s", "rxerr"),
    rate!("txerr_per_sec", "txerr/s", "txerr"),
    rate!("coll_per_sec", "coll/s", "coll"),
    rate!("rxdrop_per_sec", "rxdrop/s", "rxdrop"),
    rate!("txdrop_per_sec", "txdrop/s", "txdrop"),
    rate!("txcarr_per_sec", "txcarr/s", "txcarr"),
    rate!("rxfram_per_sec", "rxfram/s", "rxfram"),
    rate!("rxfifo_per_sec", "rxfifo/s", "rxfifo"),
    rate!("txfifo_per_sec", "txfifo/s", "txfifo"),
];

// ===========================================================================
// A_NET_* (アイテムなしのカウンタ系)
// ===========================================================================

const NET_NFS_FIELDS: &[Field] = &[
    rate!("call_per_sec", "call/s", "call"),
    rate!("retrans_per_sec", "retrans/s", "retrans"),
    rate!("read_per_sec", "read/s", "read"),
    rate!("write_per_sec", "write/s", "write"),
    rate!("access_per_sec", "access/s", "access"),
    rate!("getatt_per_sec", "getatt/s", "getatt"),
];

const NET_NFSD_FIELDS: &[Field] = &[
    rate!("scall_per_sec", "scall/s", "scall"),
    rate!("badcall_per_sec", "badcall/s", "badcall"),
    rate!("packet_per_sec", "packet/s", "packet"),
    rate!("udp_per_sec", "udp/s", "udp"),
    rate!("tcp_per_sec", "tcp/s", "tcp"),
    rate!("hit_per_sec", "hit/s", "hit"),
    rate!("miss_per_sec", "miss/s", "miss"),
    rate!("sread_per_sec", "sread/s", "sread"),
    rate!("swrite_per_sec", "swrite/s", "swrite"),
    rate!("saccess_per_sec", "saccess/s", "saccess"),
    rate!("sgetatt_per_sec", "sgetatt/s", "sgetatt"),
];

const NET_SOCK_FIELDS: &[Field] = &[
    f!("totsck", "totsck", Int, "totsck", "totsck", Int),
    f!("tcpsck", "tcpsck", Int, "tcpsck", "tcpsck", Int),
    f!("udpsck", "udpsck", Int, "udpsck", "udpsck", Int),
    f!("rawsck", "rawsck", Int, "rawsck", "rawsck", Int),
    f!("ip_frag", "ip-frag", Int, "ip-frag", "ip-frag", Int),
    f!("tcp_tw", "tcp-tw", Int, "tcp-tw", "tcp-tw", Int),
];

const NET_SOCK_RAW: &[RawField] = &[
    rawf!("totsck", "totsck", RawStyle::Int),
    rawf!("tcpsck", "tcpsck", RawStyle::Int),
    rawf!("udpsck", "udpsck", RawStyle::Int),
    rawf!("rawsck", "rawsck", RawStyle::Int),
    rawf!("ip_frag", "ip-frag", RawStyle::Int),
    rawf!("tcp_tw", "tcp-tw", RawStyle::Int),
];

const NET_IP_FIELDS: &[Field] = &[
    rate!("irec_per_sec", "irec/s", "irec"),
    rate!("fwddgm_per_sec", "fwddgm/s", "fwddgm"),
    rate!("idel_per_sec", "idel/s", "idel"),
    rate!("orq_per_sec", "orq/s", "orq"),
    rate!("asmrq_per_sec", "asmrq/s", "asmrq"),
    rate!("asmok_per_sec", "asmok/s", "asmok"),
    rate!("fragok_per_sec", "fragok/s", "fragok"),
    rate!("fragcrt_per_sec", "fragcrt/s", "fragcrt"),
];

const NET_EIP_FIELDS: &[Field] = &[
    rate!("ihdrerr_per_sec", "ihdrerr/s", "ihdrerr"),
    rate!("iadrerr_per_sec", "iadrerr/s", "iadrerr"),
    rate!("iukwnpr_per_sec", "iukwnpr/s", "iukwnpr"),
    rate!("idisc_per_sec", "idisc/s", "idisc"),
    rate!("odisc_per_sec", "odisc/s", "odisc"),
    rate!("onort_per_sec", "onort/s", "onort"),
    rate!("asmf_per_sec", "asmf/s", "asmf"),
    rate!("fragf_per_sec", "fragf/s", "fragf"),
];

const NET_ICMP_FIELDS: &[Field] = &[
    rate!("imsg_per_sec", "imsg/s", "imsg"),
    rate!("omsg_per_sec", "omsg/s", "omsg"),
    rate!("iech_per_sec", "iech/s", "iech"),
    rate!("iechr_per_sec", "iechr/s", "iechr"),
    rate!("oech_per_sec", "oech/s", "oech"),
    rate!("oechr_per_sec", "oechr/s", "oechr"),
    rate!("itm_per_sec", "itm/s", "itm"),
    rate!("itmr_per_sec", "itmr/s", "itmr"),
    rate!("otm_per_sec", "otm/s", "otm"),
    rate!("otmr_per_sec", "otmr/s", "otmr"),
    rate!("iadrmk_per_sec", "iadrmk/s", "iadrmk"),
    rate!("iadrmkr_per_sec", "iadrmkr/s", "iadrmkr"),
    rate!("oadrmk_per_sec", "oadrmk/s", "oadrmk"),
    rate!("oadrmkr_per_sec", "oadrmkr/s", "oadrmkr"),
];

const NET_EICMP_FIELDS: &[Field] = &[
    rate!("ierr_per_sec", "ierr/s", "ierr"),
    rate!("oerr_per_sec", "oerr/s", "oerr"),
    rate!("idstunr_per_sec", "idstunr/s", "idstunr"),
    rate!("odstunr_per_sec", "odstunr/s", "odstunr"),
    rate!("itmex_per_sec", "itmex/s", "itmex"),
    rate!("otmex_per_sec", "otmex/s", "otmex"),
    rate!("iparmpb_per_sec", "iparmpb/s", "iparmpb"),
    rate!("oparmpb_per_sec", "oparmpb/s", "oparmpb"),
    rate!("isrcq_per_sec", "isrcq/s", "isrcq"),
    rate!("osrcq_per_sec", "osrcq/s", "osrcq"),
    rate!("iredir_per_sec", "iredir/s", "iredir"),
    rate!("oredir_per_sec", "oredir/s", "oredir"),
];

const NET_TCP_FIELDS: &[Field] = &[
    rate!("active_per_sec", "active/s", "active"),
    rate!("passive_per_sec", "passive/s", "passive"),
    rate!("iseg_per_sec", "iseg/s", "iseg"),
    rate!("oseg_per_sec", "oseg/s", "oseg"),
];

const NET_ETCP_FIELDS: &[Field] = &[
    rate!("atmptf_per_sec", "atmptf/s", "atmptf"),
    rate!("estres_per_sec", "estres/s", "estres"),
    rate!("retrseg_per_sec", "retrseg/s", "retrseg"),
    rate!("isegerr_per_sec", "isegerr/s", "isegerr"),
    rate!("orsts_per_sec", "orsts/s", "orsts"),
];

const NET_UDP_FIELDS: &[Field] = &[
    rate!("idgm_per_sec", "idgm/s", "idgm"),
    rate!("odgm_per_sec", "odgm/s", "odgm"),
    rate!("noport_per_sec", "noport/s", "noport"),
    rate!("idgmerr_per_sec", "idgmerr/s", "idgmerr"),
];

const NET_SOCK6_FIELDS: &[Field] = &[
    f!("tcp6sck", "tcp6sck", Int, "tcp6sck", "tcp6sck", Int),
    f!("udp6sck", "udp6sck", Int, "udp6sck", "udp6sck", Int),
    f!("raw6sck", "raw6sck", Int, "raw6sck", "raw6sck", Int),
    f!("ip6_frag", "ip6-frag", Int, "ip6-frag", "ip6-frag", Int),
];

const NET_SOCK6_RAW: &[RawField] = &[
    rawf!("tcp6sck", "tcp6sck", RawStyle::Int),
    rawf!("udp6sck", "udp6sck", RawStyle::Int),
    rawf!("raw6sck", "raw6sck", RawStyle::Int),
    rawf!("ip6_frag", "ip6-frag", RawStyle::Int),
];

const NET_IP6_FIELDS: &[Field] = &[
    rate!("irec6_per_sec", "irec6/s", "irec6"),
    rate!("fwddgm6_per_sec", "fwddgm6/s", "fwddgm6"),
    rate!("idel6_per_sec", "idel6/s", "idel6"),
    rate!("orq6_per_sec", "orq6/s", "orq6"),
    rate!("asmrq6_per_sec", "asmrq6/s", "asmrq6"),
    rate!("asmok6_per_sec", "asmok6/s", "asmok6"),
    rate!("imcpck6_per_sec", "imcpck6/s", "imcpck6"),
    rate!("omcpck6_per_sec", "omcpck6/s", "omcpck6"),
    rate!("fragok6_per_sec", "fragok6/s", "fragok6"),
    rate!("fragcr6_per_sec", "fragcr6/s", "fragcr6"),
];

const NET_EIP6_FIELDS: &[Field] = &[
    rate!("ihdrer6_per_sec", "ihdrer6/s", "ihdrer6"),
    rate!("iadrer6_per_sec", "iadrer6/s", "iadrer6"),
    rate!("iukwnp6_per_sec", "iukwnp6/s", "iukwnp6"),
    rate!("i2big6_per_sec", "i2big6/s", "i2big6"),
    rate!("idisc6_per_sec", "idisc6/s", "idisc6"),
    rate!("odisc6_per_sec", "odisc6/s", "odisc6"),
    rate!("inort6_per_sec", "inort6/s", "inort6"),
    rate!("onort6_per_sec", "onort6/s", "onort6"),
    rate!("asmf6_per_sec", "asmf6/s", "asmf6"),
    rate!("fragf6_per_sec", "fragf6/s", "fragf6"),
    rate!("itrpck6_per_sec", "itrpck6/s", "itrpck6"),
];

/// `oech6/s` は存在しない (§11 の注記)。`iechr6` の次は `oechr6`。
const NET_ICMP6_FIELDS: &[Field] = &[
    rate!("imsg6_per_sec", "imsg6/s", "imsg6"),
    rate!("omsg6_per_sec", "omsg6/s", "omsg6"),
    rate!("iech6_per_sec", "iech6/s", "iech6"),
    rate!("iechr6_per_sec", "iechr6/s", "iechr6"),
    rate!("oechr6_per_sec", "oechr6/s", "oechr6"),
    rate!("igmbq6_per_sec", "igmbq6/s", "igmbq6"),
    rate!("igmbr6_per_sec", "igmbr6/s", "igmbr6"),
    rate!("ogmbr6_per_sec", "ogmbr6/s", "ogmbr6"),
    rate!("igmbrd6_per_sec", "igmbrd6/s", "igmbrd6"),
    rate!("ogmbrd6_per_sec", "ogmbrd6/s", "ogmbrd6"),
    rate!("irtsol6_per_sec", "irtsol6/s", "irtsol6"),
    rate!("ortsol6_per_sec", "ortsol6/s", "ortsol6"),
    rate!("irtad6_per_sec", "irtad6/s", "irtad6"),
    rate!("inbsol6_per_sec", "inbsol6/s", "inbsol6"),
    rate!("onbsol6_per_sec", "onbsol6/s", "onbsol6"),
    rate!("inbad6_per_sec", "inbad6/s", "inbad6"),
    rate!("onbad6_per_sec", "onbad6/s", "onbad6"),
];

const NET_EICMP6_FIELDS: &[Field] = &[
    rate!("ierr6_per_sec", "ierr6/s", "ierr6"),
    rate!("idtunr6_per_sec", "idtunr6/s", "idtunr6"),
    rate!("odtunr6_per_sec", "odtunr6/s", "odtunr6"),
    rate!("itmex6_per_sec", "itmex6/s", "itmex6"),
    rate!("otmex6_per_sec", "otmex6/s", "otmex6"),
    rate!("iprmpb6_per_sec", "iprmpb6/s", "iprmpb6"),
    rate!("oprmpb6_per_sec", "oprmpb6/s", "oprmpb6"),
    rate!("iredir6_per_sec", "iredir6/s", "iredir6"),
    rate!("oredir6_per_sec", "oredir6/s", "oredir6"),
    rate!("ipck2b6_per_sec", "ipck2b6/s", "ipck2b6"),
    rate!("opck2b6_per_sec", "opck2b6/s", "opck2b6"),
];

const NET_UDP6_FIELDS: &[Field] = &[
    rate!("idgm6_per_sec", "idgm6/s", "idgm6"),
    rate!("odgm6_per_sec", "odgm6/s", "odgm6"),
    rate!("noport6_per_sec", "noport6/s", "noport6"),
    rate!("idgmer6_per_sec", "idgmer6/s", "idgmer6"),
];

const NET_FC_FIELDS: &[Field] = &[
    item_key!("fchost", "name", ItemKeyStr),
    rate!("fch_rxf_per_sec", "fch_rxf/s", "fch_rxf"),
    rate!("fch_txf_per_sec", "fch_txf/s", "fch_txf"),
    rate!("fch_rxw_per_sec", "fch_rxw/s", "fch_rxw"),
    rate!("fch_txw_per_sec", "fch_txw/s", "fch_txw"),
];

const NET_SOFT_FIELDS: &[Field] = &[
    item_key!("cpu", "cpu", ItemKeyStr),
    rate!("total_per_sec", "total/s", "total"),
    rate!("dropd_per_sec", "dropd/s", "dropd"),
    rate!("squeezd_per_sec", "squeezd/s", "squeezd"),
    rate!("rx_rps_per_sec", "rx_rps/s", "rx_rps"),
    rate!("flw_lim_per_sec", "flw_lim/s", "flw_lim"),
    f!("blg_len", "blg_len", Int, "blg_len", "blg_len", Int),
];

const NET_SOFT_RAW: &[RawField] = &[
    rawf!("total_per_sec", "total/s", RawStyle::Pval),
    rawf!("dropd_per_sec", "dropd/s", RawStyle::Pval),
    rawf!("squeezd_per_sec", "squeezd/s", RawStyle::Pval),
    rawf!("rx_rps_per_sec", "rx_rps/s", RawStyle::Pval),
    rawf!("flw_lim_per_sec", "flw_lim/s", RawStyle::Pval),
    rawf!("blg_len", "blg_len", RawStyle::Int),
];

// ===========================================================================
// A_PWR_* (30〜36, 43)
// ===========================================================================

const PWR_CPU_FIELDS: &[Field] = &[
    item_key!("number", "number", ItemKeyStr),
    rate!("mhz", "MHz", "frequency"),
];

const PWR_CPU_RAW: &[RawField] = &[rawf!("mhz", "MHz", RawStyle::Int)];

/// `-d`/`-p` は `DEVICE;rpm;drpm` の順、`-j`/`-x` は `number,rpm,drpm,device` の順。
/// 並びが違うので [`Section::jx_order`] で切り替える。
const PWR_FAN_FIELDS: &[Field] = &[
    item_key!("number", "number", ItemKeyNum),
    f!("device", "DEVICE", Str, "device", "device", Str),
    f!("rpm", "rpm", R2, "rpm", "rpm", Int),
    f!("rpm_delta", "drpm", R2, "drpm", "drpm", Int),
];
const PWR_FAN_JX_ORDER: &[usize] = &[0, 2, 3, 1];

const PWR_FAN_RAW: &[RawField] = &[
    rawf!("device", "DEVICE", RawStyle::Text),
    rawf!("rpm", "rpm", RawStyle::Sensor),
    rawf!("rpm_min", "rpm_min", RawStyle::Sensor),
];

const PWR_TEMP_FIELDS: &[Field] = &[
    item_key!("number", "number", ItemKeyNum),
    f!("device", "DEVICE", Str, "device", "device", Str),
    rate!("temp_celsius", "degC", "degC"),
    f!("temp_pct", "%temp", R2, "percent-temp", "percent-temp", R2),
];
const PWR_TEMP_JX_ORDER: &[usize] = &[0, 2, 3, 1];

const PWR_TEMP_RAW: &[RawField] = &[
    rawf!("device", "DEVICE", RawStyle::Text),
    rawf!("temp_celsius", "degC", RawStyle::Sensor),
    rawf!("temp_min", "temp_min", RawStyle::Sensor),
    rawf!("temp_max", "temp_max", RawStyle::Sensor),
];

const PWR_IN_FIELDS: &[Field] = &[
    item_key!("number", "number", ItemKeyNum),
    f!("device", "DEVICE", Str, "device", "device", Str),
    rate!("volts", "inV", "inV"),
    f!("in_pct", "%in", R2, "percent-in", "percent-in", R2),
];
const PWR_IN_JX_ORDER: &[usize] = &[0, 2, 3, 1];

const PWR_IN_RAW: &[RawField] = &[
    rawf!("device", "DEVICE", RawStyle::Text),
    rawf!("volts", "inV", RawStyle::Sensor),
    rawf!("volts_min", "in_min", RawStyle::Sensor),
    rawf!("volts_max", "in_max", RawStyle::Sensor),
];

const PWR_FREQ_FIELDS: &[Field] = &[
    item_key!("number", "number", ItemKeyStr),
    rate!("weighted_mhz", "wghMHz", "weighted-frequency"),
];

/// `hdr_line` (`CPU;wghMHz`) を使わず直書きの `freq` / `tminst` を
/// 周波数ステップぶん繰り返す (§4.5)。繰り返しは専用経路で行う。
const PWR_FREQ_RAW: &[RawField] = &[
    rawf!("freq_khz", "freq", RawStyle::Int),
    rawf!("time_in_state", "tminst", RawStyle::Pval),
];

/// `hdr_line` は `manufact;product;BUS;idvendor;idprod;maxpower` だが、
/// **実データの順は `BUS`→`idvendor`→`idprod`→`maxpower`→`manufact`→`product`**。
/// 本家の既知バグ (§11.2 (b)) なのでそのまま再現する。
const PWR_USB_FIELDS: &[Field] = &[
    item_key!("bus_number", "bus_number", ItemKeyNum),
    f!("vendor_id", "idvendor", Hex, "idvendor", "idvendor", Hex),
    f!("product_id", "idprod", Hex, "idprod", "idprod", Hex),
    f!("max_power", "maxpower", Int, "maxpower", "maxpower", Int),
    f!("manufacturer", "manufact", Str, "manufact", "manufact", Str),
    f!("product", "product", Str, "product", "product", Str),
];

const PWR_USB_RAW: &[RawField] = &[
    rawf!("manufacturer", "manufact", RawStyle::QuotedText),
    rawf!("product", "product", RawStyle::QuotedText),
    rawf!("bus", "BUS", RawStyle::Int),
    rawf!("vendor_id", "idvendor", RawStyle::Hex),
    rawf!("product_id", "idprod", RawStyle::Hex),
    rawf!("max_power", "maxpower", RawStyle::Int),
];

const PWR_BAT_FIELDS: &[Field] = &[
    item_key!("number", "number", ItemKeyNum),
    f!(
        "capacity_pct",
        "%cap",
        Int,
        "percent-capacity",
        "percent-capacity",
        Int
    ),
    rate!("capacity_per_min", "cap/min", "variation"),
    f!("status", "status", Str, "status", "status", Str),
];

const PWR_BAT_RAW: &[RawField] = &[
    rawf!("capacity_pct", "%cap", RawStyle::Pval),
    rawf!("status", "status", RawStyle::Int),
];

// ===========================================================================
// A_FS (37)
// ===========================================================================

macro_rules! fs_fields {
    ($item_key:literal, $item_attr:literal) => {
        &[
            item_key!($item_key, $item_attr, ItemKeyStr),
            f!("fs_free", "MBfsfree", R0, "MBfsfree", "MBfsfree", R0),
            f!("fs_used", "MBfsused", R0, "MBfsused", "MBfsused", R0),
            f!(
                "fs_used_pct",
                "%fsused",
                R2,
                "%fsused",
                "fsused-percent",
                R2
            ),
            f!(
                "fs_used_pct_unpriv",
                "%ufsused",
                R2,
                "%ufsused",
                "ufsused-percent",
                R2
            ),
            f!("inodes_free", "Ifree", Int, "Ifree", "Ifree", Int),
            f!("inodes_used", "Iused", Int, "Iused", "Iused", Int),
            f!(
                "inodes_used_pct",
                "%Iused",
                R2,
                "%Iused",
                "Iused-percent",
                R2
            ),
        ]
    };
}

const FS_NAME_FIELDS: &[Field] = fs_fields!("filesystem", "fsname");
const FS_MOUNT_FIELDS: &[Field] = fs_fields!("mountpoint", "mountp");

/// `MBfs*` / `%fsused` / `%ufsused` は出さず、直書きの `f_bfree` / `f_blocks` /
/// `f_bavail` / `f_files` が入る (§4.5)。
const FS_RAW: &[RawField] = &[
    rawf!("fs_free", "f_bfree", RawStyle::Int),
    rawf!("fs_total", "f_blocks", RawStyle::Int),
    rawf!("fs_available", "f_bavail", RawStyle::Int),
    rawf!("inodes_free", "Ifree", RawStyle::Int),
    rawf!("inodes_total", "f_files", RawStyle::Int),
];

// ===========================================================================
// A_PSI_* (40〜42)
// ===========================================================================

const PSI_CPU_FIELDS: &[Field] = &[
    rate!("scpu_10", "%scpu-10", "some_avg10"),
    rate!("scpu_60", "%scpu-60", "some_avg60"),
    rate!("scpu_300", "%scpu-300", "some_avg300"),
    rate!("scpu", "%scpu", "some_avg"),
];

const PSI_CPU_RAW: &[RawField] = &[
    rawf!("scpu_10", "%scpu-10", RawStyle::Int),
    rawf!("scpu_60", "%scpu-60", RawStyle::Int),
    rawf!("scpu_300", "%scpu-300", RawStyle::Int),
    rawf!("scpu", "%scpu", RawStyle::Pval),
];

const PSI_IO_FIELDS: &[Field] = &[
    rate!("sio_10", "%sio-10", "some_avg10"),
    rate!("sio_60", "%sio-60", "some_avg60"),
    rate!("sio_300", "%sio-300", "some_avg300"),
    rate!("sio", "%sio", "some_avg"),
    rate!("fio_10", "%fio-10", "full_avg10"),
    rate!("fio_60", "%fio-60", "full_avg60"),
    rate!("fio_300", "%fio-300", "full_avg300"),
    rate!("fio", "%fio", "full_avg"),
];

const PSI_IO_RAW: &[RawField] = &[
    rawf!("sio_10", "%sio-10", RawStyle::Int),
    rawf!("sio_60", "%sio-60", RawStyle::Int),
    rawf!("sio_300", "%sio-300", RawStyle::Int),
    rawf!("sio", "%sio", RawStyle::Pval),
    rawf!("fio_10", "%fio-10", RawStyle::Int),
    rawf!("fio_60", "%fio-60", RawStyle::Int),
    rawf!("fio_300", "%fio-300", RawStyle::Int),
    rawf!("fio", "%fio", RawStyle::Pval),
];

const PSI_MEM_FIELDS: &[Field] = &[
    rate!("smem_10", "%smem-10", "some_avg10"),
    rate!("smem_60", "%smem-60", "some_avg60"),
    rate!("smem_300", "%smem-300", "some_avg300"),
    rate!("smem", "%smem", "some_avg"),
    rate!("fmem_10", "%fmem-10", "full_avg10"),
    rate!("fmem_60", "%fmem-60", "full_avg60"),
    rate!("fmem_300", "%fmem-300", "full_avg300"),
    rate!("fmem", "%fmem", "full_avg"),
];

const PSI_MEM_RAW: &[RawField] = &[
    rawf!("smem_10", "%smem-10", RawStyle::Int),
    rawf!("smem_60", "%smem-60", RawStyle::Int),
    rawf!("smem_300", "%smem-300", RawStyle::Int),
    rawf!("smem", "%smem", RawStyle::Pval),
    rawf!("fmem_10", "%fmem-10", RawStyle::Int),
    rawf!("fmem_60", "%fmem-60", RawStyle::Int),
    rawf!("fmem_300", "%fmem-300", RawStyle::Int),
    rawf!("fmem", "%fmem", RawStyle::Pval),
];

// ===========================================================================
// activity 表
// ===========================================================================

/// セクション 1 つだけの activity を短く書くためのヘルパ。
macro_rules! one_section {
    ($hdr:literal, $fields:expr, $raw:expr) => {
        &[Section {
            gate: SectionGate::Always,
            item_col: "",
            hdr_line: $hdr,
            fields: $fields,
            jx_order: &[],
            raw: $raw,
        }]
    };
    ($hdr:literal, $fields:expr, $raw:expr, $order:expr) => {
        &[Section {
            gate: SectionGate::Always,
            item_col: "",
            hdr_line: $hdr,
            fields: $fields,
            jx_order: $order,
            raw: $raw,
        }]
    };
}

/// `act[]` の並び順 = XML / JSON の出力順 (§9.4)。
///
/// `-d` / `-p` / `-r` は ID 昇順 (= `sar` のレポート順) なので、
/// 表の順序をそのまま使うのは `-j` / `-x` だけ。
pub const SPECS: &[ActivitySpec] = &[
    ActivitySpec {
        id: ActivityId::CPU,
        name: "A_CPU",
        desc: "CPU utilization",
        json_key: "cpu-load",
        xml_elem: "cpu-load",
        xml_child: "cpu",
        xml_wrapper_attrs: " per=\"second\"",
        group: Group::None,
        closes_group: false,
        item: ItemKind::Cpu,
        shape: Shape::Array,
        merge_sections: false,
        sections: &[
            Section {
                gate: SectionGate::CpuDef,
                item_col: "",
                hdr_line: "CPU;%user;%nice;%system;%iowait;%steal;%idle",
                fields: CPU_DEF_FIELDS,
                jx_order: &[],
                raw: RawSpec::Fields(CPU_DEF_RAW),
            },
            Section {
                gate: SectionGate::CpuAll,
                item_col: "",
                hdr_line: "CPU;%usr;%nice;%sys;%iowait;%steal;%irq;%soft;%guest;%gnice;%idle",
                fields: CPU_ALL_FIELDS,
                jx_order: &[],
                raw: RawSpec::Fields(CPU_ALL_RAW),
            },
        ],
    },
    ActivitySpec {
        id: ActivityId::PCSW,
        name: "A_PCSW",
        desc: "Task creation and switching activity",
        json_key: "process-and-context-switch",
        xml_elem: "process-and-context-switch",
        xml_child: "",
        xml_wrapper_attrs: " per=\"second\"",
        group: Group::None,
        closes_group: false,
        item: ItemKind::None,
        shape: Shape::Object,
        merge_sections: false,
        sections: one_section!("proc/s;cswch/s", PCSW_FIELDS, RawSpec::AllPval),
    },
    ActivitySpec {
        id: ActivityId::IRQ,
        name: "A_IRQ",
        desc: "Interrupts statistics",
        json_key: "interrupts",
        xml_elem: "interrupts",
        xml_child: "irq",
        xml_wrapper_attrs: "",
        group: Group::None,
        closes_group: false,
        item: ItemKind::Irq,
        shape: Shape::Custom,
        merge_sections: false,
        sections: one_section!("INTR;CPU*", &[], RawSpec::Fields(&[])),
    },
    ActivitySpec {
        id: ActivityId::SWAP,
        name: "A_SWAP",
        desc: "Swap activity",
        json_key: "swap-pages",
        xml_elem: "swap-pages",
        xml_child: "",
        xml_wrapper_attrs: " per=\"second\"",
        group: Group::None,
        closes_group: false,
        item: ItemKind::None,
        shape: Shape::Object,
        merge_sections: false,
        sections: one_section!("pswpin/s;pswpout/s", SWAP_FIELDS, RawSpec::AllPval),
    },
    ActivitySpec {
        id: ActivityId::PAGE,
        name: "A_PAGE",
        desc: "Paging activity",
        json_key: "paging",
        xml_elem: "paging",
        xml_child: "",
        xml_wrapper_attrs: " per=\"second\"",
        group: Group::None,
        closes_group: false,
        item: ItemKind::None,
        shape: Shape::Object,
        merge_sections: false,
        sections: one_section!(
            "pgpgin/s;pgpgout/s;fault/s;majflt/s;pgfree/s;pgscank/s;pgscand/s;pgsteal/s;pgprom/s;pgdem/s",
            PAGE_FIELDS,
            RawSpec::AllPval
        ),
    },
    ActivitySpec {
        id: ActivityId::IO,
        name: "A_IO",
        desc: "I/O and transfer rate statistics",
        json_key: "io",
        xml_elem: "io",
        xml_child: "",
        xml_wrapper_attrs: " per=\"second\"",
        group: Group::None,
        closes_group: false,
        item: ItemKind::None,
        // JSON / XML は io-reads / io-writes / io-discard に入れ子になる
        shape: Shape::Custom,
        merge_sections: false,
        sections: one_section!(
            "tps;rtps;wtps;dtps;bread/s;bwrtn/s;bdscd/s",
            IO_FIELDS,
            RawSpec::AllPval
        ),
    },
    ActivitySpec {
        id: ActivityId::MEMORY,
        name: "A_MEMORY",
        desc: "Memory and/or swap utilization",
        json_key: "memory",
        xml_elem: "memory",
        xml_child: "",
        xml_wrapper_attrs: " unit=\"kB\"",
        group: Group::None,
        closes_group: false,
        item: ItemKind::None,
        shape: Shape::TextChildren,
        // メモリ部とスワップ部が 1 つのオブジェクトに連結される (§9.6-15)
        merge_sections: true,
        sections: &[
            Section {
                gate: SectionGate::Memory,
                item_col: "",
                hdr_line: "kbmemfree;kbavail;kbmemused;%memused;kbbuffers;kbcached;kbcommit;%commit;kbactive;kbinact;kbdirty;kbshmem&kbanonpg;kbslab;kbkstack;kbpgtbl;kbvmused",
                fields: MEMORY_FIELDS,
                jx_order: &[],
                raw: RawSpec::Fields(MEMORY_RAW),
            },
            Section {
                gate: SectionGate::Swap,
                item_col: "",
                hdr_line: "kbswpfree;kbswpused;%swpused;kbswpcad;%swpcad",
                fields: SWAP_MEM_FIELDS,
                jx_order: &[],
                raw: RawSpec::Fields(SWAP_MEM_RAW),
            },
        ],
    },
    ActivitySpec {
        id: ActivityId::HUGE,
        name: "A_HUGE",
        desc: "Huge pages utilization",
        json_key: "hugepages",
        xml_elem: "hugepages",
        xml_child: "",
        xml_wrapper_attrs: " unit=\"kB\"",
        group: Group::None,
        closes_group: false,
        item: ItemKind::None,
        shape: Shape::TextChildren,
        merge_sections: false,
        sections: one_section!(
            "kbhugfree;kbhugused;%hugused;kbhugrsvd;kbhugsurp",
            HUGE_FIELDS,
            RawSpec::Fields(HUGE_RAW)
        ),
    },
    ActivitySpec {
        id: ActivityId::KTABLES,
        name: "A_KTABLES",
        desc: "Kernel tables statistics",
        json_key: "kernel",
        xml_elem: "kernel",
        xml_child: "",
        xml_wrapper_attrs: "",
        group: Group::None,
        closes_group: false,
        item: ItemKind::None,
        shape: Shape::Object,
        merge_sections: false,
        sections: one_section!(
            "dentunusd;file-nr;inode-nr;pty-nr",
            KTABLES_FIELDS,
            RawSpec::Fields(KTABLES_RAW)
        ),
    },
    ActivitySpec {
        id: ActivityId::QUEUE,
        name: "A_QUEUE",
        desc: "Queue length and load average statistics",
        json_key: "queue",
        xml_elem: "queue",
        xml_child: "",
        xml_wrapper_attrs: "",
        group: Group::None,
        closes_group: false,
        item: ItemKind::None,
        shape: Shape::Object,
        merge_sections: false,
        sections: one_section!(
            "runq-sz;plist-sz;ldavg-1;ldavg-5;ldavg-15;blocked",
            QUEUE_FIELDS,
            RawSpec::Fields(QUEUE_RAW)
        ),
    },
    ActivitySpec {
        id: ActivityId::SERIAL,
        name: "A_SERIAL",
        desc: "TTY devices statistics",
        json_key: "serial",
        xml_elem: "serial",
        xml_child: "tty",
        xml_wrapper_attrs: " per=\"second\"",
        group: Group::None,
        closes_group: false,
        item: ItemKind::Column {
            col: "line",
            prefix: "ttyS",
        },
        shape: Shape::Array,
        merge_sections: false,
        sections: one_section!(
            "TTY;rcvin/s;xmtin/s;framerr/s;prtyerr/s;brk/s;ovrun/s",
            SERIAL_FIELDS,
            RawSpec::Fields(SERIAL_RAW)
        ),
    },
    ActivitySpec {
        id: ActivityId::DISK,
        name: "A_DISK",
        desc: "Block devices statistics",
        json_key: "disk",
        xml_elem: "disk",
        xml_child: "disk-device",
        xml_wrapper_attrs: " per=\"second\"",
        group: Group::None,
        closes_group: false,
        item: ItemKind::Disk,
        shape: Shape::Array,
        merge_sections: false,
        sections: one_section!(
            "DEV;tps;rkB/s;wkB/s;dkB/s;areq-sz;aqu-sz;await;%util",
            DISK_FIELDS,
            RawSpec::Fields(DISK_RAW)
        ),
    },
    // ---- <network> グループ ----
    ActivitySpec {
        id: ActivityId::NET_DEV,
        name: "A_NET_DEV",
        desc: "Network interfaces statistics",
        json_key: "net-dev",
        xml_elem: "net-dev",
        xml_child: "net-dev",
        xml_wrapper_attrs: "",
        group: Group::Network,
        closes_group: false,
        item: ItemKind::Name,
        shape: Shape::Array,
        merge_sections: false,
        sections: one_section!(
            "IFACE;rxpck/s;txpck/s;rxkB/s;txkB/s;rxcmp/s;txcmp/s;rxmcst/s;%ifutil",
            NET_DEV_FIELDS,
            RawSpec::Fields(NET_DEV_RAW)
        ),
    },
    ActivitySpec {
        id: ActivityId::NET_EDEV,
        name: "A_NET_EDEV",
        desc: "Network interfaces errors statistics",
        json_key: "net-edev",
        xml_elem: "net-edev",
        xml_child: "net-edev",
        xml_wrapper_attrs: "",
        group: Group::Network,
        closes_group: false,
        item: ItemKind::Name,
        shape: Shape::Array,
        merge_sections: false,
        sections: one_section!(
            "IFACE;rxerr/s;txerr/s;coll/s;rxdrop/s;txdrop/s;txcarr/s;rxfram/s;rxfifo/s;txfifo/s",
            NET_EDEV_FIELDS,
            RawSpec::AllPval
        ),
    },
    ActivitySpec {
        id: ActivityId::NET_NFS,
        name: "A_NET_NFS",
        desc: "NFS client statistics",
        json_key: "net-nfs",
        xml_elem: "net-nfs",
        xml_child: "",
        xml_wrapper_attrs: "",
        group: Group::Network,
        closes_group: false,
        item: ItemKind::None,
        shape: Shape::Object,
        merge_sections: false,
        sections: one_section!(
            "call/s;retrans/s;read/s;write/s;access/s;getatt/s",
            NET_NFS_FIELDS,
            RawSpec::AllPval
        ),
    },
    ActivitySpec {
        id: ActivityId::NET_NFSD,
        name: "A_NET_NFSD",
        desc: "NFS server statistics",
        json_key: "net-nfsd",
        xml_elem: "net-nfsd",
        xml_child: "",
        xml_wrapper_attrs: "",
        group: Group::Network,
        closes_group: false,
        item: ItemKind::None,
        shape: Shape::Object,
        merge_sections: false,
        sections: one_section!(
            "scall/s;badcall/s;packet/s;udp/s;tcp/s;hit/s;miss/s;sread/s;swrite/s;saccess/s;sgetatt/s",
            NET_NFSD_FIELDS,
            RawSpec::AllPval
        ),
    },
    ActivitySpec {
        id: ActivityId::NET_SOCK,
        name: "A_NET_SOCK",
        desc: "IPv4 sockets statistics",
        json_key: "net-sock",
        xml_elem: "net-sock",
        xml_child: "",
        xml_wrapper_attrs: "",
        group: Group::Network,
        closes_group: false,
        item: ItemKind::None,
        shape: Shape::Object,
        merge_sections: false,
        sections: one_section!(
            "totsck;tcpsck;udpsck;rawsck;ip-frag;tcp-tw",
            NET_SOCK_FIELDS,
            RawSpec::Fields(NET_SOCK_RAW)
        ),
    },
    ActivitySpec {
        id: ActivityId::NET_IP,
        name: "A_NET_IP",
        desc: "IPv4 traffic statistics",
        json_key: "net-ip",
        xml_elem: "net-ip",
        xml_child: "",
        xml_wrapper_attrs: "",
        group: Group::Network,
        closes_group: false,
        item: ItemKind::None,
        shape: Shape::Object,
        merge_sections: false,
        sections: one_section!(
            "irec/s;fwddgm/s;idel/s;orq/s;asmrq/s;asmok/s;fragok/s;fragcrt/s",
            NET_IP_FIELDS,
            RawSpec::AllPval
        ),
    },
    ActivitySpec {
        id: ActivityId::NET_EIP,
        name: "A_NET_EIP",
        desc: "IPv4 traffic errors statistics",
        json_key: "net-eip",
        xml_elem: "net-eip",
        xml_child: "",
        xml_wrapper_attrs: "",
        group: Group::Network,
        closes_group: false,
        item: ItemKind::None,
        shape: Shape::Object,
        merge_sections: false,
        sections: one_section!(
            "ihdrerr/s;iadrerr/s;iukwnpr/s;idisc/s;odisc/s;onort/s;asmf/s;fragf/s",
            NET_EIP_FIELDS,
            RawSpec::AllPval
        ),
    },
    ActivitySpec {
        id: ActivityId::NET_ICMP,
        name: "A_NET_ICMP",
        desc: "ICMPv4 traffic statistics",
        json_key: "net-icmp",
        xml_elem: "net-icmp",
        xml_child: "",
        xml_wrapper_attrs: "",
        group: Group::Network,
        closes_group: false,
        item: ItemKind::None,
        shape: Shape::Object,
        merge_sections: false,
        sections: one_section!(
            "imsg/s;omsg/s;iech/s;iechr/s;oech/s;oechr/s;itm/s;itmr/s;otm/s;otmr/s;iadrmk/s;iadrmkr/s;oadrmk/s;oadrmkr/s",
            NET_ICMP_FIELDS,
            RawSpec::AllPval
        ),
    },
    ActivitySpec {
        id: ActivityId::NET_EICMP,
        name: "A_NET_EICMP",
        desc: "ICMPv4 traffic errors statistics",
        json_key: "net-eicmp",
        xml_elem: "net-eicmp",
        xml_child: "",
        xml_wrapper_attrs: "",
        group: Group::Network,
        closes_group: false,
        item: ItemKind::None,
        shape: Shape::Object,
        merge_sections: false,
        sections: one_section!(
            "ierr/s;oerr/s;idstunr/s;odstunr/s;itmex/s;otmex/s;iparmpb/s;oparmpb/s;isrcq/s;osrcq/s;iredir/s;oredir/s",
            NET_EICMP_FIELDS,
            RawSpec::AllPval
        ),
    },
    ActivitySpec {
        id: ActivityId::NET_TCP,
        name: "A_NET_TCP",
        desc: "TCPv4 traffic statistics",
        json_key: "net-tcp",
        xml_elem: "net-tcp",
        xml_child: "",
        xml_wrapper_attrs: "",
        group: Group::Network,
        closes_group: false,
        item: ItemKind::None,
        shape: Shape::Object,
        merge_sections: false,
        sections: one_section!(
            "active/s;passive/s;iseg/s;oseg/s",
            NET_TCP_FIELDS,
            RawSpec::AllPval
        ),
    },
    ActivitySpec {
        id: ActivityId::NET_ETCP,
        name: "A_NET_ETCP",
        desc: "TCPv4 traffic errors statistics",
        json_key: "net-etcp",
        xml_elem: "net-etcp",
        xml_child: "",
        xml_wrapper_attrs: "",
        group: Group::Network,
        closes_group: false,
        item: ItemKind::None,
        shape: Shape::Object,
        merge_sections: false,
        sections: one_section!(
            "atmptf/s;estres/s;retrseg/s;isegerr/s;orsts/s",
            NET_ETCP_FIELDS,
            RawSpec::AllPval
        ),
    },
    ActivitySpec {
        id: ActivityId::NET_UDP,
        name: "A_NET_UDP",
        desc: "UDPv4 traffic statistics",
        json_key: "net-udp",
        xml_elem: "net-udp",
        xml_child: "",
        xml_wrapper_attrs: "",
        group: Group::Network,
        closes_group: false,
        item: ItemKind::None,
        shape: Shape::Object,
        merge_sections: false,
        sections: one_section!(
            "idgm/s;odgm/s;noport/s;idgmerr/s",
            NET_UDP_FIELDS,
            RawSpec::AllPval
        ),
    },
    ActivitySpec {
        id: ActivityId::NET_SOCK6,
        name: "A_NET_SOCK6",
        desc: "IPv6 sockets statistics",
        json_key: "net-sock6",
        xml_elem: "net-sock6",
        xml_child: "",
        xml_wrapper_attrs: "",
        group: Group::Network,
        closes_group: false,
        item: ItemKind::None,
        shape: Shape::Object,
        merge_sections: false,
        sections: one_section!(
            "tcp6sck;udp6sck;raw6sck;ip6-frag",
            NET_SOCK6_FIELDS,
            RawSpec::Fields(NET_SOCK6_RAW)
        ),
    },
    ActivitySpec {
        id: ActivityId::NET_IP6,
        name: "A_NET_IP6",
        desc: "IPv6 traffic statistics",
        json_key: "net-ip6",
        xml_elem: "net-ip6",
        xml_child: "",
        xml_wrapper_attrs: "",
        group: Group::Network,
        closes_group: false,
        item: ItemKind::None,
        shape: Shape::Object,
        merge_sections: false,
        sections: one_section!(
            "irec6/s;fwddgm6/s;idel6/s;orq6/s;asmrq6/s;asmok6/s;imcpck6/s;omcpck6/s;fragok6/s;fragcr6/s",
            NET_IP6_FIELDS,
            RawSpec::AllPval
        ),
    },
    ActivitySpec {
        id: ActivityId::NET_EIP6,
        name: "A_NET_EIP6",
        desc: "IPv6 traffic errors statistics",
        json_key: "net-eip6",
        xml_elem: "net-eip6",
        xml_child: "",
        xml_wrapper_attrs: "",
        group: Group::Network,
        closes_group: false,
        item: ItemKind::None,
        shape: Shape::Object,
        merge_sections: false,
        sections: one_section!(
            "ihdrer6/s;iadrer6/s;iukwnp6/s;i2big6/s;idisc6/s;odisc6/s;inort6/s;onort6/s;asmf6/s;fragf6/s;itrpck6/s",
            NET_EIP6_FIELDS,
            RawSpec::AllPval
        ),
    },
    ActivitySpec {
        id: ActivityId::NET_ICMP6,
        name: "A_NET_ICMP6",
        desc: "ICMPv6 traffic statistics",
        json_key: "net-icmp6",
        xml_elem: "net-icmp6",
        xml_child: "",
        xml_wrapper_attrs: "",
        group: Group::Network,
        closes_group: false,
        item: ItemKind::None,
        shape: Shape::Object,
        merge_sections: false,
        sections: one_section!(
            "imsg6/s;omsg6/s;iech6/s;iechr6/s;oechr6/s;igmbq6/s;igmbr6/s;ogmbr6/s;igmbrd6/s;ogmbrd6/s;irtsol6/s;ortsol6/s;irtad6/s;inbsol6/s;onbsol6/s;inbad6/s;onbad6/s",
            NET_ICMP6_FIELDS,
            RawSpec::AllPval
        ),
    },
    ActivitySpec {
        id: ActivityId::NET_EICMP6,
        name: "A_NET_EICMP6",
        desc: "ICMPv6 traffic errors statistics",
        json_key: "net-eicmp6",
        xml_elem: "net-eicmp6",
        xml_child: "",
        xml_wrapper_attrs: "",
        group: Group::Network,
        closes_group: false,
        item: ItemKind::None,
        shape: Shape::Object,
        merge_sections: false,
        sections: one_section!(
            "ierr6/s;idtunr6/s;odtunr6/s;itmex6/s;otmex6/s;iprmpb6/s;oprmpb6/s;iredir6/s;oredir6/s;ipck2b6/s;opck2b6/s",
            NET_EICMP6_FIELDS,
            RawSpec::AllPval
        ),
    },
    ActivitySpec {
        id: ActivityId::NET_UDP6,
        name: "A_NET_UDP6",
        desc: "UDPv6 traffic statistics",
        json_key: "net-udp6",
        xml_elem: "net-udp6",
        xml_child: "",
        xml_wrapper_attrs: "",
        group: Group::Network,
        closes_group: false,
        item: ItemKind::None,
        shape: Shape::Object,
        merge_sections: false,
        sections: one_section!(
            "idgm6/s;odgm6/s;noport6/s;idgmer6/s",
            NET_UDP6_FIELDS,
            RawSpec::AllPval
        ),
    },
    ActivitySpec {
        id: ActivityId::NET_FC,
        name: "A_NET_FC",
        desc: "Fibre Channel HBA statistics",
        json_key: "fchosts",
        xml_elem: "fchosts",
        xml_child: "fchost",
        xml_wrapper_attrs: "",
        group: Group::Network,
        closes_group: false,
        item: ItemKind::Name,
        shape: Shape::Array,
        merge_sections: false,
        sections: one_section!(
            "FCHOST;fch_rxf/s;fch_txf/s;fch_rxw/s;fch_txw/s",
            NET_FC_FIELDS,
            RawSpec::AllPval
        ),
    },
    ActivitySpec {
        id: ActivityId::NET_SOFT,
        name: "A_NET_SOFT",
        desc: "Software-based network processing statistics",
        json_key: "softnet",
        xml_elem: "softnet",
        xml_child: "softnet",
        xml_wrapper_attrs: "",
        group: Group::Network,
        // </network> を閉じる担当 (§0.5)
        closes_group: true,
        item: ItemKind::Cpu,
        shape: Shape::Array,
        merge_sections: false,
        sections: one_section!(
            "CPU;total/s;dropd/s;squeezd/s;rx_rps/s;flw_lim/s;blg_len",
            NET_SOFT_FIELDS,
            RawSpec::Fields(NET_SOFT_RAW)
        ),
    },
    // ---- <power-management> グループ ----
    ActivitySpec {
        id: ActivityId::PWR_CPU,
        name: "A_PWR_CPU",
        desc: "CPU clock frequency",
        json_key: "cpu-frequency",
        xml_elem: "cpu-frequency",
        xml_child: "cpufreq",
        xml_wrapper_attrs: " unit=\"MHz\"",
        group: Group::PowerManagement,
        closes_group: false,
        item: ItemKind::Cpu,
        shape: Shape::Array,
        merge_sections: false,
        sections: one_section!("CPU;MHz", PWR_CPU_FIELDS, RawSpec::Fields(PWR_CPU_RAW)),
    },
    ActivitySpec {
        id: ActivityId::PWR_FAN,
        name: "A_PWR_FAN",
        desc: "Fans speed",
        json_key: "fan-speed",
        xml_elem: "fan-speed",
        xml_child: "fan",
        xml_wrapper_attrs: " unit=\"rpm\"",
        group: Group::PowerManagement,
        closes_group: false,
        item: ItemKind::Index {
            prefix: "fan",
            base: 1,
        },
        shape: Shape::Array,
        merge_sections: false,
        sections: one_section!(
            "FAN;DEVICE;rpm;drpm",
            PWR_FAN_FIELDS,
            RawSpec::Fields(PWR_FAN_RAW),
            PWR_FAN_JX_ORDER
        ),
    },
    ActivitySpec {
        id: ActivityId::PWR_TEMP,
        name: "A_PWR_TEMP",
        desc: "Devices temperature",
        json_key: "temperature",
        xml_elem: "temperature",
        xml_child: "temp",
        xml_wrapper_attrs: " unit=\"degree Celsius\"",
        group: Group::PowerManagement,
        closes_group: false,
        item: ItemKind::Index {
            prefix: "temp",
            base: 1,
        },
        shape: Shape::Array,
        merge_sections: false,
        sections: one_section!(
            "TEMP;DEVICE;degC;%temp",
            PWR_TEMP_FIELDS,
            RawSpec::Fields(PWR_TEMP_RAW),
            PWR_TEMP_JX_ORDER
        ),
    },
    ActivitySpec {
        id: ActivityId::PWR_IN,
        name: "A_PWR_IN",
        desc: "Voltage inputs statistics",
        json_key: "voltage-input",
        xml_elem: "voltage-input",
        xml_child: "in",
        xml_wrapper_attrs: " unit=\"V\"",
        group: Group::PowerManagement,
        closes_group: false,
        item: ItemKind::Index {
            prefix: "in",
            base: 0,
        },
        shape: Shape::Array,
        merge_sections: false,
        sections: one_section!(
            "IN;DEVICE;inV;%in",
            PWR_IN_FIELDS,
            RawSpec::Fields(PWR_IN_RAW),
            PWR_IN_JX_ORDER
        ),
    },
    ActivitySpec {
        id: ActivityId::PWR_FREQ,
        name: "A_PWR_FREQ",
        desc: "CPU weighted frequency",
        json_key: "cpu-weighted-frequency",
        xml_elem: "cpu-weighted-frequency",
        xml_child: "cpuwfreq",
        xml_wrapper_attrs: " unit=\"MHz\"",
        group: Group::PowerManagement,
        closes_group: false,
        item: ItemKind::Cpu,
        shape: Shape::Array,
        merge_sections: false,
        sections: one_section!("CPU;wghMHz", PWR_FREQ_FIELDS, RawSpec::Fields(PWR_FREQ_RAW)),
    },
    ActivitySpec {
        id: ActivityId::PWR_BAT,
        name: "A_PWR_BAT",
        desc: "Batteries capacity",
        json_key: "battery",
        xml_elem: "battery",
        xml_child: "bat",
        xml_wrapper_attrs: " unit=\"minute\"",
        group: Group::PowerManagement,
        closes_group: false,
        item: ItemKind::Column {
            col: "bat_id",
            prefix: "BAT",
        },
        shape: Shape::Array,
        merge_sections: false,
        sections: one_section!(
            "BAT;%cap;cap/min;status",
            PWR_BAT_FIELDS,
            RawSpec::Fields(PWR_BAT_RAW)
        ),
    },
    ActivitySpec {
        id: ActivityId::PWR_USB,
        name: "A_PWR_USB",
        desc: "USB devices",
        json_key: "usb-devices",
        xml_elem: "usb-devices",
        xml_child: "usb",
        xml_wrapper_attrs: "",
        group: Group::PowerManagement,
        // </power-management> を閉じる担当 (§0.5)
        closes_group: true,
        item: ItemKind::Column {
            col: "bus",
            prefix: "bus",
        },
        shape: Shape::Array,
        merge_sections: false,
        sections: one_section!(
            "manufact;product;BUS;idvendor;idprod;maxpower",
            PWR_USB_FIELDS,
            RawSpec::Fields(PWR_USB_RAW)
        ),
    },
    // ---- ラッパを持たない ----
    ActivitySpec {
        id: ActivityId::FS,
        name: "A_FS",
        desc: "Filesystems statistics",
        json_key: "filesystems",
        xml_elem: "filesystems",
        xml_child: "filesystem",
        xml_wrapper_attrs: "",
        group: Group::None,
        closes_group: false,
        item: ItemKind::Name,
        shape: Shape::Array,
        merge_sections: false,
        sections: &[
            Section {
                gate: SectionGate::FsName,
                item_col: "filesystem",
                hdr_line: "FILESYSTEM;MBfsfree;MBfsused;%fsused;%ufsused;Ifree;Iused;%Iused",
                fields: FS_NAME_FIELDS,
                jx_order: &[],
                raw: RawSpec::Fields(FS_RAW),
            },
            Section {
                gate: SectionGate::FsMount,
                item_col: "mountpoint",
                hdr_line: "MOUNTPOINT;MBfsfree;MBfsused;%fsused;%ufsused;Ifree;Iused;%Iused",
                fields: FS_MOUNT_FIELDS,
                jx_order: &[],
                raw: RawSpec::Fields(FS_RAW),
            },
        ],
    },
    // ---- <psi> グループ ----
    ActivitySpec {
        id: ActivityId::PSI_CPU,
        name: "A_PSI_CPU",
        desc: "Pressure-stall CPU statistics",
        json_key: "psi-cpu",
        xml_elem: "psi-cpu",
        xml_child: "",
        xml_wrapper_attrs: "",
        group: Group::Psi,
        closes_group: false,
        item: ItemKind::None,
        shape: Shape::Object,
        merge_sections: false,
        sections: one_section!(
            "%scpu-10;%scpu-60;%scpu-300;%scpu",
            PSI_CPU_FIELDS,
            RawSpec::Fields(PSI_CPU_RAW)
        ),
    },
    ActivitySpec {
        id: ActivityId::PSI_IO,
        name: "A_PSI_IO",
        desc: "Pressure-stall I/O statistics",
        json_key: "psi-io",
        xml_elem: "psi-io",
        xml_child: "",
        xml_wrapper_attrs: "",
        group: Group::Psi,
        closes_group: false,
        item: ItemKind::None,
        shape: Shape::Object,
        merge_sections: false,
        sections: one_section!(
            "%sio-10;%sio-60;%sio-300;%sio;%fio-10;%fio-60;%fio-300;%fio",
            PSI_IO_FIELDS,
            RawSpec::Fields(PSI_IO_RAW)
        ),
    },
    ActivitySpec {
        id: ActivityId::PSI_MEM,
        name: "A_PSI_MEM",
        desc: "Pressure-stall memory statistics",
        json_key: "psi-mem",
        xml_elem: "psi-mem",
        xml_child: "",
        xml_wrapper_attrs: "",
        group: Group::Psi,
        // </psi> を閉じる担当 (§0.5)
        closes_group: true,
        item: ItemKind::None,
        shape: Shape::Object,
        merge_sections: false,
        sections: one_section!(
            "%smem-10;%smem-60;%smem-300;%smem;%fmem-10;%fmem-60;%fmem-300;%fmem",
            PSI_MEM_FIELDS,
            RawSpec::Fields(PSI_MEM_RAW)
        ),
    },
];

/// activity ID から出力定義を引く。
pub fn lookup(id: ActivityId) -> Option<&'static ActivitySpec> {
    SPECS.iter().find(|s| s.id == id)
}

/// `-d` / `-p` / `-r` の出力順 (ID 昇順 = `sar` のレポート順)。
pub fn in_id_order() -> Vec<&'static ActivitySpec> {
    let mut v: Vec<_> = SPECS.iter().collect();
    v.sort_by_key(|s| s.id.0);
    v
}

// ===========================================================================
// テスト
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::registry;

    /// 43 activity すべてに出力定義があること。
    #[test]
    fn every_activity_has_a_spec() {
        for id in crate::model::activity::KNOWN_ACTIVITIES {
            assert!(lookup(*id).is_some(), "{id} の出力定義が無い");
        }
        assert_eq!(SPECS.len(), 43);
    }

    /// `col` が参照する列が `layout::registry` に実在すること。
    ///
    /// 列名の打ち間違いは値が黙って「未対応」に落ちる形で現れるので、
    /// 表の側で機械的に弾く。
    #[test]
    fn all_column_references_resolve() {
        for spec in SPECS {
            let def = registry::lookup(spec.id).expect("registry 定義");
            let has = |name: &str| def.columns.iter().any(|c| c.public_name == name);

            for sec in spec.sections {
                for fld in sec.fields {
                    if !fld.col.is_empty() {
                        assert!(
                            has(fld.col),
                            "{}: 列 {} が registry に無い",
                            spec.name,
                            fld.col
                        );
                    }
                }
                if let RawSpec::Fields(raws) = sec.raw {
                    for r in raws {
                        match r.style {
                            RawStyle::PvalSum(cols) => {
                                for c in cols {
                                    assert!(has(c), "{}: 列 {c} が registry に無い", spec.name);
                                }
                            }
                            RawStyle::PvalDiff(a, b) => {
                                assert!(has(a), "{}: 列 {a} が registry に無い", spec.name);
                                assert!(has(b), "{}: 列 {b} が registry に無い", spec.name);
                            }
                            _ => {
                                if !r.col.is_empty() {
                                    assert!(
                                        has(r.col),
                                        "{}: 列 {} が registry に無い",
                                        spec.name,
                                        r.col
                                    );
                                }
                            }
                        }
                    }
                }
            }

            if let ItemKind::Column { col, .. } = spec.item {
                assert!(
                    has(col),
                    "{}: アイテム列 {col} が registry に無い",
                    spec.name
                );
            }
        }
    }

    /// `jx_order` は `fields` の添字の置換であること。
    #[test]
    fn jx_order_is_a_permutation() {
        for spec in SPECS {
            for sec in spec.sections {
                if sec.jx_order.is_empty() {
                    continue;
                }
                assert_eq!(
                    sec.jx_order.len(),
                    sec.fields.len(),
                    "{}: jx_order の長さが合わない",
                    spec.name
                );
                let mut seen = vec![false; sec.fields.len()];
                for &i in sec.jx_order {
                    assert!(!seen[i], "{}: jx_order に重複", spec.name);
                    seen[i] = true;
                }
            }
        }
    }

    /// `hdr_line` の 1 列目はアイテムラベル (アイテムを持つ activity のみ)。
    #[test]
    fn hdr_line_matches_documented_text() {
        let cpu = lookup(ActivityId::CPU).unwrap();
        assert_eq!(
            cpu.sections[0].hdr_line,
            "CPU;%user;%nice;%system;%iowait;%steal;%idle"
        );
        assert_eq!(
            cpu.sections[1].hdr_line,
            "CPU;%usr;%nice;%sys;%iowait;%steal;%irq;%soft;%guest;%gnice;%idle"
        );
        // A_PWR_USB のヘッダは実データ順と食い違ったまま保持する (本家バグの再現)
        let usb = lookup(ActivityId::PWR_USB).unwrap();
        assert_eq!(
            usb.sections[0].hdr_line,
            "manufact;product;BUS;idvendor;idprod;maxpower"
        );
        assert_eq!(usb.sections[0].fields[1].pp, "idvendor");
    }

    /// A_MEMORY の `-p` フィールド名は `kbshared`、`hdr_line` 側は `kbshmem`。
    #[test]
    fn memory_label_mismatch_is_preserved() {
        let mem = lookup(ActivityId::MEMORY).unwrap();
        let sec = &mem.sections[0];
        assert!(sec.hdr_line.contains(";kbshmem&"));
        assert!(sec.fields.iter().any(|f| f.pp == "kbshared"));
    }

    /// `&` の展開は `-r ALL` 相当の有無で変わる。
    #[test]
    fn hdr_line_ampersand_expansion() {
        let mem = lookup(ActivityId::MEMORY).unwrap();
        let hdr = mem.sections[0].hdr_line;

        let all = SectionConfig {
            mem_all: true,
            ..SectionConfig::default()
        };
        assert_eq!(
            all.expand_hdr_line(hdr),
            "kbmemfree;kbavail;kbmemused;%memused;kbbuffers;kbcached;kbcommit;%commit;kbactive;kbinact;kbdirty;kbshmem;kbanonpg;kbslab;kbkstack;kbpgtbl;kbvmused"
        );

        let basic = SectionConfig {
            mem_all: false,
            ..SectionConfig::default()
        };
        assert_eq!(
            basic.expand_hdr_line(hdr),
            "kbmemfree;kbavail;kbmemused;%memused;kbbuffers;kbcached;kbcommit;%commit;kbactive;kbinact;kbdirty;kbshmem"
        );
    }

    /// グループの閉じ担当は 3 件だけ (§0.5)。
    #[test]
    fn exactly_three_activities_close_a_group() {
        let closers: Vec<_> = SPECS
            .iter()
            .filter(|s| s.closes_group)
            .map(|s| s.name)
            .collect();
        assert_eq!(closers, vec!["A_NET_SOFT", "A_PWR_USB", "A_PSI_MEM"]);
    }

    /// `hugepages` は `power-management` の中ではない (§9.6-14)。
    #[test]
    fn hugepages_is_not_in_power_management() {
        assert_eq!(lookup(ActivityId::HUGE).unwrap().group, Group::None);
    }
}

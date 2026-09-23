//! system 系 activity の定義 (`A_CPU` / `A_PCSW` / `A_IRQ` / `A_SWAP` / `A_PAGE` /
//! `A_IO` / `A_MEMORY` / `A_KTABLES` / `A_QUEUE` / `A_SERIAL` / `A_HUGE` /
//! `A_PSI_CPU` / `A_PSI_IO` / `A_PSI_MEM`)。
//!
//! フィールド順・オフセット・`size_lp64` / `types_nr` の出所は
//! `docs/format/02-activities.md` (§5 のオフセット表、§9 のバージョン間差分)、
//! 列メタデータの出所は `docs/format/03-output-format.md` (§7 の activity 別仕様)。
//!
//! ## レイアウトの時代区分 (§9.1)
//!
//! | 時代 | 対象 | 特徴 |
//! |---|---|---|
//! | A | v9.1.5 〜 v11.6.6 (`FORMAT_MAGIC` = `0x2171` / `0x2173`) | 全フィールドに `aligned(16)` / `aligned(8)` / `packed` を手で付けていた |
//! | B/C | v11.7.1 〜 現行 (`0x2175`) | `unsigned long` の `aligned(8)` 以外の属性を撤去し、多くのフィールドを `unsigned long long` に拡幅 |
//!
//! ここでは時代 A の revision も定義する (`sadf -c` 相当の旧フォーマット読み取りに必要)。
//! 時代 A のファイルに `types_nr` は無いが、宣言値はレイアウト記述から導出できる値に
//! 揃えてある (自己整合性テストがこれを検査する)。
//!
//! ## magic だけでは revision が決まらないもの
//!
//! sysstat は「フィールドを末尾に増やすだけなら magic を上げない」規約
//! (§8.2 `remap_struct`) を持つため、次の 4 件は同一 magic に複数のサイズが対応する。
//! 選択は magic + `types_nr` + `file_activity.size` の 3 点で行う必要がある
//! (`revision_for_magic` は同 magic のうち最も新しいものを返す)。
//!
//! | activity | magic | 対応するサイズ |
//! |---|---|---|
//! | `A_PAGE` | `0x8a` | 64 (〜v12.7.4) / 80 (v12.7.5〜) |
//! | `A_IO` | `0x8b` | 48 (時代 A) / 40 (v11.7.1〜v12.1.1) / 56 (v12.1.2〜) |
//! | `A_MEMORY` | `0x8a` | 64 / 80 / 88 / 128 / 136 (時代 A の各段階) |
//! | `A_HUGE` | `0x8b` | 16 (`types_nr` = (2,0,0)) / 32 ((4,0,0)) |
//!
//! ## v11.7.1 が書いたファイル (§3.4)
//!
//! v11.7.1 は構造体を時代 B へ移したが per-activity の `magic` を上げ忘れており、
//! 「新レイアウトなのに旧 magic」という組み合わせが実在する
//! (`A_CPU` = `0x8a`/80、`A_MEMORY` = `0x8a`/136、`A_QUEUE` = `0x8b`/40 など)。
//! 自己記述形式では時代 A 専用の配置を候補から外す。v11.7.1 の既知17組は
//! registry::activity_magic_for_source で翌版の magic に対応付けてデコードする。
//! 互換出力の表示判定は元の magic を使い、現行の本家と同じ規則を保つ。

use crate::format::wire::{FieldTy, WireField, WireLayout};
use crate::layout::registry::{ActivityDef, ColumnMeta, ItemShape, WireRevision};
use crate::model::{ActivityId, Aggregation, Unit, ValueKind};

// ===========================================================================
// A_CPU (1) — stats_cpu
// ===========================================================================

/// 現行レイアウト (v11.7.2〜)。8 バイト境界に 10 個の累積 tick が並ぶ。
///
/// 並びは `/proc/stat` の列順と**違う** (§11.2-10): `iowait` の次が `steal`、
/// その後に `hardirq` / `softirq` が来る。
const CPU_FIELDS_B: &[WireField] = &[
    WireField::natural("cpu_user", FieldTy::U64),
    WireField::natural("cpu_nice", FieldTy::U64),
    WireField::natural("cpu_sys", FieldTy::U64),
    WireField::natural("cpu_idle", FieldTy::U64),
    WireField::natural("cpu_iowait", FieldTy::U64),
    WireField::natural("cpu_steal", FieldTy::U64),
    WireField::natural("cpu_hardirq", FieldTy::U64),
    WireField::natural("cpu_softirq", FieldTy::U64),
    WireField::natural("cpu_guest", FieldTy::U64),
    WireField::natural("cpu_guest_nice", FieldTy::U64),
];

/// 時代 A (v10.1.2〜v11.6.6)。全フィールドが `aligned(16)` なので 1 本で 16 バイトを占める。
const CPU_FIELDS_A10: &[WireField] = &[
    WireField::aligned("cpu_user", FieldTy::U64, 16),
    WireField::aligned("cpu_nice", FieldTy::U64, 16),
    WireField::aligned("cpu_sys", FieldTy::U64, 16),
    WireField::aligned("cpu_idle", FieldTy::U64, 16),
    WireField::aligned("cpu_iowait", FieldTy::U64, 16),
    WireField::aligned("cpu_steal", FieldTy::U64, 16),
    WireField::aligned("cpu_hardirq", FieldTy::U64, 16),
    WireField::aligned("cpu_softirq", FieldTy::U64, 16),
    WireField::aligned("cpu_guest", FieldTy::U64, 16),
    WireField::aligned("cpu_guest_nice", FieldTy::U64, 16),
];

/// 時代 A (v9.1.5〜v10.1.1)。`cpu_guest_nice` がまだ無い 9 フィールド (§9.4)。
const CPU_FIELDS_A9: &[WireField] = &[
    WireField::aligned("cpu_user", FieldTy::U64, 16),
    WireField::aligned("cpu_nice", FieldTy::U64, 16),
    WireField::aligned("cpu_sys", FieldTy::U64, 16),
    WireField::aligned("cpu_idle", FieldTy::U64, 16),
    WireField::aligned("cpu_iowait", FieldTy::U64, 16),
    WireField::aligned("cpu_steal", FieldTy::U64, 16),
    WireField::aligned("cpu_hardirq", FieldTy::U64, 16),
    WireField::aligned("cpu_softirq", FieldTy::U64, 16),
    WireField::aligned("cpu_guest", FieldTy::U64, 16),
];

const CPU_REVISIONS: &[WireRevision] = &[
    WireRevision {
        magic: 0x8b,
        self_describing: true,
        types_nr: [10, 0, 0],
        size_lp64: 80,
        layout: WireLayout::new("stats_cpu@0x8b", CPU_FIELDS_B),
        since: "11.7.2",
    },
    // v10.1.2 で cpu_guest_nice が加わり 144 → 160 になった。
    WireRevision {
        magic: 0x8a,
        self_describing: false,
        types_nr: [10, 0, 0],
        size_lp64: 160,
        layout: WireLayout::new("stats_cpu@0x8a+guest_nice", CPU_FIELDS_A10),
        since: "10.1.2",
    },
    WireRevision {
        magic: 0x8a,
        self_describing: false,
        types_nr: [9, 0, 0],
        size_lp64: 144,
        layout: WireLayout::new("stats_cpu@0x8a", CPU_FIELDS_A9),
        since: "9.1.5",
    },
];

/// `-u` と `-u ALL` の 2 セクション分をまとめた列。
///
/// `hdr_line` = `CPU;%user;%nice;%system;%iowait;%steal;%idle|`
/// `CPU;%usr;%nice;%sys;%iowait;%steal;%irq;%soft;%guest;%gnice;%idle`。
/// `%nice` は 2 つのセクションで計算式が違う (既定は `cpu_nice` そのまま、
/// `ALL` は `cpu_nice - cpu_guest_nice`) ため、別の列として持つ。
///
/// 割合の分母は「その item の tick 合計」であり、CPU "all" 行は全 CPU の
/// interval 合計を使う (詳細は 03-output-format.md §1.4)。
const CPU_COLUMNS: &[ColumnMeta] = &[
    ColumnMeta {
        public_name: "user",
        sar_header: "%user",
        wire_name: "cpu_user",
        unit: Unit::Percent,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "nice",
        sar_header: "%nice",
        wire_name: "cpu_nice",
        unit: Unit::Percent,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    // %system = cpu_sys + cpu_hardirq + cpu_softirq
    ColumnMeta {
        public_name: "system",
        sar_header: "%system",
        wire_name: "",
        unit: Unit::Percent,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "iowait",
        sar_header: "%iowait",
        wire_name: "cpu_iowait",
        unit: Unit::Percent,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "steal",
        sar_header: "%steal",
        wire_name: "cpu_steal",
        unit: Unit::Percent,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "idle",
        sar_header: "%idle",
        wire_name: "cpu_idle",
        unit: Unit::Percent,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    // 以下 `-u ALL` セクション。%usr = cpu_user - cpu_guest
    ColumnMeta {
        public_name: "usr",
        sar_header: "%usr",
        wire_name: "",
        unit: Unit::Percent,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    // %nice (ALL) = cpu_nice - cpu_guest_nice
    ColumnMeta {
        public_name: "nice_excl_gnice",
        sar_header: "%nice",
        wire_name: "",
        unit: Unit::Percent,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "sys",
        sar_header: "%sys",
        wire_name: "cpu_sys",
        unit: Unit::Percent,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "irq",
        sar_header: "%irq",
        wire_name: "cpu_hardirq",
        unit: Unit::Percent,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "soft",
        sar_header: "%soft",
        wire_name: "cpu_softirq",
        unit: Unit::Percent,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "guest",
        sar_header: "%guest",
        wire_name: "cpu_guest",
        unit: Unit::Percent,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "gnice",
        sar_header: "%gnice",
        wire_name: "cpu_guest_nice",
        unit: Unit::Percent,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
];

// ===========================================================================
// A_PCSW (2) — stats_pcsw
// ===========================================================================

/// 現行レイアウト (v11.7.2〜)。`processes` は `unsigned long` (8 バイトスロット)。
const PCSW_FIELDS_B: &[WireField] = &[
    WireField::natural("context_switch", FieldTy::U64),
    WireField::natural("processes", FieldTy::CULong),
];

/// 時代 A (v9.1.5〜v11.6.6)。両フィールドが `aligned(16)` で 32 バイト。
const PCSW_FIELDS_A: &[WireField] = &[
    WireField::aligned("context_switch", FieldTy::U64, 16),
    WireField::aligned("processes", FieldTy::CULong, 16),
];

const PCSW_REVISIONS: &[WireRevision] = &[
    WireRevision {
        magic: 0x8b,
        self_describing: true,
        types_nr: [1, 1, 0],
        size_lp64: 16,
        layout: WireLayout::new("stats_pcsw@0x8b", PCSW_FIELDS_B),
        since: "11.7.2",
    },
    WireRevision {
        magic: 0x8a,
        self_describing: false,
        types_nr: [1, 1, 0],
        size_lp64: 32,
        layout: WireLayout::new("stats_pcsw@0x8a", PCSW_FIELDS_A),
        since: "9.1.5",
    },
];

/// `hdr_line` = `proc/s;cswch/s`
const PCSW_COLUMNS: &[ColumnMeta] = &[
    ColumnMeta {
        public_name: "proc",
        sar_header: "proc/s",
        wire_name: "processes",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "cswch",
        sar_header: "cswch/s",
        wire_name: "context_switch",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
];

// ===========================================================================
// A_IRQ (3) — stats_irq
// ===========================================================================

/// 現行レイアウト (v12.5.6〜)。`nr` 行 (CPU 数 + 1) × `nr2` 列 (割り込み数) の行列。
///
/// 要素のオフセットは `(cpu_row * nr2 + irq_col) * file_activity.size` (§6.1)。
/// `irq_name` (`MAX_SA_IRQ_LEN` = 8) は**行 0 (CPU "all") にしか書かれない** (§6.2)。
/// 列 0 は総和スロットで、行 0 列 0 の名前は `"sum"`。
const IRQ_FIELDS_C: &[WireField] = &[
    WireField::natural("irq_nr", FieldTy::U32),
    WireField::natural("irq_name", FieldTy::Bytes(8)),
];

/// v11.7.2〜v12.5.5 の 1 次元レイアウト。`irq_nr` のみで名前を持たない (§6.4)。
///
/// このとき `nr` = 割り込み数、`nr2` = 1 で、割り込みは配列添字でしか識別できない。
/// magic が `0x8c` に上がったため、現行の sar はこの世代の `A_IRQ` を読み飛ばす。
const IRQ_FIELDS_B: &[WireField] = &[WireField::natural("irq_nr", FieldTy::U64)];

/// 時代 A (v9.1.5〜v11.6.6)。`aligned(16)` により 1 フィールドで 16 バイト (§6.4)。
/// data-10.3.1 / data-11.6.5 と sa_conv.h stats_irq_8a で確認。
const IRQ_FIELDS_A: &[WireField] = &[WireField::aligned("irq_nr", FieldTy::U64, 16)];

const IRQ_REVISIONS: &[WireRevision] = &[
    WireRevision {
        magic: 0x8c,
        self_describing: true,
        types_nr: [0, 0, 1],
        size_lp64: 12,
        layout: WireLayout::new("stats_irq@0x8c", IRQ_FIELDS_C),
        since: "12.5.6",
    },
    WireRevision {
        magic: 0x8b,
        self_describing: true,
        types_nr: [1, 0, 0],
        size_lp64: 8,
        layout: WireLayout::new("stats_irq@0x8b", IRQ_FIELDS_B),
        since: "11.7.2",
    },
    WireRevision {
        magic: 0x8a,
        self_describing: false,
        types_nr: [1, 0, 0],
        size_lp64: 16,
        layout: WireLayout::new("stats_irq@0x8a", IRQ_FIELDS_A),
        since: "9.1.5",
    },
];

/// `hdr_line` = `INTR;CPU*`。
///
/// 値の列見出しは CPU ごとに動的生成される (`all` / `CPU0` / `CPU1` …) ので
/// `sar_header` は固定できない。旧世代 (magic `0x8b`) の見出しは `INTR;intr/s`。
const IRQ_COLUMNS: &[ColumnMeta] = &[
    ColumnMeta {
        public_name: "intr_name",
        sar_header: "INTR",
        wire_name: "irq_name",
        unit: Unit::Identifier,
        kind: ValueKind::Identity,
        aggregation: Aggregation::NotAggregated,
    },
    ColumnMeta {
        public_name: "intr",
        sar_header: "",
        wire_name: "irq_nr",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
];

// ===========================================================================
// A_SWAP (4) — stats_swap
// ===========================================================================

/// 全世代で共通 (magic 昇格なし・`aligned(8)` 維持・16 バイト)。
///
/// `unsigned long` は常に 8 バイトスロットなので、時代 A の `aligned(8)` と
/// 時代 B の無属性はファイル上で同一配置になる。
const SWAP_FIELDS: &[WireField] = &[
    WireField::natural("pswpin", FieldTy::CULong),
    WireField::natural("pswpout", FieldTy::CULong),
];

const SWAP_REVISIONS: &[WireRevision] = &[WireRevision {
    magic: 0x8a,
    self_describing: true,
    types_nr: [0, 2, 0],
    size_lp64: 16,
    layout: WireLayout::new("stats_swap@0x8a", SWAP_FIELDS),
    since: "9.1.5",
}];

/// `hdr_line` = `pswpin/s;pswpout/s`
const SWAP_COLUMNS: &[ColumnMeta] = &[
    ColumnMeta {
        public_name: "pswpin",
        sar_header: "pswpin/s",
        wire_name: "pswpin",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "pswpout",
        sar_header: "pswpout/s",
        wire_name: "pswpout",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
];

// ===========================================================================
// A_PAGE (5) — stats_paging
// ===========================================================================

/// 現行レイアウト (v12.7.5〜)。`pgpromote` / `pgdemote` が末尾に加わり 64 → 80 (§9.3)。
const PAGE_FIELDS_10: &[WireField] = &[
    WireField::natural("pgpgin", FieldTy::CULong),
    WireField::natural("pgpgout", FieldTy::CULong),
    WireField::natural("pgfault", FieldTy::CULong),
    WireField::natural("pgmajfault", FieldTy::CULong),
    WireField::natural("pgfree", FieldTy::CULong),
    WireField::natural("pgscan_kswapd", FieldTy::CULong),
    WireField::natural("pgscan_direct", FieldTy::CULong),
    WireField::natural("pgsteal", FieldTy::CULong),
    WireField::natural("pgpromote", FieldTy::CULong),
    WireField::natural("pgdemote", FieldTy::CULong),
];

/// v9.1.5〜v12.7.4 の 8 フィールド。
///
/// 時代 A の `aligned(8)` と時代 B の無属性は `unsigned long` では同一配置なので、
/// 旧フォーマット (`0x2171` / `0x2173`) のファイルもこの 1 つで読める。
const PAGE_FIELDS_8: &[WireField] = &[
    WireField::natural("pgpgin", FieldTy::CULong),
    WireField::natural("pgpgout", FieldTy::CULong),
    WireField::natural("pgfault", FieldTy::CULong),
    WireField::natural("pgmajfault", FieldTy::CULong),
    WireField::natural("pgfree", FieldTy::CULong),
    WireField::natural("pgscan_kswapd", FieldTy::CULong),
    WireField::natural("pgscan_direct", FieldTy::CULong),
    WireField::natural("pgsteal", FieldTy::CULong),
];

const PAGE_REVISIONS: &[WireRevision] = &[
    WireRevision {
        magic: 0x8a,
        self_describing: true,
        types_nr: [0, 10, 0],
        size_lp64: 80,
        layout: WireLayout::new("stats_paging@0x8a+promote", PAGE_FIELDS_10),
        since: "12.7.5",
    },
    // magic は昇格していない (§9.3)。64 と 80 の判別は types_nr / size で行う。
    WireRevision {
        magic: 0x8a,
        self_describing: true,
        types_nr: [0, 8, 0],
        size_lp64: 64,
        layout: WireLayout::new("stats_paging@0x8a", PAGE_FIELDS_8),
        since: "9.1.5",
    },
];

/// `hdr_line` = `pgpgin/s;pgpgout/s;fault/s;majflt/s;pgfree/s;pgscank/s;pgscand/s;`
/// `pgsteal/s;pgprom/s;pgdem/s`
///
/// `pgpgin` / `pgpgout` は kB 単位、他はページ数。負値クランプは無い。
const PAGE_COLUMNS: &[ColumnMeta] = &[
    ColumnMeta {
        public_name: "pgpgin",
        sar_header: "pgpgin/s",
        wire_name: "pgpgin",
        unit: Unit::KilobytesPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "pgpgout",
        sar_header: "pgpgout/s",
        wire_name: "pgpgout",
        unit: Unit::KilobytesPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "fault",
        sar_header: "fault/s",
        wire_name: "pgfault",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "majflt",
        sar_header: "majflt/s",
        wire_name: "pgmajfault",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "pgfree",
        sar_header: "pgfree/s",
        wire_name: "pgfree",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "pgscank",
        sar_header: "pgscank/s",
        wire_name: "pgscan_kswapd",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "pgscand",
        sar_header: "pgscand/s",
        wire_name: "pgscan_direct",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "pgsteal",
        sar_header: "pgsteal/s",
        wire_name: "pgsteal",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "pgprom",
        sar_header: "pgprom/s",
        wire_name: "pgpromote",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "pgdem",
        sar_header: "pgdem/s",
        wire_name: "pgdemote",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
];

// ===========================================================================
// A_IO (6) — stats_io
// ===========================================================================

/// 現行レイアウト (v12.1.2〜)。discard I/O の 2 本が末尾に加わり 40 → 56 (§9.3)。
const IO_FIELDS_7: &[WireField] = &[
    WireField::natural("dk_drive", FieldTy::U64),
    WireField::natural("dk_drive_rio", FieldTy::U64),
    WireField::natural("dk_drive_wio", FieldTy::U64),
    WireField::natural("dk_drive_rblk", FieldTy::U64),
    WireField::natural("dk_drive_wblk", FieldTy::U64),
    WireField::natural("dk_drive_dio", FieldTy::U64),
    WireField::natural("dk_drive_dblk", FieldTy::U64),
];

/// v11.7.1〜v12.1.1 の 5 フィールド (40 バイト)。
const IO_FIELDS_5: &[WireField] = &[
    WireField::natural("dk_drive", FieldTy::U64),
    WireField::natural("dk_drive_rio", FieldTy::U64),
    WireField::natural("dk_drive_wio", FieldTy::U64),
    WireField::natural("dk_drive_rblk", FieldTy::U64),
    WireField::natural("dk_drive_wblk", FieldTy::U64),
];

/// 時代 A (v10.1.1〜v11.6.6)。先頭が `aligned(16)`、以降 `packed` なので
/// 実データは 40 バイトだが構造体アラインメント 16 により**サイズは 48**。
/// (sysstat 10.1.5 の実ファイルで magic `0x8b` / size 48 を確認済み)
const IO_FIELDS_A5: &[WireField] = &[
    WireField::aligned("dk_drive", FieldTy::U64, 16),
    WireField::packed("dk_drive_rio", FieldTy::U64),
    WireField::packed("dk_drive_wio", FieldTy::U64),
    WireField::packed("dk_drive_rblk", FieldTy::U64),
    WireField::packed("dk_drive_wblk", FieldTy::U64),
];

/// RHEL / CentOS 6.5 以降のベンダー派生 (`format_magic = 0x1170`、sysstat 9.0.4-22〜)。
///
/// Red Hat の `sysstat-9.0.4-diskstats.patch` が 5 フィールドすべてを
/// `unsigned long long __attribute__((aligned (16)))` に変えたため、
/// upstream 9.0.4 の 20 バイトに対し **80 バイト**になる。
/// 各フィールドは 16 バイトスロットの先頭 8 バイトが値で、残り 8 バイトはパディング。
///
/// upstream にこのサイズの `stats_io` は存在しないので、`0x2170` 世代に
/// activity magic が無くても申告サイズだけで一意に判別できる。
const IO_FIELDS_RH5: &[WireField] = &[
    WireField::aligned("dk_drive", FieldTy::U64, 16),
    WireField::aligned("dk_drive_rio", FieldTy::U64, 16),
    WireField::aligned("dk_drive_wio", FieldTy::U64, 16),
    WireField::aligned("dk_drive_rblk", FieldTy::U64, 16),
    WireField::aligned("dk_drive_wblk", FieldTy::U64, 16),
];

/// 時代 A (v9.1.5〜v10.0.5)。5 フィールドが `unsigned int` で 20 バイト (§9.4)。
const IO_FIELDS_A5_U32: &[WireField] = &[
    WireField::aligned("dk_drive", FieldTy::U32, 4),
    WireField::packed("dk_drive_rio", FieldTy::U32),
    WireField::packed("dk_drive_wio", FieldTy::U32),
    WireField::packed("dk_drive_rblk", FieldTy::U32),
    WireField::packed("dk_drive_wblk", FieldTy::U32),
];

const IO_REVISIONS: &[WireRevision] = &[
    WireRevision {
        magic: 0x8b,
        self_describing: true,
        types_nr: [7, 0, 0],
        size_lp64: 56,
        layout: WireLayout::new("stats_io@0x8b+discard", IO_FIELDS_7),
        since: "12.1.2",
    },
    WireRevision {
        magic: 0x8b,
        self_describing: true,
        types_nr: [5, 0, 0],
        size_lp64: 40,
        layout: WireLayout::new("stats_io@0x8b", IO_FIELDS_5),
        since: "11.7.1",
    },
    WireRevision {
        magic: 0x8b,
        self_describing: false,
        types_nr: [5, 0, 0],
        size_lp64: 48,
        layout: WireLayout::new("stats_io@0x8b:aligned16", IO_FIELDS_A5),
        since: "10.1.1",
    },
    WireRevision {
        magic: 0x8a,
        self_describing: false,
        types_nr: [0, 0, 5],
        size_lp64: 20,
        layout: WireLayout::new("stats_io@0x8a", IO_FIELDS_A5_U32),
        since: "9.1.5",
    },
    WireRevision {
        magic: 0x8a,
        self_describing: false,
        types_nr: [5, 0, 0],
        size_lp64: 80,
        layout: WireLayout::new("stats_io@rhel6", IO_FIELDS_RH5),
        since: "9.0.4-22 (RHEL/CentOS 6.5)",
    },
];

/// `hdr_line` = `tps;rtps;wtps;dtps;bread/s;bwrtn/s;bdscd/s`
///
/// `b*` 列は 512 バイトセクタ数/秒。全列にアンマウント由来の負値クランプがある。
const IO_COLUMNS: &[ColumnMeta] = &[
    ColumnMeta {
        public_name: "tps",
        sar_header: "tps",
        wire_name: "dk_drive",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "rtps",
        sar_header: "rtps",
        wire_name: "dk_drive_rio",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "wtps",
        sar_header: "wtps",
        wire_name: "dk_drive_wio",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "dtps",
        sar_header: "dtps",
        wire_name: "dk_drive_dio",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "bread",
        sar_header: "bread/s",
        wire_name: "dk_drive_rblk",
        unit: Unit::SectorsPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "bwrtn",
        sar_header: "bwrtn/s",
        wire_name: "dk_drive_wblk",
        unit: Unit::SectorsPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "bdscd",
        sar_header: "bdscd/s",
        wire_name: "dk_drive_dblk",
        unit: Unit::SectorsPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
];

// ===========================================================================
// A_MEMORY (7) — stats_memory
// ===========================================================================

/// 現行レイアウト (v12.7.8〜)。`shmemkb` が末尾に加わり 136 → 144 (§9.3)。
const MEMORY_FIELDS_18: &[WireField] = &[
    WireField::natural("frmkb", FieldTy::U64),
    WireField::natural("bufkb", FieldTy::U64),
    WireField::natural("camkb", FieldTy::U64),
    WireField::natural("tlmkb", FieldTy::U64),
    WireField::natural("frskb", FieldTy::U64),
    WireField::natural("tlskb", FieldTy::U64),
    WireField::natural("caskb", FieldTy::U64),
    WireField::natural("comkb", FieldTy::U64),
    WireField::natural("activekb", FieldTy::U64),
    WireField::natural("inactkb", FieldTy::U64),
    WireField::natural("dirtykb", FieldTy::U64),
    WireField::natural("anonpgkb", FieldTy::U64),
    WireField::natural("slabkb", FieldTy::U64),
    WireField::natural("kstackkb", FieldTy::U64),
    WireField::natural("pgtblkb", FieldTy::U64),
    WireField::natural("vmusedkb", FieldTy::U64),
    WireField::natural("availablekb", FieldTy::U64),
    WireField::natural("shmemkb", FieldTy::U64),
];

/// v11.7.2〜v12.7.7 の 17 フィールド (136 バイト)。
const MEMORY_FIELDS_17: &[WireField] = &[
    WireField::natural("frmkb", FieldTy::U64),
    WireField::natural("bufkb", FieldTy::U64),
    WireField::natural("camkb", FieldTy::U64),
    WireField::natural("tlmkb", FieldTy::U64),
    WireField::natural("frskb", FieldTy::U64),
    WireField::natural("tlskb", FieldTy::U64),
    WireField::natural("caskb", FieldTy::U64),
    WireField::natural("comkb", FieldTy::U64),
    WireField::natural("activekb", FieldTy::U64),
    WireField::natural("inactkb", FieldTy::U64),
    WireField::natural("dirtykb", FieldTy::U64),
    WireField::natural("anonpgkb", FieldTy::U64),
    WireField::natural("slabkb", FieldTy::U64),
    WireField::natural("kstackkb", FieldTy::U64),
    WireField::natural("pgtblkb", FieldTy::U64),
    WireField::natural("vmusedkb", FieldTy::U64),
    WireField::natural("availablekb", FieldTy::U64),
];

/// 時代 A (v11.5.3〜v11.7.1)。全 17 フィールドが `unsigned long aligned(8)` で 136 バイト。
///
/// 時代 B との差は型 (`unsigned long` / `unsigned long long`) だけで、LP64 では
/// 配置が一致する。32bit ライタのファイルでは有効バイト数が 4 になるため区別が必要で、
/// 判別は `file_magic.format_magic` (`0x2173` = 時代 A / `0x2175` = 時代 B) で行う。
const MEMORY_FIELDS_A17: &[WireField] = &[
    WireField::natural("frmkb", FieldTy::CULong),
    WireField::natural("bufkb", FieldTy::CULong),
    WireField::natural("camkb", FieldTy::CULong),
    WireField::natural("tlmkb", FieldTy::CULong),
    WireField::natural("frskb", FieldTy::CULong),
    WireField::natural("tlskb", FieldTy::CULong),
    WireField::natural("caskb", FieldTy::CULong),
    WireField::natural("comkb", FieldTy::CULong),
    WireField::natural("activekb", FieldTy::CULong),
    WireField::natural("inactkb", FieldTy::CULong),
    WireField::natural("dirtykb", FieldTy::CULong),
    WireField::natural("anonpgkb", FieldTy::CULong),
    WireField::natural("slabkb", FieldTy::CULong),
    WireField::natural("kstackkb", FieldTy::CULong),
    WireField::natural("pgtblkb", FieldTy::CULong),
    WireField::natural("vmusedkb", FieldTy::CULong),
    WireField::natural("availablekb", FieldTy::CULong),
];

/// 時代 A (v11.1.3〜v11.5.2)。`availablekb` 追加前の 16 フィールド = 128 バイト (§9.4)。
const MEMORY_FIELDS_A16: &[WireField] = &[
    WireField::natural("frmkb", FieldTy::CULong),
    WireField::natural("bufkb", FieldTy::CULong),
    WireField::natural("camkb", FieldTy::CULong),
    WireField::natural("tlmkb", FieldTy::CULong),
    WireField::natural("frskb", FieldTy::CULong),
    WireField::natural("tlskb", FieldTy::CULong),
    WireField::natural("caskb", FieldTy::CULong),
    WireField::natural("comkb", FieldTy::CULong),
    WireField::natural("activekb", FieldTy::CULong),
    WireField::natural("inactkb", FieldTy::CULong),
    WireField::natural("dirtykb", FieldTy::CULong),
    WireField::natural("anonpgkb", FieldTy::CULong),
    WireField::natural("slabkb", FieldTy::CULong),
    WireField::natural("kstackkb", FieldTy::CULong),
    WireField::natural("pgtblkb", FieldTy::CULong),
    WireField::natural("vmusedkb", FieldTy::CULong),
];

/// 時代 A (v10.1.2〜v11.1.2)。`anonpgkb` 以降の 5 本が無い 11 フィールド = 88 バイト。
/// (sysstat 10.1.5 の実ファイルで magic `0x8a` / size 88 を確認済み)
const MEMORY_FIELDS_A11: &[WireField] = &[
    WireField::natural("frmkb", FieldTy::CULong),
    WireField::natural("bufkb", FieldTy::CULong),
    WireField::natural("camkb", FieldTy::CULong),
    WireField::natural("tlmkb", FieldTy::CULong),
    WireField::natural("frskb", FieldTy::CULong),
    WireField::natural("tlskb", FieldTy::CULong),
    WireField::natural("caskb", FieldTy::CULong),
    WireField::natural("comkb", FieldTy::CULong),
    WireField::natural("activekb", FieldTy::CULong),
    WireField::natural("inactkb", FieldTy::CULong),
    WireField::natural("dirtykb", FieldTy::CULong),
];

/// 時代 A (v9.1.7〜v10.1.1)。`dirtykb` 追加前の 10 フィールド = 80 バイト (§9.4 / §9.5)。
const MEMORY_FIELDS_A10: &[WireField] = &[
    WireField::natural("frmkb", FieldTy::CULong),
    WireField::natural("bufkb", FieldTy::CULong),
    WireField::natural("camkb", FieldTy::CULong),
    WireField::natural("tlmkb", FieldTy::CULong),
    WireField::natural("frskb", FieldTy::CULong),
    WireField::natural("tlskb", FieldTy::CULong),
    WireField::natural("caskb", FieldTy::CULong),
    WireField::natural("comkb", FieldTy::CULong),
    WireField::natural("activekb", FieldTy::CULong),
    WireField::natural("inactkb", FieldTy::CULong),
];

/// v9.1.5 / v9.1.6。sa_conv.h stats_memory_8a の先頭 8 本 (§9.4)。
const MEMORY_FIELDS_A8: &[WireField] = &[
    WireField::natural("frmkb", FieldTy::CULong),
    WireField::natural("bufkb", FieldTy::CULong),
    WireField::natural("camkb", FieldTy::CULong),
    WireField::natural("tlmkb", FieldTy::CULong),
    WireField::natural("frskb", FieldTy::CULong),
    WireField::natural("tlskb", FieldTy::CULong),
    WireField::natural("caskb", FieldTy::CULong),
    WireField::natural("comkb", FieldTy::CULong),
];

const MEMORY_REVISIONS: &[WireRevision] = &[
    WireRevision {
        magic: 0x8b,
        self_describing: true,
        types_nr: [18, 0, 0],
        size_lp64: 144,
        layout: WireLayout::new("stats_memory@0x8b+shmem", MEMORY_FIELDS_18),
        since: "12.7.8",
    },
    WireRevision {
        magic: 0x8b,
        self_describing: true,
        types_nr: [17, 0, 0],
        size_lp64: 136,
        layout: WireLayout::new("stats_memory@0x8b", MEMORY_FIELDS_17),
        since: "11.7.2",
    },
    WireRevision {
        magic: 0x8a,
        self_describing: false,
        types_nr: [0, 17, 0],
        size_lp64: 136,
        layout: WireLayout::new("stats_memory@0x8a:17", MEMORY_FIELDS_A17),
        since: "11.5.3",
    },
    WireRevision {
        magic: 0x8a,
        self_describing: false,
        types_nr: [0, 16, 0],
        size_lp64: 128,
        layout: WireLayout::new("stats_memory@0x8a:16", MEMORY_FIELDS_A16),
        since: "11.1.3",
    },
    WireRevision {
        magic: 0x8a,
        self_describing: false,
        types_nr: [0, 11, 0],
        size_lp64: 88,
        layout: WireLayout::new("stats_memory@0x8a:11", MEMORY_FIELDS_A11),
        since: "10.1.2",
    },
    WireRevision {
        magic: 0x8a,
        self_describing: false,
        types_nr: [0, 10, 0],
        size_lp64: 80,
        layout: WireLayout::new("stats_memory@0x8a:10", MEMORY_FIELDS_A10),
        since: "9.1.7",
    },
    WireRevision {
        magic: 0x8a,
        self_describing: false,
        types_nr: [0, 8, 0],
        size_lp64: 64,
        layout: WireLayout::new("stats_memory@0x8a:8", MEMORY_FIELDS_A8),
        since: "9.1.5",
    },
];

/// `hdr_line` (2 セクション) =
/// `kbmemfree;kbavail;kbmemused;%memused;kbbuffers;kbcached;kbcommit;%commit;`
/// `kbactive;kbinact;kbdirty;kbshmem&kbanonpg;kbslab;kbkstack;kbpgtbl;kbvmused|`
/// `kbswpfree;kbswpused;%swpused;kbswpcad;%swpcad`
///
/// `&` は `-r` と `-r ALL` の境界、`|` は RAM ブロックと swap ブロックの境界。
/// 総量 (`tlmkb` / `tlskb`) は sar の列には現れないが派生列の分母に必要なので持つ。
const MEMORY_COLUMNS: &[ColumnMeta] = &[
    ColumnMeta {
        public_name: "kbmemfree",
        sar_header: "kbmemfree",
        wire_name: "frmkb",
        unit: Unit::Kilobytes,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    ColumnMeta {
        public_name: "kbavail",
        sar_header: "kbavail",
        wire_name: "availablekb",
        unit: Unit::Kilobytes,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    // kbmemused = tlmkb - availablekb
    ColumnMeta {
        public_name: "kbmemused",
        sar_header: "kbmemused",
        wire_name: "",
        unit: Unit::Kilobytes,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    // %memused = (tlmkb - availablekb) / tlmkb * 100 (tlmkb == 0 なら 0.0)
    ColumnMeta {
        public_name: "memused_pct",
        sar_header: "%memused",
        wire_name: "",
        unit: Unit::Percent,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    ColumnMeta {
        public_name: "kbbuffers",
        sar_header: "kbbuffers",
        wire_name: "bufkb",
        unit: Unit::Kilobytes,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    ColumnMeta {
        public_name: "kbcached",
        sar_header: "kbcached",
        wire_name: "camkb",
        unit: Unit::Kilobytes,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    ColumnMeta {
        public_name: "kbcommit",
        sar_header: "kbcommit",
        wire_name: "comkb",
        unit: Unit::Kilobytes,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    // %commit = comkb / (tlmkb + tlskb) * 100 (分母 0 なら 0.0)
    ColumnMeta {
        public_name: "commit_pct",
        sar_header: "%commit",
        wire_name: "",
        unit: Unit::Percent,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    ColumnMeta {
        public_name: "kbactive",
        sar_header: "kbactive",
        wire_name: "activekb",
        unit: Unit::Kilobytes,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    ColumnMeta {
        public_name: "kbinact",
        sar_header: "kbinact",
        wire_name: "inactkb",
        unit: Unit::Kilobytes,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    ColumnMeta {
        public_name: "kbdirty",
        sar_header: "kbdirty",
        wire_name: "dirtykb",
        unit: Unit::Kilobytes,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    ColumnMeta {
        public_name: "kbshmem",
        sar_header: "kbshmem",
        wire_name: "shmemkb",
        unit: Unit::Kilobytes,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    // 以下 `-r ALL` セクション
    ColumnMeta {
        public_name: "kbanonpg",
        sar_header: "kbanonpg",
        wire_name: "anonpgkb",
        unit: Unit::Kilobytes,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    ColumnMeta {
        public_name: "kbslab",
        sar_header: "kbslab",
        wire_name: "slabkb",
        unit: Unit::Kilobytes,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    ColumnMeta {
        public_name: "kbkstack",
        sar_header: "kbkstack",
        wire_name: "kstackkb",
        unit: Unit::Kilobytes,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    ColumnMeta {
        public_name: "kbpgtbl",
        sar_header: "kbpgtbl",
        wire_name: "pgtblkb",
        unit: Unit::Kilobytes,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    ColumnMeta {
        public_name: "kbvmused",
        sar_header: "kbvmused",
        wire_name: "vmusedkb",
        unit: Unit::Kilobytes,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    // sar には現れないが派生列の分母に使う
    ColumnMeta {
        public_name: "kbmemtotal",
        sar_header: "",
        wire_name: "tlmkb",
        unit: Unit::Kilobytes,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    // 以下 swap ブロック (`-S`)
    ColumnMeta {
        public_name: "kbswpfree",
        sar_header: "kbswpfree",
        wire_name: "frskb",
        unit: Unit::Kilobytes,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    // kbswpused = tlskb - frskb
    ColumnMeta {
        public_name: "kbswpused",
        sar_header: "kbswpused",
        wire_name: "",
        unit: Unit::Kilobytes,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    // %swpused = (tlskb - frskb) / tlskb * 100 (tlskb == 0 なら 0.0)
    ColumnMeta {
        public_name: "swpused_pct",
        sar_header: "%swpused",
        wire_name: "",
        unit: Unit::Percent,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    ColumnMeta {
        public_name: "kbswpcad",
        sar_header: "kbswpcad",
        wire_name: "caskb",
        unit: Unit::Kilobytes,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    // %swpcad = caskb / (tlskb - frskb) * 100 (分母 0 なら 0.0)
    ColumnMeta {
        public_name: "swpcad_pct",
        sar_header: "%swpcad",
        wire_name: "",
        unit: Unit::Percent,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    ColumnMeta {
        public_name: "kbswptotal",
        sar_header: "",
        wire_name: "tlskb",
        unit: Unit::Kilobytes,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
];

// ===========================================================================
// A_KTABLES (8) — stats_ktables
// ===========================================================================

/// 現行レイアウト (v11.7.2〜)。4 フィールドが `unsigned long long` に拡幅されて 32 バイト。
const KTABLES_FIELDS_B: &[WireField] = &[
    WireField::natural("file_used", FieldTy::U64),
    WireField::natural("inode_used", FieldTy::U64),
    WireField::natural("dentry_stat", FieldTy::U64),
    WireField::natural("pty_nr", FieldTy::U64),
];

/// 時代 A (v9.1.5〜v11.7.1)。4 フィールドが `unsigned int` で 16 バイト (§9.3)。
const KTABLES_FIELDS_A: &[WireField] = &[
    WireField::aligned("file_used", FieldTy::U32, 4),
    WireField::packed("inode_used", FieldTy::U32),
    WireField::packed("dentry_stat", FieldTy::U32),
    WireField::packed("pty_nr", FieldTy::U32),
];

const KTABLES_REVISIONS: &[WireRevision] = &[
    WireRevision {
        magic: 0x8b,
        self_describing: true,
        types_nr: [4, 0, 0],
        size_lp64: 32,
        layout: WireLayout::new("stats_ktables@0x8b", KTABLES_FIELDS_B),
        since: "11.7.2",
    },
    WireRevision {
        magic: 0x8a,
        self_describing: false,
        types_nr: [0, 0, 4],
        size_lp64: 16,
        layout: WireLayout::new("stats_ktables@0x8a", KTABLES_FIELDS_A),
        since: "9.1.5",
    },
];

/// `hdr_line` = `dentunusd;file-nr;inode-nr;pty-nr`
///
/// すべて瞬時値 (個数)。`--human` の対象ではないので `Unit::None`。
const KTABLES_COLUMNS: &[ColumnMeta] = &[
    ColumnMeta {
        public_name: "dentunusd",
        sar_header: "dentunusd",
        wire_name: "dentry_stat",
        unit: Unit::None,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    ColumnMeta {
        public_name: "file_nr",
        sar_header: "file-nr",
        wire_name: "file_used",
        unit: Unit::None,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    ColumnMeta {
        public_name: "inode_nr",
        sar_header: "inode-nr",
        wire_name: "inode_used",
        unit: Unit::None,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    ColumnMeta {
        public_name: "pty_nr",
        sar_header: "pty-nr",
        wire_name: "pty_nr",
        unit: Unit::None,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
];

// ===========================================================================
// A_QUEUE (9) — stats_queue
// ===========================================================================

/// 現行レイアウト (v11.7.2〜)。**43 構造体の中で唯一 ABI でサイズが変わる** (§1.3)。
///
/// 先頭 3 本の `unsigned long long` に `aligned(8)` が付いていないため、
/// long long のアラインメントが 4 の i386 では構造体アラインメントが 4 になり
/// 末尾パディングが消えて 36 バイトになる (LP64 / ARM32 は 40)。
/// → ストライドは必ず `file_activity.size` を使う。
const QUEUE_FIELDS_B: &[WireField] = &[
    WireField::natural("nr_running", FieldTy::U64),
    WireField::natural("procs_blocked", FieldTy::U64),
    WireField::natural("nr_threads", FieldTy::U64),
    WireField::natural("load_avg_1", FieldTy::U32),
    WireField::natural("load_avg_5", FieldTy::U32),
    WireField::natural("load_avg_15", FieldTy::U32),
];

/// 時代 A (v9.1.7〜v11.7.1)。時代 B とは**フィールド順が違う**: `nr_threads` が
/// `load_avg_*` の後ろにあり、`nr_running` / `procs_blocked` は `unsigned long` (§9.3)。
/// (sysstat 10.1.5 の実ファイルで magic `0x8b` / size 32 を確認済み)
const QUEUE_FIELDS_A6: &[WireField] = &[
    WireField::natural("nr_running", FieldTy::CULong),
    WireField::natural("procs_blocked", FieldTy::CULong),
    WireField::aligned("load_avg_1", FieldTy::U32, 8),
    WireField::packed("load_avg_5", FieldTy::U32),
    WireField::packed("load_avg_15", FieldTy::U32),
    WireField::packed("nr_threads", FieldTy::U32),
];

/// 時代 A (v9.1.6)。`procs_blocked` 追加前の 24 バイト。
/// この追加が magic `0x8a` → `0x8b` 昇格 (v9.1.7) の理由 (§3.3)。
const QUEUE_FIELDS_A5: &[WireField] = &[
    WireField::natural("nr_running", FieldTy::CULong),
    WireField::aligned("load_avg_1", FieldTy::U32, 8),
    WireField::packed("load_avg_5", FieldTy::U32),
    WireField::packed("load_avg_15", FieldTy::U32),
    WireField::packed("nr_threads", FieldTy::U32),
];

const QUEUE_REVISIONS: &[WireRevision] = &[
    WireRevision {
        magic: 0x8c,
        self_describing: true,
        types_nr: [3, 0, 3],
        size_lp64: 40,
        layout: WireLayout::new("stats_queue@0x8c", QUEUE_FIELDS_B),
        since: "11.7.2",
    },
    WireRevision {
        magic: 0x8b,
        self_describing: false,
        types_nr: [0, 2, 4],
        size_lp64: 32,
        layout: WireLayout::new("stats_queue@0x8b", QUEUE_FIELDS_A6),
        since: "9.1.7",
    },
    WireRevision {
        magic: 0x8a,
        self_describing: false,
        types_nr: [0, 1, 4],
        size_lp64: 24,
        layout: WireLayout::new("stats_queue@0x8a", QUEUE_FIELDS_A5),
        since: "9.1.6",
    },
];

/// `hdr_line` = `runq-sz;plist-sz;ldavg-1;ldavg-5;ldavg-15;blocked`
///
/// `load_avg_*` はカーネルの **値 ×100 の整数** (§11.2-15)。表示時に 100 で割る。
const QUEUE_COLUMNS: &[ColumnMeta] = &[
    ColumnMeta {
        public_name: "runq_sz",
        sar_header: "runq-sz",
        wire_name: "nr_running",
        unit: Unit::None,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    ColumnMeta {
        public_name: "plist_sz",
        sar_header: "plist-sz",
        wire_name: "nr_threads",
        unit: Unit::None,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    ColumnMeta {
        public_name: "ldavg_1",
        sar_header: "ldavg-1",
        wire_name: "load_avg_1",
        unit: Unit::None,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    ColumnMeta {
        public_name: "ldavg_5",
        sar_header: "ldavg-5",
        wire_name: "load_avg_5",
        unit: Unit::None,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    ColumnMeta {
        public_name: "ldavg_15",
        sar_header: "ldavg-15",
        wire_name: "load_avg_15",
        unit: Unit::None,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    ColumnMeta {
        public_name: "blocked",
        sar_header: "blocked",
        wire_name: "procs_blocked",
        unit: Unit::None,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
];

// ===========================================================================
// A_SERIAL (10) — stats_serial
// ===========================================================================

/// 全世代で同一配置 (7 × `unsigned int` = 28 バイト)。
///
/// 時代 A の `aligned(4)` / `packed` は `unsigned int` では自然配置と一致するため、
/// magic (`0x8a` / `0x8b`) だけが違う 2 revision に同じ記述を使う。
/// 末尾の `line` は TTY 回線番号で、**カウンタではなく識別子**。
const SERIAL_FIELDS: &[WireField] = &[
    WireField::natural("rx", FieldTy::U32),
    WireField::natural("tx", FieldTy::U32),
    WireField::natural("frame", FieldTy::U32),
    WireField::natural("parity", FieldTy::U32),
    WireField::natural("brk", FieldTy::U32),
    WireField::natural("overrun", FieldTy::U32),
    WireField::natural("line", FieldTy::U32),
];

const SERIAL_REVISIONS: &[WireRevision] = &[
    WireRevision {
        magic: 0x8b,
        self_describing: true,
        types_nr: [0, 0, 7],
        size_lp64: 28,
        layout: WireLayout::new("stats_serial@0x8b", SERIAL_FIELDS),
        since: "11.7.2",
    },
    WireRevision {
        magic: 0x8a,
        self_describing: false,
        types_nr: [0, 0, 7],
        size_lp64: 28,
        layout: WireLayout::new("stats_serial@0x8a", SERIAL_FIELDS),
        since: "9.1.5",
    },
];

/// `hdr_line` = `TTY;rcvin/s;xmtin/s;framerr/s;prtyerr/s;brk/s;ovrun/s`
const SERIAL_COLUMNS: &[ColumnMeta] = &[
    ColumnMeta {
        public_name: "line",
        sar_header: "TTY",
        wire_name: "line",
        unit: Unit::None,
        kind: ValueKind::Identity,
        aggregation: Aggregation::NotAggregated,
    },
    ColumnMeta {
        public_name: "rcvin",
        sar_header: "rcvin/s",
        wire_name: "rx",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "xmtin",
        sar_header: "xmtin/s",
        wire_name: "tx",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "framerr",
        sar_header: "framerr/s",
        wire_name: "frame",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "prtyerr",
        sar_header: "prtyerr/s",
        wire_name: "parity",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "brk",
        sar_header: "brk/s",
        wire_name: "brk",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "ovrun",
        sar_header: "ovrun/s",
        wire_name: "overrun",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
];

// ===========================================================================
// A_HUGE (34) — stats_huge
// ===========================================================================

/// 現行レイアウト (v12.1.1〜)。`rsvdhkb` / `surphkb` が加わり 16 → 32 (§9.3)。
const HUGE_FIELDS_4: &[WireField] = &[
    WireField::natural("frhkb", FieldTy::U64),
    WireField::natural("tlhkb", FieldTy::U64),
    WireField::natural("rsvdhkb", FieldTy::U64),
    WireField::natural("surphkb", FieldTy::U64),
];

/// v11.7.2〜v12.0.6 の 2 フィールド (構造体の実サイズは 16 バイト)。
const HUGE_FIELDS_B2: &[WireField] = &[
    WireField::natural("frhkb", FieldTy::U64),
    WireField::natural("tlhkb", FieldTy::U64),
];

/// 時代 A (v9.1.6〜v11.7.1)。2 フィールドが `unsigned long aligned(8)` (実サイズ 16)。
const HUGE_FIELDS_A2: &[WireField] = &[
    WireField::natural("frhkb", FieldTy::CULong),
    WireField::natural("tlhkb", FieldTy::CULong),
];

/// `size_lp64` は**構造体の実サイズ**を書く。
///
/// `A_HUGE` は `STATS_HUGE_SIZE` が `sizeof(struct stats_memory)` と定義されていた
/// 本家のバグ (§9.5、v9.1.6〜v12.0.0) のため、**ファイル上の `file_activity.size` は
/// 構造体サイズと一致しない**。ディスクに現れる `size` は
///
/// | 書き込み側 | `size` |
/// |---|---:|
/// | v9.1.6 | 64 |
/// | v9.1.7〜v10.1.1 | 80 |
/// | v10.1.2〜v11.1.2 | **88** (sysstat 10.1.5 の実ファイルで確認) |
/// | v11.1.3〜v11.5.2 | 128 |
/// | v11.5.3〜v11.6.6 / v11.7.1〜v12.0.0 | 136 |
/// | v12.0.1〜v12.0.6 | 16 |
/// | v12.1.1〜 | 32 |
///
/// であり、意味があるのは先頭 16 バイトだけ (残りはゼロ埋め)。
/// この不一致は**正常**なので、`size_lp64` には実サイズ (16 / 32) を書き、
/// ストライドには常に申告値を使う (`DecodePlan::stride`)。
/// 逆に 32 バイト固定で読むと旧ファイルで 104 バイトずれて全崩壊する。
const HUGE_REVISIONS: &[WireRevision] = &[
    WireRevision {
        magic: 0x8b,
        self_describing: true,
        types_nr: [4, 0, 0],
        size_lp64: 32,
        layout: WireLayout::new("stats_huge@0x8b+rsvd", HUGE_FIELDS_4),
        since: "12.1.1",
    },
    WireRevision {
        magic: 0x8b,
        self_describing: true,
        types_nr: [2, 0, 0],
        size_lp64: 16,
        layout: WireLayout::new("stats_huge@0x8b", HUGE_FIELDS_B2),
        since: "11.7.2",
    },
    WireRevision {
        magic: 0x8a,
        self_describing: false,
        types_nr: [0, 2, 0],
        size_lp64: 16,
        layout: WireLayout::new("stats_huge@0x8a", HUGE_FIELDS_A2),
        since: "9.1.6",
    },
];

/// `hdr_line` = `kbhugfree;kbhugused;%hugused;kbhugrsvd;kbhugsurp`
const HUGE_COLUMNS: &[ColumnMeta] = &[
    ColumnMeta {
        public_name: "kbhugfree",
        sar_header: "kbhugfree",
        wire_name: "frhkb",
        unit: Unit::Kilobytes,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    // kbhugused = tlhkb - frhkb
    ColumnMeta {
        public_name: "kbhugused",
        sar_header: "kbhugused",
        wire_name: "",
        unit: Unit::Kilobytes,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    // %hugused = (tlhkb - frhkb) / tlhkb * 100 (tlhkb == 0 なら 0.0)
    ColumnMeta {
        public_name: "hugused_pct",
        sar_header: "%hugused",
        wire_name: "",
        unit: Unit::Percent,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    ColumnMeta {
        public_name: "kbhugrsvd",
        sar_header: "kbhugrsvd",
        wire_name: "rsvdhkb",
        unit: Unit::Kilobytes,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    ColumnMeta {
        public_name: "kbhugsurp",
        sar_header: "kbhugsurp",
        wire_name: "surphkb",
        unit: Unit::Kilobytes,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    // sar には現れないが派生列の分母に使う
    ColumnMeta {
        public_name: "kbhugtotal",
        sar_header: "",
        wire_name: "tlhkb",
        unit: Unit::Kilobytes,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
];

// ===========================================================================
// A_PSI_CPU (40) / A_PSI_IO (41) / A_PSI_MEM (42) — stats_psi_*
// ===========================================================================
//
// PSI 3 種は `AO_DETECTED` であって `AO_COUNTED` ではないので **`has_nr` は立たない**
// (§3.1)。count 関数は /proc/pressure の存在確認にしか使われず、item 数は常に 1。
// 「count 関数があるから item 数が前置される」と考えると 4 バイト余分に読む (§11.2-4)。
//
// `*_total` はマイクロ秒の累積値、`*_a*_{10,60,300}` はカーネル報告の
// **% ×100 の整数** (§11.2-15)。
//
// v12.3.3〜v12.5.1 (stable は v12.4.1) は `unsigned long` に `aligned(8)` が
// 無かったため 32bit ライタのファイルだけサイズが 20 / 40 になる (§9.6)。
// その場合 `MAP_SIZE(types_nr)` > `size` となり本家も破損扱いにするので、
// revision は置かない (拒否が本家互換)。64bit ライタのファイルは現行と同一配置。

/// 現行レイアウト (v12.3.3〜)。`unsigned long` 3 本は 8 バイトスロット。
const PSI_CPU_FIELDS: &[WireField] = &[
    WireField::natural("some_cpu_total", FieldTy::U64),
    WireField::natural("some_acpu_10", FieldTy::CULong),
    WireField::natural("some_acpu_60", FieldTy::CULong),
    WireField::natural("some_acpu_300", FieldTy::CULong),
];

const PSI_CPU_REVISIONS: &[WireRevision] = &[WireRevision {
    magic: 0x8a,
    self_describing: true,
    types_nr: [1, 3, 0],
    size_lp64: 32,
    layout: WireLayout::new("stats_psi_cpu@0x8a", PSI_CPU_FIELDS),
    since: "12.3.3",
}];

/// `hdr_line` = `%scpu-10;%scpu-60;%scpu-300;%scpu`
///
/// `%scpu` は `some_cpu_total` の差分 (µs) を経過時間で割った率:
/// `(curr - prev) / (100 * itv)` — `itv` は 1/100 秒単位なので分母は µs 換算になる。
const PSI_CPU_COLUMNS: &[ColumnMeta] = &[
    ColumnMeta {
        public_name: "scpu_10",
        sar_header: "%scpu-10",
        wire_name: "some_acpu_10",
        unit: Unit::Percent,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    ColumnMeta {
        public_name: "scpu_60",
        sar_header: "%scpu-60",
        wire_name: "some_acpu_60",
        unit: Unit::Percent,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    ColumnMeta {
        public_name: "scpu_300",
        sar_header: "%scpu-300",
        wire_name: "some_acpu_300",
        unit: Unit::Percent,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    ColumnMeta {
        public_name: "scpu",
        sar_header: "%scpu",
        wire_name: "some_cpu_total",
        unit: Unit::Percent,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
];

/// 現行レイアウト (v12.3.3〜)。ULL 2 本 + UL 6 本 = 64 バイト。
const PSI_IO_FIELDS: &[WireField] = &[
    WireField::natural("some_io_total", FieldTy::U64),
    WireField::natural("full_io_total", FieldTy::U64),
    WireField::natural("some_aio_10", FieldTy::CULong),
    WireField::natural("some_aio_60", FieldTy::CULong),
    WireField::natural("some_aio_300", FieldTy::CULong),
    WireField::natural("full_aio_10", FieldTy::CULong),
    WireField::natural("full_aio_60", FieldTy::CULong),
    WireField::natural("full_aio_300", FieldTy::CULong),
];

const PSI_IO_REVISIONS: &[WireRevision] = &[WireRevision {
    magic: 0x8a,
    self_describing: true,
    types_nr: [2, 6, 0],
    size_lp64: 64,
    layout: WireLayout::new("stats_psi_io@0x8a", PSI_IO_FIELDS),
    since: "12.3.3",
}];

/// `hdr_line` = `%sio-10;%sio-60;%sio-300;%sio;%fio-10;%fio-60;%fio-300;%fio`
const PSI_IO_COLUMNS: &[ColumnMeta] = &[
    ColumnMeta {
        public_name: "sio_10",
        sar_header: "%sio-10",
        wire_name: "some_aio_10",
        unit: Unit::Percent,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    ColumnMeta {
        public_name: "sio_60",
        sar_header: "%sio-60",
        wire_name: "some_aio_60",
        unit: Unit::Percent,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    ColumnMeta {
        public_name: "sio_300",
        sar_header: "%sio-300",
        wire_name: "some_aio_300",
        unit: Unit::Percent,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    ColumnMeta {
        public_name: "sio",
        sar_header: "%sio",
        wire_name: "some_io_total",
        unit: Unit::Percent,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "fio_10",
        sar_header: "%fio-10",
        wire_name: "full_aio_10",
        unit: Unit::Percent,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    ColumnMeta {
        public_name: "fio_60",
        sar_header: "%fio-60",
        wire_name: "full_aio_60",
        unit: Unit::Percent,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    ColumnMeta {
        public_name: "fio_300",
        sar_header: "%fio-300",
        wire_name: "full_aio_300",
        unit: Unit::Percent,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    ColumnMeta {
        public_name: "fio",
        sar_header: "%fio",
        wire_name: "full_io_total",
        unit: Unit::Percent,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
];

/// 現行レイアウト (v12.3.3〜)。`stats_psi_io` と完全に同形。
const PSI_MEM_FIELDS: &[WireField] = &[
    WireField::natural("some_mem_total", FieldTy::U64),
    WireField::natural("full_mem_total", FieldTy::U64),
    WireField::natural("some_amem_10", FieldTy::CULong),
    WireField::natural("some_amem_60", FieldTy::CULong),
    WireField::natural("some_amem_300", FieldTy::CULong),
    WireField::natural("full_amem_10", FieldTy::CULong),
    WireField::natural("full_amem_60", FieldTy::CULong),
    WireField::natural("full_amem_300", FieldTy::CULong),
];

const PSI_MEM_REVISIONS: &[WireRevision] = &[WireRevision {
    magic: 0x8a,
    self_describing: true,
    types_nr: [2, 6, 0],
    size_lp64: 64,
    layout: WireLayout::new("stats_psi_mem@0x8a", PSI_MEM_FIELDS),
    since: "12.3.3",
}];

/// `hdr_line` = `%smem-10;%smem-60;%smem-300;%smem;%fmem-10;%fmem-60;%fmem-300;%fmem`
const PSI_MEM_COLUMNS: &[ColumnMeta] = &[
    ColumnMeta {
        public_name: "smem_10",
        sar_header: "%smem-10",
        wire_name: "some_amem_10",
        unit: Unit::Percent,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    ColumnMeta {
        public_name: "smem_60",
        sar_header: "%smem-60",
        wire_name: "some_amem_60",
        unit: Unit::Percent,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    ColumnMeta {
        public_name: "smem_300",
        sar_header: "%smem-300",
        wire_name: "some_amem_300",
        unit: Unit::Percent,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    ColumnMeta {
        public_name: "smem",
        sar_header: "%smem",
        wire_name: "some_mem_total",
        unit: Unit::Percent,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "fmem_10",
        sar_header: "%fmem-10",
        wire_name: "full_amem_10",
        unit: Unit::Percent,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    ColumnMeta {
        public_name: "fmem_60",
        sar_header: "%fmem-60",
        wire_name: "full_amem_60",
        unit: Unit::Percent,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    ColumnMeta {
        public_name: "fmem_300",
        sar_header: "%fmem-300",
        wire_name: "full_amem_300",
        unit: Unit::Percent,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    ColumnMeta {
        public_name: "fmem",
        sar_header: "%fmem",
        wire_name: "full_mem_total",
        unit: Unit::Percent,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
];

// ===========================================================================
// 定義一覧
// ===========================================================================

/// この group の activity 定義。
///
/// `has_nr` は本家の `AO_COUNTED` に一致させる (§3.1)。
/// この 14 件のうち立つのは `A_CPU` / `A_IRQ` / `A_SERIAL` の 3 件だけ。
pub const DEFS: &[ActivityDef] = &[
    ActivityDef {
        id: ActivityId::CPU,
        revisions: CPU_REVISIONS,
        columns: CPU_COLUMNS,
        // CPU 数 + 1 個の item が並ぶ (index 0 = "all")
        shape: ItemShape::List,
        // CPU は名前フィールドを持たず添字で識別する (index - 1 = CPU 番号)
        item_key: "",
        has_nr: true,
    },
    ActivityDef {
        id: ActivityId::PCSW,
        revisions: PCSW_REVISIONS,
        columns: PCSW_COLUMNS,
        shape: ItemShape::Single,
        item_key: "",
        has_nr: false,
    },
    ActivityDef {
        id: ActivityId::IRQ,
        revisions: IRQ_REVISIONS,
        columns: IRQ_COLUMNS,
        // nr (CPU 数 + 1) 行 × nr2 (割り込み数) 列の行列。nr2 は file_activity の固定値
        shape: ItemShape::Matrix,
        // 割り込み名は行 0 (CPU "all") のみに入る。行 1 以降は空文字 (§6.2)
        item_key: "irq_name",
        has_nr: true,
    },
    ActivityDef {
        id: ActivityId::SWAP,
        revisions: SWAP_REVISIONS,
        columns: SWAP_COLUMNS,
        shape: ItemShape::Single,
        item_key: "",
        has_nr: false,
    },
    ActivityDef {
        id: ActivityId::PAGE,
        revisions: PAGE_REVISIONS,
        columns: PAGE_COLUMNS,
        shape: ItemShape::Single,
        item_key: "",
        has_nr: false,
    },
    ActivityDef {
        id: ActivityId::IO,
        revisions: IO_REVISIONS,
        columns: IO_COLUMNS,
        shape: ItemShape::Single,
        item_key: "",
        has_nr: false,
    },
    ActivityDef {
        id: ActivityId::MEMORY,
        revisions: MEMORY_REVISIONS,
        columns: MEMORY_COLUMNS,
        shape: ItemShape::Single,
        item_key: "",
        has_nr: false,
    },
    ActivityDef {
        id: ActivityId::KTABLES,
        revisions: KTABLES_REVISIONS,
        columns: KTABLES_COLUMNS,
        shape: ItemShape::Single,
        item_key: "",
        has_nr: false,
    },
    ActivityDef {
        id: ActivityId::QUEUE,
        revisions: QUEUE_REVISIONS,
        columns: QUEUE_COLUMNS,
        shape: ItemShape::Single,
        item_key: "",
        has_nr: false,
    },
    ActivityDef {
        id: ActivityId::SERIAL,
        revisions: SERIAL_REVISIONS,
        columns: SERIAL_COLUMNS,
        // TTY 回線数だけ item が並ぶ
        shape: ItemShape::List,
        // 同定キーは数値の `line` (u32) であって文字列ではないため item_key は空。
        // 文字列として読むと壊れるので、識別は `line` 列 (ValueKind::Identity) で行う
        item_key: "",
        has_nr: true,
    },
    ActivityDef {
        id: ActivityId::HUGE,
        revisions: HUGE_REVISIONS,
        columns: HUGE_COLUMNS,
        shape: ItemShape::Single,
        item_key: "",
        has_nr: false,
    },
    // PSI 3 件は AO_DETECTED であり AO_COUNTED ではないので has_nr = false
    ActivityDef {
        id: ActivityId::PSI_CPU,
        revisions: PSI_CPU_REVISIONS,
        columns: PSI_CPU_COLUMNS,
        shape: ItemShape::Single,
        item_key: "",
        has_nr: false,
    },
    ActivityDef {
        id: ActivityId::PSI_IO,
        revisions: PSI_IO_REVISIONS,
        columns: PSI_IO_COLUMNS,
        shape: ItemShape::Single,
        item_key: "",
        has_nr: false,
    },
    ActivityDef {
        id: ActivityId::PSI_MEM,
        revisions: PSI_MEM_REVISIONS,
        columns: PSI_MEM_COLUMNS,
        shape: ItemShape::Single,
        item_key: "",
        has_nr: false,
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::abi::{Endian, LayoutAbi, SourceEncoding};

    fn lp64() -> SourceEncoding {
        SourceEncoding::new(Endian::Little, LayoutAbi::LP64)
    }

    fn def(id: ActivityId) -> &'static ActivityDef {
        DEFS.iter().find(|d| d.id == id).expect("定義があること")
    }

    #[test]
    fn all_fourteen_system_activities_are_defined() {
        let expected = [
            ActivityId::CPU,
            ActivityId::PCSW,
            ActivityId::IRQ,
            ActivityId::SWAP,
            ActivityId::PAGE,
            ActivityId::IO,
            ActivityId::MEMORY,
            ActivityId::KTABLES,
            ActivityId::QUEUE,
            ActivityId::SERIAL,
            ActivityId::HUGE,
            ActivityId::PSI_CPU,
            ActivityId::PSI_IO,
            ActivityId::PSI_MEM,
        ];
        assert_eq!(DEFS.len(), expected.len());
        for id in expected {
            assert!(
                DEFS.iter().any(|d| d.id == id),
                "{} が未登録",
                id.display_name()
            );
        }
    }

    /// `has_nr` は本家の `AO_COUNTED` に一致する。PSI 3 件は `AO_DETECTED` なので立たない。
    #[test]
    fn has_nr_matches_ao_counted() {
        for d in DEFS {
            let expected = matches!(d.id, ActivityId::CPU | ActivityId::IRQ | ActivityId::SERIAL);
            assert_eq!(d.has_nr, expected, "{}: has_nr", d.id.display_name());
        }
    }

    /// 旧世代のファイルを magic で引けること (実データで確認済みの組み合わせ)。
    ///
    /// `A_IO` / `A_MEMORY` / `A_PAGE` は同一 magic に複数サイズが対応するため、
    /// magic だけでは確定しない。その場合はサイズを併記して照合する。
    #[test]
    fn legacy_revisions_are_reachable_by_magic() {
        // (activity, magic, 実データに現れた file_activity.size)
        let cases: &[(ActivityId, u32, usize)] = &[
            (ActivityId::CPU, 0x8a, 160),
            (ActivityId::IO, 0x8b, 48),
            (ActivityId::MEMORY, 0x8a, 88),
            (ActivityId::QUEUE, 0x8b, 32),
            (ActivityId::PAGE, 0x8a, 64),
            (ActivityId::SWAP, 0x8a, 16),
            (ActivityId::PCSW, 0x8a, 32),
            (ActivityId::KTABLES, 0x8a, 16),
        ];
        for (id, magic, size) in cases {
            let d = def(*id);
            assert!(
                d.revision_for_magic(*magic).is_some(),
                "{}: magic 0x{magic:x} が引けない",
                id.display_name()
            );
            assert!(
                d.revisions
                    .iter()
                    .any(|r| r.magic == *magic && r.size_lp64 == *size),
                "{}: magic 0x{magic:x} / size {size} の revision が無い",
                id.display_name()
            );
        }

        // A_HUGE は STATS_HUGE_SIZE バグにより「申告 88 / 実サイズ 16」になる (§9.5)
        let huge = def(ActivityId::HUGE);
        let rev = huge.revision_for_magic(0x8a).expect("時代 A の revision");
        assert_eq!(rev.size_lp64, 16, "A_HUGE の実サイズは 16 バイト");
    }

    /// `stats_queue` だけは i386 でサイズが変わる (§1.3)。
    #[test]
    fn queue_shrinks_on_i386_only() {
        let i386 = SourceEncoding::new(Endian::Little, LayoutAbi::I386);
        let rev = def(ActivityId::QUEUE).latest().unwrap();
        assert_eq!(rev.layout.resolve(&lp64()).unwrap().size, 40);
        assert_eq!(rev.layout.resolve(&i386).unwrap().size, 36);

        // 他の activity は ABI をまたいでサイズが一致する
        for d in DEFS {
            if d.id == ActivityId::QUEUE {
                continue;
            }
            for r in d.revisions {
                assert_eq!(
                    r.layout.resolve(&lp64()).unwrap().size,
                    r.layout.resolve(&i386).unwrap().size,
                    "{} (magic=0x{:x}): ABI でサイズが変わってはいけない",
                    d.id.display_name(),
                    r.magic
                );
            }
        }
    }

    /// 主要なフィールドオフセットを 02-activities.md §5 の表と突合する。
    #[test]
    fn offsets_match_documented_tables() {
        let enc = lp64();
        let cases: &[(ActivityId, &str, usize)] = &[
            // stats_cpu: /proc/stat の列順とは違い steal が 6 番目
            (ActivityId::CPU, "cpu_steal", 40),
            (ActivityId::CPU, "cpu_hardirq", 48),
            (ActivityId::CPU, "cpu_guest_nice", 72),
            (ActivityId::PCSW, "processes", 8),
            (ActivityId::IRQ, "irq_name", 4),
            (ActivityId::SWAP, "pswpout", 8),
            (ActivityId::PAGE, "pgdemote", 72),
            (ActivityId::IO, "dk_drive_dblk", 48),
            (ActivityId::MEMORY, "availablekb", 128),
            (ActivityId::MEMORY, "shmemkb", 136),
            (ActivityId::KTABLES, "pty_nr", 24),
            (ActivityId::QUEUE, "load_avg_15", 32),
            (ActivityId::SERIAL, "line", 24),
            (ActivityId::HUGE, "surphkb", 24),
            (ActivityId::PSI_CPU, "some_acpu_300", 24),
            (ActivityId::PSI_IO, "full_aio_300", 56),
            (ActivityId::PSI_MEM, "full_amem_300", 56),
        ];
        for (id, field, offset) in cases {
            let resolved = def(*id).latest().unwrap().layout.resolve(&enc).unwrap();
            let f = resolved
                .field(field)
                .unwrap_or_else(|| panic!("{}: {field} が無い", id.display_name()));
            assert_eq!(
                f.offset,
                *offset,
                "{}: {field} のオフセット",
                id.display_name()
            );
        }
    }

    /// 時代 A の `aligned(16)` による穴が再現できていること。
    #[test]
    fn legacy_alignment_holes_are_reproduced() {
        let enc = lp64();
        let cpu = def(ActivityId::CPU)
            .revisions
            .iter()
            .find(|r| r.size_lp64 == 160)
            .unwrap();
        let resolved = cpu.layout.resolve(&enc).unwrap();
        assert_eq!(resolved.field("cpu_nice").unwrap().offset, 16);
        assert_eq!(resolved.field("cpu_guest_nice").unwrap().offset, 144);

        // stats_io は先頭だけ aligned(16) なので中身は 8 バイト刻み、末尾に 8 バイトの穴
        let io = def(ActivityId::IO)
            .revisions
            .iter()
            .find(|r| r.size_lp64 == 48)
            .unwrap();
        let resolved = io.layout.resolve(&enc).unwrap();
        assert_eq!(resolved.field("dk_drive_rio").unwrap().offset, 8);
        assert_eq!(resolved.field("dk_drive_wblk").unwrap().offset, 32);
        assert_eq!(resolved.size, 48);
    }

    /// 時代 A の `stats_queue` は `nr_threads` が `load_avg_*` の後ろにある (§9.3)。
    #[test]
    fn legacy_queue_has_different_field_order() {
        let enc = lp64();
        let rev = def(ActivityId::QUEUE)
            .revisions
            .iter()
            .find(|r| r.magic == 0x8b)
            .unwrap();
        let resolved = rev.layout.resolve(&enc).unwrap();
        assert_eq!(resolved.field("load_avg_1").unwrap().offset, 16);
        assert_eq!(resolved.field("nr_threads").unwrap().offset, 28);
        assert_eq!(resolved.size, 32);
    }
}

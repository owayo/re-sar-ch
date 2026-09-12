//! storage 系 activity の定義。
//!
//! 対象は `A_DISK` (11) と `A_FS` (37)。
//!
//! フィールド順・アラインメント・`size_lp64` / `types_nr` の出所は
//! `docs/format/02-activities.md` §5 (オフセット表) と §9.3 (activity 別差分表)。
//! 旧世代 (時代 A = `FORMAT_MAGIC` `0x2171` / `0x2173`) の構造体サイズ・
//! アラインメント・`unsigned long` 本数は `docs/format/01-file-format.md`
//! §5.10 / §5.12 の実測表から写している。
//!
//! `unsigned long` は `aligned(8)` によりファイル上**常に 8 バイトのスロット**を占め、
//! 32bit ライタでは先頭 4 バイトだけが有効 (§1.1)。よってドキュメントで
//! `unsigned long` とされているフィールドは必ず [`FieldTy::CULong`] を使う。

use crate::format::wire::{FieldTy, WireField, WireLayout};
use crate::layout::registry::{ActivityDef, ColumnMeta, ItemShape, WireRevision};
use crate::model::{ActivityId, Aggregation, Unit, ValueKind};

// ===== A_DISK (11) — sar -d =====
//
// magic の履歴 (§3.3): `0x8a` @v9.1.6 → `0x8b` @v10.1.1 → `0x8c` @v11.7.2。
// ディスク上に現れるのはこの 3 値のみ。
//
// 同じ `0x8c` のまま 3 度サイズが変わっている (§9.3) ため、
// magic だけでは revision を決められない。`types_nr` (自己記述形式) と
// `file_activity.size` で絞り込む前提で 3 つとも定義する。

/// 現行 (v12.1.7 〜): `wwn[2]` と `part_nr` が入った 80 バイト版。
///
/// `wwn` は C では `unsigned long long wwn[2]` の配列だが、`types_nr[0]` には
/// 2 本として計上される (= (3,3,8) の 3 は `nr_ios` + `wwn[0]` + `wwn[1]`)。
/// レイアウト記述でも 2 フィールドに展開する。
const DISK_FIELDS_80: &[WireField] = &[
    WireField::natural("nr_ios", FieldTy::U64),
    WireField::natural("wwn_0", FieldTy::U64),
    WireField::natural("wwn_1", FieldTy::U64),
    // unsigned long 群。時代 B でも `aligned(8)` だけは残っている。
    WireField::aligned("rd_sect", FieldTy::CULong, 8),
    WireField::aligned("wr_sect", FieldTy::CULong, 8),
    WireField::aligned("dc_sect", FieldTy::CULong, 8),
    WireField::natural("rd_ticks", FieldTy::U32),
    WireField::natural("wr_ticks", FieldTy::U32),
    WireField::natural("tot_ticks", FieldTy::U32),
    WireField::natural("rq_ticks", FieldTy::U32),
    WireField::natural("major", FieldTy::U32),
    WireField::natural("minor", FieldTy::U32),
    WireField::natural("dc_ticks", FieldTy::U32),
    WireField::natural("part_nr", FieldTy::U32),
];

/// v12.1.2 〜 v12.1.6: discard 系 (`dc_sect` / `dc_ticks`) が入った 64 バイト版。
/// `dc_sect` は idx 3、`dc_ticks` は末尾に置かれた (§9.7 の通り、開発中は idx 6 だった)。
const DISK_FIELDS_64: &[WireField] = &[
    WireField::natural("nr_ios", FieldTy::U64),
    WireField::aligned("rd_sect", FieldTy::CULong, 8),
    WireField::aligned("wr_sect", FieldTy::CULong, 8),
    WireField::aligned("dc_sect", FieldTy::CULong, 8),
    WireField::natural("rd_ticks", FieldTy::U32),
    WireField::natural("wr_ticks", FieldTy::U32),
    WireField::natural("tot_ticks", FieldTy::U32),
    WireField::natural("rq_ticks", FieldTy::U32),
    WireField::natural("major", FieldTy::U32),
    WireField::natural("minor", FieldTy::U32),
    WireField::natural("dc_ticks", FieldTy::U32),
];

/// v11.7.2 〜 v12.1.1: 時代 B の初期形 (48 バイト、discard 無し)。
///
/// v11.7.1 が書いたファイルは**このレイアウトなのに magic が `0x8b`** という
/// 矛盾した状態になっている (§3.4 の magic 昇格漏れ)。救済する場合は
/// 「v11.7.1 が書いた」と判定してから magic に +1 してこの revision を引く。
/// そのため `0x8b` の項目としては定義しない (`0x8b` は時代 A の 64 バイト版に割り当てる)。
const DISK_FIELDS_48: &[WireField] = &[
    WireField::natural("nr_ios", FieldTy::U64),
    WireField::aligned("rd_sect", FieldTy::CULong, 8),
    WireField::aligned("wr_sect", FieldTy::CULong, 8),
    WireField::natural("rd_ticks", FieldTy::U32),
    WireField::natural("wr_ticks", FieldTy::U32),
    WireField::natural("tot_ticks", FieldTy::U32),
    WireField::natural("rq_ticks", FieldTy::U32),
    WireField::natural("major", FieldTy::U32),
    WireField::natural("minor", FieldTy::U32),
];

/// 時代 A (v10.1.1 〜 v11.6.6) の `stats_disk_8b`: 64 バイト / アラインメント 16 /
/// `unsigned long` 2 本 (01 §5.12)。実データでも sysstat 10.1.5 が
/// `magic = 0x8b` / `size = 64` を書いていることを確認済み。
///
/// 時代 A は全フィールドに `aligned(n)` / `packed` が手書きされていたため、
/// `nr_ios` の 16 バイト境界指定で先頭に 8 バイトの穴が空く。
const DISK_FIELDS_8B: &[WireField] = &[
    WireField::aligned("nr_ios", FieldTy::U64, 16),
    WireField::aligned("rd_sect", FieldTy::CULong, 16),
    WireField::aligned("wr_sect", FieldTy::CULong, 8),
    WireField::aligned("rd_ticks", FieldTy::U32, 8),
    WireField::packed("wr_ticks", FieldTy::U32),
    WireField::packed("tot_ticks", FieldTy::U32),
    WireField::packed("rq_ticks", FieldTy::U32),
    WireField::packed("major", FieldTy::U32),
    WireField::packed("minor", FieldTy::U32),
];

/// 時代 A (v9.1.6 〜 v10.0.5) の `stats_disk_8a`: 80 バイト / アラインメント 16 /
/// `unsigned long` 5 本 (01 §5.12)。
///
/// 現行と同じ 80 バイトだが**フィールドの型と並びがまったく違う**点に注意
/// (`rd_sect` / `wr_sect` が ULL、tick 群が UL、`nr_ios` が UL で末尾寄り)。
/// v10.1.1 の改造で `nr_ios` が先頭へ移り ULL 化した (§9.4)。
const DISK_FIELDS_8A: &[WireField] = &[
    WireField::aligned("rd_sect", FieldTy::U64, 16),
    WireField::aligned("wr_sect", FieldTy::U64, 16),
    WireField::aligned("rd_ticks", FieldTy::CULong, 16),
    WireField::aligned("wr_ticks", FieldTy::CULong, 8),
    WireField::aligned("tot_ticks", FieldTy::CULong, 8),
    WireField::aligned("rq_ticks", FieldTy::CULong, 8),
    WireField::aligned("nr_ios", FieldTy::CULong, 8),
    WireField::aligned("major", FieldTy::U32, 8),
    WireField::packed("minor", FieldTy::U32),
];

const DISK_REVISIONS: &[WireRevision] = &[
    WireRevision {
        magic: 0x8c,
        types_nr: [3, 3, 8],
        size_lp64: 80,
        layout: WireLayout::new("stats_disk@0x8c/80", DISK_FIELDS_80),
        since: "12.1.7",
    },
    WireRevision {
        magic: 0x8c,
        types_nr: [1, 3, 7],
        size_lp64: 64,
        layout: WireLayout::new("stats_disk@0x8c/64", DISK_FIELDS_64),
        since: "12.1.2",
    },
    WireRevision {
        magic: 0x8c,
        types_nr: [1, 2, 6],
        size_lp64: 48,
        layout: WireLayout::new("stats_disk@0x8c/48", DISK_FIELDS_48),
        since: "11.7.2",
    },
    // ---- 以下は時代 A (旧 FORMAT_MAGIC)。types_nr はファイルに存在しないため、
    //      ここでの宣言値はレイアウト記述との整合性検査のためだけに使う。
    //      判別は magic と file_activity.size で行う。
    WireRevision {
        magic: 0x8b,
        types_nr: [1, 2, 6],
        size_lp64: 64,
        layout: WireLayout::new("stats_disk_8b@0x8b/64", DISK_FIELDS_8B),
        since: "10.1.1",
    },
    WireRevision {
        magic: 0x8a,
        types_nr: [2, 5, 2],
        size_lp64: 80,
        layout: WireLayout::new("stats_disk_8a@0x8a/80", DISK_FIELDS_8A),
        since: "9.1.6",
    },
];

/// `sar -d` の列。`hdr_line` = `DEV;tps;rkB/s;wkB/s;dkB/s;areq-sz;aqu-sz;await;%util`
/// (03 §id=11)。
///
/// 表示側のスケーリング (計算は series 層):
/// - `tps`     = `S_VALUE(nr_ios)`
/// - `rkB/s`   = `S_VALUE(rd_sect) / 2`    (1 セクタ = 512 B = 0.5 kB)
/// - `wkB/s`   = `S_VALUE(wr_sect) / 2`
/// - `dkB/s`   = `S_VALUE(dc_sect) / 2`
/// - `areq-sz` = `arqsz / 2`               (`arqsz` = Δsect 合計 / Δnr_ios、セクタ → kB)
/// - `aqu-sz`  = `S_VALUE(rq_ticks) / 1000`
/// - `await`   = `(Δrd_ticks + Δwr_ticks + Δdc_ticks) / Δnr_ios` (ms、追加スケーリング無し)
/// - `%util`   = `S_VALUE(tot_ticks) / 10` (tot_ticks は ms 累積。1000 ms/s = 100 %)
const DISK_COLUMNS: &[ColumnMeta] = &[
    // デバイス名は wire に無い。get_device_name(major, minor, wwn, part_nr) 相当で解決する。
    ColumnMeta {
        public_name: "device",
        sar_header: "DEV",
        wire_name: "",
        unit: Unit::Identifier,
        kind: ValueKind::Identity,
        aggregation: Aggregation::NotAggregated,
    },
    ColumnMeta {
        public_name: "tps",
        sar_header: "tps",
        wire_name: "nr_ios",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "read_kb_per_sec",
        sar_header: "rkB/s",
        wire_name: "rd_sect",
        unit: Unit::KilobytesPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "write_kb_per_sec",
        sar_header: "wkB/s",
        wire_name: "wr_sect",
        unit: Unit::KilobytesPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "discard_kb_per_sec",
        sar_header: "dkB/s",
        wire_name: "dc_sect",
        unit: Unit::KilobytesPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    // 派生列: (Δrd_sect + Δwr_sect + Δdc_sect) / Δnr_ios / 2
    ColumnMeta {
        public_name: "avg_request_size",
        sar_header: "areq-sz",
        wire_name: "",
        unit: Unit::Kilobytes,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    // rq_ticks は「I/O 待ちの重み付きミリ秒」。/1000 で平均キュー長 (無次元) になる。
    ColumnMeta {
        public_name: "avg_queue_size",
        sar_header: "aqu-sz",
        wire_name: "rq_ticks",
        unit: Unit::None,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    // 派生列: (Δrd_ticks + Δwr_ticks + Δdc_ticks) / Δnr_ios
    ColumnMeta {
        public_name: "await",
        sar_header: "await",
        wire_name: "",
        unit: Unit::Milliseconds,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    ColumnMeta {
        public_name: "util_pct",
        sar_header: "%util",
        wire_name: "tot_ticks",
        unit: Unit::Percent,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    // ---- 以下は sar に単独の列として現れない内部フィールド ----
    ColumnMeta {
        public_name: "read_ticks",
        sar_header: "",
        wire_name: "rd_ticks",
        unit: Unit::Milliseconds,
        kind: ValueKind::Counter,
        aggregation: Aggregation::Sum,
    },
    ColumnMeta {
        public_name: "write_ticks",
        sar_header: "",
        wire_name: "wr_ticks",
        unit: Unit::Milliseconds,
        kind: ValueKind::Counter,
        aggregation: Aggregation::Sum,
    },
    ColumnMeta {
        public_name: "discard_ticks",
        sar_header: "",
        wire_name: "dc_ticks",
        unit: Unit::Milliseconds,
        kind: ValueKind::Counter,
        aggregation: Aggregation::Sum,
    },
    ColumnMeta {
        public_name: "major",
        sar_header: "",
        wire_name: "major",
        unit: Unit::Identifier,
        kind: ValueKind::Identity,
        aggregation: Aggregation::NotAggregated,
    },
    ColumnMeta {
        public_name: "minor",
        sar_header: "",
        wire_name: "minor",
        unit: Unit::Identifier,
        kind: ValueKind::Identity,
        aggregation: Aggregation::NotAggregated,
    },
    // wwn[0] == 0 なら永続 ID を取得できていない (§5 の注記)。
    ColumnMeta {
        public_name: "wwn_high",
        sar_header: "",
        wire_name: "wwn_0",
        unit: Unit::Identifier,
        kind: ValueKind::Identity,
        aggregation: Aggregation::NotAggregated,
    },
    ColumnMeta {
        public_name: "wwn_low",
        sar_header: "",
        wire_name: "wwn_1",
        unit: Unit::Identifier,
        kind: ValueKind::Identity,
        aggregation: Aggregation::NotAggregated,
    },
    ColumnMeta {
        public_name: "partition_nr",
        sar_header: "",
        wire_name: "part_nr",
        unit: Unit::Identifier,
        kind: ValueKind::Identity,
        aggregation: Aggregation::NotAggregated,
    },
];

// ===== A_FS (37) — sar -F / -F MOUNT =====
//
// magic の履歴 (§3.3): v10.1.6 で `0x8a` として新設 → `0x8b` @v11.7.2
// (同時に `A_FILESYSTEM` → `A_FS` に改名。構造体名は `stats_filesystem` のまま)。

/// 現行 (v11.7.2 〜): `aligned(16)` を撤去した 296 バイト版。`MAX_FS_LEN` = 128。
const FS_FIELDS_296: &[WireField] = &[
    WireField::natural("f_blocks", FieldTy::U64),
    WireField::natural("f_bfree", FieldTy::U64),
    WireField::natural("f_bavail", FieldTy::U64),
    WireField::natural("f_files", FieldTy::U64),
    WireField::natural("f_ffree", FieldTy::U64),
    WireField::natural("fs_name", FieldTy::Bytes(MAX_FS_LEN)),
    WireField::natural("mountp", FieldTy::Bytes(MAX_FS_LEN)),
];

/// 時代 A (v11.1.4 〜 v11.6.6) の `stats_filesystem_8a`: 336 バイト /
/// アラインメント 16 (01 §5.12)。`mountp` 追加と `MAX_FS_LEN` 72 → 128 が同時に入った版。
const FS_FIELDS_336: &[WireField] = &[
    WireField::aligned("f_blocks", FieldTy::U64, 16),
    WireField::aligned("f_bfree", FieldTy::U64, 16),
    WireField::aligned("f_bavail", FieldTy::U64, 16),
    WireField::aligned("f_files", FieldTy::U64, 16),
    WireField::aligned("f_ffree", FieldTy::U64, 16),
    WireField::aligned("fs_name", FieldTy::Bytes(MAX_FS_LEN), 16),
    WireField::aligned("mountp", FieldTy::Bytes(MAX_FS_LEN), 16),
];

/// 時代 A (v10.1.6 〜 v11.1.3) の初出版: 160 バイト。
/// `mountp` が無く `MAX_FS_LEN` = 72 (01 §5.10)。
///
/// `MAX_FS_LEN` の 72 → 128 は magic を上げずに行われたため、
/// 時代 A のファイルでは `size <= 160` かどうかで `mountp` の有無を判定するしかない
/// (§11.2-27)。
const FS_FIELDS_160: &[WireField] = &[
    WireField::aligned("f_blocks", FieldTy::U64, 16),
    WireField::aligned("f_bfree", FieldTy::U64, 16),
    WireField::aligned("f_bavail", FieldTy::U64, 16),
    WireField::aligned("f_files", FieldTy::U64, 16),
    WireField::aligned("f_ffree", FieldTy::U64, 16),
    WireField::aligned("fs_name", FieldTy::Bytes(MAX_FS_LEN_OLD), 16),
];

/// `MAX_FS_LEN` (v11.1.4 以降。§10.1)。
const MAX_FS_LEN: u16 = 128;
/// `MAX_FS_LEN` (v10.1.6 の新設時。v11.1.3 まで)。
const MAX_FS_LEN_OLD: u16 = 72;

const FS_REVISIONS: &[WireRevision] = &[
    WireRevision {
        magic: 0x8b,
        types_nr: [5, 0, 0],
        size_lp64: 296,
        layout: WireLayout::new("stats_filesystem@0x8b/296", FS_FIELDS_296),
        since: "11.7.2",
    },
    // ---- 時代 A。どちらも magic は 0x8a なので file_activity.size で判別する。
    WireRevision {
        magic: 0x8a,
        types_nr: [5, 0, 0],
        size_lp64: 336,
        layout: WireLayout::new("stats_filesystem_8a@0x8a/336", FS_FIELDS_336),
        since: "11.1.4",
    },
    WireRevision {
        magic: 0x8a,
        types_nr: [5, 0, 0],
        size_lp64: 160,
        layout: WireLayout::new("stats_filesystem_8a@0x8a/160", FS_FIELDS_160),
        since: "10.1.6",
    },
];

/// `sar -F` / `sar -F MOUNT` の列 (03 §id=37)。`hdr_line` は `|` 区切りの 2 系統:
/// `FILESYSTEM;MBfsfree;MBfsused;%fsused;%ufsused;Ifree;Iused;%Iused`
/// `MOUNTPOINT;MBfsfree;MBfsused;%fsused;%ufsused;Ifree;Iused;%Iused`
/// (`AO_MULTIPLE_OUTPUTS`。`-F` はデバイス名、`-F MOUNT` はマウントポイントを表示)。
///
/// 表示側のスケーリング (計算は series 層。`f_*` はすべて**バイト**単位):
/// - `MBfsfree`  = `f_bfree / 1024 / 1024`            (`--human` 時はバイトのまま渡す)
/// - `MBfsused`  = `(f_blocks - f_bfree) / 1024 / 1024`
/// - `%fsused`   = `(f_blocks - f_bfree) / f_blocks * 100`   (`f_blocks == 0` なら 0)
/// - `%ufsused`  = `(f_blocks - f_bavail) / f_blocks * 100`  (同)
/// - `Ifree`     = `f_ffree`
/// - `Iused`     = `f_files - f_ffree`
/// - `%Iused`    = `(f_files - f_ffree) / f_files * 100`     (`f_files == 0` なら 0)
///
/// 集計は [`Aggregation::Last`]。A_FS は**期間平均を取らない** activity で、
/// `Summary:` / `Last:` 行は各ファイルシステムについて
/// 「最後に観測された値」をそのまま再掲する (03 §id=37)。
const FS_COLUMNS: &[ColumnMeta] = &[
    ColumnMeta {
        public_name: "filesystem",
        sar_header: "FILESYSTEM",
        wire_name: "fs_name",
        unit: Unit::Identifier,
        kind: ValueKind::Identity,
        aggregation: Aggregation::NotAggregated,
    },
    ColumnMeta {
        public_name: "mountpoint",
        sar_header: "MOUNTPOINT",
        wire_name: "mountp",
        unit: Unit::Identifier,
        kind: ValueKind::Identity,
        aggregation: Aggregation::NotAggregated,
    },
    ColumnMeta {
        public_name: "fs_free",
        sar_header: "MBfsfree",
        wire_name: "f_bfree",
        unit: Unit::Bytes,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Last,
    },
    // 派生列: f_blocks - f_bfree
    ColumnMeta {
        public_name: "fs_used",
        sar_header: "MBfsused",
        wire_name: "",
        unit: Unit::Bytes,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Last,
    },
    // 派生列: (f_blocks - f_bfree) / f_blocks * 100
    ColumnMeta {
        public_name: "fs_used_pct",
        sar_header: "%fsused",
        wire_name: "",
        unit: Unit::Percent,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Last,
    },
    // 派生列: (f_blocks - f_bavail) / f_blocks * 100 (非特権ユーザ視点)
    ColumnMeta {
        public_name: "fs_used_pct_unpriv",
        sar_header: "%ufsused",
        wire_name: "",
        unit: Unit::Percent,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Last,
    },
    ColumnMeta {
        public_name: "inodes_free",
        sar_header: "Ifree",
        wire_name: "f_ffree",
        unit: Unit::Count,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Last,
    },
    // 派生列: f_files - f_ffree
    ColumnMeta {
        public_name: "inodes_used",
        sar_header: "Iused",
        wire_name: "",
        unit: Unit::Count,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Last,
    },
    // 派生列: (f_files - f_ffree) / f_files * 100
    ColumnMeta {
        public_name: "inodes_used_pct",
        sar_header: "%Iused",
        wire_name: "",
        unit: Unit::Percent,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Last,
    },
    // ---- 以下は sar に単独の列として現れない内部フィールド ----
    ColumnMeta {
        public_name: "fs_total",
        sar_header: "",
        wire_name: "f_blocks",
        unit: Unit::Bytes,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Last,
    },
    ColumnMeta {
        public_name: "fs_available",
        sar_header: "",
        wire_name: "f_bavail",
        unit: Unit::Bytes,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Last,
    },
    ColumnMeta {
        public_name: "inodes_total",
        sar_header: "",
        wire_name: "f_files",
        unit: Unit::Count,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Last,
    },
];

/// この group の activity 定義。
pub const DEFS: &[ActivityDef] = &[
    ActivityDef {
        id: ActivityId::DISK,
        revisions: DISK_REVISIONS,
        columns: DISK_COLUMNS,
        shape: ItemShape::List,
        // A_DISK は名前フィールドを持たない。同定キーは major / minor (+ wwn / part_nr) で、
        // デバイス名は後段で解決する (§4.3)。文字列フィールドが無いので item_key は空。
        item_key: "",
        // AO_COUNTED (§3.1)。毎サンプル統計の直前に i32 の item 数が入る。
        has_nr: true,
    },
    ActivityDef {
        id: ActivityId::FS,
        revisions: FS_REVISIONS,
        columns: FS_COLUMNS,
        shape: ItemShape::List,
        // 同定キーはデバイス名 (§4.2)。`-F MOUNT` の表示だけが mountp に変わる。
        item_key: "fs_name",
        // AO_COUNTED (§3.1)
        has_nr: true,
    },
];

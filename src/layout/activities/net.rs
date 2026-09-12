//! network 系 activity の定義。
//!
//! 対象は `A_NET_DEV` (12) 〜 `A_NET_UDP6` (29)、`A_NET_FC` (38)、`A_NET_SOFT` (39) の 20 件。
//!
//! ## レイアウト記述の共通規則
//!
//! - フィールド順・配列長・`size_lp64` / `types_nr` は `docs/format/02-activities.md` §5 の
//!   実測オフセット表 (x86_64 / aarch64 / i686 / armv7 の 4 ABI) を写したもの。
//! - `unsigned long` は [`FieldTy::CULong`] で表す。ファイル上は常に 8 バイトのスロットを
//!   占め (本家の `aligned(8)` = `UL_ALIGNMENT_WIDTH`)、32bit ライタでは先頭 4 バイトだけが
//!   有効になる。`CULong` の自然アラインメントが既に 8 なので、ソース側の `aligned(8)` を
//!   `WireField::aligned(.., 8)` として重ねて書く必要はない。
//! - 旧 revision (magic が現行より小さいもの) は `FORMAT_MAGIC` = `0x2171` / `0x2173`
//!   (sysstat v9.1.6 〜 v11.6.6、= 時代 A) のファイルに現れる。時代 A は全フィールドに
//!   `aligned(16)` / `aligned(8)` / `aligned(4)` を手で付けていたため、同じフィールド構成でも
//!   サイズが倍近くになる。**文字列フィールドにもアラインメント指定が付く**ので、
//!   直前のカウンタの終端にそのまま続くとは限らない (`stats_net_dev_8b` の `interface` は
//!   104 ではなく 112)。旧サイズの根拠は
//!   `docs/format/01-file-format.md` §5.10 の実測表 (`tests/data-9.1.6` / `data-10.3.1` /
//!   `data-11.6.5` から読んだ `file_activity.size`)。アラインメント指定の配り方は
//!   本家が変換用に保持している `stats_*_8a` / `_8b` / `_8c` と同じ配置になるよう選び、
//!   導出した全オフセットを C の `offsetof` で突合済み。
//! - 時代 A の `file_activity` には `types_nr` が無い。旧 revision の `types_nr` は
//!   レイアウト記述から導出した情報値であり、`revision_for_magic` での判別が本筋になる。

use crate::format::wire::{FieldTy, WireField, WireLayout};
use crate::layout::registry::{ActivityDef, ColumnMeta, ItemShape, WireRevision};
use crate::model::{ActivityId, Aggregation, Unit, ValueKind};

/// インターフェース名の配列長 (`MAX_IFACE_LEN`)。v12.5.3 以降はリテラル 16、
/// それ以前は `IFNAMSIZ` 経由だが Linux では同じ 16。
const MAX_IFACE_LEN: u16 = 16;

/// FC ホスト名の配列長 (`MAX_FCH_LEN`)。
const MAX_FCH_LEN: u16 = 16;

// ===== A_NET_DEV (12) =====

/// 現行 (magic `0x8d` = `BASE+3`、v11.7.2 以降。レイアウト自体は v11.7.1 で確定)。
///
/// ULL 7 本 → `speed` (u32) → `interface[16]` → `duplex` (char) の順。
/// `speed` が 56、`interface` が 60、`duplex` が 76 で末尾パディング 3 バイト → 80。
const NET_DEV_FIELDS: &[WireField] = &[
    WireField::natural("rx_packets", FieldTy::U64),
    WireField::natural("tx_packets", FieldTy::U64),
    WireField::natural("rx_bytes", FieldTy::U64),
    WireField::natural("tx_bytes", FieldTy::U64),
    WireField::natural("rx_compressed", FieldTy::U64),
    WireField::natural("tx_compressed", FieldTy::U64),
    WireField::natural("multicast", FieldTy::U64),
    WireField::natural("speed", FieldTy::U32),
    WireField::natural("interface", FieldTy::Bytes(MAX_IFACE_LEN)),
    WireField::natural("duplex", FieldTy::U8),
];

/// magic `0x8c` (v10.1.7 〜 v11.7.1)。`speed` / `duplex` が追加された時代 A の形。
///
/// ULL 7 本と `speed` が `aligned(16)`、`interface` が `aligned(4)`。
/// `multicast` が 96 で終端 104 → `speed` が 112 → `interface` が 116 →
/// `duplex` が 132 で、末尾パディングを足して 144。
const NET_DEV_FIELDS_8C: &[WireField] = &[
    WireField::aligned("rx_packets", FieldTy::U64, 16),
    WireField::aligned("tx_packets", FieldTy::U64, 16),
    WireField::aligned("rx_bytes", FieldTy::U64, 16),
    WireField::aligned("tx_bytes", FieldTy::U64, 16),
    WireField::aligned("rx_compressed", FieldTy::U64, 16),
    WireField::aligned("tx_compressed", FieldTy::U64, 16),
    WireField::aligned("multicast", FieldTy::U64, 16),
    WireField::aligned("speed", FieldTy::U32, 16),
    WireField::aligned("interface", FieldTy::Bytes(MAX_IFACE_LEN), 4),
    WireField::natural("duplex", FieldTy::U8),
];

/// magic `0x8b` (v10.1.3 〜 v10.1.6)。カウンタが `unsigned long` → ULL へ拡幅され
/// `aligned(16)` が付いた版。`speed` / `duplex` はまだ無い。
/// `interface` にも `aligned(16)` が付くため、その位置は 104 ではなく **112**。112 + 16 = 128。
const NET_DEV_FIELDS_8B: &[WireField] = &[
    WireField::aligned("rx_packets", FieldTy::U64, 16),
    WireField::aligned("tx_packets", FieldTy::U64, 16),
    WireField::aligned("rx_bytes", FieldTy::U64, 16),
    WireField::aligned("tx_bytes", FieldTy::U64, 16),
    WireField::aligned("rx_compressed", FieldTy::U64, 16),
    WireField::aligned("tx_compressed", FieldTy::U64, 16),
    WireField::aligned("multicast", FieldTy::U64, 16),
    WireField::aligned("interface", FieldTy::Bytes(MAX_IFACE_LEN), 16),
];

/// magic `0x8a` (v9.1.6 〜 v10.1.2)。カウンタが `unsigned long` (8 バイトスロット)。
/// `interface` は `aligned(8)` で 56 に載る。56 + 16 = 72。
const NET_DEV_FIELDS_8A: &[WireField] = &[
    WireField::natural("rx_packets", FieldTy::CULong),
    WireField::natural("tx_packets", FieldTy::CULong),
    WireField::natural("rx_bytes", FieldTy::CULong),
    WireField::natural("tx_bytes", FieldTy::CULong),
    WireField::natural("rx_compressed", FieldTy::CULong),
    WireField::natural("tx_compressed", FieldTy::CULong),
    WireField::natural("multicast", FieldTy::CULong),
    WireField::aligned("interface", FieldTy::Bytes(MAX_IFACE_LEN), 8),
];

const NET_DEV_REVISIONS: &[WireRevision] = &[
    WireRevision {
        magic: 0x8d,
        types_nr: [7, 0, 1],
        size_lp64: 80,
        layout: WireLayout::new("stats_net_dev@0x8d", NET_DEV_FIELDS),
        since: "11.7.2",
    },
    WireRevision {
        magic: 0x8c,
        types_nr: [7, 0, 1],
        size_lp64: 144,
        layout: WireLayout::new("stats_net_dev@0x8c", NET_DEV_FIELDS_8C),
        since: "10.1.7",
    },
    WireRevision {
        magic: 0x8b,
        types_nr: [7, 0, 0],
        size_lp64: 128,
        layout: WireLayout::new("stats_net_dev@0x8b", NET_DEV_FIELDS_8B),
        since: "10.1.3",
    },
    WireRevision {
        magic: 0x8a,
        types_nr: [0, 7, 0],
        size_lp64: 72,
        layout: WireLayout::new("stats_net_dev@0x8a", NET_DEV_FIELDS_8A),
        since: "9.1.6",
    },
];

const NET_DEV_COLUMNS: &[ColumnMeta] = &[
    ColumnMeta {
        public_name: "interface",
        sar_header: "IFACE",
        wire_name: "interface",
        unit: Unit::Identifier,
        kind: ValueKind::Identity,
        aggregation: Aggregation::NotAggregated,
    },
    ColumnMeta {
        public_name: "rxpck_per_sec",
        sar_header: "rxpck/s",
        wire_name: "rx_packets",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "txpck_per_sec",
        sar_header: "txpck/s",
        wire_name: "tx_packets",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    // sar の列名は `rxkB/s` だが、ファイルに入っているのは**バイト**累積値である。
    // 本家は非 human 出力でだけ 1024 で割って kB/s として印字し、`--human` では
    // 生のバイト/秒を `UNIT_BYTE` として渡す (`%ifutil` にもバイト/秒を渡す)。
    // ここでは保存されている単位そのまま (バイト/秒) を宣言し、1024 除算は出力側の責務にする。
    ColumnMeta {
        public_name: "rx_bytes_per_sec",
        sar_header: "rxkB/s",
        wire_name: "rx_bytes",
        unit: Unit::BytesPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "tx_bytes_per_sec",
        sar_header: "txkB/s",
        wire_name: "tx_bytes",
        unit: Unit::BytesPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "rxcmp_per_sec",
        sar_header: "rxcmp/s",
        wire_name: "rx_compressed",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "txcmp_per_sec",
        sar_header: "txcmp/s",
        wire_name: "tx_compressed",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "rxmcst_per_sec",
        sar_header: "rxmcst/s",
        wire_name: "multicast",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    // 派生列 (`compute_ifutil()`): rx / tx はバイト/秒。speed は Mbit/s。
    //   speed == 0            → 0
    //   duplex == 2 (full)    → max(rx, tx) * 800 / (speed * 1_000_000)
    //   それ以外 (half/不明)  → (rx + tx)    * 800 / (speed * 1_000_000)
    // 800 = 8 (バイト→ビット) × 100 (百分率)。
    ColumnMeta {
        public_name: "ifutil_pct",
        sar_header: "%ifutil",
        wire_name: "",
        unit: Unit::Percent,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    // 以下は sar の列には出ない内部フィールド。`%ifutil` の入力になる。
    // `speed` の単位は Mbit/s (`Unit` に該当する値が無いため `None`)。0 = 不明。
    ColumnMeta {
        public_name: "speed",
        sar_header: "",
        wire_name: "speed",
        unit: Unit::None,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Last,
    },
    // 0 = 不明 / 1 = half (`C_DUPLEX_HALF`) / 2 = full (`C_DUPLEX_FULL`)。
    ColumnMeta {
        public_name: "duplex",
        sar_header: "",
        wire_name: "duplex",
        unit: Unit::None,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Last,
    },
];

// ===== A_NET_EDEV (13) =====

/// 現行 (magic `0x8c`、v11.7.2 以降)。ULL 9 本 + `interface[16]` = 88。
const NET_EDEV_FIELDS: &[WireField] = &[
    WireField::natural("collisions", FieldTy::U64),
    WireField::natural("rx_errors", FieldTy::U64),
    WireField::natural("tx_errors", FieldTy::U64),
    WireField::natural("rx_dropped", FieldTy::U64),
    WireField::natural("tx_dropped", FieldTy::U64),
    WireField::natural("rx_fifo_errors", FieldTy::U64),
    WireField::natural("tx_fifo_errors", FieldTy::U64),
    WireField::natural("rx_frame_errors", FieldTy::U64),
    WireField::natural("tx_carrier_errors", FieldTy::U64),
    WireField::natural("interface", FieldTy::Bytes(MAX_IFACE_LEN)),
];

/// magic `0x8b` (v10.1.3 〜 v11.7.1)。ULL 9 本と `interface` が `aligned(16)`。
/// 最終カウンタが 128 で終端 136 なので、`interface` は 136 ではなく **144**。144 + 16 = 160。
const NET_EDEV_FIELDS_8B: &[WireField] = &[
    WireField::aligned("collisions", FieldTy::U64, 16),
    WireField::aligned("rx_errors", FieldTy::U64, 16),
    WireField::aligned("tx_errors", FieldTy::U64, 16),
    WireField::aligned("rx_dropped", FieldTy::U64, 16),
    WireField::aligned("tx_dropped", FieldTy::U64, 16),
    WireField::aligned("rx_fifo_errors", FieldTy::U64, 16),
    WireField::aligned("tx_fifo_errors", FieldTy::U64, 16),
    WireField::aligned("rx_frame_errors", FieldTy::U64, 16),
    WireField::aligned("tx_carrier_errors", FieldTy::U64, 16),
    WireField::aligned("interface", FieldTy::Bytes(MAX_IFACE_LEN), 16),
];

/// magic `0x8a` (v9.1.6 〜 v10.1.2)。カウンタが `unsigned long`、`interface` は `aligned(8)`。
/// 9×8 + 16 = 88 (現行と同サイズだが、32bit ライタでは有効バイト数が 4 になる点が違う)。
const NET_EDEV_FIELDS_8A: &[WireField] = &[
    WireField::natural("collisions", FieldTy::CULong),
    WireField::natural("rx_errors", FieldTy::CULong),
    WireField::natural("tx_errors", FieldTy::CULong),
    WireField::natural("rx_dropped", FieldTy::CULong),
    WireField::natural("tx_dropped", FieldTy::CULong),
    WireField::natural("rx_fifo_errors", FieldTy::CULong),
    WireField::natural("tx_fifo_errors", FieldTy::CULong),
    WireField::natural("rx_frame_errors", FieldTy::CULong),
    WireField::natural("tx_carrier_errors", FieldTy::CULong),
    WireField::aligned("interface", FieldTy::Bytes(MAX_IFACE_LEN), 8),
];

const NET_EDEV_REVISIONS: &[WireRevision] = &[
    WireRevision {
        magic: 0x8c,
        types_nr: [9, 0, 0],
        size_lp64: 88,
        layout: WireLayout::new("stats_net_edev@0x8c", NET_EDEV_FIELDS),
        since: "11.7.2",
    },
    WireRevision {
        magic: 0x8b,
        types_nr: [9, 0, 0],
        size_lp64: 160,
        layout: WireLayout::new("stats_net_edev@0x8b", NET_EDEV_FIELDS_8B),
        since: "10.1.3",
    },
    WireRevision {
        magic: 0x8a,
        types_nr: [0, 9, 0],
        size_lp64: 88,
        layout: WireLayout::new("stats_net_edev@0x8a", NET_EDEV_FIELDS_8A),
        since: "9.1.6",
    },
];

const NET_EDEV_COLUMNS: &[ColumnMeta] = &[
    ColumnMeta {
        public_name: "interface",
        sar_header: "IFACE",
        wire_name: "interface",
        unit: Unit::Identifier,
        kind: ValueKind::Identity,
        aggregation: Aggregation::NotAggregated,
    },
    ColumnMeta {
        public_name: "rxerr_per_sec",
        sar_header: "rxerr/s",
        wire_name: "rx_errors",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "txerr_per_sec",
        sar_header: "txerr/s",
        wire_name: "tx_errors",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "coll_per_sec",
        sar_header: "coll/s",
        wire_name: "collisions",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "rxdrop_per_sec",
        sar_header: "rxdrop/s",
        wire_name: "rx_dropped",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "txdrop_per_sec",
        sar_header: "txdrop/s",
        wire_name: "tx_dropped",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "txcarr_per_sec",
        sar_header: "txcarr/s",
        wire_name: "tx_carrier_errors",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "rxfram_per_sec",
        sar_header: "rxfram/s",
        wire_name: "rx_frame_errors",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "rxfifo_per_sec",
        sar_header: "rxfifo/s",
        wire_name: "rx_fifo_errors",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "txfifo_per_sec",
        sar_header: "txfifo/s",
        wire_name: "tx_fifo_errors",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
];

// ===== A_NET_NFS (14) =====

/// `unsigned int` 6 本 = 24。magic は `0x8a` のまま歴史上不変 (時代 A も同じ配置)。
const NET_NFS_FIELDS: &[WireField] = &[
    WireField::natural("nfs_rpccnt", FieldTy::U32),
    WireField::natural("nfs_rpcretrans", FieldTy::U32),
    WireField::natural("nfs_readcnt", FieldTy::U32),
    WireField::natural("nfs_writecnt", FieldTy::U32),
    WireField::natural("nfs_accesscnt", FieldTy::U32),
    WireField::natural("nfs_getattcnt", FieldTy::U32),
];

const NET_NFS_REVISIONS: &[WireRevision] = &[WireRevision {
    magic: 0x8a,
    types_nr: [0, 0, 6],
    size_lp64: 24,
    layout: WireLayout::new("stats_net_nfs@0x8a", NET_NFS_FIELDS),
    since: "9.1.6",
}];

const NET_NFS_COLUMNS: &[ColumnMeta] = &[
    ColumnMeta {
        public_name: "call_per_sec",
        sar_header: "call/s",
        wire_name: "nfs_rpccnt",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "retrans_per_sec",
        sar_header: "retrans/s",
        wire_name: "nfs_rpcretrans",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "read_per_sec",
        sar_header: "read/s",
        wire_name: "nfs_readcnt",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "write_per_sec",
        sar_header: "write/s",
        wire_name: "nfs_writecnt",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "access_per_sec",
        sar_header: "access/s",
        wire_name: "nfs_accesscnt",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "getatt_per_sec",
        sar_header: "getatt/s",
        wire_name: "nfs_getattcnt",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
];

// ===== A_NET_NFSD (15) =====

/// `unsigned int` 11 本 = 44。magic `0x8a` のまま不変。
const NET_NFSD_FIELDS: &[WireField] = &[
    WireField::natural("nfsd_rpccnt", FieldTy::U32),
    WireField::natural("nfsd_rpcbad", FieldTy::U32),
    WireField::natural("nfsd_netcnt", FieldTy::U32),
    WireField::natural("nfsd_netudpcnt", FieldTy::U32),
    WireField::natural("nfsd_nettcpcnt", FieldTy::U32),
    WireField::natural("nfsd_rchits", FieldTy::U32),
    WireField::natural("nfsd_rcmisses", FieldTy::U32),
    WireField::natural("nfsd_readcnt", FieldTy::U32),
    WireField::natural("nfsd_writecnt", FieldTy::U32),
    WireField::natural("nfsd_accesscnt", FieldTy::U32),
    WireField::natural("nfsd_getattcnt", FieldTy::U32),
];

const NET_NFSD_REVISIONS: &[WireRevision] = &[WireRevision {
    magic: 0x8a,
    types_nr: [0, 0, 11],
    size_lp64: 44,
    layout: WireLayout::new("stats_net_nfsd@0x8a", NET_NFSD_FIELDS),
    since: "9.1.6",
}];

const NET_NFSD_COLUMNS: &[ColumnMeta] = &[
    ColumnMeta {
        public_name: "scall_per_sec",
        sar_header: "scall/s",
        wire_name: "nfsd_rpccnt",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "badcall_per_sec",
        sar_header: "badcall/s",
        wire_name: "nfsd_rpcbad",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "packet_per_sec",
        sar_header: "packet/s",
        wire_name: "nfsd_netcnt",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "udp_per_sec",
        sar_header: "udp/s",
        wire_name: "nfsd_netudpcnt",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "tcp_per_sec",
        sar_header: "tcp/s",
        wire_name: "nfsd_nettcpcnt",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "hit_per_sec",
        sar_header: "hit/s",
        wire_name: "nfsd_rchits",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "miss_per_sec",
        sar_header: "miss/s",
        wire_name: "nfsd_rcmisses",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "sread_per_sec",
        sar_header: "sread/s",
        wire_name: "nfsd_readcnt",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "swrite_per_sec",
        sar_header: "swrite/s",
        wire_name: "nfsd_writecnt",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "saccess_per_sec",
        sar_header: "saccess/s",
        wire_name: "nfsd_accesscnt",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "sgetatt_per_sec",
        sar_header: "sgetatt/s",
        wire_name: "nfsd_getattcnt",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
];

// ===== A_NET_SOCK (16) =====

/// `unsigned int` 6 本 = 24。magic `0x8a` のまま不変。
/// すべて瞬時値 (使用中ソケット数) であり、差分を取ってはいけない。
const NET_SOCK_FIELDS: &[WireField] = &[
    WireField::natural("sock_inuse", FieldTy::U32),
    WireField::natural("tcp_inuse", FieldTy::U32),
    WireField::natural("tcp_tw", FieldTy::U32),
    WireField::natural("udp_inuse", FieldTy::U32),
    WireField::natural("raw_inuse", FieldTy::U32),
    WireField::natural("frag_inuse", FieldTy::U32),
];

const NET_SOCK_REVISIONS: &[WireRevision] = &[WireRevision {
    magic: 0x8a,
    types_nr: [0, 0, 6],
    size_lp64: 24,
    layout: WireLayout::new("stats_net_sock@0x8a", NET_SOCK_FIELDS),
    since: "9.1.6",
}];

// 列順は sar の `hdr_line` (totsck;tcpsck;udpsck;rawsck;ip-frag;tcp-tw) に合わせる。
// 構造体のフィールド順とは `tcp_tw` の位置が違う。
const NET_SOCK_COLUMNS: &[ColumnMeta] = &[
    ColumnMeta {
        public_name: "totsck",
        sar_header: "totsck",
        wire_name: "sock_inuse",
        unit: Unit::Count,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    ColumnMeta {
        public_name: "tcpsck",
        sar_header: "tcpsck",
        wire_name: "tcp_inuse",
        unit: Unit::Count,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    ColumnMeta {
        public_name: "udpsck",
        sar_header: "udpsck",
        wire_name: "udp_inuse",
        unit: Unit::Count,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    ColumnMeta {
        public_name: "rawsck",
        sar_header: "rawsck",
        wire_name: "raw_inuse",
        unit: Unit::Count,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    ColumnMeta {
        public_name: "ip_frag",
        sar_header: "ip-frag",
        wire_name: "frag_inuse",
        unit: Unit::Count,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    ColumnMeta {
        public_name: "tcp_tw",
        sar_header: "tcp-tw",
        wire_name: "tcp_tw",
        unit: Unit::Count,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
];

// ===== A_NET_IP (17) =====

/// 現行 (magic `0x8c`、v11.7.2 以降)。ULL 8 本 = 64。
const NET_IP_FIELDS: &[WireField] = &[
    WireField::natural("InReceives", FieldTy::U64),
    WireField::natural("ForwDatagrams", FieldTy::U64),
    WireField::natural("InDelivers", FieldTy::U64),
    WireField::natural("OutRequests", FieldTy::U64),
    WireField::natural("ReasmReqds", FieldTy::U64),
    WireField::natural("ReasmOKs", FieldTy::U64),
    WireField::natural("FragOKs", FieldTy::U64),
    WireField::natural("FragCreates", FieldTy::U64),
];

/// magic `0x8b` (v10.1.3 〜 v11.7.1)。ULL 8 本が `aligned(16)` → 128。
const NET_IP_FIELDS_8B: &[WireField] = &[
    WireField::aligned("InReceives", FieldTy::U64, 16),
    WireField::aligned("ForwDatagrams", FieldTy::U64, 16),
    WireField::aligned("InDelivers", FieldTy::U64, 16),
    WireField::aligned("OutRequests", FieldTy::U64, 16),
    WireField::aligned("ReasmReqds", FieldTy::U64, 16),
    WireField::aligned("ReasmOKs", FieldTy::U64, 16),
    WireField::aligned("FragOKs", FieldTy::U64, 16),
    WireField::aligned("FragCreates", FieldTy::U64, 16),
];

/// magic `0x8a` (v9.1.6 〜 v10.1.2)。`unsigned long` 8 本 = 64。
const NET_IP_FIELDS_8A: &[WireField] = &[
    WireField::natural("InReceives", FieldTy::CULong),
    WireField::natural("ForwDatagrams", FieldTy::CULong),
    WireField::natural("InDelivers", FieldTy::CULong),
    WireField::natural("OutRequests", FieldTy::CULong),
    WireField::natural("ReasmReqds", FieldTy::CULong),
    WireField::natural("ReasmOKs", FieldTy::CULong),
    WireField::natural("FragOKs", FieldTy::CULong),
    WireField::natural("FragCreates", FieldTy::CULong),
];

const NET_IP_REVISIONS: &[WireRevision] = &[
    WireRevision {
        magic: 0x8c,
        types_nr: [8, 0, 0],
        size_lp64: 64,
        layout: WireLayout::new("stats_net_ip@0x8c", NET_IP_FIELDS),
        since: "11.7.2",
    },
    WireRevision {
        magic: 0x8b,
        types_nr: [8, 0, 0],
        size_lp64: 128,
        layout: WireLayout::new("stats_net_ip@0x8b", NET_IP_FIELDS_8B),
        since: "10.1.3",
    },
    WireRevision {
        magic: 0x8a,
        types_nr: [0, 8, 0],
        size_lp64: 64,
        layout: WireLayout::new("stats_net_ip@0x8a", NET_IP_FIELDS_8A),
        since: "9.1.6",
    },
];

const NET_IP_COLUMNS: &[ColumnMeta] = &[
    ColumnMeta {
        public_name: "irec_per_sec",
        sar_header: "irec/s",
        wire_name: "InReceives",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "fwddgm_per_sec",
        sar_header: "fwddgm/s",
        wire_name: "ForwDatagrams",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "idel_per_sec",
        sar_header: "idel/s",
        wire_name: "InDelivers",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "orq_per_sec",
        sar_header: "orq/s",
        wire_name: "OutRequests",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "asmrq_per_sec",
        sar_header: "asmrq/s",
        wire_name: "ReasmReqds",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "asmok_per_sec",
        sar_header: "asmok/s",
        wire_name: "ReasmOKs",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "fragok_per_sec",
        sar_header: "fragok/s",
        wire_name: "FragOKs",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "fragcrt_per_sec",
        sar_header: "fragcrt/s",
        wire_name: "FragCreates",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
];

// ===== A_NET_EIP (18) =====

/// 現行 (magic `0x8c`、v11.7.2 以降)。ULL 8 本 = 64。
const NET_EIP_FIELDS: &[WireField] = &[
    WireField::natural("InHdrErrors", FieldTy::U64),
    WireField::natural("InAddrErrors", FieldTy::U64),
    WireField::natural("InUnknownProtos", FieldTy::U64),
    WireField::natural("InDiscards", FieldTy::U64),
    WireField::natural("OutDiscards", FieldTy::U64),
    WireField::natural("OutNoRoutes", FieldTy::U64),
    WireField::natural("ReasmFails", FieldTy::U64),
    WireField::natural("FragFails", FieldTy::U64),
];

/// magic `0x8b` (v10.1.3 〜 v11.7.1)。`aligned(16)` → 128。
const NET_EIP_FIELDS_8B: &[WireField] = &[
    WireField::aligned("InHdrErrors", FieldTy::U64, 16),
    WireField::aligned("InAddrErrors", FieldTy::U64, 16),
    WireField::aligned("InUnknownProtos", FieldTy::U64, 16),
    WireField::aligned("InDiscards", FieldTy::U64, 16),
    WireField::aligned("OutDiscards", FieldTy::U64, 16),
    WireField::aligned("OutNoRoutes", FieldTy::U64, 16),
    WireField::aligned("ReasmFails", FieldTy::U64, 16),
    WireField::aligned("FragFails", FieldTy::U64, 16),
];

/// magic `0x8a` (v9.1.6 〜 v10.1.2)。`unsigned long` 8 本 = 64。
const NET_EIP_FIELDS_8A: &[WireField] = &[
    WireField::natural("InHdrErrors", FieldTy::CULong),
    WireField::natural("InAddrErrors", FieldTy::CULong),
    WireField::natural("InUnknownProtos", FieldTy::CULong),
    WireField::natural("InDiscards", FieldTy::CULong),
    WireField::natural("OutDiscards", FieldTy::CULong),
    WireField::natural("OutNoRoutes", FieldTy::CULong),
    WireField::natural("ReasmFails", FieldTy::CULong),
    WireField::natural("FragFails", FieldTy::CULong),
];

const NET_EIP_REVISIONS: &[WireRevision] = &[
    WireRevision {
        magic: 0x8c,
        types_nr: [8, 0, 0],
        size_lp64: 64,
        layout: WireLayout::new("stats_net_eip@0x8c", NET_EIP_FIELDS),
        since: "11.7.2",
    },
    WireRevision {
        magic: 0x8b,
        types_nr: [8, 0, 0],
        size_lp64: 128,
        layout: WireLayout::new("stats_net_eip@0x8b", NET_EIP_FIELDS_8B),
        since: "10.1.3",
    },
    WireRevision {
        magic: 0x8a,
        types_nr: [0, 8, 0],
        size_lp64: 64,
        layout: WireLayout::new("stats_net_eip@0x8a", NET_EIP_FIELDS_8A),
        since: "9.1.6",
    },
];

const NET_EIP_COLUMNS: &[ColumnMeta] = &[
    ColumnMeta {
        public_name: "ihdrerr_per_sec",
        sar_header: "ihdrerr/s",
        wire_name: "InHdrErrors",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "iadrerr_per_sec",
        sar_header: "iadrerr/s",
        wire_name: "InAddrErrors",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "iukwnpr_per_sec",
        sar_header: "iukwnpr/s",
        wire_name: "InUnknownProtos",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "idisc_per_sec",
        sar_header: "idisc/s",
        wire_name: "InDiscards",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "odisc_per_sec",
        sar_header: "odisc/s",
        wire_name: "OutDiscards",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "onort_per_sec",
        sar_header: "onort/s",
        wire_name: "OutNoRoutes",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "asmf_per_sec",
        sar_header: "asmf/s",
        wire_name: "ReasmFails",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "fragf_per_sec",
        sar_header: "fragf/s",
        wire_name: "FragFails",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
];

// ===== A_NET_ICMP (19) =====

/// `unsigned long` 14 本 = 112。**歴史上一度も変わっていない** (magic `0x8a` 固定、
/// `aligned(8)` も維持)。32bit ライタでは各スロットの先頭 4 バイトだけが有効。
const NET_ICMP_FIELDS: &[WireField] = &[
    WireField::natural("InMsgs", FieldTy::CULong),
    WireField::natural("OutMsgs", FieldTy::CULong),
    WireField::natural("InEchos", FieldTy::CULong),
    WireField::natural("InEchoReps", FieldTy::CULong),
    WireField::natural("OutEchos", FieldTy::CULong),
    WireField::natural("OutEchoReps", FieldTy::CULong),
    WireField::natural("InTimestamps", FieldTy::CULong),
    WireField::natural("InTimestampReps", FieldTy::CULong),
    WireField::natural("OutTimestamps", FieldTy::CULong),
    WireField::natural("OutTimestampReps", FieldTy::CULong),
    WireField::natural("InAddrMasks", FieldTy::CULong),
    WireField::natural("InAddrMaskReps", FieldTy::CULong),
    WireField::natural("OutAddrMasks", FieldTy::CULong),
    WireField::natural("OutAddrMaskReps", FieldTy::CULong),
];

const NET_ICMP_REVISIONS: &[WireRevision] = &[WireRevision {
    magic: 0x8a,
    types_nr: [0, 14, 0],
    size_lp64: 112,
    layout: WireLayout::new("stats_net_icmp@0x8a", NET_ICMP_FIELDS),
    since: "9.1.6",
}];

const NET_ICMP_COLUMNS: &[ColumnMeta] = &[
    ColumnMeta {
        public_name: "imsg_per_sec",
        sar_header: "imsg/s",
        wire_name: "InMsgs",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "omsg_per_sec",
        sar_header: "omsg/s",
        wire_name: "OutMsgs",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "iech_per_sec",
        sar_header: "iech/s",
        wire_name: "InEchos",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "iechr_per_sec",
        sar_header: "iechr/s",
        wire_name: "InEchoReps",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "oech_per_sec",
        sar_header: "oech/s",
        wire_name: "OutEchos",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "oechr_per_sec",
        sar_header: "oechr/s",
        wire_name: "OutEchoReps",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "itm_per_sec",
        sar_header: "itm/s",
        wire_name: "InTimestamps",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "itmr_per_sec",
        sar_header: "itmr/s",
        wire_name: "InTimestampReps",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "otm_per_sec",
        sar_header: "otm/s",
        wire_name: "OutTimestamps",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "otmr_per_sec",
        sar_header: "otmr/s",
        wire_name: "OutTimestampReps",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "iadrmk_per_sec",
        sar_header: "iadrmk/s",
        wire_name: "InAddrMasks",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "iadrmkr_per_sec",
        sar_header: "iadrmkr/s",
        wire_name: "InAddrMaskReps",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "oadrmk_per_sec",
        sar_header: "oadrmk/s",
        wire_name: "OutAddrMasks",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "oadrmkr_per_sec",
        sar_header: "oadrmkr/s",
        wire_name: "OutAddrMaskReps",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
];

// ===== A_NET_EICMP (20) =====

/// `unsigned long` 12 本 = 96。magic `0x8a` 固定、変更なし。
const NET_EICMP_FIELDS: &[WireField] = &[
    WireField::natural("InErrors", FieldTy::CULong),
    WireField::natural("OutErrors", FieldTy::CULong),
    WireField::natural("InDestUnreachs", FieldTy::CULong),
    WireField::natural("OutDestUnreachs", FieldTy::CULong),
    WireField::natural("InTimeExcds", FieldTy::CULong),
    WireField::natural("OutTimeExcds", FieldTy::CULong),
    WireField::natural("InParmProbs", FieldTy::CULong),
    WireField::natural("OutParmProbs", FieldTy::CULong),
    WireField::natural("InSrcQuenchs", FieldTy::CULong),
    WireField::natural("OutSrcQuenchs", FieldTy::CULong),
    WireField::natural("InRedirects", FieldTy::CULong),
    WireField::natural("OutRedirects", FieldTy::CULong),
];

const NET_EICMP_REVISIONS: &[WireRevision] = &[WireRevision {
    magic: 0x8a,
    types_nr: [0, 12, 0],
    size_lp64: 96,
    layout: WireLayout::new("stats_net_eicmp@0x8a", NET_EICMP_FIELDS),
    since: "9.1.6",
}];

const NET_EICMP_COLUMNS: &[ColumnMeta] = &[
    ColumnMeta {
        public_name: "ierr_per_sec",
        sar_header: "ierr/s",
        wire_name: "InErrors",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "oerr_per_sec",
        sar_header: "oerr/s",
        wire_name: "OutErrors",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "idstunr_per_sec",
        sar_header: "idstunr/s",
        wire_name: "InDestUnreachs",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "odstunr_per_sec",
        sar_header: "odstunr/s",
        wire_name: "OutDestUnreachs",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "itmex_per_sec",
        sar_header: "itmex/s",
        wire_name: "InTimeExcds",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "otmex_per_sec",
        sar_header: "otmex/s",
        wire_name: "OutTimeExcds",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "iparmpb_per_sec",
        sar_header: "iparmpb/s",
        wire_name: "InParmProbs",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "oparmpb_per_sec",
        sar_header: "oparmpb/s",
        wire_name: "OutParmProbs",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "isrcq_per_sec",
        sar_header: "isrcq/s",
        wire_name: "InSrcQuenchs",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "osrcq_per_sec",
        sar_header: "osrcq/s",
        wire_name: "OutSrcQuenchs",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "iredir_per_sec",
        sar_header: "iredir/s",
        wire_name: "InRedirects",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "oredir_per_sec",
        sar_header: "oredir/s",
        wire_name: "OutRedirects",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
];

// ===== A_NET_TCP (21) =====

/// `unsigned long` 4 本 = 32。magic `0x8a` 固定、変更なし。
const NET_TCP_FIELDS: &[WireField] = &[
    WireField::natural("ActiveOpens", FieldTy::CULong),
    WireField::natural("PassiveOpens", FieldTy::CULong),
    WireField::natural("InSegs", FieldTy::CULong),
    WireField::natural("OutSegs", FieldTy::CULong),
];

const NET_TCP_REVISIONS: &[WireRevision] = &[WireRevision {
    magic: 0x8a,
    types_nr: [0, 4, 0],
    size_lp64: 32,
    layout: WireLayout::new("stats_net_tcp@0x8a", NET_TCP_FIELDS),
    since: "9.1.6",
}];

const NET_TCP_COLUMNS: &[ColumnMeta] = &[
    ColumnMeta {
        public_name: "active_per_sec",
        sar_header: "active/s",
        wire_name: "ActiveOpens",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "passive_per_sec",
        sar_header: "passive/s",
        wire_name: "PassiveOpens",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "iseg_per_sec",
        sar_header: "iseg/s",
        wire_name: "InSegs",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "oseg_per_sec",
        sar_header: "oseg/s",
        wire_name: "OutSegs",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
];

// ===== A_NET_ETCP (22) =====

/// `unsigned long` 5 本 = 40。magic `0x8a` 固定、変更なし。
const NET_ETCP_FIELDS: &[WireField] = &[
    WireField::natural("AttemptFails", FieldTy::CULong),
    WireField::natural("EstabResets", FieldTy::CULong),
    WireField::natural("RetransSegs", FieldTy::CULong),
    WireField::natural("InErrs", FieldTy::CULong),
    WireField::natural("OutRsts", FieldTy::CULong),
];

const NET_ETCP_REVISIONS: &[WireRevision] = &[WireRevision {
    magic: 0x8a,
    types_nr: [0, 5, 0],
    size_lp64: 40,
    layout: WireLayout::new("stats_net_etcp@0x8a", NET_ETCP_FIELDS),
    since: "9.1.6",
}];

const NET_ETCP_COLUMNS: &[ColumnMeta] = &[
    ColumnMeta {
        public_name: "atmptf_per_sec",
        sar_header: "atmptf/s",
        wire_name: "AttemptFails",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "estres_per_sec",
        sar_header: "estres/s",
        wire_name: "EstabResets",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "retrseg_per_sec",
        sar_header: "retrseg/s",
        wire_name: "RetransSegs",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "isegerr_per_sec",
        sar_header: "isegerr/s",
        wire_name: "InErrs",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "orsts_per_sec",
        sar_header: "orsts/s",
        wire_name: "OutRsts",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
];

// ===== A_NET_UDP (23) =====

/// `unsigned long` 4 本 = 32。magic `0x8a` 固定、変更なし。
const NET_UDP_FIELDS: &[WireField] = &[
    WireField::natural("InDatagrams", FieldTy::CULong),
    WireField::natural("OutDatagrams", FieldTy::CULong),
    WireField::natural("NoPorts", FieldTy::CULong),
    WireField::natural("InErrors", FieldTy::CULong),
];

const NET_UDP_REVISIONS: &[WireRevision] = &[WireRevision {
    magic: 0x8a,
    types_nr: [0, 4, 0],
    size_lp64: 32,
    layout: WireLayout::new("stats_net_udp@0x8a", NET_UDP_FIELDS),
    since: "9.1.6",
}];

const NET_UDP_COLUMNS: &[ColumnMeta] = &[
    ColumnMeta {
        public_name: "idgm_per_sec",
        sar_header: "idgm/s",
        wire_name: "InDatagrams",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "odgm_per_sec",
        sar_header: "odgm/s",
        wire_name: "OutDatagrams",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "noport_per_sec",
        sar_header: "noport/s",
        wire_name: "NoPorts",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "idgmerr_per_sec",
        sar_header: "idgmerr/s",
        wire_name: "InErrors",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
];

// ===== A_NET_SOCK6 (24) =====

/// `unsigned int` 4 本 = 16。magic `0x8a` 固定、変更なし。すべて瞬時値。
const NET_SOCK6_FIELDS: &[WireField] = &[
    WireField::natural("tcp6_inuse", FieldTy::U32),
    WireField::natural("udp6_inuse", FieldTy::U32),
    WireField::natural("raw6_inuse", FieldTy::U32),
    WireField::natural("frag6_inuse", FieldTy::U32),
];

const NET_SOCK6_REVISIONS: &[WireRevision] = &[WireRevision {
    magic: 0x8a,
    types_nr: [0, 0, 4],
    size_lp64: 16,
    layout: WireLayout::new("stats_net_sock6@0x8a", NET_SOCK6_FIELDS),
    since: "9.1.6",
}];

const NET_SOCK6_COLUMNS: &[ColumnMeta] = &[
    ColumnMeta {
        public_name: "tcp6sck",
        sar_header: "tcp6sck",
        wire_name: "tcp6_inuse",
        unit: Unit::Count,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    ColumnMeta {
        public_name: "udp6sck",
        sar_header: "udp6sck",
        wire_name: "udp6_inuse",
        unit: Unit::Count,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    ColumnMeta {
        public_name: "raw6sck",
        sar_header: "raw6sck",
        wire_name: "raw6_inuse",
        unit: Unit::Count,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    ColumnMeta {
        public_name: "ip6_frag",
        sar_header: "ip6-frag",
        wire_name: "frag6_inuse",
        unit: Unit::Count,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
];

// ===== A_NET_IP6 (25) =====

/// 現行 (magic `0x8c`、v11.7.2 以降)。ULL 10 本 = 80。
const NET_IP6_FIELDS: &[WireField] = &[
    WireField::natural("InReceives6", FieldTy::U64),
    WireField::natural("OutForwDatagrams6", FieldTy::U64),
    WireField::natural("InDelivers6", FieldTy::U64),
    WireField::natural("OutRequests6", FieldTy::U64),
    WireField::natural("ReasmReqds6", FieldTy::U64),
    WireField::natural("ReasmOKs6", FieldTy::U64),
    WireField::natural("InMcastPkts6", FieldTy::U64),
    WireField::natural("OutMcastPkts6", FieldTy::U64),
    WireField::natural("FragOKs6", FieldTy::U64),
    WireField::natural("FragCreates6", FieldTy::U64),
];

/// magic `0x8b` (v10.1.3 〜 v11.7.1)。`aligned(16)` → 160。
const NET_IP6_FIELDS_8B: &[WireField] = &[
    WireField::aligned("InReceives6", FieldTy::U64, 16),
    WireField::aligned("OutForwDatagrams6", FieldTy::U64, 16),
    WireField::aligned("InDelivers6", FieldTy::U64, 16),
    WireField::aligned("OutRequests6", FieldTy::U64, 16),
    WireField::aligned("ReasmReqds6", FieldTy::U64, 16),
    WireField::aligned("ReasmOKs6", FieldTy::U64, 16),
    WireField::aligned("InMcastPkts6", FieldTy::U64, 16),
    WireField::aligned("OutMcastPkts6", FieldTy::U64, 16),
    WireField::aligned("FragOKs6", FieldTy::U64, 16),
    WireField::aligned("FragCreates6", FieldTy::U64, 16),
];

/// magic `0x8a` (v9.1.6 〜 v10.1.2)。`unsigned long` 10 本 = 80。
const NET_IP6_FIELDS_8A: &[WireField] = &[
    WireField::natural("InReceives6", FieldTy::CULong),
    WireField::natural("OutForwDatagrams6", FieldTy::CULong),
    WireField::natural("InDelivers6", FieldTy::CULong),
    WireField::natural("OutRequests6", FieldTy::CULong),
    WireField::natural("ReasmReqds6", FieldTy::CULong),
    WireField::natural("ReasmOKs6", FieldTy::CULong),
    WireField::natural("InMcastPkts6", FieldTy::CULong),
    WireField::natural("OutMcastPkts6", FieldTy::CULong),
    WireField::natural("FragOKs6", FieldTy::CULong),
    WireField::natural("FragCreates6", FieldTy::CULong),
];

const NET_IP6_REVISIONS: &[WireRevision] = &[
    WireRevision {
        magic: 0x8c,
        types_nr: [10, 0, 0],
        size_lp64: 80,
        layout: WireLayout::new("stats_net_ip6@0x8c", NET_IP6_FIELDS),
        since: "11.7.2",
    },
    WireRevision {
        magic: 0x8b,
        types_nr: [10, 0, 0],
        size_lp64: 160,
        layout: WireLayout::new("stats_net_ip6@0x8b", NET_IP6_FIELDS_8B),
        since: "10.1.3",
    },
    WireRevision {
        magic: 0x8a,
        types_nr: [0, 10, 0],
        size_lp64: 80,
        layout: WireLayout::new("stats_net_ip6@0x8a", NET_IP6_FIELDS_8A),
        since: "9.1.6",
    },
];

const NET_IP6_COLUMNS: &[ColumnMeta] = &[
    ColumnMeta {
        public_name: "irec6_per_sec",
        sar_header: "irec6/s",
        wire_name: "InReceives6",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "fwddgm6_per_sec",
        sar_header: "fwddgm6/s",
        wire_name: "OutForwDatagrams6",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "idel6_per_sec",
        sar_header: "idel6/s",
        wire_name: "InDelivers6",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "orq6_per_sec",
        sar_header: "orq6/s",
        wire_name: "OutRequests6",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "asmrq6_per_sec",
        sar_header: "asmrq6/s",
        wire_name: "ReasmReqds6",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "asmok6_per_sec",
        sar_header: "asmok6/s",
        wire_name: "ReasmOKs6",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "imcpck6_per_sec",
        sar_header: "imcpck6/s",
        wire_name: "InMcastPkts6",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "omcpck6_per_sec",
        sar_header: "omcpck6/s",
        wire_name: "OutMcastPkts6",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "fragok6_per_sec",
        sar_header: "fragok6/s",
        wire_name: "FragOKs6",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "fragcr6_per_sec",
        sar_header: "fragcr6/s",
        wire_name: "FragCreates6",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
];

// ===== A_NET_EIP6 (26) =====

/// 現行 (magic `0x8c`、v11.7.2 以降)。ULL 11 本 = 88。
const NET_EIP6_FIELDS: &[WireField] = &[
    WireField::natural("InHdrErrors6", FieldTy::U64),
    WireField::natural("InAddrErrors6", FieldTy::U64),
    WireField::natural("InUnknownProtos6", FieldTy::U64),
    WireField::natural("InTooBigErrors6", FieldTy::U64),
    WireField::natural("InDiscards6", FieldTy::U64),
    WireField::natural("OutDiscards6", FieldTy::U64),
    WireField::natural("InNoRoutes6", FieldTy::U64),
    WireField::natural("OutNoRoutes6", FieldTy::U64),
    WireField::natural("ReasmFails6", FieldTy::U64),
    WireField::natural("FragFails6", FieldTy::U64),
    WireField::natural("InTruncatedPkts6", FieldTy::U64),
];

/// magic `0x8b` (v10.1.3 〜 v11.7.1)。`aligned(16)` → 176。
const NET_EIP6_FIELDS_8B: &[WireField] = &[
    WireField::aligned("InHdrErrors6", FieldTy::U64, 16),
    WireField::aligned("InAddrErrors6", FieldTy::U64, 16),
    WireField::aligned("InUnknownProtos6", FieldTy::U64, 16),
    WireField::aligned("InTooBigErrors6", FieldTy::U64, 16),
    WireField::aligned("InDiscards6", FieldTy::U64, 16),
    WireField::aligned("OutDiscards6", FieldTy::U64, 16),
    WireField::aligned("InNoRoutes6", FieldTy::U64, 16),
    WireField::aligned("OutNoRoutes6", FieldTy::U64, 16),
    WireField::aligned("ReasmFails6", FieldTy::U64, 16),
    WireField::aligned("FragFails6", FieldTy::U64, 16),
    WireField::aligned("InTruncatedPkts6", FieldTy::U64, 16),
];

/// magic `0x8a` (v9.1.6 〜 v10.1.2)。`unsigned long` 11 本 = 88。
const NET_EIP6_FIELDS_8A: &[WireField] = &[
    WireField::natural("InHdrErrors6", FieldTy::CULong),
    WireField::natural("InAddrErrors6", FieldTy::CULong),
    WireField::natural("InUnknownProtos6", FieldTy::CULong),
    WireField::natural("InTooBigErrors6", FieldTy::CULong),
    WireField::natural("InDiscards6", FieldTy::CULong),
    WireField::natural("OutDiscards6", FieldTy::CULong),
    WireField::natural("InNoRoutes6", FieldTy::CULong),
    WireField::natural("OutNoRoutes6", FieldTy::CULong),
    WireField::natural("ReasmFails6", FieldTy::CULong),
    WireField::natural("FragFails6", FieldTy::CULong),
    WireField::natural("InTruncatedPkts6", FieldTy::CULong),
];

const NET_EIP6_REVISIONS: &[WireRevision] = &[
    WireRevision {
        magic: 0x8c,
        types_nr: [11, 0, 0],
        size_lp64: 88,
        layout: WireLayout::new("stats_net_eip6@0x8c", NET_EIP6_FIELDS),
        since: "11.7.2",
    },
    WireRevision {
        magic: 0x8b,
        types_nr: [11, 0, 0],
        size_lp64: 176,
        layout: WireLayout::new("stats_net_eip6@0x8b", NET_EIP6_FIELDS_8B),
        since: "10.1.3",
    },
    WireRevision {
        magic: 0x8a,
        types_nr: [0, 11, 0],
        size_lp64: 88,
        layout: WireLayout::new("stats_net_eip6@0x8a", NET_EIP6_FIELDS_8A),
        since: "9.1.6",
    },
];

const NET_EIP6_COLUMNS: &[ColumnMeta] = &[
    ColumnMeta {
        public_name: "ihdrer6_per_sec",
        sar_header: "ihdrer6/s",
        wire_name: "InHdrErrors6",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "iadrer6_per_sec",
        sar_header: "iadrer6/s",
        wire_name: "InAddrErrors6",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "iukwnp6_per_sec",
        sar_header: "iukwnp6/s",
        wire_name: "InUnknownProtos6",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "i2big6_per_sec",
        sar_header: "i2big6/s",
        wire_name: "InTooBigErrors6",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "idisc6_per_sec",
        sar_header: "idisc6/s",
        wire_name: "InDiscards6",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "odisc6_per_sec",
        sar_header: "odisc6/s",
        wire_name: "OutDiscards6",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "inort6_per_sec",
        sar_header: "inort6/s",
        wire_name: "InNoRoutes6",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "onort6_per_sec",
        sar_header: "onort6/s",
        wire_name: "OutNoRoutes6",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "asmf6_per_sec",
        sar_header: "asmf6/s",
        wire_name: "ReasmFails6",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "fragf6_per_sec",
        sar_header: "fragf6/s",
        wire_name: "FragFails6",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "itrpck6_per_sec",
        sar_header: "itrpck6/s",
        wire_name: "InTruncatedPkts6",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
];

// ===== A_NET_ICMP6 (27) =====

/// `unsigned long` 17 本 = 136。magic `0x8a` 固定、変更なし。
const NET_ICMP6_FIELDS: &[WireField] = &[
    WireField::natural("InMsgs6", FieldTy::CULong),
    WireField::natural("OutMsgs6", FieldTy::CULong),
    WireField::natural("InEchos6", FieldTy::CULong),
    WireField::natural("InEchoReplies6", FieldTy::CULong),
    WireField::natural("OutEchoReplies6", FieldTy::CULong),
    WireField::natural("InGroupMembQueries6", FieldTy::CULong),
    WireField::natural("InGroupMembResponses6", FieldTy::CULong),
    WireField::natural("OutGroupMembResponses6", FieldTy::CULong),
    WireField::natural("InGroupMembReductions6", FieldTy::CULong),
    WireField::natural("OutGroupMembReductions6", FieldTy::CULong),
    WireField::natural("InRouterSolicits6", FieldTy::CULong),
    WireField::natural("OutRouterSolicits6", FieldTy::CULong),
    WireField::natural("InRouterAdvertisements6", FieldTy::CULong),
    WireField::natural("InNeighborSolicits6", FieldTy::CULong),
    WireField::natural("OutNeighborSolicits6", FieldTy::CULong),
    WireField::natural("InNeighborAdvertisements6", FieldTy::CULong),
    WireField::natural("OutNeighborAdvertisements6", FieldTy::CULong),
];

const NET_ICMP6_REVISIONS: &[WireRevision] = &[WireRevision {
    magic: 0x8a,
    types_nr: [0, 17, 0],
    size_lp64: 136,
    layout: WireLayout::new("stats_net_icmp6@0x8a", NET_ICMP6_FIELDS),
    since: "9.1.6",
}];

// `oech6/s` (Echo Request 送信) は /proc/net/snmp6 に無いため列自体が存在しない。
const NET_ICMP6_COLUMNS: &[ColumnMeta] = &[
    ColumnMeta {
        public_name: "imsg6_per_sec",
        sar_header: "imsg6/s",
        wire_name: "InMsgs6",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "omsg6_per_sec",
        sar_header: "omsg6/s",
        wire_name: "OutMsgs6",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "iech6_per_sec",
        sar_header: "iech6/s",
        wire_name: "InEchos6",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "iechr6_per_sec",
        sar_header: "iechr6/s",
        wire_name: "InEchoReplies6",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "oechr6_per_sec",
        sar_header: "oechr6/s",
        wire_name: "OutEchoReplies6",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "igmbq6_per_sec",
        sar_header: "igmbq6/s",
        wire_name: "InGroupMembQueries6",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "igmbr6_per_sec",
        sar_header: "igmbr6/s",
        wire_name: "InGroupMembResponses6",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "ogmbr6_per_sec",
        sar_header: "ogmbr6/s",
        wire_name: "OutGroupMembResponses6",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "igmbrd6_per_sec",
        sar_header: "igmbrd6/s",
        wire_name: "InGroupMembReductions6",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "ogmbrd6_per_sec",
        sar_header: "ogmbrd6/s",
        wire_name: "OutGroupMembReductions6",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "irtsol6_per_sec",
        sar_header: "irtsol6/s",
        wire_name: "InRouterSolicits6",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "ortsol6_per_sec",
        sar_header: "ortsol6/s",
        wire_name: "OutRouterSolicits6",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "irtad6_per_sec",
        sar_header: "irtad6/s",
        wire_name: "InRouterAdvertisements6",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "inbsol6_per_sec",
        sar_header: "inbsol6/s",
        wire_name: "InNeighborSolicits6",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "onbsol6_per_sec",
        sar_header: "onbsol6/s",
        wire_name: "OutNeighborSolicits6",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "inbad6_per_sec",
        sar_header: "inbad6/s",
        wire_name: "InNeighborAdvertisements6",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "onbad6_per_sec",
        sar_header: "onbad6/s",
        wire_name: "OutNeighborAdvertisements6",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
];

// ===== A_NET_EICMP6 (28) =====

/// `unsigned long` 11 本 = 88。magic `0x8a` 固定、変更なし。
/// IPv4 の EICMP と違い `OutErrors6` に相当するフィールドは無い (11 本)。
const NET_EICMP6_FIELDS: &[WireField] = &[
    WireField::natural("InErrors6", FieldTy::CULong),
    WireField::natural("InDestUnreachs6", FieldTy::CULong),
    WireField::natural("OutDestUnreachs6", FieldTy::CULong),
    WireField::natural("InTimeExcds6", FieldTy::CULong),
    WireField::natural("OutTimeExcds6", FieldTy::CULong),
    WireField::natural("InParmProblems6", FieldTy::CULong),
    WireField::natural("OutParmProblems6", FieldTy::CULong),
    WireField::natural("InRedirects6", FieldTy::CULong),
    WireField::natural("OutRedirects6", FieldTy::CULong),
    WireField::natural("InPktTooBigs6", FieldTy::CULong),
    WireField::natural("OutPktTooBigs6", FieldTy::CULong),
];

const NET_EICMP6_REVISIONS: &[WireRevision] = &[WireRevision {
    magic: 0x8a,
    types_nr: [0, 11, 0],
    size_lp64: 88,
    layout: WireLayout::new("stats_net_eicmp6@0x8a", NET_EICMP6_FIELDS),
    since: "9.1.6",
}];

const NET_EICMP6_COLUMNS: &[ColumnMeta] = &[
    ColumnMeta {
        public_name: "ierr6_per_sec",
        sar_header: "ierr6/s",
        wire_name: "InErrors6",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "idtunr6_per_sec",
        sar_header: "idtunr6/s",
        wire_name: "InDestUnreachs6",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "odtunr6_per_sec",
        sar_header: "odtunr6/s",
        wire_name: "OutDestUnreachs6",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "itmex6_per_sec",
        sar_header: "itmex6/s",
        wire_name: "InTimeExcds6",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "otmex6_per_sec",
        sar_header: "otmex6/s",
        wire_name: "OutTimeExcds6",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "iprmpb6_per_sec",
        sar_header: "iprmpb6/s",
        wire_name: "InParmProblems6",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "oprmpb6_per_sec",
        sar_header: "oprmpb6/s",
        wire_name: "OutParmProblems6",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "iredir6_per_sec",
        sar_header: "iredir6/s",
        wire_name: "InRedirects6",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "oredir6_per_sec",
        sar_header: "oredir6/s",
        wire_name: "OutRedirects6",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "ipck2b6_per_sec",
        sar_header: "ipck2b6/s",
        wire_name: "InPktTooBigs6",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "opck2b6_per_sec",
        sar_header: "opck2b6/s",
        wire_name: "OutPktTooBigs6",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
];

// ===== A_NET_UDP6 (29) =====

/// `unsigned long` 4 本 = 32。magic `0x8a` 固定、変更なし。
const NET_UDP6_FIELDS: &[WireField] = &[
    WireField::natural("InDatagrams6", FieldTy::CULong),
    WireField::natural("OutDatagrams6", FieldTy::CULong),
    WireField::natural("NoPorts6", FieldTy::CULong),
    WireField::natural("InErrors6", FieldTy::CULong),
];

const NET_UDP6_REVISIONS: &[WireRevision] = &[WireRevision {
    magic: 0x8a,
    types_nr: [0, 4, 0],
    size_lp64: 32,
    layout: WireLayout::new("stats_net_udp6@0x8a", NET_UDP6_FIELDS),
    since: "9.1.6",
}];

const NET_UDP6_COLUMNS: &[ColumnMeta] = &[
    ColumnMeta {
        public_name: "idgm6_per_sec",
        sar_header: "idgm6/s",
        wire_name: "InDatagrams6",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "odgm6_per_sec",
        sar_header: "odgm6/s",
        wire_name: "OutDatagrams6",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "noport6_per_sec",
        sar_header: "noport6/s",
        wire_name: "NoPorts6",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "idgmer6_per_sec",
        sar_header: "idgmer6/s",
        wire_name: "InErrors6",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
];

// ===== A_NET_FC (38) =====

/// `unsigned long` 4 本 + `fchost_name[16]` = 48。magic `0x8a` 固定、変更なし。
///
/// 5 フィールド全てに `aligned(8)` が付いたまま時代 B を生き残った唯一の構造体
/// (`fchost_name` の `aligned(8)` は 32 バイト境界にすでに載るため配置には影響しない)。
const NET_FC_FIELDS: &[WireField] = &[
    WireField::natural("f_rxframes", FieldTy::CULong),
    WireField::natural("f_txframes", FieldTy::CULong),
    WireField::natural("f_rxwords", FieldTy::CULong),
    WireField::natural("f_txwords", FieldTy::CULong),
    WireField::aligned("fchost_name", FieldTy::Bytes(MAX_FCH_LEN), 8),
];

const NET_FC_REVISIONS: &[WireRevision] = &[WireRevision {
    magic: 0x8a,
    types_nr: [0, 4, 0],
    size_lp64: 48,
    layout: WireLayout::new("stats_fchost@0x8a", NET_FC_FIELDS),
    since: "11.1.5",
}];

const NET_FC_COLUMNS: &[ColumnMeta] = &[
    ColumnMeta {
        public_name: "fchost_name",
        sar_header: "FCHOST",
        wire_name: "fchost_name",
        unit: Unit::Identifier,
        kind: ValueKind::Identity,
        aggregation: Aggregation::NotAggregated,
    },
    ColumnMeta {
        public_name: "fch_rxf_per_sec",
        sar_header: "fch_rxf/s",
        wire_name: "f_rxframes",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "fch_txf_per_sec",
        sar_header: "fch_txf/s",
        wire_name: "f_txframes",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "fch_rxw_per_sec",
        sar_header: "fch_rxw/s",
        wire_name: "f_rxwords",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "fch_txw_per_sec",
        sar_header: "fch_txw/s",
        wire_name: "f_txwords",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
];

// ===== A_NET_SOFT (39) =====

/// 現行 (`unsigned int` 6 本 = 24、v12.6.0 / dev v12.7.1 以降)。
///
/// `backlog_len` は magic を上げずに追加されたため、**magic は旧版と同じ `0x8a`**。
/// 判別は `types_nr` (`(0,0,6)` か `(0,0,5)`) と `size` (24 か 20) で行う。
const NET_SOFT_FIELDS: &[WireField] = &[
    WireField::natural("processed", FieldTy::U32),
    WireField::natural("dropped", FieldTy::U32),
    WireField::natural("time_squeeze", FieldTy::U32),
    WireField::natural("received_rps", FieldTy::U32),
    WireField::natural("flow_limit", FieldTy::U32),
    WireField::natural("backlog_len", FieldTy::U32),
];

/// v11.5.2 〜 v12.5.x (`backlog_len` 追加前、20 バイト)。
const NET_SOFT_FIELDS_V5: &[WireField] = &[
    WireField::natural("processed", FieldTy::U32),
    WireField::natural("dropped", FieldTy::U32),
    WireField::natural("time_squeeze", FieldTy::U32),
    WireField::natural("received_rps", FieldTy::U32),
    WireField::natural("flow_limit", FieldTy::U32),
];

const NET_SOFT_REVISIONS: &[WireRevision] = &[
    WireRevision {
        magic: 0x8a,
        types_nr: [0, 0, 6],
        size_lp64: 24,
        layout: WireLayout::new("stats_softnet@0x8a+blg", NET_SOFT_FIELDS),
        since: "12.6.0",
    },
    WireRevision {
        magic: 0x8a,
        types_nr: [0, 0, 5],
        size_lp64: 20,
        layout: WireLayout::new("stats_softnet@0x8a", NET_SOFT_FIELDS_V5),
        since: "11.5.2",
    },
];

const NET_SOFT_COLUMNS: &[ColumnMeta] = &[
    // 派生列: CPU 番号は item のインデックスから決まる (index 0 = "all"、
    // index i = CPU #(i-1))。構造体にフィールドとしては入っていない。
    ColumnMeta {
        public_name: "cpu",
        sar_header: "CPU",
        wire_name: "",
        unit: Unit::Identifier,
        kind: ValueKind::Identity,
        aggregation: Aggregation::NotAggregated,
    },
    ColumnMeta {
        public_name: "total_per_sec",
        sar_header: "total/s",
        wire_name: "processed",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "dropd_per_sec",
        sar_header: "dropd/s",
        wire_name: "dropped",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "squeezd_per_sec",
        sar_header: "squeezd/s",
        wire_name: "time_squeeze",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "rx_rps_per_sec",
        sar_header: "rx_rps/s",
        wire_name: "received_rps",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    ColumnMeta {
        public_name: "flw_lim_per_sec",
        sar_header: "flw_lim/s",
        wire_name: "flow_limit",
        unit: Unit::CountPerSec,
        kind: ValueKind::Counter,
        aggregation: Aggregation::RateOverPeriod,
    },
    // バックログキュー長は瞬時値。差分を取らない。v12.6.0 より前のファイルには存在しない。
    ColumnMeta {
        public_name: "blg_len",
        sar_header: "blg_len",
        wire_name: "backlog_len",
        unit: Unit::Count,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
];

/// network 系 activity の定義 (ID 昇順)。
///
/// `has_nr` は本家の `AO_COUNTED` に一致させている。立つのは `A_NET_DEV` /
/// `A_NET_EDEV` / `A_NET_FC` / `A_NET_SOFT` の 4 件のみで、SNMP 系・IPv6 系・
/// NFS 系・SOCK 系は固定 1 item のため `nr` はレコードに入らない。
pub const DEFS: &[ActivityDef] = &[
    ActivityDef {
        id: ActivityId::NET_DEV,
        revisions: NET_DEV_REVISIONS,
        columns: NET_DEV_COLUMNS,
        shape: ItemShape::List,
        item_key: "interface",
        has_nr: true,
    },
    ActivityDef {
        id: ActivityId::NET_EDEV,
        revisions: NET_EDEV_REVISIONS,
        columns: NET_EDEV_COLUMNS,
        shape: ItemShape::List,
        item_key: "interface",
        has_nr: true,
    },
    ActivityDef {
        id: ActivityId::NET_NFS,
        revisions: NET_NFS_REVISIONS,
        columns: NET_NFS_COLUMNS,
        shape: ItemShape::Single,
        item_key: "",
        has_nr: false,
    },
    ActivityDef {
        id: ActivityId::NET_NFSD,
        revisions: NET_NFSD_REVISIONS,
        columns: NET_NFSD_COLUMNS,
        shape: ItemShape::Single,
        item_key: "",
        has_nr: false,
    },
    ActivityDef {
        id: ActivityId::NET_SOCK,
        revisions: NET_SOCK_REVISIONS,
        columns: NET_SOCK_COLUMNS,
        shape: ItemShape::Single,
        item_key: "",
        has_nr: false,
    },
    ActivityDef {
        id: ActivityId::NET_IP,
        revisions: NET_IP_REVISIONS,
        columns: NET_IP_COLUMNS,
        shape: ItemShape::Single,
        item_key: "",
        has_nr: false,
    },
    ActivityDef {
        id: ActivityId::NET_EIP,
        revisions: NET_EIP_REVISIONS,
        columns: NET_EIP_COLUMNS,
        shape: ItemShape::Single,
        item_key: "",
        has_nr: false,
    },
    ActivityDef {
        id: ActivityId::NET_ICMP,
        revisions: NET_ICMP_REVISIONS,
        columns: NET_ICMP_COLUMNS,
        shape: ItemShape::Single,
        item_key: "",
        has_nr: false,
    },
    ActivityDef {
        id: ActivityId::NET_EICMP,
        revisions: NET_EICMP_REVISIONS,
        columns: NET_EICMP_COLUMNS,
        shape: ItemShape::Single,
        item_key: "",
        has_nr: false,
    },
    ActivityDef {
        id: ActivityId::NET_TCP,
        revisions: NET_TCP_REVISIONS,
        columns: NET_TCP_COLUMNS,
        shape: ItemShape::Single,
        item_key: "",
        has_nr: false,
    },
    ActivityDef {
        id: ActivityId::NET_ETCP,
        revisions: NET_ETCP_REVISIONS,
        columns: NET_ETCP_COLUMNS,
        shape: ItemShape::Single,
        item_key: "",
        has_nr: false,
    },
    ActivityDef {
        id: ActivityId::NET_UDP,
        revisions: NET_UDP_REVISIONS,
        columns: NET_UDP_COLUMNS,
        shape: ItemShape::Single,
        item_key: "",
        has_nr: false,
    },
    ActivityDef {
        id: ActivityId::NET_SOCK6,
        revisions: NET_SOCK6_REVISIONS,
        columns: NET_SOCK6_COLUMNS,
        shape: ItemShape::Single,
        item_key: "",
        has_nr: false,
    },
    ActivityDef {
        id: ActivityId::NET_IP6,
        revisions: NET_IP6_REVISIONS,
        columns: NET_IP6_COLUMNS,
        shape: ItemShape::Single,
        item_key: "",
        has_nr: false,
    },
    ActivityDef {
        id: ActivityId::NET_EIP6,
        revisions: NET_EIP6_REVISIONS,
        columns: NET_EIP6_COLUMNS,
        shape: ItemShape::Single,
        item_key: "",
        has_nr: false,
    },
    ActivityDef {
        id: ActivityId::NET_ICMP6,
        revisions: NET_ICMP6_REVISIONS,
        columns: NET_ICMP6_COLUMNS,
        shape: ItemShape::Single,
        item_key: "",
        has_nr: false,
    },
    ActivityDef {
        id: ActivityId::NET_EICMP6,
        revisions: NET_EICMP6_REVISIONS,
        columns: NET_EICMP6_COLUMNS,
        shape: ItemShape::Single,
        item_key: "",
        has_nr: false,
    },
    ActivityDef {
        id: ActivityId::NET_UDP6,
        revisions: NET_UDP6_REVISIONS,
        columns: NET_UDP6_COLUMNS,
        shape: ItemShape::Single,
        item_key: "",
        has_nr: false,
    },
    ActivityDef {
        id: ActivityId::NET_FC,
        revisions: NET_FC_REVISIONS,
        columns: NET_FC_COLUMNS,
        shape: ItemShape::List,
        item_key: "fchost_name",
        has_nr: true,
    },
    // item は CPU インデックス順 (index 0 = "all")。名前フィールドを持たないため
    // `item_key` は空文字。
    ActivityDef {
        id: ActivityId::NET_SOFT,
        revisions: NET_SOFT_REVISIONS,
        columns: NET_SOFT_COLUMNS,
        shape: ItemShape::List,
        item_key: "",
        has_nr: true,
    },
];

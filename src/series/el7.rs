//! RHEL / CentOS 7 の sysstat 10.1.5 (`sysstat-10.1.5-*.el7`) の `sar` が行う計算の再現。
//!
//! [`crate::model::SarProfile::Sysstat1015El7`] を選んだときだけ使う。
//! 現行版 (v12.8.0) の計算は [`crate::series::compute`] にあり、**経路を共有しない**。
//! 同じ指標でも式・型・演算順序が版ごとに違い、共通化すると片方の修正が
//! もう片方の出力を黙って変えるためである (`docs/design.md` §9.1)。
//!
//! # 何を再現するか
//!
//! el7 の `pr_stats.c` の各 `print_*_stats()` が画面に出す値そのもの。
//! 数学的に等価な式へ整理してはいけない。本家の丸め境界と一致しなくなる。
//!
//! - **差分の幅は C の型に従う。** `unsigned int` の差は 32bit で、
//!   `unsigned long` の差は採取元の `sizeof(long)` で巻き戻る。
//! - **`S_VALUE` / `SP_VALUE` は `((double)(n - m)) / p * HZ` の順で計算する。**
//! - **平均の整数除算も写す。** `(double) (avg_caskb / avg_count)` は
//!   整数で割ってから `double` にする (現行版は `(double) avg_caskb / avg_count`)。
//! - **el7 の `dyn-tick` パッチ**: `ll_s_value()` / `ll_sp_value()` は後の値が
//!   小さければ 0 を返す (upstream 10.1.5 は 32bit の巻き戻りとして補正する)。
//! - **バッファの書き換えも写す。** オフライン CPU は現サンプルを前サンプルで
//!   上書きし、NIC・ディスクの再登録判定は基準バッファを 0 に戻す。
//!   どちらも次の区間や平均行の値に効く。
//!
//! # 層の分担
//!
//! ここは「前後のバッファと区間長から、行に出す値を求める」まで。
//! どのレコードを表示するか・見出しをいつ出すか・時刻の文字列化は
//! [`crate::output::sar_el7`] が持つ。値の型 ([`Cell`]) は本家の `printf` 変換に
//! 対応させてあり、文字列にするのは出力層の仕事である。

use crate::layout::plan::DecodePlan;
use crate::model::{ActivityId, Availability};
use crate::series::snapshot::ItemSnapshot;

/// `sar` を実行するホストの `HZ` (`sysconf(_SC_CLK_TCK)`)。Linux では常に 100。
pub const HZ: f64 = 100.0;

/// el7 の `NR_CPUS` (`sysstat-10.1.5-max-cpus.patch` で 2048 → 8192)。
pub const NR_CPUS: usize = 8192;

/// el7 の `NR_IRQS`。
pub const NR_IRQS: usize = 1024;

/// 本家の `ACTIVITY_MAGIC_BASE`。
const MAGIC_BASE: u32 = 0x8a;

/// センサ名の表示幅 (`MAX_SENSORS_DEV_LEN`)。
pub const MAX_SENSORS_DEV_LEN: usize = 20;
/// USB の製造元名の表示幅 (`MAX_MANUF_LEN - 1`)。
pub const USB_MANUF_WIDTH: usize = 23;
/// USB の製品名の表示幅 (`MAX_PROD_LEN - 1`)。
pub const USB_PROD_WIDTH: usize = 47;
/// USB の要約リストが一杯になったときの製品名 (`stub_print_pwr_usb_stats()`)。
const USB_OTHER_DEVICES: &str = "Other devices not listed here";

// ============================================================================
// activity の定義
// ============================================================================

/// フィールドの C の型。差分の演算幅を決める。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CTy {
    /// `unsigned int`
    U32,
    /// `unsigned long` (幅は採取元の `sizeof(long)`)
    ULong,
    /// `unsigned long long`
    U64,
    /// `double` (センサ値)。値は IEEE-754 のビット列で持つ。
    F64,
}

/// el7 の 1 activity の定義。
#[derive(Debug)]
pub struct Spec {
    pub id: ActivityId,
    /// el7 の `activity.magic`。これと違う magic の activity は本家が読み飛ばす。
    pub magic: u32,
    /// LP64 での `sizeof(struct stats_*)` (= `file_activity.size` に書かれる値)。
    ///
    /// `A_HUGE` は本家の不具合で `sizeof(struct stats_memory)` の 88 が書かれる。
    pub size_lp64: u32,
    /// 構造体の数値フィールド (宣言順)。名前はレイアウト記述の wire 名。
    pub fields: &'static [(&'static str, CTy)],
    /// 文字列フィールド (宣言順)。
    pub texts: &'static [&'static str],
}

/// 数値フィールドの添字 (各 [`Spec::fields`] の並び)。
pub mod field {
    /// `stats_cpu`
    pub mod cpu {
        pub const USER: usize = 0;
        pub const NICE: usize = 1;
        pub const SYS: usize = 2;
        pub const IDLE: usize = 3;
        pub const IOWAIT: usize = 4;
        pub const STEAL: usize = 5;
        pub const HARDIRQ: usize = 6;
        pub const SOFTIRQ: usize = 7;
        pub const GUEST: usize = 8;
        pub const GUEST_NICE: usize = 9;
    }
    /// `stats_pcsw`
    pub mod pcsw {
        pub const CONTEXT_SWITCH: usize = 0;
        pub const PROCESSES: usize = 1;
    }
    /// `stats_paging`
    pub mod page {
        pub const PGSCAN_KSWAPD: usize = 5;
        pub const PGSCAN_DIRECT: usize = 6;
        pub const PGSTEAL: usize = 7;
    }
    /// `stats_memory`
    pub mod mem {
        pub const FRMKB: usize = 0;
        pub const BUFKB: usize = 1;
        pub const CAMKB: usize = 2;
        pub const TLMKB: usize = 3;
        pub const FRSKB: usize = 4;
        pub const TLSKB: usize = 5;
        pub const CASKB: usize = 6;
        pub const COMKB: usize = 7;
        pub const ACTIVEKB: usize = 8;
        pub const INACTKB: usize = 9;
        pub const DIRTYKB: usize = 10;
    }
    /// `stats_ktables`
    pub mod ktables {
        pub const FILE_USED: usize = 0;
        pub const INODE_USED: usize = 1;
        pub const DENTRY_STAT: usize = 2;
        pub const PTY_NR: usize = 3;
    }
    /// `stats_queue`
    pub mod queue {
        pub const NR_RUNNING: usize = 0;
        pub const PROCS_BLOCKED: usize = 1;
        pub const LOAD_AVG_1: usize = 2;
        pub const LOAD_AVG_5: usize = 3;
        pub const LOAD_AVG_15: usize = 4;
        pub const NR_THREADS: usize = 5;
    }
    /// `stats_serial`
    pub mod serial {
        pub const LINE: usize = 6;
    }
    /// `stats_disk`
    pub mod disk {
        pub const NR_IOS: usize = 0;
        pub const RD_SECT: usize = 1;
        pub const WR_SECT: usize = 2;
        pub const RD_TICKS: usize = 3;
        pub const WR_TICKS: usize = 4;
        pub const TOT_TICKS: usize = 5;
        pub const RQ_TICKS: usize = 6;
        pub const MAJOR: usize = 7;
        pub const MINOR: usize = 8;
    }
    /// `stats_net_dev`
    pub mod net_dev {
        pub const RX_PACKETS: usize = 0;
        pub const TX_PACKETS: usize = 1;
        pub const RX_BYTES: usize = 2;
        pub const TX_BYTES: usize = 3;
        pub const RX_COMPRESSED: usize = 4;
        pub const TX_COMPRESSED: usize = 5;
        pub const MULTICAST: usize = 6;
    }
    /// `stats_net_edev`
    pub mod net_edev {
        pub const COLLISIONS: usize = 0;
        pub const RX_ERRORS: usize = 1;
        pub const TX_ERRORS: usize = 2;
        pub const RX_DROPPED: usize = 3;
        pub const TX_DROPPED: usize = 4;
        pub const RX_FIFO_ERRORS: usize = 5;
        pub const TX_FIFO_ERRORS: usize = 6;
        pub const RX_FRAME_ERRORS: usize = 7;
        pub const TX_CARRIER_ERRORS: usize = 8;
    }
    /// `stats_net_sock`
    pub mod sock {
        pub const SOCK_INUSE: usize = 0;
        pub const TCP_INUSE: usize = 1;
        pub const TCP_TW: usize = 2;
        pub const UDP_INUSE: usize = 3;
        pub const RAW_INUSE: usize = 4;
        pub const FRAG_INUSE: usize = 5;
    }
    /// `stats_pwr_fan` / `stats_pwr_temp` / `stats_pwr_in` (値・最小・最大)
    pub mod sensor {
        pub const VALUE: usize = 0;
        pub const MIN: usize = 1;
        pub const MAX: usize = 2;
    }
    /// `stats_huge`
    pub mod huge {
        pub const FRHKB: usize = 0;
        pub const TLHKB: usize = 1;
    }
    /// `stats_pwr_wghfreq`
    pub mod wghfreq {
        pub const TIME_IN_STATE: usize = 0;
        pub const FREQ: usize = 1;
    }
    /// `stats_pwr_usb`
    pub mod usb {
        pub const BUS_NR: usize = 0;
        pub const VENDOR_ID: usize = 1;
        pub const PRODUCT_ID: usize = 2;
        pub const BMAXPOWER: usize = 3;
    }
    /// `stats_filesystem`
    pub mod fs {
        pub const F_BLOCKS: usize = 0;
        pub const F_BFREE: usize = 1;
        pub const F_BAVAIL: usize = 2;
        pub const F_FILES: usize = 3;
        pub const F_FFREE: usize = 4;
    }
}

use CTy::{F64, U32, U64, ULong};

/// el7 が知っている activity (`act[]` の順)。
pub static SPECS: &[Spec] = &[
    Spec {
        id: ActivityId::CPU,
        magic: MAGIC_BASE,
        size_lp64: 160,
        fields: &[
            ("cpu_user", U64),
            ("cpu_nice", U64),
            ("cpu_sys", U64),
            ("cpu_idle", U64),
            ("cpu_iowait", U64),
            ("cpu_steal", U64),
            ("cpu_hardirq", U64),
            ("cpu_softirq", U64),
            ("cpu_guest", U64),
            ("cpu_guest_nice", U64),
        ],
        texts: &[],
    },
    Spec {
        id: ActivityId::PCSW,
        magic: MAGIC_BASE,
        size_lp64: 32,
        fields: &[("context_switch", U64), ("processes", ULong)],
        texts: &[],
    },
    Spec {
        id: ActivityId::IRQ,
        magic: MAGIC_BASE,
        size_lp64: 16,
        fields: &[("irq_nr", U64)],
        texts: &[],
    },
    Spec {
        id: ActivityId::SWAP,
        magic: MAGIC_BASE,
        size_lp64: 16,
        fields: &[("pswpin", ULong), ("pswpout", ULong)],
        texts: &[],
    },
    Spec {
        id: ActivityId::PAGE,
        magic: MAGIC_BASE,
        size_lp64: 64,
        fields: &[
            ("pgpgin", ULong),
            ("pgpgout", ULong),
            ("pgfault", ULong),
            ("pgmajfault", ULong),
            ("pgfree", ULong),
            ("pgscan_kswapd", ULong),
            ("pgscan_direct", ULong),
            ("pgsteal", ULong),
        ],
        texts: &[],
    },
    Spec {
        id: ActivityId::IO,
        magic: MAGIC_BASE + 1,
        size_lp64: 48,
        fields: &[
            ("dk_drive", U64),
            ("dk_drive_rio", U64),
            ("dk_drive_wio", U64),
            ("dk_drive_rblk", U64),
            ("dk_drive_wblk", U64),
        ],
        texts: &[],
    },
    Spec {
        id: ActivityId::MEMORY,
        magic: MAGIC_BASE,
        size_lp64: 88,
        fields: &[
            ("frmkb", ULong),
            ("bufkb", ULong),
            ("camkb", ULong),
            ("tlmkb", ULong),
            ("frskb", ULong),
            ("tlskb", ULong),
            ("caskb", ULong),
            ("comkb", ULong),
            ("activekb", ULong),
            ("inactkb", ULong),
            ("dirtykb", ULong),
        ],
        texts: &[],
    },
    Spec {
        id: ActivityId::KTABLES,
        magic: MAGIC_BASE,
        size_lp64: 16,
        fields: &[
            ("file_used", U32),
            ("inode_used", U32),
            ("dentry_stat", U32),
            ("pty_nr", U32),
        ],
        texts: &[],
    },
    Spec {
        id: ActivityId::QUEUE,
        magic: MAGIC_BASE + 1,
        size_lp64: 32,
        fields: &[
            ("nr_running", ULong),
            ("procs_blocked", ULong),
            ("load_avg_1", U32),
            ("load_avg_5", U32),
            ("load_avg_15", U32),
            ("nr_threads", U32),
        ],
        texts: &[],
    },
    Spec {
        id: ActivityId::SERIAL,
        magic: MAGIC_BASE,
        size_lp64: 28,
        fields: &[
            ("rx", U32),
            ("tx", U32),
            ("frame", U32),
            ("parity", U32),
            ("brk", U32),
            ("overrun", U32),
            ("line", U32),
        ],
        texts: &[],
    },
    Spec {
        id: ActivityId::DISK,
        magic: MAGIC_BASE + 1,
        size_lp64: 64,
        fields: &[
            ("nr_ios", U64),
            ("rd_sect", ULong),
            ("wr_sect", ULong),
            ("rd_ticks", U32),
            ("wr_ticks", U32),
            ("tot_ticks", U32),
            ("rq_ticks", U32),
            ("major", U32),
            ("minor", U32),
        ],
        texts: &[],
    },
    Spec {
        id: ActivityId::NET_DEV,
        magic: MAGIC_BASE + 1,
        size_lp64: 128,
        fields: &[
            ("rx_packets", U64),
            ("tx_packets", U64),
            ("rx_bytes", U64),
            ("tx_bytes", U64),
            ("rx_compressed", U64),
            ("tx_compressed", U64),
            ("multicast", U64),
        ],
        texts: &["interface"],
    },
    Spec {
        id: ActivityId::NET_EDEV,
        magic: MAGIC_BASE + 1,
        size_lp64: 160,
        fields: &[
            ("collisions", U64),
            ("rx_errors", U64),
            ("tx_errors", U64),
            ("rx_dropped", U64),
            ("tx_dropped", U64),
            ("rx_fifo_errors", U64),
            ("tx_fifo_errors", U64),
            ("rx_frame_errors", U64),
            ("tx_carrier_errors", U64),
        ],
        texts: &["interface"],
    },
    Spec {
        id: ActivityId::NET_NFS,
        magic: MAGIC_BASE,
        size_lp64: 24,
        fields: &[
            ("nfs_rpccnt", U32),
            ("nfs_rpcretrans", U32),
            ("nfs_readcnt", U32),
            ("nfs_writecnt", U32),
            ("nfs_accesscnt", U32),
            ("nfs_getattcnt", U32),
        ],
        texts: &[],
    },
    Spec {
        id: ActivityId::NET_NFSD,
        magic: MAGIC_BASE,
        size_lp64: 44,
        fields: &[
            ("nfsd_rpccnt", U32),
            ("nfsd_rpcbad", U32),
            ("nfsd_netcnt", U32),
            ("nfsd_netudpcnt", U32),
            ("nfsd_nettcpcnt", U32),
            ("nfsd_rchits", U32),
            ("nfsd_rcmisses", U32),
            ("nfsd_readcnt", U32),
            ("nfsd_writecnt", U32),
            ("nfsd_accesscnt", U32),
            ("nfsd_getattcnt", U32),
        ],
        texts: &[],
    },
    Spec {
        id: ActivityId::NET_SOCK,
        magic: MAGIC_BASE,
        size_lp64: 24,
        fields: &[
            ("sock_inuse", U32),
            ("tcp_inuse", U32),
            ("tcp_tw", U32),
            ("udp_inuse", U32),
            ("raw_inuse", U32),
            ("frag_inuse", U32),
        ],
        texts: &[],
    },
    Spec {
        id: ActivityId::NET_IP,
        magic: MAGIC_BASE + 1,
        size_lp64: 128,
        fields: &[
            ("InReceives", U64),
            ("ForwDatagrams", U64),
            ("InDelivers", U64),
            ("OutRequests", U64),
            ("ReasmReqds", U64),
            ("ReasmOKs", U64),
            ("FragOKs", U64),
            ("FragCreates", U64),
        ],
        texts: &[],
    },
    Spec {
        id: ActivityId::NET_EIP,
        magic: MAGIC_BASE + 1,
        size_lp64: 128,
        fields: &[
            ("InHdrErrors", U64),
            ("InAddrErrors", U64),
            ("InUnknownProtos", U64),
            ("InDiscards", U64),
            ("OutDiscards", U64),
            ("OutNoRoutes", U64),
            ("ReasmFails", U64),
            ("FragFails", U64),
        ],
        texts: &[],
    },
    Spec {
        id: ActivityId::NET_ICMP,
        magic: MAGIC_BASE,
        size_lp64: 112,
        fields: &[
            ("InMsgs", ULong),
            ("OutMsgs", ULong),
            ("InEchos", ULong),
            ("InEchoReps", ULong),
            ("OutEchos", ULong),
            ("OutEchoReps", ULong),
            ("InTimestamps", ULong),
            ("InTimestampReps", ULong),
            ("OutTimestamps", ULong),
            ("OutTimestampReps", ULong),
            ("InAddrMasks", ULong),
            ("InAddrMaskReps", ULong),
            ("OutAddrMasks", ULong),
            ("OutAddrMaskReps", ULong),
        ],
        texts: &[],
    },
    Spec {
        id: ActivityId::NET_EICMP,
        magic: MAGIC_BASE,
        size_lp64: 96,
        fields: &[
            ("InErrors", ULong),
            ("OutErrors", ULong),
            ("InDestUnreachs", ULong),
            ("OutDestUnreachs", ULong),
            ("InTimeExcds", ULong),
            ("OutTimeExcds", ULong),
            ("InParmProbs", ULong),
            ("OutParmProbs", ULong),
            ("InSrcQuenchs", ULong),
            ("OutSrcQuenchs", ULong),
            ("InRedirects", ULong),
            ("OutRedirects", ULong),
        ],
        texts: &[],
    },
    Spec {
        id: ActivityId::NET_TCP,
        magic: MAGIC_BASE,
        size_lp64: 32,
        fields: &[
            ("ActiveOpens", ULong),
            ("PassiveOpens", ULong),
            ("InSegs", ULong),
            ("OutSegs", ULong),
        ],
        texts: &[],
    },
    Spec {
        id: ActivityId::NET_ETCP,
        magic: MAGIC_BASE,
        size_lp64: 40,
        fields: &[
            ("AttemptFails", ULong),
            ("EstabResets", ULong),
            ("RetransSegs", ULong),
            ("InErrs", ULong),
            ("OutRsts", ULong),
        ],
        texts: &[],
    },
    Spec {
        id: ActivityId::NET_UDP,
        magic: MAGIC_BASE,
        size_lp64: 32,
        fields: &[
            ("InDatagrams", ULong),
            ("OutDatagrams", ULong),
            ("NoPorts", ULong),
            ("InErrors", ULong),
        ],
        texts: &[],
    },
    Spec {
        id: ActivityId::NET_SOCK6,
        magic: MAGIC_BASE,
        size_lp64: 16,
        fields: &[
            ("tcp6_inuse", U32),
            ("udp6_inuse", U32),
            ("raw6_inuse", U32),
            ("frag6_inuse", U32),
        ],
        texts: &[],
    },
    Spec {
        id: ActivityId::NET_IP6,
        magic: MAGIC_BASE + 1,
        size_lp64: 160,
        fields: &[
            ("InReceives6", U64),
            ("OutForwDatagrams6", U64),
            ("InDelivers6", U64),
            ("OutRequests6", U64),
            ("ReasmReqds6", U64),
            ("ReasmOKs6", U64),
            ("InMcastPkts6", U64),
            ("OutMcastPkts6", U64),
            ("FragOKs6", U64),
            ("FragCreates6", U64),
        ],
        texts: &[],
    },
    Spec {
        id: ActivityId::NET_EIP6,
        magic: MAGIC_BASE + 1,
        size_lp64: 176,
        fields: &[
            ("InHdrErrors6", U64),
            ("InAddrErrors6", U64),
            ("InUnknownProtos6", U64),
            ("InTooBigErrors6", U64),
            ("InDiscards6", U64),
            ("OutDiscards6", U64),
            ("InNoRoutes6", U64),
            ("OutNoRoutes6", U64),
            ("ReasmFails6", U64),
            ("FragFails6", U64),
            ("InTruncatedPkts6", U64),
        ],
        texts: &[],
    },
    Spec {
        id: ActivityId::NET_ICMP6,
        magic: MAGIC_BASE,
        size_lp64: 136,
        fields: &[
            ("InMsgs6", ULong),
            ("OutMsgs6", ULong),
            ("InEchos6", ULong),
            ("InEchoReplies6", ULong),
            ("OutEchoReplies6", ULong),
            ("InGroupMembQueries6", ULong),
            ("InGroupMembResponses6", ULong),
            ("OutGroupMembResponses6", ULong),
            ("InGroupMembReductions6", ULong),
            ("OutGroupMembReductions6", ULong),
            ("InRouterSolicits6", ULong),
            ("OutRouterSolicits6", ULong),
            ("InRouterAdvertisements6", ULong),
            ("InNeighborSolicits6", ULong),
            ("OutNeighborSolicits6", ULong),
            ("InNeighborAdvertisements6", ULong),
            ("OutNeighborAdvertisements6", ULong),
        ],
        texts: &[],
    },
    Spec {
        id: ActivityId::NET_EICMP6,
        magic: MAGIC_BASE,
        size_lp64: 88,
        fields: &[
            ("InErrors6", ULong),
            ("InDestUnreachs6", ULong),
            ("OutDestUnreachs6", ULong),
            ("InTimeExcds6", ULong),
            ("OutTimeExcds6", ULong),
            ("InParmProblems6", ULong),
            ("OutParmProblems6", ULong),
            ("InRedirects6", ULong),
            ("OutRedirects6", ULong),
            ("InPktTooBigs6", ULong),
            ("OutPktTooBigs6", ULong),
        ],
        texts: &[],
    },
    Spec {
        id: ActivityId::NET_UDP6,
        magic: MAGIC_BASE,
        size_lp64: 32,
        fields: &[
            ("InDatagrams6", ULong),
            ("OutDatagrams6", ULong),
            ("NoPorts6", ULong),
            ("InErrors6", ULong),
        ],
        texts: &[],
    },
    Spec {
        id: ActivityId::PWR_CPU,
        magic: MAGIC_BASE,
        size_lp64: 8,
        fields: &[("cpufreq", ULong)],
        texts: &[],
    },
    Spec {
        id: ActivityId::PWR_FAN,
        magic: MAGIC_BASE,
        size_lp64: 40,
        fields: &[("rpm", F64), ("rpm_min", F64)],
        texts: &["device"],
    },
    Spec {
        id: ActivityId::PWR_TEMP,
        magic: MAGIC_BASE,
        size_lp64: 48,
        fields: &[("temp", F64), ("temp_min", F64), ("temp_max", F64)],
        texts: &["device"],
    },
    Spec {
        id: ActivityId::PWR_IN,
        magic: MAGIC_BASE,
        size_lp64: 48,
        fields: &[("in", F64), ("in_min", F64), ("in_max", F64)],
        texts: &["device"],
    },
    Spec {
        id: ActivityId::HUGE,
        magic: MAGIC_BASE,
        size_lp64: 88,
        fields: &[("frhkb", ULong), ("tlhkb", ULong)],
        texts: &[],
    },
    Spec {
        id: ActivityId::PWR_FREQ,
        magic: MAGIC_BASE,
        size_lp64: 32,
        fields: &[("time_in_state", U64), ("freq", ULong)],
        texts: &[],
    },
    Spec {
        id: ActivityId::PWR_USB,
        magic: MAGIC_BASE,
        size_lp64: 88,
        fields: &[
            ("bus_nr", U32),
            ("vendor_id", U32),
            ("product_id", U32),
            ("bmaxpower", U32),
        ],
        texts: &["manufacturer", "product"],
    },
    Spec {
        id: ActivityId::FS,
        magic: MAGIC_BASE,
        size_lp64: 336,
        fields: &[
            ("f_blocks", U64),
            ("f_bfree", U64),
            ("f_bavail", U64),
            ("f_files", U64),
            ("f_ffree", U64),
        ],
        texts: &["fs_name", "mountp"],
    },
];

/// activity の定義を引く。el7 が知らない activity は `None`。
pub fn spec(id: ActivityId) -> Option<&'static Spec> {
    SPECS.iter().find(|s| s.id == id)
}

/// ビットマップを使う activity か (`CPU` / `IRQ` / `PWR_CPU` / `PWR_FREQ`)。
///
/// 見出しの再表示の数え方 (`inc`) がビット数になる。
pub fn uses_bitmap(id: ActivityId) -> bool {
    matches!(
        id,
        ActivityId::CPU | ActivityId::IRQ | ActivityId::PWR_CPU | ActivityId::PWR_FREQ
    )
}

// ============================================================================
// ビットマップ
// ============================================================================

/// 本家の `struct act_bitmap` (`b_array` + `b_size`)。
///
/// バイト数は `BITMAP_SIZE(b_size) = ((b_size + 1) / 8) + 1`。
/// `-P ALL` / `-A` は**このバイト数を丸ごと `~0` で埋める**ので、立つビット数は
/// 実 CPU 数ではなく `BITMAP_SIZE(NR_CPUS) × 8` (el7 では 8200) になる。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bitmap {
    b_size: usize,
    bytes: Vec<u8>,
}

impl Bitmap {
    /// 何も立っていないビットマップ。
    pub fn new(b_size: usize) -> Self {
        Self {
            b_size,
            bytes: vec![0; (b_size + 1) / 8 + 1],
        }
    }

    /// CPU 用 (`b_size = NR_CPUS`)。
    pub fn cpu() -> Self {
        Self::new(NR_CPUS)
    }

    /// 割り込み用 (`b_size = NR_IRQS`)。
    pub fn irq() -> Self {
        Self::new(NR_IRQS)
    }

    /// 本家の `b_size`。表示ループは `i < b_size + 1` で打ち切る。
    pub fn b_size(&self) -> usize {
        self.b_size
    }

    /// `set_bitmap(b_array, ~0, BITMAP_SIZE(b_size))`。
    pub fn set_all(&mut self) {
        self.bytes.fill(0xff);
    }

    /// ビット `i` を立てる。範囲外は無視する。
    pub fn set(&mut self, i: usize) {
        if let Some(b) = self.bytes.get_mut(i >> 3) {
            *b |= 1 << (i & 7);
        }
    }

    /// バイト単位で OR する (`-I ALL` / `-I XALL` が直接バイトを書く)。
    pub fn or_byte(&mut self, index: usize, value: u8) {
        if let Some(b) = self.bytes.get_mut(index) {
            *b |= value;
        }
    }

    /// バイトを直接書く。
    pub fn set_byte(&mut self, index: usize, value: u8) {
        if let Some(b) = self.bytes.get_mut(index) {
            *b = value;
        }
    }

    /// バイトを読む。
    pub fn byte(&self, index: usize) -> u8 {
        self.bytes.get(index).copied().unwrap_or(0)
    }

    /// ビット `i` が立っているか。
    pub fn is_set(&self, i: usize) -> bool {
        self.bytes
            .get(i >> 3)
            .is_some_and(|b| b & (1 << (i & 7)) != 0)
    }

    /// `count_bits()`。
    pub fn count_bits(&self) -> u64 {
        self.bytes.iter().map(|b| u64::from(b.count_ones())).sum()
    }
}

// ============================================================================
// item とデコード
// ============================================================================

/// el7 の構造体 1 個分の値。
///
/// 本家は構造体をそのままバッファに持ち、表示中に書き換える
/// (オフライン CPU の上書き、再登録判定の 0 クリア、USB / FS の要約リスト)。
/// その書き換えを再現するため、レイアウト層の [`ItemSnapshot`] から
/// 自前の可変な表現へ写してから使う。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Item {
    /// 数値フィールド ([`Spec::fields`] の順)。`double` はビット列のまま。
    pub v: Vec<u64>,
    /// 文字列フィールド ([`Spec::texts`] の順)。NUL で終わる C 文字列の中身。
    pub t: Vec<String>,
}

impl Item {
    /// 全フィールドが 0 の item (`memset(0)` 相当)。
    pub fn zeroed(spec: &Spec) -> Self {
        Self {
            v: vec![0; spec.fields.len()],
            t: vec![String::new(); spec.texts.len()],
        }
    }

    /// `double` フィールドを読む。
    pub fn f(&self, i: usize) -> f64 {
        f64::from_bits(self.v[i])
    }

    /// 文字列フィールドを読む。
    pub fn text(&self, i: usize) -> &str {
        self.t.get(i).map(String::as_str).unwrap_or("")
    }
}

/// 1 activity 分のバッファ (`a->buf[n]`)。
pub type Buf = Vec<Item>;

/// ファイルのデコード計画と el7 のフィールドの対応。
#[derive(Debug, Clone)]
pub struct Decoder {
    spec: &'static Spec,
    /// 各フィールドの計画上の位置。このファイルに無いフィールドは `None` (0 として読む)。
    index: Vec<Option<usize>>,
    /// 各フィールドの差分の幅 (ビット)。
    bits: Vec<u32>,
    /// 文字列フィールドの `ItemSnapshot::texts` 上の位置。
    text_index: Vec<Option<usize>>,
    /// `unsigned long` の幅 (ビット)。
    ulong_bits: u32,
}

impl Decoder {
    /// `ulong_bits` は採取元の `sizeof(long) × 8`。
    pub fn new(spec: &'static Spec, plan: &DecodePlan, ulong_bits: u32) -> Self {
        let mut index = Vec::with_capacity(spec.fields.len());
        let mut bits = Vec::with_capacity(spec.fields.len());
        for (name, ty) in spec.fields {
            index.push(plan.fields.iter().position(|f| f.name == *name));
            bits.push(match ty {
                CTy::U32 => 32,
                CTy::ULong => ulong_bits,
                CTy::U64 | CTy::F64 => 64,
            });
        }
        let text_index = spec.texts.iter().map(|n| plan.text_index(n)).collect();
        Self {
            spec,
            index,
            bits,
            text_index,
            ulong_bits,
        }
    }

    /// この activity の定義。
    pub fn spec(&self) -> &'static Spec {
        self.spec
    }

    /// 計画に載っていない (レイアウト記述に名前が無い) フィールドの名前。
    ///
    /// 空でなければ el7 の構造体と読み出し側の定義が食い違っている。
    pub fn unresolved_fields(&self) -> Vec<&'static str> {
        self.spec
            .fields
            .iter()
            .zip(&self.index)
            .filter(|(_, i)| i.is_none())
            .map(|((n, _), _)| *n)
            .collect()
    }

    /// フィールドの差分の幅。
    pub fn bits(&self, field: usize) -> u32 {
        self.bits[field]
    }

    /// `unsigned long` の幅。
    pub fn ulong_bits(&self) -> u32 {
        self.ulong_bits
    }

    /// スナップショットの item を el7 の構造体として写す。
    ///
    /// 読めなかったフィールドは 0 (本家のバッファは確保時に 0 で埋まっている)。
    pub fn item(&self, snap: &ItemSnapshot) -> Item {
        let v = self
            .index
            .iter()
            .map(|i| match i.and_then(|i| snap.values.get(i)) {
                Some(Availability::Present(v)) => *v,
                _ => 0,
            })
            .collect();
        let t = self
            .text_index
            .iter()
            .map(|i| {
                i.and_then(|i| snap.text(i))
                    .map(str::to_string)
                    .unwrap_or_default()
            })
            .collect();
        Item { v, t }
    }

    /// activity 1 レコード分の item を写す。
    pub fn buf(&self, items: &[ItemSnapshot]) -> Buf {
        items.iter().map(|s| self.item(s)).collect()
    }
}

// ============================================================================
// C の算術
// ============================================================================

/// `n - m` を `bits` 幅の符号なし整数で計算する (C の減算の巻き戻り)。
#[inline]
pub fn sub(n: u64, m: u64, bits: u32) -> u64 {
    mask(n.wrapping_sub(m), bits)
}

/// `a + b` を `bits` 幅で計算する。
#[inline]
pub fn add(a: u64, b: u64, bits: u32) -> u64 {
    mask(a.wrapping_add(b), bits)
}

#[inline]
fn mask(v: u64, bits: u32) -> u64 {
    if bits >= 64 {
        v
    } else {
        v & ((1u64 << bits) - 1)
    }
}

/// `S_VALUE(m, n, p)` = `((double) ((n) - (m))) / (p) * HZ`。
#[inline]
pub fn s_value(m: u64, n: u64, bits: u32, p: u64) -> f64 {
    sub(n, m, bits) as f64 / p as f64 * HZ
}

/// `SP_VALUE(m, n, p)` = `((double) ((n) - (m))) / (p) * 100`。
#[inline]
pub fn sp_value(m: u64, n: u64, bits: u32, p: u64) -> f64 {
    sub(n, m, bits) as f64 / p as f64 * 100.0
}

/// `ll_s_value()`。el7 の `dyn-tick` パッチにより、逆行は 0 になる。
#[inline]
pub fn ll_s_value(value1: u64, value2: u64, itv: u64) -> f64 {
    if value2 < value1 {
        0.0
    } else {
        (value2 - value1) as f64 / itv as f64 * HZ
    }
}

/// `ll_sp_value()`。el7 の `dyn-tick` パッチにより、逆行は 0 になる。
#[inline]
pub fn ll_sp_value(value1: u64, value2: u64, itv: u64) -> f64 {
    if value2 < value1 {
        0.0
    } else {
        (value2 - value1) as f64 / itv as f64 * 100.0
    }
}

/// `get_interval()`。0 は 1 に置き換える。
#[inline]
pub fn get_interval(prev_uptime: u64, curr_uptime: u64) -> u64 {
    match curr_uptime.wrapping_sub(prev_uptime) {
        0 => 1,
        itv => itv,
    }
}

// ============================================================================
// 行の値
// ============================================================================

/// 1 セルの値と、本家の `printf` がどの変換で出すか。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Cell {
    /// `" %9.2f"`
    F92(f64),
    /// `"    %6.2f"` (CPU の各列・`%memused` など)
    P62(f64),
    /// `"   %7.2f"` (`%commit`)
    P72(f64),
    /// `" %9.0f"` (平均の kB・件数、FS の MB)
    F90(f64),
    /// `" %9lu"` / `" %9u"` / `" %9llu"`
    U9(u64),
    /// `" %9x"`
    X9(u64),
    /// `"       N/A"` (シリアル回線の番号が前サンプルと食い違ったとき)
    Na,
}

/// 行頭のタイムスタンプ直後に出す識別子。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Label {
    /// 識別子なし。
    None,
    /// `"     all"` (CPU 系の集約行)
    All,
    /// `"     %3d"` (CPU 系の CPU 番号、センサ番号)
    Num3(i64),
    /// `"       sum"` (割り込みの合計)
    Sum,
    /// `"       %3d"` (割り込み番号、シリアル回線)
    Num3Wide(i64),
    /// `" %9s"` (デバイス名・インターフェース名)
    Name(String),
    /// `"  %6d"` (USB のバス番号)
    Bus(i64),
}

/// 行末に出す文字列。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tail {
    /// なし。
    None,
    /// `" %20s"` (センサのデバイス名)
    Sensor(String),
    /// `" %23s" " %47s"` (USB の製造元と製品名)
    Usb(String, String),
    /// `" %s"` (FS のファイルシステム名 / マウントポイント)
    Name(String),
}

/// 1 行分の値。
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub label: Label,
    pub cells: Vec<Cell>,
    pub tail: Tail,
}

impl Row {
    fn plain(cells: Vec<Cell>) -> Self {
        Self {
            label: Label::None,
            cells,
            tail: Tail::None,
        }
    }
}

/// `A_MEMORY` の出力の種類 (`AO_MULTIPLE_OUTPUTS` のマスク)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemOutput {
    /// `-R` (`AO_F_MEM_DIA`)
    Dia,
    /// `-r` (`AO_F_MEM_AMT`)
    Amt,
    /// `-S` (`AO_F_MEM_SWAP`)
    Swap,
}

/// 表示の設定 (activity のオプションフラグと選択)。
#[derive(Debug, Clone)]
pub struct PrintOptions {
    /// `-u ALL` (`AO_F_CPU_ALL`)。偽なら `AO_F_CPU_DEF`。
    pub cpu_all: bool,
    /// `A_CPU` / `A_PWR_CPU` / `A_PWR_FREQ` のビットマップ。
    pub cpu_bitmap: Bitmap,
    /// `A_IRQ` のビットマップ。
    pub irq_bitmap: Bitmap,
    /// `-F MOUNT`。
    pub fs_mount: bool,
    /// `KB_TO_PG()` のシフト量。
    pub kb_shift: u32,
}

/// 平均行を出すときの情報。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AvgInfo {
    /// `avg_count` (区間で表示したサンプル数)。
    pub count: u64,
}

/// `print_*_stats()` の関数内 `static` に当たる、平均のための累積。
///
/// 平均行を出すと本家と同じく 0 に戻す。activity × 出力ごとに 1 つ持つ。
#[derive(Debug, Clone, Default)]
pub struct Accum {
    /// 単一 item の activity の累積 (フィールド添字で引く)。
    sums: Vec<u64>,
    /// CPU 周波数 (`avg_cpufreq[i]`)。
    per_item: Vec<u64>,
    /// センサ値の累積 (`avg_fan[i]` / `avg_temp[i]` / `avg_in[i]`)。
    sensor: Vec<f64>,
    /// センサの最小値 (`avg_fan_min[i]` は累積、温度と電圧は最後の値)。
    sensor_min: Vec<f64>,
    /// センサの最大値 (温度と電圧の最後の値)。
    sensor_max: Vec<f64>,
}

impl Accum {
    fn sum(&mut self, n: usize) -> &mut Vec<u64> {
        if self.sums.len() < n {
            self.sums.resize(n, 0);
        }
        &mut self.sums
    }

    fn reset(&mut self) {
        *self = Accum::default();
    }
}

/// 1 回の `f_print` / `f_print_avg` 呼び出しの入力。
pub struct Print<'a> {
    pub dec: &'a Decoder,
    pub opts: &'a PrintOptions,
    /// `NEED_GLOBAL_ITV` なら `g_itv`、それ以外は `itv` (jiffies)。
    pub itv: u64,
    /// `Some` なら平均行 (`f_print_avg`)。`prev` は区間の最初のサンプル (`buf[2]`)。
    pub avg: Option<AvgInfo>,
    /// `A_MEMORY` の出力の種類。
    pub mem: MemOutput,
    /// 行列型 activity (`A_PWR_WGHFREQ`) の `nr2`。それ以外は 1。
    pub nr2: usize,
}

/// `f_print` / `f_print_avg` の本体。表示する行を返す。
///
/// `prev` と `curr` は**本家と同じく書き換えられる** (モジュール冒頭の説明)。
///
/// `summary` は USB / FS の要約リスト (本家の `buf[2]`)。瞬時値の行を出すたびに
/// 見た装置をここへ入れる。平均行では `prev` がその `buf[2]` そのものなので
/// `None` を渡し、USB / FS は `prev` を要約リストとして出す。
pub fn print_rows(
    p: &Print<'_>,
    prev: &mut Buf,
    curr: &mut Buf,
    summary: Option<&mut Buf>,
    acc: &mut Accum,
) -> Vec<Row> {
    let id = p.dec.spec().id;
    let rows = match id {
        ActivityId::CPU => cpu_rows(p, prev, curr),
        ActivityId::PCSW => one(prev, curr, |pr, cu| {
            vec![
                Cell::F92(s_value(
                    pr.v[field::pcsw::PROCESSES],
                    cu.v[field::pcsw::PROCESSES],
                    p.dec.bits(field::pcsw::PROCESSES),
                    p.itv,
                )),
                Cell::F92(ll_s_value(
                    pr.v[field::pcsw::CONTEXT_SWITCH],
                    cu.v[field::pcsw::CONTEXT_SWITCH],
                    p.itv,
                )),
            ]
        }),
        ActivityId::IRQ => irq_rows(p, prev, curr),
        ActivityId::PAGE => one(prev, curr, |pr, cu| paging_cells(p, pr, cu)),
        ActivityId::MEMORY => memory_rows(p, prev, curr, acc),
        ActivityId::KTABLES => {
            use field::ktables::*;
            gauge_rows(p, curr, acc, &[DENTRY_STAT, FILE_USED, INODE_USED, PTY_NR])
        }
        ActivityId::QUEUE => queue_rows(p, curr, acc),
        ActivityId::SERIAL => serial_rows(p, prev, curr),
        ActivityId::DISK => disk_rows(p, prev, curr),
        ActivityId::NET_DEV => net_dev_rows(p, prev, curr),
        ActivityId::NET_EDEV => net_edev_rows(p, prev, curr),
        ActivityId::NET_SOCK => {
            use field::sock::*;
            gauge_rows(
                p,
                curr,
                acc,
                &[
                    SOCK_INUSE, TCP_INUSE, UDP_INUSE, RAW_INUSE, FRAG_INUSE, TCP_TW,
                ],
            )
        }
        ActivityId::NET_SOCK6 => gauge_rows(p, curr, acc, &[0, 1, 2, 3]),
        ActivityId::PWR_CPU => cpufreq_rows(p, curr, acc),
        ActivityId::PWR_FAN | ActivityId::PWR_TEMP | ActivityId::PWR_IN => {
            sensor_rows(p, curr, acc)
        }
        ActivityId::HUGE => huge_rows(p, curr, acc),
        ActivityId::PWR_FREQ => wghfreq_rows(p, prev, curr),
        ActivityId::PWR_USB => usb_rows(p, prev, curr, summary),
        ActivityId::FS => fs_rows(p, prev, curr, summary),
        // 残りはすべて「単一 item の全フィールドを S_VALUE で出す」形
        _ => simple_rate_rows(p, prev, curr),
    };
    if p.avg.is_some() {
        acc.reset();
    }
    rows
}

/// 単一 item の activity で、前後の item を 1 つずつ取り出して行を作る。
fn one(prev: &Buf, curr: &Buf, f: impl FnOnce(&Item, &Item) -> Vec<Cell>) -> Vec<Row> {
    match (prev.first(), curr.first()) {
        (Some(pr), Some(cu)) => vec![Row::plain(f(pr, cu))],
        _ => Vec::new(),
    }
}

/// `S_VALUE` を並べるだけの activity (`A_SWAP` / `A_IO` / NFS / SNMP / IPv6)。
///
/// 表示順が構造体の宣言順と違うものは [`rate_order`] で並べ替える。
fn simple_rate_rows(p: &Print<'_>, prev: &Buf, curr: &Buf) -> Vec<Row> {
    let order = rate_order(p.dec.spec().id);
    one(prev, curr, |pr, cu| {
        order
            .iter()
            .map(|&f| Cell::F92(s_value(pr.v[f], cu.v[f], p.dec.bits(f), p.itv)))
            .collect()
    })
}

/// `S_VALUE` 列の表示順 (構造体のフィールド添字)。
fn rate_order(id: ActivityId) -> Vec<usize> {
    let n = spec(id).map_or(0, |s| s.fields.len());
    (0..n).collect()
}

// ---- A_CPU ----

/// `print_cpu_stats()`。
fn cpu_rows(p: &Print<'_>, prev: &mut Buf, curr: &mut Buf) -> Vec<Row> {
    let bitmap = &p.opts.cpu_bitmap;
    let n = curr.len().min(prev.len()).min(bitmap.b_size() + 1);
    let mut rows = Vec::new();
    for i in 0..n {
        if !bitmap.is_set(i) {
            continue;
        }
        if i == 0 {
            // CPU "all" はファイルの集約スロットを g_itv で割る
            // (現行版は個別 CPU の合計から作り直す)。
            rows.push(Row {
                label: Label::All,
                cells: cpu_values(p.opts.cpu_all, &prev[0], &curr[0], p.itv),
                tail: Tail::None,
            });
            continue;
        }
        let label = Label::Num3(i as i64 - 1);
        // オフライン CPU は /proc/stat に行が無く、全フィールドが 0 のまま読まれる。
        // 本家は**現サンプルを前サンプルで上書きしてから** 0 を並べる。
        // 復帰したときに値が 0 から跳ね上がらないようにするためで、
        // 次の区間の差分はこの上書き後の値から取られる。
        if cpu_sum8(&curr[i]) == 0 {
            curr[i] = prev[i].clone();
            let width = if p.opts.cpu_all { 10 } else { 6 };
            rows.push(Row {
                label,
                cells: vec![Cell::P62(0.0); width],
                tail: Tail::None,
            });
            continue;
        }
        let itv = per_cpu_interval(&curr[i], &prev[i]);
        if itv == 0 {
            // tickless。**`-u ALL` では 9 列しか出ない** (本家の不具合で
            // `%gnice` の位置に 100.00 が入り `%idle` 列が無い)。
            let mut cells = vec![Cell::P62(0.0); 5];
            if p.opts.cpu_all {
                cells.extend([
                    Cell::P62(0.0),
                    Cell::P62(0.0),
                    Cell::P62(0.0),
                    Cell::P62(100.0),
                ]);
            } else {
                cells.push(Cell::P62(100.0));
            }
            rows.push(Row {
                label,
                cells,
                tail: Tail::None,
            });
            continue;
        }
        rows.push(Row {
            label,
            cells: cpu_values(p.opts.cpu_all, &prev[i], &curr[i], itv),
            tail: Tail::None,
        });
    }
    rows
}

/// 8 フィールドの合計 (`guest` / `guest_nice` は `user` / `nice` に含まれるので足さない)。
fn cpu_sum8(c: &Item) -> u64 {
    use field::cpu::*;
    [USER, NICE, SYS, IOWAIT, IDLE, STEAL, HARDIRQ, SOFTIRQ]
        .iter()
        .fold(0u64, |acc, &f| acc.wrapping_add(c.v[f]))
}

/// `get_per_cpu_interval()`。
fn per_cpu_interval(scc: &Item, scp: &Item) -> u64 {
    use field::cpu::*;
    let mut ishift = 0u64;
    let c_user = scc.v[USER].wrapping_sub(scc.v[GUEST]);
    let p_user = scp.v[USER].wrapping_sub(scp.v[GUEST]);
    if c_user < p_user {
        ishift = ishift.wrapping_add(p_user.wrapping_sub(c_user));
    }
    let c_nice = scc.v[NICE].wrapping_sub(scc.v[GUEST_NICE]);
    let p_nice = scp.v[NICE].wrapping_sub(scp.v[GUEST_NICE]);
    if c_nice < p_nice {
        ishift = ishift.wrapping_add(p_nice.wrapping_sub(c_nice));
    }
    cpu_sum8(scc)
        .wrapping_sub(cpu_sum8(scp))
        .wrapping_add(ishift)
}

/// CPU 1 行分の割合。
fn cpu_values(all: bool, p: &Item, c: &Item, itv: u64) -> Vec<Cell> {
    use field::cpu::*;
    let idle = if c.v[IDLE] < p.v[IDLE] {
        0.0
    } else {
        ll_sp_value(p.v[IDLE], c.v[IDLE], itv)
    };
    if !all {
        let sys = |x: &Item| {
            x.v[SYS]
                .wrapping_add(x.v[HARDIRQ])
                .wrapping_add(x.v[SOFTIRQ])
        };
        return [
            ll_sp_value(p.v[USER], c.v[USER], itv),
            ll_sp_value(p.v[NICE], c.v[NICE], itv),
            ll_sp_value(sys(p), sys(c), itv),
            ll_sp_value(p.v[IOWAIT], c.v[IOWAIT], itv),
            ll_sp_value(p.v[STEAL], c.v[STEAL], itv),
            idle,
        ]
        .into_iter()
        .map(Cell::P62)
        .collect();
    }
    let usr_c = c.v[USER].wrapping_sub(c.v[GUEST]);
    let usr_p = p.v[USER].wrapping_sub(p.v[GUEST]);
    let nice_c = c.v[NICE].wrapping_sub(c.v[GUEST_NICE]);
    let nice_p = p.v[NICE].wrapping_sub(p.v[GUEST_NICE]);
    [
        if usr_c < usr_p {
            0.0
        } else {
            ll_sp_value(usr_p, usr_c, itv)
        },
        if nice_c < nice_p {
            0.0
        } else {
            ll_sp_value(nice_p, nice_c, itv)
        },
        ll_sp_value(p.v[SYS], c.v[SYS], itv),
        ll_sp_value(p.v[IOWAIT], c.v[IOWAIT], itv),
        ll_sp_value(p.v[STEAL], c.v[STEAL], itv),
        ll_sp_value(p.v[HARDIRQ], c.v[HARDIRQ], itv),
        ll_sp_value(p.v[SOFTIRQ], c.v[SOFTIRQ], itv),
        ll_sp_value(p.v[GUEST], c.v[GUEST], itv),
        ll_sp_value(p.v[GUEST_NICE], c.v[GUEST_NICE], itv),
        idle,
    ]
    .into_iter()
    .map(Cell::P62)
    .collect()
}

// ---- A_IRQ ----

/// `print_irq_stats()`。行 = 割り込み (0 = 合計)。
fn irq_rows(p: &Print<'_>, prev: &Buf, curr: &Buf) -> Vec<Row> {
    let bitmap = &p.opts.irq_bitmap;
    let n = curr.len().min(prev.len()).min(bitmap.b_size() + 1);
    (0..n)
        .filter(|&i| bitmap.is_set(i))
        .map(|i| Row {
            label: if i == 0 {
                Label::Sum
            } else {
                Label::Num3Wide(i as i64 - 1)
            },
            cells: vec![Cell::F92(ll_s_value(prev[i].v[0], curr[i].v[0], p.itv))],
            tail: Tail::None,
        })
        .collect()
}

// ---- A_PAGE ----

/// `print_paging_stats()`。8 列の `S_VALUE` と `%vmeff`。
fn paging_cells(p: &Print<'_>, pr: &Item, cu: &Item) -> Vec<Cell> {
    use field::page::*;
    let bits = p.dec.ulong_bits();
    let mut cells: Vec<Cell> = (0..8)
        .map(|f| Cell::F92(s_value(pr.v[f], cu.v[f], bits, p.itv)))
        .collect();
    // (curr.kswapd + curr.direct - prev.kswapd - prev.direct) を unsigned long で
    let scanned = sub(
        sub(
            add(cu.v[PGSCAN_KSWAPD], cu.v[PGSCAN_DIRECT], bits),
            pr.v[PGSCAN_KSWAPD],
            bits,
        ),
        pr.v[PGSCAN_DIRECT],
        bits,
    );
    cells.push(Cell::F92(if scanned != 0 {
        sp_value(pr.v[PGSTEAL], cu.v[PGSTEAL], bits, scanned)
    } else {
        0.0
    }));
    cells
}

// ---- A_MEMORY ----

/// `stub_print_memory_stats()`。
fn memory_rows(p: &Print<'_>, prev: &Buf, curr: &Buf, acc: &mut Accum) -> Vec<Row> {
    use field::mem::*;
    let (Some(smp), Some(smc)) = (prev.first(), curr.first()) else {
        return Vec::new();
    };
    let bits = p.dec.ulong_bits();
    let cells = match p.mem {
        MemOutput::Dia => {
            // KB_TO_PG() してから double にし、double のまま差を取る
            let pg = |kb: u64| (kb >> p.opts.kb_shift) as f64;
            [FRMKB, BUFKB, CAMKB]
                .iter()
                .map(|&f| Cell::F92((pg(smc.v[f]) - pg(smp.v[f])) / p.itv as f64 * HZ))
                .collect()
        }
        MemOutput::Amt => match p.avg {
            None => {
                let s = acc.sum(11);
                for f in [FRMKB, BUFKB, CAMKB, COMKB, ACTIVEKB, INACTKB, DIRTYKB] {
                    s[f] = s[f].wrapping_add(smc.v[f]);
                }
                let tl = smc.v[TLMKB];
                let tl_sw = add(tl, smc.v[TLSKB], bits);
                vec![
                    Cell::U9(smc.v[FRMKB]),
                    Cell::U9(sub(tl, smc.v[FRMKB], bits)),
                    Cell::P62(if tl != 0 {
                        sp_value(smc.v[FRMKB], tl, bits, tl)
                    } else {
                        0.0
                    }),
                    Cell::U9(smc.v[BUFKB]),
                    Cell::U9(smc.v[CAMKB]),
                    Cell::U9(smc.v[COMKB]),
                    Cell::P72(if tl_sw != 0 {
                        sp_value(0, smc.v[COMKB], bits, tl_sw)
                    } else {
                        0.0
                    }),
                    Cell::U9(smc.v[ACTIVEKB]),
                    Cell::U9(smc.v[INACTKB]),
                    Cell::U9(smc.v[DIRTYKB]),
                ]
            }
            Some(avg) => {
                let n = avg.count.max(1);
                let s = acc.sum(11).clone();
                let mean = |f: usize| s[f] as f64 / n as f64;
                // `(double) (avg_frmkb / avg_count)`: 整数で割ってから double
                let int_mean = |f: usize| (s[f] / n) as f64;
                let tl = smc.v[TLMKB];
                let tl_sw = add(tl, smc.v[TLSKB], bits);
                vec![
                    Cell::F90(mean(FRMKB)),
                    // 最後のサンプルの tlmkb から平均 free を引く
                    Cell::F90(tl as f64 - mean(FRMKB)),
                    Cell::P62(if tl != 0 {
                        (tl as f64 - int_mean(FRMKB)) / tl as f64 * 100.0
                    } else {
                        0.0
                    }),
                    Cell::F90(mean(BUFKB)),
                    Cell::F90(mean(CAMKB)),
                    Cell::F90(mean(COMKB)),
                    Cell::P72(if tl_sw != 0 {
                        (int_mean(COMKB) - 0.0) / tl_sw as f64 * 100.0
                    } else {
                        0.0
                    }),
                    Cell::F90(mean(ACTIVEKB)),
                    Cell::F90(mean(INACTKB)),
                    Cell::F90(mean(DIRTYKB)),
                ]
            }
        },
        MemOutput::Swap => match p.avg {
            None => {
                let s = acc.sum(11);
                for f in [FRSKB, TLSKB, CASKB] {
                    s[f] = s[f].wrapping_add(smc.v[f]);
                }
                let (fr, tl, ca) = (smc.v[FRSKB], smc.v[TLSKB], smc.v[CASKB]);
                let used = sub(tl, fr, bits);
                vec![
                    Cell::U9(fr),
                    Cell::U9(used),
                    Cell::P62(if tl != 0 {
                        sp_value(fr, tl, bits, tl)
                    } else {
                        0.0
                    }),
                    Cell::U9(ca),
                    Cell::P62(if used != 0 {
                        sp_value(0, ca, bits, used)
                    } else {
                        0.0
                    }),
                ]
            }
            Some(avg) => {
                let n = avg.count.max(1);
                let s = acc.sum(11).clone();
                let mean = |f: usize| s[f] as f64 / n as f64;
                let int_mean = |f: usize| (s[f] / n) as f64;
                let used = mean(TLSKB) - mean(FRSKB);
                vec![
                    Cell::F90(mean(FRSKB)),
                    Cell::F90(used),
                    Cell::P62(if int_mean(TLSKB) != 0.0 {
                        (int_mean(TLSKB) - int_mean(FRSKB)) / int_mean(TLSKB) * 100.0
                    } else {
                        0.0
                    }),
                    // `(double) (avg_caskb / avg_count)`: 実データの 9015 はこれ
                    Cell::F90(int_mean(CASKB)),
                    Cell::P62(if used != 0.0 {
                        (int_mean(CASKB) - 0.0) / used * 100.0
                    } else {
                        0.0
                    }),
                ]
            }
        },
    };
    vec![Row::plain(cells)]
}

// ---- 瞬時値の整数列 (A_KTABLES / A_NET_SOCK / A_NET_SOCK6) ----

/// `%9u` の瞬時値と、`(double) 合計 / avg_count` の平均。
fn gauge_rows(p: &Print<'_>, curr: &Buf, acc: &mut Accum, order: &[usize]) -> Vec<Row> {
    let Some(c) = curr.first() else {
        return Vec::new();
    };
    let s = acc.sum(p.dec.spec().fields.len());
    let cells = match p.avg {
        None => order
            .iter()
            .map(|&f| {
                s[f] = s[f].wrapping_add(c.v[f]);
                Cell::U9(c.v[f])
            })
            .collect(),
        Some(avg) => order
            .iter()
            .map(|&f| Cell::F90(s[f] as f64 / avg.count.max(1) as f64))
            .collect(),
    };
    vec![Row::plain(cells)]
}

// ---- A_QUEUE ----

/// `stub_print_queue_stats()`。
fn queue_rows(p: &Print<'_>, curr: &Buf, acc: &mut Accum) -> Vec<Row> {
    use field::queue::*;
    let Some(c) = curr.first() else {
        return Vec::new();
    };
    let s = acc.sum(6);
    let cells = match p.avg {
        None => {
            for f in [
                NR_RUNNING,
                NR_THREADS,
                LOAD_AVG_1,
                LOAD_AVG_5,
                LOAD_AVG_15,
                PROCS_BLOCKED,
            ] {
                s[f] = s[f].wrapping_add(c.v[f]);
            }
            vec![
                Cell::U9(c.v[NR_RUNNING]),
                Cell::U9(c.v[NR_THREADS]),
                Cell::F92(c.v[LOAD_AVG_1] as f64 / 100.0),
                Cell::F92(c.v[LOAD_AVG_5] as f64 / 100.0),
                Cell::F92(c.v[LOAD_AVG_15] as f64 / 100.0),
                Cell::U9(c.v[PROCS_BLOCKED]),
            ]
        }
        Some(avg) => {
            let n = avg.count.max(1);
            // `(double) avg_load_avg_1 / (avg_count * 100)` (整数の積を double へ)
            let load = |f: usize| s[f] as f64 / n.wrapping_mul(100) as f64;
            vec![
                Cell::F90(s[NR_RUNNING] as f64 / n as f64),
                Cell::F90(s[NR_THREADS] as f64 / n as f64),
                Cell::F92(load(LOAD_AVG_1)),
                Cell::F92(load(LOAD_AVG_5)),
                Cell::F92(load(LOAD_AVG_15)),
                Cell::F90(s[PROCS_BLOCKED] as f64 / n as f64),
            ]
        }
    };
    vec![Row::plain(cells)]
}

// ---- A_SERIAL ----

/// `print_serial_stats()`。回線番号が前サンプルと食い違えば `N/A`。
fn serial_rows(p: &Print<'_>, prev: &Buf, curr: &Buf) -> Vec<Row> {
    use field::serial::LINE;
    let mut rows = Vec::new();
    for (i, ssc) in curr.iter().enumerate() {
        if ssc.v[LINE] == 0 {
            continue;
        }
        // 本家は unsigned int の `line - 1` を `%3d` に渡すので、2^31 以上は負の数に見える
        let label = Label::Num3Wide(i64::from((ssc.v[LINE] as u32).wrapping_sub(1) as i32));
        let cells = match prev.get(i) {
            Some(ssp) if ssp.v[LINE] == ssc.v[LINE] => (0..6)
                .map(|f| Cell::F92(s_value(ssp.v[f], ssc.v[f], 32, p.itv)))
                .collect(),
            _ => vec![Cell::Na; 6],
        };
        rows.push(Row {
            label,
            cells,
            tail: Tail::None,
        });
    }
    rows
}

// ---- A_DISK ----

/// `print_disk_stats()`。
fn disk_rows(p: &Print<'_>, prev: &mut Buf, curr: &Buf) -> Vec<Row> {
    use field::disk::*;
    let spec = p.dec.spec();
    let bits = p.dec.ulong_bits();
    let mut rows = Vec::new();
    for (i, sdc) in curr.iter().enumerate() {
        if sdc.v[MAJOR].wrapping_add(sdc.v[MINOR]) & 0xffff_ffff == 0 {
            continue;
        }
        let j = check_disk_reg(spec, prev, sdc, i);
        let sdp = &prev[j];
        let ext = ExtDisk::compute(sdc, sdp, bits, p.itv);
        rows.push(Row {
            label: Label::Name(format!("dev{}-{}", sdc.v[MAJOR], sdc.v[MINOR])),
            cells: vec![
                Cell::F92(s_value(sdp.v[NR_IOS], sdc.v[NR_IOS], 64, p.itv)),
                Cell::F92(ll_s_value(sdp.v[RD_SECT], sdc.v[RD_SECT], p.itv)),
                Cell::F92(ll_s_value(sdp.v[WR_SECT], sdc.v[WR_SECT], p.itv)),
                Cell::F92(ext.arqsz),
                Cell::F92(s_value(sdp.v[RQ_TICKS], sdc.v[RQ_TICKS], 32, p.itv) / 1000.0),
                Cell::F92(ext.await_ms),
                Cell::F92(ext.svctm),
                Cell::F92(ext.util / 10.0),
            ],
            tail: Tail::None,
        });
    }
    rows
}

/// `compute_ext_disk_stats()` の結果。
struct ExtDisk {
    util: f64,
    svctm: f64,
    await_ms: f64,
    arqsz: f64,
}

impl ExtDisk {
    fn compute(sdc: &Item, sdp: &Item, ulong_bits: u32, itv: u64) -> Self {
        use field::disk::*;
        let ios = sdc.v[NR_IOS].wrapping_sub(sdp.v[NR_IOS]);
        // `((double) (sdc->nr_ios - sdp->nr_ios)) * HZ / itv`
        let tput = ios as f64 * HZ / itv as f64;
        let util = s_value(sdp.v[TOT_TICKS], sdc.v[TOT_TICKS], 32, itv);
        let svctm = if tput != 0.0 { util / tput } else { 0.0 };
        let await_ms = if ios != 0 {
            // tick の差は unsigned int 同士の加算
            add(
                sub(sdc.v[RD_TICKS], sdp.v[RD_TICKS], 32),
                sub(sdc.v[WR_TICKS], sdp.v[WR_TICKS], 32),
                32,
            ) as f64
                / ios as f64
        } else {
            0.0
        };
        let arqsz = if ios != 0 {
            add(
                sub(sdc.v[RD_SECT], sdp.v[RD_SECT], ulong_bits),
                sub(sdc.v[WR_SECT], sdp.v[WR_SECT], ulong_bits),
                ulong_bits,
            ) as f64
                / ios as f64
        } else {
            0.0
        };
        Self {
            util,
            svctm,
            await_ms,
            arqsz,
        }
    }
}

/// `check_disk_reg()`。基準バッファ `ref_buf` を書き換えることがある。
///
/// 同じ major/minor の枠があればそれを返す。そのとき 3 つのカウンタが揃って
/// 減っていれば「抜かれて別のディスクが挿さった」とみなして枠を 0 に戻す。
/// 見つからなければ空き枠 (major + minor が 0) か同じ位置の枠を 0 に戻して使う。
fn check_disk_reg(spec: &Spec, ref_buf: &mut Buf, sdc: &Item, pos: usize) -> usize {
    use field::disk::*;
    let reset = |slot: &mut Item| {
        *slot = Item::zeroed(spec);
        slot.v[MAJOR] = sdc.v[MAJOR];
        slot.v[MINOR] = sdc.v[MINOR];
    };
    let nr = ref_buf.len();
    if let Some(index) = ref_buf
        .iter()
        .position(|sdp| sdc.v[MAJOR] == sdp.v[MAJOR] && sdc.v[MINOR] == sdp.v[MINOR])
    {
        let sdp = &ref_buf[index];
        if sdc.v[NR_IOS] < sdp.v[NR_IOS]
            && sdc.v[RD_SECT] < sdp.v[RD_SECT]
            && sdc.v[WR_SECT] < sdp.v[WR_SECT]
        {
            reset(&mut ref_buf[index]);
        }
        return index;
    }
    let mut index = ref_buf
        .iter()
        .position(|s| s.v[MAJOR].wrapping_add(s.v[MINOR]) & 0xffff_ffff == 0)
        .unwrap_or(nr);
    if index >= nr {
        index = pos;
    }
    if let Some(slot) = ref_buf.get_mut(index) {
        reset(slot);
    }
    index.min(nr.saturating_sub(1))
}

// ---- A_NET_DEV / A_NET_EDEV ----

/// `print_net_dev_stats()`。
fn net_dev_rows(p: &Print<'_>, prev: &mut Buf, curr: &Buf) -> Vec<Row> {
    use field::net_dev::*;
    let spec = p.dec.spec();
    let mut rows = Vec::new();
    for (i, sndc) in curr.iter().enumerate() {
        if sndc.text(0).is_empty() {
            continue;
        }
        let j = check_net_dev_reg(spec, prev, sndc, i, p.dec.ulong_bits());
        let sndp = &prev[j];
        let s = |f: usize| s_value(sndp.v[f], sndc.v[f], 64, p.itv);
        rows.push(Row {
            label: Label::Name(sndc.text(0).to_string()),
            cells: vec![
                Cell::F92(s(RX_PACKETS)),
                Cell::F92(s(TX_PACKETS)),
                Cell::F92(s(RX_BYTES) / 1024.0),
                Cell::F92(s(TX_BYTES) / 1024.0),
                Cell::F92(s(RX_COMPRESSED)),
                Cell::F92(s(TX_COMPRESSED)),
                Cell::F92(s(MULTICAST)),
            ],
            tail: Tail::None,
        });
    }
    rows
}

/// 本家の `strncpy(..., MAX_IFACE_LEN - 1)` (`IFNAMSIZ` = 16)。
fn iface_name(name: &str) -> String {
    let bytes = name.as_bytes();
    let n = bytes.len().min(15);
    String::from_utf8_lossy(&bytes[..n]).into_owned()
}

/// `check_net_dev_reg()`。基準バッファ `ref_buf` を書き換えることがある。
fn check_net_dev_reg(
    spec: &Spec,
    ref_buf: &mut Buf,
    sndc: &Item,
    pos: usize,
    ulong_bits: u32,
) -> usize {
    use field::net_dev::*;
    let half = if ulong_bits >= 64 {
        u64::MAX >> 1
    } else {
        (u64::MAX >> (64 - ulong_bits)) >> 1
    };
    if let Some(index) = ref_buf.iter().position(|p| sndc.text(0) == p.text(0)) {
        let sndp = &ref_buf[index];
        let decreased = [
            RX_PACKETS,
            TX_PACKETS,
            RX_BYTES,
            TX_BYTES,
            RX_COMPRESSED,
            TX_COMPRESSED,
            MULTICAST,
        ]
        .iter()
        .any(|&f| sndc.v[f] < sndp.v[f]);
        if decreased {
            // バイト数 (パケット数) だけが減り、相手が増えていて、前の値が
            // ULONG_MAX/2 を超えていれば「桁あふれ」とみなし、再登録にはしない。
            let (c, pr) = (&sndc.v, &sndp.v);
            let ovfw = (c[RX_BYTES] < pr[RX_BYTES]
                && c[RX_PACKETS] > pr[RX_PACKETS]
                && pr[RX_BYTES] > half)
                || (c[TX_BYTES] < pr[TX_BYTES]
                    && c[TX_PACKETS] > pr[TX_PACKETS]
                    && pr[TX_BYTES] > half)
                || (c[RX_PACKETS] < pr[RX_PACKETS]
                    && c[RX_BYTES] > pr[RX_BYTES]
                    && pr[RX_PACKETS] > half)
                || (c[TX_PACKETS] < pr[TX_PACKETS]
                    && c[TX_BYTES] > pr[TX_BYTES]
                    && pr[TX_PACKETS] > half);
            if !ovfw {
                let mut fresh = Item::zeroed(spec);
                fresh.t[0] = iface_name(sndc.text(0));
                ref_buf[index] = fresh;
            }
        }
        return index;
    }
    reuse_iface_slot(spec, ref_buf, sndc, pos)
}

/// `check_net_edev_reg()`。基準バッファ `ref_buf` を書き換えることがある。
fn check_net_edev_reg(spec: &Spec, ref_buf: &mut Buf, snedc: &Item, pos: usize) -> usize {
    use field::net_edev::*;
    if let Some(index) = ref_buf.iter().position(|p| snedc.text(0) == p.text(0)) {
        let snedp = &ref_buf[index];
        // rx_errors は判定に入っていない (本家のまま)
        let decreased = [
            TX_ERRORS,
            COLLISIONS,
            RX_DROPPED,
            TX_DROPPED,
            TX_CARRIER_ERRORS,
            RX_FRAME_ERRORS,
            RX_FIFO_ERRORS,
            TX_FIFO_ERRORS,
        ]
        .iter()
        .any(|&f| snedc.v[f] < snedp.v[f]);
        if decreased {
            let mut fresh = Item::zeroed(spec);
            fresh.t[0] = iface_name(snedc.text(0));
            ref_buf[index] = fresh;
        }
        return index;
    }
    reuse_iface_slot(spec, ref_buf, snedc, pos)
}

/// インターフェースが基準バッファに無いときの枠の選び方 (両関数で共通)。
///
/// 名前が `"?"` の枠があればそこ、無ければ同じ位置の枠を 0 に戻して使う。
fn reuse_iface_slot(spec: &Spec, ref_buf: &mut Buf, curr: &Item, pos: usize) -> usize {
    let nr = ref_buf.len();
    let mut index = (0..nr).find(|&k| ref_buf[k].text(0) == "?").unwrap_or(nr);
    if index >= nr {
        index = pos;
    }
    if let Some(slot) = ref_buf.get_mut(index) {
        let mut fresh = Item::zeroed(spec);
        fresh.t[0] = iface_name(curr.text(0));
        *slot = fresh;
    }
    index.min(nr.saturating_sub(1))
}

/// `print_net_edev_stats()`。
fn net_edev_rows(p: &Print<'_>, prev: &mut Buf, curr: &Buf) -> Vec<Row> {
    use field::net_edev::*;
    let spec = p.dec.spec();
    let mut rows = Vec::new();
    for (i, snedc) in curr.iter().enumerate() {
        if snedc.text(0).is_empty() {
            continue;
        }
        let j = check_net_edev_reg(spec, prev, snedc, i);
        let snedp = &prev[j];
        let s = |f: usize| Cell::F92(s_value(snedp.v[f], snedc.v[f], 64, p.itv));
        rows.push(Row {
            label: Label::Name(snedc.text(0).to_string()),
            cells: vec![
                s(RX_ERRORS),
                s(TX_ERRORS),
                s(COLLISIONS),
                s(RX_DROPPED),
                s(TX_DROPPED),
                s(TX_CARRIER_ERRORS),
                s(RX_FRAME_ERRORS),
                s(RX_FIFO_ERRORS),
                s(TX_FIFO_ERRORS),
            ],
            tail: Tail::None,
        });
    }
    rows
}

// ---- A_PWR_CPUFREQ ----

/// `stub_print_pwr_cpufreq_stats()`。
fn cpufreq_rows(p: &Print<'_>, curr: &Buf, acc: &mut Accum) -> Vec<Row> {
    let bitmap = &p.opts.cpu_bitmap;
    let n = curr.len().min(bitmap.b_size() + 1);
    if acc.per_item.len() < curr.len() {
        acc.per_item.resize(curr.len(), 0);
    }
    let mut rows = Vec::new();
    for (i, spc) in curr.iter().enumerate().take(n) {
        if !bitmap.is_set(i) {
            continue;
        }
        let label = if i == 0 {
            Label::All
        } else {
            Label::Num3(i as i64 - 1)
        };
        let value = match p.avg {
            None => {
                let f = spc.v[0];
                acc.per_item[i] = acc.per_item[i].wrapping_add(f);
                f as f64 / 100.0
            }
            // `(double) avg_cpufreq[i] / (100 * avg_count)`
            Some(avg) => acc.per_item[i] as f64 / avg.count.max(1).wrapping_mul(100) as f64,
        };
        rows.push(Row {
            label,
            cells: vec![Cell::F92(value)],
            tail: Tail::None,
        });
    }
    rows
}

// ---- A_PWR_FAN / A_PWR_TEMP / A_PWR_IN ----

/// `stub_print_pwr_{fan,temp,in}_stats()`。
fn sensor_rows(p: &Print<'_>, curr: &Buf, acc: &mut Accum) -> Vec<Row> {
    use field::sensor::*;
    let id = p.dec.spec().id;
    let n = curr.len();
    for v in [&mut acc.sensor, &mut acc.sensor_min, &mut acc.sensor_max] {
        if v.len() < n {
            v.resize(n, 0.0);
        }
    }
    let mut rows = Vec::new();
    for (i, spc) in curr.iter().enumerate() {
        // 番号の起点が activity で違う (FAN / TEMP は 1、IN は 0)
        let label = Label::Num3(if id == ActivityId::PWR_IN {
            i as i64
        } else {
            i as i64 + 1
        });
        let cells = if id == ActivityId::PWR_FAN {
            match p.avg {
                None => {
                    acc.sensor[i] += spc.f(VALUE);
                    acc.sensor_min[i] += spc.f(MIN);
                    vec![
                        Cell::F92(spc.f(VALUE)),
                        Cell::F92(spc.f(VALUE) - spc.f(MIN)),
                    ]
                }
                Some(avg) => {
                    let n = avg.count.max(1) as f64;
                    vec![
                        Cell::F92(acc.sensor[i] / n),
                        Cell::F92((acc.sensor[i] - acc.sensor_min[i]) / n),
                    ]
                }
            }
        } else {
            match p.avg {
                None => {
                    let (v, lo, hi) = (spc.f(VALUE), spc.f(MIN), spc.f(MAX));
                    acc.sensor[i] += v;
                    // 最小・最大は変わらないものとして最後の値を控える (本家のまま)
                    acc.sensor_min[i] = lo;
                    acc.sensor_max[i] = hi;
                    vec![
                        Cell::F92(v),
                        Cell::F92(if hi - lo != 0.0 {
                            (v - lo) / (hi - lo) * 100.0
                        } else {
                            0.0
                        }),
                    ]
                }
                Some(avg) => {
                    let n = avg.count.max(1) as f64;
                    let (lo, hi) = (acc.sensor_min[i], acc.sensor_max[i]);
                    vec![
                        Cell::F92(acc.sensor[i] / n),
                        Cell::F92(if hi - lo != 0.0 {
                            (acc.sensor[i] / n - lo) / (hi - lo) * 100.0
                        } else {
                            0.0
                        }),
                    ]
                }
            }
        };
        rows.push(Row {
            label,
            cells,
            tail: Tail::Sensor(spc.text(0).to_string()),
        });
    }
    rows
}

// ---- A_HUGE ----

/// `stub_print_huge_stats()`。
fn huge_rows(p: &Print<'_>, curr: &Buf, acc: &mut Accum) -> Vec<Row> {
    use field::huge::*;
    let Some(smc) = curr.first() else {
        return Vec::new();
    };
    let bits = p.dec.ulong_bits();
    let s = acc.sum(2);
    let cells = match p.avg {
        None => {
            s[FRHKB] = s[FRHKB].wrapping_add(smc.v[FRHKB]);
            s[TLHKB] = s[TLHKB].wrapping_add(smc.v[TLHKB]);
            let (fr, tl) = (smc.v[FRHKB], smc.v[TLHKB]);
            vec![
                Cell::U9(fr),
                Cell::U9(sub(tl, fr, bits)),
                Cell::P62(if tl != 0 {
                    sp_value(fr, tl, bits, tl)
                } else {
                    0.0
                }),
            ]
        }
        Some(avg) => {
            let n = avg.count.max(1);
            let mean = |f: usize| s[f] as f64 / n as f64;
            let int_mean = |f: usize| (s[f] / n) as f64;
            vec![
                Cell::F90(mean(FRHKB)),
                Cell::F90(mean(TLHKB) - mean(FRHKB)),
                Cell::P62(if int_mean(TLHKB) != 0.0 {
                    (int_mean(TLHKB) - int_mean(FRHKB)) / int_mean(TLHKB) * 100.0
                } else {
                    0.0
                }),
            ]
        }
    };
    vec![Row::plain(cells)]
}

// ---- A_PWR_WGHFREQ ----

/// `print_pwr_wghfreq_stats()`。平均行も同じ関数 (`prev` = 区間の最初)。
fn wghfreq_rows(p: &Print<'_>, prev: &Buf, curr: &Buf) -> Vec<Row> {
    use field::wghfreq::*;
    let bitmap = &p.opts.cpu_bitmap;
    let bits = p.dec.ulong_bits();
    // 行列 (CPU × 周波数) を CPU ごとに区切る
    let nr2 = p.nr2;
    if nr2 == 0 {
        return Vec::new();
    }
    let nr = curr.len() / nr2;
    let n = nr.min(bitmap.b_size() + 1);
    let mut rows = Vec::new();
    for i in 0..n {
        if !bitmap.is_set(i) {
            continue;
        }
        let label = if i == 0 {
            Label::All
        } else {
            Label::Num3(i as i64 - 1)
        };
        let (mut tisfreq, mut tis) = (0u64, 0u64);
        for k in 0..nr2 {
            let c = &curr[i * nr2 + k];
            if c.v[FREQ] == 0 {
                break;
            }
            let Some(pp) = prev.get(i * nr2 + k) else {
                break;
            };
            let d = c.v[TIME_IN_STATE].wrapping_sub(pp.v[TIME_IN_STATE]);
            // (freq / 1000) は unsigned long、差は unsigned long long
            tisfreq = tisfreq.wrapping_add(mask(c.v[FREQ] / 1000, bits).wrapping_mul(d));
            tis = tis.wrapping_add(d);
        }
        rows.push(Row {
            label,
            cells: vec![Cell::F92(if tis != 0 {
                tisfreq as f64 / tis as f64
            } else {
                0.0
            })],
            tail: Tail::None,
        });
    }
    rows
}

// ---- A_PWR_USB ----

/// `stub_print_pwr_usb_stats()`。
///
/// 瞬時値の行を出すたびに、見た装置を `summary` (本家の `buf[2]`) の
/// 要約リストへ入れる。平均行 (`Summary`) はこのリスト (= `prev`) を出す。
fn usb_rows(p: &Print<'_>, prev: &Buf, curr: &Buf, summary: Option<&mut Buf>) -> Vec<Row> {
    use field::usb::*;
    let spec = p.dec.spec();
    let source: &Buf = if p.avg.is_some() { prev } else { curr };
    let mut rows = Vec::new();
    for suc in source.iter() {
        if suc.v[BUS_NR] & 0xffff_ffff == 0 {
            break;
        }
        rows.push(Row {
            // `%6d` で符号付きとして出る (要約リストのダミーは -1 になる)
            label: Label::Bus(suc.v[BUS_NR] as u32 as i32 as i64),
            cells: vec![
                Cell::X9(suc.v[VENDOR_ID] & 0xffff_ffff),
                Cell::X9(suc.v[PRODUCT_ID] & 0xffff_ffff),
                // bMaxPower は 2 mA 単位 (`<< 1` は unsigned int)
                Cell::U9((suc.v[BMAXPOWER] << 1) & 0xffff_ffff),
            ],
            tail: Tail::Usb(suc.text(0).to_string(), suc.text(1).to_string()),
        });
    }
    if let (None, Some(summary)) = (p.avg, summary) {
        for suc in curr.iter() {
            if suc.v[BUS_NR] & 0xffff_ffff == 0 {
                break;
            }
            let nr = summary.len();
            for (j, sum) in summary.iter_mut().enumerate() {
                if sum.v[BUS_NR] == suc.v[BUS_NR]
                    && sum.v[VENDOR_ID] == suc.v[VENDOR_ID]
                    && sum.v[PRODUCT_ID] == suc.v[PRODUCT_ID]
                {
                    break;
                }
                if sum.v[BUS_NR] & 0xffff_ffff == 0 {
                    *sum = suc.clone();
                    break;
                }
                // 最後の枠まで見つからなければ、そこを「その他」の行にする
                if j == nr - 1 {
                    let mut other = Item::zeroed(spec);
                    other.v[BUS_NR] = 0xffff_ffff;
                    other.t[1] = USB_OTHER_DEVICES.to_string();
                    *sum = other;
                }
            }
        }
    }
    rows
}

// ---- A_FILESYSTEM ----

/// `stub_print_filesystem_stats()`。要約リストの扱いは USB と同じ。
fn fs_rows(p: &Print<'_>, prev: &Buf, curr: &Buf, summary: Option<&mut Buf>) -> Vec<Row> {
    use field::fs::*;
    let source: &Buf = if p.avg.is_some() { prev } else { curr };
    let mut rows = Vec::new();
    for sfc in source.iter() {
        let blocks = sfc.v[F_BLOCKS];
        if blocks == 0 {
            break;
        }
        let (bfree, bavail, files, ffree) = (
            sfc.v[F_BFREE],
            sfc.v[F_BAVAIL],
            sfc.v[F_FILES],
            sfc.v[F_FFREE],
        );
        rows.push(Row {
            label: Label::None,
            cells: vec![
                Cell::F90(bfree as f64 / 1024.0 / 1024.0),
                Cell::F90(blocks.wrapping_sub(bfree) as f64 / 1024.0 / 1024.0),
                Cell::P62(sp_value(bfree, blocks, 64, blocks)),
                Cell::P62(sp_value(bavail, blocks, 64, blocks)),
                Cell::U9(ffree),
                Cell::U9(files.wrapping_sub(ffree)),
                Cell::P62(if files != 0 {
                    sp_value(ffree, files, 64, files)
                } else {
                    0.0
                }),
            ],
            tail: Tail::Name(if p.opts.fs_mount {
                sfc.text(1).to_string()
            } else {
                sfc.text(0).to_string()
            }),
        });
    }
    if let (None, Some(summary)) = (p.avg, summary) {
        for sfc in curr.iter() {
            if sfc.v[F_BLOCKS] == 0 {
                break;
            }
            if let Some(slot) = summary
                .iter_mut()
                .find(|m| m.text(0) == sfc.text(0) || m.v[F_BLOCKS] == 0)
            {
                *slot = sfc.clone();
            }
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::abi::{Endian, LayoutAbi, SourceEncoding};
    use crate::layout::plan::{DeclaredShape, select_revision};
    use crate::layout::registry;

    /// el7 の全構造体が、読み出し側のレイアウト表で名前どおりに引けること。
    ///
    /// ここが食い違うと、そのフィールドは 0 として読まれ、値が黙ってずれる。
    #[test]
    fn every_el7_field_resolves_against_the_layout_table() {
        let enc = SourceEncoding::new(Endian::Little, LayoutAbi::LP64);
        for s in SPECS {
            let def = registry::lookup(s.id).unwrap_or_else(|| panic!("{} が未登録", s.id));
            let shape = DeclaredShape {
                magic: Some(s.magic),
                size: s.size_lp64 as usize,
                types_nr: None,
            };
            let rev = select_revision(def, &shape)
                .unwrap_or_else(|e| panic!("{} magic {:#x}: {e}", s.id, s.magic));
            let plan = DecodePlan::build_for(def, rev, &shape, 1, 1, &enc).unwrap();
            let dec = Decoder::new(s, &plan, 64);
            assert!(
                dec.unresolved_fields().is_empty(),
                "{}: {:?}",
                s.id,
                dec.unresolved_fields()
            );
            for (name, _) in s.fields {
                let f = plan.fields.iter().find(|f| f.name == *name).unwrap();
                assert!(
                    f.is_available(),
                    "{} の {name} が申告サイズに収まらない",
                    s.id
                );
            }
            for t in s.texts {
                assert!(plan.text_index(t).is_some(), "{} の文字列 {t}", s.id);
            }
        }
    }

    #[test]
    fn bitmap_sizes_follow_bitmap_size_macro() {
        // BITMAP_SIZE(8192) = 1025 バイト → -P ALL / -A で 8200 ビット
        let mut cpu = Bitmap::cpu();
        cpu.set_all();
        assert_eq!(cpu.count_bits(), 8200);
        // BITMAP_SIZE(1024) = 129 バイト
        let mut irq = Bitmap::irq();
        irq.set_all();
        assert_eq!(irq.count_bits(), 1032);
        let mut b = Bitmap::cpu();
        b.set(0);
        b.set(3);
        assert!(b.is_set(0) && b.is_set(3) && !b.is_set(1));
        assert_eq!(b.count_bits(), 2);
    }

    #[test]
    fn c_subtraction_wraps_at_the_field_width() {
        assert_eq!(sub(1, 2, 32), 0xffff_ffff);
        assert_eq!(sub(1, 2, 64), u64::MAX);
        assert_eq!(sub(5, 3, 32), 2);
        // S_VALUE は巻き戻った差をそのまま割る (unsigned int の 1 周)
        assert_eq!(s_value(0xffff_fff0, 0x10, 32, 100), 32.0);
    }

    /// el7 の `dyn-tick` パッチ: 逆行は 32bit 補正ではなく 0。
    #[test]
    fn ll_values_clamp_backward_counters_to_zero() {
        assert_eq!(ll_s_value(10, 5, 100), 0.0);
        assert_eq!(ll_sp_value(10, 5, 100), 0.0);
        assert_eq!(ll_sp_value(5, 10, 100), 5.0);
        assert_eq!(get_interval(7, 7), 1);
    }

    // ---- activity ごとの計算 ----

    /// レイアウト表から el7 の構造体の読み方を組み立てる (LP64)。
    fn decoder(id: ActivityId) -> Decoder {
        let enc = SourceEncoding::new(Endian::Little, LayoutAbi::LP64);
        let s = spec(id).unwrap();
        let def = registry::lookup(id).unwrap();
        let shape = DeclaredShape {
            magic: Some(s.magic),
            size: s.size_lp64 as usize,
            types_nr: None,
        };
        let rev = select_revision(def, &shape).unwrap();
        let plan = DecodePlan::build_for(def, rev, &shape, 1, 1, &enc).unwrap();
        Decoder::new(s, &plan, 64)
    }

    fn print_opts() -> PrintOptions {
        let mut cpu_bitmap = Bitmap::cpu();
        cpu_bitmap.set_all();
        let mut irq_bitmap = Bitmap::irq();
        irq_bitmap.set_all();
        PrintOptions {
            cpu_all: false,
            cpu_bitmap,
            irq_bitmap,
            fs_mount: false,
            kb_shift: 2,
        }
    }

    /// 指定したフィールドだけ値を入れた item。
    fn item(id: ActivityId, values: &[(usize, u64)], texts: &[&str]) -> Item {
        let mut it = Item::zeroed(spec(id).unwrap());
        for (f, v) in values {
            it.v[*f] = *v;
        }
        for (i, t) in texts.iter().enumerate() {
            it.t[i] = (*t).to_string();
        }
        it
    }

    fn rows(
        id: ActivityId,
        itv: u64,
        avg: Option<u64>,
        prev: &mut Buf,
        curr: &mut Buf,
        summary: Option<&mut Buf>,
        nr2: usize,
    ) -> Vec<Row> {
        let dec = decoder(id);
        let opts = print_opts();
        let p = Print {
            dec: &dec,
            opts: &opts,
            itv,
            avg: avg.map(|count| AvgInfo { count }),
            mem: MemOutput::Amt,
            nr2,
        };
        print_rows(&p, prev, curr, summary, &mut Accum::default())
    }

    fn f92(cells: &[Cell]) -> Vec<String> {
        cells
            .iter()
            .map(|c| match c {
                Cell::F92(v) | Cell::P62(v) => format!("{v:.2}"),
                other => format!("{other:?}"),
            })
            .collect()
    }

    /// `user - guest` が前より減った分は CPU の区間に足し戻す (`ishift`)。
    #[test]
    fn per_cpu_interval_adds_back_the_guest_shift() {
        use field::cpu::*;
        let scp = item(
            ActivityId::CPU,
            &[(USER, 1000), (GUEST, 100), (IDLE, 9000)],
            &[],
        );
        // user は 50 増えたが guest が 100 増え、user - guest は 900 → 850 に減った
        let scc = item(
            ActivityId::CPU,
            &[(USER, 1050), (GUEST, 200), (IDLE, 9950)],
            &[],
        );
        // tick の差 1000 (50 + 950) に ishift 50 を足す
        assert_eq!(per_cpu_interval(&scc, &scp), 1050);
    }

    /// 3 つのカウンタが揃って減ったディスクは、基準の枠を 0 に戻してから差を取る。
    #[test]
    fn disk_reregistration_resets_the_reference_slot() {
        use field::disk::*;
        let mut prev = vec![item(
            ActivityId::DISK,
            &[(MAJOR, 8), (NR_IOS, 1000), (RD_SECT, 5000), (WR_SECT, 7000)],
            &[],
        )];
        let mut curr = vec![item(
            ActivityId::DISK,
            &[(MAJOR, 8), (NR_IOS, 10), (RD_SECT, 50), (WR_SECT, 70)],
            &[],
        )];
        let r = rows(ActivityId::DISK, 100, None, &mut prev, &mut curr, None, 1);
        assert_eq!(r[0].label, Label::Name("dev8-0".into()));
        // 0 からの差: tps 10、rd_sec/s 50、wr_sec/s 70、avgrq-sz (50+70)/10 = 12
        assert_eq!(&f92(&r[0].cells)[..4], ["10.00", "50.00", "70.00", "12.00"]);
        // 基準の枠そのものが 0 に戻っている (本家はバッファを書き換える)
        assert_eq!(prev[0].v[NR_IOS], 0);
        assert_eq!(prev[0].v[MAJOR], 8);
    }

    /// バイト数だけが減り、パケット数が増え、前の値が ULONG_MAX/2 を超えていれば
    /// 桁あふれとみなして枠を戻さない (差は巻き戻った値で取る)。
    #[test]
    fn net_dev_counter_overflow_is_not_a_reregistration() {
        use field::net_dev::*;
        let mut prev = vec![item(
            ActivityId::NET_DEV,
            &[(RX_BYTES, u64::MAX - 1023), (RX_PACKETS, 100)],
            &["eth0"],
        )];
        let mut curr = vec![item(
            ActivityId::NET_DEV,
            &[(RX_BYTES, 1024), (RX_PACKETS, 110)],
            &["eth0"],
        )];
        let r = rows(
            ActivityId::NET_DEV,
            100,
            None,
            &mut prev,
            &mut curr,
            None,
            1,
        );
        // rxpck/s 10、rxkB/s = 2048 B / 1 秒 / 1024 = 2.00
        assert_eq!(&f92(&r[0].cells)[..3], ["10.00", "0.00", "2.00"]);
        assert_eq!(prev[0].v[RX_BYTES], u64::MAX - 1023, "枠は書き換えない");
    }

    /// pgscan が増えていない区間の `%vmeff` は 0.00 (0 で割らない)。
    #[test]
    fn vmeff_is_zero_without_scans() {
        use field::page::*;
        let mut prev = vec![item(ActivityId::PAGE, &[(PGSTEAL, 10)], &[])];
        let mut curr = vec![item(ActivityId::PAGE, &[(PGSTEAL, 20)], &[])];
        let r = rows(ActivityId::PAGE, 100, None, &mut prev, &mut curr, None, 1);
        assert_eq!(f92(&r[0].cells)[8], "0.00");
        // 走査 40 ページのうち 30 ページ回収 → 75.00
        let mut prev = vec![item(ActivityId::PAGE, &[(PGSCAN_KSWAPD, 100)], &[])];
        let mut curr = vec![item(
            ActivityId::PAGE,
            &[(PGSCAN_KSWAPD, 120), (PGSCAN_DIRECT, 20), (PGSTEAL, 30)],
            &[],
        )];
        let r = rows(ActivityId::PAGE, 100, None, &mut prev, &mut curr, None, 1);
        assert_eq!(f92(&r[0].cells)[8], "75.00");
    }

    /// USB の要約リストが一杯になったら、最後の枠を「その他」の行にする (バス番号 -1)。
    #[test]
    fn usb_summary_marks_the_last_slot_as_other_devices() {
        use field::usb::*;
        let dev = |bus, vendor| {
            item(
                ActivityId::PWR_USB,
                &[(BUS_NR, bus), (VENDOR_ID, vendor), (PRODUCT_ID, 1)],
                &["maker", "product"],
            )
        };
        let mut summary = vec![dev(1, 0xa), dev(2, 0xb)];
        let mut curr = vec![dev(3, 0xc), dev(1, 0xa)];
        let mut prev = Vec::new();
        rows(
            ActivityId::PWR_USB,
            100,
            None,
            &mut prev,
            &mut curr,
            Some(&mut summary),
            1,
        );
        assert_eq!(summary[0].v[BUS_NR], 1);
        assert_eq!(summary[1].v[BUS_NR], 0xffff_ffff);
        assert_eq!(summary[1].text(1), USB_OTHER_DEVICES);
        // 平均行 (Summary) はこのリストを出し、ダミーのバス番号は -1 になる
        let mut last = curr.clone();
        let r = rows(
            ActivityId::PWR_USB,
            100,
            Some(1),
            &mut summary,
            &mut last,
            None,
            1,
        );
        assert_eq!(r[1].label, Label::Bus(-1));
    }

    /// FS の要約リストは、同じ名前の枠を最新の値で上書きし、無ければ空き枠に入れる。
    #[test]
    fn fs_summary_keeps_the_latest_values_per_filesystem() {
        use field::fs::*;
        let fs = |name: &str, blocks| item(ActivityId::FS, &[(F_BLOCKS, blocks)], &[name, "/"]);
        let mut summary = vec![
            fs("/dev/sda1", 100),
            Item::zeroed(spec(ActivityId::FS).unwrap()),
        ];
        let mut curr = vec![fs("/dev/sda1", 200), fs("/dev/sdb1", 300)];
        let mut prev = Vec::new();
        rows(
            ActivityId::FS,
            100,
            None,
            &mut prev,
            &mut curr,
            Some(&mut summary),
            1,
        );
        assert_eq!(summary[0].v[F_BLOCKS], 200);
        assert_eq!(summary[1].text(0), "/dev/sdb1");
    }

    /// 重み付き周波数は time_in_state の増分で重みを付けた MHz の平均。
    #[test]
    fn weighted_frequency_is_weighted_by_time_in_state() {
        use field::wghfreq::*;
        let slot = |freq, tis| {
            item(
                ActivityId::PWR_FREQ,
                &[(FREQ, freq), (TIME_IN_STATE, tis)],
                &[],
            )
        };
        // CPU all (i = 0) の 2 スロット: 2000 MHz で 100、1000 MHz で 300
        let mut prev = vec![slot(2_000_000, 0), slot(1_000_000, 0)];
        let mut curr = vec![slot(2_000_000, 100), slot(1_000_000, 300)];
        let r = rows(
            ActivityId::PWR_FREQ,
            100,
            None,
            &mut prev,
            &mut curr,
            None,
            2,
        );
        assert_eq!(r[0].label, Label::All);
        // (2000 × 100 + 1000 × 300) / 400 = 1250
        assert_eq!(f92(&r[0].cells), ["1250.00"]);
    }

    /// TTY 番号は unsigned int の `line - 1` を `%3d` で出したもの。
    /// 2^31 を超える回線番号 (細工したファイル) は本家どおり負の数になる。
    #[test]
    fn serial_line_numbers_are_printed_as_signed_ints() {
        use field::serial::LINE;
        let tty = |line: u64| {
            let mut prev = vec![item(ActivityId::SERIAL, &[(LINE, line)], &[])];
            let mut curr = prev.clone();
            rows(ActivityId::SERIAL, 100, None, &mut prev, &mut curr, None, 1)
                .remove(0)
                .label
        };
        assert_eq!(tty(1), Label::Num3Wide(0));
        assert_eq!(tty(0x8000_0000), Label::Num3Wide(2_147_483_647));
        assert_eq!(tty(0x8000_0001), Label::Num3Wide(-2_147_483_648));
        assert_eq!(tty(0xffff_ffff), Label::Num3Wide(-2));
    }

    /// センサ番号の起点: FAN と TEMP は 1、IN は 0 (本家のまま)。
    #[test]
    fn sensor_numbers_start_differently_per_activity() {
        let one = |id: ActivityId| {
            let mut prev = Vec::new();
            let mut curr = vec![item(id, &[], &["dev"])];
            rows(id, 100, None, &mut prev, &mut curr, None, 1)
                .remove(0)
                .label
        };
        assert_eq!(one(ActivityId::PWR_FAN), Label::Num3(1));
        assert_eq!(one(ActivityId::PWR_TEMP), Label::Num3(1));
        assert_eq!(one(ActivityId::PWR_IN), Label::Num3(0));
    }

    // ---- 平均行の累積とビットマップ ----
    //
    // 以下の値の組は `tests/sar_el7.rs` の同名の場面と同じで、そちらは本家 el7 の
    // `sar` と突き合わせてある。

    /// 本家の `printf` と同じ桁で文字列にする (`%9.0f` は偶数丸め)。
    fn shown(cells: &[Cell]) -> Vec<String> {
        cells
            .iter()
            .map(|c| match c {
                Cell::F92(v) | Cell::P62(v) | Cell::P72(v) => format!("{v:.2}"),
                Cell::F90(v) => format!("{v:.0}"),
                Cell::U9(v) => v.to_string(),
                Cell::X9(v) => format!("{v:x}"),
                Cell::Na => "N/A".into(),
            })
            .collect()
    }

    /// `rows` と同じだが、表示設定と平均の累積 (`print_*_stats()` の `static`) を
    /// 呼び出し側が持つ。`itv` は 100 jiffies (1 秒) なので `S_VALUE` は差そのもの。
    fn rows_with(
        id: ActivityId,
        opts: &PrintOptions,
        avg: Option<u64>,
        prev: &mut Buf,
        curr: &mut Buf,
        acc: &mut Accum,
    ) -> Vec<Row> {
        let dec = decoder(id);
        let p = Print {
            dec: &dec,
            opts,
            itv: 100,
            avg: avg.map(|count| AvgInfo { count }),
            mem: MemOutput::Amt,
            nr2: 1,
        };
        print_rows(&p, prev, curr, None, acc)
    }

    /// 瞬時値を順に出したあと平均行を出す。`samples[0]` は区間の最初 (本家の `buf[2]`) で、
    /// 表示しない。返すのは (瞬時値の行…, 平均行)。
    fn with_average(
        id: ActivityId,
        opts: &PrintOptions,
        samples: &[Buf],
    ) -> (Vec<Vec<Row>>, Vec<Row>) {
        let mut acc = Accum::default();
        let mut shown_rows = Vec::new();
        for w in samples.windows(2) {
            let (mut prev, mut curr) = (w[0].clone(), w[1].clone());
            shown_rows.push(rows_with(id, opts, None, &mut prev, &mut curr, &mut acc));
        }
        let mut first = samples[0].clone();
        let mut last = samples[samples.len() - 1].clone();
        let n = samples.len() as u64 - 1;
        let avg = rows_with(id, opts, Some(n), &mut first, &mut last, &mut acc);
        (shown_rows, avg)
    }

    /// `-q` は瞬時値を並べ替えて出し (nr_running, nr_threads, 負荷, procs_blocked)、
    /// 平均は整数の件数を `(double) Σ / avg_count`、負荷だけ `Σ / (avg_count * 100)`。
    #[test]
    fn queue_average_divides_the_load_by_count_times_100() {
        use field::queue::*;
        let q = |run, blocked, load: [u64; 3], threads| {
            vec![item(
                ActivityId::QUEUE,
                &[
                    (NR_RUNNING, run),
                    (PROCS_BLOCKED, blocked),
                    (LOAD_AVG_1, load[0]),
                    (LOAD_AVG_5, load[1]),
                    (LOAD_AVG_15, load[2]),
                    (NR_THREADS, threads),
                ],
                &[],
            )]
        };
        let (rows, avg) = with_average(
            ActivityId::QUEUE,
            &print_opts(),
            &[
                q(50, 50, [5000; 3], 5000),
                q(2, 1, [105, 210, 399], 301),
                q(3, 2, [106, 211, 400], 302),
            ],
        );
        assert_eq!(
            shown(&rows[0][0].cells),
            ["2", "301", "1.05", "2.10", "3.99", "1"]
        );
        // runq-sz 5 / 2 = 2.5 と plist-sz 603 / 2 = 301.5 は偶数丸めで 2 と 302。
        // ldavg-1 = 211 / 200 = 1.055 は double で 1.05499… なので 1.05
        assert_eq!(
            shown(&avg[0].cells),
            ["2", "302", "1.05", "2.10", "4.00", "2"]
        );
        // 区間の最初 (R0) の値は平均に入らない
        assert!(matches!(avg[0].cells[0], Cell::F90(v) if v == 2.5));
    }

    /// `-v` / `-n SOCK` の平均は `(double) Σ / avg_count` (整数除算ではない)。
    /// 1000.5 は切り捨てで 1000 になるのではなく、`%9.0f` の偶数丸めで 1000 になる
    /// (2001.5 は 2002)。列は dentunusd → file-nr、tcp-tw は末尾。
    #[test]
    fn gauge_averages_are_double_divisions_in_the_printed_order() {
        use field::ktables::*;
        let kt = |file, inode, dentry, pty| {
            vec![item(
                ActivityId::KTABLES,
                &[
                    (FILE_USED, file),
                    (INODE_USED, inode),
                    (DENTRY_STAT, dentry),
                    (PTY_NR, pty),
                ],
                &[],
            )]
        };
        let (rows, avg) = with_average(
            ActivityId::KTABLES,
            &print_opts(),
            &[
                kt(9, 9, 9, 9),
                kt(1000, 2000, 3000, 1),
                kt(1001, 2003, 3004, 4),
            ],
        );
        assert_eq!(shown(&rows[0][0].cells), ["3000", "1000", "2000", "1"]);
        assert_eq!(shown(&avg[0].cells), ["3002", "1000", "2002", "2"]);
        assert!(matches!(avg[0].cells[1], Cell::F90(v) if v == 1000.5));

        let sock = |v: [u64; 6]| {
            let fields: Vec<(usize, u64)> = v.iter().copied().enumerate().collect();
            vec![item(ActivityId::NET_SOCK, &fields, &[])]
        };
        let (rows, avg) = with_average(
            ActivityId::NET_SOCK,
            &print_opts(),
            &[
                sock([9; 6]),
                sock([100, 20, 5, 7, 1, 0]),
                sock([103, 21, 6, 8, 2, 1]),
            ],
        );
        assert_eq!(shown(&rows[1][0].cells), ["103", "21", "8", "2", "1", "6"]);
        assert_eq!(shown(&avg[0].cells), ["102", "20", "8", "2", "0", "6"]);
    }

    /// hugepages の平均は `%hugused` だけ、Σtlhkb / n と Σfrhkb / n を整数で割ってから
    /// 比を取る。tlhkb が 0 の瞬時値は 0.00。
    ///
    /// ファイル上の 1 item は `STATS_HUGE_SIZE` の不具合で `sizeof(struct stats_memory)`
    /// (88 バイト) だが、読むのは先頭 16 バイトだけ。
    #[test]
    fn huge_average_divides_integers_before_the_ratio() {
        use field::huge::*;
        assert_eq!(spec(ActivityId::HUGE).unwrap().size_lp64, 88);
        let h = |fr, tl| vec![item(ActivityId::HUGE, &[(FRHKB, fr), (TLHKB, tl)], &[])];
        let (rows, avg) = with_average(
            ActivityId::HUGE,
            &print_opts(),
            &[h(99, 99), h(3, 10), h(4, 11), h(0, 0)],
        );
        assert_eq!(shown(&rows[0][0].cells), ["3", "7", "70.00"]);
        assert_eq!(shown(&rows[1][0].cells), ["4", "7", "63.64"]);
        assert_eq!(shown(&rows[2][0].cells), ["0", "0", "0.00"]);
        // kbhugfree 7 / 3 = 2.33、kbhugused 21 / 3 - 7 / 3 = 4.67、
        // %hugused = (7 - 2) / 7 (浮動小数なら 4.67 / 7 = 66.67)
        assert_eq!(shown(&avg[0].cells), ["2", "5", "71.43"]);
    }

    /// `-m CPU` は CPU のビットマップで行を選ぶ (0 番のビットが "all")。周波数 0 の
    /// CPU (オフライン) も 0.00 の行を出し、平均の分母には数える。平均は
    /// `(double) Σ / (100 * avg_count)`。
    #[test]
    fn cpu_frequency_follows_the_bitmap_and_counts_offline_cpus() {
        let mut opts = print_opts();
        opts.cpu_bitmap = Bitmap::cpu();
        opts.cpu_bitmap.set(0);
        opts.cpu_bitmap.set(2);
        let f = |v: [u64; 3]| {
            v.iter()
                .map(|&x| item(ActivityId::PWR_CPU, &[(0, x)], &[]))
                .collect::<Buf>()
        };
        let (rows, avg) = with_average(
            ActivityId::PWR_CPU,
            &opts,
            &[
                f([999_999; 3]),
                f([250_000, 300_000, 0]),
                f([150_050, 100_004, 200_000]),
            ],
        );
        let labels: Vec<&Label> = rows[0].iter().map(|r| &r.label).collect();
        assert_eq!(labels, [&Label::All, &Label::Num3(1)]);
        assert_eq!(shown(&rows[0][1].cells), ["0.00"]);
        assert_eq!(shown(&avg[0].cells), ["2000.25"]);
        assert_eq!(shown(&avg[1].cells), ["1000.00"]);
    }

    /// `-I` のビットマップ: 0 番のビットが合計 (`sum`)、i 番が割り込み i - 1。
    /// 値は `ll_s_value()` なので、逆行した割り込みは 0.00。
    #[test]
    fn irq_rows_follow_the_bitmap_and_clamp_backward_counters() {
        let mut opts = print_opts();
        opts.irq_bitmap = Bitmap::irq();
        opts.irq_bitmap.set(0);
        opts.irq_bitmap.set(3);
        let b = |v: [u64; 5]| {
            v.iter()
                .map(|&n| item(ActivityId::IRQ, &[(0, n)], &[]))
                .collect::<Buf>()
        };
        let mut prev = b([1000, 100, 200, 300, 400]);
        let mut curr = b([7000, 700, 1400, 200, 3400]);
        let r = rows_with(
            ActivityId::IRQ,
            &opts,
            None,
            &mut prev,
            &mut curr,
            &mut Accum::default(),
        );
        let got: Vec<(Label, Vec<String>)> = r
            .iter()
            .map(|row| (row.label.clone(), shown(&row.cells)))
            .collect();
        assert_eq!(
            got,
            [
                (Label::Sum, vec!["6000.00".to_string()]),
                (Label::Num3Wide(2), vec!["0.00".to_string()]),
            ]
        );
    }

    /// `check_net_edev_reg()` は rx_errors の減少を再登録とみなさない。
    /// 枠は戻らず、`unsigned long long` の差が巻き戻ったまま `S_VALUE` に入る。
    /// 他のカウンタ (ここでは tx_carrier_errors) が減れば枠を 0 に戻す。
    #[test]
    fn net_edev_reregistration_ignores_rx_errors() {
        use field::net_edev::*;
        let e = |v: [u64; 9]| {
            let fields: Vec<(usize, u64)> = v.iter().copied().enumerate().collect();
            vec![item(ActivityId::NET_EDEV, &fields, &["eth1"])]
        };
        let mut prev = e([262_144; 9]);
        let mut only_rx_errors = [262_144 + 16_384; 9];
        only_rx_errors[RX_ERRORS] = 262_144 - 65_536;
        let r = rows(
            ActivityId::NET_EDEV,
            65_536,
            None,
            &mut prev,
            &mut e(only_rx_errors),
            None,
            1,
        );
        // (2^64 - 65536) / 65536 × 100。他の列は 16384 / 65536 × 100 = 25.00
        assert_eq!(f92(&r[0].cells)[0], "28147497671065500.00");
        assert_eq!(f92(&r[0].cells)[1], "25.00");
        assert_eq!(prev[0].v[RX_ERRORS], 262_144, "枠は戻さない");

        let mut carrier = [262_144 + 16_384; 9];
        carrier[TX_CARRIER_ERRORS] = 1;
        let r = rows(
            ActivityId::NET_EDEV,
            65_536,
            None,
            &mut prev,
            &mut e(carrier),
            None,
            1,
        );
        // 0 からの差: 278528 / 65536 × 100 = 425.00
        assert_eq!(f92(&r[0].cells)[0], "425.00");
        assert_eq!(prev[0].v, vec![0; 9]);
        assert_eq!(prev[0].text(0), "eth1");
    }

    /// 前サンプルに無い NIC は、名前が `?` の枠があればそこを、無ければ同じ位置の枠を
    /// 0 に戻して使う (`check_net_dev_reg()` の後半)。同じ位置の枠に別の NIC が
    /// いても上書きする。
    #[test]
    fn new_interfaces_take_the_question_mark_slot_or_the_same_rank() {
        use field::net_dev::*;
        let d = |name: &str, rx| item(ActivityId::NET_DEV, &[(RX_PACKETS, rx)], &[name]);
        let mut prev = vec![d("eth0", 1000), d("?", 5000), d("", 0)];
        let mut curr = vec![d("eth1", 6000), d("eth0", 1600), d("eth2", 600)];
        let r = rows(
            ActivityId::NET_DEV,
            100,
            None,
            &mut prev,
            &mut curr,
            None,
            1,
        );
        let rx: Vec<String> = r.iter().map(|row| f92(&row.cells)[0].clone()).collect();
        assert_eq!(rx, ["6000.00", "600.00", "600.00"]);
        assert_eq!(prev[0], d("eth0", 1000));
        assert_eq!(prev[1], d("eth1", 0));
        assert_eq!(prev[2], d("eth2", 0));

        // `?` が無ければ、同じ位置にいる eth0 の枠でも 0 に戻して使う
        let mut prev = vec![d("eth0", 1000), d("eth1", 2000)];
        let mut curr = vec![d("eth9", 300), d("eth1", 2600)];
        let r = rows(
            ActivityId::NET_DEV,
            100,
            None,
            &mut prev,
            &mut curr,
            None,
            1,
        );
        let rx: Vec<String> = r.iter().map(|row| f92(&row.cells)[0].clone()).collect();
        assert_eq!(rx, ["300.00", "600.00"]);
        assert_eq!(prev[0], d("eth9", 0));
    }

    /// 再登録で枠を戻すときの名前は `strncpy(..., MAX_IFACE_LEN - 1)` で 15 バイトまで。
    #[test]
    fn interface_names_are_copied_up_to_15_bytes() {
        assert_eq!(iface_name("eth0"), "eth0");
        assert_eq!(iface_name("abcdefghijklmnop"), "abcdefghijklmno");
    }

    /// 前サンプルに無いディスクは、major + minor が 0 の空き枠が無ければ同じ位置の枠を
    /// 0 に戻して使う (そこにいた別のディスクの枠でも上書きする)。
    #[test]
    fn disk_without_a_free_slot_takes_the_same_rank() {
        use field::disk::*;
        let d = |minor, ios| {
            item(
                ActivityId::DISK,
                &[(MAJOR, 8), (MINOR, minor), (NR_IOS, ios)],
                &[],
            )
        };
        let mut prev = vec![d(0, 100), d(16, 200)];
        let mut curr = vec![d(0, 150), d(32, 50)];
        let r = rows(ActivityId::DISK, 100, None, &mut prev, &mut curr, None, 1);
        assert_eq!(r[1].label, Label::Name("dev8-32".into()));
        assert_eq!(f92(&r[1].cells)[0], "50.00");
        assert_eq!(prev[1], d(32, 0));
    }

    /// センサの平均: FAN の drpm は (Σrpm - Σrpm_min) / n。TEMP / IN の % は
    /// **最後のサンプルの**最小・最大で割る (本家は「変わらない」と仮定している)。
    #[test]
    fn sensor_averages_use_the_last_min_and_max() {
        use field::sensor::*;
        // FAN は値と最小の 2 つ、TEMP / IN は値・最小・最大の 3 つ
        let s = |id, v: &[f64]| {
            let fields: Vec<(usize, u64)> = [VALUE, MIN, MAX]
                .iter()
                .zip(v)
                .map(|(&f, x)| (f, x.to_bits()))
                .collect();
            vec![item(id, &fields, &["dev"])]
        };
        let (rows, avg) = with_average(
            ActivityId::PWR_TEMP,
            &print_opts(),
            &[
                s(ActivityId::PWR_TEMP, &[1.0, 0.0, 2.0]),
                s(ActivityId::PWR_TEMP, &[45.0, 20.0, 70.0]),
                s(ActivityId::PWR_TEMP, &[57.0, 20.0, 100.0]),
            ],
        );
        assert_eq!(shown(&rows[0][0].cells), ["45.00", "50.00"]);
        assert_eq!(shown(&rows[1][0].cells), ["57.00", "46.25"]);
        // (51 - 20) / (100 - 20)。最初のサンプルの最大 70 を使えば 62.00 になる
        assert_eq!(shown(&avg[0].cells), ["51.00", "38.75"]);
        assert_eq!(avg[0].tail, Tail::Sensor("dev".into()));

        let (_, avg) = with_average(
            ActivityId::PWR_FAN,
            &print_opts(),
            &[
                s(ActivityId::PWR_FAN, &[1.0, 1.0]),
                s(ActivityId::PWR_FAN, &[1200.0, 1000.0]),
                s(ActivityId::PWR_FAN, &[1300.0, 1000.0]),
            ],
        );
        assert_eq!(shown(&avg[0].cells), ["1250.00", "250.00"]);
    }

    /// `-u ALL` の `%usr` / `%nice` は `user - guest` / `nice - guest_nice` の差で、
    /// 減っていれば 0.00。個別 CPU の区間はその減少分だけ広がる。
    #[test]
    fn cpu_all_clamps_user_and_nice_below_their_guest_time() {
        use field::cpu::*;
        let c = |v: [u64; 10]| {
            let fields: Vec<(usize, u64)> = v.iter().copied().enumerate().collect();
            item(ActivityId::CPU, &fields, &[])
        };
        // CPU 1: nice - guest_nice が 50 → 0 に減る (tick の差 60000 + 50)
        let p = c([300, 100, 100, 50_000, 0, 0, 0, 0, 0, 50]);
        let n = c([300, 150, 100, 109_950, 0, 0, 0, 0, 0, 150]);
        let itv = per_cpu_interval(&n, &p);
        assert_eq!(itv, 60_050);
        let cells = cpu_values(true, &p, &n, itv);
        assert_eq!(
            shown(&cells),
            [
                "0.00", "0.00", "0.00", "0.00", "0.00", "0.00", "0.00", "0.00", "0.17", "99.83"
            ]
        );
        // -u (既定) の %nice はゲスト時間を引かない: 50 / 60050
        assert_eq!(shown(&cpu_values(false, &p, &n, itv))[1], "0.08");
        // idle が減れば %idle (-u ALL の末尾の列) は 0.00
        let back = c([31_300, 150, 30_050, 109_000, 0, 0, 0, 0, 0, 150]);
        let itv = per_cpu_interval(&back, &n);
        assert_eq!(itv, 60_000);
        assert_eq!(
            shown(&cpu_values(true, &n, &back, itv)),
            [
                "51.67", "0.00", "49.92", "0.00", "0.00", "0.00", "0.00", "0.00", "0.00", "0.00"
            ]
        );
        assert!(back.v[IDLE] < n.v[IDLE]);
    }
}

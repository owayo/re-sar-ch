//! `sar` 互換オプションの手書きパーサ。
//!
//! 典拠は [`docs/format/03-output-format.md`] 第 VI 部 (sysstat 12.8.0 の
//! `sar.c` / `sa_common.c` / `common.c`)。
//!
//! `clap` の derive では `sar` の文法を表現できない。理由は次のとおり。
//!
//! - `-u ALL` / `-r ALL` / `-F MOUNT` / `-I SUM` は「オプションがトークン末尾にあり、
//!   かつ次引数が特定のキーワードに完全一致するときだけ」引数を消費する。
//!   条件を満たさない引数は positional として再解析される。
//! - `-n DEV,EDEV` のようなカンマ区切りキーワード、`-P 0,2,4-7,12-` のような範囲構文を持つ。
//! - `-I` は数値を取らない (数値は positional interval として吸われる)。
//! - `-P ALL` と `-P all` は別物 (前者は全 CPU、後者は集約行のみ)。
//! - `-h` は help ではなく `--pretty --human` 相当、`-H` は hugepages。
//!
//! 解析は本家と同じ 2 段構造にしている。
//!
//! 1. [`parse_sar_args`] の while ループ — 完全一致 / 前置一致で判定する単独オプション群。
//!    ここに該当するものは他の 1 文字オプションと束ねられない (`-uP 0` は不正)。
//! 2. 上記に該当しない `-` 始まりの引数は [`parse_sar_opt`] に渡され、1 文字ずつ
//!    処理される (= `-bBruW` のように束ねられる)。
//!
//! [`docs/format/03-output-format.md`]: ../../../docs/format/03-output-format.md

use std::collections::BTreeMap;
use std::path::PathBuf;

// ============================================================================
// 定数 (sysstat の `sa.h` / `common.h` 由来)
// ============================================================================

/// `-P` が受け付ける CPU 番号の上限 (`NR_CPUS`)。
///
/// C 側は `__CPU_SETSIZE` (glibc なら 1024) が定義されていればそれ、無ければ 8192 を使う。
/// 仕様書 §9「要検証項目 1」に対応する値で、`-P 3-` の展開幅に直結する。
pub const NR_CPUS: usize = 1024;

/// `--int=` が受け付ける割り込み番号の上限 (`NR_IRQS`)。
pub const NR_IRQS: usize = 4096;

/// `--dev=` の 1 項目最大長 (`MAX_DEV_LEN`)。
pub const MAX_DEV_LEN: usize = 128;
/// `--fs=` の 1 項目最大長 (`MAX_FS_LEN`)。
pub const MAX_FS_LEN: usize = 128;
/// `--iface=` の 1 項目最大長 (`MAX_IFACE_LEN`)。
pub const MAX_IFACE_LEN: usize = 16;
/// `--int=` の 1 項目最大長 (`MAX_SA_IRQ_LEN`)。
pub const MAX_SA_IRQ_LEN: usize = 8;
/// `-j <type>` の最大長 (`MAX_FILE_LEN`)。511 バイト以上は usage。
pub const MAX_FILE_LEN: usize = 512;

/// `parse_range_values()` が使う `char range[16]` の長さ。
/// 16 バイトを超えるトークンはここで切り詰められてから解析される。
const MAX_RANGE_LEN: usize = 16;

/// 範囲指定を許さないことを表す `max_val` (`NO_RANGE`)。
pub(crate) const NO_RANGE: usize = 0;

/// `-s` の既定値 (`DEF_TMSTART`)。
pub const DEF_TMSTART: (u8, u8, u8) = (8, 0, 0);
/// `-e` の既定値 (`DEF_TMEND`)。
pub const DEF_TMEND: (u8, u8, u8) = (18, 0, 0);

// ============================================================================
// activity
// ============================================================================

/// sysstat の activity ID (`A_*`、`sa.h` の enum 1..=43)。
///
/// 宣言順 = ID 昇順であり、`Ord` もそれに従う。本家の出力順は `act[]` 配列順
/// (= ID 昇順に近い並び) で決まるため、集合を [`std::collections::BTreeSet`] や
/// [`BTreeMap`] で保持すればキーワードの指定順に依存しない決定的な順序が得られる。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Activity {
    /// 1: CPU 利用統計 (`-u` / `-u ALL`)
    Cpu,
    /// 2: タスク生成・コンテキストスイッチ (`-w`)
    Pcsw,
    /// 3: 割り込み (`-I` / `--int=`)
    Irq,
    /// 4: スワッピング (`-W`)
    Swap,
    /// 5: ページング (`-B`)
    Page,
    /// 6: I/O と転送レート (`-b`)
    Io,
    /// 7: メモリ / スワップ領域 (`-r` / `-r ALL` / `-S`)
    Memory,
    /// 8: カーネルテーブル (`-v`)
    Ktables,
    /// 9: 負荷とキュー長 (`-q` / `-q LOAD`)
    Queue,
    /// 10: TTY デバイス (`-y`)
    Serial,
    /// 11: ブロックデバイス (`-d` / `--dev=`)
    Disk,
    /// 12: ネットワークインタフェース (`-n DEV` / `--iface=`)
    NetDev,
    /// 13: ネットワークインタフェース (エラー) (`-n EDEV` / `--iface=`)
    NetEdev,
    /// 14: NFS クライアント (`-n NFS`)
    NetNfs,
    /// 15: NFS サーバ (`-n NFSD`)
    NetNfsd,
    /// 16: ソケット v4 (`-n SOCK`)
    NetSock,
    /// 17: IP v4 (`-n IP`)
    NetIp,
    /// 18: IP v4 エラー (`-n EIP`)
    NetEip,
    /// 19: ICMP v4 (`-n ICMP`)
    NetIcmp,
    /// 20: ICMP v4 エラー (`-n EICMP`)
    NetEicmp,
    /// 21: TCP v4 (`-n TCP`)
    NetTcp,
    /// 22: TCP v4 エラー (`-n ETCP`)
    NetEtcp,
    /// 23: UDP v4 (`-n UDP`)
    NetUdp,
    /// 24: ソケット v6 (`-n SOCK6`)
    NetSock6,
    /// 25: IP v6 (`-n IP6`)
    NetIp6,
    /// 26: IP v6 エラー (`-n EIP6`)
    NetEip6,
    /// 27: ICMP v6 (`-n ICMP6`)
    NetIcmp6,
    /// 28: ICMP v6 エラー (`-n EICMP6`)
    NetEicmp6,
    /// 29: UDP v6 (`-n UDP6`)
    NetUdp6,
    /// 30: CPU 瞬時クロック周波数 (`-m CPU`)
    PwrCpu,
    /// 31: ファン回転数 (`-m FAN`)
    PwrFan,
    /// 32: デバイス温度 (`-m TEMP`)
    PwrTemp,
    /// 33: 入力電圧 (`-m IN`)
    PwrIn,
    /// 34: hugepages (`-H`)
    Huge,
    /// 35: CPU 平均クロック周波数 (`-m FREQ`)
    PwrFreq,
    /// 36: USB デバイス (`-m USB`)
    PwrUsb,
    /// 37: ファイルシステム (`-F` / `-F MOUNT` / `--fs=`)
    Fs,
    /// 38: ファイバチャネル HBA (`-n FC`)
    NetFc,
    /// 39: ソフトウェア割り込み処理 (`-n SOFT`)
    NetSoft,
    /// 40: pressure-stall CPU (`-q CPU`)
    PsiCpu,
    /// 41: pressure-stall I/O (`-q IO`)
    PsiIo,
    /// 42: pressure-stall メモリ (`-q MEM`)
    PsiMem,
    /// 43: バッテリ容量 (`-m BAT`)
    PwrBat,
}

impl Activity {
    /// 全 43 activity を ID 昇順で並べた配列 (`NR_ACT = 43`)。
    pub const ALL: [Activity; 43] = [
        Activity::Cpu,
        Activity::Pcsw,
        Activity::Irq,
        Activity::Swap,
        Activity::Page,
        Activity::Io,
        Activity::Memory,
        Activity::Ktables,
        Activity::Queue,
        Activity::Serial,
        Activity::Disk,
        Activity::NetDev,
        Activity::NetEdev,
        Activity::NetNfs,
        Activity::NetNfsd,
        Activity::NetSock,
        Activity::NetIp,
        Activity::NetEip,
        Activity::NetIcmp,
        Activity::NetEicmp,
        Activity::NetTcp,
        Activity::NetEtcp,
        Activity::NetUdp,
        Activity::NetSock6,
        Activity::NetIp6,
        Activity::NetEip6,
        Activity::NetIcmp6,
        Activity::NetEicmp6,
        Activity::NetUdp6,
        Activity::PwrCpu,
        Activity::PwrFan,
        Activity::PwrTemp,
        Activity::PwrIn,
        Activity::Huge,
        Activity::PwrFreq,
        Activity::PwrUsb,
        Activity::Fs,
        Activity::NetFc,
        Activity::NetSoft,
        Activity::PsiCpu,
        Activity::PsiIo,
        Activity::PsiMem,
        Activity::PwrBat,
    ];

    /// `-n ALL` が選択する 20 種 (`parse_sar_n_opt()` の `K_ALL` 分岐と同じ集合)。
    pub const NET_ALL: [Activity; 20] = [
        Activity::NetDev,
        Activity::NetEdev,
        Activity::NetSock,
        Activity::NetNfs,
        Activity::NetNfsd,
        Activity::NetIp,
        Activity::NetEip,
        Activity::NetIcmp,
        Activity::NetEicmp,
        Activity::NetTcp,
        Activity::NetEtcp,
        Activity::NetUdp,
        Activity::NetSock6,
        Activity::NetIp6,
        Activity::NetEip6,
        Activity::NetIcmp6,
        Activity::NetEicmp6,
        Activity::NetUdp6,
        Activity::NetFc,
        Activity::NetSoft,
    ];

    /// `-m ALL` が選択する 7 種。除外キーワードは存在しない。
    pub const PWR_ALL: [Activity; 7] = [
        Activity::PwrCpu,
        Activity::PwrFan,
        Activity::PwrIn,
        Activity::PwrTemp,
        Activity::PwrFreq,
        Activity::PwrUsb,
        Activity::PwrBat,
    ];

    /// `-q PSI` が選択する 3 種 (`LOAD` = [`Activity::Queue`] は含まない)。
    pub const PSI_ALL: [Activity; 3] = [Activity::PsiCpu, Activity::PsiIo, Activity::PsiMem];

    /// `sa.h` の enum 値 (1..=43)。
    pub fn id(self) -> u8 {
        Activity::ALL
            .iter()
            .position(|a| *a == self)
            .expect("Activity::ALL に全 variant が含まれている") as u8
            + 1
    }

    /// `A_CPU` のような C 側の名前。`sadc -S A_<name>` と同じ綴り。
    pub fn name(self) -> &'static str {
        match self {
            Activity::Cpu => "A_CPU",
            Activity::Pcsw => "A_PCSW",
            Activity::Irq => "A_IRQ",
            Activity::Swap => "A_SWAP",
            Activity::Page => "A_PAGE",
            Activity::Io => "A_IO",
            Activity::Memory => "A_MEMORY",
            Activity::Ktables => "A_KTABLES",
            Activity::Queue => "A_QUEUE",
            Activity::Serial => "A_SERIAL",
            Activity::Disk => "A_DISK",
            Activity::NetDev => "A_NET_DEV",
            Activity::NetEdev => "A_NET_EDEV",
            Activity::NetNfs => "A_NET_NFS",
            Activity::NetNfsd => "A_NET_NFSD",
            Activity::NetSock => "A_NET_SOCK",
            Activity::NetIp => "A_NET_IP",
            Activity::NetEip => "A_NET_EIP",
            Activity::NetIcmp => "A_NET_ICMP",
            Activity::NetEicmp => "A_NET_EICMP",
            Activity::NetTcp => "A_NET_TCP",
            Activity::NetEtcp => "A_NET_ETCP",
            Activity::NetUdp => "A_NET_UDP",
            Activity::NetSock6 => "A_NET_SOCK6",
            Activity::NetIp6 => "A_NET_IP6",
            Activity::NetEip6 => "A_NET_EIP6",
            Activity::NetIcmp6 => "A_NET_ICMP6",
            Activity::NetEicmp6 => "A_NET_EICMP6",
            Activity::NetUdp6 => "A_NET_UDP6",
            Activity::PwrCpu => "A_PWR_CPU",
            Activity::PwrFan => "A_PWR_FAN",
            Activity::PwrTemp => "A_PWR_TEMP",
            Activity::PwrIn => "A_PWR_IN",
            Activity::Huge => "A_HUGE",
            Activity::PwrFreq => "A_PWR_FREQ",
            Activity::PwrUsb => "A_PWR_USB",
            Activity::Fs => "A_FS",
            Activity::NetFc => "A_NET_FC",
            Activity::NetSoft => "A_NET_SOFT",
            Activity::PsiCpu => "A_PSI_CPU",
            Activity::PsiIo => "A_PSI_IO",
            Activity::PsiMem => "A_PSI_MEM",
            Activity::PwrBat => "A_PWR_BAT",
        }
    }
}

// ============================================================================
// opt_flags (AO_F_*)
// ============================================================================

/// activity ごとのサブレポート選択 (`opt_flags` / `AO_F_*`)。
///
/// 値は `sa.h` の `AO_F_*` と同一にしてある。**同じビットが activity ごとに
/// 別の意味を持つ** 点も本家どおりで、`AO_F_MEMORY` / `AO_F_CPU_DEF` /
/// `AO_F_FILESYSTEM` はいずれも `0x0001` である。したがって判定は必ず
/// 「どの activity の `opt_flags` か」と対応付けて行うこと。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Hash)]
pub struct OptFlags(u32);

impl OptFlags {
    /// `AO_F_NULL`
    pub const NONE: OptFlags = OptFlags(0x0000);
    /// `AO_F_MEMORY` — `-r` (A_MEMORY)
    pub const MEMORY: OptFlags = OptFlags(0x0001);
    /// `AO_F_SWAP` — `-S` (A_MEMORY)
    pub const SWAP: OptFlags = OptFlags(0x0002);
    /// `AO_F_MEM_ALL` — `-r ALL` (A_MEMORY)
    pub const MEM_ALL: OptFlags = OptFlags(0x0100);
    /// `AO_F_CPU_DEF` — `-u` (A_CPU)
    pub const CPU_DEF: OptFlags = OptFlags(0x0001);
    /// `AO_F_CPU_ALL` — `-u ALL` (A_CPU)
    pub const CPU_ALL: OptFlags = OptFlags(0x0002);
    /// `AO_F_FILESYSTEM` — `-F` (A_FS)
    pub const FILESYSTEM: OptFlags = OptFlags(0x0001);
    /// `AO_F_MOUNT` — `-F MOUNT` (A_FS)
    pub const MOUNT: OptFlags = OptFlags(0x0002);

    /// 生のビット列。
    pub fn bits(self) -> u32 {
        self.0
    }

    /// 1 ビットも立っていないか。
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// `other` のビットがすべて立っているか。`other` が空なら常に `false`。
    pub fn contains(self, other: OptFlags) -> bool {
        !other.is_empty() && (self.0 & other.0) == other.0
    }

    /// 論理和で追加する (`opt_flags |= ...`)。`-r` / `-S` / `-F` の意味論。
    pub fn insert(&mut self, other: OptFlags) {
        self.0 |= other.0;
    }
}

// ============================================================================
// CPU ビットマップ (-P)
// ============================================================================

/// `-P` が設定する CPU 選択ビットマップ (`act[A_CPU]->bitmap`)。
///
/// **bit 0 は「集約行 (`all`)」** を表し、CPU 番号 `n` は bit `n + 1` に載る。
/// `A_CPU` / `A_IRQ` / `A_PWR_CPU` / `A_PWR_FREQ` / `A_NET_SOFT` がこの
/// ビットマップを共有する。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpuBitmap {
    /// `BITMAP_SIZE(NR_CPUS) = ((NR_CPUS + 1) >> 3) + 1` バイト。
    bytes: Vec<u8>,
}

impl Default for CpuBitmap {
    fn default() -> Self {
        CpuBitmap::new(NR_CPUS)
    }
}

impl CpuBitmap {
    /// `b_size = nr_cpus` のビットマップを作る。確保されるのは `nr_cpus + 1` ビット分。
    pub fn new(nr_cpus: usize) -> Self {
        CpuBitmap {
            bytes: vec![0u8; ((nr_cpus + 1) >> 3) + 1],
        }
    }

    /// 立てられるビットの総数 (= `NR_CPUS + 1` を含むバイト境界まで)。
    pub fn capacity_bits(&self) -> usize {
        self.bytes.len() * 8
    }

    /// `-P ALL` 相当。`memset(bitmap, ~0, BITMAP_SIZE(NR_CPUS))` と同じで全ビットを立てる。
    pub fn set_all(&mut self) {
        self.bytes.fill(0xff);
    }

    /// ビット `i` を立てる。範囲外は無視する (C 側も `b_size` で弾かれている)。
    pub fn set(&mut self, i: usize) {
        if let Some(byte) = self.bytes.get_mut(i >> 3) {
            *byte |= 1 << (i & 0x07);
        }
    }

    /// ビット `i` が立っているか。
    pub fn is_set(&self, i: usize) -> bool {
        self.bytes
            .get(i >> 3)
            .is_some_and(|byte| byte & (1 << (i & 0x07)) != 0)
    }

    /// 立っているビット数 (`count_bits()`)。
    pub fn count_bits(&self) -> usize {
        self.bytes.iter().map(|b| b.count_ones() as usize).sum()
    }

    /// 1 ビットも立っていないか。
    pub fn is_empty(&self) -> bool {
        self.bytes.iter().all(|b| *b == 0)
    }

    /// 集約行 (`all`) が選ばれているか (bit 0)。
    pub fn aggregate_selected(&self) -> bool {
        self.is_set(0)
    }

    /// 選択されている CPU 番号 (bit 1 以降) を昇順で返す。
    pub fn selected_cpus(&self) -> impl Iterator<Item = usize> + '_ {
        (0..self.capacity_bits().saturating_sub(1)).filter(move |n| self.is_set(n + 1))
    }
}

// ============================================================================
// 時刻指定 (-s / -e)
// ============================================================================

/// `-s` / `-e` が受け付ける時刻の表現 (`struct tstamp_ext` の `use` に対応)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TimeSpec {
    /// 未指定 (`NO_TIME`)。
    #[default]
    None,
    /// `hh:mm[:ss]` 指定 (`USE_HHMMSS_T`)。
    ///
    /// `check_time_limits()` の日跨ぎ処理により、`-e` 側の `hour` は
    /// **24..=47 になり得る** (`-s 18:00:00 -e 13:30:00` → `hour = 37`)。
    HhMmSs { hour: u8, min: u8, sec: u8 },
    /// ちょうど 10 桁の epoch 秒 (`USE_EPOCH_T`)。値 0 は指定できない。
    Epoch(u64),
}

impl TimeSpec {
    /// 未指定か。
    pub fn is_none(self) -> bool {
        matches!(self, TimeSpec::None)
    }

    /// 日跨ぎ補正 (`hour >= 24`) を受けているか。
    pub fn wraps_to_next_day(self) -> bool {
        matches!(self, TimeSpec::HhMmSs { hour, .. } if hour >= 24)
    }
}

// ============================================================================
// グローバルフラグ (S_F_*)
// ============================================================================

/// CLI から設定されるグローバルフラグ (`S_F_*` のうち引数解析で決まるもの)。
///
/// 本家は `uint64_t flags` の 1 語だが、ビット値の一部しか公開仕様に現れていないため
/// ここでは bool の集合として持つ。`sadf` も同じ語を共有するので、
/// `sadf` 固有のビット (`sec_epoch` / `horizontally` / `hdr_only`) のうち
/// 時刻基準に関わる [`SarFlags::sec_epoch`] だけはここに置いている。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SarFlags {
    /// `S_F_LOCAL_TIME` — 実行環境のローカル時刻で表示する。
    /// **`sar` は既定で真、`sadf` は既定で偽 (= UTC)**。`sadf -T` で真になる。
    pub local_time: bool,
    /// `S_F_TRUE_TIME` — `-t`。記録時のローカル時刻で表示する。
    pub true_time: bool,
    /// `S_F_SEC_EPOCH` — `sadf -U`。epoch 秒で表示する。`sar` には存在しない。
    pub sec_epoch: bool,
    /// `S_F_PRETTY` — `-p` / `--pretty` / `-h` / `-j`。
    pub pretty: bool,
    /// `S_F_UNIT` — `--human` / `-h`。
    pub human: bool,
    /// `S_F_COMMENT` — `-C`。
    pub comment: bool,
    /// `S_F_ZERO_OMIT` — `-z`。
    pub zero_omit: bool,
    /// `S_F_MINMAX` — `-x` (`sar` のみ。`sadf` では無言で無視される)。
    pub minmax: bool,
    /// `S_F_PERSIST_NAME` — `-j <type>`。
    pub persist_name: bool,
    /// `S_F_DEV_SID` — `-j SID`。
    pub dev_sid: bool,
    /// `S_F_OPTION_A` — `-A`。
    pub option_a: bool,
    /// `S_F_OPTION_P` — `-P`。
    pub option_p: bool,
    /// `S_F_INTERVAL_SET` — `-i`。
    pub interval_set: bool,
    /// `S_F_SA_YYYYMMDD` — `-D` (`-o` での書き出し名にのみ影響する)。
    pub sa_yyyymmdd: bool,
}

impl SarFlags {
    /// `sar` の初期値 (`flags = S_F_LOCAL_TIME`)。
    pub fn for_sar() -> Self {
        SarFlags {
            local_time: true,
            ..SarFlags::empty()
        }
    }

    /// `sadf` の初期値 (`flags = 0` = UTC 表示)。
    pub fn for_sadf() -> Self {
        SarFlags::empty()
    }

    fn empty() -> Self {
        SarFlags {
            local_time: false,
            true_time: false,
            sec_epoch: false,
            pretty: false,
            human: false,
            comment: false,
            zero_omit: false,
            minmax: false,
            persist_name: false,
            dev_sid: false,
            option_a: false,
            option_p: false,
            interval_set: false,
            sa_yyyymmdd: false,
        }
    }
}

impl Default for SarFlags {
    fn default() -> Self {
        SarFlags::for_sar()
    }
}

// ============================================================================
// 入出力先 / 即時アクション
// ============================================================================

/// 読み出し元 (`from_file`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SarInput {
    /// 既定の日次データファイル (`SA_DIR/saDD` または `saYYYYMMDD`)。
    ///
    /// 実際のパス解決は「`day_offset` 日前の日付」と「`saDD` / `saYYYYMMDD` の
    /// mtime 比較」で決まる実行時処理であり、パーサはここまでしか決めない。
    DefaultDaily,
    /// `-f <filename>`。ディレクトリを指す場合は日次ファイル名を付加する
    /// (`check_alt_sa_dir()` 相当) 必要があり、それは呼び出し側の責務。
    File(PathBuf),
}

/// 書き出し先 (`to_file`)。`resarch` は採取を行わないため、受け付けるだけで
/// 実行層が「非対応」として拒否する。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SarOutput {
    /// `-o` 単独 (`to_file = "-"` = 標準の日次データファイル)。
    DefaultDaily,
    /// `-o <filename>`。
    File(PathBuf),
}

/// `-j <type>` で指定された永続デバイス名の型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersistentName {
    /// `SID` (大文字完全一致)。WWN ベースの安定 ID。
    Sid,
    /// `/dev/disk/by-<type>` の `<type>`。**小文字化済み** (`-j UUID` == `-j uuid`)。
    ///
    /// ディレクトリが実在するかの確認 (`access(R_OK)`) は環境依存なのでここでは行わない。
    /// 呼び出し側が確認し、失敗時は `Invalid type of persistent device name` で
    /// exit 1 すること。
    ByType(String),
}

/// 引数解析の途中で即座に表示して終了する系のオプション。
///
/// 本家は解析ループ内で `exit()` するため、**これが立った時点で以降の引数は
/// 解析されない**。この実装も同じで、検出したらその場で `Ok` を返す。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SarImmediate {
    /// `--help` — `display_help()` を stdout に出して exit 0。
    Help,
    /// `-V` — 環境変数とバージョンを stdout に出して exit 0。
    Version,
    /// `--sadc` — データコレクタの所在を stdout に出して exit 0。
    Sadc,
}

/// [`parse_sar_opt`] の呼び出し元 (`C_SAR` / `C_SADF`)。
///
/// `-t` は `sar` だけが受け付け、`-x` は `sar` だけがフラグを立てる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Caller {
    /// `sar` 本体。
    Sar,
    /// `sadf ... -- <sar_options>`。
    Sadf,
}

// ============================================================================
// SarOptions
// ============================================================================

/// `sar` 互換オプションの解析結果。
///
/// 「どの activity を選んだか」は [`SarOptions::activities`] の**キー集合**
/// (`AO_SELECTED` 相当) で表し、「どのキーワードを選んだか」は同じマップの
/// **値** ([`OptFlags`] = `opt_flags`) で activity ごとに保持する。
/// `-n DEV` のようなキーワードは対応する activity の選択そのものになるため、
/// キーワードを別に覚える必要はない。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SarOptions {
    /// 選択された activity → その activity のサブレポート選択 (`opt_flags`)。
    ///
    /// キーが存在すること = `AO_SELECTED`。ID 昇順で走査できる。
    pub activities: BTreeMap<Activity, OptFlags>,

    /// `-P` の CPU 選択ビットマップ。bit 0 = 集約行 (`all`)、CPU `n` = bit `n + 1`。
    pub cpu_bitmap: CpuBitmap,

    /// `--dev=` / `--fs=` / `--iface=` / `--int=` / `-I SUM` で与えられた item リスト。
    ///
    /// エントリが存在すること = `AO_LIST_ON_CMDLINE` ([`SarOptions::list_on_cmdline`])。
    /// 空リストは登録されない (`--fs=` だけ書いた場合はフィルタ無し = 全 FS 表示)。
    pub item_lists: BTreeMap<Activity, Vec<String>>,

    /// `-s` (開始時刻)。
    ///
    /// **重要な意味論**: `-s` の範囲に最初に合致した R_STATS レコードは
    /// 「前サンプル」(差分計算の始点) として消費されるだけで、**統計行としては
    /// 表示されない**。`sar -s 13:20:20` でデータが 13:20:09/19/29/39/49 のとき、
    /// 最初に出る統計行は `13:20:29 → 13:20:39` の差分になり、ヘッダ行の
    /// タイムスタンプは 13:20:29 になる。
    /// また `-s` の判定は開始レコードを探す外側ループで 1 回だけ行われ、
    /// 内側ループでは `-e` しか見ない (開始点が決まったら `-e` 超過・EOF・
    /// R_RESTART まで連続して出力する)。
    /// 実際のフィルタ処理は series / output 層の責務で、ここでは値だけを運ぶ。
    pub tm_start: TimeSpec,

    /// `-e` (終了時刻)。`-e` を超えたレコードは表示されずそこで打ち切られる。
    /// `hh:mm:ss` 形式で `-e` < `-s` の場合は日跨ぎとして `hour += 24` される。
    pub tm_end: TimeSpec,

    /// 読み出し元。`finalize` 後は原則 `Some` (`-o` 指定時のみ `None`)。
    pub input: Option<SarInput>,

    /// `-o` の書き出し先。
    pub output: Option<SarOutput>,

    /// 既定の日次ファイルへフォールバックしたか (`default_file_used`)。
    /// 真のとき、ファイルが開けなければ
    /// `Please check if data collecting is enabled` の追加ヒントを出す。
    pub default_file_used: bool,

    /// `-[0-9]+` の日オフセット (何日前の日次ファイルか)。
    pub day_offset: u32,

    /// interval。`-i <interval>` と positional の第 1 引数が**同じ変数を共有する**。
    ///
    /// - `None` = 未指定 (C の `interval < 0`)。ファイル読み出しでは 1 として扱う。
    /// - `Some(0)` = 起動以降の平均 (ライブ採取時のみ。`-f` / `-o` 併用は usage)。
    /// - `-i` 由来かどうかは [`SarFlags::interval_set`] で区別する。
    pub interval: Option<u64>,

    /// positional の第 2 引数 (表示レコード数)。`None` = 無制限 (`count = -1`)。
    pub count: Option<u64>,

    /// `--dec={0|1|2}`。`None` = 既定 (2 桁相当、C の `dplaces_nr = -1`)。
    pub dec_places: Option<u8>,

    /// `-j <type>`。
    pub persistent_name: Option<PersistentName>,

    /// グローバルフラグ。
    pub flags: SarFlags,

    /// `--help` / `-V` / `--sadc`。`Some` のとき他のフィールドは未完成
    /// (解析を打ち切っており `finalize` も走っていない)。
    pub immediate: Option<SarImmediate>,
}

impl Default for SarOptions {
    fn default() -> Self {
        SarOptions {
            activities: BTreeMap::new(),
            cpu_bitmap: CpuBitmap::default(),
            item_lists: BTreeMap::new(),
            tm_start: TimeSpec::None,
            tm_end: TimeSpec::None,
            input: None,
            output: None,
            default_file_used: false,
            day_offset: 0,
            interval: None,
            count: None,
            dec_places: None,
            persistent_name: None,
            flags: SarFlags::for_sar(),
            immediate: None,
        }
    }
}

impl SarOptions {
    /// `sadf` の `--` 以降を解析するための初期状態 (`flags = 0` = UTC 表示)。
    pub fn for_sadf() -> Self {
        SarOptions {
            flags: SarFlags::for_sadf(),
            ..SarOptions::default()
        }
    }

    /// activity が選択されているか (`IS_SELECTED`)。
    pub fn is_selected(&self, act: Activity) -> bool {
        self.activities.contains_key(&act)
    }

    /// 選択された activity を ID 昇順で返す。
    pub fn selected_activities(&self) -> impl Iterator<Item = Activity> + '_ {
        self.activities.keys().copied()
    }

    /// activity の `opt_flags`。未選択なら [`OptFlags::NONE`]。
    pub fn opt_flags(&self, act: Activity) -> OptFlags {
        self.activities.get(&act).copied().unwrap_or(OptFlags::NONE)
    }

    /// コマンドラインで item リストが与えられたか (`AO_LIST_ON_CMDLINE`)。
    pub fn list_on_cmdline(&self, act: Activity) -> bool {
        self.item_lists.get(&act).is_some_and(|l| !l.is_empty())
    }

    /// item リスト。与えられていなければ空スライス。
    pub fn item_list(&self, act: Activity) -> &[String] {
        self.item_lists.get(&act).map_or(&[], |l| l.as_slice())
    }

    /// `SELECT_ACTIVITY()` 相当。既存の `opt_flags` は保つ。
    pub(crate) fn select(&mut self, act: Activity) {
        self.activities.entry(act).or_insert(OptFlags::NONE);
    }

    /// activity を選択し `opt_flags |= flags` する (`-r` / `-S` / `-F` の意味論)。
    fn select_with(&mut self, act: Activity, flags: OptFlags) {
        self.activities
            .entry(act)
            .or_insert(OptFlags::NONE)
            .insert(flags);
    }

    /// activity を選択し `opt_flags = flags` で**代入**する (`-u` / `-u ALL` / `-A` の意味論)。
    fn select_assign(&mut self, act: Activity, flags: OptFlags) {
        self.activities.insert(act, flags);
    }

    /// `add_list_item()` 相当。`max_len - 1` バイトで切り詰め、重複は追加しない。
    fn add_list_item(&mut self, act: Activity, name: &str, max_len: usize) {
        let name = truncate_bytes(name, max_len.saturating_sub(1)).to_string();
        let list = self.item_lists.entry(act).or_default();
        if !list.contains(&name) {
            list.push(name);
        }
    }

    /// `select_all_activities()` + `-A` の副作用。
    fn select_all_activities(&mut self) {
        for act in Activity::ALL {
            self.select(act);
        }
        // A_MEMORY は論理和、A_CPU / A_FS は代入。位置依存の副作用が出る点も本家どおり。
        let mut mem = OptFlags::MEMORY;
        mem.insert(OptFlags::SWAP);
        mem.insert(OptFlags::MEM_ALL);
        self.select_with(Activity::Memory, mem);
        self.select_assign(Activity::Cpu, OptFlags::CPU_ALL);
        self.select_assign(Activity::Fs, OptFlags::FILESYSTEM);
        self.flags.option_a = true;
    }

    /// `select_default_activity()` 相当。
    ///
    /// 何も選択されていなければ `A_CPU` を選ぶ。このとき `opt_flags` は
    /// `activity.c` の初期値 `AO_F_CPU_DEF` なので、出力は `-u` と完全に同じになる。
    /// CPU ビットマップが空なら bit 0 (集約行) を立てる。
    pub(crate) fn select_default_activity(&mut self) {
        if self.activities.is_empty() {
            self.select_assign(Activity::Cpu, OptFlags::CPU_DEF);
        }
        if self.cpu_bitmap.is_empty() {
            self.cpu_bitmap.set(0);
        }
    }
}

// ============================================================================
// エラー
// ============================================================================

/// `sar` 引数解析のエラー。
///
/// 本家は大半を `usage()` (stderr + exit 1) にまとめてしまうが、
/// ここでは「どのオプションが問題か」を必ず示す。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SarArgError {
    /// 束ねられたトークン中の未知の 1 文字。
    #[error("-{ch}: 不明なオプションです (トークン: {token})")]
    UnknownShortOption { token: String, ch: char },

    /// 未知の長形式オプション。
    #[error("{arg}: 不明なオプションです")]
    UnknownOption { arg: String },

    /// 必須の引数が無い。
    #[error("{opt}: 引数が必要です")]
    MissingArgument { opt: &'static str },

    /// キーワードが不正 (`-m` / `-n`)。大文字小文字を区別する。
    #[error("{opt} {value}: 不正なキーワードです (有効な値: {expected})")]
    InvalidKeyword {
        opt: &'static str,
        value: String,
        expected: &'static str,
    },

    /// `-P` の値が不正。`ALL` は単独指定のみ (`-P ALL,3` は不可)、小文字 `all` は集約行。
    #[error("-P {value}: 不正な CPU リストです (ALL / all / 0,2,4-7,12- 形式)")]
    InvalidCpuList { value: String },

    /// `-i` の値が不正 (非数値または 1 未満)。
    #[error("-i {value}: 1 以上の整数を指定してください")]
    InvalidRecordInterval { value: String },

    /// `--dec=` の値が不正。
    #[error("--dec={value}: 0, 1, 2 のいずれかを指定してください")]
    InvalidDecPlaces { value: String },

    /// `-s` / `-e` の時刻が不正。
    #[error("{opt} {value}: 時刻の形式が不正です (hh:mm / hh:mm:ss / 10 桁の epoch 秒)")]
    InvalidTimestamp { opt: &'static str, value: String },

    /// epoch 秒指定で `-e` < `-s` (`hh:mm:ss` 形式なら日跨ぎとして許される)。
    #[error("-e が -s より前の時刻です (epoch 秒指定では日跨ぎを許しません)")]
    EndBeforeStart,

    /// `-f` と `-o` の同時指定。
    #[error("-f と -o は同時に指定できません")]
    MutuallyExclusiveFromTo,

    /// `-f` / `-o` / `-[0-9]+` の競合。
    #[error("{opt}: 入力の指定が重複しています (-f / -o / -[0-9]+ は排他)")]
    ConflictingInput { opt: &'static str },

    /// `-s` / `-i` を指定したがファイル読み出しでない。
    #[error("ファイル読み出しではありません (-f オプションを使ってください)")]
    NotReadingFromFile,

    /// interval / count の組み合わせが不正。
    #[error("interval / count が不正です: {detail}")]
    InvalidIntervalCount { detail: String },

    /// `-j <type>` が長すぎる。
    #[error("-j {value}: 永続デバイス名の型が長すぎます (最大 {max} バイト)")]
    PersistentNameTooLong { value: String, max: usize },

    /// `sadf -- ` 側で受け付けられないオプション。
    #[error("-{ch}: sadf の -- 以降では指定できません")]
    NotAllowedForSadf { ch: char },
}

// ============================================================================
// 低レベルヘルパ
// ============================================================================

/// `strspn(s, DIGITS) == strlen(s)` 相当。
///
/// **空文字列は「全部数字」と判定される** (`strspn("") == 0 == strlen("")`)。
/// この癖は `-f` / `-o` のファイル名判定や `sadf` の positional 判定に効く。
pub(crate) fn is_all_digits(s: &str) -> bool {
    s.bytes().all(|b| b.is_ascii_digit())
}

/// `strspn(s, XDIGITS) == strlen(s)` 相当 (`XDIGITS = "0123456789-"`)。
fn is_xdigits(s: &str) -> bool {
    s.bytes().all(|b| b.is_ascii_digit() || b == b'-')
}

/// `atol()` 相当。先頭の空白と符号を読み、続く数字列だけを解釈する。
/// 数字が無ければ 0 (`atol("sum") == 0`)。
pub(crate) fn atol(s: &str) -> u64 {
    let s = s.trim_start();
    let s = s.strip_prefix('+').unwrap_or(s);
    let digits: String = s.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return 0;
    }
    // 桁溢れは C では未定義動作。ここでは飽和させる。
    digits.parse().unwrap_or(u64::MAX)
}

/// UTF-8 境界を壊さずに `max` バイト以下へ切り詰める。
fn truncate_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// `-f` / `-o` がファイル名として引数を消費する条件。
///
/// 「次引数が存在し、`-` で始まらず、全部数字でない」場合のみ消費する。
/// よって `sar -f 20240101` はファイル名にならず、既定ファイルが使われて
/// `20240101` が positional interval として解析される。
fn takes_filename(next: Option<&String>) -> bool {
    next.is_some_and(|s| !s.starts_with('-') && !is_all_digits(s))
}

/// `parse_valstr()` 相当。`Some(-1)` は「空文字列 = 上限/下限省略」を表す。
fn parse_valstr(s: &str, max_val: usize) -> Option<i64> {
    if s.is_empty() {
        return Some(-1);
    }
    if !is_all_digits(s) {
        return None;
    }
    let v: i64 = s.parse().ok()?;
    if v >= max_val as i64 {
        return None;
    }
    Some(v)
}

/// `parse_range_values()` 相当。`N` / `N-M` / `N-` を解釈して閉区間を返す。
///
/// - `N-` は `max_val - 1` まで**全展開**される (`--int=3-` → 3..=4095 の 4093 項目)。
/// - `-M` / `-` は失敗 (下限省略は不可)。
/// - 16 バイトを超えるトークンは `char range[16]` 相当で切り詰められる。
fn parse_range_values(token: &str, max_val: usize) -> Option<(usize, usize)> {
    if token.is_empty() {
        return None;
    }
    let token = truncate_bytes(token, MAX_RANGE_LEN - 1);
    match token.split_once('-') {
        Some((low_str, high_str)) => {
            let low = parse_valstr(low_str, max_val)?;
            if low < 0 {
                return None;
            }
            let mut high = parse_valstr(high_str, max_val)?;
            if high < 0 {
                // "N-" は上限省略
                high = max_val as i64 - 1;
            }
            if high < low {
                return None;
            }
            Some((low as usize, high as usize))
        }
        None => {
            let v = parse_valstr(token, max_val)?;
            if v < 0 {
                return None;
            }
            Some((v as usize, v as usize))
        }
    }
}

/// `parse_values()` 相当 (`-P` 用)。
pub(crate) fn parse_values(value: &str, bitmap: &mut CpuBitmap) -> Result<(), SarArgError> {
    // 文字列全体に対する判定なので、`-P ALL,3` は ALL にマッチしない。
    if value == "ALL" {
        bitmap.set_all();
        return Ok(());
    }
    for token in value.split(',').filter(|t| !t.is_empty()) {
        if token == "all" {
            // 第 0 ビット用キーワードだけは小文字。
            bitmap.set(0);
            continue;
        }
        let (low, high) =
            parse_range_values(token, NR_CPUS).ok_or_else(|| SarArgError::InvalidCpuList {
                value: token.to_string(),
            })?;
        for v in low..=high {
            bitmap.set(v + 1);
        }
    }
    Ok(())
}

/// `parse_sa_devices()` 相当 (`--dev=` / `--fs=` / `--iface=` / `--int=` 用)。
///
/// **不正な値でもエラーにならない**。範囲として解釈できなければそのまま名前として
/// 登録される (`--int=MCE-XXX` は名前、`--int=30-50` は範囲)。
pub(crate) fn parse_sa_devices(
    o: &mut SarOptions,
    act: Activity,
    value: &str,
    max_len: usize,
    max_val: usize,
) {
    for token in value.split(',').filter(|t| !t.is_empty()) {
        if max_val > NO_RANGE
            && token.len() <= MAX_RANGE_LEN
            && is_xdigits(token)
            && let Some((low, high)) = parse_range_values(token, max_val)
        {
            for v in low..=high {
                o.add_list_item(act, &v.to_string(), max_len);
            }
            continue;
        }
        o.add_list_item(act, token, max_len);
    }
}

/// `decode_timestamp()` 相当。`hh:mm:ss` の 8 バイト文字列を検証する。
fn decode_timestamp(s: &str, opt: &'static str) -> Result<TimeSpec, SarArgError> {
    let err = || SarArgError::InvalidTimestamp {
        opt,
        value: s.to_string(),
    };
    let b = s.as_bytes();
    if b.len() != 8 {
        return Err(err());
    }
    let seg = |from: usize| -> Option<u8> {
        let part = s.get(from..from + 2)?;
        if part.len() == 2 && is_all_digits(part) {
            part.parse().ok()
        } else {
            None
        }
    };
    let (Some(hour), Some(min), Some(sec)) = (seg(0), seg(3), seg(6)) else {
        return Err(err());
    };
    if hour > 23 || min > 59 || sec > 59 {
        return Err(err());
    }
    Ok(TimeSpec::HhMmSs { hour, min, sec })
}

/// `decode_epoch()` 相当。`"0000000000"` もエラー扱い。
fn decode_epoch(s: &str, opt: &'static str) -> Result<TimeSpec, SarArgError> {
    let v: u64 = s.parse().map_err(|_| SarArgError::InvalidTimestamp {
        opt,
        value: s.to_string(),
    })?;
    if v == 0 {
        return Err(SarArgError::InvalidTimestamp {
            opt,
            value: s.to_string(),
        });
    }
    Ok(TimeSpec::Epoch(v))
}

/// `parse_timestamp()` 相当 (`-s` / `-e`)。
///
/// **`opt` は常に 1 進む**。そのうえで次引数の「長さと区切り位置」が条件を満たす
/// ときだけ値として消費する。満たさない場合は既定値 (`-s` = 08:00:00 /
/// `-e` = 18:00:00) が使われ、その引数は次のループで再解析される。
pub(crate) fn parse_timestamp(
    argv: &[String],
    opt: &mut usize,
    def: (u8, u8, u8),
    which: &'static str,
) -> Result<TimeSpec, SarArgError> {
    *opt += 1;
    let mut value: Option<String> = None;
    if let Some(s) = argv.get(*opt)
        && !s.starts_with('-')
    {
        let b = s.as_bytes();
        match b.len() {
            5 if b[2] == b':' => {
                value = Some(format!("{s}:00"));
                *opt += 1;
            }
            8 if b[2] == b':' && b[5] == b':' => {
                value = Some(s.clone());
                *opt += 1;
            }
            10 if is_all_digits(s) => {
                let epoch = decode_epoch(s, which)?;
                *opt += 1;
                return Ok(epoch);
            }
            _ => {}
        }
    }
    match value {
        Some(v) => decode_timestamp(&v, which),
        None => Ok(TimeSpec::HhMmSs {
            hour: def.0,
            min: def.1,
            sec: def.2,
        }),
    }
}

/// `check_time_limits()` 相当。
///
/// - `hh:mm:ss` 同士で `-e` の時が `-s` の時より小さいなら日跨ぎとみなし `hour += 24`。
///   比較しているのは **時 (hour) だけ** なので `-s 13:30:00 -e 13:10:00` は
///   ラップ扱いにならず、条件を満たすレコードが無いまま正常終了する。
/// - epoch 同士で `-e` < `-s` は即エラー。
/// - 片方だけ epoch の混在はチェックされない。
pub(crate) fn check_time_limits(start: TimeSpec, end: &mut TimeSpec) -> Result<(), SarArgError> {
    match (start, *end) {
        (TimeSpec::HhMmSs { hour: sh, .. }, TimeSpec::HhMmSs { hour: eh, min, sec }) if eh < sh => {
            *end = TimeSpec::HhMmSs {
                hour: eh + 24,
                min,
                sec,
            };
        }
        (TimeSpec::Epoch(s), TimeSpec::Epoch(e)) if e < s => {
            return Err(SarArgError::EndBeforeStart);
        }
        _ => {}
    }
    Ok(())
}

// ============================================================================
// キーワード解析 (-m / -n / -q)
// ============================================================================

/// `parse_sar_m_opt()` 相当 (電源管理キーワード)。大文字完全一致。
pub(crate) fn parse_sar_m_opt(value: &str, o: &mut SarOptions) -> Result<(), SarArgError> {
    for token in value.split(',').filter(|t| !t.is_empty()) {
        match token {
            "CPU" => o.select(Activity::PwrCpu),
            "FAN" => o.select(Activity::PwrFan),
            "IN" => o.select(Activity::PwrIn),
            "TEMP" => o.select(Activity::PwrTemp),
            "FREQ" => o.select(Activity::PwrFreq),
            "USB" => o.select(Activity::PwrUsb),
            "BAT" => o.select(Activity::PwrBat),
            "ALL" => {
                for act in Activity::PWR_ALL {
                    o.select(act);
                }
            }
            other => {
                return Err(SarArgError::InvalidKeyword {
                    opt: "-m",
                    value: other.to_string(),
                    expected: "CPU, FAN, IN, TEMP, FREQ, USB, BAT, ALL",
                });
            }
        }
    }
    Ok(())
}

/// `parse_sar_n_opt()` 相当 (ネットワークキーワード)。大文字完全一致。
/// `ALL` は 20 種すべてを選択し、除外キーワードは存在しない。
pub(crate) fn parse_sar_n_opt(value: &str, o: &mut SarOptions) -> Result<(), SarArgError> {
    for token in value.split(',').filter(|t| !t.is_empty()) {
        let act = match token {
            "DEV" => Activity::NetDev,
            "EDEV" => Activity::NetEdev,
            "SOCK" => Activity::NetSock,
            "NFS" => Activity::NetNfs,
            "NFSD" => Activity::NetNfsd,
            "IP" => Activity::NetIp,
            "EIP" => Activity::NetEip,
            "ICMP" => Activity::NetIcmp,
            "EICMP" => Activity::NetEicmp,
            "TCP" => Activity::NetTcp,
            "ETCP" => Activity::NetEtcp,
            "UDP" => Activity::NetUdp,
            "SOCK6" => Activity::NetSock6,
            "IP6" => Activity::NetIp6,
            "EIP6" => Activity::NetEip6,
            "ICMP6" => Activity::NetIcmp6,
            "EICMP6" => Activity::NetEicmp6,
            "UDP6" => Activity::NetUdp6,
            "FC" => Activity::NetFc,
            "SOFT" => Activity::NetSoft,
            "ALL" => {
                for act in Activity::NET_ALL {
                    o.select(act);
                }
                continue;
            }
            other => {
                return Err(SarArgError::InvalidKeyword {
                    opt: "-n",
                    value: other.to_string(),
                    expected: "DEV, EDEV, SOCK, NFS, NFSD, IP, EIP, ICMP, EICMP, TCP, ETCP, \
                               UDP, SOCK6, IP6, EIP6, ICMP6, EICMP6, UDP6, FC, SOFT, ALL",
                });
            }
        };
        o.select(act);
    }
    Ok(())
}

/// `parse_sar_q_opt()` 相当 (負荷 / pressure-stall キーワード)。
///
/// 失敗を `Err(())` で返すのは、**`-q` だけは usage を出さず `A_QUEUE` を選んで
/// そのトークンを次の引数として再解析する**という特殊な回復をするため。
pub(crate) fn parse_sar_q_opt(value: &str, o: &mut SarOptions) -> Result<(), ()> {
    for token in value.split(',').filter(|t| !t.is_empty()) {
        match token {
            "LOAD" => o.select(Activity::Queue),
            "CPU" => o.select(Activity::PsiCpu),
            "IO" => o.select(Activity::PsiIo),
            "MEM" => o.select(Activity::PsiMem),
            "PSI" => {
                for act in Activity::PSI_ALL {
                    o.select(act);
                }
            }
            "ALL" => {
                o.select(Activity::Queue);
                for act in Activity::PSI_ALL {
                    o.select(act);
                }
            }
            _ => return Err(()),
        }
    }
    Ok(())
}

// ============================================================================
// 1 文字オプション (束ね可能)
// ============================================================================

/// `parse_sar_opt()` 相当。1 トークンを 1 文字ずつ処理する (`-bBruW` のように束ねられる)。
///
/// `opt` は処理したトークン数だけ進む (キーワード引数を消費した場合は 2 進む)。
pub(crate) fn parse_sar_opt(
    argv: &[String],
    opt: &mut usize,
    caller: Caller,
    o: &mut SarOptions,
) -> Result<(), SarArgError> {
    let token = argv[*opt].clone();
    let bytes = token.as_bytes();
    let next = argv.get(*opt + 1);
    // キーワード引数を消費したか (消費したらその場でトークン処理を打ち切る)
    let mut consumed_next = false;

    let mut i = 1;
    while i < bytes.len() {
        let ch = char::from(bytes[i]);
        // 「このオプション文字がトークン末尾にあるか」がキーワード消費の前提条件。
        let is_last = i + 1 == bytes.len();
        match ch {
            'A' => o.select_all_activities(),
            'B' => o.select(Activity::Page),
            'b' => o.select(Activity::Io),
            'C' => o.flags.comment = true,
            'd' => o.select(Activity::Disk),
            'F' => {
                if is_last && next.is_some_and(|s| s == "MOUNT") {
                    o.select_with(Activity::Fs, OptFlags::MOUNT);
                    consumed_next = true;
                    break;
                }
                o.select_with(Activity::Fs, OptFlags::FILESYSTEM);
            }
            'H' => o.select(Activity::Huge),
            // -h は help ではなく `--pretty --human` 相当。
            'h' => {
                o.flags.pretty = true;
                o.flags.human = true;
            }
            'I' => {
                o.select(Activity::Irq);
                if is_last {
                    match next.map(String::as_str) {
                        // SUM は小文字 "sum" を item リストに入れる。
                        Some("SUM") => {
                            o.add_list_item(Activity::Irq, "sum", MAX_SA_IRQ_LEN);
                            consumed_next = true;
                            break;
                        }
                        // ALL は消費されるが何もしない (コード上のコメント: Keyword ALL is ignored)。
                        Some("ALL") => {
                            consumed_next = true;
                            break;
                        }
                        _ => {}
                    }
                }
                // 数値は受け付けない (12.5.6 以降は --int= へ移行済み)。
                // `sar -I 3` は「-I (全割り込み) + positional interval=3」になる。
            }
            'j' => {
                // トークン内の位置に関係なく次引数を消費し、残りの文字は処理しない。
                let Some(value) = next else {
                    return Err(SarArgError::MissingArgument { opt: "-j" });
                };
                if value == "SID" {
                    o.flags.dev_sid = true;
                    o.flags.pretty = true;
                    o.persistent_name = Some(PersistentName::Sid);
                } else {
                    if value.len() >= MAX_FILE_LEN - 1 {
                        return Err(SarArgError::PersistentNameTooLong {
                            value: truncate_bytes(value, 32).to_string(),
                            max: MAX_FILE_LEN - 2,
                        });
                    }
                    o.flags.persist_name = true;
                    o.flags.pretty = true;
                    o.persistent_name = Some(PersistentName::ByType(value.to_lowercase()));
                }
                consumed_next = true;
                break;
            }
            'p' => o.flags.pretty = true,
            // 束ねられた形の -q はキーワード解析をしない。
            'q' => o.select(Activity::Queue),
            'r' => {
                o.select_with(Activity::Memory, OptFlags::MEMORY);
                if is_last && next.is_some_and(|s| s == "ALL") {
                    o.select_with(Activity::Memory, OptFlags::MEM_ALL);
                    consumed_next = true;
                    break;
                }
            }
            'S' => o.select_with(Activity::Memory, OptFlags::SWAP),
            't' => {
                // sadf の -t は `--` の前で指定する。`sadf -- -t` は usage。
                if caller != Caller::Sar {
                    return Err(SarArgError::NotAllowedForSadf { ch: 't' });
                }
                o.flags.true_time = true;
            }
            'u' => {
                // -u / -u ALL はいずれも代入なので後勝ち。
                if is_last && next.is_some_and(|s| s == "ALL") {
                    o.select_assign(Activity::Cpu, OptFlags::CPU_ALL);
                    consumed_next = true;
                    break;
                }
                o.select_assign(Activity::Cpu, OptFlags::CPU_DEF);
            }
            'v' => o.select(Activity::Ktables),
            'w' => o.select(Activity::Pcsw),
            'W' => o.select(Activity::Swap),
            'x' => {
                // C_SADF では受け付けるが無言で無視 (エラーにならない)。
                if caller == Caller::Sar {
                    o.flags.minmax = true;
                }
            }
            'y' => o.select(Activity::Serial),
            'z' => o.flags.zero_omit = true,
            _ => {
                return Err(SarArgError::UnknownShortOption {
                    token: token.clone(),
                    ch,
                });
            }
        }
        i += 1;
    }

    *opt += 1 + usize::from(consumed_next);
    Ok(())
}

// ============================================================================
// エントリポイント
// ============================================================================

/// `sar` 互換の引数列を解析する。
///
/// `argv` はプログラム名を**含まない** sar オプション部分。
///
/// ```
/// use re_sar_ch::cli::sar_args::{parse_sar_args, Activity, OptFlags, SarInput};
///
/// let argv: Vec<String> = ["-u", "-f", "sa01"].iter().map(|s| s.to_string()).collect();
/// let opts = parse_sar_args(&argv).unwrap();
/// assert!(opts.is_selected(Activity::Cpu));
/// assert_eq!(opts.opt_flags(Activity::Cpu), OptFlags::CPU_DEF);
/// assert_eq!(opts.input, Some(SarInput::File("sa01".into())));
/// ```
pub fn parse_sar_args(argv: &[String]) -> Result<SarOptions, SarArgError> {
    // -q のキーワード解析が失敗したとき、本家は strtok がその場でカンマを NUL に
    // 置換した状態の argv をそのまま再解析する (`-q 2,5` の `5` は失われる)。
    // 同じ挙動を再現するために argv のローカルコピーを書き換える。
    let mut argv: Vec<String> = argv.to_vec();
    let argc = argv.len();
    let mut o = SarOptions::default();
    let mut opt = 0usize;

    while opt < argv.len() {
        let arg = argv[opt].clone();

        if arg == "--sadc" {
            o.immediate = Some(SarImmediate::Sadc);
            return Ok(o);
        } else if let Some(value) = arg.strip_prefix("--dev=") {
            parse_sa_devices(&mut o, Activity::Disk, value, MAX_DEV_LEN, NO_RANGE);
            opt += 1;
        } else if let Some(value) = arg.strip_prefix("--fs=") {
            parse_sa_devices(&mut o, Activity::Fs, value, MAX_FS_LEN, NO_RANGE);
            opt += 1;
        } else if let Some(value) = arg.strip_prefix("--iface=") {
            // A_NET_DEV に登録したリストを A_NET_EDEV にも共有する (C 側はポインタ共有)。
            parse_sa_devices(&mut o, Activity::NetDev, value, MAX_IFACE_LEN, NO_RANGE);
            if let Some(list) = o.item_lists.get(&Activity::NetDev).cloned() {
                o.item_lists.insert(Activity::NetEdev, list);
            }
            opt += 1;
        } else if let Some(value) = arg.strip_prefix("--int=") {
            parse_sa_devices(&mut o, Activity::Irq, value, MAX_SA_IRQ_LEN, NR_IRQS);
            opt += 1;
        } else if arg == "--help" {
            o.immediate = Some(SarImmediate::Help);
            return Ok(o);
        } else if arg == "--human" {
            o.flags.human = true;
            opt += 1;
        } else if arg == "--pretty" {
            o.flags.pretty = true;
            opt += 1;
        } else if arg.starts_with("--dec=") && arg.len() == 7 {
            // 長さがちょうど 7 でなければこの分岐に入らない
            // (`--dec=12` は 1 文字ループへ落ちて先頭の '-' が未知文字になる)。
            let value = &arg[6..];
            match value {
                "0" | "1" | "2" => o.dec_places = Some(value.as_bytes()[0] - b'0'),
                _ => {
                    return Err(SarArgError::InvalidDecPlaces {
                        value: value.to_string(),
                    });
                }
            }
            opt += 1;
        } else if arg == "-D" {
            o.flags.sa_yyyymmdd = true;
            opt += 1;
        } else if arg == "-P" {
            opt += 1;
            let Some(value) = argv.get(opt).cloned() else {
                return Err(SarArgError::MissingArgument { opt: "-P" });
            };
            parse_values(&value, &mut o.cpu_bitmap)?;
            o.flags.option_p = true;
            opt += 1;
        } else if arg == "-V" {
            o.immediate = Some(SarImmediate::Version);
            return Ok(o);
        } else if arg == "-o" {
            if o.output.is_some() {
                return Err(SarArgError::ConflictingInput { opt: "-o" });
            }
            if takes_filename(argv.get(opt + 1)) {
                o.output = Some(SarOutput::File(PathBuf::from(&argv[opt + 1])));
                opt += 2;
            } else {
                o.output = Some(SarOutput::DefaultDaily);
                opt += 1;
            }
        } else if arg == "-f" {
            if o.input.is_some() || o.day_offset != 0 {
                return Err(SarArgError::ConflictingInput { opt: "-f" });
            }
            if takes_filename(argv.get(opt + 1)) {
                o.input = Some(SarInput::File(PathBuf::from(&argv[opt + 1])));
                opt += 2;
            } else {
                o.input = Some(SarInput::DefaultDaily);
                o.default_file_used = true;
                opt += 1;
            }
        } else if arg == "-s" {
            o.tm_start = parse_timestamp(&argv, &mut opt, DEF_TMSTART, "-s")?;
        } else if arg == "-e" {
            o.tm_end = parse_timestamp(&argv, &mut opt, DEF_TMEND, "-e")?;
        } else if arg == "-i" {
            opt += 1;
            let value = argv.get(opt).cloned().unwrap_or_default();
            if value.is_empty() || !is_all_digits(&value) {
                return Err(SarArgError::InvalidRecordInterval { value });
            }
            let interval = atol(&value);
            if interval < 1 {
                return Err(SarArgError::InvalidRecordInterval { value });
            }
            o.interval = Some(interval);
            o.flags.interval_set = true;
            opt += 1;
        } else if arg == "-m" {
            let Some(value) = argv.get(opt + 1).cloned() else {
                return Err(SarArgError::MissingArgument { opt: "-m" });
            };
            parse_sar_m_opt(&value, &mut o)?;
            opt += 2;
        } else if arg == "-n" {
            let Some(value) = argv.get(opt + 1).cloned() else {
                return Err(SarArgError::MissingArgument { opt: "-n" });
            };
            parse_sar_n_opt(&value, &mut o)?;
            opt += 2;
        } else if arg == "-q" {
            match argv.get(opt + 1).cloned() {
                Some(value) => {
                    if parse_sar_q_opt(&value, &mut o).is_err() {
                        // usage は出さない。A_QUEUE を選択し、strtok に切られた
                        // トークンを次の引数として再解析する。
                        o.select(Activity::Queue);
                        let truncated = value.split(',').next().unwrap_or_default().to_string();
                        argv[opt + 1] = truncated;
                        opt += 1;
                    } else {
                        opt += 2;
                    }
                }
                None => {
                    // 引数なしの -q は LOAD 相当。
                    o.select(Activity::Queue);
                    opt += 1;
                }
            }
        } else if is_day_offset(&arg) {
            if o.input.is_some() || o.day_offset != 0 {
                return Err(SarArgError::ConflictingInput { opt: "-[0-9]+" });
            }
            o.day_offset = atol(&arg[1..]) as u32;
            opt += 1;
        } else if arg.starts_with('-') {
            parse_sar_opt(&argv, &mut opt, Caller::Sar, &mut o)?;
        } else if o.interval.is_none() {
            // positional 第 1 引数 = interval。atol なので非数値は 0 になる。
            o.interval = Some(atol(&arg));
            opt += 1;
        } else {
            // positional 第 2 引数 = count。`count < 1` または interval が 0 なら usage。
            let count = atol(&arg);
            if count < 1 || o.interval == Some(0) {
                return Err(SarArgError::InvalidIntervalCount {
                    detail: format!(
                        "count={count}, interval={:?} (count は 1 以上、かつ interval が 0 のときは指定できません)",
                        o.interval
                    ),
                });
            }
            o.count = Some(count);
            opt += 1;
        }
    }

    finalize(&mut o, argc)?;
    Ok(o)
}

/// `-[0-9]+` (日オフセット) の判定。`strlen > 1 && strlen < 7` なので `-1`〜`-99999`。
pub(crate) fn is_day_offset(arg: &str) -> bool {
    arg.len() > 1 && arg.len() < 7 && arg.starts_with('-') && is_all_digits(&arg[1..])
}

/// 解析ループ終了後の後処理 (`sar.c` の `main()` と同じ順序)。
fn finalize(o: &mut SarOptions, argc: usize) -> Result<(), SarArgError> {
    // (1) 既定の日次ファイルへのフォールバック。
    //     ここで from_file が埋まるため、後続の「ファイル読み出しでない」判定に効く。
    if argc == 0
        || ((o.interval.is_none() || o.flags.interval_set)
            && o.input.is_none()
            && o.output.is_none())
    {
        o.input = Some(SarInput::DefaultDaily);
        o.default_file_used = true;
    }

    // (2) 時刻範囲の整合 (日跨ぎ補正 / epoch 逆順のエラー)。
    check_time_limits(o.tm_start, &mut o.tm_end)?;

    // (3) -f と -o は排他。
    if o.input.is_some() && o.output.is_some() {
        return Err(SarArgError::MutuallyExclusiveFromTo);
    }

    // (4) -A かつ -P 未指定なら CPU ビットマップを全ビット立てる (set_bitmaps())。
    if o.flags.option_a && !o.flags.option_p {
        o.cpu_bitmap.set_all();
    }

    // (5) -s / -i はファイル読み出し専用。
    if (!o.tm_start.is_none() || o.flags.interval_set) && o.input.is_none() {
        return Err(SarArgError::NotReadingFromFile);
    }

    // (6) interval == 0 (起動以降の平均) はライブ採取専用。
    if o.interval == Some(0) && (o.input.is_some() || o.output.is_some()) {
        return Err(SarArgError::InvalidIntervalCount {
            detail: "interval=0 は -f / -o と併用できません".to_string(),
        });
    }

    // (7) -o と日オフセットは併用不可。
    if o.output.is_some() && o.day_offset != 0 {
        return Err(SarArgError::ConflictingInput { opt: "-o" });
    }

    // (8) activity が何も選ばれていなければ A_CPU (opt_flags は初期値 AO_F_CPU_DEF)。
    o.select_default_activity();

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// テスト用: 文字列スライスから argv を作る。
    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| (*s).to_string()).collect()
    }

    fn parse(args: &[&str]) -> SarOptions {
        parse_sar_args(&argv(args)).expect("解析に成功するはず")
    }

    fn parse_err(args: &[&str]) -> SarArgError {
        parse_sar_args(&argv(args)).expect_err("解析に失敗するはず")
    }

    fn selected(o: &SarOptions) -> Vec<Activity> {
        o.selected_activities().collect()
    }

    // ---------------------------------------------------------------
    // 要件の中心: `resarch -u -f sa01`
    // ---------------------------------------------------------------

    #[test]
    fn u_with_file() {
        let o = parse(&["-u", "-f", "sa01"]);
        assert_eq!(selected(&o), vec![Activity::Cpu]);
        assert_eq!(o.opt_flags(Activity::Cpu), OptFlags::CPU_DEF);
        assert!(!o.opt_flags(Activity::Cpu).contains(OptFlags::CPU_ALL));
        assert_eq!(o.input, Some(SarInput::File(PathBuf::from("sa01"))));
        assert!(!o.default_file_used);
        // -P 未指定なら集約行のみ
        assert!(o.cpu_bitmap.aggregate_selected());
        assert_eq!(o.cpu_bitmap.count_bits(), 1);
        assert_eq!(o.interval, None);
        assert_eq!(o.count, None);
        assert!(o.flags.local_time, "sar は既定でローカル時刻表示");
    }

    #[test]
    fn no_activity_defaults_to_cpu_def() {
        // オプション無指定の sar は sar -u と同一出力になる。
        let o = parse(&["-f", "sa01"]);
        assert_eq!(selected(&o), vec![Activity::Cpu]);
        assert_eq!(o.opt_flags(Activity::Cpu), OptFlags::CPU_DEF);
        assert_eq!(o.cpu_bitmap.count_bits(), 1);
    }

    #[test]
    fn empty_argv_uses_default_daily_file() {
        let o = parse(&[]);
        assert_eq!(o.input, Some(SarInput::DefaultDaily));
        assert!(o.default_file_used);
        assert_eq!(selected(&o), vec![Activity::Cpu]);
    }

    // ---------------------------------------------------------------
    // -u / -r / -F / -S の opt_flags セマンティクス
    // ---------------------------------------------------------------

    #[test]
    fn u_all_assigns_cpu_all() {
        let o = parse(&["-u", "ALL", "-f", "sa01"]);
        assert_eq!(o.opt_flags(Activity::Cpu), OptFlags::CPU_ALL);
    }

    #[test]
    fn u_is_assignment_last_wins() {
        // -u / -u ALL は代入なので後勝ち
        let o = parse(&["-u", "ALL", "-u", "-f", "sa01"]);
        assert_eq!(o.opt_flags(Activity::Cpu), OptFlags::CPU_DEF);
        let o = parse(&["-u", "-u", "ALL", "-f", "sa01"]);
        assert_eq!(o.opt_flags(Activity::Cpu), OptFlags::CPU_ALL);
    }

    #[test]
    fn a_then_u_and_u_then_a() {
        // -A も A_CPU には代入するので位置依存
        let o = parse(&["-A", "-u", "-f", "sa01"]);
        assert_eq!(o.opt_flags(Activity::Cpu), OptFlags::CPU_DEF);
        let o = parse(&["-u", "-A", "-f", "sa01"]);
        assert_eq!(o.opt_flags(Activity::Cpu), OptFlags::CPU_ALL);
    }

    #[test]
    fn u_all_lowercase_is_not_a_keyword() {
        // -u all の "all" は消費されず positional として再解析され atol("all") = 0
        let o = parse(&["-u", "all"]);
        assert_eq!(o.opt_flags(Activity::Cpu), OptFlags::CPU_DEF);
        assert_eq!(o.interval, Some(0));
    }

    #[test]
    fn u_all_not_at_token_end_is_not_consumed() {
        // -uw ALL: 'u' はトークン末尾ではないのでキーワードを消費しない
        let o = parse(&["-uw", "ALL"]);
        assert_eq!(o.opt_flags(Activity::Cpu), OptFlags::CPU_DEF);
        assert!(o.is_selected(Activity::Pcsw));
        // "ALL" は positional interval に吸われる (atol("ALL") = 0)
        assert_eq!(o.interval, Some(0));
    }

    #[test]
    fn interval_zero_with_file_is_rejected() {
        let err = parse_err(&["-uw", "ALL", "-f", "sa01"]);
        assert!(matches!(err, SarArgError::InvalidIntervalCount { .. }));
    }

    #[test]
    fn r_and_s_are_additive() {
        let o = parse(&["-rS", "-f", "sa01"]);
        let flags = o.opt_flags(Activity::Memory);
        assert!(flags.contains(OptFlags::MEMORY));
        assert!(flags.contains(OptFlags::SWAP));
        assert!(!flags.contains(OptFlags::MEM_ALL));
    }

    #[test]
    fn r_all_adds_mem_all() {
        let o = parse(&["-r", "ALL", "-f", "sa01"]);
        let flags = o.opt_flags(Activity::Memory);
        assert!(flags.contains(OptFlags::MEMORY));
        assert!(flags.contains(OptFlags::MEM_ALL));
    }

    #[test]
    fn f_mount_and_f_are_additive() {
        let o = parse(&["-F", "-F", "MOUNT", "-f", "sa01"]);
        let flags = o.opt_flags(Activity::Fs);
        assert!(flags.contains(OptFlags::FILESYSTEM));
        assert!(flags.contains(OptFlags::MOUNT));
    }

    #[test]
    fn f_mount_lowercase_is_not_a_keyword() {
        // -F mount の "mount" はキーワードにならず positional 扱い
        let o = parse(&["-F", "mount"]);
        assert_eq!(o.opt_flags(Activity::Fs), OptFlags::FILESYSTEM);
        assert_eq!(o.interval, Some(0));
    }

    // ---------------------------------------------------------------
    // -h / -H / --human / --pretty の混同ポイント
    // ---------------------------------------------------------------

    #[test]
    fn h_is_pretty_plus_human_not_help() {
        let o = parse(&["-h", "-f", "sa01"]);
        assert!(o.flags.pretty);
        assert!(o.flags.human);
        assert!(o.immediate.is_none(), "-h はヘルプではない");
        assert!(!o.is_selected(Activity::Huge));
    }

    #[test]
    fn capital_h_is_hugepages() {
        let o = parse(&["-H", "-f", "sa01"]);
        assert!(o.is_selected(Activity::Huge));
        assert!(!o.flags.pretty);
        assert!(!o.flags.human);
    }

    #[test]
    fn human_and_pretty_are_separate() {
        let o = parse(&["--human", "-A", "-f", "sa01"]);
        assert!(o.flags.human);
        assert!(!o.flags.pretty);

        let o = parse(&["--pretty", "-d", "-f", "sa01"]);
        assert!(o.flags.pretty);
        assert!(!o.flags.human);

        let o = parse(&["-p", "-f", "sa01"]);
        assert!(o.flags.pretty);
        assert!(!o.flags.human);
    }

    // ---------------------------------------------------------------
    // -P
    // ---------------------------------------------------------------

    #[test]
    fn p_all_uppercase_selects_every_cpu() {
        let o = parse(&["-u", "-P", "ALL", "-f", "sa01"]);
        assert!(o.flags.option_p);
        assert!(o.cpu_bitmap.aggregate_selected());
        assert!(o.cpu_bitmap.is_set(1));
        assert!(o.cpu_bitmap.is_set(NR_CPUS));
        assert!(o.cpu_bitmap.count_bits() > 1000);
    }

    #[test]
    fn p_all_lowercase_selects_aggregate_only() {
        // -P ALL と -P all は別物
        let o = parse(&["-u", "-P", "all", "-f", "sa01"]);
        assert!(o.flags.option_p);
        assert!(o.cpu_bitmap.aggregate_selected());
        assert_eq!(o.cpu_bitmap.count_bits(), 1);
        assert_eq!(o.cpu_bitmap.selected_cpus().count(), 0);
    }

    #[test]
    fn p_mixed_list() {
        let o = parse(&["-P", "all,3", "-f", "sa01"]);
        assert!(o.cpu_bitmap.aggregate_selected());
        assert_eq!(o.cpu_bitmap.selected_cpus().collect::<Vec<_>>(), vec![3]);
    }

    #[test]
    fn p_range() {
        let o = parse(&["-P", "0-2", "-f", "sa01"]);
        assert!(!o.cpu_bitmap.aggregate_selected());
        assert_eq!(
            o.cpu_bitmap.selected_cpus().collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
    }

    #[test]
    fn p_open_ended_range_expands_to_max() {
        let o = parse(&["-P", "1020-", "-f", "sa01"]);
        assert_eq!(
            o.cpu_bitmap.selected_cpus().collect::<Vec<_>>(),
            vec![1020, 1021, 1022, 1023]
        );
    }

    #[test]
    fn p_all_with_list_is_an_error() {
        // ALL の判定は文字列全体に対して行われるので ALL,3 はトークン ALL が不正
        let err = parse_err(&["-P", "ALL,3", "-f", "sa01"]);
        assert!(matches!(err, SarArgError::InvalidCpuList { .. }));
    }

    #[test]
    fn p_without_argument_is_an_error() {
        assert_eq!(
            parse_err(&["-u", "-P"]),
            SarArgError::MissingArgument { opt: "-P" }
        );
    }

    #[test]
    fn p_lower_bound_omitted_is_an_error() {
        // -P は次引数を無条件に消費するので "-3" も値として解析される
        assert!(matches!(
            parse_err(&["-P", "-3", "-f", "sa01"]),
            SarArgError::InvalidCpuList { .. }
        ));
        assert!(matches!(
            parse_err(&["-P", "1,-", "-f", "sa01"]),
            SarArgError::InvalidCpuList { .. }
        ));
    }

    #[test]
    fn a_implies_p_all_unless_p_given() {
        let o = parse(&["-A", "-f", "sa01"]);
        assert!(o.cpu_bitmap.count_bits() > 1000);

        let o = parse(&["-A", "-P", "1", "-f", "sa01"]);
        assert_eq!(o.cpu_bitmap.selected_cpus().collect::<Vec<_>>(), vec![1]);
    }

    // ---------------------------------------------------------------
    // -I (数値を取らない) と --int=
    // ---------------------------------------------------------------

    #[test]
    fn i_upper_does_not_take_a_number() {
        // `sar -I 3` は「-I (全割り込み) + positional interval=3」
        let o = parse(&["-I", "3"]);
        assert!(o.is_selected(Activity::Irq));
        assert_eq!(o.interval, Some(3));
        assert!(!o.list_on_cmdline(Activity::Irq));
    }

    #[test]
    fn i_sum_adds_lowercase_sum_item() {
        let o = parse(&["-I", "SUM", "-f", "sa01"]);
        assert!(o.is_selected(Activity::Irq));
        assert_eq!(o.item_list(Activity::Irq), ["sum"]);
        assert!(o.list_on_cmdline(Activity::Irq));
    }

    #[test]
    fn i_all_is_consumed_but_ignored() {
        let o = parse(&["-I", "ALL", "-f", "sa01"]);
        assert!(o.is_selected(Activity::Irq));
        assert!(!o.list_on_cmdline(Activity::Irq));
        assert_eq!(
            o.interval, None,
            "ALL は消費されるので positional にならない"
        );
    }

    #[test]
    fn i_sum_lowercase_is_not_consumed() {
        // -I sum の "sum" は消費されず positional interval = atol("sum") = 0
        let o = parse(&["-I", "sum"]);
        assert!(!o.list_on_cmdline(Activity::Irq));
        assert_eq!(o.interval, Some(0));
    }

    #[test]
    fn int_list_ranges_and_names() {
        // expected.sar-I の実コマンド由来
        let o = parse(&[
            "-I",
            "--int=0,3,30-50,4000-,LOC,PWD,MCE-XXX,TLB,sum",
            "-P",
            "all,3",
            "--pretty",
            "-f",
            "sa01",
        ]);
        let items = o.item_list(Activity::Irq);
        // 0, 3, 30..=50 (21), 4000..=4095 (96), LOC, PWD, MCE-XXX, TLB, sum (5)
        assert_eq!(items.len(), 1 + 1 + 21 + 96 + 5);
        assert_eq!(items[0], "0");
        assert!(items.contains(&"4095".to_string()));
        assert!(items.contains(&"MCE-XXX".to_string()));
        assert!(items.contains(&"sum".to_string()));
        assert!(o.flags.pretty);
        assert!(o.cpu_bitmap.aggregate_selected());
        assert_eq!(o.cpu_bitmap.selected_cpus().collect::<Vec<_>>(), vec![3]);
    }

    #[test]
    fn int_open_range_expands_to_nr_irqs_minus_one() {
        let o = parse(&["-I", "--int=3-", "-f", "sa01"]);
        assert_eq!(o.item_list(Activity::Irq).len(), NR_IRQS - 3);
        assert_eq!(o.item_list(Activity::Irq).last().unwrap(), "4095");
    }

    #[test]
    fn int_item_names_are_truncated_and_deduplicated() {
        let o = parse(&["--int=ABCDEFGHIJ,ABCDEFGHXX,LOC,LOC", "-f", "sa01"]);
        // MAX_SA_IRQ_LEN = 8 → 7 バイトで切り詰め、重複は追加しない
        assert_eq!(o.item_list(Activity::Irq), ["ABCDEFG", "LOC"]);
    }

    // ---------------------------------------------------------------
    // --dev= / --fs= / --iface=
    // ---------------------------------------------------------------

    #[test]
    fn dev_list() {
        let o = parse(&["-d", "--dev=sda,sdb", "-f", "sa01"]);
        assert_eq!(o.item_list(Activity::Disk), ["sda", "sdb"]);
        assert!(o.list_on_cmdline(Activity::Disk));
    }

    #[test]
    fn dev_list_accepts_any_value_without_error() {
        // --dev= 系は不正値でもエラーにならない (名前として登録される)
        let o = parse(&["-d", "--dev=1-3", "-f", "sa01"]);
        assert_eq!(
            o.item_list(Activity::Disk),
            ["1-3"],
            "範囲指定は不可 (NO_RANGE)"
        );
    }

    #[test]
    fn empty_fs_list_means_no_filter() {
        let o = parse(&["-F", "--fs=", "-f", "sa01"]);
        assert!(!o.list_on_cmdline(Activity::Fs));
        assert!(o.item_list(Activity::Fs).is_empty());
    }

    #[test]
    fn iface_list_is_shared_with_net_edev() {
        let o = parse(&["-n", "DEV,EDEV", "--iface=eth0", "-f", "sa01"]);
        assert_eq!(o.item_list(Activity::NetDev), ["eth0"]);
        assert_eq!(o.item_list(Activity::NetEdev), ["eth0"]);
        assert!(o.list_on_cmdline(Activity::NetEdev));
    }

    #[test]
    fn iface_names_are_truncated_to_15_bytes() {
        let o = parse(&["--iface=0123456789abcdefghij", "-f", "sa01"]);
        assert_eq!(o.item_list(Activity::NetDev), ["0123456789abcde"]);
    }

    #[test]
    fn fs_list_matches_device_or_mountpoint() {
        let o = parse(&["-F", "--fs=/dev/sda1,/home", "-f", "sa01"]);
        assert_eq!(o.item_list(Activity::Fs), ["/dev/sda1", "/home"]);
    }

    // ---------------------------------------------------------------
    // -m / -n / -q キーワード
    // ---------------------------------------------------------------

    #[test]
    fn n_keywords() {
        let o = parse(&["-n", "DEV,EDEV", "-f", "sa01"]);
        assert_eq!(selected(&o), vec![Activity::NetDev, Activity::NetEdev]);
    }

    #[test]
    fn n_all_selects_twenty_activities() {
        let o = parse(&["-n", "ALL", "-f", "sa01"]);
        assert_eq!(o.activities.len(), 20);
        for act in Activity::NET_ALL {
            assert!(o.is_selected(act), "{} が選択されていない", act.name());
        }
    }

    #[test]
    fn n_keywords_are_case_sensitive() {
        assert!(matches!(
            parse_err(&["-n", "dev", "-f", "sa01"]),
            SarArgError::InvalidKeyword { opt: "-n", .. }
        ));
    }

    #[test]
    fn n_comma_only_succeeds_selecting_nothing() {
        // strtok がトークンを 1 つも返さないので成功扱いになる
        let o = parse(&["-n", ",", "-f", "sa01"]);
        assert_eq!(selected(&o), vec![Activity::Cpu], "既定の A_CPU だけ");
    }

    #[test]
    fn n_without_argument_is_an_error() {
        assert_eq!(
            parse_err(&["-n"]),
            SarArgError::MissingArgument { opt: "-n" }
        );
    }

    #[test]
    fn m_keywords_and_all() {
        let o = parse(&["-m", "FAN,IN,TEMP", "-f", "sa01"]);
        assert_eq!(
            selected(&o),
            vec![Activity::PwrFan, Activity::PwrTemp, Activity::PwrIn]
        );

        let o = parse(&["-m", "ALL", "-f", "sa01"]);
        assert_eq!(o.activities.len(), 7);
        for act in Activity::PWR_ALL {
            assert!(o.is_selected(act));
        }
    }

    #[test]
    fn q_without_keyword_selects_queue() {
        let o = parse(&["-q", "-f", "sa01"]);
        assert_eq!(selected(&o), vec![Activity::Queue]);
    }

    #[test]
    fn q_psi_excludes_load() {
        let o = parse(&["-q", "PSI", "-f", "sa01"]);
        assert_eq!(
            selected(&o),
            vec![Activity::PsiCpu, Activity::PsiIo, Activity::PsiMem]
        );
        assert!(!o.is_selected(Activity::Queue));
    }

    #[test]
    fn q_all_includes_load_and_psi() {
        let o = parse(&["-q", "ALL", "-f", "sa01"]);
        assert_eq!(
            selected(&o),
            vec![
                Activity::Queue,
                Activity::PsiCpu,
                Activity::PsiIo,
                Activity::PsiMem
            ]
        );
    }

    #[test]
    fn q_invalid_keyword_does_not_usage_and_reparses_token() {
        // -q 2,5 → A_QUEUE 選択 + interval=2、5 は strtok に切られて失われる
        let o = parse(&["-q", "2,5"]);
        assert!(o.is_selected(Activity::Queue));
        assert_eq!(o.interval, Some(2));
        assert_eq!(o.count, None);
    }

    #[test]
    fn bundled_q_takes_no_keyword() {
        // -qu は 1 文字ループなのでキーワード解析をしない
        let o = parse(&["-qu", "LOAD"]);
        assert!(o.is_selected(Activity::Queue));
        assert!(o.is_selected(Activity::Cpu));
        // "LOAD" は positional interval = atol("LOAD") = 0 に吸われる
        assert_eq!(o.interval, Some(0));
    }

    // ---------------------------------------------------------------
    // 束ね / 束ね不可
    // ---------------------------------------------------------------

    #[test]
    fn bundled_activity_flags() {
        // expected.sar-A の実コマンド由来 (ライブ採取形)
        let o = parse(&[
            "-BbdFHSvWwy",
            "-I",
            "ALL",
            "-m",
            "CPU,FREQ,USB",
            "-n",
            "ALL",
            "-q",
            "ALL",
            "-r",
            "ALL",
            "-u",
            "ALL",
            "1",
            "2",
        ]);
        for act in [
            Activity::Page,
            Activity::Io,
            Activity::Disk,
            Activity::Fs,
            Activity::Huge,
            Activity::Memory,
            Activity::Ktables,
            Activity::Swap,
            Activity::Pcsw,
            Activity::Serial,
            Activity::Irq,
            Activity::PwrCpu,
            Activity::PwrFreq,
            Activity::PwrUsb,
            Activity::Queue,
            Activity::PsiCpu,
            Activity::Cpu,
        ] {
            assert!(o.is_selected(act), "{} が選択されていない", act.name());
        }
        assert_eq!(o.opt_flags(Activity::Cpu), OptFlags::CPU_ALL);
        let mem = o.opt_flags(Activity::Memory);
        assert!(mem.contains(OptFlags::MEMORY));
        assert!(mem.contains(OptFlags::SWAP));
        assert!(mem.contains(OptFlags::MEM_ALL));
        assert_eq!(o.opt_flags(Activity::Fs), OptFlags::FILESYSTEM);
        assert_eq!(o.interval, Some(1));
        assert_eq!(o.count, Some(2));
    }

    #[test]
    fn main_loop_options_cannot_be_bundled() {
        // -uP 0 は parse_sar_opt に落ちて 'P' が未知文字
        assert_eq!(
            parse_err(&["-uP", "0", "-f", "sa01"]),
            SarArgError::UnknownShortOption {
                token: "-uP".to_string(),
                ch: 'P'
            }
        );
    }

    #[test]
    fn unknown_short_option_is_reported() {
        assert_eq!(
            parse_err(&["-Q", "-f", "sa01"]),
            SarArgError::UnknownShortOption {
                token: "-Q".to_string(),
                ch: 'Q'
            }
        );
    }

    #[test]
    fn a_selects_all_43_activities() {
        let o = parse(&["-A", "-f", "sa01"]);
        assert_eq!(o.activities.len(), 43);
        assert!(o.flags.option_a);
    }

    // ---------------------------------------------------------------
    // --dec=
    // ---------------------------------------------------------------

    #[test]
    fn dec_places() {
        let o = parse(&["--dec=0", "-A", "-f", "sa01"]);
        assert_eq!(o.dec_places, Some(0));
        let o = parse(&["--dec=2", "-A", "-f", "sa01"]);
        assert_eq!(o.dec_places, Some(2));
        let o = parse(&["-A", "-f", "sa01"]);
        assert_eq!(o.dec_places, None, "既定は 2 桁相当 (dplaces_nr = -1)");
    }

    #[test]
    fn dec_with_invalid_digit_is_an_error() {
        assert!(matches!(
            parse_err(&["--dec=9", "-f", "sa01"]),
            SarArgError::InvalidDecPlaces { .. }
        ));
    }

    #[test]
    fn dec_with_two_digits_falls_through_to_short_option_loop() {
        // 長さが 7 でないと --dec= の分岐に入らない。
        // C 側は 1 文字ループで先頭の '-' が default: に落ちて usage になる。
        assert_eq!(
            parse_err(&["--dec=12", "-f", "sa01"]),
            SarArgError::UnknownShortOption {
                token: "--dec=12".to_string(),
                ch: '-'
            }
        );
    }

    // ---------------------------------------------------------------
    // -s / -e
    // ---------------------------------------------------------------

    #[test]
    fn timestamp_hhmmss_and_hhmm() {
        let o = parse(&["-f", "sa01", "-s", "13:20:20", "-e", "13:20:40"]);
        assert_eq!(
            o.tm_start,
            TimeSpec::HhMmSs {
                hour: 13,
                min: 20,
                sec: 20
            }
        );
        assert_eq!(
            o.tm_end,
            TimeSpec::HhMmSs {
                hour: 13,
                min: 20,
                sec: 40
            }
        );

        let o = parse(&["-f", "sa01", "-s", "13:20"]);
        assert_eq!(
            o.tm_start,
            TimeSpec::HhMmSs {
                hour: 13,
                min: 20,
                sec: 0
            },
            "hh:mm は :00 を補完する"
        );
    }

    #[test]
    fn timestamp_without_value_uses_default() {
        let o = parse(&["-f", "sa01", "-s"]);
        assert_eq!(
            o.tm_start,
            TimeSpec::HhMmSs {
                hour: 8,
                min: 0,
                sec: 0
            }
        );

        let o = parse(&["-f", "sa01", "-e"]);
        assert_eq!(
            o.tm_end,
            TimeSpec::HhMmSs {
                hour: 18,
                min: 0,
                sec: 0
            }
        );
    }

    #[test]
    fn timestamp_shaped_value_is_not_consumed() {
        // `sar -f sa01 -s 5` は tm_start=08:00:00 + interval=5
        let o = parse(&["-f", "sa01", "-s", "5"]);
        assert_eq!(
            o.tm_start,
            TimeSpec::HhMmSs {
                hour: 8,
                min: 0,
                sec: 0
            }
        );
        assert_eq!(o.interval, Some(5));
    }

    #[test]
    fn timestamp_option_following_s_is_reparsed() {
        // -s の次が '-' 始まりなら値として消費しない
        let o = parse(&["-s", "-u", "-f", "sa01"]);
        assert!(o.is_selected(Activity::Cpu));
        assert_eq!(
            o.tm_start,
            TimeSpec::HhMmSs {
                hour: 8,
                min: 0,
                sec: 0
            }
        );
    }

    #[test]
    fn timestamp_wrong_separator_falls_back_to_default() {
        // 長さ 5 だが [2] != ':' → 既定値 + "1:2:3" は positional (atol = 1)
        let o = parse(&["-f", "sa01", "-s", "1:2:3"]);
        assert_eq!(
            o.tm_start,
            TimeSpec::HhMmSs {
                hour: 8,
                min: 0,
                sec: 0
            }
        );
        assert_eq!(o.interval, Some(1));
    }

    #[test]
    fn timestamp_non_digit_fields_are_errors() {
        assert!(matches!(
            parse_err(&["-f", "sa01", "-s", "fo:ob:ar"]),
            SarArgError::InvalidTimestamp { opt: "-s", .. }
        ));
        assert!(matches!(
            parse_err(&["-f", "sa01", "-s", "fo:ob"]),
            SarArgError::InvalidTimestamp { opt: "-s", .. }
        ));
    }

    #[test]
    fn timestamp_out_of_range_is_an_error() {
        assert!(matches!(
            parse_err(&["-f", "sa01", "-s", "13:20:60"]),
            SarArgError::InvalidTimestamp { .. }
        ));
        assert!(matches!(
            parse_err(&["-f", "sa01", "-s", "24:00:00"]),
            SarArgError::InvalidTimestamp { .. }
        ));
    }

    #[test]
    fn epoch_timestamps() {
        let o = parse(&["-f", "sa01", "-s", "1555593629"]);
        assert_eq!(o.tm_start, TimeSpec::Epoch(1_555_593_629));

        // 10 桁でなければ epoch と見なさない
        let o = parse(&["-f", "sa01", "-s", "155559362"]);
        assert_eq!(
            o.tm_start,
            TimeSpec::HhMmSs {
                hour: 8,
                min: 0,
                sec: 0
            }
        );
        assert_eq!(o.interval, Some(155_559_362));
    }

    #[test]
    fn epoch_zero_is_an_error() {
        assert!(matches!(
            parse_err(&["-f", "sa01", "-s", "0000000000"]),
            SarArgError::InvalidTimestamp { .. }
        ));
    }

    #[test]
    fn hhmmss_end_before_start_wraps_to_next_day() {
        let o = parse(&["-f", "sa01", "-s", "18:00:00", "-e", "13:30:00"]);
        assert_eq!(
            o.tm_end,
            TimeSpec::HhMmSs {
                hour: 37,
                min: 30,
                sec: 0
            }
        );
        assert!(o.tm_end.wraps_to_next_day());
    }

    #[test]
    fn same_hour_is_not_wrapped() {
        let o = parse(&["-f", "sa01", "-s", "13:30:00", "-e", "13:10:00"]);
        assert_eq!(
            o.tm_end,
            TimeSpec::HhMmSs {
                hour: 13,
                min: 10,
                sec: 0
            }
        );
        assert!(!o.tm_end.wraps_to_next_day());
    }

    #[test]
    fn epoch_end_before_start_is_an_error() {
        assert_eq!(
            parse_err(&["-f", "sa01", "-s", "1555595349", "-e", "1555593629"]),
            SarArgError::EndBeforeStart
        );
    }

    #[test]
    fn mixed_epoch_and_hhmmss_is_allowed() {
        let o = parse(&["-f", "sa01", "-s", "13:20:19", "-e", "1555595649"]);
        assert_eq!(
            o.tm_start,
            TimeSpec::HhMmSs {
                hour: 13,
                min: 20,
                sec: 19
            }
        );
        assert_eq!(o.tm_end, TimeSpec::Epoch(1_555_595_649));
    }

    #[test]
    fn s_without_file_falls_back_to_default_daily_file() {
        // 既定ファイルへのフォールバックが先に走るのでエラーにならない
        let o = parse(&["-s", "09:00:00"]);
        assert_eq!(o.input, Some(SarInput::DefaultDaily));
    }

    #[test]
    fn s_with_positional_interval_and_no_file_is_an_error() {
        assert_eq!(
            parse_err(&["-s", "09:00:00", "5"]),
            SarArgError::NotReadingFromFile
        );
    }

    // ---------------------------------------------------------------
    // -f / -o / -i / -[0-9]+
    // ---------------------------------------------------------------

    #[test]
    fn f_without_filename_uses_default_daily_file() {
        let o = parse(&["-f"]);
        assert_eq!(o.input, Some(SarInput::DefaultDaily));
        assert!(o.default_file_used);
    }

    #[test]
    fn f_with_all_digit_argument_does_not_consume_it() {
        // sar -f 20240101 → 既定ファイル + positional interval
        let o = parse(&["-f", "20240101"]);
        assert_eq!(o.input, Some(SarInput::DefaultDaily));
        assert_eq!(o.interval, Some(20_240_101));
    }

    #[test]
    fn f_and_o_are_mutually_exclusive() {
        assert_eq!(
            parse_err(&["-f", "sa01", "-o", "out"]),
            SarArgError::MutuallyExclusiveFromTo
        );
    }

    #[test]
    fn o_alone_means_default_daily_file() {
        let o = parse(&["-o"]);
        assert_eq!(o.output, Some(SarOutput::DefaultDaily));
        assert_eq!(o.input, None);
    }

    #[test]
    fn f_and_day_offset_are_mutually_exclusive() {
        assert!(matches!(
            parse_err(&["-f", "sa01", "-3"]),
            SarArgError::ConflictingInput { .. }
        ));
        assert!(matches!(
            parse_err(&["-3", "-f", "sa01"]),
            SarArgError::ConflictingInput { opt: "-f" }
        ));
    }

    #[test]
    fn day_offset() {
        let o = parse(&["-1"]);
        assert_eq!(o.day_offset, 1);
        let o = parse(&["-99999"]);
        assert_eq!(o.day_offset, 99999);
    }

    #[test]
    fn day_offset_longer_than_6_chars_is_an_unknown_option() {
        assert_eq!(
            parse_err(&["-123456"]),
            SarArgError::UnknownShortOption {
                token: "-123456".to_string(),
                ch: '1'
            }
        );
    }

    #[test]
    fn o_with_day_offset_is_an_error() {
        assert!(matches!(
            parse_err(&["-o", "out", "-3"]),
            SarArgError::ConflictingInput { .. }
        ));
    }

    #[test]
    fn record_interval() {
        // expected.sar-ix の実コマンド由来
        let o = parse(&["-i", "60", "-x", "-uw", "-P", "ALL", "-f", "sa01"]);
        assert_eq!(o.interval, Some(60));
        assert!(o.flags.interval_set);
        assert!(o.flags.minmax);
        assert!(o.is_selected(Activity::Cpu));
        assert!(o.is_selected(Activity::Pcsw));
    }

    #[test]
    fn record_interval_below_one_is_an_error() {
        assert!(matches!(
            parse_err(&["-i", "0", "-f", "sa01"]),
            SarArgError::InvalidRecordInterval { .. }
        ));
        assert!(matches!(
            parse_err(&["-i", "x", "-f", "sa01"]),
            SarArgError::InvalidRecordInterval { .. }
        ));
        assert!(matches!(
            parse_err(&["-i"]),
            SarArgError::InvalidRecordInterval { .. }
        ));
    }

    #[test]
    fn record_interval_without_file_is_an_error() {
        // -i 指定時は既定ファイルへフォールバックするのでエラーにならない
        let o = parse(&["-i", "60"]);
        assert_eq!(o.input, Some(SarInput::DefaultDaily));
        // -o を付けるとフォールバックしないので -i がファイル読み出しでなくなる
        assert_eq!(
            parse_err(&["-i", "60", "-o", "out"]),
            SarArgError::NotReadingFromFile
        );
    }

    // ---------------------------------------------------------------
    // interval / count
    // ---------------------------------------------------------------

    #[test]
    fn interval_and_count() {
        let o = parse(&["-u", "1", "2"]);
        assert_eq!(o.interval, Some(1));
        assert_eq!(o.count, Some(2));
    }

    #[test]
    fn count_below_one_is_an_error() {
        assert!(matches!(
            parse_err(&["-u", "1", "0"]),
            SarArgError::InvalidIntervalCount { .. }
        ));
    }

    #[test]
    fn count_with_zero_interval_is_an_error() {
        assert!(matches!(
            parse_err(&["-u", "0", "2"]),
            SarArgError::InvalidIntervalCount { .. }
        ));
    }

    #[test]
    fn interval_zero_alone_is_since_boot() {
        let o = parse(&["-u", "0"]);
        assert_eq!(o.interval, Some(0));
        assert_eq!(o.input, None, "ライブ採取なのでファイルを読まない");
    }

    // ---------------------------------------------------------------
    // -j / -t / -x / -z / -C / -D
    // ---------------------------------------------------------------

    #[test]
    fn j_sid_is_case_sensitive_and_implies_pretty() {
        let o = parse(&["-d", "-j", "SID", "-f", "sa01"]);
        assert_eq!(o.persistent_name, Some(PersistentName::Sid));
        assert!(o.flags.dev_sid);
        assert!(o.flags.pretty);
        assert!(!o.flags.persist_name);
    }

    #[test]
    fn j_type_is_lowercased() {
        let o = parse(&["-d", "-j", "UUID", "-f", "sa01"]);
        assert_eq!(
            o.persistent_name,
            Some(PersistentName::ByType("uuid".to_string()))
        );
        assert!(o.flags.persist_name);
        assert!(o.flags.pretty);
        assert!(!o.flags.dev_sid);

        let lower = parse(&["-d", "-j", "uuid", "-f", "sa01"]);
        assert_eq!(lower.persistent_name, o.persistent_name);
    }

    #[test]
    fn j_without_argument_is_an_error() {
        assert_eq!(
            parse_err(&["-d", "-j"]),
            SarArgError::MissingArgument { opt: "-j" }
        );
    }

    #[test]
    fn j_consumes_next_arg_and_stops_token_processing() {
        // -jd は 'j' が次引数を消費して return するので 'd' は処理されない
        let o = parse(&["-jd", "ID", "-f", "sa01"]);
        assert_eq!(
            o.persistent_name,
            Some(PersistentName::ByType("id".to_string()))
        );
        assert!(!o.is_selected(Activity::Disk));
    }

    #[test]
    fn j_too_long_is_an_error() {
        let long = "a".repeat(MAX_FILE_LEN);
        let err = parse_sar_args(&argv(&["-d", "-j", &long, "-f", "sa01"])).unwrap_err();
        assert!(matches!(err, SarArgError::PersistentNameTooLong { .. }));
    }

    #[test]
    fn t_is_accepted_by_sar() {
        let o = parse(&["-t", "-f", "sa01"]);
        assert!(o.flags.true_time);
    }

    #[test]
    fn z_and_c_and_d_flags() {
        // expected.sar-z の実コマンド由来
        let o = parse(&["-f", "sa01", "-e", "13:30", "-z", "-n", "DEV", "-dp"]);
        assert!(o.flags.zero_omit);
        assert!(o.flags.pretty);
        assert!(o.is_selected(Activity::NetDev));
        assert!(o.is_selected(Activity::Disk));
        assert_eq!(
            o.tm_end,
            TimeSpec::HhMmSs {
                hour: 13,
                min: 30,
                sec: 0
            }
        );

        let o = parse(&["-C", "-A", "-f", "sa01"]);
        assert!(o.flags.comment);

        let o = parse(&["-D", "-o", "out"]);
        assert!(o.flags.sa_yyyymmdd);
    }

    #[test]
    fn expected2_sar_x2_command_line() {
        // sar -xzh -n DEV -d -u -P ALL -q ALL -f sa01
        let o = parse(&[
            "-xzh", "-n", "DEV", "-d", "-u", "-P", "ALL", "-q", "ALL", "-f", "sa01",
        ]);
        assert!(o.flags.minmax);
        assert!(o.flags.zero_omit);
        assert!(o.flags.pretty);
        assert!(o.flags.human);
        assert!(o.is_selected(Activity::NetDev));
        assert!(o.is_selected(Activity::Disk));
        assert_eq!(o.opt_flags(Activity::Cpu), OptFlags::CPU_DEF);
        assert!(o.is_selected(Activity::Queue));
        assert!(o.is_selected(Activity::PsiMem));
        assert!(o.cpu_bitmap.count_bits() > 1000);
    }

    #[test]
    fn m_freq_with_p_all() {
        // expected.sar-m-freq の実コマンド由来
        let o = parse(&["-f", "sa01", "-m", "FREQ", "-P", "ALL"]);
        assert_eq!(selected(&o), vec![Activity::PwrFreq]);
        assert!(o.cpu_bitmap.is_set(1));
    }

    // ---------------------------------------------------------------
    // 即時アクション
    // ---------------------------------------------------------------

    #[test]
    fn immediate_actions_stop_parsing() {
        let o = parse(&["--help", "-Q"]);
        assert_eq!(o.immediate, Some(SarImmediate::Help));

        let o = parse(&["-V", "-Q"]);
        assert_eq!(o.immediate, Some(SarImmediate::Version));

        let o = parse(&["--sadc", "-Q"]);
        assert_eq!(o.immediate, Some(SarImmediate::Sadc));
    }

    #[test]
    fn unknown_long_option() {
        assert!(matches!(
            parse_err(&["--utc", "-f", "sa01"]),
            SarArgError::UnknownShortOption { ch: '-', .. }
        ));
    }

    // ---------------------------------------------------------------
    // 内部ヘルパ
    // ---------------------------------------------------------------

    #[test]
    fn empty_string_counts_as_all_digits() {
        assert!(is_all_digits(""), "strspn(\"\") == 0 == strlen(\"\")");
        assert!(!takes_filename(Some(&String::new())));
    }

    #[test]
    fn atol_mimics_c() {
        assert_eq!(atol("5"), 5);
        assert_eq!(atol("5x"), 5);
        assert_eq!(atol("sum"), 0);
        assert_eq!(atol(""), 0);
        assert_eq!(atol("1:2:3"), 1);
    }

    #[test]
    fn parse_range_values_rules() {
        assert_eq!(parse_range_values("3", 100), Some((3, 3)));
        assert_eq!(parse_range_values("3-5", 100), Some((3, 5)));
        assert_eq!(parse_range_values("3-", 100), Some((3, 99)));
        assert_eq!(parse_range_values("5-3", 100), None);
        assert_eq!(parse_range_values("-5", 100), None);
        assert_eq!(parse_range_values("-", 100), None);
        assert_eq!(parse_range_values("", 100), None);
        assert_eq!(parse_range_values("100", 100), None, "max_val 以上は不可");
        assert_eq!(parse_range_values("3-5-7", 100), None);
    }

    #[test]
    fn activity_ids_are_stable() {
        assert_eq!(Activity::Cpu.id(), 1);
        assert_eq!(Activity::Irq.id(), 3);
        assert_eq!(Activity::Huge.id(), 34);
        assert_eq!(Activity::Fs.id(), 37);
        assert_eq!(Activity::PwrBat.id(), 43);
        assert_eq!(Activity::ALL.len(), 43);
        assert_eq!(Activity::Cpu.name(), "A_CPU");
    }

    #[test]
    fn opt_flags_bits_match_sysstat() {
        assert_eq!(OptFlags::MEMORY.bits(), 0x0001);
        assert_eq!(OptFlags::SWAP.bits(), 0x0002);
        assert_eq!(OptFlags::MEM_ALL.bits(), 0x0100);
        assert_eq!(OptFlags::CPU_DEF.bits(), 0x0001);
        assert_eq!(OptFlags::CPU_ALL.bits(), 0x0002);
        assert!(!OptFlags::NONE.contains(OptFlags::NONE));
    }
}

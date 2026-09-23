//! `--sar-profile sysstat-10.1.5-el7` (RHEL / CentOS 7 の sysstat 10.1.5 の `sar`) の回帰テスト。
//!
//! fixture は el7 の `rd_stats.h` の構造体定義と `docs/format/01-file-format.md` の
//! 0x2171 のオフセット表から**独立に書き起こした**最小のライタで作る
//! (本体のレイアウト表は使わない。同じ誤りが往復して通るのを避けるため)。
//! 期待値は各テストのコメントに書いた式から手計算したもので、本家の出力を
//! 写したものではない。値の組は el7 の癖が出るように選んである。
//!
//! # 本家との突き合わせ
//!
//! 環境変数 `RESARCH_EL7_ORACLE` に本家 el7 の `sar` の実行ファイルを与えると、
//! 各テストが同じ fixture・同じ引数・`LC_ALL=C`・同じ `TZ` で本家も実行し、
//! stdout と終了コードが一致することまで確かめる。本家は GPL なので、ソースも
//! 出力もリポジトリには入れず、ローカルでビルドしたものを使う
//! (`docs/format/05-sysstat-10.1.5-el7.md` §6.2)。
//! `RESARCH_EL7_FIXTURE_DIR` を与えると fixture をそのディレクトリへ残す。

mod fixtures;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

// ===========================================================================
// 0x2171 (LP64 little endian) の最小ライタ
// ===========================================================================

const R_STATS: u8 = 1;
const R_RESTART: u8 = 2;
const R_COMMENT: u8 = 4;

/// 1 activity の宣言 (`file_activity`)。
#[derive(Debug, Clone, Copy)]
struct Act {
    id: u32,
    magic: u32,
    nr: i32,
    nr2: i32,
    size: i32,
}

impl Act {
    /// item 数だけ変えた宣言。
    const fn nr(self, nr: i32) -> Self {
        Act { nr, ..self }
    }

    /// sub-item 数 (`A_PWR_WGHFREQ` の周波数の数) だけ変えた宣言。
    const fn nr2(self, nr2: i32) -> Self {
        Act { nr2, ..self }
    }

    /// magic だけ変えた宣言 (本家が「未知の形式」として読み飛ばす activity を作る)。
    const fn magic(self, magic: u32) -> Self {
        Act { magic, ..self }
    }
}

/// el7 の `act[]` と同じ宣言。`nr` / `nr2` は 1。
///
/// `magic` は `ACTIVITY_MAGIC_BASE` (0x8a) か、その +1 (0x8b)。
/// `size` は el7 の `rd_stats.h` / `rd_sensors.h` の構造体の LP64 での `sizeof`。
const fn decl(id: u32, magic: u32, size: i32) -> Act {
    Act {
        id,
        magic,
        nr: 1,
        nr2: 1,
        size,
    }
}

/// CPU "all" + 2 個。
const A_CPU: Act = decl(1, 0x8a, 160).nr(3);
const A_PCSW: Act = decl(2, 0x8a, 32);
const A_IRQ: Act = decl(3, 0x8a, 16);
const A_SWAP: Act = decl(4, 0x8a, 16);
const A_PAGE: Act = decl(5, 0x8a, 64);
const A_IO: Act = decl(6, 0x8b, 48);
const A_MEMORY: Act = decl(7, 0x8a, 88);
const A_KTABLES: Act = decl(8, 0x8a, 16);
const A_QUEUE: Act = decl(9, 0x8b, 32);
const A_SERIAL: Act = decl(10, 0x8a, 28);
const A_DISK: Act = decl(11, 0x8b, 64);
const A_NET_DEV: Act = decl(12, 0x8b, 128);
const A_NET_EDEV: Act = decl(13, 0x8b, 160);
const A_NET_NFS: Act = decl(14, 0x8a, 24);
const A_NET_NFSD: Act = decl(15, 0x8a, 44);
const A_NET_SOCK: Act = decl(16, 0x8a, 24);
const A_NET_IP: Act = decl(17, 0x8b, 128);
const A_NET_EIP: Act = decl(18, 0x8b, 128);
const A_NET_ICMP: Act = decl(19, 0x8a, 112);
const A_NET_EICMP: Act = decl(20, 0x8a, 96);
const A_NET_TCP: Act = decl(21, 0x8a, 32);
const A_NET_ETCP: Act = decl(22, 0x8a, 40);
const A_NET_UDP: Act = decl(23, 0x8a, 32);
const A_NET_SOCK6: Act = decl(24, 0x8a, 16);
const A_NET_IP6: Act = decl(25, 0x8b, 160);
const A_NET_EIP6: Act = decl(26, 0x8b, 176);
const A_NET_ICMP6: Act = decl(27, 0x8a, 136);
const A_NET_EICMP6: Act = decl(28, 0x8a, 88);
const A_NET_UDP6: Act = decl(29, 0x8a, 32);
const A_PWR_CPUFREQ: Act = decl(30, 0x8a, 8);
const A_PWR_FAN: Act = decl(31, 0x8a, 40);
const A_PWR_TEMP: Act = decl(32, 0x8a, 48);
const A_PWR_IN: Act = decl(33, 0x8a, 48);
/// `STATS_HUGE_SIZE` は本家の不具合で `sizeof(struct stats_memory)` (88) になっている。
/// 構造体そのものは 16 バイトで、残りは読み飛ばされる。
const A_HUGE: Act = decl(34, 0x8a, 88);
const A_PWR_WGHFREQ: Act = decl(35, 0x8a, 32);
const A_PWR_USB: Act = decl(36, 0x8a, 88);
const A_FILESYSTEM: Act = decl(37, 0x8a, 336);

/// 1 レコード。
enum Rec {
    /// `uptime` (全 CPU の jiffies)、`uptime0` (CPU 1 個分)、`ust_time` の日 (`DAY0` からの日数)、
    /// 時分秒 (`-t` で出る値)、activity ごとの生バイト列。
    Stats {
        uptime: u64,
        uptime0: u64,
        day: u64,
        hms: (u8, u8, u8),
        payload: Vec<u8>,
    },
    Comment {
        hms: (u8, u8, u8),
        text: &'static str,
    },
    Restart {
        hms: (u8, u8, u8),
    },
}

fn put_u16(b: &mut [u8], off: usize, v: u16) {
    b[off..off + 2].copy_from_slice(&v.to_le_bytes());
}
fn put_u32(b: &mut [u8], off: usize, v: u32) {
    b[off..off + 4].copy_from_slice(&v.to_le_bytes());
}
fn put_u64(b: &mut [u8], off: usize, v: u64) {
    b[off..off + 8].copy_from_slice(&v.to_le_bytes());
}
fn put_str(b: &mut [u8], off: usize, s: &str) {
    b[off..off + s.len()].copy_from_slice(s.as_bytes());
}

/// `ust_time` の基準 (2020-09-13 00:00:00 UTC)。時分秒から epoch を作る。
const DAY0: u64 = 1_599_955_200;

fn epoch(day: u64, hms: (u8, u8, u8)) -> u64 {
    DAY0 + day * 86_400 + u64::from(hms.0) * 3600 + u64::from(hms.1) * 60 + u64::from(hms.2)
}

/// `record_header` (0x2171、48 バイト)。
fn record_header(
    b: &mut Vec<u8>,
    uptime: u64,
    uptime0: u64,
    rtype: u8,
    day: u64,
    hms: (u8, u8, u8),
) {
    let o = b.len();
    b.resize(o + 48, 0);
    put_u64(b, o, uptime); // uptime (aligned 16)
    put_u64(b, o + 16, uptime0); // uptime0 (aligned 16)
    put_u64(b, o + 32, epoch(day, hms)); // ust_time (unsigned long, aligned 16)
    b[o + 40] = rtype; // record_type
    b[o + 41] = hms.0;
    b[o + 42] = hms.1;
    b[o + 43] = hms.2;
}

/// 0x2171 のファイル全体。
fn build(acts: &[Act], recs: &[Rec]) -> Vec<u8> {
    let mut b = vec![0u8; 8 + 280];
    // file_magic (8 バイト): sysstat_magic / format_magic / 版 10.1.5
    put_u16(&mut b, 0, 0xd596);
    put_u16(&mut b, 2, 0x2171);
    b[4] = 10;
    b[5] = 1;
    b[6] = 5;
    b[7] = 0;
    // file_header (280 バイト)
    let fh = 8;
    put_u64(&mut b, fh, DAY0); // sa_ust_time
    put_u32(&mut b, fh + 8, acts.len() as u32); // sa_nr_act
    b[fh + 12] = 13; // sa_day
    b[fh + 13] = 8; // sa_month (0 起点 = 9 月)
    b[fh + 14] = 120; // sa_year (1900 起点)
    b[fh + 15] = 8; // sa_sizeof_long
    put_str(&mut b, fh + 16, "Linux");
    put_str(&mut b, fh + 81, "testhost");
    put_str(&mut b, fh + 146, "3.10.0-el7");
    put_str(&mut b, fh + 211, "x86_64");
    // file_activity (20 バイト × n)
    for a in acts {
        let o = b.len();
        b.resize(o + 20, 0);
        put_u32(&mut b, o, a.id);
        put_u32(&mut b, o + 4, a.magic);
        put_u32(&mut b, o + 8, a.nr as u32);
        put_u32(&mut b, o + 12, a.nr2 as u32);
        put_u32(&mut b, o + 16, a.size as u32);
    }
    for r in recs {
        match r {
            Rec::Stats {
                uptime,
                uptime0,
                day,
                hms,
                payload,
            } => {
                record_header(&mut b, *uptime, *uptime0, R_STATS, *day, *hms);
                b.extend_from_slice(payload);
            }
            Rec::Comment { hms, text } => {
                record_header(&mut b, 0, 0, R_COMMENT, 0, *hms);
                let mut c = [0u8; 64];
                c[..text.len()].copy_from_slice(text.as_bytes());
                b.extend_from_slice(&c);
            }
            Rec::Restart { hms } => record_header(&mut b, 0, 0, R_RESTART, 0, *hms),
        }
    }
    b
}

fn stats(uptime: u64, uptime0: u64, hms: (u8, u8, u8), parts: &[Vec<u8>]) -> Rec {
    stats_on(0, uptime, uptime0, hms, parts)
}

/// `DAY0` から `day` 日後の統計レコード。
fn stats_on(day: u64, uptime: u64, uptime0: u64, hms: (u8, u8, u8), parts: &[Vec<u8>]) -> Rec {
    Rec::Stats {
        uptime,
        uptime0,
        day,
        hms,
        payload: parts.concat(),
    }
}

/// 構造体 1 個分のバイト列を、オフセットを明示して組み立てる。
struct St(Vec<u8>);

impl St {
    fn new(size: usize) -> Self {
        St(vec![0; size])
    }
    /// `unsigned long` (LP64) / `unsigned long long`。
    fn u64(mut self, off: usize, v: u64) -> Self {
        put_u64(&mut self.0, off, v);
        self
    }
    /// `unsigned int`。
    fn u32(mut self, off: usize, v: u32) -> Self {
        put_u32(&mut self.0, off, v);
        self
    }
    /// `double`。
    fn f64(self, off: usize, v: f64) -> Self {
        self.u64(off, v.to_bits())
    }
    /// `char[]` (残りは NUL)。
    fn text(mut self, off: usize, s: &str) -> Self {
        put_str(&mut self.0, off, s);
        self
    }
    fn done(self) -> Vec<u8> {
        self.0
    }
}

/// `stats_cpu` (10 個の `unsigned long long`、各 `aligned(16)` → 160 バイト)。
///
/// 並びは user, nice, sys, idle, iowait, steal, hardirq, softirq, guest, guest_nice。
fn cpu(user: u64, sys: u64, idle: u64, iowait: u64) -> Vec<u8> {
    St::new(160)
        .u64(0, user)
        .u64(32, sys)
        .u64(48, idle)
        .u64(64, iowait)
        .done()
}

/// `stats_memory` (11 個の `unsigned long` → 88 バイト)。
///
/// 並びは frmkb, bufkb, camkb, tlmkb, frskb, tlskb, caskb, comkb, activekb, inactkb, dirtykb。
fn memory(v: [u64; 11]) -> Vec<u8> {
    counters(88, 8, &v)
}

/// 同じ型のカウンタを `stride` バイトおきに並べた構造体 (`size` バイト)。
///
/// `stride` が 4 なら `unsigned int` (値は下位 32 ビット)、8 なら `unsigned long` か
/// 詰めて並んだ `unsigned long long`、16 なら `aligned(16)` の `unsigned long long`。
fn counters(size: usize, stride: usize, values: &[u64]) -> Vec<u8> {
    let mut b = vec![0u8; size];
    for (i, v) in values.iter().enumerate() {
        if stride == 4 {
            put_u32(&mut b, i * 4, *v as u32);
        } else {
            put_u64(&mut b, i * stride, *v);
        }
    }
    b
}

/// `stats_pcsw` (32 バイト): `context_switch` (`unsigned long long`) @0、
/// `processes` (`unsigned long`, aligned 16) @16。
fn pcsw(context_switch: u64, processes: u64) -> Vec<u8> {
    St::new(32).u64(0, context_switch).u64(16, processes).done()
}

/// `stats_irq` (16 バイト): `irq_nr` (`unsigned long long`, aligned 16) @0。
fn irq(n: u64) -> Vec<u8> {
    St::new(16).u64(0, n).done()
}

/// `stats_ktables` (16 バイト): file_used, inode_used, dentry_stat, pty_nr (`unsigned int`)。
fn ktables(file_used: u32, inode_used: u32, dentry_stat: u32, pty_nr: u32) -> Vec<u8> {
    St::new(16)
        .u32(0, file_used)
        .u32(4, inode_used)
        .u32(8, dentry_stat)
        .u32(12, pty_nr)
        .done()
}

/// `stats_queue` (32 バイト): nr_running @0、procs_blocked @8 (`unsigned long`)、
/// load_avg_1 @16 (`unsigned int`, aligned 8)、load_avg_5 @20、load_avg_15 @24、nr_threads @28。
fn queue(nr_running: u64, procs_blocked: u64, load: [u32; 3], nr_threads: u32) -> Vec<u8> {
    St::new(32)
        .u64(0, nr_running)
        .u64(8, procs_blocked)
        .u32(16, load[0])
        .u32(20, load[1])
        .u32(24, load[2])
        .u32(28, nr_threads)
        .done()
}

/// `stats_net_sock` (24 バイト): sock_inuse, tcp_inuse, tcp_tw, udp_inuse, raw_inuse,
/// frag_inuse (`unsigned int`)。
fn sock(v: [u32; 6]) -> Vec<u8> {
    counters(24, 4, &v.map(u64::from))
}

/// `stats_net_sock6` (16 バイト): tcp6_inuse, udp6_inuse, raw6_inuse, frag6_inuse。
fn sock6(v: [u32; 4]) -> Vec<u8> {
    counters(16, 4, &v.map(u64::from))
}

/// `stats_huge` (frhkb @0、tlhkb @8) を、`STATS_HUGE_SIZE` の不具合どおり 88 バイトの枠に置く。
///
/// 枠の残り 72 バイトは読まれないことを確かめるため 0xff で埋める。
fn huge(frhkb: u64, tlhkb: u64) -> Vec<u8> {
    let mut b = St::new(88).u64(0, frhkb).u64(8, tlhkb).done();
    b[16..].fill(0xff);
    b
}

/// `stats_serial` (28 バイト): rx, tx, frame, parity, brk, overrun, line (`unsigned int`)。
///
/// `line` は回線番号 + 1 (0 は未使用の枠)。
fn serial(counts: [u32; 6], line: u32) -> Vec<u8> {
    let mut v: Vec<u64> = counts.iter().map(|&c| u64::from(c)).collect();
    v.push(u64::from(line));
    counters(28, 4, &v)
}

/// `stats_disk` (64 バイト): nr_ios (`unsigned long long`) @0、
/// rd_sect (`unsigned long`, aligned 16) @16、wr_sect (aligned 8) @24、
/// rd_ticks (`unsigned int`, aligned 8) @32、wr_ticks @36、tot_ticks @40、rq_ticks @44、
/// major @48、minor @52。
///
/// `ticks` は rd_ticks, wr_ticks, tot_ticks, rq_ticks の順。
fn disk(dev: (u32, u32), nr_ios: u64, sect: (u64, u64), ticks: [u32; 4]) -> Vec<u8> {
    St::new(64)
        .u64(0, nr_ios)
        .u64(16, sect.0)
        .u64(24, sect.1)
        .u32(32, ticks[0])
        .u32(36, ticks[1])
        .u32(40, ticks[2])
        .u32(44, ticks[3])
        .u32(48, dev.0)
        .u32(52, dev.1)
        .done()
}

/// `stats_net_dev` (128 バイト): 7 個の `unsigned long long` (各 aligned 16) と
/// interface[16] (aligned 16) @112。
///
/// 並びは rx_packets, tx_packets, rx_bytes, tx_bytes, rx_compressed, tx_compressed, multicast。
fn net_dev(name: &str, v: [u64; 7]) -> Vec<u8> {
    let mut b = counters(128, 16, &v);
    put_str(&mut b, 112, name);
    b
}

/// `stats_net_edev` (160 バイト): 9 個の `unsigned long long` (各 aligned 16) と
/// interface[16] @144。
///
/// 並びは collisions, rx_errors, tx_errors, rx_dropped, tx_dropped,
/// rx_fifo_errors, tx_fifo_errors, rx_frame_errors, tx_carrier_errors。
fn net_edev(name: &str, v: [u64; 9]) -> Vec<u8> {
    let mut b = counters(160, 16, &v);
    put_str(&mut b, 144, name);
    b
}

/// `stats_pwr_cpufreq` (8 バイト): cpufreq (`unsigned long`、MHz × 100)。
fn cpufreq(mhz_x100: u64) -> Vec<u8> {
    St::new(8).u64(0, mhz_x100).done()
}

/// `stats_pwr_fan` (40 バイト): rpm @0、rpm_min @8 (`double`)、device[20] @16。
fn fan(rpm: f64, rpm_min: f64, device: &str) -> Vec<u8> {
    St::new(40)
        .f64(0, rpm)
        .f64(8, rpm_min)
        .text(16, device)
        .done()
}

/// `stats_pwr_temp` / `stats_pwr_in` (48 バイト): 値 @0、最小 @8、最大 @16 (`double`)、
/// device[20] @24。
fn sensor(value: f64, min: f64, max: f64, device: &str) -> Vec<u8> {
    St::new(48)
        .f64(0, value)
        .f64(8, min)
        .f64(16, max)
        .text(24, device)
        .done()
}

/// `stats_pwr_wghfreq` (32 バイト): time_in_state (`unsigned long long`) @0、
/// freq (`unsigned long`, aligned 16、kHz) @16。
fn wghfreq(time_in_state: u64, freq_khz: u64) -> Vec<u8> {
    St::new(32).u64(0, time_in_state).u64(16, freq_khz).done()
}

/// `stats_pwr_usb` (88 バイト): bus_nr, vendor_id, product_id, bmaxpower (`unsigned int`)、
/// manufacturer[24] @16、product[48] @40。
fn usb(
    bus: u32,
    vendor: u32,
    product: u32,
    bmaxpower: u32,
    manufacturer: &str,
    name: &str,
) -> Vec<u8> {
    St::new(88)
        .u32(0, bus)
        .u32(4, vendor)
        .u32(8, product)
        .u32(12, bmaxpower)
        .text(16, manufacturer)
        .text(40, name)
        .done()
}

/// `stats_filesystem` (336 バイト): f_blocks, f_bfree, f_bavail, f_files, f_ffree
/// (`unsigned long long`, 各 aligned 16)、fs_name[128] @80、mountp[128] @208。
///
/// ブロック数はバイト単位 (本家の `sadc` が `f_frsize` を掛けて書く)。
fn filesystem(bytes: [u64; 3], files: u64, ffree: u64, name: &str, mount: &str) -> Vec<u8> {
    St::new(336)
        .u64(0, bytes[0])
        .u64(16, bytes[1])
        .u64(32, bytes[2])
        .u64(48, files)
        .u64(64, ffree)
        .text(80, name)
        .text(208, mount)
        .done()
}

// ===========================================================================
// 期待値の組み立て
// ===========================================================================

/// fixture 共通のバナー行 (`A_CPU` の `nr` = 3 なので `(2 CPU)`)。
const BANNER: &str = "Linux 3.10.0-el7 (testhost) \t09/13/20 \t_x86_64_\t(2 CPU)";

/// `-u` の列見出し。
const CPU_HEADER: &str = "     CPU     %user     %nice   %system   %iowait    %steal     %idle";

/// タイムスタンプ (`%-11s`) に `" %9s"` の列を並べた 1 行。
///
/// `" %9.2f"` / `" %9lu"` / `"    %6.2f"` / `"       %3d"` / `" %9s"` は、値が桁に
/// 収まる限りどれも「空白 1 個 + 9 桁の右寄せ」と同じ 10 文字になる。
fn line(ts: &str, cells: &[&str]) -> String {
    line_at(ts, "", cells)
}

/// タイムスタンプの直後に識別子 (`"     all"` など) を置いた 1 行。
fn line_at(ts: &str, label: &str, cells: &[&str]) -> String {
    let mut s = format!("{ts:<11}{label}");
    for c in cells {
        s.push_str(&format!(" {c:>9}"));
    }
    s
}

/// 列見出し (直前の空行を含む)。
fn header(ts: &str, text: &str) -> String {
    format!("\n{ts:<11}{text}")
}

/// バナー行に続けて各行を並べた、期待する出力全体。
fn report(lines: &[String]) -> String {
    let mut s = format!("{BANNER}\n");
    for l in lines {
        s.push_str(l);
        s.push('\n');
    }
    s
}

/// 全 CPU が idle の CPU "all" の行。
fn idle_all(ts: &str) -> String {
    line_at(
        ts,
        "     all",
        &["0.00", "0.00", "0.00", "0.00", "0.00", "100.00"],
    )
}

// ===========================================================================
// 実行
// ===========================================================================

fn command() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_resarch"));
    cmd.env("LC_ALL", "C")
        .env("TZ", "UTC")
        .env_remove("S_TIME_FORMAT")
        .env_remove("S_REPEAT_HEADER");
    cmd
}

/// 本家 el7 の `sar` (`RESARCH_EL7_ORACLE`)。与えられていなければ突き合わせない。
fn oracle() -> Option<PathBuf> {
    std::env::var_os("RESARCH_EL7_ORACLE").map(PathBuf::from)
}

/// el7 プロファイルで `<args> -f <file>` を実行する。
///
/// 本家が与えられていれば、同じ引数・`LC_ALL=C`・`TZ` で本家も実行し、
/// stdout と終了コードが一致することを確かめる (stderr の文言は比べない)。
fn exec(args: &[&str], file: &Path, tz: &str) -> Output {
    let out = command()
        .env("TZ", tz)
        .args(["--sar-profile", "sysstat-10.1.5-el7"])
        .args(args)
        .arg("-f")
        .arg(file)
        .output()
        .unwrap();
    if let Some(sar) = oracle() {
        let theirs = Command::new(sar)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LC_ALL", "C")
            .env("TZ", tz)
            .args(args)
            .arg("-f")
            .arg(file)
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&theirs.stdout),
            "本家と stdout が違う: {args:?} {}",
            file.display()
        );
        assert_eq!(
            out.status.code(),
            theirs.status.code(),
            "本家と終了コードが違う: {args:?} {}",
            file.display()
        );
    }
    out
}

fn run(args: &[&str], file: &Path) -> String {
    run_in(args, file, "UTC")
}

/// `TZ` を指定して実行する (`-t` を付けない表示はローカル時刻になる)。
fn run_in(args: &[&str], file: &Path, tz: &str) -> String {
    let out = exec(args, file, tz);
    assert!(out.status.success(), "{args:?}: {out:?}");
    String::from_utf8(out.stdout).unwrap()
}

fn write(dir: &Path, name: &str, bytes: Vec<u8>) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    // `RESARCH_EL7_FIXTURE_DIR` を与えると fixture を残す (本家の el7 sar と
    // ローカルで突き合わせるため。リポジトリには本家の出力を入れない)
    if let Some(keep) = std::env::var_os("RESARCH_EL7_FIXTURE_DIR") {
        std::fs::copy(&path, Path::new(&keep).join(name)).unwrap();
    }
    path
}

/// k × 10 分の時分秒。
fn at(k: u64) -> (u8, u8, u8) {
    let m = k * 10;
    ((m / 60) as u8, (m % 60) as u8, 0)
}

/// k 番目のサンプル (時刻は 00:00:00 から 10 分刻み)。CPU はすべて idle。
///
/// `uptime0` の差 (CPU 数 > 2 なので `itv` はこちら) は 60000 jiffies (600 秒) なので、
/// カウンタが 600 増えると `S_VALUE` は 1.00 になる。平均行の `itv` は区間の最初
/// (k = 0) からの差で、k = 2 までなら 120000。
fn sample(k: u64, parts: &[Vec<u8>]) -> Rec {
    sample_every(k, 60_000, parts)
}

/// `uptime0` の刻みを `step` jiffies にしたサンプル。
fn sample_every(k: u64, step: u64, parts: &[Vec<u8>]) -> Rec {
    let uptime0 = 100_000 + k * step;
    let uptime = 2 * uptime0;
    let mut all = vec![
        cpu(0, 0, uptime, 0),
        cpu(0, 0, uptime0, 0),
        cpu(0, 0, uptime0, 0),
    ];
    all.extend(parts.iter().cloned());
    stats(uptime, uptime0, at(k), &all)
}

/// `A_CPU` と `acts` を持ち、k = 0..n のサンプルを `parts(k)` で埋めたファイル。
fn samples(acts: &[Act], n: u64, parts: impl Fn(u64) -> Vec<Vec<u8>>) -> Vec<u8> {
    let mut all = vec![A_CPU];
    all.extend_from_slice(acts);
    let recs: Vec<Rec> = (0..n).map(|k| sample(k, &parts(k))).collect();
    build(&all, &recs)
}

// ===========================================================================
// CPU
// ===========================================================================

/// 2 CPU の 3 サンプル。
///
/// - R1 で CPU 1 がオフライン (全フィールド 0)
/// - R2 で CPU 0 が tickless (R1 から全く増えない)、CPU 1 が復帰
/// - CPU "all" の差分 (user 6000 / sys 1200 / iowait 600 / idle 112200、計 120000)
///   は個別 CPU の合計と一致させていない。el7 はファイルの集約スロットを
///   `record_header.uptime` の差 (g_itv = 120000) で割るので、個別 CPU から
///   作り直す現行版とは値が変わる。
fn cpu_fixture() -> Vec<u8> {
    build(
        &[A_CPU],
        &[
            stats(
                200_000,
                100_000,
                (0, 0, 0),
                &[
                    cpu(1000, 500, 198_000, 500),
                    cpu(600, 300, 98_800, 300),
                    cpu(400, 200, 99_200, 200),
                ],
            ),
            stats(
                320_000,
                160_000,
                (0, 10, 0),
                &[
                    cpu(7000, 1700, 310_200, 1100),
                    cpu(3600, 900, 154_900, 600),
                    vec![0u8; 160],
                ],
            ),
            stats(
                440_000,
                220_000,
                (0, 20, 0),
                &[
                    cpu(13_000, 2900, 422_400, 1700),
                    cpu(3600, 900, 154_900, 600),
                    cpu(6400, 1400, 211_200, 1000),
                ],
            ),
        ],
    )
}

#[test]
fn cpu_rows_follow_the_el7_rules() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "el7-cpu", cpu_fixture());
    let out = run(&["-u", "-P", "ALL", "-t"], &file);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines[0],
        "Linux 3.10.0-el7 (testhost) \t09/13/20 \t_x86_64_\t(2 CPU)"
    );
    assert_eq!(lines[1], "");
    assert_eq!(
        lines[2],
        "00:00:00        CPU     %user     %nice   %system   %iowait    %steal     %idle"
    );
    let expected = [
        // all: 6000/120000 = 5.00、(sys+hardirq+softirq) 1200 → 1.00、iowait 0.50、idle 93.50
        "00:10:00        all      5.00      0.00      1.00      0.50      0.00     93.50",
        // CPU 0: 自分の tick 差 (60000) で割る
        "00:10:00          0      5.00      0.00      1.00      0.50      0.00     93.50",
        // オフライン: %idle まで含めて 0.00 (現行版は行ごと出さない)
        "00:10:00          1      0.00      0.00      0.00      0.00      0.00      0.00",
        "00:20:00        all      5.00      0.00      1.00      0.50      0.00     93.50",
        // tickless: %idle = 100.00
        "00:20:00          0      0.00      0.00      0.00      0.00      0.00    100.00",
        // 復帰した CPU 1 はオフライン時に R0 の値で上書きされているので R0 との差:
        // user 6000 / sys 1200 / iowait 800 / idle 112000 (計 120000)
        // → 5.00 / 1.00 / 0.67 / 93.33
        "00:20:00          1      5.00      0.00      1.00      0.67      0.00     93.33",
        // 平均: all は g_itv = 440000 - 200000 = 240000
        "Average:        all      5.00      0.00      1.00      0.50      0.00     93.50",
        "Average:          0      5.00      0.00      1.00      0.50      0.00     93.50",
        "Average:          1      5.00      0.00      1.00      0.67      0.00     93.33",
    ];
    assert_eq!(&lines[3..], &expected[..], "{out}");
}

/// `-u ALL` の tickless 行は本家の不具合で 9 列しか出ない (`%gnice` の位置に 100.00)。
#[test]
fn tickless_row_with_u_all_has_nine_columns() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "el7-cpu", cpu_fixture());
    let out = run(&["-u", "ALL", "-P", "0", "-t"], &file);
    let row = out
        .lines()
        .find(|l| l.starts_with("00:20:00          0"))
        .expect("CPU 0 の R2 行");
    let values: Vec<&str> = row.split_whitespace().skip(2).collect();
    assert_eq!(
        values,
        [
            "0.00", "0.00", "0.00", "0.00", "0.00", "0.00", "0.00", "0.00", "100.00"
        ]
    );
}

/// `-P ALL` / `-A` は 1 サンプルを 8200 行 (`BITMAP_SIZE(8192) × 8`) と数えるので、
/// パイプ出力でも 11 サンプルごとに列見出しを出し直す (RHEL 7 の `NR_CPUS = 8192`)。
#[test]
fn headers_repeat_every_eleven_samples_with_all_cpus() {
    let recs: Vec<Rec> = (0..14u64)
        .map(|k| {
            stats(
                200_000 + k * 120_000,
                100_000 + k * 60_000,
                ((k / 6) as u8, ((k % 6) * 10) as u8, 0),
                &[
                    cpu(1000 + k * 6000, 500, 198_000 + k * 114_000, 0),
                    cpu(600 + k * 3000, 300, 99_100 + k * 57_000, 0),
                    cpu(400 + k * 3000, 200, 99_400 + k * 57_000, 0),
                ],
            )
        })
        .collect();
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "el7-header", build(&[A_CPU], &recs));
    let out = run(&["-u", "-P", "ALL", "-t"], &file);
    let header_times: Vec<&str> = out
        .lines()
        .filter(|l| l.contains("%user"))
        .map(|l| &l[..8])
        .collect();
    // 1 回目は R0 の時刻、2 回目は 11 サンプル表示した後 (R11 の時刻)
    assert_eq!(header_times, ["00:00:00", "01:50:00"], "{out}");
}

/// ゲスト時間を含む 2 CPU の 3 サンプル (10 フィールドを宣言順に並べる)。
///
/// 並びは user, nice, sys, idle, iowait, steal, hardirq, softirq, guest, guest_nice。
///
/// - "all" は R0→R1→R2 で同じだけ増え、`uptime` の差 (g_itv) は 120000
/// - CPU 0 は R1 で guest が user より多く増え、`user - guest` が 400 → 200 に減る
/// - CPU 1 は R1 で guest_nice が nice より多く増え、`nice - guest_nice` が 50 → 0 に減る。
///   R2 では idle が 950 減る
fn guest_cpu_fixture() -> Vec<u8> {
    let ticks = |v: [u64; 10]| counters(160, 16, &v);
    let rec = |k: u64, cpus: [[u64; 10]; 3]| {
        stats(
            200_000 + k * 120_000,
            100_000 + k * 60_000,
            at(k),
            &cpus.map(ticks),
        )
    };
    build(
        &[A_CPU],
        &[
            rec(
                0,
                [
                    [1000, 500, 300, 100_000, 100, 50, 20, 30, 200, 100],
                    [500, 0, 100, 50_000, 0, 0, 0, 0, 100, 0],
                    [300, 100, 100, 50_000, 0, 0, 0, 0, 0, 50],
                ],
            ),
            rec(
                1,
                [
                    [7000, 1700, 2100, 206_800, 700, 650, 620, 1230, 2600, 700],
                    [600, 0, 1100, 108_900, 0, 0, 0, 0, 400, 0],
                    [300, 150, 100, 109_950, 0, 0, 0, 0, 0, 150],
                ],
            ),
            rec(
                2,
                [
                    [
                        13_000, 2900, 3900, 313_600, 1300, 1250, 1220, 2430, 5000, 1300,
                    ],
                    [6600, 0, 1100, 162_900, 0, 0, 0, 0, 400, 0],
                    [31_300, 150, 30_050, 109_000, 0, 0, 0, 0, 0, 150],
                ],
            ),
        ],
    )
}

/// `-u ALL` の `%usr` / `%nice` はゲスト時間を引いた値で、減っていれば 0.00。
/// 個別 CPU の区間 (`get_per_cpu_interval()`) は、`user - guest` / `nice - guest_nice`
/// が減った分を tick の差に足し戻す。`%idle` は idle が減っていれば 0.00。
///
/// `-u` (既定) は `%user` にゲスト時間を含め、`%system` = sys + hardirq + softirq。
#[test]
fn cpu_columns_subtract_guest_time_and_widen_the_interval() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "el7-guest", guest_cpu_fixture());
    let all = |ts: &str, v: &[&str]| line_at(ts, "     all", v);
    let n = |ts: &str, cpu: &str, v: &[&str]| line_at(ts, &format!("     {cpu:>3}"), v);

    // "all" は g_itv = 120000 で割る: usr (6000 - 2400) / 1200 = 3.00、
    // nice (1200 - 600) = 0.50、sys 1.50、iowait / steal / irq 0.50、soft 1.00、
    // guest 2.00、gnice 0.50、idle 106800 / 1200 = 89.00
    let all_row = [
        "3.00", "0.50", "1.50", "0.50", "0.50", "0.50", "1.00", "2.00", "0.50", "89.00",
    ];
    let expected = report(&[
        header(
            "00:00:00",
            "     CPU      %usr     %nice      %sys   %iowait    %steal      %irq     %soft    %guest    %gnice     %idle",
        ),
        all("00:10:00", &all_row),
        // CPU 0: tick の差 60000 に user - guest の減少 200 を足して 60200 で割る。
        // %usr は減ったので 0.00、sys 1000 → 1.66、guest 300 → 0.50、idle 58900 → 97.84
        n(
            "00:10:00",
            "0",
            &[
                "0.00", "0.00", "1.66", "0.00", "0.00", "0.00", "0.00", "0.50", "0.00", "97.84",
            ],
        ),
        // CPU 1: 60000 + nice - guest_nice の減少 50 = 60050。gnice 100 → 0.17、idle 59950 → 99.83
        n(
            "00:10:00",
            "1",
            &[
                "0.00", "0.00", "0.00", "0.00", "0.00", "0.00", "0.00", "0.00", "0.17", "99.83",
            ],
        ),
        all("00:20:00", &all_row),
        n(
            "00:20:00",
            "0",
            &[
                "10.00", "0.00", "0.00", "0.00", "0.00", "0.00", "0.00", "0.00", "0.00", "90.00",
            ],
        ),
        // idle が減った: usr 31000 / 600 = 51.67、sys 29950 / 600 = 49.92、idle 0.00
        n(
            "00:20:00",
            "1",
            &[
                "51.67", "0.00", "49.92", "0.00", "0.00", "0.00", "0.00", "0.00", "0.00", "0.00",
            ],
        ),
        all("Average:", &all_row),
        // R0→R2: tick の差 120000 (ゲストの減少は無い)。usr 5800 → 4.83
        n(
            "Average:",
            "0",
            &[
                "4.83", "0.00", "0.83", "0.00", "0.00", "0.00", "0.00", "0.25", "0.00", "94.08",
            ],
        ),
        // R0→R2: nice - guest_nice が 50 → 0 に減ったので 120050 で割る
        n(
            "Average:",
            "1",
            &[
                "25.82", "0.00", "24.95", "0.00", "0.00", "0.00", "0.00", "0.00", "0.08", "49.15",
            ],
        ),
    ]);
    assert_eq!(run(&["-u", "ALL", "-P", "ALL", "-t"], &file), expected);

    // -u: all の %user = 6000 / 1200、%system = (1800 + 600 + 1200) / 1200
    let all_row = ["5.00", "1.00", "3.00", "0.50", "0.50", "89.00"];
    let expected = report(&[
        header("00:00:00", CPU_HEADER),
        all("00:10:00", &all_row),
        // 区間は -u ALL と同じ 60200 / 60050 (user 100 → 0.17、nice 50 → 0.08)
        n(
            "00:10:00",
            "0",
            &["0.17", "0.00", "1.66", "0.00", "0.00", "97.84"],
        ),
        n(
            "00:10:00",
            "1",
            &["0.00", "0.08", "0.00", "0.00", "0.00", "99.83"],
        ),
        all("00:20:00", &all_row),
        n(
            "00:20:00",
            "0",
            &["10.00", "0.00", "0.00", "0.00", "0.00", "90.00"],
        ),
        n(
            "00:20:00",
            "1",
            &["51.67", "0.00", "49.92", "0.00", "0.00", "0.00"],
        ),
        all("Average:", &all_row),
        n(
            "Average:",
            "0",
            &["5.08", "0.00", "0.83", "0.00", "0.00", "94.08"],
        ),
        n(
            "Average:",
            "1",
            &["25.82", "0.04", "24.95", "0.00", "0.00", "49.15"],
        ),
    ]);
    assert_eq!(run(&["-u", "-P", "ALL", "-t"], &file), expected);
}

// ===========================================================================
// メモリ
// ===========================================================================

/// 2 サンプル表示したときの平均で、el7 の整数除算が効く値の組。
fn memory_fixture() -> Vec<u8> {
    //            frmkb bufkb   camkb tlmkb frskb tlskb caskb comkb act inact dirty
    let m = |fr, buf, ca, com| memory([fr, buf, 5000, 1000, 60, 100, ca, com, 10, 20, 30]);
    build(
        &[A_CPU, A_MEMORY],
        &[
            stats(
                200_000,
                100_000,
                (0, 0, 0),
                &[
                    cpu(0, 0, 200_000, 0),
                    cpu(0, 0, 100_000, 0),
                    cpu(0, 0, 100_000, 0),
                    m(399, 10_000, 14, 299),
                ],
            ),
            stats(
                320_000,
                160_000,
                (0, 10, 0),
                &[
                    cpu(0, 0, 320_000, 0),
                    cpu(0, 0, 160_000, 0),
                    cpu(0, 0, 160_000, 0),
                    m(401, 14_096, 15, 300),
                ],
            ),
            stats(
                440_000,
                220_000,
                (0, 20, 0),
                &[
                    cpu(0, 0, 440_000, 0),
                    cpu(0, 0, 220_000, 0),
                    cpu(0, 0, 220_000, 0),
                    m(400, 14_096, 16, 301),
                ],
            ),
        ],
    )
}

#[test]
fn memory_averages_keep_the_integer_divisions() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "el7-mem", memory_fixture());

    let out = run(&["-r", "-t"], &file);
    let avg = out.lines().find(|l| l.starts_with("Average:")).unwrap();
    // kbmemfree = 801 / 2 = 400.5 → %9.0f は偶数丸めで 400
    // kbmemused = 最後の tlmkb 1000 - 400.5 = 599.5 → 600
    // %memused  = (1000 - (801 / 2 = 400)) / 1000 = 60.00 (整数除算。浮動小数なら 59.95)
    // %commit   = (601 / 2 = 300) / (tlmkb + tlskb = 1100) = 27.27 (浮動小数なら 27.32)
    assert_eq!(
        avg,
        "Average:          400       600     60.00     14096      5000       300     27.27        10        20        30"
    );
    // 瞬時値: %memused = (1000 - 401) / 1000 = 59.90、%commit = 300 / 1100 = 27.27
    assert!(
        out.contains(
            "00:10:00          401       599     59.90     14096      5000       300     27.27        10        20        30"
        ),
        "{out}"
    );

    let out = run(&["-S", "-t"], &file);
    let avg = out.lines().find(|l| l.starts_with("Average:")).unwrap();
    // kbswpcad = (15 + 16) / 2 を整数で割って 15 (浮動小数なら 15.5 → 16)
    // %swpcad  = 15 / (100 - 60) = 37.50
    assert_eq!(
        avg,
        "Average:           60        40     40.00        15     37.50"
    );
}

/// `-R` はページ数の変化。kB → ページの換算にページサイズを使う。
#[test]
fn memory_pages_depend_on_the_page_size() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "el7-mem", memory_fixture());
    // bufkb 10000 → 14096 を 600 秒 (itv = uptime0 の差 60000) で
    //   4 KB ページ: 2500 → 3524 で 1024 ページ → 1024 / 60000 * 100 = 1.71
    //  64 KB ページ: 156 → 220 で 64 ページ → 0.11
    let line = |out: &str| {
        out.lines()
            .find(|l| l.starts_with("00:10:00"))
            .unwrap()
            .to_string()
    };
    let out = run(&["-R", "-t"], &file);
    assert_eq!(line(&out), "00:10:00         0.00      1.71      0.00");
    // 本家の突き合わせ用ビルドはページサイズを 4096 に固定しているので、ここは比べない
    let out = command()
        .args([
            "--sar-profile",
            "sysstat-10.1.5-el7",
            "--sar-page-size",
            "65536",
        ])
        .args(["-R", "-t", "-f"])
        .arg(&file)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        line(&String::from_utf8(out.stdout).unwrap()),
        "00:10:00         0.00      0.11      0.00"
    );
}

/// 総量が 0 のときの割合は 0.00 (0 で割らない)。瞬時値と平均の両方で同じ。
///
/// `%memused` / `%commit` は tlmkb (と tlmkb + tlskb)、`%swpused` は tlskb、
/// `%swpcad` は使用中のスワップ (tlskb - frskb) が 0 かを見る。
#[test]
fn memory_percentages_are_zero_without_totals() {
    let dir = tempfile::tempdir().unwrap();
    //                    frmkb bufkb camkb tlmkb frskb tlskb caskb comkb act inact dirty
    let empty = || memory([0, 0, 0, 0, 0, 0, 0, 5, 0, 0, 0]);
    let bytes = samples(&[A_MEMORY], 3, |_| vec![empty()]);
    let file = write(dir.path(), "el7-mem-empty", bytes);
    let amt = ["0", "0", "0.00", "0", "0", "5", "0.00", "0", "0", "0"];
    assert_eq!(
        run(&["-r", "-t"], &file),
        report(&[
            header(
                "00:00:00",
                " kbmemfree kbmemused  %memused kbbuffers  kbcached  kbcommit   %commit  kbactive   kbinact   kbdirty",
            ),
            line("00:10:00", &amt),
            line("00:20:00", &amt),
            line("Average:", &amt),
        ])
    );
    let swap = ["0", "0", "0.00", "0", "0.00"];
    assert_eq!(
        run(&["-S", "-t"], &file),
        report(&[
            header(
                "00:00:00",
                " kbswpfree kbswpused  %swpused  kbswpcad   %swpcad"
            ),
            line("00:10:00", &swap),
            line("00:20:00", &swap),
            line("Average:", &swap),
        ])
    );
}

/// hugepages の平均は `%hugused` だけ整数で割ってから比を取る (`%swpused` と同じ形)。
///
/// 本家は `STATS_HUGE_SIZE` を `sizeof(struct stats_memory)` (88) と書いているので、
/// ファイル上の 1 item は 88 バイト。構造体の後ろ 72 バイトは読まれない。
#[test]
fn hugepages_average_divides_integers_before_the_ratio() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = samples(&[A_HUGE], 4, |k| match k {
        // R0 は区間の基準で、表示も平均もしない
        0 => vec![huge(99, 99)],
        1 => vec![huge(3, 10)],
        2 => vec![huge(4, 11)],
        _ => vec![huge(0, 0)],
    });
    let file = write(dir.path(), "el7-huge", bytes);
    let out = run(&["-H", "-t"], &file);
    let expected = report(&[
        header("00:00:00", " kbhugfree kbhugused  %hugused"),
        // %hugused = (10 - 3) / 10 = 70.00
        line("00:10:00", &["3", "7", "70.00"]),
        // 7 / 11 = 63.64
        line("00:20:00", &["4", "7", "63.64"]),
        // tlhkb = 0 なら 0.00 (0 で割らない)
        line("00:30:00", &["0", "0", "0.00"]),
        // kbhugfree = 7 / 3 = 2.33 → 2、kbhugused = 21 / 3 - 7 / 3 = 4.67 → 5
        // %hugused = (21 / 3 = 7 - 7 / 3 = 2) / 7 = 71.43 (整数除算。浮動小数なら 66.67)
        line("Average:", &["2", "5", "71.43"]),
    ]);
    assert_eq!(out, expected);
}

// ===========================================================================
// 瞬時値の activity (負荷・カーネルテーブル・ソケット)
// ===========================================================================

/// `-q` は瞬時値。平均は `(double) 合計 / avg_count`、負荷だけ
/// `(double) 合計 / (avg_count * 100)`。列の並びは構造体の宣言順と違う。
#[test]
fn queue_prints_instant_values_and_averages_the_load_over_count_times_100() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = samples(&[A_QUEUE], 3, |k| match k {
        0 => vec![queue(50, 50, [5000, 5000, 5000], 5000)],
        1 => vec![queue(2, 1, [105, 210, 399], 301)],
        _ => vec![queue(3, 2, [106, 211, 400], 302)],
    });
    let file = write(dir.path(), "el7-queue", bytes);
    let out = run(&["-q", "-t"], &file);
    let expected = report(&[
        header(
            "00:00:00",
            "   runq-sz  plist-sz   ldavg-1   ldavg-5  ldavg-15   blocked",
        ),
        // nr_running, nr_threads, load_avg / 100, procs_blocked の順
        line("00:10:00", &["2", "301", "1.05", "2.10", "3.99", "1"]),
        line("00:20:00", &["3", "302", "1.06", "2.11", "4.00", "2"]),
        // runq-sz 5 / 2 = 2.5 → %9.0f は偶数丸めで 2、plist-sz 603 / 2 = 301.5 → 302
        // ldavg-1 = 211 / (2 × 100) = 1.055 → double では 1.05499… なので 1.05
        // ldavg-5 = 421 / 200 = 2.105 → 2.10499… で 2.10
        // ldavg-15 = 799 / 200 = 3.995 → 3.99500…01 なので 4.00、blocked 3 / 2 = 1.5 → 2
        line("Average:", &["2", "302", "1.05", "2.10", "4.00", "2"]),
    ]);
    assert_eq!(out, expected);
}

/// `-v` / `-n SOCK` / `-n SOCK6` は瞬時値の整数。平均は `(double) 合計 / avg_count` で、
/// `%9.0f` の偶数丸めがかかる。列の並びは構造体の宣言順と違う
/// (`dentunusd` が先頭、`tcp-tw` が末尾)。
#[test]
fn gauge_activities_average_as_doubles() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = samples(&[A_KTABLES, A_NET_SOCK, A_NET_SOCK6], 3, |k| match k {
        0 => vec![ktables(9, 9, 9, 9), sock([9; 6]), sock6([9; 4])],
        1 => vec![
            ktables(1000, 2000, 3000, 1),
            sock([100, 20, 5, 7, 1, 0]),
            sock6([10, 20, 30, 40]),
        ],
        _ => vec![
            ktables(1001, 2003, 3004, 4),
            sock([103, 21, 6, 8, 2, 1]),
            sock6([12, 21, 31, 43]),
        ],
    });
    let file = write(dir.path(), "el7-gauge", bytes);

    let out = run(&["-v", "-t"], &file);
    assert_eq!(
        out,
        report(&[
            header("00:00:00", " dentunusd   file-nr  inode-nr    pty-nr"),
            line("00:10:00", &["3000", "1000", "2000", "1"]),
            line("00:20:00", &["3004", "1001", "2003", "4"]),
            // 1000.5 → 1000、2001.5 → 2002、2.5 → 2 (偶数丸め)
            line("Average:", &["3002", "1000", "2002", "2"]),
        ])
    );

    let out = run(&["-n", "SOCK", "-t"], &file);
    assert_eq!(
        out,
        report(&[
            header(
                "00:00:00",
                "    totsck    tcpsck    udpsck    rawsck   ip-frag    tcp-tw"
            ),
            // tcp_tw は構造体の 3 番目だが末尾に出る
            line("00:10:00", &["100", "20", "7", "1", "0", "5"]),
            line("00:20:00", &["103", "21", "8", "2", "1", "6"]),
            // 101.5 → 102、20.5 → 20、7.5 → 8、1.5 → 2、0.5 → 0、5.5 → 6
            line("Average:", &["102", "20", "8", "2", "0", "6"]),
        ])
    );

    let out = run(&["-n", "SOCK6", "-t"], &file);
    assert_eq!(
        out,
        report(&[
            header("00:00:00", "   tcp6sck   udp6sck   raw6sck  ip6-frag"),
            line("00:10:00", &["10", "20", "30", "40"]),
            line("00:20:00", &["12", "21", "31", "43"]),
            line("Average:", &["11", "20", "30", "42"]),
        ])
    );
}

// ===========================================================================
// S_VALUE を並べる activity
// ===========================================================================

/// `S_VALUE` を並べるだけの activity 1 つ分。
struct RateCase {
    args: &'static [&'static str],
    act: Act,
    /// フィールドの間隔 (4 = `unsigned int`、8 = `unsigned long`・詰めた `unsigned long long`、
    /// 16 = `aligned(16)` の `unsigned long long`)。
    stride: usize,
    fields: usize,
    header: &'static str,
}

const RATE_CASES: &[RateCase] = &[
    RateCase {
        args: &["-W"],
        act: A_SWAP,
        stride: 8,
        fields: 2,
        header: "  pswpin/s pswpout/s",
    },
    // stats_io は先頭だけ aligned(16) で、残り 4 個は packed (8 バイトおき)
    RateCase {
        args: &["-b"],
        act: A_IO,
        stride: 8,
        fields: 5,
        header: "       tps      rtps      wtps   bread/s   bwrtn/s",
    },
    RateCase {
        args: &["-n", "NFS"],
        act: A_NET_NFS,
        stride: 4,
        fields: 6,
        header: "    call/s retrans/s    read/s   write/s  access/s  getatt/s",
    },
    RateCase {
        args: &["-n", "NFSD"],
        act: A_NET_NFSD,
        stride: 4,
        fields: 11,
        header: "   scall/s badcall/s  packet/s     udp/s     tcp/s     hit/s    miss/s   sread/s  swrite/s saccess/s sgetatt/s",
    },
    RateCase {
        args: &["-n", "IP"],
        act: A_NET_IP,
        stride: 16,
        fields: 8,
        header: "    irec/s  fwddgm/s    idel/s     orq/s   asmrq/s   asmok/s  fragok/s fragcrt/s",
    },
    RateCase {
        args: &["-n", "EIP"],
        act: A_NET_EIP,
        stride: 16,
        fields: 8,
        header: " ihdrerr/s iadrerr/s iukwnpr/s   idisc/s   odisc/s   onort/s    asmf/s   fragf/s",
    },
    RateCase {
        args: &["-n", "ICMP"],
        act: A_NET_ICMP,
        stride: 8,
        fields: 14,
        header: "    imsg/s    omsg/s    iech/s   iechr/s    oech/s   oechr/s     itm/s    itmr/s     otm/s    otmr/s  iadrmk/s iadrmkr/s  oadrmk/s oadrmkr/s",
    },
    RateCase {
        args: &["-n", "EICMP"],
        act: A_NET_EICMP,
        stride: 8,
        fields: 12,
        header: "    ierr/s    oerr/s idstunr/s odstunr/s   itmex/s   otmex/s iparmpb/s oparmpb/s   isrcq/s   osrcq/s  iredir/s  oredir/s",
    },
    RateCase {
        args: &["-n", "TCP"],
        act: A_NET_TCP,
        stride: 8,
        fields: 4,
        header: "  active/s passive/s    iseg/s    oseg/s",
    },
    // 再送列は retrans/s (現行版の retrseg/s ではない)
    RateCase {
        args: &["-n", "ETCP"],
        act: A_NET_ETCP,
        stride: 8,
        fields: 5,
        header: "  atmptf/s  estres/s retrans/s isegerr/s   orsts/s",
    },
    RateCase {
        args: &["-n", "UDP"],
        act: A_NET_UDP,
        stride: 8,
        fields: 4,
        header: "    idgm/s    odgm/s  noport/s idgmerr/s",
    },
    RateCase {
        args: &["-n", "IP6"],
        act: A_NET_IP6,
        stride: 16,
        fields: 10,
        header: "   irec6/s fwddgm6/s   idel6/s    orq6/s  asmrq6/s  asmok6/s imcpck6/s omcpck6/s fragok6/s fragcr6/s",
    },
    RateCase {
        args: &["-n", "EIP6"],
        act: A_NET_EIP6,
        stride: 16,
        fields: 11,
        header: " ihdrer6/s iadrer6/s iukwnp6/s  i2big6/s  idisc6/s  odisc6/s  inort6/s  onort6/s   asmf6/s  fragf6/s itrpck6/s",
    },
    RateCase {
        args: &["-n", "ICMP6"],
        act: A_NET_ICMP6,
        stride: 8,
        fields: 17,
        header: "   imsg6/s   omsg6/s   iech6/s  iechr6/s  oechr6/s  igmbq6/s  igmbr6/s  ogmbr6/s igmbrd6/s ogmbrd6/s irtsol6/s ortsol6/s  irtad6/s inbsol6/s onbsol6/s  inbad6/s  onbad6/s",
    },
    RateCase {
        args: &["-n", "EICMP6"],
        act: A_NET_EICMP6,
        stride: 8,
        fields: 11,
        header: "   ierr6/s idtunr6/s odtunr6/s  itmex6/s  otmex6/s iprmpb6/s oprmpb6/s iredir6/s oredir6/s ipck2b6/s opck2b6/s",
    },
    RateCase {
        args: &["-n", "UDP6"],
        act: A_NET_UDP6,
        stride: 8,
        fields: 4,
        header: "   idgm6/s   odgm6/s noport6/s idgmer6/s",
    },
];

/// フィールド j (0 起点) の k 番目のサンプルの値。
///
/// R0→R1 で 600 × (j + 1)、R1→R2 で 1200 × (j + 1) 増える。瞬時値は j + 1 と
/// 2 × (j + 1)、平均 (R0→R2) は 1.5 × (j + 1) になる。`unsigned int` の先頭
/// フィールドは R0 を 2^32 - 600 にして、R1 で 32bit の巻き戻りを踏ませる
/// (差は 32bit で取るので 600 のまま)。
fn rate_value(stride: usize, j: usize, k: u64) -> u64 {
    let step = [0, 600, 1800][k as usize] * (j as u64 + 1);
    let base = if stride == 4 && j == 0 {
        (1u64 << 32) - 600
    } else {
        1000
    };
    base + step
}

fn rate_fixture() -> Vec<u8> {
    let mut acts = vec![A_PCSW, A_PAGE];
    acts.extend(RATE_CASES.iter().map(|c| c.act));
    samples(&acts, 3, |k| {
        // cswch/s は R1→R2 で逆行させる (ll_s_value は 0.00 にする)
        let mut parts = vec![
            pcsw(
                [10_000, 16_000, 15_000][k as usize],
                [100, 700, 1900][k as usize],
            ),
            counters(
                64,
                8,
                &(0..8).map(|j| rate_value(8, j, k)).collect::<Vec<_>>(),
            ),
        ];
        for c in RATE_CASES {
            let v: Vec<u64> = (0..c.fields).map(|j| rate_value(c.stride, j, k)).collect();
            parts.push(counters(c.act.size as usize, c.stride, &v));
        }
        parts
    })
}

/// 各 activity の列は構造体の宣言順に並んだ `S_VALUE`。構造体のオフセット
/// (`stats_io` の packed、`stats_net_ip` の aligned(16) など) や列の並びがずれると、
/// 値が入れ替わって見える。
#[test]
fn rate_activities_print_s_value_per_field() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "el7-rate", rate_fixture());
    let cells = |n: usize, f: f64| -> Vec<String> {
        (1..=n).map(|j| format!("{:.2}", j as f64 * f)).collect()
    };
    for c in RATE_CASES {
        let mut args = c.args.to_vec();
        args.push("-t");
        let out = run(&args, &file);
        let row = |ts: &str, f: f64| {
            let v = cells(c.fields, f);
            line(ts, &v.iter().map(String::as_str).collect::<Vec<_>>())
        };
        let expected = report(&[
            header("00:00:00", c.header),
            row("00:10:00", 1.0),
            row("00:20:00", 2.0),
            row("Average:", 1.5),
        ]);
        assert_eq!(out, expected, "{:?}", c.args);
    }
}

/// CPU 1 個分の区間 (`uptime0` の差) を使う条件が、瞬時値と平均行で違う。
///
/// 瞬時値 (`get_itv_value()`) は `A_CPU` の `nr` が 2 を超えるときだけ `uptime0`、
/// 平均行 (`write_stats_avg()`) は 1 を超えれば `uptime0`。`nr` = 2 (CPU 1 個) の
/// ファイルで `uptime` と `uptime0` が違えば、瞬時値は `uptime` の差で割り、
/// 平均行は `uptime0` の差で割る。`nr` = 1 ならどちらも `uptime`。
#[test]
fn rows_and_average_switch_to_uptime0_at_different_cpu_counts() {
    let dir = tempfile::tempdir().unwrap();
    // uptime は uptime0 の 2 倍 (R0→R1 で 120000 と 60000)。pswpin は 600 ずつ増える
    let file_with = |nr: i32| {
        let recs: Vec<Rec> = (0..3u64)
            .map(|k| {
                let uptime0 = 100_000 + k * 60_000;
                let mut parts = vec![cpu(0, 0, 2 * uptime0, 0)];
                if nr == 2 {
                    parts.push(cpu(0, 0, uptime0, 0));
                }
                parts.push(counters(16, 8, &[1000 + 600 * k, 0]));
                stats(2 * uptime0, uptime0, at(k), &parts)
            })
            .collect();
        build(&[A_CPU.nr(nr), A_SWAP], &recs)
    };
    let banner = "Linux 3.10.0-el7 (testhost) \t09/13/20 \t_x86_64_\t(1 CPU)";
    let expect = |avg: &str| {
        [
            banner.to_string(),
            header("00:00:00", "  pswpin/s pswpout/s"),
            // 瞬時値はどちらも uptime の差 120000 で割る: 600 / 1200
            line("00:10:00", &["0.50", "0.00"]),
            line("00:20:00", &["0.50", "0.00"]),
            line("Average:", &[avg, "0.00"]),
        ]
        .join("\n")
            + "\n"
    };
    let one_cpu = write(dir.path(), "el7-one-cpu", file_with(2));
    // 平均行は uptime0 の差 120000 で割る: 1200 / 1200
    assert_eq!(run(&["-W", "-t"], &one_cpu), expect("1.00"));
    let aggregate_only = write(dir.path(), "el7-cpu-all-only", file_with(1));
    // 平均行も uptime の差 240000 で割る: 1200 / 2400
    assert_eq!(run(&["-W", "-t"], &aggregate_only), expect("0.50"));
}

/// `-w` の `cswch/s` は `ll_s_value()` (el7 の `dyn-tick` パッチで逆行は 0.00)、
/// `proc/s` は `unsigned long` の `S_VALUE`。`-B` の `%vmeff` は
/// Δpgsteal / (Δpgscan_kswapd + Δpgscan_direct)。
#[test]
fn task_and_paging_columns_follow_the_el7_formulas() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "el7-rate", rate_fixture());

    let out = run(&["-w", "-t"], &file);
    assert_eq!(
        out,
        report(&[
            header("00:00:00", "    proc/s   cswch/s"),
            // proc/s 600 / 600、cswch/s 6000 / 600
            line("00:10:00", &["1.00", "10.00"]),
            // context_switch が 16000 → 15000 に減った区間は 0.00
            line("00:20:00", &["2.00", "0.00"]),
            // 平均は R0→R2: cswch/s = 5000 / 1200 = 4.17
            line("Average:", &["1.50", "4.17"]),
        ])
    );

    let out = run(&["-B", "-t"], &file);
    // %vmeff = Δpgsteal 8 / (Δkswapd 6 + Δdirect 7) (× 600 は約分) = 61.54
    let paging = |ts: &str, f: f64| {
        let mut v: Vec<String> = (1..=8).map(|j| format!("{:.2}", j as f64 * f)).collect();
        v.push("61.54".into());
        line(ts, &v.iter().map(String::as_str).collect::<Vec<_>>())
    };
    assert_eq!(
        out,
        report(&[
            header(
                "00:00:00",
                "  pgpgin/s pgpgout/s   fault/s  majflt/s  pgfree/s pgscank/s pgscand/s pgsteal/s    %vmeff"
            ),
            paging("00:10:00", 1.0),
            paging("00:20:00", 2.0),
            paging("Average:", 1.5),
        ])
    );
}

// ===========================================================================
// 名前・番号を持つ item (NIC・ディスク・TTY・割り込み)
// ===========================================================================

/// `-n DEV` の NIC の枠の選び方 (`check_net_dev_reg()`)。
///
/// - R1 の eth1 は前サンプルに無い → 名前が `?` の枠 (1 番) を 0 に戻して使う
/// - R2 の eth1 はカウンタが減った → 再登録とみなして枠を 0 に戻す
///   (バイト数もパケット数も減っているので桁あふれではない)
/// - R2 の eth2 は前サンプルに無く `?` の枠も無い → 同じ位置 (2 番) の枠を 0 に戻す
/// - 名前が空の枠は出さない
/// - 平均行は区間の最初 (R0) を基準にして同じ判定をやり直す
#[test]
fn net_dev_rows_reuse_slots_like_check_net_dev_reg() {
    let dir = tempfile::tempdir().unwrap();
    let none = || net_dev("", [0; 7]);
    let bytes = samples(&[A_NET_DEV.nr(3)], 3, |k| match k {
        0 => vec![
            net_dev("eth0", [1000, 2000, 1_024_000, 2_048_000, 0, 0, 10]),
            net_dev("?", [5000; 7]),
            none(),
        ],
        1 => vec![
            net_dev("eth1", [6000, 12_000, 6_144_000, 12_288_000, 60, 120, 600]),
            net_dev("eth0", [1600, 3200, 1_638_400, 3_276_800, 0, 0, 16]),
            none(),
        ],
        _ => vec![
            net_dev("eth1", [3000, 6000, 3_072_000, 6_144_000, 36, 60, 300]),
            net_dev("eth0", [2800, 5600, 2_867_200, 5_734_400, 0, 0, 34]),
            net_dev("eth2", [600, 0, 61_440, 0, 0, 0, 0]),
        ],
    });
    let file = write(dir.path(), "el7-netdev", bytes);
    let out = run(&["-n", "DEV", "-t"], &file);
    let expected = report(&[
        header(
            "00:00:00",
            "     IFACE   rxpck/s   txpck/s    rxkB/s    txkB/s   rxcmp/s   txcmp/s  rxmcst/s",
        ),
        // 0 からの差を 600 秒で割る。rxkB/s は S_VALUE / 1024
        line(
            "00:10:00",
            &[
                "eth1", "10.00", "20.00", "10.00", "20.00", "0.10", "0.20", "1.00",
            ],
        ),
        line(
            "00:10:00",
            &[
                "eth0", "1.00", "2.00", "1.00", "2.00", "0.00", "0.00", "0.01",
            ],
        ),
        // 再登録: R2 の値そのものを 600 秒で割る
        line(
            "00:20:00",
            &[
                "eth1", "5.00", "10.00", "5.00", "10.00", "0.06", "0.10", "0.50",
            ],
        ),
        line(
            "00:20:00",
            &[
                "eth0", "2.00", "4.00", "2.00", "4.00", "0.00", "0.00", "0.03",
            ],
        ),
        // 同じ位置の枠 (R1 の空の枠) を 0 に戻して使う
        line(
            "00:20:00",
            &[
                "eth2", "1.00", "0.00", "0.10", "0.00", "0.00", "0.00", "0.00",
            ],
        ),
        // 平均 (R0→R2、1200 秒): eth1 は R0 の `?` の枠、eth2 は同じ位置の枠から
        line(
            "Average:",
            &[
                "eth1", "2.50", "5.00", "2.50", "5.00", "0.03", "0.05", "0.25",
            ],
        ),
        line(
            "Average:",
            &[
                "eth0", "1.50", "3.00", "1.50", "3.00", "0.00", "0.00", "0.02",
            ],
        ),
        line(
            "Average:",
            &[
                "eth2", "0.50", "0.00", "0.05", "0.00", "0.00", "0.00", "0.00",
            ],
        ),
    ]);
    assert_eq!(out, expected);
}

/// `-n EDEV` の再登録判定 (`check_net_edev_reg()`) は `rx_errors` を見ない。
///
/// eth1 は R1 で rx_errors だけが 65536 減るので枠は戻らず、`unsigned long long` の
/// 差 2^64 - 65536 がそのまま `S_VALUE` に入る。eth0 は R2 で tx_carrier_errors が
/// 減ったので枠を 0 に戻す。
///
/// `itv` を 65536 jiffies にして、割り算が 2 進で割り切れるようにしてある
/// (16384 増えると 25.00、(2^64 - 65536) / 65536 × 100 = 28147497671065500)。
#[test]
fn net_edev_reregistration_ignores_rx_errors() {
    const U: u64 = 16_384;
    let dir = tempfile::tempdir().unwrap();
    // フィールド f (構造体の並び) を U × (f + 1) × m にした eth0
    let eth0 = |m: u64| {
        let v: [u64; 9] = std::array::from_fn(|f| U * (f as u64 + 1) * m);
        v
    };
    let recs: Vec<Rec> = (0..3)
        .map(|k| {
            let parts = match k {
                0 => vec![net_edev("eth0", eth0(0)), net_edev("eth1", [262_144; 9])],
                1 => {
                    let mut e1 = [262_144 + U; 9];
                    e1[1] = 262_144 - 65_536;
                    vec![net_edev("eth0", eth0(1)), net_edev("eth1", e1)]
                }
                _ => {
                    let mut e0 = eth0(2);
                    e0[8] = U;
                    let mut e1 = [262_144 + 2 * U; 9];
                    e1[1] = 262_144 + U;
                    vec![net_edev("eth0", e0), net_edev("eth1", e1)]
                }
            };
            sample_every(k, 65_536, &parts)
        })
        .collect();
    let file = write(
        dir.path(),
        "el7-netedev",
        build(&[A_CPU, A_NET_EDEV.nr(2)], &recs),
    );
    let out = run(&["-n", "EDEV", "-t"], &file);
    let rest = ["25.00"; 8];
    let eth1 = |ts: &str, first: &str| {
        let mut v = vec!["eth1", first];
        v.extend(rest);
        line(ts, &v)
    };
    let expected = report(&[
        header(
            "00:00:00",
            "     IFACE   rxerr/s   txerr/s    coll/s  rxdrop/s  txdrop/s  txcarr/s  rxfram/s  rxfifo/s  txfifo/s",
        ),
        // 列は rx_errors, tx_errors, collisions, rx_dropped, tx_dropped,
        // tx_carrier_errors, rx_frame_errors, rx_fifo_errors, tx_fifo_errors
        line(
            "00:10:00",
            &[
                "eth0", "50.00", "75.00", "25.00", "100.00", "125.00", "225.00", "200.00",
                "150.00", "175.00",
            ],
        ),
        eth1("00:10:00", "28147497671065500.00"),
        // 枠を 0 に戻したので R2 の値そのもの (tx_carrier_errors は U で 25.00)
        line(
            "00:20:00",
            &[
                "eth0", "100.00", "150.00", "50.00", "200.00", "250.00", "25.00", "400.00",
                "300.00", "350.00",
            ],
        ),
        // rx_errors: 196608 → 278528 で 81920 / 65536 × 100
        eth1("00:20:00", "125.00"),
        // 平均 (R0→R2、131072 jiffies): R0 の eth0 は全部 0 なので戻さない
        line(
            "Average:",
            &[
                "eth0", "50.00", "75.00", "25.00", "100.00", "125.00", "12.50", "200.00", "150.00",
                "175.00",
            ],
        ),
        eth1("Average:", "12.50"),
    ]);
    assert_eq!(out, expected);
}

/// `-d` のディスクの枠の選び方 (`check_disk_reg()`) と拡張統計の式。
///
/// - R1 の dev8-32 は前サンプルに無い → major + minor が 0 の空き枠 (2 番) を使う
/// - R1 の dev8-16 は位置が変わっても major/minor で前の枠 (1 番) を見つける
/// - R2 の dev8-0 は nr_ios / rd_sect / wr_sect が揃って減った → 枠を 0 に戻す
/// - 平均行は区間の最初 (R0) を基準にして同じ判定をやり直す
#[test]
fn disk_rows_follow_check_disk_reg_and_the_extended_stats() {
    let dir = tempfile::tempdir().unwrap();
    let sdb = || disk((8, 16), 500, (500, 500), [50, 50, 50, 50]);
    let bytes = samples(&[A_DISK.nr(3)], 3, |k| match k {
        0 => vec![
            disk((8, 0), 1000, (2000, 3000), [100, 200, 300, 400]),
            sdb(),
            disk((0, 0), 0, (0, 0), [0; 4]),
        ],
        1 => vec![
            disk((8, 0), 1600, (3200, 4800), [400, 500, 6300, 60_400]),
            disk((8, 32), 600, (1200, 0), [60, 0, 600, 600]),
            sdb(),
        ],
        _ => vec![
            disk((8, 0), 10, (20, 36), [1, 2, 3, 4]),
            disk((8, 32), 1200, (2400, 600), [120, 60, 1200, 1200]),
            sdb(),
        ],
    });
    let file = write(dir.path(), "el7-disk", bytes);
    let zeros = ["0.00"; 8];
    let idle = |ts: &str| {
        let mut v = vec!["dev8-16"];
        v.extend(zeros);
        line(ts, &v)
    };
    let expected = report(&[
        header(
            "00:00:00",
            "       DEV       tps  rd_sec/s  wr_sec/s  avgrq-sz  avgqu-sz     await     svctm     %util",
        ),
        // tps = 600 / 600、rd/wr_sec/s = 1200 / 600・1800 / 600、avgrq-sz = 3000 / 600
        // avgqu-sz = S_VALUE(rq_ticks) / 1000 = 100 / 1000、await = (300 + 300) / 600
        // util = S_VALUE(tot_ticks) = 10、svctm = util / tps = 10、%util = util / 10
        line(
            "00:10:00",
            &[
                "dev8-0", "1.00", "2.00", "3.00", "5.00", "0.10", "1.00", "10.00", "1.00",
            ],
        ),
        // 空き枠 (0 から): avgqu-sz = 1 / 1000 は 0.00
        line(
            "00:10:00",
            &[
                "dev8-32", "1.00", "2.00", "0.00", "2.00", "0.00", "0.10", "1.00", "0.10",
            ],
        ),
        // I/O が無い区間は avgrq-sz / await / svctm を 0.00 にする (0 で割らない)
        idle("00:10:00"),
        // 再登録: tps = 10 / 600、avgrq-sz = (20 + 36) / 10、await = (1 + 2) / 10
        // svctm = (3 / 600) / (10 / 600) = 0.30
        line(
            "00:20:00",
            &[
                "dev8-0", "0.02", "0.03", "0.06", "5.60", "0.00", "0.30", "0.30", "0.00",
            ],
        ),
        line(
            "00:20:00",
            &[
                "dev8-32", "1.00", "2.00", "1.00", "3.00", "0.00", "0.20", "1.00", "0.10",
            ],
        ),
        idle("00:20:00"),
        // 平均 (1200 秒): dev8-0 は R0 から見ても減っているので 0 から、
        // dev8-32 は R0 に無いので空き枠 (0 から)
        line(
            "Average:",
            &[
                "dev8-0", "0.01", "0.02", "0.03", "5.60", "0.00", "0.30", "0.30", "0.00",
            ],
        ),
        line(
            "Average:",
            &[
                "dev8-32", "1.00", "2.00", "0.50", "2.50", "0.00", "0.15", "1.00", "0.10",
            ],
        ),
        idle("Average:"),
    ]);
    assert_eq!(run(&["-d", "-t"], &file), expected);
    // -p でも実行ホストの名前は引かない (本家も Linux の /sys が無ければ同じ)
    assert_eq!(run(&["-d", "-p", "-t"], &file), expected);
}

/// 前サンプルに無い NIC / ディスクは、空き枠 (NIC は名前が `?`、ディスクは
/// major + minor が 0) が無ければ同じ位置の枠を 0 に戻して使う。そこにいた
/// 別の NIC (eth0) / ディスク (dev8-16) の枠でも上書きする。
#[test]
fn new_items_take_the_same_rank_without_a_free_slot() {
    let dir = tempfile::tempdir().unwrap();
    let nic = |name: &str, rx| net_dev(name, [rx, 0, 0, 0, 0, 0, 0]);
    let dev = |minor, ios| disk((8, minor), ios, (0, 0), [0; 4]);
    let bytes = samples(&[A_DISK.nr(2), A_NET_DEV.nr(2)], 3, |k| match k {
        0 => vec![
            dev(0, 100),
            dev(16, 200),
            nic("eth0", 1000),
            nic("eth1", 2000),
        ],
        1 => vec![
            dev(0, 700),
            dev(32, 900),
            nic("eth9", 300),
            nic("eth1", 2600),
        ],
        _ => vec![
            dev(0, 1300),
            dev(32, 1500),
            nic("eth9", 900),
            nic("eth1", 3200),
        ],
    });
    let file = write(dir.path(), "el7-same-rank", bytes);

    let disk_row = |ts: &str, name: &str, tps: &str| {
        let mut v = vec![name, tps];
        v.extend(["0.00"; 7]);
        line(ts, &v)
    };
    assert_eq!(
        run(&["-d", "-t"], &file),
        report(&[
            header(
                "00:00:00",
                "       DEV       tps  rd_sec/s  wr_sec/s  avgrq-sz  avgqu-sz     await     svctm     %util",
            ),
            disk_row("00:10:00", "dev8-0", "1.00"),
            // dev8-16 の枠を 0 に戻して使うので 900 / 600 (戻さなければ 700 / 600 = 1.17)
            disk_row("00:10:00", "dev8-32", "1.50"),
            disk_row("00:20:00", "dev8-0", "1.00"),
            disk_row("00:20:00", "dev8-32", "1.00"),
            disk_row("Average:", "dev8-0", "1.00"),
            // R0 から見ても無いので同じ位置の枠を 0 に戻す: 1500 / 1200
            disk_row("Average:", "dev8-32", "1.25"),
        ])
    );

    let nic_row = |ts: &str, name: &str, rx: &str| {
        let mut v = vec![name, rx];
        v.extend(["0.00"; 6]);
        line(ts, &v)
    };
    assert_eq!(
        run(&["-n", "DEV", "-t"], &file),
        report(&[
            header(
                "00:00:00",
                "     IFACE   rxpck/s   txpck/s    rxkB/s    txkB/s   rxcmp/s   txcmp/s  rxmcst/s",
            ),
            // eth0 の枠を 0 に戻して使うので 300 / 600
            nic_row("00:10:00", "eth9", "0.50"),
            nic_row("00:10:00", "eth1", "1.00"),
            nic_row("00:20:00", "eth9", "1.00"),
            nic_row("00:20:00", "eth1", "1.00"),
            nic_row("Average:", "eth9", "0.75"),
            nic_row("Average:", "eth1", "1.00"),
        ])
    );
}

/// `-y` は回線番号が前サンプルと同じときだけ値を出し、違えば `N/A`。
/// カウンタは `unsigned int` なので差は 32bit で巻き戻る。
#[test]
fn serial_rows_print_na_when_the_line_changes() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = samples(&[A_SERIAL.nr(3)], 3, |k| match k {
        0 => vec![
            serial([0xffff_fda8, 0, 0, 0, 0, 0], 1),
            serial([10; 6], 2),
            serial([0; 6], 0),
        ],
        1 => vec![
            serial([0, 1200, 1800, 2400, 3000, 3600], 1),
            serial([20; 6], 3),
            serial([0; 6], 0),
        ],
        _ => vec![
            serial([1200, 3600, 5400, 7200, 9000, 10_800], 1),
            serial([620; 6], 3),
            serial([5; 6], 5),
        ],
    });
    let file = write(dir.path(), "el7-serial", bytes);
    let na = |ts: &str, tty: &str| {
        let mut v = vec![tty];
        v.extend(["N/A"; 6]);
        line(ts, &v)
    };
    let expected = report(&[
        header(
            "00:00:00",
            "       TTY   rcvin/s   xmtin/s framerr/s prtyerr/s     brk/s   ovrun/s",
        ),
        // rx は 0xfffffda8 → 0 で 32bit の 600。TTY は line - 1
        line(
            "00:10:00",
            &["0", "1.00", "2.00", "3.00", "4.00", "5.00", "6.00"],
        ),
        // 回線番号が 2 → 3 に変わった枠
        na("00:10:00", "2"),
        // line = 0 の枠は出さない
        line(
            "00:20:00",
            &["0", "2.00", "4.00", "6.00", "8.00", "10.00", "12.00"],
        ),
        line(
            "00:20:00",
            &["2", "1.00", "1.00", "1.00", "1.00", "1.00", "1.00"],
        ),
        na("00:20:00", "4"),
        // 平均は R0 と比べる: 回線番号が同じなのは先頭だけ
        line(
            "Average:",
            &["0", "1.50", "3.00", "4.50", "6.00", "7.50", "9.00"],
        ),
        na("Average:", "2"),
        na("Average:", "4"),
    ]);
    assert_eq!(run(&["-y", "-t"], &file), expected);
}

/// `-I` はビットマップで行を選ぶ (0 番のビットが合計 `sum`、i 番が割り込み i - 1)。
/// `ll_s_value()` なので逆行した区間は 0.00。行数はファイルの item 数で頭打ち。
#[test]
fn irq_rows_follow_the_bitmap() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = samples(&[A_IRQ.nr(5)], 3, |k| {
        let v: [u64; 5] = match k {
            0 => [1000, 100, 200, 300, 400],
            1 => [7000, 700, 1400, 200, 3400],
            _ => [19_000, 1900, 3800, 800, 9400],
        };
        v.iter().map(|&n| irq(n)).collect()
    });
    let file = write(dir.path(), "el7-irq", bytes);
    let hdr = || header("00:00:00", "      INTR    intr/s");
    // 各行 (割り込み, R1, R2, 平均)。平均は R0→R2 を 1200 秒で割る
    let rows: [(&str, [&str; 3]); 5] = [
        ("sum", ["10.00", "20.00", "15.00"]),
        ("0", ["1.00", "2.00", "1.50"]),
        ("1", ["2.00", "4.00", "3.00"]),
        // 300 → 200 に逆行した区間は 0.00、平均は 500 / 1200 = 0.42
        ("2", ["0.00", "1.00", "0.42"]),
        ("3", ["5.00", "10.00", "7.50"]),
    ];
    let expect = |pick: &[usize]| {
        let mut lines = vec![hdr()];
        for (n, ts) in ["00:10:00", "00:20:00", "Average:"].iter().enumerate() {
            for &i in pick {
                lines.push(line(ts, &[rows[i].0, rows[i].1[n]]));
            }
        }
        report(&lines)
    };
    assert_eq!(run(&["-I", "SUM", "-t"], &file), expect(&[0]));
    assert_eq!(run(&["-I", "1,3", "-t"], &file), expect(&[2, 4]));
    assert_eq!(run(&["-I", "SUM,2", "-t"], &file), expect(&[0, 3]));
    // XALL は合計以外の全部、ALL は先頭 16 本 (どちらもファイルにある 4 本だけ出る)
    assert_eq!(run(&["-I", "XALL", "-t"], &file), expect(&[1, 2, 3, 4]));
    assert_eq!(run(&["-I", "ALL", "-t"], &file), expect(&[1, 2, 3, 4]));
}

// ===========================================================================
// 電源管理 (-m)
// ===========================================================================

/// `-m CPU` の MHz は `cpufreq / 100`、平均は `合計 / (100 × avg_count)`。
/// 行は CPU のビットマップで選ぶ (既定は "all" だけ)。周波数 0 の CPU も 0.00 で出る。
#[test]
fn cpu_frequency_rows_follow_the_cpu_bitmap() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = samples(&[A_PWR_CPUFREQ.nr(3)], 3, |k| {
        let v: [u64; 3] = match k {
            0 => [999_999; 3],
            1 => [250_000, 300_000, 0],
            _ => [150_050, 100_004, 200_000],
        };
        v.iter().map(|&f| cpufreq(f)).collect()
    });
    let file = write(dir.path(), "el7-cpufreq", bytes);
    let hdr = || header("00:00:00", "     CPU       MHz");
    // (識別子, R1, R2, 平均)。平均: all 400050 / 200、CPU 0 400004 / 200、CPU 1 200000 / 200
    let rows: [(&str, [&str; 3]); 3] = [
        ("     all", ["2500.00", "1500.50", "2000.25"]),
        ("       0", ["3000.00", "1000.04", "2000.02"]),
        ("       1", ["0.00", "2000.00", "1000.00"]),
    ];
    let expect = |pick: &[usize]| {
        let mut lines = vec![hdr()];
        for (n, ts) in ["00:10:00", "00:20:00", "Average:"].iter().enumerate() {
            for &i in pick {
                lines.push(line_at(ts, rows[i].0, &[rows[i].1[n]]));
            }
        }
        report(&lines)
    };
    assert_eq!(run(&["-m", "CPU", "-t"], &file), expect(&[0]));
    assert_eq!(
        run(&["-m", "CPU", "-P", "ALL", "-t"], &file),
        expect(&[0, 1, 2])
    );
    assert_eq!(run(&["-m", "CPU", "-P", "1", "-t"], &file), expect(&[2]));
}

/// `-m FREQ` は time_in_state の増分で重みを付けた MHz (`freq / 1000` は整数除算)。
/// 周波数 0 の枠で打ち切る。平均行は区間の最初 (R0) からの増分で重みを付ける。
#[test]
fn weighted_frequency_rows_weight_by_time_in_state() {
    let dir = tempfile::tempdir().unwrap();
    // CPU ごとに 3 枠 (all: 2000 MHz / 1000 MHz / 打ち切り、
    // CPU 0: 1999.999 MHz / 1200 MHz / 800 MHz、CPU 1: 周波数 0 のみ)
    let bytes = samples(&[A_PWR_WGHFREQ.nr(3).nr2(3)], 3, |k| {
        let (all, cpu0): ([u64; 3], [u64; 3]) = match k {
            0 => ([0, 0, 0], [0, 0, 0]),
            1 => ([100, 300, 999], [50, 50, 100]),
            _ => ([400, 400, 999], [150, 50, 100]),
        };
        vec![
            wghfreq(all[0], 2_000_000),
            wghfreq(all[1], 1_000_000),
            wghfreq(all[2], 0),
            wghfreq(cpu0[0], 1_999_999),
            wghfreq(cpu0[1], 1_200_000),
            wghfreq(cpu0[2], 800_000),
            wghfreq(0, 0),
            wghfreq(0, 0),
            wghfreq(0, 0),
        ]
    });
    let file = write(dir.path(), "el7-wghfreq", bytes);
    let hdr = || header("00:00:00", "     CPU    wghMHz");
    let rows: [(&str, [&str; 3]); 3] = [
        // (2000 × 100 + 1000 × 300) / 400、(2000 × 300 + 1000 × 100) / 400、
        // 平均 (2000 × 400 + 1000 × 400) / 800
        ("     all", ["1250.00", "1750.00", "1500.00"]),
        // (1999 × 50 + 1200 × 50 + 800 × 100) / 200、1999 × 100 / 100、
        // 平均 (1999 × 150 + 1200 × 50 + 800 × 100) / 300 = 1466.17
        ("       0", ["1199.75", "1999.00", "1466.17"]),
        // 先頭の周波数が 0 なら重みも 0 で 0.00
        ("       1", ["0.00", "0.00", "0.00"]),
    ];
    let expect = |pick: &[usize]| {
        let mut lines = vec![hdr()];
        for (n, ts) in ["00:10:00", "00:20:00", "Average:"].iter().enumerate() {
            for &i in pick {
                lines.push(line_at(ts, rows[i].0, &[rows[i].1[n]]));
            }
        }
        report(&lines)
    };
    assert_eq!(run(&["-m", "FREQ", "-t"], &file), expect(&[0]));
    assert_eq!(
        run(&["-m", "FREQ", "-P", "ALL", "-t"], &file),
        expect(&[0, 1, 2])
    );
}

/// センサ (`-m FAN` / `-m TEMP` / `-m IN`) の瞬時値と平均。
///
/// - FAN の drpm = rpm - rpm_min、平均は (Σrpm - Σrpm_min) / avg_count
/// - TEMP / IN の % は (値 - 最小) / (最大 - 最小)。平均は**最後のサンプルの**最小・最大で割る
/// - 番号は FAN / TEMP が 1 から、IN が 0 から。デバイス名は 20 桁の右寄せ
#[test]
fn sensor_rows_follow_the_el7_formulas() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = samples(
        &[A_PWR_FAN.nr(2), A_PWR_TEMP, A_PWR_IN.nr(2)],
        3,
        |k| match k {
            0 => vec![
                fan(1.0, 1.0, "fan-a"),
                fan(1.0, 1.0, "fan-b"),
                sensor(1.0, 0.0, 2.0, "cpu-temp"),
                sensor(1.0, 0.0, 2.0, "in0"),
                sensor(1.0, 0.0, 2.0, "in1"),
            ],
            1 => vec![
                fan(1200.0, 1000.0, "fan-a"),
                fan(800.25, 800.0, "fan-b"),
                sensor(45.0, 20.0, 70.0, "cpu-temp"),
                sensor(12.0, 11.0, 13.0, "in0"),
                sensor(3.3, 3.3, 3.3, "in1"),
            ],
            _ => vec![
                fan(1300.0, 1000.0, "fan-a"),
                fan(800.0, 800.0, "fan-b"),
                sensor(57.0, 20.0, 100.0, "cpu-temp"),
                sensor(12.5, 11.0, 13.0, "in0"),
                sensor(3.3, 3.3, 3.3, "in1"),
            ],
        },
    );
    let file = write(dir.path(), "el7-sensor", bytes);
    let device = format!(" {:>20}", "DEVICE");
    let row = |ts: &str, n: &str, cells: [&str; 2], dev: &str| {
        format!("{} {dev:>20}", line_at(ts, &format!("     {n:>3}"), &cells))
    };

    assert_eq!(
        run(&["-m", "FAN", "-t"], &file),
        report(&[
            header("00:00:00", &format!("     FAN       rpm      drpm{device}")),
            row("00:10:00", "1", ["1200.00", "200.00"], "fan-a"),
            row("00:10:00", "2", ["800.25", "0.25"], "fan-b"),
            row("00:20:00", "1", ["1300.00", "300.00"], "fan-a"),
            row("00:20:00", "2", ["800.00", "0.00"], "fan-b"),
            row("Average:", "1", ["1250.00", "250.00"], "fan-a"),
            // 1600.25 / 2 = 800.125、0.25 / 2 = 0.125 はどちらも 2 進で割り切れる
            // ちょうど中間の値で、%.2f は偶数丸め
            row("Average:", "2", ["800.12", "0.12"], "fan-b"),
        ])
    );

    assert_eq!(
        run(&["-m", "TEMP", "-t"], &file),
        report(&[
            header("00:00:00", &format!("    TEMP      degC     %temp{device}")),
            // (45 - 20) / (70 - 20)
            row("00:10:00", "1", ["45.00", "50.00"], "cpu-temp"),
            // (57 - 20) / (100 - 20)
            row("00:20:00", "1", ["57.00", "46.25"], "cpu-temp"),
            // (51 - 20) / (100 - 20) = 38.75 (最初のサンプルの最大 70 なら 62.00)
            row("Average:", "1", ["51.00", "38.75"], "cpu-temp"),
        ])
    );

    assert_eq!(
        run(&["-m", "IN", "-t"], &file),
        report(&[
            header("00:00:00", &format!("      IN       inV       %in{device}")),
            row("00:10:00", "0", ["12.00", "50.00"], "in0"),
            // 最大 = 最小なら 0.00 (0 で割らない)
            row("00:10:00", "1", ["3.30", "0.00"], "in1"),
            row("00:20:00", "0", ["12.50", "75.00"], "in0"),
            row("00:20:00", "1", ["3.30", "0.00"], "in1"),
            // (12.25 - 11) / (13 - 11)
            row("Average:", "0", ["12.25", "62.50"], "in0"),
            row("Average:", "1", ["3.30", "0.00"], "in1"),
        ])
    );
}

/// USB の 1 行 (`"  %6d %9x %9x %9u"` + 製造元 23 桁 + 製品名 47 桁)。
fn usb_line(
    ts: &str,
    bus: &str,
    id: [&str; 2],
    maxpower: &str,
    manufacturer: &str,
    product: &str,
) -> String {
    format!(
        "{ts:<11}  {bus:>6} {:>9} {:>9} {maxpower:>9} {manufacturer:>23} {product:>47}",
        id[0], id[1]
    )
}

/// `-m USB` の要約 (`Summary`) は区間の最初のサンプル (本家の `buf[2]`) を要約リストにして、
/// 表示した装置を足していく。空きが無くなると最後の枠を「その他」(バス番号 -1) で上書きする。
#[test]
fn usb_summary_lists_devices_seen_in_the_interval() {
    let dir = tempfile::tempdir().unwrap();
    let hub = || {
        usb(
            1,
            0x1d6b,
            0x0002,
            0,
            "Linux Foundation",
            "xHCI Host Controller",
        )
    };
    let receiver = || usb(2, 0x046d, 0xc52b, 49, "Logitech", "USB Receiver");
    let empty = || usb(0, 0, 0, 0, "", "");
    let bytes = samples(&[A_PWR_USB.nr(3)], 3, |k| match k {
        0 => vec![hub(), empty(), empty()],
        1 => vec![hub(), receiver(), empty()],
        _ => vec![
            usb(3, 0x0781, 0x5581, 250, "SanDisk", "Ultra"),
            usb(4, 0x8087, 0x0024, 0, "", "Rate Matching Hub"),
            empty(),
        ],
    });
    let file = write(dir.path(), "el7-usb", bytes);
    let hdr = |ts: &str| {
        header(
            ts,
            &format!(
                "     BUS  idvendor    idprod  maxpower {:>23} {:>47}",
                "manufact", "product"
            ),
        )
    };
    // maxpower は bMaxPower × 2 (mA)
    let hub_at = |ts: &str| {
        usb_line(
            ts,
            "1",
            ["1d6b", "2"],
            "0",
            "Linux Foundation",
            "xHCI Host Controller",
        )
    };
    let receiver_at =
        |ts: &str| usb_line(ts, "2", ["46d", "c52b"], "98", "Logitech", "USB Receiver");
    assert_eq!(
        run(&["-m", "USB", "-t"], &file),
        report(&[
            hdr("00:00:00"),
            hub_at("00:10:00"),
            receiver_at("00:10:00"),
            usb_line("00:20:00", "3", ["781", "5581"], "500", "SanDisk", "Ultra"),
            usb_line(
                "00:20:00",
                "4",
                ["8087", "24"],
                "0",
                "",
                "Rate Matching Hub"
            ),
            // 要約: R0 の hub、R1 で足した receiver、最後の枠は R2 の SanDisk を入れた後、
            // Rate Matching Hub の置き場が無くて「その他」で上書きされる
            hub_at("Summary"),
            receiver_at("Summary"),
            usb_line(
                "Summary",
                "-1",
                ["0", "0"],
                "0",
                "",
                "Other devices not listed here"
            ),
        ])
    );
    // count を使い切ると最後の反復で見出しの印が立ったまま平均へ進むので、
    // 要約にも見出しが付く (時刻欄は "Summary")
    assert_eq!(
        run(&["-m", "USB", "-t", "1", "1"], &file),
        report(&[
            hdr("00:00:00"),
            hub_at("00:10:00"),
            receiver_at("00:10:00"),
            hdr("Summary"),
            hub_at("Summary"),
            receiver_at("Summary"),
        ])
    );
}

/// `-F` / `-F MOUNT` の値と要約。要約リストは同じファイルシステム名の枠を最新の値で
/// 上書きし、無ければ空き枠に足す。要約の見出しは `Summary:`、行は `Summary`。
#[test]
fn filesystem_summary_keeps_the_latest_values() {
    const GIB: u64 = 1 << 30;
    const MIB: u64 = 1 << 20;
    let dir = tempfile::tempdir().unwrap();
    let root = |bfree, bavail, ffree| {
        filesystem([10 * GIB, bfree, bavail], 655_360, ffree, "/dev/sda1", "/")
    };
    let data = |bfree, bavail| filesystem([GIB, bfree, bavail], 0, 0, "/dev/sdb1", "/data");
    let empty = || filesystem([0; 3], 0, 0, "", "");
    let bytes = samples(&[A_FILESYSTEM.nr(3)], 3, |k| match k {
        0 => vec![root(4 * GIB, 3 * GIB, 600_000), empty(), empty()],
        1 => vec![
            root(3 * GIB, 2 * GIB, 590_000),
            data(512 * MIB, 512 * MIB),
            empty(),
        ],
        _ => vec![
            data(256 * MIB, 128 * MIB),
            filesystem([2 * GIB, GIB, GIB], 1000, 250, "/dev/sdc1", "/backup"),
            empty(),
        ],
    });
    let file = write(dir.path(), "el7-fs", bytes);
    let hdr = |ts: &str, what: &str| {
        header(
            ts,
            &format!(
                "  MBfsfree  MBfsused   %fsused  %ufsused     Ifree     Iused    %Iused {what}"
            ),
        )
    };
    let row = |ts: &str, cells: [&str; 7], name: &str| format!("{} {name}", line(ts, &cells));
    // MB は f_bfree / 1024 / 1024、%fsused = (blocks - bfree) / blocks、
    // %ufsused = (blocks - bavail) / blocks、%Iused = (files - ffree) / files
    let root1 = ["3072", "7168", "70.00", "80.00", "590000", "65360", "9.97"];
    let data1 = ["512", "512", "50.00", "50.00", "0", "0", "0.00"];
    // f_files = 0 なら %Iused は 0.00 (0 で割らない)
    let data2 = ["256", "768", "75.00", "87.50", "0", "0", "0.00"];
    let backup2 = ["1024", "1024", "50.00", "50.00", "250", "750", "75.00"];
    let expect = |by_mount: bool| {
        let (what, n) = if by_mount {
            ("MOUNTPOINT", ["/", "/data", "/backup"])
        } else {
            ("FILESYSTEM", ["/dev/sda1", "/dev/sdb1", "/dev/sdc1"])
        };
        report(&[
            hdr("00:00:00", what),
            row("00:10:00", root1, n[0]),
            row("00:10:00", data1, n[1]),
            row("00:20:00", data2, n[1]),
            row("00:20:00", backup2, n[2]),
            // 要約: R0 の枠を R1 の値で上書き、sdb1 は R2 の値、sdc1 は空き枠へ
            row("Summary", root1, n[0]),
            row("Summary", data2, n[1]),
            row("Summary", backup2, n[2]),
        ])
    };
    assert_eq!(run(&["-F", "-t"], &file), expect(false));
    assert_eq!(run(&["-F", "MOUNT", "-t"], &file), expect(true));
    // count を使い切ると要約にも見出しが付く (見出しだけ "Summary:")
    assert_eq!(
        run(&["-F", "-t", "1", "1"], &file),
        report(&[
            hdr("00:00:00", "FILESYSTEM"),
            row("00:10:00", root1, "/dev/sda1"),
            row("00:10:00", data1, "/dev/sdb1"),
            hdr("Summary:", "FILESYSTEM"),
            row("Summary", root1, "/dev/sda1"),
            row("Summary", data1, "/dev/sdb1"),
        ])
    );
}

// ===========================================================================
// 特殊レコード
// ===========================================================================

#[test]
fn restart_line_has_no_cpu_count_and_comments_are_counted() {
    let mut recs = vec![
        stats(
            200_000,
            100_000,
            (0, 0, 0),
            &[
                cpu(0, 0, 200_000, 0),
                cpu(0, 0, 100_000, 0),
                cpu(0, 0, 100_000, 0),
            ],
        ),
        stats(
            320_000,
            160_000,
            (0, 10, 0),
            &[
                cpu(0, 0, 320_000, 0),
                cpu(0, 0, 160_000, 0),
                cpu(0, 0, 160_000, 0),
            ],
        ),
        Rec::Comment {
            hms: (0, 15, 0),
            text: "hello el7",
        },
        Rec::Restart { hms: (0, 20, 0) },
    ];
    recs.push(stats(
        1000,
        500,
        (0, 30, 0),
        &[cpu(0, 0, 1000, 0), cpu(0, 0, 500, 0), cpu(0, 0, 500, 0)],
    ));
    recs.push(stats(
        121_000,
        60_500,
        (0, 40, 0),
        &[
            cpu(0, 0, 121_000, 0),
            cpu(0, 0, 60_500, 0),
            cpu(0, 0, 60_500, 0),
        ],
    ));
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "el7-restart", build(&[A_CPU], &recs));
    let out = run(&["-u", "-C", "-t"], &file);
    let expected = "\
Linux 3.10.0-el7 (testhost) \t09/13/20 \t_x86_64_\t(2 CPU)

00:00:00        CPU     %user     %nice   %system   %iowait    %steal     %idle
00:10:00        all      0.00      0.00      0.00      0.00      0.00    100.00
00:15:00     COM hello el7
Average:        all      0.00      0.00      0.00      0.00      0.00    100.00

00:20:00          LINUX RESTART

00:30:00        CPU     %user     %nice   %system   %iowait    %steal     %idle
00:40:00        all      0.00      0.00      0.00      0.00      0.00    100.00
Average:        all      0.00      0.00      0.00      0.00      0.00    100.00
";
    assert_eq!(out, expected);
}

// ===========================================================================
// 時刻の絞り込み・間隔・件数
// ===========================================================================

/// CPU だけの 1 日分。先頭と途中に COMMENT、途中に RESTART がある。
///
/// ```text
/// COM boot 00:00  R0 00:00  R1 00:10  R2 00:20  COM mid 00:25  R3 00:30
/// RESTART 00:40   R4 00:50  R5 01:00  R6 01:10
/// ```
fn timeline_fixture() -> Vec<u8> {
    // 再起動後は uptime が小さい値から数え直す
    let rebooted = |j: u64, hms| {
        let uptime0 = 500 + j * 60_000;
        let uptime = 2 * uptime0;
        stats(
            uptime,
            uptime0,
            hms,
            &[
                cpu(0, 0, uptime, 0),
                cpu(0, 0, uptime0, 0),
                cpu(0, 0, uptime0, 0),
            ],
        )
    };
    build(
        &[A_CPU],
        &[
            Rec::Comment {
                hms: (0, 0, 0),
                text: "boot",
            },
            sample(0, &[]),
            sample(1, &[]),
            sample(2, &[]),
            Rec::Comment {
                hms: (0, 25, 0),
                text: "mid",
            },
            sample(3, &[]),
            Rec::Restart { hms: (0, 40, 0) },
            rebooted(0, (0, 50, 0)),
            rebooted(1, (1, 0, 0)),
            rebooted(2, (1, 10, 0)),
        ],
    )
}

/// `-s` / `-e` の範囲外の統計・COMMENT・RESTART は出さない。
/// 範囲の最初の統計レコードは区間の基準になり、表示しない。`-e` は境界を含む。
#[test]
fn start_and_end_times_select_records_and_special_lines() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "el7-timeline", timeline_fixture());
    let window = ["-u", "-t", "-s", "00:10:00", "-e", "00:30:00"];
    assert_eq!(
        run(&window, &file),
        report(&[
            header("00:10:00", CPU_HEADER),
            idle_all("00:20:00"),
            idle_all("00:30:00"),
            idle_all("Average:"),
        ])
    );
    // -C の COMMENT も範囲内のものだけ (先頭の boot は -s より前)
    let mut with_comments = window.to_vec();
    with_comments.push("-C");
    assert_eq!(
        run(&with_comments, &file),
        report(&[
            header("00:10:00", CPU_HEADER),
            idle_all("00:20:00"),
            "00:25:00     COM mid".into(),
            idle_all("00:30:00"),
            idle_all("Average:"),
        ])
    );
    // 範囲にレコードが 1 つも無ければバナーだけ
    assert_eq!(run(&["-u", "-t", "-s", "05:00:00"], &file), report(&[]));
}

/// count を使い切ったら次の RESTART まで読み飛ばす。読み飛ばす途中の COMMENT は出し、
/// RESTART の後は count を数え直す。区間の前の COMMENT はバナーの直後に出る。
///
/// count で止まると、見出しを出す印 (`dis`) は最後に表示した反復のまま平均行へ
/// 進む。1 サンプル目で止まれば印が立っているので、平均行にも見出しが付く。
#[test]
fn count_skips_to_the_next_restart_and_starts_over() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "el7-timeline", timeline_fixture());
    assert_eq!(
        run(&["-u", "-t", "-C", "1", "1"], &file),
        report(&[
            "00:00:00     COM boot".into(),
            header("00:00:00", CPU_HEADER),
            idle_all("00:10:00"),
            header("Average:", CPU_HEADER),
            idle_all("Average:"),
            "00:25:00     COM mid".into(),
            "\n00:40:00          LINUX RESTART".into(),
            header("00:50:00", CPU_HEADER),
            idle_all("01:00:00"),
            header("Average:", CPU_HEADER),
            idle_all("Average:"),
        ])
    );
    // 2 サンプル目で止まれば印は下りているので、平均行に見出しは付かない
    assert_eq!(
        run(&["-u", "-t", "1", "2"], &file),
        report(&[
            header("00:00:00", CPU_HEADER),
            idle_all("00:10:00"),
            idle_all("00:20:00"),
            idle_all("Average:"),
            "\n00:40:00          LINUX RESTART".into(),
            header("00:50:00", CPU_HEADER),
            idle_all("01:00:00"),
            idle_all("01:10:00"),
            idle_all("Average:"),
        ])
    );
}

/// `-i` はファイルの記録間隔 (600 秒) の倍数に近いレコードだけを出す (`next_slice()`)。
/// 見出しの時刻は区間の最初 (最後に表示したサンプル) で、飛ばしたレコードではない。
#[test]
fn interval_keeps_records_near_its_multiples() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "el7-timeline", timeline_fixture());
    assert_eq!(
        run(&["-u", "-t", "-i", "1200"], &file),
        report(&[
            header("00:00:00", CPU_HEADER),
            idle_all("00:20:00"),
            idle_all("Average:"),
            "\n00:40:00          LINUX RESTART".into(),
            header("00:50:00", CPU_HEADER),
            idle_all("01:10:00"),
            idle_all("Average:"),
        ])
    );
}

/// `-t` を付けなければ `localtime()` の時刻で出す。バナーの日付もローカルになる
/// (2020-09-13 00:00 UTC は米国東部の夏時間で 09/12 20:00)。
#[test]
fn local_time_is_used_without_t() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "el7-timeline", timeline_fixture());
    let out = run_in(&["-u", "-C"], &file, "America/New_York");
    let expected = [
        "Linux 3.10.0-el7 (testhost) \t09/12/20 \t_x86_64_\t(2 CPU)".to_string(),
        "20:00:00     COM boot".into(),
        header("20:00:00", CPU_HEADER),
        idle_all("20:10:00"),
        idle_all("20:20:00"),
        "20:25:00     COM mid".into(),
        idle_all("20:30:00"),
        idle_all("Average:"),
        "\n20:40:00          LINUX RESTART".into(),
        header("20:50:00", CPU_HEADER),
        idle_all("21:00:00"),
        idle_all("21:10:00"),
        idle_all("Average:"),
    ]
    .join("\n")
        + "\n";
    assert_eq!(out, expected);
    // -s / -e もローカル時刻で比べる
    assert_eq!(
        run_in(
            &["-u", "-s", "20:15:00", "-e", "20:30:00"],
            &file,
            "America/New_York"
        ),
        [
            "Linux 3.10.0-el7 (testhost) \t09/12/20 \t_x86_64_\t(2 CPU)".to_string(),
            header("20:20:00", CPU_HEADER),
            idle_all("20:30:00"),
            idle_all("Average:"),
        ]
        .join("\n")
            + "\n"
    );
}

/// 日付を跨ぐファイル。`-s` を指定していると、時が前より小さくなった時点から
/// 24 時を足して比べる (`cross_day`)。最初の統計レコードを探す段階では足さない。
#[test]
fn start_time_crosses_midnight_once_the_hour_goes_back() {
    let dir = tempfile::tempdir().unwrap();
    let rec = |k: u64, day, hms| {
        let uptime0 = 100_000 + k * 60_000;
        let uptime = 2 * uptime0;
        stats_on(
            day,
            uptime,
            uptime0,
            hms,
            &[
                cpu(0, 0, uptime, 0),
                cpu(0, 0, uptime0, 0),
                cpu(0, 0, uptime0, 0),
            ],
        )
    };
    let bytes = build(
        &[A_CPU],
        &[
            rec(0, 0, (23, 40, 0)),
            rec(1, 0, (23, 50, 0)),
            rec(2, 1, (0, 0, 0)),
            rec(3, 1, (0, 10, 0)),
        ],
    );
    let file = write(dir.path(), "el7-crossday", bytes);
    // 00:00 と 00:10 は 24:00 と 24:10 として -s 23:45 より後になる
    assert_eq!(
        run(&["-u", "-t", "-s", "23:45:00"], &file),
        report(&[
            header("23:50:00", CPU_HEADER),
            idle_all("00:00:00"),
            idle_all("00:10:00"),
            idle_all("Average:"),
        ])
    );
    // -e 00:05:00 は -s より前なので 24:05:00 になり、24:10 で count を 0 にして止まる
    assert_eq!(
        run(&["-u", "-t", "-s", "23:45:00", "-e", "00:05:00"], &file),
        report(&[
            header("23:50:00", CPU_HEADER),
            idle_all("00:00:00"),
            idle_all("Average:"),
        ])
    );
}

// ===========================================================================
// -A (全 activity)
// ===========================================================================

/// el7 が知っている 37 activity をすべて持つファイル。
///
/// 1 レコードの中で activity が宣言の大きさどおりに並んでいないと、後ろの activity が
/// ずれて読まれる。値は各 activity のテストと同じ形のものを小さく入れてある。
fn all_activities_fixture() -> Vec<u8> {
    let mut acts = vec![
        A_PCSW,
        A_IRQ.nr(3),
        A_PAGE,
        A_MEMORY,
        A_KTABLES,
        A_QUEUE,
        A_SERIAL.nr(2),
        A_DISK.nr(2),
        A_NET_DEV.nr(2),
        A_NET_EDEV.nr(2),
        A_NET_SOCK,
        A_NET_SOCK6,
        A_PWR_CPUFREQ.nr(3),
        A_PWR_FAN,
        A_PWR_TEMP,
        A_PWR_IN,
        A_HUGE,
        A_PWR_WGHFREQ.nr(3).nr2(2),
        A_PWR_USB.nr(2),
        A_FILESYSTEM.nr(2),
    ];
    acts.extend(RATE_CASES.iter().map(|c| c.act));
    // ファイル上の並びは本家と同じく id 順
    acts.sort_by_key(|a| a.id);
    samples(&acts, 3, |k| {
        let n = k + 1;
        acts.iter()
            .flat_map(|a| -> Vec<Vec<u8>> {
                match a.id {
                    2 => vec![pcsw(10_000 * n, 100 * n)],
                    3 => (0..3).map(|i| irq(600 * n * (i + 1))).collect(),
                    5 => vec![counters(
                        64,
                        8,
                        &(0..8).map(|j| rate_value(8, j, k)).collect::<Vec<_>>(),
                    )],
                    7 => vec![memory([
                        400 + n,
                        10_000 * n,
                        5000,
                        1000,
                        60,
                        100,
                        14 + n,
                        299 + n,
                        10,
                        20,
                        30,
                    ])],
                    8 => vec![ktables(1000 + n as u32, 2000, 3000, n as u32)],
                    9 => vec![queue(n, 1, [100, 200, 300], 300 + n as u32)],
                    10 => vec![serial([600 * n as u32; 6], 1), serial([0; 6], 0)],
                    11 => vec![
                        disk((8, 0), 600 * n, (1200 * n, 1800 * n), [300 * n as u32; 4]),
                        disk((0, 0), 0, (0, 0), [0; 4]),
                    ],
                    12 => vec![net_dev("eth0", [600 * n; 7]), net_dev("", [0; 7])],
                    13 => vec![net_edev("eth0", [600 * n; 9]), net_edev("", [0; 9])],
                    16 => vec![sock([100, 20, 5, 7, 1, n as u32])],
                    24 => vec![sock6([10, 20, 30, n as u32])],
                    30 => (0..3).map(|i| cpufreq(100_000 * (i + n))).collect(),
                    31 => vec![fan(1000.0 * n as f64, 900.0, "fan1")],
                    32 => vec![sensor(40.0 + n as f64, 20.0, 60.0, "temp1")],
                    33 => vec![sensor(12.0, 11.0, 13.0, "in0")],
                    34 => vec![huge(n, 10)],
                    35 => (0..6)
                        .map(|i| wghfreq(100 * n * (i % 2 + 1), 1_000_000 * (i % 2 + 1)))
                        .collect(),
                    36 => vec![
                        usb(
                            1,
                            0x1d6b,
                            0x0002,
                            0,
                            "Linux Foundation",
                            "xHCI Host Controller",
                        ),
                        usb(0, 0, 0, 0, "", ""),
                    ],
                    37 => vec![
                        filesystem([1 << 30, 1 << 29, 1 << 28], 1000, 500, "/dev/sda1", "/"),
                        filesystem([0; 3], 0, 0, "", ""),
                    ],
                    id => {
                        let c = RATE_CASES.iter().find(|c| c.act.id == id).unwrap();
                        let v: Vec<u64> =
                            (0..c.fields).map(|j| rate_value(c.stride, j, k)).collect();
                        vec![counters(c.act.size as usize, c.stride, &v)]
                    }
                }
            })
            .collect()
    })
}

/// `-A` は全 activity をファイルの並び (id 順) に出し、A_MEMORY は `-R` → `-r` → `-S`
/// の順に 3 回出す。CPU は `-u ALL` の列、センサ・USB・FS は固有の見出しになる。
#[test]
fn a_prints_every_activity_in_file_order() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "el7-all", all_activities_fixture());
    let out = run(&["-A", "-t"], &file);
    // 見出しは空行の直後の行 (RESTART は無い)。時刻欄 (11 桁) より後ろを取り出す
    let lines: Vec<&str> = out.lines().collect();
    let headers: Vec<&str> = lines
        .windows(2)
        .filter(|w| w[0].is_empty())
        .map(|w| &w[1][11..])
        .collect();
    let rate = |id: u32| RATE_CASES.iter().find(|c| c.act.id == id).unwrap().header;
    let device = format!(" {:>20}", "DEVICE");
    let fan = format!("     FAN       rpm      drpm{device}");
    let temp = format!("    TEMP      degC     %temp{device}");
    let volt = format!("      IN       inV       %in{device}");
    let usb = format!(
        "     BUS  idvendor    idprod  maxpower {:>23} {:>47}",
        "manufact", "product"
    );
    let expected: Vec<&str> = vec![
        "     CPU      %usr     %nice      %sys   %iowait    %steal      %irq     %soft    %guest    %gnice     %idle",
        "    proc/s   cswch/s",
        "      INTR    intr/s",
        rate(4),
        "  pgpgin/s pgpgout/s   fault/s  majflt/s  pgfree/s pgscank/s pgscand/s pgsteal/s    %vmeff",
        rate(6),
        "   frmpg/s   bufpg/s   campg/s",
        " kbmemfree kbmemused  %memused kbbuffers  kbcached  kbcommit   %commit  kbactive   kbinact   kbdirty",
        " kbswpfree kbswpused  %swpused  kbswpcad   %swpcad",
        " dentunusd   file-nr  inode-nr    pty-nr",
        "   runq-sz  plist-sz   ldavg-1   ldavg-5  ldavg-15   blocked",
        "       TTY   rcvin/s   xmtin/s framerr/s prtyerr/s     brk/s   ovrun/s",
        "       DEV       tps  rd_sec/s  wr_sec/s  avgrq-sz  avgqu-sz     await     svctm     %util",
        "     IFACE   rxpck/s   txpck/s    rxkB/s    txkB/s   rxcmp/s   txcmp/s  rxmcst/s",
        "     IFACE   rxerr/s   txerr/s    coll/s  rxdrop/s  txdrop/s  txcarr/s  rxfram/s  rxfifo/s  txfifo/s",
        rate(14),
        rate(15),
        "    totsck    tcpsck    udpsck    rawsck   ip-frag    tcp-tw",
        rate(17),
        rate(18),
        rate(19),
        rate(20),
        rate(21),
        rate(22),
        rate(23),
        "   tcp6sck   udp6sck   raw6sck  ip6-frag",
        rate(25),
        rate(26),
        rate(27),
        rate(28),
        rate(29),
        "     CPU       MHz",
        &fan,
        &temp,
        &volt,
        " kbhugfree kbhugused  %hugused",
        "     CPU    wghMHz",
        &usb,
        "  MBfsfree  MBfsused   %fsused  %ufsused     Ifree     Iused    %Iused FILESYSTEM",
    ];
    assert_eq!(headers, expected, "{out}");
    // -A は CPU "all" と個別 CPU (-P ALL)、割り込みの合計と個別 (-I の全ビット) を出す
    for row in [
        "00:10:00        all",
        "00:10:00          0",
        "00:10:00          1",
        "00:10:00          sum",
        "00:10:00            1",
    ] {
        assert!(out.lines().any(|l| l.starts_with(row)), "{row}\n{out}");
    }
    // sa2sar は -A -C -t と同じ
    let sa2sar = command()
        .args(["sa2sar", "--sar-profile", "sysstat-10.1.5-el7"])
        .arg(&file)
        .output()
        .unwrap();
    assert!(sa2sar.status.success(), "{sa2sar:?}");
    assert_eq!(
        String::from_utf8(sa2sar.stdout).unwrap(),
        run(&["-A", "-C", "-t"], &file)
    );
}

// ===========================================================================
// 入口と誤りの扱い
// ===========================================================================

/// 選んだ activity がファイルに無ければ、本家と同じく何も出さずに失敗する (終了コード 1)。
#[test]
fn requested_activity_missing_from_the_file_fails() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "el7-cpu", cpu_fixture());
    let out = exec(&["-q", "-t"], &file, "UTC");
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(out.stdout.is_empty());
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(err.contains("Requested activities not available"), "{err}");
}

/// el7 が知らない id の activity は、宣言された大きさだけ読み飛ばす
/// (id 38 は後の版の `A_NET_FC`)。magic が違う activity も同じく読み飛ばし、
/// 他の activity はそのまま出る。
#[test]
fn unknown_activities_are_skipped_by_their_declared_size() {
    let dir = tempfile::tempdir().unwrap();
    let q = || queue(1, 2, [300, 200, 100], 40);
    let expected = report(&[
        header(
            "00:00:00",
            "   runq-sz  plist-sz   ldavg-1   ldavg-5  ldavg-15   blocked",
        ),
        line("00:10:00", &["1", "40", "3.00", "2.00", "1.00", "2"]),
        line("00:20:00", &["1", "40", "3.00", "2.00", "1.00", "2"]),
        line("Average:", &["1", "40", "3.00", "2.00", "1.00", "2"]),
    ]);
    let bytes = samples(&[decl(38, 0x8a, 24), A_QUEUE], 3, |_| {
        vec![vec![0xee; 24], q()]
    });
    let unknown_id = write(dir.path(), "el7-unknown-id", bytes);
    assert_eq!(run(&["-q", "-t"], &unknown_id), expected);

    // A_MEMORY の magic が違えば読み飛ばす (-q は影響を受けない)
    let bytes = samples(&[A_MEMORY.magic(0x8b), A_QUEUE], 3, |_| {
        vec![memory([7; 11]), q()]
    });
    let bad_magic = write(dir.path(), "el7-bad-magic", bytes);
    assert_eq!(run(&["-q", "-t"], &bad_magic), expected);
}

/// 選んだ activity がファイルにあっても、magic が違えば読めない。本家はこれを
/// 「ファイルにある」と数えて誤りにはせず、バナーと、最初の統計レコードより前の
/// COMMENT / RESTART だけを出して正常に終わる。
#[test]
fn selected_activity_with_an_unknown_magic_prints_only_the_banner() {
    let dir = tempfile::tempdir().unwrap();
    let q = || queue(1, 1, [1, 1, 1], 1);
    let mut recs = vec![Rec::Comment {
        hms: (0, 0, 0),
        text: "boot",
    }];
    recs.extend((0..2).map(|k| sample(k, &[q()])));
    recs.push(Rec::Comment {
        hms: (0, 15, 0),
        text: "later",
    });
    recs.push(sample(2, &[q()]));
    let file = write(
        dir.path(),
        "el7-queue-magic",
        build(&[A_CPU, A_QUEUE.magic(0x8a)], &recs),
    );
    assert_eq!(
        run(&["-q", "-C", "-t"], &file),
        report(&["00:00:00     COM boot".into()])
    );
    // 読める activity と一緒に選べば、そちらだけが出る
    assert_eq!(
        run(&["-q", "-u", "-t"], &file),
        report(&[
            header("00:00:00", CPU_HEADER),
            idle_all("00:10:00"),
            idle_all("00:20:00"),
            idle_all("Average:"),
        ])
    );
}

/// 読める A_CPU (el7 と同じ magic のもの) が無いファイルは読めない。
///
/// 本家は `handle_invalid_sa_file()` で終了コード 3 を返すが、reSARch は誤りを
/// 一律に 1 で返すので、ここは本家と終了コードを比べない。
#[test]
fn file_without_a_readable_cpu_activity_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let q = || queue(1, 1, [1, 1, 1], 1);
    let no_cpu = build(
        &[A_QUEUE],
        &(0..3)
            .map(|k| stats(200_000 + k, 100_000 + k, at(k), &[q()]))
            .collect::<Vec<_>>(),
    );
    let bad_cpu = build(
        &[A_CPU.magic(0x8b), A_QUEUE],
        &(0..3).map(|k| sample(k, &[q()])).collect::<Vec<_>>(),
    );
    for (name, bytes) in [("el7-no-cpu", no_cpu), ("el7-bad-cpu", bad_cpu)] {
        let file = write(dir.path(), name, bytes);
        let out = command()
            .args(["--sar-profile", "sysstat-10.1.5-el7", "-q", "-t", "-f"])
            .arg(&file)
            .output()
            .unwrap();
        assert!(!out.status.success(), "{name}: {out:?}");
        assert!(out.stdout.is_empty(), "{name}");
        let err = String::from_utf8(out.stderr).unwrap();
        assert!(
            err.contains("Invalid system activity file"),
            "{name}: {err}"
        );
    }
}

/// `sa2sar --sar-profile sysstat-10.1.5-el7` は el7 の `sar -A -C -t` と同じ。
#[test]
fn sa2sar_with_the_el7_profile_matches_sar_a() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "el7-mem", memory_fixture());
    let sa2sar = command()
        .args(["sa2sar", "--sar-profile", "sysstat-10.1.5-el7"])
        .arg(&file)
        .output()
        .unwrap();
    assert!(sa2sar.status.success(), "{sa2sar:?}");
    let sar = run(&["-A", "-C", "-t"], &file);
    assert_eq!(String::from_utf8(sa2sar.stdout).unwrap(), sar);
    // -A は -R / -r / -S の 3 ブロックをこの順に出す
    let r = sar.find("frmpg/s").unwrap();
    let m = sar.find("kbmemfree").unwrap();
    let s = sar.find("kbswpfree").unwrap();
    assert!(r < m && m < s, "{sar}");
}

#[test]
fn el7_profile_rejects_files_it_cannot_read() {
    use fixtures::{FixtureAbi, FixtureSpec, Generation, build as build_fixture};
    // 現行世代 (0x2175) は el7 の sar も読めない。黙って別の書式で出さずに止める。
    let dir = tempfile::tempdir().unwrap();
    let spec = FixtureSpec::minimal(Generation::G2175Current, FixtureAbi::Le64);
    let file = write(dir.path(), "current", build_fixture(spec).bytes);
    let out = command()
        .args(["--sar-profile", "sysstat-10.1.5-el7", "-u", "-f"])
        .arg(&file)
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(err.contains("0x2175") && err.contains("0x2171"), "{err}");
    // 既定 (現行版) なら読める
    let ok = command().args(["-u", "-f"]).arg(&file).output().unwrap();
    assert!(ok.status.success(), "{ok:?}");
}

#[test]
fn profile_options_are_validated() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "el7-cpu", cpu_fixture());
    let fail = |args: &[&str]| {
        let out = command().args(args).arg(&file).output().unwrap();
        assert!(!out.status.success(), "{args:?} は失敗するべき");
        String::from_utf8(out.stderr).unwrap()
    };
    // 省略形は受け付けない
    let err = fail(&["sa2sar", "--sar-profile", "10.1.5"]);
    assert!(err.contains("sysstat-10.1.5-el7"), "{err}");
    // ページサイズは el7 のときだけ
    let err = fail(&["sa2sar", "--sar-page-size", "4096"]);
    assert!(err.contains("--sar-page-size"), "{err}");
    let err = fail(&["--sar-page-size=4096", "-u", "-f"]);
    assert!(err.contains("--sar-page-size"), "{err}");
    // 2 の冪でないページサイズ
    let err = fail(&[
        "--sar-profile",
        "sysstat-10.1.5-el7",
        "--sar-page-size",
        "5000",
        "-R",
        "-f",
    ]);
    assert!(err.contains("5000"), "{err}");
    // sadf 互換出力はプロファイルを持たない
    let err = fail(&["sadf", "--sar-profile", "sysstat-10.1.5-el7", "-d"]);
    assert!(err.contains("--sar-profile"), "{err}");
    // el7 に無いオプション
    let err = fail(&["--sar-profile=sysstat-10.1.5-el7", "--dec=1", "-u", "-f"]);
    assert!(err.contains("--dec=1"), "{err}");
}

/// 既定 (現行版) の出力はプロファイルを明示しても変わらない。
#[test]
fn current_profile_is_the_default() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "el7-cpu", cpu_fixture());
    let plain = command()
        .args(["-u", "-t", "-f"])
        .arg(&file)
        .output()
        .unwrap();
    let current = command()
        .args(["--sar-profile", "current", "-u", "-t", "-f"])
        .arg(&file)
        .output()
        .unwrap();
    assert!(plain.status.success());
    assert_eq!(plain.stdout, current.stdout);
    // 現行版は CPU "all" を個別 CPU から作り直すので el7 と値が違う
    let el7 = run(&["-u", "-t"], &file);
    assert_ne!(String::from_utf8(plain.stdout).unwrap(), el7);
}

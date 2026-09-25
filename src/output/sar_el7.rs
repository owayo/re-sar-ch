//! RHEL / CentOS 7 の sysstat 10.1.5 (`sysstat-10.1.5-*.el7`) の `sar` のテキスト出力を再現する。
//!
//! [`crate::model::SarProfile::Sysstat1015El7`] を選んだときの `sar` / `sa2sar` の出力。
//! 現行版 (v12.8.0) の出力は [`crate::output::sar_text`] が持ち、経路を共有しない。
//! 描画の流れ (どのレコードを出すか・見出しの再表示・平均行・`LINUX RESTART` / `COM`
//! の出し方) が版ごとに違い、片方の修正がもう片方を黙って変えるのを避けるためである。
//!
//! 典拠は el7 の `sar.c` (`read_stats_from_file()` / `handle_curr_act_stats()` /
//! `write_stats()` / `write_stats_avg()` / `sar_print_special()`) と `pr_stats.c`。
//! 値の計算は [`crate::series::el7`] にあり、この層は書式化だけを行う。
//!
//! # 現行版との主な違い
//!
//! | 項目 | el7 (10.1.5) | 現行 (12.8.0) |
//! |---|---|---|
//! | 読めるファイル | `format_magic` 0x2171 だけ | 0x2175 (旧世代は `sadf -c` で変換) |
//! | CPU `all` 行 | ファイルの集約スロットを `record_header.uptime` の差で割る | 個別 CPU の合計から作り直す |
//! | オフライン CPU | 0.00 を並べ、現サンプルを前サンプルで上書き | 行を出さない |
//! | `LINUX RESTART` 行 | CPU 数を出さない | `(N CPU)` を出す |
//! | `COM` 行 | 本文をそのまま出す | 非印字文字を `.` にする |
//! | 見出しの再表示 | コメント行も 1 行と数える / `S_REPEAT_HEADER` なし | 先頭のコメントは数えない |
//! | 見出しの行数の数え方 | ビットマップ以外は `file_activity.nr` | レコードの item 数 |
//! | `-A` の中身 | `-R` を含み、`-r` / `-H` / `-d` / `-n DEV` などの列が少ない | `-r ALL` など |
//!
//! # ホストに依存する値
//!
//! 本家は `-R` の kB → ページ換算に**`sar` を実行したホスト**のページサイズを使う
//! ([`crate::model::PageSize`])。`-p` のデバイス名も実行ホストの `/sys` と
//! `sysstat.ioconf` を引くが、reSARch は他ホストのファイルで誤名になるのを避けて
//! 引かない。本家も Linux 以外のホストで読めば同じく `dev<major>-<minor>` になる。

use std::collections::BTreeSet;
use std::io::{self, Write};

use crate::format::SaFile;
use crate::format::file::{FileActivityEntry, ScanControl};
use crate::format::registry::RecordKind;
use crate::model::localtime::localtime;
use crate::model::{ActivityId, CompatDateFormat};
use crate::output::sar_text::{TimeStyle, banner_date, pad_left, pad_right};
use crate::series::el7::{
    self, Accum, AvgInfo, Bitmap, Buf, Cell, Decoder, Label, MemOutput, Print, PrintOptions, Row,
    Tail,
};
use crate::series::{RecordRange, Selection, WalkItem, walk_items_in};

/// el7 (10.1.5) の `FORMAT_MAGIC`。これ以外の世代は本家も読めない。
pub const FORMAT_MAGIC: u16 = 0x2171;

/// 行頭のタイムスタンプ列の幅 (`%-11s`)。
const TSW: usize = 11;

/// `-s` / `-e` の時刻 (`struct tstamp`)。
///
/// `-e` が `-s` より前の時刻なら本家は `tm_end.tm_hour += 24` するので、
/// `hour` は 24 以上にもなる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tstamp {
    pub hour: i32,
    pub min: i32,
    pub sec: i32,
}

/// el7 の `sar` の表示設定。CLI 層が解決済みの値を詰めて渡す。
#[derive(Debug, Clone)]
pub struct El7Options {
    /// 選択された activity。
    pub activities: BTreeSet<ActivityId>,
    /// activity ごとの表示フラグとビットマップ。
    pub print: PrintOptions,
    /// `-R` (`AO_F_MEM_DIA`)。
    pub mem_dia: bool,
    /// `-r` (`AO_F_MEM_AMT`)。
    pub mem_amt: bool,
    /// `-S` (`AO_F_MEM_SWAP`)。
    pub mem_swap: bool,
    /// `-C`。
    pub comment: bool,
    /// タイムスタンプの基準。`-t` は [`TimeStyle::Recorded`] (記録側ホストの時刻)、
    /// 既定は [`TimeStyle::Local`] (読み手のローカル時刻)。
    /// `sa2sar --utc` だけが [`TimeStyle::Utc`] を使う (本家の `TZ=UTC` と同じ)。
    pub time: TimeStyle,
    /// バナー行の日付 (`S_TIME_FORMAT`)。
    pub date_format: CompatDateFormat,
    /// 列見出しを出し直すまでの行数 (`get_win_height()`)。
    pub rows: u64,
    /// `-s`。
    pub tm_start: Option<Tstamp>,
    /// `-e`。
    pub tm_end: Option<Tstamp>,
    /// `-i` / positional の interval (ファイル読み出しでは 1 以上)。
    pub interval: i64,
    /// positional の count。`-1` は無制限。
    pub count: i64,
}

impl El7Options {
    /// `sar -A` (と `-C -t`) 相当。`sa2sar` はこれを使う。
    ///
    /// el7 の `-A` は全 activity を選び、`-P ALL` と `-I` の全ビット、
    /// `-u ALL`、メモリの `-R` / `-r` / `-S` を立てる (`parse_sar_opt()`)。
    pub fn all(kb_shift: u32) -> Self {
        let mut cpu_bitmap = Bitmap::cpu();
        cpu_bitmap.set_all();
        let mut irq_bitmap = Bitmap::irq();
        irq_bitmap.set_all();
        Self {
            activities: el7::SPECS.iter().map(|s| s.id).collect(),
            print: PrintOptions {
                cpu_all: true,
                cpu_bitmap,
                irq_bitmap,
                fs_mount: false,
                kb_shift,
            },
            mem_dia: true,
            mem_amt: true,
            mem_swap: true,
            comment: true,
            time: TimeStyle::Recorded,
            date_format: CompatDateFormat::default(),
            rows: u64::from(crate::model::DEFAULT_ROWS),
            tm_start: None,
            tm_end: None,
            interval: 1,
            count: -1,
        }
    }

    /// `A_MEMORY` の出力を本家のマスク順 (`-R` → `-r` → `-S`) で並べる。
    fn mem_outputs(&self) -> Vec<MemOutput> {
        let mut out = Vec::new();
        if self.mem_dia {
            out.push(MemOutput::Dia);
        }
        if self.mem_amt {
            out.push(MemOutput::Amt);
        }
        if self.mem_swap {
            out.push(MemOutput::Swap);
        }
        out
    }
}

// ============================================================================
// レコードの索引
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Stats,
    Restart,
    Comment,
}

/// 1 レコード分の `record_header` (と COMMENT の本文)。
#[derive(Debug, Clone)]
struct Rec {
    kind: Kind,
    ust_time: u64,
    hour: u8,
    minute: u8,
    second: u8,
    /// 全 CPU の jiffies の合計 (`uptime`)。
    uptime: u64,
    /// CPU 1 個分の jiffies (`uptime0`)。
    uptime0: u64,
    /// COMMENT の本文 (NUL の手前まで、最大 63 バイト)。
    comment: Vec<u8>,
}

/// 全レコードの `record_header` を読む。
///
/// 番号の振り方は [`walk_items_in`] と同じ (拡張・無効レコードは数えない)。
/// 統計データはデコードしないので、ファイルの大きさに比べて小さい。
fn index_records(file: &SaFile) -> crate::Result<Vec<Rec>> {
    let mut recs = Vec::new();
    file.scan(|rec| {
        let kind = match rec.kind {
            RecordKind::Restart => Kind::Restart,
            RecordKind::Comment => Kind::Comment,
            RecordKind::Extra(_) | RecordKind::Invalid(_) => return Ok(ScanControl::Continue),
            _ => Kind::Stats,
        };
        let (uptime, uptime0) = rec
            .uptime_jiffies
            .unwrap_or((0, rec.uptime_cs.unwrap_or(0)));
        recs.push(Rec {
            kind,
            ust_time: rec.ust_time,
            hour: rec.hour,
            minute: rec.minute,
            second: rec.second,
            uptime,
            uptime0,
            comment: rec.comment.map(<[u8]>::to_vec).unwrap_or_default(),
        });
        Ok(ScanControl::Continue)
    })?;
    Ok(recs)
}

// ============================================================================
// 時刻
// ============================================================================

/// `sar_get_record_timestamp_struct()` の時分秒。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RecTime {
    hour: i32,
    min: i32,
    sec: i32,
}

/// `-t` ならレコードに焼き込まれた時分秒、それ以外は `localtime()` (UTC 指定なら UTC)。
///
/// 変換できない時刻は `None` (本家の `localtime()` が `NULL` を返す場合)。
fn rectime(rec: &Rec, style: TimeStyle) -> Option<RecTime> {
    use chrono::{TimeZone, Timelike, Utc};
    fn hms<T: Timelike>(dt: &T) -> RecTime {
        RecTime {
            hour: dt.hour() as i32,
            min: dt.minute() as i32,
            sec: dt.second() as i32,
        }
    }
    let epoch = i64::try_from(rec.ust_time).ok();
    match style {
        TimeStyle::Recorded => Some(RecTime {
            hour: i32::from(rec.hour),
            min: i32::from(rec.minute),
            sec: i32::from(rec.second),
        }),
        TimeStyle::Utc | TimeStyle::Epoch => Some(hms(&Utc.timestamp_opt(epoch?, 0).single()?)),
        TimeStyle::Local => Some(hms(&localtime(epoch?)?)),
    }
}

/// `strftime("%X")` (C ロケール)。
fn time_text(rt: RecTime) -> String {
    format!("{:02}:{:02}:{:02}", rt.hour, rt.min, rt.sec)
}

/// `datecmp()`。
fn datecmp(rt: RecTime, t: Tstamp) -> i32 {
    if rt.hour == t.hour {
        if rt.min == t.min {
            rt.sec - t.sec
        } else {
            rt.min - t.min
        }
    } else {
        rt.hour - t.hour
    }
}

/// `next_slice()` (10.1.5)。uptime は CPU 1 個分の jiffies (`uptime0`)。
///
/// 現行版と違い、**`-i` を指定しなくても毎レコード呼ばれる**。記録間隔が
/// 0.5 秒未満のレコードは `interval = 1` でも表示されない (本家のまま)。
fn next_slice(
    uptime_ref: u64,
    uptime: u64,
    reset: bool,
    interval: i64,
    last_uptime: &mut u64,
) -> bool {
    if *last_uptime == 0 || reset {
        *last_uptime = uptime_ref;
    }
    let round = |f: f64| -> u64 {
        let mut v = f as u64;
        if f * 10.0 - (v.wrapping_mul(10)) as f64 >= 5.0 {
            v = v.wrapping_add(1);
        }
        v
    };
    let file_interval = round((uptime.wrapping_sub(*last_uptime) & 0xffff_ffff) as f64 / el7::HZ);
    *last_uptime = uptime;
    let entry = round((uptime.wrapping_sub(uptime_ref) & 0xffff_ffff) as f64 / el7::HZ);

    let min = entry.wrapping_sub(file_interval / 2) as i32;
    let max = entry
        .wrapping_add(file_interval / 2)
        .wrapping_add(file_interval & 1) as i32;
    // `entry / interval` は unsigned long 同士の割り算
    let iv = (interval as u64).max(1);
    let pt1 = (entry / iv).wrapping_mul(iv) as i32;
    let pt2 = (entry / iv).wrapping_add(1).wrapping_mul(iv) as i32;
    (pt1 >= min && pt1 < max) || (pt2 >= min && pt2 < max)
}

// ============================================================================
// 書式
// ============================================================================

/// `printf("%.*f")` 相当。glibc と同じく NaN は `nan` / `-nan`、無限大は `inf` / `-inf`。
fn c_float(v: f64, prec: usize) -> String {
    if v.is_nan() {
        return if v.is_sign_negative() { "-nan" } else { "nan" }.to_string();
    }
    if v.is_infinite() {
        return if v < 0.0 { "-inf" } else { "inf" }.to_string();
    }
    format!("{v:.prec$}")
}

fn push_cell(buf: &mut String, cell: &Cell) {
    match *cell {
        Cell::F92(v) => {
            buf.push(' ');
            buf.push_str(&pad_left(&c_float(v, 2), 9));
        }
        Cell::P62(v) => {
            buf.push_str("    ");
            buf.push_str(&pad_left(&c_float(v, 2), 6));
        }
        Cell::P72(v) => {
            buf.push_str("   ");
            buf.push_str(&pad_left(&c_float(v, 2), 7));
        }
        Cell::F90(v) => {
            buf.push(' ');
            buf.push_str(&pad_left(&c_float(v, 0), 9));
        }
        Cell::U9(v) => {
            buf.push(' ');
            buf.push_str(&pad_left(&v.to_string(), 9));
        }
        Cell::X9(v) => {
            buf.push(' ');
            buf.push_str(&pad_left(&format!("{v:x}"), 9));
        }
        Cell::Na => buf.push_str("       N/A"),
    }
}

fn push_label(buf: &mut String, label: &Label) {
    match label {
        Label::None => {}
        Label::All => buf.push_str("     all"),
        Label::Num3(n) => {
            buf.push_str("     ");
            buf.push_str(&pad_left(&n.to_string(), 3));
        }
        Label::Sum => buf.push_str("       sum"),
        Label::Num3Wide(n) => {
            buf.push_str("       ");
            buf.push_str(&pad_left(&n.to_string(), 3));
        }
        Label::Name(s) => {
            buf.push(' ');
            buf.push_str(&pad_left(s, 9));
        }
        Label::Bus(n) => {
            buf.push_str("  ");
            buf.push_str(&pad_left(&n.to_string(), 6));
        }
    }
}

fn push_tail(buf: &mut String, tail: &Tail) {
    match tail {
        Tail::None => {}
        Tail::Sensor(s) => {
            buf.push(' ');
            buf.push_str(&pad_left(s, el7::MAX_SENSORS_DEV_LEN));
        }
        Tail::Usb(manufacturer, product) => {
            buf.push(' ');
            buf.push_str(&pad_left(manufacturer, el7::USB_MANUF_WIDTH));
            buf.push(' ');
            buf.push_str(&pad_left(product, el7::USB_PROD_WIDTH));
        }
        Tail::Name(s) => {
            buf.push(' ');
            buf.push_str(s);
        }
    }
}

/// 行を 1 本書く (`%-11s` + 識別子 + 値 + 行末)。
fn push_row(buf: &mut String, ts: &str, row: &Row) {
    buf.push_str(&pad_right(ts, TSW));
    push_label(buf, &row.label);
    for cell in &row.cells {
        push_cell(buf, cell);
    }
    push_tail(buf, &row.tail);
    buf.push('\n');
}

/// 列見出しのタイムスタンプより後ろ (`pr_stats.c` の各 `printf`)。
fn header_text(id: ActivityId, mem: MemOutput, opts: &El7Options) -> String {
    let dev = pad_left("DEVICE", el7::MAX_SENSORS_DEV_LEN);
    match id {
        ActivityId::CPU if opts.print.cpu_all => "     CPU      %usr     %nice      %sys   %iowait    %steal      %irq     %soft    %guest    %gnice     %idle".into(),
        ActivityId::CPU => "     CPU     %user     %nice   %system   %iowait    %steal     %idle".into(),
        ActivityId::PCSW => "    proc/s   cswch/s".into(),
        ActivityId::IRQ => "      INTR    intr/s".into(),
        ActivityId::SWAP => "  pswpin/s pswpout/s".into(),
        ActivityId::PAGE => "  pgpgin/s pgpgout/s   fault/s  majflt/s  pgfree/s pgscank/s pgscand/s pgsteal/s    %vmeff".into(),
        ActivityId::IO => "       tps      rtps      wtps   bread/s   bwrtn/s".into(),
        ActivityId::MEMORY => match mem {
            MemOutput::Dia => "   frmpg/s   bufpg/s   campg/s".into(),
            MemOutput::Amt => " kbmemfree kbmemused  %memused kbbuffers  kbcached  kbcommit   %commit  kbactive   kbinact   kbdirty".into(),
            MemOutput::Swap => " kbswpfree kbswpused  %swpused  kbswpcad   %swpcad".into(),
        },
        ActivityId::KTABLES => " dentunusd   file-nr  inode-nr    pty-nr".into(),
        ActivityId::QUEUE => "   runq-sz  plist-sz   ldavg-1   ldavg-5  ldavg-15   blocked".into(),
        // activity.c の hdr_line は txmtin/s だが、画面に出す見出しは xmtin/s
        ActivityId::SERIAL => "       TTY   rcvin/s   xmtin/s framerr/s prtyerr/s     brk/s   ovrun/s".into(),
        ActivityId::DISK => "       DEV       tps  rd_sec/s  wr_sec/s  avgrq-sz  avgqu-sz     await     svctm     %util".into(),
        ActivityId::NET_DEV => "     IFACE   rxpck/s   txpck/s    rxkB/s    txkB/s   rxcmp/s   txcmp/s  rxmcst/s".into(),
        ActivityId::NET_EDEV => "     IFACE   rxerr/s   txerr/s    coll/s  rxdrop/s  txdrop/s  txcarr/s  rxfram/s  rxfifo/s  txfifo/s".into(),
        ActivityId::NET_NFS => "    call/s retrans/s    read/s   write/s  access/s  getatt/s".into(),
        ActivityId::NET_NFSD => "   scall/s badcall/s  packet/s     udp/s     tcp/s     hit/s    miss/s   sread/s  swrite/s saccess/s sgetatt/s".into(),
        ActivityId::NET_SOCK => "    totsck    tcpsck    udpsck    rawsck   ip-frag    tcp-tw".into(),
        ActivityId::NET_IP => "    irec/s  fwddgm/s    idel/s     orq/s   asmrq/s   asmok/s  fragok/s fragcrt/s".into(),
        ActivityId::NET_EIP => " ihdrerr/s iadrerr/s iukwnpr/s   idisc/s   odisc/s   onort/s    asmf/s   fragf/s".into(),
        ActivityId::NET_ICMP => "    imsg/s    omsg/s    iech/s   iechr/s    oech/s   oechr/s     itm/s    itmr/s     otm/s    otmr/s  iadrmk/s iadrmkr/s  oadrmk/s oadrmkr/s".into(),
        ActivityId::NET_EICMP => "    ierr/s    oerr/s idstunr/s odstunr/s   itmex/s   otmex/s iparmpb/s oparmpb/s   isrcq/s   osrcq/s  iredir/s  oredir/s".into(),
        ActivityId::NET_TCP => "  active/s passive/s    iseg/s    oseg/s".into(),
        // ETCP の再送列は retrans/s (現行版の retrseg/s ではない)
        ActivityId::NET_ETCP => "  atmptf/s  estres/s retrans/s isegerr/s   orsts/s".into(),
        ActivityId::NET_UDP => "    idgm/s    odgm/s  noport/s idgmerr/s".into(),
        ActivityId::NET_SOCK6 => "   tcp6sck   udp6sck   raw6sck  ip6-frag".into(),
        ActivityId::NET_IP6 => "   irec6/s fwddgm6/s   idel6/s    orq6/s  asmrq6/s  asmok6/s imcpck6/s omcpck6/s fragok6/s fragcr6/s".into(),
        ActivityId::NET_EIP6 => " ihdrer6/s iadrer6/s iukwnp6/s  i2big6/s  idisc6/s  odisc6/s  inort6/s  onort6/s   asmf6/s  fragf6/s itrpck6/s".into(),
        ActivityId::NET_ICMP6 => "   imsg6/s   omsg6/s   iech6/s  iechr6/s  oechr6/s  igmbq6/s  igmbr6/s  ogmbr6/s igmbrd6/s ogmbrd6/s irtsol6/s ortsol6/s  irtad6/s inbsol6/s onbsol6/s  inbad6/s  onbad6/s".into(),
        ActivityId::NET_EICMP6 => "   ierr6/s idtunr6/s odtunr6/s  itmex6/s  otmex6/s iprmpb6/s oprmpb6/s iredir6/s oredir6/s ipck2b6/s opck2b6/s".into(),
        ActivityId::NET_UDP6 => "   idgm6/s   odgm6/s noport6/s idgmer6/s".into(),
        ActivityId::PWR_CPU => "     CPU       MHz".into(),
        ActivityId::PWR_FAN => format!("     FAN       rpm      drpm {dev}"),
        ActivityId::PWR_TEMP => format!("    TEMP      degC     %temp {dev}"),
        ActivityId::PWR_IN => format!("      IN       inV       %in {dev}"),
        ActivityId::HUGE => " kbhugfree kbhugused  %hugused".into(),
        ActivityId::PWR_FREQ => "     CPU    wghMHz".into(),
        ActivityId::PWR_USB => format!(
            "     BUS  idvendor    idprod  maxpower {} {}",
            pad_left("manufact", el7::USB_MANUF_WIDTH),
            pad_left("product", el7::USB_PROD_WIDTH)
        ),
        ActivityId::FS => format!(
            "  MBfsfree  MBfsused   %fsused  %ufsused     Ifree     Iused    %Iused {}",
            if opts.print.fs_mount {
                "MOUNTPOINT"
            } else {
                "FILESYSTEM"
            }
        ),
        _ => String::new(),
    }
}

/// 見出し行 (`"\n%-11s..."`)。
fn push_header(buf: &mut String, ts: &str, id: ActivityId, mem: MemOutput, opts: &El7Options) {
    buf.push('\n');
    buf.push_str(&pad_right(ts, TSW));
    buf.push_str(&header_text(id, mem, opts));
    buf.push('\n');
}

// ============================================================================
// 本体
// ============================================================================

/// `handle_curr_act_stats()` 1 回分の終わり方。
#[derive(Debug, Clone, Copy)]
struct PassEnd {
    /// 最後に読んだレコードの次の番号。
    pos: usize,
    /// EOF まで読んだか。
    eosaf: bool,
    /// 最後に読んだのが RESTART か。
    restart: bool,
    /// `cnt` の残り (`0` なら count を使い切った)。
    cnt: i64,
}

/// 区間 × activity の間で持ち越す、本家の関数内 `static` と大域変数。
struct Shared {
    /// `write_stats()` の `static int cross_day`。一度立つと最後まで立ったまま。
    cross_day: bool,
    /// `next_slice()` の `static unsigned long long last_uptime`。
    last_uptime: u64,
}

/// 実行中に変わらない入力。
struct Ctx<'a> {
    file: &'a SaFile,
    opts: &'a El7Options,
    recs: &'a [Rec],
    /// `act[A_CPU]->nr` (CPU "all" を含む)。
    cpu_nr: i64,
    /// 採取元の `unsigned long` の幅。
    ulong_bits: u32,
}

/// el7 の `sar` のテキストを書き出す。
///
/// 本家の `read_stats_from_file()` をそのまま写した骨格:
///
/// ```text
/// バナー
/// do {
///     do { レコードを読む; RESTART / COMMENT なら出す } while (特殊 || -s/-e の外)
///     fpos = その直後
///     for (activity: ファイル順) for (出力: -R / -r / -S など)
///         lseek(fpos); 表示; Average:
///     count を使い切っていれば次の RESTART まで読み飛ばす (COMMENT は出す)
///     区間を終わらせた RESTART を出す
/// } while (!eof)
/// ```
pub fn write_report<W: Write>(out: &mut W, file: &SaFile, opts: &El7Options) -> crate::Result<()> {
    let magic = file.magic().format_magic;
    if magic != FORMAT_MAGIC {
        return Err(crate::Error::Other(format!(
            "{}: sysstat 10.1.5 (el7) の sar はこの世代 (format_magic 0x{magic:04x}) を読めません。\
             読めるのは 0x2171 だけです (--sar-profile を外すと現行版の書式で出せます)",
            file.path().display()
        )));
    }

    // 本家の check_file_actlst(): 未知 ID と magic 違いは読み飛ばし、A_CPU は必須。
    let known: Vec<FileActivityEntry> = file
        .activities()
        .iter()
        .filter(|a| el7::spec(a.id).is_some_and(|s| s.magic == a.magic))
        .copied()
        .collect();
    let Some(cpu) = known.iter().find(|a| a.id == ActivityId::CPU) else {
        return Err(crate::Error::Other(format!(
            "Invalid system activity file: {} (sysstat 10.1.5 (el7) が読める A_CPU がありません)",
            file.path().display()
        )));
    };
    let cpu_nr = i64::from(cpu.nr);
    // 選んだ activity がファイルに 1 つも無いときだけ止める。本家はこの判定で
    // magic を見ないので、magic の違う activity も「ファイルにある」と数える。
    // そうした activity は `selected` に入らず、バナー (と最初の統計レコードより
    // 前の COMMENT / RESTART) だけを出して正常に終わる。
    if !file
        .activities()
        .iter()
        .any(|a| opts.activities.contains(&a.id))
    {
        return Err(crate::Error::Other(format!(
            "Requested activities not available in file {}",
            file.path().display()
        )));
    }
    let selected: Vec<FileActivityEntry> = known
        .iter()
        .filter(|a| opts.activities.contains(&a.id))
        .copied()
        .collect();

    let io = |e: io::Error| crate::Error::Io {
        path: file.path().to_path_buf(),
        source: e,
    };

    // print_report_hdr()
    let h = file.header();
    writeln!(
        out,
        "{} {} ({}) \t{} \t_{}_\t({} CPU)",
        h.sysname,
        h.release,
        h.nodename,
        banner_date(
            h.ust_time,
            h.year,
            h.month,
            h.day,
            opts.time,
            opts.date_format
        ),
        h.machine,
        if cpu_nr > 1 { cpu_nr - 1 } else { 1 }
    )
    .map_err(io)?;

    let recs = index_records(file)?;
    let ctx = Ctx {
        file,
        opts,
        recs: &recs,
        cpu_nr,
        ulong_bits: u32::from(h.sizeof_long.max(4)) * 8,
    };
    let mut shared = Shared {
        cross_day: false,
        last_uptime: 0,
    };

    let mut pos = 0usize;
    // `sar_get_record_timestamp_struct()` が書く大域の `rectime`
    let mut rt_global = RecTime {
        hour: 0,
        min: 0,
        sec: 0,
    };
    loop {
        // 区間の最初の統計レコードを探す (特殊レコードはここで出す)
        let first = loop {
            let Some(rec) = recs.get(pos) else {
                return Ok(());
            };
            pos += 1;
            match rec.kind {
                Kind::Restart | Kind::Comment => {
                    print_special(out, rec, opts, &mut rt_global).map_err(io)?;
                }
                Kind::Stats => {
                    if let Some(rt) = rectime(rec, opts.time) {
                        rt_global = rt;
                    }
                    let too_soon = opts.tm_start.is_some_and(|s| datecmp(rt_global, s) < 0);
                    let too_late = opts.tm_end.is_some_and(|e| datecmp(rt_global, e) >= 0);
                    if !too_soon && !too_late {
                        break pos - 1;
                    }
                }
            }
        };

        let mut last: Option<PassEnd> = None;
        for act in &selected {
            // buf[2] は activity ごと。複数出力 (-R / -r / -S) の間では持ち越す。
            let mut buf2: Option<(Decoder, Buf)> = None;
            let outputs = if act.id == ActivityId::MEMORY {
                opts.mem_outputs()
            } else {
                vec![MemOutput::Amt]
            };
            for mem in outputs {
                last = Some(handle_pass(
                    out,
                    &ctx,
                    &mut shared,
                    act,
                    mem,
                    first,
                    &mut buf2,
                    &mut rt_global,
                )?);
            }
        }
        let Some(mut end) = last else {
            return Ok(());
        };

        if end.cnt == 0 {
            // count を使い切った: 次の RESTART まで読み飛ばす (COMMENT は出す)
            end.eosaf = true;
            end.restart = false;
            while let Some(rec) = recs.get(end.pos) {
                end.pos += 1;
                match rec.kind {
                    Kind::Stats => {}
                    Kind::Comment => {
                        print_special(out, rec, opts, &mut rt_global).map_err(io)?;
                    }
                    Kind::Restart => {
                        end.eosaf = false;
                        end.restart = true;
                        break;
                    }
                }
            }
        }

        // 区間を終わらせた RESTART を出す
        if !end.eosaf && end.restart {
            print_special(out, &recs[end.pos - 1], opts, &mut rt_global).map_err(io)?;
        }
        if end.eosaf {
            return Ok(());
        }
        pos = end.pos;
    }
}

/// `sar_print_special()`。表示したら真。
fn print_special<W: Write>(
    out: &mut W,
    rec: &Rec,
    opts: &El7Options,
    rt_global: &mut RecTime,
) -> io::Result<bool> {
    let Some(rt) = rectime(rec, opts.time) else {
        return Ok(false);
    };
    *rt_global = rt;
    let dp = !(opts.tm_start.is_some_and(|s| datecmp(rt, s) < 0)
        || opts.tm_end.is_some_and(|e| datecmp(rt, e) > 0));
    let ts = pad_right(&time_text(rt), TSW);
    match rec.kind {
        Kind::Restart if dp => {
            write!(out, "\n{ts}       LINUX RESTART\n")?;
            Ok(true)
        }
        // 本文は置き換えずにそのまま出す (現行版は非印字文字を `.` にする)
        Kind::Comment if dp && opts.comment => {
            out.write_all(ts.as_bytes())?;
            out.write_all(b"  COM ")?;
            out.write_all(&rec.comment)?;
            out.write_all(b"\n")?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// `get_itv_value()`: `(itv, g_itv)`。
fn itv_values(curr: &Rec, prev: &Rec, cpu_nr: i64) -> (u64, u64) {
    let g_itv = el7::get_interval(prev.uptime, curr.uptime);
    let itv = if cpu_nr > 2 {
        el7::get_interval(prev.uptime0, curr.uptime0)
    } else {
        g_itv
    };
    (itv, g_itv)
}

/// `handle_curr_act_stats()` 1 回分 (1 activity × 1 出力 × 1 区間)。
#[allow(clippy::too_many_arguments)]
fn handle_pass<W: Write>(
    out: &mut W,
    ctx: &Ctx<'_>,
    shared: &mut Shared,
    act: &FileActivityEntry,
    mem: MemOutput,
    first: usize,
    buf2: &mut Option<(Decoder, Buf)>,
    rt_global: &mut RecTime,
) -> crate::Result<PassEnd> {
    let opts = ctx.opts;
    let recs = ctx.recs;
    let id = act.id;
    let io = |e: io::Error| crate::Error::Io {
        path: ctx.file.path().to_path_buf(),
        source: e,
    };

    // 見出しの再表示のために 1 サンプルで進める行数。
    // ビットマップを持つ activity はビット数、それ以外はファイルの item 数。
    let inc = match id {
        ActivityId::IRQ => opts.print.irq_bitmap.count_bits(),
        _ if el7::uses_bitmap(id) => opts.print.cpu_bitmap.count_bits(),
        _ => u64::from(act.nr.max(0) as u32),
    };
    let nr2 = act.nr2.max(1) as usize;
    let is_global_itv = id == ActivityId::CPU;

    let mut cnt = opts.count;
    let mut lines: u64 = 0;
    let mut dis = false;
    let mut davg: u64 = 0;
    let mut reset = true;
    let mut acc = Accum::default();
    // buf[!curr] と record_hdr[!curr] (最後に表示したサンプル。最初は buf[2])
    let mut prev: Buf = Vec::new();
    let mut prev_at = first;
    let mut end: Option<PassEnd> = None;
    let mut at = first;
    let mut text = String::new();

    walk_items_in(
        ctx.file,
        &Selection::Only(vec![id]),
        RecordRange::new(first, usize::MAX),
        |item| {
            let idx = at;
            at += 1;
            if idx == first {
                // 区間の最初の統計レコード (本家の buf[2])。表示はしない。
                if let WalkItem::Sample(view) = item {
                    if buf2.is_none() {
                        let (Some(plan), Some(spec)) = (view.plan_for(id), el7::spec(id)) else {
                            return Ok(ScanControl::Stop);
                        };
                        let dec = Decoder::new(spec, plan, ctx.ulong_bits);
                        let items = view
                            .curr
                            .activity(id)
                            .map(|a| dec.buf(&a.items))
                            .unwrap_or_default();
                        *buf2 = Some((dec, items));
                    }
                    if let Some((_, b)) = buf2.as_ref() {
                        prev = b.clone();
                    }
                }
                return Ok(ScanControl::Continue);
            }
            let rec = &recs[idx];

            // ループの先頭で見出しを出すかを決める (EOF / RESTART の反復でも決め直す)
            if lines >= opts.rows || lines == 0 {
                lines = 0;
                dis = true;
            } else {
                dis = false;
            }

            let WalkItem::Sample(view) = item else {
                if rec.kind == Kind::Restart {
                    end = Some(PassEnd {
                        pos: idx + 1,
                        eosaf: false,
                        restart: true,
                        cnt,
                    });
                    return Ok(ScanControl::Stop);
                }
                // COMMENT は表示したら 1 行と数える
                if print_special(out, rec, opts, rt_global).map_err(io)? {
                    lines += 1;
                }
                return Ok(ScanControl::Continue);
            };
            let Some((dec, summary)) = buf2.as_mut() else {
                return Ok(ScanControl::Stop);
            };
            let mut curr = view
                .curr
                .activity(id)
                .map(|a| dec.buf(&a.items))
                .unwrap_or_default();

            // ---- write_stats() ----
            let shown = 'write: {
                if !next_slice(
                    recs[first].uptime0,
                    rec.uptime0,
                    reset,
                    opts.interval,
                    &mut shared.last_uptime,
                ) {
                    break 'write false;
                }
                let prev_rec = &recs[prev_at];
                let (Some(rt_prev), Some(mut rt)) =
                    (rectime(prev_rec, opts.time), rectime(rec, opts.time))
                else {
                    break 'write false;
                };
                let (ts_prev, ts_curr) = (time_text(rt_prev), time_text(rt));
                *rt_global = rt;
                if opts.tm_start.is_some()
                    && prev_rec.ust_time != 0
                    && rec.ust_time > prev_rec.ust_time
                    && rec.hour < prev_rec.hour
                {
                    shared.cross_day = true;
                }
                if shared.cross_day {
                    rt.hour += 24;
                }
                if opts.tm_start.is_some_and(|s| datecmp(rt, s) < 0) {
                    break 'write false;
                }
                let (itv, g_itv) = itv_values(rec, prev_rec, ctx.cpu_nr);
                if opts.tm_end.is_some_and(|e| datecmp(rt, e) > 0) {
                    cnt = 0;
                    break 'write false;
                }

                let p = Print {
                    dec,
                    opts: &opts.print,
                    itv: if is_global_itv { g_itv } else { itv },
                    avg: None,
                    mem,
                    nr2,
                };
                let rows = el7::print_rows(&p, &mut prev, &mut curr, Some(summary), &mut acc);
                text.clear();
                if dis {
                    push_header(&mut text, &ts_prev, id, mem, opts);
                }
                for row in &rows {
                    push_row(&mut text, &ts_curr, row);
                }
                out.write_all(text.as_bytes()).map_err(io)?;
                true
            };

            if shown && cnt > 0 {
                cnt -= 1;
            }
            if shown {
                davg += 1;
                lines += inc;
                // 表示したサンプルが次の前サンプルになる (書き換えを含めて)
                prev = curr;
                prev_at = idx;
            }
            reset = false;
            if cnt == 0 {
                end = Some(PassEnd {
                    pos: idx + 1,
                    eosaf: false,
                    restart: false,
                    cnt,
                });
                return Ok(ScanControl::Stop);
            }
            Ok(ScanControl::Continue)
        },
    )?;

    let end = match end {
        Some(end) => end,
        None => {
            // EOF を読んだ反復でも見出しの判定は行われる
            dis = lines >= opts.rows || lines == 0;
            PassEnd {
                pos: recs.len(),
                eosaf: true,
                restart: false,
                cnt,
            }
        }
    };

    // ---- write_stats_avg() ----
    if davg > 0
        && let Some((dec, first_buf)) = buf2.as_mut()
    {
        let last = &recs[prev_at];
        let g_itv = el7::get_interval(recs[first].uptime, last.uptime);
        let itv = if ctx.cpu_nr > 1 {
            el7::get_interval(recs[first].uptime0, last.uptime0)
        } else {
            g_itv
        };
        let p = Print {
            dec,
            opts: &opts.print,
            itv: if is_global_itv { g_itv } else { itv },
            avg: Some(AvgInfo { count: davg }),
            mem,
            nr2,
        };
        let rows = el7::print_rows(&p, first_buf, &mut prev, None, &mut acc);
        // USB と FS の平均は "Summary" (FS の見出しだけコロン付き)
        let (hdr_ts, row_ts) = match id {
            ActivityId::PWR_USB => ("Summary", "Summary"),
            ActivityId::FS => ("Summary:", "Summary"),
            _ => ("Average:", "Average:"),
        };
        text.clear();
        if dis {
            push_header(&mut text, hdr_ts, id, mem, opts);
        }
        for row in &rows {
            push_row(&mut text, row_ts, row);
        }
        out.write_all(text.as_bytes()).map_err(io)?;
    }
    Ok(end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn datecmp_orders_by_hour_minute_second() {
        let rt = RecTime {
            hour: 10,
            min: 30,
            sec: 5,
        };
        let t = |hour, min, sec| Tstamp { hour, min, sec };
        assert!(datecmp(rt, t(10, 30, 5)) == 0);
        assert!(datecmp(rt, t(10, 30, 6)) < 0);
        assert!(datecmp(rt, t(9, 59, 59)) > 0);
        // `-e` が `-s` より前なら本家は +24 する
        assert!(datecmp(rt, t(26, 0, 0)) < 0);
    }

    /// 記録間隔が 0.5 秒未満のレコードは interval = 1 でも表示されない。
    #[test]
    fn next_slice_drops_records_closer_than_half_a_second() {
        let mut last = 0;
        // 600 秒間隔 (60000 jiffies) は毎回表示
        assert!(next_slice(1000, 61_000, true, 1, &mut last));
        assert!(next_slice(1000, 121_000, false, 1, &mut last));
        // 40 jiffies (0.4 秒) しか離れていないレコードは表示しない
        assert!(!next_slice(1000, 121_040, false, 1, &mut last));
    }

    #[test]
    fn next_slice_selects_multiples_of_the_interval() {
        // 600 秒ごとのファイルを -i 1200 で読むと 1 つおきに出る
        let mut last = 0;
        let shown: Vec<bool> = (1..=4)
            .map(|k| next_slice(0, k * 60_000, k == 1, 1200, &mut last))
            .collect();
        assert_eq!(shown, vec![false, true, false, true]);
    }

    #[test]
    fn cells_follow_the_printf_conversions() {
        let mut s = String::new();
        push_cell(&mut s, &Cell::F92(1.285));
        push_cell(&mut s, &Cell::P62(99.354));
        push_cell(&mut s, &Cell::P72(12.5));
        push_cell(&mut s, &Cell::F90(9015.0));
        push_cell(&mut s, &Cell::U9(42));
        push_cell(&mut s, &Cell::X9(0x1d6b));
        push_cell(&mut s, &Cell::Na);
        assert_eq!(
            s,
            "      1.28     99.35     12.50      9015        42      1d6b       N/A"
        );
    }

    #[test]
    fn c_float_prints_nan_and_infinity_like_glibc() {
        assert_eq!(c_float(f64::NAN, 2), "nan");
        assert_eq!(c_float(-f64::NAN, 2), "-nan");
        assert_eq!(c_float(f64::INFINITY, 2), "inf");
        assert_eq!(c_float(f64::NEG_INFINITY, 0), "-inf");
    }

    #[test]
    fn labels_and_tails_match_the_row_prefixes() {
        let mut s = String::new();
        push_row(
            &mut s,
            "00:10:01",
            &Row {
                label: Label::All,
                cells: vec![Cell::P62(1.0)],
                tail: Tail::None,
            },
        );
        push_row(
            &mut s,
            "00:10:01",
            &Row {
                label: Label::Num3Wide(5),
                cells: vec![],
                tail: Tail::Name("sda".into()),
            },
        );
        push_row(
            &mut s,
            "Summary",
            &Row {
                label: Label::Bus(-1),
                cells: vec![],
                tail: Tail::Usb(String::new(), "x".into()),
            },
        );
        let lines: Vec<&str> = s.lines().collect();
        assert_eq!(lines[0], "00:10:01        all      1.00");
        // `%-11s` の余り 3 桁 + `"       %3d"` の 9 桁ぶんの空白
        assert_eq!(lines[1], format!("00:10:01{}5 sda", " ".repeat(12)));
        // `%-11s` の余り 4 桁 + `"  %6d"` の 6 桁ぶんの空白
        assert_eq!(
            lines[2],
            format!(
                "Summary{}-1 {} {}",
                " ".repeat(10),
                " ".repeat(23),
                pad_left("x", 47)
            )
        );
    }
}

//! `--sar-profile sysstat-10.1.5-el7` を指定した `sar` 互換入口の引数解析。
//!
//! RHEL / CentOS 7 の sysstat 10.1.5 の `sar.c: main()` と `sa_common.c` の
//! `parse_sar_opt()` / `parse_sar_I_opt()` / `parse_sa_P_opt()` /
//! `parse_sar_m_opt()` / `parse_sar_n_opt()` / `parse_timestamp()` を写したもの。
//! 現行版 (v12.8.0) の文法 ([`super::sar_args`]) とは次の点が違う。
//!
//! | オプション | el7 (10.1.5) | 現行 (12.8.0) |
//! |---|---|---|
//! | `-h` | ヘルプ | `--pretty --human` |
//! | `-I` | `SUM` / `ALL` (先頭 16 本) / `XALL` / 番号のカンマ区切り | `SUM` / `ALL` と `--int=` |
//! | `-R` | メモリのページ変化 | なし |
//! | `-r ALL` / `-q ALL` / `-x` / `-z` / `--dec=` / `--human` / `--dev=` など | なし (usage) | あり |
//! | `-f` の省略 | `SA_DIR/saDD` | `saDD` と `saYYYYMMDD` の新しい方 |
//!
//! 本家の `usage()` に当たる誤りは [`SarEl7ArgError`] で返す。
//! `-o` (採取) と `-j` (実行ホストの `/dev/disk/by-*` を引く) は reSARch では扱えないので
//! 理由を添えて拒否する。

use std::collections::BTreeSet;
use std::path::PathBuf;

use crate::model::{ActivityId, PageSize};
use crate::output::sar_el7::Tstamp;
use crate::series::el7::{self, Bitmap, NR_CPUS, NR_IRQS};

use super::sar_args::SarImmediate;

/// `-s` の既定値 (`DEF_TMSTART`)。
const DEF_TMSTART: &str = "08:00:00";
/// `-e` の既定値 (`DEF_TMEND`)。
const DEF_TMEND: &str = "18:00:00";

/// 読み出し元。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum El7Input {
    /// `-f` のファイル名省略、または `-f` 自体の省略 (`SA_DIR/saDD`)。
    DefaultDaily,
    /// `-f <file>`。
    File(PathBuf),
}

/// 解析結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SarEl7Options {
    /// 即座に表示して終わるもの (`-h` / `-V`)。
    pub immediate: Option<SarImmediate>,
    /// 読み出し元。`None` は「ファイルを指定していない」(本家ではライブ採取)。
    pub input: Option<El7Input>,
    /// `-N` (何日前の日次ファイルか)。
    pub day_offset: u32,
    /// 選択された activity (`AO_SELECTED`)。
    pub activities: BTreeSet<ActivityId>,
    /// `A_CPU` / `A_PWR_CPUFREQ` / `A_PWR_WGHFREQ` のビットマップ (`-P`)。
    pub cpu_bitmap: Bitmap,
    /// `A_IRQ` のビットマップ (`-I`)。
    pub irq_bitmap: Bitmap,
    /// `-u ALL` / `-A`。
    pub cpu_all: bool,
    /// `-R`。
    pub mem_dia: bool,
    /// `-r`。
    pub mem_amt: bool,
    /// `-S`。
    pub mem_swap: bool,
    /// `-F MOUNT`。
    pub fs_mount: bool,
    /// `-C`。
    pub comment: bool,
    /// `-t`。
    pub true_time: bool,
    /// `-p` (デバイス名は `dev<major>-<minor>` のまま。モジュール説明を参照)。
    pub pretty: bool,
    /// `-s`。
    pub tm_start: Option<Tstamp>,
    /// `-e` (`-s` より前の時刻なら `hour` に 24 が足されている)。
    pub tm_end: Option<Tstamp>,
    /// `-i` / positional の interval。**未指定は -1** (本家のまま)。
    pub interval: i64,
    /// positional の count。**未指定は 0** (解析後に -1 = 無制限へ直す前の値)。
    pub count: i64,
    /// `-R` のページサイズ (`--sar-page-size`)。
    pub page_size: PageSize,
}

/// 本家が `usage()` / エラー終了する誤り。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SarEl7ArgError {
    /// 本家の `usage()`。どの引数で止まったかを添える。
    #[error("sysstat 10.1.5 (el7) の sar として解釈できない引数です: {arg}")]
    Usage { arg: String },
    /// 値を取るオプションに値が無い。
    #[error("{option} には値が必要です")]
    MissingValue { option: &'static str },
    /// 採取は行わない。
    #[error(
        "-o: reSARch は統計の採取を行いません (sa ファイルの解析専用です)。採取は sysstat の sadc / sar -o を使ってください"
    )]
    Collect,
    /// `-j` は実行ホストの `/dev/disk/by-*` を引く。
    #[error(
        "-j {kind}: 永続デバイス名は sar を実行するホストの /dev/disk/by-{kind} から引くため、reSARch では再現できません"
    )]
    PersistentName { kind: String },
    /// `-f` と `-o` の同時指定。
    #[error("-f and -o options are mutually exclusive")]
    FileAndOutput,
}

impl SarEl7Options {
    fn new(page_size: PageSize) -> Self {
        Self {
            immediate: None,
            input: None,
            day_offset: 0,
            activities: BTreeSet::new(),
            cpu_bitmap: Bitmap::cpu(),
            irq_bitmap: Bitmap::irq(),
            cpu_all: false,
            mem_dia: false,
            mem_amt: false,
            mem_swap: false,
            fs_mount: false,
            comment: false,
            true_time: false,
            pretty: false,
            tm_start: None,
            tm_end: None,
            interval: -1,
            count: 0,
            page_size,
        }
    }

    fn select(&mut self, id: ActivityId) {
        self.activities.insert(id);
    }

    /// `select_all_activities()` と `-A` の追加設定。
    fn select_all(&mut self) {
        for s in el7::SPECS {
            self.activities.insert(s.id);
        }
        self.mem_amt = true;
        self.mem_dia = true;
        self.mem_swap = true;
        self.irq_bitmap.set_all();
        self.cpu_bitmap.set_all();
        self.cpu_all = true;
    }
}

fn usage(arg: &str) -> SarEl7ArgError {
    SarEl7ArgError::Usage {
        arg: arg.to_string(),
    }
}

fn all_digits(s: &str) -> bool {
    s.bytes().all(|b| b.is_ascii_digit())
}

/// C の `atol()` / `atoi()`。先頭の空白と符号を読み、続く数字だけを使う (無ければ 0)。
fn atol(s: &str) -> i64 {
    let t = s.trim_start_matches([' ', '\t', '\n', '\r', '\x0b', '\x0c']);
    let (neg, t) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
    let v = if digits.is_empty() {
        0
    } else {
        digits.parse::<i64>().unwrap_or(i64::MAX)
    };
    if neg { -v } else { v }
}

/// `decode_timestamp()` (`hh:mm:ss`)。
fn decode_timestamp(ts: &str) -> Option<Tstamp> {
    let b = ts.as_bytes();
    if b.len() != 8 {
        return None;
    }
    // 本家は 2・5 文字目を NUL にして atoi する (区切り文字は問わない)
    let field = |r: std::ops::Range<usize>| atol(std::str::from_utf8(&b[r]).unwrap_or(""));
    let (hour, min, sec) = (field(0..2), field(3..5), field(6..8));
    if !(0..=23).contains(&hour) || !(0..=59).contains(&min) || !(0..=59).contains(&sec) {
        return None;
    }
    Some(Tstamp {
        hour: hour as i32,
        min: min as i32,
        sec: sec as i32,
    })
}

/// 引数を解析する。`--sar-profile` / `--sar-page-size` は呼び出し側が取り除いておく。
pub fn parse_sar_el7_args(
    args: &[String],
    page_size: PageSize,
) -> Result<SarEl7Options, SarEl7ArgError> {
    let mut o = SarEl7Options::new(page_size);
    let mut collect = false;
    let mut opt = 0usize;
    let arg = |i: usize| args.get(i).map(String::as_str);

    while let Some(a) = arg(opt) {
        match a {
            "-I" => {
                let list = arg(opt + 1).ok_or(SarEl7ArgError::MissingValue { option: "-I" })?;
                parse_i(&mut o, list).map_err(|()| usage(list))?;
                opt += 2;
            }
            "-P" => {
                let list = arg(opt + 1).ok_or(SarEl7ArgError::MissingValue { option: "-P" })?;
                parse_p(&mut o, list).map_err(|()| usage(list))?;
                opt += 2;
            }
            "-o" => {
                collect = true;
                // 本家はファイル名 (か "-") を 1 つ消費する
                match arg(opt + 1) {
                    Some(n) if !n.starts_with('-') && !all_digits(n) => opt += 2,
                    _ => opt += 1,
                }
            }
            "-f" => match arg(opt + 1) {
                Some(n) if !n.starts_with('-') && !all_digits(n) => {
                    o.input = Some(El7Input::File(PathBuf::from(n)));
                    opt += 2;
                }
                _ => {
                    o.input = Some(El7Input::DefaultDaily);
                    opt += 1;
                }
            },
            "-s" | "-e" => {
                let (default, is_start) = if a == "-s" {
                    (DEF_TMSTART, true)
                } else {
                    (DEF_TMEND, false)
                };
                let ts = match arg(opt + 1) {
                    Some(t) if t.len() == 8 => {
                        opt += 2;
                        t
                    }
                    _ => {
                        opt += 1;
                        default
                    }
                };
                let t = decode_timestamp(ts).ok_or_else(|| usage(ts))?;
                if is_start {
                    o.tm_start = Some(t);
                } else {
                    o.tm_end = Some(t);
                }
            }
            "-h" => {
                o.immediate = Some(SarImmediate::Help);
                return Ok(o);
            }
            "-i" => {
                let v = arg(opt + 1).ok_or(SarEl7ArgError::MissingValue { option: "-i" })?;
                if !all_digits(v) || v.is_empty() {
                    return Err(usage(v));
                }
                o.interval = atol(v);
                if o.interval < 1 {
                    return Err(usage(v));
                }
                opt += 2;
            }
            "-m" => {
                let list = arg(opt + 1).ok_or(SarEl7ArgError::MissingValue { option: "-m" })?;
                parse_m(&mut o, list).map_err(|()| usage(list))?;
                opt += 2;
            }
            "-n" => {
                let list = arg(opt + 1).ok_or(SarEl7ArgError::MissingValue { option: "-n" })?;
                parse_n(&mut o, list).map_err(|()| usage(list))?;
                opt += 2;
            }
            _ if a.len() > 1 && a.len() < 4 && a.starts_with('-') && all_digits(&a[1..]) => {
                o.day_offset = atol(&a[1..]) as u32;
                opt += 1;
            }
            _ if a.starts_with('-') => {
                let consumed = parse_sar_opt(&mut o, a, arg(opt + 1))?;
                if o.immediate.is_some() {
                    return Ok(o);
                }
                opt += 1 + consumed;
            }
            _ if o.interval < 0 => {
                if !all_digits(a) {
                    return Err(usage(a));
                }
                o.interval = atol(a);
                opt += 1;
            }
            _ => {
                if !all_digits(a) || o.interval == 0 || o.count != 0 {
                    return Err(usage(a));
                }
                o.count = atol(a);
                if o.count < 1 {
                    return Err(usage(a));
                }
                opt += 1;
            }
        }
    }

    if collect {
        if o.input.is_some() {
            return Err(SarEl7ArgError::FileAndOutput);
        }
        return Err(SarEl7ArgError::Collect);
    }
    // `sar` 単独、または interval も -f も無い → 既定の日次ファイル
    if args.is_empty() || (o.interval < 0 && o.input.is_none()) {
        o.input = Some(El7Input::DefaultDaily);
    }
    if let (Some(s), Some(e)) = (o.tm_start, o.tm_end.as_mut())
        && e.hour < s.hour
    {
        e.hour += 24;
    }
    // 採取しない (ファイル読み出し専用) ので interval = 0 は本家と同じく usage
    if o.interval == 0 {
        return Err(usage("0"));
    }
    if o.count == 0 {
        o.count = -1;
    }
    if o.interval < 0 {
        o.interval = 1;
    }
    // select_default_activity()
    if o.activities.is_empty() {
        o.select(ActivityId::CPU);
    }
    if o.cpu_bitmap.count_bits() == 0 {
        o.cpu_bitmap.set(0);
    }
    Ok(o)
}

/// `parse_sar_opt()`。束ねた 1 文字オプション (`-bBru` など)。
///
/// 次の引数を消費した個数を返す (`-u ALL` / `-F MOUNT` / `-j <type>` は 1)。
fn parse_sar_opt(
    o: &mut SarEl7Options,
    a: &str,
    next: Option<&str>,
) -> Result<usize, SarEl7ArgError> {
    // `-` 単独は何も選ばずに受け付けられる (本家のループが 1 回も回らない)
    let chars: Vec<char> = a.chars().skip(1).collect();
    for (k, c) in chars.iter().enumerate() {
        let last = k + 1 == chars.len();
        match c {
            'A' => o.select_all(),
            'B' => o.select(ActivityId::PAGE),
            'b' => o.select(ActivityId::IO),
            'C' => o.comment = true,
            'd' => o.select(ActivityId::DISK),
            'F' => {
                o.select(ActivityId::FS);
                if last && next == Some("MOUNT") {
                    o.fs_mount = true;
                    return Ok(1);
                }
            }
            'H' => o.select(ActivityId::HUGE),
            'j' => {
                let kind = next.ok_or_else(|| usage(a))?;
                return Err(SarEl7ArgError::PersistentName {
                    kind: kind.to_lowercase(),
                });
            }
            'p' => o.pretty = true,
            'q' => o.select(ActivityId::QUEUE),
            'r' => {
                o.select(ActivityId::MEMORY);
                o.mem_amt = true;
            }
            'R' => {
                o.select(ActivityId::MEMORY);
                o.mem_dia = true;
            }
            'S' => {
                o.select(ActivityId::MEMORY);
                o.mem_swap = true;
            }
            't' => o.true_time = true,
            'u' => {
                o.select(ActivityId::CPU);
                if last && next == Some("ALL") {
                    o.cpu_all = true;
                    return Ok(1);
                }
                o.cpu_all = false;
            }
            'v' => o.select(ActivityId::KTABLES),
            'w' => o.select(ActivityId::PCSW),
            'W' => o.select(ActivityId::SWAP),
            'y' => o.select(ActivityId::SERIAL),
            'V' => {
                o.immediate = Some(SarImmediate::Version);
                return Ok(0);
            }
            _ => return Err(usage(a)),
        }
    }
    Ok(0)
}

/// `parse_sar_I_opt()`。
fn parse_i(o: &mut SarEl7Options, list: &str) -> Result<(), ()> {
    o.select(ActivityId::IRQ);
    for t in list.split(',').filter(|t| !t.is_empty()) {
        match t {
            "SUM" => o.irq_bitmap.or_byte(0, 0x01),
            // 先頭 16 本の割り込み (ビット 1〜16)
            "ALL" => {
                o.irq_bitmap.or_byte(0, 0xfe);
                o.irq_bitmap.or_byte(1, 0xff);
                o.irq_bitmap.or_byte(2, 0x01);
            }
            // 合計以外の全ビット (合計のビットはそのまま残す)
            "XALL" => {
                let c = o.irq_bitmap.byte(0);
                o.irq_bitmap.set_all();
                o.irq_bitmap.set_byte(0, 0xfe | c);
            }
            _ => {
                if !all_digits(t) {
                    return Err(());
                }
                let i = atol(t);
                if i < 0 || i as usize >= NR_IRQS {
                    return Err(());
                }
                o.irq_bitmap.set(i as usize + 1);
            }
        }
    }
    Ok(())
}

/// `parse_sa_P_opt()`。
fn parse_p(o: &mut SarEl7Options, list: &str) -> Result<(), ()> {
    for t in list.split(',').filter(|t| !t.is_empty()) {
        if t == "ALL" {
            o.cpu_bitmap.set_all();
            continue;
        }
        if !all_digits(t) {
            return Err(());
        }
        let i = atol(t);
        if i < 0 || i as usize >= NR_CPUS {
            return Err(());
        }
        o.cpu_bitmap.set(i as usize + 1);
    }
    Ok(())
}

/// `parse_sar_m_opt()`。
fn parse_m(o: &mut SarEl7Options, list: &str) -> Result<(), ()> {
    for t in list.split(',').filter(|t| !t.is_empty()) {
        let ids: &[ActivityId] = match t {
            "CPU" => &[ActivityId::PWR_CPU],
            "FAN" => &[ActivityId::PWR_FAN],
            "IN" => &[ActivityId::PWR_IN],
            "TEMP" => &[ActivityId::PWR_TEMP],
            "FREQ" => &[ActivityId::PWR_FREQ],
            "USB" => &[ActivityId::PWR_USB],
            "ALL" => &[
                ActivityId::PWR_CPU,
                ActivityId::PWR_FAN,
                ActivityId::PWR_IN,
                ActivityId::PWR_TEMP,
                ActivityId::PWR_FREQ,
                ActivityId::PWR_USB,
            ],
            _ => return Err(()),
        };
        for id in ids {
            o.select(*id);
        }
    }
    Ok(())
}

/// `parse_sar_n_opt()`。
fn parse_n(o: &mut SarEl7Options, list: &str) -> Result<(), ()> {
    const ALL: &[ActivityId] = &[
        ActivityId::NET_DEV,
        ActivityId::NET_EDEV,
        ActivityId::NET_SOCK,
        ActivityId::NET_NFS,
        ActivityId::NET_NFSD,
        ActivityId::NET_IP,
        ActivityId::NET_EIP,
        ActivityId::NET_ICMP,
        ActivityId::NET_EICMP,
        ActivityId::NET_TCP,
        ActivityId::NET_ETCP,
        ActivityId::NET_UDP,
        ActivityId::NET_SOCK6,
        ActivityId::NET_IP6,
        ActivityId::NET_EIP6,
        ActivityId::NET_ICMP6,
        ActivityId::NET_EICMP6,
        ActivityId::NET_UDP6,
    ];
    for t in list.split(',').filter(|t| !t.is_empty()) {
        let ids: &[ActivityId] = match t {
            "DEV" => &[ActivityId::NET_DEV],
            "EDEV" => &[ActivityId::NET_EDEV],
            "SOCK" => &[ActivityId::NET_SOCK],
            "NFS" => &[ActivityId::NET_NFS],
            "NFSD" => &[ActivityId::NET_NFSD],
            "IP" => &[ActivityId::NET_IP],
            "EIP" => &[ActivityId::NET_EIP],
            "ICMP" => &[ActivityId::NET_ICMP],
            "EICMP" => &[ActivityId::NET_EICMP],
            "TCP" => &[ActivityId::NET_TCP],
            "ETCP" => &[ActivityId::NET_ETCP],
            "UDP" => &[ActivityId::NET_UDP],
            "SOCK6" => &[ActivityId::NET_SOCK6],
            "IP6" => &[ActivityId::NET_IP6],
            "EIP6" => &[ActivityId::NET_EIP6],
            "ICMP6" => &[ActivityId::NET_ICMP6],
            "EICMP6" => &[ActivityId::NET_EICMP6],
            "UDP6" => &[ActivityId::NET_UDP6],
            "ALL" => ALL,
            _ => return Err(()),
        };
        for id in ids {
            o.select(*id);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<SarEl7Options, SarEl7ArgError> {
        let v: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
        parse_sar_el7_args(&v, PageSize::DEFAULT)
    }

    #[test]
    fn a_selects_everything_with_all_bits() {
        let o = parse(&["-A", "-f", "sa01"]).unwrap();
        assert_eq!(o.activities.len(), el7::SPECS.len());
        assert!(o.cpu_all && o.mem_dia && o.mem_amt && o.mem_swap);
        assert_eq!(o.cpu_bitmap.count_bits(), 8200);
        assert_eq!(o.irq_bitmap.count_bits(), 1032);
        assert_eq!(o.input, Some(El7Input::File("sa01".into())));
        assert_eq!((o.interval, o.count), (1, -1));
    }

    #[test]
    fn default_is_cpu_aggregate_from_the_daily_file() {
        let o = parse(&[]).unwrap();
        assert_eq!(o.activities, BTreeSet::from([ActivityId::CPU]));
        assert!(!o.cpu_all);
        assert_eq!(o.cpu_bitmap.count_bits(), 1);
        assert_eq!(o.input, Some(El7Input::DefaultDaily));
    }

    /// `-h` は現行版の `--pretty --human` ではなくヘルプ。
    #[test]
    fn h_is_help() {
        assert_eq!(parse(&["-h"]).unwrap().immediate, Some(SarImmediate::Help));
        assert_eq!(
            parse(&["-V"]).unwrap().immediate,
            Some(SarImmediate::Version)
        );
    }

    #[test]
    fn u_all_and_f_mount_consume_the_keyword_only_at_the_end() {
        let o = parse(&["-u", "ALL", "-F", "MOUNT", "-f", "x"]).unwrap();
        assert!(o.cpu_all && o.fs_mount);
        // 束ねた途中の u は ALL を消費しない → ALL は positional として usage
        assert!(parse(&["-ur", "ALL", "-f", "x"]).is_err());
    }

    #[test]
    fn i_option_follows_the_el7_bitmap_rules() {
        let o = parse(&["-I", "SUM", "-f", "x"]).unwrap();
        assert_eq!(o.irq_bitmap.count_bits(), 1);
        let o = parse(&["-I", "ALL", "-f", "x"]).unwrap();
        // 割り込み 0〜15 (ビット 1〜16)
        assert_eq!(o.irq_bitmap.count_bits(), 16);
        assert!(!o.irq_bitmap.is_set(0) && o.irq_bitmap.is_set(16));
        let o = parse(&["-I", "0,5", "-f", "x"]).unwrap();
        assert!(o.irq_bitmap.is_set(1) && o.irq_bitmap.is_set(6));
        let o = parse(&["-I", "XALL", "-f", "x"]).unwrap();
        assert!(!o.irq_bitmap.is_set(0));
        assert_eq!(o.irq_bitmap.count_bits(), 1031);
        assert!(parse(&["-I", "1024", "-f", "x"]).is_err());
    }

    #[test]
    fn options_that_el7_does_not_know_are_usage_errors() {
        for bad in [["--dec=1"], ["-x"], ["-z"], ["--human"]] {
            assert!(
                matches!(parse(&bad), Err(SarEl7ArgError::Usage { .. })),
                "{bad:?}"
            );
        }
        // -q ALL は el7 に無い (ALL は positional の interval として usage)
        assert!(parse(&["-q", "ALL", "-f", "x"]).is_err());
        assert!(parse(&["-n", "SOFT", "-f", "x"]).is_err());
    }

    #[test]
    fn collection_and_persistent_names_are_rejected_with_reasons() {
        assert_eq!(parse(&["-o", "out"]), Err(SarEl7ArgError::Collect));
        assert_eq!(
            parse(&["-f", "in", "-o", "out"]),
            Err(SarEl7ArgError::FileAndOutput)
        );
        assert!(matches!(
            parse(&["-d", "-j", "UUID", "-f", "x"]),
            Err(SarEl7ArgError::PersistentName { kind }) if kind == "uuid"
        ));
    }

    #[test]
    fn time_window_wraps_the_end_past_midnight() {
        let o = parse(&["-s", "22:00:00", "-e", "02:00:00", "-f", "x"]).unwrap();
        assert_eq!(o.tm_start.unwrap().hour, 22);
        assert_eq!(o.tm_end.unwrap().hour, 26);
        // 値が 8 文字でなければ既定値を使い、その引数は消費しない
        let o = parse(&["-s", "-f", "x"]).unwrap();
        assert_eq!(o.tm_start.unwrap().hour, 8);
        assert!(parse(&["-s", "25:00:00", "-f", "x"]).is_err());
    }

    #[test]
    fn interval_and_count_are_positional() {
        let o = parse(&["-u", "-f", "x", "1200", "3"]).unwrap();
        assert_eq!((o.interval, o.count), (1200, 3));
        let o = parse(&["-i", "600", "-f", "x"]).unwrap();
        assert_eq!(o.interval, 600);
        assert!(parse(&["-u", "-f", "x", "0"]).is_err());
        assert!(parse(&["-i", "0", "-f", "x"]).is_err());
    }

    #[test]
    fn day_offset_takes_one_or_two_digits() {
        assert_eq!(parse(&["-3"]).unwrap().day_offset, 3);
        assert_eq!(parse(&["-12"]).unwrap().day_offset, 12);
        // 3 桁は日数ではなく 1 文字オプションの束として解析され、usage になる
        assert!(parse(&["-123"]).is_err());
    }
}

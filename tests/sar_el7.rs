//! `--sar-profile sysstat-10.1.5-el7` (RHEL / CentOS 7 の sysstat 10.1.5 の `sar`) の回帰テスト。
//!
//! fixture は el7 の `rd_stats.h` の構造体定義と `docs/format/01-file-format.md` の
//! 0x2171 のオフセット表から**独立に書き起こした**最小のライタで作る
//! (本体のレイアウト表は使わない。同じ誤りが往復して通るのを避けるため)。
//! 期待値は各テストのコメントに書いた式から手計算したもので、本家の出力を
//! 写したものではない。値の組は el7 の癖が出るように選んである。

mod fixtures;

use std::path::Path;
use std::process::Command;

// ===========================================================================
// 0x2171 (LP64 little endian) の最小ライタ
// ===========================================================================

const R_STATS: u8 = 1;
const R_RESTART: u8 = 2;
const R_COMMENT: u8 = 4;

/// 1 activity の宣言 (`file_activity`)。
struct Act {
    id: u32,
    magic: u32,
    nr: i32,
    size: i32,
}

const A_CPU: Act = Act {
    id: 1,
    magic: 0x8a,
    nr: 3,
    size: 160,
};
const A_MEMORY: Act = Act {
    id: 7,
    magic: 0x8a,
    nr: 1,
    size: 88,
};

/// 1 レコード。
enum Rec {
    /// `uptime` (全 CPU の jiffies)、`uptime0` (CPU 1 個分)、時分秒、activity ごとの生バイト列。
    Stats {
        uptime: u64,
        uptime0: u64,
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

fn epoch(hms: (u8, u8, u8)) -> u64 {
    DAY0 + u64::from(hms.0) * 3600 + u64::from(hms.1) * 60 + u64::from(hms.2)
}

/// `record_header` (0x2171、48 バイト)。
fn record_header(b: &mut Vec<u8>, uptime: u64, uptime0: u64, rtype: u8, hms: (u8, u8, u8)) {
    let o = b.len();
    b.resize(o + 48, 0);
    put_u64(b, o, uptime); // uptime (aligned 16)
    put_u64(b, o + 16, uptime0); // uptime0 (aligned 16)
    put_u64(b, o + 32, epoch(hms)); // ust_time (unsigned long, aligned 16)
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
        put_u32(&mut b, o + 12, 1); // nr2
        put_u32(&mut b, o + 16, a.size as u32);
    }
    for r in recs {
        match r {
            Rec::Stats {
                uptime,
                uptime0,
                hms,
                payload,
            } => {
                record_header(&mut b, *uptime, *uptime0, R_STATS, *hms);
                b.extend_from_slice(payload);
            }
            Rec::Comment { hms, text } => {
                record_header(&mut b, 0, 0, R_COMMENT, *hms);
                let mut c = [0u8; 64];
                c[..text.len()].copy_from_slice(text.as_bytes());
                b.extend_from_slice(&c);
            }
            Rec::Restart { hms } => record_header(&mut b, 0, 0, R_RESTART, *hms),
        }
    }
    b
}

/// `stats_cpu` (10 個の `unsigned long long`、各 `aligned(16)` → 160 バイト)。
///
/// 並びは user, nice, sys, idle, iowait, steal, hardirq, softirq, guest, guest_nice。
fn cpu(user: u64, sys: u64, idle: u64, iowait: u64) -> Vec<u8> {
    let mut b = vec![0u8; 160];
    put_u64(&mut b, 0, user);
    put_u64(&mut b, 32, sys);
    put_u64(&mut b, 48, idle);
    put_u64(&mut b, 64, iowait);
    b
}

/// `stats_memory` (11 個の `unsigned long` → 88 バイト)。
///
/// 並びは frmkb, bufkb, camkb, tlmkb, frskb, tlskb, caskb, comkb, activekb, inactkb, dirtykb。
fn memory(v: [u64; 11]) -> Vec<u8> {
    let mut b = vec![0u8; 88];
    for (i, x) in v.iter().enumerate() {
        put_u64(&mut b, i * 8, *x);
    }
    b
}

fn stats(uptime: u64, uptime0: u64, hms: (u8, u8, u8), parts: &[Vec<u8>]) -> Rec {
    Rec::Stats {
        uptime,
        uptime0,
        hms,
        payload: parts.concat(),
    }
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

fn run(args: &[&str], file: &Path) -> String {
    let out = command()
        .args(["--sar-profile", "sysstat-10.1.5-el7"])
        .args(args)
        .arg("-f")
        .arg(file)
        .output()
        .unwrap();
    assert!(out.status.success(), "{args:?}: {out:?}");
    String::from_utf8(out.stdout).unwrap()
}

fn write(dir: &Path, name: &str, bytes: Vec<u8>) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    // `RESARCH_EL7_FIXTURE_DIR` を与えると fixture を残す (本家の el7 sar と
    // ローカルで突き合わせるため。リポジトリには本家の出力を入れない)
    if let Some(keep) = std::env::var_os("RESARCH_EL7_FIXTURE_DIR") {
        std::fs::copy(&path, Path::new(&keep).join(name)).unwrap();
    }
    path
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
    let out = run(&["-R", "-t", "--sar-page-size", "65536"], &file);
    assert_eq!(line(&out), "00:10:00         0.00      0.11      0.00");
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
// 入口と誤りの扱い
// ===========================================================================

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

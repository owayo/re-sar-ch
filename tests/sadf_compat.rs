//! `sadf` 互換出力の回帰テスト (本家 v12.8.0 との差分として見つかったもの)。
//!
//! fixture は `tests/fixtures` の生成器で組み立て、統計値は
//! `docs/format/02-activities.md` のオフセット表から独立に書き起こして埋める。
//! 期待値は本家ソースの規則 (`sadf.c` / `sadf_misc.c` / `raw_stats.c` ほか) と
//! 式から手で求めたもので、本家の出力を貼り付けたものではない。
mod fixtures;

use std::collections::BTreeMap;
use std::process::Command;

use fixtures::{ActivitySpec, FixtureAbi, FixtureSpec, Generation, RecordSpec, build};
use re_sar_ch::format::file::SaFile;
use re_sar_ch::model::ActivityId;
use re_sar_ch::output::sadf::{self, RecordSelect, SadfConfig, SadfExtra, SectionConfig, TimeBase};
use re_sar_ch::output::sar_text::CpuSelection;
use re_sar_ch::output::time_filter::{CrossDayRule, TimeBasis, TimeBound, TimeFilter};

// ===========================================================================
// fixture
// ===========================================================================

/// 2020-09-13 12:00:00 UTC。レコードの時刻はここからの経過秒で書く。
const BASE: u64 = 1_599_998_400;
/// `0x2175` / LP64 の `record_header` のサイズ。
const RECORD_HEADER: usize = 24;

/// レコード 1 本の論理内容。
enum Rec {
    /// 経過秒と、activity ごとの item のバイト列 (並びは activity 一覧と同じ)。
    Stats(u64, Vec<Vec<Vec<u8>>>),
    /// 経過秒と `sa_cpu_nr` (CPU "all" を含む数)。
    Restart(u64, i32),
    /// 経過秒と本文。
    Comment(u64, &'static str),
}

fn hms(t: u64) -> (u8, u8, u8) {
    (12, (t / 60) as u8, (t % 60) as u8)
}

/// activity 一覧とレコード列から sa ファイルを組み立てる。
fn sa_file(acts: &[ActivitySpec], recs: &[Rec], tzname: &str) -> SaFile {
    let mut spec = FixtureSpec::skeleton(Generation::G2175Current, FixtureAbi::Le64);
    spec.ust_time = BASE;
    spec.tzname = tzname.to_string();
    spec.activities = acts.to_vec();
    spec.records = recs
        .iter()
        .map(|r| match r {
            Rec::Stats(t, items) => {
                let counts = acts
                    .iter()
                    .zip(items)
                    .map(|(a, it)| it.len() as i32 / a.nr2)
                    .collect();
                let (h, m, s) = hms(*t);
                let mut rs = RecordSpec::stats(counts, BASE + t, h, m, s);
                rs.uptime = 100_000 + t * 100;
                rs
            }
            Rec::Restart(t, cpu_nr) => {
                let (h, m, s) = hms(*t);
                RecordSpec::restart(*cpu_nr, BASE + t, h, m, s)
            }
            Rec::Comment(t, text) => {
                let (h, m, s) = hms(*t);
                RecordSpec::comment(text, BASE + t, h, m, s)
            }
        })
        .collect();
    let mut built = build(spec);
    let offsets = built.record_offsets.clone();
    for ((off, _), rec) in offsets.iter().zip(recs) {
        let Rec::Stats(_, items) = rec else {
            continue;
        };
        let mut at = off + RECORD_HEADER;
        for (act, its) in acts.iter().zip(items) {
            if act.has_nr {
                at += 4;
            }
            for it in its {
                assert_eq!(it.len(), act.size as usize, "item のサイズ");
                built.bytes[at..at + it.len()].copy_from_slice(it);
                at += it.len();
            }
        }
    }
    SaFile::from_bytes("synthetic", built.bytes).unwrap()
}

// ---- item のバイト列 (02-activities.md のオフセット表から) ----

fn put_u64(b: &mut [u8], off: usize, v: u64) {
    b[off..off + 8].copy_from_slice(&v.to_le_bytes());
}

fn put_u32(b: &mut [u8], off: usize, v: u32) {
    b[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

fn put_str(b: &mut [u8], off: usize, s: &str) {
    b[off..off + s.len()].copy_from_slice(s.as_bytes());
}

/// `stats_pcsw`: `context_switch` @0 (ull)、`processes` @8 (ul)。
fn pcsw(cswch: u64, procs: u64) -> Vec<u8> {
    let mut b = vec![0; 16];
    put_u64(&mut b, 0, cswch);
    put_u64(&mut b, 8, procs);
    b
}

/// `stats_cpu`: `user` @0、`nice` @8、`sys` @16、`idle` @24 (以降 0)。
fn cpu(user: u64, nice: u64, sys: u64, idle: u64) -> Vec<u8> {
    let mut b = vec![0; 80];
    put_u64(&mut b, 0, user);
    put_u64(&mut b, 8, nice);
    put_u64(&mut b, 16, sys);
    put_u64(&mut b, 24, idle);
    b
}

/// `stats_disk`: `nr_ios` @0、`rd_sect` @24、`major` @64、`minor` @68。
fn disk(major: u32, minor: u32, ios: u64, rd_sect: u64) -> Vec<u8> {
    let mut b = vec![0; 80];
    put_u64(&mut b, 0, ios);
    put_u64(&mut b, 24, rd_sect);
    put_u32(&mut b, 64, major);
    put_u32(&mut b, 68, minor);
    b
}

/// `stats_filesystem`: `f_blocks` @0、`f_bfree` @8、`f_bavail` @16、
/// `f_files` @24、`f_ffree` @32、`fs_name` @40、`mountp` @168。
fn fs(name: &str, mountp: &str) -> Vec<u8> {
    let mut b = vec![0; 296];
    // 1024 MiB のうち 256 MiB 空き
    put_u64(&mut b, 0, 1024 * 1024 * 1024);
    put_u64(&mut b, 8, 256 * 1024 * 1024);
    put_u64(&mut b, 16, 256 * 1024 * 1024);
    put_u64(&mut b, 24, 1000);
    put_u64(&mut b, 32, 400);
    put_str(&mut b, 40, name);
    put_str(&mut b, 168, mountp);
    b
}

fn a_disk(nr: i32) -> ActivitySpec {
    ActivitySpec {
        id: 11,
        magic: 0x8c,
        nr,
        nr2: 1,
        has_nr: true,
        size: 80,
        types_nr: [3, 3, 8],
    }
}

fn a_fs(nr: i32) -> ActivitySpec {
    ActivitySpec {
        id: 37,
        magic: 0x8b,
        nr,
        nr2: 1,
        has_nr: true,
        size: 296,
        types_nr: [5, 0, 0],
    }
}

/// 12.0.0 世代の `A_IRQ` (magic `0x8b`)。現行版には形式が未知 (`[Unknown format]`)。
fn a_irq_old(nr: i32) -> ActivitySpec {
    ActivitySpec {
        id: 3,
        magic: 0x8b,
        nr,
        nr2: 1,
        has_nr: true,
        size: 8,
        types_nr: [1, 0, 0],
    }
}

// ===========================================================================
// 出力
// ===========================================================================

type Writer = fn(&mut Vec<u8>, &SaFile, &SadfConfig, &SadfExtra) -> re_sar_ch::error::Result<()>;

const DB: Writer = sadf::dbppc::write_db_with;
const PPC: Writer = sadf::dbppc::write_ppc_with;
const JSON: Writer = sadf::json::write_json_with;
const XML: Writer = sadf::xml::write_xml_with;
const RAW: Writer = sadf::raw::write_raw_with;

fn emit_with(file: &SaFile, cfg: &SadfConfig, extra: &SadfExtra, w: Writer) -> String {
    let mut out = Vec::new();
    w(&mut out, file, cfg, extra).unwrap();
    String::from_utf8(out).unwrap()
}

fn emit(file: &SaFile, cfg: &SadfConfig, w: Writer) -> String {
    emit_with(file, cfg, &SadfExtra::default(), w)
}

fn only(ids: &[ActivityId]) -> SadfConfig {
    SadfConfig {
        activities: Some(ids.to_vec()),
        cpus: CpuSelection::All,
        ..Default::default()
    }
}

fn hhmmss(hour: u8, min: u8, sec: u8) -> TimeBound {
    TimeBound::HhMmSs { hour, min, sec }
}

fn range(start: TimeBound, end: TimeBound) -> TimeFilter {
    TimeFilter {
        start,
        end,
        basis: TimeBasis::Utc,
        cross_day: CrossDayRule::Sadf,
    }
}

// ===========================================================================
// 1. A_DISK の前サンプルは major / minor で対応付ける
// ===========================================================================

/// 2 レコード目でデバイスの並びが入れ替わり、新しいデバイスが増え、
/// 3 レコード目で 1 台が再登録 (全カウンタが減少) されるファイル。
fn disk_file() -> SaFile {
    sa_file(
        &[a_disk(3)],
        &[
            Rec::Stats(
                0,
                vec![vec![disk(8, 0, 100, 1000), disk(8, 16, 5000, 50000)]],
            ),
            Rec::Stats(
                10,
                vec![vec![
                    disk(8, 16, 5100, 50200),
                    disk(8, 0, 110, 1040),
                    disk(8, 32, 30, 600),
                ]],
            ),
            Rec::Stats(
                20,
                vec![vec![
                    disk(8, 16, 5200, 50400),
                    disk(8, 0, 5, 10),
                    disk(8, 32, 40, 800),
                ]],
            ),
        ],
        "UTC",
    )
}

/// **回帰テスト (バグ 1)**: 位置ではなく `major` / `minor` で前値を引く
/// (`check_disk_reg()`)。
///
/// 12:00:10 の期待値 (itv = 10 秒):
///
/// | デバイス | 前値 | tps = Δnr_ios / 10 | rkB/s = Δrd_sect / 10 / 2 |
/// |---|---|---|---|
/// | dev8-16 | 5000 / 50000 | 10.00 | 10.00 |
/// | dev8-0 | 100 / 1000 | 1.00 | 2.00 |
/// | dev8-32 (新規 = 前値 0) | 0 / 0 | 3.00 | 30.00 |
///
/// 位置で対応付けると dev8-16 の前値が dev8-0 の 100 になり tps = 500.00 に化ける。
#[test]
fn disk_previous_sample_is_matched_by_major_minor() {
    let file = disk_file();
    let cfg = only(&[ActivityId::DISK]);
    let d = emit(&file, &cfg, DB);
    for want in [
        "testhost;10;2020-09-13 12:00:10 UTC;dev8-16;10.00;10.00;",
        "testhost;10;2020-09-13 12:00:10 UTC;dev8-0;1.00;2.00;",
        "testhost;10;2020-09-13 12:00:10 UTC;dev8-32;3.00;30.00;",
    ] {
        assert!(d.contains(want), "{want}\n{d}");
    }

    // JSON も同じ対応付け (rd_sec はセクタ/秒 = Δrd_sect / 10)
    let j = emit(&file, &cfg, JSON);
    assert!(
        j.contains("{\"disk-device\": \"dev8-16\", \"tps\": 10.00, \"rd_sec\": 20.00,"),
        "{j}"
    );

    // raw の前値も相手のもの。新規デバイスの前値は 0
    let r = emit(&file, &cfg, RAW);
    assert!(
        r.contains(
            "12:00:10 UTC; major; 8; minor; 16; DEV; dev8-16; tps; 5000; 5100; rkB/s; 50000; 50200;"
        ),
        "{r}"
    );
    assert!(
        r.contains("12:00:10 UTC; major; 8; minor; 32; DEV; dev8-32; tps; 0; 30; rkB/s; 0; 600;"),
        "{r}"
    );
}

/// **回帰テスト (バグ 1)**: 全カウンタが減ったデバイスは再登録とみなして前値 0
/// (`check_disk_reg()` の `-2`)。`-O debug` では ` [NEW]` / ` [BCK]` が付く。
#[test]
fn reregistered_disk_starts_from_zero_and_is_marked_in_debug() {
    let file = disk_file();
    let cfg = only(&[ActivityId::DISK]);
    let d = emit(&file, &cfg, DB);
    // dev8-0 は 110 → 5 / 1040 → 10 と全部減った: 前値 0 で 5/10、10/10/2
    assert!(
        d.contains("testhost;10;2020-09-13 12:00:20 UTC;dev8-0;0.50;0.50;"),
        "{d}"
    );

    let debug = SadfConfig { debug: true, ..cfg };
    let r = emit(&file, &debug, RAW);
    assert!(r.contains("; DEV [NEW]; dev8-32;"), "{r}");
    assert!(r.contains("; DEV [BCK]; dev8-0; tps; 0; 5;"), "{r}");
    assert!(
        r.contains("12:00:20 UTC; major; 8; minor; 16; DEV; dev8-16; tps; 5100; 5200;"),
        "印が付かないデバイスはそのまま: {r}"
    );
}

// ===========================================================================
// 2. A_FS の表示名と --fs=
// ===========================================================================

fn fs_file() -> SaFile {
    let items = || vec![vec![fs("/dev/sda1", "/home"), fs("/dev/sda2", "")]];
    sa_file(
        &[a_fs(2)],
        &[Rec::Stats(0, items()), Rec::Stats(10, items())],
        "UTC",
    )
}

fn mount_cfg() -> SadfConfig {
    SadfConfig {
        section: SectionConfig {
            fs_mount: true,
            ..Default::default()
        },
        ..only(&[ActivityId::FS])
    }
}

/// **回帰テスト (バグ 2)**: `-F MOUNT` でマウントポイントが空のとき、
/// item 番号ではなく空文字を出す (本家は 0 埋めの文字列をそのまま出す)。
#[test]
fn empty_mountpoint_is_printed_as_empty_string() {
    let file = fs_file();
    let cfg = mount_cfg();
    // MBfsfree = 256、MBfsused = 768、%fsused = 75.00、%ufsused = 75.00、
    // Ifree = 400、Iused = 600、%Iused = 60.00
    let j = emit(&file, &cfg, JSON);
    assert!(
        j.contains("{\"mountpoint\": \"\", \"MBfsfree\": 256, \"MBfsused\": 768,"),
        "{j}"
    );
    let x = emit(&file, &cfg, XML);
    assert!(
        x.contains("<filesystem mountp=\"\" MBfsfree=\"256\""),
        "{x}"
    );
    // -d は識別子の列が空になる (列そのものは消えない)
    let d = emit(&file, &cfg, DB);
    assert!(
        d.contains("testhost;10;2020-09-13 12:00:10 UTC;;256;768;75.00;75.00;400;600;60.00"),
        "{d}"
    );
    let r = emit(&file, &cfg, RAW);
    assert!(r.contains("; MOUNTPOINT; \"\";"), "{r}");
}

/// **回帰テスト (バグ 2)**: `--fs=` は表示名だけでなくデバイス名と
/// マウントポイントのどちらにも当たる (`match_sa_filesystem_item()`)。
#[test]
fn fs_selection_matches_the_mountpoint_under_plain_f() {
    let file = fs_file();
    let cfg = SadfConfig {
        item_names: BTreeMap::from([(ActivityId::FS, vec!["/home".to_string()])]),
        ..only(&[ActivityId::FS])
    };
    let d = emit(&file, &cfg, DB);
    assert!(d.contains(";/dev/sda1;256;"), "{d}");
    assert!(!d.contains("/dev/sda2"), "{d}");
    // -F MOUNT でもデバイス名で当たる
    let cfg = SadfConfig {
        item_names: BTreeMap::from([(ActivityId::FS, vec!["/dev/sda1".to_string()])]),
        ..mount_cfg()
    };
    let d = emit(&file, &cfg, DB);
    assert!(d.contains(";/home;256;"), "{d}");
    assert_eq!(d.lines().filter(|l| !l.starts_with('#')).count(), 1, "{d}");
}

/// **回帰テスト (バグ 13)**: 絞り込みで item が 0 件になっても、
/// そのレコードに item があれば activity の器は出る。
#[test]
fn empty_selection_keeps_the_activity_wrapper() {
    let file = fs_file();
    let cfg = SadfConfig {
        item_names: BTreeMap::from([(ActivityId::FS, vec!["zz".to_string()])]),
        ..only(&[ActivityId::FS])
    };
    let j = emit(&file, &cfg, JSON);
    assert!(
        j.contains("\t\t\t\t\t\"filesystems\": [\n\n\t\t\t\t\t]\n"),
        "{j}"
    );
    serde_json::from_str::<serde_json::Value>(&j).expect("妥当な JSON");
    let x = emit(&file, &cfg, XML);
    assert!(
        x.contains("\t\t\t\t<filesystems>\n\t\t\t\t</filesystems>\n"),
        "{x}"
    );
}

// ===========================================================================
// 3〜5. RESTART / COMMENT と -s / -e / -C / -dh
// ===========================================================================

/// `A_PCSW` だけの 2 区間 (RESTART 1 回) + COMMENT 2 本。
///
/// 各統計レコードは processes +10、context_switch +100 ずつ増える
/// (10 秒間隔なので proc/s = 1.00、cswch/s = 10.00)。
fn pcsw_file(tzname: &str) -> SaFile {
    let s = |t: u64, k: u64| Rec::Stats(t, vec![vec![pcsw(1000 + 100 * k, 50 + 10 * k)]]);
    sa_file(
        &[ActivitySpec::a_pcsw()],
        &[
            s(0, 0),
            s(10, 1),
            Rec::Comment(15, "c1"),
            s(20, 2),
            Rec::Restart(30, 3),
            s(40, 0),
            s(50, 1),
            Rec::Comment(55, "c2"),
            s(60, 2),
        ],
        tzname,
    )
}

const PCSW_HDR: &str = "# hostname;interval;timestamp;proc/s;cswch/s";

/// **回帰テスト (バグ 3)**: `-e` を超えた RESTART / COMMENT は出ない
/// (`print_special_record()` の範囲判定)。フィールド名一覧行も、範囲内の
/// 統計レコードを持たない区間には出ない。
#[test]
fn end_time_hides_later_restarts_and_comments() {
    let file = pcsw_file("UTC");
    let cfg = SadfConfig {
        comments: true,
        time_filter: range(TimeBound::None, hhmmss(12, 0, 20)),
        ..only(&[ActivityId::PCSW])
    };
    let d = emit(&file, &cfg, DB);
    assert_eq!(
        d,
        format!(
            "{PCSW_HDR}\n\
             testhost;10;2020-09-13 12:00:10 UTC;1.00;10.00\n\
             testhost;-1;2020-09-13 12:00:15 UTC;COM c1\n\
             testhost;10;2020-09-13 12:00:20 UTC;1.00;10.00\n"
        )
    );
    // -j / -x の restarts / comments も範囲外を出さない
    let j = emit(&file, &cfg, JSON);
    assert!(j.contains("\"restarts\": [\n\t\t\t],"), "{j}");
    assert!(j.contains("\"com\": \"c1\""), "{j}");
    assert!(!j.contains("\"com\": \"c2\""), "{j}");
    let x = emit(&file, &cfg, XML);
    assert!(x.contains("<restarts>\n\t\t</restarts>"), "{x}");
}

/// **回帰テスト (バグ 3)**: `-s` より前の RESTART / COMMENT も出ない。
/// 範囲内の最初の統計レコード (12:00:40) は基準として消費されるだけ。
#[test]
fn start_time_hides_earlier_restarts_and_comments() {
    let file = pcsw_file("UTC");
    let cfg = SadfConfig {
        comments: true,
        time_filter: range(hhmmss(12, 0, 35), TimeBound::None),
        ..only(&[ActivityId::PCSW])
    };
    let d = emit(&file, &cfg, DB);
    assert_eq!(
        d,
        format!(
            "{PCSW_HDR}\n\
             testhost;10;2020-09-13 12:00:50 UTC;1.00;10.00\n\
             testhost;-1;2020-09-13 12:00:55 UTC;COM c2\n\
             testhost;10;2020-09-13 12:01:00 UTC;1.00;10.00\n"
        )
    );
    let r = emit(&file, &cfg, RAW);
    assert!(!r.contains("LINUX-RESTART"), "{r}");
    assert!(r.contains("12:00:55 UTC; COM c2\n"), "{r}");
}

/// **回帰テスト (バグ 4)**: 基準レコードより前の COMMENT は区間の先頭で 1 回だけ、
/// 基準より後の COMMENT は activity ごとに出る (`logic2`)。
#[test]
fn comment_before_the_reference_is_printed_once() {
    let rec = |t: u64, k: u64| {
        Rec::Stats(
            t,
            vec![
                vec![
                    cpu(100 * k, 0, 0, 1000 + 100 * k),
                    cpu(50 * k, 0, 0, 500 + 50 * k),
                    cpu(50 * k, 0, 0, 500 + 50 * k),
                ],
                vec![pcsw(1000 + 100 * k, 50 + 10 * k)],
            ],
        )
    };
    let file = sa_file(
        &[ActivitySpec::a_cpu(3), ActivitySpec::a_pcsw()],
        &[
            Rec::Comment(0, "c0"),
            rec(5, 0),
            rec(15, 1),
            Rec::Comment(20, "c1"),
            rec(25, 2),
        ],
        "UTC",
    );
    let cfg = SadfConfig {
        comments: true,
        section: SectionConfig {
            cpu_all: false,
            ..Default::default()
        },
        // CPU 行は "all" だけにして、行の種類を数えやすくする
        cpus: CpuSelection::Aggregate,
        ..only(&[ActivityId::CPU, ActivityId::PCSW])
    };
    let d = emit(&file, &cfg, DB);
    let kinds: Vec<&str> = d
        .lines()
        .map(|l| {
            if l.contains("COM c0") {
                "c0"
            } else if l.contains("COM c1") {
                "c1"
            } else if l.starts_with("# ") && l.contains(";CPU;") {
                "hdr-cpu"
            } else if l.starts_with("# ") {
                "hdr-pcsw"
            } else if l.contains(";-1;") {
                "cpu"
            } else {
                "pcsw"
            }
        })
        .collect();
    assert_eq!(
        kinds,
        vec![
            "c0", "hdr-cpu", "cpu", "c1", "cpu", "hdr-pcsw", "pcsw", "c1", "pcsw"
        ],
        "{d}"
    );
    // ppc / raw も同じ規則 (COMMENT は 1 + 2 回)
    assert_eq!(emit(&file, &cfg, PPC).matches("COM c0").count(), 1);
    assert_eq!(emit(&file, &cfg, RAW).matches("COM c1").count(), 2);
}

/// **回帰テスト (バグ 5)**: `-dh` のフィールド名一覧行は区間ごとに出る。
#[test]
fn horizontal_db_repeats_the_field_list_after_a_restart() {
    let file = pcsw_file("UTC");
    let cfg = SadfConfig {
        horizontally: true,
        ..only(&[ActivityId::PCSW])
    };
    let d = emit(&file, &cfg, DB);
    assert_eq!(
        d,
        format!(
            "{PCSW_HDR}\n\
             testhost;10;2020-09-13 12:00:10 UTC;1.00;10.00\n\
             testhost;10;2020-09-13 12:00:20 UTC;1.00;10.00\n\
             testhost;-1;2020-09-13 12:00:30 UTC;LINUX-RESTART\t(2 CPU)\n\
             {PCSW_HDR}\n\
             testhost;10;2020-09-13 12:00:50 UTC;1.00;10.00\n\
             testhost;10;2020-09-13 12:01:00 UTC;1.00;10.00\n"
        )
    );
}

// ===========================================================================
// 6. -t で sa_tzname が空
// ===========================================================================

/// **回帰テスト (バグ 6)**: 統計行は TZ 欄を空白ごと省き、RESTART / COMMENT 行は
/// 空白を残す (`print_dbppc_timestamp()` だけが `strlen(sa_tzname)` を見る)。
#[test]
fn true_time_without_tzname_drops_the_label_only_on_data_lines() {
    let file = pcsw_file("");
    let cfg = SadfConfig {
        time_base: TimeBase::TrueTime,
        comments: true,
        ..only(&[ActivityId::PCSW])
    };
    let d = emit(&file, &cfg, DB);
    assert!(
        d.contains("testhost;10;2020-09-13 12:00:10;1.00;10.00\n"),
        "{d}"
    );
    assert!(
        d.contains("testhost;-1;2020-09-13 12:00:15 ;COM c1\n"),
        "{d}"
    );
    assert!(
        d.contains("testhost;-1;2020-09-13 12:00:30 ;LINUX-RESTART\t(2 CPU)\n"),
        "{d}"
    );
    let r = emit(&file, &cfg, RAW);
    assert!(r.contains("12:00:10; proc/s; 50; 60;"), "{r}");
    assert!(r.contains("12:00:30 ; LINUX-RESTART (2 CPU)\n"), "{r}");
    // JSON / XML は判定を持たないので tz="" のまま
    assert!(emit(&file, &cfg, JSON).contains("\"tz\": \"\", \"interval\": 10}"));
}

// ===========================================================================
// 12. positional の interval / count
// ===========================================================================

/// 10 秒間隔の `A_PCSW` 5 本 (12:00:00〜12:00:40) の区間を 2 つ。
fn pcsw_series() -> SaFile {
    let s = |t: u64, k: u64| Rec::Stats(t, vec![vec![pcsw(1000 + 100 * k, 50 + 10 * k)]]);
    let mut recs: Vec<Rec> = (0..5).map(|k| s(10 * k, k)).collect();
    recs.push(Rec::Restart(50, 3));
    recs.extend((0..3).map(|k| s(60 + 10 * k, k)));
    sa_file(&[ActivitySpec::a_pcsw()], &recs, "UTC")
}

/// **回帰テスト (バグ 12)**: `interval` は `next_slice()` で間引き、省いたレコードは
/// 前サンプルにならない (差分は最後に出したレコードとの間で取る)。
///
/// 基準 12:00:00 から interval 20: entry 10 → [5,15) に 20 の倍数なし、
/// entry 20 → [15,25) に 20 → 出す (区間 20 秒)、30 → 出さない、40 → 出す。
#[test]
fn interval_skips_records_and_keeps_the_last_shown_as_previous() {
    let file = pcsw_series();
    let cfg = only(&[ActivityId::PCSW]);
    let extra = SadfExtra {
        select: RecordSelect {
            interval: 20,
            count: None,
        },
        ..Default::default()
    };
    let d = emit_with(&file, &cfg, &extra, DB);
    assert_eq!(
        d,
        format!(
            "{PCSW_HDR}\n\
             testhost;20;2020-09-13 12:00:20 UTC;1.00;10.00\n\
             testhost;20;2020-09-13 12:00:40 UTC;1.00;10.00\n\
             testhost;-1;2020-09-13 12:00:50 UTC;LINUX-RESTART\t(2 CPU)\n\
             {PCSW_HDR}\n\
             testhost;20;2020-09-13 12:01:20 UTC;1.00;10.00\n"
        )
    );
}

/// **回帰テスト (バグ 12)**: `count` は区間ごとに数え直す。
#[test]
fn count_limits_each_block() {
    let file = pcsw_series();
    let cfg = only(&[ActivityId::PCSW]);
    let extra = SadfExtra {
        select: RecordSelect {
            interval: 1,
            count: Some(1),
        },
        ..Default::default()
    };
    let d = emit_with(&file, &cfg, &extra, DB);
    assert_eq!(
        d,
        format!(
            "{PCSW_HDR}\n\
             testhost;10;2020-09-13 12:00:10 UTC;1.00;10.00\n\
             testhost;-1;2020-09-13 12:00:50 UTC;LINUX-RESTART\t(2 CPU)\n\
             {PCSW_HDR}\n\
             testhost;10;2020-09-13 12:01:10 UTC;1.00;10.00\n"
        )
    );
}

/// **回帰テスト (バグ 12)**: JSON は `next_slice()` で省いたレコードでも
/// 空のオブジェクトを出す (`f_statistics(F_MAIN)` が先に `{` を出すため)。
#[test]
fn json_keeps_an_empty_object_for_each_skipped_record() {
    let file = pcsw_series();
    let cfg = only(&[ActivityId::PCSW]);
    let extra = SadfExtra {
        select: RecordSelect {
            interval: 20,
            count: None,
        },
        ..Default::default()
    };
    let j = emit_with(&file, &cfg, &extra, JSON);
    let v: serde_json::Value = serde_json::from_str(&j).expect("妥当な JSON");
    let stats = v["sysstat"]["hosts"][0]["statistics"].as_array().unwrap();
    let shape: Vec<bool> = stats
        .iter()
        .map(|o| o.as_object().unwrap().is_empty())
        .collect();
    // 区間 1: 10 (省く) / 20 / 30 (省く) / 40、区間 2: 70 (省く) / 80
    assert_eq!(shape, vec![true, false, true, false, true, false], "{j}");
    assert!(j.contains("\t\t\t\t{\n\t\t\t\t},\n"), "{j}");
    // XML は跡を残さない
    let x = emit_with(&file, &cfg, &extra, XML);
    assert_eq!(x.matches("<timestamp ").count(), 3, "{x}");
}

// ===========================================================================
// 14. 形式が未知の activity だけを選ぶ
// ===========================================================================

fn unknown_irq_file() -> SaFile {
    let rec = |t: u64, k: u64| {
        Rec::Stats(
            t,
            vec![
                vec![cpu(10 * k, 0, 0, 100 * k); 3],
                vec![
                    {
                        let mut b = vec![0; 8];
                        put_u64(&mut b, 0, k);
                        b
                    };
                    2
                ],
            ],
        )
    };
    sa_file(
        &[ActivitySpec::a_cpu(3), a_irq_old(2)],
        &[Rec::Restart(0, 3), rec(1, 1), rec(11, 2)],
        "UTC",
    )
}

/// **回帰テスト (バグ 14)**: 形式が未知の activity しか選ばなくてもエラーにしない。
/// `-d` は RESTART 行だけ、`-j` はタイムスタンプだけのオブジェクトになる。
#[test]
fn unknown_format_only_selection_prints_restart_and_timestamps() {
    let file = unknown_irq_file();
    let cfg = only(&[ActivityId::IRQ]);
    assert!(sadf::dbppc::any_selected_in_file(&file, &cfg));
    assert!(sadf::dbppc::selected_specs(&file, &cfg).is_empty());

    let d = emit(&file, &cfg, DB);
    assert_eq!(
        d,
        "testhost;-1;2020-09-13 12:00:00 UTC;LINUX-RESTART\t(2 CPU)\n"
    );
    let j = emit(&file, &cfg, JSON);
    assert!(
        j.contains(
            "\t\t\t\t{\n\t\t\t\t\t\"timestamp\": {\"date\": \"2020-09-13\", \"time\": \"12:00:11\", \"tz\": \"UTC\", \"interval\": 10}\n\t\t\t\t}\n"
        ),
        "{j}"
    );
}

/// **回帰テスト (バグ 14)**: CLI も正常終了する。ファイルに載っていない activity を
/// 選んだときは従来どおり「Requested activities not available」で失敗する。
#[test]
fn cli_accepts_an_unknown_format_only_selection() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("unknown-irq.sa");
    let mut spec = FixtureSpec::skeleton(Generation::G2175Current, FixtureAbi::Le64);
    spec.ust_time = BASE;
    spec.activities = vec![ActivitySpec::a_cpu(3), a_irq_old(2)];
    spec.records = vec![
        RecordSpec::restart(3, BASE, 12, 0, 0),
        RecordSpec::stats(vec![3, 2], BASE + 1, 12, 0, 1),
    ];
    std::fs::write(&path, build(spec).bytes).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_resarch"))
        .args(["sadf", "-d"])
        .arg(&path)
        .args(["--", "-I", "ALL"])
        .env("TZ", "UTC")
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "testhost;-1;2020-09-13 12:00:00 UTC;LINUX-RESTART\t(2 CPU)\n"
    );

    let out = Command::new(env!("CARGO_BIN_EXE_resarch"))
        .args(["sadf", "-d"])
        .arg(&path)
        .args(["--", "-d"])
        .env("TZ", "UTC")
        .output()
        .unwrap();
    assert!(!out.status.success(), "A_DISK はファイルに無い: {out:?}");
}

// ===========================================================================
// 11 / 15. raw の CPU 走査と -O debug
// ===========================================================================

/// `A_CPU` は採取時 3 枠 (all + CPU0 + CPU1)。2 本目以降は CPU1 が消える。
fn offline_cpu_file() -> SaFile {
    sa_file(
        &[ActivitySpec::a_cpu(3)],
        &[
            Rec::Stats(
                0,
                vec![vec![
                    cpu(30, 0, 3, 300),
                    cpu(20, 0, 2, 200),
                    cpu(10, 0, 1, 100),
                ]],
            ),
            Rec::Stats(10, vec![vec![cpu(60, 0, 6, 600), cpu(40, 0, 4, 400)]]),
            Rec::Stats(20, vec![vec![cpu(90, 0, 9, 900), cpu(40, 0, 4, 400)]]),
        ],
        "UTC",
    )
}

fn raw_cpu_cfg(debug: bool) -> SadfConfig {
    SadfConfig {
        debug,
        section: SectionConfig {
            cpu_all: false,
            ..Default::default()
        },
        ..only(&[ActivityId::CPU])
    }
}

/// **回帰テスト (バグ 11)**: raw は `nr_ini` (採取時の CPU 数) まで回し、
/// レコードに無い CPU は 0 の値で出す (`raw_print_cpu_stats()`)。
/// 前値も同じで、前のレコードに無かった CPU は 0。
#[test]
fn raw_prints_missing_cpus_with_zero_values() {
    let file = offline_cpu_file();
    let r = emit(&file, &raw_cpu_cfg(false), RAW);
    assert!(
        r.contains("12:00:10 UTC; CPU; 1; %user; 10; 0; %nice; 0; 0; %system; 1; 0; %iowait; 0; 0; %steal; 0; 0; %idle; 100; 0;\n"),
        "{r}"
    );
    assert!(
        r.contains("12:00:20 UTC; CPU; 1; %user; 0; 0; %nice; 0; 0; %system; 0; 0; %iowait; 0; 0; %steal; 0; 0; %idle; 0; 0;\n"),
        "{r}"
    );
    // 他の形式はオフライン CPU を出さない
    assert!(!emit(&file, &raw_cpu_cfg(false), DB).contains(";1;"));
}

/// **回帰テスト (バグ 15)**: `-O debug` の構造。
///
/// - レコードヘッダを読むたびに `# uptime_cs` 行 (基準レコードの 12:00:00 も出る)
/// - 表示するレコードごとに `# name` 行 (`nr_curr` はそのレコードの item 数)
/// - tick 和が 0 の CPU は ` [OFF]`、tick 差分が 0 の CPU は ` [TLS]`
#[test]
fn raw_debug_reports_headers_counts_and_cpu_states() {
    let file = offline_cpu_file();
    let r = emit(&file, &raw_cpu_cfg(true), RAW);
    let lines: Vec<&str> = r.lines().collect();
    assert_eq!(
        lines[0],
        "# uptime_cs; 100000; ust_time; 1599998400; extra_next; 0; record_type; 1; HH:MM:SS; 12:00:00"
    );
    assert_eq!(
        lines[1],
        "# uptime_cs; 101000; ust_time; 1599998410; extra_next; 0; record_type; 1; HH:MM:SS; 12:00:10"
    );
    assert_eq!(
        lines[2],
        "# name; A_CPU; nr_curr; 2; nr_alloc; 3; nr_ini; 3"
    );
    assert!(lines[5].starts_with("12:00:10 UTC; CPU [OFF]; 1;"), "{r}");
    // 12:00:20 の CPU0 は tick が 1 つも進んでいない (40 / 4 / 400 のまま)
    assert!(r.contains("12:00:20 UTC; CPU [TLS]; 0;"), "{r}");
    assert!(r.contains("12:00:20 UTC; CPU [OFF]; 1;"), "{r}");
}

/// **回帰テスト (バグ 11 と同じ系統)**: 周波数 0 (オフライン) の CPU を飛ばすのは
/// `-d` / `-p` (`render_pwr_cpufreq_stats()`) だけ。`-r` / `-j` / `-x` は
/// `cpufreq` を見ずに全 CPU を出す。
#[test]
fn only_db_and_ppc_skip_cpus_whose_frequency_is_zero() {
    // `stats_pwr_cpufreq`: `cpufreq` @0 (ul、MHz × 100)
    let freq = |mhz100: u64| {
        let mut b = vec![0; 8];
        put_u64(&mut b, 0, mhz100);
        b
    };
    let items = || vec![vec![freq(150_000), freq(300_000), freq(0)]];
    let file = sa_file(
        &[ActivitySpec {
            id: 30,
            magic: 0x8a,
            nr: 3,
            nr2: 1,
            has_nr: true,
            size: 8,
            types_nr: [0, 1, 0],
        }],
        &[Rec::Stats(0, items()), Rec::Stats(10, items())],
        "UTC",
    );
    let cfg = only(&[ActivityId::PWR_CPU]);
    let r = emit(&file, &cfg, RAW);
    assert_eq!(
        r,
        "12:00:10 UTC; CPU; -1; MHz; 150000;\n\
         12:00:10 UTC; CPU; 0; MHz; 300000;\n\
         12:00:10 UTC; CPU; 1; MHz; 0;\n"
    );
    let d = emit(&file, &cfg, DB);
    assert!(d.contains(";0;3000.00\n"), "{d}");
    assert!(!d.contains(";1;"), "{d}");
    let j = emit(&file, &cfg, JSON);
    assert!(
        j.contains("{\"number\": \"1\", \"frequency\": 0.00}"),
        "{j}"
    );
    let x = emit(&file, &cfg, XML);
    assert!(
        x.contains("<cpufreq number=\"1\" frequency=\"0.00\"/>"),
        "{x}"
    );
}

// ===========================================================================
// 7. -T のタイムゾーン名 (CLI 層で解決する)
// ===========================================================================

/// **回帰テスト (バグ 7)**: `-T` のラベルは `tzname[0]` = 標準時の略称。
/// 2020-09-13 は Europe/Paris の夏時間 (UTC+2) だが、ラベルは `CET`。
#[cfg(unix)]
#[test]
fn local_time_label_is_the_standard_zone_abbreviation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pcsw.sa");
    let mut spec = FixtureSpec::skeleton(Generation::G2175Current, FixtureAbi::Le64);
    spec.ust_time = BASE;
    spec.activities = vec![ActivitySpec::a_pcsw()];
    let mut a = RecordSpec::stats(vec![0], BASE, 12, 0, 0);
    a.uptime = 100_000;
    let mut b = RecordSpec::stats(vec![0], BASE + 10, 12, 0, 10);
    b.uptime = 101_000;
    spec.records = vec![a, b];
    std::fs::write(&path, build(spec).bytes).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_resarch"))
        .args(["sadf", "-d", "-T"])
        .arg(&path)
        .args(["--", "-w"])
        .env("TZ", "Europe/Paris")
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(";10;2020-09-13 14:00:10 CET;"),
        "時刻は CEST (UTC+2)、ラベルは CET: {stdout}"
    );
}

/// **回帰テスト (バグ 12)**: CLI の positional `interval` / `count` が効く。
#[test]
fn cli_applies_positional_interval_and_count() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pcsw.sa");
    let mut spec = FixtureSpec::skeleton(Generation::G2175Current, FixtureAbi::Le64);
    spec.ust_time = BASE;
    spec.activities = vec![ActivitySpec::a_pcsw()];
    spec.records = (0..5u64)
        .map(|k| {
            let mut r = RecordSpec::stats(vec![0], BASE + 10 * k, 12, 0, (10 * k) as u8);
            r.uptime = 100_000 + 1000 * k;
            r
        })
        .collect();
    std::fs::write(&path, build(spec).bytes).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_resarch"))
        .args(["sadf", "-d"])
        .arg(&path)
        .args(["20", "1", "--", "-w"])
        .env("TZ", "UTC")
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let data: Vec<&str> = stdout.lines().filter(|l| !l.starts_with('#')).collect();
    assert_eq!(data.len(), 1, "{stdout}");
    assert!(
        data[0].starts_with("testhost;20;2020-09-13 12:00:20 UTC;"),
        "{stdout}"
    );
}

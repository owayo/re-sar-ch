//! item の同一性 (前サンプルとの対応付け) の結合テスト。
//!
//! | # | テスト | 何を固定するか |
//! |---|---|---|
//! | 1 | [`disk_values_follow_major_minor_in_every_output`] | `A_DISK` の前サンプルを位置ではなく `major` / `minor` で引くこと。ディスクが消えても増えても、互換出力 (`sar` / `sadf -d` / `sadf -j`)・独自出力 (`show` の json / csv / table / ndjson)・集計 (`summarize`) が同じ区間値を出す (指摘 9) |
//! | 1' | [`irq_columns_are_paired_by_position_in_every_output`] | 集計も `A_IRQ` の前サンプルを `show` と同じ規則 (位置) で引くこと |
//!
//! fixture はすべて自作。統計のバイト列は `docs/format/02-activities.md` §5 / §6 の
//! オフセット表から独立に書き起こしたもので、本体のレイアウト定義は使わない。
//! 期待値は式から手で書いた。

mod fixtures;

use std::collections::BTreeMap;

use fixtures::{ActivitySpec, FixtureAbi, FixtureSpec, Generation, RecordSpec, build};
use re_sar_ch::analyze::summary::{NativePeriodSummary, RetainTimelines};
use re_sar_ch::analyze::timeline::MetricKey;
use re_sar_ch::format::file::SaFile;
use re_sar_ch::model::{ActivityId, DisplayTz};
use re_sar_ch::multi::{MultiOptions, analyze_files};
use re_sar_ch::output::json::CustomConfig;
use re_sar_ch::output::sadf::{self, SadfConfig};
use re_sar_ch::output::sar_text::{CpuSelection, SarTextOptions, TimeStyle, write_report};
use re_sar_ch::series::Selection;
use serde_json::Value;

/// 最初のレコードの時刻 (2020-09-13 12:26:40 UTC)。以降 1 秒ずつ進める。
const START: u64 = 1_600_000_000;

// ===========================================================================
// fixture
// ===========================================================================

/// 現行世代 (`0x2175`) のファイルを組み、統計の item を手書きのバイト列で置き換える。
///
/// `samples[i]` が i 番目のレコードの item 群。レコードは 1 秒間隔で、
/// `uptime` も 100 cs ずつ進める (起動時刻が揃うので 1 つの起動区間になる)。
/// `has_nr` の activity はレコードごとに item 数を変えられる。
fn file(activity: ActivitySpec, samples: Vec<Vec<Vec<u8>>>) -> SaFile {
    let mut spec = FixtureSpec::skeleton(Generation::G2175Current, FixtureAbi::Le64);
    spec.activities = vec![activity.clone()];
    spec.records = samples
        .iter()
        .enumerate()
        .map(|(i, items)| {
            let count = items.len() as i32 / activity.nr2;
            let mut rec = RecordSpec::stats(vec![count], START + i as u64, 12, 26, 40 + i as u8);
            rec.uptime = 100_000 + i as u64 * 100;
            rec
        })
        .collect();
    let mut built = build(spec);
    for ((off, _), items) in built.record_offsets.iter().zip(samples) {
        // record_header (24 バイト) と item 数 (`__nr_t`、4 バイト) の後ろ
        let mut at = off + 24 + if activity.has_nr { 4 } else { 0 };
        for item in items {
            assert_eq!(item.len(), activity.size as usize);
            built.bytes[at..at + item.len()].copy_from_slice(&item);
            at += item.len();
        }
    }
    SaFile::from_bytes("synthetic", built.bytes).unwrap()
}

/// `A_DISK` (id 11、magic `0x8c`、80 バイト、`types_nr = (3,3,8)`)。
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

/// `stats_disk` 1 台分。
///
/// オフセット (LP64): `nr_ios` 0 / `rd_sect` 24 / `wr_sect` 32 / `tot_ticks` 56 /
/// `major` 64 / `minor` 68。それ以外 (`wwn` / `dc_sect` / 他の tick) は 0。
fn disk(major: u32, minor: u32, ios: u64, rd_sect: u64, wr_sect: u64, tot_ticks: u32) -> Vec<u8> {
    let mut out = vec![0; 80];
    out[0..8].copy_from_slice(&ios.to_le_bytes());
    out[24..32].copy_from_slice(&rd_sect.to_le_bytes());
    out[32..40].copy_from_slice(&wr_sect.to_le_bytes());
    out[56..60].copy_from_slice(&tot_ticks.to_le_bytes());
    out[64..68].copy_from_slice(&major.to_le_bytes());
    out[68..72].copy_from_slice(&minor.to_le_bytes());
    out
}

// ===========================================================================
// 出力を取り出すヘルパ
// ===========================================================================

fn show_config(id: ActivityId) -> CustomConfig {
    CustomConfig {
        selection: Selection::Only(vec![id]),
        tz: DisplayTz::Utc,
        ..Default::default()
    }
}

type Writer = fn(&mut Vec<u8>, &SaFile, &CustomConfig) -> re_sar_ch::Result<()>;

fn show(file: &SaFile, cfg: &CustomConfig, write: Writer) -> String {
    let mut out = Vec::new();
    write(&mut out, file, cfg).unwrap();
    String::from_utf8(out).unwrap()
}

fn show_csv(file: &SaFile, cfg: &CustomConfig) -> String {
    let mut out = Vec::new();
    re_sar_ch::output::csv::write_csv(&mut out, file, cfg).unwrap();
    String::from_utf8(out).unwrap()
}

/// `show --format json` の区間値 (`end_epoch`, item) → 列名 → (値, 品質)。
type ShowValues = BTreeMap<(u64, String), BTreeMap<String, (Option<f64>, String)>>;

fn show_json_values(file: &SaFile, cfg: &CustomConfig, space: &str) -> ShowValues {
    let json: Value = serde_json::from_str(&show(file, cfg, re_sar_ch::output::json::write_json))
        .expect("独自 JSON");
    let mut out = ShowValues::new();
    for sample in json["samples"].as_array().unwrap() {
        let end = sample["end_epoch"].as_u64().unwrap();
        for activity in sample["activities"].as_array().unwrap() {
            for item in activity["items"].as_array().unwrap() {
                let label = item["item"].as_str().unwrap().to_string();
                let fields = out.entry((end, label)).or_default();
                for f in item[space].as_array().into_iter().flatten() {
                    let value = f["value"]
                        .as_f64()
                        .or_else(|| f["raw"].as_str().and_then(|r| r.parse().ok()));
                    fields.insert(
                        f["name"].as_str().unwrap().to_string(),
                        (value, f["quality"].as_str().unwrap().to_string()),
                    );
                }
            }
        }
    }
    out
}

fn sadf_config(id: ActivityId) -> SadfConfig {
    SadfConfig {
        activities: Some(vec![id]),
        cpus: CpuSelection::All,
        ..Default::default()
    }
}

fn sadf_out(
    file: &SaFile,
    cfg: &SadfConfig,
    write: fn(&mut Vec<u8>, &SaFile, &SadfConfig) -> re_sar_ch::Result<()>,
) -> String {
    let mut out = Vec::new();
    write(&mut out, file, cfg).unwrap();
    String::from_utf8(out).unwrap()
}

fn sar_text(file: &SaFile, id: ActivityId) -> String {
    let mut out = Vec::new();
    // 既定は読み手のローカル時刻なので、実行環境に依らないよう UTC に固定する
    let opts = SarTextOptions {
        cpus: CpuSelection::All,
        time: TimeStyle::Utc,
        ..Default::default()
    };
    write_report(&mut out, file, &opts, &[id]).unwrap();
    String::from_utf8(out).unwrap()
}

/// `summarize` と同じ経路 (複数ファイル解析) で集計する。
fn summarize(file: &SaFile, id: ActivityId) -> NativePeriodSummary {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sa");
    std::fs::write(&path, file.bytes()).unwrap();
    let mut opts = MultiOptions {
        selection: Selection::Only(vec![id]),
        ..Default::default()
    };
    // `detect` が読む区間値の時系列も確かめる
    opts.summary.retain = RetainTimelines::All;
    let result = analyze_files(&[path], &opts).unwrap();
    assert_eq!(result.hosts.len(), 1);
    assert_eq!(
        result.hosts[0].segments.len(),
        1,
        "1 つの起動区間として集計される"
    );
    result.hosts[0].segments[0].summary.clone()
}

/// 集計の区間値の時系列 (値のある点だけ)。
fn observed(s: &NativePeriodSummary, id: ActivityId, item: &str, column: &str) -> Vec<f64> {
    s.timelines
        .get(&MetricKey::new(id, item, column))
        .unwrap_or_else(|| panic!("{item} {column} の時系列が無い"))
        .points
        .iter()
        .filter_map(|p| p.value)
        .collect()
}

// ===========================================================================
// 1. A_DISK の前サンプル (指摘 9)
// ===========================================================================

/// ディスクが消える・増える 3 レコード。
///
/// | レコード | 位置 0 | 位置 1 | 位置 2 |
/// |---|---|---|---|
/// | 0 (12:26:40) | dev8-0 | dev8-16 | dev65-16 |
/// | 1 (12:26:41) | dev8-0 | dev65-16 | |
/// | 2 (12:26:42) | dev8-0 | dev8-32 (新規) | dev65-16 |
///
/// 位置で突き合わせると、レコード 1 の dev65-16 は dev8-16 と、
/// レコード 2 の dev65-16 は「前に居ない」と判定されて値が壊れる。
fn disk_file() -> SaFile {
    file(
        a_disk(3),
        vec![
            vec![
                disk(8, 0, 1_000, 10_000, 20_000, 1_000),
                disk(8, 16, 50_000, 900_000, 800_000, 70_000),
                disk(65, 16, 2_000, 4_000, 6_000, 3_000),
            ],
            vec![
                disk(8, 0, 1_010, 10_200, 20_400, 1_050),
                disk(65, 16, 2_030, 4_600, 6_900, 3_400),
            ],
            vec![
                disk(8, 0, 1_020, 10_400, 20_800, 1_100),
                disk(8, 32, 700, 1_400, 2_800, 500),
                disk(65, 16, 2_080, 5_600, 7_900, 3_700),
            ],
        ],
    )
}

/// **回帰テスト (指摘 9)**: `A_DISK` の値は `major` / `minor` が同じディスクとの差分。
///
/// 期待値 (区間 1 秒、セクタは 512 B なので kB/s = Δセクタ / 2):
///
/// | 時刻 | デバイス | tps | rkB/s | wkB/s | areq-sz | %util |
/// |---|---|---|---|---|---|---|
/// | 12:26:41 | dev65-16 | 30 | 300 | 450 | (600+900)/30/2 = 25 | 400 ms/1 s = 40 |
/// | 12:26:42 | dev65-16 | 50 | 500 | 500 | (1000+1000)/50/2 = 20 | 300 ms/1 s = 30 |
/// | 12:26:42 | dev8-32 (新規) | 互換は全ゼロからの差分 700、独自出力・集計は差分を作らない |||||
#[test]
fn disk_values_follow_major_minor_in_every_output() {
    let file = disk_file();
    let id = ActivityId::DISK;

    // --- 互換出力: sar -d / sadf -d / sadf -j ---
    let sar = sar_text(&file, id);
    let row = |time: &str, dev: &str| -> Vec<String> {
        let line = sar
            .lines()
            .find(|l| l.starts_with(time) && l.split_whitespace().any(|t| t == dev))
            .unwrap_or_else(|| panic!("{time} {dev} の行が無い:\n{sar}"));
        let tokens: Vec<&str> = line.split_whitespace().collect();
        let at = tokens.iter().position(|t| *t == dev).unwrap();
        tokens[at + 1..].iter().map(|s| s.to_string()).collect()
    };
    // tps rkB/s wkB/s dkB/s areq-sz aqu-sz await %util
    assert_eq!(
        row("12:26:41", "dev65-16"),
        [
            "30.00", "300.00", "450.00", "0.00", "25.00", "0.00", "0.00", "40.00"
        ]
    );
    assert_eq!(
        row("12:26:42", "dev65-16"),
        [
            "50.00", "500.00", "500.00", "0.00", "20.00", "0.00", "0.00", "30.00"
        ]
    );
    assert_eq!(
        row("12:26:42", "dev8-32"),
        [
            "700.00", "700.00", "1400.00", "0.00", "3.00", "0.00", "0.00", "50.00"
        ]
    );

    let cfg = sadf_config(id);
    let db = sadf_out(&file, &cfg, sadf::dbppc::write_db);
    for expected in [
        ";2020-09-13 12:26:41 UTC;dev8-0;10.00;100.00;200.00;0.00;30.00;0.00;0.00;5.00",
        ";2020-09-13 12:26:41 UTC;dev65-16;30.00;300.00;450.00;0.00;25.00;0.00;0.00;40.00",
        ";2020-09-13 12:26:42 UTC;dev8-32;700.00;700.00;1400.00;0.00;3.00;0.00;0.00;50.00",
        ";2020-09-13 12:26:42 UTC;dev65-16;50.00;500.00;500.00;0.00;20.00;0.00;0.00;30.00",
    ] {
        assert!(db.contains(expected), "{expected} が無い:\n{db}");
    }

    let json: Value = serde_json::from_str(&sadf_out(&file, &cfg, sadf::json::write_json)).unwrap();
    let stats = json["sysstat"]["hosts"][0]["statistics"]
        .as_array()
        .unwrap();
    let tps_of = |n: usize, dev: &str| {
        stats[n]["disk"]
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["disk-device"] == dev)
            .unwrap_or_else(|| panic!("{dev} が無い"))["tps"]
            .as_f64()
            .unwrap()
    };
    assert_eq!(tps_of(0, "dev65-16"), 30.0);
    assert_eq!(tps_of(1, "dev65-16"), 50.0);
    assert_eq!(tps_of(1, "dev8-32"), 700.0);

    // --- 独自出力: show の json / csv / table / ndjson ---
    let cfg = show_config(id);
    let values = show_json_values(&file, &cfg, "rates");
    let at = |sec: u64, dev: &str, col: &str| values[&(START + sec, dev.to_string())][col].clone();
    let ok = |v: f64| (Some(v), "ok".to_string());
    assert_eq!(at(1, "dev65-16", "tps"), ok(30.0));
    assert_eq!(at(1, "dev65-16", "read_kb_per_sec"), ok(300.0));
    assert_eq!(at(1, "dev65-16", "write_kb_per_sec"), ok(450.0));
    assert_eq!(at(1, "dev65-16", "util_pct"), ok(40.0));
    assert_eq!(at(2, "dev65-16", "tps"), ok(50.0));
    assert_eq!(at(2, "dev65-16", "util_pct"), ok(30.0));
    assert_eq!(at(1, "dev8-0", "tps"), ok(10.0));
    assert_eq!(at(2, "dev8-0", "tps"), ok(10.0));
    // 新しく現れたディスクは全ゼロからの差分を値にしない
    assert_eq!(at(2, "dev8-32", "tps"), (None, "item_replaced".to_string()));

    let csv = show_csv(&file, &cfg);
    let tps_line = |sec: u64, dev: &str| {
        let end = (START + sec).to_string();
        csv.lines()
            .find(|l| {
                let f: Vec<&str> = l.split(',').collect();
                f.get(4) == Some(&end.as_str())
                    && f.get(8) == Some(&dev)
                    && f.get(10) == Some(&"rates")
                    && f.get(11) == Some(&"tps")
            })
            .unwrap_or_else(|| panic!("{dev} の tps 行が無い:\n{csv}"))
            .to_string()
    };
    assert!(tps_line(1, "dev65-16").contains(",30.0000,"), "{csv}");
    assert!(tps_line(2, "dev65-16").contains(",50.0000,"), "{csv}");
    assert!(tps_line(2, "dev8-32").ends_with(",item_replaced"), "{csv}");

    let table = show(&file, &cfg, re_sar_ch::output::table::write_table);
    let header: Vec<&str> = table
        .lines()
        .find(|l| l.trim_start().starts_with("time"))
        .unwrap()
        .split_whitespace()
        .collect();
    let tps_col = header.iter().position(|h| *h == "tps").unwrap();
    let cell = |time: &str, dev: &str| {
        let line = table
            .lines()
            .find(|l| {
                let t: Vec<&str> = l.split_whitespace().collect();
                t.first() == Some(&time) && t.get(1) == Some(&dev)
            })
            .unwrap_or_else(|| panic!("{time} {dev} の行が無い:\n{table}"));
        line.split_whitespace().nth(tps_col).unwrap().to_string()
    };
    assert_eq!(cell("12:26:41", "dev65-16"), "30.00");
    assert_eq!(cell("12:26:42", "dev65-16"), "50.00");
    assert_eq!(cell("12:26:42", "dev8-32"), "-");

    let ndjson = show(&file, &cfg, re_sar_ch::output::ndjson::write_ndjson);
    let row: Value = ndjson
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .find(|r| r["item"] == "dev65-16" && r["end_epoch"] == START + 1)
        .expect("dev65-16 の行");
    let tps = row["rates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == "tps")
        .unwrap();
    assert_eq!(tps["value"], 30.0);

    // --- 集計: summarize (detect も同じ区間値を読む) ---
    let s = summarize(&file, id);
    assert_eq!(observed(&s, id, "dev65-16", "tps"), vec![30.0, 50.0]);
    assert_eq!(observed(&s, id, "dev65-16", "util_pct"), vec![40.0, 30.0]);
    assert_eq!(observed(&s, id, "dev8-0", "tps"), vec![10.0, 10.0]);
    assert!(observed(&s, id, "dev8-32", "tps").is_empty());
    let c = s.column(id, "dev65-16", "tps").unwrap();
    assert_eq!(c.intervals, 2);
    assert_eq!(c.mean, Some(40.0));
}

/// 名前を持つ世代の `A_IRQ` (id 3、magic `0x8c`、12 バイト、`types_nr = (0,0,1)`)。
///
/// `nr` 行 (CPU "all" + CPU) × `nr2` 列 (割り込み) の行列 (02 §6.1)。
fn a_irq_named(nr: i32, nr2: i32) -> ActivitySpec {
    ActivitySpec {
        id: 3,
        magic: 0x8c,
        nr,
        nr2,
        has_nr: true,
        size: 12,
        types_nr: [0, 0, 1],
    }
}

/// `stats_irq` (magic `0x8c`) 1 要素。`irq_nr` 0 / `irq_name[8]` 4。
fn irq_named(count: u32, name: &str) -> Vec<u8> {
    let mut out = vec![0; 12];
    out[..4].copy_from_slice(&count.to_le_bytes());
    out[4..4 + name.len()].copy_from_slice(name.as_bytes());
    out
}

/// 集計も `A_IRQ` の前サンプルを位置で引き、`show` (と本家 `sar` / `sadf`) と
/// 同じ区間値を出す。
///
/// 以前の集計は「前サンプルに同じ名前があるか」を名前で調べ、値は位置の前値から
/// 計算していた。列の名前が変わった区間では `show` が値を出すのに、集計だけが
/// 「入れ替わった」として除外していた。
#[test]
fn irq_columns_are_paired_by_position_in_every_output() {
    let row = |counts: [u32; 3], names: [&str; 3]| -> Vec<Vec<u8>> {
        counts
            .iter()
            .zip(names)
            .map(|(c, n)| irq_named(*c, n))
            .collect()
    };
    // 行 0 (CPU "all") だけが名前を持つ。2 列目の名前が eth0 → eth1 に変わる
    let file = file(
        a_irq_named(2, 3),
        vec![
            [
                row([100, 40, 60], ["sum", "timer", "eth0"]),
                row([100, 40, 60], ["", "", ""]),
            ]
            .concat(),
            [
                row([150, 70, 80], ["sum", "timer", "eth1"]),
                row([150, 70, 80], ["", "", ""]),
            ]
            .concat(),
        ],
    );
    let id = ActivityId::IRQ;

    let values = show_json_values(&file, &show_config(id), "rates");
    let s = summarize(&file, id);
    for (label, v) in [("sum", 50.0), ("timer", 30.0), ("eth1", 20.0)] {
        assert_eq!(
            values[&(START + 1, label.to_string())]["intr"],
            (Some(v), "ok".to_string()),
            "{label}"
        );
        assert_eq!(observed(&s, id, label, "intr"), vec![v], "{label}");
    }
}

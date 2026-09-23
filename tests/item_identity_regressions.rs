//! item の同一性 (前サンプルとの対応付け・item のラベル) の結合テスト。
//!
//! | # | テスト | 何を固定するか |
//! |---|---|---|
//! | 1 | [`disk_values_follow_major_minor_in_every_output`] | `A_DISK` の前サンプルを位置ではなく `major` / `minor` で引くこと。ディスクが消えても増えても、互換出力 (`sar` / `sadf -d` / `sadf -j`)・独自出力 (`show` の json / csv / table / ndjson)・集計 (`summarize`) が同じ区間値を出す (指摘 9) |
//! | 1' | [`irq_columns_are_paired_by_position_in_every_output`] | 集計も `A_IRQ` の前サンプルを `show` と同じ規則 (位置) で引くこと |
//! | 2 | [`old_generation_irq_rows_are_sum_and_interrupt_numbers`] | 名前を持たない旧世代 (`0x2171`) の `A_IRQ` を、全出力が `sum` / 割り込み番号で表し、「合計が減ったら 0」を総和以外の割り込みにも効かせること (指摘 10) |
//! | 3 | [`converted_irq_names_match_the_labels_of_direct_reading`] | 旧形式を変換したファイルの `irq_name` と、変換前のファイルを直接読んだときのラベル・値が一致すること |
//! | 4 | [`unnamed_irq_of_the_self_describing_generation_is_labelled_the_same`] | 名前を持たない自己記述世代 (`A_IRQ` magic `0x8b`) も独自出力と集計が同じラベルで表すこと |
//!
//! fixture はすべて自作。統計のバイト列は `docs/format/02-activities.md` §5 / §6 の
//! オフセット表から独立に書き起こしたもので、本体のレイアウト定義は使わない。
//! 期待値は式から手で書いた。

mod fixtures;

use std::collections::BTreeMap;

use fixtures::{ActivitySpec, FixtureAbi, FixtureSpec, Generation, RecordSpec, build};
use re_sar_ch::analyze::summary::{NativePeriodSummary, RetainTimelines};
use re_sar_ch::analyze::timeline::MetricKey;
use re_sar_ch::convert::{self, ConvertOptions};
use re_sar_ch::format::file::SaFile;
use re_sar_ch::model::{ActivityId, DisplayTz};
use re_sar_ch::multi::{MultiOptions, analyze_files};
use re_sar_ch::output::json::{CustomConfig, ValueScope};
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

/// 名前を持たない旧世代の `A_IRQ` (id 3、magic `0x8b`、v11.7.2〜v12.5.5)。
///
/// 1 次元で `nr` = 割り込み数 + 1 (添字 0 が総和)、`nr2` = 1。
/// `stats_irq` は `unsigned long long irq_nr` の 1 フィールド (8 バイト、02 §6.4)。
fn a_irq_unnamed(nr: i32) -> ActivitySpec {
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

fn irq(count: u64) -> Vec<u8> {
    count.to_le_bytes().to_vec()
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

// ===========================================================================
// 2. 名前を持たない旧世代の A_IRQ (指摘 10)
// ===========================================================================

/// `A_IRQ` の件数。添字 0 が総和、添字 n が割り込み n - 1。
///
/// | 時刻 | sum | 0 | 1 | 2 |
/// |---|---|---|---|---|
/// | 12:26:40 | 60 | 10 | 20 | 30 |
/// | 12:26:41 | 150 (+90) | 15 (+5) | 20 (+0) | 115 (+85) |
/// | 12:26:42 | 140 (減) | 25 (+10) | 20 (+0) | 95 (減) |
///
/// 減った区間 (CPU のオフラインなど) は 0 になる。本家は旧形式を CPU "all" 行
/// 1 本に変換し、その列の**全割り込み**をクランプする (`print_irq_stats()` の `!c`)。
const IRQ_COUNTS: [[u64; 4]; 3] = [[60, 10, 20, 30], [150, 15, 20, 115], [140, 25, 20, 95]];

/// 区間ごとの期待値 (1 秒間隔なので件数の差がそのまま毎秒の値になる)。
const IRQ_EXPECTED: [(&str, [f64; 2]); 4] = [
    ("sum", [90.0, 0.0]),
    ("0", [5.0, 10.0]),
    ("1", [0.0, 0.0]),
    ("2", [85.0, 0.0]),
];

/// 旧形式 `0x2171` (sysstat 9.1.6〜10.2.1) のファイル。`A_IRQ` の件数だけ手で書く。
///
/// `A_IRQ` は時代 A (magic `0x8a`): 1 次元で、`stats_irq` は
/// `unsigned long long irq_nr` 1 本が `aligned(16)` で 16 バイト (02 §6.4)。
/// 旧世代の統計はレコードごとの件数を持たず、record_header の直後に
/// activity の並び順 (`A_CPU` → `A_IRQ`) で `nr` 個ずつ並ぶ。
/// `A_CPU` の値は fixture 生成器の決定的な値のまま。
fn old_irq_file(counts: &[[u64; 4]]) -> SaFile {
    let a_cpu = ActivitySpec {
        id: 1,
        magic: 0x8a,
        nr: 3,
        nr2: 1,
        has_nr: false,
        size: 160,
        types_nr: [10, 0, 0],
    };
    let a_irq = ActivitySpec {
        id: 3,
        magic: 0x8a,
        nr: 4,
        nr2: 1,
        has_nr: false,
        size: 16,
        types_nr: [1, 0, 0],
    };
    let mut spec = FixtureSpec::skeleton(Generation::G2171, FixtureAbi::Le64);
    // `0x2171` を書いた版 (9.1.6〜10.2.1) にしておく
    spec.version = (9, 1, 6, 0);
    let cpu_nr = spec.cpu_nr as i32;
    spec.activities = vec![a_cpu.clone(), a_irq.clone()];
    // RESTART の `uptime0` は実ファイルでも 0
    let mut restart = RecordSpec::restart(cpu_nr, START - 1, 12, 26, 39);
    restart.uptime = 0;
    spec.records = std::iter::once(restart)
        .chain((0..counts.len()).map(|i| {
            let mut rec = RecordSpec::stats(vec![0, 0], START + i as u64, 12, 26, 40 + i as u8);
            // 100 jiffies = 1 秒 (旧世代は USER_HZ = 100 として読む)
            rec.uptime = 100_000 + i as u64 * 100;
            rec
        }))
        .collect();
    let mut built = build(spec);
    let header = built.record_header_size();
    let cpu_bytes = a_cpu.nr as usize * a_cpu.size as usize;
    // 先頭の RESTART を除いた統計レコード
    let stats: Vec<usize> = built
        .record_offsets
        .iter()
        .skip(1)
        .map(|(off, _)| *off)
        .collect();
    for (off, row) in stats.into_iter().zip(counts) {
        for (j, count) in row.iter().enumerate() {
            let at = off + header + cpu_bytes + j * a_irq.size as usize;
            built.bytes[at..at + 8].copy_from_slice(&count.to_le_bytes());
        }
    }
    SaFile::from_bytes("old", built.bytes).unwrap()
}

/// **回帰テスト (指摘 10)**: 名前を持たない旧世代の `A_IRQ` は、どの出力でも
/// 総和の行が `sum`、割り込み n が `n` になり、同じ区間値を出す。
///
/// 以前は `show` / `summarize` / `detect` が添字をそのままラベルにしており、
/// 総和の行が `0`、割り込み 0 が `1` と 1 つずれていた (`sar -I` と `sadf` は
/// `sum` / `0`)。集約の判定もラベルの文字列 (`sum`) で行っていたため、
/// `summarize` は総和の減少をクランプせず除外し、`sadf` は総和以外の割り込みの
/// 減少を符号なし減算の巨大値 (`18446744073709551616.00`) で出していた。
/// 本家 12.8 で変換後のファイルを読んだ `sadf -d` は `0.00` を出す。
#[test]
fn old_generation_irq_rows_are_sum_and_interrupt_numbers() {
    let file = old_irq_file(&IRQ_COUNTS);
    let id = ActivityId::IRQ;
    let expected = IRQ_EXPECTED;

    // --- sar -I ALL (基準: 本家と同じ `sum` / 割り込み番号) ---
    let sar = sar_text(&file, id);
    for (label, values) in expected {
        for (sec, v) in values.iter().enumerate() {
            let time = format!("12:26:4{}", sec + 1);
            let line = sar
                .lines()
                .find(|l| {
                    let t: Vec<&str> = l.split_whitespace().collect();
                    t.first() == Some(&time.as_str()) && t.get(1) == Some(&label)
                })
                .unwrap_or_else(|| panic!("{time} {label} の行が無い:\n{sar}"));
            assert!(line.ends_with(&format!("{v:.2}")), "{line}");
        }
    }

    // --- sadf -d / -j ---
    let cfg = sadf_config(id);
    let db = sadf_out(&file, &cfg, sadf::dbppc::write_db);
    assert!(
        db.contains(";2020-09-13 12:26:41 UTC;sum;90.00"),
        "総和の行は sum:\n{db}"
    );
    assert!(db.contains(";2020-09-13 12:26:41 UTC;2;85.00"), "{db}");
    // 総和以外の割り込みも減ったら 0 (以前は符号なし減算の巨大値が出ていた)
    assert!(db.contains(";2020-09-13 12:26:42 UTC;2;0.00"), "{db}");
    assert!(!db.contains(";3;"), "割り込み番号は 0 から 2 まで:\n{db}");
    let json: Value = serde_json::from_str(&sadf_out(&file, &cfg, sadf::json::write_json)).unwrap();
    let first = &json["sysstat"]["hosts"][0]["statistics"][0]["interrupts"];
    let names: Vec<&str> = first
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["intr"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["sum", "0", "1", "2"], "{json:#}");

    // --- show の json / csv / table / ndjson ---
    let cfg = show_config(id);
    let values = show_json_values(&file, &cfg, "rates");
    for (label, v) in expected {
        for (sec, v) in v.iter().enumerate() {
            let key = (START + sec as u64 + 1, label.to_string());
            let got = values
                .get(&key)
                .unwrap_or_else(|| panic!("{key:?} が無い: {:?}", values.keys()));
            assert_eq!(got["intr"], (Some(*v), "ok".to_string()), "{key:?}");
        }
    }
    assert!(
        !values.keys().any(|(_, label)| label == "3"),
        "総和の行が 0 番にずれ、割り込み 2 が 3 番にずれていない"
    );

    let csv = show_csv(&file, &cfg);
    let labels: std::collections::BTreeSet<&str> = csv
        .lines()
        .skip(1)
        .map(|l| l.split(',').nth(8).unwrap())
        .collect();
    assert_eq!(labels, ["0", "1", "2", "sum"].into_iter().collect());

    let table = show(&file, &cfg, re_sar_ch::output::table::write_table);
    assert!(table.contains("sum [CPU all]"), "{table}");
    assert!(!table.contains(" 3 [CPU all]"), "{table}");

    let ndjson = show(&file, &cfg, re_sar_ch::output::ndjson::write_ndjson);
    let items: std::collections::BTreeSet<String> = ndjson
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .filter(|r| r["record"] == "sample")
        .map(|r| r["item"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        items,
        ["0", "1", "2", "sum"]
            .into_iter()
            .map(String::from)
            .collect()
    );

    // --- 集計: summarize (detect も同じラベルと区間値を読む) ---
    let s = summarize(&file, id);
    let mut labels: Vec<&str> = s
        .activity(id)
        .unwrap()
        .items
        .iter()
        .map(|i| i.item.as_str())
        .collect();
    labels.sort_unstable();
    assert_eq!(labels, ["0", "1", "2", "sum"]);
    for (label, v) in expected {
        assert_eq!(observed(&s, id, label, "intr"), v.to_vec(), "{label}");
    }
}

// ===========================================================================
// 3. 変換前後で割り込みの名前が一致する
// ===========================================================================

/// 変換 (`sadf -c` 相当) が書く `irq_name` と、変換前のファイルを直接読んだときの
/// `show` / `summarize` のラベルが一致すること。値 (生値・区間値) も同じ item に付くこと。
///
/// どちらも `compute::irq_item_name` の規則 (添字 0 が `sum`、添字 n が n - 1) で
/// 名前が付く。直接読んだときだけ添字のままだと、同じ割り込みが変換の前後で
/// 別の名前になる。
#[test]
fn converted_irq_names_match_the_labels_of_direct_reading() {
    let direct = old_irq_file(&IRQ_COUNTS);
    let mut bytes = Vec::new();
    convert::convert(&direct, &ConvertOptions { hz: Some(100) }, &mut bytes).unwrap();
    let converted = SaFile::from_bytes("converted", bytes).unwrap();

    let cfg = CustomConfig {
        values: ValueScope::Both,
        ..show_config(ActivityId::IRQ)
    };
    // (区間の終点, ラベル, 生値, 区間値)
    let rows = |file: &SaFile| -> Vec<(u64, String, Option<f64>, Option<f64>)> {
        let raw = show_json_values(file, &cfg, "raw");
        let rates = show_json_values(file, &cfg, "rates");
        rates
            .iter()
            .filter(|((end, _), _)| *end > START)
            .map(|((end, label), fields)| {
                (
                    *end,
                    label.clone(),
                    raw[&(*end, label.clone())]["intr"].0,
                    fields["intr"].0,
                )
            })
            .collect()
    };
    let direct_rows = rows(&direct);
    assert_eq!(direct_rows, rows(&converted));
    for (label, values) in IRQ_EXPECTED {
        for (sec, v) in values.iter().enumerate() {
            let end = START + sec as u64 + 1;
            assert!(
                direct_rows
                    .iter()
                    .any(|(e, l, _, rate)| *e == end && l == label && *rate == Some(*v)),
                "{end} {label} = {v}: {direct_rows:?}"
            );
        }
    }

    let labels = |s: &NativePeriodSummary| -> Vec<String> {
        let mut v: Vec<String> = s
            .activity(ActivityId::IRQ)
            .unwrap()
            .items
            .iter()
            .map(|i| i.item.clone())
            .collect();
        v.sort_unstable();
        v
    };
    assert_eq!(
        labels(&summarize(&direct, ActivityId::IRQ)),
        labels(&summarize(&converted, ActivityId::IRQ))
    );
}

/// 名前を持たない自己記述世代 (`0x2175`、`A_IRQ` magic `0x8b`、v11.7.2〜v12.5.5) も
/// 独自出力と集計は `sum` / 割り込み番号で表す。
///
/// 本家 12.8 はこの世代の `A_IRQ` を読み飛ばすので、互換出力 (`sar` / `sadf`) には
/// 現れない (reSARch も合わせている)。独自出力と集計だけが読む。
#[test]
fn unnamed_irq_of_the_self_describing_generation_is_labelled_the_same() {
    let file = file(
        a_irq_unnamed(4),
        IRQ_COUNTS
            .iter()
            .map(|row| row.iter().map(|c| irq(*c)).collect())
            .collect(),
    );
    let id = ActivityId::IRQ;

    let values = show_json_values(&file, &show_config(id), "rates");
    for (label, v) in IRQ_EXPECTED {
        for (sec, v) in v.iter().enumerate() {
            let key = (START + sec as u64 + 1, label.to_string());
            let got = values
                .get(&key)
                .unwrap_or_else(|| panic!("{key:?} が無い: {:?}", values.keys()));
            assert_eq!(got["intr"], (Some(*v), "ok".to_string()), "{key:?}");
        }
    }

    let s = summarize(&file, id);
    for (label, v) in IRQ_EXPECTED {
        assert_eq!(observed(&s, id, label, "intr"), v.to_vec(), "{label}");
    }
}

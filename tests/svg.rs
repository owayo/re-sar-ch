mod fixtures;

use fixtures::{ActivitySpec, FixtureAbi, FixtureSpec, Generation, RecordSpec, build};
use re_sar_ch::cli::{SadfOutputOptions, SvgPalette};
use re_sar_ch::format::file::SaFile;
use re_sar_ch::model::ActivityId;
use re_sar_ch::output::sadf::{RecordSelect, SadfConfig, SadfExtra, TimeBase};
use re_sar_ch::output::sar_text::CpuSelection;
use re_sar_ch::output::svg::{write_svg, write_svg_with};
use re_sar_ch::output::time_filter::{TimeBound, TimeFilter};

fn sensor_file(counts: &[i32]) -> SaFile {
    let mut spec = FixtureSpec::skeleton(Generation::G2175Current, FixtureAbi::Le64);
    spec.nodename = "chart<&\"host".into();
    // docs/format/02-activities.md: A_PWR_FAN は double 2 個の後に device[20] が続く 40 バイト。
    spec.activities = vec![ActivitySpec {
        id: 31,
        magic: 0x8a,
        nr: 1,
        nr2: 1,
        has_nr: true,
        size: 40,
        types_nr: [2, 0, 0],
    }];
    spec.records = counts
        .iter()
        .enumerate()
        .map(|(i, &count)| {
            let mut record = RecordSpec::stats(
                vec![count],
                1_600_000_010 + i as u64 * 10,
                12,
                27,
                i as u8 * 10,
            );
            record.uptime = 100_000 + i as u64 * 1000;
            record
        })
        .collect();
    let mut fixture = build(spec);
    for (i, (offset, _)) in fixture.record_offsets.iter().enumerate() {
        if counts[i] == 0 {
            continue;
        }
        let at = offset + 24 + 4;
        fixture.bytes[at..at + 8].copy_from_slice(&(3000.0 + i as f64 * 100.0).to_le_bytes());
        fixture.bytes[at + 8..at + 16].copy_from_slice(&1000f64.to_le_bytes());
        fixture.bytes[at + 16..at + 36].fill(0);
        fixture.bytes[at + 16..at + 19].copy_from_slice(b"fan");
    }
    SaFile::from_bytes("synthetic-sensor", fixture.bytes).unwrap()
}

fn render(file: &SaFile, cfg: &SadfConfig, options: &SadfOutputOptions) -> String {
    let mut out = Vec::new();
    write_svg(&mut out, file, cfg, options).unwrap();
    String::from_utf8(out).unwrap()
}

#[test]
fn sensor_values_are_decoded_and_xml_text_is_escaped() {
    let svg = render(
        &sensor_file(&[1, 1, 1]),
        &SadfConfig::default(),
        &SadfOutputOptions::default(),
    );
    assert!(svg.starts_with("<?xml"));
    assert!(svg.ends_with("</svg>\n"));
    assert!(svg.contains("chart&lt;&amp;&quot;host"));
    assert!(!svg.contains("chart<&"));
    assert!(svg.contains("A_PWR_FAN"));
    assert!(svg.contains("data-value=\"3100\""));
    assert!(svg.contains("data-value=\"3200\""));
    assert!(svg.contains("class=\"series\""));
}

#[test]
fn absent_items_do_not_connect_lines_across_a_gap() {
    let svg = render(
        &sensor_file(&[1, 1, 0, 1]),
        &SadfConfig::default(),
        &SadfOutputOptions::default(),
    );
    assert!(svg.contains("data-value=\"3100\""));
    assert!(svg.contains("data-value=\"3300\""));
    assert!(!svg.contains("class=\"series\""));
}

#[test]
fn time_and_activity_selection_apply_to_graphs() {
    let file = sensor_file(&[1, 1, 1]);
    let cfg = SadfConfig {
        activities: Some(vec![ActivityId::CPU]),
        ..Default::default()
    };
    assert!(
        render(&file, &cfg, &SadfOutputOptions::default()).contains("No selected numeric samples")
    );
    let cfg = SadfConfig {
        time_filter: TimeFilter {
            start: TimeBound::Epoch(1_600_000_020),
            ..Default::default()
        },
        ..Default::default()
    };
    let svg = render(&file, &cfg, &SadfOutputOptions::default());
    assert!(!svg.contains("data-value=\"3100\""));
    assert!(svg.contains("data-value=\"3200\""));
}

/// positional の件数・間隔は、軸の走査とデータ点の走査の両方に適用する。
#[test]
fn positional_interval_and_count_select_svg_samples() {
    let file = sensor_file(&[1, 1, 1, 1]);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("synthetic.sa");
    std::fs::write(&path, file.bytes()).unwrap();
    let render_selected = |interval: &str, count: &str| {
        let output = assert_cmd::Command::new(assert_cmd::cargo::cargo_bin!("resarch"))
            .args(["sadf", "-g"])
            .arg(&path)
            .args([interval, count, "--", "-m", "FAN"])
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        String::from_utf8(output).unwrap()
    };
    let one = render_selected("1", "1");
    assert!(one.contains("data-time=\"1600000020\""));
    assert!(!one.contains("data-time=\"1600000030\""));
    assert!(!one.contains("data-time=\"1600000040\""));
    assert!(!one.contains("12:27:30"), "軸にも選択後の範囲を使う");

    let spaced = render_selected("20", "0");
    assert!(!spaced.contains("data-time=\"1600000020\""));
    assert!(spaced.contains("data-time=\"1600000030\""));
    assert!(!spaced.contains("data-time=\"1600000040\""));
}

/// 間引いたレコードを差分の基準にせず、前に表示したレコードからレートを求める。
#[test]
fn positional_interval_keeps_the_last_selected_counter_baseline() {
    let mut spec = FixtureSpec::skeleton(Generation::G2175Current, FixtureAbi::Le64);
    spec.activities = vec![ActivitySpec::a_pcsw()];
    spec.records = (0..5)
        .map(|i| {
            let mut rec = RecordSpec::stats(vec![1], 1_600_000_010 + i * 10, 12, 0, i as u8 * 10);
            rec.uptime = 100_000 + i * 1000;
            rec
        })
        .collect();
    let mut fixture = build(spec);
    for ((offset, _), counter) in fixture
        .record_offsets
        .iter()
        .zip([0u64, 100, 1000, 1100, 2000])
    {
        for field in [0, 8] {
            let at = offset + 24 + field;
            fixture.bytes[at..at + 8].copy_from_slice(&counter.to_le_bytes());
        }
    }
    let file = SaFile::from_bytes("synthetic", fixture.bytes).unwrap();
    let mut out = Vec::new();
    write_svg_with(
        &mut out,
        &file,
        &SadfConfig::default(),
        &SadfOutputOptions::default(),
        &SadfExtra {
            select: RecordSelect {
                interval: 20,
                count: None,
            },
            ..Default::default()
        },
    )
    .unwrap();
    let svg = String::from_utf8(out).unwrap();
    assert!(svg.contains("data-time=\"1600000030\" data-value=\"50\""));
    assert!(!svg.contains("data-value=\"90\""));
    assert!(svg.contains("data-time=\"1600000050\" data-value=\"50\""));
    assert!(
        svg.contains("class=\"series\""),
        "選択した点同士は線でつなぐ"
    );
}

/// oneday は軸を最低24時間に広げる指定であり、以降の観測を切り捨てない。
#[test]
fn one_day_axis_preserves_samples_after_the_first_24_hours() {
    let mut spec = FixtureSpec::skeleton(Generation::G2175Current, FixtureAbi::Le64);
    spec.activities = vec![ActivitySpec::a_pcsw()];
    spec.records = [1_600_000_000, 1_600_000_010, 1_600_090_000]
        .into_iter()
        .enumerate()
        .map(|(i, ust)| {
            let mut rec = RecordSpec::stats(vec![1], ust, 12, 0, 0);
            rec.uptime = 100_000 + i as u64 * 1000;
            rec
        })
        .collect();
    let file = SaFile::from_bytes("synthetic", build(spec).bytes).unwrap();
    let svg = render(
        &file,
        &SadfConfig::default(),
        &SadfOutputOptions {
            one_day: true,
            ..Default::default()
        },
    );
    assert!(svg.contains("data-time=\"1600000010\""));
    assert!(svg.contains("data-time=\"1600090000\""));
    assert!(svg.contains("Day axis:"));
    assert!(
        !svg.contains("24:00"),
        "延ばした軸の終点には実際の時刻を出す"
    );
}

#[test]
fn svg_options_have_visible_effects_or_return_an_error_before_output() {
    let file = sensor_file(&[1, 1]);
    let options = SadfOutputOptions {
        show_toc: true,
        show_info: true,
        canvas_height: Some(600),
        palette: SvgPalette::Bw,
        debug: true,
        one_day: true,
        ..Default::default()
    };
    let svg = render(&file, &SadfConfig::default(), &options);
    assert!(svg.contains("height=\"600\""));
    assert!(svg.contains("href=\"#chart-0\""));
    assert!(svg.contains("fill=\"#111827\""));
    assert!(svg.contains("24-hour axis"));
    assert!(svg.contains("<!-- reSARch SVG"));
    for options in [
        SadfOutputOptions {
            autoscale: true,
            ..Default::default()
        },
        SadfOutputOptions {
            packed: true,
            ..Default::default()
        },
        SadfOutputOptions {
            palette: SvgPalette::Custom,
            ..Default::default()
        },
    ] {
        let mut bytes = Vec::new();
        assert!(write_svg(&mut bytes, &file, &SadfConfig::default(), &options).is_err());
        assert!(bytes.is_empty());
    }
}

/// `-O oneday` の軸は、他のラベルと同じ時刻基準で切る。
///
/// ここだけ UTC 固定だと、`-T` を付けたとき軸の 00:00 とデータ点の時刻が
/// 別の基準になり、図の中で辻褄が合わなくなる。
#[test]
fn the_one_day_axis_follows_the_time_base() {
    let file = sensor_file(&[1, 1]);
    let options = SadfOutputOptions {
        one_day: true,
        ..Default::default()
    };

    // 既定 (UTC) では UTC で切る。
    let utc = render(&file, &SadfConfig::default(), &options);
    assert!(utc.contains("24-hour axis: 00:00–24:00 UTC"), "{utc}");
    assert!(utc.contains("00:00 UTC"), "{utc}");

    // `-T` (読み手のローカル) では、軸の基準名も UTC ではなくなる。
    let local = render(
        &file,
        &SadfConfig {
            time_base: TimeBase::LocalTime,
            ..Default::default()
        },
        &options,
    );
    assert!(local.contains("24-hour axis:"), "{local}");
    assert!(
        !local.contains("24-hour axis: 00:00–24:00 UTC"),
        "ローカル指定なのに UTC のままになっている: {local}"
    );

    // `-t` (採取側の記録時刻) でも同じ。
    let recorded = render(
        &file,
        &SadfConfig {
            time_base: TimeBase::TrueTime,
            ..Default::default()
        },
        &options,
    );
    assert!(recorded.contains("24-hour axis:"), "{recorded}");
}

#[test]
fn restart_and_cpu_selection_are_respected() {
    let mut spec = FixtureSpec::minimal(Generation::G2175Current, FixtureAbi::Le64);
    let mut records = Vec::new();
    for i in 0..7u64 {
        let mut record = if i == 3 {
            RecordSpec::restart(3, 1_600_000_010 + i * 10, 12, 28, 0)
        } else {
            RecordSpec::stats(vec![3, 0], 1_600_000_010 + i * 10, 12, 28, i as u8 * 8)
        };
        record.uptime = 100_000 + i * 1000;
        records.push(record);
    }
    spec.records = records;
    let file = SaFile::from_bytes("synthetic", build(spec).bytes).unwrap();
    let cfg = SadfConfig {
        activities: Some(vec![ActivityId::CPU]),
        cpus: CpuSelection::Aggregate,
        ..Default::default()
    };
    let svg = render(
        &file,
        &cfg,
        &SadfOutputOptions {
            show_idle: true,
            ..Default::default()
        },
    );
    assert!(svg.contains("A_CPU / all /"));
    assert!(!svg.contains("A_CPU / 0 /"));
    // どの系列も再起動の前後に 2 点ずつあり、線分はちょうど 2 本になる。
    assert_eq!(
        svg.matches("class=\"point\"").count(),
        svg.matches("class=\"series\"").count() * 2
    );
    assert!(svg.contains("%idle"));
    let svg_without_idle = render(&file, &cfg, &SadfOutputOptions::default());
    assert!(!svg_without_idle.contains("%idle"));

    let mut out = Vec::new();
    write_svg_with(
        &mut out,
        &file,
        &cfg,
        &SadfOutputOptions::default(),
        &SadfExtra {
            select: RecordSelect {
                interval: 1,
                count: Some(1),
            },
            ..Default::default()
        },
    )
    .unwrap();
    let selected = String::from_utf8(out).unwrap();
    assert!(selected.contains("data-time=\"1600000020\""));
    assert!(selected.contains("data-time=\"1600000060\""));
    assert!(!selected.contains("data-time=\"1600000030\""));
    assert!(!selected.contains("data-time=\"1600000070\""));
    assert!(
        !selected.contains("class=\"series\""),
        "再起動をまたいで線を引かない"
    );
}

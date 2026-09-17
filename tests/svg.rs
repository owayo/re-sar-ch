mod fixtures;

use fixtures::{ActivitySpec, FixtureAbi, FixtureSpec, Generation, RecordSpec, build};
use re_sar_ch::cli::{SadfOutputOptions, SvgPalette};
use re_sar_ch::format::file::SaFile;
use re_sar_ch::model::ActivityId;
use re_sar_ch::output::sadf::{SadfConfig, TimeBase};
use re_sar_ch::output::sar_text::CpuSelection;
use re_sar_ch::output::svg::write_svg;
use re_sar_ch::output::time_filter::{TimeBound, TimeFilter};

fn sensor_file(counts: &[i32]) -> SaFile {
    let mut spec = FixtureSpec::skeleton(Generation::G2175Current, FixtureAbi::Le64);
    spec.nodename = "chart<&\"host".into();
    // docs/format/02-activities.md: A_PWR_FAN, two doubles then device[20], 40 bytes.
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
    // Each series has two points before and two after restart, exactly two segments.
    assert_eq!(
        svg.matches("class=\"point\"").count(),
        svg.matches("class=\"series\"").count() * 2
    );
    assert!(svg.contains("%idle"));
    let svg_without_idle = render(&file, &cfg, &SadfOutputOptions::default());
    assert!(!svg_without_idle.contains("%idle"));
}

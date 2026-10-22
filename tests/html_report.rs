//! 独立した sa fixture で、HTML のデータ契約と CLI の保存を検証する。
mod fixtures;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use fixtures::{ActivitySpec, FixtureAbi, FixtureSpec, Generation, RecordSpec, build};
use serde_json::Value;

const START: u64 = 1_600_000_000;

fn source(dir: &Path, host: &str, restart: bool) -> PathBuf {
    let mut spec = FixtureSpec::skeleton(Generation::G2175Current, FixtureAbi::Le64);
    spec.nodename = host.into();
    spec.cpu_nr = 2;
    spec.activities = vec![ActivitySpec::a_cpu(2), ActivitySpec::a_queue()];
    spec.records = (0..=90)
        .map(|i| {
            let t = START + i * 60;
            let mut record = if restart && i == 60 {
                RecordSpec::restart(
                    2,
                    t,
                    ((t / 3600) % 24) as u8,
                    ((t / 60) % 60) as u8,
                    (t % 60) as u8,
                )
            } else {
                RecordSpec::stats(
                    vec![2, 0],
                    t,
                    ((t / 3600) % 24) as u8,
                    ((t / 60) % 60) as u8,
                    (t % 60) as u8,
                )
            };
            record.uptime = if restart && i >= 60 {
                (i - 60) * 6000
            } else {
                100_000 + i * 6000
            };
            record
        })
        .collect();
    let mut fixture = build(spec);
    let (mut user, mut idle) = (10_000u64, 90_000u64);
    for (i, (offset, _)) in fixture.record_offsets.iter().enumerate() {
        if restart && i == 60 {
            continue;
        }
        let spike = (30..=34).contains(&i) || (70..=74).contains(&i);
        user += if spike { 6000 } else { 600 };
        idle += if spike { 0 } else { 5400 };
        // docs/format/02: record_header=24, __nr_t=4, CPU=80, user@0, idle@24。
        let at = offset + 24 + 4;
        for cpu in 0..2 {
            let cpu_at = at + cpu * 80;
            fixture.bytes[cpu_at..cpu_at + 80].fill(0);
            fixture.bytes[cpu_at..cpu_at + 8].copy_from_slice(&user.to_le_bytes());
            fixture.bytes[cpu_at + 24..cpu_at + 32].copy_from_slice(&idle.to_le_bytes());
        }
        // QUEUE は固定1件40バイト。runq uint64@0。
        let queue = at + 160;
        fixture.bytes[queue..queue + 40].fill(0);
        fixture.bytes[queue..queue + 8]
            .copy_from_slice(&(if spike { 100u64 } else { 0 }).to_le_bytes());
    }
    let path = dir.join(if restart { "restart.sa" } else { "samples.sa" });
    std::fs::write(&path, fixture.bytes).unwrap();
    path
}

fn run(input: &Path, extra: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_resarch"));
    command.arg("report");
    if !extra.contains(&"--output") {
        command.args(["--output", "-"]);
    }
    command
        .arg(input)
        .args(["--lang", "en", "--timezone", "Asia/Tokyo"])
        .args(extra)
        .env("TZ", "Pacific/Honolulu")
        .output()
        .unwrap()
}

fn payload(html: &str) -> Value {
    let start = html
        .find("<script id=\"report-data\" type=\"application/json\">")
        .unwrap();
    let json = &html[start..];
    let json = &json[json.find('>').unwrap() + 1..];
    serde_json::from_str(&json[..json.find("</script>").unwrap()]).unwrap()
}

#[test]
fn standalone_report_preserves_computed_values_and_detection_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let path = source(dir.path(), "synthetic-host", false);
    let result = run(&path, &[]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let html = String::from_utf8(result.stdout).unwrap();
    assert!(html.starts_with("<!doctype html>\n<html lang=\"en\">"));
    assert!(html.contains("default-src 'none'"));
    assert!(!html.contains("<script src="));
    assert!(!html.contains("<link "));
    let report = payload(&html);
    assert_eq!(report["partial"], false);
    let segment = &report["segments"][0];
    assert_eq!(segment["timezone"], "Asia/Tokyo");
    assert_eq!(segment["times"][0]["label"], "2020-09-13 21:26:40+09:00");
    let series = segment["series"].as_array().unwrap();
    let idle = series
        .iter()
        .find(|s| s["activity"] == "A_CPU" && s["item"] == "all" && s["metric"] == "idle")
        .unwrap();
    assert_eq!(idle["unit"], "%");
    assert_eq!(idle["percent"], true);
    let points = idle["points"].as_array().unwrap();
    assert!(points.iter().any(|p| p[2] == 90.0));
    assert!(points.iter().any(|p| p[2] == 0.0));
    assert!(!segment["findings"].as_array().unwrap().is_empty());
    let cli = Command::new(env!("CARGO_BIN_EXE_resarch"))
        .args([
            "detect",
            "--lang",
            "en",
            "--timezone",
            "Asia/Tokyo",
            "--format",
            "json",
        ])
        .arg(&path)
        .output()
        .unwrap();
    let expected: Value = serde_json::from_slice(&cli.stdout).unwrap();
    // JSON writer が付ける日時表記以外、評価と検知の中身は同じ。
    assert_eq!(
        segment["assessment"]["coverage"],
        expected["assessments"][0]["coverage"]
    );
}

#[test]
fn source_text_cannot_end_the_embedded_json_script() {
    let dir = tempfile::tempdir().unwrap();
    let hostile = "</ScRiPt><script>alert(1)</script>&";
    let path = source(dir.path(), hostile, false);
    let result = run(&path, &[]);
    assert!(result.status.success());
    let html = String::from_utf8(result.stdout).unwrap();
    assert!(!html.contains(hostile));
    assert!(html.contains("\\u003c/ScRiPt\\u003e"));
    assert_eq!(payload(&html)["segments"][0]["host"], hostile);
}

#[test]
fn restart_segments_and_activity_selection_remain_separate() {
    let dir = tempfile::tempdir().unwrap();
    let path = source(dir.path(), "synthetic-host", true);
    let result = run(&path, &["--activity", "cpu"]);
    assert!(result.status.success());
    let report = payload(&String::from_utf8(result.stdout).unwrap());
    let segments = report["segments"].as_array().unwrap();
    assert_eq!(segments.len(), 2);
    for segment in segments {
        assert!(
            segment["series"]
                .as_array()
                .unwrap()
                .iter()
                .all(|s| s["activity"] == "A_CPU")
        );
        for series in segment["series"].as_array().unwrap() {
            let first = &series["points"][0];
            assert_eq!(first[4], false, "最初の採取は前の起動区間と接続しない");
        }
    }
}

#[test]
fn existing_output_is_preserved_and_failed_reads_leave_no_output() {
    let dir = tempfile::tempdir().unwrap();
    let path = source(dir.path(), "synthetic-host", false);
    let output = dir.path().join("report.html");
    std::fs::write(&output, "keep me").unwrap();
    let result = run(&path, &["--output", output.to_str().unwrap()]);
    assert!(!result.status.success());
    assert_eq!(std::fs::read_to_string(&output).unwrap(), "keep me");
    std::fs::remove_file(&output).unwrap();
    let result = run(
        &dir.path().join("absent.sa"),
        &["--output", output.to_str().unwrap()],
    );
    assert!(!result.status.success());
    assert!(!output.exists());
}

#[test]
fn lenient_report_exposes_partial_input_and_returns_failure() {
    let dir = tempfile::tempdir().unwrap();
    let path = source(dir.path(), "synthetic-host", false);
    let result = run(
        &path,
        &["--lenient", dir.path().join("absent.sa").to_str().unwrap()],
    );
    assert!(!result.status.success());
    let report = payload(&String::from_utf8(result.stdout).unwrap());
    assert_eq!(report["partial"], true);
    assert_eq!(report["skipped"].as_array().unwrap().len(), 1);
    assert!(!report["segments"].as_array().unwrap().is_empty());
}

#[test]
fn missing_nonfinite_and_large_epochs_keep_their_meaning() {
    use re_sar_ch::analyze::timeline::{ExclusionReason, MetricKey, MetricPoint};
    use re_sar_ch::analyze::{RetainTimelines, SummaryOptions, assess_summary};
    use re_sar_ch::detect::DetectOptions;
    use re_sar_ch::model::{ActivityId, DisplayTz, Lang, Unit, ValueKind};
    use re_sar_ch::multi::{MultiOptions, analyze_files};
    use re_sar_ch::output::html_report::write_report;

    let dir = tempfile::tempdir().unwrap();
    let path = source(dir.path(), "synthetic-host", false);
    let mut analysis = analyze_files(
        &[path],
        &MultiOptions {
            summary: SummaryOptions {
                retain: RetainTimelines::All,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .unwrap();
    let summary = &mut analysis.hosts[0].segments[0].summary;
    let timeline = summary.timelines.entry(
        MetricKey::new(ActivityId::CPU, "all", "idle"),
        Unit::Percent,
        ValueKind::Counter,
    );
    let end = u64::MAX - 1000;
    timeline.points = [Some(5.0), None, Some(f64::NAN), Some(200.0), Some(10.0)]
        .into_iter()
        .enumerate()
        .map(|(i, value)| MetricPoint {
            start_ust: end + i as u64 * 60,
            end_ust: end + (i as u64 + 1) * 60,
            elapsed_cs: 6000,
            value,
            reason: if value.is_none() {
                Some(ExclusionReason::MissingInSample)
            } else {
                None
            },
        })
        .collect();
    let opts = DetectOptions {
        lang: Lang::En,
        ..Default::default()
    };
    let assessment = assess_summary(summary, &opts);
    let mut bytes = Vec::new();
    write_report(
        &mut bytes,
        &analysis,
        &[assessment],
        DisplayTz::Utc,
        Lang::En,
    )
    .unwrap();
    let report = payload(&String::from_utf8(bytes).unwrap());
    let segment = &report["segments"][0];
    let expected_epoch = end.to_string();
    assert!(
        segment["times"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["epoch"].as_str() == Some(expected_epoch.as_str()))
    );
    let series = segment["series"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["activity"] == "A_CPU" && s["item"] == "all" && s["metric"] == "idle")
        .unwrap();
    let points = series["points"].as_array().unwrap();
    assert_eq!(points[1][2], Value::Null);
    assert_eq!(points[1][3], "missing_in_sample");
    assert_eq!(points[2][2], Value::Null);
    assert_eq!(points[2][3], "not_finite");
    assert_eq!(points[3][2], 200.0, "百分率の範囲外の実測を失わない");
    assert_eq!(points[3][4], false, "欠測をまたいで接続しない");
    assert_eq!(points[4][4], true);
}

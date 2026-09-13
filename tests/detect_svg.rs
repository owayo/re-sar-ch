//! 独立に作った CPU/QUEUE の sa バイナリで、検知→SVG 保存までを検証する。
mod fixtures;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use fixtures::{ActivitySpec, FixtureAbi, FixtureSpec, Generation, RecordSpec, build};
use serde_json::Value;

const START: u64 = 1_600_000_000;

fn source(dir: &Path, name: &str, anomalous: bool) -> PathBuf {
    let mut spec = FixtureSpec::skeleton(Generation::G2175Current, FixtureAbi::Le64);
    spec.nodename = name.into();
    spec.cpu_nr = 2;
    spec.activities = vec![ActivitySpec::a_cpu(2), ActivitySpec::a_queue()];
    spec.records = (0..=90)
        .map(|i| {
            let t = START + i * 60;
            let mut record = RecordSpec::stats(
                vec![2, 0],
                t,
                ((t / 3600) % 24) as u8,
                ((t / 60) % 60) as u8,
                (t % 60) as u8,
            );
            record.uptime = 100_000 + i * 6000;
            record
        })
        .collect();
    let mut fixture = build(spec);
    let mut user = 10_000u64;
    let mut idle = 90_000u64;
    for (i, (offset, _)) in fixture.record_offsets.iter().enumerate() {
        let spike = anomalous && ((30..=34).contains(&i) || (70..=74).contains(&i));
        user += if spike { 6000 } else { 600 };
        idle += if spike { 0 } else { 5400 };
        // docs/format/02: record_header=24, __nr_t=4, CPU=80 (user@0, idle@24).
        let at = offset + 24 + 4;
        for cpu in 0..2 {
            let cpu_at = at + cpu * 80;
            fixture.bytes[cpu_at..cpu_at + 80].fill(0);
            fixture.bytes[cpu_at..cpu_at + 8].copy_from_slice(&user.to_le_bytes());
            fixture.bytes[cpu_at + 24..cpu_at + 32].copy_from_slice(&idle.to_le_bytes());
        }
        // QUEUE: 固定1件・40バイト、runq uint64@0、残りのフィールドは0。
        let queue = at + 160;
        fixture.bytes[queue..queue + 40].fill(0);
        let runq: u64 = if spike { 100 } else { 0 };
        fixture.bytes[queue..queue + 8].copy_from_slice(&runq.to_le_bytes());
    }
    let path = dir.join(format!("{}.sa", if anomalous { "spikes" } else { "quiet" }));
    std::fs::write(&path, fixture.bytes).unwrap();
    path
}

fn run(input: &Path, dir: &Path, extra: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_resarch"))
        .arg("detect")
        .arg(input)
        .args(["--format", "json", "--svg-dir"])
        .arg(dir)
        .args(extra)
        .env("TZ", "Pacific/Honolulu")
        .output()
        .unwrap()
}

fn index(dir: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(dir.join("index.json")).unwrap()).unwrap()
}

#[test]
fn detects_local_windows_per_resource_and_keeps_the_normal_report() {
    let temp = tempfile::tempdir().unwrap();
    let input = source(temp.path(), "chart<&host", true);
    let dir = temp.path().join("charts");
    let result = run(&input, &dir, &["--svg-context", "5m"]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let stdout: Value = serde_json::from_slice(&result.stdout).unwrap();
    let saved: Value =
        serde_json::from_slice(&std::fs::read(dir.join("report.json")).unwrap()).unwrap();
    assert_eq!(stdout, saved);
    let manifest = index(&dir);
    assert_eq!(manifest["status"], "complete");
    let charts = manifest["charts"].as_array().unwrap();
    assert!(charts.len() >= 4, "{manifest:#}");
    assert!(
        charts
            .iter()
            .any(|c| c["series"]["activity_name"] == "A_CPU")
    );
    assert!(
        charts
            .iter()
            .any(|c| c["series"]["activity_name"] == "A_QUEUE")
    );
    for chart in charts {
        assert_eq!(chart["context_secs"], 300);
        let start = chart["window_start_ust"].as_u64().unwrap();
        let end = chart["window_end_ust"].as_u64().unwrap();
        assert!(start >= START && end <= START + 90 * 60);
        assert!(
            end - start < 30 * 60,
            "local anomaly must not become a full-input chart: {chart}"
        );
        let file = chart["file"].as_str().unwrap();
        assert_eq!(Path::new(file).components().count(), 1);
        let svg = std::fs::read_to_string(dir.join(file)).unwrap();
        assert!(svg.contains("<svg") && svg.ends_with("</svg>\n"));
        assert!(svg.contains("chart&lt;&amp;host"));
        assert!(!svg.contains("chart<&host"));
        assert!(svg.contains("UTC"));
        assert!(!svg.contains("NaN") && !svg.contains("Infinity"));
    }
}

#[test]
fn report_window_keeps_surrounding_context_and_activity_selection() {
    let temp = tempfile::tempdir().unwrap();
    let input = source(temp.path(), "testhost", true);
    let dir = temp.path().join("charts");
    let from = (START + 30 * 60).to_string();
    let to = (START + 34 * 60).to_string();
    let result = run(
        &input,
        &dir,
        &[
            "--activity",
            "cpu",
            "--from",
            &from,
            "--to",
            &to,
            "--svg-context",
            "10m",
        ],
    );
    assert!(result.status.success(), "{:?}", result);
    let manifest = index(&dir);
    let charts = manifest["charts"].as_array().unwrap();
    assert_eq!(charts.len(), 1, "{manifest:#}");
    assert_eq!(charts[0]["series"]["activity_name"], "A_CPU");
    assert!(charts[0]["window_start_ust"].as_u64().unwrap() < from.parse::<u64>().unwrap());
    assert!(charts[0]["window_end_ust"].as_u64().unwrap() > to.parse::<u64>().unwrap());
}

#[test]
fn no_detections_produces_an_empty_index_with_evaluation_report() {
    let temp = tempfile::tempdir().unwrap();
    let input = source(temp.path(), "quiet-host", false);
    let dir = temp.path().join("charts");
    let result = run(&input, &dir, &[]);
    assert!(result.status.success(), "{:?}", result);
    assert!(index(&dir)["charts"].as_array().unwrap().is_empty());
    assert!(dir.join("report.json").is_file());
    assert_eq!(std::fs::read_dir(dir).unwrap().count(), 2);
}

#[test]
fn existing_directory_is_preserved_and_invalid_context_is_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let input = source(temp.path(), "testhost", true);
    let dir = temp.path().join("charts");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("keep"), "keep").unwrap();
    assert!(!run(&input, &dir, &[]).status.success());
    assert_eq!(std::fs::read_to_string(dir.join("keep")).unwrap(), "keep");
    for value in ["15", "-1m", "1.5h", "m", "18446744073709551615h"] {
        assert!(
            !run(&input, &temp.path().join("new"), &["--svg-context", value])
                .status
                .success()
        );
    }
    let no_dir = Command::new(env!("CARGO_BIN_EXE_resarch"))
        .arg("detect")
        .arg(&input)
        .args(["--svg-context", "1m"])
        .output()
        .unwrap();
    assert!(!no_dir.status.success());
    assert!(!temp.path().join("new").exists());
}

#[test]
fn lenient_output_marks_the_bundle_as_partial() {
    let temp = tempfile::tempdir().unwrap();
    let input = source(temp.path(), "testhost", true);
    let mut bytes = std::fs::read(&input).unwrap();
    bytes.pop();
    std::fs::write(&input, bytes).unwrap();
    let dir = temp.path().join("charts");
    let result = run(&input, &dir, &["--lenient", "--svg-context", "5m"]);
    assert!(!result.status.success());
    let manifest = index(&dir);
    assert_eq!(manifest["status"], "partial");
    assert!(!manifest["incomplete_files"].as_array().unwrap().is_empty());
    assert!(!manifest["charts"].as_array().unwrap().is_empty());
}

#[test]
#[ignore = "xmllint が必要。CI の conformance ジョブで実行する"]
fn generated_detect_svgs_are_valid_xml() {
    let temp = tempfile::tempdir().unwrap();
    let input = source(temp.path(), "chart<&host", true);
    let dir = temp.path().join("charts");
    let result = run(&input, &dir, &["--svg-context", "5m"]);
    assert!(result.status.success(), "{:?}", result);
    let manifest = index(&dir);
    let charts = manifest["charts"].as_array().unwrap();
    assert!(!charts.is_empty());
    for chart in charts {
        let file = chart["file"].as_str().unwrap();
        let xml = Command::new("xmllint")
            .arg("--noout")
            .arg(dir.join(file))
            .output()
            .expect("install xmllint");
        assert!(
            xml.status.success(),
            "{file}: {}",
            String::from_utf8_lossy(&xml.stderr)
        );
        if let Some(preview) = std::env::var_os("RESARCH_SVG_TEST_OUTPUT") {
            std::fs::create_dir_all(&preview).unwrap();
            std::fs::copy(dir.join(file), Path::new(&preview).join(file)).unwrap();
        }
    }
}

#[cfg(unix)]
#[test]
fn many_graphs_do_not_keep_all_output_files_open() {
    let temp = tempfile::tempdir().unwrap();
    let mut inputs = Vec::new();
    for i in 0..20 {
        let parent = temp.path().join(format!("host-{i}"));
        std::fs::create_dir(&parent).unwrap();
        inputs.push(source(&parent, &format!("host-{i}"), true));
    }
    let dir = temp.path().join("charts");
    let result = Command::new("sh")
        .args([
            "-c",
            "ulimit -n 64; exec \"$@\"",
            "detect-svg-test",
            env!("CARGO_BIN_EXE_resarch"),
            "detect",
        ])
        .args(inputs)
        .arg("--svg-dir")
        .arg(&dir)
        .args(["--svg-context", "5m"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(index(&dir)["charts"].as_array().unwrap().len() > 64);
}

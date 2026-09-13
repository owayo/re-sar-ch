//! Issue #3 / #6 / #10 / #11 の CLI 配線を自作 fixture で検証する。
mod fixtures;

use fixtures::{ActivitySpec, FixtureAbi, FixtureSpec, Generation, RecordSpec, build};
use std::path::Path;
use std::process::{Command, Output};

fn run(args: &[&str], file: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_resarch"))
        .args(args)
        .arg(file)
        .env("TZ", "UTC")
        .env("LC_ALL", "C")
        .output()
        .unwrap()
}

fn source(offset_hours: u64, pcsw: bool) -> Vec<u8> {
    let mut spec = FixtureSpec::skeleton(Generation::G2175Current, FixtureAbi::Le64);
    spec.activities = vec![ActivitySpec::a_cpu(3)];
    if pcsw {
        spec.activities.push(ActivitySpec::a_pcsw());
        spec.activities.push(ActivitySpec::a_queue());
    }
    // UTC 19:50 から翌日の 16:30 まで。採取ホストの時刻だけを +9h にもできる。
    let start = 1_600_000_000 / 86_400 * 86_400 + 19 * 3600 + 50 * 60;
    spec.ust_time = start;
    spec.records = (0..125)
        .map(|i| {
            let t = start + i * 600;
            let mut counts = vec![3];
            if pcsw {
                counts.push(0);
                counts.push(0);
            }
            let mut r = RecordSpec::stats(
                counts,
                t,
                ((t / 3600 + offset_hours) % 24) as u8,
                ((t / 60) % 60) as u8,
                0,
            );
            r.uptime = 100_000 + i * 60_000;
            r
        })
        .collect();
    build(spec).bytes
}

fn save(dir: &Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

#[test]
fn utc_overnight_filter_is_independent_of_recorded_timezone() {
    let dir = tempfile::tempdir().unwrap();
    for offset in [0, 9] {
        let file = save(
            dir.path(),
            &format!("utc-plus-{offset}.sa"),
            &source(offset, true),
        );
        let out = run(
            &[
                "show",
                "--activity",
                "pcsw",
                "--format",
                "ndjson",
                "--from",
                "20:00",
                "--to",
                "02:00",
            ],
            &file,
        );
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let rows: Vec<serde_json::Value> = String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(rows.len(), 36, "UTC+{offset}");
        assert_eq!(
            rows.last().unwrap()["end_epoch"].as_u64().unwrap() % 86_400,
            7200
        );
    }
}

#[test]
fn absent_requested_activities_fail_without_a_banner() {
    let dir = tempfile::tempdir().unwrap();
    let file = save(dir.path(), "cpu.sa", &source(0, false));
    let out = run(&["sar", "-r", "-f"], &file);
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("Requested activities not available in file")
    );
    let out = Command::new(env!("CARGO_BIN_EXE_resarch"))
        .args(["sadf", "-d"])
        .arg(&file)
        .args(["--", "-r"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("Requested activities not available in file")
    );
}

#[test]
fn lenient_reports_partial_results_and_fails_for_every_native_scan() {
    let dir = tempfile::tempdir().unwrap();
    let mut bytes = source(0, true);
    bytes.truncate(bytes.len() - 1);
    let file = save(dir.path(), "truncated.sa", &bytes);
    for command in ["show", "summarize", "detect"] {
        let out = run(&[command, "--lenient", "--format", "ndjson"], &file);
        assert!(!out.status.success(), "{command}");
        assert!(
            !out.stdout.is_empty(),
            "{command}: 完全な先行レコードを保持"
        );
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("不完全な末尾レコード"),
            "{command}"
        );
    }
}

#[test]
fn native_cpu_count_and_epoch_validation_are_consistent() {
    let dir = tempfile::tempdir().unwrap();
    let file = save(dir.path(), "cpu.sa", &source(0, true));
    let out = run(&["info", "--format", "json"], &file);
    assert!(out.status.success());
    let info: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(info["header"]["cpu_nr"], 2);
    assert_eq!(info["header"]["sa_cpu_nr"], 3);
    assert_eq!(info["schema_version"], "1.0");
    for command in ["show", "summarize", "detect"] {
        let out = run(
            &[command, "--from", "1700000000", "--to", "1600000000"],
            &file,
        );
        assert!(!out.status.success(), "{command}");
        assert!(out.stdout.is_empty());
        assert!(String::from_utf8_lossy(&out.stderr).contains("--to は --from"));
    }
}

#[test]
fn compare_retains_the_reason_for_skipping_metrics() {
    let dir = tempfile::tempdir().unwrap();
    let a = save(dir.path(), "a.sa", &source(0, true));
    let b = save(dir.path(), "b.sa", &source(0, false));
    let out = Command::new(env!("CARGO_BIN_EXE_resarch"))
        .args(["compare", "--format", "json", "--host"])
        .arg(format!("a={}", a.display()))
        .arg("--host")
        .arg(format!("b={}", b.display()))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(!report["comparisons"].as_array().unwrap().is_empty());
    let skipped = report["skipped_metrics"].as_array().unwrap();
    assert!(!skipped.is_empty());
    assert!(
        skipped
            .iter()
            .any(|m| m["missing_on"] == serde_json::json!(["b"]))
    );
}

#[test]
fn native_help_explains_utc() {
    for command in ["show", "summarize", "compare", "detect"] {
        let out = Command::new(env!("CARGO_BIN_EXE_resarch"))
            .args([command, "--help"])
            .output()
            .unwrap();
        assert!(out.status.success());
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("UTC"),
            "{command}"
        );
    }
}

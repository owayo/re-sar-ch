//! Issue #3 / #6 / #10 / #11 の CLI 配線を自作 fixture で検証する。
mod fixtures;

use fixtures::{ActivitySpec, FixtureAbi, FixtureSpec, Generation, RecordSpec, build};
use std::path::Path;
use std::process::{Command, Output};

fn run(args: &[&str], file: &Path) -> Output {
    run_in_tz("UTC", args, file)
}

/// 実行環境のタイムゾーンを指定して起動する。
///
/// 独自出力の既定は**実行環境のローカルタイムゾーン**なので、
/// `TZ` を固定しないと期待値が実行機ごとに変わる。
fn run_in_tz(tz: &str, args: &[&str], file: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_resarch"))
        .args(args)
        .arg(file)
        .env("TZ", tz)
        .env("LC_ALL", "C")
        .output()
        .unwrap()
}

/// `TZ=Asia/Tokyo` のときの日内秒。JST に夏時間は無いので固定 +9 時間でよい。
fn jst_seconds_of_day(epoch: u64) -> u64 {
    (epoch + 9 * 3600) % 86_400
}

/// 表に出ている最初の時刻セル (`HH:MM:SS`)。
///
/// 期待値に fixture の絶対時刻を書かない。同じ入力を別のタイムゾーンで
/// 出し直し、**ずれ幅**を突き合わせる (fixture の開始時刻を変えても壊れない)。
fn first_time_cell(text: &str) -> String {
    text.lines()
        .filter_map(|l| l.split_whitespace().next())
        .find(|head| {
            head.len() == 8
                && head.as_bytes()[2] == b':'
                && head.as_bytes()[5] == b':'
                && head.bytes().filter(u8::is_ascii_digit).count() == 6
        })
        .map(str::to_string)
        .unwrap_or_else(|| panic!("時刻セルが見つからない: {text}"))
}

/// `HH:MM:SS` を `offset_secs` だけ進めた表記 (日を跨いでも巻き戻す)。
fn shifted(cell: &str, offset_secs: u64) -> String {
    let p: Vec<u64> = cell.split(':').map(|v| v.parse().unwrap()).collect();
    let s = (p[0] * 3600 + p[1] * 60 + p[2] + offset_secs) % 86_400;
    format!("{:02}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
}

/// `show --format table` の最初の時刻セルを、指定タイムゾーンで取る。
fn first_cell_in_tz(tz: &str, extra: &[&str], file: &Path) -> String {
    let mut args = vec!["show", "--activity", "cpu"];
    args.extend_from_slice(extra);
    let out = run_in_tz(tz, &args, file);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    first_time_cell(&String::from_utf8(out.stdout).unwrap())
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
fn tui_preparation_keeps_partial_scan_diagnostics() {
    use re_sar_ch::format::{OpenOptions, SaFile, Tolerance};
    let bytes = source(0, false);
    let file = SaFile::from_bytes("synthetic", bytes.clone()).unwrap();
    let prepared = re_sar_ch::tui::prepare(&file, &Default::default()).unwrap();
    assert!(prepared.scan_summary.is_exact());
    assert_eq!(prepared.scan_summary.stats, 125);
    let mut truncated = bytes;
    truncated.pop();
    let strict = SaFile::from_bytes("synthetic", truncated.clone()).unwrap();
    assert!(re_sar_ch::tui::prepare(&strict, &Default::default()).is_err());
    let lenient = SaFile::from_bytes_with(
        "synthetic",
        truncated,
        OpenOptions {
            tolerance: Tolerance::Lenient,
            ..Default::default()
        },
    )
    .unwrap();
    let prepared = re_sar_ch::tui::prepare(&lenient, &Default::default()).unwrap();
    assert!(prepared.scan_summary.incomplete);
    assert!(prepared.scan_summary.trailing_bytes > 0);
    assert_eq!(prepared.scan_summary.stats, 124);
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
fn zero_elapsed_rates_are_missing_in_native_output() {
    let dir = tempfile::tempdir().unwrap();
    let mut spec = FixtureSpec::skeleton(Generation::G2175Current, FixtureAbi::Le64);
    spec.activities = vec![ActivitySpec::a_pcsw(), ActivitySpec::a_queue()];
    spec.records = (0..2)
        .map(|_| {
            let mut rec = RecordSpec::stats(vec![1, 1], 1_600_000_000, 12, 0, 0);
            rec.uptime = 100_000;
            rec
        })
        .collect();
    let file = save(dir.path(), "zero-elapsed.sa", &build(spec).bytes);
    let out = run(&["show", "--format", "json"], &file);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let sample = &json["samples"][1];
    assert_eq!(sample["elapsed_cs"], 0);
    assert_eq!(sample["continuous"], false);
    let activities = sample["activities"].as_array().unwrap();
    let pcsw = activities
        .iter()
        .find(|a| a["activity"] == "A_PCSW")
        .unwrap();
    for rate in pcsw["items"][0]["rates"].as_array().unwrap() {
        assert_eq!(rate["quality"], "non_positive_elapsed");
        assert!(rate.get("value").is_none());
    }
    // 瞬時値は経過時間を必要としないので、同じサンプルでも有効なまま。
    let queue = activities
        .iter()
        .find(|a| a["activity"] == "A_QUEUE")
        .unwrap();
    assert!(
        queue["items"][0]["rates"]
            .as_array()
            .unwrap()
            .iter()
            .all(|rate| rate["quality"] == "ok")
    );
    for format in ["csv", "ndjson"] {
        let out = run(&["show", "--format", format], &file);
        assert!(
            out.status.success(),
            "{format}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("non_positive_elapsed"),
            "{format}"
        );
    }
    let out = run(&["show", "--activity", "pcsw", "--format", "table"], &file);
    assert!(out.status.success());
    let table = String::from_utf8(out.stdout).unwrap();
    assert!(
        table
            .lines()
            .any(|line| line.split_whitespace().filter(|cell| *cell == "-").count() >= 2)
    );
}

#[test]
fn restart_elapsed_is_zero_in_each_native_machine_readable_format() {
    let dir = tempfile::tempdir().unwrap();
    let mut spec = FixtureSpec::skeleton(Generation::G2175Current, FixtureAbi::Le64);
    spec.activities = vec![ActivitySpec::a_pcsw()];
    let mut first = RecordSpec::stats(vec![1], 1_600_000_000, 12, 0, 0);
    first.uptime = 100_000;
    let restart = RecordSpec::restart(2, 1_600_000_001, 12, 0, 1);
    let mut after = RecordSpec::stats(vec![1], 1_600_000_002, 12, 0, 2);
    after.uptime = 200_000;
    spec.records = vec![first, restart, after];
    let file = save(dir.path(), "restart.sa", &build(spec).bytes);
    let out = run(&["show", "--format", "json"], &file);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["samples"][1]["elapsed_cs"], 0);
    assert_eq!(json["samples"][1]["continuous"], false);
    let out = run(&["show", "--format", "ndjson"], &file);
    assert!(out.status.success());
    let rows: Vec<serde_json::Value> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let last = rows.last().unwrap();
    assert_eq!(last["elapsed_cs"], 0);
    assert_eq!(last["continuous"], false);
    let out = run(&["show", "--format", "csv"], &file);
    assert!(out.status.success());
    let mut reader = csv::Reader::from_reader(out.stdout.as_slice());
    let headers = reader.headers().unwrap().clone();
    let elapsed = headers.iter().position(|h| h == "elapsed_cs").unwrap();
    let continuous = headers.iter().position(|h| h == "continuous").unwrap();
    let last = reader.records().last().unwrap().unwrap();
    assert_eq!(&last[elapsed], "0");
    assert_eq!(&last[continuous], "false");
}

#[test]
fn svg_extreme_epochs_do_not_panic_or_become_1970() {
    let dir = tempfile::tempdir().unwrap();
    for end in [i64::MAX as u64, i64::MAX as u64 + 1, u64::MAX] {
        let mut spec = FixtureSpec::skeleton(Generation::G2175Current, FixtureAbi::Le64);
        spec.activities = vec![ActivitySpec::a_cpu(3)];
        spec.records = (0..3)
            .map(|i| {
                let mut rec = RecordSpec::stats(vec![3], end - (2 - i) * 600, 12, 0, 0);
                rec.uptime = 100_000 + i * 60_000;
                rec
            })
            .collect();
        let file = save(dir.path(), "extreme.sa", &build(spec).bytes);
        for options in [vec!["sadf", "-g"], vec!["sadf", "-g", "-O", "oneday"]] {
            let out = run(&options, &file);
            assert!(
                out.status.success(),
                "{end}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let svg = String::from_utf8(out.stdout).unwrap();
            assert!(svg.contains("</svg>"));
            assert!(svg.contains(&end.to_string()));
            assert!(!svg.contains("1970-01-01"));
        }
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

fn comparison_fixture(host: &str, cpu_nr: u32, start: u64, samples: u64) -> FixtureSpec {
    let mut spec = FixtureSpec::skeleton(Generation::G2175Current, FixtureAbi::Le64);
    spec.nodename = host.to_string();
    spec.cpu_nr = cpu_nr;
    spec.ust_time = start;
    spec.activities = vec![ActivitySpec::a_cpu(cpu_nr as i32)];
    spec.records = (0..samples)
        .map(|i| {
            let t = start + i * 60;
            let mut record = RecordSpec::stats(
                vec![cpu_nr as i32],
                t,
                ((t / 3600) % 24) as u8,
                ((t / 60) % 60) as u8,
                (t % 60) as u8,
            );
            record.uptime = 100_000 + i * 6000;
            record
        })
        .collect();
    spec
}

#[test]
fn compare_rejects_multiple_host_groups_in_one_directory() {
    for difference in ["nodename", "machine", "sysname"] {
        let dir = tempfile::tempdir().unwrap();
        let mixed = dir.path().join("mixed");
        std::fs::create_dir(&mixed).unwrap();
        let a = comparison_fixture("fixture-a", 3, 1_600_000_000, 4);
        let mut b = a.clone();
        match difference {
            "nodename" => b.nodename = "fixture-b".into(),
            "machine" => b.machine = "aarch64".into(),
            "sysname" => b.sysname = "OtherOS".into(),
            _ => unreachable!(),
        }
        save(&mixed, "sa01", &build(a.clone()).bytes);
        save(&mixed, "sa02", &build(b).bytes);
        let peer = save(dir.path(), "peer.sa", &build(a).bytes);
        for format in ["table", "json", "ndjson"] {
            for lenient in [false, true] {
                let mut cmd = Command::new(env!("CARGO_BIN_EXE_resarch"));
                cmd.args(["compare", "--utc", "--format", format, "--host"])
                    .arg(format!("mixed={}", mixed.display()))
                    .arg("--host")
                    .arg(format!("peer={}", peer.display()));
                if lenient {
                    cmd.arg("--lenient");
                }
                let out = cmd.output().unwrap();
                assert!(!out.status.success(), "{difference}/{format}/{lenient}");
                assert!(out.stdout.is_empty());
                let err = String::from_utf8_lossy(&out.stderr);
                assert!(
                    err.contains("--host mixed") && err.contains("複数のホスト"),
                    "{err}"
                );
            }
        }
    }
}

#[test]
fn compare_identity_matches_the_selected_boot_segment() {
    let dir = tempfile::tempdir().unwrap();
    let upgraded = dir.path().join("upgraded");
    std::fs::create_dir(&upgraded).unwrap();
    let mut old = comparison_fixture("fixture-a", 3, 1_600_000_000, 2);
    old.release = "old-kernel".into();
    let mut new = comparison_fixture("fixture-a", 5, 1_600_000_600, 4);
    new.release = "new-kernel".into();
    save(&upgraded, "sa01", &build(old).bytes);
    save(&upgraded, "sa02", &build(new.clone()).bytes);
    new.nodename = "fixture-b".into();
    let peer = save(dir.path(), "peer.sa", &build(new).bytes);

    for from in [None, Some("1600000600")] {
        for format in ["json", "ndjson"] {
            let out = Command::new(env!("CARGO_BIN_EXE_resarch"))
                .args(["compare", "--utc", "--format", format, "--host"])
                .arg(format!("upgraded={}", upgraded.display()))
                .arg("--host")
                .arg(format!("peer={}", peer.display()))
                .args(from.into_iter().flat_map(|start| ["--from", start]))
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            let comparisons: Vec<serde_json::Value> = if format == "ndjson" {
                String::from_utf8(out.stdout)
                    .unwrap()
                    .lines()
                    .skip(1)
                    .map(|line| serde_json::from_str(line).unwrap())
                    .collect()
            } else {
                let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
                report["comparisons"].as_array().unwrap().clone()
            };
            assert!(!comparisons.is_empty());
            for comparison in comparisons {
                let host = comparison["hosts"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|host| host["label"] == "upgraded")
                    .unwrap();
                assert_eq!(comparison["window"]["start_ust"], 1_600_000_600u64);
                assert_eq!(host["identity"]["nodename"], "fixture-a");
                assert_eq!(host["identity"]["release"], "new-kernel", "{format}");
                assert_eq!(host["identity"]["cpu_nr"], 4, "{format}");
            }
        }
    }
}

#[test]
fn native_help_explains_the_time_basis() {
    for command in ["show", "summarize", "compare", "detect"] {
        let out = Command::new(env!("CARGO_BIN_EXE_resarch"))
            .args([command, "--help"])
            .output()
            .unwrap();
        assert!(out.status.success());
        let help = String::from_utf8_lossy(&out.stdout);
        // 既定がローカルであることと、戻す手段の両方が読めること。
        assert!(help.contains("--timezone"), "{command}: {help}");
        assert!(help.contains("--utc"), "{command}: {help}");
        assert!(help.contains("local"), "{command}: {help}");
    }
}

#[test]
fn daily_time_windows_include_each_day_in_every_native_format() {
    let dir = tempfile::tempdir().unwrap();
    let mut spec = FixtureSpec::skeleton(Generation::G2175Current, FixtureAbi::Le64);
    spec.activities = vec![ActivitySpec::a_pcsw()];
    let midnight = 1_600_000_000 / 86_400 * 86_400;
    spec.records = (0..96)
        .map(|i| {
            let epoch = midnight + i * 1800;
            let mut rec = RecordSpec::stats(
                vec![1],
                epoch,
                ((i / 2) % 24) as u8,
                ((i % 2) * 30) as u8,
                0,
            );
            rec.uptime = 100_000 + i * 180_000;
            rec
        })
        .collect();
    let file = save(dir.path(), "two-days.sa", &build(spec).bytes);
    let out = run(
        &[
            "show", "--format", "json", "--from", "09:00", "--to", "11:00",
        ],
        &file,
    );
    assert!(out.status.success());
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let samples = json["samples"].as_array().unwrap();
    assert_eq!(samples.len(), 8);
    assert_eq!(samples[4]["end_epoch"], midnight + 86_400 + 9 * 3600 + 1800);
    for format in ["table", "csv", "ndjson"] {
        let out = run(
            &[
                "show", "--format", format, "--from", "09:00", "--to", "11:00",
            ],
            &file,
        );
        assert!(out.status.success(), "{format}");
        let text = String::from_utf8(out.stdout).unwrap();
        if format == "table" {
            assert_eq!(
                text.lines()
                    .filter(|line| line.split_whitespace().next() == Some("09:30:00"))
                    .count(),
                2
            );
        } else {
            assert!(
                text.contains(&(midnight + 86_400 + 9 * 3600 + 1800).to_string()),
                "{format}"
            );
        }
    }
    let out = run(
        &[
            "summarize",
            "--format",
            "json",
            "--from",
            "09:00",
            "--to",
            "11:00",
        ],
        &file,
    );
    assert!(out.status.success());
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let period = &json["hosts"][0]["segments"][0]["summary"]["period"];
    assert_eq!(period["samples"], 8);
    assert_eq!(period["covered_cs"], 2 * 2 * 3600 * 100);
}

#[cfg(unix)]
#[test]
fn native_local_timezone_honors_posix_tz() {
    let dir = tempfile::tempdir().unwrap();
    let file = save(dir.path(), "timezone.sa", &source(0, false));
    let utc = first_cell_in_tz("UTC", &[], &file);
    assert_eq!(first_cell_in_tz("UTC0", &[], &file), utc);
    assert_eq!(first_cell_in_tz("", &[], &file), utc);
    assert_eq!(
        first_cell_in_tz("<+03>-3", &[], &file),
        shifted(&utc, 3 * 3600)
    );
}

#[test]
fn repeated_dst_windows_do_not_join_across_unobserved_outside_time() {
    let dir = tempfile::tempdir().unwrap();
    let mut spec = FixtureSpec::skeleton(Generation::G2175Current, FixtureAbi::Le64);
    spec.activities = vec![ActivitySpec::a_pcsw()];
    spec.records = [1_793_511_600, 1_793_515_200]
        .into_iter()
        .enumerate()
        .map(|(i, epoch)| {
            let mut rec = RecordSpec::stats(vec![1], epoch, 1, 40, 0);
            rec.uptime = 100_000 + i as u64 * 360_000;
            rec
        })
        .collect();
    let file = save(dir.path(), "repeated-window.sa", &build(spec).bytes);
    let options = [
        "--format",
        "json",
        "--timezone",
        "America/New_York",
        "--from",
        "01:30",
        "--to",
        "01:45",
    ];
    let mut args = vec!["show"];
    args.extend_from_slice(&options);
    let out = run(&args, &file);
    assert!(out.status.success());
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(json["samples"].as_array().unwrap().is_empty());
    args[0] = "summarize";
    let out = run(&args, &file);
    assert!(out.status.success());
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    for host in json["hosts"].as_array().unwrap() {
        for segment in host["segments"].as_array().unwrap() {
            assert_eq!(segment["summary"]["period"]["covered_cs"], 0);
        }
    }
}

#[test]
fn unsupported_summary_and_comparison_formats_fail() {
    let dir = tempfile::tempdir().unwrap();
    let file = save(dir.path(), "formats.sa", &source(0, false));
    for format in [
        "csv",
        "sar",
        "sadf-json",
        "sadf-xml",
        "sadf-d",
        "sadf-p",
        "sadf-raw",
    ] {
        let out = run(&["summarize", "--format", format], &file);
        assert!(!out.status.success(), "summarize {format}");
        assert!(out.stdout.is_empty());
        let out = Command::new(env!("CARGO_BIN_EXE_resarch"))
            .args(["compare", "--format", format, "--host"])
            .arg(format!("a={}", file.display()))
            .arg("--host")
            .arg(format!("b={}", file.display()))
            .output()
            .unwrap();
        assert!(!out.status.success(), "compare {format}");
        assert!(out.stdout.is_empty());
    }
}

#[test]
fn show_compatibility_formats_filter_in_their_display_timezone() {
    let dir = tempfile::tempdir().unwrap();
    let file = save(dir.path(), "compat-time.sa", &source(0, false));
    // source の先頭は UTC 19:50。sadf の表示・絞り込みは UTC、sar はローカル時刻。
    for format in [
        "sar",
        "sadf-d",
        "sadf-json",
        "sadf-xml",
        "sadf-p",
        "sadf-raw",
    ] {
        let (from, to, expected) = if format == "sar" {
            ("05:00", "05:10", "05:10:00")
        } else {
            ("20:00", "20:10", "20:10:00")
        };
        let out = run_in_tz(
            "Asia/Tokyo",
            &[
                "show",
                "--activity",
                "cpu",
                "--format",
                format,
                "--from",
                from,
                "--to",
                to,
            ],
            &file,
        );
        assert!(out.status.success(), "{format}");
        assert!(
            String::from_utf8_lossy(&out.stdout).contains(expected),
            "{format}"
        );
        for timezone in ["--utc", "--timezone"] {
            let mut args = vec!["show", "--format", format, timezone];
            if timezone == "--timezone" {
                args.push("utc");
            }
            let out = run(&args, &file);
            assert!(!out.status.success(), "{format} {timezone}");
            assert!(out.stdout.is_empty());
        }
    }
}

/// 独自出力の時刻は実行環境のローカルタイムゾーンで出る。
#[test]
fn native_output_uses_the_local_timezone_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let file = save(dir.path(), "tz.sa", &source(0, false));

    let out = run_in_tz("Asia/Tokyo", &["show", "--activity", "cpu"], &file);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("(time は Asia/Tokyo)"), "{text}");
    // 同じ行が、UTC で出したときよりちょうど 9 時間進む。
    let utc_cell = first_cell_in_tz("UTC", &[], &file);
    assert_eq!(first_time_cell(&text), shifted(&utc_cell, 9 * 3600));
    assert_ne!(first_time_cell(&text), utc_cell);

    let summarized = run_in_tz("Asia/Tokyo", &["summarize", "--activity", "cpu"], &file);
    let text = String::from_utf8(summarized.stdout).unwrap();
    assert!(text.contains("+09:00"), "オフセットを添える: {text}");
    assert!(!text.contains("Z "), "UTC の Z が残っている: {text}");
}

/// `--timezone` / `--utc` は実行環境のタイムゾーンより優先される。
#[test]
fn the_timezone_option_overrides_the_environment() {
    let dir = tempfile::tempdir().unwrap();
    let file = save(dir.path(), "tz.sa", &source(0, false));

    for args in [
        vec!["show", "--activity", "cpu", "--utc"],
        vec!["show", "--activity", "cpu", "--timezone", "utc"],
    ] {
        let out = run_in_tz("Asia/Tokyo", &args, &file);
        let text = String::from_utf8(out.stdout).unwrap();
        assert!(text.contains("(time は UTC)"), "{args:?}: {text}");
        // 実行環境が JST でも、UTC 起動と同じ時刻になる。
        assert_eq!(
            first_time_cell(&text),
            first_cell_in_tz("UTC", &[], &file),
            "{args:?}"
        );
    }

    // IANA 名の指定も効く (実行環境とも UTC とも違う基準)
    let out = run_in_tz(
        "Asia/Tokyo",
        &["show", "--activity", "cpu", "--timezone", "Asia/Kathmandu"],
        &file,
    );
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("(time は Asia/Kathmandu)"), "{text}");
    // +05:45 なので、30 分刻みでないオフセットでも崩れない
    let cell = first_time_cell(&text);
    let utc_cell = first_cell_in_tz("UTC", &[], &file);
    assert_eq!(cell, shifted(&utc_cell, 5 * 3600 + 45 * 60), "{text}");
}

/// `--timezone` と `--utc` の同時指定は弾く。
#[test]
fn timezone_and_utc_conflict() {
    let dir = tempfile::tempdir().unwrap();
    let file = save(dir.path(), "tz.sa", &source(0, false));
    let out = run(
        &["show", "--activity", "cpu", "--timezone", "utc", "--utc"],
        &file,
    );
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--utc"), "{err}");
}

/// 知らないタイムゾーン名は、受け付ける形を示して弾く。
#[test]
fn an_unknown_timezone_is_rejected_with_the_accepted_forms() {
    let dir = tempfile::tempdir().unwrap();
    let file = save(dir.path(), "tz.sa", &source(0, false));
    let out = run(
        &["show", "--activity", "cpu", "--timezone", "Asia/Nowhere"],
        &file,
    );
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("Asia/Nowhere"), "{err}");
    assert!(err.contains("IANA"), "{err}");
}

/// 日跨ぎフィルタは**表示と同じ基準**で切る。
///
/// 同じ `--from 20:00 --to 02:00` でも、実行環境が JST なら JST の 20:00〜02:00
/// を指す。画面に出ている時刻と `--from` が食い違わないことがこの検証の眼目。
#[test]
fn the_overnight_filter_follows_the_display_timezone() {
    let dir = tempfile::tempdir().unwrap();
    let file = save(dir.path(), "overnight.sa", &source(0, true));
    let args = [
        "show",
        "--activity",
        "pcsw",
        "--format",
        "ndjson",
        "--from",
        "20:00",
        "--to",
        "02:00",
    ];

    let epochs = |out: Output| -> Vec<u64> {
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
            .map(|v| v["end_epoch"].as_u64().unwrap())
            .collect()
    };

    let utc = epochs(run_in_tz("UTC", &args, &file));
    let jst = epochs(run_in_tz("Asia/Tokyo", &args, &file));

    assert!(!utc.is_empty() && !jst.is_empty());
    // 基準が違えば選ばれる区間も違う (同じなら検証になっていない)
    assert_ne!(utc, jst);

    for e in &utc {
        let sod = e % 86_400;
        assert!(
            sod >= 20 * 3600 || sod <= 2 * 3600,
            "UTC 基準から外れた: {e}"
        );
    }
    for e in &jst {
        let sod = jst_seconds_of_day(*e);
        assert!(
            sod >= 20 * 3600 || sod <= 2 * 3600,
            "JST 基準から外れた: {e}"
        );
    }
}

/// 機械可読形式の epoch 秒はタイムゾーンで動かない。
#[test]
fn machine_readable_epochs_do_not_move_with_the_timezone() {
    let dir = tempfile::tempdir().unwrap();
    let file = save(dir.path(), "epochs.sa", &source(0, false));
    let args = ["show", "--activity", "cpu", "--format", "ndjson"];

    let rows = |out: Output| -> Vec<String> {
        assert!(out.status.success());
        String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .map(String::from)
            .collect()
    };
    let utc = rows(run_in_tz("UTC", &args, &file));
    let jst = rows(run_in_tz("Asia/Tokyo", &args, &file));
    let honolulu = rows(run_in_tz("Pacific/Honolulu", &args, &file));

    assert!(!utc.is_empty());
    // `file_date` はローカルの日付で開くため行そのものは一致しないが、
    // 時点を表す epoch 秒は 1 つも動かない。
    let epochs = |rows: &[String]| -> Vec<(u64, u64)> {
        rows.iter()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter_map(|v| Some((v["start_epoch"].as_u64()?, v["end_epoch"].as_u64()?)))
            .collect()
    };
    assert_eq!(epochs(&utc), epochs(&jst));
    assert_eq!(epochs(&utc), epochs(&honolulu));
}

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

//! sa → sar の CLI・ファイル保存と本家期待出力との比較。
mod fixtures;
mod golden;

use std::path::{Path, PathBuf};
use std::process::Command;

use fixtures::{FixtureAbi, FixtureSpec, Generation, build};

fn command() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_resarch"));
    cmd.env("LC_ALL", "C").env("TZ", "Pacific/Honolulu");
    cmd
}

fn source(dir: &Path) -> PathBuf {
    let mut spec = FixtureSpec::minimal(Generation::G2175Current, FixtureAbi::Le64);
    spec.tzname = "JST".into();
    for record in &mut spec.records {
        record.hour = (record.hour + 9) % 24;
    }
    let path = dir.join("sa13");
    std::fs::write(&path, build(spec).bytes).unwrap();
    path
}

#[test]
fn file_and_stdout_include_all_sections_and_recorded_time() {
    let dir = tempfile::tempdir().unwrap();
    let input = source(dir.path());
    let output = dir.path().join("sar13");
    let saved = command()
        .arg("sa2sar")
        .arg(&input)
        .arg("-o")
        .arg(&output)
        .output()
        .unwrap();
    assert!(saved.status.success(), "{:?}", saved);
    assert!(saved.stdout.is_empty());
    let actual = std::fs::read(&output).unwrap();
    for args in [
        vec!["sa2sar"],
        vec!["sa2sar", "-o", "-"],
        vec!["sa2sar", "--no-mmap"],
    ] {
        let stdout = command().args(args).arg(&input).output().unwrap();
        assert!(stdout.status.success(), "{:?}", stdout);
        assert_eq!(stdout.stdout, actual);
    }
    let sar = command()
        .args(["sar", "-A", "-C", "-t", "-f"])
        .arg(&input)
        .output()
        .unwrap();
    assert!(sar.status.success());
    assert_eq!(actual, sar.stdout);
    let text = String::from_utf8(actual).unwrap();
    for part in [
        "21:27:11",
        "Average:",
        "LINUX RESTART",
        "resarch fixture",
        "proc/s",
        "%guest",
        "CPU",
    ] {
        assert!(text.contains(part), "missing {part}: {text}");
    }
}

#[test]
fn utc_is_independent_of_the_execution_timezone() {
    let dir = tempfile::tempdir().unwrap();
    let input = source(dir.path());
    let mut previous = None;
    for tz in ["UTC", "Asia/Tokyo", "Pacific/Honolulu"] {
        let result = command()
            .args(["sa2sar", "--utc"])
            .arg(&input)
            .env("TZ", tz)
            .output()
            .unwrap();
        assert!(result.status.success());
        assert!(String::from_utf8_lossy(&result.stdout).contains("12:27:11"));
        if let Some(prev) = previous {
            assert_eq!(prev, result.stdout);
        }
        previous = Some(result.stdout);
    }
}

#[test]
fn existing_destination_and_input_are_preserved() {
    let dir = tempfile::tempdir().unwrap();
    let input = source(dir.path());
    let existing = dir.path().join("sar13");
    std::fs::write(&existing, b"keep this report").unwrap();
    let alias = dir.path().join("input-hardlink");
    std::fs::hard_link(&input, &alias).unwrap();
    for output in [&existing, &input, &alias] {
        let before = std::fs::read(output).unwrap();
        let result = command()
            .arg("sa2sar")
            .arg(&input)
            .arg("-o")
            .arg(output)
            .output()
            .unwrap();
        assert!(!result.status.success());
        assert!(result.stdout.is_empty());
        assert_eq!(std::fs::read(output).unwrap(), before);
    }
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 3);
}

#[cfg(unix)]
#[test]
fn symlink_destination_is_not_followed() {
    let dir = tempfile::tempdir().unwrap();
    let input = source(dir.path());
    let link = dir.path().join("sar13");
    std::os::unix::fs::symlink(&input, &link).unwrap();
    let before = std::fs::read(&input).unwrap();
    let result = command()
        .arg("sa2sar")
        .arg(&input)
        .arg("-o")
        .arg(&link)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(std::fs::read(&input).unwrap(), before);
}

#[test]
fn corrupt_input_leaves_no_report_or_temporary_file() {
    let dir = tempfile::tempdir().unwrap();
    let input = source(dir.path());
    let mut bytes = std::fs::read(&input).unwrap();
    bytes.pop();
    std::fs::write(&input, bytes).unwrap();
    let output = dir.path().join("sar13");
    let result = command()
        .arg("sa2sar")
        .arg(&input)
        .arg("-o")
        .arg(&output)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    assert!(!result.stderr.is_empty());
    assert!(!output.exists());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn help_and_missing_input() {
    let help = command().args(["sa2sar", "--help"]).output().unwrap();
    assert!(help.status.success());
    let text = String::from_utf8(help.stdout).unwrap();
    assert!(text.contains("--output") && text.contains("--utc"));
    assert!(!command().arg("sa2sar").output().unwrap().status.success());
}

#[test]
#[ignore = "make fixtures 後に実行。取得漏れはスキップせず失敗する"]
fn generated_sar_files_match_upstream_golden() {
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("target"));
    let upstream = target.join("fixtures/upstream");
    assert!(
        upstream.join("PROVENANCE.txt").is_file(),
        "run make fixtures first"
    );
    let dir = tempfile::tempdir().unwrap();
    // 旧世代 3 本、現行、big-endian。デバイス名以外の値・時刻・空白は全て比較する。
    for (name, mask_disk) in [
        ("data-9.1.6", true),
        ("data-10.3.1", true),
        ("data-11.6.5", true),
        ("data-12.0.0", true),
        ("data-ppc-11.7.2", false),
    ] {
        let output = dir.path().join(format!("{name}.sar"));
        let result = command()
            .args(["sa2sar", "--utc"])
            .arg(upstream.join(name))
            .arg("-o")
            .arg(&output)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(result.stdout.is_empty());
        let actual = std::fs::read_to_string(output).unwrap();
        let expected = std::fs::read_to_string(upstream.join(format!("expected.{name}"))).unwrap();
        assert!(!expected.is_empty());
        let masks = if mask_disk {
            &[golden::Mask::DiskDeviceName][..]
        } else {
            &[]
        };
        let comparison = golden::compare(&expected, &actual, masks);
        eprintln!("{name}: {}", comparison.verdict());
        eprint!("{}", comparison.mask_report());
        assert!(
            comparison.is_match(),
            "{name}: {}",
            comparison.diff_report(8)
        );
        if !mask_disk {
            assert_eq!(expected, actual, "{name}: byte-for-byte comparison");
        }
    }
}

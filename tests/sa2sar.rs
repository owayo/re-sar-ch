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

/// 表示サンプルが複数ある fixture (ヘッダ再表示の確認用)。
///
/// [`source`] は統計レコードが 2 件しかなく、差分の基準に 1 件使うので
/// 表示は 1 サンプルだけになる。再表示の間隔を見るには足りない。
fn multi_sample_source(dir: &Path) -> PathBuf {
    let mut spec = FixtureSpec::minimal(Generation::G2175Current, FixtureAbi::Le64);
    spec.tzname = "JST".into();
    let cpu_nr = spec.cpu_nr as i32;
    // 6 レコード = 先頭が基準、残り 5 件が表示される。
    spec.records = (0u8..6)
        .map(|i| {
            fixtures::RecordSpec::stats(
                vec![cpu_nr, 0],
                1_600_000_011 + u64::from(i) * 10,
                12,
                27,
                11 + i,
            )
        })
        .collect();
    let path = dir.join("sa14");
    std::fs::write(&path, build(spec).bytes).unwrap();
    path
}

fn run(input: &Path, env: &[(&str, &str)]) -> String {
    let mut cmd = command();
    for (key, value) in env {
        cmd.env(key, value);
    }
    let result = cmd.arg("sa2sar").arg(input).output().unwrap();
    assert!(
        result.status.success(),
        "{env:?}: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout).unwrap()
}

/// `S_TIME_FORMAT=ISO` はバナー行の日付だけを ISO 8601 に変える (03 §6.1)。
///
/// 本家 `print_gal_header()` は `S_TIME_FORMAT` が厳密に `ISO` のときだけ
/// `%Y-%m-%d` を使う。時刻側は本家では `%X` → `%H:%M:%S` に変わるが、
/// reSARch はロケールを持たず常に `%H:%M:%S` なので差が出ない。
#[test]
fn iso_time_format_changes_only_the_banner_date() {
    let dir = tempfile::tempdir().unwrap();
    let input = source(dir.path());

    let default = run(&input, &[]);
    let iso = run(&input, &[("S_TIME_FORMAT", "ISO")]);
    assert_ne!(default, iso, "ISO 指定で出力が変わる");

    // 差があるのは 1 行目 (バナー) だけ。
    let rest = |text: &str| text.lines().skip(1).collect::<Vec<_>>().join("\n");
    assert_eq!(
        rest(&default),
        rest(&iso),
        "バナー以外は 1 文字も変わらない"
    );

    // バナーの日付欄はタブ区切りの 2 番目。
    let banner_date = |text: &str| {
        text.lines()
            .next()
            .unwrap()
            .split('\t')
            .nth(1)
            .unwrap()
            .trim()
            .to_string()
    };
    let default_date = banner_date(&default);
    let iso_date = banner_date(&iso);
    assert_eq!(default_date.len(), 8, "既定は MM/DD/YY: {default_date:?}");
    assert_eq!(default_date.matches('/').count(), 2, "{default_date:?}");
    assert_eq!(iso_date.len(), 10, "ISO は YYYY-MM-DD: {iso_date:?}");
    assert_eq!(iso_date.matches('-').count(), 2, "{iso_date:?}");

    // 本家は strcmp なので、綴りが違えば効かない。
    for value in ["iso", "Iso", "ISO8601", "", " ISO"] {
        assert_eq!(
            run(&input, &[("S_TIME_FORMAT", value)]),
            default,
            "S_TIME_FORMAT={value:?} は効かない"
        );
    }
}

/// `S_REPEAT_HEADER` は標準出力が端末でないときに列見出しを繰り返す (03 §9.3)。
///
/// テストの標準出力はパイプなので `ioctl` は失敗し、本家と同じく
/// 環境変数だけが見られる経路になる。
#[test]
fn repeat_header_reprints_column_headers() {
    let dir = tempfile::tempdir().unwrap();
    let input = multi_sample_source(dir.path());

    // `A_PCSW` は 1 サンプル 1 行なので、行数と再表示の関係が読みやすい。
    let headers = |text: &str| text.lines().filter(|l| l.contains("proc/s")).count();
    let samples = |text: &str| {
        text.lines()
            .filter(|l| l.starts_with("12:27:") || l.starts_with("21:27:"))
            .count()
    };

    let default = run(&input, &[]);
    assert_eq!(headers(&default), 1, "既定はブロック先頭の 1 回だけ");
    assert!(samples(&default) > 1, "表示サンプルが複数ある前提");

    // 1 行ごとに再表示 = 表示サンプル数だけヘッダが出る。
    // さらに本家は `dish` を平均ブロックへ持ち越すので 1 回増える。
    let every_line = run(&input, &[("S_REPEAT_HEADER", "1")]);
    assert!(
        headers(&every_line) > headers(&default),
        "S_REPEAT_HEADER=1 でヘッダが増える: {} → {}",
        headers(&default),
        headers(&every_line)
    );

    // 値が大きければ届かないので既定と同じ。
    assert_eq!(
        run(&input, &[("S_REPEAT_HEADER", "100000")]),
        default,
        "閾値に届かなければ再表示しない"
    );

    // 全桁数字かつ > 0 以外は無視される (本家の strspn + atoi)。
    for value in ["0", "-1", "+1", " 1", "1 ", "1a", "", "0x1"] {
        assert_eq!(
            run(&input, &[("S_REPEAT_HEADER", value)]),
            default,
            "S_REPEAT_HEADER={value:?} は無視される"
        );
    }
}

/// `-o <file>` の宛先はファイルなので、端末の高さではなく
/// `S_REPEAT_HEADER` が見られる (03 §9.3)。
///
/// 標準出力と `-o` で同じ内容になることは
/// [`file_and_stdout_include_all_sections_and_recorded_time`] が見ているが、
/// そちらは環境変数を与えない。ここは再表示を有効にしたまま両者を比べる。
#[test]
fn repeat_header_applies_to_the_output_file_too() {
    let dir = tempfile::tempdir().unwrap();
    let input = multi_sample_source(dir.path());
    let output = dir.path().join("sar14");

    let saved = command()
        .env("S_REPEAT_HEADER", "1")
        .args(["sa2sar"])
        .arg(&input)
        .arg("-o")
        .arg(&output)
        .output()
        .unwrap();
    assert!(saved.status.success(), "{:?}", saved);

    let from_file = std::fs::read_to_string(&output).unwrap();
    let from_stdout = run(&input, &[("S_REPEAT_HEADER", "1")]);
    assert_eq!(from_file, from_stdout, "宛先が変わっても内容は同じ");
    assert!(
        from_file.lines().filter(|l| l.contains("proc/s")).count() > 1,
        "S_REPEAT_HEADER がファイル出力にも効く"
    );
}

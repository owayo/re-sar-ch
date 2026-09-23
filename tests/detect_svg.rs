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

/// 言語を決める環境変数を外してから `envs` だけを与えて実行する。
///
/// 手元の `LANG` などが混ざると、`--lang` を付けない場合の結果が
/// 実行する環境によって変わる。
fn run_in_language(input: &Path, dir: &Path, extra: &[&str], envs: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_resarch"));
    command
        .arg("detect")
        .arg(input)
        .args(["--format", "json", "--svg-dir"])
        .arg(dir)
        .args(["--svg-context", "5m"])
        .args(extra)
        .env("TZ", "Pacific/Honolulu");
    for key in ["RESARCH_LANG", "LC_ALL", "LC_MESSAGES", "LANG", "LANGUAGE"] {
        command.env_remove(key);
    }
    command.envs(envs.iter().copied()).output().unwrap()
}

/// 保存された図をすべて読む。
fn saved_svgs(dir: &Path) -> Vec<(String, String)> {
    let manifest = index(dir);
    let charts = manifest["charts"].as_array().unwrap();
    assert!(!charts.is_empty(), "{manifest:#}");
    charts
        .iter()
        .map(|chart| {
            let file = chart["file"].as_str().unwrap().to_string();
            let svg = std::fs::read_to_string(dir.join(&file)).unwrap();
            (file, svg)
        })
        .collect()
}

/// `open` から最初の `close` までの中身。
fn between<'a>(svg: &'a str, open: &str, close: &str) -> &'a str {
    let start = svg.find(open).unwrap_or_else(|| panic!("{open} が無い")) + open.len();
    let end = svg[start..].find(close).unwrap() + start;
    &svg[start..end]
}

/// `class="{class}"` の `<text>` の中身をすべて。
fn texts_of<'a>(svg: &'a str, class: &str) -> Vec<&'a str> {
    let open = format!("class=\"{class}\">");
    svg.match_indices(&open)
        .map(|(at, _)| {
            let rest = &svg[at + open.len()..];
            &rest[..rest.find("</text>").unwrap()]
        })
        .collect()
}

/// 日本語の文字 (全角の記号・かな・漢字・全角英数) を含むか。
fn has_japanese(text: &str) -> bool {
    text.chars().any(|c| {
        matches!(
            c,
            '\u{3000}'..='\u{30ff}'
                | '\u{3400}'..='\u{4dbf}'
                | '\u{4e00}'..='\u{9fff}'
                | '\u{ff00}'..='\u{ffef}'
        )
    })
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
        // 既定は実行環境のタイムゾーン。図は単体で共有されるので、
        // どの基準の時刻かを図の中に必ず書く。
        assert!(
            svg.contains("Pacific/Honolulu"),
            "図に基準のタイムゾーンが無い"
        );
        assert!(!svg.contains("NaN") && !svg.contains("Infinity"));
    }
    // 既定の基準は index.json にも入る。
    assert_eq!(manifest["timezone"], "Pacific/Honolulu");
}

/// `--timezone utc` / `--utc` は実行環境のタイムゾーンより優先される。
#[test]
fn the_timezone_flag_overrides_the_environment() {
    let temp = tempfile::tempdir().unwrap();
    let input = source(temp.path(), "tzhost", true);
    for flag in [vec!["--timezone", "utc"], vec!["--utc"]] {
        let dir = temp.path().join(format!("charts-{}", flag.len()));
        let out = run(&input, &dir, &flag);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let manifest = index(&dir);
        assert_eq!(manifest["timezone"], "UTC", "{flag:?}");
        for chart in manifest["charts"].as_array().unwrap() {
            let file = chart["file"].as_str().unwrap();
            let svg = std::fs::read_to_string(dir.join(file)).unwrap();
            assert!(svg.contains("UTC"), "{flag:?}: {file}");
            assert!(!svg.contains("Pacific/Honolulu"), "{flag:?}: {file}");
        }
    }
}

/// `--timezone` と `--utc` の同時指定は弾く (どちらが勝つか曖昧にしない)。
#[test]
fn timezone_and_utc_cannot_be_combined() {
    let temp = tempfile::tempdir().unwrap();
    let input = source(temp.path(), "tzhost", true);
    let out = run(
        &input,
        &temp.path().join("charts-conflict"),
        &["--timezone", "utc", "--utc"],
    );
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--utc"), "{err}");
}

/// 図の文は報告の言語に従う。`--lang` で決めても、環境から決めても同じ。
///
/// 英語の報告に日本語の図が付くと、図だけを切り出して共有したときに読めない。
/// 図の言語宣言 (`lang`) と、title・desc・注記 (範囲・凡例)・線の説明を確かめる。
#[test]
fn chart_text_follows_the_report_language() {
    /// 言語の決め方 1 通り。
    struct Case {
        name: &'static str,
        flags: &'static [&'static str],
        envs: &'static [(&'static str, &'static str)],
        lang: &'static str,
    }
    let cases = [
        Case {
            name: "flag-en",
            flags: &["--lang", "en"],
            envs: &[],
            lang: "en",
        },
        Case {
            name: "flag-ja",
            flags: &["--lang", "ja"],
            envs: &[],
            lang: "ja",
        },
        // `C` ロケールは「決定的な英語」の指定で、`LANGUAGE` にも負けない
        Case {
            name: "c-locale",
            flags: &[],
            envs: &[("LC_ALL", "C"), ("LANGUAGE", "ja")],
            lang: "en",
        },
        Case {
            name: "app-env",
            flags: &[],
            envs: &[("RESARCH_LANG", "ja")],
            lang: "ja",
        },
    ];
    let temp = tempfile::tempdir().unwrap();
    let input = source(temp.path(), "chart<&host", true);
    for Case {
        name,
        flags,
        envs,
        lang,
    } in cases
    {
        let dir = temp.path().join(name);
        let out = run_in_language(&input, &dir, flags, envs);
        assert!(
            out.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        for (file, svg) in saved_svgs(&dir) {
            let at = format!("{name} {file}");
            let root = between(&svg, "<svg ", ">");
            assert!(root.contains(&format!(" lang=\"{lang}\"")), "{at}: {root}");
            assert!(
                root.contains(&format!(" xml:lang=\"{lang}\"")),
                "{at}: {root}"
            );
            // 文を差し替えてもエスケープは保たれる
            assert!(!svg.contains("chart<&host"), "{at}");
            // title はホスト名と系列の識別子で、言語に依らない
            let title = between(&svg, "<title id=\"chart-title\">", "</title>");
            assert!(title.starts_with("chart&lt;&amp;host — "), "{at}: {title}");
            assert!(!has_japanese(title), "{at}: {title}");

            let desc = between(&svg, "<desc id=\"chart-description\">", "</desc>");
            // 英語は語の切れ目で折り返すので、空白で継げば 1 文に戻る
            let meta = texts_of(&svg, "meta").join(" ");
            let note = texts_of(&svg, "note").concat();
            let (desc_says, meta_says, legend_says, note_says) = if lang == "en" {
                (
                    "This is neither a probability nor a determination of the cause.",
                    "s before and after (clipped at the edges of the input and the boot segment)",
                    "Orange: range the detection rests on / Blue dots: sampled values / \
                     Green dashes:",
                    "Lines are visual aids and never join across missing samples",
                )
            } else {
                (
                    "確率や原因の断定ではない。",
                    " 秒 (入力・起動区間の端で制限)",
                    "橙: 検知を裏付けた採取の範囲　青点: 採取値　緑破線:",
                    "線は補助線。欠測・非隣接・不連続をまたいで結びません。",
                )
            };
            assert!(desc.contains(desc_says), "{at}: {desc}");
            assert!(meta.contains(meta_says), "{at}: {meta}");
            assert!(meta.contains(legend_says), "{at}: {meta}");
            assert!(note.contains(note_says), "{at}: {note}");
            for text in [desc, &meta, &note] {
                assert_eq!(has_japanese(text), lang == "ja", "{at}: {text}");
            }
        }
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
    // 図の文は言語ごとに違うので、両方の言語で確かめる
    for lang in ["ja", "en"] {
        let dir = temp.path().join(format!("charts-{lang}"));
        let result = run(&input, &dir, &["--svg-context", "5m", "--lang", lang]);
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
                "{lang} {file}: {}",
                String::from_utf8_lossy(&xml.stderr)
            );
            if let Some(preview) = std::env::var_os("RESARCH_SVG_TEST_OUTPUT") {
                let preview = Path::new(&preview).join(lang);
                std::fs::create_dir_all(&preview).unwrap();
                std::fs::copy(dir.join(file), preview.join(file)).unwrap();
            }
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

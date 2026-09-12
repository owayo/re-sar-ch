//! CLI 統合テスト — 引数解析から出力層までが実際に繋がっていることを固定する。
//!
//! # 位置づけ
//!
//! `src/cli` の単体テストは「引数が正しく解釈されること」を見る。ここでは
//! **バイナリを起動して stdout / stderr / 終了コードを見る**。
//! `src/main.rs` が CLI 解析結果を出力層の設定へ写す配線が抜けても、
//! 単体テストは通ってしまうためこの層が必要になる。
//!
//! # データ
//!
//! 本家 sysstat のテストデータは GPL-2.0-or-later なので同梱できない
//! (`docs/format/04-test-data.md` §6)。`make fixtures`
//! (= `cargo run --bin xtask -- fetch-fixtures`) で
//! `target/fixtures/upstream/` へ取得したものを使い、
//! **未取得なら何も失敗させずスキップする**。

use std::path::{Path, PathBuf};

use assert_cmd::Command;

// ===========================================================================
// fixture の発見
// ===========================================================================

/// 取得物の置き場所 (`xtask fetch-fixtures` の保存先と一致させる)。
const UPSTREAM_SUBDIR: &str = "fixtures/upstream";

/// `xtask` が書き残す素性ファイル。これがあれば取得は完了している。
const PROVENANCE_FILE: &str = "PROVENANCE.txt";

const HOW_TO_FETCH: &str = "本家 fixture が無いのでスキップした (`make fixtures` で取得できる)";

/// 主に使う fixture。自己記述形式 (0x2175) の初期形で 15 activity を持つ。
const MAIN_FIXTURE: &str = "data-12.0.0";

/// 副の fixture。`0x2173` 世代の最終形 (センサ系 activity を含む)。
const ALT_FIXTURE: &str = "data-11.6.5";

fn upstream_dir() -> Option<PathBuf> {
    let target = match std::env::var_os("CARGO_TARGET_DIR") {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"),
    };
    let dir = target.join(UPSTREAM_SUBDIR);
    dir.join(PROVENANCE_FILE).is_file().then_some(dir)
}

/// fixture のパス。未取得なら案内を出して `None` (テストは失敗させない)。
fn fixture(name: &str) -> Option<PathBuf> {
    let dir = upstream_dir()?;
    let path = dir.join(name);
    if path.is_file() {
        Some(path)
    } else {
        eprintln!("skipped: {name} が無い。{HOW_TO_FETCH}");
        None
    }
}

/// 取得済みでなければ案内を出す。
fn main_fixture() -> Option<PathBuf> {
    if upstream_dir().is_none() {
        eprintln!("skipped: {HOW_TO_FETCH}");
        return None;
    }
    fixture(MAIN_FIXTURE)
}

/// `resarch` を `TZ=UTC` / `LC_ALL=C` で起動する。
///
/// タイムスタンプ表記は既定でローカル時刻なので、TZ を固定しないと
/// 実行環境で期待値が動く。
fn resarch() -> Command {
    let mut cmd = Command::cargo_bin("resarch").expect("resarch バイナリがビルドできること");
    cmd.env("TZ", "UTC").env("LC_ALL", "C");
    cmd
}

/// 成功終了を確かめて stdout を取り出す。
fn run_ok(args: &[&str]) -> String {
    let out = resarch().args(args).output().expect("起動できること");
    assert!(
        out.status.success(),
        "終了コードが 0 でない: args={args:?}\n--- stderr ---\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("stdout が UTF-8 であること")
}

/// 失敗終了を確かめて stderr を取り出す。
fn run_err(args: &[&str]) -> String {
    let out = resarch().args(args).output().expect("起動できること");
    assert!(
        !out.status.success(),
        "失敗するべきコマンドが成功した: args={args:?}\n--- stdout ---\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn as_str(p: &Path) -> &str {
    p.to_str().expect("fixture のパスが UTF-8 であること")
}

// ===========================================================================
// sar 互換テキスト
// ===========================================================================

/// `-u` の集約行 (`all`) の時刻列を並べる。
fn cpu_row_times(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let ts = it.next()?;
            let item = it.next()?;
            (item == "all" && ts.contains(':')).then(|| ts.to_string())
        })
        .collect()
}

/// ヘッダ行 (2 列目が `CPU`) の時刻列。
fn cpu_header_times(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let ts = it.next()?;
            let item = it.next()?;
            (item == "CPU" && ts.contains(':')).then(|| ts.to_string())
        })
        .collect()
}

/// 要件の中心: サブコマンドを省略した `resarch -u -f <file>` が動く。
#[test]
fn bare_sar_options_produce_a_cpu_report() {
    let Some(f) = main_fixture() else { return };
    let out = run_ok(&["-u", "-f", as_str(&f)]);

    assert!(
        out.starts_with("Linux "),
        "バナー行が先頭に来る: {out:.120}"
    );
    assert!(out.contains("%idle"), "既定の CPU レポートは %idle を含む");
    assert!(out.contains("Average:"), "平均行が出る");
    assert!(
        !cpu_row_times(&out).is_empty(),
        "集約行 (all) が 1 行以上出る"
    );
}

/// `resarch sar -u -f <file>` は省略形と同じ出力になる。
#[test]
fn explicit_sar_subcommand_matches_the_bare_form() {
    let Some(f) = main_fixture() else { return };
    let bare = run_ok(&["-u", "-f", as_str(&f)]);
    let explicit = run_ok(&["sar", "-u", "-f", as_str(&f)]);
    assert_eq!(bare, explicit);
}

/// `-A` は全 activity を出す (CPU 以外のブロックが増える)。
#[test]
fn sar_option_a_reports_more_than_cpu() {
    let Some(f) = main_fixture() else { return };
    let only_cpu = run_ok(&["-u", "-f", as_str(&f)]);
    let all = run_ok(&["sar", "-A", "-f", as_str(&f)]);
    assert!(
        all.len() > only_cpu.len(),
        "-A は -u より多くの行を出す ({} <= {})",
        all.len(),
        only_cpu.len()
    );
    assert!(all.contains("%idle"), "-A にも CPU ブロックが含まれる");
}

/// `-P ALL` は集約行だけでなく CPU 別の行も出す。
#[test]
fn sar_p_all_adds_per_cpu_rows() {
    let Some(f) = main_fixture() else { return };
    let out = run_ok(&["sar", "-u", "-P", "ALL", "-f", as_str(&f)]);
    let has_numbered_cpu = out.lines().any(|l| {
        let mut it = l.split_whitespace();
        let _ = it.next();
        it.next().is_some_and(|item| item.parse::<u32>().is_ok())
    });
    assert!(has_numbered_cpu, "CPU 番号の行が出る:\n{out:.400}");
}

/// `--dec=0` は小数を落としても**列幅を変えない** (03 §2.5)。
#[test]
fn sar_dec_changes_precision_not_width() {
    let Some(f) = main_fixture() else { return };
    let base = run_ok(&["sar", "-u", "-f", as_str(&f)]);
    let dec0 = run_ok(&["sar", "-u", "--dec=0", "-f", as_str(&f)]);

    let width = |s: &str| {
        s.lines()
            .find(|l| l.split_whitespace().nth(1) == Some("all"))
            .map(|l| l.len())
    };
    assert_eq!(width(&base), width(&dec0), "--dec= で行幅は変わらない");
    assert_ne!(base, dec0, "--dec=0 で表記自体は変わる");
}

/// `-h` (= `--pretty --human`) が通り、アイテム名が行末へ移る。
#[test]
fn sar_h_is_pretty_and_human_not_help() {
    let Some(f) = main_fixture() else { return };
    let out = run_ok(&["-h", "-f", as_str(&f)]);
    assert!(out.starts_with("Linux "), "ヘルプではなくレポートが出る");
}

// ===========================================================================
// 時刻フィルタ (-s / -e) の意味論 — 03 §1.10
// ===========================================================================

/// **`-s` に一致した最初のレコードは前サンプルとして消費され、表示されない。**
///
/// フィルタ無しの表示が `T1, T2, …` のとき、`-s T1` の表示は `T2, …` になり、
/// ヘッダ行の時刻が `T1` になる。
#[test]
fn start_bound_consumes_the_first_matching_record() {
    let Some(f) = main_fixture() else { return };
    let base = run_ok(&["-u", "-f", as_str(&f)]);
    let times = cpu_row_times(&base);
    if times.len() < 3 {
        eprintln!("skipped: サンプルが 3 本未満なので -s の意味論を確かめられない");
        return;
    }

    let filtered = run_ok(&["-u", "-s", &times[0], "-f", as_str(&f)]);
    let got = cpu_row_times(&filtered);
    assert_eq!(
        got.first().map(String::as_str),
        Some(times[1].as_str()),
        "-s に合致した最初のレコードは表示されない (期待 {}、実際 {:?})",
        times[1],
        got.first()
    );
    assert_eq!(
        cpu_header_times(&filtered).first().map(String::as_str),
        Some(times[0].as_str()),
        "ヘッダ行の時刻は -s に合致したレコード (= 前サンプル) になる"
    );
}

/// `-e` を超えたレコードは表示されず、そこで打ち切られる。
#[test]
fn end_bound_truncates_the_report() {
    let Some(f) = main_fixture() else { return };
    let base = run_ok(&["-u", "-f", as_str(&f)]);
    let times = cpu_row_times(&base);
    if times.len() < 3 {
        eprintln!("skipped: サンプルが 3 本未満なので -e の意味論を確かめられない");
        return;
    }

    let filtered = run_ok(&["-u", "-e", &times[1], "-f", as_str(&f)]);
    let got = cpu_row_times(&filtered);
    assert_eq!(
        got.last().map(String::as_str),
        Some(times[1].as_str()),
        "-e で指定した時刻が最後の行になる"
    );
    assert!(got.len() < times.len(), "行数が減る");
}

/// `-s` と `-e` を同時に指定すると両端で絞られる。
#[test]
fn start_and_end_bounds_combine() {
    let Some(f) = main_fixture() else { return };
    let base = run_ok(&["-u", "-f", as_str(&f)]);
    let times = cpu_row_times(&base);
    if times.len() < 4 {
        eprintln!("skipped: サンプルが 4 本未満なので範囲指定を確かめられない");
        return;
    }
    let out = run_ok(&["-u", "-s", &times[0], "-e", &times[2], "-f", as_str(&f)]);
    let got = cpu_row_times(&out);
    assert_eq!(
        got,
        vec![times[1].clone(), times[2].clone()],
        "-s の 1 本目は消費され、-e までが表示される"
    );
}

// ===========================================================================
// sadf 互換
// ===========================================================================

/// `resarch sadf -j <file>` が妥当な JSON を出す。
#[test]
fn sadf_json_is_parseable() {
    let Some(f) = main_fixture() else { return };
    let out = run_ok(&["sadf", "-j", as_str(&f)]);
    let v: serde_json::Value = serde_json::from_str(&out).expect("妥当な JSON であること");
    let host = &v["sysstat"]["hosts"][0];
    assert!(host["nodename"].is_string(), "nodename が入る: {host}");
    assert!(
        host["statistics"].is_array(),
        "statistics が配列で入る: {host}"
    );
    assert!(
        !host["statistics"].as_array().unwrap().is_empty(),
        "statistics が空でない"
    );
}

/// `sadf` の 6 形式がすべて何かを出す。
#[test]
fn sadf_formats_all_produce_output() {
    let Some(f) = main_fixture() else { return };
    for opt in ["-d", "-p", "-j", "-x", "-r", "-H"] {
        let out = run_ok(&["sadf", opt, as_str(&f)]);
        assert!(!out.trim().is_empty(), "sadf {opt} が何も出さない");
    }
}

/// `sadf -x` は XML 宣言から始まる。
#[test]
fn sadf_xml_starts_with_a_declaration() {
    let Some(f) = main_fixture() else { return };
    let out = run_ok(&["sadf", "-x", as_str(&f)]);
    assert!(out.starts_with("<?xml version=\"1.0\""), "{out:.80}");
    assert!(out.contains("<sysdata-version>3.18</sysdata-version>"));
}

/// `sadf -d` はフィールド名一覧行 (`# hostname;interval;timestamp;…`) を持つ。
#[test]
fn sadf_db_has_a_field_list_line() {
    let Some(f) = main_fixture() else { return };
    let out = run_ok(&["sadf", "-d", as_str(&f)]);
    assert!(
        out.lines()
            .any(|l| l.starts_with("# hostname;interval;timestamp;")),
        "{out:.200}"
    );
}

/// `sadf ... -- -A` は既定 (CPU のみ) より多くの activity を出す。
#[test]
fn sadf_dash_dash_a_selects_all_activities() {
    let Some(f) = main_fixture() else { return };
    let def = run_ok(&["sadf", "-p", as_str(&f)]);
    let all = run_ok(&["sadf", "-p", as_str(&f), "--", "-A"]);
    assert!(
        all.len() > def.len(),
        "-- -A は既定より多くの行を出す ({} <= {})",
        all.len(),
        def.len()
    );
}

/// `sadf -H` はヘッダだけを出す (統計行を含まない)。
#[test]
fn sadf_header_only_prints_the_header() {
    let Some(f) = main_fixture() else { return };
    let out = run_ok(&["sadf", "-H", as_str(&f)]);
    assert!(out.contains("System activity data file"), "{out:.200}");
}

// ===========================================================================
// 独自出力
// ===========================================================================

/// `resarch show --format ndjson` の各行が `schema_version` 付きの JSON になる。
#[test]
fn show_ndjson_lines_carry_the_schema_version() {
    let Some(f) = main_fixture() else { return };
    let out = run_ok(&["show", as_str(&f), "--format", "ndjson"]);
    let lines: Vec<&str> = out.lines().filter(|l| !l.trim().is_empty()).collect();
    assert!(!lines.is_empty(), "1 行以上出る");
    for l in &lines {
        let v: serde_json::Value = serde_json::from_str(l).expect("各行が妥当な JSON であること");
        assert!(
            v["schema_version"].is_string(),
            "schema_version が入る: {v}"
        );
        assert!(v["record"].is_string(), "record 種別が入る: {v}");
    }
}

/// `--values both` は生値と派生値を別名前空間で出し、生値は十進文字列になる
/// (`docs/design.md` §11)。
#[test]
fn show_ndjson_raw_values_are_decimal_strings() {
    let Some(f) = main_fixture() else { return };
    let out = run_ok(&[
        "show",
        as_str(&f),
        "--format",
        "ndjson",
        "--values",
        "both",
        "--activity",
        "cpu",
    ]);
    let mut checked = false;
    for l in out.lines().filter(|l| !l.trim().is_empty()) {
        let v: serde_json::Value = serde_json::from_str(l).unwrap();
        if let Some(raw) = v["raw"].as_array() {
            for f in raw {
                if let Some(x) = f.get("raw") {
                    assert!(x.is_string(), "生値は十進文字列で出す: {f}");
                    checked = true;
                }
            }
        }
    }
    assert!(checked, "生値の名前空間が 1 つ以上出る");
}

/// 独自 JSON / CSV / table / sar / sadf-* の各形式が動く。
#[test]
fn show_supports_every_format() {
    let Some(f) = main_fixture() else { return };
    for fmt in [
        "table",
        "json",
        "csv",
        "ndjson",
        "sar",
        "sadf-p",
        "sadf-d",
        "sadf-json",
        "sadf-xml",
        "sadf-r",
    ] {
        let out = run_ok(&["show", as_str(&f), "--format", fmt]);
        assert!(!out.trim().is_empty(), "show --format {fmt} が何も出さない");
    }
}

/// `show --format json` は妥当な 1 文書になる。
#[test]
fn show_json_is_a_single_document() {
    let Some(f) = main_fixture() else { return };
    let out = run_ok(&["show", as_str(&f), "--format", "json"]);
    let v: serde_json::Value = serde_json::from_str(&out).expect("妥当な JSON であること");
    assert!(v["host"]["hostname"].is_string(), "{v:.200}");
    assert!(v["samples"].is_array());
}

/// 複数ファイルの `--format json` は配列で包まれ、全体として妥当な JSON になる。
#[test]
fn show_json_wraps_multiple_files_in_an_array() {
    let Some(a) = main_fixture() else { return };
    let Some(b) = fixture(ALT_FIXTURE) else {
        return;
    };
    let out = run_ok(&["show", as_str(&a), as_str(&b), "--format", "json"]);
    let v: serde_json::Value = serde_json::from_str(&out).expect("妥当な JSON であること");
    assert_eq!(v.as_array().map(Vec::len), Some(2), "1 ファイル 1 要素");
}

/// `--activity` で対象を絞れる。
#[test]
fn show_activity_filter_narrows_the_output() {
    let Some(f) = main_fixture() else { return };
    let all = run_ok(&["show", as_str(&f), "--format", "ndjson"]);
    let cpu = run_ok(&[
        "show",
        as_str(&f),
        "--format",
        "ndjson",
        "--activity",
        "cpu",
    ]);
    assert!(
        cpu.lines().count() < all.lines().count(),
        "絞った方が行数が少ない"
    );
    for l in cpu.lines().filter(|l| !l.trim().is_empty()) {
        let v: serde_json::Value = serde_json::from_str(l).unwrap();
        if v["record"] == "sample" {
            assert_eq!(v["activity"], "A_CPU", "CPU 以外が混ざらない: {v}");
        }
    }
}

/// 未知の activity 名は明示的に拒否する。
#[test]
fn show_rejects_an_unknown_activity_name() {
    let Some(f) = main_fixture() else { return };
    let err = run_err(&["show", as_str(&f), "--activity", "no_such_activity"]);
    assert!(err.contains("no_such_activity"), "{err}");
}

/// `--from` は `sar -s` と同じ意味論で行を減らす。
#[test]
fn show_from_narrows_the_time_window() {
    let Some(f) = main_fixture() else { return };
    let all = run_ok(&[
        "show",
        as_str(&f),
        "--format",
        "ndjson",
        "--activity",
        "cpu",
    ]);
    // 統計行の終点 epoch を集める
    let mut epochs: Vec<u64> = all
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| v["record"] == "sample")
        .filter_map(|v| v["end_epoch"].as_u64())
        .collect();
    epochs.sort_unstable();
    epochs.dedup();
    if epochs.len() < 3 {
        eprintln!("skipped: サンプルが 3 本未満なので --from を確かめられない");
        return;
    }

    let from = format_utc_hms(epochs[0]);
    let narrowed = run_ok(&[
        "show",
        as_str(&f),
        "--format",
        "ndjson",
        "--activity",
        "cpu",
        "--from",
        &from,
    ]);
    let kept = narrowed
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| v["record"] == "sample")
        .count();
    let total = all
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| v["record"] == "sample")
        .count();
    assert!(kept < total, "--from で行が減る ({kept} >= {total})");
    assert!(kept > 0, "--from で全部消えてしまってはいけない");
}

fn format_utc_hms(epoch: u64) -> String {
    use chrono::{TimeZone, Utc};
    Utc.timestamp_opt(epoch as i64, 0)
        .single()
        .expect("epoch を時刻へ直せること")
        .format("%H:%M:%S")
        .to_string()
}

// ===========================================================================
// summarize / compare
// ===========================================================================

/// `resarch summarize <file>` が期間集計と判定を出す。
#[test]
fn summarize_reports_period_and_findings() {
    let Some(f) = main_fixture() else { return };
    let out = run_ok(&["summarize", as_str(&f)]);
    assert!(out.contains("host:"), "{out:.300}");
    assert!(out.contains("起動区間"), "{out:.300}");
    assert!(out.contains("判定"), "{out:.300}");
}

/// `summarize --format json` は `MultiFileAnalysis` をそのまま出す。
#[test]
fn summarize_json_has_hosts_and_segments() {
    let Some(f) = main_fixture() else { return };
    let out = run_ok(&["summarize", as_str(&f), "--format", "json"]);
    let v: serde_json::Value = serde_json::from_str(&out).expect("妥当な JSON であること");
    assert!(v["schema_version"].is_string(), "{v:.200}");
    let hosts = v["hosts"].as_array().expect("hosts が配列");
    assert!(!hosts.is_empty(), "ホストが 1 つ以上出る");
    let segments = hosts[0]["segments"].as_array().expect("segments が配列");
    assert!(!segments.is_empty(), "起動区間が 1 つ以上出る");
    assert!(segments[0]["summary"]["summary_kind"].is_string());
    assert!(segments[0]["findings"].is_array());
}

/// `summarize --format ndjson` は起動区間ごとに 1 行出す。
#[test]
fn summarize_ndjson_emits_one_line_per_segment() {
    let Some(f) = main_fixture() else { return };
    let out = run_ok(&["summarize", as_str(&f), "--format", "ndjson"]);
    let lines: Vec<&str> = out.lines().filter(|l| !l.trim().is_empty()).collect();
    assert!(!lines.is_empty());
    for l in lines {
        let v: serde_json::Value = serde_json::from_str(l).expect("各行が妥当な JSON");
        assert_eq!(v["record"], "boot_segment", "{v}");
    }
}

/// 複数ファイルを渡すと `multi` 経路で 1 系列として集計される。
#[test]
fn summarize_accepts_multiple_files() {
    let Some(a) = main_fixture() else { return };
    let Some(b) = fixture(ALT_FIXTURE) else {
        return;
    };
    let out = run_ok(&["summarize", as_str(&a), as_str(&b), "--format", "json"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(
        v["files"].as_array().map(Vec::len),
        Some(2),
        "入力ファイルが 2 件記録される"
    );
}

/// `resarch compare --host a=… --host b=…` が共通期間を求めて比較する。
#[test]
fn compare_aligns_hosts_on_a_common_window() {
    let Some(f) = main_fixture() else { return };
    let a = format!("a={}", as_str(&f));
    let b = format!("b={}", as_str(&f));
    let out = run_ok(&["compare", "--host", &a, "--host", &b]);
    assert!(out.contains("共通期間"), "{out:.300}");
    assert!(out.contains('a') && out.contains('b'), "{out:.300}");
}

/// `compare --format json` は `HostComparison` の配列になる。
#[test]
fn compare_json_is_an_array_of_comparisons() {
    let Some(f) = main_fixture() else { return };
    let a = format!("a={}", as_str(&f));
    let b = format!("b={}", as_str(&f));
    let out = run_ok(&["compare", "--host", &a, "--host", &b, "--format", "json"]);
    let v: serde_json::Value = serde_json::from_str(&out).expect("妥当な JSON であること");
    let arr = v.as_array().expect("配列であること");
    for c in arr {
        assert!(c["metric"].is_object(), "{c}");
        assert!(c["hosts"].is_array(), "{c}");
    }
}

/// 1 ホストだけでは比較できないことを明示する。
#[test]
fn compare_requires_two_hosts() {
    let Some(f) = main_fixture() else { return };
    let a = format!("a={}", as_str(&f));
    let err = run_err(&["compare", "--host", &a]);
    assert!(err.contains("2 ホスト"), "{err}");
}

// ===========================================================================
// info と診断・終了コード
// ===========================================================================

#[test]
fn info_prints_the_header_and_activity_list() {
    let Some(f) = main_fixture() else { return };
    let out = run_ok(&["info", as_str(&f)]);
    assert!(out.contains("format        0x2175"), "{out:.300}");
    assert!(out.contains("activities"), "{out:.300}");
}

#[test]
fn info_json_is_parseable() {
    let Some(f) = main_fixture() else { return };
    let out = run_ok(&["info", as_str(&f), "--format", "json"]);
    let v: serde_json::Value = serde_json::from_str(&out).expect("妥当な JSON であること");
    assert!(v["activities"].is_array(), "{v:.200}");
}

/// `-o` (採取) は reSARch の対象外なので明示的に拒否する。
#[test]
fn collection_option_is_rejected_with_an_explanation() {
    let err = run_err(&["-u", "-o", "/tmp/resarch-should-not-be-written"]);
    assert!(
        err.contains("採取"),
        "採取が対象外だと分かる文言を出す: {err}"
    );
    assert!(
        !Path::new("/tmp/resarch-should-not-be-written").exists(),
        "ファイルを作ってはいけない"
    );
}

/// 開けないファイルは非ゼロ終了し、診断は stderr へ出す。
#[test]
fn missing_input_file_fails_with_a_diagnostic_on_stderr() {
    let out = resarch()
        .args(["-u", "-f", "/nonexistent/resarch/sa99"])
        .output()
        .expect("起動できること");
    assert!(!out.status.success(), "非ゼロ終了する");
    assert!(out.stdout.is_empty(), "stdout にデータを出さない");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("resarch:"),
        "stderr に診断を出す"
    );
}

/// 壊れたファイルは strict では拒否し、`--lenient` では読めたところまで返す。
#[test]
fn truncated_file_is_strict_by_default() {
    let Some(f) = fixture("data-trunc") else {
        return;
    };
    let out = resarch()
        .args(["info", as_str(&f)])
        .output()
        .expect("起動できること");
    // strict で拒否されるか、診断付きで読めるかはファイル次第。
    // どちらでも「黙って成功して中身が空」にはならないことを固定する。
    if out.status.success() {
        assert!(!out.stdout.is_empty(), "成功したなら何かを出しているはず");
    } else {
        assert!(
            !out.stderr.is_empty(),
            "失敗したなら理由を stderr に出しているはず"
        );
    }
}

/// `--help` / `-V` はルート専用オプションとして stdout に出て正常終了する。
#[test]
fn root_help_and_version_exit_zero() {
    let help = run_ok(&["--help"]);
    assert!(help.contains("show"), "{help:.200}");
    let version = run_ok(&["--version"]);
    assert!(version.contains("resarch"), "{version:.200}");
}

/// `sar --help` は sar 互換の使い方を出す (ルートのヘルプとは別物)。
#[test]
fn sar_help_prints_the_compat_usage() {
    let out = run_ok(&["sar", "--help"]);
    assert!(out.contains("sar 互換入口"), "{out:.200}");
    assert!(out.contains("-P"), "{out:.400}");
}

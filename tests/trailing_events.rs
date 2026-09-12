//! 末尾イベント (最後の統計レコードより後ろにある COMMENT / RESTART) の回帰テスト。
//!
//! # 背景
//!
//! 走査 API には 2 つの経路がある。
//!
//! - `walk` — イベントを次の統計レコードへ**束ねて**渡す。
//!   `STATS → COMMENT → EOF` の COMMENT と、統計を 1 件も含まないファイルの
//!   イベント列は、束ねる相手が居ないので出力に現れない。
//! - `walk_items` — イベントを**読んだ順にその場で**通知する。取りこぼしが無い。
//!
//! 出力層はすべて後者へ移行した。ここでは次の 3 点を固定する。
//!
//! 1. 最後の統計レコードより後ろにある COMMENT が各出力に現れること
//! 2. 統計レコードを 1 件も含まないファイルでも COMMENT が現れること
//! 3. **既存の出力順序が変わっていないこと**
//!    (イベント行は、ファイル上でそのイベントの後ろにある統計レコードより前に出る)
//!
//! `docs/format/03-output-format.md` §1.10 / §0.3 のとおり、本家も
//! COMMENT を読んだ時点で表示し、`Average:` 行はその後に出す。

mod fixtures;

use fixtures::{FixtureAbi, FixtureSpec, Generation, RecordSpec, build};

use re_sar_ch::format::SaFile;
use re_sar_ch::output::json::CustomConfig;
use re_sar_ch::output::sadf::SadfConfig;
use re_sar_ch::output::sar_text::SarTextOptions;
use re_sar_ch::output::{ndjson, sadf, sar_text};

// ===========================================================================
// fixture の組み立て
// ===========================================================================

/// COMMENT の本文。出力中で一意に見つけられる語にする。
const NOTE: &str = "trailing note";

/// `FixtureSpec::skeleton` の `cpu_nr` と同じ値 (CPU "all" を含む数)。
const CPU_NR: i32 = 3;

/// 統計レコード。`uptime` を進めて区間が 0 にならないようにする。
fn stats(ust_time: u64, second: u8, uptime: u64) -> RecordSpec {
    let mut rec = RecordSpec::stats(vec![CPU_NR, 0], ust_time, 12, 27, second);
    rec.uptime = uptime;
    rec
}

/// COMMENT レコード。
fn comment(ust_time: u64, second: u8, text: &str) -> RecordSpec {
    RecordSpec::comment(text, ust_time, 12, 27, second)
}

/// RESTART レコード。
fn restart(ust_time: u64, second: u8) -> RecordSpec {
    RecordSpec::restart(CPU_NR, ust_time, 12, 27, second)
}

/// `A_CPU` + `A_PCSW` を持つ標準構成に、指定したレコード列を載せて開く。
fn open(label: &str, records: Vec<RecordSpec>) -> SaFile {
    let mut spec = FixtureSpec::minimal(Generation::G2175Current, FixtureAbi::Le64);
    spec.records = records;
    SaFile::from_bytes(label, build(spec).bytes).expect("fixture を開けること")
}

/// `STATS → STATS → COMMENT → EOF`。末尾の COMMENT には束ねる相手が居ない。
fn file_with_trailing_comment() -> SaFile {
    open(
        "trailing_comment",
        vec![
            stats(1_600_000_011, 1, 100_000),
            stats(1_600_000_021, 11, 101_000),
            comment(1_600_000_031, 21, NOTE),
        ],
    )
}

/// COMMENT だけのファイル (統計レコード 0 件)。
fn file_with_only_a_comment() -> SaFile {
    open("comment_only", vec![comment(1_600_000_011, 1, NOTE)])
}

// ===========================================================================
// 出力の生成
// ===========================================================================

/// `-C` 相当 (COMMENT を出す) の `sadf` 設定。
fn sadf_cfg() -> SadfConfig {
    SadfConfig {
        comments: true,
        ..Default::default()
    }
}

fn to_text(buf: Vec<u8>) -> String {
    String::from_utf8(buf).expect("出力は UTF-8")
}

/// `sar` 互換テキスト (`-C` 相当)。
fn sar_text_out(file: &SaFile) -> String {
    let opts = SarTextOptions {
        comment: true,
        ..Default::default()
    };
    let acts = sar_text::activities_in_file(file);
    let mut buf = Vec::new();
    sar_text::write_report(&mut buf, file, &opts, &acts).expect("sar テキストを書けること");
    to_text(buf)
}

/// 独自 NDJSON。
fn ndjson_out(file: &SaFile) -> String {
    let mut buf = Vec::new();
    ndjson::write_ndjson(&mut buf, file, &CustomConfig::default()).expect("NDJSON を書けること");
    to_text(buf)
}

/// `sadf -j`。
fn sadf_json_out(file: &SaFile) -> String {
    let mut buf = Vec::new();
    sadf::json::write_json(&mut buf, file, &sadf_cfg()).expect("sadf -j を書けること");
    to_text(buf)
}

/// `sadf -x`。
fn sadf_xml_out(file: &SaFile) -> String {
    let mut buf = Vec::new();
    sadf::xml::write_xml(&mut buf, file, &sadf_cfg()).expect("sadf -x を書けること");
    to_text(buf)
}

/// `sadf -d`。
fn sadf_db_out(file: &SaFile) -> String {
    let mut buf = Vec::new();
    sadf::dbppc::write_db(&mut buf, file, &sadf_cfg()).expect("sadf -d を書けること");
    to_text(buf)
}

/// `sadf -p`。
fn sadf_ppc_out(file: &SaFile) -> String {
    let mut buf = Vec::new();
    sadf::dbppc::write_ppc(&mut buf, file, &sadf_cfg()).expect("sadf -p を書けること");
    to_text(buf)
}

/// `sadf -r`。
fn sadf_raw_out(file: &SaFile) -> String {
    let mut buf = Vec::new();
    sadf::raw::write_raw(&mut buf, file, &sadf_cfg()).expect("sadf -r を書けること");
    to_text(buf)
}

/// 出力中の位置。見つからなければテストを落とす。
fn index_of(haystack: &str, needle: &str, what: &str) -> usize {
    match haystack.find(needle) {
        Some(i) => i,
        None => panic!("{what} が出力に無い ({needle:?}):\n{haystack}"),
    }
}

// ===========================================================================
// 1. 末尾の COMMENT
// ===========================================================================

/// 最後の統計レコードより後ろにある COMMENT が、
/// `sar` 互換テキスト・`sadf` 各形式・独自 NDJSON のすべてに現れること。
#[test]
fn trailing_comment_appears_in_every_output() {
    let file = file_with_trailing_comment();

    let text = sar_text_out(&file);
    assert!(
        text.contains(&format!("COM {NOTE}")),
        "sar 互換テキストに COM 行が無い:\n{text}"
    );

    let nd = ndjson_out(&file);
    assert!(
        nd.lines()
            .any(|l| l.contains("\"record\":\"comment\"") && l.contains(NOTE)),
        "NDJSON に comment 行が無い:\n{nd}"
    );

    let js = sadf_json_out(&file);
    assert!(
        js.contains(&format!("\"com\": \"{NOTE}\"")),
        "sadf -j の comments に無い:\n{js}"
    );

    let xml = sadf_xml_out(&file);
    assert!(
        xml.contains(&format!("com=\"{NOTE}\"")),
        "sadf -x の <comments> に無い:\n{xml}"
    );

    for (label, out) in [
        ("sadf -d", sadf_db_out(&file)),
        ("sadf -p", sadf_ppc_out(&file)),
        ("sadf -r", sadf_raw_out(&file)),
    ] {
        assert!(
            out.contains(&format!("COM {NOTE}")),
            "{label} に COM 行が無い:\n{out}"
        );
    }
}

/// `sar` 互換テキストでは、末尾の COMMENT は `Average:` 行**より前**に出る。
///
/// 本家も「COMMENT は読んだ時点で表示し、区間の終わりで平均を出す」順序
/// (03 §1.10 の内側ループ)。
#[test]
fn trailing_comment_precedes_the_average_line() {
    let file = file_with_trailing_comment();
    let text = sar_text_out(&file);

    let com = index_of(&text, &format!("COM {NOTE}"), "COM 行");
    let avg = index_of(&text, "Average:", "Average 行");
    assert!(com < avg, "COM は Average より前に出ること:\n{text}");
}

// ===========================================================================
// 2. COMMENT だけのファイル
// ===========================================================================

/// 統計レコードを 1 件も含まないファイルでも COMMENT が出力に現れること。
///
/// 束ねる経路では「載せる統計レコードが無い」ため丸ごと消えていた。
#[test]
fn comment_only_file_still_reports_the_comment() {
    let file = file_with_only_a_comment();

    let text = sar_text_out(&file);
    assert!(
        text.contains(&format!("COM {NOTE}")),
        "sar 互換テキストに COM 行が無い:\n{text}"
    );

    let nd = ndjson_out(&file);
    assert!(
        nd.lines()
            .any(|l| l.contains("\"record\":\"comment\"") && l.contains(NOTE)),
        "NDJSON に comment 行が無い:\n{nd}"
    );

    let js = sadf_json_out(&file);
    assert!(
        js.contains(&format!("\"com\": \"{NOTE}\"")),
        "sadf -j の comments に無い:\n{js}"
    );

    let xml = sadf_xml_out(&file);
    assert!(
        xml.contains(&format!("com=\"{NOTE}\"")),
        "sadf -x の <comments> に無い:\n{xml}"
    );

    // 統計行は 1 行も出ない (イベントだけが出る)
    assert!(
        !nd.contains("\"record\":\"sample\""),
        "統計レコードが無いのに sample 行が出ている:\n{nd}"
    );
}

// ===========================================================================
// 3. 既存の出力順序
// ===========================================================================

/// `RESTART → STATS → COMMENT → STATS` で、イベント行が
/// **後続の統計レコードより前**に出ること (束ねていた頃と同じ並び)。
#[test]
fn events_keep_their_position_relative_to_samples() {
    let file = open(
        "ordered",
        vec![
            restart(1_600_000_001, 1),
            stats(1_600_000_011, 11, 100_000),
            comment(1_600_000_021, 21, NOTE),
            stats(1_600_000_031, 31, 101_000),
        ],
    );

    // --- NDJSON: restart → sample(1 本目) → comment → sample(2 本目) ---
    let nd = ndjson_out(&file);
    let kinds: Vec<&str> = nd
        .lines()
        .map(|l| {
            if l.contains("\"record\":\"restart\"") {
                "restart"
            } else if l.contains("\"record\":\"comment\"") {
                "comment"
            } else {
                "sample"
            }
        })
        .collect();
    assert_eq!(kinds.first().copied(), Some("restart"), "先頭は restart 行");
    let comment_at = kinds
        .iter()
        .position(|k| *k == "comment")
        .expect("comment 行がある");
    assert!(
        kinds[..comment_at].contains(&"sample"),
        "COMMENT の前に 1 本目のサンプル行が出ること:\n{nd}"
    );
    assert!(
        kinds[comment_at + 1..].contains(&"sample"),
        "COMMENT の後に 2 本目のサンプル行が出ること:\n{nd}"
    );
    assert_eq!(
        kinds.iter().filter(|k| **k == "comment").count(),
        1,
        "COMMENT 行が重複していないこと:\n{nd}"
    );
    assert_eq!(
        kinds.iter().filter(|k| **k == "restart").count(),
        1,
        "RESTART 行が重複していないこと:\n{nd}"
    );

    // --- sar 互換テキスト: LINUX RESTART → COM → 統計行 (Average より前) ---
    let text = sar_text_out(&file);
    let rst = index_of(&text, "LINUX RESTART", "LINUX RESTART 行");
    let com = index_of(&text, &format!("COM {NOTE}"), "COM 行");
    let avg = index_of(&text, "Average:", "Average 行");
    assert!(rst < com, "RESTART は COMMENT より前:\n{text}");
    assert!(com < avg, "COMMENT は Average より前:\n{text}");

    // --- sadf -d: COM 行はブロックの統計行と同じブロックに出る ---
    let db = sadf_db_out(&file);
    assert!(
        db.contains(&format!("COM {NOTE}")),
        "sadf -d に COM 行が無い:\n{db}"
    );
}

/// 末尾の RESTART も `sar` 互換テキストに現れること。
#[test]
fn trailing_restart_appears_in_sar_text() {
    let file = open(
        "trailing_restart",
        vec![
            stats(1_600_000_011, 1, 100_000),
            stats(1_600_000_021, 11, 101_000),
            restart(1_600_000_031, 21),
        ],
    );

    let text = sar_text_out(&file);
    let avg = index_of(&text, "Average:", "Average 行");
    let rst = index_of(&text, "LINUX RESTART", "LINUX RESTART 行");
    assert!(
        avg < rst,
        "区間の平均を出してから RESTART 行を出すこと:\n{text}"
    );
}

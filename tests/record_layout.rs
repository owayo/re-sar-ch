//! レコード列の構造に対する回帰テスト。
//!
//! ここに置くのは「**本家の期待出力では突けない**が、レコードの並べ方で
//! 壊れる挙動」である。本家の `expected.*` はどれも
//!
//! - `LINUX RESTART` を最初の統計レコードより前に 1 個しか持たない
//! - レコード内 item 数が 0 のサンプルを含まない
//!
//! ため、区間の入れ子や 0 件サンプルの扱いを全文比較では検証できない。
//! 自作 fixture (`tests/fixtures`) でレコード列を組み立てて固定する。
//!
//! # 1. イベントの取りこぼし (走査 API)
//!
//! 走査 API には 2 つの経路がある。
//!
//! - `walk` — イベントを次の統計レコードへ**束ねて**渡す。
//!   `STATS → COMMENT → EOF` の COMMENT と、統計を 1 件も含まないファイルの
//!   イベント列は、束ねる相手が居ないので出力に現れない。
//! - `walk_items` — イベントを**読んだ順にその場で**通知する。取りこぼしが無い。
//!
//! 出力層はすべて後者へ移行した。固定するのは次の 3 点。
//!
//! 1. 最後の統計レコードより後ろにある COMMENT が各出力に現れること
//! 2. 統計レコードを 1 件も含まないファイルでも COMMENT が現れること
//! 3. **既存の出力順序が変わっていないこと**
//!    (イベント行は、ファイル上でそのイベントの後ろにある統計レコードより前に出る)
//!
//! # 2. 区間 (`LINUX RESTART` 区切り) の入れ子
//!
//! 本家は**区間を外側、activity を内側**に回す
//! (`docs/format/03-output-format.md` §2.1 の 2′ / 2″)。区切りの RESTART 行は
//! 全ブロックの後に 1 回だけ出て、区間内の COMMENT は activity の数だけ出る。
//!
//! # 3. 走査範囲 (`RecordRange`)
//!
//! 区間ごとの走査で「区間外はデコードもしない」ための仕組み。
//! 添字が走査ごとにずれると区間の切り方が壊れるので、番号の付け方を固定する。
//!
//! # 4. レコード内 item 数 0
//!
//! 本家が許容する `count == 0` を拒否していないこと。

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
///
/// **回数と位置の両方を固定する。** 本家 `sar.c: read_stats_from_file()` の構造は
///
/// 1. activity ごとに `handle_curr_act_stats()` を呼ぶ。この関数のサンプルループは
///    `R_RESTART` を読んだ時点で**印字せずに抜け**、そのあと `Average:` を出す
/// 2. 全 activity を回し終えてから、区間を終わらせた RESTART を
///    `print_special_record()` で**1 回だけ**出す
///
/// したがって区間を区切る RESTART 行は「全ブロックの後に 1 回」であり、
/// activity ごとに繰り返されない。COMMENT は逆に `handle_curr_act_stats()` の
/// 中で出るのでブロックごとに繰り返される (03 §2.1 の 2′)。
///
/// 本家の期待出力はどれも RESTART を「最初の統計レコードより前」に 1 個しか
/// 持たないため、この違いは golden 比較では突けない。自作 fixture で固定する。
#[test]
fn trailing_restart_appears_once_after_all_blocks() {
    let file = open(
        "trailing_restart",
        vec![
            stats(1_600_000_011, 1, 100_000),
            stats(1_600_000_021, 11, 101_000),
            restart(1_600_000_031, 21),
        ],
    );

    // fixture は複数 activity を含む。ブロックが 2 つ以上ないと
    // 「1 回だけ」の検証にならないので前提として確認する。
    let acts = sar_text::activities_in_file(&file);
    assert!(
        acts.len() >= 2,
        "この検証には 2 つ以上の activity が必要 (実際 {acts:?})"
    );

    let text = sar_text_out(&file);
    let avg = index_of(&text, "Average:", "Average 行");
    let rst = index_of(&text, "LINUX RESTART", "LINUX RESTART 行");
    assert!(
        avg < rst,
        "区間の平均を出してから RESTART 行を出すこと:\n{text}"
    );
    assert_eq!(
        text.matches("LINUX RESTART").count(),
        1,
        "区間を区切る RESTART 行は全 activity ブロックの後に 1 回だけ \
         (activity ごとに繰り返さない):\n{text}"
    );
}

/// ファイル途中の RESTART は「区間 → activity」の入れ子になること。
///
/// 本家 `sar.c: read_stats_from_file()` は**区間を外側ループ、activity を
/// 内側ループ**にしている。`handle_curr_act_stats()` のサンプルループは
/// `R_RESTART` を読んだ時点で印字せずに抜け、区間を終わらせた RESTART 行は
/// 全 activity を回し終えてから 1 回だけ出る。
///
/// 期待する並び (activity が `A_CPU` / `A_PCSW` の 2 つの場合):
///
/// ```text
/// 区間 1 の CPU ブロック + Average
/// 区間 1 の PCSW ブロック + Average
/// LINUX RESTART                      ← 1 回だけ
/// 区間 2 の CPU ブロック + Average
/// 区間 2 の PCSW ブロック + Average
/// ```
///
/// 「activity を外側」にすると RESTART 行が activity の数だけ出て、
/// 同じ時刻のブロックが 2 か所に分かれる。本家の期待出力は RESTART を
/// 最初の統計レコードより前に 1 個しか持たないのでこの違いを突けない。
#[test]
fn midfile_restart_groups_by_region_then_activity() {
    let file = open(
        "midfile_restart",
        vec![
            stats(1_600_000_011, 1, 100_000),
            stats(1_600_000_021, 11, 101_000),
            restart(1_600_000_031, 21),
            stats(1_600_000_041, 31, 103_000),
            stats(1_600_000_051, 41, 104_000),
        ],
    );

    let acts = sar_text::activities_in_file(&file);
    assert!(
        acts.len() >= 2,
        "この検証には 2 つ以上の activity が必要 (実際 {acts:?})"
    );

    let text = sar_text_out(&file);
    assert_eq!(
        text.matches("LINUX RESTART").count(),
        1,
        "区間を区切る RESTART 行は 1 回だけ:\n{text}"
    );

    // ヘッダ行の並びで入れ子の向きを判定する。
    // 「区間 → activity」なら CPU, PCSW, (RESTART), CPU, PCSW の順になる。
    let order: Vec<&str> = text
        .lines()
        .filter_map(|l| {
            if l.contains("LINUX RESTART") {
                Some("RESTART")
            } else if l.contains("%user") {
                Some("CPU")
            } else if l.contains("cswch/s") {
                Some("PCSW")
            } else {
                None
            }
        })
        .collect();
    assert_eq!(
        order,
        ["CPU", "PCSW", "RESTART", "CPU", "PCSW"],
        "区間を外側、activity を内側にすること:\n{text}"
    );
}

/// 区間の最後のサンプルより後ろ・区切りの RESTART より前にある COMMENT は、
/// **各ブロックの中**に (`Average:` 行より前に) 出ること。
///
/// 本家 `handle_curr_act_stats()` のサンプルループは COMMENT を読むと
/// `print_special_record()` で出して `continue` し、`R_RESTART` を読んで初めて
/// 抜ける。したがって COMMENT は activity の数だけ繰り返され、
/// 区切りの RESTART は 1 回しか出ない (03 §2.1 の 2′ / 2″)。
#[test]
fn comment_before_a_terminating_restart_repeats_per_block() {
    let file = open(
        "comment_then_restart",
        vec![
            stats(1_600_000_011, 1, 100_000),
            stats(1_600_000_021, 11, 101_000),
            comment(1_600_000_026, 16, NOTE),
            restart(1_600_000_031, 21),
            stats(1_600_000_041, 31, 103_000),
        ],
    );

    let acts = sar_text::activities_in_file(&file);
    let text = sar_text_out(&file);

    assert_eq!(
        text.matches(&format!("COM {NOTE}")).count(),
        acts.len(),
        "区間内の COMMENT は activity の数だけ出ること (activity {} 個):\n{text}",
        acts.len()
    );
    assert_eq!(
        text.matches("LINUX RESTART").count(),
        1,
        "区切りの RESTART は 1 回だけ:\n{text}"
    );

    // 各ブロックで COM が Average より前に出ていること
    for block in text.split("LINUX RESTART").next().unwrap().split("\n\n") {
        if !block.contains(&format!("COM {NOTE}")) {
            continue;
        }
        let com = block.find(&format!("COM {NOTE}")).unwrap();
        let avg = block
            .find("Average:")
            .expect("同じブロックに Average がある");
        assert!(com < avg, "COM は Average より前:\n{block}");
    }
}

// ===========================================================================
// 4. 走査範囲 (RecordRange)
// ===========================================================================

/// `RecordRange` が指定どおりのレコードだけを通知すること。
///
/// 添字は「通知されるレコードの通し番号」で、拡張レコードは番号を消費しない。
/// `sar` 互換出力は区間 × activity で何度も走査するので、この番号が走査ごとに
/// ずれると区間の切り方が壊れる。
#[test]
fn record_range_limits_what_is_visited() {
    use re_sar_ch::format::file::ScanControl;
    use re_sar_ch::series::{RecordRange, Selection, WalkItem, walk_items_in};

    let file = open(
        "record_range",
        vec![
            stats(1_600_000_011, 1, 100_000),  // 0
            comment(1_600_000_016, 6, NOTE),   // 1
            stats(1_600_000_021, 11, 101_000), // 2
            restart(1_600_000_031, 21),        // 3
            stats(1_600_000_041, 31, 103_000), // 4
        ],
    );

    /// 通知されたレコードを種別 + 秒で記録する。
    fn collect(file: &SaFile, range: RecordRange) -> Vec<String> {
        let mut seen = Vec::new();
        walk_items_in(file, &Selection::All, range, |item| {
            match item {
                WalkItem::Event(ev) => {
                    let (_, _, s) = ev.time();
                    seen.push(format!("event@{s}"));
                }
                WalkItem::Sample(view) => {
                    seen.push(format!(
                        "sample@{}{}",
                        view.curr.second,
                        if view.has_prev { "" } else { " (基準)" }
                    ));
                }
            }
            Ok(ScanControl::Continue)
        })
        .expect("走査できること");
        seen
    }

    assert_eq!(
        collect(&file, RecordRange::ALL),
        [
            "sample@1 (基準)",
            "event@6",
            "sample@11",
            "event@21",
            "sample@31"
        ],
        "既定は全件"
    );

    assert_eq!(
        collect(&file, RecordRange::new(2, 4)),
        ["sample@11 (基準)", "event@21"],
        "範囲の最初の統計レコードは前値を持たない (区間の基準値になる)"
    );

    assert_eq!(
        collect(&file, RecordRange::new(4, usize::MAX)),
        ["sample@31 (基準)"],
        "末尾だけを走査できる"
    );

    assert_eq!(
        collect(&file, RecordRange::new(3, 3)),
        Vec::<String>::new(),
        "空範囲は何も通知しない"
    );
}

// ===========================================================================
// 5. レコード内 item 数が 0 のサンプル
// ===========================================================================

/// `has_nr` 付き activity のレコード内件数が **0** でも受け入れること。
///
/// 本家は `read_nr_value` を `non_zero = FALSE` で呼び、`count == 0` を
/// 「このサンプルはアイテム 0 件」として扱う (01 §6.2 の表)。
/// 番兵で末尾を切り詰めた結果 0 になるのは正当な出力で、
/// `sadf -c` が書くファイルにも現れ得る。
///
/// 以前は 0 を `LimitExceeded` で拒否していたため、**reSARch 自身が変換した
/// ファイルを読めなくなる**ケースがあった (`file_activity.nr == 0` の拒否と
/// 混同していた。あちらは activity リスト側で、本家も拒否する)。
#[test]
fn zero_item_count_in_a_record_is_accepted() {
    let mut spec = FixtureSpec::minimal(Generation::G2175Current, FixtureAbi::Le64);
    // 1 本目は CPU 3 件、2 本目は 0 件。
    spec.records = vec![stats(1_600_000_011, 1, 100_000), {
        let mut rec = RecordSpec::stats(vec![0, 0], 1_600_000_021, 12, 27, 11);
        rec.uptime = 101_000;
        rec
    }];
    let file = SaFile::from_bytes("zero_count", build(spec).bytes)
        .expect("レコード内件数 0 を拒否しないこと");

    // 末尾まで余りなく読めること
    let summary = re_sar_ch::series::walk_items(&file, &re_sar_ch::series::Selection::All, |_| {
        Ok(re_sar_ch::format::file::ScanControl::Continue)
    })
    .expect("走査できること");
    assert!(
        summary.is_exact(),
        "末尾まで余りなく読めること: {summary:?}"
    );

    // 0 件のサンプルは行を持たないが、レポートの生成自体は成功する
    let text = sar_text_out(&file);
    assert!(text.contains("CPU"), "1 本目の CPU ブロックは出る:\n{text}");
}

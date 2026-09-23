//! 旧フォーマット変換 (`sadf -c` 相当) の結合テスト。
//!
//! # 何を検証しているか
//!
//! | # | テスト | 何を固定するか |
//! |---|---|---|
//! | 1 | [`converted_files_round_trip_as_current_format`] | 変換結果が `0x2175` として読み直せ、レコード数・時刻・種別・item 数が元と対応すること |
//! | 2 | [`converted_values_match_direct_read`] | **全 activity・全 item・全フィールド**で、直読した値と変換後を読んだ値が一致すること (本家 `upgrade_stats_*` 18 本に相当する正しさ) |
//! | 3 | [`converted_output_matches_upstream_expected`] | 変換結果に `sar -C -A` 相当を当てた出力が本家の `expected.data-*` と一致すること |
//! | 3' | [`explicit_hz_conversion_matches_upstream_expected`] | `-O hz=250` を明示した変換でも出力が `expected.data-9.1.6-hz` と一致すること |
//! | 4 | [`unconvertible_and_current_formats_are_handled_upstream`] | `0x2170` は拒否、`0x2175` は 1 バイトも書かないこと |
//! | 5 | [`narrow_unsigned_long_is_reserialized`] 他 | 32bit / ビッグエンディアンで `unsigned long` が再直列化されること (自作 fixture) |
//! | 6 | [`sentinel_counts_become_record_item_counts`] 他 | 番兵による件数の打ち切り・素通し・RESTART・切り詰め・上限超過・出力先の失敗・CLI (item を狙って組んだ自作 fixture) |
//! | 7 | [`crafted_conversions_match_upstream_sadf_c`] | 6 の自作 fixture を本家 `sadf -c` にも通し、変換結果がバイト単位で一致すること (任意) |
//!
//! # 3 番が変換の正しさの外部基準である理由
//!
//! 本家の `expected.data-9.1.6` / `expected.data-10.3.1` / `expected.data-11.6.5` は
//!
//! ```sh
//! sadf -c <旧ファイル> > tmp && sar -C -A -f tmp
//! ```
//!
//! で作られている (`docs/format/04-test-data.md` §4.2 の `tests/00605` / `00615` / `00625`)。
//! つまり**変換したファイルを読み直した出力**が期待値である。
//! ここが一致すれば、変換のフィールド対応・型拡幅・件数の切り詰めが
//! まとめて外部から固定される。
//!
//! `tests/conformance.rs` は同じ期待出力を「旧ファイルの直読」と比べており
//! (reSARch は直読できるため)、そちらとは別の経路を見ている。
//!
//! # 本家データの扱い
//!
//! 本家データは GPL-2.0-or-later なので同梱できない。
//! `make fixtures` で `target/fixtures/upstream/` へ取得したうえで
//!
//! ```sh
//! cargo test --test convert -- --include-ignored --nocapture
//! ```
//!
//! で走らせる。未取得なら**何も失敗させずスキップ**する。
//! 自作 fixture だけを使うテストは既定でも走る。
//!
//! 7 番は本家の `sadf` バイナリそのものを起動する。GPL なので同梱せず、
//!
//! ```sh
//! RESARCH_UPSTREAM_SADF=<本家 12.8.0 の sadf> cargo test --test convert -- --include-ignored upstream_sadf
//! ```
//!
//! のようにパスを与えたときだけ比べる (未設定ならスキップ)。本家の出力は保存しない。

mod fixtures;
mod golden;

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use fixtures::{ActivitySpec, FixtureAbi, FixtureSpec, Generation, RecordSpec};
use golden::Mask;

use re_sar_ch::Error;
use re_sar_ch::cli::sar_args::{Activity, OptFlags, SarOptions, parse_sar_args};
use re_sar_ch::convert::{self, ConvertOptions, ConvertReport, HzSource};
use re_sar_ch::format::{OpenOptions, SaFile, ScanControl, Tolerance};
use re_sar_ch::model::{ActivityId, Availability, CompatDateFormat, HeaderRows};
use re_sar_ch::output::sadf;
use re_sar_ch::output::sar_text::{self, CpuSelection, SarTextOptions, TimeStyle};
use re_sar_ch::output::time_filter::TimeFilter;
use re_sar_ch::series::{Selection, WalkItem, walk_items};

// ===========================================================================
// 取得物の発見 (`tests/conformance.rs` と同じ作り)
// ===========================================================================

const UPSTREAM_SUBDIR: &str = "fixtures/upstream";
const PROVENANCE_FILE: &str = "PROVENANCE.txt";

const HOW_TO_FETCH: &str = "本家 fixture が無いのでスキップした。\n\
     取得するには `make fixtures` (= cargo run --bin xtask -- fetch-fixtures) を実行する。\n\
     本家データは GPL-2.0-or-later のためリポジトリには同梱していない。";

fn upstream_dir() -> Option<PathBuf> {
    let target = match std::env::var_os("CARGO_TARGET_DIR") {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"),
    };
    let dir = target.join(UPSTREAM_SUBDIR);
    if dir.join(PROVENANCE_FILE).is_file() {
        Some(dir)
    } else {
        None
    }
}

fn upstream_or_skip(test: &str) -> Option<PathBuf> {
    match upstream_dir() {
        Some(d) => Some(d),
        None => {
            eprintln!("skipped: {test}: {HOW_TO_FETCH}");
            None
        }
    }
}

fn upstream_file(dir: &Path, name: &str) -> Option<PathBuf> {
    let p = dir.join(name);
    if p.is_file() {
        Some(p)
    } else {
        eprintln!("skipped: {name} が無い ({})。{HOW_TO_FETCH}", p.display());
        None
    }
}

// ===========================================================================
// 変換対象の一覧
// ===========================================================================

/// 変換して期待出力と比べるケース。
struct ConvCase {
    /// 本家テスト番号 (追跡用)。
    upstream_test: &'static str,
    /// 入力データ (旧形式)。
    data: &'static str,
    /// 期待出力。`sadf -c` の結果を `sar -C -A` で読んだもの。
    golden: &'static str,
    /// 本家のコマンドライン (再現の参照用)。
    upstream_cmd: &'static str,
}

const CONV_CASES: &[ConvCase] = &[
    ConvCase {
        upstream_test: "00605",
        data: "data-9.1.6",
        golden: "expected.data-9.1.6",
        upstream_cmd: "LC_ALL=C TZ=GMT sadf -c data-9.1.6 > tmp && sar -C -A -f tmp",
    },
    ConvCase {
        upstream_test: "00615",
        data: "data-10.3.1",
        golden: "expected.data-10.3.1",
        upstream_cmd: "LC_ALL=C TZ=GMT sadf -c data-10.3.1 > tmp && sar -C -A -f tmp",
    },
    ConvCase {
        upstream_test: "00625",
        data: "data-11.6.5",
        golden: "expected.data-11.6.5",
        upstream_cmd: "LC_ALL=C TZ=GMT sadf -c data-11.6.5 > tmp && sar -C -A -f tmp",
    },
];

// ===========================================================================
// 共通ヘルパ
// ===========================================================================

/// 変換してバイト列と報告を得る。
fn convert_bytes(file: &SaFile, hz: Option<u64>) -> (Vec<u8>, ConvertReport) {
    let mut out: Vec<u8> = Vec::new();
    let report = convert::convert(file, &ConvertOptions { hz }, &mut out)
        .unwrap_or_else(|e| panic!("{} の変換に失敗: {e}", file.path().display()));
    (out, report)
}

/// 変換結果を読み直す。
fn reopen(label: &str, bytes: Vec<u8>) -> SaFile {
    SaFile::from_bytes(label, bytes)
        .unwrap_or_else(|e| panic!("変換結果 ({label}) を読み直せない: {e}"))
}

/// 1 レコードの骨格 (種別・時刻・activity ごとの item 数)。
#[derive(Debug, Clone, PartialEq, Eq)]
struct RecordShape {
    kind: &'static str,
    ust_time: u64,
    hms: (u8, u8, u8),
    /// `(activity id, このレコードでの item 数, nr2)`。
    items: Vec<(u32, u32, u32)>,
    cpu_count: Option<u32>,
    comment: Option<String>,
}

fn record_shapes(file: &SaFile) -> Vec<RecordShape> {
    let mut out = Vec::new();
    file.scan(|rec| {
        out.push(RecordShape {
            kind: rec.kind.as_str(),
            ust_time: rec.ust_time,
            hms: (rec.hour, rec.minute, rec.second),
            items: rec
                .slices
                .iter()
                .map(|s| (s.id.0, s.nr, s.nr2))
                .collect::<Vec<_>>(),
            cpu_count: rec.cpu_count,
            comment: rec.comment.map(|s| String::from_utf8_lossy(s).into_owned()),
        });
        Ok(ScanControl::Continue)
    })
    .unwrap_or_else(|e| panic!("{} を走査できない: {e}", file.path().display()));
    out
}

/// 1 item のフィールド値 (名前 → 値)。読めないフィールドは載せない。
type ItemValues = BTreeMap<&'static str, u64>;

/// 統計レコード 1 件分の値 (activity ID → item ごとの値)。
type SampleValues = BTreeMap<u32, Vec<ItemValues>>;

/// 統計レコードを順に読み、フィールド名で引ける形に集める。
///
/// 旧 revision と現行 revision ではフィールドの**宣言順が違う**ので、
/// 位置ではなく名前で対応付けなければならない。
fn collect_values(file: &SaFile) -> Vec<SampleValues> {
    let mut out: Vec<SampleValues> = Vec::new();
    walk_items(file, &Selection::All, |item| {
        if let WalkItem::Sample(view) = item {
            let mut sample: SampleValues = BTreeMap::new();
            for act in &view.curr.activities {
                let Some(plan) = view.plan_for(act.id) else {
                    continue;
                };
                let items = act
                    .items
                    .iter()
                    .map(|it| {
                        let mut m: ItemValues = BTreeMap::new();
                        for (i, f) in plan.fields.iter().enumerate() {
                            if let Some(Availability::Present(v)) = it.values.get(i).copied() {
                                m.insert(f.name, v);
                            }
                        }
                        m
                    })
                    .collect();
                sample.insert(act.id.0, items);
            }
            out.push(sample);
        }
        Ok(ScanControl::Continue)
    })
    .unwrap_or_else(|e| panic!("{} を走査できない: {e}", file.path().display()));
    out
}

/// 変換で**意味が変わる**と仕様が定めているフィールド (§5.8)。
///
/// ここに挙げたものだけは値の一致を要求しない。挙げていないフィールドが
/// 食い違ったら変換の誤りである。
fn intentional_difference(id: u32, field: &str) -> Option<&'static str> {
    match (id, field) {
        // `A_SERIAL` の `line` は基点が 1 → 0 に変わる。
        (10, "line") => Some("line の基点が 1 起点 → 0 起点"),
        _ => None,
    }
}

// ===========================================================================
// 1. 往復
// ===========================================================================

/// 変換結果が現行形式として読み直せ、レコードの骨格が元と対応すること。
#[test]
#[ignore = "本家データ (GPL) が必要。make fixtures 後 --include-ignored で実行する"]
fn converted_files_round_trip_as_current_format() {
    let Some(dir) = upstream_or_skip("converted_files_round_trip_as_current_format") else {
        return;
    };

    for case in CONV_CASES {
        let Some(path) = upstream_file(&dir, case.data) else {
            continue;
        };
        let src = SaFile::open(&path).unwrap_or_else(|e| panic!("{} を開けない: {e}", case.data));
        let (bytes, report) = convert_bytes(&src, None);

        println!(
            "[{}] {} -> {} バイト / HZ={} ({}) / cpu_nr={} / records: stats={} restart={} comment={}",
            case.upstream_test,
            case.data,
            report.bytes_written,
            report.hz,
            report.hz_source.describe(),
            report.cpu_nr,
            report.stats_records,
            report.restart_records,
            report.comment_records
        );
        for w in &report.warnings {
            println!("    [warn] {w}");
        }
        assert!(!report.already_current);
        assert_eq!(report.bytes_written as usize, bytes.len());

        let dst = reopen(case.data, bytes);

        // --- ヘッダ ---
        assert_eq!(
            dst.magic().format_magic,
            convert::OUT_FORMAT_MAGIC,
            "{}: 変換後は現行世代になること",
            case.data
        );
        assert_eq!(
            dst.magic().upgraded,
            Some(convert::OUT_UPGRADED),
            "{}: upgraded が非 0 でなければ「変換済み」と読まれない",
            case.data
        );
        assert_eq!(
            dst.magic().version,
            src.magic().version,
            "{}: 版数は元ファイルのものを保つ (§5.3)",
            case.data
        );
        assert_eq!(dst.magic().header_size, Some(336));
        assert_eq!(dst.magic().hdr_types_nr, Some([1, 1, 12]));
        assert_eq!(dst.header().act_size, Some(36));
        assert_eq!(dst.header().rec_size, Some(24));
        assert_eq!(dst.header().act_types_nr, Some([0, 0, 9]));
        assert_eq!(dst.header().rec_types_nr, Some([2, 0, 1]));
        assert_eq!(dst.header().extra_next, Some(0));
        assert_eq!(
            dst.header().tzname.as_deref(),
            Some(""),
            "{}: 元ファイルに TZ 情報が無いので空文字列 (§5.4)",
            case.data
        );

        // --- 元ファイルのバイト順と ABI を保つ (§5.3) ---
        assert_eq!(
            dst.encoding(),
            src.encoding(),
            "{}: 変換後もバイト順と ABI を保つ",
            case.data
        );
        assert_eq!(dst.header().sizeof_long, src.header().sizeof_long);

        // --- 日付・ホスト名 ---
        assert_eq!(dst.header().ust_time, src.header().ust_time);
        assert_eq!(
            (dst.header().year, dst.header().month, dst.header().day),
            (src.header().year, src.header().month, src.header().day),
            "{}: 日付が保たれること (sa_year は 1900 起点、sa_month は 0 起点で書く)",
            case.data
        );
        assert_eq!(dst.header().nodename, src.header().nodename);
        assert_eq!(dst.header().machine, src.header().machine);
        assert_eq!(dst.header().release, src.header().release);
        assert_eq!(dst.header().sysname, src.header().sysname);
        assert_eq!(dst.header().hz, Some(report.hz));
        assert_eq!(dst.header().cpu_nr, Some(report.cpu_nr));

        // --- activity リスト: 並び替えない、id は 1:1 (§5.5) ---
        assert_eq!(dst.header().act_nr, src.header().act_nr);
        assert_eq!(dst.activities().len(), src.activities().len());
        for (a, b) in src.activities().iter().zip(dst.activities()) {
            assert_eq!(a.id, b.id, "{}: activity の順序は保たれること", case.data);
            if b.id == ActivityId::IRQ {
                // 1 次元 → 2 次元行列 (§5.5)
                assert_eq!(b.nr, 1, "A_IRQ の nr は CPU \"all\" のみの 1");
                assert_eq!(b.nr2, a.nr, "A_IRQ の nr2 に旧 nr (割り込み数) が入る");
            } else {
                assert_eq!(b.nr, a.nr);
                assert_eq!(b.nr2, a.nr2);
            }
        }

        // --- レコードの骨格 ---
        let (sh_src, sh_dst) = (record_shapes(&src), record_shapes(&dst));
        assert_eq!(
            sh_src.len(),
            sh_dst.len(),
            "{}: レコード数が一致すること",
            case.data
        );
        for (i, (a, b)) in sh_src.iter().zip(&sh_dst).enumerate() {
            assert_eq!(a.kind, b.kind, "{}: レコード {i} の種別", case.data);
            assert_eq!(a.ust_time, b.ust_time, "{}: レコード {i} の時刻", case.data);
            assert_eq!(a.hms, b.hms, "{}: レコード {i} の時分秒", case.data);
            assert_eq!(
                a.comment, b.comment,
                "{}: レコード {i} のコメント",
                case.data
            );
            if a.kind == "restart" {
                // 旧 `0x2171` は RESTART にペイロードを持たないので、
                // 変換後にはじめて CPU 数が入る (§5.7)。
                assert_eq!(
                    b.cpu_count,
                    Some(report.cpu_nr),
                    "{}: RESTART の CPU 数",
                    case.data
                );
            }
            // item 数は「番兵で切り詰めた件数」になるので、元より減ることがある。
            assert_eq!(
                a.items.len(),
                b.items.len(),
                "{}: レコード {i} の activity 数",
                case.data
            );
            for ((ia, na, n2a), (ib, nb, n2b)) in a.items.iter().zip(&b.items) {
                assert_eq!(ia, ib, "{}: レコード {i} の activity 順序", case.data);
                if *ib == ActivityId::IRQ.0 {
                    assert_eq!(*nb, 1);
                    assert_eq!(*n2b, *na, "A_IRQ は割り込み数が nr2 へ移る");
                } else {
                    assert!(
                        nb <= na,
                        "{}: レコード {i} の activity {ib}: 変換後の item 数 {nb} が \
                         元の {na} を超えた (番兵での切り詰めしか起こらないはず)",
                        case.data
                    );
                    assert_eq!(n2a, n2b);
                }
            }
        }

        // --- ファイル末尾まで余りなく読めること ---
        let summary = dst.scan(|_| Ok(ScanControl::Continue)).unwrap();
        assert!(
            summary.is_exact(),
            "{}: 変換結果をファイル末尾まで余りなく読めること (残 {} バイト)",
            case.data,
            summary.file_size.saturating_sub(summary.end_offset)
        );

        // --- 「変換済み」と読めること ---
        //
        // `upgraded` が非 0 なら本家 `sadf -H` は `Genuine sa datafile: no` を出す。
        // 既存の表示経路がそう読むことまで確かめる (値を書いただけでは足りない)。
        let mut hdr: Vec<u8> = Vec::new();
        sadf::header::write_header(&mut hdr, &dst).expect("sadf -H 相当を書ける");
        let hdr = String::from_utf8(hdr).expect("sadf -H の出力は UTF-8");
        assert!(
            hdr.contains(&format!(
                "Genuine sa datafile: no ({:x})",
                convert::OUT_UPGRADED
            )),
            "{}: 変換済みとして表示されること:\n{hdr}",
            case.data
        );
    }
}

// ===========================================================================
// 2. 値の一致
// ===========================================================================

/// 変換前を直読した値と、変換後を読んだ値が全 activity・全 item・全フィールドで一致すること。
///
/// これが本家 `upgrade_stats_*` 18 本に相当する正しさを固定する。
/// reSARch は「旧 revision でフィールドを読み、同名フィールドを現行 revision の位置へ書く」
/// という 1 本の経路しか持たないので、ここが通れば 18 本ぶんの対応が正しい。
#[test]
#[ignore = "本家データ (GPL) が必要。make fixtures 後 --include-ignored で実行する"]
fn converted_values_match_direct_read() {
    let Some(dir) = upstream_or_skip("converted_values_match_direct_read") else {
        return;
    };

    for case in CONV_CASES {
        let Some(path) = upstream_file(&dir, case.data) else {
            continue;
        };
        let src = SaFile::open(&path).unwrap_or_else(|e| panic!("{} を開けない: {e}", case.data));
        let (bytes, _) = convert_bytes(&src, None);
        let dst = reopen(case.data, bytes);

        let before = collect_values(&src);
        let after = collect_values(&dst);
        assert_eq!(
            before.len(),
            after.len(),
            "{}: 統計レコード数が一致すること",
            case.data
        );

        let mut compared = 0usize;
        let mut skipped_fields: BTreeMap<String, &'static str> = BTreeMap::new();
        let mut new_activities: Vec<u32> = Vec::new();

        for (r, (b, a)) in before.iter().zip(&after).enumerate() {
            for (id, b_items) in b {
                let Some(a_items) = a.get(id) else {
                    // 直読できていた activity が変換後に読めなくなったら変換の誤り。
                    panic!(
                        "{}: レコード {r} の activity {id} が変換後に読めない",
                        case.data
                    );
                };
                // 番兵で切り詰めた分だけ少なくなる。
                let n = a_items.len().min(b_items.len());
                for i in 0..n {
                    for (name, bv) in &b_items[i] {
                        let Some(av) = a_items[i].get(name) else {
                            // 旧にあって新に無いフィールドは「現行形式が捨てた」もの。
                            // 現状そのような統計フィールドは無いので、出たら知らせる。
                            panic!(
                                "{}: レコード {r} の activity {id} item {i}: \
                                 フィールド {name} が変換後に存在しない",
                                case.data
                            );
                        };
                        if let Some(why) = intentional_difference(*id, name) {
                            skipped_fields.insert(format!("activity {id} の {name}"), why);
                            continue;
                        }
                        assert_eq!(
                            bv, av,
                            "{}: レコード {r} の activity {id} item {i} の {name} が \
                             変換で変わった (直読 {bv} → 変換後 {av})",
                            case.data
                        );
                        compared += 1;
                    }
                }
            }
            for id in a.keys() {
                if !b.contains_key(id) && !new_activities.contains(id) {
                    new_activities.push(*id);
                }
            }
        }

        println!(
            "[{}] {}: {compared} 個のフィールド値が一致",
            case.upstream_test, case.data
        );
        for (what, why) in &skipped_fields {
            println!("    [意図的な差] {what}: {why}");
        }
        if !new_activities.is_empty() {
            println!(
                "    [補足] 変換後にだけ読めるようになった activity: {new_activities:?} \
                 (旧 magic では revision を確定できなかったもの)"
            );
        }
        assert!(compared > 0, "{}: 1 つも比較していない", case.data);
    }
}

// ===========================================================================
// 3. 本家期待出力との一致
// ===========================================================================

/// 変換結果に `sar -C -A` 相当を当てた出力が `expected.data-*` と一致すること。
///
/// 本家の期待出力そのものが「`sadf -c` の結果を `sar -C -A` で読んだもの」なので、
/// これが変換の正しさの外部基準になる。
#[test]
#[ignore = "本家データ (GPL) が必要。make fixtures 後 --include-ignored で実行する"]
fn converted_output_matches_upstream_expected() {
    let Some(dir) = upstream_or_skip("converted_output_matches_upstream_expected") else {
        return;
    };

    let mut failures: Vec<String> = Vec::new();

    for case in CONV_CASES {
        let (Some(data), Some(golden)) = (
            upstream_file(&dir, case.data),
            upstream_file(&dir, case.golden),
        ) else {
            continue;
        };
        let expected = match std::fs::read_to_string(&golden) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("skipped: {} を読めない: {e}", case.golden);
                continue;
            }
        };

        let src = SaFile::open(&data).unwrap_or_else(|e| panic!("{} を開けない: {e}", case.data));
        let (bytes, report) = convert_bytes(&src, None);

        // 変換結果はファイルに書き出す。`sar -f` 相当の引数解析も通すため。
        let tmp = dir.parent().unwrap_or(&dir).join("converted");
        if let Err(e) = std::fs::create_dir_all(&tmp) {
            eprintln!("skipped: 一時ディレクトリを作れない: {e}");
            continue;
        }
        let out_path = tmp.join(format!("{}.2175", case.data));
        if let Err(e) = std::fs::write(&out_path, &bytes) {
            eprintln!("skipped: 変換結果を書けない: {e}");
            continue;
        }

        let dst = reopen(case.data, bytes);
        let actual = match render_sar_text(&dst, &out_path, &["-C", "-A"]) {
            Ok(s) => s,
            Err(e) => {
                failures.push(format!("[{}] {}: {e}", case.upstream_test, case.data));
                continue;
            }
        };

        let cmp = golden::compare(&expected, &actual, &[Mask::DiskDeviceName]);
        println!(
            "[{}] {} (変換: HZ={} / {}) -> {}: {}",
            case.upstream_test,
            case.upstream_cmd,
            report.hz,
            report.hz_source.describe(),
            case.golden,
            cmp.verdict()
        );
        // どの語を潰したかは報告に必ず出す (理由ごとにまとめる)。
        print!("{}", cmp.mask_report());
        if !cmp.is_match() {
            // 行単位の要約では追えない差 (ブロックの増減) を diff で追えるようにする。
            let dump = tmp.join(format!("{}.actual", case.golden));
            let _ = std::fs::write(&dump, &actual);
            println!("    実際の出力: {}", dump.display());
            println!("{}", cmp.diff_report(12));
            failures.push(format!(
                "[{}] {} -> {}: {}",
                case.upstream_test,
                case.data,
                case.golden,
                cmp.verdict()
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "変換結果の出力が本家の期待出力と一致しないケースがある:\n{}",
        failures.join("\n")
    );
}

/// `-O hz=250` を明示した変換結果の出力が `expected.data-9.1.6-hz` と一致すること。
///
/// 本家テスト `00602` / `00608` の組
/// (`sadf -c -O hz=250 data-9.1.6 > tmp && sar -f tmp`) に対応する。
///
/// HZ は `sa_hz` と `uptime_cs` に効くが、CPU 使用率は tick の差分から出るので
/// **表示値は HZ に依らない**。ここが一致することは「HZ の指定が
/// 統計値の変換に混入していない」ことの確認になる。
#[test]
#[ignore = "本家データ (GPL) が必要。make fixtures 後 --include-ignored で実行する"]
fn explicit_hz_conversion_matches_upstream_expected() {
    let Some(dir) = upstream_or_skip("explicit_hz_conversion_matches_upstream_expected") else {
        return;
    };
    let (Some(data), Some(golden)) = (
        upstream_file(&dir, "data-9.1.6"),
        upstream_file(&dir, "expected.data-9.1.6-hz"),
    ) else {
        return;
    };
    let Ok(expected) = std::fs::read_to_string(&golden) else {
        eprintln!("skipped: expected.data-9.1.6-hz を読めない");
        return;
    };

    let src = SaFile::open(&data).expect("data-9.1.6 を開ける");
    let (bytes, report) = convert_bytes(&src, Some(250));
    assert_eq!(report.hz, 250);
    assert_eq!(report.hz_source, HzSource::Explicit);

    let tmp = dir.parent().unwrap_or(&dir).join("converted");
    if let Err(e) = std::fs::create_dir_all(&tmp) {
        eprintln!("skipped: 一時ディレクトリを作れない: {e}");
        return;
    }
    let out_path = tmp.join("data-9.1.6-hz250.2175");
    if let Err(e) = std::fs::write(&out_path, &bytes) {
        eprintln!("skipped: 変換結果を書けない: {e}");
        return;
    }

    let dst = reopen("data-9.1.6-hz", bytes);
    assert_eq!(dst.header().hz, Some(250));

    // 本家 `tests/00608` は activity 指定なし = 既定の `-u`。
    let actual = render_sar_text(&dst, &out_path, &[]).expect("sar 既定出力を作れる");
    let cmp = golden::compare(&expected, &actual, &[]);
    println!(
        "[00608] sadf -c -O hz=250 data-9.1.6 > tmp && sar -f tmp -> expected.data-9.1.6-hz: {}",
        cmp.verdict()
    );
    if !cmp.is_match() {
        println!("{}", cmp.diff_report(12));
    }
    assert!(
        cmp.is_match(),
        "-O hz=250 の変換結果の出力が本家の期待出力と一致しない"
    );
}

/// `sar` 互換テキストを生成する (`tests/conformance.rs` の同名関数と同じ作り)。
///
/// プロセスは起動しない。`TZ=GMT` は [`TimeStyle::Utc`] で表す。
fn render_sar_text(file: &SaFile, path: &Path, args: &[&str]) -> Result<String, String> {
    let mut argv: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
    argv.push("-f".to_string());
    argv.push(path.display().to_string());
    let parsed = parse_sar_args(&argv).map_err(|e| format!("引数 {args:?} を解析できない: {e}"))?;

    let opts = sar_text_options(&parsed);
    let acts = selected_activities(file, &parsed);
    let mut buf: Vec<u8> = Vec::new();
    sar_text::write_report(&mut buf, file, &opts, &acts)
        .map_err(|e| format!("sar テキストを書けない: {e}"))?;
    String::from_utf8(buf).map_err(|e| format!("出力が UTF-8 でない: {e}"))
}

/// [`SarOptions`] を [`SarTextOptions`] へ写す。
///
/// **フィールドは `..Default::default()` で省略しない** (新しい出力オプションが
/// 既定値のまま無視されて再現が静かに崩れるのを防ぐ)。
fn sar_text_options(o: &SarOptions) -> SarTextOptions {
    assert!(
        o.tm_start.is_none() && o.tm_end.is_none(),
        "-s / -e を使うケースは時刻フィルタの写しが必要"
    );
    assert!(
        o.item_lists.is_empty(),
        "item リストを使うケースは item フィルタの写しが必要"
    );

    let bitmap = &o.cpu_bitmap;
    let cpus = if bitmap.count_bits() == bitmap.capacity_bits() {
        CpuSelection::All
    } else if bitmap.aggregate_selected() && bitmap.selected_cpus().next().is_none() {
        CpuSelection::Aggregate
    } else {
        CpuSelection::Listed {
            aggregate: bitmap.aggregate_selected(),
            cpus: bitmap.selected_cpus().collect(),
        }
    };

    let mem = o.opt_flags(Activity::Memory);
    SarTextOptions {
        pretty: o.flags.pretty,
        human: o.flags.human,
        dec_places: o.dec_places,
        comment: o.flags.comment,
        minmax: o.flags.minmax,
        zero_omit: o.flags.zero_omit,
        cpu_all: o.opt_flags(Activity::Cpu).contains(OptFlags::CPU_ALL),
        memory: mem.contains(OptFlags::MEMORY),
        mem_all: mem.contains(OptFlags::MEM_ALL),
        swap: mem.contains(OptFlags::SWAP),
        mount: o.opt_flags(Activity::Fs).contains(OptFlags::MOUNT),
        dev_sid: o.flags.dev_sid,
        time: if o.flags.true_time {
            TimeStyle::Recorded
        } else {
            TimeStyle::Utc
        },
        // 本家の期待出力は `LC_ALL=C` かつ標準出力がパイプの前提なので、
        // `S_TIME_FORMAT` / `S_REPEAT_HEADER` は効いていない状態に固定する。
        date_format: CompatDateFormat::Locale,
        header_rows: HeaderRows::default(),
        cpus,
        time_filter: TimeFilter::default(),
        item_names: BTreeMap::new(),
    }
}

/// 選択された activity を**ファイル記載順**で返す (本家の `id_seq[]` と同じ順序)。
fn selected_activities(file: &SaFile, o: &SarOptions) -> Vec<ActivityId> {
    let selected: Vec<ActivityId> = o
        .selected_activities()
        .map(|a| ActivityId(u32::from(a.id())))
        .collect();
    sar_text::activities_in_file(file)
        .into_iter()
        .filter(|id| selected.contains(id))
        .collect()
}

// ===========================================================================
// 4. 拒否と「既に最新」
// ===========================================================================

/// `0x2170` は変換できず、`0x2175` は 1 バイトも書かないこと (§5.1)。
#[test]
#[ignore = "本家データ (GPL) が必要。make fixtures 後 --include-ignored で実行する"]
fn unconvertible_and_current_formats_are_handled_upstream() {
    let Some(dir) = upstream_or_skip("unconvertible_and_current_formats_are_handled_upstream")
    else {
        return;
    };

    // `0x2170` (sysstat 9.1.5): activity 単位の magic が無いので変換できない。
    // 本家も `Cannot convert the format of this file` で終了する。
    if let Some(path) = upstream_file(&dir, "data-9.1.5") {
        let file = SaFile::open(&path).expect("0x2170 は直読できる");
        assert_eq!(file.magic().format_magic, 0x2170);
        let mut out: Vec<u8> = Vec::new();
        let err = convert::convert(&file, &ConvertOptions::default(), &mut out)
            .expect_err("0x2170 は変換できないこと");
        assert!(out.is_empty(), "拒否したときは 1 バイトも書かないこと");
        let msg = err.to_string();
        assert!(
            msg.contains("変換できません"),
            "変換不能として報告すること: {msg}"
        );
        println!("[01452] data-9.1.5 (0x2170): {msg}");
    }

    // `0x2175`: 既に最新なので何も書かない (本家は
    // `File format already up-to-date` を stderr に出して stdout は空)。
    for name in ["data-12.0.0", "data-ppc-11.7.2"] {
        let Some(path) = upstream_file(&dir, name) else {
            continue;
        };
        let file = SaFile::open(&path).expect("現行世代は読める");
        let mut out: Vec<u8> = Vec::new();
        let report = convert::convert(&file, &ConvertOptions::default(), &mut out)
            .expect("既に最新でもエラーにしない");
        assert!(report.already_current);
        assert!(
            out.is_empty(),
            "{name}: 既に最新の形式なら 1 バイトも書かないこと"
        );
        assert_eq!(report.bytes_written, 0);
        println!("[00600] {name} (0x2175): 変換不要 (出力 0 バイト)");
    }
}

// ===========================================================================
// 5. 自作 fixture (本家データが無くても走る)
// ===========================================================================

/// 変換対象の世代 × ABI の全組み合わせ。
fn convertible_generations() -> Vec<(Generation, FixtureAbi)> {
    let mut out = Vec::new();
    for g in [Generation::G2171, Generation::G2173] {
        for abi in FixtureAbi::ALL {
            out.push((g, abi));
        }
    }
    out
}

/// `A_MEMORY` の時代 A (magic `0x8a` / 17 × `unsigned long` / 136 バイト)。
///
/// 現行 revision は同じ 17 フィールドが `unsigned long long` (+ `availablekb` の
/// 後ろに `shmemkb`) なので、**`unsigned long` → `unsigned long long` の拡幅**を
/// 通る唯一の最小構成になる。32bit ファイルではスロット 8 バイトのうち
/// 先頭 4 バイトだけが値なので、バイト列のままコピーすると
/// ビッグエンディアンで `100` が `100 * 2^32` に化ける (§5.13)。
fn a_memory_old() -> ActivitySpec {
    ActivitySpec {
        id: 7,
        magic: 0x8a,
        nr: 1,
        nr2: 1,
        has_nr: false,
        size: 136,
        types_nr: [0, 17, 0],
    }
}

/// `A_IRQ` の時代 A (magic `0x8a` / `aligned(16)` で 1 本 16 バイト)。
///
/// 変換で 1 次元 (割り込み数) → 2 次元行列 (CPU × 割り込み) に付け替わる。
fn a_irq_old(nr: i32) -> ActivitySpec {
    ActivitySpec {
        id: 3,
        magic: 0x8a,
        nr,
        nr2: 1,
        has_nr: false,
        size: 16,
        types_nr: [1, 0, 0],
    }
}

/// `A_CPU` の時代 A (magic `0x8a` / `aligned(16)` × 10 = 160 バイト)。
fn a_cpu_old(nr: i32) -> ActivitySpec {
    ActivitySpec {
        id: 1,
        magic: 0x8a,
        nr,
        nr2: 1,
        has_nr: false,
        size: 160,
        types_nr: [10, 0, 0],
    }
}

/// 旧世代の自作 fixture を組む。
fn old_fixture(
    generation: Generation,
    abi: FixtureAbi,
    acts: Vec<ActivitySpec>,
) -> fixtures::Fixture {
    let mut spec = FixtureSpec::skeleton(generation, abi);
    let cpu_nr = spec.cpu_nr as i32;
    spec.activities = acts;
    // 件数は has_nr が偽なので使われないが、レコードの形を揃えるために渡す。
    let counts = vec![0; spec.activities.len()];

    // RESTART / COMMENT の `uptime0` は実ファイルでも 0 (§5.6 の注記)。
    let mut restart = RecordSpec::restart(cpu_nr, 1_600_000_001, 12, 26, 41)
        .with_volatile(vec![a_cpu_old(cpu_nr)]);
    restart.uptime = 0;
    let mut comment = RecordSpec::comment("resarch convert fixture", 1_600_000_021, 12, 27, 1);
    comment.uptime = 0;

    // 統計レコードは 20 秒間隔。`uptime0` の差を 2000 jiffies にしておくと
    // 「既定 USER_HZ = 100」になる (2000 / 20)。
    let mut first = RecordSpec::stats(counts.clone(), 1_600_000_011, 12, 26, 51);
    first.uptime = 100_000;
    let mut second = RecordSpec::stats(counts, 1_600_000_031, 12, 27, 11);
    second.uptime = 102_000;

    spec.records = vec![restart, first, comment, second];
    fixtures::build(spec)
}

/// `unsigned long` → `unsigned long long` の拡幅が 4 通りの符号化すべてで正しいこと。
///
/// バイト列のままコピーする実装だと、**ビッグエンディアン 32bit** で
/// 値が `2^32` 倍になる。本家がこれを `moveto_long_long()` の 32 ビット回転で
/// 回避しているのに対し、reSARch は値を `u64` へ正規化してから再直列化する (§5.13)。
#[test]
fn narrow_unsigned_long_is_reserialized() {
    for (generation, abi) in convertible_generations() {
        let cpu_nr = 3;
        let fx = old_fixture(generation, abi, vec![a_cpu_old(cpu_nr), a_memory_old()]);
        let label = format!("{}/{}", generation.name(), abi.name());
        let src = SaFile::from_bytes(&label, fx.bytes.clone())
            .unwrap_or_else(|e| panic!("{label}: 自作 fixture を開けない: {e}"));

        let (bytes, report) = convert_bytes(&src, Some(100));
        let dst = reopen(&label, bytes);
        assert_eq!(dst.magic().format_magic, convert::OUT_FORMAT_MAGIC);
        assert_eq!(dst.header().sizeof_long, src.header().sizeof_long);
        assert_eq!(dst.encoding(), src.encoding());
        assert_eq!(
            report.truncated_values, 0,
            "{label}: 値の切り詰めは無いこと"
        );

        let before = collect_values(&src);
        let after = collect_values(&dst);
        assert_eq!(before.len(), after.len(), "{label}: 統計レコード数");
        assert!(!before.is_empty(), "{label}: 統計レコードが無い");

        for (r, (b, a)) in before.iter().zip(&after).enumerate() {
            let (Some(bm), Some(am)) = (b.get(&7), a.get(&7)) else {
                panic!("{label}: レコード {r} の A_MEMORY が読めない");
            };
            assert_eq!(bm.len(), am.len(), "{label}: A_MEMORY の item 数");
            for (i, (bi, ai)) in bm.iter().zip(am).enumerate() {
                assert!(
                    !bi.is_empty(),
                    "{label}: レコード {r} item {i}: 旧側が 1 つも読めていない"
                );
                for (name, v) in bi {
                    let got = ai.get(name).copied();
                    assert_eq!(
                        got,
                        Some(*v),
                        "{label}: レコード {r} item {i} の {name}: \
                         unsigned long ({} バイト) → unsigned long long の拡幅で値が変わった",
                        src.header().sizeof_long
                    );
                }
            }
        }
    }
}

/// `A_IRQ` の 1 次元 → 2 次元の付け替えと `irq_name` の生成 (§5.5 / §5.8)。
#[test]
fn irq_dimensions_are_swapped_and_names_are_generated() {
    // 割り込み 4 本 + 総和スロット。
    const IRQ_NR: i32 = 5;

    for (generation, abi) in convertible_generations() {
        let cpu_nr = 3;
        let fx = old_fixture(generation, abi, vec![a_cpu_old(cpu_nr), a_irq_old(IRQ_NR)]);
        let label = format!("{}/{}", generation.name(), abi.name());
        let src = SaFile::from_bytes(&label, fx.bytes.clone())
            .unwrap_or_else(|e| panic!("{label}: 自作 fixture を開けない: {e}"));

        let (bytes, _) = convert_bytes(&src, Some(100));
        let dst = reopen(&label, bytes);

        let irq = dst
            .activities()
            .iter()
            .find(|a| a.id == ActivityId::IRQ)
            .unwrap_or_else(|| panic!("{label}: 変換後に A_IRQ が無い"));
        assert_eq!(irq.nr, 1, "{label}: 行は CPU \"all\" だけ");
        assert_eq!(irq.nr2, IRQ_NR, "{label}: 列が割り込み数になる");
        assert_eq!(irq.magic, 0x8c, "{label}: 現行 magic を書く");
        assert!(irq.has_nr, "{label}: A_IRQ は AO_COUNTED");
        assert_eq!(irq.size, 12, "{label}: 現行 stats_irq は 12 バイト");

        // 名前は添字 0 が "sum"、以降が割り込み番号の 10 進表記。
        let mut names: Vec<String> = Vec::new();
        let mut values: Vec<u64> = Vec::new();
        walk_items(&dst, &Selection::Only(vec![ActivityId::IRQ]), |item| {
            if let WalkItem::Sample(view) = item
                && names.is_empty()
                && let Some(act) = view.curr.activity(ActivityId::IRQ)
                && let Some(plan) = view.plan_for(ActivityId::IRQ)
            {
                let name_at = plan.text_index("irq_name");
                let nr_at = plan
                    .fields
                    .iter()
                    .position(|f| f.name == "irq_nr")
                    .expect("irq_nr は現行 revision にある");
                for it in &act.items {
                    names.push(
                        name_at
                            .and_then(|i| it.text(i))
                            .unwrap_or_default()
                            .to_string(),
                    );
                    if let Some(Availability::Present(v)) = it.values.get(nr_at).copied() {
                        values.push(v);
                    }
                }
            }
            Ok(ScanControl::Continue)
        })
        .unwrap_or_else(|e| panic!("{label}: 変換結果を走査できない: {e}"));

        assert_eq!(
            names,
            vec!["sum", "0", "1", "2", "3"],
            "{label}: 旧形式は「割り込み番号 = 配列添字」なので名前を生成する"
        );
        assert_eq!(
            values.len(),
            IRQ_NR as usize,
            "{label}: 全列の irq_nr が読めること"
        );
    }
}

/// HZ を明示しなければ USER_HZ=100 を使い、明示すればその値を使うこと。
#[test]
fn hz_defaults_to_user_hz_or_is_taken_from_the_option() {
    let fx = old_fixture(
        Generation::G2173,
        FixtureAbi::Le64,
        vec![a_cpu_old(3), a_memory_old()],
    );
    let src = SaFile::from_bytes("hz", fx.bytes.clone()).expect("自作 fixture を開ける");

    // 旧形式に保存されない単位は USER_HZ=100 とする。
    let (bytes, report) = convert_bytes(&src, None);
    assert_eq!(report.hz, 100, "既定 USER_HZ");
    assert!(
        matches!(report.hz_source, HzSource::Fallback),
        "既定 USER_HZ を使うこと: {:?}",
        report.hz_source
    );
    let dst = reopen("hz", bytes);
    assert_eq!(dst.header().hz, Some(100));

    // 明示指定はそのまま使われ、`uptime_cs` の換算にも効く。
    let (bytes_250, report_250) = convert_bytes(&src, Some(250));
    assert_eq!(report_250.hz, 250);
    assert_eq!(report_250.hz_source, HzSource::Explicit);
    let dst_250 = reopen("hz250", bytes_250);
    assert_eq!(dst_250.header().hz, Some(250));

    // `uptime_cs = uptime0 * 100 / HZ`。HZ が 2.5 倍なら換算後は 1/2.5 になる。
    let cs = |f: &SaFile| -> Vec<u64> {
        let mut v = Vec::new();
        f.scan(|rec| {
            v.push(rec.uptime_cs.unwrap_or(0));
            Ok(ScanControl::Continue)
        })
        .expect("走査できる");
        v
    };
    let a = cs(&dst);
    let b = cs(&dst_250);
    assert_eq!(a.len(), b.len());
    for (x, y) in a.iter().zip(&b) {
        assert_eq!(*y, x * 100 / 250, "HZ の違いが uptime_cs に効くこと");
    }
}

/// 再起動で uptime0 が戻っても既定の USER_HZ は変わらない。
#[test]
fn default_hz_is_unchanged_by_a_restart() {
    let mut spec = FixtureSpec::skeleton(Generation::G2173, FixtureAbi::Le64);
    let cpu_nr = spec.cpu_nr as i32;
    spec.activities = vec![a_cpu_old(cpu_nr), a_memory_old()];
    let counts = vec![0; spec.activities.len()];

    // 再起動前: uptime0 = 500_000 → 502_000 (20 秒で 2000 jiffies = 100 Hz)
    let mut a = RecordSpec::stats(counts.clone(), 1_600_000_000, 12, 0, 0);
    a.uptime = 500_000;
    let mut b = RecordSpec::stats(counts.clone(), 1_600_000_020, 12, 0, 20);
    b.uptime = 502_000;
    // 再起動でカウンタが 0 に戻る
    let mut restart = RecordSpec::restart(cpu_nr, 1_600_000_040, 12, 0, 40)
        .with_volatile(vec![a_cpu_old(cpu_nr)]);
    restart.uptime = 0;
    // 再起動後: uptime0 = 1_000 → 3_000 (20 秒で 2000 jiffies)
    let mut c = RecordSpec::stats(counts.clone(), 1_600_000_060, 12, 1, 0);
    c.uptime = 1_000;
    let mut d = RecordSpec::stats(counts, 1_600_000_080, 12, 1, 20);
    d.uptime = 3_000;

    spec.records = vec![a, b, restart, c, d];
    let fx = fixtures::build(spec);
    let src = SaFile::from_bytes("restart-hz", fx.bytes).expect("自作 fixture を開ける");

    let (_, report) = convert_bytes(&src, None);
    assert_eq!(
        report.hz, 100,
        "RESTART があっても既定 USER_HZ を使うこと ({:?})",
        report.hz_source
    );
    assert!(
        matches!(report.hz_source, HzSource::Fallback),
        "既定 USER_HZ を使うこと: {:?}",
        report.hz_source
    );
}

/// 変換結果は現行世代なので、もう一度変換しても何も書かない (冪等)。
#[test]
fn converting_twice_writes_nothing_the_second_time() {
    let fx = old_fixture(
        Generation::G2171,
        FixtureAbi::Le64,
        vec![a_cpu_old(3), a_memory_old()],
    );
    let src = SaFile::from_bytes("twice", fx.bytes.clone()).expect("自作 fixture を開ける");
    let (bytes, _) = convert_bytes(&src, Some(100));
    let dst = reopen("twice", bytes);

    let mut out: Vec<u8> = Vec::new();
    let report = convert::convert(&dst, &ConvertOptions::default(), &mut out)
        .expect("既に最新でもエラーにしない");
    assert!(report.already_current);
    assert!(out.is_empty());
}

/// `0x2170` (activity magic を持たない世代) は変換できない。
#[test]
fn format_2170_is_rejected() {
    for abi in FixtureAbi::ALL {
        let fx = fixtures::minimal(Generation::G2170, abi);
        let label = format!("2170/{}", abi.name());
        let file = match SaFile::from_bytes(&label, fx.bytes.clone()) {
            Ok(f) => f,
            // 0x2170 の自作 fixture が読めない ABI があっても、
            // ここで見たいのは「変換が拒否されること」なのでスキップする。
            Err(e) => {
                eprintln!("skipped: {label}: {e}");
                continue;
            }
        };
        let mut out: Vec<u8> = Vec::new();
        let err = convert::convert(&file, &ConvertOptions::default(), &mut out)
            .expect_err("0x2170 は変換できないこと");
        assert!(out.is_empty(), "{label}: 拒否時は 1 バイトも書かない");
        assert!(
            err.to_string().contains("変換できません"),
            "{label}: 変換不能として報告すること: {err}"
        );
    }
}

// ===========================================================================
// 6. item を狙って組んだ旧世代 fixture — 番兵・素通し・RESTART・異常系
// ===========================================================================
//
// 共有の独立ライタ ([`fixtures::build`]) は統計 item を型別個数の順に詰めるだけなので、
// 旧構造体の穴 (`aligned(16)`) を避けて番兵フィールドに値を置くことができない。
// `0x2173` の RESTART で item 数が変わっても、以降の統計レコードの件数は追従しない。
// そこで**ヘッダ 3 節 (`file_magic` / `file_header` / `file_activity[]`) だけを
// 共有ライタに書かせ**、レコード列はここで組む ([`OldFile`])。
//
// 旧構造体の item 配置は、本家 `sa_conv.h` の旧構造体宣言から手で導いた位置
// (`aligned(n)` は自然境界と n の大きい方、`packed` は直前のメンバの直後) で、
// サイズは `docs/format/01-file-format.md` §5.12 の表と一致する。
// 変換結果は `docs/format/01-file-format.md` §3 と `02-activities.md` §5 の
// オフセット表だけで読み直して確かめる ([`walk_output`])。
// 本体の `layouts` / `selfdesc` / `layout::activities` は参照しない。
//
// `RESARCH_CONVERT_FIXTURE_DIR` を与えると、入力と変換結果をそのディレクトリへ残す
// (本家 `sadf -c` とローカルで突き合わせるため。本家の出力はリポジトリに入れない)。

/// 旧世代 fixture のレコード時刻の基準 (2020-09-13T12:26:40Z)。
const T0: u64 = 1_600_000_000;

/// 旧 `record_header` のサイズ (§3.6.1)。
const OLD_RECORD_HEADER_SIZE: usize = 48;
/// 旧 `file_activity` (`0x2173` の volatile activity リストの 1 エントリ) のサイズ (§3.4.2)。
const OLD_FILE_ACTIVITY_SIZE: usize = 20;

/// 現行形の各節のサイズ (§3.2.4 / §3.3.5 / §3.4.3 / §3.6.3)。
const CUR_FILE_MAGIC_SIZE: usize = 76;
const CUR_FILE_HEADER_SIZE: usize = 336;
const CUR_FILE_ACTIVITY_SIZE: usize = 36;
const CUR_RECORD_HEADER_SIZE: usize = 24;

/// `R_LAST_STATS` (§6.1)。
const R_LAST_STATS: u8 = 3;

/// ファイルのバイト順と `unsigned long` の有効幅で値を読み書きする。
#[derive(Debug, Clone, Copy)]
struct Enc {
    big: bool,
    long_bytes: usize,
}

impl Enc {
    fn of(abi: FixtureAbi) -> Self {
        Self {
            big: abi.is_big_endian(),
            long_bytes: abi.long_bytes(),
        }
    }

    fn put_u32(self, b: &mut [u8], off: usize, v: u32) {
        let x = if self.big {
            v.to_be_bytes()
        } else {
            v.to_le_bytes()
        };
        b[off..off + 4].copy_from_slice(&x);
    }

    fn put_u64(self, b: &mut [u8], off: usize, v: u64) {
        let x = if self.big {
            v.to_be_bytes()
        } else {
            v.to_le_bytes()
        };
        b[off..off + 8].copy_from_slice(&x);
    }

    /// `unsigned long` は 8 バイトのスロットの先頭 `sa_sizeof_long` バイトだけが値 (§3.0)。
    fn put_ul(self, b: &mut [u8], off: usize, v: u64) {
        b[off..off + 8].fill(0);
        if self.long_bytes == 8 {
            self.put_u64(b, off, v);
        } else {
            self.put_u32(b, off, v as u32);
        }
    }

    fn u16_at(self, b: &[u8], off: usize) -> u16 {
        let x: [u8; 2] = b[off..off + 2].try_into().expect("2 バイト");
        if self.big {
            u16::from_be_bytes(x)
        } else {
            u16::from_le_bytes(x)
        }
    }

    fn u32_at(self, b: &[u8], off: usize) -> u32 {
        let x: [u8; 4] = b[off..off + 4].try_into().expect("4 バイト");
        if self.big {
            u32::from_be_bytes(x)
        } else {
            u32::from_le_bytes(x)
        }
    }

    fn u64_at(self, b: &[u8], off: usize) -> u64 {
        let x: [u8; 8] = b[off..off + 8].try_into().expect("8 バイト");
        if self.big {
            u64::from_be_bytes(x)
        } else {
            u64::from_le_bytes(x)
        }
    }

    fn ul_at(self, b: &[u8], off: usize) -> u64 {
        if self.long_bytes == 8 {
            self.u64_at(b, off)
        } else {
            u64::from(self.u32_at(b, off))
        }
    }
}

/// NUL 終端のバイト列フィールドを、NUL の手前まで読む。
fn text_at(b: &[u8], off: usize, cap: usize) -> &[u8] {
    let raw = &b[off..off + cap];
    let end = raw.iter().position(|&c| c == 0).unwrap_or(cap);
    &raw[..end]
}

/// 旧 item 1 個分のバイト列 (長さは申告サイズ。値を置かない位置は 0)。
struct OldItem {
    b: Vec<u8>,
    e: Enc,
}

impl OldItem {
    fn new(size: usize, e: Enc) -> Self {
        Self {
            b: vec![0; size],
            e,
        }
    }

    fn u32(mut self, off: usize, v: u32) -> Self {
        self.e.put_u32(&mut self.b, off, v);
        self
    }

    fn u64(mut self, off: usize, v: u64) -> Self {
        self.e.put_u64(&mut self.b, off, v);
        self
    }

    fn ul(mut self, off: usize, v: u64) -> Self {
        self.e.put_ul(&mut self.b, off, v);
        self
    }

    fn raw(mut self, off: usize, bytes: &[u8]) -> Self {
        self.b[off..off + bytes.len()].copy_from_slice(bytes);
        self
    }

    fn text(self, off: usize, s: &str) -> Self {
        self.raw(off, s.as_bytes())
    }

    fn done(self) -> Vec<u8> {
        self.b
    }
}

/// 旧 `file_activity` の 1 件。旧世代は `has_nr` / `types_nr` を持たない (§3.4.2)。
fn old_act(id: u32, magic: u32, size: i32, nr: i32) -> ActivitySpec {
    ActivitySpec {
        id,
        magic,
        nr,
        nr2: 1,
        has_nr: false,
        size,
        types_nr: [0, 0, 0],
    }
}

// --- 旧構造体の item (オフセットは `sa_conv.h` の宣言から導いたもの) ---

/// `stats_cpu_8a` (160 バイト): 10 個の ULL がそれぞれ `aligned(16)`。
/// cpu_user@0 / cpu_sys@32 / cpu_idle@48。
fn cpu_8a(e: Enc, user: u64, sys: u64, idle: u64) -> Vec<u8> {
    OldItem::new(160, e)
        .u64(0, user)
        .u64(32, sys)
        .u64(48, idle)
        .done()
}

/// `stats_disk_8b` (64 バイト): nr_ios ULL@0 (aligned 16) / rd_sect UL@16 (aligned 16) /
/// wr_sect UL@24 / rd_ticks u32@32 (aligned 8) / wr_ticks@36 / tot_ticks@40 / rq_ticks@44 /
/// major@48 / minor@52 (以上 packed)。
fn disk_8b(e: Enc, major: u32, minor: u32, nr_ios: u64) -> Vec<u8> {
    OldItem::new(64, e)
        .u64(0, nr_ios)
        .ul(16, nr_ios * 8)
        .ul(24, nr_ios * 4)
        .u32(32, 11)
        .u32(44, 44)
        .u32(48, major)
        .u32(52, minor)
        .done()
}

/// `stats_disk_8a` (80 バイト): rd_sect ULL@0 (aligned 16) / wr_sect ULL@16 (aligned 16) /
/// rd_ticks UL@32 (aligned 16) / wr_ticks UL@40 / tot_ticks UL@48 / rq_ticks UL@56 /
/// nr_ios UL@64 / major u32@72 (aligned 8) / minor u32@76 (packed)。
fn disk_8a(e: Enc, major: u32, minor: u32, rd_sect: u64) -> Vec<u8> {
    OldItem::new(80, e)
        .u64(0, rd_sect)
        .u64(16, 7)
        .ul(32, 3)
        .ul(64, 9)
        .u32(72, major)
        .u32(76, minor)
        .done()
}

/// `stats_net_dev_8c` (144 バイト): ULL × 7 (各 aligned 16) = rx_packets@0 … multicast@96 /
/// speed u32@112 (aligned 16) / interface[16]@116 (aligned 4) / duplex@132。
fn net_dev_8c(e: Enc, name: &str, rx_packets: u64) -> Vec<u8> {
    OldItem::new(144, e)
        .u64(0, rx_packets)
        .u64(96, rx_packets + 6)
        .u32(112, 1000)
        .text(116, name)
        .raw(132, &[2])
        .done()
}

/// `stats_net_edev_8b` (160 バイト): ULL × 9 (各 aligned 16) = collisions@0 … tx_carrier_errors@128 /
/// interface[16]@144 (aligned 16)。
fn net_edev_8b(e: Enc, name: &str, collisions: u64) -> Vec<u8> {
    OldItem::new(160, e)
        .u64(0, collisions)
        .u64(128, collisions + 8)
        .text(144, name)
        .done()
}

/// `stats_serial` (28 バイト、現行と同じ配置): rx@0 / tx@4 / frame@8 / parity@12 / brk@16 /
/// overrun@20 / line@24。
fn serial(e: Enc, line: u32, rx: u32) -> Vec<u8> {
    OldItem::new(28, e)
        .u32(0, rx)
        .u32(4, rx + 1)
        .u32(20, rx + 5)
        .u32(24, line)
        .done()
}

/// `stats_pwr_usb` (88 バイト、現行と同じ配置): bus_nr@0 / vendor_id@4 / product_id@8 /
/// bmaxpower@12 / manufacturer[24]@16 / product[48]@40。
fn pwr_usb(e: Enc, bus: u32, product: &str) -> Vec<u8> {
    OldItem::new(88, e)
        .u32(0, bus)
        .u32(4, 0x1d6b)
        .u32(8, 0x0002)
        .u32(12, 250)
        .text(16, "resarch")
        .text(40, product)
        .done()
}

/// `stats_filesystem_8a`: ULL × 5 (各 aligned 16) = f_blocks@0 / f_bfree@16 / f_bavail@32 /
/// f_files@48 / f_ffree@64、fs_name@80 (aligned 16)。160 バイト版は fs_name[72] で終わり
/// (`MAX_FS_LEN` = 72 時代)、336 バイト版は fs_name[128] の後に mountp[128]@208 を持つ。
fn fs_8a(e: Enc, size: usize, blocks: u64, name: &str, mountp: &str) -> Vec<u8> {
    let item = OldItem::new(size, e)
        .u64(0, blocks)
        .u64(16, blocks / 2)
        .u64(64, 123)
        .text(80, name);
    if size > 160 {
        item.text(208, mountp).done()
    } else {
        item.done()
    }
}

/// `stats_fchost` (48 バイト、現行と同じ配置): UL × 4 = f_rxframes@0 … f_txwords@24 /
/// fchost_name[16]@32。
fn fchost(e: Enc, name: &str, rxframes: u64) -> Vec<u8> {
    OldItem::new(48, e)
        .ul(0, rxframes)
        .ul(24, rxframes + 3)
        .text(32, name)
        .done()
}

/// `stats_irq_8a` (16 バイト): irq_nr ULL@0 (aligned 16)。
fn irq_8a(e: Enc, count: u64) -> Vec<u8> {
    OldItem::new(16, e).u64(0, count).done()
}

/// 旧世代 fixture の `seq` 番目のレコードの時刻 (10 秒間隔)。
fn old_record_time(seq: usize) -> u64 {
    T0 + 10 * seq as u64
}

/// 旧世代 fixture の `seq` 番目のレコードの `uptime0` (jiffies)。
///
/// USER_HZ = 100 で 10 秒 = 1000 jiffies ずつ進める。RESTART / COMMENT の `uptime0` は
/// 実ファイルでも 0 (§5.6 の注記)。
fn old_record_uptime0(seq: usize, record_type: u8) -> u64 {
    match record_type {
        fixtures::R_RESTART | fixtures::R_COMMENT => 0,
        _ => 100_000 + 1_000 * seq as u64,
    }
}

/// エポック秒の時分秒 (UTC)。
fn hms_of(t: u64) -> (u8, u8, u8) {
    let s = t % 86_400;
    ((s / 3600) as u8, (s % 3600 / 60) as u8, (s % 60) as u8)
}

/// 旧世代ファイルの 1 レコード。
enum OldRecord {
    /// 統計レコード。`items[i]` は activity リスト i 番目の item 列を連結したもの。
    ///
    /// `checked` が偽なら item 列の長さを申告値と突き合わせない (細工用)。
    Stats {
        record_type: u8,
        items: Vec<Vec<u8>>,
        checked: bool,
    },
    /// RESTART。`volatile` は `0x2173` だけが持つ volatile activity リスト。
    Restart {
        volatile: Vec<ActivitySpec>,
    },
    Comment {
        text: &'static str,
    },
}

/// 旧世代 (`0x2171` / `0x2173`) のファイルを組む。
struct OldFile {
    generation: Generation,
    abi: FixtureAbi,
    activities: Vec<ActivitySpec>,
    records: Vec<OldRecord>,
}

impl OldFile {
    fn new(generation: Generation, abi: FixtureAbi, activities: Vec<ActivitySpec>) -> Self {
        Self {
            generation,
            abi,
            activities,
            records: Vec::new(),
        }
    }

    fn enc(&self) -> Enc {
        Enc::of(self.abi)
    }

    fn label(&self) -> String {
        format!("{}/{}", self.generation.name(), self.abi.name())
    }

    fn stats(self, items: Vec<Vec<u8>>) -> Self {
        self.stats_as(fixtures::R_STATS, items)
    }

    fn stats_as(mut self, record_type: u8, items: Vec<Vec<u8>>) -> Self {
        self.records.push(OldRecord::Stats {
            record_type,
            items,
            checked: true,
        });
        self
    }

    fn stats_unchecked(mut self, items: Vec<Vec<u8>>) -> Self {
        self.records.push(OldRecord::Stats {
            record_type: fixtures::R_STATS,
            items,
            checked: false,
        });
        self
    }

    fn restart(mut self, volatile: Vec<ActivitySpec>) -> Self {
        self.records.push(OldRecord::Restart { volatile });
        self
    }

    fn comment(mut self, text: &'static str) -> Self {
        self.records.push(OldRecord::Comment { text });
        self
    }

    fn bytes(&self) -> Vec<u8> {
        self.bytes_with_boundaries().0
    }

    /// バイト列と、レコードの境界 (先頭レコードの開始位置と各レコードの終端) を返す。
    fn bytes_with_boundaries(&self) -> (Vec<u8>, Vec<usize>) {
        let e = self.enc();
        let is_2173 = self.generation == Generation::G2173;
        let vol_act_nr = if is_2173 {
            self.records
                .iter()
                .filter_map(|r| match r {
                    OldRecord::Restart { volatile } => Some(volatile.len()),
                    _ => None,
                })
                .max()
                .unwrap_or(0)
        } else {
            0
        };

        // --- ヘッダ 3 節は共有の独立ライタに書かせる ---
        let mut spec = FixtureSpec::skeleton(self.generation, self.abi);
        spec.version = if is_2173 {
            (11, 6, 5, 0)
        } else {
            (10, 2, 1, 0)
        };
        spec.activities = self.activities.clone();
        if vol_act_nr > 0 {
            // 共有ライタは `sa_vol_act_nr` を RESTART の volatile リストの長さから決める。
            // 使うのはヘッダだけなので、このレコードは長さを伝えるための置き物。
            spec.records = vec![RecordSpec::restart(1, T0, 0, 0, 0).with_volatile(vec![
                old_act(
                    1, 0x8a, 160, 1
                );
                vol_act_nr
            ])];
        }
        let fx = fixtures::build(spec);
        let mut b = fx.bytes[..fx.first_record_off].to_vec();
        let mut boundaries = vec![b.len()];

        // `0x2173` の RESTART で変わる item 数 (§5.7)
        let mut nr: Vec<i32> = self.activities.iter().map(|a| a.nr).collect();

        for (seq, rec) in self.records.iter().enumerate() {
            let t = old_record_time(seq);
            let rtype = match rec {
                OldRecord::Stats { record_type, .. } => *record_type,
                OldRecord::Restart { .. } => fixtures::R_RESTART,
                OldRecord::Comment { .. } => fixtures::R_COMMENT,
            };
            let uptime0 = old_record_uptime0(seq, rtype);

            // --- record_header (§3.6.1、48 バイト) ---
            let o = b.len();
            b.resize(o + OLD_RECORD_HEADER_SIZE, 0);
            e.put_u64(&mut b, o, uptime0 * 2); // uptime (全 CPU 合計。変換で捨てられる)
            e.put_u64(&mut b, o + 16, uptime0); // uptime0
            e.put_ul(&mut b, o + 32, t); // ust_time (unsigned long)
            b[o + 40] = rtype;
            let (hh, mm, ss) = hms_of(t);
            b[o + 41] = hh;
            b[o + 42] = mm;
            b[o + 43] = ss;

            match rec {
                OldRecord::Stats { items, checked, .. } => {
                    assert_eq!(
                        items.len(),
                        self.activities.len(),
                        "activity ごとに item 列が要る"
                    );
                    for ((act, n), bytes) in self.activities.iter().zip(&nr).zip(items) {
                        if *checked {
                            let want = act.size as usize * *n as usize * act.nr2 as usize;
                            assert_eq!(
                                bytes.len(),
                                want,
                                "activity {} の item 列の長さ (size × nr × nr2)",
                                act.id
                            );
                        }
                        b.extend_from_slice(bytes);
                    }
                }
                OldRecord::Restart { volatile } => {
                    if is_2173 {
                        // `sa_vol_act_nr` 個の旧 file_activity (§3.4.2: id@0 / magic@4 / nr@8 /
                        // nr2@12 / size@16)。足りない分は空スロット (id = 0)。
                        for i in 0..vol_act_nr {
                            let o = b.len();
                            b.resize(o + OLD_FILE_ACTIVITY_SIZE, 0);
                            let Some(v) = volatile.get(i) else {
                                continue;
                            };
                            e.put_u32(&mut b, o, v.id);
                            e.put_u32(&mut b, o + 4, v.magic);
                            e.put_u32(&mut b, o + 8, v.nr as u32);
                            e.put_u32(&mut b, o + 12, v.nr2 as u32);
                            e.put_u32(&mut b, o + 16, v.size as u32);
                            if v.id != 0
                                && v.nr > 0
                                && let Some(k) = self.activities.iter().position(|a| a.id == v.id)
                            {
                                nr[k] = v.nr;
                            }
                        }
                    }
                }
                OldRecord::Comment { text } => {
                    let o = b.len();
                    b.resize(o + fixtures::MAX_COMMENT_LEN, 0);
                    b[o..o + text.len()].copy_from_slice(text.as_bytes());
                }
            }
            boundaries.push(b.len());
        }
        (b, boundaries)
    }
}

// --- 変換結果を独立に読む ---

/// 変換結果の `file_activity` 1 件 (§3.4.3)。
#[derive(Debug, Clone, PartialEq, Eq)]
struct OutActivity {
    id: u32,
    magic: u32,
    nr: i32,
    nr2: i32,
    has_nr: bool,
    size: usize,
    types_nr: [u32; 3],
}

/// 変換結果の統計レコード内の 1 activity。
#[derive(Debug, Clone)]
struct OutSlice {
    id: u32,
    /// レコード内件数 (`has_nr` の activity だけが持つ)。
    count: Option<u32>,
    items: Vec<Vec<u8>>,
}

/// 変換結果の 1 レコード。
#[derive(Debug, Clone)]
struct OutRecord {
    /// `record_header` の先頭のファイル内オフセット。
    offset: usize,
    record_type: u8,
    uptime_cs: u64,
    ust_time: u64,
    hms: (u8, u8, u8),
    cpu_count: Option<u32>,
    comment: Option<Vec<u8>>,
    slices: Vec<OutSlice>,
}

impl OutRecord {
    fn slice(&self, id: u32) -> &OutSlice {
        self.slices
            .iter()
            .find(|s| s.id == id)
            .unwrap_or_else(|| panic!("activity {id} がレコードに無い"))
    }
}

/// 変換結果の全体。
#[derive(Debug, Clone)]
struct OutFile {
    version: (u8, u8, u8, u8),
    upgraded: u32,
    hz: u64,
    cpu_nr: u32,
    activities: Vec<OutActivity>,
    records: Vec<OutRecord>,
}

impl OutFile {
    fn activity(&self, id: u32) -> &OutActivity {
        self.activities
            .iter()
            .find(|a| a.id == id)
            .unwrap_or_else(|| panic!("activity {id} が file_activity に無い"))
    }

    fn stats(&self) -> Vec<&OutRecord> {
        self.records
            .iter()
            .filter(|r| !matches!(r.record_type, fixtures::R_RESTART | fixtures::R_COMMENT))
            .collect()
    }
}

/// 変換結果を `docs/format/01-file-format.md` §3 のオフセット表だけで読む。
///
/// 本体の読み取り側とは独立に、書き出されたバイト列の形を確かめる。
/// **ファイル末尾まで余りなく読めること**もここで確かめる。
///
/// record_type が RESTART / COMMENT 以外のレコードは統計ペイロードを持つものとして読む
/// (変換が書いた形を確かめるため。現行形式の読み手は 5〜15 を拡張レコードとして扱う)。
fn walk_output(bytes: &[u8], abi: FixtureAbi) -> OutFile {
    let e = Enc::of(abi);
    let len = bytes.len();
    assert!(
        len >= CUR_FILE_MAGIC_SIZE + CUR_FILE_HEADER_SIZE,
        "ヘッダが揃っていない ({len} バイト)"
    );

    // --- file_magic (§3.2.4) ---
    assert_eq!(e.u16_at(bytes, 0), fixtures::SYSSTAT_MAGIC, "sysstat_magic");
    assert_eq!(
        e.u16_at(bytes, 2),
        convert::OUT_FORMAT_MAGIC,
        "format_magic"
    );
    let version = (bytes[4], bytes[5], bytes[6], bytes[7]);
    assert_eq!(
        e.u32_at(bytes, 8) as usize,
        CUR_FILE_HEADER_SIZE,
        "header_size"
    );
    let upgraded = e.u32_at(bytes, 12);
    assert_eq!(
        [
            e.u32_at(bytes, 16),
            e.u32_at(bytes, 20),
            e.u32_at(bytes, 24)
        ],
        [1, 1, 12],
        "hdr_types_nr"
    );
    assert!(bytes[28..76].iter().all(|&b| b == 0), "file_magic.pad は 0");

    // --- file_header (§3.3.5) ---
    let fh = CUR_FILE_MAGIC_SIZE;
    let hz = e.ul_at(bytes, fh + 8);
    let cpu_nr = e.u32_at(bytes, fh + 16);
    let act_nr = e.u32_at(bytes, fh + 20) as usize;
    assert_eq!(
        [
            e.u32_at(bytes, fh + 28),
            e.u32_at(bytes, fh + 32),
            e.u32_at(bytes, fh + 36)
        ],
        [0, 0, 9],
        "act_types_nr"
    );
    assert_eq!(
        [
            e.u32_at(bytes, fh + 40),
            e.u32_at(bytes, fh + 44),
            e.u32_at(bytes, fh + 48)
        ],
        [2, 0, 1],
        "rec_types_nr"
    );
    assert_eq!(
        e.u32_at(bytes, fh + 52) as usize,
        CUR_FILE_ACTIVITY_SIZE,
        "act_size"
    );
    assert_eq!(
        e.u32_at(bytes, fh + 56) as usize,
        CUR_RECORD_HEADER_SIZE,
        "rec_size"
    );
    assert_eq!(e.u32_at(bytes, fh + 60), 0, "extra_next");
    assert_eq!(
        text_at(bytes, fh + 327, 8),
        b"",
        "sa_tzname は元ファイルに無いので空"
    );

    // --- file_activity[] (§3.4.3) ---
    let fa = fh + CUR_FILE_HEADER_SIZE;
    let activities: Vec<OutActivity> = (0..act_nr)
        .map(|i| {
            let o = fa + i * CUR_FILE_ACTIVITY_SIZE;
            OutActivity {
                id: e.u32_at(bytes, o),
                magic: e.u32_at(bytes, o + 4),
                nr: e.u32_at(bytes, o + 8) as i32,
                nr2: e.u32_at(bytes, o + 12) as i32,
                has_nr: e.u32_at(bytes, o + 16) != 0,
                size: e.u32_at(bytes, o + 20) as usize,
                types_nr: [
                    e.u32_at(bytes, o + 24),
                    e.u32_at(bytes, o + 28),
                    e.u32_at(bytes, o + 32),
                ],
            }
        })
        .collect();

    // --- レコード列 (§3.6.3 / §6) ---
    let mut off = fa + act_nr * CUR_FILE_ACTIVITY_SIZE;
    let mut records = Vec::new();
    while off < len {
        assert!(
            off + CUR_RECORD_HEADER_SIZE <= len,
            "record_header の途中で終わっている (offset={off})"
        );
        let h = &bytes[off..off + CUR_RECORD_HEADER_SIZE];
        assert_eq!(
            e.u32_at(h, 16),
            0,
            "record_header.extra_next (offset={off})"
        );
        let mut rec = OutRecord {
            offset: off,
            record_type: h[20],
            uptime_cs: e.u64_at(h, 0),
            ust_time: e.u64_at(h, 8),
            hms: (h[21], h[22], h[23]),
            cpu_count: None,
            comment: None,
            slices: Vec::new(),
        };
        off += CUR_RECORD_HEADER_SIZE;
        match rec.record_type {
            fixtures::R_RESTART => {
                assert!(off + 4 <= len, "RESTART の CPU 数の途中で終わっている");
                rec.cpu_count = Some(e.u32_at(bytes, off));
                off += 4;
            }
            fixtures::R_COMMENT => {
                assert!(
                    off + fixtures::MAX_COMMENT_LEN <= len,
                    "コメントの途中で終わっている"
                );
                rec.comment = Some(bytes[off..off + fixtures::MAX_COMMENT_LEN].to_vec());
                off += fixtures::MAX_COMMENT_LEN;
            }
            _ => {
                for a in &activities {
                    let count = if a.has_nr {
                        assert!(
                            off + 4 <= len,
                            "activity {} の件数の途中で終わっている",
                            a.id
                        );
                        let c = e.u32_at(bytes, off);
                        off += 4;
                        Some(c)
                    } else {
                        None
                    };
                    let n = count.unwrap_or(a.nr as u32) as usize * a.nr2 as usize;
                    assert!(
                        off + n * a.size <= len,
                        "activity {} の item 列 ({n} 個) の途中で終わっている",
                        a.id
                    );
                    let items = (0..n)
                        .map(|k| bytes[off + k * a.size..off + (k + 1) * a.size].to_vec())
                        .collect();
                    off += n * a.size;
                    rec.slices.push(OutSlice {
                        id: a.id,
                        count,
                        items,
                    });
                }
            }
        }
        records.push(rec);
    }
    assert_eq!(off, len, "変換結果を末尾まで余りなく読めること");

    OutFile {
        version,
        upgraded,
        hz,
        cpu_nr,
        activities,
        records,
    }
}

/// 現行 revision の `file_activity` に書かれるべき値
/// (`docs/format/02-activities.md` §3 の表: magic / size / types_nr / `has_nr`)。
fn current_activity(id: u32) -> (u32, usize, [u32; 3], bool) {
    match id {
        1 => (0x8b, 80, [10, 0, 0], true),  // A_CPU
        2 => (0x8b, 16, [1, 1, 0], false),  // A_PCSW
        3 => (0x8c, 12, [0, 0, 1], true),   // A_IRQ
        10 => (0x8b, 28, [0, 0, 7], true),  // A_SERIAL
        11 => (0x8c, 80, [3, 3, 8], true),  // A_DISK
        12 => (0x8d, 80, [7, 0, 1], true),  // A_NET_DEV
        13 => (0x8c, 88, [9, 0, 0], true),  // A_NET_EDEV
        36 => (0x8a, 88, [0, 0, 4], true),  // A_PWR_USB
        37 => (0x8b, 296, [5, 0, 0], true), // A_FS
        38 => (0x8a, 48, [0, 4, 0], true),  // A_NET_FC
        other => panic!("activity {other} の期待値を表に足すこと"),
    }
}

/// 変換結果の `file_activity` が現行 revision の値で、`nr` / `nr2` が期待どおりか。
fn assert_current_activity(out: &OutFile, id: u32, nr: i32, nr2: i32, who: &str) {
    let (magic, size, types_nr, has_nr) = current_activity(id);
    assert_eq!(
        out.activity(id),
        &OutActivity {
            id,
            magic,
            nr,
            nr2,
            has_nr,
            size,
            types_nr
        },
        "{who}: activity {id} の file_activity は現行 revision の値"
    );
}

/// 変換後の `record_header` が §5.6 の規則どおりか。
///
/// `uptime_cs = uptime0 × 100 / HZ` (全 CPU 合計の `uptime` は捨てる)。時刻はそのまま写す。
fn assert_record_headers(o: &OutFile, hz: u64, who: &str) {
    for (seq, r) in o.records.iter().enumerate() {
        let t = old_record_time(seq);
        assert_eq!(r.ust_time, t, "{who}: レコード {seq} の ust_time");
        assert_eq!(r.hms, hms_of(t), "{who}: レコード {seq} の時分秒");
        assert_eq!(
            r.uptime_cs,
            old_record_uptime0(seq, r.record_type) * 100 / hz,
            "{who}: レコード {seq} の uptime_cs = uptime0 × 100 / HZ"
        );
    }
}

/// 旧世代 fixture を開いて変換する。
///
/// `RESARCH_CONVERT_FIXTURE_DIR` があれば入力と変換結果をそこへ残す。
fn convert_fixture(name: &str, input: Vec<u8>) -> (SaFile, Vec<u8>, ConvertReport) {
    keep_fixture(name, "in", &input);
    let src = SaFile::from_bytes(name, input)
        .unwrap_or_else(|e| panic!("{name}: 自作 fixture を開けない: {e}"));
    let (out, report) = convert_bytes(&src, None);
    assert_eq!(report.bytes_written as usize, out.len(), "{name}");
    keep_fixture(name, "out", &out);
    (src, out, report)
}

fn keep_fixture(name: &str, suffix: &str, bytes: &[u8]) {
    if let Some(dir) = std::env::var_os("RESARCH_CONVERT_FIXTURE_DIR") {
        let file = format!("{}.{suffix}", name.replace('/', "_"));
        std::fs::write(Path::new(&dir).join(file), bytes).expect("fixture を残せる");
    }
}

/// 変換結果を reSARch 自身で開き直し、末尾まで余りなく読めることを確かめる。
fn reopen_exact(label: &str, bytes: Vec<u8>) -> SaFile {
    let dst = reopen(label, bytes);
    let summary = dst
        .scan(|_| Ok(ScanControl::Continue))
        .unwrap_or_else(|e| panic!("{label}: 変換結果を走査できない: {e}"));
    assert!(
        summary.is_exact(),
        "{label}: 変換結果を末尾まで余りなく読めること (残 {} バイト)",
        summary.file_size.saturating_sub(summary.end_offset)
    );
    dst
}

/// 直読した値と、変換結果を読んだ値が、書き出された item の範囲で一致すること。
///
/// 番兵で切り詰めた item は変換結果に無いので、比べるのは短い方の件数まで。
/// `A_SERIAL` の `line` だけは 1 起点 → 0 起点で 1 つ減る (§5.8)。
fn assert_values_survive(src: &SaFile, dst: &SaFile, who: &str) {
    let before = collect_values(src);
    let after = collect_values(dst);
    assert_eq!(before.len(), after.len(), "{who}: 統計レコード数");
    let mut compared = 0usize;
    for (r, (b, a)) in before.iter().zip(&after).enumerate() {
        for (id, b_items) in b {
            let a_items = a.get(id).unwrap_or_else(|| {
                panic!("{who}: レコード {r} の activity {id} が変換後に読めない")
            });
            for (i, (bi, ai)) in b_items.iter().zip(a_items).enumerate() {
                for (name, bv) in bi {
                    let av = ai.get(name).copied().unwrap_or_else(|| {
                        panic!(
                            "{who}: レコード {r} activity {id} item {i} の {name} が変換後に無い"
                        )
                    });
                    let want = if (*id, *name) == (10, "line") {
                        bv - 1
                    } else {
                        *bv
                    };
                    assert_eq!(
                        av, want,
                        "{who}: レコード {r} activity {id} item {i} の {name}"
                    );
                    compared += 1;
                }
            }
        }
    }
    assert!(compared > 0, "{who}: 1 つも比べていない");
}

/// 変換結果を reSARch で読んだときの、統計レコードごと・activity ごとの item 数。
fn item_counts(file: &SaFile, id: ActivityId) -> Vec<u32> {
    record_shapes(file)
        .iter()
        .filter(|s| !matches!(s.kind, "restart" | "comment"))
        .map(|s| {
            s.items
                .iter()
                .find(|(i, _, _)| *i == id.0)
                .map(|(_, n, _)| *n)
                .unwrap_or_else(|| panic!("{id} がレコードに無い"))
        })
        .collect()
}

/// 変換対象の世代 × 4 通りの ABI。
fn old_generations() -> Vec<(Generation, FixtureAbi)> {
    [Generation::G2171, Generation::G2173]
        .into_iter()
        .flat_map(|g| FixtureAbi::ALL.into_iter().map(move |abi| (g, abi)))
        .collect()
}

/// `A_CPU` の item を 3 つ (CPU "all" + 2 CPU) 並べる。`k` で値を変える。
fn cpus3(e: Enc, k: u64) -> Vec<u8> {
    [
        cpu_8a(e, 300 * k, 30 * k, 3000 * k),
        cpu_8a(e, 100 * k, 10 * k, 1000 * k),
        cpu_8a(e, 200 * k, 20 * k, 2000 * k),
    ]
    .concat()
}

// --- 6.1 番兵による件数の打ち切り ---

/// 件数を数え直す 6 activity と `A_SERIAL`・`A_CPU` を並べた fixture。
///
/// R0 は途中に番兵がある / R1 は RESTART / R2 は COMMENT / R3 は番兵の無い activity と
/// 先頭寄りの番兵。期待する件数は、各 item に置いた番兵フィールドの値から手で数えた
/// ([`sentinel_counts_become_record_item_counts`] のコメント)。
fn sentinel_fixture(generation: Generation, abi: FixtureAbi) -> OldFile {
    let e = Enc::of(abi);
    // `mountp` を持つ 336 バイト版は 11.1.4 以降 (= 0x2173 の世代) にしか無い
    let fs_size: usize = if generation == Generation::G2171 {
        160
    } else {
        336
    };
    let acts = vec![
        old_act(1, 0x8a, 160, 3),             // A_CPU
        old_act(11, 0x8b, 64, 4),             // A_DISK
        old_act(12, 0x8c, 144, 3),            // A_NET_DEV
        old_act(13, 0x8b, 160, 3),            // A_NET_EDEV
        old_act(10, 0x8a, 28, 4),             // A_SERIAL
        old_act(36, 0x8a, 88, 3),             // A_PWR_USB
        old_act(37, 0x8a, fs_size as i32, 3), // A_FS
        old_act(38, 0x8a, 48, 2),             // A_NET_FC
    ];
    let fs = |blocks: u64, name: &str, mountp: &str| fs_8a(e, fs_size, blocks, name, mountp);
    OldFile::new(generation, abi, acts)
        // R0: 途中に番兵がある
        .stats(vec![
            cpus3(e, 1),
            [
                disk_8b(e, 8, 0, 100),
                // major が 0 でも minor が非 0 なら生きている (和で判定)
                disk_8b(e, 0, 1, 101),
                disk_8b(e, 0, 0, 0),
                // 番兵より後ろに値が残っていても書かない
                disk_8b(e, 8, 16, 103),
            ]
            .concat(),
            [
                net_dev_8c(e, "eth0", 10),
                net_dev_8c(e, "", 11),
                net_dev_8c(e, "eth1", 12),
            ]
            .concat(),
            [
                net_edev_8b(e, "eth0", 20),
                net_edev_8b(e, "lo", 21),
                net_edev_8b(e, "", 22),
            ]
            .concat(),
            [
                serial(e, 1, 30),
                serial(e, 3, 31),
                serial(e, 0, 32),
                serial(e, 2, 33),
            ]
            .concat(),
            [
                pwr_usb(e, 1, "hub"),
                pwr_usb(e, 0, "stale"),
                pwr_usb(e, 2, "mouse"),
            ]
            .concat(),
            [
                fs(100, "/dev/sda1", "/"),
                fs(50, "/dev/sda2", "/home"),
                fs(0, "", ""),
            ]
            .concat(),
            [fchost(e, "host0", 40), fchost(e, "host1", 41)].concat(),
        ])
        .restart(vec![old_act(1, 0x8a, 160, 3)])
        .comment("sentinel")
        // R3: 番兵の無い activity (全枠使用) と、先頭寄りの番兵
        .stats(vec![
            cpus3(e, 2),
            [
                disk_8b(e, 8, 0, 200),
                disk_8b(e, 8, 16, 201),
                disk_8b(e, 8, 32, 202),
                disk_8b(e, 8, 48, 203),
            ]
            .concat(),
            [
                net_dev_8c(e, "eth0", 30),
                net_dev_8c(e, "eth1", 31),
                net_dev_8c(e, "eth2", 32),
            ]
            .concat(),
            [
                net_edev_8b(e, "eth0", 40),
                net_edev_8b(e, "", 41),
                net_edev_8b(e, "eth2", 42),
            ]
            .concat(),
            [
                serial(e, 2, 50),
                serial(e, 1, 51),
                serial(e, 4, 52),
                serial(e, 3, 53),
            ]
            .concat(),
            [
                pwr_usb(e, 3, "hub"),
                pwr_usb(e, 2, "mouse"),
                pwr_usb(e, 1, "kbd"),
            ]
            .concat(),
            [
                fs(100, "/dev/sda1", "/"),
                fs(0, "", ""),
                fs(70, "/dev/sdb1", "/data"),
            ]
            .concat(),
            [fchost(e, "host0", 60), fchost(e, "", 61)].concat(),
        ])
}

/// 番兵 (§5.9) で切り詰めた件数が、レコードごとの `__nr_t` として書かれること。
///
/// 旧形式は固定件数 (`file_activity.nr` = 確保した枠数) で書かれ、末尾に空き枠が並ぶ。
/// 現行形式は `has_nr` でレコードごとに件数を持つので、番兵の手前までを書く。
/// 番兵の後ろに値の残った枠があっても書かない (本家 `count_stats_*` と同じ)。
///
/// 手で数えた期待件数 (並びは CPU / DISK / NET_DEV / NET_EDEV / SERIAL / PWR_USB / FS / NET_FC):
///
/// - R0: DISK は (8,0) (0,1) の 2 件 (major + minor の**和**で判定するので (0,1) は生きている)、
///   NET_DEV は 1 件、NET_EDEV は 2 件、SERIAL は line 1 / 3 の 2 件、PWR_USB は bus 1 の 1 件、
///   FS は 2 件、NET_FC は番兵が無いので 2 件
/// - R3: DISK / NET_DEV / SERIAL / PWR_USB は番兵が無いので nr 件、NET_EDEV / FS / NET_FC は 1 件
#[test]
fn sentinel_counts_become_record_item_counts() {
    for (generation, abi) in old_generations() {
        let e = Enc::of(abi);
        let fs_size = if generation == Generation::G2171 {
            160
        } else {
            336
        };
        let file = sentinel_fixture(generation, abi);
        let who = format!("sentinel/{}", file.label());
        let (src, out, report) = convert_fixture(&who, file.bytes());

        // --- 報告 ---
        assert_eq!(
            (
                report.stats_records,
                report.restart_records,
                report.comment_records
            ),
            (2, 1, 1),
            "{who}"
        );
        assert!(report.warnings.is_empty(), "{who}: {:?}", report.warnings);
        assert!(report.opaque_activities.is_empty(), "{who}");
        assert_eq!(report.truncated_values, 0, "{who}");
        assert_eq!(report.cpu_nr, 3, "{who}");

        // --- 独立に読んだ形 ---
        let o = walk_output(&out, abi);
        assert_eq!(o.upgraded, convert::OUT_UPGRADED, "{who}");
        assert_eq!(o.version, src.magic().version, "{who}: 版数は元のまま");
        assert_eq!((o.hz, o.cpu_nr), (100, 3), "{who}");
        for (id, nr) in [
            (1, 3),
            (11, 4),
            (12, 3),
            (13, 3),
            (10, 4),
            (36, 3),
            (37, 3),
            (38, 2),
        ] {
            assert_current_activity(&o, id, nr, 1, &who);
        }
        let types: Vec<u8> = o.records.iter().map(|r| r.record_type).collect();
        assert_eq!(types, vec![1, 2, 4, 1], "{who}: レコードの並びは元のまま");
        assert_record_headers(&o, 100, &who);
        assert_eq!(o.records[1].cpu_count, Some(3), "{who}: RESTART の CPU 数");
        assert_eq!(
            text_at(o.records[2].comment.as_deref().unwrap(), 0, 64),
            b"sentinel",
            "{who}"
        );

        let stats = o.stats();
        let counts = |r: &OutRecord| -> Vec<u32> {
            r.slices
                .iter()
                .map(|s| s.count.expect("全 activity が has_nr"))
                .collect()
        };
        // 並びは CPU / DISK / NET_DEV / NET_EDEV / SERIAL / PWR_USB / FS / NET_FC
        assert_eq!(counts(stats[0]), vec![3, 2, 1, 2, 2, 1, 2, 2], "{who}: R0");
        assert_eq!(counts(stats[1]), vec![3, 4, 3, 1, 4, 3, 1, 1], "{who}: R3");

        // --- 生き残った item の値 (02-activities.md §5 の位置) ---
        let r0 = stats[0];
        let disk = &r0.slice(11).items;
        assert_eq!(
            disk.iter()
                .map(|d| (
                    e.u32_at(d, 64),
                    e.u32_at(d, 68),
                    e.u64_at(d, 0),
                    e.ul_at(d, 24)
                ))
                .collect::<Vec<_>>(),
            vec![(8, 0, 100, 800), (0, 1, 101, 808)],
            "{who}: A_DISK の major@64 / minor@68 / nr_ios@0 / rd_sect@24"
        );
        let nic = &r0.slice(12).items[0];
        assert_eq!(text_at(nic, 60, 16), b"eth0", "{who}: interface@60");
        assert_eq!(e.u64_at(nic, 0), 10, "{who}: rx_packets@0");
        assert_eq!(e.u64_at(nic, 48), 16, "{who}: multicast@48");
        assert_eq!(
            (e.u32_at(nic, 56), nic[76]),
            (1000, 2),
            "{who}: speed@56 / duplex@76"
        );
        let edev = &r0.slice(13).items;
        assert_eq!(
            edev.iter()
                .map(|d| (text_at(d, 72, 16).to_vec(), e.u64_at(d, 0), e.u64_at(d, 64)))
                .collect::<Vec<_>>(),
            vec![(b"eth0".to_vec(), 20, 28), (b"lo".to_vec(), 21, 29)],
            "{who}: A_NET_EDEV の interface@72 / collisions@0 / tx_carrier_errors@64"
        );
        let tty = &r0.slice(10).items;
        assert_eq!(
            tty.iter()
                .map(|t| (e.u32_at(t, 24), e.u32_at(t, 0), e.u32_at(t, 20)))
                .collect::<Vec<_>>(),
            vec![(0, 30, 35), (2, 31, 36)],
            "{who}: A_SERIAL の line は 1 起点 → 0 起点 (rx / overrun はそのまま)"
        );
        let usb = &r0.slice(36).items[0];
        assert_eq!(
            (e.u32_at(usb, 0), e.u32_at(usb, 4), text_at(usb, 40, 48)),
            (1, 0x1d6b, &b"hub"[..]),
            "{who}: A_PWR_USB"
        );
        let fsys = &r0.slice(37).items;
        let mounts: [&[u8]; 2] = if fs_size > 160 {
            [b"/", b"/home"]
        } else {
            // 160 バイト版に mountp は無い
            [b"", b""]
        };
        assert_eq!(
            fsys.iter()
                .map(|f| (
                    e.u64_at(f, 0),
                    text_at(f, 40, 128).to_vec(),
                    text_at(f, 168, 128).to_vec()
                ))
                .collect::<Vec<_>>(),
            vec![
                (100, b"/dev/sda1".to_vec(), mounts[0].to_vec()),
                (50, b"/dev/sda2".to_vec(), mounts[1].to_vec()),
            ],
            "{who}: A_FS の f_blocks@0 / fs_name@40 / mountp@168"
        );
        let fc = &r0.slice(38).items;
        assert_eq!(
            fc.iter()
                .map(|f| (text_at(f, 32, 16).to_vec(), e.ul_at(f, 0), e.ul_at(f, 24)))
                .collect::<Vec<_>>(),
            vec![(b"host0".to_vec(), 40, 43), (b"host1".to_vec(), 41, 44)],
            "{who}: A_NET_FC の fchost_name@32 / f_rxframes@0 / f_txwords@24"
        );
        let cpu = &r0.slice(1).items;
        assert_eq!(
            cpu.iter()
                .map(|c| (e.u64_at(c, 0), e.u64_at(c, 16), e.u64_at(c, 24)))
                .collect::<Vec<_>>(),
            vec![(300, 30, 3000), (100, 10, 1000), (200, 20, 2000)],
            "{who}: A_CPU の cpu_user@0 / cpu_sys@16 / cpu_idle@24 (aligned(16) の除去)"
        );

        // --- reSARch 自身で読み直す ---
        let dst = reopen_exact(&who, out);
        assert_eq!(item_counts(&dst, ActivityId::DISK), vec![2, 4], "{who}");
        assert_eq!(item_counts(&dst, ActivityId::SERIAL), vec![2, 4], "{who}");
        assert_values_survive(&src, &dst, &who);
    }
}

// --- 6.2 レコード内件数 0 ---

/// `A_DISK` (2 枠) の生きている枠が R0 は 1 つ、R1 / R2 は 0、R3 は 2 つの fixture。
fn zero_count_fixture(abi: FixtureAbi) -> OldFile {
    let e = Enc::of(abi);
    OldFile::new(
        Generation::G2173,
        abi,
        vec![old_act(1, 0x8a, 160, 3), old_act(11, 0x8b, 64, 2)],
    )
    .stats(vec![
        cpus3(e, 1),
        [disk_8b(e, 8, 0, 1), disk_8b(e, 0, 0, 0)].concat(),
    ])
    // 先頭が空なら、後ろに値の残った枠があっても 0 件
    .stats(vec![
        cpus3(e, 2),
        [disk_8b(e, 0, 0, 0), disk_8b(e, 8, 16, 9)].concat(),
    ])
    .stats(vec![
        cpus3(e, 3),
        [disk_8b(e, 0, 0, 0), disk_8b(e, 0, 0, 0)].concat(),
    ])
    .stats(vec![
        cpus3(e, 4),
        [disk_8b(e, 8, 0, 4), disk_8b(e, 8, 16, 5)].concat(),
    ])
}

/// 生きている枠が 1 つも無いレコードは、件数 0 (item なし) として書かれ、読み直せること。
///
/// 0 は現行形式として正当な件数である (`01-file-format.md` §6.2。本家も
/// `read_nr_value()` を `non_zero = FALSE` で呼ぶ)。同じ注意はレコードごとに積まず、
/// 1 回だけ報告する。
#[test]
fn a_record_without_any_live_slot_is_written_with_a_zero_count() {
    for abi in FixtureAbi::ALL {
        let e = Enc::of(abi);
        let file = zero_count_fixture(abi);
        let who = format!("zero-count/{}", file.label());
        let (src, out, report) = convert_fixture(&who, file.bytes());

        let o = walk_output(&out, abi);
        let disk: Vec<(Option<u32>, usize)> = o
            .stats()
            .iter()
            .map(|r| {
                let s = r.slice(11);
                (s.count, s.items.len())
            })
            .collect();
        assert_eq!(
            disk,
            vec![(Some(1), 1), (Some(0), 0), (Some(0), 0), (Some(2), 2)],
            "{who}: (件数, item 数)"
        );
        let last = &o.stats()[3].slice(11).items[1];
        assert_eq!((e.u32_at(last, 64), e.u32_at(last, 68)), (8, 16), "{who}");

        assert_eq!(
            report.warnings.len(),
            1,
            "{who}: 2 レコード分でも 1 回だけ報告する: {:?}",
            report.warnings
        );
        assert!(
            report.warnings[0].contains("A_DISK") && report.warnings[0].contains("件数 0"),
            "{who}: {:?}",
            report.warnings
        );

        let dst = reopen_exact(&who, out);
        assert_eq!(
            item_counts(&dst, ActivityId::DISK),
            vec![1, 0, 0, 2],
            "{who}: reSARch の読み取り側も件数 0 を受け入れる"
        );
        assert_values_survive(&src, &dst, &who);
    }
}

// --- 6.3 構造を解釈できない activity の素通し ---

/// 決定的なバイト列 (素通しの検証用)。
fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(37) ^ seed)
        .collect()
}

/// 未知 ID と、既知 ID だが未知の magic を既知 activity の間に挟んだ fixture。
///
/// 未知 ID は 200 (`nr` = 2 × `nr2` = 2 の行列、24 バイト)、未知 magic は `A_PCSW` の `0xff`
/// (32 バイト)。後ろの `A_DISK` が正しく読めれば、素通しで境界が保たれている。
fn opaque_fixture(generation: Generation, abi: FixtureAbi) -> OldFile {
    let e = Enc::of(abi);
    let unknown = ActivitySpec {
        id: 200,
        magic: 0x8a,
        nr: 2,
        nr2: 2,
        has_nr: false,
        size: 24,
        types_nr: [0, 0, 0],
    };
    OldFile::new(
        generation,
        abi,
        vec![
            old_act(1, 0x8a, 160, 3),
            unknown,
            old_act(2, 0xff, 32, 1),
            old_act(11, 0x8b, 64, 2),
        ],
    )
    .stats(vec![
        cpus3(e, 1),
        pattern(24 * 4, 0xa5),
        pattern(32, 0x5a),
        [disk_8b(e, 8, 0, 1), disk_8b(e, 8, 16, 2)].concat(),
    ])
    .stats(vec![
        cpus3(e, 2),
        pattern(24 * 4, 0x3c),
        pattern(32, 0xc3),
        [disk_8b(e, 8, 0, 3), disk_8b(e, 0, 0, 0)].concat(),
    ])
}

/// 構造を解釈できない activity は、旧バイト列のまま素通しで書かれること。
///
/// 本家 `sadf -c` は未知 ID で `exit(1)` するが (§5.15)、reSARch は素通しにして変換を続ける
/// (`src/convert/mod.rs` の「本家と意図的に違えた点」)。`file_activity` も旧申告値
/// (magic / size / nr / nr2) のまま書き、件数は前置しない。
#[test]
fn unrecognised_activities_are_passed_through_byte_for_byte() {
    for (generation, abi) in old_generations() {
        let e = Enc::of(abi);
        let file = opaque_fixture(generation, abi);
        let who = format!("opaque/{}", file.label());
        let (src, out, report) = convert_fixture(&who, file.bytes());

        assert_eq!(
            report.opaque_activities,
            vec![ActivityId(200), ActivityId::PCSW],
            "{who}"
        );
        assert_eq!(report.activities, 4, "{who}");
        assert!(
            report
                .warnings
                .iter()
                .any(|w| w.contains("(A_UNKNOWN(200), A_PCSW)")),
            "{who}: {:?}",
            report.warnings
        );

        let o = walk_output(&out, abi);
        assert_eq!(
            o.activity(200),
            &OutActivity {
                id: 200,
                magic: 0x8a,
                nr: 2,
                nr2: 2,
                has_nr: false,
                size: 24,
                types_nr: [0, 0, 0],
            },
            "{who}: 未知 ID は旧申告値のまま"
        );
        assert_eq!(
            o.activity(2),
            &OutActivity {
                id: 2,
                magic: 0xff,
                nr: 1,
                nr2: 1,
                has_nr: false,
                size: 32,
                types_nr: [0, 0, 0],
            },
            "{who}: 未知 magic も現行の値へ書き換えない"
        );
        assert_current_activity(&o, 1, 3, 1, &who);
        assert_current_activity(&o, 11, 2, 1, &who);

        for (r, (unknown_seed, pcsw_seed)) in o.stats().iter().zip([(0xa5, 0x5a), (0x3c, 0xc3)]) {
            assert_eq!(r.slice(200).count, None, "{who}: 件数を前置しない");
            assert_eq!(
                r.slice(200).items.concat(),
                pattern(24 * 4, unknown_seed),
                "{who}: 未知 ID の item はバイト列のまま"
            );
            assert_eq!(
                r.slice(2).items.concat(),
                pattern(32, pcsw_seed),
                "{who}: 未知 magic の item はバイト列のまま"
            );
        }
        let disks: Vec<Vec<(u32, u32)>> = o
            .stats()
            .iter()
            .map(|r| {
                r.slice(11)
                    .items
                    .iter()
                    .map(|d| (e.u32_at(d, 64), e.u32_at(d, 68)))
                    .collect()
            })
            .collect();
        assert_eq!(
            disks,
            vec![vec![(8, 0), (8, 16)], vec![(8, 0)]],
            "{who}: 素通しの後ろの activity も境界を失わずに変換される"
        );

        let dst = reopen_exact(&who, out);
        assert_values_survive(&src, &dst, &who);
    }
}

// --- 6.4 A_CPU の無いファイル ---

/// `A_CPU` を持たない fixture (`A_PCSW` だけ。RESTART は含めない)。
///
/// `stats_pcsw_8a` (32 バイト): context_switch ULL@0 (aligned 16) / processes UL@16 (aligned 16)。
fn no_cpu_fixture(abi: FixtureAbi) -> OldFile {
    let e = Enc::of(abi);
    let pcsw = |cswch: u64, procs: u64| OldItem::new(32, e).u64(0, cswch).ul(16, procs).done();
    OldFile::new(Generation::G2171, abi, vec![old_act(2, 0x8a, 32, 1)])
        .stats(vec![pcsw(1000, 10)])
        .comment("no cpu")
        .stats(vec![pcsw(2000, 20)])
}

/// `A_CPU` の無いファイルは `sa_cpu_nr = 0` で変換を続け、そのことを報告すること。
///
/// 本家は `CPU activity not found in file. Aborting...` で中断する (§5.15) が、
/// 現行形式は CPU 統計必須の前提を撤廃している (§5.17) ので reSARch は続行する。
#[test]
fn a_file_without_a_cpu_activity_is_converted_with_a_zero_cpu_count() {
    for abi in FixtureAbi::ALL {
        let e = Enc::of(abi);
        let file = no_cpu_fixture(abi);
        let who = format!("no-cpu/{}", file.label());
        let (src, out, report) = convert_fixture(&who, file.bytes());
        assert_eq!(report.cpu_nr, 0, "{who}");
        assert!(
            report.warnings.iter().any(|w| w.contains("A_CPU が無い")),
            "{who}: {:?}",
            report.warnings
        );

        let o = walk_output(&out, abi);
        assert_eq!(o.cpu_nr, 0, "{who}: sa_cpu_nr");
        assert_current_activity(&o, 2, 1, 1, &who);
        // 現行 `stats_pcsw` (16 バイト): context_switch@0 / processes@8 (UL)
        let values: Vec<(u64, u64)> = o
            .stats()
            .iter()
            .map(|r| {
                let p = &r.slice(2).items[0];
                (e.u64_at(p, 0), e.ul_at(p, 8))
            })
            .collect();
        assert_eq!(values, vec![(1000, 10), (2000, 20)], "{who}");

        let dst = reopen_exact(&who, out);
        assert_eq!(dst.header().cpu_nr, Some(0), "{who}");
        assert_values_survive(&src, &dst, &who);
    }
}

// --- 6.5 RESTART で変わる CPU 数 ---

/// `0x2173` の RESTART に並ぶ volatile エントリ。
///
/// 実ファイル (本家 `data-10.3.1`) では id と nr 以外 (magic / nr2 / size) は 0 で書かれている。
fn volatile(id: u32, nr: i32) -> ActivitySpec {
    ActivitySpec {
        id,
        magic: 0,
        nr,
        nr2: 0,
        has_nr: false,
        size: 0,
        types_nr: [0, 0, 0],
    }
}

/// `0x2173` の RESTART で CPU 数が 3 → 5 → 2 と変わる fixture。
///
/// volatile リストの空スロット (id = 0) と並び順の違い (2 本目は `A_CPU` が 2 番目) も混ぜる。
fn cpu_restart_fixture(abi: FixtureAbi) -> OldFile {
    let e = Enc::of(abi);
    let cpus = |n: u64, k: u64| -> Vec<u8> {
        (0..n)
            .flat_map(|c| cpu_8a(e, 100 * k + c, 10 * k + c, 1000 * k + c))
            .collect()
    };
    OldFile::new(
        Generation::G2173,
        abi,
        vec![old_act(1, 0x8a, 160, 3), old_act(11, 0x8b, 64, 2)],
    )
    .stats(vec![
        cpus(3, 1),
        [disk_8b(e, 8, 0, 1), disk_8b(e, 8, 16, 2)].concat(),
    ])
    .restart(vec![volatile(1, 5), volatile(0, 0)])
    .stats(vec![
        cpus(5, 2),
        [disk_8b(e, 8, 0, 3), disk_8b(e, 0, 0, 0)].concat(),
    ])
    .restart(vec![volatile(0, 0), volatile(1, 2)])
    .stats(vec![
        cpus(2, 3),
        [disk_8b(e, 8, 0, 5), disk_8b(e, 8, 16, 6)].concat(),
    ])
    .comment("cpu restart")
}

/// `0x2173` の RESTART で変わった CPU 数が、RESTART の CPU 数と以降の統計レコードの
/// 件数に反映されること (§5.7)。
///
/// `file_activity.nr` と `sa_cpu_nr` はファイル先頭の値 (3) のまま。現行形式では
/// `A_CPU` がレコードごとに件数を持つので、それで整合する。
#[test]
fn cpu_counts_announced_by_0x2173_restarts_carry_into_later_records() {
    for abi in FixtureAbi::ALL {
        let e = Enc::of(abi);
        let file = cpu_restart_fixture(abi);
        let who = format!("cpu-restart/{}", file.label());
        let (src, out, report) = convert_fixture(&who, file.bytes());
        assert!(report.warnings.is_empty(), "{who}: {:?}", report.warnings);
        assert_eq!(report.cpu_nr, 3, "{who}");
        assert_eq!(
            (
                report.stats_records,
                report.restart_records,
                report.comment_records
            ),
            (3, 2, 1),
            "{who}"
        );

        let o = walk_output(&out, abi);
        assert_eq!(o.cpu_nr, 3, "{who}: sa_cpu_nr はファイル先頭の CPU 数");
        assert_current_activity(&o, 1, 3, 1, &who);
        assert_record_headers(&o, 100, &who);
        let restarts: Vec<Option<u32>> = o
            .records
            .iter()
            .filter(|r| r.record_type == fixtures::R_RESTART)
            .map(|r| r.cpu_count)
            .collect();
        assert_eq!(restarts, vec![Some(5), Some(2)], "{who}: RESTART の CPU 数");
        let counts =
            |id: u32| -> Vec<Option<u32>> { o.stats().iter().map(|r| r.slice(id).count).collect() };
        assert_eq!(counts(1), vec![Some(3), Some(5), Some(2)], "{who}: A_CPU");
        assert_eq!(
            counts(11),
            vec![Some(2), Some(1), Some(2)],
            "{who}: A_DISK は RESTART の影響を受けない"
        );
        // 増えた CPU (5 個目) の値も現行の位置へ写る
        let fifth = &o.stats()[1].slice(1).items[4];
        assert_eq!(
            (e.u64_at(fifth, 0), e.u64_at(fifth, 16), e.u64_at(fifth, 24)),
            (204, 24, 2004),
            "{who}"
        );

        let dst = reopen_exact(&who, out);
        let restart_cpus: Vec<Option<u32>> = record_shapes(&dst)
            .iter()
            .filter(|s| s.kind == "restart")
            .map(|s| s.cpu_count)
            .collect();
        assert_eq!(restart_cpus, vec![Some(5), Some(2)], "{who}");
        assert_eq!(item_counts(&dst, ActivityId::CPU), vec![3, 5, 2], "{who}");
        assert_values_survive(&src, &dst, &who);
    }
}

// --- 6.6 A_IRQ の列の不足 ---

/// `A_IRQ` の割り込み数が RESTART で 4 → 2 に減る細工 fixture。
///
/// 実ファイルの volatile リストに `A_IRQ` は現れないが、変換後の `nr2`
/// (ファイル全体で固定) より少ない item しか無いレコードの扱いを確かめるために使う。
fn irq_shrink_fixture(abi: FixtureAbi) -> OldFile {
    let e = Enc::of(abi);
    OldFile::new(
        Generation::G2173,
        abi,
        vec![old_act(1, 0x8a, 160, 3), old_act(3, 0x8a, 16, 4)],
    )
    .stats(vec![
        cpus3(e, 1),
        [irq_8a(e, 10), irq_8a(e, 1), irq_8a(e, 2), irq_8a(e, 3)].concat(),
    ])
    .restart(vec![volatile(1, 3), volatile(3, 2)])
    .stats(vec![cpus3(e, 2), [irq_8a(e, 20), irq_8a(e, 11)].concat()])
}

/// レコードの item 数が変換後の `nr2` に足りないとき、不足分をゼロで埋めて
/// 境界を保ち、そのことを報告すること。
#[test]
fn irq_rows_shorter_than_the_declared_interrupt_count_are_zero_filled() {
    for abi in FixtureAbi::ALL {
        let e = Enc::of(abi);
        let file = irq_shrink_fixture(abi);
        let who = format!("irq-shrink/{}", file.label());
        let (_, out, report) = convert_fixture(&who, file.bytes());
        assert!(
            report
                .warnings
                .iter()
                .any(|w| w.contains("A_IRQ") && w.contains("(2)") && w.contains("(4)")),
            "{who}: {:?}",
            report.warnings
        );

        let o = walk_output(&out, abi);
        // 1 次元 (割り込み数 4) → 1 行 × 4 列
        assert_current_activity(&o, 3, 1, 4, &who);
        // 現行 `stats_irq` (12 バイト): irq_nr u32@0 / irq_name[8]@4
        let rows: Vec<Vec<(u32, Vec<u8>)>> = o
            .stats()
            .iter()
            .map(|r| {
                let s = r.slice(3);
                assert_eq!(s.count, Some(1), "{who}: A_IRQ は常に 1 行");
                s.items
                    .iter()
                    .map(|i| (e.u32_at(i, 0), text_at(i, 4, 8).to_vec()))
                    .collect()
            })
            .collect();
        assert_eq!(
            rows[0],
            vec![
                (10, b"sum".to_vec()),
                (1, b"0".to_vec()),
                (2, b"1".to_vec()),
                (3, b"2".to_vec())
            ],
            "{who}: 添字 0 は総和、以降は割り込み番号"
        );
        assert_eq!(
            rows[1][..2],
            [(20, b"sum".to_vec()), (11, b"0".to_vec())],
            "{who}"
        );
        assert!(
            o.stats()[1].slice(3).items[2..]
                .iter()
                .all(|i| i.iter().all(|&b| b == 0)),
            "{who}: 足りない列はゼロで埋める"
        );
        reopen_exact(&who, out);
    }
}

// --- 6.7 record_type ---

/// 2 本目の統計レコードの record_type を指定した fixture (`A_CPU` だけ)。
fn record_type_fixture(abi: FixtureAbi, second: u8) -> OldFile {
    let e = Enc::of(abi);
    OldFile::new(Generation::G2171, abi, vec![old_act(1, 0x8a, 160, 3)])
        .stats(vec![cpus3(e, 1)])
        .stats_as(second, vec![cpus3(e, 2)])
}

/// record_type は数値のまま写すこと。
///
/// `R_LAST_STATS` (3) は現行形式でも統計レコードなので、そのまま読み直せる。
/// 旧形式では 5〜15 も統計レコードとして扱われるが (§5.14)、現行形式では拡張レコードの
/// 番号になる。本家 `upgrade_record_header()` も値をコピーするだけなので同じ値を書き、
/// 解釈が変わることを報告に残す。
#[test]
fn record_types_are_copied_verbatim_and_unknown_ones_are_reported() {
    for abi in FixtureAbi::ALL {
        let file = record_type_fixture(abi, R_LAST_STATS);
        let who = format!("last-stats/{}", file.label());
        let (_, out, report) = convert_fixture(&who, file.bytes());
        assert_eq!(report.stats_records, 2, "{who}");
        assert!(report.warnings.is_empty(), "{who}: {:?}", report.warnings);
        let o = walk_output(&out, abi);
        let types: Vec<u8> = o.records.iter().map(|r| r.record_type).collect();
        assert_eq!(types, vec![fixtures::R_STATS, R_LAST_STATS], "{who}");
        let dst = reopen_exact(&who, out);
        let kinds: Vec<&str> = record_shapes(&dst).iter().map(|s| s.kind).collect();
        assert_eq!(kinds, vec!["stats", "last_stats"], "{who}");

        let file = record_type_fixture(abi, 7);
        let who = format!("record-type-7/{}", file.label());
        let (_, out, report) = convert_fixture(&who, file.bytes());
        assert_eq!(report.stats_records, 2, "{who}: 旧形式では統計レコード");
        assert!(
            report
                .warnings
                .iter()
                .any(|w| w.contains("record_type = 7")),
            "{who}: {:?}",
            report.warnings
        );
        let o = walk_output(&out, abi);
        let types: Vec<u8> = o.records.iter().map(|r| r.record_type).collect();
        assert_eq!(types, vec![fixtures::R_STATS, 7], "{who}");
        assert_eq!(
            o.records[1].slice(1).count,
            Some(3),
            "{who}: 統計の本体はそのまま書かれている"
        );
    }
}

// --- 6.8 出力側の幅に収まらない値 ---

/// `A_DISK` の旧 `0x8a` 版 (rd_sect が `unsigned long long`) を 2 レコード × 2 枠。
/// rd_sect はどれも 2^32 を超える。
fn wide_value_fixture(abi: FixtureAbi) -> OldFile {
    let e = Enc::of(abi);
    let disks = |k: u64| {
        [
            disk_8a(e, 8, 0, (1 << 32) + 5 + k),
            disk_8a(e, 8, 16, (1 << 32) + 6 + k),
        ]
        .concat()
    };
    OldFile::new(
        Generation::G2171,
        abi,
        vec![old_act(1, 0x8a, 160, 3), old_act(11, 0x8a, 80, 2)],
    )
    .stats(vec![cpus3(e, 1), disks(0)])
    .stats(vec![cpus3(e, 2), disks(10)])
}

/// 出力側の有効幅に収まらない値を切り詰めて書き、その個数を報告に積むこと。
///
/// `rd_sect` は旧 `0x8a` 版で `unsigned long long`、現行は `unsigned long`。
/// 32bit ライタのファイルでは現行側の有効幅が 4 バイトしかなく、下位 32 ビットだけが残る
/// (スロットの後半 4 バイトは 0 のまま)。64bit ファイルでは落ちない。
#[test]
fn values_too_wide_for_a_32bit_unsigned_long_are_counted_in_the_report() {
    for abi in FixtureAbi::ALL {
        let e = Enc::of(abi);
        let file = wide_value_fixture(abi);
        let who = format!("wide/{}", file.label());
        let (_, out, report) = convert_fixture(&who, file.bytes());
        let narrow = abi.long_bytes() == 4;
        assert_eq!(
            report.truncated_values,
            if narrow { 4 } else { 0 },
            "{who}: 2 レコード × 2 枠の rd_sect"
        );

        let o = walk_output(&out, abi);
        let disks: Vec<Vec<u8>> = o
            .stats()
            .iter()
            .flat_map(|r| r.slice(11).items.clone())
            .collect();
        let rd_sect: Vec<u64> = disks.iter().map(|d| e.ul_at(d, 24)).collect();
        let want: Vec<u64> = [5u64, 6, 15, 16]
            .iter()
            .map(|low| if narrow { *low } else { (1 << 32) + low })
            .collect();
        assert_eq!(rd_sect, want, "{who}: rd_sect@24");
        if narrow {
            assert!(
                disks.iter().all(|d| d[28..32] == [0, 0, 0, 0]),
                "{who}: unsigned long のスロット後半は 0"
            );
        }
        reopen_exact(&who, out);
    }
}

// --- 6.9 出力先の失敗 ---

/// 指定したバイト数を受け取ったあとは書き込みに失敗する出力先。
struct FailAfter {
    left: usize,
}

impl Write for FailAfter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.left == 0 {
            return Err(std::io::Error::other("出力先がいっぱい"));
        }
        let n = buf.len().min(self.left);
        self.left -= n;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// 出力先への書き込みが途中で失敗したら、どこで失敗しても `Error::Write` を返すこと
/// (パニックしない)。
#[test]
fn output_write_failures_are_returned_as_errors() {
    let file = sentinel_fixture(Generation::G2173, FixtureAbi::Le64);
    let src = SaFile::from_bytes("write-failure", file.bytes()).expect("自作 fixture を開ける");
    let (full, _) = convert_bytes(&src, None);
    let headers = CUR_FILE_MAGIC_SIZE + CUR_FILE_HEADER_SIZE + 8 * CUR_FILE_ACTIVITY_SIZE;
    for limit in [
        0,
        10,
        CUR_FILE_MAGIC_SIZE,
        CUR_FILE_MAGIC_SIZE + CUR_FILE_HEADER_SIZE,
        headers,
        headers + CUR_RECORD_HEADER_SIZE + 2,
        full.len() - 1,
    ] {
        let mut out = FailAfter { left: limit };
        let err = convert::convert(&src, &ConvertOptions::default(), &mut out)
            .expect_err("書き込みの失敗はエラーになる");
        assert!(
            matches!(err, Error::Write(_)),
            "{limit} バイトで失敗させたとき: {err}"
        );
    }
    let mut out = FailAfter { left: full.len() };
    convert::convert(&src, &ConvertOptions::default(), &mut out)
        .expect("ちょうど書き切れるなら成功する");
}

// --- 6.10 途中で切れた入力 ---

/// 途中で切れた入力は、Strict ならエラー、Lenient なら完全なレコードまでを変換して
/// 捨てた末尾を報告すること。
///
/// 1 バイト刻みで切った全ての入力について確かめる (RESTART の volatile リストの途中も含む)。
///
/// - Strict: レコード境界で切れていれば、その手前までの変換と同じ結果になる。
///   境界の途中なら `Error::Truncated` になる (パニックしない)
/// - Lenient: 常に成功し、結果は直前のレコード境界までを変換したものとバイト単位で一致する。
///   境界の途中なら、捨てた末尾のバイト数を報告する
#[test]
fn truncated_inputs_fail_in_strict_mode_and_keep_complete_records_in_lenient_mode() {
    let lenient = OpenOptions {
        tolerance: Tolerance::Lenient,
        ..OpenOptions::default()
    };
    // 切り詰めの扱いは ABI に依らないので、両端 (64bit LE と 32bit BE) だけを 1 バイト刻みで回す
    for abi in [FixtureAbi::Le64, FixtureAbi::Be32] {
        let file = cpu_restart_fixture(abi);
        let who = format!("truncated/{}", file.label());
        let (full, boundaries) = file.bytes_with_boundaries();

        let mut by_boundary: BTreeMap<usize, Vec<u8>> = BTreeMap::new();
        for &b in &boundaries {
            let src = SaFile::from_bytes(&who, full[..b].to_vec())
                .unwrap_or_else(|e| panic!("{who}: 境界 {b} で切ったファイルを開けない: {e}"));
            let (out, report) = convert_bytes(&src, None);
            assert!(
                !report
                    .warnings
                    .iter()
                    .any(|w| w.contains("解釈できなかった")),
                "{who}: 境界 {b}: {:?}",
                report.warnings
            );
            walk_output(&out, abi);
            by_boundary.insert(b, out);
        }

        for cut in boundaries[0]..=full.len() {
            let input = full[..cut].to_vec();
            let b = *boundaries
                .iter()
                .rev()
                .find(|&&b| b <= cut)
                .expect("先頭レコードの位置以上で切っている");
            let expected = &by_boundary[&b];

            let strict = SaFile::from_bytes(&who, input.clone())
                .unwrap_or_else(|e| panic!("{who}: cut={cut}: ヘッダは完全なのに開けない: {e}"));
            let mut out = Vec::new();
            match convert::convert(&strict, &ConvertOptions::default(), &mut out) {
                Ok(_) => {
                    assert_eq!(cut, b, "{who}: cut={cut}: レコードの途中なのに成功した");
                    assert_eq!(&out, expected, "{who}: cut={cut}");
                }
                Err(err) => {
                    assert_ne!(cut, b, "{who}: cut={cut}: 境界で切ったのに失敗した: {err}");
                    assert!(
                        matches!(err, Error::Truncated { .. }),
                        "{who}: cut={cut}: {err}"
                    );
                    fixtures::error_invariants(&err)
                        .unwrap_or_else(|m| panic!("{who}: cut={cut}: {m}"));
                }
            }

            let src = SaFile::from_bytes_with(&who, input, lenient.clone())
                .unwrap_or_else(|e| panic!("{who}: cut={cut}: Lenient で開けない: {e}"));
            let mut out = Vec::new();
            let report = convert::convert(&src, &ConvertOptions::default(), &mut out)
                .unwrap_or_else(|e| panic!("{who}: cut={cut}: Lenient で変換できない: {e}"));
            assert_eq!(
                &out, expected,
                "{who}: cut={cut}: 直前の境界 {b} までの変換と一致すること"
            );
            let dropped = report
                .warnings
                .iter()
                .find(|w| w.contains("解釈できなかった"));
            if cut == b {
                assert!(dropped.is_none(), "{who}: cut={cut}: {:?}", report.warnings);
            } else {
                let w = dropped
                    .unwrap_or_else(|| panic!("{who}: cut={cut}: 捨てた末尾を報告していない"));
                assert!(
                    w.contains(&format!("末尾 {} バイト", cut - b)),
                    "{who}: cut={cut}: {w}"
                );
            }
        }
    }
}

// --- 6.11 上限を超える申告 ---

/// 上限を超える件数や、巨大な item 列を申告した入力が、パニックせずにエラーになること。
///
/// 変換は `file_activity.nr × nr2 × size` 分の領域を先に確保しない (item 1 個分の
/// 作業バッファだけで書く) ので、巨大な申告でもメモリを使い果たさずに読み取り側の
/// 上限で止まる。
#[test]
fn oversized_declarations_are_rejected_without_panicking() {
    for abi in FixtureAbi::ALL {
        let e = Enc::of(abi);

        // (1) RESTART の volatile リストで A_CPU の件数が上限 (NR_CPUS + 1 = 8193) を超える
        let file = OldFile::new(Generation::G2173, abi, vec![old_act(1, 0x8a, 160, 3)])
            .stats(vec![cpus3(e, 1)])
            .restart(vec![volatile(1, fixtures::CPU_NR_MAX + 1)])
            .stats_unchecked(vec![cpus3(e, 2)]);
        let who = format!("restart-over-limit/{}", file.label());
        let src = SaFile::from_bytes(&who, file.bytes()).expect("ヘッダは正常");
        let err = convert::convert(&src, &ConvertOptions::default(), &mut Vec::new())
            .expect_err("上限を超える件数は変換しない");
        assert!(matches!(err, Error::LimitExceeded { .. }), "{who}: {err}");
        fixtures::error_invariants(&err).unwrap_or_else(|m| panic!("{who}: {m}"));

        // (2) 各申告値は上限ちょうどに収まるが、nr × nr2 × size が u32 を溢れる A_IRQ
        //     (8193 × 4096 × 1024 = 34,359,738,368。本家 data-12.7.1-A_IRQ_overflow と同じ値)
        let irq = ActivitySpec {
            id: 3,
            magic: 0x8a,
            nr: fixtures::CPU_NR_MAX,
            nr2: fixtures::NR2_MAX,
            has_nr: false,
            size: fixtures::MAX_ITEM_STRUCT_SIZE,
            types_nr: [0, 0, 0],
        };
        let file = OldFile::new(Generation::G2171, abi, vec![old_act(1, 0x8a, 160, 3), irq])
            .stats_unchecked(vec![cpus3(e, 1), vec![0; 64]]);
        let who = format!("irq-overflow/{}", file.label());
        let src = SaFile::from_bytes(&who, file.bytes()).expect("各申告値は上限内");
        let err = convert::convert(&src, &ConvertOptions::default(), &mut Vec::new())
            .expect_err("巨大な item 列は変換しない");
        assert!(matches!(err, Error::LimitExceeded { .. }), "{who}: {err}");
        fixtures::error_invariants(&err).unwrap_or_else(|m| panic!("{who}: {m}"));

        // (3) A_SERIAL の枠数が activity 別の上限 (65536) を超える。変換の前に開く段で拒否される
        let file = OldFile::new(
            Generation::G2171,
            abi,
            vec![old_act(1, 0x8a, 160, 3), old_act(10, 0x8a, 28, 65_537)],
        );
        let who = format!("serial-over-limit/{}", file.label());
        let err = SaFile::from_bytes(&who, file.bytes()).expect_err("枠数の上限を超えている");
        assert!(matches!(err, Error::LimitExceeded { .. }), "{who}: {err}");
    }
}

// --- 6.12 CLI (`resarch sadf -c`) ---

/// `resarch sadf -c` が変換結果を stdout に、報告を stderr に出すこと。
///
/// 変換後のバイナリは stdout だけに出し、進捗や注意を混ぜない (§5.1)。
#[test]
fn sadf_c_writes_the_conversion_to_stdout_and_the_report_to_stderr() {
    let dir = tempfile::tempdir().expect("一時ディレクトリ");
    let run = |args: &[&str], path: &Path| -> std::process::Output {
        assert_cmd::Command::cargo_bin("resarch")
            .expect("resarch バイナリ")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .arg("sadf")
            .args(args)
            .arg(path)
            .output()
            .expect("起動できる")
    };
    let write = |name: &str, bytes: &[u8]| -> PathBuf {
        let p = dir.path().join(name);
        std::fs::write(&p, bytes).expect("fixture を書ける");
        p
    };

    // --- 通常の変換 ---
    let input = write(
        "sa-2173",
        &sentinel_fixture(Generation::G2173, FixtureAbi::Le64).bytes(),
    );
    let src = SaFile::open(&input).expect("開ける");
    let (expected, _) = convert_bytes(&src, None);
    let out = run(&["-c"], &input);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{err}");
    assert_eq!(out.stdout, expected, "stdout は変換結果そのもの");
    assert!(
        err.contains(&format!("{} バイトを書き出しました", expected.len())),
        "{err}"
    );
    assert!(
        err.contains("activity 8 種 / レコード 4 件 / HZ 100 — 既定 USER_HZ 100"),
        "{err}"
    );
    assert!(!err.contains("警告"), "{err}");

    // --- -O hz=<値> は明示指定として報告し、sa_hz に効く ---
    let out = run(&["-c", "-O", "hz=250"], &input);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{err}");
    assert_eq!(out.stdout, convert_bytes(&src, Some(250)).0);
    assert!(err.contains("HZ 250 — 明示指定"), "{err}");
    let o = walk_output(&out.stdout, FixtureAbi::Le64);
    assert_eq!(o.hz, 250, "sa_hz");
    assert_record_headers(&o, 250, "sadf -c -O hz=250");

    // --- 素通しと切り詰めは警告として stderr に出る ---
    let e = Enc::of(FixtureAbi::Le32);
    let unknown = old_act(200, 0x8a, 8, 1);
    let warn = OldFile::new(
        Generation::G2171,
        FixtureAbi::Le32,
        vec![old_act(1, 0x8a, 160, 3), old_act(11, 0x8a, 80, 2), unknown],
    )
    .stats(vec![
        cpus3(e, 1),
        [
            disk_8a(e, 8, 0, (1 << 32) + 1),
            disk_8a(e, 8, 16, (1 << 32) + 2),
        ]
        .concat(),
        pattern(8, 0x11),
    ]);
    let input = write("sa-warn", &warn.bytes());
    let out = run(&["-c"], &input);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{err}");
    assert!(
        err.contains("警告: 2 個の値が出力側の幅に収まらず切り詰められました"),
        "{err}"
    );
    assert!(
        err.contains("警告: 構造を解釈できない activity (A_UNKNOWN(200))"),
        "{err}"
    );

    // --- 既に現行形式なら何も書かない ---
    let current = write("sa-2175", &expected);
    let out = run(&["-c"], &current);
    assert!(out.status.success());
    assert!(out.stdout.is_empty(), "既に現行形式なら stdout は空");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("File format already up-to-date"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // --- 変換できない世代 (0x2170) は失敗し、stdout に何も出さない ---
    let old = write(
        "sa-2170",
        &fixtures::minimal(Generation::G2170, FixtureAbi::Le64).bytes,
    );
    let out = run(&["-c"], &old);
    assert!(!out.status.success(), "0x2170 は変換できない");
    assert!(out.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("変換できません"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// HZ の指定が 0 なら既定の USER_HZ (100) に落ち、報告の文言がその出所を示すこと。
///
/// `sadf -c -O hz=0` は数字としては正しいので引数解析を通る。0 で割らないこと。
#[test]
fn a_zero_hz_falls_back_to_user_hz() {
    let file = sentinel_fixture(Generation::G2171, FixtureAbi::Le64);
    let src = SaFile::from_bytes("hz-zero", file.bytes()).expect("自作 fixture を開ける");
    let (out, report) = convert_bytes(&src, Some(0));
    assert_eq!(
        (report.hz, report.hz_source),
        (100, HzSource::Fallback),
        "0 は未指定と同じ"
    );
    assert_eq!(report.hz_source.describe(), "既定 USER_HZ 100");
    assert_eq!(HzSource::Explicit.describe(), "明示指定");
    assert_eq!(report.total_records(), 4);
    assert_eq!(walk_output(&out, FixtureAbi::Le64).hz, 100);
}

// --- 6.13 本家 `sadf -c` との突き合わせ (任意) ---

/// 本家 `sadf -c` に期待する結果。
#[derive(Debug, Clone, Copy)]
enum Upstream {
    /// 本家も変換に成功し、変換結果がバイト単位で一致する。
    Same,
    /// 本家は変換を中断する (終了コード)。reSARch は意図的に続行する。
    Aborts(i32),
}

/// 本家と突き合わせる自作 fixture。
///
/// `A_DISK` の旧 `0x8a` 版 ([`wide_value_fixture`]) は入れない。本家の
/// `upgrade_stats_disk()` は `unsigned long` の縮小・拡幅をホストの型で行うので、
/// 32bit / バイト順不一致のファイルでは値やスロットの後半が reSARch と食い違う
/// (§5.8 の「要検証」4)。[`irq_shrink_fixture`] も本家では境界を失う細工なので入れない。
fn upstream_cases() -> Vec<(String, OldFile, Upstream)> {
    let mut cases = Vec::new();
    for (generation, abi) in old_generations() {
        let f = sentinel_fixture(generation, abi);
        cases.push((format!("sentinel/{}", f.label()), f, Upstream::Same));
        // 未知 ID: get_activity_position[200]: Internal error (exit 1)
        let f = opaque_fixture(generation, abi);
        cases.push((format!("opaque/{}", f.label()), f, Upstream::Aborts(1)));
    }
    for abi in FixtureAbi::ALL {
        let f = zero_count_fixture(abi);
        cases.push((format!("zero-count/{}", f.label()), f, Upstream::Same));
        let f = cpu_restart_fixture(abi);
        cases.push((format!("cpu-restart/{}", f.label()), f, Upstream::Same));
        let f = record_type_fixture(abi, R_LAST_STATS);
        cases.push((format!("last-stats/{}", f.label()), f, Upstream::Same));
        let f = record_type_fixture(abi, 7);
        cases.push((format!("record-type-7/{}", f.label()), f, Upstream::Same));
        // CPU activity not found in file. Aborting... (exit 2)
        let f = no_cpu_fixture(abi);
        cases.push((format!("no-cpu/{}", f.label()), f, Upstream::Aborts(2)));
    }
    cases
}

/// 自作 fixture を本家 `sadf -c` にも通し、変換結果がバイト単位で一致すること。
///
/// 本家のバイナリは GPL なので同梱しない。`RESARCH_UPSTREAM_SADF` に本家 12.8.0 の
/// `sadf` のパスを与えたときだけ比べる (未設定ならスキップ)。HZ は実行ホストに
/// 依らないよう `-O hz=100` を渡す (reSARch の既定と同じ値)。
///
/// 比較から外すのは 1 か所だけ: ホストとバイト順が逆のファイルでは、本家の
/// `upgrade_record_header()` が `ust_time` (32bit ライタのファイルでは `uptime_cs` も) を
/// 壊す (本家は旧形式 × バイト順不一致のテストデータを持たない。§5.8 の「要検証」6)。
/// reSARch は正しい値を書くことを [`assert_record_headers`] で確かめているので、
/// その 16 バイト (`uptime_cs` / `ust_time`) だけを両方から伏せて比べる。
#[test]
#[ignore = "本家 sadf (GPL) が必要。RESARCH_UPSTREAM_SADF=<sadf のパス> を与えて --include-ignored で実行する"]
fn crafted_conversions_match_upstream_sadf_c() {
    let Some(sadf) = std::env::var_os("RESARCH_UPSTREAM_SADF") else {
        eprintln!("skipped: RESARCH_UPSTREAM_SADF (本家 sadf のパス) が未設定");
        return;
    };
    let dir = tempfile::tempdir().expect("一時ディレクトリ");
    let host_big = cfg!(target_endian = "big");

    for (name, file, expect) in upstream_cases() {
        let input = file.bytes();
        let path = dir.path().join(name.replace('/', "_"));
        std::fs::write(&path, &input).expect("fixture を書ける");
        let out = std::process::Command::new(&sadf)
            .env("LC_ALL", "C")
            .args(["-c", "-O", "hz=100"])
            .arg(&path)
            .output()
            .expect("本家 sadf を起動できる");
        let stderr = String::from_utf8_lossy(&out.stderr);

        match expect {
            Upstream::Aborts(code) => {
                assert_eq!(out.status.code(), Some(code), "{name}: {stderr}");
                println!("[upstream] {name}: 本家は exit {code} で中断 (reSARch は続行)");
            }
            Upstream::Same => {
                assert!(out.status.success(), "{name}: 本家が失敗した: {stderr}");
                let src = SaFile::from_bytes(&name, input).expect("自作 fixture を開ける");
                let (mut ours, _) = convert_bytes(&src, None);
                let mut theirs = out.stdout;
                assert_eq!(ours.len(), theirs.len(), "{name}: 変換結果の長さ");
                let masked = file.abi.is_big_endian() != host_big;
                if masked {
                    for r in walk_output(&ours, file.abi).records {
                        ours[r.offset..r.offset + 16].fill(0);
                        theirs[r.offset..r.offset + 16].fill(0);
                    }
                }
                let first_diff = ours.iter().zip(&theirs).position(|(a, b)| a != b);
                assert_eq!(
                    first_diff, None,
                    "{name}: 本家の変換結果と食い違う (最初の差: offset {first_diff:?})"
                );
                println!(
                    "[upstream] {name}: 一致{}",
                    if masked {
                        " (record_header の時刻を除く)"
                    } else {
                        ""
                    }
                );
            }
        }
    }
}

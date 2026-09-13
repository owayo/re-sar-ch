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

mod fixtures;
mod golden;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use fixtures::{ActivitySpec, FixtureAbi, FixtureSpec, Generation, RecordSpec};
use golden::Mask;

use re_sar_ch::cli::sar_args::{Activity, OptFlags, SarOptions, parse_sar_args};
use re_sar_ch::convert::{self, ConvertOptions, ConvertReport, HzSource};
use re_sar_ch::format::{SaFile, ScanControl};
use re_sar_ch::model::{ActivityId, Availability};
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

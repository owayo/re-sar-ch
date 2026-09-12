//! 本家 sysstat との突合テスト。
//!
//! # 位置づけ
//!
//! 本家データは GPL-2.0-or-later なので同梱できない (`docs/format/04-test-data.md` §6)。
//! `cargo run --bin xtask -- fetch-fixtures` (= `make fixtures`) で
//! `target/fixtures/upstream/` へ取得したうえで、次のコマンドで走らせる。
//!
//! ```sh
//! cargo test --test conformance -- --include-ignored --nocapture
//! ```
//!
//! 取得していない場合は**何も失敗させずスキップ**する。
//! `cargo test` (既定) では `#[ignore]` により実行されないので、
//! ネットワークが無い環境でもテスト全体は成功する。
//!
//! # 何を検証しているか
//!
//! 1. 取得物が揃っていること (素性の記録つき)
//! 2. 本家データのヘッダが `04-test-data.md` §2.1 の実測値どおりに読めること
//! 3. **本家データを本体 (`SaFile` / `SaFile::scan` / `series::walk`) に読ませて**、
//!    正常系は末尾まで余りなく読め、異常系は期待した分類のエラーで拒否されること
//!    ([`upstream_files_are_read_or_rejected_as_expected`])
//! 4. 自作 fixture (`tests/fixtures`) と本家の異常系ファイルが、
//!    **同じ段・同じエラー分類**で拒否されること
//!    ([`self_made_and_upstream_error_files_are_rejected_alike`])。
//!    自作 fixture が本家と同じ検査を突いていることの裏取りになる。
//! 5. **本家の期待出力との全文比較** ([`golden_outputs_match_upstream`])。
//!    [`GOLDEN_CASES`] の各ケースについて、入力 `sa` ファイルを [`SaFile`] で開き、
//!    本家のコマンドラインに相当する出力をライブラリ API 経由で生成して
//!    `expected*` と 1 行ずつ突き合わせる。これが `sar` 互換の中核検証である。
//!
//! # 比較の方式
//!
//! **プロセスは起動しない。**出力層 (`output::sar_text` / `output::sadf`) を
//! 直接呼ぶ。理由は 2 つある。
//!
//! - 差分が出たとき、CLI 層の引数解釈と出力層の書式のどちらが原因かを
//!   切り分けずに済む (引数は [`parse_sar_args`] に通すので解釈も検証される)
//! - `TZ` / `LC_ALL` といった環境変数に頼らずに時刻基準を指定できる
//!   (本家テストの `TZ=GMT` は [`TimeStyle::Utc`] で表現する)
//!
//! 再現できない箇所のマスクは [`golden::Mask`] に理由つきで宣言し、
//! 「どの語を潰したか」を報告に必ず出す (`--nocapture` で読める)。

mod fixtures;
mod golden;

use std::path::{Path, PathBuf};
use std::process::Command;

use fixtures::{Corruption, ExpectedError, FixtureAbi};
use golden::{Comparison, Mask};

use re_sar_ch::Error;
use re_sar_ch::cli::sar_args::{Activity, OptFlags, SarOptions, parse_sar_args};
use re_sar_ch::format::abi::{Endian, LayoutAbi, SourceEncoding};
use re_sar_ch::format::reader::Cursor;
use re_sar_ch::format::wire::ResolvedLayout;
use re_sar_ch::format::{SaFile, ScanControl, layouts, selfdesc};
use re_sar_ch::model::ActivityId;
use re_sar_ch::output::sadf;
use re_sar_ch::output::sar_text::{self, CpuSelection, SarTextOptions, TimeStyle};
use re_sar_ch::series::{Selection, walk};

// ===========================================================================
// 取得物の発見
// ===========================================================================

/// 取得物の置き場所 (`xtask fetch-fixtures` の保存先と一致させる)。
const UPSTREAM_SUBDIR: &str = "fixtures/upstream";

/// `xtask` が書き残す素性ファイル。これがあれば取得は完了している。
const PROVENANCE_FILE: &str = "PROVENANCE.txt";

/// 未取得時の案内。
const HOW_TO_FETCH: &str = "本家 fixture が無いのでスキップした。\n\
     取得するには `make fixtures` (= cargo run --bin xtask -- fetch-fixtures) を実行する。\n\
     本家データは GPL-2.0-or-later のためリポジトリには同梱していない。";

/// `target/fixtures/upstream` を探す。
///
/// `CARGO_TARGET_DIR` を尊重し、無ければ `CARGO_MANIFEST_DIR/target` を見る。
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

/// 取得済みなら fixture ディレクトリを返し、未取得なら案内を出して `None`。
///
/// 各テストの冒頭で呼び、`None` なら早期 return する (失敗させない)。
fn upstream_or_skip(test: &str) -> Option<PathBuf> {
    match upstream_dir() {
        Some(d) => Some(d),
        None => {
            eprintln!("skipped: {test}: {HOW_TO_FETCH}");
            None
        }
    }
}

/// 本家データ 1 件のパス。無ければ案内を出して `None`。
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
// 比較ケース表 (docs/format/04-test-data.md §4.2 / §5)
// ===========================================================================

/// golden 比較 1 件。
struct GoldenCase {
    /// 本家テスト番号 (`tests/00650` など)。追跡用。
    upstream_test: &'static str,
    /// 入力データのファイル名。
    data: &'static str,
    /// 本家側のコマンドライン (再現の参照用)。
    upstream_cmd: &'static str,
    /// 期待出力のファイル名。
    golden: &'static str,
    /// reSARch 側でこれに対応させるフェーズ (`04-test-data.md` §5)。
    phase: Phase,
    /// 期待出力を reSARch 側で再現する方法。
    repro: Repro,
    /// このケースで許すマスク。空なら全文一致が要求される。
    masks: &'static [Mask],
}

/// 期待出力の再現方法。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Repro {
    /// `sar` 互換テキスト。要素は本家のコマンドラインの引数そのまま
    /// (`-f <data>` はテスト側で足す)。[`parse_sar_args`] に通してから
    /// [`sar_text::write_report`] を呼ぶ。
    Sar(&'static [&'static str]),
    /// `sadf -H`。本家テストは `| grep -v 0x2175` を通すので、
    /// reSARch 側の出力からも同じ行を落とす (先頭行が入力パスを含むため)。
    SadfHeader,
    /// reSARch にその出力形式が無く、比較できないケース。
    Unsupported {
        /// 何が無いのか。報告にそのまま出す。
        missing: &'static str,
    },
}

/// `sadf -H` の 1 行目を落とすための語 (本家テストの `grep -v 0x2175` と同じ)。
const SADF_H_GREP_V: &str = "0x2175";

const GOLDEN_CASES: &[GoldenCase] = &[
    GoldenCase {
        upstream_test: "00655",
        data: "data-12.0.0",
        upstream_cmd: "LC_ALL=C TZ=GMT sadf -H <data> | grep -v 0x2175",
        golden: "expected.data-12.0.0-H",
        phase: Phase::Header,
        repro: Repro::SadfHeader,
        masks: &[Mask::HostLineDate],
    },
    GoldenCase {
        upstream_test: "00787",
        data: "data-ukwn",
        upstream_cmd: "LC_ALL=C sadf -H <data> | grep -v 0x2175",
        golden: "expected.sadf-data-ukwn",
        phase: Phase::Header,
        repro: Repro::SadfHeader,
        masks: &[Mask::HostLineDate],
    },
    GoldenCase {
        upstream_test: "00791",
        data: "data-ukwn0",
        upstream_cmd: "LC_ALL=C sadf -H <data> | grep -v 0x2175",
        golden: "expected.sadf-data-ukwn0",
        phase: Phase::Header,
        repro: Repro::SadfHeader,
        masks: &[Mask::HostLineDate],
    },
    GoldenCase {
        upstream_test: "00794",
        data: "data-ukwn1",
        upstream_cmd: "LC_ALL=C sadf -H <data> | grep -v 0x2175",
        golden: "expected.sadf-data-ukwn1",
        phase: Phase::Header,
        repro: Repro::SadfHeader,
        masks: &[Mask::HostLineDate],
    },
    GoldenCase {
        upstream_test: "00650",
        data: "data-12.0.0",
        upstream_cmd: "LC_ALL=C TZ=GMT sar -AC -f <data>",
        golden: "expected.data-12.0.0",
        phase: Phase::SarText,
        repro: Repro::Sar(&["-AC"]),
        masks: &[Mask::DiskDeviceName],
    },
    GoldenCase {
        upstream_test: "00700",
        data: "data-ppc-11.7.2",
        upstream_cmd: "LC_ALL=C TZ=GMT sar -C -A -f <data>",
        golden: "expected.data-ppc-11.7.2",
        phase: Phase::SarText,
        repro: Repro::Sar(&["-C", "-A"]),
        masks: &[],
    },
    GoldenCase {
        upstream_test: "00740",
        data: "data-non-printable",
        upstream_cmd: "LC_ALL=C TZ=GMT sar -C -f <data>",
        golden: "expected.sar-non-printable",
        phase: Phase::SarText,
        repro: Repro::Sar(&["-C"]),
        masks: &[],
    },
    GoldenCase {
        upstream_test: "00760",
        data: "data-12.5.6-A_QUEUE_modified",
        upstream_cmd: "LC_ALL=C TZ=GMT sar -A -f <data>",
        golden: "expected.data-12.5.6-A_QUEUE_modified",
        phase: Phase::SarText,
        repro: Repro::Sar(&["-A"]),
        masks: &[],
    },
    GoldenCase {
        upstream_test: "00770",
        data: "data-extra-12.1.7",
        upstream_cmd: "LC_ALL=C TZ=GMT sar -A -f <data>",
        golden: "expected.data-extra-12.1.7",
        phase: Phase::SarText,
        repro: Repro::Sar(&["-A"]),
        masks: &[],
    },
    GoldenCase {
        upstream_test: "00780",
        data: "data-ukwn",
        upstream_cmd: "LC_ALL=C TZ=GMT sar -P ALL -f <data>",
        golden: "expected.sar-data-ukwn",
        phase: Phase::SarText,
        repro: Repro::Sar(&["-P", "ALL"]),
        masks: &[],
    },
    GoldenCase {
        upstream_test: "00784",
        data: "data-ukwn",
        upstream_cmd: "LC_ALL=C TZ=GMT sar -w -f <data>",
        golden: "expected2.sar-data-ukwn",
        phase: Phase::SarText,
        repro: Repro::Sar(&["-w"]),
        masks: &[],
    },
    GoldenCase {
        upstream_test: "00793",
        data: "data-ukwn1",
        upstream_cmd: "LC_ALL=C TZ=GMT sar -uq -f <data>",
        golden: "expected3.sar-data-ukwn",
        phase: Phase::SarText,
        repro: Repro::Sar(&["-uq"]),
        masks: &[],
    },
    GoldenCase {
        upstream_test: "01405",
        data: "data-trunc",
        upstream_cmd: "LC_ALL=C TZ=GMT sadf -g <data> -- -A",
        golden: "expected.sadf-g-trunc",
        phase: Phase::RawValues,
        // `-g` は SVG グラフ。reSARch には対応する出力形式が無い。
        repro: Repro::Unsupported {
            missing: "sadf -g (SVG) 相当の出力形式",
        },
        masks: &[],
    },
    // 旧世代 (0x2171 / 0x2173) の golden は「sadf -c で変換したファイル」に対するもの。
    // reSARch は直読するので、変換を挟まず同じ引数を元ファイルへ当てる
    // (`04-test-data.md` §4.3: `sadf -c` は構造体の移動と型拡張だけで値を変えない)。
    GoldenCase {
        upstream_test: "00605",
        data: "data-9.1.6",
        upstream_cmd: "LC_ALL=C TZ=GMT sadf -c <data> > tmp && sar -C -A -f tmp",
        golden: "expected.data-9.1.6",
        phase: Phase::SarText,
        repro: Repro::Sar(&["-C", "-A"]),
        masks: &[Mask::DiskDeviceName],
    },
    GoldenCase {
        upstream_test: "00615",
        data: "data-10.3.1",
        upstream_cmd: "LC_ALL=C TZ=GMT sadf -c <data> > tmp && sar -C -A -f tmp",
        golden: "expected.data-10.3.1",
        phase: Phase::SarText,
        repro: Repro::Sar(&["-C", "-A"]),
        masks: &[Mask::DiskDeviceName],
    },
    GoldenCase {
        upstream_test: "00625",
        data: "data-11.6.5",
        upstream_cmd: "LC_ALL=C TZ=GMT sadf -c <data> > tmp && sar -C -A -f tmp",
        golden: "expected.data-11.6.5",
        phase: Phase::SarText,
        repro: Repro::Sar(&["-C", "-A"]),
        masks: &[Mask::DiskDeviceName],
    },
];

/// 期待出力を持たず「エラーメッセージと終了コードだけ」を見るケース (§4.2 の後半)。
struct ErrorCase {
    upstream_test: &'static str,
    data: &'static str,
    /// stderr に含まれるべき語 (本家の英語メッセージ)。reSARch は日本語なので
    /// **そのまま比較はしない**。「拒否されること」と「理由の分類」を対応させるための記録。
    upstream_message: &'static str,
    /// ヘッダ表示 (`resarch info` 相当) でも失敗すべきか。
    fails_header_only: bool,
}

/// 異常系の全件 (`04-test-data.md` §4.2 / §10 のチェックリスト)。
const ERROR_CASES: &[ErrorCase] = &[
    ErrorCase {
        upstream_test: "01452",
        data: "data-9.1.5",
        upstream_message: "cannot read the format of this file",
        fails_header_only: true,
    },
    ErrorCase {
        upstream_test: "01400",
        data: "data-trunc",
        upstream_message: "End of system activity file unexpected",
        fails_header_only: false,
    },
    ErrorCase {
        upstream_test: "00732",
        data: "data-12.6.0-file_act-types_nr-SARerr",
        upstream_message: "Invalid system activity file",
        fails_header_only: false,
    },
    ErrorCase {
        upstream_test: "00734",
        data: "data-12.7.1-A_IRQ_overflow",
        upstream_message: "Aborting",
        fails_header_only: false,
    },
];

/// `tests/00730` が glob で回す 13 本。すべて拒否されなければならない。
const HEADER_ERROR_DATA: &[&str] = &[
    "data-12.6.0-file_hdr-sa_act_nr-err",
    "data-12.6.0-file_hdr-MAP_SIZE_act_types_nr-err",
    "data-12.6.0-file_hdr-MAP_SIZE_rec_types_nr-err",
    "data-12.6.0-file_hdr-act_size-err",
    "data-12.6.0-file_hdr-rec_size-err",
    "data-12.6.0-file_act-nr-0-err",
    "data-12.6.0-file_act-nr-err",
    "data-12.6.0-file_act-nr-nr_max-err",
    "data-12.6.0-file_act-nr2-0-err",
    "data-12.6.0-file_act-nr2-err",
    "data-12.6.0-file_act-size-0-err",
    "data-12.6.0-file_act-size-err",
    "data-12.6.0-file_act-MAP_SIZE_types_nr-err",
];

// ===========================================================================
// 期待出力の再現 (ライブラリ API を直接呼ぶ)
// ===========================================================================
//
// `resarch` をプロセスとして起動はしない。CLI (`src/main.rs`) は出力層へ
// まだ繋がっておらず、また環境変数 (`TZ` / `LC_ALL`) に依存させると
// 「差分の原因が環境か実装か」を切り分けられなくなる。
// 代わりに `sar` の引数列を [`parse_sar_args`] へ通し、その結果を
// 出力層のオプションへ写して [`sar_text::write_report`] を呼ぶ。

/// `sar` 互換テキストを生成する。
///
/// `args` は本家のコマンドライン (`-AC` など) そのまま。`-f <path>` は
/// ここで補う (`parse_sar_args` は入力先が決まらないと `finalize` を通らない)。
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

/// `sadf -H` 相当を生成し、本家テストの `grep -v 0x2175` と同じ行を落とす。
fn render_sadf_header(file: &SaFile) -> Result<String, String> {
    let mut buf: Vec<u8> = Vec::new();
    sadf::header::write_header(&mut buf, file).map_err(|e| format!("sadf -H を書けない: {e}"))?;
    let text = String::from_utf8(buf).map_err(|e| format!("出力が UTF-8 でない: {e}"))?;
    Ok(text
        .lines()
        .filter(|l| !l.contains(SADF_H_GREP_V))
        .map(|l| format!("{l}\n"))
        .collect())
}

/// [`SarOptions`] (CLI 層) を [`SarTextOptions`] (出力層) へ写す。
///
/// 時刻は [`TimeStyle::Utc`] にする。本家テストは `TZ=GMT` を明示しており、
/// `sar` 既定のローカル時刻表示は GMT 環境では UTC 表示と一致する。
/// `-t` (`true_time`) のときだけレコードに焼き込まれた時分秒を使う。
fn sar_text_options(o: &SarOptions) -> SarTextOptions {
    let bitmap = &o.cpu_bitmap;
    let cpus = if bitmap.count_bits() == bitmap.capacity_bits() {
        // `-P ALL` / `-A` は全ビットを立てる
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
    }
}

/// 不一致ケースの出力全体を `target/golden-actual/` へ書き出す。
///
/// 行単位の要約では追えない食い違い (ブロックの増減・順序) を
/// `diff -u <expected> <actual>` で追えるようにする。書けなければ諦める
/// (診断の補助なので、失敗をテストの失敗にしない)。
fn dump_actual(case: &GoldenCase, actual: &str) -> Option<PathBuf> {
    let dir = upstream_dir()?.parent()?.join("golden-actual");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("{}.{}", case.golden, case.upstream_test));
    std::fs::write(&path, actual).ok()?;
    Some(path)
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
// 本家データのヘッダを、本体のレイアウト層だけで読む
// ===========================================================================

/// 本家データから読み出したヘッダの主要値。
#[derive(Debug, PartialEq, Eq)]
struct UpstreamHeader {
    endian: Endian,
    format_magic: u16,
    /// `file_magic` が申告する `header_size` (持たない世代は `None`)。
    header_size: Option<u32>,
    upgraded: u32,
    hdr_types_nr: Option<[u32; 3]>,
    act_types_nr: Option<[u32; 3]>,
    rec_types_nr: Option<[u32; 3]>,
    act_size: Option<u32>,
    rec_size: Option<u32>,
    ust_time: u64,
    /// `sa_hz`。自己記述世代のみ。**`unsigned long` なので ABI 差が出る**。
    hz: Option<u64>,
    cpu_nr: Option<u32>,
    act_nr: u32,
    vol_act_nr: Option<u32>,
    day: u8,
    month: u8,
    year: i64,
    sizeof_long: i64,
    machine: String,
    tzname: Option<String>,
}

/// `file_magic` を読んでバイト順と世代を決め、`file_header` を読む。
///
/// オフセットは ABI に依存しないので、まず暫定 ABI で解決して
/// `sa_machine` / `sa_sizeof_long` を読み、本当の ABI で解決し直す
/// (`docs/format/01-file-format.md` §1.4 の流れ)。
fn read_upstream_header(bytes: &[u8]) -> UpstreamHeader {
    // ① 先頭 2 バイトでバイト順を決める (§7.1)
    let endian = match u16::from_le_bytes([bytes[0], bytes[1]]) {
        0xd596 => Endian::Little,
        0x96d5 => Endian::Big,
        other => panic!("sysstat のファイルではない: 0x{other:04x}"),
    };

    let probe = SourceEncoding::new(endian, LayoutAbi::LP64);
    let magic_layout = {
        let c = Cursor::new(bytes, endian);
        let format_magic = c.u16_at(2).unwrap();
        match format_magic {
            0x2170 | 0x2171 => layouts::FILE_MAGIC_G1,
            0x2173 => layouts::FILE_MAGIC_G2,
            0x2175 => layouts::FILE_MAGIC_G3,
            other => panic!("未対応の format_magic: 0x{other:04x}"),
        }
    };
    let magic = magic_layout.resolve(&probe).expect("file_magic の解決");
    let c = Cursor::new(bytes, endian);
    let format_magic = c
        .read_unsigned(0, magic.field("format_magic").unwrap())
        .unwrap() as u16;
    let header_size = magic
        .field("header_size")
        .map(|f| c.read_unsigned(0, f).unwrap() as u32);
    let upgraded = magic
        .field("upgraded")
        .map(|f| c.read_unsigned(0, f).unwrap() as u32)
        .unwrap_or(0);
    let hdr_types_nr = magic.field("hdr_types_nr_0").map(|_| {
        [
            c.read_unsigned(0, magic.field("hdr_types_nr_0").unwrap())
                .unwrap() as u32,
            c.read_unsigned(0, magic.field("hdr_types_nr_1").unwrap())
                .unwrap() as u32,
            c.read_unsigned(0, magic.field("hdr_types_nr_2").unwrap())
                .unwrap() as u32,
        ]
    });

    let fh_off = magic.size;
    let resolve_header = |enc: &SourceEncoding| -> ResolvedLayout {
        match format_magic {
            0x2170 | 0x2171 => layouts::FILE_HEADER_G1.resolve(enc).unwrap(),
            0x2173 => layouts::FILE_HEADER_G2.resolve(enc).unwrap(),
            _ => selfdesc::resolve_file_header(
                selfdesc::TypesNr(hdr_types_nr.expect("0x2175 は hdr_types_nr を持つ")),
                header_size.expect("0x2175 は header_size を持つ") as usize,
                enc,
            )
            .unwrap(),
        }
    };

    // ② 暫定 ABI で解決して sa_sizeof_long / sa_machine を読む (オフセットは ABI 非依存)
    let provisional = resolve_header(&probe);
    let sizeof_long = c
        .read_signed(fh_off, provisional.field("sa_sizeof_long").unwrap())
        .unwrap();
    let machine = c
        .read_str_lossy(fh_off, provisional.field("sa_machine").unwrap())
        .unwrap()
        .into_owned();

    // ③ 本当の ABI で解決し直す
    let abi = LayoutAbi::infer(&machine, sizeof_long as u8)
        .unwrap_or_else(|| panic!("ABI を推定できない: machine={machine} szl={sizeof_long}"));
    let enc = SourceEncoding::new(endian, abi);
    let fh = resolve_header(&enc);

    let get_u32 = |name: &str| {
        fh.field(name)
            .map(|f| c.read_unsigned(fh_off, f).unwrap() as u32)
    };
    let triple = |p: &str| -> Option<[u32; 3]> {
        Some([
            get_u32(&format!("{p}_0"))?,
            get_u32(&format!("{p}_1"))?,
            get_u32(&format!("{p}_2"))?,
        ])
    };

    UpstreamHeader {
        endian,
        format_magic,
        header_size,
        upgraded,
        hdr_types_nr,
        act_types_nr: triple("act_types_nr"),
        rec_types_nr: triple("rec_types_nr"),
        act_size: get_u32("act_size"),
        rec_size: get_u32("rec_size"),
        ust_time: c
            .read_unsigned(fh_off, fh.field("sa_ust_time").unwrap())
            .unwrap(),
        hz: fh
            .field("sa_hz")
            .map(|f| c.read_unsigned(fh_off, f).unwrap()),
        cpu_nr: get_u32("sa_cpu_nr").or_else(|| get_u32("sa_last_cpu_nr")),
        act_nr: get_u32("sa_act_nr")
            .or_else(|| get_u32("sa_nr_act"))
            .expect("activity 数が読めない"),
        vol_act_nr: get_u32("sa_vol_act_nr"),
        day: c
            .read_unsigned(fh_off, fh.field("sa_day").unwrap())
            .unwrap() as u8,
        month: c
            .read_unsigned(fh_off, fh.field("sa_month").unwrap())
            .unwrap() as u8,
        year: c.read_signed(fh_off, fh.field("sa_year").unwrap()).unwrap(),
        sizeof_long,
        machine,
        tzname: fh
            .field("sa_tzname")
            .map(|f| c.read_str_lossy(fh_off, f).unwrap().into_owned()),
    }
}

/// `sar` / `sadf` が使えるか。案 C (実 sysstat との突合) のスキップ判定。
fn sysstat_version() -> Option<String> {
    let out = Command::new("sar")
        .arg("-V")
        .env("LC_ALL", "C")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines().next().map(|l| l.trim().to_string())
}

// ===========================================================================
// テスト
// ===========================================================================

/// 取得物が揃っていて、素性の記録が読めること。
///
/// 「取得が壊れていないか」を最初に落とすための確認。
#[test]
#[ignore = "本家データ (GPL) が必要。make fixtures 後 --include-ignored で実行する"]
fn upstream_fixtures_are_present_and_recorded() {
    let Some(dir) = upstream_or_skip("upstream_fixtures_are_present_and_recorded") else {
        return;
    };

    let provenance =
        std::fs::read_to_string(dir.join(PROVENANCE_FILE)).expect("PROVENANCE.txt が読めない");
    assert!(
        provenance.contains("tag    ="),
        "PROVENANCE.txt に取得タグが記録されていない"
    );
    eprintln!(
        "本家 fixture: {}\n{}",
        dir.display(),
        provenance
            .lines()
            .filter(|l| l.starts_with("url") || l.starts_with("tag") || l.starts_with("commit"))
            .collect::<Vec<_>>()
            .join("\n")
    );

    // 比較ケースが参照するファイルがすべて存在すること
    let mut missing = Vec::new();
    for case in GOLDEN_CASES {
        for name in [case.data, case.golden] {
            if !dir.join(name).is_file() {
                missing.push(name);
            }
        }
    }
    for case in ERROR_CASES {
        if !dir.join(case.data).is_file() {
            missing.push(case.data);
        }
    }
    for name in HEADER_ERROR_DATA {
        if !dir.join(name).is_file() {
            missing.push(name);
        }
    }
    missing.sort_unstable();
    missing.dedup();
    assert!(missing.is_empty(), "取得物に不足がある: {missing:?}");
}

/// 本家データのヘッダが、`docs/format/04-test-data.md` §2.1 の**実測値**どおりに読めること。
///
/// これは出力系を待たずに今すぐ回せる実質的な突合である。
/// 自作 fixture は「ドキュメントのオフセット表から独立に書き下ろしたバイト列」だが、
/// このテストは「本家が実際に書いたバイト列」を本体のレイアウト層で読む。
/// 両方が通って初めてレイアウト定義が正しいと言える (`docs/design.md` §8)。
///
/// **ホスト名 (`sa_nodename`) と `sa_release` は検証対象にしない。**
/// 本家データには実ホスト名が入っており、公開リポジトリに持ち込まないため
/// (`docs/design.md` §8.1)。`sa_machine` は汎用のアーキテクチャ名なので検証する。
#[test]
#[ignore = "本家データ (GPL) が必要。make fixtures 後 --include-ignored で実行する"]
fn upstream_headers_decode_to_the_measured_facts() {
    let Some(dir) = upstream_or_skip("upstream_headers_decode_to_the_measured_facts") else {
        return;
    };

    // 取得物が欠けていると検証が空振りするので、実際に読めた件数を数えて最後に確かめる。
    let mut checked = 0usize;
    let mut read = |name: &str| -> Option<UpstreamHeader> {
        let path = upstream_file(&dir, name)?;
        let bytes = std::fs::read(path).expect("本家データが読めない");
        checked += 1;
        Some(read_upstream_header(&bytes))
    };

    // --- 0x2170 (変換不能な最古世代) ---
    if let Some(h) = read("data-9.1.5") {
        assert_eq!(h.format_magic, 0x2170);
        assert_eq!(h.endian, Endian::Little);
        assert_eq!(h.header_size, None, "この世代は header_size を持たない");
        assert_eq!(h.ust_time, 1_623_329_128);
        assert_eq!(h.act_nr, 13);
        assert_eq!(h.sizeof_long, 8);
        assert_eq!(h.machine, "x86_64");
    }

    // --- 0x2171 ---
    if let Some(h) = read("data-9.1.6") {
        assert_eq!(h.format_magic, 0x2171);
        assert_eq!(h.ust_time, 1_484_986_571);
        assert_eq!(h.act_nr, 32);
        assert_eq!(h.sizeof_long, 8);
        // sa_month は 0 起点、sa_year は 1900 起点 (2017-01-21)
        assert_eq!((h.day, h.month, h.year), (21, 0, 117));
        assert_eq!(h.hz, None, "この世代に sa_hz は無い");
    }

    // --- 0x2173 (RESTART 後に volatile activity リストが付く世代) ---
    if let Some(h) = read("data-10.3.1") {
        assert_eq!(h.format_magic, 0x2173);
        assert_eq!(h.header_size, Some(288));
        assert_eq!(h.ust_time, 1_484_986_496);
        assert_eq!(h.act_nr, 34);
        assert_eq!(h.vol_act_nr, Some(2));
        assert_eq!(h.cpu_nr, Some(9));
    }
    if let Some(h) = read("data-11.6.5") {
        assert_eq!(h.format_magic, 0x2173);
        assert_eq!(h.header_size, Some(288));
        assert_eq!(h.ust_time, 1_535_535_218);
        assert_eq!(h.act_nr, 36);
        assert_eq!(h.vol_act_nr, Some(2));
        assert_eq!((h.day, h.month, h.year), (29, 7, 118));
    }

    // --- 0x2175 初出形 (hdr_types_nr = (1,1,11) / header_size = 328) ---
    if let Some(h) = read("data-12.0.0") {
        assert_eq!(h.format_magic, 0x2175);
        assert_eq!(h.header_size, Some(328));
        assert_eq!(h.hdr_types_nr, Some([1, 1, 11]));
        assert_eq!(h.act_types_nr, Some([0, 0, 9]));
        assert_eq!(h.rec_types_nr, Some([2, 0, 0]));
        assert_eq!((h.act_size, h.rec_size), (Some(36), Some(24)));
        assert_eq!(h.upgraded, 0, "生のファイル (sadf -c 未通過)");
        assert_eq!(h.ust_time, 1_561_873_161);
        assert_eq!(h.hz, Some(100));
        assert_eq!(h.cpu_nr, Some(9));
        assert_eq!(h.act_nr, 36);
        assert_eq!((h.day, h.month, h.year), (30, 5, 119), "2019-06-30");
        assert_eq!(h.sizeof_long, 8);
        assert_eq!(h.tzname, None, "この世代に sa_tzname は無い");
    }

    // --- big endian + sizeof(long) = 4。ABI 吸収層の最重要ケース ---
    if let Some(h) = read("data-ppc-11.7.2") {
        assert_eq!(h.endian, Endian::Big);
        assert_eq!(h.format_magic, 0x2175);
        assert_eq!(h.header_size, Some(328));
        assert_eq!(h.hdr_types_nr, Some([1, 1, 11]));
        assert_eq!(h.sizeof_long, 4);
        assert_eq!(h.machine, "ppc");
        // upgraded = 0x703 = (7 << 8) + 2 + 1 → 11.7.2 の sadf -c で変換済み
        assert_eq!(h.upgraded, 0x703);
        assert_eq!(h.ust_time, 1_493_324_675);
        // sa_hz は unsigned long。BE かつ 32bit なので「スロット 8 バイトの先頭 4 バイトを
        // BE で読む」が正しい。8 バイト全部を読むと 100 * 2^32 になる。
        assert_eq!(h.hz, Some(100), "BE + 32bit の unsigned long スロット");
        assert_eq!(h.cpu_nr, Some(17));
        assert_eq!(h.act_nr, 15);
        assert_eq!((h.day, h.month, h.year), (27, 3, 117), "2017-04-27");
        assert_eq!((h.act_size, h.rec_size), (Some(36), Some(24)));
    }

    // --- 0x2175 現行形 (header_size = 336 / sa_tzname あり) ---
    if let Some(h) = read("data-non-printable") {
        assert_eq!(h.header_size, Some(336));
        assert_eq!(h.hdr_types_nr, Some([1, 1, 12]));
        assert_eq!(h.rec_types_nr, Some([2, 0, 1]));
        assert_eq!(h.act_nr, 1);
        assert_eq!(h.tzname.as_deref(), Some("CET"));
    }

    // --- 異常系の基準構成: 自作 fixture (448 バイト) と同じであること ---
    if let Some(h) = read("data-12.6.0-file_act-nr-nr_max-err") {
        assert_eq!(h.header_size, Some(336));
        assert_eq!(h.hdr_types_nr, Some([1, 1, 12]));
        assert_eq!(h.act_nr, 1);
        assert_eq!((h.act_size, h.rec_size), (Some(36), Some(24)));
        assert_eq!(h.cpu_nr, Some(9));
    }

    assert_eq!(checked, 8, "実測値を突合したファイル数");
}

/// **本家の期待出力との全文比較**。
///
/// [`GOLDEN_CASES`] の各ケースについて
///
/// 1. 入力 `sa` ファイルを [`SaFile`] で開く
/// 2. 本家のコマンドラインに相当する出力をライブラリ API で生成する
///    ([`render_sar_text`] / [`render_sadf_header`])
/// 3. `expected*` と 1 行ずつ突き合わせる ([`golden::compare`])
/// 4. 「全文一致 / N 行マスクして一致 / 不一致」をケースごとに出す
///
/// マスクは [`GoldenCase::masks`] に宣言したものだけが効く。
/// 対象は「ファイルの中身からは原理的に再現できない値」に限り、理由を必ず添える。
///
/// 差分の一覧は `--nocapture` で読める。1 件でも不一致が残っていれば失敗する
/// (`sar` 互換が中核価値なので、未達を緑にしない)。
#[test]
#[ignore = "本家データ (GPL) が必要。make fixtures 後 --include-ignored で実行する"]
fn golden_outputs_match_upstream() {
    let Some(dir) = upstream_or_skip("golden_outputs_match_upstream") else {
        return;
    };

    let mut exact = 0usize;
    let mut masked = 0usize;
    let mut failures: Vec<String> = Vec::new();
    let mut compared = 0usize;

    eprintln!(
        "\n=== 本家期待出力との全文比較 ({} 件) ===",
        GOLDEN_CASES.len()
    );
    for case in GOLDEN_CASES {
        let (Some(data), Some(golden)) = (
            upstream_file(&dir, case.data),
            upstream_file(&dir, case.golden),
        ) else {
            failures.push(format!(
                "[{}] {} -> {}: 入力または期待出力が無い",
                case.upstream_test, case.data, case.golden
            ));
            continue;
        };
        let expected = std::fs::read_to_string(&golden).expect("期待出力が読めない");
        assert!(!expected.is_empty(), "{}: 期待出力が空", case.golden);

        let head = format!(
            "[{}] {:?} {} -> {}",
            case.upstream_test, case.phase, case.data, case.golden
        );

        // 未実装の出力形式は「比較できなかった」として明示する (緑にしない)
        if let Repro::Unsupported { missing } = case.repro {
            eprintln!(
                "  比較不能  {head}\n            {missing} が無い ({})",
                case.upstream_cmd
            );
            failures.push(format!("{head}: {missing} が無いため比較できない"));
            continue;
        }

        let actual = match SaFile::open(&data) {
            Ok(file) => match case.repro {
                Repro::Sar(args) => render_sar_text(&file, &data, args),
                Repro::SadfHeader => render_sadf_header(&file),
                Repro::Unsupported { .. } => unreachable!("上で処理済み"),
            },
            Err(e) => Err(format!("ファイルを開けない: {e}")),
        };
        let actual = match actual {
            Ok(t) => t,
            Err(detail) => {
                eprintln!("  生成失敗  {head}\n            {detail}");
                failures.push(format!("{head}: {detail}"));
                continue;
            }
        };

        compared += 1;
        let cmp: Comparison = golden::compare(&expected, &actual, case.masks);
        // 差分が出たケースは出力全体をファイルへ落とす。
        // 行単位の要約だけでは追えない食い違い (行の増減・ブロック順) を
        // `diff -u` で追えるようにするため。
        if !cmp.is_match() {
            if let Some(path) = dump_actual(case, &actual) {
                eprintln!("            実際の出力: {}", path.display());
            }
        }
        let label = if cmp.is_match() {
            if cmp.masked.is_empty() {
                exact += 1;
                "全文一致  "
            } else {
                masked += 1;
                "マスク一致"
            }
        } else {
            "不一致    "
        };
        eprintln!("  {label}{head}\n            {}", cmp.verdict());
        if !cmp.masked.is_empty() {
            eprint!("{}", cmp.mask_report());
        }
        if !cmp.is_match() {
            eprint!("{}", cmp.diff_report(6));
            failures.push(format!("{head}: {}\n{}", cmp.verdict(), cmp.diff_report(6)));
        }
    }

    eprintln!(
        "\n--- 集計: 全文一致 {} / マスク一致 {} / 不一致・比較不能 {} (全 {} 件, 比較実行 {} 件) ---",
        exact,
        masked,
        failures.len(),
        GOLDEN_CASES.len(),
        compared
    );

    assert!(
        failures.is_empty(),
        "本家の期待出力と一致しないケースが {} 件ある:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

// ===========================================================================
// 本家データを本体 (SaFile / scan / series::walk) に読ませる
// ===========================================================================

/// 本家データ 1 件に対する reSARch の**あるべき挙動**。
///
/// 本家の挙動と意図的に違う場合がある。例えば `data-9.1.5` は本家が
/// 「現行版では読めない、変換しろ」と言う最古世代だが、reSARch は直読できる
/// (`docs/design.md` の目的そのもの)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    /// 開けて、レコード列を末尾まで余りなく読める。
    Readable,
    /// ヘッダは読めるが、統計を読もうとすると拒否する (`sadf -H` 相当は成功)。
    HeaderOnly(ExpectedError),
    /// 開く時点で拒否する。
    Rejected(ExpectedError),
}

/// 正常系データの判定表。
///
/// 異常系 16 本の判定は [`Corruption::expected_error`] から引くのでここには載せない
/// (自作 fixture と**同じ期待値**を使うことで、両者が同じ検査を突いていることも保証する)。
const NORMAL_VERDICTS: &[(&str, Verdict)] = &[
    // 最古世代 (0x2170)。本家は変換なしでは読めないが reSARch は読める。
    ("data-9.1.5", Verdict::Readable),
    ("data-9.1.6", Verdict::Readable),
    ("data-10.3.1", Verdict::Readable),
    ("data-11.6.5", Verdict::Readable),
    ("data-12.0.0", Verdict::Readable),
    // ビッグエンディアン + 32bit (ppc)
    ("data-ppc-11.7.2", Verdict::Readable),
    // extra_desc チェーンつき
    ("data-extra-12.1.7", Verdict::Readable),
    ("data-non-printable", Verdict::Readable),
    // 未知 id / 未知 magic は読み飛ばして残りを読む (§10.4)
    ("data-ukwn", Verdict::Readable),
    ("data-ukwn0", Verdict::Readable),
    ("data-ukwn1", Verdict::Readable),
    ("data-12.5.6-A_QUEUE_modified", Verdict::Readable),
    // sadf -x の期待出力。名前が data で始まるが sa ファイルではない。
    (
        "data-12.7.6.xml",
        Verdict::Rejected(ExpectedError::NotSysstatFile),
    ),
];

/// 本家ファイル名に対応する自作の壊し方。
fn corruption_for(name: &str) -> Option<Corruption> {
    Corruption::ALL
        .into_iter()
        .find(|c| c.upstream_name() == Some(name))
}

/// 壊し方から判定を組み立てる (自作 fixture と同じ表を使う)。
fn verdict_of(corruption: Corruption) -> Verdict {
    let expected = corruption.expected_error();
    if corruption.fails_header_only_mode() {
        Verdict::Rejected(expected)
    } else {
        Verdict::HeaderOnly(expected)
    }
}

/// 本体に読ませた結果を「段 + エラーバリアント名」で表す。
///
/// 自作 fixture と本家ファイルの突合に使う。メッセージ本文は比較しない
/// (本家は英語・reSARch は日本語で、一致させる必要がない)。
fn outcome_label(label: &str, bytes: Vec<u8>) -> String {
    match SaFile::from_bytes(label.to_string(), bytes) {
        Err(err) => format!("open:{}", fixtures::error_variant(&err)),
        Ok(file) => {
            if let Err(err) = file.scan(|_| Ok(ScanControl::Continue)) {
                return format!("scan:{}", fixtures::error_variant(&err));
            }
            match walk(&file, &Selection::All, |_| Ok(ScanControl::Continue)) {
                Err(err) => format!("walk:{}", fixtures::error_variant(&err)),
                Ok(summary) if summary.is_exact() => "ok".to_string(),
                Ok(_) => "ok(末尾に未読が残る)".to_string(),
            }
        }
    }
}

/// 1 件を判定どおりに扱えているか確かめ、食い違いを `problems` へ足す。
fn check_verdict(name: &str, bytes: Vec<u8>, verdict: Verdict, problems: &mut Vec<String>) {
    let note = |problems: &mut Vec<String>, stage: &str, expected: ExpectedError, err: &Error| {
        if !expected.matches(err) {
            problems.push(format!(
                "{name}: {stage} で {} を期待したが {err:?}",
                expected.describe()
            ));
        }
        if let Err(detail) = fixtures::error_invariants(err) {
            problems.push(format!("{name}: {stage}: {detail}"));
        }
    };

    match SaFile::from_bytes(name.to_string(), bytes) {
        Err(err) => match verdict {
            Verdict::Rejected(expected) => note(problems, "オープン", expected, &err),
            _ => problems.push(format!(
                "{name}: ヘッダは読めなければならないのに {err:?} で拒否された"
            )),
        },
        Ok(file) => {
            if let Verdict::Rejected(_) = verdict {
                problems.push(format!("{name}: 拒否されるべきファイルが開けた"));
                return;
            }
            let scanned = file.scan(|_| Ok(ScanControl::Continue));
            let walked = walk(&file, &Selection::All, |_| Ok(ScanControl::Continue));
            match verdict {
                Verdict::Readable => {
                    match &scanned {
                        Ok(s) if s.is_exact() => {}
                        Ok(s) => problems.push(format!(
                            "{name}: 走査が末尾に到達しない (end={} size={})",
                            s.end_offset, s.file_size
                        )),
                        Err(e) => problems.push(format!("{name}: 走査が失敗した: {e:?}")),
                    }
                    match &walked {
                        Ok(s) if s.is_exact() => {}
                        Ok(s) => problems.push(format!(
                            "{name}: 統計走査が末尾に到達しない (end={} size={})",
                            s.end_offset, s.file_size
                        )),
                        Err(e) => problems.push(format!("{name}: 統計デコードが失敗した: {e:?}")),
                    }
                    if let Ok(s) = &scanned
                        && s.total_records() == 0
                    {
                        problems.push(format!("{name}: レコードが 1 件も読めていない"));
                    }
                }
                Verdict::HeaderOnly(expected) => match (scanned, walked) {
                    (Err(err), _) => note(problems, "走査", expected, &err),
                    (Ok(_), Err(err)) => note(problems, "統計デコード", expected, &err),
                    (Ok(_), Ok(_)) => {
                        problems.push(format!("{name}: 統計読みで拒否されるべきファイルが通った"))
                    }
                },
                Verdict::Rejected(_) => unreachable!("上で処理済み"),
            }
        }
    }
}

/// 本家データを 1 件ずつ本体に読ませ、判定表どおりに読める / 拒否されること。
///
/// 異常系の期待値は自作 fixture と同じ [`Corruption::expected_error`] から引く。
/// 本家のメッセージ (英語) と一致させる必要はなく、**分類が一致していればよい** (§10.3)。
///
/// 取得物に判定表の無いファイルがあれば失敗させる。新しい本家データが増えたときに
/// 「検証されないまま増える」ことを防ぐため。
#[test]
#[ignore = "本家データ (GPL) が必要。make fixtures 後 --include-ignored で実行する"]
fn upstream_files_are_read_or_rejected_as_expected() {
    let Some(dir) = upstream_or_skip("upstream_files_are_read_or_rejected_as_expected") else {
        return;
    };

    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .expect("取得物のディレクトリが読めない")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        // 入力データだけを見る (`expected*` は golden 出力)
        .filter(|n| n.starts_with("data"))
        .collect();
    names.sort();
    assert!(!names.is_empty(), "入力データが 1 件も無い");

    let mut problems: Vec<String> = Vec::new();
    for name in &names {
        let verdict = match corruption_for(name) {
            Some(c) => verdict_of(c),
            None => match NORMAL_VERDICTS.iter().find(|(n, _)| n == name) {
                Some((_, v)) => *v,
                None => {
                    problems.push(format!("{name}: 判定表に無い (期待する挙動が未記載)"));
                    continue;
                }
            },
        };
        let bytes = std::fs::read(dir.join(name)).expect("本家データが読めない");
        check_verdict(name, bytes, verdict, &mut problems);
    }

    assert!(
        problems.is_empty(),
        "本家データの扱いが期待と違う ({} 件 / 全 {} 件):\n{}",
        problems.len(),
        names.len(),
        problems.join("\n")
    );
}

/// 自作の異常系 fixture と本家の異常系ファイルが、**同じ段・同じエラー分類**で拒否されること。
///
/// 自作 fixture は本家データを同梱できないために用意したもので、
/// 「同じ検査を突いている」ことがここで初めて実証される。
/// どちらかが通ってしまう組み合わせがあれば、fixture の作り方か本体の検査が偏っている。
#[test]
#[ignore = "本家データ (GPL) が必要。make fixtures 後 --include-ignored で実行する"]
fn self_made_and_upstream_error_files_are_rejected_alike() {
    let Some(dir) = upstream_or_skip("self_made_and_upstream_error_files_are_rejected_alike")
    else {
        return;
    };

    // 本家データは x86_64 (64bit LE) で作られている (§2.1 の実測表)
    let abi = FixtureAbi::Le64;
    let mut compared = 0usize;
    for corruption in Corruption::ALL {
        let Some(name) = corruption.upstream_name() else {
            continue;
        };
        let Some(path) = upstream_file(&dir, name) else {
            continue;
        };
        let upstream = outcome_label(name, std::fs::read(&path).expect("本家データが読めない"));
        let mine = outcome_label(
            &format!("{corruption:?}"),
            fixtures::corrupted(abi, corruption).bytes,
        );
        assert_eq!(
            mine, upstream,
            "{corruption:?}: 自作 fixture と本家 {name} で拒否の段・分類が違う"
        );
        compared += 1;
    }
    assert!(compared >= 14, "突合できたのが {compared} 件しかない");
    eprintln!("自作 fixture と本家データを {compared} 件突合した");
}

/// 448 バイトの `-err` 系が本家と同じ構成であること (自作 fixture の前提の裏取り)。
///
/// 自作の基準ファイルが 448 バイト・activity 1 件・レコード 0 件であることは
/// `layout_conformance` 側で検証している。ここでは本家側がその形を保っていることを見る。
#[test]
#[ignore = "本家データ (GPL) が必要。make fixtures 後 --include-ignored で実行する"]
fn upstream_err_files_have_the_expected_shape() {
    let Some(dir) = upstream_or_skip("upstream_err_files_have_the_expected_shape") else {
        return;
    };

    for name in HEADER_ERROR_DATA {
        let Some(path) = upstream_file(&dir, name) else {
            continue;
        };
        // 本家は 1 フィールドだけを壊した 448 バイトのファイルとして作っている (§2.2)
        let len = std::fs::metadata(&path)
            .expect("メタデータが読めない")
            .len();
        assert_eq!(len, 448, "{name}: 448 バイトのはず");
    }

    // ERROR_CASES 側は「ヘッダ表示モードでも失敗するか」の対応を確認する。
    // reSARch が本家と意図的に違うのは data-9.1.5 (旧世代を直読できる) だけ。
    for case in ERROR_CASES {
        if upstream_file(&dir, case.data).is_none() {
            continue;
        }
        if case.data == "data-9.1.5" {
            // reSARch は 0x2170 を直読できるので、本家の
            // 「cannot read the format of this file」には対応しない
            eprintln!(
                "[{}] {}: 本家は {:?} で拒否するが reSARch は読める",
                case.upstream_test, case.data, case.upstream_message
            );
            continue;
        }
        let corruption = corruption_for(case.data)
            .unwrap_or_else(|| panic!("{}: 対応する自作の壊し方が無い", case.data));
        assert_eq!(
            corruption.fails_header_only_mode(),
            case.fails_header_only,
            "{}: ヘッダ表示モードの免除が本家テスト {} と食い違う",
            case.data,
            case.upstream_test
        );
        // 本家メッセージ (英語) と reSARch の分類の対応を記録に残す
        eprintln!(
            "[{}] {}: 本家 {:?} → reSARch は {} で拒否する",
            case.upstream_test,
            case.data,
            case.upstream_message,
            corruption.expected_error().describe()
        );
    }
}

/// 実 `sar` / `sadf` との突合 (案 C) の枠。
///
/// TODO(後続担当): 自作 fixture を入力に、同じ引数を `sar` と `resarch` に渡して
/// 出力を比較する (§7.3)。`sar` が無い環境ではスキップする。
#[test]
#[ignore = "sysstat のインストールが必要。CI の conformance ジョブで実行する"]
fn live_sysstat_is_available_and_recorded() {
    match sysstat_version() {
        Some(v) => {
            // どの版と比較したのかを必ず記録する (§7.4)。
            eprintln!("sysstat: {v}");
        }
        None => {
            eprintln!(
                "skipped: sar が見つからない。apt-get install sysstat で導入する \
                 (CI では conformance ジョブが導入する)。"
            );
        }
    }
}

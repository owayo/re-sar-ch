//! 本家 sysstat との突合テスト (骨格)。
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
//! # 現状
//!
//! `resarch` の出力系がまだ無いため、ここにあるのは
//! **データ発見・スキップ判定・比較の枠**までである。
//! 実際の出力比較 (`expected*` との `diff` 相当) は後続担当が
//! [`GOLDEN_CASES`] の各ケースに `run_resarch` を差し込んで埋める。
//! 埋めるべき内容は `docs/format/04-test-data.md` §5 のフェーズ別計画に対応する。

use std::path::{Path, PathBuf};
use std::process::Command;

use re_sar_ch::format::abi::{Endian, LayoutAbi, SourceEncoding};
use re_sar_ch::format::reader::Cursor;
use re_sar_ch::format::wire::ResolvedLayout;
use re_sar_ch::format::{layouts, selfdesc};

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
}

/// `docs/format/04-test-data.md` §5.1 のフェーズ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// ヘッダ解析のみ (`sadf -H` 相当)。
    Header,
    /// 生値デコード (`sadf -r -O debug` 相当)。
    RawValues,
    /// `sar` テキスト出力。
    SarText,
}

/// 同梱バイナリ由来の golden (全件)。
///
/// コマンドラインは本家テストのものをそのまま記録してある。
/// 環境変数の固定 (`LC_ALL=C` / `TZ=GMT`) は再現性のために必須 (§7.3)。
const GOLDEN_CASES: &[GoldenCase] = &[
    GoldenCase {
        upstream_test: "00655",
        data: "data-12.0.0",
        upstream_cmd: "LC_ALL=C TZ=GMT sadf -H <data> | grep -v 0x2175",
        golden: "expected.data-12.0.0-H",
        phase: Phase::Header,
    },
    GoldenCase {
        upstream_test: "00787",
        data: "data-ukwn",
        upstream_cmd: "LC_ALL=C sadf -H <data> | grep -v 0x2175",
        golden: "expected.sadf-data-ukwn",
        phase: Phase::Header,
    },
    GoldenCase {
        upstream_test: "00791",
        data: "data-ukwn0",
        upstream_cmd: "LC_ALL=C sadf -H <data> | grep -v 0x2175",
        golden: "expected.sadf-data-ukwn0",
        phase: Phase::Header,
    },
    GoldenCase {
        upstream_test: "00794",
        data: "data-ukwn1",
        upstream_cmd: "LC_ALL=C sadf -H <data> | grep -v 0x2175",
        golden: "expected.sadf-data-ukwn1",
        phase: Phase::Header,
    },
    GoldenCase {
        upstream_test: "00650",
        data: "data-12.0.0",
        upstream_cmd: "LC_ALL=C TZ=GMT sar -AC -f <data>",
        golden: "expected.data-12.0.0",
        phase: Phase::SarText,
    },
    GoldenCase {
        upstream_test: "00700",
        data: "data-ppc-11.7.2",
        upstream_cmd: "LC_ALL=C TZ=GMT sar -C -A -f <data>",
        golden: "expected.data-ppc-11.7.2",
        phase: Phase::SarText,
    },
    GoldenCase {
        upstream_test: "00740",
        data: "data-non-printable",
        upstream_cmd: "LC_ALL=C TZ=GMT sar -C -f <data>",
        golden: "expected.sar-non-printable",
        phase: Phase::SarText,
    },
    GoldenCase {
        upstream_test: "00760",
        data: "data-12.5.6-A_QUEUE_modified",
        upstream_cmd: "LC_ALL=C TZ=GMT sar -A -f <data>",
        golden: "expected.data-12.5.6-A_QUEUE_modified",
        phase: Phase::SarText,
    },
    GoldenCase {
        upstream_test: "00770",
        data: "data-extra-12.1.7",
        upstream_cmd: "LC_ALL=C TZ=GMT sar -A -f <data>",
        golden: "expected.data-extra-12.1.7",
        phase: Phase::SarText,
    },
    GoldenCase {
        upstream_test: "00780",
        data: "data-ukwn",
        upstream_cmd: "LC_ALL=C TZ=GMT sar -P ALL -f <data>",
        golden: "expected.sar-data-ukwn",
        phase: Phase::SarText,
    },
    GoldenCase {
        upstream_test: "00784",
        data: "data-ukwn",
        upstream_cmd: "LC_ALL=C TZ=GMT sar -w -f <data>",
        golden: "expected2.sar-data-ukwn",
        phase: Phase::SarText,
    },
    GoldenCase {
        upstream_test: "00793",
        data: "data-ukwn1",
        upstream_cmd: "LC_ALL=C TZ=GMT sar -uq -f <data>",
        golden: "expected3.sar-data-ukwn",
        phase: Phase::SarText,
    },
    GoldenCase {
        upstream_test: "01405",
        data: "data-trunc",
        upstream_cmd: "LC_ALL=C TZ=GMT sadf -g <data> -- -A",
        golden: "expected.sadf-g-trunc",
        phase: Phase::RawValues,
    },
    // 旧世代 (0x2171 / 0x2173) の golden は「sadf -c で変換したファイル」に対するもの。
    // reSARch は直読するので、§5.7 の自己整合性検証と組み合わせて使う。
    GoldenCase {
        upstream_test: "00605",
        data: "data-9.1.6",
        upstream_cmd: "LC_ALL=C TZ=GMT sadf -c <data> > tmp && sar -C -A -f tmp",
        golden: "expected.data-9.1.6",
        phase: Phase::SarText,
    },
    GoldenCase {
        upstream_test: "00615",
        data: "data-10.3.1",
        upstream_cmd: "LC_ALL=C TZ=GMT sadf -c <data> > tmp && sar -C -A -f tmp",
        golden: "expected.data-10.3.1",
        phase: Phase::SarText,
    },
    GoldenCase {
        upstream_test: "00625",
        data: "data-11.6.5",
        upstream_cmd: "LC_ALL=C TZ=GMT sadf -c <data> > tmp && sar -C -A -f tmp",
        golden: "expected.data-11.6.5",
        phase: Phase::SarText,
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
// resarch の起動
// ===========================================================================

/// ビルド済み `resarch` のパス。cargo がテスト時に渡してくる。
fn resarch_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_resarch"))
}

/// 表記比較のために環境を固定して `resarch` を起動する (§7.3 の表)。
///
/// 後続担当はこの関数に引数を渡して出力を取り、`expected*` と比較する。
#[allow(dead_code)]
fn run_resarch(args: &[&str]) -> std::process::Output {
    Command::new(resarch_bin())
        .args(args)
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .env("S_COLORS", "never")
        .env_remove("S_TIME_FORMAT")
        .env_remove("S_REPEAT_HEADER")
        .env_remove("S_COLORS_SGR")
        .env_remove("S_COLORS_PALETTE")
        .output()
        .expect("resarch を起動できない")
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

/// golden 比較の枠。
///
/// TODO(後続担当): `resarch` の出力系が入ったら、`phase` ごとに
/// 対応するサブコマンドを `run_resarch` で叩き、`golden` と比較する。
/// - [`Phase::Header`] → `resarch info` 相当 (`sadf -H` の 13 項目書式。§5.2)
/// - [`Phase::RawValues`] → `sadf -r -O debug` 相当 (§5.3)
/// - [`Phase::SarText`] → `sar` 互換テキスト (§5.4)
///
/// 比較は 2 段階で行う (`docs/design.md` §8):
/// 意味比較 (JSON/XML を解析して値・単位・item 対応) → 表記比較 (列順・丸め・空白まで)。
#[test]
#[ignore = "本家データ (GPL) が必要。make fixtures 後 --include-ignored で実行する"]
fn golden_cases_are_enumerable() {
    let Some(dir) = upstream_or_skip("golden_cases_are_enumerable") else {
        return;
    };

    let mut ready = 0usize;
    for case in GOLDEN_CASES {
        let (Some(data), Some(golden)) = (
            upstream_file(&dir, case.data),
            upstream_file(&dir, case.golden),
        ) else {
            continue;
        };
        let golden_text = std::fs::read(&golden).expect("期待出力が読めない");
        assert!(!golden_text.is_empty(), "{}: 期待出力が空", case.golden);
        assert!(data.is_file());
        ready += 1;

        eprintln!(
            "pending [{}] {:?} {} -> {} ({})",
            case.upstream_test, case.phase, case.data, case.golden, case.upstream_cmd
        );
    }

    assert_eq!(
        ready,
        GOLDEN_CASES.len(),
        "golden 比較ケースの入力が揃っていない"
    );
    eprintln!(
        "TODO: {} 件の出力比較は resarch の出力系が入ってから埋める",
        GOLDEN_CASES.len()
    );
}

/// 異常系の枠。
///
/// TODO(後続担当): `resarch` が各ファイルを**拒否する**ことと、
/// ヘッダ表示モードの免除 (`-SARerr` / `A_IRQ_overflow` はヘッダ表示は成功) を検証する。
/// 本家のメッセージ (英語) と一致させる必要はない。分類が一致していればよい。
#[test]
#[ignore = "本家データ (GPL) が必要。make fixtures 後 --include-ignored で実行する"]
fn error_cases_are_enumerable() {
    let Some(dir) = upstream_or_skip("error_cases_are_enumerable") else {
        return;
    };

    for name in HEADER_ERROR_DATA {
        let Some(path) = upstream_file(&dir, name) else {
            continue;
        };
        // 本家は 1 フィールドだけを壊した 448 バイトのファイルとして作っている (§2.2)。
        // 自作 fixture (tests/fixtures) が同じ構成であることは layout_conformance 側で検証済み。
        let len = std::fs::metadata(&path)
            .expect("メタデータが読めない")
            .len();
        assert_eq!(len, 448, "{name}: 448 バイトのはず");
        eprintln!("pending [00730] 拒否されるべき: {name}");
    }

    for case in ERROR_CASES {
        if upstream_file(&dir, case.data).is_none() {
            continue;
        }
        eprintln!(
            "pending [{}] {} -> 本家メッセージ {:?} (ヘッダ表示も失敗: {})",
            case.upstream_test, case.data, case.upstream_message, case.fails_header_only
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

//! 開発用タスクランナー。
//!
//! # なぜ必要か
//!
//! reSARch は MIT だが、本家 sysstat は GPL-2.0-or-later であり、その `tests/` 配下にも
//! 別建ての許諾表示が無い。したがって**本家由来のバイナリ `sa` ファイルや期待出力を
//! 本リポジトリへ 1 バイトも同梱しない**。
//! 代わりに、検証が必要なときだけ固定タグから取得して `target/` (gitignore 済み) へ置く。
//! 方針の詳細は `docs/format/04-test-data.md` の「6. ライセンス」「7. CI の具体的な実現方法」。
//!
//! # 使い方
//!
//! ```sh
//! cargo run --bin xtask -- fetch-fixtures          # 取得 (既に揃っていれば何もしない)
//! cargo run --bin xtask -- fetch-fixtures --force  # 取得し直す
//! cargo run --bin xtask -- list-fixtures           # 取得対象と用途の一覧
//! ```
//!
//! # 取得の安全性
//!
//! - タグ (`UPSTREAM_TAG`) と**そのタグが指すコミット SHA** (`UPSTREAM_COMMIT`) を
//!   ソース内に固定する。タグは付け替えられるのでコミットまで照合する。
//! - ファイルごとに SHA-256 とバイト数を照合する (期待値もソース内に固定)。
//! - 新しい依存は足さない。HTTP 取得は `git clone --depth 1 --branch <tag>` に任せる。
//! - ネットワークが無い環境では、何をすればよいか分かるメッセージで失敗する。

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};

// ===========================================================================
// 取得元の固定
// ===========================================================================

/// 取得元リポジトリ。
const UPSTREAM_URL: &str = "https://github.com/sysstat/sysstat.git";

/// 固定タグ。`docs/format/04-test-data.md` の調査時点の最新リリースタグ。
const UPSTREAM_TAG: &str = "v12.8.0";

/// `UPSTREAM_TAG` が指すコミット (タグオブジェクトではなくコミットの SHA-1)。
///
/// タグは後から付け替えられるため、clone 後に `git rev-parse HEAD` と照合する。
const UPSTREAM_COMMIT: &str = "ffabe8175aa9a7aa056380c66ed3648e50c1967d";

/// 本家リポジトリ内でテストデータが置かれているディレクトリ。
const UPSTREAM_TESTS_DIR: &str = "tests";

/// 取得物の保存先 (target 配下なのでリポジトリには入らない)。
const DEST_SUBDIR: &str = "fixtures/upstream";

/// 取得物の素性を書き残すファイル名。
const PROVENANCE_FILE: &str = "PROVENANCE.txt";

// ===========================================================================
// 取得対象の一覧 (docs/format/04-test-data.md の対応表に対応)
// ===========================================================================

/// 取得対象 1 件。
struct Fixture {
    /// 本家 `tests/` 配下のファイル名。
    name: &'static str,
    /// SHA-256 (16 進小文字)。
    sha256: &'static str,
    /// バイト数。
    size: u64,
    /// 何の検証に使うか (`docs/format/04-test-data.md` の対応表より)。
    purpose: &'static str,
}

const fn f(name: &'static str, sha256: &'static str, size: u64, purpose: &'static str) -> Fixture {
    Fixture {
        name,
        sha256,
        size,
        purpose,
    }
}

/// バイナリ `sa` データ。`04-test-data.md` §2 の素性表に対応。
const DATA_FIXTURES: &[Fixture] = &[
    f(
        "data-9.1.5",
        "f1fe619cbaa9e531d27b717ea0a10cba294027c553fcfe763fe3255cdf73b808",
        6028,
        "0x2170 の拒否 (変換案内を出さないこと)",
    ),
    f(
        "data-9.1.6",
        "8886190f0ce40c4a165ae6c0293067b88c28cfb7be1bc4f9098cdf9ea6a98452",
        20784,
        "0x2171 世代の直読",
    ),
    f(
        "data-10.3.1",
        "bf720dad98929850745a2c60953b446d2675d14190cc7af959f2db1c52b1f826",
        35108,
        "0x2173 世代の直読 + RESTART 後の volatile activity リスト",
    ),
    f(
        "data-11.6.5",
        "124f5a5f09b3db51232e5fc3f5983765730723c894efc4828fc1ba3c1509163f",
        38884,
        "0x2173 世代の最終形 + センサ系 activity",
    ),
    f(
        "data-ppc-11.7.2",
        "21286a4fbc9ad43a409aa80a6a66a3a35268010d86d0ccad6834f9744c21d223",
        8888,
        "big endian + sizeof(long)=4 + 変換済みファイル (最重要の 1 本)",
    ),
    f(
        "data-12.0.0",
        "51dbd507a70725b00e835dc3525a1148511ae025c87ba8ea82332f1336cc0204",
        22376,
        "自己記述形式の初期形 (hdr_types_nr=(1,1,11), header_size=328)",
    ),
    f(
        "data-trunc",
        "ab98c63162e8a95921c8b8d3d68ec01f97f21f8d6804ad97151cd728d118df78",
        1000,
        "レコード途中で EOF (strict / lenient の切り分け)",
    ),
    f(
        "data-non-printable",
        "006eea423bed301362490d5308566084fe74b5e5f1dac7415ea0f0d5cc6d1588",
        536,
        "COMMENT 中の非印字文字のサニタイズ",
    ),
    f(
        "data-ukwn",
        "d062b159c93272f893c8b1582095034b1b7cb2457af9be78b1d7e3a84d2ffd26",
        1136,
        "未知 activity ID と既知 ID / 未知 magic の同時存在",
    ),
    f(
        "data-ukwn0",
        "1786a132f37a04f3bdf6bc662375d3768454d4eb8fc979aaf312bc88250f6268",
        612,
        "表示可能な activity が 1 つも無いケース",
    ),
    f(
        "data-ukwn1",
        "781cc278760932065f4d7336a34807c0980eebb3feaaf4c74945ec5eab4a19f0",
        2872,
        "未知 magic を挟んだときの後続 activity のオフセット維持",
    ),
    f(
        "data-12.5.6-A_QUEUE_modified",
        "e8aab13bdbe61f93c37bb590387ae86b4897cdfb9ce2c62fb632b331eb490181",
        1472,
        "remap 不能な構造体を UNKNOWN 扱いにし、他 activity は読めること",
    ),
    f(
        "data-12.7.1-A_IRQ_overflow",
        "2b5476590cb7e519dc721f253eaa384631d3b340ebde58553434da700b452cc1",
        448,
        "nr × nr2 × size の u32 乗算オーバーフロー",
    ),
    f(
        "data-extra-12.1.7",
        "cfae21f6da41c997e6f1421af702e5a93c377d05b96c3be56b66ed1b3578c370",
        1448,
        "extra_desc 連鎖と R_EXTRA レコードの読み飛ばし",
    ),
    f(
        "data-12.7.6.xml",
        "6262eba265b744046db1b33fa3ee1f65aaa2311fde52f5ca5c2d139809e40298",
        74948,
        "sadf -x 出力の XSD / DTD 妥当性検証用の参照 XML",
    ),
];

/// ヘッダ整合性 14 条件を突く異常系データ (`04-test-data.md` §2.2)。
///
/// いずれも 448 バイトの正常ファイルから 1 フィールドだけを書き換えたもの。
/// 同じ壊し方は `tests/fixtures/mod.rs` の自作 fixture でも再現している。
const ERR_FIXTURES: &[Fixture] = &[
    f(
        "data-12.6.0-file_hdr-sa_act_nr-err",
        "dc792d56b973bac8f2f440e2adf2554a1de4aed29045fb81b17e96e68beb5395",
        448,
        "sa_act_nr = 257 > MAX_NR_ACT (256)",
    ),
    f(
        "data-12.6.0-file_hdr-MAP_SIZE_act_types_nr-err",
        "80d978b223dcf8f4183cdbd34408bf4351eb1d9ad36b0e47cbff7147b2c5461c",
        448,
        "MAP_SIZE(act_types_nr) = 56 > act_size = 36",
    ),
    f(
        "data-12.6.0-file_hdr-MAP_SIZE_rec_types_nr-err",
        "e97bbd286569de4a6f884441941fdc86ee06c7ed4626a242051d87ac472a9788",
        448,
        "MAP_SIZE(rec_types_nr) = 40 > rec_size = 24",
    ),
    f(
        "data-12.6.0-file_hdr-act_size-err",
        "092612e2342be4e114f681c581ce5f6912d02ef8fd3db30bea4a88d021f4f2a0",
        448,
        "act_size = 1025 > MAX_FILE_ACTIVITY_SIZE (1024)",
    ),
    f(
        "data-12.6.0-file_hdr-rec_size-err",
        "2cbac8dd533137251470868b568212b1688ea304c79184ea55d119b40ec6cd55",
        448,
        "rec_size = 513 > MAX_RECORD_HEADER_SIZE (512)",
    ),
    f(
        "data-12.6.0-file_act-nr-0-err",
        "0745697d77cd485b9461518537e2c96f3806011d89e8dfb607705123b7afa2a5",
        448,
        "file_activity.nr = 0 (nr < 1)",
    ),
    f(
        "data-12.6.0-file_act-nr-err",
        "0dc10c773b14219001446b71b43b146bdc42b1049b3af1b82e1fda2372032869",
        448,
        "file_activity.nr = 268435457 > NR_MAX",
    ),
    f(
        "data-12.6.0-file_act-nr-nr_max-err",
        "9fe418bb14d9054fdcd371d79df64c494e3258ec4b282bc4d30829280caf063f",
        448,
        "A_PCSW の nr = 2 > activity 個別の nr_max (1)",
    ),
    f(
        "data-12.6.0-file_act-nr2-0-err",
        "2272243a7aa054449b778aedc95c67dc98d764dcadc1fef64239c10b96ef033b",
        448,
        "file_activity.nr2 = 0 (nr2 < 1)",
    ),
    f(
        "data-12.6.0-file_act-nr2-err",
        "3387b6413e29915e11b1f350c4b0f30ca2bf36237b518cef6398b2d0ae0e73e0",
        448,
        "file_activity.nr2 = 4097 > NR2_MAX",
    ),
    f(
        "data-12.6.0-file_act-size-0-err",
        "e7f1b910d24632d55764383975425b2c476705c8b10cafd5879d1f014213518f",
        448,
        "file_activity.size = 0",
    ),
    f(
        "data-12.6.0-file_act-size-err",
        "99ad33357781622e168fd896eb6066e11c2184171c3246eaaec6d32e8f728b39",
        448,
        "file_activity.size = 1025 > MAX_ITEM_STRUCT_SIZE (1024)",
    ),
    f(
        "data-12.6.0-file_act-MAP_SIZE_types_nr-err",
        "ef8423e9489eaeb33a829e236a5161184338032e3976308e034d24c4e0d6e648",
        448,
        "MAP_SIZE(types_nr) = 32 > size = 16",
    ),
    f(
        "data-12.6.0-file_act-types_nr-SARerr",
        "4311cc24bbe64ea6e9915a3d68ddc6fc50848cd552b1671dd1cb085a3d3c7bde",
        448,
        "types_nr の単調性違反 (ヘッダ表示は成功し、統計読みだけ失敗する境界)",
    ),
];

/// 同梱バイナリ由来の期待出力 (`04-test-data.md` §4.2 / §4.3)。
const EXPECTED_FIXTURES: &[Fixture] = &[
    f(
        "expected.data-12.0.0",
        "d27073d66411bfb8032a7547e94c64e54687654a8587fcc1f5ee08970ccb114f",
        21176,
        "テスト 00650: sar -AC -f data-12.0.0",
    ),
    f(
        "expected.data-12.0.0-H",
        "fbd4d690f6c9dd1b709b12681e49f48fadb9e0f551e1ba0b8454c593d4ab9783",
        2032,
        "テスト 00655: sadf -H data-12.0.0 (Phase 1 の golden)",
    ),
    f(
        "expected.data-ppc-11.7.2",
        "8d86d57dc8bcd79840f9cf1dec6db42f61299b125296f537be30c7b080a713ab",
        10990,
        "テスト 00700: sar -C -A -f data-ppc-11.7.2 (BE + 32bit)",
    ),
    f(
        "expected.sar-non-printable",
        "f5e39304883dc0c5d0914709af6e67c85a95fc8455403fea763df60b71fdda92",
        100,
        "テスト 00740: COMMENT のサニタイズ結果",
    ),
    f(
        "expected.data-12.5.6-A_QUEUE_modified",
        "645924ddf3f42eb20d9dc24e1316d4639b1a0f5f196ba52c91b0bb384dbd713c",
        1295,
        "テスト 00760: remap 不能構造体の扱い",
    ),
    f(
        "expected.data-extra-12.1.7",
        "a55e0f8feafb39936049f8137a6cff91896c04620e38bac6399dbe2621fc0906",
        1055,
        "テスト 00770: extra 構造体つきファイルの sar 出力",
    ),
    f(
        "expected.sar-data-ukwn",
        "fe6ce42dd10b549a9b9cb0985bd93217c43e1dcd328a94ff73a3336a89c51bc0",
        642,
        "テスト 00780: sar -P ALL -f data-ukwn",
    ),
    f(
        "expected2.sar-data-ukwn",
        "dcbf321f70e3bf2dffb96267b875cc8d582c7f56050ff81530f6523ff53c8237",
        81,
        "テスト 00784 / 00790: sar -w data-ukwn と sar -A data-ukwn0",
    ),
    f(
        "expected3.sar-data-ukwn",
        "cde8842bd6caee6a577072bb51ed2fad365c0da1877c97b54f213848a4bf370d",
        403,
        "テスト 00793: sar -uq -f data-ukwn1",
    ),
    f(
        "expected.sadf-data-ukwn",
        "041362a33b69147b1c49ce38942ce0fd91639908410168383d4dc78bae14a670",
        550,
        "テスト 00787: sadf -H data-ukwn ([Unknown format] 表示)",
    ),
    f(
        "expected.sadf-data-ukwn0",
        "83a69e43818df8b350e7a785f2ef54967e93b19450061e987f6fc1061866e6b6",
        504,
        "テスト 00791: sadf -H data-ukwn0",
    ),
    f(
        "expected.sadf-data-ukwn1",
        "4779187105dc8616a3670f55682e477454b631c007d557509ed9b5a1f3624432",
        508,
        "テスト 00794: sadf -H data-ukwn1",
    ),
    f(
        "expected.sadf-g-trunc",
        "a136b465d893e064eb1c1164626edb77286dd2d761ab832a8e9ec98169b61a60",
        490,
        "テスト 01405: 切り詰めファイルを lenient に読んだ結果",
    ),
    f(
        "expected.data-9.1.6",
        "27f0213ed66fe01eed9475188ac5c0b687d8c83c8a1f1a95eacf45710342f3fb",
        32071,
        "テスト 00605: 0x2171 を変換して sar で読んだ結果 (直読との自己整合性検証に使う)",
    ),
    f(
        "expected.data-9.1.6-hz",
        "9807c9bf38df0787f0f2cd1a9c43b8c5cb604681988d1a46a3541eeaa949858e",
        348,
        "テスト 00608: 変換時に HZ を上書きした場合",
    ),
    f(
        "expected.data-10.3.1",
        "a9891ca6d7dec38625a7e388c2acded7be6446d81b1bc1912b01cf98ea75f2e1",
        48939,
        "テスト 00615: 0x2173 (初期形) の golden",
    ),
    f(
        "expected.data-11.6.5",
        "fde506bbfaf4679375aaabeb62dfcc328e1539264fc0bb858b8510bd83b9509b",
        52838,
        "テスト 00625: 0x2173 (最終形) の golden",
    ),
];

/// 取得対象の全件。
fn all_fixtures() -> Vec<&'static Fixture> {
    DATA_FIXTURES
        .iter()
        .chain(ERR_FIXTURES)
        .chain(EXPECTED_FIXTURES)
        .collect()
}

// ===========================================================================
// エントリポイント
// ===========================================================================

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(e) = run(&args) {
        eprintln!("xtask: エラー: {e:#}");
        std::process::exit(1);
    }
}

fn run(args: &[String]) -> Result<()> {
    let mut task: Option<&str> = None;
    let mut force = false;

    for a in args {
        match a.as_str() {
            "-h" | "--help" | "help" => {
                print_help();
                return Ok(());
            }
            "--force" => force = true,
            other if other.starts_with('-') => bail!("未知のオプション: {other}"),
            other if task.is_none() => task = Some(other),
            other => bail!("引数が多すぎます: {other}"),
        }
    }

    match task {
        Some("fetch-fixtures") => fetch_fixtures(force),
        Some("list-fixtures") => {
            list_fixtures();
            Ok(())
        }
        Some(other) => bail!("未知のタスク: {other} (--help で一覧)"),
        None => {
            print_help();
            Ok(())
        }
    }
}

fn print_help() {
    println!(
        "\
reSARch 開発用タスクランナー

使い方:
  cargo run --bin xtask -- <タスク> [オプション]

タスク:
  fetch-fixtures    本家 sysstat ({tag}) のテストデータを target/{dest} へ取得する
  list-fixtures     取得対象と用途の一覧を表示する

オプション:
  --force           既に取得済みでも取得し直す
  -h, --help        このヘルプを表示する

本家データは GPL-2.0-or-later のためリポジトリには同梱していない。
詳細は docs/format/04-test-data.md を参照。",
        tag = UPSTREAM_TAG,
        dest = DEST_SUBDIR,
    );
}

fn list_fixtures() {
    println!("取得元: {UPSTREAM_URL} ({UPSTREAM_TAG} = {UPSTREAM_COMMIT})");
    for (label, group) in [
        ("正常系・特殊系データ", DATA_FIXTURES),
        ("異常系データ", ERR_FIXTURES),
        ("期待出力", EXPECTED_FIXTURES),
    ] {
        println!("\n[{}] {} 件", label, group.len());
        for fx in group {
            println!("  {:<46} {:>7} B  {}", fx.name, fx.size, fx.purpose);
        }
    }
}

// ===========================================================================
// fetch-fixtures
// ===========================================================================

fn fetch_fixtures(force: bool) -> Result<()> {
    let dest = fixtures_dir()?;
    let fixtures = all_fixtures();

    if !force {
        match verify_all(&dest, &fixtures) {
            Ok(()) => {
                println!(
                    "本家 fixture は既に揃っている ({} 件, {})",
                    fixtures.len(),
                    dest.display()
                );
                return Ok(());
            }
            Err(reason) => {
                println!("取得が必要: {reason}");
            }
        }
    }

    ensure_git()?;

    let work = dest
        .parent()
        .ok_or_else(|| anyhow!("保存先の親ディレクトリが決まらない: {}", dest.display()))?
        .join(".upstream-clone");
    if work.exists() {
        std::fs::remove_dir_all(&work)
            .with_context(|| format!("作業ディレクトリを消せない: {}", work.display()))?;
    }
    std::fs::create_dir_all(&work)
        .with_context(|| format!("作業ディレクトリを作れない: {}", work.display()))?;

    println!(
        "clone 中: {UPSTREAM_URL} ({UPSTREAM_TAG}) → {}",
        work.display()
    );
    clone_upstream(&work)?;

    let head = git_head(&work)?;
    if head != UPSTREAM_COMMIT {
        bail!(
            "タグ {UPSTREAM_TAG} が指すコミットが変わっている (期待 {UPSTREAM_COMMIT}, 実際 {head})。\n\
             タグが付け替えられた可能性がある。中身を確認して xtask の UPSTREAM_COMMIT を更新すること。"
        );
    }

    std::fs::create_dir_all(&dest)
        .with_context(|| format!("保存先を作れない: {}", dest.display()))?;

    let src_dir = work.join(UPSTREAM_TESTS_DIR);
    let mut copied = 0usize;
    for fx in &fixtures {
        let src = src_dir.join(fx.name);
        let bytes = std::fs::read(&src)
            .with_context(|| format!("本家データが読めない: {}", src.display()))?;
        check_bytes(fx, &bytes)?;
        let dst = dest.join(fx.name);
        std::fs::write(&dst, &bytes).with_context(|| format!("保存できない: {}", dst.display()))?;
        copied += 1;
    }

    write_provenance(&dest, &fixtures)?;

    std::fs::remove_dir_all(&work)
        .with_context(|| format!("作業ディレクトリを消せない: {}", work.display()))?;

    verify_all(&dest, &fixtures).map_err(|e| anyhow!("取得後の検証に失敗: {e}"))?;

    println!(
        "取得完了: {copied} 件を {} へ配置 (SHA-256 照合済み)",
        dest.display()
    );
    println!(
        "注意: これらは GPL-2.0-or-later である。target/ 配下のみで使い、リポジトリへ持ち込まないこと。"
    );
    Ok(())
}

/// `target/fixtures/upstream` の絶対パス。
///
/// `CARGO_TARGET_DIR` が設定されていればそれを尊重する。
fn fixtures_dir() -> Result<PathBuf> {
    let target = match std::env::var_os("CARGO_TARGET_DIR") {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => repo_root()?.join("target"),
    };
    Ok(target.join(DEST_SUBDIR))
}

fn repo_root() -> Result<PathBuf> {
    match std::env::var_os("CARGO_MANIFEST_DIR") {
        Some(v) if !v.is_empty() => Ok(PathBuf::from(v)),
        // cargo 経由でない起動 (ビルド済みバイナリの直接実行) では現在地を使う。
        _ => std::env::current_dir().context("現在のディレクトリを取得できない"),
    }
}

/// 全件が揃っていて内容も一致しているか。不足・不一致なら理由を返す。
fn verify_all(dest: &Path, fixtures: &[&'static Fixture]) -> std::result::Result<(), String> {
    if !dest.is_dir() {
        return Err(format!("保存先が無い: {}", dest.display()));
    }
    for fx in fixtures {
        let path = dest.join(fx.name);
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(_) => return Err(format!("不足: {}", fx.name)),
        };
        if let Err(e) = check_bytes(fx, &bytes) {
            return Err(format!("{e}"));
        }
    }
    Ok(())
}

/// バイト数と SHA-256 を固定値と照合する。
fn check_bytes(fx: &Fixture, bytes: &[u8]) -> Result<()> {
    if bytes.len() as u64 != fx.size {
        bail!(
            "{}: バイト数が違う (期待 {}, 実際 {})",
            fx.name,
            fx.size,
            bytes.len()
        );
    }
    let got = hex(&sha256(bytes));
    if got != fx.sha256 {
        bail!(
            "{}: SHA-256 が違う (期待 {}, 実際 {})",
            fx.name,
            fx.sha256,
            got
        );
    }
    Ok(())
}

fn ensure_git() -> Result<()> {
    let out = Command::new("git").arg("--version").output();
    match out {
        Ok(o) if o.status.success() => Ok(()),
        _ => bail!(
            "git が見つからない。本家データの取得には git が必要。\n\
             オフライン環境では取得を諦めてよい (本家データを要するテストはスキップされる)。"
        ),
    }
}

fn clone_upstream(work: &Path) -> Result<()> {
    let out = Command::new("git")
        .args([
            "clone",
            "--quiet",
            "--depth",
            "1",
            "--branch",
            UPSTREAM_TAG,
            UPSTREAM_URL,
            ".",
        ])
        .current_dir(work)
        .output()
        .context("git clone を起動できない")?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!(
            "git clone が失敗した ({}):\n{}\n\
             ネットワークに到達できない環境では取得できない。\n\
             その場合は本家データを要するテストをスキップしたまま開発を進めること\n\
             (cargo test は本家 fixture 無しでも成功する)。",
            out.status,
            stderr.trim()
        );
    }
    Ok(())
}

fn git_head(work: &Path) -> Result<String> {
    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(work)
        .output()
        .context("git rev-parse を起動できない")?;
    if !out.status.success() {
        bail!("git rev-parse が失敗した ({})", out.status);
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// 取得物の素性を残す。「どのタグの何を、いつ、どの用途で置いたか」を後から追えるようにする。
fn write_provenance(dest: &Path, fixtures: &[&'static Fixture]) -> Result<()> {
    let mut s = String::new();
    writeln!(s, "# 本家 sysstat テストデータの取得記録").unwrap();
    writeln!(s, "#").unwrap();
    writeln!(
        s,
        "# これらは GPL-2.0-or-later であり reSARch (MIT) には同梱しない。"
    )
    .unwrap();
    writeln!(
        s,
        "# 再取得: cargo run --bin xtask -- fetch-fixtures --force"
    )
    .unwrap();
    writeln!(s, "#").unwrap();
    writeln!(s, "url    = {UPSTREAM_URL}").unwrap();
    writeln!(s, "tag    = {UPSTREAM_TAG}").unwrap();
    writeln!(s, "commit = {UPSTREAM_COMMIT}").unwrap();
    writeln!(s, "files  = {}", fixtures.len()).unwrap();
    writeln!(s).unwrap();
    for fx in fixtures {
        writeln!(
            s,
            "{}  {:>7}  {}  # {}",
            fx.sha256, fx.size, fx.name, fx.purpose
        )
        .unwrap();
    }
    let path = dest.join(PROVENANCE_FILE);
    std::fs::write(&path, s).with_context(|| format!("記録を書けない: {}", path.display()))?;
    Ok(())
}

// ===========================================================================
// SHA-256 (依存を足さないため自前実装。FIPS 180-4)
// ===========================================================================

const SHA256_K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

fn sha256(data: &[u8]) -> [u8; 32] {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    // メッセージパディング: 0x80、64 で割った余りが 56 になるまでゼロ、最後に総ビット長 (BE)。
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut padded = Vec::with_capacity(data.len() + 72);
    padded.extend_from_slice(data);
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    for block in padded.as_chunks::<64>().0 {
        sha256_compress(&mut h, block);
    }

    let mut out = [0u8; 32];
    for (chunk, word) in out.as_chunks_mut::<4>().0.iter_mut().zip(h.iter()) {
        *chunk = word.to_be_bytes();
    }
    out
}

fn sha256_compress(h: &mut [u32; 8], block: &[u8; 64]) {
    let mut w = [0u32; 64];
    for (slot, bytes) in w.iter_mut().take(16).zip(block.as_chunks::<4>().0) {
        *slot = u32::from_be_bytes(*bytes);
    }
    for i in 16..64 {
        let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
        let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16]
            .wrapping_add(s0)
            .wrapping_add(w[i - 7])
            .wrapping_add(s1);
    }

    let [mut a, mut b, mut c, mut d, mut e, mut fv, mut g, mut hv] = *h;

    for (k, wi) in SHA256_K.iter().zip(w.iter()) {
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let ch = (e & fv) ^ ((!e) & g);
        let t1 = hv
            .wrapping_add(s1)
            .wrapping_add(ch)
            .wrapping_add(*k)
            .wrapping_add(*wi);
        let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let maj = (a & b) ^ (a & c) ^ (b & c);
        let t2 = s0.wrapping_add(maj);

        hv = g;
        g = fv;
        fv = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }

    h[0] = h[0].wrapping_add(a);
    h[1] = h[1].wrapping_add(b);
    h[2] = h[2].wrapping_add(c);
    h[3] = h[3].wrapping_add(d);
    h[4] = h[4].wrapping_add(e);
    h[5] = h[5].wrapping_add(fv);
    h[6] = h[6].wrapping_add(g);
    h[7] = h[7].wrapping_add(hv);
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        write!(s, "{b:02x}").unwrap();
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FIPS 180-4 の公開テストベクタ。ハッシュ実装そのものを固定する。
    #[test]
    fn sha256_matches_published_vectors() {
        assert_eq!(
            hex(&sha256(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hex(&sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex(&sha256(
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            )),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    /// 64 バイト境界をまたぐ長さ (複数ブロック + パディングが次ブロックへ溢れる場合)。
    #[test]
    fn sha256_handles_block_boundaries() {
        let a1000 = vec![b'a'; 1000];
        assert_eq!(
            hex(&sha256(&a1000)),
            "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3"
        );
        // 長さ 55 / 56 / 57 はパディング分岐の境界。
        assert_eq!(
            hex(&sha256(&[b'a'; 55])),
            "9f4390f8d30c2dd92ec9f095b65e2b9ae9b0a925a5258e241c9f1e910f734318"
        );
        assert_eq!(
            hex(&sha256(&[b'a'; 56])),
            "b35439a4ac6f0948b6d6f9e3c6af0f5f590ce20f1bde7090ef7970686ec6738a"
        );
        assert_eq!(
            hex(&sha256(&[b'a'; 57])),
            "f13b2d724659eb3bf47f2dd6af1accc87b81f09f59f2b75e5c0bed6589dfe8c6"
        );
    }

    /// 取得対象の定義が壊れていないこと (名前重複・SHA-256 の桁数)。
    #[test]
    fn fixture_manifest_is_well_formed() {
        let all = all_fixtures();
        assert!(!all.is_empty());
        let mut names: Vec<&str> = all.iter().map(|f| f.name).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len(), "取得対象の名前が重複している");

        for fx in &all {
            assert_eq!(fx.sha256.len(), 64, "{}: SHA-256 の桁数が違う", fx.name);
            assert!(
                fx.sha256.bytes().all(|b| b.is_ascii_hexdigit()),
                "{}: SHA-256 に 16 進以外が混ざっている",
                fx.name
            );
            assert!(fx.size > 0, "{}: サイズが 0", fx.name);
            assert!(!fx.purpose.is_empty(), "{}: 用途が未記載", fx.name);
        }
    }
}

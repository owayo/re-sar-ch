//! CLI 層。
//!
//! 独自機能はサブコマンド、`sar` / `sadf` 互換動作は別パーサとして分離する
//! (`docs/design.md` の「9. CLI 構成」)。clap の derive 一つに全互換文法を
//! 押し込まない。
//!
//! ```text
//! resarch show <FILE>...        # 独自形式での閲覧
//! resarch summarize <FILE>...   # 期間集計
//! resarch detect <FILE>...      # 異変の当たり付け
//! resarch compare --host ...    # ホスト比較
//! resarch info <FILE>...        # ヘッダのみ表示
//! resarch sar  [sar オプション]  # sar 互換入口
//! resarch sadf [sadf オプション] # sadf 互換入口
//! resarch -u -f sa01            # サブコマンド省略時は sar 互換として解釈
//! ```
//!
//! 先頭引数がサブコマンド名でない場合 (`-` で始まる場合を含む) は
//! [`dispatch`] が `sar` 互換パーサへ委譲する。ただし `--help` / `--version` /
//! `-V` はルート専用として先に処理する。`sar` の `-h` は
//! 「`--pretty --human`」であってヘルプではないので、この規則と衝突しない。

pub mod sadf_args;
pub mod sar_args;
pub mod sar_el7_args;

use std::ffi::OsString;
use std::path::PathBuf;

use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum};

use crate::model::sar_profile::{ParsePageSizeError, ParseSarProfileError};
use crate::model::{DisplayTz, Lang, PageSize, SarProfile};

pub use sadf_args::{
    SadfArgError, SadfFormat, SadfImmediate, SadfOptions, SadfOutputOptions, SadfTimeBase,
    SvgPalette, parse_sadf_args,
};
pub use sar_args::{
    Activity, Caller, CpuBitmap, OptFlags, PersistentName, SarArgError, SarFlags, SarImmediate,
    SarInput, SarOptions, SarOutput, TimeSpec, parse_sar_args,
};
pub use sar_el7_args::{El7Input, SarEl7ArgError, SarEl7Options, parse_sar_el7_args};

/// ルートが受け付けるサブコマンド名。
///
/// 先頭引数がこのいずれでもなければ `sar` 互換として解釈する。
pub const SUBCOMMAND_NAMES: [&str; 11] = [
    "sa2sar",
    "tui",
    "show",
    "summarize",
    "detect",
    "compare",
    "info",
    "identify",
    "skill-install",
    "sar",
    "sadf",
];

// ============================================================================
// 共通の値型
// ============================================================================

/// 出力形式。独自形式と互換形式を 1 つの列挙で扱う。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum OutputFormat {
    /// 人間向けの表形式 (既定)。
    #[default]
    Table,
    /// 独自 JSON。
    Json,
    /// 独自 CSV。
    Csv,
    /// 独自 NDJSON (AI エージェント向け。`schema_version` 付き)。
    Ndjson,
    /// `sar` 互換テキスト。
    Sar,
    /// `sadf -p` 互換。
    #[value(name = "sadf-p")]
    SadfPpc,
    /// `sadf -d` 互換。
    #[value(name = "sadf-d")]
    SadfDb,
    /// `sadf -j` 互換。
    #[value(name = "sadf-json")]
    SadfJson,
    /// `sadf -x` 互換。
    #[value(name = "sadf-xml")]
    SadfXml,
    /// `sadf -r` 互換。
    #[value(name = "sadf-raw")]
    SadfRaw,
}

/// 生値と派生値のどちらを出すか (`--values`)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum ValueKind {
    /// 累積カウンタの生値のみ。
    Raw,
    /// レート・割合などの派生値のみ (既定)。
    #[default]
    Derived,
    /// 生値と派生値を別名前空間で両方出す。
    Both,
}

/// `compare --host NAME=PATH` の指定。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostSpec {
    /// 比較表に出す名前。
    pub name: String,
    /// 対象の `sa` ファイル (またはディレクトリ)。
    pub path: PathBuf,
}

/// `NAME=PATH` を [`HostSpec`] に解釈する。
fn parse_host_spec(value: &str) -> Result<HostSpec, String> {
    let Some((name, path)) = value.split_once('=') else {
        return Err("NAME=PATH の形式で指定してください".to_string());
    };
    if name.is_empty() || path.is_empty() {
        return Err("NAME と PATH の両方が必要です".to_string());
    }
    Ok(HostSpec {
        name: name.to_string(),
        path: PathBuf::from(path),
    })
}

// ============================================================================
// サブコマンドの引数 (骨格)
// ============================================================================

/// 独自サブコマンドで共通の時刻基準。
///
/// **表示と `--from` / `--to` の比較と `detect` の日内境界は同じ基準を使う。**
/// 片方だけ動かすと、画面の 09:00 と `--from 09:00` が食い違う。
///
/// 互換入口 (`resarch sar` / `resarch sadf` / `resarch sa2sar`) はこの引数を
/// 持たない。あちらは本家の規則 (`-T` / `-t` / `-U`) に従う。
#[derive(Debug, Clone, PartialEq, Eq, Args, Default)]
pub struct TimeZoneArgs {
    /// 時刻の表示と `--from` / `--to` の解釈に使うタイムゾーン (既定: local)。
    ///
    /// `local` は実行環境のタイムゾーン、`utc` は UTC、ほかに
    /// `Asia/Tokyo` のような IANA 名を指定できる。
    /// 機械可読形式 (json / ndjson / csv) の epoch 秒はこの指定で変わらない。
    #[arg(long, value_name = "TZ", verbatim_doc_comment)]
    pub timezone: Option<String>,

    /// `--timezone utc` の別名。
    #[arg(long, conflicts_with = "timezone")]
    pub utc: bool,
}

impl TimeZoneArgs {
    /// 表示・比較に使うタイムゾーンを決める。
    ///
    /// 未指定は実行環境のローカルタイムゾーン。
    pub fn resolve(&self) -> Result<DisplayTz, String> {
        match (self.timezone.as_deref(), self.utc) {
            // clap の `conflicts_with` が先に弾くが、
            // 引数を組み立て直す経路のために意味を落とさず持つ。
            (Some(_), true) => Err("--timezone と --utc は同時に指定できません".to_string()),
            (Some(v), false) => DisplayTz::parse(v),
            (None, true) => Ok(DisplayTz::Utc),
            (None, false) => Ok(DisplayTz::local()),
        }
    }
}

/// 独自出力の表示言語。
///
/// 互換入口 (`resarch sar` / `resarch sadf` / `resarch sa2sar`) はこの引数を
/// 持たない。本家の書式をそのまま出すので、言語の選択肢が無い。
#[derive(Debug, Clone, PartialEq, Eq, Args, Default)]
pub struct LangArgs {
    /// 表示言語 (`ja` / `en`)。
    ///
    /// 未指定なら実行環境から決める。優先順は
    /// `RESARCH_LANG` → `LC_ALL` / `LC_MESSAGES` / `LANG` → `LANGUAGE` →
    /// ローカルタイムゾーン (日本なら `ja`) → 英語。
    ///
    /// **ロケール環境変数はタイムゾーンより強い。** ロケールは
    /// 「どの言語で読みたいか」の宣言そのもので、タイムゾーンは
    /// 「どこにいるか」でしかない。
    #[arg(long, value_name = "LANG", verbatim_doc_comment)]
    pub lang: Option<String>,
}

impl LangArgs {
    /// 表示言語を決める。
    pub fn resolve(&self) -> Result<Lang, String> {
        match self.lang.as_deref() {
            Some(v) => Lang::parse_option(v),
            None => Ok(Lang::from_env()),
        }
    }
}

/// 複数のサブコマンドで共通の引数。
#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct CommonArgs {
    /// 出力形式。
    #[arg(long, value_enum, default_value_t = OutputFormat::Table)]
    pub format: OutputFormat,

    #[command(flatten)]
    pub timezone: TimeZoneArgs,

    #[command(flatten)]
    pub language: LangArgs,

    /// 対象 activity をカンマ区切りで絞る (例: `cpu,disk`)。
    #[arg(long, value_name = "LIST", value_delimiter = ',')]
    pub activity: Vec<String>,

    /// 開始時刻 (`hh:mm[:ss]` または 10 桁の epoch 秒)。
    ///
    /// `hh:mm[:ss]` は `--timezone` の基準 (既定は実行環境のローカル
    /// タイムゾーン) で解釈する。epoch 秒はタイムゾーンの影響を受けない。
    ///
    /// `show` では表示する行、`summarize` / `compare` では**集計期間そのもの**を
    /// 絞る。範囲外のサンプルは平均・最大 / 最小・p95・差分合計のどれにも
    /// 入らず、期間の端点 (`first_ust` / `last_ust` / `covered_cs`) も
    /// 範囲内だけになる。
    ///
    /// `sar -s` と同じく、**範囲に最初に合致したサンプルは差分の基準として
    /// 消費される** (`show` では表示されず、`summarize` では値に数えない)。
    /// 複数ファイルを渡した場合はファイルごとに引き直すので、
    /// `--from 09:00 --to 18:00` は「各日の 09:00〜18:00」を意味する。
    ///
    /// `detect` の `--from` / `--to` は意味が違う (報告範囲だけを絞り、
    /// 比較基準の材料は絞らない)。`resarch detect --help` を参照。
    #[arg(long, value_name = "TIME", verbatim_doc_comment)]
    pub from: Option<String>,

    /// 終了時刻 (`hh:mm[:ss]` または 10 桁の epoch 秒)。
    ///
    /// 意味は `--from` と対。`hh:mm[:ss]` 形式で `--to` < `--from` のときは
    /// 翌日までを指す (`sar` と同じ日跨ぎ補正)。
    #[arg(long, value_name = "TIME", verbatim_doc_comment)]
    pub to: Option<String>,

    /// 疑わしいデータをエラーにする (既定)。
    #[arg(long, conflicts_with = "lenient")]
    pub strict: bool,

    /// 回復可能な破損を診断付きで読み飛ばす。
    #[arg(long)]
    pub lenient: bool,

    /// mmap を使わず BufReader で読む (採取進行中のファイル向け)。
    #[arg(long)]
    pub no_mmap: bool,

    /// 並列処理するファイル数。
    #[arg(long, value_name = "N")]
    pub jobs: Option<usize>,
}

/// `resarch show` の引数。
#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct ShowArgs {
    // `SummarizeArgs::help` と同じ理由で自前の `--help` を持つ。
    // `--from` / `--to` の説明が読めないと、基準レコードが表示されない挙動を
    // 利用者が確かめられない。
    /// ヘルプを表示する。
    #[arg(long, action = ArgAction::Help)]
    help: Option<bool>,

    /// 解析対象の `sa` ファイル。
    #[arg(value_name = "FILE", required = true, num_args = 1..)]
    pub files: Vec<PathBuf>,

    /// 生値と派生値のどちらを出すか。
    #[arg(long, value_enum, default_value_t = ValueKind::Derived)]
    pub values: ValueKind,

    /// 独自出力で割り込みの CPU 別内訳も出す (既定は CPU all のみ)。
    #[arg(long)]
    pub irq_cpus: bool,

    #[command(flatten)]
    pub common: CommonArgs,
}

/// `resarch summarize` の引数。
#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct SummarizeArgs {
    // ルートの `disable_help_flag` は子コマンドへ伝播するため、自前で
    // `--help` を定義する ([`DetectArgs::help`] と同じ理由)。
    // `--from` / `--to` の意味が `detect` と違うので、ヘルプが読めないままに
    // しておけない。
    /// ヘルプを表示する。
    #[arg(long, action = ArgAction::Help)]
    help: Option<bool>,

    /// 解析対象の `sa` ファイル。
    #[arg(value_name = "FILE", required = true, num_args = 1..)]
    pub files: Vec<PathBuf>,

    #[command(flatten)]
    pub common: CommonArgs,
}

// ----------------------------------------------------------------------------
// `resarch detect`
// ----------------------------------------------------------------------------

/// 異変検出の出力形式。
///
/// [`OutputFormat`] を流用しない。`detect` の所見は表でもなく `sar` 互換でもなく、
/// 出せない形式を `--help` に並べると読み手を惑わせる。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum DetectFormat {
    /// 人が読む形式 (既定)。要約で、全文は `--verbose`。
    #[default]
    Text,
    /// 独自 JSON (エージェント向け。型のフィールドをそのまま出す)。
    Json,
    /// 独自 NDJSON (エピソード 1 件 = 1 行)。
    Ndjson,
}

/// 比較基準の材料をどこから取るか。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum BaselineScopeArg {
    /// 入力全体から作る (既定)。
    #[default]
    Input,
    /// `--from` / `--to` の範囲だけから作る。
    Window,
}

/// 報告する調査優先度の下限。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum PriorityArg {
    /// すべて出す (既定)。
    #[default]
    Informational,
    Watch,
    Investigate,
}

/// `resarch detect` の引数。
#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct DetectArgs {
    // ルートの `disable_help_flag` は子コマンドへ伝播するため、
    // このサブコマンドだけ自前で `--help` を定義する。
    // `--from` / `--to` と `--baseline-scope` の関係は説明が要るので、
    // ヘルプが読めないままにしておけない。
    /// ヘルプを表示する。
    #[arg(long, action = ArgAction::Help)]
    help: Option<bool>,

    /// 解析対象の `sa` ファイル。
    #[arg(value_name = "FILE", required = true, num_args = 1..)]
    pub files: Vec<PathBuf>,

    /// 出力形式。
    #[arg(long, value_enum, default_value_t = DetectFormat::Text)]
    pub format: DetectFormat,

    /// 全件の詳細と考えられる解釈を出す。
    ///
    /// 既定の text は優先度順に最大5系列、各系列の時刻を最大3件表示する。
    /// 代表例の観測値と判定理由を添え、背景の所見は最大3件表示する。
    /// --verbose を付けると、表示件数の上限を外し、検出ごとの根拠も表示する。
    ///
    /// 比較基準の出所、評価できた範囲、この結果だけでは分からないことは、
    /// 要約にも表示する。省略した件数も記載する。
    ///
    /// json / ndjson は、この指定によらず全件・全フィールドを出す。
    #[arg(long, verbatim_doc_comment)]
    pub verbose: bool,

    #[command(flatten)]
    pub timezone: TimeZoneArgs,

    #[command(flatten)]
    pub language: LangArgs,

    /// 検知したリソース・指標ごとの SVG と一覧を保存する新規ディレクトリ。
    /// 通常の検知レポートも標準出力へ出す。既存ディレクトリは上書きしない。
    #[arg(long, value_name = "DIR")]
    pub svg_dir: Option<PathBuf>,

    /// SVG に含める検知範囲の前後幅 (既定: 30m)。例: 300s、15m、1h。0 も指定可。
    /// 報告範囲の外も入力にあれば表示する。検知や比較基準は変えない。
    #[arg(long, value_name = "DURATION", requires = "svg_dir", value_parser = parse_svg_context)]
    pub svg_context: Option<u64>,

    /// 比較基準の材料をどこから取るか。
    ///
    /// **`--from` / `--to` は報告範囲を絞るだけで、基準の材料は絞らない。**
    /// 狭い調査範囲の外から比較材料を取れるようにするため、既定は `input`
    /// (入力全体) である。`window` を指定したときだけ、基準の材料も
    /// `--from` / `--to` の範囲に絞られる。
    ///
    /// どちらの場合も基準は**この入力自身**から作ったものであり、
    /// 外部の正常値ではない。異変が入力の大半を占めていれば基準もそちらへ寄る。
    #[arg(long, value_enum, default_value_t = BaselineScopeArg::Input, verbatim_doc_comment)]
    pub baseline_scope: BaselineScopeArg,

    /// 報告する調査優先度の下限。
    ///
    /// 優先度は順序尺度であり、確率ではない。
    #[arg(long, value_enum, default_value_t = PriorityArg::Informational)]
    pub min_priority: PriorityArg,

    /// 対象 activity をカンマ区切りで絞る (例: `cpu,disk`)。
    #[arg(long, value_name = "LIST", value_delimiter = ',')]
    pub activity: Vec<String>,

    /// 報告範囲の開始時刻 (hh:mm[:ss] または epoch 秒)。基準の材料は絞らない。
    ///
    /// `hh:mm[:ss]` は `--timezone` の基準 (既定はローカル) で解釈する。
    #[arg(long, value_name = "TIME")]
    pub from: Option<String>,

    /// 報告範囲の終了時刻 (hh:mm[:ss] または epoch 秒)。基準の材料は絞らない。
    #[arg(long, value_name = "TIME")]
    pub to: Option<String>,

    /// 疑わしいデータをエラーにする (既定)。
    #[arg(long, conflicts_with = "lenient")]
    pub strict: bool,

    /// 回復可能な破損を診断付きで読み飛ばす。
    #[arg(long)]
    pub lenient: bool,

    /// mmap を使わず BufReader で読む (採取進行中のファイル向け)。
    #[arg(long)]
    pub no_mmap: bool,

    /// 並列処理するファイル数。
    #[arg(long, value_name = "N")]
    pub jobs: Option<usize>,
}

fn parse_svg_context(value: &str) -> Result<u64, String> {
    let (number, factor) = if let Some(n) = value.strip_suffix('s') {
        (n, 1)
    } else if let Some(n) = value.strip_suffix('m') {
        (n, 60)
    } else if let Some(n) = value.strip_suffix('h') {
        (n, 3600)
    } else if value == "0" {
        (value, 1)
    } else {
        return Err("前後幅は 300s、15m、1h または 0 の形式で指定してください".into());
    };
    if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
        return Err("前後幅には 0 以上の整数と s/m/h を指定してください".into());
    }
    number
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(factor))
        .ok_or_else(|| "前後幅が大きすぎます".into())
}

/// `resarch compare` の引数。
#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct CompareArgs {
    // `SummarizeArgs::help` と同じ理由で自前の `--help` を持つ。
    /// ヘルプを表示する。
    #[arg(long, action = ArgAction::Help)]
    help: Option<bool>,

    /// 比較対象を `NAME=PATH` で指定する (複数回指定可)。
    ///
    /// `--from` / `--to` はホストごとの集計期間を絞り、その結果として
    /// 比較の共通時間窓 (各ホストの観測期間の交差) も絞られる。
    #[arg(
        long = "host",
        value_name = "NAME=PATH",
        required = true,
        action = ArgAction::Append,
        value_parser = parse_host_spec,
        verbatim_doc_comment
    )]
    pub hosts: Vec<HostSpec>,

    #[command(flatten)]
    pub common: CommonArgs,
}

/// `resarch info` の引数。
#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct InfoArgs {
    // `SummarizeArgs::help` と同じ理由で自前の `--help` を持つ。
    /// ヘルプを表示する。
    #[arg(long, action = ArgAction::Help)]
    help: Option<bool>,

    /// 解析対象の `sa` ファイル。
    #[arg(value_name = "FILE", required = true, num_args = 1..)]
    pub files: Vec<PathBuf>,

    /// 出力形式。
    #[arg(long, value_enum, default_value_t = OutputFormat::Table)]
    pub format: OutputFormat,

    // `--timezone` は持たない。`info` が出す `date` / `timezone` は
    // **ファイルヘッダに書かれている値そのもの** (採取側が記録した日付と
    // TZ 名) であり、読み手のタイムゾーンで開き直す対象ではない。
    /// 疑わしいデータをエラーにする (既定)。
    #[arg(long, conflicts_with = "lenient")]
    pub strict: bool,

    /// 回復可能な破損を診断付きで読み飛ばす。
    #[arg(long)]
    pub lenient: bool,

    /// mmap を使わず BufReader で読む。
    #[arg(long)]
    pub no_mmap: bool,
}

/// `resarch identify` の引数。
#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct IdentifyArgs {
    // `SummarizeArgs::help` と同じ理由で自前の `--help` を持つ。
    /// ヘルプを表示する。
    #[arg(long, action = ArgAction::Help)]
    help: Option<bool>,

    /// 判定対象の `sa` ファイル。
    #[arg(value_name = "FILE", required = true, num_args = 1..)]
    pub files: Vec<PathBuf>,

    /// 出力形式。
    #[arg(long, value_enum, default_value_t = OutputFormat::Table)]
    pub format: OutputFormat,
}

/// `resarch tui` の引数。
#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct TuiArgs {
    // `SummarizeArgs::help` と同じ理由で自前の `--help` を持つ。
    /// ヘルプを表示する。
    #[arg(long, action = ArgAction::Help)]
    help: Option<bool>,

    /// 閲覧する `sa` ファイル。
    #[arg(value_name = "FILE")]
    pub file: PathBuf,

    /// 表示する activity を絞る (既定は収録されている全部)。
    #[arg(long, value_delimiter = ',')]
    pub activity: Vec<String>,

    #[command(flatten)]
    pub timezone: TimeZoneArgs,

    #[command(flatten)]
    pub language: LangArgs,

    /// 回復可能な破損を診断付きで読み飛ばす。
    #[arg(long)]
    pub lenient: bool,

    /// mmap を使わず BufReader で読む。
    #[arg(long)]
    pub no_mmap: bool,
}

/// `resarch sa2sar` の引数。
#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct Sa2SarArgs {
    /// ヘルプを表示する。
    #[arg(long, action = ArgAction::Help)]
    help: Option<bool>,

    /// 読み出す sa バイナリファイル。
    #[arg(value_name = "FILE")]
    pub file: PathBuf,

    /// sar テキストの保存先。省略または - なら標準出力。既存ファイルは上書きしない。
    #[arg(short, long, value_name = "FILE")]
    pub output: Option<PathBuf>,

    /// 採取元に記録された時刻の代わりに UTC で出力する。
    #[arg(long)]
    pub utc: bool,

    /// mmap を使わず BufReader で読む。
    #[arg(long)]
    pub no_mmap: bool,

    /// 再現する sar の版。current (= sysstat-12.8.0、既定) / sysstat-10.1.5-el7。
    ///
    /// sysstat-10.1.5-el7 は RHEL / CentOS 7 の sar が出すテキストをそのまま再現する
    /// (列・計算・平均の丸めまで)。読めるのは format_magic 0x2171 のファイルだけ。
    #[arg(long, value_name = "PROFILE", default_value = "current")]
    pub sar_profile: crate::model::SarProfile,

    /// sysstat-10.1.5-el7 の -R (frmpg/s など) が使うページサイズ (バイト)。既定 4096。
    ///
    /// ファイルには記録されないので、再現したいホストの値を指定する
    /// (x86_64 は 4096、ppc64 系は 65536 が多い)。他のプロファイルでは使えない。
    #[arg(long, value_name = "BYTES")]
    pub sar_page_size: Option<crate::model::PageSize>,
}

/// `resarch skill-install` の引数。
///
/// AI エージェントへ「この CLI の使い方」を渡すためのサブコマンド。
/// スキル本文はバイナリに埋め込んであるので、リリースバイナリ 1 本で完結する。
#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct SkillArgs {
    // `SummarizeArgs::help` と同じ理由で自前の `--help` を持つ。
    /// ヘルプを表示する。
    #[arg(long, action = ArgAction::Help)]
    help: Option<bool>,

    /// インストール先のエージェント。
    ///
    /// `claude` は `~/.claude/skills/resarch/SKILL.md`、
    /// `codex` は `~/.codex/skills/resarch/SKILL.md` へ書く。
    /// 既にあれば**上書きする** (古い本文が残ると、存在しないオプションを
    /// エージェントが案内してしまう)。
    #[arg(value_name = "AGENT", required = true)]
    pub agent: String,
}

/// 互換入口に渡す生の引数列。
///
/// `sar` / `sadf` の文法は clap では表現できないため、ここでは一切解釈せずに
/// 受け取り、[`parse_sar_args`] / [`parse_sadf_args`] へ渡す。
#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct CompatArgs {
    /// 互換パーサへそのまま渡す引数列。
    #[arg(value_name = "OPTIONS", num_args = 0.., allow_hyphen_values = true, trailing_var_arg = true)]
    pub args: Vec<String>,
}

// ============================================================================
// ルート CLI
// ============================================================================

/// `resarch` のサブコマンド。
#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum Commands {
    /// sa バイナリから全項目の sar テキスト (平均・再起動・コメントを含む) を生成する。
    #[command(disable_help_flag = true, name = "sa2sar")]
    Sa2Sar(Sa2SarArgs),
    /// 独自形式で閲覧する。
    //
    // `--help` の扱いは [`Commands::Summarize`] と同じ。
    #[command(disable_help_flag = true)]
    Show(ShowArgs),
    /// 期間集計とボトルネック判定を出す。
    //
    // 自動生成の `--help` を止めて [`SummarizeArgs`] 側で定義する
    // (理由は [`SummarizeArgs::help`] を参照)。この注記は利用者向けの説明では
    // ないので、`--help` に出ないよう doc コメントにしない。
    #[command(disable_help_flag = true)]
    Summarize(SummarizeArgs),
    /// いつ・何に異変があったか当たりを付ける。
    //
    // 自動生成の `--help` を止めて [`DetectArgs`] 側で定義する
    // (ルートと同じ作法。理由は [`DetectArgs::help`] を参照)。
    #[command(disable_help_flag = true)]
    Detect(DetectArgs),
    /// 複数ホストを比較する。
    //
    // `--help` の扱いは [`Commands::Summarize`] と同じ。
    #[command(disable_help_flag = true)]
    Compare(CompareArgs),
    /// ファイルヘッダ (世代・ABI・activity 一覧) のみ表示する。
    //
    // `--help` の扱いは [`Commands::Summarize`] と同じ。
    #[command(disable_help_flag = true)]
    Info(InfoArgs),
    /// どの世代・どのバージョンの sysstat が書いたファイルかを判定する。
    ///
    /// ヘッダを解釈できない世代でも判定結果を返す点が `info` と違う。
    /// 読めないことは失敗ではないので、終了コードも 0 のままにする。
    //
    // `--help` の扱いは [`Commands::Summarize`] と同じ。
    #[command(disable_help_flag = true)]
    Identify(IdentifyArgs),
    /// sa ファイルを対話的に閲覧する (TUI)。
    //
    // `--help` の扱いは [`Commands::Summarize`] と同じ。
    //
    // **`skills/SKILL.md` には意図的に載せていない。** スキルは AI エージェントが
    // 読むものだが、TUI は対話端末を必要とするのでエージェントからは使えない。
    // 載せると選択肢として検討され、端末の無い環境で失敗する経路が増えるだけになる。
    // 「サブコマンドを足したのに skill が未更新」ではなく、載せない判断である。
    #[command(disable_help_flag = true)]
    Tui(TuiArgs),
    /// AI エージェント向けのスキルをインストールする。
    //
    // `--help` の扱いは [`Commands::Summarize`] と同じ。
    #[command(disable_help_flag = true, name = "skill-install")]
    SkillInstall(SkillArgs),
    /// sar 互換入口 (サブコマンドを省略した場合もこちらへ委譲される)。
    #[command(disable_help_flag = true)]
    Sar(CompatArgs),
    /// sadf 互換入口。
    #[command(disable_help_flag = true)]
    Sadf(CompatArgs),
}

/// ルート CLI。
#[derive(Debug, Clone, Parser)]
#[command(
    name = "resarch",
    version,
    about = "sysstat の sa ファイルを sar / sadf に依存せず解析する",
    long_about = None,
    disable_help_flag = true,
    arg_required_else_help = true
)]
pub struct Cli {
    /// ヘルプを表示する。
    ///
    /// `sar` の `-h` は `--pretty --human` であって help ではないため、
    /// ルートのヘルプは**長形式のみ**にして曖昧さを消している。
    #[arg(long, action = ArgAction::Help)]
    help: Option<bool>,

    /// 実行するサブコマンド。
    #[command(subcommand)]
    pub command: Commands,
}

// ============================================================================
// ディスパッチ
// ============================================================================

/// 引数解析の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invocation {
    /// 独自サブコマンド (`show` / `summarize` / `compare` / `info`)。
    Native(Box<Commands>),
    /// `sar` 互換。
    Sar(Box<SarOptions>),
    /// `sar` 互換 (`--sar-profile sysstat-10.1.5-el7`)。
    SarEl7(Box<SarEl7Options>),
    /// `sadf` 互換。
    Sadf(Box<SadfOptions>),
}

/// CLI 全体のエラー。
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    /// `sar` 互換パーサのエラー。
    #[error(transparent)]
    Sar(#[from] SarArgError),
    /// `sadf` 互換パーサのエラー。
    #[error(transparent)]
    Sadf(#[from] SadfArgError),
    /// `--sar-profile sysstat-10.1.5-el7` の `sar` 互換パーサのエラー。
    #[error(transparent)]
    SarEl7(#[from] SarEl7ArgError),
    /// `--sar-profile` の値が不正。
    #[error(transparent)]
    SarProfile(#[from] ParseSarProfileError),
    /// `--sar-page-size` の値が不正。
    #[error(transparent)]
    PageSize(#[from] ParsePageSizeError),
    /// `--sar-profile` / `--sar-page-size` の使い方の誤り。
    #[error("{0}")]
    ProfileUsage(String),
    /// UTF-8 として読めない引数。`sar` / `sadf` 互換入口は引数を文字列として解析する。
    #[error(
        "UTF-8 として読めない引数があります: {0} \
         (sar / sadf 互換の入口は UTF-8 の引数だけを受け付けます)"
    )]
    NonUtf8Arg(String),
    /// clap のエラー。`--help` / `--version` の正常表示もここに入る
    /// ([`clap::Error::exit`] でそのまま終了できる)。
    #[error(transparent)]
    Clap(#[from] clap::Error),
}

/// 引数列がルート専用オプション (先に処理して曖昧さを消すもの) かどうか。
fn is_root_only(arg: &str) -> bool {
    matches!(arg, "--help" | "--version" | "-V" | "help")
}

/// 先頭引数がサブコマンド名か。
pub fn is_subcommand_name(arg: &str) -> bool {
    SUBCOMMAND_NAMES.contains(&arg)
}

/// プログラム名を**含まない**引数列を解釈する。
///
/// ```
/// use re_sar_ch::cli::{dispatch, Invocation};
/// use re_sar_ch::cli::sar_args::{Activity, SarInput};
///
/// // サブコマンド省略時は sar 互換として解釈される
/// let argv: Vec<String> = ["-u", "-f", "sa01"].iter().map(|s| s.to_string()).collect();
/// let Invocation::Sar(opts) = dispatch(&argv).unwrap() else { panic!() };
/// assert!(opts.is_selected(Activity::Cpu));
/// assert_eq!(opts.input, Some(SarInput::File("sa01".into())));
/// ```
pub fn dispatch(argv: &[String]) -> Result<Invocation, CliError> {
    match argv.first().map(String::as_str) {
        // 引数なし / ルート専用オプションは clap に任せる。
        None => Ok(Invocation::Native(Box::new(parse_root(argv)?))),
        Some(first) if is_root_only(first) => Ok(Invocation::Native(Box::new(parse_root(argv)?))),
        // 互換入口は clap を通さず直接互換パーサへ渡す
        // (clap が `-h` や `--dec=0` を解釈してしまうのを避ける)。
        Some("sar") => parse_sar_entry(&argv[1..]),
        Some("sadf") => {
            // プロファイルは sar テキストだけの機能。sadf の形式 (JSON / XML / CSV など) は
            // 版ごとのキー・構造を検証していないので、黙って現行版で出さずに止める。
            if argv[1..]
                .iter()
                .any(|a| a == "--sar-profile" || a.starts_with("--sar-profile="))
            {
                return Err(CliError::ProfileUsage(
                    "--sar-profile は sar / sa2sar のテキスト出力だけで使えます \
                     (sadf 互換出力は現行版 sysstat-12.8.0 の書式のみ)"
                        .into(),
                ));
            }
            Ok(Invocation::Sadf(Box::new(parse_sadf_args(&argv[1..])?)))
        }
        Some(first) if is_subcommand_name(first) => {
            Ok(Invocation::Native(Box::new(parse_root(argv)?)))
        }
        // 先頭がサブコマンド名でない (`-` 始まりを含む) → sar 互換として解釈。
        Some(_) => parse_sar_entry(argv),
    }
}

/// `sar` 互換入口。`--sar-profile` / `--sar-page-size` を抜き出して、
/// プロファイルに応じた文法のパーサへ渡す。
///
/// 2 つのオプションは本家に無い reSARch の拡張なので、どの位置に書いても
/// 本家の文法を崩さないよう**先に取り除いてから**残りを解析する。
/// `--sar-profile=X` と `--sar-profile X` の両方を受け付ける。
pub fn parse_sar_entry(args: &[String]) -> Result<Invocation, CliError> {
    let mut profile: Option<SarProfile> = None;
    let mut page: Option<PageSize> = None;
    let mut rest: Vec<String> = Vec::with_capacity(args.len());
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        let (name, inline) = match a.split_once('=') {
            Some((n, v)) if n == "--sar-profile" || n == "--sar-page-size" => (n, Some(v)),
            _ => (a, None),
        };
        if name != "--sar-profile" && name != "--sar-page-size" {
            rest.push(args[i].clone());
            i += 1;
            continue;
        }
        let value = match inline {
            Some(v) => v.to_string(),
            None => {
                i += 1;
                args.get(i)
                    .cloned()
                    .ok_or_else(|| CliError::ProfileUsage(format!("{name} には値が必要です")))?
            }
        };
        if name == "--sar-profile" {
            profile = Some(value.parse()?);
        } else {
            page = Some(value.parse()?);
        }
        i += 1;
    }
    match profile.unwrap_or_default() {
        SarProfile::Sysstat1015El7 => Ok(Invocation::SarEl7(Box::new(parse_sar_el7_args(
            &rest,
            page.unwrap_or_default(),
        )?))),
        SarProfile::Sysstat1280 => {
            if page.is_some() {
                return Err(CliError::ProfileUsage(
                    "--sar-page-size は --sar-profile sysstat-10.1.5-el7 と一緒にしか使えません \
                     (現行版の sar は -R を持たないため、ページサイズを使う列がありません)"
                        .into(),
                ));
            }
            Ok(Invocation::Sar(Box::new(parse_sar_args(&rest)?)))
        }
    }
}

/// OS から受け取った引数列 (プログラム名を**含まない**) を解釈する。
///
/// Linux のファイル名はバイト列なので、UTF-8 として読めない引数もありうる。
/// そうした引数が混ざっていても panic しない。独自サブコマンドは clap が
/// `OsString` のまま `PathBuf` に渡すので、そのまま読める。`sar` / `sadf` 互換入口は
/// 引数を文字列として解析するので [`CliError::NonUtf8Arg`] を返す。
pub fn dispatch_os(argv: &[OsString]) -> Result<Invocation, CliError> {
    let utf8: Option<Vec<String>> = argv.iter().map(|a| a.to_str().map(str::to_owned)).collect();
    if let Some(argv) = utf8 {
        return dispatch(&argv);
    }
    match argv.first().and_then(|a| a.to_str()) {
        Some(first)
            if first != "sar"
                && first != "sadf"
                && (is_root_only(first) || is_subcommand_name(first)) =>
        {
            let full = std::iter::once(OsString::from("resarch")).chain(argv.iter().cloned());
            Ok(Invocation::Native(Box::new(
                Cli::try_parse_from(full)?.command,
            )))
        }
        _ => {
            let bad = argv
                .iter()
                .find(|a| a.to_str().is_none())
                .map(|a| a.to_string_lossy().into_owned())
                .unwrap_or_default();
            Err(CliError::NonUtf8Arg(bad))
        }
    }
}

/// `std::env::args_os()` から解釈する。
pub fn dispatch_from_env() -> Result<Invocation, CliError> {
    let argv: Vec<OsString> = std::env::args_os().skip(1).collect();
    dispatch_os(&argv)
}

/// ルート (clap) 側の解析。
fn parse_root(argv: &[String]) -> Result<Commands, clap::Error> {
    let full = std::iter::once("resarch".to_string()).chain(argv.iter().cloned());
    let cli = Cli::try_parse_from(full)?;
    Ok(cli.command)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn clap_definition_is_valid() {
        Cli::command().debug_assert();
    }

    /// UTF-8 で読めない引数で panic しない (`std::env::args()` は panic する)。
    #[cfg(unix)]
    #[test]
    fn non_utf8_arguments_do_not_panic() {
        use std::os::unix::ffi::OsStringExt;
        let os = |s: &str| OsString::from(s);
        let bad = OsString::from_vec(vec![b's', b'a', 0xff]);

        // 独自サブコマンドは clap が OsString のまま PathBuf に渡す
        let Invocation::Native(cmd) = dispatch_os(&[os("info"), bad.clone()]).unwrap() else {
            panic!("独自サブコマンドとして解釈されるべき");
        };
        let Commands::Info(args) = *cmd else {
            panic!("info のはず");
        };
        assert_eq!(args.files, vec![PathBuf::from(bad.clone())]);

        // 互換入口は文字列で解析するのでエラーにする
        for argv in [
            vec![os("sar"), os("-f"), bad.clone()],
            vec![os("-f"), bad.clone()],
            vec![os("sadf"), bad.clone()],
        ] {
            assert!(
                matches!(dispatch_os(&argv), Err(CliError::NonUtf8Arg(_))),
                "{argv:?}"
            );
        }
        // UTF-8 だけなら従来の解釈と同じ
        let Invocation::Sar(_) = dispatch_os(&[os("-u"), os("-f"), os("sa01")]).unwrap() else {
            panic!("sar 互換として解釈されるべき");
        };
    }

    // ---------------------------------------------------------------
    // サブコマンド省略時の sar 委譲 (要件の中心)
    // ---------------------------------------------------------------

    #[test]
    fn bare_options_are_parsed_as_sar() {
        let Invocation::Sar(opts) = dispatch(&argv(&["-u", "-f", "sa01"])).unwrap() else {
            panic!("sar 互換として解釈されるべき");
        };
        assert!(opts.is_selected(Activity::Cpu));
        assert_eq!(opts.opt_flags(Activity::Cpu), OptFlags::CPU_DEF);
        assert_eq!(opts.input, Some(SarInput::File(PathBuf::from("sa01"))));
    }

    #[test]
    fn explicit_sar_subcommand_matches_bare_form() {
        let bare = dispatch(&argv(&["-u", "-f", "sa01"])).unwrap();
        let explicit = dispatch(&argv(&["sar", "-u", "-f", "sa01"])).unwrap();
        assert_eq!(bare, explicit);
    }

    #[test]
    fn sar_h_is_not_root_help() {
        // -h は --pretty --human。ルートのヘルプは長形式のみなので衝突しない。
        let Invocation::Sar(opts) = dispatch(&argv(&["-h", "-f", "sa01"])).unwrap() else {
            panic!("sar 互換として解釈されるべき");
        };
        assert!(opts.flags.pretty);
        assert!(opts.flags.human);
        assert!(opts.immediate.is_none());
    }

    #[test]
    fn sar_long_help_is_handled_by_the_sar_parser_after_the_subcommand() {
        // `resarch sar --help` は sar 互換入口なので sar の --help (display_help)
        let Invocation::Sar(opts) = dispatch(&argv(&["sar", "--help"])).unwrap() else {
            panic!("sar 互換として解釈されるべき");
        };
        assert_eq!(opts.immediate, Some(SarImmediate::Help));
    }

    #[test]
    fn day_offset_only_argument_is_sar() {
        let Invocation::Sar(opts) = dispatch(&argv(&["-3"])).unwrap() else {
            panic!("sar 互換として解釈されるべき");
        };
        assert_eq!(opts.day_offset, 3);
    }

    #[test]
    fn sar_errors_propagate() {
        let err = dispatch(&argv(&["-Q"])).unwrap_err();
        assert!(matches!(
            err,
            CliError::Sar(SarArgError::UnknownShortOption { ch: 'Q', .. })
        ));
    }

    // ---------------------------------------------------------------
    // sadf 入口
    // ---------------------------------------------------------------

    #[test]
    fn sadf_subcommand() {
        let Invocation::Sadf(opts) = dispatch(&argv(&["sadf", "-j", "sa01"])).unwrap() else {
            panic!("sadf 互換として解釈されるべき");
        };
        assert_eq!(opts.format, Some(SadfFormat::Json));
        assert_eq!(
            opts.data_file.as_deref(),
            Some(std::path::Path::new("sa01"))
        );
    }

    #[test]
    fn sadf_dash_dash_reaches_the_sar_layer() {
        let Invocation::Sadf(opts) = dispatch(&argv(&["sadf", "-d", "sa01", "--", "-A"])).unwrap()
        else {
            panic!("sadf 互換として解釈されるべき");
        };
        assert_eq!(opts.sar.activities.len(), 43);
    }

    #[test]
    fn sadf_errors_propagate() {
        let err = dispatch(&argv(&["sadf", "-d", "-j", "sa01"])).unwrap_err();
        assert!(matches!(
            err,
            CliError::Sadf(SadfArgError::MultipleFormats { .. })
        ));
    }

    // ---------------------------------------------------------------
    // 独自サブコマンド
    // ---------------------------------------------------------------

    #[test]
    fn show_subcommand() {
        let Invocation::Native(cmd) =
            dispatch(&argv(&["show", "sa01", "sa02", "--activity", "cpu,disk"])).unwrap()
        else {
            panic!("独自サブコマンドとして解釈されるべき");
        };
        let Commands::Show(args) = *cmd else {
            panic!("show が選ばれるべき");
        };
        assert_eq!(
            args.files,
            vec![PathBuf::from("sa01"), PathBuf::from("sa02")]
        );
        assert_eq!(args.common.activity, vec!["cpu", "disk"]);
        assert_eq!(args.common.format, OutputFormat::Table);
        assert_eq!(args.values, ValueKind::Derived);
    }

    #[test]
    fn show_with_ndjson_and_both_values() {
        let Invocation::Native(cmd) = dispatch(&argv(&[
            "show", "sa01", "--format", "ndjson", "--values", "both",
        ]))
        .unwrap() else {
            panic!();
        };
        let Commands::Show(args) = *cmd else { panic!() };
        assert_eq!(args.common.format, OutputFormat::Ndjson);
        assert_eq!(args.values, ValueKind::Both);
    }

    #[test]
    fn summarize_subcommand() {
        let Invocation::Native(cmd) =
            dispatch(&argv(&["summarize", "sa01", "sa02", "--format", "json"])).unwrap()
        else {
            panic!();
        };
        let Commands::Summarize(args) = *cmd else {
            panic!("summarize が選ばれるべき");
        };
        assert_eq!(args.files.len(), 2);
        assert_eq!(args.common.format, OutputFormat::Json);
    }

    #[test]
    fn detect_subcommand() {
        let Invocation::Native(cmd) = dispatch(&argv(&["detect", "sa01", "sa02"])).unwrap() else {
            panic!("独自サブコマンドとして解釈されるべき");
        };
        let Commands::Detect(args) = *cmd else {
            panic!("detect が選ばれるべき");
        };
        assert_eq!(args.files.len(), 2);
        // 既定は text / 入力全体 / 下限なし / 要約
        assert_eq!(args.format, DetectFormat::Text);
        assert_eq!(args.baseline_scope, BaselineScopeArg::Input);
        assert_eq!(args.min_priority, PriorityArg::Informational);
        assert!(!args.verbose);
    }

    #[test]
    fn detect_accepts_the_documented_options() {
        let Invocation::Native(cmd) = dispatch(&argv(&[
            "detect",
            "sa01",
            "--format",
            "json",
            "--baseline-scope",
            "window",
            "--min-priority",
            "investigate",
            "--from",
            "09:00",
            "--to",
            "18:00",
            "--activity",
            "cpu,disk",
            "--verbose",
        ]))
        .unwrap() else {
            panic!();
        };
        let Commands::Detect(args) = *cmd else {
            panic!()
        };
        assert_eq!(args.format, DetectFormat::Json);
        assert!(args.verbose);
        assert_eq!(args.baseline_scope, BaselineScopeArg::Window);
        assert_eq!(args.min_priority, PriorityArg::Investigate);
        assert_eq!(args.from.as_deref(), Some("09:00"));
        assert_eq!(args.to.as_deref(), Some("18:00"));
        assert_eq!(args.activity, vec!["cpu", "disk"]);
    }

    /// `--from` / `--to` が報告範囲だけを絞ることを help に明記する。
    #[test]
    fn detect_help_explains_the_baseline_scope_distinction() {
        let mut cmd = Cli::command();
        let sub = cmd
            .get_subcommands_mut()
            .find(|c| c.get_name() == "detect")
            .expect("detect サブコマンド");
        let rendered = sub.render_long_help().to_string();
        assert!(rendered.contains("報告範囲を絞るだけ"), "{rendered}");
        assert!(rendered.contains("基準の材料は絞らない"), "{rendered}");
        assert!(rendered.contains("外部の正常値ではない"), "{rendered}");
    }

    #[test]
    fn compare_subcommand() {
        let Invocation::Native(cmd) = dispatch(&argv(&[
            "compare",
            "--host",
            "app1=app1/sa01",
            "--host",
            "app2=app2/sa01",
        ]))
        .unwrap() else {
            panic!();
        };
        let Commands::Compare(args) = *cmd else {
            panic!("compare が選ばれるべき");
        };
        assert_eq!(
            args.hosts,
            vec![
                HostSpec {
                    name: "app1".to_string(),
                    path: PathBuf::from("app1/sa01")
                },
                HostSpec {
                    name: "app2".to_string(),
                    path: PathBuf::from("app2/sa01")
                },
            ]
        );
    }

    #[test]
    fn compare_rejects_malformed_host_spec() {
        assert!(dispatch(&argv(&["compare", "--host", "app1"])).is_err());
        assert!(dispatch(&argv(&["compare", "--host", "=sa01"])).is_err());
    }

    #[test]
    fn info_subcommand() {
        let Invocation::Native(cmd) = dispatch(&argv(&["info", "sa01"])).unwrap() else {
            panic!();
        };
        let Commands::Info(args) = *cmd else {
            panic!("info が選ばれるべき");
        };
        assert_eq!(args.files, vec![PathBuf::from("sa01")]);
        assert!(!args.no_mmap);
    }

    #[test]
    fn strict_and_lenient_conflict() {
        assert!(dispatch(&argv(&["show", "sa01", "--strict", "--lenient"])).is_err());
    }

    #[test]
    fn native_flags_skeleton() {
        let Invocation::Native(cmd) = dispatch(&argv(&[
            "show",
            "sa01",
            "--from",
            "09:00",
            "--to",
            "18:00",
            "--lenient",
            "--no-mmap",
            "--jobs",
            "4",
        ]))
        .unwrap() else {
            panic!();
        };
        let Commands::Show(args) = *cmd else { panic!() };
        assert_eq!(args.common.from.as_deref(), Some("09:00"));
        assert_eq!(args.common.to.as_deref(), Some("18:00"));
        assert!(args.common.lenient);
        assert!(args.common.no_mmap);
        assert_eq!(args.common.jobs, Some(4));
    }

    // ---------------------------------------------------------------
    // ルート専用オプション
    // ---------------------------------------------------------------

    #[test]
    fn root_help_and_version_are_handled_by_clap() {
        let err = dispatch(&argv(&["--help"])).unwrap_err();
        let CliError::Clap(err) = err else {
            panic!("clap が処理するべき");
        };
        assert_eq!(err.kind(), clap::error::ErrorKind::DisplayHelp);

        for arg in ["--version", "-V"] {
            let err = dispatch(&argv(&[arg])).unwrap_err();
            let CliError::Clap(err) = err else {
                panic!("clap が処理するべき");
            };
            assert_eq!(err.kind(), clap::error::ErrorKind::DisplayVersion);
        }
    }

    #[test]
    fn no_arguments_shows_root_help() {
        let err = dispatch(&[]).unwrap_err();
        let CliError::Clap(err) = err else {
            panic!("clap が処理するべき");
        };
        assert_eq!(
            err.kind(),
            clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
        );
    }

    #[test]
    fn root_has_no_short_help_flag() {
        // -h をルートのヘルプにすると sar の -h と衝突するので定義しない
        let rendered = Cli::command().render_help().to_string();
        assert!(rendered.contains("--help"));
        assert!(!rendered.contains("-h, --help"));
    }

    #[test]
    fn subcommand_names() {
        for name in SUBCOMMAND_NAMES {
            assert!(is_subcommand_name(name));
        }
        assert!(!is_subcommand_name("-u"));
        assert!(!is_subcommand_name("sa01"));
    }
}

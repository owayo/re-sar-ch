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

use std::path::PathBuf;

use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum};

pub use sadf_args::{
    SadfArgError, SadfFormat, SadfImmediate, SadfOptions, SadfOutputOptions, SadfTimeBase,
    SvgPalette, parse_sadf_args,
};
pub use sar_args::{
    Activity, Caller, CpuBitmap, OptFlags, PersistentName, SarArgError, SarFlags, SarImmediate,
    SarInput, SarOptions, SarOutput, TimeSpec, parse_sar_args,
};

/// ルートが受け付けるサブコマンド名。
///
/// 先頭引数がこのいずれでもなければ `sar` 互換として解釈する。
pub const SUBCOMMAND_NAMES: [&str; 8] = [
    "show",
    "summarize",
    "detect",
    "compare",
    "info",
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

/// 複数のサブコマンドで共通の引数。
#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct CommonArgs {
    /// 出力形式。
    #[arg(long, value_enum, default_value_t = OutputFormat::Table)]
    pub format: OutputFormat,

    /// 対象 activity をカンマ区切りで絞る (例: `cpu,disk`)。
    #[arg(long, value_name = "LIST", value_delimiter = ',')]
    pub activity: Vec<String>,

    /// 開始時刻 (`hh:mm[:ss]` または 10 桁の epoch 秒)。
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
    /// 人が読む形式 (既定)。
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

    /// 報告範囲の開始時刻。**基準の材料は絞らない** (`--baseline-scope` を参照)。
    #[arg(long, value_name = "TIME")]
    pub from: Option<String>,

    /// 報告範囲の終了時刻。**基準の材料は絞らない** (`--baseline-scope` を参照)。
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
    ///
    /// 自動生成の `--help` を止めて [`DetectArgs`] 側で定義する
    /// (ルートと同じ作法。理由は [`DetectArgs::help`] を参照)。
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
        Some("sar") => Ok(Invocation::Sar(Box::new(parse_sar_args(&argv[1..])?))),
        Some("sadf") => Ok(Invocation::Sadf(Box::new(parse_sadf_args(&argv[1..])?))),
        Some(first) if is_subcommand_name(first) => {
            Ok(Invocation::Native(Box::new(parse_root(argv)?)))
        }
        // 先頭がサブコマンド名でない (`-` 始まりを含む) → sar 互換として解釈。
        Some(_) => Ok(Invocation::Sar(Box::new(parse_sar_args(argv)?))),
    }
}

/// `std::env::args()` から解釈する。
pub fn dispatch_from_env() -> Result<Invocation, CliError> {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    dispatch(&argv)
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
        // 既定は text / 入力全体 / 下限なし
        assert_eq!(args.format, DetectFormat::Text);
        assert_eq!(args.baseline_scope, BaselineScopeArg::Input);
        assert_eq!(args.min_priority, PriorityArg::Informational);
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
        ]))
        .unwrap() else {
            panic!();
        };
        let Commands::Detect(args) = *cmd else {
            panic!()
        };
        assert_eq!(args.format, DetectFormat::Json);
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

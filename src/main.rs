//! `resarch` — sysstat の `sa` バイナリを単体で解析する CLI。
//!
//! 引数はサブコマンド (`show` / `summarize` / `detect` / `compare` / `info`) と
//! `sar` / `sadf` 互換の 2 系統を受け付ける。サブコマンド名を省略した場合は
//! `sar` 互換として解釈するため、`resarch -u -f sa01` がそのまま動く。
//!
//! # この層の責務
//!
//! CLI 解析結果 ([`Invocation`]) を**出力層の設定へ写して呼ぶだけ**。
//! 値の計算も書式化もここではしない (`docs/design.md` §2)。
//!
//! | 変換 | 行き先 |
//! |---|---|
//! | [`SarOptions`] → [`SarTextOptions`] + activity 列 | [`sar_text::write_report`] |
//! | [`SadfOptions`] → [`SadfConfig`] | `output::sadf::*` |
//! | `show` / `summarize` / `compare` の引数 → [`CustomConfig`] / [`MultiOptions`] | 独自出力・`multi` |
//! | `detect` の引数 → [`DetectOptions`] | `analyze::assessment` → [`detect_report`] |
//!
//! # 出力先と終了コード (`docs/design.md` §7)
//!
//! - データは `stdout` ([`BufWriter`] で包み、ロックは 1 回だけ取る)、診断は `stderr`
//! - 部分結果 (読めなかったファイルを飛ばした / 途中で書き出しに失敗した) は非ゼロ終了
//! - 互換出力に独自の警告フィールドを混ぜない (診断は必ず `stderr`)

use std::collections::BTreeMap;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, bail};

use re_sar_ch::analyze::{
    Assessment, ColumnSummary, Finding, MetricKey, NativePeriodSummary, PercentileOutcome,
    Priority, RetainTimelines, SummaryOptions, Verdict, assess_summary, metric_catalog,
    rule_inputs,
};
use re_sar_ch::cli::{
    self, Activity, BaselineScopeArg, CliError, Commands, CommonArgs, CompareArgs, DetectArgs,
    DetectFormat, InfoArgs, Invocation, OptFlags, OutputFormat, PriorityArg, Sa2SarArgs,
    SadfFormat, SadfImmediate, SadfOptions, SadfTimeBase, SarFlags, SarImmediate, SarInput,
    SarOptions, SarOutput, ShowArgs, SkillArgs, SummarizeArgs, TimeSpec, ValueKind,
};
use re_sar_ch::convert::{self, ConvertOptions, ConvertReport};
use re_sar_ch::detect::{BaselineScope, DetectOptions, ReportBound};
use re_sar_ch::format::{MmapPolicy, OpenOptions, SaFile, Tolerance};
use re_sar_ch::model::{ActivityId, KNOWN_ACTIVITIES};
use re_sar_ch::multi::{self, BootSegment, FileErrorPolicy, GaugeFill, MultiOptions};
use re_sar_ch::output::json::{CustomConfig, ValueScope};
use re_sar_ch::output::sadf::{self, SadfConfig, SectionConfig, TimeBase};
use re_sar_ch::output::sar_text::{self, CpuSelection, SampleSelect, SarTextOptions, TimeStyle};
use re_sar_ch::output::time_filter::{CrossDayRule, TimeBasis, TimeBound, TimeFilter};
use re_sar_ch::output::{csv, ndjson, table};
use re_sar_ch::output::{detect_report, detect_svg};
use re_sar_ch::series::Selection;

/// 既定の日次データファイルを置くディレクトリ (`SA_DIR`)。
///
/// 本家はビルド時に `configure --with-sa-dir=` で決める。ここでは同名の
/// 環境変数で上書きできるようにし、未設定なら本家の既定値を使う。
const DEFAULT_SA_DIR: &str = "/var/log/sa";

/// ホスト比較の既定の区間長 (秒)。
const COMPARE_STEP_SECS: u64 = 60;

fn main() -> ExitCode {
    let invocation = match cli::dispatch_from_env() {
        Ok(inv) => inv,
        // `--help` / `--version` の正常表示もここに来るので clap に任せる
        Err(CliError::Clap(e)) => e.exit(),
        Err(e) => {
            eprintln!("resarch: {e}");
            return ExitCode::from(1);
        }
    };

    match run(invocation) {
        Ok(code) => code,
        Err(e) => {
            // データは stdout、診断は stderr
            eprintln!("resarch: {e}");
            for cause in e.chain().skip(1) {
                eprintln!("  原因: {cause}");
            }
            ExitCode::from(1)
        }
    }
}

fn run(invocation: Invocation) -> anyhow::Result<ExitCode> {
    match invocation {
        Invocation::Native(cmd) => match *cmd {
            Commands::Sa2Sar(args) => run_sa2sar(args),
            Commands::Info(args) => run_info(args),
            Commands::SkillInstall(args) => run_skill_install(args),
            Commands::Show(args) => run_show(args),
            Commands::Summarize(args) => run_summarize(args),
            Commands::Detect(args) => run_detect(args),
            Commands::Compare(args) => run_compare(args),
            // `dispatch` は互換入口を直接互換パーサへ回すので通常ここには来ない。
            // ルート経由で来た場合も同じ結果になるよう解析し直す。
            Commands::Sar(args) => run_sar(cli::parse_sar_args(&args.args)?),
            Commands::Sadf(args) => run_sadf(cli::parse_sadf_args(&args.args)?),
        },
        Invocation::Sar(opts) => run_sar(*opts),
        Invocation::Sadf(opts) => run_sadf(*opts),
    }
}

// ===========================================================================
// 共通ヘルパ
// ===========================================================================

/// `stdout` を 1 回だけロックして `BufWriter` で包む (`docs/design.md` §6.4)。
fn stdout_writer() -> BufWriter<io::StdoutLock<'static>> {
    BufWriter::new(io::stdout().lock())
}

fn open_options(lenient: bool, no_mmap: bool) -> OpenOptions {
    OpenOptions {
        mmap: if no_mmap {
            MmapPolicy::Never
        } else {
            MmapPolicy::Auto
        },
        tolerance: if lenient {
            Tolerance::Lenient
        } else {
            Tolerance::Strict
        },
        ..Default::default()
    }
}

fn open_file(path: &Path, options: &OpenOptions) -> anyhow::Result<SaFile> {
    SaFile::open_with(path, options.clone()).map_err(anyhow::Error::new)
}

/// 致命的でない問題を `stderr` へ出す。**stdout には混ぜない。**
fn report_diagnostics(file: &SaFile) {
    for d in file.diagnostics() {
        match d.offset {
            Some(off) => eprintln!(
                "resarch: 診断 ({}+{off}): {}",
                file.path().display(),
                d.message
            ),
            None => eprintln!("resarch: 診断 ({}): {}", file.path().display(), d.message),
        }
    }
}

fn exit_code(partial: bool) -> ExitCode {
    if partial {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

// ===========================================================================
// `-f` のファイル解決 (`docs/format/03-output-format.md` §5.2)
// ===========================================================================

/// `SA_DIR` (既定 `/var/log/sa`)。
fn sa_dir() -> PathBuf {
    match std::env::var_os("SA_DIR") {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => PathBuf::from(DEFAULT_SA_DIR),
    }
}

/// `day_offset` 日前の日付。
///
/// 本家の `get_time(&rectime, N)` はローカル時刻基準だが、
/// `S_TIME_DEF_TIME=UTC` のときは UTC 基準になる。
fn target_date(day_offset: u32) -> (i32, u32, u32) {
    use chrono::{Datelike, Duration, Local, Utc};
    let back = Duration::days(i64::from(day_offset));
    if std::env::var("S_TIME_DEF_TIME").as_deref() == Ok("UTC") {
        let d = (Utc::now() - back).date_naive();
        (d.year(), d.month(), d.day())
    } else {
        let d = (Local::now() - back).date_naive();
        (d.year(), d.month(), d.day())
    }
}

/// `guess_sa_name()` 相当。
///
/// `saYYYYMMDD` と `saDD` の **mtime (秒 + nsec)** を比べて新しい方を使う。
/// 片方しか `stat()` できなければそれを、どちらも無ければ `saDD` を返す。
fn guess_sa_name(dir: &Path, day_offset: u32) -> PathBuf {
    let (y, m, d) = target_date(day_offset);
    let short = dir.join(format!("sa{d:02}"));
    let long = dir.join(format!("sa{y:04}{m:02}{d:02}"));

    let mtime = |p: &Path| {
        std::fs::metadata(p)
            .and_then(|md| md.modified())
            .ok()
            .map(|t| t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default())
    };
    match (mtime(&long), mtime(&short)) {
        (Some(a), Some(b)) => {
            if a > b {
                long
            } else {
                short
            }
        }
        (Some(_), None) => long,
        (None, Some(_)) => short,
        (None, None) => short,
    }
}

/// `-f` / `sadf <file>` の指定を実際のパスへ解決する。
///
/// - 未指定 (`SarInput::DefaultDaily`) → `SA_DIR` 配下を [`guess_sa_name`] で推測
/// - ディレクトリ指定 → `check_alt_sa_dir()` 相当。その下に日次ファイル名を付加
/// - ファイル指定 → そのまま
fn resolve_daily(input: Option<&Path>, day_offset: u32, dir_offset: u32) -> PathBuf {
    match input {
        None => guess_sa_name(&sa_dir(), day_offset),
        Some(p) if p.is_dir() => guess_sa_name(p, dir_offset),
        Some(p) => p.to_path_buf(),
    }
}

/// `sar` の読み出し元を解決する。`-o` は採取なので呼び出し側が先に弾く。
fn resolve_sar_input(opts: &SarOptions) -> anyhow::Result<PathBuf> {
    let path = match &opts.input {
        Some(SarInput::DefaultDaily) | None => resolve_daily(None, opts.day_offset, 0),
        // `-f <dir>` はディレクトリ指定でも `day_offset` が効く
        Some(SarInput::File(p)) => resolve_daily(Some(p), opts.day_offset, opts.day_offset),
    };
    check_readable(&path, opts.default_file_used)?;
    Ok(path)
}

/// `sadf` の読み出し元を解決する。
///
/// positional のディレクトリ指定では `check_alt_sa_dir(dfile, 0, -1)` が呼ばれる
/// ため、**`-N` は効かない** (§2.6)。
fn resolve_sadf_input(opts: &SadfOptions) -> anyhow::Result<PathBuf> {
    let path = resolve_daily(opts.data_file.as_deref(), opts.day_offset, 0);
    check_readable(&path, opts.default_file_used)?;
    Ok(path)
}

/// 既定ファイルへフォールバックしたのに開けない場合は本家と同じヒントを添える。
fn check_readable(path: &Path, default_file_used: bool) -> anyhow::Result<()> {
    if path.exists() {
        return Ok(());
    }
    if default_file_used {
        bail!(
            "{}: 開けません (Please check if data collecting is enabled)",
            path.display()
        );
    }
    bail!("{}: 開けません", path.display());
}

// ===========================================================================
// SarOptions → 出力層の設定
// ===========================================================================

fn activity_id(act: Activity) -> ActivityId {
    ActivityId(u32::from(act.id()))
}

/// タイムスタンプの基準系 (`sar` テキスト用)。
fn time_style(flags: &SarFlags) -> TimeStyle {
    if flags.sec_epoch {
        TimeStyle::Epoch
    } else if flags.true_time {
        TimeStyle::Recorded
    } else if flags.local_time {
        TimeStyle::Local
    } else {
        TimeStyle::Utc
    }
}

/// `-s` / `-e` の比較に使う基準系 (§1.11)。
///
/// `-U` (epoch 表示) でも `hh:mm:ss` 形式の境界は UTC と比べる。
fn time_basis(flags: &SarFlags) -> TimeBasis {
    if flags.true_time {
        TimeBasis::Recorded
    } else if flags.local_time {
        TimeBasis::Local
    } else {
        TimeBasis::Utc
    }
}

fn time_bound(spec: TimeSpec) -> TimeBound {
    match spec {
        TimeSpec::None => TimeBound::None,
        TimeSpec::HhMmSs { hour, min, sec } => TimeBound::HhMmSs { hour, min, sec },
        TimeSpec::Epoch(e) => TimeBound::Epoch(e),
    }
}

/// `-s` / `-e` をフィルタへ写す。日跨ぎ補正は解析側 (`check_time_limits`) で済んでいる。
fn sar_time_filter(opts: &SarOptions, cross_day: CrossDayRule) -> TimeFilter {
    TimeFilter {
        start: time_bound(opts.tm_start),
        end: time_bound(opts.tm_end),
        basis: time_basis(&opts.flags),
        cross_day,
    }
}

/// `-P` のビットマップを [`CpuSelection`] へ写す。
///
/// bit 0 = 集約行 (`all`)、CPU `n` = bit `n + 1`。`-P ALL` / `-A` は全ビットが立つ。
fn cpu_selection(opts: &SarOptions) -> CpuSelection {
    let b = &opts.cpu_bitmap;
    if b.count_bits() == b.capacity_bits() {
        return CpuSelection::All;
    }
    if b.aggregate_selected() && b.count_bits() == 1 {
        return CpuSelection::Aggregate;
    }
    CpuSelection::Listed {
        aggregate: b.aggregate_selected(),
        cpus: b.selected_cpus().collect(),
    }
}

/// `--dev=` / `--iface=` / `--fs=` / `--int=` / `-I SUM` のアイテム名フィルタ。
///
/// `--int=` (`A_IRQ`) は行 = 割り込みなので、絞るのは行で、
/// 列 (CPU) は `-P` のビットマップの担当 (03 §8.6)。
fn sar_item_names(opts: &SarOptions) -> BTreeMap<ActivityId, Vec<String>> {
    let mut map = BTreeMap::new();
    for act in [
        Activity::Disk,
        Activity::NetDev,
        Activity::NetEdev,
        Activity::Fs,
        Activity::Irq,
    ] {
        let list = opts.item_list(act);
        if !list.is_empty() {
            map.insert(activity_id(act), list.to_vec());
        }
    }
    map
}

/// `-i <interval>` と positional の `interval` / `count` を出力層へ写す。
///
/// 本家はファイル読み出しに入る直前で `interval < 0` を 1 に補正する (03 §5.3)。
/// `interval == 0` は `-f` / `-o` 併用として引数解析が弾いているのでここには来ない。
fn sar_sample_select(opts: &SarOptions) -> SampleSelect {
    SampleSelect {
        interval: opts.interval.unwrap_or(1).max(1),
        count: opts.count,
    }
}

/// [`SarOptions`] を `sar` テキスト出力の設定へ写す。
fn sar_text_options(opts: &SarOptions) -> SarTextOptions {
    let mem = opts.opt_flags(Activity::Memory);
    SarTextOptions {
        pretty: opts.flags.pretty,
        human: opts.flags.human,
        dec_places: opts.dec_places,
        comment: opts.flags.comment,
        minmax: opts.flags.minmax,
        zero_omit: opts.flags.zero_omit,
        cpu_all: opts.opt_flags(Activity::Cpu).contains(OptFlags::CPU_ALL),
        memory: mem.contains(OptFlags::MEMORY),
        mem_all: mem.contains(OptFlags::MEM_ALL),
        swap: mem.contains(OptFlags::SWAP),
        mount: opts.opt_flags(Activity::Fs).contains(OptFlags::MOUNT),
        dev_sid: opts.flags.dev_sid,
        time: time_style(&opts.flags),
        cpus: cpu_selection(opts),
        time_filter: sar_time_filter(opts, CrossDayRule::Sar),
        item_names: sar_item_names(opts),
    }
}

/// 選択された activity を**ファイル記載順**で返す (`id_seq[]` 相当)。
///
/// 本家のファイル読み出しモードは `act[]` 配列順ではなくデータファイルの
/// activity リスト順で出力する (03 §11-19)。
fn sar_activities(opts: &SarOptions, file: &SaFile) -> Vec<ActivityId> {
    let selected: Vec<ActivityId> = opts.selected_activities().map(activity_id).collect();
    sar_text::activities_in_file(file)
        .into_iter()
        .filter(|id| selected.contains(id))
        .collect()
}

/// `-u ALL` / `-r ALL` / `-F MOUNT` などのセクション選択 (`sadf` 用)。
fn section_config(opts: &SarOptions) -> SectionConfig {
    let mem = opts.opt_flags(Activity::Memory);
    SectionConfig {
        cpu_all: opts.opt_flags(Activity::Cpu).contains(OptFlags::CPU_ALL),
        memory: mem.contains(OptFlags::MEMORY),
        swap: mem.contains(OptFlags::SWAP),
        mem_all: mem.contains(OptFlags::MEM_ALL),
        fs_mount: opts.opt_flags(Activity::Fs).contains(OptFlags::MOUNT),
    }
}

fn sadf_config(opts: &SadfOptions) -> SadfConfig {
    SadfConfig {
        time_base: match opts.time_base() {
            SadfTimeBase::Utc => TimeBase::Utc,
            SadfTimeBase::LocalTime => TimeBase::LocalTime,
            SadfTimeBase::TrueTime => TimeBase::TrueTime,
            SadfTimeBase::SecEpoch => TimeBase::SecEpoch,
        },
        comments: opts.sar.flags.comment,
        debug: opts.output.debug,
        horizontally: opts.horizontally,
        section: section_config(&opts.sar),
        activities: Some(opts.sar.selected_activities().map(activity_id).collect()),
        time_filter: sar_time_filter(&opts.sar, CrossDayRule::Sadf),
        cpus: cpu_selection(&opts.sar),
        item_names: sar_item_names(&opts.sar),
    }
}

// ===========================================================================
// `sar` 互換入口
// ===========================================================================

fn run_sa2sar(args: Sa2SarArgs) -> anyhow::Result<ExitCode> {
    let file = open_file(&args.file, &open_options(false, args.no_mmap))?;
    report_diagnostics(&file);
    // -A の CPU / memory / swap などの選択規則を互換入口と共有する。
    let mut opts = cli::parse_sar_args(&["-A".into(), "-C".into()])?;
    opts.flags.true_time = !args.utc;
    opts.flags.local_time = false;
    let text = sar_text_options(&opts);
    let activities = sar_activities(&opts, &file);
    if activities.is_empty() {
        bail!(
            "Requested activities not available in file {}",
            args.file.display()
        );
    }
    if let Some(path) = args.output.as_deref().filter(|p| *p != Path::new("-")) {
        // 同一ディレクトリの一時ファイルへストリーミングし、成功後に公開する。
        // persist_noclobber は入力自身・hardlink・symlink も上書きしない。
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let mut temp = tempfile::NamedTempFile::new_in(parent)
            .with_context(|| format!("出力先に一時ファイルを作成できません: {}", path.display()))?;
        {
            let mut out = BufWriter::new(temp.as_file_mut());
            sar_text::write_report(&mut out, &file, &text, &activities)?;
            out.flush()?;
        }
        temp.persist_noclobber(path).with_context(|| {
            format!(
                "sar テキストを保存できません (既存ファイルは上書きしません): {}",
                path.display()
            )
        })?;
    } else {
        let mut out = stdout_writer();
        let result = sar_text::write_report(&mut out, &file, &text, &activities);
        out.flush()?;
        result?;
    }
    Ok(ExitCode::SUCCESS)
}

fn run_sar(opts: SarOptions) -> anyhow::Result<ExitCode> {
    if let Some(immediate) = opts.immediate {
        return run_sar_immediate(immediate);
    }
    // `-o` は採取 (`sadc` 相当)。reSARch はファイル解析専用 (`docs/design.md` §9.1)。
    if let Some(output) = &opts.output {
        let name = match output {
            SarOutput::DefaultDaily => "標準の日次データファイル".to_string(),
            SarOutput::File(p) => p.display().to_string(),
        };
        bail!(
            "-o ({name}): reSARch は統計の採取を行いません (sa ファイルの解析専用です)。\
             採取は sysstat の sadc / sar -o を使ってください"
        );
    }
    // ライブ採取は行わないので、interval だけを渡された場合も読み出し元が必要。
    if opts.input.is_none() {
        bail!(
            "読み出す sa ファイルがありません。reSARch はライブ採取を行わないので \
             `-f <file>` を指定してください"
        );
    }

    let path = resolve_sar_input(&opts)?;
    let options = OpenOptions::default();
    let file = open_file(&path, &options)?;
    report_diagnostics(&file);

    let text = sar_text_options(&opts);
    let activities = sar_activities(&opts, &file);
    if activities.is_empty() {
        bail!(
            "Requested activities not available in file {}",
            path.display()
        );
    }

    let mut out = stdout_writer();
    let result = sar_text::write_report_with(
        &mut out,
        &file,
        &text,
        &activities,
        sar_sample_select(&opts),
    );
    out.flush()?;
    result?;
    Ok(ExitCode::SUCCESS)
}

fn run_sar_immediate(immediate: SarImmediate) -> anyhow::Result<ExitCode> {
    match immediate {
        SarImmediate::Help => {
            let mut out = stdout_writer();
            write!(out, "{}", SAR_USAGE)?;
            out.flush()?;
            Ok(ExitCode::SUCCESS)
        }
        SarImmediate::Version => {
            let mut out = stdout_writer();
            writeln!(out, "resarch version {}", env!("CARGO_PKG_VERSION"))?;
            writeln!(
                out,
                "sysstat の sa ファイルを sar / sadf に依存せず解析する (採取は行わない)"
            )?;
            out.flush()?;
            Ok(ExitCode::SUCCESS)
        }
        // データコレクタ (`sadc`) を持たないので所在を答えられない。
        SarImmediate::Sadc => bail!(
            "--sadc: reSARch はデータコレクタを持ちません (採取は sysstat の sadc を使ってください)"
        ),
    }
}

const SAR_USAGE: &str = "\
使い方: resarch [sar] [オプション...] [-f <sa ファイル>]

reSARch の sar 互換入口。ファイル解析専用で、採取 (-o) は行わない。

activity の選択:
  -A                すべての activity
  -u [ALL]          CPU        -w   タスク生成 / コンテキストスイッチ
  -B                ページング  -b   I/O 転送レート
  -r [ALL] / -S     メモリ / スワップ    -v   カーネルテーブル
  -q [キーワード]    負荷 / PSI   -y   TTY
  -d                ブロックデバイス      -F [MOUNT]  ファイルシステム
  -n <キーワード>    ネットワーク          -m <キーワード>  電源管理
  -I [SUM|ALL]      割り込み    -H   hugepages    -W   スワッピング

絞り込みと書式:
  -P {<cpulist>|ALL}   CPU 別統計 (ALL は全 CPU、all は集約行のみ)
  -s [hh:mm[:ss]]      開始時刻 (省略時 08:00:00 / 10 桁なら epoch 秒)
  -e [hh:mm[:ss]]      終了時刻 (省略時 18:00:00 / 10 桁なら epoch 秒)
  --dev= / --iface= / --fs=   アイテム名で絞る
  -p / --pretty        アイテム名を行末へ移す
  -h                   --pretty --human
  --human              単位付き表示
  --dec={0|1|2}        小数桁 (幅は変わらない)
  -t                   記録時のローカル時刻で表示
  -z                   前サンプルと同一の行を省略
  -C                   COM 行を表示
  -j {SID|<type>}      永続デバイス名

その他:
  -f [<file>]     読み出し元 (ディレクトリなら日次ファイル名を付加)
  -[0-9]+         何日前の日次ファイルか
  --help / -V     このヘルプ / 版の表示
";

// ===========================================================================
// `sadf` 互換入口
// ===========================================================================

fn run_sadf(opts: SadfOptions) -> anyhow::Result<ExitCode> {
    if opts.immediate == Some(SadfImmediate::Version) {
        let mut out = stdout_writer();
        writeln!(out, "resarch version {}", env!("CARGO_PKG_VERSION"))?;
        out.flush()?;
        return Ok(ExitCode::SUCCESS);
    }

    // `finalize` 後は必ず Some だが、念のため本家の既定 (`-p`) に落とす。
    let format = opts.format.unwrap_or(SadfFormat::Ppc);
    let path = resolve_sadf_input(&opts)?;
    let file = open_file(&path, &OpenOptions::default())?;
    report_diagnostics(&file);

    // `-H` も `-t` を見るので、形式の分岐より前に設定を作る
    // (本家の `get_file_timestamp_struct()` は `PRINT_TRUE_TIME` を全形式で参照する)。
    let cfg = sadf_config(&opts);

    let mut out = stdout_writer();
    if format == SadfFormat::Header {
        sadf::header::write_header_with(&mut out, &file, &cfg)?;
        out.flush()?;
        return Ok(ExitCode::SUCCESS);
    }
    if opts.header_only {
        bail!(
            "-H は他の形式との併用に未対応です (ヘッダのみを見るには `resarch sadf -H <file>` を使ってください)"
        );
    }

    if !matches!(format, SadfFormat::Conv | SadfFormat::Pcp)
        && sadf::dbppc::selected_specs(&file, &cfg).is_empty()
    {
        bail!(
            "Requested activities not available in file {}",
            path.display()
        );
    }

    let result = match format {
        SadfFormat::Db => sadf::dbppc::write_db(&mut out, &file, &cfg),
        SadfFormat::Ppc => sadf::dbppc::write_ppc(&mut out, &file, &cfg),
        SadfFormat::Json => sadf::json::write_json(&mut out, &file, &cfg),
        SadfFormat::Xml => sadf::xml::write_xml(&mut out, &file, &cfg),
        SadfFormat::Raw => sadf::raw::write_raw(&mut out, &file, &cfg),
        SadfFormat::Conv => {
            // 変換後のバイナリは stdout のみ、進捗は stderr (01 §5.1)。
            let cvt = ConvertOptions {
                hz: opts.output.user_hz.map(u64::from),
            };
            convert::convert(&file, &cvt, &mut out).map(|r| report_conversion(&r))
        }
        SadfFormat::Svg => re_sar_ch::output::svg::write_svg(&mut out, &file, &cfg, &opts.output),
        SadfFormat::Pcp => bail!("-l (PCP アーカイブ) は未対応です"),
        // 上で処理済み
        SadfFormat::Header => Ok(()),
    };
    out.flush()?;
    result?;
    Ok(ExitCode::SUCCESS)
}

/// 世代変換の結果を stderr へ報告する。
///
/// 変換後のバイナリは stdout に出るので、**進捗や警告を混ぜてはいけない**
/// (本家も進捗はすべて stderr、01 §5.1)。
fn report_conversion(r: &ConvertReport) {
    if r.already_current {
        eprintln!("File format already up-to-date (何も出力していません)");
        return;
    }
    eprintln!(
        "{} バイトを書き出しました (activity {} 種 / レコード {} 件 / HZ {} — {})",
        r.bytes_written,
        r.activities,
        r.total_records(),
        r.hz,
        r.hz_source.describe()
    );
    if r.truncated_values > 0 {
        eprintln!(
            "警告: {} 個の値が出力側の幅に収まらず切り詰められました",
            r.truncated_values
        );
    }
    for w in &r.warnings {
        eprintln!("警告: {w}");
    }
}

// ===========================================================================
// 独自サブコマンド共通
// ===========================================================================

/// `--activity cpu,disk` を [`Selection`] へ写す。
fn selection_from(names: &[String]) -> anyhow::Result<Selection> {
    if names.is_empty() {
        return Ok(Selection::All);
    }
    let mut ids = Vec::with_capacity(names.len());
    for name in names {
        let id = parse_activity_name(name)?;
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    Ok(Selection::Only(ids))
}

/// activity 名 (`cpu` / `A_CPU` / `net_dev`) を ID へ写す。
fn parse_activity_name(name: &str) -> anyhow::Result<ActivityId> {
    let upper = name.trim().to_ascii_uppercase().replace(['-', ' '], "_");
    let wanted = match upper.strip_prefix("A_").unwrap_or(&upper) {
        // よく使う短縮名
        "MEM" => "MEMORY",
        "NET" => "NET_DEV",
        "IRQ" | "INT" => "IRQ",
        other => other,
    };
    for id in KNOWN_ACTIVITIES {
        let Some(symbol) = id.symbol() else { continue };
        if symbol.strip_prefix("A_").unwrap_or(symbol) == wanted {
            return Ok(*id);
        }
    }
    bail!(
        "--activity {name}: 未知の activity です (例: cpu, memory, disk, net_dev, fs。\
         `resarch info <file>` でファイルに入っているものが分かります)"
    )
}

/// `--from` / `--to` を解釈する。
///
/// 受け付ける形は `sar -s` / `-e` と同じ (`hh:mm` / `hh:mm:ss` / 10 桁 epoch)。
/// 比較は独自出力が表示する時刻 (UTC / epoch) に合わせる。
fn parse_time_arg(opt: &str, value: &str) -> anyhow::Result<TimeBound> {
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if value.len() == 10 && digits(value) {
        let epoch: u64 = value.parse().context("epoch 秒として解釈できません")?;
        if epoch == 0 {
            bail!("{opt} {value}: epoch 秒に 0 は指定できません");
        }
        return Ok(TimeBound::Epoch(epoch));
    }
    let parts: Vec<&str> = value.split(':').collect();
    let bad =
        || anyhow::anyhow!("{opt} {value}: hh:mm[:ss] または 10 桁の epoch 秒で指定してください");
    if !(parts.len() == 2 || parts.len() == 3) {
        return Err(bad());
    }
    let mut hms = [0u8; 3];
    for (i, p) in parts.iter().enumerate() {
        if p.len() != 2 || !digits(p) {
            return Err(bad());
        }
        hms[i] = p.parse().map_err(|_| bad())?;
    }
    if hms[0] > 23 || hms[1] > 59 || hms[2] > 59 {
        return Err(bad());
    }
    Ok(TimeBound::HhMmSs {
        hour: hms[0],
        min: hms[1],
        sec: hms[2],
    })
}

/// `--from` / `--to` を独自出力のフィルタへ写す。
///
/// `check_time_limits()` と同じ日跨ぎ補正を入れる
/// (`hh:mm:ss` 形式で `--to` < `--from` なら翌日まで)。
fn custom_time_filter(common: &CommonArgs) -> anyhow::Result<TimeFilter> {
    let start = match &common.from {
        Some(v) => parse_time_arg("--from", v)?,
        None => TimeBound::None,
    };
    let mut end = match &common.to {
        Some(v) => parse_time_arg("--to", v)?,
        None => TimeBound::None,
    };
    match (start, end) {
        (
            TimeBound::HhMmSs {
                hour: sh,
                min: sm,
                sec: ss,
            },
            TimeBound::HhMmSs { hour: eh, min, sec },
        ) if (eh, min, sec) < (sh, sm, ss) => {
            end = TimeBound::HhMmSs {
                hour: eh + 24,
                min,
                sec,
            };
        }
        (TimeBound::Epoch(s), TimeBound::Epoch(e)) if e < s => {
            bail!("--to は --from より後の時刻を指定してください");
        }
        _ => {}
    }
    Ok(TimeFilter {
        start,
        end,
        // 独自出力は UTC / epoch で時刻を出すので、比較も UTC で行う。
        basis: TimeBasis::Utc,
        cross_day: CrossDayRule::Sar,
    })
}

fn value_scope(kind: ValueKind) -> ValueScope {
    match kind {
        ValueKind::Raw => ValueScope::Raw,
        ValueKind::Derived => ValueScope::Rates,
        ValueKind::Both => ValueScope::Both,
    }
}

// ===========================================================================
// `resarch skill-install`
// ===========================================================================

/// AI エージェント向けのスキルをインストールする。
///
/// 書き出した場所は `stderr` へ報告する。**`stdout` には何も出さない**
/// (他のサブコマンドと同じ規約。パイプへ混ぜない)。
fn run_skill_install(args: SkillArgs) -> anyhow::Result<ExitCode> {
    let path = re_sar_ch::skill::install(&args.agent)?;
    let agent = re_sar_ch::skill::Agent::parse(&args.agent)?;
    eprintln!(
        "{} 用のスキルを書き出しました: {}",
        agent.display_name(),
        path.display()
    );
    Ok(ExitCode::SUCCESS)
}

// ===========================================================================
// `resarch show`
// ===========================================================================

fn run_show(args: ShowArgs) -> anyhow::Result<ExitCode> {
    let common = &args.common;
    if args.irq_cpus
        && !matches!(
            common.format,
            OutputFormat::Table | OutputFormat::Json | OutputFormat::Csv | OutputFormat::Ndjson
        )
    {
        bail!("--irq-cpus は独自の table / json / csv / ndjson 出力で指定してください");
    }
    let options = open_options(common.lenient, common.no_mmap);
    let selection = selection_from(&common.activity)?;
    let filter = custom_time_filter(common)?;

    // 複数ファイルはヘッダだけ先に読み、**日付 → 作成時刻 → パス**の決定的な
    // 順序へ並べ替えてから流す (`multi.rs` の方針 §6.3)。
    let paths = ordered_show_paths(&args.files, &options, common.lenient)?;
    let mut partial = paths.len() != args.files.len();

    let custom = CustomConfig {
        selection: selection.clone(),
        values: value_scope(args.values),
        time_filter: filter,
        irq_cpus: args.irq_cpus,
    };
    // 独自 JSON は 1 ファイル 1 文書なので、複数ファイルは配列で包む。
    let wrap_json = matches!(common.format, OutputFormat::Json) && paths.len() > 1;

    // CSV のヘッダ行は**先頭の 1 回だけ**出す。
    // ファイルごとに出すと 2 本目以降のヘッダがデータ行として読まれ、
    // `pandas.read_csv` や表計算ソフトで列の型が壊れる。
    // 「最初に開けたファイル」を基準にするので、先頭のファイルが開けなくても
    // ヘッダは 1 回出る (`i == 0` を条件にすると出なくなる)。
    let mut csv_header_pending = true;

    let mut out = stdout_writer();
    if wrap_json {
        out.write_all(b"[")?;
    }
    let mut written = 0usize;
    for path in paths.iter() {
        let file = match open_file(path, &options) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("resarch: {e}");
                partial = true;
                continue;
            }
        };
        report_diagnostics(&file);
        if common.lenient {
            let scan = match file.scan(|_| Ok(re_sar_ch::format::ScanControl::Continue)) {
                Ok(scan) => scan,
                Err(error) => {
                    eprintln!(
                        "resarch: {}: 読めないので飛ばした ({error})",
                        file.path().display()
                    );
                    partial = true;
                    continue;
                }
            };
            if scan.incomplete {
                eprintln!(
                    "resarch: {}: 不完全な末尾レコード (残余 {} バイト)。完全なレコードまでを出力します",
                    file.path().display(),
                    scan.trailing_bytes
                );
                partial = true;
            }
        }
        if wrap_json && written > 0 {
            out.write_all(b",")?;
        }
        let result = write_show_one(
            &mut out,
            &file,
            common.format,
            &custom,
            &selection,
            filter,
            &mut csv_header_pending,
        );
        written += 1;
        if let Err(e) = result {
            out.flush()?;
            return Err(e);
        }
    }
    if wrap_json {
        out.write_all(b"]\n")?;
    }
    out.flush()?;
    Ok(exit_code(partial))
}

/// 1 ファイルを指定形式で書き出す。
///
/// `csv_header_pending` は CSV のヘッダ行をまだ出していないか。
/// 出したら `false` にする (複数ファイルを 1 本の CSV に連結するため)。
#[allow(clippy::too_many_arguments)]
fn write_show_one<W: Write>(
    out: &mut W,
    file: &SaFile,
    format: OutputFormat,
    custom: &CustomConfig,
    selection: &Selection,
    filter: TimeFilter,
    csv_header_pending: &mut bool,
) -> anyhow::Result<()> {
    match format {
        OutputFormat::Table => table::write_table(out, file, custom)?,
        OutputFormat::Json => re_sar_ch::output::json::write_json(out, file, custom)?,
        OutputFormat::Csv => {
            csv::write_csv_with(&mut *out, file, custom, *csv_header_pending)?;
            *csv_header_pending = false;
        }
        OutputFormat::Ndjson => ndjson::write_ndjson(out, file, custom)?,
        OutputFormat::Sar => {
            let text = SarTextOptions {
                time_filter: filter,
                ..Default::default()
            };
            let activities: Vec<ActivityId> = sar_text::activities_in_file(file)
                .into_iter()
                .filter(|id| selection_includes(selection, *id))
                .collect();
            sar_text::write_report(out, file, &text, &activities)?;
        }
        OutputFormat::SadfPpc
        | OutputFormat::SadfDb
        | OutputFormat::SadfJson
        | OutputFormat::SadfXml
        | OutputFormat::SadfRaw => {
            let cfg = SadfConfig {
                activities: match selection {
                    Selection::All => None,
                    Selection::Only(ids) => Some(ids.clone()),
                },
                time_filter: filter,
                ..Default::default()
            };
            match format {
                OutputFormat::SadfPpc => sadf::dbppc::write_ppc(out, file, &cfg)?,
                OutputFormat::SadfDb => sadf::dbppc::write_db(out, file, &cfg)?,
                OutputFormat::SadfJson => sadf::json::write_json(out, file, &cfg)?,
                OutputFormat::SadfXml => sadf::xml::write_xml(out, file, &cfg)?,
                OutputFormat::SadfRaw => sadf::raw::write_raw(out, file, &cfg)?,
                _ => unreachable!("sadf 形式に限定した分岐"),
            }
        }
    }
    Ok(())
}

fn selection_includes(selection: &Selection, id: ActivityId) -> bool {
    match selection {
        Selection::All => true,
        Selection::Only(ids) => ids.contains(&id),
    }
}

/// `show` に渡されたファイルを決定的な順序へ並べる。
///
/// 1 ファイルなら並べ替える必要が無いのでヘッダを二度読まない。
/// 複数ファイルでは [`multi::outline_files`] でヘッダだけを読み、
/// 同一ホストかどうかを `stderr` へ報告する。
fn ordered_show_paths(
    files: &[PathBuf],
    options: &OpenOptions,
    lenient: bool,
) -> anyhow::Result<Vec<PathBuf>> {
    if files.len() <= 1 {
        return Ok(files.to_vec());
    }
    let mopts = MultiOptions {
        open: options.clone(),
        on_error: if lenient {
            FileErrorPolicy::Skip
        } else {
            FileErrorPolicy::Fail
        },
        ..Default::default()
    };
    let (mut outlines, skipped) = multi::outline_files(files, &mopts)?;
    for s in &skipped {
        eprintln!("resarch: {}: 読めないので飛ばした ({})", s.path, s.reason);
    }
    outlines.sort_by_key(|o| (o.year, o.month, o.day, o.header_ust_time, o.path.clone()));

    // 別ホストのファイルが混ざっていたら黙って連結しない
    let hosts: Vec<&str> = {
        let mut v: Vec<&str> = outlines
            .iter()
            .map(|o| o.identity.nodename.as_str())
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    };
    if hosts.len() > 1 {
        eprintln!(
            "resarch: 診断: 複数ホストのファイルが混ざっている ({})。\
             ホストごとに分けて実行することを推奨する",
            hosts.join(", ")
        );
    }
    Ok(outlines
        .into_iter()
        .map(|o| PathBuf::from(o.path))
        .collect())
}

// ===========================================================================
// `resarch detect`
// ===========================================================================

/// 異変検出。
///
/// `multi` で起動区間ごとのサマリを作り、その時系列に 3 経路を走らせる。
/// **起動区間をまたいだ検出はしない** (再起動の前後で水準が変わるのは当然なので、
/// それを異変として報告しない)。
fn run_detect(args: DetectArgs) -> anyhow::Result<ExitCode> {
    if let Some(dir) = &args.svg_dir {
        match std::fs::symlink_metadata(dir) {
            Ok(_) => bail!(
                "SVG 保存先は新規ディレクトリを指定してください: {}",
                dir.display()
            ),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    let opts = detect_options(&args)?;
    let mopts = detect_multi_options(&args)?;
    let analysis = multi::analyze_files(&args.files, &mopts)?;
    report_incomplete_files(&analysis);
    for s in &analysis.skipped {
        eprintln!("resarch: {}: 読めないので飛ばした ({})", s.path, s.reason);
    }

    let min = min_priority(args.min_priority);
    // 起動区間ごとに所見を作る。**区間をまたいだ検出はしない**
    // (再起動の前後で水準が変わるのは当然なので異変として報告しない)。
    let mut assessments: Vec<Assessment> = Vec::new();
    let mut charts = Vec::new();
    for host in &analysis.hosts {
        for seg in &host.segments {
            let mut assessment = assess_summary(&seg.summary, &opts);
            assessment.filter_priority(min);
            if args.svg_dir.is_some() {
                charts.extend(detect_svg::plan(
                    &seg.summary,
                    &assessment,
                    args.svg_context.unwrap_or(1800),
                ));
            }
            assessments.push(assessment);
        }
    }
    if assessments.is_empty() {
        // 統計レコードが 1 件も無かった (ヘッダだけのファイルなど)
        eprintln!("resarch: 解析できる統計レコードがありませんでした");
    }

    if let Some(dir) = &args.svg_dir {
        save_detect_graphs(dir, &charts, &assessments, &analysis)?;
        eprintln!(
            "resarch: SVG {} 件と index.json / report.json を保存: {}",
            charts.len(),
            dir.display()
        );
    }

    let mut out = stdout_writer();
    match args.format {
        // JSON は起動区間をまとめて 1 ドキュメントにする
        DetectFormat::Json => detect_report::write_json(&mut out, &assessments)?,
        DetectFormat::Ndjson => detect_report::write_ndjson(&mut out, &assessments)?,
        DetectFormat::Text => {
            for (i, a) in assessments.iter().enumerate() {
                if i > 0 {
                    writeln!(out)?;
                }
                detect_report::write_text(&mut out, a)?;
            }
        }
    }
    out.flush()?;
    Ok(exit_code(
        !analysis.skipped.is_empty() || !analysis.incomplete_files.is_empty(),
    ))
}

/// 保存用の名前。先頭の通番がホスト・起動区間・記号置換による衝突を防ぐ。
fn graph_name(index: usize, chart: &detect_svg::Chart) -> String {
    let label = format!(
        "{}-{}-{}",
        chart.series.activity_name, chart.series.item, chart.series.column
    );
    let safe: String = label
        .chars()
        .take(96)
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{:04}-{safe}.svg", index + 1)
}

fn save_detect_graphs(
    dir: &Path,
    charts: &[detect_svg::Chart],
    assessments: &[Assessment],
    analysis: &multi::MultiFileAnalysis,
) -> anyhow::Result<()> {
    let parent = dir
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let stage = tempfile::tempdir_in(parent)?;
    let mut assets = Vec::new();
    let mut entries = Vec::new();
    for (i, chart) in charts.iter().enumerate() {
        let filename = graph_name(i, chart);
        let mut temp = tempfile::NamedTempFile::new_in(stage.path())?;
        {
            let mut out = BufWriter::new(temp.as_file_mut());
            detect_svg::write_svg(&mut out, chart)?;
            out.flush()?;
        }
        entries.push(serde_json::json!({
            "file": filename, "source": chart.source, "series": chart.series,
            "unit": chart.unit, "origin": chart.origin,
            "window_start_ust": chart.window_start_ust, "window_end_ust": chart.window_end_ust,
            "context_secs": chart.context_secs, "findings": chart.findings,
            "missing_timeline": chart.missing_timeline,
        }));
        assets.push((filename, temp.into_temp_path()));
    }
    let partial = !analysis.skipped.is_empty() || !analysis.incomplete_files.is_empty();
    let manifest = serde_json::json!({
        "schema_version": re_sar_ch::model::NATIVE_SCHEMA_VERSION,
        "kind": "detect_svg_index", "status": if partial { "partial" } else { "complete" },
        "timezone": "UTC", "report": "report.json", "charts": entries,
        "skipped_files": analysis.skipped, "incomplete_files": analysis.incomplete_files,
    });
    for name in ["report.json", "index.json"] {
        let mut temp = tempfile::NamedTempFile::new_in(stage.path())?;
        {
            let mut out = BufWriter::new(temp.as_file_mut());
            if name == "report.json" {
                detect_report::write_json(&mut out, assessments)?;
            } else {
                serde_json::to_writer_pretty(&mut out, &manifest)?;
                writeln!(out)?;
            }
            out.flush()?;
        }
        assets.push((name.into(), temp.into_temp_path()));
    }
    // 全SVGと一覧の生成後に出力先を確保する。競合時も既存ディレクトリを触らない。
    std::fs::create_dir(dir)
        .with_context(|| format!("SVG 保存先を作成できません: {}", dir.display()))?;
    let mut published = Vec::new();
    for (name, temp) in assets {
        let destination = dir.join(name);
        if let Err(error) = temp.persist_noclobber(&destination) {
            for path in published {
                let _ = std::fs::remove_file(path);
            }
            let _ = std::fs::remove_dir(dir);
            return Err(error.into());
        }
        published.push(destination);
    }
    Ok(())
}

fn detect_options(args: &DetectArgs) -> anyhow::Result<DetectOptions> {
    let report_from = report_bound("--from", args.from.as_deref())?;
    let report_to = report_bound("--to", args.to.as_deref())?;
    if matches!((report_from, report_to), (ReportBound::Epoch(s), ReportBound::Epoch(e)) if e < s) {
        bail!("--to は --from より後の時刻を指定してください");
    }
    Ok(DetectOptions {
        baseline_scope: match args.baseline_scope {
            BaselineScopeArg::Input => BaselineScope::Input,
            BaselineScopeArg::Window => BaselineScope::Window,
        },
        report_from,
        report_to,
        selected_activities: match selection_from(&args.activity)? {
            Selection::All => None,
            Selection::Only(ids) => Some(ids),
        },
        ..Default::default()
    })
}

/// `--from` / `--to` を検出層の境界へ写す。
///
/// `output::time_filter::TimeBound` をそのまま渡さないのは、分析層が
/// 出力層へ依存しないようにするため。解釈の規則 (`hh:mm[:ss]` / 10 桁 epoch) は
/// 他のサブコマンドと同じ [`parse_time_arg`] を使う。
fn report_bound(opt: &str, value: Option<&str>) -> anyhow::Result<ReportBound> {
    let Some(v) = value else {
        return Ok(ReportBound::None);
    };
    Ok(match parse_time_arg(opt, v)? {
        TimeBound::None => ReportBound::None,
        TimeBound::Epoch(e) => ReportBound::Epoch(e),
        TimeBound::HhMmSs { hour, min, sec } => ReportBound::TimeOfDay { hour, min, sec },
    })
}

/// 検出のためのデコード設定。
///
/// **カタログにある activity だけを読む。** 全 activity の全列を保持すると
/// item 数の多いホスト (128 CPU・多数のデバイス) で時系列が際限なく増える。
fn detect_multi_options(args: &DetectArgs) -> anyhow::Result<MultiOptions> {
    let selection = detect_selection(&args.activity)?;
    Ok(MultiOptions {
        open: open_options(args.lenient, args.no_mmap),
        selection,
        summary: SummaryOptions {
            // 検出はカタログの系列を見るので、選んだ activity の列は全部保持する。
            // `RetainTimelines::RuleInputs` では組み込みルールの入力しか残らない。
            retain: RetainTimelines::All,
            ..Default::default()
        },
        on_error: if args.lenient {
            FileErrorPolicy::Skip
        } else {
            FileErrorPolicy::Fail
        },
        max_concurrent_files: args.jobs.unwrap_or(4).max(1),
        ..Default::default()
    })
}

/// カタログの activity と `--activity` の積を取る。
fn detect_selection(names: &[String]) -> anyhow::Result<Selection> {
    let catalog = metric_catalog::activities();
    if names.is_empty() {
        return Ok(Selection::Only(catalog));
    }
    let mut ids = Vec::with_capacity(names.len());
    for name in names {
        let id = parse_activity_name(name)?;
        if !catalog.contains(&id) {
            bail!(
                "--activity {name}: 異変検出の対象に入っていない activity です \
                 (対象は `resarch detect --help` が案内する指標カタログの activity)"
            );
        }
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    Ok(Selection::Only(ids))
}

fn min_priority(arg: PriorityArg) -> Priority {
    match arg {
        PriorityArg::Informational => Priority::Informational,
        PriorityArg::Watch => Priority::Watch,
        PriorityArg::Investigate => Priority::Investigate,
    }
}

// ===========================================================================
// `resarch summarize`
// ===========================================================================

/// 集計結果が空で、かつ時刻範囲を指定していたときに理由を `stderr` へ出す。
///
/// 「範囲外だったので集計が空」と「そもそも読めるデータが無かった」は別の話だが、
/// 出力はどちらも `segments: []` になる。**黙って空を返さない**
/// (`docs/design.md` §11 の「欠落を欠落として出す」と同じ方針)。
///
/// `-s` / `--from` に最初に一致したレコードは**前サンプルとして消費され、
/// 値には数えない** (`sar` と同じ)。したがって範囲内のレコードが 1 本しか
/// 無い場合も区間が作れず空になる。これが一番踏みやすい。
fn report_empty_window(analysis: &multi::MultiFileAnalysis, common: &CommonArgs) {
    if common.from.is_none() && common.to.is_none() {
        return;
    }
    if analysis.hosts.iter().any(|h| !h.segments.is_empty()) {
        return;
    }
    eprintln!(
        "resarch: 指定した時刻範囲に集計できる区間がありません \
         (--from に最初に一致したレコードは前サンプルとして消費されるので、\
         範囲内のレコードが 1 本だけでは区間が作れません)"
    );
}

fn run_summarize(args: SummarizeArgs) -> anyhow::Result<ExitCode> {
    let common = &args.common;
    let mopts = multi_options(common)?;
    let analysis = multi::analyze_files(&args.files, &mopts)?;
    report_incomplete_files(&analysis);
    for s in &analysis.skipped {
        eprintln!("resarch: {}: 読めないので飛ばした ({})", s.path, s.reason);
    }
    report_empty_window(&analysis, common);

    let mut out = stdout_writer();
    match common.format {
        OutputFormat::Json | OutputFormat::SadfJson => {
            serde_json::to_writer_pretty(&mut out, &analysis)?;
            writeln!(out)?;
        }
        OutputFormat::Ndjson => {
            for host in &analysis.hosts {
                for seg in &host.segments {
                    let row = serde_json::json!({
                        "schema_version": analysis.schema_version,
                        "record": "boot_segment",
                        "host": host.identity,
                        "segment": seg,
                    });
                    serde_json::to_writer(&mut out, &row)?;
                    writeln!(out)?;
                }
            }
        }
        _ => write_summarize_text(&mut out, &analysis)?,
    }
    out.flush()?;
    Ok(exit_code(
        !analysis.skipped.is_empty() || !analysis.incomplete_files.is_empty(),
    ))
}

fn report_incomplete_files(analysis: &multi::MultiFileAnalysis) {
    for file in &analysis.incomplete_files {
        eprintln!(
            "resarch: {}: 不完全な末尾レコード (残余 {} バイト)。完全なレコードまでを解析しました",
            file.path, file.trailing_bytes
        );
    }
}

fn multi_options(common: &CommonArgs) -> anyhow::Result<MultiOptions> {
    Ok(MultiOptions {
        open: open_options(common.lenient, common.no_mmap),
        selection: selection_from(&common.activity)?,
        summary: SummaryOptions {
            // ホスト比較でも使えるようルール入力の時系列は保持する
            retain: RetainTimelines::RuleInputs,
            ..Default::default()
        },
        on_error: if common.lenient {
            FileErrorPolicy::Skip
        } else {
            FileErrorPolicy::Fail
        },
        max_concurrent_files: common.jobs.unwrap_or(4).max(1),
        // `--from` / `--to` は集計期間そのものを絞る。
        // `detect` の報告範囲とは意味が違う (`MultiOptions::time_filter` の doc /
        // `docs/design.md` §9.0)。
        time_filter: custom_time_filter(common)?,
        ..Default::default()
    })
}

fn write_summarize_text<W: Write>(
    out: &mut W,
    analysis: &multi::MultiFileAnalysis,
) -> anyhow::Result<()> {
    for (hi, host) in analysis.hosts.iter().enumerate() {
        if hi > 0 {
            writeln!(out)?;
        }
        let id = &host.identity;
        writeln!(
            out,
            "host: {} ({} {} / {}, {} CPU)",
            id.nodename,
            id.sysname,
            id.release,
            id.machine,
            id.cpu_nr.map_or("?".to_string(), |n| n.to_string())
        )?;
        let files: Vec<&str> = host
            .files
            .iter()
            .filter_map(|i| analysis.files.get(*i).map(|f| f.path.as_str()))
            .collect();
        writeln!(out, "files: {}", files.join(", "))?;

        for seg in &host.segments {
            writeln!(out)?;
            write_segment_text(out, seg)?;
        }
    }
    Ok(())
}

fn write_segment_text<W: Write>(out: &mut W, seg: &BootSegment) -> anyhow::Result<()> {
    let p = &seg.summary.period;
    writeln!(
        out,
        "起動区間 {}  {} → {}  ({} サンプル / 連続 {} 区間 / 不連続 {} 区間)",
        seg.index,
        p.first_ust.map_or("-".to_string(), format_epoch),
        p.last_ust.map_or("-".to_string(), format_epoch),
        p.samples,
        p.continuous_intervals,
        p.broken_intervals
    )?;
    for b in &seg.boundaries {
        // 引き継いだかどうかを必ず残す (`multi.rs` の方針)
        let verdict = if b.decision.continuous {
            "差分を引き継いだ".to_string()
        } else {
            format!(
                "不連続 ({}、空白 {} 秒)",
                b.decision
                    .reason
                    .map_or("理由不明".to_string(), |r| format!("{r:?}")),
                b.decision.gap_secs
            )
        };
        writeln!(
            out,
            "  ファイル境界 {} → {}: {verdict}",
            b.prev_file, b.next_file
        )?;
    }

    writeln!(out, "  指標 (ルール判定に使う列)")?;
    let mut printed = false;
    for r in rule_inputs() {
        let key = r.key();
        if let Some(col) = column_of(&seg.summary, &key) {
            writeln!(out, "    {:<28} {}", key.display(), format_column(col))?;
            printed = true;
        }
    }
    if !printed {
        writeln!(out, "    (該当する列がファイルに無い)")?;
    }

    writeln!(out, "  判定 (ルール版 {})", ruleset_version(&seg.findings))?;
    if seg.findings.is_empty() {
        writeln!(out, "    (判定なし)")?;
    }
    for f in &seg.findings {
        write_finding_text(out, f)?;
    }
    Ok(())
}

fn ruleset_version(findings: &[Finding]) -> &str {
    findings
        .first()
        .map(|f| f.ruleset_version)
        .unwrap_or(re_sar_ch::analyze::RULESET_VERSION)
}

fn write_finding_text<W: Write>(out: &mut W, f: &Finding) -> anyhow::Result<()> {
    let mark = match f.verdict {
        Verdict::Observed => "!!",
        Verdict::NotObserved => "ok",
        Verdict::Undetermined => "??",
        Verdict::NotApplicable => "--",
    };
    writeln!(out, "    {mark} {:<26} {}", f.rule_id, f.title)?;
    if let Some(obs) = &f.observation {
        writeln!(out, "       観測: {obs}")?;
    }
    if let Some(reason) = f.reason {
        writeln!(out, "       理由: {reason:?}")?;
    }
    if !f.missing_metrics.is_empty() {
        writeln!(
            out,
            "       欠けている指標: {}",
            f.missing_metrics.join(", ")
        )?;
    }
    Ok(())
}

fn column_of<'a>(summary: &'a NativePeriodSummary, key: &MetricKey) -> Option<&'a ColumnSummary> {
    summary.column(key.activity, &key.item, &key.column)
}

fn format_column(col: &ColumnSummary) -> String {
    let num = |v: Option<f64>| match v {
        Some(v) => format!("{v:>10.2}"),
        None => format!("{:>10}", "-"),
    };
    let p95 = match col.p95 {
        PercentileOutcome::Computed(r) => format!("{:>10.2}", r.value),
        PercentileOutcome::Unavailable { .. } => format!("{:>10}", "-"),
    };
    format!(
        "max={} mean={} p95={} 区間={}",
        num(col.max.map(|e| e.value)),
        num(col.mean),
        p95,
        col.intervals
    )
}

fn format_epoch(ust: u64) -> String {
    use chrono::{TimeZone, Utc};
    match Utc.timestamp_opt(ust as i64, 0).single() {
        Some(dt) => dt.format("%Y-%m-%d %H:%M:%SZ").to_string(),
        None => ust.to_string(),
    }
}

// ===========================================================================
// `resarch compare`
// ===========================================================================

fn run_compare(args: CompareArgs) -> anyhow::Result<ExitCode> {
    let common = &args.common;
    let mopts = multi_options(common)?;

    // ホストごとに解析し、代表となる起動区間 (最もサンプル数の多い区間) を採る。
    struct HostEntry {
        label: String,
        identity: multi::HostIdentity,
        segment: BootSegment,
    }
    let mut entries: Vec<HostEntry> = Vec::new();
    let mut partial = false;

    for spec in &args.hosts {
        let paths = expand_host_path(&spec.path)?;
        let analysis = multi::analyze_files(&paths, &mopts)?;
        report_incomplete_files(&analysis);
        partial |= !analysis.incomplete_files.is_empty();
        for s in &analysis.skipped {
            eprintln!("resarch: {}: 読めないので飛ばした ({})", s.path, s.reason);
            partial = true;
        }
        let Some(host) = analysis.hosts.into_iter().next() else {
            eprintln!("resarch: --host {}: 統計が取れなかった", spec.name);
            partial = true;
            continue;
        };
        let Some(segment) = host
            .segments
            .into_iter()
            .max_by_key(|s| s.summary.period.samples)
        else {
            // 時刻範囲を指定しているなら、それが原因であることが多い
            // (`--from` に最初に一致したレコードは前サンプルとして消費される)。
            let hint = if common.from.is_some() || common.to.is_some() {
                " (指定した時刻範囲に集計できる区間が無いのかもしれません)"
            } else {
                ""
            };
            eprintln!("resarch: --host {}: 起動区間が無い{hint}", spec.name);
            partial = true;
            continue;
        };
        entries.push(HostEntry {
            label: spec.name.clone(),
            identity: host.identity,
            segment,
        });
    }

    if entries.len() < 2 {
        bail!(
            "比較には 2 ホスト以上の観測が必要です (--host NAME=PATH を 2 つ以上指定してください)"
        );
    }

    // 共通時間窓。片方に観測が無い区間を 0 と見なさないため、交差を先に求める。
    let ranges: Vec<(u64, u64)> = entries
        .iter()
        .filter_map(|e| {
            let p = &e.segment.summary.period;
            Some((p.first_ust?, p.last_ust?))
        })
        .collect();
    let Some(window) = multi::common_window(&ranges, COMPARE_STEP_SECS) else {
        bail!("ホスト間に重なる観測期間がありません (同じ時間帯のファイルを指定してください)");
    };

    let mut comparisons = Vec::new();
    let mut skipped_metrics = Vec::new();
    for r in rule_inputs() {
        let key = r.key();
        let mut series = Vec::new();
        let mut missing_on = Vec::new();
        for e in &entries {
            if let Some(t) = multi::timeline_of(&e.segment, &key) {
                series.push((e.label.clone(), e.identity.clone(), t));
            } else {
                missing_on.push(e.label.clone());
            }
        }
        // 全ホストで揃っていない指標は比較しない
        if series.len() != entries.len() {
            skipped_metrics.push(serde_json::json!({
                "metric": key,
                "missing_on": missing_on,
            }));
            continue;
        }
        comparisons.push(multi::compare_hosts(key, &series, window, GaugeFill::None));
    }

    let mut out = stdout_writer();
    match common.format {
        OutputFormat::Json | OutputFormat::SadfJson => {
            serde_json::to_writer_pretty(
                &mut out,
                &serde_json::json!({
                    "schema_version": re_sar_ch::output::json::SCHEMA_VERSION,
                    "comparisons": comparisons,
                    "skipped_metrics": skipped_metrics,
                }),
            )?;
            writeln!(out)?;
        }
        OutputFormat::Ndjson => {
            serde_json::to_writer(
                &mut out,
                &serde_json::json!({
                    "schema_version": re_sar_ch::output::json::SCHEMA_VERSION,
                    "record": "comparison_coverage",
                    "skipped_metrics": skipped_metrics,
                }),
            )?;
            writeln!(out)?;
            for c in &comparisons {
                serde_json::to_writer(&mut out, c)?;
                writeln!(out)?;
            }
        }
        _ => {
            writeln!(
                out,
                "共通期間 {} → {} ({} 秒区間 × {})",
                format_epoch(window.start_ust),
                format_epoch(window.end_ust),
                window.step_secs,
                window.bucket_count()
            )?;
            if comparisons.is_empty() {
                writeln!(out, "(全ホストで揃っている指標がありません)")?;
            }
            for skipped in &skipped_metrics {
                writeln!(
                    out,
                    "比較対象外: {} (観測なし: {})",
                    skipped["metric"], skipped["missing_on"]
                )?;
            }
            writeln!(out, "mean は観測できた区間値の単純平均")?;
            for c in &comparisons {
                writeln!(out)?;
                writeln!(
                    out,
                    "{}  (比較可能な区間 {}/{})",
                    c.metric.display(),
                    c.comparable_buckets,
                    window.bucket_count()
                )?;
                for h in &c.hosts {
                    let observed = h.observed_buckets();
                    let mean = bucket_mean(h);
                    writeln!(
                        out,
                        "  {:<16} mean={} 観測区間={}/{}",
                        h.label,
                        mean.map_or(format!("{:>10}", "-"), |v| format!("{v:>10.2}")),
                        observed,
                        window.bucket_count()
                    )?;
                }
            }
        }
    }
    out.flush()?;
    Ok(exit_code(partial))
}

fn bucket_mean(h: &multi::HostAlignedSeries) -> Option<f64> {
    let values: Vec<f64> = h.buckets.iter().filter_map(|b| b.value).collect();
    if values.is_empty() {
        return None;
    }
    Some(values.iter().sum::<f64>() / values.len() as f64)
}

/// `--host NAME=PATH` の `PATH` を対象ファイル列へ展開する。
///
/// ディレクトリなら中の `sa*` を名前順で拾う。
fn expand_host_path(path: &Path) -> anyhow::Result<Vec<PathBuf>> {
    if !path.is_dir() {
        return Ok(vec![path.to_path_buf()]);
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(path)
        .with_context(|| format!("{}: ディレクトリを読めません", path.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("sa") && !n.starts_with("sar"))
        })
        .collect();
    files.sort();
    if files.is_empty() {
        bail!("{}: sa ファイルが見つかりません", path.display());
    }
    Ok(files)
}

// ===========================================================================
// `resarch info`
// ===========================================================================

/// `resarch info` — ヘッダのメタデータと activity 一覧を表示する。
fn run_info(args: InfoArgs) -> anyhow::Result<ExitCode> {
    let options = open_options(args.lenient, args.no_mmap);

    let mut out = stdout_writer();
    let mut failed = false;

    for (i, path) in args.files.iter().enumerate() {
        match SaFile::open_with(path, options.clone()) {
            Ok(file) => {
                if i > 0 {
                    writeln!(out)?;
                }
                match args.format {
                    OutputFormat::Json | OutputFormat::SadfJson => {
                        write_info_json(&mut out, &file)?
                    }
                    _ => write_info_table(&mut out, &file)?,
                }
            }
            Err(e) => {
                // 1 ファイルの失敗で全体を止めず、最後に非ゼロ終了する
                eprintln!("resarch: {e}");
                failed = true;
            }
        }
    }
    out.flush()?;

    Ok(exit_code(failed))
}

fn write_info_table<W: Write>(out: &mut W, file: &SaFile) -> anyhow::Result<()> {
    let m = file.magic();
    let h = file.header();
    let enc = file.encoding();

    writeln!(out, "file          {}", file.path().display())?;
    writeln!(
        out,
        "format        0x{:04x} ({})  sysstat {}",
        m.format_magic,
        file.spec().versions,
        m.version_string()
    )?;
    writeln!(
        out,
        "encoding      {} endian, sizeof(long)={}, abi={}",
        enc.endian, h.sizeof_long, enc.abi.name
    )?;
    if let Some(up) = m.upgraded.filter(|v| *v != 0) {
        // sadf -c で変換されたファイル。値は Y*256 + Z + 1
        let y = up >> 8;
        let z = (up & 0xff).saturating_sub(1);
        writeln!(out, "upgraded      yes (変換先 x.{y}.{z} 形式)")?;
    }
    writeln!(
        out,
        "date          {:04}-{:02}-{:02}  (ust_time {})",
        h.year, h.month, h.day, h.ust_time
    )?;
    writeln!(
        out,
        "host          {} {} {}",
        h.sysname, h.release, h.machine
    )?;
    if let Some(tz) = &h.tzname {
        writeln!(out, "timezone      {tz}")?;
    }
    if let Some(hz) = h.hz {
        writeln!(out, "hz            {hz}")?;
    } else {
        writeln!(
            out,
            "hz            (記録なし。{} を仮定)",
            file.effective_hz()
        )?;
    }
    if let Some(cpu) = h.real_cpu_count() {
        writeln!(out, "cpu_nr        {cpu}")?;
    }
    writeln!(out, "activities    {}", h.act_nr)?;
    writeln!(out, "records at    {}", file.records_offset())?;

    writeln!(out)?;
    writeln!(
        out,
        "{:>3}  {:<14} {:>6}  {:>7} {:>5} {:>6}  {:>6}  types_nr",
        "id", "activity", "magic", "nr", "nr2", "size", "has_nr"
    )?;
    for a in file.activities() {
        let types = match a.types_nr {
            Some(t) => format!("({}, {}, {})", t[0], t[1], t[2]),
            None => "-".to_string(),
        };
        writeln!(
            out,
            "{:>3}  {:<14} 0x{:04x}  {:>7} {:>5} {:>6}  {:>6}  {}",
            a.id.0,
            a.id.display_name(),
            a.magic,
            a.nr,
            a.nr2,
            a.size,
            if a.has_nr { "yes" } else { "no" },
            types
        )?;
    }

    for d in file.diagnostics() {
        writeln!(out, "\n診断: {}", d.message)?;
    }

    Ok(())
}

fn write_info_json<W: Write>(out: &mut W, file: &SaFile) -> anyhow::Result<()> {
    let m = file.magic();
    let h = file.header();
    let enc = file.encoding();

    let activities: Vec<serde_json::Value> = file
        .activities()
        .iter()
        .map(|a| {
            serde_json::json!({
                "id": a.id.0,
                "symbol": a.id.symbol(),
                "magic": a.magic,
                "nr": a.nr,
                "nr2": a.nr2,
                "size": a.size,
                "has_nr": a.has_nr,
                "types_nr": a.types_nr,
            })
        })
        .collect();

    let value = serde_json::json!({
        "schema_version": re_sar_ch::output::json::SCHEMA_VERSION,
        "file": file.path().display().to_string(),
        "format": {
            "magic": format!("0x{:04x}", m.format_magic),
            "versions": file.spec().versions,
            "sysstat_version": m.version_string(),
            "header_size": m.header_size,
            "hdr_types_nr": m.hdr_types_nr,
            "upgraded": m.upgraded,
        },
        "encoding": {
            "endian": enc.endian.as_str(),
            "sizeof_long": h.sizeof_long,
            "abi": enc.abi.name,
        },
        "header": {
            "ust_time": h.ust_time,
            "date": format!("{:04}-{:02}-{:02}", h.year, h.month, h.day),
            "sysname": h.sysname,
            "release": h.release,
            "machine": h.machine,
            "nodename": h.nodename,
            "timezone": h.tzname,
            "hz": h.hz,
            "effective_hz": file.effective_hz(),
            "cpu_nr": h.real_cpu_count(),
            "sa_cpu_nr": h.cpu_nr,
            "act_nr": h.act_nr,
            "vol_act_nr": h.vol_act_nr,
        },
        "records_offset": file.records_offset(),
        "activities": activities,
        "diagnostics": file.diagnostics().iter().map(|d| d.message.clone()).collect::<Vec<_>>(),
    });

    serde_json::to_writer_pretty(&mut *out, &value)?;
    writeln!(out)?;
    Ok(())
}

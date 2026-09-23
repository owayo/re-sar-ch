//! `sadf` 互換オプションの手書きパーサ。
//!
//! 典拠は [`docs/format/03-output-format.md`] 第 VI 部 §2 (sysstat 12.8.0 の `sadf.c` /
//! `format.c`)。
//!
//! `sadf` の解析も 2 段構造だが、`sar` とは異なり **`--` を境に「`sadf` 自身の
//! オプション」と「`sar` レポート層に渡すオプション」を切り替える**
//! (`sar_options` 変数)。`sar` に `--` は存在しない。
//!
//! `sar` との差分で特に注意が要る点。
//!
//! - `sadf` には `--help` / `--human` / `--pretty` / `--dec=` / `--utc` / `--sadc` /
//!   `-A` / `-D` / `-i` / `-f` / `-o` が**無い**。`sadf --help` は未知オプションとして
//!   usage → exit 1 になる。
//! - `-H` / `-h` は `--` の前後で意味が完全に変わる
//!   (前: ヘッダのみ / 横並び、後: hugepages / pretty+human)。
//! - 出力形式 (`-c -d -g -j -l -p -r -x`) は 1 個だけ。同じものの重複でも exit 1。
//! - `-T` / `-t` / `-U` の排他チェックは形式ごとのフラグ除去より前に行われるため、
//!   その形式が受け付けないフラグでも 2 個以上書けば exit 1。
//! - interval は 1 以上必須 (`sar` は 0 = 起動以降の平均を許す)、count は 0 可 (= 全件)。
//!
//! [`docs/format/03-output-format.md`]: ../../../docs/format/03-output-format.md

use std::path::PathBuf;

use super::sar_args::{self, Activity, Caller, CpuBitmap, SarArgError, SarOptions};

// ============================================================================
// 出力形式
// ============================================================================

/// `sadf` の出力形式 (`format.c` の `id`)。**同時に指定できるのは 1 つだけ**。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SadfFormat {
    /// `-d` — RDBMS 取り込み向けの `;` 区切り出力 (`F_DB_OUTPUT` = 1)。
    Db,
    /// `-H` のみ指定したときの既定 (`F_HEADER_OUTPUT` = 2)。
    Header,
    /// `-p` — awk 等で扱いやすい出力。**形式未指定時の既定** (`F_PPC_OUTPUT` = 3)。
    Ppc,
    /// `-x` — XML 出力 (`F_XML_OUTPUT` = 4)。
    Xml,
    /// `-j` — JSON 出力 (`F_JSON_OUTPUT` = 5)。`sar -j` (永続デバイス名) とは別物。
    Json,
    /// `-c` — 旧形式データファイルを現行形式へ変換 (`F_CONV_OUTPUT` = 6)。
    Conv,
    /// `-g` — SVG グラフ出力 (`F_SVG_OUTPUT` = 7)。同時に `S_F_MINMAX` も立つ。
    Svg,
    /// `-r` — 生カウンタ値の出力 (`F_RAW_OUTPUT` = 8)。
    Raw,
    /// `-l` — PCP アーカイブへのエクスポート (`F_PCP_OUTPUT` = 9)。
    ///
    /// 本家は `HAVE_PCP` 未定義のビルドで `PCP support not compiled in` (exit 1) を出す。
    /// パーサは形式として受け付けるだけなので、対応の有無は実行層が判断する。
    Pcp,
}

/// 形式ごとに受け付けるフラグ (`format.c` の `FO_*`)。
///
/// `check_format_options()` は**受け付けないフラグを黙って落とす**。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FormatCaps {
    /// `FO_HEADER_ONLY` — `-H`
    header_only: bool,
    /// `FO_HORIZONTALLY` — `-h`
    horizontally: bool,
    /// `FO_LOCAL_TIME` — `-T`
    local_time: bool,
    /// `FO_SEC_EPOCH` — `-U`
    sec_epoch: bool,
    /// `-t` は `FO_NO_TRUE_TIME` を持つ形式 (PCP) だけが落とす。
    true_time: bool,
}

impl SadfFormat {
    /// `format.c` の `id`。
    pub fn id(self) -> u8 {
        match self {
            SadfFormat::Db => 1,
            SadfFormat::Header => 2,
            SadfFormat::Ppc => 3,
            SadfFormat::Xml => 4,
            SadfFormat::Json => 5,
            SadfFormat::Conv => 6,
            SadfFormat::Svg => 7,
            SadfFormat::Raw => 8,
            SadfFormat::Pcp => 9,
        }
    }

    /// この形式を選ぶオプション文字 (既定の `Header` は `-H` 由来)。
    pub fn option_char(self) -> char {
        match self {
            SadfFormat::Db => 'd',
            SadfFormat::Header => 'H',
            SadfFormat::Ppc => 'p',
            SadfFormat::Xml => 'x',
            SadfFormat::Json => 'j',
            SadfFormat::Conv => 'c',
            SadfFormat::Svg => 'g',
            SadfFormat::Raw => 'r',
            SadfFormat::Pcp => 'l',
        }
    }

    fn caps(self) -> FormatCaps {
        match self {
            SadfFormat::Header => FormatCaps {
                header_only: true,
                horizontally: false,
                local_time: false,
                sec_epoch: false,
                true_time: true,
            },
            SadfFormat::Db => FormatCaps {
                header_only: false,
                horizontally: true,
                local_time: true,
                sec_epoch: true,
                true_time: true,
            },
            SadfFormat::Ppc | SadfFormat::Raw => FormatCaps {
                header_only: false,
                horizontally: false,
                local_time: true,
                sec_epoch: true,
                true_time: true,
            },
            SadfFormat::Xml | SadfFormat::Json | SadfFormat::Svg => FormatCaps {
                header_only: true,
                horizontally: false,
                local_time: true,
                sec_epoch: false,
                true_time: true,
            },
            SadfFormat::Conv => FormatCaps {
                header_only: false,
                horizontally: false,
                local_time: false,
                sec_epoch: false,
                true_time: true,
            },
            SadfFormat::Pcp => FormatCaps {
                header_only: true,
                horizontally: false,
                local_time: true,
                sec_epoch: false,
                true_time: false,
            },
        }
    }
}

// ============================================================================
// -O サブオプション
// ============================================================================

/// SVG のカラーパレット (`palette`)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SvgPalette {
    /// `SVG_DEFAULT_COL_PALETTE`
    #[default]
    Default,
    /// `-O customcol` — `S_COLORS_PALETTE` で指定したカスタムパレット。
    Custom,
    /// `-O bwcol` — 白黒パレット。
    Bw,
}

/// `-O <opts>` で指定される出力制御サブオプション。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SadfOutputOptions {
    /// `skipempty` — 全グラフがゼロのビューを描かない (`-g`)。
    pub skip_empty: bool,
    /// `autoscale` — ビューのスケールに合わせて各グラフを最大化 (`-g`)。
    pub autoscale: bool,
    /// `oneday` — 24 時間分の時間軸で描画 (`-g`)。
    pub one_day: bool,
    /// `showidle` — CPU グラフに `%idle` も描画 (`-g`)。
    pub show_idle: bool,
    /// `showinfo` — 各ビューに日付・ホスト名等を描画 (`-g`)。
    pub show_info: bool,
    /// `showtoc` — SVG 先頭に目次を追加 (`-g`)。
    pub show_toc: bool,
    /// `packed` — 同一 activity の全ビューを 1 行にまとめる (`-g`)。
    pub packed: bool,
    /// `height=<value>` — SVG キャンバス高さ (`-g`)。数字のみ。
    pub canvas_height: Option<u32>,
    /// `customcol` / `bwcol`。
    pub palette: SvgPalette,
    /// `debug` — SVG にコメントを追加 / raw 出力に追加情報 (`-g` と `-r`)。
    pub debug: bool,
    /// `hz=<value>` — 旧データファイル作成マシンの 1 秒あたり tick 数 (`-c`)。
    pub user_hz: Option<u32>,
    /// `pcparchive=<name>` — 生成する PCP アーカイブ名 (`-l`)。数字チェックなし。
    pub pcp_archive: Option<String>,
}

// ============================================================================
// その他の型
// ============================================================================

/// タイムスタンプの基準系。`-T` / `-t` / `-U` は相互排他。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SadfTimeBase {
    /// 既定。`sadf` は `flags = 0` なので UTC 表示。
    Utc,
    /// `-T` — 実行環境のローカル時刻 (`S_F_LOCAL_TIME`)。
    LocalTime,
    /// `-t` — ファイル作成者のローカル時刻 (`S_F_TRUE_TIME`)。
    TrueTime,
    /// `-U` — epoch 秒 (`S_F_SEC_EPOCH`)。
    SecEpoch,
}

/// 引数解析の途中で即座に表示して終了する系のオプション。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SadfImmediate {
    /// `-V` — 環境変数 (`S_COLORS_PALETTE` → `S_TIME_DEF_TIME`) とバージョンを出して exit 0。
    Version,
}

// ============================================================================
// SadfOptions
// ============================================================================

/// `sadf` 互換オプションの解析結果。
///
/// `-P` / `-s` / `-e` / `--dev=` / `--fs=` / `--iface=` / `--int=` は `--` の前後
/// どちらでも指定でき、本家では同じグローバル (activity の item リスト、`tm_start` 等)
/// を書き換える。そのため**それらの値は [`SadfOptions::sar`] に入る**。
/// `sar` レポート層のオプション (`--` 以降) も同じ構造体に集約される。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SadfOptions {
    /// 出力形式。`finalize` 後は必ず `Some`
    /// (未指定なら `-H` の有無で [`SadfFormat::Header`] / [`SadfFormat::Ppc`])。
    pub format: Option<SadfFormat>,

    /// `-H` — レポートヘッダのみ表示 (`S_F_HDR_ONLY`)。
    /// 形式が受け付けない場合は `check_format_options()` で落とされる。
    pub header_only: bool,

    /// `-h` — `-d` 併用時に全 activity を 1 行へ横並び表示 (`S_F_HORIZONTALLY`)。
    pub horizontally: bool,

    /// positional のデータファイル名。`None` = 既定の日次データファイル。
    pub data_file: Option<PathBuf>,

    /// 既定の日次ファイルへフォールバックしたか。`sadf` は `dfile` 未設定なら**常に**
    /// フォールバックする。
    pub default_file_used: bool,

    /// `-[0-9]+` の日オフセット。ディレクトリ指定時には効かない
    /// (`check_alt_sa_dir()` へ 0 が渡される)。
    pub day_offset: u32,

    /// positional の interval。`finalize` 後は必ず `Some` (未指定なら 1)。
    /// **`sar` と違い 0 を許さない**。
    pub interval: Option<u64>,

    /// positional の count。`None` = 全レコード (`count = -1`)。
    /// `0` を明示した場合も `None` に正規化される。
    pub count: Option<u64>,

    /// `-O <opts>`。
    pub output: SadfOutputOptions,

    /// `-V`。`Some` のとき他のフィールドは未完成 (解析を打ち切っている)。
    pub immediate: Option<SadfImmediate>,

    /// `sar` レポート層と共有される状態。
    ///
    /// - activity 選択と `opt_flags` (`--` 以降の `sar` オプション由来)
    /// - `-P` の CPU ビットマップ、`--dev=` 等の item リスト、`-s` / `-e`
    /// - `-C` (`S_F_COMMENT`)、`-T` / `-t` / `-U` の時刻基準フラグ
    ///
    /// `sar` 固有の入出力指定 ([`SarOptions::input`] / [`SarOptions::output`] /
    /// `-i` / `-D`) は `sadf` では設定されない (`--` 以降に書けばエラー)。
    pub sar: SarOptions,
}

impl Default for SadfOptions {
    fn default() -> Self {
        SadfOptions {
            format: None,
            header_only: false,
            horizontally: false,
            data_file: None,
            default_file_used: false,
            day_offset: 0,
            interval: None,
            count: None,
            output: SadfOutputOptions::default(),
            immediate: None,
            sar: SarOptions::for_sadf(),
        }
    }
}

impl SadfOptions {
    /// タイムスタンプの基準系。`finalize` 後の値を見ること
    /// (形式が受け付けないフラグは落とされている)。
    pub fn time_base(&self) -> SadfTimeBase {
        if self.sar.flags.sec_epoch {
            SadfTimeBase::SecEpoch
        } else if self.sar.flags.true_time {
            SadfTimeBase::TrueTime
        } else if self.sar.flags.local_time {
            SadfTimeBase::LocalTime
        } else {
            SadfTimeBase::Utc
        }
    }

    /// 選択された activity を ID 昇順で返す。
    pub fn selected_activities(&self) -> impl Iterator<Item = Activity> + '_ {
        self.sar.selected_activities()
    }

    /// `-P` の CPU ビットマップ。
    pub fn cpu_bitmap(&self) -> &CpuBitmap {
        &self.sar.cpu_bitmap
    }
}

// ============================================================================
// エラー
// ============================================================================

/// `sadf` 引数解析のエラー。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SadfArgError {
    /// `sar` レポート層 (`--` 以降) のオプション解析エラー、および
    /// `-P` / `-s` / `-e` / `--dev=` 系の共通処理のエラー。
    #[error(transparent)]
    Sar(#[from] SarArgError),

    /// 未知の 1 文字オプション。`sadf --help` もここに落ちる (2 文字目の `-` が未知)。
    #[error("-{ch}: 不明なオプションです (トークン: {token})")]
    UnknownShortOption { token: String, ch: char },

    /// 出力形式が 2 つ以上指定された (同じものの重複も不可)。
    #[error("-{second}: 出力形式は 1 つだけ指定してください (すでに -{first} が指定されています)")]
    MultipleFormats { first: char, second: char },

    /// `-O` の必須引数が無い。
    #[error("-O: 引数が必要です")]
    MissingOutputOptions,

    /// `-O` のサブキーワードが不正。
    #[error("-O {value}: 不正なサブオプションです")]
    InvalidOutputOption { value: String },

    /// `-O` を `--` 以降に書いた。
    #[error("-O: -- より後には指定できません")]
    OutputOptionAfterDashDash,

    /// `-m` / `-n` / `-q` を `--` より前に書いた。
    #[error("{opt}: -- より前には指定できません ({opt} は sar レポート層のオプション)")]
    KeywordBeforeDashDash { opt: &'static str },

    /// `-m` / `-n` の引数が無い。
    #[error("{opt}: 引数が必要です")]
    MissingArgument { opt: &'static str },

    /// `-T` / `-t` / `-U` を 2 つ以上指定した。
    #[error("-T / -t / -U は同時に指定できません")]
    ConflictingTimeBase,

    /// データファイル名 / 日オフセットの指定が重複している。
    #[error("{value}: データファイルの指定が重複しています")]
    DuplicateDataFile { value: String },

    /// interval / count が不正。
    #[error("{value}: interval は 1 以上を指定してください")]
    InvalidInterval { value: String },

    /// count が重複指定された。
    #[error("{value}: count の指定が重複しています")]
    DuplicateCount { value: String },
}

// ============================================================================
// -O の解析
// ============================================================================

/// `-O <opts>` を解析する (`strtok(argv, ",")` で分割して `strcmp` / `strncmp` 照合)。
fn parse_sadf_o_opt(value: &str, out: &mut SadfOutputOptions) -> Result<(), SadfArgError> {
    for token in value.split(',').filter(|t| !t.is_empty()) {
        match token {
            "skipempty" => out.skip_empty = true,
            "autoscale" => out.autoscale = true,
            "oneday" => out.one_day = true,
            "showidle" => out.show_idle = true,
            "showinfo" => out.show_info = true,
            "showtoc" => out.show_toc = true,
            "packed" => out.packed = true,
            "customcol" => out.palette = SvgPalette::Custom,
            "bwcol" => out.palette = SvgPalette::Bw,
            "debug" => out.debug = true,
            _ => {
                if let Some(v) = token.strip_prefix("height=") {
                    // 空文字や非数字は usage
                    if v.is_empty() || !sar_args::is_all_digits(v) {
                        return Err(SadfArgError::InvalidOutputOption {
                            value: token.to_string(),
                        });
                    }
                    out.canvas_height = Some(sar_args::atol(v) as u32);
                } else if let Some(v) = token.strip_prefix("hz=") {
                    if v.is_empty() || !sar_args::is_all_digits(v) {
                        return Err(SadfArgError::InvalidOutputOption {
                            value: token.to_string(),
                        });
                    }
                    out.user_hz = Some(sar_args::atol(v) as u32);
                } else if let Some(v) = token.strip_prefix("pcparchive=") {
                    // 数字チェックなし。空文字も許される。
                    out.pcp_archive = Some(v.to_string());
                } else {
                    return Err(SadfArgError::InvalidOutputOption {
                        value: token.to_string(),
                    });
                }
            }
        }
    }
    Ok(())
}

// ============================================================================
// sadf 固有の 1 文字オプション (束ね可能)
// ============================================================================

/// `--` より前の 1 文字オプションを 1 トークン分処理する (`-CHhT` のように束ねられる)。
fn parse_sadf_opt(token: &str, o: &mut SadfOptions) -> Result<(), SadfArgError> {
    let set_format = |fmt: SadfFormat, o: &mut SadfOptions| -> Result<(), SadfArgError> {
        if let Some(first) = o.format {
            return Err(SadfArgError::MultipleFormats {
                first: first.option_char(),
                second: fmt.option_char(),
            });
        }
        o.format = Some(fmt);
        Ok(())
    };

    for ch in token.chars().skip(1) {
        match ch {
            'C' => o.sar.flags.comment = true,
            'c' => set_format(SadfFormat::Conv, o)?,
            'd' => set_format(SadfFormat::Db, o)?,
            'g' => {
                set_format(SadfFormat::Svg, o)?;
                // SVG は同時に S_F_MINMAX も立てる。
                o.sar.flags.minmax = true;
            }
            // -H はヘッダのみ (sar の -H = hugepages とは別物)。
            'H' => o.header_only = true,
            // -h は横並び表示 (sar の -h = pretty+human とは別物)。
            'h' => o.horizontally = true,
            'j' => set_format(SadfFormat::Json, o)?,
            'l' => set_format(SadfFormat::Pcp, o)?,
            'p' => set_format(SadfFormat::Ppc, o)?,
            'r' => set_format(SadfFormat::Raw, o)?,
            'T' => o.sar.flags.local_time = true,
            't' => o.sar.flags.true_time = true,
            'U' => o.sar.flags.sec_epoch = true,
            'V' => {
                o.immediate = Some(SadfImmediate::Version);
                return Ok(());
            }
            'x' => set_format(SadfFormat::Xml, o)?,
            _ => {
                return Err(SadfArgError::UnknownShortOption {
                    token: token.to_string(),
                    ch,
                });
            }
        }
    }
    Ok(())
}

// ============================================================================
// エントリポイント
// ============================================================================

/// `sadf` 互換の引数列を解析する。
///
/// `argv` はプログラム名を**含まない** sadf オプション部分。
///
/// ```
/// use re_sar_ch::cli::sadf_args::{parse_sadf_args, SadfFormat};
/// use re_sar_ch::cli::sar_args::Activity;
///
/// let argv: Vec<String> = ["-j", "sa01", "--", "-u"].iter().map(|s| s.to_string()).collect();
/// let opts = parse_sadf_args(&argv).unwrap();
/// assert_eq!(opts.format, Some(SadfFormat::Json));
/// assert_eq!(opts.data_file.as_deref(), Some(std::path::Path::new("sa01")));
/// assert!(opts.sar.is_selected(Activity::Cpu));
/// ```
pub fn parse_sadf_args(argv: &[String]) -> Result<SadfOptions, SadfArgError> {
    // `-q` のキーワード解析失敗時に strtok 破壊を再現するためのローカルコピー。
    let mut argv: Vec<String> = argv.to_vec();
    let mut o = SadfOptions::default();
    // `--` を越えたか (`sar_options`)。
    let mut sar_options = false;
    let mut opt = 0usize;

    while opt < argv.len() {
        let arg = argv[opt].clone();

        if arg == "--" {
            sar_options = true;
            opt += 1;
        } else if sar_args::parse_item_filter(&arg, &mut o.sar) {
            opt += 1;
        } else if arg == "-P" {
            opt += 1;
            let Some(value) = argv.get(opt).cloned() else {
                return Err(SarArgError::MissingArgument { opt: "-P" }.into());
            };
            sar_args::parse_values(&value, &mut o.sar.cpu_bitmap)?;
            o.sar.flags.option_p = true;
            opt += 1;
        } else if arg == "-s" {
            o.sar.tm_start =
                sar_args::parse_timestamp(&argv, &mut opt, sar_args::DEF_TMSTART, "-s")?;
        } else if arg == "-e" {
            o.sar.tm_end = sar_args::parse_timestamp(&argv, &mut opt, sar_args::DEF_TMEND, "-e")?;
        } else if arg == "-O" {
            // -O は `--` より前だけ。
            if sar_options {
                return Err(SadfArgError::OutputOptionAfterDashDash);
            }
            opt += 1;
            let Some(value) = argv.get(opt).cloned() else {
                return Err(SadfArgError::MissingOutputOptions);
            };
            parse_sadf_o_opt(&value, &mut o.output)?;
            opt += 1;
        } else if arg == "-m" || arg == "-n" {
            // -m / -n は `--` より後だけ。
            let name: &'static str = if arg == "-m" { "-m" } else { "-n" };
            if !sar_options {
                return Err(SadfArgError::KeywordBeforeDashDash { opt: name });
            }
            let Some(value) = argv.get(opt + 1).cloned() else {
                return Err(SadfArgError::MissingArgument { opt: name });
            };
            if name == "-m" {
                sar_args::parse_sar_m_opt(&value, &mut o.sar)?;
            } else {
                sar_args::parse_sar_n_opt(&value, &mut o.sar)?;
            }
            opt += 2;
        } else if arg == "-q" {
            // -q も `--` より後だけ。引数なしなら A_QUEUE。
            if !sar_options {
                return Err(SadfArgError::KeywordBeforeDashDash { opt: "-q" });
            }
            sar_args::parse_queue_option(&mut argv, &mut opt, &mut o.sar);
        } else if sar_args::is_day_offset(&arg) {
            if o.data_file.is_some() || o.day_offset != 0 {
                return Err(SadfArgError::DuplicateDataFile { value: arg });
            }
            o.day_offset = sar_args::atol(&arg[1..]) as u32;
            opt += 1;
        } else if arg.starts_with('-') {
            if sar_options {
                // `--` 以降は sar の 1 文字オプションとして解釈する。
                // -C / -H / -h はここで意味が変わる (コメント表示 / hugepages / pretty+human)。
                sar_args::parse_sar_opt(&argv, &mut opt, Caller::Sadf, &mut o.sar)?;
            } else {
                parse_sadf_opt(&arg, &mut o)?;
                if o.immediate.is_some() {
                    return Ok(o);
                }
                opt += 1;
            }
        } else if !sar_args::is_all_digits(&arg) {
            // 数字以外を含む positional はデータファイル名。
            if o.data_file.is_some() || o.day_offset != 0 {
                return Err(SadfArgError::DuplicateDataFile { value: arg });
            }
            o.data_file = Some(PathBuf::from(&arg));
            opt += 1;
        } else if o.interval.is_none() {
            // interval。sar と違い 0 を許さない。
            let interval = sar_args::atol(&arg);
            if interval < 1 {
                return Err(SadfArgError::InvalidInterval { value: arg });
            }
            o.interval = Some(interval);
            opt += 1;
        } else {
            // count。0 は許され、後段で「全レコード」に正規化される。
            if o.count.is_some() {
                return Err(SadfArgError::DuplicateCount { value: arg });
            }
            o.count = Some(sar_args::atol(&arg));
            opt += 1;
        }
    }

    finalize(&mut o)?;
    Ok(o)
}

/// 解析ループ終了後の後処理 (`sadf.c` の `main()` と同じ順序)。
fn finalize(o: &mut SadfOptions) -> Result<(), SadfArgError> {
    // (1) -A かつ -P 未指定なら CPU ビットマップを全ビット立てる (set_bitmaps())。
    if o.sar.flags.option_a && !o.sar.flags.option_p {
        o.sar.cpu_bitmap.set_all();
    }

    // (2) データファイル未指定なら常に既定の日次ファイルへフォールバックする。
    if o.data_file.is_none() {
        o.default_file_used = true;
    }

    // (3) PCP アーカイブ名の既定はデータファイル名。
    //     既定日次ファイルの場合は実行層が解決したパスを入れる必要がある。
    if o.format == Some(SadfFormat::Pcp)
        && o.output.pcp_archive.is_none()
        && let Some(path) = &o.data_file
    {
        o.output.pcp_archive = Some(path.to_string_lossy().into_owned());
    }

    // (4) 時刻範囲の整合 (日跨ぎ補正 / epoch 逆順のエラー)。
    sar_args::check_time_limits(o.sar.tm_start, &mut o.sar.tm_end)?;

    // (5) -T / -t / -U の排他チェック。
    //     check_format_options() より前なので、形式が受け付けないフラグでも
    //     2 個以上書けばエラーになる。
    let time_flags = u8::from(o.sar.flags.local_time)
        + u8::from(o.sar.flags.true_time)
        + u8::from(o.sar.flags.sec_epoch);
    if time_flags > 1 {
        return Err(SadfArgError::ConflictingTimeBase);
    }

    // (6) count == 0 は「全レコード」。
    if o.count == Some(0) {
        o.count = None;
    }

    // (7) activity が何も選ばれていなければ A_CPU。
    o.sar.select_default_activity();

    // (8) check_format_options(): 既定形式の決定と、形式が受け付けないフラグの除去。
    let format = o.format.unwrap_or(if o.header_only {
        SadfFormat::Header
    } else {
        SadfFormat::Ppc
    });
    o.format = Some(format);
    let caps = format.caps();
    if !caps.header_only {
        o.header_only = false;
    }
    if !caps.horizontally {
        o.horizontally = false;
    }
    if !caps.local_time {
        o.sar.flags.local_time = false;
    }
    if !caps.sec_epoch {
        o.sar.flags.sec_epoch = false;
    }
    if !caps.true_time {
        o.sar.flags.true_time = false;
    }

    // (9) interval 未指定なら 1。
    if o.interval.is_none() {
        o.interval = Some(1);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::sar_args::{OptFlags, TimeSpec};

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| (*s).to_string()).collect()
    }

    fn parse(args: &[&str]) -> SadfOptions {
        parse_sadf_args(&argv(args)).expect("解析に成功するはず")
    }

    fn parse_err(args: &[&str]) -> SadfArgError {
        parse_sadf_args(&argv(args)).expect_err("解析に失敗するはず")
    }

    fn selected(o: &SadfOptions) -> Vec<Activity> {
        o.selected_activities().collect()
    }

    // ---------------------------------------------------------------
    // 既定値と出力形式
    // ---------------------------------------------------------------

    #[test]
    fn default_format_is_ppc() {
        let o = parse(&["sa01"]);
        assert_eq!(o.format, Some(SadfFormat::Ppc));
        assert_eq!(o.data_file.as_deref(), Some(std::path::Path::new("sa01")));
        assert_eq!(o.interval, Some(1));
        assert_eq!(o.count, None);
        assert_eq!(selected(&o), vec![Activity::Cpu]);
        assert_eq!(o.time_base(), SadfTimeBase::Utc, "sadf の既定は UTC");
    }

    #[test]
    fn no_data_file_falls_back_to_default_daily() {
        let o = parse(&[]);
        assert!(o.default_file_used);
        assert_eq!(o.data_file, None);
        assert_eq!(o.format, Some(SadfFormat::Ppc));
    }

    #[test]
    fn header_only_without_format_selects_header_format() {
        let o = parse(&["-H", "sa01"]);
        assert_eq!(o.format, Some(SadfFormat::Header));
        assert!(o.header_only);
    }

    #[test]
    fn format_ids_match_sysstat() {
        assert_eq!(SadfFormat::Db.id(), 1);
        assert_eq!(SadfFormat::Header.id(), 2);
        assert_eq!(SadfFormat::Ppc.id(), 3);
        assert_eq!(SadfFormat::Xml.id(), 4);
        assert_eq!(SadfFormat::Json.id(), 5);
        assert_eq!(SadfFormat::Conv.id(), 6);
        assert_eq!(SadfFormat::Svg.id(), 7);
        assert_eq!(SadfFormat::Raw.id(), 8);
        assert_eq!(SadfFormat::Pcp.id(), 9);
    }

    #[test]
    fn two_formats_are_rejected() {
        assert!(matches!(
            parse_err(&["-d", "-j", "sa01"]),
            SadfArgError::MultipleFormats { .. }
        ));
        // 同じものの重複でも不可
        assert!(matches!(
            parse_err(&["-d", "-d", "sa01"]),
            SadfArgError::MultipleFormats {
                first: 'd',
                second: 'd'
            }
        ));
        // 束ねた形でも同じ
        assert!(matches!(
            parse_err(&["-dj", "sa01"]),
            SadfArgError::MultipleFormats { .. }
        ));
    }

    #[test]
    fn svg_also_sets_minmax() {
        let o = parse(&["-g", "sa01"]);
        assert_eq!(o.format, Some(SadfFormat::Svg));
        assert!(o.sar.flags.minmax);
    }

    #[test]
    fn conv_format() {
        let o = parse(&["-c", "sa01"]);
        assert_eq!(o.format, Some(SadfFormat::Conv));
    }

    #[test]
    fn pcp_archive_defaults_to_data_file() {
        let o = parse(&["-l", "sa01"]);
        assert_eq!(o.format, Some(SadfFormat::Pcp));
        assert_eq!(o.output.pcp_archive.as_deref(), Some("sa01"));
    }

    // ---------------------------------------------------------------
    // sar に無いオプション
    // ---------------------------------------------------------------

    #[test]
    fn sadf_has_no_long_options_other_than_lists() {
        // --help / --human / --pretty / --dec= / --utc / --sadc は存在しない。
        // 2 文字目の '-' が未知の 1 文字として扱われる。
        for arg in [
            "--help", "--human", "--pretty", "--dec=0", "--utc", "--sadc",
        ] {
            assert!(
                matches!(
                    parse_err(&[arg, "sa01"]),
                    SadfArgError::UnknownShortOption { ch: '-', .. }
                ),
                "{arg} は受け付けてはいけない"
            );
        }
    }

    #[test]
    fn sadf_has_no_a_d_i_f_o_options() {
        for arg in ["-A", "-D", "-i", "-f", "-o", "-z"] {
            assert!(
                parse_sadf_args(&argv(&[arg, "sa01"])).is_err(),
                "{arg} は受け付けてはいけない"
            );
        }
    }

    // ---------------------------------------------------------------
    // -T / -t / -U
    // ---------------------------------------------------------------

    #[test]
    fn time_base_options() {
        assert_eq!(
            parse(&["-d", "-T", "sa01"]).time_base(),
            SadfTimeBase::LocalTime
        );
        assert_eq!(
            parse(&["-d", "-t", "sa01"]).time_base(),
            SadfTimeBase::TrueTime
        );
        assert_eq!(
            parse(&["-d", "-U", "sa01"]).time_base(),
            SadfTimeBase::SecEpoch
        );
    }

    #[test]
    fn two_time_base_options_are_rejected_regardless_of_format() {
        assert_eq!(
            parse_err(&["-T", "-U", "sa01"]),
            SadfArgError::ConflictingTimeBase
        );
        // -x は -U を落とす形式だが、排他チェックはフラグ除去より前
        assert_eq!(
            parse_err(&["-x", "-T", "-U", "sa01"]),
            SadfArgError::ConflictingTimeBase
        );
    }

    #[test]
    fn format_drops_unsupported_flags() {
        // ppc は -H と -h を落とす
        let o = parse(&["-p", "-H", "-h", "sa01"]);
        assert!(!o.header_only);
        assert!(!o.horizontally);

        // db は -h を保つが -H を落とす
        let o = parse(&["-d", "-H", "-h", "sa01"]);
        assert!(!o.header_only);
        assert!(o.horizontally);

        // xml は -H を保つが -h と -U を落とす
        let o = parse(&["-x", "-H", "-h", "sa01"]);
        assert!(o.header_only);
        assert!(!o.horizontally);
        let o = parse(&["-x", "-U", "sa01"]);
        assert_eq!(o.time_base(), SadfTimeBase::Utc, "-U は落とされる");

        // conv は -T も落とす
        let o = parse(&["-c", "-T", "sa01"]);
        assert_eq!(o.time_base(), SadfTimeBase::Utc);

        // pcp は -t を落とす (FO_NO_TRUE_TIME)
        let o = parse(&["-l", "-t", "sa01"]);
        assert_eq!(o.time_base(), SadfTimeBase::Utc);

        // raw は -T / -U を保つ
        let o = parse(&["-r", "-U", "sa01"]);
        assert_eq!(o.time_base(), SadfTimeBase::SecEpoch);
    }

    // ---------------------------------------------------------------
    // -O
    // ---------------------------------------------------------------

    #[test]
    fn output_options_full_set() {
        // tests/00550 の実コマンド由来
        let o = parse(&[
            "-g",
            "-O",
            "autoscale,packed,oneday,showidle,showtoc,skipempty,showinfo,bwcol",
            "sa01",
            "-T",
            "-C",
            "--",
            "-A",
        ]);
        assert!(o.output.autoscale);
        assert!(o.output.packed);
        assert!(o.output.one_day);
        assert!(o.output.show_idle);
        assert!(o.output.show_toc);
        assert!(o.output.skip_empty);
        assert!(o.output.show_info);
        assert_eq!(o.output.palette, SvgPalette::Bw);
        assert!(o.sar.flags.comment);
        assert_eq!(o.time_base(), SadfTimeBase::LocalTime);
        assert_eq!(o.sar.activities.len(), 43);
    }

    #[test]
    fn output_option_height() {
        let o = parse(&["-O", "height=370", "-g", "sa01"]);
        assert_eq!(o.output.canvas_height, Some(370));
    }

    #[test]
    fn output_option_height_requires_digits() {
        assert!(matches!(
            parse_err(&["-O", "height=", "-g", "sa01"]),
            SadfArgError::InvalidOutputOption { .. }
        ));
        assert!(matches!(
            parse_err(&["-O", "height=abc", "-g", "sa01"]),
            SadfArgError::InvalidOutputOption { .. }
        ));
    }

    #[test]
    fn output_option_hz_and_pcparchive() {
        let o = parse(&["-c", "-O", "hz=100", "sa01"]);
        assert_eq!(o.output.user_hz, Some(100));

        let o = parse(&["-l", "-O", "pcparchive=", "sa01"]);
        assert_eq!(
            o.output.pcp_archive.as_deref(),
            Some(""),
            "pcparchive= は空文字も許される (数字チェックなし)"
        );
    }

    #[test]
    fn unknown_output_option_is_rejected() {
        // showhints は 12.8.0 には存在しない
        assert!(matches!(
            parse_err(&["-g", "-O", "showhints", "sa01"]),
            SadfArgError::InvalidOutputOption { .. }
        ));
    }

    #[test]
    fn output_option_after_dash_dash_is_rejected() {
        assert_eq!(
            parse_err(&["-g", "sa01", "--", "-O", "debug"]),
            SadfArgError::OutputOptionAfterDashDash
        );
    }

    #[test]
    fn output_option_requires_argument() {
        assert_eq!(parse_err(&["-g", "-O"]), SadfArgError::MissingOutputOptions);
    }

    #[test]
    fn raw_debug_option() {
        // tests/00570 の実コマンド由来
        let o = parse(&["-r", "-O", "debug", "sa01", "-C", "--", "-A"]);
        assert_eq!(o.format, Some(SadfFormat::Raw));
        assert!(o.output.debug);
        assert!(o.sar.flags.comment);
    }

    // ---------------------------------------------------------------
    // -- の前後
    // ---------------------------------------------------------------

    #[test]
    fn keyword_options_require_dash_dash() {
        assert_eq!(
            parse_err(&["-m", "CPU", "sa01"]),
            SadfArgError::KeywordBeforeDashDash { opt: "-m" }
        );
        assert_eq!(
            parse_err(&["-n", "DEV", "sa01"]),
            SadfArgError::KeywordBeforeDashDash { opt: "-n" }
        );
        assert_eq!(
            parse_err(&["-q", "sa01"]),
            SadfArgError::KeywordBeforeDashDash { opt: "-q" }
        );

        let o = parse(&["-d", "sa01", "--", "-m", "FAN,IN,TEMP"]);
        assert_eq!(
            selected(&o),
            vec![Activity::PwrFan, Activity::PwrTemp, Activity::PwrIn]
        );
    }

    #[test]
    fn h_and_capital_h_change_meaning_after_dash_dash() {
        // `--` の前: -H = ヘッダのみ / -h = 横並び
        let o = parse(&["-d", "-Hh", "sa01"]);
        assert!(o.horizontally);
        assert!(!o.sar.is_selected(Activity::Huge));
        assert!(!o.sar.flags.pretty);

        // `--` の後: -H = hugepages / -h = pretty + human
        let o = parse(&["-d", "sa01", "--", "-H", "-h"]);
        assert!(o.sar.is_selected(Activity::Huge));
        assert!(o.sar.flags.pretty);
        assert!(o.sar.flags.human);
        assert!(!o.horizontally);
        assert!(!o.header_only);
    }

    #[test]
    fn t_is_rejected_after_dash_dash() {
        assert_eq!(
            parse_err(&["-d", "sa01", "--", "-t"]),
            SadfArgError::Sar(SarArgError::NotAllowedForSadf { ch: 't' })
        );
    }

    #[test]
    fn x_is_silently_ignored_after_dash_dash() {
        let o = parse(&["-d", "sa01", "--", "-x"]);
        assert!(!o.sar.flags.minmax, "-x は無言で無視される");
        assert_eq!(o.format, Some(SadfFormat::Db));
    }

    #[test]
    fn sar_only_options_are_rejected_after_dash_dash() {
        for ch in ["-f", "-o", "-i", "-D", "-V", "-P"] {
            // -P は sadf 側のハンドラが先に拾うので除外対象ではない
            if ch == "-P" {
                continue;
            }
            assert!(
                parse_sadf_args(&argv(&["-d", "sa01", "--", ch, "x"])).is_err(),
                "{ch} は -- 以降で受け付けてはいけない"
            );
        }
    }

    #[test]
    fn human_long_option_after_dash_dash_is_rejected() {
        // parse_sar_opt は単文字しか処理しないので 2 文字目の '-' が default: に落ちる
        assert_eq!(
            parse_err(&["-d", "sa01", "--", "--human"]),
            SadfArgError::Sar(SarArgError::UnknownShortOption {
                token: "--human".to_string(),
                ch: '-'
            })
        );
    }

    #[test]
    fn p_and_s_and_e_work_on_both_sides_of_dash_dash() {
        let o = parse(&["-d", "sa01", "-P", "all,3", "--", "-u"]);
        assert!(o.cpu_bitmap().aggregate_selected());
        assert_eq!(o.cpu_bitmap().selected_cpus().collect::<Vec<_>>(), vec![3]);

        let o = parse(&["-d", "sa01", "--", "-u", "-P", "1"]);
        assert_eq!(o.cpu_bitmap().selected_cpus().collect::<Vec<_>>(), vec![1]);
    }

    // ---------------------------------------------------------------
    // 実テストコマンド由来のケース
    // ---------------------------------------------------------------

    #[test]
    fn sadf_p_with_comment_and_all() {
        // tests/00500: sadf -p sa01 -C -- -A
        let o = parse(&["-p", "sa01", "-C", "--", "-A"]);
        assert_eq!(o.format, Some(SadfFormat::Ppc));
        assert!(o.sar.flags.comment);
        assert_eq!(o.sar.activities.len(), 43);
        assert!(
            o.cpu_bitmap().count_bits() > 1000,
            "-A は -P ALL を含意する"
        );
    }

    #[test]
    fn sadf_dh_with_bundled_sar_options() {
        // tests/00512: sadf -dh sa01 -- -Iu ALL -P all,3
        let o = parse(&["-dh", "sa01", "--", "-Iu", "ALL", "-P", "all,3"]);
        assert_eq!(o.format, Some(SadfFormat::Db));
        assert!(o.horizontally);
        assert!(o.sar.is_selected(Activity::Irq));
        assert_eq!(
            o.sar.opt_flags(Activity::Cpu),
            OptFlags::CPU_ALL,
            "'u' がトークン末尾なので ALL を消費する"
        );
        assert!(o.cpu_bitmap().aggregate_selected());
        assert_eq!(o.cpu_bitmap().selected_cpus().collect::<Vec<_>>(), vec![3]);
    }

    #[test]
    fn sadf_d_with_qu() {
        // tests/00515: sadf -d sa01 -- -qu
        let o = parse(&["-d", "sa01", "--", "-qu"]);
        assert_eq!(
            selected(&o),
            vec![Activity::Cpu, Activity::Queue],
            "束ねた -q はキーワードを取らない"
        );
        assert_eq!(o.sar.opt_flags(Activity::Cpu), OptFlags::CPU_DEF);
    }

    #[test]
    fn sadf_x_with_interval_and_count() {
        // tests/00525: sadf -x sa01 -C 1 2 -- -uw -P 0-2
        let o = parse(&["-x", "sa01", "-C", "1", "2", "--", "-uw", "-P", "0-2"]);
        assert_eq!(o.format, Some(SadfFormat::Xml));
        assert!(o.sar.flags.comment);
        assert_eq!(o.interval, Some(1));
        assert_eq!(o.count, Some(2));
        assert!(o.sar.is_selected(Activity::Cpu));
        assert!(o.sar.is_selected(Activity::Pcsw));
        assert_eq!(
            o.cpu_bitmap().selected_cpus().collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
    }

    #[test]
    fn sadf_with_item_filters_and_time_range() {
        // tests/00580 相当 (デバイス名は一般的なものに置き換え)
        let o = parse(&[
            "-d",
            "-s",
            "13:20:20",
            "-e",
            "13:20:40",
            "--iface=eth0",
            "--dev=sda",
            "--fs=/dev/sda1",
            "sa01",
            "--",
            "-n",
            "DEV",
            "-Fdp",
        ]);
        assert_eq!(o.format, Some(SadfFormat::Db));
        assert_eq!(
            o.sar.tm_start,
            TimeSpec::HhMmSs {
                hour: 13,
                min: 20,
                sec: 20
            }
        );
        assert_eq!(
            o.sar.tm_end,
            TimeSpec::HhMmSs {
                hour: 13,
                min: 20,
                sec: 40
            }
        );
        assert_eq!(o.sar.item_list(Activity::NetDev), ["eth0"]);
        assert_eq!(o.sar.item_list(Activity::NetEdev), ["eth0"]);
        assert_eq!(o.sar.item_list(Activity::Disk), ["sda"]);
        assert_eq!(o.sar.item_list(Activity::Fs), ["/dev/sda1"]);
        assert!(o.sar.is_selected(Activity::NetDev));
        assert_eq!(o.sar.opt_flags(Activity::Fs), OptFlags::FILESYSTEM);
        assert!(o.sar.is_selected(Activity::Disk));
        assert!(o.sar.flags.pretty);
    }

    #[test]
    fn item_filters_share_sar_semantics_on_both_sides_of_dash_dash() {
        let cases: &[(&str, Activity, &[&str])] = &[
            ("--dev=sda,sda,1-3", Activity::Disk, &["sda", "1-3"]),
            (
                "--fs=/dev/sda1,,/home",
                Activity::Fs,
                &["/dev/sda1", "/home"],
            ),
            // 12.8.0 の `add_list_item()` は長すぎる名前を切り詰めずに捨てる
            (
                "--iface=0123456789abcdefghij,0123456789abcde",
                Activity::NetDev,
                &["0123456789abcde"],
            ),
            (
                "--int=3-5,4,4095-,MCE-XXX,ABCDEFXYZ",
                Activity::Irq,
                &["3", "4", "5", "4095", "MCE-XXX"],
            ),
        ];
        for &(arg, activity, expected) in cases {
            let sar = sar_args::parse_sar_args(&argv(&[arg, "-f", "sa01"])).unwrap();
            for sadf_args in [[arg, "sa01", "--"], ["sa01", "--", arg]] {
                let sadf = parse(&sadf_args);
                assert_eq!(sar.item_list(activity), expected, "{arg}");
                assert_eq!(sadf.sar.item_list(activity), expected, "{sadf_args:?}");
                assert!(sadf.sar.list_on_cmdline(activity));
                // 絞り込みだけでは activity 自体は選択されない。
                assert_eq!(selected(&sadf), vec![Activity::Cpu]);
                if activity == Activity::NetDev {
                    assert_eq!(sadf.sar.item_list(Activity::NetEdev), expected);
                    assert!(sadf.sar.list_on_cmdline(Activity::NetEdev));
                }
            }
        }
    }

    #[test]
    fn empty_and_repeated_item_filters_preserve_accumulated_items() {
        let empty = parse(&["--dev=", "--fs=", "--iface=", "--int=", "sa01"]);
        for activity in [
            Activity::Disk,
            Activity::Fs,
            Activity::NetDev,
            Activity::NetEdev,
            Activity::Irq,
        ] {
            assert!(!empty.sar.list_on_cmdline(activity));
        }
        let repeated = parse(&[
            "--iface=eth0",
            "--fs=/home",
            "sa01",
            "--",
            "--iface=eth1,eth0",
            "--iface=",
            "--fs=",
        ]);
        assert_eq!(repeated.sar.item_list(Activity::NetDev), ["eth0", "eth1"]);
        assert_eq!(repeated.sar.item_list(Activity::NetEdev), ["eth0", "eth1"]);
        assert_eq!(repeated.sar.item_list(Activity::Fs), ["/home"]);
    }

    #[test]
    fn queue_fallback_reparses_the_truncated_token_without_changing_input() {
        let args = argv(&["-d", "sa01", "--", "-q", "2,5", "3"]);
        let original = args.clone();
        let o = parse_sadf_args(&args).unwrap();
        assert_eq!(selected(&o), vec![Activity::Queue]);
        assert_eq!(o.interval, Some(2));
        assert_eq!(o.count, Some(3));
        assert_eq!(args, original);

        // 失敗前の選択は残り、先頭キーワードはファイル名として再解析される。
        let partial = parse(&["--", "-q", "CPU,invalid"]);
        assert_eq!(selected(&partial), vec![Activity::Queue, Activity::PsiCpu]);
        assert_eq!(partial.data_file, Some(PathBuf::from("CPU")));
    }

    #[test]
    fn queue_keywords_and_missing_argument_keep_their_consumption_rules() {
        for (args, expected) in [
            (vec!["--", "-q"], vec![Activity::Queue]),
            (
                vec!["sa01", "--", "-q", "-u"],
                vec![Activity::Cpu, Activity::Queue],
            ),
            (
                vec!["sa01", "--", "-q", "CPU,IO", "2", "3"],
                vec![Activity::PsiCpu, Activity::PsiIo],
            ),
        ] {
            let o = parse(&args);
            assert_eq!(selected(&o), expected, "{args:?}");
            if args.last() == Some(&"3") {
                assert_eq!(o.interval, Some(2));
                assert_eq!(o.count, Some(3));
            }
        }
    }

    #[test]
    fn sadf_j_with_fs_list() {
        // tests/00581: sadf -j --fs=... sa01 -- -F
        let o = parse(&["-j", "--fs=/dev/sda1,/home", "sa01", "--", "-F"]);
        assert_eq!(o.format, Some(SadfFormat::Json));
        assert_eq!(o.sar.item_list(Activity::Fs), ["/dev/sda1", "/home"]);
        assert_eq!(o.sar.opt_flags(Activity::Fs), OptFlags::FILESYSTEM);
    }

    #[test]
    fn sadf_x_with_fs_mount() {
        // tests/00582: sadf -x --fs=... sa01 -- -F MOUNT
        let o = parse(&["-x", "--fs=/dev/sda1,/home", "sa01", "--", "-F", "MOUNT"]);
        assert_eq!(o.sar.opt_flags(Activity::Fs), OptFlags::MOUNT);
    }

    #[test]
    fn sadf_positional_interval_after_dash_dash() {
        // tests/00585: sadf -d --iface=eth0 sa01 -- -n DEV 65
        let o = parse(&["-d", "--iface=eth0", "sa01", "--", "-n", "DEV", "65"]);
        assert_eq!(o.interval, Some(65));
        assert_eq!(o.count, None);
    }

    #[test]
    fn sadf_epoch_time_range() {
        // tests/01960: sadf -d sa01 -U -s ... -e ...
        let o = parse(&["-d", "sa01", "-U", "-s", "1555593629", "-e", "1555594649"]);
        assert_eq!(o.sar.tm_start, TimeSpec::Epoch(1_555_593_629));
        assert_eq!(o.sar.tm_end, TimeSpec::Epoch(1_555_594_649));
        assert_eq!(o.time_base(), SadfTimeBase::SecEpoch);
    }

    #[test]
    fn sadf_mixed_time_range() {
        // tests/01977: sadf -d sa01 -s 13:20:19 -e 1555595649
        let o = parse(&["-d", "sa01", "-s", "13:20:19", "-e", "1555595649"]);
        assert_eq!(
            o.sar.tm_start,
            TimeSpec::HhMmSs {
                hour: 13,
                min: 20,
                sec: 19
            }
        );
        assert_eq!(o.sar.tm_end, TimeSpec::Epoch(1_555_595_649));
    }

    #[test]
    fn sadf_epoch_end_before_start_is_an_error() {
        assert_eq!(
            parse_err(&["-d", "sa01", "-s", "1555595649", "-e", "1555593629"]),
            SadfArgError::Sar(SarArgError::EndBeforeStart)
        );
    }

    #[test]
    fn sadf_v_can_be_bundled() {
        let o = parse(&["-CV"]);
        assert_eq!(o.immediate, Some(SadfImmediate::Version));
        assert!(o.sar.flags.comment);
    }

    // ---------------------------------------------------------------
    // positional
    // ---------------------------------------------------------------

    #[test]
    fn all_digit_data_file_is_taken_as_interval() {
        // sadf 20240101 は interval=20240101 扱い
        let o = parse(&["20240101"]);
        assert_eq!(o.data_file, None);
        assert_eq!(o.interval, Some(20_240_101));
    }

    #[test]
    fn interval_below_one_is_rejected() {
        assert!(matches!(
            parse_err(&["sa01", "0"]),
            SadfArgError::InvalidInterval { .. }
        ));
    }

    #[test]
    fn count_zero_means_all_records() {
        let o = parse(&["sa01", "1", "0"]);
        assert_eq!(o.interval, Some(1));
        assert_eq!(o.count, None);
    }

    #[test]
    fn duplicate_count_is_rejected() {
        assert!(matches!(
            parse_err(&["sa01", "1", "2", "3"]),
            SadfArgError::DuplicateCount { .. }
        ));
    }

    #[test]
    fn duplicate_data_file_is_rejected() {
        assert!(matches!(
            parse_err(&["sa01", "sa02"]),
            SadfArgError::DuplicateDataFile { .. }
        ));
        assert!(matches!(
            parse_err(&["-3", "sa01"]),
            SadfArgError::DuplicateDataFile { .. }
        ));
    }

    #[test]
    fn day_offset() {
        let o = parse(&["-d", "-3"]);
        assert_eq!(o.day_offset, 3);
        assert_eq!(o.data_file, None);
    }
}

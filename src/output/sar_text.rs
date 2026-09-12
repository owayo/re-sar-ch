//! `sar` 互換のテキストレポート出力。
//!
//! 典拠は [`docs/format/03-output-format.md`] (sysstat v12.8.0)。
//! 第 II 部 (行構造)、第 III 部 §3〜§9 (ヘッダ生成器・`cprintf_*`・activity 別書式)、
//! 第 IV 部 (タイムスタンプ・単位・`--pretty`) をそのまま写している。
//!
//! ## この層がやること / やらないこと
//!
//! **値の計算はしない。** レートも派生列も固定小数のスケーリングも
//! [`crate::series::compute`] が済ませた結果を受け取り、`printf` 相当の書式に
//! 流し込むだけ。例外は `--human` の単位換算 (`cprintf_unit()`) と、
//! 単位を持つ列の「非 human 時の表示単位への換算」で、これは本家も
//! `pr_stats.c` 側で分岐している**表示**処理である (03 §1.8.1)。
//!
//! ## 出力の骨格
//!
//! ```text
//! Linux <release> (<nodename>) \t<date> \t_<machine>_\t(<n> CPU)   ← バナー 1 行
//!                                                                 ← ヘッダ行の先頭 \n による空行
//! <前サンプル時刻> <列見出し…>                                    ← ヘッダ行
//! <現サンプル時刻> <値…>                                          ← データ行
//! Average:    <値…>                                               ← 平均行 (空行なし)
//! ```
//!
//! 1 値列は**「半角空白 1 個 + 右詰め 9 桁」= 10 桁**。区切り文字は無い。
//! タイムスタンプ列は `%-11s` (左詰め 11 桁、切り詰めなし)。
//! `--dec=` も `--human` も**幅を変えない**。
//!
//! ## 使い方
//!
//! activity ごとにファイルを読み直す本家の構造をそのまま再現できるよう、
//! 「1 出力ブロック = 1 [`SarBlock`]」とし、[`SarBlock::record`] /
//! [`SarBlock::event`] を [`crate::series::walk_items`] のコールバックから呼ぶ。
//! レコードは溜めない。
//!
//! ```no_run
//! use std::io::{BufWriter, stdout};
//! use re_sar_ch::format::{SaFile, file::ScanControl};
//! use re_sar_ch::model::ActivityId;
//! use re_sar_ch::output::sar_text::{SarBlock, SarTextOptions, write_banner};
//! use re_sar_ch::series::{Selection, WalkItem, walk_items};
//!
//! # fn main() -> re_sar_ch::Result<()> {
//! let file = SaFile::open("sa01")?;
//! let opts = SarTextOptions::default();
//! let mut out = BufWriter::new(stdout());
//! write_banner(&mut out, &file)?;
//!
//! for id in [ActivityId::CPU, ActivityId::MEMORY] {
//!     for mut block in SarBlock::blocks_for(id, &opts) {
//!         walk_items(&file, &Selection::Only(vec![id]), |item| {
//!             match item {
//!                 WalkItem::Event(ev) => block.event(&mut out, &ev)?,
//!                 WalkItem::Sample(view) => block.record(&mut out, view)?,
//!             }
//!             Ok(ScanControl::Continue)
//!         })?;
//!         block.finish(&mut out)?;
//!     }
//! }
//! # Ok(())
//! # }
//! ```
//!
//! [`write_report`] は上の定型を 1 呼び出しにまとめたもの。
//!
//! ## 既知の未実装 / 差異
//!
//! | 項目 | 状況 |
//! |---|---|
//! | `-x` の `Minimum:` / `Maximum:` 行 | 未実装。`Summary:` / `Last:` のラベル切り替えだけ反映する |
//! | `A_DISK` のデバイス名 | 既定は `dev<major>-<minor>`。**ローカルの `/sys` は引かない** (他ホストのファイルで誤名になる)。`-j SID` 相当の WWN 名だけ再現する |
//! | `Average:` の `avg_count` | 本家は activity 単位のグローバルカウンタだが、ここでは item 単位に数える。途中で現れた / 消えたデバイスで本家と値が変わり得る (常時存在する item では一致) |
//! | ヘッダ再表示 | パイプ出力と同じ `rows = 86400` 相当 (ブロック先頭で 1 回)。端末幅による再表示は行わない |
//!
//! [`docs/format/03-output-format.md`]: ../../../docs/format/03-output-format.md

use std::io::{self, Write};

use crate::format::SaFile;
use crate::layout::plan::DecodePlan;
use crate::layout::registry::{ActivityDef, lookup};
use crate::model::{ActivityId, Availability, ValueKind};
use crate::series::compute::{
    self, ComputeContext, ComputeIssue, Computed, ItemAccum, bat_col, bat_status, cpu_col,
    disk_col, fan_col, freq_col, fs_col, huge_col, in_col, irq_col, mem_col, net_dev_col, psi_col,
    pwr_cpu_col, queue_col, soft_col, temp_col, usb_col,
};
use crate::series::delta::interval_cs;
use crate::series::snapshot::{IntervalView, ItemSnapshot, RecordEvent, Snapshot};

// ============================================================================
// 定数
// ============================================================================

/// 値列の幅 (`wi`)。`pr_stats.c` の全 `cprintf_*` 呼び出しで 9。
const VW: usize = 9;

/// タイムスタンプ列の幅 (`%-11s`)。
const TSW: usize = 11;

/// `A_PWR_USB` の manufacturer 列幅 (`MAX_MANUF_LEN - 1`)。
const MANUF_W: usize = 23;

/// `--human` の単位文字 (`common.c` の `units[]`)。
const UNIT_CHARS: [char; 8] = ['s', 'B', 'k', 'M', 'G', 'T', 'P', '?'];

/// 計算できなかった値のマーカー。
///
/// **0 を出さない。** 「正常に 0」と「計算できなかった」を混同させないため、
/// 幅の中に `?` を右詰めする。ファイル世代にフィールドが無いだけの列は
/// 本家と同じく 0 として出す ([`ComputeIssue::UnsupportedBySource`])。
const UNKNOWN: &str = "?";

// ============================================================================
// オプション
// ============================================================================

/// タイムスタンプの基準系。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TimeStyle {
    /// 読み手のローカル時刻 (`sar` の既定 = `S_F_LOCAL_TIME`)。
    #[default]
    Local,
    /// UTC (`sadf` の既定)。
    Utc,
    /// レコードに焼き込まれた時分秒 (`-t` = `S_F_TRUE_TIME`)。
    ///
    /// 記録側ホストのローカル時刻。`TZ` の再設定は行わない (03 第 IV 部 §1.5)。
    Recorded,
    /// epoch 秒 (`sadf -U`)。`sar` には無いが対称性のため持つ。
    Epoch,
}

/// `-P` で選択された CPU。
///
/// 本家のビットマップと同じく **bit 0 = 集約行 (`all`)、CPU `n` = bit `n + 1`**。
/// このビットマップは `A_CPU` / `A_IRQ` / `A_PWR_CPU` / `A_PWR_FREQ` /
/// `A_NET_SOFT` の 5 つで共有される。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum CpuSelection {
    /// 集約行のみ (`sar` 既定 / `-P all`)。
    #[default]
    Aggregate,
    /// 集約行 + 全 CPU (`-P ALL`)。
    All,
    /// 明示指定 (`-P 0,2-3`)。
    Listed {
        /// 集約行を含むか。
        aggregate: bool,
        /// CPU 番号 (0 起点)。
        cpus: Vec<usize>,
    },
}

impl CpuSelection {
    /// item 添字 (`0` = 集約行、`n` = CPU `n-1`) が選択されているか。
    fn includes(&self, item_index: usize) -> bool {
        match self {
            CpuSelection::Aggregate => item_index == 0,
            CpuSelection::All => true,
            CpuSelection::Listed { aggregate, cpus } => {
                if item_index == 0 {
                    *aggregate
                } else {
                    cpus.contains(&(item_index - 1))
                }
            }
        }
    }
}

/// テキスト出力のオプション。
///
/// `cli::sar_args::SarOptions` から必要な項目だけを写して渡す
/// (出力層が CLI 解析結果の型に依存しないようにしている)。
#[derive(Debug, Clone, Default)]
pub struct SarTextOptions {
    /// `-p` / `--pretty` / `-h` / `-j` — アイテム名列を行末へ移す。
    pub pretty: bool,
    /// `--human` / `-h` — 単位付き表示 (`cprintf_unit`)。
    pub human: bool,
    /// `--dec={0|1|2}`。`None` = 未指定 (`dplaces_nr = -1`)。
    pub dec_places: Option<u8>,
    /// `-C` — `COM` 行を表示する。
    pub comment: bool,
    /// `-x` — 平均ブロックのヘッダラベルを `Summary:` にし、`A_FS` は `Last:` を使う。
    pub minmax: bool,
    /// `-z` — 前サンプルと同一のアイテム行を省略する。
    pub zero_omit: bool,
    /// `-u ALL` — `A_CPU` の 10 列版。
    pub cpu_all: bool,
    /// `-r` — `A_MEMORY` の RAM ブロック (`-r` / `-S` 両方偽なら `-r` 扱い)。
    pub memory: bool,
    /// `-r ALL` — `A_MEMORY` の拡張列 (`kbanonpg` 以降)。
    pub mem_all: bool,
    /// `-S` — `A_MEMORY` の swap ブロック。
    pub swap: bool,
    /// `-F MOUNT` — `A_FS` のアイテム名をマウントポイントにする。
    pub mount: bool,
    /// `-j SID` — `A_DISK` のデバイス名を WWN 由来の安定 ID にする。
    pub dev_sid: bool,
    /// タイムスタンプの基準系。
    pub time: TimeStyle,
    /// `-P` の CPU 選択。
    pub cpus: CpuSelection,
}

impl SarTextOptions {
    /// `A_MEMORY` のブロック選択。どちらも未指定なら `-r` 相当。
    fn memory_blocks(&self) -> (bool, bool) {
        if !self.memory && !self.swap {
            (true, false)
        } else {
            (self.memory, self.swap)
        }
    }

    /// `cprintf_f` / `cprintf_xpc` の小数桁に `--dec=` を反映する。
    ///
    /// **`wd == 0` の列には効かない** (本家の条件が `wd > 0`)。幅は変えない。
    fn decimals(&self, wd: u8) -> usize {
        match self.dec_places {
            Some(d) if wd > 0 => d as usize,
            _ => wd as usize,
        }
    }

    /// `cprintf_unit()` の小数桁。`dplaces_nr == 0` のときだけ 0 桁で、
    /// **未指定 (-1) でも 1 桁**になる (03 §1.8 / §4.6)。
    fn unit_decimals(&self) -> usize {
        match self.dec_places {
            Some(0) => 0,
            _ => 1,
        }
    }
}

// ============================================================================
// printf プリミティブ
// ============================================================================

/// C の `printf("%*s", width, s)` 相当。
///
/// **パディングはバイト単位**。Rust の `{:>w$}` は文字数基準なので、
/// マルチバイト文字 (`A_PWR_BAT` の矢印など) を含む列でずれる (03 §1.7.7)。
fn pad_left(s: &str, width: usize) -> String {
    let n = s.len();
    if n >= width {
        return s.to_string();
    }
    let mut out = String::with_capacity(width);
    for _ in 0..width - n {
        out.push(' ');
    }
    out.push_str(s);
    out
}

/// C の `printf("%-*s", width, s)` 相当 (バイト単位パディング)。
fn pad_right(s: &str, width: usize) -> String {
    let n = s.len();
    let mut out = String::with_capacity(width.max(n));
    out.push_str(s);
    for _ in n..width {
        out.push(' ');
    }
    out
}

/// `" %*.*f"` / `" %+*.*f"` 相当。
fn fmt_float(v: f64, wi: usize, wd: usize, sign: bool) -> String {
    let body = if sign {
        format!("{v:+.wd$}")
    } else {
        format!("{v:.wd$}")
    };
    format!(" {}", pad_left(&body, wi))
}

/// `" %*"PRIu64` 相当。
fn fmt_u64(v: u64, wi: usize) -> String {
    format!(" {}", pad_left(&v.to_string(), wi))
}

/// `" %*x"` 相当 (小文字 16 進、`0x` なし)。
fn fmt_hex(v: u64, wi: usize) -> String {
    format!(" {}", pad_left(&format!("{v:x}"), wi))
}

/// `cprintf_xpc()` 相当 — パーセント値。
///
/// `--human` 時は `wi -= 1` して `%` の 1 桁分を確保し、`wd > 1` なら `wd -= 1`。
/// **総幅は `1 + wi` のまま変わらない** (03 §4.5)。
fn fmt_percent(v: f64, wd: usize, human: bool) -> String {
    let mut wi = VW;
    let mut wd = wd;
    if human {
        if wi < 4 {
            wi = 4;
        }
        wi -= 1;
        if wd > 1 {
            wd -= 1;
        }
    }
    let body = format!("{v:.wd$}");
    let mut s = format!(" {}", pad_left(&body, wi));
    if human {
        s.push('%');
    }
    s
}

/// `--human` で値に付ける単位 (`common.h` の `UNIT_*`)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SarUnit {
    /// `NO_UNIT` — 単位を持たない列。
    None,
    /// `UNIT_BYTE` — `A_NET_DEV` / `A_FS`。
    Byte,
    /// `UNIT_KILOBYTE` — `A_MEMORY` / `A_HUGE` / `A_DISK`。
    Kilobyte,
}

impl SarUnit {
    /// `units[]` の添字。
    ///
    /// `UNIT_SECTOR`(0) は iostat 専用で sar からは一度も渡らないため、
    /// `units[0] = 's'` が出力に現れることはない (03 §1.8.1 落とし穴 2)。
    fn index(self) -> usize {
        match self {
            SarUnit::None => 0,
            SarUnit::Byte => 1,
            SarUnit::Kilobyte => 2,
        }
    }
}

/// `cprintf_unit()` 相当。
///
/// **分母は 1024** (1000 ではない)。ステップアップ閾値は `>= 1024`。
/// 数値部の幅は `wi - 1` で、単位 1 文字を空白なしで連結するため総幅は `1 + wi`。
fn fmt_unit(mut v: f64, unit: SarUnit, wd: usize) -> String {
    let wi = VW.max(4);
    let mut idx = unit.index();
    while v >= 1024.0 {
        v /= 1024.0;
        idx += 1;
    }
    let body = format!("{v:.wd$}");
    let ch = UNIT_CHARS[idx.min(UNIT_CHARS.len() - 1)];
    format!(" {}{}", pad_left(&body, wi - 1), ch)
}

// ============================================================================
// セル書式
// ============================================================================

/// 1 値列の書式 (`cprintf_*` のどれを使うか)。
#[derive(Debug, Clone, Copy, PartialEq)]
enum Cell {
    /// `cprintf_f(NO_UNIT, sign, .., 9, wd)`
    Float { wd: u8, sign: bool },
    /// `cprintf_u64(NO_UNIT, .., 9)` — `--dec` の影響を受けない
    Int,
    /// `cprintf_x(.., 9)`
    Hex,
    /// `cprintf_xpc(human, .., 9, wd)`
    Percent { wd: u8 },
    /// `cprintf_f(unit, FALSE, .., 9, wd)` — `--human` 時のみ単位付き
    FloatUnit {
        wd: u8,
        unit: SarUnit,
        /// 非 human 時に列の表示単位へ換算する除数 (`A_NET_DEV` は 1024 等)。
        plain_div: f64,
    },
    /// `cprintf_u64(unit, .., 9)`
    IntUnit { unit: SarUnit, plain_div: f64 },
    /// `A_PWR_BAT` の `status` 列 (矢印 or `?`)。
    BatStatus,
}

impl Cell {
    const F2: Cell = Cell::Float { wd: 2, sign: false };
    const F0: Cell = Cell::Float { wd: 0, sign: false };
    const PC2: Cell = Cell::Percent { wd: 2 };
    /// `A_MEMORY` / `A_HUGE` の kB 列 (瞬時値 = 整数)。
    const KB: Cell = Cell::IntUnit {
        unit: SarUnit::Kilobyte,
        plain_div: 1.0,
    };
    /// 同 kB 列の平均値 (`cprintf_f(unit, .., 9, 0)` = 小数 0 桁)。
    const KB_AVG: Cell = Cell::FloatUnit {
        wd: 0,
        unit: SarUnit::Kilobyte,
        plain_div: 1.0,
    };
    /// `A_DISK` の kB/s 列 (計算層が既に kB/s に換算済み)。
    const KB_PER_SEC: Cell = Cell::FloatUnit {
        wd: 2,
        unit: SarUnit::Kilobyte,
        plain_div: 1.0,
    };
    /// `A_NET_DEV` の `rxkB/s` / `txkB/s`。
    ///
    /// 計算層は**バイト毎秒**を返す。非 human では 1024 で割って kB/s にし、
    /// `--human` では生のバイト毎秒を `cprintf_unit(UNIT_BYTE, …)` に渡す
    /// (03 §1.8.1 落とし穴 1)。
    const BYTES_PER_SEC: Cell = Cell::FloatUnit {
        wd: 2,
        unit: SarUnit::Byte,
        plain_div: 1024.0,
    };
    /// `A_FS` の `MBfsfree` / `MBfsused`。バイトから MB へ、小数 0 桁。
    const MB_FROM_BYTES: Cell = Cell::FloatUnit {
        wd: 0,
        unit: SarUnit::Byte,
        plain_div: 1024.0 * 1024.0,
    };

    /// 値を 1 セル分書式化する。
    fn render(self, value: Computed, opts: &SarTextOptions) -> String {
        // その世代のファイルに無いフィールドは本家と同じく 0 埋め扱い
        let v = match value {
            Ok(v) => v,
            Err(ComputeIssue::UnsupportedBySource) => 0.0,
            Err(_) => return format!(" {}", pad_left(UNKNOWN, VW)),
        };
        match self {
            Cell::Float { wd, sign } => fmt_float(v, VW, opts.decimals(wd), sign),
            Cell::Int => fmt_u64(to_u64(v), VW),
            Cell::Hex => fmt_hex(to_u64(v), VW),
            Cell::Percent { wd } => fmt_percent(v, opts.decimals(wd), opts.human),
            Cell::FloatUnit {
                wd,
                unit,
                plain_div,
            } => {
                if opts.human {
                    fmt_unit(v, unit, opts.unit_decimals())
                } else {
                    fmt_float(v / plain_div, VW, opts.decimals(wd), false)
                }
            }
            Cell::IntUnit { unit, plain_div } => {
                if opts.human {
                    fmt_unit(v, unit, opts.unit_decimals())
                } else {
                    fmt_u64(to_u64(v / plain_div), VW)
                }
            }
            Cell::BatStatus => bat_status_cell(to_u64(v)),
        }
    }
}

/// 整数列へ流し込む前の丸め。負値は 0 に潰す (`u64` 書式なので)。
fn to_u64(v: f64) -> u64 {
    if v <= 0.0 { 0 } else { v as u64 }
}

/// `A_PWR_BAT` の `status` 列。
///
/// 矢印は UTF-8 で 3 バイトあり、本家は `" %11s"` で出す。
/// **バイト幅でパディングすると見た目が `?` の `" %9s"` と揃う**設計なので、
/// 文字数基準でパディングしてはいけない (03 §1.7.7 / §id=43)。
fn bat_status_cell(status: u64) -> String {
    let (glyph, width) = match status {
        bat_status::CHARGING => ("\u{2197}", 11),
        bat_status::DISCHARGING => ("\u{2198}", 11),
        bat_status::NOTCHARGING => ("\u{2192}", 11),
        bat_status::FULL => ("\u{2191}", 11),
        // BAT_STS_UNKNOWN など。**幅が 9 になる**
        _ => ("?", 9),
    };
    format!(" {}", pad_left(glyph, width))
}

// ============================================================================
// activity ビュー (列レイアウト)
// ============================================================================

/// `Average:` の求め方 (03 §8.1)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AvgKind {
    /// 方式 A — 最初と最後のサンプルの差を全期間 itv で割り直す。
    Rate,
    /// 方式 B — 表示値の累積平均 (`合計 / avg_count`)。
    Mean,
    /// 方式 B′ — 生フィールドの累積平均から比率を再計算する。
    MeanRatio,
    /// 方式 C — 最後に観測した値をそのまま再掲する (`A_FS` / `A_PWR_USB`)。
    Last,
}

/// 平均行のラベル。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AvgLabel {
    Average,
    /// `A_PWR_USB`
    Summary,
    /// `A_FS` — `-x` 併用時のみ `Last:`
    SummaryOrLast,
}

/// 行頭のアイテム名列 (03 §11-15)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HeadItem {
    None,
    /// `" %s"` + `"    all"` / `" %7d"` — `A_CPU` / `A_NET_SOFT` (8 桁)
    Cpu7,
    /// `"%s"` + `"     all"` / `"     %3d"` — `A_PWR_CPU` / `A_PWR_FREQ`
    /// (**先頭空白なし**の 8 桁)
    Cpu3,
    /// `" %9s"` — 非 pretty のアイテム名 (pretty では [`TailItem::Name`] へ回る)
    Name9,
    /// `"       %3d"` — `A_SERIAL` の回線番号 (10 桁)
    Line3,
    /// `"     %5d"` — `A_PWR_FAN` / `A_PWR_TEMP` / `A_PWR_IN` / `A_PWR_BAT` (10 桁)
    Index5,
    /// `"  %6d"` — `A_PWR_USB` のバス番号 (8 桁)
    Bus6,
}

/// 行末のアイテム名列 (幅指定なし)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TailItem {
    None,
    /// `" %s"` — デバイス名 / インターフェース名 / FS 名 / 割り込み名 / センサの `DEVICE`
    Name,
    /// `" %-23s"` + `" %s"` — `A_PWR_USB` の manufacturer / product
    UsbNames,
}

/// item の並べ方。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layout {
    /// 単一 item。
    Single,
    /// item 列 (デバイス・インターフェース・センサ)。
    List,
    /// CPU 列。item 添字 0 = 集約行。`-P` の選択に従う。
    Cpu,
    /// `A_IRQ` — 行 = 割り込み、列 = CPU。
    IrqMatrix,
    /// `A_PWR_FREQ` — CPU ごとに `nr2` 個の周波数スロット。
    FreqMatrix,
}

/// ヘッダ行の再表示方針 (`dish`、03 §9.3)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HeaderPolicy {
    /// ブロック先頭で 1 回だけ (パイプ出力の既定 = `rows = 86400`)。
    Once,
    /// サンプルごとに毎回 (`A_IRQ`、および `-z` 指定時の一部 activity)。
    EverySample,
}

/// 1 値列の指定。
#[derive(Debug, Clone, Copy)]
struct ColSpec {
    /// `ActivityDef::columns` の添字。
    col: usize,
    /// 瞬時値の書式。
    cell: Cell,
    /// `Average:` 行の書式。
    avg_cell: Cell,
    /// `Average:` の求め方。
    avg: AvgKind,
    /// 平均行にこの列を出すか (`A_PWR_BAT` の `status` は出さない)。
    in_average: bool,
}

impl ColSpec {
    const fn rate(col: usize, cell: Cell) -> Self {
        Self {
            col,
            cell,
            avg_cell: cell,
            avg: AvgKind::Rate,
            in_average: true,
        }
    }
    const fn mean(col: usize, cell: Cell, avg_cell: Cell) -> Self {
        Self {
            col,
            cell,
            avg_cell,
            avg: AvgKind::Mean,
            in_average: true,
        }
    }
    const fn ratio(col: usize, cell: Cell, avg_cell: Cell) -> Self {
        Self {
            col,
            cell,
            avg_cell,
            avg: AvgKind::MeanRatio,
            in_average: true,
        }
    }
    const fn last(col: usize, cell: Cell) -> Self {
        Self {
            col,
            cell,
            avg_cell: cell,
            avg: AvgKind::Last,
            in_average: true,
        }
    }
    const fn instant_only(col: usize, cell: Cell) -> Self {
        Self {
            col,
            cell,
            avg_cell: cell,
            avg: AvgKind::Last,
            in_average: false,
        }
    }
}

/// 1 出力ブロックの列レイアウト。
#[derive(Debug, Clone)]
struct View {
    id: ActivityId,
    /// `activity.hdr_line` (メタ文字を含む本家の文字列そのまま)。
    hdr_line: &'static str,
    /// `print_hdr_line()` の `pos` (複数出力の切り替え)。
    pos: usize,
    /// `print_hdr_line()` の `iwidth`。
    iwidth: i32,
    /// `&` 以降の拡張列を出すか。
    ext: bool,
    head: HeadItem,
    tail: TailItem,
    layout: Layout,
    header: HeaderPolicy,
    avg_label: AvgLabel,
    cells: Vec<ColSpec>,
}

/// `S(...)` だけの activity 用に `cells` を連番生成する。
fn rate_cells(range: std::ops::Range<usize>) -> Vec<ColSpec> {
    range.map(|c| ColSpec::rate(c, Cell::F2)).collect()
}

/// ゲージ整数列 (瞬時値 `%9llu` / 平均 `%9.0f`) を連番生成する。
fn gauge_int_cells(range: std::ops::Range<usize>) -> Vec<ColSpec> {
    range
        .map(|c| ColSpec::mean(c, Cell::Int, Cell::F0))
        .collect()
}

/// `-z` を付けるとサンプルごとにヘッダを出す activity 群 (03 §9.3 の E / F 群)。
fn zero_omit_policy(opts: &SarTextOptions) -> HeaderPolicy {
    if opts.zero_omit {
        HeaderPolicy::EverySample
    } else {
        HeaderPolicy::Once
    }
}

/// `A_MEMORY` の `hdr_line` (RAM ブロック `|` swap ブロック)。
const MEMORY_HDR: &str = "kbmemfree;kbavail;kbmemused;%memused;kbbuffers;kbcached;kbcommit;%commit;kbactive;kbinact;kbdirty;kbshmem&kbanonpg;kbslab;kbkstack;kbpgtbl;kbvmused|kbswpfree;kbswpused;%swpused;kbswpcad;%swpcad";

impl View {
    /// activity の出力ブロックを列挙する。
    ///
    /// `AO_MULTIPLE_OUTPUTS` の activity は複数ブロックになる
    /// (`A_MEMORY` の RAM / swap)。
    fn all_for(id: ActivityId, opts: &SarTextOptions) -> Vec<View> {
        match id {
            ActivityId::CPU => vec![Self::cpu(opts)],
            ActivityId::PCSW => vec![Self::simple(id, "proc/s;cswch/s", rate_cells(0..2))],
            ActivityId::IRQ => vec![Self::irq(opts)],
            ActivityId::SWAP => vec![Self::simple(id, "pswpin/s;pswpout/s", rate_cells(0..2))],
            ActivityId::PAGE => vec![Self::simple(
                id,
                "pgpgin/s;pgpgout/s;fault/s;majflt/s;pgfree/s;pgscank/s;pgscand/s;pgsteal/s;pgprom/s;pgdem/s",
                rate_cells(0..10),
            )],
            ActivityId::IO => vec![Self::simple(
                id,
                "tps;rtps;wtps;dtps;bread/s;bwrtn/s;bdscd/s",
                rate_cells(0..7),
            )],
            ActivityId::MEMORY => Self::memory(opts),
            ActivityId::KTABLES => vec![Self::simple(
                id,
                "dentunusd;file-nr;inode-nr;pty-nr",
                gauge_int_cells(0..4),
            )],
            ActivityId::QUEUE => vec![Self::queue()],
            ActivityId::SERIAL => vec![Self::serial(opts)],
            ActivityId::DISK => vec![Self::disk(opts)],
            ActivityId::NET_DEV => vec![Self::net_dev(opts)],
            ActivityId::NET_EDEV => vec![Self::net_edev(opts)],
            ActivityId::NET_NFS => vec![Self::simple(
                id,
                "call/s;retrans/s;read/s;write/s;access/s;getatt/s",
                rate_cells(0..6),
            )],
            ActivityId::NET_NFSD => vec![Self::simple(
                id,
                "scall/s;badcall/s;packet/s;udp/s;tcp/s;hit/s;miss/s;sread/s;swrite/s;saccess/s;sgetatt/s",
                rate_cells(0..11),
            )],
            ActivityId::NET_SOCK => vec![Self::simple(
                id,
                "totsck;tcpsck;udpsck;rawsck;ip-frag;tcp-tw",
                gauge_int_cells(0..6),
            )],
            ActivityId::NET_IP => vec![Self::simple(
                id,
                "irec/s;fwddgm/s;idel/s;orq/s;asmrq/s;asmok/s;fragok/s;fragcrt/s",
                rate_cells(0..8),
            )],
            ActivityId::NET_EIP => vec![Self::simple(
                id,
                "ihdrerr/s;iadrerr/s;iukwnpr/s;idisc/s;odisc/s;onort/s;asmf/s;fragf/s",
                rate_cells(0..8),
            )],
            ActivityId::NET_ICMP => vec![Self::simple(
                id,
                "imsg/s;omsg/s;iech/s;iechr/s;oech/s;oechr/s;itm/s;itmr/s;otm/s;otmr/s;iadrmk/s;iadrmkr/s;oadrmk/s;oadrmkr/s",
                rate_cells(0..14),
            )],
            ActivityId::NET_EICMP => vec![Self::simple(
                id,
                "ierr/s;oerr/s;idstunr/s;odstunr/s;itmex/s;otmex/s;iparmpb/s;oparmpb/s;isrcq/s;osrcq/s;iredir/s;oredir/s",
                rate_cells(0..12),
            )],
            ActivityId::NET_TCP => vec![Self::simple(
                id,
                "active/s;passive/s;iseg/s;oseg/s",
                rate_cells(0..4),
            )],
            ActivityId::NET_ETCP => vec![Self::simple(
                id,
                "atmptf/s;estres/s;retrseg/s;isegerr/s;orsts/s",
                rate_cells(0..5),
            )],
            ActivityId::NET_UDP => vec![Self::simple(
                id,
                "idgm/s;odgm/s;noport/s;idgmerr/s",
                rate_cells(0..4),
            )],
            ActivityId::NET_SOCK6 => vec![Self::simple(
                id,
                "tcp6sck;udp6sck;raw6sck;ip6-frag",
                gauge_int_cells(0..4),
            )],
            ActivityId::NET_IP6 => vec![Self::simple(
                id,
                "irec6/s;fwddgm6/s;idel6/s;orq6/s;asmrq6/s;asmok6/s;imcpck6/s;omcpck6/s;fragok6/s;fragcr6/s",
                rate_cells(0..10),
            )],
            ActivityId::NET_EIP6 => vec![Self::simple(
                id,
                "ihdrer6/s;iadrer6/s;iukwnp6/s;i2big6/s;idisc6/s;odisc6/s;inort6/s;onort6/s;asmf6/s;fragf6/s;itrpck6/s",
                rate_cells(0..11),
            )],
            ActivityId::NET_ICMP6 => vec![Self::simple(
                id,
                "imsg6/s;omsg6/s;iech6/s;iechr6/s;oechr6/s;igmbq6/s;igmbr6/s;ogmbr6/s;igmbrd6/s;ogmbrd6/s;irtsol6/s;ortsol6/s;irtad6/s;inbsol6/s;onbsol6/s;inbad6/s;onbad6/s",
                rate_cells(0..17),
            )],
            ActivityId::NET_EICMP6 => vec![Self::simple(
                id,
                "ierr6/s;idtunr6/s;odtunr6/s;itmex6/s;otmex6/s;iprmpb6/s;oprmpb6/s;iredir6/s;oredir6/s;ipck2b6/s;opck2b6/s",
                rate_cells(0..11),
            )],
            ActivityId::NET_UDP6 => vec![Self::simple(
                id,
                "idgm6/s;odgm6/s;noport6/s;idgmer6/s",
                rate_cells(0..4),
            )],
            ActivityId::PWR_CPU => vec![Self::pwr_cpu()],
            ActivityId::PWR_FAN => vec![Self::sensor(
                id,
                "FAN;DEVICE;rpm;drpm",
                vec![
                    ColSpec::mean(fan_col::RPM, Cell::F2, Cell::F2),
                    ColSpec::mean(fan_col::DRPM, Cell::F2, Cell::F2),
                ],
            )],
            ActivityId::PWR_TEMP => vec![Self::sensor(
                id,
                "TEMP;DEVICE;degC;%temp",
                vec![
                    ColSpec::mean(temp_col::DEGC, Cell::F2, Cell::F2),
                    ColSpec::ratio(temp_col::PCT, Cell::PC2, Cell::PC2),
                ],
            )],
            ActivityId::PWR_IN => vec![Self::sensor(
                id,
                "IN;DEVICE;inV;%in",
                vec![
                    ColSpec::mean(in_col::VOLTS, Cell::F2, Cell::F2),
                    ColSpec::ratio(in_col::PCT, Cell::PC2, Cell::PC2),
                ],
            )],
            ActivityId::HUGE => vec![Self::huge()],
            ActivityId::PWR_FREQ => vec![Self::pwr_freq()],
            ActivityId::PWR_USB => vec![Self::usb()],
            ActivityId::FS => vec![Self::fs(opts)],
            ActivityId::NET_FC => vec![Self::net_fc()],
            ActivityId::NET_SOFT => vec![Self::net_soft(opts)],
            ActivityId::PSI_CPU => vec![Self::psi(id, "%scpu-10;%scpu-60;%scpu-300;%scpu", false)],
            ActivityId::PSI_IO => vec![Self::psi(
                id,
                "%sio-10;%sio-60;%sio-300;%sio;%fio-10;%fio-60;%fio-300;%fio",
                true,
            )],
            ActivityId::PSI_MEM => vec![Self::psi(
                id,
                "%smem-10;%smem-60;%smem-300;%smem;%fmem-10;%fmem-60;%fmem-300;%fmem",
                true,
            )],
            ActivityId::PWR_BAT => vec![Self::bat()],
            _ => Vec::new(),
        }
    }

    /// アイテム名列を持たない単一 item の activity。
    fn simple(id: ActivityId, hdr_line: &'static str, cells: Vec<ColSpec>) -> View {
        View {
            id,
            hdr_line,
            pos: 0,
            iwidth: 0,
            ext: false,
            head: HeadItem::None,
            tail: TailItem::None,
            layout: Layout::Single,
            header: HeaderPolicy::Once,
            avg_label: AvgLabel::Average,
            cells,
        }
    }

    fn cpu(opts: &SarTextOptions) -> View {
        let cols: Vec<usize> = if opts.cpu_all {
            vec![
                cpu_col::USR,
                cpu_col::NICE_EXCL_GNICE,
                cpu_col::SYS,
                cpu_col::IOWAIT,
                cpu_col::STEAL,
                cpu_col::IRQ,
                cpu_col::SOFT,
                cpu_col::GUEST,
                cpu_col::GNICE,
                cpu_col::IDLE,
            ]
        } else {
            vec![
                cpu_col::USER,
                cpu_col::NICE,
                cpu_col::SYSTEM,
                cpu_col::IOWAIT,
                cpu_col::STEAL,
                cpu_col::IDLE,
            ]
        };
        View {
            id: ActivityId::CPU,
            hdr_line: "CPU;%user;%nice;%system;%iowait;%steal;%idle|CPU;%usr;%nice;%sys;%iowait;%steal;%irq;%soft;%guest;%gnice;%idle",
            pos: usize::from(opts.cpu_all),
            iwidth: 7,
            ext: false,
            head: HeadItem::Cpu7,
            tail: TailItem::None,
            layout: Layout::Cpu,
            header: HeaderPolicy::Once,
            avg_label: AvgLabel::Average,
            cells: cols
                .into_iter()
                .map(|c| ColSpec::rate(c, Cell::PC2))
                .collect(),
        }
    }

    fn irq(opts: &SarTextOptions) -> View {
        View {
            id: ActivityId::IRQ,
            hdr_line: "INTR;CPU*",
            pos: 0,
            iwidth: if opts.pretty { -1 } else { 0 },
            ext: false,
            head: if opts.pretty {
                HeadItem::None
            } else {
                HeadItem::Name9
            },
            tail: if opts.pretty {
                TailItem::Name
            } else {
                TailItem::None
            },
            layout: Layout::IrqMatrix,
            // CPU の online/offline で列構成が変わり得るため `dish` を見ずに毎回出す
            header: HeaderPolicy::EverySample,
            avg_label: AvgLabel::Average,
            cells: vec![ColSpec::rate(irq_col::COUNT, Cell::F2)],
        }
    }

    fn memory(opts: &SarTextOptions) -> Vec<View> {
        let (ram, swap) = opts.memory_blocks();
        let mut out = Vec::new();
        if ram {
            let mut cells = vec![
                ColSpec::mean(mem_col::KBMEMFREE, Cell::KB, Cell::KB_AVG),
                ColSpec::mean(mem_col::KBAVAIL, Cell::KB, Cell::KB_AVG),
                ColSpec::ratio(mem_col::KBMEMUSED, Cell::KB, Cell::KB_AVG),
                ColSpec::ratio(mem_col::MEMUSED_PCT, Cell::PC2, Cell::PC2),
                ColSpec::mean(mem_col::KBBUFFERS, Cell::KB, Cell::KB_AVG),
                ColSpec::mean(mem_col::KBCACHED, Cell::KB, Cell::KB_AVG),
                ColSpec::mean(mem_col::KBCOMMIT, Cell::KB, Cell::KB_AVG),
                ColSpec::ratio(mem_col::COMMIT_PCT, Cell::PC2, Cell::PC2),
                ColSpec::mean(mem_col::KBACTIVE, Cell::KB, Cell::KB_AVG),
                ColSpec::mean(mem_col::KBINACT, Cell::KB, Cell::KB_AVG),
                ColSpec::mean(mem_col::KBDIRTY, Cell::KB, Cell::KB_AVG),
                ColSpec::mean(mem_col::KBSHMEM, Cell::KB, Cell::KB_AVG),
            ];
            if opts.mem_all {
                for c in [
                    mem_col::KBANONPG,
                    mem_col::KBSLAB,
                    mem_col::KBKSTACK,
                    mem_col::KBPGTBL,
                    mem_col::KBVMUSED,
                ] {
                    cells.push(ColSpec::mean(c, Cell::KB, Cell::KB_AVG));
                }
            }
            out.push(View {
                id: ActivityId::MEMORY,
                hdr_line: MEMORY_HDR,
                pos: 0,
                iwidth: 0,
                ext: opts.mem_all,
                head: HeadItem::None,
                tail: TailItem::None,
                layout: Layout::Single,
                header: HeaderPolicy::Once,
                avg_label: AvgLabel::Average,
                cells,
            });
        }
        if swap {
            out.push(View {
                id: ActivityId::MEMORY,
                hdr_line: MEMORY_HDR,
                pos: 1,
                iwidth: 0,
                ext: false,
                head: HeadItem::None,
                tail: TailItem::None,
                layout: Layout::Single,
                header: HeaderPolicy::Once,
                avg_label: AvgLabel::Average,
                cells: vec![
                    ColSpec::mean(mem_col::KBSWPFREE, Cell::KB, Cell::KB_AVG),
                    ColSpec::ratio(mem_col::KBSWPUSED, Cell::KB, Cell::KB_AVG),
                    ColSpec::ratio(mem_col::SWPUSED_PCT, Cell::PC2, Cell::PC2),
                    ColSpec::mean(mem_col::KBSWPCAD, Cell::KB, Cell::KB_AVG),
                    ColSpec::ratio(mem_col::SWPCAD_PCT, Cell::PC2, Cell::PC2),
                ],
            });
        }
        out
    }

    fn queue() -> View {
        Self::simple(
            ActivityId::QUEUE,
            "runq-sz;plist-sz;ldavg-1;ldavg-5;ldavg-15;blocked",
            vec![
                ColSpec::mean(queue_col::RUNQ_SZ, Cell::Int, Cell::F0),
                ColSpec::mean(queue_col::PLIST_SZ, Cell::Int, Cell::F0),
                ColSpec::mean(queue_col::LDAVG_1, Cell::F2, Cell::F2),
                ColSpec::mean(queue_col::LDAVG_5, Cell::F2, Cell::F2),
                ColSpec::mean(queue_col::LDAVG_15, Cell::F2, Cell::F2),
                ColSpec::mean(queue_col::BLOCKED, Cell::Int, Cell::F0),
            ],
        )
    }

    fn serial(opts: &SarTextOptions) -> View {
        View {
            id: ActivityId::SERIAL,
            hdr_line: "TTY;rcvin/s;xmtin/s;framerr/s;prtyerr/s;brk/s;ovrun/s",
            pos: 0,
            iwidth: 0,
            ext: false,
            head: HeadItem::Line3,
            tail: TailItem::None,
            layout: Layout::List,
            header: zero_omit_policy(opts),
            avg_label: AvgLabel::Average,
            cells: rate_cells(1..7),
        }
    }

    fn disk(opts: &SarTextOptions) -> View {
        View {
            id: ActivityId::DISK,
            hdr_line: "DEV;tps;rkB/s;wkB/s;dkB/s;areq-sz;aqu-sz;await;%util",
            pos: 0,
            iwidth: if opts.pretty { -1 } else { 0 },
            ext: false,
            head: if opts.pretty {
                HeadItem::None
            } else {
                HeadItem::Name9
            },
            tail: if opts.pretty {
                TailItem::Name
            } else {
                TailItem::None
            },
            layout: Layout::List,
            header: zero_omit_policy(opts),
            avg_label: AvgLabel::Average,
            cells: vec![
                ColSpec::rate(disk_col::TPS, Cell::F2),
                ColSpec::rate(disk_col::RKB, Cell::KB_PER_SEC),
                ColSpec::rate(disk_col::WKB, Cell::KB_PER_SEC),
                ColSpec::rate(disk_col::DKB, Cell::KB_PER_SEC),
                ColSpec::rate(disk_col::AREQ_SZ, Cell::KB_PER_SEC),
                ColSpec::rate(disk_col::AQU_SZ, Cell::F2),
                ColSpec::rate(disk_col::AWAIT, Cell::F2),
                ColSpec::rate(disk_col::UTIL_PCT, Cell::PC2),
            ],
        }
    }

    fn net_dev(opts: &SarTextOptions) -> View {
        View {
            id: ActivityId::NET_DEV,
            hdr_line: "IFACE;rxpck/s;txpck/s;rxkB/s;txkB/s;rxcmp/s;txcmp/s;rxmcst/s;%ifutil",
            pos: 0,
            iwidth: if opts.pretty { -1 } else { 0 },
            ext: false,
            head: if opts.pretty {
                HeadItem::None
            } else {
                HeadItem::Name9
            },
            tail: if opts.pretty {
                TailItem::Name
            } else {
                TailItem::None
            },
            layout: Layout::List,
            header: zero_omit_policy(opts),
            avg_label: AvgLabel::Average,
            cells: vec![
                ColSpec::rate(net_dev_col::RXPCK, Cell::F2),
                ColSpec::rate(net_dev_col::TXPCK, Cell::F2),
                ColSpec::rate(net_dev_col::RXKB, Cell::BYTES_PER_SEC),
                ColSpec::rate(net_dev_col::TXKB, Cell::BYTES_PER_SEC),
                ColSpec::rate(net_dev_col::RXCMP, Cell::F2),
                ColSpec::rate(net_dev_col::TXCMP, Cell::F2),
                ColSpec::rate(net_dev_col::RXMCST, Cell::F2),
                ColSpec::rate(net_dev_col::IFUTIL_PCT, Cell::PC2),
            ],
        }
    }

    fn net_edev(opts: &SarTextOptions) -> View {
        View {
            id: ActivityId::NET_EDEV,
            hdr_line: "IFACE;rxerr/s;txerr/s;coll/s;rxdrop/s;txdrop/s;txcarr/s;rxfram/s;rxfifo/s;txfifo/s",
            pos: 0,
            iwidth: if opts.pretty { -1 } else { 0 },
            ext: false,
            head: if opts.pretty {
                HeadItem::None
            } else {
                HeadItem::Name9
            },
            tail: if opts.pretty {
                TailItem::Name
            } else {
                TailItem::None
            },
            layout: Layout::List,
            header: zero_omit_policy(opts),
            avg_label: AvgLabel::Average,
            cells: rate_cells(1..10),
        }
    }

    fn pwr_cpu() -> View {
        View {
            id: ActivityId::PWR_CPU,
            hdr_line: "CPU;MHz",
            pos: 0,
            iwidth: 7,
            ext: false,
            head: HeadItem::Cpu3,
            tail: TailItem::None,
            layout: Layout::Cpu,
            header: HeaderPolicy::Once,
            avg_label: AvgLabel::Average,
            cells: vec![ColSpec::mean(pwr_cpu_col::MHZ, Cell::F2, Cell::F2)],
        }
    }

    /// `A_PWR_FAN` / `A_PWR_TEMP` / `A_PWR_IN`。
    ///
    /// `iwidth = -2` なので**第 2 トークン (`DEVICE`) が行末へ回る**。
    fn sensor(id: ActivityId, hdr_line: &'static str, cells: Vec<ColSpec>) -> View {
        View {
            id,
            hdr_line,
            pos: 0,
            iwidth: -2,
            ext: false,
            head: HeadItem::Index5,
            tail: TailItem::Name,
            layout: Layout::List,
            header: HeaderPolicy::Once,
            avg_label: AvgLabel::Average,
            cells,
        }
    }

    fn huge() -> View {
        Self::simple(
            ActivityId::HUGE,
            "kbhugfree;kbhugused;%hugused;kbhugrsvd;kbhugsurp",
            vec![
                ColSpec::mean(huge_col::KBHUGFREE, Cell::KB, Cell::KB_AVG),
                ColSpec::ratio(huge_col::KBHUGUSED, Cell::KB, Cell::KB_AVG),
                ColSpec::ratio(huge_col::HUGUSED_PCT, Cell::PC2, Cell::PC2),
                ColSpec::mean(huge_col::KBHUGRSVD, Cell::KB, Cell::KB_AVG),
                ColSpec::mean(huge_col::KBHUGSURP, Cell::KB, Cell::KB_AVG),
            ],
        )
    }

    fn pwr_freq() -> View {
        View {
            id: ActivityId::PWR_FREQ,
            hdr_line: "CPU;wghMHz",
            pos: 0,
            iwidth: 7,
            ext: false,
            head: HeadItem::Cpu3,
            tail: TailItem::None,
            layout: Layout::FreqMatrix,
            header: HeaderPolicy::Once,
            avg_label: AvgLabel::Average,
            cells: vec![ColSpec::rate(freq_col::WGH_MHZ, Cell::F2)],
        }
    }

    fn usb() -> View {
        View {
            id: ActivityId::PWR_USB,
            // テキスト出力では使わない (ヘッダは手書き)。sadf 用の定義を控えておく
            hdr_line: "manufact;product;BUS;idvendor;idprod;maxpower",
            pos: 0,
            iwidth: 0,
            ext: false,
            head: HeadItem::Bus6,
            tail: TailItem::UsbNames,
            layout: Layout::List,
            header: HeaderPolicy::Once,
            avg_label: AvgLabel::Summary,
            cells: vec![
                ColSpec::last(usb_col::VENDOR_ID, Cell::Hex),
                ColSpec::last(usb_col::PRODUCT_ID, Cell::Hex),
                ColSpec::last(usb_col::MAX_POWER, Cell::Int),
            ],
        }
    }

    fn fs(opts: &SarTextOptions) -> View {
        View {
            id: ActivityId::FS,
            hdr_line: "FILESYSTEM;MBfsfree;MBfsused;%fsused;%ufsused;Ifree;Iused;%Iused|MOUNTPOINT;MBfsfree;MBfsused;%fsused;%ufsused;Ifree;Iused;%Iused",
            pos: usize::from(opts.mount),
            iwidth: -1,
            ext: false,
            head: HeadItem::None,
            tail: TailItem::Name,
            layout: Layout::List,
            header: zero_omit_policy(opts),
            avg_label: AvgLabel::SummaryOrLast,
            cells: vec![
                ColSpec::last(fs_col::MB_FREE, Cell::MB_FROM_BYTES),
                ColSpec::last(fs_col::MB_USED, Cell::MB_FROM_BYTES),
                ColSpec::last(fs_col::USED_PCT, Cell::PC2),
                ColSpec::last(fs_col::UNPRIV_USED_PCT, Cell::PC2),
                ColSpec::last(fs_col::IFREE, Cell::Int),
                ColSpec::last(fs_col::IUSED, Cell::Int),
                ColSpec::last(fs_col::IUSED_PCT, Cell::PC2),
            ],
        }
    }

    fn net_fc() -> View {
        View {
            id: ActivityId::NET_FC,
            hdr_line: "FCHOST;fch_rxf/s;fch_txf/s;fch_rxw/s;fch_txw/s",
            pos: 0,
            iwidth: -1,
            ext: false,
            head: HeadItem::None,
            tail: TailItem::Name,
            layout: Layout::List,
            header: HeaderPolicy::Once,
            avg_label: AvgLabel::Average,
            cells: rate_cells(1..5),
        }
    }

    fn net_soft(opts: &SarTextOptions) -> View {
        View {
            id: ActivityId::NET_SOFT,
            hdr_line: "CPU;total/s;dropd/s;squeezd/s;rx_rps/s;flw_lim/s;blg_len",
            pos: 0,
            iwidth: 7,
            ext: false,
            head: HeadItem::Cpu7,
            tail: TailItem::None,
            layout: Layout::Cpu,
            header: zero_omit_policy(opts),
            avg_label: AvgLabel::Average,
            cells: vec![
                ColSpec::rate(soft_col::TOTAL, Cell::F2),
                ColSpec::rate(soft_col::DROPD, Cell::F2),
                ColSpec::rate(soft_col::SQUEEZD, Cell::F2),
                ColSpec::rate(soft_col::RX_RPS, Cell::F2),
                ColSpec::rate(soft_col::FLW_LIM, Cell::F2),
                // blg_len だけ累積平均 (03 §id=39)
                ColSpec::mean(soft_col::BLG_LEN, Cell::Int, Cell::F0),
            ],
        }
    }

    fn psi(id: ActivityId, hdr_line: &'static str, full: bool) -> View {
        let mut cells = vec![
            ColSpec::mean(psi_col::SOME_10, Cell::PC2, Cell::PC2),
            ColSpec::mean(psi_col::SOME_60, Cell::PC2, Cell::PC2),
            ColSpec::mean(psi_col::SOME_300, Cell::PC2, Cell::PC2),
            // 累積 µs 由来の列は全期間から再計算する (累積平均ではない)
            ColSpec::rate(psi_col::SOME_TOTAL, Cell::PC2),
        ];
        if full {
            cells.extend([
                ColSpec::mean(psi_col::FULL_10, Cell::PC2, Cell::PC2),
                ColSpec::mean(psi_col::FULL_60, Cell::PC2, Cell::PC2),
                ColSpec::mean(psi_col::FULL_300, Cell::PC2, Cell::PC2),
                ColSpec::rate(psi_col::FULL_TOTAL, Cell::PC2),
            ]);
        }
        Self::simple(id, hdr_line, cells)
    }

    fn bat() -> View {
        View {
            id: ActivityId::PWR_BAT,
            hdr_line: "BAT;%cap;cap/min;status",
            pos: 0,
            iwidth: 0,
            ext: false,
            head: HeadItem::Index5,
            tail: TailItem::None,
            layout: Layout::List,
            header: HeaderPolicy::Once,
            avg_label: AvgLabel::Average,
            cells: vec![
                // 瞬時値は小数 0 桁、平均は 2 桁 (03 §id=43 の落とし穴)
                ColSpec::mean(
                    bat_col::CAP_PCT,
                    Cell::Percent { wd: 0 },
                    Cell::Percent { wd: 2 },
                ),
                ColSpec::rate(bat_col::CAP_PER_MIN, Cell::Float { wd: 2, sign: true }),
                // status は平均行では出力されない
                ColSpec::instant_only(bat_col::STATUS, Cell::BatStatus),
            ],
        }
    }
}

// ============================================================================
// ヘッダ行
// ============================================================================

/// `print_hdr_line()` の `*` 展開で使うラベル。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StarLabel {
    /// `j == 0` — `K_LOWERALL` の `all` (接頭辞を付けない)。
    Aggregate,
    /// `j > 0` — `{prefix}{j-1}{suffix}`。
    Index(usize),
}

/// `print_hdr_line()` 相当 (03 §3)。
///
/// - 先頭に `\n` を出す = **ヘッダ行の直前に必ず空行 1 行が入る**
/// - 各列は「空白 1 個 + 右詰め `vwidth`(=9) 桁」
/// - `|` は複数出力の切り替え、`;` は列区切り、`&` は拡張列の境界、
///   `*` はアイテム展開 (`A_IRQ` の `CPU*`)
/// - `iwidth > 0` は先頭列の専用幅、`0` は通常幅、`< 0` は
///   「`-iwidth` 番目のトークンを行末へ幅指定なしで出す」
fn header_line(
    timestamp: &str,
    hdr_line: &str,
    pos: usize,
    iwidth: i32,
    ext: bool,
    star_labels: &[StarLabel],
) -> String {
    let Some(section) = hdr_line.split('|').nth(pos) else {
        return String::new();
    };
    // `&` の処理: 拡張列を出すなら `;` と同じ、出さないならそこで打ち切る
    let body: String = match section.find('&') {
        Some(i) if ext => {
            let mut s = section.to_string();
            s.replace_range(i..i + 1, ";");
            s
        }
        Some(i) => section[..i].to_string(),
        None => section.to_string(),
    };

    let mut out = String::with_capacity(192);
    out.push('\n');
    out.push_str(&pad_right(timestamp, TSW));

    let mut iwidth = iwidth;
    let mut i: i32 = -1;
    let mut tail: Option<&str> = None;
    for tk in body.split(';') {
        if let Some(star) = tk.find('*') {
            let (prefix, suffix) = (&tk[..star], &tk[star + 1..]);
            for label in star_labels {
                let text = match label {
                    StarLabel::Aggregate => "all".to_string(),
                    StarLabel::Index(n) => format!("{prefix}{n}{suffix}"),
                };
                out.push(' ');
                out.push_str(&pad_left(&text, VW));
            }
            i -= 1;
            continue;
        }
        if iwidth > 0 {
            out.push(' ');
            out.push_str(&pad_left(tk, iwidth as usize));
            iwidth = 0;
            i -= 1;
            continue;
        }
        if iwidth < 0 && iwidth == i {
            tail = Some(tk);
            iwidth = 0;
        } else {
            out.push(' ');
            out.push_str(&pad_left(tk, VW));
        }
        i -= 1;
    }
    if let Some(t) = tail {
        out.push(' ');
        out.push_str(t);
    }
    out.push('\n');
    out
}

/// `A_PWR_USB` の手書きヘッダ (`print_hdr_line()` を使わない唯一の activity)。
fn usb_header_line(timestamp: &str) -> String {
    format!(
        "\n{}     BUS  idvendor    idprod  maxpower {} product\n",
        pad_right(timestamp, TSW),
        pad_right("manufact", MANUF_W)
    )
}

// ============================================================================
// バナー / 特殊行
// ============================================================================

/// 実 CPU 数 (`sa_cpu_nr > 1 ? sa_cpu_nr - 1 : 1`)。
pub fn real_cpu_count(sa_cpu_nr: Option<u32>) -> u32 {
    match sa_cpu_nr {
        Some(n) if n > 1 => n - 1,
        _ => 1,
    }
}

/// バナー行 (`print_gal_header()`、03 §1)。
///
/// ```text
/// printf("%s %s (%s) \t%s \t_%s_\t(%d CPU)\n", sysname, release, nodename, date, machine, cpu_nr)
/// ```
///
/// **区切りはタブ文字**であり空白ではない。`(nodename)` と日付の後には
/// 「空白 1 個 + タブ」が入り、`_machine_` の後はタブのみ。
pub fn write_banner<W: Write>(out: &mut W, file: &SaFile) -> io::Result<()> {
    let h = file.header();
    writeln!(
        out,
        "{} {} ({}) \t{} \t_{}_\t({} CPU)",
        h.sysname,
        h.release,
        h.nodename,
        report_date(h.year, h.month, h.day),
        h.machine,
        real_cpu_count(h.cpu_nr)
    )
}

/// バナーの日付 (`DATE_FORMAT_LOCAL` = `%x`、C ロケールでは `MM/DD/YY`)。
///
/// `FileHeader::month` は既に 1 起点に正規化されている。
pub fn report_date(year: i32, month: u8, day: u8) -> String {
    let yy = year.rem_euclid(100);
    format!("{month:02}/{day:02}/{yy:02}")
}

/// `LINUX RESTART` 行 (03 §8.4)。
///
/// 先頭に `\n` (= 直前に空行 1 行)、時刻の後に空白 2 個、
/// **`LINUX RESTART` と `(N CPU)` の間はタブ 1 個**。
pub fn write_restart<W: Write>(out: &mut W, timestamp: &str, cpu_nr: u32) -> io::Result<()> {
    writeln!(
        out,
        "\n{}  LINUX RESTART\t({cpu_nr} CPU)",
        pad_right(timestamp, TSW)
    )
}

/// `COM` 行 (03 §8.5)。**先頭に `\n` は入らない。**
pub fn write_comment<W: Write>(out: &mut W, timestamp: &str, text: &str) -> io::Result<()> {
    writeln!(out, "{}  COM {}", pad_right(timestamp, TSW), sanitize(text))
}

/// 非印字文字を `.` に置換する (`replace_nonprintable_char()`)。
fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_graphic() || c == ' ' {
                c
            } else {
                '.'
            }
        })
        .collect()
}

// ============================================================================
// タイムスタンプ
// ============================================================================

/// レコードのタイムスタンプ文字列 (`set_record_timestamp_string()`)。
fn timestamp_of(snap: &Snapshot, style: TimeStyle) -> String {
    time_string(snap.ust_time, snap.hour, snap.minute, snap.second, style)
}

fn event_timestamp(ev: &RecordEvent, style: TimeStyle) -> String {
    let (h, m, s) = ev.time();
    time_string(ev.ust_time(), h, m, s, style)
}

fn time_string(ust_time: u64, hour: u8, minute: u8, second: u8, style: TimeStyle) -> String {
    use chrono::{Local, TimeZone, Utc};
    match style {
        // `-t`: レコードに焼き込まれた時分秒をそのまま使う
        TimeStyle::Recorded => format!("{hour:02}:{minute:02}:{second:02}"),
        TimeStyle::Epoch => ust_time.to_string(),
        TimeStyle::Utc => match Utc.timestamp_opt(ust_time as i64, 0).single() {
            Some(dt) => dt.format("%H:%M:%S").to_string(),
            None => format!("{hour:02}:{minute:02}:{second:02}"),
        },
        TimeStyle::Local => match Local.timestamp_opt(ust_time as i64, 0).single() {
            Some(dt) => dt.format("%H:%M:%S").to_string(),
            None => format!("{hour:02}:{minute:02}:{second:02}"),
        },
    }
}

// ============================================================================
// 行データ
// ============================================================================

/// アイテム名列の値。
#[derive(Debug, Clone, PartialEq, Eq)]
enum RowLabel {
    None,
    /// CPU 集約行 (`all`)。
    Aggregate,
    /// 数値 (CPU 番号 / センサ番号 / 回線番号 / バス番号)。
    Number(i64),
    /// 文字列 (デバイス名など)。
    Name(String),
}

/// item の同一性。
#[derive(Debug, Clone, PartialEq, Eq)]
enum ItemKey {
    /// 位置で対応付ける (CPU / センサなど、順序が安定している activity)。
    Index(usize),
    /// 名前で対応付ける (インターフェース / FS / 割り込み / FC ホスト)。
    Name(String),
    /// `major` / `minor` で対応付ける (`A_DISK`。名前は wire に無い)。
    Dev(u64, u64),
    /// 回線番号 (`A_SERIAL`)。
    Line(u64),
    /// `(bus, vendor, product)` (`A_PWR_USB`)。
    Usb(u64, u64, u64),
}

/// 1 行分の表示データ。
#[derive(Debug, Clone)]
struct Row {
    key: ItemKey,
    label: RowLabel,
    /// 値列。`view.cells` と同順 (`A_IRQ` だけは CPU 列ぶん並ぶ)。
    values: Vec<Computed>,
    /// 行末に付ける文字列 (デバイス名など)。
    tail: Option<String>,
    /// `A_PWR_USB` の manufacturer / product。
    usb_names: Option<(String, String)>,
    /// この行の現サンプル (累積・最終値用)。
    snapshot: ItemSnapshot,
    /// 行列型の付随スロット。
    slots: Vec<ItemSnapshot>,
}

/// item 1 個とその付随スロット (行列型)。
#[derive(Debug, Clone)]
struct ItemGroup {
    /// 名前・識別子を持つ代表 item。
    primary: ItemSnapshot,
    /// 行列型のスロット (`A_IRQ` は CPU 列、`A_PWR_FREQ` は周波数スロット)。
    slots: Vec<ItemSnapshot>,
}

/// item 1 個分の平均計算用状態。
#[derive(Debug, Clone)]
struct ItemState {
    key: ItemKey,
    label: RowLabel,
    tail: Option<String>,
    usb_names: Option<(String, String)>,
    /// 最初のサンプル (`buf[2]` 相当。方式 A の差分基準)。
    first: ItemSnapshot,
    /// 最後に表示したサンプル。
    last: ItemSnapshot,
    first_slots: Vec<ItemSnapshot>,
    last_slots: Vec<ItemSnapshot>,
    /// 方式 B の累積。
    accum: ItemAccum,
    /// このブロックで 1 度でも表示されたか。
    displayed: bool,
}

// ============================================================================
// 出力ブロック
// ============================================================================

/// 1 出力ブロック (activity × サブレポート) の状態。
///
/// レコードは溜めない。平均のために「最初のサンプル」「最後のサンプル」
/// および累積和だけを item ごとに保持する (O(item 数 × 列数))。
#[derive(Debug)]
pub struct SarBlock {
    view: View,
    opts: SarTextOptions,
    def: &'static ActivityDef,
    /// デコード計画 (平均行の再計算に必要なので控える)。
    plan: Option<DecodePlan>,
    /// ヘッダを出したか。
    header_done: bool,
    /// ヘッダ行に載せるタイムスタンプ (= 直前サンプルの時刻)。
    prev_ts: String,
    /// 最初のサンプルの uptime (cs)。方式 A の分母の始点。
    first_uptime: Option<u64>,
    /// 最後に表示したサンプルの uptime (cs)。
    last_uptime: u64,
    /// 表示したサンプル数 (`avg_count`)。
    displayed: u64,
    /// `A_IRQ` のヘッダで展開する CPU 列 (item 添字。0 = 集約列)。
    irq_cpu_cols: Vec<usize>,
    items: Vec<ItemState>,
}

impl SarBlock {
    /// activity の出力ブロックを列挙する。
    ///
    /// 定義を持たない activity では空を返す。`A_MEMORY` のように
    /// 複数のサブレポートを持つ activity は複数返る。
    pub fn blocks_for(id: ActivityId, opts: &SarTextOptions) -> Vec<SarBlock> {
        let Some(def) = lookup(id) else {
            return Vec::new();
        };
        View::all_for(id, opts)
            .into_iter()
            .map(|view| SarBlock {
                view,
                opts: opts.clone(),
                def,
                plan: None,
                header_done: false,
                prev_ts: String::new(),
                first_uptime: None,
                last_uptime: 0,
                displayed: 0,
                irq_cpu_cols: Vec::new(),
                items: Vec::new(),
            })
            .collect()
    }

    /// このブロックの activity。
    pub fn activity(&self) -> ActivityId {
        self.view.id
    }

    /// `RESTART` / `COMMENT` を 1 件処理する。
    ///
    /// 走査が**イベントを読んだ時点で**呼ばれる。統計レコードに束ねないので、
    /// 最後の統計レコードより後ろにあるイベントもここに届く
    /// (本家も読んだ順に `COM` / `LINUX RESTART` 行を出す。03 §1.10 の内側ループ)。
    pub fn event<W: Write>(&mut self, out: &mut W, ev: &RecordEvent) -> io::Result<()> {
        let ts = event_timestamp(ev, self.opts.time);
        match ev {
            RecordEvent::Restart { cpu_count, .. } => {
                // 区間が終わるので平均を先に出す
                self.flush_average(out)?;
                write_restart(out, &ts, real_cpu_count(*cpu_count))?;
                self.reset_region();
            }
            RecordEvent::Comment { text, .. } => {
                if self.opts.comment {
                    write_comment(out, &ts, text)?;
                }
            }
        }
        Ok(())
    }

    /// 統計レコード 1 件を処理する。
    ///
    /// - `view.has_prev == false` のレコードは**表示しない**
    ///   (前サンプルとして消費されるだけ)
    /// - `RESTART` をまたいだレコードも表示しない (差分の基準がリセットされる)
    ///
    /// `RESTART` / `COMMENT` は [`SarBlock::event`] が受け持つ。
    pub fn record<W: Write>(&mut self, out: &mut W, view: &IntervalView<'_>) -> io::Result<()> {
        let Some(plan) = view.plan_for(self.view.id) else {
            return Ok(());
        };
        if self.plan.is_none() {
            self.plan = Some(plan.clone());
        }
        let Some(curr_act) = view.curr.activity(self.view.id) else {
            return Ok(());
        };
        let nr2 = curr_act.nr2;

        // 前サンプルが無い / 不連続なレコードは区間の基準として消費するだけ
        if !view.has_prev || !view.continuous || self.first_uptime.is_none() {
            self.adopt_reference(plan, &curr_act.items, nr2, view.curr);
            return Ok(());
        }

        let prev_items: &[ItemSnapshot] = view
            .prev
            .activity(self.view.id)
            .map(|a| a.items.as_slice())
            .unwrap_or(&[]);

        let ts = timestamp_of(view.curr, self.opts.time);
        let rows = self.build_rows(plan, prev_items, &curr_act.items, nr2, view.itv_cs);
        if rows.is_empty() {
            self.prev_ts = ts;
            return Ok(());
        }

        self.write_header(out)?;
        let mut buf = String::new();
        for row in &rows {
            buf.clear();
            self.render_row(&mut buf, &ts, row, false);
            out.write_all(buf.as_bytes())?;
        }
        for row in rows {
            self.accumulate(row);
        }

        self.displayed += 1;
        self.last_uptime = view.curr.uptime_cs;
        self.prev_ts = ts;
        Ok(())
    }

    /// ブロックを閉じ、`Average:` / `Summary:` / `Last:` 行を出す。
    pub fn finish<W: Write>(&mut self, out: &mut W) -> io::Result<()> {
        self.flush_average(out)
    }

    // ---- 区間管理 ----

    /// 区間の基準サンプルを採用する (表示はしない)。
    fn adopt_reference(
        &mut self,
        plan: &DecodePlan,
        items: &[ItemSnapshot],
        nr2: u32,
        snap: &Snapshot,
    ) {
        self.first_uptime = Some(snap.uptime_cs);
        self.last_uptime = snap.uptime_cs;
        self.prev_ts = timestamp_of(snap, self.opts.time);
        self.items.clear();
        for (idx, group) in self.iter_groups(plan, items, nr2) {
            self.items.push(ItemState {
                key: self.item_key(plan, &group.primary, idx),
                label: self.row_label(plan, &group.primary, idx),
                tail: self.tail_text(plan, &group.primary, idx),
                usb_names: self.usb_names(plan, &group.primary),
                first: group.primary.clone(),
                last: group.primary.clone(),
                first_slots: group.slots.clone(),
                last_slots: group.slots,
                accum: ItemAccum::new(self.def.columns.len()),
                displayed: false,
            });
        }
    }

    /// RESTART をまたいだので区間状態を捨てる。
    fn reset_region(&mut self) {
        self.items.clear();
        self.displayed = 0;
        self.first_uptime = None;
        self.header_done = false;
    }

    // ---- ヘッダ ----

    fn write_header<W: Write>(&mut self, out: &mut W) -> io::Result<()> {
        if self.header_done && self.view.header == HeaderPolicy::Once {
            return Ok(());
        }
        let ts = self.prev_ts.clone();
        out.write_all(self.header_for(&ts).as_bytes())?;
        self.header_done = true;
        Ok(())
    }

    fn header_for(&self, timestamp: &str) -> String {
        if self.view.id == ActivityId::PWR_USB {
            return usb_header_line(timestamp);
        }
        let star: Vec<StarLabel> = if self.view.layout == Layout::IrqMatrix {
            self.irq_cpu_cols
                .iter()
                .map(|c| {
                    if *c == 0 {
                        StarLabel::Aggregate
                    } else {
                        StarLabel::Index(c - 1)
                    }
                })
                .collect()
        } else {
            Vec::new()
        };
        header_line(
            timestamp,
            self.view.hdr_line,
            self.view.pos,
            self.view.iwidth,
            self.view.ext,
            &star,
        )
    }

    // ---- item の列挙と対応付け ----

    /// 表示対象の item を列挙する。
    ///
    /// 戻り値の添字は本家のループ変数 `i` に対応する
    /// (CPU 系では 0 = 集約行、`n` = CPU `n-1`)。
    fn iter_groups(
        &self,
        plan: &DecodePlan,
        items: &[ItemSnapshot],
        nr2: u32,
    ) -> Vec<(usize, ItemGroup)> {
        let width = plan.fields.len();
        let plain = |i: usize, it: &ItemSnapshot| {
            (
                i,
                ItemGroup {
                    primary: it.clone(),
                    slots: Vec::new(),
                },
            )
        };
        match self.view.layout {
            Layout::Single => items
                .first()
                .map(|it| vec![plain(0, it)])
                .unwrap_or_default(),
            Layout::List => items
                .iter()
                .enumerate()
                .map(|(i, it)| plain(i, it))
                .collect(),
            Layout::Cpu => {
                let mut out = Vec::new();
                for i in 0..items.len() {
                    if !self.opts.cpus.includes(i) {
                        continue;
                    }
                    // 集約行を個別 CPU の単純和で作り直すのは
                    // `A_CPU` (`get_global_cpu_statistics()`) と
                    // `A_NET_SOFT` (`get_global_soft_statistics()`) だけ。
                    // `A_PWR_CPU` / `A_PWR_FREQ` の item 0 は**収集時に
                    // 平均が入っている**ので、合算すると CPU 数倍になる。
                    let primary = if i == 0 && items.len() > 1 && self.recomputes_aggregate() {
                        compute::sum_items(width, items.iter().skip(1))
                    } else {
                        items[i].clone()
                    };
                    out.push((
                        i,
                        ItemGroup {
                            primary,
                            slots: Vec::new(),
                        },
                    ));
                }
                out
            }
            Layout::IrqMatrix => {
                // item 添字 = cpu * nr2 + irq (CPU 主、割り込み副)
                let nr2 = nr2.max(1) as usize;
                // `nr2 == 1` は「CPU 次元を持たない世代」(12.5 以前の `stats_irq`)。
                // nr が割り込み数になるので、1 item = 1 割り込み行 (合計列のみ) になる。
                if nr2 == 1 {
                    return items
                        .iter()
                        .enumerate()
                        .map(|(i, it)| {
                            (
                                i,
                                ItemGroup {
                                    primary: it.clone(),
                                    slots: vec![it.clone()],
                                },
                            )
                        })
                        .collect();
                }
                let cpus = items.len() / nr2;
                let mut out = Vec::new();
                for irq in 0..nr2 {
                    let mut slots = Vec::new();
                    for c in 0..cpus {
                        if !self.opts.cpus.includes(c) {
                            continue;
                        }
                        let item = if c == 0 && cpus > 1 {
                            compute::sum_items(
                                width,
                                (1..cpus).filter_map(|k| items.get(k * nr2 + irq)),
                            )
                        } else {
                            match items.get(c * nr2 + irq) {
                                Some(it) => it.clone(),
                                None => continue,
                            }
                        };
                        slots.push(item);
                    }
                    let Some(name_slot) = items.get(irq) else {
                        continue;
                    };
                    out.push((
                        irq,
                        ItemGroup {
                            primary: name_slot.clone(),
                            slots,
                        },
                    ));
                }
                out
            }
            Layout::FreqMatrix => {
                // item 添字 = cpu * nr2 + freq_slot
                let nr2 = nr2.max(1) as usize;
                let cpus = items.len() / nr2;
                let mut out = Vec::new();
                for i in 0..cpus {
                    if !self.opts.cpus.includes(i) {
                        continue;
                    }
                    let end = ((i + 1) * nr2).min(items.len());
                    let slots = items[i * nr2..end].to_vec();
                    let primary = slots.first().cloned().unwrap_or_default();
                    out.push((i, ItemGroup { primary, slots }));
                }
                out
            }
        }
    }

    /// 集約行 (item 0) を個別行の単純和で作り直す activity か。
    fn recomputes_aggregate(&self) -> bool {
        matches!(self.view.id, ActivityId::CPU | ActivityId::NET_SOFT)
    }

    /// `A_IRQ` で表示する CPU 列の item 添字。
    fn irq_columns(&self, items: usize, nr2: u32) -> Vec<usize> {
        let nr2 = nr2.max(1) as usize;
        // CPU 次元を持たない世代は合計列 (`all`) だけ
        if nr2 == 1 {
            return vec![0];
        }
        let cpus = items / nr2;
        (0..cpus).filter(|c| self.opts.cpus.includes(*c)).collect()
    }

    fn item_key(&self, plan: &DecodePlan, item: &ItemSnapshot, index: usize) -> ItemKey {
        match self.view.id {
            ActivityId::DISK => ItemKey::Dev(
                compute::raw_column(plan, item, disk_col::MAJOR).unwrap_or(0),
                compute::raw_column(plan, item, disk_col::MINOR).unwrap_or(0),
            ),
            ActivityId::SERIAL => {
                ItemKey::Line(compute::raw_column(plan, item, 0).unwrap_or(index as u64))
            }
            ActivityId::PWR_USB => ItemKey::Usb(
                compute::raw_column(plan, item, usb_col::BUS).unwrap_or(0),
                compute::raw_column(plan, item, usb_col::VENDOR_ID).unwrap_or(0),
                compute::raw_column(plan, item, usb_col::PRODUCT_ID).unwrap_or(0),
            ),
            // 名前が item の同一性を表すのはこの 5 つだけ。
            // `A_PWR_FAN` / `A_PWR_TEMP` / `A_PWR_IN` の `device` は
            // 「センサチップ名」で**複数のセンサが同じ値を持つ**ため、
            // 名前でまとめると行が潰れる (位置で対応付ける)。
            ActivityId::NET_DEV
            | ActivityId::NET_EDEV
            | ActivityId::FS
            | ActivityId::NET_FC
            | ActivityId::IRQ => match item.key.as_deref() {
                Some(k) if !k.is_empty() => ItemKey::Name(k.to_string()),
                // 名前を持たない世代 (12.5 以前の `A_IRQ` など) は位置で対応付ける
                _ => ItemKey::Index(index),
            },
            _ => ItemKey::Index(index),
        }
    }

    /// 行頭 (または行末) のアイテム名列の値。
    fn row_label(&self, plan: &DecodePlan, item: &ItemSnapshot, index: usize) -> RowLabel {
        match self.view.head {
            HeadItem::None => match self.view.tail {
                TailItem::Name => RowLabel::None,
                _ => RowLabel::None,
            },
            HeadItem::Cpu7 | HeadItem::Cpu3 => {
                if index == 0 {
                    RowLabel::Aggregate
                } else {
                    RowLabel::Number(index as i64 - 1)
                }
            }
            HeadItem::Name9 => RowLabel::Name(self.item_name(plan, item, index)),
            HeadItem::Line3 => {
                RowLabel::Number(compute::raw_column(plan, item, 0).unwrap_or(0) as i64)
            }
            HeadItem::Index5 => match self.view.id {
                // FAN / TEMP は 1 起点、IN は 0 起点 (03 §11-16)
                ActivityId::PWR_FAN | ActivityId::PWR_TEMP => RowLabel::Number(index as i64 + 1),
                ActivityId::PWR_IN => RowLabel::Number(index as i64),
                ActivityId::PWR_BAT => RowLabel::Number(i64::from(compute::signed_byte(
                    compute::raw_column(plan, item, bat_col::ID).unwrap_or(0),
                ))),
                _ => RowLabel::Number(index as i64),
            },
            HeadItem::Bus6 => {
                RowLabel::Number(compute::raw_column(plan, item, usb_col::BUS).unwrap_or(0) as i64)
            }
        }
    }

    /// 行末に出すアイテム名。
    fn tail_text(&self, plan: &DecodePlan, item: &ItemSnapshot, index: usize) -> Option<String> {
        match self.view.tail {
            TailItem::Name => Some(self.item_name(plan, item, index)),
            _ => None,
        }
    }

    /// `A_PWR_USB` の manufacturer / product。
    ///
    /// 1 item が 2 本の文字列を持つので `item.key` だけでは足りない。
    /// [`DecodePlan::text_index`] で位置を引いて [`ItemSnapshot::text`] から取る。
    fn usb_names(&self, plan: &DecodePlan, item: &ItemSnapshot) -> Option<(String, String)> {
        if self.view.tail != TailItem::UsbNames {
            return None;
        }
        let text = |name: &str| -> String {
            plan.text_index(name)
                .and_then(|i| item.text(i))
                .unwrap_or("")
                .to_string()
        };
        Some((text("manufacturer"), text("product")))
    }

    /// アイテム名 (`A_DISK` は名前が wire に無いので合成する)。
    fn item_name(&self, plan: &DecodePlan, item: &ItemSnapshot, index: usize) -> String {
        if self.view.id == ActivityId::DISK {
            return self.disk_name(plan, item);
        }
        match item.key.as_deref() {
            Some(k) if !k.is_empty() => k.to_string(),
            // `A_IRQ` の `irq_name` は 12.6 で入ったフィールド。
            // 持たない世代では item 0 が総数 (`sum`)、item n が割り込み `n-1`。
            _ if self.view.id == ActivityId::IRQ => {
                if index == 0 {
                    "sum".to_string()
                } else {
                    (index - 1).to_string()
                }
            }
            _ => String::new(),
        }
    }

    /// `A_DISK` のデバイス名。
    ///
    /// `stats_disk` は名前文字列を持たないため、**他ホストのファイルで
    /// ローカルの `/sys` を引いてはいけない**。既定は `dev<major>-<minor>` で、
    /// `-j SID` のときだけ WWN 由来の安定 ID を使う (03 §2.8)。
    fn disk_name(&self, plan: &DecodePlan, item: &ItemSnapshot) -> String {
        let maj = compute::raw_column(plan, item, disk_col::MAJOR).unwrap_or(0);
        let min = compute::raw_column(plan, item, disk_col::MINOR).unwrap_or(0);
        if self.opts.dev_sid {
            let hi = compute::raw_column(plan, item, disk_col::WWN_HIGH).unwrap_or(0);
            if hi != 0 {
                let lo = compute::raw_column(plan, item, disk_col::WWN_LOW).unwrap_or(0);
                let part = compute::raw_column(plan, item, disk_col::PART_NR).unwrap_or(0);
                let mut s = format!("{hi:#016x}");
                if lo != 0 {
                    s.push_str(&format!("{lo:016x}"));
                }
                if part != 0 {
                    s.push_str(&format!("-{part}"));
                }
                return s;
            }
        }
        format!("dev{maj}-{min}")
    }

    // ---- 行の構築 ----

    fn build_rows(
        &mut self,
        plan: &DecodePlan,
        prev_items: &[ItemSnapshot],
        curr_items: &[ItemSnapshot],
        nr2: u32,
        itv_cs: u64,
    ) -> Vec<Row> {
        if self.view.layout == Layout::IrqMatrix {
            self.irq_cpu_cols = self.irq_columns(curr_items.len(), nr2);
        }
        let width = plan.fields.len();
        let zero = || ItemSnapshot {
            key: None,
            texts: Vec::new(),
            values: vec![Availability::Present(0); width],
        };

        let prev_groups = self.iter_groups(plan, prev_items, nr2);
        let mut rows = Vec::new();

        for (idx, group) in self.iter_groups(plan, curr_items, nr2) {
            let key = self.item_key(plan, &group.primary, idx);
            // 前サンプルは**位置ではなく識別子で**対応付ける。
            // 見つからない (新規登録) 場合は全ゼロ構造体を前値にする
            // (本家の check_*_reg() が -1/-2 を返したときと同じ扱い)。
            let matched = prev_groups
                .iter()
                .find(|(pi, pg)| self.item_key(plan, &pg.primary, *pi) == key);
            let (prev_primary, prev_slots) = match matched {
                Some((_, pg)) => (pg.primary.clone(), pg.slots.clone()),
                None => (zero(), vec![zero(); group.slots.len()]),
            };

            // -z: 前サンプルと同一なら行を出さない
            if self.opts.zero_omit
                && matched.is_some()
                && self.same_sample(plan, &prev_primary, &group.primary)
            {
                continue;
            }

            let Some(row) =
                self.make_row(plan, idx, key, &group, &prev_primary, &prev_slots, itv_cs)
            else {
                continue;
            };
            rows.push(row);
        }
        rows
    }

    /// `-z` の同一判定。
    ///
    /// 本家は activity ごとの比較長で `memcmp` する。ここでは
    /// 「デコードした数値フィールドがすべて等しいか」で判定し、
    /// `A_NET_DEV` の `duplex` のように本家が比較対象外にしている
    /// 末尾フィールドは除外する (03 §2.7)。
    fn same_sample(&self, plan: &DecodePlan, prev: &ItemSnapshot, curr: &ItemSnapshot) -> bool {
        let skip = if self.view.id == ActivityId::NET_DEV {
            plan.column_fields
                .get(net_dev_col::DUPLEX)
                .copied()
                .flatten()
                .map(|f| f.index())
        } else {
            None
        };
        prev.values.len() == curr.values.len()
            && prev
                .values
                .iter()
                .zip(curr.values.iter())
                .enumerate()
                .all(|(i, (p, c))| Some(i) == skip || p == c)
    }

    /// 1 行分の値を計算する。`None` なら行を出さない (オフライン CPU など)。
    #[allow(clippy::too_many_arguments)]
    fn make_row(
        &self,
        plan: &DecodePlan,
        idx: usize,
        key: ItemKey,
        group: &ItemGroup,
        prev_primary: &ItemSnapshot,
        prev_slots: &[ItemSnapshot],
        itv_cs: u64,
    ) -> Option<Row> {
        let curr = &group.primary;
        let mut ctx = ComputeContext::new(itv_cs);
        ctx.aggregate_item = idx == 0;

        let values = match self.view.layout {
            // --- A_IRQ: CPU 列ぶん値が並ぶ ---
            Layout::IrqMatrix => {
                let mut vals = Vec::with_capacity(group.slots.len());
                for (n, slot) in group.slots.iter().enumerate() {
                    let mut c = ComputeContext::new(itv_cs);
                    // 合計列 (先頭) だけ「総数が減ったら 0」のクランプが効く
                    c.aggregate_item = self.irq_cpu_cols.first() == Some(&0) && n == 0;
                    let empty = ItemSnapshot::default();
                    let p = prev_slots.get(n).unwrap_or(&empty);
                    vals.push(self.value_of(plan, irq_col::COUNT, p, slot, &c));
                }
                vals
            }
            // --- A_PWR_FREQ: CPU ごとの重み付き平均 ---
            Layout::FreqMatrix => {
                vec![compute::weighted_mhz(plan, prev_slots, &group.slots)]
            }
            // --- A_CPU: オフライン / tickless の特別扱い ---
            Layout::Cpu if self.view.id == ActivityId::CPU => {
                if compute::cpu_is_offline(plan, curr) {
                    // オフライン CPU は行そのものを出さない
                    return None;
                }
                let (fixed_prev, mut total) = compute::per_cpu_interval(plan, prev_primary, curr);
                if idx == 0 {
                    // CPU "all" が tickless になることはない前提で 1 に補正する
                    total = total.max(1);
                } else if total == 0 {
                    // tickless CPU: 計算せず 0.00 × n + %idle = 100.00
                    return Some(Row {
                        key,
                        label: self.row_label(plan, curr, idx),
                        values: self.tickless_cpu_values(),
                        tail: None,
                        usb_names: None,
                        snapshot: curr.clone(),
                        slots: Vec::new(),
                    });
                }
                ctx.tick_total = Some(total);
                self.cell_values(plan, &fixed_prev, curr, &ctx)
            }
            _ => self.cell_values(plan, prev_primary, curr, &ctx),
        };

        Some(Row {
            key,
            label: self.row_label(plan, curr, idx),
            values,
            tail: self.tail_text(plan, curr, idx),
            usb_names: self.usb_names(plan, curr),
            snapshot: curr.clone(),
            slots: group.slots.clone(),
        })
    }

    fn cell_values(
        &self,
        plan: &DecodePlan,
        prev: &ItemSnapshot,
        curr: &ItemSnapshot,
        ctx: &ComputeContext,
    ) -> Vec<Computed> {
        self.view
            .cells
            .iter()
            .map(|spec| self.value_of(plan, spec.col, prev, curr, ctx))
            .collect()
    }

    fn value_of(
        &self,
        plan: &DecodePlan,
        column: usize,
        prev: &ItemSnapshot,
        curr: &ItemSnapshot,
        ctx: &ComputeContext,
    ) -> Computed {
        let Some(meta) = self.def.columns.get(column) else {
            return Err(ComputeIssue::NotImplemented);
        };
        // 識別子列 (`A_PWR_USB` の `idvendor` / `idprod` など) は計算対象ではないので
        // 生値をそのまま読む。計算層は意図的に `NotNumeric` を返す。
        if meta.kind == ValueKind::Identity {
            return compute::raw_column(plan, curr, column).map(|v| v as f64);
        }
        compute::column_value(self.view.id, column, meta, plan, prev, curr, ctx)
    }

    /// tickless CPU の固定値 (`0.00` × n + `%idle = 100.00`)。
    fn tickless_cpu_values(&self) -> Vec<Computed> {
        self.view
            .cells
            .iter()
            .map(|spec| {
                if spec.col == cpu_col::IDLE {
                    Ok(100.0)
                } else {
                    Ok(0.0)
                }
            })
            .collect()
    }

    // ---- 行の書式化 ----

    fn render_row(&self, out: &mut String, label: &str, row: &Row, average: bool) {
        out.push_str(&pad_right(label, TSW));
        self.render_head(out, row);
        for (i, v) in row.values.iter().enumerate() {
            let spec = self
                .view
                .cells
                .get(i)
                .or_else(|| self.view.cells.last())
                .copied();
            let Some(spec) = spec else { continue };
            if average && !spec.in_average {
                continue;
            }
            let cell = if average { spec.avg_cell } else { spec.cell };
            out.push_str(&cell.render(*v, &self.opts));
        }
        self.render_tail(out, row);
        out.push('\n');
    }

    fn render_head(&self, out: &mut String, row: &Row) {
        match (self.view.head, &row.label) {
            (HeadItem::None, _) => {}
            // CPU "all" は `" %s"` + 文字列 `"    all"` (合計 8 桁)
            (HeadItem::Cpu7, RowLabel::Aggregate) => out.push_str("     all"),
            (HeadItem::Cpu7, RowLabel::Number(n)) => {
                out.push(' ');
                out.push_str(&pad_left(&n.to_string(), 7));
            }
            // `A_PWR_CPU` / `A_PWR_FREQ` は書式に先頭空白が無い
            (HeadItem::Cpu3, RowLabel::Aggregate) => out.push_str("     all"),
            (HeadItem::Cpu3, RowLabel::Number(n)) => {
                out.push_str("     ");
                out.push_str(&pad_left(&n.to_string(), 3));
            }
            (HeadItem::Name9, RowLabel::Name(name)) => {
                out.push(' ');
                out.push_str(&pad_left(name, VW));
            }
            (HeadItem::Line3, RowLabel::Number(n)) => {
                out.push_str("       ");
                out.push_str(&pad_left(&n.to_string(), 3));
            }
            (HeadItem::Index5, RowLabel::Number(n)) => {
                out.push_str("     ");
                out.push_str(&pad_left(&n.to_string(), 5));
            }
            (HeadItem::Bus6, RowLabel::Number(n)) => {
                out.push_str("  ");
                out.push_str(&pad_left(&n.to_string(), 6));
            }
            _ => {}
        }
    }

    fn render_tail(&self, out: &mut String, row: &Row) {
        match self.view.tail {
            TailItem::None => {}
            TailItem::Name => {
                if let Some(t) = &row.tail {
                    out.push(' ');
                    out.push_str(t);
                }
            }
            TailItem::UsbNames => {
                let (manuf, product) = row
                    .usb_names
                    .clone()
                    .unwrap_or_else(|| (String::new(), String::new()));
                out.push(' ');
                out.push_str(&pad_right(&manuf, MANUF_W));
                out.push(' ');
                out.push_str(&product);
            }
        }
    }

    // ---- 累積 ----

    fn accumulate(&mut self, row: Row) {
        let columns = self.def.columns.len();
        let cells = self.view.cells.clone();
        let pos = self.items.iter().position(|it| it.key == row.key);
        let idx = match pos {
            Some(i) => i,
            None => {
                // 途中で現れた item。差分の基準は全ゼロ (本家と同じ扱い)
                let zero = ItemSnapshot {
                    key: None,
                    texts: Vec::new(),
                    values: vec![Availability::Present(0); row.snapshot.values.len()],
                };
                self.items.push(ItemState {
                    key: row.key.clone(),
                    label: row.label.clone(),
                    tail: row.tail.clone(),
                    usb_names: row.usb_names.clone(),
                    first: zero.clone(),
                    last: row.snapshot.clone(),
                    first_slots: vec![zero; row.slots.len()],
                    last_slots: row.slots.clone(),
                    accum: ItemAccum::new(columns),
                    displayed: false,
                });
                self.items.len() - 1
            }
        };

        let plan = self.plan.clone();
        let state = &mut self.items[idx];
        state.label = row.label.clone();
        state.tail = row.tail.clone();
        state.usb_names = row.usb_names.clone();
        state.last = row.snapshot.clone();
        state.last_slots = row.slots.clone();
        state.displayed = true;
        if let Some(plan) = plan.as_ref() {
            for (i, spec) in cells.iter().enumerate() {
                let raw = compute::raw_column(plan, &row.snapshot, spec.col).ok();
                let value = row.values.get(i).and_then(|v| v.as_ref().ok().copied());
                state.accum.add(spec.col, raw, value);
            }
            // 比率列の平均は「分子・分母を別々に平均する」ため、
            // 表示に出ない入力列も累積しておく (03 §7-A / §id=34)
            for col in ratio_inputs(self.view.id) {
                let raw = compute::raw_column(plan, &row.snapshot, *col).ok();
                state.accum.add(*col, raw, None);
            }
        }
        // 本家の `avg_count` は activity 単位のグローバル値だが、ここでは item 単位に数える。
        // 常時存在する item では同じ値になり、途中で現れたデバイスでは
        // 「観測できた回数」で平均が取れる分こちらの方が素直になる。
        state.accum.count += 1;
    }

    // ---- 平均行 ----

    fn flush_average<W: Write>(&mut self, out: &mut W) -> io::Result<()> {
        if self.displayed == 0 {
            return Ok(());
        }
        let label = match self.view.avg_label {
            AvgLabel::Average => "Average:",
            AvgLabel::Summary => "Summary:",
            AvgLabel::SummaryOrLast => {
                if self.opts.minmax {
                    "Last:"
                } else {
                    "Summary:"
                }
            }
        };
        // `-x` 併用時のヘッダ行ラベルは `Summary:`
        if self.view.header == HeaderPolicy::EverySample {
            let hdr_label = if self.opts.minmax { "Summary:" } else { label };
            out.write_all(self.header_for(hdr_label).as_bytes())?;
        }

        let itv = interval_cs(self.first_uptime.unwrap_or(0), self.last_uptime);
        let rows = self.average_rows(itv);
        let mut buf = String::new();
        for row in &rows {
            buf.clear();
            self.render_row(&mut buf, label, row, true);
            out.write_all(buf.as_bytes())?;
        }
        self.displayed = 0;
        Ok(())
    }

    fn average_rows(&self, itv: u64) -> Vec<Row> {
        let Some(plan) = self.plan.as_ref() else {
            return Vec::new();
        };
        let mut rows = Vec::new();
        for (idx, state) in self.items.iter().enumerate() {
            if !state.displayed {
                continue;
            }
            let mut ctx = ComputeContext::new(itv);
            ctx.aggregate_item = idx == 0;
            if self.view.id == ActivityId::CPU {
                let (_, total) = compute::per_cpu_interval(plan, &state.first, &state.last);
                ctx.tick_total = Some(total.max(1));
            }

            let values: Vec<Computed> = match self.view.layout {
                Layout::IrqMatrix => (0..state.last_slots.len())
                    .map(|n| {
                        let mut c = ComputeContext::new(itv);
                        c.aggregate_item = self.irq_cpu_cols.first() == Some(&0) && n == 0;
                        let empty = ItemSnapshot::default();
                        let p = state.first_slots.get(n).unwrap_or(&empty);
                        self.value_of(plan, irq_col::COUNT, p, &state.last_slots[n], &c)
                    })
                    .collect(),
                Layout::FreqMatrix => {
                    vec![compute::weighted_mhz(
                        plan,
                        &state.first_slots,
                        &state.last_slots,
                    )]
                }
                _ => self
                    .view
                    .cells
                    .iter()
                    .map(|spec| match spec.avg {
                        AvgKind::Rate => {
                            let prev = if self.view.id == ActivityId::CPU {
                                compute::per_cpu_interval(plan, &state.first, &state.last).0
                            } else {
                                state.first.clone()
                            };
                            self.value_of(plan, spec.col, &prev, &state.last, &ctx)
                        }
                        AvgKind::Mean => state.accum.mean(spec.col),
                        AvgKind::MeanRatio => compute::average_ratio(
                            self.view.id,
                            spec.col,
                            plan,
                            &state.accum,
                            &state.last,
                        ),
                        // 方式 C: 最後に観測した値をそのまま再掲する
                        AvgKind::Last => {
                            self.value_of(plan, spec.col, &state.last, &state.last, &ctx)
                        }
                    })
                    .collect(),
            };

            rows.push(Row {
                key: state.key.clone(),
                label: state.label.clone(),
                values,
                tail: state.tail.clone(),
                usb_names: state.usb_names.clone(),
                snapshot: state.last.clone(),
                slots: state.last_slots.clone(),
            });
        }
        rows
    }
}

// ============================================================================
// レポート全体
// ============================================================================

/// ファイルに含まれる既知 activity を**ファイル記載順**で返す (`id_seq[]` 相当)。
///
/// 本家のファイル読み出しモードは `act[]` 配列順ではなく
/// **データファイルの activity リスト順**で出力する (03 §11-19)。
pub fn activities_in_file(file: &SaFile) -> Vec<ActivityId> {
    let mut out: Vec<ActivityId> = Vec::new();
    for act in file.activities() {
        if lookup(act.id).is_some() && !out.contains(&act.id) {
            out.push(act.id);
        }
    }
    out
}

/// バナー + 指定 activity のブロックを順に書き出す。
///
/// activity ごとに [`walk_items`](crate::series::walk_items) を 1 回ずつ回す
/// (本家がファイルを activity ごとに読み直すのと同じ構造)。
/// レコードは溜めないので、`out` に [`std::io::BufWriter`] を渡せば
/// そのままストリーミング出力になる。
pub fn write_report<W: Write>(
    out: &mut W,
    file: &SaFile,
    opts: &SarTextOptions,
    activities: &[ActivityId],
) -> crate::Result<()> {
    use crate::format::file::ScanControl;
    use crate::series::{Selection, WalkItem, walk_items};

    let io = |e: io::Error| crate::Error::Io {
        path: file.path().to_path_buf(),
        source: e,
    };

    write_banner(out, file).map_err(io)?;
    for id in activities {
        for mut block in SarBlock::blocks_for(*id, opts) {
            // イベントは読んだ順にその場で渡す。最後の統計レコードより後ろにある
            // `COM` / `LINUX RESTART` 行も、`Average:` 行の前に出る。
            walk_items(file, &Selection::Only(vec![*id]), |item| {
                match item {
                    WalkItem::Event(ev) => block.event(out, &ev).map_err(io)?,
                    WalkItem::Sample(view) => block.record(out, view).map_err(io)?,
                }
                Ok(ScanControl::Continue)
            })?;
            block.finish(out).map_err(io)?;
        }
    }
    Ok(())
}

/// 比率列の平均計算に必要な「表示されない入力列」。
fn ratio_inputs(id: ActivityId) -> &'static [usize] {
    match id {
        ActivityId::MEMORY => &[mem_col::KBMEMTOTAL, mem_col::KBSWPTOTAL],
        ActivityId::HUGE => &[huge_col::KBHUGTOTAL],
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::abi::{Endian, LayoutAbi, SourceEncoding};

    fn plan_for(id: ActivityId) -> DecodePlan {
        let def = lookup(id).expect("定義がある");
        let rev = def.latest().expect("revision がある");
        let enc = SourceEncoding::new(Endian::Little, LayoutAbi::LP64);
        DecodePlan::build(def, rev, rev.size_lp64, 2, 2, &enc).expect("計画を作れる")
    }

    // ---- printf プリミティブ ----

    /// 値 1 個は「空白 1 個 + 右詰め 9 桁」= 10 桁。
    #[test]
    fn one_value_column_is_ten_bytes() {
        let s = fmt_float(0.0, VW, 2, false);
        assert_eq!(s, "      0.00");
        assert_eq!(s.len(), 10);
        assert_eq!(fmt_float(98.75, VW, 2, false), "     98.75");
        assert_eq!(fmt_u64(1_437_740, VW).len(), 10);
    }

    /// `--dec=0` でも幅は 10 桁のまま (パディングが差分を吸収する)。
    #[test]
    fn dec_changes_precision_not_width() {
        let mut opts = SarTextOptions::default();
        assert_eq!(Cell::F2.render(Ok(2.15), &opts), "      2.15");
        opts.dec_places = Some(0);
        assert_eq!(Cell::F2.render(Ok(2.15), &opts), "         2");
        // 丸めは printf と同じ「厳密な 2 進値に対する最近接偶数丸め」。
        // 2.15 は 2.1499999999999999... なので 1 桁では 2.1 になる (2.2 ではない)。
        opts.dec_places = Some(1);
        assert_eq!(Cell::F2.render(Ok(2.15), &opts), "       2.1");
        for d in [None, Some(0), Some(1), Some(2)] {
            opts.dec_places = d;
            assert_eq!(Cell::F2.render(Ok(2.15), &opts).len(), 10, "{d:?}");
        }
    }

    /// `--dec=` は `wd == 0` の列には効かない。
    #[test]
    fn dec_does_not_touch_integer_columns() {
        let opts = SarTextOptions {
            dec_places: Some(2),
            ..Default::default()
        };
        assert_eq!(Cell::Int.render(Ok(396.0), &opts), "       396");
        assert_eq!(Cell::F0.render(Ok(396.0), &opts), "       396");
    }

    /// `--human` でもパーセント列の幅は 10 桁 (`%` は `wi` の内側)。
    #[test]
    fn human_percent_keeps_width() {
        let mut opts = SarTextOptions::default();
        assert_eq!(Cell::PC2.render(Ok(82.88), &opts), "     82.88");
        opts.human = true;
        let s = Cell::PC2.render(Ok(82.88), &opts);
        assert_eq!(s, "     82.9%");
        assert_eq!(s.len(), 10);
    }

    /// `cprintf_unit` の分母は 1024。既定の小数は 1 桁、`--dec=0` で 0 桁。
    #[test]
    fn human_unit_uses_1024_and_one_decimal() {
        let mut opts = SarTextOptions {
            human: true,
            ..Default::default()
        };
        // 1_437_740 kB → 1.3 G
        assert_eq!(Cell::KB.render(Ok(1_437_740.0), &opts), "      1.4G");
        assert_eq!(Cell::KB.render(Ok(1_023.9), &opts), "   1023.9k");
        assert_eq!(Cell::KB.render(Ok(1_024.0), &opts), "      1.0M");
        opts.dec_places = Some(0);
        assert_eq!(Cell::KB.render(Ok(1_024.0), &opts), "        1M");
        // 非 human では素の整数
        opts.human = false;
        opts.dec_places = None;
        assert_eq!(Cell::KB.render(Ok(1_437_740.0), &opts), "   1437740");
    }

    /// `A_NET_DEV` は非 human だけ 1024 で割る。`--human` はバイト単位で出る。
    #[test]
    fn net_dev_kb_column_divides_only_without_human() {
        let mut opts = SarTextOptions::default();
        assert_eq!(Cell::BYTES_PER_SEC.render(Ok(2_048.0), &opts), "      2.00");
        opts.human = true;
        assert_eq!(Cell::BYTES_PER_SEC.render(Ok(0.0), &opts), "      0.0B");
    }

    /// `A_FS` の MB 列はバイトから MB へ、小数 0 桁。
    #[test]
    fn fs_mb_column_converts_bytes() {
        let opts = SarTextOptions::default();
        let bytes = 705.0 * 1024.0 * 1024.0;
        assert_eq!(Cell::MB_FROM_BYTES.render(Ok(bytes), &opts), "       705");
    }

    /// 計算できなかった値は 0 ではなく `?` で出す。
    #[test]
    fn unknown_values_are_not_rendered_as_zero() {
        let opts = SarTextOptions::default();
        assert_eq!(
            Cell::F2.render(Err(ComputeIssue::NotImplemented), &opts),
            "         ?"
        );
        // その世代のファイルに無いフィールドは本家と同じく 0
        assert_eq!(
            Cell::F2.render(Err(ComputeIssue::UnsupportedBySource), &opts),
            "      0.00"
        );
    }

    /// `A_PWR_BAT` の矢印はバイト幅でパディングする。
    ///
    /// 矢印 (UTF-8 3 バイト) は `" %11s"`、`?` は `" %9s"` で、
    /// **バイト幅で揃えると見た目が一致する**。文字数基準だと 2 バイトずれる。
    #[test]
    fn bat_status_pads_by_bytes() {
        let charging = bat_status_cell(bat_status::CHARGING);
        assert_eq!(charging.len(), 12, "空白 9 + 矢印 3 バイト");
        assert_eq!(charging.chars().count(), 10, "見た目は 10 桁");
        assert_eq!(charging, "         \u{2197}");

        let unknown = bat_status_cell(bat_status::UNKNOWN);
        assert_eq!(unknown.len(), 10);
        assert_eq!(unknown, "         ?");

        // 文字数基準でパディングすると 1 バイト足りなくなることを明示
        let naive = format!(" {:>11}", "\u{2197}");
        assert_eq!(naive.len(), 14);
        assert_ne!(naive.len(), charging.len());
    }

    /// `?` 行と矢印行では行のバイト長が 2 バイト変わる (本家と同じ)。
    #[test]
    fn bat_status_row_length_differs_by_two_bytes() {
        assert_eq!(
            bat_status_cell(bat_status::FULL).len() - bat_status_cell(0).len(),
            2
        );
    }

    // ---- ヘッダ行 ----

    /// `A_CPU` (`-u ALL`) のヘッダ行が本家の実測値と一致する。
    #[test]
    fn cpu_all_header_matches_upstream() {
        let opts = SarTextOptions {
            cpu_all: true,
            ..Default::default()
        };
        let view = View::cpu(&opts);
        let got = header_line(
            "09:33:48",
            view.hdr_line,
            view.pos,
            view.iwidth,
            view.ext,
            &[],
        );
        assert_eq!(
            got,
            "\n09:33:48        CPU      %usr     %nice      %sys   %iowait    %steal      %irq     %soft    %guest    %gnice     %idle\n"
        );
        // 行長 = 11 + 8 + 10 × 10 = 119
        assert_eq!(got.trim_start_matches('\n').trim_end().len(), 119);
    }

    /// `-u` (既定) は 6 列。
    #[test]
    fn cpu_default_header_matches_upstream() {
        let opts = SarTextOptions::default();
        let view = View::cpu(&opts);
        let got = header_line(
            "10:00:01",
            view.hdr_line,
            view.pos,
            view.iwidth,
            view.ext,
            &[],
        );
        assert_eq!(
            got,
            "\n10:00:01        CPU     %user     %nice   %system   %iowait    %steal     %idle\n"
        );
    }

    /// `A_MEMORY` は `&` で `-r` と `-r ALL` を切り替える。
    #[test]
    fn memory_header_extension_is_gated_by_ampersand() {
        let short = header_line("10:00:01", MEMORY_HDR, 0, 0, false, &[]);
        assert!(short.trim_end().ends_with("kbshmem"), "{short}");
        assert_eq!(short.trim_start_matches('\n').trim_end().len(), 131);

        let long = header_line("10:00:01", MEMORY_HDR, 0, 0, true, &[]);
        assert!(long.trim_end().ends_with("kbvmused"), "{long}");
        assert_eq!(long.trim_start_matches('\n').trim_end().len(), 181);

        let swap = header_line("10:00:01", MEMORY_HDR, 1, 0, false, &[]);
        assert_eq!(
            swap,
            "\n10:00:01    kbswpfree kbswpused  %swpused  kbswpcad   %swpcad\n"
        );
    }

    /// `iwidth < 0` のときは該当トークンが行末へ回る。
    #[test]
    fn negative_iwidth_moves_token_to_end() {
        // A_DISK の --pretty: 第 1 トークン (DEV) が行末
        let pretty = header_line(
            "13:20:09",
            "DEV;tps;rkB/s;wkB/s;dkB/s;areq-sz;aqu-sz;await;%util",
            0,
            -1,
            false,
            &[],
        );
        assert_eq!(
            pretty,
            "\n13:20:09          tps     rkB/s     wkB/s     dkB/s   areq-sz    aqu-sz     await     %util DEV\n"
        );
        // 非 pretty は行頭
        let plain = header_line(
            "10:00:01",
            "DEV;tps;rkB/s;wkB/s;dkB/s;areq-sz;aqu-sz;await;%util",
            0,
            0,
            false,
            &[],
        );
        assert_eq!(
            plain,
            "\n10:00:01          DEV       tps     rkB/s     wkB/s     dkB/s   areq-sz    aqu-sz     await     %util\n"
        );
    }

    /// `iwidth = -2` は第 2 トークン (`DEVICE`) を行末へ回す。
    #[test]
    fn sensor_header_moves_device_to_end() {
        assert_eq!(
            header_line("10:00:01", "FAN;DEVICE;rpm;drpm", 0, -2, false, &[]),
            "\n10:00:01          FAN       rpm      drpm DEVICE\n"
        );
        assert_eq!(
            header_line("10:00:01", "TEMP;DEVICE;degC;%temp", 0, -2, false, &[]),
            "\n10:00:01         TEMP      degC     %temp DEVICE\n"
        );
        assert_eq!(
            header_line("10:00:01", "IN;DEVICE;inV;%in", 0, -2, false, &[]),
            "\n10:00:01           IN       inV       %in DEVICE\n"
        );
    }

    /// `A_IRQ` の `CPU*` は選択された CPU ごとに展開される (`j == 0` は `all`)。
    #[test]
    fn irq_header_expands_cpu_columns() {
        let labels = [
            StarLabel::Aggregate,
            StarLabel::Index(0),
            StarLabel::Index(1),
        ];
        assert_eq!(
            header_line("10:00:01", "INTR;CPU*", 0, 0, false, &labels),
            "\n10:00:01         INTR       all      CPU0      CPU1\n"
        );
        // --pretty では INTR が行末
        assert_eq!(
            header_line("10:00:01", "INTR;CPU*", 0, -1, false, &labels),
            "\n10:00:01          all      CPU0      CPU1 INTR\n"
        );
    }

    /// `A_PWR_USB` のヘッダは手書き (`print_hdr_line()` を通らない)。
    #[test]
    fn usb_header_is_hand_written() {
        assert_eq!(
            usb_header_line("10:00:01"),
            "\n10:00:01        BUS  idvendor    idprod  maxpower manufact                product\n"
        );
    }

    /// その他 activity のヘッダ行も実測値と一致する。
    #[test]
    fn misc_headers_match_upstream() {
        let cases: &[(ActivityId, &str)] = &[
            (ActivityId::PCSW, "\n10:00:01       proc/s   cswch/s\n"),
            (ActivityId::SWAP, "\n10:00:01     pswpin/s pswpout/s\n"),
            (
                ActivityId::IO,
                "\n10:00:01          tps      rtps      wtps      dtps   bread/s   bwrtn/s   bdscd/s\n",
            ),
            (
                ActivityId::KTABLES,
                "\n10:00:01    dentunusd   file-nr  inode-nr    pty-nr\n",
            ),
            (
                ActivityId::QUEUE,
                "\n10:00:01      runq-sz  plist-sz   ldavg-1   ldavg-5  ldavg-15   blocked\n",
            ),
            (
                ActivityId::HUGE,
                "\n10:00:01    kbhugfree kbhugused  %hugused kbhugrsvd kbhugsurp\n",
            ),
            (
                ActivityId::NET_SOCK,
                "\n10:00:01       totsck    tcpsck    udpsck    rawsck   ip-frag    tcp-tw\n",
            ),
            (
                ActivityId::NET_TCP,
                "\n10:00:01     active/s passive/s    iseg/s    oseg/s\n",
            ),
            (
                ActivityId::PSI_CPU,
                "\n10:00:01     %scpu-10  %scpu-60 %scpu-300     %scpu\n",
            ),
            (
                ActivityId::PSI_IO,
                "\n10:00:01      %sio-10   %sio-60  %sio-300      %sio   %fio-10   %fio-60  %fio-300      %fio\n",
            ),
            (
                ActivityId::PSI_MEM,
                "\n10:00:01     %smem-10  %smem-60 %smem-300     %smem  %fmem-10  %fmem-60 %fmem-300     %fmem\n",
            ),
            (ActivityId::PWR_CPU, "\n10:00:01        CPU       MHz\n"),
            (ActivityId::PWR_FREQ, "\n10:00:01        CPU    wghMHz\n"),
            (
                ActivityId::PWR_BAT,
                "\n10:00:01          BAT      %cap   cap/min    status\n",
            ),
            (
                ActivityId::NET_SOFT,
                "\n10:00:01        CPU   total/s   dropd/s squeezd/s  rx_rps/s flw_lim/s   blg_len\n",
            ),
            (
                ActivityId::NET_FC,
                "\n10:00:01    fch_rxf/s fch_txf/s fch_rxw/s fch_txw/s FCHOST\n",
            ),
            (
                ActivityId::FS,
                "\n10:00:01     MBfsfree  MBfsused   %fsused  %ufsused     Ifree     Iused    %Iused FILESYSTEM\n",
            ),
            (
                ActivityId::SERIAL,
                "\n10:00:01          TTY   rcvin/s   xmtin/s framerr/s prtyerr/s     brk/s   ovrun/s\n",
            ),
            (
                ActivityId::NET_DEV,
                "\n10:00:01        IFACE   rxpck/s   txpck/s    rxkB/s    txkB/s   rxcmp/s   txcmp/s  rxmcst/s   %ifutil\n",
            ),
            (
                ActivityId::NET_EDEV,
                "\n10:00:01        IFACE   rxerr/s   txerr/s    coll/s  rxdrop/s  txdrop/s  txcarr/s  rxfram/s  rxfifo/s  txfifo/s\n",
            ),
            (
                ActivityId::PAGE,
                "\n10:00:01     pgpgin/s pgpgout/s   fault/s  majflt/s  pgfree/s pgscank/s pgscand/s pgsteal/s  pgprom/s   pgdem/s\n",
            ),
        ];
        let opts = SarTextOptions::default();
        for (id, expected) in cases {
            let view = &View::all_for(*id, &opts)[0];
            let got = header_line(
                "10:00:01",
                view.hdr_line,
                view.pos,
                view.iwidth,
                view.ext,
                &[],
            );
            assert_eq!(&got, expected, "{id}");
        }
    }

    /// `-F MOUNT` では 1 列目の見出しが `MOUNTPOINT` になる。
    #[test]
    fn fs_mount_header_uses_mountpoint() {
        let opts = SarTextOptions {
            mount: true,
            ..Default::default()
        };
        let view = &View::all_for(ActivityId::FS, &opts)[0];
        let got = header_line(
            "10:00:01",
            view.hdr_line,
            view.pos,
            view.iwidth,
            view.ext,
            &[],
        );
        assert!(got.trim_end().ends_with(" MOUNTPOINT"), "{got}");
    }

    /// ドキュメントに書式が載っている 43 activity すべてでヘッダが出る。
    #[test]
    fn every_activity_has_a_header_and_cells() {
        let opts = SarTextOptions {
            memory: true,
            swap: true,
            ..Default::default()
        };
        let mut covered = 0;
        for id in crate::model::KNOWN_ACTIVITIES {
            let views = View::all_for(*id, &opts);
            assert!(!views.is_empty(), "{id}: ビューが無い");
            for view in &views {
                assert!(!view.cells.is_empty(), "{id}: 値列が無い");
                let line = if *id == ActivityId::PWR_USB {
                    usb_header_line("10:00:01")
                } else {
                    header_line(
                        "10:00:01",
                        view.hdr_line,
                        view.pos,
                        view.iwidth,
                        view.ext,
                        &[StarLabel::Aggregate],
                    )
                };
                assert!(line.starts_with("\n10:00:01   "), "{id}: {line:?}");
                assert!(line.ends_with('\n'));
            }
            covered += 1;
        }
        assert_eq!(covered, 43);
    }

    // ---- 特殊行 ----

    /// `LINUX RESTART` 行: 先頭に空行、時刻の後に空白 2 個、`(N CPU)` の前はタブ。
    #[test]
    fn restart_line_matches_upstream() {
        let mut out = Vec::new();
        write_restart(&mut out, "09:33:38", 8).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "\n09:33:38     LINUX RESTART\t(8 CPU)\n"
        );
    }

    /// `COM` 行: 先頭に空行は入らない。
    #[test]
    fn comment_line_has_no_leading_blank() {
        let mut out = Vec::new();
        write_comment(&mut out, "09:34:30", "Hello, world!").unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "09:34:30     COM Hello, world!\n"
        );
    }

    #[test]
    fn comment_replaces_nonprintable_characters() {
        let mut out = Vec::new();
        write_comment(&mut out, "09:34:30", "a\tb\u{7}c").unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "09:34:30     COM a.b.c\n");
    }

    #[test]
    fn real_cpu_count_subtracts_the_aggregate_slot() {
        assert_eq!(real_cpu_count(Some(9)), 8);
        assert_eq!(real_cpu_count(Some(1)), 1);
        assert_eq!(real_cpu_count(None), 1);
    }

    #[test]
    fn report_date_is_mm_dd_yy() {
        assert_eq!(report_date(2018, 8, 29), "08/29/18");
        assert_eq!(report_date(2026, 1, 2), "01/02/26");
    }

    // ---- ラベル幅 ----

    /// タイムスタンプ・`Average:` はどれも `%-11s`。
    #[test]
    fn row_labels_are_eleven_bytes() {
        for label in ["09:34:34", "Average:", "Summary:", "Minimum:", "Maximum:"] {
            assert_eq!(pad_right(label, TSW).len(), 11, "{label}");
        }
        assert_eq!(pad_right("Last:", TSW), "Last:      ");
    }

    // ---- 行の組み立て ----

    fn block(id: ActivityId, opts: &SarTextOptions) -> SarBlock {
        SarBlock::blocks_for(id, opts).remove(0)
    }

    fn zeros(plan: &DecodePlan) -> ItemSnapshot {
        ItemSnapshot {
            key: None,
            texts: Vec::new(),
            values: vec![Availability::Present(0); plan.fields.len()],
        }
    }

    fn put(plan: &DecodePlan, item: &mut ItemSnapshot, column: usize, v: u64) {
        if let Some(Some(f)) = plan.column_fields.get(column) {
            item.values[f.index()] = Availability::Present(v);
        }
    }

    /// `A_CPU` のデータ行が本家の桁に一致する。
    #[test]
    fn cpu_row_matches_upstream_columns() {
        let opts = SarTextOptions::default();
        let mut blk = block(ActivityId::CPU, &opts);
        let plan = plan_for(ActivityId::CPU);
        blk.plan = Some(plan.clone());

        let prev = zeros(&plan);
        let mut curr = zeros(&plan);
        put(&plan, &mut curr, cpu_col::USER, 47);
        put(&plan, &mut curr, cpu_col::SYS, 55);
        put(&plan, &mut curr, cpu_col::IOWAIT, 8);
        put(&plan, &mut curr, cpu_col::IDLE, 9_875);
        put(&plan, &mut curr, cpu_col::IRQ, 13);
        put(&plan, &mut curr, cpu_col::SOFT, 2);

        let group = ItemGroup {
            primary: curr.clone(),
            slots: Vec::new(),
        };
        let row = blk
            .make_row(&plan, 0, ItemKey::Index(0), &group, &prev, &[], 10_000)
            .expect("行が出る");
        let mut s = String::new();
        blk.render_row(&mut s, "09:34:34", &row, false);
        assert_eq!(
            s,
            "09:34:34        all      0.47      0.00      0.70      0.08      0.00     98.75\n"
        );
        // 11 + 8 + 6 × 10 + 改行
        assert_eq!(s.len(), 11 + 8 + 60 + 1);
    }

    /// オフライン CPU は行そのものを出さない。
    #[test]
    fn offline_cpu_row_is_suppressed() {
        let opts = SarTextOptions::default();
        let blk = block(ActivityId::CPU, &opts);
        let plan = plan_for(ActivityId::CPU);
        let zero = zeros(&plan);
        let group = ItemGroup {
            primary: zero.clone(),
            slots: Vec::new(),
        };
        assert!(
            blk.make_row(&plan, 1, ItemKey::Index(1), &group, &zero, &[], 100)
                .is_none()
        );
    }

    /// tickless CPU は `0.00` × 5 + `%idle = 100.00`。
    #[test]
    fn tickless_cpu_row_is_all_zero_with_full_idle() {
        let opts = SarTextOptions::default();
        let mut blk = block(ActivityId::CPU, &opts);
        let plan = plan_for(ActivityId::CPU);
        blk.plan = Some(plan.clone());

        // 差分は 0 だが絶対値は非ゼロ = tickless
        let mut same = zeros(&plan);
        put(&plan, &mut same, cpu_col::IDLE, 5_000);
        let group = ItemGroup {
            primary: same.clone(),
            slots: Vec::new(),
        };
        let row = blk
            .make_row(&plan, 1, ItemKey::Index(1), &group, &same, &[], 100)
            .expect("tickless でも行は出る");
        let mut s = String::new();
        blk.render_row(&mut s, "10:00:01", &row, false);
        assert_eq!(
            s,
            "10:00:01          0      0.00      0.00      0.00      0.00      0.00    100.00\n"
        );
    }

    /// `A_DISK` のデバイス名は `dev<major>-<minor>` (ローカル `/sys` を引かない)。
    #[test]
    fn disk_device_name_defaults_to_major_minor() {
        let opts = SarTextOptions::default();
        let blk = block(ActivityId::DISK, &opts);
        let plan = plan_for(ActivityId::DISK);
        let mut item = zeros(&plan);
        put(&plan, &mut item, disk_col::MAJOR, 8);
        put(&plan, &mut item, disk_col::MINOR, 0);
        assert_eq!(blk.disk_name(&plan, &item), "dev8-0");

        let sid_opts = SarTextOptions {
            dev_sid: true,
            ..Default::default()
        };
        let sid_blk = block(ActivityId::DISK, &sid_opts);
        put(&plan, &mut item, disk_col::WWN_HIGH, 0x5000_c500_0000_0001);
        put(&plan, &mut item, disk_col::PART_NR, 3);
        assert_eq!(sid_blk.disk_name(&plan, &item), "0x5000c50000000001-3");
    }

    /// `--pretty` でアイテム名が行末へ移る。
    #[test]
    fn pretty_moves_item_name_to_row_end() {
        let plain = block(ActivityId::DISK, &SarTextOptions::default());
        assert_eq!(plain.view.head, HeadItem::Name9);
        assert_eq!(plain.view.tail, TailItem::None);

        let pretty = block(
            ActivityId::DISK,
            &SarTextOptions {
                pretty: true,
                ..Default::default()
            },
        );
        assert_eq!(pretty.view.head, HeadItem::None);
        assert_eq!(pretty.view.tail, TailItem::Name);
        assert_eq!(pretty.view.iwidth, -1);
    }

    /// `A_PWR_FAN` / `A_PWR_TEMP` は 1 起点、`A_PWR_IN` は 0 起点。
    #[test]
    fn sensor_index_origin_differs_per_activity() {
        let opts = SarTextOptions::default();
        for (id, expected) in [
            (ActivityId::PWR_FAN, 1),
            (ActivityId::PWR_TEMP, 1),
            (ActivityId::PWR_IN, 0),
        ] {
            let blk = block(id, &opts);
            let plan = plan_for(id);
            let item = zeros(&plan);
            assert_eq!(
                blk.row_label(&plan, &item, 0),
                RowLabel::Number(expected),
                "{id}"
            );
        }
    }

    /// `A_PWR_BAT` の `%cap` は瞬時値 0 桁 / 平均 2 桁。
    #[test]
    fn bat_capacity_precision_differs_between_row_and_average() {
        let opts = SarTextOptions::default();
        let blk = block(ActivityId::PWR_BAT, &opts);
        let cap = blk.view.cells[0];
        assert_eq!(cap.cell, Cell::Percent { wd: 0 });
        assert_eq!(cap.avg_cell, Cell::Percent { wd: 2 });
        assert_eq!(cap.cell.render(Ok(100.0), &opts), "       100");
        assert_eq!(cap.avg_cell.render(Ok(80.5), &opts), "     80.50");
        // status 列は平均行に出ない
        assert!(!blk.view.cells[2].in_average);
    }

    /// `A_MEMORY` の kB 列は瞬時値が整数、平均が小数 0 桁の浮動小数。
    #[test]
    fn memory_average_uses_float_with_zero_decimals() {
        let opts = SarTextOptions::default();
        let blk = block(ActivityId::MEMORY, &opts);
        let free = blk.view.cells[0];
        assert_eq!(free.cell, Cell::KB);
        assert_eq!(free.avg_cell, Cell::KB_AVG);
        assert_eq!(free.cell.render(Ok(4_066_192.0), &opts), "   4066192");
        assert_eq!(free.avg_cell.render(Ok(4_066_192.4), &opts), "   4066192");
    }

    /// `Average:` の方式が activity ごとに固定されている。
    #[test]
    fn average_method_is_pinned_per_activity() {
        let opts = SarTextOptions {
            memory: true,
            swap: true,
            ..Default::default()
        };
        // カウンタ型は方式 A (端点の差分)
        for id in [
            ActivityId::PCSW,
            ActivityId::PAGE,
            ActivityId::IO,
            ActivityId::DISK,
            ActivityId::NET_DEV,
            ActivityId::NET_FC,
            ActivityId::CPU,
        ] {
            let blk = block(id, &opts);
            assert!(
                blk.view.cells.iter().all(|c| c.avg == AvgKind::Rate),
                "{id} は方式 A"
            );
        }
        // ゲージ型は方式 B / B′
        for id in [
            ActivityId::KTABLES,
            ActivityId::QUEUE,
            ActivityId::NET_SOCK,
            ActivityId::NET_SOCK6,
        ] {
            let blk = block(id, &opts);
            assert!(
                blk.view.cells.iter().all(|c| c.avg == AvgKind::Mean),
                "{id} は方式 B"
            );
        }
        // A_MEMORY は kB 列が方式 B、比率列が方式 B′
        let mem = block(ActivityId::MEMORY, &opts);
        assert_eq!(mem.view.cells[mem_col::KBMEMFREE].avg, AvgKind::Mean);
        assert_eq!(mem.view.cells[mem_col::MEMUSED_PCT].avg, AvgKind::MeanRatio);
        // A_FS / A_PWR_USB は方式 C (最後の値の再掲)
        for id in [ActivityId::FS, ActivityId::PWR_USB] {
            let blk = block(id, &opts);
            assert!(
                blk.view.cells.iter().all(|c| c.avg == AvgKind::Last),
                "{id} は方式 C"
            );
        }
        // A_NET_SOFT は blg_len だけ方式 B
        let soft = block(ActivityId::NET_SOFT, &opts);
        assert_eq!(soft.view.cells[0].avg, AvgKind::Rate);
        assert_eq!(soft.view.cells[5].avg, AvgKind::Mean);
        // A_PSI_* は移動平均だけ方式 B、累積 µs 列は方式 A
        let psi = block(ActivityId::PSI_IO, &opts);
        assert_eq!(psi.view.cells[psi_col::SOME_10].avg, AvgKind::Mean);
        assert_eq!(psi.view.cells[psi_col::SOME_TOTAL].avg, AvgKind::Rate);
    }

    /// 平均行のラベルは activity ごとに決まる。
    #[test]
    fn average_labels_are_activity_specific() {
        let opts = SarTextOptions::default();
        assert_eq!(
            block(ActivityId::CPU, &opts).view.avg_label,
            AvgLabel::Average
        );
        assert_eq!(
            block(ActivityId::PWR_USB, &opts).view.avg_label,
            AvgLabel::Summary
        );
        assert_eq!(
            block(ActivityId::FS, &opts).view.avg_label,
            AvgLabel::SummaryOrLast
        );
    }

    /// `A_IRQ` は `dish` を見ずに毎サンプルでヘッダを出す。
    #[test]
    fn irq_prints_header_every_sample() {
        let opts = SarTextOptions::default();
        assert_eq!(
            block(ActivityId::IRQ, &opts).view.header,
            HeaderPolicy::EverySample
        );
        assert_eq!(
            block(ActivityId::CPU, &opts).view.header,
            HeaderPolicy::Once
        );
        // -z を付けると A_DISK なども毎サンプルになる
        let z = SarTextOptions {
            zero_omit: true,
            ..Default::default()
        };
        assert_eq!(
            block(ActivityId::DISK, &z).view.header,
            HeaderPolicy::EverySample
        );
    }

    /// `-P` の選択が item 添字に正しく効く (bit 0 = 集約行)。
    #[test]
    fn cpu_selection_maps_bit0_to_aggregate() {
        assert!(CpuSelection::Aggregate.includes(0));
        assert!(!CpuSelection::Aggregate.includes(1));
        assert!(CpuSelection::All.includes(0));
        assert!(CpuSelection::All.includes(5));
        let listed = CpuSelection::Listed {
            aggregate: false,
            cpus: vec![0, 2],
        };
        assert!(!listed.includes(0), "集約行は含まない");
        assert!(listed.includes(1), "CPU0 は item 添字 1");
        assert!(!listed.includes(2));
        assert!(listed.includes(3), "CPU2 は item 添字 3");
    }

    /// `-r` / `-S` の両方を指定すると 2 ブロックになる。
    #[test]
    fn memory_yields_two_blocks_for_r_and_s() {
        let opts = SarTextOptions {
            memory: true,
            swap: true,
            ..Default::default()
        };
        let blocks = SarBlock::blocks_for(ActivityId::MEMORY, &opts);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].view.pos, 0);
        assert_eq!(blocks[1].view.pos, 1);
        // 未指定なら -r 相当 1 ブロック
        assert_eq!(
            SarBlock::blocks_for(ActivityId::MEMORY, &SarTextOptions::default()).len(),
            1
        );
    }

    /// `A_PWR_USB` の行は manufacturer が空でも行末に空白が残る (本家と同じ)。
    #[test]
    fn usb_row_keeps_trailing_spaces_when_names_are_empty() {
        let opts = SarTextOptions::default();
        let mut blk = block(ActivityId::PWR_USB, &opts);
        let plan = plan_for(ActivityId::PWR_USB);
        blk.plan = Some(plan.clone());
        let mut item = zeros(&plan);
        put(&plan, &mut item, usb_col::BUS, 2);
        put(&plan, &mut item, usb_col::VENDOR_ID, 0x8087);
        put(&plan, &mut item, usb_col::PRODUCT_ID, 0x24);
        let group = ItemGroup {
            primary: item.clone(),
            slots: Vec::new(),
        };
        let row = blk
            .make_row(
                &plan,
                0,
                ItemKey::Usb(2, 0x8087, 0x24),
                &group,
                &item,
                &[],
                100,
            )
            .unwrap();
        let mut s = String::new();
        blk.render_row(&mut s, "09:34:34", &row, false);
        assert_eq!(
            s,
            "09:34:34          2      8087        24         0                         \n"
        );
        // 11 (ts) + 8 (BUS) + 10 × 3 + 1 + 23 (manufact) + 1 + 0 (product) = 74
        assert_eq!(
            s.trim_end_matches('\n').len(),
            74,
            "本家の実測 74 バイト (改行を除く)"
        );
    }
    /// `A_PWR_BAT` の 1 行 (矢印付き) がバイト単位で揃う。
    #[test]
    fn bat_row_is_byte_exact() {
        let opts = SarTextOptions::default();
        let mut blk = block(ActivityId::PWR_BAT, &opts);
        let plan = plan_for(ActivityId::PWR_BAT);
        blk.plan = Some(plan.clone());

        let mut prev = zeros(&plan);
        let mut curr = zeros(&plan);
        put(&plan, &mut prev, bat_col::CAP_PCT, 80);
        put(&plan, &mut curr, bat_col::ID, 0);
        put(&plan, &mut curr, bat_col::CAP_PCT, 78);
        put(&plan, &mut curr, bat_col::STATUS, bat_status::DISCHARGING);

        let group = ItemGroup {
            primary: curr.clone(),
            slots: Vec::new(),
        };
        // itv = 6000 cs = 60 秒 → -2.00 %/分
        let row = blk
            .make_row(&plan, 0, ItemKey::Index(0), &group, &prev, &[], 6_000)
            .unwrap();
        let mut s = String::new();
        blk.render_row(&mut s, "10:00:01", &row, false);
        assert_eq!(
            s,
            "10:00:01            0        78     -2.00         \u{2198}\n"
        );
        // 11 + 10 (BAT) + 10 (%cap) + 10 (cap/min) + 12 (矢印列 = 空白 9 + 3 バイト)
        assert_eq!(s.trim_end_matches('\n').len(), 53);

        // 平均行は status を出さず、%cap が 2 桁になる
        let mut avg = String::new();
        blk.render_row(&mut avg, "Average:", &row, true);
        assert_eq!(avg, "Average:            0     78.00     -2.00\n");
    }

    /// `A_PWR_BAT` の `status` が不明なときは幅 9 で `?` が出る。
    #[test]
    fn bat_row_with_unknown_status_is_two_bytes_shorter() {
        let opts = SarTextOptions::default();
        let mut blk = block(ActivityId::PWR_BAT, &opts);
        let plan = plan_for(ActivityId::PWR_BAT);
        blk.plan = Some(plan.clone());
        let curr = zeros(&plan);
        let group = ItemGroup {
            primary: curr.clone(),
            slots: Vec::new(),
        };
        let row = blk
            .make_row(&plan, 0, ItemKey::Index(0), &group, &curr, &[], 100)
            .unwrap();
        let mut s = String::new();
        blk.render_row(&mut s, "10:00:01", &row, false);
        assert_eq!(
            s.trim_end_matches('\n').len(),
            51,
            "矢印行より 2 バイト短い"
        );
        assert!(s.ends_with("?\n"), "{s:?}");
    }

    /// `A_PWR_FREQ` の行は CPU ごとの重み付き平均 1 列。
    #[test]
    fn pwr_freq_row_uses_weighted_average() {
        let opts = SarTextOptions {
            cpus: CpuSelection::All,
            ..Default::default()
        };
        let mut blk = block(ActivityId::PWR_FREQ, &opts);
        let plan = plan_for(ActivityId::PWR_FREQ);
        blk.plan = Some(plan.clone());

        let mut p0 = zeros(&plan);
        let mut p1 = zeros(&plan);
        let mut c0 = zeros(&plan);
        let mut c1 = zeros(&plan);
        put(&plan, &mut c0, freq_col::FREQ_KHZ, 1_500_000);
        put(&plan, &mut c0, freq_col::TIME_IN_STATE, 100);
        put(&plan, &mut p0, freq_col::TIME_IN_STATE, 0);
        put(&plan, &mut c1, freq_col::FREQ_KHZ, 800_000);
        put(&plan, &mut c1, freq_col::TIME_IN_STATE, 300);
        put(&plan, &mut p1, freq_col::TIME_IN_STATE, 0);

        let group = ItemGroup {
            primary: c0.clone(),
            slots: vec![c0, c1],
        };
        let row = blk
            .make_row(
                &plan,
                1,
                ItemKey::Index(1),
                &group,
                &p0,
                &[p0.clone(), p1],
                100,
            )
            .unwrap();
        let mut s = String::new();
        blk.render_row(&mut s, "10:00:01", &row, false);
        // (1500 × 100 + 800 × 300) / 400 = 975
        assert_eq!(s, "10:00:01          0    975.00\n");
    }

    /// `A_PSI_IO` の 8 列すべてがパーセント書式で出る。
    #[test]
    fn psi_io_row_has_eight_percent_columns() {
        let opts = SarTextOptions::default();
        let mut blk = block(ActivityId::PSI_IO, &opts);
        let plan = plan_for(ActivityId::PSI_IO);
        blk.plan = Some(plan.clone());

        let prev = zeros(&plan);
        let mut curr = zeros(&plan);
        put(&plan, &mut curr, psi_col::SOME_10, 1_234);
        // 10 秒 (1000 cs) のうち 1 秒 (1e6 µs) 停止 → 10.00%
        put(&plan, &mut curr, psi_col::SOME_TOTAL, 1_000_000);
        put(&plan, &mut curr, psi_col::FULL_TOTAL, 500_000);

        let group = ItemGroup {
            primary: curr.clone(),
            slots: Vec::new(),
        };
        let row = blk
            .make_row(&plan, 0, ItemKey::Index(0), &group, &prev, &[], 1_000)
            .unwrap();
        let mut s = String::new();
        blk.render_row(&mut s, "10:00:01", &row, false);
        assert_eq!(
            s,
            "10:00:01        12.34      0.00      0.00     10.00      0.00      0.00      0.00      5.00\n"
        );
        assert_eq!(s.trim_end_matches('\n').len(), 11 + 8 * 10);
    }

    /// `A_SERIAL` の回線番号列は 10 桁 (`"       %3d"`)。
    #[test]
    fn serial_row_uses_three_digit_line_number() {
        let opts = SarTextOptions::default();
        let mut blk = block(ActivityId::SERIAL, &opts);
        let plan = plan_for(ActivityId::SERIAL);
        blk.plan = Some(plan.clone());
        let prev = zeros(&plan);
        let mut curr = zeros(&plan);
        put(&plan, &mut curr, 0, 64);
        let group = ItemGroup {
            primary: curr.clone(),
            slots: Vec::new(),
        };
        let row = blk
            .make_row(&plan, 0, ItemKey::Line(64), &group, &prev, &[], 100)
            .unwrap();
        let mut s = String::new();
        blk.render_row(&mut s, "10:00:01", &row, false);
        assert_eq!(
            s,
            "10:00:01           64      0.00      0.00      0.00      0.00      0.00      0.00\n"
        );
    }

    /// `A_NET_SOFT` の集約行は個別 CPU の単純和で作り直す。
    #[test]
    fn net_soft_aggregate_row_is_the_sum_of_cpus() {
        let opts = SarTextOptions::default();
        let blk = block(ActivityId::NET_SOFT, &opts);
        let plan = plan_for(ActivityId::NET_SOFT);
        let mut cpu0 = zeros(&plan);
        put(&plan, &mut cpu0, soft_col::TOTAL, 100);
        let mut cpu1 = zeros(&plan);
        put(&plan, &mut cpu1, soft_col::TOTAL, 300);
        let items = vec![zeros(&plan), cpu0, cpu1];

        let groups = blk.iter_groups(&plan, &items, 1);
        assert_eq!(groups.len(), 1, "既定は集約行のみ");
        let total = compute::raw_column(&plan, &groups[0].1.primary, soft_col::TOTAL).unwrap();
        assert_eq!(total, 400, "ファイルの item 0 ではなく個別 CPU の和");
        assert!(blk.recomputes_aggregate());

        // `A_PWR_CPU` は収集時に平均が入っているので合算しない
        let pwr = block(ActivityId::PWR_CPU, &opts);
        assert!(!pwr.recomputes_aggregate());
    }

    // ---- 実データによる結合テスト ----
    //
    // 本家 sysstat のテストデータは GPL なので同梱できない。
    // `cargo run --bin xtask -- fetch-fixtures` で取得した場合だけ走らせる。

    fn fixture(name: &str) -> Option<std::path::PathBuf> {
        let p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/fixtures/upstream")
            .join(name);
        p.exists().then_some(p)
    }

    fn render(name: &str, opts: &SarTextOptions) -> Option<String> {
        let file = crate::format::SaFile::open(fixture(name)?).ok()?;
        let ids = activities_in_file(&file);
        let mut buf = Vec::new();
        write_report(&mut buf, &file, opts, &ids).expect("レポートを書ける");
        Some(String::from_utf8(buf).expect("UTF-8"))
    }

    /// レポート全体の骨格 (バナー / 空行 / ヘッダ / Average) が揃う。
    #[test]
    fn report_skeleton_from_real_file() {
        let Some(text) = render("data-12.0.0", &SarTextOptions::default()) else {
            eprintln!("fixture 未取得: スキップ");
            return;
        };
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].starts_with("Linux "), "{:?}", lines[0]);
        assert!(lines[0].contains('\t'), "バナーの区切りはタブ");
        assert!(lines[0].ends_with(" CPU)"), "{:?}", lines[0]);
        // バナー直後は空行 (ヘッダ行の先頭 \n による)
        assert_eq!(lines[1], "", "バナーの次は空行");
        assert!(text.contains("\nAverage:"), "Average: 行が出る");
        // 空行が 2 行連続することはない
        assert!(!text.contains("\n\n\n"), "空行は 2 行連続しない");
        // A_PWR_USB 以外に行末空白は出ない
        for line in &lines {
            if line.contains("Linux ") || line.len() < TSW {
                continue;
            }
            let is_usb = line.len() > 40 && line[11..].starts_with("  ");
            if !is_usb {
                assert_eq!(line.trim_end(), *line, "行末空白: {line:?}");
            }
        }
    }

    /// CPU 行の 6 列は合計 100% になる (tick 合計で正規化されている証拠)。
    #[test]
    fn cpu_percentages_sum_to_100_on_real_file() {
        let Some(text) = render("data-12.0.0", &SarTextOptions::default()) else {
            eprintln!("fixture 未取得: スキップ");
            return;
        };
        let mut checked = 0;
        let mut in_cpu = false;
        for line in text.lines() {
            if line.contains("%user") && line.contains("%idle") {
                in_cpu = true;
                continue;
            }
            if line.is_empty() {
                in_cpu = false;
                continue;
            }
            if !in_cpu {
                continue;
            }
            // 11 桁ラベル + 8 桁 CPU 列 のあとに 6 列
            let rest = &line[TSW + 8..];
            let vals: Vec<f64> = rest
                .split_whitespace()
                .filter_map(|t| t.parse::<f64>().ok())
                .collect();
            if vals.len() != 6 {
                continue;
            }
            let sum: f64 = vals.iter().sum();
            assert!(
                (sum - 100.0).abs() < 0.05,
                "CPU の割合合計が 100 でない ({sum}): {line:?}"
            );
            checked += 1;
        }
        assert!(checked > 0, "CPU 行が 1 つも見つからない");
    }

    /// 全列の幅が 10 桁で揃う (`--dec=` / `--human` でも変わらない)。
    #[test]
    fn column_width_is_stable_across_options() {
        let variants = [
            SarTextOptions::default(),
            SarTextOptions {
                dec_places: Some(0),
                ..Default::default()
            },
            SarTextOptions {
                human: true,
                ..Default::default()
            },
            SarTextOptions {
                pretty: true,
                human: true,
                ..Default::default()
            },
        ];
        let mut widths = Vec::new();
        for opts in &variants {
            let Some(text) = render("data-12.0.0", opts) else {
                eprintln!("fixture 未取得: スキップ");
                return;
            };
            // A_QUEUE ブロックの行長を比べる (6 列固定・pretty の影響も受けない)
            let w = text
                .lines()
                .find(|l| l.contains("ldavg-1"))
                .map(|l| l.len())
                .expect("A_QUEUE のヘッダがある");
            widths.push(w);
        }
        assert_eq!(widths[0], 11 + 6 * 10);
        assert!(
            widths.iter().all(|w| *w == widths[0]),
            "オプションで列幅が変わった: {widths:?}"
        );
    }

    /// `-P ALL` にすると CPU 行が増える。
    #[test]
    fn cpu_selection_changes_row_count() {
        let base = SarTextOptions::default();
        let all = SarTextOptions {
            cpus: CpuSelection::All,
            ..Default::default()
        };
        let (Some(a), Some(b)) = (render("data-12.0.0", &base), render("data-12.0.0", &all)) else {
            eprintln!("fixture 未取得: スキップ");
            return;
        };
        let count = |t: &str| {
            t.lines()
                .filter(|l| l.len() > 19 && &l[11..19] == "     all")
                .count()
        };
        assert!(count(&a) > 0);
        assert!(b.len() > a.len(), "-P ALL で行が増える");
    }

    /// `-C` を付けないと `COM` 行は出ない。
    #[test]
    fn comments_require_the_c_option() {
        let Some(plain) = render("data-11.6.5", &SarTextOptions::default()) else {
            eprintln!("fixture 未取得: スキップ");
            return;
        };
        let with_c = render(
            "data-11.6.5",
            &SarTextOptions {
                comment: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(!plain.contains("  COM "), "-C 無しで COM 行が出た");
        assert!(with_c.contains("  COM "), "-C 付きで COM 行が出ない");
    }
}

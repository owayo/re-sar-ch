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
//! use re_sar_ch::output::sar_text::{SampleSelect, SarBlock, SarTextOptions, write_banner};
//! use re_sar_ch::series::{Selection, WalkItem, walk_items};
//!
//! # fn main() -> re_sar_ch::Result<()> {
//! let file = SaFile::open("sa01")?;
//! let opts = SarTextOptions::default();
//! let mut out = BufWriter::new(stdout());
//! write_banner(&mut out, &file, &opts)?;
//!
//! for id in [ActivityId::CPU, ActivityId::MEMORY] {
//!     let select = SampleSelect::default();
//!     for mut block in SarBlock::blocks_for(id, &opts, file.header().cpu_nr, select) {
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
//! | `A_DISK` のデバイス名 | 既定は `dev<major>-<minor>`。**ローカルの `/sys` は引かない** (他ホストのファイルで誤名になる)。`-j SID` 相当の WWN 名だけ再現する |
//! | ヘッダ再表示 | パイプ出力と同じ `rows = 86400` 相当 (ブロック先頭で 1 回)。端末幅による再表示は行わない |
//! | 個別 CPU の `Average:` が tickless | 区間全体の tick 差分が 0 の CPU も分母 1 で割る (本家は `%idle = 100.00` の固定行)。実データでは「区間中ずっと完全アイドル」でしか起きない |
//!
//! [`docs/format/03-output-format.md`]: ../../../docs/format/03-output-format.md

use std::collections::BTreeMap;
use std::io::{self, Write};

use crate::format::SaFile;
use crate::layout::plan::DecodePlan;
use crate::layout::registry::{ActivityDef, lookup};
use crate::model::{ActivityId, Availability, ValueKind};
use crate::output::time_filter::{Admit, TimeFilter};
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
    pub fn includes(&self, item_index: usize) -> bool {
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

/// `-i <interval>` と positional の `interval` / `count` によるサンプル選別。
///
/// 本家では `interval` / `count` がグローバル変数で、`-i` と positional の
/// 第 1 引数が**同じ変数を共有する** (03 §5.3)。ファイル読み出しでは
/// `interval < 0` が 1 に補正され、`count` 未指定は `-1` (= 無制限) になる。
///
/// [`SarTextOptions`] とは別の型にしてある。書式ではなく「どのレコードを
/// 表示するか」の指定であり、既定 (全レコード) のときは [`SarBlock`] の
/// 走査経路に一切影響を与えないようにしたいため。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SampleSelect {
    /// ユーザ指定インターバル (秒)。`1` = 最小インターバル = 全レコード。
    ///
    /// `0` は渡さないこと (本家もファイル読み出しでは `interval == 0` を
    /// usage で弾く)。念のため取り出し側で 1 に丸める。
    pub interval: u64,
    /// 表示するサンプル数の上限。`None` = 無制限 (本家の `count = -1`)。
    pub count: Option<u64>,
}

impl Default for SampleSelect {
    fn default() -> Self {
        // 本家のファイル読み出し時の既定 (`interval < 0` → 1、`count` 未指定 → -1)。
        SampleSelect {
            interval: 1,
            count: None,
        }
    }
}

impl SampleSelect {
    /// 有効なユーザ指定インターバル。`0` は 1 として扱う。
    fn interval(self) -> u64 {
        self.interval.max(1)
    }

    /// 「全レコードをそのまま出す」指定か。
    ///
    /// 真のときは前サンプルの控え ([`HeldSample`]) を作らない。
    /// 既定の経路で item 配列の複製を増やさないための判定である。
    fn selects_every_record(self) -> bool {
        self.interval() == 1
    }
}

/// `-i` で表示を省いたときに持ち越す「最後に表示したサンプル」。
///
/// 本家は `next_slice()` が偽を返したレコードで `curr` を入れ替えない
/// (`write_stats()` が 0 を返し `*curr ^= 1` に届かない) ため、
/// 次に表示するレコードの差分は**最後に表示したサンプル**との間で取られる。
/// レコード対を作るのは [`crate::series::walk_items`] 側なので、
/// 省いたレコードを前サンプルにしないためにはここで控える必要がある。
#[derive(Debug)]
struct HeldSample {
    uptime_cs: u64,
    items: Vec<ItemSnapshot>,
}

/// `-i <interval>` のサンプル選別 (`sa_common.c: next_slice()`、03 §1.3.4)。
///
/// 判定の意味は「ユーザ指定インターバル `Iu` の整数倍が
/// `[En - In/2, En + In/2)` に入るならサンプル `En` を表示する」
/// (`In` = ファイル中の実インターバル)。
///
/// 本家の書き方をそのまま写す必要がある箇所が 3 つある。
///
/// 1. **uptime 差分は `& 0xffffffff` でマスクしてから**秒に直す (§1.10 の落とし穴 #12)。
///    `uptime_cs` は 64bit だが、本家は 32bit に切ってから割っている。
/// 2. 四捨五入は `(f * 10) - (整数部 * 10) >= 5`。素直な `round()` に
///    置き換えると境界 (`x.5` 未満の丸め誤差) で挙動が変わる。
/// 3. `min` / `max` / `pt1` / `pt2` は C の `int` (32bit)。`entry` が
///    `file_interval / 2` より小さいときの巻き下がりまで含めて再現する。
///
/// `last_uptime` は本家では関数内 `static`。**表示を省いたレコードでも
/// 毎回更新される**ので、`file_interval` は「連続する 2 レコードの間隔」に
/// なる (表示した 2 本の間隔ではない)。
///
/// 本家の C 実装をそのままビルドして 495 ケース (通常のレコード列 7 本 ×
/// 40 レコード、丸め境界 5900〜6100 cs、32bit 跨ぎ 5 件) を突き合わせ、
/// 全一致を確認してある。列の形は
/// [`next_slice_sequence_matches_upstream`](tests::next_slice_sequence_matches_upstream)
/// に写した。
fn next_slice(
    uptime_ref: u64,
    uptime: u64,
    reset: bool,
    interval: u64,
    last_uptime: &mut u64,
) -> bool {
    if *last_uptime == 0 || reset {
        *last_uptime = uptime_ref;
    }

    // ファイル中の実インターバル (秒、四捨五入)
    let f = ((uptime.wrapping_sub(*last_uptime)) & 0xffff_ffff) as f64 / 100.0;
    let mut file_interval = f as u64;
    if (f * 10.0) - (file_interval as f64 * 10.0) >= 5.0 {
        file_interval += 1;
    }

    *last_uptime = uptime;

    // 最小インターバルなら常に採用
    if interval == 1 {
        return true;
    }

    // 基準点からの経過秒 (四捨五入)
    let f = ((uptime.wrapping_sub(uptime_ref)) & 0xffff_ffff) as f64 / 100.0;
    let mut entry = f as u64;
    if (f * 10.0) - (entry as f64 * 10.0) >= 5.0 {
        entry += 1;
    }

    // ここから下は C の `int` 演算。切り詰めと符号の付き方まで写す。
    let min = entry.wrapping_sub(file_interval / 2) as i32;
    let max = entry
        .wrapping_add(file_interval / 2)
        .wrapping_add(file_interval & 1) as i32;
    let pt1 = (entry / interval).wrapping_mul(interval) as i32;
    let pt2 = (entry / interval + 1).wrapping_mul(interval) as i32;

    (pt1 >= min && pt1 < max) || (pt2 >= min && pt2 < max)
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
    /// `-s` / `-e` の時刻フィルタ。既定は無効 (全レコードを出す)。
    pub time_filter: TimeFilter,
    /// `--dev=` / `--iface=` / `--fs=` / `--int=` / `-I SUM` のアイテム名フィルタ。
    ///
    /// キーは activity。エントリが無い activity は絞り込まない。
    /// 比較対象は**表示されるアイテム名**そのもので、本家の
    /// `search_list_item()` と同じ (`-F MOUNT` ならマウントポイントと比べる)。
    ///
    /// `A_IRQ` (`--int=`) は行 = 割り込みなので、比較対象は割り込み名
    /// (`irq_name`) になる。本家 `print_irq_stats()` も
    /// `search_list_item(a->item_list, stc_cpuall_irq->irq_name)` で絞る。
    /// **割り込み名を持たない世代では番号でしか絞れない**
    /// (`check_irq_name_filter` が「名前でしか書けない指定」を拒否する)。
    pub item_names: BTreeMap<ActivityId, Vec<String>>,
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

/// `LINUX RESTART` 行に出す CPU 数。
///
/// 本家 `print_special_record()` が見るのは常に `file_hdr.sa_cpu_nr` である。
/// RESTART レコードに CPU 数が入っている世代 (`0x2175`) はそれを読んで
/// `sa_cpu_nr` を更新するので結果は同じだが、CPU 数を持たない旧世代では
/// ファイルヘッダの値が残る。レコード側を優先し、無ければヘッダへ落とす。
fn restart_cpu_count(record: Option<u32>, file_cpu_nr: Option<u32>) -> u32 {
    real_cpu_count(record.or(file_cpu_nr))
}

/// バナー行 (`print_gal_header()`、03 §1)。
///
/// ```text
/// printf("%s %s (%s) \t%s \t_%s_\t(%d CPU)\n", sysname, release, nodename, date, machine, cpu_nr)
/// ```
///
/// **区切りはタブ文字**であり空白ではない。`(nodename)` と日付の後には
/// 「空白 1 個 + タブ」が入り、`_machine_` の後はタブのみ。
pub fn write_banner<W: Write>(out: &mut W, file: &SaFile, opts: &SarTextOptions) -> io::Result<()> {
    let h = file.header();
    writeln!(
        out,
        "{} {} ({}) \t{} \t_{}_\t({} CPU)",
        h.sysname,
        h.release,
        h.nodename,
        banner_date(h.ust_time, h.year, h.month, h.day, opts.time),
        h.machine,
        real_cpu_count(h.cpu_nr)
    )
}

/// バナーに載せる日付。
///
/// 本家 `sa_common.c: get_file_timestamp_struct()` は
///
/// ```c
/// if (PRINT_TRUE_TIME(flags)) {        /* sar -t */
///     rectime->tm_mday = file_hdr->sa_day;
///     rectime->tm_mon  = file_hdr->sa_month;
///     rectime->tm_year = file_hdr->sa_year;
/// } else {
///     *rectime = *localtime(&file_hdr->sa_ust_time);
/// }
/// ```
///
/// つまり**既定は `sa_ust_time` 由来**で、ヘッダの `sa_day` / `sa_month` /
/// `sa_year` を使うのは `-t` のときだけである。両者は一致するのが普通だが、
/// 食い違うファイルがある (本家テストデータ `data-ukwn` は
/// `sa_ust_time` が 2019-09-15、ヘッダ日付が 2019-10-15)。
fn banner_date(ust_time: u64, year: i32, month: u8, day: u8, style: TimeStyle) -> String {
    use chrono::{Datelike, Local, TimeZone, Utc};

    /// エポック秒から落とした日付を `MM/DD/YY` にする。
    fn from_epoch<Tz: TimeZone>(dt: &chrono::DateTime<Tz>) -> String {
        report_date(dt.year(), dt.month() as u8, dt.day() as u8)
    }

    let fallback = || report_date(year, month, day);
    match style {
        // `-t`: ヘッダに焼き込まれた年月日をそのまま使う
        TimeStyle::Recorded => fallback(),
        // エポック表示でも日付そのものは UTC 換算 (`sadf` の `-U` 相当)
        TimeStyle::Utc | TimeStyle::Epoch => Utc
            .timestamp_opt(ust_time as i64, 0)
            .single()
            .map_or_else(fallback, |dt| from_epoch(&dt)),
        TimeStyle::Local => Local
            .timestamp_opt(ust_time as i64, 0)
            .single()
            .map_or_else(fallback, |dt| from_epoch(&dt)),
    }
}

/// バナーの日付の書式 (`DATE_FORMAT_LOCAL` = `%x`、C ロケールでは `MM/DD/YY`)。
///
/// `FileHeader::month` は既に 1 起点に正規化されている
/// (本家の `tm_mon` は 0 起点なので、そちらへ渡すなら `month - 1`)。
pub fn report_date(year: i32, month: u8, day: u8) -> String {
    let yy = year.rem_euclid(100);
    format!("{month:02}/{day:02}/{yy:02}")
}

/// `LINUX RESTART` 行 (03 §8.4)。
///
/// 先頭に `\n` (= 直前に空行 1 行)、時刻の後に空白 2 個、
/// **`LINUX RESTART` と `(N CPU)` の間はタブ 1 個**。
fn restart_line(timestamp: &str, cpu_nr: u32) -> String {
    format!(
        "\n{}  LINUX RESTART\t({cpu_nr} CPU)\n",
        pad_right(timestamp, TSW)
    )
}

/// `COM` 行 (03 §8.5)。**先頭に `\n` は入らない。**
fn comment_line(timestamp: &str, text: &str) -> String {
    format!("{}  COM {}\n", pad_right(timestamp, TSW), sanitize(text))
}

/// [`restart_line`] を書き出す。
pub fn write_restart<W: Write>(out: &mut W, timestamp: &str, cpu_nr: u32) -> io::Result<()> {
    out.write_all(restart_line(timestamp, cpu_nr).as_bytes())
}

/// [`comment_line`] を書き出す。
pub fn write_comment<W: Write>(out: &mut W, timestamp: &str, text: &str) -> io::Result<()> {
    out.write_all(comment_line(timestamp, text).as_bytes())
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
    /// item 添字 ([`SarBlock::iter_groups`] のループ変数 `i`)。
    ///
    /// CPU 系では 0 = 集約行。`-P` で行が間引かれても本家の `i` と一致する。
    index: usize,
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

/// 行の書式モード。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowMode {
    /// 通常のデータ行 (`f_print`、瞬時値の `cprintf_*`)。
    Instant,
    /// `Average:` / `Summary:` / `Last:` 行 (`f_print_avg`)。
    Average,
    /// `-x` の `Minimum:` / `Maximum:` 行。
    ///
    /// 本家の `print_*_xstats()` は**瞬時値と同じ `cprintf_*` 呼び出し**で出す
    /// (03 §8.3)。列の取捨だけは平均行と揃える (`A_PWR_BAT` の `status` は
    /// 矢印なので極値を持たない)。
    Extreme,
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
    /// item 添字 (本家のループ変数 `i`)。CPU 系では 0 = 集約行。
    index: usize,
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
    /// `-x` の最小値 (列ごと)。未更新は `f64::MAX` (本家の `spmin` 初期値)。
    min: Vec<f64>,
    /// `-x` の最大値 (列ごと)。未更新は `f64::MIN` (本家の `-DBL_MAX`)。
    max: Vec<f64>,
}

impl ItemState {
    /// 極値を 1 度でも更新したか。
    ///
    /// 本家は `spmin == DBL_MAX` の item について `Minimum:` / `Maximum:` の
    /// ブロックを出さない (03 §2.6.4)。
    fn has_extrema(&self) -> bool {
        self.min.iter().any(|v| *v != f64::MAX)
    }

    /// 表示した行の値で極値を更新する (`save_extrema()` 相当、03 §1.6.3)。
    ///
    /// 記録するのは**表示した瞬時値そのもの**。本家も `S_VALUE()` と
    /// 0 クランプを通した後の値を保存する。
    fn update_extrema(&mut self, values: &[Computed]) {
        if self.min.len() < values.len() {
            self.min.resize(values.len(), f64::MAX);
            self.max.resize(values.len(), f64::MIN);
        }
        for (slot, v) in self.min.iter_mut().zip(values.iter()) {
            if let Ok(v) = v {
                *slot = slot.min(*v);
            }
        }
        for (slot, v) in self.max.iter_mut().zip(values.iter()) {
            if let Ok(v) = v {
                *slot = slot.max(*v);
            }
        }
    }
}

/// 極値の保存領域を表示用の値列へ変換する。
///
/// 一度も更新されていない列は 0 ではなく「計算できなかった」扱いにする
/// (本家はそこに `DBL_MAX` / `-DBL_MAX` を出してしまうが、0 と混同させない)。
fn extrema_values(store: &[f64], len: usize, unset: f64) -> Vec<Computed> {
    (0..len)
        .map(|i| match store.get(i) {
            Some(v) if *v != unset => Ok(*v),
            _ => Err(ComputeIssue::MissingInSample),
        })
        .collect()
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
    /// `-i` / positional `interval` `count` によるサンプル選別。
    select: SampleSelect,
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
    /// [`next_slice`] の `last_uptime` (本家の関数内 `static`)。
    ///
    /// 本家は `handle_curr_act_stats()` の入口で `reset = TRUE` を渡すので
    /// **activity ごと・区間ごとに基準点から取り直される**
    /// (`sar.c` は `*reset = TRUE` を同関数の末尾で立て直す)。
    /// ブロックが activity × 区間に 1 つなので、ここに持てば同じになる。
    slice_last_uptime: u64,
    /// 次の [`next_slice`] 呼び出しに渡す `reset`。
    slice_reset: bool,
    /// 残り表示可能サンプル数 (`handle_curr_act_stats()` の `cnt`)。
    ///
    /// `None` = 無制限。`Some(0)` = 上限に達した (以降は表示しない)。
    remaining: Option<u64>,
    /// `-i` で表示を省いたときの「最後に表示したサンプル」。
    ///
    /// `interval == 1` では作らない ([`SampleSelect::selects_every_record`])。
    held: Option<HeldSample>,
    /// `A_IRQ` のヘッダで展開する CPU 列 (item 添字。0 = 集約列)。
    irq_cpu_cols: Vec<usize>,
    /// `A_CPU` の区間始点の**生** item 配列 (本家の `buf[2]` 相当)。
    ///
    /// CPU "all" は個別 CPU の合算で作り直すため、平均行でも
    /// [`compute::aggregate_cpu`] を通し直す必要がある (03 §1.4.3 / §8.1)。
    /// 集約済みの値だけを持っていると offline 補正をやり直せない。
    cpu_first: Vec<ItemSnapshot>,
    /// `A_CPU` の区間終点の生 item 配列 (最後に表示したレコード)。
    cpu_last: Vec<ItemSnapshot>,
    /// ファイルヘッダの `sa_cpu_nr`。`LINUX RESTART` 行の既定値。
    ///
    /// 本家 `print_special_record()` は常に `file_hdr.sa_cpu_nr` を出す。
    /// RESTART レコードが CPU 数を持つ世代 (`0x2175`) ではその値で
    /// 更新されるが、持たない旧世代ではヘッダの値がそのまま使われる
    /// (`expected.data-9.1.6` の `LINUX RESTART` は `(8 CPU)`)。
    file_cpu_nr: Option<u32>,
    items: Vec<ItemState>,
}

impl SarBlock {
    /// activity の出力ブロックを列挙する。
    ///
    /// 定義を持たない activity では空を返す。`A_MEMORY` のように
    /// 複数のサブレポートを持つ activity は複数返る。
    /// `file_cpu_nr` は `LINUX RESTART` 行に出す CPU 数の既定値
    /// (= その時点の `sa_cpu_nr`)。RESTART レコードが CPU 数を持たない世代
    /// (`0x2171` / `0x2173`) ではこれが使われる。渡さないと本家が
    /// `(8 CPU)` と出す行が `(1 CPU)` になる。
    ///
    /// `select` は `-i` / positional `interval` `count` の指定。
    /// 既定 ([`SampleSelect::default`]) は全レコードを出す。
    /// 本家は区間ごと・activity ごとに `cnt = count` を入れ直すので、
    /// ブロック 1 個 = 「1 区間 × 1 activity」で作り直すこと。
    pub fn blocks_for(
        id: ActivityId,
        opts: &SarTextOptions,
        file_cpu_nr: Option<u32>,
        select: SampleSelect,
    ) -> Vec<SarBlock> {
        let Some(def) = lookup(id) else {
            return Vec::new();
        };
        View::all_for(id, opts)
            .into_iter()
            .map(|view| SarBlock {
                view,
                opts: opts.clone(),
                select,
                def,
                plan: None,
                header_done: false,
                prev_ts: String::new(),
                first_uptime: None,
                last_uptime: 0,
                displayed: 0,
                slice_last_uptime: 0,
                slice_reset: true,
                remaining: select.count,
                held: None,
                irq_cpu_cols: Vec::new(),
                cpu_first: Vec::new(),
                cpu_last: Vec::new(),
                file_cpu_nr,
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
    ///
    /// **区間の外にあるイベントはここへ渡さない。**
    /// 区間の先頭のイベントと、区間を終わらせた `LINUX RESTART` は
    /// activity ループの外で 1 回だけ出す ([`plan_regions`] を参照)。
    /// したがって通常この関数に届くのは区間内の `COMMENT` だけだが、
    /// 区間の切り方が変わっても行が化けないよう `RESTART` の処理も残してある。
    pub fn event<W: Write>(&mut self, out: &mut W, ev: &RecordEvent) -> io::Result<()> {
        let ts = event_timestamp(ev, self.opts.time);
        match ev {
            RecordEvent::Restart { cpu_count, .. } => {
                // 区間が終わるので平均を先に出す
                self.flush_average(out)?;
                write_restart(out, &ts, restart_cpu_count(*cpu_count, self.file_cpu_nr))?;
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
    /// - `-i` の間隔に十分近くないレコードも表示しない (`next_slice`)
    /// - `count` の上限に達した後のレコードも表示しない
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

        // count の上限に達したら以降は読まない (本家は `do { … } while (*cnt)` を抜ける)。
        // 通常はここへ来る前に [`plan_regions`] が区間の範囲を切っているが、
        // activity ごとに item 数が 0 のレコードがあると本家の `cnt` の減り方が
        // ずれ得るので、ブロック側でも上限を持つ。
        if self.remaining == Some(0) {
            return Ok(());
        }

        // `-i`: ユーザ指定インターバルに十分近いレコードだけを表示する (03 §1.3.4)。
        // **省いたレコードは前サンプルにもならず `avg_count` にも数えない**
        // (§1.10 の落とし穴 #13)。期間端点 (`last_uptime`) も動かさない。
        let admitted = next_slice(
            self.first_uptime.unwrap_or(0),
            view.curr.uptime_cs,
            self.slice_reset,
            self.select.interval(),
            &mut self.slice_last_uptime,
        );
        self.slice_reset = false;
        if !admitted {
            return Ok(());
        }

        let ts = timestamp_of(view.curr, self.opts.time);

        // 本家は `act[i].nr[curr] > 0` のときだけ `f_print` を呼ぶ。
        // item が 1 つも無いレコードはヘッダも `avg_count` も動かさない (03 §8.1)。
        if curr_act.items.is_empty() {
            self.prev_ts = ts;
            return Ok(());
        }

        // `-i` で省いたレコードを前サンプルにしない (本家は `curr` を入れ替えない)。
        let held = self.held.take();
        let (prev_items, itv_cs): (&[ItemSnapshot], u64) = match held.as_ref() {
            Some(h) => (
                h.items.as_slice(),
                interval_cs(h.uptime_cs, view.curr.uptime_cs),
            ),
            None => (
                view.prev
                    .activity(self.view.id)
                    .map(|a| a.items.as_slice())
                    .unwrap_or(&[]),
                view.itv_cs,
            ),
        };

        let rows = self.build_rows(plan, prev_items, &curr_act.items, nr2, itv_cs);
        if !rows.is_empty() {
            self.write_header(out)?;
            let mut buf = String::new();
            for row in &rows {
                buf.clear();
                self.render_row(&mut buf, &ts, row, RowMode::Instant);
                out.write_all(buf.as_bytes())?;
            }
        }
        self.commit_record(rows, &curr_act.items, view.curr.uptime_cs, ts);
        Ok(())
    }

    /// 表示後の区間管理。**行を 1 本も出せなくても必ず通す。**
    ///
    /// 本家の `avg_count` と全期間 `itv` はレコード単位で進む。`-z` の省略は
    /// item ごとの `continue` なので、行が消えてもレコード自体は
    /// 「表示した」扱いになる (03 §2.7 / §8.1)。ここを飛ばすと末尾の
    /// ゼロ区間が `Average:` の分母から落ちる。
    ///
    /// 逆に `-i` で**表示を省いた**レコードはここを通さない。
    /// 本家も `next_slice()` が偽なら `avg_count++` の手前で `return 0` する。
    fn commit_record(
        &mut self,
        rows: Vec<Row>,
        curr_items: &[ItemSnapshot],
        uptime_cs: u64,
        ts: String,
    ) {
        self.displayed += 1;
        self.last_uptime = uptime_cs;
        self.prev_ts = ts;
        // `cnt--` (本家は `if (*cnt > 0) (*cnt)--`。`None` = 無制限は減らない)
        self.remaining = self.remaining.map(|c| c.saturating_sub(1));
        // `-i` で次のレコードを省いても、差分の相手は「最後に表示したサンプル」。
        if !self.select.selects_every_record() {
            self.held = Some(HeldSample {
                uptime_cs,
                items: curr_items.to_vec(),
            });
        }
        // 平均行で集約をやり直すため、CPU は生の item 配列も控える
        if matches!(
            self.view.id,
            ActivityId::CPU | ActivityId::NET_SOFT | ActivityId::IRQ
        ) {
            self.cpu_last = curr_items.to_vec();
        }
        for row in rows {
            self.accumulate(row);
        }
        // 本家の `avg_count` は **activity 単位のグローバルカウンタ**なので、
        // 全 item で分母を揃える (03 §8.1)。item ごとの観測回数で割ると、
        // 途中から現れたデバイス / センサの平均が本家より大きく出る
        // (2 回表示のうち最後だけ現れた 1000 rpm は本家では 500)。
        for state in &mut self.items {
            state.accum.count = self.displayed;
        }
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
        // 本家 `handle_curr_act_stats()` の入口と同じ初期化。
        // `cnt = count`、`next_slice()` の `last_uptime` は基準点から取り直し
        // (`reset = TRUE`)、差分の相手 (`buf[!curr]`) は基準サンプル (`buf[2]`)。
        self.remaining = self.select.count;
        self.slice_reset = true;
        self.slice_last_uptime = 0;
        self.held = (!self.select.selects_every_record()).then(|| HeldSample {
            uptime_cs: snap.uptime_cs,
            items: items.to_vec(),
        });
        // 集約行は前後 2 サンプルが揃って初めて作れるので、基準サンプルでは
        // ファイルの item 0 をそのまま控えるだけにする (平均行では
        // `cpu_first` / `cpu_last` から集約をやり直す)。
        if matches!(
            self.view.id,
            ActivityId::CPU | ActivityId::NET_SOFT | ActivityId::IRQ
        ) {
            self.cpu_first = items.to_vec();
            self.cpu_last = items.to_vec();
        }
        if self.view.layout == Layout::IrqMatrix {
            self.irq_cpu_cols =
                self.irq_columns(items.len(), nr2, plan.text_index("irq_name").is_some());
        }
        for (idx, group) in self.iter_groups(plan, items, nr2, None) {
            self.items.push(ItemState {
                key: self.item_key(plan, &group.primary, idx),
                index: idx,
                label: self.row_label(plan, &group.primary, idx),
                tail: self.tail_text(plan, &group.primary, idx),
                usb_names: self.usb_names(plan, &group.primary),
                first: group.primary.clone(),
                last: group.primary.clone(),
                first_slots: group.slots.clone(),
                last_slots: group.slots,
                accum: ItemAccum::new(self.def.columns.len()),
                displayed: false,
                min: Vec::new(),
                max: Vec::new(),
            });
        }
    }

    /// RESTART をまたいだので区間状態を捨てる。
    ///
    /// `-x` の極値も [`ItemState`] ごと落ちる = **RESTART 区間ごとに初期化**される
    /// (本家の `xinit` → `init_extrema_values()`、03 §1.6.3)。
    fn reset_region(&mut self) {
        self.items.clear();
        self.cpu_first.clear();
        self.cpu_last.clear();
        self.displayed = 0;
        self.first_uptime = None;
        self.header_done = false;
        // 区間が変わるのでサンプル選別もやり直す (次の `adopt_reference` で
        // 基準点が決まるが、基準レコードが来ないまま終わる場合もあるため
        // ここでも初期値に戻す)。
        self.remaining = self.select.count;
        self.slice_reset = true;
        self.slice_last_uptime = 0;
        self.held = None;
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
    ///
    /// `aggregate` を渡すと CPU 系の item 0 をその値で差し替える。
    /// `A_CPU` の集約行は前後 2 サンプルを一緒に見ないと作れないため
    /// ([`compute::aggregate_cpu`])、この関数の中では計算しない。
    fn iter_groups(
        &self,
        plan: &DecodePlan,
        items: &[ItemSnapshot],
        nr2: u32,
        aggregate: Option<&ItemSnapshot>,
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
                    // 集約行を個別 CPU から作り直すのは
                    // `A_CPU` (`get_global_cpu_statistics()`) と
                    // `A_NET_SOFT` (`get_global_soft_statistics()`) だけ。
                    // `A_PWR_CPU` / `A_PWR_FREQ` の item 0 は**収集時に
                    // 平均が入っている**ので、合算すると CPU 数倍になる。
                    //
                    // `A_CPU` は offline 判定と前値補正を挟むため単純和では
                    // 足りず、呼び出し側が `aggregate` を渡してくる (03 §1.4.3)。
                    let primary = match aggregate {
                        Some(agg) if i == 0 => agg.clone(),
                        _ if i == 0 && items.len() > 1 && self.recomputes_aggregate() => {
                            compute::sum_items(width, items.iter().skip(1))
                        }
                        _ => items[i].clone(),
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
                if nr2 == 1 && plan.text_index("irq_name").is_none() {
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
                        if !self.irq_cpu_cols.contains(&c) {
                            continue;
                        }
                        let item = match items.get(c * nr2 + irq) {
                            Some(it) => it.clone(),
                            None => continue,
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

    /// 集約行 (item 0) を個別行の**単純和**で作り直す activity か。
    ///
    /// `A_CPU` はここに入らない。オフライン CPU の現値をそのまま足すと
    /// 「0 になった減少」が他 CPU の増加を相殺するため、
    /// [`compute::aggregate_cpu`] で offline 補正込みに合算する (03 §1.4.3)。
    fn recomputes_aggregate(&self) -> bool {
        matches!(self.view.id, ActivityId::NET_SOFT)
    }

    /// `A_IRQ` で表示する CPU 列の item 添字。
    fn irq_columns(&self, items: usize, nr2: u32, named: bool) -> Vec<usize> {
        let nr2 = nr2.max(1) as usize;
        // CPU 次元を持たない世代は合計列 (`all`) だけ
        if nr2 == 1 && !named {
            return self
                .opts
                .cpus
                .includes(0)
                .then_some(0)
                .into_iter()
                .collect();
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
            HeadItem::Line3 => RowLabel::Number(
                compute::raw_column(plan, item, 0)
                    .unwrap_or(0)
                    .saturating_sub(u64::from(plan.serial_line_offset)) as i64,
            ),
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
            self.irq_cpu_cols =
                self.irq_columns(curr_items.len(), nr2, plan.text_index("irq_name").is_some());
            if nr2 > 1 || plan.text_index("irq_name").is_some() {
                self.irq_cpu_cols.retain(|&cpu| {
                    compute::prepare_item(
                        ActivityId::IRQ,
                        plan,
                        cpu * nr2.max(1) as usize,
                        prev_items,
                        curr_items,
                        ComputeContext::new(itv_cs),
                    )
                    .is_some_and(|p| !p.offline)
                });
            }
        }
        if self.view.layout == Layout::IrqMatrix && self.irq_cpu_cols.is_empty() {
            return Vec::new();
        }
        let width = plan.fields.len();
        let zero = || ItemSnapshot {
            key: None,
            texts: Vec::new(),
            values: vec![Availability::Present(0); width],
        };

        // `A_CPU` の集約行 (`all`) は、前後のサンプルを**一緒に**見て
        // 「CPU ごとに offline 判定 → 前値補正 → 合算」した値を使う (03 §1.4.3)。
        // 前後を別々に単純合計してから差分を取ると、オフラインで現値が 0 に
        // なった CPU の減少が他 CPU の増加を相殺してしまう。
        let cpu_agg = if self.view.id == ActivityId::CPU {
            compute::aggregate_cpu(plan, prev_items, curr_items, false)
        } else if self.view.id == ActivityId::NET_SOFT {
            compute::aggregate_soft(plan, prev_items, curr_items)
        } else {
            None
        };

        let prev_groups =
            self.iter_groups(plan, prev_items, nr2, cpu_agg.as_ref().map(|a| &a.prev));
        let mut rows = Vec::new();

        for (idx, group) in
            self.iter_groups(plan, curr_items, nr2, cpu_agg.as_ref().map(|a| &a.curr))
        {
            // オフライン CPU は行そのものを出さない (本家の `offline_cpu_bitmap`)。
            // 「現在オフライン」だけでなく「前サンプルでオフライン = 差分の
            // 基準値が無い」CPU も対象になる (03 §1.4.3)。
            if cpu_agg.as_ref().is_some_and(|a| a.is_offline(idx)) {
                continue;
            }
            // 未使用の item スロットも行にしない。`file_activity.nr` は
            // 「採取時に確保した枠数」なので空き枠が混ざる (`data-10.3.1` は
            // 12 デバイスに対して枠が 20 ある)。本家は数を数えず、各
            // `print_*_stats()` の先頭で activity 固有の番兵を見て `continue`
            // する ([`compute::is_unused_item`] がその表)。
            // 行を出さない = 累積もしないので、`Average:` 行と平均の分母からも
            // 自動的に外れる ([`SarBlock::average_rows`] は `displayed` を見る)。
            if compute::is_unused_item(self.view.id, idx, plan, &group.primary) {
                continue;
            }
            let key = self.item_key(plan, &group.primary, idx);
            // 前サンプルは**位置ではなく識別子で**対応付ける。
            // 見つからない (新規登録) 場合は全ゼロ構造体を前値にする
            // (本家の check_*_reg() が -1/-2 を返したときと同じ扱い)。
            let matched = prev_groups
                .iter()
                .find(|(pi, pg)| self.item_key(plan, &pg.primary, *pi) == key);
            let (prev_primary, prev_slots) = match matched {
                Some((_, pg))
                    if !compute::item_reregistered(
                        self.view.id,
                        plan,
                        &pg.primary,
                        &group.primary,
                    ) =>
                {
                    (pg.primary.clone(), pg.slots.clone())
                }
                Some(_) => (zero(), vec![zero(); group.slots.len()]),
                None => (zero(), vec![zero(); group.slots.len()]),
            };

            // --dev= / --iface= / --fs=: 名前が一致しないアイテムは出さない
            if !self.name_selected(plan, &group.primary, idx) {
                continue;
            }

            // -z: 前サンプルと同一なら行を出さない (対象 activity のみ)
            if self.opts.zero_omit
                && zero_omit_applies(self.view.id)
                && matched.is_some()
                && self.same_sample(plan, &prev_primary, &group.primary)
            {
                continue;
            }

            // 集約行だけは合算済みの端点と `deltot_jiffies` で計算する
            let cpu_all = cpu_agg
                .as_ref()
                .filter(|_| idx == 0 && self.view.id == ActivityId::CPU);
            let Some(row) = self.make_row(
                plan,
                idx,
                key,
                &group,
                &prev_primary,
                &prev_slots,
                itv_cs,
                cpu_all,
            ) else {
                continue;
            };
            rows.push(row);
        }
        rows
    }

    /// `--dev=` / `--iface=` / `--fs=` / `--int=` / `-I SUM` のアイテム名フィルタ
    /// (`search_list_item()`)。
    ///
    /// フィルタが無い activity では常に真。名前で同一性が決まる 5 activity
    /// (`A_DISK` / `A_NET_DEV` / `A_NET_EDEV` / `A_FS` / `A_IRQ`) だけを対象にする。
    ///
    /// `A_IRQ` は「行 = 割り込み / 列 = CPU」の行列レイアウトなので、
    /// **絞るのは行 (割り込み) で、列 (CPU) は `-P` の担当**である。
    /// 本家 `print_irq_stats()` も割り込みのループの先頭で
    /// `search_list_item(a->item_list, stc_cpuall_irq->irq_name)` を見て
    /// `continue` する (03 §11 の `A_IRQ` / §8.6)。
    /// 比較する名前は行の代表スロット (CPU `all`) の `irq_name` で、
    /// 名前を持たない世代では合成名 (`sum` / 番号) になる
    /// ([`SarBlock::item_name`] / [`check_irq_name_filter`])。
    fn name_selected(&self, plan: &DecodePlan, item: &ItemSnapshot, index: usize) -> bool {
        let Some(list) = self.opts.item_names.get(&self.view.id) else {
            return true;
        };
        if list.is_empty() {
            return true;
        }
        if !matches!(
            self.view.id,
            ActivityId::DISK
                | ActivityId::NET_DEV
                | ActivityId::NET_EDEV
                | ActivityId::FS
                | ActivityId::IRQ
        ) {
            return true;
        }
        let name = self.item_name(plan, item, index);
        list.iter().any(|n| n == &name)
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
    ///
    /// `cpu_all` は `A_CPU` 集約行 (`idx == 0`) のときだけ
    /// [`compute::aggregate_cpu`] の結果を渡す。分母 (`deltot_jiffies`) は
    /// そこから引く。
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
        cpu_all: Option<&compute::CpuAggregate>,
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
            // --- A_CPU: 集約行 / オフライン / tickless の特別扱い ---
            Layout::Cpu if self.view.id == ActivityId::CPU => {
                if let Some(agg) = cpu_all {
                    // 集約行 (SMP)。`prev_primary` / `curr` は既に
                    // offline 補正込みで合算済みで、分母もその合算に使った
                    // CPU の tick 合計 (`deltot_jiffies`) になる。
                    ctx = agg.context(ctx);
                    self.cell_values(plan, prev_primary, curr, &ctx)
                } else {
                    if compute::cpu_is_offline(plan, curr) {
                        // オフライン CPU は行そのものを出さない
                        return None;
                    }
                    let (fixed_prev, mut total) =
                        compute::per_cpu_interval(plan, prev_primary, curr);
                    if idx == 0 {
                        // UP 機 (個別 CPU が無い) の CPU "all"
                        total = total.max(1);
                    } else if total == 0 {
                        // tickless CPU: 計算せず 0.00 × n + %idle = 100.00
                        return Some(Row {
                            key,
                            index: idx,
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
            }
            _ => self.cell_values(plan, prev_primary, curr, &ctx),
        };

        Some(Row {
            key,
            index: idx,
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

    fn render_row(&self, out: &mut String, label: &str, row: &Row, mode: RowMode) {
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
            if mode != RowMode::Instant && !spec.in_average {
                continue;
            }
            let cell = if mode == RowMode::Average {
                spec.avg_cell
            } else {
                spec.cell
            };
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
                    index: row.index,
                    label: row.label.clone(),
                    tail: row.tail.clone(),
                    usb_names: row.usb_names.clone(),
                    first: zero.clone(),
                    last: row.snapshot.clone(),
                    first_slots: vec![zero; row.slots.len()],
                    last_slots: row.slots.clone(),
                    accum: ItemAccum::new(columns),
                    displayed: false,
                    min: Vec::new(),
                    max: Vec::new(),
                });
                self.items.len() - 1
            }
        };

        let plan = self.plan.clone();
        let minmax = self.opts.minmax;
        let state = &mut self.items[idx];
        state.index = row.index;
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
        // `-x` の極値はレコードごとに更新する (03 §1.6.3)。
        // 分母 (`avg_count`) は [`SarBlock::commit_record`] が activity 単位で入れる。
        if minmax {
            state.update_extrema(&row.values);
        }
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
        let itv = interval_cs(self.first_uptime.unwrap_or(0), self.last_uptime);
        if self.view.id == ActivityId::IRQ
            && let Some(plan) = &self.plan
        {
            let named = plan.text_index("irq_name").is_some();
            if plan.nr2 > 1 || named {
                self.irq_cpu_cols = self.irq_columns(self.cpu_last.len(), plan.nr2, named);
                self.irq_cpu_cols.retain(|&cpu| {
                    compute::prepare_item(
                        ActivityId::IRQ,
                        plan,
                        cpu * plan.nr2.max(1) as usize,
                        &self.cpu_first,
                        &self.cpu_last,
                        ComputeContext::new(itv),
                    )
                    .is_some_and(|p| !p.offline)
                });
            }
        }
        let rows = self.average_rows(itv);
        if self.opts.minmax && has_xstats(self.view.id) {
            self.write_minmax_average(out, label, &rows)?;
            self.displayed = 0;
            return Ok(());
        }

        // `-x` 無しのヘッダ行ラベルは平均行と同じ
        if self.view.header == HeaderPolicy::EverySample {
            out.write_all(self.header_for(label).as_bytes())?;
        }

        let mut buf = String::new();
        for (_, row) in &rows {
            buf.clear();
            self.render_row(&mut buf, label, row, RowMode::Average);
            out.write_all(buf.as_bytes())?;
        }
        self.displayed = 0;
        Ok(())
    }

    /// `-x` の平均ブロック (03 §2.6.4 / §8.3)。
    ///
    /// item ごとに
    /// 「`Summary:` ヘッダ → `Minimum:` → `Maximum:` → 平均行」
    /// の 4 行組を繰り返す。ヘッダ行の先頭 `\n` で各組の前に空行が 1 行入る。
    ///
    /// 本家は `print_*_xstats()` が item ごとにヘッダを出し直すため、
    /// ブロック先頭の 1 回だけのヘッダ (`dish` 由来) は出さない。
    fn write_minmax_average<W: Write>(
        &self,
        out: &mut W,
        label: &str,
        rows: &[(usize, Row)],
    ) -> io::Result<()> {
        let mut buf = String::new();
        for (pos, row) in rows {
            let width = row.values.len();
            // 極値が 1 度も更新されていない item は min/max を出さない
            let extrema = self
                .items
                .get(*pos)
                .filter(|st| st.has_extrema())
                .map(|st| {
                    (
                        extrema_values(&st.min, width, f64::MAX),
                        extrema_values(&st.max, width, f64::MIN),
                    )
                });
            if let Some((min, max)) = extrema {
                out.write_all(self.header_for("Summary:").as_bytes())?;
                for (lbl, values) in [("Minimum:", min), ("Maximum:", max)] {
                    let extreme = Row {
                        values,
                        ..row.clone()
                    };
                    buf.clear();
                    self.render_row(&mut buf, lbl, &extreme, RowMode::Extreme);
                    out.write_all(buf.as_bytes())?;
                }
            }
            buf.clear();
            self.render_row(&mut buf, label, row, RowMode::Average);
            out.write_all(buf.as_bytes())?;
        }
        Ok(())
    }

    /// 平均行を組み立てる。戻り値の `usize` は [`SarBlock::items`] の位置
    /// (`-x` の極値を引くのに使う)。
    fn average_rows(&self, itv: u64) -> Vec<(usize, Row)> {
        let Some(plan) = self.plan.as_ref() else {
            return Vec::new();
        };
        // `A_CPU` の `f_print_avg` は `f_print` と同じ関数なので、平均行でも
        // `get_global_cpu_statistics()` を通り直す。つまり集約値・分母・
        // オフライン判定は「最初のサンプル」と「最後に表示したサンプル」の
        // 2 点から作り直される (03 §1.4.3 / §8.1)。
        let cpu_agg = if self.view.id == ActivityId::CPU {
            compute::aggregate_cpu(plan, &self.cpu_first, &self.cpu_last, false)
        } else if self.view.id == ActivityId::NET_SOFT {
            compute::aggregate_soft(plan, &self.cpu_first, &self.cpu_last)
        } else {
            None
        };

        let mut rows = Vec::new();
        for (pos, state) in self.items.iter().enumerate() {
            if !state.displayed {
                continue;
            }
            // 区間の端点でオフラインだった CPU は平均行も出さない
            if cpu_agg.as_ref().is_some_and(|a| a.is_offline(state.index)) {
                continue;
            }
            // 集約行だけは合算済みの端点と `deltot_jiffies` に差し替える
            let aggregated = cpu_agg.as_ref().filter(|_| state.index == 0);
            let (first, last) = match aggregated {
                Some(agg) => (&agg.prev, &agg.curr),
                None => (&state.first, &state.last),
            };
            let zero = compute::zero_item(plan);
            let first = if compute::item_reregistered(self.view.id, plan, first, last) {
                &zero
            } else {
                first
            };
            let tickless = self.view.id == ActivityId::CPU
                && state.index > 0
                && compute::cpu_interval(plan, first, last, compute::CpuRole::Single).is_tickless();
            let mut ctx = ComputeContext::new(itv);
            if let Some(agg) = aggregated.filter(|_| self.view.id == ActivityId::CPU) {
                ctx = agg.context(ctx);
            } else {
                ctx.aggregate_item = state.index == 0;
                if self.view.id == ActivityId::CPU {
                    let total = compute::per_cpu_interval(plan, first, last).1;
                    ctx.tick_total = Some(total.max(1));
                }
            }

            let values: Vec<Computed> = match self.view.layout {
                Layout::Cpu if tickless => self.tickless_cpu_values(),
                Layout::IrqMatrix if plan.nr2 > 1 || plan.text_index("irq_name").is_some() => self
                    .irq_cpu_cols
                    .iter()
                    .filter_map(|&cpu| {
                        let index = cpu * plan.nr2.max(1) as usize + state.index;
                        compute::prepare_item(
                            ActivityId::IRQ,
                            plan,
                            index,
                            &self.cpu_first,
                            &self.cpu_last,
                            ComputeContext::new(itv),
                        )
                        .map(|p| {
                            p.computed(
                                ActivityId::IRQ,
                                irq_col::COUNT,
                                &self.def.columns[irq_col::COUNT],
                                plan,
                                compute::MissingPolicy::Compat,
                            )
                        })
                    })
                    .collect(),
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
                _ => {
                    // 方式 A の差分基準。`A_CPU` は `iowait` / `idle` の
                    // 前値補正後を使う (集約行は既に補正済みの合算値)。
                    let rate_prev = match (self.view.id, aggregated) {
                        (ActivityId::CPU, None) => compute::per_cpu_interval(plan, first, last).0,
                        _ => first.clone(),
                    };
                    self.view
                        .cells
                        .iter()
                        .map(|spec| match spec.avg {
                            AvgKind::Rate => self.value_of(plan, spec.col, &rate_prev, last, &ctx),
                            AvgKind::Mean => state.accum.mean(spec.col),
                            AvgKind::MeanRatio => compute::average_ratio(
                                self.view.id,
                                spec.col,
                                plan,
                                &state.accum,
                                last,
                            ),
                            // 方式 C: 最後に観測した値をそのまま再掲する
                            AvgKind::Last => self.value_of(plan, spec.col, last, last, &ctx),
                        })
                        .collect()
                }
            };

            rows.push((
                pos,
                Row {
                    key: state.key.clone(),
                    index: state.index,
                    label: state.label.clone(),
                    values,
                    tail: state.tail.clone(),
                    usb_names: state.usb_names.clone(),
                    snapshot: last.clone(),
                    slots: state.last_slots.clone(),
                },
            ));
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
/// レコードは溜めないので、`out` に [`std::io::BufWriter`] を渡せば
/// そのままストリーミング出力になる。
///
/// 本家 `sar.c: read_stats_from_file()` の骨格をそのまま写している。
///
/// ```text
/// バナーを出す (print_report_hdr)
/// do {                                    ← 区間 (LINUX RESTART で区切られる) のループ
///     do { レコードを読む                  ← 外側ループ
///          RESTART / COMMENT ならその場で出す
///     } while (特殊レコード || 範囲外)
///     fpos = 現在位置                      ← 最初の統計レコードの直後
///     for (activity) {                     ← activity のループ
///         lseek(fpos); サンプルを出す; Average: を出す
///     }                                    ← ループは RESTART を読んだ時点で抜ける
///     区間を終わらせた RESTART を 1 回出す
/// } while (!eof)
/// ```
///
/// 帰結は 3 つある。
///
/// 1. **区間の先頭にあるイベントは全ブロックの前に 1 回だけ**出る
/// 2. **区間内のイベント (`COM`) は各ブロックの中に**ファイル順で出る
/// 3. **区間を終わらせた `LINUX RESTART` は全ブロックの後に 1 回だけ**出る。
///    したがって出力は「区間 → activity」の順に入れ子になり、
///    activity ごとに RESTART 行が繰り返されることはない
///
/// 3 は本家の期待出力では突けない (どの `expected.*` も RESTART を
/// 最初の統計レコードより前に 1 個しか持たない)。
/// `tests/record_layout.rs` の自作 fixture で固定している。
pub fn write_report<W: Write>(
    out: &mut W,
    file: &SaFile,
    opts: &SarTextOptions,
    activities: &[ActivityId],
) -> crate::Result<()> {
    write_report_with(out, file, opts, activities, SampleSelect::default())
}

/// [`write_report`] に `-i` / positional `interval` `count` を加えたもの。
///
/// `select` が既定 ([`SampleSelect::default`]) なら [`write_report`] と同一。
///
/// `count` に達した後の扱いだけ骨格が増える。本家は
/// 「全 activity を出し終えた時点で `cnt == 0` なら、**次の `LINUX RESTART`
/// まで読み飛ばす** (`COMMENT` は表示する)」ので (03 §1.10 の外側ループ)、
/// 打ち切り位置より後ろの `COM` 行は各ブロックの中ではなく
/// **全ブロックの後に 1 回だけ**出る。
pub fn write_report_with<W: Write>(
    out: &mut W,
    file: &SaFile,
    opts: &SarTextOptions,
    activities: &[ActivityId],
    select: SampleSelect,
) -> crate::Result<()> {
    use crate::format::file::ScanControl;
    use crate::series::{RecordRange, Selection, WalkItem, walk_items_in};

    let io = |e: io::Error| crate::Error::Io {
        path: file.path().to_path_buf(),
        source: e,
    };

    check_irq_name_filter(file, opts, activities)?;
    write_banner(out, file, opts).map_err(io)?;

    for region in plan_regions(file, opts, select)? {
        // 外側ループに相当。ここで出したイベントは各ブロックでは出さない。
        for line in &region.leading {
            out.write_all(line.as_bytes()).map_err(io)?;
        }

        for id in activities {
            // magic が参照版と違う activity は `sar` 互換出力から丸ごと落とす
            if !file.displays_activity(*id) {
                continue;
            }
            for mut block in SarBlock::blocks_for(*id, opts, region.cpu_nr, select) {
                // 本家は activity ごとに区間の先頭へシークし直し、そのたびに
                // 範囲判定の状態を作り直す。区間ごとに cursor を作れば同じになる。
                let mut cursor = opts.time_filter.cursor();
                // 読み直しは「最初に採用した統計レコードの直後」から始まる。
                // それより前のイベントは `region.leading` が出しているので、
                // 最初のレコードを採るまでイベントを渡さない。
                let mut started = false;
                // 区間外は `walk_items_in` がデコードごと飛ばす。ここで添字を
                // 数えて出力を絞ると、RESTART の個数に比例して無駄が増える。
                let range = RecordRange::new(region.start, region.end);
                walk_items_in(file, &Selection::Only(vec![*id]), range, |item| {
                    match item {
                        WalkItem::Event(ev) => {
                            // 範囲外の特殊レコードは表示しない (`print_special_record()`)
                            if started && cursor.event(ev.ust_time(), ev.time()) {
                                block.event(out, &ev).map_err(io)?;
                            }
                        }
                        WalkItem::Sample(view) => match cursor.sample(view) {
                            Admit::Skip => {}
                            // `Reference` は `-s` に最初に合致したレコード。
                            // `SarBlock` 側が「前サンプルが無い区間の基準」として
                            // 表示せずに採る (`adopt_reference`)。
                            Admit::Reference | Admit::Emit => {
                                started = true;
                                block.record(out, view).map_err(io)?;
                            }
                            // `-e` 超過。このレコードは出さずに打ち切る。
                            Admit::Stop => return Ok(ScanControl::Stop),
                        },
                    }
                    Ok(ScanControl::Continue)
                })?;
                block.finish(out).map_err(io)?;
            }
        }

        // `count` で打ち切った後に残っていた `COM` 行 (本家の読み飛ばしループ)。
        for line in &region.trailing {
            out.write_all(line.as_bytes()).map_err(io)?;
        }

        // 区間を終わらせた `LINUX RESTART` を 1 回だけ出す。
        if let Some(line) = &region.terminator {
            out.write_all(line.as_bytes()).map_err(io)?;
        }
    }
    Ok(())
}

/// `--int=` に名前を指定したが、そのファイルの `A_IRQ` が割り込み名を持たない
/// 場合にエラーにする。
///
/// `stats_irq.irq_name` は v12.5.6 で入ったフィールドである
/// (02 §6.4 / §8 の表)。それより前の世代は「割り込み番号 = 配列添字」しか
/// 持たないので、reSARch はアイテム名を**合成**して出す
/// (index 0 = `sum`、index `i` = `i-1` の 10 進表記。本家 `sadf -c` の変換も
/// 同じ名前を作る)。つまりこの世代では
/// **数字と `sum` だけが `--int=` で指定できる名前**である。
///
/// 本家は旧世代を直接読めないので (`sadf -c` で変換してから読む) この状況に
/// ならない。reSARch は旧世代を直接読む方針なので、名前指定が 1 つも一致し得ない
/// ことを黙って空の結果にせず、**どの指定が使えないか**を告げて止める。
fn check_irq_name_filter(
    file: &SaFile,
    opts: &SarTextOptions,
    activities: &[ActivityId],
) -> crate::Result<()> {
    use crate::series::Selection;
    use crate::series::snapshot::plan_activities;

    let Some(list) = opts.item_names.get(&ActivityId::IRQ) else {
        return Ok(());
    };
    if list.is_empty() || !activities.contains(&ActivityId::IRQ) {
        return Ok(());
    }
    // 合成名で表せない指定 (数字でも `sum` でもないもの)
    let unusable: Vec<&str> = list
        .iter()
        .map(String::as_str)
        .filter(|n| *n != "sum" && !(!n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())))
        .collect();
    if unusable.is_empty() {
        return Ok(());
    }
    let plans = plan_activities(file, &Selection::Only(vec![ActivityId::IRQ]))?;
    let has_names = plans
        .plans
        .iter()
        .find(|p| p.id == ActivityId::IRQ)
        .is_some_and(|p| irq_plan_has_names(&p.plan));
    if has_names {
        return Ok(());
    }
    Err(crate::Error::Other(format!(
        "{}: この世代の A_IRQ は割り込み名 (irq_name) を持たないため \
         --int= の名前指定 {:?} は一致し得ません \
         (irq_name は sysstat 12.5.6 以降。割り込み番号か sum を指定してください)",
        file.path().display(),
        unusable,
    )))
}

/// このファイルの `A_IRQ` に `irq_name` があるか。
///
/// `DecodePlan::text_fields` は「レイアウト記述上の位置」を保つので、
/// 世代に無いフィールドも並びには残る (値は `None`)。
/// **実際に読めるか** ([`crate::layout::plan::FieldPlan::is_available`]) で判定する。
fn irq_plan_has_names(plan: &DecodePlan) -> bool {
    plan.fields
        .iter()
        .any(|f| f.name == "irq_name" && f.is_available())
}

/// `LINUX RESTART` で区切られた 1 区間の出力計画。
///
/// 本家 `sar.c: read_stats_from_file()` の外側ループ 1 周に対応する。
#[derive(Debug, Default)]
struct Region {
    /// この区間の最初のレコードの通し番号 ([`walk_items`](crate::series::walk_items) の呼び出し順)。
    start: usize,
    /// 区間の終わり。**この番号のレコードは含まない**
    /// (区切りの RESTART / EOF、または `count` で打ち切った位置)。
    end: usize,
    /// 区間の先頭で 1 回だけ出す行 (`LINUX RESTART` / `COM`)。
    leading: Vec<String>,
    /// `count` で打ち切った後に読み飛ばした範囲の `COM` 行。
    ///
    /// 本家は `cnt == 0` になると全 activity を出し終えてから次の RESTART まで
    /// 読み飛ばし、その間の `COMMENT` だけを表示する (03 §1.10 / §2.1)。
    /// つまりこれらの行は**全ブロックの後・区切り RESTART の前**に 1 回だけ出る。
    trailing: Vec<String>,
    /// 区間を終わらせた `LINUX RESTART` 行。最後の区間や範囲外なら `None`。
    terminator: Option<String>,
    /// 区間の開始時点で有効な CPU 数 (本家の `file_hdr.sa_cpu_nr` 相当)。
    cpu_nr: Option<u32>,
}

/// 区間の境界と、区間の外で出す行を先に決める。
///
/// 1 パスで済ませるため、行はここで組み立てて文字列として持つ
/// (特殊レコードはファイル全体でも数個なので、溜めても実害がない)。
///
/// 区切りとみなすのは「**その区間で採用した統計レコードが 1 件以上ある**
/// RESTART」だけである。本家の外側ループは最初の統計レコードに達するまで
/// 特殊レコードを出し続けるので、統計レコードより前の RESTART は区切りではなく
/// 先頭イベントになる (`expected.data-11.6.5` の先頭 `LINUX RESTART` がこれ)。
///
/// ## `count` の打ち切り位置
///
/// `count` が指定されていると、本家は各 activity が `count` 行を出した時点で
/// 内側ループを抜け、**全 activity を出し終えてから**次の RESTART まで
/// 読み飛ばす (03 §1.10 / §2.1)。読み飛ばし中の `COMMENT` は表示される。
///
/// どのレコードが `count` を消費するかはレコード列だけで決まる
/// ([`next_slice`] は activity に依存しない) ので、ここで先に決めて
/// `end` を打ち切り位置に、その後ろの `COM` 行を
/// [`Region::trailing`] に置く。
///
/// **activity ごとの item 数は見ていない。** 本家の `cnt` は
/// 「その activity の item 数が 0 のレコード」では減らないため、そういう
/// レコードが混じるファイルでは打ち切り位置が activity 間でずれ得る。
/// その場合でもブロック側が自分の `remaining` で上限を守るので、
/// 行数が `count` を超えることはない。
fn plan_regions(
    file: &SaFile,
    opts: &SarTextOptions,
    select: SampleSelect,
) -> crate::Result<Vec<Region>> {
    use crate::format::file::ScanControl;
    use crate::series::{Selection, WalkItem, walk_items};

    let mut cursor = opts.time_filter.cursor();
    // RESTART レコードが CPU 数を持たない世代のための既定値。
    // 本家の `file_hdr.sa_cpu_nr` と同じで、RESTART を読むたびに更新される。
    let mut cpu_nr = file.header().cpu_nr;
    let mut regions: Vec<Region> = Vec::new();
    let mut cur = Region {
        cpu_nr,
        ..Region::default()
    };
    // この区間で採用した統計レコードがあるか (= 外側ループを抜けたか)。
    let mut adopted = false;
    let mut index = 0usize;
    let mut stopped = false;
    // 区間ごとのサンプル選別の状態 (本家 `handle_curr_act_stats()` の入口と同じ)。
    let mut sel = SliceState::new(select);

    // ここではどの activity もデコードしない (レコード種別しか見ない)。
    walk_items(file, &Selection::Only(Vec::new()), |item| {
        let at = index;
        index += 1;
        match item {
            WalkItem::Event(ev) => {
                let show = cursor.event(ev.ust_time(), ev.time());
                let ts = event_timestamp(&ev, opts.time);
                match &ev {
                    RecordEvent::Restart { cpu_count, .. } => {
                        let line =
                            show.then(|| restart_line(&ts, restart_cpu_count(*cpu_count, cpu_nr)));
                        // 本家は RESTART が持つ CPU 数で `sa_cpu_nr` を上書きする。
                        cpu_nr = cpu_count.or(cpu_nr);
                        if adopted {
                            // 区間の区切り。行は全ブロックの後に出す。
                            cur.end = sel.cut.unwrap_or(at);
                            cur.terminator = line;
                            regions.push(std::mem::replace(
                                &mut cur,
                                Region {
                                    start: at + 1,
                                    cpu_nr,
                                    ..Region::default()
                                },
                            ));
                            adopted = false;
                            sel = SliceState::new(select);
                            // 本家の外側ループは区間ごとに範囲判定をやり直し、
                            // その区間で最初に範囲へ入ったレコードを基準値として
                            // 消費する。cursor も区間ごとに作り直す。
                            cursor = opts.time_filter.cursor();
                        } else {
                            // まだ基準レコードが無い = 外側ループの中。
                            cur.leading.extend(line);
                            cur.cpu_nr = cpu_nr;
                        }
                    }
                    RecordEvent::Comment { text, .. } => {
                        if !show || !opts.comment {
                            return Ok(ScanControl::Continue);
                        }
                        if !adopted {
                            // 区間の先頭 (外側ループの中)。
                            cur.leading.push(comment_line(&ts, text));
                        } else if sel.cut.is_some() {
                            // `count` で打ち切った後の読み飛ばしループ。
                            cur.trailing.push(comment_line(&ts, text));
                        }
                        // 区間に入った後・打ち切り前の COMMENT は各ブロックが出す。
                    }
                }
            }
            WalkItem::Sample(view) => match cursor.sample(view) {
                // 範囲前のレコードはまだ外側ループの中なので、
                // それに続くイベントも先頭イベントとして扱う。
                Admit::Skip => {}
                Admit::Reference | Admit::Emit => {
                    if adopted {
                        sel.feed(at, view.curr.uptime_cs);
                    } else {
                        adopted = true;
                        sel.start(view.curr.uptime_cs);
                    }
                }
                // `-e` 超過。ここで走査ごと打ち切る。
                Admit::Stop => {
                    cur.end = sel.cut.unwrap_or(at);
                    stopped = true;
                    return Ok(ScanControl::Stop);
                }
            },
        }
        Ok(ScanControl::Continue)
    })?;

    if !stopped {
        cur.end = sel.cut.unwrap_or(index);
    }
    regions.push(cur);
    Ok(regions)
}

/// 区間 1 つ分のサンプル選別のなぞり (`count` の打ち切り位置を決めるため)。
///
/// [`SarBlock`] と同じ規則で `next_slice()` と `cnt` を回す。値は出さない。
#[derive(Debug)]
struct SliceState {
    select: SampleSelect,
    /// 区間の基準レコードの uptime (本家の `record_hdr[2].uptime_cs`)。
    uptime_ref: u64,
    last_uptime: u64,
    reset: bool,
    remaining: Option<u64>,
    /// `count` に達した位置の**次**のレコード番号 (= ブロックが読む範囲の終わり)。
    cut: Option<usize>,
}

impl SliceState {
    fn new(select: SampleSelect) -> Self {
        SliceState {
            select,
            uptime_ref: 0,
            last_uptime: 0,
            reset: true,
            remaining: select.count,
            cut: None,
        }
    }

    /// 区間の基準レコード (表示されない 1 本目)。
    fn start(&mut self, uptime_cs: u64) {
        self.uptime_ref = uptime_cs;
        self.last_uptime = 0;
        self.reset = true;
        self.remaining = self.select.count;
        self.cut = None;
    }

    /// 基準レコードより後の統計レコード。
    fn feed(&mut self, at: usize, uptime_cs: u64) {
        // 打ち切りは `count` が無ければ起きない。既定の経路では
        // レコードごとの判定そのものを省く (結果は同じ)。
        if self.select.count.is_none() || self.cut.is_some() {
            return;
        }
        let admitted = next_slice(
            self.uptime_ref,
            uptime_cs,
            self.reset,
            self.select.interval(),
            &mut self.last_uptime,
        );
        self.reset = false;
        if !admitted {
            return;
        }
        if let Some(left) = self.remaining {
            let left = left.saturating_sub(1);
            self.remaining = Some(left);
            if left == 0 {
                // このレコードまでは出す。以降は読み飛ばし。
                self.cut = Some(at + 1);
            }
        }
    }
}

/// 比率列の平均計算に必要な「表示されない入力列」。
fn ratio_inputs(id: ActivityId) -> &'static [usize] {
    match id {
        ActivityId::MEMORY => &[mem_col::KBMEMTOTAL, mem_col::KBSWPTOTAL],
        ActivityId::HUGE => &[huge_col::KBHUGTOTAL],
        _ => &[],
    }
}

/// `-x` の極値ブロックを持つ activity か。
///
/// 43 activity のうち `A_PWR_USB` だけはヘッダ条件に `DISPLAY_MINMAX` が
/// 現れない (03 §9.3 の H 群)。最後に観測したデバイス一覧を再掲するだけで、
/// 極値を出す `print_*_xstats()` が無いためである。
fn has_xstats(id: ActivityId) -> bool {
    id != ActivityId::PWR_USB
}

/// `-z` がアイテム行を省略する activity か (03 §2.7)。
///
/// 本家で同一判定 (`memcmp` / `irq_nr` 比較) を持つのはこの 7 つだけ。
/// 全 activity に同じ判定をかけると、値が変わらない `A_MEMORY` /
/// `A_QUEUE` の行や tickless CPU の行まで消える。
fn zero_omit_applies(id: ActivityId) -> bool {
    matches!(
        id,
        ActivityId::IRQ
            | ActivityId::SERIAL
            | ActivityId::DISK
            | ActivityId::NET_DEV
            | ActivityId::NET_EDEV
            | ActivityId::FS
            | ActivityId::NET_SOFT
    )
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

    /// バナーの日付は既定で `sa_ust_time` 由来、`-t` だけヘッダ日付。
    ///
    /// 本家 `get_file_timestamp_struct()` の分岐 (`PRINT_TRUE_TIME`)。
    /// 本家テストデータ `data-ukwn` は両者が意図的に食い違っており
    /// (`sa_ust_time` = 1568533161 = 2019-09-15、ヘッダ日付は 2019-10-15)、
    /// 期待出力は `09/15/19` である。
    #[test]
    fn banner_date_comes_from_ust_time_unless_true_time() {
        const UST: u64 = 1_568_533_161;
        assert_eq!(banner_date(UST, 2019, 10, 15, TimeStyle::Utc), "09/15/19");
        assert_eq!(
            banner_date(UST, 2019, 10, 15, TimeStyle::Recorded),
            "10/15/19",
            "-t はヘッダの年月日を使う"
        );
    }

    /// `LINUX RESTART` 行の CPU 数はレコード優先・ヘッダ補完。
    #[test]
    fn restart_cpu_count_falls_back_to_the_file_header() {
        // `0x2175`: レコードが新しい CPU 数 (集約スロットを含む 9) を持つ
        assert_eq!(restart_cpu_count(Some(9), Some(3)), 8);
        // 旧世代: レコードに CPU 数が無いのでヘッダの `sa_cpu_nr` を使う
        assert_eq!(restart_cpu_count(None, Some(9)), 8);
        // どちらも無ければ 1 (`sa_cpu_nr > 1 ? sa_cpu_nr - 1 : 1`)
        assert_eq!(restart_cpu_count(None, None), 1);
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
        SarBlock::blocks_for(id, opts, None, SampleSelect::default()).remove(0)
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

    /// item に名前を入れる (`key` と `texts` の両方)。
    ///
    /// 実デコードは `key` と `texts[0]` の**両方**に同じ名前を入れる
    /// ([`DecodePlan::read_texts_into`])。`key` だけ埋めると
    /// [`compute::is_unused_item`] が「インターフェース名が空 = 未使用枠」と
    /// 判定して行が消えるため、fixture でも両方を埋める。
    fn name(plan: &DecodePlan, item: &mut ItemSnapshot, text: &str) {
        item.key = Some(text.into());
        item.texts = vec![None; plan.text_fields.len().max(1)];
        item.texts[0] = Some(text.into());
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
            .make_row(
                &plan,
                0,
                ItemKey::Index(0),
                &group,
                &prev,
                &[],
                10_000,
                None,
            )
            .expect("行が出る");
        let mut s = String::new();
        blk.render_row(&mut s, "09:34:34", &row, RowMode::Instant);
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
            blk.make_row(&plan, 1, ItemKey::Index(1), &group, &zero, &[], 100, None)
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
            .make_row(&plan, 1, ItemKey::Index(1), &group, &same, &[], 100, None)
            .expect("tickless でも行は出る");
        let mut s = String::new();
        blk.render_row(&mut s, "10:00:01", &row, RowMode::Instant);
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
        let blocks = SarBlock::blocks_for(ActivityId::MEMORY, &opts, None, SampleSelect::default());
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].view.pos, 0);
        assert_eq!(blocks[1].view.pos, 1);
        // 未指定なら -r 相当 1 ブロック
        assert_eq!(
            SarBlock::blocks_for(
                ActivityId::MEMORY,
                &SarTextOptions::default(),
                None,
                SampleSelect::default()
            )
            .len(),
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
                None,
            )
            .unwrap();
        let mut s = String::new();
        blk.render_row(&mut s, "09:34:34", &row, RowMode::Instant);
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
            .make_row(&plan, 0, ItemKey::Index(0), &group, &prev, &[], 6_000, None)
            .unwrap();
        let mut s = String::new();
        blk.render_row(&mut s, "10:00:01", &row, RowMode::Instant);
        assert_eq!(
            s,
            "10:00:01            0        78     -2.00         \u{2198}\n"
        );
        // 11 + 10 (BAT) + 10 (%cap) + 10 (cap/min) + 12 (矢印列 = 空白 9 + 3 バイト)
        assert_eq!(s.trim_end_matches('\n').len(), 53);

        // 平均行は status を出さず、%cap が 2 桁になる
        let mut avg = String::new();
        blk.render_row(&mut avg, "Average:", &row, RowMode::Average);
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
            .make_row(&plan, 0, ItemKey::Index(0), &group, &curr, &[], 100, None)
            .unwrap();
        let mut s = String::new();
        blk.render_row(&mut s, "10:00:01", &row, RowMode::Instant);
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
                None,
            )
            .unwrap();
        let mut s = String::new();
        blk.render_row(&mut s, "10:00:01", &row, RowMode::Instant);
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
            .make_row(&plan, 0, ItemKey::Index(0), &group, &prev, &[], 1_000, None)
            .unwrap();
        let mut s = String::new();
        blk.render_row(&mut s, "10:00:01", &row, RowMode::Instant);
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
            .make_row(&plan, 0, ItemKey::Line(64), &group, &prev, &[], 100, None)
            .unwrap();
        let mut s = String::new();
        blk.render_row(&mut s, "10:00:01", &row, RowMode::Instant);
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

        let groups = blk.iter_groups(&plan, &items, 1, None);
        assert_eq!(groups.len(), 1, "既定は集約行のみ");
        let total = compute::raw_column(&plan, &groups[0].1.primary, soft_col::TOTAL).unwrap();
        assert_eq!(total, 400, "ファイルの item 0 ではなく個別 CPU の和");
        assert!(blk.recomputes_aggregate());

        // `A_PWR_CPU` は収集時に平均が入っているので合算しない
        let pwr = block(ActivityId::PWR_CPU, &opts);
        assert!(!pwr.recomputes_aggregate());
        // `A_CPU` は単純和ではなく aggregate_cpu() 経由
        assert!(!block(ActivityId::CPU, &opts).recomputes_aggregate());
    }

    // ---- 区間管理 (CPU 集約 / -z / avg_count / -x) ----

    /// 未使用の item スロットは行にも `Average:` にも出ない。
    ///
    /// `file_activity.nr` は採取時に確保した枠数なので空き枠が混ざる。
    /// 本家は各 `print_*_stats()` の先頭で activity 固有の番兵を見て
    /// `continue` する ([`compute::is_unused_item`])。
    /// `expected.data-10.3.1` の `Average:` 行は 12 デバイスぶんしか無い。
    #[test]
    fn unused_item_slots_are_not_rows() {
        // `A_DISK`: `major + minor == 0` は確保しただけの空き枠
        let opts = SarTextOptions::default();
        let mut blk = block(ActivityId::DISK, &opts);
        let plan = plan_for(ActivityId::DISK);
        blk.plan = Some(plan.clone());
        let dev = |major: u64, minor: u64| {
            let mut it = zeros(&plan);
            put(&plan, &mut it, disk_col::MAJOR, major);
            put(&plan, &mut it, disk_col::MINOR, minor);
            it
        };
        let items = vec![dev(8, 0), dev(0, 0), dev(0, 0)];
        adopt(&mut blk, &plan, &items, 0);
        let text = feed(&mut blk, &plan, &items, &items, 100, 100);
        assert_eq!(text.lines().count(), 1, "空き枠が行になった: {text}");
        let avg = tail(&mut blk);
        assert_eq!(
            avg.lines().filter(|l| l.starts_with("Average:")).count(),
            1,
            "空き枠が Average: 行になった: {avg}"
        );

        // `A_NET_SOFT`: 5 カウンタが全 0 の CPU はオフライン。
        // CPU "all" (item 0) は常に表示する。
        let all = SarTextOptions {
            cpus: CpuSelection::All,
            ..Default::default()
        };
        let mut soft = block(ActivityId::NET_SOFT, &all);
        let splan = plan_for(ActivityId::NET_SOFT);
        soft.plan = Some(splan.clone());
        let cpu = |total: u64| {
            let mut it = zeros(&splan);
            put(&splan, &mut it, soft_col::TOTAL, total);
            it
        };
        // [集約スロット, CPU0 (稼働), CPU1 (オフライン)]
        let sitems = vec![cpu(0), cpu(100), cpu(0)];
        adopt(&mut soft, &splan, &sitems, 0);
        let stext = feed(&mut soft, &splan, &sitems, &sitems, 100, 100);
        assert_eq!(
            stext.lines().count(),
            2,
            "オフライン CPU の行が出た (期待は all と CPU0 の 2 行): {stext}"
        );
    }

    /// 表示されない基準サンプルを 1 件食わせる。
    fn adopt(blk: &mut SarBlock, plan: &DecodePlan, items: &[ItemSnapshot], uptime_cs: u64) {
        let snap = Snapshot {
            uptime_cs,
            ..Default::default()
        };
        blk.adopt_reference(plan, items, 0, &snap);
    }

    /// 表示レコードを 1 件食わせ、出力されたデータ行を返す。
    fn feed(
        blk: &mut SarBlock,
        plan: &DecodePlan,
        prev: &[ItemSnapshot],
        curr: &[ItemSnapshot],
        itv_cs: u64,
        uptime_cs: u64,
    ) -> String {
        let rows = blk.build_rows(plan, prev, curr, 0, itv_cs);
        let mut text = String::new();
        for row in &rows {
            blk.render_row(&mut text, "10:00:01", row, RowMode::Instant);
        }
        blk.commit_record(rows, curr, uptime_cs, "10:00:01".to_string());
        text
    }

    /// ブロックを閉じて平均ブロックの文字列を得る。
    fn tail(blk: &mut SarBlock) -> String {
        let mut out = Vec::new();
        blk.finish(&mut out).expect("書き出せる");
        String::from_utf8(out).expect("UTF-8")
    }

    /// オフラインになった CPU の減少が `all` 行の増加を打ち消さない (指摘 1)。
    ///
    /// 前後のサンプルを**それぞれ**単純合計してから差分化すると、
    /// 現値が 0 になった CPU のぶん合計が減り、他 CPU の増加が消える。
    /// 本家は CPU ごとに offline 判定 → 前値で埋める → 合算する (03 §1.4.3)。
    #[test]
    fn cpu_all_row_survives_an_offline_cpu() {
        let opts = SarTextOptions {
            cpus: CpuSelection::All,
            ..Default::default()
        };
        let mut blk = block(ActivityId::CPU, &opts);
        let plan = plan_for(ActivityId::CPU);
        blk.plan = Some(plan.clone());

        let cpu = |user: u64, idle: u64| {
            let mut it = zeros(&plan);
            put(&plan, &mut it, cpu_col::USER, user);
            put(&plan, &mut it, cpu_col::IDLE, idle);
            it
        };
        // item 0 = ファイルの `cpu` 行 (集約行は作り直すので値は使われない)
        let prev = vec![cpu(600, 1_400), cpu(100, 900), cpu(500, 500)];
        // CPU0 は +100 tick user / +900 tick idle、CPU1 はオフライン (全ゼロ)
        let curr = vec![cpu(600, 1_400), cpu(200, 1_800), cpu(0, 0)];

        adopt(&mut blk, &plan, &prev, 0);
        let text = feed(&mut blk, &plan, &prev, &curr, 1_000, 1_000);

        // `all` と CPU0 の 2 行だけ。オフラインの CPU1 は行そのものが出ない
        assert_eq!(
            text,
            "10:00:01        all     10.00      0.00      0.00      0.00      0.00     90.00\n\
             10:00:01          0     10.00      0.00      0.00      0.00      0.00     90.00\n",
            "単純和だと all 行が 0.00 / 100.00 に潰れる"
        );

        // 平均行も同じ集約・同じ分母で出る (端点は最初と最後のサンプル)
        let avg = tail(&mut blk);
        assert_eq!(
            avg,
            "Average:        all     10.00      0.00      0.00      0.00      0.00     90.00\n\
             Average:          0     10.00      0.00      0.00      0.00      0.00     90.00\n",
            "{avg}"
        );
    }

    /// 前サンプルでオフラインだった CPU は基準値が無いので行が出ない。
    #[test]
    fn cpu_returning_from_offline_has_no_row() {
        let opts = SarTextOptions {
            cpus: CpuSelection::All,
            ..Default::default()
        };
        let mut blk = block(ActivityId::CPU, &opts);
        let plan = plan_for(ActivityId::CPU);
        blk.plan = Some(plan.clone());

        let cpu = |user: u64, idle: u64| {
            let mut it = zeros(&plan);
            put(&plan, &mut it, cpu_col::USER, user);
            put(&plan, &mut it, cpu_col::IDLE, idle);
            it
        };
        // CPU1 は前サンプルでオフライン (全ゼロ) → 復帰しても差分が取れない
        let prev = vec![cpu(100, 900), cpu(100, 900), cpu(0, 0)];
        let curr = vec![cpu(200, 1_800), cpu(200, 1_800), cpu(50, 50)];

        adopt(&mut blk, &plan, &prev, 0);
        let text = feed(&mut blk, &plan, &prev, &curr, 1_000, 1_000);
        let rows: Vec<&str> = text.lines().collect();
        assert_eq!(rows.len(), 2, "all と CPU0 だけ: {text}");
        assert!(rows[1].starts_with("10:00:01          0"), "{text}");
        // 復帰した CPU1 は `all` の分母にも入らないので CPU0 の割合がそのまま出る
        assert!(rows[0].contains("     10.00"), "{text}");
    }

    /// `-z` は対象外 activity の行を消さない (指摘 3)。
    #[test]
    fn zero_omit_only_applies_to_seven_activities() {
        for id in [
            ActivityId::IRQ,
            ActivityId::SERIAL,
            ActivityId::DISK,
            ActivityId::NET_DEV,
            ActivityId::NET_EDEV,
            ActivityId::FS,
            ActivityId::NET_SOFT,
        ] {
            assert!(zero_omit_applies(id), "{id} は -z の対象");
        }
        for id in [
            ActivityId::CPU,
            ActivityId::MEMORY,
            ActivityId::QUEUE,
            ActivityId::KTABLES,
            ActivityId::PWR_FAN,
        ] {
            assert!(!zero_omit_applies(id), "{id} は -z の対象外");
        }
        // `-x` の極値を持たないのは A_PWR_USB だけ (03 §9.3 の H 群)
        assert!(!has_xstats(ActivityId::PWR_USB));
        for id in crate::model::KNOWN_ACTIVITIES {
            assert_eq!(
                has_xstats(*id),
                *id != ActivityId::PWR_USB,
                "{id} の -x 対応"
            );
        }

        let opts = SarTextOptions {
            zero_omit: true,
            memory: true,
            ..Default::default()
        };
        // A_MEMORY: 前後で 1 バイトも変わらなくても行は出る
        let mut mem = block(ActivityId::MEMORY, &opts);
        let plan = plan_for(ActivityId::MEMORY);
        mem.plan = Some(plan.clone());
        let mut item = zeros(&plan);
        put(&plan, &mut item, mem_col::KBMEMTOTAL, 4_000);
        put(&plan, &mut item, mem_col::KBMEMFREE, 1_000);
        let items = vec![item];
        assert_eq!(
            mem.build_rows(&plan, &items, &items, 0, 1_000).len(),
            1,
            "-z が A_MEMORY の行を消した"
        );

        // A_NET_DEV: 対象なので同一サンプルの行は消える
        let mut net = block(ActivityId::NET_DEV, &opts);
        let nplan = plan_for(ActivityId::NET_DEV);
        net.plan = Some(nplan.clone());
        let mut iface = zeros(&nplan);
        name(&nplan, &mut iface, "eth0");
        put(&nplan, &mut iface, net_dev_col::RXPCK, 100);
        let nitems = vec![iface];
        assert!(
            net.build_rows(&nplan, &nitems, &nitems, 0, 1_000)
                .is_empty(),
            "-z が A_NET_DEV の同一行を残した"
        );
        // `-z` 以外の理由 (未使用枠の判定) で消えていないことを確かめる
        let mut off = block(ActivityId::NET_DEV, &SarTextOptions::default());
        off.plan = Some(nplan.clone());
        assert_eq!(
            off.build_rows(&nplan, &nitems, &nitems, 0, 1_000).len(),
            1,
            "-z 無しで A_NET_DEV の行が消えた"
        );
    }

    /// `-z` で省略した区間も `Average:` の分母に残る (指摘 3 後半)。
    ///
    /// 「表示を省略する」ことと「期間の端点を進める」ことは別。
    #[test]
    fn zero_omit_still_extends_the_average_interval() {
        let opts = SarTextOptions {
            zero_omit: true,
            ..Default::default()
        };
        let mut blk = block(ActivityId::NET_DEV, &opts);
        let plan = plan_for(ActivityId::NET_DEV);
        blk.plan = Some(plan.clone());

        let iface = |rxpck: u64| {
            let mut it = zeros(&plan);
            name(&plan, &mut it, "eth0");
            put(&plan, &mut it, net_dev_col::RXPCK, rxpck);
            it
        };
        let base = vec![iface(0)];
        let moved = vec![iface(100)];

        adopt(&mut blk, &plan, &base, 0);
        // 1 秒で +100 パケット → 100.00/s
        let first = feed(&mut blk, &plan, &base, &moved, 100, 100);
        assert_eq!(&first[TSW + 10..TSW + 20], "    100.00", "{first}");
        // 次の 1 秒は増分ゼロ → 行は省略されるが区間は 2 秒に伸びる
        let second = feed(&mut blk, &plan, &moved, &moved, 100, 200);
        assert!(second.is_empty(), "-z でゼロ行が出た: {second:?}");

        let text = tail(&mut blk);
        // `-z` では平均ブロックにもヘッダ行が付く (ラベルは `Average:`)
        let avg = text
            .lines()
            .filter(|l| l.starts_with("Average:"))
            .nth(1)
            .expect("Average データ行がある");
        // 100 パケット / 2 秒 = 50.00。端点を進めないと 100.00 になる
        assert_eq!(&avg[TSW + 10..TSW + 20], "     50.00", "{text}");
    }

    /// ゲージの `Average:` の分母は activity 共通の `avg_count` (指摘 4)。
    ///
    /// 2 回の表示のうち最後だけ現れた 1,000 rpm は、
    /// item ごとの観測回数で割ると 1,000、本家方式では 500 になる。
    #[test]
    fn gauge_average_divides_by_the_activity_wide_count() {
        let opts = SarTextOptions::default();
        let mut blk = block(ActivityId::PWR_FAN, &opts);
        let plan = plan_for(ActivityId::PWR_FAN);
        blk.plan = Some(plan.clone());

        // `stats_pwr_fan` の `rpm` は double なのでビット列で入れる
        let fan = |rpm: f64| {
            let mut it = zeros(&plan);
            put(&plan, &mut it, fan_col::RPM, rpm.to_bits());
            it
        };
        let one = vec![fan(0.0)];
        let two = vec![fan(0.0), fan(1_000.0)];

        adopt(&mut blk, &plan, &one, 0);
        feed(&mut blk, &plan, &one, &one, 100, 100);
        feed(&mut blk, &plan, &one, &two, 100, 200);

        let text = tail(&mut blk);
        let rows: Vec<&str> = text.lines().filter(|l| l.starts_with("Average:")).collect();
        assert_eq!(rows.len(), 2, "{text}");
        assert_eq!(&rows[0][TSW + 10..TSW + 20], "      0.00", "{text}");
        assert_eq!(
            &rows[1][TSW + 10..TSW + 20],
            "    500.00",
            "item ごとの観測回数で割ると 1000 になる: {text}"
        );
        // 分母は全 item で揃う
        assert!(
            blk.items.iter().all(|st| st.accum.count == 2),
            "avg_count が item ごとにずれている"
        );
    }

    /// `-x` は item ごとに `Summary:` / `Minimum:` / `Maximum:` / 平均行を出す (指摘 2)。
    #[test]
    fn minmax_block_has_summary_minimum_maximum_average() {
        let opts = SarTextOptions {
            minmax: true,
            ..Default::default()
        };
        let mut blk = block(ActivityId::QUEUE, &opts);
        let plan = plan_for(ActivityId::QUEUE);
        blk.plan = Some(plan.clone());

        let queue = |runq: u64| {
            let mut it = zeros(&plan);
            put(&plan, &mut it, queue_col::RUNQ_SZ, runq);
            it
        };
        adopt(&mut blk, &plan, &[queue(1)], 0);
        feed(&mut blk, &plan, &[queue(1)], &[queue(1)], 100, 100);
        feed(&mut blk, &plan, &[queue(1)], &[queue(3)], 100, 200);

        let text = tail(&mut blk);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 5, "{text}");
        // ヘッダ行の先頭 \n による空行 → Summary: ヘッダ → Minimum: → Maximum: → Average:
        assert_eq!(lines[0], "", "ブロックの前に空行 1 行");
        assert!(lines[1].starts_with("Summary:   "), "{text}");
        assert!(lines[1].contains("runq-sz"), "{text}");
        assert!(lines[2].starts_with("Minimum:   "), "{text}");
        assert!(lines[3].starts_with("Maximum:   "), "{text}");
        assert!(lines[4].starts_with("Average:   "), "{text}");
        // runq-sz は 1 と 3 を観測 → 最小 1 / 最大 3 / 平均 2
        // 極値は瞬時値と同じ書式 (整数)、平均だけ小数 0 桁の浮動小数
        assert_eq!(&lines[2][TSW..TSW + 10], "         1", "{text}");
        assert_eq!(&lines[3][TSW..TSW + 10], "         3", "{text}");
        assert_eq!(&lines[4][TSW..TSW + 10], "         2", "{text}");
    }

    /// `-x` の極値は `LINUX RESTART` ごとに初期化される。
    #[test]
    fn minmax_resets_on_restart() {
        let opts = SarTextOptions {
            minmax: true,
            ..Default::default()
        };
        let mut blk = block(ActivityId::QUEUE, &opts);
        let plan = plan_for(ActivityId::QUEUE);
        blk.plan = Some(plan.clone());

        let queue = |runq: u64| {
            let mut it = zeros(&plan);
            put(&plan, &mut it, queue_col::RUNQ_SZ, runq);
            it
        };
        adopt(&mut blk, &plan, &[queue(9)], 0);
        feed(&mut blk, &plan, &[queue(9)], &[queue(9)], 100, 100);

        let mut out = Vec::new();
        blk.event(
            &mut out,
            &RecordEvent::Restart {
                ust_time: 0,
                hour: 10,
                minute: 0,
                second: 1,
                cpu_count: Some(2),
            },
        )
        .expect("書き出せる");
        // RESTART 後の区間は 1 だけを観測する
        adopt(&mut blk, &plan, &[queue(1)], 200);
        feed(&mut blk, &plan, &[queue(1)], &[queue(1)], 100, 300);
        let text = tail(&mut blk);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            &lines[3][TSW..TSW + 10],
            "         1",
            "前区間の 9 が残った: {text}"
        );
    }

    // ---- `-i` / interval / count によるサンプル選別 ----

    /// [`next_slice`] の四捨五入は「ちょうど .5 で繰り上げ」。
    ///
    /// 本家の書き方 `(f * 10) - (整数部 * 10) >= 5` を切り捨てに変えると、
    /// 基準点からの経過秒 `entry` が 1 秒ずれてユーザ指定インターバルの
    /// 整数倍を外す。ファイル実インターバルを 1 秒にして判定窓を
    /// `[entry, entry + 1)` に狭め、`entry` の丸めだけで結果が変わる形にしてある。
    #[test]
    fn next_slice_rounds_half_up_like_upstream() {
        // 経過 59.50 秒 → entry = 60。窓 [60, 61) に p*60 = 60 が入る。
        // 切り捨て (entry = 59) だと窓 [59, 60) で 0 も 60 も入らない。
        assert!(next_slice(0, 5_950, false, 60, &mut 5_850));
        // 経過 59.49 秒 → entry = 59。どの 60 の倍数も窓に入らない。
        assert!(!next_slice(0, 5_949, false, 60, &mut 5_849));
        // 経過 60.50 秒 → entry = 61。窓 [61, 62) に 60 も 120 も入らない。
        assert!(!next_slice(0, 6_050, false, 60, &mut 5_950));
    }

    /// 連続するレコードに対する採否の列が本家と一致する。
    ///
    /// 期待値は本家 `sa_common.c: next_slice()` をそのまま C で回して採った
    /// (`cc` でビルドして 495 ケースを突き合わせ、全一致を確認したうちの 2 本)。
    #[test]
    fn next_slice_sequence_matches_upstream() {
        // 基準点・インターバル・レコード間隔 (cs) → 採否の列
        let cases: [(u64, u64, u64, &str); 2] = [
            // 10 秒刻みのファイルを -i 60 で読む: 6 本ごとに 1 本
            (100, 60, 1_000, "0000010000010000010000010000010000010000"),
            // 2.5 秒刻み・-i 7・uptime が 2^32 を跨いでいる
            (1 << 32, 7, 250, "0010110100100100101101001001001011010010"),
        ];
        for (uptime_ref, interval, step, expect) in cases {
            let mut last = 0u64;
            let mut uptime = uptime_ref;
            let got: String = expect
                .chars()
                .enumerate()
                .map(|(k, _)| {
                    uptime = uptime.wrapping_add(step);
                    let r = next_slice(uptime_ref, uptime, k == 0, interval, &mut last);
                    if r { '1' } else { '0' }
                })
                .collect();
            assert_eq!(
                got, expect,
                "ref={uptime_ref} interval={interval} step={step}"
            );
        }
    }

    /// uptime 差分は `& 0xffffffff` してから秒に直す (03 §1.10 の落とし穴 #12)。
    ///
    /// マスクは 2 箇所ある (`uptime - last_uptime` と `uptime - uptime_ref`)。
    /// どちらも 32bit を跨ぐ入力で確かめる。
    #[test]
    fn next_slice_masks_the_uptime_difference_to_32_bits() {
        const WRAP: u64 = 1 << 32;

        // `uptime - uptime_ref` = 2^32 + 200 → マスクして 200 cs = 2 秒。
        // entry = 2 は interval = 2 の倍数なので窓 [2, 3) に入る。
        // マスクしないと entry = 42_949_673 になり、どの倍数も窓に入らない。
        assert!(next_slice(0, WRAP + 200, false, 2, &mut (WRAP + 100)));

        // `uptime - last_uptime` が巻き戻る (last_uptime > uptime) 場合も、
        // 下位 32bit だけを見れば 100 cs = 1 秒。
        // マスクしないと file_interval が天文学的になり窓が崩れる。
        assert!(next_slice(0, 200, false, 2, &mut (WRAP + 100)));

        // `last_uptime == 0` のときは基準点で初期化される (本家の static の初期値)。
        let mut last = 0u64;
        assert!(next_slice(WRAP, WRAP + 100, false, 1, &mut last));
        assert_eq!(last, WRAP + 100, "last_uptime は毎回現サンプルで更新される");
    }

    /// `SliceState` は `count` に達したレコードの次で打ち切り位置を決める。
    #[test]
    fn slice_state_cuts_after_the_count_th_displayed_sample() {
        let mut sel = SliceState::new(SampleSelect {
            interval: 1,
            count: Some(2),
        });
        // 通し番号 10 が基準レコード (表示されない)
        sel.start(0);
        sel.feed(11, 100);
        assert_eq!(sel.cut, None);
        sel.feed(12, 200);
        assert_eq!(sel.cut, Some(13), "2 本表示した直後で打ち切る");
        // 打ち切り後は何も動かない
        sel.feed(13, 300);
        assert_eq!(sel.cut, Some(13));

        // `-i` で省いたレコードは `count` を消費しない
        let mut sel = SliceState::new(SampleSelect {
            interval: 2,
            count: Some(1),
        });
        sel.start(0);
        sel.feed(1, 100); // entry = 1 → 2 の倍数から外れる = 表示しない
        assert_eq!(sel.cut, None, "省いたレコードで count が減った");
        sel.feed(2, 200); // entry = 2 → 表示する
        assert_eq!(sel.cut, Some(3));

        // `count` 未指定なら打ち切らない
        let mut sel = SliceState::new(SampleSelect::default());
        sel.start(0);
        for at in 1..5 {
            sel.feed(at, at as u64 * 100);
        }
        assert_eq!(sel.cut, None);
    }

    /// `record()` に食わせる 1 レコード。時刻は `-t` 相当で埋める。
    fn stat_snapshot(
        id: ActivityId,
        uptime_cs: u64,
        hms: (u8, u8, u8),
        items: Vec<ItemSnapshot>,
    ) -> Snapshot {
        Snapshot {
            valid: true,
            kind: None,
            ust_time: 0,
            uptime_cs,
            hour: hms.0,
            minute: hms.1,
            second: hms.2,
            activities: vec![crate::series::ActivitySnapshot {
                id,
                index: 0,
                nr: items.len() as u32,
                nr2: 1,
                items,
            }],
        }
    }

    fn interval_view<'a>(
        prev: &'a Snapshot,
        curr: &'a Snapshot,
        plans: &'a [crate::series::snapshot::ActivityPlan],
    ) -> IntervalView<'a> {
        IntervalView {
            prev,
            curr,
            itv_cs: interval_cs(prev.uptime_cs, curr.uptime_cs),
            has_prev: true,
            continuous: true,
            events: &[],
            plans,
        }
    }

    /// `-i 2` で 1 秒刻みのファイルを読むと 1 本飛ばしで表示され、
    /// **省いたサンプルは `Average:` の分母に入らない** (03 §1.10 の落とし穴 #13)。
    #[test]
    fn interval_skips_samples_and_keeps_them_out_of_the_average() {
        let id = ActivityId::QUEUE;
        let plan = plan_for(id);
        let plans = vec![crate::series::snapshot::ActivityPlan {
            index: 0,
            id,
            plan: plan.clone(),
        }];
        let opts = SarTextOptions {
            time: TimeStyle::Recorded,
            ..Default::default()
        };
        let mut blk = SarBlock::blocks_for(
            id,
            &opts,
            None,
            SampleSelect {
                interval: 2,
                count: None,
            },
        )
        .remove(0);

        let queue = |runq: u64| {
            let mut it = zeros(&plan);
            put(&plan, &mut it, queue_col::RUNQ_SZ, runq);
            vec![it]
        };
        // uptime 0〜600 cs (1 秒刻み)、runq-sz は uptime 秒 × 2。
        // 1 本目の対で基準サンプル (uptime 100) が消費されるので、
        // 基準点は 100 cs = 1 秒になる。
        let snaps: Vec<Snapshot> = (0..7u64)
            .map(|k| stat_snapshot(id, k * 100, (10, 0, k as u8), queue(k * 2)))
            .collect();

        let mut out: Vec<u8> = Vec::new();
        for pair in snaps.windows(2) {
            let view = interval_view(&pair[0], &pair[1], &plans);
            blk.record(&mut out, &view).expect("書ける");
        }
        let text = String::from_utf8(out).expect("UTF-8");
        let rows: Vec<&str> = text
            .lines()
            .filter(|l| l.starts_with("10:00:") && !l.contains("runq-sz"))
            .collect();
        assert_eq!(
            rows.len(),
            2,
            "基準点から 2 秒 / 4 秒の 2 本だけが -i 2 の倍数に十分近い: {text}"
        );
        assert!(rows[0].starts_with("10:00:03"), "{text}");
        assert!(rows[1].starts_with("10:00:05"), "{text}");
        assert_eq!(blk.displayed, 2, "省いたサンプルを数えている");

        // ゲージの平均は (6 + 10) / 2 = 8 (`runq-sz` は整数列)。
        // 省いた 3 本まで分母に数えると 16 / 5 = 3 になる。
        let avg = tail(&mut blk);
        let line = avg
            .lines()
            .find(|l| l.starts_with("Average:"))
            .expect("Average 行がある");
        assert_eq!(&line[TSW..TSW + 10], "         8", "{avg}");
    }

    /// `-i` で省いたレコードは**前サンプルにもならない**。
    ///
    /// 本家は `next_slice()` が偽のとき `write_stats()` が 0 を返し
    /// `*curr ^= 1` に届かないため、次に表示するレコードの差分は
    /// 「最後に表示したサンプル」との間で取られる。
    /// レートで見ると、飛ばした 1 本を前サンプルにすると値が 2 倍になる。
    #[test]
    fn interval_pairs_a_displayed_sample_with_the_previous_displayed_one() {
        let id = ActivityId::PCSW;
        let plan = plan_for(id);
        let plans = vec![crate::series::snapshot::ActivityPlan {
            index: 0,
            id,
            plan: plan.clone(),
        }];
        let opts = SarTextOptions {
            time: TimeStyle::Recorded,
            ..Default::default()
        };
        let mut blk = SarBlock::blocks_for(
            id,
            &opts,
            None,
            SampleSelect {
                interval: 2,
                count: None,
            },
        )
        .remove(0);

        // `processes` は 2 秒ごとに 100 だけ進む階段状のカウンタ。
        // 表示されるレコード (基準点から 2 秒 / 4 秒) の 1 本前では値が動かないので、
        // 前サンプルの取り違えがレートの差として出る。
        let pcsw = |n: u64| {
            let mut it = zeros(&plan);
            put(&plan, &mut it, 0, n);
            vec![it]
        };
        let counters = [0u64, 0, 100, 100, 200, 200, 300];
        let snaps: Vec<Snapshot> = counters
            .iter()
            .enumerate()
            .map(|(k, n)| stat_snapshot(id, k as u64 * 100, (10, 0, k as u8), pcsw(*n)))
            .collect();

        let mut out: Vec<u8> = Vec::new();
        for pair in snaps.windows(2) {
            let view = interval_view(&pair[0], &pair[1], &plans);
            blk.record(&mut out, &view).expect("書ける");
        }
        let text = String::from_utf8(out).expect("UTF-8");
        let rows: Vec<&str> = text
            .lines()
            .filter(|l| l.starts_with("10:00:") && !l.contains("proc/s"))
            .collect();
        assert_eq!(rows.len(), 2, "{text}");
        for row in &rows {
            assert_eq!(
                &row[TSW..TSW + 10],
                "     50.00",
                "省いたレコードを前サンプルにすると 0.00 になる: {text}"
            );
        }
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

    /// `-i` / `count` 付きで書き出す。`activities` が空なら全 activity。
    fn render_select(
        name: &str,
        opts: &SarTextOptions,
        select: SampleSelect,
        activities: &[ActivityId],
    ) -> Option<crate::Result<String>> {
        let file = crate::format::SaFile::open(fixture(name)?).ok()?;
        let ids = if activities.is_empty() {
            activities_in_file(&file)
        } else {
            activities.to_vec()
        };
        let mut buf = Vec::new();
        Some(
            write_report_with(&mut buf, &file, opts, &ids, select)
                .map(|()| String::from_utf8(buf).expect("UTF-8")),
        )
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

    /// positional `count` は表示するサンプル数の上限。
    ///
    /// `data-ppc-11.7.2` は統計レコードを 3 本持ち、1 本目が基準サンプルとして
    /// 消費されるので既定では 2 行出る。`count = 1` なら 1 行。
    #[test]
    fn count_limits_the_displayed_samples() {
        let opts = SarTextOptions::default();
        let cpu = [ActivityId::CPU];
        let Some(all) = render_select("data-ppc-11.7.2", &opts, SampleSelect::default(), &cpu)
        else {
            eprintln!("fixture 未取得: スキップ");
            return;
        };
        let all = all.expect("既定で書ける");
        let one = render_select(
            "data-ppc-11.7.2",
            &opts,
            SampleSelect {
                interval: 1,
                count: Some(1),
            },
            &cpu,
        )
        .unwrap()
        .expect("count=1 で書ける");

        let rows = |t: &str| {
            t.lines()
                .filter(|l| l.len() > 19 && &l[11..19] == "     all")
                .count()
        };
        // データ行 + Average 行
        assert_eq!(rows(&all), 3, "{all}");
        assert_eq!(rows(&one), 2, "count=1 でデータ行は 1 本: {one}");
        assert!(one.contains("Average:"), "{one}");
    }

    /// `count` に達したら次の `LINUX RESTART` まで読み飛ばす (`COM` は表示する)。
    ///
    /// `data-12.0.0` は「RESTART → 統計 2 本 → COMMENT」という並びで、
    /// COMMENT は最後の統計レコードより後ろにある。
    /// 既定では各 activity のブロックがそれぞれ COM 行を出す (本家の
    /// `expected.data-12.0.0` も 36 本ある) が、`count` で打ち切ったあとは
    /// 読み飛ばしループが 1 回だけ出す (03 §1.10 / §2.1)。
    #[test]
    fn count_moves_the_trailing_comment_out_of_every_block() {
        let opts = SarTextOptions {
            comment: true,
            ..Default::default()
        };
        let Some(all) = render_select("data-12.0.0", &opts, SampleSelect::default(), &[]) else {
            eprintln!("fixture 未取得: スキップ");
            return;
        };
        let all = all.expect("既定で書ける");
        let one = render_select(
            "data-12.0.0",
            &opts,
            SampleSelect {
                interval: 1,
                count: Some(1),
            },
            &[],
        )
        .unwrap()
        .expect("count=1 で書ける");

        let coms = |t: &str| t.lines().filter(|l| l.contains("  COM ")).count();
        assert!(
            coms(&all) > 1,
            "既定では activity ごとに COM 行が出る: {}",
            coms(&all)
        );
        assert_eq!(coms(&one), 1, "打ち切り後の COM は 1 回だけ");
        // 打ち切り後の COM は全ブロックの後ろ = 最後の `Average:` より後
        let last_avg = one
            .lines()
            .enumerate()
            .filter(|(_, l)| l.starts_with("Average:"))
            .map(|(i, _)| i)
            .last()
            .expect("Average 行がある");
        let com = one
            .lines()
            .position(|l| l.contains("  COM "))
            .expect("COM 行がある");
        assert!(com > last_avg, "COM 行が平均行より前に出た: {one}");
    }

    /// `A_IRQ` のデータ行 / 平均行から割り込み名の列だけを取り出す。
    ///
    /// バナー・`LINUX RESTART`・列見出し (`INTR`) は落とす。
    fn irq_row_names(text: &str) -> Vec<&str> {
        text.lines()
            .filter(|l| l.len() >= TSW + 10 && !l.contains("INTR"))
            .filter(|l| l.starts_with("Average:") || l.as_bytes()[2] == b':')
            .filter(|l| !l.contains("LINUX RESTART"))
            .map(|l| l[TSW..TSW + 10].trim())
            .collect()
    }

    /// `--int=` は割り込み番号で行を絞る (旧世代は合成名 = 番号で一致する)。
    ///
    /// `data-11.6.5` の `A_IRQ` は magic `0x8a` の 1 次元 (`nr` = 489 割り込み /
    /// `nr2` = 1) で `irq_name` を持たない。行の名前は index 0 が `sum`、
    /// index `i` が `i-1` の 10 進表記になる (02 §6.4)。
    #[test]
    fn int_filter_selects_interrupts_by_number() {
        let opts = SarTextOptions {
            item_names: BTreeMap::from([(ActivityId::IRQ, vec!["0".to_string(), "3".to_string()])]),
            ..Default::default()
        };
        let irq = [ActivityId::IRQ];
        let Some(filtered) = render_select("data-11.6.5", &opts, SampleSelect::default(), &irq)
        else {
            eprintln!("fixture 未取得: スキップ");
            return;
        };
        let filtered = filtered.expect("番号指定で書ける");
        // データ行 2 本 + 平均行 2 本
        assert_eq!(
            irq_row_names(&filtered),
            vec!["0", "3", "0", "3"],
            "{filtered}"
        );

        // フィルタ無しなら 489 item 分 (sum + 488 割り込み) の行が出る
        let plain = render_select(
            "data-11.6.5",
            &SarTextOptions::default(),
            SampleSelect::default(),
            &irq,
        )
        .unwrap()
        .expect("フィルタ無しで書ける");
        assert_eq!(
            irq_row_names(&plain).len(),
            489 * 2,
            "フィルタ無しの行数が合わない"
        );
    }

    /// `-I SUM` 相当 (`sum` の 1 件リスト) は総和行だけを残す。
    #[test]
    fn int_filter_accepts_the_synthetic_sum_name() {
        let opts = SarTextOptions {
            item_names: BTreeMap::from([(ActivityId::IRQ, vec!["sum".to_string()])]),
            ..Default::default()
        };
        let Some(text) = render_select(
            "data-11.6.5",
            &opts,
            SampleSelect::default(),
            &[ActivityId::IRQ],
        ) else {
            eprintln!("fixture 未取得: スキップ");
            return;
        };
        let text = text.expect("sum 指定で書ける");
        assert_eq!(irq_row_names(&text), vec!["sum", "sum"], "{text}");
    }

    /// 割り込み名を持たない世代に名前を指定したら**黙って空にせずエラーにする**。
    ///
    /// この世代で指定できるのは合成名 (番号と `sum`) だけである。
    #[test]
    fn int_filter_rejects_a_name_on_a_generation_without_irq_names() {
        let opts = SarTextOptions {
            item_names: BTreeMap::from([(
                ActivityId::IRQ,
                vec!["0".to_string(), "LOC".to_string()],
            )]),
            ..Default::default()
        };
        let Some(result) = render_select(
            "data-11.6.5",
            &opts,
            SampleSelect::default(),
            &[ActivityId::IRQ],
        ) else {
            eprintln!("fixture 未取得: スキップ");
            return;
        };
        let err = result.expect_err("名前指定は拒否される").to_string();
        assert!(err.contains("irq_name"), "{err}");
        assert!(err.contains("LOC"), "{err}");
        assert!(!err.contains("\"0\""), "番号指定は問題にしない: {err}");

        // `A_IRQ` を選んでいなければ何も起きない
        assert!(
            render_select(
                "data-11.6.5",
                &opts,
                SampleSelect::default(),
                &[ActivityId::CPU]
            )
            .unwrap()
            .is_ok()
        );
    }

    /// 現行世代 (`irq_name` あり) では割り込み名で絞れる。
    ///
    /// `irq_name` を持つ `A_IRQ` の本家データは取得物に無いので、
    /// 最新 revision の計画と手組みの item で [`SarBlock::name_selected`] を見る。
    #[test]
    fn int_filter_matches_irq_names_on_the_current_generation() {
        let plan = plan_for(ActivityId::IRQ);
        assert!(
            irq_plan_has_names(&plan),
            "最新 revision は irq_name を持つ"
        );
        let opts = SarTextOptions {
            item_names: BTreeMap::from([(
                ActivityId::IRQ,
                vec!["LOC".to_string(), "3".to_string()],
            )]),
            ..Default::default()
        };
        let blk = block(ActivityId::IRQ, &opts);
        let named = |text: &str| {
            let mut it = zeros(&plan);
            name(&plan, &mut it, text);
            it
        };
        assert!(blk.name_selected(&plan, &named("LOC"), 7));
        assert!(blk.name_selected(&plan, &named("3"), 4));
        assert!(!blk.name_selected(&plan, &named("MCE"), 9));
        // 名前を持つ世代では位置由来の合成名は使わない
        assert!(!blk.name_selected(&plan, &named("sum"), 0));
    }
}

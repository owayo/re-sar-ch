//! 旧フォーマット (`format_magic` = `0x2171` / `0x2173`) から
//! 現行フォーマット (`0x2175`) への変換。本家 `sadf -c` に対応する。
//!
//! 仕様の正本は `docs/format/01-file-format.md` §5 (旧フォーマット変換)。
//! 本家の実装は `sa_conv.c` / `sa_conv.h`。
//!
//! ## なぜ必要か
//!
//! reSARch は旧世代を**直読**できるので、解析のためにこの変換は要らない。
//! 必要なのは「変換して他のツールへ渡す」用途である。本家 `sar` / `sadf` は
//! 旧フォーマットを直接読めず、唯一の道が `sadf -c` による別ファイルへの変換だからである。
//!
//! ## 全体フロー (§5.2)
//!
//! ```mermaid
//! flowchart TD
//!     A["convert(file, options, out)"] --> B{"format_magic"}
//!     B -->|"0x2175"| Z["1 バイトも書かずに<br/>already_current で報告"]
//!     B -->|"0x2170 以下"| E["変換不能としてエラー"]
//!     B -->|"0x2171 / 0x2173"| H["HZ を決める<br/>(明示指定 or USER_HZ=100)"]
//!     H --> M["file_magic (76B) を書く"]
//!     M --> FH["file_header (336B) を書く"]
//!     FH --> FA["file_activity[] (36B × act_nr) を書く"]
//!     FA --> L{"レコードを走査"}
//!     L -->|"R_COMMENT"| C["record_header (24B) + コメント 64B"]
//!     L -->|"R_RESTART"| R["record_header + CPU 数 (__nr_t)"]
//!     L -->|"R_STATS"| S["record_header + activity ごとに<br/>[__nr_t] + item 列"]
//!     C --> L
//!     R --> L
//!     S --> L
//!     L --> O["ConvertReport を返す"]
//! ```
//!
//! ## 本家と意図的に違えた点
//!
//! | 論点 | 本家 | reSARch | 理由 |
//! |---|---|---|---|
//! | 未知 activity id | `exit(1)` (`get_activity_position[<id>]: Internal error`) | 旧バイト列を**素通し**して変換を続ける | 読み側は未知 id を読み飛ばす作りなので、素通しでも境界は保たれる。1 つの未知 id でファイル全体が救えなくなる方が損失が大きい |
//! | `A_CPU` が無いファイル | `CPU activity not found in file. Aborting...` | `sa_cpu_nr = 0` で続行し警告 | 現行形式は CPU 統計必須の前提を撤廃している (§5.17) |
//! | `unsigned long` の拡幅 | `moveto_long_long()` で 32 ビット回転 | 値を `u64` へ正規化してから再直列化 | 回転はバイト列を持ち回る実装の都合。再直列化ならエンディアン不一致でも壊れない (§5.13) |
//! | `A_IRQ` の `irq_nr` 縮小 | ホストのバイト順で縮小するためエンディアン不一致で壊れる (§5.8 の「要検証」3) | 値として読んでから書くので壊れない | 同上 |

pub mod stats;

use std::io::Write;

use crate::error::{Error, Result};
use crate::format::abi::SourceEncoding;
use crate::format::layouts;
use crate::format::reader::{Cursor, OutOfBounds};
use crate::format::registry::{MAX_COMMENT_LEN, RecordKind, StructSource};
use crate::format::selfdesc::{self, TypesNr};
use crate::format::wire::ResolvedLayout;
use crate::format::writer::WriteCursor;
use crate::format::{RawRecord, SaFile, ScanControl};
use crate::model::ActivityId;
use stats::ActivityPlan;

// ===========================================================================
// 書き出す世代の変種 (§「書き出す世代の変種」)
// ===========================================================================

/// 書き出す `format_magic`。
pub const OUT_FORMAT_MAGIC: u16 = 0x2175;

/// 書き出す `file_header` の申告サイズ (現行形 = v12.2.0〜v12.8.0)。
const OUT_HEADER_SIZE: u32 = 336;
/// 書き出す `hdr_types_nr`。
const OUT_HDR_TYPES_NR: [u32; 3] = [1, 1, 12];
/// 書き出す `act_types_nr` / `act_size`。
const OUT_ACT_TYPES_NR: [u32; 3] = [0, 0, 9];
const OUT_ACT_SIZE: u32 = 36;
/// 書き出す `rec_types_nr` / `rec_size`。
const OUT_REC_TYPES_NR: [u32; 3] = [2, 0, 1];
const OUT_REC_SIZE: u32 = 24;

/// `file_magic.upgraded` に書く値の基準バージョン。
///
/// 本家は「変換に使った sysstat の `patchlevel * 256 + sublevel + 1`」を入れる (§5.3)。
/// reSARch は sysstat ではないので、**書き出す形式の基準バージョン**を使う。
/// `docs/format/01-file-format.md` が現行と規定している v12.8.0 の
/// `patchlevel` / `sublevel` がその基準である。
///
/// 0 は「変換されていない生のファイル」を意味するので、非 0 であること自体に
/// 意味がある (`sadf -H` の `Genuine sa datafile: no`、`resarch info` の `upgraded yes`)。
const OUT_UPGRADED_PATCHLEVEL: u32 = 8;
const OUT_UPGRADED_SUBLEVEL: u32 = 0;

/// `file_magic.upgraded` に書く値。
pub const OUT_UPGRADED: u32 = OUT_UPGRADED_PATCHLEVEL * 256 + OUT_UPGRADED_SUBLEVEL + 1;

/// `sysstat_magic`。
const SYSSTAT_MAGIC: u16 = 0xd596;

/// `__nr_t` のバイト数。
const NR_T_SIZE: usize = 4;

/// 旧形式の tick 単位 USER_HZ。CONFIG_HZ (カーネル割り込み頻度) とは異なる。
/// 主要 Linux ABI と直読経路の既定値に合わせる。例外的な ABI は明示指定する。
pub const FALLBACK_HZ: u64 = 100;

// ===========================================================================
// 公開 API
// ===========================================================================

/// 変換の設定。
#[derive(Debug, Clone, Default)]
pub struct ConvertOptions {
    /// 秒あたりの jiffies。
    ///
    /// 旧ヘッダには HZ が保存されていないが、新形式の `file_header.sa_hz` と
    /// `record_header.uptime_cs` の両方に必要になる (§5.6)。
    ///
    /// `None` なら USER_HZ = 100。壁時計と CPU0 の tick は suspend や時刻補正で
    /// 一致しないため、それらの比率から単位を推定しない。
    /// 本家の `sadf -c -O hz=<値>` に相当する明示指定で上書きできる。
    pub hz: Option<u64>,
}

/// HZ をどう決めたか。報告に必ず出す (uptime の誤変換は静かに起こるため)。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HzSource {
    /// 呼び出し側が明示指定した (`sadf -c -O hz=<値>` 相当)。
    Explicit,
    /// USER_HZ の既定値を使った。
    Fallback,
}

impl HzSource {
    pub fn describe(self) -> String {
        match self {
            HzSource::Explicit => "明示指定".to_string(),
            HzSource::Fallback => format!("既定 USER_HZ {FALLBACK_HZ}"),
        }
    }
}

/// 変換の結果。
#[derive(Debug, Clone)]
pub struct ConvertReport {
    /// 入力の `format_magic`。
    pub source_format_magic: u16,
    /// 既に現行形式で、1 バイトも書いていない。
    pub already_current: bool,
    /// 書き出したバイト数。
    pub bytes_written: u64,
    /// 変換した activity 数 (`file_activity[]` の件数)。
    pub activities: usize,
    /// 構造を解釈せず素通しした activity の ID。
    pub opaque_activities: Vec<ActivityId>,
    /// 変換したレコード数。
    pub stats_records: u64,
    pub restart_records: u64,
    pub comment_records: u64,
    /// 採用した HZ とその出所。
    pub hz: u64,
    pub hz_source: HzSource,
    /// 新形式の `file_header.sa_cpu_nr`。
    pub cpu_nr: u32,
    /// 出力側の幅に収まらず切り詰めた統計フィールドの個数。
    ///
    /// 32bit ライタのファイルでは現行形式にも `unsigned long` (有効 4 バイト) が
    /// 残るため、旧 `unsigned long long` の大きな値が落ちることがある。
    pub truncated_values: u64,
    /// 致命的でない注意。
    pub warnings: Vec<String>,
}

impl ConvertReport {
    fn new(source_format_magic: u16) -> Self {
        Self {
            source_format_magic,
            already_current: false,
            bytes_written: 0,
            activities: 0,
            opaque_activities: Vec::new(),
            stats_records: 0,
            restart_records: 0,
            comment_records: 0,
            hz: FALLBACK_HZ,
            hz_source: HzSource::Fallback,
            cpu_nr: 0,
            truncated_values: 0,
            warnings: Vec::new(),
        }
    }

    /// 同じ注意を 2 度積まない。
    ///
    /// レコードごとに起こる事象 (件数 0、item の不足など) をそのまま積むと、
    /// レコード数に比例して同文が並び、報告が読めなくなる。
    fn warn_once(&mut self, message: String) {
        if !self.warnings.contains(&message) {
            self.warnings.push(message);
        }
    }

    pub fn total_records(&self) -> u64 {
        self.stats_records + self.restart_records + self.comment_records
    }
}

/// 変換できる世代か。
pub fn is_convertible(format_magic: u16) -> bool {
    matches!(format_magic, 0x2171 | 0x2173)
}

/// 旧フォーマットを現行フォーマットへ変換して `out` へ書く。
///
/// - 入力が既に `0x2175` なら **1 バイトも書かず**、
///   [`ConvertReport::already_current`] を立てて返す (本家も
///   `File format already up-to-date` を stderr に出して正常終了し、stdout は空になる)。
/// - `0x2170` 以下は変換できない (activity 単位の magic が無く、
///   構造体の意味を確定できない。本家も未実装)。
///
/// 変換後ファイルは**元ファイルのバイト順と `sa_sizeof_long` を保つ** (§5.3)。
///
/// # 出力の粒度
///
/// 書き出しは「ヘッダ 1 節 / レコードヘッダ / item 1 個」の単位で行う。
/// item ごとに構造体の再配置が入るため一括化できない。
/// **`out` は呼び出し側でバッファリングすること** (`BufWriter` など)。
/// 生のファイルハンドルを渡すと item ごとに write システムコールが出る。
pub fn convert(
    file: &SaFile,
    options: &ConvertOptions,
    out: &mut impl Write,
) -> Result<ConvertReport> {
    let src_magic = file.magic();
    let mut report = ConvertReport::new(src_magic.format_magic);

    if src_magic.format_magic == OUT_FORMAT_MAGIC {
        report.already_current = true;
        return Ok(report);
    }
    if !is_convertible(src_magic.format_magic) {
        return Err(Error::Other(format!(
            "{}: このファイルの形式は変換できません \
             (format_magic=0x{:04x}, sysstat {})。\
             変換できるのは 0x2171 (sysstat 9.1.6〜10.2) と 0x2173 (10.3〜11.6) です",
            file.path().display(),
            src_magic.format_magic,
            src_magic.version_string()
        )));
    }

    let enc = *file.encoding();
    let src = SourceLayouts::resolve(file, &enc)?;
    let dst = TargetLayouts::resolve(&enc)?;

    // --- HZ の決定 (§5.6) ---
    let (hz, hz_source) = match options.hz {
        Some(h) if h > 0 => (h, HzSource::Explicit),
        _ => (FALLBACK_HZ, HzSource::Fallback),
    };
    report.hz = hz;
    report.hz_source = hz_source;

    // --- activity ごとの変換計画 ---
    let mut plans: Vec<ActivityPlan> = Vec::with_capacity(file.activities().len());
    for a in file.activities() {
        let plan = ActivityPlan::build(
            a.id,
            (a.magic != 0).then_some(a.magic),
            a.size as usize,
            a.nr.max(0) as u32,
            a.nr2.max(0) as u32,
            &enc,
        )?;
        if !plan.known {
            report.opaque_activities.push(a.id);
        }
        plans.push(plan);
    }
    report.activities = plans.len();

    if !report.opaque_activities.is_empty() {
        let names: Vec<String> = report
            .opaque_activities
            .iter()
            .map(|id| id.display_name())
            .collect();
        report.warnings.push(format!(
            "構造を解釈できない activity ({}) は旧バイト列のまま書き出した \
             (本家 sadf -c はこの場合 exit 1 で中断する)",
            names.join(", ")
        ));
    }

    // --- CPU 数 (§5.4: A_CPU の nr が sa_last_cpu_nr より優先される) ---
    let cpu_nr = match file.activities().iter().find(|a| a.id == ActivityId::CPU) {
        Some(cpu) => cpu.nr.max(0) as u32,
        None => {
            report.warnings.push(
                "activity リストに A_CPU が無いため sa_cpu_nr = 0 とした \
                 (本家 sadf -c は `CPU activity not found in file. Aborting...` で中断する)"
                    .to_string(),
            );
            0
        }
    };
    report.cpu_nr = cpu_nr;

    // --- ヘッダ 3 節を書く ---
    let mut sink = CountingSink::new(out);
    write_file_magic(&mut sink, file, &dst)?;
    write_file_header(&mut sink, file, &src, &dst, hz, cpu_nr)?;
    write_file_activities(&mut sink, file, &dst, &plans)?;

    // --- レコードを書く ---
    let mut ctx = RecordCtx {
        path: file.path(),
        cur: Cursor::new(file.bytes(), enc.endian),
        src: &src,
        dst: &dst,
        plans: &plans,
        hz,
        // `0x2173` の RESTART が A_CPU を含まない場合に備えて現在値を持ち回る
        // (本家も file_hdr.sa_cpu_nr を更新しながら使う)。
        cpu_state: cpu_nr,
        rec_buf: vec![0u8; dst.record_header.size],
        item_buf: Vec::new(),
    };
    let mut scan_err: Option<Error> = None;

    let summary = file.scan(|rec| {
        let r = write_record(&mut sink, &mut ctx, rec, &mut report);
        match r {
            Ok(()) => Ok(ScanControl::Continue),
            Err(e) => {
                scan_err = Some(e);
                Ok(ScanControl::Stop)
            }
        }
    })?;
    if let Some(e) = scan_err {
        return Err(e);
    }

    if !summary.is_exact() {
        report.warnings.push(format!(
            "入力の末尾 {} バイトをレコードとして解釈できなかった (変換対象外)",
            summary.file_size.saturating_sub(summary.end_offset)
        ));
    }

    report.bytes_written = sink.written;
    Ok(report)
}

// ===========================================================================
// レイアウトの解決
// ===========================================================================

/// 入力側 (旧世代) の解決済みレイアウト。
struct SourceLayouts {
    file_header: ResolvedLayout,
    /// `file_header` の先頭オフセット (`file_magic` のサイズ)。
    ///
    /// `0x2171` の `file_magic` は 8 バイトしかない。本家は 76 バイト読んでから
    /// `lseek(fd, -68, SEEK_CUR)` で巻き戻すが (§5.3)、reSARch はファイル全体を
    /// バイト列として持っているので、この世代ごとのサイズを足すだけで済む。
    header_offset: usize,
    /// `record_header` のストライド。旧世代は申告値を持たないので導出値 (48)。
    rec_stride: usize,
}

impl SourceLayouts {
    fn resolve(file: &SaFile, enc: &SourceEncoding) -> Result<Self> {
        let StructSource::Fixed {
            file_header,
            record_header,
            ..
        } = file.spec().structs
        else {
            // `is_convertible` を通ったので固定レイアウト世代しか来ない。
            return Err(Error::Other(
                "変換対象の世代は固定レイアウトのはずだが自己記述形式だった".to_string(),
            ));
        };
        let file_header = file_header.resolve(enc)?;
        let record_header = record_header.resolve(enc)?;
        let Some(magic_layout) = file.spec().file_magic_layout() else {
            // 同上。固定レイアウト世代は必ず `file_magic` を持つ。
            return Err(Error::Other(
                "変換対象の世代は file_magic を持つはずだが持っていなかった".to_string(),
            ));
        };
        let header_offset = magic_layout.resolve(enc)?.size;
        let rec_stride = record_header.size;
        Ok(Self {
            file_header,
            header_offset,
            rec_stride,
        })
    }
}

/// 出力側 (現行形 `0x2175`) の解決済みレイアウト。
struct TargetLayouts {
    file_magic: ResolvedLayout,
    file_header: ResolvedLayout,
    file_activity: ResolvedLayout,
    record_header: ResolvedLayout,
}

impl TargetLayouts {
    fn resolve(enc: &SourceEncoding) -> Result<Self> {
        let file_magic = layouts::FILE_MAGIC_G3.resolve(enc)?;
        let file_header = selfdesc::resolve_file_header(
            TypesNr(OUT_HDR_TYPES_NR),
            OUT_HEADER_SIZE as usize,
            enc,
        )?;
        let file_activity = selfdesc::resolve_file_activity(TypesNr(OUT_ACT_TYPES_NR), enc)?;
        let record_header = selfdesc::resolve_record_header(TypesNr(OUT_REC_TYPES_NR), enc)?;

        // 申告値と導出値が食い違ったまま書くと、読み側が境界を失う。
        // 出力形式の定数はこちらで決めているので、合わないのは実装の誤りである。
        let check = |name: &'static str, got: usize, want: u32| -> Result<()> {
            if got != want as usize {
                return Err(Error::Other(format!(
                    "変換の出力レイアウト {name} が {got} バイトになった (申告値は {want})"
                )));
            }
            Ok(())
        };
        check("file_header", file_header.size, OUT_HEADER_SIZE)?;
        check("file_activity", file_activity.size, OUT_ACT_SIZE)?;
        check("record_header", record_header.size, OUT_REC_SIZE)?;

        Ok(Self {
            file_magic,
            file_header,
            file_activity,
            record_header,
        })
    }
}

// ===========================================================================
// 出力
// ===========================================================================

/// 書き出したバイト数を数える出力ラッパ。
struct CountingSink<'o, W: Write> {
    out: &'o mut W,
    written: u64,
}

impl<'o, W: Write> CountingSink<'o, W> {
    fn new(out: &'o mut W) -> Self {
        Self { out, written: 0 }
    }

    fn put(&mut self, bytes: &[u8]) -> Result<()> {
        self.out.write_all(bytes)?;
        self.written += bytes.len() as u64;
        Ok(())
    }
}

/// 範囲外アクセスを `Error::Truncated` へ写す。
fn truncated<'a>(
    file_label: &'a std::path::Path,
    ctx: &'a str,
) -> impl Fn(OutOfBounds) -> Error + 'a {
    let ctx = ctx.to_string();
    move |e: OutOfBounds| Error::Truncated {
        path: file_label.to_path_buf(),
        context: ctx.clone(),
        need: e.need,
        have: e.len.saturating_sub(e.offset),
    }
}

/// `file_magic` (76 バイト) を書く (§5.3)。
fn write_file_magic<W: Write>(
    sink: &mut CountingSink<'_, W>,
    file: &SaFile,
    dst: &TargetLayouts,
) -> Result<()> {
    let m = file.magic();
    let l = &dst.file_magic;
    let mut buf = vec![0u8; l.size];
    let mut w = WriteCursor::new(&mut buf, file.encoding().endian);
    let oob = truncated(file.path(), "file_magic (出力)");

    let put = |w: &mut WriteCursor<'_>, name: &str, v: u64| -> Result<()> {
        if let Some(f) = l.field(name) {
            w.write_unsigned(0, f, v).map_err(&oob)?;
        }
        Ok(())
    };

    put(&mut w, "sysstat_magic", SYSSTAT_MAGIC as u64)?;
    // 元ファイルのバイト順で書くので、本家の `FORMAT_MAGIC_SWAPPED` (0x7521) を
    // 使い分ける必要はない。ディスク上のバイト像は同じになる。
    put(&mut w, "format_magic", OUT_FORMAT_MAGIC as u64)?;
    // 版数は**入力のまま**。元ファイルを作った sysstat が保持される (§5.3)。
    let (a, b, c, d) = m.version;
    put(&mut w, "sysstat_version", a as u64)?;
    put(&mut w, "sysstat_patchlevel", b as u64)?;
    put(&mut w, "sysstat_sublevel", c as u64)?;
    put(&mut w, "sysstat_extraversion", d as u64)?;
    put(&mut w, "header_size", OUT_HEADER_SIZE as u64)?;
    put(&mut w, "upgraded", OUT_UPGRADED as u64)?;
    put(&mut w, "hdr_types_nr_0", OUT_HDR_TYPES_NR[0] as u64)?;
    put(&mut w, "hdr_types_nr_1", OUT_HDR_TYPES_NR[1] as u64)?;
    put(&mut w, "hdr_types_nr_2", OUT_HDR_TYPES_NR[2] as u64)?;
    // `pad[48]` はゼロ初期化のまま。

    sink.put(&buf)
}

/// `file_header` (336 バイト) を書く (§5.4)。
fn write_file_header<W: Write>(
    sink: &mut CountingSink<'_, W>,
    file: &SaFile,
    src: &SourceLayouts,
    dst: &TargetLayouts,
    hz: u64,
    cpu_nr: u32,
) -> Result<()> {
    let h = file.header();
    let l = &dst.file_header;
    let enc = file.encoding();
    let mut buf = vec![0u8; l.size];
    let mut w = WriteCursor::new(&mut buf, enc.endian);
    let oob = truncated(file.path(), "file_header (出力)");

    let put = |w: &mut WriteCursor<'_>, name: &str, v: u64| -> Result<()> {
        if let Some(f) = l.field(name) {
            w.write_unsigned(0, f, v).map_err(&oob)?;
        }
        Ok(())
    };

    put(&mut w, "sa_ust_time", h.ust_time)?;
    put(&mut w, "sa_hz", hz)?;
    put(&mut w, "sa_cpu_nr", cpu_nr as u64)?;
    put(&mut w, "sa_act_nr", h.act_nr as u64)?;
    // 旧 `sa_year` は 1900 起点の 1 バイト。新形式の `int` にも**同じ生値**を書く
    // (本家も代入するだけ)。読み側は 1000 未満を 1900 起点として解釈する。
    put(&mut w, "sa_year", h.year.saturating_sub(1900).max(0) as u64)?;
    put(&mut w, "act_types_nr_0", OUT_ACT_TYPES_NR[0] as u64)?;
    put(&mut w, "act_types_nr_1", OUT_ACT_TYPES_NR[1] as u64)?;
    put(&mut w, "act_types_nr_2", OUT_ACT_TYPES_NR[2] as u64)?;
    put(&mut w, "rec_types_nr_0", OUT_REC_TYPES_NR[0] as u64)?;
    put(&mut w, "rec_types_nr_1", OUT_REC_TYPES_NR[1] as u64)?;
    put(&mut w, "rec_types_nr_2", OUT_REC_TYPES_NR[2] as u64)?;
    put(&mut w, "act_size", OUT_ACT_SIZE as u64)?;
    put(&mut w, "rec_size", OUT_REC_SIZE as u64)?;
    // `extra_next` は 0 (旧ファイルに拡張レコードは存在しない)。
    put(&mut w, "extra_next", 0)?;
    put(&mut w, "sa_day", h.day as u64)?;
    // 旧 `sa_month` は 0 起点。`FileHeader.month` は 1-12 に正規化済みなので戻す。
    put(&mut w, "sa_month", h.month.saturating_sub(1) as u64)?;
    // `sa_sizeof_long` はそのままコピーする。これが「変換後も元ファイルの ABI を
    // 保つ」ことの宣言であり、`unsigned long` フィールドの有効幅を決める。
    put(&mut w, "sa_sizeof_long", h.sizeof_long as u64)?;

    // uname 由来の文字列は**生バイト列**をコピーする。
    // 表示用の String (不正バイトを U+FFFD にしたもの) を使うと内容が変わる。
    let cur = Cursor::new(file.bytes(), enc.endian);
    for name in ["sa_sysname", "sa_nodename", "sa_release", "sa_machine"] {
        let (Some(sf), Some(df)) = (src.file_header.field(name), l.field(name)) else {
            continue;
        };
        let raw = cur
            .read_bytes(src.header_offset, sf)
            .map_err(truncated(file.path(), name))?;
        w.write_bytes(0, df, raw).map_err(&oob)?;
    }
    // `sa_tzname` は元ファイルに情報が無いので空文字列 (ゼロ初期化のまま。§5.4)。

    sink.put(&buf)
}

/// `file_activity[]` (36 バイト × `sa_act_nr`) を書く (§5.5)。
///
/// **リストの並び替えは行わない。** 旧リストの順序のまま 1:1 で出力する。
fn write_file_activities<W: Write>(
    sink: &mut CountingSink<'_, W>,
    file: &SaFile,
    dst: &TargetLayouts,
    plans: &[ActivityPlan],
) -> Result<()> {
    let l = &dst.file_activity;
    let enc = file.encoding();
    let oob = truncated(file.path(), "file_activity (出力)");

    for (a, plan) in file.activities().iter().zip(plans) {
        let mut buf = vec![0u8; l.size];
        let mut w = WriteCursor::new(&mut buf, enc.endian);

        let put = |w: &mut WriteCursor<'_>, name: &str, v: i64| -> Result<()> {
            if let Some(f) = l.field(name) {
                w.write_signed(0, f, v).map_err(&oob)?;
            }
            Ok(())
        };

        put(&mut w, "id", a.id.0 as i64)?;
        if plan.known {
            // magic / size / types_nr / has_nr は**現行実装の値**を書く。
            // つまり変換後の activity リストは「現行版が書いたもの」と区別が付かない
            // (`file_magic.upgraded` が非 0 である点を除く)。
            put(&mut w, "magic", plan.out_magic as i64)?;
            put(&mut w, "size", plan.dst_size as i64)?;
            put(&mut w, "has_nr", i64::from(plan.has_nr))?;
            put(&mut w, "types_nr_0", plan.out_types_nr[0] as i64)?;
            put(&mut w, "types_nr_1", plan.out_types_nr[1] as i64)?;
            put(&mut w, "types_nr_2", plan.out_types_nr[2] as i64)?;
        } else {
            // 素通し: 旧申告値をそのまま保つ。
            put(&mut w, "magic", a.magic as i64)?;
            put(&mut w, "size", a.size as i64)?;
            put(&mut w, "has_nr", 0)?;
        }
        // `A_IRQ` は 1 次元 → 2 次元行列になったため nr / nr2 を入れ替える (§5.5)。
        let (nr, nr2) = if plan.swapped_dimensions {
            (1i64, plan.out_nr2 as i64)
        } else {
            (a.nr as i64, plan.out_nr2 as i64)
        };
        put(&mut w, "nr", nr)?;
        put(&mut w, "nr2", nr2)?;

        sink.put(&buf)?;
    }
    Ok(())
}

/// レコード変換に必要なものの束。
///
/// 引数の数を抑えるためだけの入れ物である
/// (作業バッファはレコードごとに再確保しないよう持ち回る)。
struct RecordCtx<'a> {
    path: &'a std::path::Path,
    cur: Cursor<'a>,
    src: &'a SourceLayouts,
    dst: &'a TargetLayouts,
    plans: &'a [ActivityPlan],
    hz: u64,
    /// RESTART で更新される CPU 数。
    cpu_state: u32,
    rec_buf: Vec<u8>,
    item_buf: Vec<u8>,
}

/// 1 レコードを書く (§5.6 / §5.7)。
fn write_record<W: Write>(
    sink: &mut CountingSink<'_, W>,
    ctx: &mut RecordCtx<'_>,
    rec: &RawRecord<'_>,
    report: &mut ConvertReport,
) -> Result<()> {
    let endian = ctx.cur.endian();
    let l = &ctx.dst.record_header;

    // --- record_header (48 → 24 バイト) ---
    ctx.rec_buf.fill(0);
    {
        let mut w = WriteCursor::new(&mut ctx.rec_buf, endian);
        let put = |w: &mut WriteCursor<'_>, name: &str, v: u64| {
            if let Some(f) = l.field(name) {
                // バッファは rec_size ちょうどで確保しているので範囲外にならない。
                let _ = w.write_unsigned(0, f, v);
            }
        };
        // `uptime_cs = uptime0 * 100 / HZ`。全 CPU 合計の `uptime` は破棄される (§5.6)。
        let uptime0 = rec.uptime_jiffies.map(|(_, u0)| u0).unwrap_or(0);
        let uptime_cs = uptime0
            .checked_mul(100)
            .map(|v| v / ctx.hz.max(1))
            .unwrap_or(0);
        put(&mut w, "uptime_cs", uptime_cs);
        put(&mut w, "ust_time", rec.ust_time);
        put(&mut w, "extra_next", 0);
        put(&mut w, "record_type", raw_record_type(rec, report) as u64);
        put(&mut w, "hour", rec.hour as u64);
        put(&mut w, "minute", rec.minute as u64);
        put(&mut w, "second", rec.second as u64);
    }
    // 借用を切ってから書き出す (sink は ctx とは別のもの)。
    let header_bytes = std::mem::take(&mut ctx.rec_buf);
    let written = sink.put(&header_bytes);
    ctx.rec_buf = header_bytes;
    written?;

    match rec.kind {
        RecordKind::Comment => {
            // 64 バイトを読んで、末尾を NUL にしてからそのまま書く (§5.7)。
            let at = rec.offset + ctx.src.rec_stride;
            let raw = ctx
                .cur
                .raw(at, MAX_COMMENT_LEN)
                .map_err(truncated(ctx.path, "comment"))?;
            let mut text = [0u8; MAX_COMMENT_LEN];
            text.copy_from_slice(raw);
            text[MAX_COMMENT_LEN - 1] = 0;
            sink.put(&text)?;
            report.comment_records += 1;
        }
        RecordKind::Restart => {
            // 新形式の RESTART ペイロードは CPU 数 (`__nr_t`) 1 個だけ。
            // volatile activity リスト自体は書かれない (§5.7)。
            if let Some(n) = rec.cpu_count {
                ctx.cpu_state = n;
            }
            let mut buf = [0u8; NR_T_SIZE];
            let mut w = WriteCursor::new(&mut buf, endian);
            let _ = w.put_u32(0, ctx.cpu_state);
            sink.put(&buf)?;
            report.restart_records += 1;
        }
        _ => {
            write_stats_payload(sink, ctx, rec, report)?;
            report.stats_records += 1;
        }
    }
    Ok(())
}

/// 統計レコードの本体を書く (§5.7 の R_STATS)。
fn write_stats_payload<W: Write>(
    sink: &mut CountingSink<'_, W>,
    ctx: &mut RecordCtx<'_>,
    rec: &RawRecord<'_>,
    report: &mut ConvertReport,
) -> Result<()> {
    let cur = &ctx.cur;
    let endian = cur.endian();
    let oob = truncated(ctx.path, "統計データ");
    let item_buf = &mut ctx.item_buf;

    for slice in rec.slices {
        let Some(plan) = ctx.plans.get(slice.index) else {
            continue;
        };

        // (1) 書き出す件数を決める (§5.9)
        let count = plan.count(cur, slice.offset, slice.nr);
        let out_count = if plan.swapped_dimensions { 1 } else { count };

        // (2) `has_nr` が真なら `__nr_t` を前置する
        if plan.has_nr {
            if out_count == 0 {
                // 仕様上 0 は正当な件数である (§6.2「`count == 0` は許容される」)。
                // ただし reSARch の読み取り側は現在 0 を拒否するので、
                // 変換結果を自分で読み直せなくなることを知らせる。
                report.warn_once(format!(
                    "{}: 番兵の手前に有効な item が 1 つも無く、レコード内件数 0 を書いた \
                     (形式としては正当だが、reSARch の読み取り側は現在 0 を拒否する)",
                    plan.id.display_name()
                ));
            }
            let mut nr_buf = [0u8; NR_T_SIZE];
            let mut w = WriteCursor::new(&mut nr_buf, endian);
            let _ = w.put_u32(0, out_count);
            sink.put(&nr_buf)?;
        }

        // (3) item を並べる。
        //
        // 出力の item 数は `out_count × out_nr2`。行優先で並ぶので、
        // 切り詰めた行より前の item は旧ファイルと同じ添字になる。
        // `A_IRQ` だけは 1 次元 → 2 次元の付け替えで (行 0 × 割り込み数) になり、
        // 添字はそのまま割り込みの列番号になる。
        let out_items = (out_count as u64).saturating_mul(plan.out_nr2 as u64);
        let src_items = (slice.nr as u64).saturating_mul(slice.nr2 as u64);
        if out_items == 0 {
            continue;
        }

        item_buf.clear();
        item_buf.resize(plan.dst_size, 0);

        for i in 0..out_items {
            item_buf.fill(0);
            if i < src_items {
                let src_base = slice.offset + (i as usize) * plan.src_stride;
                let mut w = WriteCursor::new(item_buf, endian);
                plan.convert_item(
                    cur,
                    src_base,
                    &mut w,
                    0,
                    i as u32,
                    &mut report.truncated_values,
                )
                .map_err(&oob)?;
            }
            sink.put(item_buf)?;
        }

        if out_items > src_items {
            report.warn_once(format!(
                "{}: レコードの item 数 ({src_items}) が申告 nr2 ({}) に足りず、\
                 不足分をゼロで埋めた",
                plan.id.display_name(),
                plan.out_nr2
            ));
        }
    }
    Ok(())
}

/// `record_header.record_type` に書く生の値。
///
/// [`RecordKind`] は種別を分類してしまうので、元の数値へ戻す。
/// 旧世代に拡張レコードは存在しないため、未知値も統計レコードとして扱われている
/// (`RecordKind::UnknownStats`)。
///
/// ただし現行形式では 5〜15 が拡張レコード、16 以上が無効という意味になる。
/// 旧ファイルに 1〜4 以外の種別が入っていた場合、そのまま書くと読み側の解釈が
/// 変わってしまう (本家 `upgrade_record_header()` も値をコピーするだけなので同じ)。
/// 黙って化けさせないよう報告に残す。
fn raw_record_type(rec: &RawRecord<'_>, report: &mut ConvertReport) -> u8 {
    match rec.kind {
        RecordKind::Stats => RecordKind::R_STATS,
        RecordKind::Restart => RecordKind::R_RESTART,
        RecordKind::LastStats => RecordKind::R_LAST_STATS,
        RecordKind::Comment => RecordKind::R_COMMENT,
        RecordKind::UnknownStats(v) | RecordKind::Extra(v) | RecordKind::Invalid(v) => {
            report.warn_once(format!(
                "record_type = {v} のレコードをそのまま書き出した \
                 (旧形式では統計レコード扱いだが、現行形式では 5〜15 が拡張レコード、\
                 16 以上が無効という意味になる)"
            ));
            v
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_two_old_generations_are_convertible() {
        assert!(is_convertible(0x2171));
        assert!(is_convertible(0x2173));
        assert!(!is_convertible(0x2170), "activity 単位の magic が無い世代");
        assert!(!is_convertible(0x2175), "既に現行形式");
    }

    /// `upgraded` は非 0 でなければ「変換済み」と読まれない。
    #[test]
    fn upgraded_is_non_zero_and_decodes_to_the_reference_version() {
        assert_ne!(OUT_UPGRADED, 0);
        assert_eq!(OUT_UPGRADED >> 8, OUT_UPGRADED_PATCHLEVEL);
        assert_eq!((OUT_UPGRADED & 0xff) - 1, OUT_UPGRADED_SUBLEVEL);
    }

    /// 出力レイアウトが 4 通りの符号化すべてで申告値どおりに解けること。
    #[test]
    fn target_layouts_match_the_declared_sizes_on_every_encoding() {
        use crate::format::abi::{Endian, LayoutAbi};
        for endian in [Endian::Little, Endian::Big] {
            for abi in [LayoutAbi::LP64, LayoutAbi::I386, LayoutAbi::ILP32_ALIGN8] {
                let enc = SourceEncoding::new(endian, abi);
                let t = TargetLayouts::resolve(&enc)
                    .unwrap_or_else(|e| panic!("endian={endian} abi={}: {e}", abi.name));
                assert_eq!(t.file_magic.size, 76);
                assert_eq!(t.file_header.size, OUT_HEADER_SIZE as usize);
                assert_eq!(t.file_activity.size, OUT_ACT_SIZE as usize);
                assert_eq!(t.record_header.size, OUT_REC_SIZE as usize);
                // 現行形は `sa_tzname` と `extra_next` を持つ
                assert!(t.file_header.field("sa_tzname").is_some());
                assert!(t.record_header.field("extra_next").is_some());
            }
        }
    }
}

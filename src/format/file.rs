//! `sa` ファイルのオープンとレコード走査。
//!
//! ## 読み取りの流れ
//!
//! 1. 先頭 2 バイトから**バイト順**を決める (`0xd596` がそのまま読めれば LE、
//!    入れ替わって読めれば BE)。
//! 2. `format_magic` で**世代**を決める (レジストリ参照)。
//! 3. `sa_sizeof_long` と `sa_machine` から**生成元 ABI** を決める。
//!    `sa_sizeof_long` は `unsigned long` の幅に依存しない位置にあるため、
//!    ABI を決める前に読める。
//! 4. `file_header` / `file_activity[]` を解決済みレイアウトでデコードする。
//! 5. レコード列を走査する。レコード境界はヘッダ情報から決定的に計算できるので、
//!    バイト列の探索は行わない。
//!
//! ## 性能上の設計
//!
//! - 既定は `mmap`。`read` の往復とバッファコピーを避ける。
//! - [`SaFile::scan`] は内部バッファを再利用し、レコードごとの確保を行わない。
//! - 選択されていない activity は `nr × nr2 × size` を加算するだけでデコードしない。

use std::fs::File;
use std::path::{Path, PathBuf};

use memmap2::Mmap;

use super::abi::{Endian, LayoutAbi, SourceEncoding};
use super::layouts;
use super::reader::{Cursor, OutOfBounds};
use super::registry::{
    self, EXTRA_DESC_SIZE, FormatSpec, MAX_COMMENT_LEN, MAX_EXTRA_NR, MAX_EXTRA_SIZE,
    MAX_ITEM_STRUCT_SIZE, NR_MAX, NR2_MAX, RecordKind, RestartPayload, StructSource,
};
use super::selfdesc::{self, TypesNr};
use super::wire::ResolvedLayout;
use crate::error::{Error, Result};
use crate::model::{ActivityId, MAX_NR_ACT};

/// sysstat のファイル識別子。
pub const SYSSTAT_MAGIC: u16 = 0xd596;

/// 型別フィールド数の健全性上限。これを超える申告は破損とみなす。
const TYPES_NR_LIMIT: u32 = 1024;

/// 1 レコードの最大バイト数 (健全性上限)。
const MAX_RECORD_BYTES: usize = 64 * 1024 * 1024;

/// 旧世代で HZ が不明なときに仮定する値。
pub const DEFAULT_ASSUMED_HZ: u64 = 100;

/// 入力バイト列の保持方法。
#[derive(Debug)]
enum Source {
    Mapped(Mmap),
    Owned(Vec<u8>),
}

impl std::ops::Deref for Source {
    type Target = [u8];
    #[inline]
    fn deref(&self) -> &[u8] {
        match self {
            Source::Mapped(m) => m,
            Source::Owned(v) => v,
        }
    }
}

/// `file_magic` のデコード結果。
#[derive(Debug, Clone)]
pub struct FileMagic {
    pub format_magic: u16,
    pub version: (u8, u8, u8, u8),
    /// `file_header` の実サイズ (`0x2173` 以降)。
    pub header_size: Option<u32>,
    /// `sadf -c` で変換された場合の変換先バージョン情報。
    pub upgraded: Option<u32>,
    /// `file_header` の型別フィールド数 (`0x2175`)。
    pub hdr_types_nr: Option<[u32; 3]>,
}

impl FileMagic {
    pub fn version_string(&self) -> String {
        let (a, b, c, d) = self.version;
        if d == 0 {
            format!("{a}.{b}.{c}")
        } else {
            format!("{a}.{b}.{c}.{d}")
        }
    }
}

/// `file_header` のデコード結果。
///
/// その世代に存在しないフィールドは `None`。0 で埋めない。
#[derive(Debug, Clone)]
pub struct FileHeader {
    /// ファイル作成時刻 (エポック秒)。
    pub ust_time: u64,
    /// 秒あたりの jiffies。`0x2175` 以降のみ記録される。
    pub hz: Option<u64>,
    /// 作成時の CPU 数。
    pub cpu_nr: Option<u32>,
    /// ファイルに含まれる activity 数。
    pub act_nr: u32,
    /// volatile activity 数 (`0x2173` のみ)。RESTART レコードの読み取りに必要。
    pub vol_act_nr: Option<u32>,
    /// 年 (西暦)。
    pub year: i32,
    /// 月 (1-12 に正規化済み)。
    pub month: u8,
    /// 日 (1-31)。
    pub day: u8,
    /// 生成元の `sizeof(long)`。
    pub sizeof_long: u8,
    pub sysname: String,
    pub nodename: String,
    pub release: String,
    pub machine: String,
    /// タイムゾーン名 (v12.5 以降)。
    pub tzname: Option<String>,
    /// `file_activity` の型別フィールド数 (`0x2175`)。
    pub act_types_nr: Option<[u32; 3]>,
    /// `record_header` の型別フィールド数 (`0x2175`)。
    pub rec_types_nr: Option<[u32; 3]>,
    /// `file_activity` の実サイズ (`0x2175`)。
    pub act_size: Option<u32>,
    /// `record_header` の実サイズ (`0x2175`)。
    pub rec_size: Option<u32>,
    /// `extra_desc` が続くか (`0x2175` の v12.5 以降)。
    pub extra_next: Option<u32>,
}

/// `file_activity` のデコード結果。
#[derive(Debug, Clone, Copy)]
pub struct FileActivityEntry {
    pub id: ActivityId,
    /// activity magic。`0` は magic を持たない世代 (`0x2170`)。
    pub magic: u32,
    /// item 数。
    pub nr: i32,
    /// sub-item 数 (行列型 activity)。
    pub nr2: i32,
    /// 1 item のサイズ。**ストライドとして使うのは常にこの申告値**。
    pub size: u32,
    /// レコード内に item 数が前置されるか (`0x2175` の `AO_COUNTED`)。
    pub has_nr: bool,
    /// 統計構造体の型別フィールド数 (`0x2175`)。
    pub types_nr: Option<[u32; 3]>,
}

/// 1 レコード分の生データ。
#[derive(Debug)]
pub struct RawRecord<'a> {
    pub kind: RecordKind,
    /// ファイル内オフセット (レコードヘッダの先頭)。
    pub offset: usize,
    /// 1/100 秒単位の稼働時間。
    ///
    /// `0x2175` は `uptime_cs` をそのまま。旧世代は jiffies 単位の `uptime0` を
    /// HZ で割って換算する。HZ が不明で換算できない場合は `None`。
    pub uptime_cs: Option<u64>,
    /// 旧世代の生の `uptime` / `uptime0` (jiffies)。`0x2175` では `None`。
    pub uptime_jiffies: Option<(u64, u64)>,
    /// レコードの時刻 (エポック秒)。
    pub ust_time: u64,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    /// activity ごとの統計データの位置。
    pub slices: &'a [ActivitySlice],
    /// コメント本文 (`R_COMMENT` のみ)。
    pub comment: Option<&'a str>,
    /// 再起動後の CPU 数 (`R_RESTART` のみ)。
    pub cpu_count: Option<u32>,
}

/// レコード内の 1 activity 分のデータ範囲。
#[derive(Debug, Clone, Copy)]
pub struct ActivitySlice {
    /// `SaFile::activities()` の添字。
    pub index: usize,
    pub id: ActivityId,
    /// このレコードでの item 数 (レコード内前置値で上書きされることがある)。
    pub nr: u32,
    pub nr2: u32,
    /// 1 item のストライド。
    pub stride: usize,
    /// ファイル内オフセット。
    pub offset: usize,
    /// バイト長。
    pub len: usize,
}

/// 走査の継続制御。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanControl {
    Continue,
    Stop,
}

/// 破損への対応方針。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tolerance {
    /// 疑わしいデータを見つけたらエラーで止める (既定)。
    #[default]
    Strict,
    /// 読めたところまでを返し、診断を残す。
    Lenient,
}

/// オープン時の設定。
#[derive(Debug, Clone)]
pub struct OpenOptions {
    /// `mmap` を使うか。
    pub mmap: bool,
    /// 破損への対応方針。
    pub tolerance: Tolerance,
    /// 旧世代で HZ が不明なときに仮定する値。
    pub assumed_hz: u64,
}

impl Default for OpenOptions {
    fn default() -> Self {
        Self {
            mmap: true,
            tolerance: Tolerance::Strict,
            assumed_hz: DEFAULT_ASSUMED_HZ,
        }
    }
}

/// 解析中に見つかった、致命的でない問題。
#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub offset: Option<usize>,
    pub message: String,
}

/// オープン済みの `sa` ファイル。
#[derive(Debug)]
pub struct SaFile {
    path: PathBuf,
    source: Source,
    options: OpenOptions,
    encoding: SourceEncoding,
    spec: &'static FormatSpec,
    magic: FileMagic,
    header: FileHeader,
    activities: Vec<FileActivityEntry>,
    /// 解決済み `record_header` レイアウト。
    record_layout: ResolvedLayout,
    /// 解決済み `file_activity` レイアウト (RESTART の volatile リスト用)。
    activity_layout: ResolvedLayout,
    /// レコード列の開始オフセット。
    records_offset: usize,
    diagnostics: Vec<Diagnostic>,
}

impl SaFile {
    /// 既定の設定でオープンする。
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with(path, OpenOptions::default())
    }

    /// 設定を指定してオープンする。
    pub fn open_with(path: impl AsRef<Path>, options: OpenOptions) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let file = File::open(&path).map_err(|source| Error::Io {
            path: path.clone(),
            source,
        })?;

        let source = if options.mmap {
            // SAFETY: 読み取り専用でマップする。マップ中に他プロセスがファイルを
            // 切り詰めると SIGBUS になり得るため、その懸念がある環境では
            // `mmap: false` (read 方式) を使う。
            match unsafe { Mmap::map(&file) } {
                Ok(m) => Source::Mapped(m),
                Err(_) => Source::Owned(read_all(&file, &path)?),
            }
        } else {
            Source::Owned(read_all(&file, &path)?)
        };

        Self::from_source(path, source, options)
    }

    /// バイト列から直接組み立てる (テスト用)。
    pub fn from_bytes(label: impl Into<PathBuf>, bytes: Vec<u8>) -> Result<Self> {
        Self::from_source(label.into(), Source::Owned(bytes), OpenOptions::default())
    }

    /// バイト列から設定付きで組み立てる (テスト用)。
    pub fn from_bytes_with(
        label: impl Into<PathBuf>,
        bytes: Vec<u8>,
        options: OpenOptions,
    ) -> Result<Self> {
        Self::from_source(label.into(), Source::Owned(bytes), options)
    }

    fn from_source(path: PathBuf, source: Source, options: OpenOptions) -> Result<Self> {
        let bytes: &[u8] = &source;
        let mut diagnostics = Vec::new();

        // --- 1. バイト順の判定 ---
        if bytes.len() < 4 {
            return Err(Error::Truncated {
                path,
                context: "file_magic".into(),
                need: 4,
                have: bytes.len(),
            });
        }
        let head = [bytes[0], bytes[1]];
        let endian = if u16::from_le_bytes(head) == SYSSTAT_MAGIC {
            Endian::Little
        } else if u16::from_be_bytes(head) == SYSSTAT_MAGIC {
            Endian::Big
        } else {
            return Err(Error::NotSysstatFile {
                path,
                found: u16::from_le_bytes(head),
                expected: SYSSTAT_MAGIC,
            });
        };

        // --- 2. 世代の判定 ---
        let format_magic = endian.u16_from([bytes[2], bytes[3]]);
        let spec = registry::lookup(format_magic).ok_or_else(|| Error::UnsupportedFormat {
            path: path.clone(),
            format_magic,
            version: format!("{}.{}.{}", bytes[4], bytes[5], bytes[6]),
        })?;

        // file_magic 自体は `unsigned long` を含まないので、暫定 ABI で解決できる。
        let probe_enc = SourceEncoding::new(endian, LayoutAbi::LP64);
        let magic_layout = spec.file_magic.resolve(&probe_enc)?;
        let cur = Cursor::new(bytes, endian);
        let oob = |e: OutOfBounds, ctx: &str| Error::Truncated {
            path: path.clone(),
            context: ctx.into(),
            need: e.need,
            have: e.len.saturating_sub(e.offset),
        };

        let magic =
            decode_file_magic(&cur, &magic_layout, spec).map_err(|e| oob(e, "file_magic"))?;
        let file_header_offset = magic_layout.size;

        // --- 3. 生成元 ABI の判定 ---
        //
        // `sa_sizeof_long` は `unsigned long` の幅に依存しない位置にある
        // (後続フィールドが aligned(8) で整列している、かつ ul スロットが 8 バイト固定)。
        // したがって暫定 ABI で解決したレイアウトから読める。
        let header_layout_probe =
            resolve_file_header(spec, &magic, &probe_enc).map_err(Error::from)?;
        let sizeof_long_field = header_layout_probe.field("sa_sizeof_long").ok_or_else(|| {
            Error::InconsistentHeader {
                path: path.clone(),
                detail: "sa_sizeof_long フィールドが定義に無い".into(),
            }
        })?;
        let sizeof_long = cur
            .read_unsigned(file_header_offset, sizeof_long_field)
            .map_err(|e| oob(e, "file_header.sa_sizeof_long"))? as u8;

        if sizeof_long != 4 && sizeof_long != 8 {
            return Err(Error::InconsistentHeader {
                path,
                detail: format!("sa_sizeof_long = {sizeof_long} (4 か 8 のはず)"),
            });
        }

        let machine_field = header_layout_probe.field("sa_machine");
        let machine = match machine_field {
            Some(f) => cur
                .read_str(file_header_offset, f)
                .map_err(|e| oob(e, "file_header.sa_machine"))?,
            None => "",
        };

        let abi = match LayoutAbi::infer(machine, sizeof_long) {
            Some(a) => a,
            None => {
                // 32bit で machine 名から判別できない場合、8 バイト整数の
                // アラインメントが ABI によって違うため一意に決められない。
                // 都合のよい候補を黙って選ばない。
                if options.tolerance == Tolerance::Strict {
                    return Err(Error::AmbiguousAbi {
                        path,
                        detail: format!(
                            "machine={machine:?} sizeof_long={sizeof_long} から ABI を特定できない"
                        ),
                    });
                }
                diagnostics.push(Diagnostic {
                    offset: Some(file_header_offset),
                    message: format!(
                        "machine={machine:?} から ABI を特定できないため ilp32-align8 を仮定した"
                    ),
                });
                LayoutAbi::ILP32_ALIGN8
            }
        };
        let encoding = SourceEncoding::new(endian, abi);

        // --- 4. ヘッダと activity リストのデコード ---
        let header_layout = resolve_file_header(spec, &magic, &encoding).map_err(Error::from)?;

        // 自己記述形式では、申告サイズと解決サイズが一致すべき。
        if let Some(declared) = magic.header_size {
            let declared = declared as usize;
            if declared != header_layout.size {
                let detail = format!(
                    "header_size の申告値 {declared} が解決サイズ {} と一致しない",
                    header_layout.size
                );
                if options.tolerance == Tolerance::Strict {
                    return Err(Error::InconsistentHeader { path, detail });
                }
                diagnostics.push(Diagnostic {
                    offset: Some(file_header_offset),
                    message: detail,
                });
            }
        }

        let header = decode_file_header(&cur, &header_layout, file_header_offset, spec, &magic)
            .map_err(|e| oob(e, "file_header"))?;

        if header.act_nr > MAX_NR_ACT {
            return Err(Error::LimitExceeded {
                path,
                what: "sa_act_nr".into(),
                value: header.act_nr as u64,
                limit: MAX_NR_ACT as u64,
            });
        }

        // ust_time の健全性: 2001-09-09 より前は破損とみなす
        if header.ust_time < 1_000_000_000 {
            let detail = format!("sa_ust_time = {} が小さすぎる", header.ust_time);
            if options.tolerance == Tolerance::Strict {
                return Err(Error::InconsistentHeader { path, detail });
            }
            diagnostics.push(Diagnostic {
                offset: Some(file_header_offset),
                message: detail,
            });
        }

        let mut offset = file_header_offset + header_layout.size;

        for (label, t) in [
            ("act_types_nr", header.act_types_nr),
            ("rec_types_nr", header.rec_types_nr),
        ] {
            if let Some(t) = t
                && !TypesNr(t).within(TYPES_NR_LIMIT)
            {
                return Err(Error::LimitExceeded {
                    path,
                    what: label.into(),
                    value: t.iter().copied().max().unwrap_or(0) as u64,
                    limit: TYPES_NR_LIMIT as u64,
                });
            }
        }

        let activity_layout =
            resolve_file_activity(spec, &header, &encoding).map_err(Error::from)?;
        if let Some(declared) = header.act_size {
            let declared = declared as usize;
            if declared != activity_layout.size {
                let detail = format!(
                    "act_size の申告値 {declared} が解決サイズ {} と一致しない",
                    activity_layout.size
                );
                if options.tolerance == Tolerance::Strict {
                    return Err(Error::InconsistentHeader { path, detail });
                }
                diagnostics.push(Diagnostic {
                    offset: Some(offset),
                    message: detail,
                });
            }
        }

        let mut activities = Vec::with_capacity(header.act_nr as usize);
        for i in 0..header.act_nr as usize {
            let base = offset + i * activity_layout.size;
            let entry = decode_file_activity(&cur, &activity_layout, base)
                .map_err(|e| oob(e, "file_activity"))?;
            validate_activity_entry(&entry, &path, options.tolerance, &mut diagnostics, base)?;
            activities.push(entry);
        }
        offset += header.act_nr as usize * activity_layout.size;

        // `extra_desc` チェーンは **file_activity[] の後**に置かれる。
        //
        // 出所: 本家のテストデータ生成ソースの書き込み順が
        // file_magic → file_header → file_activity[] → extra_desc チェーン
        // であることと、実データ (12.1.7) の file_header 直後に
        // file_activity (id=1, magic=0x8b, size=80) が現れることの両方で確認済み。
        //
        // チェーンは「extra_desc (24 バイト固定) + extra_nr × extra_size の本体」を
        // 1 段として、extra_next が 0 の段 (終端は extra_nr = 0) まで続く。
        if header.extra_next.unwrap_or(0) != 0 {
            let len = skip_extra_chain(&cur, offset, &path)?;
            diagnostics.push(Diagnostic {
                offset: Some(offset),
                message: format!(
                    "ファイルヘッダに extra_desc チェーン ({len} バイト) が付随している (内容は未使用)"
                ),
            });
            offset += len;
        }

        let record_layout = resolve_record_header(spec, &header, &encoding).map_err(Error::from)?;
        if let Some(declared) = header.rec_size {
            let declared = declared as usize;
            if declared != record_layout.size {
                let detail = format!(
                    "rec_size の申告値 {declared} が解決サイズ {} と一致しない",
                    record_layout.size
                );
                if options.tolerance == Tolerance::Strict {
                    return Err(Error::InconsistentHeader { path, detail });
                }
                diagnostics.push(Diagnostic {
                    offset: Some(offset),
                    message: detail,
                });
            }
        }

        Ok(Self {
            path,
            source,
            options,
            encoding,
            spec,
            magic,
            header,
            activities,
            record_layout,
            activity_layout,
            records_offset: offset,
            diagnostics,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn bytes(&self) -> &[u8] {
        &self.source
    }

    pub fn encoding(&self) -> &SourceEncoding {
        &self.encoding
    }

    pub fn spec(&self) -> &'static FormatSpec {
        self.spec
    }

    pub fn magic(&self) -> &FileMagic {
        &self.magic
    }

    pub fn header(&self) -> &FileHeader {
        &self.header
    }

    pub fn activities(&self) -> &[FileActivityEntry] {
        &self.activities
    }

    pub fn records_offset(&self) -> usize {
        self.records_offset
    }

    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// 旧世代の jiffies を 1/100 秒へ換算するのに使う HZ。
    pub fn effective_hz(&self) -> u64 {
        self.header.hz.unwrap_or(self.options.assumed_hz)
    }

    /// レコードを走査する。
    ///
    /// 内部バッファを再利用するため、レコードごとの確保が発生しない。
    pub fn scan<F>(&self, mut visit: F) -> Result<ScanSummary>
    where
        F: FnMut(&RawRecord<'_>) -> Result<ScanControl>,
    {
        let bytes: &[u8] = &self.source;
        let cur = Cursor::new(bytes, self.encoding.endian);
        let rec_size = self.record_layout.size;
        let hz = self.effective_hz();
        let self_describing = registry::is_self_describing(self.spec);

        // 0x2173 の RESTART で更新される item 数。初期値は file_activity の申告値。
        let mut nr_state: Vec<u32> = self.activities.iter().map(|a| a.nr.max(0) as u32).collect();
        let mut slices: Vec<ActivitySlice> = Vec::with_capacity(self.activities.len());

        let mut summary = ScanSummary::default();
        let mut offset = self.records_offset;

        let f_uptime = self.record_layout.field("uptime");
        let f_uptime0 = self.record_layout.field("uptime0");
        let f_uptime_cs = self.record_layout.field("uptime_cs");
        let f_ust = self.record_layout.field("ust_time");
        let f_type = self.record_layout.field("record_type");
        let f_hour = self.record_layout.field("hour");
        let f_min = self.record_layout.field("minute");
        let f_sec = self.record_layout.field("second");
        let f_extra_next = self.record_layout.field("extra_next");

        while offset < bytes.len() {
            let remaining = bytes.len() - offset;
            if remaining < rec_size {
                // 末尾の切れ端
                summary.trailing_bytes = remaining;
                if self.options.tolerance == Tolerance::Strict {
                    return Err(Error::Truncated {
                        path: self.path.clone(),
                        context: "record_header".into(),
                        need: rec_size,
                        have: remaining,
                    });
                }
                summary.incomplete = true;
                break;
            }

            let raw_type = f_type
                .map(|f| cur.read_unsigned(offset, f))
                .transpose()
                .map_err(|e| self.truncated(e, "record_header.record_type"))?
                .unwrap_or(0) as u8;
            let kind = RecordKind::from_raw(raw_type, self_describing);

            let ust_time = f_ust
                .map(|f| cur.read_unsigned(offset, f))
                .transpose()
                .map_err(|e| self.truncated(e, "record_header.ust_time"))?
                .unwrap_or(0);

            let (uptime_cs, uptime_jiffies) = match f_uptime_cs {
                Some(f) => {
                    let v = cur
                        .read_unsigned(offset, f)
                        .map_err(|e| self.truncated(e, "record_header.uptime_cs"))?;
                    (Some(v), None)
                }
                None => {
                    let up = match f_uptime {
                        Some(f) => cur
                            .read_unsigned(offset, f)
                            .map_err(|e| self.truncated(e, "record_header.uptime"))?,
                        None => 0,
                    };
                    let up0 = match f_uptime0 {
                        Some(f) => cur
                            .read_unsigned(offset, f)
                            .map_err(|e| self.truncated(e, "record_header.uptime0"))?,
                        None => 0,
                    };
                    // 旧世代は jiffies 単位。HZ で 1/100 秒へ換算する。
                    let cs = if hz == 0 {
                        None
                    } else {
                        up0.checked_mul(100).map(|v| v / hz)
                    };
                    (cs, Some((up, up0)))
                }
            };

            let hour = read_u8(&cur, offset, f_hour);
            let minute = read_u8(&cur, offset, f_min);
            let second = read_u8(&cur, offset, f_sec);

            let mut payload = offset + rec_size;

            // `extra_desc` チェーンの位置はレコード種別で変わる。
            //
            // - 統計レコード / 拡張レコード → record_header の直後 (統計データより前)
            // - RESTART → CPU 数の後
            // - COMMENT → コメント 64 バイトの後
            let extra_next = f_extra_next
                .map(|f| cur.read_unsigned(offset, f))
                .transpose()
                .map_err(|e| self.truncated(e, "record_header.extra_next"))?
                .unwrap_or(0);
            let extra_before_payload = matches!(
                kind,
                RecordKind::Stats
                    | RecordKind::LastStats
                    | RecordKind::UnknownStats(_)
                    | RecordKind::Extra(_)
            );
            if extra_next != 0 && extra_before_payload {
                payload += skip_extra_chain(&cur, payload, &self.path)?;
            }

            let mut comment: Option<&str> = None;
            let mut cpu_count: Option<u32> = None;
            slices.clear();

            if let RecordKind::Invalid(v) = kind {
                return Err(Error::RecordBoundaryLost {
                    path: self.path.clone(),
                    offset: offset as u64,
                    detail: format!("無効な record_type = {v}"),
                });
            }

            match kind {
                RecordKind::Comment => {
                    let raw = cur
                        .raw(payload, MAX_COMMENT_LEN)
                        .map_err(|e| self.truncated(e, "comment"))?;
                    let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
                    comment = std::str::from_utf8(&raw[..end]).ok();
                    payload += MAX_COMMENT_LEN;
                    summary.comments += 1;
                }
                RecordKind::Restart => {
                    match self.spec.restart_payload {
                        RestartPayload::None => {}
                        RestartPayload::CpuCount => {
                            let v = cur
                                .u32_at(payload)
                                .map_err(|e| self.truncated(e, "restart.cpu_count"))?;
                            cpu_count = Some(v);
                            payload += 4;
                        }
                        RestartPayload::VolatileActivityList => {
                            // 各エントリの nr で以降のレコードの item 数が変わる。
                            // 単純にスキップすると以降の位置が全部ずれる。
                            let n = self.header.vol_act_nr.unwrap_or(0) as usize;
                            for i in 0..n {
                                let base = payload + i * self.activity_layout.size;
                                let e = decode_file_activity(&cur, &self.activity_layout, base)
                                    .map_err(|e| self.truncated(e, "restart.volatile_activity"))?;
                                if e.id.0 == 0 || e.nr <= 0 {
                                    // 空スロット
                                    continue;
                                }
                                if let Some(pos) = self.activities.iter().position(|a| a.id == e.id)
                                {
                                    nr_state[pos] = e.nr as u32;
                                    if e.id == ActivityId::CPU {
                                        cpu_count = Some(e.nr as u32);
                                    }
                                }
                            }
                            payload += n * self.activity_layout.size;
                        }
                    }
                    summary.restarts += 1;
                }
                RecordKind::Extra(_) => {
                    // 統計を伴わない拡張レコード。extra チェーンは既に読み飛ばしている。
                    summary.extras += 1;
                }
                _ => {
                    // 統計レコード: activity リスト順にデータが並ぶ
                    for (i, act) in self.activities.iter().enumerate() {
                        let mut nr = nr_state[i];
                        if act.has_nr {
                            // レコード内に item 数が前置される
                            let v = cur
                                .u32_at(payload)
                                .map_err(|e| self.truncated(e, "record.item_count"))?;
                            payload += 4;
                            nr = v;
                        }
                        let nr2 = act.nr2.max(0) as u32;
                        let stride = act.size as usize;

                        // nr × nr2 × size は u32 を超え得る (細工ファイル対策)
                        let items = (nr as u64)
                            .checked_mul(nr2.max(1) as u64)
                            .ok_or_else(|| self.limit("nr * nr2", u64::MAX))?;
                        let len = items
                            .checked_mul(stride as u64)
                            .filter(|v| *v <= MAX_RECORD_BYTES as u64)
                            .ok_or_else(|| {
                                self.limit("nr * nr2 * size", items.saturating_mul(stride as u64))
                            })? as usize;

                        if payload + len > bytes.len() {
                            if self.options.tolerance == Tolerance::Strict {
                                return Err(Error::Truncated {
                                    path: self.path.clone(),
                                    context: format!("{} のデータ", act.id),
                                    need: len,
                                    have: bytes.len().saturating_sub(payload),
                                });
                            }
                            summary.incomplete = true;
                            break;
                        }

                        slices.push(ActivitySlice {
                            index: i,
                            id: act.id,
                            nr,
                            nr2: nr2.max(1),
                            stride,
                            offset: payload,
                            len,
                        });
                        payload += len;
                    }
                    summary.stats += 1;
                }
            }

            // RESTART / COMMENT では extra チェーンがペイロードの後に来る
            if extra_next != 0 && !extra_before_payload {
                payload += skip_extra_chain(&cur, payload, &self.path)?;
            }

            if summary.incomplete {
                break;
            }

            let record = RawRecord {
                kind,
                offset,
                uptime_cs,
                uptime_jiffies,
                ust_time,
                hour,
                minute,
                second,
                slices: &slices,
                comment,
                cpu_count,
            };

            match visit(&record)? {
                ScanControl::Continue => {}
                ScanControl::Stop => {
                    summary.stopped_early = true;
                    return Ok(summary);
                }
            }

            if payload <= offset {
                // 前進しない = レコード境界を失っている
                return Err(Error::RecordBoundaryLost {
                    path: self.path.clone(),
                    offset: offset as u64,
                    detail: "レコード長が 0 と計算された".into(),
                });
            }
            offset = payload;
        }

        summary.end_offset = offset;
        summary.file_size = bytes.len();
        Ok(summary)
    }

    fn truncated(&self, e: OutOfBounds, context: &str) -> Error {
        Error::Truncated {
            path: self.path.clone(),
            context: context.into(),
            need: e.need,
            have: e.len.saturating_sub(e.offset),
        }
    }

    fn limit(&self, what: &str, value: u64) -> Error {
        Error::LimitExceeded {
            path: self.path.clone(),
            what: what.into(),
            value,
            limit: MAX_RECORD_BYTES as u64,
        }
    }
}

/// 走査結果の要約。
#[derive(Debug, Clone, Default)]
pub struct ScanSummary {
    pub stats: u64,
    pub restarts: u64,
    pub comments: u64,
    /// 統計を伴わない拡張レコードの件数。
    pub extras: u64,
    /// 走査が終わったオフセット。
    pub end_offset: usize,
    pub file_size: usize,
    /// 末尾に残ったバイト数 (レコードとして解釈できなかった分)。
    pub trailing_bytes: usize,
    /// 不完全なレコードで打ち切ったか。
    pub incomplete: bool,
    /// コールバックが早期終了を指示したか。
    pub stopped_early: bool,
}

impl ScanSummary {
    pub fn total_records(&self) -> u64 {
        self.stats + self.restarts + self.comments + self.extras
    }

    /// ファイル末尾まで余りなく読めたか。
    pub fn is_exact(&self) -> bool {
        !self.incomplete && !self.stopped_early && self.end_offset == self.file_size
    }
}

// ===========================================================================
// デコード補助
// ===========================================================================

/// `file_activity` エントリの健全性を検査する。
///
/// 本家が `check_file_actlst()` で行う検証に対応する。
/// 上限を超える値をそのまま信じると、オフセット計算や確保サイズが破綻する。
fn validate_activity_entry(
    e: &FileActivityEntry,
    path: &Path,
    tolerance: Tolerance,
    diagnostics: &mut Vec<Diagnostic>,
    offset: usize,
) -> Result<()> {
    let mut reject = |detail: String, what: &str, value: u64, limit: u64| -> Result<()> {
        if tolerance == Tolerance::Strict {
            return Err(Error::LimitExceeded {
                path: path.to_path_buf(),
                what: format!("{} ({detail})", what),
                value,
                limit,
            });
        }
        diagnostics.push(Diagnostic {
            offset: Some(offset),
            message: format!("{}: {detail}", e.id),
        });
        Ok(())
    };

    // 1 item のサイズ: 0 は不可、上限は MAX_ITEM_STRUCT_SIZE
    if e.size == 0 || e.size > MAX_ITEM_STRUCT_SIZE {
        reject(
            format!("size = {}", e.size),
            "file_activity.size",
            e.size as u64,
            MAX_ITEM_STRUCT_SIZE as u64,
        )?;
    }

    // item 数: 0 と負値は不可
    if e.nr <= 0 {
        reject(
            format!("nr = {}", e.nr),
            "file_activity.nr",
            e.nr.max(0) as u64,
            NR_MAX as u64,
        )?;
    } else if e.nr as u32 > NR_MAX {
        reject(
            format!("nr = {}", e.nr),
            "file_activity.nr",
            e.nr as u64,
            NR_MAX as u64,
        )?;
    }

    // sub-item 数: 0 と負値は不可、上限は NR2_MAX
    if e.nr2 <= 0 {
        reject(
            format!("nr2 = {}", e.nr2),
            "file_activity.nr2",
            e.nr2.max(0) as u64,
            NR2_MAX as u64,
        )?;
    } else if e.nr2 as u32 > NR2_MAX {
        reject(
            format!("nr2 = {}", e.nr2),
            "file_activity.nr2",
            e.nr2 as u64,
            NR2_MAX as u64,
        )?;
    }

    // 数値フィールドが申告サイズに収まること (MAP_SIZE <= size)
    if let Some(t) = e.types_nr {
        let types = TypesNr(t);
        if !types.within(TYPES_NR_LIMIT) {
            reject(
                format!("types_nr = {t:?}"),
                "file_activity.types_nr",
                t.iter().copied().max().unwrap_or(0) as u64,
                TYPES_NR_LIMIT as u64,
            )?;
        } else {
            let map = types.map_size();
            if map > e.size as u64 {
                reject(
                    format!("MAP_SIZE({t:?}) = {map} > size = {}", e.size),
                    "file_activity の MAP_SIZE",
                    map,
                    e.size as u64,
                )?;
            }
        }
    }

    Ok(())
}

/// `extra_desc` チェーンを読み飛ばし、チェーン全体のバイト数を返す。
///
/// 1 段は `extra_desc` (24 バイト固定) + `extra_size × extra_nr` バイトの本体で構成され、
/// `extra_next` が 0 になるまで連鎖する。中身は未知拡張なので解釈せず、
/// 長さの検証だけを行って飛ばす。
fn skip_extra_chain(cur: &Cursor<'_>, start: usize, path: &Path) -> Result<usize> {
    let mut offset = start;
    loop {
        let oob = |e: OutOfBounds| Error::Truncated {
            path: path.to_path_buf(),
            context: "extra_desc".into(),
            need: e.need,
            have: e.len.saturating_sub(e.offset),
        };
        let extra_nr = cur.u32_at(offset).map_err(oob)?;
        let extra_size = cur.u32_at(offset + 4).map_err(oob)?;
        let extra_next = cur.u32_at(offset + 8).map_err(oob)?;

        if extra_nr > MAX_EXTRA_NR {
            return Err(Error::LimitExceeded {
                path: path.to_path_buf(),
                what: "extra_desc.extra_nr".into(),
                value: extra_nr as u64,
                limit: MAX_EXTRA_NR as u64,
            });
        }
        if extra_size > MAX_EXTRA_SIZE {
            return Err(Error::LimitExceeded {
                path: path.to_path_buf(),
                what: "extra_desc.extra_size".into(),
                value: extra_size as u64,
                limit: MAX_EXTRA_SIZE as u64,
            });
        }

        let body = (extra_nr as u64) * (extra_size as u64);
        offset += EXTRA_DESC_SIZE + body as usize;
        if offset > cur.len() {
            return Err(Error::Truncated {
                path: path.to_path_buf(),
                context: "extra_desc の本体".into(),
                need: body as usize,
                have: cur.len().saturating_sub(offset),
            });
        }
        if extra_next == 0 {
            break;
        }
    }
    Ok(offset - start)
}

fn read_all(file: &File, path: &Path) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut buf = Vec::new();
    let mut f = file;
    f.read_to_end(&mut buf).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(buf)
}

#[inline]
fn read_u8(cur: &Cursor<'_>, base: usize, f: Option<&super::wire::PlacedField>) -> u8 {
    f.and_then(|f| cur.read_unsigned(base, f).ok()).unwrap_or(0) as u8
}

fn decode_file_magic(
    cur: &Cursor<'_>,
    layout: &ResolvedLayout,
    _spec: &FormatSpec,
) -> std::result::Result<FileMagic, OutOfBounds> {
    let g = |name: &str| layout.field(name);
    let format_magic = match g("format_magic") {
        Some(f) => cur.read_unsigned(0, f)? as u16,
        None => 0,
    };
    let version = (
        g("sysstat_version")
            .map(|f| cur.read_unsigned(0, f))
            .transpose()?
            .unwrap_or(0) as u8,
        g("sysstat_patchlevel")
            .map(|f| cur.read_unsigned(0, f))
            .transpose()?
            .unwrap_or(0) as u8,
        g("sysstat_sublevel")
            .map(|f| cur.read_unsigned(0, f))
            .transpose()?
            .unwrap_or(0) as u8,
        g("sysstat_extraversion")
            .map(|f| cur.read_unsigned(0, f))
            .transpose()?
            .unwrap_or(0) as u8,
    );
    let header_size = g("header_size")
        .map(|f| cur.read_unsigned(0, f))
        .transpose()?
        .map(|v| v as u32);
    let upgraded = g("upgraded")
        .map(|f| cur.read_unsigned(0, f))
        .transpose()?
        .map(|v| v as u32);
    let hdr_types_nr = match (
        g("hdr_types_nr_0"),
        g("hdr_types_nr_1"),
        g("hdr_types_nr_2"),
    ) {
        (Some(a), Some(b), Some(c)) => Some([
            cur.read_unsigned(0, a)? as u32,
            cur.read_unsigned(0, b)? as u32,
            cur.read_unsigned(0, c)? as u32,
        ]),
        _ => None,
    };

    Ok(FileMagic {
        format_magic,
        version,
        header_size,
        upgraded,
        hdr_types_nr,
    })
}

fn resolve_file_header(
    spec: &FormatSpec,
    magic: &FileMagic,
    enc: &SourceEncoding,
) -> std::result::Result<ResolvedLayout, crate::error::LayoutError> {
    match spec.structs {
        StructSource::Fixed { file_header, .. } => file_header.resolve(enc),
        StructSource::SelfDescribing => {
            let types = TypesNr(magic.hdr_types_nr.unwrap_or([1, 1, 12]));
            let declared = magic.header_size.unwrap_or(0) as usize;
            selfdesc::resolve_file_header(types, declared, enc)
        }
    }
}

fn resolve_file_activity(
    spec: &FormatSpec,
    header: &FileHeader,
    enc: &SourceEncoding,
) -> std::result::Result<ResolvedLayout, crate::error::LayoutError> {
    match spec.structs {
        StructSource::Fixed { file_activity, .. } => file_activity.resolve(enc),
        StructSource::SelfDescribing => {
            let types = TypesNr(header.act_types_nr.unwrap_or([0, 0, 9]));
            selfdesc::resolve_file_activity(types, enc)
        }
    }
}

fn resolve_record_header(
    spec: &FormatSpec,
    header: &FileHeader,
    enc: &SourceEncoding,
) -> std::result::Result<ResolvedLayout, crate::error::LayoutError> {
    match spec.structs {
        StructSource::Fixed { record_header, .. } => record_header.resolve(enc),
        StructSource::SelfDescribing => {
            let types = TypesNr(header.rec_types_nr.unwrap_or([2, 0, 1]));
            selfdesc::resolve_record_header(types, enc)
        }
    }
}

fn decode_file_header(
    cur: &Cursor<'_>,
    layout: &ResolvedLayout,
    base: usize,
    _spec: &FormatSpec,
    _magic: &FileMagic,
) -> std::result::Result<FileHeader, OutOfBounds> {
    let g = |name: &str| layout.field(name);
    let num = |name: &str| -> std::result::Result<Option<u64>, OutOfBounds> {
        match g(name) {
            Some(f) => Ok(Some(cur.read_unsigned(base, f)?)),
            None => Ok(None),
        }
    };
    let text = |name: &str| -> std::result::Result<Option<String>, OutOfBounds> {
        match g(name) {
            Some(f) => Ok(Some(cur.read_str(base, f)?.to_string())),
            None => Ok(None),
        }
    };
    let triple =
        |a: &str, b: &str, c: &str| -> std::result::Result<Option<[u32; 3]>, OutOfBounds> {
            match (g(a), g(b), g(c)) {
                (Some(x), Some(y), Some(z)) => Ok(Some([
                    cur.read_unsigned(base, x)? as u32,
                    cur.read_unsigned(base, y)? as u32,
                    cur.read_unsigned(base, z)? as u32,
                ])),
                _ => Ok(None),
            }
        };

    let ust_time = num("sa_ust_time")?.unwrap_or(0);
    // act 数のフィールド名は世代で異なる
    let act_nr = num("sa_act_nr")?.or(num("sa_nr_act")?).unwrap_or(0) as u32;
    let cpu_nr = num("sa_cpu_nr")?
        .or(num("sa_last_cpu_nr")?)
        .map(|v| v as u32);

    // 月は 0 起点、年は 1900 起点で記録される世代がある
    let raw_month = num("sa_month")?.unwrap_or(0) as u8;
    let raw_year = num("sa_year")?.unwrap_or(0);
    let year = if g("sa_year").map(|f| f.value_width) == Some(1) {
        // 1 バイトの sa_year は 1900 起点
        1900 + raw_year as i32
    } else if raw_year < 1000 {
        // int でも 1900 起点で書かれている版がある
        1900 + raw_year as i32
    } else {
        raw_year as i32
    };

    Ok(FileHeader {
        ust_time,
        hz: num("sa_hz")?,
        cpu_nr,
        act_nr,
        vol_act_nr: num("sa_vol_act_nr")?.map(|v| v as u32),
        year,
        // 0 起点 → 1-12
        month: raw_month.saturating_add(1),
        day: num("sa_day")?.unwrap_or(0) as u8,
        sizeof_long: num("sa_sizeof_long")?.unwrap_or(8) as u8,
        sysname: text("sa_sysname")?.unwrap_or_default(),
        nodename: text("sa_nodename")?.unwrap_or_default(),
        release: text("sa_release")?.unwrap_or_default(),
        machine: text("sa_machine")?.unwrap_or_default(),
        tzname: text("sa_tzname")?,
        act_types_nr: triple("act_types_nr_0", "act_types_nr_1", "act_types_nr_2")?,
        rec_types_nr: triple("rec_types_nr_0", "rec_types_nr_1", "rec_types_nr_2")?,
        act_size: num("act_size")?.map(|v| v as u32),
        rec_size: num("rec_size")?.map(|v| v as u32),
        extra_next: num("extra_next")?.map(|v| v as u32),
    })
}

fn decode_file_activity(
    cur: &Cursor<'_>,
    layout: &ResolvedLayout,
    base: usize,
) -> std::result::Result<FileActivityEntry, OutOfBounds> {
    let g = |name: &str| layout.field(name);
    let num = |name: &str| -> std::result::Result<Option<u64>, OutOfBounds> {
        match g(name) {
            Some(f) => Ok(Some(cur.read_unsigned(base, f)?)),
            None => Ok(None),
        }
    };
    let snum = |name: &str| -> std::result::Result<Option<i64>, OutOfBounds> {
        match g(name) {
            Some(f) => Ok(Some(cur.read_signed(base, f)?)),
            None => Ok(None),
        }
    };

    let types_nr = match (g("types_nr_0"), g("types_nr_1"), g("types_nr_2")) {
        (Some(a), Some(b), Some(c)) => Some([
            cur.read_unsigned(base, a)? as u32,
            cur.read_unsigned(base, b)? as u32,
            cur.read_unsigned(base, c)? as u32,
        ]),
        _ => None,
    };

    Ok(FileActivityEntry {
        id: ActivityId(num("id")?.unwrap_or(0) as u32),
        magic: num("magic")?.unwrap_or(0) as u32,
        nr: snum("nr")?.unwrap_or(0) as i32,
        // nr2 を持たない世代 (0x2170) は 1 として扱う
        nr2: snum("nr2")?.unwrap_or(1) as i32,
        size: snum("size")?.unwrap_or(0).max(0) as u32,
        has_nr: snum("has_nr")?.unwrap_or(0) != 0,
        types_nr,
    })
}

#[allow(unused_imports)]
use layouts as _layouts_used;

#[cfg(test)]
mod tests {
    use super::*;

    /// マジックナンバーが違うファイルは sysstat のものではないと判定する。
    #[test]
    fn rejects_non_sysstat_file() {
        let bytes = vec![0u8; 128];
        let err = SaFile::from_bytes("x", bytes).unwrap_err();
        assert!(matches!(err, Error::NotSysstatFile { .. }), "{err}");
    }

    /// 短すぎるファイルは切り詰めと報告する。
    #[test]
    fn rejects_tiny_file() {
        let err = SaFile::from_bytes("x", vec![0x96, 0xd5]).unwrap_err();
        assert!(matches!(err, Error::Truncated { .. }), "{err}");
    }

    /// 未知の format_magic は未対応フォーマットとして報告する (0x2172 は欠番)。
    #[test]
    fn rejects_unknown_format_magic() {
        let mut bytes = vec![0u8; 128];
        bytes[0..2].copy_from_slice(&SYSSTAT_MAGIC.to_le_bytes());
        bytes[2..4].copy_from_slice(&0x2172u16.to_le_bytes());
        let err = SaFile::from_bytes("x", bytes).unwrap_err();
        assert!(matches!(err, Error::UnsupportedFormat { .. }), "{err}");
    }
}

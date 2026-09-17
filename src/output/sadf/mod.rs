//! `sadf` 互換の出力形式。
//!
//! | 形式 | モジュール | 表示ループ |
//! |---|---|---|
//! | `-d` (DB/CSV) / `-p` (ppc) | [`dbppc`] | activity 順 (`logic2`) |
//! | `-j` (JSON) | [`json`] | 時刻順 (`logic1`) |
//! | `-x` (XML) | [`xml`] | 時刻順 (`logic1`) |
//! | `-r` (raw) | [`raw`] | activity 順 (`logic2`) |
//! | `-H` (ヘッダのみ) | [`header`] | なし |
//!
//! 仕様の出典は `docs/format/03-output-format.md` 第 V 部。
//! フィールド名・キー名・書式の表は [`spec`] に集約してある。
//!
//! # 値の扱い
//!
//! 表示値は `series` 層 ([`crate::series::compute`]) の結果だけを使う。
//! 出力層では再計算しない。単位が違う列 (`rxkB` / `MBfsfree` / `rd_sec` …) も
//! 出力層で掛け算をせず、[`crate::series::compute::SadfUnitColumn`] に
//! 換算させる。形式ごとに計算がずれる事故を層の分離で防ぐのが目的。
//!
//! 値が出ないときの表記は欠落の種類で 2 通りに分かれる
//! ([`crate::series::compute::missing_kind`])。
//!
//! - **その世代のファイルにフィールドが無いだけ** の列は `0` を書く。
//!   本家が 0 埋めした構造体で計算を完了するため (03 §1.9-1)、
//!   互換出力としては `0.00` が正解である (旧 `A_IO` の `dtps` / `bdscd`)。
//! - **レコードで欠測 / 不連続 / 計算未実装** は `0` にせず「値なし」として出す
//!   ([`Unavailable`])。0 を代入すると「正常に 0」と区別できなくなり、
//!   集計が静かに誤る。
//!
//! この分岐は互換出力だけのもの。独自出力 (`table` / `json` / `csv` /
//! `ndjson`) は欠落を欠落のまま残す
//! ([`crate::series::compute::column_value_strict`])。

pub mod access;
pub mod dbppc;
pub mod header;
pub mod json;
pub mod raw;
pub mod render;
pub mod spec;
pub mod xml;

use std::fmt::Write as _;

use chrono::{DateTime, Datelike, TimeZone, Timelike, Utc};

use crate::format::file::SaFile;
use crate::model::ActivityId;
use crate::output::time_filter::TimeFilter;
use crate::series::compute::{ComputeIssue, Computed, MissingKind, missing_kind};

pub use spec::{ActivitySpec, Fmt, Group, ItemKind, SectionConfig, Shape};

// ===========================================================================
// 設定
// ===========================================================================

/// タイムスタンプの基準系。`-T` / `-t` / `-U` は相互排他 (§1.1)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TimeBase {
    /// 既定。UTC で表示し、TZ 欄はリテラル `UTC`。
    #[default]
    Utc,
    /// `-T` — sadf を実行している環境のローカル時刻。
    LocalTime,
    /// `-t` — データ収集時のローカル時刻。TZ 欄はファイルの `sa_tzname`。
    TrueTime,
    /// `-U` — epoch 秒。日付も TZ も出ない。
    SecEpoch,
}

/// `sadf` 出力の設定。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SadfConfig {
    pub time_base: TimeBase,
    /// `-C` — COMMENT レコードも出す。
    pub comments: bool,
    /// `-O debug` — raw 出力に追加情報を入れる。
    pub debug: bool,
    /// `-h` — `-d` で全 activity を 1 行に並べる。
    pub horizontally: bool,
    /// セクション選択 (`-u ALL` / `-r ALL` / `-F MOUNT` など)。
    pub section: SectionConfig,
    /// 出力対象 activity (`-- <sar オプション>` による選択)。
    ///
    /// `None` = ファイルに含まれる既知 activity すべて (`-- -A` 相当)。
    /// 既定は `None` なので、この項目を渡さない呼び出し側の出力は変わらない。
    pub activities: Option<Vec<ActivityId>>,
    /// `-s` / `-e` の時刻フィルタ。既定は無効 (全レコードを出す)。
    pub time_filter: TimeFilter,
    pub cpus: crate::output::sar_text::CpuSelection,
    pub item_names: std::collections::BTreeMap<ActivityId, Vec<String>>,
}

impl SadfConfig {
    pub fn name_selected(&self, id: ActivityId, name: &str) -> bool {
        self.item_names
            .get(&id)
            .is_none_or(|names| names.is_empty() || names.iter().any(|n| n == name))
    }
}

// ===========================================================================
// 書き込みエラー
// ===========================================================================

/// 出力先への書き込みエラーを解析エラーへ包む。
///
/// 低レベルの書き出しは `std::io::Result` を返し、`series` 層の走査
/// ([`crate::series::walk`]) と組み合わせる境界でだけこの変換を通す。
pub fn wrap_io(e: std::io::Error) -> crate::error::Error {
    crate::error::Error::Io {
        path: std::path::PathBuf::from("-"),
        source: e,
    }
}

// ===========================================================================
// 値なしの表現
// ===========================================================================

/// 値を出せない理由と、その形式での表記。
///
/// 「未提供」「当該レコードで欠落」「不連続」「計算未実装」を区別したまま
/// 運ぶ。`sadf` 互換形式はテキストで区別を表現できないので表記は 1 つに潰れる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unavailable(pub ComputeIssue);

impl Unavailable {
    /// 診断ログ用のラベル。
    pub fn label(self) -> &'static str {
        match self.0 {
            ComputeIssue::UnsupportedBySource => "unsupported_by_source",
            ComputeIssue::MissingInSample => "missing_in_sample",
            ComputeIssue::Discontinuous(d) => d.as_str(),
            ComputeIssue::NotNumeric => "not_numeric",
            ComputeIssue::NeedsItemGroup => "needs_item_group",
            ComputeIssue::NotImplemented => "not_implemented",
        }
    }
}

/// `-d` / `-p` / `-r` で値が「観測できなかった」ときの表記。
///
/// 空フィールドにする。`0` を書くと正常値と区別できない。
/// **その世代にフィールドが無いだけの列はここに落とさない**
/// ([`write_value`] のゼロ補完を参照)。
pub const ABSENT_TEXT: &str = "";
/// `-j` で値が観測できなかったときの表記。
pub const ABSENT_JSON: &str = "null";
/// `-x` で値が観測できなかったときの属性値。
pub const ABSENT_XML: &str = "";

// ===========================================================================
// 数値の書式
// ===========================================================================

/// 表示値を書式化して書き出す (**互換出力**)。
///
/// `absent` は値が観測できなかったときに書く文字列 (形式ごとに違う)。
///
/// # 欠落の 2 分類
///
/// 本家は「期待する型別本数よりファイル側が少なければ足りない分を 0 埋め」した
/// 構造体で計算を完了するので、**その世代にフィールドが無い列は `0.00` として
/// 表示される** (03 §1.9-1)。discard 統計を持たない旧 `A_IO` の
/// `dtps` / `bdscd` がこれに当たる。この分類は計算層の
/// [`missing_kind`] が持っており、出力層は表記を選ぶだけにする。
///
/// | 分類 | 例 | 表記 |
/// |---|---|---|
/// | [`MissingKind::ZeroFilled`] | その世代にフィールドが無い | `0.00` (本家と同じ) |
/// | [`MissingKind::Absent`] | レコードで欠測 / 不連続 | `absent` |
/// | (分類なし) | 識別子列 / item 群が必要 / 未実装 | `absent` |
///
/// `0` を書くのは 1 行目だけで、`MissingInSample` や不連続を `0` にしてはいけない
/// (「正常に 0」と区別できなくなる)。独自出力は
/// [`crate::series::compute::column_value_strict`] 系を使うのでこの補完を通らない。
pub fn write_value(out: &mut String, v: Computed, fmt: Fmt, absent: &str) {
    match v {
        Ok(x) => write_f64(out, x, fmt),
        // 本家が 0 埋めして表示する欠落だけ 0 にする (03 §1.9-1)
        Err(e) if missing_kind(e) == Some(MissingKind::ZeroFilled) => write_f64(out, 0.0, fmt),
        Err(_) => out.push_str(absent),
    }
}

/// `f64` を指定書式で書き出す。
pub fn write_f64(out: &mut String, x: f64, fmt: Fmt) {
    match fmt {
        // PT_NOFLAG / json_stats.c の %.2f
        Fmt::R2 => {
            let _ = write!(out, "{x:.2}");
        }
        // PT_USERND / %.0f (四捨五入して小数点なし)
        Fmt::R0 => {
            let _ = write!(out, "{x:.0}");
        }
        // PT_USEINT / %llu (C の (unsigned long long) キャスト = 切り捨て)
        Fmt::Int => {
            let _ = write!(out, "{}", truncate_u64(x));
        }
        Fmt::Hex => {
            let _ = write!(out, "{:x}", truncate_u64(x));
        }
        // 文字列系はここでは扱わない
        Fmt::Str | Fmt::ItemKeyStr | Fmt::ItemKeyNum | Fmt::Skip => {}
    }
}

/// C の `(unsigned long long)` キャスト相当 (0 方向への切り捨て、負値は 0)。
#[inline]
pub fn truncate_u64(x: f64) -> u64 {
    if !x.is_finite() || x <= 0.0 {
        0
    } else {
        x as u64
    }
}

/// センサ値の `%f` (小数 6 桁固定)。raw 出力専用 (§4.3)。
pub fn write_sensor(out: &mut String, x: f64) {
    let _ = write!(out, "{x:.6}");
}

/// ファイル上で IEEE-754 の `double` として書かれている値を解釈する。
///
/// `series` 層は整数フィールドとして読むため、ビット列を `f64` に戻すのは
/// 出力側の責務になる。**計算ではなくデコード**である。
#[inline]
pub fn double_from_bits(bits: u64) -> f64 {
    f64::from_bits(bits)
}

// ===========================================================================
// ファイルヘッダの要約
// ===========================================================================

/// 全形式が使うファイル情報。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileInfo {
    pub nodename: String,
    pub sysname: String,
    pub release: String,
    pub machine: String,
    /// 表示用 CPU 数 (`sa_cpu_nr > 1 ? sa_cpu_nr - 1 : 1`)。
    pub cpu_count: u32,
    /// `YYYY-MM-DD`。`sa_ust_time` を暦に開いた日付 (`-t` ではヘッダの年月日)。
    pub file_date: String,
    /// `HH:MM:SS` (UTC)。
    pub file_utc_time: String,
    /// ファイル作成時刻 (epoch 秒)。
    pub ust_time: u64,
    /// 収集時のタイムゾーン名。古いファイルは空。
    pub tzname: String,
}

impl FileInfo {
    /// 既定の時刻基準でファイル情報を作る。
    pub fn from_file(file: &SaFile) -> Self {
        Self::from_file_with(file, TimeBase::default())
    }

    /// 時刻基準を指定してファイル情報を作る。
    ///
    /// `file_date` の求め方は `sa_common.c: get_file_timestamp_struct()`。
    /// **既定は `sa_ust_time` 由来**で、ヘッダの `sa_day` / `sa_month` /
    /// `sa_year` を使うのは `-t` ([`TimeBase::TrueTime`]) のときだけ (§1.5)。
    /// 両者は食い違い得る (本家のテストデータ `data-ukwn` は `sa_ust_time` が
    /// 09-15、ヘッダ日付が 10-15)。
    ///
    /// 既定側は次の `file_utc_time` と同じ UTC で開く。本家は `localtime_r()`
    /// を使うが、本家のテストは `TZ=GMT` 固定であり、reSARch は環境変数に
    /// 依存せず基準系で表す (§1.6 の対応表)。
    pub fn from_file_with(file: &SaFile, base: TimeBase) -> Self {
        let h = file.header();
        let cpu_count = match h.cpu_nr {
            Some(n) if n > 1 => n - 1,
            _ => 1,
        };
        let utc = utc_of(h.ust_time);
        let file_date = match base {
            TimeBase::TrueTime => format_date(h.year, u32::from(h.month), u32::from(h.day)),
            // `-T` は読み手のローカル時刻。レコードの [`Stamp`] と同じ基準に揃える
            TimeBase::LocalTime => {
                let l = utc.with_timezone(&chrono::Local);
                format_date(l.year(), l.month(), l.day())
            }
            _ => format_date(utc.year(), utc.month(), utc.day()),
        };
        Self {
            nodename: h.nodename.clone(),
            sysname: h.sysname.clone(),
            release: h.release.clone(),
            machine: h.machine.clone(),
            cpu_count,
            file_date,
            file_utc_time: format!("{:02}:{:02}:{:02}", utc.hour(), utc.minute(), utc.second()),
            ust_time: h.ust_time,
            tzname: h.tzname.clone().unwrap_or_default(),
        }
    }
}

fn utc_of(secs: u64) -> DateTime<Utc> {
    Utc.timestamp_opt(secs as i64, 0)
        .single()
        .unwrap_or_else(|| Utc.timestamp_opt(0, 0).unwrap())
}

// ===========================================================================
// タイムスタンプ
// ===========================================================================

/// 1 レコード分のタイムスタンプ表記 (§1.1)。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Stamp {
    /// `YYYY-MM-DD`。`-U` では空文字。
    pub date: String,
    /// `HH:MM:SS`、`-U` では epoch 秒の十進表記。
    pub time: String,
    /// TZ 名。`-U` では空文字。
    pub tz: String,
}

impl Stamp {
    /// レコードの時刻から表記を作る。
    ///
    /// `rec_hms` はレコードヘッダが持つ「収集時ローカル時刻」。
    /// `-t` ではこれと UTC の差からオフセットを復元して日付を決める
    /// (日付跨ぎで 1 日ずれないようにするため)。
    pub fn new(base: TimeBase, ust_time: u64, rec_hms: (u8, u8, u8), info: &FileInfo) -> Self {
        match base {
            TimeBase::SecEpoch => Stamp {
                date: String::new(),
                time: ust_time.to_string(),
                tz: String::new(),
            },
            TimeBase::Utc => {
                let t = utc_of(ust_time);
                Stamp {
                    date: format_date(t.year(), t.month(), t.day()),
                    time: format_time(t.hour(), t.minute(), t.second()),
                    tz: "UTC".to_string(),
                }
            }
            TimeBase::TrueTime => {
                let shifted = utc_of(shift_to_recorded(ust_time, rec_hms));
                Stamp {
                    date: format_date(shifted.year(), shifted.month(), shifted.day()),
                    time: format_time(shifted.hour(), shifted.minute(), shifted.second()),
                    tz: info.tzname.clone(),
                }
            }
            TimeBase::LocalTime => {
                let local = utc_of(ust_time).with_timezone(&chrono::Local);
                Stamp {
                    date: format_date(local.year(), local.month(), local.day()),
                    time: format_time(local.hour(), local.minute(), local.second()),
                    tz: local_tz_name(),
                }
            }
        }
    }

    /// `<date> <time> <tz>` の 1 フィールド表記 (`-d` / `-p`)。
    ///
    /// `-U` のときは epoch 秒だけ。`-t` で `sa_tzname` が空でも
    /// `print_dbppc_timestamp` と同じく TZ 前の区切り空白を残す。
    pub fn dbppc(&self) -> String {
        if self.date.is_empty() {
            return self.time.clone();
        }
        format!("{} {} {}", self.date, self.time, self.tz)
    }

    /// `<time> <tz>` 表記 (`-r`)。TZ が空なら時刻だけ。
    pub fn raw(&self) -> String {
        if self.tz.is_empty() {
            self.time.clone()
        } else {
            format!("{} {}", self.time, self.tz)
        }
    }
}

fn format_date(y: i32, m: u32, d: u32) -> String {
    format!("{y:04}-{m:02}-{d:02}")
}

fn format_time(h: u32, m: u32, s: u32) -> String {
    format!("{h:02}:{m:02}:{s:02}")
}

/// 収集時ローカル時刻の epoch 秒を復元する。
///
/// レコードは「UTC の epoch 秒」と「収集時ローカルの時分秒」を両方持つので、
/// 両者の時刻差から UTC オフセットが分かる。±12 時間へ正規化して足す。
pub(crate) fn shift_to_recorded(ust_time: u64, (h, m, s): (u8, u8, u8)) -> u64 {
    const DAY: i64 = 86_400;
    let utc = utc_of(ust_time);
    let utc_sod = (utc.hour() * 3600 + utc.minute() * 60 + utc.second()) as i64;
    let rec_sod = (h as i64) * 3600 + (m as i64) * 60 + s as i64;
    let mut diff = (rec_sod - utc_sod).rem_euclid(DAY);
    if diff > DAY / 2 {
        diff -= DAY;
    }
    (ust_time as i64 + diff).max(0) as u64
}

/// `-T` で使う実行環境の TZ 略称。
///
/// `TZ` が IANA 名なら `chrono-tz` から略称を取る。取れない場合は
/// `+09:00` のような数値オフセット表記へ落とす (空文字にはしない)。
fn local_tz_name() -> String {
    use chrono::Offset;
    // 略称は chrono-tz 側のトレイト (`OffsetName`) にある
    use chrono_tz::OffsetName;
    if let Ok(tz) = std::env::var("TZ")
        && let Ok(tz) = tz.parse::<chrono_tz::Tz>()
    {
        let now = Utc::now().naive_utc();
        if let Some(abbr) = tz.offset_from_utc_datetime(&now).abbreviation() {
            return abbr.to_string();
        }
    }
    chrono::Local::now().offset().fix().to_string()
}

/// `interval` 欄の秒数 (§1.2)。
///
/// `itv`(1/100 秒) を 100 で割り、余りが 50 以上なら切り上げる。
pub fn interval_secs(itv_cs: u64) -> u64 {
    let mut dt = itv_cs / 100;
    if itv_cs % 100 >= 50 {
        dt += 1;
    }
    dt
}

/// RESTART / COMMENT 行の `interval` 欄。常に `-1`。
pub const EVENT_INTERVAL: &str = "-1";

// ===========================================================================
// アイテム識別子
// ===========================================================================

/// アイテム識別子の 3 通りの表記。
///
/// 同じアイテムが形式ごとに違う文字列になる (§14.4-4)。
/// 例: CPU 集約行は `-p` が `all`、`-d` が `-1`、`-j`/`-x` が `"all"`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemLabel {
    /// `-p` の第 4 フィールド。
    pub ppc: String,
    /// `-d` のキー列 / `-r` のアイテム識別子。
    pub db: String,
    /// `-j` / `-x` の値。
    pub jx: String,
}

impl ItemLabel {
    /// アイテムを持たない activity。`-p` はリテラル `-`。
    pub fn none() -> Self {
        Self {
            ppc: "-".to_string(),
            db: String::new(),
            jx: String::new(),
        }
    }

    pub fn cpu(index: usize) -> Self {
        if index == 0 {
            Self {
                ppc: "all".to_string(),
                db: "-1".to_string(),
                jx: "all".to_string(),
            }
        } else {
            let n = index - 1;
            Self {
                ppc: format!("cpu{n}"),
                db: n.to_string(),
                jx: n.to_string(),
            }
        }
    }

    pub fn named(name: &str) -> Self {
        Self {
            ppc: name.to_string(),
            db: name.to_string(),
            jx: name.to_string(),
        }
    }

    pub fn numbered(prefix: &str, n: u64) -> Self {
        Self {
            ppc: format!("{prefix}{n}"),
            db: n.to_string(),
            jx: n.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interval_rounds_at_half_a_second() {
        assert_eq!(interval_secs(3117), 31);
        assert_eq!(interval_secs(3150), 32);
        assert_eq!(interval_secs(3149), 31);
        assert_eq!(interval_secs(0), 0);
    }

    /// `-U` では日付も TZ も出ない (§14.4-7)。
    #[test]
    fn epoch_stamp_has_no_date_or_tz() {
        let info = dummy_info();
        let s = Stamp::new(TimeBase::SecEpoch, 1_555_593_639, (13, 20, 39), &info);
        assert_eq!(s.dbppc(), "1555593639");
        assert!(s.date.is_empty());
        assert!(s.tz.is_empty());
    }

    #[test]
    fn utc_stamp_matches_documented_example() {
        let info = dummy_info();
        // 2019-04-18 13:20:19 UTC
        let s = Stamp::new(TimeBase::Utc, 1_555_593_619, (15, 20, 19), &info);
        assert_eq!(s.dbppc(), "2019-04-18 13:20:19 UTC");
        assert_eq!(s.raw(), "13:20:19 UTC");
    }

    /// `-t` は収集時ローカル時刻を復元する (§1.2 の検証例: Europe/Paris = +2h)。
    #[test]
    fn true_time_uses_recorded_local_clock() {
        let mut info = dummy_info();
        info.tzname = "CET".to_string();
        let s = Stamp::new(TimeBase::TrueTime, 1_555_593_619, (15, 20, 19), &info);
        assert_eq!(s.dbppc(), "2019-04-18 15:20:19 CET");
    }

    /// `-t` かつ `sa_tzname` が空でも TZ の区切り空白は残す。
    #[test]
    fn true_time_without_tzname_keeps_separator() {
        let info = dummy_info();
        let s = Stamp::new(TimeBase::TrueTime, 1_555_593_619, (13, 20, 19), &info);
        assert_eq!(s.dbppc(), "2019-04-18 13:20:19 ");
        assert_eq!(s.raw(), "13:20:19");
    }

    /// 日付跨ぎでオフセットを足しても日付がずれないこと。
    #[test]
    fn true_time_handles_day_rollover() {
        let mut info = dummy_info();
        info.tzname = "JST".to_string();
        // UTC 2019-04-18 23:30:00 / 収集時ローカル 08:30:00 (+9h) → 翌日
        let s = Stamp::new(TimeBase::TrueTime, 1_555_630_200, (8, 30, 0), &info);
        assert_eq!(s.dbppc(), "2019-04-19 08:30:00 JST");
    }

    #[test]
    fn item_labels_differ_per_format() {
        let all = ItemLabel::cpu(0);
        assert_eq!(
            (all.ppc.as_str(), all.db.as_str(), all.jx.as_str()),
            ("all", "-1", "all")
        );
        let c3 = ItemLabel::cpu(4);
        assert_eq!(
            (c3.ppc.as_str(), c3.db.as_str(), c3.jx.as_str()),
            ("cpu3", "3", "3")
        );
        assert_eq!(ItemLabel::none().ppc, "-");
    }

    #[test]
    fn integer_format_truncates_like_c_cast() {
        let mut s = String::new();
        write_f64(&mut s, 1283.99, Fmt::Int);
        assert_eq!(s, "1283");
        s.clear();
        write_f64(&mut s, -5.0, Fmt::Int);
        assert_eq!(
            s, "0",
            "負値は 0 (C のキャストは未定義動作なので安全側に倒す)"
        );
    }

    #[test]
    fn rounded_format_has_no_decimal_point() {
        let mut s = String::new();
        write_f64(&mut s, 272.6, Fmt::R0);
        assert_eq!(s, "273");
    }

    /// 値が無い列は 0 ではなく空 / null になる。
    #[test]
    fn absent_values_are_not_zero() {
        let mut s = String::new();
        write_value(
            &mut s,
            Err(ComputeIssue::NotImplemented),
            Fmt::R2,
            ABSENT_TEXT,
        );
        assert_eq!(s, "");
        s.clear();
        write_value(
            &mut s,
            Err(ComputeIssue::NotImplemented),
            Fmt::R2,
            ABSENT_JSON,
        );
        assert_eq!(s, "null");

        // レコードでの欠測・不連続も 0 にしない
        for issue in [
            ComputeIssue::MissingInSample,
            ComputeIssue::Discontinuous(crate::series::delta::Discontinuity::Restart),
            ComputeIssue::NeedsItemGroup,
        ] {
            s.clear();
            write_value(&mut s, Err(issue), Fmt::R2, ABSENT_JSON);
            assert_eq!(s, "null", "{issue:?} を 0 にしてはいけない");
        }
    }

    /// **回帰テスト (指摘 6)**: その世代に無いフィールドは本家と同じ `0`。
    ///
    /// 本家は足りない型別本数を 0 埋めした構造体で計算を完了するので
    /// (03 §1.9-1)、discard 統計を持たない旧 `A_IO` の `dtps` / `bdscd` は
    /// `0.00` と表示される。全エラーを空欄にすると `sar` 互換出力と
    /// `sadf` 互換出力で不統一になる。
    #[test]
    fn unsupported_by_source_is_zero_filled_like_upstream() {
        let cases = [
            (Fmt::R2, "0.00"),
            (Fmt::R0, "0"),
            (Fmt::Int, "0"),
            (Fmt::Hex, "0"),
        ];
        for (fmt, want) in cases {
            let mut s = String::new();
            write_value(
                &mut s,
                Err(ComputeIssue::UnsupportedBySource),
                fmt,
                ABSENT_TEXT,
            );
            assert_eq!(s, want, "{fmt:?}");

            // JSON でも `null` ではなく 0
            let mut s = String::new();
            write_value(
                &mut s,
                Err(ComputeIssue::UnsupportedBySource),
                fmt,
                ABSENT_JSON,
            );
            assert_eq!(s, want, "{fmt:?} (JSON)");
        }
    }

    fn dummy_info() -> FileInfo {
        FileInfo {
            nodename: "testhost".to_string(),
            sysname: "Linux".to_string(),
            release: "5.0.0".to_string(),
            machine: "x86_64".to_string(),
            cpu_count: 8,
            file_date: "2019-04-18".to_string(),
            file_utc_time: "13:20:09".to_string(),
            ust_time: 1_555_593_609,
            tzname: String::new(),
        }
    }
}

// ===========================================================================
// 実データでの通し確認
// ===========================================================================

/// 本家のテストデータを使った通し確認。
///
/// データは GPL-2.0-or-later なので同梱できない。
/// `cargo run --bin xtask -- fetch-fixtures` で取得済みの場合だけ実行し、
/// **無ければ何も失敗させずスキップ**する (ネットワークが無い環境でも通るように)。
#[cfg(test)]
mod smoke {
    use super::*;
    use crate::format::file::SaFile;
    use crate::output::json::{CustomConfig, ValueScope};
    use std::path::PathBuf;

    /// `xtask fetch-fixtures` の保存先。
    fn fixture(name: &str) -> Option<PathBuf> {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/fixtures/upstream")
            .join(name);
        p.exists().then_some(p)
    }

    fn open(name: &str) -> Option<SaFile> {
        let path = fixture(name)?;
        // 壊れたファイルの検証は別テストの担当。ここは読めるものだけ扱う。
        SaFile::open(&path).ok()
    }

    /// 6 種の `sadf` 形式がすべて出力でき、形式ごとの目印が入っていること。
    #[test]
    fn all_sadf_formats_produce_output() {
        let Some(file) = open("data-12.0.0") else {
            eprintln!("fixture 未取得: スキップ");
            return;
        };
        let cfg = SadfConfig::default();

        let mut buf = Vec::new();
        header::write_header(&mut buf, &file).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.starts_with("System activity data file: "), "{s}");
        assert!(s.contains("\nHost: "), "{s}");
        assert!(s.contains("\nList of activities:\n"), "{s}");

        let mut buf = Vec::new();
        dbppc::write_db(&mut buf, &file, &cfg).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(
            s.contains("# hostname;interval;timestamp;"),
            "-d はフィールド名一覧行を出す"
        );

        let mut buf = Vec::new();
        dbppc::write_ppc(&mut buf, &file, &cfg).unwrap();
        let ppc = String::from_utf8(buf).unwrap();
        assert!(
            !ppc.contains("# hostname"),
            "-p にフィールド名一覧行は無い (§2.2)"
        );
        // RESTART / COMMENT 行は 5 フィールド。統計行は常に 6 フィールドで、
        // アイテムを持たない activity でもリテラル `-` が入る (§2.1)。
        let data_line = ppc
            .lines()
            .find(|l| !l.contains("LINUX-RESTART") && !l.contains("\tCOM "));
        if let Some(line) = data_line {
            assert_eq!(
                line.split('\t').count(),
                6,
                "-p は常に 6 フィールド: {line}"
            );
        }

        let mut buf = Vec::new();
        raw::write_raw(&mut buf, &file, &cfg).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("; "), "raw の区切りは `; `");

        let mut buf = Vec::new();
        json::write_json(&mut buf, &file, &cfg).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(
            s.starts_with("{\"sysstat\": {\n\t\"hosts\": [\n"),
            "{}",
            &s[..60.min(s.len())]
        );
        assert!(s.trim_end().ends_with("}}"));
        // 妥当な JSON であること
        serde_json::from_str::<serde_json::Value>(&s).expect("-j は妥当な JSON");

        let mut buf = Vec::new();
        xml::write_xml(&mut buf, &file, &cfg).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<sysstat\n"));
        assert!(s.contains("<sysdata-version>3.18</sysdata-version>"));
        assert!(s.trim_end().ends_with("</sysstat>"));
        // 開いたタグはすべて閉じていること
        for tag in ["sysstat", "host", "statistics", "restarts"] {
            assert_eq!(
                s.matches(&format!("<{tag}")).count(),
                s.matches(&format!("</{tag}>")).count(),
                "<{tag}> の開閉数が合わない"
            );
        }
    }

    /// `-U` (epoch 秒) では日付も TZ も出ない。
    #[test]
    fn epoch_mode_drops_date_and_tz() {
        let Some(file) = open("data-12.0.0") else {
            return;
        };
        let cfg = SadfConfig {
            time_base: TimeBase::SecEpoch,
            ..Default::default()
        };
        let mut buf = Vec::new();
        dbppc::write_db(&mut buf, &file, &cfg).unwrap();
        let s = String::from_utf8(buf).unwrap();
        for line in s.lines().filter(|l| !l.starts_with('#')) {
            assert!(!line.contains(" UTC"), "TZ が出ている: {line}");
            let ts = line.split(';').nth(2).unwrap_or("");
            assert!(
                ts.chars().all(|c| c.is_ascii_digit()),
                "timestamp が epoch 秒 1 個ではない: {line}"
            );
        }
    }

    /// 公開スキーマが文字列フィールドを `text` として運ぶこと。
    ///
    /// `A_FS` は `filesystem` と `mountpoint` の両方、
    /// `A_PWR_USB` は `manufacturer` と `product` の両方が出る。
    #[test]
    fn public_schema_carries_every_text_field() {
        let Some(file) = open("data-12.0.0") else {
            return;
        };
        let cfg = CustomConfig {
            values: ValueScope::Both,
            ..Default::default()
        };
        let mut buf = Vec::new();
        crate::output::ndjson::write_ndjson(&mut buf, &file, &cfg).unwrap();
        let out = String::from_utf8(buf).unwrap();

        let mut seen_fs = false;
        let mut seen_usb = false;
        for line in out.lines() {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            let texts = |act: &str| -> Vec<(String, Option<String>)> {
                if v["activity"] != act {
                    return Vec::new();
                }
                v["raw"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter(|f| f["kind"] == "identity")
                            .map(|f| {
                                (
                                    f["name"].as_str().unwrap_or("").to_string(),
                                    f["text"].as_str().map(|s| s.to_string()),
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            };

            let fs = texts("A_FS");
            if !fs.is_empty() {
                seen_fs = true;
                let names: Vec<&str> = fs.iter().map(|(n, _)| n.as_str()).collect();
                assert!(names.contains(&"filesystem"), "{names:?}");
                assert!(names.contains(&"mountpoint"), "{names:?}");
                // 両方に実際の値が入る
                assert!(
                    fs.iter().all(|(_, t)| t.is_some()),
                    "文字列が欠けている: {fs:?}"
                );
            }

            let usb = texts("A_PWR_USB");
            if !usb.is_empty() {
                seen_usb = true;
                let names: Vec<&str> = usb.iter().map(|(n, _)| n.as_str()).collect();
                assert!(names.contains(&"manufacturer"), "{names:?}");
                assert!(names.contains(&"product"), "{names:?}");
                // 数値の識別子 (バス番号 / ベンダ ID) は raw に十進で入る
                let ids: Vec<&str> = v["raw"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|f| f["name"] == "vendor_id")
                    .filter_map(|f| f["raw"].as_str())
                    .collect();
                assert_eq!(ids.len(), 1, "vendor_id の生値が無い");
            }
        }
        assert!(seen_fs, "A_FS の行が無い");
        assert!(seen_usb, "A_PWR_USB の行が無い");
    }

    /// 4 種の独自形式がすべて出力できること。
    #[test]
    fn all_custom_formats_produce_output() {
        let Some(file) = open("data-12.0.0") else {
            return;
        };
        let cfg = CustomConfig {
            values: ValueScope::Both,
            ..Default::default()
        };

        let mut buf = Vec::new();
        crate::output::table::write_table(&mut buf, &file, &cfg).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.starts_with("host: "), "{s}");
        assert!(s.contains("A_CPU"), "{s}");
        for line in s.lines() {
            assert!(!line.ends_with(' '), "行末に空白がある: {line:?}");
        }

        let mut buf = Vec::new();
        crate::output::json::write_json(&mut buf, &file, &cfg).unwrap();
        let s = String::from_utf8(buf).unwrap();
        let v: serde_json::Value = serde_json::from_str(&s).expect("独自 JSON は妥当");
        assert_eq!(v["schema_version"], crate::output::json::SCHEMA_VERSION);
        assert!(v["host"]["hostname"].is_string());
        assert!(v["samples"].is_array());

        let mut buf = Vec::new();
        crate::output::csv::write_csv(&mut buf, &file, &cfg).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.starts_with(&crate::output::csv::HEADER.join(",")), "{s}");

        let mut buf = Vec::new();
        crate::output::ndjson::write_ndjson(&mut buf, &file, &cfg).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(!s.is_empty());
        let mut rows = 0;
        for line in s.lines() {
            let v: serde_json::Value = serde_json::from_str(line).expect("NDJSON の各行は妥当");
            assert_eq!(v["schema_version"], crate::output::json::SCHEMA_VERSION);
            assert!(v["host"]["source"].is_string(), "出典が無い: {line}");
            if v["record"] == "sample" {
                assert!(v["activity"].is_string());
                assert!(v["item"].is_string());
                assert!(v["elapsed_cs"].is_u64());
                // 生値は十進文字列
                if let Some(arr) = v["raw"].as_array() {
                    for f in arr {
                        if !f["raw"].is_null() {
                            assert!(f["raw"].is_string(), "生値が数値で出ている: {f}");
                        }
                        assert!(f["quality"].is_string());
                        assert!(f["unit"].is_string());
                    }
                }
            }
            rows += 1;
        }
        assert!(rows > 0);
    }

    /// RESTART を含むファイルでもブロック分割が壊れないこと。
    #[test]
    fn restart_blocks_are_emitted_once_each() {
        let Some(file) = open("data-11.6.5") else {
            return;
        };
        let cfg = SadfConfig::default();
        let restarts = dbppc::scan_restarts(&file).unwrap();

        let mut buf = Vec::new();
        dbppc::write_db(&mut buf, &file, &cfg).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert_eq!(
            s.matches("LINUX-RESTART\t(").count(),
            restarts.len(),
            "RESTART 行はブロックごとに 1 回"
        );
    }
}

// ===========================================================================
// ドキュメントに載っている本家の出力例との突合
// ===========================================================================

/// `docs/format/03-output-format.md` に実測値として載っている行を期待値に固定する。
///
/// データは GPL-2.0-or-later なので同梱できない。
/// `cargo run --bin xtask -- fetch-fixtures` で取得済みのときだけ実行し、
/// 無ければ**何も失敗させずスキップ**する。
#[cfg(test)]
mod golden {
    use super::*;
    use crate::format::file::SaFile;
    use std::path::PathBuf;

    fn open(name: &str) -> Option<SaFile> {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/fixtures/upstream")
            .join(name);
        if !p.exists() {
            return None;
        }
        SaFile::open(&p).ok()
    }

    fn emit(
        file: &SaFile,
        f: fn(&mut Vec<u8>, &SaFile, &SadfConfig) -> crate::Result<()>,
    ) -> String {
        let mut buf = Vec::new();
        f(&mut buf, file, &SadfConfig::default()).expect("出力できること");
        String::from_utf8(buf).expect("UTF-8")
    }

    /// テストデータのホスト名。
    ///
    /// 期待値にホスト名を直書きせず、読み込んだファイルのヘッダから取る。
    fn node(file: &SaFile) -> String {
        FileInfo::from_file(file).nodename
    }

    /// 断片がそのまま行として現れることを確かめる。
    fn assert_has_line(text: &str, want: &str) {
        assert!(
            text.lines().any(|l| l.trim_start() == want),
            "次の行が見つからない:\n  {want}"
        );
    }

    /// `-d` (§3.1 の `data-11.6.5` 実測)。
    #[test]
    fn db_output_matches_documented_lines() {
        let Some(file) = open("data-11.6.5") else {
            eprintln!("fixture 未取得: スキップ");
            return;
        };
        let out = emit(&file, dbppc::write_db);
        let n = node(&file);

        // RESTART 行は `;` 区切りだが LINUX-RESTART の直後だけタブ
        assert_has_line(
            &out,
            &format!("{n};-1;2018-08-29 09:33:38 UTC;LINUX-RESTART\t(8 CPU)"),
        );
        assert_has_line(&out, "# hostname;interval;timestamp;FAN;DEVICE;rpm;drpm");
        assert_has_line(
            &out,
            &format!("{n};46;2018-08-29 09:34:34 UTC;1;f71858fg-isa-0200;1283.00;1283.00"),
        );
        assert_has_line(&out, "# hostname;interval;timestamp;TEMP;DEVICE;degC;%temp");
        assert_has_line(
            &out,
            &format!("{n};46;2018-08-29 09:34:34 UTC;1;f71858fg-isa-0200;34.00;48.57"),
        );
        // CPU 集約行のキーは `-1` (文字列 `all` ではない)
        assert!(
            out.contains(&format!("{n};46;2018-08-29 09:34:34 UTC;-1;")),
            "CPU 集約行のキーが -1 でない"
        );
    }

    /// `-p` (§2.1 の `data-11.6.5` 実測)。アイテム名は接頭辞 + 1 始まり。
    #[test]
    fn ppc_output_matches_documented_lines() {
        let Some(file) = open("data-11.6.5") else {
            return;
        };
        let out = emit(&file, dbppc::write_ppc);
        let n = node(&file);

        assert_has_line(
            &out,
            &format!("{n}\t-1\t2018-08-29 09:33:38 UTC\tLINUX-RESTART\t(8 CPU)"),
        );
        assert_has_line(
            &out,
            &format!("{n}\t46\t2018-08-29 09:34:34 UTC\tfan1\tDEVICE\tf71858fg-isa-0200"),
        );
        assert_has_line(
            &out,
            &format!("{n}\t46\t2018-08-29 09:34:34 UTC\tfan1\trpm\t1283.00"),
        );
        assert_has_line(
            &out,
            &format!("{n}\t46\t2018-08-29 09:34:34 UTC\tfan1\tdrpm\t1283.00"),
        );
        // アイテムを持たない activity はリテラル `-`
        assert!(
            out.contains("\t-\tproc/s\t"),
            "アイテム無しの位置に `-` が入っていない"
        );
    }

    /// `-r` (§4.4 の `data-11.6.5` 実測)。センサ値は小数 6 桁、
    /// `hdr_line` に無い `rpm_min` / `temp_min` / `temp_max` / `in_min` / `in_max` が出る。
    #[test]
    fn raw_output_matches_documented_lines() {
        let Some(file) = open("data-11.6.5") else {
            return;
        };
        let out = emit(&file, raw::write_raw);

        assert_has_line(&out, "09:33:38 UTC; LINUX-RESTART (8 CPU)");
        assert_has_line(
            &out,
            "09:34:34 UTC; FAN; 1; DEVICE; f71858fg-isa-0200; rpm; 1283.000000; rpm_min; 0.000000;",
        );
        assert_has_line(
            &out,
            "09:34:34 UTC; TEMP; 1; DEVICE; f71858fg-isa-0200; degC; 34.000000; temp_min; 0.000000; temp_max; 70.000000;",
        );
        assert_has_line(
            &out,
            "09:34:34 UTC; IN; 0; DEVICE; f71858fg-isa-0200; inV; 3.328000; in_min; 0.000000; in_max; 0.000000;",
        );
        // A_CPU の集約行のアイテム識別子は -1、`-u ALL` の第 2 変種が使われる
        assert!(
            out.contains("09:34:34 UTC; CPU; -1; %usr; "),
            "A_CPU の raw 行頭が想定と違う"
        );
    }

    /// `-j` (§9.8 の `data-11.6.5` 実測)。
    /// `rpm` / `drpm` は整数、`degC` / `inV` は 2 桁小数、番号の起点が違う。
    #[test]
    fn json_output_matches_documented_lines() {
        let Some(file) = open("data-11.6.5") else {
            return;
        };
        let out = emit(&file, json::write_json);

        for want in [
            r#"{"number": 1, "rpm": 1283, "drpm": 1283, "device": "f71858fg-isa-0200"},"#,
            r#"{"number": 1, "degC": 34.00, "percent-temp": 48.57, "device": "f71858fg-isa-0200"},"#,
            r#"{"number": 0, "inV": 3.33, "percent-in": 0.00, "device": "f71858fg-isa-0200"},"#,
        ] {
            assert_has_line(&out, want);
        }
        // power-management ラッパの中に入る (tab 6)
        assert!(
            out.contains("\t\t\t\t\t\"fan-speed\": ["),
            "fan-speed の深さが違う"
        );
        assert!(out.contains("\t\t\t\t\t\"power-management\": {"));
    }

    /// `-x` (§10.9 の `data-11.6.5` 実測)。
    #[test]
    fn xml_output_matches_documented_lines() {
        let Some(file) = open("data-11.6.5") else {
            return;
        };
        let out = emit(&file, xml::write_xml);

        for want in [
            r#"<fan number="1" rpm="1283" drpm="1283" device="f71858fg-isa-0200"/>"#,
            r#"<temp number="1" degC="34.00" percent-temp="48.57" device="f71858fg-isa-0200"/>"#,
            r#"<in number="0" inV="3.33" percent-in="0.00" device="f71858fg-isa-0200"/>"#,
            r#"<fan-speed unit="rpm">"#,
            r#"<temperature unit="degree Celsius">"#,
            r#"<voltage-input unit="V">"#,
            "<power-management>",
        ] {
            assert_has_line(&out, want);
        }
    }

    /// `-dh` はフィールド名一覧行に `[...]` を挟み、1 サンプル = 1 行になる (§3.2)。
    #[test]
    fn horizontal_db_output_packs_a_sample_into_one_line() {
        let Some(file) = open("data-11.6.5") else {
            return;
        };
        let cfg = SadfConfig {
            horizontally: true,
            ..Default::default()
        };
        let mut buf = Vec::new();
        dbppc::write_db(&mut buf, &file, &cfg).unwrap();
        let out = String::from_utf8(buf).unwrap();

        let mut lines = out.lines().skip_while(|l| !l.starts_with("# hostname;"));
        let hdr = lines.next().expect("フィールド名一覧行");
        assert!(hdr.starts_with("# hostname;interval;timestamp;"));
        // アイテムが 2 個以上ある activity の後には [...] が入る
        assert!(hdr.contains("[...]"), "{hdr}");
        // フィールド名一覧行は 1 回だけ
        assert_eq!(out.matches("# hostname;interval;timestamp").count(), 1);

        let n = node(&file);
        let row = lines.next().expect("データ行");
        assert!(row.starts_with(&format!("{n};")), "{row}");
        // 全 activity が 1 行に連なるので、activity ごとに行が分かれない
        assert!(
            row.split(';').count() > 50,
            "1 行に全 activity が入っていない: {} 列",
            row.split(';').count()
        );
    }

    /// `-H` は現行 magic でないファイルでは 2 行で打ち切る (§5)。
    #[test]
    fn header_output_stops_early_for_old_format() {
        let Some(file) = open("data-9.1.6") else {
            return;
        };
        let mut buf = Vec::new();
        header::write_header(&mut buf, &file).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert_eq!(out.lines().count(), 2, "{out}");
        assert!(
            out.lines()
                .next()
                .unwrap()
                .starts_with("System activity data file: ")
        );
        assert!(
            out.lines()
                .nth(1)
                .unwrap()
                .starts_with("File created by sar/sadc from sysstat version 9.1.6")
        );
    }

    /// `A_PWR_USB` の `manufact` / `product` が全形式で出ること。
    ///
    /// 1 item が文字列フィールドを 2 つ持つ activity。`product` が主識別子で、
    /// `manufact` も `ItemSnapshot::texts` から引ける。
    #[test]
    fn usb_strings_appear_in_every_format() {
        let Some(file) = open("data-12.0.0") else {
            eprintln!("fixture 未取得: スキップ");
            return;
        };
        let product = "HP Wireless Keyboard Mouse Kit";

        // -d: hdr_line の列順はバグったまま、データは BUS→idvendor→idprod→
        // maxpower→manufact→product の順 (§11.2 (b))
        let d = emit(&file, dbppc::write_db);
        assert!(
            d.contains(&format!(";3f0;862;196;HP;{product}")),
            "-d に manufact が出ていない"
        );

        // -p: 1 メトリック 1 行なので名前付きで出る
        let p = emit(&file, dbppc::write_ppc);
        assert!(p.contains("\tmanufact\tHP"), "-p に manufact が出ていない");
        assert!(p.contains(&format!("\tproduct\t{product}")));

        // -r: 文字列 2 つはダブルクォートで囲まれる (§14.5-5)
        let r = emit(&file, raw::write_raw);
        assert!(
            r.contains(&format!("manufact; \"HP\"; product; \"{product}\";")),
            "-r の引用付き文字列が出ていない"
        );

        // -j / -x
        let j = emit(&file, json::write_json);
        assert!(j.contains(&format!("\"manufact\": \"HP\", \"product\": \"{product}\"")));
        let x = emit(&file, xml::write_xml);
        assert!(x.contains(&format!("manufact=\"HP\" product=\"{product}\"")));
    }

    /// 空の文字列フィールドは `-d`/`-p`/`-x` で空、`-j` でも `""` (null ではない)。
    ///
    /// 本家は空の `manufact` を空フィールド / `""` として出す (§11.2 (b) の実測)。
    #[test]
    fn empty_strings_stay_empty_not_null() {
        let Some(file) = open("data-12.0.0") else {
            return;
        };
        let j = emit(&file, json::write_json);
        assert!(
            j.contains("\"manufact\": \"\", \"product\": \"\""),
            "-j の空文字が null になっている"
        );
        let x = emit(&file, xml::write_xml);
        assert!(x.contains("manufact=\"\" product=\"\""));
        let d = emit(&file, dbppc::write_db);
        assert!(d.contains(";8087;24;0;;"), "-d の空フィールドが崩れている");
    }

    /// `A_FS` は `-F` が `fs_name`、`-F MOUNT` が `mountp` を出す (§2.8.1)。
    #[test]
    fn filesystem_switches_between_device_and_mountpoint() {
        let Some(file) = open("data-12.0.0") else {
            return;
        };
        let mount = SadfConfig {
            section: SectionConfig {
                fs_mount: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut buf = Vec::new();
        dbppc::write_db(&mut buf, &file, &mount).unwrap();
        let d = String::from_utf8(buf).unwrap();
        assert_has_line(
            &d,
            "# hostname;interval;timestamp;MOUNTPOINT;MBfsfree;MBfsused;%fsused;%ufsused;Ifree;Iused;%Iused",
        );
        // 既定 (-F) はデバイス名、-F MOUNT はマウントポイント
        let plain = emit(&file, dbppc::write_db);
        assert!(plain.contains(";/dev/sda9;"), "-F でデバイス名が出ていない");
        assert!(
            d.lines().any(|l| l.contains(";/;")),
            "-F MOUNT でマウントポイントが出ていない"
        );

        // -x の属性名と -j のキー名も切り替わる
        let mut buf = Vec::new();
        xml::write_xml(&mut buf, &file, &mount).unwrap();
        let x = String::from_utf8(buf).unwrap();
        assert!(
            x.contains("<filesystem mountp=\"/\""),
            "属性名が mountp でない"
        );
        assert!(!x.contains("<filesystem fsname="));

        let mut buf = Vec::new();
        json::write_json(&mut buf, &file, &mount).unwrap();
        let j = String::from_utf8(buf).unwrap();
        assert!(
            j.contains("{\"mountpoint\": \"/\""),
            "キー名が mountpoint でない"
        );

        // -r のラベルも MOUNTPOINT になり、値はダブルクォート付き
        let mut buf = Vec::new();
        raw::write_raw(&mut buf, &file, &mount).unwrap();
        let r = String::from_utf8(buf).unwrap();
        assert!(
            r.contains("; MOUNTPOINT; \"/\";"),
            "-r のラベルが切り替わっていない"
        );
    }

    /// `A_DISK` のデバイス名はファイルに無いので `dev<major>-<minor>` を組み立てる。
    ///
    /// ローカルの `/sys` は引かない (他ホストのファイルで誤った名前が出る、§2.8.1)。
    #[test]
    fn disk_names_use_the_major_minor_fallback() {
        let Some(file) = open("data-12.0.0") else {
            return;
        };
        let d = emit(&file, dbppc::write_db);
        assert!(
            d.contains(";dev8-0;"),
            "-d のデバイス名が dev<major>-<minor> でない"
        );

        let x = emit(&file, xml::write_xml);
        assert!(x.contains("<disk-device dev=\"dev8-0\""));

        let j = emit(&file, json::write_json);
        assert!(j.contains("{\"disk-device\": \"dev8-0\""));

        // raw は直書きの major / minor が hdr_line のラベルより前に出る (§4.5)
        let r = emit(&file, raw::write_raw);
        assert!(
            r.contains("; major; 8; minor; 0; DEV; dev8-0; tps; "),
            "-r の major/minor/DEV の並びが違う"
        );
    }

    /// `-j` / `-x` で `<io>` と `<memory>` の形が違うこと (§10.3 / §9.5)。
    #[test]
    fn io_and_memory_shapes_differ_between_json_and_xml() {
        let Some(file) = open("data-12.0.0") else {
            return;
        };

        let x = emit(&file, xml::write_xml);
        // A_IO は <tps> だけテキスト内容、残り 3 つは属性
        assert!(x.contains("<tps>"), "A_IO の tps がテキスト内容でない");
        assert!(x.contains("<io-reads rtps="));
        // A_MEMORY は全値がテキスト内容
        assert!(x.contains("<memory unit=\"kB\">"));
        assert!(x.contains("<memfree>"));

        let j = emit(&file, json::write_json);
        // JSON の A_IO は入れ子オブジェクト、A_MEMORY はフラット
        assert!(j.contains("\"io\": {\"tps\": "));
        assert!(j.contains("\"io-reads\": {\"rtps\": "));
        assert!(j.contains("\"memory\": {\"memfree\": "));
        // hugepages は power-management の中ではない (§9.6-14)
        let pm = j.find("\"power-management\"");
        let hp = j.find("\"hugepages\"");
        if let (Some(pm), Some(hp)) = (pm, hp) {
            assert!(hp < pm, "hugepages が power-management の後に来ている");
        }
    }
}

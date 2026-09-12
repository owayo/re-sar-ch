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
//! 出力層では再計算しない。まだ計算式が実装されていない列
//! ([`crate::series::compute::ComputeIssue::NotImplemented`]) は
//! **0 にせず「値なし」として出す** ([`Unavailable`])。
//! 0 を代入すると「正常に 0」と区別できなくなり、集計が静かに誤る。

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
use crate::series::compute::{ComputeIssue, Computed};

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
/// 運ぶ。`sadf` 互換形式はテキストで区別を表現できないので表記は 1 つに潰れるが、
/// **0 にはしない**。
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

/// `-d` / `-p` / `-r` で値が無いときの表記。
///
/// 空フィールドにする。`0` を書くと正常値と区別できない。
pub const ABSENT_TEXT: &str = "";
/// `-j` で値が無いときの表記。
pub const ABSENT_JSON: &str = "null";
/// `-x` で値が無いときの属性値。
pub const ABSENT_XML: &str = "";

// ===========================================================================
// 数値の書式
// ===========================================================================

/// 表示値を書式化して書き出す。
///
/// `absent` は値が無いときに書く文字列 (形式ごとに違う)。
pub fn write_value(out: &mut String, v: Computed, fmt: Fmt, absent: &str) {
    match v {
        Ok(x) => write_f64(out, x, fmt),
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
    /// `YYYY-MM-DD` (ファイルヘッダの年月日)。
    pub file_date: String,
    /// `HH:MM:SS` (UTC)。
    pub file_utc_time: String,
    /// ファイル作成時刻 (epoch 秒)。
    pub ust_time: u64,
    /// 収集時のタイムゾーン名。古いファイルは空。
    pub tzname: String,
}

impl FileInfo {
    pub fn from_file(file: &SaFile) -> Self {
        let h = file.header();
        let cpu_count = match h.cpu_nr {
            Some(n) if n > 1 => n - 1,
            _ => 1,
        };
        let utc = utc_of(h.ust_time);
        Self {
            nodename: h.nodename.clone(),
            sysname: h.sysname.clone(),
            release: h.release.clone(),
            machine: h.machine.clone(),
            cpu_count,
            file_date: format!("{:04}-{:02}-{:02}", h.year, h.month, h.day),
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
    /// `-U` のときは epoch 秒だけ。`-t` で `sa_tzname` が空のときは TZ を出さない
    /// (`print_dbppc_timestamp` の `strlen(sa_tzname)` チェック)。
    pub fn dbppc(&self) -> String {
        if self.date.is_empty() {
            return self.time.clone();
        }
        if self.tz.is_empty() {
            return format!("{} {}", self.date, self.time);
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
fn shift_to_recorded(ust_time: u64, (h, m, s): (u8, u8, u8)) -> u64 {
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
    if let Ok(tz) = std::env::var("TZ") {
        if let Ok(tz) = tz.parse::<chrono_tz::Tz>() {
            let now = Utc::now().naive_utc();
            if let Some(abbr) = tz.offset_from_utc_datetime(&now).abbreviation() {
                return abbr.to_string();
            }
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

    /// `-t` かつ `sa_tzname` が空なら TZ 欄そのものを出さない。
    #[test]
    fn true_time_without_tzname_omits_tz() {
        let info = dummy_info();
        let s = Stamp::new(TimeBase::TrueTime, 1_555_593_619, (13, 20, 19), &info);
        assert_eq!(s.dbppc(), "2019-04-18 13:20:19");
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

#[cfg(test)]
mod zzdump {
    use super::*;
    use crate::format::file::SaFile;
    use crate::output::json::{CustomConfig, ValueScope};
    use std::path::PathBuf;

    #[test]
    fn dump() {
        let dir = PathBuf::from("/tmp/claude-501/-Users-owa-GitHub-re-sar-ch/f36d1cd0-7ca0-4853-8503-c054a06b69af/scratchpad/out");
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["data-12.0.0", "data-11.6.5"] {
            let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/fixtures/upstream").join(name);
            if !p.exists() { continue; }
            let file = match SaFile::open(&p) { Ok(f) => f, Err(e) => { eprintln!("{name}: {e}"); continue } };
            let cfg = SadfConfig::default();
            let mut b = Vec::new(); header::write_header(&mut b, &file).unwrap(); std::fs::write(dir.join(format!("{name}.H")), &b).unwrap();
            let mut b = Vec::new(); dbppc::write_db(&mut b, &file, &cfg).unwrap(); std::fs::write(dir.join(format!("{name}.d")), &b).unwrap();
            let mut b = Vec::new(); dbppc::write_ppc(&mut b, &file, &cfg).unwrap(); std::fs::write(dir.join(format!("{name}.p")), &b).unwrap();
            let mut b = Vec::new(); raw::write_raw(&mut b, &file, &cfg).unwrap(); std::fs::write(dir.join(format!("{name}.r")), &b).unwrap();
            let mut b = Vec::new(); json::write_json(&mut b, &file, &cfg).unwrap(); std::fs::write(dir.join(format!("{name}.j")), &b).unwrap();
            let mut b = Vec::new(); xml::write_xml(&mut b, &file, &cfg).unwrap(); std::fs::write(dir.join(format!("{name}.x")), &b).unwrap();
            let dbg = SadfConfig { debug: true, ..SadfConfig::default() };
            let mut b = Vec::new(); raw::write_raw(&mut b, &file, &dbg).unwrap(); std::fs::write(dir.join(format!("{name}.rdebug")), &b).unwrap();
            let c = CustomConfig { values: ValueScope::Both, ..Default::default() };
            let mut b = Vec::new(); crate::output::table::write_table(&mut b, &file, &c).unwrap(); std::fs::write(dir.join(format!("{name}.table")), &b).unwrap();
            let mut b = Vec::new(); crate::output::ndjson::write_ndjson(&mut b, &file, &c).unwrap(); std::fs::write(dir.join(format!("{name}.ndjson")), &b).unwrap();
            let mut b = Vec::new(); crate::output::csv::write_csv(&mut b, &file, &c).unwrap(); std::fs::write(dir.join(format!("{name}.csv")), &b).unwrap();
        }
    }
}

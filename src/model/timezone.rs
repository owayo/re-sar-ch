//! 独自出力が時刻を解釈・表示するときの基準。
//!
//! **表示・`--from` / `--to` の比較・`detect` の日内境界を同じ基準から導く。**
//! 表示だけローカルにして比較を UTC のままにすると、画面に出ている 09:00 と
//! `--from 09:00` が食い違う。基準を 1 箇所に集めてその不整合を構造的に防ぐ。
//!
//! 互換出力 (`sar` / `sadf` / `sa2sar`) はここを**使わない**。本家の規則
//! (`sar` の既定は読み手のローカル、`sadf` の既定は UTC、`-t` は記録時刻) は
//! `output::sar_text::TimeStyle` と `output::sadf::TimeBase` が持っている。
//!
//! # epoch → 現地時刻は常に一意
//!
//! 夏時間の「存在しない時刻」「2 回ある時刻」は *現地時刻 → epoch* の向きで
//! しか起きない。独自コマンドの `hh:mm[:ss]` は絶対時刻へ解かず**毎日の
//! 壁時計**として比べるので、春の移行で消える 02:30 は「該当サンプルが無い」、
//! 秋の移行で 2 度現れる 01:30 は「どちらも該当する」となり、破綻しない。

use chrono::{DateTime, FixedOffset, Local, TimeZone, Timelike, Utc};

/// 時刻の解釈と表示に使うタイムゾーン。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayTz {
    /// UTC。表示は末尾 `Z`。
    Utc,
    /// IANA タイムゾーン。`local` を解決できた場合もここに入る。
    Named(chrono_tz::Tz),
    /// 実行環境のローカル時刻。**IANA 名を特定できなかったときだけ**使う。
    ///
    /// 変換自体は `chrono::Local` が epoch ごとに行うので、
    /// 夏時間や歴史的なオフセット変更も正しく反映される。
    /// 名前が無いぶん、表示ラベルだけが数値オフセットになる。
    System,
}

impl Default for DisplayTz {
    /// **構造体を組み立てるための既定であって、CLI の既定ではない。**
    ///
    /// CLI は `--timezone` の解決結果 (既定 `local`) を必ず明示的に渡す。
    /// ここを `local()` にすると、渡し忘れた経路の出力が実行環境で変わり、
    /// テストの期待値も環境依存になる。渡し忘れを「常に UTC」という
    /// 再現可能な失敗に寄せるため、既定は [`DisplayTz::Utc`] にしてある。
    fn default() -> Self {
        Self::Utc
    }
}

impl DisplayTz {
    /// 実行環境のローカルタイムゾーンを解決する。
    ///
    /// 1. `TZ` が IANA 名ならそれを使う (`TZ=UTC` は [`DisplayTz::Utc`] に寄せる)
    /// 2. OS から IANA 名を取れればそれを使う (macOS / Linux とも `TZ` 未設定で有効)
    /// 3. どちらも取れなければ [`DisplayTz::System`]
    ///
    /// `TZ=UTC` を `Named(Tz::UTC)` ではなく [`DisplayTz::Utc`] にするのは、
    /// 「UTC で見たい」と言われたときに `+00:00` ではなく `Z` を出すため。
    pub fn local() -> Self {
        if let Ok(name) = std::env::var("TZ")
            && let Some(tz) = parse_tz(&name)
        {
            return Self::from_tz(tz);
        }
        if let Some(tz) = system_zone() {
            return Self::from_tz(tz);
        }
        Self::System
    }

    /// `--timezone` の値を解釈する。
    ///
    /// 受け付けるのは `local` / `utc` / IANA 名 (`Asia/Tokyo` など)。
    pub fn parse(value: &str) -> Result<Self, String> {
        let v = value.trim();
        if v.eq_ignore_ascii_case("local") {
            return Ok(Self::local());
        }
        if v.eq_ignore_ascii_case("utc") {
            return Ok(Self::Utc);
        }
        parse_tz(v).map(Self::from_tz).ok_or_else(|| {
            format!(
                "{value}: タイムゾーンとして解釈できません \
                 (local / utc / IANA 名 (例 Asia/Tokyo) を指定してください)"
            )
        })
    }

    fn from_tz(tz: chrono_tz::Tz) -> Self {
        if tz == chrono_tz::Tz::UTC {
            Self::Utc
        } else {
            Self::Named(tz)
        }
    }

    /// epoch 秒をこのタイムゾーンの日時へ開く。
    ///
    /// epoch から現地時刻への変換は一意なので、失敗するのは
    /// `i64` に収まらない値など、暦に開けない入力のときだけ。
    pub fn at(&self, ust: u64) -> Option<DateTime<FixedOffset>> {
        let secs = i64::try_from(ust).ok()?;
        match self {
            Self::Utc => Utc
                .timestamp_opt(secs, 0)
                .single()
                .map(|d| d.fixed_offset()),
            Self::Named(tz) => tz.timestamp_opt(secs, 0).single().map(|d| d.fixed_offset()),
            Self::System => Local
                .timestamp_opt(secs, 0)
                .single()
                .map(|d| d.fixed_offset()),
        }
    }

    /// `YYYY-MM-DD HH:MM:SS` にタイムゾーンを添えた表記。
    ///
    /// UTC は従来どおり末尾 `Z`、それ以外は `+09:00` のような数値オフセット。
    /// 略称 (`JST` / `CST`) は使わない。重複する略称があるうえ、
    /// 夏時間の切り替え日に同じ壁時計が 2 度現れたとき区別できない。
    pub fn datetime(&self, ust: u64) -> String {
        match self.at(ust) {
            Some(dt) if *self == Self::Utc => dt.format("%Y-%m-%d %H:%M:%SZ").to_string(),
            Some(dt) => dt.format("%Y-%m-%d %H:%M:%S%:z").to_string(),
            None => ust.to_string(),
        }
    }

    /// `YYYY-MM-DD`。
    pub fn date(&self, ust: u64) -> String {
        match self.at(ust) {
            Some(dt) => dt.format("%Y-%m-%d").to_string(),
            None => ust.to_string(),
        }
    }

    /// `HH:MM:SS`。タイムゾーンは列見出しなど**まとまった単位**で 1 回出す。
    ///
    /// 表の各行で呼ばれるので、`strftime` (`format("%H:%M:%S")`) は使わない。
    /// 実測で 1 回あたり 585 ns かかり、その大半が書式解釈だった
    /// (手で組むと 100 ns 台。`examples/` のマイクロベンチで計測)。
    pub fn time(&self, ust: u64) -> String {
        match self.at(ust) {
            Some(dt) => format!("{:02}:{:02}:{:02}", dt.hour(), dt.minute(), dt.second()),
            None => ust.to_string(),
        }
    }

    /// `MM-DD HH:MM`。日を跨ぐ軸の目盛に使う。
    pub fn month_day_time(&self, ust: u64) -> String {
        match self.at(ust) {
            Some(dt) => dt.format("%m-%d %H:%M").to_string(),
            None => ust.to_string(),
        }
    }

    /// 現地の 00:00:00 からの経過秒 (0..=86399)。
    ///
    /// 暦に開けない入力では UTC の剰余へ落とす (順序が壊れるよりはましな近似)。
    pub fn seconds_of_day(&self, ust: u64) -> u64 {
        match self.at(ust) {
            Some(dt) => u64::from(dt.num_seconds_from_midnight()),
            None => ust % 86_400,
        }
    }

    /// 時・分・秒 (`--from` / `--to` の階層比較に使う)。
    pub fn hms(&self, ust: u64) -> Option<(u32, u32, u32)> {
        self.at(ust).map(|dt| (dt.hour(), dt.minute(), dt.second()))
    }

    /// 表示に添えるタイムゾーンの名前。
    ///
    /// IANA 名が分かっていればそれ (`Asia/Tokyo`)、分からなければ
    /// **その時点の**数値オフセット (`+09:00`)。実行時のオフセットを
    /// 過去のデータへ当てはめないよう、必ず対象の epoch を渡す。
    pub fn label_at(&self, ust: u64) -> String {
        match self {
            Self::Utc => "UTC".to_string(),
            Self::Named(tz) => tz.name().to_string(),
            Self::System => match self.at(ust) {
                Some(dt) => dt.offset().to_string(),
                None => "local".to_string(),
            },
        }
    }

    /// UTC かどうか (従来表記を保つ判定に使う)。
    pub fn is_utc(&self) -> bool {
        *self == Self::Utc
    }
}

/// OS が設定しているタイムゾーンの IANA 名。
///
/// **`/etc/localtime` のリンク先を先に見る。** `iana-time-zone` は macOS で
/// CoreFoundation を初期化するため 1 プロセスあたり約 2 ms かかり、
/// 38 KB のファイルを 1 本読むだけの実行 (実測 9.5 ms) で 2 割強を占めた。
/// シンボリックリンクの解決は `readlink` 1 回で済む。
///
/// `/etc/localtime` が実ファイルの環境や Windows では読めないので、
/// そのときだけ `iana-time-zone` へ落とす。
fn system_zone() -> Option<chrono_tz::Tz> {
    #[cfg(unix)]
    if let Ok(path) = std::fs::read_link("/etc/localtime")
        && let Some(s) = path.to_str()
        && let Some(i) = s.find("/zoneinfo/")
        && let Some(tz) = parse_tz(&s[i + "/zoneinfo/".len()..])
    {
        return Some(tz);
    }
    iana_time_zone::get_timezone()
        .ok()
        .and_then(|name| parse_tz(&name))
}

/// IANA 名を解釈する。`TZ` が `:Asia/Tokyo` 形式でも受ける。
fn parse_tz(name: &str) -> Option<chrono_tz::Tz> {
    let n = name.trim().trim_start_matches(':');
    if n.is_empty() {
        return None;
    }
    n.parse::<chrono_tz::Tz>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2019-06-30 05:39:33 UTC = 同日 14:39:33 JST。
    const UST: u64 = 1_561_873_173;

    #[test]
    fn utc_keeps_the_z_suffix() {
        let tz = DisplayTz::Utc;
        assert_eq!(tz.datetime(UST), "2019-06-30 05:39:33Z");
        assert_eq!(tz.time(UST), "05:39:33");
        assert_eq!(tz.label_at(UST), "UTC");
    }

    #[test]
    fn named_zone_shows_a_numeric_offset() {
        let tz = DisplayTz::parse("Asia/Tokyo").unwrap();
        assert_eq!(tz.datetime(UST), "2019-06-30 14:39:33+09:00");
        assert_eq!(tz.time(UST), "14:39:33");
        assert_eq!(tz.label_at(UST), "Asia/Tokyo");
        assert_eq!(tz.seconds_of_day(UST), 14 * 3600 + 39 * 60 + 33);
    }

    /// 日付が繰り上がる (UTC の前日 23:30 は JST の翌日 08:30)。
    #[test]
    fn the_date_rolls_over_with_the_offset() {
        let tz = DisplayTz::parse("Asia/Tokyo").unwrap();
        // 2019-06-29 23:30:00 UTC
        let ust = 1_561_851_000;
        assert_eq!(tz.datetime(ust), "2019-06-30 08:30:00+09:00");
        assert_eq!(DisplayTz::Utc.datetime(ust), "2019-06-29 23:30:00Z");
    }

    /// 30 分刻みでないオフセットも崩れない。
    #[test]
    fn a_45_minute_offset_is_handled() {
        let tz = DisplayTz::parse("Asia/Kathmandu").unwrap();
        assert_eq!(tz.datetime(UST), "2019-06-30 11:24:33+05:45");
        assert_eq!(tz.seconds_of_day(UST), 11 * 3600 + 24 * 60 + 33);
    }

    /// 夏時間の切り替えで同じ壁時計が 2 度現れても、オフセットで区別できる。
    #[test]
    fn a_repeated_wall_clock_is_distinguished_by_its_offset() {
        let tz = DisplayTz::parse("America/New_York").unwrap();
        // 2019-11-03 05:30:00 UTC = 01:30 EDT (-04:00)
        assert_eq!(tz.datetime(1_572_759_000), "2019-11-03 01:30:00-04:00");
        // 2019-11-03 06:30:00 UTC = 01:30 EST (-05:00)
        assert_eq!(tz.datetime(1_572_762_600), "2019-11-03 01:30:00-05:00");
        // どちらも「毎日の 01:30」として同じ日内秒を持つ
        assert_eq!(tz.seconds_of_day(1_572_759_000), 5400);
        assert_eq!(tz.seconds_of_day(1_572_762_600), 5400);
    }

    /// `utc` は `Named(Tz::UTC)` ではなく [`DisplayTz::Utc`] に寄せる。
    #[test]
    fn utc_spellings_collapse_to_one_variant() {
        assert_eq!(DisplayTz::parse("utc").unwrap(), DisplayTz::Utc);
        assert_eq!(DisplayTz::parse("UTC").unwrap(), DisplayTz::Utc);
        assert_eq!(DisplayTz::parse("Utc").unwrap(), DisplayTz::Utc);
    }

    #[test]
    fn an_unknown_zone_is_rejected() {
        let e = DisplayTz::parse("Asia/Nowhere").unwrap_err();
        assert!(e.contains("Asia/Nowhere"), "入力を含める: {e}");
        assert!(e.contains("IANA"), "受け付ける形を示す: {e}");
    }

    /// 渡し忘れを環境依存にしないため、既定は UTC。
    #[test]
    fn the_struct_default_is_utc_not_local() {
        assert_eq!(DisplayTz::default(), DisplayTz::Utc);
    }
}

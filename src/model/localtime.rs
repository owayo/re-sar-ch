//! 互換出力の「読み手のローカル時刻」。本家の `localtime()` に当たる。
//!
//! 本家の `sar` は既定で、`sadf` は `-T` で、時刻を `localtime()` で開く。
//! `localtime()` は `TZ` に従うので、同じファイルでも `TZ` を変えれば時刻が変わる。
//! 独自出力の基準 ([`crate::model::timezone`]) とは別の規則で、
//! 互換出力 (`sar_text` / `sar_el7` / `sadf` / `time_filter`) はここを使う。
//!
//! # OS で分ける理由
//!
//! **Unix では `chrono::Local` をそのまま使う。** `chrono::Local` は libc と同じく
//! `TZ` とシステムの tzdata で変換する。互換出力は本家とバイト単位で一致させる
//! ものなので、本家と同じ tzdata を使う必要がある (`chrono-tz` が同梱する tzdata は
//! 版が違いうる)。
//!
//! **Windows では `TZ` が IANA 名ならそのタイムゾーンで変換する。** Windows の
//! `chrono::Local` は `TZ` を読まず、OS のタイムゾーン設定だけを見る。そのままでは
//! 同じ `TZ` でも Windows だけ時刻が変わり、`TZ` を読む独自出力
//! ([`crate::model::DisplayTz::local`]) や `sadf -T` のタイムゾーン名とも食い違う。
//! `TZ` が無いか IANA 名でなければ、OS の設定 (`chrono::Local`) に任せる。
//!
//! 環境を読むのは CLI 層だけという約束 ([`crate::model::sysstat_env`]) の例外に
//! 当たるが、Unix の `chrono::Local` も出力層の中で `TZ` を読んでいる。
//! `localtime()` の意味をどの OS でもそろえるための読み取りで、ほかの環境変数は読まない。

use chrono::{DateTime, FixedOffset, Local, TimeZone, Utc};

/// エポック秒を読み手のローカル時刻へ開く (`localtime()`)。
///
/// 暦に開けない値 (`i64` の範囲外の年など) は `None`。
pub fn localtime(secs: i64) -> Option<DateTime<FixedOffset>> {
    #[cfg(not(unix))]
    if let Some(tz) = tz_from_env() {
        return tz
            .timestamp_opt(secs, 0)
            .single()
            .map(|dt| dt.fixed_offset());
    }
    Local
        .timestamp_opt(secs, 0)
        .single()
        .map(|dt| dt.fixed_offset())
}

/// 現在時刻の [`localtime`] (`localtime(time(NULL))`)。
pub fn now() -> DateTime<FixedOffset> {
    let utc = Utc::now();
    localtime(utc.timestamp()).unwrap_or_else(|| utc.fixed_offset())
}

/// `TZ` の IANA 名 (Windows だけが読む)。
#[cfg(not(unix))]
fn tz_from_env() -> Option<chrono_tz::Tz> {
    crate::model::timezone::parse_tz(&std::env::var("TZ").ok()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Timelike;

    /// 1 日の中の秒と UTC からのずれは、どのタイムゾーンでも辻褄が合う。
    /// 実際のタイムゾーンは実行環境しだいなので、値そのものは比べない
    /// (`TZ` による違いは `tests/sar_el7.rs` がプロセスを分けて確かめる)。
    #[test]
    fn local_time_is_consistent_with_its_offset() {
        const UST: i64 = 1_600_000_000;
        let dt = localtime(UST).expect("an ordinary epoch opens");
        assert_eq!(dt.timestamp(), UST);
        let local_secs = UST + i64::from(dt.offset().local_minus_utc());
        assert_eq!(
            i64::from(dt.num_seconds_from_midnight()),
            local_secs.rem_euclid(86_400)
        );
    }

    #[test]
    fn now_is_close_to_the_utc_clock() {
        let before = Utc::now().timestamp();
        let now = now().timestamp();
        let after = Utc::now().timestamp();
        assert!((before..=after).contains(&now));
    }
}

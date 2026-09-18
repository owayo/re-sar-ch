//! 出力に使う言語の決定。
//!
//! **決めるのは 1 箇所だけ。** 表示言語が層ごとに別々に決まると、
//! 見出しは英語なのに留保は日本語、といった混在が起きる。
//!
//! # 判定の順序
//!
//! 1. `--lang` (明示指定)
//! 2. `RESARCH_LANG`
//! 3. `LC_ALL` → `LC_MESSAGES` → `LANG` (POSIX の優先順)
//! 4. ローカルタイムゾーンが `Asia/Tokyo` なら日本語
//! 5. 既定は英語
//!
//! **ロケール環境変数をタイムゾーンより先に見る。** ロケールは
//! 「どの言語で読みたいか」の宣言そのもので、タイムゾーンは
//! 「どこにいるか」でしかない。日本から英語環境で使う人は珍しくないし、
//! 海外の日本語環境もある。`LANG=en_US.UTF-8` を設定している人に
//! タイムゾーンを理由に日本語を出すのは、指示を無視することになる。
//!
//! 互換出力 (`sar` / `sadf` / `sa2sar`) はここを**使わない**。
//! 本家の書式をそのまま出すので、言語の選択肢が無い。

use crate::model::DisplayTz;

/// 出力に使う言語。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Lang {
    /// 英語。**既定**。
    #[default]
    En,
    /// 日本語。
    Ja,
}

/// ロケールを読む環境変数 (POSIX の優先順)。
const LOCALE_VARS: [&str; 3] = ["LC_ALL", "LC_MESSAGES", "LANG"];

/// 日本語と判定するタイムゾーン。
const JAPAN_ZONE: chrono_tz::Tz = chrono_tz::Tz::Asia__Tokyo;

impl Lang {
    /// 実行環境から決める。
    pub fn from_env() -> Self {
        Self::resolve(
            &|k| std::env::var(k).ok(),
            || matches!(DisplayTz::local(), DisplayTz::Named(tz) if tz == JAPAN_ZONE),
        )
    }

    /// 判定の本体。
    ///
    /// 環境変数の読み取りとタイムゾーンの解決を引数で受けるのは、
    /// **テストが環境変数を書き換えなくて済むようにするため**。
    /// `std::env::set_var` は edition 2024 で `unsafe` であり、
    /// 並行実行するテストの間で状態が漏れる。
    fn resolve(
        env: &dyn Fn(&str) -> Option<String>,
        local_is_japan: impl FnOnce() -> bool,
    ) -> Self {
        if let Some(lang) = env("RESARCH_LANG").as_deref().and_then(Self::parse) {
            return lang;
        }
        for key in LOCALE_VARS {
            let Some(value) = env(key) else { continue };
            let value = value.trim();
            if value.is_empty() {
                continue;
            }
            // **ここで判定を打ち切る。** ロケールが設定されているなら、
            // それが日本語でなくても「日本語ではない」という情報である。
            // `fr_FR` の環境でタイムゾーンを理由に日本語へ倒してはいけない。
            return Self::parse(value).unwrap_or(Self::En);
        }
        if local_is_japan() {
            return Self::Ja;
        }
        Self::En
    }

    /// 言語タグを解釈する。
    ///
    /// `ja_JP.UTF-8` / `ja-JP` / `ja` / `japanese` を日本語とみなす。
    /// 知らない言語は [`None`] を返し、呼び手が既定へ倒す。
    /// `C` / `POSIX` は「ロケールを使わない」の意味なので英語。
    pub fn parse(tag: &str) -> Option<Self> {
        let tag = tag.trim();
        if tag.is_empty() {
            return None;
        }
        if tag.eq_ignore_ascii_case("C") || tag.eq_ignore_ascii_case("POSIX") {
            return Some(Self::En);
        }
        // `ja_JP.UTF-8@foo` の先頭要素だけを見る
        let primary = tag.split(['_', '-', '.', '@']).next()?;
        match primary.to_ascii_lowercase().as_str() {
            "ja" | "jpn" | "japanese" => Some(Self::Ja),
            "en" | "eng" | "english" => Some(Self::En),
            _ => None,
        }
    }

    /// `--lang` の値を解釈する。
    ///
    /// 未知の値は**黙って既定へ倒さない**。打ち間違いが
    /// 「英語で出た」という形でしか現れないと気付けない。
    pub fn parse_option(value: &str) -> Result<Self, String> {
        Self::parse(value).ok_or_else(|| {
            format!("{value}: 言語として解釈できません (ja / en を指定してください)")
        })
    }

    /// 言語タグ (`ja` / `en`)。出力のメタデータに載せる。
    pub fn tag(self) -> &'static str {
        match self {
            Self::Ja => "ja",
            Self::En => "en",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// 環境変数の代わり。
    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |k: &str| map.get(k).cloned()
    }

    #[test]
    fn the_app_specific_variable_wins() {
        let env = env_of(&[("RESARCH_LANG", "ja"), ("LC_ALL", "en_US.UTF-8")]);
        assert_eq!(Lang::resolve(&env, || false), Lang::Ja);
    }

    #[test]
    fn locale_variables_follow_the_posix_order() {
        let env = env_of(&[("LC_ALL", "ja_JP.UTF-8"), ("LANG", "en_US.UTF-8")]);
        assert_eq!(Lang::resolve(&env, || false), Lang::Ja);

        let env = env_of(&[("LC_MESSAGES", "en_US.UTF-8"), ("LANG", "ja_JP.UTF-8")]);
        assert_eq!(Lang::resolve(&env, || true), Lang::En);
    }

    /// **ロケールはタイムゾーンより強い。**
    ///
    /// 日本にいながら英語環境で使う人の指定を、位置で上書きしない。
    #[test]
    fn an_explicit_locale_beats_the_timezone() {
        let env = env_of(&[("LANG", "en_US.UTF-8")]);
        assert_eq!(Lang::resolve(&env, || true), Lang::En);
    }

    /// 知らない言語のロケールでも「日本語ではない」は分かる。
    #[test]
    fn an_unknown_locale_does_not_fall_through_to_the_timezone() {
        let env = env_of(&[("LANG", "fr_FR.UTF-8")]);
        assert_eq!(Lang::resolve(&env, || true), Lang::En);
    }

    /// ロケールが無いときだけタイムゾーンを見る。
    #[test]
    fn the_timezone_decides_when_no_locale_is_set() {
        let env = env_of(&[]);
        assert_eq!(Lang::resolve(&env, || true), Lang::Ja);
        assert_eq!(Lang::resolve(&env, || false), Lang::En);
    }

    /// 空文字の `LANG` は「設定されていない」と同じに扱う。
    #[test]
    fn an_empty_locale_is_skipped() {
        let env = env_of(&[("LC_ALL", ""), ("LANG", "ja_JP.UTF-8")]);
        assert_eq!(Lang::resolve(&env, || false), Lang::Ja);
    }

    #[test]
    fn the_c_locale_means_english() {
        for tag in ["C", "POSIX", "c"] {
            let env = env_of(&[("LANG", tag)]);
            assert_eq!(Lang::resolve(&env, || true), Lang::En, "{tag}");
        }
    }

    #[test]
    fn tags_are_parsed_case_insensitively() {
        for tag in ["ja", "JA", "ja_JP", "ja-JP", "ja_JP.UTF-8", "japanese"] {
            assert_eq!(Lang::parse(tag), Some(Lang::Ja), "{tag}");
        }
        for tag in ["en", "en_US.UTF-8", "English"] {
            assert_eq!(Lang::parse(tag), Some(Lang::En), "{tag}");
        }
        assert_eq!(Lang::parse("de_DE"), None);
        assert_eq!(Lang::parse(""), None);
    }

    /// 打ち間違いは黙って既定へ倒さない。
    #[test]
    fn the_option_rejects_an_unknown_value() {
        assert_eq!(Lang::parse_option("ja"), Ok(Lang::Ja));
        assert!(Lang::parse_option("jp").is_err());
        assert!(Lang::parse_option("japanes").is_err());
    }
}

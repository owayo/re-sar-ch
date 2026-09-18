//! 出力に使う言語の決定。
//!
//! **決めるのは 1 箇所だけ。** 表示言語が層ごとに別々に決まると、
//! 見出しは英語なのに留保は日本語、といった混在が起きる。
//!
//! # 判定の順序
//!
//! 1. `--lang` (明示指定)
//! 2. `RESARCH_LANG`
//! 3. `LC_ALL` → `LC_MESSAGES` → `LANG` (POSIX の優先順)。
//!    ただし値が `C` / `POSIX` / `C.UTF-8` なら**そこで英語に確定**する
//! 4. `LANGUAGE` (GNU gettext の「メッセージに使いたい言語」。`:` 区切りの先頭一致)
//! 5. ローカルタイムゾーンが日本なら日本語
//! 6. 既定は英語
//!
//! **ロケール環境変数をタイムゾーンより先に見る。** ロケールは
//! 「どの言語で読みたいか」の宣言そのもので、タイムゾーンは
//! 「どこにいるか」でしかない。日本から英語環境で使う人は珍しくないし、
//! 海外の日本語環境もある。`LANG=en_US.UTF-8` を設定している人に
//! タイムゾーンを理由に日本語を出すのは、指示を無視することになる。
//!
//! **`LC_ALL=C` は `LANGUAGE` より強い。** `C` ロケールは
//! 「決定的な英語出力がほしい」という意思表示で、スクリプトやテストが
//! これに頼る。ここで `LANGUAGE=ja` に負けると、その決定性が壊れる。
//!
//! **タイムゾーンは識別子で見る。** オフセットが UTC+9 かどうかで
//! 判定すると韓国 (`Asia/Seoul`) も日本語になる。
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

/// 言語をどこから決めたか。
///
/// **診断とテストのために持つ。** 「なぜ英語になったのか」を
/// 利用者が追えないと、環境変数の設定ミスが「壊れている」に見える。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LangSource {
    /// `--lang`。
    Option,
    /// `RESARCH_LANG`。
    AppEnv,
    /// `LC_ALL` / `LC_MESSAGES` / `LANG`。
    Locale,
    /// `LANGUAGE`。
    GnuLanguage,
    /// ローカルタイムゾーン。
    TimeZone,
    /// どれも判定材料にならなかった。
    Default,
}

/// 判定の結果と、その出所。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedLang {
    pub lang: Lang,
    pub source: LangSource,
}

/// ロケールを読む環境変数 (POSIX の優先順)。
const LOCALE_VARS: [&str; 3] = ["LC_ALL", "LC_MESSAGES", "LANG"];

/// 日本語と判定するタイムゾーン。
///
/// `Japan` は `Asia/Tokyo` の IANA エイリアスで、`chrono-tz` では別の値になる。
const JAPAN_ZONES: [chrono_tz::Tz; 2] = [chrono_tz::Tz::Asia__Tokyo, chrono_tz::Tz::Japan];

impl Lang {
    /// 実行環境から決める。
    pub fn from_env() -> Self {
        Self::resolved_from_env().lang
    }

    /// 実行環境から決め、出所も返す。
    pub fn resolved_from_env() -> ResolvedLang {
        Self::resolve(&|k| std::env::var(k).ok(), local_is_japan)
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
    ) -> ResolvedLang {
        if let Some(lang) = env("RESARCH_LANG").as_deref().and_then(Self::parse) {
            return ResolvedLang {
                lang,
                source: LangSource::AppEnv,
            };
        }

        // POSIX のロケール。空でない最初の 1 つだけを見る。
        let locale = LOCALE_VARS
            .iter()
            .filter_map(|k| env(k))
            .map(|v| v.trim().to_string())
            .find(|v| !v.is_empty());

        if let Some(locale) = locale.as_deref() {
            // `C` 系は「決定的な英語がほしい」の意思表示。
            // **`LANGUAGE` に上書きさせない** (gettext もこの順で扱う)。
            if is_c_locale(locale) {
                return ResolvedLang {
                    lang: Self::En,
                    source: LangSource::Locale,
                };
            }
        }

        // GNU の `LANGUAGE` は「メッセージに使いたい言語」で、`LANG` より目的に合う。
        // `fr:ja` のように候補を並べられるので、対応している先頭を採る。
        if let Some(list) = env("LANGUAGE")
            && let Some(lang) = list.split(':').filter_map(Self::parse).next()
        {
            return ResolvedLang {
                lang,
                source: LangSource::GnuLanguage,
            };
        }

        if let Some(locale) = locale.as_deref() {
            // **ここで判定を打ち切る。** ロケールが設定されているなら、
            // それが日本語でなくても「日本語ではない」という情報である。
            // `fr_FR` の環境でタイムゾーンを理由に日本語へ倒してはいけない。
            return ResolvedLang {
                lang: Self::parse(locale).unwrap_or(Self::En),
                source: LangSource::Locale,
            };
        }

        if local_is_japan() {
            return ResolvedLang {
                lang: Self::Ja,
                source: LangSource::TimeZone,
            };
        }
        ResolvedLang {
            lang: Self::En,
            source: LangSource::Default,
        }
    }

    /// 言語タグを解釈する。
    ///
    /// `ja_JP.UTF-8` / `ja-JP` / `ja` / `japanese` を日本語とみなす。
    /// 知らない言語は [`None`] を返し、呼び手が既定へ倒す。
    /// `C` / `POSIX` / `C.UTF-8` は「ロケールを使わない」の意味なので英語。
    pub fn parse(tag: &str) -> Option<Self> {
        let tag = tag.trim();
        if tag.is_empty() {
            return None;
        }
        if is_c_locale(tag) {
            return Some(Self::En);
        }
        // `ja_JP.UTF-8@calendar=japanese` の先頭要素だけを見る
        let primary = tag.split(['_', '-', '.', '@']).next()?;
        match primary.to_ascii_lowercase().as_str() {
            "ja" | "jpn" | "japanese" => Some(Self::Ja),
            "en" | "eng" | "english" => Some(Self::En),
            _ => None,
        }
    }

    /// `--lang` / `RESARCH_LANG` の値を解釈する。
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

/// `C` / `POSIX` / `C.UTF-8` か。
fn is_c_locale(tag: &str) -> bool {
    let base = tag.split(['.', '@']).next().unwrap_or(tag);
    base.eq_ignore_ascii_case("C") || base.eq_ignore_ascii_case("POSIX")
}

/// ローカルタイムゾーンが日本か。
fn local_is_japan() -> bool {
    matches!(DisplayTz::local(), DisplayTz::Named(tz) if JAPAN_ZONES.contains(&tz))
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

    fn resolve(pairs: &[(&str, &str)], japan: bool) -> ResolvedLang {
        Lang::resolve(&env_of(pairs), || japan)
    }

    #[test]
    fn the_app_specific_variable_wins() {
        let r = resolve(&[("RESARCH_LANG", "ja"), ("LC_ALL", "en_US.UTF-8")], false);
        assert_eq!(r.lang, Lang::Ja);
        assert_eq!(r.source, LangSource::AppEnv);
    }

    #[test]
    fn locale_variables_follow_the_posix_order() {
        let r = resolve(&[("LC_ALL", "ja_JP.UTF-8"), ("LANG", "en_US.UTF-8")], false);
        assert_eq!(r.lang, Lang::Ja);

        let r = resolve(
            &[("LC_MESSAGES", "en_US.UTF-8"), ("LANG", "ja_JP.UTF-8")],
            true,
        );
        assert_eq!(r.lang, Lang::En);
    }

    /// **ロケールはタイムゾーンより強い。**
    ///
    /// 日本にいながら英語環境で使う人の指定を、位置で上書きしない。
    #[test]
    fn an_explicit_locale_beats_the_timezone() {
        let r = resolve(&[("LANG", "en_US.UTF-8")], true);
        assert_eq!(r.lang, Lang::En);
        assert_eq!(r.source, LangSource::Locale);
    }

    /// 知らない言語のロケールでも「日本語ではない」は分かる。
    #[test]
    fn an_unknown_locale_does_not_fall_through_to_the_timezone() {
        let r = resolve(&[("LANG", "fr_FR.UTF-8")], true);
        assert_eq!(r.lang, Lang::En);
    }

    /// `LANGUAGE` は「メッセージに使いたい言語」なので `LANG` より優先する。
    #[test]
    fn the_gnu_language_variable_selects_the_message_language() {
        let r = resolve(&[("LANG", "de_DE.UTF-8"), ("LANGUAGE", "ja:en")], false);
        assert_eq!(r.lang, Lang::Ja);
        assert_eq!(r.source, LangSource::GnuLanguage);

        // 対応していない言語は読み飛ばして次の候補を見る
        let r = resolve(&[("LANGUAGE", "fr:ja")], false);
        assert_eq!(r.lang, Lang::Ja);
    }

    /// **`C` ロケールは `LANGUAGE` より強い。**
    ///
    /// スクリプトが `LC_ALL=C` で決定的な英語出力を得る慣行を壊さない。
    #[test]
    fn the_c_locale_is_not_overridden_by_the_language_variable() {
        let r = resolve(&[("LC_ALL", "C"), ("LANGUAGE", "ja")], true);
        assert_eq!(r.lang, Lang::En);
        assert_eq!(r.source, LangSource::Locale);
    }

    /// ロケールが無いときだけタイムゾーンを見る。
    #[test]
    fn the_timezone_decides_when_no_locale_is_set() {
        let r = resolve(&[], true);
        assert_eq!(r.lang, Lang::Ja);
        assert_eq!(r.source, LangSource::TimeZone);

        let r = resolve(&[], false);
        assert_eq!(r.lang, Lang::En);
        assert_eq!(r.source, LangSource::Default);
    }

    /// 空文字の `LANG` は「設定されていない」と同じに扱う。
    #[test]
    fn an_empty_locale_is_skipped() {
        let r = resolve(&[("LC_ALL", ""), ("LANG", "ja_JP.UTF-8")], false);
        assert_eq!(r.lang, Lang::Ja);
    }

    #[test]
    fn the_c_locale_means_english() {
        for tag in ["C", "POSIX", "c", "C.UTF-8", "POSIX.UTF-8"] {
            let r = resolve(&[("LANG", tag)], true);
            assert_eq!(r.lang, Lang::En, "{tag}");
        }
    }

    #[test]
    fn tags_are_parsed_case_insensitively() {
        for tag in [
            "ja",
            "JA",
            "ja_JP",
            "ja-JP",
            "ja_JP.UTF-8",
            "japanese",
            "ja_JP@calendar=japanese",
        ] {
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

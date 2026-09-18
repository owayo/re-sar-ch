//! 表示文字列の言語対。
//!
//! # なぜ「対」で持つのか
//!
//! 日本語の配列と英語の配列を別々に置くと、**件数と順序がずれる**。
//! ずれても型は通り、実行して読むまで気付かない。1 件を 1 つの値にまとめ、
//! 両方の言語を同時に要求すれば、片方だけ足した時点でコンパイルが止まる。
//!
//! ```
//! # use re_sar_ch::model::{Lang, Text};
//! const MARK: Text = Text::new("調査", "Investigate");
//! assert_eq!(MARK.get(Lang::Ja), "調査");
//! assert_eq!(MARK.get(Lang::En), "Investigate");
//! ```
//!
//! # 文を切り貼りしない
//!
//! 「`{指標}` が `{方向}` へ離れた」のような文を、語ごとの [`Text`] を
//! 連結して作ってはいけない。助詞の有無・語順・冠詞・複数形が言語で違うので、
//! **意味のまとまり 1 文をそれぞれの言語で書く**。数値を埋める文は
//! `analyze` 層のメッセージ型が言語ごとの `format!` で組み立てる。

use crate::model::Lang;

/// 同じ内容の日本語と英語。
///
/// **両方必須。** 片方だけの値は作れない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Text {
    ja: &'static str,
    en: &'static str,
}

impl Text {
    /// 日本語と英語から作る。
    pub const fn new(ja: &'static str, en: &'static str) -> Self {
        Self { ja, en }
    }

    /// 言語を選んで取り出す。
    pub const fn get(self, lang: Lang) -> &'static str {
        match lang {
            Lang::Ja => self.ja,
            Lang::En => self.en,
        }
    }

    /// 言語に依らず同じ文字列 (記号・識別子・単位記号)。
    ///
    /// **訳し忘れの受け皿にしない。** `%` や `MiB/s`、`A_CPU/all/idle` のように
    /// **訳すと誤りになる**ものだけに使う。
    pub const fn symbol(s: &'static str) -> Self {
        Self { ja: s, en: s }
    }
}

/// 日本語と英語を並べて [`Text`] を作る。
///
/// `Text::new` と同じだが、呼び出し側でどちらがどの言語か読み取れる。
#[macro_export]
macro_rules! text {
    (ja: $ja:expr, en: $en:expr $(,)?) => {
        $crate::model::Text::new($ja, $en)
    };
}

/// 英語の数え上げ (`1 sample` / `3 samples` / `0 samples`)。
///
/// **日本語側では使わない。** 日本語に数による語形変化は無く、
/// 助数詞は名詞ごとに決まる (「3 採取」「3 系列」) ので、
/// 数と語を組み立てる規則そのものが言語で違う。
///
/// 不規則な複数形 (`stretch` → `stretches`) があるので、
/// **両方の形を呼び出し側が渡す**。`-s` を付けるだけの実装にすると、
/// 名詞を足したときに静かに壊れる。
pub fn count_en(n: u64, singular: &str, plural: &str) -> String {
    if n == 1 {
        format!("{n} {singular}")
    } else {
        format!("{n} {plural}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counting_follows_the_number() {
        assert_eq!(count_en(0, "sample", "samples"), "0 samples");
        assert_eq!(count_en(1, "sample", "samples"), "1 sample");
        assert_eq!(count_en(2, "sample", "samples"), "2 samples");
        // 不規則な複数形も呼び出し側が渡せる
        assert_eq!(count_en(1, "stretch", "stretches"), "1 stretch");
        assert_eq!(count_en(3, "stretch", "stretches"), "3 stretches");
    }

    #[test]
    fn a_pair_carries_both_languages() {
        let t = text!(ja: "調査", en: "Investigate");
        assert_eq!(t.get(Lang::Ja), "調査");
        assert_eq!(t.get(Lang::En), "Investigate");
    }

    #[test]
    fn a_symbol_is_the_same_in_both() {
        let t = Text::symbol("A_CPU/all/idle");
        assert_eq!(t.get(Lang::Ja), t.get(Lang::En));
    }

    /// `const` 文脈で使える (静的なカタログに置くため)。
    #[test]
    fn pairs_work_in_const_context() {
        const T: Text = Text::new("秒", "s");
        const JA: &str = T.get(Lang::Ja);
        assert_eq!(JA, "秒");
    }
}

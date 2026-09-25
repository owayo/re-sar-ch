//! 本家 sysstat が参照する環境変数のうち、**互換出力の書式を変えるもの**。
//!
//! `sar` は表示の一部をロケールと端末から決める。reSARch は互換出力で
//! 本家の書式をそのまま出すので、**その決め方も本家に合わせる**必要がある。
//!
//! # 扱う変数
//!
//! | 変数 | 効く先 | 出典 |
//! |---|---|---|
//! | `S_TIME_FORMAT` | `sar` のバナー行の日付 | 03 §6.1 |
//! | `S_REPEAT_HEADER` | `sar` の列見出しの再表示間隔 | 03 §5.5 / §9.3 |
//!
//! **`sadf` には効かない。** `sadf` は `S_F_PREFD_TIME_OUTPUT` を立てず、
//! 日付は常に `%Y-%m-%d`、時刻は常に `%H:%M:%S` を出す (03 §6.1 の組み合わせ表)。
//! 「互換出力 6 形式は同じ値を出す」という規律の「値」は統計量のことであり、
//! 日時の字句表現やヘッダ再表示まで揃えるという意味ではない。
//!
//! # 読み取りと解決を分ける理由
//!
//! 解決は環境変数と端末高を**引数で受ける純粋関数**にしてある。
//! [`crate::model::lang`] と同じ理由で、`std::env::set_var` は edition 2024 で
//! `unsafe` であり、並行実行するテストの間で状態が漏れるためである。
//! 端末への `ioctl` も同様にテストから切り離したい。
//!
//! 実際の環境を読むのは CLI 層 (`main`) の 1 箇所だけで、
//! 出力層へは**解決済みの値**を [`crate::output::sar_text::SarTextOptions`]
//! に載せて渡す。出力層が環境を直接見ると、同じ入力・同じオプションでも
//! 出力が変わる隠れた依存になり、ライブラリとして呼んだときに再現しない。

/// `S_TIME_FORMAT`。
const S_TIME_FORMAT: &str = "S_TIME_FORMAT";

/// `S_TIME_FORMAT` が ISO 8601 を要求する値 (本家の `K_ISO`)。
///
/// 本家は `strcmp(e, K_ISO)` で比較する。**大文字小文字は区別され、
/// `ISO8601` のような別綴りは受け付けない**ので、ここでも正規化しない。
const K_ISO: &str = "ISO";

/// `S_REPEAT_HEADER`。
const S_REPEAT_HEADER: &str = "S_REPEAT_HEADER";

/// ヘッダを再表示しないときの行数 (本家の `DEFAULT_ROWS = SEC_PER_DAY`)。
///
/// 「無効」ではなく**非常に大きな閾値**である。本家もこの値を番兵ではなく
/// 実際の比較対象として使うので、そのまま持つ。
pub const DEFAULT_ROWS: u32 = 86_400;

/// 本家の `MIN_ROWS`。
const MIN_ROWS: u32 = 1;

/// 本家 (Linux の sysstat) の C の `long`。
///
/// `std::ffi::c_long` は**実行するホストの** C の `long` で、64bit の Windows
/// (LLP64) では 32bit になる。本家は Linux でしか動かず、Linux の ABI
/// (LP64 / ILP32 / x32) では `long` がどれもポインタと同じ幅になる。
/// そこでホストの ABI ではなくポインタ幅 (`isize`) で写し、64bit の Windows でも
/// 64bit の Linux で動く本家と同じ値にする。
type SysstatLong = isize;

/// `sar` のバナー行に出す日付の書式 (03 §6.1)。
///
/// | `S_TIME_FORMAT` | バナーの日付 | 各行のタイムスタンプ |
/// |---|---|---|
/// | 未設定 / `ISO` 以外 | `%x` (ロケール依存) | `%X` (ロケール依存) |
/// | `ISO` | `%Y-%m-%d` | `%H:%M:%S` |
///
/// reSARch はロケールを持たない (C ロケール相当で固定) ため、
/// **時刻側はどちらでも `%H:%M:%S`** になり差が出ない。
/// 実際に変わるのはバナー行の日付だけである。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CompatDateFormat {
    /// `%x`。C ロケールでは `MM/DD/YY`。
    #[default]
    Locale,
    /// `S_TIME_FORMAT=ISO` — `%Y-%m-%d`。
    Iso,
}

impl CompatDateFormat {
    /// 実行環境から決める。
    pub fn from_env() -> Self {
        Self::resolve(&|k| std::env::var(k).ok())
    }

    /// 判定の本体。環境変数の読み取りを引数で受ける (モジュール冒頭の理由)。
    fn resolve(env: &dyn Fn(&str) -> Option<String>) -> Self {
        match env(S_TIME_FORMAT).as_deref() {
            Some(K_ISO) => Self::Iso,
            _ => Self::Locale,
        }
    }
}

/// 列見出しを再表示するまでの行数。
///
/// `u32` を直に持たないのは、`Default::default()` が `0` になると
/// **毎サンプル再表示**(`lines >= 0` が常に真)に化けるため。
/// 既定は [`DEFAULT_ROWS`] = パイプ出力の挙動 (実質繰り返さない) にする。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeaderRows(u32);

impl HeaderRows {
    /// `rows` は本家と同じく [`MIN_ROWS`] で下限を取る
    /// (`return (rows < MIN_ROWS) ? MIN_ROWS : rows;`)。
    ///
    /// 本家の `rows` は `int` で、`S_REPEAT_HEADER` の桁あふれで負にもなる。
    pub fn new(rows: i32) -> Self {
        Self(u32::try_from(rows).unwrap_or(0).max(MIN_ROWS))
    }

    /// 再表示までの行数。
    pub fn get(self) -> u32 {
        self.0
    }
}

impl Default for HeaderRows {
    fn default() -> Self {
        Self(DEFAULT_ROWS)
    }
}

/// 列見出しを再表示するまでの行数を決める (`get_win_height()`、03 §9.3)。
///
/// 本家の手順をそのまま写したもの:
///
/// ```text
/// rows = DEFAULT_ROWS
/// if ioctl(STDOUT_FILENO, TIOCGWINSZ) が成功 {
///     if ws_row > 2 { rows = ws_row - 2 }
/// } else if S_REPEAT_HEADER が全桁数字かつ > 0 {
///     rows = その値
/// }
/// return max(rows, MIN_ROWS)
/// ```
///
/// **`S_REPEAT_HEADER` は ioctl が失敗したとき (= stdout が端末でない) しか
/// 見られない** (本家が `else if` のため)。端末で `ws_row <= 2` のときは
/// `S_REPEAT_HEADER` も見ずに [`DEFAULT_ROWS`] になる。
///
/// `terminal_rows` は「stdout が端末で、かつ寸法が取れたときだけ `Some`」。
pub fn header_rows_from_env(terminal_rows: Option<u16>) -> HeaderRows {
    resolve_header_rows(terminal_rows, &|k| std::env::var(k).ok())
}

/// sysstat 10.1.5 (el7) の `get_win_height()`。
///
/// ```text
/// rows = 3600 * 24
/// if ioctl(STDOUT_FILENO, TIOCGWINSZ) が成功 && ws_row > 2 { rows = ws_row - 2 }
/// return rows
/// ```
///
/// 現行版との違いは 2 つ。**`S_REPEAT_HEADER` を読まない** (この変数は後の版で
/// 入った) ことと、`MIN_ROWS` の下限処理が無いこと (`ws_row > 2` のときしか
/// 書き換えないので 1 未満にはならない)。環境変数を読まないので純粋関数になる。
pub fn header_rows_el7(terminal_rows: Option<u16>) -> HeaderRows {
    match terminal_rows {
        Some(ws_row) if ws_row > 2 => HeaderRows::new(i32::from(ws_row) - 2),
        _ => HeaderRows::default(),
    }
}

/// [`header_rows_from_env`] の本体。
///
/// 本家は `rows` を `int` で持ち、最後に `MIN_ROWS` で下限を取る。
/// 途中の値が負になり得る (下の [`parse_repeat_header`]) ので、
/// ここも `i32` のまま計算して最後に丸める。
fn resolve_header_rows(
    terminal_rows: Option<u16>,
    env: &dyn Fn(&str) -> Option<String>,
) -> HeaderRows {
    let mut rows = DEFAULT_ROWS as i32;
    match terminal_rows {
        // ioctl 成功。`ws_row <= 2` では `S_REPEAT_HEADER` を見ずに既定値のまま。
        Some(ws_row) => {
            if ws_row > 2 {
                rows = i32::from(ws_row) - 2;
            }
        }
        // ioctl 失敗 (パイプ / ファイル) のときだけ環境変数を見る。
        None => {
            if let Some(value) = parse_repeat_header(env(S_REPEAT_HEADER).as_deref()) {
                rows = value;
            }
        }
    }
    HeaderRows::new(rows)
}

/// `S_REPEAT_HEADER` の値を本家 `get_win_height()` と同じ手順で解釈する。
///
/// ```c
/// if (strspn(e, DIGITS) == strlen(e)) {
///     long v = strtol(e, &endptr, 10);
///     if ((v > 0) && (*endptr == '\0')) { rows = (int) v; }
/// }
/// ```
///
/// **符号・空白・空文字はすべて不可。** 条件を外れた指定は無視される
/// (エラーにはならない)。
///
/// **桁あふれは「無効」ではなく `int` への切り詰め**である。本家は `strtol` の
/// 結果を範囲検査せずに `(int)` へキャストするので、LP64 では
/// `2147483648` が負値に化けて最終的に `MIN_ROWS` になり、
/// `4294967297` は `1` になる。`u32` で弾くと本家と食い違う。
fn parse_repeat_header(value: Option<&str>) -> Option<i32> {
    let value = value?;
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    // `strtol` の受け皿は `long`。**幅は ABI で変わる**ので本家の `long` の幅
    // ([`SysstatLong`]) で受ける。LP64 では 64bit、ILP32 では 32bit になり、
    // 飽和する閾値が本家と揃う。あふれたら `LONG_MAX` に張り付く (本家は errno を見ない)。
    let parsed = value.parse::<SysstatLong>().unwrap_or(SysstatLong::MAX);
    if parsed <= 0 {
        return None;
    }
    // `rows = (int) v` の切り詰め。2 の補数環境の挙動をそのまま写す。
    Some(parsed as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// テスト用の環境。存在しないキーは `None`。
    fn env(pairs: &[(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |key| {
            pairs
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn iso_is_matched_exactly() {
        assert_eq!(
            CompatDateFormat::resolve(&env(&[("S_TIME_FORMAT", "ISO")])),
            CompatDateFormat::Iso
        );
        // 本家は strcmp なので、綴りが違えばすべてロケール側に落ちる。
        for value in ["iso", "Iso", "ISO8601", "ISO ", " ISO", "", "1"] {
            assert_eq!(
                CompatDateFormat::resolve(&env(&[("S_TIME_FORMAT", value)])),
                CompatDateFormat::Locale,
                "S_TIME_FORMAT={value:?}"
            );
        }
        assert_eq!(
            CompatDateFormat::resolve(&env(&[])),
            CompatDateFormat::Locale
        );
    }

    #[test]
    fn terminal_height_wins_over_the_environment() {
        let with_env = env(&[("S_REPEAT_HEADER", "20")]);
        let rows = |term| resolve_header_rows(term, &with_env).get();
        // 端末高が取れたら `ws_row - 2`。環境変数は見ない。
        assert_eq!(rows(Some(24)), 22);
        // `ws_row - 2` が MIN_ROWS を下回らない境界。
        assert_eq!(rows(Some(3)), 1);
        // `ws_row <= 2` は既定値。**環境変数へ落ちない** (本家が else if のため)。
        assert_eq!(rows(Some(2)), DEFAULT_ROWS);
        assert_eq!(rows(Some(0)), DEFAULT_ROWS);
    }

    #[test]
    fn repeat_header_is_read_only_without_a_terminal() {
        let rows =
            |pairs: &[(&'static str, &'static str)]| resolve_header_rows(None, &env(pairs)).get();
        assert_eq!(rows(&[("S_REPEAT_HEADER", "20")]), 20);
        assert_eq!(rows(&[]), DEFAULT_ROWS);
        // 全桁数字かつ > 0 以外はすべて無視される。
        for value in ["0", "-1", "+1", " 20", "20 ", "20a", "a20", "", "0x14"] {
            assert_eq!(
                rows(&[("S_REPEAT_HEADER", value)]),
                DEFAULT_ROWS,
                "S_REPEAT_HEADER={value:?}"
            );
        }
        assert_eq!(rows(&[("S_REPEAT_HEADER", "1")]), 1);
    }

    /// 桁あふれは「無効」ではなく `int` への切り詰め (本家 `(int) strtol(...)`)。
    ///
    /// 期待値は本家の `long` の幅で分かれる。分岐をポインタ幅で書くのは、
    /// 64bit の Windows (C の `long` は 32bit) でも 64bit の Linux の本家と
    /// 同じ値になることを、Windows の CI で確かめるため。
    #[test]
    fn repeat_header_overflow_wraps_like_the_c_cast() {
        let rows = |value: &'static str| {
            resolve_header_rows(None, &env(&[("S_REPEAT_HEADER", value)])).get()
        };
        // int に収まる最大値はそのまま。
        assert_eq!(rows("2147483647"), 2_147_483_647);
        if cfg!(target_pointer_width = "64") {
            // LP64: long に収まった値を (int) で下位 32bit に切り詰める。
            // 2^31 は int で負値 → MIN_ROWS へ丸められる。
            assert_eq!(rows("2147483648"), MIN_ROWS);
            // 2^32 + 1 は下位 32bit が 1。
            assert_eq!(rows("4294967297"), 1);
            // 2^32 は下位 32bit が 0 → MIN_ROWS。
            assert_eq!(rows("4294967296"), MIN_ROWS);
            // long を超える桁は strtol が LONG_MAX で頭打ちになり、
            // `(int) LONG_MAX` = -1 → MIN_ROWS。
            assert_eq!(rows("99999999999999999999999"), MIN_ROWS);
        } else {
            // ILP32: long も 32bit なので、2^31 以上は strtol が LONG_MAX (= INT_MAX) で
            // 頭打ちになり、そのまま残る。
            for value in [
                "2147483648",
                "4294967296",
                "4294967297",
                "99999999999999999999999",
            ] {
                assert_eq!(rows(value), i32::MAX as u32, "S_REPEAT_HEADER={value}");
            }
        }
    }

    #[test]
    fn default_rows_never_repeat() {
        // `Default` が 0 に落ちると毎サンプル再表示に化けるので、ここで固定する。
        assert_eq!(HeaderRows::default().get(), DEFAULT_ROWS);
        assert_eq!(HeaderRows::new(0).get(), MIN_ROWS);
        assert_eq!(HeaderRows::new(-1).get(), MIN_ROWS);
    }

    /// el7 (10.1.5) は `S_REPEAT_HEADER` を持たず、端末の高さだけで決まる。
    #[test]
    fn el7_rows_depend_only_on_the_terminal() {
        assert_eq!(header_rows_el7(None).get(), DEFAULT_ROWS);
        assert_eq!(header_rows_el7(Some(40)).get(), 38);
        // `ws_row > 2` でなければ書き換えない
        assert_eq!(header_rows_el7(Some(2)).get(), DEFAULT_ROWS);
        assert_eq!(header_rows_el7(Some(3)).get(), 1);
    }
}

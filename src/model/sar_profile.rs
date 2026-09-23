//! `sar` テキストで**どの版の本家 `sar` を再現するか** (出力仕様のプロファイル)。
//!
//! 「読むファイルの世代」と「再現する出力の世代」は別の設定である
//! (`docs/design.md` §9.1)。v10 のファイルを現行 (v12.8.0) の書式で出すのが既定で、
//! プロファイルを指定すると、そのファイルを書いた側の `sar` が出していたはずの
//! テキストを再現する。
//!
//! # 名前を版番号だけにしない理由
//!
//! 同じ `10.1.5` でも、配布元のパッチで出力が変わる。RHEL / CentOS 7 の
//! `sysstat-10.1.5-*.el7` は `NR_CPUS` を 8192 に上げるパッチを当てており、
//! これが `sar -A` の列見出しの再表示間隔 (1 サンプル = `count_bits(cpu_bitmap)` 行)
//! を upstream の 10.1.5 から変える。ファイルシステム統計 (`-F`) のバックポートや、
//! カウンタ逆行を 0 にする `dyn-tick` パッチも出力に効く。
//! したがって**検証した配布物の単位で名乗る** (`sysstat-10.1.5-el7`)。
//! `10.1.5` のような省略形は、どれを指すか決まらないので受け付けない。
//!
//! # 自動判定をしない理由
//!
//! ファイルヘッダに記録された版は「sa ファイルを書いた `sadc` の版」であり、
//! その後に `sa2` が使った `sar` の版とも、配布元のパッチとも一致する保証がない。
//! ページサイズ (`-R` の換算に使う) もファイルからは分からない。
//! 推定で出力の書式と計算を切り替えると、同じファイルの既存の出力が
//! 利用者の知らないうちに変わるので、プロファイルは常に明示指定にする。

use std::fmt;
use std::str::FromStr;

/// 再現する `sar` の出力仕様。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SarProfile {
    /// 本家 sysstat v12.8.0 (既定。`current` はこの別名)。
    #[default]
    Sysstat1280,
    /// RHEL / CentOS 7 の sysstat 10.1.5 (`sysstat-10.1.5-17.el7` 〜 `-20.el7_9`)。
    ///
    /// 読めるのは本家 10.1.5 と同じく `format_magic` 0x2171 のファイルだけ。
    Sysstat1015El7,
}

impl SarProfile {
    /// 受け付ける名前 (エラー表示と `--help` に使う)。
    pub const NAMES: &'static [&'static str] = &["current", "sysstat-12.8.0", "sysstat-10.1.5-el7"];

    /// 正式名。
    pub fn name(self) -> &'static str {
        match self {
            SarProfile::Sysstat1280 => "sysstat-12.8.0",
            SarProfile::Sysstat1015El7 => "sysstat-10.1.5-el7",
        }
    }

    /// ページサイズ (`--sar-page-size`) を使うプロファイルか。
    ///
    /// 10.1.5 の `sar -R` はページ数の変化を出すため、kB をページへ直す
    /// `KB_TO_PG` に**`sar` を実行したホストの**ページサイズを使う。
    /// 現行版は `-R` を廃止しているので、指定しても効かない。
    pub fn uses_page_size(self) -> bool {
        matches!(self, SarProfile::Sysstat1015El7)
    }
}

impl fmt::Display for SarProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// プロファイル名の解析エラー。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "--sar-profile に使えない値です: {value} (使える値: {})",
    SarProfile::NAMES.join(" / ")
)]
pub struct ParseSarProfileError {
    /// 指定された値。
    pub value: String,
}

impl FromStr for SarProfile {
    type Err = ParseSarProfileError;

    /// 完全一致だけを受け付ける (大文字小文字も区別する)。
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "current" | "sysstat-12.8.0" => Ok(SarProfile::Sysstat1280),
            "sysstat-10.1.5-el7" => Ok(SarProfile::Sysstat1015El7),
            _ => Err(ParseSarProfileError {
                value: s.to_string(),
            }),
        }
    }
}

/// 10.1.5 の `sar -R` が kB をページへ直すときのページサイズ。
///
/// 本家は `get_kb_shift()` で `sysconf(_SC_PAGESIZE)` から
/// `kb_shift` (= log2(ページサイズ / 1024)) を求め、`KB_TO_PG(k) = k >> kb_shift`
/// で換算する。**ファイルには記録されない**ので、再現したいホストの値を指定する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageSize(u64);

impl PageSize {
    /// 既定値。RHEL / CentOS 7 の x86_64 (と s390x) のページサイズ。
    pub const DEFAULT: PageSize = PageSize(4096);

    /// バイト数から作る。1024 以上の 2 の冪だけを受け付ける
    /// (`kb_shift` が整数のシフト量として表せる値だけが本家でも意味を持つ)。
    pub fn new(bytes: u64) -> Option<Self> {
        (bytes >= 1024 && bytes.is_power_of_two()).then_some(PageSize(bytes))
    }

    /// バイト数。
    pub fn bytes(self) -> u64 {
        self.0
    }

    /// 本家の `kb_shift`。
    ///
    /// ```c
    /// size >>= 10;  /* 1 kB を最小とみなす */
    /// while (size > 1) { shift++; size >>= 1; }
    /// ```
    pub fn kb_shift(self) -> u32 {
        (self.0 >> 10).trailing_zeros()
    }
}

impl Default for PageSize {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// `--sar-page-size` の解析エラー。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "--sar-page-size に使えない値です: {value} (1024 以上の 2 の冪のバイト数を指定してください。例: 4096 / 65536)"
)]
pub struct ParsePageSizeError {
    /// 指定された値。
    pub value: String,
}

impl FromStr for PageSize {
    type Err = ParsePageSizeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let err = || ParsePageSizeError {
            value: s.to_string(),
        };
        if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
            return Err(err());
        }
        s.parse::<u64>()
            .ok()
            .and_then(PageSize::new)
            .ok_or_else(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_names_round_trip() {
        for name in SarProfile::NAMES {
            let p: SarProfile = name.parse().unwrap();
            // `current` は正式名 `sysstat-12.8.0` の別名
            if *name != "current" {
                assert_eq!(p.name(), *name);
            }
        }
        assert_eq!(
            "current".parse::<SarProfile>().unwrap(),
            SarProfile::Sysstat1280
        );
        assert_eq!(SarProfile::default(), SarProfile::Sysstat1280);
    }

    /// 省略形・大文字違い・別綴りは受け付けない (どれを指すか決まらないため)。
    #[test]
    fn ambiguous_profile_names_are_rejected() {
        for bad in [
            "10.1.5",
            "el7",
            "rhel7",
            "SYSSTAT-10.1.5-EL7",
            "sysstat-10.1.5",
            "",
        ] {
            let err = bad.parse::<SarProfile>().unwrap_err();
            assert_eq!(err.value, bad);
            assert!(err.to_string().contains("sysstat-10.1.5-el7"), "{err}");
        }
    }

    #[test]
    fn page_size_maps_to_kb_shift() {
        assert_eq!(PageSize::DEFAULT.kb_shift(), 2);
        assert_eq!("4096".parse::<PageSize>().unwrap().kb_shift(), 2);
        assert_eq!("65536".parse::<PageSize>().unwrap().kb_shift(), 6);
        assert_eq!("1024".parse::<PageSize>().unwrap().kb_shift(), 0);
    }

    #[test]
    fn page_size_rejects_values_that_are_not_a_shift() {
        for bad in ["0", "512", "4095", "5000", "-4096", "4k", "", " 4096"] {
            assert!(
                bad.parse::<PageSize>().is_err(),
                "{bad} を受け付けてはいけない"
            );
        }
    }
}

//! 値の表現。欠落とゼロを型で区別する。

use serde::Serialize;

/// フィールド値の在否。
///
/// `Option` を使わないのは、「値が無い」理由を区別する必要があるため。
/// 未提供と欠落を混同すると、集計や判定が静かに誤る。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "state", content = "value", rename_all = "snake_case")]
pub enum Availability<T> {
    /// 値が取得できた。0 も正常な値として扱う。
    Present(T),
    /// そのフォーマット世代のファイルには存在しないフィールド。
    UnsupportedBySource,
    /// フィールドは存在するが、当該レコードでは取得できていない。
    MissingInSample,
}

impl<T> Availability<T> {
    #[inline]
    pub fn is_present(&self) -> bool {
        matches!(self, Availability::Present(_))
    }

    #[inline]
    pub fn get(&self) -> Option<&T> {
        match self {
            Availability::Present(v) => Some(v),
            _ => None,
        }
    }

    #[inline]
    pub fn into_option(self) -> Option<T> {
        match self {
            Availability::Present(v) => Some(v),
            _ => None,
        }
    }

    pub fn map<U, F: FnOnce(T) -> U>(self, f: F) -> Availability<U> {
        match self {
            Availability::Present(v) => Availability::Present(f(v)),
            Availability::UnsupportedBySource => Availability::UnsupportedBySource,
            Availability::MissingInSample => Availability::MissingInSample,
        }
    }

    /// 診断・出力用の短いラベル。
    pub fn state_label(&self) -> &'static str {
        match self {
            Availability::Present(_) => "present",
            Availability::UnsupportedBySource => "unsupported_by_source",
            Availability::MissingInSample => "missing_in_sample",
        }
    }
}

impl<T> From<Option<T>> for Availability<T> {
    fn from(v: Option<T>) -> Self {
        match v {
            Some(v) => Availability::Present(v),
            None => Availability::MissingInSample,
        }
    }
}

/// カウンタの元のビット幅。ラップアラウンド判定に使う。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CounterBits {
    B32,
    B64,
}

impl CounterBits {
    /// この幅で表現できる値の個数 (ラップ 1 周分)。
    #[inline]
    pub const fn modulus(self) -> u128 {
        match self {
            CounterBits::B32 => 1u128 << 32,
            CounterBits::B64 => 1u128 << 64,
        }
    }

    /// ファイル上の有効バイト数から決める。
    #[inline]
    pub const fn from_value_width(width: usize) -> Self {
        if width <= 4 {
            CounterBits::B32
        } else {
            CounterBits::B64
        }
    }
}

/// 累積カウンタの生値。
///
/// **`f64` へ早期に変換しない。** 大きな値で差分の精度が失われる。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Counter {
    pub value: u64,
    pub bits: CounterBits,
}

impl Counter {
    #[inline]
    pub const fn new(value: u64, bits: CounterBits) -> Self {
        Self { value, bits }
    }

    #[inline]
    pub const fn b64(value: u64) -> Self {
        Self::new(value, CounterBits::B64)
    }

    #[inline]
    pub const fn b32(value: u64) -> Self {
        Self::new(value, CounterBits::B32)
    }
}

/// 値の性質。差分を取るべきか、そのまま使うべきかを決める。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ValueKind {
    /// 起動時からの累積値。2 点間の差分を経過時間で割ってレートにする。
    Counter,
    /// その時点の瞬時値。差分化しない。
    Gauge,
    /// デバイス名など、値ではなく同一性を表すもの。
    Identity,
}

/// 値の単位。出力時の書式と `--human` 変換の判断に使う。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    /// 無次元 (個数など、単位表記を持たない)
    None,
    /// 百分率
    Percent,
    /// 件数 (累積)
    Count,
    /// 件数/秒
    CountPerSec,
    /// バイト
    Bytes,
    /// バイト/秒
    BytesPerSec,
    /// キロバイト (sysstat が kB 単位で記録するフィールド)
    Kilobytes,
    /// キロバイト/秒
    KilobytesPerSec,
    /// セクタ (512 バイト単位)
    Sectors,
    /// セクタ/秒
    SectorsPerSec,
    /// ミリ秒
    Milliseconds,
    /// 1/100 秒 (sysstat の uptime_cs など)
    Centiseconds,
    /// マイクロ秒 (PSI の累積値など)
    Microseconds,
    /// ジフィ (CPU tick)
    Jiffies,
    /// メガヘルツ
    Megahertz,
    /// 摂氏
    Celsius,
    /// ボルト
    Volts,
    /// 毎分回転数
    Rpm,
    /// ミリアンペア時
    MilliampereHours,
    /// 識別子・名前 (単位を持たない文字列)
    Identifier,
}

impl Unit {
    /// `--human` によるスケーリング対象か。
    #[inline]
    pub const fn is_scalable(self) -> bool {
        matches!(
            self,
            Unit::Bytes
                | Unit::BytesPerSec
                | Unit::Kilobytes
                | Unit::KilobytesPerSec
                | Unit::Count
                | Unit::CountPerSec
        )
    }

    /// 出力に添える短い単位表記 (無い場合は空文字)。
    pub const fn suffix(self) -> &'static str {
        match self {
            Unit::Percent => "%",
            Unit::Bytes => "B",
            Unit::BytesPerSec => "B/s",
            Unit::Kilobytes => "kB",
            Unit::KilobytesPerSec => "kB/s",
            Unit::Milliseconds => "ms",
            Unit::Megahertz => "MHz",
            Unit::Celsius => "degC",
            Unit::Volts => "V",
            Unit::Rpm => "rpm",
            Unit::MilliampereHours => "mAh",
            _ => "",
        }
    }
}

/// 期間集計の方法。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Aggregation {
    /// 有効区間の差分合計 ÷ 有効区間の時間合計
    RateOverPeriod,
    /// 区間値の平均 (ゲージ)
    Mean,
    /// 最後の観測値をそのまま採る (sysstat の A_FS / A_PWR_USB 相当)
    Last,
    /// 合計
    Sum,
    /// 集計しない (識別子など)
    NotAggregated,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_is_distinct_from_missing() {
        let zero: Availability<u64> = Availability::Present(0);
        let unsupported: Availability<u64> = Availability::UnsupportedBySource;
        let missing: Availability<u64> = Availability::MissingInSample;

        assert!(zero.is_present());
        assert_eq!(zero.get(), Some(&0));
        assert!(!unsupported.is_present());
        assert!(!missing.is_present());
        assert_ne!(zero, unsupported);
        assert_ne!(unsupported, missing);
    }

    #[test]
    fn counter_bits_from_value_width() {
        assert_eq!(CounterBits::from_value_width(4), CounterBits::B32);
        assert_eq!(CounterBits::from_value_width(8), CounterBits::B64);
        assert_eq!(CounterBits::B32.modulus(), 1u128 << 32);
    }

    #[test]
    fn serializes_state_and_value() {
        let v: Availability<u64> = Availability::Present(42);
        let s = serde_json::to_string(&v).unwrap();
        assert!(s.contains("present"), "{s}");
        assert!(s.contains("42"), "{s}");

        let u: Availability<u64> = Availability::UnsupportedBySource;
        let s = serde_json::to_string(&u).unwrap();
        assert!(s.contains("unsupported_by_source"), "{s}");
    }

    #[test]
    fn human_scaling_applies_only_to_size_units() {
        assert!(Unit::Kilobytes.is_scalable());
        assert!(!Unit::Percent.is_scalable());
        assert!(!Unit::Celsius.is_scalable());
    }
}

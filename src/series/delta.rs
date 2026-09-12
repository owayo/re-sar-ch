//! カウンタ差分とレート計算。
//!
//! `sa` ファイルに入っている値の多くは起動時からの累積カウンタであり、
//! 表示値は 2 レコード間の差分を経過時間で割って得る。
//!
//! ## 本家との一致
//!
//! 本家の計算マクロは次の形をしている (経過時間 `p` は 1/100 秒単位)。
//!
//! ```text
//! S_VALUE(m, n, p)  = ((double)(n - m)) / p * 100
//! SP_VALUE(m, n, p) = ((double)(n - m)) / p * 100
//! ```
//!
//! `S_VALUE` と `SP_VALUE` は式が同一で、名前の違いは「毎秒のレート」と
//! 「パーセント」という**意図の差**しか表さない。
//!
//! 重要なのは **減算が符号なしのまま行われる**点である。
//! `curr as f64 - prev as f64` と書くとカウンタが逆行した場合に本家と値が食い違う。
//! ここでは `wrapping_sub` を使って本家の挙動をそのまま再現する。
//!
//! ## 減算の幅は「元のフィールド幅」で決まる
//!
//! レイアウト層はどのフィールドも `u64` へゼロ拡張して返す。しかし
//! **カウンタが一周する周期は元のフィールド幅**で決まるため、
//! 32bit 幅のカウンタを `u64` のまま引くと一周を復元できない。
//!
//! 例: `unsigned int` のカウンタが `4294967290 → 4` と一周した場合、
//! 正しい差分は 10 だが、`u64::wrapping_sub` では 18446744073709551620
//! (≈1.84×10¹⁹) になり、レートが 10¹⁸ 台の嘘の値として表示される。
//!
//! これは本家との一致の問題でもある。本家の統計構造体は
//! `unsigned long long` / `unsigned long` / `unsigned int` の 3 グループを持ち、
//! `unsigned int` のフィールドは C の整数変換規則により
//! **`S_VALUE` に渡る前に 32bit のまま減算**される (`unsigned int - unsigned int`
//! は `unsigned int`)。つまり本家は 32bit カウンタの一周を正しく扱っており、
//! 64bit で引いている reSARch 側だけが 10¹⁹ を出していた。
//! `int` グループのカウンタは `stats_serial` / `stats_net_sock` / `stats_softnet` /
//! `stats_disk` の tick 群 / NFS 系など多数ある (02 §7)。
//!
//! そのため差分関数を 2 系統持つ。
//!
//! | 関数 | 減算の幅 | 使う場面 |
//! |---|---|---|
//! | [`s_value`] / [`ll_sp_value`] | 常に 64bit | 本家が `unsigned long long` で引いている列 |
//! | [`s_value_bits`] / [`ll_sp_value_bits`] / [`wrapping_delta`] | 列の幅に従う | 列のカウンタ幅が分かる経路 (`DecodePlan::column_bits()`) |
//!
//! 列の幅が `B64` のとき両者は完全に同一の結果になるので、
//! 幅が分かる経路では常に `*_bits` 版を使ってよい。

use crate::model::CounterBits;

/// 経過時間 (1/100 秒単位) を求める。
///
/// 本家 `get_interval()` に対応する。0 になった場合は 1 に置き換える
/// (0 除算で `inf` / `NaN` を出さないための処置で、本家も同じ)。
#[inline]
pub fn interval_cs(prev_uptime_cs: u64, curr_uptime_cs: u64) -> u64 {
    let itv = curr_uptime_cs.wrapping_sub(prev_uptime_cs);
    if itv == 0 { 1 } else { itv }
}

/// 本家 `S_VALUE` / `SP_VALUE` 相当 (**64bit 減算**)。
///
/// 経過時間が 1/100 秒単位なので、100 倍して「毎秒あたり」にする。
///
/// 本家が `unsigned long long` のフィールドを引いている列専用。
/// 32bit 幅のカウンタに使うと一周を復元できないので、
/// 列の幅が分かる経路では [`s_value_bits`] を使う (モジュール冒頭の表)。
#[inline]
pub fn s_value(prev: u64, curr: u64, itv_cs: u64) -> f64 {
    s_value_bits(prev, curr, itv_cs, CounterBits::B64)
}

/// 本家 `ll_sp_value` 相当 (**64bit 減算**)。カウンタが逆行したら 0 を返す。
#[inline]
pub fn ll_sp_value(prev: u64, curr: u64, itv_cs: u64) -> f64 {
    ll_sp_value_bits(prev, curr, itv_cs, CounterBits::B64)
}

/// 元のフィールド幅で行う符号なし減算。
///
/// ファイルから読んだ値は `u64` へゼロ拡張されているが、カウンタが一周する
/// 周期は**元の幅**で決まる。32bit カウンタが `4294967290 → 4` と一周したとき、
/// 正しい差分は 10 なのに `u64::wrapping_sub` は 1.84×10¹⁹ を返す。
/// 下位 32bit だけを残せば、C の `unsigned int` 同士の減算と同じ値になる。
///
/// 幅が `B64` のときは `u64::wrapping_sub` と完全に同一。
#[inline]
pub fn wrapping_delta(prev: u64, curr: u64, bits: CounterBits) -> u64 {
    let raw = curr.wrapping_sub(prev);
    match bits {
        // 本家の `unsigned int` 減算 (mod 2^32) と同じ
        CounterBits::B32 => raw & u64::from(u32::MAX),
        CounterBits::B64 => raw,
    }
}

/// 幅を意識した `S_VALUE` / `SP_VALUE`。
///
/// 32bit 幅のカウンタでは一周を復元してからレート化する。
/// これは本家が `unsigned int` フィールドに対して行っている計算と一致する。
#[inline]
pub fn s_value_bits(prev: u64, curr: u64, itv_cs: u64, bits: CounterBits) -> f64 {
    wrapping_delta(prev, curr, bits) as f64 / itv_cs as f64 * 100.0
}

/// 幅を意識した `ll_sp_value`。
///
/// 逆行クランプは本家と同じく**元の値の比較**で行う (幅を意識した差分を
/// 取る前に判定する)。クランプ対象の列では一周と逆行を区別できないため、
/// 本家に合わせて 0 を返す方を選ぶ。
#[inline]
pub fn ll_sp_value_bits(prev: u64, curr: u64, itv_cs: u64, bits: CounterBits) -> f64 {
    if curr < prev {
        0.0
    } else {
        s_value_bits(prev, curr, itv_cs, bits)
    }
}

/// 2 点間の差分。
///
/// 本家は単純に符号なし減算するだけだが、独自出力や集計では
/// 「なぜ差分が取れないのか」を区別する必要があるため型で表す。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delta {
    /// 差分が取れた。
    Valid(u64),
    /// カウンタが一周したと判断できた。
    Wrapped(u64),
    /// 差分を計算できない。
    Unavailable(Discontinuity),
}

impl Delta {
    /// 差分値。計算できない場合は `None`。
    #[inline]
    pub fn value(self) -> Option<u64> {
        match self {
            Delta::Valid(v) | Delta::Wrapped(v) => Some(v),
            Delta::Unavailable(_) => None,
        }
    }

    /// 毎秒あたりのレート。
    #[inline]
    pub fn rate_per_sec(self, itv_cs: u64) -> Option<f64> {
        self.value().map(|d| d as f64 / itv_cs as f64 * 100.0)
    }
}

/// 差分が取れない理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Discontinuity {
    /// 基準となる前サンプルが無い (系列の先頭)。
    FirstSample,
    /// 間に RESTART レコードがある。
    Restart,
    /// item の同一性が崩れた (デバイス名の再利用、CPU のオンライン変化など)。
    ItemReplaced,
    /// 経過時間が 0 以下。
    NonPositiveElapsed,
    /// 値が減少したが、ラップとは断定できない。
    AmbiguousDecrease,
}

impl Discontinuity {
    pub fn as_str(self) -> &'static str {
        match self {
            Discontinuity::FirstSample => "first_sample",
            Discontinuity::Restart => "restart",
            Discontinuity::ItemReplaced => "item_replaced",
            Discontinuity::NonPositiveElapsed => "non_positive_elapsed",
            Discontinuity::AmbiguousDecrease => "ambiguous_decrease",
        }
    }
}

/// 差分計算の前提条件。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeltaContext {
    /// 前サンプルとの間に RESTART が無いこと。
    pub continuous: bool,
    /// item が同一であること。
    pub same_item: bool,
}

impl Default for DeltaContext {
    fn default() -> Self {
        Self {
            continuous: true,
            same_item: true,
        }
    }
}

/// 差分を判定付きで計算する。
///
/// カウンタの減少を一律にラップと解釈しない。32bit カウンタの一周として扱うのは、
/// 起動区間と item の連続性が確認できる場合に限る。
/// 長い観測間隔で複数回ラップした可能性は 2 点だけからは復元できないため、
/// 曖昧なケースは `AmbiguousDecrease` として報告する。
pub fn compute_delta(prev: u64, curr: u64, bits: CounterBits, ctx: DeltaContext) -> Delta {
    if !ctx.continuous {
        return Delta::Unavailable(Discontinuity::Restart);
    }
    if !ctx.same_item {
        return Delta::Unavailable(Discontinuity::ItemReplaced);
    }
    if curr >= prev {
        return Delta::Valid(curr - prev);
    }

    // 減少した場合
    match bits {
        CounterBits::B32 => {
            // 32bit カウンタの一周として解釈できるか。
            // 一周分を足して妥当な範囲に収まるなら採用する。
            let modulus = CounterBits::B32.modulus();
            let wrapped = modulus + curr as u128 - prev as u128;
            if wrapped < modulus {
                Delta::Wrapped(wrapped as u64)
            } else {
                Delta::Unavailable(Discontinuity::AmbiguousDecrease)
            }
        }
        // 64bit カウンタが一周するのは現実的でないため、減少は異常として扱う
        CounterBits::B64 => Delta::Unavailable(Discontinuity::AmbiguousDecrease),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interval_never_returns_zero() {
        assert_eq!(interval_cs(100, 200), 100);
        assert_eq!(interval_cs(100, 100), 1, "0 は 1 に置き換える");
    }

    /// 最初のサンプルでは前サンプルの uptime を 0 として扱うため、
    /// itv は起動からの経過時間になる。
    #[test]
    fn first_sample_interval_is_uptime_since_boot() {
        assert_eq!(interval_cs(0, 12_345), 12_345);
    }

    /// 経過時間が 1/100 秒単位なので、100 cs = 1 秒あたりの値になる。
    #[test]
    fn s_value_converts_cs_to_per_second() {
        // 1 秒 (100 cs) の間に 50 増えた → 50/s
        assert_eq!(s_value(100, 150, 100), 50.0);
        // 10 秒 (1000 cs) の間に 50 増えた → 5/s
        assert_eq!(s_value(100, 150, 1000), 5.0);
    }

    /// 減算は符号なしのまま行う。
    ///
    /// `curr as f64 - prev as f64` と書くと逆行時に負値になり、本家と食い違う。
    #[test]
    fn subtraction_wraps_like_upstream() {
        let prev = 10u64;
        let curr = 5u64;
        let v = s_value(prev, curr, 100);
        // 符号なし減算なので巨大な正値になる (本家と同じ)
        assert!(v > 0.0, "符号なし減算の結果は正: {v}");
        // 浮動小数で引いた場合との差を明示
        let naive = (curr as f64 - prev as f64) / 100.0 * 100.0;
        assert!(naive < 0.0);
        assert_ne!(v, naive);
    }

    /// `ll_sp_value` は逆行時に 0 を返す。
    #[test]
    fn ll_sp_value_clamps_decrease_to_zero() {
        assert_eq!(ll_sp_value(10, 5, 100), 0.0);
        assert_eq!(ll_sp_value(10, 20, 100), 10.0);
    }

    /// **回帰テスト (指摘 1)**: 32bit カウンタの一周を 64bit で引くと
    /// 1.84×10¹⁹ になる。幅を渡せば正しい差分 10 が出る。
    #[test]
    fn wrapping_delta_restores_32bit_wraparound() {
        let prev = 4_294_967_290u64; // u32::MAX - 5
        let curr = 4u64;

        assert_eq!(
            wrapping_delta(prev, curr, CounterBits::B32),
            10,
            "5 (上限まで) + 1 (0 へ) + 4 = 10"
        );
        // 64bit のまま引くと約 1.84e19 になる (これが修正前の値)
        let as_64 = wrapping_delta(prev, curr, CounterBits::B64);
        assert_eq!(as_64, u64::MAX - (prev - curr) + 1, "2^64 - 4294967286");
        assert!(as_64 as f64 > 1.0e19, "約 1.84e19: {as_64}");
    }

    /// 32bit 幅のカウンタのレートが巨大値にならないこと。
    #[test]
    fn s_value_bits_rate_is_sane_across_32bit_wrap() {
        let (prev, curr, itv) = (4_294_967_290u64, 4u64, 100u64);

        // 1 秒 (100 cs) の間に 10 増えた → 10/s
        assert_eq!(s_value_bits(prev, curr, itv, CounterBits::B32), 10.0);
        // 64bit 減算では 1.8e19/s という表示不能な値になる
        assert!(s_value(prev, curr, itv) > 1.0e19);
        // 逆行クランプ付きの経路では本家と同じく 0 (幅の復元より本家一致を採る)
        assert_eq!(ll_sp_value_bits(prev, curr, itv, CounterBits::B32), 0.0);
    }

    /// 一周していない通常の増加では幅の指定が結果を変えない。
    ///
    /// 幅を意識した減算を全経路に通しても、既存の値が動かないことの保証。
    #[test]
    fn width_does_not_change_monotonic_increase() {
        for (prev, curr) in [(0u64, 1u64), (100, 150), (4_294_967_000, 4_294_967_290)] {
            assert_eq!(
                s_value_bits(prev, curr, 100, CounterBits::B32),
                s_value_bits(prev, curr, 100, CounterBits::B64),
                "増加のみなら幅に依らず同じ ({prev} → {curr})"
            );
            assert_eq!(
                s_value_bits(prev, curr, 100, CounterBits::B64),
                s_value(prev, curr, 100)
            );
        }
    }

    #[test]
    fn delta_is_valid_when_increasing() {
        let d = compute_delta(100, 150, CounterBits::B64, DeltaContext::default());
        assert_eq!(d, Delta::Valid(50));
        assert_eq!(d.rate_per_sec(100), Some(50.0));
    }

    /// 32bit カウンタの一周は復元できる。
    #[test]
    fn detects_32bit_wraparound() {
        let prev = u32::MAX as u64 - 10;
        let curr = 20u64;
        let d = compute_delta(prev, curr, CounterBits::B32, DeltaContext::default());
        assert_eq!(d, Delta::Wrapped(31), "10 + 1 + 20 = 31");
    }

    /// **回帰テスト (指摘 1)**: 指摘にあった実例をそのまま固定する。
    #[test]
    fn delta_of_wrapped_32bit_counter_is_not_astronomical() {
        let d = compute_delta(4_294_967_290, 4, CounterBits::B32, DeltaContext::default());
        assert_eq!(d, Delta::Wrapped(10));
        assert_eq!(d.value(), Some(10));
        // 1 秒あたり 10 件。1.84e19 ではない
        assert_eq!(d.rate_per_sec(100), Some(10.0));
    }

    /// 64bit カウンタの減少はラップと断定しない。
    #[test]
    fn does_not_assume_wrap_for_64bit() {
        let d = compute_delta(1_000_000, 10, CounterBits::B64, DeltaContext::default());
        assert_eq!(
            d,
            Delta::Unavailable(Discontinuity::AmbiguousDecrease),
            "64bit カウンタの一周は現実的でない"
        );
    }

    /// RESTART を挟んだら差分を作らない。
    #[test]
    fn restart_breaks_continuity() {
        let ctx = DeltaContext {
            continuous: false,
            ..Default::default()
        };
        assert_eq!(
            compute_delta(100, 50, CounterBits::B32, ctx),
            Delta::Unavailable(Discontinuity::Restart),
            "再起動後の値は前の値と繋がらない"
        );
    }

    /// item が入れ替わったら差分を作らない。
    #[test]
    fn item_replacement_breaks_continuity() {
        let ctx = DeltaContext {
            same_item: false,
            ..Default::default()
        };
        assert_eq!(
            compute_delta(100, 200, CounterBits::B64, ctx),
            Delta::Unavailable(Discontinuity::ItemReplaced)
        );
    }

    /// カウンタ両端に同じ定数を足しても、オーバーフローが無ければレートは変わらない
    /// (メタモルフィックテスト)。
    #[test]
    fn rate_is_invariant_under_constant_shift() {
        let (prev, curr, itv) = (1_000u64, 1_500u64, 250u64);
        let base = s_value(prev, curr, itv);
        for shift in [0u64, 1, 1_000, 1_000_000, 1 << 40] {
            assert_eq!(s_value(prev + shift, curr + shift, itv), base);
        }
    }
}

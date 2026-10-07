//! 独自集計の平均。通常値の演算順を保ち、桁あふれしたときだけ逐次平均を使う。

#[derive(Debug, Default)]
pub(crate) struct WeightedMean {
    sum: f64,
    weight: f64,
    total: u128,
    fallback: f64,
}

impl WeightedMean {
    pub(crate) fn push(&mut self, value: f64, weight: u64) {
        if weight == 0 || !value.is_finite() {
            return;
        }
        let total = self.total + u128::from(weight);
        self.fallback = if self.total == 0 {
            value
        } else {
            blend(self.fallback, value, weight as f64 / total as f64)
        };
        self.total = total;
        self.sum += value * weight as f64;
        self.weight += weight as f64;
    }

    pub(crate) fn value(&self) -> Option<f64> {
        (self.total > 0).then(|| {
            if self.sum.is_finite() {
                self.sum / self.weight
            } else {
                self.fallback
            }
        })
    }
}

/// 同符号なら差、異符号なら重みを先に掛けて中間値の桁あふれを避ける。
pub(crate) fn blend(a: f64, b: f64, t: f64) -> f64 {
    if a.is_sign_negative() == b.is_sign_negative() {
        a + (b - a) * t
    } else {
        a * (1.0 - t) + b * t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finite_extremes_and_zero_weights_keep_a_finite_mean() {
        for v in [f64::MAX, -f64::MAX, f64::from_bits(1)] {
            let mut m = WeightedMean::default();
            m.push(999.0, 0);
            assert_eq!(m.value(), None);
            m.push(v, 100);
            m.push(v, 300);
            assert_eq!(m.value(), Some(v));
        }
        let mut m = WeightedMean::default();
        m.push(f64::MAX, 100);
        m.push(-f64::MAX, 100);
        assert_eq!(m.value(), Some(0.0));
    }

    #[test]
    fn ordinary_values_preserve_sum_then_divide_rounding() {
        let mut m = WeightedMean::default();
        let mut sum = 0.0;
        let mut weight = 0.0;
        for (v, w) in [(0.1, 7), (33.33333, 123), (-1.25, 999)] {
            m.push(v, w);
            sum += v * w as f64;
            weight += w as f64;
        }
        assert_eq!(m.value().unwrap().to_bits(), (sum / weight).to_bits());
    }
}

//! 重み付きパーセンタイル。
//!
//! p95 のような分位点は「何を 1 標本と数えるか」で値が変わる。
//! サンプリング間隔が一定でないログ (日境界・採取停止・`-i` による間引き) では
//! **標本重み (サンプル数) と時間重み (区間長) で結果が食い違う**ため、
//! どちらで計算したかを必ず出力メタデータへ残す ([`PercentileSpec`])。
//!
//! 方式は既定で**厳密計算**とする。保持上限を超えた場合は概算へ切り替えず
//! 「算出不能」を返す ([`PercentileUnavailable::RetentionCapExceeded`])。
//! 概算値を黙って出すと、AI エージェントが精度を誤認したまま判定に使ってしまう。

use serde::Serialize;

/// 分位点の重み付け方法。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PercentileWeighting {
    /// 1 サンプル = 重み 1。採取間隔が一定な場合に時間重みと一致する。
    Sample,
    /// 区間長 (1/100 秒) を重みにする。間隔が不均一なログではこちらが実時間を表す。
    Time,
}

impl PercentileWeighting {
    pub const fn as_str(self) -> &'static str {
        match self {
            PercentileWeighting::Sample => "sample",
            PercentileWeighting::Time => "time",
        }
    }
}

/// 分位点の算出アルゴリズム。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PercentileAlgorithm {
    /// 全標本を保持して昇順に並べ、累積重みが目標に達した最初の標本値を採る
    /// (nearest-rank。補間しない)。
    ///
    /// `retention_cap` は 1 系列あたりの保持標本数の上限。
    /// 超えた場合は概算に切り替えず算出不能として報告する。
    ExactWeightedNearestRank { retention_cap: usize },
}

/// 分位点の補間方法。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PercentileInterpolation {
    /// 補間しない。返る値は必ず実観測値のいずれか。
    None,
}

/// 分位点計算の宣言。出力メタデータへそのまま載せる。
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct PercentileSpec {
    /// 分位 (0.0〜1.0)。p95 なら 0.95。
    pub quantile: f64,
    pub weighting: PercentileWeighting,
    pub algorithm: PercentileAlgorithm,
    pub interpolation: PercentileInterpolation,
}

impl PercentileSpec {
    /// 既定の p95 (標本重み・厳密計算・補間なし)。
    pub const fn p95_sample_weighted() -> Self {
        Self {
            quantile: 0.95,
            weighting: PercentileWeighting::Sample,
            algorithm: PercentileAlgorithm::ExactWeightedNearestRank {
                retention_cap: DEFAULT_RETENTION_CAP,
            },
            interpolation: PercentileInterpolation::None,
        }
    }

    /// 時間重みの p95。
    pub const fn p95_time_weighted() -> Self {
        Self {
            quantile: 0.95,
            weighting: PercentileWeighting::Time,
            algorithm: PercentileAlgorithm::ExactWeightedNearestRank {
                retention_cap: DEFAULT_RETENTION_CAP,
            },
            interpolation: PercentileInterpolation::None,
        }
    }

    pub const fn retention_cap(&self) -> usize {
        match self.algorithm {
            PercentileAlgorithm::ExactWeightedNearestRank { retention_cap } => retention_cap,
        }
    }
}

/// 1 系列あたりの標本保持上限の既定値。
///
/// 10 分間隔で 31 日分なら約 4,500 標本、1 秒間隔で 1 日分でも 86,400 標本なので、
/// 実運用のログはこの上限に収まる。
pub const DEFAULT_RETENTION_CAP: usize = 200_000;

/// 分位点が出せない理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PercentileUnavailable {
    /// 有効な標本が無い。
    NoSamples,
    /// 重みの合計が 0 (時間重みで、重み付けできる区間が無い)。
    ZeroWeight,
    /// 保持上限を超えた。概算へは切り替えない。
    RetentionCapExceeded,
}

/// 分位点の算出結果。
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct PercentileResult {
    pub value: f64,
    /// 算出に使った宣言 (再現に必要なのでそのまま持つ)。
    pub spec: PercentileSpec,
    /// 使った標本数。
    pub samples: u64,
    /// 重みの合計。標本重みなら標本数と一致する。
    pub total_weight: u64,
}

/// 重み付き標本の集合。
///
/// 標本は昇順に並べ替えてから使うので、投入順は結果に影響しない
/// (ファイルのマージ順が結果を変えないという性質を保つ)。
#[derive(Debug, Clone)]
pub struct WeightedSamples {
    spec: PercentileSpec,
    samples: Vec<(f64, u64)>,
    /// 上限超過で捨てた標本数。1 以上なら厳密計算は成立しない。
    overflowed: u64,
    total_weight: u128,
}

impl WeightedSamples {
    pub fn new(spec: PercentileSpec) -> Self {
        Self {
            spec,
            samples: Vec::new(),
            overflowed: 0,
            total_weight: 0,
        }
    }

    /// 標本を 1 つ加える。
    ///
    /// `weight_cs` は区間長 (1/100 秒)。標本重みの宣言では無視して 1 を使う。
    /// **非有限値は受け付けない** (0 に丸めると分位点が静かに歪む)。
    pub fn push(&mut self, value: f64, weight_cs: u64) {
        if !value.is_finite() {
            return;
        }
        let w = match self.spec.weighting {
            PercentileWeighting::Sample => 1,
            PercentileWeighting::Time => weight_cs,
        };
        if self.samples.len() >= self.spec.retention_cap() {
            self.overflowed += 1;
            return;
        }
        self.samples.push((value, w));
        self.total_weight += u128::from(w);
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// 分位点を求める。
    pub fn quantile(&self) -> Result<PercentileResult, PercentileUnavailable> {
        if self.overflowed > 0 {
            return Err(PercentileUnavailable::RetentionCapExceeded);
        }
        if self.samples.is_empty() {
            return Err(PercentileUnavailable::NoSamples);
        }
        if self.total_weight == 0 {
            return Err(PercentileUnavailable::ZeroWeight);
        }

        let mut sorted = self.samples.clone();
        // 値の昇順。同値は重みで安定化させ、投入順に依存しないようにする。
        sorted.sort_by(|a, b| {
            a.0.partial_cmp(&b.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.1.cmp(&b.1))
        });

        // 目標累積重み。q=0 のときも最小値が返るよう、比較は「以上」で行う。
        let target = self.spec.quantile.clamp(0.0, 1.0) * self.total_weight as f64;
        let mut acc: u128 = 0;
        let mut picked = sorted[0].0;
        for (v, w) in &sorted {
            if *w == 0 {
                // 重み 0 の標本は分位点の位置を動かさない (時間重みで区間長が無い標本)
                continue;
            }
            acc += u128::from(*w);
            picked = *v;
            if acc as f64 >= target {
                break;
            }
        }

        Ok(PercentileResult {
            value: picked,
            spec: self.spec,
            samples: self.samples.len() as u64,
            total_weight: u64::try_from(self.total_weight).unwrap_or(u64::MAX),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_weighted() -> PercentileSpec {
        PercentileSpec::p95_sample_weighted()
    }

    fn time_weighted() -> PercentileSpec {
        PercentileSpec::p95_time_weighted()
    }

    #[test]
    fn no_samples_is_not_zero() {
        let s = WeightedSamples::new(sample_weighted());
        assert_eq!(s.quantile(), Err(PercentileUnavailable::NoSamples));
    }

    /// 標本重みでは 1 標本 = 1 票。重み引数は無視される。
    #[test]
    fn sample_weighting_ignores_interval_length() {
        let mut s = WeightedSamples::new(sample_weighted());
        // 値 100 は 1 標本だけだが区間は極端に長い
        s.push(100.0, 1_000_000);
        for _ in 0..99 {
            s.push(1.0, 1);
        }
        let r = s.quantile().unwrap();
        assert_eq!(r.total_weight, 100, "標本重みなので重み合計 = 標本数");
        // 100 標本中 95 番目は 1.0 (100.0 は上位 1 標本なので p95 には出ない)
        assert_eq!(r.value, 1.0);
    }

    /// 時間重みでは長い区間の値が分位点を支配する。
    ///
    /// 同じ入力を標本重みで計算すると別の値になることを同時に固定し、
    /// 「宣言どおりの重み付けで計算している」ことを検査する。
    #[test]
    fn time_weighting_follows_interval_length() {
        let mut t = WeightedSamples::new(time_weighted());
        let mut n = WeightedSamples::new(sample_weighted());
        // 短い区間に低い値が 99 個、長い区間に高い値が 1 個
        for _ in 0..99 {
            t.push(1.0, 1);
            n.push(1.0, 1);
        }
        t.push(100.0, 10_000);
        n.push(100.0, 10_000);

        let rt = t.quantile().unwrap();
        assert_eq!(rt.total_weight, 99 + 10_000);
        assert_eq!(rt.value, 100.0, "時間重みでは長時間続いた値が p95 を取る");

        let rn = n.quantile().unwrap();
        assert_eq!(rn.value, 1.0, "標本重みでは 1 標本の外れ値は p95 に出ない");
        assert_ne!(rt.value, rn.value, "重み付けの宣言で結果が変わる");
    }

    /// 投入順が結果を変えない (ファイルのマージ順に依存しない)。
    #[test]
    fn result_is_order_independent() {
        let values = [5.0, 1.0, 9.0, 3.0, 7.0, 2.0, 8.0, 4.0, 6.0, 10.0];
        let mut a = WeightedSamples::new(sample_weighted());
        for v in values {
            a.push(v, 100);
        }
        let mut b = WeightedSamples::new(sample_weighted());
        for v in values.iter().rev() {
            b.push(*v, 100);
        }
        assert_eq!(a.quantile().unwrap().value, b.quantile().unwrap().value);
    }

    /// 補間しないので、返る値は必ず観測値のいずれか。
    #[test]
    fn never_interpolates() {
        let mut s = WeightedSamples::new(sample_weighted());
        for v in [0.0, 10.0] {
            s.push(v, 100);
        }
        let r = s.quantile().unwrap();
        assert!(r.value == 0.0 || r.value == 10.0, "補間値 5.0 を返さない");
        assert_eq!(r.value, 10.0);
    }

    /// 非有限値は受け付けない (0 として数えない)。
    #[test]
    fn rejects_non_finite_values() {
        let mut s = WeightedSamples::new(sample_weighted());
        s.push(f64::NAN, 100);
        s.push(f64::INFINITY, 100);
        assert!(s.is_empty());
        assert_eq!(s.quantile(), Err(PercentileUnavailable::NoSamples));
    }

    /// 保持上限を超えたら概算せず算出不能を返す。
    #[test]
    fn retention_cap_reports_unavailable_instead_of_approximating() {
        let spec = PercentileSpec {
            quantile: 0.95,
            weighting: PercentileWeighting::Sample,
            algorithm: PercentileAlgorithm::ExactWeightedNearestRank { retention_cap: 4 },
            interpolation: PercentileInterpolation::None,
        };
        let mut s = WeightedSamples::new(spec);
        for i in 0..10 {
            s.push(i as f64, 100);
        }
        assert_eq!(s.len(), 4);
        assert_eq!(
            s.quantile(),
            Err(PercentileUnavailable::RetentionCapExceeded)
        );
    }

    /// 時間重みで重み 0 の標本しか無ければ算出不能。
    #[test]
    fn zero_total_weight_is_unavailable() {
        let mut s = WeightedSamples::new(time_weighted());
        s.push(1.0, 0);
        s.push(2.0, 0);
        assert_eq!(s.quantile(), Err(PercentileUnavailable::ZeroWeight));
    }

    /// 宣言はそのまま結果に含まれる (再現可能性のため)。
    #[test]
    fn spec_is_carried_into_result() {
        let mut s = WeightedSamples::new(time_weighted());
        s.push(1.0, 100);
        let r = s.quantile().unwrap();
        assert_eq!(r.spec.weighting, PercentileWeighting::Time);
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("exact_weighted_nearest_rank"), "{json}");
        assert!(json.contains(r#""weighting":"time""#), "{json}");
    }
}

//! 指標の時系列 (区間値の並び) と、そこからの連続区間の抽出。
//!
//! ボトルネック判定は「閾値を超えた」だけでは足りず、**どれだけ続いたか**が要る。
//! そのため判定に使う指標だけを区間値の列として保持し、
//! 連続して条件を満たした区間 ([`Run`]) を根拠として取り出す。
//!
//! ## 欠損を 0 と見なさない
//!
//! 値が得られなかった区間は [`MetricPoint::value`] が `None` になり、
//! 理由 ([`ExclusionReason`]) が付く。`None` の区間は
//! 「閾値を超えていない」とも「超えている」とも扱わず、**連続区間を切る**。
//! 欠損を 0 とみなすと、採取が止まっていた時間が「負荷が無かった時間」に化ける。

use serde::Serialize;

use crate::model::{ActivityId, Unit, ValueKind};
use crate::series::compute::ComputeIssue;
use crate::series::delta::Discontinuity;

/// 指標を一意に指す鍵。
///
/// `item` は item のラベル (`"all"` / `"cpu0"` / `"sda"` など)。
/// item を持たない activity は [`SINGLE_ITEM`] を使う。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct MetricKey {
    pub activity: ActivityId,
    pub item: String,
    pub column: String,
}

/// item を持たない activity の item ラベル。
pub const SINGLE_ITEM: &str = "-";

impl MetricKey {
    pub fn new(activity: ActivityId, item: impl Into<String>, column: impl Into<String>) -> Self {
        Self {
            activity,
            item: item.into(),
            column: column.into(),
        }
    }

    /// 診断・出力用の表記 (`A_CPU/all/idle` の形)。
    pub fn display(&self) -> String {
        format!(
            "{}/{}/{}",
            self.activity.display_name(),
            self.item,
            self.column
        )
    }
}

/// 静的に宣言する指標参照 (ルール定義が持つ)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct MetricRef {
    pub activity: ActivityId,
    pub item: &'static str,
    pub column: &'static str,
}

impl MetricRef {
    pub const fn new(activity: ActivityId, item: &'static str, column: &'static str) -> Self {
        Self {
            activity,
            item,
            column,
        }
    }

    pub fn key(&self) -> MetricKey {
        MetricKey::new(self.activity, self.item, self.column)
    }

    pub fn display(&self) -> String {
        format!(
            "{}/{}/{}",
            self.activity.display_name(),
            self.item,
            self.column
        )
    }
}

/// 値を集計・判定から除外した理由。
///
/// `Availability` (未提供 / レコード欠落) と `Discontinuity` (差分が作れない) の
/// 両方を 1 つの語彙にまとめたもの。**「0 だった」とは別の状態**である。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExclusionReason {
    /// その世代のファイルにフィールドが存在しない。
    UnsupportedBySource,
    /// フィールドはあるが当該レコードで取得できていない。
    MissingInSample,
    /// 系列の先頭で、差分の基準となる前サンプルが無い。
    FirstSample,
    /// RESTART を挟んだ。
    Restart,
    /// item の同一性が崩れた (デバイス名の再利用・CPU のオンライン変化)。
    ItemReplaced,
    /// 経過時間が 0 以下。
    NonPositiveElapsed,
    /// 減少したがラップとは断定できない。
    AmbiguousDecrease,
    /// 分母が 0 (オフライン CPU の tick 合計など)。
    ZeroDenominator,
    /// 計算結果が非有限 (NaN / inf)。
    NotFinite,
    /// 数値ではなく識別子の列 (デバイス名・CPU 番号など)。
    NotNumeric,
    /// 1 item だけでは計算できない列 (item 群全体を要する派生列)。
    NeedsItemGroup,
    /// 派生列の計算が未実装。
    NotImplemented,
    /// ファイル境界で連続性を確認できなかった。
    FileBoundaryUnverified,
    /// activity のレイアウトが前ファイルと違う (値の意味を引き継げない)。
    LayoutChanged,
}

impl ExclusionReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            ExclusionReason::UnsupportedBySource => "unsupported_by_source",
            ExclusionReason::MissingInSample => "missing_in_sample",
            ExclusionReason::FirstSample => "first_sample",
            ExclusionReason::Restart => "restart",
            ExclusionReason::ItemReplaced => "item_replaced",
            ExclusionReason::NonPositiveElapsed => "non_positive_elapsed",
            ExclusionReason::AmbiguousDecrease => "ambiguous_decrease",
            ExclusionReason::ZeroDenominator => "zero_denominator",
            ExclusionReason::NotFinite => "not_finite",
            ExclusionReason::NotNumeric => "not_numeric",
            ExclusionReason::NeedsItemGroup => "needs_item_group",
            ExclusionReason::NotImplemented => "not_implemented",
            ExclusionReason::FileBoundaryUnverified => "file_boundary_unverified",
            ExclusionReason::LayoutChanged => "layout_changed",
        }
    }

    /// 不連続として数えるか (フィールドの未提供・未実装と区別する)。
    pub const fn is_discontinuity(self) -> bool {
        matches!(
            self,
            ExclusionReason::FirstSample
                | ExclusionReason::Restart
                | ExclusionReason::ItemReplaced
                | ExclusionReason::NonPositiveElapsed
                | ExclusionReason::AmbiguousDecrease
                | ExclusionReason::FileBoundaryUnverified
                | ExclusionReason::LayoutChanged
        )
    }

    /// この不連続は**全系列に及ぶ**か。
    ///
    /// 再起動・採取の中断・ファイルの切り替え・レイアウトの変更は
    /// **レコード列の性質**なので、その時刻をまたぐ全系列に効く。
    ///
    /// 一方 item の入れ替えやカウンタの逆行は**その系列だけ**の話である。
    /// 区別しないと、あるデバイスの着脱が無関係な系列の所見まで分断する。
    ///
    /// `is_discontinuity()` が偽の理由 (欠落・未実装など) は連続区間を
    /// 切らないので、ここでは意味を持たない。
    pub const fn affects_all_series(self) -> bool {
        matches!(
            self,
            ExclusionReason::Restart
                | ExclusionReason::FileBoundaryUnverified
                | ExclusionReason::LayoutChanged
        )
    }
}

impl From<Discontinuity> for ExclusionReason {
    fn from(d: Discontinuity) -> Self {
        match d {
            Discontinuity::FirstSample => ExclusionReason::FirstSample,
            Discontinuity::Restart => ExclusionReason::Restart,
            Discontinuity::ItemReplaced => ExclusionReason::ItemReplaced,
            Discontinuity::NonPositiveElapsed => ExclusionReason::NonPositiveElapsed,
            Discontinuity::AmbiguousDecrease => ExclusionReason::AmbiguousDecrease,
        }
    }
}

impl From<ComputeIssue> for ExclusionReason {
    fn from(i: ComputeIssue) -> Self {
        match i {
            ComputeIssue::UnsupportedBySource => ExclusionReason::UnsupportedBySource,
            ComputeIssue::MissingInSample => ExclusionReason::MissingInSample,
            ComputeIssue::Discontinuous(d) => ExclusionReason::from(d),
            // 識別子列・item 群を要する列は数値として集計しない
            ComputeIssue::NotNumeric => ExclusionReason::NotNumeric,
            ComputeIssue::NeedsItemGroup => ExclusionReason::NeedsItemGroup,
            ComputeIssue::NotImplemented => ExclusionReason::NotImplemented,
        }
    }
}

/// 1 区間の観測。
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct MetricPoint {
    /// 区間の始点 (エポック秒)。前サンプルの時刻。
    pub start_ust: u64,
    /// 区間の終点 (エポック秒)。当サンプルの時刻。
    pub end_ust: u64,
    /// 区間長 (1/100 秒)。`uptime` 差分由来なので時刻差より信頼できる。
    pub elapsed_cs: u64,
    /// 区間値。得られなかった場合は `None` (**0 ではない**)。
    pub value: Option<f64>,
    /// `value` が `None` の理由。
    pub reason: Option<ExclusionReason>,
}

impl MetricPoint {
    pub fn observed(start_ust: u64, end_ust: u64, elapsed_cs: u64, value: f64) -> Self {
        Self {
            start_ust,
            end_ust,
            elapsed_cs,
            value: Some(value),
            reason: None,
        }
    }

    pub fn missing(start_ust: u64, end_ust: u64, elapsed_cs: u64, reason: ExclusionReason) -> Self {
        Self {
            start_ust,
            end_ust,
            elapsed_cs,
            value: None,
            reason: Some(reason),
        }
    }
}

/// 観測の網羅度。判定できるだけの観測があるかを示す。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct Coverage {
    pub points: u64,
    pub observed: u64,
    pub missing: u64,
    pub observed_cs: u64,
    pub missing_cs: u64,
}

impl Coverage {
    pub fn has_observation(&self) -> bool {
        self.observed > 0
    }
}

/// 条件を連続して満たした区間。判定の根拠として出力に載せる。
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Run {
    pub start_ust: u64,
    pub end_ust: u64,
    /// 継続時間 (1/100 秒)。区間長の合計。
    pub duration_cs: u64,
    /// 含まれる区間数。
    pub intervals: u64,
    pub min: f64,
    pub max: f64,
    /// 時間加重平均 (`Σ value×elapsed / Σ elapsed`)。
    pub mean: f64,
    /// 最大値を観測した区間の終点。
    pub max_at_ust: u64,
    /// 最小値を観測した区間の終点。
    pub min_at_ust: u64,
}

impl Run {
    pub fn duration_secs(&self) -> f64 {
        self.duration_cs as f64 / 100.0
    }
}

/// 連続区間を組み立てる作業用の蓄積器。
#[derive(Debug, Default)]
struct RunBuilder {
    start_ust: u64,
    end_ust: u64,
    duration_cs: u64,
    intervals: u64,
    min: f64,
    max: f64,
    min_at: u64,
    max_at: u64,
    weighted_sum: f64,
    weight: f64,
    open: bool,
}

impl RunBuilder {
    fn push(&mut self, p: &MetricPoint, v: f64) {
        if !self.open {
            self.open = true;
            self.start_ust = p.start_ust;
            self.duration_cs = 0;
            self.intervals = 0;
            self.min = v;
            self.max = v;
            self.min_at = p.end_ust;
            self.max_at = p.end_ust;
            self.weighted_sum = 0.0;
            self.weight = 0.0;
        }
        self.end_ust = p.end_ust;
        self.duration_cs = self.duration_cs.saturating_add(p.elapsed_cs);
        self.intervals += 1;
        if v < self.min {
            self.min = v;
            self.min_at = p.end_ust;
        }
        if v > self.max {
            self.max = v;
            self.max_at = p.end_ust;
        }
        // 区間長 0 の区間は平均の重みに寄与しない
        let w = p.elapsed_cs as f64;
        self.weighted_sum += v * w;
        self.weight += w;
    }

    fn finish(&self) -> Option<Run> {
        if !self.open {
            return None;
        }
        let mean = if self.weight > 0.0 {
            self.weighted_sum / self.weight
        } else {
            // 区間長が取れない場合は最大値と最小値の中点ではなく、
            // 観測値そのもの (min == max のはず) を返す
            self.max
        };
        Some(Run {
            start_ust: self.start_ust,
            end_ust: self.end_ust,
            duration_cs: self.duration_cs,
            intervals: self.intervals,
            min: self.min,
            max: self.max,
            mean,
            max_at_ust: self.max_at,
            min_at_ust: self.min_at,
        })
    }
}

/// 1 指標の区間値の列。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MetricTimeline {
    pub key: MetricKey,
    pub unit: Unit,
    pub kind: ValueKind,
    pub points: Vec<MetricPoint>,
}

impl MetricTimeline {
    pub fn new(key: MetricKey, unit: Unit, kind: ValueKind) -> Self {
        Self {
            key,
            unit,
            kind,
            points: Vec::new(),
        }
    }

    pub fn push(&mut self, p: MetricPoint) {
        self.points.push(p);
    }

    pub fn coverage(&self) -> Coverage {
        let mut c = Coverage::default();
        for p in &self.points {
            c.points += 1;
            match p.value {
                Some(_) => {
                    c.observed += 1;
                    c.observed_cs = c.observed_cs.saturating_add(p.elapsed_cs);
                }
                None => {
                    c.missing += 1;
                    c.missing_cs = c.missing_cs.saturating_add(p.elapsed_cs);
                }
            }
        }
        c
    }

    /// 条件を連続して満たす最長区間。
    ///
    /// **値が無い区間 (`None`) は連続を切る。** 欠損を「条件を満たさない」と
    /// 見なすのではなく、そこで一旦区切って前後を別の区間として扱う。
    /// 時刻が繋がっていない区間 (別ファイル・採取停止) も同様に切る。
    pub fn longest_run<F>(&self, mut pred: F) -> Option<Run>
    where
        F: FnMut(f64) -> bool,
    {
        let mut best: Option<Run> = None;
        let mut cur = RunBuilder::default();
        let mut prev_end: Option<u64> = None;

        for p in &self.points {
            let adjacent = prev_end.is_none_or(|e| e == p.start_ust);
            prev_end = Some(p.end_ust);

            let keep = match p.value {
                Some(v) => pred(v),
                None => false,
            };

            if !keep || !adjacent {
                // 条件を満たさない / 時刻が繋がらない → いま伸ばしている区間を確定
                if let Some(r) = cur.finish() {
                    best = better(best, r);
                }
                cur = RunBuilder::default();
                if keep && !adjacent {
                    // 繋がらないが条件は満たす → 新しい区間として開始する
                    if let Some(v) = p.value {
                        cur.push(p, v);
                    }
                }
                continue;
            }

            if let Some(v) = p.value {
                cur.push(p, v);
            }
        }
        if let Some(r) = cur.finish() {
            best = better(best, r);
        }
        best
    }

    /// 閾値以上が続いた最長区間。
    pub fn longest_run_at_least(&self, threshold: f64) -> Option<Run> {
        self.longest_run(|v| v >= threshold)
    }

    /// 閾値以下が続いた最長区間。
    pub fn longest_run_at_most(&self, threshold: f64) -> Option<Run> {
        self.longest_run(|v| v <= threshold)
    }
}

fn better(best: Option<Run>, r: Run) -> Option<Run> {
    match best {
        // 継続時間が同じなら早い方を残す (決定的にする)
        Some(b) if b.duration_cs > r.duration_cs => Some(b),
        Some(b) if b.duration_cs == r.duration_cs && b.start_ust <= r.start_ust => Some(b),
        _ => Some(r),
    }
}

/// 指標の集合。判定に必要な指標だけを保持する。
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Timelines {
    entries: Vec<MetricTimeline>,
}

impl Timelines {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, key: &MetricKey) -> Option<&MetricTimeline> {
        self.entries.iter().find(|t| &t.key == key)
    }

    pub fn get_ref(&self, r: &MetricRef) -> Option<&MetricTimeline> {
        self.get(&r.key())
    }

    /// 無ければ作って返す。
    pub fn entry(&mut self, key: MetricKey, unit: Unit, kind: ValueKind) -> &mut MetricTimeline {
        if let Some(i) = self.entries.iter().position(|t| t.key == key) {
            return &mut self.entries[i];
        }
        self.entries.push(MetricTimeline::new(key, unit, kind));
        let last = self.entries.len() - 1;
        &mut self.entries[last]
    }

    pub fn iter(&self) -> impl Iterator<Item = &MetricTimeline> {
        self.entries.iter()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 保持している指標を鍵の昇順へ並べる (出力を決定的にする)。
    pub fn sort(&mut self) {
        self.entries.sort_by(|a, b| a.key.cmp(&b.key));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> MetricKey {
        MetricKey::new(ActivityId::CPU, "all", "idle")
    }

    fn timeline(values: &[(u64, Option<f64>)]) -> MetricTimeline {
        // 1 区間 = 10 秒 (1000 cs) 固定で組む
        let mut t = MetricTimeline::new(key(), Unit::Percent, ValueKind::Counter);
        for (i, (start, v)) in values.iter().enumerate() {
            let _ = i;
            let p = match v {
                Some(v) => MetricPoint::observed(*start, start + 10, 1000, *v),
                None => {
                    MetricPoint::missing(*start, start + 10, 1000, ExclusionReason::MissingInSample)
                }
            };
            t.push(p);
        }
        t
    }

    #[test]
    fn longest_run_accumulates_duration() {
        let t = timeline(&[
            (0, Some(95.0)),
            (10, Some(96.0)),
            (20, Some(97.0)),
            (30, Some(10.0)),
        ]);
        let r = t.longest_run_at_least(90.0).unwrap();
        assert_eq!(r.intervals, 3);
        assert_eq!(r.duration_cs, 3000);
        assert_eq!(r.start_ust, 0);
        assert_eq!(r.end_ust, 30);
        assert_eq!(r.max, 97.0);
        assert_eq!(r.min, 95.0);
        assert_eq!(r.mean, 96.0);
    }

    /// 欠損は「閾値未満」ではなく「連続を切る」。
    ///
    /// 欠損を 0 と見なしていれば区間が切れるのは同じだが、
    /// 欠損を「条件を満たす」と見なしていれば 1 本に繋がってしまう。
    /// ここでは前後が別の区間に分かれることを固定する。
    #[test]
    fn missing_value_breaks_the_run() {
        let t = timeline(&[
            (0, Some(95.0)),
            (10, Some(96.0)),
            (20, None),
            (30, Some(97.0)),
            (40, Some(98.0)),
            (50, Some(99.0)),
        ]);
        let r = t.longest_run_at_least(90.0).unwrap();
        assert_eq!(r.intervals, 3, "欠損の後ろ側 3 区間が最長");
        assert_eq!(r.start_ust, 30);
        assert_eq!(r.duration_cs, 3000);
    }

    /// 欠損は 0 として平均にも入らない。
    #[test]
    fn missing_value_is_not_counted_as_zero() {
        let t = timeline(&[(0, Some(100.0)), (10, None), (20, Some(100.0))]);
        let c = t.coverage();
        assert_eq!(c.observed, 2);
        assert_eq!(c.missing, 1);
        // 100 が 2 区間続いたわけではない (間に欠損があるので 1 区間ずつ)
        let r = t.longest_run_at_least(50.0).unwrap();
        assert_eq!(r.intervals, 1);
        assert_eq!(r.mean, 100.0, "欠損を 0 として平均すると 66.7 になる");
    }

    /// 時刻が繋がらない区間は連続と見なさない。
    #[test]
    fn non_adjacent_points_break_the_run() {
        let mut t = MetricTimeline::new(key(), Unit::Percent, ValueKind::Counter);
        t.push(MetricPoint::observed(0, 10, 1000, 95.0));
        // 1 時間空いてから再開 (別ファイル・採取停止)
        t.push(MetricPoint::observed(3610, 3620, 1000, 96.0));
        t.push(MetricPoint::observed(3620, 3630, 1000, 97.0));
        let r = t.longest_run_at_least(90.0).unwrap();
        assert_eq!(r.intervals, 2, "後半の 2 区間だけが連続");
        assert_eq!(r.start_ust, 3610);
    }

    #[test]
    fn no_run_when_threshold_never_met() {
        let t = timeline(&[(0, Some(1.0)), (10, Some(2.0))]);
        assert!(t.longest_run_at_least(90.0).is_none());
    }

    #[test]
    fn longest_run_at_most_finds_low_side() {
        let t = timeline(&[(0, Some(1.0)), (10, Some(2.0)), (20, Some(50.0))]);
        let r = t.longest_run_at_most(5.0).unwrap();
        assert_eq!(r.intervals, 2);
        assert_eq!(r.max, 2.0);
    }

    #[test]
    fn exclusion_reason_maps_from_discontinuity() {
        assert_eq!(
            ExclusionReason::from(Discontinuity::Restart),
            ExclusionReason::Restart
        );
        assert!(ExclusionReason::Restart.is_discontinuity());
        assert!(!ExclusionReason::UnsupportedBySource.is_discontinuity());
    }

    #[test]
    fn exclusion_reason_maps_from_compute_issue() {
        assert_eq!(
            ExclusionReason::from(ComputeIssue::Discontinuous(Discontinuity::FirstSample)),
            ExclusionReason::FirstSample
        );
        assert_eq!(
            ExclusionReason::from(ComputeIssue::UnsupportedBySource),
            ExclusionReason::UnsupportedBySource
        );
    }

    #[test]
    fn timelines_entry_is_idempotent() {
        let mut ts = Timelines::new();
        ts.entry(key(), Unit::Percent, ValueKind::Counter)
            .push(MetricPoint::observed(0, 10, 1000, 1.0));
        ts.entry(key(), Unit::Percent, ValueKind::Counter)
            .push(MetricPoint::observed(10, 20, 1000, 2.0));
        assert_eq!(ts.len(), 1);
        assert_eq!(ts.get(&key()).unwrap().points.len(), 2);
    }
}

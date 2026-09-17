//! TUI の時系列グラフ。
//!
//! # ここで値を作らない
//!
//! [`GraphView`] は `output::json` の観測値を **(x, y) の座標へ写しただけ**の
//! 表示モデルである。平均・補間・単位換算・間引きはしない。
//! 座標へ写す以上のことをすると、同じ指標が表とグラフで食い違う。
//!
//! # 点を線で結ばない
//!
//! 折れ線は**連続区間ごとに別の系列**へ切る。次のどれかに当たれば線を切る。
//!
//! - その時刻の値が無い (欠測 / その世代に無い列 / 未実装)
//! - サンプルが直前と不連続 (`continuous == false`。再起動・欠測・時刻の巻き戻り)
//! - その時刻にこの item が無い (デバイスの着脱、CPU のオンライン変化)
//!
//! 欠測を `0` や `NaN` に写して 1 本の点列へ混ぜてはいけない。
//! `0` は「観測された 0」と区別がつかず、`NaN` は描画側の実装依存になる。
//!
//! # X 座標は相対秒
//!
//! epoch 秒 (10 桁) をそのまま `f64` の座標にすると、有効桁の大半を
//! 「1970 年からの経過」が占めてしまう。表示範囲の先頭を 0 とした相対秒で持ち、
//! 軸ラベルを作るときだけ [`DisplayTz`] で実時刻へ戻す。

use crate::model::DisplayTz;
use crate::output::json::{FieldOut, Quality, SampleOut};

use super::{display_item, visible_fields};

/// 1 本の折れ線として描ける表示モデル。
pub struct GraphView {
    /// 連続区間ごとの点列。**区間をまたいで線を引かない。**
    pub segments: Vec<Vec<(f64, f64)>>,
    /// X 軸の範囲 (相対秒)。
    pub x_bounds: [f64; 2],
    /// Y 軸の範囲。
    pub y_bounds: [f64; 2],
    /// X = 0 に対応する epoch 秒。軸ラベルを実時刻へ戻すのに使う。
    pub x_origin: u64,
    /// 列の単位 (`percent` / `kB/s` など)。空文字もあり得る。
    pub unit: &'static str,
    /// 値が取れなかった理由の内訳 (件数の多い順)。
    ///
    /// **1 点も描けなかったときに「なぜ描けないか」を出すためだけに持つ。**
    /// これは観測値の集計ではなく、空の画面の説明である。
    pub absent: Vec<(&'static str, usize)>,
    /// 描けた点の総数。
    pub plotted: usize,
}

impl GraphView {
    /// 選択中の activity / item / 列から表示モデルを組み立てる。
    ///
    /// `samples` は時刻順に並んでいることを前提にする (収集時点で保証済み)。
    pub fn build(samples: &[SampleOut], activity: &str, item: &str, column: &str) -> Self {
        let origin = samples.first().map_or(0, |s| s.end_epoch);
        let last = samples.last().map_or(origin, |s| s.end_epoch);

        let mut segments: Vec<Vec<(f64, f64)>> = Vec::new();
        let mut current: Vec<(f64, f64)> = Vec::new();
        let mut absent: Vec<(&'static str, usize)> = Vec::new();
        let mut unit = "";
        let mut plotted = 0usize;

        for s in samples {
            let field = find_field(s, activity, item, column);
            if let Some(f) = field {
                unit = f.unit;
            }
            // **不連続をまたぐ線は引かない。** 値があっても、直前との間に
            // 不連続があれば別の区間として描き始める。
            if !s.continuous && !current.is_empty() {
                segments.push(std::mem::take(&mut current));
            }
            match field.and_then(|f| f.value.filter(|v| v.is_finite()).map(|v| (f, v))) {
                Some((_, v)) => {
                    let x = (s.end_epoch as f64) - (origin as f64);
                    current.push((x, v));
                    plotted += 1;
                }
                None => {
                    // 値が無い時刻では線を切る (飛び越えて次の値へつながない)。
                    if !current.is_empty() {
                        segments.push(std::mem::take(&mut current));
                    }
                    let reason = match field {
                        Some(f) => f.quality.label(),
                        // その時刻にこの item が無い (着脱・入れ替え)。
                        None => "item_absent",
                    };
                    match absent.iter_mut().find(|(r, _)| *r == reason) {
                        Some((_, n)) => *n += 1,
                        None => absent.push((reason, 1)),
                    }
                }
            }
        }
        if !current.is_empty() {
            segments.push(current);
        }
        absent.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));

        let x_bounds = [0.0, ((last.saturating_sub(origin)) as f64).max(1.0)];
        let y_bounds = y_bounds_for(&segments, unit);

        Self {
            segments,
            x_bounds,
            y_bounds,
            x_origin: origin,
            unit,
            absent,
            plotted,
        }
    }

    /// 1 点も描けなかったか。
    pub fn is_empty(&self) -> bool {
        self.plotted == 0
    }

    /// 描けなかった理由の 1 行表記 (`unsupported_by_source: 120, missing_in_sample: 24`)。
    ///
    /// 理由が混ざるときに 1 つへ丸めない。「なぜ空なのか」は 1 つとは限らない。
    pub fn absent_summary(&self) -> String {
        self.absent
            .iter()
            .map(|(r, n)| format!("{r}: {n}"))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// X 軸のラベル。**軸の幅に入るだけ刻む。**
    ///
    /// 両端だけでは、山や谷が何時ごろのことなのか読み取れない。
    /// 幅が許すかぎり刻みを増やし、入らなくなったら本数を減らす。
    ///
    /// 表示範囲が 1 時間を超えたら秒を落として `HH:MM` にする。
    /// 10 分間隔の採取で秒まで出しても読み取りの助けにならず、
    /// 1 本あたり 3 桁を余計に食って刻みが粗くなるだけである。
    pub fn x_labels(&self, tz: DisplayTz, plot_width: u16) -> Vec<String> {
        let span = self.x_bounds[1] - self.x_bounds[0];
        let minutes_only = span >= 3600.0;
        let n = x_label_count(plot_width, minutes_only);
        (0..n)
            .map(|i| {
                let offset = span * (i as f64) / ((n - 1) as f64);
                let at = self.x_origin + offset as u64;
                let t = tz.time(at);
                if minutes_only {
                    // `HH:MM:SS` は ASCII なので、境界で切っても壊れない。
                    t[..5].to_string()
                } else {
                    t
                }
            })
            .collect()
    }

    /// Y 軸のラベル (下端・上端)。
    pub fn y_labels(&self) -> Vec<String> {
        vec![
            super::format_number(self.y_bounds[0]),
            super::format_number(self.y_bounds[1]),
        ]
    }
}

/// 軸に並べるラベルの本数。
///
/// ratatui はラベルを軸上に等間隔で置くので、本数がそのまま刻みの細かさになる。
/// 隣同士がくっつくと読めないため、1 本あたり**ラベル幅 + 余白 4 桁**を見込む。
/// 両端の 2 本は必ず出す (範囲の始まりと終わりが分からないと図を読めない)。
fn x_label_count(plot_width: u16, minutes_only: bool) -> usize {
    let label = if minutes_only { 5 } else { 8 };
    let per = label + 4;
    ((plot_width / per) as usize).clamp(2, 12)
}

/// Y 軸の範囲を決める。
///
/// **観測値を作り変えるのではなく、収まる窓を選ぶだけ。**
///
/// - 割合で 0..=100 に収まっていれば `0-100` に固定する
///   (CPU 使用率の 3% が画面いっぱいに見えると、値を読み違える)
/// - 収まらない割合 (CPU 数で 100% を超える集計値など) は固定しない
/// - 非負の系列は 0 起点にする (0 との距離が意味を持つ量なので)
/// - 負の値を含むなら min-max
fn y_bounds_for(segments: &[Vec<(f64, f64)>], unit: &str) -> [f64; 2] {
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    for seg in segments {
        for (_, y) in seg {
            min = min.min(*y);
            max = max.max(*y);
        }
    }
    if !min.is_finite() || !max.is_finite() {
        return [0.0, 1.0];
    }
    if unit == "percent" && min >= 0.0 && max <= 100.0 {
        return [0.0, 100.0];
    }
    let low = if min >= 0.0 { 0.0 } else { min };
    // 上端に余白を足す。最大値が天井に貼り付くと、そこが上限なのか
    // 切れているのか読めない。全部同じ値のときも幅を 0 にしない。
    let span = (max - low).abs();
    let pad = if span > 0.0 {
        span * 0.05
    } else {
        max.abs().max(1.0) * 0.05
    };
    [low, max + pad]
}

/// 選択中の activity / item / 列に対応するフィールドを引く。
fn find_field<'a>(
    sample: &'a SampleOut,
    activity: &str,
    item: &str,
    column: &str,
) -> Option<&'a FieldOut> {
    sample
        .activities
        .iter()
        .find(|a| a.activity == activity)?
        .items
        .iter()
        .find(|it| display_item(&it.item, it.cpu.as_deref()) == item)
        .and_then(|it| visible_fields(it).find(|f| f.name == column))
}

/// 描画できる列か (数値でない列はグラフにしない)。
pub fn is_plottable(f: &FieldOut) -> bool {
    f.kind != "identity" && f.text.is_none() && f.unit != "identifier"
}

/// 列の品質が「値が無い」ことを示すか (ピッカーの注記に使う)。
pub fn quality_note(q: Quality) -> Option<&'static str> {
    match q {
        Quality::Ok => None,
        other => Some(other.label()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::json::{ActivityOut, ItemOut};

    fn field(name: &'static str, value: Option<f64>, quality: Quality) -> FieldOut {
        FieldOut {
            name,
            unit: "percent",
            kind: "counter",
            raw: None,
            value,
            text: None,
            quality,
        }
    }

    fn sample(end: u64, continuous: bool, value: Option<f64>, quality: Quality) -> SampleOut {
        SampleOut {
            boot: 1,
            start_epoch: end.saturating_sub(600),
            end_epoch: end,
            elapsed_cs: 60_000,
            continuous,
            activities: vec![ActivityOut {
                activity: "A_CPU",
                label: "CPU 使用率",
                items: vec![ItemOut {
                    item: "all".into(),
                    index: 0,
                    cpu: None,
                    raw: Vec::new(),
                    rates: vec![field("user", value, quality)],
                }],
            }],
        }
    }

    const T0: u64 = 1_767_225_600;

    #[test]
    fn a_missing_sample_breaks_the_line_instead_of_plotting_zero() {
        let samples = vec![
            sample(T0, true, Some(1.0), Quality::Ok),
            sample(T0 + 600, true, Some(2.0), Quality::Ok),
            sample(T0 + 1200, true, None, Quality::MissingInSample),
            sample(T0 + 1800, true, Some(3.0), Quality::Ok),
        ];
        let v = GraphView::build(&samples, "A_CPU", "all", "user");
        assert_eq!(v.segments.len(), 2, "欠測で区間が切れる: {:?}", v.segments);
        assert_eq!(v.segments[0], vec![(0.0, 1.0), (600.0, 2.0)]);
        assert_eq!(v.segments[1], vec![(1800.0, 3.0)]);
        // 0 を打っていないこと
        assert!(
            v.segments.iter().flatten().all(|(_, y)| *y > 0.0),
            "欠測を 0 で埋めていない"
        );
        assert_eq!(v.absent, vec![("missing_in_sample", 1)]);
    }

    #[test]
    fn a_discontinuity_breaks_the_line_even_when_both_sides_have_values() {
        let samples = vec![
            sample(T0, true, Some(1.0), Quality::Ok),
            sample(T0 + 600, true, Some(2.0), Quality::Ok),
            // 値はあるが直前と不連続 (再起動など)
            sample(T0 + 1200, false, Some(3.0), Quality::Ok),
            sample(T0 + 1800, true, Some(4.0), Quality::Ok),
        ];
        let v = GraphView::build(&samples, "A_CPU", "all", "user");
        assert_eq!(v.segments.len(), 2);
        assert_eq!(v.segments[0].len(), 2);
        assert_eq!(v.segments[1].len(), 2);
        assert_eq!(v.plotted, 4, "点そのものは全部描く");
    }

    /// 割合は 0-100 に固定する。3% の変動が画面いっぱいに見えてはいけない。
    #[test]
    fn a_percent_series_keeps_the_full_scale() {
        let samples = vec![
            sample(T0, true, Some(2.0), Quality::Ok),
            sample(T0 + 600, true, Some(3.0), Quality::Ok),
        ];
        let v = GraphView::build(&samples, "A_CPU", "all", "user");
        assert_eq!(v.y_bounds, [0.0, 100.0]);
    }

    /// 100% を超え得る値では固定しない (CPU 数で割らない集計値など)。
    #[test]
    fn a_percent_series_above_one_hundred_is_not_clamped() {
        let samples = vec![
            sample(T0, true, Some(150.0), Quality::Ok),
            sample(T0 + 600, true, Some(320.0), Quality::Ok),
        ];
        let v = GraphView::build(&samples, "A_CPU", "all", "user");
        assert!(v.y_bounds[1] > 320.0, "上端に余白: {:?}", v.y_bounds);
        assert_eq!(v.y_bounds[0], 0.0, "非負なので 0 起点");
    }

    #[test]
    fn an_all_missing_series_reports_why_it_is_empty() {
        let samples = vec![
            sample(T0, true, None, Quality::UnsupportedBySource),
            sample(T0 + 600, true, None, Quality::UnsupportedBySource),
            sample(T0 + 1200, true, None, Quality::MissingInSample),
        ];
        let v = GraphView::build(&samples, "A_CPU", "all", "user");
        assert!(v.is_empty());
        assert!(v.segments.is_empty());
        // 件数の多い理由が先に来る。1 つへ丸めない。
        assert_eq!(
            v.absent_summary(),
            "unsupported_by_source: 2, missing_in_sample: 1"
        );
    }

    /// その時刻に item そのものが無い場合も線を切る。
    #[test]
    fn a_vanished_item_breaks_the_line() {
        let mut gone = sample(T0 + 600, true, Some(2.0), Quality::Ok);
        gone.activities[0].items.clear();
        let samples = vec![
            sample(T0, true, Some(1.0), Quality::Ok),
            gone,
            sample(T0 + 1200, true, Some(3.0), Quality::Ok),
        ];
        let v = GraphView::build(&samples, "A_CPU", "all", "user");
        assert_eq!(v.segments.len(), 2);
        assert_eq!(v.absent, vec![("item_absent", 1)]);
    }

    /// X 座標は先頭を 0 とした相対秒。epoch をそのまま使わない。
    #[test]
    fn x_coordinates_are_relative_to_the_first_sample() {
        let samples = vec![
            sample(T0, true, Some(1.0), Quality::Ok),
            sample(T0 + 600, true, Some(2.0), Quality::Ok),
        ];
        let v = GraphView::build(&samples, "A_CPU", "all", "user");
        assert_eq!(v.x_origin, T0);
        assert_eq!(v.x_bounds, [0.0, 600.0]);
        assert_eq!(v.segments[0][0].0, 0.0);
        assert_eq!(v.segments[0][1].0, 600.0);
    }

    /// 軸ラベルは表示タイムゾーンで実時刻へ戻す。
    #[test]
    fn x_labels_come_back_as_wall_clock_in_the_display_timezone() {
        let samples = vec![
            sample(T0, true, Some(1.0), Quality::Ok),
            sample(T0 + 3600, true, Some(2.0), Quality::Ok),
        ];
        let v = GraphView::build(&samples, "A_CPU", "all", "user");
        // T0 = 2026-01-01 00:00:00 UTC = 09:00:00 JST
        // 範囲がちょうど 1 時間なので分までの表記になる。
        let jst = DisplayTz::parse("Asia/Tokyo").unwrap();
        assert_eq!(
            v.x_labels(jst, 40),
            vec!["09:00", "09:20", "09:40", "10:00"],
            "JST"
        );
        assert_eq!(
            v.x_labels(DisplayTz::Utc, 40),
            vec!["00:00", "00:20", "00:40", "01:00"],
            "UTC"
        );
    }

    /// 軸の刻みは画面幅で増える。両端だけでは時刻を読み取れない。
    #[test]
    fn the_time_axis_gets_more_ticks_on_a_wider_screen() {
        let samples: Vec<_> = (0..24)
            .map(|i| sample(T0 + i * 600, true, Some(i as f64), Quality::Ok))
            .collect();
        let v = GraphView::build(&samples, "A_CPU", "all", "user");
        let n = |w| v.x_labels(DisplayTz::Utc, w).len();

        // 狭ければ両端だけ。広がるにつれて刻みが増える。
        assert_eq!(n(10), 2);
        assert!(n(40) > 2, "40 桁: {}", n(40));
        assert!(n(120) > n(40), "120 桁: {} > {}", n(120), n(40));
        // 際限なく増やさない (ラベルで軸が埋まる)
        assert!(n(400) <= 12, "上限: {}", n(400));

        // 刻みは等間隔で、両端は必ず範囲の端を指す。
        let labels = v.x_labels(DisplayTz::Utc, 120);
        assert_eq!(labels.first().unwrap(), "00:00");
        assert_eq!(
            labels.last().unwrap(),
            &DisplayTz::Utc.time(T0 + 23 * 600)[..5]
        );
    }

    /// 短い範囲では秒まで出す (分だけでは同じラベルが並ぶ)。
    #[test]
    fn a_short_span_keeps_the_seconds() {
        let samples = vec![
            sample(T0, true, Some(1.0), Quality::Ok),
            sample(T0 + 60, true, Some(2.0), Quality::Ok),
        ];
        let v = GraphView::build(&samples, "A_CPU", "all", "user");
        let labels = v.x_labels(DisplayTz::Utc, 60);
        assert!(labels.iter().all(|l| l.len() == 8), "秒まで: {labels:?}");
    }
}

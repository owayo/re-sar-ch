//! 検知された系列の前後を切り出した SVG。検知・値計算は再実行しない。
//!
//! 入力全体の保持済み時系列と、報告フィルタ適用後の所見を受け取る。
//! 同じ系列で重なる前後窓だけをまとめ、別ホスト・別起動区間は呼び出し単位で分ける。
//!
//! # 図の言語
//!
//! 図に書く文 (title・desc・凡例・注記・ツールチップ) は [`Chart::lang`]、
//! つまり**所見と同じ言語**で書く。図だけ日本語のまま残すと、英語の報告から
//! 切り出して共有した図が読めない。言語によらないのは数値・時刻・単位記号と、
//! 除外理由の識別子 (`missing_in_sample` など。JSON と同じ値) だけである。
use std::collections::BTreeMap;
use std::io::{self, Write};

use serde::Serialize;

use crate::analyze::assessment::{AssessedDetection, Assessment, describe_detection};
use crate::analyze::summary::{NativePeriodSummary, SummarySource};
use crate::analyze::timeline::{ExclusionReason, MetricKey, MetricPoint};
use crate::detect::{DecisionBasis, Detection, ObservationOrigin, SeriesKey};
use crate::model::{DisplayTz, Lang, Unit, count_en};
use crate::text;

/// 1 枚の図に含まれる所見。優先度・充足度・根拠を元の型で保持する。
#[derive(Debug, Clone, Serialize)]
pub struct ChartFinding {
    pub detection: Detection,
    pub assessed: AssessedDetection,
    pub background: bool,
}

/// 元の採取値と、直前の採取へ線を引いてよいか。
#[derive(Debug, Clone, Serialize)]
pub struct ChartPoint {
    pub point: MetricPoint,
    pub connect_from_previous: bool,
}

/// ホスト・起動区間・系列・検知付近の窓ごとの図。
#[derive(Debug, Clone, Serialize)]
pub struct Chart {
    pub source: SummarySource,
    pub series: SeriesKey,
    pub unit: Unit,
    pub origin: ObservationOrigin,
    pub window_start_ust: u64,
    pub window_end_ust: u64,
    pub context_secs: u64,
    pub findings: Vec<ChartFinding>,
    pub points: Vec<ChartPoint>,
    pub missing_timeline: bool,
    /// 図の文を組み立てる言語。**所見と同じものを使う** (図だけ別言語にしない)。
    #[serde(skip)]
    pub lang: Lang,
}

/// 報告対象の検知から、同一系列の前後窓を作る。
///
/// `summary` は起動区間全体の時系列を保持したものを渡すこと。
/// `assessment` の報告時間範囲や優先度で落ちた所見を復活させない。
pub fn plan(
    summary: &NativePeriodSummary,
    assessment: &Assessment,
    context_secs: u64,
) -> Vec<Chart> {
    let mut grouped: BTreeMap<(SeriesKey, bool), Vec<ChartFinding>> = BTreeMap::new();
    for episode in &assessment.episodes {
        for (detection, assessed) in episode.episode.detections.iter().zip(&episode.detections) {
            grouped
                .entry((detection.series.clone(), false))
                .or_default()
                .push(ChartFinding {
                    detection: detection.clone(),
                    assessed: assessed.clone(),
                    background: false,
                });
        }
    }
    for background in &assessment.background {
        grouped
            .entry((background.detection.series.clone(), true))
            .or_default()
            .push(ChartFinding {
                detection: background.detection.clone(),
                assessed: background.finding.clone(),
                background: true,
            });
    }
    let mut charts = Vec::new();
    // 背景の全期間窓が局所の短い窓を吸収しないよう、図を分ける。
    for ((series, _background), mut findings) in grouped {
        findings.sort_by_key(|f| {
            (
                f.detection.support.start_ust,
                f.detection.support.end_ust,
                f.detection.route(),
            )
        });
        let timeline = summary.timelines.get(&MetricKey::new(
            series.activity,
            &series.item,
            &series.column,
        ));
        let input_start = summary
            .period
            .first_ust
            .or_else(|| timeline.and_then(|t| t.points.iter().map(|p| p.start_ust).min()));
        let input_end = summary
            .period
            .last_ust
            .or_else(|| timeline.and_then(|t| t.points.iter().map(|p| p.end_ust).max()));
        let mut windows: Vec<Chart> = Vec::new();
        for finding in findings {
            let support = finding.detection.support;
            let start = support
                .start_ust
                .saturating_sub(context_secs)
                .max(input_start.unwrap_or(0));
            let end = support
                .end_ust
                .saturating_add(context_secs)
                .min(input_end.unwrap_or(u64::MAX));
            if start > end {
                continue;
            }
            if let Some(previous) = windows.last_mut()
                && start <= previous.window_end_ust
            {
                previous.window_end_ust = previous.window_end_ust.max(end);
                previous.findings.push(finding);
            } else {
                windows.push(Chart {
                    source: summary.source.clone(),
                    series: series.clone(),
                    unit: finding.detection.unit,
                    origin: finding.detection.origin,
                    window_start_ust: start,
                    window_end_ust: end,
                    context_secs,
                    findings: vec![finding],
                    points: Vec::new(),
                    missing_timeline: timeline.is_none(),
                    lang: assessment.lang,
                });
            }
        }
        for chart in &mut windows {
            if let Some(timeline) = timeline {
                let mut previous: Option<&MetricPoint> = None;
                for original in &timeline.points {
                    if original.end_ust < chart.window_start_ust
                        || original.end_ust > chart.window_end_ust
                    {
                        previous = None;
                        continue;
                    }
                    let usable =
                        |p: &MetricPoint| p.reason.is_none() && p.value.is_some_and(f64::is_finite);
                    let connect_from_previous = usable(original)
                        && original.elapsed_cs > 0
                        && original.end_ust > original.start_ust
                        && previous.is_some_and(|p| usable(p) && p.end_ust == original.start_ust);
                    let mut point = *original;
                    if point.value.is_some_and(|v| !v.is_finite()) {
                        point.value = None;
                        point.reason = Some(ExclusionReason::NotFinite);
                    }
                    chart.points.push(ChartPoint {
                        point,
                        connect_from_previous,
                    });
                    previous = Some(original);
                }
            }
        }
        charts.extend(windows);
    }
    charts
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\t' | '\n' | '\r' => out.push(c),
            c if c >= ' ' && c != '\u{fffe}' && c != '\u{ffff}' => out.push(c),
            _ => out.push('�'),
        }
    }
    out
}

/// 図に書く日時。**タイムゾーンを必ず添える。**
///
/// SVG は図だけを切り出して共有されるので、どの基準の時刻かが図の中に
/// 書かれていないと読めない。
fn epoch(tz: DisplayTz, ust: u64) -> String {
    if tz.at(ust).is_none() {
        return format!("epoch {ust}");
    }
    if tz.is_utc() {
        // `datetime()` の末尾 `Z` と名前を重ねない (`...33Z UTC` になる)。
        return format!("{} {} UTC", tz.date(ust), tz.time(ust));
    }
    format!("{} {}", tz.datetime(ust), tz.label_at(ust))
}

/// 長いホスト名・日本語名・改行を、幅に収まる行へ分ける。
///
/// **英数字の語の途中では折り返さない。** 幅の位置で機械的に切ると、英語の文が
/// 語の途中で割れる (`this does no` / `t lead on`)。語の途中で幅を超えたら、
/// その語ごと次の行へ運ぶ。全角の文字の前後は日本語の組版どおりどこでも
/// 折り返せるので、日本語の文は幅いっぱいまで詰める (空白で早めに切ると
/// 行が不揃いになり、行数も増える)。
///
/// 折り返せる位置が行の前半にしか無いとき (長いホスト名など) は、行が短く
/// なりすぎないよう語の途中でも幅の位置で切る。折り返した位置の空白は
/// 行頭にも行末にも残さない。
fn wrapped(text: &str, max_width: usize) -> Vec<String> {
    fn width_of(c: char) -> usize {
        if c.is_ascii() { 1 } else { 2 }
    }
    /// `prev` と `c` の間で折り返してよいか (英数字の語の中でなければよい)。
    fn breakable(prev: char, c: char) -> bool {
        prev.is_whitespace() || c.is_whitespace() || !prev.is_ascii() || !c.is_ascii()
    }
    let mut lines = Vec::new();
    for input in text.split('\n') {
        let mut line = String::new();
        let mut width = 0;
        let mut prev: Option<char> = None;
        // 行内で最後に折り返せる位置 (バイト位置) と、そこまでの幅。
        let mut last_break: Option<(usize, usize)> = None;
        let mut continued = false;
        for c in input.chars() {
            let n = width_of(c);
            let here = prev.is_some_and(|p| breakable(p, c));
            prev = Some(c);
            if width + n > max_width && !line.is_empty() {
                let carried = match last_break {
                    // 語の途中で溢れた: 最後に折り返せる位置より後ろを次の行へ運ぶ
                    Some((at, w)) if !here && w * 2 >= max_width => {
                        line.split_off(at).trim_start().to_string()
                    }
                    _ => String::new(),
                };
                line.truncate(line.trim_end().len());
                lines.push(std::mem::replace(&mut line, carried));
                width = line.chars().map(width_of).sum();
                last_break = None;
                continued = true;
            }
            if c.is_whitespace() && continued && line.is_empty() {
                continue;
            }
            if here {
                last_break = Some((line.len(), width));
            }
            line.push(c);
            width += n;
        }
        lines.push(line);
    }
    lines
}

fn text_lines(
    out: &mut impl Write,
    text: &str,
    y: &mut f64,
    class: &str,
    width: usize,
) -> io::Result<()> {
    for line in wrapped(text, width) {
        writeln!(
            out,
            "<text x=\"32\" y=\"{y:.1}\" class=\"{class}\">{}</text>",
            escape(&line)
        )?;
        *y += if class == "title" { 27.0 } else { 20.0 };
    }
    Ok(())
}

struct Guide {
    value: f64,
    start: u64,
    end: u64,
    label: String,
}

/// 比較基準の中央値を指す線のキャプション。
fn median_caption(d: &Detection, lang: Lang) -> String {
    let basis = d.baseline.basis.label().get(lang);
    match lang {
        Lang::Ja => format!("比較基準の中央値 ({basis})"),
        Lang::En => format!("Median of the comparison basis ({basis})"),
    }
}

fn unit_symbol(unit: Unit) -> &'static str {
    match unit {
        Unit::None | Unit::Identifier => "",
        Unit::Count => "count",
        Unit::CountPerSec => "count/s",
        Unit::Sectors => "sectors",
        Unit::SectorsPerSec => "sectors/s",
        Unit::Centiseconds => "cs",
        Unit::Microseconds => "µs",
        Unit::Jiffies => "jiffies",
        _ => unit.suffix(),
    }
}

fn axis_number(value: f64, lang: Lang) -> String {
    if !value.is_finite() {
        text!(ja: "範囲外", en: "out of range").get(lang).into()
    } else if value.abs() >= 1_000_000.0 || (value != 0.0 && value.abs() < 0.001) {
        format!("{value:.2e}")
    } else {
        format!("{value:.3}")
    }
}

fn guides(chart: &Chart) -> Vec<Guide> {
    let lang = chart.lang;
    let mut out = Vec::new();
    for finding in &chart.findings {
        let d = &finding.detection;
        let mut add = |value: f64, start: u64, end: u64, label: String| {
            if value.is_finite()
                && !out.iter().any(|g: &Guide| {
                    g.value == value && g.start == start && g.end == end && g.label == label
                })
            {
                out.push(Guide {
                    value,
                    start,
                    end,
                    label,
                });
            }
        };
        match &d.decision.basis {
            DecisionBasis::FixedCondition {
                threshold,
                comparison,
                ..
            } => add(*threshold, chart.window_start_ust, chart.window_end_ust, {
                let (cmp, u) = (comparison.label().get(lang), unit_symbol(chart.unit));
                match lang {
                    Lang::Ja => format!("固定条件: {cmp} {threshold}{u}"),
                    Lang::En => format!("Fixed condition: {cmp} {threshold}{u}"),
                }
            }),
            DecisionBasis::RobustDeviation { median, .. } => add(
                *median,
                chart.window_start_ust,
                chart.window_end_ust,
                median_caption(d, lang),
            ),
            DecisionBasis::AbsoluteDeparture { reference, .. } => add(
                *reference,
                chart.window_start_ust,
                chart.window_end_ust,
                median_caption(d, lang),
            ),
            DecisionBasis::LevelShift {
                before_median,
                after_median,
                before,
                after,
                ..
            } => {
                add(
                    *before_median,
                    before.start_ust,
                    before.end_ust,
                    text!(ja: "前窓の中央値", en: "Median of the earlier window")
                        .get(lang)
                        .into(),
                );
                add(
                    *after_median,
                    after.start_ust,
                    after.end_ust,
                    text!(ja: "後窓の中央値", en: "Median of the later window")
                        .get(lang)
                        .into(),
                );
            }
        }
    }
    out
}

const LEFT: f64 = 100.0;
const RIGHT: f64 = 1145.0;
const PLOT_HEIGHT: f64 = 285.0;

struct Coordinates {
    start: u64,
    end: u64,
    top: f64,
    scale: f64,
    low: f64,
    high: f64,
}

impl Coordinates {
    fn new(chart: &Chart, guides: &[Guide], top: f64) -> Self {
        let values: Vec<_> = chart
            .points
            .iter()
            .filter_map(|p| p.point.value.filter(|v| v.is_finite()))
            .chain(guides.iter().map(|g| g.value))
            .collect();
        let min = values.iter().copied().fold(f64::INFINITY, f64::min);
        let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let scale = if values.is_empty() {
            1.0
        } else {
            min.abs().max(max.abs()).max(1.0)
        };
        let (mut low, mut high) = if values.is_empty() {
            (0.0, 1.0)
        } else {
            (min / scale, max / scale)
        };
        if low == high {
            low -= 0.05;
            high += 0.05;
        } else {
            let padding = (high - low) * 0.08;
            low -= padding;
            high += padding;
        }
        // 百分率の入力が定義域内にあるときだけ、見せる余白を定義域内に収める。
        // 実測が負または 100 超なら、値を隠すための clamp はしない。
        if chart.unit == Unit::Percent && !values.is_empty() && min >= 0.0 && max <= 100.0 {
            low = low.max(0.0);
            high = high.min(100.0 / scale);
        }
        Self {
            start: chart.window_start_ust,
            end: chart.window_end_ust,
            top,
            scale,
            low,
            high,
        }
    }
    fn x(&self, t: u64) -> f64 {
        if self.start == self.end {
            return (LEFT + RIGHT) / 2.0;
        }
        LEFT + t.clamp(self.start, self.end).saturating_sub(self.start) as f64
            / self.end.saturating_sub(self.start).max(1) as f64
            * (RIGHT - LEFT)
    }
    fn y(&self, value: f64) -> f64 {
        self.top + (1.0 - (value / self.scale - self.low) / (self.high - self.low)) * PLOT_HEIGHT
    }
}

/// 1 系列・1 前後窓を、外部依存のない SVG にする。
///
/// 文は [`Chart::lang`] で書く (モジュール冒頭の「図の言語」)。
pub fn write_svg<W: Write>(out: &mut W, chart: &Chart, tz: DisplayTz) -> io::Result<()> {
    let lang = chart.lang;
    if chart.window_start_ust > chart.window_end_ust {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            text!(
                ja: "描画範囲の始点が終点より後です",
                en: "the start of the drawing range is after its end",
            )
            .get(lang),
        ));
    }
    let title = format!("{} — {}", chart.source.label, chart.series.display());
    let mut detail_lines = Vec::new();
    for (index, finding) in chart.findings.iter().enumerate() {
        let d = &finding.detection;
        let prefix = if finding.background {
            text!(
                ja: "背景の所見 (入力の大半で続く状態)",
                en: "Standing finding (a state holding across most of the input)",
            )
        } else {
            text!(ja: "局所の所見", en: "Local finding")
        };
        let n = index + 1;
        detail_lines.push(format!(
            "{n}. {} / {}",
            prefix.get(lang),
            d.route().label().get(lang)
        ));
        let (priority, level, basis) = (
            finding.assessed.priority.label().get(lang),
            finding.assessed.sufficiency.level.label().get(lang),
            finding.assessed.sufficiency.basis.label().get(lang),
        );
        detail_lines.push(match lang {
            Lang::Ja => format!("調査優先度: {priority}　根拠の充足度: {level} ({basis})"),
            // **半角空白を並べて間を空けない。** SVG は連続した空白を 1 つに詰めて
            // 描くので、日本語側の全角空白 (詰められない) の代わりにならない。
            Lang::En => format!(
                "Investigation priority: {priority} / Evidence sufficiency: {level} ({basis})"
            ),
        });
        let (from, to, samples) = (
            epoch(tz, d.support.start_ust),
            epoch(tz, d.support.end_ust),
            d.support.samples,
        );
        detail_lines.push(match lang {
            Lang::Ja => format!("検知を裏付けた範囲: {from} → {to} ({samples} 回の採取)"),
            Lang::En => format!(
                "Range the detection rests on: {from} → {to} ({})",
                count_en(samples, "sample", "samples")
            ),
        });
        detail_lines.push(describe_detection(d, lang));
        detail_lines.extend(
            finding
                .assessed
                .priority_reasons
                .iter()
                .map(|s| match lang {
                    Lang::Ja => format!("優先度の理由: {s}"),
                    Lang::En => format!("Reason for the priority: {s}"),
                }),
        );
        if let DecisionBasis::FixedCondition { rationale, .. } = &d.decision.basis {
            detail_lines.push(match lang {
                Lang::Ja => format!("条件の説明: {rationale}"),
                Lang::En => format!("What the condition means: {rationale}"),
            });
        }
        if let DecisionBasis::LevelShift { before, after, .. } = &d.decision.basis {
            let (split, prev_end) = (epoch(tz, after.start_ust), epoch(tz, before.end_ust));
            detail_lines.push(match lang {
                Lang::Ja => format!(
                    "分割時刻: {split} (前窓の末尾: {prev_end})。\
                     変化の発生時刻を確定したものではない。色帯は後窓。"
                ),
                Lang::En => format!(
                    "Split time: {split} (end of the earlier window: {prev_end}). This does not \
                     fix when the change happened. The shaded band is the later window."
                ),
            });
        }
        if d.baseline.may_reflect_the_anomaly() {
            detail_lines.push(
                text!(
                    ja: "留保: 比較基準そのものが異変側に寄っている疑いがある。",
                    en: "Caveat: the comparison basis itself may lean toward the anomaly.",
                )
                .get(lang)
                .to_string(),
            );
        }
    }
    let (window_from, window_to, context) = (
        epoch(tz, chart.window_start_ust),
        epoch(tz, chart.window_end_ust),
        chart.context_secs,
    );
    let range = match lang {
        Lang::Ja => {
            format!("{window_from} → {window_to} / 前後 {context} 秒 (入力・起動区間の端で制限)")
        }
        Lang::En => format!(
            "{window_from} → {window_to} / {context}s before and after \
             (clipped at the edges of the input and the boot segment)"
        ),
    };
    let series_meta = {
        let origin = chart.origin.label().get(lang);
        let unit = if unit_symbol(chart.unit).is_empty() {
            text!(ja: "無次元", en: "dimensionless").get(lang)
        } else {
            unit_symbol(chart.unit)
        };
        let boot = chart.source.boot_segment.map_or_else(
            || {
                text!(ja: "単一区間", en: "single segment")
                    .get(lang)
                    .to_string()
            },
            |n| n.to_string(),
        );
        match lang {
            Lang::Ja => format!("{origin} / 単位: {unit} / 起動区間: {boot}"),
            Lang::En => format!("{origin} / unit: {unit} / boot segment: {boot}"),
        }
    };
    // 英語の区切りも空白を並べずに記号で見せる (詳細行の優先度と同じ理由)。
    // 「検知を裏付けた範囲」は詳細行の見出し (`Range the detection rests on`) と語を揃える。
    let legend = text!(
        ja: "橙: 検知を裏付けた採取の範囲　青点: 採取値　\
             緑破線: 固定条件・比較基準 (正常値ではない)",
        en: "Orange: range the detection rests on / Blue dots: sampled values / \
             Green dashes: fixed conditions and the comparison basis (not a notion of normal)",
    )
    .get(lang);
    let meta = [range.as_str(), series_meta.as_str(), legend];
    let title_lines = wrapped(&title, 90).len();
    let meta_lines: usize = meta.iter().map(|s| wrapped(s, 150).len()).sum();
    // 描画領域は見出し (1 行 27px) と注記 (1 行 20px) の下に 20px 空けて置く。
    // **注記の行数は折り返した結果から数える。** 英語の文は日本語より長く折り返す
    // ので、3 行に収まる前提で固定すると注記が描画領域に重なる。
    let top = 35.0 + title_lines as f64 * 27.0 + meta_lines as f64 * 20.0 + 20.0;
    let footer = top + PLOT_HEIGHT + 80.0;
    let line_count: usize = detail_lines.iter().map(|s| wrapped(s, 150).len()).sum();
    let height = footer + line_count as f64 * 20.0 + 65.0;
    let guides = guides(chart);
    let coords = Coordinates::new(chart, &guides, top);
    writeln!(out, "<?xml version=\"1.0\" encoding=\"UTF-8\"?>")?;
    // 図の言語を宣言する。読み上げの音声と、漢字の字形 (日本語 / 中国語) の選択に効く。
    // `lang` は SVG 2、`xml:lang` は SVG 1.1 の描画系のために両方書く。
    let tag = lang.tag();
    writeln!(
        out,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" lang=\"{tag}\" xml:lang=\"{tag}\" width=\"1200\" height=\"{height:.0}\" viewBox=\"0 0 1200 {height:.0}\" role=\"img\" aria-labelledby=\"chart-title chart-description\">"
    )?;
    writeln!(
        out,
        "<title id=\"chart-title\">{}</title><desc id=\"chart-description\">{}</desc>",
        escape(&title),
        escape(
            text!(
                ja: "検知された系列の前後の採取値。確率や原因の断定ではない。\
                     線は採取値を結ぶ補助線で、採取間の値を観測してはいない。",
                en: "Sampled values of the detected series before and after the detection. \
                     This is neither a probability nor a determination of the cause. \
                     The lines are visual aids joining the samples; values between samples \
                     were not observed.",
            )
            .get(lang)
        )
    )?;
    writeln!(
        out,
        "<style>text{{font:13px sans-serif;fill:#334155}}.title{{font-size:20px;font-weight:600}}.finding{{font-weight:600}}.axis{{stroke:#cbd5e1;stroke-width:1}}.sample-link{{stroke:#0369a1;stroke-width:1.6;fill:none}}.guide{{stroke:#0f766e;stroke-dasharray:6 4;stroke-width:1.2}}.split{{stroke:#7c3aed;stroke-dasharray:4 3}}</style><rect width=\"100%\" height=\"100%\" fill=\"#fff\"/>"
    )?;
    let mut y = 35.0;
    text_lines(out, &title, &mut y, "title", 90)?;
    for line in meta {
        text_lines(out, line, &mut y, "meta", 150)?;
    }
    for finding in &chart.findings {
        let support = finding.detection.support;
        let x1 = coords.x(support.start_ust);
        let x2 = coords.x(support.end_ust);
        let fill = if finding.background {
            "#dbeafe"
        } else {
            "#ffedd5"
        };
        writeln!(
            out,
            "<rect class=\"detection-band\" x=\"{x1:.2}\" y=\"{top:.2}\" width=\"{:.2}\" height=\"{PLOT_HEIGHT}\" fill=\"{fill}\" fill-opacity=\"0.65\"><title>{}</title></rect>",
            (x2 - x1).max(1.0),
            escape(&finding.assessed.headline)
        )?;
        if let DecisionBasis::LevelShift { after, .. } = &finding.detection.decision.basis {
            let x = coords.x(after.start_ust);
            let split = epoch(tz, after.start_ust);
            let caption = match lang {
                Lang::Ja => format!("分割時刻: {split}。確定した発生時刻ではない"),
                Lang::En => {
                    format!("Split time: {split}. This does not fix when the change happened")
                }
            };
            writeln!(
                out,
                "<path class=\"split\" d=\"M {x:.2} {top:.2} V {:.2}\"><title>{}</title></path>",
                top + PLOT_HEIGHT,
                escape(&caption)
            )?;
        }
    }
    for tick in 0..=4 {
        let fraction = f64::from(tick) / 4.0;
        let y = top + PLOT_HEIGHT * fraction;
        let value = (coords.high + (coords.low - coords.high) * fraction) * coords.scale;
        // 表示目盛も有限値に保つ (巨大な有限採取値で余白の乗算が溢れる場合)。
        let label = axis_number(value, lang);
        writeln!(
            out,
            "<path class=\"axis\" d=\"M {LEFT} {y:.2} H {RIGHT}\"/><text x=\"92\" y=\"{:.2}\" text-anchor=\"end\">{}</text>",
            y + 4.0,
            escape(&label)
        )?;
    }
    if chart.window_start_ust < chart.window_end_ust {
        let mut previous_tick = chart.window_start_ust;
        for tick in 1..4u64 {
            let offset = ((chart.window_end_ust - chart.window_start_ust) as u128
                * u128::from(tick)
                / 4) as u64;
            let time = chart.window_start_ust + offset;
            if time <= previous_tick || time >= chart.window_end_ust {
                continue;
            }
            previous_tick = time;
            let x = coords.x(time);
            // 1 日以上に及ぶ窓では日付も出す (同じ時刻が何度も現れるため)
            let label = if chart.window_end_ust - chart.window_start_ust >= 86_400 {
                tz.month_day_time(time)
            } else {
                tz.time(time)
            };
            writeln!(
                out,
                "<path class=\"axis time-tick\" d=\"M {x:.2} {top:.2} V {:.2}\"/><text x=\"{x:.2}\" y=\"{:.2}\" text-anchor=\"middle\">{}</text>",
                top + PLOT_HEIGHT + 5.0,
                top + PLOT_HEIGHT + 25.0,
                escape(&label)
            )?;
        }
    }
    for guide in &guides {
        if guide.end < chart.window_start_ust || guide.start > chart.window_end_ust {
            continue;
        }
        let y = coords.y(guide.value);
        writeln!(
            out,
            "<path class=\"guide\" data-value=\"{}\" d=\"M {:.2} {y:.2} H {:.2}\"><title>{}: {}{}</title></path>",
            guide.value,
            coords.x(guide.start),
            coords.x(guide.end),
            escape(&guide.label),
            guide.value,
            escape(unit_symbol(chart.unit))
        )?;
    }
    let mut previous = None;
    let mut observed = 0;
    for point in &chart.points {
        let p = &point.point;
        let Some(value) = p.value.filter(|v| v.is_finite() && p.reason.is_none()) else {
            previous = None;
            // 理由は JSON と同じ識別子で書く (言語によらない)。
            let (at, no_value) = (
                epoch(tz, p.end_ust),
                text!(ja: "値なし", en: "no value").get(lang),
            );
            let reason = p.reason.map_or(no_value, ExclusionReason::as_str);
            let missing = match lang {
                Lang::Ja => format!("欠測: {at} ({reason})"),
                Lang::En => format!("Missing sample: {at} ({reason})"),
            };
            writeln!(out, "<desc>{}</desc>", escape(&missing))?;
            continue;
        };
        let x = coords.x(p.end_ust);
        let y = coords.y(value);
        if point.connect_from_previous
            && let Some((px, py)) = previous
        {
            writeln!(
                out,
                "<path class=\"sample-link\" d=\"M {px:.2} {py:.2} L {x:.2} {y:.2}\"/>"
            )?;
        }
        let duration = if chart.origin.is_instant() {
            text!(ja: "瞬時値", en: "instantaneous value")
                .get(lang)
                .to_string()
        } else {
            let (from, to) = (epoch(tz, p.start_ust), epoch(tz, p.end_ust));
            match lang {
                Lang::Ja => format!("区間 {from} → {to}"),
                Lang::En => format!("interval {from} → {to}"),
            }
        };
        writeln!(
            out,
            "<circle class=\"sample\" cx=\"{x:.2}\" cy=\"{y:.2}\" r=\"3\" fill=\"#0369a1\" data-time=\"{}\" data-value=\"{value}\"><title>{}: {value}{} ({})</title></circle>",
            p.end_ust,
            escape(&epoch(tz, p.end_ust)),
            escape(unit_symbol(chart.unit)),
            escape(&duration)
        )?;
        previous = Some((x, y));
        observed += 1;
    }
    if observed == 0 {
        let reason = if chart.missing_timeline {
            text!(
                ja: "保持された入力時系列が無いため、前後の採取値を描けません。",
                en: "The input time series was not retained, so the samples around the \
                     detection cannot be drawn.",
            )
        } else {
            text!(
                ja: "この範囲には有効な採取値がありません。",
                en: "There are no valid samples in this range.",
            )
        };
        writeln!(
            out,
            "<text x=\"130\" y=\"{:.2}\">{}</text>",
            top + 140.0,
            escape(reason.get(lang))
        )?;
    }
    writeln!(
        out,
        "<text x=\"{LEFT}\" y=\"{:.2}\">{}</text><text x=\"{RIGHT}\" y=\"{:.2}\" text-anchor=\"end\">{}</text>",
        top + PLOT_HEIGHT + 25.0,
        escape(&epoch(tz, chart.window_start_ust)),
        top + PLOT_HEIGHT + 25.0,
        escape(&epoch(tz, chart.window_end_ust))
    )?;
    writeln!(
        out,
        "<text x=\"{LEFT}\" y=\"{:.2}\" class=\"note\">{}</text>",
        top + PLOT_HEIGHT + 48.0,
        escape(
            text!(
                ja: "線は補助線。欠測・非隣接・不連続をまたいで結びません。\
                     背景の所見は青い帯で表示します。",
                en: "Lines are visual aids and never join across missing samples, non-adjacent \
                     samples or discontinuities. Standing findings are shown as blue bands.",
            )
            .get(lang)
        )
    )?;
    let mut y = footer;
    for line in &detail_lines {
        text_lines(out, line, &mut y, "detail", 150)?;
    }
    writeln!(out, "</svg>")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze::assessment::{Priority, assess};
    use crate::analyze::summary::{NativeSummaryBuilder, PeriodBounds, SummaryOptions};
    use crate::detect::testing::{STEP_CS, STEP_SECS, T0, cpu_idle, single, vals};
    use crate::detect::{DetectOptions, DetectRoute, ReportBound};

    fn fixture(low_ranges: &[(usize, usize)], count: usize) -> (NativePeriodSummary, Assessment) {
        fixture_in(Lang::Ja, low_ranges, count)
    }

    /// 所見の言語を指定して作る。図の言語は所見の言語を引き継ぐ。
    fn fixture_in(
        lang: Lang,
        low_ranges: &[(usize, usize)],
        count: usize,
    ) -> (NativePeriodSummary, Assessment) {
        let mut values = vec![90.0; count];
        for &(start, end) in low_ranges {
            values[start..end].fill(1.0);
        }
        let timelines = single(cpu_idle(&vals(&values)));
        let source = SummarySource {
            label: "synthetic-host".into(),
            boot_segment: Some(2),
            ..Default::default()
        };
        let mut summary =
            NativeSummaryBuilder::new(SummaryOptions::default()).finish(source.clone());
        summary.timelines = timelines;
        summary.period = PeriodBounds {
            first_ust: Some(T0),
            last_ust: Some(T0 + count as u64 * STEP_SECS),
            samples: count as u64,
            ..Default::default()
        };
        let opts = DetectOptions {
            lang,
            ..Default::default()
        };
        let mut outcome = crate::detect::detect(&summary.timelines, &opts);
        // 窓の試験は固定条件の位置を入力どおりに固定し、逸脱/段差の重複に依存させない。
        outcome
            .detections
            .retain(|d| d.route() == DetectRoute::FixedCondition);
        let assessment = assess(outcome, source, summary.period, &opts);
        (summary, assessment)
    }

    /// テストの T0 は UTC 基準に置いた値なので、描画も UTC で確かめる。
    const TZ: DisplayTz = DisplayTz::Utc;

    fn rendered(chart: &Chart) -> String {
        let mut bytes = Vec::new();
        write_svg(&mut bytes, chart, TZ).unwrap();
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn nearby_windows_merge_but_distant_windows_stay_separate() {
        let (summary, assessment) = fixture(&[(10, 12), (14, 16), (30, 32)], 50);
        let charts = plan(&summary, &assessment, STEP_SECS);
        assert_eq!(charts.len(), 2);
        assert_eq!(charts[0].findings.len(), 2);
        assert_eq!(charts[0].window_start_ust, T0 + 9 * STEP_SECS);
        assert_eq!(charts[0].window_end_ust, T0 + 17 * STEP_SECS);
        assert_eq!(charts[1].findings.len(), 1);
        assert_eq!(charts[0].source.boot_segment, Some(2));
        assert!(charts[0].points.iter().any(|p| p.point.value == Some(90.0)));
    }

    #[test]
    fn context_is_clamped_to_input_and_zero_context_keeps_detection_bounds() {
        let (summary, assessment) = fixture(&[(0, 2), (18, 20)], 20);
        let charts = plan(&summary, &assessment, STEP_SECS * 5);
        assert_eq!(charts[0].window_start_ust, T0);
        assert_eq!(charts.last().unwrap().window_end_ust, T0 + 20 * STEP_SECS);
        let exact = plan(&summary, &assessment, 0);
        for chart in exact {
            assert_eq!(
                chart.window_start_ust,
                chart.findings[0].detection.support.start_ust
            );
            assert_eq!(
                chart.window_end_ust,
                chart.findings[0].detection.support.end_ust
            );
        }
    }

    #[test]
    fn report_bounds_do_not_remove_the_input_context() {
        let (summary, _) = fixture(&[(10, 12), (30, 32)], 50);
        let opts = DetectOptions {
            lang: Lang::Ja,
            report_from: ReportBound::Epoch(T0 + 10 * STEP_SECS),
            report_to: ReportBound::Epoch(T0 + 12 * STEP_SECS),
            ..Default::default()
        };
        let mut outcome = crate::detect::detect(&summary.timelines, &opts);
        outcome
            .detections
            .retain(|d| d.route() == DetectRoute::FixedCondition);
        let assessment = assess(outcome, summary.source.clone(), summary.period, &opts);
        let charts = plan(&summary, &assessment, STEP_SECS * 3);
        assert_eq!(charts.len(), 1);
        assert!(
            charts[0]
                .points
                .iter()
                .any(|p| p.point.end_ust < T0 + 10 * STEP_SECS)
        );
        assert!(
            charts[0]
                .points
                .iter()
                .any(|p| p.point.end_ust > T0 + 12 * STEP_SECS)
        );
    }

    #[test]
    fn different_resources_are_never_combined() {
        let (summary, mut assessment) = fixture(&[(10, 12)], 25);
        let mut second = assessment.episodes[0].clone();
        second.episode.detections[0].series.item = "cpu0".into();
        second.detections[0].series.item = "cpu0".into();
        assessment.episodes.push(second);
        let charts = plan(&summary, &assessment, STEP_SECS);
        assert_eq!(charts.len(), 2);
        assert_ne!(charts[0].series.item, charts[1].series.item);
        let missing = charts.iter().find(|c| c.missing_timeline).unwrap();
        assert!(rendered(missing).contains("保持された入力時系列が無い"));
    }

    #[test]
    fn missing_nonadjacent_and_restart_points_break_connections() {
        let (mut summary, assessment) = fixture(&[(10, 12)], 25);
        let key = MetricKey::new(crate::model::ActivityId::CPU, "all", "idle");
        let timeline =
            summary
                .timelines
                .entry(key, Unit::Percent, crate::model::ValueKind::Counter);
        timeline.points[9] = MetricPoint::missing(
            T0 + 9 * STEP_SECS,
            T0 + 10 * STEP_SECS,
            STEP_CS,
            ExclusionReason::MissingInSample,
        );
        timeline.points[13].start_ust += 1;
        timeline.points[15] = MetricPoint::missing(
            T0 + 15 * STEP_SECS,
            T0 + 16 * STEP_SECS,
            STEP_CS,
            ExclusionReason::Restart,
        );
        let charts = plan(&summary, &assessment, 6 * STEP_SECS);
        let chart = &charts[0];
        for index in [9, 10, 13, 15, 16] {
            let point = chart
                .points
                .iter()
                .find(|p| p.point.end_ust == T0 + (index + 1) * STEP_SECS)
                .unwrap();
            assert!(!point.connect_from_previous, "index {index}");
        }
        let regular = chart
            .points
            .iter()
            .find(|p| p.point.end_ust == T0 + 12 * STEP_SECS)
            .unwrap();
        assert!(regular.connect_from_previous);
        let svg = rendered(chart);
        assert!(svg.contains("欠測:"));
        assert!(svg.contains("missing_in_sample"));
        assert!(svg.contains("restart"));
    }

    #[test]
    fn background_and_filtered_results_follow_the_report() {
        let (summary, mut assessment) = fixture(&[(0, 25)], 25);
        assert_eq!(assessment.background.len(), 1);
        let charts = plan(&summary, &assessment, STEP_SECS);
        assert_eq!(charts.len(), 1);
        assert!(charts[0].findings[0].background);
        assert!(rendered(&charts[0]).contains("背景の所見"));
        assessment.background.clear();
        assert!(plan(&summary, &assessment, STEP_SECS).is_empty());
        let (summary, mut assessment) = fixture(&[(10, 12)], 25);
        assessment.episodes.clear();
        assert!(plan(&summary, &assessment, STEP_SECS).is_empty());
    }

    #[test]
    fn a_background_window_never_absorbs_local_findings_of_the_same_series() {
        let (summary, mut assessment) = fixture(&[(10, 12), (30, 32)], 50);
        let (_, background) = fixture(&[(0, 50)], 50);
        assessment.background = background.background;
        assert_eq!(assessment.background.len(), 1);
        let charts = plan(&summary, &assessment, STEP_SECS);
        assert_eq!(charts.len(), 3);
        let local: Vec<_> = charts
            .iter()
            .filter(|c| !c.findings[0].background)
            .collect();
        assert_eq!(local.len(), 2);
        assert!(
            local
                .iter()
                .all(|c| c.window_end_ust - c.window_start_ust == 4 * STEP_SECS)
        );
        let background = charts.iter().find(|c| c.findings[0].background).unwrap();
        assert_eq!(background.window_start_ust, T0);
        assert_eq!(background.window_end_ust, T0 + 50 * STEP_SECS);
        assert!(charts.iter().all(|c| {
            c.findings
                .iter()
                .all(|f| f.background == c.findings[0].background)
        }));
    }

    #[test]
    fn svg_keeps_evidence_priority_and_sufficiency_distinct_and_escapes_text() {
        let (summary, assessment) = fixture(&[(10, 12)], 25);
        let mut chart = plan(&summary, &assessment, STEP_SECS).remove(0);
        chart.source.label = format!("<bad&\"{}\u{1}", "長い名前".repeat(30));
        chart.findings[0].assessed.priority = Priority::Watch;
        let svg = rendered(&chart);
        assert!(svg.contains("&lt;bad&amp;&quot;"));
        assert!(!svg.contains('\u{1}'));
        assert!(svg.matches("class=\"title\"").count() > 1);
        assert!(svg.contains("調査優先度:"));
        assert!(svg.contains("根拠の充足度:"));
        assert!(svg.contains("固定条件"));
        assert!(svg.contains("正常値ではない"));
        assert!(svg.contains("data-value=\"1\""));
        assert!(svg.ends_with("</svg>\n"));
    }

    /// 水準が 90 → 60 と変わる系列の、水準変化の図。
    fn level_shift_chart(lang: Lang) -> Chart {
        let (mut summary, _) = fixture_in(lang, &[], 60);
        let mut values = vec![90.0; 30];
        values.extend(vec![60.0; 30]);
        summary.timelines = single(cpu_idle(&vals(&values)));
        let opts = DetectOptions {
            lang,
            ..Default::default()
        };
        let mut outcome = crate::detect::detect(&summary.timelines, &opts);
        outcome
            .detections
            .retain(|d| d.route() == DetectRoute::LevelShift);
        let assessment = assess(outcome, summary.source.clone(), summary.period, &opts);
        plan(&summary, &assessment, STEP_SECS).remove(0)
    }

    #[test]
    fn level_shift_marks_a_split_without_claiming_an_exact_event_time() {
        let svg = rendered(&level_shift_chart(Lang::Ja));
        assert!(svg.contains("class=\"split\""));
        assert!(svg.contains("分割時刻"));
        assert!(svg.contains("変化の発生時刻を確定したものではない"));
        assert!(svg.contains("前窓の中央値"));
        assert!(svg.contains("後窓の中央値"));
    }

    #[test]
    fn percent_axis_padding_respects_bounds_without_clipping_out_of_range_values() {
        let (summary, assessment) = fixture(&[(10, 12)], 25);
        let mut chart = plan(&summary, &assessment, STEP_SECS).remove(0);
        let coords = Coordinates::new(&chart, &guides(&chart), 0.0);
        assert!(coords.low * coords.scale >= 0.0);
        assert!(coords.high * coords.scale <= 100.0);
        chart.points[0].point.value = Some(-10.0);
        chart.points[1].point.value = Some(120.0);
        let coords = Coordinates::new(&chart, &guides(&chart), 0.0);
        assert!(coords.low * coords.scale < -10.0);
        assert!(coords.high * coords.scale > 120.0);
    }

    #[test]
    fn intermediate_time_ticks_and_exact_detection_bounds_are_visible() {
        let (summary, assessment) = fixture(&[(10, 12)], 25);
        let chart = plan(&summary, &assessment, STEP_SECS).remove(0);
        let svg = rendered(&chart);
        assert_eq!(svg.matches("class=\"axis time-tick\"").count(), 3);
        assert!(svg.contains(&format!(
            "検知を裏付けた範囲: {} → {}",
            epoch(TZ, T0 + 10 * STEP_SECS),
            epoch(TZ, T0 + 12 * STEP_SECS)
        )));
    }

    #[test]
    fn nonfinite_missing_and_extreme_values_never_make_nonfinite_coordinates() {
        let (summary, assessment) = fixture(&[(10, 12)], 25);
        let mut chart = plan(&summary, &assessment, STEP_SECS).remove(0);
        chart.points[0].point.value = Some(f64::MAX);
        chart.points[1].point.value = Some(-f64::MAX);
        chart.points[2].point.value = Some(f64::NAN);
        chart.points[3].point.value = Some(f64::INFINITY);
        let svg = rendered(&chart);
        assert!(!svg.contains("NaN"));
        assert!(!svg.contains("inf"));
        chart.points.clear();
        assert!(rendered(&chart).contains("この範囲には有効な採取値がありません"));
    }

    /// 日本語の文字 (全角の記号・かな・漢字・全角英数) を含む行。
    fn japanese_lines(svg: &str) -> Vec<&str> {
        svg.lines()
            .filter(|line| {
                line.chars().any(|c| {
                    matches!(
                        c,
                        '\u{3000}'..='\u{30ff}'
                            | '\u{3400}'..='\u{4dbf}'
                            | '\u{4e00}'..='\u{9fff}'
                            | '\u{ff00}'..='\u{ffef}'
                    )
                })
            })
            .collect()
    }

    /// 図が自分で書く文をすべて通す場面 (場面の名前, 図)。
    ///
    /// 優先度の理由は分析層が作る文なので固定の英文へ差し替え、
    /// **この層が書いた文だけ**を確かめられるようにする。
    fn scenes(lang: Lang) -> Vec<(&'static str, String)> {
        let render = |mut chart: Chart| {
            for finding in &mut chart.findings {
                finding.assessed.priority_reasons = vec!["reason given by the analysis layer"];
            }
            rendered(&chart)
        };
        let (mut summary, assessment) = fixture_in(lang, &[(10, 12)], 25);
        let key = MetricKey::new(crate::model::ActivityId::CPU, "all", "idle");
        let timeline =
            summary
                .timelines
                .entry(key, Unit::Percent, crate::model::ValueKind::Counter);
        timeline.points[9] = MetricPoint::missing(
            T0 + 9 * STEP_SECS,
            T0 + 10 * STEP_SECS,
            STEP_CS,
            ExclusionReason::MissingInSample,
        );
        // 理由の付かない欠測
        timeline.points[14].value = None;
        let chart = plan(&summary, &assessment, 6 * STEP_SECS).remove(0);
        let mut instant = chart.clone();
        instant.origin = ObservationOrigin::InstantGauge;
        // 余白を足した目盛が有限値で表せない
        let mut extreme = chart.clone();
        extreme.points[0].point.value = Some(f64::MAX);
        extreme.points[1].point.value = Some(-f64::MAX);
        let mut empty = chart.clone();
        empty.points.clear();
        let mut unretained = empty.clone();
        unretained.missing_timeline = true;
        let (summary, assessment) = fixture_in(lang, &[(0, 25)], 25);
        let background = plan(&summary, &assessment, STEP_SECS).remove(0);
        vec![
            ("fixed", render(chart)),
            ("instant", render(instant)),
            ("extreme", render(extreme)),
            ("empty", render(empty)),
            ("unretained", render(unretained)),
            ("level_shift", render(level_shift_chart(lang))),
            ("background", render(background)),
        ]
    }

    /// 図が書く文は所見の言語に従う。英語の報告に日本語の図を付けない。
    #[test]
    fn every_text_the_chart_writes_follows_the_report_language() {
        // (場面, 日本語の図にある文, 英語の図にある文)
        let expected = [
            (
                "fixed",
                "<desc id=\"chart-description\">検知された系列の前後の採取値。\
                 確率や原因の断定ではない。",
                "<desc id=\"chart-description\">Sampled values of the detected series before \
                 and after the detection. This is neither a probability nor a determination \
                 of the cause.",
            ),
            (
                "fixed",
                " 秒 (入力・起動区間の端で制限)</text>",
                "s before and after (clipped at the edges of the input and the boot segment)</text>",
            ),
            (
                "fixed",
                ">橙: 検知を裏付けた採取の範囲　青点: 採取値　緑破線:",
                ">Orange: range the detection rests on / Blue dots: sampled values / Green dashes:",
            ),
            (
                "fixed",
                "class=\"note\">線は補助線。",
                "class=\"note\">Lines are visual aids and never join",
            ),
            ("fixed", "　根拠の充足度: ", " / Evidence sufficiency: "),
            ("fixed", "<title>固定条件: ", "<title>Fixed condition: "),
            ("fixed", "<desc>欠測: ", "<desc>Missing sample: "),
            (
                "fixed",
                " (missing_in_sample)</desc>",
                " (missing_in_sample)</desc>",
            ),
            ("fixed", " (値なし)</desc>", " (no value)</desc>"),
            ("fixed", " (区間 ", " (interval "),
            (
                "instant",
                " (瞬時値)</title>",
                " (instantaneous value)</title>",
            ),
            ("extreme", ">範囲外</text>", ">out of range</text>"),
            (
                "empty",
                ">この範囲には有効な採取値がありません。</text>",
                ">There are no valid samples in this range.</text>",
            ),
            (
                "unretained",
                ">保持された入力時系列が無いため、前後の採取値を描けません。</text>",
                ">The input time series was not retained, so the samples around the detection \
                 cannot be drawn.</text>",
            ),
            ("level_shift", "<title>分割時刻: ", "<title>Split time: "),
            (
                "level_shift",
                "。確定した発生時刻ではない</title>",
                ". This does not fix when the change happened</title>",
            ),
            (
                "level_shift",
                "<title>前窓の中央値: ",
                "<title>Median of the earlier window: ",
            ),
            (
                "level_shift",
                "<title>後窓の中央値: ",
                "<title>Median of the later window: ",
            ),
            (
                "background",
                "背景の所見 (入力の大半で続く状態)",
                "Standing finding (a state holding across most of the input)",
            ),
        ];
        for (lang, tag) in [(Lang::Ja, "ja"), (Lang::En, "en")] {
            let scenes = scenes(lang);
            for (scene, svg) in &scenes {
                // 図の言語を宣言する (読み上げの音声・漢字の字形の選択に使われる)
                let root = format!(
                    "<svg xmlns=\"http://www.w3.org/2000/svg\" lang=\"{tag}\" xml:lang=\"{tag}\" "
                );
                assert!(svg.contains(&root), "{tag} {scene}:\n{svg}");
                match lang {
                    Lang::En => assert_eq!(japanese_lines(svg), Vec::<&str>::new(), "{scene}"),
                    // 判定そのものが働いていることも確かめる (空振りで英語側が通らないように)
                    Lang::Ja => assert!(!japanese_lines(svg).is_empty(), "{scene}"),
                }
            }
            for (scene, ja, en) in expected {
                let text = if lang == Lang::Ja { ja } else { en };
                let (_, svg) = scenes.iter().find(|(name, _)| *name == scene).unwrap();
                assert!(svg.contains(text), "{tag} {scene}: {text}\n{svg}");
            }
        }
    }

    /// 英語の採取回数は数に合わせて単数・複数を変える (`1 samples` にしない)。
    #[test]
    fn the_english_sample_count_agrees_with_the_number() {
        let (summary, assessment) = fixture_in(Lang::En, &[(10, 12)], 25);
        let mut chart = plan(&summary, &assessment, STEP_SECS).remove(0);
        for (samples, expected) in [(1, "(1 sample)</text>"), (2, "(2 samples)</text>")] {
            chart.findings[0].detection.support.samples = samples;
            let svg = rendered(&chart);
            assert!(svg.contains(expected), "{expected}\n{svg}");
        }
    }

    /// 英語の文は語の途中で割らない。折り返した位置の空白も残さない。
    #[test]
    fn english_text_wraps_between_words() {
        let text = "What the condition means: Non-I/O-waiting idle stayed at or below 5%. \
                    In sar(1), %idle is the share of time the CPU was idle with no \
                    outstanding disk I/O request, so this does not lead on to a shortage \
                    of CPU capacity";
        let lines = wrapped(text, 60);
        assert!(lines.len() > 2, "{lines:?}");
        for line in &lines {
            assert!(line.len() <= 60, "{line:?}");
            assert_eq!(line.trim(), line, "{line:?}");
        }
        // 空白 1 つで継ぎ直すと元に戻る = 語の途中で割っていない
        assert_eq!(lines.join(" "), text);
    }

    /// 日本語の文は幅いっぱいまで詰める。英数字の語が溢れたらその語ごと
    /// 次の行へ運び、折り返せる位置が行の前半に無い長い語は幅の位置で切る。
    #[test]
    fn japanese_text_fills_the_line_and_long_words_are_still_cut() {
        // 行の後半に空白があっても、全角の文字の間で詰められるだけ詰める
        let text = format!("{} {}", "時".repeat(12), "空".repeat(20));
        let lines = wrapped(&text, 40);
        assert_eq!(
            lines[0],
            format!("{} {}", "時".repeat(12), "空".repeat(7)),
            "{lines:?}"
        );
        assert_eq!(lines.concat(), text);
        // 英数字の語の途中で溢れたら、その語を次の行へ運ぶ
        let text = format!("{}です Pacific/Honolulu", "時".repeat(15));
        assert_eq!(
            wrapped(&text, 40),
            [
                format!("{}です", "時".repeat(15)),
                "Pacific/Honolulu".into()
            ]
        );
        // 折り返せる位置が行の前半にしか無い長い語 (ホスト名など) は幅の位置で切る
        let host = format!("db {}", "x".repeat(100));
        let lines = wrapped(&host, 40);
        assert!(lines.iter().all(|l| l.len() <= 40), "{lines:?}");
        assert_eq!(lines[0].len(), 40);
        assert_eq!(lines.concat(), host);
    }

    /// 注記が折り返して行が増えたら、その分だけ描画領域を下げる。
    ///
    /// 英語の文は日本語より長く、タイムゾーン名が長いと範囲の行も折り返す。
    /// 3 行に収まる前提で描画領域の位置を固定すると、注記と重なる。
    #[test]
    fn wrapped_header_lines_push_the_plot_down() {
        fn y_of(line: &str) -> f64 {
            let rest = &line[line.find(" y=\"").unwrap() + 4..];
            rest[..rest.find('"').unwrap()].parse().unwrap()
        }
        let tz = DisplayTz::Named(chrono_tz::Pacific::Honolulu);
        // (注記の行数, 注記の最終行から描画領域の上端までの距離)
        let layout = |lang: Lang| {
            let (summary, assessment) = fixture_in(lang, &[(10, 12)], 25);
            let chart = plan(&summary, &assessment, STEP_SECS).remove(0);
            let mut bytes = Vec::new();
            write_svg(&mut bytes, &chart, tz).unwrap();
            let svg = String::from_utf8(bytes).unwrap();
            let meta: Vec<f64> = svg
                .lines()
                .filter(|l| l.contains("class=\"meta\""))
                .map(y_of)
                .collect();
            let last = meta.iter().copied().fold(f64::MIN, f64::max);
            let top = svg
                .lines()
                .find(|l| l.contains("class=\"detection-band\""))
                .map(y_of)
                .unwrap();
            (meta.len(), top - last)
        };
        let (ja_lines, ja_gap) = layout(Lang::Ja);
        let (en_lines, en_gap) = layout(Lang::En);
        assert_eq!(ja_lines, 3, "比較の基準にする日本語の図は折り返さない");
        assert!(en_lines > 3, "英語の注記が折り返していない");
        // 折り返した分だけ下げれば、注記と描画領域の間隔は折り返さない図と同じになる
        assert_eq!(en_gap, ja_gap);
    }
}

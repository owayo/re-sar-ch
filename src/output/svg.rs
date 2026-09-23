//! `sadf -g` の SVG グラフ。値は sadf の他の形式と同じ選択・計算結果を使い、
//! 描画 (装飾・座標) は reSARch 独自のもの。
//!
//! ファイルを 2 回走査して、メモリを系列数に比例する量に抑える
//! (1 回目で値域を測り、2 回目で点と線分をストリーミングで書く)。
//! サンプルの全履歴は保持しない。
use std::collections::BTreeMap;
use std::io::Write;

use crate::cli::SadfOutputOptions;
use crate::cli::sadf_args::SvgPalette;
use crate::error::{Error, Result};
use crate::format::file::{SaFile, ScanControl};
use crate::model::ActivityId;
use crate::series::snapshot::{IntervalView, RecordEvent, Selection, WalkItem, walk_items};

use super::sadf::access::ActivityPair;
use super::sadf::dbppc::selected_specs;
use super::sadf::render::{item_label_in, value_of};
use super::sadf::spec::{ActivitySpec, Fmt};
use super::sadf::{FileInfo, SadfConfig, Stamp, TimeBase};
use super::time_filter::Admit;

const WIDTH: f64 = 1040.0;
const LEFT: f64 = 100.0;
const RIGHT: f64 = 1000.0;
const PANEL: f64 = 205.0;
const PLOT: f64 = 125.0;

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Key {
    activity: ActivityId,
    item: String,
    metric: &'static str,
}

impl Key {
    fn label(&self) -> String {
        format!(
            "{} / {} / {}",
            self.activity.display_name(),
            self.item,
            self.metric
        )
    }
}

#[derive(Default)]
struct Chart {
    min: Option<f64>,
    max: Option<f64>,
    nonzero: bool,
    index: usize,
    previous: Option<(u64, f64, f64)>,
}

impl Chart {
    fn observe(&mut self, value: f64) {
        self.min = Some(self.min.map_or(value, |v| v.min(value)));
        self.max = Some(self.max.map_or(value, |v| v.max(value)));
        self.nonzero |= value != 0.0;
    }

    fn coordinates(&self, t: u64, value: f64, start: u64, end: u64, top: f64) -> (f64, f64) {
        let low = self.min.unwrap_or(0.0).min(0.0);
        let high = self.max.unwrap_or(0.0).max(0.0);
        let scale = high.abs().max(low.abs()).max(1.0);
        let range = (high / scale - low / scale).max(f64::EPSILON);
        let x = LEFT
            + (t.saturating_sub(start)) as f64 / end.saturating_sub(start).max(1) as f64
                * (RIGHT - LEFT);
        let y = top + self.index as f64 * PANEL + 40.0 + PLOT
            - (value / scale - low / scale) / range * PLOT;
        (x, y)
    }
}

/// `-O oneday` の軸を切る基準。
///
/// 日の境目は時刻基準ごとに違う。**軸だけ別の基準で切らない。**
#[derive(Debug, Clone)]
enum DayZone {
    /// UTC の 0 時で切る (`sadf` の既定と `-U`)。
    Utc,
    /// 実行環境のローカル 0 時で切る (`-T`)。
    Local,
    /// 採取側のローカル 0 時で切る (`-t`)。オフセットは秒。
    Recorded { offset: i64, name: String },
}

impl DayZone {
    /// 軸に添える基準の名前。
    fn label(&self) -> String {
        match self {
            DayZone::Utc => "UTC".to_string(),
            DayZone::Local => local_zone_label(),
            // `sa_tzname` が空のファイルでは名前を出せない。
            DayZone::Recorded { name, .. } if name.is_empty() => "recorded".to_string(),
            DayZone::Recorded { name, .. } => name.clone(),
        }
    }
}

/// 実行環境の TZ 名 (取れなければ数値オフセット)。
fn local_zone_label() -> String {
    use chrono::Offset;
    chrono::Local::now().offset().fix().to_string()
}

/// `-O oneday` の基準を時刻基準から決める。
///
/// `recorded_offset` は採取側のローカル時刻と UTC の差 (秒)。
/// レコードが持つ時分秒から復元した値で、`-t` のときだけ意味を持つ。
fn one_day_zone(base: TimeBase, info: &FileInfo, recorded_offset: Option<i64>) -> DayZone {
    match base {
        // `-U` は epoch 秒表示。日付の概念を持ち込まないので UTC で切る。
        TimeBase::Utc | TimeBase::SecEpoch => DayZone::Utc,
        TimeBase::LocalTime => DayZone::Local,
        TimeBase::TrueTime => DayZone::Recorded {
            // 統計レコードが 1 つも無ければ差を取れない。UTC 相当に落とす。
            offset: recorded_offset.unwrap_or(0),
            name: info.tzname.clone(),
        },
    }
}

/// `ust_time` が属する日の 0 時 (epoch 秒)。
fn day_start(ust: u64, zone: &DayZone) -> u64 {
    let shift = match zone {
        DayZone::Utc => 0,
        DayZone::Local => {
            use chrono::{Offset, TimeZone};
            chrono::Local
                .timestamp_opt(ust as i64, 0)
                .single()
                .map_or(0, |dt| i64::from(dt.offset().fix().local_minus_utc()))
        }
        DayZone::Recorded { offset, .. } => *offset,
    };
    // 現地時刻へ寄せて日境界で切り、UTC へ戻す。
    let local = ust as i64 + shift;
    let floored = local.div_euclid(86_400) * 86_400;
    (floored - shift).max(0) as u64
}

fn escaped(text: &str) -> String {
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

fn validate(options: &SadfOutputOptions) -> Result<()> {
    let unsupported = if options.autoscale {
        Some("autoscale")
    } else if options.packed {
        Some("packed")
    } else if options.palette == SvgPalette::Custom {
        Some("customcol")
    } else if options.user_hz.is_some() {
        Some("hz (-c only)")
    } else if options.pcp_archive.is_some() {
        Some("pcparchive (-l only)")
    } else {
        None
    };
    if let Some(option) = unsupported {
        return Err(Error::Other(format!("sadf -g: -O {option} は未対応です")));
    }
    if options.canvas_height == Some(0) {
        return Err(Error::Other("sadf -g: height は 1 以上が必要です".into()));
    }
    Ok(())
}

/// 1 サンプル分の計算済みの値。出力層は座標へ縮尺するだけで、値は計算しない。
fn values(
    view: &IntervalView<'_>,
    cfg: &SadfConfig,
    options: &SadfOutputOptions,
    specs: &[&ActivitySpec],
) -> Vec<(Key, Option<f64>)> {
    let mut out = Vec::new();
    for spec in specs {
        let Some(pair) = ActivityPair::from_view(view, spec.id) else {
            continue;
        };
        if spec.id == ActivityId::IRQ {
            let (cpus, irqs) = pair.irq_dimensions();
            for irq in 0..irqs {
                let name = pair.irq_name(irq);
                if !cfg.name_selected(spec.id, &name) {
                    continue;
                }
                for cpu in 0..cpus {
                    if !pair.irq_cpu_selected(cfg, cpu, false) {
                        continue;
                    }
                    let Some(item) = pair.irq_item(cpu, irq) else {
                        continue;
                    };
                    let cpu = if cpu == 0 {
                        "all".to_string()
                    } else {
                        format!("cpu{}", cpu - 1)
                    };
                    out.push((
                        Key {
                            activity: spec.id,
                            item: format!("{name}/{cpu}"),
                            metric: "intr/s",
                        },
                        item.computed(0).ok().filter(|v| v.is_finite()),
                    ));
                }
            }
            continue;
        }
        for section in spec.active_sections(&cfg.section) {
            for item in pair.selected_items(cfg, false) {
                let label = item_label_in(spec, section, &item).jx;
                for field in section.fields {
                    if !cfg.section.allows_field(field.gate)
                        || !matches!(field.dp_fmt, Fmt::R2 | Fmt::R0 | Fmt::Int)
                        || (spec.id == ActivityId::CPU && field.col == "idle" && !options.show_idle)
                    {
                        continue;
                    }
                    let metric = if field.pp.is_empty() {
                        field.key
                    } else {
                        field.pp
                    };
                    if metric.is_empty() {
                        continue;
                    }
                    out.push((
                        Key {
                            activity: spec.id,
                            item: if label.is_empty() {
                                "-".into()
                            } else {
                                label.clone()
                            },
                            metric,
                        },
                        value_of(&item, field).ok().filter(|v| v.is_finite()),
                    ));
                }
            }
        }
    }
    out
}

/// 単体で開ける SVG を書く。未対応の `-O` サブオプションは書き始める前に拒否する。
pub fn write_svg<W: Write>(
    out: &mut W,
    file: &SaFile,
    cfg: &SadfConfig,
    options: &SadfOutputOptions,
) -> Result<()> {
    validate(options)?;
    let specs = selected_specs(file, cfg);
    let selection = Selection::Only(specs.iter().map(|s| s.id).collect());
    let info = FileInfo::from_file_with(file, cfg.time_base);
    let mut charts: BTreeMap<Key, Chart> = BTreeMap::new();
    let mut cursor = cfg.time_filter.cursor();
    let mut bounds: Option<(u64, u64)> = None;
    let mut first_label = String::new();
    let mut last_label = String::new();
    // `-t` の日境界を切るのに要る、採取側ローカルと UTC の差 (秒)。
    // レコードが持つ時分秒から復元する。
    let mut recorded_offset: Option<i64> = None;
    walk_items(file, &selection, |item| {
        let WalkItem::Sample(view) = item else {
            return Ok(ScanControl::Continue);
        };
        match cursor.sample(view) {
            Admit::Reference | Admit::Skip => return Ok(ScanControl::Continue),
            Admit::Stop => return Ok(ScanControl::Stop),
            Admit::Emit => {}
        }
        if !view.has_prev || !view.continuous {
            return Ok(ScanControl::Continue);
        }
        let t = view.curr.ust_time;
        if recorded_offset.is_none() {
            let shifted = super::sadf::shift_to_recorded(
                t,
                (view.curr.hour, view.curr.minute, view.curr.second),
            );
            recorded_offset = Some(shifted as i64 - t as i64);
        }
        let stamp = Stamp::new(
            cfg.time_base,
            t,
            (view.curr.hour, view.curr.minute, view.curr.second),
            &info,
        )
        .dbppc();
        match &mut bounds {
            None => {
                bounds = Some((t, t));
                first_label = stamp.clone();
            }
            Some((start, end)) => {
                *start = (*start).min(t);
                *end = (*end).max(t);
            }
        }
        last_label = stamp;
        for (key, value) in values(view, cfg, options, &specs) {
            let chart = charts.entry(key).or_default();
            if let Some(value) = value {
                chart.observe(value);
            }
        }
        Ok(ScanControl::Continue)
    })?;
    if options.skip_empty {
        charts.retain(|_, c| c.nonzero);
    }
    for (index, chart) in charts.values_mut().enumerate() {
        chart.index = index;
    }
    let top = 115.0
        + if options.show_toc {
            charts.len() as f64 * 22.0 + 20.0
        } else {
            0.0
        };
    let natural_height = (top + charts.len() as f64 * PANEL + 40.0).max(220.0);
    let height = options.canvas_height.map_or(natural_height, f64::from);
    writeln!(out, "<?xml version=\"1.0\" encoding=\"UTF-8\"?>")?;
    writeln!(
        out,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{WIDTH}\" height=\"{height}\" viewBox=\"0 0 {WIDTH} {natural_height}\" role=\"img\" aria-labelledby=\"title description\">"
    )?;
    writeln!(
        out,
        "<title id=\"title\">reSARch — {}</title><desc id=\"description\">Time series from the selected sar activities. Gaps and restarts are not connected.</desc>",
        escaped(&info.nodename)
    )?;
    writeln!(
        out,
        "<style>text{{font:12px sans-serif;fill:#334155}}.heading{{font-size:22px;font-weight:600}}.metric{{font-size:14px;font-weight:600}}.axis{{stroke:#cbd5e1;stroke-width:1}}.series{{stroke-width:1.7;fill:none}}.point{{stroke:none}}</style><rect width=\"100%\" height=\"100%\" fill=\"#fff\"/>"
    )?;
    writeln!(
        out,
        "<text x=\"32\" y=\"36\" class=\"heading\">reSARch · {}</text><text x=\"32\" y=\"62\">{} → {}</text>",
        escaped(&info.nodename),
        escaped(&first_label),
        escaped(&last_label)
    )?;
    if options.debug {
        writeln!(
            out,
            "<!-- reSARch SVG; two-pass streaming; {} charts -->",
            charts.len()
        )?;
    }
    if charts.is_empty() {
        writeln!(
            out,
            "<text x=\"32\" y=\"115\">No selected numeric samples.</text>"
        )?;
    }
    if options.show_toc {
        for (key, chart) in &charts {
            writeln!(
                out,
                "<a href=\"#chart-{}\"><text x=\"32\" y=\"{}\">{}</text></a>",
                chart.index,
                98.0 + chart.index as f64 * 22.0,
                escaped(&key.label())
            )?;
        }
    }
    let (mut start, mut end) = bounds.unwrap_or((0, 1));
    // `-O oneday` の軸は、**他のラベルと同じ時刻基準で切る。**
    // ここだけ UTC 固定にすると、`-T` / `-t` を付けたとき軸の 00:00 と
    // データ点の時刻が別の基準になり、図の中で辻褄が合わなくなる。
    let day_tz = one_day_zone(cfg.time_base, &info, recorded_offset);
    if options.one_day {
        start = day_start(start, &day_tz);
        end = start.saturating_add(86_400);
        writeln!(
            out,
            "<text x=\"32\" y=\"83\">24-hour axis: 00:00–24:00 {}</text>",
            escaped(&day_tz.label())
        )?;
    }
    for (key, chart) in &charts {
        let y = top + chart.index as f64 * PANEL;
        writeln!(
            out,
            "<g id=\"chart-{}\"><text x=\"32\" y=\"{}\" class=\"metric\">{}</text>",
            chart.index,
            y + 17.0,
            escaped(&key.label())
        )?;
        if options.show_info {
            writeln!(
                out,
                "<text x=\"32\" y=\"{}\">{} · {} · {} CPU</text>",
                y + 33.0,
                escaped(&info.nodename),
                escaped(&info.file_date),
                info.cpu_count
            )?;
        }
        writeln!(
            out,
            "<path class=\"axis\" d=\"M {LEFT} {} V {} H {RIGHT}\"/><text x=\"90\" y=\"{}\" text-anchor=\"end\">{:.2}</text><text x=\"90\" y=\"{}\" text-anchor=\"end\">{:.2}</text>",
            y + 40.0,
            y + 40.0 + PLOT,
            y + 45.0,
            chart.max.unwrap_or(0.0).max(0.0),
            y + 40.0 + PLOT,
            chart.min.unwrap_or(0.0).min(0.0)
        )?;
        writeln!(
            out,
            "<text x=\"{LEFT}\" y=\"{}\">{}</text><text x=\"{RIGHT}\" y=\"{}\" text-anchor=\"end\">{}</text>",
            y + 185.0,
            escaped(&if options.one_day {
                format!("00:00 {}", day_tz.label())
            } else {
                first_label.clone()
            }),
            y + 185.0,
            escaped(&if options.one_day {
                format!("24:00 {}", day_tz.label())
            } else {
                last_label.clone()
            })
        )?;
        if chart.min.is_none() {
            writeln!(
                out,
                "<text x=\"125\" y=\"{}\">No usable observation</text>",
                y + 85.0
            )?;
        }
        writeln!(out, "</g>")?;
    }
    let mut cursor = cfg.time_filter.cursor();
    let mut ordinal = 0u64;
    walk_items(file, &selection, |item| {
        let WalkItem::Sample(view) = item else {
            if let WalkItem::Event(event) = item {
                if matches!(event, RecordEvent::Restart { .. }) {
                    for chart in charts.values_mut() {
                        chart.previous = None;
                    }
                }
                if let RecordEvent::Comment { ref text, .. } = event
                    && cfg.comments
                    && cursor.event(event.ust_time(), event.time())
                {
                    writeln!(out, "<desc>{}</desc>", escaped(text))?;
                }
            }
            return Ok(ScanControl::Continue);
        };
        ordinal += 1;
        match cursor.sample(view) {
            Admit::Reference | Admit::Skip => return Ok(ScanControl::Continue),
            Admit::Stop => return Ok(ScanControl::Stop),
            Admit::Emit => {}
        }
        if !view.has_prev || !view.continuous {
            for chart in charts.values_mut() {
                chart.previous = None;
            }
            return Ok(ScanControl::Continue);
        }
        let t = view.curr.ust_time;
        if t < start || t > end {
            return Ok(ScanControl::Continue);
        }
        for (key, value) in values(view, cfg, options, &specs) {
            let Some(chart) = charts.get_mut(&key) else {
                continue;
            };
            let Some(value) = value else {
                chart.previous = None;
                continue;
            };
            let (x, y) = chart.coordinates(t, value, start, end, top);
            let color = if options.palette == SvgPalette::Bw {
                "#111827"
            } else {
                "#0369a1"
            };
            if let Some((previous, px, py)) = chart.previous
                && previous + 1 == ordinal
            {
                writeln!(
                    out,
                    "<path class=\"series\" stroke=\"{color}\" data-chart=\"{}\" d=\"M {px:.2} {py:.2} L {x:.2} {y:.2}\"/>",
                    chart.index
                )?;
            }
            writeln!(
                out,
                "<circle class=\"point\" fill=\"{color}\" cx=\"{x:.2}\" cy=\"{y:.2}\" r=\"2\" data-chart=\"{}\" data-time=\"{t}\" data-value=\"{value}\"><title>{}: {value}</title></circle>",
                chart.index,
                escaped(&key.label())
            )?;
            chart.previous = Some((ordinal, x, y));
        }
        Ok(ScanControl::Continue)
    })?;
    writeln!(out, "</svg>")?;
    Ok(())
}

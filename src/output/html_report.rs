//! 単体で開ける HTML レポート。計算済み時系列と評価結果を描画用に書式化する。
//! 時刻は DisplayTz で解決し、ブラウザのタイムゾーンでは解釈し直さない。
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};

use serde::Serialize;

use crate::analyze::assessment::{AssessedDetection, Assessment, describe_detection};
use crate::analyze::summary::ColumnSummary;
use crate::analyze::timeline::{ExclusionReason, MetricKey};
use crate::detect::{DecisionBasis, Detection, ObservationOrigin};
use crate::model::{DisplayTz, Lang, Unit, ValueKind};
use crate::multi::MultiFileAnalysis;

#[derive(Serialize)]
struct Report<'a> {
    schema_version: &'static str,
    lang: &'static str,
    partial: bool,
    skipped: &'a [crate::multi::SkippedFile],
    incomplete: &'a [crate::multi::IncompleteFile],
    segments: Vec<Segment<'a>>,
}

#[derive(Serialize)]
struct Segment<'a> {
    host: &'a str,
    boot: usize,
    timezone: String,
    times: Vec<TimeLabel>,
    series: Vec<Series<'a>>,
    findings: Vec<Finding>,
    evaluations: Vec<[String; 6]>,
    assessment: &'a Assessment,
}

#[derive(Serialize)]
struct TimeLabel {
    // u64 を JavaScript の Number に丸めない。座標変換だけで Number を使う。
    epoch: String,
    label: String,
    short: String,
    clock: String,
}

// 始点索引・終点索引・値・除外理由・直前の点と接続できるか。
type Point = (usize, usize, Option<f64>, Option<ExclusionReason>, bool);

#[derive(Serialize)]
struct Series<'a> {
    activity: String,
    item: &'a str,
    metric: &'a str,
    label: &'a str,
    unit: &'static str,
    percent: bool,
    origin: &'static str,
    points: Vec<Point>,
    summary: Option<&'a ColumnSummary>,
}

#[derive(Serialize)]
struct Reference {
    value: f64,
    start: String,
    end: String,
    label: String,
}

#[derive(Serialize)]
struct Finding {
    series: Option<usize>,
    start: String,
    end: String,
    from_label: String,
    to_label: String,
    headline: String,
    priority: &'static str,
    evidence: &'static str,
    detail: String,
    background: bool,
    references: Vec<Reference>,
}

fn finding(
    d: &Detection,
    assessed: &AssessedDetection,
    background: bool,
    keys: &[MetricKey],
    lang: Lang,
    tz: DisplayTz,
) -> Finding {
    let mut references = Vec::new();
    let mut add = |value: f64, start: u64, end: u64, label: String| {
        if value.is_finite() {
            references.push(Reference {
                value,
                start: start.to_string(),
                end: end.to_string(),
                label,
            });
        }
    };
    let median = match lang {
        Lang::Ja => "比較基準の中央値",
        Lang::En => "Comparison median",
    };
    match &d.decision.basis {
        DecisionBasis::FixedCondition {
            threshold,
            comparison,
            ..
        } => add(
            *threshold,
            d.support.start_ust,
            d.support.end_ust,
            format!("{} {}", comparison.label().get(lang), threshold),
        ),
        DecisionBasis::RobustDeviation { median: value, .. } => add(
            *value,
            d.support.start_ust,
            d.support.end_ust,
            format!("{median} ({})", d.baseline.basis.label().get(lang)),
        ),
        DecisionBasis::AbsoluteDeparture { reference, .. } => add(
            *reference,
            d.support.start_ust,
            d.support.end_ust,
            format!("{median} ({})", d.baseline.basis.label().get(lang)),
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
                match lang {
                    Lang::Ja => "前窓の中央値",
                    Lang::En => "Earlier window median",
                }
                .into(),
            );
            add(
                *after_median,
                after.start_ust,
                after.end_ust,
                match lang {
                    Lang::Ja => "後窓の中央値",
                    Lang::En => "Later window median",
                }
                .into(),
            );
        }
    }
    let mut detail = describe_detection(d, lang);
    if let DecisionBasis::FixedCondition { rationale, .. } = &d.decision.basis {
        detail.push(' ');
        detail.push_str(rationale);
    }
    if matches!(d.decision.basis, DecisionBasis::LevelShift { .. }) {
        detail.push_str(match lang {
            Lang::Ja => " 比較窓の分割は、変化の発生時刻を確定したものではありません。",
            Lang::En => " The window split does not fix when the change happened.",
        });
    }
    if d.baseline.may_reflect_the_anomaly() {
        detail.push_str(match lang {
            Lang::Ja => " 留保: 比較基準そのものが異変側に寄っている疑いがあります。",
            Lang::En => " Caveat: the comparison basis itself may lean toward the anomaly.",
        });
    }
    for reason in &assessed.priority_reasons {
        detail.push(' ');
        detail.push_str(reason);
    }
    Finding {
        series: keys.iter().position(|key| {
            key.activity == d.series.activity
                && key.item == d.series.item
                && key.column == d.series.column
        }),
        start: d.support.start_ust.to_string(),
        end: d.support.end_ust.to_string(),
        from_label: tz.datetime(d.support.start_ust),
        to_label: tz.datetime(d.support.end_ust),
        headline: assessed.headline.clone(),
        priority: assessed.priority.label().get(lang),
        evidence: assessed.sufficiency.level.label().get(lang),
        detail,
        background,
        references,
    }
}

fn unit_label(unit: Unit) -> &'static str {
    match unit {
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

/// JSON の文字列を HTML の script 要素に安全に埋め込む writer。
/// ホスト名・item 名などが終了タグを含んでも script を抜けられない。
struct ScriptData<'a, W>(&'a mut W);

impl<W: Write> Write for ScriptData<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut start = 0;
        for (i, byte) in bytes.iter().enumerate() {
            let replacement = match byte {
                b'<' => b"\\u003c".as_slice(),
                b'>' => b"\\u003e".as_slice(),
                b'&' => b"\\u0026".as_slice(),
                _ => continue,
            };
            self.0.write_all(&bytes[start..i])?;
            self.0.write_all(replacement)?;
            start = i + 1;
        }
        self.0.write_all(&bytes[start..])?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

/// 解析・検知済みのデータから、通信不要の HTML を書く。
/// assessment の並びは analysis のホスト・起動区間順と一致させる。
pub fn write_report<W: Write>(
    out: &mut W,
    analysis: &MultiFileAnalysis,
    assessments: &[Assessment],
    tz: DisplayTz,
    lang: Lang,
) -> io::Result<()> {
    let expected: usize = analysis.hosts.iter().map(|host| host.segments.len()).sum();
    if expected != assessments.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "assessment count does not match boot segments",
        ));
    }
    let mut segments = Vec::new();
    let mut assessments = assessments.iter();
    for host in &analysis.hosts {
        for segment in &host.segments {
            let assessment = assessments.next().expect("validated length");
            let timelines: Vec<_> = segment
                .summary
                .timelines
                .iter()
                .filter(|t| t.kind != ValueKind::Identity)
                .map(|t| (&t.key, t))
                .collect();
            let keys: Vec<_> = timelines.iter().map(|(key, _)| (*key).clone()).collect();
            let epochs: BTreeSet<_> = timelines
                .iter()
                .flat_map(|(_, timeline)| {
                    timeline
                        .points
                        .iter()
                        .flat_map(|p| [p.start_ust, p.end_ust])
                })
                .collect();
            let indices: BTreeMap<_, _> = epochs.iter().enumerate().map(|(i, t)| (*t, i)).collect();
            let times: Vec<_> = epochs
                .iter()
                .map(|&t| TimeLabel {
                    epoch: t.to_string(),
                    label: tz.datetime(t),
                    short: tz.month_day_time(t),
                    clock: tz.time(t),
                })
                .collect();
            let mut series = Vec::new();
            for (key, timeline) in &timelines {
                let mut previous = None;
                let points = timeline
                    .points
                    .iter()
                    .map(|p| {
                        let valid = p.reason.is_none() && p.value.is_some_and(f64::is_finite);
                        let reason = if p.value.is_some_and(|v| !v.is_finite()) {
                            Some(ExclusionReason::NotFinite)
                        } else {
                            p.reason
                        };
                        let connect = valid
                            && p.elapsed_cs > 0
                            && p.end_ust > p.start_ust
                            && previous == Some(p.start_ust);
                        previous = (valid && p.elapsed_cs > 0).then_some(p.end_ust);
                        (
                            indices[&p.start_ust],
                            indices[&p.end_ust],
                            p.value.filter(|_| valid),
                            reason,
                            connect,
                        )
                    })
                    .collect();
                let summary = segment.summary.column(key.activity, &key.item, &key.column);
                series.push(Series {
                    activity: key.activity.display_name(),
                    item: &key.item,
                    metric: &key.column,
                    label: summary.map_or(key.column.as_str(), |s| {
                        if s.sar_header.is_empty() {
                            key.column.as_str()
                        } else {
                            s.sar_header
                        }
                    }),
                    unit: unit_label(timeline.unit),
                    percent: timeline.unit == Unit::Percent,
                    origin: ObservationOrigin::of(key, timeline.kind).label().get(lang),
                    points,
                    summary,
                });
            }
            let findings = assessment
                .episodes
                .iter()
                .flat_map(|episode| {
                    episode
                        .episode
                        .detections
                        .iter()
                        .zip(&episode.detections)
                        .map(|(d, a)| finding(d, a, false, &keys, lang, tz))
                })
                .chain(
                    assessment
                        .background
                        .iter()
                        .map(|b| finding(&b.detection, &b.finding, true, &keys, lang, tz)),
                )
                .collect();
            segments.push(Segment {
                host: &host.identity.nodename,
                boot: segment.index,
                timezone: tz.label_at(epochs.first().copied().unwrap_or(0)),
                times,
                series,
                findings,
                evaluations: assessment
                    .coverage
                    .series
                    .iter()
                    .map(|evaluation| {
                        [
                            evaluation.series.display(),
                            evaluation.observed_samples.to_string(),
                            evaluation.missing_samples.to_string(),
                            evaluation.fixed_condition.label(lang),
                            evaluation.robust_deviation.label(lang),
                            evaluation.level_shift.label(lang),
                        ]
                    })
                    .collect(),
                assessment,
            });
        }
    }
    let report = Report {
        schema_version: crate::model::NATIVE_SCHEMA_VERSION,
        lang: lang.tag(),
        partial: !analysis.skipped.is_empty() || !analysis.incomplete_files.is_empty(),
        skipped: &analysis.skipped,
        incomplete: &analysis.incomplete_files,
        segments,
    };
    write!(out, "<!doctype html>\n<html lang=\"{}\">", lang.tag())?;
    out.write_all(include_bytes!("report/shell.html"))?;
    out.write_all(b"<style>")?;
    out.write_all(include_bytes!("report/style.css"))?;
    out.write_all(b"</style></head><body><script id=\"report-data\" type=\"application/json\">")?;
    serde_json::to_writer(ScriptData(out), &report).map_err(io::Error::other)?;
    out.write_all(b"</script><script>")?;
    out.write_all(include_bytes!("report/app.js"))?;
    out.write_all(b"</script></body></html>\n")
}

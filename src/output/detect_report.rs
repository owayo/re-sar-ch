//! 異変検出の所見の書式化。
//!
//! **この層では値を計算しない** (`docs/design.md` §2)。
//! [`crate::analyze::assessment::Assessment`] が持つ値と文をそのまま並べる。
//! 判定の言い回し (「20 分にわたる 3 回の採取」) も分析層が作ったものを使う。
//! 出力形式ごとに文が変わると、同じ所見が text と JSON で食い違う。
//!
//! # 3 形式の役割
//!
//! | 形式 | 用途 |
//! |---|---|
//! | `text` | 人が読む。優先度・根拠・留保を 1 画面で追える |
//! | `json` | エージェント向け。型のフィールドをそのまま出す (**確率値は無い**) |
//! | `ndjson` | 行単位で流す。エピソード 1 件 = 1 行 + 網羅度 1 行 |
//!
//! # 必ず出すもの
//!
//! - **比較基準の出所** — 入力自身から作ったことを毎回明示する
//! - **評価できなかった系列** — 「検出なし」と混同させない
//! - **確かめていないこと** — 観測事実と解釈の境界

use std::io::{self, Write};

use chrono::{TimeZone, Utc};

use crate::analyze::assessment::{
    ASSESSMENT_KIND, AssessedEpisode, Assessment, EvaluationCoverage, RouteStatus, RouteTally,
    describe_detection,
};
use crate::detect::episodes::Episode;
use crate::detect::{DETECT_SCHEMA_VERSION, DETECTOR_VERSION};
use crate::detect::{DecisionBasis, DetectRoute, Detection, Observation};

/// エポック秒を `YYYY-MM-DD HH:MM:SSZ` へ。
///
/// 独自出力は UTC / epoch で時刻を出す方針に合わせる。
fn epoch(ust: u64) -> String {
    match Utc.timestamp_opt(ust as i64, 0).single() {
        Some(dt) => dt.format("%Y-%m-%d %H:%M:%SZ").to_string(),
        None => ust.to_string(),
    }
}

/// 時刻だけ (`HH:MM:SS`)。観測列の表示に使う。
fn hms(ust: u64) -> String {
    match Utc.timestamp_opt(ust as i64, 0).single() {
        Some(dt) => dt.format("%H:%M:%S").to_string(),
        None => ust.to_string(),
    }
}

fn json_err(e: serde_json::Error) -> io::Error {
    io::Error::other(e)
}

// ===========================================================================
// text
// ===========================================================================

/// 人が読む形式。
pub fn write_text<W: Write>(out: &mut W, a: &Assessment) -> io::Result<()> {
    write_header(out, a)?;

    if a.episodes.is_empty() {
        writeln!(out)?;
        writeln!(
            out,
            "エピソードなし (報告範囲・優先度の下限で絞った結果を含む)"
        )?;
    }
    for e in &a.episodes {
        writeln!(out)?;
        write_episode(out, e)?;
    }

    writeln!(out)?;
    write_coverage(out, &a.coverage)?;

    writeln!(out)?;
    writeln!(out, "注意")?;
    for n in a.notes {
        writeln!(out, "  - {n}")?;
    }
    Ok(())
}

fn write_header<W: Write>(out: &mut W, a: &Assessment) -> io::Result<()> {
    let s = &a.source;
    writeln!(
        out,
        "異変検出: {} ({} / {}, {} CPU)",
        if s.label.is_empty() { "-" } else { &s.label },
        s.release.as_deref().unwrap_or("-"),
        s.machine.as_deref().unwrap_or("-"),
        s.cpu_nr.map_or("?".to_string(), |n| n.to_string())
    )?;
    if !s.files.is_empty() {
        writeln!(out, "files: {}", s.files.join(", "))?;
    }
    let p = &a.period;
    writeln!(
        out,
        "期間: {} → {}  ({} サンプル / 連続 {} 区間 / 不連続 {} 区間)",
        p.first_ust.map_or("-".to_string(), epoch),
        p.last_ust.map_or("-".to_string(), epoch),
        p.samples,
        p.continuous_intervals,
        p.broken_intervals
    )?;
    writeln!(
        out,
        "比較基準: {}  採取間隔の代表値: {}  エピソード結合: {} 秒以内",
        a.baseline_basis.label(),
        a.interval_p90_secs
            .map_or("不明".to_string(), |i| format!("{i} 秒")),
        a.episode_gap_secs
    )?;
    writeln!(
        out,
        "検出器: {}  カタログ: {}  逸脱閾値: MAD の {} 倍  水準変化: 窓 {} 秒 / 正規化 {} 倍 / 持続 {:.0}%",
        a.detector_version,
        a.coverage.catalog_version,
        a.thresholds.deviation_ratio,
        a.thresholds.shift_window_secs,
        a.thresholds.shift_normalized,
        a.thresholds.shift_persistence_share * 100.0
    )?;
    writeln!(out, "所見: {}", crate::analyze::describe_assessment(a))
}

fn write_episode<W: Write>(out: &mut W, e: &AssessedEpisode) -> io::Result<()> {
    let ep = &e.episode;
    writeln!(
        out,
        "エピソード {}  {} → {}",
        ep.index + 1,
        epoch(ep.support.start_ust),
        epoch(ep.support.end_ust)
    )?;
    writeln!(out, "  {} {}", e.priority.mark(), e.headline)?;
    writeln!(
        out,
        "     優先度: {} (下地 {})",
        e.priority.label(),
        e.base_priority.label()
    )?;
    for r in &e.priority_reasons {
        writeln!(out, "       - {r}")?;
    }
    let s = &e.sufficiency;
    writeln!(
        out,
        "     根拠の充足度: {} (基準 {} 採取 / 検出 {} 採取 / 欠測 {} / 不連続 {})",
        s.level.label(),
        s.baseline_samples,
        s.detected_samples,
        s.missing_samples,
        s.discontinuities
    )?;
    if s.basis_may_reflect_the_anomaly {
        writeln!(
            out,
            "       ! 比較基準が異変側へ寄っている疑いがある (入力自身が材料のため)"
        )?;
    }
    // **「独立な裏付けが N 個」と書かない。** 観点として並べるだけ。
    writeln!(out, "     観点: {}", e.viewpoints.join(", "))?;
    if ep.max_viewpoints_on_one_series > 1 {
        writeln!(
            out,
            "       (同じ系列に {} つの観点が当たった。3 経路は相関するので独立な裏付けの数ではない)",
            ep.max_viewpoints_on_one_series
        )?;
    }

    write_series_rollup(out, ep)?;
    write_detections(out, ep)?;

    if !e.possible_interpretations.is_empty() {
        writeln!(out, "     考えられる解釈 (どれとも断定しない)")?;
        for i in &e.possible_interpretations {
            writeln!(out, "       - {i}")?;
        }
    }
    if !e.not_established.is_empty() {
        writeln!(out, "     この所見では確かめていないこと")?;
        for n in &e.not_established {
            writeln!(out, "       - {n}")?;
        }
    }
    Ok(())
}

/// エピソード内で列挙する系列数の上限。
const MAX_LISTED_SERIES_IN_EPISODE: usize = 12;

/// エピソード内で根拠まで書き出す検出数の上限。
///
/// 負荷の高いホストでは 1 日で 100 件を超えることがあり、全件の根拠を
/// 並べると読めない。**件数は必ず出し**、詳細だけを打ち切る。
const MAX_DETAILED_DETECTIONS: usize = 6;

/// 関わった系列の一覧 (件数と観点)。
///
/// 検出を全件並べる前に「何が動いたか」を 1 望できるようにする。
fn write_series_rollup<W: Write>(out: &mut W, ep: &Episode) -> io::Result<()> {
    // (系列, 指標名) → (件数, 観点)
    let mut rows: Vec<(String, &'static str, usize, Vec<&'static str>)> = Vec::new();
    for d in &ep.detections {
        let key = d.series.display();
        match rows.iter_mut().find(|(k, _, _, _)| *k == key) {
            Some((_, _, n, routes)) => {
                *n += 1;
                let label = d.route().label();
                if !routes.contains(&label) {
                    routes.push(label);
                }
            }
            None => rows.push((key, d.metric_label, 1, vec![d.route().label()])),
        }
    }
    // 件数の多い順 → 系列名順 (決定的にする)
    rows.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));

    writeln!(out, "     関わった系列 ({})", rows.len())?;
    for (series, label, count, routes) in rows.iter().take(MAX_LISTED_SERIES_IN_EPISODE) {
        writeln!(
            out,
            "       {:<30} {:<22} {count} 件 [{}]",
            series,
            label,
            routes.join(", ")
        )?;
    }
    let rest = rows.len().saturating_sub(MAX_LISTED_SERIES_IN_EPISODE);
    if rest > 0 {
        writeln!(out, "       … 他 {rest} 系列")?;
    }
    Ok(())
}

/// 検出の根拠を書き出す。
///
/// 件数が多い場合は**系列ごとに 1 件**へ絞る。同じ系列の区間を
/// 優先度順に並べると `pgscand` の 18 件が全枠を埋めてしまい、
/// 他に何が起きていたか分からなくなる。
fn write_detections<W: Write>(out: &mut W, ep: &Episode) -> io::Result<()> {
    let total = ep.detections.len();
    if total <= MAX_DETAILED_DETECTIONS {
        writeln!(out, "     検出の根拠 (全 {total} 件)")?;
        // 時刻順のまま出す (エピソードの並びと一致させる)
        for d in &ep.detections {
            write_detection(out, d)?;
        }
        return Ok(());
    }

    // 系列ごとの代表 (優先度の下地が高く、裏付けの採取回数が多いもの)
    let mut best: Vec<&Detection> = Vec::new();
    for d in &ep.detections {
        match best.iter_mut().find(|b| b.series == d.series) {
            Some(b) => {
                let better =
                    (d.base_priority, d.support.samples) > (b.base_priority, b.support.samples);
                if better {
                    *b = d;
                }
            }
            None => best.push(d),
        }
    }
    best.sort_by(|a, b| {
        b.base_priority
            .cmp(&a.base_priority)
            .then_with(|| b.support.samples.cmp(&a.support.samples))
            .then_with(|| a.series.cmp(&b.series))
            .then_with(|| a.route().cmp(&b.route()))
    });
    let shown = best.len().min(MAX_DETAILED_DETECTIONS);
    writeln!(
        out,
        "     検出の根拠 (全 {total} 件。系列ごとの代表を {shown} 件だけ示す。\
         全件は --format json / --format ndjson で出る)"
    )?;
    for d in best.into_iter().take(MAX_DETAILED_DETECTIONS) {
        write_detection(out, d)?;
    }
    Ok(())
}

fn write_detection<W: Write>(out: &mut W, d: &Detection) -> io::Result<()> {
    writeln!(
        out,
        "     ・[{}] {}",
        d.route().label(),
        describe_detection(d)
    )?;
    writeln!(
        out,
        "         系列 {} / {} / 形 {}",
        d.series.display(),
        d.origin.label(),
        d.pattern.label()
    )?;
    match &d.decision.basis {
        DecisionBasis::FixedCondition {
            condition_id,
            rationale,
            min_samples,
            ..
        } => {
            writeln!(
                out,
                "         条件 {condition_id} (連続 {min_samples} 回以上): {rationale}"
            )?;
        }
        DecisionBasis::RobustDeviation {
            ratio_threshold,
            min_absolute_deviation,
            peak_absolute_deviation,
            ..
        } => {
            writeln!(
                out,
                "         閾値 MAD の {ratio_threshold} 倍以上、かつ絶対差 {min_absolute_deviation:.2} 以上 (実測 {peak_absolute_deviation:.2})"
            )?;
        }
        DecisionBasis::LevelShift {
            min_shift,
            pooled_mad,
            normalized_threshold,
            persistence_share,
            persistence_threshold,
            ..
        } => {
            writeln!(
                out,
                "         最小変化量 {min_shift:.2} / 窓内の散らばり {} / 正規化閾値 {normalized_threshold} 倍 / 持続 {:.0}% (要 {:.0}%)",
                pooled_mad.map_or("測れない".to_string(), |m| format!("{m:.2}")),
                persistence_share * 100.0,
                persistence_threshold * 100.0
            )?;
        }
    }
    write_observations(
        out,
        &d.decision.observations,
        d.decision.observations_truncated,
    )?;
    write_baseline(out, d)
}

fn write_observations<W: Write>(
    out: &mut W,
    observations: &[Observation],
    truncated: bool,
) -> io::Result<()> {
    if observations.is_empty() {
        return Ok(());
    }
    let mut line = String::from("         観測:");
    for o in observations {
        line.push_str(&format!(" {}={:.2}", hms(o.end_ust), o.value));
    }
    if truncated {
        line.push_str(" … (以降は省略)");
    }
    writeln!(out, "{line}")
}

fn write_baseline<W: Write>(out: &mut W, d: &Detection) -> io::Result<()> {
    let b = &d.baseline;
    writeln!(
        out,
        "         比較基準: 中央値 {} / MAD {} / 材料 {} 採取 / 散らばり {} — {}",
        b.median.map_or("-".to_string(), |v| format!("{v:.2}")),
        b.mad.map_or("-".to_string(), |v| format!("{v:.2}")),
        b.samples,
        b.dispersion.label(),
        b.basis.label()
    )?;
    if b.flagged_share > 0.0 {
        writeln!(
            out,
            "           材料のうち固定条件を満たした割合: {:.0}%",
            b.flagged_share * 100.0
        )?;
    }
    for c in &b.caveats {
        writeln!(out, "           ! {c}")?;
    }
    Ok(())
}

fn write_coverage<W: Write>(out: &mut W, c: &EvaluationCoverage) -> io::Result<()> {
    writeln!(out, "評価の網羅度")?;
    writeln!(
        out,
        "  カタログ {} パターン / 入力にあった系列 {} / 評価できた系列 {} / 評価できなかった系列 {}",
        c.patterns_in_catalog, c.series_present, c.series_evaluated, c.series_not_evaluated
    )?;
    for (route, tally) in [
        (DetectRoute::FixedCondition, &c.fixed_condition),
        (DetectRoute::RobustDeviation, &c.robust_deviation),
        (DetectRoute::LevelShift, &c.level_shift),
    ] {
        write_tally(out, route, tally)?;
    }

    write_blocked(out, c)
}

/// 系列ごとに列挙できる件数の上限。
///
/// 1 日分のファイルでもデバイス数 × 列数で数百行になる。
/// **件数は必ず出し**、名前の列挙だけを打ち切る。
const MAX_LISTED_SERIES: usize = 3;

/// 評価できなかったものを理由ごとにまとめて出す。
///
/// **「評価できなかった」は「検出なし」ではない。** 理由と件数を必ず出す。
/// 系列を 1 行ずつ並べると数百行になるので、理由でまとめて代表名だけ挙げる。
fn write_blocked<W: Write>(out: &mut W, c: &EvaluationCoverage) -> io::Result<()> {
    // (観点, 理由) → 該当系列
    let mut groups: Vec<((&'static str, &'static str), Vec<String>)> = Vec::new();
    for e in c.blocked() {
        for (route, status) in [
            (DetectRoute::FixedCondition, e.fixed_condition),
            (DetectRoute::RobustDeviation, e.robust_deviation),
            (DetectRoute::LevelShift, e.level_shift),
        ] {
            let RouteStatus::NotEvaluated { reason } = status else {
                continue;
            };
            if reason.is_by_design() {
                continue;
            }
            let key = (route.label(), reason.label());
            match groups.iter_mut().find(|(k, _)| *k == key) {
                Some((_, v)) => v.push(e.series.display()),
                None => groups.push((key, vec![e.series.display()])),
            }
        }
    }
    if groups.is_empty() {
        return Ok(());
    }
    writeln!(
        out,
        "  評価できなかったもの (データの制約による。これは「検出なし」ではない)"
    )?;
    for ((route, reason), series) in &groups {
        let shown: Vec<&str> = series
            .iter()
            .take(MAX_LISTED_SERIES)
            .map(String::as_str)
            .collect();
        let rest = series.len().saturating_sub(shown.len());
        let tail = if rest > 0 {
            format!(", 他 {rest} 系列")
        } else {
            String::new()
        };
        writeln!(
            out,
            "    [{route}] {reason}: {} 系列 ({}{tail})",
            series.len(),
            shown.join(", ")
        )?;
    }
    Ok(())
}

fn write_tally<W: Write>(out: &mut W, route: DetectRoute, t: &RouteTally) -> io::Result<()> {
    writeln!(
        out,
        "  {:<20} 検出 {} 系列 ({} 件) / 評価済み {} 系列 / 対象外 {} 系列 / 評価不能 {} 系列",
        route.label(),
        t.detected_series,
        t.detections,
        t.evaluated_series,
        t.not_applicable_series,
        t.blocked_series
    )
}

// ===========================================================================
// json / ndjson
// ===========================================================================

/// エージェント向け。**型のフィールドをそのまま出す (確率値は無い)。**
///
/// **起動区間ごとの所見を 1 つのドキュメントへ収める。**
/// 区間ごとに独立した JSON を並べると、先頭の 1 件しか読めない
/// ドキュメント列になってしまう (`summarize --format json` と同じ方針)。
pub fn write_json<W: Write>(out: &mut W, assessments: &[Assessment]) -> io::Result<()> {
    let doc = serde_json::json!({
        "schema_version": DETECT_SCHEMA_VERSION,
        "assessment_kind": ASSESSMENT_KIND,
        "detector_version": DETECTOR_VERSION,
        // 起動区間ごとに 1 件。区間をまたいだ検出はしない
        "assessments": assessments,
    });
    serde_json::to_writer_pretty(&mut *out, &doc).map_err(json_err)?;
    writeln!(out)
}

/// 行単位。起動区間ごとにヘッダ 1 行 + エピソード 1 件 = 1 行 + 網羅度 1 行。
///
/// どの行がどの起動区間のものかを `segment` で示す。
pub fn write_ndjson<W: Write>(out: &mut W, assessments: &[Assessment]) -> io::Result<()> {
    for (segment, a) in assessments.iter().enumerate() {
        let head = serde_json::json!({
            "schema_version": a.schema_version,
            "record": "detect_header",
            "segment": segment,
            "assessment_kind": a.assessment_kind,
            "detector_version": a.detector_version,
            "catalog_version": a.coverage.catalog_version,
            "thresholds": a.thresholds,
            "baseline_basis": a.baseline_basis,
            "episode_gap_secs": a.episode_gap_secs,
            "interval_p90_secs": a.interval_p90_secs,
            "source": a.source,
            "period": a.period,
            "notes": a.notes,
        });
        serde_json::to_writer(&mut *out, &head).map_err(json_err)?;
        writeln!(out)?;

        for e in &a.episodes {
            let row = serde_json::json!({
                "schema_version": a.schema_version,
                "record": "episode",
                "segment": segment,
                "episode": e,
            });
            serde_json::to_writer(&mut *out, &row).map_err(json_err)?;
            writeln!(out)?;
        }

        let cov = serde_json::json!({
            "schema_version": a.schema_version,
            "record": "coverage",
            "segment": segment,
            "coverage": a.coverage,
        });
        serde_json::to_writer(&mut *out, &cov).map_err(json_err)?;
        writeln!(out)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze::assessment::{Priority, assess};
    use crate::analyze::summary::{PeriodBounds, SummarySource};
    use crate::detect::testing::*;
    use crate::detect::{DetectOptions, detect};

    fn assessment(values: &[f64]) -> Assessment {
        let ts = single(cpu_idle(&vals(values)));
        let opts = DetectOptions::default();
        let outcome = detect(&ts, &opts);
        assess(
            outcome,
            SummarySource {
                label: "example".to_string(),
                ..Default::default()
            },
            PeriodBounds::default(),
            &opts,
        )
    }

    fn render(a: &Assessment) -> String {
        let mut buf: Vec<u8> = Vec::new();
        write_text(&mut buf, a).expect("書き出し");
        String::from_utf8(buf).expect("UTF-8")
    }

    #[test]
    fn the_text_report_always_states_where_the_basis_came_from() {
        let mut v = vec![80.0; 20];
        v[10] = 2.0;
        v[11] = 2.0;
        let a = assessment(&v);
        let text = render(&a);
        assert!(text.contains("この入力自身が材料"), "{text}");
        assert!(text.contains("外部の正常値ではない"), "{text}");
        // 「正常値」を基準の名前として使っていない
        assert!(!text.contains("正常値:"), "{text}");
    }

    #[test]
    fn the_text_report_lists_what_could_not_be_evaluated() {
        let a = assessment(&[50.0; 20]);
        let text = render(&a);
        assert!(text.contains("評価の網羅度"), "{text}");
        assert!(text.contains("評価できなかった系列"), "{text}");
    }

    #[test]
    fn an_episode_shows_priority_reasons_and_sufficiency_separately() {
        let mut v = vec![80.0; 40];
        for x in v.iter_mut().take(25).skip(20) {
            *x = 1.0;
        }
        let a = assessment(&v);
        let text = render(&a);
        assert!(text.contains("優先度:"), "{text}");
        assert!(text.contains("根拠の充足度:"), "{text}");
        // **確率値を出さない。** 「確率は出さない」という注意書き自体は出る
        assert!(!text.contains("確信度:"), "{text}");
        assert!(!text.contains("確率:"), "{text}");
        assert!(
            text.contains("確率や確信度は出さない"),
            "出さないことを明示する: {text}"
        );
        // 優先度と充足度は別の行 (1 つのスコアへ潰していない)
        let priority_line = text
            .lines()
            .find(|l| l.contains("優先度:"))
            .expect("優先度の行");
        assert!(
            !priority_line.contains("充足度"),
            "優先度と充足度を同じ行に混ぜない: {priority_line}"
        );
    }

    #[test]
    fn viewpoints_are_never_called_independent() {
        let mut v = vec![80.0; 40];
        for x in v.iter_mut().take(25).skip(20) {
            *x = 1.0;
        }
        let a = assessment(&v);
        let text = render(&a);
        assert!(text.contains("観点:"), "{text}");
        assert!(!text.contains("独立な裏付け 2"), "{text}");
    }

    #[test]
    fn json_carries_the_fields_without_a_confidence_value() {
        let mut v = vec![80.0; 20];
        v[10] = 2.0;
        v[11] = 2.0;
        let a = assessment(&v);
        let mut buf: Vec<u8> = Vec::new();
        // 起動区間が 2 つあっても 1 つのドキュメントに収まること
        write_json(&mut buf, &[a.clone(), a]).expect("JSON");
        let text = String::from_utf8(buf).expect("UTF-8");
        let parsed: serde_json::Value = serde_json::from_str(&text).expect("パース");
        assert_eq!(parsed["assessment_kind"], "resarch_detect_assessment");
        assert_eq!(parsed["assessments"].as_array().map(Vec::len), Some(2));
        let first = &parsed["assessments"][0];
        assert_eq!(first["baseline_basis"], "input_itself");
        assert!(first["episodes"].is_array());
        assert!(first["coverage"]["series"].is_array());
        assert!(!text.contains("confidence"));
        assert!(!text.contains("probability"));
    }

    #[test]
    fn ndjson_emits_one_line_per_episode_plus_header_and_coverage() {
        let mut v = vec![80.0; 20];
        v[10] = 2.0;
        v[11] = 2.0;
        let a = assessment(&v);
        let mut buf: Vec<u8> = Vec::new();
        write_ndjson(&mut buf, std::slice::from_ref(&a)).expect("NDJSON");
        let text = String::from_utf8(buf).expect("UTF-8");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2 + a.episodes.len());
        let head: serde_json::Value = serde_json::from_str(lines[0]).expect("パース");
        assert_eq!(head["record"], "detect_header");
        assert_eq!(head["segment"], 0, "どの起動区間の行か分かること");
        let last: serde_json::Value = serde_json::from_str(lines[lines.len() - 1]).expect("パース");
        assert_eq!(last["record"], "coverage");
    }

    #[test]
    fn an_empty_assessment_says_so_rather_than_printing_nothing() {
        let mut a = assessment(&[50.0; 20]);
        a.filter_priority(Priority::Investigate);
        let text = render(&a);
        assert!(text.contains("エピソードなし"), "{text}");
    }
}

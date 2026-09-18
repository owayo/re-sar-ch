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

use crate::analyze::assessment::{
    ASSESSMENT_KIND, AssessedEpisode, Assessment, EvaluationCoverage, RouteStatus, RouteTally,
    describe_detection,
};
use crate::detect::episodes::Episode;
use crate::detect::{
    BASELINE_CAVEAT_MAD_ZERO, BASELINE_CAVEAT_SELF_SOURCED, BASELINE_CAVEAT_TOO_SPARSE,
    DecisionBasis, DetectRoute, Detection, Observation,
};
use crate::detect::{DETECT_SCHEMA_VERSION, DETECTOR_VERSION};
use crate::model::DisplayTz;
use crate::model::{Lang, Text, count_en};
use crate::text;

/// タイムゾーン名を決めるときの基準時刻。
///
/// 夏時間のある地域ではオフセットが時期で変わるので、
/// **入力の先頭サンプル**に合わせる (実行時刻ではない)。
fn report_anchor(assessments: &[Assessment]) -> u64 {
    assessments
        .iter()
        .find_map(|a| a.period.first_ust)
        .unwrap_or(0)
}

fn json_err(e: serde_json::Error) -> io::Error {
    io::Error::other(e)
}

// ===========================================================================
// text
// ===========================================================================

/// `text` の詳しさ。
///
/// **省くのは `text` だけ。** `json` / `ndjson` は指定によらず全フィールドを出すので、
/// 要約で落とした根拠は機械可読形式から取れる。
///
/// 要約でも落とさないものがある。**その所見に固有の留保**
/// (比較基準が異変側へ寄っている疑い) と、**評価の網羅度**、**注意**である。
/// 前者は規律 3、後者 2 つは規律 7 と「観測と解釈の境界」がかかっている
/// (`docs/design.md` §11.2)。落としてよいのは、検出パターンが決まれば
/// 内容も決まる固定文と、機械可読形式に同じものがある内訳だけ。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Detail {
    /// 既定。エピソードの見出し・優先度・関わった系列・検出の要約まで。
    #[default]
    Summary,
    /// 検出 1 件ごとの内訳と、検出パターンごとの解釈・留保まで出す。
    Full,
}

impl Detail {
    fn is_full(self) -> bool {
        matches!(self, Self::Full)
    }
}

/// 人が読む形式。
pub fn write_text<W: Write>(
    out: &mut W,
    a: &Assessment,
    tz: DisplayTz,
    detail: Detail,
) -> io::Result<()> {
    let lang = a.lang;
    write_header(out, a, tz)?;

    if a.episodes.is_empty() {
        writeln!(out)?;
        writeln!(
            out,
            "{}",
            text!(
                ja: "エピソードなし (報告範囲・優先度の下限で絞った結果を含む)",
                en: "No episodes (this includes what the report window and the priority \
                     floor filtered out)",
            )
            .get(lang)
        )?;
    }
    if detail.is_full() {
        for e in &a.episodes {
            writeln!(out)?;
            write_episode(out, e, tz, detail, lang)?;
        }
    } else {
        write_episode_groups(out, a, tz)?;
    }
    // **省いたことを黙らない。** 出ていない情報があると分からなければ、
    // 読み手は「この所見にはこれしか根拠が無い」と読む。
    if !detail.is_full() && !a.episodes.is_empty() {
        writeln!(out)?;
        writeln!(
            out,
            "{}",
            text!(
                ja: "エピソード 1 件ずつの根拠 (観測値・比較基準・窓) と考えられる解釈は省いた \
                     (--verbose で出る。全フィールドは --format json / --format ndjson)",
                en: "Per-episode evidence (observed values, comparison basis, windows) and the \
                     interpretations are omitted (--verbose shows them; --format json / \
                     --format ndjson carry every field)",
            )
            .get(lang)
        )?;
    }

    writeln!(out)?;
    write_background(out, a, tz, detail)?;

    // 要約ではエピソード本文から外した分をここで 1 度だけ出す。
    // **落とさずに位置を変えているだけ**である (規律 21 / 22)。
    if !detail.is_full() {
        write_not_established(out, a)?;
    }

    writeln!(out)?;
    write_coverage(out, &a.coverage, lang)?;

    writeln!(out)?;
    writeln!(out, "{}", text!(ja: "注意", en: "Notes").get(lang))?;
    for n in &a.notes {
        writeln!(out, "  - {n}")?;
    }
    Ok(())
}

fn write_header<W: Write>(out: &mut W, a: &Assessment, tz: DisplayTz) -> io::Result<()> {
    let lang = a.lang;
    let s = &a.source;
    let host = if s.label.is_empty() { "-" } else { &s.label };
    let (release, machine) = (
        s.release.as_deref().unwrap_or("-"),
        s.machine.as_deref().unwrap_or("-"),
    );
    let cpus = s.cpu_nr.map_or("?".to_string(), |n| n.to_string());
    writeln!(
        out,
        "{}: {host} ({release} / {machine}, {cpus} CPU)",
        text!(ja: "異変検出", en: "Anomaly detection").get(lang)
    )?;
    if !s.files.is_empty() {
        writeln!(out, "files: {}", s.files.join(", "))?;
    }
    let p = &a.period;
    let (first, last) = (
        p.first_ust.map_or("-".to_string(), |ust| tz.datetime(ust)),
        p.last_ust.map_or("-".to_string(), |ust| tz.datetime(ust)),
    );
    let (samples, cont, broken) = (p.samples, p.continuous_intervals, p.broken_intervals);
    writeln!(
        out,
        "{}",
        match lang {
            Lang::Ja => format!(
                "期間: {first} → {last}  \
                 ({samples} サンプル / 連続 {cont} 区間 / 不連続 {broken} 区間)"
            ),
            Lang::En => format!(
                "Period: {first} → {last}  ({} / {} / {})",
                count_en(samples, "sample", "samples"),
                count_en(cont, "continuous stretch", "continuous stretches"),
                count_en(broken, "break", "breaks"),
            ),
        }
    )?;
    let basis = a.baseline_basis.label().get(lang);
    let interval = a.interval_p90_secs.map_or_else(
        || text!(ja: "不明", en: "unknown").get(lang).to_string(),
        |i| match lang {
            Lang::Ja => format!("{i} 秒"),
            Lang::En => format!("{i}s"),
        },
    );
    let gap = a.episode_gap_secs;
    let cap = if a.episode_onset_span_cap_secs == u64::MAX {
        text!(ja: "なし", en: "none").get(lang).to_string()
    } else {
        match lang {
            Lang::Ja => format!("{} 秒", a.episode_onset_span_cap_secs),
            Lang::En => format!("{}s", a.episode_onset_span_cap_secs),
        }
    };
    writeln!(
        out,
        "{}",
        match lang {
            Lang::Ja => format!(
                "比較基準: {basis}  採取間隔の代表値: {interval}  \
                 エピソード結合: 始まりが {gap} 秒以内 (広がりの上限 {cap})"
            ),
            Lang::En => format!(
                "Comparison basis: {basis}  Typical sampling interval: {interval}  \
                 Episode grouping: onsets within {gap}s (spread capped at {cap})"
            ),
        }
    )?;
    let s = &a.report_scope;
    // `hh:mm:ss` 指定を**どの壁時計として読んだか**を添える。これが無いと、
    // 報告範囲と画面の時刻が同じ基準かどうか読み手に分からない。
    // epoch 秒と「指定なし」はタイムゾーンによらないので添えない。
    let scope_tz = if s.from.is_time_of_day() || s.to.is_time_of_day() {
        format!(" ({})", tz.label_at(a.period.first_ust.unwrap_or(0)))
    } else {
        String::new()
    };
    let (from, to) = (s.from.label(lang), s.to.label(lang));
    let min = s.min_priority.label().get(lang);
    let standing = a.standing_span_percent;
    writeln!(
        out,
        "{}",
        match lang {
            Lang::Ja => format!(
                "報告範囲: {from} → {to}{scope_tz}  最低優先度: {min}  \
                 背景の所見に回す割合: 入力の {standing}% 以上"
            ),
            Lang::En => format!(
                "Report window: {from} → {to}{scope_tz}  Minimum priority: {min}  \
                 Moved to standing findings at: {standing}% or more of the input"
            ),
        }
    )?;
    let (d_in, d_win, bg, eps, dropped) = (
        s.detections_in_input,
        s.detections_in_report_window,
        s.background_findings,
        s.episodes_before_priority_filter,
        s.episodes_excluded_by_priority,
    );
    writeln!(
        out,
        "{}",
        match lang {
            Lang::Ja => format!(
                "件数: 入力全体の検出 {d_in} 件 / 報告範囲の検出 {d_win} 件 / \
                 背景の所見 {bg} 件 / エピソード {eps} 件 (優先度で除外 {dropped} 件)"
            ),
            Lang::En => format!(
                "Counts: {} across the input / {d_win} within the report window / \
                 {} / {} ({dropped} dropped by priority)",
                count_en(d_in as u64, "detection", "detections"),
                count_en(bg as u64, "standing finding", "standing findings"),
                count_en(eps as u64, "episode", "episodes"),
            ),
        }
    )?;
    // **窓幅は「要求した幅」であって実効幅ではない。** 実効幅は
    // 採取間隔と点数の下限で決まるので、検出ごとに根拠へ出す
    // (600 秒採取では要求 1800 秒に対して実効 3000 秒になる)。
    let (detector, catalog) = (a.detector_version, a.coverage.catalog_version);
    let t = &a.thresholds;
    let (ratio, window, normalized, persistence) = (
        t.deviation_ratio,
        t.shift_window_secs,
        t.shift_normalized,
        t.shift_persistence_share * 100.0,
    );
    writeln!(
        out,
        "{}",
        match lang {
            Lang::Ja => format!(
                "検出器: {detector}  カタログ: {catalog}  逸脱閾値: MAD の {ratio} 倍  \
                 水準変化: 要求窓 {window} 秒 (実効幅は検出ごと) / 正規化 {normalized} 倍 / \
                 持続 {persistence:.0}%"
            ),
            Lang::En => format!(
                "Detector: {detector}  Catalog: {catalog}  Deviation threshold: {ratio}x MAD  \
                 Level change: {window}s requested (effective width is per detection) / \
                 {normalized}x normalised / {persistence:.0}% persistence"
            ),
        }
    )?;
    writeln!(
        out,
        "{}: {}",
        text!(ja: "所見", en: "Assessment").get(lang),
        crate::analyze::describe_assessment(a)
    )
}

/// 1 つの系列にまとめたエピソード群。
struct SeriesGroup<'a> {
    series: &'a crate::detect::SeriesKey,
    metric_label: &'static str,
    episodes: Vec<&'a AssessedEpisode>,
}

/// 1 系列あたりで時刻を並べるエピソード数の上限。
///
/// **件数は必ず出し**、時刻の列挙だけを打ち切る。
const MAX_LISTED_ONSETS: usize = 8;

/// 1 行に並べる時刻の数 (日付を省けるとき / 省けないとき)。
const ONSETS_PER_LINE: usize = 3;
const ONSETS_PER_LINE_WITH_DATE: usize = 2;

/// エピソードを**見出しの系列ごとに**まとめて書き出す (要約用)。
///
/// # なぜまとめるのか
///
/// エピソードは検出の**始まり**でまとめるので、同じ系列が 1 日を通じて
/// 散発的に鳴ると、その回数だけエピソードができる (実測で `pgscand` が 18 件、
/// ディスクの `tps` が 15 件、合計 111 エピソード)。1 件ずつ並べると
/// 「何が鳴っているか」が 1400 行に薄まる。系列でまとめると
/// **「何が」が先に見え、「いつ」はその下に時刻として残る**。
///
/// # 失わないもの
///
/// - **いつ** — エピソードごとの範囲を時刻として並べる (打ち切ったら件数を出す)
/// - **優先度** — 系列の最高優先度を見出しに、各時刻にもその回の優先度を添える
/// - **同時に鳴った別の系列** — `+N 系列` として時刻の後ろに出す。
///   複数系列が重なったエピソードは事象らしさの手がかりなので、埋めてはいけない
///
/// 1 件ずつの根拠は `--verbose` で従来どおり出る。
fn write_episode_groups<W: Write>(out: &mut W, a: &Assessment, tz: DisplayTz) -> io::Result<()> {
    let mut groups: Vec<SeriesGroup<'_>> = Vec::new();
    for e in &a.episodes {
        match groups.iter_mut().find(|g| *g.series == e.headline_series) {
            Some(g) => g.episodes.push(e),
            None => groups.push(SeriesGroup {
                series: &e.headline_series,
                metric_label: e.headline_metric_label,
                episodes: vec![e],
            }),
        }
    }
    // 優先度の高い順 → 件数の多い順 → 系列名順 (決定的にする)
    groups.sort_by(|x, y| {
        let px = x.episodes.iter().map(|e| e.priority).max();
        let py = y.episodes.iter().map(|e| e.priority).max();
        py.cmp(&px)
            .then_with(|| y.episodes.len().cmp(&x.episodes.len()))
            .then_with(|| x.series.cmp(y.series))
    });

    // 報告が 1 日に収まるなら時刻だけにする。**日付はヘッダの「期間」が持っている**ので、
    // 全行に繰り返すと時刻そのものが読み取りにくくなる。
    let same_day = match (a.period.first_ust, a.period.last_ust) {
        (Some(f), Some(l)) => tz.date(f) == tz.date(l),
        _ => false,
    };
    let per_line = if same_day {
        ONSETS_PER_LINE
    } else {
        ONSETS_PER_LINE_WITH_DATE
    };

    let lang = a.lang;
    for g in &groups {
        writeln!(out)?;
        let top = g
            .episodes
            .iter()
            .max_by_key(|e| e.priority)
            .expect("グループは空でない");
        let (mark, metric, series, n, priority) = (
            top.priority.mark(),
            g.metric_label,
            g.series.display(),
            g.episodes.len(),
            top.priority.label().get(lang),
        );
        writeln!(
            out,
            "{}",
            match lang {
                Lang::Ja => format!("{mark} {metric} [{series}] — {n} 件 (最高: {priority})"),
                Lang::En => format!(
                    "{mark} {metric} [{series}] — {} (highest: {priority})",
                    count_en(n as u64, "episode", "episodes")
                ),
            }
        )?;
        // 観点は系列単位の和集合。**「N 個の裏付け」とは書かない** (規律 2′)。
        let mut viewpoints: Vec<&'static str> = Vec::new();
        for v in g.episodes.iter().flat_map(|e| e.viewpoints.iter()) {
            if !viewpoints.contains(v) {
                viewpoints.push(v);
            }
        }
        writeln!(
            out,
            "     {}: {}",
            text!(ja: "観点", en: "Views").get(lang),
            viewpoints.join(", ")
        )?;

        for chunk in g
            .episodes
            .iter()
            .take(MAX_LISTED_ONSETS)
            .collect::<Vec<_>>()
            .chunks(per_line)
        {
            let cells: Vec<String> = chunk
                .iter()
                .map(|e| {
                    let ep = &e.episode;
                    // **同時に鳴った系列を隠さない。** 見出し以外の系列があれば数を添える。
                    let others = ep
                        .detections
                        .iter()
                        .filter(|d| d.series != e.headline_series)
                        .map(|d| &d.series)
                        .collect::<std::collections::BTreeSet<_>>()
                        .len();
                    let start = if same_day {
                        tz.time(ep.support.start_ust)
                    } else {
                        tz.datetime(ep.support.start_ust)
                    };
                    let end = tz.time(ep.support.end_ust);
                    let priority = e.priority.label().get(lang);
                    let n = ep.support.samples;
                    let extra = match (others, lang) {
                        (0, _) => String::new(),
                        (n, Lang::Ja) => format!(", +{n} 系列"),
                        (n, Lang::En) => format!(", +{n} series"),
                    };
                    match lang {
                        Lang::Ja => format!("{start}→{end} ({priority}, {n} 採取{extra})"),
                        Lang::En => format!(
                            "{start}→{end} ({priority}, {}{extra})",
                            count_en(n, "sample", "samples")
                        ),
                    }
                })
                .collect();
            writeln!(out, "       {}", cells.join("  "))?;
        }
        let rest = g.episodes.len().saturating_sub(MAX_LISTED_ONSETS);
        if rest > 0 {
            writeln!(
                out,
                "       {}",
                match lang {
                    Lang::Ja => format!("… 他 {rest} 件 (全件は --verbose / --format json)"),
                    Lang::En =>
                        format!("… and {rest} more (--verbose / --format json show all of them)"),
                }
            )?;
        }

        // 充足度は**エピソードごとに違う**。系列でまとめても最低と最高を出す。
        let levels: Vec<_> = g.episodes.iter().map(|e| e.sufficiency.level).collect();
        let lo = levels.iter().min().expect("空でない").label().get(lang);
        let hi = levels.iter().max().expect("空でない").label().get(lang);
        let sufficiency = text!(ja: "根拠の充足度", en: "Evidence sufficiency").get(lang);
        if lo == hi {
            writeln!(out, "     {sufficiency}: {hi}")?;
        } else {
            writeln!(
                out,
                "     {sufficiency}: {}",
                match lang {
                    Lang::Ja => format!("{lo}〜{hi} (件により違う)"),
                    Lang::En => format!("{lo} to {hi} (varies by episode)"),
                }
            )?;
        }
        // **系列に固有の留保は要約でも出す** (規律 3)。
        if g.episodes
            .iter()
            .any(|e| e.sufficiency.basis_may_reflect_the_anomaly)
        {
            writeln!(out, "       ! {}", BASIS_MAY_LEAN.get(lang))?;
        }
    }
    Ok(())
}

/// 比較基準がその系列の異変側へ寄っている疑い (規律 3)。
///
/// **要約でも落とさない。** その系列の逸脱判定が当てにならないという話で、
/// 注意書きの一般論では代用できない。
const BASIS_MAY_LEAN: Text = text!(
    ja: "比較基準が異変側へ寄っている疑いがある (入力自身が材料のため)",
    en: "the comparison basis may lean toward the anomaly (the input itself is its material)",
);

fn write_episode<W: Write>(
    out: &mut W,
    e: &AssessedEpisode,
    tz: DisplayTz,
    detail: Detail,
    lang: Lang,
) -> io::Result<()> {
    let ep = &e.episode;
    // **2 つの範囲を区別して出す。** 根拠が及ぶ範囲だけを出すと
    // 「その期間まるごとが 1 つの事象で、内側のエピソードはその一部」と
    // 読まれてしまう。エピソードは検出の**始まり**のまとまりである。
    let index = ep.index + 1;
    let (start, last_onset, end) = (
        tz.datetime(ep.support.start_ust),
        tz.datetime(ep.last_onset_ust),
        tz.datetime(ep.support.end_ust),
    );
    writeln!(
        out,
        "{}",
        match lang {
            Lang::Ja => format!("エピソード {index}  検出の始まり {start} 〜 {last_onset}"),
            Lang::En => format!("Episode {index}  Detections began {start} – {last_onset}"),
        }
    )?;
    writeln!(
        out,
        "{}",
        match lang {
            Lang::Ja => format!(
                "              根拠が及ぶ範囲 {start} → {end} (他のエピソードと重なることがある)"
            ),
            Lang::En =>
                format!("          Evidence covers {start} → {end} (may overlap other episodes)"),
        }
    )?;
    writeln!(out, "  {} {}", e.priority.mark(), e.headline)?;
    // **優先度は検出単位。** 見出しの検出について書いていることを明示する
    // (別の検出の持続性で上がったのではない)。
    //
    // 要約では**昇降があったときだけ**下地と理由を出す。下地のままなら
    // 「注視 (下地 注視) — 昇降なし」は同じことを 3 回言っている。
    let priority_label = text!(ja: "優先度", en: "Priority").get(lang);
    let moved = e.priority != e.base_priority;
    if detail.is_full() || moved {
        let (p, base) = (
            e.priority.label().get(lang),
            e.base_priority.label().get(lang),
        );
        writeln!(
            out,
            "     {priority_label}: {}",
            match lang {
                Lang::Ja => format!("{p} (下地 {base}) — この見出しの検出について"),
                Lang::En => format!("{p} (baseline {base}) — for the headline detection"),
            }
        )?;
        for r in &e.priority_reasons {
            writeln!(out, "       - {r}")?;
        }
    } else {
        writeln!(
            out,
            "     {priority_label}: {}",
            e.priority.label().get(lang)
        )?;
    }
    if let Some(longest) = &e.longest_running_headline {
        writeln!(
            out,
            "     {}: {longest}",
            text!(ja: "最も長く続いた検出", en: "Longest-running detection").get(lang)
        )?;
    }
    let s = &e.sufficiency;
    writeln!(
        out,
        "     {}: {}",
        text!(ja: "根拠の充足度", en: "Evidence sufficiency").get(lang),
        e.sufficiency_spread.label(lang)
    )?;
    let basis = s.basis.label().get(lang);
    let (material, required, baseline, detected, missing, breaks) = (
        s.material_samples,
        s.required_samples,
        s.baseline_samples,
        s.detected_samples,
        s.missing_samples,
        s.discontinuities,
    );
    writeln!(
        out,
        "       {}",
        match lang {
            Lang::Ja => format!(
                "内訳 ({basis}): {material} 採取 / 要 {required} 採取 — 基準 {baseline} 採取 / \
                 検出 {detected} 採取 / 欠測 {missing} / 不連続 {breaks}"
            ),
            Lang::En => format!(
                "Breakdown ({basis}): {material} samples / {required} required — \
                 {baseline} in the basis / {detected} detected / {missing} missing / \
                 {breaks} discontinuities"
            ),
        }
    )?;
    // **この留保は要約でも落とさない。** その系列の逸脱判定が当てにならない
    // という話で、注意書きの一般論では代用できない (規律 3)。
    if s.basis_may_reflect_the_anomaly {
        writeln!(out, "       ! {}", BASIS_MAY_LEAN.get(lang))?;
    }
    // **「独立な裏付けが N 個」と書かない。** 観点として並べるだけ。
    writeln!(
        out,
        "     {}: {}",
        text!(ja: "観点", en: "Views").get(lang),
        e.viewpoints.join(", ")
    )?;
    // 相関することは末尾の注意が言うので、要約では繰り返さない。
    if detail.is_full() {
        if ep.max_viewpoints_on_one_series > 1 {
            let n = ep.max_viewpoints_on_one_series;
            writeln!(
                out,
                "       {}",
                match lang {
                    Lang::Ja => format!(
                        "(同じ系列に {n} つの観点が当たった。3 経路は相関するので独立な裏付けの数ではない)"
                    ),
                    Lang::En => format!(
                        "({n} views fired on the same series. The three routes correlate, so this \
                         is not that many independent corroborations)"
                    ),
                }
            )?;
        }
        if !e.corroborating_series.is_empty() {
            let series = e.corroborating_series.join(", ");
            writeln!(
                out,
                "       {}",
                match lang {
                    Lang::Ja => format!(
                        "水準変化と別の観点が同じ時刻で当たった系列: {series} (優先度は上げていない)"
                    ),
                    Lang::En => format!(
                        "Series where a level change and another view fired at the same time: \
                         {series} (priority was not raised for it)"
                    ),
                }
            )?;
        }
    }

    write_series_rollup(out, ep, lang)?;
    write_detections(out, ep, tz, detail, lang)?;

    // 解釈と留保は**検出パターンが決まれば中身も決まる**。エピソードごとに
    // 並べるとエピソード数に比例して同じ文が増えるので、要約では指標ごとに
    // 1 度だけレポート末尾へ集約する (`write_not_established`)。
    if detail.is_full() {
        if !e.possible_interpretations.is_empty() {
            writeln!(
                out,
                "     {}",
                text!(
                    ja: "考えられる解釈 (どれとも断定しない)",
                    en: "Possible interpretations (none of them asserted)",
                )
                .get(lang)
            )?;
            for i in &e.possible_interpretations {
                writeln!(out, "       - {i}")?;
            }
        }
        if !e.not_established.is_empty() {
            writeln!(
                out,
                "     {}",
                text!(
                    ja: "この所見では確かめていないこと",
                    en: "What this finding does not establish",
                )
                .get(lang)
            )?;
            for n in &e.not_established {
                writeln!(out, "       - {n}")?;
            }
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

/// 要約で 1 行ずつ並べる検出数の上限。
///
/// 1 件が 1 行なので詳細より多く並べられる。ここも**件数は必ず出す**。
const MAX_SUMMARIZED_DETECTIONS: usize = 12;

/// 関わった系列の一覧 (件数と観点)。
///
/// 検出を全件並べる前に「何が動いたか」を 1 望できるようにする。
fn write_series_rollup<W: Write>(out: &mut W, ep: &Episode, lang: Lang) -> io::Result<()> {
    // (系列, 指標名) → (件数, 観点)
    let mut rows: Vec<(String, &'static str, usize, Vec<&'static str>)> = Vec::new();
    for d in &ep.detections {
        let key = d.series.display();
        let route = d.route().label().get(lang);
        match rows.iter_mut().find(|(k, _, _, _)| *k == key) {
            Some((_, _, n, routes)) => {
                *n += 1;
                if !routes.contains(&route) {
                    routes.push(route);
                }
            }
            None => rows.push((key, d.metric_label, 1, vec![route])),
        }
    }
    // 件数の多い順 → 系列名順 (決定的にする)
    rows.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));

    writeln!(
        out,
        "     {} ({})",
        text!(ja: "関わった系列", en: "Series involved").get(lang),
        rows.len()
    )?;
    for (series, label, count, routes) in rows.iter().take(MAX_LISTED_SERIES_IN_EPISODE) {
        let routes = routes.join(", ");
        writeln!(
            out,
            "       {series:<30} {label:<22} {}",
            match lang {
                Lang::Ja => format!("{count} 件 [{routes}]"),
                Lang::En => format!("{count} detections [{routes}]"),
            }
        )?;
    }
    let rest = rows.len().saturating_sub(MAX_LISTED_SERIES_IN_EPISODE);
    if rest > 0 {
        writeln!(
            out,
            "       {}",
            match lang {
                Lang::Ja => format!("… 他 {rest} 系列"),
                Lang::En => format!("… and {rest} more series"),
            }
        )?;
    }
    Ok(())
}

/// 検出の根拠を書き出す。
///
/// 件数が多い場合は**系列ごとに 1 件**へ絞る。同じ系列の区間を
/// 優先度順に並べると `pgscand` の 18 件が全枠を埋めてしまい、
/// 他に何が起きていたか分からなくなる。
fn write_detections<W: Write>(
    out: &mut W,
    ep: &Episode,
    tz: DisplayTz,
    detail: Detail,
    lang: Lang,
) -> io::Result<()> {
    let total = ep.detections.len();
    // 要約は 1 検出 1 行なので、詳細を書くときより多くを並べられる。
    // **絞る規則は同じ** (系列ごとの代表) で、上限だけが違う。
    let cap = if detail.is_full() {
        MAX_DETAILED_DETECTIONS
    } else {
        MAX_SUMMARIZED_DETECTIONS
    };
    if total <= cap {
        writeln!(
            out,
            "     {}",
            match lang {
                Lang::Ja => format!("検出の根拠 (全 {total} 件)"),
                Lang::En => format!(
                    "Evidence ({})",
                    count_en(total as u64, "detection", "detections")
                ),
            }
        )?;
        // 時刻順のまま出す (エピソードの並びと一致させる)
        for d in &ep.detections {
            write_detection(out, d, tz, detail, lang)?;
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
    let shown = best.len().min(cap);
    writeln!(
        out,
        "     {}",
        match lang {
            Lang::Ja => format!(
                "検出の根拠 (全 {total} 件。系列ごとの代表を {shown} 件だけ示す。\
                 全件は --format json / --format ndjson で出る)"
            ),
            Lang::En => format!(
                "Evidence ({total} detections; showing {shown} as one representative per series. \
                 --format json / --format ndjson carry all of them)"
            ),
        }
    )?;
    for d in best.into_iter().take(cap) {
        write_detection(out, d, tz, detail, lang)?;
    }
    Ok(())
}

fn write_detection<W: Write>(
    out: &mut W,
    d: &Detection,
    tz: DisplayTz,
    detail: Detail,
    lang: Lang,
) -> io::Result<()> {
    writeln!(
        out,
        "     ・[{}] {}",
        d.route().label().get(lang),
        describe_detection(d, lang)
    )?;
    // 要約はここで打ち切る。**判定に使った数値は 1 行目に入っている**
    // (分析層が作った文がそう書いている) ので、落ちるのは検算の材料だけ。
    // 系列に固有の留保だけはここでも出す。
    if !detail.is_full() {
        return write_series_specific_caveats(out, d, lang);
    }
    let (series, origin, pattern) = (
        d.series.display(),
        d.origin.label().get(lang),
        d.pattern.label().get(lang),
    );
    writeln!(
        out,
        "         {}",
        match lang {
            Lang::Ja => format!("系列 {series} / {origin} / 形 {pattern}"),
            Lang::En => format!("Series {series} / {origin} / shape {pattern}"),
        }
    )?;
    // **平均の取り方を必ず添える。** 瞬時値を区間長で重み付けした平均と
    // 同じ数字として読まれないようにするため。
    let (mean, mean_basis) = (d.decision.mean, d.decision.mean_basis.label().get(lang));
    writeln!(
        out,
        "         {}",
        match lang {
            Lang::Ja => format!("平均 {mean:.2} ({mean_basis})"),
            Lang::En => format!("Mean {mean:.2} ({mean_basis})"),
        }
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
                "         {}",
                match lang {
                    Lang::Ja =>
                        format!("条件 {condition_id} (連続 {min_samples} 回以上): {rationale}"),
                    Lang::En => format!(
                        "Condition {condition_id} ({min_samples}+ consecutive samples): {rationale}"
                    ),
                }
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
                "         {}",
                match lang {
                    Lang::Ja => format!(
                        "閾値 MAD の {ratio_threshold} 倍以上、かつ絶対差 \
                         {min_absolute_deviation:.2} 以上 (実測 {peak_absolute_deviation:.2})"
                    ),
                    Lang::En => format!(
                        "Threshold: at least {ratio_threshold}x MAD and an absolute difference of \
                         at least {min_absolute_deviation:.2} (measured {peak_absolute_deviation:.2})"
                    ),
                }
            )?;
        }
        // **倍数を書かない。** 散らばりが測れないので基準になる倍数が無い。
        DecisionBasis::AbsoluteDeparture {
            dispersion,
            min_absolute_deviation,
            peak_absolute_deviation,
            ..
        } => {
            let dispersion = dispersion.label().get(lang);
            writeln!(
                out,
                "         {}",
                match lang {
                    Lang::Ja => format!(
                        "絶対差 {min_absolute_deviation:.2} 以上 (実測 {peak_absolute_deviation:.2})。\
                         倍数では判断していない ({dispersion})"
                    ),
                    Lang::En => format!(
                        "Absolute difference of at least {min_absolute_deviation:.2} \
                         (measured {peak_absolute_deviation:.2}). No multiple of the spread was \
                         used ({dispersion})"
                    ),
                }
            )?;
        }
        DecisionBasis::LevelShift {
            min_shift,
            pooled_mad,
            normalized_threshold,
            persistence_share,
            persistence_threshold,
            window_samples,
            window_requested_secs,
            window_secs,
            ..
        } => {
            let spread = pooled_mad.map_or_else(
                || {
                    text!(ja: "測れない", en: "not measurable")
                        .get(lang)
                        .to_string()
                },
                |m| format!("{m:.2}"),
            );
            let (share, required) = (persistence_share * 100.0, persistence_threshold * 100.0);
            writeln!(
                out,
                "         {}",
                match lang {
                    Lang::Ja => format!(
                        "最小変化量 {min_shift:.2} / 窓内の散らばり {spread} / \
                         正規化閾値 {normalized_threshold} 倍 / 持続 {share:.0}% (要 {required:.0}%)"
                    ),
                    Lang::En => format!(
                        "Minimum shift {min_shift:.2} / spread within the windows {spread} / \
                         normalised threshold {normalized_threshold}x / \
                         persistence {share:.0}% ({required:.0}% required)"
                    ),
                }
            )?;
            // **要求した窓幅と実効の窓幅を書き分ける。**
            // 点数は採取間隔と下限で決まるので、要求幅をそのまま
            // 「この幅で判定した」と出すと嘘になる。
            let (effective, requested) = (
                crate::detect::describe_duration(*window_secs, lang),
                crate::detect::describe_duration(*window_requested_secs, lang),
            );
            writeln!(
                out,
                "         {}",
                match lang {
                    Lang::Ja => format!(
                        "窓: 前後 {window_samples} 点ずつ / 実効幅 {effective} (要求 {requested})"
                    ),
                    Lang::En => format!(
                        "Windows: {window_samples} points either side / \
                         effective width {effective} ({requested} requested)"
                    ),
                }
            )?;
        }
    }
    write_observations(
        out,
        &d.decision.observations,
        d.decision.observations_truncated,
        tz,
        lang,
    )?;
    write_baseline(out, d, lang)
}

fn write_observations<W: Write>(
    out: &mut W,
    observations: &[Observation],
    truncated: bool,
    tz: DisplayTz,
    lang: Lang,
) -> io::Result<()> {
    if observations.is_empty() {
        return Ok(());
    }
    let mut line = format!("         {}:", text!(ja: "観測", en: "Observed").get(lang));
    for o in observations {
        line.push_str(&format!(" {}={:.2}", tz.time(o.end_ust), o.value));
    }
    if truncated {
        line.push_str(text!(ja: " … (以降は省略)", en: " … (rest omitted)").get(lang));
    }
    writeln!(out, "{line}")
}

fn write_baseline<W: Write>(out: &mut W, d: &Detection, lang: Lang) -> io::Result<()> {
    let b = &d.baseline;
    let (median, mad) = (
        b.median.map_or("-".to_string(), |v| format!("{v:.2}")),
        b.mad.map_or("-".to_string(), |v| format!("{v:.2}")),
    );
    let (samples, dispersion, basis) = (
        b.samples,
        b.dispersion.label().get(lang),
        b.basis.label().get(lang),
    );
    writeln!(
        out,
        "         {}",
        match lang {
            Lang::Ja => format!(
                "比較基準: 中央値 {median} / MAD {mad} / 材料 {samples} 採取 / \
                 散らばり {dispersion} — {basis}"
            ),
            Lang::En => format!(
                "Comparison basis: median {median} / MAD {mad} / {samples} samples of material / \
                 spread {dispersion} — {basis}"
            ),
        }
    )?;
    if b.flagged_share > 0.0 {
        let share = b.flagged_share * 100.0;
        writeln!(
            out,
            "           {}",
            match lang {
                Lang::Ja => format!("材料のうち固定条件を満たした割合: {share:.0}%"),
                Lang::En => format!("Share of the material meeting a fixed condition: {share:.0}%"),
            }
        )?;
    }
    for c in &b.caveats {
        writeln!(out, "           ! {c}")?;
    }
    Ok(())
}

/// 報告の別の場所が同じことを言っている留保。
///
/// **要約で外してよいのはここに挙げたものだけ。** 落とすのではなく、
/// 既に出ている文と重複しているから繰り返さないという判断である。
const CAVEATS_STATED_ELSEWHERE: &[Text] = &[
    // ヘッダの「比較基準: …(この入力自身が材料)」と末尾の「注意」が言う
    BASELINE_CAVEAT_SELF_SOURCED,
    // 検出の説明文が「散らばりが測れないため絶対差で判断した」と言う
    BASELINE_CAVEAT_MAD_ZERO,
    BASELINE_CAVEAT_TOO_SPARSE,
];

/// 留保が「別の場所で言っている」ものかどうか。
///
/// 比較は**解決済みの文字列**で行う。[`Detection`] が持つ留保は
/// 分析層が言語を解決したあとの値なので、ここで `Text` に戻さない。
fn is_stated_elsewhere(caveat: &str, lang: Lang) -> bool {
    CAVEATS_STATED_ELSEWHERE
        .iter()
        .any(|t| t.get(lang) == caveat)
}

/// 系列に固有の留保だけを書き出す (要約用)。
///
/// 残るのは**報告の他のどこにも出ていない留保**で、実際には
/// 「中央値そのものが固定条件を満たしている」「材料の半分以上が固定条件を
/// 満たしている」「区間の 1 要求あたりの値なので基準は要求当たりの平均ではない」の
/// 3 つになる。規律 3 が求めているのは出所と当てにならなさを伝えることで、
/// 検出ごとに同じ一文を並べることではない。
///
/// 固定条件の検出には出さない。**その経路は比較基準を判定に使っていない**ので、
/// 基準の留保を添えると使っていない根拠で所見を割り引くことになる (規律 17)。
fn write_series_specific_caveats<W: Write>(
    out: &mut W,
    d: &Detection,
    lang: Lang,
) -> io::Result<()> {
    if matches!(d.decision.basis, DecisionBasis::FixedCondition { .. }) {
        return Ok(());
    }
    for c in d
        .baseline
        .caveats
        .iter()
        .filter(|c| !is_stated_elsewhere(c, lang))
    {
        writeln!(out, "         ! {c}")?;
    }
    Ok(())
}

/// 確かめていないことを**指標ごとに 1 度だけ**書き出す (要約用)。
///
/// # なぜエピソード本文から出すのか
///
/// 中身は指標が決まれば決まる。同じ `%idle` の所見が 4 回立てば、
/// 同じ 6 行が 4 回並ぶ。エピソード数に比例して増えるのは繰り返しであって
/// 情報ではない。ここへ集めると**指標の数**にしか比例しない。
///
/// # 落としてはいけない
///
/// 「観測と解釈の境界」はこの出力の必須要素で (モジュール doc の「必ず出すもの」、
/// 規律 21 / 22)、`--verbose` や JSON への案内では代用にならない。
/// 位置を変えているだけで、要約でも必ず出す。
fn write_not_established<W: Write>(out: &mut W, a: &Assessment) -> io::Result<()> {
    // 指標ごとに最初の 1 件。並びは検出の出現順 (決定的)。
    let mut rows: Vec<(&'static str, &[&'static str])> = Vec::new();
    let detections = a
        .episodes
        .iter()
        .flat_map(|e| e.episode.detections.iter())
        .chain(a.background.iter().map(|b| &b.detection));
    for d in detections {
        if d.not_established.is_empty() || rows.iter().any(|(label, _)| *label == d.metric_label) {
            continue;
        }
        rows.push((d.metric_label, d.not_established.as_slice()));
    }
    if rows.is_empty() {
        return Ok(());
    }
    writeln!(out)?;
    writeln!(
        out,
        "{}",
        text!(
            ja: "確かめていないこと (指標ごとに 1 度。観測と解釈の境界)",
            en: "What these findings do not establish (once per metric — the line between \
                 observation and interpretation)",
        )
        .get(a.lang)
    )?;
    for (label, items) in rows {
        writeln!(out, "  {label}")?;
        for n in items {
            writeln!(out, "    - {n}")?;
        }
    }
    Ok(())
}

/// 背景の所見を書き出す。
///
/// **エピソードと同じ画面に出す。** 「一日中スワップが使われている」を
/// 別枠にした理由 (いつの手がかりを持たない) と、そのぶん
/// エピソードから外れていることを読み手へ伝えるため。
fn write_background<W: Write>(
    out: &mut W,
    a: &Assessment,
    tz: DisplayTz,
    detail: Detail,
) -> io::Result<()> {
    let lang = a.lang;
    let excluded = a.report_scope.background_excluded_by_priority;
    if a.background.is_empty() {
        let dropped = match (excluded, lang) {
            (0, _) => String::new(),
            (n, Lang::Ja) => format!("。優先度の下限で {n} 件を除外"),
            (n, Lang::En) => format!("; {n} dropped by the priority floor"),
        };
        writeln!(
            out,
            "{}",
            match lang {
                Lang::Ja =>
                    format!("背景の所見なし (入力のほぼ全体を占める検出は無かった{dropped})"),
                Lang::En => format!(
                    "No standing findings (nothing spanned almost the whole input{dropped})"
                ),
            }
        )?;
        return Ok(());
    }
    writeln!(
        out,
        "{}",
        text!(
            ja: "背景の所見 (入力のほぼ全体を占め、いつ起きたかの手がかりを持たない)",
            en: "Standing findings (they span almost the whole input and carry no clue about when)",
        )
        .get(lang)
    )?;
    // **「重要でない」と読まれないようにする。** 時刻を絞る材料にならない
    // ことだけを言っている。優先度はエピソードと同じ規則で付けてある。
    writeln!(
        out,
        "  {}",
        text!(
            ja: "(重要でないという意味ではない。入力のどこを切っても成立するので、\
                 いつ何が起きたかを絞る材料にならないという意味である)",
            en: "(This does not make them unimportant. They hold wherever you cut the input, \
                 which is why they narrow down nothing about when something happened)",
        )
        .get(lang)
    )?;
    for b in &a.background {
        let f = &b.finding;
        let share = b
            .share_of_input_percent
            .map_or(String::new(), |p| match lang {
                Lang::Ja => format!(" — 入力の {p}% を占める"),
                Lang::En => format!(" — spans {p}% of the input"),
            });
        writeln!(out, "  {} {}{share}", f.priority.mark(), f.headline)?;
        // エピソードと同じ扱い。昇降がなければ下地と理由を繰り返さない。
        if detail.is_full() || f.priority != f.base_priority {
            let (p, base) = (
                f.priority.label().get(lang),
                f.base_priority.label().get(lang),
            );
            writeln!(
                out,
                "     {}: {}",
                text!(ja: "優先度", en: "Priority").get(lang),
                match lang {
                    Lang::Ja => format!("{p} (下地 {base}) — この検出について"),
                    Lang::En => format!("{p} (baseline {base}) — for this detection"),
                }
            )?;
            for r in &f.priority_reasons {
                writeln!(out, "       - {r}")?;
            }
        } else {
            writeln!(
                out,
                "     {}: {}",
                text!(ja: "優先度", en: "Priority").get(lang),
                f.priority.label().get(lang)
            )?;
        }
        let (level, basis) = (
            f.sufficiency.level.label().get(lang),
            f.sufficiency.basis.label().get(lang),
        );
        let (material, required) = (
            f.sufficiency.material_samples,
            f.sufficiency.required_samples,
        );
        writeln!(
            out,
            "     {}: {}",
            text!(ja: "根拠の充足度", en: "Evidence sufficiency").get(lang),
            match lang {
                Lang::Ja => format!("{level} ({basis}: {material} 採取 / 要 {required} 採取)"),
                Lang::En => format!("{level} ({basis}: {material} samples / {required} required)"),
            }
        )?;
        write_detection(out, &b.detection, tz, detail, lang)?;
    }
    if excluded > 0 {
        writeln!(
            out,
            "  {}",
            match lang {
                Lang::Ja => format!("(優先度の下限で {excluded} 件を除外した)"),
                Lang::En => format!("({excluded} were dropped by the priority floor)"),
            }
        )?;
    }
    Ok(())
}

fn write_coverage<W: Write>(out: &mut W, c: &EvaluationCoverage, lang: Lang) -> io::Result<()> {
    writeln!(
        out,
        "{}",
        text!(ja: "評価の網羅度", en: "Evaluation coverage").get(lang)
    )?;
    let (patterns, present, evaluated, not_evaluated) = (
        c.patterns_in_catalog,
        c.series_present,
        c.series_evaluated,
        c.series_not_evaluated,
    );
    writeln!(
        out,
        "  {}",
        match lang {
            Lang::Ja => format!(
                "カタログ {patterns} パターン / 入力にあった系列 {present} / \
                 評価できた系列 {evaluated} / 評価できなかった系列 {not_evaluated}"
            ),
            Lang::En => format!(
                "{patterns} patterns in the catalog / {present} series present in the input / \
                 {evaluated} evaluated / {not_evaluated} not evaluated"
            ),
        }
    )?;
    for (route, tally) in [
        (DetectRoute::FixedCondition, &c.fixed_condition),
        (DetectRoute::RobustDeviation, &c.robust_deviation),
        (DetectRoute::LevelShift, &c.level_shift),
    ] {
        write_tally(out, route, tally, lang)?;
    }

    write_blocked(out, c, lang)?;
    write_basis_leaning(out, c, lang)
}

/// 比較基準が異変側へ寄っている系列を列挙する。
///
/// **エピソードの有無にかかわらず出す。** 検出が立たなかった系列の警告を
/// text から落とすと、同じ留保が JSON にだけ残り、形式ごとに伝播が変わる。
/// `%idle` が 4 と 6 を交互に取る系列では中央値 5 が固定条件の内側だが、
/// 連続 2 回を満たさないので固定条件の検出は無く、逸脱も水準変化も
/// 絶対差の下限に届かない。それでも「この系列の基準は当てにならない」は伝える。
fn write_basis_leaning<W: Write>(
    out: &mut W,
    c: &EvaluationCoverage,
    lang: Lang,
) -> io::Result<()> {
    let series: Vec<String> = c.basis_leaning().map(|e| e.series.display()).collect();
    if series.is_empty() {
        return Ok(());
    }
    let n = series.len();
    writeln!(
        out,
        "  {}",
        match lang {
            Lang::Ja => format!(
                "比較基準が異変側へ寄っている疑いがある系列: {n} 系列 (検出の有無とは無関係)"
            ),
            Lang::En => format!(
                "Series whose comparison basis may lean toward the anomaly: {n} \
                 (independent of whether anything was detected)"
            ),
        }
    )?;
    let shown: Vec<&str> = series
        .iter()
        .take(MAX_LISTED_SERIES_IN_EPISODE)
        .map(String::as_str)
        .collect();
    let rest = series.len().saturating_sub(shown.len());
    let tail = match (rest, lang) {
        (0, _) => String::new(),
        (n, Lang::Ja) => format!(", 他 {n} 系列"),
        (n, Lang::En) => format!(", and {n} more"),
    };
    writeln!(out, "    {}{tail}", shown.join(", "))?;
    writeln!(
        out,
        "    {}",
        text!(
            ja: "(中央値そのものが固定条件の内側にある、または材料の半分以上が条件を満たしている。\
                 この系列の逸脱検出は当てにならない)",
            en: "(Either the median itself sits inside a fixed condition, or more than half the \
                 material meets one. Deviation detection for these series cannot be trusted)",
        )
        .get(lang)
    )
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
fn write_blocked<W: Write>(out: &mut W, c: &EvaluationCoverage, lang: Lang) -> io::Result<()> {
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
            let key = (route.label().get(lang), reason.label().get(lang));
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
        "  {}",
        text!(
            ja: "評価できなかったもの (データの制約による。これは「検出なし」ではない)",
            en: "What could not be evaluated (limits in the data — this is not 'nothing found')",
        )
        .get(lang)
    )?;
    for ((route, reason), series) in &groups {
        let shown: Vec<&str> = series
            .iter()
            .take(MAX_LISTED_SERIES)
            .map(String::as_str)
            .collect();
        let rest = series.len().saturating_sub(shown.len());
        let tail = match (rest, lang) {
            (0, _) => String::new(),
            (n, Lang::Ja) => format!(", 他 {n} 系列"),
            (n, Lang::En) => format!(", and {n} more"),
        };
        let (n, names) = (series.len(), shown.join(", "));
        writeln!(
            out,
            "    {}",
            match lang {
                Lang::Ja => format!("[{route}] {reason}: {n} 系列 ({names}{tail})"),
                Lang::En => format!("[{route}] {reason}: {n} series ({names}{tail})"),
            }
        )?;
    }
    Ok(())
}

fn write_tally<W: Write>(
    out: &mut W,
    route: DetectRoute,
    t: &RouteTally,
    lang: Lang,
) -> io::Result<()> {
    let label = route.label().get(lang);
    let (detected, detections, evaluated, not_applicable, blocked) = (
        t.detected_series,
        t.detections,
        t.evaluated_series,
        t.not_applicable_series,
        t.blocked_series,
    );
    writeln!(
        out,
        "  {label:<20} {}",
        match lang {
            Lang::Ja => format!(
                "検出 {detected} 系列 ({detections} 件) / 評価済み {evaluated} 系列 / \
                 対象外 {not_applicable} 系列 / 評価不能 {blocked} 系列"
            ),
            Lang::En => format!(
                "{detected} series with detections ({detections} of them) / \
                 {evaluated} evaluated / {not_applicable} not applicable / {blocked} blocked"
            ),
        }
    )?;
    // 構造的に見ていない端は「検出なし」に数えているので、別行で必ず出す。
    // 出さないと「ファイル端で起きた変化が無かった」と読まれる (規律 7)。
    if t.series_with_blind_edges > 0 {
        let (n, samples) = (t.series_with_blind_edges, t.blind_edge_samples);
        writeln!(
            out,
            "  {:<20}   {}",
            "",
            match lang {
                Lang::Ja => format!(
                    "うち {n} 系列は前後窓を取れない端があり、計 {samples} 採取を見ていない"
                ),
                Lang::En => format!(
                    "{n} of them have edges where no window fits; {samples} samples in total \
                     were not looked at"
                ),
            }
        )?;
    }
    Ok(())
}

// ===========================================================================
// json / ndjson
// ===========================================================================

/// エージェント向け。**型のフィールドをそのまま出す (確率値は無い)。**
///
/// **起動区間ごとの所見を 1 つのドキュメントへ収める。**
/// 区間ごとに独立した JSON を並べると、先頭の 1 件しか読めない
/// ドキュメント列になってしまう (`summarize --format json` と同じ方針)。
pub fn write_json<W: Write>(
    out: &mut W,
    assessments: &[Assessment],
    tz: DisplayTz,
) -> io::Result<()> {
    let doc = serde_json::json!({
        "schema_version": DETECT_SCHEMA_VERSION,
        "assessment_kind": ASSESSMENT_KIND,
        "detector_version": DETECTOR_VERSION,
        // 時刻そのものは epoch 秒で出す。この欄は `report_scope` の
        // `hh:mm:ss` をどの壁時計として読んだかを示す。
        "report_timezone": tz.label_at(report_anchor(assessments)),
        // 起動区間ごとに 1 件。区間をまたいだ検出はしない
        "assessments": assessments,
    });
    serde_json::to_writer_pretty(&mut *out, &doc).map_err(json_err)?;
    writeln!(out)
}

/// 行単位。起動区間ごとにヘッダ 1 行 + エピソード 1 件 = 1 行
/// + 背景の所見 1 件 = 1 行 + 網羅度 1 行。
///
/// どの行がどの起動区間のものかを `segment` で示す。
pub fn write_ndjson<W: Write>(
    out: &mut W,
    assessments: &[Assessment],
    tz: DisplayTz,
) -> io::Result<()> {
    let zone = tz.label_at(report_anchor(assessments));
    for (segment, a) in assessments.iter().enumerate() {
        let head = serde_json::json!({
            "schema_version": a.schema_version,
            "record": "detect_header",
            "segment": segment,
            // 時刻は epoch 秒。この欄は `report_scope` の `hh:mm:ss` の基準。
            "report_timezone": zone,
            "assessment_kind": a.assessment_kind,
            "detector_version": a.detector_version,
            "catalog_version": a.coverage.catalog_version,
            "thresholds": a.thresholds,
            "baseline_basis": a.baseline_basis,
            "episode_gap_secs": a.episode_gap_secs,
            "episode_onset_span_cap_secs": a.episode_onset_span_cap_secs,
            "standing_span_percent": a.standing_span_percent,
            "interval_p90_secs": a.interval_p90_secs,
            "source": a.source,
            "period": a.period,
            // 報告時間帯・最低優先度・除外件数。**行だけを見て絞り込みを復元できるように**
            "report_scope": a.report_scope,
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

        // 背景の所見もエピソードと同じ粒度で 1 件 = 1 行
        for b in &a.background {
            let row = serde_json::json!({
                "schema_version": a.schema_version,
                "record": "background",
                "segment": segment,
                "background": b,
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
        assessment_with_period(values, PeriodBounds::default())
    }

    /// 期間を明示した所見。
    ///
    /// 「入力のほぼ全体を占める」の分母は期間なので、背景の所見を
    /// 試すテストでは期間を渡す。
    fn assessment_with_period(values: &[f64], period: PeriodBounds) -> Assessment {
        let ts = single(cpu_idle(&vals(values)));
        let opts = DetectOptions {
            lang: Lang::Ja,
            ..Default::default()
        };
        let outcome = detect(&ts, &opts);
        assess(
            outcome,
            SummarySource {
                label: "example".to_string(),
                ..Default::default()
            },
            period,
            &opts,
        )
    }

    /// 値の並びをちょうど覆う期間。
    fn period_of(values: &[f64]) -> PeriodBounds {
        PeriodBounds {
            first_ust: Some(T0),
            last_ust: Some(T0 + values.len() as u64 * STEP_SECS),
            samples: values.len() as u64,
            ..Default::default()
        }
    }

    /// テストの T0 は UTC 基準に置いた値なので、表記も UTC で確かめる。
    const TZ: DisplayTz = DisplayTz::Utc;

    /// 既定 (要約) の text。
    fn render(a: &Assessment) -> String {
        render_with(a, Detail::Summary)
    }

    /// `--verbose` の text。
    fn render_full(a: &Assessment) -> String {
        render_with(a, Detail::Full)
    }

    fn render_with(a: &Assessment, detail: Detail) -> String {
        let mut buf: Vec<u8> = Vec::new();
        write_text(&mut buf, a, TZ, detail).expect("書き出し");
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
        write_json(&mut buf, &[a.clone(), a], TZ).expect("JSON");
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
        write_ndjson(&mut buf, std::slice::from_ref(&a), TZ).expect("NDJSON");
        let text = String::from_utf8(buf).expect("UTF-8");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2 + a.episodes.len() + a.background.len());
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

    /// 基準の寄りを、検出が無くても text に出す (Issue #5 ⑲)。
    ///
    /// `%idle` が 4 と 6 を交互に取ると中央値 5 は固定条件の内側だが、
    /// 連続 2 回を満たさないので固定条件の検出は無く、
    /// 逸脱も水準変化も絶対差の下限に届かない。
    /// **JSON にだけ警告が残る状態にしてはいけない。**
    #[test]
    fn the_coverage_lists_series_whose_basis_leans_even_without_an_episode() {
        let v: Vec<f64> = (0..30)
            .map(|i| if i % 2 == 0 { 4.0 } else { 6.0 })
            .collect();
        let a = assessment(&v);
        assert!(a.episodes.is_empty(), "この入力では検出が立たない");
        assert_eq!(
            a.coverage.series_with_basis_leaning, 1,
            "網羅度は基準の寄りを数える"
        );

        let text = render(&a);
        assert!(
            text.contains("比較基準が異変側へ寄っている疑いがある系列"),
            "{text}"
        );
        assert!(text.contains("A_CPU/all/idle"), "{text}");

        // JSON も同じことを持っている (形式間で表現をずらさない)
        let mut buf: Vec<u8> = Vec::new();
        write_json(&mut buf, std::slice::from_ref(&a), TZ).expect("JSON");
        let parsed: serde_json::Value =
            serde_json::from_str(&String::from_utf8(buf).expect("UTF-8")).expect("パース");
        let cov = &parsed["assessments"][0]["coverage"];
        assert_eq!(cov["series_with_basis_leaning"], 1);
        let leaning = cov["series"]
            .as_array()
            .expect("系列")
            .iter()
            .filter(|e| e["basis_may_reflect_the_anomaly"] == true)
            .count();
        assert_eq!(leaning, 1);
    }

    /// 背景の所見を 3 形式すべてに出す (Issue #5 ⑨)。
    #[test]
    fn a_background_finding_appears_in_every_format() {
        // 30 点すべて %idle 2% → 入力全体を覆うので背景の所見
        let v = vec![2.0; 30];
        let a = assessment_with_period(&v, period_of(&v));
        assert_eq!(a.background.len(), 1, "背景の所見が 1 件");
        assert!(
            a.episodes.is_empty(),
            "いつの手がかりが無いものをエピソードにしない"
        );

        let text = render(&a);
        assert!(text.contains("背景の所見"), "{text}");
        assert!(text.contains("入力の 100% を占める"), "{text}");
        assert!(text.contains("エピソードなし"), "{text}");

        let mut buf: Vec<u8> = Vec::new();
        write_json(&mut buf, std::slice::from_ref(&a), TZ).expect("JSON");
        let parsed: serde_json::Value =
            serde_json::from_str(&String::from_utf8(buf).expect("UTF-8")).expect("パース");
        assert_eq!(
            parsed["assessments"][0]["background"]
                .as_array()
                .map(Vec::len),
            Some(1)
        );

        let mut buf: Vec<u8> = Vec::new();
        write_ndjson(&mut buf, std::slice::from_ref(&a), TZ).expect("NDJSON");
        let text = String::from_utf8(buf).expect("UTF-8");
        let records: Vec<String> = text
            .lines()
            .map(|l| {
                serde_json::from_str::<serde_json::Value>(l).expect("パース")["record"]
                    .as_str()
                    .expect("record")
                    .to_string()
            })
            .collect();
        assert_eq!(
            records,
            vec!["detect_header", "background", "coverage"],
            "背景の所見も 1 件 = 1 行"
        );
    }

    /// 報告時間帯と最低優先度を 3 形式すべてに出す (Issue #5 ㉑)。
    #[test]
    fn the_report_scope_is_visible_in_every_format() {
        let mut a = assessment(&[50.0; 20]);
        a.filter_priority(Priority::Investigate);

        let text = render(&a);
        assert!(text.contains("報告範囲:"), "{text}");
        assert!(text.contains("最低優先度: 調査"), "{text}");
        assert!(text.contains("件数: 入力全体の検出"), "{text}");

        let mut buf: Vec<u8> = Vec::new();
        write_json(&mut buf, std::slice::from_ref(&a), TZ).expect("JSON");
        let parsed: serde_json::Value =
            serde_json::from_str(&String::from_utf8(buf).expect("UTF-8")).expect("パース");
        let scope = &parsed["assessments"][0]["report_scope"];
        assert_eq!(scope["min_priority"], "investigate");
        assert!(scope["detections_in_input"].is_number());
        assert!(scope["episodes_excluded_by_priority"].is_number());

        let mut buf: Vec<u8> = Vec::new();
        write_ndjson(&mut buf, std::slice::from_ref(&a), TZ).expect("NDJSON");
        let first = String::from_utf8(buf)
            .expect("UTF-8")
            .lines()
            .next()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).expect("パース"))
            .expect("ヘッダ行");
        assert_eq!(first["report_scope"]["min_priority"], "investigate");
    }

    /// 検出が立つ入力。`%idle` が 80 から 1 へ落ちて戻る。
    fn assessment_with_an_episode() -> Assessment {
        let mut v = vec![80.0; 40];
        for x in v.iter_mut().take(25).skip(20) {
            *x = 1.0;
        }
        let a = assessment(&v);
        assert!(!a.episodes.is_empty(), "この入力では検出が立つ");
        a
    }

    /// 要約でも落とさないもの。
    ///
    /// **規律 3 / 7 と「観測と解釈の境界」は `--verbose` を付けなくても満たす。**
    /// 冗長さの解消のために出力の契約を削ってはいけない。
    #[test]
    fn the_summary_still_states_the_basis_the_coverage_and_the_boundary() {
        let text = render(&assessment_with_an_episode());
        // 規律 3: 比較基準の出所
        assert!(text.contains("この入力自身が材料"), "{text}");
        assert!(text.contains("外部の正常値ではない"), "{text}");
        // 規律 7: 評価の網羅度
        assert!(text.contains("評価の網羅度"), "{text}");
        // 規律 21 / 22: 観測と解釈の境界 (指標ごとに 1 度)
        assert!(text.contains("確かめていないこと"), "{text}");
        assert!(text.contains("CPU 能力の不足"), "{text}");
        // 優先度と充足度は別のフィールドのまま
        assert!(text.contains("優先度:"), "{text}");
        assert!(text.contains("根拠の充足度:"), "{text}");
        assert!(!text.contains("確信度:"), "{text}");
    }

    /// 要約で省くもの。
    #[test]
    fn the_summary_drops_the_per_detection_breakdown_and_the_interpretations() {
        let text = render(&assessment_with_an_episode());
        assert!(!text.contains("観測: "), "検算用の値列挙は省く\n{text}");
        // 見出しで判定する (何を省いたかの案内には「考えられる解釈」の語が入る)
        assert!(
            !text.contains("考えられる解釈 (どれとも断定しない)"),
            "{text}"
        );
        assert!(
            !text.contains("この所見では確かめていないこと"),
            "エピソード本文ではなく末尾へ集約する\n{text}"
        );
        // **省いたことを黙らない。**
        assert!(text.contains("--verbose"), "{text}");
    }

    /// 同じ留保を 2 か所で言わない。
    ///
    /// 「散らばりが測れない」は検出の説明文に入っているので、
    /// 比較基準の留保として繰り返さない。
    #[test]
    fn the_summary_does_not_repeat_a_caveat_the_description_already_carries() {
        // 背景の所見は要約でも検出 1 件の根拠を出すので、そこで確かめる。
        let v = vec![2.0; 30];
        let a = assessment_with_period(&v, period_of(&v));
        assert_eq!(a.background.len(), 1, "背景の所見が立つ入力");
        let text = render(&a);
        assert!(
            !text.contains(BASELINE_CAVEAT_SELF_SOURCED_IN_DETECTION),
            "比較基準の出所はヘッダと注意が言う。検出ごとに繰り返さない\n{text}"
        );
        assert!(
            !text.contains(BASELINE_CAVEAT_MAD_ZERO.get(Lang::Ja)),
            "説明文と同じ内容を留保として並べない\n{text}"
        );
        // ヘッダと注意では言っている (落としたのではなく繰り返さないだけ)
        assert!(text.contains("この入力自身が材料"), "{text}");
        assert!(text.contains("外部の正常値ではない"), "{text}");
    }

    /// 検出行に付く一般留保 (`! ` 付きの形)。
    const BASELINE_CAVEAT_SELF_SOURCED_IN_DETECTION: &str =
        "! この基準は入力自身から作ったものであり";

    /// 同じ系列の散発的な検出を 1 ブロックにまとめる。
    ///
    /// 実データで 111 エピソード中 111 件が単独系列になり、同じ指標が
    /// 18 回・15 回と並んだのがこの形式を入れた理由。
    #[test]
    fn the_summary_groups_episodes_by_series() {
        // 80 の中に短い落ち込みを 3 回作る → 同じ系列で 3 エピソード
        let mut v = vec![80.0; 60];
        for start in [10usize, 30, 50] {
            for x in v.iter_mut().skip(start).take(3) {
                *x = 1.0;
            }
        }
        let a = assessment(&v);
        assert!(a.episodes.len() >= 2, "複数のエピソードが立つ入力");
        let text = render(&a);
        // 系列は 1 ブロックにまとまり、件数が出る
        assert!(
            text.contains(&format!("— {} 件 (最高:", a.episodes.len())),
            "系列ごとに件数を出す\n{text}"
        );
        // 「いつ」は時刻として残る
        assert!(text.contains("採取)"), "{text}");
        // エピソードごとの見出しは出さない (それが繰り返しの正体)
        assert!(!text.contains("エピソード 2  検出の始まり"), "{text}");
        // --verbose では従来どおり 1 件ずつ出る
        let full = render_full(&a);
        assert!(full.contains("エピソード 2  検出の始まり"), "{full}");
    }

    /// 同時に鳴った別の系列を隠さない。
    #[test]
    fn the_summary_marks_episodes_where_another_series_fired_too() {
        let a = assessment_with_an_episode();
        let multi = a.episodes.iter().any(|e| {
            e.episode
                .detections
                .iter()
                .any(|d| d.series != e.headline_series)
        });
        let text = render(&a);
        if multi {
            assert!(text.contains("系列)"), "+N 系列 を出す\n{text}");
        }
        // 系列が 1 つだけのエピソードに余計な表記を付けない
        assert!(!text.contains("+0 系列"), "{text}");
    }

    /// `--verbose` は従来の全文に戻す。
    #[test]
    fn verbose_restores_the_full_report() {
        let a = assessment_with_an_episode();
        let text = render_full(&a);
        assert!(text.contains("考えられる解釈"), "{text}");
        assert!(text.contains("この所見では確かめていないこと"), "{text}");
        assert!(text.contains("観測: "), "{text}");
        assert!(text.contains("比較基準: 中央値"), "{text}");
        // 要約への案内は出さない (省いていないため)
        assert!(!text.contains("--verbose で出る"), "{text}");
        // 要約より必ず長い
        assert!(
            text.lines().count() > render(&a).lines().count(),
            "全文が要約より短いことはない"
        );
    }

    /// 基準が異変側へ寄っている疑いは要約でも出す (規律 3)。
    #[test]
    fn the_summary_keeps_a_warning_that_the_basis_leans_toward_the_anomaly() {
        // 大半が固定条件を満たす → 中央値そのものが条件の内側
        let mut v = vec![2.0; 30];
        for x in v.iter_mut().take(6) {
            *x = 80.0;
        }
        let a = assessment_with_period(&v, period_of(&v));
        let text = render(&a);
        assert!(
            text.contains("比較基準が異変側へ寄っている疑いがある")
                || text.contains("中央値そのものが固定条件を満たしている"),
            "{text}"
        );
    }
}

//! 複数ファイルの期間集計 (`summarize`) の書式化。
//!
//! `multi` / `analyze` が作った集計値と判定を描画する。
//! 集計・環境変数の読み取り・診断・出力先の flush はここでは行わない。
//! 時刻の表示には CLI が解決した [`DisplayTz`] を使う。

use std::io::Write;

use crate::analyze::{
    ColumnSummary, Finding, PercentileOutcome, RULESET_VERSION, Verdict, rule_inputs,
};
use crate::model::DisplayTz;
use crate::multi::{BootSegment, MultiFileAnalysis};

/// ホスト・起動区間ごとの指標と判定をテキストで書き出す。
pub fn write_text<W: Write>(
    out: &mut W,
    analysis: &MultiFileAnalysis,
    tz: DisplayTz,
) -> anyhow::Result<()> {
    for (hi, host) in analysis.hosts.iter().enumerate() {
        if hi > 0 {
            writeln!(out)?;
        }
        let id = &host.identity;
        writeln!(
            out,
            "host: {} ({} {} / {}, {} CPU)",
            id.nodename,
            id.sysname,
            id.release,
            id.machine,
            id.cpu_nr.map_or("?".to_string(), |n| n.to_string())
        )?;
        let files: Vec<&str> = host
            .files
            .iter()
            .filter_map(|i| analysis.files.get(*i).map(|f| f.path.as_str()))
            .collect();
        writeln!(out, "files: {}", files.join(", "))?;

        for seg in &host.segments {
            writeln!(out)?;
            write_segment_text(out, seg, tz)?;
        }
    }
    Ok(())
}

fn write_segment_text<W: Write>(
    out: &mut W,
    seg: &BootSegment,
    tz: DisplayTz,
) -> anyhow::Result<()> {
    let p = &seg.summary.period;
    writeln!(
        out,
        "起動区間 {}  {} → {}  ({} サンプル / 連続 {} 区間 / 不連続 {} 区間)",
        seg.index,
        p.first_ust.map_or("-".to_string(), |ust| tz.datetime(ust)),
        p.last_ust.map_or("-".to_string(), |ust| tz.datetime(ust)),
        p.samples,
        p.continuous_intervals,
        p.broken_intervals
    )?;
    for b in &seg.boundaries {
        // 引き継いだかどうかを必ず残す (`multi.rs` の方針)
        let verdict = if b.decision.continuous {
            "差分を引き継いだ".to_string()
        } else {
            format!(
                "不連続 ({}、空白 {} 秒)",
                b.decision
                    .reason
                    .map_or("理由不明".to_string(), |r| format!("{r:?}")),
                b.decision.gap_secs
            )
        };
        writeln!(
            out,
            "  ファイル境界 {} → {}: {verdict}",
            b.prev_file, b.next_file
        )?;
    }

    writeln!(out, "  指標 (ルール判定に使う列)")?;
    let mut printed = false;
    for r in rule_inputs() {
        let key = r.key();
        if let Some(col) = seg.summary.column(key.activity, &key.item, &key.column) {
            writeln!(out, "    {:<28} {}", key.display(), format_column(col))?;
            printed = true;
        }
    }
    if !printed {
        writeln!(out, "    (該当する列がファイルに無い)")?;
    }

    writeln!(out, "  判定 (ルール版 {})", ruleset_version(&seg.findings))?;
    if seg.findings.is_empty() {
        writeln!(out, "    (判定なし)")?;
    }
    for f in &seg.findings {
        write_finding_text(out, f)?;
    }
    Ok(())
}

fn ruleset_version(findings: &[Finding]) -> &str {
    findings
        .first()
        .map(|f| f.ruleset_version)
        .unwrap_or(RULESET_VERSION)
}

fn write_finding_text<W: Write>(out: &mut W, f: &Finding) -> anyhow::Result<()> {
    let mark = match f.verdict {
        Verdict::Observed => "!!",
        Verdict::NotObserved => "ok",
        Verdict::Undetermined => "??",
        Verdict::NotApplicable => "--",
    };
    writeln!(out, "    {mark} {:<26} {}", f.rule_id, f.title)?;
    if let Some(obs) = &f.observation {
        writeln!(out, "       観測: {obs}")?;
    }
    if let Some(reason) = f.reason {
        writeln!(out, "       理由: {reason:?}")?;
    }
    if !f.missing_metrics.is_empty() {
        writeln!(
            out,
            "       欠けている指標: {}",
            f.missing_metrics.join(", ")
        )?;
    }
    Ok(())
}

fn format_column(col: &ColumnSummary) -> String {
    let num = |v: Option<f64>| match v {
        Some(v) => format!("{v:>10.2}"),
        None => format!("{:>10}", "-"),
    };
    let p95 = match col.p95 {
        PercentileOutcome::Computed(r) => format!("{:>10.2}", r.value),
        PercentileOutcome::Unavailable { .. } => format!("{:>10}", "-"),
    };
    format!(
        "max={} mean={} p95={} 区間={}",
        num(col.max.map(|e| e.value)),
        num(col.mean),
        p95,
        col.intervals
    )
}

/// 解析結果全体を JSON 1 文書として書き出す (末尾改行付き)。
pub fn write_json<W: Write>(out: &mut W, analysis: &MultiFileAnalysis) -> anyhow::Result<()> {
    serde_json::to_writer_pretty(&mut *out, analysis)?;
    writeln!(out)?;
    Ok(())
}

/// 起動区間ごとに 1 行の NDJSON として書き出す。
pub fn write_ndjson<W: Write>(out: &mut W, analysis: &MultiFileAnalysis) -> anyhow::Result<()> {
    for host in &analysis.hosts {
        for seg in &host.segments {
            let row = serde_json::json!({
                "schema_version": analysis.schema_version,
                "record": "boot_segment",
                "host": host.identity,
                "segment": seg,
            });
            serde_json::to_writer(&mut *out, &row)?;
            writeln!(out)?;
        }
    }
    Ok(())
}

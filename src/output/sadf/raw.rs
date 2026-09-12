//! `sadf -r` (raw) の出力。
//!
//! オンディスクの生カウンタをそのまま出す。レート変換をしないのが基本で、
//! 単調増加カウンタは `名前; 前値; 現値;` の 3 トークンになる (§4)。
//!
//! ```text
//! 13:20:19 UTC; CPU; -1; %usr; 96005; 96538; %nice; 2578701; 2581805; …
//! 13:20:19 UTC; proc/s; 46972; 47083; cswch/s; 130465866; 132598184;
//! ```
//!
//! - 区切りは `"; "` (セミコロン + 空白)、行末は `";"` + 改行。
//! - アイテムを持たない activity はアイテム識別子フィールドが無い。
//! - **オフライン CPU も必ず出す** (他形式は除外する、§11.3)。
//! - `hdr_line` に無い直書きフィールド名が多数ある (§14.5-3)。

use std::io::{self, Write};

use super::access::{ActivityPair, ItemPair};
use super::render::{item_label, raw_pair};
use super::spec::{ActivitySpec, ItemKind, RawField, RawSpec, RawStyle, Section};
use super::{
    ABSENT_TEXT, FileInfo, SadfConfig, Stamp, double_from_bits, render, spec, write_sensor,
};
use crate::error::Result;
use crate::format::file::{SaFile, ScanControl};
use crate::model::{ActivityId, Availability};
use crate::series::{IntervalView, RecordEvent, Selection, walk};

use super::dbppc::{display_cpu_count, present_specs, scan_restarts};

/// `-r` の出力。
pub fn write_raw<W: Write>(out: &mut W, file: &SaFile, cfg: &SadfConfig) -> Result<()> {
    let info = FileInfo::from_file(file);
    let specs = present_specs(file);
    let restarts = scan_restarts(file)?;

    for block in 0..=restarts.len() {
        for spec in &specs {
            for section in spec.active_sections(&cfg.section) {
                if cfg.debug {
                    write_activity_debug_header(out, file, spec).map_err(super::wrap_io)?;
                }
                write_activity_block(out, file, cfg, &info, spec, section, block)?;
            }
        }
        if let Some(r) = restarts.get(block) {
            let stamp = Stamp::new(cfg.time_base, r.ust_time, r.hms, &info);
            // RESTART / COMMENT 行は nodename も interval も出さず、`;` の後に空白 1 個
            writeln!(
                out,
                "{}; LINUX-RESTART ({} CPU)",
                stamp.raw(),
                display_cpu_count(r.cpu_count)
            )
            .map_err(super::wrap_io)?;
        }
    }
    Ok(())
}

/// `-O debug` のアクティビティヘッダ行 (§4.4-2)。
fn write_activity_debug_header<W: Write>(
    out: &mut W,
    file: &SaFile,
    spec: &ActivitySpec,
) -> io::Result<()> {
    if let Some(e) = file.activities().iter().find(|e| e.id == spec.id) {
        writeln!(
            out,
            "# name; {}; nr_curr; {}; nr_alloc; {}; nr_ini; {}",
            spec.name,
            e.nr.max(0),
            e.nr.max(0),
            e.nr.max(0)
        )?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn write_activity_block<W: Write>(
    out: &mut W,
    file: &SaFile,
    cfg: &SadfConfig,
    info: &FileInfo,
    spec: &ActivitySpec,
    section: &Section,
    block: usize,
) -> Result<()> {
    let mut current_block = 0usize;

    walk(file, &Selection::Only(vec![spec.id]), |view| {
        current_block += count_restarts(view.events);
        if current_block != block {
            return Ok(ScanControl::Continue);
        }
        emit_sample(out, view, cfg, info, spec, section).map_err(super::wrap_io)?;
        Ok(ScanControl::Continue)
    })?;
    Ok(())
}

/// 1 レコード分を書き出す。
fn emit_sample<W: Write>(
    out: &mut W,
    view: &IntervalView<'_>,
    cfg: &SadfConfig,
    info: &FileInfo,
    spec: &ActivitySpec,
    section: &Section,
) -> io::Result<()> {
    if cfg.comments {
        for e in view.events {
            if let RecordEvent::Comment { ust_time, text, .. } = e {
                let stamp = Stamp::new(cfg.time_base, *ust_time, e.time(), info);
                writeln!(out, "{}; COM {text}", stamp.raw())?;
            }
        }
    }
    if !view.has_prev || !view.continuous {
        return Ok(());
    }

    let stamp = Stamp::new(
        cfg.time_base,
        view.curr.ust_time,
        (view.curr.hour, view.curr.minute, view.curr.second),
        info,
    );
    let ts = stamp.raw();

    if cfg.debug {
        writeln!(
            out,
            "# uptime_cs; {}; ust_time; {}; extra_next; 0; record_type; 1; HH:MM:SS; {:02}:{:02}:{:02}",
            view.curr.uptime_cs,
            view.curr.ust_time,
            view.curr.hour,
            view.curr.minute,
            view.curr.second
        )?;
    }

    match spec.id {
        ActivityId::IRQ => write_irq(out, view, &ts),
        ActivityId::PWR_FREQ => write_wghfreq(out, view, &ts, cfg),
        _ => write_generic(out, view, &ts, cfg, spec, section),
    }
}

// ===========================================================================
// 汎用経路
// ===========================================================================

fn write_generic<W: Write>(
    out: &mut W,
    view: &IntervalView<'_>,
    ts: &str,
    cfg: &SadfConfig,
    spec: &ActivitySpec,
    section: &Section,
) -> io::Result<()> {
    let Some(pair) = ActivityPair::from_view(view, spec.id) else {
        return Ok(());
    };
    let fields = raw_fields(section);

    // A_DISK だけは直書きの major / minor が hdr_line のアイテムラベルより
    // **前**に出る (§4.5)。ラベルを挟む位置をここで決める。
    let label_at = if spec.id == ActivityId::DISK { 2 } else { 0 };

    for item in pair.output_items() {
        let mut line = String::with_capacity(128);
        line.push_str(ts);

        for (i, f) in fields.iter().enumerate() {
            if i == label_at {
                push_item_label(&mut line, spec, &item, cfg);
            }
            push_raw_field(&mut line, spec, &item, f, cfg);
        }
        if fields.len() <= label_at {
            push_item_label(&mut line, spec, &item, cfg);
        }

        line.push('\n');
        out.write_all(line.as_bytes())?;
    }
    Ok(())
}

/// アイテム識別子を `; <ラベル>; <値>` の形で足す。
///
/// `A_PWR_USB` は raw ではアイテムを持たない (§4.5)。
/// `A_FS` のデバイス名だけダブルクォートが付く (§14.5-5)。
fn push_item_label(line: &mut String, spec: &ActivitySpec, item: &ItemPair<'_>, cfg: &SadfConfig) {
    if spec.item == ItemKind::None || spec.id == ActivityId::PWR_USB {
        return;
    }
    let head = spec.sections[0]
        .hdr_line
        .split(';')
        .next()
        .unwrap_or_default();
    let label = item_label(spec, item);

    line.push_str("; ");
    line.push_str(head);
    // -O debug では A_CPU のオフライン判定を名前の直後に付ける (§4.4-4)
    if cfg.debug && spec.id == ActivityId::CPU && item.ctx.tick_total == Some(0) {
        line.push_str(" [OFF]");
    }
    line.push_str("; ");
    if spec.id == ActivityId::FS {
        line.push('"');
        line.push_str(&label.db);
        line.push('"');
    } else {
        line.push_str(&label.db);
    }
    line.push(';');
}

fn push_raw_field(
    line: &mut String,
    spec: &ActivitySpec,
    item: &ItemPair<'_>,
    f: &RawField,
    cfg: &SadfConfig,
) {
    match f.style {
        RawStyle::Pval | RawStyle::PvalSum(_) | RawStyle::PvalDiff(_, _) => {
            let (prev, curr) = raw_pair(item, f);
            line.push_str("; ");
            line.push_str(f.name);
            // -O debug ではカウンタが減少したフィールド名の直後に [DEC] (§4.4-3)
            if cfg.debug {
                if let (Availability::Present(p), Availability::Present(c)) = (prev, curr) {
                    if c < p {
                        line.push_str(" [DEC]");
                    }
                }
            }
            line.push_str("; ");
            push_u64(line, prev);
            line.push_str("; ");
            push_u64(line, curr);
            line.push(';');
        }
        RawStyle::Int => {
            let v = item.raw_curr_by_name(f.col);
            line.push_str("; ");
            line.push_str(f.name);
            // A_PWR_BAT の status は値の後に名前付きの注記が入る (§4.4-5)
            line.push_str("; ");
            push_u64(line, v);
            if cfg.debug && spec.id == ActivityId::PWR_BAT && f.col == "status" {
                if let Availability::Present(s) = v {
                    line.push_str(" [");
                    line.push_str(render::bat_status(s));
                    line.push(']');
                }
            }
            line.push(';');
        }
        RawStyle::Sensor => {
            line.push_str("; ");
            line.push_str(f.name);
            line.push_str("; ");
            match item.raw_curr_by_name(f.col) {
                Availability::Present(bits) => write_sensor(line, double_from_bits(bits)),
                _ => line.push_str(ABSENT_TEXT),
            }
            line.push(';');
        }
        RawStyle::Text | RawStyle::QuotedText => {
            let quoted = matches!(f.style, RawStyle::QuotedText);
            line.push_str("; ");
            line.push_str(f.name);
            line.push_str("; ");
            let text = render::field_text(spec, item, f.col).unwrap_or_default();
            if quoted {
                line.push('"');
                line.push_str(&text);
                line.push('"');
            } else {
                line.push_str(&text);
            }
            line.push(';');
        }
        RawStyle::Hex => {
            line.push_str("; ");
            line.push_str(f.name);
            line.push_str("; ");
            match item.raw_curr_by_name(f.col) {
                Availability::Present(v) => {
                    use std::fmt::Write as _;
                    let _ = write!(line, "{v:x}");
                }
                _ => line.push_str(ABSENT_TEXT),
            }
            line.push(';');
        }
    }
}

fn push_u64(line: &mut String, v: Availability<u64>) {
    match v {
        Availability::Present(x) => {
            use std::fmt::Write as _;
            let _ = write!(line, "{x}");
        }
        // 欠落は空にする。0 を書くと「正常に 0」と区別できない。
        _ => line.push_str(ABSENT_TEXT),
    }
}

/// `RawSpec` を実フィールド列へ展開する。
///
/// [`RawSpec::AllPval`] は `-d`/`-p` のフィールド名をそのまま使い、全部 `pval`
/// にする (アイテムを持たないカウンタ系、§4.5)。
fn raw_fields(section: &Section) -> Vec<RawField> {
    match section.raw {
        RawSpec::Fields(list) => list.to_vec(),
        RawSpec::AllPval => section
            .fields
            .iter()
            .filter(|f| !f.pp.is_empty() && !f.col.is_empty())
            .map(|f| RawField {
                col: f.col,
                name: f.pp,
                style: RawStyle::Pval,
            })
            .collect(),
    }
}

// ===========================================================================
// A_IRQ / A_PWR_FREQ の専用経路
// ===========================================================================

/// `A_IRQ` はアイテムが割り込み名で、フィールド名の位置に CPU ラベルが入る。
///
/// フィールド名は `all` (CPU 0) / `CPU0` / `CPU1` … (§4.5)。
/// raw ではオフライン CPU も出す。
fn write_irq<W: Write>(out: &mut W, view: &IntervalView<'_>, ts: &str) -> io::Result<()> {
    let Some(pair) = ActivityPair::from_view(view, ActivityId::IRQ) else {
        return Ok(());
    };
    let nr = pair.curr.nr.max(1) as usize;
    let nr2 = pair.curr.nr2.max(1) as usize;

    for irq in 0..nr2 {
        let name = pair
            .matrix_item(0, irq)
            .and_then(|i| i.key().map(|s| s.to_string()))
            .unwrap_or_else(|| irq.to_string());

        let mut line = String::with_capacity(128);
        line.push_str(ts);
        line.push_str("; INTR; ");
        line.push_str(&name);
        line.push(';');

        for cpu in 0..nr {
            let field = if cpu == 0 {
                "all".to_string()
            } else {
                format!("CPU{}", cpu - 1)
            };
            line.push_str("; ");
            line.push_str(&field);
            line.push_str("; ");
            match pair.matrix_item(cpu, irq) {
                Some(item) => {
                    push_u64(&mut line, item.raw_prev_by_name("intr"));
                    line.push_str("; ");
                    push_u64(&mut line, item.raw_curr_by_name("intr"));
                }
                None => line.push_str("; "),
            }
            line.push(';');
        }
        line.push('\n');
        out.write_all(line.as_bytes())?;
    }
    Ok(())
}

/// `A_PWR_FREQ` は `hdr_line` を使わず直書きの `freq` / `tminst` を
/// 周波数ステップぶん (`nr2` 個、`freq == 0` で打ち切り) 繰り返す (§4.5)。
fn write_wghfreq<W: Write>(
    out: &mut W,
    view: &IntervalView<'_>,
    ts: &str,
    cfg: &SadfConfig,
) -> io::Result<()> {
    let Some(pair) = ActivityPair::from_view(view, ActivityId::PWR_FREQ) else {
        return Ok(());
    };
    let nr = pair.curr.nr.max(0) as usize;
    let nr2 = pair.curr.nr2.max(1) as usize;

    for row in 0..nr {
        let mut line = String::with_capacity(128);
        line.push_str(ts);
        line.push_str("; CPU; ");
        line.push_str(&super::ItemLabel::cpu(row).db);
        line.push(';');

        for step in 0..nr2 {
            let Some(item) = pair.item(row * nr2 + step) else {
                break;
            };
            let freq = item.raw_curr_by_name("freq_khz");
            if matches!(freq, Availability::Present(0)) {
                break;
            }
            line.push_str("; freq; ");
            push_u64(&mut line, freq);
            line.push(';');
            push_raw_field(
                &mut line,
                pair_spec(),
                &item,
                &RawField {
                    col: "time_in_state",
                    name: "tminst",
                    style: RawStyle::Pval,
                },
                cfg,
            );
        }
        line.push('\n');
        out.write_all(line.as_bytes())?;
    }
    Ok(())
}

fn pair_spec() -> &'static ActivitySpec {
    spec::lookup(ActivityId::PWR_FREQ).expect("A_PWR_FREQ の出力定義")
}

fn count_restarts(events: &[RecordEvent]) -> usize {
    events
        .iter()
        .filter(|e| matches!(e, RecordEvent::Restart { .. }))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `AllPval` はフィールド名を `-d`/`-p` から取り、全部 `pval` にする。
    #[test]
    fn all_pval_expands_from_dp_fields() {
        let pcsw = spec::lookup(ActivityId::PCSW).unwrap();
        let fields = raw_fields(&pcsw.sections[0]);
        let names: Vec<_> = fields.iter().map(|f| f.name).collect();
        assert_eq!(names, vec!["proc/s", "cswch/s"]);
        assert!(fields.iter().all(|f| f.style == RawStyle::Pval));
    }

    /// raw のセンサ系フィールド名は `hdr_line` と一致しない (§4.5 の落とし穴)。
    #[test]
    fn sensor_raw_names_differ_from_hdr_line() {
        let fan = spec::lookup(ActivityId::PWR_FAN).unwrap();
        let fields = raw_fields(&fan.sections[0]);
        let names: Vec<_> = fields.iter().map(|f| f.name).collect();
        // hdr_line は FAN;DEVICE;rpm;drpm だが raw は drpm の代わりに rpm_min を出す
        assert_eq!(names, vec!["DEVICE", "rpm", "rpm_min"]);
        assert!(fan.sections[0].hdr_line.contains("drpm"));
    }

    /// A_MEMORY の raw には `hdr_line` に無い `kbttlmem` が入り、派生値は出ない。
    #[test]
    fn memory_raw_uses_hardcoded_total_name() {
        let mem = spec::lookup(ActivityId::MEMORY).unwrap();
        let fields = raw_fields(&mem.sections[0]);
        let names: Vec<_> = fields.iter().map(|f| f.name).collect();
        assert!(names.contains(&"kbttlmem"));
        assert!(!names.contains(&"kbmemused"));
        assert!(!names.contains(&"%memused"));
    }

    /// A_DISK は直書きの major / minor が先頭に来る。
    #[test]
    fn disk_raw_starts_with_major_minor() {
        let disk = spec::lookup(ActivityId::DISK).unwrap();
        let fields = raw_fields(&disk.sections[0]);
        assert_eq!(fields[0].name, "major");
        assert_eq!(fields[1].name, "minor");
        // await / %util は消費されない
        let names: Vec<_> = fields.iter().map(|f| f.name).collect();
        assert!(!names.contains(&"await"));
        assert!(names.contains(&"tot_ticks"));
    }

    /// 欠落は空トークンになり 0 にはならない。
    #[test]
    fn absent_raw_value_is_empty_not_zero() {
        let mut line = String::new();
        push_u64(&mut line, Availability::UnsupportedBySource);
        assert_eq!(line, "");
        push_u64(&mut line, Availability::Present(0));
        assert_eq!(line, "0");
    }

    /// センサ値は小数 6 桁固定。
    #[test]
    fn sensor_values_have_six_decimals() {
        let mut s = String::new();
        write_sensor(&mut s, 1283.0);
        assert_eq!(s, "1283.000000");
    }
}

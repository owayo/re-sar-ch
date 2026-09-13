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
//!
//! # 文字列フィールド
//!
//! 1 item に文字列フィールドが複数ある activity (`A_PWR_USB` の
//! `manufact` / `product`、`A_FS` の `fs_name` / `mountp`) も
//! `ItemSnapshot::texts` からすべて引ける。
//! その世代のファイルに無いフィールドだけが空になる
//! (例: 最古の `A_FS` は `mountp` を持たない)。
//!
//! `A_DISK` のデバイス名だけはファイルに入っていないため、
//! 本家 `get_devname()` の最終フォールバックと同じ `dev<major>-<minor>` を
//! 組み立てる (§2.8.1)。ローカルの `/sys` は引かない — 他ホストで採取した
//! ファイルでは別デバイスの名前が出てしまう。

use std::io::{self, Write};

use super::access::{ActivityPair, ItemPair};
use super::render::{item_label_in, raw_pair};
use super::spec::{ActivitySpec, ItemKind, RawField, RawSpec, RawStyle, Section};
use super::{
    ABSENT_TEXT, FileInfo, SadfConfig, Stamp, double_from_bits, render, spec, write_sensor,
};
use crate::error::Result;
use crate::format::file::{SaFile, ScanControl};
use crate::model::{ActivityId, Availability};
use crate::output::time_filter::Admit;
use crate::series::{IntervalView, RecordEvent, Selection, WalkItem, walk_items};

use super::dbppc::{display_cpu_count, scan_blocks, selected_specs};

/// `-r` の出力。
pub fn write_raw<W: Write>(out: &mut W, file: &SaFile, cfg: &SadfConfig) -> Result<()> {
    let info = FileInfo::from_file_with(file, cfg.time_base);
    let specs = selected_specs(file, cfg);
    let blocks = scan_blocks(file)?;

    for block in 0..blocks.len() {
        if !blocks.has_output(block) {
            if let Some(r) = blocks.restarts.get(block) {
                write_restart_line(out, cfg, &info, r).map_err(super::wrap_io)?;
            }
            continue;
        }
        for spec in &specs {
            for section in spec.active_sections(&cfg.section) {
                if cfg.debug {
                    write_activity_debug_header(out, file, spec).map_err(super::wrap_io)?;
                }
                write_activity_block(out, file, cfg, &info, spec, section, block)?;
            }
        }
        if let Some(r) = blocks.restarts.get(block) {
            write_restart_line(out, cfg, &info, r).map_err(super::wrap_io)?;
        }
    }
    Ok(())
}

/// RESTART 行。nodename も interval も出さず、`;` の後に空白 1 個 (§1.3)。
fn write_restart_line<W: Write>(
    out: &mut W,
    cfg: &SadfConfig,
    info: &FileInfo,
    r: &super::dbppc::RestartMark,
) -> io::Result<()> {
    let stamp = Stamp::new(cfg.time_base, r.ust_time, r.hms, info);
    writeln!(
        out,
        "{}; LINUX-RESTART ({} CPU)",
        stamp.raw(),
        display_cpu_count(r.cpu_count)
    )
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
    let mut cursor = cfg.time_filter.cursor();

    walk_items(file, &Selection::Only(vec![spec.id]), |item| {
        match item {
            // RESTART がブロックの境界 (`logic2`)。読んだ時点で次のブロックへ移る。
            WalkItem::Event(RecordEvent::Restart { .. }) => current_block += 1,
            // COMMENT は読んだ時点で出す。最後の統計レコードより後ろにあっても届く。
            WalkItem::Event(ev) => {
                if cfg.comments && current_block == block && cursor.event(ev.ust_time(), ev.time())
                {
                    emit_comment(out, &ev, cfg, info).map_err(super::wrap_io)?;
                }
            }
            WalkItem::Sample(view) => match cursor.sample(view) {
                Admit::Skip | Admit::Reference => {}
                Admit::Stop => return Ok(ScanControl::Stop),
                Admit::Emit => {
                    if current_block == block {
                        emit_sample(out, view, cfg, info, spec, section).map_err(super::wrap_io)?;
                    }
                }
            },
        }
        Ok(ScanControl::Continue)
    })?;
    Ok(())
}

/// COMMENT の 1 行 (`<時刻>; COM <本文>`)。
fn emit_comment<W: Write>(
    out: &mut W,
    ev: &RecordEvent,
    cfg: &SadfConfig,
    info: &FileInfo,
) -> io::Result<()> {
    let RecordEvent::Comment { ust_time, text, .. } = ev else {
        return Ok(());
    };
    let stamp = Stamp::new(cfg.time_base, *ust_time, ev.time(), info);
    writeln!(out, "{}; COM {text}", stamp.raw())
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
    // COMMENT はここでは出さない (走査で読んだ時点に [`emit_comment`] が出す)。
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
        ActivityId::IRQ => write_irq(out, view, &ts, cfg),
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

    for item in pair.selected_items(cfg, true) {
        let mut tok = vec![ts.to_string()];
        for (i, f) in fields.iter().enumerate() {
            if i == label_at {
                push_item_label(&mut tok, spec, section, &item, cfg);
            }
            push_raw_field(&mut tok, spec, &item, f, cfg);
        }
        if fields.len() <= label_at {
            push_item_label(&mut tok, spec, section, &item, cfg);
        }
        out.write_all(join_tokens(&tok).as_bytes())?;
    }
    Ok(())
}

/// トークンを `"; "` でつないで行にする。
///
/// raw の 1 行は `<timestr>; <名前>; <値>; …;` の形で、**行末に `;` が付く**
/// (§4.1)。区切りが一様なのでトークン列として組み立てるのが安全
/// (`;;` の二重出力のような取り違えが起きない)。
fn join_tokens(tokens: &[String]) -> String {
    let mut s = tokens.join("; ");
    s.push_str(";\n");
    s
}

/// アイテム識別子を `; <ラベル>; <値>` の形で足す。
///
/// `A_PWR_USB` は raw ではアイテムを持たない (§4.5)。
/// `A_FS` のデバイス名だけダブルクォートが付く (§14.5-5)。
fn push_item_label(
    tok: &mut Vec<String>,
    spec: &ActivitySpec,
    section: &Section,
    item: &ItemPair<'_>,
    cfg: &SadfConfig,
) {
    if spec.item == ItemKind::None || spec.id == ActivityId::PWR_USB {
        return;
    }
    // 先頭のフィールド名はそのセクションの `hdr_line` から取る
    // (`A_FS` は `-F MOUNT` で `FILESYSTEM` → `MOUNTPOINT` に変わる)。
    let head = section.hdr_line.split(';').next().unwrap_or_default();
    let label = item_label_in(spec, section, item);

    // -O debug では A_CPU のオフライン判定を名前の直後に付ける (§4.4-4)
    if cfg.debug && spec.id == ActivityId::CPU && item.ctx.tick_total == Some(0) {
        tok.push(format!("{head} [OFF]"));
    } else {
        tok.push(head.to_string());
    }
    // A_FS のデバイス名だけダブルクォートが付く (§14.5-5)
    if spec.id == ActivityId::FS {
        tok.push(format!("\"{}\"", label.db));
    } else {
        tok.push(label.db);
    }
}

fn push_raw_field(
    tok: &mut Vec<String>,
    spec: &ActivitySpec,
    item: &ItemPair<'_>,
    f: &RawField,
    cfg: &SadfConfig,
) {
    match f.style {
        RawStyle::Pval | RawStyle::PvalSum(_) | RawStyle::PvalDiff(_, _) => {
            let (prev, curr) = raw_pair(item, f);
            // -O debug ではカウンタが減少したフィールド名の直後に [DEC] (§4.4-3)
            let dec = cfg.debug
                && matches!((prev, curr), (Availability::Present(p), Availability::Present(c)) if c < p);
            tok.push(if dec {
                format!("{} [DEC]", f.name)
            } else {
                f.name.to_string()
            });
            tok.push(u64_token(prev));
            tok.push(u64_token(curr));
        }
        RawStyle::Int => {
            let v = item.raw_curr_by_name(f.col);
            tok.push(f.name.to_string());
            // A_PWR_BAT の status は値の後に名前付きの注記が入る (§4.4-5)
            if cfg.debug
                && spec.id == ActivityId::PWR_BAT
                && f.col == "status"
                && let Availability::Present(sts) = v
            {
                tok.push(format!("{} [{}]", u64_token(v), render::bat_status(sts)));
                return;
            }
            tok.push(u64_token(v));
        }
        RawStyle::Sensor => {
            tok.push(f.name.to_string());
            tok.push(match item.raw_curr_by_name(f.col) {
                Availability::Present(bits) => {
                    let mut s = String::new();
                    write_sensor(&mut s, double_from_bits(bits));
                    s
                }
                _ => ABSENT_TEXT.to_string(),
            });
        }
        RawStyle::Text | RawStyle::QuotedText => {
            tok.push(f.name.to_string());
            let text = render::field_text(spec, item, f.col).unwrap_or_default();
            tok.push(if matches!(f.style, RawStyle::QuotedText) {
                format!("\"{text}\"")
            } else {
                text
            });
        }
        RawStyle::Hex => {
            tok.push(f.name.to_string());
            tok.push(match item.raw_curr_by_name(f.col) {
                Availability::Present(v) => format!("{v:x}"),
                _ => ABSENT_TEXT.to_string(),
            });
        }
    }
}

/// 生値 1 個のトークン。
///
/// **欠落の 2 種類を区別する** (指摘 8 と同じ規則)。
///
/// | 欠落 | 出力 | 理由 |
/// |---|---|---|
/// | `UnsupportedBySource` (その世代にフィールドが無い) | `0` | 本家は「期待する型別本数よりファイル側が少なければ足りない分を 0 埋め」した構造体を読むので、`pval()` は `0` を出す (03 §1.9-1) |
/// | `MissingInSample` (フィールドはあるがこのレコードで読めていない) | 空文字 | 本家ならその行自体が無い。0 を書くと観測値と区別が付かなくなる |
///
/// `-d` / `-p` / `-j` / `-x` は [`super::write_value`] が同じ区別をする。
/// ここだけ空文字にすると**互換出力どうしで不統一**になる
/// (旧 `A_IO` の `dtps` が `-d` では `0.00`、`-r` では空欄になっていた)。
fn u64_token(v: Availability<u64>) -> String {
    match v {
        Availability::Present(x) => x.to_string(),
        Availability::UnsupportedBySource => "0".to_string(),
        Availability::MissingInSample => ABSENT_TEXT.to_string(),
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
fn write_irq<W: Write>(
    out: &mut W,
    view: &IntervalView<'_>,
    ts: &str,
    cfg: &SadfConfig,
) -> io::Result<()> {
    let Some(pair) = ActivityPair::from_view(view, ActivityId::IRQ) else {
        return Ok(());
    };
    let (nr, nr2) = pair.irq_dimensions();

    for irq in 0..nr2 {
        if !(0..nr).any(|cpu| pair.irq_cpu_selected(cfg, cpu, true)) {
            continue;
        }
        // 割り込み名は CPU "all" 行 (行 0) にのみ書かれている
        let name = pair.irq_name(irq);
        if !cfg.name_selected(ActivityId::IRQ, &name) {
            continue;
        }

        let mut tok = vec![ts.to_string(), "INTR".to_string(), name];
        for cpu in 0..nr {
            if !pair.irq_cpu_selected(cfg, cpu, true) {
                continue;
            }
            // フィールド名は `all` (CPU 0) / `CPU0` / `CPU1` … (§4.5)
            tok.push(if cpu == 0 {
                "all".to_string()
            } else {
                format!("CPU{}", cpu - 1)
            });
            match pair.irq_item(cpu, irq) {
                Some(item) => {
                    tok.push(u64_token(item.raw_prev_by_name("intr")));
                    tok.push(u64_token(item.raw_curr_by_name("intr")));
                }
                None => {
                    tok.push(ABSENT_TEXT.to_string());
                    tok.push(ABSENT_TEXT.to_string());
                }
            }
        }
        out.write_all(join_tokens(&tok).as_bytes())?;
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
    let nr = pair.curr.nr as usize;
    let nr2 = pair.curr.nr2.max(1) as usize;
    let spec = pair_spec();

    for row in 0..nr {
        if !cfg.cpus.includes(row) {
            continue;
        }
        let mut tok = vec![
            ts.to_string(),
            "CPU".to_string(),
            super::ItemLabel::cpu(row).db,
        ];
        for step in 0..nr2 {
            let Some(item) = pair.item(row * nr2 + step) else {
                break;
            };
            let freq = item.raw_curr_by_name("freq_khz");
            if matches!(freq, Availability::Present(0)) {
                break;
            }
            tok.push("freq".to_string());
            tok.push(u64_token(freq));
            push_raw_field(
                &mut tok,
                spec,
                &item,
                &RawField {
                    col: "time_in_state",
                    name: "tminst",
                    style: RawStyle::Pval,
                },
                cfg,
            );
        }
        out.write_all(join_tokens(&tok).as_bytes())?;
    }
    Ok(())
}

fn pair_spec() -> &'static ActivitySpec {
    spec::lookup(ActivityId::PWR_FREQ).expect("A_PWR_FREQ の出力定義")
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

    /// 欠落の 2 種類を区別する (指摘 8 と同じ規則)。
    ///
    /// 「その世代にフィールドが無い」は本家がゼロ補完した構造体を読むので `0`、
    /// 「このレコードで読めていない」は本家ならその行自体が無いので空文字。
    #[test]
    fn unsupported_field_is_zero_filled_but_missing_sample_is_empty() {
        assert_eq!(
            u64_token(Availability::UnsupportedBySource),
            "0",
            "本家は 0 埋めした構造体の値を出す (03 §1.9-1)"
        );
        assert_eq!(
            u64_token(Availability::MissingInSample),
            "",
            "観測できていない値に 0 を与えない"
        );
        assert_eq!(u64_token(Availability::Present(0)), "0", "正常な 0 は 0");
    }

    /// 行は `"; "` 区切りで、末尾に `;` が付く (§4.1)。
    #[test]
    fn line_is_semicolon_space_separated_with_trailing_semicolon() {
        let tok = vec![
            "13:20:19 UTC".to_string(),
            "proc/s".to_string(),
            "46972".to_string(),
            "47083".to_string(),
        ];
        assert_eq!(join_tokens(&tok), "13:20:19 UTC; proc/s; 46972; 47083;\n");
    }

    /// センサ値は小数 6 桁固定。
    #[test]
    fn sensor_values_have_six_decimals() {
        let mut s = String::new();
        write_sensor(&mut s, 1283.0);
        assert_eq!(s, "1283.000000");
    }
}

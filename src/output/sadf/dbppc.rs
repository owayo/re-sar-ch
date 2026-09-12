//! `sadf -d` (DB / CSV) と `sadf -p` (ppc) の出力。
//!
//! 本家は 1 つの `render()` に `isdb` を渡して両方を作る (§8)。ここも同じ構造で、
//! **区切り文字と行の切り方**だけが違う。
//!
//! | | `-d` | `-p` |
//! |---|---|---|
//! | 区切り | `;` | タブ |
//! | 行 | 1 アイテム = 1 行 (`-h` なら 1 サンプル = 1 行) | 1 メトリック = 1 行 |
//! | フィールド名 | ブロック先頭の `# …` 行に 1 回 | 毎行に入る |
//! | アイテム識別子 | `-1` / `<N>` / 名前 | `all` / `cpu<N>` / 名前、無ければ `-` |
//!
//! 表示ループは `logic2` = **activity 順**。RESTART でブロックが切れ、
//! ブロックごとに全 activity を出し直す (§0.3)。

use std::io::{self, Write};

use super::access::ActivityPair;
use super::render::item_label;
use super::spec::{ActivitySpec, Section};
use super::{
    ABSENT_TEXT, EVENT_INTERVAL, FileInfo, SadfConfig, Stamp, interval_secs, render, spec,
};
use crate::error::Result;
use crate::format::file::{SaFile, ScanControl};
use crate::model::ActivityId;
use crate::series::{IntervalView, RecordEvent, Selection, walk};

/// 区切り文字。`seps[isdb]` (`rndr_stats.c`)。
const SEPS: [&str; 2] = ["\t", ";"];

/// `-d` の出力。
pub fn write_db<W: Write>(out: &mut W, file: &SaFile, cfg: &SadfConfig) -> Result<()> {
    write_dbppc(out, file, cfg, true)
}

/// `-p` の出力。
pub fn write_ppc<W: Write>(out: &mut W, file: &SaFile, cfg: &SadfConfig) -> Result<()> {
    write_dbppc(out, file, cfg, false)
}

fn write_dbppc<W: Write>(out: &mut W, file: &SaFile, cfg: &SadfConfig, isdb: bool) -> Result<()> {
    let info = FileInfo::from_file(file);
    let specs = present_specs(file);
    let restarts = scan_restarts(file)?;

    // -h は -d のみ有効 (§0.2)。全 activity を 1 行に連ねる別ループになる。
    if isdb && cfg.horizontally {
        return write_horizontal(out, file, cfg, &info, &specs);
    }

    for block in 0..=restarts.len() {
        for spec in &specs {
            for section in spec.active_sections(&cfg.section) {
                if isdb {
                    write_field_list(out, section, cfg).map_err(super::wrap_io)?;
                }
                write_activity_block(out, file, cfg, &info, spec, section, isdb, block)?;
            }
        }
        // ブロックを閉じる RESTART 行 (ブロックごとに 1 回)
        if let Some(r) = restarts.get(block) {
            write_restart(out, cfg, &info, r, isdb).map_err(super::wrap_io)?;
        }
    }
    Ok(())
}

// ===========================================================================
// フィールド名一覧行 (`-d` のみ)
// ===========================================================================

/// `# hostname;interval;timestamp;<hdr_line>` を出す。
///
/// **activity ブロックの先頭で毎回**出る。`-A` のように多数選ぶとブロック数だけ
/// 現れ、RESTART の後も再度出る (§3.1)。
fn write_field_list<W: Write>(out: &mut W, section: &Section, cfg: &SadfConfig) -> io::Result<()> {
    let hdr = cfg.section.expand_hdr_line(section.hdr_line);
    writeln!(out, "# hostname;interval;timestamp;{hdr}")?;
    Ok(())
}

// ===========================================================================
// 1 activity ブロック
// ===========================================================================

#[allow(clippy::too_many_arguments)]
fn write_activity_block<W: Write>(
    out: &mut W,
    file: &SaFile,
    cfg: &SadfConfig,
    info: &FileInfo,
    spec: &ActivitySpec,
    section: &Section,
    isdb: bool,
    block: usize,
) -> Result<()> {
    let mut current_block = 0usize;

    walk(file, &Selection::Only(vec![spec.id]), |view| {
        current_block += count_restarts(view.events);
        if current_block != block {
            return Ok(ScanControl::Continue);
        }
        emit_sample(out, view, cfg, info, spec, section, isdb).map_err(super::wrap_io)?;
        Ok(ScanControl::Continue)
    })?;
    Ok(())
}

/// 1 レコード分を書き出す。
///
/// 書き込み失敗は `io::Error` のまま返し、走査との境界で 1 度だけ包む。
#[allow(clippy::too_many_arguments)]
fn emit_sample<W: Write>(
    out: &mut W,
    view: &IntervalView<'_>,
    cfg: &SadfConfig,
    info: &FileInfo,
    spec: &ActivitySpec,
    section: &Section,
    isdb: bool,
) -> io::Result<()> {
    let sep = SEPS[usize::from(isdb)];

    // COMMENT は activity ごとに (= ブロック内で何度も) 出る (§0.3)
    if cfg.comments {
        for e in view.events {
            if let RecordEvent::Comment { ust_time, text, .. } = e {
                let stamp = Stamp::new(cfg.time_base, *ust_time, e.time(), info);
                writeln!(
                    out,
                    "{}{sep}{EVENT_INTERVAL}{sep}{}{sep}COM {text}",
                    info.nodename,
                    stamp.dbppc()
                )?;
            }
        }
    }
    // 先頭レコードは基準値として消費するだけ。レートが作れないので出さない。
    if !view.has_prev || !view.continuous {
        return Ok(());
    }

    let stamp = Stamp::new(cfg.time_base, view.curr.ust_time, curr_hms(view), info);
    let pre = format!(
        "{}{sep}{}{sep}{}",
        info.nodename,
        interval_secs(view.itv_cs),
        stamp.dbppc()
    );

    if spec.id == ActivityId::IRQ {
        return write_irq(out, view, &pre, sep, isdb);
    }

    let Some(pair) = ActivityPair::from_view(view, spec.id) else {
        return Ok(());
    };
    for item in pair.output_items() {
        let label = item_label(spec, &item);
        let mut line = String::with_capacity(96);

        if isdb {
            line.push_str(&pre);
            if !label.db.is_empty() {
                line.push_str(sep);
                line.push_str(&label.db);
            }
        }

        for field in section.fields {
            if field.pp.is_empty() || !cfg.section.allows_field(field.gate) {
                continue;
            }
            let v = render::field_value(spec, &item, field, field.dp_fmt, &label, ABSENT_TEXT);
            if isdb {
                line.push_str(sep);
                line.push_str(&v);
            } else {
                // -p は 1 メトリック = 1 行
                writeln!(out, "{pre}{sep}{}{sep}{}{sep}{v}", label.ppc, field.pp)?;
            }
        }

        if isdb {
            line.push('\n');
            out.write_all(line.as_bytes())?;
        }
    }
    Ok(())
}

/// `A_IRQ` は「フィールド名の位置に CPU ラベルを入れる」逆転構造 (§11.1)。
///
/// 行列は `行 = CPU (nr)` × `列 = 割り込み (nr2)`。割り込み名は行 0 にしかない。
fn write_irq<W: Write>(
    out: &mut W,
    view: &IntervalView<'_>,
    pre: &str,
    sep: &str,
    isdb: bool,
) -> io::Result<()> {
    let Some(pair) = ActivityPair::from_view(view, ActivityId::IRQ) else {
        return Ok(());
    };
    let nr = pair.curr.nr.max(1) as usize;
    let nr2 = pair.curr.nr2.max(1) as usize;

    for irq in 0..nr2 {
        // 割り込み名は CPU "all" 行 (行 0) にのみ書かれている
        let name = pair
            .matrix_item(0, irq)
            .and_then(|i| i.key().map(|s| s.to_string()))
            .unwrap_or_else(|| irq.to_string());

        let mut line = String::with_capacity(96);
        if isdb {
            line.push_str(pre);
            line.push_str(sep);
            line.push_str(&name);
        }

        for cpu in 0..nr {
            let mut v = ABSENT_TEXT.to_string();
            if let Some(item) = pair.matrix_item(cpu, irq) {
                let mut s = String::new();
                super::write_value(
                    &mut s,
                    item.computed_by_name("intr"),
                    super::Fmt::R2,
                    ABSENT_TEXT,
                );
                v = s;
            }
            if isdb {
                line.push_str(sep);
                line.push_str(&v);
            } else {
                let cpu_label = if cpu == 0 {
                    "all".to_string()
                } else {
                    format!("cpu{}", cpu - 1)
                };
                writeln!(out, "{pre}{sep}{name}{sep}{cpu_label}{sep}{v}")?;
            }
        }

        if isdb {
            line.push('\n');
            out.write_all(line.as_bytes())?;
        }
    }
    Ok(())
}

// ===========================================================================
// `-dh` (横並び)
// ===========================================================================

/// `-h` はフィールド名一覧行に `[...]` を挟み、全 activity を 1 行に連ねる (§3.2)。
fn write_horizontal<W: Write>(
    out: &mut W,
    file: &SaFile,
    cfg: &SadfConfig,
    info: &FileInfo,
    specs: &[&'static ActivitySpec],
) -> Result<()> {
    // フィールド名一覧行 (1 回だけ)
    let mut hdr = String::from("# hostname;interval;timestamp");
    for spec in specs {
        for section in spec.active_sections(&cfg.section) {
            hdr.push(';');
            hdr.push_str(&cfg.section.expand_hdr_line(section.hdr_line));
            if multi_item(file, spec.id) {
                hdr.push_str("[...]");
            }
        }
    }
    writeln!(out, "{hdr}").map_err(super::wrap_io)?;

    let ids: Vec<ActivityId> = specs.iter().map(|s| s.id).collect();
    walk(file, &Selection::Only(ids), |view| {
        if !view.has_prev || !view.continuous {
            return Ok(ScanControl::Continue);
        }
        let line = horizontal_line(view, cfg, info, specs);
        out.write_all(line.as_bytes()).map_err(super::wrap_io)?;
        Ok(ScanControl::Continue)
    })?;
    Ok(())
}

/// `-dh` の 1 行を組み立てる。
fn horizontal_line(
    view: &IntervalView<'_>,
    cfg: &SadfConfig,
    info: &FileInfo,
    specs: &[&'static ActivitySpec],
) -> String {
    let stamp = Stamp::new(cfg.time_base, view.curr.ust_time, curr_hms(view), info);
    let mut line = format!(
        "{};{};{}",
        info.nodename,
        interval_secs(view.itv_cs),
        stamp.dbppc()
    );

    for spec in specs {
        let Some(pair) = ActivityPair::from_view(view, spec.id) else {
            continue;
        };
        for section in spec.active_sections(&cfg.section) {
            for item in pair.output_items() {
                let label = item_label(spec, &item);
                if !label.db.is_empty() {
                    line.push(';');
                    line.push_str(&label.db);
                }
                for field in section.fields {
                    if field.pp.is_empty() || !cfg.section.allows_field(field.gate) {
                        continue;
                    }
                    line.push(';');
                    line.push_str(&render::field_value(
                        spec,
                        &item,
                        field,
                        field.dp_fmt,
                        &label,
                        ABSENT_TEXT,
                    ));
                }
            }
        }
    }
    line.push('\n');
    line
}

// ===========================================================================
// RESTART
// ===========================================================================

/// RESTART レコードの位置 (`logic2` のブロック境界)。
#[derive(Debug, Clone, Copy)]
pub struct RestartMark {
    pub ust_time: u64,
    pub hms: (u8, u8, u8),
    pub cpu_count: Option<u32>,
}

/// `LINUX-RESTART` 行。
///
/// 直後は**リテラルのタブ 1 個**。区切りが `;` の `-d` でもここだけタブ (§1.3)。
fn write_restart<W: Write>(
    out: &mut W,
    cfg: &SadfConfig,
    info: &FileInfo,
    r: &RestartMark,
    isdb: bool,
) -> io::Result<()> {
    let sep = SEPS[usize::from(isdb)];
    let stamp = Stamp::new(cfg.time_base, r.ust_time, r.hms, info);
    let cpus = display_cpu_count(r.cpu_count);
    writeln!(
        out,
        "{}{sep}{EVENT_INTERVAL}{sep}{}{sep}LINUX-RESTART\t({cpus} CPU)",
        info.nodename,
        stamp.dbppc()
    )?;
    Ok(())
}

/// RESTART レコードの CPU 数表示。`sa_cpu_nr > 1 ? sa_cpu_nr - 1 : 1` (§1.3)。
pub fn display_cpu_count(cpu_count: Option<u32>) -> u32 {
    match cpu_count {
        Some(n) if n > 1 => n - 1,
        _ => 1,
    }
}

// ===========================================================================
// 共通ヘルパ
// ===========================================================================

/// ファイルに実データを持つ activity の出力定義を ID 昇順で返す。
pub fn present_specs(file: &SaFile) -> Vec<&'static ActivitySpec> {
    let mut out: Vec<&'static ActivitySpec> = Vec::new();
    for entry in file.activities() {
        if entry.nr <= 0 {
            continue;
        }
        if let Some(s) = spec::lookup(entry.id) {
            if !out.iter().any(|x| x.id == s.id) {
                out.push(s);
            }
        }
    }
    out.sort_by_key(|s| s.id.0);
    out
}

/// アイテムが 2 個以上ある activity か (`-dh` の `[...]` 判定)。
fn multi_item(file: &SaFile, id: ActivityId) -> bool {
    file.activities()
        .iter()
        .find(|e| e.id == id)
        .map(|e| e.nr > 1)
        .unwrap_or(false)
}

/// 現サンプルの「収集時ローカル時分秒」。`-t` の日付復元に使う。
fn curr_hms(view: &IntervalView<'_>) -> (u8, u8, u8) {
    (view.curr.hour, view.curr.minute, view.curr.second)
}

fn count_restarts(events: &[RecordEvent]) -> usize {
    events
        .iter()
        .filter(|e| matches!(e, RecordEvent::Restart { .. }))
        .count()
}

/// ファイル内の RESTART を先頭から順に列挙する。
///
/// `walk` は統計レコードだけを訪れるため、末尾の RESTART を取りこぼす。
/// ブロック境界は全レコードを見る必要があるので低レベルの `scan` を使う。
pub fn scan_restarts(file: &SaFile) -> Result<Vec<RestartMark>> {
    use crate::format::registry::RecordKind;
    let mut marks = Vec::new();
    file.scan(|rec| {
        if rec.kind == RecordKind::Restart {
            marks.push(RestartMark {
                ust_time: rec.ust_time,
                hms: (rec.hour, rec.minute, rec.second),
                cpu_count: rec.cpu_count,
            });
        }
        Ok(ScanControl::Continue)
    })?;
    Ok(marks)
}

/// ファイル内の COMMENT を先頭から順に列挙する (`-j` / `-x` の `comments` 用)。
pub fn scan_comments(file: &SaFile) -> Result<Vec<(u64, (u8, u8, u8), String)>> {
    use crate::format::registry::RecordKind;
    let mut out = Vec::new();
    file.scan(|rec| {
        if rec.kind == RecordKind::Comment {
            out.push((
                rec.ust_time,
                (rec.hour, rec.minute, rec.second),
                rec.comment.unwrap_or("").to_string(),
            ));
        }
        Ok(ScanControl::Continue)
    })?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `-d` のフィールド名一覧行がドキュメントの実測と一致すること。
    #[test]
    fn field_list_line_matches_upstream() {
        let cpu = spec::lookup(ActivityId::CPU).unwrap();
        let cfg = SadfConfig::default();
        let mut buf = Vec::new();
        // -u ALL (= -A) 相当の 2 番目のセクション
        let section = cpu.active_sections(&cfg.section).next().unwrap();
        write_field_list(&mut buf, section, &cfg).unwrap();
        assert_eq!(
            String::from_utf8(buf).unwrap(),
            "# hostname;interval;timestamp;CPU;%usr;%nice;%sys;%iowait;%steal;%irq;%soft;%guest;%gnice;%idle\n"
        );
    }

    #[test]
    fn pcsw_field_list_line() {
        let pcsw = spec::lookup(ActivityId::PCSW).unwrap();
        let cfg = SadfConfig::default();
        let mut buf = Vec::new();
        write_field_list(&mut buf, &pcsw.sections[0], &cfg).unwrap();
        assert_eq!(
            String::from_utf8(buf).unwrap(),
            "# hostname;interval;timestamp;proc/s;cswch/s\n"
        );
    }

    /// A_IRQ のヘッダは `INTR;CPU*`。
    #[test]
    fn irq_field_list_keeps_star() {
        let irq = spec::lookup(ActivityId::IRQ).unwrap();
        let cfg = SadfConfig::default();
        let mut buf = Vec::new();
        write_field_list(&mut buf, &irq.sections[0], &cfg).unwrap();
        assert_eq!(
            String::from_utf8(buf).unwrap(),
            "# hostname;interval;timestamp;INTR;CPU*\n"
        );
    }

    /// A_FS のヘッダは `-F MOUNT` で 1 列目が変わる。
    #[test]
    fn fs_field_list_switches_on_mount() {
        let fs = spec::lookup(ActivityId::FS).unwrap();
        let mut cfg = SadfConfig::default();
        let mut buf = Vec::new();
        let sec = fs.active_sections(&cfg.section).next().unwrap();
        write_field_list(&mut buf, sec, &cfg).unwrap();
        assert!(String::from_utf8(buf).unwrap().contains(";FILESYSTEM;"));

        cfg.section.fs_mount = true;
        let mut buf = Vec::new();
        let sec = fs.active_sections(&cfg.section).next().unwrap();
        write_field_list(&mut buf, sec, &cfg).unwrap();
        assert!(String::from_utf8(buf).unwrap().contains(";MOUNTPOINT;"));
    }

    /// `LINUX-RESTART` の直後はタブ。`-d` でもここだけ `;` ではない。
    #[test]
    fn restart_line_uses_literal_tab() {
        let info = FileInfo {
            nodename: "testhost".into(),
            sysname: "Linux".into(),
            release: "5.0.0".into(),
            machine: "x86_64".into(),
            cpu_count: 8,
            file_date: "2019-04-18".into(),
            file_utc_time: "13:20:09".into(),
            ust_time: 1_555_593_609,
            tzname: String::new(),
        };
        let r = RestartMark {
            ust_time: 1_555_594_649,
            hms: (13, 37, 29),
            cpu_count: Some(10),
        };
        let cfg = SadfConfig::default();

        let mut buf = Vec::new();
        write_restart(&mut buf, &cfg, &info, &r, true).unwrap();
        assert_eq!(
            String::from_utf8(buf).unwrap(),
            "testhost;-1;2019-04-18 13:37:29 UTC;LINUX-RESTART\t(9 CPU)\n"
        );

        let mut buf = Vec::new();
        write_restart(&mut buf, &cfg, &info, &r, false).unwrap();
        assert_eq!(
            String::from_utf8(buf).unwrap(),
            "testhost\t-1\t2019-04-18 13:37:29 UTC\tLINUX-RESTART\t(9 CPU)\n"
        );
    }

    #[test]
    fn cpu_count_subtracts_the_aggregate_slot() {
        assert_eq!(display_cpu_count(Some(9)), 8);
        assert_eq!(display_cpu_count(Some(1)), 1);
        assert_eq!(display_cpu_count(None), 1);
    }
}

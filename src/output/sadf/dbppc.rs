//! `sadf -d` (DB / CSV) と `sadf -p` (ppc) の出力。
//!
//! 本家は 1 つの `render()` に `isdb` を渡して両方を作る (§8)。ここも同じ構造で、
//! **区切り文字と行の切り方**だけが違う。
//!
//! | | `-d` | `-p` |
//! |---|---|---|
//! | 区切り | `;` | タブ |
//! | 行 | 1 アイテム = 1 行 (`-h` なら 1 サンプル = 1 行) | 1 メトリック = 1 行 |
//! | フィールド名 | パス先頭の `# …` 行に 1 回 | 毎行に入る |
//! | アイテム識別子 | `-1` / `<N>` / 名前 | `all` / `cpu<N>` / 名前、無ければ `-` |
//!
//! 表示ループは `logic2` = **activity 順** (エンジンは `logic2` モジュール)。
//! どのレコードを何回出すかはそちらが決め、ここは 1 行の書式だけを持つ。

use std::io::{self, Write};

use super::access::{ActivityPair, ZeroActivity};
use super::logic2::{self, Logic2Sink, Pass, Shown};
use super::records::Rec;
use super::render::item_label_in;
use super::spec::{ActivitySpec, ItemKind, Section};
use super::{
    ABSENT_TEXT, EVENT_INTERVAL, FileInfo, SadfConfig, SadfExtra, Stamp, interval_secs, render,
    spec,
};
use crate::error::Result;
use crate::format::file::SaFile;
use crate::model::ActivityId;
use crate::series::IntervalView;

/// 区切り文字。`seps[isdb]` (`rndr_stats.c`)。
const SEPS: [&str; 2] = ["\t", ";"];

/// `-d` の出力。
pub fn write_db<W: Write>(out: &mut W, file: &SaFile, cfg: &SadfConfig) -> Result<()> {
    write_db_with(out, file, cfg, &SadfExtra::default())
}

/// `-d` の出力 (`interval` / `count` と `-T` の TZ 名を指定する)。
pub fn write_db_with<W: Write>(
    out: &mut W,
    file: &SaFile,
    cfg: &SadfConfig,
    extra: &SadfExtra,
) -> Result<()> {
    write_dbppc(out, file, cfg, extra, true)
}

/// `-p` の出力。
pub fn write_ppc<W: Write>(out: &mut W, file: &SaFile, cfg: &SadfConfig) -> Result<()> {
    write_ppc_with(out, file, cfg, &SadfExtra::default())
}

/// `-p` の出力 (`interval` / `count` と `-T` の TZ 名を指定する)。
pub fn write_ppc_with<W: Write>(
    out: &mut W,
    file: &SaFile,
    cfg: &SadfConfig,
    extra: &SadfExtra,
) -> Result<()> {
    write_dbppc(out, file, cfg, extra, false)
}

fn write_dbppc<W: Write>(
    out: &mut W,
    file: &SaFile,
    cfg: &SadfConfig,
    extra: &SadfExtra,
    isdb: bool,
) -> Result<()> {
    let info = FileInfo::from_config(file, cfg, extra);
    let specs = selected_specs(file, cfg);
    let ids: Vec<ActivityId> = specs.iter().map(|s| s.id).collect();
    // -h は -d のみ有効 (§0.2)。全 activity を 1 行に連ねる 1 パスになる。
    let horizontal = isdb && cfg.horizontally;
    let passes = if horizontal {
        vec![Pass::Horizontal]
    } else {
        activity_passes(&specs, cfg)
    };
    let mut sink = DbPpcSink {
        out,
        cfg,
        info,
        isdb,
        hdr: if horizontal {
            horizontal_field_list(file, cfg)
        } else {
            String::new()
        },
        horizontal: if horizontal {
            act_entries(file, cfg, &specs)
        } else {
            Vec::new()
        },
    };
    logic2::run(file, cfg, extra.select, &passes, &ids, false, &mut sink)
}

/// activity (ファイル記載順 = `id_seq[]`) × 有効なセクションのパス列。
pub(super) fn activity_passes(specs: &[&'static ActivitySpec], cfg: &SadfConfig) -> Vec<Pass> {
    specs
        .iter()
        .flat_map(|spec| {
            spec.active_sections(&cfg.section)
                .map(move |section| Pass::Activity { spec, section })
        })
        .collect()
}

/// `-d` / `-p` の書き出し。
struct DbPpcSink<'w, 'c, W: Write> {
    out: &'w mut W,
    cfg: &'c SadfConfig,
    info: FileInfo,
    isdb: bool,
    /// `-dh` のフィールド名一覧行。
    hdr: String,
    /// `-dh` で 1 行に並べる activity (`act[]` 順)。
    horizontal: Vec<ActEntry>,
}

impl<W: Write> Logic2Sink for DbPpcSink<'_, '_, W> {
    fn restart(&mut self, rec: &Rec, cpu_nr: Option<u32>) -> io::Result<()> {
        write_restart(self.out, self.cfg, &self.info, rec, cpu_nr, self.isdb)
    }

    fn comment(&mut self, rec: &Rec) -> io::Result<()> {
        emit_comment(self.out, rec, self.cfg, &self.info, self.isdb)
    }

    fn begin_pass(&mut self, pass: &Pass) -> io::Result<()> {
        // フィールド名一覧行は -d だけ (`FO_FIELD_LIST`)
        if !self.isdb {
            return Ok(());
        }
        match pass {
            Pass::Activity { section, .. } => write_field_list(self.out, section, self.cfg),
            Pass::Horizontal => writeln!(self.out, "{}", self.hdr),
        }
    }

    fn sample(&mut self, pass: &Pass, shown: &Shown<'_, '_>) -> io::Result<()> {
        match pass {
            Pass::Activity { spec, section } => emit_sample(
                self.out, shown.view, self.cfg, &self.info, spec, section, self.isdb,
            ),
            Pass::Horizontal => {
                let line = horizontal_line(shown.view, self.cfg, &self.info, &self.horizontal);
                self.out.write_all(line.as_bytes())
            }
        }
    }
}

// ===========================================================================
// フィールド名一覧行 (`-d` のみ)
// ===========================================================================

/// `# hostname;interval;timestamp;<hdr_line>` を出す。
///
/// **パスの先頭で毎回**出る。`-A` のように多数選ぶとパスの数だけ現れ、
/// RESTART の後の区間でも再度出る (§3.1)。
fn write_field_list<W: Write>(out: &mut W, section: &Section, cfg: &SadfConfig) -> io::Result<()> {
    let hdr = cfg.section.expand_hdr_line(section.hdr_line);
    writeln!(out, "# hostname;interval;timestamp;{hdr}")?;
    Ok(())
}

// ===========================================================================
// 1 レコード
// ===========================================================================

/// COMMENT の 1 行 (`COM <本文>`)。区間は `EVENT_INTERVAL` 固定。
fn emit_comment<W: Write>(
    out: &mut W,
    rec: &Rec,
    cfg: &SadfConfig,
    info: &FileInfo,
    isdb: bool,
) -> io::Result<()> {
    let sep = SEPS[usize::from(isdb)];
    let stamp = Stamp::new(cfg.time_base, rec.ust_time, rec.hms, info);
    writeln!(
        out,
        "{}{sep}{EVENT_INTERVAL}{sep}{}{sep}COM {}",
        info.nodename,
        stamp.dbppc_event(),
        rec.comment.as_deref().unwrap_or("")
    )
}

/// `-d` にアイテム識別子の列があるか。
///
/// 識別子が空文字でも列は出る (`-F MOUNT` で旧世代のマウントポイントが
/// 空のとき、本家は空のフィールドを出す)。列が無いのはアイテムを持たない
/// activity だけ。
fn has_item_column(spec: &ActivitySpec) -> bool {
    spec.item != ItemKind::None
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

    let stamp = Stamp::new(cfg.time_base, view.curr.ust_time, curr_hms(view), info);
    let pre = format!(
        "{}{sep}{}{sep}{}",
        info.nodename,
        interval_secs(view.itv_cs),
        stamp.dbppc()
    );

    if spec.id == ActivityId::IRQ {
        return write_irq(out, view, &pre, sep, isdb, cfg);
    }

    let Some(pair) = ActivityPair::from_view(view, spec.id) else {
        return Ok(());
    };
    for item in pair.selected_items(cfg, false) {
        let label = item_label_in(spec, section, &item);
        let mut line = String::with_capacity(96);

        if isdb {
            line.push_str(&pre);
            if has_item_column(spec) {
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

/// `A_IRQ` は「フィールド名の位置に CPU ラベルを入れる」逆転構造 (03 §11.1)。
///
/// 行列は `行 = CPU (nr)` × `列 = 割り込み (nr2)`。割り込み名は行 0 にしかない。
fn write_irq<W: Write>(
    out: &mut W,
    view: &IntervalView<'_>,
    pre: &str,
    sep: &str,
    isdb: bool,
    cfg: &SadfConfig,
) -> io::Result<()> {
    let Some(pair) = ActivityPair::from_view(view, ActivityId::IRQ) else {
        return Ok(());
    };
    let (nr, nr2) = pair.irq_dimensions();

    for irq in 0..nr2 {
        if !(0..nr).any(|cpu| pair.irq_cpu_selected(cfg, cpu, false)) {
            continue;
        }
        // 割り込み名は CPU "all" 行 (行 0) にのみ書かれている
        let name = pair.irq_name(irq);
        if !cfg.name_selected(ActivityId::IRQ, &name) {
            continue;
        }

        let mut line = String::with_capacity(96);
        if isdb {
            line.push_str(pre);
            line.push_str(sep);
            line.push_str(&name);
        }

        for cpu in 0..nr {
            if !pair.irq_cpu_selected(cfg, cpu, false) {
                continue;
            }
            let mut v = ABSENT_TEXT.to_string();
            if let Some(item) = pair.irq_item(cpu, irq) {
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

/// `-dh` のフィールド名一覧行 (`list_fields(ALL_ACTIVITIES)`)。
///
/// 並びは**固定の `act[]` 順** (ファイルの記載順ではない、§9.4)。
/// 本家は `IS_SELECTED && nr_ini > 0` で判定するので、形式が未知で
/// データを出さない activity (`[Unknown format]`) も一覧行には載る。
/// アイテムが 2 個以上ある activity の後には `[...]` が付く (§3.2)。
fn horizontal_field_list(file: &SaFile, cfg: &SadfConfig) -> String {
    let mut hdr = String::from("# hostname;interval;timestamp");
    for spec in spec::SPECS {
        let Some(entry) = file.activities().iter().find(|e| e.id == spec.id) else {
            continue;
        };
        if entry.nr <= 0
            || cfg
                .activities
                .as_ref()
                .is_some_and(|ids| !ids.contains(&spec.id))
        {
            continue;
        }
        for section in spec.active_sections(&cfg.section) {
            hdr.push(';');
            hdr.push_str(&cfg.section.expand_hdr_line(section.hdr_line));
            if entry.nr > 1 {
                hdr.push_str("[...]");
            }
        }
    }
    hdr
}

/// `act[]` 順に回す形式 (`-j` / `-x` / `-dh`) の表示対象 1 つ。
#[derive(Debug)]
pub(super) struct ActEntry {
    pub spec: &'static ActivitySpec,
    /// 形式が未知で読めない単一 item の activity は 0 埋めの値で出る
    /// ([`ZeroActivity`] の doc)。
    zero: Option<ZeroActivity>,
}

impl ActEntry {
    /// このレコードでの前後 1 対。
    pub fn pair<'a>(&'a self, view: &'a IntervalView<'a>) -> Option<ActivityPair<'a>> {
        match &self.zero {
            Some(z) => Some(z.pair(view.itv_cs)),
            None => ActivityPair::from_view(view, self.spec.id),
        }
    }
}

/// `act[]` 順の表示対象 (`-j` / `-x` / `-dh`)。
///
/// 本家の `generic_write_stats()` / `list_fields()` は `act[]` 配列を
/// 先頭から回すので、`logic1` の形式と `-dh` の並びはファイルの記載順に
/// よらない (§9.4)。`-d` / `-p` / `-r` の縦並びは `id_seq[]` (記載順)。
/// [`spec::SPECS`] は `act[]` と同じ順に並んでいる。
///
/// `displayed` は `id_seq[]` に入る (読める) activity ([`selected_specs`])。
/// それに加え、選択されていてファイルに載っているが形式が未知の
/// **単一 item** の activity を 0 埋めで並べる (本家の静的初期値 `nr = 1` による)。
pub(super) fn act_entries(
    file: &SaFile,
    cfg: &SadfConfig,
    displayed: &[&'static ActivitySpec],
) -> Vec<ActEntry> {
    spec::SPECS
        .iter()
        .filter_map(|s| {
            if displayed.iter().any(|x| x.id == s.id) {
                return Some(ActEntry {
                    spec: s,
                    zero: None,
                });
            }
            let unreadable = s.item == ItemKind::None
                && !file.displays_activity(s.id)
                && file.activities().iter().any(|e| e.id == s.id && e.nr > 0)
                && cfg
                    .activities
                    .as_ref()
                    .is_none_or(|ids| ids.contains(&s.id));
            if !unreadable {
                return None;
            }
            Some(ActEntry {
                spec: s,
                zero: Some(ZeroActivity::new(s.id)?),
            })
        })
        .collect()
}

/// `-dh` の 1 行を組み立てる。
fn horizontal_line(
    view: &IntervalView<'_>,
    cfg: &SadfConfig,
    info: &FileInfo,
    entries: &[ActEntry],
) -> String {
    let stamp = Stamp::new(cfg.time_base, view.curr.ust_time, curr_hms(view), info);
    let mut line = format!(
        "{};{};{}",
        info.nodename,
        interval_secs(view.itv_cs),
        stamp.dbppc()
    );

    for entry in entries {
        let spec = entry.spec;
        let Some(pair) = entry.pair(view) else {
            continue;
        };
        // `IS_SELECTED && nr[curr] > 0` の activity だけが並ぶ
        if pair.is_empty() {
            continue;
        }
        if spec.id == ActivityId::IRQ {
            let (cpus, irqs) = pair.irq_dimensions();
            for irq in 0..irqs {
                let name = pair.irq_name(irq);
                if !cfg.name_selected(ActivityId::IRQ, &name) {
                    continue;
                }
                line.push(';');
                line.push_str(&name);
                for cpu in 0..cpus {
                    if !pair.irq_cpu_selected(cfg, cpu, false) {
                        continue;
                    }
                    line.push(';');
                    if let Some(item) = pair.irq_item(cpu, irq) {
                        super::write_value(
                            &mut line,
                            item.computed_by_name("intr"),
                            super::Fmt::R2,
                            ABSENT_TEXT,
                        );
                    }
                }
            }
            continue;
        }
        for section in spec.active_sections(&cfg.section) {
            for item in pair.selected_items(cfg, false) {
                let label = item_label_in(spec, section, &item);
                if has_item_column(spec) {
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

/// `LINUX-RESTART` 行。
///
/// 直後は**リテラルのタブ 1 個**。区切りが `;` の `-d` でもここだけタブ (§1.3)。
/// CPU 数は「その時点で有効な `sa_cpu_nr`」
/// ([`super::records::CpuNrTracker`])。
fn write_restart<W: Write>(
    out: &mut W,
    cfg: &SadfConfig,
    info: &FileInfo,
    rec: &Rec,
    cpu_nr: Option<u32>,
    isdb: bool,
) -> io::Result<()> {
    let sep = SEPS[usize::from(isdb)];
    let stamp = Stamp::new(cfg.time_base, rec.ust_time, rec.hms, info);
    let cpus = display_cpu_count(cpu_nr);
    writeln!(
        out,
        "{}{sep}{EVENT_INTERVAL}{sep}{}{sep}LINUX-RESTART\t({cpus} CPU)",
        info.nodename,
        stamp.dbppc_event()
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

/// ファイルに実データを持つ activity の出力定義をファイル内の順序で返す。
///
/// **現行世代のファイルでは**、参照する `sar` 版と形式が違う activity を除く。
/// 本家 `sa_common.c: check_file_actlst()` が `ACTIVITY_MAGIC_UNKNOWN` を
/// 立てた activity は `id_seq[]` に入らないので、`sadf` のデータブロックも
/// 出ない (`-H` の一覧には `[Unknown format]` 付きで出る)。
/// 判定は [`SaFile::displays_activity`]。世代の扱い (旧世代では弾かない理由)
/// もそちらの doc にまとめてある。`sar` テキスト側と同じ関数を使う。
///
/// 独自出力 (`table` / `json` / `csv` / `ndjson`) はこの関数を通らず、
/// 世代を問わず読めたものをすべて出す。
pub fn present_specs(file: &SaFile) -> Vec<&'static ActivitySpec> {
    let mut out: Vec<&'static ActivitySpec> = Vec::new();
    for entry in file.activities() {
        if entry.nr <= 0 {
            continue;
        }
        if !file.displays_activity(entry.id) {
            continue;
        }
        if let Some(s) = spec::lookup(entry.id)
            && !out.iter().any(|x| x.id == s.id)
        {
            out.push(s);
        }
    }
    out
}

/// [`present_specs`] の結果を [`SadfConfig::activities`] で絞る。
///
/// `cfg.activities` が `None` なら絞らない (`-- -A` 相当)。
pub fn selected_specs(file: &SaFile, cfg: &SadfConfig) -> Vec<&'static ActivitySpec> {
    let specs = present_specs(file);
    match &cfg.activities {
        None => specs,
        Some(ids) => specs.into_iter().filter(|s| ids.contains(&s.id)).collect(),
    }
}

/// 選択した activity のうち 1 つでもファイルに載っているか。
///
/// 本家 `check_file_actlst()` の「Requested activities not available in file」は
/// **形式が未知の activity も「載っている」と数える** (選択を外すのは
/// ファイルに無い activity だけ)。そうした activity しか選ばなかった場合は
/// エラーにならず、RESTART 行 (`-j` / `-x` はタイムスタンプだけ) を出して
/// 正常終了する。
pub fn any_selected_in_file(file: &SaFile, cfg: &SadfConfig) -> bool {
    file.activities().iter().any(|e| {
        spec::lookup(e.id).is_some()
            && cfg
                .activities
                .as_ref()
                .is_none_or(|ids| ids.contains(&e.id))
    })
}

/// 現サンプルの「収集時ローカル時分秒」。`-t` の日付復元に使う。
fn curr_hms(view: &IntervalView<'_>) -> (u8, u8, u8) {
    (view.curr.hour, view.curr.minute, view.curr.second)
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

    fn info() -> FileInfo {
        FileInfo {
            nodename: "testhost".into(),
            sysname: "Linux".into(),
            release: "5.0.0".into(),
            machine: "x86_64".into(),
            cpu_count: 8,
            file_date: "2019-04-18".into(),
            file_utc_time: "13:20:09".into(),
            ust_time: 1_555_593_609,
            tzname: String::new(),
            local_tz: "+00:00".into(),
        }
    }

    fn restart_rec() -> Rec {
        Rec {
            kind: super::super::records::RecKind::Restart,
            ust_time: 1_555_594_649,
            hms: (13, 37, 29),
            uptime_cs: 0,
            cpu_count: Some(10),
            comment: None,
        }
    }

    /// `LINUX-RESTART` の直後はタブ。`-d` でもここだけ `;` ではない。
    #[test]
    fn restart_line_uses_literal_tab() {
        let cfg = SadfConfig::default();
        let r = restart_rec();

        let mut buf = Vec::new();
        write_restart(&mut buf, &cfg, &info(), &r, Some(10), true).unwrap();
        assert_eq!(
            String::from_utf8(buf).unwrap(),
            "testhost;-1;2019-04-18 13:37:29 UTC;LINUX-RESTART\t(9 CPU)\n"
        );

        let mut buf = Vec::new();
        write_restart(&mut buf, &cfg, &info(), &r, Some(10), false).unwrap();
        assert_eq!(
            String::from_utf8(buf).unwrap(),
            "testhost\t-1\t2019-04-18 13:37:29 UTC\tLINUX-RESTART\t(9 CPU)\n"
        );
    }

    /// **回帰テスト**: `-t` かつ `sa_tzname` が空でも RESTART 行は
    /// TZ 前の区切り空白を残す (統計行は省く、`print_dbppc_restart()`)。
    #[test]
    fn restart_line_keeps_the_tz_separator_under_true_time() {
        let cfg = SadfConfig {
            time_base: super::super::TimeBase::TrueTime,
            ..Default::default()
        };
        let mut buf = Vec::new();
        write_restart(&mut buf, &cfg, &info(), &restart_rec(), Some(10), true).unwrap();
        assert_eq!(
            String::from_utf8(buf).unwrap(),
            "testhost;-1;2019-04-18 13:37:29 ;LINUX-RESTART\t(9 CPU)\n"
        );
    }

    #[test]
    fn cpu_count_subtracts_the_aggregate_slot() {
        assert_eq!(display_cpu_count(Some(9)), 8);
        assert_eq!(display_cpu_count(Some(1)), 1);
        assert_eq!(display_cpu_count(None), 1);
    }

    /// **回帰テスト (activity の並び)**: `-j` / `-x` / `-dh` が回す表は
    /// `act[]` 順で、hugepages は memory の直後 (ファイル記載順ではない、§9.4)。
    ///
    /// 以前はファイル記載順に並べていたため、A_HUGE を末尾寄りに記載する
    /// 9.1.6 のファイルで `hugepages` が `power-management` の後ろに出ていた。
    #[test]
    fn act_order_puts_hugepages_after_memory() {
        let pos = |id| spec::SPECS.iter().position(|s| s.id == id).unwrap();
        assert_eq!(pos(ActivityId::HUGE), pos(ActivityId::MEMORY) + 1);
        assert!(pos(ActivityId::HUGE) < pos(ActivityId::NET_DEV));
        assert!(pos(ActivityId::PWR_USB) < pos(ActivityId::FS));
    }
}

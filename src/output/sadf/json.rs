//! `sadf -j` (JSON) の出力。
//!
//! 表示ループは `logic1` = **時刻順** (エンジンは `logic1` モジュール)。1 タイムスタンプの中に
//! 全 activity を並べ、`restarts` / `comments` は `statistics` を全部出し終わった後に
//! まとめて出る (§0.3)。
//!
//! # 書式の要点 (§9)
//!
//! - インデントは**タブ**。空白ではない。
//! - カンマは「次要素の直前」に出す。最後の要素に末尾カンマは付かない。
//! - 小数は**全フィールド 2 桁固定** (`MBfsfree` / `MBfsused` だけ 0 桁)。
//!   `--dec=` も `--human` も `sadf` には無い。
//! - activity の並びは**固定の `act[]` 順** (ファイルの記載順ではない、§9.4)。
//! - `network` / `power-management` / `psi` は遅延オープンのラッパ。
//!   中身が 1 つも出ないときはラッパごと出ない。
//! - 配列型の activity は、そのレコードに item があれば (`nr[curr] > 0`)
//!   絞り込みで 0 件になっても**空の配列**として出る。
//! - `next_slice()` で省いたレコードや `-e` を超えたレコードは
//!   **空のオブジェクト**になる (本家の実測。理由は `logic1` モジュールの doc)。
//! - キー名は XML の属性名と**微妙に違う** (§10.6)。機械変換してはいけない。

use std::fmt::Write as _;
use std::io::{self, Write};

use super::access::{ActivityPair, CompatFormat};
use super::dbppc::{ActEntry, act_entries, display_cpu_count, selected_specs};
use super::logic1::{self, Logic1Sink};
use super::records::Rec;
use super::render::{item_label_in, jx_fields};
use super::spec::{ActivitySpec, Fmt, Group, Shape};
use super::{ABSENT_JSON, FileInfo, SadfConfig, SadfExtra, Stamp, interval_secs, render};
use crate::error::Result;
use crate::format::file::SaFile;
use crate::model::ActivityId;
use crate::series::IntervalView;

/// `-j` の出力。
pub fn write_json<W: Write>(out: &mut W, file: &SaFile, cfg: &SadfConfig) -> Result<()> {
    write_json_with(out, file, cfg, &SadfExtra::default())
}

/// `-j` の出力 (`interval` / `count` と `-T` の TZ 名を指定する)。
pub fn write_json_with<W: Write>(
    out: &mut W,
    file: &SaFile,
    cfg: &SadfConfig,
    extra: &SadfExtra,
) -> Result<()> {
    let info = FileInfo::from_config(file, cfg, extra);
    let displayed = selected_specs(file, cfg);
    let ids: Vec<ActivityId> = displayed.iter().map(|s| s.id).collect();
    let acts = act_entries(file, cfg, &displayed);

    write_prologue(out, &info).map_err(super::wrap_io)?;

    // ---- statistics (時刻順に 1 回走査) ----
    let mut sink = JsonSink {
        out,
        cfg,
        info: &info,
        acts: &acts,
        first: true,
    };
    let specials = logic1::run(file, cfg, extra.select, &ids, &mut sink)?;
    let first = sink.first;
    if !first {
        writeln!(out).map_err(super::wrap_io)?;
    }
    write_epilogue(out, cfg, &info, &specials.restarts, &specials.comments)
        .map_err(super::wrap_io)?;
    Ok(())
}

/// `statistics` の要素を書き出す。
struct JsonSink<'w, 'c, W: Write> {
    out: &'w mut W,
    cfg: &'c SadfConfig,
    info: &'c FileInfo,
    /// 出す activity (`act[]` 順)。
    acts: &'c [ActEntry],
    /// まだ 1 要素も出していないか (カンマの要否)。
    first: bool,
}

impl<W: Write> Logic1Sink for JsonSink<'_, '_, W> {
    fn record(&mut self, view: Option<&IntervalView<'_>>) -> io::Result<()> {
        let sep = if self.first { "" } else { ",\n" };
        self.first = false;
        match view {
            Some(view) => {
                let body = sample_body(view, self.cfg, self.info, self.acts);
                write!(self.out, "{sep}\t\t\t\t{{\n{body}\n\t\t\t\t}}")
            }
            // 表示しなかったレコード: `f_statistics(F_MAIN)` の `{` だけが出る
            None => write!(self.out, "{sep}\t\t\t\t{{\n\t\t\t\t}}"),
        }
    }
}

/// `"statistics": [` までのヘッダ部。
///
/// カンマは「次要素の直前」に出すイディオムなので、`"timezone"` 行の
/// 末尾カンマまではここで確定できる (§9.2)。
fn write_prologue<W: Write>(out: &mut W, info: &FileInfo) -> io::Result<()> {
    writeln!(out, "{{\"sysstat\": {{")?;
    writeln!(out, "\t\"hosts\": [")?;
    writeln!(out, "\t\t{{")?;
    writeln!(out, "\t\t\t\"nodename\": \"{}\",", esc(&info.nodename))?;
    writeln!(out, "\t\t\t\"sysname\": \"{}\",", esc(&info.sysname))?;
    writeln!(out, "\t\t\t\"release\": \"{}\",", esc(&info.release))?;
    writeln!(out, "\t\t\t\"machine\": \"{}\",", esc(&info.machine))?;
    writeln!(out, "\t\t\t\"number-of-cpus\": {},", info.cpu_count)?;
    writeln!(out, "\t\t\t\"file-date\": \"{}\",", info.file_date)?;
    writeln!(out, "\t\t\t\"file-utc-time\": \"{}\",", info.file_utc_time)?;
    writeln!(out, "\t\t\t\"timezone\": \"{}\",", esc(&info.tzname))?;
    writeln!(out, "\t\t\t\"statistics\": [")?;
    Ok(())
}

/// `statistics` を閉じてから `restarts` / `comments` を出す。
///
/// `logic1` はファイルを 3 回走査するので、この 2 つは必ず統計の**後**に来る。
/// どちらも `-s` / `-e` の範囲外は出ない (`print_special_record()`)。
fn write_epilogue<W: Write>(
    out: &mut W,
    cfg: &SadfConfig,
    info: &FileInfo,
    restarts: &[(Rec, Option<u32>)],
    comments: &[Rec],
) -> io::Result<()> {
    writeln!(out, "\t\t\t],")?;

    writeln!(out, "\t\t\t\"restarts\": [")?;
    for (i, (r, cpu_nr)) in restarts.iter().enumerate() {
        let s = Stamp::new(cfg.time_base, r.ust_time, r.hms, info);
        if i > 0 {
            writeln!(out, ",")?;
        }
        write!(
            out,
            "\t\t\t\t{{\n\t\t\t\t\t\"boot\": {{\"date\": \"{}\", \"time\": \"{}\", \"tz\": \"{}\", \"cpu_count\": {}}}\n\t\t\t\t}}",
            s.date,
            s.time,
            esc(&s.tz),
            display_cpu_count(*cpu_nr)
        )?;
    }
    if !restarts.is_empty() {
        writeln!(out)?;
    }

    if cfg.comments {
        writeln!(out, "\t\t\t],")?;
        writeln!(out, "\t\t\t\"comments\": [")?;
        for (i, c) in comments.iter().enumerate() {
            let s = Stamp::new(cfg.time_base, c.ust_time, c.hms, info);
            if i > 0 {
                writeln!(out, ",")?;
            }
            write!(
                out,
                "\t\t\t\t{{\n\t\t\t\t\t\"comment\": {{\"date\": \"{}\", \"time\": \"{}\", \"tz\": \"{}\", \"com\": \"{}\"}}\n\t\t\t\t}}",
                s.date,
                s.time,
                esc(&s.tz),
                esc(c.comment.as_deref().unwrap_or(""))
            )?;
        }
        if !comments.is_empty() {
            writeln!(out)?;
        }
    }
    writeln!(out, "\t\t\t]")?;
    writeln!(out, "\t\t}}")?;
    writeln!(out, "\t]")?;
    writeln!(out, "}}}}")?;
    Ok(())
}

// ===========================================================================
// 1 サンプル
// ===========================================================================

/// 1 タイムスタンプ分の中身 (`"timestamp"` + 各 activity) を組み立てる。
///
/// 1 サンプルぶんだけを文字列にする。全レコードを溜めることはしない。
fn sample_body(
    view: &IntervalView<'_>,
    cfg: &SadfConfig,
    info: &FileInfo,
    acts: &[ActEntry],
) -> String {
    let stamp = Stamp::new(
        cfg.time_base,
        view.curr.ust_time,
        (view.curr.hour, view.curr.minute, view.curr.second),
        info,
    );
    let mut entries: Vec<String> = Vec::new();
    entries.push(format!(
        "\t\t\t\t\t\"timestamp\": {{\"date\": \"{}\", \"time\": \"{}\", \"tz\": \"{}\", \"interval\": {}}}",
        stamp.date,
        stamp.time,
        esc(&stamp.tz),
        interval_secs(view.itv_cs)
    ));

    let mut open_group = Group::None;
    let mut children: Vec<String> = Vec::new();

    // `act[]` 順 = JSON の出力順。グループは `act[]` の中で連続しているので
    // 順に走査しながら開閉できる。
    for act in acts {
        let spec = act.spec;
        if spec.group != open_group {
            flush_group(&mut entries, open_group, &mut children);
            open_group = spec.group;
        }
        let Some(pair) = act.pair(view) else {
            continue;
        };
        let Some(block) = activity_block(&pair, cfg, spec, group_tab(spec.group)) else {
            continue;
        };
        if spec.group == Group::None {
            entries.push(block);
        } else {
            children.push(block);
        }
    }
    flush_group(&mut entries, open_group, &mut children);

    entries.join(",\n")
}

/// ラッパを持つ activity の中身は 1 段深い。
fn group_tab(group: Group) -> usize {
    if group == Group::None { 5 } else { 6 }
}

/// 溜めたグループの子をラッパで包んで書き出す。
///
/// 子が 1 つも無いときはラッパを出さない (`markup_state` が OPEN にならない)。
fn flush_group(entries: &mut Vec<String>, group: Group, children: &mut Vec<String>) {
    if group == Group::None || children.is_empty() {
        children.clear();
        return;
    }
    let t = tabs(5);
    let body = children.join(",\n");
    entries.push(format!("{t}\"{}\": {{\n{body}\n{t}}}", group.tag()));
    children.clear();
}

// ===========================================================================
// activity 1 種
// ===========================================================================

fn activity_block(
    pair: &ActivityPair<'_>,
    cfg: &SadfConfig,
    spec: &ActivitySpec,
    tab: usize,
) -> Option<String> {
    // `IS_SELECTED && nr[curr] > 0` の activity だけが出る
    if pair.is_empty() {
        return None;
    }
    let t = tabs(tab);

    match spec.shape {
        Shape::Object | Shape::TextChildren => {
            // TextChildren (A_MEMORY / A_HUGE) は JSON ではフラットなオブジェクト
            let item = pair.item(0)?;
            let mut members: Vec<String> = Vec::new();
            for section in spec.active_sections(&cfg.section) {
                let label = item_label_in(spec, section, &item);
                for field in jx_fields(section) {
                    if field.key.is_empty() || !cfg.section.allows_field(field.gate) {
                        continue;
                    }
                    members.push(member(spec, &item, field, &label));
                }
            }
            Some(format!(
                "{t}\"{}\": {{{}}}",
                spec.json_key,
                members.join(", ")
            ))
        }
        Shape::Array => {
            let inner = tabs(tab + 1);
            let mut rows: Vec<String> = Vec::new();
            for item in pair.selected_items_in(cfg, CompatFormat::JsonXml) {
                let mut members: Vec<String> = Vec::new();
                for section in spec.active_sections(&cfg.section) {
                    let label = item_label_in(spec, section, &item);
                    for field in jx_fields(section) {
                        if field.key.is_empty() || !cfg.section.allows_field(field.gate) {
                            continue;
                        }
                        members.push(member(spec, &item, field, &label));
                    }
                }
                rows.push(format!("{inner}{{{}}}", members.join(", ")));
            }
            // 絞り込みで 0 件でも配列そのものは出る (`"key": [\n\n\t…]`)。
            // 本家の `json_print_*()` は `nr[curr] > 0` なら必ず呼ばれて
            // 開き括弧と閉じ括弧を出す。
            Some(format!(
                "{t}\"{}\": [\n{}\n{t}]",
                spec.json_key,
                rows.join(",\n")
            ))
        }
        Shape::Custom => match spec.id {
            ActivityId::IO => io_block(pair, tab),
            ActivityId::IRQ => irq_block(pair, tab, spec, cfg),
            _ => None,
        },
    }
}

/// `"key": value` を 1 つ組み立てる。
fn member(
    spec: &ActivitySpec,
    item: &super::access::ItemPair<'_>,
    field: &super::spec::Field,
    label: &super::ItemLabel,
) -> String {
    let quoted = matches!(field.jx_fmt, Fmt::Str | Fmt::ItemKeyStr | Fmt::Hex);
    if quoted {
        // 文字列列は本家と同じく**空文字**を出す。`null` にはしない
        // (本家は空の `manufact` を `""` として出す、§11.2 (b) の実測)。
        // 「その世代に無い」と「空文字」の区別が要る用途は独自 JSON / NDJSON の
        // `quality` を使う。
        let v = render::field_value(spec, item, field, field.jx_fmt, label, "");
        return format!("\"{}\": \"{}\"", field.key, esc(&v));
    }
    let v = render::field_value(spec, item, field, field.jx_fmt, label, ABSENT_JSON);
    format!("\"{}\": {v}", field.key)
}

/// `A_IO` は `io-reads` / `io-writes` / `io-discard` に入れ子になる (§9.5)。
fn io_block(pair: &ActivityPair<'_>, tab: usize) -> Option<String> {
    let item = pair.item(0)?;
    let g = |name: &str| {
        let mut s = String::new();
        super::write_value(&mut s, item.computed_by_name(name), Fmt::R2, ABSENT_JSON);
        s
    };
    Some(format!(
        "{}\"io\": {{\"tps\": {}, \"io-reads\": {{\"rtps\": {}, \"bread\": {}}}, \"io-writes\": {{\"wtps\": {}, \"bwrtn\": {}}}, \"io-discard\": {{\"dtps\": {}, \"bdscd\": {}}}}}",
        tabs(tab),
        g("tps"),
        g("rtps"),
        g("bread"),
        g("wtps"),
        g("bwrtn"),
        g("dtps"),
        g("bdscd"),
    ))
}

/// `A_IRQ` は 1 要素が「1 割込 × 全 CPU」。
///
/// CPU キー集合はサンプルごとに変わり得る (§9.6-13)。固定スキーマにしてはいけない。
fn irq_block(
    pair: &ActivityPair<'_>,
    tab: usize,
    spec: &ActivitySpec,
    cfg: &SadfConfig,
) -> Option<String> {
    let (nr, nr2) = pair.irq_dimensions();
    let t = tabs(tab);
    let inner = tabs(tab + 1);
    let mut rows: Vec<String> = Vec::new();

    for irq in 0..nr2 {
        if !(0..nr).any(|cpu| pair.irq_cpu_selected(cfg, cpu, false)) {
            continue;
        }
        let name = pair.irq_name(irq);
        if !cfg.name_selected(ActivityId::IRQ, &name) {
            continue;
        }
        let mut members = vec![format!("\"intr\": \"{}\"", esc(&name))];
        for cpu in 0..nr {
            if !pair.irq_cpu_selected(cfg, cpu, false) {
                continue;
            }
            let key = if cpu == 0 {
                "all".to_string()
            } else {
                format!("CPU{}", cpu - 1)
            };
            let mut v = String::new();
            match pair.irq_item(cpu, irq) {
                Some(item) => {
                    super::write_value(&mut v, item.computed_by_name("intr"), Fmt::R2, ABSENT_JSON)
                }
                None => v.push_str(ABSENT_JSON),
            }
            members.push(format!("\"{key}\": {v}"));
        }
        rows.push(format!("{inner}{{{}}}", members.join(", ")));
    }
    // 絞り込み (`--int=`) で 0 件でも空の配列として出る
    Some(format!(
        "{t}\"{}\": [\n{}\n{t}]",
        spec.json_key,
        rows.join(",\n")
    ))
}

// ===========================================================================
// ヘルパ
// ===========================================================================

fn tabs(n: usize) -> String {
    "\t".repeat(n)
}

/// JSON 文字列のエスケープ。
///
/// sysstat はエスケープしないが、デバイス名やコメントに `"` や `\` が入ると
/// JSON が壊れる。壊れた JSON を出すより正しくエスケープする方を選ぶ。
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::sadf::spec;

    /// ラッパを持たない activity は tab 5、グループ内は tab 6 (§9.4)。
    #[test]
    fn indent_depth_matches_upstream() {
        assert_eq!(group_tab(Group::None), 5);
        assert_eq!(group_tab(Group::Network), 6);
        assert_eq!(group_tab(Group::PowerManagement), 6);
        assert_eq!(group_tab(Group::Psi), 6);
    }

    /// `filesystems` / `psi` 自身は tab 5 に置かれる。
    #[test]
    fn group_wrapper_sits_at_tab_five() {
        let mut entries = Vec::new();
        let mut children = vec!["\t\t\t\t\t\t\"psi-cpu\": {}".to_string()];
        flush_group(&mut entries, Group::Psi, &mut children);
        assert_eq!(entries.len(), 1);
        assert!(entries[0].starts_with("\t\t\t\t\t\"psi\": {\n"));
        assert!(entries[0].ends_with("\n\t\t\t\t\t}"));
    }

    /// 中身が無いグループはラッパごと出ない。
    #[test]
    fn empty_group_emits_nothing() {
        let mut entries = Vec::new();
        let mut children = Vec::new();
        flush_group(&mut entries, Group::Network, &mut children);
        assert!(entries.is_empty());
    }

    #[test]
    fn escaping_protects_the_document() {
        assert_eq!(esc("a\"b\\c"), "a\\\"b\\\\c");
        assert_eq!(esc("x\ty"), "x\\ty");
    }

    /// `%` を含むキーはそのまま出る (§9.6-12)。
    #[test]
    fn filesystem_keys_keep_percent_sign() {
        let fs = spec::lookup(ActivityId::FS).unwrap();
        let keys: Vec<_> = fs.sections[0].fields.iter().map(|f| f.key).collect();
        assert!(keys.contains(&"%fsused"));
        assert!(keys.contains(&"%ufsused"));
        assert!(keys.contains(&"%Iused"));
        assert!(keys.contains(&"filesystem"));
    }

    /// XML とキー名が違う箇所を取り違えていないこと (§10.6)。
    #[test]
    fn json_and_xml_names_differ_where_documented() {
        let fs = spec::lookup(ActivityId::FS).unwrap();
        let f = fs.sections[0]
            .fields
            .iter()
            .find(|f| f.key == "%fsused")
            .unwrap();
        assert_eq!(f.attr, "fsused-percent");

        let disk = spec::lookup(ActivityId::DISK).unwrap();
        let d = disk.sections[0]
            .fields
            .iter()
            .find(|f| f.key == "disk-device")
            .unwrap();
        assert_eq!(d.attr, "dev");

        let fc = spec::lookup(ActivityId::NET_FC).unwrap();
        let n = fc.sections[0]
            .fields
            .iter()
            .find(|f| f.key == "fchost")
            .unwrap();
        assert_eq!(n.attr, "name");

        let cpu = spec::lookup(ActivityId::CPU).unwrap();
        let c = cpu.sections[0]
            .fields
            .iter()
            .find(|f| f.key == "cpu")
            .unwrap();
        assert_eq!(c.attr, "number");
    }

    /// `fan-speed` の `rpm` / `drpm` は JSON では整数、`-d`/`-p` では 2 桁小数 (§9.6-1)。
    #[test]
    fn fan_rpm_is_integer_in_json_but_float_in_db() {
        let fan = spec::lookup(ActivityId::PWR_FAN).unwrap();
        let rpm = fan.sections[0]
            .fields
            .iter()
            .find(|f| f.key == "rpm")
            .unwrap();
        assert_eq!(rpm.jx_fmt, Fmt::Int);
        assert_eq!(rpm.dp_fmt, Fmt::R2);
    }

    /// `net-sock` / `net-sock6` だけ整数、他の `net-*` は 2 桁小数 (§9.6-7)。
    #[test]
    fn only_socket_counters_are_integers() {
        for id in [ActivityId::NET_SOCK, ActivityId::NET_SOCK6] {
            let s = spec::lookup(id).unwrap();
            assert!(s.sections[0].fields.iter().all(|f| f.jx_fmt == Fmt::Int));
        }
        let tcp = spec::lookup(ActivityId::NET_TCP).unwrap();
        assert!(tcp.sections[0].fields.iter().all(|f| f.jx_fmt == Fmt::R2));
    }

    /// `softnet.blg_len` だけが整数 (§9.6-8)。
    #[test]
    fn softnet_backlog_is_the_only_integer() {
        let s = spec::lookup(ActivityId::NET_SOFT).unwrap();
        let ints: Vec<_> = s.sections[0]
            .fields
            .iter()
            .filter(|f| f.jx_fmt == Fmt::Int)
            .map(|f| f.key)
            .collect();
        assert_eq!(ints, vec!["blg_len"]);
    }

    /// `voltage-input.number` は 0 始まり、`fan-speed` / `temperature` は 1 始まり。
    #[test]
    fn sensor_numbering_base_differs() {
        use super::super::spec::ItemKind;
        assert!(matches!(
            spec::lookup(ActivityId::PWR_IN).unwrap().item,
            ItemKind::Index { base: 0, .. }
        ));
        assert!(matches!(
            spec::lookup(ActivityId::PWR_FAN).unwrap().item,
            ItemKind::Index { base: 1, .. }
        ));
        assert!(matches!(
            spec::lookup(ActivityId::PWR_TEMP).unwrap().item,
            ItemKind::Index { base: 1, .. }
        ));
    }
}

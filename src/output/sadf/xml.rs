//! `sadf -x` (XML) の出力。
//!
//! 表示ループは JSON と同じ `logic1` (時刻順、`restarts` / `comments` は末尾)。
//!
//! # 書式の要点 (§10)
//!
//! - `<!DOCTYPE>` は出さない。`xsi:schemaLocation` が XSD を指す。
//! - `<sysdata-version>` は `sadf.h` の `XML_DTD_VERSION` = **3.18 固定**。
//!   読み込んだファイルの版には依存しない。
//! - インデントはタブ。深さは `<sysstat>`=0 … activity=4、その子=5、孫=6。
//! - 値はほとんど属性。例外は `A_IO` の `<tps>` と、
//!   `A_MEMORY` / `A_HUGE` の**全値** (テキスト内容)。
//! - 属性名は JSON のキー名と微妙に違う (§10.6)。
//! - `<network>` 内の配列 activity は自分のラッパを持たず、
//!   子要素が直接 `<network>` の中に並ぶ。

use std::io::{self, Write};

use super::access::{ActivityPair, ItemPair};
use super::dbppc::{display_cpu_count, present_specs, scan_comments, scan_restarts};
use super::render::{item_label_in, jx_fields};
use super::spec::{ActivitySpec, Field, Fmt, Group, Shape};
use super::{ABSENT_XML, FileInfo, ItemLabel, SadfConfig, Stamp, interval_secs, render, spec};
use crate::error::Result;
use crate::format::file::{SaFile, ScanControl};
use crate::model::ActivityId;
use crate::series::{IntervalView, Selection, walk};

/// `sadf.h` の `XML_DTD_VERSION`。
pub const XML_DTD_VERSION: &str = "3.18";

/// `-x` の出力。
pub fn write_xml<W: Write>(out: &mut W, file: &SaFile, cfg: &SadfConfig) -> Result<()> {
    let info = FileInfo::from_file(file);
    let specs = present_specs(file);

    write_prologue(out, &info).map_err(super::wrap_io)?;

    let ids: Vec<ActivityId> = specs.iter().map(|s| s.id).collect();
    walk(file, &Selection::Only(ids), |view| {
        if !view.has_prev || !view.continuous {
            return Ok(ScanControl::Continue);
        }
        write_timestamp(out, view, cfg, &info, &specs).map_err(super::wrap_io)?;
        Ok(ScanControl::Continue)
    })?;

    let restarts = scan_restarts(file)?;
    let comments = if cfg.comments {
        scan_comments(file)?
    } else {
        Vec::new()
    };
    write_epilogue(out, cfg, &info, &restarts, &comments).map_err(super::wrap_io)?;
    Ok(())
}

/// `<statistics>` までのヘッダ部。
fn write_prologue<W: Write>(out: &mut W, info: &FileInfo) -> io::Result<()> {
    writeln!(out, "<?xml version=\"1.0\" encoding=\"UTF-8\"?>")?;
    // <sysstat> の開始タグは属性ごとに改行され、インデントを持たない (§10.1)
    writeln!(out, "<sysstat")?;
    writeln!(out, "xmlns=\"https://sysstat.github.io\"")?;
    writeln!(
        out,
        "xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\""
    )?;
    writeln!(
        out,
        "xsi:schemaLocation=\"https://sysstat.github.io https://sysstat.github.io/sysstat.xsd\">"
    )?;
    writeln!(
        out,
        "\t<sysdata-version>{XML_DTD_VERSION}</sysdata-version>"
    )?;
    writeln!(out, "\t<host nodename=\"{}\">", esc(&info.nodename))?;
    writeln!(out, "\t\t<sysname>{}</sysname>", esc(&info.sysname))?;
    writeln!(out, "\t\t<release>{}</release>", esc(&info.release))?;
    writeln!(out, "\t\t<machine>{}</machine>", esc(&info.machine))?;
    writeln!(
        out,
        "\t\t<number-of-cpus>{}</number-of-cpus>",
        info.cpu_count
    )?;
    writeln!(out, "\t\t<file-date>{}</file-date>", info.file_date)?;
    writeln!(
        out,
        "\t\t<file-utc-time>{}</file-utc-time>",
        info.file_utc_time
    )?;
    // sa_tzname が空でも自己終了タグにはならない (§10.1)
    writeln!(out, "\t\t<timezone>{}</timezone>", esc(&info.tzname))?;
    writeln!(out, "\t\t<statistics>")?;
    Ok(())
}

/// 1 サンプル分の `<timestamp>` ブロック。
fn write_timestamp<W: Write>(
    out: &mut W,
    view: &IntervalView<'_>,
    cfg: &SadfConfig,
    info: &FileInfo,
    specs: &[&'static ActivitySpec],
) -> io::Result<()> {
    let stamp = Stamp::new(
        cfg.time_base,
        view.curr.ust_time,
        (view.curr.hour, view.curr.minute, view.curr.second),
        info,
    );
    writeln!(
        out,
        "\t\t\t<timestamp date=\"{}\" time=\"{}\" tz=\"{}\" interval=\"{}\">",
        stamp.date,
        stamp.time,
        esc(&stamp.tz),
        interval_secs(view.itv_cs)
    )?;
    write_sample(out, view, cfg, specs)?;
    writeln!(out, "\t\t\t</timestamp>")?;
    Ok(())
}

/// `</statistics>` 以降。`<restarts>` / `<comments>` は統計の後にまとめて出る。
fn write_epilogue<W: Write>(
    out: &mut W,
    cfg: &SadfConfig,
    info: &FileInfo,
    restarts: &[super::dbppc::RestartMark],
    comments: &[(u64, (u8, u8, u8), String)],
) -> io::Result<()> {
    writeln!(out, "\t\t</statistics>")?;

    writeln!(out, "\t\t<restarts>")?;
    for r in restarts {
        let s = Stamp::new(cfg.time_base, r.ust_time, r.hms, info);
        writeln!(
            out,
            "\t\t\t<boot date=\"{}\" time=\"{}\" tz=\"{}\" cpu_count=\"{}\"/>",
            s.date,
            s.time,
            esc(&s.tz),
            display_cpu_count(r.cpu_count)
        )?;
    }
    writeln!(out, "\t\t</restarts>")?;

    if cfg.comments {
        writeln!(out, "\t\t<comments>")?;
        for (ust, hms, text) in comments {
            let s = Stamp::new(cfg.time_base, *ust, *hms, info);
            writeln!(
                out,
                "\t\t\t<comment date=\"{}\" time=\"{}\" tz=\"{}\" com=\"{}\"/>",
                s.date,
                s.time,
                esc(&s.tz),
                esc(text)
            )?;
        }
        writeln!(out, "\t\t</comments>")?;
    }

    writeln!(out, "\t</host>")?;
    writeln!(out, "</sysstat>")?;
    Ok(())
}

// ===========================================================================
// 1 サンプル
// ===========================================================================

fn write_sample<W: Write>(
    out: &mut W,
    view: &IntervalView<'_>,
    cfg: &SadfConfig,
    specs: &[&'static ActivitySpec],
) -> io::Result<()> {
    let mut open_group = Group::None;

    for spec in spec::SPECS {
        if !specs.iter().any(|s| s.id == spec.id) {
            continue;
        }
        let Some(pair) = ActivityPair::from_view(view, spec.id) else {
            continue;
        };
        if pair.is_empty() {
            continue;
        }
        let body = activity_body(
            &pair,
            cfg,
            spec,
            if spec.group == Group::None { 4 } else { 5 },
        );
        if body.is_empty() {
            continue;
        }
        if spec.group != open_group {
            close_group(out, open_group)?;
            open_group = spec.group;
            if open_group != Group::None {
                writeln!(
                    out,
                    "\t\t\t\t<{}{}>",
                    open_group.tag(),
                    open_group.xml_attrs()
                )?;
            }
        }
        out.write_all(body.as_bytes())?;
    }
    close_group(out, open_group)?;
    Ok(())
}

/// `AO_CLOSE_MARKUP` に相当する閉じタグ。
fn close_group<W: Write>(out: &mut W, group: Group) -> io::Result<()> {
    if group != Group::None {
        writeln!(out, "\t\t\t\t</{}>", group.tag())?;
    }
    Ok(())
}

// ===========================================================================
// activity 1 種
// ===========================================================================

fn activity_body(
    pair: &ActivityPair<'_>,
    cfg: &SadfConfig,
    spec: &ActivitySpec,
    depth: usize,
) -> String {
    match spec.shape {
        Shape::Object => {
            let Some(item) = pair.item(0) else {
                return String::new();
            };
            let attrs = attr_list(spec, &item, cfg);
            format!(
                "{}<{}{}{}/>\n",
                tabs(depth),
                spec.xml_elem,
                spec.xml_wrapper_attrs,
                attrs
            )
        }
        // A_MEMORY / A_HUGE は全値がテキスト内容の子要素 (§10.3)
        Shape::TextChildren => {
            let Some(item) = pair.item(0) else {
                return String::new();
            };
            let mut s = format!(
                "{}<{}{}>\n",
                tabs(depth),
                spec.xml_elem,
                spec.xml_wrapper_attrs
            );
            for section in spec.active_sections(&cfg.section) {
                let label = item_label_in(spec, section, &item);
                for field in jx_fields(section) {
                    if field.attr.is_empty() || !cfg.section.allows_field(field.gate) {
                        continue;
                    }
                    let v =
                        render::field_value(spec, &item, field, field.jx_fmt, &label, ABSENT_XML);
                    s.push_str(&format!(
                        "{}<{}>{}</{}>\n",
                        tabs(depth + 1),
                        field.attr,
                        esc(&v),
                        field.attr
                    ));
                }
            }
            s.push_str(&format!("{}</{}>\n", tabs(depth), spec.xml_elem));
            s
        }
        Shape::Array => {
            // <network> 内は自分のラッパを持たず、子要素が直接並ぶ (§10.3)
            let wrap = spec.group != Group::Network;
            let child_depth = if wrap { depth + 1 } else { depth };
            let mut rows = String::new();
            for item in pair.output_items() {
                let attrs = attr_list(spec, &item, cfg);
                rows.push_str(&format!(
                    "{}<{}{}/>\n",
                    tabs(child_depth),
                    spec.xml_child,
                    attrs
                ));
            }
            if rows.is_empty() {
                return String::new();
            }
            if wrap {
                format!(
                    "{}<{}{}>\n{rows}{}</{}>\n",
                    tabs(depth),
                    spec.xml_elem,
                    spec.xml_wrapper_attrs,
                    tabs(depth),
                    spec.xml_elem
                )
            } else {
                rows
            }
        }
        Shape::Custom => match spec.id {
            ActivityId::IO => io_body(pair, depth),
            ActivityId::IRQ => irq_body(pair, depth),
            _ => String::new(),
        },
    }
}

/// 属性列 (` name="value"` の連結)。
fn attr_list(spec: &ActivitySpec, item: &ItemPair<'_>, cfg: &SadfConfig) -> String {
    let mut s = String::new();
    for section in spec.active_sections(&cfg.section) {
        let label = item_label_in(spec, section, item);
        for field in jx_fields(section) {
            if field.attr.is_empty() || !cfg.section.allows_field(field.gate) {
                continue;
            }
            s.push_str(&attr(spec, item, field, &label));
        }
    }
    s
}

fn attr(spec: &ActivitySpec, item: &ItemPair<'_>, field: &Field, label: &ItemLabel) -> String {
    let v = render::field_value(spec, item, field, field.jx_fmt, label, ABSENT_XML);
    format!(" {}=\"{}\"", field.attr, esc(&v))
}

/// `A_IO` は `<tps>` だけテキスト内容、残り 3 つは属性 (§10.3)。
fn io_body(pair: &ActivityPair<'_>, depth: usize) -> String {
    let Some(item) = pair.item(0) else {
        return String::new();
    };
    let g = |name: &str| {
        let mut s = String::new();
        super::write_value(&mut s, item.computed_by_name(name), Fmt::R2, ABSENT_XML);
        s
    };
    let t = tabs(depth);
    let c = tabs(depth + 1);
    format!(
        "{t}<io per=\"second\">\n{c}<tps>{}</tps>\n{c}<io-reads rtps=\"{}\" bread=\"{}\"/>\n{c}<io-writes wtps=\"{}\" bwrtn=\"{}\"/>\n{c}<io-discard dtps=\"{}\" bdscd=\"{}\"/>\n{t}</io>\n",
        g("tps"),
        g("rtps"),
        g("bread"),
        g("wtps"),
        g("bwrtn"),
        g("dtps"),
        g("bdscd"),
    )
}

/// `A_IRQ` は `<interrupts>` → `<int-global per="second">` → `<irq/>`。
///
/// **1 要素が 1 割込 × 1 CPU** で、JSON とは構造が本質的に違う (§10.6)。
fn irq_body(pair: &ActivityPair<'_>, depth: usize) -> String {
    let nr = pair.curr.nr.max(1) as usize;
    let nr2 = pair.curr.nr2.max(1) as usize;
    let t = tabs(depth);
    let mid = tabs(depth + 1);
    let leaf = tabs(depth + 2);

    let mut rows = String::new();
    for irq in 0..nr2 {
        let name = pair
            .matrix_item(0, irq)
            .and_then(|i| i.key().map(|s| s.to_string()))
            .unwrap_or_else(|| irq.to_string());
        for cpu in 0..nr {
            let cpu_label = if cpu == 0 {
                "all".to_string()
            } else {
                (cpu - 1).to_string()
            };
            let mut v = String::new();
            match pair.matrix_item(cpu, irq) {
                Some(item) => {
                    super::write_value(&mut v, item.computed_by_name("intr"), Fmt::R2, ABSENT_XML)
                }
                None => v.push_str(ABSENT_XML),
            }
            rows.push_str(&format!(
                "{leaf}<irq intr=\"{}\" cpu=\"{cpu_label}\" value=\"{v}\"/>\n",
                esc(&name)
            ));
        }
    }
    if rows.is_empty() {
        return String::new();
    }
    format!(
        "{t}<interrupts>\n{mid}<int-global per=\"second\">\n{rows}{mid}</int-global>\n{t}</interrupts>\n"
    )
}

// ===========================================================================
// ヘルパ
// ===========================================================================

fn tabs(n: usize) -> String {
    "\t".repeat(n)
}

/// XML の最小エスケープ。
///
/// sysstat は属性値をエスケープしないが、デバイス名やコメントに `<` や `&` が
/// 入ると文書が壊れる。壊れた XML を出すより正しくエスケープする方を選ぶ。
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `<sysdata-version>` はファイルの版に依存しない固定値。
    #[test]
    fn dtd_version_is_fixed() {
        assert_eq!(XML_DTD_VERSION, "3.18");
    }

    #[test]
    fn escaping_protects_the_document() {
        assert_eq!(esc("a<b&c\""), "a&lt;b&amp;c&quot;");
    }

    /// `<memory>` / `<hugepages>` は `unit="kB"` を持つ (§10.5)。
    #[test]
    fn memory_wrapper_declares_unit() {
        assert_eq!(
            spec::lookup(ActivityId::MEMORY).unwrap().xml_wrapper_attrs,
            " unit=\"kB\""
        );
        assert_eq!(
            spec::lookup(ActivityId::HUGE).unwrap().xml_wrapper_attrs,
            " unit=\"kB\""
        );
    }

    /// センサ系ラッパの `unit` 値 (XSD の enumeration と一致させる)。
    #[test]
    fn sensor_wrappers_declare_documented_units() {
        let u = |id| spec::lookup(id).unwrap().xml_wrapper_attrs;
        assert_eq!(u(ActivityId::PWR_FAN), " unit=\"rpm\"");
        assert_eq!(u(ActivityId::PWR_TEMP), " unit=\"degree Celsius\"");
        assert_eq!(u(ActivityId::PWR_IN), " unit=\"V\"");
        assert_eq!(u(ActivityId::PWR_CPU), " unit=\"MHz\"");
        assert_eq!(u(ActivityId::PWR_FREQ), " unit=\"MHz\"");
        assert_eq!(u(ActivityId::PWR_BAT), " unit=\"minute\"");
    }

    /// グループのラッパ属性 — `network` / `psi` は `per="second"`、
    /// `power-management` は属性なし。
    #[test]
    fn group_wrapper_attributes() {
        assert_eq!(Group::Network.xml_attrs(), " per=\"second\"");
        assert_eq!(Group::Psi.xml_attrs(), " per=\"second\"");
        assert_eq!(Group::PowerManagement.xml_attrs(), "");
    }

    /// `<network>` 内の配列 activity は自分のラッパを持たない。
    #[test]
    fn network_arrays_have_no_own_wrapper() {
        for id in [
            ActivityId::NET_DEV,
            ActivityId::NET_EDEV,
            ActivityId::NET_FC,
            ActivityId::NET_SOFT,
        ] {
            assert_eq!(spec::lookup(id).unwrap().group, Group::Network);
        }
    }

    /// `A_FS` の第 1 属性名は `-F MOUNT` で `fsname` → `mountp` に変わる。
    #[test]
    fn filesystem_first_attribute_is_dynamic() {
        let fs = spec::lookup(ActivityId::FS).unwrap();
        assert_eq!(fs.sections[0].fields[0].attr, "fsname");
        assert_eq!(fs.sections[1].fields[0].attr, "mountp");
    }
}

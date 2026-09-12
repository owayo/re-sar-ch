//! 独自 CSV 出力 (`csv` クレート)。
//!
//! # 縦持ち (long / tidy) にする理由
//!
//! activity ごとに列の集合が違うため、1 つの表に横持ちすると列が
//! 43 activity 分の和集合になり、ほとんどが空になる。
//! ここでは **1 行 = 1 メトリック**の縦持ちにして、
//! `activity` / `metric` 列で絞り込める形にする。
//! 表計算ソフトのピボットにも、`awk` にもそのまま流せる。
//!
//! 値は公開スキーマ ([`super::json`]) から取る。列は固定なので、
//! 内部の列定義が増えても CSV のヘッダは変わらない。
//!
//! # 生値の桁
//!
//! `raw` 列は**十進文字列**のまま書く。CSV は型を持たないので、
//! 表計算ソフトが 2^53 超の整数を丸める危険は残る。
//! 桁落ちを避けたい用途では NDJSON を使うこと。

use std::io::Write;

use super::json::{CustomConfig, FieldOut, HostOut, Quality, item_out, selected_ids};
use super::sadf::access::ActivityPair;
use super::sadf::spec;
use crate::error::Result;
use crate::format::file::{SaFile, ScanControl};
use crate::series::{RecordEvent, WalkItem, walk_items};

/// CSV のヘッダ (この順で固定)。
pub const HEADER: &[&str] = &[
    "hostname",
    "source",
    "boot",
    "start_epoch",
    "end_epoch",
    "elapsed_cs",
    "continuous",
    "activity",
    "item",
    "item_index",
    "space",
    "metric",
    "unit",
    "kind",
    "value",
    "raw",
    "text",
    "quality",
];

/// 独自 CSV の出力。
///
/// `csv::Writer` が内部バッファを持つので、レコードを書いた時点で
/// 順次書き出される。全レコードを溜めることはしない。
pub fn write_csv<W: Write>(out: W, file: &SaFile, cfg: &CustomConfig) -> Result<()> {
    let host = HostOut::new(file);
    let mut w = csv::WriterBuilder::new().from_writer(out);
    w.write_record(HEADER).map_err(csv_err)?;

    let mut boot: u32 = 0;
    walk_items(file, &cfg.selection.clone(), |item| {
        // 縦持ちの統計行しか持たない形式なので、イベントは起動区間の番号にだけ効かせる。
        let view = match item {
            WalkItem::Event(ev) => {
                if matches!(ev, RecordEvent::Restart { .. }) {
                    boot += 1;
                }
                return Ok(ScanControl::Continue);
            }
            WalkItem::Sample(view) => view,
        };
        let start_epoch = if view.has_prev {
            view.prev.ust_time
        } else {
            view.curr.ust_time
        };
        let continuous = view.has_prev && view.continuous;

        for id in selected_ids(view, cfg) {
            let Some(pair) = ActivityPair::from_view(view, id) else {
                continue;
            };
            let Some(sp) = spec::lookup(id) else { continue };
            for item in pair.output_items() {
                let row = item_out(pair.def, sp, &item, cfg);
                for (space, fields) in [("raw", &row.raw), ("rates", &row.rates)] {
                    for f in fields.iter() {
                        write_field(
                            &mut w,
                            &host,
                            boot,
                            start_epoch,
                            view.curr.ust_time,
                            view.itv_cs,
                            continuous,
                            sp.name,
                            &row.item,
                            row.index,
                            space,
                            f,
                        )?;
                    }
                }
            }
        }
        Ok(ScanControl::Continue)
    })?;

    w.flush().map_err(super::sadf::wrap_io)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn write_field<W: Write>(
    w: &mut csv::Writer<W>,
    host: &HostOut,
    boot: u32,
    start_epoch: u64,
    end_epoch: u64,
    elapsed_cs: u64,
    continuous: bool,
    activity: &str,
    item: &str,
    item_index: usize,
    space: &str,
    f: &FieldOut,
) -> Result<()> {
    // 値が無い列は空欄にする。**0 を書いてはいけない** (正常な 0 と区別できない)。
    let value = f.value.map(|v| format!("{v:.4}")).unwrap_or_default();
    let raw = f.raw.clone().unwrap_or_default();
    let text = f.text.clone().unwrap_or_default();

    w.write_record([
        host.hostname.as_str(),
        host.source.as_str(),
        &boot.to_string(),
        &start_epoch.to_string(),
        &end_epoch.to_string(),
        &elapsed_cs.to_string(),
        if continuous { "true" } else { "false" },
        activity,
        item,
        &item_index.to_string(),
        space,
        f.name,
        f.unit,
        f.kind,
        &value,
        &raw,
        &text,
        quality_label(f.quality),
    ])
    .map_err(csv_err)?;
    Ok(())
}

fn quality_label(q: Quality) -> &'static str {
    q.label()
}

fn csv_err(e: csv::Error) -> crate::error::Error {
    crate::error::Error::Other(format!("CSV の書き出しに失敗しました: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ヘッダは固定。内部の列定義が増えても変わらない。
    #[test]
    fn header_is_stable() {
        assert_eq!(HEADER[0], "hostname");
        assert_eq!(HEADER[HEADER.len() - 1], "quality");
        assert!(HEADER.contains(&"space"), "生値と派生値を列で区別する");
        assert!(HEADER.contains(&"raw"));
        assert!(HEADER.contains(&"value"));
        assert!(HEADER.contains(&"text"), "文字列フィールドも出す");
    }

    /// 値が無い列は空欄。0 にはしない。
    #[test]
    fn absent_value_is_an_empty_cell() {
        let host = HostOut {
            hostname: "testhost".into(),
            sysname: "Linux".into(),
            release: "5.0.0".into(),
            machine: "x86_64".into(),
            cpu_count: 8,
            file_date: "2019-04-18".into(),
            timezone: "UTC".into(),
            source: "sa18".into(),
        };
        let f = FieldOut {
            name: "await",
            unit: "ms",
            kind: "gauge",
            raw: None,
            value: None,
            text: None,
            quality: Quality::NotImplemented,
        };
        let mut buf = Vec::new();
        {
            let mut w = csv::Writer::from_writer(&mut buf);
            write_field(
                &mut w, &host, 0, 100, 110, 1000, true, "A_DISK", "sda", 0, "rates", &f,
            )
            .unwrap();
            w.flush().unwrap();
        }
        let s = String::from_utf8(buf).unwrap();
        // value / raw / text が空欄で、その後に理由が入る (0 で埋めない)
        assert!(
            s.contains("gauge,,,,not_implemented"),
            "空欄 3 つの後に理由: {s}"
        );
        assert!(!s.contains("gauge,0,"), "0 で埋めてはいけない: {s}");
    }

    /// 生値は十進文字列のまま書かれる。
    #[test]
    fn raw_counter_keeps_all_digits() {
        let host = HostOut {
            hostname: "testhost".into(),
            sysname: "Linux".into(),
            release: "5.0.0".into(),
            machine: "x86_64".into(),
            cpu_count: 8,
            file_date: "2019-04-18".into(),
            timezone: "UTC".into(),
            source: "sa18".into(),
        };
        let f = FieldOut {
            name: "rx_bytes",
            unit: "B",
            kind: "counter",
            raw: Some("18446744073709551615".into()),
            value: None,
            text: None,
            quality: Quality::Ok,
        };
        let mut buf = Vec::new();
        {
            let mut w = csv::Writer::from_writer(&mut buf);
            write_field(
                &mut w,
                &host,
                1,
                100,
                110,
                1000,
                true,
                "A_NET_DEV",
                "eth0",
                1,
                "raw",
                &f,
            )
            .unwrap();
            w.flush().unwrap();
        }
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("18446744073709551615"), "{s}");
    }
}

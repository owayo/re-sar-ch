//! AI エージェント向け NDJSON 出力。
//!
//! `docs/design.md` の「11. AI エージェント向け出力の契約」を満たす形で固定する。
//! 長期利用される前提なので、契約は [`super::json::SCHEMA_VERSION`] で版管理する。
//!
//! | 契約 | 実装 |
//! |---|---|
//! | `schema_version` を必ず含める | 全行の先頭キー |
//! | ホスト | `host` (hostname / sysname / release / machine / cpu_count) |
//! | 起動区間 | `boot` (RESTART をまたぐごとに増える番号) |
//! | 時刻 | `start_epoch` / `end_epoch` / `elapsed_cs` |
//! | activity / item | `activity` / `item` |
//! | 単位 | 各値に `unit` |
//! | 品質 | `quality` に `Availability` と `Discontinuity` を**区別して**入れる |
//! | 出典 | `source` (読み込んだファイル) |
//! | `u64` の生値 | **十進文字列** (JS の `Number` は 2^53 超で精度が落ちる) |
//! | 生値と派生値 | `raw` / `rates` の別名前空間 (`--values raw|rates|both`) |
//!
//! 1 行 = (サンプル × activity × item)。行単位で独立して読めるため、
//! 途中で打ち切っても残りの行が壊れない。

use std::io::Write;

use serde::Serialize;

use super::json::{
    BootCounter, CustomConfig, FieldOut, HostOut, ItemOut, SCHEMA_VERSION, item_out, selected_ids,
};
use super::sadf::access::ActivityPair;
use super::sadf::spec;
use crate::error::Result;
use crate::format::file::{SaFile, ScanControl};
use crate::output::time_filter::Admit;
use crate::series::{RecordEvent, WalkItem, walk_items};

/// NDJSON の 1 行。
///
/// 内部構造体ではなく**この型が公開契約**である。
#[derive(Debug, Serialize)]
pub struct Row<'a> {
    /// 公開スキーマの版。
    pub schema_version: &'static str,
    /// 行の種類。`sample` / `restart` / `comment`。
    pub record: &'static str,
    pub host: &'a HostOut,
    /// 起動区間の番号。RESTART をまたぐと増える。
    pub boot: u32,
    /// 区間の始点 (epoch 秒)。
    pub start_epoch: u64,
    /// 区間の終点 (epoch 秒)。
    pub end_epoch: u64,
    /// 経過時間 (1/100 秒)。**秒に丸めない**ので、呼び出し側で自由に扱える。
    pub elapsed_cs: u64,
    /// 前サンプルとの間に不連続が無いか。
    pub continuous: bool,
    /// 本家のシンボル名 (`A_CPU`)。
    pub activity: &'static str,
    /// 人間向けの名称。
    pub activity_label: &'static str,
    /// item の識別子 (`all` / `cpu0` / `sda` / `-`)。
    pub item: &'a str,
    /// activity 内での添字。
    pub item_index: usize,
    /// 累積カウンタの生値。値は十進文字列。
    #[serde(skip_serializing_if = "<[FieldOut]>::is_empty")]
    pub raw: &'a [FieldOut],
    /// 派生値 (レート・割合)。
    #[serde(skip_serializing_if = "<[FieldOut]>::is_empty")]
    pub rates: &'a [FieldOut],
}

/// RESTART / COMMENT の 1 行。
///
/// サンプル行と混ぜても読めるよう、`record` で区別する。
#[derive(Debug, Serialize)]
pub struct EventRow<'a> {
    pub schema_version: &'static str,
    pub record: &'static str,
    pub host: &'a HostOut,
    pub boot: u32,
    pub epoch: u64,
    /// RESTART のときの CPU 数。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_count: Option<u32>,
    /// COMMENT の本文。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<&'a str>,
}

/// NDJSON の出力。
///
/// 1 行ずつ書き出す。全レコードを溜めない。
pub fn write_ndjson<W: Write>(out: &mut W, file: &SaFile, cfg: &CustomConfig) -> Result<()> {
    let host = HostOut::new(file);
    let mut boot = BootCounter::default();
    let mut cursor = cfg.time_filter.cursor();

    walk_items(file, &cfg.selection.clone(), |item| {
        // 起動区間の境界と注記は**読んだ時点で**出す。行の並びがファイル上の
        // 順序と一致するので「どのサンプルが再起動の直後か」が読める。
        // 最後の統計レコードより後ろにある COMMENT もここで出る。
        let view = match item {
            WalkItem::Event(ev) => {
                // RESTART なら起動区間を進める (COMMENT では増えない)
                boot.advance(std::slice::from_ref(&ev));
                let row = match &ev {
                    RecordEvent::Restart {
                        ust_time,
                        cpu_count,
                        ..
                    } => EventRow {
                        schema_version: SCHEMA_VERSION,
                        record: "restart",
                        host: &host,
                        boot: boot.get(),
                        epoch: *ust_time,
                        cpu_count: *cpu_count,
                        comment: None,
                    },
                    RecordEvent::Comment { ust_time, text, .. } => EventRow {
                        schema_version: SCHEMA_VERSION,
                        record: "comment",
                        host: &host,
                        boot: boot.get(),
                        epoch: *ust_time,
                        cpu_count: None,
                        comment: Some(text.as_str()),
                    },
                };
                // 範囲外のイベント行は出さない
                if cursor.event(ev.ust_time(), ev.time()) {
                    write_line(out, &row)?;
                }
                return Ok(ScanControl::Continue);
            }
            WalkItem::Sample(view) => view,
        };
        match cursor.sample(view) {
            Admit::Skip | Admit::Reference => return Ok(ScanControl::Continue),
            Admit::Stop => return Ok(ScanControl::Stop),
            Admit::Emit => {}
        }

        let start_epoch = if view.has_prev {
            view.prev.ust_time
        } else {
            view.curr.ust_time
        };

        for id in selected_ids(view, cfg) {
            let Some(pair) = ActivityPair::from_view(view, id) else {
                continue;
            };
            let Some(sp) = spec::lookup(id) else { continue };
            for item in pair.output_items() {
                let out_item: ItemOut = item_out(pair.def, sp, &item, cfg);
                let row = Row {
                    schema_version: SCHEMA_VERSION,
                    record: "sample",
                    host: &host,
                    boot: boot.get(),
                    start_epoch,
                    end_epoch: view.curr.ust_time,
                    elapsed_cs: view.itv_cs,
                    continuous: view.has_prev && view.continuous,
                    activity: sp.name,
                    activity_label: id.label().unwrap_or(""),
                    item: &out_item.item,
                    item_index: out_item.index,
                    raw: &out_item.raw,
                    rates: &out_item.rates,
                };
                write_line(out, &row)?;
            }
        }
        Ok(ScanControl::Continue)
    })?;
    Ok(())
}

fn write_line<W: Write, T: Serialize>(out: &mut W, row: &T) -> Result<()> {
    serde_json::to_writer(&mut *out, row)
        .map_err(|e| crate::error::Error::Other(format!("NDJSON の直列化に失敗しました: {e}")))?;
    out.write_all(b"\n").map_err(super::sadf::wrap_io)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::json::{Quality, ValueScope};
    use super::*;

    fn host() -> HostOut {
        HostOut {
            hostname: "testhost".into(),
            sysname: "Linux".into(),
            release: "5.0.0".into(),
            machine: "x86_64".into(),
            cpu_count: 8,
            file_date: "2019-04-18".into(),
            timezone: "UTC".into(),
            source: "sa18".into(),
        }
    }

    /// 契約の必須項目がすべて 1 行に入っていること。
    #[test]
    fn every_line_satisfies_the_contract() {
        let h = host();
        let raw = vec![FieldOut {
            name: "user",
            unit: "percent",
            kind: "counter",
            raw: Some("9007199254740993".to_string()),
            value: None,
            text: None,
            quality: Quality::Ok,
        }];
        let rates = vec![FieldOut {
            name: "user",
            unit: "percent",
            kind: "counter",
            raw: None,
            value: Some(2.15),
            text: None,
            quality: Quality::Ok,
        }];
        let row = Row {
            schema_version: SCHEMA_VERSION,
            record: "sample",
            host: &h,
            boot: 1,
            start_epoch: 1_555_593_609,
            end_epoch: 1_555_593_619,
            elapsed_cs: 3117,
            continuous: true,
            activity: "A_CPU",
            activity_label: "CPU 使用率",
            item: "all",
            item_index: 0,
            raw: &raw,
            rates: &rates,
        };
        let s = serde_json::to_string(&row).unwrap();

        for key in [
            "\"schema_version\"",
            "\"host\"",
            "\"boot\"",
            "\"start_epoch\"",
            "\"end_epoch\"",
            "\"elapsed_cs\"",
            "\"activity\"",
            "\"item\"",
            "\"unit\"",
            "\"quality\"",
            "\"source\"",
            "\"raw\"",
            "\"rates\"",
        ] {
            assert!(s.contains(key), "{key} が欠けている: {s}");
        }
        // 2^53 を超える生値が十進文字列で出ること
        assert!(s.contains("\"9007199254740993\""), "{s}");
        // 数値として裸で出ていないこと
        assert!(!s.contains(":9007199254740993"), "{s}");
    }

    /// RESTART / COMMENT は `record` で区別できる。
    #[test]
    fn events_are_tagged_rows() {
        let h = host();
        let r = EventRow {
            schema_version: SCHEMA_VERSION,
            record: "restart",
            host: &h,
            boot: 1,
            epoch: 1_555_594_649,
            cpu_count: Some(9),
            comment: None,
        };
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("\"record\":\"restart\""), "{s}");
        assert!(s.contains("\"cpu_count\":9"), "{s}");
        assert!(!s.contains("\"comment\""), "{s}");
    }

    /// `--values raw` では派生値の名前空間が出ない。
    #[test]
    fn namespaces_follow_value_scope() {
        let h = host();
        let raw = vec![FieldOut {
            name: "user",
            unit: "percent",
            kind: "counter",
            raw: Some("1".into()),
            value: None,
            text: None,
            quality: Quality::Ok,
        }];
        let row = Row {
            schema_version: SCHEMA_VERSION,
            record: "sample",
            host: &h,
            boot: 0,
            start_epoch: 0,
            end_epoch: 0,
            elapsed_cs: 0,
            continuous: false,
            activity: "A_CPU",
            activity_label: "",
            item: "all",
            item_index: 0,
            raw: &raw,
            rates: &[],
        };
        let s = serde_json::to_string(&row).unwrap();
        assert!(s.contains("\"raw\""));
        assert!(!s.contains("\"rates\""), "{s}");
        assert!(ValueScope::Raw.wants_raw());
    }

    /// 品質は「0」に潰れない。
    #[test]
    fn unavailable_value_is_not_zero() {
        let f = FieldOut {
            name: "await",
            unit: "ms",
            kind: "gauge",
            raw: None,
            value: None,
            text: None,
            quality: Quality::NotImplemented,
        };
        let s = serde_json::to_string(&f).unwrap();
        assert!(!s.contains("\"value\":0"), "{s}");
        assert!(s.contains("not_implemented"), "{s}");
    }
}

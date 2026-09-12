//! 端末で読みやすい整列テーブル。
//!
//! 罫線ライブラリは使わず自前で整形する。`sar` 互換の固定幅とは別物で、
//! **幅は内容に合わせる**。
//!
//! ```text
//! A_CPU — CPU 使用率
//!   time      item    user    nice  system  iowait   steal    idle
//!   13:20:19  all     2.15   12.50    1.84    0.12    0.00   82.88
//!   13:20:19  cpu0    0.00   99.55    0.00    0.00    0.00    0.45
//! ```
//!
//! # 幅の決め方と走査回数
//!
//! 幅を内容に合わせるには、行を書く前に全行の幅を知る必要がある。
//! しかし**レコードを溜めるのは禁止**なので、代わりに
//! **ファイルを 2 回走査する**(1 回目は幅の計測だけ、2 回目で書き出し)。
//! 保持するのは activity ごとの列幅 (数十バイト) のみ。
//!
//! # 値が無い列
//!
//! `-` を出す。**0 では埋めない** (正常な 0 と区別できなくなる)。

use std::collections::BTreeMap;
use std::io::Write;

use super::json::{CustomConfig, FieldOut, item_out, selected_ids};
use super::sadf::access::ActivityPair;
use super::sadf::spec;
use crate::error::Result;
use crate::format::file::{SaFile, ScanControl};
use crate::model::ActivityId;
use crate::series::{Selection, WalkItem, walk_items};

/// 値が無いことを示す表記。
pub const ABSENT: &str = "-";

/// 列と列の間に入れる空白数。
const GAP: usize = 2;

/// 左端のインデント。
const INDENT: &str = "  ";

/// activity 1 種分の列幅。
#[derive(Debug, Clone, Default)]
struct Widths {
    time: usize,
    item: usize,
    /// 表示列ごとの幅 (ヘッダ名の幅も含めた最大)。
    cols: Vec<usize>,
    /// 表示列の名前 (公開名)。
    names: Vec<&'static str>,
    /// 表示列の単位 (見出しの 2 行目)。
    units: Vec<&'static str>,
}

/// 整列テーブルの出力。
pub fn write_table<W: Write>(out: &mut W, file: &SaFile, cfg: &CustomConfig) -> Result<()> {
    // --- 1 回目: 幅の計測 ---
    let widths = measure(file, cfg)?;

    // --- 2 回目: 書き出し ---
    let host = super::json::HostOut::new(file);
    writeln!(
        out,
        "host: {}  ({} {} / {}, {} CPU)",
        host.hostname, host.sysname, host.release, host.machine, host.cpu_count
    )
    .map_err(super::sadf::wrap_io)?;
    writeln!(out, "source: {}", host.source).map_err(super::sadf::wrap_io)?;

    let mut printed_header: BTreeMap<u32, bool> = BTreeMap::new();
    walk_items(file, &cfg.selection.clone(), |item| {
        // 表には統計行しか並べないので、イベントは読み飛ばす。
        let WalkItem::Sample(view) = item else {
            return Ok(ScanControl::Continue);
        };
        // 派生値を出すときは先頭レコードを飛ばす。基準となる前サンプルが無く、
        // 1 行すべてが `-` になって読みにくいだけなので。
        // 生値だけを見るときは先頭レコードにも意味がある。
        if !view.has_prev && cfg.values.wants_rates() {
            return Ok(ScanControl::Continue);
        }
        let ts = utc_hms(view.curr.ust_time);
        for id in selected_ids(view, cfg) {
            let Some(w) = widths.get(&id.0) else { continue };
            let Some(pair) = ActivityPair::from_view(view, id) else {
                continue;
            };
            let Some(sp) = spec::lookup(id) else { continue };

            if !printed_header.get(&id.0).copied().unwrap_or(false) {
                printed_header.insert(id.0, true);
                write_block_header(out, id, w)?;
            }

            for item in pair.output_items() {
                let row = item_out(pair.def, sp, &item, cfg);
                let fields = display_fields(&row.raw, &row.rates);
                let mut line = String::from(INDENT);
                push_cell(&mut line, &ts, w.time, false);
                push_cell(&mut line, &row.item, w.item, false);
                for (i, name) in w.names.iter().enumerate() {
                    let cell = fields
                        .iter()
                        .find(|f| f.name == *name)
                        .map(|f| cell_text(f))
                        .unwrap_or_else(|| ABSENT.to_string());
                    let last = i + 1 == w.names.len();
                    push_cell(&mut line, &cell, w.cols[i], last);
                }
                // 行末の空白は残さない (列が 1 つも無い activity でも崩れないように)
                let line = format!("{}\n", line.trim_end());
                out.write_all(line.as_bytes())
                    .map_err(super::sadf::wrap_io)?;
            }
        }
        Ok(ScanControl::Continue)
    })?;
    Ok(())
}

fn write_block_header<W: Write>(out: &mut W, id: ActivityId, w: &Widths) -> Result<()> {
    let label = id.label().unwrap_or("");
    writeln!(out).map_err(super::sadf::wrap_io)?;
    writeln!(out, "{} — {label}  (time は UTC)", id.display_name())
        .map_err(super::sadf::wrap_io)?;

    let mut head = String::from(INDENT);
    push_cell(&mut head, "time", w.time, false);
    push_cell(&mut head, "item", w.item, false);
    for (i, name) in w.names.iter().enumerate() {
        push_cell(&mut head, name, w.cols[i], i + 1 == w.names.len());
    }
    writeln!(out, "{}", head.trim_end()).map_err(super::sadf::wrap_io)?;

    // 単位の行 (すべて空なら省く)
    if w.units.iter().any(|u| !u.is_empty()) {
        let mut units = String::from(INDENT);
        push_cell(&mut units, "", w.time, false);
        push_cell(&mut units, "", w.item, false);
        for (i, u) in w.units.iter().enumerate() {
            push_cell(&mut units, u, w.cols[i], i + 1 == w.units.len());
        }
        writeln!(out, "{}", units.trim_end()).map_err(super::sadf::wrap_io)?;
    }
    Ok(())
}

/// 区間終点の UTC 時刻 (`HH:MM:SS`)。
///
/// レコードが持つ「収集時ローカルの時分秒」ではなく epoch 秒から作る。
/// JSON / NDJSON / CSV が出す `end_epoch` と同じ時点を指すようにするため。
fn utc_hms(ust_time: u64) -> String {
    use chrono::{TimeZone, Timelike, Utc};
    let t = Utc
        .timestamp_opt(ust_time as i64, 0)
        .single()
        .unwrap_or_else(|| Utc.timestamp_opt(0, 0).unwrap());
    format!("{:02}:{:02}:{:02}", t.hour(), t.minute(), t.second())
}

/// 1 回目の走査。activity ごとの列幅を決める。
fn measure(file: &SaFile, cfg: &CustomConfig) -> Result<BTreeMap<u32, Widths>> {
    let mut widths: BTreeMap<u32, Widths> = BTreeMap::new();

    walk_items(file, &cfg.selection.clone(), |item| {
        // 幅の計測も統計行だけを見る (イベント行は表に並ばない)。
        let WalkItem::Sample(view) = item else {
            return Ok(ScanControl::Continue);
        };
        if !view.has_prev && cfg.values.wants_rates() {
            return Ok(ScanControl::Continue);
        }
        let ts_len = "00:00:00".len();
        for id in selected_ids(view, cfg) {
            let Some(pair) = ActivityPair::from_view(view, id) else {
                continue;
            };
            let Some(sp) = spec::lookup(id) else { continue };

            let entry = widths.entry(id.0).or_insert_with(|| Widths {
                time: "time".len().max(ts_len),
                item: "item".len(),
                ..Default::default()
            });

            for item in pair.output_items() {
                let row = item_out(pair.def, sp, &item, cfg);
                entry.item = entry.item.max(display_width(&row.item));

                let fields = display_fields(&row.raw, &row.rates);
                for f in &fields {
                    match entry.names.iter().position(|n| *n == f.name) {
                        Some(i) => {
                            entry.cols[i] = entry.cols[i].max(display_width(&cell_text(f)));
                        }
                        None => {
                            entry.names.push(f.name);
                            entry.units.push(f.unit);
                            entry
                                .cols
                                .push(display_width(f.name).max(display_width(&cell_text(f))));
                        }
                    }
                }
            }
        }
        Ok(ScanControl::Continue)
    })?;

    // 単位の行も列幅に効く
    for w in widths.values_mut() {
        for (i, u) in w.units.iter().enumerate() {
            w.cols[i] = w.cols[i].max(display_width(u));
        }
    }
    Ok(widths)
}

/// 表示する列を選ぶ。
///
/// 派生値があればそれを、無ければ生値を出す。`--values both` のときも
/// テーブルは 1 列 1 値なので派生値を優先する (両方欲しいときは JSON / NDJSON)。
fn display_fields<'a>(raw: &'a [FieldOut], rates: &'a [FieldOut]) -> Vec<&'a FieldOut> {
    if rates.is_empty() {
        raw.iter().collect()
    } else {
        rates.iter().collect()
    }
}

/// セルの文字列。値が無ければ [`ABSENT`]。
///
/// 文字列フィールド (デバイス名 / マウントポイント / 製品名) はそのまま出す。
/// 値が取れなかった理由は表には出さず `-` で表す (理由が要るときは
/// NDJSON / CSV の `quality` 列を見る)。
fn cell_text(f: &FieldOut) -> String {
    if let Some(t) = &f.text {
        return t.clone();
    }
    if let Some(v) = f.value {
        return format_number(v);
    }
    if let Some(r) = &f.raw {
        return r.clone();
    }
    ABSENT.to_string()
}

/// 数値の表示。桁が大きいものは小数を落とす。
///
/// 端末で読むための整形であり、`sadf` 互換の固定桁とは無関係。
pub fn format_number(v: f64) -> String {
    if !v.is_finite() {
        return ABSENT.to_string();
    }
    let a = v.abs();
    if a >= 100_000.0 {
        format!("{v:.0}")
    } else if a >= 1000.0 {
        format!("{v:.1}")
    } else {
        format!("{v:.2}")
    }
}

/// 右詰めでセルを足す。最後の列の後ろには空白を入れない (行末空白を作らない)。
fn push_cell(line: &mut String, text: &str, width: usize, last: bool) {
    let pad = width.saturating_sub(display_width(text));
    for _ in 0..pad {
        line.push(' ');
    }
    line.push_str(text);
    if !last {
        for _ in 0..GAP {
            line.push(' ');
        }
    }
}

/// 表示幅。
///
/// 列名と単位は ASCII なのでバイト数と一致するが、デバイス名に非 ASCII が
/// 入り得るため文字数で数える (パディングを**バイト数**で決めると崩れる)。
fn display_width(s: &str) -> usize {
    s.chars().count()
}

/// 既定の設定 (`--format table` で activity を絞らない場合)。
pub fn default_config() -> CustomConfig {
    CustomConfig {
        selection: Selection::All,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::super::json::{Quality, unit_name};
    use super::*;

    #[test]
    fn cells_are_right_aligned_with_two_space_gap() {
        let mut line = String::new();
        push_cell(&mut line, "all", 5, false);
        push_cell(&mut line, "2.15", 6, true);
        // 幅 5 に右詰め + 空白 2 + 幅 6 に右詰め
        assert_eq!(line, "  all    2.15");
    }

    /// 最後の列の後ろに空白を残さない。
    #[test]
    fn no_trailing_whitespace() {
        let mut line = String::new();
        push_cell(&mut line, "x", 3, true);
        assert_eq!(line, "  x");
        assert!(!line.ends_with(' '));
    }

    /// パディングは文字数で数える (バイト数だと非 ASCII で崩れる)。
    #[test]
    fn padding_counts_characters_not_bytes() {
        assert_eq!(display_width("abc"), 3);
        assert_eq!(display_width("メモリ"), 3);
        let mut line = String::new();
        push_cell(&mut line, "メモリ", 5, true);
        assert_eq!(line, "  メモリ");
    }

    #[test]
    fn number_format_drops_decimals_for_large_values() {
        assert_eq!(format_number(2.153), "2.15");
        assert_eq!(format_number(1234.56), "1234.6");
        assert_eq!(format_number(123456.7), "123457");
        assert_eq!(format_number(f64::NAN), ABSENT);
    }

    /// 値が無い列は `-`。0 にはしない。
    #[test]
    fn absent_cell_is_a_dash() {
        let f = FieldOut {
            name: "await",
            unit: "ms",
            kind: "gauge",
            raw: None,
            value: None,
            text: None,
            quality: Quality::NotImplemented,
        };
        assert_eq!(cell_text(&f), "-");

        let zero = FieldOut {
            value: Some(0.0),
            ..f.clone()
        };
        assert_eq!(cell_text(&zero), "0.00", "正常な 0 は 0 として出す");
    }

    /// 生値だけの場合は生値を表示する。
    #[test]
    fn raw_only_falls_back_to_raw_namespace() {
        let raw = vec![FieldOut {
            name: "user",
            unit: "percent",
            kind: "counter",
            raw: Some("96538".into()),
            value: None,
            text: None,
            quality: Quality::Ok,
        }];
        let picked = display_fields(&raw, &[]);
        assert_eq!(picked.len(), 1);
        assert_eq!(cell_text(picked[0]), "96538");
    }

    /// 単位表記は公開スキーマから取る。
    #[test]
    fn units_come_from_the_public_schema() {
        let def = crate::layout::registry::lookup(ActivityId::DISK).unwrap();
        let col = def
            .columns
            .iter()
            .find(|c| c.public_name == "await")
            .unwrap();
        assert_eq!(unit_name(col), "ms");
    }
}

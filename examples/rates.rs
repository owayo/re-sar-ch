//! `sa` ファイルから CPU 使用率を計算して表示する開発用サンプル。
//!
//! ```sh
//! cargo run --example rates -- <FILE>
//! ```
//!
//! 本番の出力は `output` 層が担当する。ここは series 層が計算まで
//! 到達できているかを確かめるための最小実装。

use re_sar_ch::format::SaFile;
use re_sar_ch::format::file::ScanControl;
use re_sar_ch::layout;
use re_sar_ch::model::{ActivityId, Availability};
use re_sar_ch::series::{Selection, WalkItem, walk_items};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).ok_or("usage: rates <FILE>")?;
    let f = SaFile::open(&path)?;
    let def = layout::registry::lookup(ActivityId::CPU).ok_or("A_CPU の定義が無い")?;

    // sar -u に出る列だけを拾う
    let cols: Vec<(usize, &str)> = def
        .columns
        .iter()
        .enumerate()
        .filter(|(_, c)| !c.sar_header.is_empty())
        .map(|(i, c)| (i, c.sar_header))
        .collect();

    print!("{:>8} {:>5}", "time", "CPU");
    for (_, name) in &cols {
        print!(" {name:>9}");
    }
    println!();

    let mut shown = 0usize;
    walk_items(&f, &Selection::Only(vec![ActivityId::CPU]), |item| {
        // ここでは使用率だけを見るので RESTART / COMMENT は読み飛ばす
        let WalkItem::Sample(view) = item else {
            return Ok(ScanControl::Continue);
        };
        if !view.has_prev || shown >= 5 {
            return Ok(ScanControl::Continue);
        }
        let plan = view.plan_for(ActivityId::CPU).expect("CPU の計画");
        let (Some(pa), Some(ca)) = (
            view.prev.activity(ActivityId::CPU),
            view.curr.activity(ActivityId::CPU),
        ) else {
            return Ok(ScanControl::Continue);
        };

        for (idx, (pi, ci)) in pa.items.iter().zip(ca.items.iter()).enumerate() {
            // CPU 使用率はグローバル itv ではなく per-CPU の tick 合計で正規化する
            let tot: u64 = ci
                .values
                .iter()
                .zip(pi.values.iter())
                .filter_map(|(c, p)| match (c, p) {
                    (Availability::Present(c), Availability::Present(p)) => {
                        Some(c.wrapping_sub(*p))
                    }
                    _ => None,
                })
                .sum();
            if tot == 0 {
                continue; // オフライン CPU (全フィールドが 0)
            }

            let label = if idx == 0 {
                "all".to_string()
            } else {
                (idx - 1).to_string()
            };
            print!(
                "{:02}:{:02}:{:02} {label:>5}",
                view.curr.hour, view.curr.minute, view.curr.second
            );
            for (ci_idx, _) in &cols {
                let p = plan.column_value(&pi.values, *ci_idx);
                let c = plan.column_value(&ci.values, *ci_idx);
                match (p, c) {
                    (Availability::Present(p), Availability::Present(c)) => {
                        let pct = c.wrapping_sub(p) as f64 / tot as f64 * 100.0;
                        print!(" {pct:>9.2}");
                    }
                    _ => print!(" {:>9}", "-"),
                }
            }
            println!();
        }
        shown += 1;
        Ok(ScanControl::Continue)
    })?;
    Ok(())
}

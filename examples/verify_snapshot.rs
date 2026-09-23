//! Reject incomplete activity decoding when verifying native live snapshots.
//!
//! Unlike `scan`, this requires every declared activity to have a decode plan
//! and exercises decoding of all samples, including at least one record pair.

use re_sar_ch::format::{SaFile, ScanControl};
use re_sar_ch::series::snapshot::plan_activities;
use re_sar_ch::series::{Selection, walk};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or("usage: verify_snapshot <FILE>")?;
    let file = SaFile::open(path)?;
    let plans = plan_activities(&file, &Selection::All)?;
    println!(
        "activities: declared={} planned={} skipped={}",
        file.activities().len(),
        plans.plans.len(),
        plans.skipped.len()
    );
    for skipped in &plans.skipped {
        println!(
            "skipped: index={} id={} reason={}",
            skipped.index, skipped.id.0, skipped.reason
        );
    }
    if !plans.skipped.is_empty() {
        return Err("snapshot contains activities that cannot be decoded".into());
    }
    let mut pairs = 0_u64;
    let summary = walk(&file, &Selection::All, |view| {
        if view.prev.valid && view.curr.valid {
            pairs += 1;
        }
        Ok(ScanControl::Continue)
    })?;
    println!(
        "decode: records={} pairs={} exact={}",
        summary.total_records(),
        pairs,
        summary.is_exact()
    );
    if !summary.is_exact() {
        return Err("snapshot decoding did not reach exact EOF".into());
    }
    if pairs == 0 {
        return Err("snapshot contains no decoded record pairs".into());
    }
    Ok(())
}

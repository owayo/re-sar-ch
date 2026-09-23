//! Native samples are independent of reSARch's layouts. Keep this matrix pinned
//! to its measurement manifest so adding future source pins needs no old samples.
use re_sar_ch::format::{SaFile, ScanControl};
use re_sar_ch::series::snapshot::plan_activities;
use re_sar_ch::series::{Selection, walk};

#[test]
fn all_official_snapshots_scan_and_decode() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest =
        std::fs::read_to_string(root.join("docs/measurements/upstream-matrix-2026-09-24.tsv"))
            .unwrap();
    let mut rows = manifest.lines().filter(|line| !line.starts_with('#'));
    let columns: Vec<_> = rows.next().unwrap().split('\t').collect();
    let column = |name| columns.iter().position(|value| *value == name).unwrap();
    let mut count = 0;
    let mut skipped = Vec::new();
    for row in rows {
        let fields: Vec<_> = row.split('\t').collect();
        let case = fields[column("case")];
        let path = root.join(fields[column("pair_dir")]).join("sa");
        let file = SaFile::open(&path).unwrap_or_else(|error| panic!("{case}: {error}"));
        let plans = plan_activities(&file, &Selection::All).unwrap();
        if !plans.skipped.is_empty() {
            skipped.push(format!("{case}: {:?}", plans.skipped));
        }
        assert_eq!(file.header().nodename, "resarch-fixture", "{case}");
        assert_eq!(
            format!("0x{:04x}", file.magic().format_magic),
            fields[column("format_magic")],
            "{case}"
        );
        let mut pairs = 0;
        let summary = walk(&file, &Selection::All, |view| {
            if view.prev.valid && view.curr.valid {
                pairs += 1;
            }
            Ok(ScanControl::Continue)
        })
        .unwrap_or_else(|error| panic!("{case}: {error}"));
        assert!(summary.is_exact(), "{case}");
        assert_eq!(
            summary.total_records(),
            fields[column("records")].parse::<u64>().unwrap(),
            "{case}"
        );
        assert!(pairs > 0, "{case}: no record pairs decoded");
        count += 1;
    }
    assert_eq!(count, 181);
    assert!(skipped.is_empty(), "{}", skipped.join("\n"));
}

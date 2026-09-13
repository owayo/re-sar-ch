mod fixtures;

use fixtures::{ActivitySpec, FixtureAbi, FixtureSpec, Generation, build};
use re_sar_ch::analyze::summary::{SummaryOptions, summarize_file};
use re_sar_ch::format::file::{SaFile, Tolerance};
use re_sar_ch::model::ActivityId;
use re_sar_ch::multi::{MultiOptions, analyze_files};
use re_sar_ch::series::snapshot::Selection;

#[test]
fn summary_and_multi_report_real_cpu_count() {
    let fixture = build(FixtureSpec::minimal(
        Generation::G2175Current,
        FixtureAbi::Le64,
    ));
    let file = SaFile::from_bytes("synthetic", fixture.bytes.clone()).unwrap();
    let summary = summarize_file(&file, &Selection::All, SummaryOptions::default()).unwrap();
    assert_eq!(summary.source.cpu_nr, Some(2));
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sa");
    std::fs::write(&path, &fixture.bytes).unwrap();
    let result = analyze_files(&[path], &MultiOptions::default()).unwrap();
    assert_eq!(result.hosts[0].identity.cpu_nr, Some(2));
    assert_eq!(result.hosts[0].segments[0].summary.source.cpu_nr, Some(2));
}

#[test]
fn multi_unknown_magic_uses_the_same_plans_as_single_file() {
    let mut spec = FixtureSpec::minimal(Generation::G2175Current, FixtureAbi::Le64);
    spec.activities[1] = ActivitySpec::known_id_unknown_magic();
    let fixture = build(spec);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sa");
    std::fs::write(&path, &fixture.bytes).unwrap();
    let result = analyze_files(&[path], &MultiOptions::default()).unwrap();
    for segment in &result.hosts[0].segments {
        assert!(segment.summary.activity(ActivityId::PCSW).is_none());
    }
}

#[test]
fn lenient_multi_preserves_complete_records_and_reports_truncation() {
    let fixture = build(FixtureSpec::minimal(
        Generation::G2175Current,
        FixtureAbi::Le64,
    ));
    let last_start = fixture.record_offsets.last().unwrap().0;
    let mut bytes = fixture.bytes;
    bytes.truncate(last_start + 2);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sa");
    std::fs::write(&path, bytes).unwrap();
    let mut opts = MultiOptions::default();
    opts.open.tolerance = Tolerance::Lenient;
    let result = analyze_files(&[path], &opts).unwrap();
    assert_eq!(result.incomplete_files.len(), 1);
    assert_eq!(result.incomplete_files[0].end_offset, last_start);
    assert_eq!(result.incomplete_files[0].trailing_bytes, 2);
    assert_eq!(result.hosts[0].segments[0].summary.period.samples, 1);
    assert!(result.skipped.is_empty());
}

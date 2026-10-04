mod fixtures;

use fixtures::{ActivitySpec, FixtureAbi, FixtureSpec, Generation, RecordSpec, build};
use re_sar_ch::analyze::summary::{SummaryOptions, summarize_file};
use re_sar_ch::format::file::{SaFile, Tolerance};
use re_sar_ch::model::ActivityId;
use re_sar_ch::multi::{MultiOptions, analyze_files};
use re_sar_ch::series::snapshot::Selection;

#[test]
fn zero_elapsed_is_excluded_from_single_and_cross_file_aggregates() {
    fn source(samples: &[(u64, u64, u64)]) -> Vec<u8> {
        let mut spec = FixtureSpec::skeleton(Generation::G2175Current, FixtureAbi::Le64);
        spec.activities = vec![ActivitySpec::a_pcsw()];
        spec.records = samples
            .iter()
            .map(|&(ust, uptime, _)| {
                let mut rec = RecordSpec::stats(vec![1], ust, 12, 0, 0);
                rec.uptime = uptime;
                rec
            })
            .collect();
        let mut fx = build(spec);
        // PCSW は ctxt (ULL) と processes (ULスロット)、各8バイト (02 §2)。
        for ((offset, _), (_, _, counter)) in fx.record_offsets.iter().zip(samples) {
            for field in [0, 8] {
                let at = offset + 24 + field;
                fx.bytes[at..at + 8].copy_from_slice(&counter.to_le_bytes());
            }
        }
        fx.bytes
    }
    fn check(summary: &re_sar_ch::analyze::summary::NativePeriodSummary) {
        use re_sar_ch::analyze::timeline::ExclusionReason;
        assert_eq!(summary.period.covered_cs, 100);
        assert_eq!(summary.period.continuous_intervals, 1);
        let activity = summary.activity(ActivityId::PCSW).unwrap();
        for column in &activity.items[0].columns {
            assert_eq!(column.mean, Some(10.0));
            assert_eq!(column.min.as_ref().unwrap().value, 10.0);
            assert_eq!(column.max.as_ref().unwrap().value, 10.0);
            assert_eq!(column.intervals, 1);
            assert_eq!(column.delta_total.as_deref(), Some("10"));
            assert_eq!(column.denominator_total.as_deref(), Some("100"));
            assert!(
                column
                    .exclusions
                    .iter()
                    .any(|e| e.reason == ExclusionReason::NonPositiveElapsed && e.intervals == 1)
            );
        }
    }
    let samples = [
        (1_600_000_000, 100_000, 100),
        (1_600_000_001, 100_000, 110),
        (1_600_000_002, 100_100, 120),
    ];
    let file = SaFile::from_bytes("synthetic", source(&samples)).unwrap();
    check(&summarize_file(&file, &Selection::All, SummaryOptions::default()).unwrap());
    let dir = tempfile::tempdir().unwrap();
    let paths: Vec<_> = samples
        .iter()
        .enumerate()
        .map(|(i, sample)| {
            let path = dir.path().join(format!("sa{i}"));
            std::fs::write(&path, source(&[*sample])).unwrap();
            path
        })
        .collect();
    let result = analyze_files(&paths, &MultiOptions::default()).unwrap();
    assert_eq!(result.hosts.len(), 1);
    assert_eq!(result.hosts[0].segments.len(), 1);
    check(&result.hosts[0].segments[0].summary);
}

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

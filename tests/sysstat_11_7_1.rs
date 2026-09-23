//! Independent values from the measured v11.7.1 table in 02-activities §3.4.
mod fixtures;

use fixtures::{ActivitySpec, FixtureAbi, FixtureSpec, Generation, RecordSpec, build};
use re_sar_ch::format::reader::Cursor;
use re_sar_ch::format::{SaFile, ScanControl};
use re_sar_ch::model::Availability;
use re_sar_ch::series::Selection;
use re_sar_ch::series::snapshot::plan_activities;

#[test]
fn omitted_magic_increments_decode_all_seventeen_layouts_on_both_abis_and_endians() {
    let facts = [
        (1, 0x8a, [10, 0, 0], 80),
        (2, 0x8a, [1, 1, 0], 16),
        (3, 0x8a, [1, 0, 0], 8),
        (7, 0x8a, [17, 0, 0], 136),
        (8, 0x8a, [4, 0, 0], 32),
        (9, 0x8b, [3, 0, 3], 40),
        (10, 0x8a, [0, 0, 7], 28),
        (11, 0x8b, [1, 2, 6], 48),
        (12, 0x8c, [7, 0, 1], 80),
        (13, 0x8b, [9, 0, 0], 88),
        (17, 0x8b, [8, 0, 0], 64),
        (18, 0x8b, [8, 0, 0], 64),
        (25, 0x8b, [10, 0, 0], 80),
        (26, 0x8b, [11, 0, 0], 88),
        (34, 0x8a, [2, 0, 0], 136), // Native HUGE stride bug; payload is 16.
        (35, 0x8a, [1, 1, 0], 16),
        (37, 0x8a, [5, 0, 0], 296),
    ];
    for abi in FixtureAbi::ALL {
        for (id, magic, types_nr, mut size) in facts {
            if id == 9 && abi.long_bytes() == 4 {
                size = 36;
            }
            let mut spec = FixtureSpec::skeleton(Generation::G2175V120, abi);
            spec.version = (11, 7, 1, 0);
            spec.activities = vec![ActivitySpec {
                id,
                magic,
                size,
                types_nr,
                nr: 1,
                nr2: 1,
                has_nr: false,
            }];
            spec.records = vec![
                RecordSpec::stats(vec![1], 1_600_000_000, 12, 0, 0),
                RecordSpec::stats(vec![1], 1_600_000_001, 12, 0, 1),
            ];
            let file = SaFile::from_bytes("11.7.1 synthetic", build(spec).bytes).unwrap();
            let plans = plan_activities(&file, &Selection::All).unwrap();
            assert!(
                plans.skipped.is_empty(),
                "id={id}, {abi:?}: {:?}",
                plans.skipped
            );
            assert_eq!(plans.plans.len(), 1);
            let plan = &plans.plans[0].plan;
            assert_eq!(plan.stride, size as usize);
            // Reading must not rewrite the header or change native compatibility.
            assert_eq!(file.activities()[0].magic, magic);
            assert!(!file.displays_activity(file.activities()[0].id));
            let cursor = Cursor::new(file.bytes(), file.encoding().endian);
            let mut seq = 0;
            let summary = file
                .scan(|record| {
                    let item = plan.item_view(&cursor, record.slices[0].offset).unwrap();
                    let mut values = Vec::new();
                    plan.decode_item_into(&item, &mut values).unwrap();
                    for (field, value) in values
                        .iter()
                        .take(types_nr.iter().sum::<u32>() as usize)
                        .enumerate()
                    {
                        assert_eq!(
                            *value,
                            Availability::Present(fixtures::stat_value(id, seq, 0, field)),
                            "id={id}, {abi:?}, field={field}"
                        );
                    }
                    seq += 1;
                    Ok(ScanControl::Continue)
                })
                .unwrap();
            assert!(summary.is_exact());
            assert_eq!(seq, 2);
        }
    }
}

#[test]
fn producer_exception_does_not_accept_other_versions_or_unknown_magic() {
    for version in [(11, 7, 0, 0), (11, 7, 1, 1), (11, 7, 2, 0), (12, 0, 0, 0)] {
        let mut spec = FixtureSpec::minimal(Generation::G2175V120, FixtureAbi::Le64);
        spec.version = version;
        spec.activities[0].magic = 0x8a;
        let file = SaFile::from_bytes("different producer", build(spec).bytes).unwrap();
        let plans = plan_activities(&file, &Selection::All).unwrap();
        assert_eq!(plans.skipped.len(), 1, "{version:?}");
    }
    let mut spec = FixtureSpec::minimal(Generation::G2175V120, FixtureAbi::Le64);
    spec.version = (11, 7, 1, 0);
    spec.activities[0].magic = 0xffff;
    let file = SaFile::from_bytes("unknown magic", build(spec).bytes).unwrap();
    assert_eq!(
        plan_activities(&file, &Selection::All)
            .unwrap()
            .skipped
            .len(),
        1
    );
}

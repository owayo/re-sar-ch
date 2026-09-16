//! Issue #6/#7/#9: independently authored wire fixtures lock down invalid input and old formats.
mod fixtures;

use fixtures::{ActivitySpec, ExtraSpec, FixtureAbi, FixtureSpec, Generation, RecordSpec, build};
use re_sar_ch::format::{OpenOptions, SaFile, ScanControl, Tolerance};
use re_sar_ch::layout::plan::{DeclaredShape, DecodePlan, Incompatible, select_revision};
use re_sar_ch::layout::registry;
use re_sar_ch::model::{ActivityId, CounterBits};

fn open(bytes: Vec<u8>, tolerance: Tolerance) -> re_sar_ch::error::Result<SaFile> {
    SaFile::from_bytes_with(
        "synthetic",
        bytes,
        OpenOptions {
            tolerance,
            ..Default::default()
        },
    )
}
fn offset(fields: &[(&str, usize)], name: &str) -> usize {
    fields.iter().find(|(n, _)| *n == name).unwrap().1
}

#[test]
fn record_time_validation_covers_every_generation_and_abi() {
    for generation in Generation::ALL {
        for abi in FixtureAbi::ALL {
            let base = FixtureSpec::minimal(generation, abi);
            for (hour, minute, second, epoch) in [
                (24, 0, 0, 1_600_000_000),
                (0, 60, 0, 1_600_000_000),
                (0, 0, 61, 1_600_000_000),
                (0, 0, 0, 999_999_999),
            ] {
                let mut spec = base.clone();
                let rec = &mut spec.records[0];
                rec.hour = hour;
                rec.minute = minute;
                rec.second = second;
                rec.ust_time = epoch;
                let file = open(build(spec).bytes, Tolerance::Strict).unwrap();
                assert!(
                    file.scan(|_| Ok(ScanControl::Continue)).is_err(),
                    "{generation:?} {abi:?}"
                );
            }
            let mut fx = build(base);
            let at = fx.first_record_off + offset(fx.facts().record_header_offsets, "record_type");
            fx.bytes[at] = 0;
            assert!(
                open(fx.bytes, Tolerance::Strict)
                    .unwrap()
                    .scan(|_| Ok(ScanControl::Continue))
                    .is_err()
            );
        }
    }
    let mut spec = FixtureSpec::minimal(Generation::G2175Current, FixtureAbi::Le64);
    spec.records[0].second = 60;
    assert!(
        open(build(spec).bytes, Tolerance::Strict)
            .unwrap()
            .scan(|_| Ok(ScanControl::Continue))
            .is_ok()
    );
}

#[test]
fn restart_cpu_count_validates_lower_upper_and_signed_bounds() {
    for abi in FixtureAbi::ALL {
        for nr in [0, 8194, -1] {
            let mut spec = FixtureSpec::skeleton(Generation::G2175Current, abi);
            spec.records = vec![RecordSpec::restart(nr, 1_600_000_001, 12, 0, 1)];
            assert!(
                open(build(spec).bytes, Tolerance::Strict)
                    .unwrap()
                    .scan(|_| Ok(ScanControl::Continue))
                    .is_err()
            );
        }
        for nr in [1, 8193] {
            let mut spec = FixtureSpec::skeleton(Generation::G2175Current, abi);
            spec.records = vec![RecordSpec::restart(nr, 1_600_000_001, 12, 0, 1)];
            assert!(
                open(build(spec).bytes, Tolerance::Strict)
                    .unwrap()
                    .scan(|_| Ok(ScanControl::Continue))
                    .is_ok()
            );
        }
    }
}

#[test]
fn thirty_two_bit_userspace_can_report_a_sixty_four_bit_kernel() {
    for (abi, machine) in [
        (FixtureAbi::Le32, "x86_64"),
        (FixtureAbi::Le32, "amd64"),
        (FixtureAbi::Be32, "aarch64"),
        (FixtureAbi::Be32, "arm64"),
        (FixtureAbi::Be32, "ppc64"),
        (FixtureAbi::Be32, "ppc64le"),
        (FixtureAbi::Be32, "mips64"),
        (FixtureAbi::Be32, "s390x"),
        (FixtureAbi::Be32, "sparc64"),
        (FixtureAbi::Be32, "riscv64"),
    ] {
        let mut spec = FixtureSpec::minimal(Generation::G2175Current, abi);
        spec.machine = machine.into();
        let file = open(build(spec).bytes, Tolerance::Strict).unwrap();
        assert!(
            file.scan(|_| Ok(ScanControl::Continue)).unwrap().is_exact(),
            "{machine}"
        );
    }
}

#[test]
fn comment_keeps_non_utf8_bytes_and_reserves_the_final_nul() {
    let mut spec = FixtureSpec::skeleton(Generation::G2175Current, FixtureAbi::Le64);
    spec.records = vec![RecordSpec::comment("hello", 1_600_000_001, 12, 0, 1)];
    let mut fx = build(spec);
    let body = fx.first_record_off + fx.record_header_size();
    fx.bytes[body..body + 64].fill(0xe9);
    open(fx.bytes, Tolerance::Strict)
        .unwrap()
        .scan(|rec| {
            assert_eq!(rec.comment, Some(&[0xe9; 63][..]));
            Ok(ScanControl::Continue)
        })
        .unwrap();
}

#[test]
fn lenient_drops_only_the_incomplete_record_for_every_payload_kind() {
    for generation in [Generation::G2173, Generation::G2175Current] {
        let mut spec = FixtureSpec::minimal(generation, FixtureAbi::Le64);
        if generation == Generation::G2175Current {
            for rec in &mut spec.records {
                rec.extra = vec![ExtraSpec::sample()];
            }
            spec.records.push(
                RecordSpec::extra_record(5, 1_600_000_040, 12, 0, 40)
                    .with_extra(vec![ExtraSpec::sample()]),
            );
        }
        let fx = build(spec);
        for (index, &(start, len)) in fx.record_offsets.iter().enumerate() {
            for cut in 1..len {
                let bytes = fx.bytes[..start + cut].to_vec();
                let strict = open(bytes.clone(), Tolerance::Strict).unwrap();
                assert!(strict.scan(|_| Ok(ScanControl::Continue)).is_err());
                let file = open(bytes, Tolerance::Lenient).unwrap();
                let mut visited = 0;
                let summary = file
                    .scan(|_| {
                        visited += 1;
                        Ok(ScanControl::Continue)
                    })
                    .unwrap();
                assert!(
                    summary.incomplete,
                    "{generation:?} record={index} cut={cut}"
                );
                assert_eq!(visited, index);
                assert_eq!(summary.total_records() as usize, index);
                assert_eq!(summary.end_offset, start);
                assert_eq!(summary.trailing_bytes, cut);
            }
        }
    }
}

#[test]
fn extra_types_must_fit_even_when_its_payload_is_opaque() {
    for in_header in [false, true] {
        let mut spec = FixtureSpec::minimal(Generation::G2175Current, FixtureAbi::Le64);
        if in_header {
            spec.file_extra = vec![ExtraSpec::sample()];
        } else {
            spec.records[1].extra = vec![ExtraSpec::sample()];
        }
        let mut fx = build(spec);
        let at = if in_header {
            fx.extra_chain_off
        } else {
            fx.record_offsets[1].0 + fx.record_header_size()
        };
        fx.bytes[at + 12..at + 16].copy_from_slice(&100u32.to_le_bytes());
        match open(fx.bytes, Tolerance::Strict) {
            Err(_) => assert!(in_header),
            Ok(file) => assert!(file.scan(|_| Ok(ScanControl::Continue)).is_err()),
        }
    }
}

#[test]
fn old_cpu_banner_uses_initial_activity_count_and_creation_time_has_no_record_limit() {
    let mut spec = FixtureSpec::minimal(Generation::G2173, FixtureAbi::Le64);
    spec.cpu_nr = 65;
    spec.ust_time = 12345;
    let file = open(build(spec).bytes, Tolerance::Strict).unwrap();
    assert_eq!(file.header().cpu_nr, Some(3));
    assert_eq!(file.header().real_cpu_count(), Some(2));
    assert!(file.scan(|_| Ok(ScanControl::Continue)).unwrap().is_exact());
}

#[test]
fn unknown_activity_is_skipped_without_interpreting_its_types() {
    let mut spec = FixtureSpec::skeleton(Generation::G2175Current, FixtureAbi::Le64);
    spec.activities = vec![ActivitySpec::unknown_id()];
    spec.records = vec![RecordSpec::stats(vec![1], 1_600_000_001, 12, 0, 1)];
    let mut fx = build(spec);
    let at = fx.file_activity_off + 24;
    fx.bytes[at..at + 4].copy_from_slice(&100u32.to_le_bytes());
    assert!(
        open(fx.bytes, Tolerance::Strict)
            .unwrap()
            .scan(|_| Ok(ScanControl::Continue))
            .unwrap()
            .is_exact()
    );
}

#[test]
fn unknown_magic_uses_generic_item_limit() {
    let mut spec = FixtureSpec::skeleton(Generation::G2175Current, FixtureAbi::Le64);
    let mut act = ActivitySpec::a_cpu(3);
    act.magic = 0xff;
    act.size = 8;
    act.types_nr = [1, 0, 0];
    spec.activities = vec![act];
    spec.fill = fixtures::StatFill::Extreme;
    spec.records = vec![RecordSpec::stats(vec![8194], 1_600_000_001, 12, 0, 1)];
    assert!(
        open(build(spec).bytes, Tolerance::Strict)
            .unwrap()
            .scan(|_| Ok(ScanControl::Continue))
            .unwrap()
            .is_exact()
    );
}

#[test]
fn declared_record_size_need_not_include_final_struct_padding() {
    let spec = FixtureSpec::skeleton(Generation::G2175Current, FixtureAbi::Le64);
    let mut fx = build(spec);
    let fh = fx.file_header_off;
    // G5 file_header: rec_types_nr at 40,44,48; rec_size at 56 (01 §3.3.5).
    fx.bytes[fh + 48..fh + 52].copy_from_slice(&2u32.to_le_bytes());
    fx.bytes[fh + 56..fh + 60].copy_from_slice(&28u32.to_le_bytes());
    assert!(open(fx.bytes, Tolerance::Strict).is_ok());
}

#[test]
fn declared_file_header_size_has_explicit_bounds() {
    for size in [0u32, 8193] {
        let mut fx = build(FixtureSpec::skeleton(
            Generation::G2175Current,
            FixtureAbi::Le64,
        ));
        fx.bytes[8..12].copy_from_slice(&size.to_le_bytes());
        assert!(matches!(
            open(fx.bytes, Tolerance::Strict),
            Err(re_sar_ch::error::Error::InconsistentHeader { .. })
        ));
    }
}

#[test]
fn self_describing_old_magic_never_selects_an_era_a_layout() {
    for (id, magic, size, types) in [
        (ActivityId::CPU, 0x8a, 80, [10, 0, 0]),
        (ActivityId::MEMORY, 0x8a, 136, [17, 0, 0]),
        (ActivityId::PCSW, 0x8a, 16, [1, 1, 0]),
        (ActivityId::DISK, 0x8b, 48, [1, 2, 6]),
        (ActivityId::NET_DEV, 0x8c, 80, [7, 0, 1]),
    ] {
        let shape = DeclaredShape {
            magic: Some(magic),
            size,
            types_nr: Some(types),
        };
        assert!(matches!(
            select_revision(registry::lookup(id).unwrap(), &shape),
            Err(Incompatible::UnknownMagic(_))
        ));
    }
    let def = registry::lookup(ActivityId::IO).unwrap();
    let rev = select_revision(
        def,
        &DeclaredShape {
            magic: Some(0x8b),
            size: 48,
            types_nr: Some([5, 0, 0]),
        },
    )
    .unwrap();
    assert_eq!(rev.size_lp64, 40);
}

#[test]
fn magicless_memory_and_network_use_the_old_counter_width() {
    let enc = re_sar_ch::format::abi::SourceEncoding::new(
        re_sar_ch::format::abi::Endian::Little,
        re_sar_ch::format::abi::LayoutAbi::I386,
    );
    for (id, size) in [
        (ActivityId::MEMORY, 64),
        (ActivityId::NET_EDEV, 88),
        (ActivityId::NET_IP, 64),
        (ActivityId::NET_IP6, 80),
        (ActivityId::NET_EIP6, 88),
    ] {
        let def = registry::lookup(id).unwrap();
        let shape = DeclaredShape {
            magic: None,
            size,
            types_nr: None,
        };
        let rev = select_revision(def, &shape).unwrap();
        assert_eq!(rev.magic, 0x8a);
        let plan = DecodePlan::build_for(def, rev, &shape, 1, 1, &enc).unwrap();
        assert_eq!(
            plan.column_bits(usize::from(id == ActivityId::NET_EDEV)),
            Some(CounterBits::B32),
            "{id}"
        );
    }
}

#[test]
fn conversion_unit_is_unchanged_by_wall_clock_anomalies() {
    for (wall, ticks) in [(3600, 60_000), (2, 100), (600, 0)] {
        let mut spec = FixtureSpec::skeleton(Generation::G2171, FixtureAbi::Le64);
        spec.activities = vec![ActivitySpec {
            id: 1,
            magic: 0x8a,
            nr: 2,
            nr2: 1,
            has_nr: false,
            size: 144,
            types_nr: [9, 0, 0],
        }];
        let mut first = RecordSpec::stats(vec![2], 1_600_000_000, 12, 0, 0);
        first.uptime = 100_000;
        let mut last = RecordSpec::stats(vec![2], 1_600_000_000 + wall, 12, 0, 1);
        last.uptime = 100_000 + ticks;
        spec.records = vec![first, last];
        let file = open(build(spec).bytes, Tolerance::Strict).unwrap();
        let mut converted = Vec::new();
        let report =
            re_sar_ch::convert::convert(&file, &Default::default(), &mut converted).unwrap();
        assert_eq!(report.hz, 100);
        let dest = open(converted, Tolerance::Strict).unwrap();
        let mut uptime = Vec::new();
        dest.scan(|rec| {
            uptime.push(rec.uptime_cs.unwrap());
            Ok(ScanControl::Continue)
        })
        .unwrap();
        assert_eq!(uptime, vec![100_000, 100_000 + ticks]);
    }
}

#[test]
fn unrecognized_old_magic_does_not_reject_other_activities() {
    let mut spec = FixtureSpec::minimal(Generation::G2175Current, FixtureAbi::Le64);
    spec.activities[0].magic = 0x8a;
    spec.activities.push(ActivitySpec {
        id: 7,
        magic: 0x8a,
        nr: 1,
        nr2: 1,
        has_nr: false,
        size: 136,
        types_nr: [17, 0, 0],
    });
    spec.records = vec![RecordSpec::stats(vec![3, 1, 1], 1_600_000_001, 12, 0, 1)];
    let file = open(build(spec).bytes, Tolerance::Strict).unwrap();
    let plans =
        re_sar_ch::series::snapshot::plan_activities(&file, &re_sar_ch::series::Selection::All)
            .unwrap();
    assert_eq!(plans.skipped.len(), 2);
    assert_eq!(plans.plans.len(), 1);
    assert_eq!(plans.plans[0].id, ActivityId::PCSW);
    assert!(file.scan(|_| Ok(ScanControl::Continue)).unwrap().is_exact());
}

/// **回帰テスト**: `file_activity` に activity magic を持たない世代でも、
/// activity がデコード計画まで到達すること。
///
/// `series::snapshot` が「この世代は activity magic を持つか」を
/// `format_magic != 0x2170` という**値比較**で判定していたため、同じ構造を持つ
/// 世代 (`0x1170` = RHEL/CentOS 6.5 以降の派生) を足した瞬間に
/// 「magic を持つ世代」と誤認し、**全 activity が「未知 magic」として無言で消えた**。
///
/// この壊れ方は見つけにくい。`SaFile::open` は成功し、ヘッダ表示も
/// activity 一覧も正常に出て、終了コードも 0 のままで、
/// **統計が 1 行も出ないことにしか現れない**。
/// 判定をレイアウト記述から導く形に直したので、ここで固定する。
#[test]
fn activities_are_planned_in_generations_without_activity_magic() {
    // **`0x2170` だけで試しても、この回帰は捕まえられない。**
    // かつての判定は `format_magic != 0x2170` だったので `0x2170` では正しく動き、
    // **`0x1170` のファイルでだけ全 activity が消えた**。必ず両方を通すこと。
    for magic in [0x2170u16, 0x1170] {
        for abi in FixtureAbi::ALL {
            let fx = fixtures::minimal(Generation::G2170, abi);
            let label = format!("{magic:04x}/{}", abi.name());

            // `0x1170` のヘッダ 4 構造体は `0x2170` と完全に同一なので
            // (`docs/format/01-file-format.md` §2.7)、`file_magic.format_magic`
            // (オフセット 2..4) の 2 バイトを差し替えるだけで有効な
            // `0x1170` ファイルになる。
            let mut bytes = fx.bytes.clone();
            let raw = if abi.is_big_endian() {
                magic.to_be_bytes()
            } else {
                magic.to_le_bytes()
            };
            bytes[2..4].copy_from_slice(&raw);

            // 自作 fixture が読めない ABI があっても、ここで見たいのは
            // 「読めたファイルで activity が消えないこと」なので飛ばす。
            let Ok(file) = SaFile::from_bytes(&label, bytes) else {
                continue;
            };
            assert_eq!(file.magic().format_magic, magic, "{label}: 世代の取り違え");

            let planned = re_sar_ch::series::snapshot::plan_activities(
                &file,
                &re_sar_ch::series::Selection::All,
            )
            .expect("計画を作れること");
            assert!(
                planned.skipped.is_empty(),
                "{label}: activity が読み飛ばされた: {:?}",
                planned
                    .skipped
                    .iter()
                    .map(|s| s.reason.as_str())
                    .collect::<Vec<_>>()
            );
            assert!(
                !planned.plans.is_empty(),
                "{label}: activity が 1 つも残っていない"
            );
        }
    }
}

/// **回帰テスト**: RHEL 派生 (`0x1170`) の `stats_io` は 80 バイトで、
/// 5 フィールドすべてが 16 バイト境界に並ぶ。
///
/// upstream 9.0.4 の `stats_io` は `unsigned int` × 5 の 20 バイトだが、
/// Red Hat の `sysstat-9.0.4-diskstats.patch` が
/// `unsigned long long __attribute__((aligned (16)))` × 5 に差し替えている。
/// `0x2170` 系には activity magic が無いため、**申告サイズだけが手がかり**になる。
/// 20 バイト版と取り違えると、`dk_drive` 以外の全列が隣のフィールドを読む。
#[test]
fn rhel_variant_stats_io_has_eighty_byte_layout() {
    use re_sar_ch::format::abi::{Endian, LayoutAbi, SourceEncoding};

    let def = registry::lookup(ActivityId::IO).expect("A_IO の定義がある");
    let shape = DeclaredShape {
        magic: None, // `0x2170` 系は activity magic を持たない
        size: 80,
        types_nr: None,
    };
    let rev = select_revision(def, &shape).expect("80 バイトの revision が選べること");
    assert_eq!(
        rev.size_lp64, 80,
        "申告サイズと一致する revision を選ぶこと"
    );

    let enc = SourceEncoding::new(Endian::Little, LayoutAbi::LP64);
    let resolved = rev.layout.resolve(&enc).expect("配置を解決できる");
    assert_eq!(resolved.size, 80);

    // 16 バイトスロットの先頭 8 バイトが値、残り 8 バイトはパディング。
    for (name, want) in [
        ("dk_drive", 0usize),
        ("dk_drive_rio", 16),
        ("dk_drive_wio", 32),
        ("dk_drive_rblk", 48),
        ("dk_drive_wblk", 64),
    ] {
        assert_eq!(
            resolved.field(name).expect("フィールドがある").offset,
            want,
            "{name} の位置"
        );
    }

    // upstream 20 バイト版と取り違えていないこと。
    let upstream = select_revision(
        def,
        &DeclaredShape {
            magic: None,
            size: 20,
            types_nr: None,
        },
    )
    .expect("20 バイトの revision も引けること");
    assert_eq!(upstream.size_lp64, 20);
    assert_ne!(
        upstream.layout.name, rev.layout.name,
        "20 バイト版と 80 バイト版は別のレイアウトでなければならない"
    );
}

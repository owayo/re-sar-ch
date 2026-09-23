//! docs/format/08-packed-legacy.md だけから独立に生成する fixture。
use re_sar_ch::format::abi::Endian;
use re_sar_ch::format::{OpenOptions, SaFile, ScanControl, Tolerance};
use re_sar_ch::model::{ActivityId as A, Availability as V};
use re_sar_ch::series::{Selection, walk};

fn put(bytes: &mut [u8], at: usize, value: u64, width: usize, be: bool) {
    let value = if be {
        value.to_be_bytes()
    } else {
        value.to_le_bytes()
    };
    bytes[at..at + width].copy_from_slice(if be {
        &value[8 - width..]
    } else {
        &value[..width]
    });
}

fn fixture(long: usize, be: bool, flags: u16) -> Vec<u8> {
    let mut row = Vec::new();
    let mut append = |value, width| {
        let at = row.len();
        row.resize(at + width, 0);
        put(&mut row, at, value, width, be);
    };
    if flags & 1 != 0 {
        append(101, long);
    }
    if flags & 2 != 0 {
        append(102, 4);
    }
    if flags & 4 != 0 {
        append(11, 4);
        append(12, 4);
        append(13, 4);
        append(14, long);
    }
    if flags & 8 != 0 {
        append(103, 4);
    }
    if flags & 0x10 != 0 {
        append(104, 4);
        append(105, 4);
    }
    if flags & 0x20 != 0 {
        append(106, 4);
        append(107, 4);
    }
    if flags & 0x40 != 0 {
        for value in 108..113 {
            append(value, 4);
        }
    }
    if flags & 0x80 != 0 {
        for value in [201, 202, 203] {
            append(value, 4);
        }
        append(204, long);
    }
    if flags & 0x100 != 0 {
        append(301, 4);
    }
    if flags & 0x200 != 0 {
        for value in 401..405 {
            append(value, long);
        }
    }
    let mut bytes = vec![0; 280];
    put(&mut bytes, 0, 0x015d, 2, false);
    bytes[2..8].copy_from_slice(&[28, 1, 124, 23, 59, 59]);
    put(&mut bytes, 8, flags.into(), 2, false);
    bytes[10] = 1 << 3; // sparse CPU #3
    bytes[42] = 1 << 7; // sparse IRQ #7
    put(&mut bytes, 74, row.len() as u64, 2, false);
    put(&mut bytes, 76, 2, long, be);
    bytes[84..89].copy_from_slice(b"Linux");
    bytes[214..231].copy_from_slice(b"synthetic-fixture");
    bytes[279] = long as u8;
    bytes.extend_from_slice(&row);
    bytes.extend_from_slice(&row);
    bytes
}

fn options(be: bool) -> OpenOptions {
    OpenOptions {
        legacy_endian: Some(if be { Endian::Big } else { Endian::Little }),
        ..Default::default()
    }
}

#[test]
fn packed_fields_use_native_width_without_alignment_and_keep_missing_values() {
    for long in [4, 8] {
        for be in [false, true] {
            let file =
                SaFile::from_bytes_with("packed", fixture(long, be, 0x3ff), options(be)).unwrap();
            let mut times = Vec::new();
            walk(&file, &Selection::All, |view| {
                let read = |id, name| {
                    let plan = view.plan_for(id).unwrap();
                    let index = plan.fields.iter().position(|f| f.name == name).unwrap();
                    view.curr.activity(id).unwrap().items[0].values[index]
                };
                assert_eq!(read(A::CPU, "cpu_user"), V::Present(11));
                assert_eq!(read(A::CPU, "cpu_idle"), V::Present(14));
                assert_eq!(read(A::CPU, "cpu_iowait"), V::UnsupportedBySource);
                assert_eq!(read(A::PCSW, "processes"), V::Present(101));
                assert_eq!(read(A::PCSW, "context_switch"), V::Present(102));
                assert_eq!(read(A::IRQ, "irq_nr"), V::Present(103));
                assert_eq!(read(A::PAGE, "pgpgout"), V::UnsupportedBySource);
                assert_eq!(read(A::SWAP, "pswpout"), V::Present(107));
                assert_eq!(read(A::IO, "dk_drive_wio"), V::Present(110));
                assert_eq!(read(A::IO, "dk_drive_wblk"), V::UnsupportedBySource);
                assert_eq!(read(A::MEMORY, "frmkb"), V::UnsupportedBySource);
                assert_eq!(view.curr.activity(A::CPU).unwrap().items.len(), 1);
                if view.has_prev {
                    assert_eq!(view.itv_cs, 200);
                }
                times.push(view.curr.ust_time);
                Ok(ScanControl::Continue)
            })
            .unwrap();
            assert_eq!(times, [1_709_164_799, 1_709_164_801]);
            assert!(file.scan(|_| Ok(ScanControl::Continue)).unwrap().is_exact());
        }
    }
}

#[test]
fn every_activity_subset_changes_offsets_without_inventing_missing_counters() {
    for flags in [1, 2, 4, 8, 0x10, 0x20, 0x40, 0x200, 0x44, 0x12] {
        let file =
            SaFile::from_bytes_with("packed", fixture(4, false, flags), options(false)).unwrap();
        walk(&file, &Selection::All, |view| {
            if flags & 3 != 0 {
                let plan = view.plan_for(A::PCSW).unwrap();
                let items = &view.curr.activity(A::PCSW).unwrap().items;
                for (name, flag, value) in [("processes", 1, 101), ("context_switch", 2, 102)] {
                    let i = plan.fields.iter().position(|f| f.name == name).unwrap();
                    assert_eq!(
                        items[0].values[i],
                        if flags & flag == 0 {
                            V::UnsupportedBySource
                        } else {
                            V::Present(value)
                        }
                    );
                }
            }
            Ok(ScanControl::Continue)
        })
        .unwrap();
        assert!(file.scan(|_| Ok(ScanControl::Continue)).unwrap().is_exact());
    }
}

#[test]
fn bitmap_storage_bits_outside_writer_limits_do_not_add_payload_items() {
    for long in [4, 8] {
        for be in [false, true] {
            let mut bytes = fixture(long, be, 0x3ff);
            bytes[14..42].fill(0xff);
            bytes[70..74].fill(0xff);
            let file = SaFile::from_bytes_with("padding", bytes.clone(), options(be)).unwrap();
            let summary = file.scan(|_| Ok(ScanControl::Continue)).unwrap();
            assert_eq!(summary.stats, 2);
            assert!(summary.is_exact());
            // 有効範囲の bit は引き続き payload の長さと一致しなければならない。
            bytes[10] |= 1;
            assert!(SaFile::from_bytes_with("invalid-count", bytes, options(be)).is_err());
        }
    }

    // 5 CPU と IRQ 8〜223 の全選択。格納用 IRQ bitmap の末尾も ff になる。
    // LP64 の固定列 72 + CPU 100 + IRQ 864 + memory 32 = 1068 バイト。
    let original = fixture(8, false, 0x3ff);
    let mut row = original[280..352].to_vec();
    for _ in 0..5 {
        row.extend_from_slice(&original[352..372]);
    }
    for _ in 8..224 {
        row.extend_from_slice(&301u32.to_le_bytes());
    }
    row.extend_from_slice(&original[376..408]);
    assert_eq!(row.len(), 1068);
    let mut bytes = original[..280].to_vec();
    bytes[10] = 0x1f;
    bytes[42] = 0;
    bytes[43..74].fill(0xff);
    put(&mut bytes, 74, 1068, 2, false);
    for _ in 0..3 {
        bytes.extend_from_slice(&row);
    }
    assert_eq!(bytes.len(), 3484);
    let file = SaFile::from_bytes_with("full-selection", bytes, options(false)).unwrap();
    let summary = file.scan(|_| Ok(ScanControl::Continue)).unwrap();
    assert_eq!(summary.stats, 3);
    assert!(summary.is_exact());
}

#[test]
fn packed_headers_truncation_stop_and_time_overflow_are_checked() {
    let bytes = fixture(4, false, 4);
    for (at, value) in [
        (8, 0),
        (9, 0x80),
        (74, 1),
        (76, 0),
        (279, 7),
        (3, 12),
        (5, 24),
    ] {
        let mut broken = bytes.clone();
        broken[at] = value;
        assert!(
            SaFile::from_bytes_with("invalid", broken, options(false)).is_err(),
            "at {at}"
        );
    }
    for len in [0, 1, 279] {
        assert!(
            SaFile::from_bytes_with("truncated", bytes[..len].to_vec(), options(false)).is_err()
        );
    }
    let truncated = bytes[..bytes.len() - 1].to_vec();
    let strict = SaFile::from_bytes_with("truncated", truncated.clone(), options(false)).unwrap();
    assert!(strict.scan(|_| Ok(ScanControl::Continue)).is_err());
    let lenient = SaFile::from_bytes_with(
        "truncated",
        truncated,
        OpenOptions {
            tolerance: Tolerance::Lenient,
            ..options(false)
        },
    )
    .unwrap();
    let summary = lenient.scan(|_| Ok(ScanControl::Continue)).unwrap();
    assert_eq!(summary.stats, 1);
    assert!(!summary.is_exact());
    assert!(summary.incomplete);
    let file = SaFile::from_bytes_with("packed", bytes, options(false)).unwrap();
    let stopped = file.scan(|_| Ok(ScanControl::Stop)).unwrap();
    assert_eq!(stopped.stats, 1);
    assert!(stopped.stopped_early);
    assert!(!stopped.is_exact());
    let mut overflow = fixture(8, false, 4);
    put(&mut overflow, 76, i64::MAX as u64, 8, false);
    let overflow = SaFile::from_bytes_with("overflow", overflow, options(false)).unwrap();
    assert!(overflow.scan(|_| Ok(ScanControl::Continue)).is_err());
}

#[test]
fn packed_generation_renders_all_six_compatibility_formats() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("packed.sa");
    std::fs::write(&path, fixture(8, false, 0x3ff)).unwrap();
    for args in [
        vec!["sar", "-A", "-f"],
        vec!["sadf", "-d"],
        vec!["sadf", "-p"],
        vec!["sadf", "-r"],
        vec!["sadf", "-j"],
        vec!["sadf", "-x"],
    ] {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_resarch"))
            .args(&args)
            .arg(&path)
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!output.stdout.is_empty());
    }
}

//! 独立 fixture: docs/format/07-legacy-centos.md の位置表だけから組み立てる。
use re_sar_ch::format::{OpenOptions, SaFile, ScanControl, Tolerance};
use re_sar_ch::model::{ActivityId as A, Availability as V};
use re_sar_ch::series::snapshot::plan_activities;
use re_sar_ch::series::{Selection, walk};

fn put(b: &mut [u8], at: usize, n: u64, width: usize, be: bool) {
    let bytes = if be { n.to_be_bytes() } else { n.to_le_bytes() };
    b[at..at + width].copy_from_slice(if be {
        &bytes[8 - width..]
    } else {
        &bytes[..width]
    });
}

fn fixture(old: bool, long: usize, be: bool, smp: bool) -> Vec<u8> {
    let fixed = if old { 288 } else { 464 };
    let cpu_stride = if old { 32 } else { 112 };
    let cpus = if smp { 2 } else { 0 };
    let record = fixed + cpus * cpu_stride;
    let mut b = vec![0; 240 + record * 2];
    put(
        &mut b,
        if old { 4 } else { 36 },
        if old { 0x2163 } else { 0x2169 },
        2,
        be,
    );
    put(&mut b, if old { 6 } else { 38 }, fixed as u64, 2, be);
    put(&mut b, if old { 0 } else { 8 }, 0x1f00ef, 4, be);
    put(
        &mut b,
        if old { 28 } else { 24 },
        if smp { if old { 1 } else { 2 } } else { 0 },
        4,
        be,
    );
    put(&mut b, if old { 16 } else { 0 }, 1_700_000_000, long, be);
    b[40] = 14;
    b[41] = 10;
    b[42] = 123;
    if !old {
        b[43] = long as u8;
    }
    let string = if old { 43 } else { 44 };
    b[string..string + 5].copy_from_slice(b"Linux");
    for rec in 0..2 {
        let start = 240 + rec * record;
        let ty = start + if old { 0 } else { 448 };
        b[ty] = 1;
        b[ty + 1] = 12;
        b[ty + 3] = rec as u8;
        put(
            &mut b,
            start + if old { 8 } else { 160 },
            1_700_000_000 + rec as u64,
            long,
            be,
        );
        put(
            &mut b,
            start + if old { 16 } else { 0 },
            1000 + rec as u64 * 100,
            if old { long } else { 8 },
            be,
        );
        put(
            &mut b,
            start + if old { 24 } else { 16 },
            if smp { 500 + rec as u64 * 100 } else { 0 },
            if old { long } else { 8 },
            be,
        );
        for (o, n, w) in if old {
            vec![
                (44, 10, 4),
                (48, 20, 4),
                (52, 30, 4),
                (56, 40, long),
                (64, 50, long),
                (72, 60, long),
                (136, 700, long),
                (160, 1000, long),
            ]
        } else {
            vec![
                (48, 10, 8),
                (64, 20, 8),
                (80, 30, 8),
                (96, 40, 8),
                (112, 50, 8),
                (128, 55, 8),
                (144, 60, 8),
                (208, 700, long),
                (232, 1000, long),
            ]
        } {
            put(&mut b, start + o, n + rec as u64, w, be);
        }
        for cpu in 0..cpus {
            let at = start + fixed + cpu * cpu_stride;
            for (o, n, w) in if old {
                vec![
                    (0, 101, long),
                    (8, 102, long),
                    (16, 103, 4),
                    (20, 104, 4),
                    (24, 105, 4),
                ]
            } else {
                vec![
                    (0, 101, 8),
                    (16, 102, 8),
                    (32, 103, 8),
                    (48, 104, 8),
                    (64, 105, 8),
                    (80, 106, 8),
                ]
            } {
                put(&mut b, at + o, n + cpu as u64 * 100 + rec as u64, w, be);
            }
        }
    }
    b
}
fn open(b: Vec<u8>, long: usize, tolerance: Tolerance) -> SaFile {
    SaFile::from_bytes_with(
        "synthetic-legacy",
        b,
        OpenOptions {
            legacy_long_bytes: long as u8,
            tolerance,
            ..Default::default()
        },
    )
    .unwrap()
}

#[test]
fn fixed_and_per_cpu_layouts_work_for_both_generations_endians_and_long_widths() {
    for old in [false, true] {
        for long in [4, 8] {
            for be in [false, true] {
                for smp in [false, true] {
                    let file = open(fixture(old, long, be, smp), long, Tolerance::Strict);
                    let plans = plan_activities(&file, &Selection::All).unwrap();
                    assert!(plans.skipped.is_empty());
                    let mut count = 0;
                    let summary = walk(&file, &Selection::All, |v| {
                        let cpu = v.curr.activity(A::CPU).unwrap();
                        let plan = v.plan_for(A::CPU).unwrap();
                        let value = |i: usize, name: &str| {
                            let field = plan.fields.iter().position(|f| f.name == name).unwrap();
                            cpu.items[i].values[field]
                        };
                        assert_eq!(value(0, "cpu_user"), V::Present(10 + count));
                        assert_eq!(value(0, "cpu_sys"), V::Present(30 + count));
                        assert_eq!(value(0, "cpu_hardirq"), V::UnsupportedBySource);
                        assert_eq!(
                            value(0, "cpu_steal"),
                            if old {
                                V::UnsupportedBySource
                            } else {
                                V::Present(55 + count)
                            }
                        );
                        if smp {
                            assert_eq!(cpu.items.len(), 3);
                            assert_eq!(value(1, "cpu_user"), V::Present(103 + count));
                            assert_eq!(value(2, "cpu_idle"), V::Present(201 + count));
                        } else {
                            assert_eq!(cpu.items.len(), 1);
                        }
                        if v.has_prev {
                            assert_eq!(v.itv_cs, 100);
                        }
                        count += 1;
                        Ok(ScanControl::Continue)
                    })
                    .unwrap();
                    assert_eq!(count, 2);
                    assert!(summary.is_exact());
                }
            }
        }
    }
}

#[test]
fn partial_records_restart_and_early_stop_have_precise_boundaries() {
    for old in [false, true] {
        let b = fixture(old, 8, false, true);
        let record = (b.len() - 240) / 2;
        let fixed = if old { 288 } else { 464 };
        for cut in [1, record / 2, record - 1] {
            let mut short = b.clone();
            short.truncate(short.len() - cut);
            assert!(
                open(short.clone(), 8, Tolerance::Strict)
                    .scan(|_| Ok(ScanControl::Continue))
                    .is_err()
            );
            let scan = open(short, 8, Tolerance::Lenient)
                .scan(|_| Ok(ScanControl::Continue))
                .unwrap();
            assert_eq!(scan.stats, 1);
            assert!(scan.incomplete);
            assert_eq!(scan.end_offset, 240 + record);
            assert_eq!(scan.trailing_bytes, record - cut);
        }
        let mut with_restart = b[..240 + record].to_vec();
        let mut restart = b[240..240 + fixed].to_vec();
        restart[if old { 0 } else { 448 }] = 2;
        with_restart.extend(restart);
        with_restart.extend_from_slice(&b[240 + record..]);
        let scan = open(with_restart, 8, Tolerance::Strict)
            .scan(|_| Ok(ScanControl::Continue))
            .unwrap();
        assert!(scan.is_exact());
        assert_eq!(scan.restarts, 1);
        assert_eq!(scan.stats, 2);
        let scan = open(b, 8, Tolerance::Strict)
            .scan(|_| Ok(ScanControl::Stop))
            .unwrap();
        assert!(scan.stopped_early);
        assert_eq!(scan.end_offset, 240 + record);
    }
}

#[test]
fn malformed_counts_widths_dates_and_record_types_are_rejected() {
    for old in [false, true] {
        for at in [
            if old { 28 } else { 24 },
            if old { 36 } else { 32 },
            if old { 12 } else { 16 },
            if old { 24 } else { 20 },
        ] {
            let mut b = fixture(old, 8, false, true);
            put(&mut b, at, u32::MAX as u64, 4, false);
            assert!(SaFile::from_bytes("bad", b).is_err());
        }
        let mut b = fixture(old, 8, false, true);
        b[41] = 255;
        assert!(SaFile::from_bytes("bad", b).is_err());
        let mut b = fixture(old, 8, false, true);
        b[240 + if old { 0 } else { 448 }] = 4;
        assert!(
            open(b, 8, Tolerance::Strict)
                .scan(|_| Ok(ScanControl::Continue))
                .is_err()
        );
        if !old {
            let mut b = fixture(false, 8, false, true);
            b[43] = 3;
            assert!(SaFile::from_bytes("bad", b).is_err());
        }
    }
}

#[test]
fn all_twenty_collected_files_scan_and_decode_without_skipped_activities() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/sysstat-live");
    let mut count = 0;
    for date in std::fs::read_dir(root)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name() == "2026-09-24")
    {
        for case in std::fs::read_dir(date.path())
            .unwrap()
            .flatten()
            .filter(|e| e.path().is_dir())
        {
            let path = case.path().join("sa");
            if !path.exists() {
                continue;
            }
            let file = SaFile::open(&path).unwrap();
            let plans = plan_activities(&file, &Selection::All).unwrap();
            assert!(
                plans.skipped.is_empty(),
                "{}: {:?}",
                path.display(),
                plans.skipped
            );
            let scan = walk(&file, &Selection::All, |v| {
                if file.magic().format_magic == 0x2163 || file.magic().format_magic == 0x2169 {
                    let net = v.curr.activity(A::NET_DEV).unwrap();
                    assert_eq!(net.items.len(), 2);
                    assert_eq!(net.items[1].key.as_deref(), Some("eth0"));
                    if let Some(serial) = v.curr.activity(A::SERIAL) {
                        assert!(serial.items.is_empty());
                    }
                }
                Ok(ScanControl::Continue)
            })
            .unwrap();
            assert!(scan.is_exact(), "{}", path.display());
            assert!(scan.stats >= 2);
            count += 1;
        }
    }
    assert_eq!(count, 20);
}

#[test]
fn optional_irq_array_and_separate_pcsw_flags_preserve_absence() {
    for old in [false, true] {
        let original = fixture(old, 8, false, false);
        let record = (original.len() - 240) / 2;
        let mut bytes = original[..240].to_vec();
        // 個別 IRQ と proc のみ。総 IRQ と cswch は未採取。
        put(&mut bytes, if old { 0 } else { 8 }, 0x11, 4, false);
        for i in 0..2 {
            bytes.extend_from_slice(&original[240 + i * record..240 + (i + 1) * record]);
            for irq in 0..256u32 {
                bytes.extend_from_slice(&(irq + 123 + i as u32).to_le_bytes());
            }
        }
        let file = open(bytes, 8, Tolerance::Strict);
        let scan = walk(&file, &Selection::All, |v| {
            let irq = v.curr.activity(A::IRQ).unwrap();
            assert_eq!(irq.items.len(), 257);
            assert_eq!(irq.items[0].values[0], V::UnsupportedBySource);
            assert_eq!(
                irq.items[256].values[0],
                V::Present(378 + u64::from(v.has_prev))
            );
            let p = v.plan_for(A::PCSW).unwrap();
            let s = &v.curr.activity(A::PCSW).unwrap().items[0];
            assert_eq!(p.column_value(&s.values, 1), V::UnsupportedBySource);
            Ok(ScanControl::Continue)
        })
        .unwrap();
        assert!(scan.is_exact());
    }
}

#[test]
fn six_compatibility_formats_render_each_legacy_snapshot() {
    use std::process::Command;
    for version in ["3.9", "4.9", "5.11"] {
        let file = format!(
            "{}/testdata/sysstat-live/2026-09-24/centos-{version}/sa",
            env!("CARGO_MANIFEST_DIR")
        );
        for mode in ["sar", "-d", "-p", "-r", "-j", "-x"] {
            let mut cmd = Command::new(env!("CARGO_BIN_EXE_resarch"));
            cmd.env("LC_ALL", "C").env("TZ", "UTC");
            if mode == "sar" {
                cmd.args(["sar", "-A", "-t", "-f", &file]);
            } else {
                cmd.args(["sadf", mode, &file, "--", "-A"]);
            }
            let out = cmd.output().unwrap();
            assert!(
                out.status.success(),
                "{version} {mode}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let text = String::from_utf8(out.stdout).unwrap();
            assert!(text.contains("eth0"));
            assert!(!text.contains("4294967294"));
            if mode == "-j" {
                serde_json::from_str::<serde_json::Value>(&text).unwrap();
            }
        }
    }
}

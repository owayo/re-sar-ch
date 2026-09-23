//! Fixture は仕様書の実測表から構築する。本体のレイアウト定義は参照しない。
use re_sar_ch::format::{OpenOptions, SaFile, ScanControl, Tolerance};
use re_sar_ch::model::{ActivityId as A, Availability as V};
use re_sar_ch::series::{Selection, walk};
use std::collections::BTreeMap;

#[derive(Clone)]
struct Field {
    name: &'static str,
    ty: &'static str,
    at: [usize; 2],
}
#[derive(Default, Clone)]
struct Layout {
    size: usize,
    fields: Vec<Field>,
}
impl Layout {
    fn field(&self, name: &str) -> Option<&Field> {
        self.fields.iter().find(|f| f.name == name)
    }
    fn put(&self, b: &mut [u8], name: &str, value: u64, long: usize, be: bool) {
        if let Some(f) = self.field(name) {
            let width = match f.ty {
                "U8" => 1,
                "U16" => 2,
                "U32" => 4,
                "U64" => 8,
                "CULong" => long,
                _ => return,
            };
            put(b, f.at[usize::from(long == 4)], value, width, be);
        }
    }
}
fn tables() -> BTreeMap<u16, BTreeMap<&'static str, Layout>> {
    let mut out: BTreeMap<u16, BTreeMap<&str, Layout>> = BTreeMap::new();
    for line in include_str!("../docs/format/legacy-layouts.tsv")
        .lines()
        .skip(1)
    {
        let p: Vec<_> = line.split('\t').collect();
        let magic = u16::from_str_radix(&p[0][2..], 16).unwrap();
        let s = out.entry(magic).or_default().entry(p[2]).or_default();
        s.size = p[3].parse().unwrap();
        s.fields.push(Field {
            name: p[4],
            ty: p[5],
            at: [p[6].parse().unwrap(), p[7].parse().unwrap()],
        });
    }
    out
}
fn put(b: &mut [u8], at: usize, n: u64, width: usize, be: bool) {
    let raw = if be { n.to_be_bytes() } else { n.to_le_bytes() };
    b[at..at + width].copy_from_slice(if be { &raw[8 - width..] } else { &raw[..width] });
}
fn fixture(magic: u16, map: &BTreeMap<&str, Layout>, long: usize, be: bool, smp: bool) -> Vec<u8> {
    let prefix = if magic == 0x216f { 8 } else { 0 };
    let h = &map["file_hdr"];
    let stats = &map["file_stats"];
    let cpu = &map["stats_one_cpu"];
    let serial = &map["stats_serial"];
    let net = &map["stats_net_dev"];
    let nr_irq = if magic < 0x2163 { 224 } else { 256 };
    let nr_cpu = if smp { 2 } else { 0 };
    let irq_cpus = if magic <= 0x2168 {
        if smp { 2 } else { 1 }
    } else {
        nr_cpu
    };
    let size = stats.size + cpu.size * nr_cpu + 4 * nr_irq + serial.size + 8 * irq_cpus + net.size;
    let mut out = vec![0; prefix + h.size];
    if prefix != 0 {
        put(&mut out, 0, 0xd596, 2, be);
        put(&mut out, 2, magic.into(), 2, be);
        out[4..8].copy_from_slice(&[8, 1, 1, 0]);
    }
    let header = &mut out[prefix..];
    let flags = if magic < 0x2160 {
        4 | 8 | 256 | 8192 | 131072 | 16384
    } else {
        4 | 8 | 16 | 256 | 512 | 131072
    };
    for (name, value) in [
        ("sa_magic", u64::from(magic)),
        ("sa_st_size", stats.size as u64),
        ("sa_actflag", flags),
        (
            "sa_proc",
            if smp {
                if magic <= 0x2168 { 1 } else { 2 }
            } else {
                0
            },
        ),
        ("sa_serial", 1),
        ("sa_iface", 1),
        ("sa_irqcpu", 1),
        ("sa_day", 14),
        ("sa_month", 10),
        ("sa_year", 123),
        ("sa_sizeof_long", long as u64),
    ] {
        h.put(header, name, value, long, be);
    }
    let host = h.field("sa_nodename").unwrap().at[0];
    header[host..host + 7].copy_from_slice(b"fixture");
    for rec in 0..2 {
        let mut b = vec![0; size];
        for (name, value) in [
            ("record_type", 1),
            ("hour", 12),
            ("second", rec),
            ("ust_time", 1_700_000_000 + rec),
            ("uptime", 1000 + rec * 200),
            ("uptime0", 500 + rec * 100),
            ("cpu_user", 100 + rec),
            ("cpu_idle", 800 + rec),
            ("frmkb", if long == 8 { 0x123456789 } else { 0xf1234567 }),
        ] {
            stats.put(&mut b, name, value, long, be);
        }
        let mut at = stats.size;
        for c in 0..nr_cpu {
            cpu.put(
                &mut b[at..],
                "per_cpu_user",
                1000 + c as u64 * 100 + rec,
                long,
                be,
            );
            cpu.put(
                &mut b[at..],
                "per_cpu_idle",
                2000 + c as u64 * 100 + rec,
                long,
                be,
            );
            at += cpu.size;
        }
        for irq in 0..nr_irq {
            put(&mut b, at + irq * 4, irq as u64 + 3000 + rec, 4, be);
        }
        at += nr_irq * 4;
        serial.put(&mut b[at..], "line", 7, long, be);
        serial.put(&mut b[at..], "rx", 4000 + rec, long, be);
        at += serial.size + 8 * irq_cpus;
        net.put(&mut b[at..], "rx_packets", 5000 + rec, long, be);
        let iface = net.field("interface").unwrap().at[0];
        b[at + iface..at + iface + 4].copy_from_slice(b"eth0");
        out.extend(b);
    }
    out
}
fn open(b: Vec<u8>, long: usize, tolerance: Tolerance) -> SaFile {
    SaFile::from_bytes_with(
        "synthetic-generation",
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
fn legacy_kernel_units_are_not_silently_interpreted_as_modern_units() {
    let cases = tables();
    let map = &cases[&0x2168];
    for (release, supported) in [
        ("2.2.26", false),
        ("unknown", false),
        ("2.4.0", true),
        ("6.18.0", true),
    ] {
        let mut bytes = fixture(0x2168, map, 8, false, false);
        let h = &map["file_hdr"];
        h.put(&mut bytes, "sa_actflag", 0x10000 | 0x40, 8, false);
        // Clear optional arrays so the two records contain only the fixed statistics.
        for name in ["sa_serial", "sa_iface", "sa_irqcpu"] {
            h.put(&mut bytes, name, 0, 8, false);
        }
        let at = h.field("sa_release").unwrap().at[0];
        bytes[at..at + release.len()].copy_from_slice(release.as_bytes());
        bytes.truncate(h.size);
        for second in 0..2 {
            let stats = &map["file_stats"];
            let mut rec = vec![0; stats.size];
            for (name, value) in [
                ("record_type", 1),
                ("ust_time", 1_700_000_000 + second),
                ("uptime", 1000 + second * 100),
                ("pgpgin", 111),
                ("dk_drive_rblk", 222),
            ] {
                stats.put(&mut rec, name, value, 8, false);
            }
            bytes.extend(rec);
        }
        let file = open(bytes, 8, Tolerance::Strict);
        walk(&file, &Selection::All, |v| {
            for (id, name, raw) in [(A::PAGE, "pgpgin", 111), (A::IO, "dk_drive_rblk", 222)] {
                let plan = v.plan_for(id).unwrap();
                let index = plan
                    .fields
                    .iter()
                    .position(|field| field.name == name)
                    .unwrap();
                assert_eq!(
                    v.curr.activity(id).unwrap().items[0].values[index],
                    if supported {
                        V::Present(raw)
                    } else {
                        V::UnsupportedBySource
                    },
                    "{release}: {name}"
                );
            }
            Ok(ScanControl::Continue)
        })
        .unwrap();
    }
}

#[test]
fn all_monolithic_generations_decode_values_across_abis_and_array_boundaries() {
    let cases = tables();
    assert_eq!(cases.len(), 22);
    for (magic, map) in cases {
        for long in [4, 8] {
            for be in [false, true] {
                for smp in [false, true] {
                    let file = open(fixture(magic, &map, long, be, smp), long, Tolerance::Strict);
                    assert_eq!(file.magic().format_magic, magic);
                    assert_eq!(
                        file.magic().version_string(),
                        if magic == 0x216f { "8.1.1" } else { "unknown" }
                    );
                    let mut count = 0;
                    let summary = walk(&file, &Selection::All, |v| {
                        let value = |id: A, name: &str, item: usize| {
                            let plan = v.plan_for(id).unwrap();
                            let index = plan.fields.iter().position(|f| f.name == name).unwrap();
                            v.curr.activity(id).unwrap().items[item].values[index]
                        };
                        assert_eq!(
                            value(A::CPU, "cpu_user", 0),
                            V::Present(100 + count),
                            "{magic:x}"
                        );
                        assert_eq!(value(A::CPU, "cpu_hardirq", 0), V::UnsupportedBySource);
                        if smp {
                            assert_eq!(
                                value(A::CPU, "cpu_user", 2),
                                V::Present(1100 + count),
                                "{magic:x}"
                            );
                        }
                        assert_eq!(
                            value(A::MEMORY, "frmkb", 0),
                            V::Present(if long == 8 { 0x123456789 } else { 0xf1234567 }),
                            "{magic:x}"
                        );
                        let nr = if magic < 0x2163 { 224 } else { 256 };
                        assert_eq!(
                            value(A::IRQ, "irq_nr", nr),
                            V::Present(3000 + nr as u64 - 1 + count),
                            "{magic:x}"
                        );
                        assert_eq!(
                            value(A::SERIAL, "rx", 0),
                            V::Present(4000 + count),
                            "{magic:x}"
                        );
                        assert_eq!(
                            value(A::NET_DEV, "rx_packets", 0),
                            V::Present(5000 + count),
                            "{magic:x}"
                        );
                        count += 1;
                        Ok(ScanControl::Continue)
                    })
                    .unwrap();
                    assert_eq!(count, 2);
                    assert!(summary.is_exact(), "{magic:x}");
                }
            }
        }
    }
}

#[test]
fn all_monolithic_generations_detect_truncation_and_restart_boundaries() {
    for (magic, map) in tables() {
        let bytes = fixture(magic, &map, 8, false, true);
        let header = map["file_hdr"].size + if magic == 0x216f { 8 } else { 0 };
        let stride = (bytes.len() - header) / 2;
        for cut in [1, stride / 2, stride - 1] {
            let short = bytes[..bytes.len() - cut].to_vec();
            assert!(
                open(short.clone(), 8, Tolerance::Strict)
                    .scan(|_| Ok(ScanControl::Continue))
                    .is_err()
            );
            let s = open(short, 8, Tolerance::Lenient)
                .scan(|_| Ok(ScanControl::Continue))
                .unwrap();
            assert_eq!(s.stats, 1);
            assert_eq!(s.end_offset, header + stride);
            assert!(!s.is_exact());
        }
        let fixed = &map["file_stats"];
        let mut restart = bytes[header..header + fixed.size].to_vec();
        fixed.put(&mut restart, "record_type", 2, 8, false);
        let mut with_restart = bytes[..header + stride].to_vec();
        with_restart.extend(restart);
        with_restart.extend_from_slice(&bytes[header + stride..]);
        let s = open(with_restart, 8, Tolerance::Strict)
            .scan(|_| Ok(ScanControl::Continue))
            .unwrap();
        assert_eq!(s.stats, 2);
        assert_eq!(s.restarts, 1);
        assert!(s.is_exact());
    }
}

#[test]
fn comments_are_inside_fixed_records_and_follow_producer_long_width() {
    for (magic, map) in tables().into_iter().filter(|(magic, _)| *magic >= 0x216c) {
        for long in [4, 8] {
            for be in [false, true] {
                let mut b = fixture(magic, &map, long, be, false);
                let fixed = &map["file_stats"];
                let mut comment = vec![0; fixed.size];
                fixed.put(&mut comment, "record_type", 4, long, be);
                // 独立したABI境界の期待値。32bitは164、64bitは168（統計レコード先頭から）。
                let at = if long == 4 { 164 } else { 168 };
                comment[at..at + 12].copy_from_slice(b"comment-test");
                b.extend(comment);
                let mut seen = false;
                let s = open(b, long, Tolerance::Strict)
                    .scan(|r| {
                        if let Some(text) = r.comment {
                            assert_eq!(&text[..12], b"comment-test");
                            seen = true;
                        }
                        Ok(ScanControl::Continue)
                    })
                    .unwrap();
                assert!(seen);
                assert_eq!(s.comments, 1);
                assert!(s.is_exact());
            }
        }
    }
}

#[test]
fn every_generation_renders_all_six_compatibility_formats() {
    let dir = tempfile::tempdir().unwrap();
    for (magic, map) in tables() {
        let path = dir.path().join(format!("{magic:x}.sa"));
        std::fs::write(&path, fixture(magic, &map, 8, false, true)).unwrap();
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
                "{magic:x} {args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(!output.stdout.is_empty());
        }
    }
}

#[test]
fn vanished_serial_ports_use_the_generation_specific_sentinel() {
    for (magic, map) in tables() {
        let mut bytes = fixture(magic, &map, 8, false, false);
        let start = map["file_hdr"].size + if magic == 0x216f { 8 } else { 0 };
        let stride = (bytes.len() - start) / 2;
        let serial_at = map["file_stats"].size + if magic < 0x2163 { 224 * 4 } else { 256 * 4 };
        let serial = &map["stats_serial"];
        let sentinel = if serial.field("line").unwrap().ty == "U8" {
            255
        } else {
            u32::MAX as u64
        };
        for record in 0..2 {
            serial.put(
                &mut bytes[start + record * stride + serial_at..],
                "line",
                sentinel,
                8,
                false,
            );
        }
        let file = open(bytes, 8, Tolerance::Strict);
        walk(&file, &Selection::All, |v| {
            assert!(
                v.curr.activity(A::SERIAL).unwrap().items.is_empty(),
                "{magic:04x}"
            );
            Ok(ScanControl::Continue)
        })
        .unwrap();
    }
}

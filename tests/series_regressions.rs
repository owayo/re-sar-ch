mod fixtures;

use fixtures::{ActivitySpec, FixtureAbi, FixtureSpec, Generation, RecordSpec, build};
use re_sar_ch::format::file::SaFile;
use re_sar_ch::model::ActivityId;
use re_sar_ch::output::sadf::{self, SadfConfig};
use re_sar_ch::output::sar_text::{CpuSelection, SarTextOptions, write_report};

// wire のオフセットは docs/format/02-activities.md から独立に書き起こしたもの。
fn file(activity: ActivitySpec, samples: Vec<Vec<Vec<u8>>>) -> SaFile {
    let mut spec = FixtureSpec::skeleton(Generation::G2175Current, FixtureAbi::Le64);
    spec.activities = vec![activity.clone()];
    spec.records = samples
        .iter()
        .enumerate()
        .map(|(i, items)| {
            let count = items.len() as i32 / activity.nr2;
            let mut rec =
                RecordSpec::stats(vec![count], 1_600_000_000 + i as u64, 12, 26, 40 + i as u8);
            rec.uptime = 100_000 + i as u64 * 100;
            rec
        })
        .collect();
    let mut built = build(spec);
    for ((off, _), items) in built.record_offsets.iter().zip(samples) {
        let mut at = off + 24 + if activity.has_nr { 4 } else { 0 };
        for item in items {
            assert_eq!(item.len(), activity.size as usize);
            built.bytes[at..at + item.len()].copy_from_slice(&item);
            at += item.len();
        }
    }
    SaFile::from_bytes("synthetic", built.bytes).unwrap()
}
fn cpu(user: u64, idle: u64) -> Vec<u8> {
    let mut out = vec![0; 80];
    out[0..8].copy_from_slice(&user.to_le_bytes());
    out[24..32].copy_from_slice(&idle.to_le_bytes());
    out
}
fn emit(
    file: &SaFile,
    cfg: &SadfConfig,
    f: fn(&mut Vec<u8>, &SaFile, &SadfConfig) -> re_sar_ch::error::Result<()>,
) -> String {
    let mut out = Vec::new();
    f(&mut out, file, cfg).unwrap();
    String::from_utf8(out).unwrap()
}
fn cfg(id: ActivityId) -> SadfConfig {
    let mut cfg = SadfConfig {
        activities: Some(vec![id]),
        cpus: CpuSelection::All,
        ..Default::default()
    };
    cfg.section.cpu_all = false;
    cfg
}
#[test]
fn cpu_all_tickless_offline_and_selection_are_shared_by_sadf() {
    let file = file(
        ActivitySpec::a_cpu(4),
        vec![
            vec![cpu(900, 100), cpu(10, 90), cpu(20, 80), cpu(50, 50)],
            vec![cpu(990, 110), cpu(20, 180), cpu(20, 80), cpu(0, 0)],
        ],
    );
    let mut cfg = cfg(ActivityId::CPU);
    let json: serde_json::Value =
        serde_json::from_str(&emit(&file, &cfg, sadf::json::write_json)).unwrap();
    let rows = json["sysstat"]["hosts"][0]["statistics"][0]["cpu-load"]
        .as_array()
        .unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0]["user"], 10.0);
    assert_eq!(rows[0]["idle"], 90.0);
    assert_eq!(rows[2]["idle"], 100.0);
    let xml = emit(&file, &cfg, sadf::xml::write_xml);
    assert!(xml.contains("<cpu-load>"));
    assert!(!xml.contains("number=\"2\""));
    let raw = emit(&file, &cfg, sadf::raw::write_raw);
    assert!(raw.contains("; CPU; 2;"), "{raw}");
    cfg.cpus = CpuSelection::Listed {
        aggregate: false,
        cpus: vec![1],
    };
    for format in [
        sadf::dbppc::write_db,
        sadf::dbppc::write_ppc,
        sadf::json::write_json,
        sadf::xml::write_xml,
        sadf::raw::write_raw,
    ] {
        let text = emit(&file, &cfg, format);
        assert!(!text.contains("; CPU; -1;"));
        assert!(!text.contains("\"cpu\": \"all\""));
        assert!(!text.contains("number=\"all\""));
    }
    let mut sar = Vec::new();
    let options = SarTextOptions {
        cpus: CpuSelection::All,
        ..Default::default()
    };
    write_report(&mut sar, &file, &options, &[ActivityId::CPU]).unwrap();
    let sar = String::from_utf8(sar).unwrap();
    let avg = sar
        .lines()
        .find(|s| s.starts_with("Average:") && s.split_whitespace().nth(1) == Some("1"))
        .unwrap();
    assert_eq!(avg.split_whitespace().last(), Some("100.00"));
}
fn nic(name: &str, values: [u64; 7]) -> Vec<u8> {
    let mut out = vec![0; 80];
    for (i, v) in values.into_iter().enumerate() {
        out[i * 8..i * 8 + 8].copy_from_slice(&v.to_le_bytes());
    }
    // ull 7 個 (56 バイト)、speed は uint (4)、interface[16]、duplex は 1 バイト。
    out[60..60 + name.len()].copy_from_slice(name.as_bytes());
    out
}
#[test]
fn new_and_reregistered_network_items_use_zero_in_every_compat_format() {
    let activity = ActivitySpec {
        id: 12,
        magic: 0x8d,
        nr: 2,
        nr2: 1,
        has_nr: true,
        size: 80,
        types_nr: [7, 0, 1],
    };
    for new in [false, true] {
        let previous = nic(
            if new { "old" } else { "eth0" },
            [1000, 2000, 1000000, 2000000, 10, 10, 10],
        );
        let current = nic("eth0", [50, 60, 5000, 6000, 0, 0, 1]);
        let file = file(activity.clone(), vec![vec![previous], vec![current]]);
        let cfg = cfg(ActivityId::NET_DEV);
        let json: serde_json::Value =
            serde_json::from_str(&emit(&file, &cfg, sadf::json::write_json)).unwrap();
        assert_eq!(
            json["sysstat"]["hosts"][0]["statistics"][0]["network"]["net-dev"][0]["rxpck"], 50.0,
            "{json:#}"
        );
        for format in [
            sadf::dbppc::write_db,
            sadf::dbppc::write_ppc,
            sadf::xml::write_xml,
        ] {
            let text = emit(&file, &cfg, format);
            assert!(text.contains("50.00"), "{text}");
            assert!(!text.contains("184467"), "{text}");
        }
        let raw = emit(&file, &cfg, sadf::raw::write_raw);
        assert!(raw.contains("rxpck/s; 0; 50;"), "{raw}");
        let mut out = Vec::new();
        let opts = SarTextOptions::default();
        write_report(&mut out, &file, &opts, &[ActivityId::NET_DEV]).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("50.00"), "{text}");
        assert!(!text.contains("184467"));
    }
}
fn soft(processed: u32, backlog: u32) -> Vec<u8> {
    let mut out = vec![0; 24];
    out[..4].copy_from_slice(&processed.to_le_bytes());
    out[20..24].copy_from_slice(&backlog.to_le_bytes());
    out
}
#[test]
fn softnet_hotplug_aggregate_and_average_do_not_wrap() {
    let act = ActivitySpec {
        id: 39,
        magic: 0x8a,
        nr: 3,
        nr2: 1,
        has_nr: true,
        size: 24,
        types_nr: [0, 0, 6],
    };
    for (before, after) in [(soft(200, 0), soft(0, 0)), (soft(0, 0), soft(500, 0))] {
        let file = file(
            act.clone(),
            vec![
                vec![soft(0, 0), soft(100, 0), before],
                vec![soft(0, 0), soft(150, 0), after],
            ],
        );
        let cfg = cfg(ActivityId::NET_SOFT);
        let db = emit(&file, &cfg, sadf::dbppc::write_db);
        assert!(db.contains(";-1;50.00;"), "{db}");
        assert_eq!(
            db.lines().filter(|l| !l.starts_with('#')).count(),
            2,
            "{db}"
        );
        let mut out = Vec::new();
        write_report(
            &mut out,
            &file,
            &SarTextOptions {
                cpus: CpuSelection::All,
                ..Default::default()
            },
            &[ActivityId::NET_SOFT],
        )
        .unwrap();
        let text = String::from_utf8(out).unwrap();
        let avg = text
            .lines()
            .find(|l| l.starts_with("Average:") && l.contains("all"))
            .unwrap();
        assert!(avg.contains("50.00"), "{text}");
        assert!(!text.contains("429496"));
    }
}
fn irq(value: u32, name: &str) -> Vec<u8> {
    let mut out = vec![0; 12];
    out[..4].copy_from_slice(&value.to_le_bytes());
    out[4..4 + name.len()].copy_from_slice(name.as_bytes());
    out
}
#[test]
fn irq_masks_cpu_without_reference_and_keeps_file_aggregate() {
    let act = ActivitySpec {
        id: 3,
        magic: 0x8c,
        nr: 3,
        nr2: 2,
        has_nr: true,
        size: 12,
        types_nr: [0, 0, 1],
    };
    let file = file(
        act,
        vec![
            vec![
                irq(100, "sum"),
                irq(40, "timer"),
                irq(70, ""),
                irq(30, ""),
                irq(0, ""),
                irq(0, ""),
            ],
            vec![
                irq(150, "sum"),
                irq(65, "timer"),
                irq(90, ""),
                irq(40, ""),
                irq(500, ""),
                irq(100, ""),
            ],
        ],
    );
    let mut cfg = cfg(ActivityId::IRQ);
    cfg.item_names.insert(ActivityId::IRQ, vec!["timer".into()]);
    let db = emit(&file, &cfg, sadf::dbppc::write_db);
    assert!(db.contains(";timer;25.00;10.00"), "{db}");
    assert!(!db.contains(";sum;"));
    let json: serde_json::Value =
        serde_json::from_str(&emit(&file, &cfg, sadf::json::write_json)).unwrap();
    let row = &json["sysstat"]["hosts"][0]["statistics"][0]["interrupts"][0];
    assert_eq!(row["all"], 25.0);
    assert_eq!(row["CPU0"], 10.0);
    assert!(row.get("CPU1").is_none());
    let mut out = Vec::new();
    write_report(
        &mut out,
        &file,
        &SarTextOptions {
            cpus: CpuSelection::All,
            ..Default::default()
        },
        &[ActivityId::IRQ],
    )
    .unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(!text.contains("CPU1"), "{text}");
    assert!(text.contains("25.00"));
    cfg.horizontally = true;
    let text = emit(&file, &cfg, sadf::dbppc::write_db);
    assert!(text.contains(";INTR;CPU*[...]"));
    assert!(text.contains(";timer;25.00;10.00"));
}
fn disk(ios: u64, rd: u64, wr: u64, ticks: [u32; 4]) -> Vec<u8> {
    let mut out = vec![0; 80];
    for (offset, value) in [(0, ios), (24, rd), (32, wr)] {
        out[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }
    for (i, value) in ticks.into_iter().enumerate() {
        out[48 + i * 4..52 + i * 4].copy_from_slice(&value.to_le_bytes());
    }
    out[64..68].copy_from_slice(&8u32.to_le_bytes());
    out
}
#[test]
fn disk_reregistration_resets_derived_values_together() {
    let act = ActivitySpec {
        id: 11,
        magic: 0x8c,
        nr: 1,
        nr2: 1,
        has_nr: true,
        size: 80,
        types_nr: [3, 3, 8],
    };
    let file = file(
        act,
        vec![
            vec![disk(10000, 500000, 600000, [7000, 8000, 10000, 9000])],
            vec![disk(50, 5000, 6000, [70, 80, 100, 90])],
        ],
    );
    let cfg = cfg(ActivityId::DISK);
    let db = emit(&file, &cfg, sadf::dbppc::write_db);
    assert!(
        db.contains(";50.00;2500.00;3000.00;0.00;110.00;0.09;3.00;10.00"),
        "{db}"
    );
}
#[test]
fn hot_added_cpu_is_hidden_until_it_has_a_reference() {
    let file = file(
        ActivitySpec::a_cpu(3),
        vec![
            vec![cpu(1, 1), cpu(10, 90)],
            vec![cpu(2, 2), cpu(20, 180), cpu(50, 50)],
        ],
    );
    let db = emit(&file, &cfg(ActivityId::CPU), sadf::dbppc::write_db);
    assert_eq!(
        db.lines().filter(|l| !l.starts_with('#')).count(),
        2,
        "{db}"
    );
    assert!(db.contains(";-1;10.00;"));
}
#[test]
fn nic_overflow_exception_keeps_the_existing_registration() {
    let act = ActivitySpec {
        id: 12,
        magic: 0x8d,
        nr: 1,
        nr2: 1,
        has_nr: true,
        size: 80,
        types_nr: [7, 0, 1],
    };
    let file = file(
        act,
        vec![
            vec![nic("eth0", [100, 200, u64::MAX - 10, 2000, 0, 0, 0])],
            vec![nic("eth0", [110, 210, 10, 2100, 0, 0, 0])],
        ],
    );
    let db = emit(&file, &cfg(ActivityId::NET_DEV), sadf::dbppc::write_db);
    assert!(db.contains(";eth0;10.00;10.00;0.02;"), "{db}");
}
#[test]
fn sadf_preserves_activity_order_and_empty_statistics_spacing() {
    let mut spec = FixtureSpec::minimal(Generation::G2175Current, FixtureAbi::Le64);
    spec.activities.reverse();
    for rec in &mut spec.records {
        if let fixtures::RecordKind::Stats { counts } = &mut rec.kind {
            counts.reverse();
        }
    }
    let file = SaFile::from_bytes("synthetic", build(spec).bytes).unwrap();
    let cfg = SadfConfig::default();
    // -d / -p / -r の縦並びはファイルの記載順 (`id_seq[]`)
    let db = emit(&file, &cfg, sadf::dbppc::write_db);
    assert!(db.find(";proc/s;cswch/s").unwrap() < db.find(";CPU;").unwrap());
    // -j / -x は固定の `act[]` 順 (`generic_write_stats()` が act[] を回す、03 §9.4)
    let json = emit(&file, &cfg, sadf::json::write_json);
    assert!(
        json.find("\"cpu-load\"").unwrap() < json.find("\"process-and-context-switch\"").unwrap()
    );
    let xml = emit(&file, &cfg, sadf::xml::write_xml);
    assert!(xml.find("<cpu-load>").unwrap() < xml.find("<process-and-context-switch").unwrap());
    let mut empty = FixtureSpec::skeleton(Generation::G2175Current, FixtureAbi::Le64);
    empty.activities = vec![ActivitySpec::a_cpu(3)];
    let file = SaFile::from_bytes("synthetic", build(empty).bytes).unwrap();
    let cfg = SadfConfig {
        horizontally: true,
        ..Default::default()
    };
    assert!(emit(&file, &cfg, sadf::dbppc::write_db).is_empty());
    let json = emit(&file, &cfg, sadf::json::write_json);
    assert!(json.contains("\"statistics\": [\n\t\t\t],"), "{json}");
}
#[test]
fn irq_average_matches_cpu_identity_after_a_middle_column_goes_offline() {
    let act = ActivitySpec {
        id: 3,
        magic: 0x8c,
        nr: 3,
        nr2: 1,
        has_nr: true,
        size: 12,
        types_nr: [0, 0, 1],
    };
    let file = file(
        act,
        vec![
            vec![irq(100, "sum"), irq(10, ""), irq(80, "")],
            vec![irq(160, "sum"), irq(0, ""), irq(110, "")],
        ],
    );
    let mut out = Vec::new();
    write_report(
        &mut out,
        &file,
        &SarTextOptions {
            cpus: CpuSelection::All,
            ..Default::default()
        },
        &[ActivityId::IRQ],
    )
    .unwrap();
    let text = String::from_utf8(out).unwrap();
    let average = text
        .lines()
        .find(|l| l.starts_with("Average:") && l.split_whitespace().nth(1) == Some("sum"))
        .unwrap();
    assert_eq!(
        average.split_whitespace().collect::<Vec<_>>(),
        ["Average:", "sum", "60.00", "30.00"],
        "{text}"
    );
}

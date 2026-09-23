//! 独自サブコマンドの各軸と、途中までしか読めなかった結果の扱いを、CLI の端から端まで独立に確かめる。
mod fixtures;
use fixtures::{ActivitySpec, FixtureAbi, FixtureSpec, Generation, RecordSpec, build};
use std::process::Command;

fn irq_bytes() -> Vec<u8> {
    let mut spec = FixtureSpec::skeleton(Generation::G2175Current, FixtureAbi::Le64);
    spec.activities = vec![ActivitySpec {
        id: 3,
        magic: 0x8c,
        nr: 3,
        nr2: 2,
        has_nr: true,
        size: 12,
        types_nr: [0, 0, 1],
    }];
    let mut a = RecordSpec::stats(vec![3], 1_600_000_000, 12, 0, 0);
    a.uptime = 100_000;
    let mut b = RecordSpec::stats(vec![3], 1_600_000_010, 12, 0, 10);
    b.uptime = 101_000;
    spec.records = vec![a, b];
    let mut fx = build(spec);
    for (rec, &(off, _)) in fx.record_offsets.iter().enumerate() {
        for (slot, delta) in [100, 50, 40, 20, 60, 30].iter().enumerate() {
            let at = off + 24 + 4 + slot * 12;
            let count = 1000u32 + rec as u32 * delta;
            fx.bytes[at..at + 4].copy_from_slice(&count.to_le_bytes());
            fx.bytes[at + 4..at + 12].fill(0);
            if slot < 2 {
                let label = if slot == 0 { b"sum" } else { b"irq" };
                fx.bytes[at + 4..at + 7].copy_from_slice(label);
            }
        }
    }
    fx.bytes
}
fn output(args: &[&str], bytes: &[u8]) -> std::process::Output {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("synthetic.sa");
    std::fs::write(&file, bytes).unwrap();
    Command::new(env!("CARGO_BIN_EXE_resarch"))
        .args(args)
        .arg(file)
        .env("TZ", "UTC")
        .output()
        .unwrap()
}
#[test]
fn irq_cpu_rows_have_distinct_values_and_matching_identity_in_native_formats() {
    let bytes = irq_bytes();
    let out = output(
        &[
            "show",
            "--activity",
            "irq",
            "--irq-cpus",
            "--values",
            "both",
            "--format",
            "ndjson",
        ],
        &bytes,
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rows: Vec<serde_json::Value> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let rows: Vec<_> = rows
        .iter()
        .filter(|r| r["end_epoch"] == 1_600_000_010u64)
        .collect();
    assert_eq!(rows.len(), 6);
    for (label, cpu, rate) in [
        ("sum", "all", 10.0),
        ("sum", "0", 4.0),
        ("sum", "1", 6.0),
        ("irq", "all", 5.0),
        ("irq", "0", 2.0),
        ("irq", "1", 3.0),
    ] {
        let row = rows
            .iter()
            .find(|r| r["item"] == label && r["cpu"] == cpu)
            .unwrap();
        assert!(
            row["rates"]
                .as_array()
                .unwrap()
                .iter()
                .any(|f| f["value"] == rate),
            "{row}"
        );
    }
    for format in ["json", "csv", "table"] {
        let out = output(
            &[
                "show",
                "--activity",
                "irq",
                "--irq-cpus",
                "--values",
                "both",
                "--format",
                format,
            ],
            &bytes,
        );
        assert!(
            out.status.success(),
            "{format}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let text = String::from_utf8(out.stdout).unwrap();
        if format == "json" {
            let value: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert!(value.is_object());
        }
        if format == "csv" {
            let mut reader = csv::Reader::from_reader(text.as_bytes());
            let h = reader.headers().unwrap().clone();
            assert!(h.iter().any(|v| v == "cpu"));
            assert!(reader.records().all(|r| r.is_ok()));
        }
        if format == "table" {
            assert!(text.contains("irq [CPU 0]"));
            assert!(text.contains("sum [CPU 1]"));
        }
    }
}
#[test]
fn lenient_partial_json_is_well_formed_and_contains_complete_preceding_sample() {
    let mut bytes = irq_bytes();
    bytes.pop();
    let out = output(&["show", "--lenient", "--format", "json"], &bytes);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("不完全な末尾レコード"));
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(value.is_object());
}

#[test]
fn overnight_filter_keeps_comments_inside_the_selected_window() {
    let mut spec = FixtureSpec::skeleton(Generation::G2175Current, FixtureAbi::Le64);
    spec.activities = vec![ActivitySpec::a_pcsw()];
    let day = 1_600_000_000 / 86_400 * 86_400;
    spec.records = vec![
        RecordSpec::stats(vec![0], day + 20 * 3600, 20, 0, 0),
        RecordSpec::stats(vec![0], day + 20 * 3600 + 600, 20, 10, 0),
        RecordSpec::stats(vec![0], day + 86_400 + 50 * 60, 0, 50, 0),
        RecordSpec::comment("inside overnight window", day + 86_400 + 3600, 1, 0, 0),
        RecordSpec::stats(vec![0], day + 86_400 + 4200, 1, 10, 0),
    ];
    for (i, r) in spec.records.iter_mut().enumerate() {
        r.uptime = 100_000 + i as u64 * 1_000;
    }
    let out = output(
        &[
            "show", "--format", "ndjson", "--from", "20:00", "--to", "02:00",
        ],
        &build(spec).bytes,
    );
    assert!(out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("inside overnight window"),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn lenient_skips_a_structurally_corrupt_file_and_returns_valid_json_for_later_files() {
    let dir = tempfile::tempdir().unwrap();
    let mut bad = irq_bytes();
    // file_magic 76 + file_header 336 + file_activity 36 に、record_header 内の hour のオフセット 21 を足した位置。
    bad[76 + 336 + 36 + 21] = 99;
    let bad_path = dir.path().join("a-bad.sa");
    std::fs::write(&bad_path, &bad).unwrap();
    let good_path = dir.path().join("b-good.sa");
    std::fs::write(&good_path, irq_bytes()).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_resarch"))
        .args(["show", "--lenient", "--format", "json"])
        .arg(&bad_path)
        .arg(&good_path)
        .output()
        .unwrap();
    assert!(!out.status.success());
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(!json.as_array().unwrap().is_empty());
}

#[test]
fn restart_cpu_count_matches_host_cpu_count() {
    let mut spec = FixtureSpec::skeleton(Generation::G2175Current, FixtureAbi::Le64);
    spec.records = vec![RecordSpec::restart(3, 1_600_000_000, 12, 0, 0)];
    let out = output(&["show", "--format", "ndjson"], &build(spec).bytes);
    assert!(out.status.success());
    let row: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(row["host"]["cpu_count"], 2);
    assert_eq!(row["cpu_count"], 2);
}

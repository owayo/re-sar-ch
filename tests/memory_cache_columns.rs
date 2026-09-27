//! 世代で式が変わらない `A_MEMORY` の派生列 4 本 (独自出力のみ) の回帰テスト。
//!
//! `kbmemused` / `%memused` は `tlmkb − availablekb` (sysstat 12.8.0 準拠) なので、
//! `availablekb` を持たない世代 (RHEL 7 の sysstat 10.1.5 形式など) では独自出力に
//! 値が出ない。欠落を代替値で埋めない規律どおりで、これは変えない。
//! その代わりではなく、全世代にある `tlmkb` / `frmkb` / `bufkb` / `camkb` だけから作る
//! 別指標として次の 4 列を持つ。
//!
//! | 列 | 定義 |
//! |---|---|
//! | `kbmemused_nocache` | `tlmkb − frmkb − bufkb − camkb` (負なら 0) |
//! | `memused_nocache_pct` | `kbmemused_nocache / tlmkb × 100` |
//! | `kbmemfree_withcache` | `frmkb + bufkb + camkb` |
//! | `memused_withcache_pct` | `(tlmkb − frmkb) / tlmkb × 100` |
//!
//! 同梱の実採取ファイル (`testdata/sysstat-live/`) を `show --format json` で読み、
//! 4 列が同じ出力に並ぶ直接列から定義どおりに求めた値と一致すること、
//! `availablekb` を持たない世代でも値が出ること、その世代の `memused_pct` などは
//! 従来どおり値が無いことを確かめる。

use std::process::Command;

use serde_json::Value;

/// 同梱の実採取ファイルを `show --format json --activity memory` で読む。
fn show_memory(case: &str) -> Value {
    let path = format!(
        "{}/testdata/sysstat-live/2026-09-24/{case}/sa",
        env!("CARGO_MANIFEST_DIR")
    );
    let out = Command::new(env!("CARGO_BIN_EXE_resarch"))
        .args(["show", "--format", "json", "--activity", "memory", &path])
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{case}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

/// 1 item の派生値 (`rates`) から列を引く。
fn field<'a>(item: &'a Value, name: &str) -> &'a Value {
    item["rates"]
        .as_array()
        .expect("rates がある")
        .iter()
        .find(|f| f["name"] == name)
        .unwrap_or_else(|| panic!("列 {name} が出力に無い"))
}

/// 列の値。値が無い列は `value` キーごと省かれるので `None`。
fn value(item: &Value, name: &str) -> Option<f64> {
    field(item, name).get("value").and_then(Value::as_f64)
}

/// 整数で届く kB の直接列。
fn kb(item: &Value, name: &str) -> u64 {
    let v = value(item, name).unwrap_or_else(|| panic!("{name} に値が無い"));
    assert_eq!(v.fract(), 0.0, "{name} は整数");
    v as u64
}

/// 割合の列を相対誤差で比べる。
///
/// 出力の文字列は最短の往復表現だが、serde_json の既定の浮動小数パーサ
/// (`float_roundtrip` 無効) は仮数が 17 桁の値を 1 ulp ずらして読むことがある。
/// 演算そのものの一致は `series::compute` の単体テストが厳密に固定している。
fn assert_pct(actual: Option<f64>, expected: f64, label: &str) {
    let actual = actual.unwrap_or_else(|| panic!("{label} に値が無い"));
    assert!(
        (actual - expected).abs() <= expected.abs() * 1e-12,
        "{label}: {actual} != {expected}"
    );
}

/// 最後のサンプルに記録された入力と、そこから求めた 2 つの kB 列。
struct Case {
    name: &'static str,
    has_available: bool,
    total: u64,
    free: u64,
    buffers: u64,
    cached: u64,
    used_nocache: u64,
    free_withcache: u64,
}

#[test]
fn cache_derived_memory_columns_follow_one_formula_on_every_generation() {
    let cases = [
        // sysstat 10.1.5 (el7) の形式。`availablekb` を持たない
        Case {
            name: "centos-7.9",
            has_available: false,
            total: 1_127_716,
            free: 791_700,
            buffers: 19_564,
            cached: 195_872,
            // 1,127,716 - (791,700 + 19,564 + 195,872)
            used_nocache: 120_580,
            free_withcache: 1_007_136,
        },
        // 現行形式。`availablekb` を持つ
        Case {
            name: "upstream-12.8.0",
            has_available: true,
            total: 1_127_716,
            free: 352_656,
            buffers: 45_968,
            cached: 530_648,
            // 1,127,716 - (352,656 + 45,968 + 530,648)
            used_nocache: 198_444,
            free_withcache: 929_272,
        },
    ];

    for case in cases {
        let doc = show_memory(case.name);
        let items: Vec<&Value> = doc["samples"]
            .as_array()
            .expect("samples")
            .iter()
            .flat_map(|s| s["activities"].as_array().expect("activities"))
            .filter(|a| a["activity"] == "A_MEMORY")
            .flat_map(|a| a["items"].as_array().expect("items"))
            .collect();
        assert_eq!(items.len(), 2, "{}: 2 サンプル分の item", case.name);

        for item in &items {
            let total = kb(item, "kbmemtotal");
            let free = kb(item, "kbmemfree");
            let buffers = kb(item, "kbbuffers");
            let cached = kb(item, "kbcached");
            assert!(total > 0, "{}: 総量が読めている", case.name);

            // 定義どおりに、同じ出力に並ぶ直接列から求める
            let free_withcache = free + buffers + cached;
            let used_nocache = total.saturating_sub(free_withcache);
            let label = |col: &str| format!("{} の {col}", case.name);
            assert_eq!(
                value(item, "kbmemused_nocache"),
                Some(used_nocache as f64),
                "{}",
                label("kbmemused_nocache")
            );
            assert_pct(
                value(item, "memused_nocache_pct"),
                used_nocache as f64 / total as f64 * 100.0,
                &label("memused_nocache_pct"),
            );
            assert_eq!(
                value(item, "kbmemfree_withcache"),
                Some(free_withcache as f64),
                "{}",
                label("kbmemfree_withcache")
            );
            assert_pct(
                value(item, "memused_withcache_pct"),
                (total - free) as f64 / total as f64 * 100.0,
                &label("memused_withcache_pct"),
            );

            // availablekb に依存する列は、持たない世代では従来どおり値が無い (代替しない)
            for col in ["kbavail", "kbmemused", "memused_pct"] {
                if case.has_available {
                    assert!(value(item, col).is_some(), "{}", label(col));
                } else {
                    assert_eq!(value(item, col), None, "{}", label(col));
                    assert_eq!(
                        field(item, col)["quality"],
                        "unsupported_by_source",
                        "{}",
                        label(col)
                    );
                }
            }
        }

        // 最後のサンプルは採取値そのものとも突き合わせる (入力の読み違いを式の一致で隠さない)
        let last = items.last().unwrap();
        assert_eq!(
            [
                kb(last, "kbmemtotal"),
                kb(last, "kbmemfree"),
                kb(last, "kbbuffers"),
                kb(last, "kbcached"),
            ],
            [case.total, case.free, case.buffers, case.cached],
            "{}: 入力",
            case.name
        );
        assert_eq!(
            value(last, "kbmemused_nocache"),
            Some(case.used_nocache as f64),
            "{}",
            case.name
        );
        assert_eq!(
            value(last, "kbmemfree_withcache"),
            Some(case.free_withcache as f64),
            "{}",
            case.name
        );
    }
}

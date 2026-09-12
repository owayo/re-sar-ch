//! `sadf -H` (ファイルヘッダのみ) の出力。
//!
//! `sadf_misc.c: print_hdr_header()` を再現する (§5)。
//! 表示ループを持たず、ヘッダを出して終わる。
//!
//! ```text
//! System activity data file: tests/data.tmp (0x2175)
//! File created by sar/sadc from sysstat version 12.0.0
//! Genuine sa datafile: yes (0)
//! Host: Linux 5.0.16 (testhost) \t06/30/19 \t_x86_64_\t(8 CPU)
//! File date: 2019-06-30
//! File time: 05:39:21 UTC (1561873161)
//! Timezone:
//! File composition: (1,1,11),(0,0,9),(2,0,0)
//! Size of a long int: 8
//! HZ = 100
//! Number of activities in file: 36
//! Extra structures available: N
//! List of activities:
//! 01: [8b] A_CPU                Y:   9\t(10,0,0)
//! 03: [8b] A_IRQ                Y: 489\t(1,0,0) \t[Unknown format]
//! ```
//!
//! 2 行目以降は `format_magic` が現行 (`0x2175`) でない場合そこで打ち切られる。

use std::io::{self, Write};

use chrono::{Datelike, TimeZone, Utc};

use crate::format::file::SaFile;

/// 現行の `FORMAT_MAGIC`。これ以外のファイルはヘッダ 2 行で打ち切る。
pub const FORMAT_MAGIC: u16 = 0x2175;

/// `-H` の出力。
pub fn write_header<W: Write>(out: &mut W, file: &SaFile) -> io::Result<()> {
    let magic = file.magic();
    let h = file.header();

    writeln!(
        out,
        "System activity data file: {} ({:#x})",
        file.path().display(),
        magic.format_magic
    )?;
    writeln!(
        out,
        "File created by sar/sadc from sysstat version {}",
        magic.version_string()
    )?;

    // 現行 magic 以外はここで打ち切る (`print_hdr_header()` の早期 return)
    if magic.format_magic != FORMAT_MAGIC {
        return Ok(());
    }

    let upgraded = magic.upgraded.unwrap_or(0);
    writeln!(
        out,
        "Genuine sa datafile: {} ({:x})",
        if upgraded == 0 { "yes" } else { "no" },
        upgraded
    )?;

    // `Host: ` の直後は改行せず print_gal_header() の出力が続く
    write!(out, "Host: ")?;
    write_gal_header(out, file)?;

    writeln!(out, "File date: {:04}-{:02}-{:02}", h.year, h.month, h.day)?;
    let utc = Utc
        .timestamp_opt(h.ust_time as i64, 0)
        .single()
        .unwrap_or_else(|| Utc.timestamp_opt(0, 0).unwrap());
    writeln!(
        out,
        "File time: {} UTC ({})",
        utc.format("%H:%M:%S"),
        h.ust_time
    )?;
    writeln!(out, "Timezone: {}", h.tzname.clone().unwrap_or_default())?;

    let hdr = magic.hdr_types_nr.unwrap_or([0, 0, 0]);
    let act = h.act_types_nr.unwrap_or([0, 0, 0]);
    let rec = h.rec_types_nr.unwrap_or([0, 0, 0]);
    writeln!(
        out,
        "File composition: ({},{},{}),({},{},{}),({},{},{})",
        hdr[0], hdr[1], hdr[2], act[0], act[1], act[2], rec[0], rec[1], rec[2]
    )?;
    writeln!(out, "Size of a long int: {}", h.sizeof_long)?;
    // 「HZ = 」の行だけは翻訳対象外の literal
    writeln!(out, "HZ = {}", h.hz.unwrap_or(0))?;
    writeln!(out, "Number of activities in file: {}", h.act_nr)?;
    writeln!(
        out,
        "Extra structures available: {}",
        if h.extra_next.unwrap_or(0) != 0 {
            'Y'
        } else {
            'N'
        }
    )?;
    writeln!(out, "List of activities:")?;

    for e in file.activities() {
        write_activity_line(out, e)?;
    }
    Ok(())
}

/// `common.c: print_gal_header()` の `PLAIN_OUTPUT` 形式。
///
/// 書式は `"%s %s (%s) \t%s \t_%s_\t(%d CPU)\n"`。
/// **「空白 + タブ」の 2 文字ペアが 2 箇所**あるのが移植で落としやすい点 (§3.1)。
/// 日付は `LC_ALL=C` の `%x` = `MM/DD/YY`。CPU は常に単数形 `CPU`。
pub fn write_gal_header<W: Write>(out: &mut W, file: &SaFile) -> io::Result<()> {
    let h = file.header();
    let cpu = match h.cpu_nr {
        Some(n) if n > 1 => n - 1,
        _ => 1,
    };
    let local = Utc
        .timestamp_opt(h.ust_time as i64, 0)
        .single()
        .unwrap_or_else(|| Utc.timestamp_opt(0, 0).unwrap())
        .with_timezone(&chrono::Local);
    let date = format!(
        "{:02}/{:02}/{:02}",
        local.month(),
        local.day(),
        local.year() % 100
    );
    writeln!(
        out,
        "{} {} ({}) \t{} \t_{}_\t({} CPU)",
        h.sysname, h.release, h.nodename, date, h.machine, cpu
    )?;
    Ok(())
}

/// アクティビティ 1 行。
///
/// 書式は `"%02u: [%02x] %-20s %c:%4d[x%d]\t(%u,%u,%u)[ \t[Unknown format]]"`。
/// `nr2 > 1` のときだけ `x<nr2>` が付き、既知 ID で magic が現行と違う場合だけ
/// 末尾に `" \t[Unknown format]"` (空白 + タブ + 文字列) が付く。
fn write_activity_line<W: Write>(
    out: &mut W,
    e: &crate::format::file::FileActivityEntry,
) -> io::Result<()> {
    let name =
        e.id.symbol()
            .map(|s| s.to_string())
            .unwrap_or_else(|| "Unknown activity".to_string());

    let mut line = format!(
        "{:02}: [{:02x}] {:<20} {}:{:4}",
        e.id.0,
        e.magic,
        name,
        if e.has_nr { 'Y' } else { 'N' },
        e.nr
    );
    if e.nr2 > 1 {
        line.push_str(&format!("x{}", e.nr2));
    }
    let t = e.types_nr.unwrap_or([0, 0, 0]);
    line.push_str(&format!("\t({},{},{})", t[0], t[1], t[2]));

    // 本家は「**現行の** magic と食い違うか」だけを見る (`act[p]->magic != fal->magic`)。
    // 旧 magic のレイアウトを知っていても marker は付く点に注意
    // (実測: A_IRQ の 0x8b は読めるが `[Unknown format]` が付く)。
    let marker = match crate::layout::registry::lookup(e.id) {
        Some(def) => def.latest().map(|r| r.magic) != Some(e.magic),
        // 未知 ID は常に付く
        None => true,
    };
    if marker {
        line.push_str(" \t[Unknown format]");
    }

    writeln!(out, "{line}")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::file::FileActivityEntry;
    use crate::model::ActivityId;

    /// ドキュメントの実測行と一致すること (§5 の `data-12.0.0-H` 抜粋)。
    #[test]
    fn activity_line_matches_upstream_layout() {
        let e = FileActivityEntry {
            id: ActivityId::CPU,
            magic: 0x8b,
            nr: 9,
            nr2: 1,
            size: 80,
            has_nr: true,
            types_nr: Some([10, 0, 0]),
        };
        let mut buf = Vec::new();
        write_activity_line(&mut buf, &e).unwrap();
        assert_eq!(
            String::from_utf8(buf).unwrap(),
            "01: [8b] A_CPU                Y:   9\t(10,0,0)\n"
        );
    }

    /// `nr2 > 1` のときだけ `x<nr2>` が付く (`expected.sadf-H` の `A_IRQ`)。
    #[test]
    fn nr2_suffix_only_when_greater_than_one() {
        let e = FileActivityEntry {
            id: ActivityId::IRQ,
            magic: 0x8c,
            nr: 10,
            nr2: 44,
            size: 12,
            has_nr: true,
            types_nr: Some([0, 0, 1]),
        };
        let mut buf = Vec::new();
        write_activity_line(&mut buf, &e).unwrap();
        assert_eq!(
            String::from_utf8(buf).unwrap(),
            "03: [8c] A_IRQ                Y:  10x44\t(0,0,1)\n"
        );
    }

    /// **現行**の magic と食い違うと末尾に `" \t[Unknown format]"` が付く。
    ///
    /// 旧 magic のレイアウトを読めるかどうかとは無関係。実測
    /// (`expected.data-12.0.0-H`) では A_IRQ の `0x8b` にマーカーが付く
    /// (現行は `0x8c`)。
    #[test]
    fn stale_magic_appends_marker_with_space_then_tab() {
        let e = FileActivityEntry {
            id: ActivityId::IRQ,
            magic: 0x8b,
            nr: 489,
            nr2: 1,
            size: 8,
            has_nr: true,
            types_nr: Some([1, 0, 0]),
        };
        let mut buf = Vec::new();
        write_activity_line(&mut buf, &e).unwrap();
        assert_eq!(
            String::from_utf8(buf).unwrap(),
            "03: [8b] A_IRQ                Y: 489\t(1,0,0) \t[Unknown format]\n"
        );

        // 現行 magic ならマーカーは付かない
        let e = FileActivityEntry { magic: 0x8c, ..e };
        let mut buf = Vec::new();
        write_activity_line(&mut buf, &e).unwrap();
        assert!(!String::from_utf8(buf).unwrap().contains("[Unknown format]"));
    }

    /// 未知 ID は名前の位置が `Unknown activity`。
    #[test]
    fn unknown_activity_name() {
        let e = FileActivityEntry {
            id: ActivityId(200),
            magic: 0x01,
            nr: 1,
            nr2: 1,
            size: 8,
            has_nr: false,
            types_nr: Some([0, 1, 0]),
        };
        let mut buf = Vec::new();
        write_activity_line(&mut buf, &e).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.starts_with("200: [01] Unknown activity     N:   1\t(0,1,0)"));
        assert!(s.contains("[Unknown format]"));
    }
}

/// 本家の期待値ファイルとの突合。
///
/// データと期待値は GPL-2.0-or-later なので同梱できない。
/// `cargo run --bin xtask -- fetch-fixtures` で取得済みのときだけ実行する。
#[cfg(test)]
mod conformance {
    use super::*;
    use std::path::PathBuf;

    fn upstream(name: &str) -> Option<PathBuf> {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/fixtures/upstream")
            .join(name);
        p.exists().then_some(p)
    }

    /// `Host:` 行の日付はロケール依存 (`%x`) なので置き換えて比較する。
    ///
    /// 本家のテストは `TZ=GMT` で走らせているが、こちらは実行環境の TZ に従う。
    /// 日付以外のバイト列 (空白 + タブの 2 文字ペアを含む) は厳密に比較する。
    fn mask_host_date(line: &str) -> String {
        if !line.starts_with("Host: ") {
            return line.to_string();
        }
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() != 4 {
            return line.to_string();
        }
        format!("{}\t<DATE>\t{}\t{}", parts[0], parts[2], parts[3])
    }

    /// `sadf -H tests/data-12.0.0 | grep -v 0x2175` と一致すること。
    #[test]
    fn header_output_matches_upstream_expectation() {
        let (Some(data), Some(expected)) =
            (upstream("data-12.0.0"), upstream("expected.data-12.0.0-H"))
        else {
            eprintln!("fixture 未取得: スキップ");
            return;
        };
        let file = SaFile::open(&data).expect("data-12.0.0 が読めること");

        let mut buf = Vec::new();
        write_header(&mut buf, &file).unwrap();
        let got = String::from_utf8(buf).unwrap();
        // 本家のテストは 1 行目 (magic を含む) を grep -v で落としている
        let got: Vec<String> = got
            .lines()
            .filter(|l| !l.contains("0x2175"))
            .map(mask_host_date)
            .collect();

        let want: Vec<String> = std::fs::read_to_string(&expected)
            .unwrap()
            .lines()
            .map(mask_host_date)
            .collect();

        for (i, (g, w)) in got.iter().zip(want.iter()).enumerate() {
            assert_eq!(g, w, "{} 行目が不一致", i + 1);
        }
        assert_eq!(got.len(), want.len(), "行数が不一致");
    }
}

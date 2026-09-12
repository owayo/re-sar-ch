//! `sa` ファイルを走査して構造の概要を表示する開発用サンプル。
//!
//! ```sh
//! cargo run --example scan -- <FILE>
//! ```
//!
//! ファイル末尾まで余りなく読めたか (`exact`) が、レイアウト解釈が
//! 正しいことの強い証拠になる。

use re_sar_ch::format::{SaFile, ScanControl};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = args.next().ok_or("usage: scan <FILE>")?;

    let f = SaFile::open(&path)?;
    let m = f.magic();
    let h = f.header();

    println!(
        "format_magic = 0x{:04x} ({})",
        m.format_magic,
        f.spec().label
    );
    println!("sysstat      = {}", m.version_string());
    println!(
        "endian/abi   = {} / {}",
        f.encoding().endian,
        f.encoding().abi.name
    );
    println!("header_size  = {:?}", m.header_size);
    println!("hdr_types_nr = {:?}", m.hdr_types_nr);
    println!("upgraded     = {:?}", m.upgraded);
    println!(
        "date         = {:04}-{:02}-{:02}  ust_time = {}",
        h.year, h.month, h.day, h.ust_time
    );
    println!(
        "uname        = {} {} (sizeof(long)={})",
        h.sysname, h.machine, h.sizeof_long
    );
    println!("hz / cpu_nr  = {:?} / {:?}", h.hz, h.cpu_nr);
    println!(
        "act_nr       = {}  vol_act_nr = {:?}",
        h.act_nr, h.vol_act_nr
    );
    println!("tzname       = {:?}", h.tzname);
    println!("records at   = {}", f.records_offset());

    println!("\nactivities:");
    for (i, a) in f.activities().iter().enumerate() {
        println!(
            "  [{:2}] id={:3} {:14} magic=0x{:02x} nr={:5} nr2={:4} size={:5} has_nr={}",
            i,
            a.id.0,
            a.id.display_name(),
            a.magic,
            a.nr,
            a.nr2,
            a.size,
            a.has_nr
        );
    }

    for d in f.diagnostics() {
        println!("  [diag] {}", d.message);
    }

    let mut first: Option<String> = None;
    let mut last: Option<String> = None;
    let summary = f.scan(|rec| {
        let line = format!(
            "{:02}:{:02}:{:02} {} ust={} uptime_cs={:?} slices={}",
            rec.hour,
            rec.minute,
            rec.second,
            rec.kind.as_str(),
            rec.ust_time,
            rec.uptime_cs,
            rec.slices.len()
        );
        if first.is_none() {
            first = Some(line.clone());
        }
        last = Some(line);
        Ok(ScanControl::Continue)
    })?;

    println!("\nfirst record: {}", first.as_deref().unwrap_or("-"));
    println!("last  record: {}", last.as_deref().unwrap_or("-"));
    println!(
        "\nrecords: stats={} restart={} comment={} total={}",
        summary.stats,
        summary.restarts,
        summary.comments,
        summary.total_records()
    );
    println!(
        "scan: end_offset={} file_size={} trailing={} exact={}",
        summary.end_offset,
        summary.file_size,
        summary.trailing_bytes,
        summary.is_exact()
    );
    Ok(())
}

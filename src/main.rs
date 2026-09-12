//! `resarch` — sysstat の `sa` バイナリを単体で解析する CLI。
//!
//! 引数はサブコマンド (`show` / `summarize` / `compare` / `info`) と
//! `sar` / `sadf` 互換の 2 系統を受け付ける。サブコマンド名を省略した場合は
//! `sar` 互換として解釈するため、`resarch -u -f sa01` がそのまま動く。

use std::io::{self, Write};
use std::process::ExitCode;

use re_sar_ch::cli::{self, CliError, Commands, InfoArgs, Invocation, OutputFormat};
use re_sar_ch::format::{MmapPolicy, OpenOptions, SaFile, Tolerance};

fn main() -> ExitCode {
    let invocation = match cli::dispatch_from_env() {
        Ok(inv) => inv,
        // `--help` / `--version` の正常表示もここに来るので clap に任せる
        Err(CliError::Clap(e)) => e.exit(),
        Err(e) => {
            eprintln!("resarch: {e}");
            return ExitCode::from(1);
        }
    };

    match run(invocation) {
        Ok(code) => code,
        Err(e) => {
            // データは stdout、診断は stderr
            eprintln!("resarch: {e}");
            ExitCode::from(1)
        }
    }
}

fn run(invocation: Invocation) -> anyhow::Result<ExitCode> {
    match invocation {
        Invocation::Native(cmd) => match *cmd {
            Commands::Info(args) => run_info(args),
            Commands::Show(_) => not_yet("show"),
            Commands::Summarize(_) => not_yet("summarize"),
            Commands::Compare(_) => not_yet("compare"),
            Commands::Sar(_) => not_yet("sar"),
            Commands::Sadf(_) => not_yet("sadf"),
        },
        Invocation::Sar(_) => not_yet("sar 互換出力"),
        Invocation::Sadf(_) => not_yet("sadf 互換出力"),
    }
}

fn not_yet(what: &str) -> anyhow::Result<ExitCode> {
    anyhow::bail!("{what} はまだ実装されていません")
}

/// `resarch info` — ヘッダのメタデータと activity 一覧を表示する。
fn run_info(args: InfoArgs) -> anyhow::Result<ExitCode> {
    let options = OpenOptions {
        mmap: if args.no_mmap {
            MmapPolicy::Never
        } else {
            MmapPolicy::Auto
        },
        tolerance: if args.lenient {
            Tolerance::Lenient
        } else {
            Tolerance::Strict
        },
        ..Default::default()
    };

    let stdout = io::stdout();
    let mut out = io::BufWriter::new(stdout.lock());
    let mut failed = false;

    for (i, path) in args.files.iter().enumerate() {
        match SaFile::open_with(path, options.clone()) {
            Ok(file) => {
                if i > 0 {
                    writeln!(out)?;
                }
                match args.format {
                    OutputFormat::Json | OutputFormat::SadfJson => {
                        write_info_json(&mut out, &file)?
                    }
                    _ => write_info_table(&mut out, &file)?,
                }
            }
            Err(e) => {
                // 1 ファイルの失敗で全体を止めず、最後に非ゼロ終了する
                eprintln!("resarch: {e}");
                failed = true;
            }
        }
    }
    out.flush()?;

    Ok(if failed {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

fn write_info_table<W: Write>(out: &mut W, file: &SaFile) -> anyhow::Result<()> {
    let m = file.magic();
    let h = file.header();
    let enc = file.encoding();

    writeln!(out, "file          {}", file.path().display())?;
    writeln!(
        out,
        "format        0x{:04x} ({})  sysstat {}",
        m.format_magic,
        file.spec().versions,
        m.version_string()
    )?;
    writeln!(
        out,
        "encoding      {} endian, sizeof(long)={}, abi={}",
        enc.endian, h.sizeof_long, enc.abi.name
    )?;
    if let Some(up) = m.upgraded.filter(|v| *v != 0) {
        // sadf -c で変換されたファイル。値は Y*256 + Z + 1
        let y = up >> 8;
        let z = (up & 0xff).saturating_sub(1);
        writeln!(out, "upgraded      yes (変換先 x.{y}.{z} 形式)")?;
    }
    writeln!(
        out,
        "date          {:04}-{:02}-{:02}  (ust_time {})",
        h.year, h.month, h.day, h.ust_time
    )?;
    writeln!(
        out,
        "host          {} {} {}",
        h.sysname, h.release, h.machine
    )?;
    if let Some(tz) = &h.tzname {
        writeln!(out, "timezone      {tz}")?;
    }
    if let Some(hz) = h.hz {
        writeln!(out, "hz            {hz}")?;
    } else {
        writeln!(
            out,
            "hz            (記録なし。{} を仮定)",
            file.effective_hz()
        )?;
    }
    if let Some(cpu) = h.cpu_nr {
        writeln!(out, "cpu_nr        {cpu}")?;
    }
    writeln!(out, "activities    {}", h.act_nr)?;
    writeln!(out, "records at    {}", file.records_offset())?;

    writeln!(out)?;
    writeln!(
        out,
        "{:>3}  {:<14} {:>6}  {:>7} {:>5} {:>6}  {:>6}  {}",
        "id", "activity", "magic", "nr", "nr2", "size", "has_nr", "types_nr"
    )?;
    for a in file.activities() {
        let types = match a.types_nr {
            Some(t) => format!("({}, {}, {})", t[0], t[1], t[2]),
            None => "-".to_string(),
        };
        writeln!(
            out,
            "{:>3}  {:<14} 0x{:04x}  {:>7} {:>5} {:>6}  {:>6}  {}",
            a.id.0,
            a.id.display_name(),
            a.magic,
            a.nr,
            a.nr2,
            a.size,
            if a.has_nr { "yes" } else { "no" },
            types
        )?;
    }

    for d in file.diagnostics() {
        writeln!(out, "\n診断: {}", d.message)?;
    }

    Ok(())
}

fn write_info_json<W: Write>(out: &mut W, file: &SaFile) -> anyhow::Result<()> {
    let m = file.magic();
    let h = file.header();
    let enc = file.encoding();

    let activities: Vec<serde_json::Value> = file
        .activities()
        .iter()
        .map(|a| {
            serde_json::json!({
                "id": a.id.0,
                "symbol": a.id.symbol(),
                "magic": a.magic,
                "nr": a.nr,
                "nr2": a.nr2,
                "size": a.size,
                "has_nr": a.has_nr,
                "types_nr": a.types_nr,
            })
        })
        .collect();

    let value = serde_json::json!({
        "schema_version": 1,
        "file": file.path().display().to_string(),
        "format": {
            "magic": format!("0x{:04x}", m.format_magic),
            "versions": file.spec().versions,
            "sysstat_version": m.version_string(),
            "header_size": m.header_size,
            "hdr_types_nr": m.hdr_types_nr,
            "upgraded": m.upgraded,
        },
        "encoding": {
            "endian": enc.endian.as_str(),
            "sizeof_long": h.sizeof_long,
            "abi": enc.abi.name,
        },
        "header": {
            "ust_time": h.ust_time,
            "date": format!("{:04}-{:02}-{:02}", h.year, h.month, h.day),
            "sysname": h.sysname,
            "release": h.release,
            "machine": h.machine,
            "nodename": h.nodename,
            "timezone": h.tzname,
            "hz": h.hz,
            "effective_hz": file.effective_hz(),
            "cpu_nr": h.cpu_nr,
            "act_nr": h.act_nr,
            "vol_act_nr": h.vol_act_nr,
        },
        "records_offset": file.records_offset(),
        "activities": activities,
        "diagnostics": file.diagnostics().iter().map(|d| d.message.clone()).collect::<Vec<_>>(),
    });

    serde_json::to_writer_pretty(&mut *out, &value)?;
    writeln!(out)?;
    Ok(())
}

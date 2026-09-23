//! 旧モノリシック形式。実測配置とレコード順序を分けて扱う。
use std::path::Path;

use super::abi::{Endian, LayoutAbi, SourceEncoding};
use super::file::{
    ActivitySlice, Diagnostic, FileActivityEntry, FileHeader, FileMagic, OpenOptions, RawRecord,
    ScanControl, ScanSummary, Tolerance,
};
use super::legacy_layouts::{self, LegacyLayout, LegacyStruct};
use super::registry::{FormatSpec, RecordKind};
use super::wire::FieldTy;
use crate::error::{Error, Result};
use crate::model::ActivityId as A;

#[derive(Debug)]
pub(crate) struct LegacyFile {
    pub encoding: SourceEncoding,
    pub magic: FileMagic,
    pub header: FileHeader,
    pub activities: Vec<FileActivityEntry>,
    pub diagnostics: Vec<Diagnostic>,
    pub layout: &'static LegacyLayout,
    pub flags: u32,
    pub fixed_size: usize,
    pub cpu_items: u32,
    pub one_irq_offset: Option<usize>,
    /// Linux 2.4 以降の paging=kB / I/O block=512B と確認できるか。
    pub modern_kernel_units: bool,
    slices: Vec<ActivitySlice>,
    record_size: usize,
}

impl LegacyFile {
    pub fn open(
        bytes: &[u8],
        path: &Path,
        spec: &FormatSpec,
        endian: Endian,
        options: &OpenOptions,
    ) -> Result<Self> {
        let layout = legacy_layouts::lookup(spec.magic).expect("readable legacy layout");
        let header_end = layout.prefix + layout.header.size;
        let bad = |detail: String| Error::InconsistentHeader {
            path: path.to_owned(),
            detail,
        };
        if bytes.len() < header_end {
            return Err(Error::Truncated {
                path: path.to_owned(),
                context: "legacy file header".into(),
                need: header_end,
                have: bytes.len(),
            });
        }
        let header_bytes = &bytes[layout.prefix..header_end];
        let long = layout
            .header
            .field("sa_sizeof_long")
            .map_or(options.legacy_long_bytes, |f| header_bytes[f.offset[0]]);
        let abi = match long {
            4 => LayoutAbi::I386,
            8 => LayoutAbi::LP64,
            _ => {
                return Err(bad(format!(
                    "legacy sizeof(long)={long}; 4 または 8 が必要"
                )));
            }
        };
        let encoding = SourceEncoding::new(endian, abi);
        let read = |name| read_field(header_bytes, layout.header, name, &encoding).unwrap_or(0);
        let flags = read("sa_actflag") as u32;
        let known_flags = layout.flags.iter().fold(0, |bits, (_, flag)| bits | flag);
        if flags & !known_flags != 0 {
            return Err(bad(format!("未知の legacy activity flags: 0x{flags:x}")));
        }
        let count = |name, limit| -> Result<u32> {
            let n = read(name);
            if n > u64::from(limit) {
                return Err(bad(format!("legacy count {name}={n} > {limit}")));
            }
            Ok(n as u32)
        };
        let last_index = layout.cpu_count_is_last_index();
        let proc = count("sa_proc", 8192 - u32::from(last_index))?;
        let cpu_items = if last_index && proc > 0 {
            proc + 1
        } else {
            proc
        };
        let serial = count("sa_serial", 4096)?;
        let iface = count("sa_iface", 65536)?;
        let irqcpu = count("sa_irqcpu", 4096)?;
        let disks = count("sa_nr_disk", 65536)?;
        let pids = count("sa_nr_pid", 256)?;
        if pids != 0 {
            return Err(bad("PID 統計を含む legacy ストリームは未対応".into()));
        }
        let fixed_size = read("sa_st_size") as usize;
        if fixed_size != layout.stats.size {
            return Err(bad(format!(
                "legacy record size {fixed_size} != {}",
                layout.stats.size
            )));
        }
        let mut offset = fixed_size;
        let mut region = |nr: u32, stride: usize| -> Result<usize> {
            let start = offset;
            let len = (nr as usize)
                .checked_mul(stride)
                .ok_or_else(|| bad("legacy record size overflow".into()))?;
            offset = offset
                .checked_add(len)
                .filter(|&n| n <= 64 * 1024 * 1024)
                .ok_or_else(|| bad("legacy record exceeds 64 MiB".into()))?;
            Ok(start)
        };
        region(cpu_items, layout.cpu.size)?;
        let one_irq_offset = if flags & layout.flag("A_ONE_IRQ") != 0 {
            Some(region(layout.irq_count, 4)?)
        } else {
            None
        };
        let serial_at = region(serial, layout.serial.size)?;
        region(
            (if last_index { proc + 1 } else { proc })
                .checked_mul(irqcpu)
                .ok_or_else(|| bad("legacy IRQ count overflow".into()))?,
            layout.irqcpu.size,
        )?;
        let net_at = region(iface, layout.net.size)?;
        let disk_nr = if !layout.disk_requires_flag() || flags & layout.flag("A_DISK") != 0 {
            disks
        } else {
            0
        };
        let disk_at = region(disk_nr, layout.disk.size)?;
        let record_size = offset;
        let string = |name| {
            read_string(header_bytes, layout.header, name, &encoding)
                .unwrap_or_else(|| "unknown".into())
        };
        let year = 1900 + read("sa_year") as i32;
        let month = read("sa_month") as u8;
        let day = read("sa_day") as u8;
        let date = chrono::NaiveDate::from_ymd_opt(year, u32::from(month) + 1, u32::from(day))
            .ok_or_else(|| bad("legacy header の日付が範囲外".into()))?;
        let month = month + 1;
        let header = FileHeader {
            ust_time: if layout.header.field("sa_ust_time").is_some() {
                read("sa_ust_time")
            } else {
                date.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp() as u64
            },
            hz: None,
            cpu_nr: Some(cpu_items.max(1) + 1),
            act_nr: 0,
            vol_act_nr: None,
            year,
            month,
            day,
            sizeof_long: long,
            sysname: string("sa_sysname"),
            nodename: string("sa_nodename"),
            release: string("sa_release"),
            machine: string("sa_machine"),
            tzname: None,
            act_types_nr: None,
            rec_types_nr: None,
            act_size: None,
            rec_size: Some(fixed_size as u32),
            extra_next: None,
        };
        let mut release = header.release.split('.');
        let modern_kernel_units = release
            .next()
            .and_then(|v| v.parse::<u32>().ok())
            .zip(release.next().and_then(|v| v.parse::<u32>().ok()))
            .is_some_and(|version| version >= (2, 4));
        let mut result = Self {
            encoding,
            magic: FileMagic {
                format_magic: spec.magic,
                version: if layout.prefix == 8 {
                    (bytes[4], bytes[5], bytes[6], bytes[7])
                } else {
                    (0, 0, 0, 0)
                },
                header_size: Some(layout.header.size as u32),
                upgraded: None,
                hdr_types_nr: None,
            },
            header,
            activities: Vec::new(),
            diagnostics: vec![Diagnostic {
                offset: None,
                message: format!(
                    "legacy 形式で記録されていない版・machine は不明、HZ={} と仮定",
                    options.assumed_hz
                ),
            }],
            layout,
            flags,
            fixed_size,
            cpu_items,
            one_irq_offset,
            modern_kernel_units,
            slices: Vec::new(),
            record_size,
        };
        if layout.header.field("sa_sizeof_long").is_none() {
            result.diagnostics.push(Diagnostic {offset: None, message: format!("0x{:04x} は long 幅を記録しないため {long} バイトと仮定 (OpenOptions.legacy_long_bytes)", layout.magic)});
        }
        if !modern_kernel_units {
            result.diagnostics.push(Diagnostic { offset: None, message: "Linux 2.4 以降と確認できないため、旧 PAGE の pgpgin/out と IO の読み書きブロック数は単位不明として取得不可".into() });
        }
        if layout.stats.field("ust_time").is_none() {
            result.diagnostics.push(Diagnostic { offset: None, message: "この旧形式は epoch とタイムゾーンを記録しないため、ヘッダの日付と各レコードの時計を UTC と仮定して復元".into() });
        }
        if disk_nr > 0 && layout.disk.field("rd_sect").is_none() {
            result.diagnostics.push(Diagnostic { offset: None, message: "旧ディスク配列は 読み書き別のセクタ数を持たないため、境界だけを検証して読み飛ばす".into() });
        }
        if irqcpu > 0 {
            result.diagnostics.push(Diagnostic { offset: None, message: "CPU 別 IRQ の旧配列は境界を検証して読み飛ばす (IRQ 総数・個別 IRQ 総数は読み取り可能)".into() });
        }
        let mut add = |id, flag, nr, stride, offset| {
            if flags & flag == 0 || nr == 0 {
                return;
            }
            let index = result.activities.len();
            result.activities.push(FileActivityEntry {
                id,
                magic: 0,
                nr: nr as i32,
                nr2: 1,
                size: stride as u32,
                has_nr: false,
                types_nr: None,
            });
            result.slices.push(ActivitySlice {
                index,
                id,
                nr,
                nr2: 1,
                stride,
                offset,
                len: if id == A::CPU || id == A::IRQ {
                    record_size
                } else {
                    nr as usize * stride
                },
            });
        };
        add(A::CPU, layout.flag("A_CPU"), cpu_items + 1, fixed_size, 0);
        add(
            A::PCSW,
            layout.flag("A_PROC") | layout.flag("A_CTXSW"),
            1,
            fixed_size,
            0,
        );
        add(
            A::IRQ,
            layout.flag("A_IRQ") | layout.flag("A_ONE_IRQ"),
            if one_irq_offset.is_some() {
                layout.irq_count + 1
            } else {
                1
            },
            fixed_size,
            0,
        );
        for (id, flag) in [
            (A::SWAP, layout.flag("A_SWAP")),
            (A::PAGE, layout.flag("A_PAGE")),
            (A::IO, layout.flag("A_IO")),
            (
                A::MEMORY,
                layout.flag("A_MEMORY") | layout.flag("A_MEM_AMT"),
            ),
            (A::KTABLES, layout.flag("A_KTABLES")),
            (A::QUEUE, layout.flag("A_QUEUE")),
            (A::NET_SOCK, layout.flag("A_NET_SOCK")),
            (A::NET_NFS, layout.flag("A_NET_NFS")),
            (A::NET_NFSD, layout.flag("A_NET_NFSD")),
        ] {
            add(id, flag, 1, fixed_size, 0);
        }
        add(
            A::SERIAL,
            layout.flag("A_SERIAL"),
            serial,
            layout.serial.size,
            serial_at,
        );
        add(
            A::NET_DEV,
            layout.flag("A_NET_DEV"),
            iface,
            layout.net.size,
            net_at,
        );
        add(
            A::NET_EDEV,
            layout.flag("A_NET_EDEV"),
            iface,
            layout.net.size,
            net_at,
        );
        if layout.disk.field("rd_sect").is_some() {
            add(
                A::DISK,
                layout.flag("A_DISK"),
                disk_nr,
                layout.disk.size,
                disk_at,
            );
        }
        result.header.act_nr = result.activities.len() as u32;
        Ok(result)
    }

    pub fn scan<F>(
        &self,
        bytes: &[u8],
        path: &Path,
        options: &OpenOptions,
        mut visit: F,
    ) -> Result<ScanSummary>
    where
        F: FnMut(&RawRecord<'_>) -> Result<ScanControl>,
    {
        let mut summary = ScanSummary {
            file_size: bytes.len(),
            end_offset: self.layout.prefix + self.layout.header.size,
            ..Default::default()
        };
        let mut slices = self.slices.clone();
        let mut offset = summary.end_offset;
        let mut clock_date = chrono::NaiveDate::from_ymd_opt(
            self.header.year,
            self.header.month.into(),
            self.header.day.into(),
        )
        .expect("validated date");
        let mut last_clock = None;
        while offset < bytes.len() {
            let remaining = bytes.len() - offset;
            let type_at = self
                .layout
                .stats
                .field("record_type")
                .expect("record type")
                .at(&self.encoding);
            let need = if remaining >= self.fixed_size && !matches!(bytes[offset + type_at], 2 | 4)
            {
                self.record_size
            } else {
                self.fixed_size
            };
            if remaining < need {
                if options.tolerance == Tolerance::Strict {
                    return Err(Error::Truncated {
                        path: path.to_owned(),
                        context: "legacy record".into(),
                        need,
                        have: remaining,
                    });
                }
                summary.incomplete = true;
                summary.trailing_bytes = remaining;
                break;
            }
            let fixed = &bytes[offset..offset + self.fixed_size];
            let read =
                |name| read_field(fixed, self.layout.stats, name, &self.encoding).unwrap_or(0);
            let kind = match bytes[offset + type_at] {
                1 => RecordKind::Stats,
                2 => RecordKind::Restart,
                3 if self.layout.magic >= 0x2168 => RecordKind::LastStats,
                4 if self.layout.comment.size > 0 => RecordKind::Comment,
                n => {
                    return Err(Error::RecordBoundaryLost {
                        path: path.to_owned(),
                        offset: offset as u64,
                        detail: format!("legacy record_type={n}"),
                    });
                }
            };
            let hour = read("hour") as u8;
            let minute = read("minute") as u8;
            let second = read("second") as u8;
            if hour > 23 || minute > 59 || second > 60 {
                return Err(Error::RecordBoundaryLost {
                    path: path.to_owned(),
                    offset: offset as u64,
                    detail: "legacy record の時刻が範囲外".into(),
                });
            }
            let ust_time = if self.layout.stats.field("ust_time").is_some() {
                read("ust_time")
            } else {
                let clock = u32::from(hour) * 3600 + u32::from(minute) * 60 + u32::from(second);
                if last_clock.is_some_and(|last| clock < last) {
                    clock_date = clock_date.succ_opt().expect("legacy year fits date range");
                }
                last_clock = Some(clock);
                (clock_date
                    .and_hms_opt(0, 0, 0)
                    .unwrap()
                    .and_utc()
                    .timestamp()
                    + i64::from(clock)) as u64
            };
            let up = read("uptime");
            let up0 = if self.layout.stats.field("uptime0").is_none() {
                up / u64::from(self.cpu_items.max(1))
            } else if self.cpu_items == 0 && self.layout.magic <= 0x216c {
                up
            } else {
                read("uptime0")
            };
            for (dst, src) in slices.iter_mut().zip(&self.slices) {
                dst.offset = offset + src.offset;
            }
            let rec = RawRecord {
                kind,
                offset,
                uptime_cs: up0
                    .checked_mul(100)
                    .and_then(|v| v.checked_div(options.assumed_hz)),
                uptime_jiffies: Some((up, up0)),
                ust_time,
                hour,
                minute,
                second,
                slices: if kind.carries_stats() { &slices } else { &[] },
                comment: if kind == RecordKind::Comment {
                    let field = self.layout.comment.field("comment").expect("comment field");
                    let at = field.at(&self.encoding);
                    Some(&fixed[at..at + 64])
                } else {
                    None
                },
                cpu_count: None,
            };
            if kind.carries_stats() {
                summary.stats += 1;
            } else if kind == RecordKind::Restart {
                summary.restarts += 1;
            } else {
                summary.comments += 1;
            }
            offset += need;
            summary.end_offset = offset;
            if visit(&rec)? == ScanControl::Stop {
                summary.stopped_early = true;
                break;
            }
        }
        Ok(summary)
    }
}

fn read_number(bytes: &[u8], at: usize, width: usize, endian: Endian) -> u64 {
    let mut value = 0u64;
    for i in 0..width {
        let shift = if endian == Endian::Little {
            i
        } else {
            width - 1 - i
        };
        value |= u64::from(bytes[at + i]) << (shift * 8);
    }
    value
}

fn read_field(bytes: &[u8], layout: LegacyStruct, name: &str, enc: &SourceEncoding) -> Option<u64> {
    let f = layout.field(name)?;
    let width = match f.ty {
        FieldTy::U8 => 1,
        FieldTy::U16 => 2,
        FieldTy::U32 => 4,
        FieldTy::U64 => 8,
        FieldTy::CULong => enc.abi.long_bytes as usize,
        _ => return None,
    };
    Some(read_number(bytes, f.at(enc), width, enc.endian))
}
fn read_string(
    bytes: &[u8],
    layout: LegacyStruct,
    name: &str,
    enc: &SourceEncoding,
) -> Option<String> {
    let field = layout.field(name)?;
    let FieldTy::Bytes(len) = field.ty else {
        return None;
    };
    let at = field.at(enc);
    let data = &bytes[at..at + usize::from(len)];
    Some(String::from_utf8_lossy(data.split(|&b| b == 0).next().unwrap_or_default()).into_owned())
}

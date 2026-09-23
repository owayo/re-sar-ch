//! sysstat 2.2 の選択列を詰めた形式。構造体形式とは別の境界規則を持つ。
use std::path::Path;

use super::abi::{Endian, LayoutAbi, SourceEncoding};
use super::file::{
    ActivitySlice, Diagnostic, FileActivityEntry, FileHeader, FileMagic, OpenOptions, RawRecord,
    ScanControl, ScanSummary, Tolerance,
};
use super::registry::{FormatSpec, RecordKind};
use super::wire::FieldTy;
use crate::error::{Error, Result};
use crate::model::ActivityId as A;

const HEADER_SIZE: usize = 280;

#[derive(Debug)]
pub(crate) struct PackedLegacyFile {
    pub encoding: SourceEncoding,
    pub magic: FileMagic,
    pub header: FileHeader,
    pub activities: Vec<FileActivityEntry>,
    pub diagnostics: Vec<Diagnostic>,
    pub fields: Vec<(A, &'static str, FieldTy, usize)>,
    record_size: usize,
    interval: u64,
    slices: Vec<ActivitySlice>,
}

impl PackedLegacyFile {
    pub fn open(
        bytes: &[u8],
        path: &Path,
        spec: &FormatSpec,
        endian: Endian,
        options: &OpenOptions,
    ) -> Result<Self> {
        let bad = |detail: String| Error::InconsistentHeader {
            path: path.to_owned(),
            detail,
        };
        if bytes.len() < HEADER_SIZE {
            return Err(Error::Truncated {
                path: path.to_owned(),
                context: "packed legacy file header".into(),
                need: HEADER_SIZE,
                have: bytes.len(),
            });
        }
        let little = |at| u16::from_le_bytes([bytes[at], bytes[at + 1]]);
        if little(0) != 0x015d || spec.magic != 0x015d {
            return Err(bad("packed legacy magic が一致しない".into()));
        }
        let long = bytes[279];
        let abi = match long {
            4 => LayoutAbi::I386,
            8 => LayoutAbi::LP64,
            _ => {
                return Err(bad(format!(
                    "packed legacy sizeof(long)={long}; 4 または 8 が必要"
                )));
            }
        };
        let encoding = SourceEncoding::new(endian, abi);
        let interval_bytes = &bytes[76..76 + long as usize];
        let interval = interval_bytes.iter().enumerate().fold(0u64, |n, (i, &b)| {
            let shift = if endian == Endian::Little {
                i
            } else {
                long as usize - 1 - i
            };
            n | (u64::from(b) << (shift * 8))
        });
        if interval == 0 || interval > i64::MAX as u64 {
            return Err(bad("packed legacy 採取間隔が正の秒数ではない".into()));
        }
        let flags = little(8);
        if flags == 0 || flags & !0x03ff != 0 {
            return Err(bad(format!(
                "未知または空の packed legacy flags: 0x{flags:x}"
            )));
        }
        // 全選択時は本家が上限の次の格納ワードにも bit を立てる。
        // レコードへ書かれる CPU < 32 / IRQ < 224 の範囲だけを数える。
        let count_bits = |range: std::ops::Range<usize>| {
            bytes[range]
                .iter()
                .map(|b| b.count_ones() as usize)
                .sum::<usize>()
        };
        let cpu_items = if flags & 0x80 != 0 {
            count_bits(10..14)
        } else {
            0
        };
        let irq_items = if flags & 0x100 != 0 {
            count_bits(42..70)
        } else {
            0
        };
        let long_ty = if long == 4 {
            FieldTy::U32
        } else {
            FieldTy::U64
        };
        let mut fields = Vec::new();
        let mut offset = 0;
        let mut column = |id, flag, name, ty: FieldTy| {
            if flags & flag != 0 {
                fields.push((id, name, ty, offset));
                offset += ty.width(&abi);
            }
        };
        column(A::PCSW, 1, "processes", long_ty);
        column(A::PCSW, 2, "context_switch", FieldTy::U32);
        for name in ["cpu_user", "cpu_nice", "cpu_sys"] {
            column(A::CPU, 4, name, FieldTy::U32);
        }
        column(A::CPU, 4, "cpu_idle", long_ty);
        column(A::IRQ, 8, "irq_nr", FieldTy::U32);
        for name in ["pgpgin", "pgpgout"] {
            column(A::PAGE, 0x10, name, FieldTy::U32);
        }
        for name in ["pswpin", "pswpout"] {
            column(A::SWAP, 0x20, name, FieldTy::U32);
        }
        for name in [
            "dk_drive",
            "dk_drive_rio",
            "dk_drive_wio",
            "dk_drive_rblk",
            "dk_drive_wblk",
        ] {
            column(A::IO, 0x40, name, FieldTy::U32);
        }
        offset += cpu_items * (12 + long as usize) + irq_items * 4;
        if flags & 0x200 != 0 {
            offset += 4 * long as usize;
        }
        let record_size = little(74) as usize;
        if record_size == 0 || record_size != offset {
            return Err(bad(format!(
                "packed legacy record size {record_size} != {offset}"
            )));
        }
        let year = 1900 + i32::from(bytes[4]);
        let month = u32::from(bytes[3]) + 1;
        let day = u32::from(bytes[2]);
        let epoch = chrono::NaiveDate::from_ymd_opt(year, month, day)
            .and_then(|d| d.and_hms_opt(bytes[5].into(), bytes[6].into(), bytes[7].into()))
            .map(|d| d.and_utc().timestamp())
            .and_then(|t| u64::try_from(t).ok())
            .ok_or_else(|| bad("packed legacy の開始日時が範囲外".into()))?;
        let string = |at| {
            String::from_utf8_lossy(
                bytes[at..at + 65]
                    .split(|&b| b == 0)
                    .next()
                    .unwrap_or_default(),
            )
            .into_owned()
        };
        let mut activities = Vec::new();
        for (id, mask) in [
            (A::CPU, 4),
            (A::PCSW, 3),
            (A::IRQ, 8),
            (A::PAGE, 0x10),
            (A::SWAP, 0x20),
            (A::IO, 0x40),
            (A::MEMORY, 0x200),
        ] {
            if flags & mask != 0 {
                activities.push(FileActivityEntry {
                    id,
                    magic: 0,
                    nr: 1,
                    nr2: 1,
                    size: record_size as u32,
                    has_nr: false,
                    types_nr: None,
                });
            }
        }
        let slices = activities
            .iter()
            .enumerate()
            .map(|(index, act)| ActivitySlice {
                index,
                id: act.id,
                nr: 1,
                nr2: 1,
                stride: record_size,
                offset: 0,
                len: record_size,
            })
            .collect();
        let header = FileHeader {
            ust_time: epoch,
            hz: None,
            cpu_nr: None,
            act_nr: activities.len() as u32,
            vol_act_nr: None,
            year,
            month: month as u8,
            day: day as u8,
            sizeof_long: long,
            sysname: string(84),
            release: string(149),
            nodename: string(214),
            machine: "unknown".into(),
            tzname: None,
            act_types_nr: None,
            rec_types_nr: None,
            act_size: None,
            rec_size: Some(record_size as u32),
            extra_next: None,
        };
        let endian_basis = if options.legacy_endian.is_some() {
            "OpenOptions.legacy_endian の明示指定"
        } else {
            "未記録のため仮定"
        };
        let mut diagnostics = vec![
            Diagnostic { offset: None, message: "packed legacy はレコード時刻・uptime・タイムゾーンを持たないため、開始日時を UTC と仮定し採取間隔から時刻・相対 uptime を復元".into() },
            Diagnostic { offset: None, message: format!("packed legacy の数値列は {endian:?} ({endian_basis})。版・machine・実 CPU 数は不明") },
        ];
        if cpu_items > 0 || irq_items > 0 {
            diagnostics.push(Diagnostic { offset: None, message: "選択された CPU / IRQ の配列は疎な番号を保持する必要があるため、現在は境界だけを検証して読み飛ばす".into() });
        }
        if flags & 0x200 != 0 {
            diagnostics.push(Diagnostic { offset: None, message: "packed legacy のメモリ値はページ数でページサイズが不明なため、kB 指標は UnsupportedBySource".into() });
        }
        if flags & 0x50 != 0 {
            diagnostics.push(Diagnostic {
                offset: None,
                message: "packed legacy の paging / I/O 転送量は生成カーネルによって単位が異なるため UnsupportedBySource (I/O 回数は読み取り可能)".into(),
            });
        }
        Ok(Self {
            encoding,
            magic: FileMagic {
                format_magic: spec.magic,
                version: (0, 0, 0, 0),
                header_size: Some(HEADER_SIZE as u32),
                upgraded: None,
                hdr_types_nr: None,
            },
            header,
            activities,
            diagnostics,
            fields,
            record_size,
            interval,
            slices,
        })
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
            end_offset: HEADER_SIZE,
            ..Default::default()
        };
        let mut slices = self.slices.clone();
        while summary.end_offset < bytes.len() {
            let offset = summary.end_offset;
            let remaining = bytes.len() - offset;
            if remaining < self.record_size {
                if options.tolerance == Tolerance::Strict {
                    return Err(Error::Truncated {
                        path: path.to_owned(),
                        context: "packed legacy record".into(),
                        need: self.record_size,
                        have: remaining,
                    });
                }
                summary.incomplete = true;
                summary.trailing_bytes = remaining;
                break;
            }
            let bad_time = || Error::RecordBoundaryLost {
                path: path.to_owned(),
                offset: offset as u64,
                detail: "packed legacy の復元時刻が範囲外".into(),
            };
            let elapsed = summary
                .stats
                .checked_mul(self.interval)
                .ok_or_else(bad_time)?;
            let epoch = self
                .header
                .ust_time
                .checked_add(elapsed)
                .ok_or_else(bad_time)?;
            let time = i64::try_from(epoch)
                .ok()
                .and_then(|t| chrono::DateTime::from_timestamp(t, 0))
                .ok_or_else(bad_time)?;
            let uptime_cs = elapsed.checked_mul(100).ok_or_else(bad_time)?;
            for slice in &mut slices {
                slice.offset = offset;
            }
            use chrono::Timelike;
            let rec = RawRecord {
                kind: RecordKind::Stats,
                offset,
                uptime_cs: Some(uptime_cs),
                uptime_jiffies: None,
                ust_time: epoch,
                hour: time.hour() as u8,
                minute: time.minute() as u8,
                second: time.second() as u8,
                slices: &slices,
                comment: None,
                cpu_count: None,
            };
            summary.stats += 1;
            summary.end_offset += self.record_size;
            if visit(&rec)? == ScanControl::Stop {
                summary.stopped_early = true;
                break;
            }
        }
        Ok(summary)
    }
}

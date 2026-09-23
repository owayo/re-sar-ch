//! 旧形式の実測フィールドを現在のドメインへ直接対応付ける。
use super::{
    plan::{DecodePlan, UnusedItem},
    registry::ActivityDef,
};
use crate::format::{
    abi::SourceEncoding, file::FileActivityEntry, legacy::LegacyFile, legacy_layouts::LegacyStruct,
    wire::FieldTy,
};
use crate::model::ActivityId as A;

pub(crate) fn plan(
    file: &LegacyFile,
    act: &FileActivityEntry,
    def: &ActivityDef,
    enc: &SourceEncoding,
) -> crate::error::Result<DecodePlan> {
    let layout = file.layout;
    let source = match act.id {
        A::SERIAL => layout.serial,
        A::NET_DEV | A::NET_EDEV => layout.net,
        A::DISK => layout.disk,
        _ => layout.stats,
    };
    let revision = if act.id == A::IRQ {
        def.revisions
            .iter()
            .find(|rev| !rev.layout.has_field("irq_name"))
            .expect("unnamed IRQ schema")
    } else {
        def.latest().expect("registered legacy activity")
    };
    let positions = |source: LegacyStruct, per_cpu: bool| {
        revision
            .layout
            .fields
            .iter()
            .filter_map(|target| {
                let name = target.name;
                if !file.modern_kernel_units
                    && matches!(
                        (act.id, name),
                        (A::PAGE, "pgpgin" | "pgpgout")
                            | (A::IO, "dk_drive_rblk" | "dk_drive_wblk")
                    )
                {
                    return None;
                }
                let source_name = match (act.id, name, per_cpu) {
                    (A::CPU, "cpu_sys", false) => "cpu_system",
                    (A::CPU, "cpu_sys", true) => "per_cpu_system",
                    (A::CPU, "cpu_user", true) => "per_cpu_user",
                    (A::CPU, "cpu_nice", true) => "per_cpu_nice",
                    (A::CPU, "cpu_idle", true) => "per_cpu_idle",
                    (A::CPU, "cpu_iowait", true) => "per_cpu_iowait",
                    (A::CPU, "cpu_steal", true) => "per_cpu_steal",
                    (A::PCSW, "context_switch", _) => "context_swtch",
                    (A::IRQ, "irq_nr", _) => "irq_sum",
                    (A::DISK, "minor", _) if source.field("minor").is_none() => "index",
                    (A::DISK, "nr_ios", _) if source.field("nr_ios").is_none() => "dk_drive",
                    (A::DISK, "rd_sect", _) if source.field("rd_sect").is_none() => "dk_drive_rblk",
                    (A::DISK, "wr_sect", _) if source.field("wr_sect").is_none() => "dk_drive_wblk",
                    _ => name,
                };
                let flag = match (act.id, name) {
                    (A::PCSW, "processes") => "A_PROC",
                    (A::PCSW, "context_switch") => "A_CTXSW",
                    (A::IRQ, "irq_nr") => "A_IRQ",
                    _ => "",
                };
                if !flag.is_empty() && file.flags & layout.flag(flag) == 0 {
                    return None;
                }
                source
                    .field(source_name)
                    .map(|field| (name, field.ty, field.at(enc)))
            })
            .collect::<Vec<_>>()
    };
    let mut result = DecodePlan::from_positions(
        def,
        revision,
        act.size as usize,
        act.nr as u32,
        &positions(source, false),
        enc,
    )?;
    if act.id == A::CPU && file.cpu_items > 0 {
        let following = DecodePlan::from_positions(
            def,
            revision,
            layout.cpu.size,
            file.cpu_items,
            &positions(layout.cpu, true),
            enc,
        )?;
        result.following = Some((file.fixed_size, Box::new(following)));
    }
    if act.id == A::IRQ
        && let Some(at) = file.one_irq_offset
    {
        let following = DecodePlan::from_positions(
            def,
            revision,
            4,
            layout.irq_count,
            &[("irq_nr", FieldTy::U32, 0)],
            enc,
        )?;
        result.following = Some((at, Box::new(following)));
    }
    result.serial_line_offset = false;
    result.unused = match act.id {
        A::SERIAL => Some(UnusedItem::Column {
            index: 0,
            sentinel: if source.field("line").is_some_and(|f| f.ty == FieldTy::U8) {
                u8::MAX as u64
            } else {
                u32::MAX as u64
            },
        }),
        A::NET_DEV | A::NET_EDEV => Some(UnusedItem::Key("?")),
        _ => None,
    };
    Ok(result)
}

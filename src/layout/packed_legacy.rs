//! 可変列形式のフィールドを直接デコードする。単位不明のページ数は kB に見せない。
use super::{plan::DecodePlan, registry::ActivityDef};
use crate::format::{
    abi::SourceEncoding, file::FileActivityEntry, packed_legacy::PackedLegacyFile,
};
use crate::model::ActivityId as A;

pub(crate) fn plan(
    file: &PackedLegacyFile,
    act: &FileActivityEntry,
    def: &ActivityDef,
    enc: &SourceEncoding,
) -> crate::error::Result<DecodePlan> {
    let revision = if act.id == A::IRQ {
        def.revisions
            .iter()
            .find(|rev| !rev.layout.has_field("irq_name"))
            .expect("unnamed IRQ schema")
    } else {
        def.latest().expect("registered packed legacy activity")
    };
    let positions = file
        .fields
        .iter()
        .filter(|(id, _, _, _)| *id == act.id)
        // paging / block の単位は当時のカーネルに依存する。現行単位と混同しない。
        .filter(|(_, name, _, _)| {
            !matches!(
                *name,
                "pgpgin" | "pgpgout" | "dk_drive_rblk" | "dk_drive_wblk"
            )
        })
        .map(|&(_, name, ty, at)| (name, ty, at))
        .collect::<Vec<_>>();
    Ok(DecodePlan::from_positions(
        def,
        revision,
        act.size as usize,
        1,
        &positions,
        enc,
    )?)
}

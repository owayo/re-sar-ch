//! 形式をまたいで共有する値の取り出しと書式化。
//!
//! 数値は [`crate::series::compute`] の結果を書式化するだけ。
//! 文字列 (デバイス名・バッテリ状態) とアイテム識別子の作り方だけは
//! `sadf` 固有の規則があるのでここに置く。

use super::access::ItemPair;
use super::spec::{ActivitySpec, Field, Fmt, ItemKind, RawStyle, Section};
use super::{ItemLabel, Unavailable};
use crate::model::{ActivityId, Availability};
use crate::series::compute::{ComputeIssue, Computed};

/// バッテリ状態の名前 (`raw_stats.c` / `json_stats.c` の `bat_status[]`)。
///
/// 範囲外は `UNDEFINED`。
pub fn bat_status(v: u64) -> &'static str {
    use crate::series::compute::bat_status as sts;
    match v {
        sts::UNKNOWN => "Unknown",
        sts::CHARGING => "Charging",
        sts::DISCHARGING => "Discharging",
        sts::NOTCHARGING => "NotCharging",
        sts::FULL => "Full",
        _ => "UNDEFINED",
    }
}

/// アイテム識別子を組み立てる。
pub fn item_label(spec: &ActivitySpec, item: &ItemPair<'_>) -> ItemLabel {
    match spec.item {
        ItemKind::None | ItemKind::Irq => ItemLabel::none(),
        ItemKind::Cpu => ItemLabel::cpu(item.index),
        ItemKind::Name => match item.key() {
            Some(k) => ItemLabel::named(k),
            // 名前を持たない item は位置で表す (0 で埋めない)
            None => ItemLabel::numbered("", item.index as u64),
        },
        ItemKind::Index { prefix, base } => {
            ItemLabel::numbered(prefix, item.index as u64 + base as u64)
        }
        ItemKind::Column { col, prefix } => match item.raw_curr_by_name(col) {
            Availability::Present(v) => ItemLabel::numbered(prefix, v),
            _ => ItemLabel::numbered(prefix, item.index as u64),
        },
        // デバイス名はファイルに入っていないので major/minor から作る
        // (本家 `get_devname()` のフォールバックと同じ `dev<major>-<minor>`)。
        ItemKind::Disk => {
            let major = item.raw_curr_by_name("major");
            let minor = item.raw_curr_by_name("minor");
            match (major, minor) {
                (Availability::Present(ma), Availability::Present(mi)) => {
                    ItemLabel::named(&format!("dev{ma}-{mi}"))
                }
                _ => ItemLabel::numbered("dev", item.index as u64),
            }
        }
    }
}

/// 文字列フィールドの値。
///
/// 取得できない場合は `None` (空文字ではなく「無い」を返す)。
/// `A_PWR_BAT` の `status` だけは数値コードを名前へ写す。
pub fn field_text(spec: &ActivitySpec, item: &ItemPair<'_>, col: &str) -> Option<String> {
    if spec.id == ActivityId::PWR_BAT && col == "status" {
        return match item.raw_curr_by_name(col) {
            Availability::Present(v) => Some(bat_status(v).to_string()),
            _ => None,
        };
    }
    // 文字列フィールドはスナップショットの識別子としてのみ保持される
    item.text(col).map(|s| s.to_string())
}

/// フィールド 1 個の値を「その形式の書式」で文字列にする。
///
/// `absent` は値を出せないときの表記。**0 は使わない。**
pub fn field_value(
    spec: &ActivitySpec,
    item: &ItemPair<'_>,
    field: &Field,
    fmt: Fmt,
    label: &ItemLabel,
    absent: &str,
) -> String {
    match fmt {
        Fmt::Skip => String::new(),
        Fmt::ItemKeyStr | Fmt::ItemKeyNum => label.jx.clone(),
        Fmt::Str => field_text(spec, item, field.col).unwrap_or_else(|| absent.to_string()),
        Fmt::Hex | Fmt::Int | Fmt::R0 | Fmt::R2 => {
            let mut s = String::new();
            let v = value_of(item, field);
            super::write_value(&mut s, v, fmt, absent);
            s
        }
    }
}

/// 表示値を引く。列を持たないフィールドは「未対応」として返す。
pub fn value_of(item: &ItemPair<'_>, field: &Field) -> Computed {
    if field.col.is_empty() {
        return Err(ComputeIssue::NotImplemented);
    }
    item.computed_by_name(field.col)
}

/// 値が無かった理由 (診断用)。
pub fn absence_of(item: &ItemPair<'_>, field: &Field) -> Option<Unavailable> {
    value_of(item, field).err().map(Unavailable)
}

/// `-j` / `-x` でのフィールド並び。
pub fn jx_fields(section: &Section) -> Vec<&Field> {
    if section.jx_order.is_empty() {
        section.fields.iter().collect()
    } else {
        section
            .jx_order
            .iter()
            .map(|&i| &section.fields[i])
            .collect()
    }
}

/// raw の 1 フィールド分の生値 (前, 現)。
pub fn raw_pair(
    item: &ItemPair<'_>,
    f: &super::spec::RawField,
) -> (Availability<u64>, Availability<u64>) {
    match f.style {
        RawStyle::PvalSum(cols) => (item.raw_sum(cols, true), item.raw_sum(cols, false)),
        RawStyle::PvalDiff(a, b) => (item.raw_diff(a, b, true), item.raw_diff(a, b, false)),
        _ => (item.raw_prev_by_name(f.col), item.raw_curr_by_name(f.col)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn battery_status_names_match_upstream_table() {
        assert_eq!(bat_status(0), "Unknown");
        assert_eq!(bat_status(4), "Full");
        assert_eq!(bat_status(99), "UNDEFINED");
    }
}

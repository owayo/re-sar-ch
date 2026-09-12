//! 形式をまたいで共有する値の取り出しと書式化。
//!
//! 数値は [`crate::series::compute`] の結果を書式化するだけ。
//! 文字列 (デバイス名・バッテリ状態) とアイテム識別子の作り方だけは
//! `sadf` 固有の規則があるのでここに置く。

use super::ItemLabel;
use super::access::ItemPair;
use super::spec::{ActivitySpec, Field, Fmt, ItemKind, RawStyle, Section};
use crate::model::{ActivityId, Availability};
use crate::series::compute::{ComputeIssue, Computed, SadfUnitColumn};

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

/// セクションを踏まえてアイテム識別子を組み立てる。
///
/// `A_FS` だけは `-F` / `-F MOUNT` で表示するフィールドが変わる (§2.8.1)。
/// 該当フィールドがその世代のファイルに無い場合は、
/// 適当な値で埋めずアイテム添字へ落とす。
pub fn item_label_in(spec: &ActivitySpec, section: &Section, item: &ItemPair<'_>) -> ItemLabel {
    if !section.item_col.is_empty() {
        return match item.text(section.item_col) {
            Some(name) => ItemLabel::named(name),
            None => ItemLabel::numbered("", item.index as u64),
        };
    }
    item_label(spec, item)
}

/// アイテム識別子を組み立てる。
pub fn item_label(spec: &ActivitySpec, item: &ItemPair<'_>) -> ItemLabel {
    match spec.item {
        ItemKind::None => ItemLabel::none(),
        // `A_IRQ` の出力 1 行 = 1 割り込み。割り込み名は CPU 行 0 にしか無い
        // (§6.2) ので、行の代表スロット (CPU `all`) から引く。
        //
        // `sadf` の `-d`/`-p`/`-j`/`-x`/`-r` は「フィールド名の位置に CPU
        // ラベルを入れる」逆転構造の専用経路を持つのでここを通らない (03 §11.1)。
        // 通るのは独自出力と `-dh` で、そこでは行を割り込み名で識別する。
        ItemKind::Irq => match item.key() {
            Some(k) => ItemLabel::named(k),
            None => ItemLabel::numbered("", item.index as u64),
        },
        ItemKind::Cpu => ItemLabel::cpu(item.index),
        ItemKind::Name => match item.key() {
            Some(k) => ItemLabel::named(k),
            // 名前を持たない item は位置で表す (0 で埋めない)
            None => ItemLabel::numbered("", item.index as u64),
        },
        // ブロックデバイス名はファイルに入っていない (§2.8.1)。
        // 本家 `get_devname()` の最終フォールバック `DEF_DEVICE_NAME` と同じ
        // `dev<major>-<minor>` 形式で組み立てる。ローカルの /sys を引くと
        // 「別ホストの major/minor に対応する名前」という誤った名前になるため引かない。
        ItemKind::Index { prefix, base } => {
            ItemLabel::numbered(prefix, item.index as u64 + base as u64)
        }
        ItemKind::Column { col, prefix } => match item.raw_curr_by_name(col) {
            Availability::Present(v) => ItemLabel::numbered(prefix, v),
            _ => ItemLabel::numbered(prefix, item.index as u64),
        },
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

/// `sadf` にしか無い「別単位の列」か。
///
/// 単位換算の表は計算層 ([`SadfUnitColumn::from_sadf_name`]) にある。
/// 出力層に 512 や 1024 を持ち込むと、同じ指標を `sar` 互換テキストと
/// `sadf` で出したときに形式ごとに値がずれる (`docs/design.md` §2)。
///
/// 列名は形式によって綴りが違う (`-d`/`-p` は `rxkB/s`、`-j`/`-x` は `rxkB`) ので
/// 3 つの綴りすべてで引く。どれも同じ変種に解決する。
fn sadf_unit_column(id: ActivityId, field: &Field) -> Option<SadfUnitColumn> {
    [field.key, field.attr, field.pp]
        .into_iter()
        .find_map(|name| SadfUnitColumn::from_sadf_name(id, name))
}

/// 表示値を引く。列を持たないフィールドは「未対応」として返す。
///
/// 識別子列 (`ValueKind::Identity`) は `series` 層が数値として扱わない
/// (`ComputeIssue::NotNumeric`) ので、生値を直接読む。
/// `A_PWR_USB` の `idvendor` / `idprod` や `A_PWR_BAT` の番号がこれに当たる。
pub fn value_of(item: &ItemPair<'_>, field: &Field) -> Computed {
    // 別単位の列 (`rd_sec` / `rxkB` / `MBfsfree` …) は計算層が換算まで持つ。
    // `rd_sec` 系は `col` を持たない (対応する `sar` 列が無い) ため、
    // 空チェックより先に判定する (03 §9.6-10)。
    if let Some(variant) = sadf_unit_column(item.def.id, field) {
        return item.sadf_unit(variant);
    }
    if field.col.is_empty() {
        return Err(ComputeIssue::NotImplemented);
    }
    if let Some(i) = item.column_index(field.col)
        && item.column_meta(i).map(|m| m.kind) == Some(crate::model::ValueKind::Identity)
    {
        return match item.raw_curr(i) {
            Availability::Present(v) => Ok(v as f64),
            Availability::UnsupportedBySource => Err(ComputeIssue::UnsupportedBySource),
            Availability::MissingInSample => Err(ComputeIssue::MissingInSample),
        };
    }
    item.computed_by_name(field.col)
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
    use crate::layout::plan::DecodePlan;
    use crate::model::{Availability, ValueKind};
    use crate::series::ItemSnapshot;
    use crate::series::compute::{ComputeContext, disk_col, fs_col, net_dev_col};

    #[test]
    fn battery_status_names_match_upstream_table() {
        assert_eq!(bat_status(0), "Unknown");
        assert_eq!(bat_status(4), "Full");
        assert_eq!(bat_status(99), "UNDEFINED");
    }

    fn plan_for(id: ActivityId) -> DecodePlan {
        let def = crate::layout::registry::lookup(id).expect("定義がある");
        let rev = def.latest().expect("revision がある");
        let enc = crate::format::abi::SourceEncoding::new(
            crate::format::abi::Endian::Little,
            crate::format::abi::LayoutAbi::LP64,
        );
        DecodePlan::build(def, rev, rev.size_lp64, 1, 1, &enc).expect("計画を作れる")
    }

    fn zeros(plan: &DecodePlan) -> ItemSnapshot {
        ItemSnapshot {
            key: None,
            texts: Vec::new(),
            values: vec![Availability::Present(0); plan.fields.len()],
        }
    }

    fn put(plan: &DecodePlan, item: &mut ItemSnapshot, column: usize, v: u64) {
        if let Some(Some(id)) = plan.column_fields.get(column)
            && let Some(slot) = item.values.get_mut(id.index())
        {
            *slot = Availability::Present(v);
        }
    }

    /// フィールド表からフィールドを 1 本引く (`-j` のキー名で探す)。
    fn field_by_key(id: ActivityId, key: &str) -> &'static Field {
        super::super::spec::lookup(id)
            .expect("出力定義")
            .sections
            .iter()
            .flat_map(|s| s.fields.iter())
            .find(|f| f.key == key)
            .expect("フィールドがある")
    }

    fn value(
        id: ActivityId,
        key: &str,
        plan: &DecodePlan,
        prev: &ItemSnapshot,
        curr: &ItemSnapshot,
        itv_cs: u64,
    ) -> Computed {
        let item = ItemPair {
            index: 1,
            def: crate::layout::registry::lookup(id).unwrap(),
            plan,
            prev,
            curr,
            ctx: ComputeContext::new(itv_cs),
            row: None,
        };
        value_of(&item, field_by_key(id, key))
    }

    /// **回帰テスト (指摘 3)**: ネットワークの kB 列がバイトのまま出ない。
    ///
    /// `A_NET_DEV` の保存値は**バイト**累積なので、非 human 出力は 1024 で
    /// 割って kB/s にする (03 §1.8.1)。以前は `rx_bytes_per_sec` を
    /// そのまま `rxkB` として書式化しており 1,024 倍ずれていた。
    #[test]
    fn network_kilobyte_columns_are_converted() {
        let plan = plan_for(ActivityId::NET_DEV);
        let prev = zeros(&plan);
        let mut curr = zeros(&plan);
        // 1 秒で 1 MiB / 2 MiB
        put(&plan, &mut curr, net_dev_col::RXKB, 1_024 * 1_024);
        put(&plan, &mut curr, net_dev_col::TXKB, 2 * 1_024 * 1_024);

        let rx = value(ActivityId::NET_DEV, "rxkB", &plan, &prev, &curr, 100).unwrap();
        let tx = value(ActivityId::NET_DEV, "txkB", &plan, &prev, &curr, 100).unwrap();
        assert_eq!(rx, 1_024.0, "1 MiB/s は 1024 kB/s");
        assert_eq!(tx, 2_048.0);
    }

    /// **回帰テスト (指摘 3)**: ファイルシステムの MB 列がバイトのまま出ない。
    ///
    /// `A_FS` の `f_*` はバイトなので 1024² で割る (03 §9.6-2)。
    #[test]
    fn filesystem_megabyte_columns_are_converted() {
        let plan = plan_for(ActivityId::FS);
        let prev = zeros(&plan);
        let mut curr = zeros(&plan);
        put(&plan, &mut curr, fs_col::TOTAL, 1_024 * 1_024 * 1_000);
        put(&plan, &mut curr, fs_col::MB_FREE, 1_024 * 1_024 * 300);

        let free = value(ActivityId::FS, "MBfsfree", &plan, &prev, &curr, 100).unwrap();
        let used = value(ActivityId::FS, "MBfsused", &plan, &prev, &curr, 100).unwrap();
        assert_eq!(free, 300.0, "1 MiB 空きは 1 (1048576 ではない)");
        assert_eq!(used, 700.0);
    }

    /// **回帰テスト (指摘 5)**: ディスクのセクタ列が接続されていること。
    ///
    /// `rd_sec` / `wr_sec` / `dc_sec` / `avgrq-sz` は kB 系列・`areq-sz` の
    /// 2 倍 (512 B セクタ、03 §9.6-10)。以前は参照列が空文字で
    /// `NotImplemented` に落ち、JSON で `null` になっていた。
    #[test]
    fn disk_sector_columns_are_twice_the_kilobyte_columns() {
        let plan = plan_for(ActivityId::DISK);
        let mut prev = zeros(&plan);
        let mut curr = zeros(&plan);
        put(&plan, &mut prev, disk_col::RKB, 0);
        // 1 秒で 読み 2048 セクタ / 書き 1024 セクタ / discard 512 セクタ、I/O 4 件
        put(&plan, &mut curr, disk_col::RKB, 2_048);
        put(&plan, &mut curr, disk_col::WKB, 1_024);
        put(&plan, &mut curr, disk_col::DKB, 512);
        put(&plan, &mut curr, disk_col::TPS, 4);

        let g = |key: &str| value(ActivityId::DISK, key, &plan, &prev, &curr, 100).unwrap();
        let pairs = [
            ("rd_sec", "rkB"),
            ("wr_sec", "wkB"),
            ("dc_sec", "dkB"),
            ("avgrq-sz", "areq-sz"),
        ];
        for (sectors, kb) in pairs {
            let s = g(sectors);
            let k = g(kb);
            assert!(s.is_finite(), "{sectors} が計算できない");
            assert_eq!(s, k * 2.0, "{sectors} は {kb} の 2 倍");
        }
        // セクタ列は「未実装」に落ちない
        assert_eq!(g("rd_sec"), 2_048.0);
    }

    /// 識別子列は生値をそのまま返す (`series` 層は数値として扱わない)。
    #[test]
    fn identity_columns_read_the_raw_value() {
        let plan = plan_for(ActivityId::PWR_USB);
        let prev = zeros(&plan);
        let mut curr = zeros(&plan);
        let def = crate::layout::registry::lookup(ActivityId::PWR_USB).unwrap();
        let col = def
            .columns
            .iter()
            .position(|c| c.public_name == "vendor_id")
            .unwrap();
        assert_eq!(def.columns[col].kind, ValueKind::Identity);
        put(&plan, &mut curr, col, 0x1d6b);

        assert_eq!(
            value(ActivityId::PWR_USB, "idvendor", &plan, &prev, &curr, 100),
            Ok(0x1d6b as f64)
        );
    }
}

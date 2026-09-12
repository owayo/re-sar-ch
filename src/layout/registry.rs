//! activity 定義のレジストリ。

use crate::format::wire::WireLayout;
use crate::model::{ActivityId, Aggregation, Unit, ValueKind};

/// activity 構造体の 1 世代分のレイアウト。
///
/// sysstat は activity ごとに `magic` を持ち、構造体が変わるとこれを上げる。
/// ただし `magic` が同じままフィールドが増えた版も存在するため、
/// `types_nr` (自己記述形式) と `size` も判別に使う。
#[derive(Debug, Clone, Copy)]
pub struct WireRevision {
    /// この revision に対応する activity magic。
    pub magic: u32,
    /// 型別フィールド数 `(ull, ul, int)`。自己記述形式のファイルとの突合に使う。
    pub types_nr: [u32; 3],
    /// LP64 における 1 item のサイズ。`file_activity.size` との突合に使う
    /// (**ストライドとして使うのは常にファイル申告値**)。
    pub size_lp64: usize,
    /// フィールド並び。
    pub layout: WireLayout,
    /// この revision が導入された sysstat バージョン (診断用)。
    pub since: &'static str,
}

/// 出力に現れる 1 列のメタデータ。
#[derive(Debug, Clone, Copy)]
pub struct ColumnMeta {
    /// 独自出力 (JSON / CSV / NDJSON) で使う公開名。
    pub public_name: &'static str,
    /// `sar` のヘッダ行に出る列名。`sar` に現れない列は空文字。
    pub sar_header: &'static str,
    /// 対応する wire フィールド名。派生列 (複数フィールドから計算) は空文字。
    pub wire_name: &'static str,
    pub unit: Unit,
    pub kind: ValueKind,
    pub aggregation: Aggregation,
}

impl ColumnMeta {
    /// 単一の wire フィールドから直接得られる列か。
    #[inline]
    pub const fn is_direct(&self) -> bool {
        !self.wire_name.is_empty()
    }
}

/// item の並び方。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemShape {
    /// 単一の構造体 (`nr` は 1)。
    Single,
    /// `nr` 個の item が連続する (CPU / デバイス / インターフェースなど)。
    List,
    /// `nr` 行 × `nr2` 列の行列 (`A_IRQ` のみ)。
    Matrix,
}

/// 1 activity の定義。
#[derive(Debug, Clone, Copy)]
pub struct ActivityDef {
    pub id: ActivityId,
    /// 既知の wire revision (新しい順に並べる)。
    pub revisions: &'static [WireRevision],
    /// 列メタデータ。
    pub columns: &'static [ColumnMeta],
    pub shape: ItemShape,
    /// item を識別する wire フィールド名 (デバイス名など)。無い場合は空文字。
    pub item_key: &'static str,
    /// `file_activity.has_nr` が立つ activity か (本家の `AO_COUNTED`)。
    ///
    /// これを取り違えると、レコードから 4 バイト余分に読む / 読み損ねる事故になる。
    pub has_nr: bool,
}

impl ActivityDef {
    /// activity magic に対応する revision を探す。
    pub fn revision_for_magic(&self, magic: u32) -> Option<&'static WireRevision> {
        self.revisions.iter().find(|r| r.magic == magic)
    }

    /// 型別フィールド数が一致する revision を探す (自己記述形式向け)。
    pub fn revision_for_types_nr(&self, types_nr: [u32; 3]) -> Option<&'static WireRevision> {
        self.revisions.iter().find(|r| r.types_nr == types_nr)
    }

    /// 最も新しい revision。
    pub fn latest(&self) -> Option<&'static WireRevision> {
        self.revisions.first()
    }
}

/// 定義済み activity を列挙する。
pub fn all() -> impl Iterator<Item = &'static ActivityDef> {
    super::activities::GROUPS.iter().flat_map(|g| g.iter())
}

/// activity ID から定義を引く。未登録なら `None` (エラーではなくスキップ対象)。
pub fn lookup(id: ActivityId) -> Option<&'static ActivityDef> {
    all().find(|d| d.id == id)
}

/// 定義済み activity の件数。
pub fn defined_count() -> usize {
    all().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_returns_none_for_unknown_id() {
        assert!(lookup(ActivityId(200)).is_none());
    }

    /// 登録済み定義の整合性を機械的に検査する。
    ///
    /// - ID の重複が無いこと
    /// - revision の `size_lp64` がレイアウト記述から導出した値と一致すること
    /// - `types_nr` がレイアウト記述のフィールド型構成と一致すること
    #[test]
    fn all_definitions_are_self_consistent() {
        use crate::format::abi::{Endian, LayoutAbi, SourceEncoding};
        use crate::format::wire::FieldTy;

        let enc = SourceEncoding::new(Endian::Little, LayoutAbi::LP64);
        let mut seen: Vec<u32> = Vec::new();

        for def in all() {
            assert!(
                !seen.contains(&def.id.0),
                "activity ID {} が重複している",
                def.id.0
            );
            seen.push(def.id.0);

            assert!(
                !def.columns.is_empty(),
                "{}: 列メタデータが空",
                def.id.display_name()
            );

            for rev in def.revisions {
                let resolved = rev
                    .layout
                    .resolve(&enc)
                    .unwrap_or_else(|e| panic!("{}: {e}", def.id.display_name()));

                assert_eq!(
                    resolved.size,
                    rev.size_lp64,
                    "{} (magic=0x{:x}): 導出サイズ {} が宣言値 {} と不一致",
                    def.id.display_name(),
                    rev.magic,
                    resolved.size,
                    rev.size_lp64
                );

                let ull = resolved
                    .fields
                    .iter()
                    .filter(|f| matches!(f.ty, FieldTy::U64 | FieldTy::I64))
                    .count() as u32;
                let ul = resolved
                    .fields
                    .iter()
                    .filter(|f| matches!(f.ty, FieldTy::CULong | FieldTy::CLong))
                    .count() as u32;
                let int = resolved
                    .fields
                    .iter()
                    .filter(|f| matches!(f.ty, FieldTy::U32 | FieldTy::I32))
                    .count() as u32;

                assert_eq!(
                    [ull, ul, int],
                    rev.types_nr,
                    "{} (magic=0x{:x}): 型別個数 [{ull},{ul},{int}] が宣言値 {:?} と不一致",
                    def.id.display_name(),
                    rev.magic,
                    rev.types_nr
                );
            }

            // 直接列は wire フィールドとして存在すること
            if let Some(rev) = def.latest() {
                let resolved = rev.layout.resolve(&enc).unwrap();
                for col in def.columns {
                    if col.is_direct() {
                        assert!(
                            resolved.field(col.wire_name).is_some(),
                            "{}: 列 {} が参照する wire フィールド {} が無い",
                            def.id.display_name(),
                            col.public_name,
                            col.wire_name
                        );
                    }
                }
            }
        }
    }
}

//! デコード計画。
//!
//! レイアウト記述と `file_activity` の申告値から、
//! 「どのオフセットを何バイト読むか」を**事前に解決**した表を作る。
//! レコードごとの走査ではこの表を引くだけで済み、
//! ホットパスからレイアウト解釈とフィールド名の照合が消える。

use super::registry::{ActivityDef, ItemShape, WireRevision};
use crate::format::abi::SourceEncoding;
use crate::format::reader::{Cursor, ReadResult};
use crate::format::wire::{FieldTy, PlacedField};
use crate::model::{Availability, CounterBits};

/// 解決済みフィールドの索引。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct FieldId(pub u16);

impl FieldId {
    #[inline]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// activity 1 種分のデコード計画。
#[derive(Debug, Clone)]
pub struct DecodePlan {
    /// 1 item のストライド。**`file_activity.size` の申告値**をそのまま使う。
    ///
    /// 構造体定義から計算した値を使ってはいけない。旧版の `A_HUGE` のように
    /// 本家が別構造体のサイズを書き込んでいる例があり、ずれるとレコード全体が崩れる。
    pub stride: usize,
    /// item の個数 (`file_activity.nr`)。
    pub nr: u32,
    /// 行列型 activity の列数 (`file_activity.nr2`)。それ以外は 1。
    pub nr2: u32,
    /// item の並び方。
    pub shape: ItemShape,
    /// フィールドの配置 (レイアウト記述の宣言順)。
    pub fields: Box<[PlacedField]>,
    /// 列 → フィールド索引。派生列やこの世代に無い列は `None`。
    pub column_fields: Box<[Option<FieldId>]>,
    /// item を識別するフィールド (デバイス名など)。
    pub item_key: Option<FieldId>,
    /// 文字列フィールドの索引 (宣言順)。
    ///
    /// `A_PWR_USB` の `manufact` / `product` のように、1 item が複数の文字列を
    /// 持つ activity があるため、`item_key` 1 本では足りない。
    pub text_fields: Box<[FieldId]>,
    /// レイアウト記述から導出したサイズ。申告値との差を診断に使う。
    pub derived_size: usize,
}

impl DecodePlan {
    /// 計画を組み立てる。
    ///
    /// `declared_size` は `file_activity.size` の申告値。
    pub fn build(
        def: &ActivityDef,
        rev: &WireRevision,
        declared_size: usize,
        nr: u32,
        nr2: u32,
        enc: &SourceEncoding,
    ) -> Result<Self, crate::error::LayoutError> {
        let resolved = rev.layout.resolve(enc)?;

        let mut column_fields: Vec<Option<FieldId>> = Vec::with_capacity(def.columns.len());
        for col in def.columns {
            let id = if col.is_direct() {
                resolved
                    .fields
                    .iter()
                    .position(|f| f.name == col.wire_name)
                    .map(|i| FieldId(i as u16))
            } else {
                None
            };
            column_fields.push(id);
        }

        let item_key = if def.item_key.is_empty() {
            None
        } else {
            resolved
                .fields
                .iter()
                .position(|f| f.name == def.item_key)
                .map(|i| FieldId(i as u16))
        };

        // 文字列フィールドを宣言順に集める
        let text_fields: Vec<FieldId> = resolved
            .fields
            .iter()
            .enumerate()
            .filter(|(_, f)| matches!(f.ty, FieldTy::Bytes(_)))
            .map(|(i, _)| FieldId(i as u16))
            .collect();

        Ok(Self {
            // ストライドは常に申告値。導出値との差は診断で報告する。
            stride: declared_size,
            nr,
            nr2,
            shape: def.shape,
            fields: resolved.fields,
            column_fields: column_fields.into_boxed_slice(),
            item_key,
            text_fields: text_fields.into_boxed_slice(),
            derived_size: resolved.size,
        })
    }

    /// このレコードで activity が占めるバイト数。
    ///
    /// `nr × nr2 × size` は 32bit で桁溢れし得る (本家の既知の脆弱性
    /// GHSL-2022-074 を突く細工ファイルが存在する)。必ず checked 演算で計算する。
    #[inline]
    pub fn payload_bytes(&self) -> Option<usize> {
        let items = match self.shape {
            ItemShape::Matrix => (self.nr as u64).checked_mul(self.nr2 as u64)?,
            _ => self.nr as u64,
        };
        let total = items.checked_mul(self.stride as u64)?;
        // 32bit 環境でも扱えることを保証する
        if total > u32::MAX as u64 {
            return None;
        }
        usize::try_from(total).ok()
    }

    /// 導出サイズが申告サイズより大きい = フィールドがはみ出す。
    #[inline]
    pub fn overflows_declared_size(&self) -> bool {
        self.derived_size > self.stride
    }

    /// item 1 個分の数値フィールドをデコードする。
    ///
    /// `out` は呼び出し側が再利用するバッファ。毎 item の確保を避ける。
    #[inline]
    pub fn decode_item_into(
        &self,
        cur: &Cursor<'_>,
        base: usize,
        out: &mut Vec<Availability<u64>>,
    ) -> ReadResult<()> {
        out.clear();
        out.reserve(self.fields.len());
        for f in self.fields.iter() {
            // 申告サイズ (ストライド) を超える位置は、このファイルには存在しない領域。
            //
            // 同じ activity magic で構造体サイズが複数ある世代があり、
            // 選んだ revision が申告サイズより大きいことがある
            // (例: A_CPU は magic 0x8a のまま 144 バイトと 160 バイトの 2 版が存在する)。
            // ここで読むと隣の item やファイル末尾を踏むため、未提供として扱う。
            if f.offset + f.width > self.stride {
                out.push(Availability::UnsupportedBySource);
                continue;
            }
            match f.ty {
                FieldTy::Bytes(_) => {
                    // 文字列は数値として保持しない (item_key 経由で読む)
                    out.push(Availability::MissingInSample);
                }
                _ => {
                    let v = cur.read_unsigned(base, f)?;
                    out.push(Availability::Present(v));
                }
            }
        }
        Ok(())
    }

    /// item の識別子を読む (ゼロコピー)。
    #[inline]
    pub fn read_item_key<'a>(&self, cur: &Cursor<'a>, base: usize) -> ReadResult<Option<&'a str>> {
        match self.item_key {
            Some(id) => {
                let f = &self.fields[id.index()];
                // 申告サイズを超える位置は読まない (decode_item_into と同じ理由)
                if f.offset + f.width > self.stride {
                    return Ok(None);
                }
                cur.read_str(base, f).map(Some)
            }
            None => Ok(None),
        }
    }

    /// 文字列フィールドをすべて読む (ゼロコピー)。
    ///
    /// `out` は [`DecodePlan::text_fields`] と同じ順で埋まる。
    #[inline]
    pub fn read_texts_into<'a>(
        &self,
        cur: &Cursor<'a>,
        base: usize,
        out: &mut Vec<Option<&'a str>>,
    ) -> ReadResult<()> {
        out.clear();
        out.reserve(self.text_fields.len());
        for id in self.text_fields.iter() {
            let f = &self.fields[id.index()];
            // 申告サイズを超える位置は読まない
            if f.offset + f.width > self.stride {
                out.push(None);
                continue;
            }
            out.push(cur.read_str(base, f).ok().filter(|s| !s.is_empty()));
        }
        Ok(())
    }

    /// wire フィールド名から、`text_fields` 内での位置を引く。
    ///
    /// 出力層が `item.texts[i]` を引くために使う。計画構築時に 1 度だけ呼ぶこと。
    pub fn text_index(&self, wire_name: &str) -> Option<usize> {
        self.text_fields
            .iter()
            .position(|id| self.fields[id.index()].name == wire_name)
    }

    /// 列に対応する値を取り出す。
    #[inline]
    pub fn column_value(&self, values: &[Availability<u64>], column: usize) -> Availability<u64> {
        match self.column_fields.get(column).copied().flatten() {
            Some(id) => values
                .get(id.index())
                .copied()
                .unwrap_or(Availability::MissingInSample),
            // この世代のファイルに存在しない列
            None => Availability::UnsupportedBySource,
        }
    }

    /// 列のカウンタ幅 (ラップ判定用)。
    #[inline]
    pub fn column_bits(&self, column: usize) -> Option<CounterBits> {
        let id = self.column_fields.get(column).copied().flatten()?;
        let f = self.fields.get(id.index())?;
        Some(CounterBits::from_value_width(f.value_width))
    }
}

/// item 1 個分のデコード結果。
#[derive(Debug, Clone)]
pub struct ItemValues<'a> {
    /// 数値フィールドの値 (宣言順)。
    pub numbers: Vec<Availability<u64>>,
    /// item の識別子 (デバイス名・インターフェース名など)。
    pub key: Option<&'a str>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_bytes_rejects_u32_overflow() {
        // data-12.7.1-A_IRQ_overflow 相当: 上限ぴったりの値で乗算が桁溢れする
        let plan = DecodePlan {
            stride: 1024,
            nr: 8193,
            nr2: 4096,
            shape: ItemShape::Matrix,
            fields: Box::new([]),
            column_fields: Box::new([]),
            item_key: None,
            text_fields: Box::new([]),
            derived_size: 1024,
        };
        // 8193 * 4096 * 1024 = 34,359,869,440 > u32::MAX
        assert_eq!(
            plan.payload_bytes(),
            None,
            "u32 を超える確保要求は拒否されること"
        );
    }

    /// 申告サイズを超える位置のフィールドは読まず、未提供として扱う。
    ///
    /// `A_CPU` は activity magic `0x8a` のまま 144 バイト版 (v10.1.1 以前) と
    /// 160 バイト版 (v10.1.2 以降) が存在する。160 の定義で 144 のファイルを読むと
    /// 最後のフィールドが隣の item を踏み、末尾 item ではファイル外へ出る。
    /// ここで範囲外エラーにも誤った値にもならないことを固定する。
    #[test]
    fn fields_beyond_declared_size_are_reported_as_unsupported() {
        use crate::format::abi::Endian;

        let field = |name: &'static str, offset: usize| PlacedField {
            name,
            offset,
            width: 8,
            value_width: 8,
            ty: FieldTy::U64,
        };
        let plan = DecodePlan {
            stride: 144,       // ファイルの申告値
            derived_size: 160, // 選んだ revision の導出サイズ
            nr: 1,
            nr2: 1,
            shape: ItemShape::List,
            fields: Box::new([field("first", 0), field("last", 152)]),
            column_fields: Box::new([Some(FieldId(0)), Some(FieldId(1))]),
            item_key: None,
            text_fields: Box::new([]),
        };
        assert!(plan.overflows_declared_size());

        // ファイルには申告サイズ分しか無い
        let buf = vec![0u8; 144];
        let cur = Cursor::new(&buf, Endian::Little);
        let mut out = Vec::new();

        plan.decode_item_into(&cur, 0, &mut out)
            .expect("はみ出すフィールドがあっても範囲外エラーにしない");
        assert_eq!(out.len(), 2);
        assert_eq!(out[0], Availability::Present(0), "存在する領域は読む");
        assert_eq!(
            out[1],
            Availability::UnsupportedBySource,
            "申告サイズを超える領域は未提供 (0 ではない)"
        );
        // 列経由でも未提供として見える
        assert_eq!(
            plan.column_value(&out, 1),
            Availability::UnsupportedBySource
        );
    }

    #[test]
    fn payload_bytes_for_list_ignores_nr2() {
        let plan = DecodePlan {
            stride: 64,
            nr: 9,
            nr2: 1,
            shape: ItemShape::List,
            fields: Box::new([]),
            column_fields: Box::new([]),
            item_key: None,
            text_fields: Box::new([]),
            derived_size: 64,
        };
        assert_eq!(plan.payload_bytes(), Some(576));
    }

    #[test]
    fn stride_uses_declared_size_not_derived() {
        // 旧版の A_HUGE は宣言サイズと導出サイズが食い違う。
        // ストライドは必ず申告値側を使う。
        let plan = DecodePlan {
            stride: 136,
            nr: 1,
            nr2: 1,
            shape: ItemShape::Single,
            fields: Box::new([]),
            column_fields: Box::new([]),
            item_key: None,
            text_fields: Box::new([]),
            derived_size: 16,
        };
        assert_eq!(plan.payload_bytes(), Some(136));
        assert!(!plan.overflows_declared_size());
    }

    #[test]
    fn detects_fields_overflowing_declared_size() {
        let plan = DecodePlan {
            stride: 16,
            nr: 1,
            nr2: 1,
            shape: ItemShape::Single,
            fields: Box::new([]),
            column_fields: Box::new([]),
            item_key: None,
            text_fields: Box::new([]),
            derived_size: 64,
        };
        assert!(plan.overflows_declared_size());
    }
}

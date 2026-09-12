//! デコード計画。
//!
//! レイアウト記述と `file_activity` の申告値から、
//! 「どのオフセットを何バイト読むか」を**事前に解決**した表を作る。
//! レコードごとの走査ではこの表を引くだけで済み、
//! ホットパスからレイアウト解釈とフィールド名の照合が消える。
//!
//! ## 計画に載るのは「このファイルで実際に読める領域」だけ
//!
//! 申告サイズ (`file_activity.size`) に収まらないフィールドは、計画を作る段階で
//! 「このファイルには存在しない」と確定させる ([`FieldPlan::read`] が `None`)。
//! 実行時に「オフセット + 幅 > 申告サイズなら読まない」と判定する方式では足りない:
//! レコード全体・ファイル全体を指す [`Cursor`] を渡していると、はみ出した読み取りが
//! バイト列の範囲内に収まってしまい、**隣の item や別レコードのバイトが統計値として
//! 出る**。実際に `A_CPU` を `size = 8` / `types_nr = [1,0,0]` と申告したファイルで、
//! ヘッダ検証を通ったまま別 item のバイトが読めることが確認されている。
//!
//! そのため、読み取りは [`ItemView`] (ストライド分だけを指すビュー) 越しにしか
//! できない形にしてある。境界を越える読み取りは構造的に起こらない。
//!
//! ## 自己記述形式ではファイルの申告からフィールド位置を組み立てる
//!
//! `0x2175` 世代の統計構造体は「ull × n0 → ul × n1 → int × n2 → その他 (文字列)」の
//! 物理順を必ず守る (8/8/4 スロットモデル。`docs/format/02-activities.md` §1)。
//! したがって `file_activity.types_nr` の申告値から各フィールドの位置を構築できる
//! (`docs/format/01-file-format.md` §4.3 / `02-activities.md` §8.2)。
//! 既知 revision と個数が食い違うときは [`DecodePlan::build_for`] がこの再配置を行う。
//! 同じことを `file_header` などのメタデータ構造体に対して行っているのが
//! [`crate::format::selfdesc`] で、規則はそちらと同一である。

use super::registry::{ActivityDef, ItemShape, WireRevision};
use crate::format::abi::SourceEncoding;
use crate::format::reader::{Cursor, ReadResult};
use crate::format::wire::{
    AlignSpec, FieldTy, PlacedField, ResolvedLayout, WireField, resolve_fields,
};
use crate::model::{Availability, CounterBits};

/// 既知フィールド数を超えた申告分に割り当てる名前。値は読まず位置だけを進める。
///
/// [`crate::format::selfdesc`] の同名定数と同じ役割。
const RESERVED: &str = "__reserved";

/// 型グループ 1 つあたりの申告個数の上限 (防御用)。
///
/// `file_activity` の検証 (`MAP_SIZE <= size <= MAX_ITEM_STRUCT_SIZE`) を通れば
/// この値には届かないが、計画構築を直接呼ばれた場合に
/// 申告値に比例した確保をしないための歯止めとして置く。
const MAX_DECLARED_PER_GROUP: u32 = 1024;

/// 解決済みフィールドの索引。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct FieldId(pub u16);

impl FieldId {
    #[inline]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// 計画に載った 1 フィールド。
///
/// `read` が `None` のフィールドは**このファイルには存在しない**。
/// 世代が古くてフィールドがまだ無い場合と、申告サイズに収まらない場合の両方が
/// ここに落ちる。値は [`Availability::UnsupportedBySource`] になり、0 では埋めない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldPlan {
    /// wire フィールド名 (レイアウト記述の宣言名)。
    pub name: &'static str,
    /// レイアウト記述上の型。フィールドが存在しない場合でも型は分かる。
    pub ty: FieldTy,
    /// 読み取り位置と幅。`None` はこのファイルに存在しないフィールド。
    pub read: Option<PlacedField>,
}

impl FieldPlan {
    /// このファイルで読めるフィールドか。
    #[inline]
    pub const fn is_available(&self) -> bool {
        self.read.is_some()
    }
}

/// item 1 個分のバイト列ビュー。
///
/// **ストライド分だけ**を指すことが型で保証されるので、フィールドの読み取りが
/// 隣の item やファイル末尾へはみ出すことが構造的に起こらない。
/// レコード全体を指す [`Cursor`] を直接渡せないようにするためのラッパである。
#[derive(Debug, Clone, Copy)]
pub struct ItemView<'a> {
    cur: Cursor<'a>,
}

impl<'a> ItemView<'a> {
    /// レコード内の 1 item を切り出す。
    ///
    /// `stride` は `file_activity.size` の申告値。ここで範囲を確定させるので、
    /// 以降の読み取りは item の外へ出られない。
    /// 切り出せない (バイト列が足りない) 場合は範囲外として返す。
    #[inline]
    pub fn new(cur: &Cursor<'a>, base: usize, stride: usize) -> ReadResult<Self> {
        Ok(Self {
            cur: cur.slice(base, stride)?,
        })
    }

    /// item のバイト数 (= ストライド)。
    #[inline]
    pub fn len(&self) -> usize {
        self.cur.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.cur.is_empty()
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
    pub fields: Box<[FieldPlan]>,
    /// 列 → フィールド索引。派生列は `None`。
    ///
    /// この世代に無い列もフィールド索引は持つ (値が
    /// [`Availability::UnsupportedBySource`] になる)。
    pub column_fields: Box<[Option<FieldId>]>,
    /// item を識別するフィールド (デバイス名など)。
    pub item_key: Option<FieldId>,
    /// 文字列フィールドの索引 (宣言順)。
    ///
    /// `A_PWR_USB` の `manufact` / `product` のように、1 item が複数の文字列を
    /// 持つ activity があるため、`item_key` 1 本では足りない。
    ///
    /// 申告サイズに収まらない文字列フィールドも**位置は保つ**。
    /// ここを詰めると [`DecodePlan::text_index`] の位置が世代でずれ、
    /// 出力側が別の文字列を引いてしまう。値は `None` になる。
    pub text_fields: Box<[FieldId]>,
    /// 解決した配置のサイズ。申告値との差を診断に使う。
    pub derived_size: usize,
}

/// `file_activity` が申告した統計構造体の形。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeclaredShape {
    /// activity magic。
    ///
    /// `None` は `file_activity` に magic フィールドが無い世代
    /// (`format_magic` = `0x2170`)。「magic が 0」と「magic が無い」は別物なので
    /// `Option` で区別する。
    pub magic: Option<u32>,
    /// `file_activity.size`。1 item のストライドでもある。
    pub size: usize,
    /// `file_activity.types_nr` (自己記述形式のみ)。
    pub types_nr: Option<[u32; 3]>,
}

/// 互換な配置が確定できない理由。
///
/// いずれも**エラーではなくスキップ**の理由である。未知の activity を読み飛ばすのと
/// 同じ扱いで、境界は申告値から確定しているので後続のレコードは読み続けられる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Incompatible {
    /// 定義に revision が 1 つも無い (定義側の漏れ)。
    NoRevisions,
    /// 既知のどの revision とも activity magic が一致しない。
    ///
    /// magic は「構造体の意味が変わった」ことを表す番号なので、
    /// 個数とサイズが既知の値と一致していても中身の意味は保証されない。
    /// 本家もこの activity を読み飛ばす (`docs/format/02-activities.md` §8.1)。
    UnknownMagic(u32),
    /// magic を持たない世代で、申告サイズが既知 revision と一致しない。
    UnknownSize(usize),
}

impl std::fmt::Display for Incompatible {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Incompatible::NoRevisions => write!(f, "レイアウト定義に revision が無い"),
            Incompatible::UnknownMagic(m) => write!(
                f,
                "activity magic 0x{m:x} が既知のどの revision とも一致しない \
                 (フィールドの意味が確認できないので読み飛ばす)"
            ),
            Incompatible::UnknownSize(s) => write!(
                f,
                "magic を持たない世代で、申告サイズ {s} が既知のどの revision とも一致しない"
            ),
        }
    }
}

/// 申告値と互換な revision を選ぶ。
///
/// **magic が一致する revision だけを互換とみなす。** 未知 magic を
/// 「個数とサイズがそれらしいから」で読むと、フィールドの意味が違う可能性を
/// 確認しないまま「もっともらしい誤値」を出す。実際 v11.7.1 は新レイアウトを
/// 旧 magic で書いてしまっており (`docs/format/02-activities.md` §3.4)、
/// magic を無視して読むとフィールドがずれたまま統計値として通ってしまう。
///
/// 同じ magic に複数の配置がある場合 (`A_CPU` の magic `0x8a` は 144 バイト版と
/// 160 バイト版がある) は、型別個数の完全一致 → 申告サイズ一致 → 同 magic の最新
/// の順に絞る。最後の経路では [`DecodePlan::build_for`] が申告された型別個数から
/// 配置を組み立て直す。
///
/// 「サイズも一致しなければ読み飛ばす」まで厳しくしてはいけない。
/// 旧 `A_HUGE` は本家が別構造体のサイズ (136) を書き込んでいるため
/// (`docs/format/02-activities.md` §9.5)、正常なファイルでもサイズが一致しない。
/// 実データ (v9.1.6 / v10.3.1 / v11.6.5) で確認済み。
pub fn select_revision(
    def: &ActivityDef,
    shape: &DeclaredShape,
) -> Result<&'static WireRevision, Incompatible> {
    if def.revisions.is_empty() {
        return Err(Incompatible::NoRevisions);
    }

    let Some(magic) = shape.magic else {
        // magic を持たない世代 (`0x2170`) の明示的な例外。
        // 互換性を確認する手がかりが申告サイズしかないので、サイズが一致する
        // revision だけを使う。一致しなければ読み飛ばす (推測で最新の配置を
        // 当てると、意味の違うフィールドを統計値として出すことになる)。
        return def
            .revision_for_size(shape.size)
            .ok_or(Incompatible::UnknownSize(shape.size));
    };

    let mut newest: Option<&'static WireRevision> = None;
    let mut by_types: Option<&'static WireRevision> = None;
    let mut by_size: Option<&'static WireRevision> = None;
    // revisions は新しい順に並んでいるので、最初に見つかったものが最新。
    for rev in def.revisions.iter().filter(|r| r.magic == magic) {
        if newest.is_none() {
            newest = Some(rev);
        }
        if by_types.is_none() && shape.types_nr == Some(rev.types_nr) {
            by_types = Some(rev);
        }
        if by_size.is_none() && rev.size_lp64 == shape.size {
            by_size = Some(rev);
        }
    }

    by_types
        .or(by_size)
        .or(newest)
        .ok_or(Incompatible::UnknownMagic(magic))
}

/// 申告された型別個数が、既知の個数に対して「全て増加方向」か
/// 「全て減少方向」のどちらかに収まっているか。
///
/// sysstat の規約では**フィールドを減らすときは activity magic を上げる**ため、
/// 同じ magic のまま「ULL は増えたが int は減った」という混在は起こらない
/// (`docs/format/01-file-format.md` §4.5)。混在しているファイルは申告が壊れており、
/// どちらの配置で読んでも意味が合わないので、統計を読む段で拒否する
/// (ヘッダ表示だけなら通す、という免除が本家にもある)。
pub fn types_nr_is_monotonic(declared: [u32; 3], known: [u32; 3]) -> bool {
    let all_ge = declared.iter().zip(known).all(|(f, g)| *f >= g);
    let all_le = declared.iter().zip(known).all(|(f, g)| *f <= g);
    all_ge || all_le
}

impl DecodePlan {
    /// 計画を組み立てる (申告サイズだけを使う経路)。
    ///
    /// `declared_size` は `file_activity.size` の申告値。
    /// 自己記述形式の `types_nr` も反映する場合は [`DecodePlan::build_for`] を使う。
    pub fn build(
        def: &ActivityDef,
        rev: &WireRevision,
        declared_size: usize,
        nr: u32,
        nr2: u32,
        enc: &SourceEncoding,
    ) -> Result<Self, crate::error::LayoutError> {
        let shape = DeclaredShape {
            magic: Some(rev.magic),
            size: declared_size,
            types_nr: None,
        };
        Self::build_for(def, rev, &shape, nr, nr2, enc)
    }

    /// ファイルの申告値を反映して計画を組み立てる。
    ///
    /// - `types_nr` が選んだ revision と完全一致するなら、その revision の配置を使う。
    /// - 食い違うなら、**申告された型別個数から配置を組み立て直す**
    ///   (8/8/4 スロットモデル。`docs/format/02-activities.md` §8.2)。
    ///   グループの申告個数が既知フィールド数より少なければ、そのグループ末尾の
    ///   既知フィールドは欠落として扱う。
    /// - いずれの経路でも、**申告サイズに収まらないフィールドは欠落**にする。
    pub fn build_for(
        def: &ActivityDef,
        rev: &WireRevision,
        shape: &DeclaredShape,
        nr: u32,
        nr2: u32,
        enc: &SourceEncoding,
    ) -> Result<Self, crate::error::LayoutError> {
        let resolved = match shape.types_nr {
            // 完全一致する revision が選ばれているときは、その配置をそのまま使う
            // (再配置は不一致時だけの経路)。
            Some(types) if types != rev.types_nr => match remap_to_declared(rev, types, enc)? {
                Some(r) => r,
                // 宣言順が物理順に並んでいない revision は並べ直せない。
                // 既知配置のまま扱い、申告サイズに収まらない分は欠落にする。
                None => rev.layout.resolve(enc)?,
            },
            _ => rev.layout.resolve(enc)?,
        };

        // 宣言順のまま、各フィールドが「このファイルで読めるか」を確定させる。
        let fields: Box<[FieldPlan]> = rev
            .layout
            .fields
            .iter()
            .map(|wf| FieldPlan {
                name: wf.name,
                ty: wf.ty,
                read: resolved
                    .field(wf.name)
                    .copied()
                    // 申告サイズに収まらない位置は、このファイルには存在しない領域。
                    // 読むと隣の item を踏むので、計画の段階で欠落に落とす。
                    .filter(|p| fits_in(p, shape.size)),
            })
            .collect();

        let mut column_fields: Vec<Option<FieldId>> = Vec::with_capacity(def.columns.len());
        for col in def.columns {
            let id = if col.is_direct() {
                fields
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
            fields
                .iter()
                .position(|f| f.name == def.item_key)
                .map(|i| FieldId(i as u16))
        };

        // 文字列フィールドを宣言順に集める (欠落しているものも位置を保つ)
        let text_fields: Vec<FieldId> = fields
            .iter()
            .enumerate()
            .filter(|(_, f)| matches!(f.ty, FieldTy::Bytes(_)))
            .map(|(i, _)| FieldId(i as u16))
            .collect();

        Ok(Self {
            // ストライドは常に申告値。導出値との差は診断で報告する。
            stride: shape.size,
            nr,
            nr2,
            shape: def.shape,
            fields,
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

    /// 導出サイズが申告サイズより大きい = はみ出すフィールドがある。
    #[inline]
    pub fn overflows_declared_size(&self) -> bool {
        self.derived_size > self.stride
    }

    /// このファイルには存在しないフィールドの数 (診断用)。
    #[inline]
    pub fn unavailable_fields(&self) -> usize {
        self.fields.iter().filter(|f| !f.is_available()).count()
    }

    /// レコード内の 1 item を、ストライド分だけのビューとして切り出す。
    #[inline]
    pub fn item_view<'a>(&self, cur: &Cursor<'a>, base: usize) -> ReadResult<ItemView<'a>> {
        ItemView::new(cur, base, self.stride)
    }

    /// item 1 個分の数値フィールドをデコードする。
    ///
    /// `out` は呼び出し側が再利用するバッファ。毎 item の確保を避ける。
    /// 読み取りは `item` の範囲内に限られるので、はみ出しは起こらない。
    #[inline]
    pub fn decode_item_into(
        &self,
        item: &ItemView<'_>,
        out: &mut Vec<Availability<u64>>,
    ) -> ReadResult<()> {
        out.clear();
        out.reserve(self.fields.len());
        for f in self.fields.iter() {
            match &f.read {
                // このファイルに存在しないフィールド。0 で埋めると
                // 「正常な 0」と区別できなくなり、平均や閾値判定が静かに誤る。
                None => out.push(Availability::UnsupportedBySource),
                // 文字列は数値として保持しない (texts / item_key 経由で読む)
                Some(_) if matches!(f.ty, FieldTy::Bytes(_)) => {
                    out.push(Availability::MissingInSample)
                }
                Some(p) => {
                    let v = item.cur.read_unsigned(0, p)?;
                    out.push(Availability::Present(v));
                }
            }
        }
        Ok(())
    }

    /// item の識別子を読む (ゼロコピー)。
    ///
    /// 不正な UTF-8 を含む名前は `None` になる (別の item と同一視しないため)。
    #[inline]
    pub fn read_item_key<'a>(&self, item: &ItemView<'a>) -> ReadResult<Option<&'a str>> {
        let Some(id) = self.item_key else {
            return Ok(None);
        };
        let Some(p) = self.fields[id.index()].read.as_ref() else {
            // この世代・この申告サイズには識別子フィールドが無い
            return Ok(None);
        };
        item.cur.read_str(0, p)
    }

    /// 文字列フィールドをすべて読む (ゼロコピー)。
    ///
    /// `out` は [`DecodePlan::text_fields`] と同じ順で埋まる。
    #[inline]
    pub fn read_texts_into<'a>(
        &self,
        item: &ItemView<'a>,
        out: &mut Vec<Option<&'a str>>,
    ) -> ReadResult<()> {
        out.clear();
        out.reserve(self.text_fields.len());
        for id in self.text_fields.iter() {
            match self.fields[id.index()].read.as_ref() {
                // このファイルには無い文字列フィールド
                None => out.push(None),
                Some(p) => out.push(item.cur.read_str(0, p)?.filter(|s| !s.is_empty())),
            }
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

    /// 列のカウンタ幅 (ラップ判定用)。このファイルに無い列は `None`。
    #[inline]
    pub fn column_bits(&self, column: usize) -> Option<CounterBits> {
        let id = self.column_fields.get(column).copied().flatten()?;
        let p = self.fields.get(id.index())?.read.as_ref()?;
        Some(CounterBits::from_value_width(p.value_width))
    }
}

/// フィールドの終端が申告サイズに収まるか。
#[inline]
fn fits_in(p: &PlacedField, declared_size: usize) -> bool {
    p.offset
        .checked_add(p.width)
        .is_some_and(|end| end <= declared_size)
}

/// 8/8/4 スロットモデルでの型グループ (0 = ull / 1 = ul / 2 = int / 3 = その他)。
///
/// `double` は `unsigned long long` と同じ 8 バイトスロットを占め `types_nr[0]` に
/// 数えられる (`docs/format/02-activities.md` §1.2)。レイアウト記述では
/// [`FieldTy::U64`] として表しているので、ここでも同じグループになる。
/// `char` 配列や 1 バイトフラグは個数に数えられない末尾領域 (グループ 3)。
const fn group_index(ty: FieldTy) -> usize {
    match ty {
        FieldTy::U64 | FieldTy::I64 => 0,
        FieldTy::CULong | FieldTy::CLong => 1,
        FieldTy::U32 | FieldTy::I32 => 2,
        FieldTy::U8 | FieldTy::U16 | FieldTy::I8 | FieldTy::I16 | FieldTy::Bytes(_) => 3,
    }
}

/// 1 グループ分のフィールドを積む。
///
/// [`crate::format::selfdesc`] の `push_group` と同じ規則。
///
/// - 申告個数が既知フィールド数**以下**なら、先頭から申告個数だけ使う
///   (残りはこのファイルに存在しない)。
/// - 申告個数が既知フィールド数を**超える**なら、超過分を予備フィールドとして積み、
///   後続フィールドの位置だけを正しく進める。値は読まない。
fn push_group(out: &mut Vec<WireField>, known: &[WireField], count: u32, filler: FieldTy) {
    let count = count as usize;
    let take = count.min(known.len());
    out.extend_from_slice(&known[..take]);
    for _ in take..count {
        out.push(WireField::natural(RESERVED, filler));
    }
}

/// 申告された型別個数からフィールドを並べ直す。
///
/// 自己記述形式の統計構造体は「ull → ul → int → その他」の物理順を必ず守り、
/// 各グループの占有幅は 8 / 8 / 4 バイトに固定されている
/// (`docs/format/02-activities.md` §1)。したがってファイル側の申告個数だけで
/// 既知フィールドの位置を確定できる。
///
/// 例: `A_NET_DEV` の ULL 群が互換拡張されて `types_nr = [8,0,1]` / `size = 88` に
/// なった場合、`speed` / `interface` / `duplex` の正しい位置は 64 / 68 / 84 で、
/// 現行定義の 56 / 60 / 76 ではない。
///
/// 既知フィールドの宣言順が物理順に並んでいない revision は並べ直せないので
/// `None` を返す (呼び出し側は既知配置のまま扱う)。
fn remap_to_declared(
    rev: &WireRevision,
    declared: [u32; 3],
    enc: &SourceEncoding,
) -> Result<Option<ResolvedLayout>, crate::error::LayoutError> {
    // 申告値に比例した確保をしないための歯止め (通常は file_activity の
    // 検証で弾かれている値なので、ここに来たら並べ直しをあきらめる)。
    if declared.iter().any(|&n| n > MAX_DECLARED_PER_GROUP) {
        return Ok(None);
    }

    let mut groups: [Vec<WireField>; 4] = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
    let mut prev_group = 0usize;
    for f in rev.layout.fields {
        let g = group_index(f.ty);
        if g < prev_group {
            // 物理順に並んでいない = 8/8/4 スロットモデルで表せない
            return Ok(None);
        }
        prev_group = g;
        // 数値グループは固定スロットに詰めて並ぶ。時代 A の revision に付いている
        // `aligned(16)` などの指定は自己記述形式の構造体には無いので、
        // 自然アラインメントへ戻してから並べ直す。
        groups[g].push(WireField::natural(f.name, f.ty));
    }

    let mut fields: Vec<WireField> = Vec::with_capacity(rev.layout.fields.len() + 4);
    push_group(&mut fields, &groups[0], declared[0], FieldTy::U64);
    push_group(&mut fields, &groups[1], declared[1], FieldTy::CULong);
    push_group(&mut fields, &groups[2], declared[2], FieldTy::U32);
    // 文字列などの末尾領域は types_nr に数えられない (§4.2)
    fields.extend_from_slice(&groups[3]);

    resolve_fields(rev.layout.name, &fields, AlignSpec::Natural, enc).map(Some)
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
    use crate::format::abi::{Endian, LayoutAbi};
    use crate::layout::registry;
    use crate::model::ActivityId;

    fn lp64() -> SourceEncoding {
        SourceEncoding::new(Endian::Little, LayoutAbi::LP64)
    }

    fn def_of(id: ActivityId) -> &'static ActivityDef {
        registry::lookup(id).expect("定義済み activity")
    }

    /// 名前でフィールド計画を引く (テスト用)。
    fn field<'p>(plan: &'p DecodePlan, name: &str) -> &'p FieldPlan {
        plan.fields
            .iter()
            .find(|f| f.name == name)
            .unwrap_or_else(|| panic!("{name} が計画に無い"))
    }

    fn offset_of(plan: &DecodePlan, name: &str) -> Option<usize> {
        field(plan, name).read.map(|p| p.offset)
    }

    fn plan_for(id: ActivityId, shape: DeclaredShape, nr: u32) -> DecodePlan {
        let def = def_of(id);
        let rev = select_revision(def, &shape).expect("互換な revision があること");
        DecodePlan::build_for(def, rev, &shape, nr, 1, &lp64()).expect("計画を組める")
    }

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

    // =======================================================================
    // 指摘 1: 申告サイズを超えるフィールドを item 境界の外から読まない
    // =======================================================================

    /// 申告サイズに収まらないフィールドは、**計画の段階で**欠落に確定する。
    ///
    /// `A_CPU` は activity magic `0x8b` のまま 80 バイトを申告するのが正常だが、
    /// `size = 16` と申告するファイルを作れる。以前の実装は「実行時に
    /// オフセットを比べる」だけだったので、ファイル全体を指す `Cursor` を
    /// 渡していると隣の item のバイトが読めてしまった。
    #[test]
    fn fields_beyond_declared_size_are_absent_in_the_plan() {
        let plan = plan_for(
            ActivityId::CPU,
            DeclaredShape {
                magic: Some(0x8b),
                size: 16,
                types_nr: None,
            },
            1,
        );
        assert!(plan.overflows_declared_size(), "導出 80 > 申告 16");
        // 16 バイトに収まるのは先頭 2 本だけ
        assert_eq!(offset_of(&plan, "cpu_user"), Some(0));
        assert_eq!(offset_of(&plan, "cpu_nice"), Some(8));
        assert_eq!(offset_of(&plan, "cpu_sys"), None);
        assert_eq!(plan.unavailable_fields(), 8);
        // 列経由でも「この世代には無い」と見える
        let idx = plan
            .fields
            .iter()
            .position(|f| f.name == "cpu_sys")
            .unwrap();
        let mut values = vec![Availability::Present(0u64); plan.fields.len()];
        values[idx] = Availability::UnsupportedBySource;
        let col = def_of(ActivityId::CPU)
            .columns
            .iter()
            .position(|c| c.wire_name == "cpu_sys")
            .unwrap();
        assert_eq!(
            plan.column_value(&values, col),
            Availability::UnsupportedBySource
        );
        // 存在しないフィールドにカウンタ幅を答えてはいけない
        // (差分計算がラップ判定の根拠として使う値なので、
        //  「読めない列に 64bit カウンタがある」と答えると誤った差分を作る)
        assert_eq!(plan.column_bits(col), None);
    }

    /// 欠落したフィールドは、隣の item の値ではなく未提供としてデコードされる。
    ///
    /// レビューの再現内容 (`A_CPU` を `size = 8` / `types_nr = [1,0,0]` と申告) を
    /// そのまま固定する。item 1 に別の値を置き、item 0 のデコードへ漏れないことを見る。
    #[test]
    fn neighbouring_item_bytes_never_leak_into_missing_fields() {
        let plan = plan_for(
            ActivityId::CPU,
            DeclaredShape {
                magic: Some(0x8b),
                size: 8,
                types_nr: Some([1, 0, 0]),
            },
            2,
        );
        assert_eq!(plan.stride, 8);
        assert_eq!(offset_of(&plan, "cpu_user"), Some(0));
        assert_eq!(offset_of(&plan, "cpu_nice"), None, "申告は ull 1 本だけ");

        // item 0 = 111、item 1 = 999 を置く
        let mut buf = Vec::new();
        buf.extend_from_slice(&111u64.to_le_bytes());
        buf.extend_from_slice(&999u64.to_le_bytes());
        let cur = Cursor::new(&buf, Endian::Little);

        let mut out = Vec::new();
        let item0 = plan.item_view(&cur, 0).expect("item 0 を切り出せる");
        plan.decode_item_into(&item0, &mut out).expect("デコード");
        assert_eq!(out[0], Availability::Present(111));
        for (i, v) in out.iter().enumerate().skip(1) {
            assert_eq!(
                *v,
                Availability::UnsupportedBySource,
                "{} は未提供のはず (隣の item を読んでいない)",
                plan.fields[i].name
            );
        }

        // item 1 を読むと 999 になる = バイト列自体は隣に存在している
        let item1 = plan.item_view(&cur, plan.stride).expect("item 1");
        plan.decode_item_into(&item1, &mut out).expect("デコード");
        assert_eq!(out[0], Availability::Present(999));
    }

    /// item ビューはストライド分しか指さないので、読み取りが外へ出られない。
    #[test]
    fn item_view_is_limited_to_the_stride() {
        let buf = [0u8; 24];
        let cur = Cursor::new(&buf, Endian::Little);
        let view = ItemView::new(&cur, 8, 8).expect("範囲内");
        assert_eq!(view.len(), 8, "ビューはストライド分だけ");
        // レコード末尾を越える切り出しは範囲外として失敗する
        assert!(ItemView::new(&cur, 20, 8).is_err());
    }

    /// 申告サイズに収まらない識別子フィールドは読まない。
    #[test]
    fn item_key_beyond_declared_size_is_not_read() {
        // A_NET_DEV の interface は 60..76。size = 56 では存在しない。
        let plan = plan_for(
            ActivityId::NET_DEV,
            DeclaredShape {
                magic: Some(0x8d),
                size: 56,
                types_nr: None,
            },
            1,
        );
        assert_eq!(offset_of(&plan, "interface"), None);
        let mut buf = vec![0u8; 56];
        buf[..4].copy_from_slice(b"eth0");
        let cur = Cursor::new(&buf, Endian::Little);
        let item = plan.item_view(&cur, 0).unwrap();
        assert_eq!(plan.read_item_key(&item), Ok(None));
        // 文字列の位置は保たれ、値だけが None になる
        let mut texts = Vec::new();
        plan.read_texts_into(&item, &mut texts).unwrap();
        assert_eq!(plan.text_index("interface"), Some(0));
        assert_eq!(texts, vec![None]);
    }

    // =======================================================================
    // 指摘 2: 未知 magic を既知レイアウトで解釈しない
    // =======================================================================

    /// 既知の個数・サイズでも、magic が未知なら互換とみなさない。
    #[test]
    fn unknown_activity_magic_is_not_compatible() {
        let shape = DeclaredShape {
            magic: Some(0x99),
            size: 80,
            types_nr: Some([10, 0, 0]),
        };
        assert_eq!(
            select_revision(def_of(ActivityId::CPU), &shape).err(),
            Some(Incompatible::UnknownMagic(0x99))
        );
    }

    /// 同じ magic に複数の配置がある場合は申告サイズで決める。
    #[test]
    fn same_magic_is_disambiguated_by_declared_size() {
        // A_CPU の magic 0x8a には 144 バイト版 (cpu_guest_nice なし) と
        // 160 バイト版がある。magic だけでは決まらない。
        for (size, expect_types) in [(144usize, [9u32, 0, 0]), (160, [10, 0, 0])] {
            let shape = DeclaredShape {
                magic: Some(0x8a),
                size,
                types_nr: None,
            };
            let rev = select_revision(def_of(ActivityId::CPU), &shape).expect("選べる");
            assert_eq!(rev.size_lp64, size);
            assert_eq!(rev.types_nr, expect_types);
        }
    }

    /// 型別個数の完全一致は申告サイズより強い手がかり。
    ///
    /// 旧 `A_HUGE` は別構造体のサイズ (136) が書かれていることがあり
    /// (`docs/format/02-activities.md` §9.5)、サイズ一致では選べない。
    #[test]
    fn exact_types_nr_wins_over_size() {
        let def = def_of(ActivityId::HUGE);
        let shape = DeclaredShape {
            magic: Some(0x8b),
            size: 136,
            types_nr: Some([2, 0, 0]),
        };
        let rev = select_revision(def, &shape).expect("選べる");
        assert_eq!(rev.types_nr, [2, 0, 0]);
    }

    /// magic を持たない世代 (`0x2170`) は申告サイズだけで判断する。
    #[test]
    fn generation_without_activity_magic_uses_size_only() {
        let def = def_of(ActivityId::CPU);
        let shape = DeclaredShape {
            magic: None,
            size: 160,
            types_nr: None,
        };
        assert_eq!(
            select_revision(def, &shape).map(|r| r.size_lp64),
            Ok(160),
            "サイズが一致する revision を使う"
        );

        // 一致しなければ読み飛ばす (最新の配置を当てて誤値を出さない)
        let unknown = DeclaredShape {
            magic: None,
            size: 96,
            types_nr: None,
        };
        assert_eq!(
            select_revision(def, &unknown).err(),
            Some(Incompatible::UnknownSize(96))
        );
    }

    // =======================================================================
    // 指摘 3: 申告された型別個数をフィールド位置に反映する
    // =======================================================================

    /// ULL 群が 1 本増えた申告では、後続フィールドがその分だけ後ろへ動く。
    ///
    /// レビューの例をそのまま固定する: `A_NET_DEV` が `types_nr = [8,0,1]` /
    /// `size = 88` を申告したら、`speed` / `interface` / `duplex` は
    /// 64 / 68 / 84 にある (現行定義の 56 / 60 / 76 ではない)。
    #[test]
    fn declared_types_nr_moves_following_groups() {
        let plan = plan_for(
            ActivityId::NET_DEV,
            DeclaredShape {
                magic: Some(0x8d),
                size: 88,
                types_nr: Some([8, 0, 1]),
            },
            1,
        );
        // 既知の ULL 7 本の位置は変わらない
        assert_eq!(offset_of(&plan, "rx_packets"), Some(0));
        assert_eq!(offset_of(&plan, "multicast"), Some(48));
        // 未知の 8 本目 (8 バイト) の分だけ後続が動く
        assert_eq!(offset_of(&plan, "speed"), Some(64));
        assert_eq!(offset_of(&plan, "interface"), Some(68));
        assert_eq!(offset_of(&plan, "duplex"), Some(84));
        assert_eq!(plan.unavailable_fields(), 0);
    }

    /// 完全一致する revision があるときは、その配置をそのまま使う。
    #[test]
    fn exact_types_nr_keeps_the_declared_layout() {
        let plan = plan_for(
            ActivityId::NET_DEV,
            DeclaredShape {
                magic: Some(0x8d),
                size: 80,
                types_nr: Some([7, 0, 1]),
            },
            1,
        );
        assert_eq!(offset_of(&plan, "speed"), Some(56));
        assert_eq!(offset_of(&plan, "interface"), Some(60));
        assert_eq!(offset_of(&plan, "duplex"), Some(76));
    }

    /// 申告個数が既知フィールド数より少ないグループでは、末尾が欠落する。
    #[test]
    fn fewer_declared_fields_become_missing() {
        let plan = plan_for(
            ActivityId::CPU,
            DeclaredShape {
                magic: Some(0x8b),
                size: 80,
                types_nr: Some([5, 0, 0]),
            },
            1,
        );
        assert_eq!(offset_of(&plan, "cpu_user"), Some(0));
        assert_eq!(offset_of(&plan, "cpu_iowait"), Some(32));
        // 6 本目以降はこのファイルに存在しない (0 ではなく未提供)
        assert_eq!(offset_of(&plan, "cpu_steal"), None);
        assert_eq!(plan.unavailable_fields(), 5);

        let buf = vec![0xffu8; 80];
        let cur = Cursor::new(&buf, Endian::Little);
        let item = plan.item_view(&cur, 0).unwrap();
        let mut out = Vec::new();
        plan.decode_item_into(&item, &mut out).unwrap();
        assert_eq!(out[4], Availability::Present(u64::MAX));
        assert_eq!(out[5], Availability::UnsupportedBySource);
    }

    /// 文字列より前にある int 群が減ると、文字列の位置も前へ動く。
    #[test]
    fn declared_types_nr_moves_text_fields_too() {
        let plan = plan_for(
            ActivityId::NET_DEV,
            DeclaredShape {
                magic: Some(0x8d),
                size: 80,
                types_nr: Some([7, 0, 0]),
            },
            1,
        );
        // speed (int 1 本目) が無い世代では interface が 56 に来る
        assert_eq!(offset_of(&plan, "speed"), None);
        assert_eq!(offset_of(&plan, "interface"), Some(56));
        assert_eq!(offset_of(&plan, "duplex"), Some(72));

        let mut buf = vec![0u8; 80];
        buf[56..60].copy_from_slice(b"eth0");
        let cur = Cursor::new(&buf, Endian::Little);
        let item = plan.item_view(&cur, 0).unwrap();
        assert_eq!(plan.read_item_key(&item), Ok(Some("eth0")));
    }

    /// 登録済みの全 revision が 8/8/4 スロットモデルの物理順に並んでいること。
    ///
    /// 並んでいない revision があると [`remap_to_declared`] が並べ直しを
    /// あきらめる (既知配置のままになる) ので、機械的に検査する。
    #[test]
    fn all_revisions_declare_fields_in_physical_order() {
        for def in registry::all() {
            for rev in def.revisions {
                let mut prev = 0usize;
                for f in rev.layout.fields {
                    let g = group_index(f.ty);
                    assert!(
                        g >= prev,
                        "{} (magic=0x{:x}): {} が型グループの物理順に反している",
                        def.id.display_name(),
                        rev.magic,
                        f.name
                    );
                    prev = g;
                }
            }
        }
    }

    /// 型別個数から並べ直した配置は、その revision の申告サイズを超えないこと。
    ///
    /// スロットモデルはパディングを詰めるので、時代 A の `aligned(16)` 付き
    /// revision では並べ直した方が小さくなる。**超えないこと**が安全性の要点
    /// (超えるとフィールドが item の外へ出る)。
    #[test]
    fn remapped_layouts_never_exceed_the_declared_size() {
        for def in registry::all() {
            for rev in def.revisions {
                let remapped = remap_to_declared(rev, rev.types_nr, &lp64())
                    .expect("並べ直しでエラーにならない")
                    .expect("物理順に並んでいる");
                assert!(
                    remapped.size <= rev.size_lp64,
                    "{} (magic=0x{:x}): 並べ直したサイズ {} が宣言値 {} を超える",
                    def.id.display_name(),
                    rev.magic,
                    remapped.size,
                    rev.size_lp64
                );
            }
        }
    }

    /// 現行世代 (自己記述形式) の revision は、並べ直しても同じ配置になること。
    ///
    /// 並べ直しの経路と既知配置の経路が食い違っていたら、
    /// `types_nr` が 1 つ変わった瞬間に全フィールドの位置がずれる。
    #[test]
    fn remap_reproduces_the_current_layout_for_self_describing_revisions() {
        for def in registry::all() {
            // 自己記述形式の世代で使われるのは各 activity の最新 revision。
            let Some(rev) = def.latest() else { continue };
            let plain = rev.layout.resolve(&lp64()).expect("解決できる");
            let remapped = remap_to_declared(rev, rev.types_nr, &lp64())
                .expect("並べ直しでエラーにならない")
                .expect("物理順に並んでいる");
            for f in plain.fields.iter() {
                let r = remapped
                    .field(f.name)
                    .unwrap_or_else(|| panic!("{} が並べ直し後に無い", f.name));
                assert_eq!(
                    (r.offset, r.width),
                    (f.offset, f.width),
                    "{} (magic=0x{:x}): {} の位置が既知配置と食い違う",
                    def.id.display_name(),
                    rev.magic,
                    f.name
                );
            }
        }
    }
}

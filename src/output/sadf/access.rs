//! 出力層から `series` 層の計算結果を引くためのアクセサ。
//!
//! **ここで値を計算しない。** 表示値は [`crate::series::compute::column_value`] が
//! 返したものだけを使い、出力層は書式化に専念する。
//! 形式ごとに計算がずれる事故を層の分離で防ぐのが目的
//! (`docs/design.md` §2 / §5)。
//!
//! 置き場所について: `src/output/mod.rs` は `sar_text` と共有されるため、
//! 共通ヘルパは `sadf` 配下に置いて独自出力側 (`table` / `json` / `csv` /
//! `ndjson`) から参照する。

use crate::layout::plan::DecodePlan;
use crate::layout::registry::{ActivityDef, ColumnMeta, ItemShape};
use crate::model::{ActivityId, Availability};
use crate::series::compute::{
    ComputeContext, ComputeIssue, Computed, column_value, column_value_strict, tick_total,
};
use crate::series::{ActivitySnapshot, IntervalView, ItemSnapshot};

/// activity の「前後 1 対」。
///
/// 前サンプルに同じ item が無い場合は空の item を指すため、
/// `series` 層が不連続 (`Discontinuity::ItemReplaced` 相当) を判定できる。
#[derive(Debug)]
pub struct ActivityPair<'a> {
    pub id: ActivityId,
    pub def: &'static ActivityDef,
    pub plan: &'a DecodePlan,
    pub curr: &'a ActivitySnapshot,
    pub prev: Option<&'a ActivitySnapshot>,
    pub itv_cs: u64,
    pub has_prev: bool,
    pub continuous: bool,
}

/// 空の item。前サンプルに対応する item が無いときの相手役。
static EMPTY_ITEM: std::sync::LazyLock<ItemSnapshot> =
    std::sync::LazyLock::new(ItemSnapshot::default);

impl<'a> ActivityPair<'a> {
    /// `IntervalView` から 1 activity 分を取り出す。
    pub fn from_view(view: &'a IntervalView<'a>, id: ActivityId) -> Option<Self> {
        let curr = view.curr.activity(id)?;
        let def = crate::layout::registry::lookup(id)?;
        let plan = view.plan_for(id)?;
        Some(Self {
            id,
            def,
            plan,
            curr,
            prev: view.prev.activity(id),
            itv_cs: view.itv_cs,
            has_prev: view.has_prev,
            continuous: view.continuous,
        })
    }

    /// この activity の item 数。
    pub fn len(&self) -> usize {
        self.curr.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.curr.items.is_empty()
    }

    /// item を 1 個取り出す。
    ///
    /// 前サンプル側は**識別子で**対応付ける。デバイスの着脱で配列位置が
    /// ずれるため、位置で突き合わせると別デバイスの値を引いてしまう。
    /// 識別子を持たない activity (CPU / センサ) は位置で対応付ける。
    pub fn item(&self, index: usize) -> Option<ItemPair<'a>> {
        self.item_indexed(index, index)
    }

    /// 値を引く位置と、ラベルに使う添字を別に指定して item を取り出す。
    ///
    /// 行列型 (`A_PWR_FREQ`) では「1 行 = 1 CPU」を 1 アイテムとして出すため、
    /// データ位置 (`row * nr2`) とラベル添字 (`row`) が食い違う。
    pub fn item_indexed(&self, index: usize, label_index: usize) -> Option<ItemPair<'a>> {
        let curr = self.curr.items.get(index)?;
        let prev = match (&curr.key, self.prev) {
            (Some(key), Some(p)) => p.item_by_key(key),
            (None, Some(p)) => p.items.get(index),
            _ => None,
        };
        // 前サンプルに相手がいない = 差分が取れない。0 で埋めずに不連続として扱う。
        let matched = prev.is_some();
        let prev = prev.unwrap_or(&EMPTY_ITEM);

        let mut ctx = ComputeContext::new(self.itv_cs);
        ctx.has_prev = self.has_prev && matched;
        ctx.continuous = self.continuous && matched;
        // 集約 item (CPU の `all` 行 / A_IRQ の合計列) は逆行クランプの扱いが違う
        ctx.aggregate_item = match self.def.shape {
            ItemShape::Matrix => index.is_multiple_of(self.curr.nr2.max(1) as usize),
            _ => label_index == 0,
        };
        // A_CPU の割合はグローバル itv ではなく、その CPU の tick 合計で正規化する
        if self.id == ActivityId::CPU && ctx.has_prev {
            ctx.tick_total = Some(tick_total(prev, curr));
        }

        Some(ItemPair {
            index: label_index,
            def: self.def,
            plan: self.plan,
            prev,
            curr,
            ctx,
        })
    }

    /// 全 item を順に返す。
    pub fn items(&self) -> impl Iterator<Item = ItemPair<'a>> + '_ {
        (0..self.len()).filter_map(|i| self.item(i))
    }

    /// 出力に現れる単位で item を返す。
    ///
    /// 行列型は「行 (= CPU) ごとに 1 アイテム」。`A_IRQ` だけは
    /// 「割り込み × CPU」の逆転構造なので専用経路で扱い、ここでは返さない。
    pub fn output_items(&self) -> Vec<ItemPair<'a>> {
        if self.def.shape != ItemShape::Matrix {
            return self.items().collect();
        }
        if self.id == ActivityId::IRQ {
            return Vec::new();
        }
        let nr2 = self.curr.nr2.max(1) as usize;
        (0..self.curr.nr as usize)
            .filter_map(|row| self.item_indexed(row * nr2, row))
            .collect()
    }

    /// 行列型 (`A_IRQ`) の (行, 列) 添字から item を引く。
    ///
    /// 要素の並びは `行 * nr2 + 列`。
    pub fn matrix_item(&self, row: usize, col: usize) -> Option<ItemPair<'a>> {
        if self.def.shape != ItemShape::Matrix {
            return None;
        }
        let nr2 = self.curr.nr2.max(1) as usize;
        self.item(row * nr2 + col)
    }
}

/// item 1 個分のアクセサ。
#[derive(Debug)]
pub struct ItemPair<'a> {
    /// activity 内での item 添字。
    pub index: usize,
    pub def: &'static ActivityDef,
    pub plan: &'a DecodePlan,
    pub prev: &'a ItemSnapshot,
    pub curr: &'a ItemSnapshot,
    pub ctx: ComputeContext,
}

impl<'a> ItemPair<'a> {
    /// 識別子 (デバイス名など)。
    pub fn key(&self) -> Option<&'a str> {
        self.curr.key.as_deref()
    }

    /// 公開名から列添字を引く。
    pub fn column_index(&self, public_name: &str) -> Option<usize> {
        self.def
            .columns
            .iter()
            .position(|c| c.public_name == public_name)
    }

    pub fn column_meta(&self, column: usize) -> Option<&'static ColumnMeta> {
        self.def.columns.get(column)
    }

    /// 表示値。`series` 層の計算結果をそのまま返す。
    pub fn computed(&self, column: usize) -> Computed {
        let Some(meta) = self.def.columns.get(column) else {
            return Err(ComputeIssue::UnsupportedBySource);
        };
        column_value(
            self.def.id,
            column,
            meta,
            self.plan,
            self.prev,
            self.curr,
            &self.ctx,
        )
    }

    /// 表示値 (**厳密モード**)。
    ///
    /// 互換出力用の [`ItemPair::computed`] は、欠落したフィールドを本家と同じ規則で
    /// 代替して埋める (旧世代の `%memused` を `frmkb` から出す等)。
    /// 独自出力と集計では嘘の値を出さないことが優先なので、
    /// 欠落は欠落のまま返すこちらを使う。
    pub fn computed_strict(&self, column: usize) -> Computed {
        let Some(meta) = self.def.columns.get(column) else {
            return Err(ComputeIssue::UnsupportedBySource);
        };
        column_value_strict(
            self.def.id,
            column,
            meta,
            self.plan,
            self.prev,
            self.curr,
            &self.ctx,
        )
    }

    /// 公開名で表示値を引く。列が無ければ「その世代に無い」として返す。
    pub fn computed_by_name(&self, public_name: &str) -> Computed {
        match self.column_index(public_name) {
            Some(i) => self.computed(i),
            None => Err(ComputeIssue::UnsupportedBySource),
        }
    }

    /// 現サンプルの生値。
    pub fn raw_curr(&self, column: usize) -> Availability<u64> {
        self.plan.column_value(&self.curr.values, column)
    }

    /// 前サンプルの生値。
    pub fn raw_prev(&self, column: usize) -> Availability<u64> {
        if !self.ctx.has_prev {
            return Availability::MissingInSample;
        }
        self.plan.column_value(&self.prev.values, column)
    }

    pub fn raw_curr_by_name(&self, public_name: &str) -> Availability<u64> {
        match self.column_index(public_name) {
            Some(i) => self.raw_curr(i),
            None => Availability::UnsupportedBySource,
        }
    }

    pub fn raw_prev_by_name(&self, public_name: &str) -> Availability<u64> {
        match self.column_index(public_name) {
            Some(i) => self.raw_prev(i),
            None => Availability::UnsupportedBySource,
        }
    }

    /// 複数列の生値の合計 (`A_CPU` の `%system` 用)。
    ///
    /// 1 つでも欠けていたら合計を作らない。0 で補うと「動いていない CPU」と
    /// 区別できなくなる。
    pub fn raw_sum(&self, names: &[&str], previous: bool) -> Availability<u64> {
        let mut total: u64 = 0;
        for n in names {
            let v = if previous {
                self.raw_prev_by_name(n)
            } else {
                self.raw_curr_by_name(n)
            };
            match v {
                Availability::Present(x) => total = total.wrapping_add(x),
                other => return other,
            }
        }
        Availability::Present(total)
    }

    /// 2 列の生値の差 (`A_CPU ALL` の `%usr` = `cpu_user - cpu_guest` 用)。
    pub fn raw_diff(&self, a: &str, b: &str, previous: bool) -> Availability<u64> {
        let (x, y) = if previous {
            (self.raw_prev_by_name(a), self.raw_prev_by_name(b))
        } else {
            (self.raw_curr_by_name(a), self.raw_curr_by_name(b))
        };
        match (x, y) {
            (Availability::Present(x), Availability::Present(y)) => {
                Availability::Present(x.wrapping_sub(y))
            }
            (Availability::Present(_), other) | (other, _) => other,
        }
    }

    /// 列の文字列値。
    ///
    /// 1 item が文字列フィールドを複数持つ activity (`A_PWR_USB` の
    /// `manufact`/`product`、`A_FS` の `fs_name`/`mountp`) でも正しく引ける。
    /// その世代に無いフィールドや空文字は `None` (空文字で埋めない)。
    pub fn text(&self, public_name: &str) -> Option<&'a str> {
        self.text_by_wire(self.column_wire_name(public_name)?)
    }

    /// wire フィールド名で文字列値を引く。
    ///
    /// 名前の照合先は `DecodePlan::text_fields` で、1 item あたり数個しかない。
    /// ここは 1 フィールド 1 文字列を生成する書式化経路なので、
    /// 名前照合のコストは無視できる。
    pub fn text_by_wire(&self, wire_name: &str) -> Option<&'a str> {
        if wire_name.is_empty() {
            return None;
        }
        let i = self.plan.text_index(wire_name)?;
        self.curr.text(i)
    }

    /// その列がファイル上の文字列フィールドか。
    ///
    /// 世代によってフィールドが無いこともあるので、レイアウト記述ではなく
    /// **このファイルのデコード計画**に問い合わせる。
    pub fn is_text_column(&self, public_name: &str) -> bool {
        match self.column_wire_name(public_name) {
            Some(w) if !w.is_empty() => self.plan.text_index(w).is_some(),
            _ => false,
        }
    }

    fn column_wire_name(&self, public_name: &str) -> Option<&'static str> {
        self.def
            .columns
            .iter()
            .find(|c| c.public_name == public_name)
            .map(|c| c.wire_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Availability;

    fn item(values: &[u64]) -> ItemSnapshot {
        ItemSnapshot {
            key: None,
            texts: Vec::new(),
            values: values.iter().map(|v| Availability::Present(*v)).collect(),
        }
    }

    /// 欠落を 0 で埋めずに伝播すること。
    #[test]
    fn raw_sum_propagates_absence() {
        let def = crate::layout::registry::lookup(ActivityId::CPU).unwrap();
        let rev = def.latest().unwrap();
        let enc = crate::format::abi::SourceEncoding::new(
            crate::format::abi::Endian::Little,
            crate::format::abi::LayoutAbi::LP64,
        );
        let plan = DecodePlan::build(def, rev, rev.size_lp64, 1, 1, &enc).unwrap();

        let curr = item(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
        let prev = item(&[0; 10]);
        let pair = ItemPair {
            index: 0,
            def,
            plan: &plan,
            prev: &prev,
            curr: &curr,
            ctx: ComputeContext::new(100),
        };

        // sys + irq + soft = 3 + 7 + 8
        assert_eq!(
            pair.raw_sum(&["sys", "irq", "soft"], false),
            Availability::Present(18)
        );
        // 存在しない列が混ざると合計を作らない
        assert_eq!(
            pair.raw_sum(&["sys", "no_such_column"], false),
            Availability::UnsupportedBySource
        );
    }
}

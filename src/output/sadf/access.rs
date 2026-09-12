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
    self, ComputeContext, ComputeIssue, Computed, SadfUnitColumn, column_value,
    column_value_strict, matrix_row_values, matrix_row_values_strict, sadf_unit_value, tick_total,
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
///
/// `values` が空なので [`DecodePlan::column_value`] は
/// [`Availability::MissingInSample`] を返す。0 で埋めた item ではないので、
/// これを相手に差分を取ると「差分が取れない」として伝播する。
static EMPTY_ITEM: std::sync::LazyLock<ItemSnapshot> =
    std::sync::LazyLock::new(ItemSnapshot::default);

/// 行列型 activity の「出力 1 行ぶんのスロット群」。
///
/// `A_PWR_FREQ` の `wghMHz` (03 §id=35) のように、1 スロットだけでは値を出せず
/// 行全体の滞在時間差分を要する列があるため、行を丸ごと運ぶ。
/// 並びは出力に現れる順で、計算は [`matrix_row_values`] に渡す。
#[derive(Debug)]
pub struct MatrixRow<'a> {
    /// 前サンプルのスロット。対応が無いスロットは [`EMPTY_ITEM`]。
    pub prev: Vec<&'a ItemSnapshot>,
    /// 現サンプルのスロット。
    pub curr: Vec<&'a ItemSnapshot>,
}

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
        let (prev, curr, matched) = self.slot(index)?;
        Some(ItemPair {
            index: label_index,
            def: self.def,
            plan: self.plan,
            prev,
            curr,
            ctx: self.slot_context(index, label_index, matched, prev, curr),
            row: None,
        })
    }

    /// 保存位置から前後 1 スロットを取り出す。
    ///
    /// 戻り値の 3 番目は「前サンプルに相手が居たか」。
    /// 前サンプル側は**識別子で**対応付ける (位置で突き合わせるとデバイスの
    /// 着脱で別デバイスの値を引く)。識別子を持たないスロットは位置で対応付ける。
    fn slot(&self, index: usize) -> Option<(&'a ItemSnapshot, &'a ItemSnapshot, bool)> {
        let curr = self.curr.items.get(index)?;
        let prev = match (&curr.key, self.prev) {
            (Some(key), Some(p)) => p.item_by_key(key),
            (None, Some(p)) => p.items.get(index),
            _ => None,
        };
        // 前サンプルに相手がいない = 差分が取れない。0 で埋めずに不連続として扱う。
        let matched = prev.is_some();
        Some((prev.unwrap_or(&EMPTY_ITEM), curr, matched))
    }

    /// 1 スロット分の計算コンテキスト。
    fn slot_context(
        &self,
        index: usize,
        label_index: usize,
        matched: bool,
        prev: &ItemSnapshot,
        curr: &ItemSnapshot,
    ) -> ComputeContext {
        let mut ctx = ComputeContext::new(self.itv_cs);
        ctx.has_prev = self.has_prev && matched;
        ctx.continuous = self.continuous && matched;
        ctx.aggregate_item = self.is_aggregate_slot(index, label_index);
        // A_CPU の割合はグローバル itv ではなく、その CPU の tick 合計で正規化する
        if self.id == ActivityId::CPU && ctx.has_prev {
            // guest / guest_nice を分母に入れないため、列を特定する plan が必要
            ctx.tick_total = Some(tick_total(self.plan, prev, curr));
        }
        ctx
    }

    /// このスロットは集約 item か (逆行クランプの扱いが変わる)。
    ///
    /// 保存位置は `A_IRQ` / `A_PWR_FREQ` のどちらも `行 * nr2 + 列` だが、
    /// **どちらが集約かは activity で違う**:
    ///
    /// | activity | 保存形 | 集約スロット | 典拠 |
    /// |---|---|---|---|
    /// | `A_IRQ` | 行 = CPU / 列 = 割り込み | CPU 行 0 (`all`) = `index < nr2` | 03 §id=3 |
    /// | `A_PWR_FREQ` | 行 = CPU / 列 = 周波数 | 行頭スロット | (クランプ対象外) |
    ///
    /// 以前は両方を `index % nr2 == 0` で判定しており、`A_IRQ` では
    /// 「割り込み 0 の列」を全 CPU ぶん集約扱いしていた。逆行クランプ
    /// (`curr < prev` で 0) が本来 `all` 列だけに効くところで
    /// CPU 別の列にも効いてしまう。
    fn is_aggregate_slot(&self, index: usize, label_index: usize) -> bool {
        let nr2 = self.curr.nr2.max(1) as usize;
        match self.def.shape {
            ItemShape::Matrix if self.id == ActivityId::IRQ => index < nr2,
            ItemShape::Matrix => index.is_multiple_of(nr2),
            _ => label_index == 0,
        }
    }

    /// 全 item を順に返す。
    pub fn items(&self) -> impl Iterator<Item = ItemPair<'a>> + '_ {
        (0..self.len()).filter_map(|i| self.item(i))
    }

    /// 出力に現れる単位で item を返す。
    ///
    /// 行列型は「出力 1 行」を 1 アイテムとして返す
    /// ([`ActivityPair::matrix_row`])。行の中の全スロットを
    /// [`MatrixRow`] として持つので、`wghMHz` のように行全体を要する列も計算できる。
    ///
    /// `A_IRQ` も**ここから返す**。`sadf` の `-d`/`-p`/`-j`/`-x`/`-r` は
    /// 「フィールド名の位置に CPU ラベルを入れる」逆転構造を再現する専用経路を
    /// 持つので (03 §11.1) この列挙を通らないが、独自出力 (`table` / `json` /
    /// `csv` / `ndjson`) は専用分岐を持たない。ここで空を返すと
    /// **観測済みの割り込み統計が独自出力から丸ごと消える**。
    pub fn output_items(&self) -> Vec<ItemPair<'a>> {
        if self.def.shape != ItemShape::Matrix {
            return self.items().collect();
        }
        (0..self.output_row_count())
            .filter_map(|row| self.matrix_row(row))
            .collect()
    }

    /// **本家互換出力**に出す item を返す。
    ///
    /// [`output_items`](Self::output_items) から**未使用の枠を落とす**。
    /// `file_activity.nr` は採取時に確保した枠数なので、枠が余っていると
    /// 名前の無いデバイスや `dev0-0` が行になる。本家の
    /// `render_*()` / `json_print_*()` / `xml_print_*()` はどれも
    /// activity 固有の番兵を見て `continue` する
    /// (判定表は [`compute::is_unused_item`] の doc)。
    ///
    /// **独自出力 (`table` / `json` / `csv` / `ndjson`) はこれを使わない。**
    /// 空き枠も観測結果として出す方が、「本家が表示を省く規則」を
    /// 独自形式に持ち込むより説明しやすい (`docs/design.md` §3.8)。
    pub fn compat_items(&self) -> Vec<ItemPair<'a>> {
        self.output_items()
            .into_iter()
            .filter(|it| !compute::is_unused_item(self.id, it.index, self.plan, it.curr))
            .collect()
    }

    /// 行列型の出力行数。
    ///
    /// 転置する `A_IRQ` は「1 割り込み × 全 CPU」が 1 行なので `nr2` 行、
    /// それ以外 (`A_PWR_FREQ`、および 1 次元世代の `A_IRQ`) は `nr` 行。
    fn output_row_count(&self) -> usize {
        if self.def.shape != ItemShape::Matrix {
            return self.len();
        }
        let n = if self.irq_is_transposed() {
            self.curr.nr2
        } else {
            self.curr.nr
        };
        n as usize
    }

    /// `A_IRQ` が「行 = CPU / 列 = 割り込み」の 2 次元で保存されているか。
    ///
    /// 現行 (magic `0x8c`、v12.5.6〜) は 2 次元で、`nr` = CPU 数 + 1 /
    /// `nr2` = 割り込み数。出力は「行 = 割り込み」に転置する (§6.1)。
    ///
    /// v11.7.2〜v12.5.5 の `A_IRQ` は `nr` = 割り込み数 / `nr2` = 1 の
    /// **1 次元**で、割り込み名も持たない (§6.4)。転置すると 1 行に
    /// 全割り込みを詰め込むことになるので、この世代は転置しない。
    ///
    /// 判定材料は `nr2 > 1` と「このファイルに `irq_name` があるか」。
    /// どちらかが成り立てば 2 次元である
    /// (1 次元世代は `nr2` が必ず 1 で `irq_name` を持たない)。
    fn irq_is_transposed(&self) -> bool {
        self.id == ActivityId::IRQ
            && (self.curr.nr2 > 1 || self.plan.text_index("irq_name").is_some())
    }

    /// 行列型 activity の「出力 1 行」を組み立てる。
    ///
    /// 保存位置はどちらも `CPU 行 * nr2 + 列` (§6.1) だが、
    /// 出力の 1 行がどちらの軸かは activity で違う:
    ///
    /// | activity | 出力 1 行 | スロットの保存位置 | 代表スロット |
    /// |---|---|---|---|
    /// | `A_IRQ` (2 次元) | 1 割り込み × 全 CPU (転置) | `cpu * nr2 + row` | `row` (CPU `all`) |
    /// | `A_PWR_FREQ` | 1 CPU × 全周波数ステップ | `row * nr2 + k` | `row * nr2` |
    ///
    /// 代表スロットは「その行を 1 値で代表させる位置」で、
    /// 生値・文字列 (割り込み名は CPU 行 0 にしか無い、§6.2) もここから引く。
    pub fn matrix_row(&self, row: usize) -> Option<ItemPair<'a>> {
        if self.def.shape != ItemShape::Matrix {
            return self.item(row);
        }
        let nr2 = self.curr.nr2.max(1) as usize;
        let nr = self.curr.nr as usize;
        let transposed = self.irq_is_transposed();
        let slots = if transposed { nr } else { nr2 };
        let at = |slot: usize| {
            if transposed {
                slot * nr2 + row
            } else {
                row * nr2 + slot
            }
        };

        let mut prev = Vec::with_capacity(slots);
        let mut curr = Vec::with_capacity(slots);
        let mut head_matched = false;
        for slot in 0..slots {
            let Some((p, c, matched)) = self.slot(at(slot)) else {
                break;
            };
            if slot == 0 {
                head_matched = matched;
            }
            prev.push(p);
            curr.push(c);
        }
        // 行に 1 スロットも無ければ行そのものが無い
        let (&head_prev, &head_curr) = (prev.first()?, curr.first()?);

        Some(ItemPair {
            index: row,
            def: self.def,
            plan: self.plan,
            prev: head_prev,
            curr: head_curr,
            ctx: self.slot_context(at(0), row, head_matched, head_prev, head_curr),
            row: Some(MatrixRow { prev, curr }),
        })
    }

    /// 行列型 (`A_IRQ`) の (行, 列) 添字から item を引く。
    ///
    /// 要素の並びは `行 * nr2 + 列`。`A_IRQ` では 行 = CPU / 列 = 割り込み。
    /// 1 スロットだけを見るので、行全体を要する列 (`wghMHz`) は計算できない。
    pub fn matrix_item(&self, row: usize, col: usize) -> Option<ItemPair<'a>> {
        if self.def.shape != ItemShape::Matrix {
            return None;
        }
        let nr2 = self.curr.nr2.max(1) as usize;
        self.item(row * nr2 + col)
    }
}

/// item 1 個分のアクセサ。
///
/// 行列型 activity の「出力 1 行」を表す場合は [`ItemPair::row`] が
/// `Some` になり、`prev` / `curr` はその行の代表スロットを指す。
#[derive(Debug)]
pub struct ItemPair<'a> {
    /// activity 内での item 添字。
    pub index: usize,
    pub def: &'static ActivityDef,
    pub plan: &'a DecodePlan,
    pub prev: &'a ItemSnapshot,
    pub curr: &'a ItemSnapshot,
    pub ctx: ComputeContext,
    /// 行列型の出力 1 行ぶんのスロット群 (行でない場合は `None`)。
    pub row: Option<MatrixRow<'a>>,
}

impl<'a> ItemPair<'a> {
    /// 単一 item のアクセサを組み立てる (行列型の行ではない)。
    ///
    /// 構造体リテラルで書くと [`ItemPair::row`] の指定を忘れやすいので、
    /// 行を持たない場合はこちらを使う。
    pub fn single(
        index: usize,
        def: &'static ActivityDef,
        plan: &'a DecodePlan,
        prev: &'a ItemSnapshot,
        curr: &'a ItemSnapshot,
        ctx: ComputeContext,
    ) -> Self {
        Self {
            index,
            def,
            plan,
            prev,
            curr,
            ctx,
            row: None,
        }
    }

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
        if self.row.is_some() {
            return first_of(self.row_values(column));
        }
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

    /// 行列型の出力 1 行ぶんの値。
    ///
    /// 戻り値の長さは activity と列で変わる (`series` 層の
    /// [`matrix_row_values`] のとおり):
    ///
    /// - `A_PWR_FREQ` の `wghMHz` は行全体で 1 値 (03 §id=35)
    /// - `A_IRQ` の `intr` は CPU スロットごとに 1 値 (03 §id=3)
    ///
    /// 行でない item に対しては単一要素になる。
    pub fn row_values(&self, column: usize) -> Vec<Computed> {
        match &self.row {
            Some(row) => matrix_row_values(
                self.def.id,
                column,
                self.plan,
                &row.prev,
                &row.curr,
                &self.ctx,
            ),
            None => vec![self.computed(column)],
        }
    }

    /// [`ItemPair::row_values`] の**厳密モード** (欠落を埋めない)。
    pub fn row_values_strict(&self, column: usize) -> Vec<Computed> {
        match &self.row {
            Some(row) => matrix_row_values_strict(
                self.def.id,
                column,
                self.plan,
                &row.prev,
                &row.curr,
                &self.ctx,
            ),
            None => vec![self.computed_strict(column)],
        }
    }

    /// 行列型の出力 1 行に含まれるスロット数 (行でなければ 1)。
    pub fn row_len(&self) -> usize {
        self.row.as_ref().map_or(1, |r| r.curr.len())
    }

    /// `sadf` にしか無い「別単位の列」の表示値 (03 §9.6-2 / §9.6-10 / §1.8.1)。
    ///
    /// 換算は計算層 ([`sadf_unit_value`]) の担当。出力層で 512 や 1024 を
    /// 掛け直すと、同じ指標の `sar` 互換テキストと値がずれる。
    /// 互換出力専用なので厳密モードは持たない。独自出力は同じ指標を
    /// 計算層の単位 (バイト/秒など) でそのまま出す。
    pub fn sadf_unit(&self, variant: SadfUnitColumn) -> Computed {
        sadf_unit_value(variant, self.plan, self.prev, self.curr, &self.ctx)
    }

    /// 表示値 (**厳密モード**)。
    ///
    /// 互換出力用の [`ItemPair::computed`] は、欠落したフィールドを本家と同じ規則で
    /// 代替して埋める (旧世代の `%memused` を `frmkb` から出す等)。
    /// 独自出力と集計では嘘の値を出さないことが優先なので、
    /// 欠落は欠落のまま返すこちらを使う。
    pub fn computed_strict(&self, column: usize) -> Computed {
        if self.row.is_some() {
            return first_of(self.row_values_strict(column));
        }
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

/// 行の値を 1 値へ畳む。
///
/// 行全体で 1 値の列 (`wghMHz`) はそれを、スロットごとに値を持つ列
/// (`A_IRQ` の `intr`) は代表スロット (CPU `all`) の値を返す。
/// 行が空なら「item 群が必要」= 呼び出し方の問題として返す。
fn first_of(values: Vec<Computed>) -> Computed {
    values
        .into_iter()
        .next()
        .unwrap_or(Err(ComputeIssue::NeedsItemGroup))
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
            row: None,
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

    // ========================================================================
    // 行列型 activity (指摘 3 / 指摘 4)
    // ========================================================================

    fn plan_for(id: ActivityId, nr: u32, nr2: u32) -> DecodePlan {
        let def = crate::layout::registry::lookup(id).expect("定義がある");
        let rev = def.latest().expect("revision がある");
        let enc = crate::format::abi::SourceEncoding::new(
            crate::format::abi::Endian::Little,
            crate::format::abi::LayoutAbi::LP64,
        );
        DecodePlan::build(def, rev, rev.size_lp64, nr, nr2, &enc).expect("計画を作れる")
    }

    fn zeros(plan: &DecodePlan) -> ItemSnapshot {
        ItemSnapshot {
            key: None,
            texts: Vec::new(),
            values: vec![Availability::Present(0); plan.fields.len()],
        }
    }

    /// 列添字で値を入れる (`series` 層のテストの `put` と同じ)。
    fn put(plan: &DecodePlan, item: &mut ItemSnapshot, column: usize, v: u64) {
        if let Some(Some(id)) = plan.column_fields.get(column)
            && let Some(slot) = item.values.get_mut(id.index())
        {
            *slot = Availability::Present(v);
        }
    }

    fn snapshot(id: ActivityId, nr: u32, nr2: u32, items: Vec<ItemSnapshot>) -> ActivitySnapshot {
        ActivitySnapshot {
            id,
            index: 0,
            nr,
            nr2,
            items,
        }
    }

    fn pair_of<'a>(
        id: ActivityId,
        plan: &'a DecodePlan,
        prev: &'a ActivitySnapshot,
        curr: &'a ActivitySnapshot,
        itv_cs: u64,
    ) -> ActivityPair<'a> {
        ActivityPair {
            id,
            def: crate::layout::registry::lookup(id).unwrap(),
            plan,
            curr,
            prev: Some(prev),
            itv_cs,
            has_prev: true,
            continuous: true,
        }
    }

    /// **回帰テスト (指摘 4)**: `wghMHz` が行全体から計算されること。
    ///
    /// 以前は `output_items()` が各 CPU 行の先頭スロットだけを
    /// [`ItemPair`] にしていたため、単一 item 計算が `NeedsItemGroup` を返し、
    /// `weighted-frequency` が JSON で `null`・テキストで空欄になっていた。
    #[test]
    fn weighted_frequency_comes_from_the_whole_row() {
        use crate::series::compute::freq_col;

        // 1 CPU × 2 周波数ステップ
        let plan = plan_for(ActivityId::PWR_FREQ, 1, 2);
        let (mut p0, mut p1) = (zeros(&plan), zeros(&plan));
        let (mut c0, mut c1) = (zeros(&plan), zeros(&plan));
        put(&plan, &mut p0, freq_col::TIME_IN_STATE, 0);
        put(&plan, &mut c0, freq_col::FREQ_KHZ, 1_500_999);
        put(&plan, &mut c0, freq_col::TIME_IN_STATE, 100);
        put(&plan, &mut p1, freq_col::TIME_IN_STATE, 0);
        put(&plan, &mut c1, freq_col::FREQ_KHZ, 800_000);
        put(&plan, &mut c1, freq_col::TIME_IN_STATE, 300);

        let prev = snapshot(ActivityId::PWR_FREQ, 1, 2, vec![p0, p1]);
        let curr = snapshot(ActivityId::PWR_FREQ, 1, 2, vec![c0, c1]);
        let pair = pair_of(ActivityId::PWR_FREQ, &plan, &prev, &curr, 400);

        let rows = pair.output_items();
        assert_eq!(rows.len(), 1, "1 CPU = 1 行");
        assert_eq!(rows[0].row_len(), 2, "行は 2 スロット");
        // (1500 × 100 + 800 × 300) / 400。`freq / 1000` は整数除算
        assert_eq!(rows[0].computed(freq_col::WGH_MHZ), Ok(975.0));
        assert_eq!(rows[0].row_values(freq_col::WGH_MHZ).len(), 1, "行で 1 値");
    }

    /// **回帰テスト (指摘 7)**: `A_IRQ` が出力の列挙に現れること。
    ///
    /// 以前は `output_items()` が `A_IRQ` に無条件で空配列を返しており、
    /// 専用分岐を持たない独自出力から割り込み統計が丸ごと消えていた。
    #[test]
    fn irq_rows_are_one_interrupt_each() {
        use crate::series::compute::irq_col;

        // 保存形は 行 = CPU (2) × 列 = 割り込み (2)。出力は転置して 2 行
        let plan = plan_for(ActivityId::IRQ, 2, 2);
        let mut slots = Vec::new();
        // (cpu, irq) = (0,0) (0,1) (1,0) (1,1) の順で 1 秒あたり 100/200/300/400 件
        for (n, delta) in [100u64, 200, 300, 400].into_iter().enumerate() {
            let mut prev = zeros(&plan);
            let mut curr = zeros(&plan);
            put(&plan, &mut prev, irq_col::COUNT, 1_000);
            put(&plan, &mut curr, irq_col::COUNT, 1_000 + delta);
            // 割り込み名は CPU 行 0 にしか入らない (§6.2)
            if n < 2 {
                let name: Box<str> = format!("int{n}").into();
                prev.key = Some(name.clone());
                curr.key = Some(name);
            }
            slots.push((prev, curr));
        }
        let prev = snapshot(
            ActivityId::IRQ,
            2,
            2,
            slots.iter().map(|(p, _)| p.clone()).collect(),
        );
        let curr = snapshot(
            ActivityId::IRQ,
            2,
            2,
            slots.iter().map(|(_, c)| c.clone()).collect(),
        );
        let pair = pair_of(ActivityId::IRQ, &plan, &prev, &curr, 100);

        let rows = pair.output_items();
        assert_eq!(rows.len(), 2, "割り込み 2 本 = 2 行");
        assert_eq!(rows[0].key(), Some("int0"));
        assert_eq!(rows[1].key(), Some("int1"));
        // 行は CPU スロットぶん。代表スロットは CPU "all" 行
        assert_eq!(rows[0].row_len(), 2);
        assert_eq!(rows[0].computed(irq_col::COUNT), Ok(100.0));
        assert_eq!(
            rows[0].row_values(irq_col::COUNT),
            vec![Ok(100.0), Ok(300.0)],
            "CPU all / CPU0 の順"
        );
        assert_eq!(
            rows[1].row_values(irq_col::COUNT),
            vec![Ok(200.0), Ok(400.0)]
        );
    }

    /// `A_IRQ` の集約スロットは CPU 行 0 (`all` 列) であること (03 §id=3)。
    ///
    /// 以前は `index % nr2 == 0` で判定していたため「割り込み 0 の列」が
    /// 全 CPU ぶん集約扱いになり、`all` 列だけに効くはずの逆行クランプが
    /// CPU 別の列にも効いていた。
    #[test]
    fn irq_aggregate_slot_is_the_all_cpu_row() {
        let plan = plan_for(ActivityId::IRQ, 2, 2);
        let items: Vec<ItemSnapshot> = (0..4).map(|_| zeros(&plan)).collect();
        let prev = snapshot(ActivityId::IRQ, 2, 2, items.clone());
        let curr = snapshot(ActivityId::IRQ, 2, 2, items);
        let pair = pair_of(ActivityId::IRQ, &plan, &prev, &curr, 100);

        // 行 = CPU、列 = 割り込み
        assert!(pair.matrix_item(0, 0).unwrap().ctx.aggregate_item);
        assert!(pair.matrix_item(0, 1).unwrap().ctx.aggregate_item);
        assert!(!pair.matrix_item(1, 0).unwrap().ctx.aggregate_item);
        assert!(!pair.matrix_item(1, 1).unwrap().ctx.aggregate_item);
    }
}

//! レコードのデコード結果を保持するスナップショットと、レコード対の走査。
//!
//! カウンタのレートは 2 レコード間の差分から得るため、前後 2 つのサンプルを
//! 同時に持つ必要がある。ここでは 2 つのバッファを入れ替えて使い回し、
//! レコードごとの確保を避ける (本家も同じダブルバッファ方式)。

use crate::error::Result;
use crate::format::file::{ActivitySlice, RawRecord, SaFile, ScanControl, ScanSummary};
use crate::format::reader::Cursor;
use crate::format::registry::RecordKind;
use crate::layout::plan::{DeclaredShape, DecodePlan, select_revision};
use crate::layout::registry::ItemShape;
use crate::model::{ActivityId, Availability};

use super::delta::interval_cs;

/// `file_activity` に activity magic フィールドが無い世代 (`format_magic`)。
///
/// この世代だけは magic による互換性確認ができない (デコード結果は常に 0 になる)。
/// magic を持つ世代と同じ扱いにすると、全 activity が「未知 magic」になってしまう。
const FORMAT_MAGIC_WITHOUT_ACTIVITY_MAGIC: u16 = 0x2170;

/// item 1 個分のデコード結果。
#[derive(Debug, Clone, Default)]
pub struct ItemSnapshot {
    /// 主識別子 (デバイス名・インターフェース名など)。無い activity は `None`。
    pub key: Option<Box<str>>,
    /// 文字列フィールドの値。順序は [`DecodePlan::text_fields`] と同じ。
    ///
    /// `A_PWR_USB` の `manufact` / `product` のように 1 item が複数の文字列を
    /// 持つ activity があるため、`key` 1 本では足りない。
    /// 位置は [`DecodePlan::text_index`] で引く。
    pub texts: Vec<Option<Box<str>>>,
    /// wire フィールドの値 (宣言順)。
    pub values: Vec<Availability<u64>>,
}

impl ItemSnapshot {
    /// 文字列フィールドを位置で引く。
    #[inline]
    pub fn text(&self, index: usize) -> Option<&str> {
        self.texts.get(index).and_then(|t| t.as_deref())
    }
}

/// activity 1 種分のデコード結果。
#[derive(Debug, Clone, Default)]
pub struct ActivitySnapshot {
    pub id: ActivityId,
    /// `SaFile::activities()` の添字。
    pub index: usize,
    pub nr: u32,
    pub nr2: u32,
    pub items: Vec<ItemSnapshot>,
}

impl ActivitySnapshot {
    /// item を識別子で探す。
    ///
    /// デバイスの着脱で配列位置が変わるため、**位置ではなく識別子で対応付ける**。
    pub fn item_by_key(&self, key: &str) -> Option<&ItemSnapshot> {
        self.items.iter().find(|i| i.key.as_deref() == Some(key))
    }
}

/// 1 レコード分のスナップショット。
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    /// 有効な統計レコードを読み込んだか。
    pub valid: bool,
    pub kind: Option<RecordKind>,
    /// レコードの時刻 (エポック秒)。
    pub ust_time: u64,
    /// 1/100 秒単位の稼働時間。取得できない場合は 0。
    pub uptime_cs: u64,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    pub activities: Vec<ActivitySnapshot>,
}

impl Snapshot {
    /// activity を ID で探す。
    pub fn activity(&self, id: ActivityId) -> Option<&ActivitySnapshot> {
        self.activities.iter().find(|a| a.id == id)
    }

    /// メタデータだけを書き換える。
    ///
    /// `activities` は**消さない**。デコード時に上書きし、最後に `truncate` することで
    /// 内側の `Vec` の容量を保ち、レコードごとの再確保を避ける。
    fn reset_meta(&mut self) {
        self.valid = false;
        self.kind = None;
        self.ust_time = 0;
        self.uptime_cs = 0;
        self.hour = 0;
        self.minute = 0;
        self.second = 0;
    }
}

/// レコード対の見え方。
#[derive(Debug)]
pub struct IntervalView<'a> {
    /// 前サンプル。先頭レコードでは `valid == false` の空スナップショット。
    pub prev: &'a Snapshot,
    /// 現サンプル。
    pub curr: &'a Snapshot,
    /// 経過時間 (1/100 秒)。
    ///
    /// 先頭レコードでは `curr.uptime_cs` そのもの (起動からの経過) になる。
    pub itv_cs: u64,
    /// 前サンプルが存在するか。
    ///
    /// `false` のとき、`sar` 互換出力はこのレコードを**表示しない**
    /// (前サンプルとして消費されるだけ)。一方、生値を出す独自出力では使える。
    pub has_prev: bool,
    /// 前サンプルとの間に RESTART が無いか。
    pub continuous: bool,
    /// このレコードの直前に読み込んだイベント (RESTART / COMMENT)。
    ///
    /// 束ねて渡すのは [`walk`] 経路だけで、[`walk_items`] 経路では**常に空**
    /// (イベントは [`WalkItem::Event`] として読んだ順に通知済み)。
    pub events: &'a [RecordEvent],
    /// デコード計画。列メタデータから値を引くのに使う。
    pub plans: &'a [ActivityPlan],
}

impl<'a> IntervalView<'a> {
    /// activity のデコード計画を引く。
    pub fn plan_for(&self, id: ActivityId) -> Option<&'a DecodePlan> {
        self.plans.iter().find(|p| p.id == id).map(|p| &p.plan)
    }
}

/// 統計を伴わないレコード。
#[derive(Debug, Clone)]
pub enum RecordEvent {
    /// 再起動。CPU 数が変わることがある。
    Restart {
        ust_time: u64,
        hour: u8,
        minute: u8,
        second: u8,
        cpu_count: Option<u32>,
    },
    /// コメント。
    Comment {
        ust_time: u64,
        hour: u8,
        minute: u8,
        second: u8,
        text: String,
    },
}

impl RecordEvent {
    pub fn ust_time(&self) -> u64 {
        match self {
            RecordEvent::Restart { ust_time, .. } | RecordEvent::Comment { ust_time, .. } => {
                *ust_time
            }
        }
    }

    pub fn time(&self) -> (u8, u8, u8) {
        match self {
            RecordEvent::Restart {
                hour,
                minute,
                second,
                ..
            }
            | RecordEvent::Comment {
                hour,
                minute,
                second,
                ..
            } => (*hour, *minute, *second),
        }
    }
}

/// デコード対象の activity と、その計画。
///
/// 出力層が「列 → 値」を引くために必要なので公開する。
#[derive(Debug)]
pub struct ActivityPlan {
    /// `SaFile::activities()` の添字。
    pub index: usize,
    pub id: ActivityId,
    pub plan: DecodePlan,
}

/// デコードしなかった activity と、その理由。
///
/// 未知の activity ID や未知の activity magic は**エラーではなくスキップ**する
/// (境界は `file_activity` の申告値から確定しているので、後続のレコードは
/// そのまま読み続けられる)。黙って落とすと「この activity がファイルに無い」のと
/// 区別できないため、理由を添えて返す。
#[derive(Debug, Clone)]
pub struct SkippedActivity {
    /// `SaFile::activities()` の添字。
    pub index: usize,
    pub id: ActivityId,
    /// 診断メッセージ (日本語)。
    pub reason: String,
}

/// 走査に使うデコード計画一式。
#[derive(Debug, Default)]
pub struct PlanSet {
    /// デコードする activity の計画。
    pub plans: Vec<ActivityPlan>,
    /// 互換性が確認できずデコードしない activity。
    pub skipped: Vec<SkippedActivity>,
}

/// 走査に使うデコード計画を組み立てる。
///
/// `walk` が内部で使うものと同じ。呼び出し側が「どの activity を読み飛ばしたか」を
/// 診断したい場合は、これを直接呼んで [`PlanSet::skipped`] を見る。
///
/// ## 互換性の確定を配置の選択より先に行う
///
/// 世代 (`format_magic`) と activity magic で互換性を確定してから配置を選ぶ。
/// 「既知 activity ID だから最新の配置で読む」と、未知 magic のファイルに対して
/// 互換性を確認しないままフィールドを解釈し、意味の違う値を統計値として出す。
/// 本家も magic 不一致の activity は読み飛ばす
/// (`docs/format/02-activities.md` §8.1)。
pub fn plan_activities(file: &SaFile, selection: &Selection) -> Result<PlanSet> {
    // `0x2170` 世代の `file_activity` には magic フィールドが無い (§3.4 の表)。
    // デコード結果が 0 になるだけなので、「magic が無い」ことを明示的に区別する。
    let has_activity_magic = file.spec().magic != FORMAT_MAGIC_WITHOUT_ACTIVITY_MAGIC;

    let mut out = PlanSet::default();
    for (index, act) in file.activities().iter().enumerate() {
        if !selection.includes(act.id) {
            continue;
        }
        let Some(def) = crate::layout::registry::lookup(act.id) else {
            // 未知 activity は読み飛ばす (エラーではない = 正常な前方互換)
            out.skipped.push(SkippedActivity {
                index,
                id: act.id,
                reason: format!(
                    "activity ID {} の定義が無い (前方互換として読み飛ばす)",
                    act.id.raw()
                ),
            });
            continue;
        };

        let shape = DeclaredShape {
            magic: has_activity_magic.then_some(act.magic),
            size: act.size as usize,
            types_nr: act.types_nr,
        };
        let rev = match select_revision(def, &shape) {
            Ok(rev) => rev,
            Err(why) => {
                out.skipped.push(SkippedActivity {
                    index,
                    id: act.id,
                    reason: why.to_string(),
                });
                continue;
            }
        };

        // 型別個数の増減が混在している申告は、どちらの配置で読んでも意味が合わない。
        // 同じ magic のままフィールドを減らすことは規約で禁じられているので (§4.5)、
        // 混在は破損である。読み飛ばしではなくエラーにする
        // (ヘッダ表示だけなら通る = `SaFile::open` は成功する)。
        if let Some(types) = shape.types_nr
            && !crate::layout::plan::types_nr_is_monotonic(types, rev.types_nr)
        {
            return Err(crate::error::Error::InconsistentHeader {
                path: file.path().to_path_buf(),
                detail: format!(
                    "{}: types_nr {:?} が既知の {:?} に対して増減混在している \
                     (減らす場合は activity magic が上がるはず)",
                    act.id, types, rev.types_nr
                ),
            });
        }

        let plan = DecodePlan::build_for(
            def,
            rev,
            &shape,
            act.nr.max(0) as u32,
            act.nr2.max(1) as u32,
            file.encoding(),
        )?;

        // `nr × nr2 × size` が 32bit を溢れる申告は、確保も位置計算も破綻する
        // (本家の既知の脆弱性 GHSL-2022-074 を突く細工ファイルがある)。
        // 個々の上限は通ってしまうので、積で検査する。
        if plan.payload_bytes().is_none() {
            let items = (plan.nr as u64).saturating_mul(plan.nr2.max(1) as u64);
            return Err(crate::error::Error::LimitExceeded {
                path: file.path().to_path_buf(),
                what: format!("{} の nr × nr2 × size", act.id),
                value: items.saturating_mul(plan.stride as u64),
                limit: u32::MAX as u64,
            });
        }

        out.plans.push(ActivityPlan {
            index,
            id: act.id,
            plan,
        });
    }
    Ok(out)
}

/// activity の選択。
#[derive(Debug, Clone, Default)]
pub enum Selection {
    /// ファイルに含まれる既知 activity すべて。
    #[default]
    All,
    /// 指定した activity のみ。
    Only(Vec<ActivityId>),
}

impl Selection {
    fn includes(&self, id: ActivityId) -> bool {
        match self {
            Selection::All => true,
            Selection::Only(list) => list.contains(&id),
        }
    }
}

/// 走査で通知されるもの。
///
/// イベント (RESTART / COMMENT) を統計レコードに従属させないための型。
/// [`walk`] はイベントを次の統計レコードへ束ねて渡すため、最後の統計レコードより
/// 後ろにあるイベント (`STATS → COMMENT → EOF`) や、統計を 1 件も含まないファイルの
/// イベントを取りこぼす。[`walk_items`] はイベントを**読んだ順にその場で**通知するので
/// 取りこぼしが起こらず、束ねるためのバッファも増えない。
#[derive(Debug)]
pub enum WalkItem<'a> {
    /// 統計レコード 1 件 (前サンプルとの対)。
    ///
    /// この経路では [`IntervalView::events`] は**常に空**である
    /// (イベントは `Event` で先に通知済み。二重に渡さない)。
    Sample(&'a IntervalView<'a>),
    /// 統計を伴わないレコード (RESTART / COMMENT)。
    Event(RecordEvent),
}

/// レコード対とイベントを順に走査する。
///
/// 選択されていない activity はデコードせず、オフセット加算だけで読み飛ばす。
/// 1 レコードに多数の activity が並ぶため、`-u` だけを見る場合の削減効果が大きい。
///
/// イベントは読んだ順に [`WalkItem::Event`] として通知される。
/// **最後の統計レコードより後ろにあるイベントも必ず届く。**
///
/// 互換性が確認できない activity (未知 ID / 未知 magic) は読み飛ばす。
/// 何を読み飛ばしたかを診断したい場合は [`plan_activities`] を直接呼ぶ。
pub fn walk_items<F>(file: &SaFile, selection: &Selection, mut visit: F) -> Result<ScanSummary>
where
    F: FnMut(WalkItem<'_>) -> Result<ScanControl>,
{
    // --- デコード計画を 1 度だけ構築する ---
    let plans = plan_activities(file, selection)?.plans;

    let mut prev = Snapshot::default();
    let mut curr = Snapshot::default();
    let empty = Snapshot::default();
    let mut have_prev = false;
    let mut continuous = true;

    file.scan(|rec| {
        match rec.kind {
            RecordKind::Restart => {
                // 再起動をまたぐと累積カウンタが 0 に戻るので、差分を作ってはいけない
                continuous = false;
                return visit(WalkItem::Event(RecordEvent::Restart {
                    ust_time: rec.ust_time,
                    hour: rec.hour,
                    minute: rec.minute,
                    second: rec.second,
                    cpu_count: rec.cpu_count,
                }));
            }
            RecordKind::Comment => {
                return visit(WalkItem::Event(RecordEvent::Comment {
                    ust_time: rec.ust_time,
                    hour: rec.hour,
                    minute: rec.minute,
                    second: rec.second,
                    text: rec.comment.unwrap_or("").to_string(),
                }));
            }
            RecordKind::Extra(_) | RecordKind::Invalid(_) => {
                return Ok(ScanControl::Continue);
            }
            _ => {}
        }

        decode_snapshot(&mut curr, rec, file, &plans)?;

        let itv_cs = if have_prev {
            interval_cs(prev.uptime_cs, curr.uptime_cs)
        } else {
            // 先頭レコードは「起動からの経過」を区間とみなす (本家と同じ)
            if curr.uptime_cs == 0 {
                1
            } else {
                curr.uptime_cs
            }
        };

        let view = IntervalView {
            prev: if have_prev { &prev } else { &empty },
            curr: &curr,
            itv_cs,
            has_prev: have_prev,
            continuous: have_prev && continuous,
            // イベントは Event で通知済み
            events: &[],
            plans: &plans,
        };
        let control = visit(WalkItem::Sample(&view))?;

        std::mem::swap(&mut prev, &mut curr);
        have_prev = true;
        continuous = true;

        Ok(control)
    })
}

/// レコード対を順に走査する (イベントを次の統計レコードへ束ねる経路)。
///
/// [`IntervalView::events`] には「直前の統計レコード以降に現れたイベント」が入る。
///
/// **最後の統計レコードより後ろにあるイベントは通知されない。**
/// `STATS → COMMENT → EOF` の COMMENT や、統計を 1 件も含まないファイルの
/// イベント列はここでは受け取れないので、取りこぼしたくない場合は
/// [`walk_items`] を使うこと (イベントが読んだ順に届く)。
pub fn walk<F>(file: &SaFile, selection: &Selection, mut visit: F) -> Result<ScanSummary>
where
    F: FnMut(&IntervalView<'_>) -> Result<ScanControl>,
{
    let mut pending: Vec<RecordEvent> = Vec::new();
    walk_items(file, selection, |item| match item {
        WalkItem::Event(ev) => {
            pending.push(ev);
            Ok(ScanControl::Continue)
        }
        WalkItem::Sample(view) => {
            // 束ねたイベントを載せ替えて渡す (他のフィールドはそのまま)。
            // `..*view` が書けるのは IntervalView の全フィールドが Copy だからで、
            // 非 Copy のフィールドを足すとここが壊れる。
            let merged = IntervalView {
                events: &pending,
                ..*view
            };
            let control = visit(&merged)?;
            pending.clear();
            Ok(control)
        }
    })
}

/// レコードの統計データをスナップショットへデコードする。
///
/// `out` の `Vec` を使い回すため、2 回目以降のレコードでは確保が起きない。
fn decode_snapshot(
    out: &mut Snapshot,
    rec: &RawRecord<'_>,
    file: &SaFile,
    plans: &[ActivityPlan],
) -> Result<()> {
    out.reset_meta();
    out.valid = true;
    out.kind = Some(rec.kind);
    out.ust_time = rec.ust_time;
    out.uptime_cs = rec.uptime_cs.unwrap_or(0);
    out.hour = rec.hour;
    out.minute = rec.minute;
    out.second = rec.second;

    let cur = Cursor::new(file.bytes(), file.encoding().endian);
    let mut used = 0usize;

    for ap in plans {
        // このレコードでの実際の位置と item 数を取る
        let Some(slice) = rec.slices.iter().find(|s| s.index == ap.index) else {
            continue;
        };

        // 既存の枠を再利用する (足りなければ 1 つだけ増やす)
        if used >= out.activities.len() {
            out.activities.push(ActivitySnapshot::default());
        }
        let dest = &mut out.activities[used];
        decode_activity_into(dest, &cur, slice, &ap.plan, ap.id)?;
        used += 1;
    }

    // 余った枠は捨てる (内側の Vec の容量ごと消えるが、通常 activity 構成は変わらない)
    out.activities.truncate(used);
    Ok(())
}

fn decode_activity_into(
    dest: &mut ActivitySnapshot,
    cur: &Cursor<'_>,
    slice: &ActivitySlice,
    plan: &DecodePlan,
    id: ActivityId,
) -> Result<()> {
    let count = match plan.shape {
        ItemShape::Matrix => slice.nr as usize * slice.nr2.max(1) as usize,
        ItemShape::Single => 1.min(slice.nr as usize),
        ItemShape::List => slice.nr as usize,
    };

    dest.id = id;
    dest.index = slice.index;
    dest.nr = slice.nr;
    dest.nr2 = slice.nr2;

    let mut filled = 0usize;
    for i in 0..count {
        let base = slice.offset + i * slice.stride;

        // item ごとに「申告サイズ分だけ」のビューを作る。
        //
        // レコード全体を指す Cursor を渡すと、選んだ配置が申告サイズより大きい場合に
        // **隣の item や別レコードのバイトを統計値として読めてしまう**
        // (ファイル末尾まで読めることだけでは、この誤読は検出できない)。
        // ここで範囲を確定させれば、はみ出した読み取りは構造的に起こらない。
        // 範囲は scan 側で検証済みなので、切り出せないのは想定外の状態。
        let Ok(view) = plan.item_view(cur, base) else {
            break;
        };

        if filled >= dest.items.len() {
            dest.items.push(ItemSnapshot::default());
        }
        let item = &mut dest.items[filled];

        // 値は item のバッファへ直接書く (中間バッファからのコピーを省く)
        plan.decode_item_into(&view, &mut item.values)
            .map_err(|e| {
                crate::error::Error::Other(format!(
                    "{id} の item {i} をデコードできない: 範囲外 (offset={}, need={})",
                    e.offset, e.need
                ))
            })?;

        // 名前が変わらなければ確保し直さない (デバイス名は通常固定。texts 側と同じ扱い)。
        //
        // 計測 (criterion / `benches/decode.rs` の `walk_all_activities`、
        // 名前付き 50 インターフェース × 2000 レコードの 9.5 MB ファイル、
        // 交互に 4 往復) では 9.45 ms → 7.75 ms (約 18% 短縮)。
        // 本家 fixture (22 KB) では雑音に埋もれて差が出ないので、
        // 名前付き item が多いファイルで測る必要がある。
        let key = plan
            .read_item_key(&view)
            .ok()
            .flatten()
            .filter(|s| !s.is_empty());
        if item.key.as_deref() != key {
            item.key = key.map(|s| s.to_owned().into_boxed_str());
        }

        // 文字列フィールドを持つ activity は少数なので、無ければ何もしない
        if plan.text_fields.is_empty() {
            item.texts.clear();
        } else {
            let mut read: Vec<Option<&str>> = Vec::with_capacity(plan.text_fields.len());
            plan.read_texts_into(&view, &mut read).map_err(|e| {
                crate::error::Error::Other(format!(
                    "{id} の item {i} の文字列フィールドを読めない (offset={}, need={})",
                    e.offset, e.need
                ))
            })?;
            if item.texts.len() != read.len() {
                item.texts.resize(read.len(), None);
            }
            for (dst, src) in item.texts.iter_mut().zip(read.iter()) {
                // 内容が変わらなければ確保し直さない (デバイス名は通常固定)
                if dst.as_deref() != *src {
                    *dst = src.map(|s| s.to_owned().into_boxed_str());
                }
            }
        }

        filled += 1;
    }
    dest.items.truncate(filled);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_all_includes_everything() {
        let s = Selection::All;
        assert!(s.includes(ActivityId::CPU));
        assert!(s.includes(ActivityId(200)));
    }

    #[test]
    fn selection_only_filters() {
        let s = Selection::Only(vec![ActivityId::CPU, ActivityId::MEMORY]);
        assert!(s.includes(ActivityId::CPU));
        assert!(!s.includes(ActivityId::DISK));
    }

    #[test]
    fn reset_meta_keeps_buffers() {
        let mut s = Snapshot {
            valid: true,
            ust_time: 123,
            activities: vec![ActivitySnapshot {
                id: ActivityId::CPU,
                index: 0,
                nr: 2,
                nr2: 1,
                items: vec![ItemSnapshot::default(); 2],
            }],
            ..Default::default()
        };
        let cap = s.activities.capacity();
        s.reset_meta();
        assert!(!s.valid);
        assert_eq!(s.ust_time, 0);
        // activities は消さない (デコード時に上書きして truncate する)
        assert_eq!(s.activities.len(), 1);
        assert_eq!(s.activities.capacity(), cap, "容量は保たれる");
    }

    /// item は配列位置ではなく識別子で対応付ける。
    #[test]
    fn items_are_matched_by_key_not_position() {
        let a = ActivitySnapshot {
            id: ActivityId::NET_DEV,
            index: 0,
            nr: 2,
            nr2: 1,
            items: vec![
                ItemSnapshot {
                    key: Some("eth0".into()),
                    texts: Vec::new(),
                    values: vec![Availability::Present(1)],
                },
                ItemSnapshot {
                    key: Some("lo".into()),
                    texts: Vec::new(),
                    values: vec![Availability::Present(2)],
                },
            ],
        };
        assert_eq!(
            a.item_by_key("lo").unwrap().values[0],
            Availability::Present(2)
        );
        assert!(a.item_by_key("eth1").is_none());
    }

    // =======================================================================
    // 走査の回帰テスト用の最小 fixture
    //
    // オフセットは `docs/format/01-file-format.md` §3 の表から**独立に**書き起こす。
    // 本体の `layouts` / `selfdesc` を参照して組み立てると、同じ誤りが往復して
    // 検出できなくなる。
    // =======================================================================

    /// `0x2175` 世代の各構造体サイズ (LP64 / 現行形)。
    const MAGIC_BYTES: usize = 76;
    const HEADER_BYTES: usize = 336;
    const ACT_BYTES: usize = 36;
    const REC_BYTES: usize = 24;
    const COMMENT_BYTES: usize = 64;

    /// `file_activity` の申告値。
    #[derive(Debug, Clone, Copy)]
    struct ActSpec {
        id: u32,
        magic: u32,
        nr: i32,
        nr2: i32,
        has_nr: bool,
        size: u32,
        types_nr: [u32; 3],
    }

    impl ActSpec {
        /// 現行 `A_CPU` (magic `0x8b` / 80 バイト / ull 10 本)。
        fn cpu(nr: i32) -> Self {
            Self {
                id: 1,
                magic: 0x8b,
                nr,
                nr2: 1,
                has_nr: true,
                size: 80,
                types_nr: [10, 0, 0],
            }
        }

        /// 現行 `A_PCSW` (magic `0x8b` / 16 バイト / ull 1 + ul 1)。
        fn pcsw() -> Self {
            Self {
                id: 2,
                magic: 0x8b,
                nr: 1,
                nr2: 1,
                has_nr: false,
                size: 16,
                types_nr: [1, 1, 0],
            }
        }

        /// 現行 `A_NET_DEV` (magic `0x8d` / 80 バイト / ull 7 + int 1)。
        fn net_dev(nr: i32) -> Self {
            Self {
                id: 12,
                magic: 0x8d,
                nr,
                nr2: 1,
                has_nr: true,
                size: 80,
                types_nr: [7, 0, 1],
            }
        }

        /// このレコードで占めるバイト数 (item 数の前置を含まない)。
        fn payload_len(&self) -> usize {
            self.nr.max(0) as usize * self.nr2.max(1) as usize * self.size as usize
        }
    }

    fn put_u16(buf: &mut [u8], off: usize, v: u16) {
        buf[off..off + 2].copy_from_slice(&v.to_le_bytes());
    }

    fn put_u32(buf: &mut [u8], off: usize, v: u32) {
        buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
    }

    fn put_u64(buf: &mut [u8], off: usize, v: u64) {
        buf[off..off + 8].copy_from_slice(&v.to_le_bytes());
    }

    fn put_str(buf: &mut [u8], off: usize, cap: usize, s: &str) {
        let n = s.len().min(cap - 1);
        buf[off..off + n].copy_from_slice(&s.as_bytes()[..n]);
    }

    /// 最小構成の `0x2175` ファイルを組み立てる (リトルエンディアン / LP64)。
    struct SaBuilder {
        acts: Vec<ActSpec>,
        records: Vec<Vec<u8>>,
    }

    impl SaBuilder {
        fn new(acts: Vec<ActSpec>) -> Self {
            Self {
                acts,
                records: Vec::new(),
            }
        }

        fn record_header(kind: u8, uptime_cs: u64, ust_time: u64) -> Vec<u8> {
            // rec_types_nr = (2,0,1): uptime_cs @0 / ust_time @8 / extra_next @16 /
            // record_type @20 / hour @21 / minute @22 / second @23
            let mut r = vec![0u8; REC_BYTES];
            put_u64(&mut r, 0, uptime_cs);
            put_u64(&mut r, 8, ust_time);
            put_u32(&mut r, 16, 0);
            r[20] = kind;
            r[21] = 10;
            r[22] = 20;
            r[23] = 30;
            r
        }

        /// 統計レコードを足す。`items[i]` は activity `i` の item 列のバイト。
        fn stats(mut self, uptime_cs: u64, ust_time: u64, items: &[Vec<u8>]) -> Self {
            let mut r = Self::record_header(1, uptime_cs, ust_time);
            for (a, bytes) in self.acts.iter().zip(items) {
                assert_eq!(
                    bytes.len(),
                    a.payload_len(),
                    "activity id={} の item バイト数が申告と合わない",
                    a.id
                );
                if a.has_nr {
                    // レコード内に item 数が前置される (AO_COUNTED)
                    let mut n = [0u8; 4];
                    n.copy_from_slice(&(a.nr as u32).to_le_bytes());
                    r.extend_from_slice(&n);
                }
                r.extend_from_slice(bytes);
            }
            self.records.push(r);
            self
        }

        fn comment(mut self, ust_time: u64, text: &str) -> Self {
            let mut r = Self::record_header(4, 0, ust_time);
            let mut c = vec![0u8; COMMENT_BYTES];
            put_str(&mut c, 0, COMMENT_BYTES, text);
            r.extend_from_slice(&c);
            self.records.push(r);
            self
        }

        fn restart(mut self, ust_time: u64, cpu_nr: u32) -> Self {
            let mut r = Self::record_header(2, 0, ust_time);
            r.extend_from_slice(&cpu_nr.to_le_bytes());
            self.records.push(r);
            self
        }

        fn build(self) -> Vec<u8> {
            let mut buf = vec![0u8; MAGIC_BYTES + HEADER_BYTES + self.acts.len() * ACT_BYTES];

            // --- file_magic (§3.2) ---
            put_u16(&mut buf, 0, 0xd596); // sysstat_magic
            put_u16(&mut buf, 2, 0x2175); // format_magic
            buf[4] = 12; // sysstat_version
            buf[5] = 8; // patchlevel
            buf[6] = 0; // sublevel
            buf[7] = 0; // extraversion
            put_u32(&mut buf, 8, HEADER_BYTES as u32); // header_size
            put_u32(&mut buf, 12, 0); // upgraded
            put_u32(&mut buf, 16, 1); // hdr_types_nr[0]
            put_u32(&mut buf, 20, 1); // hdr_types_nr[1]
            put_u32(&mut buf, 24, 12); // hdr_types_nr[2]

            // --- file_header (§3.3、hdr_types_nr = (1,1,12) / 336) ---
            let h = MAGIC_BYTES;
            put_u64(&mut buf, h, 1_600_000_000); // sa_ust_time
            put_u64(&mut buf, h + 8, 100); // sa_hz
            put_u32(&mut buf, h + 16, 1); // sa_cpu_nr
            put_u32(&mut buf, h + 20, self.acts.len() as u32); // sa_act_nr
            put_u32(&mut buf, h + 24, 120); // sa_year (1900 起点)
            put_u32(&mut buf, h + 28, 0); // act_types_nr[0]
            put_u32(&mut buf, h + 32, 0); // act_types_nr[1]
            put_u32(&mut buf, h + 36, 9); // act_types_nr[2]
            put_u32(&mut buf, h + 40, 2); // rec_types_nr[0]
            put_u32(&mut buf, h + 44, 0); // rec_types_nr[1]
            put_u32(&mut buf, h + 48, 1); // rec_types_nr[2]
            put_u32(&mut buf, h + 52, ACT_BYTES as u32); // act_size
            put_u32(&mut buf, h + 56, REC_BYTES as u32); // rec_size
            put_u32(&mut buf, h + 60, 0); // extra_next
            buf[h + 64] = 13; // sa_day
            buf[h + 65] = 8; // sa_month (0 起点 = 9 月)
            buf[h + 66] = 8; // sa_sizeof_long
            put_str(&mut buf, h + 67, 65, "Linux"); // sa_sysname
            put_str(&mut buf, h + 132, 65, "testhost"); // sa_nodename
            put_str(&mut buf, h + 197, 65, "0.0.0-resarch"); // sa_release
            put_str(&mut buf, h + 262, 65, "x86_64"); // sa_machine
            put_str(&mut buf, h + 327, 8, "UTC"); // sa_tzname

            // --- file_activity[] (§3.4、act_types_nr = (0,0,9) / 36) ---
            for (i, a) in self.acts.iter().enumerate() {
                let o = MAGIC_BYTES + HEADER_BYTES + i * ACT_BYTES;
                put_u32(&mut buf, o, a.id);
                put_u32(&mut buf, o + 4, a.magic);
                put_u32(&mut buf, o + 8, a.nr as u32);
                put_u32(&mut buf, o + 12, a.nr2 as u32);
                put_u32(&mut buf, o + 16, a.has_nr as u32);
                put_u32(&mut buf, o + 20, a.size);
                put_u32(&mut buf, o + 24, a.types_nr[0]);
                put_u32(&mut buf, o + 28, a.types_nr[1]);
                put_u32(&mut buf, o + 32, a.types_nr[2]);
            }

            for r in &self.records {
                buf.extend_from_slice(r);
            }
            buf
        }

        fn open(self) -> SaFile {
            let bytes = self.build();
            SaFile::from_bytes("fixture", bytes).expect("ヘッダは読めること")
        }
    }

    /// 8 バイト値を並べた item を作る。
    fn u64_item(values: &[u64], size: usize) -> Vec<u8> {
        let mut v = vec![0u8; size];
        for (i, x) in values.iter().enumerate() {
            put_u64(&mut v, i * 8, *x);
        }
        v
    }

    /// 走査結果を集める (統計レコードの値とイベント)。
    fn collect_items(file: &SaFile, selection: &Selection) -> (Vec<Snapshot>, Vec<RecordEvent>) {
        let mut samples = Vec::new();
        let mut events = Vec::new();
        walk_items(file, selection, |item| {
            match item {
                WalkItem::Sample(view) => samples.push(view.curr.clone()),
                WalkItem::Event(ev) => events.push(ev),
            }
            Ok(ScanControl::Continue)
        })
        .expect("走査できること");
        (samples, events)
    }

    /// 組み立てた fixture が末尾まで余りなく読めること (組み立て自体の検算)。
    #[test]
    fn builder_produces_a_file_that_scans_exactly() {
        let acts = vec![ActSpec::cpu(1), ActSpec::pcsw()];
        let file = SaBuilder::new(acts)
            .restart(1_600_000_000, 1)
            .stats(
                100,
                1_600_000_001,
                &[u64_item(&[1, 2, 3], 80), u64_item(&[4, 5], 16)],
            )
            .comment(1_600_000_002, "hello")
            .stats(
                200,
                1_600_000_003,
                &[u64_item(&[11, 12, 13], 80), u64_item(&[14, 15], 16)],
            )
            .open();
        let summary = walk(&file, &Selection::All, |_| Ok(ScanControl::Continue)).unwrap();
        assert!(
            summary.is_exact(),
            "末尾まで余りなく読めること: {summary:?}"
        );
        assert_eq!(summary.stats, 2);
        assert_eq!(summary.comments, 1);
        assert_eq!(summary.restarts, 1);
    }

    // =======================================================================
    // 指摘 1: 申告サイズを超えるフィールドが item 境界を越えて読めない
    // =======================================================================

    /// `A_CPU` を `size = 8` / `types_nr = [1,0,0]` と申告したファイルで、
    /// 2 本目以降のフィールドが**隣の item の値にならない**こと。
    ///
    /// レビューで再現確認された誤読 (ヘッダ検証を通ったうえで別 item・別レコードの
    /// バイトを統計値として読む) をそのまま固定する。
    #[test]
    fn shrunken_item_size_does_not_read_neighbouring_items() {
        let mut cpu = ActSpec::cpu(2);
        cpu.size = 8;
        cpu.types_nr = [1, 0, 0];

        let items = {
            let mut v = Vec::new();
            v.extend_from_slice(&111u64.to_le_bytes()); // item 0
            v.extend_from_slice(&222u64.to_le_bytes()); // item 1
            v
        };
        let file = SaBuilder::new(vec![cpu])
            .stats(100, 1_600_000_001, &[items])
            .open();

        // 計画の段階で「この世代には無い」と確定していること
        let planned = plan_activities(&file, &Selection::All).expect("計画");
        assert_eq!(planned.plans[0].plan.unavailable_fields(), 9);

        let (samples, _) = collect_items(&file, &Selection::All);
        assert_eq!(samples.len(), 1);
        let act = samples[0].activity(ActivityId::CPU).expect("CPU がある");
        assert_eq!(act.items.len(), 2);
        assert_eq!(act.items[0].values[0], Availability::Present(111));
        assert_eq!(act.items[1].values[0], Availability::Present(222));
        for item in &act.items {
            for (i, v) in item.values.iter().enumerate().skip(1) {
                assert_eq!(
                    *v,
                    Availability::UnsupportedBySource,
                    "field {i} は未提供のはず (隣の item を読んでいない)"
                );
            }
        }
    }

    // =======================================================================
    // 指摘 2: 未知 magic は診断付きでスキップする
    // =======================================================================

    /// 既知 activity ID に未知 magic を与えたら、デコードせず理由付きでスキップする。
    #[test]
    fn unknown_activity_magic_is_skipped_with_a_reason() {
        let mut cpu = ActSpec::cpu(1);
        cpu.magic = 0x99; // 実在しない activity magic
        let file = SaBuilder::new(vec![cpu, ActSpec::pcsw()])
            .stats(
                100,
                1_600_000_001,
                &[u64_item(&[1], 80), u64_item(&[2, 3], 16)],
            )
            .open();

        let planned = plan_activities(&file, &Selection::All).expect("計画");
        assert_eq!(planned.skipped.len(), 1);
        assert_eq!(planned.skipped[0].id, ActivityId::CPU);
        assert!(
            planned.skipped[0].reason.contains("0x99"),
            "理由に magic が入ること: {}",
            planned.skipped[0].reason
        );
        // 互換な activity だけがデコードされる
        assert_eq!(planned.plans.len(), 1);
        assert_eq!(planned.plans[0].id, ActivityId::PCSW);

        let (samples, _) = collect_items(&file, &Selection::All);
        assert!(
            samples[0].activity(ActivityId::CPU).is_none(),
            "未知 magic の activity は「もっともらしい誤値」を出さない"
        );
        assert!(samples[0].activity(ActivityId::PCSW).is_some());
    }

    /// 未登録の activity ID も同じ扱い (スキップ + 理由)。
    #[test]
    fn unknown_activity_id_is_skipped_with_a_reason() {
        let unknown = ActSpec {
            id: 250,
            magic: 0x8a,
            nr: 1,
            nr2: 1,
            has_nr: false,
            size: 8,
            types_nr: [1, 0, 0],
        };
        let file = SaBuilder::new(vec![unknown, ActSpec::pcsw()])
            .stats(
                100,
                1_600_000_001,
                &[u64_item(&[9], 8), u64_item(&[2, 3], 16)],
            )
            .open();

        let planned = plan_activities(&file, &Selection::All).expect("計画");
        assert_eq!(planned.plans.len(), 1);
        assert_eq!(planned.skipped.len(), 1);
        assert_eq!(planned.skipped[0].id, ActivityId(250));
    }

    // =======================================================================
    // 指摘 3: 申告された型別個数がフィールド位置に反映される
    // =======================================================================

    /// ULL 群が互換拡張された `A_NET_DEV` (`types_nr = [8,0,1]` / `size = 88`) を
    /// 正しい位置から読むこと。
    ///
    /// 現行定義の 56 / 60 / 76 ではなく 64 / 68 / 84 が正しい。
    /// 誤った位置から読むと `speed` が 0 になり、インターフェース名も化ける。
    #[test]
    fn extended_types_nr_is_reflected_in_decoded_values() {
        let mut dev = ActSpec::net_dev(1);
        dev.size = 88;
        dev.types_nr = [8, 0, 1];

        let mut item = vec![0u8; 88];
        // ull 8 本 (0..64)。7 本目 = multicast、8 本目は未知フィールド。
        put_u64(&mut item, 0, 1000); // rx_packets
        put_u64(&mut item, 48, 7); // multicast
        put_u64(&mut item, 56, 0xdead_beef); // 未知の 8 本目 (読まない)
        put_u32(&mut item, 64, 1000); // speed
        put_str(&mut item, 68, 16, "eth0"); // interface
        item[84] = 1; // duplex

        let file = SaBuilder::new(vec![dev])
            .stats(100, 1_600_000_001, &[item])
            .open();

        let planned = plan_activities(&file, &Selection::All).expect("計画");
        assert!(planned.skipped.is_empty());
        let plan = &planned.plans[0].plan;
        let (samples, _) = collect_items(&file, &Selection::All);
        let act = samples[0].activity(ActivityId::NET_DEV).expect("NET_DEV");
        let item = &act.items[0];

        assert_eq!(item.key.as_deref(), Some("eth0"), "名前が化けていない");
        let speed_col = crate::layout::registry::lookup(ActivityId::NET_DEV)
            .unwrap()
            .columns
            .iter()
            .position(|c| c.wire_name == "speed")
            .unwrap();
        assert_eq!(
            plan.column_value(&item.values, speed_col),
            Availability::Present(1000),
            "speed を 64 から読むこと"
        );
        let mc_col = crate::layout::registry::lookup(ActivityId::NET_DEV)
            .unwrap()
            .columns
            .iter()
            .position(|c| c.wire_name == "multicast")
            .unwrap();
        assert_eq!(
            plan.column_value(&item.values, mc_col),
            Availability::Present(7)
        );
    }

    /// 型別個数が減った申告では、そのフィールドを欠落として扱う。
    #[test]
    fn reduced_types_nr_reports_missing_fields() {
        let mut dev = ActSpec::net_dev(1);
        // speed (int 群) を持たない世代を模す
        dev.size = 80;
        dev.types_nr = [7, 0, 0];

        let mut item = vec![0u8; 80];
        put_u64(&mut item, 0, 5); // rx_packets
        put_str(&mut item, 56, 16, "eth0"); // int が無いので interface は 56

        let file = SaBuilder::new(vec![dev])
            .stats(100, 1_600_000_001, &[item])
            .open();
        let planned = plan_activities(&file, &Selection::All).expect("計画");
        let plan = &planned.plans[0].plan;
        let (samples, _) = collect_items(&file, &Selection::All);
        let act = samples[0].activity(ActivityId::NET_DEV).unwrap();
        let item = &act.items[0];

        assert_eq!(item.key.as_deref(), Some("eth0"));
        let speed_col = crate::layout::registry::lookup(ActivityId::NET_DEV)
            .unwrap()
            .columns
            .iter()
            .position(|c| c.wire_name == "speed")
            .unwrap();
        assert_eq!(
            plan.column_value(&item.values, speed_col),
            Availability::UnsupportedBySource,
            "無いフィールドは 0 ではなく未提供"
        );
    }

    /// 型別個数の増減が混在している申告は破損として拒否する (§4.5)。
    ///
    /// ヘッダ表示 (`SaFile::open`) は通り、統計を読む段で落ちる。
    #[test]
    fn mixed_types_nr_change_is_rejected_when_reading_stats() {
        let mut cpu = ActSpec::cpu(1);
        // ull を減らし int を増やす混在 (magic を上げずにこれは起こらない)
        cpu.types_nr = [0, 2, 0];
        let file = SaBuilder::new(vec![cpu])
            .stats(100, 1_600_000_001, &[u64_item(&[1], 80)])
            .open();
        let err = plan_activities(&file, &Selection::All).expect_err("拒否されること");
        assert!(
            matches!(err, crate::error::Error::InconsistentHeader { .. }),
            "{err:?}"
        );
    }

    // =======================================================================
    // 指摘 4: 最後の統計レコード以降のイベントも通知される
    // =======================================================================

    /// `STATS → COMMENT → EOF` の COMMENT が届くこと。
    #[test]
    fn trailing_comment_is_reported_by_walk_items() {
        let file = SaBuilder::new(vec![ActSpec::pcsw()])
            .stats(100, 1_600_000_001, &[u64_item(&[1, 2], 16)])
            .comment(1_600_000_002, "last word")
            .open();

        let (samples, events) = collect_items(&file, &Selection::All);
        assert_eq!(samples.len(), 1);
        assert_eq!(events.len(), 1, "末尾のイベントが通知されること");
        match &events[0] {
            RecordEvent::Comment { text, ust_time, .. } => {
                assert_eq!(text, "last word");
                assert_eq!(*ust_time, 1_600_000_002);
            }
            other => panic!("COMMENT のはず: {other:?}"),
        }
    }

    /// 統計を 1 件も含まないファイルでもイベントが届くこと。
    #[test]
    fn event_only_file_still_reports_events() {
        let file = SaBuilder::new(vec![ActSpec::pcsw()])
            .comment(1_600_000_001, "one")
            .restart(1_600_000_002, 4)
            .comment(1_600_000_003, "two")
            .open();

        let (samples, events) = collect_items(&file, &Selection::All);
        assert!(samples.is_empty(), "統計レコードは無い");
        assert_eq!(events.len(), 3, "溜め込んだまま捨てられないこと");
        assert!(matches!(events[1], RecordEvent::Restart { .. }));
    }

    /// イベントは読んだ順に、対応する統計レコードより先に通知される。
    #[test]
    fn events_are_reported_before_the_following_sample() {
        let file = SaBuilder::new(vec![ActSpec::pcsw()])
            .restart(1_600_000_000, 2)
            .stats(100, 1_600_000_001, &[u64_item(&[1, 2], 16)])
            .open();

        let mut order = Vec::new();
        walk_items(&file, &Selection::All, |item| {
            order.push(match item {
                WalkItem::Event(RecordEvent::Restart { .. }) => "restart",
                WalkItem::Event(_) => "comment",
                WalkItem::Sample(_) => "sample",
            });
            Ok(ScanControl::Continue)
        })
        .unwrap();
        assert_eq!(order, vec!["restart", "sample"]);
    }

    /// 互換経路 (`walk`) の契約: イベントは次の統計レコードに束ねられ、
    /// **最後の統計レコードより後ろのイベントは届かない**。
    ///
    /// これは既存の呼び出し側の挙動を変えないための仕様であり、
    /// 取りこぼしたくない場合は `walk_items` を使う。
    #[test]
    fn walk_batches_events_and_drops_the_trailing_ones() {
        let file = SaBuilder::new(vec![ActSpec::pcsw()])
            .restart(1_600_000_000, 2)
            .stats(100, 1_600_000_001, &[u64_item(&[1, 2], 16)])
            .comment(1_600_000_002, "after the last sample")
            .open();

        let mut batches: Vec<usize> = Vec::new();
        walk(&file, &Selection::All, |view| {
            batches.push(view.events.len());
            // 直前の RESTART は束ねられて届く
            assert!(
                view.events
                    .iter()
                    .any(|e| matches!(e, RecordEvent::Restart { .. }))
            );
            Ok(ScanControl::Continue)
        })
        .unwrap();
        assert_eq!(batches, vec![1], "末尾の COMMENT は届かない (既知の制約)");
    }

    // =======================================================================
    // 指摘 5: item 名の再確保を避ける (内容が同じなら保持する)
    // =======================================================================

    /// 名前が変わらない間は保持し、変わったら追随すること。
    ///
    /// 再確保を避ける最適化で「古い名前が残る」誤りを作らないための回帰テスト。
    #[test]
    fn item_key_follows_the_name_across_records() {
        let mut item_a = vec![0u8; 80];
        put_str(&mut item_a, 60, 16, "eth0");
        let mut item_b = vec![0u8; 80];
        put_str(&mut item_b, 60, 16, "eth0");
        let mut item_c = vec![0u8; 80];
        put_str(&mut item_c, 60, 16, "eth1");

        let file = SaBuilder::new(vec![ActSpec::net_dev(1)])
            .stats(100, 1_600_000_001, &[item_a])
            .stats(200, 1_600_000_002, &[item_b])
            .stats(300, 1_600_000_003, &[item_c])
            .open();

        let (samples, _) = collect_items(&file, &Selection::All);
        let keys: Vec<Option<String>> = samples
            .iter()
            .map(|s| {
                s.activity(ActivityId::NET_DEV).unwrap().items[0]
                    .key
                    .as_deref()
                    .map(str::to_owned)
            })
            .collect();
        assert_eq!(
            keys,
            vec![
                Some("eth0".to_string()),
                Some("eth0".to_string()),
                Some("eth1".to_string())
            ]
        );
    }
}

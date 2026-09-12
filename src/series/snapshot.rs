//! レコードのデコード結果を保持するスナップショットと、レコード対の走査。
//!
//! カウンタのレートは 2 レコード間の差分から得るため、前後 2 つのサンプルを
//! 同時に持つ必要がある。ここでは 2 つのバッファを入れ替えて使い回し、
//! レコードごとの確保を避ける (本家も同じダブルバッファ方式)。

use crate::error::Result;
use crate::format::file::{ActivitySlice, RawRecord, SaFile, ScanControl, ScanSummary};
use crate::format::reader::Cursor;
use crate::format::registry::RecordKind;
use crate::layout::plan::DecodePlan;
use crate::layout::registry::ItemShape;
use crate::model::{ActivityId, Availability};

use super::delta::interval_cs;

/// item 1 個分のデコード結果。
#[derive(Debug, Clone, Default)]
pub struct ItemSnapshot {
    /// デバイス名・インターフェース名など。無い activity は `None`。
    pub key: Option<Box<str>>,
    /// wire フィールドの値 (宣言順)。
    pub values: Vec<Availability<u64>>,
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

/// レコード対を順に走査する。
///
/// 選択されていない activity はデコードせず、オフセット加算だけで読み飛ばす。
/// 1 レコードに多数の activity が並ぶため、`-u` だけを見る場合の削減効果が大きい。
pub fn walk<F>(file: &SaFile, selection: &Selection, mut visit: F) -> Result<ScanSummary>
where
    F: FnMut(&IntervalView<'_>) -> Result<ScanControl>,
{
    // --- デコード計画を 1 度だけ構築する ---
    let mut plans: Vec<ActivityPlan> = Vec::new();
    for (index, act) in file.activities().iter().enumerate() {
        if !selection.includes(act.id) {
            continue;
        }
        let Some(def) = crate::layout::registry::lookup(act.id) else {
            // 未知 activity は読み飛ばす (エラーではない)
            continue;
        };
        // magic と型別個数の両方で revision を絞る。
        // 同じ magic のまま構造体が変わった版があるため magic だけでは決まらない。
        let rev = act
            .types_nr
            .and_then(|t| def.revision_for_types_nr(t))
            .or_else(|| def.revision_for_magic(act.magic))
            .or_else(|| def.latest());
        let Some(rev) = rev else { continue };

        let plan = DecodePlan::build(
            def,
            rev,
            act.size as usize,
            act.nr.max(0) as u32,
            act.nr2.max(1) as u32,
            file.encoding(),
        )?;
        plans.push(ActivityPlan {
            index,
            id: act.id,
            plan,
        });
    }

    let mut prev = Snapshot::default();
    let mut curr = Snapshot::default();
    let empty = Snapshot::default();
    let mut events: Vec<RecordEvent> = Vec::new();
    let mut have_prev = false;
    let mut continuous = true;

    let summary = file.scan(|rec| {
        match rec.kind {
            RecordKind::Restart => {
                events.push(RecordEvent::Restart {
                    ust_time: rec.ust_time,
                    hour: rec.hour,
                    minute: rec.minute,
                    second: rec.second,
                    cpu_count: rec.cpu_count,
                });
                // 再起動をまたぐと累積カウンタが 0 に戻るので、差分を作ってはいけない
                continuous = false;
                return Ok(ScanControl::Continue);
            }
            RecordKind::Comment => {
                events.push(RecordEvent::Comment {
                    ust_time: rec.ust_time,
                    hour: rec.hour,
                    minute: rec.minute,
                    second: rec.second,
                    text: rec.comment.unwrap_or("").to_string(),
                });
                return Ok(ScanControl::Continue);
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
            events: &events,
            plans: &plans,
        };
        let control = visit(&view)?;
        events.clear();

        std::mem::swap(&mut prev, &mut curr);
        have_prev = true;
        continuous = true;

        Ok(control)
    })?;

    Ok(summary)
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
        // 範囲は scan 側で検証済みだが、念のため確認する
        if base + slice.stride > cur.len() {
            break;
        }

        if filled >= dest.items.len() {
            dest.items.push(ItemSnapshot::default());
        }
        let item = &mut dest.items[filled];

        // 値は item のバッファへ直接書く (中間バッファからのコピーを省く)
        plan.decode_item_into(cur, base, &mut item.values)
            .map_err(|e| {
                crate::error::Error::Other(format!(
                    "{id} の item {i} をデコードできない: 範囲外 (offset={}, need={})",
                    e.offset, e.need
                ))
            })?;

        item.key = plan
            .read_item_key(cur, base)
            .ok()
            .flatten()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_owned().into_boxed_str());

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
                    values: vec![Availability::Present(1)],
                },
                ItemSnapshot {
                    key: Some("lo".into()),
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
}

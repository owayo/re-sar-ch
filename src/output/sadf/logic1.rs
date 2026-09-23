//! `sadf.c: logic1_display_loop()` の再現 (`-j` / `-x`)。
//!
//! 時刻順に 1 回走査して統計を出し、RESTART と COMMENT はその後に
//! ファイルを読み直してまとめて出す (§0.3)。
//!
//! ```text
//! do {
//!     // 外側: RESTART / COMMENT と範囲外の統計レコードを読み飛ばし、基準 R を探す
//!     R の次から RESTART / EOF / count / -e まで:
//!         統計レコードごとに f_statistics(F_MAIN)   ← JSON はここで `{` を出す
//!         generic_write_stats(): next_slice() → -e 判定 → 表示
//!     count / -e で止まったなら次の RESTART まで読み飛ばす
//! } while (!EOF)
//! restarts: 範囲内の RESTART / comments: 範囲内の COMMENT (-C のときだけ)
//! ```
//!
//! **JSON は表示しなかったレコードでも空のオブジェクトを出す。**
//! `f_statistics(F_MAIN)` が `generic_write_stats()` より先に呼ばれ、
//! `next_slice()` で省いたレコードや `-e` を超えたレコードでも `{` が出るため
//! (本家の実測)。これを [`Logic1Sink::record`] の `None` で伝える。
//!
//! `cross_day` は `logic2` と違い区間をまたいでも戻らない (`reset_cd = FALSE`)。

use std::io;

use super::SadfConfig;
use super::records::{CpuNrTracker, Rec, RecKind, RecordIndex};
use super::select::{RecordSelect, Slicer};
use crate::error::Result;
use crate::format::file::{SaFile, ScanControl};
use crate::model::ActivityId;
use crate::output::time_filter::{Admit, TimeCursor};
use crate::series::delta::interval_cs;
use crate::series::{IntervalView, RecordRange, Selection, Snapshot, WalkItem, walk_items_in};

/// 形式ごとの書き出し。
pub(crate) trait Logic1Sink {
    /// 基準レコードより後の統計レコード 1 本。
    ///
    /// `None` は「読んだが表示しなかった」(`next_slice()` で省いた /
    /// `-e` を超えた)。JSON は空のオブジェクトを出す。
    fn record(&mut self, view: Option<&IntervalView<'_>>) -> io::Result<()>;
}

/// 統計の後に出す RESTART / COMMENT (時刻範囲の判定済み)。
#[derive(Debug, Default)]
pub(crate) struct Specials {
    /// RESTART と、その時点で有効な `sa_cpu_nr`。
    pub restarts: Vec<(Rec, Option<u32>)>,
    /// `-C` のときだけ入る。
    pub comments: Vec<Rec>,
}

/// `logic1` を回す。統計を `sink` へ流し、RESTART / COMMENT の一覧を返す。
///
/// `select` は positional の `interval` / `count` (区間ごとに数え直す)。
pub(crate) fn run<S: Logic1Sink>(
    file: &SaFile,
    cfg: &SadfConfig,
    select: RecordSelect,
    selection: &[ActivityId],
    sink: &mut S,
) -> Result<Specials> {
    let index = RecordIndex::build(file, None)?;
    let range = cfg.time_filter.cursor();
    // `-e` の判定と `cross_day`。logic1 では区間をまたいでも戻らない。
    let mut cursor = cfg.time_filter.cursor();
    let sel = Selection::Only(selection.to_vec());
    let n = index.len();
    let mut pos = 0usize;

    while pos < n {
        let Some(r) = (pos..n).find(|&i| {
            let rec = &index.recs[i];
            rec.kind == RecKind::Stats && in_range(&range, rec)
        }) else {
            break;
        };
        let block_end = index.next_restart(r + 1);
        run_block(file, select, &index, r, block_end, &sel, &mut cursor, sink)?;
        // RESTART まで読んで次の区間へ (count / -e で止まった場合も読み飛ばす)
        pos = block_end + 1;
    }

    // RESTART は別の走査で読み直す。`sa_cpu_nr` はファイルヘッダの値から数え直す
    // (本家は `seek_file_position(DO_RESTORE)` で保存値に戻してから読む)。
    let mut cpu = CpuNrTracker::new(file);
    let mut specials = Specials::default();
    for rec in &index.recs {
        match rec.kind {
            RecKind::Restart => {
                let nr = cpu.take(rec.cpu_count);
                if in_range(&range, rec) {
                    specials.restarts.push((rec.clone(), nr));
                }
            }
            RecKind::Comment if cfg.comments && in_range(&range, rec) => {
                specials.comments.push(rec.clone());
            }
            _ => {}
        }
    }
    Ok(specials)
}

/// 1 区間ぶん (基準 R の次から RESTART / EOF / count / -e まで)。
#[allow(clippy::too_many_arguments)]
fn run_block<S: Logic1Sink>(
    file: &SaFile,
    select: RecordSelect,
    index: &RecordIndex,
    r: usize,
    block_end: usize,
    selection: &Selection,
    cursor: &mut TimeCursor,
    sink: &mut S,
) -> Result<()> {
    let mut slicer = Slicer::new(select, index.recs[r].uptime_cs);
    // `next_slice()` で表示を省いたレコードは前サンプルにならない
    let keep_prev = !select.selects_every_record();
    let mut held: Option<Snapshot> = None;
    let mut remaining = select.count;
    let mut at = r;

    walk_items_in(
        file,
        selection,
        RecordRange {
            start: r,
            end: block_end,
        },
        |item| {
            let k = at;
            at += 1;
            let WalkItem::Sample(view) = item else {
                // COMMENT は統計の走査では読み飛ばす
                return Ok(ScanControl::Continue);
            };
            if k == r {
                // 基準レコード。前サンプルとして使うだけで表示しない。
                // 2 区間目以降は `cursor` が開始済みなので判定には効かない
                // (前サンプルを持たないので `cross_day` も動かない)。
                let _ = cursor.sample(view);
                if keep_prev {
                    held = Some(view.curr.clone());
                }
                return Ok(ScanControl::Continue);
            }
            if !slicer.admit(view.curr.uptime_cs) {
                sink.record(None).map_err(super::wrap_io)?;
                return Ok(ScanControl::Continue);
            }
            let owned;
            let v: &IntervalView<'_> = match &held {
                Some(prev) => {
                    owned = IntervalView {
                        prev,
                        curr: view.curr,
                        itv_cs: interval_cs(prev.uptime_cs, view.curr.uptime_cs),
                        has_prev: true,
                        continuous: true,
                        events: &[],
                        plans: view.plans,
                    };
                    &owned
                }
                None => view,
            };
            if cursor.sample(v) == Admit::Stop {
                sink.record(None).map_err(super::wrap_io)?;
                return Ok(ScanControl::Stop);
            }
            sink.record(Some(v)).map_err(super::wrap_io)?;
            if keep_prev {
                held = Some(view.curr.clone());
            }
            if let Some(left) = remaining.as_mut() {
                *left = left.saturating_sub(1);
                if *left == 0 {
                    return Ok(ScanControl::Stop);
                }
            }
            Ok(ScanControl::Continue)
        },
    )?;
    Ok(())
}

/// `print_special_record()` / 外側ループの範囲判定 (`cross_day` は偽固定)。
fn in_range(range: &TimeCursor, rec: &Rec) -> bool {
    range.event(rec.ust_time, rec.hms)
}

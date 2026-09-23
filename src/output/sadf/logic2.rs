//! `sadf.c: logic2_display_loop()` の再現 (`-d` / `-p` / `-r`、`-dh`)。
//!
//! 本家の読み方をそのまま状態遷移として写す。形式ごとの書式は
//! [`Logic2Sink`] の実装 (`dbppc` / `raw`) が持ち、**どのレコードを・どの順で・
//! 何回出すか**はここだけで決める。形式ごとに走査を書くと、RESTART / COMMENT /
//! `-s` / `-e` / `count` の規則が形式ごとにずれる (実際に 5 形式で食い違っていた)。
//!
//! ```text
//! do {
//!     // 外側: 範囲内の最初の統計レコード (基準 R) まで読む。
//!     //       途中の RESTART / COMMENT は範囲内なら出す (print_special_record)
//!     for パス in activity × セクション (-dh は全 activity で 1 パス) {
//!         list_fields()                    // -d / -dh のフィールド名一覧行
//!         R の直後へ巻き戻し、R を前サンプルにして
//!         RESTART / EOF / count / -e まで読む:
//!             COMMENT → 範囲内なら出す (パスごとに出る)
//!             統計    → next_slice() → -e 判定 → 表示
//!     }
//!     count か -e で止まったなら次の RESTART まで読み進める (COMMENT は出す)
//!     RESTART を出す (範囲内なら)
//! } while (!EOF)
//! ```
//!
//! 基準 R より前の COMMENT は外側で 1 回だけ出て、R より後の COMMENT は
//! パスごとに出る。フィールド名一覧行は「範囲内の統計レコードが 1 本以上ある
//! 区間」でだけ出る (R が 1 本だけでデータ行が無い区間でも出る)。
//!
//! 読む範囲は [`RecordIndex`] (見出しだけの索引) で決め、統計値のデコードは
//! 表示する範囲に限って [`walk_items_in`] で行う。

use std::io;

use super::SadfConfig;
use super::records::{CpuNrTracker, Rec, RecHeader, RecKind, RecordIndex};
use super::select::{RecordSelect, Slicer};
use super::spec::{ActivitySpec, Section};
use crate::error::Result;
use crate::format::file::{SaFile, ScanControl};
use crate::model::ActivityId;
use crate::output::time_filter::{Admit, TimeCursor};
use crate::series::delta::interval_cs;
use crate::series::{
    IntervalView, RecordEvent, RecordRange, Selection, Snapshot, WalkItem, walk_items_in,
};

/// 1 回の読み直し (`rw_curr_act_stats()`) で出すもの。
#[derive(Debug, Clone, Copy)]
pub(crate) enum Pass {
    /// 1 activity の 1 セクション。`AO_MULTIPLE_OUTPUTS` の activity
    /// (A_CPU / A_MEMORY / A_FS) はビットごとに別パスになる。
    Activity {
        spec: &'static ActivitySpec,
        section: &'static Section,
    },
    /// `-dh`: 全 activity を 1 行に並べる (`ALL_ACTIVITIES`)。
    Horizontal,
}

/// 表示する 1 レコードの文脈。
pub(crate) struct Shown<'v, 'a> {
    pub view: &'v IntervalView<'a>,
    /// パスの activity の `nr_ini` (raw の CPU / softnet はここまで回す)。
    pub nr_ini: u32,
    /// パスの activity の `nr_allocated` (`-O debug` の `# name` 行専用)。
    pub nr_alloc: u32,
}

/// 形式ごとの書き出し。
pub(crate) trait Logic2Sink {
    /// RESTART 行 (時刻範囲の判定済み)。`cpu_nr` はその時点の `sa_cpu_nr`。
    fn restart(&mut self, rec: &Rec, cpu_nr: Option<u32>) -> io::Result<()>;
    /// COMMENT 行 (`-C` と時刻範囲の判定済み)。
    fn comment(&mut self, rec: &Rec) -> io::Result<()>;
    /// パスの先頭 (`list_fields()`)。
    fn begin_pass(&mut self, pass: &Pass) -> io::Result<()>;
    /// 表示する 1 レコード。
    fn sample(&mut self, pass: &Pass, shown: &Shown<'_, '_>) -> io::Result<()>;
    /// レコードヘッダを 1 つ読んだ (`-r -O debug` の `# uptime_cs; …` 行)。
    fn header(&mut self, _hdr: &RecHeader) -> io::Result<()> {
        Ok(())
    }
}

/// パスが止まった位置。
#[derive(Debug, Clone, Copy)]
struct PassEnd {
    /// 次に読むレコードの添字。RESTART で止まった場合はその RESTART。
    next: usize,
    /// `count` に達したか `-e` を超えた (本家の `cnt == 0`)。
    cnt_zero: bool,
    /// ファイル末尾に達した。
    eosaf: bool,
}

/// `logic2` を回す。
///
/// `select` は positional の `interval` / `count`。
/// `debug` は `-r -O debug` (レコードヘッダと `# name` 行の材料を索引に控える)。
pub(crate) fn run<S: Logic2Sink>(
    file: &SaFile,
    cfg: &SadfConfig,
    select: RecordSelect,
    passes: &[Pass],
    selection: &[ActivityId],
    debug: bool,
    sink: &mut S,
) -> Result<()> {
    let tracked: Vec<ActivityId> = passes
        .iter()
        .filter_map(|p| match p {
            Pass::Activity { spec, .. } => Some(spec.id),
            Pass::Horizontal => None,
        })
        .fold(Vec::new(), |mut v, id| {
            if !v.contains(&id) {
                v.push(id);
            }
            v
        });
    let index = RecordIndex::build(file, debug.then_some(tracked.as_slice()))?;
    let mut st = Tracker::new(file, &index, &tracked, debug);
    // RESTART / COMMENT / 基準レコードの範囲判定 (`cross_day` は常に偽)
    let range = cfg.time_filter.cursor();
    let n = index.len();
    let mut pos = 0usize;

    loop {
        // ---- 外側: 範囲内の最初の統計レコード (基準) を探す ----
        let mut reference = None;
        while pos < n {
            let at = pos;
            pos += 1;
            st.read(at, sink)?;
            let rec = &index.recs[at];
            match rec.kind {
                RecKind::Restart => {
                    let cpu = st.restart(rec);
                    if in_range(&range, rec) {
                        sink.restart(rec, cpu).map_err(super::wrap_io)?;
                    }
                }
                RecKind::Comment => {
                    if cfg.comments && in_range(&range, rec) {
                        sink.comment(rec).map_err(super::wrap_io)?;
                    }
                }
                RecKind::Stats => {
                    if in_range(&range, rec) {
                        reference = Some(at);
                        break;
                    }
                }
            }
        }
        let Some(r) = reference else {
            st.read_eof(sink)?;
            return Ok(());
        };

        // ---- activity ごと (セクションごと) に読み直す ----
        let block_end = index.next_restart(r + 1);
        // パスが 1 つも無い (表示できる activity が無い) と本家の `eosaf` は
        // 初期値の真のまま残り、外側を 1 周したところで終わる。
        let mut end = PassEnd {
            next: r + 1,
            cnt_zero: false,
            eosaf: true,
        };
        let walk_selection = Selection::Only(selection.to_vec());
        for pass in passes {
            sink.begin_pass(pass).map_err(super::wrap_io)?;
            let sel = match pass {
                Pass::Activity { spec, .. } => Selection::Only(vec![spec.id]),
                Pass::Horizontal => walk_selection.clone(),
            };
            end = run_pass(
                file, cfg, select, &index, r, block_end, pass, &sel, &mut st, sink,
            )?;
        }

        // ---- count / -e で止まったなら次の RESTART まで読み進める ----
        let mut next = end.next;
        let mut eosaf = end.eosaf;
        if end.cnt_zero && !eosaf {
            loop {
                if next >= n {
                    st.read_eof(sink)?;
                    eosaf = true;
                    break;
                }
                st.read(next, sink)?;
                let rec = &index.recs[next];
                match rec.kind {
                    RecKind::Restart => break,
                    RecKind::Comment if cfg.comments && in_range(&range, rec) => {
                        sink.comment(rec).map_err(super::wrap_io)?;
                    }
                    _ => {}
                }
                next += 1;
            }
        }
        if eosaf {
            return Ok(());
        }

        // ---- 区間を閉じる RESTART (区間ごとに 1 回) ----
        let rec = &index.recs[next];
        let cpu = st.restart(rec);
        if in_range(&range, rec) {
            sink.restart(rec, cpu).map_err(super::wrap_io)?;
        }
        pos = next + 1;
    }
}

/// 1 パス (`rw_curr_act_stats()`)。
#[allow(clippy::too_many_arguments)]
fn run_pass<S: Logic2Sink>(
    file: &SaFile,
    cfg: &SadfConfig,
    select: RecordSelect,
    index: &RecordIndex,
    r: usize,
    block_end: usize,
    pass: &Pass,
    selection: &Selection,
    st: &mut Tracker<'_>,
    sink: &mut S,
) -> Result<PassEnd> {
    let n = index.len();
    let range = cfg.time_filter.cursor();
    // `-e` の判定と `cross_day` (パスごとに戻る: `reset_cd = 1`)
    let mut cursor = cfg.time_filter.cursor();
    let mut slicer = Slicer::new(select, index.recs[r].uptime_cs);
    // `next_slice()` で表示を省いたレコードは前サンプルにならない
    // (本家は `curr` を入れ替えない)。全レコードを出す既定では不要なので控えない。
    let keep_prev = !select.selects_every_record();
    let mut held: Option<Snapshot> = None;
    let mut remaining = select.count;
    let mut at = r;
    let mut stopped: Option<usize> = None;

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
            if k == r {
                // 基準レコード。前サンプルとして使うだけで表示しない。
                if let WalkItem::Sample(view) = item {
                    let _ = cursor.sample(view);
                    if keep_prev {
                        held = Some(view.curr.clone());
                    }
                }
                return Ok(ScanControl::Continue);
            }
            st.read(k, sink)?;
            let view = match item {
                WalkItem::Sample(view) => view,
                WalkItem::Event(RecordEvent::Comment { .. }) => {
                    let rec = &index.recs[k];
                    if cfg.comments && in_range(&range, rec) {
                        sink.comment(rec).map_err(super::wrap_io)?;
                    }
                    return Ok(ScanControl::Continue);
                }
                // 範囲は次の RESTART の手前までなので来ない
                WalkItem::Event(RecordEvent::Restart { .. }) => {
                    return Ok(ScanControl::Continue);
                }
            };
            if !slicer.admit(view.curr.uptime_cs) {
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
                // `-e` 超過: このレコードは出さず cnt = 0
                stopped = Some(k);
                return Ok(ScanControl::Stop);
            }
            st.show(pass, v, sink)?;
            if keep_prev {
                held = Some(view.curr.clone());
            }
            if let Some(left) = remaining.as_mut() {
                *left = left.saturating_sub(1);
                if *left == 0 {
                    stopped = Some(k);
                    return Ok(ScanControl::Stop);
                }
            }
            Ok(ScanControl::Continue)
        },
    )?;

    if let Some(k) = stopped {
        return Ok(PassEnd {
            next: k + 1,
            cnt_zero: true,
            eosaf: false,
        });
    }
    if block_end < n {
        // RESTART のヘッダを読んで止まる (CPU 数は読まない: DONT_READ_CPU_NR)
        st.read(block_end, sink)?;
        Ok(PassEnd {
            next: block_end,
            cnt_zero: false,
            eosaf: false,
        })
    } else {
        st.read_eof(sink)?;
        Ok(PassEnd {
            next: n,
            cnt_zero: false,
            eosaf: true,
        })
    }
}

/// `print_special_record()` / 外側ループの範囲判定 (`cross_day` は偽固定)。
fn in_range(range: &TimeCursor, rec: &Rec) -> bool {
    range.event(rec.ust_time, rec.hms)
}

// ===========================================================================
// 本家のメモリ上の状態 (`sa_cpu_nr` / `nr_ini` / `nr_allocated`)
// ===========================================================================

/// activity 1 つ分の `nr_ini` / `nr_allocated`。
#[derive(Debug, Clone, Copy)]
struct ActState {
    id: ActivityId,
    nr_ini: u32,
    nr_alloc: u32,
}

impl ActState {
    /// `reallocate_buffers()`: 足りなければ倍々で広げる。
    fn grow(&mut self, nr_min: u32) {
        let nr_min = nr_min.max(1);
        if nr_min <= self.nr_alloc {
            return;
        }
        if self.nr_alloc == 0 {
            self.nr_alloc = nr_min;
            return;
        }
        while self.nr_alloc < nr_min {
            self.nr_alloc = self.nr_alloc.saturating_mul(2);
        }
    }
}

/// `AO_PERSISTENT` の activity (RESTART で `nr_ini` が `sa_cpu_nr` に置き換わる)。
fn is_persistent(id: ActivityId) -> bool {
    matches!(id, ActivityId::CPU | ActivityId::IRQ | ActivityId::NET_SOFT)
}

/// 本家がメモリ上に持ち回る状態の写し。
///
/// `nr_allocated` は「それまでに読んだレコードの item 数の最大」で決まる。
/// パスは同じ区間を読み直すので、2 つ目以降のパスでは区間末尾までの最大が
/// 見える (読んだ位置の最前線 `frontier` で管理する)。
struct Tracker<'i> {
    index: &'i RecordIndex,
    debug: bool,
    frontier: Option<usize>,
    acts: Vec<ActState>,
    cpu: CpuNrTracker,
}

impl<'i> Tracker<'i> {
    fn new(file: &SaFile, index: &'i RecordIndex, tracked: &[ActivityId], debug: bool) -> Self {
        let acts = tracked
            .iter()
            .map(|&id| {
                let nr = file
                    .activities()
                    .iter()
                    .find(|e| e.id == id)
                    .map_or(0, |e| e.nr.max(0) as u32);
                ActState {
                    id,
                    nr_ini: nr,
                    // allocate_structures(): nr_ini > 0 の activity だけ確保する
                    nr_alloc: nr,
                }
            })
            .collect();
        Tracker {
            index,
            debug,
            frontier: None,
            acts,
            cpu: CpuNrTracker::new(file),
        }
    }

    /// レコード `at` を読む (`read_record_hdr()` + `read_file_stat_bunch()`)。
    fn read<S: Logic2Sink>(&mut self, at: usize, sink: &mut S) -> Result<()> {
        if self.debug {
            for h in self.index.headers_for(at) {
                sink.header(h).map_err(super::wrap_io)?;
            }
        }
        if self.frontier.is_none_or(|f| at > f) {
            self.frontier = Some(at);
            if self.debug && self.index.recs[at].kind == RecKind::Stats {
                for a in &mut self.acts {
                    if let Some(nr) = self.index.nr_at(at, a.id) {
                        a.grow(nr);
                    }
                }
            }
        }
        Ok(())
    }

    /// ファイル末尾に達した (末尾の拡張レコードのヘッダだけが出る)。
    fn read_eof<S: Logic2Sink>(&mut self, sink: &mut S) -> Result<()> {
        if self.debug {
            for h in self.index.headers_for(self.index.len()) {
                sink.header(h).map_err(super::wrap_io)?;
            }
        }
        Ok(())
    }

    /// RESTART の CPU 数を読む (`print_special_record()`)。
    ///
    /// `sa_cpu_nr` を更新し、`AO_PERSISTENT` の activity の `nr_ini` を置き換える。
    fn restart(&mut self, rec: &Rec) -> Option<u32> {
        let cpu = self.cpu.take(rec.cpu_count);
        if let Some(n) = cpu {
            for a in &mut self.acts {
                if is_persistent(a.id) && a.nr_ini > 0 {
                    a.nr_ini = n;
                    a.grow(n);
                }
            }
        }
        cpu
    }

    /// 1 レコードを表示する。
    fn show<S: Logic2Sink>(
        &mut self,
        pass: &Pass,
        view: &IntervalView<'_>,
        sink: &mut S,
    ) -> Result<()> {
        let id = match pass {
            Pass::Activity { spec, .. } => Some(spec.id),
            Pass::Horizontal => None,
        };
        let state = id.and_then(|id| self.acts.iter_mut().find(|a| a.id == id));
        let (nr_ini, nr_alloc) = state.as_ref().map_or((0, 0), |a| (a.nr_ini, a.nr_alloc));
        sink.sample(
            pass,
            &Shown {
                view,
                nr_ini,
                nr_alloc,
            },
        )
        .map_err(super::wrap_io)?;
        // CPU / 割り込み / softnet の表示関数は `nr[curr] > nr_ini` なら
        // `nr_ini` を引き上げる (`# name` 行はその前に出る)
        if let (Some(a), Some(id)) = (state, id)
            && is_persistent(id)
        {
            let nr = view.curr.activity(id).map_or(0, |s| s.nr);
            a.nr_ini = a.nr_ini.max(nr);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `reallocate_buffers()` は倍々で広げる。
    #[test]
    fn allocation_doubles_until_it_fits() {
        let mut a = ActState {
            id: ActivityId::CPU,
            nr_ini: 10,
            nr_alloc: 10,
        };
        a.grow(9);
        assert_eq!(a.nr_alloc, 10, "足りていれば広げない");
        a.grow(11);
        assert_eq!(a.nr_alloc, 20);
        a.grow(41);
        assert_eq!(a.nr_alloc, 80);

        let mut z = ActState {
            id: ActivityId::DISK,
            nr_ini: 0,
            nr_alloc: 0,
        };
        z.grow(3);
        assert_eq!(z.nr_alloc, 3, "未確保なら要求数ちょうど");
    }

    #[test]
    fn only_cpu_irq_and_softnet_are_persistent() {
        assert!(is_persistent(ActivityId::CPU));
        assert!(is_persistent(ActivityId::IRQ));
        assert!(is_persistent(ActivityId::NET_SOFT));
        assert!(!is_persistent(ActivityId::DISK));
        assert!(!is_persistent(ActivityId::PWR_CPU));
    }
}

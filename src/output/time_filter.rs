//! 時刻フィルタ (`sar -s` / `-e`、`sadf -s` / `-e`、独自 `--from` / `--to`)。
//!
//! 典拠は `docs/format/03-output-format.md` 第 IV 部 §1.8〜§1.11。
//!
//! # 守っている意味論 (最重要)
//!
//! **`-s` の範囲に最初に合致したレコードは「前サンプル」として消費されるだけで、
//! 統計行としては表示されない。** 本家は
//!
//! 1. 外側ループで `tm_start <= rectime <= tm_end` を満たす最初の `R_STATS` を探し、
//!    それを差分の基準 (`slot2`) に採る (この 1 件は表示しない)
//! 2. 内側ループでは `-e` しか見ず、超過したらそのレコードを出さずに打ち切る
//!
//! という 2 段構造になっている (§1.10)。ここでは [`TimeCursor`] が同じ状態遷移を
//! [`Admit`] として返し、呼び出し側 (`sar` テキスト / `sadf` / 独自形式) が
//! それぞれの「基準レコードの扱い」に流し込む。
//!
//! # 比較に使う時刻
//!
//! `-s` / `-e` の比較対象は**表示に使うのと同じ時刻**である (§1.11)。
//! `sar` の既定は読み手のローカル時刻、`sadf` の既定は UTC、`-t` は記録側の
//! 時分秒。epoch 秒 (10 桁) で与えた場合は `record_header.ust_time` と直接
//! 比較するので TZ の影響を受けない。
//!
//! # 日跨ぎ (`cross_day`)
//!
//! `hh:mm:ss` 形式で `-e` < `-s` のとき、本家は `tm_end.tm_hour += 24` して
//! 翌日までを意味させる (`check_time_limits()`)。その `+24` された `-e` と
//! 比較できるよう、時刻が巻き戻ったレコード以降は `hour + 24` で比較する。
//! `cross_day` が立つ条件は `sar` と `sadf` で微妙に違う (§1.9) ので
//! [`CrossDayRule`] で選ぶ。

use std::cmp::Ordering;

use crate::series::snapshot::{IntervalView, Snapshot};

// ===========================================================================
// 境界
// ===========================================================================

/// `-s` / `-e` の境界値。
///
/// `cli::sar_args::TimeSpec` と同じ情報を持つが、出力層が CLI 解析結果の型に
/// 依存しないよう別に定義している (`SarTextOptions` と同じ方針)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TimeBound {
    /// 未指定 (`NO_TIME`)。フィルタとして働かない。
    #[default]
    None,
    /// `hh:mm[:ss]` 指定 (`USE_HHMMSS_T`)。
    ///
    /// `check_time_limits()` の日跨ぎ補正により `hour` は 24..=47 になり得る。
    HhMmSs {
        /// 0..=47。
        hour: u8,
        min: u8,
        sec: u8,
    },
    /// ちょうど 10 桁の epoch 秒 (`USE_EPOCH_T`)。
    Epoch(u64),
}

impl TimeBound {
    /// 未指定か。
    pub fn is_none(self) -> bool {
        matches!(self, TimeBound::None)
    }

    /// 指定されているか。
    pub fn is_set(self) -> bool {
        !self.is_none()
    }
}

/// 比較に使う時刻の基準系。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TimeBasis {
    /// 読み手のローカル時刻 (`sar` の既定)。
    #[default]
    Local,
    /// UTC (`sadf` の既定。`sadf -U` も `hh:mm:ss` 指定はここで比較する)。
    Utc,
    /// レコードに焼き込まれた時分秒 (`-t`)。
    Recorded,
}

/// `cross_day` を立てる規則 (§1.9)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CrossDayRule {
    /// `sar`: `-s` が `hh:mm:ss` 形式のときだけ立ち、**表示基準に換算した hour** を比べる。
    #[default]
    Sar,
    /// `sadf`: `-s` が指定されていれば epoch 形式でも立ち、**記録側の生 hour** を比べる。
    Sadf,
}

// ===========================================================================
// フィルタ本体
// ===========================================================================

/// 時刻フィルタの指定 (不変)。
///
/// 既定は「両端未指定」= フィルタ無効で、[`TimeCursor::sample`] は常に
/// [`Admit::Emit`] を返す。既存の出力と 1 バイトも変わらない。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TimeFilter {
    /// `-s` / `--from`。
    pub start: TimeBound,
    /// `-e` / `--to`。日跨ぎ補正済みの値を入れる。
    pub end: TimeBound,
    /// 比較に使う時刻の基準系。
    pub basis: TimeBasis,
    /// `cross_day` の判定規則。
    pub cross_day: CrossDayRule,
}

impl TimeFilter {
    /// 両端とも未指定か (= 何も絞らない)。
    pub fn is_unbounded(&self) -> bool {
        self.start.is_none() && self.end.is_none()
    }

    /// 走査 1 周分の状態を作る。
    ///
    /// 本家はアクティビティごとにファイルを巻き戻して読み直し、そのたびに
    /// `cross_day` を `FALSE` に戻す。走査ごとに [`TimeCursor`] を作れば同じになる。
    pub fn cursor(&self) -> TimeCursor {
        TimeCursor {
            filter: *self,
            started: false,
            cross_day: false,
        }
    }

    /// 比較用の時刻へ換算する。
    fn rec_time(&self, ust_time: u64, hms: (u8, u8, u8)) -> RecTime {
        use chrono::{Local, TimeZone, Timelike, Utc};
        let (h, m, s) = hms;
        let recorded = RecTime {
            hour: u32::from(h),
            min: u32::from(m),
            sec: u32::from(s),
            epoch: ust_time,
        };
        fn from<Tz: TimeZone>(dt: &chrono::DateTime<Tz>, ust_time: u64) -> RecTime {
            RecTime {
                hour: dt.hour(),
                min: dt.minute(),
                sec: dt.second(),
                epoch: ust_time,
            }
        }
        match self.basis {
            TimeBasis::Recorded => recorded,
            TimeBasis::Utc => Utc
                .timestamp_opt(ust_time as i64, 0)
                .single()
                .map(|dt| from(&dt, ust_time))
                .unwrap_or(recorded),
            TimeBasis::Local => Local
                .timestamp_opt(ust_time as i64, 0)
                .single()
                .map(|dt| from(&dt, ust_time))
                .unwrap_or(recorded),
        }
    }
}

/// 比較用に取り出したレコード時刻。
#[derive(Debug, Clone, Copy)]
struct RecTime {
    hour: u32,
    min: u32,
    sec: u32,
    epoch: u64,
}

/// `datecmp()` 相当 (§1.9)。
///
/// `hh:mm:ss` は **hour → min → sec の階層比較**で、hour が違えば min / sec は
/// 一切見ない。未指定 (`NO_TIME`) は常に「一致」= フィルタ無効。
fn datecmp(rec: RecTime, bound: TimeBound, cross_day: bool) -> Ordering {
    match bound {
        TimeBound::None => Ordering::Equal,
        TimeBound::HhMmSs { hour, min, sec } => {
            let h = rec.hour + if cross_day { 24 } else { 0 };
            if h == u32::from(hour) {
                if rec.min == u32::from(min) {
                    rec.sec.cmp(&u32::from(sec))
                } else {
                    rec.min.cmp(&u32::from(min))
                }
            } else {
                h.cmp(&u32::from(hour))
            }
        }
        TimeBound::Epoch(e) => rec.epoch.cmp(&e),
    }
}

// ===========================================================================
// 判定結果
// ===========================================================================

/// レコード 1 件の扱い。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admit {
    /// 範囲に入る前のレコード。何も出さず、基準にも採らない。
    Skip,
    /// 範囲に最初に合致したレコード。**差分の基準として消費するだけで表示しない。**
    Reference,
    /// 表示する。
    Emit,
    /// `-e` を超えた。**このレコードは出さず**、走査を打ち切る。
    Stop,
}

/// 走査 1 周分のフィルタ状態。
///
/// 本家の「外側ループ (開始点探索) → 内側ループ (`-e` だけ見る)」を
/// [`TimeCursor::started`] の遷移で表す。
#[derive(Debug, Clone)]
pub struct TimeCursor {
    filter: TimeFilter,
    /// 開始レコードが決まったか (= 内側ループに入ったか)。
    started: bool,
    /// 日跨ぎを検出したか。一度立つと走査の終わりまで立ち続ける。
    cross_day: bool,
}

impl TimeCursor {
    /// フィルタが無効か (呼び出し側が分岐を省くための早期判定)。
    pub fn is_unbounded(&self) -> bool {
        self.filter.is_unbounded()
    }

    /// 日跨ぎを検出したか (診断用)。
    pub fn crossed_day(&self) -> bool {
        self.cross_day
    }

    /// 統計レコード 1 件を判定する。
    pub fn sample(&mut self, view: &IntervalView<'_>) -> Admit {
        if self.filter.is_unbounded() {
            return Admit::Emit;
        }
        let rec = self
            .filter
            .rec_time(view.curr.ust_time, snapshot_hms(view.curr));

        if !self.started {
            // 外側ループ: cross_day は FALSE 固定で両端を見る。
            let before_start = datecmp(rec, self.filter.start, false) == Ordering::Less;
            let after_end = datecmp(rec, self.filter.end, false) == Ordering::Greater;
            if before_start || after_end {
                return Admit::Skip;
            }
            self.started = true;
            return Admit::Reference;
        }

        // 内側ループ: cross_day を更新し、`-e` だけを見る。
        self.update_cross_day(view, rec);
        if self.filter.end.is_set()
            && datecmp(rec, self.filter.end, self.cross_day) == Ordering::Greater
        {
            return Admit::Stop;
        }
        Admit::Emit
    }

    /// `RESTART` / `COMMENT` を表示するか。
    ///
    /// 本家の `print_special_record()` は範囲外の特殊レコードを**表示しない**
    /// (読み飛ばしはする)。`cross_day` は渡されないので常に `FALSE` で比較する。
    pub fn event(&self, ust_time: u64, hms: (u8, u8, u8)) -> bool {
        if self.filter.is_unbounded() {
            return true;
        }
        let rec = self.filter.rec_time(ust_time, hms);
        datecmp(rec, self.filter.start, false) != Ordering::Less
            && datecmp(rec, self.filter.end, false) != Ordering::Greater
    }

    /// 独自出力のイベントは、日跨ぎ指定を日内の時刻の弧として判定する。
    /// 互換出力の特殊レコードに固有の `cross_day = false` 規則とは分ける。
    pub fn native_event(&self, ust_time: u64, hms: (u8, u8, u8)) -> bool {
        if let (TimeBound::HhMmSs { .. }, TimeBound::HhMmSs { hour, min, sec }) =
            (self.filter.start, self.filter.end)
            && hour >= 24
        {
            let rec = self.filter.rec_time(ust_time, hms);
            let end = TimeBound::HhMmSs {
                hour: hour - 24,
                min,
                sec,
            };
            return datecmp(rec, self.filter.start, false) != Ordering::Less
                || datecmp(rec, end, false) != Ordering::Greater;
        }
        self.event(ust_time, hms)
    }

    /// `cross_day` の更新 (§1.9)。一度立ったら戻さない。
    fn update_cross_day(&mut self, view: &IntervalView<'_>, rec: RecTime) {
        if self.cross_day || !view.has_prev {
            return;
        }
        let enabled = match self.filter.cross_day {
            // `sar` は `-s` が hh:mm:ss 形式のときだけ
            CrossDayRule::Sar => matches!(self.filter.start, TimeBound::HhMmSs { .. }),
            // `sadf` は `-s` が指定されていれば epoch 形式でも立つ
            CrossDayRule::Sadf => self.filter.start.is_set(),
        };
        if !enabled {
            return;
        }
        if view.prev.ust_time == 0 || view.curr.ust_time <= view.prev.ust_time {
            return;
        }
        let (curr_hour, prev_hour) = match self.filter.cross_day {
            // 表示基準に換算した hour を比べる
            CrossDayRule::Sar => (
                rec.hour,
                self.filter
                    .rec_time(view.prev.ust_time, snapshot_hms(view.prev))
                    .hour,
            ),
            // 記録側の生 hour を比べる
            CrossDayRule::Sadf => (u32::from(view.curr.hour), u32::from(view.prev.hour)),
        };
        if curr_hour < prev_hour {
            self.cross_day = true;
        }
    }
}

fn snapshot_hms(snap: &Snapshot) -> (u8, u8, u8) {
    (snap.hour, snap.minute, snap.second)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hhmmss(hour: u8, min: u8, sec: u8) -> TimeBound {
        TimeBound::HhMmSs { hour, min, sec }
    }

    fn rec(hour: u32, min: u32, sec: u32, epoch: u64) -> RecTime {
        RecTime {
            hour,
            min,
            sec,
            epoch,
        }
    }

    /// 未指定の境界は常に「一致」= フィルタ無効。
    #[test]
    fn unset_bound_always_matches() {
        assert_eq!(
            datecmp(rec(13, 20, 9, 0), TimeBound::None, false),
            Ordering::Equal
        );
    }

    /// hour が違えば min / sec は一切見ない (階層比較)。
    #[test]
    fn hour_dominates_the_comparison() {
        // 13:59:59 vs 14:00:00 → hour の差で Less
        assert_eq!(
            datecmp(rec(13, 59, 59, 0), hhmmss(14, 0, 0), false),
            Ordering::Less
        );
        // 14:00:00 vs 13:59:59 → Greater
        assert_eq!(
            datecmp(rec(14, 0, 0, 0), hhmmss(13, 59, 59), false),
            Ordering::Greater
        );
        // hour 一致 → min の差
        assert_eq!(
            datecmp(rec(13, 20, 59, 0), hhmmss(13, 21, 0), false),
            Ordering::Less
        );
        // hour / min 一致 → sec の差
        assert_eq!(
            datecmp(rec(13, 20, 20, 0), hhmmss(13, 20, 20), false),
            Ordering::Equal
        );
    }

    /// `cross_day` は hour に +24 して比較する。
    #[test]
    fn cross_day_adds_24_hours() {
        // 01:00 は -e 25:00 (= 翌日 01:00) と等しい
        assert_eq!(
            datecmp(rec(1, 0, 0, 0), hhmmss(25, 0, 0), true),
            Ordering::Equal
        );
        // cross_day が立っていなければ 01:00 < 25:00
        assert_eq!(
            datecmp(rec(1, 0, 0, 0), hhmmss(25, 0, 0), false),
            Ordering::Less
        );
    }

    /// epoch 境界は epoch 同士で比べる。
    #[test]
    fn epoch_bound_compares_epoch() {
        let b = TimeBound::Epoch(1_555_593_629);
        assert_eq!(
            datecmp(rec(0, 0, 0, 1_555_593_619), b, false),
            Ordering::Less
        );
        assert_eq!(
            datecmp(rec(0, 0, 0, 1_555_593_629), b, false),
            Ordering::Equal
        );
        assert_eq!(
            datecmp(rec(0, 0, 0, 1_555_593_639), b, false),
            Ordering::Greater
        );
    }

    /// 境界なしのフィルタは常に `Emit` (既存出力と等価)。
    #[test]
    fn unbounded_filter_emits_everything() {
        let f = TimeFilter::default();
        assert!(f.is_unbounded());
        let c = f.cursor();
        assert!(c.is_unbounded());
        assert!(c.event(0, (0, 0, 0)));
        // sample は IntervalView が必要なので、境界なしの早期 return を
        // event 側と is_unbounded で確認する (sample の網羅は統合テスト側)。
        assert!(!c.crossed_day());
    }
}

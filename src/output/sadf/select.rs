//! positional の `interval` / `count` によるサンプル選別 (`sadf [interval [count]]`)。
//!
//! 本家 `sadf` はファイル読み出しでも `interval` / `count` を受け付け、
//! `generic_write_stats()` の先頭で `next_slice()` を通したレコードだけを出す
//! (`sadf.c`)。`count` は**区間 (RESTART で区切られた範囲) ごと**に数え直す。
//!
//! `sar` テキスト側にも同じ判定があるが (`sar_text.rs`)、あちらは非公開で
//! 別担当が編集中のため、`sadf` 側に同じ規則を置く。規則の出典は
//! `sa_common.c: next_slice()` と 03 §1.3.4 で、両者で食い違わせないこと。

/// `interval` / `count` の指定。
///
/// 既定は本家のファイル読み出し時と同じ「全レコード」
/// (`interval < 0` → 1、`count` 未指定 → `-1` = 無制限)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordSelect {
    /// ユーザ指定インターバル (秒)。`1` = 最小インターバル = 全レコード。
    ///
    /// `sadf` は `interval < 1` を usage で弾くので 0 は来ないが、
    /// 取り出し側で 1 に丸める。
    pub interval: u64,
    /// 区間ごとに表示するサンプル数の上限。`None` = 無制限。
    pub count: Option<u64>,
}

impl Default for RecordSelect {
    fn default() -> Self {
        RecordSelect {
            interval: 1,
            count: None,
        }
    }
}

impl RecordSelect {
    /// 有効なインターバル (`0` は 1 として扱う)。
    pub fn interval(self) -> u64 {
        self.interval.max(1)
    }

    /// 「全レコードをそのまま出す」指定か。
    ///
    /// 真なら `next_slice()` は常に真を返すので、表示を省いたレコードの
    /// 前値を控える必要が無い (既定の経路で item 配列の複製を作らない)。
    pub fn selects_every_record(self) -> bool {
        self.interval() == 1
    }
}

/// 1 区間ぶんの `next_slice()` の状態。
///
/// 本家の `last_uptime` は関数内 `static` で、**表示を省いたレコードでも
/// 毎回更新される**。区間の先頭 (`reset`) で基準レコードの uptime に戻る。
#[derive(Debug, Clone)]
pub(crate) struct Slicer {
    interval: u64,
    /// 区間の基準レコードの uptime (本家の `record_hdr[2].uptime_cs`)。
    uptime_ref: u64,
    last_uptime: u64,
    reset: bool,
}

impl Slicer {
    /// 区間の基準レコード (表示しない 1 本目) の uptime で始める。
    pub(crate) fn new(select: RecordSelect, uptime_ref: u64) -> Self {
        Slicer {
            interval: select.interval(),
            uptime_ref,
            last_uptime: 0,
            reset: true,
        }
    }

    /// 基準レコードより後の統計レコード 1 本を判定する。
    ///
    /// 本家は `reset` を統計レコードを読むたびに偽へ落とす
    /// (表示したかどうかに関係なく)。
    pub(crate) fn admit(&mut self, uptime_cs: u64) -> bool {
        let ok = next_slice(
            self.uptime_ref,
            uptime_cs,
            self.reset,
            self.interval,
            &mut self.last_uptime,
        );
        self.reset = false;
        ok
    }
}

/// `sa_common.c: next_slice()` (03 §1.3.4)。
///
/// 「ユーザ指定インターバル `Iu` の整数倍が `[En - In/2, En + In/2)` に入るなら
/// サンプル `En` を表示する」(`In` = ファイル中の実インターバル)。
///
/// 本家の書き方をそのまま写す必要がある箇所:
///
/// 1. uptime の差分は `& 0xffffffff` でマスクしてから秒に直す。
/// 2. 四捨五入は `(f * 10) - (整数部 * 10) >= 5`。
/// 3. `min` / `max` / `pt1` / `pt2` は C の `int` (32bit)。
pub(crate) fn next_slice(
    uptime_ref: u64,
    uptime: u64,
    reset: bool,
    interval: u64,
    last_uptime: &mut u64,
) -> bool {
    if *last_uptime == 0 || reset {
        *last_uptime = uptime_ref;
    }

    // ファイル中の実インターバル (秒、四捨五入)
    let f = ((uptime.wrapping_sub(*last_uptime)) & 0xffff_ffff) as f64 / 100.0;
    let mut file_interval = f as u64;
    if (f * 10.0) - (file_interval as f64 * 10.0) >= 5.0 {
        file_interval += 1;
    }

    *last_uptime = uptime;

    // 最小インターバルなら常に採用
    if interval == 1 {
        return true;
    }

    // 基準点からの経過秒 (四捨五入)
    let f = ((uptime.wrapping_sub(uptime_ref)) & 0xffff_ffff) as f64 / 100.0;
    let mut entry = f as u64;
    if (f * 10.0) - (entry as f64 * 10.0) >= 5.0 {
        entry += 1;
    }

    // ここから下は C の `int` 演算。切り詰めと符号の付き方まで写す。
    let min = entry.wrapping_sub(file_interval / 2) as i32;
    let max = entry
        .wrapping_add(file_interval / 2)
        .wrapping_add(file_interval & 1) as i32;
    let pt1 = (entry / interval).wrapping_mul(interval) as i32;
    let pt2 = (entry / interval + 1).wrapping_mul(interval) as i32;

    (pt1 >= min && pt1 < max) || (pt2 >= min && pt2 < max)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 最小インターバル (既定) は常に採用する。
    #[test]
    fn interval_one_admits_everything() {
        let mut s = Slicer::new(RecordSelect::default(), 1_000);
        for up in [1_000u64, 4_100, 7_300, 9_000] {
            assert!(s.admit(up));
        }
    }

    /// 10 秒間隔のレコードを 60 秒で間引くと、基準から 60 秒ごとの
    /// レコードだけが残る (式: `pt1 = (entry/60)*60 ∈ [entry-5, entry+5)`)。
    #[test]
    fn interval_picks_multiples_of_the_user_interval() {
        let select = RecordSelect {
            interval: 60,
            count: None,
        };
        let mut s = Slicer::new(select, 0);
        let picked: Vec<u64> = (1..=12u64)
            .map(|k| k * 1_000)
            .filter(|&up| s.admit(up))
            .collect();
        assert_eq!(picked, vec![6_000, 12_000]);
    }

    /// 実インターバルが奇数秒のときの `max` は `+1` される (半開区間の右端)。
    #[test]
    fn odd_file_interval_widens_the_right_edge() {
        // 59.5 秒は四捨五入で entry 60、実インターバル 1 秒 (奇数) → [60, 61) に 60 が入る
        assert!(next_slice(0, 5_950, false, 60, &mut 5_850));
        // 59.49 秒 → entry 59、実インターバル 1 秒 → [59, 60) に 60 は入らない
        assert!(!next_slice(0, 5_949, false, 60, &mut 5_849));
    }

    /// uptime の差分は 32bit でマスクしてから秒に直す。
    #[test]
    fn uptime_difference_is_masked_to_32_bits() {
        const WRAP: u64 = 1 << 32;
        assert!(next_slice(0, WRAP + 200, false, 2, &mut (WRAP + 100)));
    }
}

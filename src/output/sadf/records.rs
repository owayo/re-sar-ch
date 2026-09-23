//! 互換出力の表示ループが使うレコード索引。
//!
//! 本家 `sadf` の表示ループは、区間 (RESTART で区切られた範囲) の基準レコードを
//! 探し、activity ごとにそこへ巻き戻して読み直す (`logic2`)。
//! **どこから読み直し、どこで止まり、どの RESTART / COMMENT を出すか**は
//! 統計値をデコードしなくても決まるので、先に 1 回だけ全レコードの見出しを
//! 走査して索引にしておく。統計値のデコードは表示する範囲に限って
//! [`crate::series::walk_items_in`] で行う。
//!
//! 添字は [`crate::series::walk_items_in`] の通し番号と同じ
//! (拡張レコード・無効レコードは数えない)。これが食い違うと、索引で決めた
//! 範囲と実際にデコードされるレコードがずれる。

use crate::error::Result;
use crate::format::file::{SaFile, ScanControl};
use crate::format::registry::RecordKind;
use crate::model::ActivityId;

/// 索引に載るレコードの種別。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecKind {
    /// 統計レコード (`R_STATS` / `R_LAST_STATS` / 旧世代の未知種別)。
    Stats,
    Restart,
    Comment,
}

/// レコード 1 本の見出し。
#[derive(Debug, Clone)]
pub(crate) struct Rec {
    pub kind: RecKind,
    pub ust_time: u64,
    /// 収集時ローカルの時分秒。
    pub hms: (u8, u8, u8),
    /// 1/100 秒単位の稼働時間 (`next_slice()` の入力)。取れない世代は 0。
    pub uptime_cs: u64,
    /// RESTART が運ぶ CPU 数 (`sa_cpu_nr`)。運ばない世代は `None`。
    pub cpu_count: Option<u32>,
    /// COMMENT の本文。非表示文字は `.` に置き換え済み
    /// (`sa_common.c: replace_nonprintable_char()`)。
    pub comment: Option<Box<str>>,
}

/// `-r -O debug` の `# uptime_cs; …` 行に出すレコードヘッダ。
///
/// 本家は `read_record_hdr()` がヘッダを読むたびに出す。拡張レコード
/// (`R_EXTRA_MIN`〜`R_EXTRA_MAX`) も読み飛ばす前に出るので、索引の本体とは
/// 別に持つ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RecHeader {
    pub uptime_cs: u64,
    pub ust_time: u64,
    /// ファイル上の `record_type` の値。
    pub record_type: u8,
    pub hms: (u8, u8, u8),
}

/// ファイル全体のレコード索引。
#[derive(Debug, Default)]
pub(crate) struct RecordIndex {
    pub recs: Vec<Rec>,
    /// `-O debug` のときだけ作る。
    debug: Option<DebugIndex>,
}

/// `-r -O debug` 用の追加情報。
#[derive(Debug, Default)]
struct DebugIndex {
    /// 拡張レコードを含む全レコードのヘッダ (ファイル順)。
    headers: Vec<RecHeader>,
    /// `recs[i]` 自身のヘッダが `headers` のどこにあるか。
    own: Vec<usize>,
    /// 統計レコードの activity 別 item 数 (`nr[curr]`)。並びは `tracked` と同じ。
    /// 統計レコード以外は空。
    nrs: Vec<Vec<u32>>,
    tracked: Vec<ActivityId>,
}

impl RecordIndex {
    /// 索引を作る。
    ///
    /// `debug` に activity の並びを渡すと、`-r -O debug` 用に
    /// レコードヘッダ (拡張レコードを含む) と activity 別 item 数も控える。
    pub(crate) fn build(file: &SaFile, debug: Option<&[ActivityId]>) -> Result<Self> {
        let mut index = RecordIndex {
            recs: Vec::new(),
            debug: debug.map(|ids| DebugIndex {
                tracked: ids.to_vec(),
                ..DebugIndex::default()
            }),
        };
        file.scan(|rec| {
            let hms = (rec.hour, rec.minute, rec.second);
            let uptime_cs = rec.uptime_cs.unwrap_or(0);
            let kind = match rec.kind {
                RecordKind::Restart => Some(RecKind::Restart),
                RecordKind::Comment => Some(RecKind::Comment),
                // 通知しない (walk_items_in と同じく番号を消費しない)
                RecordKind::Extra(_) | RecordKind::Invalid(_) => None,
                RecordKind::Stats | RecordKind::LastStats | RecordKind::UnknownStats(_) => {
                    Some(RecKind::Stats)
                }
            };
            if let Some(d) = index.debug.as_mut() {
                d.headers.push(RecHeader {
                    uptime_cs,
                    ust_time: rec.ust_time,
                    record_type: raw_record_type(rec.kind),
                    hms,
                });
            }
            let Some(kind) = kind else {
                return Ok(ScanControl::Continue);
            };
            if let Some(d) = index.debug.as_mut() {
                d.own.push(d.headers.len() - 1);
                let nrs = if kind == RecKind::Stats {
                    d.tracked
                        .iter()
                        .map(|id| rec.slices.iter().find(|s| s.id == *id).map_or(0, |s| s.nr))
                        .collect()
                } else {
                    Vec::new()
                };
                d.nrs.push(nrs);
            }
            index.recs.push(Rec {
                kind,
                ust_time: rec.ust_time,
                hms,
                uptime_cs,
                cpu_count: rec.cpu_count,
                comment: (kind == RecKind::Comment)
                    .then(|| printable(rec.comment.unwrap_or(b"")).into_boxed_str()),
            });
            Ok(ScanControl::Continue)
        })?;
        Ok(index)
    }

    pub(crate) fn len(&self) -> usize {
        self.recs.len()
    }

    /// `from` 以降で最初の RESTART の添字 (無ければレコード数)。
    pub(crate) fn next_restart(&self, from: usize) -> usize {
        self.recs
            .get(from..)
            .and_then(|rest| rest.iter().position(|r| r.kind == RecKind::Restart))
            .map_or(self.recs.len(), |p| from + p)
    }

    /// `recs[at]` を読んだときに本家が出すヘッダ列
    /// (直前の拡張レコード + 自身)。`at == len()` は EOF で、
    /// 末尾の拡張レコードだけが出る。debug 索引が無ければ空。
    pub(crate) fn headers_for(&self, at: usize) -> &[RecHeader] {
        let Some(d) = &self.debug else {
            return &[];
        };
        let start = match at.checked_sub(1) {
            Some(prev) => d.own.get(prev).map_or(d.headers.len(), |p| p + 1),
            None => 0,
        };
        let end = d.own.get(at).map_or(d.headers.len(), |p| p + 1);
        d.headers.get(start..end).unwrap_or(&[])
    }

    /// 統計レコード `at` での activity の item 数 (`nr[curr]`)。debug 索引専用。
    pub(crate) fn nr_at(&self, at: usize, id: ActivityId) -> Option<u32> {
        let d = self.debug.as_ref()?;
        let col = d.tracked.iter().position(|t| *t == id)?;
        d.nrs.get(at)?.get(col).copied()
    }
}

/// RESTART の CPU 数を「その時点で有効な `sa_cpu_nr`」に解決する。
///
/// ファイルヘッダの値から始め、値を持つ RESTART を読むたびに更新する
/// (本家がメモリ上の `file_hdr.sa_cpu_nr` を書き換えるのと同じ)。
/// `0x2171` の RESTART はペイロードを持たないので、レコードの値だけを見ると
/// 旧世代で `(1 CPU)` になる。
#[derive(Debug, Clone, Copy)]
pub(crate) struct CpuNrTracker(Option<u32>);

impl CpuNrTracker {
    pub(crate) fn new(file: &SaFile) -> Self {
        Self(file.header().cpu_nr)
    }

    /// RESTART 1 件を取り込み、その行に出すべき CPU 数を返す。
    pub(crate) fn take(&mut self, record: Option<u32>) -> Option<u32> {
        if record.is_some() {
            self.0 = record;
        }
        self.0
    }
}

/// ファイル上の `record_type` の値。
fn raw_record_type(kind: RecordKind) -> u8 {
    match kind {
        RecordKind::Stats => RecordKind::R_STATS,
        RecordKind::Restart => RecordKind::R_RESTART,
        RecordKind::LastStats => RecordKind::R_LAST_STATS,
        RecordKind::Comment => RecordKind::R_COMMENT,
        RecordKind::Extra(v) | RecordKind::UnknownStats(v) | RecordKind::Invalid(v) => v,
    }
}

/// 非表示文字を `.` に置き換える (`replace_nonprintable_char()`)。
///
/// 走査層 ([`crate::series::RecordEvent::Comment`]) と同じ規則。
fn printable(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&b| {
            if (0x20..=0x7e).contains(&b) {
                char::from(b)
            } else {
                '.'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(kind: RecKind) -> Rec {
        Rec {
            kind,
            ust_time: 0,
            hms: (0, 0, 0),
            uptime_cs: 0,
            cpu_count: None,
            comment: None,
        }
    }

    #[test]
    fn next_restart_finds_the_boundary() {
        let index = RecordIndex {
            recs: vec![
                rec(RecKind::Stats),
                rec(RecKind::Comment),
                rec(RecKind::Restart),
                rec(RecKind::Stats),
            ],
            debug: None,
        };
        assert_eq!(index.next_restart(0), 2);
        assert_eq!(index.next_restart(2), 2);
        assert_eq!(index.next_restart(3), 4, "無ければレコード数");
        assert_eq!(index.next_restart(9), 4, "範囲外でも落ちない");
    }

    /// 拡張レコードの見出しは直後のレコードを読むときに一緒に出る。
    #[test]
    fn extra_headers_travel_with_the_next_record() {
        let h = |t: u8| RecHeader {
            uptime_cs: 0,
            ust_time: u64::from(t),
            record_type: t,
            hms: (0, 0, 0),
        };
        let index = RecordIndex {
            recs: vec![rec(RecKind::Stats), rec(RecKind::Stats)],
            debug: Some(DebugIndex {
                // STATS, EXTRA(5), STATS, EXTRA(6)
                headers: vec![h(1), h(5), h(1), h(6)],
                own: vec![0, 2],
                nrs: vec![Vec::new(), Vec::new()],
                tracked: Vec::new(),
            }),
        };
        assert_eq!(index.headers_for(0), &[h(1)]);
        assert_eq!(index.headers_for(1), &[h(5), h(1)]);
        assert_eq!(
            index.headers_for(2),
            &[h(6)],
            "EOF では末尾の拡張レコードだけ"
        );
    }

    #[test]
    fn nonprintable_bytes_become_dots() {
        assert_eq!(printable(b"a\tb\x7fc"), "a.b.c");
    }
}

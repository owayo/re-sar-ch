//! 表の下に出す列の説明。
//!
//! # 値を持たない
//!
//! ここに書くのは式と意味だけで、数値はすべて表の列から読む。
//! 説明のために値を組み立て直すと、表と説明で値が食い違う。
//!
//! # 意味を先に、式を後に書く
//!
//! 「旧来の近似」とだけ書いても、何をどう近似したのかは伝わらない。
//! 各列が**何を空き (または使用中) と数えた値か**を先に書き、式を添える。
//! 4 列は同じ式ではない (`kbmemfree_withcache` は cache を空きに、
//! `memused_withcache_pct` は使用中に数える) ので、まとめて 1 つの説明にしない。
//!
//! # 記号は ASCII で書く
//!
//! `−` (U+2212)・`×`・`…`・`※` は East Asian Width が曖昧で、日本語環境の端末では
//! 2 桁に描かれる。列名の桁揃えが崩れるので、式には `-` `+` `/` を使う。

/// 1 列の説明。
pub struct ColumnNote {
    /// 列の公開名 (`FieldOut::name`、表の見出しと同じ表記)。
    pub column: &'static str,
    /// 全文。
    pub text: &'static str,
    /// 幅が足りないときの短い版。**何を空き (使用中) と数えた値か**は残す。
    pub short: &'static str,
}

/// activity 1 種ぶんの説明。
pub struct NoteSet {
    /// 対象の activity (`A_MEMORY`)。
    pub activity: &'static str,
    /// 説明する列 (表示は表の列順に並べ直す)。
    pub notes: &'static [ColumnNote],
    /// 説明の最後に添える注意 (全文 / 短い版)。
    pub caveat: &'static str,
    pub caveat_short: &'static str,
    /// 列ごとに並べられないとき (端末が低い・狭い) の 1 行。
    /// 説明の全文への道筋 (`?` のヘルプ) を必ず示す。
    pub summary: &'static str,
    pub summary_short: &'static str,
}

/// MEMORY に足した 4 列の説明。
///
/// どれも buffers と cache を一律に空き (または使用中) と見なす近似である。
/// **正確な値とは書かない。** カーネルが `MemAvailable` を加えたのは、
/// free と cached を足す見積もりが誤るからである (commit 34e431b0)。
pub static MEMORY_NOTES: NoteSet = NoteSet {
    activity: "A_MEMORY",
    notes: &[
        ColumnNote {
            column: "kbmemused_nocache",
            text: "buffers/cache を空きに数えた使用量 = kbmemtotal - kbmemfree - kbbuffers - kbcached (負なら 0)",
            short: "cache を空きに数えた使用量 (total-free-buffers-cached)",
        },
        ColumnNote {
            column: "memused_nocache_pct",
            text: "buffers/cache を空きに数えた使用率 = kbmemused_nocache / kbmemtotal (%)",
            short: "cache を空きに数えた使用率 (%)",
        },
        ColumnNote {
            column: "kbmemfree_withcache",
            text: "buffers/cache も空きに数えた空き量 = kbmemfree + kbbuffers + kbcached",
            short: "cache も空きに数えた空き量 (free+buffers+cached)",
        },
        ColumnNote {
            column: "memused_withcache_pct",
            text: "buffers/cache も使用中に数えた使用率 = (kbmemtotal - kbmemfree) / kbmemtotal (%)。sysstat 11.7.4 より前の sar の %memused",
            short: "cache も使用中に数えた使用率 (旧 sar の %memused)",
        },
    ],
    caveat: "注: どれも buffers/cache を一律に扱う近似 (cached には回収できない共有メモリ・tmpfs も入る)。正確な空きは kbavail (MemAvailable)",
    caveat_short: "注: cache を一律に扱う近似。正確な空きは kbavail",
    summary: "nocache / withcache の列は buffers/cache を一律に扱う近似 (列ごとの式は ? のヘルプ。正確な空きは kbavail)",
    summary_short: "列の説明は ? のヘルプ",
};

/// activity ごとの説明。
static NOTE_SETS: &[&NoteSet] = &[&MEMORY_NOTES];

/// その activity の説明。
pub fn notes_for(activity: &str) -> Option<&'static NoteSet> {
    NOTE_SETS.iter().copied().find(|s| s.activity == activity)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::registry::lookup;
    use crate::model::ActivityId;

    /// 説明する列は layout に実在する。
    ///
    /// 列名は文字列で持つので、layout 側で名前が変わると説明が黙って消える。
    #[test]
    fn every_note_names_a_layout_column() {
        assert_eq!(ActivityId::MEMORY.symbol(), Some(MEMORY_NOTES.activity));
        let def = lookup(ActivityId::MEMORY).expect("MEMORY の layout");
        for n in MEMORY_NOTES.notes {
            assert!(
                def.columns.iter().any(|c| c.public_name == n.column),
                "{}: layout に無い列",
                n.column
            );
        }
    }

    /// 端末によって 2 桁に描かれる記号を使わない (桁揃えが崩れる)。
    #[test]
    fn notes_avoid_ambiguous_width_symbols() {
        let texts = MEMORY_NOTES
            .notes
            .iter()
            .flat_map(|n| [n.text, n.short])
            .chain([
                MEMORY_NOTES.caveat,
                MEMORY_NOTES.caveat_short,
                MEMORY_NOTES.summary,
                MEMORY_NOTES.summary_short,
            ]);
        for t in texts {
            for c in ['−', '×', '…', '※', '■', '―'] {
                assert!(!t.contains(c), "{t:?} に {c:?}");
            }
        }
    }

    /// 短い版は全文より短い (短くならない短い版は、狭い端末で役に立たない)。
    #[test]
    fn short_texts_are_shorter() {
        for n in MEMORY_NOTES.notes {
            assert!(
                n.short.chars().count() < n.text.chars().count(),
                "{}",
                n.column
            );
        }
        assert!(MEMORY_NOTES.caveat_short.chars().count() < MEMORY_NOTES.caveat.chars().count());
        assert!(MEMORY_NOTES.summary_short.chars().count() < MEMORY_NOTES.summary.chars().count());
    }
}

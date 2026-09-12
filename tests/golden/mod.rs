//! 期待出力との全文比較 (差分の要約とマスク処理)。
//!
//! ここには**出力形式に依存しない**比較機構だけを置く。
//! 「どのケースをどう再現するか」は [`crate`] 側 (`tests/conformance.rs`) の責務。
//!
//! # マスクの考え方
//!
//! 本家の期待出力には、**ファイルの中身だけでは原理的に再現できない値**が混じる
//! (読み手のタイムゾーン、ローカルの `/dev` 解決結果、実行環境の CPU 数など)。
//! こうした箇所は「行ごと捨てる」のではなく、**その部分文字列だけを置換**して
//! 残りを全文比較する。どの語を潰したかは [`Masked`] に理由つきで記録し、
//! 報告に必ず出す。
//!
//! 「実装が面倒」「値が合わない」はマスクの理由にならない。

use std::fmt::Write as _;

// ===========================================================================
// マスク
// ===========================================================================

/// 1 行の中で、どの語を可変部分と見なすか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    /// タブで分割した `index` 番目のフィールドを、空白で分割した `word` 番目の語。
    ///
    /// 前後の空白・タブはそのまま残るので、**区切り文字の検証は維持される**。
    TabWord { index: usize, word: usize },
}

/// 可変部分の置換規則。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mask {
    /// 適用対象の行 (この接頭辞で始まる行だけを対象にする)。
    pub line_prefix: &'static str,
    /// 置換する語。
    pub field: Field,
    /// なぜファイルから再現できないのか。**報告に出るので必須**。
    pub reason: &'static str,
}

/// 実際に置換が起きた記録。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Masked {
    /// 対象行 (1 起点)。
    pub line: usize,
    /// 期待出力にあった値。
    pub expected: String,
    /// reSARch が出した値。
    pub actual: String,
    /// 理由 ([`Mask::reason`])。
    pub reason: &'static str,
}

/// マスク後の置換文字 (どちらの側にも同じ物を入れる)。
const PLACEHOLDER: &str = "\u{1}masked\u{1}";

impl Mask {
    /// 行が対象か。
    fn applies(&self, line: &str) -> bool {
        line.starts_with(self.line_prefix)
    }

    /// 対象語を取り出す。語が無ければ `None` (マスクは起きない)。
    fn word_of(&self, line: &str) -> Option<String> {
        let Field::TabWord { index, word } = self.field;
        let field = line.split('\t').nth(index)?;
        field
            .split(' ')
            .filter(|w| !w.is_empty())
            .nth(word)
            .map(str::to_string)
    }

    /// 対象語を [`PLACEHOLDER`] に置き換えた行を返す。
    fn apply(&self, line: &str) -> String {
        let Some(target) = self.word_of(line) else {
            return line.to_string();
        };
        let Field::TabWord { index, .. } = self.field;
        let mut out = String::with_capacity(line.len() + PLACEHOLDER.len());
        for (i, field) in line.split('\t').enumerate() {
            if i > 0 {
                out.push('\t');
            }
            if i == index {
                // 同じ語が 1 フィールド内に複数あっても、置換するのは最初の 1 個だけ
                out.push_str(&field.replacen(&target, PLACEHOLDER, 1));
            } else {
                out.push_str(field);
            }
        }
        out
    }
}

// ===========================================================================
// 比較
// ===========================================================================

/// 1 行分の食い違い。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineDiff {
    /// 行番号 (1 起点)。
    pub line: usize,
    /// 期待出力の行 (無ければ `None` = 実際の側にだけある)。
    pub expected: Option<String>,
    /// reSARch の行 (無ければ `None` = 期待の側にだけある)。
    pub actual: Option<String>,
}

/// 比較結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comparison {
    /// 期待出力の行数。
    pub expected_lines: usize,
    /// reSARch 出力の行数。
    pub actual_lines: usize,
    /// マスクした箇所。
    pub masked: Vec<Masked>,
    /// 食い違った行。
    pub diffs: Vec<LineDiff>,
}

impl Comparison {
    /// 一致したか (マスク込み)。
    pub fn is_match(&self) -> bool {
        self.diffs.is_empty()
    }

    /// 一行の判定ラベル。
    pub fn verdict(&self) -> String {
        if !self.diffs.is_empty() {
            format!(
                "不一致 (差分 {} 行 / 期待 {} 行・実際 {} 行)",
                self.diffs.len(),
                self.expected_lines,
                self.actual_lines
            )
        } else if self.masked.is_empty() {
            format!("全文一致 ({} 行)", self.expected_lines)
        } else {
            format!(
                "{} 行マスクして一致 ({} 行)",
                self.masked.len(),
                self.expected_lines
            )
        }
    }

    /// 差分の詳細 (先頭 `limit` 件)。期待値と実際の値を 1 行ずつ並べる。
    pub fn diff_report(&self, limit: usize) -> String {
        let mut s = String::new();
        for d in self.diffs.iter().take(limit) {
            let _ = writeln!(s, "    {} 行目", d.line);
            let _ = writeln!(s, "      期待: {}", show(d.expected.as_deref()));
            let _ = writeln!(s, "      実際: {}", show(d.actual.as_deref()));
        }
        if self.diffs.len() > limit {
            let _ = writeln!(s, "    ... ほか {} 行", self.diffs.len() - limit);
        }
        s
    }

    /// マスクの詳細。
    pub fn mask_report(&self) -> String {
        let mut s = String::new();
        for m in &self.masked {
            let _ = writeln!(
                s,
                "    {} 行目: 期待 {:?} / 実際 {:?} — {}",
                m.line, m.expected, m.actual, m.reason
            );
        }
        s
    }
}

/// 制御文字とタブを見える形にする (差分を目で追えるようにするため)。
fn show(line: Option<&str>) -> String {
    match line {
        None => "(行なし)".to_string(),
        Some(l) => {
            let mut s = String::with_capacity(l.len() + 8);
            s.push('"');
            for c in l.chars() {
                match c {
                    '\t' => s.push_str("\\t"),
                    '\r' => s.push_str("\\r"),
                    c if (c as u32) < 0x20 => {
                        let _ = write!(s, "\\x{:02x}", c as u32);
                    }
                    c => s.push(c),
                }
            }
            s.push('"');
            s
        }
    }
}

/// 末尾の改行だけを落として行へ分割する (空行は保つ)。
fn lines_of(text: &str) -> Vec<&str> {
    let body = text.strip_suffix('\n').unwrap_or(text);
    if body.is_empty() {
        return Vec::new();
    }
    body.split('\n').collect()
}

/// 期待出力と実際の出力を**全文比較**する。
///
/// 行数が違っても、行番号をずらさずに突き合わせる (先に出た差分から原因を追える)。
pub fn compare(expected: &str, actual: &str, masks: &[Mask]) -> Comparison {
    let exp = lines_of(expected);
    let act = lines_of(actual);
    let mut cmp = Comparison {
        expected_lines: exp.len(),
        actual_lines: act.len(),
        masked: Vec::new(),
        diffs: Vec::new(),
    };

    for i in 0..exp.len().max(act.len()) {
        match (exp.get(i), act.get(i)) {
            (Some(e), Some(a)) => {
                if e == a {
                    continue;
                }
                // そのままでは違う行にだけマスクを試す。
                // 一致している行を潰さないので、マスクが検証を薄める範囲を最小にできる。
                let mut me = (*e).to_string();
                let mut ma = (*a).to_string();
                let mut applied: Vec<Masked> = Vec::new();
                for mask in masks {
                    if !mask.applies(e) || !mask.applies(a) {
                        continue;
                    }
                    let (we, wa) = (mask.word_of(&me), mask.word_of(&ma));
                    let (Some(we), Some(wa)) = (we, wa) else {
                        continue;
                    };
                    if we == wa {
                        continue;
                    }
                    me = mask.apply(&me);
                    ma = mask.apply(&ma);
                    applied.push(Masked {
                        line: i + 1,
                        expected: we,
                        actual: wa,
                        reason: mask.reason,
                    });
                }
                if me == ma {
                    cmp.masked.extend(applied);
                } else {
                    cmp.diffs.push(LineDiff {
                        line: i + 1,
                        expected: Some((*e).to_string()),
                        actual: Some((*a).to_string()),
                    });
                }
            }
            (e, a) => cmp.diffs.push(LineDiff {
                line: i + 1,
                expected: e.map(|s| (*s).to_string()),
                actual: a.map(|s| (*s).to_string()),
            }),
        }
    }
    cmp
}

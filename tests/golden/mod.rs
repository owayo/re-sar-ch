//! 期待出力との全文比較 (差分の要約とマスク処理)。
//!
//! ここには**出力形式に依存しない**比較機構だけを置く。
//! 「どのケースをどう再現するか」は `tests/conformance.rs` の責務。
//!
//! # マスクの考え方
//!
//! 本家の期待出力には、**ファイルの中身だけでは原理的に再現できない値**が混じる
//! (読み手のタイムゾーン、`/dev` の解決結果など)。こうした箇所は「行ごと捨てる」
//! のではなく、**その語 (または固定幅の 1 列) だけを置換**して残りを全文比較する。
//! 置換した箇所は [`Masked`] に理由つきで記録し、報告に必ず出す。
//!
//! 「実装が面倒」「値が合わない」はマスクの理由にならない。
//! マスクは**一致しなかった行にだけ**試すので、一致している行の検証は薄まらない。

use std::fmt::Write as _;

// ===========================================================================
// マスク
// ===========================================================================

/// 可変部分の置換規則。
///
/// 各規則は「期待行と実際の行の組」を受け取り、可変部分だけを潰した組を返す。
/// 潰す対象が見つからない / 潰しても差が残る場合は不一致のまま報告される。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mask {
    /// `sadf -H` の `Host:` 行に出る `MM/DD/YY`。
    ///
    /// 本家 (`sadf_misc.c: print_hdr_header()` → `localtime_r(&sa_ust_time)`) は
    /// **読み手のタイムゾーン**で日付を作る。期待出力は `TZ=GMT` あるいは生成環境の
    /// `TZ` で作られており、ファイルの中身だけからは再現できない。
    /// 日付の語だけを潰し、区切り (空白 + タブ) と他の語は比較したまま残す。
    HostLineDate,

    /// `A_DISK` のデバイス名列 (`sar -d` の `DEV` 列)。
    ///
    /// 本家は `major:minor` を**実行ホストの `/dev` / `/sys`** で名前へ解決する
    /// (`ioconf.c` / `get_persistent_name()`)。期待出力の `sda` `sda1` … は
    /// 期待出力を作ったホストの構成であり、`sa` ファイルには major/minor しか
    /// 入っていないので再現できない。reSARch は他ホストのファイルを誤名で
    /// 表示しないよう、あえて `dev<major>-<minor>` のまま出す
    /// (`src/output/sar_text.rs` の「既知の未実装 / 差異」表)。
    ///
    /// **実際の側が `dev<数字>-<数字>` の場合だけ**、その 1 列 (幅 10 バイト) を潰す。
    DiskDeviceName,
}

/// 行内の固定幅 1 列 (`%-11s` のタイムスタンプ列に続く ` %9s`)。
const ITEM_COL: std::ops::Range<usize> = 11..21;

/// マスク後の置換文字 (どちらの側にも同じ物を入れる)。
const PLACEHOLDER: &str = "\u{1}masked\u{1}";

/// マスクが 1 箇所で成立したときの記録。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Masked {
    /// 対象行 (1 起点)。
    pub line: usize,
    /// 期待出力側の値。
    pub expected: String,
    /// reSARch 側の値。
    pub actual: String,
    /// 理由。
    pub reason: &'static str,
}

impl Mask {
    /// なぜファイルから再現できないのか (報告に出す)。
    pub fn reason(self) -> &'static str {
        match self {
            Mask::HostLineDate => {
                "`Host:` 行の日付は localtime(sa_ust_time) 由来で読み手の TZ に依存する"
            }
            Mask::DiskDeviceName => {
                "A_DISK のデバイス名は major:minor を実行ホストの /dev で解決した結果で、\
                 sa ファイルには入っていない"
            }
        }
    }

    /// 期待行と実際の行から可変部分を潰す。
    ///
    /// 戻り値は `(潰した期待行, 潰した実際の行, 期待側の値, 実際側の値)`。
    fn apply(self, exp: &str, act: &str) -> Option<(String, String, String, String)> {
        match self {
            Mask::HostLineDate => {
                if !exp.starts_with("Host: ") || !act.starts_with("Host: ") {
                    return None;
                }
                // `Host: <sysname> <release> (<nodename>) \t<MM/DD/YY> \t_<machine>_\t(<N> CPU)`
                let we = tab_word(exp, 1, 0)?;
                let wa = tab_word(act, 1, 0)?;
                if we == wa {
                    return None;
                }
                Some((
                    exp.replacen(&we, PLACEHOLDER, 1),
                    act.replacen(&wa, PLACEHOLDER, 1),
                    we,
                    wa,
                ))
            }
            Mask::DiskDeviceName => {
                let (ce, ca) = (column(exp, ITEM_COL)?, column(act, ITEM_COL)?);
                // 実際の側が major-minor 由来の既定名でなければ対象外
                if !is_dev_major_minor(ca.trim()) || ce.trim() == ca.trim() {
                    return None;
                }
                Some((
                    replace_column(exp, ITEM_COL),
                    replace_column(act, ITEM_COL),
                    ce.trim().to_string(),
                    ca.trim().to_string(),
                ))
            }
        }
    }
}

/// タブ区切り `field` 番目のフィールドを空白で割った `word` 番目の語。
fn tab_word(line: &str, field: usize, word: usize) -> Option<String> {
    line.split('\t')
        .nth(field)?
        .split(' ')
        .filter(|w| !w.is_empty())
        .nth(word)
        .map(str::to_string)
}

/// 固定幅 1 列を切り出す (行が短ければ `None`)。
fn column(line: &str, range: std::ops::Range<usize>) -> Option<&str> {
    line.get(range)
}

/// 固定幅 1 列を [`PLACEHOLDER`] に置き換える。
fn replace_column(line: &str, range: std::ops::Range<usize>) -> String {
    let mut out = String::with_capacity(line.len() + PLACEHOLDER.len());
    out.push_str(&line[..range.start]);
    out.push_str(PLACEHOLDER);
    out.push_str(&line[range.end..]);
    out
}

/// `dev<major>-<minor>` 形式か (reSARch の A_DISK 既定名)。
fn is_dev_major_minor(s: &str) -> bool {
    let Some(rest) = s.strip_prefix("dev") else {
        return false;
    };
    let Some((major, minor)) = rest.split_once('-') else {
        return false;
    };
    !major.is_empty()
        && !minor.is_empty()
        && major.bytes().all(|b| b.is_ascii_digit())
        && minor.bytes().all(|b| b.is_ascii_digit())
}

// ===========================================================================
// 比較
// ===========================================================================

/// 1 行分の食い違い。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineDiff {
    /// 行番号 (1 起点)。
    pub line: usize,
    /// 期待出力の行 (`None` = 実際の側にだけある)。
    pub expected: Option<String>,
    /// reSARch の行 (`None` = 期待の側にだけある)。
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
                "不一致 (差分 {} 行 / 期待 {} 行・実際 {} 行{})",
                self.diffs.len(),
                self.expected_lines,
                self.actual_lines,
                if self.masked.is_empty() {
                    String::new()
                } else {
                    format!("・マスク {} 行", self.masked.len())
                }
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

    /// マスクの詳細 (同じ理由はまとめる)。
    pub fn mask_report(&self) -> String {
        let mut s = String::new();
        let mut reasons: Vec<&'static str> = Vec::new();
        for m in &self.masked {
            if !reasons.contains(&m.reason) {
                reasons.push(m.reason);
            }
        }
        for reason in reasons {
            let hits: Vec<&Masked> = self.masked.iter().filter(|m| m.reason == reason).collect();
            let sample = hits[0];
            let _ = writeln!(
                s,
                "    マスク {} 行: {} (例: {} 行目 期待 {:?} / 実際 {:?})",
                hits.len(),
                reason,
                sample.line,
                sample.expected,
                sample.actual
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

/// 末尾の改行だけを落として行へ分割する (途中の空行は保つ)。
fn lines_of(text: &str) -> Vec<&str> {
    let body = text.strip_suffix('\n').unwrap_or(text);
    if body.is_empty() {
        return Vec::new();
    }
    body.split('\n').collect()
}

/// 期待出力と実際の出力を**全文比較**する。
///
/// 行数が違っても行番号をずらさずに突き合わせる (最初の食い違いから原因を追える)。
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
                // 食い違った行にだけマスクを試す
                let mut me = (*e).to_string();
                let mut ma = (*a).to_string();
                let mut hits: Vec<Masked> = Vec::new();
                for mask in masks {
                    if let Some((ne, na, we, wa)) = mask.apply(&me, &ma) {
                        me = ne;
                        ma = na;
                        hits.push(Masked {
                            line: i + 1,
                            expected: we,
                            actual: wa,
                            reason: mask.reason(),
                        });
                    }
                }
                if me == ma {
                    cmp.masked.extend(hits);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// マスク無しなら全文一致 / 不一致がそのまま出る。
    #[test]
    fn compares_line_by_line() {
        let c = compare("a\nb\n", "a\nb\n", &[]);
        assert!(c.is_match() && c.masked.is_empty());

        let c = compare("a\nb\n", "a\nc\n", &[]);
        assert_eq!(c.diffs.len(), 1);
        assert_eq!(c.diffs[0].line, 2);
    }

    /// 行数が違う場合も行番号をずらさずに報告する。
    #[test]
    fn reports_missing_and_extra_lines() {
        let c = compare("a\n", "a\nb\n", &[]);
        assert_eq!(c.diffs.len(), 1);
        assert_eq!(c.diffs[0].expected, None);
        assert_eq!(c.diffs[0].actual.as_deref(), Some("b"));
    }

    /// `Host:` 行の日付だけが潰れ、他の語は比較されたまま残る。
    #[test]
    fn host_line_date_is_masked_but_the_rest_is_not() {
        let e = "Host: Linux 5.0 (node) \t09/15/19 \t_x86_64_\t(2 CPU)\n";
        let a = "Host: Linux 5.0 (node) \t10/15/19 \t_x86_64_\t(2 CPU)\n";
        let c = compare(e, a, &[Mask::HostLineDate]);
        assert!(c.is_match());
        assert_eq!(c.masked.len(), 1);
        assert_eq!(c.masked[0].expected, "09/15/19");

        // CPU 数が違えばマスクしても一致しない
        let a2 = "Host: Linux 5.0 (node) \t10/15/19 \t_x86_64_\t(4 CPU)\n";
        assert!(!compare(e, a2, &[Mask::HostLineDate]).is_match());
    }

    /// `dev<major>-<minor>` の列だけが潰れる。値列は潰れない。
    #[test]
    fn disk_name_column_is_masked_only_for_the_default_name() {
        let e = "05:39:34          sda      0.00      1.00\n";
        let a = "05:39:34       dev8-0      0.00      1.00\n";
        let c = compare(e, a, &[Mask::DiskDeviceName]);
        assert!(c.is_match());
        assert_eq!(c.masked[0].actual, "dev8-0");

        // 値が違えば不一致のまま
        let a2 = "05:39:34       dev8-0      0.00      2.00\n";
        assert!(!compare(e, a2, &[Mask::DiskDeviceName]).is_match());

        // 実際の側が解決済みの名前ならマスクしない (取り違えの検出を残す)
        let a3 = "05:39:34          sdb      0.00      1.00\n";
        assert!(!compare(e, a3, &[Mask::DiskDeviceName]).is_match());
    }
}

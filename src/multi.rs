//! 複数ファイル横断 — `sa01`..`sa31` を 1 つの時系列として扱う。
//!
//! # 方針 (`docs/design.md` §6.3)
//!
//! 1. 各ファイルのヘッダだけを先に読み、ホスト・日付・世代を把握する
//! 2. **ファイル単位**で rayon 並列デコードする (activity 単位では並列化しない)
//! 3. 同一ホストのレコードを**決定的順序**でマージする
//! 4. **差分をファイル単位で完結させない** — 日境界で連続性が確認できる場合、
//!    前ファイルの最終サンプルを次ファイルの基準値として引き継ぐ
//! 5. 全結果を `collect` せず、同時処理ファイル数に上限を設ける
//!
//! # 同一マシンの判定
//!
//! **ホスト名 (`nodename`) だけで同一マシンと断定しない。** 名前の付け替えや
//! 使い回しがあるため、`sysname` / `release` / `machine` / `cpu_nr` も見て
//! [`IdentityComparison`] に判定材料を残す。
//!
//! | 判定 | 意味 | 系列の扱い |
//! |---|---|---|
//! | [`IdentityVerdict::Identical`] | 全項目一致 | 同一系列 |
//! | [`IdentityVerdict::LikelySameMachine`] | `release` / `cpu_nr` だけ違う | 同一ホストだが**起動区間を分ける** |
//! | [`IdentityVerdict::Ambiguous`] | `nodename` は一致、`machine` / `sysname` が違う | 別マシンの可能性。起動区間を分ける |
//! | [`IdentityVerdict::Different`] | `nodename` が違う | 別ホスト |
//!
//! **この判定は境界ごとに実際に効く。** マージ側は直前サンプルのファイルと
//! 当該ファイルの識別材料を比べ、`Identical` 以外なら
//! [`BreakReason::IdentityChanged`] として差分を切る。比較相手は
//! 「グループ代表」ではなく**直前サンプル**である (`A → B → B → A` と
//! 構成が戻る並びで、切るべき境界だけを切るため)。
//!
//! 実際にマージ経路へ来るのは `Identical` と `LikelySameMachine` がほとんどで、
//! `Ambiguous` / `Different` は [`HostIdentity::group_key`] の段階で
//! 別グループに分かれる (`machine` / `sysname` / `nodename` が鍵に入っている)。
//! それでも判定を渡すのは、グループ分けと境界判定が別の鍵で動いており、
//! 片方だけを信用すると診断と挙動が食い違うためである。
//!
//! # 連続性が不明なら差分を作らない
//!
//! 日境界の引き継ぎは、次のすべてを確認できたときだけ行う。
//! 1 つでも確認できなければ [`BreakReason`] を付けて不連続として返す。
//!
//! - 起動時刻の推定値 (`ust_time − uptime`) が一致する
//! - 時刻が逆行していない
//! - 空白が許容範囲に収まる
//! - activity のレイアウト (revision・item サイズ・フィールド数) が一致する
//!
//! # 時刻で絞るとき (`summarize --from` / `--to`)
//!
//! 集計の時刻フィルタは [`MultiOptions::time_filter`] として受け取り、
//! **マージ側 (この層) で適用する**。デコードはファイル単位で並列に走るため、
//! そこで絞ると「範囲に最初に合致したレコード」の判定 (`sar -s` の意味論) が
//! ファイルごとに独立してしまい、並列度で結果が変わる。
//!
//! 絞り方の詳細は [`MultiOptions::time_filter`] を参照。
//!
//! # 使い方
//!
//! ```no_run
//! use std::path::PathBuf;
//! use re_sar_ch::multi::{MultiOptions, analyze_files};
//!
//! let paths: Vec<PathBuf> = (1..=3).map(|d| PathBuf::from(format!("sa0{d}"))).collect();
//! let result = analyze_files(&paths, &MultiOptions::default())?;
//! for host in &result.hosts {
//!     for seg in &host.segments {
//!         // 起動区間ごとに独自サマリとルール判定が付く
//!         let _ = (&seg.summary, &seg.findings, &seg.boundaries);
//!     }
//! }
//! # Ok::<(), re_sar_ch::Error>(())
//! ```

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rayon::prelude::*;
use serde::Serialize;

use crate::analyze::rules::{Finding, RuleContext, evaluate};
use crate::analyze::summary::{
    NativePeriodSummary, NativeSummaryBuilder, SummaryOptions, SummarySource,
};
use crate::analyze::timeline::{MetricKey, MetricTimeline};
use crate::error::Result;
use crate::format::file::{FileHeader, OpenOptions, SaFile, ScanControl};
use crate::layout::plan::DecodePlan;
use crate::model::{ActivityId, ValueKind};
use crate::output::time_filter::{Admit, TimeFilter};
use crate::series::delta::interval_cs;
use crate::series::snapshot::{
    ActivityPlan, IntervalView, RecordEvent, Selection, Snapshot, WalkItem, walk_items,
};

/// 複数ファイル横断の出力スキーマ版。
pub const MULTI_SCHEMA_VERSION: &str = "1";

// ===========================================================================
// ホストの同一性
// ===========================================================================

/// ホストの識別材料。
///
/// ファイルヘッダの申告値をそのまま持つ。**実行中のホストの値ではない。**
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct HostIdentity {
    pub nodename: String,
    pub sysname: String,
    pub release: String,
    pub machine: String,
    pub cpu_nr: Option<u32>,
}

impl HostIdentity {
    pub fn from_header(h: &FileHeader) -> Self {
        Self {
            nodename: h.nodename.clone(),
            sysname: h.sysname.clone(),
            release: h.release.clone(),
            machine: h.machine.clone(),
            cpu_nr: h.cpu_nr,
        }
    }

    /// 「同一マシンの候補」としてまとめる鍵。
    ///
    /// `release` と `cpu_nr` は**含めない** (カーネル更新や CPU 数の変化でも
    /// 同じホストの系列として扱い、起動区間の分割で対処する)。
    /// 逆に `machine` / `sysname` が違うものは同じホスト扱いにしない。
    ///
    /// **この鍵は「差分を引き継いでよいか」の判断には使えない。**
    /// そちらは [`HostIdentity::continuity_key`] を見る。
    pub fn group_key(&self) -> (String, String, String) {
        (
            self.nodename.clone(),
            self.sysname.clone(),
            self.machine.clone(),
        )
    }

    /// 「カウンタの差分を引き継いでよいか」を決める鍵。
    ///
    /// [`HostIdentity::group_key`] と違い **`release` と `cpu_nr` を含める**。
    /// この 2 つは別の判断であり、混ぜると診断と挙動が矛盾する。
    ///
    /// | 判断 | 鍵 | 違ったときの扱い |
    /// |---|---|---|
    /// | 同じホストの系列か | [`group_key`](HostIdentity::group_key) | 別ホストとして分ける |
    /// | 差分を引き継げるか | `continuity_key` | 同じホストのまま**起動区間を分ける** |
    ///
    /// カーネル更新や CPU 数の変化は「同じマシンだが再起動を挟んだ」ことを
    /// 意味するので、系列は 1 つのまま保ちつつ差分は切る。
    /// 文字列を確保しないよう参照のタプルを返す (ファイル境界ごとに引く)。
    pub fn continuity_key(&self) -> (&str, &str, &str, &str, Option<u32>) {
        (
            &self.nodename,
            &self.sysname,
            &self.machine,
            &self.release,
            self.cpu_nr,
        )
    }
}

/// 同一性の判定。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityVerdict {
    /// 全項目が一致した。
    Identical,
    /// `nodename` / `sysname` / `machine` は一致し、`release` か `cpu_nr` が違う。
    ///
    /// カーネル更新や CPU の増減が考えられる。同一マシンの可能性が高いが、
    /// **その間に再起動があった**はずなので差分は引き継がない。
    LikelySameMachine,
    /// `nodename` は一致するが `sysname` / `machine` が違う。
    ///
    /// ホスト名の使い回しで別マシンの可能性がある。同一視しない。
    Ambiguous,
    /// `nodename` が違う。
    Different,
}

/// 項目ごとの一致状況。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IdentityFieldMatch {
    pub field: &'static str,
    pub left: String,
    pub right: String,
    pub agrees: bool,
}

/// 同一性の判定材料をまとめたもの。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IdentityComparison {
    pub verdict: IdentityVerdict,
    pub fields: Vec<IdentityFieldMatch>,
    /// 判定に添える注記。
    pub notes: Vec<String>,
}

impl IdentityComparison {
    /// 差分を引き継いでよい同一性か。
    pub fn allows_delta_carry(&self) -> bool {
        self.verdict == IdentityVerdict::Identical
    }
}

fn field_match(field: &'static str, left: &str, right: &str) -> IdentityFieldMatch {
    IdentityFieldMatch {
        field,
        left: left.to_string(),
        right: right.to_string(),
        agrees: left == right,
    }
}

/// 2 つのヘッダ由来の識別材料を比較する。
///
/// **ホスト名の一致だけで同一マシンと断定しない。**
pub fn compare_identity(a: &HostIdentity, b: &HostIdentity) -> IdentityComparison {
    let cpu = |v: Option<u32>| v.map(|n| n.to_string()).unwrap_or_else(|| "-".to_string());
    let fields = vec![
        field_match("nodename", &a.nodename, &b.nodename),
        field_match("sysname", &a.sysname, &b.sysname),
        field_match("machine", &a.machine, &b.machine),
        field_match("release", &a.release, &b.release),
        field_match("cpu_nr", &cpu(a.cpu_nr), &cpu(b.cpu_nr)),
    ];
    let agrees = |name: &str| {
        fields
            .iter()
            .find(|f| f.field == name)
            .is_some_and(|f| f.agrees)
    };

    let mut notes = Vec::new();
    let verdict = if !agrees("nodename") {
        IdentityVerdict::Different
    } else if !agrees("machine") || !agrees("sysname") {
        notes.push(
            "ホスト名は一致するがアーキテクチャ / OS 名が違う。名前の使い回しの可能性があるため同一マシンと見なさない".to_string(),
        );
        IdentityVerdict::Ambiguous
    } else if !agrees("release") || !agrees("cpu_nr") {
        if !agrees("release") {
            notes.push(
                "カーネル版が違う。同一マシンでも再起動を挟んでいるので差分は引き継がない"
                    .to_string(),
            );
        }
        if !agrees("cpu_nr") {
            notes.push(
                "CPU 数の申告値が違う。構成変更または再起動を挟んでいる可能性がある".to_string(),
            );
        }
        IdentityVerdict::LikelySameMachine
    } else {
        IdentityVerdict::Identical
    };

    IdentityComparison {
        verdict,
        fields,
        notes,
    }
}

/// 2 つの識別材料の判定だけを求める (判定材料の一覧は作らない)。
///
/// サンプル境界ごとに呼ぶため、[`compare_identity`] の
/// `IdentityFieldMatch` (5 項目 × 2 本の `String`) を毎回確保しない。
/// 同一ファイル内および構成が変わっていないファイル間では
/// [`HostIdentity::continuity_key`] の比較だけで済み、確保は起きない。
fn identity_verdict(prev: &HostIdentity, next: &HostIdentity) -> IdentityVerdict {
    if prev.continuity_key() == next.continuity_key() {
        return IdentityVerdict::Identical;
    }
    compare_identity(prev, next).verdict
}

// ===========================================================================
// レイアウトの署名
// ===========================================================================

/// activity 1 種のレイアウト署名。
///
/// 前ファイルの値をそのまま基準値として使えるのは、
/// フィールドの並びと幅が完全に同じときだけ。
/// sysstat の版が違うファイルが並んでいる可能性があるため必ず確認する。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ActivitySignature {
    pub id: ActivityId,
    pub magic: u32,
    pub types_nr: Option<[u32; 3]>,
    /// `file_activity.size` の申告値 (= ストライド)。
    pub item_size: u32,
    /// デコード計画のフィールド数。
    pub fields: usize,
}

/// ファイル全体のレイアウト署名。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PlanSignature {
    pub activities: Vec<ActivitySignature>,
}

// ===========================================================================
// 境界の判定
// ===========================================================================

/// 連続性の判定に使う設定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContinuityOptions {
    /// サンプル間で許す最大の空白 (1/100 秒)。既定は 2 時間。
    ///
    /// 採取が長く止まっていた場合、その間の増分を 1 区間のレートに均すと
    /// 実態とかけ離れるため、不連続として扱う。
    pub max_gap_cs: u64,
    /// 起動時刻の推定値の許容差 (1/100 秒)。
    ///
    /// `ust_time − uptime/100` は秒精度の丸めでわずかにずれるため、
    /// 完全一致は要求しない。
    pub boot_tolerance_cs: u64,
    /// ファイル境界で前ファイルの最終サンプルを基準値として引き継ぐか。
    ///
    /// `false` にすると各ファイルの先頭サンプルは常に不連続になる
    /// (本家 `sar -f` を 1 ファイルずつ実行した場合と同じ見え方)。
    pub carry_across_files: bool,
}

impl Default for ContinuityOptions {
    fn default() -> Self {
        Self {
            max_gap_cs: 2 * 60 * 60 * 100,
            boot_tolerance_cs: 5 * 100,
            carry_across_files: true,
        }
    }
}

/// 連続性を切った理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BreakReason {
    /// 前サンプルが無い (系列の先頭)。
    NoPreviousSample,
    /// RESTART レコードを挟んだ。
    RestartRecord,
    /// 起動時刻の推定値が変わった (再起動)。
    BootEpochChanged,
    /// 起動時刻を推定できない (`uptime` が取れない)。
    ///
    /// 「たぶん同じ起動区間だろう」と推測して差分を作らない。
    BootEpochUnknown,
    /// 時刻が逆行した。
    TimeWentBackwards,
    /// 空白が許容範囲を超えた。
    GapTooLarge,
    /// activity のレイアウトが違う (値の意味を引き継げない)。
    LayoutChanged,
    /// ホストの識別材料が変わった。
    IdentityChanged,
    /// 設定でファイル境界の引き継ぎを無効にしている。
    CarryDisabled,
}

impl BreakReason {
    /// 新しい起動区間を始めるべき理由か。
    pub const fn starts_new_segment(self) -> bool {
        matches!(
            self,
            BreakReason::RestartRecord
                | BreakReason::BootEpochChanged
                | BreakReason::IdentityChanged
        )
    }
}

/// 境界の片側 (1 サンプル) の情報。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct BoundaryPoint {
    pub ust_time: u64,
    /// 稼働時間 (1/100 秒)。0 は「取得できていない」を意味する。
    pub uptime_cs: u64,
}

impl BoundaryPoint {
    pub fn of(snap: &Snapshot) -> Self {
        Self {
            ust_time: snap.ust_time,
            uptime_cs: snap.uptime_cs,
        }
    }

    /// 起動時刻の推定値 (エポック秒)。`uptime` が無ければ `None`。
    pub fn boot_epoch(&self) -> Option<i64> {
        if self.uptime_cs == 0 {
            return None;
        }
        Some(self.ust_time as i64 - (self.uptime_cs / 100) as i64)
    }
}

/// 境界の判定結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct BoundaryDecision {
    /// 差分を引き継いでよいか。
    pub continuous: bool,
    /// 連続でない場合の理由。
    pub reason: Option<BreakReason>,
    /// 新しい起動区間を始めるか。
    pub new_segment: bool,
    /// 前サンプルからの空白 (秒)。
    pub gap_secs: i64,
    pub boot_epoch_prev: Option<i64>,
    pub boot_epoch_next: Option<i64>,
}

impl BoundaryDecision {
    fn broken(reason: BreakReason, gap_secs: i64, prev: Option<i64>, next: Option<i64>) -> Self {
        Self {
            continuous: false,
            reason: Some(reason),
            new_segment: reason.starts_new_segment(),
            gap_secs,
            boot_epoch_prev: prev,
            boot_epoch_next: next,
        }
    }
}

/// サンプル境界の連続性を判定する。
///
/// **推測で差分を作らない。** 起動区間の同一性が確認できない場合は
/// 不連続として返し、理由を [`BreakReason`] で示す。
pub fn decide_boundary(
    prev: BoundaryPoint,
    next: BoundaryPoint,
    layout_matches: bool,
    identity: IdentityVerdict,
    cross_file: bool,
    opts: &ContinuityOptions,
) -> BoundaryDecision {
    let bp = prev.boot_epoch();
    let bn = next.boot_epoch();
    let gap_secs = next.ust_time as i64 - prev.ust_time as i64;

    if identity != IdentityVerdict::Identical {
        return BoundaryDecision::broken(BreakReason::IdentityChanged, gap_secs, bp, bn);
    }
    if !layout_matches {
        return BoundaryDecision::broken(BreakReason::LayoutChanged, gap_secs, bp, bn);
    }
    let (Some(b_prev), Some(b_next)) = (bp, bn) else {
        return BoundaryDecision::broken(BreakReason::BootEpochUnknown, gap_secs, bp, bn);
    };
    let tolerance_secs = (opts.boot_tolerance_cs / 100) as i64;
    if (b_prev - b_next).abs() > tolerance_secs {
        return BoundaryDecision::broken(BreakReason::BootEpochChanged, gap_secs, bp, bn);
    }
    if next.uptime_cs < prev.uptime_cs || gap_secs < 0 {
        return BoundaryDecision::broken(BreakReason::TimeWentBackwards, gap_secs, bp, bn);
    }
    if next.uptime_cs - prev.uptime_cs > opts.max_gap_cs {
        return BoundaryDecision::broken(BreakReason::GapTooLarge, gap_secs, bp, bn);
    }
    if cross_file && !opts.carry_across_files {
        return BoundaryDecision::broken(BreakReason::CarryDisabled, gap_secs, bp, bn);
    }

    BoundaryDecision {
        continuous: true,
        reason: None,
        new_segment: false,
        gap_secs,
        boot_epoch_prev: bp,
        boot_epoch_next: bn,
    }
}

// ===========================================================================
// 入力の説明
// ===========================================================================

/// ヘッダだけを読んで得たファイルの概要。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileOutline {
    /// 入力全体での添字 (出力での参照に使う)。
    pub index: usize,
    pub path: String,
    pub identity: HostIdentity,
    pub format_magic: u16,
    pub sysstat_version: String,
    pub year: i32,
    pub month: u8,
    pub day: u8,
    pub tzname: Option<String>,
    /// ヘッダに記録された作成時刻 (エポック秒)。
    pub header_ust_time: u64,
    /// ファイルに含まれる activity 数。
    pub activities: usize,
}

impl FileOutline {
    /// 決定的な並び順の鍵 (日付 → 作成時刻 → パス)。
    fn order_key(&self) -> (i32, u8, u8, u64, &str) {
        (
            self.year,
            self.month,
            self.day,
            self.header_ust_time,
            self.path.as_str(),
        )
    }
}

/// 読めなかったファイル。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SkippedFile {
    pub path: String,
    pub reason: String,
}

/// ファイル読み取り失敗時の方針。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FileErrorPolicy {
    /// エラーで止める (既定)。
    #[default]
    Fail,
    /// そのファイルを飛ばして続ける。飛ばしたことは出力に残す。
    Skip,
}

/// 複数ファイル横断の設定。
#[derive(Debug, Clone)]
pub struct MultiOptions {
    pub open: OpenOptions,
    pub selection: Selection,
    pub summary: SummaryOptions,
    pub continuity: ContinuityOptions,
    /// 集計期間を絞る時刻フィルタ (`summarize` / `compare` の `--from` / `--to`)。
    ///
    /// 既定は両端未指定 (絞らない) で、出力は 1 バイトも変わらない。
    ///
    /// # `detect` の `--from` / `--to` とは意味が違う
    ///
    /// `detect` の報告範囲 ([`crate::detect::ReportWindow`]) は**報告する
    /// エピソードを絞るだけで、比較基準の材料は絞らない** (`docs/design.md` §11.2)。
    /// こちらは**集計期間そのもの**で、範囲外のサンプルは平均・最大 / 最小・p95・
    /// 差分合計・区間長合計のどれにも入らない。`detect` はこの欄を使わない
    /// (既定の「絞らない」まま渡し、報告範囲は検出層が持つ)。
    ///
    /// # 何を「範囲内」と見るか
    ///
    /// 範囲の判定は出力層と同じ [`TimeFilter`] に任せる (`sar -s` / `-e` の
    /// 意味論を二重実装しないため。`docs/format/03-output-format.md`
    /// 第 IV 部 §1.8〜§1.11)。
    ///
    /// | [`Admit`] | 集計での扱い |
    /// |---|---|
    /// | `Skip` | 範囲に入る前。集計にも基準にも使わない |
    /// | `Reference` | 範囲に最初に合致したレコード。原則**差分の起点としてだけ使う** (下記) |
    /// | `Emit` | 集計に入れる |
    /// | `Stop` | `--to` を超えた。**このレコードは入れず**、そのファイルの走査を打ち切る |
    ///
    /// # 基準レコードだけ `show` と扱いが違う
    ///
    /// `Reference` を値に数えないのは、その区間 (前サンプル → 当レコード) が
    /// 範囲の外へはみ出しているからである。**範囲の手前で 1 件も捨てていなければ
    /// はみ出す区間が無いので、通常どおり数える** (系列の先頭サンプルと同じ扱い)。
    ///
    /// ここだけは `show` と挙動が違う (`show` は基準レコードを必ず表示から落とす)。
    /// 集計で同じにすると、**何も絞られていないのに先頭サンプルが平均・p95 から
    /// 落ちる**ため、「範囲を全体に取った結果」と「絞らない結果」が食い違う。
    /// 期間集計では「絞ったぶんだけ減る」ことを保証する方を採った。
    ///
    /// # フィルタはファイルごとに引き直す
    ///
    /// `sa01`..`sa31` に `--from 09:00 --to 18:00` を与えたときは
    /// **各日の 09:00〜18:00** を集計する (本家が 1 ファイルずつ走査するのと同じで、
    /// `show` に複数ファイルを渡した場合とも揃う)。系列全体で 1 度しか
    /// 引かないと、初日の 18:00 で `Stop` して以降の全日が落ちる。
    ///
    /// 日をまたいだ区間が紛れ込む心配は要らない。範囲の外を挟んだサンプル対は
    /// [`ContinuityOptions::max_gap_cs`] (既定 2 時間) を超えるので、
    /// そもそも差分を作らない。
    pub time_filter: TimeFilter,
    pub on_error: FileErrorPolicy,
    /// 同時にデコードするファイル数の上限。
    ///
    /// 1 ファイル分のサンプルはすべてメモリに載るため、
    /// 上限を設けずに全ファイルを並列デコードするとメモリが読めなくなる。
    pub max_concurrent_files: usize,
}

impl Default for MultiOptions {
    fn default() -> Self {
        Self {
            open: OpenOptions::default(),
            selection: Selection::All,
            summary: SummaryOptions::default(),
            continuity: ContinuityOptions::default(),
            time_filter: TimeFilter::default(),
            on_error: FileErrorPolicy::default(),
            max_concurrent_files: 4,
        }
    }
}

// ===========================================================================
// 出力
// ===========================================================================

/// ファイル境界の記録。引き継いだかどうかを必ず残す。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BoundaryRecord {
    pub prev_file: usize,
    pub next_file: usize,
    pub prev_sample_ust: u64,
    pub next_sample_ust: u64,
    pub decision: BoundaryDecision,
}

/// 起動区間 1 つ分の集計と判定。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BootSegment {
    pub index: usize,
    /// この区間に寄与したファイル ([`FileOutline::index`])。
    pub files: Vec<usize>,
    /// 起動時刻の推定値 (エポック秒)。
    pub boot_epoch: Option<i64>,
    pub summary: NativePeriodSummary,
    pub findings: Vec<Finding>,
    /// ファイル境界の判定記録。
    pub boundaries: Vec<BoundaryRecord>,
}

/// 1 ホスト分の系列。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HostSeries {
    pub identity: HostIdentity,
    /// 同一性の判定記録 (ファイルごとに先頭ファイルと比較したもの)。
    pub identity_checks: Vec<IdentityComparison>,
    pub files: Vec<usize>,
    pub segments: Vec<BootSegment>,
}

/// 複数ファイル横断の結果。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MultiFileAnalysis {
    pub schema_version: &'static str,
    /// 入力ファイルの概要 (指定順)。
    pub files: Vec<FileOutline>,
    pub skipped: Vec<SkippedFile>,
    /// ホストごとの系列 (`nodename` 昇順)。
    pub hosts: Vec<HostSeries>,
}

// ===========================================================================
// デコード
// ===========================================================================

/// 1 サンプル分の記録。
#[derive(Debug, Clone)]
struct SampleRecord {
    snapshot: Snapshot,
    /// このサンプルの直前に RESTART レコードがあったか。
    restart_before: bool,
}

/// 1 ファイル分のデコード結果。
///
/// **差分は含まない。** 差分はマージ側 (ホスト・起動区間ごとの処理器) が計算する。
struct FileSamples {
    index: usize,
    /// このファイルのヘッダが申告する識別材料。
    ///
    /// グループ代表の識別材料ではなく**ファイルごとの値**を持つ。
    /// ファイル境界で `release` / `cpu_nr` の変化を見るために必要
    /// ([`HostIdentity::continuity_key`])。
    ///
    /// `Arc` で持つのは、マージ側が**サンプルごとに**直前の識別材料を
    /// 抱え直すため (`PrevSample`)。ファイル内では不変な 4 本の `String` を
    /// サンプル数だけ確保し直すのは無駄なので、参照だけを配る。
    identity: Arc<HostIdentity>,
    /// レイアウト署名。`identity` と同じ理由で `Arc` にする
    /// (activity 数ぶんの `Vec` をサンプルごとに複製しない)。
    signature: Arc<PlanSignature>,
    plans: Vec<ActivityPlan>,
    samples: Vec<SampleRecord>,
}

/// `Selection` の判定 (`series` 側の実装は非公開なので同じ規則をここに置く)。
fn selected(selection: &Selection, id: ActivityId) -> bool {
    match selection {
        Selection::All => true,
        Selection::Only(list) => list.contains(&id),
    }
}

/// ファイルのデコード計画を組み立てる。
///
/// `series::walk` と同じ規則で revision を選ぶ。マージ時に前ファイルの値を
/// 解釈するために計画そのものが必要なので、ここで作って保持する。
fn build_plans(file: &SaFile, selection: &Selection) -> Result<Vec<ActivityPlan>> {
    let mut plans = Vec::new();
    for (index, act) in file.activities().iter().enumerate() {
        if !selected(selection, act.id) {
            continue;
        }
        let Some(def) = crate::layout::registry::lookup(act.id) else {
            continue;
        };
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
    Ok(plans)
}

fn signature_of(file: &SaFile, plans: &[ActivityPlan]) -> PlanSignature {
    let acts = file.activities();
    PlanSignature {
        activities: plans
            .iter()
            .map(|p| {
                let e = &acts[p.index];
                ActivitySignature {
                    id: p.id,
                    magic: e.magic,
                    types_nr: e.types_nr,
                    item_size: e.size,
                    fields: p.plan.fields.len(),
                }
            })
            .collect(),
    }
}

/// 1 ファイルをデコードしてサンプル列にする。
fn decode_file(outline: &FileOutline, opts: &MultiOptions) -> Result<FileSamples> {
    let file = SaFile::open_with(&outline.path, opts.open.clone())?;
    let plans = build_plans(&file, &opts.selection)?;
    let signature = signature_of(&file, &plans);

    let mut samples: Vec<SampleRecord> = Vec::new();
    // 直前に RESTART を読んだか。次の統計レコードへ持ち越す
    // (走査はイベントを束ねずに読んだ順で渡すため、ここで覚えておく)。
    let mut restart_pending = false;
    walk_items(&file, &opts.selection, |item| {
        match item {
            WalkItem::Event(ev) => {
                if matches!(ev, RecordEvent::Restart { .. }) {
                    restart_pending = true;
                }
            }
            WalkItem::Sample(view) => samples.push(SampleRecord {
                snapshot: view.curr.clone(),
                restart_before: std::mem::take(&mut restart_pending),
            }),
        }
        Ok(ScanControl::Continue)
    })?;

    Ok(FileSamples {
        index: outline.index,
        identity: Arc::new(outline.identity.clone()),
        signature: Arc::new(signature),
        plans,
        samples,
    })
}

// ===========================================================================
// マージ
// ===========================================================================

/// マージ中の起動区間。
struct OpenSegment {
    index: usize,
    boot_epoch: Option<i64>,
    /// この区間を開いたファイルの識別材料。
    ///
    /// 区間は識別材料が変わった時点で切れる (`BreakReason::IdentityChanged`) ため、
    /// 区間内では `release` / `cpu_nr` が一定である。グループ代表 (先頭ファイル)
    /// の値を出力に載せると、カーネル更新後の区間に更新前の `release` が付く。
    identity: Arc<HostIdentity>,
    builder: NativeSummaryBuilder,
    files: Vec<usize>,
    boundaries: Vec<BoundaryRecord>,
    observed: u64,
}

/// 直前のサンプル (ファイルをまたいで保持する)。
struct PrevSample {
    snapshot: Snapshot,
    file_index: usize,
    /// そのサンプルが入っていたファイルの識別材料 (参照のみ。複製しない)。
    identity: Arc<HostIdentity>,
    signature: Arc<PlanSignature>,
}

/// 1 ホスト分の系列を組み立てる。
///
/// ファイルを決定的順序で受け取り、**ファイル境界をまたいで差分を作る**。
///
/// 識別材料はグループ代表の 1 つを持たず、[`FileSamples::identity`] として
/// ファイルごとに受け取る。境界での差分引き継ぎの可否も、出力に載せる
/// `release` / `cpu_nr` も、**代表ではなくその場のファイルの申告値**で決まる。
struct GroupMerger<'o> {
    opts: &'o MultiOptions,
    /// 入力ファイルの概要。出力にパスを載せるために参照する。
    outlines: &'o [FileOutline],
    /// タイムゾーン (このホストの最初のファイルの申告値)。
    tzname: Option<String>,
    segments: Vec<BootSegment>,
    current: Option<OpenSegment>,
    prev: Option<PrevSample>,
    files: Vec<usize>,
    next_segment_index: usize,
}

impl<'o> GroupMerger<'o> {
    fn new(opts: &'o MultiOptions, outlines: &'o [FileOutline]) -> Self {
        Self {
            opts,
            outlines,
            tzname: None,
            segments: Vec::new(),
            current: None,
            prev: None,
            files: Vec::new(),
            next_segment_index: 0,
        }
    }

    /// ファイル添字を出力用のラベル (パス) に直す。
    fn file_label(&self, index: usize) -> String {
        self.outlines
            .iter()
            .find(|o| o.index == index)
            .map(|o| o.path.clone())
            .unwrap_or_else(|| format!("#{index}"))
    }

    fn open_segment(
        &mut self,
        boot_epoch: Option<i64>,
        identity: Arc<HostIdentity>,
    ) -> &mut OpenSegment {
        let index = self.next_segment_index;
        self.next_segment_index += 1;
        self.current = Some(OpenSegment {
            index,
            boot_epoch,
            identity,
            builder: NativeSummaryBuilder::new(self.opts.summary.clone()),
            files: Vec::new(),
            boundaries: Vec::new(),
            observed: 0,
        });
        self.current.as_mut().expect("開いた区間")
    }

    fn close_segment(&mut self) {
        let Some(seg) = self.current.take() else {
            return;
        };
        if seg.observed == 0 {
            return;
        }
        // `release` / `cpu_nr` は**その区間の**申告値を載せる。
        // 区間は識別材料の変化で切れるので区間内では一定であり、
        // グループ代表の値を使うとカーネル更新後の区間に更新前の版が付く。
        let source = SummarySource {
            label: seg.identity.nodename.clone(),
            nodename: Some(seg.identity.nodename.clone()),
            release: Some(seg.identity.release.clone()),
            machine: Some(seg.identity.machine.clone()),
            cpu_nr: seg.identity.cpu_nr,
            tzname: self.tzname.clone(),
            files: seg.files.iter().map(|i| self.file_label(*i)).collect(),
            boot_segment: Some(seg.index),
        };
        let summary = seg.builder.finish(source);
        // 判定 (`run_queue` など) の分母も区間の CPU 数を使う
        let findings = evaluate(
            &summary,
            &RuleContext {
                cpu_nr: seg.identity.cpu_nr,
            },
        );
        self.segments.push(BootSegment {
            index: seg.index,
            files: seg.files,
            boot_epoch: seg.boot_epoch,
            summary,
            findings,
            boundaries: seg.boundaries,
        });
    }

    /// ファイル 1 つ分のサンプルをマージする。
    fn push_file(&mut self, fs: FileSamples) {
        self.files.push(fs.index);
        if self.tzname.is_none() {
            self.tzname = self
                .outlines
                .iter()
                .find(|o| o.index == fs.index)
                .and_then(|o| o.tzname.clone());
        }
        let empty = Snapshot::default();
        // 時刻フィルタはファイルごとに引き直す ([`MultiOptions::time_filter`])。
        let mut cursor = self.opts.time_filter.cursor();
        // 範囲に入る前にサンプルを捨てたか。`Admit::Reference` を値に数えるかの
        // 判定に使う (捨てていなければ、その区間は範囲の外へはみ出していない)。
        let mut dropped_before_window = false;

        for sample in &fs.samples {
            if !sample.snapshot.valid {
                continue;
            }
            let point = BoundaryPoint::of(&sample.snapshot);

            // --- 前サンプルとの関係を決める ---
            let (decision, cross_file) = match &self.prev {
                None => (
                    Some(BoundaryDecision::broken(
                        BreakReason::NoPreviousSample,
                        0,
                        None,
                        point.boot_epoch(),
                    )),
                    false,
                ),
                Some(prev) => {
                    let cross_file = prev.file_index != fs.index;
                    if sample.restart_before {
                        // RESTART は明示的な再起動。ファイル内でも区間を切る。
                        (
                            Some(BoundaryDecision::broken(
                                BreakReason::RestartRecord,
                                point.ust_time as i64 - prev.snapshot.ust_time as i64,
                                BoundaryPoint::of(&prev.snapshot).boot_epoch(),
                                point.boot_epoch(),
                            )),
                            cross_file,
                        )
                    } else {
                        let d = decide_boundary(
                            BoundaryPoint::of(&prev.snapshot),
                            point,
                            prev.signature == fs.signature,
                            // **同一性検査の結果をそのまま渡す。**
                            // ここを固定値 `Identical` にすると、
                            // `LikelySameMachine` (カーネル更新 / CPU 数変化) と
                            // 診断したファイル間でも、時刻とレイアウトが揃えば
                            // 差分を引き継いでしまい、出力される診断と
                            // 実際の集計動作が食い違う。
                            identity_verdict(&prev.identity, &fs.identity),
                            cross_file,
                            &self.opts.continuity,
                        );
                        (if d.continuous { None } else { Some(d) }, cross_file)
                    }
                }
            };

            // --- 起動区間の切り替え ---
            //
            // **時刻フィルタより先に行う。** 範囲の外で起きた再起動も区間の
            // 分割として効かせないと、再起動後の範囲内サンプルが再起動前の
            // 起動区間に入ってしまう (区間の `boot_epoch` と中身が食い違う)。
            // 副作用として、区間の索引は絞らないときと同じ値のままになる
            // (範囲外の区間は `observed == 0` で落ちるので索引に欠番が出る)。
            let start_new = match &decision {
                Some(d) => d.new_segment || self.current.is_none(),
                None => self.current.is_none(),
            };
            if start_new {
                self.close_segment();
                self.open_segment(point.boot_epoch(), Arc::clone(&fs.identity));
            }

            // --- 集計 ---
            // 連続と判定できた場合だけ前サンプルを基準値として渡す。
            // 不明な場合は前サンプルを渡さないので、差分は作られない。
            let carry = decision.is_none();
            let prev_snapshot = match (&self.prev, carry) {
                (Some(p), true) => Some(&p.snapshot),
                _ => None,
            };
            let itv_cs = match prev_snapshot {
                Some(p) => interval_cs(p.uptime_cs, sample.snapshot.uptime_cs),
                None if sample.snapshot.uptime_cs > 0 => sample.snapshot.uptime_cs,
                None => 1,
            };
            let view = IntervalView {
                prev: prev_snapshot.unwrap_or(&empty),
                curr: &sample.snapshot,
                itv_cs,
                has_prev: prev_snapshot.is_some(),
                continuous: prev_snapshot.is_some(),
                events: &[],
                plans: &fs.plans,
            };

            // --- 時刻フィルタ ---
            // 範囲外の区間は平均・極値・p95・差分合計のどれにも入れない
            // (集計器へ渡さないので、重みも期間の端点も自動的に範囲内だけになる)。
            let counted = match cursor.sample(&view) {
                Admit::Emit => true,
                // 範囲に最初に合致したレコード。手前で捨てたサンプルがあるなら
                // その区間は範囲の外へはみ出しているので**値には数えず**、
                // 次の区間の起点 (差分の基準) としてだけ使う。
                // 1 件も捨てていなければはみ出す区間が無いので、系列の先頭
                // サンプルと同じに数える (絞らないときと結果を揃えるため)。
                Admit::Reference => !dropped_before_window,
                Admit::Skip => {
                    dropped_before_window = true;
                    false
                }
                // `--to` を超えた。このレコードは入れず、このファイルは打ち切る。
                Admit::Stop => break,
            };

            if counted {
                // --- ファイル境界の記録 ---
                // 区間を切り替えた場合、記録は**新しく開いた区間**に残す
                // (「この区間はどこから始まったか」を追えるようにする)。
                // 集計に入った区間だけ記録する。入っていない境界を
                // 「差分を引き継いだ」と報告すると、報告と期間が食い違う。
                if cross_file && let (Some(prev), Some(seg)) = (&self.prev, self.current.as_mut()) {
                    let d = decision.unwrap_or(BoundaryDecision {
                        continuous: true,
                        reason: None,
                        new_segment: false,
                        gap_secs: point.ust_time as i64 - prev.snapshot.ust_time as i64,
                        boot_epoch_prev: BoundaryPoint::of(&prev.snapshot).boot_epoch(),
                        boot_epoch_next: point.boot_epoch(),
                    });
                    seg.boundaries.push(BoundaryRecord {
                        prev_file: prev.file_index,
                        next_file: fs.index,
                        prev_sample_ust: prev.snapshot.ust_time,
                        next_sample_ust: point.ust_time,
                        decision: d,
                    });
                }

                if let Some(seg) = self.current.as_mut() {
                    seg.builder.observe(&view);
                    seg.observed += 1;
                    if !seg.files.contains(&fs.index) {
                        seg.files.push(fs.index);
                    }
                }
            }

            self.prev = Some(PrevSample {
                snapshot: sample.snapshot.clone(),
                file_index: fs.index,
                // Arc の参照だけを増やす (ファイル内で不変なものを複製しない)
                identity: Arc::clone(&fs.identity),
                signature: Arc::clone(&fs.signature),
            });
        }
    }

    fn finish(mut self) -> (Vec<usize>, Vec<BootSegment>) {
        self.close_segment();
        (self.files, self.segments)
    }
}

// ===========================================================================
// 入口
// ===========================================================================

/// 複数ファイルを横断して集計・判定する。
///
/// 手順:
/// 1. ヘッダだけを読んで概要を得る (逐次。安価)
/// 2. ホストごとにまとめ、日付順に並べる (決定的)
/// 3. 同時処理数の上限ごとに区切って rayon でデコードし、順序どおりにマージする
pub fn analyze_files(paths: &[PathBuf], opts: &MultiOptions) -> Result<MultiFileAnalysis> {
    let (outlines, mut skipped) = outline_files(paths, opts)?;

    // --- ホストごとにまとめる ---
    let mut groups: Vec<(HostIdentity, Vec<usize>)> = Vec::new();
    for o in &outlines {
        let key = o.identity.group_key();
        match groups.iter_mut().find(|(id, _)| id.group_key() == key) {
            Some((_, files)) => files.push(o.index),
            None => groups.push((o.identity.clone(), vec![o.index])),
        }
    }
    // ホストの出力順を決定的にする
    groups.sort_by_key(|g| g.0.group_key());

    let mut hosts = Vec::with_capacity(groups.len());
    for (identity, mut indices) in groups {
        // 日付 → 作成時刻 → パスの順。ファイル名の辞書順に依存しない。
        indices.sort_by(|a, b| outlines[*a].order_key().cmp(&outlines[*b].order_key()));

        let identity_checks: Vec<IdentityComparison> = indices
            .iter()
            .map(|i| compare_identity(&identity, &outlines[*i].identity))
            .collect();

        let mut merger = GroupMerger::new(opts, &outlines);
        let limit = opts.max_concurrent_files.max(1);
        // **全結果を collect しない。** 同時処理数の上限ごとに区切って
        // デコードし、その都度マージへ流して解放する。
        for window in indices.chunks(limit) {
            let decoded: Vec<Result<FileSamples>> = window
                .par_iter()
                .map(|i| decode_file(&outlines[*i], opts))
                .collect();
            for (slot, result) in window.iter().zip(decoded) {
                match result {
                    Ok(fs) => merger.push_file(fs),
                    Err(e) => match opts.on_error {
                        FileErrorPolicy::Fail => return Err(e),
                        // 飛ばしたファイルは出力に残す (黙って落とさない)
                        FileErrorPolicy::Skip => skipped.push(SkippedFile {
                            path: outlines[*slot].path.clone(),
                            reason: e.to_string(),
                        }),
                    },
                }
            }
        }
        let (files, segments) = merger.finish();
        hosts.push(HostSeries {
            identity,
            identity_checks,
            files,
            segments,
        });
    }

    Ok(MultiFileAnalysis {
        schema_version: MULTI_SCHEMA_VERSION,
        files: outlines,
        skipped,
        hosts,
    })
}

/// ヘッダだけを読んで概要を作る。
pub fn outline_files(
    paths: &[PathBuf],
    opts: &MultiOptions,
) -> Result<(Vec<FileOutline>, Vec<SkippedFile>)> {
    let mut outlines = Vec::with_capacity(paths.len());
    let mut skipped = Vec::new();
    for path in paths {
        match outline_one(path, outlines.len(), opts) {
            Ok(o) => outlines.push(o),
            Err(e) => match opts.on_error {
                FileErrorPolicy::Fail => return Err(e),
                FileErrorPolicy::Skip => skipped.push(SkippedFile {
                    path: path.display().to_string(),
                    reason: e.to_string(),
                }),
            },
        }
    }
    Ok((outlines, skipped))
}

fn outline_one(path: &Path, index: usize, opts: &MultiOptions) -> Result<FileOutline> {
    let file = SaFile::open_with(path, opts.open.clone())?;
    let h = file.header();
    Ok(FileOutline {
        index,
        path: path.display().to_string(),
        identity: HostIdentity::from_header(h),
        format_magic: file.magic().format_magic,
        sysstat_version: file.magic().version_string(),
        year: h.year,
        month: h.month,
        day: h.day,
        tzname: h.tzname.clone(),
        header_ust_time: h.ust_time,
        activities: file.activities().len(),
    })
}

// ===========================================================================
// ホスト間比較
// ===========================================================================

/// 比較に使う共通時間窓。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ComparisonWindow {
    pub start_ust: u64,
    pub end_ust: u64,
    /// 1 区間の長さ (秒)。
    pub step_secs: u64,
}

impl ComparisonWindow {
    pub fn new(start_ust: u64, end_ust: u64, step_secs: u64) -> Self {
        Self {
            start_ust,
            end_ust,
            step_secs: step_secs.max(1),
        }
    }

    /// 区間の数。
    pub fn bucket_count(&self) -> usize {
        if self.end_ust <= self.start_ust {
            return 0;
        }
        let span = self.end_ust - self.start_ust;
        span.div_ceil(self.step_secs) as usize
    }
}

/// 観測が無い区間の埋め方。**既定は埋めない。**
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GaugeFill {
    /// 埋めない。観測が無い区間は値なしのまま比較対象から外す。
    #[default]
    None,
    /// 直前の観測値を持ち越す (ゲージのみ)。`max_age_secs` を超えたら埋めない。
    HoldLast { max_age_secs: u64 },
    /// 前後の観測値で線形補間する (ゲージのみ)。`max_gap_secs` を超えたら埋めない。
    Linear { max_gap_secs: u64 },
}

/// 区間値の出どころ。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BucketSource {
    /// 実際の観測から算出した。
    Observed,
    /// 直前の観測値を持ち越した (明示的に選択された場合のみ)。
    HeldFromPrevious { age_secs: u64 },
    /// 前後の観測から補間した (明示的に選択された場合のみ)。
    Interpolated,
    /// 観測が無い。**0 ではない。**
    NoObservation,
}

/// 共通時間窓の 1 区間。
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Bucket {
    pub start_ust: u64,
    pub end_ust: u64,
    /// 区間値。観測が無ければ `None` (0 で埋めない)。
    pub value: Option<f64>,
    pub source: BucketSource,
    /// この区間に重なった観測の長さ (秒)。
    pub covered_secs: u64,
}

/// ホスト 1 台分の整列済み系列。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HostAlignedSeries {
    pub label: String,
    pub identity: HostIdentity,
    pub buckets: Vec<Bucket>,
}

impl HostAlignedSeries {
    /// 値が入っている区間数。
    pub fn observed_buckets(&self) -> usize {
        self.buckets.iter().filter(|b| b.value.is_some()).count()
    }
}

/// ホスト間比較の結果。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HostComparison {
    pub metric: MetricKey,
    pub window: ComparisonWindow,
    pub fill: GaugeFill,
    pub hosts: Vec<HostAlignedSeries>,
    /// 全ホストで値が揃っている区間数。ここだけが直接比較できる。
    pub comparable_buckets: usize,
}

/// 各ホストの観測範囲から共通時間窓を求める。
///
/// 交差が無ければ `None`。「片方に観測が無い区間を 0 とみなして比較する」ことを
/// 避けるため、共通範囲を明示的に求めてから比較する。
///
/// # `--from` / `--to` との合成は「交差」
///
/// 時刻フィルタ ([`MultiOptions::time_filter`]) は**各ホストの集計期間**を絞る。
/// ここへ渡す `ranges` はその絞られた期間 ([`NativePeriodSummary::period`] の
/// `first_ust` / `last_ust`) なので、共通窓は自動的に
/// 「各ホストの観測範囲の交差 ∩ 指定範囲」になる。
///
/// 指定範囲をそのまま窓の端点に使う形は採らない。`--from 09:00` のような
/// 時分秒指定は「毎日の 09:00」であって 1 つの絶対時刻に解けないうえ、
/// **観測が無い時間帯まで窓に含めると、比較可能な区間数の分母が水増しされる**
/// (片方に観測が無い区間を 0 と見なさない方針と衝突する)。
pub fn common_window(ranges: &[(u64, u64)], step_secs: u64) -> Option<ComparisonWindow> {
    if ranges.is_empty() {
        return None;
    }
    let start = ranges.iter().map(|r| r.0).max()?;
    let end = ranges.iter().map(|r| r.1).min()?;
    if end <= start {
        return None;
    }
    Some(ComparisonWindow::new(start, end, step_secs))
}

/// 時系列を共通時間窓の区間へ集約する。
///
/// - 区間に重なる観測の**時間加重平均**を値とする
/// - 重なる観測が無い区間は [`BucketSource::NoObservation`] (0 で埋めない)
/// - カウンタ由来のレートは持ち越し・補間をしない
///   (増分が無かったのか観測が無かったのか区別できないため)
pub fn align_to_window(
    timeline: &MetricTimeline,
    window: ComparisonWindow,
    fill: GaugeFill,
) -> Vec<Bucket> {
    let gauge = matches!(timeline.kind, ValueKind::Gauge);
    let mut buckets = Vec::with_capacity(window.bucket_count());

    for i in 0..window.bucket_count() {
        let b0 = window.start_ust + i as u64 * window.step_secs;
        let b1 = (b0 + window.step_secs).min(window.end_ust);
        let mut weighted = 0.0f64;
        let mut weight = 0u64;

        for p in &timeline.points {
            let Some(v) = p.value else { continue };
            let lo = p.start_ust.max(b0);
            let hi = p.end_ust.min(b1);
            if hi <= lo {
                continue;
            }
            let w = hi - lo;
            weighted += v * w as f64;
            weight += w;
        }

        let bucket = if weight > 0 {
            Bucket {
                start_ust: b0,
                end_ust: b1,
                value: Some(weighted / weight as f64),
                source: BucketSource::Observed,
                covered_secs: weight,
            }
        } else {
            // 観測が無い。埋めるかどうかは呼び出し側が明示的に選ぶ。
            let filled = if gauge {
                fill_bucket(timeline, b0, b1, fill)
            } else {
                None
            };
            match filled {
                Some((v, source)) => Bucket {
                    start_ust: b0,
                    end_ust: b1,
                    value: Some(v),
                    source,
                    covered_secs: 0,
                },
                None => Bucket {
                    start_ust: b0,
                    end_ust: b1,
                    value: None,
                    source: BucketSource::NoObservation,
                    covered_secs: 0,
                },
            }
        };
        buckets.push(bucket);
    }
    buckets
}

/// 観測が無い区間を埋める (ゲージのみ)。
fn fill_bucket(
    timeline: &MetricTimeline,
    b0: u64,
    b1: u64,
    fill: GaugeFill,
) -> Option<(f64, BucketSource)> {
    let before = timeline
        .points
        .iter()
        .filter(|p| p.value.is_some() && p.end_ust <= b0)
        .max_by_key(|p| p.end_ust);
    let after = timeline
        .points
        .iter()
        .filter(|p| p.value.is_some() && p.start_ust >= b1)
        .min_by_key(|p| p.start_ust);

    match fill {
        GaugeFill::None => None,
        GaugeFill::HoldLast { max_age_secs } => {
            let p = before?;
            let age = b0.saturating_sub(p.end_ust);
            if age > max_age_secs {
                return None;
            }
            Some((p.value?, BucketSource::HeldFromPrevious { age_secs: age }))
        }
        GaugeFill::Linear { max_gap_secs } => {
            let (a, b) = (before?, after?);
            let gap = b.start_ust.saturating_sub(a.end_ust);
            if gap > max_gap_secs || gap == 0 {
                return None;
            }
            let (va, vb) = (a.value?, b.value?);
            // 区間の中点で線形補間する
            let mid = b0 + (b1 - b0) / 2;
            let t = (mid.saturating_sub(a.end_ust)) as f64 / gap as f64;
            Some((va + (vb - va) * t, BucketSource::Interpolated))
        }
    }
}

/// 複数ホストの同一指標を共通時間窓で比較する。
pub fn compare_hosts(
    metric: MetricKey,
    series: &[(String, HostIdentity, &MetricTimeline)],
    window: ComparisonWindow,
    fill: GaugeFill,
) -> HostComparison {
    let hosts: Vec<HostAlignedSeries> = series
        .iter()
        .map(|(label, identity, t)| HostAlignedSeries {
            label: label.clone(),
            identity: identity.clone(),
            buckets: align_to_window(t, window, fill),
        })
        .collect();

    let comparable = (0..window.bucket_count())
        .filter(|i| {
            !hosts.is_empty()
                && hosts
                    .iter()
                    .all(|h| h.buckets.get(*i).is_some_and(|b| b.value.is_some()))
        })
        .count();

    HostComparison {
        metric,
        window,
        fill,
        hosts,
        comparable_buckets: comparable,
    }
}

/// 起動区間から指標の時系列を引く (ホスト間比較の入力を作る補助)。
pub fn timeline_of<'a>(segment: &'a BootSegment, key: &MetricKey) -> Option<&'a MetricTimeline> {
    segment.summary.timelines.get(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze::summary::{RetainTimelines, SummaryOptions};
    use crate::analyze::timeline::{MetricPoint, SINGLE_ITEM};
    use crate::format::abi::{Endian, LayoutAbi, SourceEncoding};
    use crate::layout::registry::lookup;
    use crate::model::{Availability, Unit};
    use crate::series::snapshot::{ActivitySnapshot, ItemSnapshot};

    fn ident(nodename: &str) -> HostIdentity {
        HostIdentity {
            nodename: nodename.to_string(),
            sysname: "Linux".to_string(),
            release: "5.15.0-generic".to_string(),
            machine: "x86_64".to_string(),
            cpu_nr: Some(4),
        }
    }

    // -----------------------------------------------------------------------
    // ホストの同一性
    // -----------------------------------------------------------------------

    #[test]
    fn identical_headers_are_identical() {
        let c = compare_identity(&ident("host-a"), &ident("host-a"));
        assert_eq!(c.verdict, IdentityVerdict::Identical);
        assert!(c.allows_delta_carry());
        assert!(c.fields.iter().all(|f| f.agrees));
    }

    #[test]
    fn different_nodename_is_different_host() {
        let c = compare_identity(&ident("host-a"), &ident("host-b"));
        assert_eq!(c.verdict, IdentityVerdict::Different);
        assert!(!c.allows_delta_carry());
    }

    /// **ホスト名だけで同一マシンと断定しない。**
    #[test]
    fn same_nodename_different_machine_is_ambiguous() {
        let mut b = ident("host-a");
        b.machine = "aarch64".to_string();
        let c = compare_identity(&ident("host-a"), &b);
        assert_eq!(
            c.verdict,
            IdentityVerdict::Ambiguous,
            "アーキテクチャが違えば同一マシンとしない"
        );
        assert!(!c.allows_delta_carry());
        assert!(!c.notes.is_empty(), "判定材料を注記として残す");
        // 判定材料が項目ごとに残る
        let m = c.fields.iter().find(|f| f.field == "machine").unwrap();
        assert!(!m.agrees);
        assert_eq!(m.left, "x86_64");
        assert_eq!(m.right, "aarch64");
    }

    /// カーネル更新は同一マシンだが差分は引き継がない。
    #[test]
    fn kernel_upgrade_is_likely_same_machine_but_not_carried() {
        let mut b = ident("host-a");
        b.release = "6.1.0-generic".to_string();
        let c = compare_identity(&ident("host-a"), &b);
        assert_eq!(c.verdict, IdentityVerdict::LikelySameMachine);
        assert!(
            !c.allows_delta_carry(),
            "再起動を挟んでいるので引き継がない"
        );
        assert!(c.notes.iter().any(|n| n.contains("カーネル")));
    }

    /// CPU 数の違いも判定材料に入る。
    #[test]
    fn cpu_count_change_is_recorded() {
        let mut b = ident("host-a");
        b.cpu_nr = Some(8);
        let c = compare_identity(&ident("host-a"), &b);
        assert_eq!(c.verdict, IdentityVerdict::LikelySameMachine);
        assert!(c.notes.iter().any(|n| n.contains("CPU 数")));
    }

    /// まとめる鍵に `release` / `cpu_nr` は含めない (起動区間の分割で扱う)。
    #[test]
    fn group_key_ignores_kernel_and_cpu_count() {
        let mut b = ident("host-a");
        b.release = "6.1.0-generic".to_string();
        b.cpu_nr = Some(16);
        assert_eq!(ident("host-a").group_key(), b.group_key());
        assert_ne!(ident("host-a").group_key(), ident("host-b").group_key());
    }

    // -----------------------------------------------------------------------
    // 境界の判定
    // -----------------------------------------------------------------------

    fn point(ust: u64, uptime_cs: u64) -> BoundaryPoint {
        BoundaryPoint {
            ust_time: ust,
            uptime_cs,
        }
    }

    fn decide(prev: BoundaryPoint, next: BoundaryPoint) -> BoundaryDecision {
        decide_boundary(
            prev,
            next,
            true,
            IdentityVerdict::Identical,
            true,
            &ContinuityOptions::default(),
        )
    }

    /// 同じ起動区間で時刻が進んでいれば引き継ぐ。
    #[test]
    fn same_boot_segment_carries_across_files() {
        // 起動から 100_000 cs (1000 秒) 後 → 起動時刻 = 1000
        let prev = point(2_000, 100_000);
        // 600 秒後 (日境界をまたいだ次ファイルの先頭サンプル)
        let next = point(2_600, 160_000);
        let d = decide(prev, next);
        assert!(d.continuous, "{d:?}");
        assert_eq!(d.reason, None);
        assert!(!d.new_segment);
        assert_eq!(d.boot_epoch_prev, Some(1_000));
        assert_eq!(d.boot_epoch_next, Some(1_000));
        assert_eq!(d.gap_secs, 600);
    }

    /// 再起動を挟んだら新しい起動区間を始め、差分は作らない。
    #[test]
    fn reboot_starts_a_new_segment() {
        let prev = point(2_000, 100_000);
        // uptime が巻き戻っている = 別の起動
        let next = point(2_600, 1_000);
        let d = decide(prev, next);
        assert!(!d.continuous);
        assert_eq!(d.reason, Some(BreakReason::BootEpochChanged));
        assert!(d.new_segment);
    }

    /// **起動時刻が推定できないときは推測で差分を作らない。**
    #[test]
    fn unknown_uptime_breaks_continuity_without_new_segment() {
        let prev = point(2_000, 0);
        let next = point(2_600, 0);
        let d = decide(prev, next);
        assert!(!d.continuous);
        assert_eq!(d.reason, Some(BreakReason::BootEpochUnknown));
        assert!(!d.new_segment, "起動時刻が不明なだけで再起動と決めつけない");
    }

    /// 空白が大きすぎる場合も引き継がない。
    #[test]
    fn long_gap_breaks_continuity() {
        let prev = point(2_000, 100_000);
        // 3 時間後
        let next = point(2_000 + 10_800, 100_000 + 1_080_000);
        let d = decide(prev, next);
        assert!(!d.continuous);
        assert_eq!(d.reason, Some(BreakReason::GapTooLarge));
        assert!(!d.new_segment);
    }

    /// レイアウトが違えば値の意味を引き継げない。
    #[test]
    fn layout_change_breaks_continuity() {
        let d = decide_boundary(
            point(2_000, 100_000),
            point(2_600, 160_000),
            false,
            IdentityVerdict::Identical,
            true,
            &ContinuityOptions::default(),
        );
        assert!(!d.continuous);
        assert_eq!(d.reason, Some(BreakReason::LayoutChanged));
    }

    /// 同一性が確認できないホストの値は繋げない。
    #[test]
    fn ambiguous_identity_breaks_continuity() {
        let d = decide_boundary(
            point(2_000, 100_000),
            point(2_600, 160_000),
            true,
            IdentityVerdict::Ambiguous,
            true,
            &ContinuityOptions::default(),
        );
        assert!(!d.continuous);
        assert_eq!(d.reason, Some(BreakReason::IdentityChanged));
        assert!(d.new_segment);
    }

    /// 引き継ぎを無効にできる (ファイル境界のみ)。
    #[test]
    fn carry_can_be_disabled_for_file_boundaries_only() {
        let opts = ContinuityOptions {
            carry_across_files: false,
            ..Default::default()
        };
        let across = decide_boundary(
            point(2_000, 100_000),
            point(2_600, 160_000),
            true,
            IdentityVerdict::Identical,
            true,
            &opts,
        );
        assert_eq!(across.reason, Some(BreakReason::CarryDisabled));

        let within = decide_boundary(
            point(2_000, 100_000),
            point(2_600, 160_000),
            true,
            IdentityVerdict::Identical,
            false,
            &opts,
        );
        assert!(within.continuous, "同一ファイル内は影響を受けない");
    }

    /// 起動区間は同じなのに時刻が巻き戻っている場合 (時刻補正など)。
    #[test]
    fn time_going_backwards_breaks_continuity() {
        // どちらも起動時刻の推定値は 1000 のまま
        let prev = point(3_000, 200_000);
        let next = point(2_900, 190_000);
        assert_eq!(prev.boot_epoch(), next.boot_epoch());
        let d = decide(prev, next);
        assert!(!d.continuous);
        assert_eq!(d.reason, Some(BreakReason::TimeWentBackwards));
        assert!(!d.new_segment);
    }

    // -----------------------------------------------------------------------
    // ファイル横断のマージ
    // -----------------------------------------------------------------------

    fn plan_of(id: ActivityId) -> DecodePlan {
        let def = lookup(id).expect("既知 activity");
        let rev = def.latest().expect("revision");
        let enc = SourceEncoding::new(Endian::Little, LayoutAbi::LP64);
        DecodePlan::build(def, rev, rev.size_lp64, 1, 1, &enc).expect("計画")
    }

    fn item_of(plan: &DecodePlan, values: &[(&str, u64)]) -> ItemSnapshot {
        let mut v = vec![Availability::Present(0u64); plan.fields.len()];
        for (name, value) in values {
            let i = plan
                .fields
                .iter()
                .position(|f| f.name == *name)
                .unwrap_or_else(|| panic!("フィールド {name} が無い"));
            v[i] = Availability::Present(*value);
        }
        ItemSnapshot {
            key: None,
            texts: Vec::new(),
            values: v,
        }
    }

    fn snap(id: ActivityId, ust: u64, uptime_cs: u64, item: ItemSnapshot) -> Snapshot {
        Snapshot {
            valid: true,
            kind: None,
            ust_time: ust,
            uptime_cs,
            hour: 0,
            minute: 0,
            second: 0,
            activities: vec![ActivitySnapshot {
                id,
                index: 0,
                nr: 1,
                nr2: 1,
                items: vec![item],
            }],
        }
    }

    /// テスト用のファイル 1 つ分を組む。
    ///
    /// 識別材料は `testhost` の既定値。ファイル間で構成が変わる状況を作る
    /// テストは、戻り値の [`FileSamples::identity`] を直接書き換える。
    fn file_samples(
        index: usize,
        id: ActivityId,
        samples: Vec<(u64, u64, u64)>,
        restart_at: &[usize],
        signature_tag: u32,
    ) -> FileSamples {
        let plan = plan_of(id);
        let records = samples
            .into_iter()
            .enumerate()
            .map(|(i, (ust, uptime, counter))| SampleRecord {
                snapshot: snap(id, ust, uptime, item_of(&plan, &[("pswpin", counter)])),
                restart_before: restart_at.contains(&i),
            })
            .collect();
        FileSamples {
            index,
            identity: Arc::new(ident("testhost")),
            signature: Arc::new(PlanSignature {
                activities: vec![ActivitySignature {
                    id,
                    magic: signature_tag,
                    types_nr: None,
                    item_size: 16,
                    fields: plan.fields.len(),
                }],
            }),
            plans: vec![ActivityPlan { index: 0, id, plan }],
            samples: records,
        }
    }

    /// その activity の最新 revision で 1 item のデコード計画を作る
    /// (revision を持たない / 計画が作れない activity は `None`)。
    fn latest_plan(def: &'static crate::layout::registry::ActivityDef) -> Option<DecodePlan> {
        let rev = def.latest()?;
        let enc = SourceEncoding::new(Endian::Little, LayoutAbi::LP64);
        DecodePlan::build(def, rev, rev.size_lp64, 1, 1, &enc).ok()
    }

    /// 指定の計画・署名でファイル 1 つ分を組む (item の中身は呼び出し側が作る)。
    fn samples_of(
        index: usize,
        id: ActivityId,
        plan: DecodePlan,
        items: Vec<(u64, u64, ItemSnapshot)>,
    ) -> FileSamples {
        let fields = plan.fields.len();
        FileSamples {
            index,
            identity: Arc::new(ident("testhost")),
            signature: Arc::new(PlanSignature {
                activities: vec![ActivitySignature {
                    id,
                    magic: 1,
                    types_nr: None,
                    item_size: 16,
                    fields,
                }],
            }),
            plans: vec![ActivityPlan { index: 0, id, plan }],
            samples: items
                .into_iter()
                .map(|(ust, uptime, item)| SampleRecord {
                    snapshot: snap(id, ust, uptime, item),
                    restart_before: false,
                })
                .collect(),
        }
    }

    /// 全フィールドに別々の値を入れたファイル 1 つ分を組む。
    ///
    /// `samples` は `(ust, uptime_cs, base, step)`。列の取り違え (隣の列の差分を
    /// 読んでいる) を検出できるよう、一様な値にはしない。
    fn graded_file(
        index: usize,
        id: ActivityId,
        plan: &DecodePlan,
        samples: &[(u64, u64, u64, u64)],
    ) -> FileSamples {
        let items = samples
            .iter()
            .map(|(ust, uptime, base, step)| {
                let values = (0..plan.fields.len())
                    .map(|i| Availability::Present(base + step * i as u64))
                    .collect();
                (
                    *ust,
                    *uptime,
                    ItemSnapshot {
                        key: None,
                        texts: Vec::new(),
                        values,
                    },
                )
            })
            .collect();
        samples_of(index, id, plan.clone(), items)
    }

    /// 指定フィールドだけに値を入れたファイル 1 つ分を組む。
    fn field_file_samples(
        index: usize,
        id: ActivityId,
        field: &str,
        samples: &[(u64, u64, u64)],
    ) -> FileSamples {
        let plan = plan_of(id);
        let items = samples
            .iter()
            .map(|(ust, uptime, value)| (*ust, *uptime, item_of(&plan, &[(field, *value)])))
            .collect();
        samples_of(index, id, plan, items)
    }

    fn opts_for_merge() -> MultiOptions {
        MultiOptions {
            summary: SummaryOptions {
                retain: RetainTimelines::All,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    /// **差分をファイル単位で完結させない。**
    ///
    /// 日境界をまたいだ区間の差分が集計に入ることを固定する。
    #[test]
    fn delta_is_carried_across_the_file_boundary() {
        let id = ActivityId::SWAP;
        let opts = opts_for_merge();
        let mut m = GroupMerger::new(&opts, &[]);
        // ファイル 1: 起動時刻 1000。カウンタ 0 → 100
        m.push_file(file_samples(
            0,
            id,
            vec![(2_000, 100_000, 0), (2_010, 101_000, 100)],
            &[],
            1,
        ));
        // ファイル 2: 同じ起動区間の続き。100 → 300
        m.push_file(file_samples(
            1,
            id,
            vec![(2_020, 102_000, 300), (2_030, 103_000, 400)],
            &[],
            1,
        ));
        let (files, segments) = m.finish();
        assert_eq!(files, vec![0, 1]);
        assert_eq!(segments.len(), 1, "同じ起動区間なので 1 本");

        let seg = &segments[0];
        let c = seg.summary.column(id, SINGLE_ITEM, "pswpin").expect("列");
        // 有効区間は 3 本 (ファイル 1 内 1 本 + 境界 1 本 + ファイル 2 内 1 本)
        assert_eq!(c.intervals, 3, "境界の区間が集計に入る: {c:?}");
        assert_eq!(c.delta_total.as_deref(), Some("400"));
        assert_eq!(c.denominator_total.as_deref(), Some("3000"));

        // 境界の判定が記録される
        assert_eq!(seg.boundaries.len(), 1);
        let b = &seg.boundaries[0];
        assert_eq!((b.prev_file, b.next_file), (0, 1));
        assert!(b.decision.continuous);
        assert_eq!(b.decision.reason, None);
    }

    /// 連続性が確認できないファイル境界では差分を作らない。
    #[test]
    fn unverifiable_boundary_does_not_produce_a_delta() {
        let id = ActivityId::SWAP;
        let opts = opts_for_merge();
        let mut m = GroupMerger::new(&opts, &[]);
        m.push_file(file_samples(
            0,
            id,
            vec![(2_000, 100_000, 0), (2_010, 101_000, 100)],
            &[],
            1,
        ));
        // 起動時刻が違う (再起動を挟んだ) ファイル
        m.push_file(file_samples(
            1,
            id,
            vec![(2_020, 500, 5), (2_030, 1_500, 105)],
            &[],
            1,
        ));
        let (_, segments) = m.finish();
        assert_eq!(segments.len(), 2, "起動区間が分かれる");

        for seg in &segments {
            let c = seg.summary.column(id, SINGLE_ITEM, "pswpin").expect("列");
            assert_eq!(c.intervals, 1, "各区間は自分の中の 1 本だけ");
            assert_eq!(c.delta_total.as_deref(), Some("100"));
        }
        // 境界の記録は新しい区間側に残る
        let b = segments
            .iter()
            .flat_map(|s| s.boundaries.iter())
            .find(|b| b.prev_file == 0 && b.next_file == 1)
            .expect("境界の記録");
        assert!(!b.decision.continuous);
        assert_eq!(b.decision.reason, Some(BreakReason::BootEpochChanged));
    }

    /// レイアウトが違うファイルの値は基準値にしない。
    #[test]
    fn layout_change_across_files_is_not_carried() {
        let id = ActivityId::SWAP;
        let opts = opts_for_merge();
        let mut m = GroupMerger::new(&opts, &[]);
        m.push_file(file_samples(
            0,
            id,
            vec![(2_000, 100_000, 0), (2_010, 101_000, 100)],
            &[],
            1,
        ));
        // 署名が違う (別 revision で採取されたファイル)
        m.push_file(file_samples(1, id, vec![(2_020, 102_000, 300)], &[], 2));
        let (_, segments) = m.finish();
        assert_eq!(segments.len(), 1, "再起動ではないので区間は 1 本");
        let seg = &segments[0];
        let c = seg.summary.column(id, SINGLE_ITEM, "pswpin").expect("列");
        assert_eq!(c.intervals, 1, "境界の差分は作らない");
        assert_eq!(
            seg.boundaries[0].decision.reason,
            Some(BreakReason::LayoutChanged)
        );
    }

    /// **同一性検査の結果が境界判定に効く。**
    ///
    /// カーネル版が変わったファイル間は [`IdentityVerdict::LikelySameMachine`]
    /// と診断される。時刻もレイアウトも揃っているので、判定を使わずに
    /// `Identical` 固定で境界を見ると差分を引き継いでしまい、
    /// 「再起動を挟んでいるので差分は引き継がない」という診断の注記と
    /// 実際の集計動作が矛盾する。
    #[test]
    fn identity_change_across_files_breaks_the_delta() {
        let id = ActivityId::SWAP;
        let opts = opts_for_merge();
        let mut m = GroupMerger::new(&opts, &[]);
        m.push_file(file_samples(
            0,
            id,
            vec![(2_000, 100_000, 0), (2_010, 101_000, 100)],
            &[],
            1,
        ));
        // 起動時刻・レイアウトは揃っているが、カーネル版だけが違うファイル
        let mut next = file_samples(
            1,
            id,
            vec![(2_020, 102_000, 300), (2_030, 103_000, 400)],
            &[],
            1,
        );
        next.identity = Arc::new(HostIdentity {
            release: "6.1.0-generic".to_string(),
            ..ident("testhost")
        });
        assert_eq!(
            identity_verdict(&ident("testhost"), &next.identity),
            IdentityVerdict::LikelySameMachine
        );
        m.push_file(next);

        let (_, segments) = m.finish();
        assert_eq!(segments.len(), 2, "識別材料が変わったら起動区間を分ける");
        for seg in &segments {
            let c = seg.summary.column(id, SINGLE_ITEM, "pswpin").expect("列");
            assert_eq!(c.intervals, 1, "境界の差分は作らない: {c:?}");
            assert_eq!(c.delta_total.as_deref(), Some("100"));
        }
        // 区間ごとに、その区間のファイルが申告した版が載る
        assert_eq!(
            segments[0].summary.source.release.as_deref(),
            Some("5.15.0-generic")
        );
        assert_eq!(
            segments[1].summary.source.release.as_deref(),
            Some("6.1.0-generic")
        );
        // 境界の記録には理由が残る
        let b = &segments[1].boundaries[0];
        assert!(!b.decision.continuous);
        assert_eq!(b.decision.reason, Some(BreakReason::IdentityChanged));
        assert!(b.decision.new_segment);
    }

    /// CPU 数だけが変わった場合も差分を引き継がない。
    ///
    /// `cpu_nr` は判定 (`run_queue`) の分母でもあるので、
    /// 区間ごとにその区間の申告値が使われることも併せて確認する。
    #[test]
    fn cpu_count_change_across_files_breaks_the_delta() {
        let id = ActivityId::SWAP;
        let opts = opts_for_merge();
        let mut m = GroupMerger::new(&opts, &[]);
        m.push_file(file_samples(
            0,
            id,
            vec![(2_000, 100_000, 0), (2_010, 101_000, 100)],
            &[],
            1,
        ));
        let mut next = file_samples(
            1,
            id,
            vec![(2_020, 102_000, 300), (2_030, 103_000, 400)],
            &[],
            1,
        );
        next.identity = Arc::new(HostIdentity {
            cpu_nr: Some(8),
            ..ident("testhost")
        });
        m.push_file(next);

        let (_, segments) = m.finish();
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].summary.source.cpu_nr, Some(4));
        assert_eq!(segments[1].summary.source.cpu_nr, Some(8));
    }

    /// **切るのは識別材料が変わった境界だけ。**
    ///
    /// 判定は「グループ代表との比較」ではなく「直前サンプルとの比較」で行う。
    /// 代表と比べると `A → B → B → A` の 2 本目の B で無用に区間が切れ、
    /// 構成が戻った 4 本目で切れない。
    #[test]
    fn only_the_changing_boundary_is_broken() {
        let id = ActivityId::SWAP;
        let opts = opts_for_merge();
        let other = Arc::new(HostIdentity {
            release: "6.1.0-generic".to_string(),
            ..ident("testhost")
        });
        let mut m = GroupMerger::new(&opts, &[]);
        // A → B → B → A の 4 ファイル (時刻・レイアウトはすべて連続)
        for (index, release_changed) in [(0, false), (1, true), (2, true), (3, false)] {
            let ust = 2_000 + index as u64 * 20;
            let uptime = 100_000 + index as u64 * 2_000;
            let counter = index as u64 * 200;
            let mut fs = file_samples(
                index,
                id,
                vec![
                    (ust, uptime, counter),
                    (ust + 10, uptime + 1_000, counter + 100),
                ],
                &[],
                1,
            );
            if release_changed {
                fs.identity = Arc::clone(&other);
            }
            m.push_file(fs);
        }
        let (_, segments) = m.finish();

        assert_eq!(segments.len(), 3, "切れるのは A→B と B→A の 2 箇所だけ");
        // 区間 1 は B のファイル 2 本分。その内側と境界で 3 区間ぶん差分が取れる
        let c = segments[1]
            .summary
            .column(id, SINGLE_ITEM, "pswpin")
            .expect("列");
        assert_eq!(c.intervals, 3, "B → B の境界は引き継ぐ: {c:?}");
        assert_eq!(segments[1].files, vec![1, 2]);
        assert_eq!(
            segments[1].summary.source.release.as_deref(),
            Some("6.1.0-generic")
        );
        assert_eq!(
            segments[2].summary.source.release.as_deref(),
            Some("5.15.0-generic"),
            "構成が戻った区間は元の版で報告する"
        );
    }

    /// ファイル内の RESTART でも起動区間を分ける。
    #[test]
    fn restart_inside_a_file_splits_segments() {
        let id = ActivityId::SWAP;
        let opts = opts_for_merge();
        let mut m = GroupMerger::new(&opts, &[]);
        m.push_file(file_samples(
            0,
            id,
            vec![
                (2_000, 100_000, 0),
                (2_010, 101_000, 100),
                (2_020, 1_000, 5),
                (2_030, 2_000, 105),
            ],
            &[2],
            1,
        ));
        let (_, segments) = m.finish();
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].index, 0);
        assert_eq!(segments[1].index, 1);
        for seg in &segments {
            let c = seg.summary.column(id, SINGLE_ITEM, "pswpin").expect("列");
            assert_eq!(c.intervals, 1);
        }
    }

    /// 判定はホストごと・起動区間ごとに付く。
    #[test]
    fn findings_are_attached_per_segment() {
        let id = ActivityId::SWAP;
        let opts = opts_for_merge();
        let mut m = GroupMerger::new(&opts, &[]);
        m.push_file(file_samples(
            0,
            id,
            vec![(2_000, 100_000, 0), (2_010, 101_000, 100)],
            &[],
            1,
        ));
        let (_, segments) = m.finish();
        let f = &segments[0].findings;
        assert!(!f.is_empty(), "ルール判定が付く");
        assert!(f.iter().all(|x| !x.rule_id.is_empty()));
        // CPU の指標が無いので CPU 系は判定不能になる
        let cpu = f.iter().find(|x| x.rule_id == "cpu-saturation").unwrap();
        assert_eq!(cpu.verdict, crate::analyze::rules::Verdict::Undetermined);
    }

    /// ファイルの並び順は日付 → 作成時刻 → パスで決まる (名前の辞書順に依存しない)。
    #[test]
    fn file_order_is_deterministic_by_date() {
        let mk = |index: usize, path: &str, day: u8, ust: u64| FileOutline {
            index,
            path: path.to_string(),
            identity: ident("host-a"),
            format_magic: 0x2175,
            sysstat_version: "12.6.0".to_string(),
            year: 2026,
            month: 1,
            day,
            tzname: None,
            header_ust_time: ust,
            activities: 1,
        };
        let mut v = [
            mk(0, "sa10", 10, 300),
            mk(1, "sa02", 2, 100),
            mk(2, "sa09", 9, 200),
        ];
        v.sort_by(|a, b| a.order_key().cmp(&b.order_key()));
        let order: Vec<&str> = v.iter().map(|o| o.path.as_str()).collect();
        assert_eq!(order, vec!["sa02", "sa09", "sa10"]);
    }

    // -----------------------------------------------------------------------
    // ホスト間比較
    // -----------------------------------------------------------------------

    fn gauge_timeline(points: &[(u64, u64, Option<f64>)]) -> MetricTimeline {
        let mut t = MetricTimeline::new(
            MetricKey::new(ActivityId::MEMORY, SINGLE_ITEM, "kbmemfree"),
            Unit::Kilobytes,
            ValueKind::Gauge,
        );
        for (start, end, v) in points {
            let elapsed = (end - start) * 100;
            t.push(match v {
                Some(v) => MetricPoint::observed(*start, *end, elapsed, *v),
                None => MetricPoint::missing(
                    *start,
                    *end,
                    elapsed,
                    crate::analyze::timeline::ExclusionReason::MissingInSample,
                ),
            });
        }
        t
    }

    #[test]
    fn common_window_is_the_intersection() {
        let w = common_window(&[(100, 400), (200, 500)], 100).unwrap();
        assert_eq!(w.start_ust, 200);
        assert_eq!(w.end_ust, 400);
        assert_eq!(w.bucket_count(), 2);
        // 重ならなければ窓は作れない
        assert!(common_window(&[(100, 200), (300, 400)], 100).is_none());
    }

    /// **観測が無い区間を 0 として比較しない。**
    #[test]
    fn buckets_without_observation_are_none_not_zero() {
        let t = gauge_timeline(&[(0, 100, Some(10.0))]);
        let w = ComparisonWindow::new(0, 300, 100);
        let b = align_to_window(&t, w, GaugeFill::None);
        assert_eq!(b.len(), 3);
        assert_eq!(b[0].value, Some(10.0));
        assert_eq!(b[0].source, BucketSource::Observed);
        assert_eq!(b[1].value, None, "0 で埋めない");
        assert_eq!(b[1].source, BucketSource::NoObservation);
        assert_eq!(b[2].value, None);
    }

    /// 持ち越しは明示的に選択したときだけ行われ、出どころが残る。
    #[test]
    fn hold_last_is_opt_in_and_recorded() {
        let t = gauge_timeline(&[(0, 100, Some(10.0))]);
        let w = ComparisonWindow::new(0, 300, 100);

        let without = align_to_window(&t, w, GaugeFill::None);
        assert_eq!(without[1].value, None);

        let held = align_to_window(&t, w, GaugeFill::HoldLast { max_age_secs: 100 });
        assert_eq!(held[1].value, Some(10.0));
        assert_eq!(
            held[1].source,
            BucketSource::HeldFromPrevious { age_secs: 0 }
        );
        assert_eq!(
            held[2].source,
            BucketSource::HeldFromPrevious { age_secs: 100 },
            "持ち越しの経過時間を出どころに残す"
        );

        // 許容期間を超えた区間は埋めない
        let short = align_to_window(&t, w, GaugeFill::HoldLast { max_age_secs: 50 });
        assert_eq!(short[1].value, Some(10.0), "age=0 は許容内");
        assert_eq!(short[2].value, None, "age=100 は許容 50 を超える");
        assert_eq!(short[2].source, BucketSource::NoObservation);
    }

    /// 線形補間も明示的な選択が要る。
    #[test]
    fn linear_fill_is_opt_in() {
        let t = gauge_timeline(&[(0, 100, Some(0.0)), (300, 400, Some(30.0))]);
        let w = ComparisonWindow::new(0, 400, 100);
        let none = align_to_window(&t, w, GaugeFill::None);
        assert_eq!(none[1].value, None);
        assert_eq!(none[2].value, None);

        let lin = align_to_window(&t, w, GaugeFill::Linear { max_gap_secs: 300 });
        assert_eq!(lin[1].source, BucketSource::Interpolated);
        assert!(lin[1].value.unwrap() > 0.0 && lin[1].value.unwrap() < 30.0);
    }

    /// レートは持ち越しも補間もしない (増分 0 と観測なしを区別できないため)。
    #[test]
    fn counter_rates_are_never_filled() {
        let mut t = MetricTimeline::new(
            MetricKey::new(ActivityId::SWAP, SINGLE_ITEM, "pswpin"),
            Unit::CountPerSec,
            ValueKind::Counter,
        );
        t.push(MetricPoint::observed(0, 100, 10_000, 5.0));
        let w = ComparisonWindow::new(0, 300, 100);
        let b = align_to_window(
            &t,
            w,
            GaugeFill::HoldLast {
                max_age_secs: 10_000,
            },
        );
        assert_eq!(b[0].value, Some(5.0));
        assert_eq!(b[1].value, None, "レートは持ち越さない");
        assert_eq!(b[1].source, BucketSource::NoObservation);
    }

    /// 区間値は重なった観測の時間加重平均。
    #[test]
    fn bucket_value_is_time_weighted_over_overlap() {
        // 0〜50 秒は 0、50〜100 秒は 100 → 平均 50
        let t = gauge_timeline(&[(0, 50, Some(0.0)), (50, 100, Some(100.0))]);
        let w = ComparisonWindow::new(0, 100, 100);
        let b = align_to_window(&t, w, GaugeFill::None);
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].value, Some(50.0));
        assert_eq!(b[0].covered_secs, 100);
    }

    /// 比較できる区間数は「全ホストに値がある区間」だけを数える。
    #[test]
    fn comparable_buckets_require_all_hosts() {
        let a = gauge_timeline(&[(0, 100, Some(1.0)), (100, 200, Some(2.0))]);
        let b = gauge_timeline(&[(0, 100, Some(3.0))]);
        let w = ComparisonWindow::new(0, 200, 100);
        let c = compare_hosts(
            MetricKey::new(ActivityId::MEMORY, SINGLE_ITEM, "kbmemfree"),
            &[
                ("host-a".to_string(), ident("host-a"), &a),
                ("host-b".to_string(), ident("host-b"), &b),
            ],
            w,
            GaugeFill::None,
        );
        assert_eq!(c.hosts.len(), 2);
        assert_eq!(c.comparable_buckets, 1, "2 区間目は host-b に観測が無い");
        assert_eq!(c.hosts[0].observed_buckets(), 2);
        assert_eq!(c.hosts[1].observed_buckets(), 1);
    }

    /// 欠損として記録された点は集約に使わない。
    #[test]
    fn missing_points_are_not_aggregated() {
        let t = gauge_timeline(&[(0, 100, None)]);
        let w = ComparisonWindow::new(0, 100, 100);
        let b = align_to_window(&t, w, GaugeFill::None);
        assert_eq!(b[0].value, None);
        assert_eq!(b[0].source, BucketSource::NoObservation);
    }

    // -----------------------------------------------------------------------
    // 集計期間の絞り込み (`summarize` / `compare` の `--from` / `--to`)
    // -----------------------------------------------------------------------

    /// epoch 秒の両端から時刻フィルタを組む。
    ///
    /// テストのスナップショットは時分秒を持たないので、比較は epoch で行う
    /// (`--from 1555593629` のように 10 桁 epoch を渡した場合と同じ経路)。
    fn epoch_window(from: Option<u64>, to: Option<u64>) -> TimeFilter {
        use crate::output::time_filter::{CrossDayRule, TimeBasis, TimeBound};
        TimeFilter {
            start: from.map_or(TimeBound::None, TimeBound::Epoch),
            end: to.map_or(TimeBound::None, TimeBound::Epoch),
            basis: TimeBasis::Utc,
            cross_day: CrossDayRule::Sadf,
        }
    }

    fn opts_with_window(filter: TimeFilter) -> MultiOptions {
        MultiOptions {
            time_filter: filter,
            ..opts_for_merge()
        }
    }

    /// 4 サンプル (10 秒間隔・カウンタ増分つき) を 2 ファイルに分けて流す。
    fn merge_two_files(opts: &MultiOptions, id: ActivityId) -> Vec<BootSegment> {
        let mut m = GroupMerger::new(opts, &[]);
        m.push_file(file_samples(
            0,
            id,
            vec![(2_000, 100_000, 0), (2_010, 101_000, 100)],
            &[],
            1,
        ));
        m.push_file(file_samples(
            1,
            id,
            vec![(2_020, 102_000, 400), (2_030, 103_000, 900)],
            &[],
            1,
        ));
        m.finish().1
    }

    /// **範囲を全体に取った結果は、絞らない結果と 1 バイトも変わらない。**
    ///
    /// 基準レコード (`Admit::Reference`) を無条件に落とすと、何も絞られて
    /// いないのに先頭サンプルがゲージの標本平均と p95 から落ちる。
    /// ファイル境界の引き継ぎも失われる (2 本目の先頭も基準レコードになる)。
    #[test]
    fn a_window_covering_everything_changes_nothing() {
        let id = ActivityId::SWAP;
        let bare = merge_two_files(&opts_with_window(epoch_window(None, None)), id);
        let wide = merge_two_files(
            &opts_with_window(epoch_window(Some(1_000), Some(9_000))),
            id,
        );
        assert_eq!(wide, bare, "範囲が全体を覆うなら結果は同じ");

        // 念のため中身も見る (等値比較が両方とも空になっていないこと)
        let c = bare[0].summary.column(id, SINGLE_ITEM, "pswpin").unwrap();
        assert_eq!(c.intervals, 3);
        assert_eq!(bare[0].summary.period.first_ust, Some(2_000));
        assert_eq!(bare[0].summary.period.last_ust, Some(2_030));
        assert_eq!(bare[0].boundaries.len(), 1, "境界の記録も残る");
    }

    /// 範囲を絞ると期間の端点も絞られ、基準レコードが差分の起点になる。
    #[test]
    fn window_narrows_the_period_and_keeps_the_reference_as_the_delta_origin() {
        let id = ActivityId::SWAP;
        let segs = merge_two_files(
            &opts_with_window(epoch_window(Some(2_010), Some(2_030))),
            id,
        );
        assert_eq!(segs.len(), 1);
        let p = &segs[0].summary.period;
        // 2_000 は範囲外、2_010 は基準として消費、数えるのは 2_020 / 2_030
        assert_eq!(p.samples, 2, "範囲外のサンプルは数えない");
        assert_eq!(p.first_ust, Some(2_010), "期間の始点は基準レコードの時刻");
        assert_eq!(p.last_ust, Some(2_030));
        assert_eq!(p.covered_cs, 2_000, "20 秒ぶんだけ覆う");
        assert_eq!(p.continuous_intervals, 2);
        assert_eq!(
            p.broken_intervals, 0,
            "基準レコードがあるので先頭は落ちない"
        );

        let c = segs[0].summary.column(id, SINGLE_ITEM, "pswpin").unwrap();
        assert_eq!(c.intervals, 2);
        assert_eq!(c.delta_total.as_deref(), Some("800"), "300 + 500");
        assert_eq!(c.denominator_total.as_deref(), Some("2000"));
        assert_eq!(c.mean, Some(40.0), "800 / 20 秒");
        assert_eq!(c.max.unwrap().value, 50.0);
        assert_eq!(c.min.unwrap().value, 30.0);
        assert!(
            c.exclusions.is_empty(),
            "基準レコードを起点に使うので first_sample は出ない: {:?}",
            c.exclusions
        );

        // 絞らない場合と比べて、確かに減っている
        let bare = merge_two_files(&opts_with_window(epoch_window(None, None)), id);
        let b = bare[0].summary.column(id, SINGLE_ITEM, "pswpin").unwrap();
        assert_eq!(bare[0].summary.period.covered_cs, 3_000);
        assert_eq!(b.intervals, 3);
    }

    /// **区間が 1 本だけになる範囲では、平均とその区間の瞬時値が一致する。**
    ///
    /// `analyze::summary` の `single_interval_mean_matches_instant_value` と
    /// 同じ趣旨の窓つき版。期間平均は「差分合計 ÷ 分母合計」から作るため、
    /// 絞り込みで分母の集め方を間違えると (基準レコードの区間を分母にだけ
    /// 足す等) ここが機械的に食い違う。
    #[test]
    fn a_single_interval_window_averages_to_the_instant_value() {
        let mut mismatched: Vec<String> = Vec::new();
        let mut checked = 0usize;

        for def in crate::layout::registry::all() {
            let Some(plan) = latest_plan(def) else {
                continue;
            };
            // 2_010 を基準にして 2_010 → 2_020 の 1 区間だけを数える
            let opts = opts_with_window(epoch_window(Some(2_010), Some(2_020)));
            let mut m = GroupMerger::new(&opts, &[]);
            m.push_file(graded_file(
                0,
                def.id,
                &plan,
                &[
                    (2_000, 100_000, 100, 7),
                    (2_010, 101_000, 1_000, 37),
                    (2_020, 102_000, 5_000, 101),
                    (2_030, 103_000, 9_000, 211),
                ],
            ));
            let segs = m.finish().1;
            let Some(item) = segs
                .first()
                .and_then(|s| s.summary.activities.first())
                .and_then(|a| a.items.first())
            else {
                continue;
            };
            assert_eq!(
                segs[0].summary.period.samples, 1,
                "{}: 数える区間は 1 本だけ",
                def.id
            );
            for c in &item.columns {
                if c.method != crate::analyze::summary::AggregationMethod::RateOverValidIntervals
                    || c.intervals == 0
                {
                    continue;
                }
                let (Some(mean), Some(max)) = (c.mean, c.max) else {
                    continue;
                };
                checked += 1;
                if (mean - max.value).abs() > max.value.abs() * 1e-9 + 1e-12 {
                    mismatched.push(format!(
                        "{} {}: 平均 {mean} vs 瞬時 {}",
                        def.id, c.column, max.value
                    ));
                }
            }
        }

        assert!(
            mismatched.is_empty(),
            "1 区間に絞ったときの平均と瞬時値が食い違う列がある: {mismatched:?}"
        );
        assert!(checked > 50, "検査した列が少なすぎる: {checked}");
    }

    /// 範囲外のサンプルは p95 の重みに入らない。
    #[test]
    fn samples_outside_the_window_do_not_weigh_on_the_percentile() {
        use crate::analyze::summary::PercentileOutcome;
        let id = ActivityId::MEMORY;
        // 前半 10 本は 900、後半 10 本は 10。標本重みの p95 は
        // 全体なら 900、後半だけなら 10 になる。
        let mut samples: Vec<(u64, u64, u64)> = Vec::new();
        for i in 0..20u64 {
            let value = if i < 10 { 900 } else { 10 };
            samples.push((2_000 + i * 10, 100_000 + i * 1_000, value));
        }

        let p95_of = |filter: TimeFilter| {
            let opts = opts_with_window(filter);
            let mut m = GroupMerger::new(&opts, &[]);
            m.push_file(field_file_samples(0, id, "frmkb", &samples));
            let segs = m.finish().1;
            let c = segs[0]
                .summary
                .column(id, SINGLE_ITEM, "kbmemfree")
                .expect("列")
                .clone();
            match c.p95 {
                PercentileOutcome::Computed(r) => r,
                other => panic!("p95 が出ていない: {other:?}"),
            }
        };

        let all = p95_of(epoch_window(None, None));
        assert_eq!(all.value, 900.0, "全体なら前半の 900 が p95 を取る");
        assert_eq!(all.samples, 20);

        // 後半だけに絞る (2_090 が基準として消費され、数えるのは 2_100 以降)
        let narrowed = p95_of(epoch_window(Some(2_090), None));
        assert_eq!(narrowed.value, 10.0, "範囲外の 900 は重みに入らない");
        assert_eq!(narrowed.samples, 10);
    }

    /// **範囲の外で起きた再起動も起動区間を分ける。**
    ///
    /// 時刻フィルタを起動区間の切り替えより先に適用すると、再起動後の
    /// 範囲内サンプルが再起動前の区間に入り、区間の `boot_epoch` と中身が
    /// 食い違う。範囲外になった区間は落ちるが、索引は詰めない
    /// (絞らない実行と同じ番号のままにする)。
    #[test]
    fn a_restart_outside_the_window_still_splits_segments() {
        let id = ActivityId::SWAP;
        let opts = opts_with_window(epoch_window(Some(2_030), None));
        let mut m = GroupMerger::new(&opts, &[]);
        m.push_file(file_samples(
            0,
            id,
            vec![
                (2_000, 100_000, 0),
                (2_010, 101_000, 100),
                // 再起動 (範囲外)
                (2_020, 1_000, 5),
                (2_030, 2_000, 105),
                (2_040, 3_000, 205),
            ],
            &[2],
            1,
        ));
        let segs = m.finish().1;
        assert_eq!(segs.len(), 1, "範囲外の区間は落ちる");
        assert_eq!(segs[0].index, 1, "索引は詰めない (欠番 0 は範囲外の区間)");
        assert_eq!(segs[0].boot_epoch, Some(2_010), "再起動後の起動時刻");

        let p = &segs[0].summary.period;
        assert_eq!(p.samples, 1);
        assert_eq!(p.first_ust, Some(2_030));
        assert_eq!(p.last_ust, Some(2_040));
        assert_eq!(p.covered_cs, 1_000);

        let c = segs[0].summary.column(id, SINGLE_ITEM, "pswpin").unwrap();
        assert_eq!(c.intervals, 1);
        assert_eq!(c.mean, Some(10.0), "100 / 10 秒");
        assert!(c.exclusions.is_empty(), "{:?}", c.exclusions);
    }

    /// 範囲にサンプルが 1 つも入らなければ起動区間は出ない (空の集計を出さない)。
    #[test]
    fn a_window_with_no_samples_yields_no_segments() {
        let id = ActivityId::SWAP;
        let segs = merge_two_files(&opts_with_window(epoch_window(Some(5_000), None)), id);
        assert!(segs.is_empty(), "{segs:?}");
    }

    /// **`compare` の共通窓は「各ホストの観測範囲の交差 ∩ 指定範囲」になる。**
    ///
    /// 共通窓は絞った後の集計期間から求めるので、合成は自動的に交差になる
    /// ([`common_window`] の doc を参照)。
    #[test]
    fn the_comparison_window_intersects_the_time_filter() {
        let id = ActivityId::SWAP;
        // host-a は 2_000〜2_060、host-b は 2_030〜2_090 を観測している
        let series = |start: u64, count: u64| -> Vec<(u64, u64, u64)> {
            (0..count)
                .map(|i| {
                    (
                        start + i * 10,
                        100_000 + (start - 2_000 + i * 10) * 100,
                        i * 100,
                    )
                })
                .collect()
        };
        let range_of = |filter: TimeFilter, samples: Vec<(u64, u64, u64)>| {
            let opts = opts_with_window(filter);
            let mut m = GroupMerger::new(&opts, &[]);
            m.push_file(file_samples(0, id, samples, &[], 1));
            let segs = m.finish().1;
            let p = segs[0].summary.period;
            (p.first_ust.unwrap(), p.last_ust.unwrap())
        };

        let bare = [
            range_of(epoch_window(None, None), series(2_000, 7)),
            range_of(epoch_window(None, None), series(2_030, 7)),
        ];
        let unclipped = common_window(&bare, 10).expect("交差がある");
        assert_eq!((unclipped.start_ust, unclipped.end_ust), (2_030, 2_060));

        // --from 2_040 --to 2_050 で絞る
        let filter = epoch_window(Some(2_040), Some(2_050));
        let clipped = [
            range_of(filter, series(2_000, 7)),
            range_of(filter, series(2_030, 7)),
        ];
        let w = common_window(&clipped, 10).expect("交差がある");
        assert_eq!(
            (w.start_ust, w.end_ust),
            (unclipped.start_ust.max(2_040), unclipped.end_ust.min(2_050)),
            "共通窓と指定範囲の交差になる"
        );
        assert_eq!((w.start_ust, w.end_ust), (2_040, 2_050));
    }
}

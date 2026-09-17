//! 独自 JSON 出力と、**独自出力すべてが共有する公開スキーマ**。
//!
//! # 内部構造をそのまま出さない
//!
//! `Snapshot` / `ItemSnapshot` / `DecodePlan` のような内部構造体に `Serialize` を
//! 付けて出すと、内部のリファクタリングが外部形式の破壊的変更になってしまう。
//! そこでこのモジュールに**公開スキーマの型を別に定義**し、内部構造からの写しを
//! 明示的に書く。公開形が変わるのは [`SCHEMA_VERSION`] を上げたときだけ。
//!
//! 公開スキーマは `table` / `csv` / `ndjson` からも使う。
//! (`src/output/mod.rs` は `sar_text` と共有されるため、共通の置き場所として
//! ここを使っている。)
//!
//! # 値の表し方
//!
//! - 生値 (`raw`) と派生値 (`rates`) を**別の名前空間**に置く。
//!   同じ名前で単位の違う値が混ざるのを防ぐ。
//! - `u64` の生値は**十進文字列**で出す。JavaScript の `Number` は 2^53 を超えると
//!   精度が落ちるため、累積カウンタを数値で出してはいけない。
//! - 値が無い場合は `null` + `quality` に理由を入れる。**0 で埋めない。**

use std::io::Write;

use serde::Serialize;

use super::sadf::access::{ActivityPair, ItemPair};
use super::sadf::render::item_label;
use super::sadf::{FileInfo, spec};
use crate::error::Result;
use crate::format::file::{SaFile, ScanControl};
use crate::layout::registry::{ActivityDef, ColumnMeta};
use crate::model::{ActivityId, Availability, DisplayTz, ValueKind};
use crate::output::time_filter::{Admit, TimeFilter};
use crate::series::compute::ComputeIssue;
use crate::series::{IntervalView, RecordEvent, Selection, WalkItem, walk_items};

/// 公開スキーマの版。
///
/// 内部構造の変更ではなく、**公開形が変わったときだけ**上げる。
pub const SCHEMA_VERSION: &str = crate::model::NATIVE_SCHEMA_VERSION;

// ===========================================================================
// 設定
// ===========================================================================

/// 生値と派生値のどちらを出すか (`--values raw|rates|both`)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ValueScope {
    /// 累積カウンタの生値のみ。
    Raw,
    /// レート・割合などの派生値のみ。
    #[default]
    Rates,
    /// 両方を別名前空間で出す。
    Both,
}

impl ValueScope {
    pub fn wants_raw(self) -> bool {
        matches!(self, ValueScope::Raw | ValueScope::Both)
    }
    pub fn wants_rates(self) -> bool {
        matches!(self, ValueScope::Rates | ValueScope::Both)
    }
}

/// 独自出力の設定。
#[derive(Debug, Clone, Default)]
pub struct CustomConfig {
    /// 対象 activity。
    pub selection: Selection,
    pub values: ValueScope,
    /// `--from` / `--to` の時刻フィルタ。既定は無効 (全レコードを出す)。
    ///
    /// 意味論は `sar -s` / `-e` と同じで、範囲に最初に合致したレコードは
    /// 差分の基準として消費されるだけで行にならない
    /// (`crate::output::time_filter` 参照)。
    pub time_filter: TimeFilter,
    /// IRQ の集約行に加えて、記録されている CPU 別内訳を出す。
    pub irq_cpus: bool,
    /// 時刻の表示と `--from` / `--to` の解釈に使うタイムゾーン。
    ///
    /// **`time_filter.basis` と必ず同じ基準にする。** 表示だけ動かすと、
    /// 画面に出ている 09:00 と `--from 09:00` が食い違う。
    /// epoch 秒で出す項目 (`start_epoch` / `end_epoch`) はここで変わらない。
    pub tz: DisplayTz,
}

// ===========================================================================
// 公開スキーマ
// ===========================================================================

/// ホスト・ファイル情報。
#[derive(Debug, Clone, Serialize)]
pub struct HostOut {
    pub hostname: String,
    pub sysname: String,
    pub release: String,
    pub machine: String,
    pub cpu_count: u32,
    /// ファイル作成日 (`YYYY-MM-DD`)。表示タイムゾーンで開いた日付。
    pub file_date: String,
    /// **採取したホストの**タイムゾーン名 (`sa_tzname`)。古いファイルでは空。
    ///
    /// 表示に使っているタイムゾーンではない。読み手側の基準は `--timezone`
    /// が決める (既定はローカル)。両者は普通は違う。
    pub timezone: String,
    /// 出典 (読み込んだファイルのパス)。
    pub source: String,
}

impl HostOut {
    pub fn new(file: &SaFile, tz: DisplayTz) -> Self {
        let info = FileInfo::from_file(file);
        Self {
            hostname: info.nodename,
            sysname: info.sysname,
            release: info.release,
            machine: info.machine,
            cpu_count: info.cpu_count,
            // `FileInfo::file_date` は sadf 既定 (UTC) の日付なので使わない。
            // 独自出力の日付は表示タイムゾーンで開く。
            file_date: tz.date(info.ust_time),
            timezone: info.tzname,
            source: file.path().display().to_string(),
        }
    }
}

/// 値の品質。
///
/// `Availability` (フィールドの在否) と `Discontinuity` (差分が取れない理由) を
/// **区別したまま**外へ出す。どちらも「0」ではない。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Quality {
    /// 値が取れた (0 も正常値)。
    Ok,
    /// その世代のファイルにフィールドが無い。
    UnsupportedBySource,
    /// フィールドはあるが当該レコードで欠落。
    MissingInSample,
    /// 前サンプルが無い。
    FirstSample,
    /// RESTART を挟んだ。
    Restart,
    /// item が入れ替わった (デバイス名の再利用 / CPU のオンライン変化)。
    ItemReplaced,
    /// 経過時間が 0 以下。
    NonPositiveElapsed,
    /// 減少したがラップとは断定できない。
    AmbiguousDecrease,
    /// 数値ではなく識別子の列。
    NotNumeric,
    /// item 群全体が必要な派生列。
    NeedsItemGroup,
    /// 計算式が未実装 (**0 とは違う**)。
    NotImplemented,
}

impl Quality {
    fn from_issue(issue: ComputeIssue) -> Self {
        use crate::series::Discontinuity as D;
        match issue {
            ComputeIssue::UnsupportedBySource => Quality::UnsupportedBySource,
            ComputeIssue::MissingInSample => Quality::MissingInSample,
            ComputeIssue::Discontinuous(D::FirstSample) => Quality::FirstSample,
            ComputeIssue::Discontinuous(D::Restart) => Quality::Restart,
            ComputeIssue::Discontinuous(D::ItemReplaced) => Quality::ItemReplaced,
            ComputeIssue::Discontinuous(D::NonPositiveElapsed) => Quality::NonPositiveElapsed,
            ComputeIssue::Discontinuous(D::AmbiguousDecrease) => Quality::AmbiguousDecrease,
            ComputeIssue::NotNumeric => Quality::NotNumeric,
            ComputeIssue::NeedsItemGroup => Quality::NeedsItemGroup,
            ComputeIssue::NotImplemented => Quality::NotImplemented,
        }
    }

    /// 短いラベル (CSV / テーブル用)。
    pub fn label(self) -> &'static str {
        match self {
            Quality::Ok => "ok",
            Quality::UnsupportedBySource => "unsupported_by_source",
            Quality::MissingInSample => "missing_in_sample",
            Quality::FirstSample => "first_sample",
            Quality::Restart => "restart",
            Quality::ItemReplaced => "item_replaced",
            Quality::NonPositiveElapsed => "non_positive_elapsed",
            Quality::AmbiguousDecrease => "ambiguous_decrease",
            Quality::NotNumeric => "not_numeric",
            Quality::NeedsItemGroup => "needs_item_group",
            Quality::NotImplemented => "not_implemented",
        }
    }
}

/// 1 列分の出力。
#[derive(Debug, Clone, Serialize)]
pub struct FieldOut {
    /// 公開名 (`ColumnMeta::public_name`)。`sar` のヘッダ表記とは独立。
    pub name: &'static str,
    pub unit: &'static str,
    /// `counter` / `gauge` / `identity`。
    pub kind: &'static str,
    /// 累積カウンタの生値。**十進文字列**で出す (JS の精度落ちを避ける)。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<String>,
    /// 派生値 (レート・割合)。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<f64>,
    /// 文字列フィールドの値 (デバイス名 / マウントポイント / 製品名など)。
    ///
    /// `A_FS` の `mountpoint` や `A_PWR_USB` の `manufacturer` のように、
    /// 1 item が複数の文字列を持つ activity でもすべて出せる。
    /// その世代に無いフィールドは `None` + `quality` に理由が入る。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    pub quality: Quality,
}

/// 1 item 分の出力。
#[derive(Debug, Clone, Serialize)]
pub struct ItemOut {
    /// item の識別子 (デバイス名 / `all` / `cpu0` など)。
    pub item: String,
    /// activity 内での添字。
    pub index: usize,
    /// IRQ の CPU 次元。`all` または 0 始まりの CPU 番号。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu: Option<String>,
    /// 生値の名前空間。`--values rates` では空。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub raw: Vec<FieldOut>,
    /// 派生値の名前空間。`--values raw` では空。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub rates: Vec<FieldOut>,
}

/// 1 activity 分の出力。
#[derive(Debug, Clone, Serialize)]
pub struct ActivityOut {
    /// 本家のシンボル名 (`A_CPU`)。
    pub activity: &'static str,
    /// 日本語の名称。
    pub label: &'static str,
    pub items: Vec<ItemOut>,
}

/// 1 サンプル (区間) 分の出力。
#[derive(Debug, Clone, Serialize)]
pub struct SampleOut {
    /// 起動区間の番号。RESTART をまたぐごとに 1 増える。
    pub boot: u32,
    /// 区間の始点 (epoch 秒)。前サンプルが無い場合は終点と同じ。
    pub start_epoch: u64,
    /// 区間の終点 (epoch 秒)。
    pub end_epoch: u64,
    /// 経過時間 (1/100 秒)。
    pub elapsed_cs: u64,
    /// 前サンプルとの間に不連続が無いか。
    pub continuous: bool,
    pub activities: Vec<ActivityOut>,
}

// ===========================================================================
// 内部構造 → 公開スキーマ
// ===========================================================================

/// activity 1 種を公開スキーマへ写す。
pub fn activity_out(pair: &ActivityPair<'_>, cfg: &CustomConfig) -> Option<ActivityOut> {
    let sp = spec::lookup(pair.id)?;
    let items = custom_items(pair, cfg);
    if items.is_empty() {
        return None;
    }
    Some(ActivityOut {
        activity: sp.name,
        label: pair.id.label().unwrap_or(""),
        items,
    })
}

/// 全独自形式で同じ item / CPU 次元を列挙する。
pub fn custom_items(pair: &ActivityPair<'_>, cfg: &CustomConfig) -> Vec<ItemOut> {
    let Some(sp) = spec::lookup(pair.id) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in pair.output_items() {
        let aggregate = item_out(pair.def, sp, &item, cfg);
        let label = aggregate.item.clone();
        out.push(aggregate);
        if pair.id != ActivityId::IRQ || !cfg.irq_cpus {
            continue;
        }
        // 古い一次元 IRQ は CPU all しか記録していない。CPU 別をゼロで捏造しない。
        for cpu in 1..item.row_len() {
            let Some(slot) = pair.matrix_item(cpu, item.index) else {
                continue;
            };
            let mut row = item_out(pair.def, sp, &slot, cfg);
            row.item = label.clone();
            row.index = item.index;
            row.cpu = Some((cpu - 1).to_string());
            out.push(row);
        }
    }
    out
}

/// item 1 個を公開スキーマへ写す。
///
/// # 行列型 (`A_IRQ` / `A_PWR_FREQ`) の表し方
///
/// 行列型は [`ActivityPair::output_items`] が「出力 1 行」を 1 item として返す。
/// `A_IRQ` なら 1 行 = 1 割り込みで、行の中に CPU ぶんのスロットが並ぶ。
/// 公開スキーマは 1 列 1 値なので、ここで出すのは
/// **その行の代表スロット** (`A_IRQ` は CPU `all`、02 §6.1) の値である。
/// `sar -I` の既定表示と同じ粒度で、CPU 別の内訳は `sar` 互換出力
/// (CPU 列を展開する `-P` 相当の経路) から得る。
pub fn item_out(
    def: &'static ActivityDef,
    sp: &spec::ActivitySpec,
    item: &ItemPair<'_>,
    cfg: &CustomConfig,
) -> ItemOut {
    let mut raw = Vec::new();
    let mut rates = Vec::new();

    for (i, col) in def.columns.iter().enumerate() {
        if cfg.values.wants_raw() {
            raw.push(raw_field(col, item, i));
        }
        if cfg.values.wants_rates() {
            rates.push(rate_field(col, item, i));
        }
    }

    // item の識別子は `sadf` と同じ組み立て (`render::item_label`) を共有する。
    // 行列型 (`A_IRQ`) の行も割り込み名で識別されるので、ここに独自出力専用の
    // 分岐は要らない (`irq_rows_are_identified_by_interrupt_name` が固定)。
    let label = item_label(sp, item);
    ItemOut {
        item: if label.jx.is_empty() {
            // アイテムを持たない activity (`A_MEMORY` など)
            "-".to_string()
        } else {
            label.jx
        },
        index: item.index,
        cpu: (sp.id == ActivityId::IRQ).then(|| "all".to_string()),
        raw,
        rates,
    }
}

/// 生値 1 列。**十進文字列**で出す。
///
/// センサ値 (rpm / degC / V) はファイル上で IEEE-754 の `double` として
/// 書かれているので、ビット列のままでは意味を持たない。
/// ここで実数として解釈し、`sadf -r` と同じ小数 6 桁で文字列化する。
fn raw_field(col: &ColumnMeta, item: &ItemPair<'_>, index: usize) -> FieldOut {
    if item.is_text_column(col.public_name) {
        return text_field(col, item);
    }
    let (raw, quality) = match item.raw_curr(index) {
        Availability::Present(v) if is_double_field(col) => {
            (Some(format!("{:.6}", f64::from_bits(v))), Quality::Ok)
        }
        Availability::Present(v) => (Some(v.to_string()), Quality::Ok),
        Availability::UnsupportedBySource => (None, Quality::UnsupportedBySource),
        Availability::MissingInSample => (None, Quality::MissingInSample),
    };
    FieldOut {
        name: col.public_name,
        unit: unit_name(col),
        kind: kind_name(col.kind),
        raw,
        value: None,
        text: None,
        quality,
    }
}

/// 派生値 1 列。
fn rate_field(col: &ColumnMeta, item: &ItemPair<'_>, index: usize) -> FieldOut {
    if item.is_text_column(col.public_name) {
        return text_field(col, item);
    }
    // 識別子列にレートは無い。数値の識別子 (バス番号 / ベンダ ID / バッテリ ID)
    // は生値をそのまま見せる。`-` に落とすと item の同定ができなくなる。
    if col.kind == ValueKind::Identity {
        return raw_identity_field(col, item, index);
    }
    // 独自出力は欠落を代替で埋めない (互換出力だけが本家の代替規則に従う)
    let (value, quality) = match item.computed_strict(index) {
        Ok(v) => (Some(v), Quality::Ok),
        Err(e) => (None, Quality::from_issue(e)),
    };
    FieldOut {
        name: col.public_name,
        unit: unit_name(col),
        kind: kind_name(col.kind),
        raw: None,
        value,
        text: None,
        quality,
    }
}

/// 数値の識別子 1 列。
///
/// 十進で出す。`sadf -j` が 16 進文字列にする `idvendor` / `idprod` とは表記が
/// 違うが、独自スキーマは互換仕様に縛られないので機械処理しやすい十進に揃える。
fn raw_identity_field(col: &ColumnMeta, item: &ItemPair<'_>, index: usize) -> FieldOut {
    let (raw, quality) = match item.raw_curr(index) {
        Availability::Present(v) => (Some(v.to_string()), Quality::Ok),
        Availability::UnsupportedBySource => (None, Quality::UnsupportedBySource),
        Availability::MissingInSample => (None, Quality::MissingInSample),
    };
    FieldOut {
        name: col.public_name,
        unit: unit_name(col),
        kind: kind_name(col.kind),
        raw,
        value: None,
        text: None,
        quality,
    }
}

/// 文字列 1 列。
///
/// 数値として意味を持たないので `raw` / `value` は空にし、`text` に入れる。
fn text_field(col: &ColumnMeta, item: &ItemPair<'_>) -> FieldOut {
    let text = item.text(col.public_name).map(|s| s.to_string());
    let quality = if text.is_some() {
        Quality::Ok
    } else {
        // 空文字とフィールド自体が無い場合は区別できないため欠落として扱う
        Quality::MissingInSample
    };
    FieldOut {
        name: col.public_name,
        unit: unit_name(col),
        kind: kind_name(col.kind),
        raw: None,
        value: None,
        text,
        quality,
    }
}

/// ファイル上で IEEE-754 の `double` として保存される列か。
///
/// sysstat のセンサ系構造体 (`stats_pwr_fan` / `_temp` / `_in`) は
/// `double` でファイルへ書く。`series` 層は整数として読むので、
/// 生値を出すときはここで解釈する必要がある。
fn is_double_field(col: &ColumnMeta) -> bool {
    use crate::model::Unit::*;
    matches!(col.unit, Rpm | Celsius | Volts)
}

/// 単位の公開表記。`Unit` の内部名ではなく安定した短い表記を使う。
pub fn unit_name(col: &ColumnMeta) -> &'static str {
    use crate::model::Unit::*;
    match col.unit {
        None => "",
        Percent => "percent",
        Count => "count",
        CountPerSec => "count/s",
        Bytes => "B",
        BytesPerSec => "B/s",
        Kilobytes => "kB",
        KilobytesPerSec => "kB/s",
        Sectors => "sector",
        SectorsPerSec => "sector/s",
        Milliseconds => "ms",
        Centiseconds => "cs",
        Microseconds => "us",
        Jiffies => "jiffy",
        Megahertz => "MHz",
        Celsius => "degC",
        Volts => "V",
        Rpm => "rpm",
        MilliampereHours => "mAh",
        Identifier => "identifier",
    }
}

pub fn kind_name(kind: ValueKind) -> &'static str {
    match kind {
        ValueKind::Counter => "counter",
        ValueKind::Gauge => "gauge",
        ValueKind::Identity => "identity",
    }
}

/// 起動区間の番号を数える。
///
/// RESTART をまたぐたびに 1 増える。累積カウンタの基準が変わる境界なので、
/// 集計側が区間をまたいだ差分を作らないための目印になる。
#[derive(Debug, Default)]
pub struct BootCounter(u32);

impl BootCounter {
    pub fn advance(&mut self, events: &[RecordEvent]) {
        for e in events {
            if matches!(e, RecordEvent::Restart { .. }) {
                self.0 += 1;
            }
        }
    }
    pub fn get(&self) -> u32 {
        self.0
    }
}

/// 1 レコードを公開スキーマのサンプルへ写す。
pub fn sample_out(view: &IntervalView<'_>, boot: u32, cfg: &CustomConfig) -> SampleOut {
    let mut activities = Vec::new();
    for id in selected_ids(view, cfg) {
        if let Some(pair) = ActivityPair::from_view(view, id)
            && let Some(a) = activity_out(&pair, cfg)
        {
            activities.push(a);
        }
    }
    SampleOut {
        boot,
        start_epoch: if view.has_prev {
            view.prev.ust_time
        } else {
            view.curr.ust_time
        },
        end_epoch: view.curr.ust_time,
        elapsed_cs: view.itv_cs,
        continuous: view.has_prev && view.continuous,
        activities,
    }
}

/// このレコードに含まれる activity を ID 昇順で返す。
pub fn selected_ids(view: &IntervalView<'_>, _cfg: &CustomConfig) -> Vec<ActivityId> {
    let mut ids: Vec<ActivityId> = view.curr.activities.iter().map(|a| a.id).collect();
    ids.sort_by_key(|i| i.0);
    ids.dedup();
    ids
}

// ===========================================================================
// 出力
// ===========================================================================

/// 独自 JSON の出力。
///
/// 文書全体を組み立ててから書くのではなく、**サンプル 1 つずつ**直列化する。
/// 保持するのは 1 レコード分だけ。
pub fn write_json<W: Write>(out: &mut W, file: &SaFile, cfg: &CustomConfig) -> Result<()> {
    let host = HostOut::new(file, cfg.tz);
    let head = serde_json::json!({
        "schema_version": SCHEMA_VERSION,
        "host": host,
    });
    let head = serde_json::to_string(&head).map_err(json_err)?;
    // 先頭の `{` を外して `"samples"` を継ぎ足す
    let head = head.trim_start_matches('{').trim_end_matches('}');
    write!(out, "{{{head},\"samples\":[").map_err(super::sadf::wrap_io)?;

    let mut boot = BootCounter::default();
    let mut first = true;
    let mut cursor = cfg.time_filter.cursor();
    walk_items(file, &cfg.selection.clone(), |item| {
        // `samples` しか持たない形式なので、イベントは起動区間の番号にだけ効かせる。
        let view = match item {
            WalkItem::Event(ev) => {
                boot.advance(std::slice::from_ref(&ev));
                return Ok(ScanControl::Continue);
            }
            WalkItem::Sample(view) => view,
        };
        match cursor.sample(view) {
            Admit::Skip | Admit::Reference => return Ok(ScanControl::Continue),
            Admit::Stop => return Ok(ScanControl::Stop),
            Admit::Emit => {}
        }
        let sample = sample_out(view, boot.get(), cfg);
        if sample.activities.is_empty() {
            return Ok(ScanControl::Continue);
        }
        if !first {
            out.write_all(b",").map_err(super::sadf::wrap_io)?;
        }
        first = false;
        serde_json::to_writer(&mut *out, &sample).map_err(json_err)?;
        Ok(ScanControl::Continue)
    })?;

    write!(out, "]}}").map_err(super::sadf::wrap_io)?;
    writeln!(out).map_err(super::sadf::wrap_io)?;
    Ok(())
}

fn json_err(e: serde_json::Error) -> crate::error::Error {
    crate::error::Error::Other(format!("JSON の直列化に失敗しました: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 公開スキーマの版は固定値で、内部変更では動かさない。
    #[test]
    fn schema_version_is_declared() {
        assert_eq!(SCHEMA_VERSION, "1.0");
    }

    /// `Availability` と `Discontinuity` を潰さずに区別して出す。
    #[test]
    fn quality_distinguishes_absence_reasons() {
        use crate::series::Discontinuity as D;
        assert_eq!(
            Quality::from_issue(ComputeIssue::UnsupportedBySource),
            Quality::UnsupportedBySource
        );
        assert_eq!(
            Quality::from_issue(ComputeIssue::MissingInSample),
            Quality::MissingInSample
        );
        assert_eq!(
            Quality::from_issue(ComputeIssue::Discontinuous(D::Restart)),
            Quality::Restart
        );
        assert_eq!(
            Quality::from_issue(ComputeIssue::Discontinuous(D::AmbiguousDecrease)),
            Quality::AmbiguousDecrease
        );
        assert_ne!(Quality::NotImplemented, Quality::Ok);
    }

    /// 生値は十進文字列として直列化される (JS の 2^53 問題を避ける)。
    #[test]
    fn raw_counters_are_decimal_strings() {
        let f = FieldOut {
            name: "user",
            unit: "percent",
            kind: "counter",
            raw: Some(18_446_744_073_709_551_615u64.to_string()),
            value: None,
            text: None,
            quality: Quality::Ok,
        };
        let s = serde_json::to_string(&f).unwrap();
        assert!(s.contains("\"raw\":\"18446744073709551615\""), "{s}");
        assert!(
            !s.contains("18446744073709551615,"),
            "数値で出してはいけない"
        );
    }

    /// 値が無い列は `null` ではなくキーごと省き、`quality` に理由が入る。
    #[test]
    fn absent_value_carries_its_reason() {
        let f = FieldOut {
            name: "areq_sz",
            unit: "kB",
            kind: "gauge",
            raw: None,
            value: None,
            text: None,
            quality: Quality::NotImplemented,
        };
        let s = serde_json::to_string(&f).unwrap();
        assert!(!s.contains("\"value\""), "{s}");
        assert!(s.contains("\"quality\":\"not_implemented\""), "{s}");
    }

    #[test]
    fn value_scope_selects_namespaces() {
        assert!(ValueScope::Raw.wants_raw() && !ValueScope::Raw.wants_rates());
        assert!(!ValueScope::Rates.wants_raw() && ValueScope::Rates.wants_rates());
        assert!(ValueScope::Both.wants_raw() && ValueScope::Both.wants_rates());
    }

    /// 単位は内部の `Unit` 名ではなく安定した公開表記になる。
    #[test]
    fn unit_names_are_stable_public_labels() {
        let def = crate::layout::registry::lookup(ActivityId::MEMORY).unwrap();
        let col = def
            .columns
            .iter()
            .find(|c| c.public_name == "kbmemfree")
            .unwrap();
        assert_eq!(unit_name(col), "kB");
    }

    /// センサ値の生値はビット列ではなく実数として出す。
    #[test]
    fn sensor_raw_values_are_decoded_doubles() {
        let def = crate::layout::registry::lookup(ActivityId::PWR_FAN).unwrap();
        let rpm = def.columns.iter().find(|c| c.public_name == "rpm").unwrap();
        assert!(is_double_field(rpm), "rpm は double 保存");

        let user = crate::layout::registry::lookup(ActivityId::CPU)
            .unwrap()
            .columns
            .iter()
            .find(|c| c.public_name == "user")
            .unwrap();
        assert!(!is_double_field(user), "CPU tick は整数");
    }

    /// 起動区間は RESTART ごとに 1 増える。
    #[test]
    fn boot_counter_advances_on_restart() {
        let mut b = BootCounter::default();
        assert_eq!(b.get(), 0);
        b.advance(&[RecordEvent::Restart {
            ust_time: 0,
            hour: 0,
            minute: 0,
            second: 0,
            cpu_count: Some(2),
        }]);
        assert_eq!(b.get(), 1);
        b.advance(&[RecordEvent::Comment {
            ust_time: 0,
            hour: 0,
            minute: 0,
            second: 0,
            text: String::new(),
        }]);
        assert_eq!(b.get(), 1, "COMMENT では増えない");
    }

    /// **`A_IRQ` が独自出力から消えない。**
    ///
    /// `A_IRQ` は「行 = 割り込み、列 = CPU」の行列型で、ファイル上の並びは
    /// その転置 (行 = CPU / 列 = 割り込み、02 §6.1)。`sadf` は専用経路で
    /// 扱うためアイテム識別子を持たない (`ItemKind::Irq`) が、独自出力は
    /// [`ActivityPair::output_items`] をそのまま並べるので、
    /// 行の識別子 (= 割り込み名) をここで当てる必要がある。
    /// 当てなければ `item` 列が全行 `-` になり割り込みを区別できない。
    #[test]
    fn irq_rows_are_identified_by_interrupt_name() {
        use crate::layout::plan::DecodePlan;
        use crate::model::Availability;
        use crate::series::{ActivitySnapshot, ItemSnapshot};

        let def = crate::layout::registry::lookup(ActivityId::IRQ).unwrap();
        let rev = def.latest().unwrap();
        let enc = crate::format::abi::SourceEncoding::new(
            crate::format::abi::Endian::Little,
            crate::format::abi::LayoutAbi::LP64,
        );
        // ファイル上は 行 = CPU (all + cpu0) × 列 = 割り込み (sum + eth0-tx)
        let plan = DecodePlan::build(def, rev, rev.size_lp64, 2, 2, &enc).unwrap();
        let sp = spec::lookup(ActivityId::IRQ).unwrap();
        let cfg = CustomConfig::default();

        // 割り込み名は CPU 行 0 にしか入らない (02 §6.2)
        let cell = |name: Option<&str>, count: u64| ItemSnapshot {
            key: name.map(|k| k.into()),
            texts: vec![name.map(|k| k.into())],
            values: plan
                .fields
                .iter()
                .map(|f| Availability::Present(if f.name == "irq_nr" { count } else { 0 }))
                .collect(),
        };
        let snapshot = |scale: u64| ActivitySnapshot {
            id: ActivityId::IRQ,
            index: 0,
            nr: 2,
            nr2: 2,
            items: vec![
                // CPU all
                cell(Some("sum"), 300 * scale),
                cell(Some("eth0-tx"), 100 * scale),
                // cpu0 (名前は入らない)
                cell(None, 200 * scale),
                cell(None, 40 * scale),
            ],
        };
        let (prev, curr) = (snapshot(1), snapshot(2));
        let pair = ActivityPair {
            id: ActivityId::IRQ,
            def,
            plan: &plan,
            curr: &curr,
            prev: Some(&prev),
            itv_cs: 100,
            has_prev: true,
            continuous: true,
        };

        let items = pair.output_items();
        assert_eq!(items.len(), 2, "出力は割り込みごとの 1 行 (nr2 行)");
        let rows: Vec<ItemOut> = items.iter().map(|it| item_out(def, sp, it, &cfg)).collect();

        // 行の識別子は割り込み名 (`-` にしない)
        assert_eq!(rows[0].item, "sum");
        assert_eq!(rows[1].item, "eth0-tx");
        // 割り込み名は列としても出る (文字列フィールドなので `text`)
        let name_of = |r: &ItemOut| {
            r.rates
                .iter()
                .find(|f| f.name == "intr_name")
                .unwrap()
                .text
                .clone()
        };
        assert_eq!(name_of(&rows[1]).as_deref(), Some("eth0-tx"));
        // 値は行の代表スロット = CPU "all" のレート (1 秒で 100 回)
        let count_of = |r: &ItemOut| -> FieldOut {
            r.rates
                .iter()
                .find(|f| f.name == "intr")
                .expect("intr 列")
                .clone()
        };
        assert_eq!(count_of(&rows[1]).value, Some(100.0));
        assert_eq!(count_of(&rows[1]).quality, Quality::Ok);
        assert_eq!(
            count_of(&rows[0]).value,
            Some(300.0),
            "sum 列は全割り込みの和"
        );
        let expanded = custom_items(
            &pair,
            &CustomConfig {
                irq_cpus: true,
                ..cfg
            },
        );
        assert_eq!(expanded.len(), 4);
        assert_eq!(expanded[2].item, "eth0-tx");
        assert_eq!(expanded[2].cpu.as_deref(), Some("all"));
        assert_eq!(expanded[3].item, "eth0-tx");
        assert_eq!(expanded[3].cpu.as_deref(), Some("0"));
        assert_eq!(count_of(&expanded[3]).value, Some(40.0));
    }
}

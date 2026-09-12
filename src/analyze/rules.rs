//! ボトルネック判定 — **断定しない判定**。
//!
//! `docs/design.md` §11 の契約に従う。
//!
//! - 各判定に**安定したルール ID** (`cpu-saturation` など) を付ける
//! - **観測根拠**を添える (どの指標がどの区間でどの値だったか)
//! - 閾値・継続時間・必要指標・欠損時の扱い・ルール版を出力に保存する
//! - 必要な指標が欠けていれば [`Verdict::Undetermined`] を返す。
//!   **欠損を 0 と見なして判定しない**
//!
//! # 観測事実と解釈を分ける
//!
//! 「`%iowait` が高い」は観測事実であり、「ストレージ障害」は解釈の 1 つに過ぎない。
//! `%iowait` は I/O 待ちで暇な CPU 時間の割合なので、
//! 正常な大量 I/O でも、CPU が空いているだけでも上がる。
//! そこで [`Finding`] は次を分けて持つ。
//!
//! | 項目 | 内容 |
//! |---|---|
//! | [`Finding::observation`] | 観測された事実 (指標・閾値・継続時間) |
//! | [`Finding::evidence`] | 根拠となった区間と値 |
//! | [`Finding::possible_interpretations`] | 考えられる解釈 (複数。どれとも断定しない) |
//! | [`Finding::not_established`] | この判定では確かめていないこと |

use serde::Serialize;

use crate::model::{ActivityId, Unit, ValueKind};

use super::summary::NativePeriodSummary;
use super::timeline::{Coverage, MetricKey, MetricRef, MetricTimeline, Run, Timelines};

/// ルールセットの版。閾値や判定手順を変えたら上げる。
pub const RULESET_VERSION: &str = "resarch-rules/1";

/// item を問わないことを表す item ラベル。
pub const ANY_ITEM: &str = "*";

// ===========================================================================
// 判定結果の型
// ===========================================================================

/// 判定の結論。**原因の断定ではない。**
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// 閾値と継続時間の条件を満たす区間が観測された。
    ///
    /// 「その症状が観測された」だけを意味し、原因を特定したわけではない。
    Observed,
    /// 指標は揃っていたが、条件を満たす区間は無かった。
    NotObserved,
    /// 必要な指標が無い / 欠損しているため判定できない。
    Undetermined,
    /// 判定の前提が成り立たない (スワップ未構成など)。
    NotApplicable,
}

/// 判定できなかった / 非該当の理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerdictReason {
    /// 必要な指標がファイルに無い。
    RequiredMetricAbsent,
    /// 指標はあるが有効な観測が無い (全区間が欠損・不連続)。
    NoValidObservation,
    /// 判定に必要な構成情報が無い (CPU 数など)。
    MissingConfiguration,
    /// 対象の機能が構成されていない (スワップ領域が 0 など)。
    FeatureNotConfigured,
}

/// 欠損時の扱い。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MissingPolicy {
    /// 必要な指標が欠けていれば判定不能とする。**0 で代替しない。**
    UndeterminedOnMissing,
}

/// 閾値の比較方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Comparison {
    /// 閾値以上。
    AtLeast,
    /// 閾値以下。
    AtMost,
}

/// 出力に保存する閾値。
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct ThresholdRecord {
    /// 閾値を当てる対象の名前 (派生指標名を含む)。
    pub target: &'static str,
    pub comparison: Comparison,
    pub value: f64,
    pub unit: Unit,
}

/// 根拠。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Evidence {
    /// 指標の表記 (`A_CPU/all/idle` の形)。
    pub metric: String,
    pub unit: Unit,
    /// 条件を満たして連続した区間。
    pub run: Run,
    /// 派生指標の場合、計算元の指標。
    pub derived_from: Vec<String>,
    /// 計算式 (派生指標のみ)。
    pub formula: Option<&'static str>,
}

/// 1 ルールの判定結果。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Finding {
    /// 安定したルール ID。出力契約の一部なので変更しない。
    pub rule_id: &'static str,
    /// ルール単体の版。
    pub rule_version: &'static str,
    /// ルールセットの版。
    pub ruleset_version: &'static str,
    pub title: &'static str,
    pub verdict: Verdict,
    pub reason: Option<VerdictReason>,
    /// 適用した閾値。
    pub thresholds: Vec<ThresholdRecord>,
    /// 条件の継続時間の下限 (1/100 秒)。
    pub min_duration_cs: u64,
    /// 判定に必要な指標。
    pub required_metrics: Vec<String>,
    /// そのうち欠けていた指標。
    pub missing_metrics: Vec<String>,
    pub missing_policy: MissingPolicy,
    /// 観測の網羅度。
    pub coverage: Coverage,
    /// 観測された事実 (断定しない書き方にする)。
    pub observation: Option<String>,
    pub evidence: Vec<Evidence>,
    /// 考えられる解釈 (複数)。
    pub possible_interpretations: &'static [&'static str],
    /// この判定では確かめていないこと。
    pub not_established: &'static [&'static str],
}

impl Finding {
    fn base(def: &RuleDef, verdict: Verdict) -> Self {
        Self {
            rule_id: def.id,
            rule_version: def.version,
            ruleset_version: RULESET_VERSION,
            title: def.title,
            verdict,
            reason: None,
            thresholds: def.thresholds.to_vec(),
            min_duration_cs: def.min_duration_cs,
            required_metrics: def.required.iter().map(MetricRef::display).collect(),
            missing_metrics: Vec::new(),
            missing_policy: def.missing_policy,
            coverage: Coverage::default(),
            observation: None,
            evidence: Vec::new(),
            possible_interpretations: def.interpretations,
            not_established: def.not_established,
        }
    }

    fn undetermined(def: &RuleDef, reason: VerdictReason, missing: Vec<String>) -> Self {
        let mut f = Self::base(def, Verdict::Undetermined);
        f.reason = Some(reason);
        f.missing_metrics = missing;
        f
    }

    fn not_applicable(def: &RuleDef, reason: VerdictReason) -> Self {
        let mut f = Self::base(def, Verdict::NotApplicable);
        f.reason = Some(reason);
        f
    }
}

// ===========================================================================
// ルール定義
// ===========================================================================

/// 判定に使う外部情報。
#[derive(Debug, Clone, Copy, Default)]
pub struct RuleContext {
    /// ファイルヘッダの CPU 数。実行中のホストの CPU 数ではない。
    pub cpu_nr: Option<u32>,
}

/// ルール 1 件の宣言。
pub struct RuleDef {
    pub id: &'static str,
    pub version: &'static str,
    pub title: &'static str,
    /// 判定に必要な指標。1 つでも欠ければ判定不能。
    pub required: &'static [MetricRef],
    pub thresholds: &'static [ThresholdRecord],
    pub min_duration_cs: u64,
    pub missing_policy: MissingPolicy,
    pub interpretations: &'static [&'static str],
    pub not_established: &'static [&'static str],
    eval: fn(&RuleDef, &Timelines, &RuleContext) -> Finding,
}

impl std::fmt::Debug for RuleDef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuleDef")
            .field("id", &self.id)
            .field("version", &self.version)
            .field("min_duration_cs", &self.min_duration_cs)
            .finish_non_exhaustive()
    }
}

/// 5 分 (1/100 秒単位)。
const FIVE_MIN_CS: u64 = 5 * 60 * 100;
/// 1 分 (1/100 秒単位)。
const ONE_MIN_CS: u64 = 60 * 100;

const M_CPU_IDLE: MetricRef = MetricRef::new(ActivityId::CPU, "all", "idle");
const M_CPU_IOWAIT: MetricRef = MetricRef::new(ActivityId::CPU, "all", "iowait");
const M_CPU_STEAL: MetricRef = MetricRef::new(ActivityId::CPU, "all", "steal");
const M_MEM_AVAIL: MetricRef = MetricRef::new(ActivityId::MEMORY, "-", "kbavail");
const M_MEM_TOTAL: MetricRef = MetricRef::new(ActivityId::MEMORY, "-", "kbmemtotal");
const M_SWP_FREE: MetricRef = MetricRef::new(ActivityId::MEMORY, "-", "kbswpfree");
const M_SWP_TOTAL: MetricRef = MetricRef::new(ActivityId::MEMORY, "-", "kbswptotal");
const M_PSWPIN: MetricRef = MetricRef::new(ActivityId::SWAP, "-", "pswpin");
const M_PSWPOUT: MetricRef = MetricRef::new(ActivityId::SWAP, "-", "pswpout");
const M_RUNQ: MetricRef = MetricRef::new(ActivityId::QUEUE, "-", "runq_sz");
const M_BLOCKED: MetricRef = MetricRef::new(ActivityId::QUEUE, "-", "blocked");

/// 組み込みルール。
///
/// 順序は出力の順序になる (決定的にするため固定)。
pub static RULES: &[RuleDef] = &[
    RuleDef {
        id: "cpu-saturation",
        version: "1",
        title: "CPU の空き時間が少ない状態が継続した",
        required: &[M_CPU_IDLE],
        thresholds: &[ThresholdRecord {
            target: "cpu_busy (100 - %idle)",
            comparison: Comparison::AtLeast,
            value: 90.0,
            unit: Unit::Percent,
        }],
        min_duration_cs: FIVE_MIN_CS,
        missing_policy: MissingPolicy::UndeterminedOnMissing,
        interpretations: &[
            "CPU 能力が要求に対して不足している",
            "単一のプロセスが CPU を占有している",
            "意図的に CPU を使い切るバッチ処理が動いていた",
        ],
        not_established: &[
            "どのプロセスが CPU を使っていたか (sa ファイルにプロセス別の内訳は無い)",
            "処理が遅延したかどうか (応答時間は観測していない)",
        ],
        eval: eval_cpu_saturation,
    },
    RuleDef {
        id: "cpu-iowait-elevated",
        version: "1",
        title: "%iowait が高い状態が継続した",
        required: &[M_CPU_IOWAIT],
        thresholds: &[ThresholdRecord {
            target: "%iowait",
            comparison: Comparison::AtLeast,
            value: 20.0,
            unit: Unit::Percent,
        }],
        min_duration_cs: FIVE_MIN_CS,
        missing_policy: MissingPolicy::UndeterminedOnMissing,
        interpretations: &[
            "ストレージの応答が遅い",
            "I/O 要求が多い (正常な負荷でも上がる)",
            "CPU が空いているため待ち時間が相対的に大きく見えている",
        ],
        not_established: &[
            "ストレージ障害の有無 (%iowait だけでは判定できない。デバイス別の await / %util と併せて見る必要がある)",
            "どのデバイスが待たされていたか",
        ],
        eval: eval_cpu_iowait,
    },
    RuleDef {
        id: "cpu-steal-elevated",
        version: "1",
        title: "%steal が高い状態が継続した",
        required: &[M_CPU_STEAL],
        thresholds: &[ThresholdRecord {
            target: "%steal",
            comparison: Comparison::AtLeast,
            value: 10.0,
            unit: Unit::Percent,
        }],
        min_duration_cs: FIVE_MIN_CS,
        missing_policy: MissingPolicy::UndeterminedOnMissing,
        interpretations: &[
            "同一ハイパーバイザ上の他ゲストと CPU を競合している",
            "CPU クォータによる制限を受けている",
        ],
        not_established: &["ホスト側の構成・他ゲストの負荷 (ゲスト内の統計からは見えない)"],
        eval: eval_cpu_steal,
    },
    RuleDef {
        id: "memory-available-low",
        version: "1",
        title: "利用可能メモリが少ない状態が継続した",
        required: &[M_MEM_AVAIL, M_MEM_TOTAL],
        thresholds: &[ThresholdRecord {
            target: "kbavail / kbmemtotal",
            comparison: Comparison::AtMost,
            value: 5.0,
            unit: Unit::Percent,
        }],
        min_duration_cs: FIVE_MIN_CS,
        missing_policy: MissingPolicy::UndeterminedOnMissing,
        interpretations: &[
            "メモリ要求が搭載量に対して大きい",
            "回収できないページ (tmpfs・カーネルスラブなど) が増えている",
        ],
        not_established: &[
            "OOM Killer が動いたか (sa ファイルに記録は無い)",
            "kbmemfree が小さいこと自体は異常ではない (ページキャッシュは回収可能)",
        ],
        eval: eval_memory_available,
    },
    RuleDef {
        id: "swap-activity",
        version: "1",
        title: "スワップの入出力が継続して発生した",
        required: &[M_PSWPIN, M_PSWPOUT],
        thresholds: &[ThresholdRecord {
            target: "pswpin/s + pswpout/s",
            comparison: Comparison::AtLeast,
            value: 1.0,
            unit: Unit::CountPerSec,
        }],
        min_duration_cs: ONE_MIN_CS,
        missing_policy: MissingPolicy::UndeterminedOnMissing,
        interpretations: &[
            "メモリ不足でページの追い出しが起きている",
            "長時間使われないページを退避しているだけ (swappiness による通常動作)",
        ],
        not_established: &[
            "スワップ発生が性能低下を招いたか (遅延は観測していない)",
            "どのプロセスのページが退避されたか",
        ],
        eval: eval_swap_activity,
    },
    RuleDef {
        id: "swap-space-used-high",
        version: "1",
        title: "スワップ領域の使用率が高い状態が継続した",
        required: &[M_SWP_FREE, M_SWP_TOTAL],
        thresholds: &[ThresholdRecord {
            target: "(kbswptotal - kbswpfree) / kbswptotal",
            comparison: Comparison::AtLeast,
            value: 50.0,
            unit: Unit::Percent,
        }],
        min_duration_cs: FIVE_MIN_CS,
        missing_policy: MissingPolicy::UndeterminedOnMissing,
        interpretations: &[
            "過去にメモリ不足があり、退避したページが残っている",
            "現在もメモリが不足している",
        ],
        not_established: &[
            "使用率が高いこと自体は現在のメモリ不足を意味しない (退避済みページは読み戻されるまで残る)",
        ],
        eval: eval_swap_space,
    },
    RuleDef {
        id: "run-queue-saturation",
        version: "1",
        title: "実行待ちタスクが CPU 数に対して多い状態が継続した",
        required: &[M_RUNQ],
        thresholds: &[ThresholdRecord {
            target: "runq-sz / cpu_nr",
            comparison: Comparison::AtLeast,
            value: 2.0,
            unit: Unit::None,
        }],
        min_duration_cs: FIVE_MIN_CS,
        missing_policy: MissingPolicy::UndeterminedOnMissing,
        interpretations: &[
            "CPU 数に対して実行可能タスクが多い",
            "短時間に大量のタスクが投入された",
        ],
        not_established: &[
            "待ち時間の長さ (キュー長からは算出できない)",
            "cpu_nr はファイルヘッダの申告値であり、観測時点のオンライン CPU 数とは異なる場合がある",
        ],
        eval: eval_run_queue,
    },
    RuleDef {
        id: "blocked-processes-sustained",
        version: "1",
        title: "I/O 待ちでブロックされたタスクが継続して存在した",
        required: &[M_BLOCKED],
        thresholds: &[ThresholdRecord {
            target: "blocked",
            comparison: Comparison::AtLeast,
            value: 1.0,
            unit: Unit::None,
        }],
        min_duration_cs: FIVE_MIN_CS,
        missing_policy: MissingPolicy::UndeterminedOnMissing,
        interpretations: &[
            "I/O の完了待ちが常に存在する",
            "ネットワークストレージの応答待ちが続いている",
        ],
        not_established: &["待たされていたデバイス (blocked にデバイスの内訳は無い)"],
        eval: eval_blocked,
    },
];

/// 組み込みルールが必要とする指標か (時系列の保持判断に使う)。
///
/// **ルールが宣言した item だけを保持する。** 組み込みルールは CPU の `all` 行しか
/// 見ないので、per-CPU の列まで抱えるとメモリが item 数に比例して増える
/// (128 CPU × 31 日分を保持すると数十 MB になる)。
/// item を問わず保持したいルールは item に [`ANY_ITEM`] を宣言する。
pub fn is_rule_input(key: &MetricKey) -> bool {
    RULES.iter().flat_map(|r| r.required.iter()).any(|m| {
        m.activity == key.activity
            && m.column == key.column
            && (m.item == ANY_ITEM || m.item == key.item)
    })
}

/// 組み込みルールが必要とする指標の一覧。
pub fn rule_inputs() -> Vec<MetricRef> {
    let mut v: Vec<MetricRef> = Vec::new();
    for m in RULES.iter().flat_map(|r| r.required.iter()) {
        if !v.contains(m) {
            v.push(*m);
        }
    }
    v
}

/// サマリに対して全ルールを評価する。
///
/// `ctx.cpu_nr` が未設定なら、サマリの出自 (ファイルヘッダ由来) から補う。
pub fn evaluate(summary: &NativePeriodSummary, ctx: &RuleContext) -> Vec<Finding> {
    let ctx = RuleContext {
        cpu_nr: ctx.cpu_nr.or(summary.source.cpu_nr),
    };
    evaluate_timelines(&summary.timelines, &ctx)
}

/// 時系列に対して全ルールを評価する。
pub fn evaluate_timelines(timelines: &Timelines, ctx: &RuleContext) -> Vec<Finding> {
    RULES.iter().map(|r| (r.eval)(r, timelines, ctx)).collect()
}

// ===========================================================================
// 派生指標の組み立て
// ===========================================================================

/// `base - value` の指標を作る (`100 - %idle` など)。
fn offset_from(t: &MetricTimeline, base: f64, column: &str) -> MetricTimeline {
    let mut out = MetricTimeline::new(
        MetricKey::new(t.key.activity, t.key.item.clone(), column),
        t.unit,
        t.kind,
    );
    for p in &t.points {
        let mut q = *p;
        q.value = p.value.map(|v| base - v);
        out.push(q);
    }
    out
}

/// 2 つの指標の比 (`numer / denom * scale`) を作る。
///
/// - 区間の始点・終点が一致しない点は対応付けない (別の時刻の値を混ぜない)
/// - どちらかが欠損している点は**欠損**として残す (0 とみなさない)
/// - 分母が 0 の点は欠損として残す
fn ratio_of(
    numer: &MetricTimeline,
    denom: &MetricTimeline,
    scale: f64,
    column: &str,
    unit: Unit,
) -> MetricTimeline {
    let mut out = MetricTimeline::new(
        MetricKey::new(numer.key.activity, numer.key.item.clone(), column),
        unit,
        ValueKind::Gauge,
    );
    for (a, b) in numer.points.iter().zip(denom.points.iter()) {
        if a.start_ust != b.start_ust || a.end_ust != b.end_ust {
            continue;
        }
        let mut q = *a;
        q.value = match (a.value, b.value) {
            (Some(n), Some(d)) if d != 0.0 => Some(n / d * scale),
            _ => None,
        };
        if q.value.is_none() && q.reason.is_none() {
            // 分子は取れていたので、欠損の理由は分母側にある
            q.reason = Some(match b.value {
                // 分母が 0 (総量が申告されていない等)
                Some(_) => super::timeline::ExclusionReason::ZeroDenominator,
                None => b
                    .reason
                    .unwrap_or(super::timeline::ExclusionReason::MissingInSample),
            });
        }
        out.push(q);
    }
    out
}

/// 2 つの指標の和を作る。どちらかが欠損している点は欠損のまま。
fn sum_of(a: &MetricTimeline, b: &MetricTimeline, column: &str) -> MetricTimeline {
    let mut out = MetricTimeline::new(
        MetricKey::new(a.key.activity, a.key.item.clone(), column),
        a.unit,
        a.kind,
    );
    for (x, y) in a.points.iter().zip(b.points.iter()) {
        if x.start_ust != y.start_ust || x.end_ust != y.end_ust {
            continue;
        }
        let mut q = *x;
        q.value = match (x.value, y.value) {
            (Some(m), Some(n)) => Some(m + n),
            _ => None,
        };
        out.push(q);
    }
    out
}

/// 定数で割った指標を作る (`runq-sz / cpu_nr` など)。
fn scaled_by(t: &MetricTimeline, divisor: f64, column: &str) -> MetricTimeline {
    let mut out = MetricTimeline::new(
        MetricKey::new(t.key.activity, t.key.item.clone(), column),
        t.unit,
        t.kind,
    );
    for p in &t.points {
        let mut q = *p;
        q.value = p.value.map(|v| v / divisor);
        out.push(q);
    }
    out
}

// ===========================================================================
// 判定の共通処理
// ===========================================================================

/// 閾値と継続時間の検査を行い、[`Finding`] を組み立てる。
fn judge(
    def: &RuleDef,
    metric: &MetricTimeline,
    derived_from: Vec<String>,
    formula: Option<&'static str>,
    ctx_note: Option<String>,
) -> Finding {
    let threshold = def.thresholds[0];
    let coverage = metric.coverage();
    if !coverage.has_observation() {
        let mut f = Finding::undetermined(
            def,
            VerdictReason::NoValidObservation,
            vec![metric.key.display()],
        );
        f.coverage = coverage;
        return f;
    }

    let run = match threshold.comparison {
        Comparison::AtLeast => metric.longest_run_at_least(threshold.value),
        Comparison::AtMost => metric.longest_run_at_most(threshold.value),
    };

    let mut f = Finding::base(def, Verdict::NotObserved);
    f.coverage = coverage;

    match run {
        Some(r) if r.duration_cs >= def.min_duration_cs => {
            f.verdict = Verdict::Observed;
            let cmp = match threshold.comparison {
                Comparison::AtLeast => "以上",
                Comparison::AtMost => "以下",
            };
            let mut obs = format!(
                "{} が {}{} {}の状態が {:.0} 秒続いた (区間 {} 本、最小 {:.2} / 最大 {:.2} / 平均 {:.2})",
                threshold.target,
                threshold.value,
                threshold.unit.suffix(),
                cmp,
                r.duration_secs(),
                r.intervals,
                r.min,
                r.max,
                r.mean,
            );
            if let Some(note) = &ctx_note {
                obs.push_str(&format!(" / {note}"));
            }
            f.observation = Some(obs);
            f.evidence.push(Evidence {
                metric: metric.key.display(),
                unit: metric.unit,
                run: r,
                derived_from,
                formula,
            });
        }
        Some(r) => {
            // 閾値は超えたが継続時間が足りない → 観測事実として根拠だけ残す
            f.evidence.push(Evidence {
                metric: metric.key.display(),
                unit: metric.unit,
                run: r,
                derived_from,
                formula,
            });
            f.observation = Some(format!(
                "閾値を満たす区間はあったが継続時間が {:.0} 秒で下限 {:.0} 秒に届かなかった",
                r.duration_secs(),
                def.min_duration_cs as f64 / 100.0
            ));
        }
        None => {}
    }
    f
}

/// 必要指標のうち欠けているものを列挙する。
fn missing_metrics(refs: &[MetricRef], timelines: &Timelines) -> Vec<String> {
    refs.iter()
        .filter(|m| timelines.get_ref(m).is_none())
        .map(MetricRef::display)
        .collect()
}

// ===========================================================================
// 各ルールの評価
// ===========================================================================

fn eval_cpu_saturation(def: &RuleDef, t: &Timelines, _ctx: &RuleContext) -> Finding {
    let Some(idle) = t.get_ref(&M_CPU_IDLE) else {
        return Finding::undetermined(
            def,
            VerdictReason::RequiredMetricAbsent,
            missing_metrics(def.required, t),
        );
    };
    // %idle から使用率を作る。%idle の欠損は使用率の欠損として伝わる。
    let busy = offset_from(idle, 100.0, "cpu_busy");
    judge(
        def,
        &busy,
        vec![M_CPU_IDLE.display()],
        Some("100 - %idle"),
        None,
    )
}

fn eval_cpu_iowait(def: &RuleDef, t: &Timelines, _ctx: &RuleContext) -> Finding {
    let Some(iowait) = t.get_ref(&M_CPU_IOWAIT) else {
        return Finding::undetermined(
            def,
            VerdictReason::RequiredMetricAbsent,
            missing_metrics(def.required, t),
        );
    };
    judge(def, iowait, Vec::new(), None, None)
}

fn eval_cpu_steal(def: &RuleDef, t: &Timelines, _ctx: &RuleContext) -> Finding {
    let Some(steal) = t.get_ref(&M_CPU_STEAL) else {
        return Finding::undetermined(
            def,
            VerdictReason::RequiredMetricAbsent,
            missing_metrics(def.required, t),
        );
    };
    judge(def, steal, Vec::new(), None, None)
}

fn eval_memory_available(def: &RuleDef, t: &Timelines, _ctx: &RuleContext) -> Finding {
    let (Some(avail), Some(total)) = (t.get_ref(&M_MEM_AVAIL), t.get_ref(&M_MEM_TOTAL)) else {
        // kbavail は sysstat 12.x 以降のフィールド。無い世代では
        // kbmemfree で代替しない (ページキャッシュを含まないので過大に危険側へ振れる)。
        return Finding::undetermined(
            def,
            VerdictReason::RequiredMetricAbsent,
            missing_metrics(def.required, t),
        );
    };
    let pct = ratio_of(avail, total, 100.0, "available_pct", Unit::Percent);
    judge(
        def,
        &pct,
        vec![M_MEM_AVAIL.display(), M_MEM_TOTAL.display()],
        Some("kbavail / kbmemtotal * 100"),
        None,
    )
}

fn eval_swap_activity(def: &RuleDef, t: &Timelines, _ctx: &RuleContext) -> Finding {
    let (Some(pin), Some(pout)) = (t.get_ref(&M_PSWPIN), t.get_ref(&M_PSWPOUT)) else {
        return Finding::undetermined(
            def,
            VerdictReason::RequiredMetricAbsent,
            missing_metrics(def.required, t),
        );
    };
    let total = sum_of(pin, pout, "pswp_total");
    judge(
        def,
        &total,
        vec![M_PSWPIN.display(), M_PSWPOUT.display()],
        Some("pswpin/s + pswpout/s"),
        None,
    )
}

fn eval_swap_space(def: &RuleDef, t: &Timelines, _ctx: &RuleContext) -> Finding {
    let (Some(free), Some(total)) = (t.get_ref(&M_SWP_FREE), t.get_ref(&M_SWP_TOTAL)) else {
        return Finding::undetermined(
            def,
            VerdictReason::RequiredMetricAbsent,
            missing_metrics(def.required, t),
        );
    };
    // スワップ未構成 (総量が常に 0) は「使用率 0%」ではなく非該当。
    let configured = total
        .points
        .iter()
        .any(|p| p.value.is_some_and(|v| v > 0.0));
    if !configured {
        let mut f = Finding::not_applicable(def, VerdictReason::FeatureNotConfigured);
        f.coverage = total.coverage();
        f.observation = Some("スワップ領域が構成されていない (kbswptotal が 0)".to_string());
        return f;
    }
    // used = total - free
    let mut used = MetricTimeline::new(
        MetricKey::new(total.key.activity, total.key.item.clone(), "kbswpused"),
        Unit::Kilobytes,
        ValueKind::Gauge,
    );
    for (tp, fp) in total.points.iter().zip(free.points.iter()) {
        if tp.start_ust != fp.start_ust || tp.end_ust != fp.end_ust {
            continue;
        }
        let mut q = *tp;
        q.value = match (tp.value, fp.value) {
            (Some(t), Some(f)) => Some(t - f),
            _ => None,
        };
        used.push(q);
    }
    let pct = ratio_of(&used, total, 100.0, "swap_used_pct", Unit::Percent);
    judge(
        def,
        &pct,
        vec![M_SWP_FREE.display(), M_SWP_TOTAL.display()],
        Some("(kbswptotal - kbswpfree) / kbswptotal * 100"),
        None,
    )
}

fn eval_run_queue(def: &RuleDef, t: &Timelines, ctx: &RuleContext) -> Finding {
    let Some(runq) = t.get_ref(&M_RUNQ) else {
        return Finding::undetermined(
            def,
            VerdictReason::RequiredMetricAbsent,
            missing_metrics(def.required, t),
        );
    };
    // CPU 数が分からなければ「1 CPU と仮定」せずに判定不能とする。
    let Some(cpu_nr) = ctx.cpu_nr.filter(|n| *n > 0) else {
        let mut f = Finding::undetermined(def, VerdictReason::MissingConfiguration, Vec::new());
        f.coverage = runq.coverage();
        f.observation =
            Some("CPU 数が分からないため 1 CPU あたりの実行待ち数を出せない".to_string());
        return f;
    };
    let per_cpu = scaled_by(runq, f64::from(cpu_nr), "runq_per_cpu");
    judge(
        def,
        &per_cpu,
        vec![M_RUNQ.display()],
        Some("runq-sz / cpu_nr"),
        Some(format!("cpu_nr = {cpu_nr} (ファイルヘッダの申告値)")),
    )
}

fn eval_blocked(def: &RuleDef, t: &Timelines, _ctx: &RuleContext) -> Finding {
    let Some(blocked) = t.get_ref(&M_BLOCKED) else {
        return Finding::undetermined(
            def,
            VerdictReason::RequiredMetricAbsent,
            missing_metrics(def.required, t),
        );
    };
    judge(def, blocked, Vec::new(), None, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze::timeline::{ExclusionReason, MetricPoint};

    /// 1 区間 10 秒として時系列を組む。
    fn build(r: &MetricRef, values: &[Option<f64>]) -> MetricTimeline {
        let mut t = MetricTimeline::new(r.key(), Unit::Percent, ValueKind::Counter);
        for (i, v) in values.iter().enumerate() {
            let start = i as u64 * 10;
            let p = match v {
                Some(v) => MetricPoint::observed(start, start + 10, 1000, *v),
                None => {
                    MetricPoint::missing(start, start + 10, 1000, ExclusionReason::MissingInSample)
                }
            };
            t.push(p);
        }
        t
    }

    fn timelines(entries: Vec<MetricTimeline>) -> Timelines {
        let mut ts = Timelines::new();
        for e in entries {
            let t = ts.entry(e.key.clone(), e.unit, e.kind);
            for p in e.points {
                t.push(p);
            }
        }
        ts
    }

    fn find<'a>(fs: &'a [Finding], id: &str) -> &'a Finding {
        fs.iter().find(|f| f.rule_id == id).expect("ルールが無い")
    }

    /// 全ルールが ID と版を持ち、重複が無い。
    #[test]
    fn every_rule_has_a_stable_id_and_version() {
        let mut ids: Vec<&str> = Vec::new();
        for r in RULES {
            assert!(!r.id.is_empty());
            assert!(!r.version.is_empty());
            assert!(!r.required.is_empty(), "{}: 必要指標が空", r.id);
            assert!(!r.thresholds.is_empty(), "{}: 閾値が空", r.id);
            assert!(
                !r.interpretations.is_empty(),
                "{}: 解釈の候補が空 (断定を避けるため必須)",
                r.id
            );
            assert!(!ids.contains(&r.id), "ルール ID {} が重複", r.id);
            ids.push(r.id);
        }
        assert!(RULES.len() >= 4, "主要な 4 系統を満たす");
    }

    /// 指標が無ければ判定不能。`NotObserved` (問題なし) にしない。
    #[test]
    fn missing_metric_yields_undetermined_not_negative() {
        let fs = evaluate_timelines(&Timelines::new(), &RuleContext::default());
        assert_eq!(fs.len(), RULES.len());
        for f in &fs {
            assert_eq!(
                f.verdict,
                Verdict::Undetermined,
                "{}: 指標が無いのに断定している",
                f.rule_id
            );
            assert_eq!(f.reason, Some(VerdictReason::RequiredMetricAbsent));
            assert!(!f.missing_metrics.is_empty());
        }
    }

    /// 全区間が欠損なら判定不能 (0 として「正常」と判定しない)。
    #[test]
    fn all_missing_observations_yield_undetermined() {
        let ts = timelines(vec![build(&M_CPU_IDLE, &[None, None, None])]);
        let fs = evaluate_timelines(&ts, &RuleContext::default());
        let f = find(&fs, "cpu-saturation");
        assert_eq!(f.verdict, Verdict::Undetermined);
        assert_eq!(f.reason, Some(VerdictReason::NoValidObservation));
        assert_eq!(f.coverage.missing, 3);
        assert_eq!(f.coverage.observed, 0);
    }

    /// CPU 飽和は %idle から使用率を作って判定し、根拠に区間が入る。
    #[test]
    fn cpu_saturation_is_observed_with_evidence() {
        // %idle = 2% が 40 区間 (400 秒) 続く → cpu_busy = 98%
        let idle: Vec<Option<f64>> = (0..40).map(|_| Some(2.0)).collect();
        let ts = timelines(vec![build(&M_CPU_IDLE, &idle)]);
        let fs = evaluate_timelines(&ts, &RuleContext::default());
        let f = find(&fs, "cpu-saturation");
        assert_eq!(f.verdict, Verdict::Observed);
        assert_eq!(f.evidence.len(), 1);
        let e = &f.evidence[0];
        assert_eq!(e.formula, Some("100 - %idle"));
        assert_eq!(e.run.duration_cs, 40_000);
        assert!((e.run.mean - 98.0).abs() < 1e-9);
        assert_eq!(f.rule_version, "1");
        assert_eq!(f.ruleset_version, RULESET_VERSION);
        assert_eq!(f.min_duration_cs, FIVE_MIN_CS);
        assert!(f.observation.is_some());
        // 解釈は複数示し、断定しない
        assert!(f.possible_interpretations.len() >= 2);
        assert!(!f.not_established.is_empty());
    }

    /// 継続時間が足りなければ `Observed` にしない。
    #[test]
    fn short_spike_is_not_observed() {
        // 100% だが 3 区間 (30 秒) しか続かない
        let idle = vec![Some(0.0), Some(0.0), Some(0.0), Some(90.0)];
        let ts = timelines(vec![build(&M_CPU_IDLE, &idle)]);
        let fs = evaluate_timelines(&ts, &RuleContext::default());
        let f = find(&fs, "cpu-saturation");
        assert_eq!(f.verdict, Verdict::NotObserved);
        assert_eq!(f.evidence.len(), 1, "根拠は残す");
        assert_eq!(f.evidence[0].run.duration_cs, 3_000);
    }

    /// 欠損が挟まると継続時間は繋がらない (欠損を「閾値超過の継続」と見なさない)。
    #[test]
    fn missing_interval_breaks_sustained_condition() {
        let mut idle: Vec<Option<f64>> = (0..20).map(|_| Some(1.0)).collect();
        idle.push(None);
        idle.extend((0..20).map(|_| Some(1.0)));
        let ts = timelines(vec![build(&M_CPU_IDLE, &idle)]);
        let fs = evaluate_timelines(&ts, &RuleContext::default());
        let f = find(&fs, "cpu-saturation");
        // 前半 20 区間 = 200 秒 < 300 秒なので観測に至らない
        assert_eq!(f.verdict, Verdict::NotObserved);
        assert_eq!(f.evidence[0].run.duration_cs, 20_000);
    }

    /// iowait のルールは「ストレージ障害」と断定しない。
    #[test]
    fn iowait_rule_separates_observation_from_interpretation() {
        let iowait: Vec<Option<f64>> = (0..40).map(|_| Some(55.0)).collect();
        let ts = timelines(vec![build(&M_CPU_IOWAIT, &iowait)]);
        let fs = evaluate_timelines(&ts, &RuleContext::default());
        let f = find(&fs, "cpu-iowait-elevated");
        assert_eq!(f.verdict, Verdict::Observed);
        assert!(f.possible_interpretations.len() >= 3);
        assert!(
            f.not_established
                .iter()
                .any(|s| s.contains("ストレージ障害")),
            "障害の有無を確かめていないことを明示する"
        );
        let json = serde_json::to_string(f).unwrap();
        assert!(json.contains(r#""verdict":"observed""#), "{json}");
    }

    /// メモリ判定は kbavail が無い世代で判定不能になる (kbmemfree で代替しない)。
    #[test]
    fn memory_rule_does_not_substitute_kbmemfree() {
        let ts = timelines(vec![MetricTimeline::new(
            M_MEM_TOTAL.key(),
            Unit::Kilobytes,
            ValueKind::Gauge,
        )]);
        let fs = evaluate_timelines(&ts, &RuleContext::default());
        let f = find(&fs, "memory-available-low");
        assert_eq!(f.verdict, Verdict::Undetermined);
        assert!(
            f.missing_metrics.iter().any(|m| m.contains("kbavail")),
            "{:?}",
            f.missing_metrics
        );
    }

    /// メモリ枯渇は比率で判定する。
    #[test]
    fn memory_available_low_uses_ratio() {
        let n = 40;
        let avail: Vec<Option<f64>> = (0..n).map(|_| Some(1_000.0)).collect();
        let total: Vec<Option<f64>> = (0..n).map(|_| Some(100_000.0)).collect();
        let ts = timelines(vec![
            build(&M_MEM_AVAIL, &avail),
            build(&M_MEM_TOTAL, &total),
        ]);
        let fs = evaluate_timelines(&ts, &RuleContext::default());
        let f = find(&fs, "memory-available-low");
        assert_eq!(f.verdict, Verdict::Observed, "1% は閾値 5% 以下");
        assert_eq!(f.evidence[0].derived_from.len(), 2);
        assert!((f.evidence[0].run.mean - 1.0).abs() < 1e-9);
    }

    /// 分母が欠損している区間は比率を作らない (0 として扱わない)。
    #[test]
    fn ratio_skips_intervals_with_missing_denominator() {
        let n = 40;
        let avail: Vec<Option<f64>> = (0..n).map(|_| Some(1_000.0)).collect();
        // 総量が全区間で欠損
        let total: Vec<Option<f64>> = (0..n).map(|_| None).collect();
        let ts = timelines(vec![
            build(&M_MEM_AVAIL, &avail),
            build(&M_MEM_TOTAL, &total),
        ]);
        let fs = evaluate_timelines(&ts, &RuleContext::default());
        let f = find(&fs, "memory-available-low");
        assert_eq!(f.verdict, Verdict::Undetermined);
        assert_eq!(f.reason, Some(VerdictReason::NoValidObservation));
    }

    /// スワップ入出力は 2 指標の和で判定する。
    #[test]
    fn swap_activity_sums_in_and_out() {
        let n = 10;
        let pin: Vec<Option<f64>> = (0..n).map(|_| Some(0.4)).collect();
        let pout: Vec<Option<f64>> = (0..n).map(|_| Some(0.8)).collect();
        let ts = timelines(vec![build(&M_PSWPIN, &pin), build(&M_PSWPOUT, &pout)]);
        let fs = evaluate_timelines(&ts, &RuleContext::default());
        let f = find(&fs, "swap-activity");
        assert_eq!(f.verdict, Verdict::Observed, "0.4 + 0.8 = 1.2 >= 1.0");
        assert!((f.evidence[0].run.mean - 1.2).abs() < 1e-9);
    }

    /// 片方だけ欠けていれば和は作れず判定不能。
    #[test]
    fn swap_activity_requires_both_directions() {
        let pin: Vec<Option<f64>> = (0..10).map(|_| Some(5.0)).collect();
        let ts = timelines(vec![build(&M_PSWPIN, &pin)]);
        let fs = evaluate_timelines(&ts, &RuleContext::default());
        let f = find(&fs, "swap-activity");
        assert_eq!(f.verdict, Verdict::Undetermined);
        assert!(f.missing_metrics.iter().any(|m| m.contains("pswpout")));
    }

    /// スワップ未構成は「使用率 0%」ではなく非該当。
    #[test]
    fn unconfigured_swap_is_not_applicable() {
        let n = 40;
        let free: Vec<Option<f64>> = (0..n).map(|_| Some(0.0)).collect();
        let total: Vec<Option<f64>> = (0..n).map(|_| Some(0.0)).collect();
        let ts = timelines(vec![build(&M_SWP_FREE, &free), build(&M_SWP_TOTAL, &total)]);
        let fs = evaluate_timelines(&ts, &RuleContext::default());
        let f = find(&fs, "swap-space-used-high");
        assert_eq!(f.verdict, Verdict::NotApplicable);
        assert_eq!(f.reason, Some(VerdictReason::FeatureNotConfigured));
    }

    /// CPU 数が分からなければ実行キューの判定はしない (1 CPU と仮定しない)。
    #[test]
    fn run_queue_needs_cpu_count() {
        let runq: Vec<Option<f64>> = (0..40).map(|_| Some(8.0)).collect();
        let ts = timelines(vec![build(&M_RUNQ, &runq)]);

        let f = find(
            &evaluate_timelines(&ts, &RuleContext::default()),
            "run-queue-saturation",
        )
        .clone();
        assert_eq!(f.verdict, Verdict::Undetermined);
        assert_eq!(f.reason, Some(VerdictReason::MissingConfiguration));

        // CPU 数が分かれば判定できる (8 / 2 CPU = 4.0 >= 2.0)
        let fs = evaluate_timelines(&ts, &RuleContext { cpu_nr: Some(2) });
        let f = find(&fs, "run-queue-saturation");
        assert_eq!(f.verdict, Verdict::Observed);
        assert!((f.evidence[0].run.mean - 4.0).abs() < 1e-9);

        // CPU 数が多ければ観測されない (8 / 16 = 0.5)
        let fs = evaluate_timelines(&ts, &RuleContext { cpu_nr: Some(16) });
        let f = find(&fs, "run-queue-saturation");
        assert_eq!(f.verdict, Verdict::NotObserved);
    }

    /// 判定入力の指標は時系列の保持対象として認識される。
    ///
    /// 宣言していない item / 列は保持しない (item 数に比例したメモリを避ける)。
    #[test]
    fn rule_inputs_are_retained_metrics() {
        assert!(is_rule_input(&M_CPU_IDLE.key()));
        assert!(
            !is_rule_input(&MetricKey::new(ActivityId::CPU, "cpu3", "idle")),
            "宣言は all 行のみ"
        );
        assert!(!is_rule_input(&MetricKey::new(
            ActivityId::CPU,
            "all",
            "guest"
        )));
        assert!(rule_inputs().len() >= 8);

        // item に `ANY_ITEM` を宣言したルールは item を問わず保持される
        let any = MetricRef::new(ActivityId::DISK, ANY_ITEM, "util_pct");
        assert_eq!(any.item, ANY_ITEM);
    }

    /// 出力は決定的な順序で返る。
    #[test]
    fn findings_follow_rule_order() {
        let fs = evaluate_timelines(&Timelines::new(), &RuleContext::default());
        let ids: Vec<&str> = fs.iter().map(|f| f.rule_id).collect();
        let expect: Vec<&str> = RULES.iter().map(|r| r.id).collect();
        assert_eq!(ids, expect);
    }
}

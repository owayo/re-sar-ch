//! 何を見るかの表 — 異変検出の対象系列とその扱い。
//!
//! `analyze::rules` のルール表が「症状ごとの判定手順」を持つのに対し、
//! この表は「**系列ごとの性質**」を持つ。どの絶対値に意味があるか、
//! どちら側への逸脱を見るか、どれだけの水準変化を有意とみなすか。
//!
//! # Counter と Gauge を混ぜない
//!
//! Counter (差分を取ってレートにするもの) と Gauge (その時点の量) は
//! 検出の意味が違う。Counter のレートは区間を代表する値で、
//! 区間の内側の変動は観測されていない。Gauge は採取時点の値で、
//! 採取と採取の間の値は観測されていない。
//! [`CatalogEntry::kind`] で種別を持ち、[`crate::detect`] が
//! [`crate::detect::ObservationOrigin`] へ写して経路を分ける。
//!
//! **種別は `layout` 層の宣言と一致していなければならない。**
//! [`tests::catalog_matches_the_layout_registry`] が機械的に検査する。
//!
//! # 固定条件を置く基準
//!
//! 固定条件 ([`FixedCondition`]) は「**文脈なしで判断できる絶対値**」にだけ置く。
//!
//! | 置く | 置かない |
//! |---|---|
//! | `%idle` が 5% 未満 (その区間 CPU がほぼ空いていなかった) | `runq-sz` (CPU 数に依存する) |
//! | `pswpin/s` > 0 (退避ページを読み戻した) | `kbavail` (搭載量に依存する) |
//! | `pgscand/s` > 0 (direct reclaim が起きた) | `%memused` (ページキャッシュを含む) |
//! | `%fsused` が 95% 以上 | `cswch/s` (ワークロード次第) |
//!
//! 絶対値に意味が無い系列は固定条件を持たず、ロバスト逸脱と水準変化だけで見る。
//! **固定条件が無いことは「検出しない」ではない。**

use crate::analyze::assessment::Priority;
use crate::analyze::timeline::{MetricKey, SINGLE_ITEM};
use crate::detect::{FixedComparison, Pattern, ShiftDirection};
use crate::model::{ActivityId, Unit, ValueKind};

/// カタログの版。項目・閾値を変えたら上げる。
pub const CATALOG_VERSION: &str = "resarch-detect-catalog/1";

// ===========================================================================
// item の対象範囲
// ===========================================================================

/// どの item を見るか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemScope {
    /// item を持たない activity (ラベルは `-`)。
    Single,
    /// 集約 item のみ (`A_CPU` の `all` 行)。
    ///
    /// per-CPU まで見ると系列数が CPU 数に比例して増え、
    /// 1 コアの偏りが全体の異変として報告されてしまう。
    Aggregate,
    /// item ごと (デバイス・インターフェース・ファイルシステム)。
    Each,
}

impl ItemScope {
    /// item ラベルがこの範囲に入るか。
    pub fn matches(self, item: &str) -> bool {
        match self {
            ItemScope::Single => item == SINGLE_ITEM,
            ItemScope::Aggregate => item == "all",
            // 集約行は個別 item として数えない (二重計上になる)
            ItemScope::Each => item != "all" && item != "sum" && item != SINGLE_ITEM,
        }
    }

    /// 入力に現れなかった場合に表示する item ラベル。
    pub const fn placeholder(self) -> &'static str {
        match self {
            ItemScope::Single => SINGLE_ITEM,
            ItemScope::Aggregate => "all",
            ItemScope::Each => "*",
        }
    }
}

// ===========================================================================
// 固定条件
// ===========================================================================

/// 意味が確立している絶対値の条件。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FixedCondition {
    /// 安定した条件 ID。**出力契約の一部なので変更しない。**
    pub id: &'static str,
    pub comparison: FixedComparison,
    pub value: f64,
    /// 観測された形。
    pub pattern: Pattern,
    /// 条件を満たした採取が連続して何回あれば 1 件とするか。
    ///
    /// 1 回だけの値で報告してよいのは、発生自体が事象であるもの
    /// (スワップ、direct reclaim、エラーカウンタ) に限る。
    pub min_samples: u32,
    /// この条件が単独で立ったときの調査優先度。
    pub priority: Priority,
    /// **この絶対値に意味がある根拠。** 出力にそのまま載せる。
    pub rationale: &'static str,
}

// ===========================================================================
// ロバスト逸脱・水準変化の扱い
// ===========================================================================

/// 逸脱のどちら側を見るか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviationInterest {
    /// 上へ外れたときだけ。
    Upper,
    /// 下へ外れたときだけ。
    Lower,
    /// 両側。
    Both,
    /// 見ない。
    None,
}

impl DeviationInterest {
    pub fn accepts(self, direction: ShiftDirection) -> bool {
        matches!(
            (self, direction),
            (DeviationInterest::Both, _)
                | (DeviationInterest::Upper, ShiftDirection::Rise)
                | (DeviationInterest::Lower, ShiftDirection::Fall)
        )
    }

    pub const fn is_none(self) -> bool {
        matches!(self, DeviationInterest::None)
    }
}

/// 最小有意変化量 — 「これ以上動いたら意味がある」の宣言。
///
/// 水準変化の判定と、逸脱の**絶対差の下限**の両方に使う。
/// 百分率の指標は絶対差 (`%idle` が 20 ポイント動いた) で書けるが、
/// メモリ量・スループットは搭載量やワークロード規模に依存するため
/// 絶対差では書けない。**指標ごとにどちらで測るかを宣言する。**
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ShiftMagnitude {
    /// 指標の単位での絶対差。
    Absolute(f64),
    /// 中央値に対する相対差と、単位での絶対下限の**大きい方**。
    ///
    /// **相対だけで書いてはいけない。** 中央値が 0 付近の系列では
    /// 相対差が無意味になる。`aqu-sz` の中央値が 0.0002 のとき、
    /// 0.01 への変化は「中央値の 50 倍」になるが、キュー長 0.01 は
    /// 何も起きていないのと同じである。`floor` がその報告を止める。
    Relative {
        /// 中央値に対する割合 (0.5 = 中央値の 50%)。
        fraction: f64,
        /// 単位での絶対下限。**中央値が小さいときはこちらが効く。**
        floor: f64,
    },
    /// 水準変化・絶対差の下限を宣言しない。
    ///
    /// 普段 0 の事象カウンタ (スワップ・エラー) 向け。
    /// 発生自体が事象なので固定条件経路に任せる。
    NotEvaluated,
}

// ===========================================================================
// カタログ項目
// ===========================================================================

/// 1 系列の宣言。
#[derive(Debug, Clone, Copy)]
pub struct CatalogEntry {
    pub activity: ActivityId,
    /// 独自出力での列名 (`layout` の `public_name`)。
    pub column: &'static str,
    pub scope: ItemScope,
    /// 値の性質。**`layout` の宣言と一致すること** (テストで固定)。
    pub kind: ValueKind,
    /// 単位。**`layout` の宣言と一致すること** (テストで固定)。
    pub unit: Unit,
    /// 人間向けの指標名。
    pub label: &'static str,
    /// 固定条件 (0 件でもよい)。
    pub fixed: &'static [FixedCondition],
    pub deviation: DeviationInterest,
    pub shift: ShiftMagnitude,
    /// 考えられる解釈 (複数。どれとも断定しない)。
    pub interpretations: &'static [&'static str],
    /// この系列からは確かめられないこと。
    pub not_established: &'static [&'static str],
}

impl CatalogEntry {
    /// **水準変化**で関心のある方向。
    ///
    /// 逸脱 (`deviation`) とは別の判断である。両者を同じ宣言で済ませると、
    /// **処理量の指標で「止まった」を検出前に捨てる**ことになる
    /// (受信スループットや I/O 転送数は `Upper` 宣言なので、
    /// 一定の転送がゼロへ落ちても水準変化が方向で弾かれていた)。
    ///
    /// 「外れ値として上だけ見たい」と「水準が動いたことを両方向で見たい」は
    /// 別の関心である。
    ///
    /// | 例 | 逸脱 | 水準変化 |
    /// |---|---|---|
    /// | `%idle` | `Lower` (低いほうだけ外れ値) | 両方向 (負荷の増減はどちらも所見) |
    /// | 受信スループット | `Upper` (急増が外れ値) | 両方向 (**停止も所見**) |
    /// | `%util` | `Upper` | 両方向 |
    ///
    /// **未完了**: 現在は逸脱の宣言をそのまま返している (= 従来の挙動)。
    /// 指標ごとに「水準変化として見たい方向」を宣言し直す作業が残っている。
    /// 判断が要るのは次の点で、Issue #5 の 20 で追跡している。
    ///
    /// - 処理量の指標 (スループット / 転送数) は**停止も所見**なので両方向にする
    /// - `%idle` の上昇は「負荷源の消失」で、処理終了か障害かは断定できない。
    ///   水準が動いた事実として出し、解釈は `interpretations` に並べる
    /// - 一方で全指標を無条件に両方向にすると偽陽性が増える。
    ///   指標ごとに「その向きの変化に意味があるか」を決める必要がある
    pub fn shift_interest(&self) -> DeviationInterest {
        self.deviation
    }

    /// この項目が指す系列か。
    pub fn matches(&self, key: &MetricKey) -> bool {
        self.activity == key.activity && self.column == key.column && self.scope.matches(&key.item)
    }

    /// 入力に現れなかった場合の表示用の鍵。
    pub fn placeholder_key(&self) -> MetricKey {
        MetricKey::new(self.activity, self.scope.placeholder(), self.column)
    }

    pub fn display(&self) -> String {
        format!(
            "{}/{}/{}",
            self.activity.display_name(),
            self.scope.placeholder(),
            self.column
        )
    }
}

/// 鍵に対応するカタログ項目を引く。
pub fn lookup(key: &MetricKey) -> Option<&'static CatalogEntry> {
    CATALOG.iter().find(|e| e.matches(key))
}

/// カタログが必要とする activity の一覧 (重複なし・ID 昇順)。
///
/// デコード対象を絞るのに使う。**カタログに無い activity は読まない**ので、
/// item 数の多いホスト (128 CPU・多数のデバイス) でも保持する時系列が
/// 際限なく増えない。
pub fn activities() -> Vec<ActivityId> {
    let mut v: Vec<ActivityId> = Vec::new();
    for e in CATALOG {
        if !v.contains(&e.activity) {
            v.push(e.activity);
        }
    }
    v.sort_unstable();
    v
}

// ===========================================================================
// 解釈の文面 (複数の候補を並べ、どれとも断定しない)
// ===========================================================================

const I_CPU_BUSY: &[&str] = &[
    "CPU 能力が要求に対して不足している",
    "単一のプロセスが CPU を占有している",
    "意図的に CPU を使い切るバッチ処理が動いていた",
];
const NE_CPU_BUSY: &[&str] = &[
    "どのプロセスが CPU を使っていたか (sa ファイルにプロセス別の内訳は無い)",
    "処理が遅延したかどうか (応答時間は観測していない)",
];

const I_IOWAIT: &[&str] = &[
    "ストレージの応答が遅い",
    "I/O 要求が多い (正常な負荷でも上がる)",
    "CPU が空いているため待ち時間が相対的に大きく見えている",
];
const NE_IOWAIT: &[&str] = &[
    "ストレージ障害の有無 (%iowait だけでは判定できない。デバイス別の await / %util と併せて見る)",
    "どのデバイスが待たされていたか",
];

const I_STEAL: &[&str] = &[
    "同一ハイパーバイザ上の他ゲストと CPU を競合している",
    "CPU クォータによる制限を受けている",
];
const NE_STEAL: &[&str] = &["ホスト側の構成・他ゲストの負荷 (ゲスト内の統計からは見えない)"];

const I_SYS: &[&str] = &[
    "システムコールや割り込み処理が増えた",
    "ファイルシステム・ネットワークスタックでの処理が増えた",
];
const NE_SYS: &[&str] = &["どのカーネル処理が増えたか (内訳は記録されていない)"];

const I_RUNQ: &[&str] = &[
    "CPU 数に対して実行可能タスクが多い",
    "短時間に大量のタスクが投入された",
];
const NE_RUNQ: &[&str] = &[
    "待ち時間の長さ (キュー長からは算出できない)",
    "絶対値の妥当性は CPU 数に依存する。固定条件は置いていない",
];

const I_BLOCKED: &[&str] = &[
    "I/O の完了待ちが常に存在する",
    "ネットワークストレージの応答待ちが続いている",
];
const NE_BLOCKED: &[&str] = &["待たされていたデバイス (blocked にデバイスの内訳は無い)"];

const I_LOADAVG: &[&str] = &[
    "実行可能・I/O 待ちのタスクが増えた",
    "CPU 数に対して負荷が大きい",
];
const NE_LOADAVG: &[&str] = &[
    "load average は I/O 待ちを含む。CPU 不足とは限らない",
    "適正値は CPU 数に依存する。固定条件は置いていない",
];

const I_MEM_AVAIL: &[&str] = &[
    "メモリ要求が増えた",
    "回収できないページ (tmpfs・カーネルスラブ) が増えた",
];
const NE_MEM_AVAIL: &[&str] = &[
    "OOM Killer が動いたか (sa ファイルに記録は無い)",
    "絶対値の妥当性は搭載量に依存する。固定条件は置いていない",
];

const I_MEMUSED: &[&str] = &["割り当て済みメモリが増えた", "ページキャッシュが増えた"];
const NE_MEMUSED: &[&str] = &[
    "%memused はページキャッシュを含むため、高いこと自体はメモリ不足を意味しない。\
     固定条件を置いていないのはこのため",
];

const I_COMMIT: &[&str] = &["割り当てを約束したメモリ量が増えた"];
const NE_COMMIT: &[&str] =
    &["%commit が 100 を超えること自体は異常ではない (overcommit は既定で許可される)"];

const I_SWAP_SPACE: &[&str] = &[
    "過去にメモリ不足があり、退避したページが残っている",
    "現在もメモリが不足している",
];
const NE_SWAP_SPACE: &[&str] =
    &["使用率が高いこと自体は現在のメモリ不足を意味しない (退避済みページは読み戻されるまで残る)"];

const I_SWAP_IO: &[&str] = &[
    "メモリ不足でページの追い出し・読み戻しが起きている",
    "長時間使われないページを退避しているだけ (swappiness による通常動作)",
];
const NE_SWAP_IO: &[&str] = &[
    "スワップ発生が性能低下を招いたか (遅延は観測していない)",
    "どのプロセスのページが退避されたか",
];

const I_RECLAIM_K: &[&str] = &[
    "空きメモリが回収閾値を下回り kswapd が回収を始めた",
    "ページキャッシュの入れ替えが活発 (大量の逐次 I/O でも起きる)",
];
const NE_RECLAIM_K: &[&str] = &["回収が割り当て待ちを起こしたか (待ち時間は観測していない)"];

const I_RECLAIM_D: &[&str] = &[
    "kswapd の回収が追いつかず、プロセス自身が回収している",
    "特定の zone / NUMA ノードのメモリが枯渇している",
];
const NE_RECLAIM_D: &[&str] = &["どのプロセスが待たされたか"];

const I_MAJFLT: &[&str] = &[
    "実行イメージやマップしたファイルの読み込みが発生した",
    "スワップインが発生した",
];
const NE_MAJFLT: &[&str] = &["起動直後やバッチ開始時には通常発生する。単独では異常を意味しない"];

const I_DISK_UTIL: &[&str] = &[
    "デバイスへの要求が処理能力に達している",
    "逐次 I/O でデバイスを使い切っている (正常な高スループット)",
];
const NE_DISK_UTIL: &[&str] = &[
    "%util は「要求が 1 つ以上あった時間の割合」であり、複数キューのデバイス \
     (NVMe・RAID) では 100% でも飽和を意味しない",
    "デバイス名は major/minor から組んだ表記であり、OS 上の名前とは異なる場合がある",
];

const I_DISK_AWAIT: &[&str] = &[
    "デバイスの応答が遅い",
    "キューに要求が滞留している",
    "要求サイズが大きく 1 要求あたりの時間が伸びている",
];
const NE_DISK_AWAIT: &[&str] = &[
    "await は待ち時間とサービス時間の合計。デバイス障害と輻輳を区別できない",
    "アプリケーションから見た遅延 (await はブロック層の値)",
];

const I_DISK_LOAD: &[&str] = &["I/O 要求が増えた", "書き込みフラッシュが集中した"];
const NE_DISK_LOAD: &[&str] = &["どのプロセスの I/O か (内訳は記録されていない)"];

const I_NET_TP: &[&str] = &["転送量が増えた", "バックアップ・レプリケーションが動いた"];
const NE_NET_TP: &[&str] = &["相手先・プロトコルの内訳 (記録されていない)"];

const I_NET_UTIL: &[&str] = &["リンク帯域を使い切っている"];
const NE_NET_UTIL: &[&str] = &[
    "%ifutil はインターフェースの申告速度を分母にする。速度が取得できない \
     インターフェースでは値が意味を持たない",
];

const I_NET_ERR: &[&str] = &[
    "リンク品質・ケーブル・対向機器に問題がある",
    "受信バッファが溢れている (負荷起因)",
];
const NE_NET_ERR: &[&str] = &["どのエラーがどの層で起きたか (内訳は限定的)"];

const I_FS_FULL: &[&str] = &[
    "書き込みが増えて空き容量が減った",
    "ログ・一時ファイルが溜まっている",
];
const NE_FS_FULL: &[&str] = &[
    "どのディレクトリが使っているか (sa ファイルに内訳は無い)",
    "%fsused は特権ユーザ視点。非特権プロセスから見た空きは %ufsused",
];

const I_FS_INODE: &[&str] = &["小さなファイルが大量に作られている"];
const NE_FS_INODE: &[&str] = &["どのディレクトリのファイルか"];

const I_FILE_NR: &[&str] = &[
    "開いているファイル記述子が増えた",
    "記述子を閉じ忘れているプロセスがある",
];
const NE_FILE_NR: &[&str] = &[
    "上限に達したか (file-max はこのファイルに記録されていない)",
    "どのプロセスが開いているか",
];

const I_PSI_CPU: &[&str] = &["実行可能タスクが CPU を待っている"];
const NE_PSI_CPU: &[&str] = &["PSI は待ち時間の割合であり、待ったタスクの内訳は持たない"];

const I_PSI_IO: &[&str] = &["I/O 完了待ちで処理が止まっている"];
const NE_PSI_IO: &[&str] = &["どのデバイス・どのプロセスが待ったか"];

const I_PSI_MEM: &[&str] = &["メモリ回収待ちで処理が止まっている"];
const NE_PSI_MEM: &[&str] = &["どのプロセスが待ったか"];

const I_CSWCH: &[&str] = &[
    "実行するタスクが増えた",
    "ロック競合や短い待ちが頻発している",
    "割り込みが増えた",
];
const NE_CSWCH: &[&str] = &["適正値はワークロードに依存する。固定条件は置いていない"];

const I_PROC: &[&str] = &[
    "プロセス・スレッドの生成が増えた",
    "fork を多用する処理が動いた",
];
const NE_PROC: &[&str] = &["生成されたプロセスの内容 (記録されていない)"];

const I_SOCK: &[&str] = &[
    "接続数が増えた",
    "短命な接続が大量に作られている (TIME_WAIT の滞留)",
];
const NE_SOCK: &[&str] = &["接続先・ポートの内訳 (記録されていない)"];

// ===========================================================================
// カタログ本体
// ===========================================================================

/// 見る系列の表。**順序は出力順ではない** (出力は時刻順に並べ替える)。
pub static CATALOG: &[CatalogEntry] = &[
    // ---------------- CPU ----------------
    CatalogEntry {
        activity: ActivityId::CPU,
        column: "idle",
        scope: ItemScope::Aggregate,
        kind: ValueKind::Counter,
        unit: Unit::Percent,
        label: "CPU の空き時間",
        fixed: &[FixedCondition {
            id: "cpu-idle-exhausted",
            comparison: FixedComparison::AtMost,
            value: 5.0,
            pattern: Pattern::Saturation,
            min_samples: 2,
            priority: Priority::Investigate,
            rationale: "%idle が 5% を下回る区間では CPU がほぼ空いていなかった。\
                        搭載量やワークロードに依存せず意味が定まる値である",
        }],
        deviation: DeviationInterest::Lower,
        shift: ShiftMagnitude::Absolute(20.0),
        interpretations: I_CPU_BUSY,
        not_established: NE_CPU_BUSY,
    },
    CatalogEntry {
        activity: ActivityId::CPU,
        column: "iowait",
        scope: ItemScope::Aggregate,
        kind: ValueKind::Counter,
        unit: Unit::Percent,
        label: "I/O 待ちの CPU 時間",
        fixed: &[FixedCondition {
            id: "cpu-iowait-high",
            comparison: FixedComparison::AtLeast,
            value: 30.0,
            pattern: Pattern::Sustained,
            min_samples: 2,
            priority: Priority::Watch,
            rationale: "CPU 時間の 3 割以上が I/O 完了待ちだった。\
                        正常な大量 I/O でも上がるため、原因の特定には至らない",
        }],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Absolute(10.0),
        interpretations: I_IOWAIT,
        not_established: NE_IOWAIT,
    },
    CatalogEntry {
        activity: ActivityId::CPU,
        column: "steal",
        scope: ItemScope::Aggregate,
        kind: ValueKind::Counter,
        unit: Unit::Percent,
        label: "奪われた CPU 時間",
        fixed: &[FixedCondition {
            id: "cpu-steal-present",
            comparison: FixedComparison::Above,
            value: 2.0,
            pattern: Pattern::Sustained,
            min_samples: 2,
            priority: Priority::Watch,
            rationale: "仮想化環境で実行を待たされた CPU 時間の割合。\
                        ゲスト内の対策では解消しないため、2% 超でも報告する価値がある",
        }],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Absolute(5.0),
        interpretations: I_STEAL,
        not_established: NE_STEAL,
    },
    CatalogEntry {
        activity: ActivityId::CPU,
        column: "sys",
        scope: ItemScope::Aggregate,
        kind: ValueKind::Counter,
        unit: Unit::Percent,
        label: "カーネルモードの CPU 時間",
        fixed: &[],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Absolute(15.0),
        interpretations: I_SYS,
        not_established: NE_SYS,
    },
    // ---------------- 実行キュー・負荷 ----------------
    CatalogEntry {
        activity: ActivityId::QUEUE,
        column: "runq_sz",
        scope: ItemScope::Single,
        kind: ValueKind::Gauge,
        unit: Unit::None,
        label: "実行待ちタスク数",
        fixed: &[],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Absolute(4.0),
        interpretations: I_RUNQ,
        not_established: NE_RUNQ,
    },
    CatalogEntry {
        activity: ActivityId::QUEUE,
        column: "blocked",
        scope: ItemScope::Single,
        kind: ValueKind::Gauge,
        unit: Unit::None,
        label: "ブロックされたタスク数",
        fixed: &[FixedCondition {
            id: "queue-blocked-present",
            comparison: FixedComparison::Above,
            value: 0.0,
            pattern: Pattern::Emergence,
            min_samples: 3,
            priority: Priority::Watch,
            rationale: "I/O 完了待ちで走れないタスクが存在した。\
                        通常は 0 であり、複数回続けて 1 以上になるのは事象として意味がある",
        }],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Absolute(2.0),
        interpretations: I_BLOCKED,
        not_established: NE_BLOCKED,
    },
    CatalogEntry {
        activity: ActivityId::QUEUE,
        column: "ldavg_1",
        scope: ItemScope::Single,
        kind: ValueKind::Gauge,
        unit: Unit::None,
        label: "1 分平均負荷",
        fixed: &[],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Absolute(2.0),
        interpretations: I_LOADAVG,
        not_established: NE_LOADAVG,
    },
    CatalogEntry {
        activity: ActivityId::QUEUE,
        column: "ldavg_15",
        scope: ItemScope::Single,
        kind: ValueKind::Gauge,
        unit: Unit::None,
        label: "15 分平均負荷",
        fixed: &[],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Absolute(2.0),
        interpretations: I_LOADAVG,
        not_established: NE_LOADAVG,
    },
    // ---------------- メモリ ----------------
    CatalogEntry {
        activity: ActivityId::MEMORY,
        column: "kbavail",
        scope: ItemScope::Single,
        kind: ValueKind::Gauge,
        unit: Unit::Kilobytes,
        label: "利用可能メモリ",
        fixed: &[],
        deviation: DeviationInterest::Lower,
        shift: ShiftMagnitude::Relative {
            fraction: 0.3,
            floor: 262_144.0,
        },
        interpretations: I_MEM_AVAIL,
        not_established: NE_MEM_AVAIL,
    },
    CatalogEntry {
        activity: ActivityId::MEMORY,
        column: "memused_pct",
        scope: ItemScope::Single,
        kind: ValueKind::Gauge,
        unit: Unit::Percent,
        label: "メモリ使用率",
        fixed: &[],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Absolute(10.0),
        interpretations: I_MEMUSED,
        not_established: NE_MEMUSED,
    },
    CatalogEntry {
        activity: ActivityId::MEMORY,
        column: "commit_pct",
        scope: ItemScope::Single,
        kind: ValueKind::Gauge,
        unit: Unit::Percent,
        label: "約束済みメモリの比率",
        fixed: &[],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Absolute(25.0),
        interpretations: I_COMMIT,
        not_established: NE_COMMIT,
    },
    CatalogEntry {
        activity: ActivityId::MEMORY,
        column: "swpused_pct",
        scope: ItemScope::Single,
        kind: ValueKind::Gauge,
        unit: Unit::Percent,
        label: "スワップ使用率",
        fixed: &[FixedCondition {
            id: "swap-space-in-use",
            comparison: FixedComparison::Above,
            value: 1.0,
            pattern: Pattern::Depletion,
            min_samples: 2,
            priority: Priority::Watch,
            rationale: "スワップ領域が使われている。過去または現在のメモリ不足を示す。\
                        スワップ未構成のホストでは列自体が 0 総量になり評価されない",
        }],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Absolute(10.0),
        interpretations: I_SWAP_SPACE,
        not_established: NE_SWAP_SPACE,
    },
    // ---------------- スワップ入出力 ----------------
    CatalogEntry {
        activity: ActivityId::SWAP,
        column: "pswpin",
        scope: ItemScope::Single,
        kind: ValueKind::Counter,
        unit: Unit::CountPerSec,
        label: "スワップイン",
        fixed: &[FixedCondition {
            id: "swap-in-occurred",
            comparison: FixedComparison::Above,
            value: 0.0,
            pattern: Pattern::Emergence,
            min_samples: 1,
            priority: Priority::Watch,
            rationale: "退避したページを読み戻している。通常運用では 0 であり、\
                        0 でないこと自体が事象として意味を持つ",
        }],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::NotEvaluated,
        interpretations: I_SWAP_IO,
        not_established: NE_SWAP_IO,
    },
    CatalogEntry {
        activity: ActivityId::SWAP,
        column: "pswpout",
        scope: ItemScope::Single,
        kind: ValueKind::Counter,
        unit: Unit::CountPerSec,
        label: "スワップアウト",
        fixed: &[FixedCondition {
            id: "swap-out-occurred",
            comparison: FixedComparison::Above,
            value: 0.0,
            pattern: Pattern::Emergence,
            min_samples: 1,
            priority: Priority::Watch,
            rationale: "ページを退避している。通常運用では 0 であり、\
                        0 でないこと自体が事象として意味を持つ",
        }],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::NotEvaluated,
        interpretations: I_SWAP_IO,
        not_established: NE_SWAP_IO,
    },
    // ---------------- ページ回収 ----------------
    CatalogEntry {
        activity: ActivityId::PAGE,
        column: "pgscank",
        scope: ItemScope::Single,
        kind: ValueKind::Counter,
        unit: Unit::CountPerSec,
        label: "kswapd のページスキャン",
        fixed: &[FixedCondition {
            id: "page-reclaim-kswapd",
            comparison: FixedComparison::Above,
            value: 0.0,
            pattern: Pattern::Emergence,
            min_samples: 1,
            priority: Priority::Informational,
            rationale: "空きメモリが回収閾値を下回り kswapd が回収を始めた。\
                        大量の逐次 I/O でも起きるため、単独では負荷の証拠にならない",
        }],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::NotEvaluated,
        interpretations: I_RECLAIM_K,
        not_established: NE_RECLAIM_K,
    },
    CatalogEntry {
        activity: ActivityId::PAGE,
        column: "pgscand",
        scope: ItemScope::Single,
        kind: ValueKind::Counter,
        unit: Unit::CountPerSec,
        label: "direct reclaim のページスキャン",
        fixed: &[FixedCondition {
            id: "page-reclaim-direct",
            comparison: FixedComparison::Above,
            value: 0.0,
            pattern: Pattern::Emergence,
            min_samples: 1,
            priority: Priority::Investigate,
            rationale: "kswapd が追いつかず、プロセス自身が回収を行っている。\
                        割り当てを求めたプロセスがその場で待たされる状態である",
        }],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::NotEvaluated,
        interpretations: I_RECLAIM_D,
        not_established: NE_RECLAIM_D,
    },
    CatalogEntry {
        activity: ActivityId::PAGE,
        column: "majflt",
        scope: ItemScope::Single,
        kind: ValueKind::Counter,
        unit: Unit::CountPerSec,
        label: "メジャーフォールト",
        fixed: &[],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Relative {
            fraction: 1.0,
            floor: 10.0,
        },
        interpretations: I_MAJFLT,
        not_established: NE_MAJFLT,
    },
    // ---------------- ブロック I/O (全体) ----------------
    CatalogEntry {
        activity: ActivityId::IO,
        column: "tps",
        scope: ItemScope::Single,
        kind: ValueKind::Counter,
        unit: Unit::CountPerSec,
        label: "ブロック I/O 転送数",
        fixed: &[],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Relative {
            fraction: 0.5,
            floor: 20.0,
        },
        interpretations: I_DISK_LOAD,
        not_established: NE_DISK_LOAD,
    },
    // ---------------- デバイス別 ----------------
    CatalogEntry {
        activity: ActivityId::DISK,
        column: "util_pct",
        scope: ItemScope::Each,
        kind: ValueKind::Counter,
        unit: Unit::Percent,
        label: "デバイス使用率",
        fixed: &[FixedCondition {
            id: "disk-utilization-saturated",
            comparison: FixedComparison::AtLeast,
            value: 95.0,
            pattern: Pattern::Saturation,
            min_samples: 2,
            priority: Priority::Watch,
            rationale: "要求が 1 つ以上あった時間の割合が 95% を超えた。\
                        単一キューのデバイスでは飽和を示す",
        }],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Absolute(25.0),
        interpretations: I_DISK_UTIL,
        not_established: NE_DISK_UTIL,
    },
    CatalogEntry {
        activity: ActivityId::DISK,
        column: "await",
        scope: ItemScope::Each,
        kind: ValueKind::Gauge,
        unit: Unit::Milliseconds,
        label: "デバイス応答時間",
        fixed: &[FixedCondition {
            id: "disk-latency-high",
            comparison: FixedComparison::AtLeast,
            value: 100.0,
            pattern: Pattern::Sustained,
            min_samples: 2,
            priority: Priority::Watch,
            rationale: "1 要求あたりの平均待ち時間が 100 ms を超えた。\
                        回転ディスクでも高く、要求の滞留を示す",
        }],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Relative {
            fraction: 1.0,
            floor: 10.0,
        },
        interpretations: I_DISK_AWAIT,
        not_established: NE_DISK_AWAIT,
    },
    CatalogEntry {
        activity: ActivityId::DISK,
        column: "avg_queue_size",
        scope: ItemScope::Each,
        kind: ValueKind::Counter,
        unit: Unit::None,
        label: "デバイスキュー長",
        fixed: &[],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Relative {
            fraction: 1.0,
            floor: 1.0,
        },
        interpretations: I_DISK_AWAIT,
        not_established: NE_DISK_AWAIT,
    },
    CatalogEntry {
        activity: ActivityId::DISK,
        column: "tps",
        scope: ItemScope::Each,
        kind: ValueKind::Counter,
        unit: Unit::CountPerSec,
        label: "デバイス転送数",
        fixed: &[],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Relative {
            fraction: 0.5,
            floor: 20.0,
        },
        interpretations: I_DISK_LOAD,
        not_established: NE_DISK_LOAD,
    },
    // ---------------- ネットワーク ----------------
    CatalogEntry {
        activity: ActivityId::NET_DEV,
        column: "ifutil_pct",
        scope: ItemScope::Each,
        kind: ValueKind::Gauge,
        unit: Unit::Percent,
        label: "インターフェース使用率",
        fixed: &[FixedCondition {
            id: "net-interface-saturated",
            comparison: FixedComparison::AtLeast,
            value: 90.0,
            pattern: Pattern::Saturation,
            min_samples: 2,
            priority: Priority::Watch,
            rationale: "リンク帯域の 9 割以上を使っている。\
                        申告速度が取得できないインターフェースでは値が 0 になり評価されない",
        }],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Absolute(25.0),
        interpretations: I_NET_UTIL,
        not_established: NE_NET_UTIL,
    },
    CatalogEntry {
        activity: ActivityId::NET_DEV,
        column: "rx_bytes_per_sec",
        scope: ItemScope::Each,
        kind: ValueKind::Counter,
        unit: Unit::BytesPerSec,
        label: "受信スループット",
        fixed: &[],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Relative {
            fraction: 1.0,
            floor: 1_048_576.0,
        },
        interpretations: I_NET_TP,
        not_established: NE_NET_TP,
    },
    CatalogEntry {
        activity: ActivityId::NET_DEV,
        column: "tx_bytes_per_sec",
        scope: ItemScope::Each,
        kind: ValueKind::Counter,
        unit: Unit::BytesPerSec,
        label: "送信スループット",
        fixed: &[],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Relative {
            fraction: 1.0,
            floor: 1_048_576.0,
        },
        interpretations: I_NET_TP,
        not_established: NE_NET_TP,
    },
    CatalogEntry {
        activity: ActivityId::NET_EDEV,
        column: "rxerr_per_sec",
        scope: ItemScope::Each,
        kind: ValueKind::Counter,
        unit: Unit::CountPerSec,
        label: "受信エラー",
        fixed: &[FixedCondition {
            id: "net-rx-error-occurred",
            comparison: FixedComparison::Above,
            value: 0.0,
            pattern: Pattern::Emergence,
            min_samples: 1,
            priority: Priority::Watch,
            rationale: "受信エラーが発生した。通常は 0 であり、\
                        0 でないこと自体が事象として意味を持つ",
        }],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::NotEvaluated,
        interpretations: I_NET_ERR,
        not_established: NE_NET_ERR,
    },
    CatalogEntry {
        activity: ActivityId::NET_EDEV,
        column: "txerr_per_sec",
        scope: ItemScope::Each,
        kind: ValueKind::Counter,
        unit: Unit::CountPerSec,
        label: "送信エラー",
        fixed: &[FixedCondition {
            id: "net-tx-error-occurred",
            comparison: FixedComparison::Above,
            value: 0.0,
            pattern: Pattern::Emergence,
            min_samples: 1,
            priority: Priority::Watch,
            rationale: "送信エラーが発生した。通常は 0 であり、\
                        0 でないこと自体が事象として意味を持つ",
        }],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::NotEvaluated,
        interpretations: I_NET_ERR,
        not_established: NE_NET_ERR,
    },
    CatalogEntry {
        activity: ActivityId::NET_EDEV,
        column: "rxdrop_per_sec",
        scope: ItemScope::Each,
        kind: ValueKind::Counter,
        unit: Unit::CountPerSec,
        label: "受信パケットの破棄",
        fixed: &[FixedCondition {
            id: "net-rx-drop-occurred",
            comparison: FixedComparison::Above,
            value: 0.0,
            pattern: Pattern::Emergence,
            min_samples: 1,
            priority: Priority::Watch,
            rationale: "受信キューでパケットを捨てた。通常は 0 であり、\
                        0 でないこと自体が事象として意味を持つ",
        }],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::NotEvaluated,
        interpretations: I_NET_ERR,
        not_established: NE_NET_ERR,
    },
    CatalogEntry {
        activity: ActivityId::NET_EDEV,
        column: "txdrop_per_sec",
        scope: ItemScope::Each,
        kind: ValueKind::Counter,
        unit: Unit::CountPerSec,
        label: "送信パケットの破棄",
        fixed: &[FixedCondition {
            id: "net-tx-drop-occurred",
            comparison: FixedComparison::Above,
            value: 0.0,
            pattern: Pattern::Emergence,
            min_samples: 1,
            priority: Priority::Watch,
            rationale: "送信キューでパケットを捨てた。通常は 0 であり、\
                        0 でないこと自体が事象として意味を持つ",
        }],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::NotEvaluated,
        interpretations: I_NET_ERR,
        not_established: NE_NET_ERR,
    },
    CatalogEntry {
        activity: ActivityId::NET_SOCK,
        column: "totsck",
        scope: ItemScope::Single,
        kind: ValueKind::Gauge,
        unit: Unit::Count,
        label: "使用中ソケット数",
        fixed: &[],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Relative {
            fraction: 0.5,
            floor: 100.0,
        },
        interpretations: I_SOCK,
        not_established: NE_SOCK,
    },
    CatalogEntry {
        activity: ActivityId::NET_SOCK,
        column: "tcp_tw",
        scope: ItemScope::Single,
        kind: ValueKind::Gauge,
        unit: Unit::Count,
        label: "TIME_WAIT のソケット数",
        fixed: &[],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Relative {
            fraction: 1.0,
            floor: 200.0,
        },
        interpretations: I_SOCK,
        not_established: NE_SOCK,
    },
    // ---------------- ファイルシステム ----------------
    CatalogEntry {
        activity: ActivityId::FS,
        column: "fs_used_pct",
        scope: ItemScope::Each,
        kind: ValueKind::Gauge,
        unit: Unit::Percent,
        label: "ファイルシステム使用率",
        fixed: &[FixedCondition {
            id: "filesystem-nearly-full",
            comparison: FixedComparison::AtLeast,
            value: 95.0,
            pattern: Pattern::Depletion,
            min_samples: 1,
            priority: Priority::Investigate,
            rationale: "空き容量が 5% を下回った。予約ブロックと断片化により、\
                        この水準からは書き込み失敗が現実的になる",
        }],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Absolute(5.0),
        interpretations: I_FS_FULL,
        not_established: NE_FS_FULL,
    },
    CatalogEntry {
        activity: ActivityId::FS,
        column: "inodes_used_pct",
        scope: ItemScope::Each,
        kind: ValueKind::Gauge,
        unit: Unit::Percent,
        label: "inode 使用率",
        fixed: &[FixedCondition {
            id: "filesystem-inodes-nearly-exhausted",
            comparison: FixedComparison::AtLeast,
            value: 95.0,
            pattern: Pattern::Depletion,
            min_samples: 1,
            priority: Priority::Investigate,
            rationale: "inode の残りが 5% を下回った。容量が空いていても\
                        ファイルを作れなくなる",
        }],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Absolute(5.0),
        interpretations: I_FS_INODE,
        not_established: NE_FS_INODE,
    },
    // ---------------- カーネルテーブル ----------------
    CatalogEntry {
        activity: ActivityId::KTABLES,
        column: "file_nr",
        scope: ItemScope::Single,
        kind: ValueKind::Gauge,
        unit: Unit::None,
        label: "使用中ファイル記述子数",
        fixed: &[],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Relative {
            fraction: 0.5,
            floor: 256.0,
        },
        interpretations: I_FILE_NR,
        not_established: NE_FILE_NR,
    },
    // ---------------- プロセス生成・切り替え ----------------
    CatalogEntry {
        activity: ActivityId::PCSW,
        column: "cswch",
        scope: ItemScope::Single,
        kind: ValueKind::Counter,
        unit: Unit::CountPerSec,
        label: "コンテキストスイッチ",
        fixed: &[],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Relative {
            fraction: 1.0,
            floor: 1_000.0,
        },
        interpretations: I_CSWCH,
        not_established: NE_CSWCH,
    },
    CatalogEntry {
        activity: ActivityId::PCSW,
        column: "proc",
        scope: ItemScope::Single,
        kind: ValueKind::Counter,
        unit: Unit::CountPerSec,
        label: "プロセス生成",
        fixed: &[],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Relative {
            fraction: 1.0,
            floor: 20.0,
        },
        interpretations: I_PROC,
        not_established: NE_PROC,
    },
    // ---------------- PSI (待ち圧力) ----------------
    CatalogEntry {
        activity: ActivityId::PSI_CPU,
        column: "scpu",
        scope: ItemScope::Single,
        kind: ValueKind::Counter,
        unit: Unit::Percent,
        label: "CPU の待ち圧力 (some)",
        fixed: &[FixedCondition {
            id: "psi-cpu-some-stalled",
            comparison: FixedComparison::AtLeast,
            value: 20.0,
            pattern: Pattern::Sustained,
            min_samples: 2,
            priority: Priority::Watch,
            rationale: "実行可能なタスクが CPU を待っていた時間の割合。\
                        20% を超える状態は待ちが常態化していることを示す",
        }],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Absolute(10.0),
        interpretations: I_PSI_CPU,
        not_established: NE_PSI_CPU,
    },
    CatalogEntry {
        activity: ActivityId::PSI_IO,
        column: "sio",
        scope: ItemScope::Single,
        kind: ValueKind::Counter,
        unit: Unit::Percent,
        label: "I/O の待ち圧力 (some)",
        fixed: &[],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Absolute(10.0),
        interpretations: I_PSI_IO,
        not_established: NE_PSI_IO,
    },
    CatalogEntry {
        activity: ActivityId::PSI_IO,
        column: "fio",
        scope: ItemScope::Single,
        kind: ValueKind::Counter,
        unit: Unit::Percent,
        label: "I/O の待ち圧力 (full)",
        fixed: &[FixedCondition {
            id: "psi-io-full-stalled",
            comparison: FixedComparison::Above,
            value: 1.0,
            pattern: Pattern::Sustained,
            min_samples: 2,
            priority: Priority::Investigate,
            rationale: "全タスクが I/O 待ちで進めなかった時間の割合。\
                        full stall は 1% でも処理が止まっていた証拠になる",
        }],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Absolute(5.0),
        interpretations: I_PSI_IO,
        not_established: NE_PSI_IO,
    },
    CatalogEntry {
        activity: ActivityId::PSI_MEM,
        column: "smem",
        scope: ItemScope::Single,
        kind: ValueKind::Counter,
        unit: Unit::Percent,
        label: "メモリの待ち圧力 (some)",
        fixed: &[],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Absolute(10.0),
        interpretations: I_PSI_MEM,
        not_established: NE_PSI_MEM,
    },
    CatalogEntry {
        activity: ActivityId::PSI_MEM,
        column: "fmem",
        scope: ItemScope::Single,
        kind: ValueKind::Counter,
        unit: Unit::Percent,
        label: "メモリの待ち圧力 (full)",
        fixed: &[FixedCondition {
            id: "psi-mem-full-stalled",
            comparison: FixedComparison::Above,
            value: 1.0,
            pattern: Pattern::Sustained,
            min_samples: 2,
            priority: Priority::Investigate,
            rationale: "全タスクがメモリ回収待ちで進めなかった時間の割合。\
                        full stall は 1% でも処理が止まっていた証拠になる",
        }],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Absolute(5.0),
        interpretations: I_PSI_MEM,
        not_established: NE_PSI_MEM,
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::registry::lookup as layout_lookup;

    /// カタログの宣言が `layout` 層と一致すること。
    ///
    /// 種別 (Counter / Gauge) と単位を二重管理しているので、
    /// 食い違えば検出の経路が間違う。**機械的に突き合わせる。**
    #[test]
    fn catalog_matches_the_layout_registry() {
        for e in CATALOG {
            let def = layout_lookup(e.activity)
                .unwrap_or_else(|| panic!("{}: 未知 activity", e.display()));
            let meta = def
                .columns
                .iter()
                .find(|c| c.public_name == e.column)
                .unwrap_or_else(|| panic!("{}: 列が layout に無い", e.display()));
            assert_eq!(meta.kind, e.kind, "{}: 種別が layout と違う", e.display());
            assert_eq!(meta.unit, e.unit, "{}: 単位が layout と違う", e.display());
            assert_ne!(
                meta.kind,
                ValueKind::Identity,
                "{}: 識別子列は検出対象にしない",
                e.display()
            );
        }
    }

    /// 条件 ID は一意であること (出力契約の一部なので衝突させない)。
    #[test]
    fn fixed_condition_ids_are_unique() {
        let mut ids: Vec<&str> = CATALOG
            .iter()
            .flat_map(|e| e.fixed.iter().map(|f| f.id))
            .collect();
        let before = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(before, ids.len(), "条件 ID が重複している");
    }

    /// カタログ項目 (activity, column, scope) が一意であること。
    ///
    /// 重複すると [`lookup`] が先頭だけを返し、後ろの宣言が黙って無効になる。
    #[test]
    fn catalog_entries_are_unique() {
        for (i, a) in CATALOG.iter().enumerate() {
            for b in &CATALOG[i + 1..] {
                assert!(
                    !(a.activity == b.activity && a.column == b.column && a.scope == b.scope),
                    "{} が重複している",
                    a.display()
                );
            }
        }
    }

    #[test]
    fn item_scope_separates_aggregate_from_each() {
        assert!(ItemScope::Aggregate.matches("all"));
        assert!(!ItemScope::Aggregate.matches("cpu0"));
        assert!(ItemScope::Each.matches("dev8-0"));
        assert!(!ItemScope::Each.matches("all"), "集約行を二重に数えない");
        assert!(ItemScope::Single.matches(SINGLE_ITEM));
        assert!(!ItemScope::Single.matches("dev8-0"));
    }

    #[test]
    fn lookup_finds_cpu_idle_for_the_all_row_only() {
        let all = MetricKey::new(ActivityId::CPU, "all", "idle");
        let cpu0 = MetricKey::new(ActivityId::CPU, "cpu0", "idle");
        assert!(lookup(&all).is_some());
        assert!(lookup(&cpu0).is_none(), "per-CPU は見ない");
    }

    #[test]
    fn activities_are_sorted_and_deduplicated() {
        let a = activities();
        let mut sorted = a.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(a, sorted);
        assert!(a.contains(&ActivityId::CPU));
        assert!(a.contains(&ActivityId::DISK));
    }

    /// 絶対値に意味が無い系列に固定条件を置いていないこと。
    ///
    /// CPU 数や搭載量に依存する系列へ固定条件を置くと、
    /// 小さいホストで鳴り続ける / 大きいホストで鳴らない検出になる。
    #[test]
    fn context_dependent_metrics_have_no_fixed_condition() {
        for (activity, column) in [
            (ActivityId::QUEUE, "runq_sz"),
            (ActivityId::QUEUE, "ldavg_1"),
            (ActivityId::QUEUE, "ldavg_15"),
            (ActivityId::MEMORY, "kbavail"),
            (ActivityId::MEMORY, "memused_pct"),
            (ActivityId::PCSW, "cswch"),
        ] {
            let e = CATALOG
                .iter()
                .find(|e| e.activity == activity && e.column == column)
                .expect("カタログ項目");
            assert!(
                e.fixed.is_empty(),
                "{}: 文脈依存の系列に固定条件を置いてはいけない",
                e.display()
            );
            assert!(
                !e.deviation.is_none(),
                "{}: 固定条件が無い系列は逸脱検出で見る必要がある",
                e.display()
            );
        }
    }

    /// すべての項目が少なくとも 1 つの経路で評価されること。
    #[test]
    fn every_entry_is_reachable_by_some_route() {
        for e in CATALOG {
            let reachable = !e.fixed.is_empty()
                || !e.deviation.is_none()
                || !matches!(e.shift, ShiftMagnitude::NotEvaluated);
            assert!(reachable, "{}: どの経路でも評価されない", e.display());
        }
    }

    #[test]
    fn deviation_interest_filters_direction() {
        assert!(DeviationInterest::Lower.accepts(ShiftDirection::Fall));
        assert!(!DeviationInterest::Lower.accepts(ShiftDirection::Rise));
        assert!(DeviationInterest::Both.accepts(ShiftDirection::Rise));
        assert!(!DeviationInterest::None.accepts(ShiftDirection::Rise));
    }
}

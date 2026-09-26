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
//! | `pswpin/s` > 0 (退避ページを読み戻した) | `runq-sz` (CPU 数に依存する) |
//! | `pgscand/s` > 0 (direct reclaim が起きた) | `kbavail` (搭載量に依存する) |
//! | `%fsused` が 95% 以上 (残り容量の割合で意味が定まる) | `cswch/s` (ワークロード次第) |
//! | `%memused` が 97% 以上 (利用可能メモリの割合。**運用上の設定値**) | `ldavg-*` (CPU 数に依存する) |
//!
//! 絶対値に意味が無い系列は固定条件を持たず、ロバスト逸脱と水準変化だけで見る。
//! **固定条件が無いことは「検出しない」ではない。**
//!
//! ## 固定条件は「観測」までしか確定させない
//!
//! 条件を満たしたことが示すのは**その値がその範囲にあった**という事実だけである。
//! 値の意味づけ (飽和・能力不足・メモリ不足) は条件から自動的には出てこない。
//!
//! - [`FixedCondition::pattern`] には**観測の形**を置く。`%idle` が低いことに
//!   [`Pattern::Saturation`] (「飽和」) を付けると、headline が
//!   ([`crate::analyze::assessment`] が `pattern.label()` をそのまま使う)
//!   確かめていない解釈を断定してしまう。
//! - [`FixedCondition::rationale`] は「**なぜその値に意味があるか**」だけを書く。
//!   そこから先の推論は [`CatalogEntry::interpretations`] に並べ、
//!   確かめられないことは [`CatalogEntry::not_established`] に書く。
//! - 普遍的な境界ではなく運用上都合で決めた値なら、rationale に
//!   **そうと明記する** (`%memused` の 97%、`%swpused` の 1% など)。
//!
//! ## 境界の表現は比較演算子から機械的に作る
//!
//! 「5% を下回る」と書いて比較が `AtMost` (5% 以下) だと、同じデータを
//! 人と AI が再判定したときに結果が食い違う。rationale には
//! [`boundary_phrase`] が [`FixedComparison`] から組んだ文言を必ず含める。
//! [`tests::rationales_state_the_boundary_that_the_comparison_implements`] が
//! 全件を機械的に照合する。

use crate::analyze::assessment::Priority;
use crate::analyze::summary::total_item_label;
use crate::analyze::timeline::{MetricKey, SINGLE_ITEM};
use crate::detect::{FixedComparison, Pattern, ShiftDirection};
use crate::model::{ActivityId, Lang, Text, Unit, ValueKind};
use crate::text;

/// カタログの版。項目・閾値を変えたら上げる。
///
/// `/2` で次を変えた (Issue #5 の 13〜22)。
///
/// - `%idle` / `%util` の固定条件から「飽和」の断定を外し、観測の形に改めた
/// - `%swpused` / `pswpout` / `rxdrop` の調査優先度を発生の事実と切り離した
/// - `%memused` に利用可能メモリ比率の固定観測条件を置いた
/// - 水準変化の関心方向を [`CatalogEntry::shift`]`_interest` として指標ごとに宣言した
pub const CATALOG_VERSION: &str = "resarch-detect-catalog/2";

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
    /// `activity` の item ラベルがこの範囲に入るか。
    ///
    /// 集約行かどうかは activity ごとに決まっている ([`total_item_label`])。
    /// ラベルの文字列 (`all` / `sum`) だけで決めると、集約行を持たない activity
    /// (`A_DISK` / `A_NET_DEV` / `A_FS` など) で `all` や `sum` という名前の
    /// デバイスを黙って対象から外し、「評価しなかった」を「検出なし」にしてしまう。
    pub fn matches(self, activity: ActivityId, item: &str) -> bool {
        let total = total_item_label(activity);
        match self {
            ItemScope::Single => item == SINGLE_ITEM,
            ItemScope::Aggregate => total == Some(item),
            // 集約行は個別 item として数えない (二重計上になる)
            ItemScope::Each => item != SINGLE_ITEM && total != Some(item),
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
    ///
    /// [`boundary_phrase`] が作る境界の文言を必ず含めること
    /// (テストで機械的に照合している)。解釈はここに書かず
    /// [`CatalogEntry::interpretations`] へ回す。
    pub rationale: Text,
}

/// 固定条件の境界を、実装している比較演算子から文言に起こす。
///
/// 「5% を下回る」(実装は `<= 5`)、「95% を超えた」(実装は `>= 95`) のような
/// 食い違いを無くすため、**rationale はこの関数が作る文言を含む**ことを
/// テストで固定している。境界のデータを人・AI が再判定したときに
/// 結果が変わらないようにするのが目的である。
///
/// 単位は [`Unit::suffix`] から採る。`%` は数値に続けて書き
/// (`5% 以下`)、それ以外の記号は 1 つ空ける (`100 ms 以上`)。
pub fn boundary_phrase(unit: Unit, condition: &FixedCondition, lang: Lang) -> String {
    let suffix = unit.suffix();
    let sep = if suffix.is_empty() || suffix == "%" {
        ""
    } else {
        " "
    };
    // 閾値は整数で宣言してある (0 / 1 / 95 / 100)。小数が来たら 1 桁で出す。
    let value = if condition.value.fract() == 0.0 {
        format!("{}", condition.value as i64)
    } else {
        format!("{:.1}", condition.value)
    };
    // **比較の語は言語で位置が変わる。** 日本語は後置 (`5% 以下`)、
    // 英語は前置 (`at most 5%`) で、語順まで含めてここで決める。
    let comparison = condition.comparison.label().get(lang).trim();
    match lang {
        Lang::Ja => format!("{value}{sep}{suffix} {comparison}"),
        Lang::En => format!("{comparison} {value}{sep}{suffix}"),
    }
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
    pub label: Text,
    /// 固定条件 (0 件でもよい)。
    pub fixed: &'static [FixedCondition],
    pub deviation: DeviationInterest,
    pub shift: ShiftMagnitude,
    /// **水準変化**で関心のある方向。逸脱 ([`Self::deviation`]) とは別の宣言。
    ///
    /// 判断の基準は [`CatalogEntry::shift_interest`] の doc にある。
    pub shift_direction: DeviationInterest,
    /// 考えられる解釈 (複数。どれとも断定しない)。
    pub interpretations: &'static [Text],
    /// この系列からは確かめられないこと。
    pub not_established: &'static [Text],
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
    /// | `%idle` | `Lower` (低いほうだけ外れ値) | `Both` (負荷の増減はどちらも所見) |
    /// | 受信スループット | `Upper` (急増が外れ値) | `Both` (**停止も所見**) |
    /// | `%util` | `Upper` | `Both` |
    /// | `runq-sz` | `Upper` | `Upper` (低下は負荷の緩和で所見にしない) |
    ///
    /// # 指標ごとの決め方
    ///
    /// 全指標を無条件に両方向にすると、夜間に負荷が抜けるだけで毎日鳴る。
    /// 系列の性質で 3 つに分ける。
    ///
    /// | 種別 | 方向 | 理由 |
    /// |---|---|---|
    /// | **処理量・稼働** (スループット / 転送数 / `%util` / `cswch/s` / `proc/s` / ソケット数 / `%idle`) | `Both` | 一定の処理が**ゼロへ落ちる**ことがサービス停止に対応する。方向で弾くと、停止を判定前に捨てる |
    /// | **圧力・滞留・占有** (`%iowait` / `%steal` / `%sys` / `runq-sz` / `ldavg` / `await` / `aqu-sz` / PSI / `%commit` / `%swpused` / `majflt` / `%fsused` / `%inodeused` / `file-nr` / `tcp-tw` / `%memused`) | `Upper` | 下がるのは圧力が抜けた状態で、それ自体は所見にならない。両方向にすると負荷の日次変動で鳴り続ける |
    /// | **余裕** (`kbavail`) | `Lower` | 減る方向だけが所見。増えるのは解放された状態 |
    ///
    /// `%idle` を `Both` にするのは、上昇が「負荷源の消失」だからである。
    /// 処理が終わったのか障害で止まったのかは**この系列からは断定できない**ので、
    /// 水準が動いた事実として出し、解釈は
    /// [`CatalogEntry::interpretations`] に並べる。
    ///
    /// `%ifutil` は `Upper` に留める。同じ事象を受信・送信スループットが
    /// すでに `Both` で見ており、ここを両方向にすると 1 つの停止を
    /// 3 系列で報告することになる。
    pub fn shift_interest(&self) -> DeviationInterest {
        self.shift_direction
    }

    /// この項目が指す系列か。
    pub fn matches(&self, key: &MetricKey) -> bool {
        self.activity == key.activity
            && self.column == key.column
            && self.scope.matches(key.activity, &key.item)
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

// `%idle` は「CPU が働いていた割合」の裏返しではない。
// `sar(1)`: `%idle` = 「CPU が idle で、**未完了のディスク I/O 要求が無かった**
// 時間の割合」、`%iowait` = 「CPU が idle で、未完了のディスク I/O 要求が
// **あった**時間の割合」。**どちらも CPU は idle** である。
// したがって `%idle` が 0 でも、その時間が `%iowait` に寄っているだけで
// CPU は何も実行していない、という状態があり得る。
const I_CPU_IDLE: &[Text] = &[
    text!(
        ja: "実行時間 (%user + %nice + %system) が増え、CPU 能力が要求に対して不足している",
        en: "Running time (%user + %nice + %system) rose and the CPU cannot keep up with demand",
    ),
    text!(
        ja: "単一のプロセスが CPU を占有している",
        en: "A single process is monopolising the CPU",
    ),
    text!(
        ja: "意図的に CPU を使い切るバッチ処理が動いていた",
        en: "A batch job designed to use the whole CPU was running",
    ),
    text!(
        ja: "idle 時間が I/O 完了待ち (%iowait) に寄っており、CPU は実行していない",
        en: "The idle time shifted into waiting for I/O (%iowait); the CPU is not executing",
    ),
    text!(
        ja: "仮想化環境で実行を待たされている (%steal に寄っている)",
        en: "The guest is waiting to run under virtualisation (the time shifted into %steal)",
    ),
    text!(
        ja: "水準が上がった場合は負荷源が消えた (処理の完了とサービスの停止を区別できない)",
        en: "If the level rose, the source of load disappeared (work finishing and a service stopping cannot be told apart)",
    ),
];
const NE_CPU_IDLE: &[Text] = &[
    text!(
        ja: "CPU 能力の不足。%idle と %iowait は、どちらも CPU が処理していない時間。\
             未完了のディスク I/O がある場合は %iowait に数えられるため、%idle の低さだけでは CPU 不足と判断できない。\
             %idle=0 かつ %iowait=99 の場合も含む",
        en: "That the CPU is short of capacity. In sar(1), %idle is idle time with no outstanding disk I/O \
     and %iowait is idle time with outstanding disk I/O — **the CPU is idle in both** — so a low \
     %idle leads nowhere on its own. %idle=0 with %iowait=99 also meets this condition",
    ),
    text!(
        ja: "CPU が実際に処理していた時間の割合 (%user + %nice + %system)。この検出では合計値を評価していない",
        en: "The share of running time (%user + %nice + %system). There is no mechanism for conditions on a \
     sum of columns, and this catalog has no derived columns",
    ),
    text!(
        ja: "どのプロセスが CPU を使っていたか (sa ファイルにプロセス別の内訳は無い)",
        en: "Which processes used the CPU (the sa file has no per-process breakdown)",
    ),
    text!(
        ja: "処理が遅延したかどうか (応答時間は観測していない)",
        en: "Whether anything was actually delayed (response time is not observed)",
    ),
];

// `%iowait` は「CPU が idle で、未完了のディスク I/O 要求があった時間」の割合
// (`sar(1)`)。**CPU が I/O のために働いている時間ではない。**
// 分母は tick 合計なので、CPU が空いていれば同じ I/O 量でも割合は大きく見える。
const I_IOWAIT: &[Text] = &[
    text!(
        ja: "ストレージの応答が遅い",
        en: "Storage is responding slowly",
    ),
    text!(
        ja: "I/O 要求が多い (正常な負荷でも上がる)",
        en: "There are many I/O requests (a healthy load raises this too)",
    ),
    text!(
        ja: "CPU が空いているため待ち時間が相対的に大きく見えている",
        en: "The CPU is idle, which makes the waiting time look large in relative terms",
    ),
];
const NE_IOWAIT: &[Text] = &[
    text!(
        ja: "ストレージ障害の有無 (%iowait だけでは判定できない。デバイス別の await / %util と併せて見る)",
        en: "Whether storage is faulty (%iowait alone cannot tell; read it with per-device await / %util)",
    ),
    text!(
        ja: "CPU の忙しさ。%iowait の時間は CPU が idle だった時間であり、\
     %idle と足して「空いていた割合」になる",
        en: "How busy the CPU was. Time in %iowait is time the CPU was idle; added to %idle it gives the \
     share of time it was free",
    ),
    text!(
        ja: "どのデバイスが待たされていたか",
        en: "Which device was being waited on",
    ),
];

const I_STEAL: &[Text] = &[
    text!(
        ja: "同一ハイパーバイザ上の他ゲストと CPU を競合している",
        en: "The guest is contending for CPU with other guests on the same hypervisor",
    ),
    text!(
        ja: "CPU クォータによる制限を受けている",
        en: "A CPU quota is capping it",
    ),
];
const NE_STEAL: &[Text] = &[text!(
    ja: "ホスト側の構成・他ゲストの負荷 (ゲスト内の統計からは見えない)",
    en: "The host's configuration and the load from other guests (invisible from statistics inside the guest)",
)];

const I_SYS: &[Text] = &[
    text!(
        ja: "システムコールや割り込み処理が増えた",
        en: "System calls or interrupt handling increased",
    ),
    text!(
        ja: "ファイルシステム・ネットワークスタックでの処理が増えた",
        en: "Work in the filesystem or the network stack increased",
    ),
];
const NE_SYS: &[Text] = &[text!(
    ja: "どのカーネル処理が増えたか (内訳は記録されていない)",
    en: "Which kernel work increased (no breakdown is recorded)",
)];

const I_RUNQ: &[Text] = &[
    text!(
        ja: "CPU 数に対して実行可能タスクが多い",
        en: "There are many runnable tasks for the number of CPUs",
    ),
    text!(
        ja: "短時間に大量のタスクが投入された",
        en: "A burst of tasks was submitted in a short time",
    ),
];
const NE_RUNQ: &[Text] = &[
    text!(
        ja: "待ち時間の長さ (キュー長からは算出できない)",
        en: "How long anything waited (queue length does not give this)",
    ),
    text!(
        ja: "絶対値の妥当性は CPU 数に依存する。固定条件は置いていない",
        en: "Whether the absolute value is reasonable depends on the CPU count, so no fixed condition is declared",
    ),
];

const I_BLOCKED: &[Text] = &[
    text!(
        ja: "I/O の完了待ちが常に存在する",
        en: "There is always something waiting for I/O to complete",
    ),
    text!(
        ja: "ネットワークストレージの応答待ちが続いている",
        en: "Waits on network storage are continuing",
    ),
];
const NE_BLOCKED: &[Text] = &[text!(
    ja: "待たされていたデバイス (blocked にデバイスの内訳は無い)",
    en: "Which device was being waited on (blocked carries no per-device breakdown)",
)];

const I_LOADAVG: &[Text] = &[
    text!(
        ja: "実行可能・I/O 待ちのタスクが増えた",
        en: "Runnable and I/O-waiting tasks increased",
    ),
    text!(
        ja: "CPU 数に対して負荷が大きい",
        en: "The load is large for the number of CPUs",
    ),
];
const NE_LOADAVG: &[Text] = &[
    text!(
        ja: "load average は I/O 待ちを含む。CPU 不足とは限らない",
        en: "Load average includes tasks waiting on I/O, so it does not have to mean a CPU shortage",
    ),
    text!(
        ja: "適正値は CPU 数に依存する。固定条件は置いていない",
        en: "What counts as reasonable depends on the CPU count, so no fixed condition is declared",
    ),
];

const I_MEM_AVAIL: &[Text] = &[
    text!(
        ja: "メモリ要求が増えた",
        en: "Demand for memory increased",
    ),
    text!(
        ja: "回収できないページ (tmpfs・カーネルスラブ) が増えた",
        en: "Unreclaimable pages (tmpfs, kernel slab) increased",
    ),
];
const NE_MEM_AVAIL: &[Text] = &[
    text!(
        ja: "OOM Killer が動いたか (sa ファイルに記録は無い)",
        en: "Whether the OOM killer ran (the sa file records nothing about it)",
    ),
    text!(
        ja: "絶対値の妥当性は搭載量に依存する。固定条件は置いていない",
        en: "Whether the absolute value is reasonable depends on how much memory is installed, so no fixed \
     condition is declared",
    ),
];

// この実装の `%memused` は `100 × (tlmkb − availablekb) / tlmkb`
// (`series::compute` の `memory_derived`)。`availablekb` は `/proc/meminfo` の
// `MemAvailable` で、カーネル文書 (`filesystems/proc.rst`) では
// 「swapping なしで新しいアプリケーションを起動するのに使える量の推定値。
// MemFree・SReclaimable・file LRU の大きさと各 zone の low watermark から計算する」
// と定義されている。**回収可能なページキャッシュは既に差し引かれている**ので、
// 「ページキャッシュを含むから高くても問題ない」は**この列には当てはまらない**。
const I_MEMUSED: &[Text] = &[
    text!(
        ja: "割り当て済み (回収できない) メモリが増えた",
        en: "Allocated, unreclaimable memory increased",
    ),
    text!(
        ja: "回収できないページ (tmpfs・カーネルスラブ・mlock) が増えた",
        en: "Unreclaimable pages (tmpfs, kernel slab, mlock) increased",
    ),
    text!(
        ja: "ページキャッシュのうち回収できると見積もられない分が増えた",
        en: "The part of the page cache not estimated as reclaimable increased",
    ),
];
const NE_MEMUSED: &[Text] = &[
    text!(
        ja: "OOM Killer が動いたか (sa ファイルに記録は無い)",
        en: "Whether the OOM killer ran (the sa file records nothing about it)",
    ),
    text!(
        ja: "割り当てが実際に待たされたか。待ちの有無は PSI memory (some / full) と \
     pgscand/s が示すが、どちらも採取されていない世代がある",
        en: "Whether an allocation actually had to wait. PSI memory (some / full) and pgscand/s would show \
     that, but there are generations where neither is collected",
    ),
    text!(
        ja: "97% という境界は運用上の設定値であり、カーネルがこの比率で挙動を変えるわけではない。\
     搭載量が大きいホストでは残り 3% が絶対量としては十分な場合がある",
        en: "The 97% boundary is an operational setting, not a ratio at which the kernel changes behaviour. \
     On a host with a lot of memory the remaining 3% can still be plenty in absolute terms",
    ),
    text!(
        ja: "旧世代 (availablekb 非搭載) の値。本家互換出力は kbmemfree で代用するが、\
     その値は回収可能なページキャッシュを含み意味が変わる。検出経路は厳密モードで\
     読むため代用せず、評価不能として報告する",
        en: "The value on older generations that carry no availablekb. The compatibility output substitutes \
     kbmemfree, but that value includes reclaimable page cache and means something different. The \
     detection path reads strictly and does not substitute; it reports the series as not evaluated",
    ),
];

const I_COMMIT: &[Text] = &[text!(
    ja: "割り当てを約束したメモリ量が増えた",
    en: "The amount of memory promised to allocations increased",
)];
const NE_COMMIT: &[Text] = &[text!(
    ja: "%commit が 100 を超えること自体は異常ではない (overcommit は既定で許可される)",
    en: "%commit going above 100 is not itself a fault (overcommit is permitted by default)",
)];

// Linux は不足時にだけ退避するのではない。`vm.swappiness` (既定 60、
// カーネル文書 `admin-guide/sysctl/vm.rst`) は「swap とファイルページングの
// 相対 I/O コスト」の設定で、0 でない既定構成では**不足が無くても**
// 使われないページが退避される。したがって「使用中」は背景情報であり、
// それ自体が調査の理由にはならない。
const I_SWAP_SPACE: &[Text] = &[
    text!(
        ja: "swappiness の設定に従って使われないページが退避された (通常動作)",
        en: "Unused pages were evicted in line with the swappiness setting (ordinary behaviour)",
    ),
    text!(
        ja: "過去にメモリ不足があり、退避したページが残っている",
        en: "Memory ran short in the past and the evicted pages are still there",
    ),
    text!(
        ja: "現在もメモリが不足している",
        en: "Memory is short right now",
    ),
];
const NE_SWAP_SPACE: &[Text] = &[
    text!(
        ja: "現在のメモリ不足。退避済みページは読み戻されるまで残るので、\
     使用率からは過去の痕跡と現在の不足を区別できない",
        en: "Whether memory is short now. Evicted pages stay until they are read back, so usage cannot \
     separate a trace of the past from a shortage in the present",
    ),
    text!(
        ja: "この 1% は運用上の設定値であり、普遍的な意味を持つ境界ではない。\
     swappiness とワークロードによって通常運用の水準が変わる",
        en: "This 1% is an operational setting, not a boundary with universal meaning. What is normal moves \
     with swappiness and with the workload",
    ),
    text!(
        ja: "スワップが未構成かどうか。総量が 0 の場合は使用率を 0% と計算し、検出しない。\
             「未構成のため対象外」という区別は出していない",
        en: "What it means on a host with no swap configured. With a total of 0 the usage is computed as 0% \
     (`swpused_pct` in `series::compute`), so the condition never holds — but the report does not \
     say 'not applicable because swap is not configured' either",
    ),
];

const I_SWAP_IO: &[Text] = &[
    text!(
        ja: "長時間使われないページを退避しているだけ (swappiness による通常動作)",
        en: "Long-unused pages are simply being evicted (ordinary behaviour under swappiness)",
    ),
    text!(
        ja: "メモリ不足でページの追い出し・読み戻しが起きている",
        en: "Memory is short, so pages are being pushed out and read back",
    ),
];
const NE_SWAP_IO: &[Text] = &[
    text!(
        ja: "スワップ発生が性能低下を招いたか (遅延は観測していない)",
        en: "Whether the swapping hurt performance (latency is not observed)",
    ),
    text!(
        ja: "メモリ不足かどうか。swappiness が 0 でない既定構成では不足が無くても発生する",
        en: "Whether memory is short. With the default non-zero swappiness this happens without any shortage",
    ),
    text!(
        ja: "どのプロセスのページが退避されたか",
        en: "Whose pages were evicted",
    ),
];

const I_RECLAIM_K: &[Text] = &[
    text!(
        ja: "空きメモリが回収閾値を下回り kswapd が回収を始めた",
        en: "Free memory fell below the watermark and kswapd started reclaiming",
    ),
    text!(
        ja: "ページキャッシュの入れ替えが活発 (大量の逐次 I/O でも起きる)",
        en: "The page cache is turning over quickly (heavy sequential I/O does this too)",
    ),
];
const NE_RECLAIM_K: &[Text] = &[text!(
    ja: "回収が割り当て待ちを起こしたか (待ち時間は観測していない)",
    en: "Whether the reclaim made any allocation wait (waiting time is not observed)",
)];

const I_RECLAIM_D: &[Text] = &[
    text!(
        ja: "kswapd の回収が追いつかず、プロセス自身が回収している",
        en: "kswapd cannot keep up, so processes are reclaiming for themselves",
    ),
    text!(
        ja: "特定の zone / NUMA ノードのメモリが枯渇している",
        en: "Memory in a particular zone or NUMA node is exhausted",
    ),
];
const NE_RECLAIM_D: &[Text] = &[text!(
    ja: "どのプロセスが待たされたか",
    en: "Which processes were made to wait",
)];

const I_MAJFLT: &[Text] = &[
    text!(
        ja: "実行イメージやマップしたファイルの読み込みが発生した",
        en: "Executable images or mapped files were read in",
    ),
    text!(
        ja: "スワップインが発生した",
        en: "Pages were swapped in",
    ),
];
const NE_MAJFLT: &[Text] = &[text!(
    ja: "起動直後やバッチ開始時には通常発生する。単独では異常を意味しない",
    en: "This is normal just after start-up or at the beginning of a batch job; on its own it means nothing is wrong",
)];

// `sar(1)`: `%util` = 「デバイスへ I/O 要求が発行されていた経過時間の割合
// (デバイスの帯域利用率)」。**要求を何本同時に処理していたかは入っていない**ので、
// 並列に処理するデバイス (NVMe・RAID・SSD) では 100% でも余力があり得る。
// 本家の man も「such as RAID arrays and modern SSDs, this number does not
// reflect their performance limits」と注記している。
const I_DISK_UTIL: &[Text] = &[
    text!(
        ja: "デバイスへの要求が処理能力に達している",
        en: "Requests to the device have reached what it can process",
    ),
    text!(
        ja: "逐次 I/O でデバイスを使い切っている (正常な高スループット)",
        en: "Sequential I/O is using the device fully (healthy high throughput)",
    ),
    text!(
        ja: "要求を並列に処理するデバイスで、稼働時間が長くても余力が残っている",
        en: "On a device that processes requests in parallel, a long busy time can still leave headroom",
    ),
    text!(
        ja: "水準が下がった場合は I/O を出していた処理が止まった (完了か停止かは区別できない)",
        en: "If the level fell, whatever was issuing I/O stopped (finishing and halting cannot be told apart)",
    ),
];
const NE_DISK_UTIL: &[Text] = &[
    text!(
        ja: "処理能力の飽和。%util は「要求が 1 つ以上あった時間の割合」で同時実行数を含まないため、\
     複数キューのデバイス (NVMe・RAID) では 100% でも飽和を意味しない",
        en: "That the device is saturated. %util is the share of time at least one request was outstanding \
     and carries no notion of concurrency, so on multi-queue devices (NVMe, RAID) even 100% does \
     not mean saturation",
    ),
    text!(
        ja: "残っている余力の量 (キュー深度・並列度はこの列からは分からない)",
        en: "How much headroom is left (queue depth and parallelism are not in this column)",
    ),
    text!(
        ja: "デバイス名は major/minor から組んだ表記であり、OS 上の名前とは異なる場合がある",
        en: "The device name is built from major/minor and can differ from the name the OS uses",
    ),
];

// `sar(1)`: `await` = 「デバイスへ発行された I/O 要求が処理されるまでの平均時間
// (ミリ秒)。**キューで待った時間とサービスに要した時間の両方を含む**」。
// 大きいことは「遅かった」までしか示さず、滞留 (キュー待ち) と
// サービス時間の長さ (大きな要求・低速デバイス) を分けられない。
const I_DISK_AWAIT: &[Text] = &[
    text!(
        ja: "デバイスの応答が遅い",
        en: "The device is responding slowly",
    ),
    text!(
        ja: "キューに要求が滞留している",
        en: "Requests are queueing up",
    ),
    text!(
        ja: "要求サイズが大きく 1 要求あたりの時間が伸びている",
        en: "Requests are large, which stretches the time each one takes",
    ),
];
const NE_DISK_AWAIT: &[Text] = &[
    text!(
        ja: "滞留かサービス時間か。await はキュー待ち時間とサービス時間の合計なので、\
     値の大きさだけでは切り分けられない (aqu-sz / areq-sz と併せる)",
        en: "Whether this is queueing or service time. await is the sum of both, so its size alone does not \
     separate them (read it with aqu-sz / areq-sz)",
    ),
    text!(
        ja: "デバイス障害と輻輳の区別",
        en: "Telling a failing device from a congested one",
    ),
    text!(
        ja: "アプリケーションから見た遅延 (await はブロック層の値)",
        en: "The latency an application sees (await is a block-layer figure)",
    ),
];

const I_DISK_LOAD: &[Text] = &[
    text!(
        ja: "I/O 要求が増えた",
        en: "I/O requests increased",
    ),
    text!(
        ja: "書き込みフラッシュが集中した",
        en: "Write flushes bunched up",
    ),
    text!(
        ja: "水準が下がった場合は I/O を出していた処理が止まった (完了か停止かは区別できない)",
        en: "If the level fell, whatever was issuing I/O stopped (finishing and halting cannot be told apart)",
    ),
];
const NE_DISK_LOAD: &[Text] = &[
    text!(
        ja: "どのプロセスの I/O か (内訳は記録されていない)",
        en: "Whose I/O this is (no breakdown is recorded)",
    ),
    text!(
        ja: "転送数が減った理由 (処理の完了・停止・上流の詰まりを区別できない)",
        en: "Why the transfer count fell (work finishing, halting, and a blockage upstream cannot be told apart)",
    ),
];

const I_NET_TP: &[Text] = &[
    text!(
        ja: "転送量が増えた",
        en: "The volume transferred increased",
    ),
    text!(
        ja: "バックアップ・レプリケーションが動いた",
        en: "A backup or replication job ran",
    ),
    text!(
        ja: "水準が下がった場合は通信が止まった (処理の完了・上流の停止・経路障害を区別できない)",
        en: "If the level fell, traffic stopped (work finishing, an upstream halt, and a broken path cannot \
     be told apart)",
    ),
];
const NE_NET_TP: &[Text] = &[
    text!(
        ja: "相手先・プロトコルの内訳 (記録されていない)",
        en: "Which peers and protocols (not recorded)",
    ),
    text!(
        ja: "転送量が減った理由 (このインターフェースの統計だけでは断定できない)",
        en: "Why the volume fell (this interface's statistics alone cannot settle it)",
    ),
];

const I_NET_UTIL: &[Text] = &[text!(
    ja: "リンク帯域を使い切っている",
    en: "The link's bandwidth is fully used",
)];
const NE_NET_UTIL: &[Text] = &[
    text!(
        ja: "%ifutil はインターフェースの申告速度 (`speed`、Mbit/s) を分母にする。\
     速度が 0 = 不明のインターフェース (仮想デバイス・ethtool が返さない NIC) では\
     分母が無いので**値を作らず評価不能として報告する**",
        en: "%ifutil divides by the speed the interface declares (`speed`, Mbit/s). Where that is 0, meaning \
     unknown (virtual devices, NICs ethtool will not answer for), there is no denominator, so \
     **no value is produced and the series is reported as not evaluated**",
    ),
    text!(
        ja: "全二重では受信・送信の**大きい方**だけを見る (sar(1))。逆方向の余裕は分からない",
        en: "On full duplex only the **larger** of receive and transmit is considered (sar(1)), so headroom \
     in the other direction is unknown",
    ),
];

// `rx_errors` / `tx_errors` (カーネル文書 `networking/statistics.rst`) は
// 「受信した不良パケットの総数」「送信時の問題の総数」で、下位カウンタ
// (crc / frame / carrier / fifo など) を束ねた値である。内訳は sa ファイルに無い。
const I_NET_ERR: &[Text] = &[
    text!(
        ja: "リンク品質・ケーブル・対向機器に問題がある",
        en: "There is a problem with link quality, the cable, or the device at the other end",
    ),
    text!(
        ja: "デバイスの FIFO が溢れている (負荷起因)",
        en: "The device's FIFO is overflowing (load-induced)",
    ),
];
const NE_NET_ERR: &[Text] = &[text!(
    ja: "どのエラーがどの層で起きたか。rx_errors / tx_errors は下位カウンタを束ねた総数で、\
 内訳 (crc / frame / carrier / fifo) は記録されていない",
    en: "Which error happened at which layer. rx_errors / tx_errors are totals bundling lower counters, \
 and the breakdown (crc / frame / carrier / fifo) is not recorded",
)];

// `/proc/net/dev` の drop 列は**キュー溢れ専用のカウンタではない**。
// カーネル文書 (`networking/statistics.rst`) の `rx_dropped` は
// 「受信したが処理されなかったパケットの数。例えば資源不足や**未対応プロトコル**による。
// ハードウェアインターフェースではこのカウンタは **L2 アドレスフィルタで破棄された
// パケットを含み得る**」。さらに `rx_missed_errors` (「ホストが取りこぼしたパケット」) は
// 「procfs では drop カウンタに畳み込まれる」ので、この列は 2 つの合算である。
const I_NET_RX_DROP: &[Text] = &[
    text!(
        ja: "未対応のプロトコル・VLAN タグのパケットを受け取った (処理されないのが正常)",
        en: "Packets arrived for an unsupported protocol or VLAN tag (not processing them is correct)",
    ),
    text!(
        ja: "受信キュー・ソケットバッファが溢れた",
        en: "A receive queue or socket buffer overflowed",
    ),
    text!(
        ja: "L2 アドレスフィルタで破棄された",
        en: "They were discarded by the L2 address filter",
    ),
    text!(
        ja: "ホストが取りこぼした (rx_missed_errors 分。procfs が drop に畳み込む)",
        en: "The host missed them (the rx_missed_errors part, which procfs folds into drop)",
    ),
];
const NE_NET_RX_DROP: &[Text] = &[
    text!(
        ja: "破棄の原因。procfs の drop 列は資源不足・未対応プロトコル・L2 フィルタ・\
     rx_missed_errors を合算した値で、内訳は sa ファイルからは分けられない",
        en: "Why they were dropped. The drop column in procfs sums resource shortages, unsupported \
     protocols, the L2 filter and rx_missed_errors, and the sa file cannot separate them",
    ),
    text!(
        ja: "運用上許容できる件数・割合。受信パケット数に対する比率と、\
     そのホストで通常どれだけ計上されるかを別に決める必要がある",
        en: "How many, or what share, is acceptable in practice. The ratio against received packets and what \
     this host normally records both have to be decided separately",
    ),
    text!(
        ja: "通信品質への影響 (破棄されたパケットが再送されたかは観測していない)",
        en: "The effect on quality of service (whether the dropped packets were retransmitted is not observed)",
    ),
];

// `tx_dropped` = 「送信に向かう途中で破棄されたパケットの数。例えば資源不足による」
// (カーネル文書 `networking/statistics.rst`)。
const I_NET_TX_DROP: &[Text] = &[
    text!(
        ja: "送信キューの資源が不足した",
        en: "The transmit queue ran short of resources",
    ),
    text!(
        ja: "デバイスが停止している間に送信しようとした",
        en: "Something tried to send while the device was down",
    ),
];
const NE_NET_TX_DROP: &[Text] = &[
    text!(
        ja: "破棄の原因 (tx_dropped は「送信に向かう途中の資源不足等」を束ねた値)",
        en: "Why they were dropped (tx_dropped bundles resource shortages and the like on the way out)",
    ),
    text!(
        ja: "運用上許容できる件数・割合",
        en: "How many, or what share, is acceptable in practice",
    ),
];

const I_FS_FULL: &[Text] = &[
    text!(
        ja: "書き込みが増えて空き容量が減った",
        en: "Writing increased and free space shrank",
    ),
    text!(
        ja: "ログ・一時ファイルが溜まっている",
        en: "Logs or temporary files are piling up",
    ),
];
const NE_FS_FULL: &[Text] = &[
    text!(
        ja: "どのディレクトリが使っているか (sa ファイルに内訳は無い)",
        en: "Which directories are using it (the sa file has no breakdown)",
    ),
    text!(
        ja: "%fsused は特権ユーザ視点。非特権プロセスから見た空きは %ufsused",
        en: "%fsused is the privileged view; what an unprivileged process sees free is %ufsused",
    ),
];

const I_FS_INODE: &[Text] = &[text!(
    ja: "小さなファイルが大量に作られている",
    en: "Large numbers of small files are being created",
)];
const NE_FS_INODE: &[Text] = &[text!(
    ja: "どのディレクトリのファイルか",
    en: "Which directory the files are in",
)];

const I_FILE_NR: &[Text] = &[
    text!(
        ja: "開いているファイル記述子が増えた",
        en: "The number of open file descriptors increased",
    ),
    text!(
        ja: "記述子を閉じ忘れているプロセスがある",
        en: "Some process is failing to close descriptors",
    ),
];
const NE_FILE_NR: &[Text] = &[
    text!(
        ja: "上限に達したか (file-max はこのファイルに記録されていない)",
        en: "Whether the limit was reached (file-max is not recorded in this file)",
    ),
    text!(
        ja: "どのプロセスが開いているか",
        en: "Which process has them open",
    ),
];

// PSI (カーネル文書 `accounting/psi.rst`): some = 「少なくとも一部のタスクが
// 待たされていた時間の割合」、full = 「**全 non-idle タスク**が同時に
// 待たされていた時間の割合」。full は「全タスク」ではない
// (待つべき仕事を持たないタスクは数に入らない)。
const I_PSI_CPU: &[Text] = &[text!(
    ja: "実行可能タスクが CPU を待っている",
    en: "Runnable tasks are waiting for CPU",
)];
const NE_PSI_CPU: &[Text] = &[
    text!(
        ja: "PSI は待ち時間の割合であり、待ったタスクの内訳は持たない",
        en: "PSI is a share of stalled time and carries no breakdown of which tasks stalled",
    ),
    text!(
        ja: "待ちが応答時間に響いたか (遅延は観測していない)",
        en: "Whether the waiting showed up in response time (latency is not observed)",
    ),
];

const I_PSI_IO: &[Text] = &[text!(
    ja: "I/O 完了待ちで処理が止まっている",
    en: "Work is stalled waiting for I/O to complete",
)];
const NE_PSI_IO: &[Text] = &[
    text!(
        ja: "どのデバイス・どのプロセスが待ったか",
        en: "Which device, and which process, was waiting",
    ),
    text!(
        ja: "full が示すのは「全 non-idle タスクが同時に待った」ことであり、\
     待つ仕事を持たないタスクまで止まっていたことではない",
        en: "full means every **non-idle** task stalled at the same time; it does not mean tasks with no work \
     to wait on were stopped too",
    ),
];

const I_PSI_MEM: &[Text] = &[text!(
    ja: "メモリ回収待ちで処理が止まっている",
    en: "Work is stalled waiting for memory reclaim",
)];
const NE_PSI_MEM: &[Text] = &[
    text!(
        ja: "どのプロセスが待ったか",
        en: "Which process was waiting",
    ),
    text!(
        ja: "full が示すのは「全 non-idle タスクが同時に待った」ことであり、\
     待つ仕事を持たないタスクまで止まっていたことではない",
        en: "full means every **non-idle** task stalled at the same time; it does not mean tasks with no work \
     to wait on were stopped too",
    ),
];

const I_CSWCH: &[Text] = &[
    text!(
        ja: "実行するタスクが増えた",
        en: "There are more tasks to run",
    ),
    text!(
        ja: "ロック競合や短い待ちが頻発している",
        en: "Lock contention or short waits are happening often",
    ),
    text!(
        ja: "割り込みが増えた",
        en: "Interrupts increased",
    ),
    text!(
        ja: "水準が下がった場合は動いていたタスクが減った (処理の完了と停止を区別できない)",
        en: "If the level fell, fewer tasks are running (work finishing and halting cannot be told apart)",
    ),
];
const NE_CSWCH: &[Text] = &[text!(
    ja: "適正値はワークロードに依存する。固定条件は置いていない",
    en: "What counts as reasonable depends on the workload, so no fixed condition is declared",
)];

const I_PROC: &[Text] = &[
    text!(
        ja: "プロセス・スレッドの生成が増えた",
        en: "Process and thread creation increased",
    ),
    text!(
        ja: "fork を多用する処理が動いた",
        en: "Something that forks heavily was running",
    ),
    text!(
        ja: "水準が下がった場合は生成していた処理が止まった (完了と停止を区別できない)",
        en: "If the level fell, whatever was creating them stopped (finishing and halting cannot be told apart)",
    ),
];
const NE_PROC: &[Text] = &[text!(
    ja: "生成されたプロセスの内容 (記録されていない)",
    en: "What the created processes were (not recorded)",
)];

const I_SOCK: &[Text] = &[
    text!(
        ja: "接続数が増えた",
        en: "The number of connections increased",
    ),
    text!(
        ja: "短命な接続が大量に作られている (TIME_WAIT の滞留)",
        en: "Large numbers of short-lived connections are being made (TIME_WAIT piling up)",
    ),
    text!(
        ja: "水準が下がった場合は接続が切れた・受け付けが止まった",
        en: "If the level fell, connections were lost or the host stopped accepting them",
    ),
];
const NE_SOCK: &[Text] = &[
    text!(
        ja: "接続先・ポートの内訳 (記録されていない)",
        en: "Which peers and ports (not recorded)",
    ),
    text!(
        ja: "接続数が減った理由 (正常な終了と受け付け停止を区別できない)",
        en: "Why the count fell (a clean shutdown and a halt in accepting cannot be told apart)",
    ),
];

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
        label: text!(ja: "CPU の空き時間", en: "CPU idle time"),
        fixed: &[FixedCondition {
            id: "cpu-idle-exhausted",
            comparison: FixedComparison::AtMost,
            value: 5.0,
            pattern: Pattern::Sustained,
            min_samples: 2,
            priority: Priority::Watch,
            rationale: text!(
                ja: "非 I/O 待ちの idle が 5% 以下で続いた。sar(1) の %idle は\
                            「CPU が idle で未完了のディスク I/O 要求が無かった時間」の割合で、\
                            搭載量やワークロードに依存せず意味が定まるのは**この観測まで**である。\
                            %iowait も idle 時間なので、ここから CPU 能力の不足には進めない",
                en: "Non-I/O-waiting idle stayed at or below 5%. In sar(1), %idle is the share of time the CPU was \
                 idle with no outstanding disk I/O request; its meaning is fixed, independent of \
                 capacity or workload, **only as far as that observation**. %iowait is idle time too, \
                 so this does not lead on to a shortage of CPU capacity",
            ),
        }],
        deviation: DeviationInterest::Lower,
        shift: ShiftMagnitude::Absolute(20.0),
        // 処理量の指標として両方向。上昇は「負荷源の消失」で、
        // 処理の完了かサービスの停止かは断定できない (解釈側に並べる)。
        shift_direction: DeviationInterest::Both,
        interpretations: I_CPU_IDLE,
        not_established: NE_CPU_IDLE,
    },
    CatalogEntry {
        activity: ActivityId::CPU,
        column: "iowait",
        scope: ItemScope::Aggregate,
        kind: ValueKind::Counter,
        unit: Unit::Percent,
        label: text!(ja: "I/O 待ちの CPU 時間", en: "CPU time idle with I/O outstanding"),
        fixed: &[FixedCondition {
            id: "cpu-iowait-high",
            comparison: FixedComparison::AtLeast,
            value: 30.0,
            pattern: Pattern::Sustained,
            min_samples: 2,
            priority: Priority::Watch,
            rationale: text!(
                ja: "未完了のディスク I/O があった idle 時間の割合が 30% 以上で続いた。\
                            正常な大量 I/O でも上がるため、原因の特定には至らない",
                en: "The share of idle time with disk I/O outstanding stayed at or above 30%. A healthy, heavy I/O \
                 load raises this too, so it does not identify a cause",
            ),
        }],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Absolute(10.0),
        // 圧力の指標。下がるのは待ちが解けた状態で、それ自体は所見にしない。
        shift_direction: DeviationInterest::Upper,
        interpretations: I_IOWAIT,
        not_established: NE_IOWAIT,
    },
    CatalogEntry {
        activity: ActivityId::CPU,
        column: "steal",
        scope: ItemScope::Aggregate,
        kind: ValueKind::Counter,
        unit: Unit::Percent,
        label: text!(ja: "奪われた CPU 時間", en: "Stolen CPU time"),
        fixed: &[FixedCondition {
            id: "cpu-steal-present",
            comparison: FixedComparison::Above,
            value: 2.0,
            pattern: Pattern::Sustained,
            min_samples: 2,
            priority: Priority::Watch,
            rationale: text!(
                ja: "仮想化環境で実行を待たされた CPU 時間の割合が 2% 超で続いた。\
                            ゲスト内の対策では解消しないため、この水準でも報告する価値がある",
                en: "The share of CPU time the guest spent waiting to run under virtualisation stayed above 2%. \
                 Nothing inside the guest resolves it, which is why even this level is worth reporting",
            ),
        }],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Absolute(5.0),
        // 圧力の指標。下がるのは競合が解けた状態。
        shift_direction: DeviationInterest::Upper,
        interpretations: I_STEAL,
        not_established: NE_STEAL,
    },
    CatalogEntry {
        activity: ActivityId::CPU,
        column: "sys",
        scope: ItemScope::Aggregate,
        kind: ValueKind::Counter,
        unit: Unit::Percent,
        label: text!(ja: "カーネルモードの CPU 時間", en: "CPU time in kernel mode"),
        fixed: &[],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Absolute(15.0),
        // 圧力の指標。低下はカーネル処理が減った状態で所見にしない。
        shift_direction: DeviationInterest::Upper,
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
        label: text!(ja: "実行待ちタスク数", en: "Runnable tasks"),
        fixed: &[],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Absolute(4.0),
        // 圧力の指標。キューが短くなるのは負荷の緩和で所見にしない。
        shift_direction: DeviationInterest::Upper,
        interpretations: I_RUNQ,
        not_established: NE_RUNQ,
    },
    CatalogEntry {
        activity: ActivityId::QUEUE,
        column: "blocked",
        scope: ItemScope::Single,
        kind: ValueKind::Gauge,
        unit: Unit::None,
        label: text!(ja: "ブロックされたタスク数", en: "Blocked tasks"),
        fixed: &[FixedCondition {
            id: "queue-blocked-present",
            comparison: FixedComparison::Above,
            value: 0.0,
            pattern: Pattern::Emergence,
            min_samples: 3,
            priority: Priority::Watch,
            rationale: text!(
                ja: "I/O 完了待ちで走れないタスクの数が 0 超で続いた。\
                            瞬間値としては珍しくないが、採取をまたいで続けて観測されるのは\
                            待ちが常態化していることを示す",
                en: "The number of tasks unable to run while waiting for I/O stayed above 0. A single sample is \
                 unremarkable, but seeing it across consecutive samples means the waiting has become \
                 the normal state",
            ),
        }],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Absolute(2.0),
        // 圧力の指標。
        shift_direction: DeviationInterest::Upper,
        interpretations: I_BLOCKED,
        not_established: NE_BLOCKED,
    },
    CatalogEntry {
        activity: ActivityId::QUEUE,
        column: "ldavg_1",
        scope: ItemScope::Single,
        kind: ValueKind::Gauge,
        unit: Unit::None,
        label: text!(ja: "1 分平均負荷", en: "1-minute load average"),
        fixed: &[],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Absolute(2.0),
        // 圧力の指標。低下は負荷の緩和。
        shift_direction: DeviationInterest::Upper,
        interpretations: I_LOADAVG,
        not_established: NE_LOADAVG,
    },
    CatalogEntry {
        activity: ActivityId::QUEUE,
        column: "ldavg_15",
        scope: ItemScope::Single,
        kind: ValueKind::Gauge,
        unit: Unit::None,
        label: text!(ja: "15 分平均負荷", en: "15-minute load average"),
        fixed: &[],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Absolute(2.0),
        // 圧力の指標。低下は負荷の緩和。
        shift_direction: DeviationInterest::Upper,
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
        label: text!(ja: "利用可能メモリ", en: "Available memory"),
        // kB の絶対量なので搭載量に依存する。割合による固定条件は
        // 同じ入力から作る `%memused` (= 100 − 利用可能比率) の側に置いた。
        fixed: &[],
        deviation: DeviationInterest::Lower,
        shift: ShiftMagnitude::Relative {
            fraction: 0.3,
            floor: 262_144.0,
        },
        // 余裕の指標。減る方向だけが所見 (増えるのは解放された状態)。
        shift_direction: DeviationInterest::Lower,
        interpretations: I_MEM_AVAIL,
        not_established: NE_MEM_AVAIL,
    },
    CatalogEntry {
        activity: ActivityId::MEMORY,
        column: "memused_pct",
        scope: ItemScope::Single,
        kind: ValueKind::Gauge,
        unit: Unit::Percent,
        label: text!(ja: "メモリ使用率", en: "Memory used"),
        // この列は `100 × (総量 − 利用可能) / 総量` なので、条件は
        // 「利用可能メモリが総量の 3% 以下」という**割合**の観測である。
        // 搭載量に依存しない形で書けるのはここまでで、3% という線は
        // 運用上の設定値である (rationale に明記する)。
        //
        // 旧世代 (availablekb 非搭載) では検出経路 (厳密モード) が
        // 欠落を返すため、`kbmemfree` で代用された値がこの条件に
        // 掛かることはない (`series::compute` の `memory_available`)。
        fixed: &[FixedCondition {
            id: "memory-available-scarce",
            comparison: FixedComparison::AtLeast,
            value: 97.0,
            pattern: Pattern::Depletion,
            min_samples: 2,
            priority: Priority::Watch,
            rationale: text!(
                ja: "利用可能メモリが総量の 3% 以下 (%memused が 97% 以上) で続いた。\
                            この列の分子は総量 − MemAvailable で、MemAvailable は\
                            「swapping なしで新しいアプリケーションを起動するのに使える量の推定値」\
                            (カーネル文書 filesystems/proc.rst) なので、回収可能なページキャッシュは\
                            既に差し引かれている。**97% は運用上の設定値**であり、\
                            カーネルがこの比率で挙動を変えるわけではない",
                en: "Available memory stayed at or below 3% of the total (%memused at or above 97%). The numerator \
                 of this column is total minus MemAvailable, and MemAvailable is the kernel's estimate \
                 of how much is available to start new applications without swapping \
                 (filesystems/proc.rst), so reclaimable page cache is already excluded",
            ),
        }],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Absolute(10.0),
        // 圧力の指標。低下は余裕が戻った状態 (kbavail 側の Lower と対になる)。
        shift_direction: DeviationInterest::Upper,
        interpretations: I_MEMUSED,
        not_established: NE_MEMUSED,
    },
    CatalogEntry {
        activity: ActivityId::MEMORY,
        column: "commit_pct",
        scope: ItemScope::Single,
        kind: ValueKind::Gauge,
        unit: Unit::Percent,
        label: text!(ja: "約束済みメモリの比率", en: "Committed memory ratio"),
        fixed: &[],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Absolute(25.0),
        // 圧力の指標。
        shift_direction: DeviationInterest::Upper,
        interpretations: I_COMMIT,
        not_established: NE_COMMIT,
    },
    CatalogEntry {
        activity: ActivityId::MEMORY,
        column: "swpused_pct",
        scope: ItemScope::Single,
        kind: ValueKind::Gauge,
        unit: Unit::Percent,
        label: text!(ja: "スワップ使用率", en: "Swap used"),
        // 「使われている」ことは**背景情報**である。swappiness が 0 でない
        // 既定構成では不足が無くても退避されるので、使用の事実から
        // メモリ不足へは進めない。1% は運用上の設定値として扱い、
        // 単独では調査の理由にしない (Informational)。
        fixed: &[FixedCondition {
            id: "swap-space-in-use",
            comparison: FixedComparison::Above,
            value: 1.0,
            pattern: Pattern::Sustained,
            min_samples: 2,
            priority: Priority::Informational,
            rationale: text!(
                ja: "スワップ領域の使用率が 1% 超で続いた。**この 1% は運用上の設定値**で、\
                            普遍的な境界ではない。Linux は vm.swappiness (既定 60) に従って\
                            使われないページを退避するため、使用中であること自体は\
                            メモリ不足の証拠にならない。退避済みページは読み戻されるまで残る",
                en: "Swap usage stayed above 1%. **This 1% is an operational setting**, not a universal boundary. \
                 Linux evicts unused pages in line with vm.swappiness (default 60), so swap being in \
                 use is not itself evidence that memory is short. Evicted pages stay until read back",
            ),
        }],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Absolute(10.0),
        // 圧力の指標。使用率の低下は読み戻しが進んだ状態。
        shift_direction: DeviationInterest::Upper,
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
        label: text!(ja: "スワップイン", en: "Swap-in"),
        fixed: &[FixedCondition {
            id: "swap-in-occurred",
            comparison: FixedComparison::Above,
            value: 0.0,
            pattern: Pattern::Emergence,
            min_samples: 1,
            priority: Priority::Watch,
            rationale: text!(
                ja: "退避したページの読み戻しが 0 超で観測された。読み戻しは\
                            ページフォールトの待ちを伴うので、発生したこと自体に意味がある。\
                            ただし**スワップを構成したホストで 0 が常態とは限らない**ため、\
                            どれだけ続いたか・どれだけの量かを別に見る必要がある",
                en: "Reading evicted pages back was observed above 0. A read-back carries a page-fault wait, so the \
                 fact it happened means something. But **0 is not necessarily the normal state on a \
                 host with swap configured**, so how long it continued and how much moved have to be \
                 read separately",
            ),
        }],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::NotEvaluated,
        // 発生自体が事象なので固定条件経路に任せる (shift は NotEvaluated)。
        shift_direction: DeviationInterest::Upper,
        interpretations: I_SWAP_IO,
        not_established: NE_SWAP_IO,
    },
    CatalogEntry {
        activity: ActivityId::SWAP,
        column: "pswpout",
        scope: ItemScope::Single,
        kind: ValueKind::Counter,
        unit: Unit::CountPerSec,
        label: text!(ja: "スワップアウト", en: "Swap-out"),
        fixed: &[FixedCondition {
            id: "swap-out-occurred",
            comparison: FixedComparison::Above,
            value: 0.0,
            pattern: Pattern::Emergence,
            min_samples: 1,
            priority: Priority::Informational,
            rationale: text!(
                ja: "ページの退避が 0 超で観測された。退避が起きた事実を示すが、\
                            vm.swappiness が 0 でない既定構成では**不足が無くても起きる**ため、\
                            単独では調査の理由にならない",
                en: "Page eviction was observed above 0. It shows eviction happened, but with the default non-zero \
                 vm.swappiness it **happens without any shortage**, so on its own it is not a reason \
                 to investigate",
            ),
        }],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::NotEvaluated,
        // 発生自体が事象なので固定条件経路に任せる。
        shift_direction: DeviationInterest::Upper,
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
        label: text!(ja: "kswapd のページスキャン", en: "Pages scanned by kswapd"),
        fixed: &[FixedCondition {
            id: "page-reclaim-kswapd",
            comparison: FixedComparison::Above,
            value: 0.0,
            pattern: Pattern::Emergence,
            min_samples: 1,
            priority: Priority::Informational,
            rationale: text!(
                ja: "kswapd のページスキャンが 0 超で観測された。空きメモリが\
                            回収閾値を下回ったことを示すが、大量の逐次 I/O でも起きるため\
                            単独では負荷の証拠にならない",
                en: "Page scanning by kswapd was observed above 0. It means free memory fell below the reclaim \
                 watermark, but heavy sequential I/O does this too, so on its own it is not evidence \
                 of load",
            ),
        }],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::NotEvaluated,
        // 発生自体が事象なので固定条件経路に任せる。
        shift_direction: DeviationInterest::Upper,
        interpretations: I_RECLAIM_K,
        not_established: NE_RECLAIM_K,
    },
    CatalogEntry {
        activity: ActivityId::PAGE,
        column: "pgscand",
        scope: ItemScope::Single,
        kind: ValueKind::Counter,
        unit: Unit::CountPerSec,
        label: text!(ja: "メモリ割り当て時のページ回収スキャン (direct reclaim)", en: "Pages scanned in direct reclaim"),
        fixed: &[FixedCondition {
            id: "page-reclaim-direct",
            comparison: FixedComparison::Above,
            value: 0.0,
            pattern: Pattern::Emergence,
            min_samples: 1,
            priority: Priority::Investigate,
            rationale: text!(
                ja: "direct reclaim のページスキャンが 0 超で観測された。\
                            kswapd が追いつかずプロセス自身が回収していることを示し、\
                            割り当てを求めたプロセスはその場で待たされる",
                en: "Page scanning in direct reclaim was observed above 0. It means kswapd could not keep up and \
                 processes are reclaiming for themselves; a process asking for an allocation waits \
                 there and then",
            ),
        }],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::NotEvaluated,
        // 発生自体が事象なので固定条件経路に任せる。
        shift_direction: DeviationInterest::Upper,
        interpretations: I_RECLAIM_D,
        not_established: NE_RECLAIM_D,
    },
    CatalogEntry {
        activity: ActivityId::PAGE,
        column: "majflt",
        scope: ItemScope::Single,
        kind: ValueKind::Counter,
        unit: Unit::CountPerSec,
        label: text!(ja: "メジャーフォールト", en: "Major faults"),
        fixed: &[],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Relative {
            fraction: 1.0,
            floor: 10.0,
        },
        // 圧力の指標。低下はディスクからの読み込みが減った状態。
        shift_direction: DeviationInterest::Upper,
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
        label: text!(ja: "ブロック I/O 転送数", en: "Block I/O transfers"),
        fixed: &[],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Relative {
            fraction: 0.5,
            floor: 20.0,
        },
        // 処理量の指標。**転送がゼロへ落ちることは所見**なので両方向。
        shift_direction: DeviationInterest::Both,
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
        label: text!(ja: "デバイス使用率", en: "Device utilisation"),
        // 汎用の条件は「**高い稼働時間割合**」までである。処理能力の飽和という
        // 解釈はデバイス種別 (単一キューか、並列に処理するか) が分からないと
        // 成立せず、この系列からはそれが分からない。したがって pattern に
        // `Saturation` (「飽和」) を置かない — headline がその解釈を断定してしまう。
        fixed: &[FixedCondition {
            id: "disk-utilization-saturated",
            comparison: FixedComparison::AtLeast,
            value: 95.0,
            pattern: Pattern::Sustained,
            min_samples: 2,
            priority: Priority::Watch,
            rationale: text!(
                ja: "I/O 要求が発行されていた経過時間の割合が 95% 以上で続いた。\
                            sar(1) の %util は「要求が 1 つ以上あった時間の割合」で\
                            **同時に何本処理していたかを含まない**ため、単一キューの\
                            デバイスでなければ処理能力の上限を示さない",
                en: "The share of elapsed time with I/O requests outstanding stayed at or above 95%. In sar(1), \
                 %util is the share of time at least one request was outstanding and **carries no \
                 notion of how many were in flight**, so on anything but a single-queue device it does \
                 not indicate a limit on throughput",
            ),
        }],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Absolute(25.0),
        // 稼働の指標。**稼働がゼロへ落ちることは所見**なので両方向。
        shift_direction: DeviationInterest::Both,
        interpretations: I_DISK_UTIL,
        not_established: NE_DISK_UTIL,
    },
    CatalogEntry {
        activity: ActivityId::DISK,
        column: "await",
        scope: ItemScope::Each,
        kind: ValueKind::Gauge,
        unit: Unit::Milliseconds,
        label: text!(ja: "デバイス応答時間", en: "Device response time"),
        fixed: &[FixedCondition {
            id: "disk-latency-high",
            comparison: FixedComparison::AtLeast,
            value: 100.0,
            pattern: Pattern::Sustained,
            min_samples: 2,
            priority: Priority::Watch,
            rationale: text!(
                ja: "1 要求あたりの平均時間が 100 ms 以上で続いた。回転ディスクの\
                            シーク時間と比べても大きい。ただし sar(1) の await は\
                            **キュー待ち時間とサービス時間の合計**なので、値の大きさだけでは\
                            要求の滞留とサービス時間の長さを区別できない",
                en: "The average time per request stayed at or above 100 ms, which is large even next to the seek \
                 time of a rotating disk. But await in sar(1) is **the sum of queueing time and service \
                 time**, so its size alone does not separate requests piling up from each one taking \
                 long",
            ),
        }],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Relative {
            fraction: 1.0,
            floor: 10.0,
        },
        // 滞留・遅延の指標。短くなるのは改善で所見にしない。
        shift_direction: DeviationInterest::Upper,
        interpretations: I_DISK_AWAIT,
        not_established: NE_DISK_AWAIT,
    },
    CatalogEntry {
        activity: ActivityId::DISK,
        column: "avg_queue_size",
        scope: ItemScope::Each,
        kind: ValueKind::Counter,
        unit: Unit::None,
        label: text!(ja: "デバイスキュー長", en: "Device queue length"),
        fixed: &[],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Relative {
            fraction: 1.0,
            floor: 1.0,
        },
        // 滞留の指標。短くなるのは改善。
        shift_direction: DeviationInterest::Upper,
        interpretations: I_DISK_AWAIT,
        not_established: NE_DISK_AWAIT,
    },
    CatalogEntry {
        activity: ActivityId::DISK,
        column: "tps",
        scope: ItemScope::Each,
        kind: ValueKind::Counter,
        unit: Unit::CountPerSec,
        label: text!(ja: "デバイス転送数", en: "Device transfers"),
        fixed: &[],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Relative {
            fraction: 0.5,
            floor: 20.0,
        },
        // 処理量の指標。**転送がゼロへ落ちることは所見**なので両方向。
        shift_direction: DeviationInterest::Both,
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
        label: text!(ja: "インターフェース使用率", en: "Interface utilisation"),
        // 分母 (申告速度) が取れないインターフェースでは、この列は値を持たない。
        // 以前は `speed == 0` でも 0.0 を返していたため、固定条件経路が
        // 「評価済み・検出なし」になっていた (規律 7 の抜け)。
        // 現在は `series::compute` の `net_dev_derived` が厳密モードで
        // 欠落を返し、評価不能として報告される。
        fixed: &[FixedCondition {
            id: "net-interface-saturated",
            comparison: FixedComparison::AtLeast,
            value: 90.0,
            pattern: Pattern::Sustained,
            min_samples: 2,
            priority: Priority::Watch,
            rationale: text!(
                ja: "インターフェース使用率が 90% 以上で続いた。分母は申告速度\
                            (`speed`、Mbit/s) で、全二重では受信・送信の大きい方だけを見る。\
                            速度が 0 = 不明のインターフェースでは値を作らず評価不能として報告する",
                en: "Interface utilisation stayed at or above 90%. The denominator is the declared speed (`speed`, \
                 Mbit/s), and on full duplex only the larger of receive and transmit is considered. \
                 Where the speed is 0, meaning unknown, no value is produced and the series is \
                 reported as not evaluated",
            ),
        }],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Absolute(25.0),
        // 受信・送信スループットが同じ事象を両方向で見ているので、ここは
        // 上方向に留める (1 つの停止を 3 系列で報告しないため)。
        shift_direction: DeviationInterest::Upper,
        interpretations: I_NET_UTIL,
        not_established: NE_NET_UTIL,
    },
    CatalogEntry {
        activity: ActivityId::NET_DEV,
        column: "rx_bytes_per_sec",
        scope: ItemScope::Each,
        kind: ValueKind::Counter,
        unit: Unit::BytesPerSec,
        label: text!(ja: "受信スループット", en: "Receive throughput"),
        fixed: &[],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Relative {
            fraction: 1.0,
            floor: 1_048_576.0,
        },
        // 処理量の指標。**通信がゼロへ落ちることは所見**なので両方向。
        // 上方向だけにすると、サービス停止を判定前に捨てることになる。
        shift_direction: DeviationInterest::Both,
        interpretations: I_NET_TP,
        not_established: NE_NET_TP,
    },
    CatalogEntry {
        activity: ActivityId::NET_DEV,
        column: "tx_bytes_per_sec",
        scope: ItemScope::Each,
        kind: ValueKind::Counter,
        unit: Unit::BytesPerSec,
        label: text!(ja: "送信スループット", en: "Transmit throughput"),
        fixed: &[],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Relative {
            fraction: 1.0,
            floor: 1_048_576.0,
        },
        // 処理量の指標。**通信がゼロへ落ちることは所見**なので両方向。
        shift_direction: DeviationInterest::Both,
        interpretations: I_NET_TP,
        not_established: NE_NET_TP,
    },
    CatalogEntry {
        activity: ActivityId::NET_EDEV,
        column: "rxerr_per_sec",
        scope: ItemScope::Each,
        kind: ValueKind::Counter,
        unit: Unit::CountPerSec,
        label: text!(ja: "受信エラー", en: "Receive errors"),
        fixed: &[FixedCondition {
            id: "net-rx-error-occurred",
            comparison: FixedComparison::Above,
            value: 0.0,
            pattern: Pattern::Emergence,
            min_samples: 1,
            priority: Priority::Watch,
            rationale: text!(
                ja: "受信エラーが 0 超で観測された。rx_errors は「受信した不良パケットの総数」\
                            (カーネル文書 networking/statistics.rst) で、正常なリンクでは増えない。\
                            0 でないこと自体が事象として意味を持つ",
                en: "Receive errors were observed above 0. rx_errors is the total number of bad packets received \
                 (networking/statistics.rst) and does not grow on a healthy link, so being non-zero is \
                 itself the event",
            ),
        }],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::NotEvaluated,
        // 発生自体が事象なので固定条件経路に任せる。
        shift_direction: DeviationInterest::Upper,
        interpretations: I_NET_ERR,
        not_established: NE_NET_ERR,
    },
    CatalogEntry {
        activity: ActivityId::NET_EDEV,
        column: "txerr_per_sec",
        scope: ItemScope::Each,
        kind: ValueKind::Counter,
        unit: Unit::CountPerSec,
        label: text!(ja: "送信エラー", en: "Transmit errors"),
        fixed: &[FixedCondition {
            id: "net-tx-error-occurred",
            comparison: FixedComparison::Above,
            value: 0.0,
            pattern: Pattern::Emergence,
            min_samples: 1,
            priority: Priority::Watch,
            rationale: text!(
                ja: "送信エラーが 0 超で観測された。tx_errors は「送信時の問題の総数」\
                            (カーネル文書 networking/statistics.rst) で、正常なリンクでは増えない。\
                            0 でないこと自体が事象として意味を持つ",
                en: "Transmit errors were observed above 0. tx_errors is the total number of problems on transmit \
                 (networking/statistics.rst) and does not grow on a healthy link, so being non-zero is \
                 itself the event",
            ),
        }],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::NotEvaluated,
        // 発生自体が事象なので固定条件経路に任せる。
        shift_direction: DeviationInterest::Upper,
        interpretations: I_NET_ERR,
        not_established: NE_NET_ERR,
    },
    CatalogEntry {
        activity: ActivityId::NET_EDEV,
        column: "rxdrop_per_sec",
        scope: ItemScope::Each,
        kind: ValueKind::Counter,
        unit: Unit::CountPerSec,
        label: text!(ja: "受信パケットの破棄", en: "Received packets dropped"),
        // **キュー溢れ専用のカウンタではない。** 原因を特定できないので、
        // 発生の事実だけを参考情報として出す (調査優先度は上げない)。
        // 未対応プロトコルのパケットを受け取るだけで計上されるホストは珍しくない。
        fixed: &[FixedCondition {
            id: "net-rx-drop-occurred",
            comparison: FixedComparison::Above,
            value: 0.0,
            pattern: Pattern::Emergence,
            min_samples: 1,
            priority: Priority::Informational,
            rationale: text!(
                ja: "受信したが処理されなかったパケットの計上が 0 超で観測された。\
                            カーネル文書 (networking/statistics.rst) の rx_dropped は\
                            「資源不足や**未対応プロトコル**等で処理されなかったパケットの数」で、\
                            L2 アドレスフィルタによる破棄を含み得る。さらに procfs はホストの\
                            取りこぼし (rx_missed_errors) をこの列に畳み込む。\
                            **原因は特定できない**",
                en: "Packets received but not processed were counted above 0. In the kernel documentation \
                 (networking/statistics.rst), rx_dropped is the number of packets not processed, for \
                 example through a shortage of resources or an **unsupported protocol**, and it can \
                 include packets discarded by the L2 address filter. procfs also folds the host's own \
                 rx_missed_errors into this column",
            ),
        }],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::NotEvaluated,
        // 発生自体が事象なので固定条件経路に任せる。
        shift_direction: DeviationInterest::Upper,
        interpretations: I_NET_RX_DROP,
        not_established: NE_NET_RX_DROP,
    },
    CatalogEntry {
        activity: ActivityId::NET_EDEV,
        column: "txdrop_per_sec",
        scope: ItemScope::Each,
        kind: ValueKind::Counter,
        unit: Unit::CountPerSec,
        label: text!(ja: "送信パケットの破棄", en: "Transmitted packets dropped"),
        fixed: &[FixedCondition {
            id: "net-tx-drop-occurred",
            comparison: FixedComparison::Above,
            value: 0.0,
            pattern: Pattern::Emergence,
            min_samples: 1,
            priority: Priority::Watch,
            rationale: text!(
                ja: "送信に向かう途中で破棄されたパケットの計上が 0 超で観測された。\
                            カーネル文書 (networking/statistics.rst) の tx_dropped は\
                            「送信に向かう途中で破棄されたパケットの数。例えば資源不足による」で、\
                            送信側の資源不足を示すが**内訳は特定できない**",
                en: "Packets discarded on the way out were counted above 0. In the kernel documentation \
                 (networking/statistics.rst), tx_dropped is the number of packets dropped on the way \
                 to transmission, for example through a shortage of resources. It points at resources \
                 on the sending side but **the breakdown cannot be identified**",
            ),
        }],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::NotEvaluated,
        // 発生自体が事象なので固定条件経路に任せる。
        shift_direction: DeviationInterest::Upper,
        interpretations: I_NET_TX_DROP,
        not_established: NE_NET_TX_DROP,
    },
    CatalogEntry {
        activity: ActivityId::NET_SOCK,
        column: "totsck",
        scope: ItemScope::Single,
        kind: ValueKind::Gauge,
        unit: Unit::Count,
        label: text!(ja: "使用中ソケット数", en: "Sockets in use"),
        fixed: &[],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Relative {
            fraction: 0.5,
            floor: 100.0,
        },
        // 稼働の指標。**接続がまとめて消えることは所見**なので両方向。
        shift_direction: DeviationInterest::Both,
        interpretations: I_SOCK,
        not_established: NE_SOCK,
    },
    CatalogEntry {
        activity: ActivityId::NET_SOCK,
        column: "tcp_tw",
        scope: ItemScope::Single,
        kind: ValueKind::Gauge,
        unit: Unit::Count,
        label: text!(ja: "TIME_WAIT のソケット数", en: "Sockets in TIME_WAIT"),
        fixed: &[],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Relative {
            fraction: 1.0,
            floor: 200.0,
        },
        // 滞留の指標 (接続数そのものは totsck 側で両方向に見ている)。
        shift_direction: DeviationInterest::Upper,
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
        label: text!(ja: "ファイルシステム使用率", en: "Filesystem used"),
        fixed: &[FixedCondition {
            id: "filesystem-nearly-full",
            comparison: FixedComparison::AtLeast,
            value: 95.0,
            pattern: Pattern::Depletion,
            min_samples: 1,
            priority: Priority::Investigate,
            rationale: text!(
                ja: "使用率が 95% 以上になった (空き容量が 5% 以下)。予約ブロックと\
                            断片化により、この水準からは書き込み失敗が現実的になる",
                en: "Usage stayed at or above 95% (5% or less free). With reserved blocks and \
                     fragmentation, a write failing becomes a realistic prospect from this level",
            ),
        }],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Absolute(5.0),
        // 余裕の減少を見る指標。使用率が下がるのは削除・巻き戻しで、
        // 資源のリスクではない。ログ回転で日常的に動くため両方向にしない。
        shift_direction: DeviationInterest::Upper,
        interpretations: I_FS_FULL,
        not_established: NE_FS_FULL,
    },
    CatalogEntry {
        activity: ActivityId::FS,
        column: "inodes_used_pct",
        scope: ItemScope::Each,
        kind: ValueKind::Gauge,
        unit: Unit::Percent,
        label: text!(ja: "inode 使用率", en: "Inodes used"),
        fixed: &[FixedCondition {
            id: "filesystem-inodes-nearly-exhausted",
            comparison: FixedComparison::AtLeast,
            value: 95.0,
            pattern: Pattern::Depletion,
            min_samples: 1,
            priority: Priority::Investigate,
            rationale: text!(
                ja: "inode 使用率が 95% 以上になった (残りが 5% 以下)。\
                            容量が空いていてもファイルを作れなくなる",
                en: "Inode usage stayed at or above 95% (5% or less left). Files can stop being \
                     creatable even with space to spare",
            ),
        }],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Absolute(5.0),
        // 余裕の減少を見る指標。
        shift_direction: DeviationInterest::Upper,
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
        label: text!(ja: "使用中ファイル記述子数", en: "File descriptors in use"),
        fixed: &[],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Relative {
            fraction: 0.5,
            floor: 256.0,
        },
        // 占有の指標。減るのは解放された状態。
        shift_direction: DeviationInterest::Upper,
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
        label: text!(ja: "コンテキストスイッチ", en: "Context switches"),
        fixed: &[],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Relative {
            fraction: 1.0,
            floor: 1_000.0,
        },
        // 処理量の指標。**動いていたものが止まることは所見**なので両方向。
        // 最小有意変化量が「前窓の中央値の 100% かつ 1000 回/秒以上」なので、
        // 日次の緩やかな上下では立たない。
        shift_direction: DeviationInterest::Both,
        interpretations: I_CSWCH,
        not_established: NE_CSWCH,
    },
    CatalogEntry {
        activity: ActivityId::PCSW,
        column: "proc",
        scope: ItemScope::Single,
        kind: ValueKind::Counter,
        unit: Unit::CountPerSec,
        label: text!(ja: "プロセス生成", en: "Process creation"),
        fixed: &[],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Relative {
            fraction: 1.0,
            floor: 20.0,
        },
        // 処理量の指標。**生成が止まることは所見**なので両方向。
        shift_direction: DeviationInterest::Both,
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
        label: text!(ja: "CPU の待ち圧力 (some)", en: "CPU pressure (some)"),
        fixed: &[FixedCondition {
            id: "psi-cpu-some-stalled",
            comparison: FixedComparison::AtLeast,
            value: 20.0,
            pattern: Pattern::Sustained,
            min_samples: 2,
            priority: Priority::Watch,
            rationale: text!(
                ja: "一部のタスクが CPU を待っていた時間の割合が 20% 以上で続いた。\
                            PSI の some は「少なくとも一部のタスクが待たされていた時間の割合」\
                            (カーネル文書 accounting/psi.rst) で、この水準が続くのは\
                            待ちが常態化していることを示す",
                en: "The share of time some tasks were waiting for CPU stayed at or above 20%. PSI's some is the \
                 share of time at least some tasks were stalled (accounting/psi.rst), and staying at \
                 this level means the waiting has become the normal state",
            ),
        }],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Absolute(10.0),
        // 圧力の指標。低下は待ちが解けた状態。
        shift_direction: DeviationInterest::Upper,
        interpretations: I_PSI_CPU,
        not_established: NE_PSI_CPU,
    },
    CatalogEntry {
        activity: ActivityId::PSI_IO,
        column: "sio",
        scope: ItemScope::Single,
        kind: ValueKind::Counter,
        unit: Unit::Percent,
        label: text!(ja: "I/O の待ち圧力 (some)", en: "I/O pressure (some)"),
        fixed: &[],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Absolute(10.0),
        // 圧力の指標。
        shift_direction: DeviationInterest::Upper,
        interpretations: I_PSI_IO,
        not_established: NE_PSI_IO,
    },
    CatalogEntry {
        activity: ActivityId::PSI_IO,
        column: "fio",
        scope: ItemScope::Single,
        kind: ValueKind::Counter,
        unit: Unit::Percent,
        label: text!(ja: "I/O の待ち圧力 (full)", en: "I/O pressure (full)"),
        fixed: &[FixedCondition {
            id: "psi-io-full-stalled",
            comparison: FixedComparison::Above,
            value: 1.0,
            pattern: Pattern::Sustained,
            min_samples: 2,
            priority: Priority::Investigate,
            rationale: text!(
                ja: "全 non-idle タスクが同時に I/O 待ちで進めなかった時間の割合が\
                            1% 超で続いた。PSI の full は「全 non-idle タスクが同時に\
                            待たされていた時間の割合」(カーネル文書 accounting/psi.rst) で、\
                            待っていない実行可能タスクが 1 つも無かった状態を指す",
                en: "The share of time every non-idle task was stalled on I/O at once stayed above 1%. PSI's full \
                 is the share of time **all non-idle tasks** were stalled simultaneously \
                 (accounting/psi.rst), meaning not one runnable task was making progress",
            ),
        }],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Absolute(5.0),
        // 圧力の指標。
        shift_direction: DeviationInterest::Upper,
        interpretations: I_PSI_IO,
        not_established: NE_PSI_IO,
    },
    CatalogEntry {
        activity: ActivityId::PSI_MEM,
        column: "smem",
        scope: ItemScope::Single,
        kind: ValueKind::Counter,
        unit: Unit::Percent,
        label: text!(ja: "メモリの待ち圧力 (some)", en: "Memory pressure (some)"),
        fixed: &[],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Absolute(10.0),
        // 圧力の指標。
        shift_direction: DeviationInterest::Upper,
        interpretations: I_PSI_MEM,
        not_established: NE_PSI_MEM,
    },
    CatalogEntry {
        activity: ActivityId::PSI_MEM,
        column: "fmem",
        scope: ItemScope::Single,
        kind: ValueKind::Counter,
        unit: Unit::Percent,
        label: text!(ja: "メモリの待ち圧力 (full)", en: "Memory pressure (full)"),
        fixed: &[FixedCondition {
            id: "psi-mem-full-stalled",
            comparison: FixedComparison::Above,
            value: 1.0,
            pattern: Pattern::Sustained,
            min_samples: 2,
            priority: Priority::Investigate,
            rationale: text!(
                ja: "全 non-idle タスクが同時にメモリ回収待ちで進めなかった時間の割合が\
                            1% 超で続いた。PSI の full は「全 non-idle タスクが同時に\
                            待たされていた時間の割合」(カーネル文書 accounting/psi.rst) で、\
                            カーネル文書はこの状態を thrashing として扱っている",
                en: "The share of time every non-idle task was stalled on memory reclaim at once stayed above 1%. \
                 PSI's full is the share of time **all non-idle tasks** were stalled simultaneously \
                 (accounting/psi.rst), and the kernel documentation treats this state as thrashing",
            ),
        }],
        deviation: DeviationInterest::Upper,
        shift: ShiftMagnitude::Absolute(5.0),
        // 圧力の指標。
        shift_direction: DeviationInterest::Upper,
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

    /// **rationale の境界表現が、実装している比較演算子と一致すること。**
    ///
    /// 「5% を下回る」と書いて比較が `AtMost` (5% 以下) だと、同じデータを
    /// 人と AI が再判定したときに境界上の値の扱いが食い違う。
    /// 演算子から組んだ文言 ([`boundary_phrase`]) を rationale に必ず含める。
    #[test]
    fn rationales_state_the_boundary_that_the_comparison_implements() {
        // **両方の言語で確かめる。** 片方だけ守っても、もう片方を読む人には
        // 境界上の値の扱いが伝わらない。
        for lang in [Lang::Ja, Lang::En] {
            for e in CATALOG {
                for f in e.fixed {
                    let phrase = boundary_phrase(e.unit, f, lang);
                    assert!(
                        f.rationale.get(lang).contains(&phrase),
                        "{} ({}, {:?}): rationale に境界の文言 `{}` が無い。\
                         比較演算子と文言が食い違うと境界上の値の扱いが読み手に伝わらない\n{}",
                        e.display(),
                        f.id,
                        lang,
                        phrase,
                        f.rationale.get(lang)
                    );
                }
            }
        }
    }

    #[test]
    fn complementary_free_percentages_include_the_threshold_boundary() {
        for (id, free) in [
            ("memory-available-scarce", 3.0),
            ("filesystem-nearly-full", 5.0),
            ("filesystem-inodes-nearly-exhausted", 5.0),
        ] {
            let condition = CATALOG
                .iter()
                .flat_map(|e| e.fixed)
                .find(|f| f.id == id)
                .unwrap();
            assert!(condition.comparison.holds(condition.value, condition.value));
            assert_eq!(100.0 - condition.value, free);
            assert!(
                condition
                    .rationale
                    .get(Lang::Ja)
                    .contains(&format!("{free}% 以下"))
            );
            assert!(
                !condition
                    .rationale
                    .get(Lang::Ja)
                    .contains(&format!("{free}% 未満"))
            );
        }
    }

    #[test]
    fn boundary_phrase_follows_the_unit_and_the_comparison() {
        let at_most = FixedCondition {
            id: "t",
            comparison: FixedComparison::AtMost,
            value: 5.0,
            pattern: Pattern::Sustained,
            min_samples: 1,
            priority: Priority::Watch,
            rationale: text!(ja: "", en: ""),
        };
        assert_eq!(
            boundary_phrase(Unit::Percent, &at_most, Lang::Ja),
            "5% 以下"
        );
        let above = FixedCondition {
            comparison: FixedComparison::Above,
            value: 0.0,
            ..at_most
        };
        // 単位表記を持たない系列では数値だけを書く
        assert_eq!(boundary_phrase(Unit::CountPerSec, &above, Lang::Ja), "0 超");
        let at_least = FixedCondition {
            comparison: FixedComparison::AtLeast,
            value: 100.0,
            ..at_most
        };
        // 記号でない単位は 1 つ空ける
        assert_eq!(
            boundary_phrase(Unit::Milliseconds, &at_least, Lang::Ja),
            "100 ms 以上"
        );
    }

    /// 固定条件は観測の形までしか宣言しないこと。
    ///
    /// `%idle` が低い / `%util` が高いことに [`Pattern::Saturation`] (「飽和」) を
    /// 付けると、headline がこの系列では確かめられない解釈を断定する
    /// (`%idle` は I/O 待ちの idle を含まない割合、`%util` は同時実行数を
    /// 含まない稼働時間の割合にすぎない)。
    #[test]
    fn no_fixed_condition_claims_saturation() {
        for e in CATALOG {
            for f in e.fixed {
                assert_ne!(
                    f.pattern,
                    Pattern::Saturation,
                    "{} ({}): 固定条件に「飽和」を宣言してはいけない。\
                     観測の形 (Sustained / Depletion / Emergence) を使う",
                    e.display(),
                    f.id
                );
            }
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
        assert!(ItemScope::Aggregate.matches(ActivityId::CPU, "all"));
        assert!(!ItemScope::Aggregate.matches(ActivityId::CPU, "cpu0"));
        assert!(ItemScope::Each.matches(ActivityId::DISK, "dev8-0"));
        assert!(
            !ItemScope::Each.matches(ActivityId::CPU, "all"),
            "集約行を二重に数えない"
        );
        assert!(
            !ItemScope::Each.matches(ActivityId::IRQ, "sum"),
            "割り込みの総和も集約行"
        );
        assert!(ItemScope::Single.matches(ActivityId::MEMORY, SINGLE_ITEM));
        assert!(!ItemScope::Single.matches(ActivityId::DISK, "dev8-0"));
    }

    /// **回帰テスト**: 集約行を持たない activity では、`all` / `sum` という名前の
    /// item も個別 item として数える。
    ///
    /// 以前はラベルの文字列だけで集約行を判定しており、`all` という名前の
    /// インターフェースや `sum` という名前のファイルシステムが異変検出の対象から
    /// 黙って外れていた (評価していないのに「検出なし」になる)。
    #[test]
    fn items_named_like_an_aggregate_are_individual_items_without_one() {
        for activity in [
            ActivityId::DISK,
            ActivityId::NET_DEV,
            ActivityId::NET_EDEV,
            ActivityId::FS,
        ] {
            for name in ["all", "sum"] {
                assert!(
                    ItemScope::Each.matches(activity, name),
                    "{activity:?} の {name}"
                );
                assert!(
                    !ItemScope::Aggregate.matches(activity, name),
                    "{activity:?} に集約行は無い"
                );
            }
        }
        let key = MetricKey::new(ActivityId::NET_DEV, "all", "rx_bytes_per_sec");
        assert!(
            lookup(&key).is_some(),
            "all という名前のインターフェースも検出の対象"
        );
    }

    /// `all` という名前のインターフェースは、カタログ検索だけでなく
    /// `detect` の評価にも入力に存在した系列として残る。
    #[test]
    fn detect_evaluates_an_interface_named_all() {
        use crate::analyze::timeline::{MetricPoint, Timelines};

        let key = MetricKey::new(ActivityId::NET_DEV, "all", "rx_bytes_per_sec");
        let entry = lookup(&key).expect("カタログ項目");
        let mut timelines = Timelines::new();
        let t = timelines.entry(key, entry.unit, entry.kind);
        for i in 0..10u64 {
            t.push(MetricPoint::observed(
                1_000 + i * 10,
                1_010 + i * 10,
                1_000,
                100.0,
            ));
        }
        let out = crate::detect::detect(&timelines, &crate::detect::DetectOptions::default());
        let eval = out
            .evaluations
            .iter()
            .find(|e| {
                e.series.activity == ActivityId::NET_DEV
                    && e.series.item == "all"
                    && e.series.column == "rx_bytes_per_sec"
            })
            .expect("評価の記録がある");
        assert!(eval.present, "入力にある系列として評価する");
        assert_eq!(eval.observed_samples, 10);
    }

    /// 集約行だけを見る宣言は、集約行を持つ activity にしか置けない。
    ///
    /// 入力に現れなかったときの表示 (`placeholder`) も集約行のラベルと一致させる。
    #[test]
    fn aggregate_scope_is_declared_only_where_an_aggregate_row_exists() {
        for e in CATALOG.iter().filter(|e| e.scope == ItemScope::Aggregate) {
            assert_eq!(
                total_item_label(e.activity),
                Some(e.scope.placeholder()),
                "{}",
                e.display()
            );
        }
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
    ///
    /// `%memused` はこの一覧から外した。**割合**の系列であり
    /// (`100 × (総量 − 利用可能) / 総量`)、搭載量で割った値だからである。
    /// `kbavail` は kB の絶対量なので残す。
    #[test]
    fn context_dependent_metrics_have_no_fixed_condition() {
        for (activity, column) in [
            (ActivityId::QUEUE, "runq_sz"),
            (ActivityId::QUEUE, "ldavg_1"),
            (ActivityId::QUEUE, "ldavg_15"),
            (ActivityId::MEMORY, "kbavail"),
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

    /// **処理量・稼働の指標は水準変化を両方向で見ること。**
    ///
    /// 一定の転送・処理がゼロへ落ちることはサービス停止に対応する。
    /// 逸脱の宣言 (`Upper`) を水準変化にも流用すると、その方向を
    /// **判定前に捨てる**ことになる (Issue #5 の 20)。
    #[test]
    fn throughput_metrics_watch_both_shift_directions() {
        for (activity, column) in [
            (ActivityId::NET_DEV, "rx_bytes_per_sec"),
            (ActivityId::NET_DEV, "tx_bytes_per_sec"),
            (ActivityId::IO, "tps"),
            (ActivityId::DISK, "tps"),
            (ActivityId::DISK, "util_pct"),
            (ActivityId::CPU, "idle"),
            (ActivityId::PCSW, "cswch"),
            (ActivityId::PCSW, "proc"),
            (ActivityId::NET_SOCK, "totsck"),
        ] {
            let e = CATALOG
                .iter()
                .find(|e| e.activity == activity && e.column == column)
                .expect("カタログ項目");
            assert_eq!(
                e.shift_interest(),
                DeviationInterest::Both,
                "{}: 処理量・稼働の指標は停止も所見なので両方向で見る",
                e.display()
            );
        }
    }

    /// 圧力・滞留の指標は上方向だけを見ること。
    ///
    /// 下がるのは圧力が抜けた状態で、それ自体は所見にならない。
    /// 無条件に両方向にすると夜間に負荷が抜けるだけで毎日鳴る。
    #[test]
    fn pressure_metrics_watch_only_the_rising_shift() {
        for (activity, column) in [
            (ActivityId::CPU, "iowait"),
            (ActivityId::CPU, "steal"),
            (ActivityId::QUEUE, "runq_sz"),
            (ActivityId::QUEUE, "ldavg_15"),
            (ActivityId::MEMORY, "memused_pct"),
            (ActivityId::MEMORY, "swpused_pct"),
            (ActivityId::DISK, "await"),
            (ActivityId::DISK, "avg_queue_size"),
            (ActivityId::NET_DEV, "ifutil_pct"),
            (ActivityId::FS, "fs_used_pct"),
            (ActivityId::PSI_IO, "fio"),
            (ActivityId::PSI_MEM, "fmem"),
        ] {
            let e = CATALOG
                .iter()
                .find(|e| e.activity == activity && e.column == column)
                .expect("カタログ項目");
            assert_eq!(
                e.shift_interest(),
                DeviationInterest::Upper,
                "{}: 圧力の指標の低下は所見にしない",
                e.display()
            );
        }
    }

    /// 余裕の指標は下方向だけを見ること。
    #[test]
    fn headroom_metrics_watch_only_the_falling_shift() {
        let e = CATALOG
            .iter()
            .find(|e| e.activity == ActivityId::MEMORY && e.column == "kbavail")
            .expect("カタログ項目");
        assert_eq!(e.shift_interest(), DeviationInterest::Lower);
    }

    /// 水準変化の方向を宣言しない項目を作らないこと。
    ///
    /// [`DeviationInterest::None`] は「水準変化を見ない」という宣言になる。
    /// 発生自体が事象の系列は [`ShiftMagnitude::NotEvaluated`] 側で
    /// 経路を降りるので、方向を `None` にする理由が無い。
    #[test]
    fn every_entry_declares_a_shift_direction() {
        for e in CATALOG {
            assert!(
                !e.shift_interest().is_none(),
                "{}: 水準変化の方向を宣言していない",
                e.display()
            );
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

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
use crate::analyze::timeline::{MetricKey, SINGLE_ITEM};
use crate::detect::{FixedComparison, Pattern, ShiftDirection};
use crate::model::{ActivityId, Unit, ValueKind};

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
    ///
    /// [`boundary_phrase`] が作る境界の文言を必ず含めること
    /// (テストで機械的に照合している)。解釈はここに書かず
    /// [`CatalogEntry::interpretations`] へ回す。
    pub rationale: &'static str,
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
pub fn boundary_phrase(unit: Unit, condition: &FixedCondition) -> String {
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
    format!(
        "{value}{sep}{suffix} {}",
        condition.comparison.label().trim()
    )
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
    /// **水準変化**で関心のある方向。逸脱 ([`Self::deviation`]) とは別の宣言。
    ///
    /// 判断の基準は [`CatalogEntry::shift_interest`] の doc にある。
    pub shift_direction: DeviationInterest,
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

// `%idle` は「CPU が働いていた割合」の裏返しではない。
// `sar(1)`: `%idle` = 「CPU が idle で、**未完了のディスク I/O 要求が無かった**
// 時間の割合」、`%iowait` = 「CPU が idle で、未完了のディスク I/O 要求が
// **あった**時間の割合」。**どちらも CPU は idle** である。
// したがって `%idle` が 0 でも、その時間が `%iowait` に寄っているだけで
// CPU は何も実行していない、という状態があり得る。
const I_CPU_IDLE: &[&str] = &[
    "実行時間 (%user + %nice + %system) が増え、CPU 能力が要求に対して不足している",
    "単一のプロセスが CPU を占有している",
    "意図的に CPU を使い切るバッチ処理が動いていた",
    "idle 時間が I/O 完了待ち (%iowait) に寄っており、CPU は実行していない",
    "仮想化環境で実行を待たされている (%steal に寄っている)",
    "水準が上がった場合は負荷源が消えた (処理の完了とサービスの停止を区別できない)",
];
const NE_CPU_IDLE: &[&str] = &[
    "CPU 能力の不足。sar(1) の %idle は「未完了ディスク I/O が無い idle 時間」、\
     %iowait は「未完了ディスク I/O がある idle 時間」で**どちらも CPU は idle** なので、\
     %idle が低いことからは進めない。%idle=0 かつ %iowait=99 もこの条件を満たす",
    "実行時間の割合 (%user + %nice + %system)。複数列の和を条件にする仕組みが無く、\
     このカタログは合成列を持たない",
    "どのプロセスが CPU を使っていたか (sa ファイルにプロセス別の内訳は無い)",
    "処理が遅延したかどうか (応答時間は観測していない)",
];

// `%iowait` は「CPU が idle で、未完了のディスク I/O 要求があった時間」の割合
// (`sar(1)`)。**CPU が I/O のために働いている時間ではない。**
// 分母は tick 合計なので、CPU が空いていれば同じ I/O 量でも割合は大きく見える。
const I_IOWAIT: &[&str] = &[
    "ストレージの応答が遅い",
    "I/O 要求が多い (正常な負荷でも上がる)",
    "CPU が空いているため待ち時間が相対的に大きく見えている",
];
const NE_IOWAIT: &[&str] = &[
    "ストレージ障害の有無 (%iowait だけでは判定できない。デバイス別の await / %util と併せて見る)",
    "CPU の忙しさ。%iowait の時間は CPU が idle だった時間であり、\
     %idle と足して「空いていた割合」になる",
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

// この実装の `%memused` は `100 × (tlmkb − availablekb) / tlmkb`
// (`series::compute` の `memory_derived`)。`availablekb` は `/proc/meminfo` の
// `MemAvailable` で、カーネル文書 (`filesystems/proc.rst`) では
// 「swapping なしで新しいアプリケーションを起動するのに使える量の推定値。
// MemFree・SReclaimable・file LRU の大きさと各 zone の low watermark から計算する」
// と定義されている。**回収可能なページキャッシュは既に差し引かれている**ので、
// 「ページキャッシュを含むから高くても問題ない」は**この列には当てはまらない**。
const I_MEMUSED: &[&str] = &[
    "割り当て済み (回収できない) メモリが増えた",
    "回収できないページ (tmpfs・カーネルスラブ・mlock) が増えた",
    "ページキャッシュのうち回収できると見積もられない分が増えた",
];
const NE_MEMUSED: &[&str] = &[
    "OOM Killer が動いたか (sa ファイルに記録は無い)",
    "割り当てが実際に待たされたか。待ちの有無は PSI memory (some / full) と \
     pgscand/s が示すが、どちらも採取されていない世代がある",
    "97% という境界は運用上の設定値であり、カーネルがこの比率で挙動を変えるわけではない。\
     搭載量が大きいホストでは残り 3% が絶対量としては十分な場合がある",
    "旧世代 (availablekb 非搭載) の値。本家互換出力は kbmemfree で代用するが、\
     その値は回収可能なページキャッシュを含み意味が変わる。検出経路は厳密モードで\
     読むため代用せず、評価不能として報告する",
];

const I_COMMIT: &[&str] = &["割り当てを約束したメモリ量が増えた"];
const NE_COMMIT: &[&str] =
    &["%commit が 100 を超えること自体は異常ではない (overcommit は既定で許可される)"];

// Linux は不足時にだけ退避するのではない。`vm.swappiness` (既定 60、
// カーネル文書 `admin-guide/sysctl/vm.rst`) は「swap とファイルページングの
// 相対 I/O コスト」の設定で、0 でない既定構成では**不足が無くても**
// 使われないページが退避される。したがって「使用中」は背景情報であり、
// それ自体が調査の理由にはならない。
const I_SWAP_SPACE: &[&str] = &[
    "swappiness の設定に従って使われないページが退避された (通常動作)",
    "過去にメモリ不足があり、退避したページが残っている",
    "現在もメモリが不足している",
];
const NE_SWAP_SPACE: &[&str] = &[
    "現在のメモリ不足。退避済みページは読み戻されるまで残るので、\
     使用率からは過去の痕跡と現在の不足を区別できない",
    "この 1% は運用上の設定値であり、普遍的な意味を持つ境界ではない。\
     swappiness とワークロードによって通常運用の水準が変わる",
    "スワップ未構成のホストでの扱い。総量が 0 のとき使用率は 0% として計算されるため\
     (`series::compute` の `swpused_pct`)、この条件は成立しないが\
     「未構成なので適用対象外」とは報告されない",
];

const I_SWAP_IO: &[&str] = &[
    "長時間使われないページを退避しているだけ (swappiness による通常動作)",
    "メモリ不足でページの追い出し・読み戻しが起きている",
];
const NE_SWAP_IO: &[&str] = &[
    "スワップ発生が性能低下を招いたか (遅延は観測していない)",
    "メモリ不足かどうか。swappiness が 0 でない既定構成では不足が無くても発生する",
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

// `sar(1)`: `%util` = 「デバイスへ I/O 要求が発行されていた経過時間の割合
// (デバイスの帯域利用率)」。**要求を何本同時に処理していたかは入っていない**ので、
// 並列に処理するデバイス (NVMe・RAID・SSD) では 100% でも余力があり得る。
// 本家の man も「such as RAID arrays and modern SSDs, this number does not
// reflect their performance limits」と注記している。
const I_DISK_UTIL: &[&str] = &[
    "デバイスへの要求が処理能力に達している",
    "逐次 I/O でデバイスを使い切っている (正常な高スループット)",
    "要求を並列に処理するデバイスで、稼働時間が長くても余力が残っている",
    "水準が下がった場合は I/O を出していた処理が止まった (完了か停止かは区別できない)",
];
const NE_DISK_UTIL: &[&str] = &[
    "処理能力の飽和。%util は「要求が 1 つ以上あった時間の割合」で同時実行数を含まないため、\
     複数キューのデバイス (NVMe・RAID) では 100% でも飽和を意味しない",
    "残っている余力の量 (キュー深度・並列度はこの列からは分からない)",
    "デバイス名は major/minor から組んだ表記であり、OS 上の名前とは異なる場合がある",
];

// `sar(1)`: `await` = 「デバイスへ発行された I/O 要求が処理されるまでの平均時間
// (ミリ秒)。**キューで待った時間とサービスに要した時間の両方を含む**」。
// 大きいことは「遅かった」までしか示さず、滞留 (キュー待ち) と
// サービス時間の長さ (大きな要求・低速デバイス) を分けられない。
const I_DISK_AWAIT: &[&str] = &[
    "デバイスの応答が遅い",
    "キューに要求が滞留している",
    "要求サイズが大きく 1 要求あたりの時間が伸びている",
];
const NE_DISK_AWAIT: &[&str] = &[
    "滞留かサービス時間か。await はキュー待ち時間とサービス時間の合計なので、\
     値の大きさだけでは切り分けられない (aqu-sz / areq-sz と併せる)",
    "デバイス障害と輻輳の区別",
    "アプリケーションから見た遅延 (await はブロック層の値)",
];

const I_DISK_LOAD: &[&str] = &[
    "I/O 要求が増えた",
    "書き込みフラッシュが集中した",
    "水準が下がった場合は I/O を出していた処理が止まった (完了か停止かは区別できない)",
];
const NE_DISK_LOAD: &[&str] = &[
    "どのプロセスの I/O か (内訳は記録されていない)",
    "転送数が減った理由 (処理の完了・停止・上流の詰まりを区別できない)",
];

const I_NET_TP: &[&str] = &[
    "転送量が増えた",
    "バックアップ・レプリケーションが動いた",
    "水準が下がった場合は通信が止まった (処理の完了・上流の停止・経路障害を区別できない)",
];
const NE_NET_TP: &[&str] = &[
    "相手先・プロトコルの内訳 (記録されていない)",
    "転送量が減った理由 (このインターフェースの統計だけでは断定できない)",
];

const I_NET_UTIL: &[&str] = &["リンク帯域を使い切っている"];
const NE_NET_UTIL: &[&str] = &[
    "%ifutil はインターフェースの申告速度 (`speed`、Mbit/s) を分母にする。\
     速度が 0 = 不明のインターフェース (仮想デバイス・ethtool が返さない NIC) では\
     分母が無いので**値を作らず評価不能として報告する**",
    "全二重では受信・送信の**大きい方**だけを見る (sar(1))。逆方向の余裕は分からない",
];

// `rx_errors` / `tx_errors` (カーネル文書 `networking/statistics.rst`) は
// 「受信した不良パケットの総数」「送信時の問題の総数」で、下位カウンタ
// (crc / frame / carrier / fifo など) を束ねた値である。内訳は sa ファイルに無い。
const I_NET_ERR: &[&str] = &[
    "リンク品質・ケーブル・対向機器に問題がある",
    "デバイスの FIFO が溢れている (負荷起因)",
];
const NE_NET_ERR: &[&str] = &[
    "どのエラーがどの層で起きたか。rx_errors / tx_errors は下位カウンタを束ねた総数で、\
     内訳 (crc / frame / carrier / fifo) は記録されていない",
];

// `/proc/net/dev` の drop 列は**キュー溢れ専用のカウンタではない**。
// カーネル文書 (`networking/statistics.rst`) の `rx_dropped` は
// 「受信したが処理されなかったパケットの数。例えば資源不足や**未対応プロトコル**による。
// ハードウェアインターフェースではこのカウンタは **L2 アドレスフィルタで破棄された
// パケットを含み得る**」。さらに `rx_missed_errors` (「ホストが取りこぼしたパケット」) は
// 「procfs では drop カウンタに畳み込まれる」ので、この列は 2 つの合算である。
const I_NET_RX_DROP: &[&str] = &[
    "未対応のプロトコル・VLAN タグのパケットを受け取った (処理されないのが正常)",
    "受信キュー・ソケットバッファが溢れた",
    "L2 アドレスフィルタで破棄された",
    "ホストが取りこぼした (rx_missed_errors 分。procfs が drop に畳み込む)",
];
const NE_NET_RX_DROP: &[&str] = &[
    "破棄の原因。procfs の drop 列は資源不足・未対応プロトコル・L2 フィルタ・\
     rx_missed_errors を合算した値で、内訳は sa ファイルからは分けられない",
    "運用上許容できる件数・割合。受信パケット数に対する比率と、\
     そのホストで通常どれだけ計上されるかを別に決める必要がある",
    "通信品質への影響 (破棄されたパケットが再送されたかは観測していない)",
];

// `tx_dropped` = 「送信に向かう途中で破棄されたパケットの数。例えば資源不足による」
// (カーネル文書 `networking/statistics.rst`)。
const I_NET_TX_DROP: &[&str] = &[
    "送信キューの資源が不足した",
    "デバイスが停止している間に送信しようとした",
];
const NE_NET_TX_DROP: &[&str] = &[
    "破棄の原因 (tx_dropped は「送信に向かう途中の資源不足等」を束ねた値)",
    "運用上許容できる件数・割合",
];

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

// PSI (カーネル文書 `accounting/psi.rst`): some = 「少なくとも一部のタスクが
// 待たされていた時間の割合」、full = 「**全 non-idle タスク**が同時に
// 待たされていた時間の割合」。full は「全タスク」ではない
// (待つべき仕事を持たないタスクは数に入らない)。
const I_PSI_CPU: &[&str] = &["実行可能タスクが CPU を待っている"];
const NE_PSI_CPU: &[&str] = &[
    "PSI は待ち時間の割合であり、待ったタスクの内訳は持たない",
    "待ちが応答時間に響いたか (遅延は観測していない)",
];

const I_PSI_IO: &[&str] = &["I/O 完了待ちで処理が止まっている"];
const NE_PSI_IO: &[&str] = &[
    "どのデバイス・どのプロセスが待ったか",
    "full が示すのは「全 non-idle タスクが同時に待った」ことであり、\
     待つ仕事を持たないタスクまで止まっていたことではない",
];

const I_PSI_MEM: &[&str] = &["メモリ回収待ちで処理が止まっている"];
const NE_PSI_MEM: &[&str] = &[
    "どのプロセスが待ったか",
    "full が示すのは「全 non-idle タスクが同時に待った」ことであり、\
     待つ仕事を持たないタスクまで止まっていたことではない",
];

const I_CSWCH: &[&str] = &[
    "実行するタスクが増えた",
    "ロック競合や短い待ちが頻発している",
    "割り込みが増えた",
    "水準が下がった場合は動いていたタスクが減った (処理の完了と停止を区別できない)",
];
const NE_CSWCH: &[&str] = &["適正値はワークロードに依存する。固定条件は置いていない"];

const I_PROC: &[&str] = &[
    "プロセス・スレッドの生成が増えた",
    "fork を多用する処理が動いた",
    "水準が下がった場合は生成していた処理が止まった (完了と停止を区別できない)",
];
const NE_PROC: &[&str] = &["生成されたプロセスの内容 (記録されていない)"];

const I_SOCK: &[&str] = &[
    "接続数が増えた",
    "短命な接続が大量に作られている (TIME_WAIT の滞留)",
    "水準が下がった場合は接続が切れた・受け付けが止まった",
];
const NE_SOCK: &[&str] = &[
    "接続先・ポートの内訳 (記録されていない)",
    "接続数が減った理由 (正常な終了と受け付け停止を区別できない)",
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
        label: "CPU の空き時間",
        fixed: &[FixedCondition {
            id: "cpu-idle-exhausted",
            comparison: FixedComparison::AtMost,
            value: 5.0,
            pattern: Pattern::Sustained,
            min_samples: 2,
            priority: Priority::Watch,
            rationale: "非 I/O 待ちの idle が 5% 以下で続いた。sar(1) の %idle は\
                        「CPU が idle で未完了のディスク I/O 要求が無かった時間」の割合で、\
                        搭載量やワークロードに依存せず意味が定まるのは**この観測まで**である。\
                        %iowait も idle 時間なので、ここから CPU 能力の不足には進めない",
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
        label: "I/O 待ちの CPU 時間",
        fixed: &[FixedCondition {
            id: "cpu-iowait-high",
            comparison: FixedComparison::AtLeast,
            value: 30.0,
            pattern: Pattern::Sustained,
            min_samples: 2,
            priority: Priority::Watch,
            rationale: "未完了のディスク I/O があった idle 時間の割合が 30% 以上で続いた。\
                        正常な大量 I/O でも上がるため、原因の特定には至らない",
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
        label: "奪われた CPU 時間",
        fixed: &[FixedCondition {
            id: "cpu-steal-present",
            comparison: FixedComparison::Above,
            value: 2.0,
            pattern: Pattern::Sustained,
            min_samples: 2,
            priority: Priority::Watch,
            rationale: "仮想化環境で実行を待たされた CPU 時間の割合が 2% 超で続いた。\
                        ゲスト内の対策では解消しないため、この水準でも報告する価値がある",
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
        label: "カーネルモードの CPU 時間",
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
        label: "実行待ちタスク数",
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
        label: "ブロックされたタスク数",
        fixed: &[FixedCondition {
            id: "queue-blocked-present",
            comparison: FixedComparison::Above,
            value: 0.0,
            pattern: Pattern::Emergence,
            min_samples: 3,
            priority: Priority::Watch,
            rationale: "I/O 完了待ちで走れないタスクの数が 0 超で続いた。\
                        瞬間値としては珍しくないが、採取をまたいで続けて観測されるのは\
                        待ちが常態化していることを示す",
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
        label: "1 分平均負荷",
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
        label: "15 分平均負荷",
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
        label: "利用可能メモリ",
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
        label: "メモリ使用率",
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
            rationale: "利用可能メモリが総量の 3% 以下 (%memused が 97% 以上) で続いた。\
                        この列の分子は総量 − MemAvailable で、MemAvailable は\
                        「swapping なしで新しいアプリケーションを起動するのに使える量の推定値」\
                        (カーネル文書 filesystems/proc.rst) なので、回収可能なページキャッシュは\
                        既に差し引かれている。**97% は運用上の設定値**であり、\
                        カーネルがこの比率で挙動を変えるわけではない",
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
        label: "約束済みメモリの比率",
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
        label: "スワップ使用率",
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
            rationale: "スワップ領域の使用率が 1% 超で続いた。**この 1% は運用上の設定値**で、\
                        普遍的な境界ではない。Linux は vm.swappiness (既定 60) に従って\
                        使われないページを退避するため、使用中であること自体は\
                        メモリ不足の証拠にならない。退避済みページは読み戻されるまで残る",
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
        label: "スワップイン",
        fixed: &[FixedCondition {
            id: "swap-in-occurred",
            comparison: FixedComparison::Above,
            value: 0.0,
            pattern: Pattern::Emergence,
            min_samples: 1,
            priority: Priority::Watch,
            rationale: "退避したページの読み戻しが 0 超で観測された。読み戻しは\
                        ページフォールトの待ちを伴うので、発生したこと自体に意味がある。\
                        ただし**スワップを構成したホストで 0 が常態とは限らない**ため、\
                        どれだけ続いたか・どれだけの量かを別に見る必要がある",
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
        label: "スワップアウト",
        fixed: &[FixedCondition {
            id: "swap-out-occurred",
            comparison: FixedComparison::Above,
            value: 0.0,
            pattern: Pattern::Emergence,
            min_samples: 1,
            priority: Priority::Informational,
            rationale: "ページの退避が 0 超で観測された。退避が起きた事実を示すが、\
                        vm.swappiness が 0 でない既定構成では**不足が無くても起きる**ため、\
                        単独では調査の理由にならない",
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
        label: "kswapd のページスキャン",
        fixed: &[FixedCondition {
            id: "page-reclaim-kswapd",
            comparison: FixedComparison::Above,
            value: 0.0,
            pattern: Pattern::Emergence,
            min_samples: 1,
            priority: Priority::Informational,
            rationale: "kswapd のページスキャンが 0 超で観測された。空きメモリが\
                        回収閾値を下回ったことを示すが、大量の逐次 I/O でも起きるため\
                        単独では負荷の証拠にならない",
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
        label: "direct reclaim のページスキャン",
        fixed: &[FixedCondition {
            id: "page-reclaim-direct",
            comparison: FixedComparison::Above,
            value: 0.0,
            pattern: Pattern::Emergence,
            min_samples: 1,
            priority: Priority::Investigate,
            rationale: "direct reclaim のページスキャンが 0 超で観測された。\
                        kswapd が追いつかずプロセス自身が回収していることを示し、\
                        割り当てを求めたプロセスはその場で待たされる",
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
        label: "メジャーフォールト",
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
        label: "ブロック I/O 転送数",
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
        label: "デバイス使用率",
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
            rationale: "I/O 要求が発行されていた経過時間の割合が 95% 以上で続いた。\
                        sar(1) の %util は「要求が 1 つ以上あった時間の割合」で\
                        **同時に何本処理していたかを含まない**ため、単一キューの\
                        デバイスでなければ処理能力の上限を示さない",
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
        label: "デバイス応答時間",
        fixed: &[FixedCondition {
            id: "disk-latency-high",
            comparison: FixedComparison::AtLeast,
            value: 100.0,
            pattern: Pattern::Sustained,
            min_samples: 2,
            priority: Priority::Watch,
            rationale: "1 要求あたりの平均時間が 100 ms 以上で続いた。回転ディスクの\
                        シーク時間と比べても大きい。ただし sar(1) の await は\
                        **キュー待ち時間とサービス時間の合計**なので、値の大きさだけでは\
                        要求の滞留とサービス時間の長さを区別できない",
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
        label: "デバイスキュー長",
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
        label: "デバイス転送数",
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
        label: "インターフェース使用率",
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
            rationale: "インターフェース使用率が 90% 以上で続いた。分母は申告速度\
                        (`speed`、Mbit/s) で、全二重では受信・送信の大きい方だけを見る。\
                        速度が 0 = 不明のインターフェースでは値を作らず評価不能として報告する",
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
        label: "受信スループット",
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
        label: "送信スループット",
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
        label: "受信エラー",
        fixed: &[FixedCondition {
            id: "net-rx-error-occurred",
            comparison: FixedComparison::Above,
            value: 0.0,
            pattern: Pattern::Emergence,
            min_samples: 1,
            priority: Priority::Watch,
            rationale: "受信エラーが 0 超で観測された。rx_errors は「受信した不良パケットの総数」\
                        (カーネル文書 networking/statistics.rst) で、正常なリンクでは増えない。\
                        0 でないこと自体が事象として意味を持つ",
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
        label: "送信エラー",
        fixed: &[FixedCondition {
            id: "net-tx-error-occurred",
            comparison: FixedComparison::Above,
            value: 0.0,
            pattern: Pattern::Emergence,
            min_samples: 1,
            priority: Priority::Watch,
            rationale: "送信エラーが 0 超で観測された。tx_errors は「送信時の問題の総数」\
                        (カーネル文書 networking/statistics.rst) で、正常なリンクでは増えない。\
                        0 でないこと自体が事象として意味を持つ",
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
        label: "受信パケットの破棄",
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
            rationale: "受信したが処理されなかったパケットの計上が 0 超で観測された。\
                        カーネル文書 (networking/statistics.rst) の rx_dropped は\
                        「資源不足や**未対応プロトコル**等で処理されなかったパケットの数」で、\
                        L2 アドレスフィルタによる破棄を含み得る。さらに procfs はホストの\
                        取りこぼし (rx_missed_errors) をこの列に畳み込む。\
                        **原因は特定できない**",
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
        label: "送信パケットの破棄",
        fixed: &[FixedCondition {
            id: "net-tx-drop-occurred",
            comparison: FixedComparison::Above,
            value: 0.0,
            pattern: Pattern::Emergence,
            min_samples: 1,
            priority: Priority::Watch,
            rationale: "送信に向かう途中で破棄されたパケットの計上が 0 超で観測された。\
                        カーネル文書 (networking/statistics.rst) の tx_dropped は\
                        「送信に向かう途中で破棄されたパケットの数。例えば資源不足による」で、\
                        送信側の資源不足を示すが**内訳は特定できない**",
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
        label: "使用中ソケット数",
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
        label: "TIME_WAIT のソケット数",
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
        label: "ファイルシステム使用率",
        fixed: &[FixedCondition {
            id: "filesystem-nearly-full",
            comparison: FixedComparison::AtLeast,
            value: 95.0,
            pattern: Pattern::Depletion,
            min_samples: 1,
            priority: Priority::Investigate,
            rationale: "使用率が 95% 以上になった (空き容量が 5% 以下)。予約ブロックと\
                        断片化により、この水準からは書き込み失敗が現実的になる",
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
        label: "inode 使用率",
        fixed: &[FixedCondition {
            id: "filesystem-inodes-nearly-exhausted",
            comparison: FixedComparison::AtLeast,
            value: 95.0,
            pattern: Pattern::Depletion,
            min_samples: 1,
            priority: Priority::Investigate,
            rationale: "inode 使用率が 95% 以上になった (残りが 5% 以下)。\
                        容量が空いていてもファイルを作れなくなる",
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
        label: "使用中ファイル記述子数",
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
        label: "コンテキストスイッチ",
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
        label: "プロセス生成",
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
        label: "CPU の待ち圧力 (some)",
        fixed: &[FixedCondition {
            id: "psi-cpu-some-stalled",
            comparison: FixedComparison::AtLeast,
            value: 20.0,
            pattern: Pattern::Sustained,
            min_samples: 2,
            priority: Priority::Watch,
            rationale: "一部のタスクが CPU を待っていた時間の割合が 20% 以上で続いた。\
                        PSI の some は「少なくとも一部のタスクが待たされていた時間の割合」\
                        (カーネル文書 accounting/psi.rst) で、この水準が続くのは\
                        待ちが常態化していることを示す",
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
        label: "I/O の待ち圧力 (some)",
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
        label: "I/O の待ち圧力 (full)",
        fixed: &[FixedCondition {
            id: "psi-io-full-stalled",
            comparison: FixedComparison::Above,
            value: 1.0,
            pattern: Pattern::Sustained,
            min_samples: 2,
            priority: Priority::Investigate,
            rationale: "全 non-idle タスクが同時に I/O 待ちで進めなかった時間の割合が\
                        1% 超で続いた。PSI の full は「全 non-idle タスクが同時に\
                        待たされていた時間の割合」(カーネル文書 accounting/psi.rst) で、\
                        待っていない実行可能タスクが 1 つも無かった状態を指す",
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
        label: "メモリの待ち圧力 (some)",
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
        label: "メモリの待ち圧力 (full)",
        fixed: &[FixedCondition {
            id: "psi-mem-full-stalled",
            comparison: FixedComparison::Above,
            value: 1.0,
            pattern: Pattern::Sustained,
            min_samples: 2,
            priority: Priority::Investigate,
            rationale: "全 non-idle タスクが同時にメモリ回収待ちで進めなかった時間の割合が\
                        1% 超で続いた。PSI の full は「全 non-idle タスクが同時に\
                        待たされていた時間の割合」(カーネル文書 accounting/psi.rst) で、\
                        カーネル文書はこの状態を thrashing として扱っている",
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
        for e in CATALOG {
            for f in e.fixed {
                let phrase = boundary_phrase(e.unit, f);
                assert!(
                    f.rationale.contains(&phrase),
                    "{} ({}): rationale に境界の文言 `{}` が無い。\
                     比較演算子と文言が食い違うと境界上の値の扱いが読み手に伝わらない\n{}",
                    e.display(),
                    f.id,
                    phrase,
                    f.rationale
                );
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
            assert!(condition.rationale.contains(&format!("{free}% 以下")));
            assert!(!condition.rationale.contains(&format!("{free}% 未満")));
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
            rationale: "",
        };
        assert_eq!(boundary_phrase(Unit::Percent, &at_most), "5% 以下");
        let above = FixedCondition {
            comparison: FixedComparison::Above,
            value: 0.0,
            ..at_most
        };
        // 単位表記を持たない系列では数値だけを書く
        assert_eq!(boundary_phrase(Unit::CountPerSec, &above), "0 超");
        let at_least = FixedCondition {
            comparison: FixedComparison::AtLeast,
            value: 100.0,
            ..at_most
        };
        // 記号でない単位は 1 つ空ける
        assert_eq!(
            boundary_phrase(Unit::Milliseconds, &at_least),
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

//! 列の表示値の計算。
//!
//! `sar` が表示する値は、生の累積カウンタから次のいずれかの方法で得られる。
//!
//! | 種別 | 計算 |
//! |---|---|
//! | 単一カウンタのレート | `S_VALUE(prev, curr, itv)` |
//! | CPU 割合 | per-CPU の tick 合計で正規化 (グローバル itv は使わない) |
//! | ゲージ | 差分化せず現在値をそのまま |
//! | 派生値 | 複数フィールドから専用計算 (`%memused`, `%util`, `areq-sz` など) |
//!
//! **出力層はこの層の結果を書式化するだけで、値を再計算しない。**
//! 同じ指標を `sar` 互換テキストと JSON で出したときに、
//! 形式ごとに計算がずれる事故を層の分離で防ぐ。
//!
//! ## 「保存値のスケーリング」もこの層の責務
//!
//! 本家は `pr_stats.c` の中で `cpufreq / 100.0` や `xds.util / 10.0` のような
//! スケーリングを行ってから `cprintf_*` に渡す。これは書式化ではなく計算なので、
//! reSARch では [`column_value`] の中で済ませる。出力層が受け取るのは
//! **常に `ColumnMeta::unit` が示す単位の値**である。
//!
//! 例外は「`--human` の有無で渡す値そのものが変わる」列
//! (`A_NET_DEV` の `rxkB/s` = バイト/秒、`A_FS` の `MBfsfree` = バイト) で、
//! ここでは `ColumnMeta::unit` どおりの値 (バイト / バイト毎秒) を返し、
//! 表示単位への換算は出力層の単位処理に委ねる (03 §1.8.1)。
//!
//! ## 差分は「元のフィールド幅」で取る
//!
//! レイアウト層は 4 バイトのフィールドも `u64` へゼロ拡張して渡してくる。
//! そのまま 64bit で引くと 32bit カウンタの一周を復元できず、
//! `4294967290 → 4` のような入力で差分が 1.84×10¹⁹ になる。
//! この層では [`DecodePlan::column_bits`] で列のカウンタ幅を引き、
//! [`wrapping_delta`] / [`s_value_bits`] に幅を渡す
//! (詳しい理由と本家との関係は [`super::delta`] のモジュールドキュメント)。
//!
//! ## 欠落の扱いは方針で分ける
//!
//! 「その世代のファイルに無いフィールド」を 0 とみなすと、
//! 総量から引く形の派生列が静かに嘘の値になる
//! (`%memused` = `(tlmkb - availablekb) / tlmkb` で `availablekb` を 0 にすると
//! 常に 100%)。一方で互換出力は**本家が表示した値**を出さなければならない。
//! そこで [`MissingPolicy`] で 2 つの方針を分ける。
//!
//! - [`MissingPolicy::Compat`] — [`column_value`]。本家の挙動 (0 埋め /
//!   世代別の代替フィールド) を再現する。`sar` / `sadf` 互換出力用。
//! - [`MissingPolicy::Strict`] — [`column_value_strict`]。欠落は
//!   [`ComputeIssue::UnsupportedBySource`] として返す。独自出力・集計用。
//!
//! 典拠: `docs/format/03-output-format.md` 第 I 部 §1.2〜§1.5、第 III 部 §7。

use super::delta::{
    Delta, DeltaContext, Discontinuity, compute_delta, s_value_bits, wrapping_delta,
};
use super::snapshot::ItemSnapshot;
use crate::layout::plan::DecodePlan;
use crate::layout::registry::ColumnMeta;
use crate::model::{ActivityId, Availability, CounterBits, ValueKind};

// ============================================================================
// 列インデックス定数
//
// 派生列の計算は「同じ activity の別の列」を参照する。列メタデータの宣言順は
// `layout::activities` で固定されているため、名前で引かずに添字で引く
// (ホットパスに文字列比較を置かない)。
// ============================================================================

/// `A_CPU` の列添字。
pub mod cpu_col {
    /// `%user` (`-u`)
    pub const USER: usize = 0;
    /// `%nice` (`-u`)
    pub const NICE: usize = 1;
    /// `%system` (`-u`、派生 = sys + hardirq + softirq)
    pub const SYSTEM: usize = 2;
    /// `%iowait`
    pub const IOWAIT: usize = 3;
    /// `%steal`
    pub const STEAL: usize = 4;
    /// `%idle`
    pub const IDLE: usize = 5;
    /// `%usr` (`-u ALL`、派生 = user - guest)
    pub const USR: usize = 6;
    /// `%nice` (`-u ALL`、派生 = nice - guest_nice)
    pub const NICE_EXCL_GNICE: usize = 7;
    /// `%sys` (`-u ALL`、hardirq/softirq を含まない)
    pub const SYS: usize = 8;
    /// `%irq`
    pub const IRQ: usize = 9;
    /// `%soft`
    pub const SOFT: usize = 10;
    /// `%guest`
    pub const GUEST: usize = 11;
    /// `%gnice`
    pub const GNICE: usize = 12;

    /// tick 合計に含める 8 列 (`guest` / `guest_nice` は `user` / `nice` に内包)。
    pub const TICK_FIELDS: [usize; 8] = [USER, NICE, SYS, IOWAIT, IDLE, STEAL, IRQ, SOFT];
}

/// `A_IRQ` の列添字。
pub mod irq_col {
    /// 割り込み名 (`INTR`)
    pub const NAME: usize = 0;
    /// 割り込み回数
    pub const COUNT: usize = 1;
}

/// `A_MEMORY` の列添字。
pub mod mem_col {
    pub const KBMEMFREE: usize = 0;
    pub const KBAVAIL: usize = 1;
    pub const KBMEMUSED: usize = 2;
    pub const MEMUSED_PCT: usize = 3;
    pub const KBBUFFERS: usize = 4;
    pub const KBCACHED: usize = 5;
    pub const KBCOMMIT: usize = 6;
    pub const COMMIT_PCT: usize = 7;
    pub const KBACTIVE: usize = 8;
    pub const KBINACT: usize = 9;
    pub const KBDIRTY: usize = 10;
    pub const KBSHMEM: usize = 11;
    pub const KBANONPG: usize = 12;
    pub const KBSLAB: usize = 13;
    pub const KBKSTACK: usize = 14;
    pub const KBPGTBL: usize = 15;
    pub const KBVMUSED: usize = 16;
    pub const KBMEMTOTAL: usize = 17;
    pub const KBSWPFREE: usize = 18;
    pub const KBSWPUSED: usize = 19;
    pub const SWPUSED_PCT: usize = 20;
    pub const KBSWPCAD: usize = 21;
    pub const SWPCAD_PCT: usize = 22;
    pub const KBSWPTOTAL: usize = 23;
}

/// `A_QUEUE` の列添字。
pub mod queue_col {
    pub const RUNQ_SZ: usize = 0;
    pub const PLIST_SZ: usize = 1;
    pub const LDAVG_1: usize = 2;
    pub const LDAVG_5: usize = 3;
    pub const LDAVG_15: usize = 4;
    pub const BLOCKED: usize = 5;
}

/// `A_HUGE` の列添字。
pub mod huge_col {
    pub const KBHUGFREE: usize = 0;
    pub const KBHUGUSED: usize = 1;
    pub const HUGUSED_PCT: usize = 2;
    pub const KBHUGRSVD: usize = 3;
    pub const KBHUGSURP: usize = 4;
    pub const KBHUGTOTAL: usize = 5;
}

/// `A_DISK` の列添字。
pub mod disk_col {
    pub const DEVICE: usize = 0;
    pub const TPS: usize = 1;
    pub const RKB: usize = 2;
    pub const WKB: usize = 3;
    pub const DKB: usize = 4;
    pub const AREQ_SZ: usize = 5;
    pub const AQU_SZ: usize = 6;
    pub const AWAIT: usize = 7;
    pub const UTIL_PCT: usize = 8;
    pub const RD_TICKS: usize = 9;
    pub const WR_TICKS: usize = 10;
    pub const DC_TICKS: usize = 11;
    pub const MAJOR: usize = 12;
    pub const MINOR: usize = 13;
    pub const WWN_HIGH: usize = 14;
    pub const WWN_LOW: usize = 15;
    pub const PART_NR: usize = 16;
}

/// `A_FS` の列添字。
pub mod fs_col {
    pub const FILESYSTEM: usize = 0;
    pub const MOUNTPOINT: usize = 1;
    pub const MB_FREE: usize = 2;
    pub const MB_USED: usize = 3;
    pub const USED_PCT: usize = 4;
    pub const UNPRIV_USED_PCT: usize = 5;
    pub const IFREE: usize = 6;
    pub const IUSED: usize = 7;
    pub const IUSED_PCT: usize = 8;
    pub const TOTAL: usize = 9;
    pub const AVAILABLE: usize = 10;
    pub const INODES_TOTAL: usize = 11;
}

/// `A_NET_DEV` の列添字。
pub mod net_dev_col {
    pub const IFACE: usize = 0;
    pub const RXPCK: usize = 1;
    pub const TXPCK: usize = 2;
    pub const RXKB: usize = 3;
    pub const TXKB: usize = 4;
    pub const RXCMP: usize = 5;
    pub const TXCMP: usize = 6;
    pub const RXMCST: usize = 7;
    pub const IFUTIL_PCT: usize = 8;
    pub const SPEED: usize = 9;
    pub const DUPLEX: usize = 10;
}

/// `A_NET_SOFT` の列添字。
pub mod soft_col {
    pub const CPU: usize = 0;
    pub const TOTAL: usize = 1;
    pub const DROPD: usize = 2;
    pub const SQUEEZD: usize = 3;
    pub const RX_RPS: usize = 4;
    pub const FLW_LIM: usize = 5;
    pub const BLG_LEN: usize = 6;
}

/// `A_PWR_CPU` の列添字。
pub mod pwr_cpu_col {
    pub const CPU: usize = 0;
    pub const MHZ: usize = 1;
}

/// `A_PWR_FAN` の列添字。
pub mod fan_col {
    pub const INDEX: usize = 0;
    pub const RPM: usize = 1;
    pub const DRPM: usize = 2;
    pub const RPM_MIN: usize = 3;
    pub const DEVICE: usize = 4;
}

/// `A_PWR_TEMP` の列添字。
pub mod temp_col {
    pub const INDEX: usize = 0;
    pub const DEGC: usize = 1;
    pub const PCT: usize = 2;
    pub const MIN: usize = 3;
    pub const MAX: usize = 4;
    pub const DEVICE: usize = 5;
}

/// `A_PWR_IN` の列添字。
pub mod in_col {
    pub const INDEX: usize = 0;
    pub const VOLTS: usize = 1;
    pub const PCT: usize = 2;
    pub const MIN: usize = 3;
    pub const MAX: usize = 4;
    pub const DEVICE: usize = 5;
}

/// `A_PWR_FREQ` の列添字。
pub mod freq_col {
    pub const CPU: usize = 0;
    pub const WGH_MHZ: usize = 1;
    pub const TIME_IN_STATE: usize = 2;
    pub const FREQ_KHZ: usize = 3;
}

/// `A_PWR_USB` の列添字。
pub mod usb_col {
    pub const BUS: usize = 0;
    pub const VENDOR_ID: usize = 1;
    pub const PRODUCT_ID: usize = 2;
    pub const MAX_POWER: usize = 3;
    pub const MANUFACTURER: usize = 4;
    pub const PRODUCT: usize = 5;
}

/// `A_PWR_BAT` の列添字。
pub mod bat_col {
    pub const ID: usize = 0;
    pub const CAP_PCT: usize = 1;
    pub const CAP_PER_MIN: usize = 2;
    pub const STATUS: usize = 3;
}

/// `A_PSI_*` の列添字。`A_PSI_CPU` は 0〜3 のみ。
pub mod psi_col {
    /// `%s*-10`
    pub const SOME_10: usize = 0;
    /// `%s*-60`
    pub const SOME_60: usize = 1;
    /// `%s*-300`
    pub const SOME_300: usize = 2;
    /// `%s*` (累積 µs からの再計算)
    pub const SOME_TOTAL: usize = 3;
    /// `%f*-10`
    pub const FULL_10: usize = 4;
    /// `%f*-60`
    pub const FULL_60: usize = 5;
    /// `%f*-300`
    pub const FULL_300: usize = 6;
    /// `%f*` (累積 µs からの再計算)
    pub const FULL_TOTAL: usize = 7;
}

/// `A_PWR_BAT` の `status` 値 (`sa.h` の `BAT_STS_*`)。
pub mod bat_status {
    pub const UNKNOWN: u64 = 0;
    pub const CHARGING: u64 = 1;
    pub const DISCHARGING: u64 = 2;
    pub const NOTCHARGING: u64 = 3;
    pub const FULL: u64 = 4;
}

/// `stats_net_dev.duplex` の値 (`rd_stats.h` の `C_DUPLEX_*`)。
pub mod duplex {
    pub const UNKNOWN: u64 = 0;
    pub const HALF: u64 = 1;
    pub const FULL: u64 = 2;
}

/// CPU カウンタ逆行の判定閾値 (`ULLONG_MAX - 0x7ffff`)。
///
/// 前値がほぼ u64 上限なら「オーバーフロー由来の逆行」であり CPU 復帰ではない、
/// というヒューリスティック (03 §1.3.3)。
const CPU_OVERFLOW_THRESHOLD: u64 = u64::MAX - 0x7_ffff;

// ============================================================================
// 計算文脈
// ============================================================================

/// 計算に必要な文脈。
#[derive(Debug, Clone, Copy)]
pub struct ComputeContext {
    /// 経過時間 (1/100 秒)。
    pub itv_cs: u64,
    /// この item の tick 合計差分 (`A_CPU` / `A_IRQ` など CPU 時間で正規化する activity 用)。
    ///
    /// `A_CPU` の割合はグローバル itv ではなく、その CPU の tick 合計で正規化する。
    pub tick_total: Option<u64>,
    /// 前サンプルとの連続性。RESTART を挟んだ場合や item が入れ替わった場合は `false`。
    pub continuous: bool,
    /// 前サンプルが存在するか。
    pub has_prev: bool,
    /// 集約 item (CPU `all` 行 / `A_IRQ` の合計列) か。
    ///
    /// `A_IRQ` は合計列のみ「割り込み総数が減ったら 0」というクランプを持つ
    /// (CPU がオフラインになると総数が減る)。個別 CPU 列にはクランプが無い
    /// (03 §id=3)。
    pub aggregate_item: bool,
}

impl ComputeContext {
    pub fn new(itv_cs: u64) -> Self {
        Self {
            itv_cs,
            tick_total: None,
            continuous: true,
            has_prev: true,
            aggregate_item: false,
        }
    }

    /// tick 合計を指定した文脈を作る (`A_CPU`)。
    pub fn with_tick_total(mut self, total: u64) -> Self {
        self.tick_total = Some(total);
        self
    }
}

/// 計算できなかった理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComputeIssue {
    /// その世代のファイルにフィールドが無い。
    UnsupportedBySource,
    /// レコードに値が無い。
    MissingInSample,
    /// 差分が取れない。
    Discontinuous(Discontinuity),
    /// 数値ではなく識別子の列 (デバイス名・CPU 番号など)。
    ///
    /// 生値が必要な場合は [`raw_column`] を使う。
    NotNumeric,
    /// 1 item だけでは計算できない列 (`A_PWR_FREQ` の `wghMHz`)。
    ///
    /// 行列型 activity の派生列は item 群全体を必要とするため、
    /// [`weighted_mhz`] のような専用関数から計算する。
    NeedsItemGroup,
    /// 派生列の計算がまだ実装されていない。
    NotImplemented,
}

/// 表示値。
pub type Computed = Result<f64, ComputeIssue>;

/// 「その世代のファイルに無い入力フィールド」の扱い方。
///
/// 派生列の入力が欠落したとき、互換出力と独自出力で求められるものが違う。
///
/// - 互換出力は**本家が表示した値**を出す必要がある。本家は期待する型別本数より
///   ファイル側が少なければ 0 埋めした構造体で計算する (03 §1.9-1)。
///   さらに旧形式の変換 (`sadf -c`) では、世代別に**代替フィールドを代入**する
///   ものもある (`availablekb` ← `frmkb`、02 §8)。
/// - 独自出力・集計では「欠落」と「0」を混同してはいけない (`docs/design.md` §4)。
///   欠落を 0 とみなした結果が値の意味を変える箇所では、計算せずに理由を返す。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MissingPolicy {
    /// 本家互換: その世代の `sar` が表示した値を再現する。
    #[default]
    Compat,
    /// 厳密: 欠落を代替値で埋めず、[`ComputeIssue`] として報告する。
    Strict,
}

// ============================================================================
// 生値アクセス
// ============================================================================

/// 列に対応する生の wire 値を取り出す。
///
/// 識別子列 (デバイス番号・バス番号・バッテリ ID など) や、
/// 派生計算の入力を取るために使う。
#[inline]
pub fn raw_column(
    plan: &DecodePlan,
    item: &ItemSnapshot,
    column: usize,
) -> Result<u64, ComputeIssue> {
    match plan.column_value(&item.values, column) {
        Availability::Present(v) => Ok(v),
        Availability::UnsupportedBySource => Err(ComputeIssue::UnsupportedBySource),
        Availability::MissingInSample => Err(ComputeIssue::MissingInSample),
    }
}

/// 生値を取り出す。その世代のファイルに無いフィールドは 0 とみなす。
///
/// 本家は「期待する型別本数よりファイル側が少なければ足りない分を 0 埋め」して
/// 構造体を組み立てる (03 §1.9-1)。したがって `discard` 統計を持たない世代の
/// `dc_sect` は本家でも 0 として `areq-sz` の分子に入る。
///
/// **使ってよいのは「0 を足しても値の意味が変わらない加算項」だけ**である。
/// 分子・分母・被減数のような主要項には [`primary_input`] を使う
/// (0 埋めが `%memused` = 100% のような嘘を作る)。
#[inline]
fn raw_or_zero(plan: &DecodePlan, item: &ItemSnapshot, column: usize) -> Result<u64, ComputeIssue> {
    match plan.column_value(&item.values, column) {
        Availability::Present(v) => Ok(v),
        Availability::UnsupportedBySource => Ok(0),
        Availability::MissingInSample => Err(ComputeIssue::MissingInSample),
    }
}

/// 派生計算の**主要項** (分子・分母・被減数) を取り出す。
///
/// 欠落を 0 で埋めると値そのものが嘘になる位置で使う。
/// 「総量 - 欠落」は総量に等しくなり使用率 100% を、
/// 「欠落 / 総量」は 0% を、静かに作り出す。
///
/// [`MissingPolicy::Compat`] では本家と同じ 0 埋めを行い、
/// [`MissingPolicy::Strict`] では計算せずに理由を返す。
#[inline]
fn primary_input(
    plan: &DecodePlan,
    item: &ItemSnapshot,
    column: usize,
    policy: MissingPolicy,
) -> Result<u64, ComputeIssue> {
    match plan.column_value(&item.values, column) {
        Availability::Present(v) => Ok(v),
        Availability::MissingInSample => Err(ComputeIssue::MissingInSample),
        Availability::UnsupportedBySource => match policy {
            // 本家は 0 埋めした構造体で計算する (03 §1.9-1)
            MissingPolicy::Compat => Ok(0),
            MissingPolicy::Strict => Err(ComputeIssue::UnsupportedBySource),
        },
    }
}

/// 列のカウンタ幅 (ラップの復元幅)。
///
/// このファイルに無い列は 64bit として扱う。その列は [`raw_column`] が
/// [`ComputeIssue::UnsupportedBySource`] を返すので差分計算まで到達しない
/// (幅の既定値が結果に影響することはない)。
#[inline]
fn counter_bits(plan: &DecodePlan, column: usize) -> CounterBits {
    plan.column_bits(column).unwrap_or(CounterBits::B64)
}

/// IEEE-754 の `double` として保存されているフィールドを読む。
///
/// `A_PWR_FAN` / `A_PWR_TEMP` / `A_PWR_IN` の値は C の `double` であり、
/// レイアウト層は 8 バイトを `u64` として読み出している。ビットパターンを
/// そのまま `f64` に読み替える (整数として解釈してはいけない)。
#[inline]
fn raw_f64(plan: &DecodePlan, item: &ItemSnapshot, column: usize) -> Computed {
    raw_column(plan, item, column).map(f64::from_bits)
}

/// 列の生値を書き戻す。
///
/// `get_per_cpu_interval()` は前サンプルの `iowait` / `idle` を破壊的に補正する。
/// CPU "all" の合算は**補正後の前値**を使うため、補正結果を保持する必要がある
/// (03 §1.3.3 / §1.4.3)。
#[inline]
fn set_column(plan: &DecodePlan, item: &mut ItemSnapshot, column: usize, value: u64) {
    if let Some(Some(id)) = plan.column_fields.get(column)
        && let Some(slot) = item.values.get_mut(id.index())
    {
        *slot = Availability::Present(value);
    }
}

/// `A_CPU` などで使う tick 合計差分を求める。
///
/// 全フィールドの差分を合計する。オフライン CPU は全フィールドが 0 になるため
/// 合計も 0 になり、そこから「この CPU は動いていない」と判定できる。
///
/// **`A_CPU` では [`per_cpu_interval`] を使うこと。** この関数は
/// 「全フィールドを単純に合計する」素朴な版で、`guest` の内包補正や
/// 逆行クランプを行わないため CPU 使用率の分母には使えない。
pub fn tick_total(prev: &ItemSnapshot, curr: &ItemSnapshot) -> u64 {
    curr.values
        .iter()
        .zip(prev.values.iter())
        .filter_map(|(c, p)| match (c, p) {
            (Availability::Present(c), Availability::Present(p)) => Some(c.wrapping_sub(*p)),
            _ => None,
        })
        .sum()
}

// ============================================================================
// 列の表示値
// ============================================================================

/// 1 列分の表示値を計算する (**本家互換**)。
///
/// 直接列 (単一の wire フィールドに対応) はここで計算する。
/// 派生列は activity 固有の計算が必要なため [`derived_value`] に委譲する。
///
/// 入力フィールドが「その世代のファイルに無い」場合は本家と同じ扱いをする
/// ([`MissingPolicy::Compat`])。欠落を欠落として受け取りたい場合は
/// [`column_value_strict`] を使う。
pub fn column_value(
    id: ActivityId,
    column: usize,
    meta: &ColumnMeta,
    plan: &DecodePlan,
    prev: &ItemSnapshot,
    curr: &ItemSnapshot,
    ctx: &ComputeContext,
) -> Computed {
    column_value_with(
        id,
        column,
        meta,
        plan,
        prev,
        curr,
        ctx,
        MissingPolicy::Compat,
    )
}

/// 1 列分の表示値を計算する (**欠落を埋めない**)。
///
/// 独自出力・集計はこちらを使う。その世代のファイルに入力フィールドが無く、
/// 0 で埋めると値の意味が変わる派生列は [`ComputeIssue::UnsupportedBySource`]
/// を返す (`docs/design.md` §4「欠落とゼロを混同しない」)。
///
/// 計算式自体は [`column_value`] と同一の実装を共有する。
/// 形式ごとに式が分岐するとこの層を置いた意味が無くなるため、
/// 分岐させるのは**欠落の埋め方だけ**に限定している。
pub fn column_value_strict(
    id: ActivityId,
    column: usize,
    meta: &ColumnMeta,
    plan: &DecodePlan,
    prev: &ItemSnapshot,
    curr: &ItemSnapshot,
    ctx: &ComputeContext,
) -> Computed {
    column_value_with(
        id,
        column,
        meta,
        plan,
        prev,
        curr,
        ctx,
        MissingPolicy::Strict,
    )
}

#[allow(clippy::too_many_arguments)]
fn column_value_with(
    id: ActivityId,
    column: usize,
    meta: &ColumnMeta,
    plan: &DecodePlan,
    prev: &ItemSnapshot,
    curr: &ItemSnapshot,
    ctx: &ComputeContext,
    policy: MissingPolicy,
) -> Computed {
    if !meta.is_direct() {
        return derived_value(id, column, meta, plan, prev, curr, ctx, policy);
    }

    // 保存形式が特殊な直接列を先に処理する
    if let Some(v) = special_direct(id, column, plan, prev, curr, ctx) {
        return v;
    }

    let curr_v = raw_column(plan, curr, column)?;

    match meta.kind {
        // ゲージは差分化しない
        ValueKind::Gauge => Ok(curr_v as f64 * gauge_scale(id, column)),
        // 識別子は数値として扱わない
        ValueKind::Identity => Err(ComputeIssue::NotNumeric),
        ValueKind::Counter => {
            if !ctx.has_prev {
                return Err(ComputeIssue::Discontinuous(Discontinuity::FirstSample));
            }
            if !ctx.continuous {
                return Err(ComputeIssue::Discontinuous(Discontinuity::Restart));
            }
            let prev_v = raw_column(plan, prev, column)?;

            // 逆行クランプ (`ll_sp_value` / 各 print 関数の明示クランプ)。
            // ラップの復元より先に判定する: クランプ対象の列は本家が
            // 「減っていたら 0」と決めているので、そこに合わせる。
            if curr_v < prev_v && clamps_decrease(id, column, ctx) {
                return Ok(0.0);
            }

            // 差分は列の元の幅で取る。32bit カウンタ (`unsigned int` のフィールドや
            // 32bit ライタの `unsigned long`) を 64bit のまま引くと、一周した入力で
            // 差分が 1.84e19 になる。本家も `unsigned int` は 32bit で引いている。
            let bits = counter_bits(plan, column);

            let delta = match policy {
                // 本家は符号なし減算の結果をそのまま表示する。幅で復元できない
                // 逆行 (64bit カウンタの巻き戻し) で巨大値が出るのも本家の値。
                MissingPolicy::Compat => wrapping_delta(prev_v, curr_v, bits),
                // 独自出力・集計では「一周として説明できる減少」だけを差分にする。
                // 説明できない減少に値を与えると、巨大な外れ値が平均や p95 を壊す。
                MissingPolicy::Strict => {
                    // 連続性はこの関数の入口で確認済み
                    match compute_delta(prev_v, curr_v, bits, DeltaContext::default()) {
                        Delta::Valid(d) | Delta::Wrapped(d) => d,
                        Delta::Unavailable(disc) => {
                            return Err(ComputeIssue::Discontinuous(disc));
                        }
                    }
                }
            };

            // `S_VALUE(m, n, p)` = `(n - m) / p * 100`。
            // CPU 時間で正規化する activity は itv ではなく tick 合計を分母にする。
            let denominator = match ctx.tick_total {
                Some(0) => {
                    return Err(ComputeIssue::Discontinuous(
                        Discontinuity::NonPositiveElapsed,
                    ));
                }
                Some(total) => total,
                None => ctx.itv_cs,
            };
            let rate = delta as f64 / denominator as f64 * 100.0;
            Ok(rate * counter_scale(id, column))
        }
    }
}

/// ゲージ列に掛けるスケール。
///
/// カーネル / sysstat が固定小数で保存している値を実単位に戻す。
fn gauge_scale(id: ActivityId, column: usize) -> f64 {
    match id {
        // load_avg_* は 100 倍固定小数 (03 §1.5.4)
        ActivityId::QUEUE
            if matches!(
                column,
                queue_col::LDAVG_1 | queue_col::LDAVG_5 | queue_col::LDAVG_15
            ) =>
        {
            0.01
        }
        // cpufreq は MHz × 100 (03 §id=30)
        ActivityId::PWR_CPU if column == pwr_cpu_col::MHZ => 0.01,
        // PSI の移動平均は 100 倍固定小数 (03 §1.5.3)
        ActivityId::PSI_CPU | ActivityId::PSI_IO | ActivityId::PSI_MEM
            if is_psi_moving_average(column) =>
        {
            0.01
        }
        // bMaxPower は 2 mA 単位 (03 §id=36)
        ActivityId::PWR_USB if column == usb_col::MAX_POWER => 2.0,
        _ => 1.0,
    }
}

/// カウンタ列のレートに掛けるスケール。
fn counter_scale(id: ActivityId, column: usize) -> f64 {
    match id {
        ActivityId::DISK => match column {
            // セクタ (512 B) → kB
            disk_col::RKB | disk_col::WKB | disk_col::DKB => 0.5,
            // rq_ticks は「I/O 待ちの重み付きミリ秒」。/1000 で平均キュー長
            disk_col::AQU_SZ => 0.001,
            // tot_ticks はミリ秒。1000 ms/s = 100% なので /10
            disk_col::UTIL_PCT => 0.1,
            _ => 1.0,
        },
        _ => 1.0,
    }
}

/// カウンタ逆行を 0.0 にクランプする列か。
fn clamps_decrease(id: ActivityId, column: usize, ctx: &ComputeContext) -> bool {
    match id {
        // CPU 系は全列が ll_sp_value を通る (03 §1.4.4)
        ActivityId::CPU => true,
        // A_IO は 7 列すべてに明示クランプ (03 §id=6)
        ActivityId::IO => true,
        // A_IRQ は合計列のみクランプ (03 §id=3)
        ActivityId::IRQ => column == irq_col::COUNT && ctx.aggregate_item,
        // %util のみ (compute_ext_disk_stats の util)
        ActivityId::DISK => column == disk_col::UTIL_PCT,
        // A_PAGE / SNMP 系にクランプは無い (負値がそのまま出る)
        _ => false,
    }
}

/// PSI の移動平均列 (`-10` / `-60` / `-300`) か。
fn is_psi_moving_average(column: usize) -> bool {
    matches!(
        column,
        psi_col::SOME_10
            | psi_col::SOME_60
            | psi_col::SOME_300
            | psi_col::FULL_10
            | psi_col::FULL_60
            | psi_col::FULL_300
    )
}

/// PSI の累積 µs 列 (`%scpu` / `%sio` / `%fio` / `%smem` / `%fmem`) か。
fn is_psi_total(column: usize) -> bool {
    matches!(column, psi_col::SOME_TOTAL | psi_col::FULL_TOTAL)
}

/// 保存形式が特殊で汎用経路に乗らない直接列。
///
/// `Some` を返した場合はそれが最終値。`None` なら汎用経路に進む。
fn special_direct(
    id: ActivityId,
    column: usize,
    plan: &DecodePlan,
    prev: &ItemSnapshot,
    curr: &ItemSnapshot,
    ctx: &ComputeContext,
) -> Option<Computed> {
    match id {
        // --- PSI: 累積マイクロ秒。S_VALUE は使わない (03 §1.5.3) ---
        ActivityId::PSI_CPU | ActivityId::PSI_IO | ActivityId::PSI_MEM if is_psi_total(column) => {
            Some(psi_pressure(plan, prev, curr, column, ctx))
        }
        // --- IEEE-754 double で保存されているセンサ値 ---
        ActivityId::PWR_FAN if matches!(column, fan_col::RPM | fan_col::RPM_MIN) => {
            Some(raw_f64(plan, curr, column))
        }
        ActivityId::PWR_TEMP
            if matches!(column, temp_col::DEGC | temp_col::MIN | temp_col::MAX) =>
        {
            Some(raw_f64(plan, curr, column))
        }
        ActivityId::PWR_IN if matches!(column, in_col::VOLTS | in_col::MIN | in_col::MAX) => {
            Some(raw_f64(plan, curr, column))
        }
        // --- A_PWR_BAT の capacity は signed char (03 §id=43) ---
        ActivityId::PWR_BAT if column == bat_col::CAP_PCT => {
            Some(raw_column(plan, curr, column).map(|v| f64::from(signed_byte(v))))
        }
        _ => None,
    }
}

/// PSI の圧力値。
///
/// `total` はマイクロ秒の累計、`itv` は 1/100 秒。
/// `Δµs / (itv/100 秒 × 1e6 µs/秒) × 100 [%] = Δµs / (itv × 100)`。
///
/// 本家は `((double) curr - prev) / (100 * itv)` と **f64 で減算**している
/// (`S_VALUE` の符号なし減算ではない)。計算順序もそのまま合わせる。
fn psi_pressure(
    plan: &DecodePlan,
    prev: &ItemSnapshot,
    curr: &ItemSnapshot,
    column: usize,
    ctx: &ComputeContext,
) -> Computed {
    if !ctx.has_prev {
        return Err(ComputeIssue::Discontinuous(Discontinuity::FirstSample));
    }
    if !ctx.continuous {
        return Err(ComputeIssue::Discontinuous(Discontinuity::Restart));
    }
    let c = raw_column(plan, curr, column)?;
    let p = raw_column(plan, prev, column)?;
    Ok((c as f64 - p as f64) / (100.0 * ctx.itv_cs as f64))
}

/// `signed char` として保存された値を復元する。
///
/// レイアウト層は 1 バイトをゼロ拡張して `u64` にしている。`A_PWR_BAT` の
/// `capacity` / `bat_id` は C の `char` (x86_64 Linux では符号付き) なので、
/// 差分計算の前に符号を戻す必要がある (03 §id=43)。
#[inline]
pub fn signed_byte(v: u64) -> i8 {
    v as u8 as i8
}

// ============================================================================
// 派生列
// ============================================================================

/// 派生列の計算。
///
/// activity 固有の計算式を実装する場所。未実装の列は
/// [`ComputeIssue::NotImplemented`] を返し、出力側で「未対応」と分かる形にする
/// (0 を返して正常値に見せてはいけない)。
///
/// `policy` は入力フィールドが欠落したときの扱い ([`MissingPolicy`])。
#[allow(clippy::too_many_arguments)]
pub fn derived_value(
    id: ActivityId,
    column: usize,
    meta: &ColumnMeta,
    plan: &DecodePlan,
    prev: &ItemSnapshot,
    curr: &ItemSnapshot,
    ctx: &ComputeContext,
    policy: MissingPolicy,
) -> Computed {
    // 識別子列 (デバイス名・CPU 番号など) は数値ではない
    if meta.kind == ValueKind::Identity {
        return Err(ComputeIssue::NotNumeric);
    }

    match id {
        ActivityId::CPU => cpu_derived(column, plan, prev, curr, ctx),
        ActivityId::MEMORY => memory_derived(column, plan, curr, policy),
        ActivityId::HUGE => huge_derived(column, plan, curr),
        ActivityId::DISK => disk_derived(column, plan, prev, curr, ctx),
        ActivityId::FS => fs_derived(column, plan, curr),
        ActivityId::NET_DEV => net_dev_derived(column, plan, prev, curr, ctx, policy),
        ActivityId::PWR_FAN => fan_derived(column, plan, curr),
        ActivityId::PWR_TEMP => temp_derived(column, plan, curr),
        ActivityId::PWR_IN => in_derived(column, plan, curr),
        ActivityId::PWR_FREQ if column == freq_col::WGH_MHZ => Err(ComputeIssue::NeedsItemGroup),
        ActivityId::PWR_BAT => bat_derived(column, plan, prev, curr, ctx),
        _ => Err(ComputeIssue::NotImplemented),
    }
}

/// `A_CPU` の派生列。分母は tick 合計 (グローバル itv ではない)。
fn cpu_derived(
    column: usize,
    plan: &DecodePlan,
    prev: &ItemSnapshot,
    curr: &ItemSnapshot,
    ctx: &ComputeContext,
) -> Computed {
    if !ctx.has_prev {
        return Err(ComputeIssue::Discontinuous(Discontinuity::FirstSample));
    }
    if !ctx.continuous {
        return Err(ComputeIssue::Discontinuous(Discontinuity::Restart));
    }
    let total = match ctx.tick_total {
        Some(0) | None => {
            return Err(ComputeIssue::Discontinuous(
                Discontinuity::NonPositiveElapsed,
            ));
        }
        Some(t) => t,
    };

    // `ll_sp_value(v1, v2, dj)` 相当。
    //
    // ここは 64bit 減算のままでよい。CPU の tick は本家では
    // `unsigned long long` (旧世代は `unsigned long`) で、複数フィールドの
    // 和を取ってから引くため減算の幅は 64bit になる。加えて逆行は
    // 本家と同じく 0.0 にクランプするので、一周した区間でも巨大値は出ない。
    let llsp = |p: u64, c: u64| -> f64 {
        if c < p {
            0.0
        } else {
            c.wrapping_sub(p) as f64 / total as f64 * 100.0
        }
    };

    match column {
        // %system = sys + hardirq + softirq (`-u`)
        cpu_col::SYSTEM => {
            let p = raw_column(plan, prev, cpu_col::SYS)?
                .wrapping_add(raw_or_zero(plan, prev, cpu_col::IRQ)?)
                .wrapping_add(raw_or_zero(plan, prev, cpu_col::SOFT)?);
            let c = raw_column(plan, curr, cpu_col::SYS)?
                .wrapping_add(raw_or_zero(plan, curr, cpu_col::IRQ)?)
                .wrapping_add(raw_or_zero(plan, curr, cpu_col::SOFT)?);
            Ok(llsp(p, c))
        }
        // %usr = user - guest (`-u ALL`)
        cpu_col::USR => {
            let p = raw_column(plan, prev, cpu_col::USER)?.wrapping_sub(raw_or_zero(
                plan,
                prev,
                cpu_col::GUEST,
            )?);
            let c = raw_column(plan, curr, cpu_col::USER)?.wrapping_sub(raw_or_zero(
                plan,
                curr,
                cpu_col::GUEST,
            )?);
            Ok(llsp(p, c))
        }
        // %nice = nice - guest_nice (`-u ALL`)
        cpu_col::NICE_EXCL_GNICE => {
            let p = raw_column(plan, prev, cpu_col::NICE)?.wrapping_sub(raw_or_zero(
                plan,
                prev,
                cpu_col::GNICE,
            )?);
            let c = raw_column(plan, curr, cpu_col::NICE)?.wrapping_sub(raw_or_zero(
                plan,
                curr,
                cpu_col::GNICE,
            )?);
            Ok(llsp(p, c))
        }
        _ => Err(ComputeIssue::NotImplemented),
    }
}

/// `SP_VALUE(m, n, p)` = `(n - m) / p * 100`。符号なし減算のまま f64 化する。
#[inline]
fn sp_value(m: u64, n: u64, p: u64) -> f64 {
    n.wrapping_sub(m) as f64 / p as f64 * 100.0
}

/// `availablekb` (= `/proc/meminfo` の `MemAvailable`) を取り出す。
///
/// このフィールドは **v11.5.3 で追加**されたもので、それより前の `A_MEMORY`
/// (`magic 0x8a` / `size` 128 以下) には存在しない (02 §9.4)。
///
/// 欠落を 0 として `tlmkb - 0` を計算すると、総量が正のとき
/// `kbmemused` = 総量、`%memused` = **100%** になる。旧世代のファイルを
/// 読んだだけで「メモリ使用率 100%」という嘘を表示することになるため、
/// 0 埋め ([`raw_or_zero`]) は使ってはいけない。
///
/// 世代別の正しい扱い:
///
/// - 本家は旧形式を直接読めず `sadf -c` で変換してから読む。その変換は
///   `availablekb` が無い世代に **`frmkb` を代入する**。02 §8 に
///   「`%memused` が 100% にならないようにするため」と明記されている。
///   これは `availablekb` 導入前の `sar` が `%memused` を
///   `(tlmkb - frmkb) / tlmkb` として表示していたことと一致する。
/// - したがって [`MissingPolicy::Compat`] では `frmkb` を代替に使う
///   (= その世代の `sar` が出していた値)。
/// - [`MissingPolicy::Strict`] では代替せず欠落を返す。
///   `kbavail` 相当の値はこのファイルからは得られない、が正しい報告である。
#[inline]
fn memory_available(
    plan: &DecodePlan,
    item: &ItemSnapshot,
    policy: MissingPolicy,
) -> Result<u64, ComputeIssue> {
    match plan.column_value(&item.values, mem_col::KBAVAIL) {
        Availability::Present(v) => Ok(v),
        Availability::MissingInSample => Err(ComputeIssue::MissingInSample),
        Availability::UnsupportedBySource => match policy {
            MissingPolicy::Compat => raw_column(plan, item, mem_col::KBMEMFREE),
            MissingPolicy::Strict => Err(ComputeIssue::UnsupportedBySource),
        },
    }
}

/// `A_MEMORY` の派生列。すべてゲージ (差分化しない)。
fn memory_derived(
    column: usize,
    plan: &DecodePlan,
    curr: &ItemSnapshot,
    policy: MissingPolicy,
) -> Computed {
    match column {
        // kbmemused = tlmkb - availablekb (frmkb ではない)
        mem_col::KBMEMUSED => {
            let total = raw_column(plan, curr, mem_col::KBMEMTOTAL)?;
            let avail = memory_available(plan, curr, policy)?;
            Ok(total.wrapping_sub(avail) as f64)
        }
        mem_col::MEMUSED_PCT => {
            let total = raw_column(plan, curr, mem_col::KBMEMTOTAL)?;
            let avail = memory_available(plan, curr, policy)?;
            Ok(if total != 0 {
                sp_value(avail, total, total)
            } else {
                0.0
            })
        }
        mem_col::COMMIT_PCT => {
            // tlskb は分母の加算項。swap を持たない世代では 0 でよい
            // (RAM だけが分母になる = その世代の sar と同じ)。
            let total = raw_column(plan, curr, mem_col::KBMEMTOTAL)?.wrapping_add(raw_or_zero(
                plan,
                curr,
                mem_col::KBSWPTOTAL,
            )?);
            // comkb は分子そのもの。欠落を 0 にすると %commit が常に 0% になる
            let com = primary_input(plan, curr, mem_col::KBCOMMIT, policy)?;
            Ok(if total != 0 {
                sp_value(0, com, total)
            } else {
                0.0
            })
        }
        mem_col::KBSWPUSED => {
            let total = raw_column(plan, curr, mem_col::KBSWPTOTAL)?;
            let free = raw_column(plan, curr, mem_col::KBSWPFREE)?;
            Ok(total.wrapping_sub(free) as f64)
        }
        mem_col::SWPUSED_PCT => {
            let total = raw_column(plan, curr, mem_col::KBSWPTOTAL)?;
            let free = raw_column(plan, curr, mem_col::KBSWPFREE)?;
            Ok(if total != 0 {
                sp_value(free, total, total)
            } else {
                0.0
            })
        }
        mem_col::SWPCAD_PCT => {
            let total = raw_column(plan, curr, mem_col::KBSWPTOTAL)?;
            let free = raw_column(plan, curr, mem_col::KBSWPFREE)?;
            // caskb は分子そのもの。欠落を 0 にすると %swpcad が常に 0% になる
            let cad = primary_input(plan, curr, mem_col::KBSWPCAD, policy)?;
            let used = total.wrapping_sub(free);
            Ok(if used != 0 {
                sp_value(0, cad, used)
            } else {
                0.0
            })
        }
        _ => Err(ComputeIssue::NotImplemented),
    }
}

/// `A_HUGE` の派生列。
fn huge_derived(column: usize, plan: &DecodePlan, curr: &ItemSnapshot) -> Computed {
    let total = raw_column(plan, curr, huge_col::KBHUGTOTAL)?;
    let free = raw_column(plan, curr, huge_col::KBHUGFREE)?;
    match column {
        huge_col::KBHUGUSED => Ok(total.wrapping_sub(free) as f64),
        huge_col::HUGUSED_PCT => Ok(if total != 0 {
            sp_value(free, total, total)
        } else {
            0.0
        }),
        _ => Err(ComputeIssue::NotImplemented),
    }
}

/// `A_DISK` の派生列 (`compute_ext_disk_stats()` 相当、03 §5.1)。
fn disk_derived(
    column: usize,
    plan: &DecodePlan,
    prev: &ItemSnapshot,
    curr: &ItemSnapshot,
    ctx: &ComputeContext,
) -> Computed {
    if !ctx.has_prev {
        return Err(ComputeIssue::Discontinuous(Discontinuity::FirstSample));
    }
    if !ctx.continuous {
        return Err(ComputeIssue::Discontinuous(Discontinuity::Restart));
    }

    let ios_p = raw_column(plan, prev, disk_col::TPS)?;
    let ios_c = raw_column(plan, curr, disk_col::TPS)?;
    // 完了 I/O が増えていないときは 0 (0 除算回避も兼ねる)。
    // 本家も `nr_ios_c > nr_ios_p` を素の比較で行い、偽なら 0.0 を返す (03 §5.1)。
    if ios_c <= ios_p {
        return Ok(0.0);
    }
    let d_ios = (ios_c - ios_p) as f64;

    // 各入力列の差分は**その列の幅**で取る。
    //
    // `rd_ticks` / `wr_ticks` / `dc_ticks` は全世代で `unsigned int` (02 §7)。
    // 64bit のまま引くと、tick カウンタが一周した区間で差分が 1.84e19 になり、
    // `await` が 10¹⁸ ms という値になる。本家は `unsigned int` 同士の減算なので
    // 32bit で一周が畳まれ、正しい ms が出る。ここを合わせる。
    //
    // 3 本の和は f64 で取る。本家は `unsigned int` の和なので 2^32 ms
    // (約 49 日分の tick) を超えると本家側だけが一周するが、
    // 1 区間でそこまで積み上がる入力は現実には無い。
    let sum_delta = |cols: [usize; 3]| -> Result<f64, ComputeIssue> {
        let mut acc = 0.0;
        for c in cols {
            let p = raw_or_zero(plan, prev, c)?;
            let n = raw_or_zero(plan, curr, c)?;
            acc += wrapping_delta(p, n, counter_bits(plan, c)) as f64;
        }
        Ok(acc)
    };

    match column {
        // areq-sz = Σ(Δsect) / Δnr_ios / 2 (セクタ → kB)
        disk_col::AREQ_SZ => {
            let sect = sum_delta([disk_col::RKB, disk_col::WKB, disk_col::DKB])?;
            Ok(sect / d_ios / 2.0)
        }
        // await = Σ(Δticks) / Δnr_ios (ミリ秒、追加スケーリングなし)
        disk_col::AWAIT => {
            let ticks = sum_delta([disk_col::RD_TICKS, disk_col::WR_TICKS, disk_col::DC_TICKS])?;
            Ok(ticks / d_ios)
        }
        _ => Err(ComputeIssue::NotImplemented),
    }
}

/// `A_FS` の派生列。`f_*` はバイト単位のゲージ (03 §id=37)。
fn fs_derived(column: usize, plan: &DecodePlan, curr: &ItemSnapshot) -> Computed {
    match column {
        fs_col::MB_USED => {
            let blocks = raw_column(plan, curr, fs_col::TOTAL)?;
            let free = raw_column(plan, curr, fs_col::MB_FREE)?;
            Ok(blocks.wrapping_sub(free) as f64)
        }
        fs_col::USED_PCT => {
            let blocks = raw_column(plan, curr, fs_col::TOTAL)?;
            let free = raw_column(plan, curr, fs_col::MB_FREE)?;
            Ok(if blocks != 0 {
                sp_value(free, blocks, blocks)
            } else {
                0.0
            })
        }
        fs_col::UNPRIV_USED_PCT => {
            let blocks = raw_column(plan, curr, fs_col::TOTAL)?;
            let avail = raw_column(plan, curr, fs_col::AVAILABLE)?;
            Ok(if blocks != 0 {
                sp_value(avail, blocks, blocks)
            } else {
                0.0
            })
        }
        fs_col::IUSED => {
            let files = raw_column(plan, curr, fs_col::INODES_TOTAL)?;
            let ffree = raw_column(plan, curr, fs_col::IFREE)?;
            Ok(files.wrapping_sub(ffree) as f64)
        }
        fs_col::IUSED_PCT => {
            let files = raw_column(plan, curr, fs_col::INODES_TOTAL)?;
            let ffree = raw_column(plan, curr, fs_col::IFREE)?;
            Ok(if files != 0 {
                sp_value(ffree, files, files)
            } else {
                0.0
            })
        }
        _ => Err(ComputeIssue::NotImplemented),
    }
}

/// `A_NET_DEV` の派生列 (`%ifutil`)。
fn net_dev_derived(
    column: usize,
    plan: &DecodePlan,
    prev: &ItemSnapshot,
    curr: &ItemSnapshot,
    ctx: &ComputeContext,
    policy: MissingPolicy,
) -> Computed {
    if column != net_dev_col::IFUTIL_PCT {
        return Err(ComputeIssue::NotImplemented);
    }
    if !ctx.has_prev {
        return Err(ComputeIssue::Discontinuous(Discontinuity::FirstSample));
    }
    if !ctx.continuous {
        return Err(ComputeIssue::Discontinuous(Discontinuity::Restart));
    }

    // rx / tx は**バイト毎秒**。名前が rxkb でも 1024 で割ってはいけない (03 §1.8.1)。
    // 差分は列の幅で取る (旧世代の `rx_bytes` は `unsigned long`。
    // 32bit ライタのファイルでは一周が 2^32 で起きる)。
    let rx = s_value_bits(
        raw_column(plan, prev, net_dev_col::RXKB)?,
        raw_column(plan, curr, net_dev_col::RXKB)?,
        ctx.itv_cs,
        counter_bits(plan, net_dev_col::RXKB),
    );
    let tx = s_value_bits(
        raw_column(plan, prev, net_dev_col::TXKB)?,
        raw_column(plan, curr, net_dev_col::TXKB)?,
        ctx.itv_cs,
        counter_bits(plan, net_dev_col::TXKB),
    );
    // speed は分母。この世代に `speed` が無いと 0 になり、
    // 本家は `speed == 0` を「不明」として 0.0 を返す (03 §5.2)。
    // 互換出力ではそれに従うが、独自出力では「不明」を 0% と見せない。
    let speed = primary_input(plan, curr, net_dev_col::SPEED, policy)?;
    // duplex は式の選択にしか使わない。本家も未提供時は
    // 0 = C_DUPLEX_UNKNOWN として半二重側の式を使う。
    let duplex_v = raw_or_zero(plan, curr, net_dev_col::DUPLEX)?;
    Ok(ifutil(rx, tx, speed, duplex_v))
}

/// NIC 利用率 (`compute_ifutil()`、03 §5.2)。
///
/// `rx` / `tx` はバイト毎秒、`speed` は Mbit/s。`800` = 8 (バイト→ビット) × 100 (百分率)。
pub fn ifutil(rx: f64, tx: f64, speed: u64, duplex_value: u64) -> f64 {
    if speed == 0 {
        return 0.0;
    }
    let bps = (speed * 1_000_000) as f64;
    if duplex_value == duplex::FULL {
        rx.max(tx) * 800.0 / bps
    } else {
        (rx + tx) * 800.0 / bps
    }
}

/// `A_PWR_FAN` の派生列 (`drpm` = rpm - rpm_min)。
fn fan_derived(column: usize, plan: &DecodePlan, curr: &ItemSnapshot) -> Computed {
    if column != fan_col::DRPM {
        return Err(ComputeIssue::NotImplemented);
    }
    let rpm = raw_f64(plan, curr, fan_col::RPM)?;
    let rpm_min = raw_f64(plan, curr, fan_col::RPM_MIN)?;
    Ok(rpm - rpm_min)
}

/// センサ値の「レンジ内位置」を百分率で返す (`%temp` / `%in` 共通)。
#[inline]
fn range_pct(value: f64, min: f64, max: f64) -> f64 {
    if (max - min) != 0.0 {
        (value - min) / (max - min) * 100.0
    } else {
        0.0
    }
}

/// `A_PWR_TEMP` の派生列 (`%temp`)。
fn temp_derived(column: usize, plan: &DecodePlan, curr: &ItemSnapshot) -> Computed {
    if column != temp_col::PCT {
        return Err(ComputeIssue::NotImplemented);
    }
    Ok(range_pct(
        raw_f64(plan, curr, temp_col::DEGC)?,
        raw_f64(plan, curr, temp_col::MIN)?,
        raw_f64(plan, curr, temp_col::MAX)?,
    ))
}

/// `A_PWR_IN` の派生列 (`%in`)。
fn in_derived(column: usize, plan: &DecodePlan, curr: &ItemSnapshot) -> Computed {
    if column != in_col::PCT {
        return Err(ComputeIssue::NotImplemented);
    }
    Ok(range_pct(
        raw_f64(plan, curr, in_col::VOLTS)?,
        raw_f64(plan, curr, in_col::MIN)?,
        raw_f64(plan, curr, in_col::MAX)?,
    ))
}

/// `A_PWR_BAT` の派生列 (`cap/min`)。
///
/// `capacity` は `signed char`。`itv` は 1/100 秒なので
/// `Δ / (itv/100) * 60 = Δ * 6000 / itv` で「%/分」になる (03 §id=43)。
fn bat_derived(
    column: usize,
    plan: &DecodePlan,
    prev: &ItemSnapshot,
    curr: &ItemSnapshot,
    ctx: &ComputeContext,
) -> Computed {
    if column != bat_col::CAP_PER_MIN {
        return Err(ComputeIssue::NotImplemented);
    }
    if !ctx.has_prev {
        return Err(ComputeIssue::Discontinuous(Discontinuity::FirstSample));
    }
    if !ctx.continuous {
        return Err(ComputeIssue::Discontinuous(Discontinuity::Restart));
    }
    let p = i32::from(signed_byte(raw_column(plan, prev, bat_col::CAP_PCT)?));
    let c = i32::from(signed_byte(raw_column(plan, curr, bat_col::CAP_PCT)?));
    Ok(f64::from(c - p) * 6000.0 / ctx.itv_cs as f64)
}

// ============================================================================
// A_CPU の tick 合計と CPU "all" の再合算
// ============================================================================

/// CPU の 1 フィールド分の差分。アンダーフローは 0 に潰す。
#[inline]
fn cpu_delta(prev: u64, curr: u64) -> u64 {
    curr.saturating_sub(prev)
}

/// `get_per_cpu_interval()` 相当 (03 §1.4.2)。
///
/// 戻り値は「補正済みの前サンプル」と tick 合計 (jiffies)。
/// 前サンプルの `iowait` / `idle` は CPU 復帰・トラッキング誤差の補正で
/// 書き換わるため、CPU "all" の合算には**補正後の値**を使う必要がある。
pub fn per_cpu_interval(
    plan: &DecodePlan,
    prev: &ItemSnapshot,
    curr: &ItemSnapshot,
) -> (ItemSnapshot, u64) {
    let mut fixed = prev.clone();
    let get = |item: &ItemSnapshot, col: usize| -> u64 {
        match plan.column_value(&item.values, col) {
            Availability::Present(v) => v,
            _ => 0,
        }
    };

    let (cu, cn, cg, cgn) = (
        get(curr, cpu_col::USER),
        get(curr, cpu_col::NICE),
        get(curr, cpu_col::GUEST),
        get(curr, cpu_col::GNICE),
    );
    let (pu, pn, pg, pgn) = (
        get(prev, cpu_col::USER),
        get(prev, cpu_col::NICE),
        get(prev, cpu_col::GUEST),
        get(prev, cpu_col::GNICE),
    );

    // guest が user に含まれる分の補正
    let mut ishift: u64 = 0;
    if cu >= pu && cu.wrapping_sub(cg) < pu.wrapping_sub(pg) {
        ishift = ishift.wrapping_add(pu.wrapping_sub(pg).wrapping_sub(cu.wrapping_sub(cg)));
    }
    if cn >= pn && cn.wrapping_sub(cgn) < pn.wrapping_sub(pgn) {
        ishift = ishift.wrapping_add(pn.wrapping_sub(pgn).wrapping_sub(cn.wrapping_sub(cgn)));
    }

    // CPU 復帰 / iowait 誤差の補正 (03 §1.3.3)
    let (c_iowait, p_iowait) = (get(curr, cpu_col::IOWAIT), get(prev, cpu_col::IOWAIT));
    let (c_idle, p_idle) = (get(curr, cpu_col::IDLE), get(prev, cpu_col::IDLE));
    let mut fixed_iowait = p_iowait;
    let mut fixed_idle = p_idle;
    if c_iowait < p_iowait && p_iowait < CPU_OVERFLOW_THRESHOLD {
        fixed_iowait = if c_idle > p_idle || p_idle >= CPU_OVERFLOW_THRESHOLD {
            c_iowait
        } else {
            0
        };
        set_column(plan, &mut fixed, cpu_col::IOWAIT, fixed_iowait);
    }
    if c_idle < p_idle && p_idle < CPU_OVERFLOW_THRESHOLD {
        fixed_idle = 0;
        set_column(plan, &mut fixed, cpu_col::IDLE, 0);
    }

    // guest / guest_nice は user / nice に内包されるので足さない
    let mut interval: u64 = 0;
    for col in cpu_col::TICK_FIELDS {
        let (p, c) = match col {
            cpu_col::IOWAIT => (fixed_iowait, c_iowait),
            cpu_col::IDLE => (fixed_idle, c_idle),
            _ => (get(prev, col), get(curr, col)),
        };
        interval = interval.wrapping_add(cpu_delta(p, c));
    }

    (fixed, interval.wrapping_add(ishift))
}

/// CPU がオフラインか (`tot_jiffies_c == 0`、03 §1.4.3)。
///
/// `/proc/stat` から当該 CPU 行が消えると全フィールドが 0 になる。
/// tickless CPU (差分が 0) とは区別が必要なので、判定は**現サンプルの絶対値**で行う。
pub fn cpu_is_offline(plan: &DecodePlan, item: &ItemSnapshot) -> bool {
    cpu_tick_sum(plan, item) == 0
}

/// tick 8 フィールドの単純和 (オフライン判定用)。
fn cpu_tick_sum(plan: &DecodePlan, item: &ItemSnapshot) -> u64 {
    cpu_col::TICK_FIELDS
        .iter()
        .map(|c| match plan.column_value(&item.values, *c) {
            Availability::Present(v) => v,
            _ => 0,
        })
        .fold(0u64, |a, b| a.wrapping_add(b))
}

/// 再合算した CPU "all" と全 CPU の tick 合計。
#[derive(Debug, Clone)]
pub struct CpuAggregate {
    /// 合算した前サンプル (補正後の値を足し込んだもの)。
    pub prev: ItemSnapshot,
    /// 合算した現サンプル。
    pub curr: ItemSnapshot,
    /// `deltot_jiffies` — 全 CPU の [`per_cpu_interval`] の総和。
    pub tick_total: u64,
    /// オフラインと判定した item 添字 (1 起点 = CPU 番号 + 1)。
    pub offline: Vec<usize>,
}

/// `get_global_cpu_statistics()` 相当 (03 §1.4.3)。
///
/// SMP では CPU "all" を `/proc/stat` の `cpu` 行ではなく
/// **個別 CPU の合算**で作り直す。オフライン CPU は合算にも入れない。
///
/// `items` は item 添字 0 = CPU "all"、添字 `n` = CPU `n-1`。
/// 個別 CPU が存在しない (`len <= 1`) 場合は `None` を返し、
/// 呼び出し側は item 0 をそのまま使う (UP 機の経路)。
///
/// `since_boot` は本家の `WANT_SINCE_BOOT`。真のとき前サンプルは全ゼロが正常なので、
/// 「前サンプルでもオフライン」によるスキップを行わない。
pub fn aggregate_cpu(
    plan: &DecodePlan,
    prev_items: &[ItemSnapshot],
    curr_items: &[ItemSnapshot],
    since_boot: bool,
) -> Option<CpuAggregate> {
    if curr_items.len() <= 1 || prev_items.len() <= 1 {
        return None;
    }
    let width = curr_items[0].values.len();
    let zero = || ItemSnapshot {
        key: None,
        texts: Vec::new(),
        values: vec![Availability::Present(0); width],
    };
    let mut agg_prev = zero();
    let mut agg_curr = zero();
    let mut total: u64 = 0;
    let mut offline = Vec::new();

    let n = prev_items.len().min(curr_items.len());
    for i in 1..n {
        let scc = &curr_items[i];
        let scp = &prev_items[i];
        let tot_c = cpu_tick_sum(plan, scc);
        let tot_p = cpu_tick_sum(plan, scp);

        // 現在オフライン: 現値を前値で埋めて復帰時のジャンプを防ぐ
        let scc_owned = if tot_c == 0 {
            offline.push(i);
            scp.clone()
        } else {
            scc.clone()
        };
        // 直前サンプルでもオフライン = 基準値が無い → CPU "all" にも加算しない。
        // 「起動時からの統計」モードでは前値が全ゼロなのが正常なので除外しない
        // (本家の `!WANT_SINCE_BOOT` 条件)。
        if tot_p == 0 && !since_boot {
            if !offline.contains(&i) {
                offline.push(i);
            }
            continue;
        }

        let (fixed_prev, itv) = per_cpu_interval(plan, scp, &scc_owned);
        total = total.wrapping_add(itv);
        add_into(&mut agg_prev, &fixed_prev);
        add_into(&mut agg_curr, &scc_owned);
    }

    Some(CpuAggregate {
        prev: agg_prev,
        curr: agg_curr,
        tick_total: total,
        offline,
    })
}

/// item 群をフィールド単位で合算する。
///
/// `A_NET_SOFT` の CPU "all" 行 (`get_global_soft_statistics()`) や
/// `A_IRQ` の合計列 (`get_global_int_statistics()`) のように、
/// 「集約行は個別行の単純和」で作る activity のために使う。
pub fn sum_items<'a, I>(width: usize, items: I) -> ItemSnapshot
where
    I: IntoIterator<Item = &'a ItemSnapshot>,
{
    let mut acc = ItemSnapshot {
        key: None,
        texts: Vec::new(),
        values: vec![Availability::Present(0); width],
    };
    for it in items {
        add_into(&mut acc, it);
    }
    acc
}

/// フィールド単位の加算 (CPU "all" の合算用)。
fn add_into(acc: &mut ItemSnapshot, src: &ItemSnapshot) {
    for (slot, v) in acc.values.iter_mut().zip(src.values.iter()) {
        if let (Availability::Present(a), Availability::Present(b)) = (&slot, v) {
            *slot = Availability::Present(a.wrapping_add(*b));
        }
    }
}

// ============================================================================
// A_PWR_FREQ の重み付き平均周波数
// ============================================================================

/// `wghMHz` の計算 (03 §id=35)。
///
/// 行列型なので 1 CPU 分の全周波数スロット (`nr2` 個) を必要とする。
/// `prev_slots` / `curr_slots` は当該 CPU の連続する `nr2` item。
///
/// `freq / 1000` は**整数除算** (kHz → MHz、切り捨て)。
pub fn weighted_mhz(
    plan: &DecodePlan,
    prev_slots: &[ItemSnapshot],
    curr_slots: &[ItemSnapshot],
) -> Computed {
    let mut tisfreq: u64 = 0;
    let mut tis: u64 = 0;
    let n = prev_slots.len().min(curr_slots.len());
    for k in 0..n {
        let freq = raw_column(plan, &curr_slots[k], freq_col::FREQ_KHZ)?;
        // 未使用スロットで打ち切り
        if freq == 0 {
            break;
        }
        let c = raw_column(plan, &curr_slots[k], freq_col::TIME_IN_STATE)?;
        let p = raw_column(plan, &prev_slots[k], freq_col::TIME_IN_STATE)?;
        let d = c.wrapping_sub(p);
        tisfreq = tisfreq.wrapping_add((freq / 1000).wrapping_mul(d));
        tis = tis.wrapping_add(d);
    }
    Ok(if tis != 0 {
        tisfreq as f64 / tis as f64
    } else {
        0.0
    })
}

// ============================================================================
// Average: の計算 (方式 B′ — 生フィールドの累積平均から再計算する比率列)
// ============================================================================

/// 1 item 分の累積 (`Average:` の方式 B 用)。
///
/// 本家は activity ごとの `static` 変数に「瞬時値表示のたびに生フィールドを
/// 加算」して `Average:` 行で `合計 / avg_count` を出す (03 §8.1)。
/// ここでは列単位に、生値の整数和と表示値の f64 和の両方を持つ。
#[derive(Debug, Clone, Default)]
pub struct ItemAccum {
    /// 列ごとの生 wire 値の和 (整数のまま。本家の `unsigned long long` 累積に対応)。
    pub raw_sum: Vec<u64>,
    /// 列ごとの表示値の和。
    pub value_sum: Vec<f64>,
    /// 加算したサンプル数 (`avg_count`)。
    pub count: u64,
}

impl ItemAccum {
    pub fn new(columns: usize) -> Self {
        Self {
            raw_sum: vec![0; columns],
            value_sum: vec![0.0; columns],
            count: 0,
        }
    }

    /// 表示値と生値を 1 サンプル分足し込む。
    pub fn add(&mut self, column: usize, raw: Option<u64>, value: Option<f64>) {
        if let (Some(slot), Some(v)) = (self.raw_sum.get_mut(column), raw) {
            *slot = slot.wrapping_add(v);
        }
        if let (Some(slot), Some(v)) = (self.value_sum.get_mut(column), value) {
            *slot += v;
        }
    }

    /// 表示値の累積平均 (方式 B)。
    pub fn mean(&self, column: usize) -> Computed {
        if self.count == 0 {
            return Err(ComputeIssue::MissingInSample);
        }
        self.value_sum
            .get(column)
            .copied()
            .map(|s| s / self.count as f64)
            .ok_or(ComputeIssue::MissingInSample)
    }

    /// 生値の整数平均 (本家の `avg_xxx / avg_count` = **整数除算**)。
    fn int_mean(&self, column: usize) -> u64 {
        if self.count == 0 {
            return 0;
        }
        self.raw_sum.get(column).copied().unwrap_or(0) / self.count
    }

    /// 生値の浮動小数平均 (本家の `(double) avg_xxx / avg_count`)。
    fn float_mean(&self, column: usize) -> f64 {
        if self.count == 0 {
            return 0.0;
        }
        self.raw_sum.get(column).copied().unwrap_or(0) as f64 / self.count as f64
    }
}

/// `Average:` 行で「平均 availablekb」を引く列。
///
/// `availablekb` を持たない世代では、その位置に累積されているものが無いため
/// [`ItemAccum`] の合計は 0 のままになる。そのまま `tlmkb - 0` を計算すると
/// `Average:` 行だけ `%memused` = 100% になる。
///
/// [`memory_available`] と同じ理由で `frmkb` の平均に切り替える。
/// [`average_ratio`] は本家の `Average:` 行専用なので、方針は
/// [`MissingPolicy::Compat`] 固定でよい。
#[inline]
fn average_available_column(plan: &DecodePlan, last: &ItemSnapshot) -> usize {
    match plan.column_value(&last.values, mem_col::KBAVAIL) {
        Availability::UnsupportedBySource => mem_col::KBMEMFREE,
        _ => mem_col::KBAVAIL,
    }
}

/// 比率列の `Average:` 値 (方式 B′)。
///
/// 「分子と分母をそれぞれ平均してから比を取る」列がこれに該当する。
/// 表示値の算術平均とは一致しないので専用に計算する (03 §7-A / §7-B / §id=34)。
///
/// `last` は最後に表示したサンプル。`tlmkb` / `temp_min` / `temp_max` のように
/// **累積せず最終値を使う**フィールドの参照元になる。
///
/// これは本家の `Average:` 行を再現する経路なので、欠落の扱いは
/// [`MissingPolicy::Compat`] 相当に固定している。
pub fn average_ratio(
    id: ActivityId,
    column: usize,
    plan: &DecodePlan,
    acc: &ItemAccum,
    last: &ItemSnapshot,
) -> Computed {
    if acc.count == 0 {
        return Err(ComputeIssue::MissingInSample);
    }
    match (id, column) {
        // kbmemused = 最終サンプルの tlmkb - 平均 availablekb (浮動小数除算)
        (ActivityId::MEMORY, mem_col::KBMEMUSED) => {
            let total = raw_column(plan, last, mem_col::KBMEMTOTAL)?;
            Ok(total as f64 - acc.float_mean(average_available_column(plan, last)))
        }
        // %memused は **整数除算**を経由する (03 §7-A の落とし穴)
        (ActivityId::MEMORY, mem_col::MEMUSED_PCT) => {
            let total = raw_column(plan, last, mem_col::KBMEMTOTAL)?;
            Ok(if total != 0 {
                sp_value(
                    acc.int_mean(average_available_column(plan, last)),
                    total,
                    total,
                )
            } else {
                0.0
            })
        }
        // %commit も整数除算
        (ActivityId::MEMORY, mem_col::COMMIT_PCT) => {
            let total = raw_column(plan, last, mem_col::KBMEMTOTAL)?.wrapping_add(raw_or_zero(
                plan,
                last,
                mem_col::KBSWPTOTAL,
            )?);
            Ok(if total != 0 {
                sp_value(0, acc.int_mean(mem_col::KBCOMMIT), total)
            } else {
                0.0
            })
        }
        (ActivityId::MEMORY, mem_col::KBSWPUSED) => {
            Ok(acc.float_mean(mem_col::KBSWPTOTAL) - acc.float_mean(mem_col::KBSWPFREE))
        }
        // swap 側は浮動小数除算
        (ActivityId::MEMORY, mem_col::SWPUSED_PCT) => {
            let total = acc.float_mean(mem_col::KBSWPTOTAL);
            let free = acc.float_mean(mem_col::KBSWPFREE);
            Ok(if acc.raw_sum[mem_col::KBSWPTOTAL] != 0 {
                (total - free) / total * 100.0
            } else {
                0.0
            })
        }
        (ActivityId::MEMORY, mem_col::SWPCAD_PCT) => {
            let total = acc.float_mean(mem_col::KBSWPTOTAL);
            let free = acc.float_mean(mem_col::KBSWPFREE);
            let cad = acc.float_mean(mem_col::KBSWPCAD);
            Ok(
                if acc.raw_sum[mem_col::KBSWPTOTAL] != acc.raw_sum[mem_col::KBSWPFREE] {
                    cad / (total - free) * 100.0
                } else {
                    0.0
                },
            )
        }
        (ActivityId::HUGE, huge_col::KBHUGUSED) => {
            Ok(acc.float_mean(huge_col::KBHUGTOTAL) - acc.float_mean(huge_col::KBHUGFREE))
        }
        (ActivityId::HUGE, huge_col::HUGUSED_PCT) => {
            let total = acc.float_mean(huge_col::KBHUGTOTAL);
            let free = acc.float_mean(huge_col::KBHUGFREE);
            Ok(if acc.raw_sum[huge_col::KBHUGTOTAL] != 0 {
                (total - free) / total * 100.0
            } else {
                0.0
            })
        }
        // %temp / %in は min/max を**累積せず最終値で代入**する (03 §id=32 / §id=33)
        (ActivityId::PWR_TEMP, temp_col::PCT) => Ok(range_pct(
            acc.mean(temp_col::DEGC)?,
            raw_f64(plan, last, temp_col::MIN)?,
            raw_f64(plan, last, temp_col::MAX)?,
        )),
        (ActivityId::PWR_IN, in_col::PCT) => Ok(range_pct(
            acc.mean(in_col::VOLTS)?,
            raw_f64(plan, last, in_col::MIN)?,
            raw_f64(plan, last, in_col::MAX)?,
        )),
        _ => Err(ComputeIssue::NotImplemented),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::abi::{Endian, LayoutAbi, SourceEncoding};
    use crate::layout::registry::lookup;

    fn item(values: &[u64]) -> ItemSnapshot {
        ItemSnapshot {
            key: None,
            texts: Vec::new(),
            values: values.iter().map(|v| Availability::Present(*v)).collect(),
        }
    }

    /// 最新 revision のデコード計画を作る (単体テスト用)。
    fn plan_for(id: ActivityId) -> DecodePlan {
        let def = lookup(id).expect("定義がある");
        let rev = def.latest().expect("revision がある");
        let enc = SourceEncoding::new(Endian::Little, LayoutAbi::LP64);
        DecodePlan::build(def, rev, rev.size_lp64, 1, 1, &enc).expect("計画を作れる")
    }

    /// wire フィールド数ぶんのゼロ item を作る。
    fn zeros(plan: &DecodePlan) -> ItemSnapshot {
        ItemSnapshot {
            key: None,
            texts: Vec::new(),
            values: vec![Availability::Present(0); plan.fields.len()],
        }
    }

    /// 列を指定して値を入れる。
    fn put(plan: &DecodePlan, item: &mut ItemSnapshot, column: usize, v: u64) {
        set_column(plan, item, column, v);
    }

    fn compute(
        id: ActivityId,
        column: usize,
        plan: &DecodePlan,
        prev: &ItemSnapshot,
        curr: &ItemSnapshot,
        ctx: &ComputeContext,
    ) -> Computed {
        let def = lookup(id).unwrap();
        column_value(id, column, &def.columns[column], plan, prev, curr, ctx)
    }

    #[test]
    fn tick_total_sums_all_field_deltas() {
        let p = item(&[100, 200, 300]);
        let c = item(&[110, 220, 330]);
        assert_eq!(tick_total(&p, &c), 10 + 20 + 30);
    }

    /// オフライン CPU は全フィールドが 0 のままなので合計も 0 になる。
    #[test]
    fn offline_cpu_has_zero_tick_total() {
        let p = item(&[0, 0, 0]);
        let c = item(&[0, 0, 0]);
        assert_eq!(tick_total(&p, &c), 0);
    }

    // ---- A_CPU ----

    /// `%system` は sys + hardirq + softirq。`%sys` (`-u ALL`) とは別物。
    #[test]
    fn cpu_system_includes_irq_and_softirq() {
        let plan = plan_for(ActivityId::CPU);
        let mut p = zeros(&plan);
        let mut c = zeros(&plan);
        // idle だけ動かして tick 合計を 1000 にする
        put(&plan, &mut c, cpu_col::IDLE, 700);
        put(&plan, &mut c, cpu_col::SYS, 100);
        put(&plan, &mut c, cpu_col::IRQ, 120);
        put(&plan, &mut c, cpu_col::SOFT, 80);
        let (_, total) = per_cpu_interval(&plan, &p, &c);
        assert_eq!(total, 1000);
        let ctx = ComputeContext::new(1000).with_tick_total(total);

        let sys_all = compute(ActivityId::CPU, cpu_col::SYS, &plan, &p, &c, &ctx).unwrap();
        let system = compute(ActivityId::CPU, cpu_col::SYSTEM, &plan, &p, &c, &ctx).unwrap();
        assert_eq!(sys_all, 10.0, "%sys は sys のみ");
        assert_eq!(system, 30.0, "%system は sys+irq+soft");

        // prev を動かしても関係が保たれる (メタモルフィック)
        put(&plan, &mut p, cpu_col::SYS, 5);
        put(&plan, &mut c, cpu_col::SYS, 105);
        let (_, total2) = per_cpu_interval(&plan, &p, &c);
        let ctx2 = ComputeContext::new(1000).with_tick_total(total2);
        assert!(compute(ActivityId::CPU, cpu_col::SYSTEM, &plan, &p, &c, &ctx2).unwrap() > 0.0);
    }

    /// `%usr` は user から guest を引く。`%user` は引かない。
    #[test]
    fn cpu_usr_excludes_guest_but_user_does_not() {
        let plan = plan_for(ActivityId::CPU);
        let p = zeros(&plan);
        let mut c = zeros(&plan);
        put(&plan, &mut c, cpu_col::USER, 400); // うち 150 が guest
        put(&plan, &mut c, cpu_col::GUEST, 150);
        put(&plan, &mut c, cpu_col::IDLE, 600);
        let (_, total) = per_cpu_interval(&plan, &p, &c);
        assert_eq!(total, 1000, "guest は tick 合計に足さない");
        let ctx = ComputeContext::new(1000).with_tick_total(total);

        assert_eq!(
            compute(ActivityId::CPU, cpu_col::USER, &plan, &p, &c, &ctx).unwrap(),
            40.0
        );
        assert_eq!(
            compute(ActivityId::CPU, cpu_col::USR, &plan, &p, &c, &ctx).unwrap(),
            25.0,
            "%usr = (400-150)/1000"
        );
        assert_eq!(
            compute(ActivityId::CPU, cpu_col::GUEST, &plan, &p, &c, &ctx).unwrap(),
            15.0
        );
    }

    /// `%nice` (`-u ALL`) は guest_nice を引く。
    #[test]
    fn cpu_nice_all_excludes_guest_nice() {
        let plan = plan_for(ActivityId::CPU);
        let p = zeros(&plan);
        let mut c = zeros(&plan);
        put(&plan, &mut c, cpu_col::NICE, 300);
        put(&plan, &mut c, cpu_col::GNICE, 100);
        put(&plan, &mut c, cpu_col::IDLE, 700);
        let (_, total) = per_cpu_interval(&plan, &p, &c);
        let ctx = ComputeContext::new(1000).with_tick_total(total);
        assert_eq!(
            compute(ActivityId::CPU, cpu_col::NICE, &plan, &p, &c, &ctx).unwrap(),
            30.0
        );
        assert_eq!(
            compute(
                ActivityId::CPU,
                cpu_col::NICE_EXCL_GNICE,
                &plan,
                &p,
                &c,
                &ctx
            )
            .unwrap(),
            20.0
        );
    }

    /// 逆行した CPU カウンタは 0.0 にクランプされる (`ll_sp_value`)。
    #[test]
    fn cpu_counters_clamp_on_decrease() {
        let plan = plan_for(ActivityId::CPU);
        let mut p = zeros(&plan);
        let mut c = zeros(&plan);
        put(&plan, &mut p, cpu_col::IDLE, 1000);
        put(&plan, &mut c, cpu_col::IDLE, 900);
        let ctx = ComputeContext::new(1000).with_tick_total(1000);
        assert_eq!(
            compute(ActivityId::CPU, cpu_col::IDLE, &plan, &p, &c, &ctx).unwrap(),
            0.0,
            "curr < prev は 0.0 (巨大な正値にしない)"
        );
    }

    /// idle が逆行したら前値を 0 とみなす補正が入る (03 §1.3.3)。
    #[test]
    fn per_cpu_interval_resets_idle_on_rollback() {
        let plan = plan_for(ActivityId::CPU);
        let mut p = zeros(&plan);
        let mut c = zeros(&plan);
        put(&plan, &mut p, cpu_col::IDLE, 5000);
        put(&plan, &mut c, cpu_col::IDLE, 100);
        let (fixed, total) = per_cpu_interval(&plan, &p, &c);
        assert_eq!(
            raw_column(&plan, &fixed, cpu_col::IDLE).unwrap(),
            0,
            "補正後の前値は 0"
        );
        assert_eq!(total, 100, "差分は 100 - 0");
    }

    /// CPU "all" は個別 CPU の合算で作り直す。
    #[test]
    fn cpu_all_is_recomputed_from_per_cpu() {
        let plan = plan_for(ActivityId::CPU);
        // 前サンプルは「稼働していた」状態にする (全ゼロだとオフライン扱いになる)
        let mut p0 = zeros(&plan);
        put(&plan, &mut p0, cpu_col::IDLE, 10);
        let mut p1 = zeros(&plan);
        put(&plan, &mut p1, cpu_col::IDLE, 10);
        let prev = vec![zeros(&plan), p0, p1];

        let mut cpu0 = zeros(&plan);
        put(&plan, &mut cpu0, cpu_col::USER, 200);
        put(&plan, &mut cpu0, cpu_col::IDLE, 810);
        let mut cpu1 = zeros(&plan);
        put(&plan, &mut cpu1, cpu_col::USER, 400);
        put(&plan, &mut cpu1, cpu_col::IDLE, 610);
        // item 0 (ファイル上の CPU "all") にはあえて別の値を入れておく
        let mut bogus_all = zeros(&plan);
        put(&plan, &mut bogus_all, cpu_col::USER, 9999);
        let curr = vec![bogus_all, cpu0, cpu1];

        let agg = aggregate_cpu(&plan, &prev, &curr, false).expect("SMP なので合算する");
        assert_eq!(agg.tick_total, 2000);
        assert!(agg.offline.is_empty());
        assert_eq!(
            raw_column(&plan, &agg.curr, cpu_col::USER).unwrap(),
            600,
            "ファイルの cpu 行ではなく個別 CPU の和"
        );
        let ctx = ComputeContext::new(1000).with_tick_total(agg.tick_total);
        assert_eq!(
            compute(
                ActivityId::CPU,
                cpu_col::USER,
                &plan,
                &agg.prev,
                &agg.curr,
                &ctx
            )
            .unwrap(),
            30.0
        );
    }

    /// 前サンプルが全ゼロの CPU はオフライン扱いで CPU "all" に加算しない。
    /// ただし「起動時からの統計」モードでは加算する。
    #[test]
    fn cpu_without_previous_sample_is_offline_unless_since_boot() {
        let plan = plan_for(ActivityId::CPU);
        let prev = vec![zeros(&plan), zeros(&plan)];
        let mut cpu0 = zeros(&plan);
        put(&plan, &mut cpu0, cpu_col::IDLE, 1_000);
        let curr = vec![zeros(&plan), cpu0];

        let agg = aggregate_cpu(&plan, &prev, &curr, false).unwrap();
        assert_eq!(agg.tick_total, 0);
        assert_eq!(agg.offline, vec![1]);

        let boot = aggregate_cpu(&plan, &prev, &curr, true).unwrap();
        assert_eq!(boot.tick_total, 1_000);
        assert!(boot.offline.is_empty());
    }

    /// 現サンプルが全ゼロ (= オフライン) の CPU は前値で埋めて差分 0 にする。
    #[test]
    fn currently_offline_cpu_is_filled_from_previous() {
        let plan = plan_for(ActivityId::CPU);
        let mut p1 = zeros(&plan);
        put(&plan, &mut p1, cpu_col::IDLE, 500);
        let prev = vec![zeros(&plan), p1];
        let curr = vec![zeros(&plan), zeros(&plan)];

        let agg = aggregate_cpu(&plan, &prev, &curr, false).unwrap();
        assert_eq!(agg.offline, vec![1]);
        assert_eq!(
            agg.tick_total, 0,
            "前値で埋めるので差分 0 (0 からのジャンプを作らない)"
        );
    }

    #[test]
    fn offline_cpu_is_detected_by_absolute_zero() {
        let plan = plan_for(ActivityId::CPU);
        let mut online = zeros(&plan);
        put(&plan, &mut online, cpu_col::IDLE, 1);
        assert!(cpu_is_offline(&plan, &zeros(&plan)));
        assert!(!cpu_is_offline(&plan, &online));
    }

    // ---- A_MEMORY ----

    /// `kbmemused` は `tlmkb - availablekb`。`- frmkb` ではない。
    #[test]
    fn memory_used_subtracts_available_not_free() {
        let plan = plan_for(ActivityId::MEMORY);
        let p = zeros(&plan);
        let mut c = zeros(&plan);
        put(&plan, &mut c, mem_col::KBMEMTOTAL, 8_000_000);
        put(&plan, &mut c, mem_col::KBMEMFREE, 1_000_000);
        put(&plan, &mut c, mem_col::KBAVAIL, 4_000_000);
        let ctx = ComputeContext::new(1000);

        assert_eq!(
            compute(ActivityId::MEMORY, mem_col::KBMEMUSED, &plan, &p, &c, &ctx).unwrap(),
            4_000_000.0,
            "8,000,000 - 4,000,000 (frmkb を使うと 7,000,000 になる)"
        );
        assert_eq!(
            compute(
                ActivityId::MEMORY,
                mem_col::MEMUSED_PCT,
                &plan,
                &p,
                &c,
                &ctx
            )
            .unwrap(),
            50.0
        );
    }

    #[test]
    fn memory_commit_pct_uses_ram_plus_swap() {
        let plan = plan_for(ActivityId::MEMORY);
        let p = zeros(&plan);
        let mut c = zeros(&plan);
        put(&plan, &mut c, mem_col::KBMEMTOTAL, 1_000);
        put(&plan, &mut c, mem_col::KBSWPTOTAL, 1_000);
        put(&plan, &mut c, mem_col::KBCOMMIT, 500);
        let ctx = ComputeContext::new(1000);
        assert_eq!(
            compute(ActivityId::MEMORY, mem_col::COMMIT_PCT, &plan, &p, &c, &ctx).unwrap(),
            25.0
        );
    }

    #[test]
    fn memory_zero_total_yields_zero_pct() {
        let plan = plan_for(ActivityId::MEMORY);
        let p = zeros(&plan);
        let c = zeros(&plan);
        let ctx = ComputeContext::new(1000);
        assert_eq!(
            compute(
                ActivityId::MEMORY,
                mem_col::MEMUSED_PCT,
                &plan,
                &p,
                &c,
                &ctx
            )
            .unwrap(),
            0.0
        );
        assert_eq!(
            compute(
                ActivityId::MEMORY,
                mem_col::SWPUSED_PCT,
                &plan,
                &p,
                &c,
                &ctx
            )
            .unwrap(),
            0.0
        );
        assert_eq!(
            compute(ActivityId::MEMORY, mem_col::SWPCAD_PCT, &plan, &p, &c, &ctx).unwrap(),
            0.0
        );
    }

    #[test]
    fn swap_columns() {
        let plan = plan_for(ActivityId::MEMORY);
        let p = zeros(&plan);
        let mut c = zeros(&plan);
        put(&plan, &mut c, mem_col::KBSWPTOTAL, 2_000);
        put(&plan, &mut c, mem_col::KBSWPFREE, 500);
        put(&plan, &mut c, mem_col::KBSWPCAD, 300);
        let ctx = ComputeContext::new(1000);
        assert_eq!(
            compute(ActivityId::MEMORY, mem_col::KBSWPUSED, &plan, &p, &c, &ctx).unwrap(),
            1_500.0
        );
        assert_eq!(
            compute(
                ActivityId::MEMORY,
                mem_col::SWPUSED_PCT,
                &plan,
                &p,
                &c,
                &ctx
            )
            .unwrap(),
            75.0
        );
        assert_eq!(
            compute(ActivityId::MEMORY, mem_col::SWPCAD_PCT, &plan, &p, &c, &ctx).unwrap(),
            20.0,
            "300 / (2000-500)"
        );
    }

    // ---- A_HUGE ----

    #[test]
    fn huge_used_and_pct() {
        let plan = plan_for(ActivityId::HUGE);
        let p = zeros(&plan);
        let mut c = zeros(&plan);
        put(&plan, &mut c, huge_col::KBHUGTOTAL, 4_000);
        put(&plan, &mut c, huge_col::KBHUGFREE, 1_000);
        let ctx = ComputeContext::new(1000);
        assert_eq!(
            compute(ActivityId::HUGE, huge_col::KBHUGUSED, &plan, &p, &c, &ctx).unwrap(),
            3_000.0
        );
        assert_eq!(
            compute(ActivityId::HUGE, huge_col::HUGUSED_PCT, &plan, &p, &c, &ctx).unwrap(),
            75.0
        );
    }

    // ---- A_DISK ----

    /// `%util` は保存値 (ms/s) を 10 で割る。`areq-sz` は 2 で、`aqu-sz` は 1000 で割る。
    #[test]
    fn disk_scaling_matches_upstream() {
        let plan = plan_for(ActivityId::DISK);
        let p = zeros(&plan);
        let mut c = zeros(&plan);
        // 1 秒 (itv = 100 cs) で 10 I/O、512 セクタ read、tot_ticks 500 ms、rq_ticks 2000
        put(&plan, &mut c, disk_col::TPS, 10);
        put(&plan, &mut c, disk_col::RKB, 512);
        put(&plan, &mut c, disk_col::UTIL_PCT, 500);
        put(&plan, &mut c, disk_col::AQU_SZ, 2_000);
        put(&plan, &mut c, disk_col::RD_TICKS, 40);
        let ctx = ComputeContext::new(100);

        assert_eq!(
            compute(ActivityId::DISK, disk_col::TPS, &plan, &p, &c, &ctx).unwrap(),
            10.0
        );
        assert_eq!(
            compute(ActivityId::DISK, disk_col::RKB, &plan, &p, &c, &ctx).unwrap(),
            256.0,
            "512 セクタ/s = 256 kB/s"
        );
        assert_eq!(
            compute(ActivityId::DISK, disk_col::UTIL_PCT, &plan, &p, &c, &ctx).unwrap(),
            50.0,
            "500 ms/s = 50%"
        );
        assert_eq!(
            compute(ActivityId::DISK, disk_col::AQU_SZ, &plan, &p, &c, &ctx).unwrap(),
            2.0,
            "2000 ms/s / 1000"
        );
        assert_eq!(
            compute(ActivityId::DISK, disk_col::AREQ_SZ, &plan, &p, &c, &ctx).unwrap(),
            25.6,
            "512 セクタ / 10 I/O / 2"
        );
        assert_eq!(
            compute(ActivityId::DISK, disk_col::AWAIT, &plan, &p, &c, &ctx).unwrap(),
            4.0,
            "40 ms / 10 I/O"
        );
    }

    /// I/O が増えていないときは `areq-sz` / `await` は 0。
    #[test]
    fn disk_derived_is_zero_without_completed_io() {
        let plan = plan_for(ActivityId::DISK);
        let p = zeros(&plan);
        let c = zeros(&plan);
        let ctx = ComputeContext::new(100);
        assert_eq!(
            compute(ActivityId::DISK, disk_col::AREQ_SZ, &plan, &p, &c, &ctx).unwrap(),
            0.0
        );
        assert_eq!(
            compute(ActivityId::DISK, disk_col::AWAIT, &plan, &p, &c, &ctx).unwrap(),
            0.0
        );
    }

    #[test]
    fn disk_util_clamps_on_decrease() {
        let plan = plan_for(ActivityId::DISK);
        let mut p = zeros(&plan);
        let mut c = zeros(&plan);
        put(&plan, &mut p, disk_col::UTIL_PCT, 1_000);
        put(&plan, &mut c, disk_col::UTIL_PCT, 100);
        let ctx = ComputeContext::new(100);
        assert_eq!(
            compute(ActivityId::DISK, disk_col::UTIL_PCT, &plan, &p, &c, &ctx).unwrap(),
            0.0
        );
    }

    // ---- A_NET_DEV ----

    /// `%ifutil` はバイト毎秒から計算する (1024 で割ってはいけない)。
    #[test]
    fn ifutil_uses_bytes_per_second() {
        // 全二重 1 Gbit/s、rx = 12.5 MB/s → 100 Mbit/s → 10%
        assert_eq!(ifutil(12_500_000.0, 0.0, 1000, duplex::FULL), 10.0);
        // 半二重は rx + tx
        assert_eq!(ifutil(6_250_000.0, 6_250_000.0, 1000, duplex::HALF), 10.0);
        // 全二重は max(rx, tx)
        assert_eq!(ifutil(6_250_000.0, 6_250_000.0, 1000, duplex::FULL), 5.0);
        // speed 不明は 0
        assert_eq!(ifutil(1.0e9, 0.0, 0, duplex::FULL), 0.0);
    }

    #[test]
    fn net_dev_rx_is_bytes_per_second_and_ifutil_matches() {
        let plan = plan_for(ActivityId::NET_DEV);
        let p = zeros(&plan);
        let mut c = zeros(&plan);
        put(&plan, &mut c, net_dev_col::RXKB, 12_500_000);
        put(&plan, &mut c, net_dev_col::SPEED, 1_000);
        put(&plan, &mut c, net_dev_col::DUPLEX, duplex::FULL);
        let ctx = ComputeContext::new(100);
        assert_eq!(
            compute(ActivityId::NET_DEV, net_dev_col::RXKB, &plan, &p, &c, &ctx).unwrap(),
            12_500_000.0,
            "列名は rxkB/s だが値はバイト毎秒"
        );
        assert_eq!(
            compute(
                ActivityId::NET_DEV,
                net_dev_col::IFUTIL_PCT,
                &plan,
                &p,
                &c,
                &ctx
            )
            .unwrap(),
            10.0
        );
    }

    // ---- A_FS ----

    #[test]
    fn fs_usage_columns() {
        let plan = plan_for(ActivityId::FS);
        let p = zeros(&plan);
        let mut c = zeros(&plan);
        put(&plan, &mut c, fs_col::TOTAL, 1_000);
        put(&plan, &mut c, fs_col::MB_FREE, 400);
        put(&plan, &mut c, fs_col::AVAILABLE, 300);
        put(&plan, &mut c, fs_col::INODES_TOTAL, 100);
        put(&plan, &mut c, fs_col::IFREE, 25);
        let ctx = ComputeContext::new(100);
        assert_eq!(
            compute(ActivityId::FS, fs_col::MB_USED, &plan, &p, &c, &ctx).unwrap(),
            600.0
        );
        assert_eq!(
            compute(ActivityId::FS, fs_col::USED_PCT, &plan, &p, &c, &ctx).unwrap(),
            60.0
        );
        assert_eq!(
            compute(ActivityId::FS, fs_col::UNPRIV_USED_PCT, &plan, &p, &c, &ctx).unwrap(),
            70.0,
            "%ufsused は f_bavail 基準"
        );
        assert_eq!(
            compute(ActivityId::FS, fs_col::IUSED, &plan, &p, &c, &ctx).unwrap(),
            75.0
        );
        assert_eq!(
            compute(ActivityId::FS, fs_col::IUSED_PCT, &plan, &p, &c, &ctx).unwrap(),
            75.0
        );
    }

    // ---- A_QUEUE / A_PSI / A_PWR ----

    /// `ldavg-*` は 100 倍固定小数なので 100 で割る。
    #[test]
    fn load_average_is_divided_by_100() {
        let plan = plan_for(ActivityId::QUEUE);
        let p = zeros(&plan);
        let mut c = zeros(&plan);
        put(&plan, &mut c, queue_col::LDAVG_1, 123);
        put(&plan, &mut c, queue_col::RUNQ_SZ, 4);
        let ctx = ComputeContext::new(100);
        assert_eq!(
            compute(ActivityId::QUEUE, queue_col::LDAVG_1, &plan, &p, &c, &ctx).unwrap(),
            1.23
        );
        assert_eq!(
            compute(ActivityId::QUEUE, queue_col::RUNQ_SZ, &plan, &p, &c, &ctx).unwrap(),
            4.0,
            "runq-sz は瞬時値そのまま"
        );
    }

    /// PSI の圧力は `Δµs / (100 × itv)`。`S_VALUE` は使わない。
    #[test]
    fn psi_pressure_uses_micro_seconds_over_100_itv() {
        let plan = plan_for(ActivityId::PSI_CPU);
        let mut p = zeros(&plan);
        let mut c = zeros(&plan);
        // 10 秒 (itv = 1000 cs) のうち 1 秒 (1e6 µs) 停止 → 10%
        put(&plan, &mut p, psi_col::SOME_TOTAL, 0);
        put(&plan, &mut c, psi_col::SOME_TOTAL, 1_000_000);
        put(&plan, &mut c, psi_col::SOME_10, 1_234);
        let ctx = ComputeContext::new(1_000);
        assert_eq!(
            compute(
                ActivityId::PSI_CPU,
                psi_col::SOME_TOTAL,
                &plan,
                &p,
                &c,
                &ctx
            )
            .unwrap(),
            10.0
        );
        assert_eq!(
            compute(ActivityId::PSI_CPU, psi_col::SOME_10, &plan, &p, &c, &ctx).unwrap(),
            12.34,
            "移動平均は 100 倍固定小数"
        );
    }

    #[test]
    fn psi_io_has_full_columns_too() {
        let plan = plan_for(ActivityId::PSI_IO);
        let p = zeros(&plan);
        let mut c = zeros(&plan);
        put(&plan, &mut c, psi_col::FULL_TOTAL, 500_000);
        put(&plan, &mut c, psi_col::FULL_300, 50);
        let ctx = ComputeContext::new(1_000);
        assert_eq!(
            compute(ActivityId::PSI_IO, psi_col::FULL_TOTAL, &plan, &p, &c, &ctx).unwrap(),
            5.0
        );
        assert_eq!(
            compute(ActivityId::PSI_IO, psi_col::FULL_300, &plan, &p, &c, &ctx).unwrap(),
            0.5
        );
    }

    #[test]
    fn pwr_cpu_mhz_is_divided_by_100() {
        let plan = plan_for(ActivityId::PWR_CPU);
        let p = zeros(&plan);
        let mut c = zeros(&plan);
        put(&plan, &mut c, pwr_cpu_col::MHZ, 280_000);
        let ctx = ComputeContext::new(100);
        assert_eq!(
            compute(ActivityId::PWR_CPU, pwr_cpu_col::MHZ, &plan, &p, &c, &ctx).unwrap(),
            2_800.0
        );
    }

    /// センサ値は IEEE-754 の `double` として保存されている。
    #[test]
    fn sensor_values_are_ieee754_doubles() {
        let plan = plan_for(ActivityId::PWR_TEMP);
        let p = zeros(&plan);
        let mut c = zeros(&plan);
        put(&plan, &mut c, temp_col::DEGC, 42.5f64.to_bits());
        put(&plan, &mut c, temp_col::MIN, 20.0f64.to_bits());
        put(&plan, &mut c, temp_col::MAX, 70.0f64.to_bits());
        let ctx = ComputeContext::new(100);
        assert_eq!(
            compute(ActivityId::PWR_TEMP, temp_col::DEGC, &plan, &p, &c, &ctx).unwrap(),
            42.5
        );
        assert_eq!(
            compute(ActivityId::PWR_TEMP, temp_col::PCT, &plan, &p, &c, &ctx).unwrap(),
            45.0,
            "(42.5-20)/(70-20)*100"
        );
    }

    #[test]
    fn fan_drpm_is_rpm_minus_min() {
        let plan = plan_for(ActivityId::PWR_FAN);
        let p = zeros(&plan);
        let mut c = zeros(&plan);
        put(&plan, &mut c, fan_col::RPM, 1_500.0f64.to_bits());
        put(&plan, &mut c, fan_col::RPM_MIN, 1_200.0f64.to_bits());
        let ctx = ComputeContext::new(100);
        assert_eq!(
            compute(ActivityId::PWR_FAN, fan_col::DRPM, &plan, &p, &c, &ctx).unwrap(),
            300.0
        );
    }

    #[test]
    fn sensor_pct_is_zero_when_range_is_empty() {
        let plan = plan_for(ActivityId::PWR_IN);
        let p = zeros(&plan);
        let mut c = zeros(&plan);
        put(&plan, &mut c, in_col::VOLTS, 12.0f64.to_bits());
        put(&plan, &mut c, in_col::MIN, 12.0f64.to_bits());
        put(&plan, &mut c, in_col::MAX, 12.0f64.to_bits());
        let ctx = ComputeContext::new(100);
        assert_eq!(
            compute(ActivityId::PWR_IN, in_col::PCT, &plan, &p, &c, &ctx).unwrap(),
            0.0
        );
    }

    /// `maxpower` は bMaxPower (2 mA 単位) を 2 倍して mA にする。
    #[test]
    fn usb_max_power_is_doubled() {
        let plan = plan_for(ActivityId::PWR_USB);
        let p = zeros(&plan);
        let mut c = zeros(&plan);
        put(&plan, &mut c, usb_col::MAX_POWER, 250);
        let ctx = ComputeContext::new(100);
        assert_eq!(
            compute(ActivityId::PWR_USB, usb_col::MAX_POWER, &plan, &p, &c, &ctx).unwrap(),
            500.0
        );
    }

    /// `capacity` は signed char。`cap/min` は %/分。
    #[test]
    fn bat_capacity_is_signed_and_cap_per_min_is_per_minute() {
        let plan = plan_for(ActivityId::PWR_BAT);
        let mut p = zeros(&plan);
        let mut c = zeros(&plan);
        put(&plan, &mut p, bat_col::CAP_PCT, 80);
        put(&plan, &mut c, bat_col::CAP_PCT, 78);
        // itv = 6000 cs = 60 秒 → -2 %/分
        let ctx = ComputeContext::new(6_000);
        assert_eq!(
            compute(ActivityId::PWR_BAT, bat_col::CAP_PCT, &plan, &p, &c, &ctx).unwrap(),
            78.0
        );
        assert_eq!(
            compute(
                ActivityId::PWR_BAT,
                bat_col::CAP_PER_MIN,
                &plan,
                &p,
                &c,
                &ctx
            )
            .unwrap(),
            -2.0
        );
        assert_eq!(signed_byte(0xff), -1);
    }

    // ---- A_PWR_FREQ ----

    /// `wghMHz` は `Σ((freq/1000) × Δtis) / ΣΔtis`。`freq/1000` は整数除算。
    #[test]
    fn weighted_mhz_uses_integer_khz_to_mhz() {
        let plan = plan_for(ActivityId::PWR_FREQ);
        let mut p0 = zeros(&plan);
        let mut p1 = zeros(&plan);
        let mut c0 = zeros(&plan);
        let mut c1 = zeros(&plan);
        // 2 スロット: 1_500_999 kHz を 100 cs、800_000 kHz を 300 cs
        put(&plan, &mut c0, freq_col::FREQ_KHZ, 1_500_999);
        put(&plan, &mut c0, freq_col::TIME_IN_STATE, 100);
        put(&plan, &mut p0, freq_col::TIME_IN_STATE, 0);
        put(&plan, &mut c1, freq_col::FREQ_KHZ, 800_000);
        put(&plan, &mut c1, freq_col::TIME_IN_STATE, 300);
        put(&plan, &mut p1, freq_col::TIME_IN_STATE, 0);

        let got = weighted_mhz(&plan, &[p0, p1], &[c0, c1]).unwrap();
        // (1500 * 100 + 800 * 300) / 400 = 975
        assert_eq!(got, 975.0);
    }

    #[test]
    fn weighted_mhz_stops_at_unused_slot() {
        let plan = plan_for(ActivityId::PWR_FREQ);
        let p = vec![zeros(&plan), zeros(&plan)];
        let c = vec![zeros(&plan), zeros(&plan)];
        assert_eq!(weighted_mhz(&plan, &p, &c).unwrap(), 0.0);
    }

    /// 行列型の派生列は 1 item では計算できないことを型で示す。
    #[test]
    fn wgh_mhz_needs_item_group_from_single_item_path() {
        let plan = plan_for(ActivityId::PWR_FREQ);
        let p = zeros(&plan);
        let c = zeros(&plan);
        let ctx = ComputeContext::new(100);
        assert_eq!(
            compute(ActivityId::PWR_FREQ, freq_col::WGH_MHZ, &plan, &p, &c, &ctx),
            Err(ComputeIssue::NeedsItemGroup)
        );
    }

    // ---- 未実装 / 非数値の区別 ----

    /// 識別子列は数値ではないことを明示する (0 を返さない)。
    #[test]
    fn identity_columns_are_not_numeric() {
        let plan = plan_for(ActivityId::DISK);
        let p = zeros(&plan);
        let c = zeros(&plan);
        let ctx = ComputeContext::new(100);
        assert_eq!(
            compute(ActivityId::DISK, disk_col::DEVICE, &plan, &p, &c, &ctx),
            Err(ComputeIssue::NotNumeric)
        );
        assert_eq!(
            compute(ActivityId::DISK, disk_col::MAJOR, &plan, &p, &c, &ctx),
            Err(ComputeIssue::NotNumeric)
        );
    }

    /// 前サンプルが無い / 不連続なカウンタは理由付きで失敗する (0 を返さない)。
    #[test]
    fn counters_report_why_they_cannot_be_computed() {
        let plan = plan_for(ActivityId::PCSW);
        let p = zeros(&plan);
        let c = zeros(&plan);
        let first = ComputeContext {
            has_prev: false,
            ..ComputeContext::new(100)
        };
        assert_eq!(
            compute(ActivityId::PCSW, 0, &plan, &p, &c, &first),
            Err(ComputeIssue::Discontinuous(Discontinuity::FirstSample))
        );
        let restart = ComputeContext {
            continuous: false,
            ..ComputeContext::new(100)
        };
        assert_eq!(
            compute(ActivityId::PCSW, 0, &plan, &p, &c, &restart),
            Err(ComputeIssue::Discontinuous(Discontinuity::Restart))
        );
    }

    /// 定義済み activity の「sar に現れる数値列」がすべて計算できることを機械的に確認する。
    ///
    /// 新しい activity や列を追加したときに `NotImplemented` が残っていれば落ちる。
    #[test]
    fn every_sar_numeric_column_is_implemented() {
        let enc = SourceEncoding::new(Endian::Little, LayoutAbi::LP64);
        let mut missing: Vec<String> = Vec::new();

        for def in crate::layout::registry::all() {
            let Some(rev) = def.latest() else { continue };
            let plan = DecodePlan::build(def, rev, rev.size_lp64, 2, 2, &enc).unwrap();
            let zero = ItemSnapshot {
                key: None,
                texts: Vec::new(),
                values: vec![Availability::Present(0); plan.fields.len()],
            };
            let ctx = ComputeContext {
                tick_total: if def.id == ActivityId::CPU {
                    Some(100)
                } else {
                    None
                },
                ..ComputeContext::new(100)
            };
            for (i, col) in def.columns.iter().enumerate() {
                // sar に現れない内部フィールドと識別子列は対象外
                if col.sar_header.is_empty() || col.kind == ValueKind::Identity {
                    continue;
                }
                match column_value(def.id, i, col, &plan, &zero, &zero, &ctx) {
                    Err(ComputeIssue::NotImplemented) => {
                        missing.push(format!("{} {}", def.id, col.public_name));
                    }
                    // 行列型の派生列は専用関数から計算する (weighted_mhz)
                    Err(ComputeIssue::NeedsItemGroup) => {}
                    _ => {}
                }
            }
        }

        assert!(
            missing.is_empty(),
            "未実装の sar 列が残っている: {missing:?}"
        );
    }

    // ---- カウンタ幅 (指摘 1) ----

    /// 指定した revision のデコード計画を作る (旧世代 / 32bit ライタの再現用)。
    fn plan_for_revision(id: ActivityId, magic: u32, size: usize, abi: LayoutAbi) -> DecodePlan {
        let def = lookup(id).expect("定義がある");
        let rev = def
            .revision_for_magic_and_size(magic, size)
            .expect("revision がある");
        let enc = SourceEncoding::new(Endian::Little, abi);
        DecodePlan::build(def, rev, size, 1, 1, &enc).expect("計画を作れる")
    }

    /// 公開名から列添字を引く。
    fn column_of(id: ActivityId, public_name: &str) -> usize {
        lookup(id)
            .expect("定義がある")
            .columns
            .iter()
            .position(|c| c.public_name == public_name)
            .expect("列がある")
    }

    /// **回帰テスト (指摘 1)**: `unsigned int` のカウンタが一周した区間で、
    /// レートが 1.84×10¹⁹ ではなく正しい値になる。
    ///
    /// `A_SERIAL` の `rx` は全世代で `unsigned int` (4 バイト)。
    /// 本家も `unsigned int` 同士の減算なので 32bit で畳まれ、同じ値になる。
    #[test]
    fn counter_rate_folds_32bit_wraparound() {
        let plan = plan_for(ActivityId::SERIAL);
        let col = column_of(ActivityId::SERIAL, "rcvin");
        assert_eq!(
            plan.column_bits(col),
            Some(CounterBits::B32),
            "4 バイトのフィールドは B32 として計画に載る"
        );

        let mut p = zeros(&plan);
        let mut c = zeros(&plan);
        put(&plan, &mut p, col, 4_294_967_290);
        put(&plan, &mut c, col, 4);
        let ctx = ComputeContext::new(100); // 1 秒

        assert_eq!(
            compute(ActivityId::SERIAL, col, &plan, &p, &c, &ctx).unwrap(),
            10.0,
            "6 (0 へ) + 4 = 10 件/秒"
        );
        // 64bit のまま引いていたときの値 (修正前はこれが表示されていた)
        assert!(
            crate::series::delta::s_value(4_294_967_290, 4, 100) > 1.0e19,
            "64bit 減算は 1.8e19 台になる"
        );
    }

    /// **回帰テスト (指摘 1)**: `disk_derived` の tick 差分も幅を意識する。
    ///
    /// `rd_ticks` / `rq_ticks` は全世代で `unsigned int`。64bit で引くと
    /// `await` が 10¹⁸ ms、`aqu-sz` が 10¹⁶ という表示不能な値になっていた。
    #[test]
    fn disk_tick_derivations_fold_32bit_wraparound() {
        let plan = plan_for(ActivityId::DISK);
        for col in [disk_col::RD_TICKS, disk_col::AQU_SZ, disk_col::UTIL_PCT] {
            assert_eq!(
                plan.column_bits(col),
                Some(CounterBits::B32),
                "tick 群は unsigned int"
            );
        }

        let mut p = zeros(&plan);
        let mut c = zeros(&plan);
        // rd_ticks: 2^32-6 → 4 で一周 = 10 ms
        put(&plan, &mut p, disk_col::RD_TICKS, 4_294_967_290);
        put(&plan, &mut c, disk_col::RD_TICKS, 4);
        // rq_ticks: 2^32-1000 → 1000 で一周 = 2000 ms
        put(&plan, &mut p, disk_col::AQU_SZ, 4_294_966_296);
        put(&plan, &mut c, disk_col::AQU_SZ, 1_000);
        // 1 秒間に 10 件完了
        put(&plan, &mut c, disk_col::TPS, 10);
        let ctx = ComputeContext::new(100);

        assert_eq!(
            compute(ActivityId::DISK, disk_col::AWAIT, &plan, &p, &c, &ctx).unwrap(),
            1.0,
            "10 ms / 10 I/O"
        );
        assert_eq!(
            compute(ActivityId::DISK, disk_col::AQU_SZ, &plan, &p, &c, &ctx).unwrap(),
            2.0,
            "2000 ms/s / 1000"
        );
        // %util は本家が「減っていたら 0」と決めているので一周も 0 のまま (03 §5.1)
        put(&plan, &mut p, disk_col::UTIL_PCT, 4_294_967_290);
        put(&plan, &mut c, disk_col::UTIL_PCT, 4);
        assert_eq!(
            compute(ActivityId::DISK, disk_col::UTIL_PCT, &plan, &p, &c, &ctx).unwrap(),
            0.0,
            "逆行クランプは本家どおり維持する"
        );
    }

    /// 32bit ライタが書いた `unsigned long` のカウンタも一周を復元する。
    ///
    /// 旧 `A_NET_DEV` (`magic 0x8a`) の `rx_bytes` は `unsigned long` で、
    /// 32bit マシンが書いたファイルでは有効 4 バイト = 一周も 2^32 で起きる。
    ///
    /// **ここは本家と値が変わる箇所**である。本家は読み込み先が
    /// `unsigned long` (読み手側で 8 バイト) なので 64bit で引き、
    /// 一周した区間で 1.8e19 を表示する。01 §8.3 が 32bit ライタの
    /// `unsigned long` の扱いを「本家より正しい」側に倒すと決めているので、
    /// 差分でも同じ方針を採る (逆行が一周で説明できる唯一のケース)。
    #[test]
    fn counter_of_32bit_writer_long_field_folds_at_2_pow_32() {
        let plan = plan_for_revision(ActivityId::NET_DEV, 0x8a, 72, LayoutAbi::I386);
        let col = column_of(ActivityId::NET_DEV, "rx_bytes_per_sec");
        assert_eq!(
            plan.column_bits(col),
            Some(CounterBits::B32),
            "sa_sizeof_long == 4 のファイルでは unsigned long の有効幅は 4"
        );

        let mut p = zeros(&plan);
        let mut c = zeros(&plan);
        put(&plan, &mut p, col, 4_294_967_290);
        put(&plan, &mut c, col, 4);
        let ctx = ComputeContext::new(100);
        assert_eq!(
            compute(ActivityId::NET_DEV, col, &plan, &p, &c, &ctx).unwrap(),
            10.0,
            "10 バイト/秒"
        );

        // 同じ revision を 64bit ライタのファイルとして読むと幅は 64bit になる
        let plan64 = plan_for_revision(ActivityId::NET_DEV, 0x8a, 72, LayoutAbi::LP64);
        assert_eq!(plan64.column_bits(col), Some(CounterBits::B64));
    }

    /// 一周として説明できない逆行の扱いは方針で分かれる。
    ///
    /// `A_PCSW` のカウンタは `unsigned long long` で逆行クランプも無いため、
    /// 本家は符号なし減算の巨大値をそのまま表示する。互換出力はそれに合わせ、
    /// 独自出力では値を作らずに理由を返す (外れ値が平均や p95 を壊さないように)。
    #[test]
    fn unexplainable_decrease_is_refused_only_in_strict_policy() {
        let plan = plan_for(ActivityId::PCSW);
        let col = 0;
        let meta = &lookup(ActivityId::PCSW).unwrap().columns[col];
        assert_eq!(plan.column_bits(col), Some(CounterBits::B64));

        let mut p = zeros(&plan);
        let mut c = zeros(&plan);
        put(&plan, &mut p, col, 1_000_000);
        put(&plan, &mut c, col, 10);
        let ctx = ComputeContext::new(100);

        let compat = column_value(ActivityId::PCSW, col, meta, &plan, &p, &c, &ctx).unwrap();
        assert!(compat > 1.0e19, "本家と同じ符号なし減算の結果: {compat}");
        assert_eq!(
            column_value_strict(ActivityId::PCSW, col, meta, &plan, &p, &c, &ctx),
            Err(ComputeIssue::Discontinuous(
                Discontinuity::AmbiguousDecrease
            )),
            "64bit カウンタの一周は現実的でないので差分を作らない"
        );
    }

    // ---- 欠落の扱い (指摘 2) ----

    /// **回帰テスト (指摘 2)**: `availablekb` を持たない世代のファイルで
    /// `%memused` が 100% にならないこと。
    ///
    /// - 互換出力: `frmkb` を代替に使う (本家の `sadf -c` と同じ。02 §8)
    /// - 独自 API: 欠落として返す (0 で埋めない)
    #[test]
    fn memused_without_available_field_is_not_100_percent() {
        // v11.1.3〜v11.5.2 の A_MEMORY (16 フィールド / 128 バイト) に availablekb は無い
        let plan = plan_for_revision(ActivityId::MEMORY, 0x8a, 128, LayoutAbi::LP64);
        assert_eq!(
            plan.column_value(&zeros(&plan).values, mem_col::KBAVAIL),
            Availability::UnsupportedBySource,
            "この世代には availablekb が無い"
        );

        let p = zeros(&plan);
        let mut c = zeros(&plan);
        put(&plan, &mut c, mem_col::KBMEMTOTAL, 8_000_000);
        put(&plan, &mut c, mem_col::KBMEMFREE, 2_000_000);
        let ctx = ComputeContext::new(100);
        let meta = |col: usize| &lookup(ActivityId::MEMORY).unwrap().columns[col];

        // --- 互換出力: その世代の sar が出していた値 (tlmkb - frmkb) ---
        let pct = column_value(
            ActivityId::MEMORY,
            mem_col::MEMUSED_PCT,
            meta(mem_col::MEMUSED_PCT),
            &plan,
            &p,
            &c,
            &ctx,
        )
        .unwrap();
        assert_ne!(pct, 100.0, "欠落を 0 として扱うと 100% になる");
        assert_eq!(pct, 75.0, "(8,000,000 - 2,000,000) / 8,000,000");
        assert_eq!(
            column_value(
                ActivityId::MEMORY,
                mem_col::KBMEMUSED,
                meta(mem_col::KBMEMUSED),
                &plan,
                &p,
                &c,
                &ctx
            )
            .unwrap(),
            6_000_000.0,
            "kbmemused も総量そのものにはならない"
        );

        // --- 独自 API: 欠落を維持する ---
        for col in [mem_col::MEMUSED_PCT, mem_col::KBMEMUSED] {
            assert_eq!(
                column_value_strict(ActivityId::MEMORY, col, meta(col), &plan, &p, &c, &ctx),
                Err(ComputeIssue::UnsupportedBySource),
                "availablekb 無しでは使用量は求められない"
            );
        }

        // --- availablekb を持つ世代では両方針が一致する ---
        let modern = plan_for(ActivityId::MEMORY);
        let mut mc = zeros(&modern);
        put(&modern, &mut mc, mem_col::KBMEMTOTAL, 8_000_000);
        put(&modern, &mut mc, mem_col::KBMEMFREE, 1_000_000);
        put(&modern, &mut mc, mem_col::KBAVAIL, 4_000_000);
        let mp = zeros(&modern);
        for col in [mem_col::MEMUSED_PCT, mem_col::KBMEMUSED] {
            assert_eq!(
                column_value(ActivityId::MEMORY, col, meta(col), &modern, &mp, &mc, &ctx),
                column_value_strict(ActivityId::MEMORY, col, meta(col), &modern, &mp, &mc, &ctx),
                "フィールドが揃っていれば方針で値は変わらない"
            );
        }
    }

    /// **回帰テスト (指摘 2)**: `Average:` 行も 100% にならないこと。
    ///
    /// 累積器は「欠落」を知らないため、`availablekb` の合計が 0 のまま
    /// `tlmkb - 0` を計算すると平均行だけ 100% になる。
    #[test]
    fn average_memused_without_available_field_falls_back_to_free() {
        let plan = plan_for_revision(ActivityId::MEMORY, 0x8a, 128, LayoutAbi::LP64);
        let mut last = zeros(&plan);
        put(&plan, &mut last, mem_col::KBMEMTOTAL, 1_000);
        put(&plan, &mut last, mem_col::KBMEMFREE, 250);

        let mut acc = ItemAccum::new(24);
        // frmkb = 250, 250, 250 → 平均 250
        for _ in 0..3 {
            acc.add(mem_col::KBMEMFREE, Some(250), Some(250.0));
            // availablekb は列が無いので何も累積されない
            acc.add(mem_col::KBAVAIL, None, None);
            acc.count += 1;
        }

        let pct =
            average_ratio(ActivityId::MEMORY, mem_col::MEMUSED_PCT, &plan, &acc, &last).unwrap();
        assert_ne!(pct, 100.0);
        assert_eq!(pct, 75.0, "(1000 - 250) / 1000");
        assert_eq!(
            average_ratio(ActivityId::MEMORY, mem_col::KBMEMUSED, &plan, &acc, &last).unwrap(),
            750.0
        );
    }

    /// **指摘 2 の点検**: `speed` を持たない世代の `%ifutil`。
    ///
    /// 互換出力は本家と同じ 0.0 (`speed == 0` = 不明 → 0)、
    /// 独自 API では「この世代では求められない」を返す。
    #[test]
    fn ifutil_without_speed_field_is_zero_only_in_compat() {
        // v10.1.2〜v10.1.6 の A_NET_DEV (128 バイト) に speed / duplex は無い
        let plan = plan_for_revision(ActivityId::NET_DEV, 0x8b, 128, LayoutAbi::LP64);
        let col = net_dev_col::IFUTIL_PCT;
        let meta = &lookup(ActivityId::NET_DEV).unwrap().columns[col];
        let p = zeros(&plan);
        let mut c = zeros(&plan);
        put(&plan, &mut c, net_dev_col::RXKB, 1_000_000);
        let ctx = ComputeContext::new(100);

        assert_eq!(
            column_value(ActivityId::NET_DEV, col, meta, &plan, &p, &c, &ctx).unwrap(),
            0.0,
            "本家は speed 不明を 0.0 として表示する (03 §5.2)"
        );
        assert_eq!(
            column_value_strict(ActivityId::NET_DEV, col, meta, &plan, &p, &c, &ctx),
            Err(ComputeIssue::UnsupportedBySource),
            "リンク速度が分からないのに利用率 0% と見せない"
        );
    }

    // ---- Average: (方式 B′) ----

    /// `%memused` の平均は整数除算を経由し、`tlmkb` は最終サンプルの値を使う。
    #[test]
    fn average_memused_pct_uses_integer_division() {
        let plan = plan_for(ActivityId::MEMORY);
        let mut last = zeros(&plan);
        put(&plan, &mut last, mem_col::KBMEMTOTAL, 1_000);

        let mut acc = ItemAccum::new(24);
        // availablekb = 301, 300, 300 → 合計 901、整数平均 300 (切り捨て)
        for v in [301u64, 300, 300] {
            acc.add(mem_col::KBAVAIL, Some(v), Some(v as f64));
            acc.count += 1;
        }
        let pct =
            average_ratio(ActivityId::MEMORY, mem_col::MEMUSED_PCT, &plan, &acc, &last).unwrap();
        assert_eq!(pct, 70.0, "(1000 - 300) / 1000 — 300.33 ではない");

        // kbavail 列自身は浮動小数除算
        let avail = acc.mean(mem_col::KBAVAIL).unwrap();
        assert!((avail - 300.333_333).abs() < 1e-5, "{avail}");
    }

    #[test]
    fn average_huge_pct_uses_float_division() {
        let plan = plan_for(ActivityId::HUGE);
        let last = zeros(&plan);
        let mut acc = ItemAccum::new(6);
        for (total, free) in [(1_000u64, 300u64), (1_000, 100)] {
            acc.add(huge_col::KBHUGTOTAL, Some(total), Some(total as f64));
            acc.add(huge_col::KBHUGFREE, Some(free), Some(free as f64));
            acc.count += 1;
        }
        let pct =
            average_ratio(ActivityId::HUGE, huge_col::HUGUSED_PCT, &plan, &acc, &last).unwrap();
        assert_eq!(pct, 80.0, "(1000 - 200) / 1000");
        let used =
            average_ratio(ActivityId::HUGE, huge_col::KBHUGUSED, &plan, &acc, &last).unwrap();
        assert_eq!(used, 800.0);
    }

    /// `%temp` の平均は min/max に**最終サンプルの値**を使う (累積しない)。
    #[test]
    fn average_temp_pct_uses_last_min_max() {
        let plan = plan_for(ActivityId::PWR_TEMP);
        let mut last = zeros(&plan);
        put(&plan, &mut last, temp_col::MIN, 0.0f64.to_bits());
        put(&plan, &mut last, temp_col::MAX, 100.0f64.to_bits());

        let mut acc = ItemAccum::new(6);
        for v in [30.0f64, 50.0] {
            acc.add(temp_col::DEGC, None, Some(v));
            acc.count += 1;
        }
        let pct = average_ratio(ActivityId::PWR_TEMP, temp_col::PCT, &plan, &acc, &last).unwrap();
        assert_eq!(pct, 40.0);
    }

    #[test]
    fn accumulator_without_samples_reports_missing() {
        let plan = plan_for(ActivityId::HUGE);
        let last = zeros(&plan);
        let acc = ItemAccum::new(6);
        assert_eq!(acc.mean(0), Err(ComputeIssue::MissingInSample));
        assert_eq!(
            average_ratio(ActivityId::HUGE, huge_col::HUGUSED_PCT, &plan, &acc, &last),
            Err(ComputeIssue::MissingInSample)
        );
    }
}

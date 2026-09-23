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
//! ## 出力経路ごとに計算を持たせない
//!
//! 「表示値を出す計算」は 1 区間ぶんだけではない。次の 4 種類があり、
//! **どれもこの層に置く**。sar 互換出力にしか無い計算があると、
//! 同じファイルから出した sadf / 独自出力 / 期間集計の値がずれる。
//!
//! | 何を出すか | 入口 |
//! |---|---|
//! | 1 区間の表示値 | [`column_value`] / [`column_value_strict`] |
//! | 期間集計の素材と表示単位 | [`rate_sample`] → [`rate_from_totals`] |
//! | `sadf` だけにある別単位の列 | [`SadfUnitColumn`] / [`sadf_unit_value`] |
//! | 行列型の 1 行分 (`wghMHz` / `A_IRQ`) | [`matrix_row_values`] |
//!
//! `A_CPU` は分母が特殊なので、どの経路も値を出す前に
//! [`cpu_interval`] (個別 CPU) / [`aggregate_cpu`] (CPU "all") を通す。
//! ここに「補正済みの前値・分母・オフライン / tickless 判定」がまとまっている。
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
//!   世代別の代替フィールド / 符号なし整数の折り返し) を再現する。
//!   `sar` / `sadf` 互換出力用。
//! - [`MissingPolicy::Strict`] — [`column_value_strict`]。欠落は
//!   [`ComputeIssue::UnsupportedBySource`] として返す。独自出力・集計用。
//!
//! 方針は「欠落の埋め方」だけでなく「互換のための整数演算をどこまで真似るか」も
//! 決める。`await` の分子は本家が `unsigned int` で足すので 2³² で折り返すが、
//! 折り返した値は待ち時間として意味を持たないため、独自出力では折り返さない
//! (`disk_sum_delta` の `SumWidth`)。
//!
//! 典拠: `docs/format/03-output-format.md` 第 I 部 §1.2〜§1.5、第 III 部 §7。

use std::borrow::Cow;

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
//
// **表は必ず全列を並べる。参照されていない定数も消さない。**
// 後ろの定数の値は前の定数が存在することを前提にしているので、
// 「誰も使っていないから」で 1 行消すと残りの添字の意味が変わる。
// 識別子列 (`NAME` / `IFACE` / `MOUNTPOINT` / `MANUFACTURER` / `PRODUCT` など) は
// 数値として読まないため参照が付かないが、列番号としては存在している。
// dead-code 検査はこれらを未参照として挙げるが、対応は不要である。
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
///
/// ## ゼロ補完は「計算層の責務」である (指摘 5)
///
/// 以前は直接列の欠落を [`ComputeIssue::UnsupportedBySource`] として返し、
/// **出力層がそれを 0.0 に読み替えていた**。その結果 `sar` 互換テキストは
/// `0.00` を出すのに `sadf` は空欄 / `null` を出すという、
/// 互換出力どうしの不統一が生まれていた。
///
/// 本家は「足りないフィールドを 0 埋めした構造体」で計算を完了するので、
/// ゼロ補完は書式化ではなく**計算の一部**である。よって
/// [`MissingPolicy::Compat`] では計算層が `Ok(0.0)` まで出し、
/// [`MissingPolicy::Strict`] だけが欠落を返す。
/// 呼び出し側が欠落の種類で分岐したい場合は [`missing_kind`] を使う。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MissingPolicy {
    /// 本家互換: その世代の `sar` が表示した値を再現する (欠落は 0 埋め)。
    #[default]
    Compat,
    /// 厳密: 欠落を代替値で埋めず、[`ComputeIssue`] として報告する。
    Strict,
}

/// 欠落の種類 (指摘 5)。
///
/// 互換出力は [`MissingKind::ZeroFilled`] を `0.00` として出し、
/// 独自出力は欠落のまま残す、という使い分けを呼び出し側でできるようにする。
///
/// [`MissingPolicy::Compat`] で計算した場合、`ZeroFilled` 相当の欠落は
/// 計算層の中で 0 に埋められるのでここには現れない。
/// この分類が要るのは [`MissingPolicy::Strict`] の結果を
/// 互換出力へ流し込むときと、欠落理由を出力へ書き出すときである。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissingKind {
    /// 本家がゼロ補完して表示する欠落 (03 §1.9-1)。
    ///
    /// 「その世代のファイルにフィールドが無い」だけで、
    /// 本家は 0 埋めした構造体で計算を完了する。
    ZeroFilled,
    /// ゼロ補完してはいけない欠落。
    ///
    /// レコードに値が無い / 差分が取れない / 数値ではない / 未実装。
    /// 0 を与えると「正常に 0」と区別できなくなる。
    Absent,
}

/// [`ComputeIssue`] を「本家がゼロ補完する欠落」かどうかで分類する (指摘 5)。
///
/// 典拠: 03 §1.9-1。本家は期待する型別本数よりファイル側が少なければ
/// 足りない分を 0 埋めした構造体で計算するため、
/// discard 統計を持たない旧 `A_IO` の `dtps` / `bdscd` は `0.00` と表示される。
/// 一方 `MissingInSample` や不連続は本家でも値が出ない (行そのものが無い)。
///
/// 「値の欠落」ではない理由 (識別子列 / 行列型 / 未実装) は `None` を返す。
/// これらは 0 でも欠測でもなく、**呼び出し方の問題**なので
/// 欠落として集計に数えてはいけない。
#[inline]
pub fn missing_kind(issue: ComputeIssue) -> Option<MissingKind> {
    match issue {
        // その世代のファイルにフィールドが無いだけ = 本家は 0 埋めして計算する
        ComputeIssue::UnsupportedBySource => Some(MissingKind::ZeroFilled),
        // このサンプルでは値が読めなかった / 差分が取れない = 本家なら行が無い
        ComputeIssue::MissingInSample | ComputeIssue::Discontinuous(_) => Some(MissingKind::Absent),
        // 値の欠落ではない (数値でない列 / item 群が必要 / 未実装)
        ComputeIssue::NotNumeric | ComputeIssue::NeedsItemGroup | ComputeIssue::NotImplemented => {
            None
        }
    }
}

/// この列がこのファイルに存在するか (指摘 5)。
///
/// [`MissingPolicy::Compat`] は本家に合わせて欠落を 0 に埋めるので、
/// 返ってきた `0.0` が「観測した 0」なのか「ゼロ補完」なのかは値から判らない。
/// 由来が必要な呼び出し側 (欠落を注記したい独自出力など) はこれで確かめる。
///
/// 偽になるのは「その世代のファイルにフィールドが無い」場合だけで、
/// サンプル単位の欠測 ([`ComputeIssue::MissingInSample`]) は判定できない。
pub fn column_is_present(plan: &DecodePlan, column: usize) -> bool {
    matches!(plan.column_fields.get(column), Some(Some(_)))
}

// ============================================================================
// 未使用スロットの判定
// ============================================================================

/// この item スロットは**未使用**か。
///
/// `file_activity.nr` は「採取時に確保した枠数」であって、常に全部が
/// 埋まっているわけではない。レコードごとの item 数 (`has_nr`) を持たない
/// 世代では枠数しか手掛かりが無いため、素直に回すと空き枠まで行になる
/// (`expected.data-10.3.1` は 12 デバイスだが枠は 20 あり、
/// reSARch は差分の 8 行を `dev0-0` として出していた = golden 比較 ⑤)。
///
/// 本家は数を数えるのではなく、各 `print_*_stats()` の先頭で
/// **その activity 固有の番兵**を見て `continue` する。ここはその表である。
///
/// | activity | 未使用の条件 | 由来 |
/// |---|---|---|
/// | `A_DISK` | `major + minor == 0` | `print_disk_stats()` |
/// | `A_FS` | `f_blocks == 0` | `print_filesystem_stats()` |
/// | `A_NET_DEV` / `A_NET_EDEV` | インターフェース名が空 | `print_net_dev_stats()` |
/// | `A_NET_FC` | `fchost_name` が空 | `count_stats_fchost()` |
/// | `A_SERIAL` | 旧 magic 0x8a の `line == 0` | `print_tty_stats()` |
/// | `A_PWR_USB` | `bus_nr == 0` | `print_pwr_usb_stats()` |
/// | `A_PWR_CPU` | `cpufreq == 0` (オフライン) | `print_pwr_cpufreq_stats()` |
/// | `A_NET_SOFT` | CPU ごとの 5 カウンタと backlog_len が全 0 (オフライン) | `print_softnet_stats()` |
///
/// `A_NET_SOFT` の `index == 0` は CPU "all" なので常に使用中とする。
/// `blg_len` も本家のオンライン判定に含める。
///
/// フィールドがその世代に無い場合は 0 とみなす。**無いフィールドが
/// スロットを「使用中」にすることはない**ため、判定は保守的に働く。
///
/// 独自出力はこれを使わない。空き枠も観測結果として出す方が、
/// 「本家が表示を省く規則」を独自形式に持ち込むより説明しやすい。
pub fn is_unused_item(
    id: ActivityId,
    index: usize,
    plan: &DecodePlan,
    item: &ItemSnapshot,
) -> bool {
    /// 欠落・欠測を 0 とみなして生値を取る。
    #[inline]
    fn v(plan: &DecodePlan, item: &ItemSnapshot, column: usize) -> u64 {
        raw_column(plan, item, column).unwrap_or(0)
    }
    /// 文字列列が空 (または取得できない) か。
    #[inline]
    fn text_empty(item: &ItemSnapshot, index: usize) -> bool {
        item.texts
            .get(index)
            .map(|t| t.as_deref().unwrap_or("").is_empty())
            .unwrap_or(true)
    }

    match id {
        ActivityId::DISK => v(plan, item, disk_col::MAJOR) + v(plan, item, disk_col::MINOR) == 0,
        ActivityId::FS => v(plan, item, fs_col::TOTAL) == 0,
        // インターフェース名 / FC ホスト名は 1 本目の文字列フィールド
        ActivityId::NET_DEV | ActivityId::NET_EDEV | ActivityId::NET_FC => text_empty(item, 0),
        ActivityId::SERIAL => plan.serial_line_offset && v(plan, item, 0) == 0,
        ActivityId::PWR_USB => v(plan, item, usb_col::BUS) == 0,
        ActivityId::PWR_CPU => v(plan, item, pwr_cpu_col::MHZ) == 0,
        ActivityId::NET_SOFT => {
            index != 0
                && v(plan, item, soft_col::TOTAL) == 0
                && v(plan, item, soft_col::DROPD) == 0
                && v(plan, item, soft_col::SQUEEZD) == 0
                && v(plan, item, soft_col::RX_RPS) == 0
                && v(plan, item, soft_col::FLW_LIM) == 0
                && v(plan, item, soft_col::BLG_LEN) == 0
        }
        _ => false,
    }
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

/// 方針に従って生値を取り出す。
///
/// 派生計算の**主要項** (分子・分母・被減数) と直接列の読み出しに使う。
/// この位置で欠落を無条件に 0 で埋めると値そのものが嘘になる
/// (「総量 - 欠落」は総量に等しくなり使用率 100% を、
/// 「欠落 / 総量」は 0% を、静かに作り出す)。
///
/// [`MissingPolicy::Compat`] では本家と同じ 0 埋めを行い (03 §1.9-1)、
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
///
/// 欠落時は方針に従う。`Compat` では 0 埋めした構造体の `double` = `0.0` になる
/// (ビットパターン 0 は IEEE-754 の `+0.0` なので、そのまま読み替えてよい)。
#[inline]
fn raw_f64(
    plan: &DecodePlan,
    item: &ItemSnapshot,
    column: usize,
    policy: MissingPolicy,
) -> Computed {
    primary_input(plan, item, column, policy).map(f64::from_bits)
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

/// `A_CPU` の割合の分母になる tick 合計差分 (`deltot_jiffies`、03 §1.4.2)。
///
/// **`plan` を取る形に変更した (指摘 1)。**
/// 以前は「item の全フィールドの差分を単純合計する」実装だったため、
/// `user` に内包される `guest` と `nice` に内包される `guest_nice` を
/// 二重に計上していた。Δuser=100 / Δguest=50 / 他 0 のとき `%user` は
/// 本来 100% だが、分母が 150 になり約 66.67% になっていた
/// (仮想マシン稼働中の使用率が過小に出る)。
///
/// 仕様の `get_per_cpu_interval()` は
///
/// - `user, nice, sys, iowait, idle, steal, hardirq, softirq` の **8 フィールドだけ**を足す
///   (`guest` / `guest_nice` は足さない、03 §1.10-4)
/// - 前サンプルの `iowait` / `idle` を補正してから差分を取る (03 §1.3.3)
/// - `guest` が `user` を上回る誤差を `ishift` で足し戻す
///
/// より詳しい情報 (オフライン / tickless の判定、補正後の前値) が必要な場合は
/// [`cpu_interval`] を使う。この関数はその `tick_total` だけを返す薄い入口で、
/// 前サンプルの clone を作らない。
pub fn tick_total(plan: &DecodePlan, prev: &ItemSnapshot, curr: &ItemSnapshot) -> u64 {
    cpu_fix(plan, prev, curr).interval
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
    if let Some(v) = special_direct(id, column, plan, prev, curr, ctx, policy) {
        return v;
    }

    // その世代のファイルに無いフィールドは方針に従う。
    // 本家は 0 埋めした構造体で計算を完了するので、互換では `Ok(0.0)` まで出す
    // (以前は出力層が `Err` を 0.0 に読み替えていて sar と sadf で不統一だった)。
    let curr_v = primary_input(plan, curr, column, policy)?;

    match meta.kind {
        // ゲージは差分化しない
        ValueKind::Gauge => Ok(gauge_scale(id, column).apply(curr_v)),
        // 識別子は数値として扱わない
        ValueKind::Identity => Err(ComputeIssue::NotNumeric),
        ValueKind::Counter => {
            if !ctx.has_prev {
                return Err(ComputeIssue::Discontinuous(Discontinuity::FirstSample));
            }
            if !ctx.continuous {
                return Err(ComputeIssue::Discontinuous(Discontinuity::Restart));
            }
            let prev_v = primary_input(plan, prev, column, policy)?;

            // 逆行クランプ (`ll_sp_value` / 各 print 関数の明示クランプ)。
            // ラップの復元より先に判定する: クランプ対象の列は本家が
            // 「減っていたら 0」と決めているので、そこに合わせる。
            if curr_v < prev_v && clamps_decrease(id, column, ctx) {
                return Ok(0.0);
            }

            // 差分は列の元の幅で取る。32bit カウンタ (`unsigned int` のフィールドや
            // 32bit ライタの `unsigned long`) を 64bit のまま引くと、一周した入力で
            // 差分が 1.84e19 になる。`unsigned int` は本家と一致するが、
            // 32bit ライタの `unsigned long` の幅復元は 64bit 読み手の本家と意図的に異なる。
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
            Ok(rate / rate_divisor(id, column))
        }
    }
}

/// ゲージ列の保存値を表示単位に直す規則 (**保存値 → 表示単位**)。
///
/// カーネル / sysstat が固定小数で保存している値を実単位に戻す。
/// 期間集計でゲージ列を平均する場合、区間値 ([`column_value`]) には
/// この換算が既に掛かっているので二重に掛けてはいけない。
///
/// **係数ではなく本家の演算そのものを表す。** 本家は `(double) x / 100` と
/// 割り算で書いており、`0.01` は二進で正確に表せないため、掛け算に置き換えると
/// 丸め境界で 1 桁ずれる (`ldavg` の保存値 435 は本家が `4.35` → `--dec=1` で
/// `4.3`、`× 0.01` だと `4.3500000000000005` → `4.4`)。
/// 平均も同じで、本家は `Σx / (avg_count × 100)` と**合計を 1 回だけ割る**
/// ([`GaugeScale::mean`])。表示値 `x / 100` を足し込んでから割ると別の丸めになる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GaugeScale {
    /// 保存値がそのまま表示単位。
    Unit,
    /// `(double) x / d` — 固定小数 (`load_avg_*` / `cpufreq` / PSI の移動平均)。
    Divide(u32),
    /// `(unsigned long long) (x << 1)` のような `unsigned int` 幅の整数倍
    /// (`bMaxPower` は 2 mA 単位)。倍率を掛けた結果は 32bit で折り返す。
    MultiplyU32(u32),
}

impl GaugeScale {
    /// 瞬時値 1 個を表示単位に直す。
    #[inline]
    pub fn apply(self, raw: u64) -> f64 {
        match self {
            GaugeScale::Unit => raw as f64,
            GaugeScale::Divide(d) => raw as f64 / f64::from(d),
            GaugeScale::MultiplyU32(k) => f64::from((raw as u32).wrapping_mul(k)),
        }
    }

    /// 保存値の合計から `Average:` の平均値を出す (本家の `dispavg` 分岐)。
    ///
    /// [`GaugeScale::Divide`] は本家どおり `(double) Σx / (avg_count × d)`。
    /// 分母の積は本家の `unsigned long` の積なので整数で掛けてから `double` にする。
    /// `count == 0` のときは `None` (平均が無い)。
    #[inline]
    pub fn mean(self, raw_sum: u64, count: u64) -> Option<f64> {
        if count == 0 {
            return None;
        }
        Some(match self {
            GaugeScale::Unit => raw_sum as f64 / count as f64,
            GaugeScale::Divide(d) => raw_sum as f64 / count.wrapping_mul(u64::from(d)) as f64,
            // 本家に平均は無い (`A_PWR_USB` は最後に観測した値を再掲する)。
            // 呼ばれた場合は保存値の平均を換算する。
            GaugeScale::MultiplyU32(k) => raw_sum as f64 * f64::from(k) / count as f64,
        })
    }
}

/// ゲージ列の換算規則 ([`GaugeScale`])。
pub fn gauge_scale(id: ActivityId, column: usize) -> GaugeScale {
    match id {
        // load_avg_* は 100 倍固定小数 (03 §1.5.4)
        ActivityId::QUEUE
            if matches!(
                column,
                queue_col::LDAVG_1 | queue_col::LDAVG_5 | queue_col::LDAVG_15
            ) =>
        {
            GaugeScale::Divide(100)
        }
        // cpufreq は MHz × 100 (03 §id=30)
        ActivityId::PWR_CPU if column == pwr_cpu_col::MHZ => GaugeScale::Divide(100),
        // PSI の移動平均は 100 倍固定小数 (03 §1.5.3)
        ActivityId::PSI_CPU | ActivityId::PSI_IO | ActivityId::PSI_MEM
            if is_psi_moving_average(column) =>
        {
            GaugeScale::Divide(100)
        }
        // bMaxPower は 2 mA 単位 (03 §id=36)
        ActivityId::PWR_USB if column == usb_col::MAX_POWER => GaugeScale::MultiplyU32(2),
        _ => GaugeScale::Unit,
    }
}

/// カウンタ列の「生のレート」を割る除数 (**保存値 → 表示単位**)。
///
/// 生のレートとは `S_VALUE(prev, curr, 分母)` = `Δ / 分母 × 100` のこと。
/// この関数の戻り値で割ったものが [`ColumnMeta::unit`] の単位になる。
///
/// **逆数を掛けてはいけない。** 本家は `S_VALUE(...) / 1000.0` や
/// `xds.util / 10.0` と**割り算**で書いており、`0.1` や `0.001` は二進で
/// 正確に表せないため、掛け算に置き換えると丸め境界で 1 桁ずれる
/// (`Δtot_ticks = 7710`、`itv = 60000` の `%util` は本家が `1.2849999…` → `1.28`、
/// `× 0.1` だと `1.2850000…1` → `1.29`)。
///
/// **期間集計もこの除数で必ず割る。** 割り忘れると平均 `rkB/s` が 2 倍、
/// `aqu-sz` が 1,000 倍、`%util` が 10 倍、PSI が 10,000 倍になる (指摘 2)。
/// 「差分合計 ÷ 分母合計」から表示値を出す入口は [`rate_from_totals`]。
///
/// 典拠: 03 §id=11 (ディスク列の式) / §1.5.3 (PSI) / §1.10-11 / §1.10-15。
pub fn rate_divisor(id: ActivityId, column: usize) -> f64 {
    match id {
        ActivityId::DISK => match column {
            // セクタ (512 B) → kB (`S_VALUE(...) / 2`)
            disk_col::RKB | disk_col::WKB | disk_col::DKB => 2.0,
            // rq_ticks は「I/O 待ちの重み付きミリ秒」。`S_VALUE(...) / 1000.0` で平均キュー長
            disk_col::AQU_SZ => 1000.0,
            // tot_ticks はミリ秒。1000 ms/s = 100% なので `xds.util / 10.0`
            disk_col::UTIL_PCT => 10.0,
            _ => 1.0,
        },
        // PSI の累積 µs 列は本家が `Δµs / (100 × itv)` を出す (03 §1.5.3)。
        // 生のレート `Δ/itv×100` からは 10000 で割って同じ量になる
        // (区間値は [`psi_pressure`] が本家の式そのままで計算する)。
        // この除数があるので、集計は PSI も他のカウンタと同じ経路で扱える。
        ActivityId::PSI_CPU | ActivityId::PSI_IO | ActivityId::PSI_MEM if is_psi_total(column) => {
            10_000.0
        }
        _ => 1.0,
    }
}

/// 期間集計の「差分合計 ÷ 分母合計」を**表示単位のレート**に直す (指摘 2)。
///
/// 集計側は区間ごとの [`RateSample`] を足し込むだけでよく、
/// 単位換算をこちらに寄せることで「出力形式ごとにスケーリングが抜ける」事故を防ぐ。
///
/// 1 区間だけを足し込んだ場合、結果はその区間の [`column_value`] と一致する
/// (テスト `single_interval_aggregate_matches_instant_value` がそれを固定している)。
///
/// 分母の合計が 0 のときは `None` (「0% だった」と報告してはいけない)。
pub fn rate_from_totals(
    id: ActivityId,
    column: usize,
    delta_total: u128,
    denom_total: u128,
) -> Option<f64> {
    if denom_total == 0 {
        return None;
    }
    let rate = delta_total as f64 / denom_total as f64 * 100.0;
    Some(rate / rate_divisor(id, column))
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
#[allow(clippy::too_many_arguments)]
fn special_direct(
    id: ActivityId,
    column: usize,
    plan: &DecodePlan,
    prev: &ItemSnapshot,
    curr: &ItemSnapshot,
    ctx: &ComputeContext,
    policy: MissingPolicy,
) -> Option<Computed> {
    match id {
        // --- PSI: 累積マイクロ秒。S_VALUE は使わない (03 §1.5.3) ---
        ActivityId::PSI_CPU | ActivityId::PSI_IO | ActivityId::PSI_MEM if is_psi_total(column) => {
            Some(psi_pressure(plan, prev, curr, column, ctx, policy))
        }
        // --- IEEE-754 double で保存されているセンサ値 ---
        ActivityId::PWR_FAN if matches!(column, fan_col::RPM | fan_col::RPM_MIN) => {
            Some(raw_f64(plan, curr, column, policy))
        }
        ActivityId::PWR_TEMP
            if matches!(column, temp_col::DEGC | temp_col::MIN | temp_col::MAX) =>
        {
            Some(raw_f64(plan, curr, column, policy))
        }
        ActivityId::PWR_IN if matches!(column, in_col::VOLTS | in_col::MIN | in_col::MAX) => {
            Some(raw_f64(plan, curr, column, policy))
        }
        // --- A_PWR_BAT の capacity は signed char (03 §id=43) ---
        ActivityId::PWR_BAT if column == bat_col::CAP_PCT => {
            Some(primary_input(plan, curr, column, policy).map(|v| f64::from(signed_byte(v))))
        }
        // --- kbavail は列そのものにも代替規則が要る ---
        //
        // `availablekb` が無い世代 (`0x2170` / `0x2171` / 一部の `0x2173`) では、
        // 派生列 (`kbmemused` / `%memused`) だけでなく **`kbavail` 列自身**も
        // `frmkb` で代用する。本家の構造体は `availablekb` が存在しないため
        // `print_memory_stats()` が `frmkb` を読み、`kbmemfree` と同じ値が出る
        // (`expected.data-10.3.1` の `5646540   5646540`)。
        //
        // 汎用経路に落とすと欠落が `Compat` で `0` に埋まり、
        // 本家が出す値と食い違う (golden 比較 ④)。
        ActivityId::MEMORY if column == mem_col::KBAVAIL => {
            Some(memory_available(plan, curr, policy).map(|v| v as f64))
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
///
/// **同じ係数が [`rate_divisor`] にもある。** 区間値はここで直接 `100 × itv` で
/// 割るが、期間集計は生のレート `Δ/itv×100` を `10000` で割って同じ値に到達する。
/// 2 箇所に分かれているのは本家の式の形をそのまま残すためで、
/// 両者が一致することは `single_interval_aggregate_matches_instant_value` が固定する。
fn psi_pressure(
    plan: &DecodePlan,
    prev: &ItemSnapshot,
    curr: &ItemSnapshot,
    column: usize,
    ctx: &ComputeContext,
    policy: MissingPolicy,
) -> Computed {
    if !ctx.has_prev {
        return Err(ComputeIssue::Discontinuous(Discontinuity::FirstSample));
    }
    if !ctx.continuous {
        return Err(ComputeIssue::Discontinuous(Discontinuity::Restart));
    }
    let c = primary_input(plan, curr, column, policy)?;
    let p = primary_input(plan, prev, column, policy)?;
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
        ActivityId::CPU => cpu_derived(column, plan, prev, curr, ctx, policy),
        ActivityId::MEMORY => memory_derived(column, plan, curr, policy),
        ActivityId::HUGE => huge_derived(column, plan, curr, policy),
        ActivityId::DISK => disk_derived(column, plan, prev, curr, ctx, policy),
        ActivityId::FS => fs_derived(column, plan, curr, policy),
        ActivityId::NET_DEV => net_dev_derived(column, plan, prev, curr, ctx, policy),
        ActivityId::PWR_FAN => fan_derived(column, plan, curr, policy),
        ActivityId::PWR_TEMP => temp_derived(column, plan, curr, policy),
        ActivityId::PWR_IN => in_derived(column, plan, curr, policy),
        ActivityId::PWR_FREQ if column == freq_col::WGH_MHZ => Err(ComputeIssue::NeedsItemGroup),
        ActivityId::PWR_BAT => bat_derived(column, plan, prev, curr, ctx, policy),
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
    policy: MissingPolicy,
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
            let p = primary_input(plan, prev, cpu_col::SYS, policy)?
                .wrapping_add(raw_or_zero(plan, prev, cpu_col::IRQ)?)
                .wrapping_add(raw_or_zero(plan, prev, cpu_col::SOFT)?);
            let c = primary_input(plan, curr, cpu_col::SYS, policy)?
                .wrapping_add(raw_or_zero(plan, curr, cpu_col::IRQ)?)
                .wrapping_add(raw_or_zero(plan, curr, cpu_col::SOFT)?);
            Ok(llsp(p, c))
        }
        // %usr = user - guest (`-u ALL`)
        cpu_col::USR => {
            let p = primary_input(plan, prev, cpu_col::USER, policy)?.wrapping_sub(raw_or_zero(
                plan,
                prev,
                cpu_col::GUEST,
            )?);
            let c = primary_input(plan, curr, cpu_col::USER, policy)?.wrapping_sub(raw_or_zero(
                plan,
                curr,
                cpu_col::GUEST,
            )?);
            Ok(llsp(p, c))
        }
        // %nice = nice - guest_nice (`-u ALL`)
        cpu_col::NICE_EXCL_GNICE => {
            let p = primary_input(plan, prev, cpu_col::NICE, policy)?.wrapping_sub(raw_or_zero(
                plan,
                prev,
                cpu_col::GNICE,
            )?);
            let c = primary_input(plan, curr, cpu_col::NICE, policy)?.wrapping_sub(raw_or_zero(
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
            let total = primary_input(plan, curr, mem_col::KBMEMTOTAL, policy)?;
            let avail = memory_available(plan, curr, policy)?;
            Ok(total.wrapping_sub(avail) as f64)
        }
        mem_col::MEMUSED_PCT => {
            let total = primary_input(plan, curr, mem_col::KBMEMTOTAL, policy)?;
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
            let total = primary_input(plan, curr, mem_col::KBMEMTOTAL, policy)?
                .wrapping_add(raw_or_zero(plan, curr, mem_col::KBSWPTOTAL)?);
            // comkb は分子そのもの。欠落を 0 にすると %commit が常に 0% になる
            let com = primary_input(plan, curr, mem_col::KBCOMMIT, policy)?;
            Ok(if total != 0 {
                sp_value(0, com, total)
            } else {
                0.0
            })
        }
        mem_col::KBSWPUSED => {
            let total = primary_input(plan, curr, mem_col::KBSWPTOTAL, policy)?;
            let free = primary_input(plan, curr, mem_col::KBSWPFREE, policy)?;
            Ok(total.wrapping_sub(free) as f64)
        }
        mem_col::SWPUSED_PCT => {
            let total = primary_input(plan, curr, mem_col::KBSWPTOTAL, policy)?;
            if total == 0 && policy == MissingPolicy::Strict {
                return Err(ComputeIssue::MissingInSample);
            }
            let free = primary_input(plan, curr, mem_col::KBSWPFREE, policy)?;
            Ok(if total != 0 {
                sp_value(free, total, total)
            } else {
                0.0
            })
        }
        mem_col::SWPCAD_PCT => {
            let total = primary_input(plan, curr, mem_col::KBSWPTOTAL, policy)?;
            if total == 0 && policy == MissingPolicy::Strict {
                return Err(ComputeIssue::MissingInSample);
            }
            let free = primary_input(plan, curr, mem_col::KBSWPFREE, policy)?;
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
fn huge_derived(
    column: usize,
    plan: &DecodePlan,
    curr: &ItemSnapshot,
    policy: MissingPolicy,
) -> Computed {
    let total = primary_input(plan, curr, huge_col::KBHUGTOTAL, policy)?;
    let free = primary_input(plan, curr, huge_col::KBHUGFREE, policy)?;
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
    policy: MissingPolicy,
) -> Computed {
    if !ctx.has_prev {
        return Err(ComputeIssue::Discontinuous(Discontinuity::FirstSample));
    }
    if !ctx.continuous {
        return Err(ComputeIssue::Discontinuous(Discontinuity::Restart));
    }

    let ios_p = primary_input(plan, prev, disk_col::TPS, policy)?;
    let ios_c = primary_input(plan, curr, disk_col::TPS, policy)?;
    // 完了 I/O が増えていないときは 0 (0 除算回避も兼ねる)。
    // 本家も `nr_ios_c > nr_ios_p` を素の比較で行い、偽なら 0.0 を返す (03 §5.1)。
    if ios_c <= ios_p {
        return Ok(0.0);
    }
    let d_ios = (ios_c - ios_p) as f64;

    match column {
        // areq-sz = Σ(Δsect) / Δnr_ios / 2 (セクタ → kB)
        disk_col::AREQ_SZ => {
            let sect = disk_sum_delta(
                plan,
                prev,
                curr,
                [disk_col::RKB, disk_col::WKB, disk_col::DKB],
                SumWidth::Bits64,
            )?;
            Ok(sect / d_ios / 2.0)
        }
        // await = Σ(Δticks) / Δnr_ios (ミリ秒、追加スケーリングなし)
        disk_col::AWAIT => {
            let ticks = disk_sum_delta(
                plan,
                prev,
                curr,
                [disk_col::RD_TICKS, disk_col::WR_TICKS, disk_col::DC_TICKS],
                // 本家は `unsigned int` 同士を足すので和も 32bit で折り返す
                match policy {
                    MissingPolicy::Compat => SumWidth::Bits32,
                    MissingPolicy::Strict => SumWidth::Bits64,
                },
            )?;
            Ok(ticks / d_ios)
        }
        _ => Err(ComputeIssue::NotImplemented),
    }
}

/// 差分の**和**を取る幅 (指摘 4)。
///
/// 本家の `compute_ext_disk_stats()` は C の整数式なので、和の型は
/// フィールドの型で決まる (02 §7 の `stats_disk`)。
///
/// | 分子 | フィールドの型 | 和の型 |
/// |---|---|---|
/// | `await` | `rd_ticks` / `wr_ticks` / `dc_ticks` = `unsigned int` | `unsigned int` (mod 2³²) |
/// | `arqsz` | `rd_sect` / `wr_sect` / `dc_sect` = `unsigned long` | `unsigned long` (読み手で 64bit) |
///
/// つまり `await` の分子だけが 2³² で折り返す。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SumWidth {
    /// `unsigned int` の和 (mod 2³²)。
    Bits32,
    /// `unsigned long` / `unsigned long long` の和 (mod 2⁶⁴)。
    Bits64,
}

/// ディスク派生列の分子 (複数列の差分の和) を求める。
///
/// 各列の差分は**その列の幅**で取る。`rd_ticks` などは全世代で `unsigned int`
/// (02 §7) なので、64bit のまま引くと一周した区間で差分が 1.84e19 になり
/// `await` が 10¹⁸ ms になる。本家は `unsigned int` 同士の減算なので
/// 32bit で一周が畳まれる。
///
/// **和の幅は [`SumWidth`] で分ける (指摘 4)。** 互換出力の `await` は
/// 本家と同じく `unsigned int` で足すため、分子が 2³² を超えると折り返す
/// (Δ = 3,000,000,000 と 2,000,000,000 なら本家は 705,032,704)。
/// 和を f64 で取ると 5,000,000,000 になり本家と食い違う。
///
/// 一方 [`MissingPolicy::Strict`] (独自出力・集計) では折り返しを再現しない。
/// 「本家がそう出す」ことと「その値が正しい」ことは別で、
/// 折り返した分子は待ち時間として意味を持たないため、
/// 独自出力では 64bit で足して実際の合計を保つ。
fn disk_sum_delta(
    plan: &DecodePlan,
    prev: &ItemSnapshot,
    curr: &ItemSnapshot,
    cols: [usize; 3],
    width: SumWidth,
) -> Result<f64, ComputeIssue> {
    let mut acc: u64 = 0;
    for c in cols {
        // 欠落は加算項なので 0 でよい (discard 統計を持たない世代の `dc_ticks`)。
        // 本家も 0 埋めした構造体で足している (03 §1.9-1)。
        let p = raw_or_zero(plan, prev, c)?;
        let n = raw_or_zero(plan, curr, c)?;
        let d = wrapping_delta(p, n, counter_bits(plan, c));
        acc = match width {
            // `unsigned int` の加算 = mod 2^32
            SumWidth::Bits32 => u64::from((acc as u32).wrapping_add(d as u32)),
            SumWidth::Bits64 => acc.wrapping_add(d),
        };
    }
    // f64 化は加算を終えた後。先に f64 にすると折り返しが再現できない。
    Ok(acc as f64)
}

/// `A_FS` の派生列。`f_*` はバイト単位のゲージ (03 §id=37)。
fn fs_derived(
    column: usize,
    plan: &DecodePlan,
    curr: &ItemSnapshot,
    policy: MissingPolicy,
) -> Computed {
    match column {
        fs_col::MB_USED => {
            let blocks = primary_input(plan, curr, fs_col::TOTAL, policy)?;
            let free = primary_input(plan, curr, fs_col::MB_FREE, policy)?;
            Ok(blocks.wrapping_sub(free) as f64)
        }
        fs_col::USED_PCT => {
            let blocks = primary_input(plan, curr, fs_col::TOTAL, policy)?;
            let free = primary_input(plan, curr, fs_col::MB_FREE, policy)?;
            Ok(if blocks != 0 {
                sp_value(free, blocks, blocks)
            } else {
                0.0
            })
        }
        fs_col::UNPRIV_USED_PCT => {
            let blocks = primary_input(plan, curr, fs_col::TOTAL, policy)?;
            let avail = primary_input(plan, curr, fs_col::AVAILABLE, policy)?;
            Ok(if blocks != 0 {
                sp_value(avail, blocks, blocks)
            } else {
                0.0
            })
        }
        fs_col::IUSED => {
            let files = primary_input(plan, curr, fs_col::INODES_TOTAL, policy)?;
            let ffree = primary_input(plan, curr, fs_col::IFREE, policy)?;
            Ok(files.wrapping_sub(ffree) as f64)
        }
        fs_col::IUSED_PCT => {
            let files = primary_input(plan, curr, fs_col::INODES_TOTAL, policy)?;
            let ffree = primary_input(plan, curr, fs_col::IFREE, policy)?;
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
        primary_input(plan, prev, net_dev_col::RXKB, policy)?,
        primary_input(plan, curr, net_dev_col::RXKB, policy)?,
        ctx.itv_cs,
        counter_bits(plan, net_dev_col::RXKB),
    );
    let tx = s_value_bits(
        primary_input(plan, prev, net_dev_col::TXKB, policy)?,
        primary_input(plan, curr, net_dev_col::TXKB, policy)?,
        ctx.itv_cs,
        counter_bits(plan, net_dev_col::TXKB),
    );
    // speed は分母 (Mbit/s)。この世代に `speed` が無ければ上の
    // [`primary_input`] が方針に従って処理するが、**フィールドがあって値が 0**
    // という場合が別にある。0 は「速度を取得できなかった」の意味で
    // (仮想デバイス、`ethtool` が速度を返さない NIC)、本家は
    // `speed == 0` のとき `%ifutil` に `0.00` を出す (03 §5.2)。
    let speed = primary_input(plan, curr, net_dev_col::SPEED, policy)?;
    // **分母が無いので率は作れない。**
    //
    // ここで 0.0 を返すと「利用率 0%」という別の意味の値になり、
    // 集計・検出では**有効な観測として数えられてしまう**
    // (固定条件の経路が「評価済み・検出なし」になる)。
    // 欠落と 0 を混同しない (`docs/design.md` §4)、
    // 評価できなかったものを「検出なし」にしない (同 §11.2 の規律 7)。
    //
    // 互換出力は本家が出す `0.00` を再現しなければならないので、
    // [`MissingPolicy::Compat`] ではこの分岐に入らず下の式へ進む。
    if speed == 0 && matches!(policy, MissingPolicy::Strict) {
        return Err(ComputeIssue::MissingInSample);
    }
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
fn fan_derived(
    column: usize,
    plan: &DecodePlan,
    curr: &ItemSnapshot,
    policy: MissingPolicy,
) -> Computed {
    if column != fan_col::DRPM {
        return Err(ComputeIssue::NotImplemented);
    }
    let rpm = raw_f64(plan, curr, fan_col::RPM, policy)?;
    let rpm_min = raw_f64(plan, curr, fan_col::RPM_MIN, policy)?;
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
fn temp_derived(
    column: usize,
    plan: &DecodePlan,
    curr: &ItemSnapshot,
    policy: MissingPolicy,
) -> Computed {
    if column != temp_col::PCT {
        return Err(ComputeIssue::NotImplemented);
    }
    Ok(range_pct(
        raw_f64(plan, curr, temp_col::DEGC, policy)?,
        raw_f64(plan, curr, temp_col::MIN, policy)?,
        raw_f64(plan, curr, temp_col::MAX, policy)?,
    ))
}

/// `A_PWR_IN` の派生列 (`%in`)。
fn in_derived(
    column: usize,
    plan: &DecodePlan,
    curr: &ItemSnapshot,
    policy: MissingPolicy,
) -> Computed {
    if column != in_col::PCT {
        return Err(ComputeIssue::NotImplemented);
    }
    Ok(range_pct(
        raw_f64(plan, curr, in_col::VOLTS, policy)?,
        raw_f64(plan, curr, in_col::MIN, policy)?,
        raw_f64(plan, curr, in_col::MAX, policy)?,
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
    policy: MissingPolicy,
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
    let p = i32::from(signed_byte(primary_input(
        plan,
        prev,
        bat_col::CAP_PCT,
        policy,
    )?));
    let c = i32::from(signed_byte(primary_input(
        plan,
        curr,
        bat_col::CAP_PCT,
        policy,
    )?));
    Ok(f64::from(c - p) * 6000.0 / ctx.itv_cs as f64)
}

// ============================================================================
// A_CPU の区間前処理 (tick 合計 / 前値補正 / オフライン・tickless 判定)
//
// **この前処理は sar 互換出力の専有物ではない (指摘 1)。**
// 以前は `per_cpu_interval()` を sar 互換テキストだけが呼び、sadf・独自出力・
// 集計は素の `tick_total()` を使っていた。その結果、同じファイルでも
// 出力形式によって CPU 使用率の分母が変わり、オフライン / tickless の
// 判定も形式ごとに違っていた。
//
// そこで「1 CPU 分の区間をどう解釈するか」を [`CpuInterval`] に一本化し、
// どの経路からも同じ判定・同じ分母を得られるようにする。
// 集約 (`all` 行) は [`aggregate_cpu`] が CPU ごとに補正してから合算する。
// ============================================================================

/// CPU の 1 フィールド分の差分。アンダーフローは 0 に潰す。
///
/// 12.8.0 で追加された修正 (03 §1.4.2)。これがないと 1 フィールドだけ
/// 逆行したときに巨大な interval になり、**全パーセントが 0.00 に丸められる**。
#[inline]
fn cpu_delta(prev: u64, curr: u64) -> u64 {
    curr.saturating_sub(prev)
}

/// この item がその列を持っていれば値を、無ければ 0 を返す。
///
/// CPU の tick フィールドは「無い世代」があり (`steal` / `guest` など)、
/// 本家はそこを 0 埋めした構造体で計算する (03 §1.9-1)。
#[inline]
fn cpu_field(plan: &DecodePlan, item: &ItemSnapshot, column: usize) -> u64 {
    match plan.column_value(&item.values, column) {
        Availability::Present(v) => v,
        _ => 0,
    }
}

/// `get_per_cpu_interval()` の計算結果 (snapshot を作らない内部表現)。
struct CpuFix {
    /// 補正後の前サンプルの `iowait` (書き換えが起きたときだけ `Some`)。
    iowait: Option<u64>,
    /// 補正後の前サンプルの `idle` (書き換えが起きたときだけ `Some`)。
    idle: Option<u64>,
    /// tick 合計 (`ishift` 込み) = 割合の分母。
    interval: u64,
    /// 現サンプルの tick 8 フィールドの絶対和 (オフライン判定用)。
    curr_sum: u64,
    /// 前サンプルの tick 8 フィールドの絶対和 (基準値の有無の判定用)。
    prev_sum: u64,
}

/// `get_per_cpu_interval()` 本体 (03 §1.4.2)。
///
/// 前サンプルを clone せずに「補正値」と「tick 合計」を求める。
/// snapshot が必要な経路 ([`per_cpu_interval`] / [`cpu_interval`]) だけが
/// clone を作る。
fn cpu_fix(plan: &DecodePlan, prev: &ItemSnapshot, curr: &ItemSnapshot) -> CpuFix {
    let (cu, cn, cg, cgn) = (
        cpu_field(plan, curr, cpu_col::USER),
        cpu_field(plan, curr, cpu_col::NICE),
        cpu_field(plan, curr, cpu_col::GUEST),
        cpu_field(plan, curr, cpu_col::GNICE),
    );
    let (pu, pn, pg, pgn) = (
        cpu_field(plan, prev, cpu_col::USER),
        cpu_field(plan, prev, cpu_col::NICE),
        cpu_field(plan, prev, cpu_col::GUEST),
        cpu_field(plan, prev, cpu_col::GNICE),
    );

    // guest が user に含まれる分の補正 (`ishift`)
    let mut ishift: u64 = 0;
    if cu >= pu && cu.wrapping_sub(cg) < pu.wrapping_sub(pg) {
        ishift = ishift.wrapping_add(pu.wrapping_sub(pg).wrapping_sub(cu.wrapping_sub(cg)));
    }
    if cn >= pn && cn.wrapping_sub(cgn) < pn.wrapping_sub(pgn) {
        ishift = ishift.wrapping_add(pn.wrapping_sub(pgn).wrapping_sub(cn.wrapping_sub(cgn)));
    }

    // CPU 復帰 / iowait 誤差の補正 (03 §1.3.3)
    let (c_iowait, p_iowait) = (
        cpu_field(plan, curr, cpu_col::IOWAIT),
        cpu_field(plan, prev, cpu_col::IOWAIT),
    );
    let (c_idle, p_idle) = (
        cpu_field(plan, curr, cpu_col::IDLE),
        cpu_field(plan, prev, cpu_col::IDLE),
    );
    let mut iowait = None;
    let mut idle = None;
    if c_iowait < p_iowait && p_iowait < CPU_OVERFLOW_THRESHOLD {
        iowait = Some(if c_idle > p_idle || p_idle >= CPU_OVERFLOW_THRESHOLD {
            c_iowait
        } else {
            0
        });
    }
    if c_idle < p_idle && p_idle < CPU_OVERFLOW_THRESHOLD {
        idle = Some(0);
    }

    // guest / guest_nice は user / nice に内包されるので足さない (03 §1.10-4)
    let mut interval: u64 = 0;
    let mut curr_sum: u64 = 0;
    let mut prev_sum: u64 = 0;
    for col in cpu_col::TICK_FIELDS {
        let c = match col {
            cpu_col::IOWAIT => c_iowait,
            cpu_col::IDLE => c_idle,
            _ => cpu_field(plan, curr, col),
        };
        let raw_p = match col {
            cpu_col::IOWAIT => p_iowait,
            cpu_col::IDLE => p_idle,
            _ => cpu_field(plan, prev, col),
        };
        // 差分は補正後の前値で取る。オフライン判定の絶対和は補正前の値で取る
        // (本家も `tot_jiffies_p` を `get_per_cpu_interval()` の前に数える)。
        let p = match col {
            cpu_col::IOWAIT => iowait.unwrap_or(raw_p),
            cpu_col::IDLE => idle.unwrap_or(raw_p),
            _ => raw_p,
        };
        interval = interval.wrapping_add(cpu_delta(p, c));
        curr_sum = curr_sum.wrapping_add(c);
        prev_sum = prev_sum.wrapping_add(raw_p);
    }

    CpuFix {
        iowait,
        idle,
        interval: interval.wrapping_add(ishift),
        curr_sum,
        prev_sum,
    }
}

/// `get_per_cpu_interval()` 相当 (03 §1.4.2)。
///
/// 戻り値は「補正済みの前サンプル」と tick 合計 (jiffies)。
/// 前サンプルの `iowait` / `idle` は CPU 復帰・トラッキング誤差の補正で
/// 書き換わるため、CPU "all" の合算には**補正後の値**を使う必要がある
/// (03 §1.10-6)。
///
/// オフライン / tickless の判定まで含めて欲しい場合は [`cpu_interval`] を使う。
pub fn per_cpu_interval(
    plan: &DecodePlan,
    prev: &ItemSnapshot,
    curr: &ItemSnapshot,
) -> (ItemSnapshot, u64) {
    let fix = cpu_fix(plan, prev, curr);
    (apply_cpu_fix(plan, prev, &fix), fix.interval)
}

/// 補正値を前サンプルの clone に書き戻す。
fn apply_cpu_fix(plan: &DecodePlan, prev: &ItemSnapshot, fix: &CpuFix) -> ItemSnapshot {
    let mut fixed = prev.clone();
    if let Some(v) = fix.iowait {
        set_column(plan, &mut fixed, cpu_col::IOWAIT, v);
    }
    if let Some(v) = fix.idle {
        set_column(plan, &mut fixed, cpu_col::IDLE, v);
    }
    fixed
}

/// この item が CPU "all" (集約行) か個別 CPU かを表す。
///
/// 分母と tickless の扱いが変わる (03 §1.4.5)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuRole {
    /// CPU "all" (item 添字 0)。
    ///
    /// 「CPU all が tickless になることはない」という前提で、
    /// tick 合計が 0 のときは **1 に差し替える**。
    Aggregate,
    /// 個別 CPU。tick 合計 0 は tickless CPU を意味する。
    Single,
}

/// 1 CPU 分の区間の状態 (03 §1.4.3 / §1.4.5)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuState {
    /// 通常。[`CpuInterval::tick_total`] を分母にして各列を計算する。
    Online,
    /// オフライン。`/proc/stat` から当該 CPU 行が消えている
    /// (現サンプルの tick 8 フィールドの和が 0)。
    ///
    /// **行そのものを出力しない。** tickless と混同すると
    /// 「ずっと 100% idle の CPU」が並ぶ嘘の出力になる。
    Offline,
    /// tickless。オンラインだが tick が発生していない (`CONFIG_NO_HZ_FULL`)。
    ///
    /// 計算せずに固定値を出す ([`CpuInterval::tickless_value`])。
    Tickless,
}

/// 1 CPU 分の区間前処理の結果 (指摘 1)。
///
/// sar / sadf / 独自出力 / 集計のどの経路も、`A_CPU` の値を出す前に
/// これを通すことで同じ分母・同じ判定になる。
#[derive(Debug, Clone)]
pub struct CpuInterval {
    /// 補正済みの前サンプル。`iowait` / `idle` が書き換わっている場合がある。
    ///
    /// **列の計算にはこちらを渡す** (元の前サンプルではない)。
    pub prev: ItemSnapshot,
    /// 割合の分母 (`deltot_jiffies`)。[`CpuRole::Aggregate`] では 1 以上。
    pub tick_total: u64,
    pub state: CpuState,
    /// 前サンプルの時点で稼働していたか (`tot_jiffies_p != 0`)。
    ///
    /// 偽なら「復帰直後で基準値が無い」。本家は
    /// `!WANT_SINCE_BOOT` のときこの CPU をオフライン扱いにして
    /// CPU "all" にも加算しない (03 §1.4.3)。
    /// 「起動時からの統計」モードでは前値が全ゼロなのが正常なので、
    /// 呼び出し側がそのモードと組み合わせて判断する。
    pub prev_online: bool,
    role: CpuRole,
}

impl CpuInterval {
    /// 行を出力してはいけない CPU か。
    #[inline]
    pub fn is_offline(&self) -> bool {
        self.state == CpuState::Offline
    }

    /// 固定値を出す CPU か。
    #[inline]
    pub fn is_tickless(&self) -> bool {
        self.state == CpuState::Tickless
    }

    #[inline]
    pub fn role(&self) -> CpuRole {
        self.role
    }

    /// この CPU の計算文脈。
    ///
    /// `base` の `itv_cs` / `continuous` / `has_prev` はそのまま引き継ぎ、
    /// 分母 (`tick_total`) と集約フラグだけを上書きする。
    ///
    /// **連続性を勝手に真にしない。** RESTART 直後や item が入れ替わった区間で
    /// `continuous` を偽装すると、全ゼロの前値から 100% が出る。
    /// 呼び出し側はその区間の連続性を持っているので、それを渡してもらう。
    pub fn context(&self, base: ComputeContext) -> ComputeContext {
        ComputeContext {
            tick_total: Some(self.tick_total),
            aggregate_item: self.role == CpuRole::Aggregate,
            ..base
        }
    }

    /// tickless CPU の固定値 (03 §1.4.5)。
    ///
    /// `%idle` は `100.00`、他の割合列は `0.00`。
    /// tickless でない場合は `None` を返すので、
    /// `interval.tickless_value(col).unwrap_or_else(|| 計算)` と書ける。
    ///
    /// `%guest` / `%gnice` / `%irq` / `%soft` も 0 になる
    /// (本家は 5 個ずつ 2 回に分けて出力しており、2 回目の最後が `100.00`)。
    pub fn tickless_value(&self, column: usize) -> Option<f64> {
        if !self.is_tickless() {
            return None;
        }
        Some(if column == cpu_col::IDLE { 100.0 } else { 0.0 })
    }
}

/// 1 CPU 分の区間前処理 (指摘 1)。
///
/// `get_per_cpu_interval()` (03 §1.4.2) に、オフライン判定 (§1.4.3) と
/// tickless 判定 (§1.4.5) を合わせた入口。
///
/// - 現サンプルの tick 8 フィールドの和が 0 → [`CpuState::Offline`]
///   (行を出さない)
/// - tick 合計差分が 0 で個別 CPU → [`CpuState::Tickless`] (固定値)
/// - [`CpuRole::Aggregate`] では tick 合計を 1 以上に補正する
pub fn cpu_interval(
    plan: &DecodePlan,
    prev: &ItemSnapshot,
    curr: &ItemSnapshot,
    role: CpuRole,
) -> CpuInterval {
    let fix = cpu_fix(plan, prev, curr);
    let state = if fix.curr_sum == 0 {
        // `/proc/stat` から行が消えている = オフライン。
        // tickless (差分 0) とは**現サンプルの絶対値**で区別する。
        CpuState::Offline
    } else if fix.interval == 0 && role == CpuRole::Single {
        CpuState::Tickless
    } else {
        CpuState::Online
    };
    let tick_total = match role {
        // 「CPU all が tickless になることはない」前提で 0 を 1 に差し替える
        CpuRole::Aggregate => fix.interval.max(1),
        CpuRole::Single => fix.interval,
    };
    CpuInterval {
        prev: apply_cpu_fix(plan, prev, &fix),
        tick_total,
        state,
        prev_online: fix.prev_sum != 0,
        role,
    }
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
        .map(|c| cpu_field(plan, item, *c))
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

impl CpuAggregate {
    /// CPU "all" 行の計算文脈 (指摘 1)。
    ///
    /// 分母は「オンラインだった CPU の tick 合計」なので、
    /// `%user + … + %idle` は常に 100% になる (オフライン時間は分母に入らない)。
    /// `CPU all が tickless になることはない`前提で 1 以上に補正する (03 §1.4.5)。
    pub fn context(&self, base: ComputeContext) -> ComputeContext {
        ComputeContext {
            tick_total: Some(self.tick_total.max(1)),
            aggregate_item: true,
            ..base
        }
    }

    /// その item 添字 (1 起点) がオフラインとしてマークされたか。
    #[inline]
    pub fn is_offline(&self, item_index: usize) -> bool {
        self.offline.contains(&item_index)
    }
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
///
/// **CPU ごとに補正してから合算する (指摘 1 / 03 §1.10-6)。**
/// `get_per_cpu_interval()` が前サンプルを書き換えるため、
/// 補正前の値を足し込むと CPU "all" の割合がずれる。
pub fn aggregate_cpu(
    plan: &DecodePlan,
    prev_items: &[ItemSnapshot],
    curr_items: &[ItemSnapshot],
    since_boot: bool,
) -> Option<CpuAggregate> {
    if curr_items.len() <= 1 {
        return None;
    }
    let zero = || aggregate_zero(plan);
    let mut agg_prev = zero();
    let mut agg_curr = zero();
    let mut total: u64 = 0;
    let mut offline = Vec::new();

    let n = prev_items.len().min(curr_items.len());
    offline.extend(n.max(1)..curr_items.len());
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

        // CPU ごとに補正 → その CPU の tick 合計を足す → 補正後の前値を足し込む
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

/// 同一名のデバイスが再登録されたか。本家 check_*_reg() の -2 判定。
pub fn item_reregistered(
    id: ActivityId,
    plan: &DecodePlan,
    prev: &ItemSnapshot,
    curr: &ItemSnapshot,
) -> bool {
    let p = |c| raw_column(plan, prev, c).unwrap_or(0);
    let c = |n| raw_column(plan, curr, n).unwrap_or(0);
    match id {
        ActivityId::NET_DEV => {
            let decreased = (net_dev_col::RXPCK..=net_dev_col::RXMCST).any(|n| c(n) < p(n));
            let overflow = [
                (net_dev_col::RXKB, net_dev_col::RXPCK),
                (net_dev_col::TXKB, net_dev_col::TXPCK),
                (net_dev_col::RXPCK, net_dev_col::RXKB),
                (net_dev_col::TXPCK, net_dev_col::TXKB),
            ]
            .iter()
            .any(|&(n, other)| c(n) < p(n) && c(other) > p(other) && p(n) > u64::MAX / 2);
            decreased && !overflow
        }
        ActivityId::NET_EDEV => (2..=9).any(|n| c(n) < p(n)),
        ActivityId::DISK => {
            c(disk_col::TPS) < p(disk_col::TPS)
                && [disk_col::RKB, disk_col::WKB, disk_col::DKB]
                    .iter()
                    .all(|&n| p(n) == 0 || c(n) < p(n))
        }
        _ => false,
    }
}

/// 本家互換の新規 item の差分基準。
pub fn zero_item(plan: &DecodePlan) -> ItemSnapshot {
    ItemSnapshot {
        key: None,
        texts: Vec::new(),
        values: vec![Availability::Present(0); plan.fields.len()],
    }
}

fn aggregate_zero(plan: &DecodePlan) -> ItemSnapshot {
    ItemSnapshot {
        values: plan
            .fields
            .iter()
            .map(|field| {
                if field.read.is_some() {
                    Availability::Present(0)
                } else {
                    Availability::UnsupportedBySource
                }
            })
            .collect(),
        ..ItemSnapshot::default()
    }
}

/// softnet の CPU hotplug 補正付き合算。
pub fn aggregate_soft(
    plan: &DecodePlan,
    prev: &[ItemSnapshot],
    curr: &[ItemSnapshot],
) -> Option<CpuAggregate> {
    if curr.len() <= 1 {
        return None;
    }
    let mut out = CpuAggregate {
        prev: aggregate_zero(plan),
        curr: aggregate_zero(plan),
        tick_total: 0,
        offline: Vec::new(),
    };
    for (i, c) in curr.iter().enumerate().skip(1) {
        let Some(p) = prev.get(i).filter(|p| !soft_offline(plan, p)) else {
            out.offline.push(i);
            continue;
        };
        let c = if soft_offline(plan, c) {
            out.offline.push(i);
            p
        } else {
            c
        };
        add_into(&mut out.prev, p);
        add_into(&mut out.curr, c);
    }
    Some(out)
}

fn soft_offline(plan: &DecodePlan, item: &ItemSnapshot) -> bool {
    (soft_col::TOTAL..=soft_col::BLG_LEN).all(|c| raw_column(plan, item, c).unwrap_or(0) == 0)
}

// ============================================================================
// オフライン CPU の前値持ち越し (本家のバッファ書き換え)
// ============================================================================

/// 本家が表示のたびに行う「オフライン CPU の現値を前値で埋める」書き換えを再現する。
///
/// 本家の `get_global_cpu_statistics()` (`*scc = *scp`)・
/// `get_global_soft_statistics()` (`*ssnc = *ssnp`)・
/// `get_global_int_statistics()` (`memcpy`) は、現サンプルでオフラインの CPU について
/// **現サンプルのバッファ (`buf[curr]`) そのものを前サンプルの値で上書きする**。
/// 書き換えたバッファは次のレコードの前サンプル (`buf[!curr]`) になるので、
/// CPU が 1 区間だけオフラインになって戻ってきたとき、本家は
/// 「オフラインになる直前の値」との差分で復帰後の行を出し、CPU "all" にも加える。
/// 生の前レコード (オフライン = 全ゼロ) を前サンプルにすると
/// 「前サンプルでもオフライン = 基準値が無い」と判定されて行が消え、
/// CPU "all" の合算からも落ちる (03 §1.4.3)。平均行も同じで、本家の
/// `Average:` は書き換え後の最終サンプルと最初のサンプル (`buf[2]`) の差から出る。
///
/// 戻り値は**書き換え後の現サンプル** (= 次のレコードの前サンプル)。
/// 書き換えが無ければ `Cow::Borrowed(curr)` を返すので、呼び出し側は
/// 変化があったときだけ複製を持てばよい。**区間値の計算には使わない。**
/// その区間のオフライン判定は生の現サンプルで行う必要があり
/// ([`aggregate_cpu`] / [`aggregate_soft`] / [`prepare_item`] が
/// 区間内の埋め合わせを自分で行う)、これは「次の区間の前サンプル」を作る関数である。
///
/// - `prev`: 前サンプル。**これ自体が前回この関数で書き換えた後の値**
///   (本家の `buf[!curr]`) でなければならない。区間の最初は基準レコードの生の値。
/// - `curr`: 生の現サンプル。
/// - `cpu_selected`: item 添字 (0 = CPU "all"、`n` = CPU `n-1`) が `-P` で
///   選択されているか。`A_IRQ` だけが使う (本家は未選択の CPU を書き換えない)。
///
/// | activity | 対象の添字 | 書き換える条件 | 書き換える範囲 |
/// |---|---|---|---|
/// | `A_CPU` | 1 以上 (個別 CPU) | 現サンプルの tick 8 フィールドの和が 0 | その CPU の item |
/// | `A_NET_SOFT` | 1 以上 | 前サンプルの 6 カウンタの和が非 0、かつ現サンプルの和が 0 | その CPU の item |
/// | `A_IRQ` | 0 以上 (CPU "all" を含む) | 選択されている、前サンプルのその CPU の総数 (割り込み 0 の値) が非 0、かつ現サンプルの総数が 0 | その CPU の割り込み `nr2` 個すべて |
///
/// item が 1 つしか無い `A_CPU` / `A_NET_SOFT` (UP 機) は対象が無い。
/// 割り込み名を持たない旧世代の `A_IRQ` (`nr2 == 1`) は 1 item = 1 割り込みで
/// CPU の次元が無いので書き換えない。それ以外の activity はそのまま返す。
pub fn carry_offline<'a>(
    id: ActivityId,
    plan: &DecodePlan,
    prev: &[ItemSnapshot],
    curr: &'a [ItemSnapshot],
    cpu_selected: impl Fn(usize) -> bool,
) -> Cow<'a, [ItemSnapshot]> {
    let mut out: Option<Vec<ItemSnapshot>> = None;
    match id {
        ActivityId::CPU => {
            for i in 1..curr.len() {
                if cpu_tick_sum(plan, &curr[i]) != 0 {
                    continue;
                }
                if let Some(p) = prev.get(i) {
                    out.get_or_insert_with(|| curr.to_vec())[i] = p.clone();
                }
            }
        }
        ActivityId::NET_SOFT => {
            for i in 1..curr.len() {
                let Some(p) = prev.get(i) else { continue };
                // 前サンプルでもオフラインなら本家は書き換える前に `continue` する
                if soft_offline(plan, p) || !soft_offline(plan, &curr[i]) {
                    continue;
                }
                out.get_or_insert_with(|| curr.to_vec())[i] = p.clone();
            }
        }
        ActivityId::IRQ if plan.nr2 > 1 || plan.text_index("irq_name").is_some() => {
            let nr2 = plan.nr2.max(1) as usize;
            let total = |items: &[ItemSnapshot], start: usize| {
                items
                    .get(start)
                    .map_or(0, |it| raw_column(plan, it, irq_col::COUNT).unwrap_or(0))
            };
            for cpu in 0..curr.len() / nr2 {
                let start = cpu * nr2;
                if !cpu_selected(cpu) || total(prev, start) == 0 || total(curr, start) != 0 {
                    continue;
                }
                let Some(src) = prev.get(start..start + nr2) else {
                    continue;
                };
                out.get_or_insert_with(|| curr.to_vec())[start..start + nr2].clone_from_slice(src);
            }
        }
        _ => {}
    }
    match out {
        Some(v) => Cow::Owned(v),
        None => Cow::Borrowed(curr),
    }
}

/// 本家が CPU 系 activity の各レコードに行う「`nr_ini` 個までのゼロ埋め」を再現する。
///
/// `A_CPU` / `A_IRQ` / `A_NET_SOFT` は本家で `AO_PERSISTENT` の activity で、
/// `read_file_stat_bunch()` はレコードを読む前にバッファを **`nr_ini` 個
/// (× `nr2`) ぶんゼロで埋める**。レコードごとの item 数 (`has_nr`) が
/// `nr_ini` より少ないレコードでは、載っていない CPU は全ゼロ = オフラインとして
/// 扱われ、[`carry_offline`] の前値持ち越しの対象になる。載っていない CPU を
/// 「存在しない」と扱うと、その CPU は前値を持ち越せず、次に現れたときに
/// 基準値が無いとして行が消える (本家 `data.tmp` の `-P ALL` で CPU8 が出る区間)。
///
/// `nr_ini` は CPU の行数 (CPU "all" を含む)。本家はファイルの `file_activity.nr` で
/// 始め、それを超えるレコードを読むと広げ、`LINUX RESTART` で CPU 数に戻す。
/// 呼び出し側は「ファイルの `nr`・前サンプルの行数・現サンプルの行数」の最大を
/// 渡せば同じ結果になる (余分なゼロ行はオフラインとして表示されない)。
///
/// 対象外の activity と、割り込み名を持たない旧世代の `A_IRQ` (行が CPU ではない) は
/// そのまま返す。
pub fn pad_persistent<'a>(
    id: ActivityId,
    plan: &DecodePlan,
    items: &'a [ItemSnapshot],
    nr_ini: usize,
) -> Cow<'a, [ItemSnapshot]> {
    let per_row = match id {
        ActivityId::CPU | ActivityId::NET_SOFT => 1,
        ActivityId::IRQ if plan.nr2 > 1 || plan.text_index("irq_name").is_some() => {
            plan.nr2.max(1) as usize
        }
        _ => return Cow::Borrowed(items),
    };
    let want = nr_ini.saturating_mul(per_row);
    if items.len() >= want {
        return Cow::Borrowed(items);
    }
    let mut out = items.to_vec();
    out.resize(want, zero_item(plan));
    Cow::Owned(out)
}

/// tickless CPU について本家 `save_cpu_xstats()` が記録する極値 (`sar -x`)。
///
/// 本家の tickless 専用分岐の条件は `!cpu && !deltot_jiffies` で、`cpu == 0`
/// (CPU "all") は分母を 1 に差し替えた後に呼ばれるので実際には通らない。
/// 個別 CPU の tickless (`deltot_jiffies == 0`) は**通常の式に分母 0 を与えて**
/// 記録される。表示行は固定値 (`0.00` × n と `%idle = 100.00`、03 §1.4.5) だが、
/// 極値として記録される値は次のとおり別物になる。
///
/// - 差分 0 のフィールド → `0.0 / 0` = NaN。比較 (`<` / `>`) が偽なので最小・最大を更新しない
/// - 本家の式が 0 にクランプするフィールド (逆行) → `0.0`
/// - 差分が正のフィールド (tick 合計に入らない `guest` / `guest_nice`) → `+inf`
///
/// 列は表示と同じ `cpu_col` の添字で指定する。`prev` は [`per_cpu_interval`] で
/// 補正済みの前サンプルを渡すこと (本家も `get_per_cpu_interval()` が
/// 書き換えた `scp` で記録する)。CPU の列でなければ NaN (= 記録しない) を返す。
pub fn cpu_tickless_extremum(
    plan: &DecodePlan,
    column: usize,
    prev: &ItemSnapshot,
    curr: &ItemSnapshot,
) -> f64 {
    let p = |c: usize| cpu_field(plan, prev, c);
    let c = |c: usize| cpu_field(plan, curr, c);
    // `ll_sp_value(v1, v2, 0)`: 逆行は 0、それ以外は `(double) (v2 - v1) / 0 * 100`
    let zero_jiffies = 0.0_f64;
    let llsp = |v1: u64, v2: u64| -> f64 {
        if v2 < v1 {
            0.0
        } else {
            v2.wrapping_sub(v1) as f64 / zero_jiffies * 100.0
        }
    };
    let sum3 = |f: &dyn Fn(usize) -> u64| {
        f(cpu_col::SYS)
            .wrapping_add(f(cpu_col::IRQ))
            .wrapping_add(f(cpu_col::SOFT))
    };
    match column {
        cpu_col::USER => llsp(p(cpu_col::USER), c(cpu_col::USER)),
        cpu_col::NICE => llsp(p(cpu_col::NICE), c(cpu_col::NICE)),
        cpu_col::SYSTEM => llsp(sum3(&p), sum3(&c)),
        cpu_col::USR | cpu_col::NICE_EXCL_GNICE => {
            let (base, guest) = if column == cpu_col::USR {
                (cpu_col::USER, cpu_col::GUEST)
            } else {
                (cpu_col::NICE, cpu_col::GNICE)
            };
            let pv = p(base).wrapping_sub(p(guest));
            let cv = c(base).wrapping_sub(c(guest));
            if cv < pv { 0.0 } else { llsp(pv, cv) }
        }
        cpu_col::IOWAIT
        | cpu_col::STEAL
        | cpu_col::IDLE
        | cpu_col::SYS
        | cpu_col::IRQ
        | cpu_col::SOFT
        | cpu_col::GUEST
        | cpu_col::GNICE => llsp(p(column), c(column)),
        _ => f64::NAN,
    }
}

/// 全出力・集計で共有する、1 item の補正済み端点と状態。
#[derive(Debug, Clone)]
pub struct PreparedItem {
    pub prev: ItemSnapshot,
    pub curr: ItemSnapshot,
    pub ctx: ComputeContext,
    pub offline: bool,
    pub tickless: bool,
    pub replaced: bool,
}

impl PreparedItem {
    /// 補正済み端点から値を求める。厳密モードでは再登録・欠測を値にしない。
    pub fn computed(
        &self,
        id: ActivityId,
        column: usize,
        meta: &ColumnMeta,
        plan: &DecodePlan,
        policy: MissingPolicy,
    ) -> Computed {
        if policy == MissingPolicy::Strict {
            if self.replaced {
                return Err(ComputeIssue::Discontinuous(Discontinuity::ItemReplaced));
            }
            if self.offline {
                return Err(ComputeIssue::MissingInSample);
            }
        }
        if self.ctx.has_prev && self.ctx.continuous && self.tickless {
            // まず入力の可用性を調べ、未提供の列を tickless の 0 に偽装しない。
            let ctx = self.ctx.with_tick_total(1);
            column_value_with(id, column, meta, plan, &self.prev, &self.curr, &ctx, policy)?;
            return Ok(if column == cpu_col::IDLE { 100.0 } else { 0.0 });
        }
        column_value_with(
            id, column, meta, plan, &self.prev, &self.curr, &self.ctx, policy,
        )
    }
}

/// CPU の分母・端点、softnet hotplug、デバイス再登録を共通処理する。
pub fn prepare_item(
    id: ActivityId,
    plan: &DecodePlan,
    index: usize,
    prev_items: &[ItemSnapshot],
    curr_items: &[ItemSnapshot],
    mut ctx: ComputeContext,
) -> Option<PreparedItem> {
    let curr = curr_items.get(index)?;
    let prev = match curr.key.as_deref() {
        Some(key) if id != ActivityId::IRQ => {
            prev_items.iter().find(|p| p.key.as_deref() == Some(key))
        }
        _ => prev_items.get(index),
    };
    let replaced = ctx.has_prev
        && (prev.is_none() || prev.is_some_and(|p| item_reregistered(id, plan, p, curr)));
    let mut out = PreparedItem {
        prev: prev.cloned().unwrap_or_else(|| zero_item(plan)),
        curr: curr.clone(),
        ctx,
        offline: false,
        tickless: false,
        replaced,
    };
    if replaced {
        out.prev = zero_item(plan);
    }
    match id {
        ActivityId::CPU => {
            if index == 0
                && let Some(agg) = aggregate_cpu(plan, prev_items, curr_items, false)
            {
                out.ctx = agg.context(ctx);
                out.prev = agg.prev;
                out.curr = agg.curr;
            } else {
                let interval = cpu_interval(
                    plan,
                    &out.prev,
                    curr,
                    if index == 0 {
                        CpuRole::Aggregate
                    } else {
                        CpuRole::Single
                    },
                );
                out.ctx = interval.context(ctx);
                out.offline = interval.is_offline() || (index > 0 && !interval.prev_online);
                out.tickless = interval.is_tickless();
                out.prev = interval.prev;
            }
        }
        ActivityId::NET_SOFT => {
            if index == 0
                && let Some(agg) = aggregate_soft(plan, prev_items, curr_items)
            {
                out.prev = agg.prev;
                out.curr = agg.curr;
            } else if index > 0 {
                out.offline = soft_offline(plan, &out.prev) || soft_offline(plan, curr);
                if soft_offline(plan, curr) {
                    out.curr = out.prev.clone();
                }
            }
        }
        ActivityId::IRQ if plan.nr2 > 1 || plan.text_index("irq_name").is_some() => {
            let start = index / plan.nr2.max(1) as usize * plan.nr2.max(1) as usize;
            let p = prev_items.get(start);
            let c = curr_items.get(start);
            out.offline = p.is_none_or(|p| raw_column(plan, p, irq_col::COUNT).unwrap_or(0) == 0)
                || c.is_none_or(|c| raw_column(plan, c, irq_col::COUNT).unwrap_or(0) == 0);
            if c.is_some_and(|c| raw_column(plan, c, irq_col::COUNT).unwrap_or(0) == 0) {
                out.curr = out.prev.clone();
            }
            ctx.aggregate_item = start == 0;
            out.ctx = ctx;
        }
        _ => {}
    }
    Some(out)
}

/// item 群をフィールド単位で合算する。
///
/// 差分の相手を必要としない初期集約に用いる。
/// CPU / softnet の区間値では hotplug 補正付きの専用入口を使う。
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
        *slot = match (*slot, *v) {
            (Availability::Present(a), Availability::Present(b)) => {
                Availability::Present(a.wrapping_add(b))
            }
            (Availability::MissingInSample, _) | (_, Availability::MissingInSample) => {
                Availability::MissingInSample
            }
            _ => Availability::UnsupportedBySource,
        };
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
    let n = prev_slots.len().min(curr_slots.len());
    weighted_mhz_core(plan, n, |k| &prev_slots[k], |k| &curr_slots[k])
}

/// [`weighted_mhz`] の**参照スライス**版。
///
/// 行が連続していない行列 (`A_IRQ` のように stride で走査するもの) でも
/// clone を作らずに渡せる。
fn weighted_mhz_refs(
    plan: &DecodePlan,
    prev_slots: &[&ItemSnapshot],
    curr_slots: &[&ItemSnapshot],
) -> Computed {
    let n = prev_slots.len().min(curr_slots.len());
    weighted_mhz_core(plan, n, |k| prev_slots[k], |k| curr_slots[k])
}

/// `wghMHz` の本体。スロットの取り出し方だけを呼び出し側から受け取る。
fn weighted_mhz_core<'a>(
    plan: &DecodePlan,
    slots: usize,
    prev_at: impl Fn(usize) -> &'a ItemSnapshot,
    curr_at: impl Fn(usize) -> &'a ItemSnapshot,
) -> Computed {
    let mut tisfreq: u64 = 0;
    let mut tis: u64 = 0;
    for k in 0..slots {
        let curr = curr_at(k);
        let freq = raw_column(plan, curr, freq_col::FREQ_KHZ)?;
        // 未使用スロットで打ち切り
        if freq == 0 {
            break;
        }
        let c = raw_column(plan, curr, freq_col::TIME_IN_STATE)?;
        let p = raw_column(plan, prev_at(k), freq_col::TIME_IN_STATE)?;
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
// 行列型 activity の 1 行分の計算 (指摘 3)
//
// `A_PWR_FREQ` (id=35) と `A_IRQ` (id=3) は「行 × 列」の行列で、
// 1 item だけでは値を出せない列を持つ。以前は sar 互換出力だけが
// [`weighted_mhz`] を呼び、sadf・独自出力は単一 item 計算に落ちて
// [`ComputeIssue::NeedsItemGroup`] になり `wghMHz` が常に欠損していた。
//
// [`matrix_row_values`] が「1 行分のスロット群」を受け取る共通入口になる。
// ============================================================================

/// 行列型 activity の 1 行分の値 (指摘 3)。
///
/// `prev_slots` / `curr_slots` はその論理行に属する item の**参照**を
/// 出力に現れる順に並べたもの。参照で受けるのは、行の並びが activity で違うため。
///
/// - `A_PWR_FREQ`: 保存形も論理行も「CPU ごとに連続する `nr2` スロット」
///   (`行 * nr2 + 列` の連続スライス)
/// - `A_IRQ`: 保存形は 行 = CPU / 列 = 割り込みだが、出力の 1 行は
///   「1 割り込み × 全 CPU」なので **stride 走査**した非連続の並びになる
///
/// どちらも参照を並べるだけで渡せるので、clone は要らない。
///
/// 戻り値の長さは activity によって変わる。
///
/// | activity | 列 | 戻り値 |
/// |---|---|---|
/// | `A_PWR_FREQ` | `wghMHz` | 全スロットの重み付き平均 = **1 要素** (03 §id=35) |
/// | `A_IRQ` | `intr` | CPU スロットごとのレート = **スロット数ぶん** (03 §id=3) |
/// | その他 | — | スロットごとに [`column_value`] を評価 |
///
/// `A_IRQ` の先頭スロットは「全 CPU 合計」列で、
/// 「割り込み総数が減ったら 0」というクランプが効く。
/// そのため `ctx.aggregate_item` はスロットごとにこの関数が設定し直す
/// (呼び出し側で詰める必要はない)。
///
/// `ctx` の `itv_cs` / `continuous` / `has_prev` はそのまま使う。
pub fn matrix_row_values(
    id: ActivityId,
    column: usize,
    plan: &DecodePlan,
    prev_slots: &[&ItemSnapshot],
    curr_slots: &[&ItemSnapshot],
    ctx: &ComputeContext,
) -> Vec<Computed> {
    matrix_row_values_with(
        id,
        column,
        plan,
        prev_slots,
        curr_slots,
        ctx,
        MissingPolicy::Compat,
    )
}

/// [`matrix_row_values`] の欠落を埋めない版 (独自出力・集計用)。
pub fn matrix_row_values_strict(
    id: ActivityId,
    column: usize,
    plan: &DecodePlan,
    prev_slots: &[&ItemSnapshot],
    curr_slots: &[&ItemSnapshot],
    ctx: &ComputeContext,
) -> Vec<Computed> {
    matrix_row_values_with(
        id,
        column,
        plan,
        prev_slots,
        curr_slots,
        ctx,
        MissingPolicy::Strict,
    )
}

#[allow(clippy::too_many_arguments)]
fn matrix_row_values_with(
    id: ActivityId,
    column: usize,
    plan: &DecodePlan,
    prev_slots: &[&ItemSnapshot],
    curr_slots: &[&ItemSnapshot],
    ctx: &ComputeContext,
    policy: MissingPolicy,
) -> Vec<Computed> {
    // `wghMHz` は行全体で 1 値。スロットごとには意味を持たない
    if id == ActivityId::PWR_FREQ && column == freq_col::WGH_MHZ {
        return vec![weighted_mhz_refs(plan, prev_slots, curr_slots)];
    }

    let Some(def) = crate::layout::registry::lookup(id) else {
        return vec![Err(ComputeIssue::NotImplemented)];
    };
    let Some(meta) = def.columns.get(column) else {
        return vec![Err(ComputeIssue::UnsupportedBySource)];
    };

    let empty = ItemSnapshot::default();
    curr_slots
        .iter()
        .enumerate()
        .map(|(n, curr)| {
            let matched = prev_slots.get(n).copied();
            let mut slot_ctx = *ctx;
            // 前サンプルに対応スロットが無い = 差分が取れない
            if matched.is_none() {
                slot_ctx.has_prev = false;
            }
            // `A_IRQ` の先頭スロットは「全 CPU 合計」列で逆行クランプが効く
            slot_ctx.aggregate_item = n == 0;
            let prev = matched.unwrap_or(&empty);
            column_value_with(id, column, meta, plan, prev, curr, &slot_ctx, policy)
        })
        .collect()
}

// ============================================================================
// 期間集計の入口 (指摘 2)
//
// 期間集計は「区間ごとの表示値の平均」ではなく
// 「差分の総和 ÷ 分母の総和」でレートを出す (03 §1.10-8)。
// その素材 (生の差分と分母) をここで作り、表示単位への換算は
// [`rate_from_totals`] に閉じ込める。
//
// **集計側でスケーリングを再実装しない。** 以前は集計が
// `delta_total / denom_total * 100` をそのまま返していたため、
// 平均 `rkB/s` が 2 倍、`aqu-sz` が 1,000 倍、`%util` が 10 倍、
// PSI が 10,000 倍になっていた。
// ============================================================================

/// 1 区間ぶんのレート素材 (指摘 2)。
///
/// 表示単位に直すには [`RateSample::display`] を使うか、
/// 複数区間ぶんを足してから [`rate_from_totals`] に渡す。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateSample {
    /// 生の差分 (スケーリング前)。
    pub delta: u64,
    /// 分母。`A_CPU` は tick 合計、それ以外は区間長 (1/100 秒)。
    pub denominator: u64,
    /// カウンタのラップを復元して得た差分か。
    pub wrapped: bool,
}

impl RateSample {
    /// この 1 区間の表示単位のレート。
    ///
    /// [`column_value`] が返す区間値と一致する (スケーリング込み)。
    pub fn display(&self, id: ActivityId, column: usize) -> Option<f64> {
        rate_from_totals(
            id,
            column,
            u128::from(self.delta),
            u128::from(self.denominator),
        )
    }
}

/// 期間集計のための「生の差分と分母」を求める (指摘 2)。
///
/// [`column_value`] との違い:
///
/// - 本家の符号なし減算をそのまま再現せず、「一周として説明できる減少」だけを
///   差分にする ([`compute_delta`])。説明できない減少に値を与えると、
///   巨大な外れ値が平均や p95 を壊す。
/// - 表示単位へのスケーリングを**掛けない**。掛けるのは
///   [`rate_from_totals`] / [`RateSample::display`] の役目。
///
/// 分母の決め方は [`column_value`] と同じ。`ctx.tick_total` が `Some` なら
/// それ (= `A_CPU` の `deltot_jiffies`)、`None` なら `ctx.itv_cs`。
/// tick 合計が 0 の CPU は「動いていない」ので 0% と報告せず
/// [`Discontinuity::NonPositiveElapsed`] を返す。
///
/// 直接列 (`ColumnMeta::is_direct()`) のカウンタ列にだけ使える。
/// 派生列は単一の差分を持たないため [`ComputeIssue::NotImplemented`] を返す。
pub fn rate_sample(
    plan: &DecodePlan,
    id: ActivityId,
    column: usize,
    prev: &ItemSnapshot,
    curr: &ItemSnapshot,
    ctx: &ComputeContext,
) -> Result<RateSample, ComputeIssue> {
    let def = crate::layout::registry::lookup(id).ok_or(ComputeIssue::NotImplemented)?;
    let meta = def
        .columns
        .get(column)
        .ok_or(ComputeIssue::NotImplemented)?;
    if !meta.is_direct() || meta.kind != ValueKind::Counter {
        return Err(ComputeIssue::NotImplemented);
    }
    if !ctx.has_prev {
        return Err(ComputeIssue::Discontinuous(Discontinuity::FirstSample));
    }
    if !ctx.continuous {
        return Err(ComputeIssue::Discontinuous(Discontinuity::Restart));
    }

    // 集計は欠落を 0 で埋めない。「フィールドが無い」と「0 だった」は別物
    let curr_v = raw_column(plan, curr, column)?;
    let prev_v = raw_column(plan, prev, column)?;

    let denominator = match ctx.tick_total {
        // tick 合計 0 = その CPU は動いていない。0% と報告しない
        Some(0) => {
            return Err(ComputeIssue::Discontinuous(
                Discontinuity::NonPositiveElapsed,
            ));
        }
        Some(t) => t,
        None if ctx.itv_cs == 0 => {
            return Err(ComputeIssue::Discontinuous(
                Discontinuity::NonPositiveElapsed,
            ));
        }
        None => ctx.itv_cs,
    };

    // 逆行クランプは**サンプル単位**で効く (03 §1.4.4 / §id=3 / §id=6 / §5.1)。
    // 本家が「減っていたら 0」と決めている列は、その区間の寄与を 0 として
    // 次の区間へ進む。区間を跨いで端点差分を取ると値が変わる
    // (Δ = -5, +10 のとき サンプル毎クランプは 10、端点差分は 5)。
    // ここを `column_value` と同じ判定にすることで
    // 「1 区間の集計 == その区間の瞬時値」が逆行区間でも成り立つ。
    if curr_v < prev_v && clamps_decrease(id, column, ctx) {
        return Ok(RateSample {
            delta: 0,
            denominator,
            wrapped: false,
        });
    }

    let bits = counter_bits(plan, column);
    let (delta, wrapped) = match compute_delta(prev_v, curr_v, bits, DeltaContext::default()) {
        Delta::Valid(d) => (d, false),
        Delta::Wrapped(d) => (d, true),
        Delta::Unavailable(disc) => return Err(ComputeIssue::Discontinuous(disc)),
    };

    Ok(RateSample {
        delta,
        denominator,
        wrapped,
    })
}

// ============================================================================
// sadf 専用の別単位列 (指摘 2-3)
//
// `sadf` の JSON / XML / CSV には、`sar` に無い「同じ指標の別単位」の列がある。
// 以前は出力層がそれぞれ自分で換算していたため、
// `rxkB` にバイト/秒をそのまま入れて 1,024 倍、
// `MBfsfree` にバイトをそのまま入れて 1,048,576 倍ずれていた。
//
// 典拠: 03 §9.6-2 (`MBfsfree` / `MBfsused`) / §9.6-10 (`rd_sec` / `avgrq-sz`) /
// §1.8.1 (`rxkB` はバイト/秒なので非 human では 1024 で割る)。
// ============================================================================

/// `sadf` にしか現れない「別単位の列」(指摘 2-3)。
///
/// どれも計算層が持っている列の値を換算したものなので、
/// 元の列と値が食い違うことはない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SadfUnitColumn {
    /// `rd_sec` — `rkB/s` のセクタ表現 (× 2、03 §9.6-10)。
    DiskReadSectors,
    /// `wr_sec` — `wkB/s` のセクタ表現 (× 2)。
    DiskWriteSectors,
    /// `dc_sec` — `dkB/s` のセクタ表現 (× 2)。
    DiskDiscardSectors,
    /// `avgrq-sz` — `areq-sz` のセクタ表現 (× 2)。
    DiskAvgRequestSectors,
    /// `rxkB` — バイト/秒 → kB/s (÷ 1024、03 §1.8.1)。
    NetRxKilobytes,
    /// `txkB` — バイト/秒 → kB/s (÷ 1024)。
    NetTxKilobytes,
    /// `MBfsfree` — バイト → MB (÷ 1024²、03 §9.6-2)。
    FsFreeMegabytes,
    /// `MBfsused` — バイト → MB (÷ 1024²)。
    FsUsedMegabytes,
}

impl SadfUnitColumn {
    /// `sadf` の列名から引く。
    ///
    /// CSV / DB 形式と JSON / XML で名前が違う列 (`rxkB/s` ↔ `rxkB`) は
    /// どちらでも引ける。
    pub fn from_sadf_name(id: ActivityId, name: &str) -> Option<Self> {
        match (id, name) {
            (ActivityId::DISK, "rd_sec" | "rd_sec/s") => Some(Self::DiskReadSectors),
            (ActivityId::DISK, "wr_sec" | "wr_sec/s") => Some(Self::DiskWriteSectors),
            (ActivityId::DISK, "dc_sec" | "dc_sec/s") => Some(Self::DiskDiscardSectors),
            (ActivityId::DISK, "avgrq-sz") => Some(Self::DiskAvgRequestSectors),
            (ActivityId::NET_DEV, "rxkB" | "rxkB/s") => Some(Self::NetRxKilobytes),
            (ActivityId::NET_DEV, "txkB" | "txkB/s") => Some(Self::NetTxKilobytes),
            (ActivityId::FS, "MBfsfree") => Some(Self::FsFreeMegabytes),
            (ActivityId::FS, "MBfsused") => Some(Self::FsUsedMegabytes),
            _ => None,
        }
    }

    /// この列が属する activity。
    pub fn activity(self) -> ActivityId {
        match self {
            Self::DiskReadSectors
            | Self::DiskWriteSectors
            | Self::DiskDiscardSectors
            | Self::DiskAvgRequestSectors => ActivityId::DISK,
            Self::NetRxKilobytes | Self::NetTxKilobytes => ActivityId::NET_DEV,
            Self::FsFreeMegabytes | Self::FsUsedMegabytes => ActivityId::FS,
        }
    }

    /// 換算元になる列の添字 (計算層が値を持っている列)。
    pub fn source_column(self) -> usize {
        match self {
            Self::DiskReadSectors => disk_col::RKB,
            Self::DiskWriteSectors => disk_col::WKB,
            Self::DiskDiscardSectors => disk_col::DKB,
            Self::DiskAvgRequestSectors => disk_col::AREQ_SZ,
            Self::NetRxKilobytes => net_dev_col::RXKB,
            Self::NetTxKilobytes => net_dev_col::TXKB,
            Self::FsFreeMegabytes => fs_col::MB_FREE,
            Self::FsUsedMegabytes => fs_col::MB_USED,
        }
    }

    /// 換算元の表示値を、この列の単位に直す係数。
    pub fn scale(self) -> f64 {
        match self {
            // kB → セクタ (512 B)。`rkB/s` は既にセクタを 2 で割ってあるので戻す
            Self::DiskReadSectors
            | Self::DiskWriteSectors
            | Self::DiskDiscardSectors
            | Self::DiskAvgRequestSectors => 2.0,
            // `A_NET_DEV` の rx/tx はバイト/秒。非 human 出力は 1024 で割って kB/s
            Self::NetRxKilobytes | Self::NetTxKilobytes => 1.0 / 1024.0,
            // `A_FS` の `f_*` はバイト。MB へ
            Self::FsFreeMegabytes | Self::FsUsedMegabytes => 1.0 / (1024.0 * 1024.0),
        }
    }

    /// 換算元の表示値を、この列の単位に直す。
    #[inline]
    pub fn convert(self, source_value: f64) -> f64 {
        source_value * self.scale()
    }
}

/// `sadf` の別単位列の値を計算する (指摘 2-3)。
///
/// 換算元の列を [`column_value`] で計算し、[`SadfUnitColumn::scale`] を掛ける。
/// 元の列と同じ計算を通るので、`rd_sec` と `rkB/s` が食い違うことはない
/// (本家も「同じ式を 2 回評価している」だけである。03 §9.6-10)。
pub fn sadf_unit_value(
    variant: SadfUnitColumn,
    plan: &DecodePlan,
    prev: &ItemSnapshot,
    curr: &ItemSnapshot,
    ctx: &ComputeContext,
) -> Computed {
    sadf_unit_value_with(variant, plan, prev, curr, ctx, MissingPolicy::Compat)
}

fn sadf_unit_value_with(
    variant: SadfUnitColumn,
    plan: &DecodePlan,
    prev: &ItemSnapshot,
    curr: &ItemSnapshot,
    ctx: &ComputeContext,
    policy: MissingPolicy,
) -> Computed {
    let id = variant.activity();
    let column = variant.source_column();
    let def = crate::layout::registry::lookup(id).ok_or(ComputeIssue::NotImplemented)?;
    let meta = def
        .columns
        .get(column)
        .ok_or(ComputeIssue::NotImplemented)?;
    let base = column_value_with(id, column, meta, plan, prev, curr, ctx, policy)?;
    Ok(variant.convert(base))
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

/// 方式 B (累積平均) の `Average:` 値 (本家の `print_avg_*` / `dispavg` 分岐)。
///
/// 本家は瞬時値を表示するたびに**保存値**を `static` 変数へ足し、
/// `Average:` 行で 1 回だけ割る。式の形は列ごとに次の 3 通りで、
/// 表示値を足してから割る [`ItemAccum::mean`] とは丸めが一致しないものがある。
///
/// | 列 | 本家の式 |
/// |---|---|
/// | 固定小数のゲージ ([`GaugeScale::Divide`]: `ldavg-*` / `MHz` / PSI の `-10/-60/-300`) | `(double) Σx / (avg_count × 100)` |
/// | `A_PWR_FAN` の `drpm` | `(Σrpm − Σrpm_min) / avg_count` (`double` の和の差) |
/// | それ以外 (整数ゲージ・`double` のセンサ値) | `(double) Σx / avg_count` |
///
/// 最後の行は表示値の和でよい。整数ゲージの表示値は保存値そのもので
/// (和が 2⁵³ を超えない限り `f64` の和は正確)、`double` のセンサ値は
/// 本家も表示値と同じ `double` を足している。`availablekb` を持たない世代の
/// `kbavail` は代替値 (`frmkb`) が表示値に入るので、保存値の和では出せない。
///
/// `A_PWR_FAN` の `drpm` を使うには `rpm_min` の表示値も [`ItemAccum`] に
/// 足し込んでおくこと (表示されない入力列)。
pub fn average_mean(id: ActivityId, column: usize, acc: &ItemAccum) -> Computed {
    if acc.count == 0 {
        return Err(ComputeIssue::MissingInSample);
    }
    match (id, column) {
        (ActivityId::PWR_FAN, fan_col::DRPM) => {
            let sum = |c: usize| acc.value_sum.get(c).copied().unwrap_or(0.0);
            Ok((sum(fan_col::RPM) - sum(fan_col::RPM_MIN)) / acc.count as f64)
        }
        _ => match gauge_scale(id, column) {
            scale @ GaugeScale::Divide(_) => scale
                .mean(acc.raw_sum.get(column).copied().unwrap_or(0), acc.count)
                .ok_or(ComputeIssue::MissingInSample),
            _ => acc.mean(column),
        },
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
            raw_f64(plan, last, temp_col::MIN, MissingPolicy::Compat)?,
            raw_f64(plan, last, temp_col::MAX, MissingPolicy::Compat)?,
        )),
        (ActivityId::PWR_IN, in_col::PCT) => Ok(range_pct(
            acc.mean(in_col::VOLTS)?,
            raw_f64(plan, last, in_col::MIN, MissingPolicy::Compat)?,
            raw_f64(plan, last, in_col::MAX, MissingPolicy::Compat)?,
        )),
        _ => Err(ComputeIssue::NotImplemented),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::abi::{Endian, LayoutAbi, SourceEncoding};
    use crate::layout::registry::lookup;

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

    /// tick 合計は 8 フィールドだけを足す (`guest` / `guest_nice` を含めない)。
    #[test]
    fn tick_total_sums_only_the_eight_tick_fields() {
        let plan = plan_for(ActivityId::CPU);
        let p = zeros(&plan);
        let mut c = zeros(&plan);
        put(&plan, &mut c, cpu_col::USER, 10);
        put(&plan, &mut c, cpu_col::SYS, 20);
        put(&plan, &mut c, cpu_col::IDLE, 30);
        assert_eq!(tick_total(&plan, &p, &c), 60);
    }

    /// オフライン CPU は全フィールドが 0 のままなので合計も 0 になる。
    #[test]
    fn offline_cpu_has_zero_tick_total() {
        let plan = plan_for(ActivityId::CPU);
        let z = zeros(&plan);
        assert_eq!(tick_total(&plan, &z, &z), 0);
        assert!(cpu_is_offline(&plan, &z));
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

    /// `%util` / `aqu-sz` は本家と同じく**割り算**で縮める。
    ///
    /// `0.1` や `0.001` は二進で正確に表せないので、逆数の掛け算では
    /// 丸め境界の値が本家と 1 桁ずれる。実データで見つかった組
    /// (`Δtot_ticks = 7710`、10 分間隔 = `itv 60000`) を固定する。
    /// 本家 v12.8.0 は `1.28` を出す (`× 0.1` の実装は `1.29` を出していた)。
    #[test]
    fn disk_scaling_divides_like_upstream() {
        let plan = plan_for(ActivityId::DISK);
        let p = zeros(&plan);
        let mut c = zeros(&plan);
        put(&plan, &mut c, disk_col::UTIL_PCT, 7_710);
        let ctx = ComputeContext::new(60_000);
        let util = compute(ActivityId::DISK, disk_col::UTIL_PCT, &plan, &p, &c, &ctx).unwrap();
        assert_eq!(util, 7_710.0 / 60_000.0 * 100.0 / 10.0);
        assert_eq!(format!("{util:.2}"), "1.28");

        // aqu-sz も同じ形 (`S_VALUE(rq_ticks) / 1000.0`)。
        // 175 ms/s は `÷ 1000.0` なら 0.175 (→ 0.17)、`× 0.001` だと
        // 0.17500000000000002 (→ 0.18) になる。
        let mut c = zeros(&plan);
        put(&plan, &mut c, disk_col::AQU_SZ, 175);
        let ctx = ComputeContext::new(100);
        let aqu = compute(ActivityId::DISK, disk_col::AQU_SZ, &plan, &p, &c, &ctx).unwrap();
        assert_eq!(aqu, 175.0 / 100.0 * 100.0 / 1000.0);
        assert_eq!(format!("{aqu:.2}"), "0.17");
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

    /// **指摘 18 の点検**: `speed` フィールドはあるが値が 0 の `%ifutil`。
    ///
    /// フィールドの欠落 (上のテスト) とは別の経路である。速度を取得できない
    /// インターフェース (仮想デバイス、`ethtool` が速度を返さない NIC) では
    /// フィールドが存在して値が 0 になる。ここで `0.0` を返すと
    /// **計算結果の 0 が有効な観測になり**、固定条件の経路が
    /// 「評価済み・検出なし」として数えてしまう (規律 7 の抜け)。
    #[test]
    fn ifutil_with_a_zero_speed_value_is_unavailable_outside_compat() {
        // 現行レイアウト。speed / duplex フィールドは存在する
        let plan = plan_for(ActivityId::NET_DEV);
        let col = net_dev_col::IFUTIL_PCT;
        let meta = &lookup(ActivityId::NET_DEV).unwrap().columns[col];
        let p = zeros(&plan);
        let mut c = zeros(&plan);
        put(&plan, &mut c, net_dev_col::RXKB, 1_000_000);
        // speed = 0 (取得できなかった)。フィールド自体は存在する
        put(&plan, &mut c, net_dev_col::SPEED, 0);
        let ctx = ComputeContext::new(100);

        assert_eq!(
            column_value(ActivityId::NET_DEV, col, meta, &plan, &p, &c, &ctx).unwrap(),
            0.0,
            "互換出力は本家と同じ 0.00 を出す (03 §5.2)"
        );
        assert_eq!(
            column_value_strict(ActivityId::NET_DEV, col, meta, &plan, &p, &c, &ctx),
            Err(ComputeIssue::MissingInSample),
            "分母が無いので率を作れない。0% と見せると評価済みに数えられる"
        );

        // 速度が取れていれば従来どおり計算する (この分岐で他を壊していない)
        put(&plan, &mut c, net_dev_col::SPEED, 1_000);
        let v = column_value_strict(ActivityId::NET_DEV, col, meta, &plan, &p, &c, &ctx)
            .expect("速度があるので計算できる");
        assert!(v > 0.0, "{v}");
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

    // ========================================================================
    // 固定小数ゲージの換算は割り算 (0.01 を掛けない)
    // ========================================================================

    /// 瞬時値は本家どおり `(double) x / 100`。
    ///
    /// `0.01` は二進で正確に表せないので、`35 × 0.01` = 0.35000000000000003 と
    /// `35 / 100` = 0.35 は別の値になり、`--dec=1` で `0.4` / `0.3` に分かれる。
    #[test]
    fn fixed_point_gauges_are_divided_not_multiplied() {
        let plan = plan_for(ActivityId::QUEUE);
        let p = zeros(&plan);
        let mut c = zeros(&plan);
        put(&plan, &mut c, queue_col::LDAVG_1, 35);
        let ctx = ComputeContext::new(100);
        let v = compute(ActivityId::QUEUE, queue_col::LDAVG_1, &plan, &p, &c, &ctx).unwrap();
        assert_eq!(v.to_bits(), 0.35_f64.to_bits());
        assert_eq!(format!("{v:.1}"), "0.3", "本家の --dec=1 は 0.3");

        assert_eq!(GaugeScale::Divide(100).apply(35), 0.35);
        assert_eq!(GaugeScale::Unit.apply(35), 35.0);
        // bMaxPower は unsigned int の 2 倍 (32bit で折り返す)
        assert_eq!(GaugeScale::MultiplyU32(2).apply(250), 500.0);
        assert_eq!(
            GaugeScale::MultiplyU32(2).apply(0x8000_0001),
            2.0,
            "unsigned int の x << 1 は 2^32 で折り返す"
        );
    }

    /// 固定小数ゲージの平均は `Σx / (avg_count × 100)` (合計を 1 回だけ割る)。
    ///
    /// 保存値 1 と 6 の平均は本家 `7 / 200` = 0.035 (二進では 0.035000…03 → `0.04`)。
    /// 表示値 (`x / 100`) を足して 2 で割ると 0.034999999999999996 → `0.03` になる。
    /// 表示値に `0.01` を掛けていた頃は保存値 0 と 35 の平均が 0.17500000000000002
    /// (本家は 0.175 → `0.17`) になり、`ldavg-1` の `Average:` が 1 桁ずれていた。
    #[test]
    fn fixed_point_gauge_average_divides_the_raw_sum_once() {
        let mut acc = ItemAccum::new(6);
        for raw in [1u64, 6] {
            let shown = GaugeScale::Divide(100).apply(raw);
            acc.add(queue_col::LDAVG_1, Some(raw), Some(shown));
            acc.add(queue_col::RUNQ_SZ, Some(raw), Some(raw as f64));
        }
        acc.count = 2;
        let v = average_mean(ActivityId::QUEUE, queue_col::LDAVG_1, &acc).unwrap();
        assert_eq!(v.to_bits(), 0.035_f64.to_bits());
        assert_eq!(format!("{v:.2}"), "0.04");
        assert_eq!(
            format!("{:.2}", acc.mean(queue_col::LDAVG_1).unwrap()),
            "0.03",
            "表示値の平均は本家と丸めが違う (比較用)"
        );
        // 整数ゲージは表示値の平均 (= 保存値の平均) のまま
        assert_eq!(
            average_mean(ActivityId::QUEUE, queue_col::RUNQ_SZ, &acc),
            Ok(3.5)
        );
        // `avg_count` が 0 なら平均は無い
        acc.count = 0;
        assert_eq!(
            average_mean(ActivityId::QUEUE, queue_col::LDAVG_1, &acc),
            Err(ComputeIssue::MissingInSample)
        );
    }

    /// `A_PWR_FAN` の `drpm` の平均は `(Σrpm − Σrpm_min) / avg_count`。
    ///
    /// 本家は `rpm` と `rpm_min` を別々の `double` 配列に足してから引く。
    /// 差を足していくと最下位ビットが食い違う組がある。
    #[test]
    fn fan_drpm_average_subtracts_the_sums() {
        let rpm = [1104.639_f64, 4313.452, 3936.986];
        let min = [202.028_f64, 298.174, 279.796];
        let mut acc = ItemAccum::new(5);
        for (r, m) in rpm.iter().zip(min.iter()) {
            acc.add(fan_col::RPM, None, Some(*r));
            acc.add(fan_col::RPM_MIN, None, Some(*m));
            acc.add(fan_col::DRPM, None, Some(r - m));
        }
        acc.count = 3;
        let upstream = ((rpm[0] + rpm[1] + rpm[2]) - (min[0] + min[1] + min[2])) / 3.0;
        let per_sample = ((rpm[0] - min[0]) + (rpm[1] - min[1]) + (rpm[2] - min[2])) / 3.0;
        assert_ne!(upstream.to_bits(), per_sample.to_bits(), "例の前提");
        let v = average_mean(ActivityId::PWR_FAN, fan_col::DRPM, &acc).unwrap();
        assert_eq!(v.to_bits(), upstream.to_bits());
    }

    // ========================================================================
    // オフライン CPU の前値持ち越し
    // ========================================================================

    fn cpu_item(plan: &DecodePlan, user: u64, idle: u64) -> ItemSnapshot {
        let mut it = zeros(plan);
        put(plan, &mut it, cpu_col::USER, user);
        put(plan, &mut it, cpu_col::IDLE, idle);
        it
    }

    /// `A_CPU`: 現サンプルでオフライン (tick の和が 0) の CPU は前値で埋める。
    #[test]
    fn carry_offline_fills_an_offline_cpu_with_the_previous_values() {
        let plan = plan_for(ActivityId::CPU);
        let prev = vec![
            cpu_item(&plan, 0, 0),
            cpu_item(&plan, 100, 900),
            cpu_item(&plan, 500, 500),
        ];
        // CPU1 がオフライン
        let curr = vec![
            cpu_item(&plan, 0, 0),
            cpu_item(&plan, 200, 1_800),
            cpu_item(&plan, 0, 0),
        ];
        let next = carry_offline(ActivityId::CPU, &plan, &prev, &curr, |_| true);
        assert!(matches!(next, Cow::Owned(_)));
        assert_eq!(
            next[1].values, curr[1].values,
            "オンラインの CPU はそのまま"
        );
        assert_eq!(next[2].values, prev[2].values, "オフラインの CPU は前値");
        // 集約スロット (添字 0) は対象外
        assert_eq!(next[0].values, curr[0].values);

        // 全 CPU がオンラインなら複製しない
        let online = vec![
            cpu_item(&plan, 0, 0),
            cpu_item(&plan, 300, 2_700),
            cpu_item(&plan, 600, 600),
        ];
        assert!(matches!(
            carry_offline(ActivityId::CPU, &plan, &curr, &online, |_| true),
            Cow::Borrowed(_)
        ));
        // 持ち越しの対象でない activity はそのまま
        assert!(matches!(
            carry_offline(ActivityId::DISK, &plan, &prev, &curr, |_| true),
            Cow::Borrowed(_)
        ));
    }

    /// `A_NET_SOFT`: 前サンプルでもオフラインなら書き換えない。
    #[test]
    fn carry_offline_softnet_needs_a_previous_value() {
        let plan = plan_for(ActivityId::NET_SOFT);
        let soft = |total: u64, blg: u64| {
            let mut it = zeros(&plan);
            put(&plan, &mut it, soft_col::TOTAL, total);
            put(&plan, &mut it, soft_col::BLG_LEN, blg);
            it
        };
        let prev = vec![soft(0, 0), soft(100, 7), soft(0, 0)];
        let curr = vec![soft(0, 0), soft(0, 0), soft(0, 0)];
        let next = carry_offline(ActivityId::NET_SOFT, &plan, &prev, &curr, |_| true);
        assert_eq!(next[1].values, prev[1].values, "CPU0 は前値 (blg_len 込み)");
        assert_eq!(
            next[2].values, curr[2].values,
            "前値も 0 の CPU1 は 0 のまま"
        );
    }

    /// `A_IRQ`: CPU ごとに割り込み `nr2` 個をまとめて前値で埋める。未選択の CPU は書き換えない。
    #[test]
    fn carry_offline_irq_copies_the_whole_cpu_and_respects_the_selection() {
        let plan = plan_for(ActivityId::IRQ);
        let irq = |n: u64| {
            let mut it = zeros(&plan);
            put(&plan, &mut it, irq_col::COUNT, n);
            it
        };
        let mut plan2 = plan.clone();
        plan2.nr2 = 2;
        // 並びは CPU 主: [all の割り込み 0, 1] [CPU0 の 0, 1] [CPU1 の 0, 1]
        let prev = vec![irq(30), irq(3), irq(10), irq(1), irq(20), irq(2)];
        let curr = vec![irq(40), irq(4), irq(0), irq(0), irq(25), irq(3)];
        let next = carry_offline(ActivityId::IRQ, &plan2, &prev, &curr, |_| true);
        assert_eq!(next[2].values, prev[2].values);
        assert_eq!(next[3].values, prev[3].values, "総数以外の割り込みも前値");
        assert_eq!(next[4].values, curr[4].values);
        // CPU0 (item 添字 1) を選んでいなければ本家は書き換えない
        let unselected = carry_offline(ActivityId::IRQ, &plan2, &prev, &curr, |i| i != 1);
        assert!(matches!(unselected, Cow::Borrowed(_)));
    }

    /// `A_CPU` などはレコードに載っていない CPU も `nr_ini` まで全ゼロで埋める。
    #[test]
    fn pad_persistent_zero_fills_up_to_nr_ini() {
        let plan = plan_for(ActivityId::CPU);
        let items = vec![cpu_item(&plan, 1, 1), cpu_item(&plan, 2, 2)];
        let padded = pad_persistent(ActivityId::CPU, &plan, &items, 3);
        assert_eq!(padded.len(), 3);
        assert_eq!(padded[2].values, zero_item(&plan).values);
        assert!(matches!(
            pad_persistent(ActivityId::CPU, &plan, &items, 2),
            Cow::Borrowed(_)
        ));
        // 持ち越しの対象でない activity は埋めない
        let dplan = plan_for(ActivityId::DISK);
        let disks = vec![zeros(&dplan)];
        assert_eq!(pad_persistent(ActivityId::DISK, &dplan, &disks, 5).len(), 1);
    }

    /// tickless CPU の極値は分母 0 の式で記録される (本家 `save_cpu_xstats()`)。
    ///
    /// 差分 0 は NaN (= 最小・最大を更新しない)、逆行は 0、正の差分は +inf。
    #[test]
    fn tickless_extremum_uses_a_zero_denominator() {
        let plan = plan_for(ActivityId::CPU);
        let mut p = cpu_item(&plan, 100, 900);
        put(&plan, &mut p, cpu_col::STEAL, 50);
        let mut c = cpu_item(&plan, 100, 900);
        put(&plan, &mut c, cpu_col::STEAL, 40);
        put(&plan, &mut c, cpu_col::GUEST, 5);
        assert!(cpu_tickless_extremum(&plan, cpu_col::USER, &p, &c).is_nan());
        assert!(cpu_tickless_extremum(&plan, cpu_col::IDLE, &p, &c).is_nan());
        assert_eq!(cpu_tickless_extremum(&plan, cpu_col::STEAL, &p, &c), 0.0);
        assert_eq!(
            cpu_tickless_extremum(&plan, cpu_col::GUEST, &p, &c),
            f64::INFINITY
        );
        // `%usr` = user - guest が減った → 0
        assert_eq!(cpu_tickless_extremum(&plan, cpu_col::USR, &p, &c), 0.0);
    }
    // ========================================================================
    // 指摘 1: CPU 使用率の分母 (guest の二重計上)
    // ========================================================================

    /// **回帰テスト (指摘 1)**: guest を含む CPU の `%user` が仕様どおりになる。
    ///
    /// レビューの具体例: Δuser=100、Δguest=50、他 0。
    /// `guest` は `user` に内包されるので分母は 100 で、`%user` は **100%**。
    /// 修正前は全フィールドの差分を足していたため分母が 150 になり
    /// 約 66.67% (= 仮想マシン稼働中の使用率が過小) になっていた。
    #[test]
    fn cpu_denominator_excludes_guest_time() {
        let plan = plan_for(ActivityId::CPU);
        let p = zeros(&plan);
        let mut c = zeros(&plan);
        put(&plan, &mut c, cpu_col::USER, 100);
        put(&plan, &mut c, cpu_col::GUEST, 50);

        let total = tick_total(&plan, &p, &c);
        assert_eq!(
            total, 100,
            "guest / guest_nice は分母に入れない (03 §1.10-4)"
        );
        // 修正前の「全フィールドの単純和」は 150 だった
        let naive: u64 = c
            .values
            .iter()
            .zip(p.values.iter())
            .filter_map(|(x, y)| match (x, y) {
                (Availability::Present(x), Availability::Present(y)) => Some(x.wrapping_sub(*y)),
                _ => None,
            })
            .sum();
        assert_eq!(naive, 150, "全フィールドを足すと guest を二重計上する");

        let ctx = ComputeContext::new(100).with_tick_total(total);
        assert_eq!(
            compute(ActivityId::CPU, cpu_col::USER, &plan, &p, &c, &ctx).unwrap(),
            100.0,
            "%user は 100% (66.67% ではない)"
        );
        assert_eq!(
            compute(ActivityId::CPU, cpu_col::GUEST, &plan, &p, &c, &ctx).unwrap(),
            50.0,
            "%guest は user の内訳なので 50%"
        );
        assert_eq!(
            compute(ActivityId::CPU, cpu_col::USR, &plan, &p, &c, &ctx).unwrap(),
            50.0,
            "%usr = user - guest"
        );
        // `-u` の 6 列 (%user + %nice + %system + %iowait + %steal + %idle) は 100% になる
        let sum: f64 = [
            cpu_col::USER,
            cpu_col::NICE,
            cpu_col::SYSTEM,
            cpu_col::IOWAIT,
            cpu_col::STEAL,
            cpu_col::IDLE,
        ]
        .iter()
        .map(|col| compute(ActivityId::CPU, *col, &plan, &p, &c, &ctx).unwrap())
        .sum();
        assert!((sum - 100.0).abs() < 1e-9, "合計は 100%: {sum}");
    }

    /// `cpu_interval` が「補正済み前値・分母・状態」を 1 度に返す (指摘 1)。
    ///
    /// sar 互換出力だけでなく sadf / 独自出力 / 集計もこれを通せば
    /// 同じ分母・同じ判定になる。
    #[test]
    fn cpu_interval_reports_online_offline_and_tickless() {
        let plan = plan_for(ActivityId::CPU);

        // --- 通常 ---
        let p = zeros(&plan);
        let mut c = zeros(&plan);
        put(&plan, &mut c, cpu_col::USER, 300);
        put(&plan, &mut c, cpu_col::IDLE, 700);
        let iv = cpu_interval(&plan, &p, &c, CpuRole::Single);
        assert_eq!(iv.state, CpuState::Online);
        assert_eq!(iv.tick_total, 1_000);
        assert!(
            !iv.prev_online,
            "前サンプルが全ゼロ = 復帰直後 (基準値が無い)"
        );
        assert_eq!(iv.role(), CpuRole::Single);
        assert_eq!(iv.tickless_value(cpu_col::IDLE), None);
        let ctx = iv.context(ComputeContext::new(100));
        assert_eq!(ctx.tick_total, Some(1_000));
        assert!(!ctx.aggregate_item);
        assert_eq!(
            compute(ActivityId::CPU, cpu_col::USER, &plan, &iv.prev, &c, &ctx).unwrap(),
            30.0
        );

        // --- オフライン (現サンプルの tick 8 フィールドが全 0) ---
        let mut online_prev = zeros(&plan);
        put(&plan, &mut online_prev, cpu_col::IDLE, 500);
        let off = cpu_interval(&plan, &online_prev, &zeros(&plan), CpuRole::Single);
        assert_eq!(off.state, CpuState::Offline);
        assert!(off.is_offline(), "行そのものを出さない");

        // --- tickless (オンラインだが tick が増えない) ---
        let mut same = zeros(&plan);
        put(&plan, &mut same, cpu_col::IDLE, 500);
        let tl = cpu_interval(&plan, &same, &same, CpuRole::Single);
        assert_eq!(tl.state, CpuState::Tickless);
        assert!(tl.prev_online, "前サンプルでも稼働していた");
        assert_eq!(
            tl.tickless_value(cpu_col::IDLE),
            Some(100.0),
            "%idle = 100.00 (03 §1.4.5)"
        );
        assert_eq!(tl.tickless_value(cpu_col::USER), Some(0.0));
        assert_eq!(tl.tickless_value(cpu_col::GUEST), Some(0.0));

        // --- CPU "all" は tickless にならない (分母 0 は 1 に差し替え) ---
        let agg = cpu_interval(&plan, &same, &same, CpuRole::Aggregate);
        assert_eq!(agg.state, CpuState::Online);
        assert_eq!(
            agg.tick_total, 1,
            "「CPU all が tickless になることはない」"
        );
        assert!(agg.context(ComputeContext::new(100)).aggregate_item);
    }

    /// 集約 (`all` 行) は CPU ごとに補正してから合算する (指摘 1 / 03 §1.4.3)。
    #[test]
    fn cpu_aggregate_context_is_ready_to_use() {
        let plan = plan_for(ActivityId::CPU);
        let mut p0 = zeros(&plan);
        put(&plan, &mut p0, cpu_col::IDLE, 10);
        let mut p1 = zeros(&plan);
        put(&plan, &mut p1, cpu_col::IDLE, 10);
        let prev = vec![zeros(&plan), p0, p1];

        let mut cpu0 = zeros(&plan);
        put(&plan, &mut cpu0, cpu_col::USER, 200);
        put(&plan, &mut cpu0, cpu_col::GUEST, 100);
        put(&plan, &mut cpu0, cpu_col::IDLE, 810);
        let mut cpu1 = zeros(&plan);
        put(&plan, &mut cpu1, cpu_col::USER, 400);
        put(&plan, &mut cpu1, cpu_col::IDLE, 610);
        let curr = vec![zeros(&plan), cpu0, cpu1];

        let agg = aggregate_cpu(&plan, &prev, &curr, false).expect("SMP なので合算する");
        // guest を含んでいても分母は 8 フィールドぶんのまま
        assert_eq!(agg.tick_total, 2_000);
        assert!(!agg.is_offline(1));
        let ctx = agg.context(ComputeContext::new(1_000));
        assert!(ctx.aggregate_item);
        assert_eq!(ctx.tick_total, Some(2_000));
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
            30.0,
            "(200 + 400) / 2000"
        );
    }

    // ========================================================================
    // 指摘 2: 表示単位への変換を計算層で共通化
    // ========================================================================

    /// **回帰テスト (指摘 2)**: 単一区間の集計結果が、その区間の瞬時値と一致する。
    ///
    /// 期間集計は「差分合計 ÷ 分母合計」でレートを出すため、
    /// 表示単位へのスケーリングを掛け忘れると平均 `rkB/s` が 2 倍、
    /// `aqu-sz` が 1,000 倍、`%util` が 10 倍、PSI が 10,000 倍になる。
    /// 1 区間だけ足し込んだ集計は定義上その区間の瞬時値と等しいので、
    /// この不変量を全 activity の全カウンタ列で機械的に確認すれば
    /// スケーリング漏れが検出できる。
    #[test]
    fn single_interval_aggregate_matches_instant_value() {
        let enc = SourceEncoding::new(Endian::Little, LayoutAbi::LP64);
        let mut mismatched: Vec<String> = Vec::new();
        let mut checked = 0usize;

        // 増加する区間と、逆行する区間の両方を見る。
        // 逆行側は「本家が 0 にクランプする列」で集計と瞬時値が揃うことの確認
        // (クランプはサンプル単位なので、集計も区間ごとに 0 を足す必要がある)。
        for increasing in [true, false] {
            for def in crate::layout::registry::all() {
                let Some(rev) = def.latest() else { continue };
                let plan = DecodePlan::build(def, rev, rev.size_lp64, 1, 1, &enc).unwrap();
                let mut lo = zeros(&plan);
                let mut hi = zeros(&plan);
                // 全フィールドを別々の値にして、列の取り違えを検出できるようにする
                for (i, slot) in hi.values.iter_mut().enumerate() {
                    *slot = Availability::Present(1_000 + 37 * i as u64);
                }
                for (i, slot) in lo.values.iter_mut().enumerate() {
                    *slot = Availability::Present(100 + 7 * i as u64);
                }
                let (prev, curr) = if increasing { (&lo, &hi) } else { (&hi, &lo) };

                let mut ctx = ComputeContext::new(250);
                if def.id == ActivityId::CPU {
                    ctx.tick_total = Some(tick_total(&plan, prev, curr));
                }

                for (column, meta) in def.columns.iter().enumerate() {
                    if !meta.is_direct() || meta.kind != ValueKind::Counter {
                        continue;
                    }
                    // 集計が値を作らない区間 (説明できない逆行など) は比較対象外。
                    // 「集計が値を出したなら瞬時値と一致する」が守りたい不変量。
                    let Ok(sample) = rate_sample(&plan, def.id, column, prev, curr, &ctx) else {
                        continue;
                    };
                    let Some(aggregated) = sample.display(def.id, column) else {
                        continue;
                    };
                    let instant = column_value(def.id, column, meta, &plan, prev, curr, &ctx)
                        .expect("全フィールドを埋めてあるので計算できる");
                    checked += 1;
                    if (aggregated - instant).abs() > instant.abs() * 1e-9 + 1e-12 {
                        mismatched.push(format!(
                            "{} {} (increasing={increasing}): 集計 {aggregated} vs 瞬時 {instant}",
                            def.id, meta.public_name
                        ));
                    }
                }
            }
        }

        assert!(
            mismatched.is_empty(),
            "1 区間の集計と瞬時値が食い違う列がある (スケーリング漏れ): {mismatched:?}"
        );
        assert!(checked > 50, "検査した列が少なすぎる: {checked}");
    }

    /// 逆行クランプはサンプル単位で効く (集計も区間ごとに 0 を足す)。
    #[test]
    fn rate_sample_clamps_decrease_per_interval() {
        let plan = plan_for(ActivityId::IO);
        let col = column_of(ActivityId::IO, "tps");
        let meta = &lookup(ActivityId::IO).unwrap().columns[col];
        let mut p = zeros(&plan);
        let mut c = zeros(&plan);
        put(&plan, &mut p, col, 5_000);
        put(&plan, &mut c, col, 4_000);
        let ctx = ComputeContext::new(100);

        // `A_IO` は 7 列すべてに明示クランプがある (03 §id=6)
        assert_eq!(
            column_value(ActivityId::IO, col, meta, &plan, &p, &c, &ctx),
            Ok(0.0)
        );
        let sample = rate_sample(&plan, ActivityId::IO, col, &p, &c, &ctx).unwrap();
        assert_eq!(sample.delta, 0, "その区間の寄与を 0 にして次へ進む");
        assert_eq!(sample.display(ActivityId::IO, col), Some(0.0));
        assert!(!sample.wrapped);
    }

    /// ゼロ補完かどうかは値からは判らないので、列の有無で確かめる (指摘 5)。
    #[test]
    fn column_presence_tells_zero_fill_from_observed_zero() {
        // discard 統計を持たない世代
        let old = plan_for_revision(ActivityId::IO, 0x8b, 40, LayoutAbi::LP64);
        let dtps = column_of(ActivityId::IO, "dtps");
        let tps = column_of(ActivityId::IO, "tps");
        assert!(!column_is_present(&old, dtps), "この世代には無い列");
        assert!(column_is_present(&old, tps));

        // 現行世代なら両方ある
        let modern = plan_for(ActivityId::IO);
        assert!(column_is_present(&modern, dtps));
    }

    /// 保存値のスケールが必要な代表列で、集計経路の値が区間値と一致する。
    ///
    /// 機械的なテストが「たまたま全部 1.0 倍」で通っていないことの確認。
    #[test]
    fn rate_divisor_is_applied_to_scaled_columns() {
        // ディスク: セクタ → kB (÷2)、rq_ticks → aqu-sz (÷1000)、tot_ticks → %util (÷10)
        assert_eq!(rate_divisor(ActivityId::DISK, disk_col::RKB), 2.0);
        assert_eq!(rate_divisor(ActivityId::DISK, disk_col::AQU_SZ), 1000.0);
        assert_eq!(rate_divisor(ActivityId::DISK, disk_col::UTIL_PCT), 10.0);
        assert_eq!(rate_divisor(ActivityId::DISK, disk_col::TPS), 1.0);
        // PSI の累積 µs は `Δµs / (100 × itv)` なので生のレートの 1/10000
        assert_eq!(
            rate_divisor(ActivityId::PSI_CPU, psi_col::SOME_TOTAL),
            10_000.0
        );
        assert_eq!(
            rate_divisor(ActivityId::PSI_IO, psi_col::FULL_TOTAL),
            10_000.0
        );
        assert_eq!(rate_divisor(ActivityId::PSI_IO, psi_col::SOME_10), 1.0);
        // ゲージの換算は別関数 (区間値には既に掛かっている)
        assert_eq!(
            gauge_scale(ActivityId::QUEUE, queue_col::LDAVG_1),
            GaugeScale::Divide(100)
        );
        assert_eq!(
            gauge_scale(ActivityId::PWR_CPU, pwr_cpu_col::MHZ),
            GaugeScale::Divide(100)
        );

        // 1 秒間に 1000 セクタ読んだ区間を 3 つ足した「期間平均」
        let totals = (1_000u128 * 3, 100u128 * 3);
        assert_eq!(
            rate_from_totals(ActivityId::DISK, disk_col::RKB, totals.0, totals.1),
            Some(500.0),
            "1000 セクタ/秒 = 500 kB/s (2 倍にならない)"
        );
        // PSI: 10 秒のうち 1 秒 (1e6 µs) 停止 → 10%
        assert_eq!(
            rate_from_totals(ActivityId::PSI_CPU, psi_col::SOME_TOTAL, 1_000_000, 1_000),
            Some(10.0)
        );
        // 分母が 0 なら「0%」と報告しない
        assert_eq!(
            rate_from_totals(ActivityId::DISK, disk_col::RKB, 10, 0),
            None
        );
    }

    /// 集計用の差分は「一周として説明できる減少」だけを採る。
    #[test]
    fn rate_sample_refuses_unexplainable_decrease() {
        let plan = plan_for(ActivityId::PCSW);
        let mut p = zeros(&plan);
        let mut c = zeros(&plan);
        put(&plan, &mut p, 0, 1_000_000);
        put(&plan, &mut c, 0, 10);
        let ctx = ComputeContext::new(100);
        assert_eq!(
            rate_sample(&plan, ActivityId::PCSW, 0, &p, &c, &ctx),
            Err(ComputeIssue::Discontinuous(
                Discontinuity::AmbiguousDecrease
            )),
            "巨大な外れ値を平均や p95 に入れない"
        );

        // 32bit カウンタの一周は復元して `wrapped` を立てる
        let serial = plan_for(ActivityId::SERIAL);
        let col = column_of(ActivityId::SERIAL, "rcvin");
        let mut sp = zeros(&serial);
        let mut sc = zeros(&serial);
        put(&serial, &mut sp, col, 4_294_967_290);
        put(&serial, &mut sc, col, 4);
        let s = rate_sample(&serial, ActivityId::SERIAL, col, &sp, &sc, &ctx).unwrap();
        assert_eq!(s.delta, 10);
        assert!(s.wrapped);
        assert_eq!(s.display(ActivityId::SERIAL, col), Some(10.0));

        // 派生列は単一の差分を持たない
        let disk = plan_for(ActivityId::DISK);
        assert_eq!(
            rate_sample(
                &disk,
                ActivityId::DISK,
                disk_col::AREQ_SZ,
                &zeros(&disk),
                &zeros(&disk),
                &ctx
            ),
            Err(ComputeIssue::NotImplemented)
        );
    }

    /// **回帰テスト (指摘 2-3)**: `sadf` の別単位列。
    ///
    /// 換算を出力層ごとに書くと `rxkB` が 1,024 倍、`MBfsfree` が
    /// 1,048,576 倍ずれる。計算層の換算と元の列が食い違わないことを確認する。
    #[test]
    fn sadf_unit_columns_convert_from_the_same_computation() {
        let ctx = ComputeContext::new(100); // 1 秒

        // --- ディスク: セクタ = kB 系列の 2 倍 (03 §9.6-10) ---
        let plan = plan_for(ActivityId::DISK);
        let p = zeros(&plan);
        let mut c = zeros(&plan);
        put(&plan, &mut c, disk_col::TPS, 10);
        put(&plan, &mut c, disk_col::RKB, 512);
        put(&plan, &mut c, disk_col::WKB, 128);
        let rkb = compute(ActivityId::DISK, disk_col::RKB, &plan, &p, &c, &ctx).unwrap();
        assert_eq!(rkb, 256.0);
        assert_eq!(
            sadf_unit_value(SadfUnitColumn::DiskReadSectors, &plan, &p, &c, &ctx).unwrap(),
            512.0,
            "rd_sec は rkB/s の 2 倍"
        );
        assert_eq!(
            sadf_unit_value(SadfUnitColumn::DiskWriteSectors, &plan, &p, &c, &ctx).unwrap(),
            128.0
        );
        assert_eq!(
            sadf_unit_value(SadfUnitColumn::DiskDiscardSectors, &plan, &p, &c, &ctx).unwrap(),
            0.0
        );
        let areq = compute(ActivityId::DISK, disk_col::AREQ_SZ, &plan, &p, &c, &ctx).unwrap();
        assert_eq!(areq, 32.0, "(512 + 128) セクタ / 10 I/O / 2");
        assert_eq!(
            sadf_unit_value(SadfUnitColumn::DiskAvgRequestSectors, &plan, &p, &c, &ctx).unwrap(),
            64.0,
            "avgrq-sz は areq-sz の 2 倍"
        );

        // --- ネットワーク: 保存値はバイト/秒。非 human 出力は 1024 で割る (03 §1.8.1) ---
        let net = plan_for(ActivityId::NET_DEV);
        let np = zeros(&net);
        let mut nc = zeros(&net);
        put(&net, &mut nc, net_dev_col::RXKB, 1_048_576);
        put(&net, &mut nc, net_dev_col::TXKB, 2_097_152);
        assert_eq!(
            compute(ActivityId::NET_DEV, net_dev_col::RXKB, &net, &np, &nc, &ctx).unwrap(),
            1_048_576.0,
            "計算層はバイト毎秒を返す"
        );
        assert_eq!(
            sadf_unit_value(SadfUnitColumn::NetRxKilobytes, &net, &np, &nc, &ctx).unwrap(),
            1_024.0,
            "rxkB は kB/s (1024 倍にならない)"
        );
        assert_eq!(
            sadf_unit_value(SadfUnitColumn::NetTxKilobytes, &net, &np, &nc, &ctx).unwrap(),
            2_048.0
        );

        // --- ファイルシステム: 保存値はバイト。MB へ (03 §9.6-2) ---
        let fs = plan_for(ActivityId::FS);
        let fp = zeros(&fs);
        let mut fc = zeros(&fs);
        put(&fs, &mut fc, fs_col::TOTAL, 1_024 * 1_024 * 1_000);
        put(&fs, &mut fc, fs_col::MB_FREE, 1_024 * 1_024 * 300);
        assert_eq!(
            sadf_unit_value(SadfUnitColumn::FsFreeMegabytes, &fs, &fp, &fc, &ctx).unwrap(),
            300.0,
            "MBfsfree は MB (1,048,576 倍にならない)"
        );
        assert_eq!(
            sadf_unit_value(SadfUnitColumn::FsUsedMegabytes, &fs, &fp, &fc, &ctx).unwrap(),
            700.0
        );

        // 列名からの逆引き (CSV / DB の `rxkB/s` と JSON の `rxkB` の両方)
        assert_eq!(
            SadfUnitColumn::from_sadf_name(ActivityId::NET_DEV, "rxkB"),
            Some(SadfUnitColumn::NetRxKilobytes)
        );
        assert_eq!(
            SadfUnitColumn::from_sadf_name(ActivityId::NET_DEV, "rxkB/s"),
            Some(SadfUnitColumn::NetRxKilobytes)
        );
        assert_eq!(
            SadfUnitColumn::from_sadf_name(ActivityId::DISK, "avgrq-sz"),
            Some(SadfUnitColumn::DiskAvgRequestSectors)
        );
        assert_eq!(
            SadfUnitColumn::from_sadf_name(ActivityId::FS, "MBfsused"),
            Some(SadfUnitColumn::FsUsedMegabytes)
        );
        assert_eq!(
            SadfUnitColumn::from_sadf_name(ActivityId::DISK, "tps"),
            None
        );
    }

    // ========================================================================
    // 指摘 3: 行列型の派生列
    // ========================================================================

    /// **回帰テスト (指摘 3)**: `wghMHz` を単一 item 経路以外からも計算できる。
    ///
    /// 以前は sar 互換出力だけが [`weighted_mhz`] を呼んでおり、
    /// sadf / 独自出力は `NeedsItemGroup` になって常に欠損していた。
    #[test]
    fn matrix_row_values_computes_weighted_mhz_for_any_output() {
        let plan = plan_for(ActivityId::PWR_FREQ);
        let mut p0 = zeros(&plan);
        let mut p1 = zeros(&plan);
        let mut c0 = zeros(&plan);
        let mut c1 = zeros(&plan);
        put(&plan, &mut c0, freq_col::FREQ_KHZ, 1_500_999);
        put(&plan, &mut c0, freq_col::TIME_IN_STATE, 100);
        put(&plan, &mut p0, freq_col::TIME_IN_STATE, 0);
        put(&plan, &mut c1, freq_col::FREQ_KHZ, 800_000);
        put(&plan, &mut c1, freq_col::TIME_IN_STATE, 300);
        put(&plan, &mut p1, freq_col::TIME_IN_STATE, 0);

        let ctx = ComputeContext::new(400);
        let got = matrix_row_values(
            ActivityId::PWR_FREQ,
            freq_col::WGH_MHZ,
            &plan,
            &[&p0, &p1],
            &[&c0, &c1],
            &ctx,
        );
        assert_eq!(got.len(), 1, "行全体で 1 値");
        assert_eq!(got[0], Ok(975.0), "(1500 × 100 + 800 × 300) / 400");
    }

    /// `A_IRQ` も同じ形の API で扱える (CPU スロットごとに 1 値)。
    #[test]
    fn matrix_row_values_yields_one_value_per_slot_for_irq() {
        let plan = plan_for(ActivityId::IRQ);
        let mut p_all = zeros(&plan);
        let mut p_cpu0 = zeros(&plan);
        let mut c_all = zeros(&plan);
        let mut c_cpu0 = zeros(&plan);
        // 1 秒で 合計 300 件 / CPU0 は 100 件
        put(&plan, &mut p_all, irq_col::COUNT, 1_000);
        put(&plan, &mut c_all, irq_col::COUNT, 1_300);
        put(&plan, &mut p_cpu0, irq_col::COUNT, 500);
        put(&plan, &mut c_cpu0, irq_col::COUNT, 600);

        let ctx = ComputeContext::new(100);
        let got = matrix_row_values(
            ActivityId::IRQ,
            irq_col::COUNT,
            &plan,
            &[&p_all, &p_cpu0],
            &[&c_all, &c_cpu0],
            &ctx,
        );
        assert_eq!(got, vec![Ok(300.0), Ok(100.0)]);

        // 先頭スロット (= `all` 列) だけ「総数が減ったら 0」のクランプが効く (03 §id=3)。
        // CPU がオフラインになると割り込み総数が減る。
        let mut high = zeros(&plan);
        put(&plan, &mut high, irq_col::COUNT, 1_300);
        let mut low = zeros(&plan);
        put(&plan, &mut low, irq_col::COUNT, 900);
        let clamped = matrix_row_values(
            ActivityId::IRQ,
            irq_col::COUNT,
            &plan,
            &[&high, &high],
            &[&low, &low],
            &ctx,
        );
        assert_eq!(
            clamped[0],
            Ok(0.0),
            "合計列は CPU オフラインで総数が減っても 0"
        );
        // 個別 CPU 列にクランプは無いので、本家と同じ符号なし減算の巨大値が出る
        let per_cpu = clamped[1].unwrap();
        assert!(
            per_cpu > 1.0e9,
            "個別 CPU 列にクランプは無い (03 §id=3): {per_cpu}"
        );
    }

    // ========================================================================
    // 指摘 4: await の加算幅
    // ========================================================================

    /// **回帰テスト (指摘 4)**: `await` の分子が 2³² を超えたときの値。
    ///
    /// 本家の `compute_ext_disk_stats()` は `unsigned int` 同士を足すため、
    /// Δ=3,000,000,000 と Δ=2,000,000,000 の和は 5,000,000,000 ではなく
    /// **705,032,704** (= 5,000,000,000 − 2³²) になる。
    /// 和を f64 で取ると本家と食い違う。
    #[test]
    fn await_numerator_adds_like_unsigned_int() {
        let plan = plan_for(ActivityId::DISK);
        for col in [disk_col::RD_TICKS, disk_col::WR_TICKS, disk_col::DC_TICKS] {
            assert_eq!(
                plan.column_bits(col),
                Some(CounterBits::B32),
                "tick 群は unsigned int (02 §7)"
            );
        }
        let meta = &lookup(ActivityId::DISK).unwrap().columns[disk_col::AWAIT];

        let p = zeros(&plan);
        let mut c = zeros(&plan);
        put(&plan, &mut c, disk_col::TPS, 1); // Δnr_ios = 1
        put(&plan, &mut c, disk_col::RD_TICKS, 3_000_000_000);
        put(&plan, &mut c, disk_col::WR_TICKS, 2_000_000_000);
        let ctx = ComputeContext::new(100);

        assert_eq!(
            column_value(ActivityId::DISK, disk_col::AWAIT, meta, &plan, &p, &c, &ctx).unwrap(),
            705_032_704.0,
            "本家の unsigned int 加算を再現する (5,000,000,000 ではない)"
        );
        // 独自出力・集計では折り返しを再現しない (折り返した分子は待ち時間として
        // 意味を持たないため。互換と独自で意図的に分けている)
        assert_eq!(
            column_value_strict(ActivityId::DISK, disk_col::AWAIT, meta, &plan, &p, &c, &ctx)
                .unwrap(),
            5_000_000_000.0
        );
    }

    /// セクタ系の加算幅は `unsigned long` なので 2³² では折り返さない (指摘 4)。
    #[test]
    fn sector_numerator_adds_in_64_bits() {
        let plan = plan_for(ActivityId::DISK);
        for col in [disk_col::RKB, disk_col::WKB, disk_col::DKB] {
            assert_eq!(
                plan.column_bits(col),
                Some(CounterBits::B64),
                "rd_sect / wr_sect / dc_sect は unsigned long (LP64 では 8 バイト)"
            );
        }
        let meta = &lookup(ActivityId::DISK).unwrap().columns[disk_col::AREQ_SZ];

        let p = zeros(&plan);
        let mut c = zeros(&plan);
        put(&plan, &mut c, disk_col::TPS, 1);
        put(&plan, &mut c, disk_col::RKB, 3_000_000_000);
        put(&plan, &mut c, disk_col::WKB, 2_000_000_000);
        let ctx = ComputeContext::new(100);

        assert_eq!(
            column_value(
                ActivityId::DISK,
                disk_col::AREQ_SZ,
                meta,
                &plan,
                &p,
                &c,
                &ctx
            )
            .unwrap(),
            2_500_000_000.0,
            "5,000,000,000 セクタ / 1 I/O / 2 (折り返さない)"
        );
    }

    // ========================================================================
    // 指摘 5: 旧世代の欠落フィールドのゼロ補完
    // ========================================================================

    /// **回帰テスト (指摘 5)**: discard 統計を持たない旧 `A_IO`。
    ///
    /// 本家は足りないフィールドを 0 埋めした構造体で計算するので
    /// `dtps` / `bdscd` は `0.00` と表示される (03 §1.9-1)。
    /// 以前はこのゼロ補完が `sar` 互換テキストの**出力層**にしか無く、
    /// `sadf` は同じ列を空欄 / `null` にしていた (互換出力どうしの不統一)。
    #[test]
    fn missing_discard_fields_are_zero_filled_only_in_compat() {
        // v11.7.1〜v12.1.1 の A_IO (5 フィールド / 40 バイト) に discard 統計は無い
        let plan = plan_for_revision(ActivityId::IO, 0x8b, 40, LayoutAbi::LP64);
        let dtps = column_of(ActivityId::IO, "dtps");
        let bdscd = column_of(ActivityId::IO, "bdscd");
        let tps = column_of(ActivityId::IO, "tps");
        assert_eq!(
            plan.column_value(&zeros(&plan).values, dtps),
            Availability::UnsupportedBySource,
            "この世代には dk_drive_dio が無い"
        );

        let p = zeros(&plan);
        let mut c = zeros(&plan);
        put(&plan, &mut c, tps, 500);
        let ctx = ComputeContext::new(100);
        let meta = |col: usize| &lookup(ActivityId::IO).unwrap().columns[col];

        // --- 互換出力: 本家と同じ 0.00 ---
        for col in [dtps, bdscd] {
            assert_eq!(
                column_value(ActivityId::IO, col, meta(col), &plan, &p, &c, &ctx),
                Ok(0.0),
                "本家はゼロ補完した構造体で計算する"
            );
        }
        assert_eq!(
            column_value(ActivityId::IO, tps, meta(tps), &plan, &p, &c, &ctx).unwrap(),
            500.0,
            "存在する列は普通に計算する"
        );

        // --- 独自出力: 欠落のまま ---
        for col in [dtps, bdscd] {
            assert_eq!(
                column_value_strict(ActivityId::IO, col, meta(col), &plan, &p, &c, &ctx),
                Err(ComputeIssue::UnsupportedBySource),
                "0 と欠落を混同しない"
            );
        }
    }

    /// 欠落の種類を分類できる (指摘 5)。
    ///
    /// 互換出力は `ZeroFilled` を 0 として出し、独自出力は欠落のまま残す。
    #[test]
    fn missing_kind_separates_zero_filled_from_absent() {
        assert_eq!(
            missing_kind(ComputeIssue::UnsupportedBySource),
            Some(MissingKind::ZeroFilled),
            "その世代にフィールドが無いだけ = 本家は 0 埋めする"
        );
        for issue in [
            ComputeIssue::MissingInSample,
            ComputeIssue::Discontinuous(Discontinuity::FirstSample),
            ComputeIssue::Discontinuous(Discontinuity::Restart),
            ComputeIssue::Discontinuous(Discontinuity::AmbiguousDecrease),
        ] {
            assert_eq!(
                missing_kind(issue),
                Some(MissingKind::Absent),
                "ゼロ補完してはいけない: {issue:?}"
            );
        }
        for issue in [
            ComputeIssue::NotNumeric,
            ComputeIssue::NeedsItemGroup,
            ComputeIssue::NotImplemented,
        ] {
            assert_eq!(
                missing_kind(issue),
                None,
                "値の欠落ではない (呼び出し方の問題): {issue:?}"
            );
        }
    }

    /// レコードに値が無い欠落は、方針に関わらずゼロ補完しない (指摘 5)。
    ///
    /// 「その世代にフィールドが無い」(= 本家も 0) と
    /// 「このサンプルで値が読めなかった」(= 本家なら行が無い) は別物。
    #[test]
    fn missing_in_sample_is_never_zero_filled() {
        let plan = plan_for(ActivityId::PCSW);
        let meta = &lookup(ActivityId::PCSW).unwrap().columns[0];
        let p = zeros(&plan);
        // 値を持たない item (レコードが短い等)
        let c = ItemSnapshot {
            key: None,
            texts: Vec::new(),
            values: vec![Availability::MissingInSample; plan.fields.len()],
        };
        let ctx = ComputeContext::new(100);
        assert_eq!(
            column_value(ActivityId::PCSW, 0, meta, &plan, &p, &c, &ctx),
            Err(ComputeIssue::MissingInSample)
        );
    }

    // ---- 未使用スロットの判定 (golden 比較 ⑤ / ⑥) ----

    /// `A_DISK` は `major + minor == 0` のスロットを未使用とみなす。
    ///
    /// `file_activity.nr` は確保枠数なので、枠が余っていると
    /// 「`dev0-0` が 0.00 を並べる行」が出る。本家はこの番兵で飛ばす。
    #[test]
    fn disk_slot_without_major_minor_is_unused() {
        let plan = plan_for(ActivityId::DISK);
        let empty = zeros(&plan);
        assert!(is_unused_item(ActivityId::DISK, 3, &plan, &empty));

        // minor だけでも 0 でなければ使用中 (`dev8-0` は major=8 / minor=0)
        let mut used = zeros(&plan);
        put(&plan, &mut used, disk_col::MAJOR, 8);
        assert!(!is_unused_item(ActivityId::DISK, 3, &plan, &used));

        let mut minor_only = zeros(&plan);
        put(&plan, &mut minor_only, disk_col::MINOR, 1);
        assert!(!is_unused_item(ActivityId::DISK, 3, &plan, &minor_only));
    }

    /// `A_FS` は `f_blocks == 0` のスロットを未使用とみなす。
    #[test]
    fn filesystem_slot_without_total_blocks_is_unused() {
        let plan = plan_for(ActivityId::FS);
        let empty = zeros(&plan);
        assert!(is_unused_item(ActivityId::FS, 1, &plan, &empty));

        let mut used = zeros(&plan);
        put(&plan, &mut used, fs_col::TOTAL, 1 << 30);
        assert!(!is_unused_item(ActivityId::FS, 1, &plan, &used));
    }

    /// `A_NET_SOFT` はカウンタが全 0 の CPU をオフラインとみなすが、
    /// CPU "all" (先頭スロット) は常に表示する。
    #[test]
    fn net_soft_offline_cpu_is_unused_but_all_is_kept() {
        let plan = plan_for(ActivityId::NET_SOFT);
        let z = zeros(&plan);
        assert!(
            !is_unused_item(ActivityId::NET_SOFT, 0, &plan, &z),
            "index 0 は CPU \"all\" なので値が 0 でも出す"
        );
        assert!(is_unused_item(ActivityId::NET_SOFT, 1, &plan, &z));

        // カウンタが 1 本でも動いていればオンライン
        let mut online = zeros(&plan);
        put(&plan, &mut online, soft_col::RX_RPS, 1);
        assert!(!is_unused_item(ActivityId::NET_SOFT, 1, &plan, &online));

        // 本家は backlog_len もオンライン判定に含める。
        let mut backlog_only = zeros(&plan);
        put(&plan, &mut backlog_only, soft_col::BLG_LEN, 5);
        assert!(
            !is_unused_item(ActivityId::NET_SOFT, 1, &plan, &backlog_only),
            "blg_len が残っていればオンラインと判定する"
        );
    }

    /// 番兵を持たない activity は常に使用中 (枠を飛ばさない)。
    #[test]
    fn activities_without_sentinel_are_never_unused() {
        for id in [ActivityId::CPU, ActivityId::MEMORY, ActivityId::QUEUE] {
            let plan = plan_for(id);
            let z = zeros(&plan);
            assert!(!is_unused_item(id, 1, &plan, &z), "{id:?}");
        }
    }

    /// `kbavail` 列そのものも、持たない世代では `kbmemfree` で代用する
    /// (golden 比較 ④)。
    ///
    /// 派生列 (`kbmemused` / `%memused`) だけを代用しても、
    /// 表示される `kbavail` 列が 0 になって本家と食い違う。
    #[test]
    fn kbavail_column_falls_back_to_free_memory() {
        let def = lookup(ActivityId::MEMORY).unwrap();
        let rev = def.latest().unwrap();
        let enc = SourceEncoding::new(Endian::Little, LayoutAbi::LP64);
        // `availablekb` を持たない旧世代を、申告サイズを削って再現する
        // (`DecodePlan::build` は申告領域に収まらないフィールドを欠落にする)
        let ctx = ComputeContext::new(100);
        let meta = &def.columns[mem_col::KBAVAIL];

        let modern = DecodePlan::build(def, rev, rev.size_lp64, 1, 1, &enc).unwrap();
        let mut m = zeros(&modern);
        put(&modern, &mut m, mem_col::KBMEMFREE, 4_066_192);
        put(&modern, &mut m, mem_col::KBAVAIL, 5_791_704);
        assert_eq!(
            column_value(
                ActivityId::MEMORY,
                mem_col::KBAVAIL,
                meta,
                &modern,
                &m,
                &m,
                &ctx
            ),
            Ok(5_791_704.0),
            "持っている世代は自分の値を出す"
        );

        // 旧世代を模したゼロ幅の計画。`availablekb` が欠落する構成を探す。
        let old = def
            .revisions
            .iter()
            .find(|r| {
                let p = DecodePlan::build(def, r, r.size_lp64, 1, 1, &enc);
                p.map(|p| !column_is_present(&p, mem_col::KBAVAIL))
                    .unwrap_or(false)
            })
            .map(|r| DecodePlan::build(def, r, r.size_lp64, 1, 1, &enc).unwrap());
        let Some(old) = old else {
            // レジストリに `availablekb` 抜きの revision が無い構成では検証できない
            return;
        };
        let mut o = zeros(&old);
        put(&old, &mut o, mem_col::KBMEMFREE, 5_646_540);
        assert_eq!(
            column_value(
                ActivityId::MEMORY,
                mem_col::KBAVAIL,
                meta,
                &old,
                &o,
                &o,
                &ctx
            ),
            Ok(5_646_540.0),
            "持たない世代は kbmemfree を出す (本家と同じ値)"
        );
        // 独自出力・集計は欠落を欠落として受け取る
        assert_eq!(
            column_value_strict(
                ActivityId::MEMORY,
                mem_col::KBAVAIL,
                meta,
                &old,
                &o,
                &o,
                &ctx
            ),
            Err(ComputeIssue::UnsupportedBySource),
            "厳密モードは代用しない"
        );
    }
}

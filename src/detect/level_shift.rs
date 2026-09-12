//! 時間的変化 — 前後の窓の水準差による検出。
//!
//! 「ある時刻から高止まりした」という変化は、逸脱検出では拾えない。
//! 変化後の値が長時間を占めていれば、それが比較基準そのものになるためである。
//! この経路は**時刻を挟んだ前後の窓を比べる**ので、基準が変化後へ寄っていても
//! 水準が違う 2 つの時間帯を指せる。
//!
//! # 指せるのは「分割時刻」であって「変化した時刻」ではない
//!
//! この経路が返す時刻は**採用した前後窓の境目**である。
//! 「その瞬間に水準が移った」ことは観測されていない。
//!
//! - 採取と採取の間に何が起きていたかは観測されていない (規律 5)。
//!   境目の前後 1 採取のどこで動いたかは決められない
//! - 連続的に立ち上がる曲線 (前窓 `[0,0,0,0,1]` / 後窓 `[5,10,10,10,10]`) からも
//!   前後窓の差は出る。傾向の除去は**一次の傾き**しか説明しないので、
//!   滑らかな非線形の立ち上がりは残る。段差の存在自体が確定していない
//!
//! したがって出力の語は「この時刻に変わった」ではなく
//! 「この境目の前後で水準が違う」に留める。**時刻を出すこと自体は有用**で、
//! 調査の起点になる (`--from` / `--to` で周辺を見に行ける)。
//!
//! # 窓はサンプル数ではなく時間幅で決める
//!
//! 10 秒採取と 10 分採取で同じサンプル数の窓を使うと、前者は 50 秒、
//! 後者は 50 分を比べてしまう。窓幅は
//! [`crate::detect::DetectThresholds::shift_window_secs`] (既定 30 分) を
//! 採取間隔で割って決め、サンプル数の下限
//! ([`crate::detect::DetectThresholds::shift_window_min_samples`]) で底を打つ。
//! 窓が小さいと単発のスパイクが窓の中央値を動かしてしまうため。
//!
//! **底を打った結果、実際の窓は要求より長くなる。** 既定の 600 秒採取では
//! `max(ceil(1800/600), 5) = 5` 点で、区間値なら各窓 3000 秒である。
//! 「窓 1800 秒で判定した」と書くと嘘になるので、実際に使った点数と窓幅を
//! 根拠 ([`crate::detect::DecisionBasis::LevelShift`] の `window_samples` /
//! `window_secs`) に載せて出力へ渡す。
//!
//! # 欠測を挟んだ前後窓は比べない
//!
//! 欠測 (値が得られなかった採取) は連続区間を切らない
//! ([`crate::detect::PreparedSeries::from_timeline`])。したがって
//! **欠測を除いた配列の上で窓を切ると、低値 5 点と高値 5 点の間に
//! 数時間の空白があっても水準変化が成立してしまう**。
//! それは「この時刻に水準が変わった」と言える材料ではない。
//! 固定条件・逸脱の経路には [`crate::detect::group_runs`] に時刻の
//! 接続チェックがあるが、この経路には無かった。
//!
//! そこで**前後窓を合わせた区間が実時間で連続していること**を要求し、
//! 途切れていればその分割候補を評価しない。1 点でも欠測を挟むと
//! その間に何が起きたか分からないので、許容量は置かない。
//! どの候補も評価できなければ「検出なし」ではなく**評価不能**として返す。
//!
//! # 傾向と段差を区別する
//!
//! 各窓を自身の中央値で中心化しても、**窓をまたぐ傾向は残る**。
//! `runq-sz` が毎回 1 ずつ増えるだけの系列でも、前窓 `[0,1,2,3,4]` /
//! 後窓 `[5,6,7,8,9]` は差 5・MAD 1・正規化差 3.37・持続率 80% を満たし、
//! 存在しない段差を「この時刻に変わった」として報告してしまう。
//! 自己相関のある負荷増加や定時バッチでは常に起こる。
//!
//! そこで窓内の傾きから**傾向で説明できる量**を見積もり、引いた残りで判定する
//! ([`trend_explained_shift`])。傾きは前半・後半の中央値差を実時間で割った
//! 記述量で、確率でも検定統計量でもない (規律 1)。
//! **傾向は合格候補を増やさない側にしか使わない** — 粗い推定で段差を
//! 大きくできる作りにすると、推定誤差が新しい検出を生む。
//!
//! 「合格候補が増えない」は「報告件数が必ず減る」ではない。
//! 隣接候補を 1 件へ畳む [`collapse`] があるので、候補群の中央だけが落ちれば
//! 1 件が 2 件へ分かれることがある。
//!
//! 除去できるのは**一次の傾き**だけである。滑らかな非線形の立ち上がりは
//! 残るので、「段差があった」とまでは主張しない (この経路の冒頭を参照)。
//!
//! # 判定は 3 条件 + 持続性
//!
//! | # | 条件 | 理由 |
//! |---|---|---|
//! | a | 絶対差が指標ごとの最小有意変化量以上 | 単位を持つ差でないと読み手が判断できない |
//! | a′ | **傾向を引いた段差**も最小有意変化量以上 | 滑らかな増加から架空の変化点を作らない |
//! | b | 散らばりが測れたときのみ、正規化差も閾値以上 | 揺れの大きい系列で小さな段差を拾わない |
//! | c | 後窓の 7 割以上が同方向へ最小変化量以上動いている | 単発の値が窓の中央値を押し出すのを防ぐ |
//!
//! **b を無条件の AND にしない。** 一定値から一定値へ動いた明瞭な段差では
//! 散らばりが 0 になるので、b を必須にすると最も分かりやすい変化を落とす。
//! 散らばりが測れないときは b を課さず、「正規化できなかった」ことを記録する
//! (**ε で割らない**)。
//!
//! # 正規化の尺度は窓ごとに中心化してから束ねる
//!
//! 前後の窓を合わせたまま MAD を取ると、**尺度が段差自身で膨らむ**ので
//! 大きな段差ほど正規化差が小さくなる。各窓を自身の中央値で中心化した
//! 残差を束ねてから MAD を取る。
//!
//! # 不連続を跨がない
//!
//! 連続区間の内側でしか窓を取らない。RESTART を挟んだ前後を比べると
//! 「再起動で水準が変わった」を異変として報告してしまう。

use crate::analyze::assessment::{NotEvaluated, RouteStatus};
use crate::analyze::metric_catalog::CatalogEntry;

use super::{
    Baseline, DETECTOR_VERSION, DecisionBasis, DecisionEvidence, DetectOptions, Detection,
    MAD_SCALE, Observation, Pattern, PreparedSeries, SeriesKey, ShiftDirection, mad,
    magnitude_floor, median,
};

/// 1 つの分割候補の評価結果。
#[derive(Debug, Clone, Copy)]
struct Split {
    /// 後窓の開始位置 (連続区間内の索引)。
    at: usize,
    before_median: f64,
    after_median: f64,
    shift: f64,
    /// `shift` のうち窓内の傾きで説明できる量。
    trend: f64,
    /// 傾向を引いた残りの段差。**判定はこれで行う。**
    step: f64,
    min_shift: f64,
    pooled_mad: Option<f64>,
    normalized: Option<f64>,
    persistence: f64,
    /// 順位付けに使う大きさ。正規化できたならそれを、できなければ
    /// 最小変化量で割った絶対差を使う (単位の違う系列を混ぜないため)。
    rank: f64,
}

/// 実際に使った窓の寸法。
///
/// 要求幅 (設定値) と**実際の幅**は違う。点数の下限で底を打つと
/// 実際の窓は要求より長くなるので、両方を根拠に載せる。
#[derive(Debug, Clone, Copy)]
struct WindowPlan {
    /// 前後それぞれの点数。
    points: usize,
    /// 設定が要求した窓幅 (秒)。
    requested_secs: u64,
    /// 実際の窓幅 (秒) = 点数 × 採取間隔。
    effective_secs: u64,
}

/// 水準変化で検出する。
pub fn detect(
    series: &PreparedSeries,
    entry: &'static CatalogEntry,
    baseline: &Baseline,
    opts: &DetectOptions,
) -> (Vec<Detection>, RouteStatus) {
    if matches!(
        entry.shift,
        crate::analyze::metric_catalog::ShiftMagnitude::NotEvaluated
    ) {
        return (
            Vec::new(),
            RouteStatus::NotEvaluated {
                reason: NotEvaluated::LevelShiftNotDeclared,
            },
        );
    }
    if series.is_empty() {
        return (
            Vec::new(),
            RouteStatus::NotEvaluated {
                reason: NotEvaluated::NoUsableObservation,
            },
        );
    }

    let th = &opts.thresholds;
    let mut out = Vec::new();
    // 「前後窓を実時間で連続して取れた候補が 1 つでもあったか」。
    // 窓が取れても欠測を挟んでいれば評価できていない (規律 7)
    let mut any_evaluable = false;

    for segment in series.iter_segments() {
        let Some(interval) = representative_interval(segment) else {
            continue;
        };
        let w = window_points(interval, th.shift_window_secs, th.shift_window_min_samples);
        if segment.len() < 2 * w {
            continue;
        }
        let plan = WindowPlan {
            points: w,
            requested_secs: th.shift_window_secs,
            effective_secs: effective_window_secs(interval, w, series.origin.is_instant()),
        };

        // 分割候補を順に評価する
        let mut candidates: Vec<Split> = Vec::new();
        for at in w..=segment.len() - w {
            // **欠測を挟む比較は評価しない。** 境目を指す意味が無くなる
            if !is_contiguous(&segment[at - w..at + w]) {
                continue;
            }
            any_evaluable = true;

            let before = &segment[at - w..at];
            let after = &segment[at..at + w];
            let Some(mut split) = evaluate(before, after, entry) else {
                continue;
            };
            split.at = at;
            let direction = ShiftDirection::of(split.shift);
            if !accepts(entry, direction) {
                continue;
            }
            if split.shift.abs() < split.min_shift {
                continue;
            }
            // 傾向で説明できる差は段差ではない
            if split.step.abs() < split.min_shift {
                continue;
            }
            if split.persistence < th.shift_persistence_share {
                continue;
            }
            // 散らばりが測れたときだけ正規化差を課す (**ε で割らない**)
            if let Some(n) = split.normalized
                && n < th.shift_normalized
            {
                continue;
            }
            candidates.push(split);
        }

        // 隣接する候補は 1 件へ畳む。**同じ段差を窓ごとに何度も報告しない。**
        for best in collapse(&candidates) {
            let after = &segment[best.at..best.at + w];
            let before = series.support_of(&segment[best.at - w..best.at]);
            out.push(build(series, entry, baseline, best, plan, before, after));
        }
    }

    // 閾値は根拠にそのまま載せる (読み手が判定を再現できるように)
    stamp_thresholds(&mut out, opts);

    if out.is_empty() {
        if !any_evaluable {
            // 実時間で連続した前後窓が取れなかった。**「検出なし」ではない。**
            //
            // 長さが足りない場合と、欠測を挟んでいて連続した窓が取れない場合が
            // ある。どちらも「取れる窓が無い」だが、理由を分けて出せると
            // 読み手が次に何をすべきか分かる (Issue #5 の 4 / 5。
            // `NotEvaluated` の variant 追加は `analyze::assessment` 側の担当)
            return (
                Vec::new(),
                RouteStatus::NotEvaluated {
                    reason: NotEvaluated::NoWindowLongEnough,
                },
            );
        }
        return (out, RouteStatus::Evaluated);
    }
    let count = out.len();
    (out, RouteStatus::Detected { count })
}

/// 窓の点数。
///
/// 要求幅を採取間隔で割り、**点数の下限で底を打つ**。
/// 底を打った場合は実際の窓幅が要求より長くなるので、
/// [`WindowPlan::effective_secs`] として根拠に載せる。
fn window_points(interval: u64, window_secs: u64, min_samples: usize) -> usize {
    let by_time = window_secs.div_ceil(interval.max(1)) as usize;
    by_time.max(min_samples)
}

/// 実際の窓幅 (秒)。
///
/// 区間値は `点数 × 採取間隔` を覆う。瞬時値は**採取と採取の間を
/// 観測していない**ので、5 点の広がりは 4 間隔ぶんである (規律 5)。
/// [`super::PreparedSeries::support_of`] が作る範囲と一致させる。
fn effective_window_secs(interval: u64, points: usize, instant: bool) -> u64 {
    let spans = if instant {
        (points as u64).saturating_sub(1)
    } else {
        points as u64
    };
    interval.saturating_mul(spans)
}

/// 観測が実時間で連続しているか (欠測を挟んでいないか)。
///
/// 欠測点は連続区間を切らないので、1 つの連続区間の中でも観測の間に
/// 穴が開く。穴を挟んだ前後の窓から「この時刻に水準が変わった」とは言えない。
fn is_contiguous(window: &[Observation]) -> bool {
    window.windows(2).all(|p| p[0].end_ust == p[1].start_ust)
}

/// 連続区間の採取間隔の代表値 (秒)。
fn representative_interval(segment: &[Observation]) -> Option<u64> {
    let mut gaps: Vec<u64> = segment
        .iter()
        .map(Observation::interval_secs)
        .filter(|g| *g > 0)
        .collect();
    if gaps.is_empty() {
        return None;
    }
    gaps.sort_unstable();
    Some(gaps[gaps.len() / 2])
}

/// カタログの関心方向に合うか。
///
/// **逸脱 (`deviation`) の宣言ではなく [`CatalogEntry::shift_interest`] を見る。**
/// 「外れ値として上だけ見たい」と「水準が動いたことを両方向で見たい」は別の関心で、
/// 前者を流用すると処理量の指標で**停止を検出前に捨てる**ことになる。
fn accepts(entry: &CatalogEntry, direction: ShiftDirection) -> bool {
    let interest = entry.shift_interest();
    interest.is_none() || interest.accepts(direction)
}

fn evaluate(before: &[Observation], after: &[Observation], entry: &CatalogEntry) -> Option<Split> {
    let bv: Vec<f64> = before.iter().map(|o| o.value).collect();
    let av: Vec<f64> = after.iter().map(|o| o.value).collect();
    let before_median = median(&bv)?;
    let after_median = median(&av)?;
    // 相対で宣言された指標は、**入力全体ではなく前窓の水準**を基準にする。
    // 局所的な水準に対する変化として読めるようにするため。
    let min_shift = magnitude_floor(entry.shift, before_median)?;
    let shift = after_median - before_median;

    // 窓をまたぐ傾向は窓ごとの中心化では消えない。引いた残りで判定する
    let trend = trend_explained_shift(before, after);
    let step = step_of(shift, trend);

    // 各窓を自身の中央値で中心化した残差を束ねる。
    // 段差を含めたまま束ねると尺度が段差自身で膨らむ。
    let mut residuals: Vec<f64> = bv.iter().map(|v| v - before_median).collect();
    residuals.extend(av.iter().map(|v| v - after_median));
    let pooled = mad(&residuals, 0.0).filter(|m| *m > 0.0);
    // 正規化するのは**段差**。観測差を正規化すると傾向の分まで
    // 「散らばりの何倍」に数えてしまう
    let normalized = pooled.map(|m| step.abs() / (MAD_SCALE * m));

    // 持続性: 後窓のうち前窓の水準から同方向へ最小変化量以上離れた割合。
    // **これは観測された値についての事実**なので傾向を引かない
    // (「後窓の値が前窓の水準から離れたままだったか」を数えている)。
    let direction = ShiftDirection::of(shift);
    let moved = av
        .iter()
        .filter(|v| {
            let d = *v - before_median;
            ShiftDirection::of(d) == direction && d.abs() >= min_shift
        })
        .count();
    let persistence = moved as f64 / av.len() as f64;

    let rank = match normalized {
        Some(n) => n,
        // 正規化できない場合は単位を消すため最小変化量で割る
        None if min_shift > 0.0 => step.abs() / min_shift,
        None => step.abs(),
    };

    Some(Split {
        at: 0,
        before_median,
        after_median,
        shift,
        trend,
        step,
        min_shift,
        pooled_mad: pooled,
        normalized,
        persistence,
        rank,
    })
}

/// 観測差のうち、窓内の傾きで説明できる量。
///
/// 各窓を前半と後半に割り、**値の中央値と代表時刻の中央値**の差から
/// 1 秒あたりの傾きを取る。前後 2 つの窓から得た傾きを平均し、
/// 2 つの窓の代表時刻の差を掛けたものが「傾向だけで生じる中央値差」である。
///
/// **サンプル数ではなく実時間で測る。** 採取間隔が窓の中で変わると
/// (`sadc` の停止・再開、別の採取間隔での追記)、点数あたりの傾きは
/// 実際の変化率と一致しない。時間に対して一定の率で増える系列でも、
/// 4 秒間隔の 5 点と 100 秒間隔の 5 点を比べれば「点数あたり」では
/// 傾向を過小に見積もり、段差が残ってしまう。
///
/// **確率でも検定統計量でもない。** 「この窓では 1 秒あたりこれだけ
/// 動いていた」という記述量である (規律 1)。中央値の差を使うのは、
/// 単発のスパイクで傾きが跳ねないようにするため。
///
/// 窓が 4 点未満のときは前半・後半に 2 点ずつ取れないので 0 を返す
/// (傾きを推定しない = 傾向による棄却をしない)。
fn trend_explained_shift(before: &[Observation], after: &[Observation]) -> f64 {
    let w = before.len();
    if w < 4 || after.len() != w {
        return 0.0;
    }
    let half = w / 2;
    let slope = |win: &[Observation]| -> Option<f64> {
        let (lo_value, lo_time) = medians_of(&win[..half])?;
        let (hi_value, hi_time) = medians_of(&win[w - half..])?;
        let elapsed = hi_time - lo_time;
        (elapsed > 0.0).then(|| (hi_value - lo_value) / elapsed)
    };
    let (Some(before_slope), Some(after_slope)) = (slope(before), slope(after)) else {
        return 0.0;
    };
    let (Some((_, before_time)), Some((_, after_time))) = (medians_of(before), medians_of(after))
    else {
        return 0.0;
    };
    let elapsed = after_time - before_time;
    if elapsed <= 0.0 {
        return 0.0;
    }
    (before_slope + after_slope) / 2.0 * elapsed
}

/// 観測列の値と代表時刻の中央値。
fn medians_of(window: &[Observation]) -> Option<(f64, f64)> {
    let values: Vec<f64> = window.iter().map(|o| o.value).collect();
    let times: Vec<f64> = window.iter().map(representative_time).collect();
    Some((median(&values)?, median(&times)?))
}

/// 観測が代表する時刻 (エポック秒)。
///
/// 瞬時値は採取時点そのもの、区間値は区間の中央を代表とする。
/// 区間値に区間の始点を使うと、傾きの分母が半区間ぶんずれる。
fn representative_time(o: &Observation) -> f64 {
    if o.origin.is_instant() {
        o.end_ust as f64
    } else {
        (o.start_ust as f64 + o.end_ust as f64) / 2.0
    }
}

/// 観測差から傾向で説明できる分を引いた段差。
///
/// **傾向は合格候補を増やさない側にしか使わない。** 傾きの推定は窓内の
/// 中央値差という粗いものなので、それで段差を大きくできる作りにすると
/// 推定誤差が新しい検出を生む。したがって結果は
///
/// - 符号が `shift` と同じ (または 0)
/// - 大きさが `|shift|` を超えない
///
/// を必ず満たす。傾向が逆向きなら観測差をそのまま採り、
/// 傾向が観測差以上なら「傾向だけで説明できる」として 0 にする。
fn step_of(shift: f64, trend: f64) -> f64 {
    if trend * shift <= 0.0 {
        // 傾向が無い / 逆向き。観測差をそのまま採る (増やさない)
        shift
    } else if trend.abs() >= shift.abs() {
        // 観測差は傾向だけで説明できる
        0.0
    } else {
        shift - trend
    }
}

/// 分割候補の優劣。
///
/// 大きさが同じなら**持続性が高い方**を採る。一定値から一定値への段差では
/// 大きさ (正規化できないので絶対差 ÷ 最小変化量) が窓をずらしても
/// 同じ値になり、そのままでは段差にまたがった窓が先に採られてしまう。
/// 持続性は段差にぴったり合った窓で最大になるので、境界が正しく決まる。
fn rank_key(s: &Split) -> (f64, f64) {
    (s.rank, s.persistence)
}

/// 隣接する分割候補を 1 件へ畳む。
///
/// 同じ段差は窓をずらすたびに条件を満たすので、連続する候補の中から
/// 最も良いものだけを残す。**完全に同じなら早い時刻を残す** (決定的にする)。
fn collapse(candidates: &[Split]) -> Vec<Split> {
    let mut out: Vec<Split> = Vec::new();
    let mut best: Option<Split> = None;
    let mut prev_at: Option<usize> = None;
    for c in candidates {
        let adjacent = prev_at.is_some_and(|p| c.at == p + 1);
        if !adjacent && let Some(b) = best.take() {
            out.push(b);
        }
        best = match best {
            Some(b) if rank_key(&b) >= rank_key(c) => Some(b),
            _ => Some(*c),
        };
        prev_at = Some(c.at);
    }
    if let Some(b) = best {
        out.push(b);
    }
    out
}

fn build(
    series: &PreparedSeries,
    entry: &'static CatalogEntry,
    baseline: &Baseline,
    split: Split,
    plan: WindowPlan,
    before: super::TemporalSupport,
    after: &[Observation],
) -> Detection {
    let after_support = series.support_of(after);
    let direction = ShiftDirection::of(split.shift);
    let basis = DecisionBasis::LevelShift {
        before_median: split.before_median,
        after_median: split.after_median,
        shift: split.shift,
        trend_explained_shift: split.trend,
        step_shift: split.step,
        min_shift: split.min_shift,
        pooled_mad: split.pooled_mad,
        normalized_shift: split.normalized,
        normalized_threshold: 0.0,
        persistence_share: split.persistence,
        persistence_threshold: 0.0,
        window_samples: plan.points as u64,
        window_requested_secs: plan.requested_secs,
        window_secs: plan.effective_secs,
        before,
        after: after_support,
    };
    Detection {
        detector_version: DETECTOR_VERSION,
        series: SeriesKey::from_metric(&series.key),
        metric_label: entry.label,
        unit: series.unit,
        kind: series.kind,
        origin: series.origin,
        pattern: Pattern::LevelShift { direction },
        support: after_support,
        baseline: baseline.evidence.clone(),
        decision: DecisionEvidence::new(basis, after),
        // 水準変化は「いつ変わったか」を足すが、絶対水準の裏付けは無い
        base_priority: crate::analyze::assessment::Priority::Watch,
        possible_interpretations: entry.interpretations,
        not_established: entry.not_established,
    }
}

/// 出力に載せる閾値を設定から埋める。
///
/// [`build`] は閾値を知らないので、組み立て後に差し込む。
fn stamp_thresholds(detections: &mut [Detection], opts: &DetectOptions) {
    for d in detections {
        if let DecisionBasis::LevelShift {
            normalized_threshold,
            persistence_threshold,
            ..
        } = &mut d.decision.basis
        {
            *normalized_threshold = opts.thresholds.shift_normalized;
            *persistence_threshold = opts.thresholds.shift_persistence_share;
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::analyze::metric_catalog::{DeviationInterest, ItemScope};
    use crate::analyze::timeline::SINGLE_ITEM;

    use super::super::testing::*;
    use super::super::*;
    use super::*;

    /// 水準変化の方向宣言だけを差し替えたカタログ項目 (テスト用)。
    ///
    /// **逸脱の宣言 (`deviation`) は `Upper` に固定する。**
    /// この経路が見るのは `shift_direction` であって `deviation` ではない、
    /// という分離をテストで固定するため (逸脱の宣言を流用していた頃は、
    /// 処理量の指標で「止まった」を検出前に捨てていた)。
    ///
    /// 指標ごとの方向はカタログが決めるので、ここでは
    /// 「どちらの宣言が来ても経路が壊れないこと」だけを固定する。
    const fn entry_with_interest(interest: DeviationInterest) -> CatalogEntry {
        CatalogEntry {
            activity: ActivityId::QUEUE,
            column: "runq_sz",
            scope: ItemScope::Single,
            kind: ValueKind::Gauge,
            unit: Unit::None,
            label: "テスト用の系列",
            fixed: &[],
            deviation: DeviationInterest::Upper,
            shift: ShiftMagnitude::Absolute(4.0),
            shift_direction: interest,
            interpretations: &[],
            not_established: &[],
        }
    }

    static BOTH_WAYS: CatalogEntry = entry_with_interest(DeviationInterest::Both);
    static UPWARD_ONLY: CatalogEntry = entry_with_interest(DeviationInterest::Upper);
    static NO_INTEREST: CatalogEntry = entry_with_interest(DeviationInterest::None);

    fn run(t: crate::analyze::timeline::MetricTimeline) -> (Vec<Detection>, RouteStatus) {
        let entry = entry_for(&t);
        let series = PreparedSeries::from_timeline(&t);
        let opts = DetectOptions::default();
        let material = series.observations.clone();
        let baseline = build_baseline(&series, entry, material, &opts);
        super::detect(&series, entry, &baseline, &opts)
    }

    /// 一定値から一定値への明瞭な段差を拾う。
    ///
    /// 散らばりが 0 なので正規化はできない。**そこで落とさない**ことが要点。
    #[test]
    fn a_clean_step_is_detected_without_normalization() {
        let mut v = vec![90.0; 15];
        v.extend(vec![30.0; 15]);
        let (found, status) = run(cpu_idle(&vals(&v)));
        assert_eq!(found.len(), 1, "{found:#?}");
        let d = &found[0];
        assert_eq!(
            d.pattern,
            Pattern::LevelShift {
                direction: ShiftDirection::Fall
            }
        );
        assert!(matches!(status, RouteStatus::Detected { count: 1 }));
        let DecisionBasis::LevelShift {
            shift,
            normalized_shift,
            pooled_mad,
            persistence_share,
            ..
        } = d.decision.basis
        else {
            panic!("水準変化の根拠");
        };
        assert!((shift + 60.0).abs() < 1e-9);
        assert_eq!(pooled_mad, None, "一定値なので散らばりは測れない");
        assert_eq!(
            normalized_shift, None,
            "散らばりが測れないときは ε で割らず None"
        );
        assert!((persistence_share - 1.0).abs() < 1e-9);
    }

    /// 水準が違う 2 つの時間帯の**境目**を指す (裏付けは後窓)。
    ///
    /// 「その瞬間に変わった」ではない (モジュール doc を参照)。
    /// 指すのは採用した前後窓の分割時刻である。
    #[test]
    fn the_detection_points_at_the_boundary_between_the_two_windows() {
        let mut v = vec![90.0; 12];
        v.extend(vec![20.0; 12]);
        let (found, _) = run(cpu_idle(&vals(&v)));
        assert_eq!(found.len(), 1);
        // 13 点目 (索引 12) から水準が違う
        assert_eq!(found[0].support.start_ust, T0 + 12 * STEP_SECS);
    }

    /// 最小有意変化量に届かない小さな段差は出さない。
    #[test]
    fn a_small_step_below_the_declared_magnitude_is_rejected() {
        // %idle の最小有意変化量は 20 ポイント
        let mut v = vec![90.0; 15];
        v.extend(vec![80.0; 15]);
        let (found, _) = run(cpu_idle(&vals(&v)));
        assert!(found.is_empty(), "10 ポイントの段差は報告しない");
    }

    /// 揺れの大きい系列では正規化差の条件が効く。
    ///
    /// 窓幅 (5 点) と同じ周期の揺れにするのは、窓の中央値が揺れに
    /// 引きずられないようにするため。周期 2 の矩形波では窓の中央値自体が
    /// 交互に動いてしまい、手法の限界を試すだけのテストになる。
    #[test]
    fn a_step_buried_in_noise_is_rejected() {
        // 中央値 50 の周りを ±40 揺れる系列に 25 の段差を足す
        const NOISE: [f64; 5] = [10.0, 90.0, 50.0, 30.0, 70.0];
        let mut v: Vec<f64> = (0..20).map(|i| NOISE[i % 5]).collect();
        v.extend((0..20).map(|i| NOISE[i % 5] + 25.0));
        let (found, _) = run(runq(&vals(&v)));
        assert!(
            found.is_empty(),
            "揺れに埋もれた段差は水準変化として報告しない: {found:#?}"
        );
    }

    /// 同じ揺れでも段差が十分大きければ拾う (上の裏返し)。
    #[test]
    fn a_step_larger_than_the_noise_is_detected() {
        const NOISE: [f64; 5] = [10.0, 90.0, 50.0, 30.0, 70.0];
        let mut v: Vec<f64> = (0..20).map(|i| NOISE[i % 5]).collect();
        v.extend((0..20).map(|i| NOISE[i % 5] + 300.0));
        let (found, _) = run(runq(&vals(&v)));
        assert_eq!(found.len(), 1, "{found:#?}");
        let DecisionBasis::LevelShift {
            normalized_shift, ..
        } = found[0].decision.basis
        else {
            panic!();
        };
        assert!(normalized_shift.is_some_and(|n| n >= 3.0));
    }

    /// 単発のスパイクは水準変化にしない (持続性の条件)。
    #[test]
    fn a_single_spike_is_not_a_level_shift() {
        let mut v = vec![5.0; 40];
        v[20] = 500.0;
        let (found, _) = run(runq(&vals(&v)));
        assert!(found.is_empty(), "1 点だけの跳ねは水準変化ではない");
    }

    /// 不連続 (RESTART) を挟んだ前後は比べない。
    #[test]
    fn a_step_across_a_restart_is_not_compared() {
        let mut points = vals(&[90.0; 15]);
        points.push(P::Restart);
        points.extend(vals(&[20.0; 15]));
        let (found, status) = run(cpu_idle(&points));
        assert!(
            found.is_empty(),
            "再起動を挟んだ水準差を異変として報告してはいけない: {found:#?}"
        );
        // 連続区間が 15 点しかなく 2 窓 (各 5 点以上) を取れない場合もある
        assert!(matches!(
            status,
            RouteStatus::Evaluated | RouteStatus::NotEvaluated { .. }
        ));
    }

    /// 宣言した向きだけを見る。
    ///
    /// 見るのは [`CatalogEntry::shift_interest`] であり、**逸脱の宣言
    /// (`deviation`) ではない**。両者を同じ宣言で済ませていたとき、
    /// 処理量の指標では「止まった」が判定前に捨てられていた (Issue #5 の 20)。
    #[test]
    fn only_the_declared_direction_is_reported() {
        // runq-sz は圧力の指標。キューが短くなったのは負荷の緩和であって
        // 所見ではない (逸脱・水準変化ともに上方向だけを宣言している)
        let mut v = vec![30.0; 15];
        v.extend(vec![1.0; 15]);
        let (found, _) = run(runq(&vals(&v)));
        assert!(found.is_empty(), "runq-sz の低下は異変ではない: {found:#?}");
    }

    /// 処理量の指標では**水準の上昇も**報告する。
    ///
    /// `%idle` の逸脱は下方向だけを見る (低いほうが外れ値) が、
    /// 水準変化は両方向を宣言している。上昇は「負荷源の消失」であり、
    /// 処理が終わったのか障害で止まったのかはこの系列からは断定できない。
    /// **方向で弾くのではなく、水準が動いた事実として出して解釈を並べる。**
    #[test]
    fn a_rise_is_reported_for_metrics_that_declare_both_directions() {
        let mut v = vec![20.0; 15];
        v.extend(vec![90.0; 15]);
        let (found, _) = run(cpu_idle(&vals(&v)));
        assert_eq!(found.len(), 1, "{found:#?}");
        assert_eq!(
            found[0].pattern,
            Pattern::LevelShift {
                direction: ShiftDirection::Rise
            }
        );
    }

    /// 窓が取れない短い系列は評価しない。
    #[test]
    fn a_short_series_declines_the_route() {
        let (found, status) = run(cpu_idle(&vals(&[90.0, 90.0, 20.0, 20.0])));
        assert!(found.is_empty());
        assert!(matches!(
            status,
            RouteStatus::NotEvaluated {
                reason: NotEvaluated::NoWindowLongEnough
            }
        ));
    }

    /// 窓幅は採取間隔から決まる (サンプル数固定ではない)。
    #[test]
    fn the_window_is_sized_by_time_not_by_sample_count() {
        let obs: Vec<Observation> = (0..100)
            .map(|i| Observation {
                start_ust: T0 + i * 10,
                end_ust: T0 + (i + 1) * 10,
                elapsed_cs: 1000,
                value: 1.0,
                origin: ObservationOrigin::InstantGauge,
            })
            .collect();
        // 10 秒採取で 30 分窓 → 180 点
        assert_eq!(representative_interval(&obs), Some(10));
        assert_eq!(window_points(10, 1800, 5), 180);

        // 10 分採取で 30 分窓 → 3 点だが下限 5 で底を打つ
        assert_eq!(window_points(600, 1800, 5), 5);
    }

    /// 点数の下限で底を打った窓の**実際の幅**を根拠に載せる。
    ///
    /// 既定の 600 秒採取では `max(ceil(1800/600), 5) = 5` 点になり、
    /// 区間値では各窓 3000 秒である。「窓 1800 秒で判定した」と出さないため、
    /// 要求幅と実際の幅の両方を渡す。
    #[test]
    fn the_effective_window_width_is_recorded_next_to_the_requested_one() {
        let mut v = vec![90.0; 15];
        v.extend(vec![30.0; 15]);
        let (found, _) = run(cpu_idle(&vals(&v)));
        let DecisionBasis::LevelShift {
            window_samples,
            window_requested_secs,
            window_secs,
            before,
            after,
            ..
        } = found[0].decision.basis
        else {
            panic!("水準変化の根拠");
        };
        assert_eq!(window_samples, 5, "max(ceil(1800/600), 5) = 5 点");
        assert_eq!(window_requested_secs, 1800, "要求した幅");
        assert_eq!(window_secs, 3000, "実際の幅。要求の 1800 秒ではない");
        // 区間値なので実測の範囲も 5 区間ぶん
        assert_eq!(before.span_secs(), 3000);
        assert_eq!(after.span_secs(), 3000);
    }

    /// 瞬時値の窓幅は「点数 − 1」区間ぶん (採取の間は観測していない)。
    #[test]
    fn an_instant_gauge_window_is_one_interval_shorter() {
        assert_eq!(effective_window_secs(600, 5, false), 3000);
        assert_eq!(effective_window_secs(600, 5, true), 2400);

        let mut v = vec![1.0; 15];
        v.extend(vec![100.0; 15]);
        let (found, _) = run(runq(&vals(&v)));
        let DecisionBasis::LevelShift {
            window_secs,
            before,
            after,
            ..
        } = found[0].decision.basis
        else {
            panic!("水準変化の根拠");
        };
        assert_eq!(window_secs, 2400);
        assert_eq!(before.span_secs(), 2400, "実測の範囲と一致する");
        assert_eq!(after.span_secs(), 2400);
    }

    /// 欠測を挟んだ前後窓から水準変化を作らない。
    ///
    /// 低値 5 点 → 数時間の欠測 → 高値 5 点は、欠測を除いた配列の上では
    /// 隣接して見える。変化した時刻を特定できる材料ではないので、
    /// 「検出なし」ではなく**評価不能**にする。
    #[test]
    fn a_step_across_a_long_gap_is_not_evaluated() {
        let mut points = vals(&[90.0; 6]);
        // 3 時間ぶんの欠測。欠測は連続区間を切らない
        points.extend(std::iter::repeat_n(P::Missing, 18));
        points.extend(vals(&[20.0; 6]));
        let (found, status) = run(cpu_idle(&points));
        assert!(
            found.is_empty(),
            "欠測を挟んだ水準差を報告してはいけない: {found:#?}"
        );
        assert!(
            matches!(
                status,
                RouteStatus::NotEvaluated {
                    reason: NotEvaluated::NoWindowLongEnough
                }
            ),
            "評価不能として返す (検出なしではない): {status:?}"
        );
    }

    /// 欠測を含む窓だけを飛ばし、他の候補はそのまま評価する。
    #[test]
    fn only_the_candidates_containing_a_gap_are_skipped() {
        // 段差の手前に 1 点の欠測。段差自身は欠測を含まない窓で拾える
        let mut points = vals(&[90.0; 3]);
        points.push(P::Missing);
        points.extend(vals(&[90.0; 12]));
        points.extend(vals(&[20.0; 12]));
        let (found, _) = run(cpu_idle(&points));
        assert_eq!(found.len(), 1, "{found:#?}");
        assert_eq!(found[0].support.start_ust, T0 + 16 * STEP_SECS);
    }

    /// 滑らかな増加から架空の変化点を作らない。
    ///
    /// `runq_sz` が毎回 1 ずつ増えるだけの系列では、前窓 `[0,1,2,3,4]` /
    /// 後窓 `[5,6,7,8,9]` が差 5 (最小有意変化量 4 以上)・MAD 1・
    /// 正規化差 3.37 (閾値 3.0 以上)・持続率 80% (閾値 70% 以上) を
    /// すべて満たす。段差は存在しない。
    #[test]
    fn a_smooth_ramp_does_not_produce_a_change_point() {
        let v: Vec<f64> = (0..40).map(f64::from).collect();
        let (found, status) = run(runq(&vals(&v)));
        assert!(
            found.is_empty(),
            "傾向を特定時刻の段差として報告してはいけない: {found:#?}"
        );
        // 評価はできている (**評価不能ではない**)
        assert!(matches!(status, RouteStatus::Evaluated), "{status:?}");
    }

    /// 傾向の上に乗った段差は拾う (上の裏返し)。
    #[test]
    fn a_step_on_top_of_a_ramp_is_still_detected() {
        let mut v: Vec<f64> = (0..20).map(f64::from).collect();
        v.extend((20..40).map(|i| f64::from(i) + 50.0));
        let (found, _) = run(runq(&vals(&v)));
        assert_eq!(found.len(), 1, "{found:#?}");
        let DecisionBasis::LevelShift {
            shift,
            trend_explained_shift,
            step_shift,
            ..
        } = found[0].decision.basis
        else {
            panic!("水準変化の根拠");
        };
        assert!(
            (trend_explained_shift - 5.0).abs() < 1e-9,
            "傾き 1 × 窓 5 点: {trend_explained_shift}"
        );
        assert!((step_shift - 50.0).abs() < 1e-9, "{step_shift}");
        assert!((shift - 55.0).abs() < 1e-9, "{shift}");
        // 瞬時値なので変化後の最初の採取時刻を指す
        assert_eq!(found[0].support.start_ust, T0 + 21 * STEP_SECS);
    }

    /// 傾向は検出を減らす側にしか使わない。
    #[test]
    fn the_trend_never_enlarges_the_step() {
        // 傾向が逆向き / 過大でも、段差が観測差より大きくはならない
        assert!((step_of(10.0, -4.0) - 10.0).abs() < 1e-9);
        assert!((step_of(10.0, 4.0) - 6.0).abs() < 1e-9);
        assert_eq!(step_of(10.0, 20.0), 0.0, "傾向だけで説明できる");
        assert!((step_of(-10.0, 4.0) + 10.0).abs() < 1e-9);
        assert!((step_of(-10.0, -4.0) + 6.0).abs() < 1e-9);
    }

    /// 傾きは**実時間**に対して測る。
    ///
    /// 採取間隔が窓の中で変わると、点数あたりの傾きは実際の変化率と
    /// 一致しない。時間に対して一定の率で増える系列を、前半は短い間隔・
    /// 後半は長い間隔で採取した場合、点数あたりでは傾向を過小に見積もり
    /// 段差が残る。
    #[test]
    fn the_trend_is_measured_against_real_time() {
        // 1 秒あたり 1 ずつ増える系列を、10 秒間隔 5 点 + 100 秒間隔 5 点で採取
        let mut t = 0u64;
        let mut obs: Vec<Observation> = Vec::new();
        for i in 0..10 {
            let step = if i < 5 { 10 } else { 100 };
            let start = t;
            t += step;
            obs.push(Observation {
                start_ust: T0 + start,
                end_ust: T0 + t,
                elapsed_cs: step * 100,
                value: t as f64,
                origin: ObservationOrigin::InstantGauge,
            });
        }
        let trend = trend_explained_shift(&obs[..5], &obs[5..]);
        let shift = median(&obs[5..].iter().map(|o| o.value).collect::<Vec<_>>()).unwrap()
            - median(&obs[..5].iter().map(|o| o.value).collect::<Vec<_>>()).unwrap();
        // 傾向がそのまま差を説明する → 段差は残らない
        assert!(
            step_of(shift, trend).abs() < 0.5 * shift.abs(),
            "実時間で測れば傾向が差を説明する: shift={shift} trend={trend}"
        );
    }

    /// 窓が 4 点未満なら傾きを推定しない (傾向による棄却をしない)。
    #[test]
    fn a_tiny_window_does_not_estimate_a_trend() {
        let obs: Vec<Observation> = (0..6u32)
            .map(|i| Observation {
                start_ust: T0 + u64::from(i) * 600,
                end_ust: T0 + u64::from(i + 1) * 600,
                elapsed_cs: 60_000,
                value: f64::from(i),
                origin: ObservationOrigin::InstantGauge,
            })
            .collect();
        assert_eq!(trend_explained_shift(&obs[..3], &obs[3..]), 0.0);
    }

    /// 3 点だけの大変動は持続率に届かない。
    ///
    /// 「`MAD = 0` の系列の短い大変動」を水準変化で拾えない理由。
    /// 受け皿は逸脱経路側 (`robust::absolute_departures`) に置いてある。
    #[test]
    fn a_three_sample_burst_does_not_reach_the_persistence_share() {
        let mut v = vec![0.0; 40];
        for x in v.iter_mut().skip(20).take(3) {
            *x = 100.0;
        }
        let (found, status) = run(runq(&vals(&v)));
        assert!(found.is_empty(), "後窓 5 点のうち 3 点では 0.7 に届かない");
        assert!(matches!(status, RouteStatus::Evaluated), "{status:?}");
    }

    /// 方向の宣言を差し替えても壊れない。
    ///
    /// 指標ごとの方向はカタログが決める (Issue #5 の 20)。
    /// ここでは**両方向の宣言が来ても経路が通ること**を固定する。
    #[test]
    fn the_declared_direction_decides_which_shifts_are_reported() {
        assert!(accepts(&BOTH_WAYS, ShiftDirection::Rise));
        assert!(accepts(&BOTH_WAYS, ShiftDirection::Fall));
        assert!(accepts(&UPWARD_ONLY, ShiftDirection::Rise));
        assert!(!accepts(&UPWARD_ONLY, ShiftDirection::Fall));
        // 宣言が無ければ方向で捨てない (水準が動いた事実は出す)
        assert!(accepts(&NO_INTEREST, ShiftDirection::Rise));
        assert!(accepts(&NO_INTEREST, ShiftDirection::Fall));
    }

    /// 下方向の水準変化が経路の端まで通る。
    ///
    /// 処理量の指標では**停止も所見**である。両方向の宣言で
    /// `ShiftDirection::Fall` が出力まで届くことを固定する。
    #[test]
    fn a_downward_shift_is_reported_when_both_directions_are_declared() {
        let mut v = vec![500.0; 15];
        v.extend(vec![0.0; 15]);
        let t = timeline(
            MetricKey::new(ActivityId::QUEUE, SINGLE_ITEM, "runq_sz"),
            Unit::None,
            ValueKind::Gauge,
            &vals(&v),
        );
        let series = PreparedSeries::from_timeline(&t);
        let opts = DetectOptions::default();
        let baseline = build_baseline(&series, &BOTH_WAYS, series.observations.clone(), &opts);
        let (found, status) = super::detect(&series, &BOTH_WAYS, &baseline, &opts);

        assert_eq!(found.len(), 1, "{found:#?}");
        assert_eq!(
            found[0].pattern,
            Pattern::LevelShift {
                direction: ShiftDirection::Fall
            }
        );
        assert!(matches!(status, RouteStatus::Detected { count: 1 }));
        let DecisionBasis::LevelShift { shift, .. } = found[0].decision.basis else {
            panic!("水準変化の根拠");
        };
        assert!(shift < 0.0, "低下として報告する");
    }

    /// 上方向だけの宣言では低下を報告しない (上の裏返し)。
    #[test]
    fn a_downward_shift_is_dropped_when_only_the_rise_is_declared() {
        let mut v = vec![500.0; 15];
        v.extend(vec![0.0; 15]);
        let t = timeline(
            MetricKey::new(ActivityId::QUEUE, SINGLE_ITEM, "runq_sz"),
            Unit::None,
            ValueKind::Gauge,
            &vals(&v),
        );
        let series = PreparedSeries::from_timeline(&t);
        let opts = DetectOptions::default();
        let baseline = build_baseline(&series, &UPWARD_ONLY, series.observations.clone(), &opts);
        let (found, _) = super::detect(&series, &UPWARD_ONLY, &baseline, &opts);
        assert!(found.is_empty(), "{found:#?}");
    }

    /// 同じ段差を窓ごとに何度も報告しない。
    #[test]
    fn adjacent_candidates_collapse_into_one() {
        let mut v = vec![90.0; 25];
        v.extend(vec![20.0; 25]);
        let (found, _) = run(cpu_idle(&vals(&v)));
        assert_eq!(found.len(), 1, "1 つの段差は 1 件: {found:#?}");
    }

    /// 閾値が出力に載る。
    #[test]
    fn thresholds_are_recorded_in_the_output() {
        let mut v = vec![90.0; 15];
        v.extend(vec![30.0; 15]);
        let (found, _) = run(cpu_idle(&vals(&v)));
        let DecisionBasis::LevelShift {
            normalized_threshold,
            persistence_threshold,
            ..
        } = found[0].decision.basis
        else {
            panic!();
        };
        assert_eq!(normalized_threshold, 3.0);
        assert!((persistence_threshold - 0.7).abs() < 1e-9);
    }
}

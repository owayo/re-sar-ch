//! `sadf -r` (raw) の出力。
//!
//! オンディスクの生カウンタをそのまま出す。レート変換をしないのが基本で、
//! 単調増加カウンタは `名前; 前値; 現値;` の 3 トークンになる (§4)。
//!
//! ```text
//! 13:20:19 UTC; CPU; -1; %usr; 96005; 96538; %nice; 2578701; 2581805; …
//! 13:20:19 UTC; proc/s; 46972; 47083; cswch/s; 130465866; 132598184;
//! ```
//!
//! - 区切りは `"; "` (セミコロン + 空白)、行末は `";"` + 改行。
//! - アイテムを持たない activity はアイテム識別子フィールドが無い。
//! - **オフライン CPU も必ず出す** (他形式は除外する、§11.3)。
//!   `A_CPU` と `A_NET_SOFT` は `nr_ini` (採取時の CPU 数) まで回し、
//!   そのレコードに無い CPU は 0 の値で出す (`raw_print_cpu_stats()`)。
//! - `hdr_line` に無い直書きフィールド名が多数ある (§14.5-3)。
//!
//! 表示ループは `logic2` = **activity 順** (エンジンは `logic2` モジュール)。
//!
//! # 文字列フィールド
//!
//! 1 item に文字列フィールドが複数ある activity (`A_PWR_USB` の
//! `manufact` / `product`、`A_FS` の `fs_name` / `mountp`) も
//! `ItemSnapshot::texts` からすべて引ける。
//! その世代のファイルに無いフィールドだけが空になる
//! (例: 最古の `A_FS` は `mountp` を持たない)。
//!
//! `A_DISK` のデバイス名だけはファイルに入っていないため、
//! 本家 `get_devname()` の最終フォールバックと同じ `dev<major>-<minor>` を
//! 組み立てる (§2.8.1)。ローカルの `/sys` は引かない — 他ホストで採取した
//! ファイルでは別デバイスの名前が出てしまう。
//!
//! # `-O debug` (§4.4)
//!
//! | 追加されるもの | 本家 |
//! |---|---|
//! | レコードヘッダを読むたびに `# uptime_cs; …` 行 (基準レコード・RESTART・COMMENT・拡張レコードも) | `read_record_hdr()` |
//! | 表示するレコードごとに `# name; <activity>; nr_curr; …` 行 (`nr_curr` はそのレコードの item 数) | `generic_write_stats()` |
//! | 前値より減ったカウンタ名の直後に ` [DEC]` | `pval()` |
//! | 個別 CPU の名前の直後に ` [OFF]` (tick 和が 0) / ` [TLS]` (tick 差分が 0) | `raw_print_cpu_stats()` |
//! | 前サンプルに無いデバイスの名前の直後に ` [NEW]` / 再登録なら ` [BCK]` | `raw_print_disk_stats()` ほか |

use std::io::{self, Write};

use super::access::{ActivityPair, ItemPair};
use super::dbppc::{activity_passes, display_cpu_count, selected_specs};
use super::logic2::{self, Logic2Sink, Pass, Shown};
use super::records::{Rec, RecHeader};
use super::render::{item_label_in, raw_pair};
use super::spec::{ActivitySpec, FieldGate, ItemKind, RawField, RawSpec, RawStyle, Section};
use super::{
    ABSENT_TEXT, FileInfo, ItemLabel, SadfConfig, SadfExtra, Stamp, double_from_bits, render, spec,
    write_sensor,
};
use crate::error::Result;
use crate::format::file::SaFile;
use crate::model::{ActivityId, Availability};
use crate::series::compute::{self, ComputeContext, CpuRole};
use crate::series::{IntervalView, ItemSnapshot};

/// `-r` の出力。
pub fn write_raw<W: Write>(out: &mut W, file: &SaFile, cfg: &SadfConfig) -> Result<()> {
    write_raw_with(out, file, cfg, &SadfExtra::default())
}

/// `-r` の出力 (`interval` / `count` と `-T` の TZ 名を指定する)。
pub fn write_raw_with<W: Write>(
    out: &mut W,
    file: &SaFile,
    cfg: &SadfConfig,
    extra: &SadfExtra,
) -> Result<()> {
    let info = FileInfo::from_config(file, cfg, extra);
    let specs = selected_specs(file, cfg);
    let ids: Vec<ActivityId> = specs.iter().map(|s| s.id).collect();
    let passes = activity_passes(&specs, cfg);
    let mut sink = RawSink { out, cfg, info };
    logic2::run(file, cfg, extra.select, &passes, &ids, cfg.debug, &mut sink)
}

/// `-r` の書き出し。
struct RawSink<'w, 'c, W: Write> {
    out: &'w mut W,
    cfg: &'c SadfConfig,
    info: FileInfo,
}

impl<W: Write> Logic2Sink for RawSink<'_, '_, W> {
    /// RESTART 行。nodename も interval も出さず、`;` の後に空白 1 個 (§1.3)。
    fn restart(&mut self, rec: &Rec, cpu_nr: Option<u32>) -> io::Result<()> {
        let stamp = Stamp::new(self.cfg.time_base, rec.ust_time, rec.hms, &self.info);
        writeln!(
            self.out,
            "{}; LINUX-RESTART ({} CPU)",
            stamp.raw_event(),
            display_cpu_count(cpu_nr)
        )
    }

    /// COMMENT の 1 行 (`<時刻>; COM <本文>`)。
    fn comment(&mut self, rec: &Rec) -> io::Result<()> {
        let stamp = Stamp::new(self.cfg.time_base, rec.ust_time, rec.hms, &self.info);
        writeln!(
            self.out,
            "{}; COM {}",
            stamp.raw_event(),
            rec.comment.as_deref().unwrap_or("")
        )
    }

    /// raw はフィールド名一覧行を持たない (`FO_FIELD_LIST` は `-d` だけ)。
    fn begin_pass(&mut self, _pass: &Pass) -> io::Result<()> {
        Ok(())
    }

    fn sample(&mut self, pass: &Pass, shown: &Shown<'_, '_>) -> io::Result<()> {
        let Pass::Activity { spec, section } = pass else {
            return Ok(());
        };
        let view = shown.view;
        let nr_curr = view.curr.activity(spec.id).map_or(0, |a| a.nr);
        if self.cfg.debug {
            writeln!(
                self.out,
                "# name; {}; nr_curr; {}; nr_alloc; {}; nr_ini; {}",
                spec.name, nr_curr, shown.nr_alloc, shown.nr_ini
            )?;
        }
        // `IS_SELECTED && nr[curr] > 0` のときだけ本体を出す
        if nr_curr == 0 {
            return Ok(());
        }
        let stamp = Stamp::new(
            self.cfg.time_base,
            view.curr.ust_time,
            (view.curr.hour, view.curr.minute, view.curr.second),
            &self.info,
        );
        let ts = stamp.raw();
        match spec.id {
            ActivityId::IRQ => write_irq(self.out, view, &ts, self.cfg),
            ActivityId::PWR_FREQ => write_wghfreq(self.out, view, &ts, self.cfg),
            ActivityId::CPU | ActivityId::NET_SOFT => {
                // 表示関数は `nr[curr] > nr_ini` なら先に `nr_ini` を引き上げる
                let nr_ini = shown.nr_ini.max(nr_curr);
                write_per_cpu(self.out, view, &ts, self.cfg, spec, section, nr_ini)
            }
            _ => write_generic(self.out, view, &ts, self.cfg, spec, section),
        }
    }

    /// `# uptime_cs; …` 行 (`read_record_hdr()` の debug 出力)。
    ///
    /// `extra_next` は索引が持たないので 0 を出す (拡張構造を持つファイルでだけ
    /// 本家と食い違う)。
    fn header(&mut self, h: &RecHeader) -> io::Result<()> {
        writeln!(
            self.out,
            "# uptime_cs; {}; ust_time; {}; extra_next; 0; record_type; {}; HH:MM:SS; {:02}:{:02}:{:02}",
            h.uptime_cs, h.ust_time, h.record_type, h.hms.0, h.hms.1, h.hms.2
        )
    }
}

// ===========================================================================
// 汎用経路
// ===========================================================================

fn write_generic<W: Write>(
    out: &mut W,
    view: &IntervalView<'_>,
    ts: &str,
    cfg: &SadfConfig,
    spec: &ActivitySpec,
    section: &Section,
) -> io::Result<()> {
    let Some(pair) = ActivityPair::from_view(view, spec.id) else {
        return Ok(());
    };
    let fields = raw_fields(section, cfg);

    // A_DISK だけは直書きの major / minor が hdr_line のアイテムラベルより
    // **前**に出る (§4.5)。ラベルを挟む位置をここで決める。
    let label_at = if spec.id == ActivityId::DISK { 2 } else { 0 };

    for item in pair.selected_items(cfg, true) {
        let mut tok = vec![ts.to_string()];
        // 前サンプルに無い回線は回線番号までで行を閉じる (`raw_print_serial_stats()`)
        if pair.is_new_serial_line(&item) {
            push_item_label(&mut tok, spec, section, &item, cfg);
            out.write_all(join_tokens(&tok).as_bytes())?;
            continue;
        }
        for (i, f) in fields.iter().enumerate() {
            if i == label_at {
                push_item_label(&mut tok, spec, section, &item, cfg);
            }
            push_raw_field(&mut tok, spec, &item, f, cfg);
        }
        if fields.len() <= label_at {
            push_item_label(&mut tok, spec, section, &item, cfg);
        }
        out.write_all(join_tokens(&tok).as_bytes())?;
    }
    Ok(())
}

/// トークンを `"; "` でつないで行にする。
///
/// raw の 1 行は `<timestr>; <名前>; <値>; …;` の形で、**行末に `;` が付く**
/// (§4.1)。区切りが一様なのでトークン列として組み立てるのが安全
/// (`;;` の二重出力のような取り違えが起きない)。
fn join_tokens(tokens: &[String]) -> String {
    let mut s = tokens.join("; ");
    s.push_str(";\n");
    s
}

/// アイテム識別子を `; <ラベル>; <値>` の形で足す。
///
/// `A_PWR_USB` は raw ではアイテムを持たない (§4.5)。
/// `A_FS` のデバイス名だけダブルクォートが付く (§14.5-5)。
fn push_item_label(
    tok: &mut Vec<String>,
    spec: &ActivitySpec,
    section: &Section,
    item: &ItemPair<'_>,
    cfg: &SadfConfig,
) {
    if spec.item == ItemKind::None || spec.id == ActivityId::PWR_USB {
        return;
    }
    // 先頭のフィールド名はそのセクションの `hdr_line` から取る
    // (`A_FS` は `-F MOUNT` で `FILESYSTEM` → `MOUNTPOINT` に変わる)。
    let head = section.hdr_line.split(';').next().unwrap_or_default();
    let label = item_label_in(spec, section, item);

    // -O debug では前サンプルに相手が居ないデバイスに印を付ける (§4.4)
    let mark = if cfg.debug {
        registration_mark(spec, item)
    } else {
        ""
    };
    tok.push(format!("{head}{mark}"));
    // A_FS のデバイス名だけダブルクォートが付く (§14.5-5)
    if spec.id == ActivityId::FS {
        tok.push(format!("\"{}\"", label.db));
    } else {
        tok.push(label.db);
    }
}

/// `-O debug` の登録状態の印 (`check_*_reg()` の戻り値)。
///
/// | 状態 | 印 | 対象 |
/// |---|---|---|
/// | 前サンプルに相手が居ない (`-1`) | ` [NEW]` | disk / net-dev / net-edev / fchost / serial |
/// | 相手は居るが全カウンタが減った = 再登録 (`-2`) | ` [BCK]` | disk / net-dev / net-edev |
fn registration_mark(spec: &ActivitySpec, item: &ItemPair<'_>) -> &'static str {
    let tracked = matches!(
        spec.id,
        ActivityId::DISK
            | ActivityId::NET_DEV
            | ActivityId::NET_EDEV
            | ActivityId::NET_FC
            | ActivityId::SERIAL
    );
    if !tracked {
        return "";
    }
    // 相手が居たかは文脈の `has_prev` に出る (区間の前サンプルはある前提)
    if !item.ctx.has_prev {
        return " [NEW]";
    }
    let reregistered = item.prepared.as_ref().is_some_and(|p| p.replaced);
    if reregistered
        && matches!(
            spec.id,
            ActivityId::DISK | ActivityId::NET_DEV | ActivityId::NET_EDEV
        )
    {
        " [BCK]"
    } else {
        ""
    }
}

fn push_raw_field(
    tok: &mut Vec<String>,
    spec: &ActivitySpec,
    item: &ItemPair<'_>,
    f: &RawField,
    cfg: &SadfConfig,
) {
    match f.style {
        RawStyle::Pval | RawStyle::Pair | RawStyle::PvalSum(_) | RawStyle::PvalDiff(_, _) => {
            let (prev, curr) = raw_pair(item, f);
            // -O debug ではカウンタが減少したフィールド名の直後に [DEC] (§4.4-3)。
            // `pval()` を通らない `Pair` には付かない。
            let dec = cfg.debug && !matches!(f.style, RawStyle::Pair) && is_decrease(prev, curr);
            tok.push(if dec {
                format!("{} [DEC]", f.name)
            } else {
                f.name.to_string()
            });
            tok.push(u64_token(prev));
            tok.push(u64_token(curr));
        }
        RawStyle::Int => {
            let v = item.raw_curr_by_name(f.col);
            tok.push(f.name.to_string());
            // A_PWR_BAT の status は値の後に名前付きの注記が入る (§4.4-5)
            if cfg.debug
                && spec.id == ActivityId::PWR_BAT
                && f.col == "status"
                && let Availability::Present(sts) = v
            {
                tok.push(format!("{} [{}]", u64_token(v), render::bat_status(sts)));
                return;
            }
            tok.push(u64_token(v));
        }
        RawStyle::IntCompat => {
            tok.push(f.name.to_string());
            tok.push(match item.computed_by_name(f.col) {
                Ok(v) => super::truncate_u64(v).to_string(),
                Err(e) if super::missing_kind(e) == Some(super::MissingKind::ZeroFilled) => {
                    "0".to_string()
                }
                Err(_) => ABSENT_TEXT.to_string(),
            });
        }
        RawStyle::Sensor => {
            tok.push(f.name.to_string());
            tok.push(match item.raw_curr_by_name(f.col) {
                Availability::Present(bits) => {
                    let mut s = String::new();
                    write_sensor(&mut s, double_from_bits(bits));
                    s
                }
                _ => ABSENT_TEXT.to_string(),
            });
        }
        RawStyle::Text | RawStyle::QuotedText => {
            tok.push(f.name.to_string());
            let text = render::field_text(spec, item, f.col).unwrap_or_default();
            tok.push(if matches!(f.style, RawStyle::QuotedText) {
                format!("\"{text}\"")
            } else {
                text
            });
        }
        RawStyle::Hex => {
            tok.push(f.name.to_string());
            tok.push(match item.raw_curr_by_name(f.col) {
                Availability::Present(v) => format!("{v:x}"),
                _ => ABSENT_TEXT.to_string(),
            });
        }
    }
}

/// `pval()` の ` [DEC]` 判定 (現値が前値より小さい)。
///
/// 本家は 0 埋めした構造体どうしを比べるので、その世代に無いフィールドは
/// 0 として比べる (`u64_token` と同じ扱い)。観測できていない値は比べない。
fn is_decrease(prev: Availability<u64>, curr: Availability<u64>) -> bool {
    let v = |a: Availability<u64>| match a {
        Availability::Present(x) => Some(x),
        Availability::UnsupportedBySource => Some(0),
        Availability::MissingInSample => None,
    };
    matches!((v(prev), v(curr)), (Some(p), Some(c)) if c < p)
}

/// 生値 1 個のトークン。
///
/// **欠落の 2 種類を区別する** (指摘 8 と同じ規則)。
///
/// | 欠落 | 出力 | 理由 |
/// |---|---|---|
/// | `UnsupportedBySource` (その世代にフィールドが無い) | `0` | 本家は「期待する型別本数よりファイル側が少なければ足りない分を 0 埋め」した構造体を読むので、`pval()` は `0` を出す (03 §1.9-1) |
/// | `MissingInSample` (フィールドはあるがこのレコードで読めていない) | 空文字 | 本家ならその行自体が無い。0 を書くと観測値と区別が付かなくなる |
///
/// `-d` / `-p` / `-j` / `-x` は [`super::write_value`] が同じ区別をする。
/// ここだけ空文字にすると**互換出力どうしで不統一**になる
/// (旧 `A_IO` の `dtps` が `-d` では `0.00`、`-r` では空欄になっていた)。
fn u64_token(v: Availability<u64>) -> String {
    match v {
        Availability::Present(x) => x.to_string(),
        Availability::UnsupportedBySource => "0".to_string(),
        Availability::MissingInSample => ABSENT_TEXT.to_string(),
    }
}

/// `RawSpec` を実フィールド列へ展開する (`-r ALL` でだけ出るフィールドの判定込み)。
///
/// [`RawSpec::AllPval`] は `-d`/`-p` のフィールド名をそのまま使い、全部 `pval`
/// にする (アイテムを持たないカウンタ系、§4.5)。
fn raw_fields(section: &Section, cfg: &SadfConfig) -> Vec<RawField> {
    match section.raw {
        RawSpec::Fields(list) => list
            .iter()
            .filter(|f| cfg.section.allows_field(f.gate))
            .copied()
            .collect(),
        RawSpec::AllPval => section
            .fields
            .iter()
            .filter(|f| !f.pp.is_empty() && !f.col.is_empty())
            .map(|f| RawField {
                col: f.col,
                name: f.pp,
                style: RawStyle::Pval,
                gate: FieldGate::Always,
            })
            .collect(),
    }
}

// ===========================================================================
// A_CPU / A_NET_SOFT (CPU ごと、オフラインも出す)
// ===========================================================================

/// `A_CPU` / `A_NET_SOFT` を CPU ごとに出す (`raw_print_cpu_stats()` /
/// `raw_print_softnet_stats()`)。
///
/// 本家は `nr_ini` (採取時の CPU 数。RESTART で置き換わる) まで回し、
/// そのレコードに無い CPU は 0 埋めされたバッファの値を出す
/// (`AO_PERSISTENT` の activity は読むたびに `nr_ini` 分を 0 で消してから読む)。
/// 前値も同じで、前のレコードに無かった CPU は 0 になる。
/// `A_NET_SOFT` はファイルに無い CPU "all" を出さず 1 から始める。
///
/// `-O debug` の印 (個別 CPU のみ):
///
/// - `A_CPU`: tick 8 フィールドの和が 0 なら ` [OFF]`、そうでなく
///   `get_per_cpu_interval()` が 0 なら ` [TLS]`。後者は本家が前値の
///   `iowait` / `idle` を補正して書き戻すので、**表示する前値も補正後**になる。
/// - `A_NET_SOFT`: 6 つのカウンタがすべて 0 なら ` [OFF]`。
fn write_per_cpu<W: Write>(
    out: &mut W,
    view: &IntervalView<'_>,
    ts: &str,
    cfg: &SadfConfig,
    spec: &ActivitySpec,
    section: &Section,
    nr_ini: u32,
) -> io::Result<()> {
    let (Some(plan), Some(def), Some(curr_act)) = (
        view.plan_for(spec.id),
        crate::layout::registry::lookup(spec.id),
        view.curr.activity(spec.id),
    ) else {
        return Ok(());
    };
    let prev_items: &[ItemSnapshot] = view
        .prev
        .activity(spec.id)
        .map_or(&[], |a| a.items.as_slice());
    let zero = compute::zero_item(plan);
    let fields = raw_fields(section, cfg);
    let head = section.hdr_line.split(';').next().unwrap_or_default();
    let start = usize::from(spec.id == ActivityId::NET_SOFT);
    let n = (nr_ini as usize).max(curr_act.items.len());

    for i in start..n {
        if !cfg.cpus.includes(i) {
            continue;
        }
        let curr = curr_act.items.get(i).unwrap_or(&zero);
        let prev_raw = prev_items.get(i).unwrap_or(&zero);
        let mut mark = "";
        let fixed;
        let prev: &ItemSnapshot = if cfg.debug && i > 0 {
            match spec.id {
                ActivityId::CPU => {
                    let iv = compute::cpu_interval(plan, prev_raw, curr, CpuRole::Single);
                    if iv.is_offline() {
                        mark = " [OFF]";
                        prev_raw
                    } else {
                        if iv.is_tickless() {
                            mark = " [TLS]";
                        }
                        fixed = iv.prev;
                        &fixed
                    }
                }
                _ => {
                    if compute::is_unused_item(spec.id, i, plan, curr) {
                        mark = " [OFF]";
                    }
                    prev_raw
                }
            }
        } else {
            prev_raw
        };
        let mut ctx = ComputeContext::new(view.itv_cs);
        ctx.has_prev = true;
        ctx.continuous = true;
        let item = ItemPair::single(i, def, plan, prev, curr, ctx);
        let mut tok = vec![
            ts.to_string(),
            format!("{head}{mark}"),
            ItemLabel::cpu(i).db,
        ];
        for f in &fields {
            push_raw_field(&mut tok, spec, &item, f, cfg);
        }
        out.write_all(join_tokens(&tok).as_bytes())?;
    }
    Ok(())
}

// ===========================================================================
// A_IRQ / A_PWR_FREQ の専用経路
// ===========================================================================

/// `A_IRQ` はアイテムが割り込み名で、フィールド名の位置に CPU ラベルが入る。
///
/// フィールド名は `all` (CPU 0) / `CPU0` / `CPU1` … (§4.5)。
/// raw ではオフライン CPU も出す。
fn write_irq<W: Write>(
    out: &mut W,
    view: &IntervalView<'_>,
    ts: &str,
    cfg: &SadfConfig,
) -> io::Result<()> {
    let Some(pair) = ActivityPair::from_view(view, ActivityId::IRQ) else {
        return Ok(());
    };
    let (nr, nr2) = pair.irq_dimensions();

    for irq in 0..nr2 {
        if !(0..nr).any(|cpu| pair.irq_cpu_selected(cfg, cpu, true)) {
            continue;
        }
        // 割り込み名は CPU "all" 行 (行 0) にのみ書かれている
        let name = pair.irq_name(irq);
        if !cfg.name_selected(ActivityId::IRQ, &name) {
            continue;
        }

        let mut tok = vec![ts.to_string(), "INTR".to_string(), name];
        for cpu in 0..nr {
            if !pair.irq_cpu_selected(cfg, cpu, true) {
                continue;
            }
            // フィールド名は `all` (CPU 0) / `CPU0` / `CPU1` … (§4.5)
            let name = if cpu == 0 {
                "all".to_string()
            } else {
                format!("CPU{}", cpu - 1)
            };
            let (prev, curr) = match pair.irq_item(cpu, irq) {
                Some(item) => (item.raw_prev_by_name("intr"), item.raw_curr_by_name("intr")),
                None => (Availability::MissingInSample, Availability::MissingInSample),
            };
            // 値は `pval()` で出るので、-O debug の [DEC] もここに付く
            tok.push(if cfg.debug && is_decrease(prev, curr) {
                format!("{name} [DEC]")
            } else {
                name
            });
            tok.push(u64_token(prev));
            tok.push(u64_token(curr));
        }
        out.write_all(join_tokens(&tok).as_bytes())?;
    }
    Ok(())
}

/// `A_PWR_FREQ` は `hdr_line` を使わず直書きの `freq` / `tminst` を
/// 周波数ステップぶん (`nr2` 個、`freq == 0` で打ち切り) 繰り返す (§4.5)。
fn write_wghfreq<W: Write>(
    out: &mut W,
    view: &IntervalView<'_>,
    ts: &str,
    cfg: &SadfConfig,
) -> io::Result<()> {
    let Some(pair) = ActivityPair::from_view(view, ActivityId::PWR_FREQ) else {
        return Ok(());
    };
    let nr = pair.curr.nr as usize;
    let nr2 = pair.curr.nr2.max(1) as usize;
    let spec = pair_spec();

    for row in 0..nr {
        if !cfg.cpus.includes(row) {
            continue;
        }
        let mut tok = vec![ts.to_string(), "CPU".to_string(), ItemLabel::cpu(row).db];
        for step in 0..nr2 {
            let Some(item) = pair.item(row * nr2 + step) else {
                break;
            };
            let freq = item.raw_curr_by_name("freq_khz");
            if matches!(freq, Availability::Present(0)) {
                break;
            }
            tok.push("freq".to_string());
            tok.push(u64_token(freq));
            push_raw_field(
                &mut tok,
                spec,
                &item,
                &RawField {
                    col: "time_in_state",
                    name: "tminst",
                    style: RawStyle::Pval,
                    gate: FieldGate::Always,
                },
                cfg,
            );
        }
        out.write_all(join_tokens(&tok).as_bytes())?;
    }
    Ok(())
}

fn pair_spec() -> &'static ActivitySpec {
    spec::lookup(ActivityId::PWR_FREQ).expect("A_PWR_FREQ の出力定義")
}

#[cfg(test)]
mod tests {
    use super::super::spec::SectionConfig;
    use super::*;

    /// `AllPval` はフィールド名を `-d`/`-p` から取り、全部 `pval` にする。
    #[test]
    fn all_pval_expands_from_dp_fields() {
        let pcsw = spec::lookup(ActivityId::PCSW).unwrap();
        let fields = raw_fields(&pcsw.sections[0], &SadfConfig::default());
        let names: Vec<_> = fields.iter().map(|f| f.name).collect();
        assert_eq!(names, vec!["proc/s", "cswch/s"]);
        assert!(fields.iter().all(|f| f.style == RawStyle::Pval));
    }

    /// raw のセンサ系フィールド名は `hdr_line` と一致しない (§4.5 の落とし穴)。
    #[test]
    fn sensor_raw_names_differ_from_hdr_line() {
        let fan = spec::lookup(ActivityId::PWR_FAN).unwrap();
        let fields = raw_fields(&fan.sections[0], &SadfConfig::default());
        let names: Vec<_> = fields.iter().map(|f| f.name).collect();
        // hdr_line は FAN;DEVICE;rpm;drpm だが raw は drpm の代わりに rpm_min を出す
        assert_eq!(names, vec!["DEVICE", "rpm", "rpm_min"]);
        assert!(fan.sections[0].hdr_line.contains("drpm"));
    }

    /// A_MEMORY の raw には `hdr_line` に無い `kbttlmem` が入り、派生値は出ない。
    #[test]
    fn memory_raw_uses_hardcoded_total_name() {
        let mem = spec::lookup(ActivityId::MEMORY).unwrap();
        let fields = raw_fields(&mem.sections[0], &SadfConfig::default());
        let names: Vec<_> = fields.iter().map(|f| f.name).collect();
        assert!(names.contains(&"kbttlmem"));
        assert!(!names.contains(&"kbmemused"));
        assert!(!names.contains(&"%memused"));
    }

    /// **回帰テスト**: `kbanonpg` 以降は `-r ALL` のときだけ出る
    /// (`raw_print_ram_memory_stats()` の `dispall`)。以前は `-r` でも出ていた。
    #[test]
    fn memory_raw_detail_fields_need_r_all() {
        let mem = spec::lookup(ActivityId::MEMORY).unwrap();
        let plain = SadfConfig {
            section: SectionConfig {
                mem_all: false,
                ..SectionConfig::default()
            },
            ..SadfConfig::default()
        };
        let names: Vec<_> = raw_fields(&mem.sections[0], &plain)
            .iter()
            .map(|f| f.name)
            .collect();
        assert_eq!(names.last(), Some(&"kbshmem"), "{names:?}");
        assert!(!names.contains(&"kbanonpg"));

        let all: Vec<_> = raw_fields(&mem.sections[0], &SadfConfig::default())
            .iter()
            .map(|f| f.name)
            .collect();
        assert_eq!(all.last(), Some(&"kbvmused"), "{all:?}");
    }

    /// **回帰テスト (バグ 10)**: `availablekb` を持たない旧世代の `kbavail` は
    /// 本家の変換 (`availablekb = frmkb`) と同じく `kbmemfree` の値になる。
    /// 以前はその世代に無いフィールドとして `0` を出していた。
    #[test]
    fn kbavail_falls_back_to_kbmemfree_on_old_generations() {
        use crate::layout::plan::DecodePlan;
        use crate::model::ActivityId;

        let def = crate::layout::registry::lookup(ActivityId::MEMORY).unwrap();
        let col = |name: &str| {
            def.columns
                .iter()
                .position(|c| c.public_name == name)
                .unwrap()
        };
        let enc = crate::format::abi::SourceEncoding::new(
            crate::format::abi::Endian::Little,
            crate::format::abi::LayoutAbi::LP64,
        );
        // `availablekb` を持たない revision の計画
        let plan = def
            .revisions
            .iter()
            .filter_map(|rev| DecodePlan::build(def, rev, rev.size_lp64, 1, 1, &enc).ok())
            .find(|plan| plan.column_fields[col("kbavail")].is_none())
            .expect("availablekb を持たない revision がある");

        let mut curr = compute::zero_item(&plan);
        let free = plan.column_fields[col("kbmemfree")]
            .as_ref()
            .expect("kbmemfree はある")
            .index();
        curr.values[free] = Availability::Present(1234);
        let prev = curr.clone();
        let mut ctx = ComputeContext::new(100);
        ctx.has_prev = true;
        ctx.continuous = true;
        let item = ItemPair::single(0, def, &plan, &prev, &curr, ctx);

        let mem = spec::lookup(ActivityId::MEMORY).unwrap();
        let field = raw_fields(&mem.sections[0], &SadfConfig::default())
            .into_iter()
            .find(|f| f.name == "kbavail")
            .unwrap();
        let mut tok = Vec::new();
        push_raw_field(&mut tok, mem, &item, &field, &SadfConfig::default());
        assert_eq!(tok, vec!["kbavail".to_string(), "1234".to_string()]);
    }

    /// A_DISK は直書きの major / minor が先頭に来る。
    #[test]
    fn disk_raw_starts_with_major_minor() {
        let disk = spec::lookup(ActivityId::DISK).unwrap();
        let fields = raw_fields(&disk.sections[0], &SadfConfig::default());
        assert_eq!(fields[0].name, "major");
        assert_eq!(fields[1].name, "minor");
        // await / %util は消費されない
        let names: Vec<_> = fields.iter().map(|f| f.name).collect();
        assert!(!names.contains(&"await"));
        assert!(names.contains(&"tot_ticks"));
    }

    /// 欠落の 2 種類を区別する (指摘 8 と同じ規則)。
    ///
    /// 「その世代にフィールドが無い」は本家がゼロ補完した構造体を読むので `0`、
    /// 「このレコードで読めていない」は本家ならその行自体が無いので空文字。
    #[test]
    fn unsupported_field_is_zero_filled_but_missing_sample_is_empty() {
        assert_eq!(
            u64_token(Availability::UnsupportedBySource),
            "0",
            "本家は 0 埋めした構造体の値を出す (03 §1.9-1)"
        );
        assert_eq!(
            u64_token(Availability::MissingInSample),
            "",
            "観測できていない値に 0 を与えない"
        );
        assert_eq!(u64_token(Availability::Present(0)), "0", "正常な 0 は 0");
    }

    /// 行は `"; "` 区切りで、末尾に `;` が付く (§4.1)。
    #[test]
    fn line_is_semicolon_space_separated_with_trailing_semicolon() {
        let tok = vec![
            "13:20:19 UTC".to_string(),
            "proc/s".to_string(),
            "46972".to_string(),
            "47083".to_string(),
        ];
        assert_eq!(join_tokens(&tok), "13:20:19 UTC; proc/s; 46972; 47083;\n");
    }

    /// センサ値は小数 6 桁固定。
    #[test]
    fn sensor_values_have_six_decimals() {
        let mut s = String::new();
        write_sensor(&mut s, 1283.0);
        assert_eq!(s, "1283.000000");
    }
}

//! 統計 item 1 個分の変換計画。
//!
//! 本家は activity ごとに `upgrade_stats_<activity>()` を手書きしている
//! (`docs/format/01-file-format.md` §5.8。全 18 本)。reSARch は
//! **全 revision のフィールド定義をレジストリに持っている**ので、その必要がない。
//!
//! ```text
//! 旧 item のバイト列
//!   → 旧 revision の ResolvedLayout でフィールドを読む
//!   → 同名フィールドを現行 revision の ResolvedLayout の位置へ書く
//!   → 現行にしか無いフィールドは 0 (バッファのゼロ初期化がその根拠)
//! ```
//!
//! これで `upgrade_stats_*` 18 本が**フィールド名の対応**へ一元化される。
//! 型の拡幅 (`unsigned long` → `unsigned long long`)、`aligned(16)` の除去、
//! フィールドの移動 (`A_DISK` の `nr_ios` が末尾 → 先頭)、
//! フィールドの追加 (`A_DISK` の `dc_sect` / `wwn` / `part_nr`) は
//! すべてこの 1 本の経路で吸収される。
//!
//! ## 名前の対応だけでは足りないもの
//!
//! 「フィールドの意味が変わった」3 件だけは明示的に扱う (`Fixup`)。
//! これは配置の差ではなく意味の差なので、レイアウト記述には現れない。
//!
//! | activity | 何が変わったか | 典拠 |
//! |---|---|---|
//! | `A_IRQ` | 割り込みの識別が「配列添字」から `irq_name` 文字列へ変わった。名前を**生成する** | §5.8 `upgrade_stats_irq()` |
//! | `A_SERIAL` | `line` の基点が 1 → 0 | §5.8 `upgrade_stats_serial()` |
//! | `A_MEMORY` | `availablekb` が無い世代は `frmkb` で代用する (`%memused` が 100% になるのを避ける) | §5.8 `upgrade_stats_memory()` |

use crate::format::reader::Cursor;
use crate::format::wire::{FieldTy, PlacedField, ResolvedLayout};
use crate::format::writer::{WriteCursor, value_fits};
use crate::layout::plan::{DeclaredShape, select_revision};
use crate::layout::registry::{self, ActivityDef, WireRevision};
use crate::model::ActivityId;
use crate::series::compute;

/// `A_IRQ` が名前指定になった revision の magic (`ACTIVITY_MAGIC_BASE + 2`)。
///
/// 本家 `upgrade_stats_irq()` はこの値より小さい magic のときだけ
/// `nr` / `nr2` を入れ替えて名前を生成する (§5.5 / §5.8)。
pub(crate) const IRQ_NAMED_MAGIC: u32 = 0x8c;

/// `MAX_SA_IRQ_LEN` (`irq_name` の配列長)。名前はここへ収まる範囲で生成する。
const MAX_SA_IRQ_LEN: usize = 8;

/// 旧 → 新 の 1 フィールド対応。
#[derive(Debug, Clone, Copy)]
enum FieldMove {
    /// 整数。値を `u64` へ正規化してから出力側の型・幅で再直列化する。
    Unsigned { src: PlacedField, dst: PlacedField },
    /// 符号付き整数。符号拡張してから書く。
    Signed { src: PlacedField, dst: PlacedField },
    /// バイト列。**UTF-8 として解釈しない** (不正バイトを含むファイルが実在する)。
    /// 長さが違う場合は切り詰め / NUL 埋め。
    Bytes { src: PlacedField, dst: PlacedField },
}

/// レコードに書き出す件数の決め方 (本家の `count_stats_*`、§5.9)。
///
/// 旧ファイルは固定件数 (`file_activity.nr` = 割り当て上限) で書かれており、
/// 末尾に空エントリが並ぶ。新形式は `has_nr` でレコードごとに件数を持つので、
/// ここで切り詰める。
#[derive(Debug, Clone, Copy)]
enum Counter {
    /// `nr` をそのまま使う (切り詰めない)。
    All,
    /// 常に 1。`A_IRQ` は割り込み数が `nr2` へ移るため行が 1 本になる。
    One,
    /// 番兵フィールドが 0 になる手前まで (`A_PWR_USB` の `bus_nr`、`A_FS` の `f_blocks`)。
    UntilZero(PlacedField),
    /// 2 つの番兵の**和**が 0 になる手前まで (`A_DISK` の `major` + `minor`)。
    UntilBothZero(PlacedField, PlacedField),
    /// バイト列の先頭が NUL になる手前まで (`A_NET_DEV` 等の `interface`)。
    UntilEmptyText(PlacedField),
}

/// 名前の対応では表せない意味の変換 (§5.8)。
#[derive(Debug, Clone, Copy)]
enum Fixup {
    /// `A_IRQ`: 添字から `irq_name` を生成する (添字 0 は `"sum"`、以降は `添字-1`)。
    IrqName { dst: PlacedField },
    /// `A_SERIAL`: `line` を 1 起点から 0 起点へ。
    SerialLine { src: PlacedField, dst: PlacedField },
    /// `A_MEMORY`: `availablekb` が旧構造体に無いので `frmkb` で代用する。
    MemoryAvailable {
        src_frmkb: PlacedField,
        dst_available: PlacedField,
    },
}

/// activity 1 種の変換計画。レコードごとに作り直さず、変換の開始時に 1 回作る。
#[derive(Debug)]
pub(crate) struct ActivityPlan {
    pub id: ActivityId,
    /// 旧 item のストライド。**`file_activity.size` の申告値**であり、
    /// 構造体定義から計算した値ではない (§5.5 の `A_HUGE` の罠)。
    pub src_stride: usize,
    /// 新 item のサイズ。現行 revision を出力側の符号化で解決した値。
    pub dst_size: usize,
    /// 新 `file_activity.nr2`。`A_IRQ` では旧 `nr` が入る。
    pub out_nr2: u32,
    /// 新 `file_activity` に書く値。
    pub out_magic: u32,
    pub out_types_nr: [u32; 3],
    /// レコードごとに `__nr_t` を前置するか (現行実装の `AO_COUNTED`)。
    pub has_nr: bool,
    /// 既知 activity として構造を解釈できたか。
    ///
    /// 偽なら旧バイト列を**そのまま**書き出す (本家は `exit(1)` する。§5.15)。
    pub known: bool,
    /// `A_IRQ` のように `nr` / `nr2` を入れ替えたか (診断用)。
    pub swapped_dimensions: bool,
    moves: Vec<FieldMove>,
    counter: Counter,
    fixups: Vec<Fixup>,
}

/// 申告サイズに収まるフィールドか。
///
/// 収まらないフィールドは**そのファイルには存在しない**。読むと隣の item を踏むので
/// 対応表から落とす。本家が `upgrade_stats_memory()` などで
/// 「旧サイズが N 以上なら」と段階判定しているのと同じことを、
/// レイアウト記述から機械的に導いている。
#[inline]
fn fits_in(f: &PlacedField, declared: usize) -> bool {
    f.offset
        .checked_add(f.width)
        .is_some_and(|end| end <= declared)
}

/// 旧 item から「このファイルに実在するフィールド」だけを引ける表を作る。
fn present_fields(layout: &ResolvedLayout, declared: usize) -> Vec<PlacedField> {
    layout
        .fields
        .iter()
        .copied()
        .filter(|f| fits_in(f, declared))
        .collect()
}

fn find<'a>(fields: &'a [PlacedField], name: &str) -> Option<&'a PlacedField> {
    fields.iter().find(|f| f.name == name)
}

impl ActivityPlan {
    /// 既知 activity の計画を組み立てる。
    ///
    /// `src_enc` と `dst_enc` は同一 (変換後ファイルは元ファイルのバイト順と
    /// `sa_sizeof_long` を保つ。§5.3)。引数を分けているのは、
    /// 将来 ABI を変える変換を足したときにここが破綻しないようにするためである。
    pub(crate) fn build(
        id: ActivityId,
        src_magic: Option<u32>,
        src_size: usize,
        src_nr: u32,
        src_nr2: u32,
        enc: &crate::format::abi::SourceEncoding,
    ) -> Result<Self, PlanError> {
        match registry::lookup(id) {
            Some(def) => Self::build_for_def(def, src_magic, src_size, src_nr, src_nr2, enc),
            None => Ok(Self::opaque(id, src_size, src_nr2)),
        }
    }

    /// [`ActivityPlan::build`] の本体。activity 定義を引数で受ける。
    ///
    /// 定義を外から渡せるようにしてあるのは、実在の定義では起こらない
    /// 「定義側の矛盾」([`PlanError`]) を検出する経路をテストで確かめるためである。
    fn build_for_def(
        def: &ActivityDef,
        src_magic: Option<u32>,
        src_size: usize,
        src_nr: u32,
        src_nr2: u32,
        enc: &crate::format::abi::SourceEncoding,
    ) -> Result<Self, PlanError> {
        let id = def.id;
        let Some(dst_rev) = def.latest() else {
            return Ok(Self::opaque(id, src_size, src_nr2));
        };

        let shape = DeclaredShape {
            magic: src_magic,
            size: src_size,
            types_nr: None,
        };
        let src_rev: &WireRevision = match select_revision(def, &shape) {
            Ok(r) => r,
            // 既知 ID でも構造を確認できない magic は素通しにする。
            // 意味の分からないバイト列をフィールド単位で動かしてはいけない。
            Err(_) => return Ok(Self::opaque(id, src_size, src_nr2)),
        };

        let src_layout = src_rev.layout.resolve(enc)?;
        let dst_layout = dst_rev.layout.resolve(enc)?;
        let src_fields = present_fields(&src_layout, src_size);

        let mut moves = Vec::with_capacity(dst_layout.fields.len());
        for dst in dst_layout.fields.iter() {
            let Some(src) = find(&src_fields, dst.name) else {
                // 現行にしか無いフィールド。ゼロ初期化のまま残す (§5.5)。
                continue;
            };
            moves.push(match (src.ty, dst.ty) {
                (FieldTy::Bytes(_), FieldTy::Bytes(_)) => FieldMove::Bytes {
                    src: *src,
                    dst: *dst,
                },
                // 片方だけがバイト列なのは定義側の矛盾。黙って数値として写すと
                // 別の意味の値が統計として出るので、ここで止める。
                (FieldTy::Bytes(_), _) | (_, FieldTy::Bytes(_)) => {
                    return Err(PlanError::FieldKindMismatch {
                        activity: id,
                        field: dst.name,
                    });
                }
                _ if dst.ty.is_signed() || src.ty.is_signed() => FieldMove::Signed {
                    src: *src,
                    dst: *dst,
                },
                _ => FieldMove::Unsigned {
                    src: *src,
                    dst: *dst,
                },
            });
        }

        // --- 件数の数え方 (§5.9) ---
        let counter = if id == ActivityId::IRQ {
            Counter::One
        } else {
            match id {
                ActivityId::DISK => {
                    match (find(&src_fields, "major"), find(&src_fields, "minor")) {
                        (Some(a), Some(b)) => Counter::UntilBothZero(*a, *b),
                        _ => Counter::All,
                    }
                }
                ActivityId::NET_DEV | ActivityId::NET_EDEV => {
                    match find(&src_fields, "interface") {
                        Some(f) => Counter::UntilEmptyText(*f),
                        None => Counter::All,
                    }
                }
                ActivityId::NET_FC => match find(&src_fields, "fchost_name") {
                    Some(f) => Counter::UntilEmptyText(*f),
                    None => Counter::All,
                },
                ActivityId::PWR_USB => match find(&src_fields, "bus_nr") {
                    Some(f) => Counter::UntilZero(*f),
                    None => Counter::All,
                },
                ActivityId::FS => match find(&src_fields, "f_blocks") {
                    Some(f) => Counter::UntilZero(*f),
                    None => Counter::All,
                },
                // `A_SERIAL` は `line == 0` で打ち切る (本家は upgrade_stats_serial()
                // の戻り値を件数に使っており、実質この番兵と同じ)。
                ActivityId::SERIAL => match find(&src_fields, "line") {
                    Some(f) => Counter::UntilZero(*f),
                    None => Counter::All,
                },
                _ => Counter::All,
            }
        };

        // --- 意味の変換 (§5.8) ---
        let mut fixups = Vec::new();
        if id == ActivityId::IRQ
            && src_magic.is_some_and(|m| m < IRQ_NAMED_MAGIC)
            && let Some(dst) = dst_layout.field("irq_name")
        {
            fixups.push(Fixup::IrqName { dst: *dst });
        }
        if id == ActivityId::SERIAL
            && let (Some(src), Some(dst)) = (find(&src_fields, "line"), dst_layout.field("line"))
        {
            fixups.push(Fixup::SerialLine {
                src: *src,
                dst: *dst,
            });
        }
        if id == ActivityId::MEMORY
            && find(&src_fields, "availablekb").is_none()
            && let (Some(src_frmkb), Some(dst_available)) =
                (find(&src_fields, "frmkb"), dst_layout.field("availablekb"))
        {
            fixups.push(Fixup::MemoryAvailable {
                src_frmkb: *src_frmkb,
                dst_available: *dst_available,
            });
        }

        // `A_IRQ` は 1 次元 (割り込み数) から 2 次元行列 (CPU × 割り込み) になった。
        // 旧 `nr` (割り込み数) を `nr2` へ移し、`nr` は CPU "all" だけの 1 にする (§5.5)。
        let swap = id == ActivityId::IRQ && src_magic.is_some_and(|m| m < IRQ_NAMED_MAGIC);
        let out_nr2 = if swap { src_nr.max(1) } else { src_nr2.max(1) };

        Ok(Self {
            id,
            src_stride: src_size,
            dst_size: dst_layout.size,
            out_nr2,
            out_magic: dst_rev.magic,
            out_types_nr: dst_rev.types_nr,
            has_nr: def.has_nr,
            known: true,
            swapped_dimensions: swap,
            moves,
            counter,
            fixups,
        })
    }

    /// 構造を解釈しない素通し計画。
    ///
    /// 旧バイト列をそのまま複製し、`file_activity` も旧値のまま書く。
    /// 本家は未知 activity id を見つけると `get_activity_position[<id>]: Internal error`
    /// で `exit(1)` するが (§5.5 / §5.15)、それではファイル全体が救えない。
    /// 読み側 (`sar` / `sadf`) は未知 id を読み飛ばす作りなので、
    /// 素通しにしても後続のレコード境界は保たれる。
    fn opaque(id: ActivityId, src_size: usize, src_nr2: u32) -> Self {
        Self {
            id,
            src_stride: src_size,
            dst_size: src_size,
            out_nr2: src_nr2.max(1),
            // magic を持たない世代 (`0x2170`) は変換対象外なので、ここへは来ない。
            out_magic: 0,
            out_types_nr: [0, 0, 0],
            has_nr: false,
            known: false,
            swapped_dimensions: false,
            moves: Vec::new(),
            counter: Counter::All,
            fixups: Vec::new(),
        }
    }

    /// このレコードで書き出す item 数を決める (§5.9)。
    ///
    /// `nr` はそのレコードで有効な件数 (`0x2173` の RESTART で更新された値)。
    pub(crate) fn count(&self, cur: &Cursor<'_>, base: usize, nr: u32) -> u32 {
        match self.counter {
            Counter::All => nr,
            Counter::One => 1,
            Counter::UntilZero(f) => self.scan_sentinel(cur, base, nr, |cur, item| {
                cur.read_unsigned(item, &f).unwrap_or(0) != 0
            }),
            Counter::UntilBothZero(a, b) => self.scan_sentinel(cur, base, nr, |cur, item| {
                let x = cur.read_unsigned(item, &a).unwrap_or(0);
                let y = cur.read_unsigned(item, &b).unwrap_or(0);
                x.saturating_add(y) != 0
            }),
            Counter::UntilEmptyText(f) => self.scan_sentinel(cur, base, nr, |cur, item| {
                !cur.read_bytes(item, &f).unwrap_or(&[]).is_empty()
            }),
        }
    }

    fn scan_sentinel(
        &self,
        cur: &Cursor<'_>,
        base: usize,
        nr: u32,
        alive: impl Fn(&Cursor<'_>, usize) -> bool,
    ) -> u32 {
        for i in 0..nr as usize {
            let item = base + i * self.src_stride;
            if !alive(cur, item) {
                return i as u32;
            }
        }
        nr
    }

    /// 旧 item 1 個を新 item へ変換する。
    ///
    /// `dst` は**ゼロ初期化済み**で `dst_size` バイト以上あること。
    /// 「旧構造体に無いフィールドが 0 になる」根拠はこのゼロ初期化だけである (§5.5)。
    ///
    /// `item_index` は出力側の item 添字。`A_IRQ` の名前生成に使う
    /// (変換後は `nr` = 1 なので添字 = 割り込みの列番号になる)。
    pub(crate) fn convert_item(
        &self,
        cur: &Cursor<'_>,
        src_base: usize,
        dst: &mut WriteCursor<'_>,
        dst_base: usize,
        item_index: u32,
        lossy: &mut u64,
    ) -> Result<(), crate::format::reader::OutOfBounds> {
        if !self.known {
            // 素通し。意味を解釈しないのでバイト列をそのまま複製する。
            let raw = cur.raw(src_base, self.src_stride)?;
            dst.put_raw(dst_base, raw)?;
            return Ok(());
        }

        for m in &self.moves {
            match m {
                FieldMove::Unsigned { src, dst: d } => {
                    let v = cur.read_unsigned(src_base, src)?;
                    if !value_fits(d, v) {
                        // 出力側が `unsigned long` (32bit ライタのファイルでは 4 バイト)
                        // で、旧値が収まらない。切り詰めて書くが件数を報告する。
                        *lossy += 1;
                    }
                    dst.write_unsigned(dst_base, d, v)?;
                }
                FieldMove::Signed { src, dst: d } => {
                    let v = cur.read_signed(src_base, src)?;
                    dst.write_signed(dst_base, d, v)?;
                }
                FieldMove::Bytes { src, dst: d } => {
                    let raw = cur.read_bytes(src_base, src)?;
                    dst.write_bytes(dst_base, d, raw)?;
                }
            }
        }

        for f in &self.fixups {
            match f {
                Fixup::IrqName { dst: d } => {
                    // 旧形式は「割り込み番号 = 配列添字」だったため名前が無い。
                    // 添字 0 は総和スロット、以降は 10 進表記の割り込み番号。
                    let name = irq_name(item_index);
                    dst.write_bytes(dst_base, d, name.as_bytes())?;
                }
                Fixup::SerialLine { src, dst: d } => {
                    // `line` の基点が 1 → 0 に変わった。0 は空スロットの印なので、
                    // ここへ来る item は必ず 1 以上 (件数の番兵で切っている)。
                    let v = cur.read_unsigned(src_base, src)?;
                    dst.write_unsigned(dst_base, d, v.saturating_sub(1))?;
                }
                Fixup::MemoryAvailable {
                    src_frmkb,
                    dst_available,
                } => {
                    // `availablekb` が無い世代を 0 にすると `%memused` が 100% になる。
                    // 本家は空きメモリで代用する。
                    let v = cur.read_unsigned(src_base, src_frmkb)?;
                    dst.write_unsigned(dst_base, dst_available, v)?;
                }
            }
        }

        Ok(())
    }
}

/// 旧形式の割り込み添字から `irq_name` を作る (本家 `upgrade_stats_irq()`)。
///
/// 添字 0 は総和スロットで名前は `"sum"`、添字 `i` は割り込み番号 `i - 1`。
/// 名前の規則は出力・集計が旧世代のファイルを直接読むときのラベルと共有する
/// ([`compute::irq_item_name`])。変換後のファイルと変換前のファイルで
/// 同じ割り込みが同じ名前になる。
/// `MAX_SA_IRQ_LEN` に収まらない桁数は切り詰める (本家の `snprintf` と同じ)。
fn irq_name(item_index: u32) -> String {
    let mut s = compute::irq_item_name(item_index as usize, None);
    s.truncate(MAX_SA_IRQ_LEN - 1);
    s
}

/// 変換計画を組み立てられない理由。**実装側の定義の矛盾**を表す。
#[derive(Debug, thiserror::Error)]
pub(crate) enum PlanError {
    #[error("レイアウト定義の不整合: {0}")]
    Layout(#[from] crate::error::LayoutError),

    #[error("{activity}: フィールド {field} の種別が旧新で食い違っている")]
    FieldKindMismatch {
        activity: ActivityId,
        field: &'static str,
    },
}

impl From<PlanError> for crate::Error {
    fn from(e: PlanError) -> Self {
        match e {
            PlanError::Layout(l) => crate::Error::Layout(l),
            other => crate::Error::Other(other.to_string()),
        }
    }
}

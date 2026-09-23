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
    /// 偽なら旧バイト列を**そのまま**書き出す (本家は未知 ID なら `exit(1)` する。§5.15。
    /// 既知 ID の未知 magic では中断せず、既知の形式とみなして書き換える)。
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

#[cfg(test)]
mod tests {
    //! 変換計画 ([`ActivityPlan`]) の単体テスト。
    //!
    //! 旧 item のバイト列は、本家 `sa_conv.h` の旧構造体宣言から手で導いた位置に値を置いて組む
    //! (`aligned(n)` は「自然境界と n の大きい方」、`packed` は「直前のメンバの直後」)。
    //! 導いたサイズは `docs/format/01-file-format.md` §5.12 の表と一致する。
    //! 変換後の値は `docs/format/02-activities.md` §5 のオフセット表の位置から読む。
    //!
    //! **本体のレイアウト定義からオフセットを引かない。** 旧 revision の定義に誤りがあると、
    //! その定義で書いた item をその定義で読むテストは誤りごと通ってしまう。

    use super::*;
    use crate::error::LayoutError;
    use crate::format::abi::{Endian, LayoutAbi, SourceEncoding};
    use crate::format::wire::{WireField, WireLayout};
    use crate::layout::registry::ItemShape;

    // -----------------------------------------------------------------------
    // 旧構造体 (時代 A) の item 配置
    // -----------------------------------------------------------------------

    /// `stats_disk_8a` (magic `0x8a` / 80 バイト)。
    ///
    /// rd_sect ULL@0 (aligned 16) / wr_sect ULL@16 (aligned 16) / rd_ticks UL@32 (aligned 16) /
    /// wr_ticks UL@40 / tot_ticks UL@48 / rq_ticks UL@56 / nr_ios UL@64 (各 aligned 8) /
    /// major u32@72 (aligned 8) / minor u32@76 (packed)。
    const DISK_8A: (u32, usize) = (0x8a, 80);
    const DISK_8A_MAJOR: usize = 72;
    const DISK_8A_MINOR: usize = 76;

    /// `stats_disk_8b` (magic `0x8b` / 64 バイト)。
    ///
    /// nr_ios ULL@0 (aligned 16) / rd_sect UL@16 (aligned 16) / wr_sect UL@24 (aligned 8) /
    /// rd_ticks u32@32 (aligned 8) / wr_ticks・tot_ticks・rq_ticks・major・minor は packed で
    /// 36 / 40 / 44 / 48 / 52 (末尾は 16 境界へ丸めて 64)。
    const DISK_8B: (u32, usize) = (0x8b, 64);
    const DISK_8B_MAJOR: usize = 48;
    const DISK_8B_MINOR: usize = 52;

    /// `stats_net_dev_8a` (72 バイト): UL × 7 (各 aligned 8) の後に interface[16]@56。
    /// `stats_net_dev_8b` (128 バイト): ULL × 7 (各 aligned 16) の後に interface[16]@112。
    /// `stats_net_dev_8c` (144 バイト): ULL × 7 (各 aligned 16)、speed u32@112、
    /// interface[16]@116 (aligned 4)、duplex@132。
    const NET_DEV_OLD: [(u32, usize, usize); 3] =
        [(0x8a, 72, 56), (0x8b, 128, 112), (0x8c, 144, 116)];

    /// `stats_net_edev_8a` (88 バイト): UL × 9 の後に interface[16]@72。
    /// `stats_net_edev_8b` (160 バイト): ULL × 9 (各 aligned 16) の後に interface[16]@144。
    const NET_EDEV_OLD: [(u32, usize, usize); 2] = [(0x8a, 88, 72), (0x8b, 160, 144)];

    /// `stats_fchost` (magic `0x8a` / 48 バイト、現行と同一): UL × 4 の後に fchost_name[16]@32。
    const FCHOST_NAME: usize = 32;

    /// `stats_pwr_usb` (magic `0x8a` / 88 バイト、現行と同一): bus_nr u32@0。
    const USB_BUS_NR: usize = 0;

    /// `stats_filesystem_8a`: ULL × 5 (各 aligned 16) = f_blocks@0 … f_ffree@64、
    /// fs_name@80 (aligned 16)。160 バイト版は fs_name[72] で終わり (`MAX_FS_LEN` = 72 時代)、
    /// 336 バイト版は fs_name[128] の後に mountp[128]@208 が続く。
    const FS_160: (u32, usize) = (0x8a, 160);
    const FS_336: (u32, usize) = (0x8a, 336);
    const FS_NAME: usize = 80;
    const FS_MOUNTP: usize = 208;

    /// `stats_serial` (magic `0x8a` / 28 バイト、現行と同じ配置): u32 × 7、line@24。
    const SERIAL_LINE: usize = 24;

    // -----------------------------------------------------------------------
    // 現行構造体 (02-activities.md §5) の位置
    // -----------------------------------------------------------------------

    /// `stats_disk` (80 バイト): nr_ios@0 / rd_sect@24 / wr_sect@32 / rd_ticks@48 /
    /// major@64 / minor@68。
    const CUR_DISK_NR_IOS: usize = 0;
    const CUR_DISK_RD_SECT: usize = 24;
    const CUR_DISK_WR_SECT: usize = 32;
    const CUR_DISK_RD_TICKS: usize = 48;
    const CUR_DISK_MAJOR: usize = 64;
    const CUR_DISK_MINOR: usize = 68;

    /// `stats_net_dev` (80 バイト): speed@56 / interface[16]@60 / duplex@76。
    const CUR_NET_DEV_SPEED: usize = 56;
    const CUR_NET_DEV_IFACE: usize = 60;
    const CUR_NET_DEV_DUPLEX: usize = 76;

    /// `stats_filesystem` (296 バイト): fs_name[128]@40 / mountp[128]@168。
    const CUR_FS_NAME: usize = 40;
    const CUR_FS_MOUNTP: usize = 168;

    // -----------------------------------------------------------------------
    // 補助
    // -----------------------------------------------------------------------

    fn le64() -> SourceEncoding {
        SourceEncoding::new(Endian::Little, LayoutAbi::LP64)
    }

    /// 変換対象になる 4 通りの符号化 (LE / BE × 64bit / 32bit)。
    fn all_encodings() -> [SourceEncoding; 4] {
        [
            SourceEncoding::new(Endian::Little, LayoutAbi::LP64),
            SourceEncoding::new(Endian::Big, LayoutAbi::LP64),
            SourceEncoding::new(Endian::Little, LayoutAbi::I386),
            SourceEncoding::new(Endian::Big, LayoutAbi::ILP32_ALIGN8),
        ]
    }

    fn label(enc: &SourceEncoding) -> String {
        format!("{}/{}", enc.endian, enc.abi.name)
    }

    /// 旧 item 1 個分のバイト列を組む。
    ///
    /// 値はファイルのバイト順で置く。`unsigned long` は 8 バイトのスロットの
    /// **先頭** `sizeof(long)` バイトだけに入れる (`01-file-format.md` §3.0)。
    #[derive(Clone)]
    struct Item {
        buf: Vec<u8>,
        enc: SourceEncoding,
    }

    impl Item {
        fn new(size: usize, enc: &SourceEncoding) -> Self {
            Self {
                buf: vec![0; size],
                enc: *enc,
            }
        }

        fn put(mut self, off: usize, bytes: &[u8]) -> Self {
            self.buf[off..off + bytes.len()].copy_from_slice(bytes);
            self
        }

        fn u32(self, off: usize, v: u32) -> Self {
            let b = match self.enc.endian {
                Endian::Little => v.to_le_bytes(),
                Endian::Big => v.to_be_bytes(),
            };
            self.put(off, &b)
        }

        fn u64(self, off: usize, v: u64) -> Self {
            let b = match self.enc.endian {
                Endian::Little => v.to_le_bytes(),
                Endian::Big => v.to_be_bytes(),
            };
            self.put(off, &b)
        }

        fn ul(self, off: usize, v: u64) -> Self {
            if self.enc.abi.long_bytes == 8 {
                self.u64(off, v)
            } else {
                self.u32(off, v as u32)
            }
        }

        fn text(self, off: usize, s: &str) -> Self {
            self.put(off, s.as_bytes())
        }
    }

    /// item 列を申告サイズのストライドで 1 本に並べる。
    fn concat(items: &[Item]) -> Vec<u8> {
        items.iter().flat_map(|i| i.buf.iter().copied()).collect()
    }

    fn plan(
        id: ActivityId,
        (magic, size): (u32, usize),
        nr: u32,
        enc: &SourceEncoding,
    ) -> ActivityPlan {
        ActivityPlan::build(id, Some(magic), size, nr, 1, enc).unwrap_or_else(|e| {
            panic!(
                "{id} magic=0x{magic:x} size={size} ({}): 計画を組めない: {e}",
                label(enc)
            )
        })
    }

    /// レコード内の item 列から、書き出す件数を数える。
    fn count_of(plan: &ActivityPlan, items: &[Item], nr: u32) -> u32 {
        let enc = items.first().map(|i| i.enc).unwrap_or_else(le64);
        let bytes = concat(items);
        plan.count(&Cursor::new(&bytes, enc.endian), 0, nr)
    }

    /// 旧 item 1 個を変換し、変換後の item と「切り詰めたフィールド数」を返す。
    fn convert(plan: &ActivityPlan, item: &Item, index: u32) -> (Vec<u8>, u64) {
        let mut out = vec![0u8; plan.dst_size];
        let mut lossy = 0u64;
        let cur = Cursor::new(&item.buf, item.enc.endian);
        let mut w = WriteCursor::new(&mut out, item.enc.endian);
        plan.convert_item(&cur, 0, &mut w, 0, index, &mut lossy)
            .unwrap_or_else(|e| panic!("{}: 範囲内の item を変換できない: {e:?}", plan.id));
        (out, lossy)
    }

    fn rd_u32(buf: &[u8], off: usize, enc: &SourceEncoding) -> u32 {
        Cursor::new(buf, enc.endian).u32_at(off).expect("範囲内")
    }

    fn rd_u64(buf: &[u8], off: usize, enc: &SourceEncoding) -> u64 {
        Cursor::new(buf, enc.endian).u64_at(off).expect("範囲内")
    }

    /// `unsigned long` のスロットを読む (先頭 `sizeof(long)` バイトだけが値)。
    fn rd_ul(buf: &[u8], off: usize, enc: &SourceEncoding) -> u64 {
        if enc.abi.long_bytes == 8 {
            rd_u64(buf, off, enc)
        } else {
            u64::from(rd_u32(buf, off, enc))
        }
    }

    // -----------------------------------------------------------------------
    // 素通し (構造を解釈しない activity)
    // -----------------------------------------------------------------------

    /// 未知 ID は、ID・申告サイズ・nr2 だけを保った素通しの計画になる。
    #[test]
    fn unknown_activity_id_is_planned_as_an_opaque_pass_through() {
        let enc = le64();
        let p = ActivityPlan::build(ActivityId(200), Some(0x8a), 24, 3, 2, &enc)
            .expect("未知 ID はエラーにしない");
        assert!(!p.known, "未知 ID は構造を解釈できない");
        assert_eq!(p.id, ActivityId(200));
        assert_eq!(p.src_stride, 24, "ストライドは申告サイズ");
        assert_eq!(p.dst_size, 24, "書き出すサイズも旧申告値のまま");
        assert_eq!(p.out_nr2, 2);
        assert!(!p.has_nr, "件数を前置しない (旧ファイルと同じ固定件数)");
        assert!(!p.swapped_dimensions);
        assert_eq!(p.out_magic, 0);
        assert_eq!(p.out_types_nr, [0, 0, 0]);
        assert!(p.moves.is_empty() && p.fixups.is_empty());
        assert!(matches!(p.counter, Counter::All));

        // 全ゼロの item も数える。意味の分からない item に番兵を当てはめない。
        let zeros = vec![0u8; 24 * 3 * 2];
        assert_eq!(p.count(&Cursor::new(&zeros, enc.endian), 0, 3), 3);
    }

    /// `nr2` が 0 と申告されていても、書き出す `nr2` は 1 以上にする
    /// (現行形式の読み手は `nr2 < 1` を拒否する)。
    #[test]
    fn opaque_plan_writes_at_least_one_sub_item() {
        let p = ActivityPlan::build(ActivityId(200), Some(0x8a), 8, 1, 0, &le64())
            .expect("未知 ID はエラーにしない");
        assert_eq!(p.out_nr2, 1);
    }

    /// 既知 ID でも、magic から revision を決められないものは素通しになる。
    ///
    /// `A_DISK` の旧 `0x8a` 版と同じ 80 バイトでも、magic `0x8f` は既知のどの revision でもない。
    /// 意味の分からないバイト列をフィールド単位で動かすと、別の意味の値が統計として出る。
    #[test]
    fn known_id_with_an_unrecognised_magic_is_passed_through_byte_for_byte() {
        for enc in all_encodings() {
            let who = label(&enc);
            let p = ActivityPlan::build(ActivityId::DISK, Some(0x8f), 80, 4, 1, &enc)
                .expect("magic が未知でもエラーにしない");
            assert!(!p.known, "{who}: revision を決められないので素通し");
            assert_eq!(p.dst_size, 80, "{who}");
            assert!(
                !p.has_nr,
                "{who}: A_DISK は現行で AO_COUNTED だが、素通しは件数を前置しない"
            );
            assert!(
                matches!(p.counter, Counter::All),
                "{who}: 番兵 (major + minor) を当てはめない"
            );
            let zeros = vec![0u8; 80 * 4];
            assert_eq!(p.count(&Cursor::new(&zeros, enc.endian), 0, 4), 4, "{who}");

            let item = Item::new(80, &enc)
                .u64(0, 0x0102_0304_0506_0708)
                .ul(64, 42)
                .u32(DISK_8A_MAJOR, 8)
                .u32(DISK_8A_MINOR, 16);
            let (out, lossy) = convert(&p, &item, 0);
            assert_eq!(out, item.buf, "{who}: 素通しはバイト列をそのまま写す");
            assert_eq!(lossy, 0, "{who}");

            // 同じバイト列でも magic `0x8a` なら旧 revision として解釈され、配置が変わる。
            // 素通しが「たまたま同じ配置だった」のではないことの確認。
            let known = plan(ActivityId::DISK, DISK_8A, 4, &enc);
            let (moved, _) = convert(&known, &item, 0);
            assert_ne!(moved, item.buf, "{who}");
            assert_eq!(rd_u32(&moved, CUR_DISK_MAJOR, &enc), 8, "{who}");
            assert_eq!(rd_u32(&moved, CUR_DISK_MINOR, &enc), 16, "{who}");
        }
    }

    /// magic を持たない形 (`0x2170` と同じく magic = 0) では申告サイズだけで revision を探し、
    /// 一致しなければ素通しにする。
    #[test]
    fn missing_magic_selects_the_revision_by_declared_size_or_passes_through() {
        let enc = le64();
        let by_size = ActivityPlan::build(ActivityId::DISK, None, DISK_8B.1, 1, 1, &enc)
            .expect("計画を組める");
        assert!(by_size.known, "64 バイトは旧 0x8b 版として引ける");

        let unknown_size =
            ActivityPlan::build(ActivityId::DISK, None, 72, 1, 1, &enc).expect("計画を組める");
        assert!(
            !unknown_size.known,
            "どの revision とも合わないサイズは素通し"
        );
        assert_eq!(unknown_size.dst_size, 72);
    }

    /// 定義に revision が 1 つも無い activity は素通しになる。
    #[test]
    fn a_definition_without_revisions_is_passed_through() {
        let def = ActivityDef {
            id: ActivityId::PCSW,
            revisions: &[],
            columns: &[],
            shape: ItemShape::Single,
            item_key: "",
            has_nr: false,
        };
        let p = ActivityPlan::build_for_def(&def, Some(0x8a), 32, 1, 1, &le64())
            .expect("revision が無くてもエラーにしない");
        assert!(!p.known);
        assert_eq!((p.src_stride, p.dst_size), (32, 32));
    }

    // -----------------------------------------------------------------------
    // 件数の番兵 (count_stats_*、01 §5.9)
    // -----------------------------------------------------------------------

    /// `A_DISK` は `major + minor == 0` の手前までを数える (本家 `count_stats_disk()`)。
    ///
    /// 判定は**和**なので、major が 0 でも minor が非 0 なら生きている item として数える。
    /// 番兵より後ろに値の残った item があっても数えない。
    #[test]
    fn disk_count_stops_where_major_plus_minor_is_zero() {
        for enc in all_encodings() {
            for (rev, major_off, minor_off) in [
                (DISK_8A, DISK_8A_MAJOR, DISK_8A_MINOR),
                (DISK_8B, DISK_8B_MAJOR, DISK_8B_MINOR),
            ] {
                let who = format!("{} magic=0x{:x}", label(&enc), rev.0);
                let p = plan(ActivityId::DISK, rev, 5, &enc);
                assert!(
                    matches!(p.counter, Counter::UntilBothZero(a, b)
                        if a.name == "major" && b.name == "minor"),
                    "{who}: {:?}",
                    p.counter
                );
                let dev = |major: u32, minor: u32| {
                    Item::new(rev.1, &enc)
                        .u32(major_off, major)
                        .u32(minor_off, minor)
                };

                let items = [dev(8, 0), dev(0, 1), dev(0, 0), dev(8, 16), dev(0, 0)];
                assert_eq!(count_of(&p, &items, 5), 2, "{who}");
                // 先頭が空なら 0 件 (現行形式として正当な件数)
                assert_eq!(count_of(&p, &[dev(0, 0), dev(8, 0)], 2), 0, "{who}");
                // 番兵が無ければ nr 件すべて
                let full = [dev(8, 0), dev(8, 16), dev(8, 32)];
                assert_eq!(count_of(&p, &full, 3), 3, "{who}");
            }
        }
    }

    /// `A_NET_DEV` / `A_NET_EDEV` は interface 名が空の手前までを数える
    /// (本家 `count_stats_net_dev()` / `count_stats_net_edev()` の `strcmp(interface, "")`)。
    ///
    /// 判定は先頭 1 バイトだけで決まる。空の名前の item がカウンタ値を持っていても打ち切る。
    #[test]
    fn net_dev_counts_stop_at_the_first_empty_interface_name() {
        let revisions = NET_DEV_OLD
            .iter()
            .map(|r| (ActivityId::NET_DEV, *r))
            .chain(NET_EDEV_OLD.iter().map(|r| (ActivityId::NET_EDEV, *r)));
        for (id, (magic, size, iface)) in revisions {
            for enc in all_encodings() {
                let who = format!("{id} magic=0x{magic:x} ({})", label(&enc));
                let p = plan(id, (magic, size), 4, &enc);
                assert!(
                    matches!(p.counter, Counter::UntilEmptyText(f) if f.name == "interface"),
                    "{who}: {:?}",
                    p.counter
                );
                let nic = |name: &str| Item::new(size, &enc).text(iface, name).ul(0, 99);

                let items = [nic("eth0"), nic("lo"), nic(""), nic("eth1")];
                assert_eq!(count_of(&p, &items, 4), 2, "{who}");

                // 先頭が NUL なら、後ろに文字が残っていても空の名前
                let stale = Item::new(size, &enc).text(iface + 1, "th0");
                assert_eq!(count_of(&p, &[nic("eth0"), stale], 2), 1, "{who}");

                let full = [nic("eth0"), nic("eth1")];
                assert_eq!(count_of(&p, &full, 2), 2, "{who}: 番兵が無ければ nr 件");
            }
        }
    }

    /// `A_NET_FC` は `fchost_name` が空の手前までを数える (本家 `count_stats_fchost()`)。
    #[test]
    fn net_fc_count_stops_at_the_first_empty_host_name() {
        for enc in all_encodings() {
            let who = label(&enc);
            let p = plan(ActivityId::NET_FC, (0x8a, 48), 3, &enc);
            assert!(
                matches!(p.counter, Counter::UntilEmptyText(f) if f.name == "fchost_name"),
                "{who}: {:?}",
                p.counter
            );
            let host = |name: &str| Item::new(48, &enc).ul(0, 5).text(FCHOST_NAME, name);
            assert_eq!(
                count_of(&p, &[host("host0"), host(""), host("host1")], 3),
                1,
                "{who}"
            );
            assert_eq!(count_of(&p, &[host("host0"), host("host1")], 2), 2, "{who}");
        }
    }

    /// `A_PWR_USB` は `bus_nr == 0` の手前までを数える (本家 `count_stats_pwr_usb()`)。
    #[test]
    fn pwr_usb_count_stops_at_bus_number_zero() {
        for enc in all_encodings() {
            let who = label(&enc);
            let p = plan(ActivityId::PWR_USB, (0x8a, 88), 4, &enc);
            assert!(
                matches!(p.counter, Counter::UntilZero(f) if f.name == "bus_nr"),
                "{who}: {:?}",
                p.counter
            );
            let usb = |bus: u32| {
                Item::new(88, &enc)
                    .u32(USB_BUS_NR, bus)
                    .u32(4, 0x1d6b)
                    .text(16, "vendor")
            };
            // bus_nr が 0 なら vendor_id や文字列が残っていても空きスロット
            assert_eq!(
                count_of(&p, &[usb(1), usb(2), usb(0), usb(3)], 4),
                2,
                "{who}"
            );
            assert_eq!(count_of(&p, &[usb(1), usb(2)], 2), 2, "{who}");
        }
    }

    /// `A_FS` は `f_blocks == 0` の手前までを数える (本家 `count_stats_filesystem()`)。
    /// 旧形式は `mountp` の有無で 160 / 336 バイトの 2 版がある。
    #[test]
    fn fs_count_stops_at_zero_total_blocks() {
        for enc in all_encodings() {
            for rev in [FS_160, FS_336] {
                let who = format!("{} size={}", label(&enc), rev.1);
                let p = plan(ActivityId::FS, rev, 3, &enc);
                assert!(
                    matches!(p.counter, Counter::UntilZero(f) if f.name == "f_blocks"),
                    "{who}: {:?}",
                    p.counter
                );
                let fs = |blocks: u64| {
                    Item::new(rev.1, &enc)
                        .u64(0, blocks)
                        .text(FS_NAME, "/dev/sda1")
                };
                assert_eq!(count_of(&p, &[fs(100), fs(0), fs(50)], 3), 1, "{who}");
                assert_eq!(count_of(&p, &[fs(100), fs(50)], 2), 2, "{who}");
            }
        }
    }

    /// `A_SERIAL` は `line == 0` の手前までを数え (本家 `upgrade_stats_serial()` の戻り値)、
    /// 書き出す `line` は 1 起点から 0 起点へ 1 つ減る。
    #[test]
    fn serial_count_stops_at_line_zero_and_lines_become_zero_based() {
        for enc in all_encodings() {
            let who = label(&enc);
            let p = plan(ActivityId::SERIAL, (0x8a, 28), 4, &enc);
            assert!(
                matches!(p.counter, Counter::UntilZero(f) if f.name == "line"),
                "{who}: {:?}",
                p.counter
            );
            assert!(
                p.fixups
                    .iter()
                    .any(|f| matches!(f, Fixup::SerialLine { .. })),
                "{who}: line の基点を直す"
            );
            let tty = |line: u32| {
                Item::new(28, &enc)
                    .u32(0, 1000 + line)
                    .u32(4, 2000 + line)
                    .u32(20, 6000 + line)
                    .u32(SERIAL_LINE, line)
            };
            let items = [tty(1), tty(3), tty(0), tty(2)];
            assert_eq!(count_of(&p, &items, 4), 2, "{who}");

            for (i, line) in [(0u32, 1u32), (1, 3)] {
                let (out, _) = convert(&p, &items[i as usize], i);
                assert_eq!(
                    rd_u32(&out, SERIAL_LINE, &enc),
                    line - 1,
                    "{who}: line {line}"
                );
                // line 以外は同じ位置にそのまま写る
                assert_eq!(rd_u32(&out, 0, &enc), 1000 + line, "{who}: rx");
                assert_eq!(rd_u32(&out, 4, &enc), 2000 + line, "{who}: tx");
                assert_eq!(rd_u32(&out, 20, &enc), 6000 + line, "{who}: overrun");
            }
        }
    }

    /// 番兵フィールドが申告サイズの外にある (このファイルには存在しない) なら、
    /// 読めない位置を読まずに `nr` 件すべてを書く。
    #[test]
    fn sentinel_fields_beyond_the_declared_size_fall_back_to_counting_every_slot() {
        let cases: [(ActivityId, u32, usize); 7] = [
            // major@48 / minor@52 が 48 バイトに収まらない
            (ActivityId::DISK, 0x8b, 48),
            // interface@56 が収まらない
            (ActivityId::NET_DEV, 0x8a, 56),
            (ActivityId::NET_EDEV, 0x8a, 72),
            (ActivityId::NET_FC, 0x8a, FCHOST_NAME),
            // bus_nr (0..4) すら収まらない
            (ActivityId::PWR_USB, 0x8a, 2),
            // f_blocks (0..8) が収まらない
            (ActivityId::FS, 0x8a, 4),
            // line@24 が収まらない
            (ActivityId::SERIAL, 0x8a, SERIAL_LINE),
        ];
        for (id, magic, size) in cases {
            for enc in all_encodings() {
                let who = format!("{id} size={size} ({})", label(&enc));
                let p = plan(id, (magic, size), 3, &enc);
                assert!(p.known, "{who}: magic は既知なので構造は解釈する");
                assert!(matches!(p.counter, Counter::All), "{who}: {:?}", p.counter);
                let zeros = vec![0u8; size * 3];
                assert_eq!(p.count(&Cursor::new(&zeros, enc.endian), 0, 3), 3, "{who}");
            }
        }
    }

    /// 番兵を読む途中でバイト列が尽きたら、そこで数えるのをやめる (パニックしない)。
    #[test]
    fn a_sentinel_past_the_end_of_the_bytes_ends_the_count() {
        let enc = le64();
        let disk = plan(ActivityId::DISK, DISK_8B, 5, &enc);
        let dev = |major: u32| Item::new(DISK_8B.1, &enc).u32(DISK_8B_MAJOR, major);
        // 5 件と申告されているが 2 件分しか無い
        assert_eq!(count_of(&disk, &[dev(8), dev(8)], 5), 2);

        let (magic, size, iface) = NET_DEV_OLD[2];
        let nic = plan(ActivityId::NET_DEV, (magic, size), 3, &enc);
        let one = Item::new(size, &enc).text(iface, "eth0");
        // 2 件目は interface の途中で切れている
        let mut bytes = concat(&[one.clone(), one]);
        bytes.truncate(size + iface + 2);
        assert_eq!(nic.count(&Cursor::new(&bytes, enc.endian), 0, 3), 1);
    }

    /// 番兵を持たない activity (`A_CPU` など) は `nr` をそのまま使い、
    /// `A_IRQ` は割り込み数が `nr2` へ移るので常に 1 行になる。
    #[test]
    fn activities_without_sentinels_keep_nr_and_irq_is_a_single_row() {
        let enc = le64();
        let cpu = plan(ActivityId::CPU, (0x8a, 160), 4, &enc);
        assert!(matches!(cpu.counter, Counter::All));
        let zeros = vec![0u8; 160 * 4];
        assert_eq!(cpu.count(&Cursor::new(&zeros, enc.endian), 0, 4), 4);

        let irq = plan(ActivityId::IRQ, (0x8a, 16), 7, &enc);
        assert!(matches!(irq.counter, Counter::One));
        assert!(irq.swapped_dimensions);
        assert_eq!(irq.out_nr2, 7, "旧 nr (割り込み数) が nr2 へ移る");
        let zeros = vec![0u8; 16 * 7];
        assert_eq!(irq.count(&Cursor::new(&zeros, enc.endian), 0, 7), 1);
    }

    // -----------------------------------------------------------------------
    // フィールドの写し方
    // -----------------------------------------------------------------------

    /// 符号付きフィールドは値を保って写る (`A_PWR_BAT` の `char` × 3)。
    ///
    /// `A_PWR_BAT` は旧ファイルには現れないが、magic `0x8a` / 3 バイトなら
    /// 現行 revision そのものとして解釈される。
    #[test]
    fn signed_fields_keep_their_values() {
        for enc in all_encodings() {
            let who = label(&enc);
            let p = plan(ActivityId::PWR_BAT, (0x8a, 3), 1, &enc);
            assert!(
                p.moves
                    .iter()
                    .all(|m| matches!(m, FieldMove::Signed { .. })),
                "{who}: {:?}",
                p.moves
            );
            let item = Item::new(3, &enc).put(0, &[0x01, 0xff, 0x80]);
            let (out, lossy) = convert(&p, &item, 0);
            assert_eq!(out, vec![0x01, 0xff, 0x80], "{who}");
            assert_eq!(lossy, 0, "{who}");
        }
    }

    /// 符号付きフィールドが広がるときは符号拡張する (-2 は 4 バイトでも -2)。
    #[test]
    fn signed_fields_are_sign_extended_when_widened() {
        const NEW: &[WireField] = &[WireField::natural("delta", FieldTy::I32)];
        const OLD: &[WireField] = &[WireField::natural("delta", FieldTy::I16)];
        const REVISIONS: &[WireRevision] = &[
            WireRevision {
                magic: 0x8b,
                self_describing: true,
                types_nr: [0, 0, 1],
                size_lp64: 4,
                layout: WireLayout::new("widen@0x8b", NEW),
                since: "test",
            },
            WireRevision {
                magic: 0x8a,
                self_describing: false,
                types_nr: [0, 0, 0],
                size_lp64: 2,
                layout: WireLayout::new("widen@0x8a", OLD),
                since: "test",
            },
        ];
        let def = ActivityDef {
            id: ActivityId::PCSW,
            revisions: REVISIONS,
            columns: &[],
            shape: ItemShape::Single,
            item_key: "",
            has_nr: false,
        };
        for enc in all_encodings() {
            let who = label(&enc);
            let p =
                ActivityPlan::build_for_def(&def, Some(0x8a), 2, 1, 1, &enc).expect("計画を組める");
            for (raw, want) in [
                (-2i16, -2i32),
                (i16::MIN, i32::from(i16::MIN)),
                (0x7fff, 0x7fff),
            ] {
                let b = match enc.endian {
                    Endian::Little => raw.to_le_bytes(),
                    Endian::Big => raw.to_be_bytes(),
                };
                let item = Item::new(2, &enc).put(0, &b);
                let (out, _) = convert(&p, &item, 0);
                assert_eq!(rd_u32(&out, 0, &enc) as i32, want, "{who}: {raw}");
            }
        }
    }

    /// バイト列フィールドは UTF-8 として解釈せずに写し、出力側の残りは NUL で埋める。
    ///
    /// `A_FS` の 160 バイト版は fs_name が 72 バイトしかなく、mountp を持たない。
    #[test]
    fn byte_fields_are_copied_verbatim_and_nul_padded() {
        for enc in all_encodings() {
            let who = label(&enc);
            // 72 バイトを使い切る (NUL 終端の無い) 名前と、UTF-8 として不正なバイト
            let long_name = [b'x'; 72];
            let old = plan(ActivityId::FS, FS_160, 1, &enc);
            let item = Item::new(FS_160.1, &enc).u64(0, 1).put(FS_NAME, &long_name);
            let (out, _) = convert(&old, &item, 0);
            assert_eq!(&out[CUR_FS_NAME..CUR_FS_NAME + 72], &long_name[..], "{who}");
            assert!(
                out[CUR_FS_NAME + 72..CUR_FS_NAME + 128]
                    .iter()
                    .all(|&b| b == 0),
                "{who}: 旧より長い出力側は NUL で埋める"
            );
            assert!(
                out[CUR_FS_MOUNTP..CUR_FS_MOUNTP + 128]
                    .iter()
                    .all(|&b| b == 0),
                "{who}: 160 バイト版に mountp は無いので空"
            );

            let with_mount = plan(ActivityId::FS, FS_336, 1, &enc);
            let item = Item::new(FS_336.1, &enc)
                .u64(0, 1)
                .put(FS_NAME, b"/dev/\xffsda1")
                .text(FS_MOUNTP, "/mnt/data");
            let (out, _) = convert(&with_mount, &item, 0);
            assert_eq!(
                &out[CUR_FS_NAME..CUR_FS_NAME + 10],
                b"/dev/\xffsda1",
                "{who}"
            );
            assert_eq!(out[CUR_FS_NAME + 10], 0, "{who}");
            assert_eq!(
                &out[CUR_FS_MOUNTP..CUR_FS_MOUNTP + 9],
                b"/mnt/data",
                "{who}"
            );
            assert_eq!(out[CUR_FS_MOUNTP + 9], 0, "{who}");
        }
    }

    /// `A_NET_DEV` の `speed` / `duplex` は `0x8c` 版だけが持ち、それより前の版では 0 になる
    /// (本家 `upgrade_stats_net_dev()` の「New field」)。
    #[test]
    fn net_dev_speed_and_duplex_exist_only_from_the_0x8c_revision() {
        for enc in all_encodings() {
            for (magic, size, iface) in NET_DEV_OLD {
                let who = format!("magic=0x{magic:x} ({})", label(&enc));
                let p = plan(ActivityId::NET_DEV, (magic, size), 1, &enc);
                let mut item = Item::new(size, &enc).text(iface, "eth0");
                if magic == 0x8c {
                    item = item.u32(112, 1000).put(132, &[2]);
                }
                let (out, _) = convert(&p, &item, 0);
                assert_eq!(
                    &out[CUR_NET_DEV_IFACE..CUR_NET_DEV_IFACE + 5],
                    b"eth0\0",
                    "{who}"
                );
                let (speed, duplex) = if magic == 0x8c { (1000, 2) } else { (0, 0) };
                assert_eq!(rd_u32(&out, CUR_NET_DEV_SPEED, &enc), speed, "{who}");
                assert_eq!(out[CUR_NET_DEV_DUPLEX], duplex, "{who}");
            }
        }
    }

    /// 出力側の有効幅に収まらない値は切り詰めて書き、個数を数える。
    ///
    /// `A_DISK` の旧 `0x8a` 版は rd_sect / wr_sect が `unsigned long long` で、現行は
    /// `unsigned long`。32bit ファイルでは現行側の有効幅が 4 バイトしかない。
    /// tick 群は逆に `unsigned long` → `unsigned int` で、64bit ファイルで溢れうる。
    #[test]
    fn values_too_wide_for_the_output_field_are_truncated_and_counted() {
        for enc in all_encodings() {
            let who = label(&enc);
            let p = plan(ActivityId::DISK, DISK_8A, 1, &enc);
            let item = Item::new(DISK_8A.1, &enc)
                .u64(0, 0x1_0000_0005) // rd_sect
                .u64(16, 7) // wr_sect
                .ul(32, 0x2_0000_0003) // rd_ticks
                .ul(64, 9) // nr_ios
                .u32(DISK_8A_MAJOR, 8)
                .u32(DISK_8A_MINOR, 1);
            let (out, lossy) = convert(&p, &item, 0);

            let wide = enc.abi.long_bytes == 8;
            // 64bit: rd_ticks だけが 4 バイトに収まらない。
            // 32bit: rd_sect が 4 バイトに収まらない (rd_ticks は元から 4 バイトしか値を持たない)。
            assert_eq!(lossy, 1, "{who}");
            let rd_sect = if wide { 0x1_0000_0005 } else { 5 };
            assert_eq!(
                rd_ul(&out, CUR_DISK_RD_SECT, &enc),
                rd_sect,
                "{who}: rd_sect"
            );
            assert_eq!(rd_ul(&out, CUR_DISK_WR_SECT, &enc), 7, "{who}: wr_sect");
            assert_eq!(
                rd_u32(&out, CUR_DISK_RD_TICKS, &enc),
                3,
                "{who}: rd_ticks は下位 32 ビット"
            );
            assert_eq!(
                rd_u64(&out, CUR_DISK_NR_IOS, &enc),
                9,
                "{who}: nr_ios は ULL へ広がる"
            );
            assert_eq!(rd_u32(&out, CUR_DISK_MAJOR, &enc), 8, "{who}");
            assert_eq!(rd_u32(&out, CUR_DISK_MINOR, &enc), 1, "{who}");
        }
    }

    // -----------------------------------------------------------------------
    // 定義側の矛盾 (PlanError)
    // -----------------------------------------------------------------------

    /// 同名フィールドが旧新で「バイト列」と「数値」に分かれている定義は、黙って数値として
    /// 写さずにエラーにする。
    #[test]
    fn a_field_that_is_text_in_one_revision_and_numeric_in_the_other_is_rejected() {
        const TEXT: &[WireField] = &[
            WireField::natural("count", FieldTy::U32),
            WireField::natural("label", FieldTy::Bytes(8)),
        ];
        const NUMBER: &[WireField] = &[
            WireField::natural("count", FieldTy::U32),
            WireField::natural("label", FieldTy::U64),
        ];
        // 新しい順に並べる (registry の規約)。どちら向きの食い違いも拒否すること。
        const TEXT_TO_NUMBER: &[WireRevision] = &[
            WireRevision {
                magic: 0x8b,
                self_describing: true,
                types_nr: [1, 0, 1],
                size_lp64: 16,
                layout: WireLayout::new("number@0x8b", NUMBER),
                since: "test",
            },
            WireRevision {
                magic: 0x8a,
                self_describing: false,
                types_nr: [0, 0, 1],
                size_lp64: 12,
                layout: WireLayout::new("text@0x8a", TEXT),
                since: "test",
            },
        ];
        const NUMBER_TO_TEXT: &[WireRevision] = &[
            WireRevision {
                magic: 0x8b,
                self_describing: true,
                types_nr: [0, 0, 1],
                size_lp64: 12,
                layout: WireLayout::new("text@0x8b", TEXT),
                since: "test",
            },
            WireRevision {
                magic: 0x8a,
                self_describing: false,
                types_nr: [1, 0, 1],
                size_lp64: 16,
                layout: WireLayout::new("number@0x8a", NUMBER),
                since: "test",
            },
        ];
        for (revisions, size) in [(TEXT_TO_NUMBER, 12), (NUMBER_TO_TEXT, 16)] {
            let def = ActivityDef {
                id: ActivityId::PCSW,
                revisions,
                columns: &[],
                shape: ItemShape::Single,
                item_key: "",
                has_nr: false,
            };
            let err = ActivityPlan::build_for_def(&def, Some(0x8a), size, 1, 1, &le64())
                .expect_err("種別の食い違いは計画にしない");
            assert!(
                matches!(
                    err,
                    PlanError::FieldKindMismatch {
                        activity: ActivityId::PCSW,
                        field: "label"
                    }
                ),
                "{err:?}"
            );

            // 変換全体のエラーとしては「その他」に分類され、activity とフィールドを名指しする。
            match crate::Error::from(err) {
                crate::Error::Other(msg) => {
                    assert!(msg.contains("A_PCSW") && msg.contains("label"), "{msg}");
                }
                other => panic!("Error::Other になるはず: {other:?}"),
            }
        }
    }

    /// 旧 / 新どちらの revision のレイアウトが解決できなくても、
    /// `Error::Layout` として報告する (パニックしない)。
    #[test]
    fn unresolvable_layouts_are_reported_as_layout_errors() {
        const GOOD: &[WireField] = &[WireField::natural("count", FieldTy::U32)];
        // アラインメント 3 は 2 の冪ではないので解決できない
        const BROKEN: &[WireField] = &[WireField::aligned("count", FieldTy::U32, 3)];
        const BROKEN_OLD: &[WireRevision] = &[
            WireRevision {
                magic: 0x8b,
                self_describing: true,
                types_nr: [0, 0, 1],
                size_lp64: 4,
                layout: WireLayout::new("good@0x8b", GOOD),
                since: "test",
            },
            WireRevision {
                magic: 0x8a,
                self_describing: false,
                types_nr: [0, 0, 1],
                size_lp64: 4,
                layout: WireLayout::new("broken@0x8a", BROKEN),
                since: "test",
            },
        ];
        const BROKEN_NEW: &[WireRevision] = &[
            WireRevision {
                magic: 0x8b,
                self_describing: true,
                types_nr: [0, 0, 1],
                size_lp64: 4,
                layout: WireLayout::new("broken@0x8b", BROKEN),
                since: "test",
            },
            WireRevision {
                magic: 0x8a,
                self_describing: false,
                types_nr: [0, 0, 1],
                size_lp64: 4,
                layout: WireLayout::new("good@0x8a", GOOD),
                since: "test",
            },
        ];
        for revisions in [BROKEN_OLD, BROKEN_NEW] {
            let def = ActivityDef {
                id: ActivityId::PCSW,
                revisions,
                columns: &[],
                shape: ItemShape::Single,
                item_key: "",
                has_nr: false,
            };
            let err = ActivityPlan::build_for_def(&def, Some(0x8a), 4, 1, 1, &le64())
                .expect_err("解決できないレイアウトは計画にしない");
            assert!(
                matches!(
                    err,
                    PlanError::Layout(LayoutError::AlignNotPowerOfTwo { align: 3, .. })
                ),
                "{err:?}"
            );
            assert!(
                matches!(
                    crate::Error::from(err),
                    crate::Error::Layout(LayoutError::AlignNotPowerOfTwo { .. })
                ),
                "レイアウトの誤りは Error::Layout のまま伝える"
            );
        }
    }

    /// 実在の定義では、すべての revision が 4 通りの符号化で変換計画になる。
    ///
    /// 旧 revision を足したときに、同名フィールドの種別を旧新で食い違わせたり
    /// (`FieldKindMismatch`)、magic と申告サイズで引けない revision を作ったりすると、
    /// 実ファイルの変換がその activity だけ失敗 / 素通しになる。ここで先に落とす。
    #[test]
    fn every_registered_revision_can_be_planned_on_every_encoding() {
        let mut planned = 0usize;
        for def in registry::all() {
            for rev in def.revisions {
                for enc in all_encodings() {
                    let who = format!(
                        "{} magic=0x{:x} size={} ({})",
                        def.id,
                        rev.magic,
                        rev.size_lp64,
                        label(&enc)
                    );
                    let p = ActivityPlan::build(def.id, Some(rev.magic), rev.size_lp64, 1, 1, &enc)
                        .unwrap_or_else(|e| panic!("{who}: {e}"));
                    assert!(p.known, "{who}: 定義済みの revision が素通しになった");
                    assert_eq!(p.src_stride, rev.size_lp64, "{who}: ストライドは申告サイズ");
                    assert_eq!(p.has_nr, def.has_nr, "{who}");
                    planned += 1;
                }
            }
        }
        assert!(planned > 0);
    }
}

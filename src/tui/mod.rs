//! TUI 閲覧モード。
//!
//! # 層の位置づけ
//!
//! **ここで値を計算しない。** 表示するのは `series` が確定させ、
//! `output::json` のデータモデルへ写された観測値だけである
//! (`CLAUDE.md` の「出力層で値を計算しない」と同じ規律)。
//! 平均・割合・単位換算をこの層で作ると、同じ指標が TUI と `show` で
//! 食い違う — 層を分けて防いでいるのはまさにその事故である。
//!
//! `output` 層と違うのは、**状態と入力処理を持つ**点だけ。
//!
//! | 持ってよい状態 | 持ってはいけない状態 |
//! |---|---|
//! | 選択中の activity / item / 時刻、スクロール位置、絞り込み文字列 | 値そのものの加工結果 (丸め・換算・集計) |
//!
//! # 値を読み違えさせないための規律
//!
//! - **欠測とゼロを混同しない。** 値が無いセルは `—` で表し、
//!   その理由 (`Quality`) を選択時に表示する。
//! - **点を線で結ばない。** 不連続 (再起動・欠測・item の入れ替え) をまたぐ区間は
//!   表で印を付ける。採取と採取の間に何が起きたかは観測されていない。
//! - **時刻の基準を画面に明記する** (`--timezone`、既定はローカル)。独自出力の既定と揃える。
//! - **グラフも同じ規律で描く。** 折れ線は不連続と欠測のところで切り、
//!   飛び越えて結ばない。欠測を 0 に写さない (`graph` モジュールを参照)。
//! - **比較グラフも列を重ねるだけ。** MEMORY の既定は使用率 3 通りの比較
//!   ([`graph::MEMORY_USAGE`]) だが、線はどれも `series` 層の列そのもので、
//!   差や和の線をここで作らない。値の無い系列は凡例から消さずに理由を書く。
//! - **表の下の列の説明は式と意味だけを書く** ([`notes`])。数値は表から読み、
//!   説明のために値を組み立て直さない。説明のために表を潰さない (`note_layout`)。

pub mod graph;
pub mod notes;

use std::path::Path;

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols;
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Axis, Block, Cell, Chart, Clear, Dataset, GraphType, List, ListItem, ListState, Paragraph, Row,
    Table, TableState, Tabs,
};
use ratatui::{Frame, Terminal, prelude::Backend};

use crate::error::Result;
use crate::format::{SaFile, ScanControl};
use crate::model::DisplayTz;
use crate::output::json::{BootCounter, CustomConfig, FieldOut, HostOut, Quality, SampleOut};
use crate::series::{RecordEvent, WalkItem, walk_items};

/// 値が無いことを表す記号。**0 とは別物**。
const ABSENT: &str = "—";

/// グラフの観測値の線の色。
///
/// **表の見出しと共有する。** グラフに描いている列がどれなのかを、
/// 上の線と下の列名で同じ色にして結びつける。
const GRAPH_LINE_COLOR: Color = Color::Cyan;

/// 表で選んでいる時刻を指すカーソルの色。
///
/// 観測値の線と補色に近いものを選ぶ。赤と緑の対比は避ける
/// (色覚によっては区別できない)。
const GRAPH_CURSOR_COLOR: Color = Color::Red;

// ===========================================================================
// 収集
// ===========================================================================

/// 走査中に見つけた注記 (再起動・コメント)。
///
/// 統計そのものではないが、**値を読み違えないために要る**情報なので
/// 検知機能とは独立に初版から持つ。
struct Mark {
    epoch: u64,
    kind: &'static str,
    text: String,
}

/// 表示に使う観測値一式。
struct Collected {
    host: HostOut,
    samples: Vec<SampleOut>,
    marks: Vec<Mark>,
    /// 時刻の表示に使うタイムゾーン (`--timezone`、既定はローカル)。
    tz: DisplayTz,
}

/// 端末を初期化する前に得る観測値と走査結果。
pub struct Prepared {
    collected: Collected,
    pub scan_summary: crate::format::ScanSummary,
}

/// ファイルを 1 回走査して観測値を集める。
///
/// 描画のたびにファイルを読まない。静的なファイルなので、
/// 開いた時点の内容がすべてである。
pub fn prepare(file: &SaFile, cfg: &CustomConfig) -> Result<Prepared> {
    let host = HostOut::new(file, cfg.tz);
    let mut samples = Vec::new();
    let mut marks = Vec::new();
    let mut boot = BootCounter::default();

    let scan_summary = walk_items(file, &cfg.selection.clone(), |item| {
        match item {
            WalkItem::Event(ev) => {
                boot.advance(std::slice::from_ref(&ev));
                let (kind, text) = match &ev {
                    RecordEvent::Restart { cpu_count, .. } => (
                        "restart",
                        match cpu_count {
                            Some(n) => format!("再起動 (CPU {})", n.saturating_sub(1).max(1)),
                            None => "再起動".to_string(),
                        },
                    ),
                    RecordEvent::Comment { text, .. } => ("comment", text.clone()),
                };
                marks.push(Mark {
                    epoch: ev.ust_time(),
                    kind,
                    text,
                });
            }
            WalkItem::Sample(view) => {
                samples.push(crate::output::json::sample_out(view, boot.get(), cfg));
            }
        }
        Ok(ScanControl::Continue)
    })?;

    Ok(Prepared {
        collected: Collected {
            host,
            samples,
            marks,
            tz: cfg.tz,
        },
        scan_summary,
    })
}

// ===========================================================================
// 画面の状態
// ===========================================================================

/// 入力モード。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// 通常。キーは操作として解釈する。
    Normal,
    /// item 選択のポップアップ。
    PickItem,
    /// 列を選ぶポップアップ。
    ///
    /// **表に出す列とグラフに描く列を 1 つの画面で扱う。** 別々のキーへ
    /// 割り当てると「どちらが表でどちらがグラフか」を覚える羽目になる。
    PickColumn,
    /// 絞り込み入力中。Esc で取り消して通常画面に戻る。
    Filter,
    /// ヘルプ。
    Help,
}

/// グラフを出すかどうか。
///
/// 単なる `bool` にしないのは、「まだ何も指定していない」と
/// 「利用者が明示的に隠した」を区別するため。前者は端末の高さに任せ、
/// 後者は広い端末でも隠したままにする。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum GraphVisibility {
    /// 端末の高さに任せる (既定)。
    #[default]
    Auto,
    /// 明示的に出す。
    Shown,
    /// 明示的に隠す。
    Hidden,
}

/// グラフに描く対象。
///
/// **添字ではなく名前で持つ。** item を切り替えると列の顔ぶれが変わるので、
/// 添字では「別の列に化ける」。
#[derive(Debug, Clone, Copy, PartialEq)]
enum GraphTarget {
    /// 1 列を 1 本の線で描く。
    Column(&'static str),
    /// 決まった列の組を同じ軸に重ねる (比較グラフ)。
    Preset(&'static graph::GraphPreset),
}

/// 比較グラフの凡例で、系列の色を示す印。
///
/// 線と同じ Braille の全点を使う。`■` や `█` は East Asian Width が曖昧で、
/// 日本語環境の端末では 2 桁に描かれて凡例の位置がずれる。
const LEGEND_MARK: &str = "⣿";

/// グラフに割く行数。0 なら出さない。
///
/// **表よりグラフを優先しない。** グラフを出したせいで表が 2〜3 行しか
/// 見えなくなると、時刻を追う操作ができなくなって閲覧の用をなさない。
/// 低い端末では既定で出さず、明示されたときだけ最小限の高さを渡す。
fn graph_height(vis: GraphVisibility, total: u16) -> u16 {
    // ヘッダ 2 + タブ 1 + ヒント 1 + 表の枠と 3 行 = 9 行は譲れない。
    match vis {
        GraphVisibility::Hidden => 0,
        // 明示されても、表が潰れる高さでは出さない。
        GraphVisibility::Shown if total < 18 => 0,
        GraphVisibility::Shown if total < 25 => 6,
        GraphVisibility::Auto if total < 25 => 0,
        _ if total < 32 => 7,
        _ => (total / 3).clamp(9, 14),
    }
}

/// 表の下の列の説明を、どの形で出すか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NoteLayout {
    /// 出さない (説明する列が表に無い、または表が潰れる)。
    None,
    /// 1 行の要約。列ごとの式は `?` のヘルプで読む。
    Summary,
    /// 列ごとに 1 行 + 注意 1 行。
    Full,
}

/// 説明欄を置いても表に残す行数 (枠 2 + 見出し 1 + 5 行)。
///
/// **説明より表を優先する。** 説明のために表が 2〜3 行になると、
/// 時刻を追って値を読むという本来の用ができなくなる。
const TABLE_KEEP: u16 = 8;

/// 説明欄の形を決める。
///
/// - `notes`: 説明する列のうち表に出ている数
/// - `left`: 表と説明欄に使える行数 (ヘッダ・タブ・グラフ・ヒントを引いた残り)
/// - `fits_width`: 列ごとの説明が (短い版で) 1 行に収まる幅か
///
/// **列名と説明の途中で切らない。** 1 行に収まらない幅では、切れた説明を並べる
/// 代わりに 1 行の要約で `?` のヘルプへ案内する (列名と説明の対応が崩れると
/// 読み違えるため)。
fn note_layout(notes: usize, left: u16, fits_width: bool) -> NoteLayout {
    if notes == 0 {
        return NoteLayout::None;
    }
    let full = notes as u16 + 1;
    if fits_width && left >= TABLE_KEEP + full {
        NoteLayout::Full
    } else if left > TABLE_KEEP {
        NoteLayout::Summary
    } else {
        NoteLayout::None
    }
}

/// 説明欄に使う行数。
fn note_rows(layout: NoteLayout, notes: usize) -> u16 {
    match layout {
        NoteLayout::None => 0,
        NoteLayout::Summary => 1,
        NoteLayout::Full => notes as u16 + 1,
    }
}

/// タブ 1 つ = activity 1 種。
struct TabInfo {
    /// `A_CPU` などの本家シンボル名。
    name: &'static str,
    /// 画面に出す名称。
    label: &'static str,
}

struct App {
    host: HostOut,
    samples: Vec<SampleOut>,
    marks: Vec<Mark>,
    /// 時刻の表示に使うタイムゾーン。画面にも明記する。
    tz: DisplayTz,
    /// 完全な先行レコードだけを表示しているか。
    incomplete: bool,
    tabs: Vec<TabInfo>,
    /// 選択中のタブ。
    tab: usize,
    /// タブごとの選択 item。タブを切り替えても選択を保つ。
    item: Vec<usize>,
    /// タブごとのグラフ対象。タブを切り替えても選択を保つ。
    ///
    /// `None` は「まだ選んでいない」で、そのときは [`App::default_target`] に従う。
    graph_target: Vec<Option<GraphTarget>>,
    /// タブごとの「表に出す列」。
    ///
    /// `None` は既定 = **全時刻で 1 度も値が出なかった列を隠す**。
    /// その世代に無いフィールドや未実装の列は全行が `—` になり、
    /// 読みたい列を画面の外へ押し出すだけなので既定では出さない。
    /// `Some` は利用者が明示的に選んだ列 (順序は元の列順)。
    shown_cols: Vec<Option<Vec<&'static str>>>,
    /// 列ピッカーで編集中の選択 (確定するまで表へは反映しない)。
    col_draft: Vec<&'static str>,
    /// タブごとの横スクロール位置 (表に出す列のうち、左端に置く列の添字)。
    ///
    /// **`time` 列はスクロールしない。** どの行を見ているかを見失うため。
    col_offset: Vec<usize>,
    /// グラフを出すか。
    graph: GraphVisibility,
    /// 直近の描画で使った端末の高さ。`v` の判定に使う。
    last_height: u16,
    /// 直近の描画で表に出せた列数。横スクロールの上限に使う。
    ///
    /// 何列入るかは幅と列幅で決まり、描画時にしか分からない。
    visible_cols: usize,
    table: TableState,
    picker: ListState,
    /// 列ピッカーの選択位置 (`columns()` の添字)。
    col_picker: ListState,
    /// 列ピッカーを開いたときのカーソル位置。
    ///
    /// カーソルを動かさずに確定したら、グラフの対象を変えないために使う。
    col_picker_origin: Option<usize>,
    mode: Mode,
    filter: String,
    /// 終了要求。
    quit: bool,
}

impl App {
    fn new(c: Collected) -> Self {
        // タブは「実際に観測値が出た activity」だけを、最初に現れた順で並べる。
        // 収録されていない activity のタブを出すと、空の画面を見せることになる。
        let mut tabs: Vec<TabInfo> = Vec::new();
        for s in &c.samples {
            for a in &s.activities {
                if !tabs.iter().any(|t| t.name == a.activity) {
                    tabs.push(TabInfo {
                        name: a.activity,
                        label: a.label,
                    });
                }
            }
        }
        let item = vec![0; tabs.len()];
        let graph_target = vec![None; tabs.len()];
        let col_offset = vec![0; tabs.len()];
        let shown_cols = vec![None; tabs.len()];
        let mut table = TableState::default();
        table.select(Some(0));
        App {
            host: c.host,
            samples: c.samples,
            marks: c.marks,
            tz: c.tz,
            incomplete: false,
            graph_target,
            shown_cols,
            col_draft: Vec::new(),
            col_offset,
            graph: GraphVisibility::default(),
            last_height: 0,
            visible_cols: 1,
            col_picker: ListState::default(),
            col_picker_origin: None,
            tabs,
            tab: 0,
            item,
            table,
            picker: ListState::default(),
            mode: Mode::Normal,
            filter: String::new(),
            quit: false,
        }
    }

    fn current_activity(&self) -> Option<&'static str> {
        self.tabs.get(self.tab).map(|t| t.name)
    }

    /// 選択中 activity の item 名を、出現順に重複なく集める。
    ///
    /// item は時刻によって増減しうる (ディスクの着脱など)。
    /// **添字を item の同一性の根拠にしない** — 名前で対応させる。
    fn item_names(&self) -> Vec<String> {
        let Some(act) = self.current_activity() else {
            return Vec::new();
        };
        let mut names: Vec<String> = Vec::new();
        for s in &self.samples {
            for a in s.activities.iter().filter(|a| a.activity == act) {
                for it in &a.items {
                    let name = display_item(&it.item, it.cpu.as_deref());
                    if !names.contains(&name) {
                        names.push(name);
                    }
                }
            }
        }
        names
    }

    /// 絞り込みを適用した item 名。
    fn filtered_items(&self) -> Vec<String> {
        let all = self.item_names();
        if self.filter.is_empty() {
            return all;
        }
        let needle = self.filter.to_lowercase();
        all.into_iter()
            .filter(|n| n.to_lowercase().contains(&needle))
            .collect()
    }

    fn selected_item_name(&self) -> Option<String> {
        let items = self.filtered_items();
        if items.is_empty() {
            return None;
        }
        let i = self.item.get(self.tab).copied().unwrap_or(0);
        Some(items[i.min(items.len() - 1)].clone())
    }

    /// 表の列名。選択中 activity / item の最初のサンプルから取る。
    fn columns(&self) -> Vec<&'static str> {
        let Some(act) = self.current_activity() else {
            return Vec::new();
        };
        let Some(want) = self.selected_item_name() else {
            return Vec::new();
        };
        for s in &self.samples {
            for a in s.activities.iter().filter(|a| a.activity == act) {
                for it in &a.items {
                    if display_item(&it.item, it.cpu.as_deref()) == want {
                        return visible_fields(it).map(|f| f.name).collect();
                    }
                }
            }
        }
        Vec::new()
    }

    /// 選択中の区間にかかる注記 (再起動・コメント)。
    fn note_at_selection(&self) -> Option<&Mark> {
        let i = self.table.selected()?;
        let s = self.samples.get(i)?;
        self.marks
            .iter()
            .find(|m| m.epoch > s.start_epoch && m.epoch <= s.end_epoch)
    }

    fn move_row(&mut self, delta: isize) {
        if self.samples.is_empty() {
            return;
        }
        let cur = self.table.selected().unwrap_or(0) as isize;
        let last = self.samples.len() as isize - 1;
        self.table
            .select(Some(cur.saturating_add(delta).clamp(0, last) as usize));
    }

    fn move_tab(&mut self, delta: isize) {
        if self.tabs.is_empty() {
            return;
        }
        let n = self.tabs.len() as isize;
        self.tab = (((self.tab as isize + delta) % n + n) % n) as usize;
        // タブごとに絞り込みは持ち越さない (別 activity では意味が違う)。
        self.filter.clear();
    }

    fn move_item(&mut self, delta: isize) {
        let n = self.filtered_items().len();
        if n == 0 {
            return;
        }
        let slot = self.item.get_mut(self.tab);
        let Some(slot) = slot else { return };
        let cur = *slot as isize;
        *slot = (((cur + delta) % n as isize + n as isize) % n as isize) as usize;
        // item が変われば列の顔ぶれも変わる。横位置を持ち越すと、
        // 前の item の 5 列目に当たる場所から見せることになって意味がない。
        if let Some(off) = self.col_offset.get_mut(self.tab) {
            *off = 0;
        }
    }

    /// 全サンプルを通して 1 度でも値が出た列。
    ///
    /// **「その世代に無い」「未実装」の列は全行が `—` になる。**
    /// MEMORY のように列の多い activity でそれが何列もあると、読みたい列が
    /// 画面の外へ出てしまう。隠した列は消したのではなく、`c` で出せることを
    /// タイトルに書く。
    fn measured_columns(&self) -> Vec<&'static str> {
        let all = self.columns();
        let (Some(act), Some(want)) = (self.current_activity(), self.selected_item_name()) else {
            return all;
        };
        all.into_iter()
            .filter(|name| {
                self.samples.iter().any(|s| {
                    graph::find_field(s, act, &want, name)
                        .is_some_and(|f| f.value.is_some() || f.text.is_some() || f.raw.is_some())
                })
            })
            .collect()
    }

    /// 表に出す列。
    fn table_columns(&self) -> Vec<&'static str> {
        match self.shown_cols.get(self.tab).and_then(|c| c.as_ref()) {
            Some(chosen) => {
                // 元の列順を保つ (選んだ順に並べ替えると読みにくい)。
                let all = self.columns();
                all.into_iter().filter(|c| chosen.contains(c)).collect()
            }
            None => self.measured_columns(),
        }
    }

    /// 各列の表示幅。
    ///
    /// **列名の長さだけで決めない。** `kbmemfree` のような列は値が
    /// 8 桁まで伸びるので、列名に合わせると値が切れる。逆に値が短い列に
    /// 一律 8 桁を与えると、その分だけ他の列が画面から押し出される。
    /// 全サンプルを見て決めるので、行をスクロールしても幅が揺れない。
    fn column_widths(&self, cols: &[&'static str]) -> Vec<u16> {
        let (Some(act), Some(want)) = (self.current_activity(), self.selected_item_name()) else {
            return cols.iter().map(|c| c.chars().count() as u16).collect();
        };
        cols.iter()
            .map(|name| {
                let widest = self
                    .samples
                    .iter()
                    .map(|s| {
                        let f = graph::find_field(s, act, &want, name);
                        cell_text(f).chars().count()
                    })
                    .max()
                    .unwrap_or(0);
                (name.chars().count().max(widest) as u16).max(3)
            })
            .collect()
    }

    /// 列ピッカーを開く (いまの表示列を下書きに写す)。
    fn open_column_picker(&mut self) {
        self.col_draft = self.table_columns();
        // カーソルは**いまグラフに出ている列**に合わせる。比較グラフなら
        // その先頭の (値のある) 系列に合わせる。
        let all = self.columns();
        let focus = match self.selected_target() {
            Some(GraphTarget::Column(c)) => Some(c),
            Some(GraphTarget::Preset(p)) => self.preset_measured(p).first().copied(),
            None => None,
        };
        let pos = focus
            .and_then(|c| all.iter().position(|n| *n == c))
            .unwrap_or(0);
        self.col_picker.select(Some(pos));
        self.col_picker_origin = Some(pos);
        self.mode = Mode::PickColumn;
    }

    /// 下書きの列を 1 つ出し入れする。
    fn toggle_draft_column(&mut self) {
        let all = self.columns();
        let Some(name) = self.col_picker.selected().and_then(|i| all.get(i)).copied() else {
            return;
        };
        match self.col_draft.iter().position(|c| *c == name) {
            Some(i) => {
                self.col_draft.remove(i);
            }
            None => self.col_draft.push(name),
        }
    }

    /// 下書きを「全部」と「値が出た列だけ」で切り替える。
    fn toggle_draft_all(&mut self) {
        let all = self.columns();
        if self.col_draft.len() == all.len() {
            self.col_draft = self.measured_columns();
        } else {
            self.col_draft = all;
        }
    }

    /// 下書きを表へ反映する。
    ///
    /// 既定 (値が出た列だけ) と同じ内容なら `None` に戻す。そうしないと、
    /// item を切り替えて列の顔ぶれが変わったとき、古い選択に縛られる。
    fn commit_columns(&mut self) {
        let draft = std::mem::take(&mut self.col_draft);
        let slot_is_default = draft == self.measured_columns();
        if let Some(slot) = self.shown_cols.get_mut(self.tab) {
            *slot = if slot_is_default { None } else { Some(draft) };
        }
        // 列を選び直したら左端へ戻す (選んだ列が画面の外だと選んだ実感がない)。
        if let Some(off) = self.col_offset.get_mut(self.tab) {
            *off = 0;
        }
        // **カーソルを動かさなければグラフは変えない** (「表の列だけ直したら
        // グラフまで変わった」を防ぐ)。比較グラフを出したまま表の列を直したときに、
        // カーソルが置かれていた系列の 1 列だけのグラフへ切り替わらないようにする。
        let cursor = self.col_picker.selected();
        let origin = self.col_picker_origin.take();
        if cursor.is_some() && origin == cursor {
            return;
        }
        // カーソル位置の列をグラフ対象にする。ただし
        // **描けない列 (識別子など) と、いま表から外した列は対象にしない。**
        // 外した列をグラフへ回すと、`[` / `]` の巡回からも外れているのに
        // グラフにだけ出ている、という辻褄の合わない状態になる。
        let all = self.columns();
        let Some(name) = cursor.and_then(|i| all.get(i)).copied() else {
            return;
        };
        if self.plottable_columns().contains(&name)
            && self.table_columns().contains(&name)
            && let Some(slot) = self.graph_target.get_mut(self.tab)
        {
            *slot = Some(GraphTarget::Column(name));
        }
    }

    fn move_col_picker(&mut self, delta: isize) {
        let n = self.columns().len();
        if n == 0 {
            return;
        }
        let cur = self.col_picker.selected().unwrap_or(0) as isize;
        let next = (((cur + delta) % n as isize) + n as isize) % n as isize;
        self.col_picker.select(Some(next as usize));
    }

    /// グラフに描ける列。**数値でない列は除く** (デバイス名などは線にならない)。
    fn plottable_columns(&self) -> Vec<&'static str> {
        let Some(act) = self.current_activity() else {
            return Vec::new();
        };
        let Some(want) = self.selected_item_name() else {
            return Vec::new();
        };
        for s in &self.samples {
            for a in s.activities.iter().filter(|a| a.activity == act) {
                for it in &a.items {
                    if display_item(&it.item, it.cpu.as_deref()) == want {
                        return visible_fields(it)
                            .filter(|f| graph::is_plottable(f))
                            .map(|f| f.name)
                            .collect();
                    }
                }
            }
        }
        Vec::new()
    }

    /// グラフに描く対象。描けるものが無ければ `None`。
    fn selected_target(&self) -> Option<GraphTarget> {
        match self.graph_target.get(self.tab).copied().flatten() {
            // 選んだ列が今の item に無ければ (デバイスを替えた等)、既定へ戻す。
            Some(GraphTarget::Column(name)) if self.plottable_columns().contains(&name) => {
                Some(GraphTarget::Column(name))
            }
            Some(GraphTarget::Preset(p)) if self.preset() == Some(p) => {
                Some(GraphTarget::Preset(p))
            }
            _ => self.default_target(),
        }
    }

    /// 1 列だけのグラフに描いている列。比較グラフのときは `None`。
    #[cfg(test)]
    fn selected_column(&self) -> Option<&'static str> {
        match self.selected_target()? {
            GraphTarget::Column(c) => Some(c),
            GraphTarget::Preset(_) => None,
        }
    }

    /// まだ選んでいないときのグラフ。
    ///
    /// 比較グラフがあればそれを先に使う ([`graph::MEMORY_USAGE`] の理由を参照)。
    /// 無ければ値の出る最初の列 ([`App::default_graph_column`])。
    fn default_target(&self) -> Option<GraphTarget> {
        self.preset()
            .map(GraphTarget::Preset)
            .or_else(|| self.default_graph_column().map(GraphTarget::Column))
    }

    /// この item で出せる比較グラフ。
    ///
    /// **系列の 1 本にも値が出ないなら出さない。** 空のグラフを既定にすると、
    /// タブを開いた瞬間に「描画できる観測がありません」を見せることになる。
    fn preset(&self) -> Option<&'static graph::GraphPreset> {
        let p = graph::preset_for(self.current_activity()?)?;
        (!self.preset_measured(p).is_empty()).then_some(p)
    }

    /// 比較グラフの系列のうち、1 度でも値が出た列 (凡例の順)。
    fn preset_measured(&self, p: &graph::GraphPreset) -> Vec<&'static str> {
        let measured = self.measured_columns();
        let plottable = self.plottable_columns();
        p.series
            .iter()
            .map(|s| s.column)
            .filter(|c| measured.contains(c) && plottable.contains(c))
            .collect()
    }

    /// 表の見出しに付けるグラフの線の色。グラフが出ていなければ空。
    ///
    /// **線の無い系列には色を付けない。** 付けると、線が無いのに
    /// 「グラフに描いている列」のように見える。
    fn graph_header_colors(&self) -> Vec<(&'static str, Color)> {
        if !self.graph_visible() {
            return Vec::new();
        }
        match self.selected_target() {
            Some(GraphTarget::Column(c)) => vec![(c, GRAPH_LINE_COLOR)],
            Some(GraphTarget::Preset(p)) => {
                let measured = self.preset_measured(p);
                p.series
                    .iter()
                    .filter(|s| measured.contains(&s.column))
                    .map(|s| (s.column, s.color))
                    .collect()
            }
            None => Vec::new(),
        }
    }

    /// 表の下に説明を出す列 (表に出ている列だけ、表の列順)。
    ///
    /// `c` で表から外した列の説明は出さない。表に無い列の説明が並ぶと、
    /// どの列の話なのかを表で確かめられない。
    fn shown_notes(&self) -> Option<(&'static notes::NoteSet, Vec<&'static notes::ColumnNote>)> {
        let set = notes::notes_for(self.current_activity()?)?;
        let shown: Vec<&'static notes::ColumnNote> = self
            .table_columns()
            .into_iter()
            .filter_map(|c| set.notes.iter().find(|n| n.column == c))
            .collect();
        (!shown.is_empty()).then_some((set, shown))
    }

    /// まだ選んでいないときにグラフへ出す列。
    ///
    /// **値の出る列を先に探す。** 単に先頭を採ると、その列がその世代に無いだけで
    /// タブを開いた瞬間「描画できる観測がありません」に当たる。
    /// 値の出る列が 1 つも無ければ、先頭を返して理由を出させる。
    fn default_graph_column(&self) -> Option<&'static str> {
        let plottable = self.plottable_columns();
        let measured = self.measured_columns();
        plottable
            .iter()
            .find(|c| measured.contains(c))
            .or(plottable.first())
            .copied()
    }

    /// `[` / `]` で順に送る列。
    ///
    /// **表に出していない列は飛ばす。** 画面に無い列へ移ると、グラフの見出しだけが
    /// 変わって、どの列を見ているのかを表で確かめられない。
    fn steppable_columns(&self) -> Vec<&'static str> {
        let shown = self.table_columns();
        self.plottable_columns()
            .into_iter()
            .filter(|c| shown.contains(c))
            .collect()
    }

    /// `[` / `]` の巡回。比較グラフがあれば先頭に置き、続けて 1 列ずつ。
    ///
    /// 比較グラフを別のキーに割り当てないのは、「グラフに何を描くかを送る」
    /// という同じ操作だから。キーを増やすと、どちらで何が変わるかを覚える羽目になる。
    fn graph_cycle(&self) -> Vec<GraphTarget> {
        let mut cycle: Vec<GraphTarget> =
            self.preset().map(GraphTarget::Preset).into_iter().collect();
        cycle.extend(
            self.steppable_columns()
                .into_iter()
                .map(GraphTarget::Column),
        );
        cycle
    }

    fn move_col(&mut self, delta: isize) {
        let cycle = self.graph_cycle();
        let n = cycle.len();
        if n == 0 {
            return;
        }
        let cur = self
            .selected_target()
            .and_then(|t| cycle.iter().position(|c| *c == t));
        let next = match cur {
            Some(i) => (((i as isize + delta) % n as isize + n as isize) % n as isize) as usize,
            // いま見ている列が巡回の対象外 (表から外した列) なら端から入る。
            None if delta >= 0 => 0,
            None => n - 1,
        };
        if let Some(slot) = self.graph_target.get_mut(self.tab) {
            *slot = Some(cycle[next]);
        }
    }

    /// 表を横に送る。
    ///
    /// **端では止める。** 巻き戻すと「一番右まで見た」ことが分からなくなる
    /// (縦方向と違い、横は全体像を掴みながら読む動きなので位置が意味を持つ)。
    fn scroll_cols(&mut self, delta: isize, visible: usize) {
        let total = self.table_columns().len();
        // 右端は「最後の列が右に出る位置」まで。それ以上送っても空白が増えるだけ。
        let max = total.saturating_sub(visible.max(1));
        let Some(slot) = self.col_offset.get_mut(self.tab) else {
            return;
        };
        let next = (*slot as isize + delta).clamp(0, max as isize);
        *slot = next as usize;
    }

    /// いまの横スクロール位置 (列が減っていれば切り詰める)。
    fn col_offset(&self) -> usize {
        let total = self.table_columns().len();
        self.col_offset
            .get(self.tab)
            .copied()
            .unwrap_or(0)
            .min(total.saturating_sub(1))
    }

    /// いま実際にグラフが出ているか。
    fn graph_visible(&self) -> bool {
        graph_height(self.graph, self.last_height) > 0
    }

    /// グラフの表示を切り替える。
    ///
    /// 「いま見えているか」を基準に反転する。`Auto` で出ていない低い端末では
    /// `Shown` にして (出せる高さなら) 出す。
    fn toggle_graph(&mut self) {
        self.graph = if self.graph_visible() {
            GraphVisibility::Hidden
        } else {
            GraphVisibility::Shown
        };
    }
}

/// IRQ のように CPU 次元を持つ item は、名前に次元を足して区別する。
fn display_item(item: &str, cpu: Option<&str>) -> String {
    match cpu {
        Some(c) => format!("{item}@{c}"),
        None => item.to_string(),
    }
}

/// 表に出すフィールド。
///
/// 派生値があればそれを、無ければ生値を出す。`identity` (デバイス名など) は
/// item の識別に使うだけで、時系列の列としては出さない。
fn visible_fields(it: &crate::output::json::ItemOut) -> impl Iterator<Item = &FieldOut> {
    let src = if it.rates.is_empty() {
        &it.raw
    } else {
        &it.rates
    };
    src.iter().filter(|f| f.kind != "identity")
}

/// セルの表示文字列。**値が無いことを 0 と書かない。**
fn cell_text(f: Option<&FieldOut>) -> String {
    let Some(f) = f else {
        return ABSENT.to_string();
    };
    if let Some(t) = &f.text {
        return t.clone();
    }
    if let Some(v) = f.value {
        return format_number(v);
    }
    if let Some(r) = &f.raw {
        return r.clone();
    }
    ABSENT.to_string()
}

fn format_number(v: f64) -> String {
    if !v.is_finite() {
        return ABSENT.to_string();
    }
    let a = v.abs();
    if a >= 100_000.0 {
        format!("{v:.0}")
    } else if a >= 1000.0 {
        format!("{v:.1}")
    } else {
        format!("{v:.2}")
    }
}

fn quality_style(q: Quality) -> Style {
    match q {
        Quality::Ok => Style::default(),
        _ => Style::default().fg(Color::DarkGray),
    }
}

/// 表示用の `HH:MM:SS`。
///
/// **`epoch % 86_400` で日内秒を出さない。** それは UTC 固定の時刻であり、
/// 画面のタイムゾーン表記と食い違う。
fn hhmmss(tz: DisplayTz, epoch: u64) -> String {
    tz.time(epoch)
}

// ===========================================================================
// 描画
// ===========================================================================

fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    // `v` の判定に使うので、描画のたびに実際の高さを控える。
    app.last_height = area.height;
    let gh = graph_height(app.graph, area.height);
    // 説明欄は、グラフを決めたあとの残り (表と説明欄の分) から取る。
    // グラフの高さは説明欄の有無で変えない (タブを替えるたびにグラフが伸び縮みしない)。
    let shown_notes = app.shown_notes();
    let left = area.height.saturating_sub(2 + 1 + 1 + gh);
    let (layout, n_notes) = match &shown_notes {
        Some((set, list)) => (
            note_layout(list.len(), left, notes_fit_width(set, list, area.width)),
            list.len(),
        ),
        None => (NoteLayout::None, 0),
    };
    let nh = note_rows(layout, n_notes);
    let chunks = Layout::vertical([
        Constraint::Length(2),  // ヘッダ
        Constraint::Length(1),  // タブ
        Constraint::Length(gh), // グラフ (0 なら出ない)
        Constraint::Min(3),     // 表
        Constraint::Length(nh), // 列の説明 (0 なら出ない)
        Constraint::Length(1),  // キーヒント
    ])
    .split(area);

    draw_header(f, chunks[0], app);
    draw_tabs(f, chunks[1], app);
    if gh > 0 {
        draw_graph(f, chunks[2], app);
    }
    draw_table(f, chunks[3], app);
    if let Some((set, list)) = &shown_notes
        && nh > 0
    {
        draw_notes(f, chunks[4], app, set, list, layout);
    }
    draw_hint(f, chunks[5], app);

    match app.mode {
        Mode::PickItem => draw_picker(f, area, app),
        Mode::PickColumn => draw_column_picker(f, area, app),
        Mode::Help => draw_help(f, area),
        _ => {}
    }
}

/// グラフに重ねる 1 系列。
struct Plot {
    view: graph::GraphView,
    color: Color,
    /// 凡例の名前 (1 列だけのグラフでは凡例を出さない)。
    label: &'static str,
    /// 幅が足りないときの凡例の名前。
    short: &'static str,
}

/// 選択中の系列を折れ線で描く (1 列、または比較グラフの複数列)。
///
/// **不連続と欠測ごとに `Dataset` を分ける。** 1 本の点列に混ぜて
/// 飛び越えた線を引くと、観測していない区間をあたかも観測したかのように見せる。
/// 比較グラフでも系列ごとに同じ規律で切る。
fn draw_graph(f: &mut Frame, area: Rect, app: &App) {
    let (Some(act), Some(item)) = (app.current_activity(), app.selected_item_name()) else {
        return;
    };
    let Some(target) = app.selected_target() else {
        f.render_widget(
            Paragraph::new("グラフに描ける数値の列がありません")
                .style(Style::default().fg(Color::DarkGray))
                .block(Block::bordered().title(" グラフ ")),
            area,
        );
        return;
    };
    let (name, plots): (&str, Vec<Plot>) = match target {
        GraphTarget::Column(col) => (
            col,
            vec![Plot {
                view: graph::GraphView::build(&app.samples, act, &item, col),
                color: GRAPH_LINE_COLOR,
                label: col,
                short: col,
            }],
        ),
        GraphTarget::Preset(p) => (
            p.title,
            p.series
                .iter()
                .map(|s| Plot {
                    view: graph::GraphView::build(&app.samples, act, &item, s.column),
                    color: s.color,
                    label: s.label,
                    short: s.short,
                })
                .collect(),
        ),
    };

    let unit = plots
        .iter()
        .map(|p| p.view.unit)
        .find(|u| !u.is_empty())
        .map(|u| format!(" ({u})"))
        .unwrap_or_default();
    let title = format!(" {act} / {item} / {name}{unit} ");

    if plots.iter().all(|p| p.view.is_empty()) {
        // **空の軸を出さない。** 「描けなかった」ことと「なぜか」を書く。
        // 比較グラフでは理由を系列ごとに出す (1 つへ丸めない)。
        let mut lines = vec![Line::from("この範囲に描画できる観測がありません")];
        for p in plots.iter().filter(|p| !p.view.absent.is_empty()) {
            let reason = if plots.len() == 1 {
                p.view.absent_summary()
            } else {
                format!("{}: {}", p.label, p.view.absent_summary())
            };
            lines.push(Line::from(Span::styled(
                reason,
                Style::default().fg(Color::DarkGray),
            )));
        }
        f.render_widget(
            Paragraph::new(lines)
                .centered()
                .block(Block::bordered().title(title)),
            area,
        );
        return;
    }

    // 系列は同じサンプル列から作るので、X の原点と範囲は共通。
    // Y は全系列の点をまとめて 1 本の軸にする (系列ごとに軸を持たない)。
    let (x_origin, x_bounds) = (plots[0].view.x_origin, plots[0].view.x_bounds);
    let views: Vec<&graph::GraphView> = plots.iter().map(|p| &p.view).collect();
    let y_bounds = graph::merged_y_bounds(&views);

    // 表で選んでいる時刻に縦線を立てる。表とグラフが同じ時刻を指していることを
    // 見せるため。**値が欠測の時刻でも線は立つ** (1 点の散布では消えてしまう)。
    let cursor: Option<[(f64, f64); 2]> = app
        .table
        .selected()
        .and_then(|i| app.samples.get(i))
        .map(|s| {
            let x = (s.end_epoch as f64) - (x_origin as f64);
            [(x, y_bounds[0]), (x, y_bounds[1])]
        });

    let segments: usize = plots.iter().map(|p| p.view.segments.len()).sum();
    let mut datasets: Vec<Dataset> = Vec::with_capacity(segments + 1);
    if let Some(c) = cursor.as_ref() {
        datasets.push(
            Dataset::default()
                .graph_type(GraphType::Line)
                // 観測値と同じ Braille で細く引く。太いマーカーだと
                // カーソルが値の線より目立って、形が読み取りにくい。
                .marker(symbols::Marker::Braille)
                .style(Style::default().fg(GRAPH_CURSOR_COLOR))
                .data(c),
        );
    }
    // 後に描いたものが上に乗るので、凡例の先頭 (読み取ってほしい系列) を最後に描く。
    for p in plots.iter().rev() {
        for seg in &p.view.segments {
            datasets.push(
                Dataset::default()
                    .graph_type(GraphType::Line)
                    .marker(symbols::Marker::Braille)
                    .style(Style::default().fg(p.color))
                    .data(seg),
            );
        }
    }

    let gap = plots
        .iter()
        .map(|p| p.view.segments.len().saturating_sub(1))
        .max()
        .unwrap_or(0);
    // 線の切れ目は表の `!` / `R` と同じ意味。**色だけに頼らず語で書く。**
    // 軸の title には出さない (Y 軸ラベルと重なって読めなくなる)。
    let note = if gap > 0 {
        format!(" 切れ目 {gap} (不連続・欠測) ")
    } else {
        String::new()
    };
    let y_labels = graph::axis_labels(y_bounds);
    let y_label_w = y_labels
        .iter()
        .map(|l| l.chars().count())
        .max()
        .unwrap_or(0) as u16;
    let plot_width = area.width.saturating_sub(y_label_w + 3);

    let mut block = Block::bordered().title(title.clone());
    let mut bottom_note = Some(Line::from(note.clone()).right_aligned());
    if plots.len() > 1 {
        // **2 系列以上なら凡例を必ず出す。** 上辺の題名の右に入らなければ、
        // 下辺の左 (切れ目の注記と並べて) に置く。
        let inner = area.width.saturating_sub(2) as usize;
        let top_room = inner.saturating_sub(Line::from(title.as_str()).width() + 1);
        let bottom_room = inner.saturating_sub(Line::from(note.as_str()).width() + 1);
        if let Some(legend) = legend_line(&plots, top_room) {
            block = block.title_top(legend.right_aligned());
        } else if let Some(legend) = legend_line(&plots, bottom_room) {
            block = block.title_bottom(legend.left_aligned());
        } else if let Some(legend) = legend_line(&plots, inner) {
            // 注記より凡例を優先する (どの線が何かが分からないと図を読めない)
            block = block.title_bottom(legend.left_aligned());
            bottom_note = None;
        }
    }
    if let Some(n) = bottom_note {
        block = block.title_bottom(n);
    }

    let chart = Chart::new(datasets)
        .block(block)
        .x_axis(
            Axis::default()
                .style(Style::default().fg(Color::DarkGray))
                .bounds(x_bounds)
                // 軸に使える幅は、枠 (2) と Y 軸ラベルの分を引いた残り。
                // ここを渡さないと刻みが幅に追従せず、広い画面でも両端だけになる。
                .labels(plots[0].view.x_labels(app.tz, plot_width)),
        )
        .y_axis(
            Axis::default()
                .style(Style::default().fg(Color::DarkGray))
                .bounds(y_bounds)
                .labels(y_labels),
        );
    f.render_widget(chart, area);
}

/// 列ごとの説明の、列名の桁 (表の見出しと同じ表記で左に揃える)。
fn note_name_width(list: &[&notes::ColumnNote]) -> usize {
    list.iter().map(|n| text_width(n.column)).max().unwrap_or(0)
}

/// 列ごとの説明が、短い版なら 1 行に収まる幅か。
///
/// 注意の行も含めて確かめる。1 行でも収まらなければ、列ごとに並べるのをやめる。
fn notes_fit_width(set: &notes::NoteSet, list: &[&notes::ColumnNote], width: u16) -> bool {
    let width = width as usize;
    let room = width.saturating_sub(1 + note_name_width(list) + 2);
    // 注意の行は先頭に 1 桁の余白を置くので、幅より狭い必要がある。
    list.iter().all(|n| text_width(n.short) <= room) && text_width(set.caveat_short) < width
}

/// 表の下に列の説明を出す。
///
/// - 列名は表の見出しと同じ表記・同じ色の規則にする (グラフに描いている列は線の色)。
///   色は対応の手がかりで、**対応そのものは列名の文字で示す**
/// - 説明は本文の色で書く (灰色にすると読むための欄なのに読みにくい)
/// - 幅に入らない全文は短い版にする。短い版も入らない幅では [`NoteLayout::Summary`]
///   になっている ([`note_layout`])
fn draw_notes(
    f: &mut Frame,
    area: Rect,
    app: &App,
    set: &notes::NoteSet,
    list: &[&notes::ColumnNote],
    layout: NoteLayout,
) {
    let width = area.width as usize;
    // 1 行だけの行 (要約・注意) は先頭に 1 桁の余白を置く。入るのは幅より狭い文。
    let lines: Vec<Line> = match layout {
        NoteLayout::None => return,
        NoteLayout::Summary => {
            let text = if text_width(set.summary) < width {
                set.summary
            } else {
                set.summary_short
            };
            vec![Line::from(format!(" {text}"))]
        }
        NoteLayout::Full => {
            let name_w = note_name_width(list);
            let room = width.saturating_sub(1 + name_w + 2);
            // **全文と短い版を行ごとに混ぜない。** 1 行でも全文が入らなければ全部を
            // 短い版にそろえる (説明の粒度が行ごとに違うと、見比べにくい)。
            let full = list.iter().all(|n| text_width(n.text) <= room);
            let colors = app.graph_header_colors();
            let mut out: Vec<Line> = list
                .iter()
                .map(|n| {
                    let text = if full { n.text } else { n.short };
                    let mut name_style = Style::default().add_modifier(Modifier::BOLD);
                    if let Some((_, color)) = colors.iter().find(|(c, _)| *c == n.column) {
                        name_style = name_style.fg(*color);
                    }
                    Line::from(vec![
                        Span::raw(" "),
                        Span::styled(format!("{:<name_w$}", n.column), name_style),
                        Span::raw("  "),
                        Span::raw(text),
                    ])
                })
                .collect();
            let caveat = if text_width(set.caveat) < width {
                set.caveat
            } else {
                set.caveat_short
            };
            out.push(Line::from(format!(" {caveat}")));
            out
        }
    };
    f.render_widget(Paragraph::new(lines), area);
}

/// 比較グラフの凡例。幅 `room` に入らなければ短い名前で試し、それでも入らなければ `None`。
///
/// - **色だけに頼らない。** 色の印の後に名前を書く。名前は本文の色のまま
///   (系列の色で文字を塗ると、線との対応ではなく強調に見える)。
/// - **値の無い系列も消さない。** 灰色で「(記録なし)」と書く。線が無いこと自体が
///   「その世代のファイルに無い」という観測だからである。
fn legend_line(plots: &[Plot], room: usize) -> Option<Line<'static>> {
    for short in [false, true] {
        let mut spans: Vec<Span<'static>> = vec![Span::raw(" ")];
        for (i, p) in plots.iter().enumerate() {
            if i > 0 {
                spans.push(Span::raw("  "));
            }
            let name = if short { p.short } else { p.label };
            if p.view.is_empty() {
                spans.push(Span::styled(
                    format!("- {name} ({})", absent_word(&p.view)),
                    Style::default().fg(Color::DarkGray),
                ));
            } else {
                spans.push(Span::styled(LEGEND_MARK, Style::default().fg(p.color)));
                spans.push(Span::raw(format!(" {name}")));
            }
        }
        spans.push(Span::raw(" "));
        let line = Line::from(spans);
        if line.width() <= room {
            return Some(line);
        }
    }
    None
}

/// 1 点も描けなかった系列の言い方。
///
/// **「無い」の理由を混ぜない。** 全時刻で「その世代のファイルに無い」なら
/// 記録そのものが無い。欠測など別の理由が混じるなら、値が無いとだけ言う。
fn absent_word(view: &graph::GraphView) -> &'static str {
    if !view.absent.is_empty()
        && view
            .absent
            .iter()
            .all(|(reason, _)| *reason == Quality::UnsupportedBySource.label())
    {
        "記録なし"
    } else {
        "値なし"
    }
}

fn draw_header(f: &mut Frame, area: Rect, app: &App) {
    let h = &app.host;
    let l1 = Line::from(vec![
        Span::styled(
            h.hostname.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(
            "  {} {} / {}  ({} CPU)",
            h.sysname, h.release, h.machine, h.cpu_count
        )),
    ]);
    let l2 = Line::from(Span::styled(
        format!(
            "{}  {}  時刻は {}  ({} サンプル){}",
            h.file_date,
            h.source,
            // 先頭サンプルの時点で解決する (夏時間のある地域では時期で変わる)
            app.tz
                .label_at(app.samples.first().map_or(0, |s| s.end_epoch)),
            app.samples.len(),
            if app.incomplete {
                "  不完全な入力"
            } else {
                ""
            }
        ),
        Style::default().fg(Color::DarkGray),
    ));
    f.render_widget(Paragraph::new(vec![l1, l2]), area);
}

fn draw_tabs(f: &mut Frame, area: Rect, app: &App) {
    let titles: Vec<String> = app
        .tabs
        .iter()
        .map(|t| t.name.trim_start_matches("A_").to_string())
        .collect();
    if titles.is_empty() {
        return;
    }
    f.render_widget(
        Tabs::new(titles)
            .select(app.tab)
            .highlight_style(
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )
            .divider(" "),
        area,
    );
}

fn draw_table(f: &mut Frame, area: Rect, app: &mut App) {
    let Some(act) = app.current_activity() else {
        f.render_widget(
            Paragraph::new("表示できる activity がありません").block(Block::bordered()),
            area,
        );
        return;
    };
    let item_name = app.selected_item_name();
    let chosen = app.table_columns();
    // **潰れた表を出さない。** ratatui の Table は幅が足りないと全列を縮めるので、
    // 23 列を幅 100 へ詰めると 1 列 3 桁になって、どの列の値も読めなくなる。
    // 入る列だけを出し、残りは横スクロール (`shift + ← →`) で見せる。
    let all_widths = app.column_widths(&chosen);
    let offset = app.col_offset().min(chosen.len().saturating_sub(1));
    let widths: Vec<u16> = all_widths.iter().skip(offset).copied().collect();
    let fit = fit_column_count(&widths, area.width);
    let cols: Vec<&'static str> = chosen.iter().skip(offset).take(fit).copied().collect();
    // 次の描画で `shift + →` の上限を決めるのに使う (幅は描画側でしか分からない)。
    app.visible_cols = cols.len();
    let shown_total = chosen.len();
    // 表から外している列 (値なしや利用者の選択) は、スクロールしても出てこない。
    let dropped = app.columns().len().saturating_sub(shown_total);
    // 行の描画で参照するので、可変借用に入る前に切り出しておく。
    let marks: Vec<Mark> = app
        .marks
        .iter()
        .map(|m| Mark {
            epoch: m.epoch,
            kind: m.kind,
            text: m.text.clone(),
        })
        .collect();

    let mut title = match &item_name {
        Some(n) => format!(" {} — {}  [{}] ", act, app.tabs[app.tab].label, n),
        None => format!(" {} — {} ", act, app.tabs[app.tab].label),
    };
    // **いま何列目を見ているかを出す。** 出さないと、右にまだ列があることに
    // 気づけないまま「値が無い」と読み違える。
    if shown_total > cols.len() {
        let from = offset + 1;
        let to = offset + cols.len();
        title.push_str(&format!(" {from}-{to}/{shown_total} 列 "));
    }
    if dropped > 0 {
        // **外したことを黙らない。** 「その列が無い」と読まれると、
        // 観測できなかった事実まで消えてしまう。出し方も一緒に書く。
        title.push_str(&format!(" 他 {dropped} 列 (c で選ぶ) "));
    }

    // グラフに描いている列は、表の見出しも同じ色にする。
    // **グラフの線と同じ色を使う** — 「どの列が上の線なのか」を色で結びつける。
    // 比較グラフなら系列ごとにその線の色になる。
    // グラフが出ていないときは色を付けない (対応する線が無い)。
    let graphed = app.graph_header_colors();
    let mut header: Vec<Cell> = vec![Cell::from("time")];
    header.extend(cols.iter().map(|c| {
        let cell = Cell::from(*c);
        match graphed.iter().find(|(name, _)| name == c) {
            Some((_, color)) => {
                cell.style(Style::default().fg(*color).add_modifier(Modifier::BOLD))
            }
            None => cell,
        }
    }));

    let rows: Vec<Row> = app
        .samples
        .iter()
        .enumerate()
        .map(|(row, s)| {
            let mut cells: Vec<Cell> = Vec::with_capacity(cols.len() + 1);
            // 注記 (再起動・コメント) と不連続を時刻の前に出す。
            // **これは異変検知ではなく、値を読み違えないための情報**である。
            // 点を線で結ばせないために、区間をまたいだ事象はここで見えるようにする。
            let note = marks
                .iter()
                .find(|m| m.epoch > s.start_epoch && m.epoch <= s.end_epoch);
            let (mark, style) = match (note.map(|m| m.kind), s.continuous, row) {
                (Some("restart"), ..) => ("R", Style::default().fg(Color::Magenta)),
                (Some(_), ..) => ("C", Style::default().fg(Color::Cyan)),
                // 先頭行に前サンプルが無いのは当然なので、印を出さない。
                // ここに「!」を出すと、毎回異常があるように見えてしまう。
                (None, false, 0) => (" ", Style::default()),
                (None, false, _) => ("!", Style::default().fg(Color::Yellow)),
                (None, true, _) => (" ", Style::default()),
            };
            cells.push(Cell::from(format!("{mark}{}", hhmmss(app.tz, s.end_epoch))).style(style));

            let found = item_name.as_ref().and_then(|want| {
                s.activities
                    .iter()
                    .find(|a| a.activity == act)
                    .and_then(|a| {
                        a.items
                            .iter()
                            .find(|it| display_item(&it.item, it.cpu.as_deref()) == *want)
                    })
            });
            match found {
                Some(it) => {
                    let fields: Vec<&FieldOut> = visible_fields(it).collect();
                    for name in &cols {
                        let fv = fields.iter().find(|f| f.name == *name).copied();
                        let style = fv.map(|f| quality_style(f.quality)).unwrap_or_default();
                        cells.push(Cell::from(cell_text(fv)).style(style));
                    }
                }
                None => {
                    // この時刻にはこの item が無い (着脱・再起動をまたいだ入れ替え)。
                    for _ in &cols {
                        cells.push(Cell::from(ABSENT).style(Style::default().fg(Color::DarkGray)));
                    }
                }
            }
            Row::new(cells)
        })
        .collect();

    let mut constraints = vec![Constraint::Length(TIME_COL_WIDTH)];
    constraints.extend(widths.iter().take(fit).map(|w| Constraint::Length(*w)));

    let table = Table::new(rows, constraints)
        .header(Row::new(header).style(Style::default().add_modifier(Modifier::BOLD)))
        .block(Block::bordered().title(title))
        .row_highlight_style(Style::default().bg(Color::DarkGray))
        .highlight_symbol("");

    f.render_stateful_widget(table, area, &mut app.table);
}

fn draw_hint(f: &mut Frame, area: Rect, app: &App) {
    let text = match app.mode {
        Mode::Filter => format!("絞り込み: {}_    Enter 確定  Esc 取消", app.filter),
        _ => {
            // 選択中の区間に注記があれば、キーヒントより先にそれを見せる。
            if let Some(note) = app.note_at_selection() {
                format!("{}  {}", hhmmss(app.tz, note.epoch), note.text)
            } else {
                let n = app.filtered_items().len();
                // **右に続きがあるときだけ横送りを案内する。**
                // 全部入っているときは押す理由が無いので出さない。逆に切れている
                // ときは、案内が無いと右に列があること自体に気づけない。
                let scroll = if app.visible_cols < app.table_columns().len() {
                    "shift←→ 横  "
                } else {
                    ""
                };
                let graph_keys = if app.graph_visible() {
                    "[] 送り  v グラフ  "
                } else if graph_height(GraphVisibility::Shown, app.last_height) > 0 {
                    "v グラフ  "
                } else {
                    ""
                };
                // **幅に入る最初のものを出す。** 途中で切れて語の途中で終わると、
                // 案内どころか何のキーか読めない。優先度の低いものから落とす
                // (最後まで残すのは `?` と `esc` — ここから先は調べられる)。
                let candidates = [
                    format!(
                        "←→ activity  ↑↓ 時刻  {scroll}i item ({n})  / 絞り込み  home/end 端  c 列  {graph_keys}? help  esc 終了"
                    ),
                    format!("←→ act  ↑↓ 時刻  {scroll}i item  c 列  {graph_keys}? help  esc"),
                    format!("←→ act  ↑↓ 時刻  {scroll}i item  c 列  ? help  esc"),
                    format!("←→ act  ↑↓ 時刻  {scroll}? help  esc"),
                    format!("{scroll}? help  esc"),
                    "? help  esc".to_string(),
                ];
                candidates
                    .into_iter()
                    .find(|c| text_width(c) <= area.width as usize)
                    .unwrap_or_else(|| "?".to_string())
            }
        }
    };
    f.render_widget(
        Paragraph::new(text).style(Style::default().fg(Color::DarkGray)),
        area,
    );
}

/// 端末での表示幅 (全角を 2 桁で数える)。
///
/// `chars().count()` だと日本語のヒントが実際の倍の幅を占めて溢れる。
fn text_width(s: &str) -> usize {
    s.chars().map(|c| if c.is_ascii() { 1 } else { 2 }).sum()
}

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let x = area.x + area.width.saturating_sub(w) / 2;
    let y = area.y + area.height.saturating_sub(h) / 2;
    Rect {
        x,
        y,
        width: w.min(area.width),
        height: h.min(area.height),
    }
}

fn draw_picker(f: &mut Frame, area: Rect, app: &mut App) {
    let items = app.filtered_items();
    let h = (items.len() as u16 + 2)
        .min(area.height.saturating_sub(2))
        .max(3);
    let w = items
        .iter()
        .map(|s| s.chars().count() as u16)
        .max()
        .unwrap_or(10)
        .clamp(20, area.width.saturating_sub(4));
    let r = centered(area, w + 4, h);
    f.render_widget(Clear, r);
    let list = List::new(
        items
            .iter()
            .map(|s| ListItem::new(s.clone()))
            .collect::<Vec<_>>(),
    )
    .block(Block::bordered().title(" item (Enter 決定 / Esc 取消) "))
    .highlight_style(Style::default().bg(Color::DarkGray));
    app.picker
        .select(Some(app.item.get(app.tab).copied().unwrap_or(0)));
    f.render_stateful_widget(list, r, &mut app.picker);
}

/// `time` 列の幅 (`HH:MM:SS` + 不連続の印 1 桁 + 余白)。
const TIME_COL_WIDTH: u16 = 10;

/// 幅に入る列数。
///
/// 1 列も入らない場合でも 1 は返す (空の表より 1 列でも読める方がよい)。
fn fit_column_count(widths: &[u16], area_width: u16) -> usize {
    // 枠 2 + `time` 列 + 列間の空白 1。
    let mut used = 2 + TIME_COL_WIDTH + 1;
    let mut n = 0;
    for w in widths {
        if used + w > area_width {
            break;
        }
        used += w + 1;
        n += 1;
    }
    n.max(1)
}
/// 列を選ぶポップアップ。
///
/// **表に出す列とグラフに描く列を 1 つの画面で扱う。** 別々のキーに割り当てると
/// 「どちらが表でどちらがグラフか」を覚える必要が出る。
///
/// - `[x]` が表に出す列。`space` で出し入れする
/// - カーソルの行が、確定したときにグラフへ描かれる列になる
/// - **値が出ない列も一覧から消さない。** 消すと「その列が無い」と読まれ、
///   観測できなかった事実まで隠れる。灰色と注記で「選んでも線は出ない」と示す
fn draw_column_picker(f: &mut Frame, area: Rect, app: &mut App) {
    let all = app.columns();
    if all.is_empty() {
        return;
    }
    let measured = app.measured_columns();
    let plottable = app.plottable_columns();
    let rows: Vec<ListItem> = all
        .iter()
        .map(|name| {
            let on = if app.col_draft.contains(name) {
                "x"
            } else {
                " "
            };
            let has_value = measured.contains(name);
            let note = if !has_value {
                "  (全時刻で値なし)"
            } else if !plottable.contains(name) {
                "  (グラフ不可)"
            } else {
                ""
            };
            let style = if has_value {
                Style::default()
            } else {
                Style::default().fg(Color::DarkGray)
            };
            ListItem::new(Line::from(Span::styled(
                format!("[{on}] {name}{note}"),
                style,
            )))
        })
        .collect();
    let h = (rows.len() as u16 + 2)
        .min(area.height.saturating_sub(2))
        .max(3);
    let w = all
        .iter()
        .map(|s| s.chars().count() as u16 + 22)
        .max()
        .unwrap_or(24)
        .clamp(30, area.width.saturating_sub(4));
    let r = centered(area, w, h);
    f.render_widget(Clear, r);
    let list = List::new(rows)
        // タイトルは枠幅で切られる。詳しい説明は `?` のヘルプに置き、
        // ここには操作の骨だけを出す。
        .block(
            Block::bordered()
                .title(" 列 ")
                .title_bottom(" space 表示  a 全部  enter 決定  esc 取消 "),
        )
        .highlight_style(Style::default().bg(Color::DarkGray));
    f.render_stateful_widget(list, r, &mut app.col_picker);
}

fn draw_help(f: &mut Frame, area: Rect) {
    // **すべて小文字と記号。** Shift の有無で別の操作になると、
    // 打ち間違いが「別の機能が動く」形で出る。
    let lines = vec![
        Line::from("←  →        activity を切り替える"),
        Line::from("shift + ← → 表を横に送る"),
        Line::from("↑  ↓        時刻を移動する"),
        Line::from("pgup pgdn   10 行ずつ移動する"),
        Line::from("home end    先頭 / 末尾"),
        Line::from("i           item を選ぶ"),
        Line::from("/           item を名前で絞り込む"),
        Line::from("c           列を選ぶ (表に出す列とグラフの列)"),
        Line::from("[  ]        グラフの列を前 / 次へ (表に出ている列だけ)"),
        Line::from("            MEMORY は先頭が使用率 3 通りの比較グラフ"),
        Line::from("v           グラフの表示を切り替える"),
        Line::from("?           このヘルプ"),
        Line::from("esc ctrl-c  終了 (esc は通常画面で)"),
        Line::from(""),
        Line::from("c のポップアップ:"),
        Line::from("  space     その列を表に出す / 外す"),
        Line::from("  a         全部の列 / 値のある列だけ"),
        Line::from("  enter     決定 (カーソルの列をグラフへ)"),
        Line::from("  esc       取消"),
        Line::from(""),
        Line::from(format!("{ABSENT} は値が無いこと。0 ではない。")),
        Line::from("時刻の前の ! は、直前との間に不連続があること。"),
        Line::from("表は既定で「全時刻で値が出なかった列」を外す。c で出せる。"),
        Line::from("グラフの線は、不連続と欠測のところで切れる。"),
        Line::from("切れ目を飛び越えて結ばないのは、その間を観測していないため。"),
        Line::from(""),
        Line::from("MEMORY の使用率と追加の列 (表の下にも説明が出る):"),
        Line::from("  memused_pct            MemAvailable 基準の使用率"),
        Line::from("  memused_nocache_pct    cache を空きに数えた使用率"),
        Line::from("  memused_withcache_pct  cache も使用中に数えた使用率"),
        Line::from("  kbmemused_nocache      total-free-buffers-cached (kB)"),
        Line::from("  kbmemfree_withcache    free+buffers+cached (kB)"),
        Line::from("正確な空きは kbavail。cached には共有メモリ・tmpfs も入る。"),
        Line::from("kbmemfree が小さいだけではメモリ不足と言えない。"),
    ];
    let r = centered(area, 62, lines.len() as u16 + 2);
    f.render_widget(Clear, r);
    f.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(" キー操作 ")),
        r,
    );
}

// ===========================================================================
// 入力
// ===========================================================================

fn on_key(app: &mut App, code: KeyCode, mods: KeyModifiers) {
    if mods.contains(KeyModifiers::CONTROL) && matches!(code, KeyCode::Char('c')) {
        app.quit = true;
        return;
    }
    match app.mode {
        Mode::Filter => match code {
            KeyCode::Esc => {
                app.filter.clear();
                app.mode = Mode::Normal;
            }
            KeyCode::Enter => app.mode = Mode::Normal,
            KeyCode::Backspace => {
                app.filter.pop();
            }
            KeyCode::Char(c) => app.filter.push(c),
            _ => {}
        },
        Mode::PickItem => match code {
            KeyCode::Esc | KeyCode::Char('i') => app.mode = Mode::Normal,
            KeyCode::Enter => app.mode = Mode::Normal,
            KeyCode::Up => app.move_item(-1),
            KeyCode::Down => app.move_item(1),
            KeyCode::Char('/') => {
                app.filter.clear();
                app.mode = Mode::Filter;
            }
            _ => {}
        },
        Mode::PickColumn => match code {
            // Esc は下書きを捨てる (表もグラフも元のまま)。
            KeyCode::Esc => {
                app.col_draft.clear();
                app.col_picker_origin = None;
                app.mode = Mode::Normal;
            }
            KeyCode::Enter => {
                app.commit_columns();
                app.mode = Mode::Normal;
            }
            KeyCode::Up => app.move_col_picker(-1),
            KeyCode::Down => app.move_col_picker(1),
            KeyCode::Char(' ') => app.toggle_draft_column(),
            KeyCode::Char('a') => app.toggle_draft_all(),
            _ => {}
        },
        Mode::Help => app.mode = Mode::Normal,
        Mode::Normal => match code {
            KeyCode::Esc => app.quit = true,
            // `shift` 付きの矢印は表の横送り。activity の切り替えと
            // 同じ方向キーに載せるのは、どちらも「横に動く」操作だから。
            KeyCode::Left if mods.contains(KeyModifiers::SHIFT) => {
                app.scroll_cols(-1, app.visible_cols);
            }
            KeyCode::Right if mods.contains(KeyModifiers::SHIFT) => {
                app.scroll_cols(1, app.visible_cols);
            }
            KeyCode::Left => app.move_tab(-1),
            KeyCode::Right => app.move_tab(1),
            KeyCode::Up => app.move_row(-1),
            KeyCode::Down => app.move_row(1),
            KeyCode::PageUp => app.move_row(-10),
            KeyCode::PageDown => app.move_row(10),
            // **大文字を使わない。** Shift の有無で別の操作になると、
            // 打ち間違いが「別の機能が動く」形で表に出る。
            KeyCode::Home => app.table.select(Some(0)),
            KeyCode::End => {
                let last = app.samples.len().saturating_sub(1);
                app.table.select(Some(last));
            }
            KeyCode::Char('i') => app.mode = Mode::PickItem,
            KeyCode::Char('c') => app.open_column_picker(),
            // グラフの列送り。`←` / `→` は activity に使っているので別のキーにする。
            KeyCode::Char('[') => app.move_col(-1),
            KeyCode::Char(']') => app.move_col(1),
            KeyCode::Char('v') => app.toggle_graph(),
            KeyCode::Char('/') => {
                app.filter.clear();
                app.mode = Mode::Filter;
            }
            KeyCode::Char('?') => app.mode = Mode::Help,
            _ => {}
        },
    }
}

// ===========================================================================
// 入口
// ===========================================================================

/// TUI を起動する。
///
/// 端末の後始末は `ratatui::init` / `restore` に任せる。パニックしても
/// 端末が生 raw mode のまま残らないよう、`init` がフックを入れる。
pub fn run(path: &Path, cfg: &CustomConfig, file: &SaFile) -> Result<()> {
    run_prepared(path, prepare(file, cfg)?)
}

/// 準備済みの観測値を表示する。呼び出し側は走査結果を確認してから端末を開ける。
pub fn run_prepared(path: &Path, prepared: Prepared) -> Result<()> {
    let incomplete = prepared.scan_summary.incomplete;
    let collected = prepared.collected;
    if collected.samples.is_empty() {
        return Err(crate::error::Error::Other(format!(
            "{}: 表示できるサンプルがありません",
            path.display()
        )));
    }
    let mut app = App::new(collected);
    app.incomplete = incomplete;

    // 端末が無い (パイプ・リダイレクト・CI) 場合にパニックさせない。
    // 「対話端末が要る」ことを、使える代替と一緒に伝える。
    let mut terminal = ratatui::try_init().map_err(|e| {
        crate::error::Error::Other(format!(
            "端末を初期化できません ({e})。\
             TUI は対話端末でのみ動きます。パイプやファイルへ出す場合は \
             `resarch show` / `resarch sar` を使ってください"
        ))
    })?;
    let res = event_loop(&mut terminal, &mut app);
    // 描画が失敗しても端末は必ず戻す (raw mode のまま抜けない)。
    ratatui::restore();
    res
}

fn event_loop<B: Backend>(terminal: &mut Terminal<B>, app: &mut App) -> Result<()> {
    // 静的なファイルなので、入力とリサイズのときだけ描き直す。
    // 定期的な再描画は端末を無駄に叩くだけで、得るものが無い。
    loop {
        terminal
            .draw(|f| draw(f, app))
            .map_err(|e| crate::error::Error::Other(format!("描画に失敗しました: {e}")))?;
        match event::read()
            .map_err(|e| crate::error::Error::Other(format!("入力を読めません: {e}")))?
        {
            Event::Key(k) if k.kind == KeyEventKind::Press => on_key(app, k.code, k.modifiers),
            Event::Resize(_, _) => {}
            _ => {}
        }
        if app.quit {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::json::{ActivityOut, ItemOut, Quality};
    use ratatui::backend::TestBackend;

    const T0: u64 = 1_767_225_600;

    fn host() -> HostOut {
        HostOut {
            hostname: "testhost".into(),
            sysname: "Linux".into(),
            release: "0.0.0".into(),
            machine: "x86_64".into(),
            cpu_count: 2,
            file_date: "2026-01-01".into(),
            timezone: String::new(),
            source: "sa01".into(),
        }
    }

    /// `values` の `None` は欠測。`continuous` が false の添字で線が切れる。
    fn app_with(values: &[Option<f64>], broken: &[usize]) -> App {
        let samples = values
            .iter()
            .enumerate()
            .map(|(i, v)| SampleOut {
                boot: 1,
                start_epoch: T0 + i as u64 * 600,
                end_epoch: T0 + (i as u64 + 1) * 600,
                elapsed_cs: 60_000,
                continuous: !broken.contains(&i),
                activities: vec![ActivityOut {
                    activity: "A_CPU",
                    label: "CPU 使用率",
                    items: vec![ItemOut {
                        item: "all".into(),
                        index: 0,
                        cpu: None,
                        raw: Vec::new(),
                        rates: vec![FieldOut {
                            name: "user",
                            unit: "percent",
                            kind: "counter",
                            raw: None,
                            value: *v,
                            text: None,
                            quality: if v.is_some() {
                                Quality::Ok
                            } else {
                                Quality::MissingInSample
                            },
                        }],
                    }],
                }],
            })
            .collect();
        App::new(Collected {
            host: host(),
            samples,
            marks: Vec::new(),
            tz: DisplayTz::Utc,
        })
    }

    fn render(app: &mut App, w: u16, h: u16) -> String {
        let mut terminal = ratatui::Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        let buf = terminal.backend().buffer().clone();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// 画面に文字列が出ているか。
    ///
    /// 全角文字は 2 セルを占め、2 セル目が空白になる。バッファをそのまま
    /// 連結すると「時 刻」のように隙間が入るので、**両辺から空白を落として**
    /// 比べる。列の間隔ではなく「その語が出ているか」を見たいテストなので、
    /// この丸めで失うものはない。
    fn shows(screen: &str, needle: &str) -> bool {
        let strip = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
        strip(screen).contains(&strip(needle))
    }

    /// 広い端末では既定でグラフが出て、選択中の系列を名指しする。
    #[test]
    fn a_tall_terminal_shows_the_graph_by_default() {
        let mut app = app_with(&[Some(1.0), Some(2.0), Some(3.0)], &[]);
        let screen = render(&mut app, 80, 40);
        assert!(shows(&screen, "A_CPU / all / user"), "{screen}");
        assert!(shows(&screen, "percent"), "単位を出す: {screen}");
    }

    /// 低い端末では既定で出さない。表が潰れる方が困る。
    #[test]
    fn a_short_terminal_keeps_the_table_and_hides_the_graph() {
        let mut app = app_with(&[Some(1.0), Some(2.0)], &[]);
        let screen = render(&mut app, 80, 20);
        assert!(!shows(&screen, "A_CPU / all / user"), "{screen}");
        // 表は出ている
        assert!(shows(&screen, "CPU 使用率"), "{screen}");
    }

    /// `v` で出し入れできる。
    #[test]
    fn the_v_key_toggles_the_graph() {
        let mut app = app_with(&[Some(1.0), Some(2.0), Some(3.0)], &[]);
        let shown = render(&mut app, 80, 40);
        assert!(shows(&shown, "A_CPU / all / user"));

        on_key(&mut app, KeyCode::Char('v'), KeyModifiers::NONE);
        let hidden = render(&mut app, 80, 40);
        assert!(!shows(&hidden, "A_CPU / all / user"), "{hidden}");

        on_key(&mut app, KeyCode::Char('v'), KeyModifiers::NONE);
        let again = render(&mut app, 80, 40);
        assert!(shows(&again, "A_CPU / all / user"), "{again}");
    }

    /// 全部欠測なら、空の軸ではなく理由を出す。
    #[test]
    fn an_all_missing_series_says_why_it_is_empty() {
        let mut app = app_with(&[None, None, None], &[]);
        let screen = render(&mut app, 80, 40);
        assert!(shows(&screen, "描画できる観測がありません"), "{screen}");
        assert!(shows(&screen, "missing_in_sample"), "理由も出す: {screen}");
    }

    /// 線の切れ目があることを語で書く (色だけに頼らない)。
    #[test]
    fn a_broken_line_is_stated_in_words() {
        let mut app = app_with(&[Some(1.0), Some(2.0), None, Some(3.0)], &[]);
        let screen = render(&mut app, 80, 40);
        assert!(shows(&screen, "切れ目"), "{screen}");
    }

    /// 端末が低くてグラフを出せないときは、効かないキーを案内しない。
    #[test]
    fn the_hint_omits_graph_keys_when_the_graph_cannot_fit() {
        let mut app = app_with(&[Some(1.0)], &[]);
        let screen = render(&mut app, 80, 16);
        assert!(!shows(&screen, "v グラフ"), "{screen}");
        // 使えるキーの案内は残る
        assert!(shows(&screen, "i item"), "{screen}");
    }

    /// ヒントは端末幅に収まる (語の途中で切れない)。
    ///
    /// 収まったかどうかは**右端に余白が残っているか**で見る。
    /// バッファのダンプは全角の 2 セル目が空白になるため、
    /// ダンプ文字列の長さからは実際の表示幅を数えられない。
    #[test]
    fn the_hint_fits_the_terminal_width() {
        // 列を増やして横送りの案内も出る状態にする (行が最も長くなる条件)
        let mut app = app_with(&[Some(1.0), Some(2.0), Some(3.0)], &[]);
        for i in 0..9 {
            let name: &'static str = Box::leak(format!("col{i}").into_boxed_str());
            for s in &mut app.samples {
                s.activities[0].items[0].rates.push(FieldOut {
                    name,
                    unit: "percent",
                    kind: "counter",
                    raw: None,
                    value: Some(11_111_111.0),
                    text: None,
                    quality: Quality::Ok,
                });
            }
        }
        for w in [40u16, 46, 50, 60, 70, 80, 100, 120] {
            // 1 度描いて「何列入ったか」を確定させてから測る
            render(&mut app, w, 40);
            let screen = render(&mut app, w, 40);
            let hint = screen.lines().last().unwrap();
            // **どの幅でも最後まで読める。** 途中で切れると何のキーか分からない。
            assert!(shows(hint, "esc"), "幅 {w} で末尾まで出る: {hint}");
            assert_eq!(
                hint.chars().last(),
                Some(' '),
                "幅 {w} で右端に余白が残る (切れていない): {hint}"
            );
        }
    }

    #[test]
    fn graph_height_never_starves_the_table() {
        // 低い端末では既定で出さない
        assert_eq!(graph_height(GraphVisibility::Auto, 20), 0);
        // 明示されても、表が潰れる高さでは出さない
        assert_eq!(graph_height(GraphVisibility::Shown, 17), 0);
        assert_eq!(graph_height(GraphVisibility::Shown, 20), 6);
        // 隠す指定は常に優先
        assert_eq!(graph_height(GraphVisibility::Hidden, 100), 0);
        // 高い端末でも上限を超えて広げない
        assert_eq!(graph_height(GraphVisibility::Auto, 200), 14);
        assert_eq!(graph_height(GraphVisibility::Auto, 30), 7);
    }

    /// 値が出ない列は既定で表から外す。
    #[test]
    fn columns_without_any_value_are_dropped_by_default() {
        let mut app = app_with(&[Some(1.0), Some(2.0)], &[]);
        // 全時刻で値の無い列を足す
        for s in &mut app.samples {
            s.activities[0].items[0].rates.push(FieldOut {
                name: "never",
                unit: "percent",
                kind: "counter",
                raw: None,
                value: None,
                text: None,
                quality: Quality::UnsupportedBySource,
            });
        }
        assert_eq!(app.columns(), vec!["user", "never"]);
        assert_eq!(app.table_columns(), vec!["user"], "値の無い列は外す");

        let screen = render(&mut app, 80, 40);
        assert!(!shows(&screen, "never"), "表には出ない: {screen}");
        // **黙って消さない。** 何列外したかと、出し方を書く。
        assert!(shows(&screen, "他 1 列"), "{screen}");
        assert!(shows(&screen, "c で選ぶ"), "{screen}");
    }

    /// グラフの列ピッカーも、値の出ない列を灰色と注記で示す。
    #[test]
    fn the_graph_picker_greys_out_columns_without_values() {
        let mut app = app_with(&[Some(1.0), Some(2.0)], &[]);
        for s in &mut app.samples {
            s.activities[0].items[0].rates.push(FieldOut {
                name: "never",
                unit: "percent",
                kind: "counter",
                raw: None,
                value: None,
                text: None,
                quality: Quality::UnsupportedBySource,
            });
        }
        on_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE);
        assert_eq!(app.mode, Mode::PickColumn);

        let mut terminal = ratatui::Terminal::new(TestBackend::new(80, 40)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let buf = terminal.backend().buffer().clone();
        let dump = (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        // **選択肢から消さない。** 灰色と注記で「選んでも線は出ない」と示す。
        assert!(shows(&dump, "never"), "{dump}");
        assert!(shows(&dump, "全時刻で値なし"), "{dump}");

        // `never` の行が灰色で、値のある `user` の行はそうでないこと。
        let fg_of = |needle: &str| -> Option<Color> {
            for y in 0..buf.area.height {
                let line: String = (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>();
                if line.contains(needle) {
                    let x = line.find(needle).unwrap() as u16;
                    return buf[(x, y)].style().fg;
                }
            }
            None
        };
        assert_eq!(fg_of("never"), Some(Color::DarkGray), "灰色にする");
        assert_ne!(
            fg_of("user"),
            Some(Color::DarkGray),
            "値のある列は灰色にしない"
        );
    }

    /// `C` で選べば、値の無い列も表に出せる。
    #[test]
    fn the_picker_can_bring_back_a_column_without_values() {
        let mut app = app_with(&[Some(1.0), Some(2.0)], &[]);
        for s in &mut app.samples {
            s.activities[0].items[0].rates.push(FieldOut {
                name: "never",
                unit: "percent",
                kind: "counter",
                raw: None,
                value: None,
                text: None,
                quality: Quality::UnsupportedBySource,
            });
        }
        on_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE);
        assert_eq!(app.mode, Mode::PickColumn);
        // 一覧には値の無い列も並ぶ (外しただけで、無いわけではない)
        let picker = render(&mut app, 80, 40);
        assert!(shows(&picker, "never"), "{picker}");
        assert!(shows(&picker, "全時刻で値なし"), "理由を書く: {picker}");

        // `a` で全列、Enter で確定
        on_key(&mut app, KeyCode::Char('a'), KeyModifiers::NONE);
        on_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.table_columns(), vec!["user", "never"]);
        let screen = render(&mut app, 80, 40);
        assert!(shows(&screen, "never"), "{screen}");
    }

    /// Esc は下書きを捨てる (表は元のまま)。
    #[test]
    fn escaping_the_picker_keeps_the_table_unchanged() {
        let mut app = app_with(&[Some(1.0)], &[]);
        let before = app.table_columns();
        on_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE);
        on_key(&mut app, KeyCode::Char(' '), KeyModifiers::NONE);
        on_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(app.table_columns(), before);
        assert!(app.col_draft.is_empty());
    }

    /// 幅に入らない列は潰さずに落とす。
    #[test]
    fn columns_are_dropped_rather_than_squashed() {
        // 幅 20 の列を 3 本
        let widths = vec![20u16, 20, 20];
        assert_eq!(fit_column_count(&widths, 80), 3);
        assert_eq!(fit_column_count(&widths, 60), 2);
        assert_eq!(fit_column_count(&widths, 40), 1);
        // 1 列も入らなくても、空の表にはしない
        assert_eq!(fit_column_count(&widths, 10), 1);
    }

    /// 列幅は値の桁で決まる (列名だけで決めない)。
    #[test]
    fn column_width_follows_the_widest_value() {
        let app = app_with(&[Some(1.0), Some(123_456.0)], &[]);
        // `user` は 4 文字だが、値は `123456` まで伸びる
        let wide = app.column_widths(&["user"]);
        assert!(wide[0] >= 6, "値の桁に合わせる: {wide:?}");
        assert!(wide[0] > 4, "列名の長さだけで決めない: {wide:?}");

        // 逆に値が短ければ、列名の長さで足りる
        let small = app_with(&[Some(1.0), Some(2.0)], &[]);
        let narrow = small.column_widths(&["user"]);
        assert!(
            narrow[0] < wide[0],
            "値が短い列に一律の幅を与えない: {narrow:?} < {wide:?}"
        );
    }

    /// `[` / `]` でグラフの列が送られる。
    #[test]
    fn brackets_step_through_the_graph_metric() {
        let mut app = app_with(&[Some(1.0), Some(2.0)], &[]);
        // 描ける列を 3 本にする
        for s in &mut app.samples {
            for (name, v) in [("b", 2.0), ("c", 3.0)] {
                s.activities[0].items[0].rates.push(FieldOut {
                    name,
                    unit: "percent",
                    kind: "counter",
                    raw: None,
                    value: Some(v),
                    text: None,
                    quality: Quality::Ok,
                });
            }
        }
        assert_eq!(app.plottable_columns(), vec!["user", "b", "c"]);
        assert_eq!(app.selected_column(), Some("user"));

        on_key(&mut app, KeyCode::Char(']'), KeyModifiers::NONE);
        assert_eq!(app.selected_column(), Some("b"));
        on_key(&mut app, KeyCode::Char(']'), KeyModifiers::NONE);
        assert_eq!(app.selected_column(), Some("c"));
        // 端で巻き戻る (止まると「壊れた」ように見える)
        on_key(&mut app, KeyCode::Char(']'), KeyModifiers::NONE);
        assert_eq!(app.selected_column(), Some("user"));
        on_key(&mut app, KeyCode::Char('['), KeyModifiers::NONE);
        assert_eq!(app.selected_column(), Some("c"));
    }

    /// 幅に入らない列は `shift + ←/→` で送って見られる。
    #[test]
    fn the_table_scrolls_sideways_through_columns() {
        let mut app = app_with(&[Some(1.0), Some(2.0)], &[]);
        for i in 0..9 {
            let name: &'static str = Box::leak(format!("col{i}").into_boxed_str());
            for s in &mut app.samples {
                s.activities[0].items[0].rates.push(FieldOut {
                    name,
                    unit: "percent",
                    kind: "counter",
                    raw: None,
                    // 幅を食わせて、狭い画面に全部は入らないようにする
                    value: Some(11_111_111.0 + i as f64),
                    text: None,
                    quality: Quality::Ok,
                });
            }
        }
        assert_eq!(app.table_columns().len(), 10);
        // グラフを隠す。タイトルに列名が出るので、表だけを見て確かめたい。
        // `v` は直近の描画で分かった高さを見るので、先に 1 度描く。
        render(&mut app, 60, 40);
        on_key(&mut app, KeyCode::Char('v'), KeyModifiers::NONE);
        render(&mut app, 60, 40);
        assert!(!app.graph_visible());

        // 狭い画面では右の列が出ない
        let first = render(&mut app, 60, 40);
        assert!(shows(&first, "user"), "{first}");
        assert!(!shows(&first, "col8"), "右端はまだ出ない: {first}");
        // **何列目を見ているかを出す** (出さないと右に続きがあると気づけない)
        assert!(shows(&first, "/10 列"), "{first}");

        // 右へ送ると、左端が隠れて右の列が出る
        for _ in 0..9 {
            on_key(&mut app, KeyCode::Right, KeyModifiers::SHIFT);
        }
        let scrolled = render(&mut app, 60, 40);
        assert!(shows(&scrolled, "col8"), "{scrolled}");
        assert!(!shows(&scrolled, "user"), "左端は流れた: {scrolled}");

        // 右端では止まる (巻き戻さない)
        let at_end = app.col_offset();
        on_key(&mut app, KeyCode::Right, KeyModifiers::SHIFT);
        render(&mut app, 60, 40);
        assert_eq!(app.col_offset(), at_end, "右端で止まる");

        // 左へ戻せば元に戻る
        for _ in 0..20 {
            on_key(&mut app, KeyCode::Left, KeyModifiers::SHIFT);
        }
        assert_eq!(app.col_offset(), 0, "左端でも止まる");
        let back = render(&mut app, 60, 40);
        assert!(shows(&back, "user"), "{back}");
    }

    /// 横送りの案内は、右に続きがあるときだけヒント行に出す。
    #[test]
    fn the_hint_mentions_sideways_scrolling_only_when_columns_are_cut_off() {
        let mut app = app_with(&[Some(1.0), Some(2.0)], &[]);
        for i in 0..9 {
            let name: &'static str = Box::leak(format!("col{i}").into_boxed_str());
            for s in &mut app.samples {
                s.activities[0].items[0].rates.push(FieldOut {
                    name,
                    unit: "percent",
                    kind: "counter",
                    raw: None,
                    value: Some(11_111_111.0),
                    text: None,
                    quality: Quality::Ok,
                });
            }
        }
        // 右に続きがある状態でも、ヒント行には出さない
        render(&mut app, 60, 40);
        let screen = render(&mut app, 60, 40);
        assert!(
            app.visible_cols < app.table_columns().len(),
            "列は入り切らない"
        );
        let hint = screen.lines().last().unwrap();
        assert!(shows(hint, "shift←→"), "右に続きがあるなら出す: {hint}");

        // **狭い画面でも消さない。** 狭いほど列は切れるので、そこでこそ要る案内。
        let screen = render(&mut app, 46, 40);
        let hint = screen.lines().last().unwrap();
        assert!(shows(hint, "shift←→"), "短縮版でも残す: {hint}");

        // 列が全部入るなら出さない (押す理由が無い)
        let mut narrow = app_with(&[Some(1.0), Some(2.0)], &[]);
        render(&mut narrow, 80, 40);
        let screen = render(&mut narrow, 80, 40);
        let hint = screen.lines().last().unwrap();
        assert!(!shows(hint, "shift"), "全部入るなら出さない: {hint}");

        // ヘルプには常に載っている
        on_key(&mut app, KeyCode::Char('?'), KeyModifiers::NONE);
        let help = render(&mut app, 80, 40);
        assert!(shows(&help, "表を横に送る"), "{help}");
    }

    /// `shift` の無い矢印は activity の切り替えのまま。
    #[test]
    fn a_plain_arrow_still_switches_activity() {
        let mut app = app_with(&[Some(1.0), Some(2.0)], &[]);
        let before = app.tab;
        on_key(&mut app, KeyCode::Right, KeyModifiers::NONE);
        // タブが 1 つしかない fixture では巡回して同じ位置に戻る
        assert_eq!(app.tab, before);
        assert_eq!(app.col_offset(), 0, "横位置は動かない");
    }

    /// item を替えたら横位置は左端へ戻す。
    #[test]
    fn changing_the_item_resets_the_horizontal_position() {
        let mut app = app_with(&[Some(1.0), Some(2.0)], &[]);
        for i in 0..9 {
            let name: &'static str = Box::leak(format!("col{i}").into_boxed_str());
            for s in &mut app.samples {
                s.activities[0].items[0].rates.push(FieldOut {
                    name,
                    unit: "percent",
                    kind: "counter",
                    raw: None,
                    value: Some(11_111_111.0),
                    text: None,
                    quality: Quality::Ok,
                });
            }
        }
        render(&mut app, 60, 40);
        on_key(&mut app, KeyCode::Right, KeyModifiers::SHIFT);
        assert!(app.col_offset() > 0);
        app.move_item(1);
        assert_eq!(app.col_offset(), 0);
    }

    /// 既定のグラフ列は「値の出る最初の列」。
    ///
    /// 先頭の列がその世代に無いだけで、開いた瞬間に空のグラフを見せない。
    #[test]
    fn the_default_graph_column_is_one_that_has_values() {
        let mut app = app_with(&[Some(1.0), Some(2.0)], &[]);
        // 先頭に「値の出ない列」を挿す
        for s in &mut app.samples {
            s.activities[0].items[0].rates.insert(
                0,
                FieldOut {
                    name: "never",
                    unit: "percent",
                    kind: "counter",
                    raw: None,
                    value: None,
                    text: None,
                    quality: Quality::UnsupportedBySource,
                },
            );
        }
        assert_eq!(app.plottable_columns(), vec!["never", "user"]);
        assert_eq!(
            app.selected_column(),
            Some("user"),
            "値の出ない先頭を選ばない"
        );

        let screen = render(&mut app, 80, 40);
        assert!(!shows(&screen, "描画できる観測がありません"), "{screen}");
    }

    /// `[` / `]` は、表に出していない列を飛ばす。
    #[test]
    fn stepping_skips_columns_hidden_from_the_table() {
        let mut app = app_with(&[Some(1.0), Some(2.0)], &[]);
        for (name, v) in [("b", 2.0), ("c", 3.0)] {
            for s in &mut app.samples {
                s.activities[0].items[0].rates.push(FieldOut {
                    name,
                    unit: "percent",
                    kind: "counter",
                    raw: None,
                    value: Some(v),
                    text: None,
                    quality: Quality::Ok,
                });
            }
        }
        assert_eq!(app.plottable_columns(), vec!["user", "b", "c"]);

        // 真ん中の `b` を表から外す
        app.open_column_picker();
        app.col_picker.select(Some(1));
        app.toggle_draft_column();
        app.commit_columns();
        app.mode = Mode::Normal;
        assert_eq!(app.table_columns(), vec!["user", "c"]);
        assert_eq!(app.steppable_columns(), vec!["user", "c"]);

        // `]` は `b` を飛ばして `c` へ
        assert_eq!(app.selected_column(), Some("user"));
        on_key(&mut app, KeyCode::Char(']'), KeyModifiers::NONE);
        assert_eq!(app.selected_column(), Some("c"), "外した列は飛ばす");
        on_key(&mut app, KeyCode::Char(']'), KeyModifiers::NONE);
        assert_eq!(app.selected_column(), Some("user"), "端で巻き戻る");
        on_key(&mut app, KeyCode::Char('['), KeyModifiers::NONE);
        assert_eq!(app.selected_column(), Some("c"));
    }

    /// グラフ対象が巡回の対象外になっていても `[` / `]` は動く。
    ///
    /// 通常の操作ではこの状態にならない (確定時に表から外れた列は選ばない) が、
    /// item を替えて列の顔ぶれが変わったときに起こり得る。
    /// **押しても動かない、にはしない。**
    #[test]
    fn stepping_from_a_hidden_column_enters_at_the_edge() {
        let mut app = app_with(&[Some(1.0), Some(2.0)], &[]);
        for s in &mut app.samples {
            s.activities[0].items[0].rates.push(FieldOut {
                name: "b",
                unit: "percent",
                kind: "counter",
                raw: None,
                value: Some(2.0),
                text: None,
                quality: Quality::Ok,
            });
        }
        // `b` をグラフ対象にしたまま、表からは外す
        app.graph_target[app.tab] = Some(GraphTarget::Column("b"));
        app.open_column_picker();
        app.col_picker.select(Some(1));
        app.toggle_draft_column();
        app.commit_columns();
        app.mode = Mode::Normal;
        assert_eq!(app.table_columns(), vec!["user"]);
        assert_eq!(app.selected_column(), Some("b"), "グラフには出たまま");

        on_key(&mut app, KeyCode::Char(']'), KeyModifiers::NONE);
        assert_eq!(app.selected_column(), Some("user"));
    }

    /// item を替えて列の顔ぶれが変わっても、別の列に化けない。
    #[test]
    fn the_graph_column_is_remembered_by_name() {
        let mut app = app_with(&[Some(1.0), Some(2.0)], &[]);
        for s in &mut app.samples {
            s.activities[0].items[0].rates.push(FieldOut {
                name: "b",
                unit: "percent",
                kind: "counter",
                raw: None,
                value: Some(2.0),
                text: None,
                quality: Quality::Ok,
            });
        }
        on_key(&mut app, KeyCode::Char(']'), KeyModifiers::NONE);
        assert_eq!(app.selected_column(), Some("b"));
        // 列が消えた item に移ったことにする
        for s in &mut app.samples {
            s.activities[0].items[0].rates.retain(|f| f.name != "b");
        }
        assert_eq!(
            app.selected_column(),
            Some("user"),
            "無くなった列は先頭へ戻す (別の列に化けさせない)"
        );
    }

    /// グラフに描いている列は、表の見出しも同じ色にする。
    #[test]
    fn the_graphed_column_is_highlighted_in_the_table_header() {
        let mut app = app_with(&[Some(1.0), Some(2.0)], &[]);
        for s in &mut app.samples {
            s.activities[0].items[0].rates.push(FieldOut {
                name: "other",
                unit: "percent",
                kind: "counter",
                raw: None,
                value: Some(5.0),
                text: None,
                quality: Quality::Ok,
            });
        }
        assert_eq!(app.selected_column(), Some("user"));

        let fg_of = |app: &mut App, needle: &str| -> Option<Color> {
            let mut terminal = ratatui::Terminal::new(TestBackend::new(80, 40)).unwrap();
            terminal.draw(|f| draw(f, app)).unwrap();
            let buf = terminal.backend().buffer().clone();
            // 表の見出し行を探す (time 列の右に列名が並ぶ行)
            for y in 0..buf.area.height {
                let line: String = (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>();
                if line.contains("time") && line.contains(needle) {
                    let x = line.find(needle).unwrap() as u16;
                    return buf[(x, y)].style().fg;
                }
            }
            None
        };
        // グラフに出ている列だけが線と同じ色になる
        assert_eq!(fg_of(&mut app, "user"), Some(GRAPH_LINE_COLOR));
        assert_ne!(fg_of(&mut app, "other"), Some(GRAPH_LINE_COLOR));

        // 列を送れば、色が付く列も移る
        on_key(&mut app, KeyCode::Char(']'), KeyModifiers::NONE);
        assert_eq!(app.selected_column(), Some("other"));
        assert_eq!(fg_of(&mut app, "other"), Some(GRAPH_LINE_COLOR));
        assert_ne!(fg_of(&mut app, "user"), Some(GRAPH_LINE_COLOR));

        // グラフを隠せば色は付かない (対応する線が無い)
        on_key(&mut app, KeyCode::Char('v'), KeyModifiers::NONE);
        assert!(!app.graph_visible());
        assert_ne!(fg_of(&mut app, "other"), Some(GRAPH_LINE_COLOR));
    }

    /// カーソルは観測値と同じ細さで、色で区別する。
    #[test]
    fn the_cursor_is_thin_and_red() {
        let mut app = app_with(&[Some(1.0), Some(2.0), Some(3.0)], &[]);
        let mut terminal = ratatui::Terminal::new(TestBackend::new(80, 40)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let buf = terminal.backend().buffer().clone();

        let mut red = 0usize;
        let mut cyan = 0usize;
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                let cell = &buf[(x, y)];
                // Braille (U+28xx) で描かれた点だけを数える。
                let braille = cell
                    .symbol()
                    .chars()
                    .next()
                    .is_some_and(|c| ('\u{2800}'..='\u{28ff}').contains(&c));
                if !braille {
                    continue;
                }
                match cell.style().fg {
                    Some(Color::Red) => red += 1,
                    Some(Color::Cyan) => cyan += 1,
                    _ => {}
                }
            }
        }
        assert!(red > 0, "選択時刻のカーソルが赤で出る");
        assert!(cyan > 0, "観測値の線は別の色で出る");
    }

    // -----------------------------------------------------------------------
    // メモリ使用率の比較グラフ
    // -----------------------------------------------------------------------

    /// MEMORY の 1 時刻分。`None` はその列の値が無いこと。
    struct MemRow {
        free: f64,
        /// `memused_pct` (MemAvailable 基準)。`None` は「その世代に無い」。
        avail_based: Option<f64>,
        nocache: Option<f64>,
        withcache: Option<f64>,
    }

    fn pct(name: &'static str, v: Option<f64>, absent: Quality) -> FieldOut {
        FieldOut {
            name,
            unit: "percent",
            kind: "gauge",
            raw: None,
            value: v,
            text: None,
            quality: if v.is_some() { Quality::Ok } else { absent },
        }
    }

    fn kb(name: &'static str, v: f64) -> FieldOut {
        FieldOut {
            name,
            unit: "kB",
            kind: "gauge",
            raw: None,
            value: Some(v),
            text: None,
            quality: Quality::Ok,
        }
    }

    /// 値の無いこともある kB の列。`None` は欠測。
    fn kb_or_missing(name: &'static str, v: Option<f64>) -> FieldOut {
        FieldOut {
            name,
            unit: "kB",
            kind: "gauge",
            raw: None,
            value: v,
            text: None,
            quality: if v.is_some() {
                Quality::Ok
            } else {
                Quality::MissingInSample
            },
        }
    }

    /// テスト用の総量 (kB)。追加列の kB 値を割合から作るためだけに使う。
    const TOTAL_KB: f64 = 8_220_672.0;

    /// 列の並びは layout と同じ
    /// (kbmemfree → memused_pct → 追加の 4 列 → kbcached)。
    fn memory_app(rows: &[MemRow]) -> App {
        let samples = rows
            .iter()
            .enumerate()
            .map(|(i, r)| SampleOut {
                boot: 1,
                start_epoch: T0 + i as u64 * 600,
                end_epoch: T0 + (i as u64 + 1) * 600,
                elapsed_cs: 60_000,
                continuous: true,
                activities: vec![ActivityOut {
                    activity: "A_MEMORY",
                    label: "メモリ・スワップ利用状況",
                    items: vec![ItemOut {
                        item: "-".into(),
                        index: 0,
                        cpu: None,
                        raw: Vec::new(),
                        rates: vec![
                            kb("kbmemfree", r.free),
                            pct("memused_pct", r.avail_based, Quality::UnsupportedBySource),
                            kb_or_missing(
                                "kbmemused_nocache",
                                r.nocache.map(|p| TOTAL_KB * p / 100.0),
                            ),
                            pct("memused_nocache_pct", r.nocache, Quality::MissingInSample),
                            kb_or_missing(
                                "kbmemfree_withcache",
                                r.nocache.map(|p| TOTAL_KB * (100.0 - p) / 100.0),
                            ),
                            pct(
                                "memused_withcache_pct",
                                r.withcache,
                                Quality::MissingInSample,
                            ),
                            kb("kbcached", 2_600_000.0),
                        ],
                    }],
                }],
            })
            .collect();
        App::new(Collected {
            host: host(),
            samples,
            marks: Vec::new(),
            tz: DisplayTz::Utc,
        })
    }

    /// el7 (sysstat 10.1.5) のように MemAvailable を記録しない世代。
    fn el7_rows() -> Vec<MemRow> {
        [
            (345_324.0, 64.0, 95.8),
            (321_536.0, 65.2, 96.1),
            (155_016.0, 66.0, 98.1),
        ]
        .iter()
        .map(|&(free, nocache, withcache)| MemRow {
            free,
            avail_based: None,
            nocache: Some(nocache),
            withcache: Some(withcache),
        })
        .collect()
    }

    /// Braille で描かれたセルのうち、その色のものの数。
    fn braille_cells(app: &mut App, w: u16, h: u16, color: Color) -> usize {
        let mut terminal = ratatui::Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        let buf = terminal.backend().buffer().clone();
        let mut n = 0;
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                let cell = &buf[(x, y)];
                let braille = cell
                    .symbol()
                    .chars()
                    .next()
                    .is_some_and(|c| ('\u{2800}'..='\u{28ff}').contains(&c));
                if braille && cell.style().fg == Some(color) {
                    n += 1;
                }
            }
        }
        n
    }

    /// 表の見出しに出ている列名の文字色。
    fn header_fg(app: &mut App, w: u16, needle: &str) -> Option<Color> {
        let mut terminal = ratatui::Terminal::new(TestBackend::new(w, 40)).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        let buf = terminal.backend().buffer().clone();
        for y in 0..buf.area.height {
            let line: String = (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>();
            if line.contains("time")
                && let Some(x) = line.find(needle)
            {
                return buf[(x as u16, y)].style().fg;
            }
        }
        None
    }

    /// MEMORY は kbmemfree 1 本ではなく、使用率の比較から開く。
    ///
    /// kbmemfree は page cache に使われて小さく見えるのが普通の値で、
    /// それだけを見せるとメモリ不足と読み違える。
    #[test]
    fn memory_opens_on_the_usage_comparison() {
        let mut app = memory_app(&el7_rows());
        assert_eq!(
            app.selected_target(),
            Some(GraphTarget::Preset(&graph::MEMORY_USAGE))
        );
        assert_eq!(app.selected_column(), None, "1 列だけのグラフではない");

        let screen = render(&mut app, 140, 40);
        assert!(
            shows(&screen, "A_MEMORY / - / メモリ使用率の比較 (percent)"),
            "{screen}"
        );
        assert!(shows(&screen, "buffers/cache を除く"), "{screen}");
        assert!(shows(&screen, "buffers/cache を含む"), "{screen}");
        // **記録の無い系列も凡例から消さない。** 無いこと自体を書く。
        assert!(shows(&screen, "MemAvailable 基準 (記録なし)"), "{screen}");
    }

    /// 系列ごとに別の色で描き、値の無い系列は線を引かない。
    #[test]
    fn the_comparison_draws_each_series_in_its_own_colour() {
        let mut app = memory_app(&el7_rows());
        assert!(braille_cells(&mut app, 100, 40, Color::Cyan) > 0, "除く");
        assert!(braille_cells(&mut app, 100, 40, Color::Magenta) > 0, "含む");
        assert_eq!(
            braille_cells(&mut app, 100, 40, Color::Green),
            0,
            "記録の無い系列は 0 の線にしない"
        );

        // MemAvailable を記録している世代なら 3 本とも出る
        let mut rows = el7_rows();
        for (r, v) in rows.iter_mut().zip([61.0, 62.5, 63.0]) {
            r.avail_based = Some(v);
        }
        let mut app = memory_app(&rows);
        assert!(braille_cells(&mut app, 100, 40, Color::Green) > 0);
        let screen = render(&mut app, 140, 40);
        assert!(shows(&screen, "MemAvailable 基準"), "{screen}");
        assert!(!shows(&screen, "記録なし"), "{screen}");
    }

    /// 比較グラフの系列の列は、表の見出しもそれぞれの線の色になる。
    #[test]
    fn the_comparison_colours_each_graphed_header() {
        let mut app = memory_app(&el7_rows());
        assert_eq!(
            header_fg(&mut app, 120, "memused_nocache_pct"),
            Some(Color::Cyan)
        );
        assert_eq!(
            header_fg(&mut app, 120, "memused_withcache_pct"),
            Some(Color::Magenta)
        );
        // 線の無い列 (kbmemfree) には色を付けない
        let free = header_fg(&mut app, 120, "kbmemfree");
        assert!(
            !matches!(free, Some(Color::Cyan | Color::Magenta | Color::Green)),
            "{free:?}"
        );
    }

    /// `]` で 1 列のグラフへ出て、`[` で比較グラフへ戻る。
    #[test]
    fn brackets_leave_and_return_to_the_comparison() {
        let mut app = memory_app(&el7_rows());
        on_key(&mut app, KeyCode::Char(']'), KeyModifiers::NONE);
        assert_eq!(app.selected_column(), Some("kbmemfree"));
        let screen = render(&mut app, 120, 40);
        assert!(shows(&screen, "A_MEMORY / - / kbmemfree (kB)"), "{screen}");

        on_key(&mut app, KeyCode::Char('['), KeyModifiers::NONE);
        assert_eq!(
            app.selected_target(),
            Some(GraphTarget::Preset(&graph::MEMORY_USAGE))
        );
        // 巻き戻しでも比較グラフを通る
        on_key(&mut app, KeyCode::Char('['), KeyModifiers::NONE);
        assert_eq!(app.selected_column(), Some("kbcached"), "末尾の列へ");
        on_key(&mut app, KeyCode::Char(']'), KeyModifiers::NONE);
        assert_eq!(
            app.selected_target(),
            Some(GraphTarget::Preset(&graph::MEMORY_USAGE))
        );
    }

    /// 列ピッカーでカーソルを動かさなければ、比較グラフのまま。
    ///
    /// 表の列を直しただけで、カーソルの置かれた 1 列のグラフへ切り替わると
    /// 「表をいじったらグラフまで変わった」になる。
    #[test]
    fn the_picker_keeps_the_comparison_when_the_cursor_stays() {
        let mut app = memory_app(&el7_rows());
        on_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE);
        on_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(
            app.selected_target(),
            Some(GraphTarget::Preset(&graph::MEMORY_USAGE))
        );

        // カーソルを動かして確定すれば、その列のグラフになる
        // (カーソルは先頭の系列 memused_nocache_pct にあり、下は kbmemfree_withcache)
        on_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE);
        on_key(&mut app, KeyCode::Down, KeyModifiers::NONE);
        on_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.selected_column(), Some("kbmemfree_withcache"));

        // 値の無い列 (表から外れている) へ動かしても、グラフは空にならない
        app.graph_target[app.tab] = None;
        on_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE);
        on_key(&mut app, KeyCode::Up, KeyModifiers::NONE); // kbmemused_nocache
        on_key(&mut app, KeyCode::Up, KeyModifiers::NONE); // memused_pct (記録なし)
        on_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(
            app.selected_target(),
            Some(GraphTarget::Preset(&graph::MEMORY_USAGE))
        );
    }

    /// 系列に 1 点も値が無ければ、比較グラフを既定にしない。
    #[test]
    fn a_comparison_without_values_is_not_the_default() {
        let rows: Vec<MemRow> = (0..3)
            .map(|i| MemRow {
                free: 300_000.0 + i as f64,
                avail_based: None,
                nocache: None,
                withcache: None,
            })
            .collect();
        let mut app = memory_app(&rows);
        assert_eq!(app.selected_column(), Some("kbmemfree"));
        assert!(
            !app.graph_cycle()
                .iter()
                .any(|t| matches!(t, GraphTarget::Preset(_)))
        );
        let screen = render(&mut app, 120, 40);
        assert!(!shows(&screen, "描画できる観測がありません"), "{screen}");
    }

    /// 凡例は幅に合わせて短くし、入らなければ下辺へ移す。題名とは重ならない。
    #[test]
    fn the_legend_adapts_to_the_terminal_width() {
        for w in [60u16, 80, 100, 140] {
            let mut app = memory_app(&el7_rows());
            let screen = render(&mut app, w, 40);
            assert!(
                shows(&screen, "メモリ使用率の比較"),
                "幅 {w} で題名が残る: {screen}"
            );
            assert!(shows(&screen, "除く"), "幅 {w} で凡例が出る: {screen}");
            assert!(shows(&screen, "含む"), "幅 {w} で凡例が出る: {screen}");
            assert!(shows(&screen, "MemAvailable"), "幅 {w}: {screen}");
        }
        // 広い端末では省略しない名前で出す
        let mut app = memory_app(&el7_rows());
        let wide = render(&mut app, 140, 40);
        assert!(shows(&wide, "⣿ buffers/cache を除く"), "{wide}");
    }

    /// 欠測は「記録なし」と言わない (その世代に無いのとは別の理由)。
    #[test]
    fn a_missing_series_is_not_called_unrecorded() {
        let mut rows = el7_rows();
        for r in &mut rows {
            r.withcache = None; // MissingInSample
        }
        let mut app = memory_app(&rows);
        let screen = render(&mut app, 140, 40);
        assert!(shows(&screen, "buffers/cache を含む (値なし)"), "{screen}");
        assert!(shows(&screen, "MemAvailable 基準 (記録なし)"), "{screen}");
    }

    // -----------------------------------------------------------------------
    // 表の下の列の説明
    // -----------------------------------------------------------------------

    /// 画面の行 (全角の 2 桁目の空白は詰める)。
    fn screen_lines(screen: &str) -> Vec<String> {
        screen
            .lines()
            .map(|l| l.chars().filter(|c| !c.is_whitespace()).collect())
            .collect()
    }

    /// 追加の 4 列が何の値かを、表の下 (キーヒントの上) に列ごとに書く。
    #[test]
    fn memory_explains_the_added_columns_below_the_table() {
        let mut app = memory_app(&el7_rows());
        let screen = render(&mut app, 160, 44);
        for n in notes::MEMORY_NOTES.notes {
            assert!(shows(&screen, n.text), "{}: {screen}", n.column);
        }
        assert!(shows(&screen, notes::MEMORY_NOTES.caveat), "{screen}");

        // 位置: 表の枠の下、キーヒントの上
        let lines = screen_lines(&screen);
        let strip = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
        let note_at = lines
            .iter()
            .position(|l| l.contains(&strip(notes::MEMORY_NOTES.notes[0].text)))
            .expect("説明の行");
        let table_bottom = lines.iter().rposition(|l| l.starts_with('└')).unwrap();
        assert!(note_at > table_bottom, "表の下: {note_at} > {table_bottom}");
        assert!(note_at < lines.len() - 1, "キーヒントの上");
        assert!(
            shows(lines.last().unwrap(), "esc"),
            "最後の行はキーヒントのまま"
        );
    }

    /// 説明は表に出ている列のものだけ。`c` で外した列の説明は消える。
    #[test]
    fn notes_follow_the_columns_shown_in_the_table() {
        let mut app = memory_app(&el7_rows());
        let all = app.columns();
        let pos = all
            .iter()
            .position(|c| *c == "kbmemfree_withcache")
            .unwrap();
        app.open_column_picker();
        app.col_picker.select(Some(pos));
        app.toggle_draft_column();
        app.commit_columns();
        app.mode = Mode::Normal;
        assert!(!app.table_columns().contains(&"kbmemfree_withcache"));

        let screen = render(&mut app, 160, 44);
        let hidden = notes::MEMORY_NOTES
            .notes
            .iter()
            .find(|n| n.column == "kbmemfree_withcache")
            .unwrap();
        assert!(
            !shows(&screen, hidden.text),
            "外した列の説明は出さない: {screen}"
        );
        assert!(
            shows(&screen, notes::MEMORY_NOTES.notes[0].text),
            "表に残っている列の説明は出る: {screen}"
        );
    }

    /// 全文が入らない幅では短い版にし、それも入らなければ `?` へ案内する 1 行にする。
    ///
    /// 列名と説明が途中で切れると対応が崩れるので、切れた説明は並べない。
    #[test]
    fn a_narrow_terminal_shortens_the_notes_or_points_to_help() {
        let short = &notes::MEMORY_NOTES.notes[0].short;
        let full = &notes::MEMORY_NOTES.notes[0].text;

        let mut app = memory_app(&el7_rows());
        let mid = render(&mut app, 90, 44);
        assert!(shows(&mid, short), "短い版: {mid}");
        assert!(!shows(&mid, full), "全文は入らない: {mid}");

        // 全文が入る行があっても、入らない行があれば全部を短い版にそろえる
        let wide_enough_for_some = render(&mut app, 100, 44);
        let pct = notes::MEMORY_NOTES
            .notes
            .iter()
            .find(|n| n.column == "memused_nocache_pct")
            .unwrap();
        assert!(
            shows(&wide_enough_for_some, pct.short) && !shows(&wide_enough_for_some, pct.text),
            "行ごとに全文と短い版を混ぜない: {wide_enough_for_some}"
        );

        let narrow = render(&mut app, 60, 44);
        assert!(
            shows(&narrow, notes::MEMORY_NOTES.summary_short),
            "{narrow}"
        );
        assert!(!shows(&narrow, short), "列ごとには並べない: {narrow}");

        // どの幅でもキーヒントは最後の行に残る
        for w in [60u16, 90, 160] {
            let screen = render(&mut app, w, 44);
            assert!(shows(screen.lines().last().unwrap(), "esc"), "幅 {w}");
        }
    }

    /// 低い端末では 1 行に畳む。**表を潰してまで説明を出さない。**
    #[test]
    fn notes_never_starve_the_table() {
        // 表と説明欄に使える行数に応じた形 (説明する列が 4 つ = 全文は 5 行)
        assert_eq!(note_layout(4, 30, true), NoteLayout::Full);
        assert_eq!(note_layout(4, TABLE_KEEP + 5, true), NoteLayout::Full);
        assert_eq!(note_layout(4, TABLE_KEEP + 4, true), NoteLayout::Summary);
        assert_eq!(note_layout(4, TABLE_KEEP + 1, true), NoteLayout::Summary);
        assert_eq!(note_layout(4, TABLE_KEEP, true), NoteLayout::None);
        // 幅が足りなければ高さがあっても要約
        assert_eq!(note_layout(4, 30, false), NoteLayout::Summary);
        // 説明する列が表に無ければ出さない
        assert_eq!(note_layout(0, 30, true), NoteLayout::None);

        // 実際の画面: 低い端末でも表の行は残る
        let mut app = memory_app(&el7_rows());
        let screen = render(&mut app, 160, 16);
        assert!(
            shows(&screen, notes::MEMORY_NOTES.summary),
            "1 行に畳む: {screen}"
        );
        assert!(shows(&screen, "00:30:00"), "表の最終行まで見える: {screen}");
    }

    /// グラフに描いている列は、説明の列名も線と同じ色 (表の見出しと同じ規則)。
    #[test]
    fn a_graphed_column_is_coloured_in_the_notes_too() {
        let fg_in_notes = |app: &mut App, needle: &str| -> Option<Color> {
            let mut terminal = ratatui::Terminal::new(TestBackend::new(160, 44)).unwrap();
            terminal.draw(|f| draw(f, app)).unwrap();
            let buf = terminal.backend().buffer().clone();
            // 下から探す (説明欄は表より下にある)
            for y in (0..buf.area.height).rev() {
                let line: String = (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>();
                if line.starts_with(' ')
                    && let Some(x) = line.find(needle)
                    && !line.contains("time")
                {
                    return buf[(x as u16, y)].style().fg;
                }
            }
            None
        };
        let mut app = memory_app(&el7_rows());
        assert_eq!(
            fg_in_notes(&mut app, "memused_nocache_pct  "),
            Some(Color::Cyan)
        );
        assert_eq!(
            fg_in_notes(&mut app, "memused_withcache_pct  "),
            Some(Color::Magenta)
        );
        // 線を引いていない列には色を付けない
        let plain = fg_in_notes(&mut app, "kbmemused_nocache  ");
        assert!(
            !matches!(plain, Some(Color::Cyan | Color::Magenta | Color::Green)),
            "{plain:?}"
        );
        // グラフを隠せば色は付かない (対応する線が無い)
        on_key(&mut app, KeyCode::Char('v'), KeyModifiers::NONE);
        assert!(!app.graph_visible());
        let hidden = fg_in_notes(&mut app, "memused_nocache_pct  ");
        assert_ne!(hidden, Some(Color::Cyan));
    }

    /// 説明を持たない activity では説明欄を出さない。
    #[test]
    fn other_activities_have_no_notes() {
        let mut app = app_with(&[Some(1.0), Some(2.0)], &[]);
        assert!(app.shown_notes().is_none());
        let screen = render(&mut app, 120, 40);
        assert!(!shows(&screen, "注:"), "{screen}");
    }

    /// 列ごとの式は `?` のヘルプでも読める (説明欄を畳んだときの全文への道筋)。
    #[test]
    fn the_help_lists_what_each_added_column_is() {
        let mut app = memory_app(&el7_rows());
        on_key(&mut app, KeyCode::Char('?'), KeyModifiers::NONE);
        let help = render(&mut app, 80, 44);
        for col in [
            "kbmemused_nocache",
            "memused_nocache_pct",
            "kbmemfree_withcache",
            "memused_withcache_pct",
        ] {
            assert!(shows(&help, col), "{col}: {help}");
        }
        assert!(shows(&help, "total-free-buffers-cached"), "{help}");
        assert!(shows(&help, "正確な空きは kbavail"), "{help}");
    }

    /// 実ファイルの描画を目で見るための一時確認。
    #[test]
    #[ignore = "目視確認用"]
    fn preview_real_file() {
        let path = std::path::Path::new("target/fixtures/upstream/data-ppc-11.7.2");
        if !path.exists() {
            eprintln!("fixture なし");
            return;
        }
        let file = crate::format::SaFile::open_with(path, Default::default()).unwrap();
        let cfg = crate::output::table::default_config();
        let collected = prepare(&file, &cfg).unwrap().collected;
        let mut app = App::new(collected);
        println!("{}", render(&mut app, 100, 36));
    }

    #[test]
    fn absent_is_not_zero() {
        // 値が無いセルを 0 と書いてはいけない。
        assert_eq!(cell_text(None), ABSENT);
    }

    #[test]
    fn numbers_keep_two_decimals_until_they_get_large() {
        assert_eq!(format_number(0.0), "0.00");
        assert_eq!(format_number(12.345), "12.35");
        assert_eq!(format_number(1234.5), "1234.5");
        assert_eq!(format_number(123456.0), "123456");
        assert_eq!(format_number(f64::NAN), ABSENT);
    }

    /// IRQ のように次元を持つ item は、名前だけでは区別できない。
    #[test]
    fn item_names_carry_their_extra_dimension() {
        assert_eq!(display_item("LOC", Some("all")), "LOC@all");
        assert_eq!(display_item("sda", None), "sda");
    }

    #[test]
    fn hhmmss_wraps_within_a_day() {
        let utc = DisplayTz::Utc;
        assert_eq!(hhmmss(utc, 0), "00:00:00");
        assert_eq!(hhmmss(utc, 3661), "01:01:01");
        assert_eq!(hhmmss(utc, 86_399), "23:59:59");
        assert_eq!(hhmmss(utc, 86_400), "00:00:00");
    }

    /// 表示タイムゾーンが時刻に効く (`epoch % 86_400` では効かない)。
    #[test]
    fn hhmmss_follows_the_display_timezone() {
        let jst = DisplayTz::parse("Asia/Tokyo").unwrap();
        // 2019-06-30 05:39:33 UTC = 同日 14:39:33 JST
        assert_eq!(hhmmss(jst, 1_561_873_173), "14:39:33");
        assert_eq!(hhmmss(DisplayTz::Utc, 1_561_873_173), "05:39:33");
    }
}

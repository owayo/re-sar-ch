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

pub mod graph;

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

/// ファイルを 1 回走査して観測値を集める。
///
/// 描画のたびにファイルを読まない。静的なファイルなので、
/// 開いた時点の内容がすべてである。
fn collect(file: &SaFile, cfg: &CustomConfig) -> Result<Collected> {
    let host = HostOut::new(file, cfg.tz);
    let mut samples = Vec::new();
    let mut marks = Vec::new();
    let mut boot = BootCounter::default();

    walk_items(file, &cfg.selection.clone(), |item| {
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

    Ok(Collected {
        host,
        samples,
        marks,
        tz: cfg.tz,
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
    /// グラフに描く列を選ぶポップアップ。
    PickColumn,
    /// 絞り込み入力中。**ここでは `q` は文字であって終了ではない**。
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
    tabs: Vec<TabInfo>,
    /// 選択中のタブ。
    tab: usize,
    /// タブごとの選択 item。タブを切り替えても選択を保つ。
    item: Vec<usize>,
    /// タブごとのグラフ対象列。タブを切り替えても選択を保つ。
    col: Vec<usize>,
    /// グラフを出すか。
    graph: GraphVisibility,
    /// 直近の描画で使った端末の高さ。`v` の判定に使う。
    last_height: u16,
    table: TableState,
    picker: ListState,
    /// 列ピッカーの選択位置。
    col_picker: ListState,
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
        let col = vec![0; tabs.len()];
        let mut table = TableState::default();
        table.select(Some(0));
        App {
            host: c.host,
            samples: c.samples,
            marks: c.marks,
            tz: c.tz,
            col,
            graph: GraphVisibility::default(),
            last_height: 0,
            col_picker: ListState::default(),
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

    /// グラフに描く列。描ける列が無ければ `None`。
    fn selected_column(&self) -> Option<&'static str> {
        let cols = self.plottable_columns();
        if cols.is_empty() {
            return None;
        }
        let i = self.col.get(self.tab).copied().unwrap_or(0);
        Some(cols[i.min(cols.len() - 1)])
    }

    fn move_col(&mut self, delta: isize) {
        let n = self.plottable_columns().len();
        if n == 0 {
            return;
        }
        let Some(slot) = self.col.get_mut(self.tab) else {
            return;
        };
        let cur = (*slot).min(n - 1) as isize;
        *slot = (((cur + delta) % n as isize + n as isize) % n as isize) as usize;
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
    let chunks = Layout::vertical([
        Constraint::Length(2),  // ヘッダ
        Constraint::Length(1),  // タブ
        Constraint::Length(gh), // グラフ (0 なら出ない)
        Constraint::Min(3),     // 表
        Constraint::Length(1),  // キーヒント
    ])
    .split(area);

    draw_header(f, chunks[0], app);
    draw_tabs(f, chunks[1], app);
    if gh > 0 {
        draw_graph(f, chunks[2], app);
    }
    draw_table(f, chunks[3], app);
    draw_hint(f, chunks[4], app);

    match app.mode {
        Mode::PickItem => draw_picker(f, area, app),
        Mode::PickColumn => draw_column_picker(f, area, app),
        Mode::Help => draw_help(f, area),
        _ => {}
    }
}

/// 選択中の 1 系列を折れ線で描く。
///
/// **不連続と欠測ごとに `Dataset` を分ける。** 1 本の点列に混ぜて
/// 飛び越えた線を引くと、観測していない区間をあたかも観測したかのように見せる。
fn draw_graph(f: &mut Frame, area: Rect, app: &App) {
    let (Some(act), Some(item)) = (app.current_activity(), app.selected_item_name()) else {
        return;
    };
    let Some(col) = app.selected_column() else {
        f.render_widget(
            Paragraph::new("グラフに描ける数値の列がありません")
                .style(Style::default().fg(Color::DarkGray))
                .block(Block::bordered().title(" グラフ ")),
            area,
        );
        return;
    };
    let view = graph::GraphView::build(&app.samples, act, &item, col);

    let unit = if view.unit.is_empty() {
        String::new()
    } else {
        format!(" ({})", view.unit)
    };
    let title = format!(" {act} / {item} / {col}{unit} ");

    if view.is_empty() {
        // **空の軸を出さない。** 「描けなかった」ことと「なぜか」を書く。
        let mut lines = vec![Line::from("この範囲に描画できる観測がありません")];
        if !view.absent.is_empty() {
            lines.push(Line::from(Span::styled(
                view.absent_summary(),
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

    // 表で選んでいる時刻に縦線を立てる。表とグラフが同じ時刻を指していることを
    // 見せるため。**値が欠測の時刻でも線は立つ** (1 点の散布では消えてしまう)。
    let cursor: Option<[(f64, f64); 2]> = app
        .table
        .selected()
        .and_then(|i| app.samples.get(i))
        .map(|s| {
            let x = (s.end_epoch as f64) - (view.x_origin as f64);
            [(x, view.y_bounds[0]), (x, view.y_bounds[1])]
        });

    let mut datasets: Vec<Dataset> = Vec::with_capacity(view.segments.len() + 1);
    if let Some(c) = cursor.as_ref() {
        datasets.push(
            Dataset::default()
                .graph_type(GraphType::Line)
                // 観測値と同じ Braille で細く引く。太いマーカーだと
                // カーソルが値の線より目立って、形が読み取りにくい。
                .marker(symbols::Marker::Braille)
                .style(Style::default().fg(Color::Red))
                .data(c),
        );
    }
    for seg in &view.segments {
        datasets.push(
            Dataset::default()
                .graph_type(GraphType::Line)
                .marker(symbols::Marker::Braille)
                .style(Style::default().fg(Color::Cyan))
                .data(seg),
        );
    }

    let gap = view.segments.len().saturating_sub(1);
    // 線の切れ目は表の `!` / `R` と同じ意味。**色だけに頼らず語で書く。**
    // 軸の title には出さない (Y 軸ラベルと重なって読めなくなる)。
    let note = if gap > 0 {
        format!(" 切れ目 {gap} (不連続・欠測) ")
    } else {
        String::new()
    };
    let y_label_w = view
        .y_labels()
        .iter()
        .map(|l| l.chars().count())
        .max()
        .unwrap_or(0) as u16;
    let plot_width = area.width.saturating_sub(y_label_w + 3);

    let chart = Chart::new(datasets)
        .block(
            Block::bordered()
                .title(title)
                .title_bottom(Line::from(note).right_aligned()),
        )
        .x_axis(
            Axis::default()
                .style(Style::default().fg(Color::DarkGray))
                .bounds(view.x_bounds)
                // 軸に使える幅は、枠 (2) と Y 軸ラベルの分を引いた残り。
                // ここを渡さないと刻みが幅に追従せず、広い画面でも両端だけになる。
                .labels(view.x_labels(app.tz, plot_width)),
        )
        .y_axis(
            Axis::default()
                .style(Style::default().fg(Color::DarkGray))
                .bounds(view.y_bounds)
                .labels(view.y_labels()),
        );
    f.render_widget(chart, area);
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
            "{}  {}  時刻は {}  ({} サンプル)",
            h.file_date,
            h.source,
            // 先頭サンプルの時点で解決する (夏時間のある地域では時期で変わる)
            app.tz
                .label_at(app.samples.first().map_or(0, |s| s.end_epoch)),
            app.samples.len()
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
    let cols = app.columns();
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

    let title = match &item_name {
        Some(n) => format!(" {} — {}  [{}]  ", act, app.tabs[app.tab].label, n),
        None => format!(" {} — {} ", act, app.tabs[app.tab].label),
    };

    let mut header: Vec<Cell> = vec![Cell::from("time")];
    header.extend(cols.iter().map(|c| Cell::from(*c)));

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

    let mut widths = vec![Constraint::Length(10)];
    widths.extend(
        cols.iter()
            .map(|c| Constraint::Length((c.len() as u16).max(8))),
    );

    let table = Table::new(rows, widths)
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
                // **押しても効かないキーを案内しない。** 端末が低くてグラフを
                // 出せないときは、限られた 1 行をグラフの説明で埋めない。
                let graph = if app.graph_visible() {
                    "c/[] 列  v 図  "
                } else if graph_height(GraphVisibility::Shown, app.last_height) > 0 {
                    "v 図  "
                } else {
                    ""
                };
                let full = format!(
                    "←→ activity  ↑↓ 時刻  i item ({n})  / 絞り込み  g/G 先頭末尾  {graph}? help  q 終了"
                );
                // 幅に入らないときは短い方を出す。途中で切れて語の途中で
                // 終わるより、短くても最後まで読める方が案内になる。
                if text_width(&full) <= area.width as usize {
                    full
                } else {
                    format!("←→ act  ↑↓ 時刻  i item  {graph}? help  q")
                }
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

/// グラフに描く列を選ぶポップアップ。
///
/// **描ける列だけを並べる。** デバイス名のような識別子の列を混ぜると、
/// 選んでも線が出ない選択肢を見せることになる。
fn draw_column_picker(f: &mut Frame, area: Rect, app: &mut App) {
    let cols = app.plottable_columns();
    if cols.is_empty() {
        return;
    }
    let h = (cols.len() as u16 + 2)
        .min(area.height.saturating_sub(2))
        .max(3);
    let w = cols
        .iter()
        .map(|s| s.chars().count() as u16)
        .max()
        .unwrap_or(10)
        .clamp(20, area.width.saturating_sub(4));
    let r = centered(area, w + 4, h);
    f.render_widget(Clear, r);
    let list = List::new(
        cols.iter()
            .map(|s| ListItem::new((*s).to_string()))
            .collect::<Vec<_>>(),
    )
    .block(Block::bordered().title(" グラフの列 (Enter 決定 / Esc 取消) "))
    .highlight_style(Style::default().bg(Color::DarkGray));
    app.col_picker.select(Some(
        app.col
            .get(app.tab)
            .copied()
            .unwrap_or(0)
            .min(cols.len() - 1),
    ));
    f.render_stateful_widget(list, r, &mut app.col_picker);
}

fn draw_help(f: &mut Frame, area: Rect) {
    let lines = vec![
        Line::from("←/→        activity を切り替える"),
        Line::from("↑/↓        時刻を移動する"),
        Line::from("PgUp/PgDn  10 行ずつ移動する"),
        Line::from("g / G      先頭 / 末尾"),
        Line::from("i          item を選ぶ"),
        Line::from("/          item を名前で絞り込む"),
        Line::from("c          グラフに描く列を選ぶ"),
        Line::from("[ / ]      グラフの列を前 / 次へ"),
        Line::from("v          グラフの表示を切り替える"),
        Line::from("?          このヘルプ"),
        Line::from("q / Ctrl-C 終了"),
        Line::from(""),
        Line::from(format!("{ABSENT} は値が無いこと。0 ではない。")),
        Line::from("時刻の前の ! は、直前との間に不連続があること。"),
        Line::from("グラフの線は、不連続と欠測のところで切れる。"),
        Line::from("切れ目を飛び越えて結ばないのは、その間を観測していないため。"),
    ];
    let r = centered(area, 60, lines.len() as u16 + 2);
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
            // 入力中の `q` は文字であって終了ではない。
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
            KeyCode::Esc | KeyCode::Char('c') | KeyCode::Enter => app.mode = Mode::Normal,
            KeyCode::Up => app.move_col(-1),
            KeyCode::Down => app.move_col(1),
            _ => {}
        },
        Mode::Help => app.mode = Mode::Normal,
        Mode::Normal => match code {
            KeyCode::Char('q') => app.quit = true,
            KeyCode::Left => app.move_tab(-1),
            KeyCode::Right => app.move_tab(1),
            KeyCode::Up => app.move_row(-1),
            KeyCode::Down => app.move_row(1),
            KeyCode::PageUp => app.move_row(-10),
            KeyCode::PageDown => app.move_row(10),
            KeyCode::Char('g') => app.table.select(Some(0)),
            KeyCode::Char('G') => {
                let last = app.samples.len().saturating_sub(1);
                app.table.select(Some(last));
            }
            KeyCode::Char('i') => app.mode = Mode::PickItem,
            KeyCode::Char('c') => app.mode = Mode::PickColumn,
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
    let collected = collect(file, cfg)?;
    if collected.samples.is_empty() {
        return Err(crate::error::Error::Other(format!(
            "{}: 表示できるサンプルがありません",
            path.display()
        )));
    }
    let mut app = App::new(collected);

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
        assert!(!shows(&screen, "v 図"), "{screen}");
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
        let mut app = app_with(&[Some(1.0), Some(2.0), Some(3.0)], &[]);
        for w in [80u16, 100, 120] {
            let screen = render(&mut app, w, 40);
            let hint = screen.lines().last().unwrap();
            assert!(shows(hint, "q"), "幅 {w} で末尾まで出る: {hint}");
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
        let collected = collect(&file, &cfg).unwrap();
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

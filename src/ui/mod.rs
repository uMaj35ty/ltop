pub mod graph;

use std::sync::atomic::{AtomicBool, AtomicU16, Ordering::Relaxed};
use std::sync::Mutex;

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Cell, Clear, Paragraph, Row, Table};
use ratatui::Frame;

use crate::app::{
    fmt_dur, fmt_interval, local_time_str, App, IoGraphMode, LayoutMode, MenuState, BOX_CONNS,
    BOX_DISKIO, BOX_DISKS, BOX_LAN, BOX_NET, BOX_PROCS,
};
use crate::collect::conn::{sock_str, Conn, ConnFilter, ConnSort};
use crate::collect::disk::TreeNode;
use crate::collect::lan::Neighbor;
use crate::config::{Config, Lang};
use crate::theme::*;

pub use graph::GraphMode;
use graph::{braille_bar, braille_graph};

const GRAPH_FLOOR: f64 = 64.0 * 1024.0; // keep idle graphs from being pure noise
pub(crate) const THEME_VISIBLE: usize = 14;

/// Screen rects of the visible boxes this frame, for mouse-wheel scrolling
static BOX_AREAS: Mutex<Vec<(usize, Rect)>> = Mutex::new(Vec::new());

/// Box index under the given terminal position, if any
pub fn hit_box(col: u16, row: u16) -> Option<usize> {
    BOX_AREAS
        .lock()
        .unwrap()
        .iter()
        .find(|(_, r)| col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height)
        .map(|(i, _)| *i)
}

/// Full rect of a box, for mouse hit-testing of list rows
pub fn box_rect(idx: usize) -> Option<Rect> {
    BOX_AREAS
        .lock()
        .unwrap()
        .iter()
        .find(|(i, _)| *i == idx)
        .map(|(_, r)| *r)
}

pub fn draw(f: &mut Frame, app: &App) {
    let area = f.area();
    // detail 弹窗未打开时,鼠标命中缓存的弹窗几何即刻失效
    DETAIL_VALID.store(matches!(app.menu, MenuState::Detail(..)), Relaxed);
    if let Some(bg) = bg() {
        f.render_widget(Block::new().style(Style::new().bg(bg)), area);
    }
    BOX_AREAS.lock().unwrap().clear();

    let outer = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(area);

    let zoom_active = app.zoom.is_some_and(|b| {
        matches!(
            b,
            crate::app::BOX_LAN | crate::app::BOX_CONNS | crate::app::BOX_PROCS
        ) && app.visible[b]
    });

    if zoom_active {
        // zoom:焦点盒独占主区;footer 与 popup(MenuState 分派)照常渲染
        if let Some(b) = app.zoom {
            draw_box(f, outer[0], app, b);
        }
    } else {
        let graph_ids: Vec<usize> = [BOX_NET, BOX_DISKIO]
            .into_iter()
            .filter(|&i| app.visible[i])
            .collect();
        let table_ids: Vec<usize> = [BOX_LAN, BOX_CONNS, BOX_PROCS, BOX_DISKS]
            .into_iter()
            .filter(|&i| app.visible[i])
            .collect();

        match (graph_ids.is_empty(), table_ids.is_empty()) {
            (false, false) => match app.layout {
                LayoutMode::Classic => {
                    // tables first: they need ~76 columns for full rates/addresses;
                    // the graphs give up width down to 40 columns
                    let (graphs_w, _) = split_horizontal(outer[0].width, 76, 40);
                    let main =
                        Layout::horizontal([Constraint::Length(graphs_w), Constraint::Fill(1)])
                            .split(outer[0]);
                    draw_stack(f, main[0], app, &graph_ids, true);
                    // 右列在中线分组:LAN+CONNS 的底边与左列 NET|DISK 分界对齐
                    draw_list_stack(f, main[1], app, &table_ids, true);
                }
                LayoutMode::Wide | LayoutMode::Tall => {
                    // lists keep their ideal height; graphs absorb the remainder
                    // and only shrink to GRAPH_MIN_ROWS (each) before lists scroll
                    let stacked_graphs = app.layout == LayoutMode::Tall;
                    let graph_min = GRAPH_MIN_ROWS
                        * if stacked_graphs {
                            graph_ids.len() as u16
                        } else {
                            1
                        };
                    let ideal = tables_ideal_h(app, &table_ids, outer[0].width);
                    let (g_desired, t_desired) = split_vertical(outer[0].height, graph_min, ideal);
                    let hs = VERT_ALLOC.lock().unwrap().update(
                        vec![g_desired, t_desired],
                        outer[0].height,
                        std::time::Instant::now(),
                    );
                    let graphs_h = hs.first().copied().unwrap_or(g_desired);
                    let main =
                        Layout::vertical([Constraint::Length(graphs_h), Constraint::Fill(1)])
                            .split(outer[0]);
                    if stacked_graphs {
                        draw_stack(f, main[0], app, &graph_ids, true);
                    } else {
                        draw_row(f, main[0], app, &graph_ids);
                    }
                    draw_table_grid(f, main[1], app, &table_ids);
                }
            },
            (false, true) => {
                draw_stack(f, outer[0], app, &graph_ids, app.layout == LayoutMode::Tall)
            }
            (true, false) => draw_table_grid(f, outer[0], app, &table_ids),
            (true, true) => {}
        }
    }

    match app.menu {
        MenuState::None if app.zoom.is_some() => {
            // zoom 态 footer:该状态下可用的键(与正常态不同)
            let lang = app.lang;
            let key = |k: &str, label: &str| footer_key(k, label);
            let mut spans: Vec<Span> = vec![Span::styled(" ", dim())];
            spans.extend(key("z", lang.tr("exit_zoom")));
            spans.extend(key("esc", lang.tr("unfocus")));
            spans.extend(key("enter", lang.tr("detail_desc")));
            spans.extend(key("space", lang.tr("pause")));
            spans.extend(key("/", lang.tr("search")));
            f.render_widget(Paragraph::new(Line::from(spans)), outer[1]);
        }
        MenuState::None => draw_footer(f, outer[1], app),
        MenuState::Search => {
            // 搜索模式:footer 左侧变输入行,实时回显缓冲
            let cols =
                Layout::horizontal([Constraint::Min(0), Constraint::Length(9)]).split(outer[1]);
            let target = if app.search_box == BOX_PROCS {
                "procs"
            } else {
                "conns"
            };
            let q = &app.search_buf;
            let mut spans = vec![
                Span::styled("[/] ", Style::new().fg(hi())),
                Span::styled(
                    format!("{q}▏ "),
                    Style::new().fg(if q.is_empty() {
                        palette().dim
                    } else {
                        palette().text
                    }),
                ),
            ];
            // 与主页按键同款 [键] 标签 样式;[tab] 的标签即当前目标盒
            spans.extend(footer_key("tab", target));
            spans.extend(footer_key("enter", app.lang.tr("filter")));
            spans.extend(footer_key("esc", app.lang.tr("clear")));
            f.render_widget(Paragraph::new(Line::from(spans)), cols[0]);
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    format!("{} ", local_time_str()),
                    Style::new().fg(accent()),
                ))),
                cols[1],
            );
        }
        MenuState::Main(sel) => draw_menu_footer(f, outer[1], sel, app.lang),
        MenuState::Theme(sel, scroll) => {
            draw_menu_footer(f, outer[1], 0, app.lang);
            draw_theme_popup(f, outer[0], sel, scroll);
        }
        MenuState::Settings(sel) => {
            draw_menu_footer(f, outer[1], 1, app.lang);
            draw_settings_popup(f, outer[0], app, sel);
        }
        MenuState::Reset(sel) => {
            draw_menu_footer(f, outer[1], 2, app.lang);
            draw_reset_popup(f, outer[0], sel, app.lang);
        }
        MenuState::Graphs(sel) => {
            draw_menu_footer(f, outer[1], 3, app.lang);
            draw_graphs_popup(f, outer[0], app, sel);
        }
        MenuState::Help => {
            draw_menu_footer(f, outer[1], 4, app.lang);
            draw_help_popup(f, outer[0], app);
        }
        MenuState::Detail(b, id) => {
            draw_menu_footer(f, outer[1], 0, app.lang);
            draw_detail_popup(f, outer[0], app, b, id);
        }
        MenuState::ConfirmKill(pid) => {
            draw_menu_footer(f, outer[1], 0, app.lang);
            draw_confirm_popup(f, outer[0], app, pid, false);
        }
        MenuState::ConfirmRestart(pid) => {
            draw_menu_footer(f, outer[1], 0, app.lang);
            draw_confirm_popup(f, outer[0], app, pid, true);
        }
    }
}

fn draw_stack(f: &mut Frame, area: Rect, app: &App, ids: &[usize], fill: bool) {
    if ids.is_empty() {
        return;
    }
    let cons: Vec<Constraint> = if fill {
        vec![Constraint::Fill(1); ids.len()]
    } else {
        // graph boxes keep a natural ratio when stacked in a half
        ids.iter()
            .map(|&i| {
                if i == BOX_NET {
                    Constraint::Percentage(58)
                } else {
                    Constraint::Percentage(42)
                }
            })
            .collect()
    };
    let parts = Layout::vertical(cons).split(area);
    for (&i, r) in ids.iter().zip(parts.iter()) {
        draw_box(f, *r, app, i);
    }
}

fn draw_row(f: &mut Frame, area: Rect, app: &App, ids: &[usize]) {
    if ids.is_empty() {
        return;
    }
    let cons = vec![Constraint::Fill(1); ids.len()];
    let parts = Layout::horizontal(cons).split(area);
    for (&i, r) in ids.iter().zip(parts.iter()) {
        draw_box(f, *r, app, i);
    }
}

fn draw_box(f: &mut Frame, area: Rect, app: &App, idx: usize) {
    match idx {
        BOX_NET => draw_net(f, area, app, app.focus[BOX_NET]),
        BOX_LAN => draw_lan(f, area, app, app.focus[BOX_LAN]),
        BOX_DISKIO => draw_diskio(f, area, app, app.focus[BOX_DISKIO]),
        BOX_DISKS => draw_disks(f, area, app, app.focus[BOX_DISKS]),
        BOX_CONNS => draw_conns(f, area, app, app.focus[BOX_CONNS]),
        BOX_PROCS => draw_procs(f, area, app, app.focus[BOX_PROCS]),
        _ => {}
    }
}

/// btop-style box frame: `┤ N word ├ rest` title with the box number just
/// left of the word and the function letter highlighted inside it.
fn boxed(
    f: &mut Frame,
    area: Rect,
    title: Line<'static>,
    border: Color,
    badge: usize,
    focused: bool,
) -> Rect {
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .style(Style::new().bg(bg().unwrap_or(Color::Reset)))
        .border_style(Style::new().fg(if focused { hi() } else { border }))
        .title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);
    BOX_AREAS.lock().unwrap().push((badge - 1, area));
    inner
}

/// Minimum rows a graph box can usefully show: one section header + a 3-row
/// braille plot.
const GRAPH_MIN_ROWS: u16 = 4;
/// A list box never asks for more than this before the others get their share.
const LIST_MAX_ROWS: u16 = 12;
/// Below this width a table row of four boxes squeezes each box past useful
/// column widths, so tables stack instead of forming a grid.
const TABLE_GRID_MIN_W: u16 = 100;

/// Applied-heights memory with hysteresis. Live row counts change every tick
/// (a connection opens, a process shows activity), and recomputing the split
/// from them moves every box by a row or two. A new allocation is only
/// adopted when it differs by `MIN_DELTA` rows and has persisted for
/// `SETTLE`, so genuine changes land while transient blips are ignored.
/// A terminal resize always re-applies immediately.
struct StableAlloc {
    applied: Vec<u16>,
    total: u16,
    last: Option<std::time::Instant>,
}

impl StableAlloc {
    const fn new() -> Self {
        Self {
            applied: Vec::new(),
            total: 0,
            last: None,
        }
    }

    fn update(&mut self, desired: Vec<u16>, total: u16, now: std::time::Instant) -> Vec<u16> {
        const MIN_DELTA: i32 = 4;
        const SETTLE: std::time::Duration = std::time::Duration::from_secs(3);
        let reset = self.applied.len() != desired.len() || self.total != total;
        if reset || self.last.is_none() {
            self.applied = desired;
            self.total = total;
            self.last = Some(now);
            return self.applied.clone();
        }
        // Adopt the whole allocation or none of it: taking only the boxes
        // that grew would break the invariant that the heights sum to the
        // available space, and the overflow would clip the last box.
        if now.duration_since(self.last.unwrap_or(now)) >= SETTLE {
            let moved = desired
                .iter()
                .zip(&self.applied)
                .any(|(d, a)| (*d as i32 - *a as i32).abs() >= MIN_DELTA);
            if moved && desired.iter().sum::<u16>() == total {
                self.applied = desired;
                self.last = Some(now);
            }
        }
        self.applied.clone()
    }
}

static LIST_ALLOC: Mutex<StableAlloc> = Mutex::new(StableAlloc::new());
static VERT_ALLOC: Mutex<StableAlloc> = Mutex::new(StableAlloc::new());
// classic 右列按中线分组后,上(LAN+CONNS)/下(PROCS+DISKS)各自平滑
static LIST_ALLOC_TOP: Mutex<StableAlloc> = Mutex::new(StableAlloc::new());
/// classic 里 LAN 盒的高度上限:它可滚动,不必随设备数膨胀挤占 CONNS
const LAN_BOX_MAX_ROWS: u16 = 6;
static LIST_ALLOC_BOTTOM: Mutex<StableAlloc> = Mutex::new(StableAlloc::new());

/// Split `total` rows among `ideals`: everyone gets their ideal while there is
/// room, but the spare height goes only to the boxes listed in `spare_to`
/// (process tables) so short lan/disks lists don't waste rows. When space is
/// short everything shrinks proportionally down to 3 rows each (border + one
/// content line).
fn alloc_heights(ideals: &[u16], total: u16, spare_to: &[usize]) -> Vec<u16> {
    let n = ideals.len();
    if n == 0 {
        return Vec::new();
    }
    let sum: u16 = ideals.iter().sum();
    let mut out = ideals.to_vec();
    if total >= sum {
        let spare = total - sum;
        let targets: Vec<usize> = if spare_to.is_empty() {
            (0..n).collect()
        } else {
            spare_to.iter().copied().filter(|&i| i < n).collect()
        };
        if !targets.is_empty() {
            let base = spare / targets.len() as u16;
            let rem = spare % targets.len() as u16;
            for (k, &i) in targets.iter().enumerate() {
                out[i] += base + u16::from((k as u16) < rem);
            }
        }
        let used: u16 = out.iter().sum();
        if let Some(last) = out.last_mut() {
            *last += total.saturating_sub(used);
        }
        return out;
    }
    let min_each = 3u16;
    if total <= min_each * n as u16 {
        // pathological terminal: equal shares, borders win
        return vec![(total / n as u16).max(1); n];
    }
    let usable = total - min_each * n as u16;
    let flex: u32 = ideals
        .iter()
        .map(|h| h.saturating_sub(min_each) as u32)
        .sum();
    let mut out = vec![min_each; n];
    if flex > 0 {
        let mut assigned = 0u16;
        for (i, &h) in ideals.iter().enumerate() {
            let share = (h.saturating_sub(min_each) as u32)
                .saturating_mul(usable as u32)
                .saturating_div(flex) as u16;
            out[i] += share;
            assigned += share;
        }
        // rounding remainder goes to the box that wanted the most
        let rem = usable - assigned;
        let biggest = ideals
            .iter()
            .enumerate()
            .max_by_key(|(_, &h)| h)
            .map(|(i, _)| i)
            .unwrap_or(0);
        out[biggest] += rem;
    }
    out
}

/// Classic layout width split (graphs_w, tables_w). Lists are the priority:
/// they keep at least half the width and take up to `need` columns, while the
/// graphs shrink down to `graph_min` before losing anything else.
fn split_horizontal(w: u16, need: u16, graph_min: u16) -> (u16, u16) {
    let tables = (w / 2).max(need.min(w.saturating_sub(graph_min)));
    (w.saturating_sub(tables), tables)
}

/// Vertical split for wide/tall (graphs_h, tables_h). Lists get their ideal
/// height first; the graphs absorb the surplus but are capped at half the
/// height — on full-screen terminals the surplus flows back to the lists
/// instead of letting the two charts balloon to 4/5 of the screen. Only when
/// the lists themselves shrink below ideal do graphs give way down to
/// `graph_min`, after which the lists start scrolling.
fn split_vertical(h: u16, graph_min: u16, tables_ideal: u16) -> (u16, u16) {
    let graph_cap = h / 2;
    let tables = if h.saturating_sub(tables_ideal) > graph_cap {
        h - graph_cap
    } else {
        tables_ideal.clamp(3, h.saturating_sub(graph_min).max(3))
    };
    (h.saturating_sub(tables), tables)
}

/// Rows a list box would like (content + border + table header), capped and
/// rounded up to an even number: a connection opening/closing then changes the
/// requested height only every other row, so the graph/list split does not
/// breathe on every tick.
fn list_ideal_rows(rows: usize) -> u16 {
    let h = ((rows as u16).saturating_add(3)).clamp(4, LIST_MAX_ROWS);
    (h + 1) & !1
}

fn box_ideal_rows(app: &App, idx: usize) -> u16 {
    match idx {
        BOX_LAN => list_ideal_rows(app.lan.neighbors.len()),
        BOX_CONNS => list_ideal_rows(app.conn.conns.len()),
        BOX_PROCS => list_ideal_rows(app.proc.procs.len()),
        BOX_DISKS => list_ideal_rows(app.disk.tree.len() * 2),
        _ => GRAPH_MIN_ROWS,
    }
}

/// Rows the table area needs: side by side (grid) the two boxes of a row share
/// a height, stacked they add up.
fn tables_ideal_h(app: &App, ids: &[usize], width: u16) -> u16 {
    if ids.is_empty() {
        return 0;
    }
    if width < TABLE_GRID_MIN_W {
        return ids.iter().map(|&i| box_ideal_rows(app, i)).sum();
    }
    ids.chunks(2)
        .map(|pair| {
            pair.iter()
                .map(|&i| box_ideal_rows(app, i))
                .max()
                .unwrap_or(0)
        })
        .sum()
}

/// Column widths from a fixed width vocabulary — never from live content.
/// A rate flipping between "999B/s" and "1.0KiB/s" must not resize the table
/// every tick, so the bases are constants per table; spare width is spread
/// evenly with a per-column growth cap. The result depends only on the box
/// width, which makes the layout completely stable between frames.
fn fit_columns(
    base: &[usize],
    flex: usize,
    flex_min: usize,
    flex_max: usize,
    total: u16,
    spacing: usize,
) -> Vec<Constraint> {
    let n = base.len();
    if n == 0 {
        return Vec::new();
    }
    let avail = (total as usize).saturating_sub(spacing * n.saturating_sub(1));
    let fixed: usize = base
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != flex)
        .map(|(_, w)| *w)
        .sum();
    let room = avail.saturating_sub(fixed);
    let mut want = base.to_vec();
    // keep the flexible column visible even when the fixed ones already
    // overflow; the right edge clips, which is the least bad option
    want[flex] = if room >= flex_min {
        room.min(flex_max.max(flex_min))
    } else {
        flex_min
    };

    let base_sum: usize = want.iter().sum();
    if base_sum > avail {
        // even the fixed vocabulary does not fit: keep every column and let
        // the right edge clip, rather than hiding the name column entirely
        return want.iter().map(|&w| Constraint::Length(w as u16)).collect();
    }
    let max_grow: Vec<usize> = want.iter().map(|&w| (w / 2).max(2)).collect();
    let mut grow = vec![0usize; n];
    let mut slack = avail - base_sum;
    let mut progressed = true;
    while slack > 0 && progressed {
        progressed = false;
        for i in 0..n {
            if slack == 0 {
                break;
            }
            if grow[i] < max_grow[i] {
                grow[i] += 1;
                slack -= 1;
                progressed = true;
            }
        }
    }
    want.iter()
        .zip(&grow)
        .map(|(&w, &g)| Constraint::Length((w + g) as u16))
        .collect()
}

/// Table area: a grid of two columns where width allows, otherwise a stack.
/// Heights follow the boxes' ideal row counts so a short list is not padded
/// with blank space while a long one scrolls.
fn draw_table_grid(f: &mut Frame, area: Rect, app: &App, ids: &[usize]) {
    if ids.is_empty() {
        return;
    }
    let grid = area.width >= TABLE_GRID_MIN_W && ids.len() > 1;
    if !grid {
        // 非经典布局的列表栈无需中线对齐
        draw_list_stack(f, area, app, ids, false);
        return;
    }
    let row_ideals: Vec<u16> = ids
        .chunks(2)
        .map(|pair| {
            pair.iter()
                .map(|&i| box_ideal_rows(app, i))
                .max()
                .unwrap_or(4)
        })
        .collect();
    // in each grid row the process table (conns/procs) gets the spare height
    // — spare targets are the GLOBAL indices of those boxes (alloc_heights
    // indexes into the ideals vec); none visible ⇒ spread evenly
    let spare_rows: Vec<usize> = ids
        .iter()
        .enumerate()
        .filter(|(_, &i)| matches!(i, BOX_CONNS | BOX_PROCS))
        .map(|(i, _)| i)
        .collect();
    let desired = alloc_heights(&row_ideals, area.height, &spare_rows);
    let hs = LIST_ALLOC
        .lock()
        .unwrap()
        .update(desired, area.height, std::time::Instant::now());
    let cons: Vec<Constraint> = hs.iter().map(|&h| Constraint::Length(h)).collect();
    let rows = Layout::vertical(cons).split(area);
    for (pair, r) in ids.chunks(2).zip(rows.iter()) {
        if pair.len() == 1 {
            draw_box(f, *r, app, pair[0]);
        } else {
            let cols = Layout::horizontal([Constraint::Fill(1), Constraint::Fill(1)]).split(*r);
            draw_box(f, cols[0], app, pair[0]);
            draw_box(f, cols[1], app, pair[1]);
        }
    }
}

/// Stack list boxes by their ideal heights; spare space is spread evenly and
/// when short they shrink proportionally (each keeps at least 3 rows).
fn draw_list_stack(f: &mut Frame, area: Rect, app: &App, ids: &[usize], snap_mid: bool) {
    // classic:LAN+CONNS 组钉在列的上半,底边与左列 NET|DISK 分界(中线)对齐;
    // PROCS+DISKS 在下半。组的开拆点是第一个非 LAN/CONNS 的盒子。
    if snap_mid {
        let bi = ids
            .iter()
            .position(|&i| !matches!(i, BOX_LAN | BOX_CONNS))
            .unwrap_or(ids.len());
        if bi > 0 && bi < ids.len() {
            // 与左列 NET|DISK 相同的 Fill(1)+Fill(1) 分法,奇数高度时余行
            // 给上半,两侧分界线逐行重合
            let parts = Layout::vertical([Constraint::Fill(1), Constraint::Fill(1)]).split(area);
            // 上半组里 LAN 单独算:钉一个矮个头上限(滚动可见全量),
            // 剩余全部让给信息量最大的 CONNS
            // LAN 钉矮个头的前提是 CONNS 可见(矮个头是为给 CONNS 让位);
            // CONNS hide 后上组只剩 LAN,应占满上半而非留空
            if ids[0] == BOX_LAN && ids.contains(&BOX_CONNS) && ids.len() > 2 {
                // 内层 clamp 的上界恒 ≥ 下界(1≤6),迷你终端不会 panic
                let lan_h = box_ideal_rows(app, BOX_LAN).min(LAN_BOX_MAX_ROWS).clamp(
                    1,
                    (parts[0].height.saturating_sub(3)).clamp(1, LAN_BOX_MAX_ROWS),
                );
                let sub = Layout::vertical([Constraint::Length(lan_h), Constraint::Fill(1)])
                    .split(parts[0]);
                draw_box(f, sub[0], app, ids[0]);
                draw_table_group(f, sub[1], app, &ids[1..bi], &LIST_ALLOC_TOP);
            } else {
                draw_table_group(f, parts[0], app, &ids[..bi], &LIST_ALLOC_TOP);
            }
            draw_table_group(f, parts[1], app, &ids[bi..], &LIST_ALLOC_BOTTOM);
            return;
        }
    }
    draw_table_group(f, area, app, ids, &LIST_ALLOC);
}

fn draw_table_group(
    f: &mut Frame,
    area: Rect,
    app: &App,
    ids: &[usize],
    alloc: &Mutex<StableAlloc>,
) {
    if ids.is_empty() || area.height == 0 {
        return;
    }
    let ideals: Vec<u16> = ids.iter().map(|&i| box_ideal_rows(app, i)).collect();
    let spare_to: Vec<usize> = ids
        .iter()
        .enumerate()
        .filter(|&(_, id)| matches!(*id, BOX_CONNS | BOX_PROCS))
        .map(|(i, _)| i)
        .collect();
    let desired = alloc_heights(&ideals, area.height, &spare_to);
    let hs = alloc
        .lock()
        .unwrap()
        .update(desired, area.height, std::time::Instant::now());
    let cons: Vec<Constraint> = hs.iter().map(|&h| Constraint::Length(h)).collect();
    let parts = Layout::vertical(cons).split(area);
    for (&id, r) in ids.iter().zip(parts.iter()) {
        draw_box(f, *r, app, id);
    }
}

/// btop-style title: the box word framed as `─┐¹word┌`, then every info
/// segment framed in its own `┐…┌` pair (btop menu-button style), all in
/// border color. Superscript digit + function letters highlighted.
fn title_line(
    badge: usize,
    word: &str,
    key: char,
    pieces: Vec<Vec<Span<'static>>>,
    border: Color,
    focused: bool,
) -> Line<'static> {
    let frame = if focused { hi() } else { border };
    let b = Style::new().fg(frame);
    let mut spans: Vec<Span> = vec![Span::styled("─", b), Span::styled("┐", b)];
    if badge > 0 {
        spans.push(Span::styled(superscript(badge), bold(hi())));
    }
    match word.find(key) {
        Some(pos) => {
            let (head, tail) = word.split_at(pos);
            let (k, tail) = tail.split_at(1);
            spans.push(Span::styled(head.to_string(), bold(palette().title)));
            spans.push(Span::styled(k.to_string(), bold(hi())));
            spans.push(Span::styled(tail.to_string(), bold(palette().title)));
        }
        None => spans.push(Span::styled(word.to_string(), bold(palette().title))),
    }
    spans.push(Span::styled("┌", b));
    for p in pieces {
        // frames chained together by the border line, btop menu style
        spans.push(Span::styled("─", b));
        spans.push(Span::styled("┐", b));
        spans.extend(p);
        spans.push(Span::styled("┌", b));
    }
    Line::from(spans)
}

fn text_s(s: String) -> Span<'static> {
    Span::styled(s, text())
}

fn key_span(k: &str) -> Span<'static> {
    Span::styled(k.to_string(), bold(hi()))
}

/// A function word with its key letter highlighted: `[f]ilter: all`
fn fword(key: char, word: &str, value: &str) -> Vec<Span<'static>> {
    let mut v = Vec::new();
    match word.find(key) {
        Some(pos) => {
            let (head, tail) = word.split_at(pos);
            let (k, tail) = tail.split_at(1);
            v.push(Span::styled(
                head.to_string(),
                Style::new().fg(palette().title),
            ));
            v.push(Span::styled(k.to_string(), bold(hi())));
            v.push(Span::styled(
                tail.to_string(),
                Style::new().fg(palette().title),
            ));
        }
        None => v.push(Span::styled(
            word.to_string(),
            Style::new().fg(palette().title),
        )),
    }
    if !value.is_empty() {
        v.push(Span::styled(
            format!(": {value}"),
            Style::new().fg(palette().title),
        ));
    }
    v
}

/// Append a "+N more rows" hint to a title rest string when a list scrolls
fn scroll_hint(list_len: usize, off: usize, area: Rect) -> String {
    let visible = area.height.saturating_sub(3); // borders + table header
    let hidden = list_len.saturating_sub(off + visible as usize);
    if hidden > 0 {
        format!("+{hidden}")
    } else {
        String::new()
    }
}

/// btop-style overflow scrollbar, drawn in the rightmost content column of a
/// list box: `↑` above, `↓` below, a one-row `█` thumb on a blank track.
/// Position is the scroll fraction over `len - visible`; focus-independent.
/// Returns the column rect (data rows only, header row excluded) or None
/// when the list fits.
fn scrollbar_col(inner: Rect, len: usize, visible: usize) -> Option<Rect> {
    if inner.width < 4 || inner.height < 4 || len <= visible.max(1) {
        return None;
    }
    Some(Rect {
        x: inner.right().saturating_sub(1),
        y: inner.y + 1, // below the table header row
        width: 1,
        height: inner.height - 1,
    })
}

/// thumb row inside the scrollbar column for a clamped offset
fn scrollbar_thumb_row(col: Rect, len: usize, off: usize, visible: usize) -> u16 {
    let track = col.height.saturating_sub(2); // rows between the arrows
    if track == 0 {
        return col.y + 1;
    }
    let span = len.saturating_sub(visible).max(1);
    let frac = (off.min(span)) as f64 / span as f64;
    col.y + 1 + (frac * (track - 1) as f64).round() as u16
}

fn draw_scrollbar(f: &mut Frame, col: Rect, len: usize, off: usize, visible: usize) {
    let thumb = scrollbar_thumb_row(col, len, off, visible);
    let st = text().add_modifier(Modifier::BOLD);
    let lines: Vec<Line> = (0..col.height)
        .map(|i| {
            let ch = match i {
                0 => "\u{2191}",                        // ↑
                _ if i == col.height - 1 => "\u{2193}", // ↓
                _ if col.y + i == thumb => "\u{2588}",  // █
                _ => " ",
            };
            Line::from(Span::styled(ch, st))
        })
        .collect();
    f.render_widget(Paragraph::new(lines), col);
}

/// scroll geometry of the DISKS box shared by draw and input: (len, visible
/// data rows, clamped offset). Single source so mouse hit-testing matches
/// what was rendered.
pub fn disks_scroll_state(app: &App) -> (usize, usize, usize) {
    let len = list_len(app, BOX_DISKS);
    let visible = box_rect(BOX_DISKS)
        .map(|r| (r.height.saturating_sub(3)).max(1) as usize)
        .unwrap_or(1);
    (
        len,
        visible,
        app.scroll[BOX_DISKS].min(len.saturating_sub(visible)),
    )
}

/// what a click on the DISKS scrollbar column landed on; `grab` is the
/// click's offset from the thumb top for drag tracking
pub enum SbHit {
    PageUp,
    PageDown,
    Thumb { grab: u16 },
}

pub fn scrollbar_hit(col: u16, row: u16, len: usize, visible: usize, off: usize) -> Option<SbHit> {
    let inner = box_rect(BOX_DISKS).map(|r| Rect {
        x: r.x + 1,
        y: r.y + 1,
        width: r.width.saturating_sub(2),
        height: r.height.saturating_sub(2),
    })?;
    scrollbar_hit_at(inner, col, row, len, visible, off)
}

/// pure geometry kernel of the hit test (no global state; unit-testable)
fn scrollbar_hit_at(
    inner: Rect,
    col: u16,
    row: u16,
    len: usize,
    visible: usize,
    off: usize,
) -> Option<SbHit> {
    let bar = scrollbar_col(inner, len, visible)?;
    if col != bar.x || row < bar.y || row >= bar.y + bar.height {
        return None;
    }
    if row == bar.y {
        return Some(SbHit::PageUp);
    }
    if row == bar.y + bar.height - 1 {
        return Some(SbHit::PageDown);
    }
    let thumb = scrollbar_thumb_row(bar, len, off, visible);
    if row == thumb {
        Some(SbHit::Thumb {
            grab: row - bar.y - 1,
        })
    } else if row < thumb {
        Some(SbHit::PageUp)
    } else {
        Some(SbHit::PageDown)
    }
}

/// drag mapping: a row on the scrollbar track (with the recorded grab offset)
/// becomes a list offset. Inverse of the thumb-position math above.
pub fn scrollbar_drag(app: &mut App, row: u16, grab: u16) {
    let (len, visible, _) = disks_scroll_state(app);
    if let Some(inner) = box_rect(BOX_DISKS).map(|r| Rect {
        x: r.x + 1,
        y: r.y + 1,
        width: r.width.saturating_sub(2),
        height: r.height.saturating_sub(2),
    }) {
        scrollbar_drag_at(app, inner, len, visible, row, grab);
    }
}

/// pure geometry kernel of the drag mapping (no global state; unit-testable)
fn scrollbar_drag_at(app: &mut App, inner: Rect, len: usize, visible: usize, row: u16, grab: u16) {
    let Some(bar) = scrollbar_col(inner, len, visible) else {
        return;
    };
    let track = bar.height.saturating_sub(2);
    if track == 0 {
        return;
    }
    // clicked track position minus the grab offset, clamped to the track
    let pos = (i32::from(row) - i32::from(bar.y) - 1 - i32::from(grab)).clamp(0, i32::from(track));
    let span = len.saturating_sub(visible);
    let off = if track == 1 {
        if pos == 0 {
            0
        } else {
            span
        }
    } else {
        (pos as f64 / (track - 1) as f64 * span as f64).round() as usize
    };
    app.scroll[BOX_DISKS] = off.min(span);
}

/// Unicode superscript digits — the btop-style corner badge look
fn superscript(n: usize) -> String {
    const SUP: [char; 10] = ['⁰', '¹', '²', '³', '⁴', '⁵', '⁶', '⁷', '⁸', '⁹'];
    n.to_string()
        .chars()
        .map(|c| SUP[c.to_digit(10).unwrap_or(0) as usize])
        .collect()
}

// Readable-row kernels: palette-derived only (theme text / theme bg
// anchors), no hardcoded black/white, so they hold across all 72 builtin
// themes and user theme files.

/// nudge a highlight `c` toward the theme bg until the theme text reads on
/// it at WCAG 3:1; keeps the hue, only the luminance moves. Themes without
/// a measurable bg (transparent) pass through
fn readable_bg_over(c: Color, base: Option<Color>, text: Color) -> Color {
    let Some(base) = base else {
        return c;
    };
    if crate::theme::contrast_ratio(c, text) >= 3.0 {
        return c;
    }
    let mut best = c;
    let mut t = 0.1;
    while t <= 0.9 + f32::EPSILON {
        let cand = crate::theme::blend(c, base, t);
        best = cand;
        if crate::theme::contrast_ratio(cand, text) >= 3.0 {
            return cand;
        }
        t += 0.1;
    }
    best
}

/// better-contrasting of the theme's two anchors (text / bg) as fg on `bg`
fn readable_fg_between(bg: Color, text: Color, anchor: Option<Color>) -> Color {
    match anchor {
        Some(a) if crate::theme::contrast_ratio(a, bg) > crate::theme::contrast_ratio(text, bg) => {
            a
        }
        _ => text,
    }
}

/// selection wash over the row background: the download-gradient hue is
/// swept from subtle to strong until it is both visible against the row
/// background and readable under the overlay's own span colors. When the
/// hue itself cannot separate (dark gradient start on a dark theme — the
/// invisible-selection bug), the hue is lifted toward the text color for
/// luminance while keeping its hue, and the sweep retries
fn selection_bg_over(hue: Color, base: Option<Color>, text: Color) -> Color {
    let Some(base) = base else {
        return hue;
    };
    let ok = |c: Color| {
        crate::theme::contrast_ratio(c, base) >= 1.3 && crate::theme::contrast_ratio(c, text) >= 2.2
    };
    for t in [0.15, 0.22, 0.3, 0.4, 0.55] {
        let c = crate::theme::blend(base, hue, t);
        if ok(c) {
            return c;
        }
    }
    let lifted = crate::theme::blend(hue, text, 0.45);
    for t in [0.15, 0.22, 0.3, 0.4, 0.55] {
        let c = crate::theme::blend(base, lifted, t);
        if ok(c) {
            return c;
        }
    }
    crate::theme::blend(base, lifted, 0.55)
}

fn readable_bg(c: Color) -> Color {
    let pal = palette();
    readable_bg_over(c, pal.bg, pal.text)
}

fn readable_fg(bg: Color) -> Color {
    let pal = palette();
    readable_fg_between(bg, pal.text, pal.bg)
}

/// alternating track colours: two distinct theme accents
fn track_bg(slot: usize) -> Color {
    if slot.is_multiple_of(2) {
        download().at(1.0)
    } else {
        upload().at(1.0)
    }
}

fn track_fg(slot: usize) -> Color {
    readable_fg(track_bg(slot))
}

/// Rows of a box's current list, for selection clamping
/// 滚动偏移的统一封顶:最后一页(与 draw_procs/draw_conns/鼠标点击同源),
/// 旧版各自手写 min(len-1) 导致滚到底后点击错位、只剩单行
pub fn clamped_offset(app: &App, idx: usize, visible: usize) -> usize {
    app.scroll[idx].min(list_len(app, idx).saturating_sub(visible.max(1)))
}

/// 跳到列表顶/底(Home/End/g/G):有选中模型的盒移动选中,DISKS 盒只滚
pub fn jump_selection(app: &mut App, idx: usize, top: bool) {
    if idx == crate::app::BOX_DISKS {
        app.scroll[idx] = if top { 0 } else { usize::MAX };
        return;
    }
    let len = list_len(app, idx);
    if len == 0 {
        return;
    }
    let target = if top { 0 } else { len - 1 };
    app.sel[idx] = Some(target);
    // 窗口跟随,与 move_selection 同语义
    let cap = box_capacity(idx);
    let off = &mut app.scroll[idx];
    if target < *off {
        *off = target;
    } else if target + 1 >= *off + cap {
        *off = target + 1 - cap;
    }
}

pub fn list_len(app: &App, idx: usize) -> usize {
    match idx {
        BOX_LAN => app.lan.neighbors.len(),
        BOX_CONNS => conn_items(app).len(),
        BOX_PROCS => procs_display(app).len(),
        BOX_DISKS => {
            let mut flat: Vec<(&TreeNode, String)> = Vec::new();
            emit_tree_rows(&app.disk.tree, "", 0, &mut flat);
            flat.len()
        }
        _ => 0,
    }
}

/// Process list in display order: tracked rows pinned at the top in the
/// order they were activated, the rest following the active sort.
/// `/` 搜索:名字子串过滤(大小写不敏感);空串不过滤
fn name_matches(name: &str, q: &str) -> bool {
    q.is_empty() || name.to_lowercase().contains(&q.to_lowercase())
}

/// proc 分组的聚合信息(可见行、聚合行与详情共用)
#[derive(Clone, Copy, Default)]
pub struct GroupInfo {
    pub n: usize,
    pub rx: u64,
    pub tx: u64,
    pub rr: f64,
    pub rs: f64,
}

/// CONNS proc 排序下的可见行:聚合父行或单条连接
pub enum ConnItem<'a> {
    Agg {
        key: String,
        name: String,
        rep: &'a Conn,
        info: GroupInfo,
    },
    /// `child`:展开组的子行(渲染时树形缩进)
    Row { c: &'a Conn, child: bool },
}

impl ConnItem<'_> {
    /// 稳定 id:连接行 = inode;聚合行 = 归属键哈希,最高位置 1
    /// (真实 socket inode 远小于 2^63,不会相撞)
    pub fn id(&self) -> u64 {
        match self {
            Self::Row { c, .. } => c.inode,
            Self::Agg { key, .. } => {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                key.hash(&mut h);
                0x8000_0000_0000_0000 | (h.finish() & 0x7fff_ffff_ffff_ffff)
            }
        }
    }

    pub fn is_agg(&self) -> bool {
        matches!(self, Self::Agg { .. })
    }

    pub fn conn(&self) -> Option<&Conn> {
        match self {
            Self::Row { c, .. } => Some(c),
            _ => None,
        }
    }
}

/// CONNS 的可见行序列(默认折叠 proc 分组;搜索时全部展开)
pub fn conn_items(app: &App) -> Vec<ConnItem<'_>> {
    let list = conns_display(app);
    if app.conn.sort != ConnSort::Proc {
        return list
            .into_iter()
            .map(|c| ConnItem::Row { c, child: false })
            .collect();
    }
    // 搜索时全部展开:命中不可见的折叠组等于没搜到。仅当搜索目标是
    // CONNS 本身——目标是 procs 时 conns 未被过滤,摊平只会白白降信息密度
    let expand_all = !app.search.is_empty() && app.search_box == BOX_CONNS;
    let group_open = |key: &str| -> bool { app.proc_expand_default != app.expanded.contains(key) };
    let mut out: Vec<ConnItem> = Vec::new();
    let mut i = 0;
    while i < list.len() {
        let key = crate::collect::conn::owner_key(list[i]);
        let n = list[i..]
            .iter()
            .take_while(|c| crate::collect::conn::owner_key(c) == key)
            .count();
        if n == 1 {
            out.push(ConnItem::Row {
                c: list[i],
                child: false,
            });
            i += 1;
            continue;
        }
        let group = &list[i..i + n];
        let info = GroupInfo {
            n,
            rx: group.iter().map(|c| c.bytes_recv).sum(),
            tx: group.iter().map(|c| c.bytes_sent).sum(),
            rr: group.iter().map(|c| c.rate_recv).sum(),
            rs: group.iter().map(|c| c.rate_sent).sum(),
        };
        let expanded = expand_all || group_open(&key);
        out.push(ConnItem::Agg {
            key: key.clone(),
            name: key.clone(),
            rep: group[0],
            info,
        });
        if expanded {
            out.extend(group.iter().map(|c| ConnItem::Row { c, child: true }));
        }
        i += n;
    }
    out
}

/// 选中聚合行时的归属键(展开/折叠用)
pub fn selected_agg_key(app: &App, idx: usize) -> Option<String> {
    match conn_items(app).get(app.sel[idx]?) {
        Some(ConnItem::Agg { key, .. }) => Some(key.clone()),
        _ => None,
    }
}

/// 选中行是否为聚合父行
pub fn selected_is_agg(app: &App, idx: usize) -> bool {
    idx == BOX_CONNS
        && app.sel[idx].is_some()
        && conn_items(app)
            .get(app.sel[idx].unwrap())
            .is_some_and(|i| i.is_agg())
}

/// 组当前是否展开
pub fn group_is_open(app: &App, key: &str) -> bool {
    app.proc_expand_default != app.expanded.contains(key)
}

/// 设定组开/合(相对默认态反转)
pub fn set_group_open(app: &mut App, key: &str, open: bool) {
    if open != app.proc_expand_default {
        app.expanded.insert(key.to_string());
    } else {
        app.expanded.remove(key);
    }
}

/// 切换组开/合
pub fn toggle_group(app: &mut App, key: &str) {
    let open = group_is_open(app, key);
    set_group_open(app, key, !open);
}

pub fn procs_display(app: &App) -> Vec<&crate::collect::proc::ProcRow> {
    let mut out = Vec::new();
    for pid in &app.tracked.procs {
        if let Some(p) = app.proc.procs.iter().find(|p| p.pid == *pid) {
            out.push(p);
        }
    }
    for p in app.proc.procs.iter() {
        if !app.tracked.procs.contains(&p.pid)
            && (app.search_box != BOX_PROCS || name_matches(&p.name, &app.search))
        {
            out.push(p);
        }
    }
    out
}
/// Connection list in display order: tracked pinned first, then the rest.
pub fn conns_display(app: &App) -> Vec<&Conn> {
    // Proc 排序下不按追踪单行置顶——单行浮到顶部会在 pin 顺序下把同名
    // 进程的分组撕成多个碎组(死亡的追踪行还以墓碑身份带着旧进程名混在
    // 最前);改为整组提升,见 proc_grouped_display
    if app.conn.sort == ConnSort::Proc {
        let mut out = proc_grouped_display(app);
        out = match app.conn.filter {
            ConnFilter::All => out,
            ConnFilter::Lan => out
                .into_iter()
                .filter(|c| matches!(c.remote.0, std::net::IpAddr::V4(v4) if v4.is_private()))
                .collect(),
            ConnFilter::Tcp => out.into_iter().filter(|c| c.is_tcp()).collect(),
            ConnFilter::Udp => out.into_iter().filter(|c| !c.is_tcp()).collect(),
        };
        if let Some(ip) = app.conn.peer_filter {
            out.retain(|c| c.remote.0 == std::net::IpAddr::V4(ip));
        }
        if app.search_box == BOX_CONNS && !app.search.is_empty() {
            let q = app.search.to_lowercase();
            out.retain(|c| c.proc.as_deref().unwrap_or("").to_lowercase().contains(&q));
        }
        return out;
    }
    let mut out = Vec::new();
    for ino in &app.tracked.conns {
        if let Some(c) = app.conn.conns.iter().find(|c| c.inode == *ino) {
            out.push(c);
        }
    }
    for c in app.conn.conns.iter() {
        if !app.tracked.conns.contains(&c.inode) {
            out.push(c);
        }
    }
    let mut out = match app.conn.filter {
        ConnFilter::All => out,
        ConnFilter::Lan => out
            .into_iter()
            .filter(|c| matches!(c.remote.0, std::net::IpAddr::V4(v4) if v4.is_private()))
            .collect(),
        ConnFilter::Tcp => out.into_iter().filter(|c| c.is_tcp()).collect(),
        ConnFilter::Udp => out.into_iter().filter(|c| !c.is_tcp()).collect(),
    };
    // LAN 联动的对端过滤(enter from lan row)
    if let Some(ip) = app.conn.peer_filter {
        out.retain(|c| c.remote.0 == std::net::IpAddr::V4(ip));
    }
    // `/` 搜索:仅作用于目标盒(非目标盒直通显示)
    if app.search_box == BOX_CONNS && !app.search.is_empty() {
        let q = app.search.to_lowercase();
        out.retain(|c| c.proc.as_deref().unwrap_or("").to_lowercase().contains(&q));
    }
    out
}

/// Proc 排序的显示序:按归属键整组为单位,含追踪行的组整体提升到最前,
/// 其余组保持排序给出的流量降序;组内相邻永不打散(活行在原位,同键墓碑
/// 从尾部并回本组、排在活行之后)。conn_items 的连续同键分组因此恒定
/// 每键一组
fn proc_grouped_display(app: &App) -> Vec<&Conn> {
    let all = &app.conn.conns;
    // 保序归组:排序后的活行同键相邻;墓碑追加在尾部,同键可能再次出现
    // ——并入已存在的组(排在该组活行之后)
    let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
    for (i, c) in all.iter().enumerate() {
        let k = crate::collect::conn::owner_key(c);
        match groups.iter_mut().find(|(kk, _)| *kk == k) {
            Some((_, v)) => v.push(i),
            None => groups.push((k, vec![i])),
        }
    }
    let tracked: std::collections::HashSet<u64> = app.tracked.conns.iter().copied().collect();
    let has_tracked = |v: &Vec<usize>| v.iter().any(|&i| tracked.contains(&all[i].inode));
    // 稳定排序:含追踪的组提前,组间相对顺序不变
    groups.sort_by_key(|(_, v)| !has_tracked(v));
    let mut out = Vec::with_capacity(all.len());
    for (_, v) in &groups {
        for &i in v {
            out.push(&all[i]);
        }
    }
    out
}

/// the connection list after the active filter, in display order
pub fn conn_list(app: &App) -> Vec<&Conn> {
    conns_display(app)
}

/// stable id of the selected row of a list box
pub fn selected_id(app: &App, idx: usize) -> Option<u64> {
    match idx {
        BOX_PROCS => procs_display(app)
            .get(app.sel[BOX_PROCS]?)
            .map(|p| p.pid as u64),
        BOX_LAN => app
            .lan
            .sorted_neighbors()
            .get(app.sel[BOX_LAN]?)
            .map(|n| u32::from(n.ip) as u64),
        BOX_CONNS => conn_items(app).get(app.sel[BOX_CONNS]?).map(|i| i.id()),
        _ => None,
    }
}

/// pid behind the selected row, when there is one
pub fn selected_pid(app: &App, idx: usize) -> Option<u32> {
    match idx {
        BOX_PROCS => procs_display(app).get(app.sel[BOX_PROCS]?).map(|p| p.pid),
        BOX_CONNS => conn_items(app)
            .get(app.sel[BOX_CONNS]?)
            .and_then(|i| i.conn())
            .and_then(|c| c.pid),
        _ => None,
    }
}

/// toggle tracking of the item shown in the detail panel
pub fn toggle_track(app: &mut App, idx: usize, id: u64) {
    match idx {
        BOX_PROCS => app.tracked.toggle_proc(id as u32),
        BOX_CONNS => app.tracked.toggle_conn(id),
        _ => {}
    }
}

/// rows that fit in a list box (border + header + the selected row's 2nd line)
pub fn box_capacity(idx: usize) -> usize {
    BOX_AREAS
        .lock()
        .unwrap()
        .iter()
        .find(|(i, _)| *i == idx)
        .map(|(_, r)| (r.height.saturating_sub(3)).max(1) as usize)
        .unwrap_or(10)
}

/// Re-anchor a box's selection onto the entity it names (pid for procs,
/// inode for conns) after the list re-sorted underneath it, and keep the row
/// inside the viewport. An entity that left the list clears the selection.
pub fn reselect(app: &mut App, idx: usize, id: u64) {
    let pos = match idx {
        BOX_PROCS => procs_display(app).iter().position(|p| p.pid as u64 == id),
        BOX_LAN => app
            .lan
            .sorted_neighbors()
            .iter()
            .position(|n| u32::from(n.ip) as u64 == id),
        BOX_CONNS => conn_items(app).iter().position(|i| i.id() == id),
        _ => None,
    };
    app.sel[idx] = pos;
    if let Some(n) = pos {
        let cap = box_capacity(idx);
        let off = &mut app.scroll[idx];
        if n < *off {
            *off = n;
        } else if n + 1 >= *off + cap {
            *off = (n + 1).saturating_sub(cap);
        }
    }
}

/// Move the cursor of a list box, keeping it inside the visible window
pub fn move_selection(app: &mut App, idx: usize, delta: i32) {
    let len = list_len(app, idx);
    if len == 0 {
        return;
    }
    let cap = box_capacity(idx);
    let next = match app.sel[idx] {
        None => Some(if delta < 0 { len - 1 } else { 0 }),
        Some(cur) => Some((cur as i32 + delta).clamp(0, len as i32 - 1) as usize),
    };
    app.sel[idx] = next;
    // keep the selected row (which is 2 lines tall) inside the viewport
    if let Some(n) = next {
        let off = &mut app.scroll[idx];
        if n < *off {
            *off = n;
        } else if n + 1 >= *off + cap {
            *off = (n + 1).saturating_sub(cap);
        }
    }
}

fn draw_net(f: &mut Frame, area: Rect, app: &App, focused: bool) {
    let name = app.net.selected.clone().unwrap_or_else(|| "-".to_string());
    // hostname and refresh interval live in the title (no separate top bar)
    let rest = vec![
        vec![text_s(app.hostname.clone())],
        vec![text_s(name.clone())],
        vec![
            key_span("-"),
            text_s("refresh ".to_string()),
            text_s(fmt_interval(app.tick)),
            key_span("+"),
        ],
    ];
    let inner = boxed(
        f,
        area,
        title_line(BOX_NET + 1, "net", 'n', rest, border_net(), focused),
        border_net(),
        BOX_NET + 1,
        focused,
    );
    if inner.width < 10 || inner.height < 4 {
        return;
    }

    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
        Constraint::Min(3),
    ])
    .split(inner);

    // interface info line
    let mut info: Vec<Span> = vec![Span::styled(" ", dim())];
    if let Some(i) = app.net.ifaces.get(&name) {
        let (mark, col) = if i.operstate == "up" {
            ("▲ up", hi())
        } else {
            ("▼ down", alert_end())
        };
        info.push(Span::styled(format!("{mark} "), Style::new().fg(col)));
        if let Some(s) = i.speed_mbps {
            info.push(Span::styled(format!("{s}Mb/s "), Style::new().fg(accent())));
        }
        if let Some(d) = &i.duplex {
            info.push(Span::styled(
                format!("{} ", if d == "full" { "FD" } else { "HD" }),
                dim(),
            ));
        }
        if let Some(m) = i.mtu {
            info.push(Span::styled(format!("mtu {m} "), dim()));
        }
        if let Some(a) = i.addrs.first() {
            info.push(Span::styled(a.clone(), Style::new().fg(accent())));
        }
        if i.addrs.len() > 1 {
            info.push(Span::styled(format!(" +{}", i.addrs.len() - 1), dim()));
        }
    }
    f.render_widget(Paragraph::new(Line::from(info)), chunks[0]);

    let rates = app.net.selected_rates();
    let (hrx, htx) = app.net.selected_hist();
    // 阻尼缩放峰值:即时跟涨、慢速回落——单帧尖峰不再把整窗波形压扁
    let (prx, ptx) = app.net.selected_peaks();
    // coupled scaling (opt-in): when one direction dwarfs the other, draw the
    // small side at its true fraction of the chart height instead of letting
    // auto-scale blow a tiny waveform up to full height
    let rows = chunks[2].height;
    let (rx_frac, tx_frac) = if app.net_graph_coupled {
        let f = graph::coupled_frac(ptx, prx, rows, app.net_graph_scale_ratio);
        if ptx < prx {
            (1.0, f)
        } else {
            (f, 1.0)
        }
    } else {
        (1.0, 1.0)
    };

    // 头行标签随列宽分级:≥52 全称,≥40 缩写,≥30 紧凑(Σa 无留白),
    // 更窄只留 session——alive Σ 是头行主指标,能塞下就不静默消失
    let head_w = chunks[1].width as usize;
    let sess_lbl = if head_w >= 52 {
        "  session Σ "
    } else {
        " Σs "
    };
    let (alive_lbl, _show_alive) = if head_w >= 52 {
        ("  alive Σ ", true)
    } else if head_w >= 40 {
        (" Σa ", true)
    } else if head_w >= 30 {
        ("Σa", true)
    } else {
        ("", false)
    };

    let alive = app.net.selected_alive();

    let rx_head = Line::from(vec![
        Span::styled(" ▼ RX ", bold(download().at(0.5))),
        Span::styled(
            rates.map(|r| fmt_speed(r.rx)).unwrap_or_else(|| "…".into()),
            bold(download().at(0.5)),
        ),
        Span::styled(sess_lbl, dim()),
        Span::styled(
            rates
                .map(|r| fmt_bytes(r.rx_total as f64))
                .unwrap_or_default(),
            text(),
        ),
        Span::styled(alive_lbl, dim()),
        Span::styled(
            if alive_lbl.is_empty() {
                String::new()
            } else {
                alive.map(|(a, _)| fmt_bytes(a as f64)).unwrap_or_default()
            },
            dim(),
        ),
    ]);
    let tx_head = Line::from(vec![
        Span::styled(" ▲ TX ", bold(upload().at(0.5))),
        Span::styled(
            rates.map(|r| fmt_speed(r.tx)).unwrap_or_else(|| "…".into()),
            bold(upload().at(0.5)),
        ),
        Span::styled(sess_lbl, dim()),
        Span::styled(
            rates
                .map(|r| fmt_bytes(r.tx_total as f64))
                .unwrap_or_default(),
            text(),
        ),
        Span::styled(alive_lbl, dim()),
        Span::styled(
            if alive_lbl.is_empty() {
                String::new()
            } else {
                alive.map(|(_, a)| fmt_bytes(a as f64)).unwrap_or_default()
            },
            dim(),
        ),
    ]);
    let tx_head = if tx_frac < 1.0 {
        let mut spans = tx_head.spans;
        spans.push(Span::styled(format!("  ×{tx_frac:.2}"), dim()));
        Line::from(spans)
    } else {
        tx_head
    };
    f.render_widget(Paragraph::new(rx_head), chunks[1]);
    f.render_widget(
        Paragraph::new(braille_graph(
            &hrx,
            chunks[2].width as usize,
            chunks[2].height as usize,
            GRAPH_FLOOR.max(prx),
            download(),
            app.graph_net,
            rx_frac,
        )),
        chunks[2],
    );
    f.render_widget(Paragraph::new(tx_head), chunks[3]);
    f.render_widget(
        Paragraph::new(braille_graph(
            &htx,
            chunks[4].width as usize,
            chunks[4].height as usize,
            GRAPH_FLOOR.max(ptx),
            upload(),
            app.graph_net,
            tx_frac,
        )),
        chunks[4],
    );
}

fn draw_lan(f: &mut Frame, area: Rect, app: &App, focused: bool) {
    let list = app.lan.sorted_neighbors();
    let subnets = app
        .lan
        .subnets
        .iter()
        .map(|(_, n, p)| format!("{n}/{p}"))
        .collect::<Vec<_>>()
        .join(" ");
    let sweep = if app.lan.sweeping {
        "scanning…"
    } else if app.lan.sweep_enabled {
        "on"
    } else {
        "off"
    };
    let per_remote = app.conn.per_remote_v4();
    let visible = area.height.saturating_sub(3).max(1) as usize;
    let off = clamped_offset(app, BOX_LAN, visible);
    let sel = if focused { app.sel[BOX_LAN] } else { None };
    let mut rest = vec![
        vec![text_s(subnets)],
        vec![text_s(format!("{} hosts", list.len()))],
        fword('s', "sweep", sweep),
        fword('e', "order", app.lan.sort.label()),
    ];
    let sh = scroll_hint(list.len(), off, area);
    if !sh.is_empty() {
        rest.push(vec![text_s(sh)]);
    }
    let inner = boxed(
        f,
        area,
        title_line(BOX_LAN + 1, "lan", 'l', rest, border_lan(), focused),
        border_lan(),
        BOX_LAN + 1,
        focused,
    );
    if inner.width < 10 || inner.height < 2 {
        return;
    }

    let headers = ["IP", "MAC", "HOSTNAME", "IF"];
    // 滚动窗口:一页 = 表头之下的全部行(inner 已不含边框,只减表头 1 行;
    // 旧写法 -2 把表头算两次,6 行高的盒子只渲染出 2 台设备)
    // mac:隐私门控提示(0.2.11 口径:给可执行的修复动作,而不是只描述限制)。
    // 列表空=整屏提示;非空(rt_scan 已填出无 MAC 行)=盒底一行不挤数据
    let mac_hint = if app.lan.arp_blocked {
        Some(if app.lang == Lang::Zh {
            "macOS 对未签名程序隐藏邻居 MAC —— 运行 ltop lan-setup 恢复真实 MAC(免 root)"
        } else {
            "macOS hides neighbour MACs from unsigned apps — run ltop lan-setup for real MACs (no root)"
        })
    } else {
        None
    };
    if list.is_empty() {
        if let Some(tip) = mac_hint {
            f.render_widget(
                Paragraph::new(vec![Line::from(vec![Span::styled(tip, dim())])]),
                inner,
            );
        }
        return;
    }
    let (table_area, hint_area) = match mac_hint {
        Some(tip) if inner.height >= 3 => {
            let chunks = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(inner);
            (chunks[0], Some((tip, chunks[1])))
        }
        _ => (inner, None),
    };
    let visible = table_area.height.saturating_sub(1) as usize;
    let rows: Vec<Row> = list
        .iter()
        .enumerate()
        .skip(off)
        .take(visible)
        .map(|(i, nb)| {
            let pinned = app.lan.tracked.contains(&nb.ip);
            let selected = sel == Some(i);
            let alt = (i - off) % 2 == 1;
            neighbor_row(
                nb,
                &app.lan.resolve_failures,
                per_remote.get(&nb.ip),
                pinned,
                selected,
                alt,
            )
        })
        .collect();
    let header = Row::new(headers).style(dim().add_modifier(Modifier::BOLD));
    // IP/MAC/IF have fixed formats; the hostname column takes the spare width
    let cols = fit_columns(&[16, 17, 0, 8], 2, 8, 30, table_area.width, 1);
    let table = Table::new(rows, cols).header(header).column_spacing(1);
    f.render_widget(table, table_area);
    if let Some((tip, area)) = hint_area {
        f.render_widget(
            Paragraph::new(vec![Line::from(vec![Span::styled(tip, dim())])]),
            area,
        );
    }
}

fn neighbor_row(
    nb: &Neighbor,
    resolve_failures: &std::collections::HashSet<std::net::Ipv4Addr>,
    conns: Option<&(usize, f64, f64, u64, u64)>,
    pinned: bool,
    selected: bool,
    alt: bool,
) -> Row<'static> {
    let (marker, color) = if nb.is_gateway {
        ("✦", hi())
    } else if nb.is_self {
        ("·", accent())
    } else {
        (" ", palette().text)
    };
    let host = match &nb.hostname {
        Some(h) => (h.clone(), palette().text),
        None if resolve_failures.contains(&nb.ip) => ("—".to_string(), palette().dim),
        None => ("resolving…".to_string(), palette().dim),
    };
    let host_text = host.0.clone();
    // 置顶行:高亮主色底(红);选中行:复色高亮(隔行双底色)。
    // 底色铺在 row 级 style 上,整行(含列间空隙)一体化,而非分散到列
    let (bg, base_fg) = if pinned {
        (Some(track_bg(0)), readable_fg(track_bg(0)))
    } else if selected {
        let selt = track_bg(alt as usize);
        (Some(selt), readable_fg(selt))
    } else {
        (None, palette().text)
    };
    let fg = |col: Color| if bg.is_some() { base_fg } else { col };
    let mut host_cell = Line::from(Span::styled(host_text.clone(), fg(host.1)));
    if let Some((n, r, _, tr, _)) = conns {
        if *n > 0 {
            let extra = format!(" ·{n}conn {}", fmt_speed(*r));
            let sum = if *tr > 0 {
                format!(" Σ{}", fmt_bytes(*tr as f64))
            } else {
                String::new()
            };
            host_cell = Line::from(vec![
                Span::styled(host_text, fg(host.1)),
                Span::styled(extra, fg(hi())),
                Span::styled(sum, fg(hi())),
            ]);
        }
    }
    // 普通行不设 bg:保留主题 main_bg(Reset 会盖掉主题底色,与 conn_row 同规)
    let style = match bg {
        Some(c) => Style::new().fg(base_fg).bg(c),
        None => Style::new().fg(base_fg),
    };
    let mac_plain = if nb.mac_hidden {
        "—".to_string()
    } else {
        nb.mac.clone().unwrap_or_else(|| "expired".into())
    };
    Row::new([
        Cell::from(Line::from(vec![
            Span::styled(marker.to_string(), fg(color)),
            Span::styled(nb.ip.to_string(), fg(color)),
        ])),
        Cell::from(Span::styled(mac_plain, fg(palette().text))),
        Cell::from(host_cell),
        Cell::from(Span::styled(nb.iface.clone(), fg(palette().text))),
    ])
    .style(style)
}

fn draw_diskio(f: &mut Frame, area: Rect, app: &App, focused: bool) {
    let (tr, tw, ti, sr, sw) = app.disk.totals();
    // r/w rates already live in the READ/WRITE section headers; the title
    // keeps only what is not shown inside
    let rest = vec![
        vec![text_s(format!("{:.0} iop/s", ti))],
        vec![text_s(format!("mode {}", app.io_graph.label()))],
    ];
    let inner = boxed(
        f,
        area,
        title_line(
            BOX_DISKIO + 1,
            "disk i/o",
            'i',
            rest,
            border_disk(),
            focused,
        ),
        border_disk(),
        BOX_DISKIO + 1,
        focused,
    );
    if inner.width < 16 || inner.height < 4 {
        return;
    }
    let (hr, hw) = app.disk.hist();
    let (pr, pw) = app.disk.peaks();
    match app.io_graph {
        IoGraphMode::Both => {
            let g = Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)])
                .split(inner);
            graph_section(
                f,
                g[0],
                " ▼ READ ",
                download(),
                &hr,
                tr,
                sr,
                app.graph_disk,
                pr,
            );
            graph_section(
                f,
                g[1],
                " ▲ WRITE ",
                upload(),
                &hw,
                tw,
                sw,
                app.graph_disk,
                pw,
            );
        }
        IoGraphMode::Read => graph_section(
            f,
            inner,
            " ▼ READ ",
            download(),
            &hr,
            tr,
            sr,
            app.graph_disk,
            pr,
        ),
        IoGraphMode::Write => graph_section(
            f,
            inner,
            " ▲ WRITE ",
            upload(),
            &hw,
            tw,
            sw,
            app.graph_disk,
            pw,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn graph_section(
    f: &mut Frame,
    area: Rect,
    label: &str,
    grad: Gradient,
    data: &[f64],
    total: f64,
    sum: u64,
    mode: GraphMode,
    peak: f64,
) {
    let g = Layout::vertical([Constraint::Length(1), Constraint::Min(2)]).split(area);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(label, bold(grad.at(0.5))),
            Span::styled(fmt_speed(total), bold(grad.at(0.5))),
            Span::styled("  Σ ", dim()),
            Span::styled(fmt_bytes(sum as f64), text()),
        ])),
        g[0],
    );
    f.render_widget(
        Paragraph::new(braille_graph(
            data,
            g[1].width as usize,
            g[1].height as usize,
            // 阻尼峰值做比例下限:单帧尖峰不压扁整窗波形
            GRAPH_FLOOR.max(peak),
            grad,
            mode,
            1.0,
        )),
        g[1],
    );
}

fn draw_disks(f: &mut Frame, area: Rect, app: &App, focused: bool) {
    let mut flat: Vec<(&TreeNode, String)> = Vec::new();
    emit_tree_rows(&app.disk.tree, "", 0, &mut flat);
    let flat_len = flat.len();
    // 表头之下的可视行数封顶:滚到底后停在完整最后一页,与 LAN/CONNS/PROCS
    // 的 clamped_offset 同语义;draw 与滚动条共用这一个偏移
    let visible = (area.height.saturating_sub(3)).max(1) as usize; // borders + header
    let off = app.scroll[BOX_DISKS].min(flat_len.saturating_sub(visible));
    // 口径 = 挂载卷数,不是数据面大小:mac 上 devices 还含恒零合成层与
    // 模拟器镜像(1 块物理盘能报 32 dev),dev 数会严重虚高;树的物理
    // 全貌照旧全画,标题只报用户语义上的"挂了多少卷"
    let vols = app
        .disk
        .devices
        .iter()
        .filter(|d| !d.mounts.is_empty())
        .count();
    let mut rest = vec![
        vec![text_s(format!("{vols} vol"))],
        vec![text_s(format!(
            "mounts: {}",
            if app.disks_expanded { "all" } else { "first" }
        ))],
    ];
    let sh = scroll_hint(flat_len, off, area);
    if !sh.is_empty() {
        rest.push(vec![text_s(sh)]);
    }
    let inner = boxed(
        f,
        area,
        title_line(BOX_DISKS + 1, "disks", 'd', rest, border_disk(), focused),
        border_disk(),
        BOX_DISKS + 1,
        focused,
    );
    if inner.width < 16 || inner.height < 2 {
        return;
    }

    let rows: Vec<Row> = flat
        .into_iter()
        .skip(off)
        .map(|(node, label)| disk_row(app, node, label))
        .collect();
    let headers = ["DEV", "USED", "R/S", "W/S"];
    let header = Row::new(headers).style(dim().add_modifier(Modifier::BOLD));
    // the scrollbar column takes the rightmost content column when active;
    // DEV absorbs the remaining slack, the bar and both rates are exact-width
    let bar = scrollbar_col(inner, flat_len, visible);
    let table_w = if bar.is_some() {
        inner.width - 1
    } else {
        inner.width
    };
    let cols = fit_columns(&[0, 16, 9, 9], 0, 10, 40, table_w, 1);
    let table = Table::new(rows, cols).header(header).column_spacing(1);
    f.render_widget(table, inner);
    if let Some(col) = bar {
        draw_scrollbar(f, col, flat_len, off, visible);
    }
}

fn disk_row(app: &App, node: &TreeNode, label: String) -> Row<'static> {
    let dev = app.disk.dev_by_name(&node.name);
    // everything after the device name (pretty alias, mounts) is truncatable
    // tail: it goes into its own span so the dim fstype tail keeps its place
    let mut rest = String::new();
    match dev {
        Some(d) => {
            if let Some(p) = &d.pretty {
                rest.push_str(&format!(" ({p})"));
            }
            if let Some(m) = d.mounts.first() {
                if app.disks_expanded {
                    rest.push_str(&format!(" {}", d.mounts.join(" ")));
                } else {
                    rest.push_str(&format!(" {m}"));
                    if d.mounts.len() > 1 {
                        rest.push_str(&format!(" +{}", d.mounts.len() - 1));
                    }
                }
            }
        }
        // mapper alias is still useful for unmounted devices (e.g. closed LUKS)
        None => {
            if let Some(p) = &node.pretty {
                rest.push_str(&format!(" ({p})"));
            }
        }
    }
    // usage bars only for rows backed by a filesystem (one per dev);
    // structural nodes (whole disks, carrier partitions) show their size
    let usage = if let Some(d) = dev {
        let frac = if d.total > 0 {
            d.used as f64 / d.total as f64
        } else {
            0.0
        };
        Line::from(vec![
            braille_bar(frac, 10, palette().used.at(frac as f32)),
            Span::styled(format!(" {:>3.0}%", frac * 100.0), text()),
        ])
    } else if let Some(sz) = node.size {
        Line::from(Span::styled(fmt_bytes(sz as f64), text()))
    } else {
        Line::from(Span::styled("", dim()))
    };
    let rate_r = dev.map(|d| fmt_speed(d.rate_r)).unwrap_or_default();
    let rate_w = dev.map(|d| fmt_speed(d.rate_w)).unwrap_or_default();
    // fs type as a dim tail right after the name: identity info that stays
    // visible on narrow terminals, where the pretty/mount tail truncates first
    let mut dev_spans = vec![Span::styled(label, text())];
    if let Some(fs) = dev.map(|d| d.fstype.as_str()).filter(|f| !f.is_empty()) {
        dev_spans.push(Span::styled(format!(" · {fs}"), dim()));
    }
    dev_spans.push(Span::styled(rest, text()));
    Row::new([
        Cell::from(Line::from(dev_spans)),
        Cell::from(usage),
        Cell::from(Span::styled(rate_r, Style::new().fg(download().at(0.5)))),
        Cell::from(Span::styled(rate_w, Style::new().fg(upload().at(0.5)))),
    ])
}

/// 选中聚合行时的详情:组 Σ/速率 + 成员连接清单(最多 3 条,余者计数)
/// 选中聚合行时的详情:单行组摘要(Σ 与速率小计;子行细节展开后自见)
fn proc_group_detail(name: &str, info: GroupInfo) -> Vec<Line<'static>> {
    let k = dim();
    let line1 = vec![
        Span::styled(format!(" {} ×{} ", name, info.n), bold(accent())),
        Span::styled("  Σ ", k),
        Span::styled(
            format!(
                "{}{} {}{} ",
                '\u{2193}',
                fmt_bytes(info.rx as f64),
                '\u{2191}',
                fmt_bytes(info.tx as f64)
            ),
            text(),
        ),
        Span::styled("rate ", k),
        Span::styled(
            format!(
                "{}{}/s {}{}/s",
                '\u{2193}',
                fmt_speed(info.rr),
                '\u{2191}',
                fmt_speed(info.rs)
            ),
            text(),
        ),
    ];
    vec![Line::from(line1)]
}

/// proc 分组的聚合父行:进程级 Σ 与连接数(M1,无折叠——子行全部展开)
fn conn_row_aggregate(
    name: &str,
    n: usize,
    open: bool,
    rx: u64,
    tx: u64,
    slot: Option<usize>,
) -> Row<'static> {
    if open {
        // 展开态:高亮底色(主题下载渐变顶,亮度向主题 bg 缓动到 text
        // 可读;追踪行保留隔行底色),整行正文色
        let bg = readable_bg(match slot {
            Some(sl) => track_bg(sl % 2),
            None => track_bg(0),
        });
        let cell = |s: String| Cell::from(Span::styled(s, Style::new().fg(palette().text).bg(bg)));
        Row::new([
            cell(format!("\u{25be}{name} ×{n} Σ")),
            cell("-".into()),
            cell("-".into()),
            cell(format!("{n} conns")),
            cell(fmt_bytes(rx as f64)),
            cell(fmt_bytes(tx as f64)),
        ])
        .style(Style::new().bg(bg))
    } else {
        // 折叠态:无底色,常规配色(名字强调色 + Σ 标签 dim + 数值渐变色)
        let paint = |col: Color| slot.map(track_fg).unwrap_or(col);
        Row::new([
            Cell::from(Line::from(vec![
                Span::styled(
                    format!("\u{25b8}{name} ×{n}"),
                    Style::new().fg(paint(accent())),
                ),
                Span::styled(" Σ", Style::new().fg(paint(palette().dim))),
            ])),
            Cell::from(Span::styled("-", Style::new().fg(paint(palette().dim)))),
            Cell::from(Span::styled("-", Style::new().fg(paint(palette().dim)))),
            Cell::from(Span::styled(
                format!("{n} conns"),
                Style::new().fg(paint(accent())),
            )),
            Cell::from(Span::styled(
                fmt_bytes(rx as f64),
                Style::new().fg(paint(download().at(0.5))),
            )),
            Cell::from(Span::styled(
                fmt_bytes(tx as f64),
                Style::new().fg(paint(upload().at(0.5))),
            )),
        ])
    }
}

fn draw_conns(f: &mut Frame, area: Rect, app: &App, focused: bool) {
    let filter = app.conn.filter;
    let items = conn_items(app);
    // 滚动封顶 = 最后一页,而非最后一行(旧写法滚到底只剩 1 行且卡死)
    let visible = area.height.saturating_sub(3).max(1) as usize;
    let off = clamped_offset(app, BOX_CONNS, visible);
    // same as procs: the selection overlay follows focus, not persistence
    let sel = if focused { app.sel[BOX_CONNS] } else { None };
    let mut rest = vec![
        vec![text_s(format!("{}", items.len()))],
        fword('f', "filter", filter.label()),
        fword('o', "order", app.conn.sort.label()),
    ];
    // 采集降级徽标:netlink 被拒(linux)/nettop 失败(mac)时 CONNS 可能
    // 比实际少,明示而不是让用户以为"没有连接"
    #[cfg(target_os = "linux")]
    let degraded = app.conn.diag_error.clone();
    #[cfg(target_os = "macos")]
    let degraded = if app.nettop.stale {
        Some("nettop".to_string())
    } else {
        None
    };
    if let Some(e) = degraded {
        rest.push(vec![Span::styled(format!("⚠ {e}"), bold(alert_end()))]);
    }
    let sh = scroll_hint(items.len(), off, area);
    if !sh.is_empty() {
        rest.push(vec![text_s(sh)]);
    }
    let inner = boxed(
        f,
        area,
        title_line(BOX_CONNS + 1, "conns", 'c', rest, border_lan(), focused),
        border_lan(),
        BOX_CONNS + 1,
        focused,
    );
    if inner.width < 16 || inner.height < 2 {
        return;
    }

    // sum/proc 排序下这两列显示的都是合计量,表头如实标注
    let total_mode = matches!(app.conn.sort, ConnSort::Sum | ConnSort::Proc);
    let headers = if total_mode {
        ["NAME", "PID", "LOCAL", "REMOTE", "\u{3a3} RX", "\u{3a3} TX"]
    } else {
        ["NAME", "PID", "LOCAL", "REMOTE", "RX/S", "TX/S"]
    };
    let mut rows: Vec<Row> = Vec::new();
    let mut sel_y = None;
    for (i, item) in items.iter().enumerate().skip(off).take(visible) {
        let id = item.id();
        let slot = app.tracked.conn_slot(id);
        let mut row = match item {
            ConnItem::Agg { name, info, .. } => {
                // 展开态的聚合行 = 打开状态
                conn_row_aggregate(
                    name,
                    info.n,
                    group_is_open(app, name),
                    info.rx,
                    info.tx,
                    slot,
                )
            }
            ConnItem::Row { c, child } => conn_row(c, app, slot, *child),
        };
        if sel == Some(i) {
            row = row.height(2);
            sel_y = Some(inner.y + 1 + (i - off) as u16);
        }
        rows.push(row);
    }
    let header = Row::new(headers).style(dim().add_modifier(Modifier::BOLD));
    // squeeze the address columns first when the box is narrow; rates stay exact
    // name column min 12: it now carries the 3-4 char protocol label
    let cols = fit_columns(&[0, 7, 18, 18, 9, 9], 0, 12, 24, inner.width, 1);
    let table = Table::new(rows, cols).header(header).column_spacing(1);
    f.render_widget(table, inner);
    if let (Some(y), Some(si)) = (sel_y, sel) {
        if let Some(item) = items.get(si) {
            if let Some(rect) = overlay_rect(inner, y) {
                // 选中聚合父行:详情显示进程组摘要(Σ + 速率小计),
                // 子行细节展开后自见;选中普通行仍是该连接详情
                let lines = match item {
                    ConnItem::Agg { name, info, .. } => proc_group_detail(name, *info),
                    ConnItem::Row { c, .. } => conn_detail(c, app),
                };
                draw_selected(f, rect, lines);
            }
        }
    }
}

/// Two-line selected-row overlay starting at `y`, clipped to the box interior.
/// The selected row can sit on the frame's bottom edge (bottom-most box, last
/// visible row); unclipped, the overlay's second line would land outside the
/// buffer and panic inside ratatui (observed as "index … (126, 67)" on a
/// 249x67 terminal).
fn overlay_rect(inner: Rect, y: u16) -> Option<Rect> {
    if y < inner.y || y >= inner.bottom() {
        return None; // row not on screen
    }
    Some(Rect {
        x: inner.x,
        y,
        width: inner.width,
        height: 2.min(inner.bottom() - y),
    })
}

/// selection wash for the current palette (see `selection_bg_over`)
fn selection_bg() -> Color {
    let pal = palette();
    selection_bg_over(download().at(0.12), pal.bg, pal.text)
}

/// two-line detail drawn over the selected row
fn draw_selected(f: &mut Frame, rect: Rect, lines: Vec<Line<'static>>) {
    // 选中底 = selection_bg():主题 bg 上扫 download 渐变色相的强度,保证
    // 既与行底可分、又不吞掉 overlay 内 span 的语义色(accent/dim/红绿);
    // 色相本身分不出时向 text 提亮同色相再扫。Paragraph 只铺 bg 不铺 fg,
    // span 各自的显式色全部保留。
    let bgc = selection_bg();
    // wipe the underlying table text first — Paragraph.style only recolors
    // cells, it does not erase the symbols underneath
    f.render_widget(Clear, rect);
    let mut ls: Vec<Line> = lines.into_iter().take(2).collect();
    while ls.len() < 2 {
        ls.push(Line::from(""));
    }
    // 左缘竖条:选中锚点;accent 与选中底拉不开时退到可读前景
    let bar = if crate::theme::contrast_ratio(accent(), bgc) >= 1.6 {
        accent()
    } else {
        readable_fg(bgc)
    };
    let bar_w = 1.min(rect.width);
    f.render_widget(
        Paragraph::new(vec![Line::from("▌"), Line::from("▌")]).style(Style::new().fg(bar).bg(bgc)),
        Rect {
            x: rect.x,
            y: rect.y,
            width: bar_w,
            height: rect.height,
        },
    );
    let rest = Rect {
        x: rect.x + bar_w,
        y: rect.y,
        width: rect.width.saturating_sub(bar_w),
        height: rect.height,
    };
    f.render_widget(Paragraph::new(ls).style(Style::new().bg(bgc)), rest);
}

/// two overlay lines for a selected connection
fn conn_detail(c: &Conn, app: &App) -> Vec<Line<'static>> {
    let name = c.proc.clone().unwrap_or_else(|| app.conn.user_name(c.uid));
    let pid = c.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into());
    // UDP has no LISTEN/ESTAB states; the plain label says everything
    let mut state = if c.is_listen() {
        "tcp LISTEN".to_string()
    } else if c.is_tcp() {
        "tcp ESTAB".to_string()
    } else {
        "udp".to_string()
    };
    if c.dead {
        state.push_str(" closed");
    }
    let k = Style::new().fg(palette().dim);
    let dl = download().at(0.5);
    let up = upload().at(0.5);
    let (local_s, remote_s) = if c.is_listen() {
        (sock_str(&c.local), "—".to_string())
    } else {
        (sock_str(&c.local), sock_str(&c.remote))
    };
    vec![
        Line::from(vec![
            Span::styled(format!(" {} ", name), bold(accent())),
            Span::styled(format!("(pid {})   ", pid), dim()),
            Span::styled(format!("{state}   ",), text()),
            Span::styled("inode ", k),
            Span::styled(c.inode.to_string(), text()),
            Span::styled(
                match c.first_seen {
                    Some(fs) => {
                        let end = c.dead_at.unwrap_or_else(std::time::Instant::now);
                        format!(
                            "   {} {}",
                            app.lang.tr("lifetime"),
                            fmt_dur(end.duration_since(fs).as_secs())
                        )
                    }
                    _ => String::new(),
                },
                dim(),
            ),
        ]),
        Line::from(vec![
            Span::styled(format!("{}  ", local_s), text()),
            Span::styled("→ ".to_string(), k),
            Span::styled(format!("{}   ", remote_s), text()),
            Span::styled("↓ ", Style::new().fg(dl)),
            Span::styled(
                format!("{} ", fmt_bytes(c.bytes_recv as f64)),
                Style::new().fg(dl),
            ),
            Span::styled(format!("({})   ", fmt_speed(c.rate_recv)), dim()),
            Span::styled("↑ ", Style::new().fg(up)),
            Span::styled(
                format!("{} ", fmt_bytes(c.bytes_sent as f64)),
                Style::new().fg(up),
            ),
            Span::styled(format!("({})", fmt_speed(c.rate_sent)), dim()),
        ]),
    ]
}

fn conn_row(c: &Conn, app: &App, slot: Option<usize>, child: bool) -> Row<'static> {
    // 墓碑行整体压暗(协议标签/远端一并),语义是"已关闭的余像"
    let dim_all = c.dead;
    let paint = |col: Color| {
        if dim_all {
            palette().dim
        } else {
            slot.map(track_fg).unwrap_or(col)
        }
    };
    // proc 档与 sum 同为合计口径:表头 Σ 对应的单元格显示字节数
    let sum_mode = matches!(app.conn.sort, ConnSort::Sum | ConnSort::Proc);
    let proc_str = match &c.proc {
        Some(n) => n.clone(),
        None => app.conn.user_name(c.uid),
    };
    let pid_str = c.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into());
    let proc_color = if c.proc.is_some() {
        accent()
    } else {
        palette().dim
    };
    // protocol label replaces the old ▸/⋄ glyphs: explicit text needs no
    // legend. The ◦ listen marker merges into the label and survives the
    // narrow name column (the old "tcp listen" wording truncated there).
    let label = if c.is_listen() {
        "\u{25e6}tcp"
    } else if c.is_tcp() {
        "tcp"
    } else {
        "udp"
    };
    let (local_s, remote_s) = if c.is_listen() {
        (sock_str(&c.local), "—".to_string())
    } else {
        (sock_str(&c.local), sock_str(&c.remote))
    };
    let mark = "";
    let (r_cell, w_cell) = if sum_mode {
        (
            fmt_bytes(c.bytes_recv as f64),
            fmt_bytes(c.bytes_sent as f64),
        )
    } else {
        (fmt_speed(c.rate_recv), fmt_speed(c.rate_sent))
    };
    let rx = format!("{mark}{r_cell}");
    let tx = format!("{mark}{w_cell}");
    // 展开组的子行树形缩进两级,层次一眼分明
    let indent = if child { "  " } else { "" };
    Row::new([
        Cell::from(Line::from(vec![
            Span::styled(
                format!("{indent}{proc_str}"),
                Style::new().fg(paint(proc_color)),
            ),
            Span::styled(format!(" {label}"), Style::new().fg(paint(palette().dim))),
        ])),
        Cell::from(Span::styled(pid_str, Style::new().fg(paint(palette().dim)))),
        Cell::from(Span::styled(
            local_s,
            Style::new().fg(paint(palette().text)),
        )),
        Cell::from(Span::styled(
            remote_s,
            Style::new().fg(paint(palette().text)),
        )),
        Cell::from(Span::styled(rx, Style::new().fg(paint(download().at(0.5))))),
        Cell::from(Span::styled(tx, Style::new().fg(paint(upload().at(0.5))))),
    ])
    .style(match slot {
        Some(sl) => Style::new().bg(track_bg(sl)),
        None => Style::new(),
    })
}

fn draw_procs(f: &mut Frame, area: Rect, app: &App, focused: bool) {
    let list = procs_display(app);
    let visible = area.height.saturating_sub(3).max(1) as usize;
    let off = clamped_offset(app, BOX_PROCS, visible);
    // the two-line selection overlay only makes sense while the box holds
    // focus — otherwise it lingers after p/c unfocus and reads as a stuck
    // highlight
    let sel = if focused { app.sel[BOX_PROCS] } else { None };
    let mut rest = vec![
        vec![text_s(format!("{} procs", app.proc.procs.len()))],
        fword('r', "order", app.proc.sort.label()),
    ];
    if app.proc.is_root {
        rest.push(vec![text_s("root".to_string())]);
    }
    if app.log.enabled {
        rest.push(vec![Span::styled("● rec", bold(hi()))]);
    }
    // eBPF UDP accounting is live: the ltop-bpf daemon snapshot is fresh
    if app.udp.fresh {
        rest.push(vec![Span::styled("udp", bold(hi()))]);
    }
    let sh = scroll_hint(list.len(), off, area);
    if !sh.is_empty() {
        rest.push(vec![text_s(sh)]);
    }
    let inner = boxed(
        f,
        area,
        title_line(BOX_PROCS + 1, "procs", 'p', rest, border_lan(), focused),
        border_lan(),
        BOX_PROCS + 1,
        focused,
    );
    if inner.width < 16 || inner.height < 2 {
        return;
    }

    let headers = [
        "NAME",
        "PID",
        "Σ NET↓",
        "Σ NET↑",
        "↓/s",
        "↑/s",
        "DISK ΣR",
        "DISK ΣW",
    ];
    let mut rows: Vec<Row> = Vec::new();
    let mut sel_y = None;
    // rows AND the selection overlay must read the same list: procs_display
    // is reordered (tracked first), so indexing into the raw procs vec shows
    // the overlay of whichever process sat at that position before pinning
    let list = procs_display(app);
    for (i, p) in list.iter().enumerate().skip(off).take(visible) {
        let slot = app.tracked.proc_slot(p.pid);
        let paint = |col: Color| slot.map(track_fg).unwrap_or(col);
        let cells = [
            p.name.clone(),
            p.pid.to_string(),
            fmt_bytes(p.net_recv as f64),
            fmt_bytes(p.net_sent as f64),
            fmt_speed(p.net_rr),
            fmt_speed(p.net_sr),
            fmt_bytes(p.disk_r as f64),
            fmt_bytes(p.disk_w as f64),
        ];
        let mut row = Row::new([
            Cell::from(Span::styled(
                cells[0].clone(),
                Style::new().fg(paint(if p.alive {
                    palette().text
                } else {
                    palette().dim
                })),
            )),
            Cell::from(Span::styled(
                cells[1].clone(),
                Style::new().fg(paint(palette().text)),
            )),
            Cell::from(Span::styled(
                cells[2].clone(),
                Style::new().fg(paint(download().at(0.5))),
            )),
            Cell::from(Span::styled(
                cells[3].clone(),
                Style::new().fg(paint(upload().at(0.5))),
            )),
            Cell::from(Span::styled(
                cells[4].clone(),
                Style::new().fg(paint(download().at(0.5))),
            )),
            Cell::from(Span::styled(
                cells[5].clone(),
                Style::new().fg(paint(upload().at(0.5))),
            )),
            Cell::from(Span::styled(
                cells[6].clone(),
                Style::new().fg(paint(download().at(1.0))),
            )),
            Cell::from(Span::styled(
                cells[7].clone(),
                Style::new().fg(paint(upload().at(1.0))),
            )),
        ])
        .style(match slot {
            Some(sl) => Style::new().bg(track_bg(sl)),
            None => Style::new(),
        });
        if sel == Some(i) {
            row = row.height(2);
            sel_y = Some(inner.y + 1 + (i - off) as u16);
        }
        rows.push(row);
    }
    let header = Row::new(headers).style(dim().add_modifier(Modifier::BOLD));
    // every numeric column is exact; only the process name stretches
    let cols = fit_columns(&[0, 7, 7, 7, 9, 9, 7, 7], 0, 8, 24, inner.width, 1);
    let table = Table::new(rows, cols).header(header).column_spacing(1);
    f.render_widget(table, inner);
    if let (Some(y), Some(si)) = (sel_y, sel) {
        if let Some(p) = list.get(si) {
            if let Some(rect) = overlay_rect(inner, y) {
                draw_selected(f, rect, proc_detail(p, app.lang));
            }
        }
    }
}

/// two overlay lines for a selected process
fn proc_detail(p: &crate::collect::proc::ProcRow, lang: crate::config::Lang) -> Vec<Line<'static>> {
    let cmd = std::fs::read(format!("/proc/{}/cmdline", p.pid))
        .ok()
        .map(|b| {
            let s: String = b
                .split(|&c| c == 0)
                .filter(|s| !s.is_empty())
                .map(|s| String::from_utf8_lossy(s).into_owned())
                .collect::<Vec<_>>()
                .join(" ");
            s
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "-".to_string());
    let k = Style::new().fg(palette().dim);
    let dl = download().at(0.5);
    let up = upload().at(0.5);
    let arrows = ['\u{2193}', '\u{2191}'];
    vec![
        Line::from(vec![
            Span::styled(format!(" {} ", p.name), bold(accent())),
            Span::styled(format!("(pid {})   ", p.pid), dim()),
            Span::styled(
                match p.uptime_s {
                    Some(s) => format!("{} {}   ", lang.tr("uptime"), fmt_dur(s)),
                    None => String::new(),
                },
                dim(),
            ),
            Span::styled("net ", k),
            Span::styled(
                format!("{}{}  ", arrows[0], fmt_bytes(p.net_recv as f64)),
                Style::new().fg(dl),
            ),
            Span::styled(
                format!("{}{}   ", arrows[1], fmt_bytes(p.net_sent as f64)),
                Style::new().fg(up),
            ),
            Span::styled("rate ", k),
            Span::styled(
                format!("{}{} ", arrows[0], fmt_speed(p.net_rr)),
                Style::new().fg(dl),
            ),
            Span::styled(
                format!("{}{}", arrows[1], fmt_speed(p.net_sr)),
                Style::new().fg(up),
            ),
            Span::styled("  udp ", k),
            Span::styled(
                format!(
                    "{}Σ{} {}{} ",
                    arrows[0],
                    fmt_bytes(p.udp_recv as f64),
                    arrows[1],
                    fmt_bytes(p.udp_sent as f64)
                ),
                Style::new().fg(dl),
            ),
            Span::styled(
                format!(
                    "{}{}/s {}{}/s",
                    arrows[0],
                    fmt_speed(p.udp_rr),
                    arrows[1],
                    fmt_speed(p.udp_sr)
                ),
                Style::new().fg(up),
            ),
        ]),
        Line::from(vec![
            Span::styled("disk ", k),
            Span::styled(
                format!("\u{3a3}R{} ", fmt_bytes(p.disk_r as f64)),
                Style::new().fg(dl),
            ),
            Span::styled(
                format!("\u{3a3}W{}   ", fmt_bytes(p.disk_w as f64)),
                Style::new().fg(up),
            ),
            Span::styled("rate ", k),
            Span::styled(format!("R{} ", fmt_speed(p.disk_rr)), Style::new().fg(dl)),
            Span::styled(format!("W{}", fmt_speed(p.disk_wr)), Style::new().fg(up)),
            Span::styled("   cmd ", k),
            Span::styled(cmd, text()),
        ]),
    ]
}

fn emit_tree_rows<'a>(
    nodes: &'a [TreeNode],
    prefix: &str,
    depth: usize,
    out: &mut Vec<(&'a TreeNode, String)>,
) {
    let n = nodes.len();
    for (i, node) in nodes.iter().enumerate() {
        let last = i + 1 == n;
        let glyph = if depth == 0 {
            String::new()
        } else if last {
            "└ ".to_string()
        } else {
            "├ ".to_string()
        };
        out.push((node, format!("{prefix}{glyph}{}", node.name)));
        let branch = if last { "  " } else { "│ " };
        let child_prefix = if depth == 0 {
            String::new()
        } else {
            format!("{prefix}{branch}")
        };
        emit_tree_rows(&node.children, &child_prefix, depth + 1, out);
    }
}

/// footer 按键统一样式:[键] hi_fg + 标签 dim,主页与搜索栏共用
fn footer_key(k: &str, label: &str) -> Vec<Span<'static>> {
    vec![
        Span::styled(format!("[{k}]"), Style::new().fg(hi())),
        Span::styled(format!(" {label}  "), dim()),
    ]
}

/// 终端显示宽度(CJK 与全角字符双宽):弹窗布局宽度一律按它算——按字符数
/// 计算会把中文面板撑出边界,内容被整列裁掉
fn disp_w(s: &str) -> usize {
    use unicode_width::UnicodeWidthStr;
    UnicodeWidthStr::width(s)
}

fn draw_footer(f: &mut Frame, area: Rect, app: &App) {
    let cols = Layout::horizontal([Constraint::Min(0), Constraint::Length(9)]).split(area);
    let key = |k: &str, label: &str| footer_key(k, label);
    let lang = app.lang;
    let mut spans: Vec<Span> = vec![Span::styled(" ", dim())];
    if app.paused {
        spans.push(Span::styled(
            format!(" ⏸ {} ", app.lang.tr("paused")),
            Style::new()
                .fg(bg().unwrap_or(Color::Reset))
                .bg(hi())
                .add_modifier(Modifier::BOLD),
        ));
    }
    spans.extend(key("q", lang.tr("quit")));
    spans.extend(key("m", lang.tr("menu")));
    spans.extend(key("space", lang.tr("pause")));
    spans.extend(key("/", lang.tr("search")));
    spans.extend(key(
        "Tab",
        &format!("{}:{}", lang.tr("layout"), app.layout.label()),
    ));
    spans.extend(key("1-6", lang.tr("boxes")));
    spans.extend(key("z", lang.tr("zoom")));
    if app.allow_global_kill {
        spans.extend(key("k", lang.tr("kill")));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), cols[0]);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!("{} ", local_time_str()),
            Style::new().fg(accent()),
        ))),
        cols[1],
    );
}

/// btop-style bottom menu bar: [ themes ] [ reset ] [ graphs ] [ about ] [ quit ] [ close ]
fn draw_menu_footer(f: &mut Frame, area: Rect, sel: usize, lang: Lang) {
    let labels = [
        "themes", "settings", "reset", "graphs", "help", "quit", "close",
    ];
    let mut spans: Vec<Span> = vec![Span::styled(" ", dim())];
    for (i, l) in labels.iter().enumerate() {
        let style = if i == sel {
            Style::new()
                .fg(bg().unwrap_or(Color::Reset))
                .bg(hi())
                .add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(palette().text)
        };
        spans.push(Span::styled(format!(" {} ", lang.tr(l)), style));
        spans.push(Span::styled("  ", dim()));
    }
    spans.push(Span::styled(lang.tr("hint"), dim()));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// graphs 面板:与 settings 同款风格(居中弹窗、label: value 行列表)
fn draw_graphs_popup(f: &mut Frame, area: Rect, app: &App, sel: usize) {
    let lang = app.lang;
    let ratio_label = |r: u32| format!("{r}×");
    let rows: [(&str, &str, String); 5] = [
        ("n", "net", app.graph_net.label().to_string()),
        ("d", "disk i/o", app.graph_disk.label().to_string()),
        ("a", "all", lang.tr("set_both").to_string()),
        (
            "c",
            "coupled scaling",
            if app.net_graph_coupled {
                format!("on ({})", ratio_label(app.net_graph_scale_ratio))
            } else {
                lang.tr("off").to_string()
            },
        ),
        ("r", "scale ratio", ratio_label(app.net_graph_scale_ratio)),
    ];
    let (w, h) = (60.min(area.width), 9.min(area.height));
    let popup = centered(area, w, h);
    f.render_widget(Clear, popup);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .style(Style::new().bg(bg().unwrap_or(Color::Reset)))
        .border_style(Style::new().fg(hi()))
        .title(title_line(0, "graphs", '\0', vec![], hi(), false));
    let inner = block.inner(popup);
    f.render_widget(block, popup);
    let mut lines: Vec<Line> = rows
        .iter()
        .enumerate()
        .map(|(i, (k, word, value))| {
            let sel_style = Style::new()
                .fg(bg().unwrap_or(Color::Reset))
                .bg(hi())
                .add_modifier(Modifier::BOLD);
            let (ks, ls, vs) = if i == sel {
                (sel_style, sel_style, sel_style)
            } else {
                (
                    Style::new().fg(palette().title),
                    text(),
                    Style::new().fg(palette().dim),
                )
            };
            Line::from(vec![
                Span::styled(" [", dim()),
                Span::styled(k.to_string(), ks),
                Span::styled(format!("] {word}"), ls),
                Span::styled(format!("  {value}"), vs),
            ])
        })
        .collect();
    lines.push(Line::from(Span::styled(lang.tr("letters_hint"), dim())));
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_reset_popup(f: &mut Frame, area: Rect, sel: usize, lang: Lang) {
    let items = [
        ("n", "net", lang.tr("reset_net")),
        ("d", "disk", lang.tr("reset_disk")),
        ("a", "all", lang.tr("reset_all")),
    ];
    let (w, h) = (34.min(area.width), 5.min(area.height));
    let popup = centered(area, w, h);
    f.render_widget(Clear, popup);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .style(Style::new().bg(bg().unwrap_or(Color::Reset)))
        .border_style(Style::new().fg(hi()))
        .title(title_line(0, "reset", '\0', vec![], hi(), false));
    let inner = block.inner(popup);
    f.render_widget(block, popup);
    let lines = items
        .iter()
        .enumerate()
        .map(|(i, (k, word, desc))| {
            let style = if i == sel {
                Style::new()
                    .fg(bg().unwrap_or(Color::Reset))
                    .bg(hi())
                    .add_modifier(Modifier::BOLD)
            } else {
                text()
            };
            Line::from(vec![
                Span::styled(" [", dim()),
                Span::styled(
                    k.to_string(),
                    Style::new().fg(if i == sel {
                        bg().unwrap_or(Color::Reset)
                    } else {
                        hi()
                    }),
                ),
                Span::styled(format!("] {word}"), style),
                Span::styled(format!("  {desc}"), dim()),
            ])
        })
        .collect::<Vec<_>>();
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_settings_popup(f: &mut Frame, area: Rect, app: &App, sel: usize) {
    let lang = app.lang;
    let retention = if app.log.retention_days == 0 {
        lang.tr("never").to_string()
    } else {
        format!("{}d", app.log.retention_days)
    };
    let toggle = |v: bool| lang.tr(if v { "on" } else { "off" }).to_string();
    // 分组显示:行序即 cycle_setting 的 sel 索引(测试
    // settings_rows_map_to_their_values 钉住映射)
    let sections: Vec<(&str, Vec<(&str, String)>)> = vec![
        (
            "group_general",
            vec![
                ("language", lang.label().to_string()),
                ("refresh", fmt_interval(app.tick)),
                ("layout", app.layout.label().to_string()),
            ],
        ),
        (
            "group_graphs",
            vec![
                ("net", app.graph_net.label().to_string()),
                ("disk", app.graph_disk.label().to_string()),
            ],
        ),
        (
            "group_recording",
            vec![
                ("recording", toggle(app.log.enabled)),
                ("global_log", toggle(app.allow_global_log)),
                ("log_interval", fmt_interval(app.log.interval)),
                ("log_retention", retention),
                ("tracked_only", toggle(app.log_tracked_only)),
            ],
        ),
        (
            "group_lan",
            vec![
                ("sweep_interval", fmt_interval(app.lan.sweep_interval)),
                ("sweep", toggle(app.lan.sweep_enabled)),
            ],
        ),
        (
            "group_procs",
            vec![
                (
                    "tombstone",
                    if app.conn.tombstone_ttl.is_zero() {
                        lang.tr("off").to_string()
                    } else {
                        fmt_interval(app.conn.tombstone_ttl)
                    },
                ),
                (
                    "proc_groups",
                    lang.tr(if app.proc_expand_default {
                        "expanded"
                    } else {
                        "collapsed"
                    })
                    .to_string(),
                ),
                ("global_kill", toggle(app.allow_global_kill)),
            ],
        ),
    ];
    // 高度=内容行数(5 分组头+15 行+提示行)±配置路径行,收紧不留空洞
    let (w, h) = (60.min(area.width), 24.min(area.height));
    let popup = centered(area, w, h);
    f.render_widget(Clear, popup);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .style(Style::new().bg(bg().unwrap_or(Color::Reset)))
        .border_style(Style::new().fg(hi()))
        .title(title_line(0, "settings", '\0', vec![], hi(), false));
    let inner = block.inner(popup);
    f.render_widget(block, popup);
    let mut lines: Vec<Line> = Vec::new();
    let mut sel_row = 0usize;
    for (gkey, rows) in &sections {
        lines.push(Line::from(Span::styled(
            format!(" ── {} ──", lang.tr(gkey)),
            dim(),
        )));
        for (label, value) in rows {
            let selected = sel_row == sel;
            let (ls, vs) = if selected {
                (
                    Style::new()
                        .fg(bg().unwrap_or(Color::Reset))
                        .bg(hi())
                        .add_modifier(Modifier::BOLD),
                    Style::new()
                        .fg(bg().unwrap_or(Color::Reset))
                        .bg(hi())
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                (Style::new().fg(palette().title), text())
            };
            lines.push(Line::from(vec![
                Span::styled(format!(" {}: ", lang.tr(label)), ls),
                Span::styled(value.clone(), vs),
            ]));
            sel_row += 1;
        }
    }
    lines.push(Line::from(Span::styled(lang.tr("hint"), dim())));
    if let Some(p) = Config::path() {
        let mut path = p.display().to_string();
        let prefix = format!(" {}: ", lang.tr("config_path"));
        // 中文标签双宽:前缀宽度与截断都按显示宽度算,否则路径溢出弹窗
        let room = (inner.width.saturating_sub(disp_w(&prefix) as u16)) as usize;
        if disp_w(&path) > room {
            let mut s = path.as_str();
            let mut w = disp_w(s);
            while w > room.saturating_sub(1) {
                let mut it = s.chars();
                let c = it.next().unwrap_or('\0');
                if c == '\0' {
                    break;
                }
                s = it.as_str();
                w -= disp_w(c.to_string().as_str());
            }
            path = format!("…{s}");
        }
        lines.push(Line::from(Span::styled(format!("{prefix}{path}"), dim())));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

/// extra /proc fields for the detail panel
/// structured field for the detail popup: (label, value)
type Field = (&'static str, String);

/// detail 弹窗字段值的配色层级:Text=正文,Dl/Up=收发色(与 NET 盒图表
/// 同色彩语言),Accent=名字,Hi=状态徽章
#[derive(Clone, Copy, PartialEq)]
enum ValStyle {
    Text,
    Dl,
    Up,
    Accent,
    Hi,
}

/// detail 弹窗的字段分组:(组标题, 字段组);空标题 = 首组(无标题行)
type FieldGroup = (&'static str, Vec<(Field, ValStyle)>);

fn val_style(s: ValStyle) -> Style {
    match s {
        ValStyle::Text => text(),
        ValStyle::Dl => Style::new().fg(download().at(0.5)),
        ValStyle::Up => Style::new().fg(upload().at(0.5)),
        ValStyle::Accent => Style::new().fg(accent()),
        ValStyle::Hi => Style::new().fg(hi()),
    }
}

fn proc_status_fields(p: &crate::collect::proc::ProcRow, lang: crate::config::Lang) -> Vec<Field> {
    let mut out: Vec<Field> = Vec::new();
    let pid = p.pid;
    // explicit running/exited banner first — the whole point of the field
    out.push((
        "state",
        if p.alive {
            lang.tr("running").into()
        } else {
            lang.tr("exited").into()
        },
    ));
    if !p.alive {
        // the ledger keeps exited processes; say so instead of showing empty
        // live fields
        if let Some(cmd) = &p.cmdline {
            out.push(("cmd", cmd.join(" ")));
            out.push(("start", lang.tr("restarts_cmd").replace("{cmd}", &cmd[0])));
        }
        return out;
    }
    if let Ok(st) = std::fs::read_to_string(format!("/proc/{pid}/status")) {
        let pick = |key: &str, label: &'static str| -> Option<Field> {
            st.lines()
                .find(|l| l.starts_with(key))
                .and_then(|l| l.split_whitespace().nth(1))
                .map(|v| (label, v.to_string()))
        };
        if let Some(f) = pick("Uid:", "uid") {
            out.push(f);
        }
        if let Some(f) = pick("State:", "kstate") {
            out.push(f);
        }
        if let Some(f) = pick("Threads:", "threads") {
            out.push(f);
        }
        if let Some(f) = pick("VmRSS:", "rss") {
            out.push((f.0, format!("{} kB", f.1)));
        }
    }
    // exe/cwd:linux 读 /proc 符号链接;mac 走 libproc(proc_pidpath 即
    // exe 路径,cwd 经 PROC_PIDVNODEPATHINFO)
    #[cfg(target_os = "linux")]
    let exe = std::fs::read_link(format!("/proc/{pid}/exe"))
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "-".into());
    #[cfg(target_os = "macos")]
    let exe = crate::collect::macproc::cmdline(pid)
        .and_then(|v| v.into_iter().next())
        .unwrap_or_else(|| "-".into());
    out.push(("exe", exe));
    #[cfg(target_os = "linux")]
    let cwd = std::fs::read_link(format!("/proc/{pid}/cwd"))
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "-".into());
    #[cfg(target_os = "macos")]
    let cwd = crate::collect::macproc::proc_cwd(pid).unwrap_or_else(|| "-".into());
    out.push(("cwd", cwd));
    if let Ok(cmd) = std::fs::read(format!("/proc/{pid}/cmdline")) {
        let s: String = cmd
            .split(|&c| c == 0)
            .filter(|s| !s.is_empty())
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .collect::<Vec<_>>()
            .join(" ");
        if !s.is_empty() {
            out.push(("cmd", s));
        }
    }
    out
}

/// detail 弹窗的字段数据(渲染、游标计数、复制取值三处同源)
struct DetailData {
    groups: Vec<FieldGroup>,
    pid: Option<u32>,
    tracked: bool,
    gone: String,
    start_hint: bool,
}

fn detail_data(app: &App, box_idx: usize, id: u64) -> DetailData {
    let mut pid: Option<u32> = None;
    let mut tracked = false;
    let mut groups: Vec<FieldGroup> = Vec::new();
    let mut gone = String::new();
    let mut start_hint = false;
    match box_idx {
        BOX_PROCS => {
            if let Some(p) = app.proc.procs.iter().find(|p| p.pid as u64 == id) {
                pid = Some(p.pid);
                tracked = app.tracked.proc_slot(p.pid).is_some();
                // resident hint: s starts an exited process, restarts a live one
                start_hint = p.cmdline.is_some();
                let mut identity: Vec<(Field, ValStyle)> = vec![
                    (("name", p.name.clone()), ValStyle::Accent),
                    (("pid", p.pid.to_string()), ValStyle::Text),
                ];
                identity.extend(
                    proc_status_fields(p, app.lang)
                        .into_iter()
                        .map(|f| (f, ValStyle::Text)),
                );
                let traffic = vec![
                    (
                        ("net \u{3a3}\u{2193}", fmt_bytes(p.net_recv as f64)),
                        ValStyle::Dl,
                    ),
                    (
                        ("net \u{3a3}\u{2191}", fmt_bytes(p.net_sent as f64)),
                        ValStyle::Up,
                    ),
                    (("net \u{2193}/s", fmt_speed(p.net_rr)), ValStyle::Dl),
                    (("net \u{2191}/s", fmt_speed(p.net_sr)), ValStyle::Up),
                ];
                let disk = vec![
                    (
                        ("disk \u{3a3}R", fmt_bytes(p.disk_r as f64)),
                        ValStyle::Text,
                    ),
                    (
                        ("disk \u{3a3}W", fmt_bytes(p.disk_w as f64)),
                        ValStyle::Text,
                    ),
                    (("disk R/s", fmt_speed(p.disk_rr)), ValStyle::Text),
                    (("disk W/s", fmt_speed(p.disk_wr)), ValStyle::Text),
                ];
                groups = vec![("", identity), ("traffic", traffic), ("disk", disk)];
            } else {
                gone = format!("process {id} is gone");
            }
        }
        BOX_CONNS => {
            if let Some(c) = app.conn.conns.iter().find(|c| c.inode == id) {
                pid = c.pid;
                tracked = app.tracked.conn_slot(c.inode).is_some();
                let (state, state_st) = if c.is_listen() {
                    ("LISTEN", ValStyle::Hi)
                } else if c.is_tcp() {
                    ("ESTAB", ValStyle::Dl)
                } else {
                    ("UDP", ValStyle::Accent)
                };
                let proto = if c.is_tcp() { "tcp" } else { "udp" };
                let mut identity: Vec<(Field, ValStyle)> = vec![(
                    (
                        "name",
                        c.proc.clone().unwrap_or_else(|| app.conn.user_name(c.uid)),
                    ),
                    ValStyle::Accent,
                )];
                if let Some(p) = c.pid {
                    identity.push((("pid", p.to_string()), ValStyle::Text));
                }
                identity.push((("user", app.conn.user_name(c.uid)), ValStyle::Text));
                let conn = vec![
                    (("proto", proto.to_string()), ValStyle::Text),
                    (("state", state.to_string()), state_st),
                    (("local", sock_str(&c.local)), ValStyle::Text),
                    (("remote", sock_str(&c.remote)), ValStyle::Text),
                    (("inode", c.inode.to_string()), ValStyle::Text),
                ];
                let traffic = vec![
                    (
                        ("net \u{2193}", fmt_bytes(c.bytes_recv as f64)),
                        ValStyle::Dl,
                    ),
                    (
                        ("net \u{2191}", fmt_bytes(c.bytes_sent as f64)),
                        ValStyle::Up,
                    ),
                    (("net \u{2193}/s", fmt_speed(c.rate_recv)), ValStyle::Dl),
                    (("net \u{2191}/s", fmt_speed(c.rate_sent)), ValStyle::Up),
                ];
                groups = vec![("", identity), ("connection", conn), ("traffic", traffic)];
            } else {
                gone = format!("connection {id} is gone");
            }
        }
        _ => {}
    }
    DetailData {
        groups,
        pid,
        tracked,
        gone,
        start_hint,
    }
}

/// detail 弹窗的字段总数(游标上界)
pub fn detail_field_count(app: &App, box_idx: usize, id: u64) -> usize {
    detail_data(app, box_idx, id)
        .groups
        .iter()
        .map(|(_, fs)| fs.len())
        .sum()
}

/// 第 detail_sel 个字段的值(y 复制的内容)
pub fn detail_selected_value(app: &App, box_idx: usize, id: u64) -> Option<String> {
    let d = detail_data(app, box_idx, id);
    let mut flat = d
        .groups
        .iter()
        .flat_map(|(_, fs)| fs.iter())
        .map(|((_, v), _)| v.clone());
    flat.nth(app.detail_sel)
}

pub struct DetailLayout {
    pub popup: Rect,
    pub inner: Rect,
    pub fields_h: u16,
    pub hints_y: u16,
    pub hint_lines: Vec<String>,
    pub scroll: usize,
    pub sel: usize,
    /// 每个字段的行区间(起始行, 行数)——渲染高亮与鼠标命中共用
    pub field_rows: Vec<(usize, usize)>,
    /// 每个字段的完整值(复制用)
    pub values: Vec<String>,
}

/// detail 弹窗的几何与字段行映射——渲染与鼠标命中测试的单一事实源
fn detail_layout(app: &App, box_idx: usize, id: u64, area: Rect, sel: usize) -> DetailLayout {
    let width = 92.min(area.width);
    let DetailContent {
        field_rows,
        values,
        n_lines,
        hint_lines,
        hints_h,
    } = detail_content(app, box_idx, id, width);
    let h = (n_lines as u16 + 4).min(area.height);
    let popup = centered(area, width, h);
    let inner = Rect {
        x: popup.x + 1,
        y: popup.y + 1,
        width: popup.width.saturating_sub(2),
        height: popup.height.saturating_sub(2),
    };
    let hints_y = inner.bottom().saturating_sub(hints_h);
    let fields_h = hints_y.saturating_sub(inner.y);
    let sel = sel.min(field_rows.len().saturating_sub(1));
    let (s0, n0) = field_rows.get(sel).copied().unwrap_or((0, 1));
    let vis = fields_h as usize;
    let scroll = (s0 + n0).saturating_sub(vis);

    DetailLayout {
        popup,
        inner,
        fields_h,
        hints_y,
        hint_lines,
        scroll,
        sel,
        field_rows,
        values,
    }
}

/// 弹窗内容里与位置无关的部分
struct DetailContent {
    /// 每个字段的行区间(起始行, 行数)
    field_rows: Vec<(usize, usize)>,
    /// 每个字段的完整值(复制用)
    values: Vec<String>,
    /// 总行数(含 gone 行/组标题/空行)
    n_lines: usize,
    hint_lines: Vec<String>,
    hints_h: u16,
}

/// 渲染与鼠标命中测试共用,保证两视图永不漂移
fn detail_content(app: &App, box_idx: usize, id: u64, width: u16) -> DetailContent {
    let DetailData {
        groups,
        pid,
        tracked,
        gone,
        start_hint,
    } = detail_data(app, box_idx, id);
    let track_hint = if tracked {
        app.lang.tr("hint_track_on")
    } else {
        app.lang.tr("hint_track_off")
    };
    let mut hints = vec![
        track_hint.to_string(),
        app.lang.tr("hint_copy").to_string(),
        app.lang.tr("hint_close").to_string(),
    ];
    if let Some(p) = pid {
        hints.insert(2, app.lang.tr("hint_kill").replace("{p}", &p.to_string()));
    }
    if start_hint {
        hints.insert(2, app.lang.tr("hint_start").to_string());
    }

    let lbl_w = groups
        .iter()
        .flat_map(|(_, fs)| fs.iter())
        .map(|((l, _), _)| disp_w(l))
        .max()
        .unwrap_or(0);
    let inner_w = width.saturating_sub(2) as usize;
    let chunk_w = inner_w.saturating_sub(lbl_w + 4).max(10);
    let n_lines: usize = gone.len().min(1)
        + groups
            .iter()
            .map(|(title, fs)| {
                let head = if title.is_empty() { 0 } else { 2 };
                head + fs
                    .iter()
                    .map(|((_, v), _)| wrap_chunks(v, chunk_w).len())
                    .sum::<usize>()
            })
            .sum::<usize>();

    let mut lines = 0usize;
    let mut field_rows: Vec<(usize, usize)> = Vec::new();
    let mut values: Vec<String> = Vec::new();
    if !gone.is_empty() {
        lines += 1;
    }
    for (title, fields) in &groups {
        if !title.is_empty() {
            lines += 2; // 空行 + 组标题(不计入字段游标)
        }
        for ((_, value), _) in fields {
            let start = lines;
            let count = wrap_chunks(value, chunk_w).len();
            lines += count;
            field_rows.push((start, count));
            values.push(value.clone());
        }
    }
    let hint_lines = pack_rows(&hints, width.saturating_sub(2) as usize);
    let hints_h = hint_lines.len() as u16;
    DetailContent {
        field_rows,
        values,
        n_lines,
        hint_lines,
        hints_h,
    }
}

/// 鼠标命中:弹窗某行对应什么。几何直接读渲染时缓存的弹窗矩形(与屏幕上
/// 真实位置零漂移),col 不参与(字段行整行可点)。
pub enum DetailHit {
    /// 字段行:(游标索引, 字段完整值)
    Field(usize, String),
    /// 弹窗内的非字段行(组标题/空行)
    Heading,
    /// 弹窗外
    Outside,
}

pub fn detail_hit_test(app: &App, box_idx: usize, id: u64, row: u16) -> DetailHit {
    if !DETAIL_VALID.load(Relaxed) {
        return DetailHit::Outside; // 打开 detail 后还没渲染过一帧
    }
    let y = DETAIL_Y.load(Relaxed);
    let h = DETAIL_H.load(Relaxed);
    if row < y || row >= y + h {
        return DetailHit::Outside; // 弹窗外
    }
    let inner_y = y + 1; // 上边框
    let fields_h = DETAIL_FIELDS_H.load(Relaxed);
    if row < inner_y || row >= inner_y + fields_h {
        return DetailHit::Heading; // 边框/空行/hint 行
    }
    let all_y = (row - inner_y) as usize + DETAIL_SCROLL.load(Relaxed) as usize;
    let width = DETAIL_W.load(Relaxed);
    let DetailContent {
        field_rows, values, ..
    } = detail_content(app, box_idx, id, width);
    for (i, (start, count)) in field_rows.iter().enumerate() {
        if all_y >= *start && all_y < start + count {
            return DetailHit::Field(i, values[i].clone());
        }
    }
    DetailHit::Heading
}

// detail 弹窗实际渲染几何的缓存:鼠标命中测试直接用它,避免用全屏尺寸
// 复算 centered 时与主区(少 footer 一行)产生半行级取整偏差
static DETAIL_Y: AtomicU16 = AtomicU16::new(0);
static DETAIL_H: AtomicU16 = AtomicU16::new(0);
static DETAIL_W: AtomicU16 = AtomicU16::new(0);
static DETAIL_FIELDS_H: AtomicU16 = AtomicU16::new(0);
static DETAIL_SCROLL: AtomicU16 = AtomicU16::new(0);
static DETAIL_VALID: AtomicBool = AtomicBool::new(false);

fn draw_detail_popup(f: &mut Frame, area: Rect, app: &App, box_idx: usize, id: u64) {
    let DetailData { groups, gone, .. } = detail_data(app, box_idx, id);
    let l = detail_layout(app, box_idx, id, area, app.detail_sel);
    // 缓存实际渲染几何:鼠标命中测试(detail_hit_test)据此定位,与屏幕零漂移
    DETAIL_Y.store(l.popup.y, Relaxed);
    DETAIL_H.store(l.popup.height, Relaxed);
    DETAIL_W.store(l.popup.width, Relaxed);
    DETAIL_FIELDS_H.store(l.fields_h, Relaxed);
    DETAIL_SCROLL.store(l.scroll as u16, Relaxed);
    DETAIL_VALID.store(true, Relaxed);

    f.render_widget(Clear, l.popup);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .style(Style::new().bg(bg().unwrap_or(Color::Reset)))
        .border_style(Style::new().fg(hi()))
        .title(title_line(0, "detail", '\0', vec![], hi(), true));
    let inner = block.inner(l.popup);
    f.render_widget(block, l.popup);

    // rows re-rendered with the shared layout's metrics (label width + wrap)
    let lbl_w = groups
        .iter()
        .flat_map(|(_, fs)| fs.iter())
        .map(|((l, _), _)| disp_w(l))
        .max()
        .unwrap_or(0);
    let chunk_w = (l.popup.width.saturating_sub(2) as usize)
        .saturating_sub(lbl_w + 4)
        .max(10);
    let mut all: Vec<Line> = Vec::new();
    if !gone.is_empty() {
        all.push(Line::from(Span::styled(format!(" {gone}"), dim())));
    }
    for (title, fields) in &groups {
        if !title.is_empty() {
            all.push(Line::from(""));
            all.push(Line::from(Span::styled(
                format!(" \u{2500}\u{2500} {title} \u{2500}\u{2500}"),
                bold(hi()),
            )));
        }
        for ((label, value), st) in fields {
            for (i, chunk) in wrap_chunks(value, chunk_w).into_iter().enumerate() {
                let indent = if i == 0 {
                    format!(" {:<lbl_w$}  ", label)
                } else {
                    format!(" {:<lbl_w$}  ", "")
                };
                let vspan = if *st == ValStyle::Accent {
                    Span::styled(chunk, bold(accent()))
                } else {
                    Span::styled(chunk, val_style(*st))
                };
                all.push(Line::from(vec![Span::styled(indent, dim()), vspan]));
            }
        }
    }
    let hl_bg = download().at(0.12);
    let lines_with_cursor: Vec<Line> = all
        .into_iter()
        .enumerate()
        .map(|(li, line)| {
            let in_field = l
                .field_rows
                .get(l.sel)
                .is_some_and(|(start, count)| li >= *start && li < start + count);
            if !in_field {
                return line;
            }
            let bar = if li == l.field_rows[l.sel].0 {
                "▸"
            } else {
                " "
            };
            let mut spans = vec![Span::styled(bar, Style::new().fg(accent()).bg(hl_bg))];
            spans.extend(line.spans.into_iter().map(|mut sp| {
                sp.style = sp.style.bg(hl_bg);
                sp
            }));
            Line::from(spans)
        })
        .collect();
    let skipped: Vec<Line> = lines_with_cursor
        .iter()
        .skip(l.scroll)
        .take(l.fields_h as usize)
        .cloned()
        .collect();
    f.render_widget(
        Paragraph::new(skipped),
        Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: l.fields_h,
        },
    );
    f.render_widget(
        Paragraph::new(
            l.hint_lines
                .into_iter()
                .map(|x| Line::from(Span::styled(x, bold(hi()))))
                .collect::<Vec<_>>(),
        ),
        Rect {
            x: inner.x,
            y: l.hints_y,
            width: inner.width,
            height: l.popup.bottom().saturating_sub(1).saturating_sub(l.hints_y),
        },
    );
}

/// greedy line packing: join `items` with three spaces, breaking to a new
/// row only between items when the width would overflow
fn pack_rows(items: &[String], w: usize) -> Vec<String> {
    let mut rows: Vec<String> = Vec::new();
    let mut cur = String::new();
    for it in items {
        let sep = if cur.is_empty() { "" } else { "   " };
        if !cur.is_empty() && disp_w(&cur) + 3 + disp_w(it) > w {
            rows.push(std::mem::take(&mut cur));
        } else {
            cur.push_str(sep);
        }
        cur.push_str(it);
    }
    if !cur.is_empty() {
        rows.push(cur);
    }
    rows.into_iter().map(|r| format!(" {r}")).collect()
}

/// split `s` into display chunks of at most `w` terminal columns
/// (CJK 双宽按 unicode-width 计,中文命令行/路径折行不再超宽)
fn wrap_chunks(s: &str, w: usize) -> Vec<String> {
    if w == 0 {
        return vec![s.to_string()];
    }
    if disp_w(s) <= w {
        return vec![s.to_string()];
    }
    use unicode_width::UnicodeWidthStr;
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut cur_w = 0usize;
    for c in s.chars() {
        let cw = UnicodeWidthStr::width(c.to_string().as_str());
        if cur_w + cw > w && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
            cur_w = 0;
        }
        cur.push(c);
        cur_w += cw;
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn draw_confirm_popup(f: &mut Frame, area: Rect, app: &App, pid: u32, restart: bool) {
    let name = app
        .proc
        .procs
        .iter()
        .find(|p| p.pid == pid)
        .map(|p| p.name.clone())
        .unwrap_or_else(|| "?".into());
    let (w, h) = (52.min(area.width), 7.min(area.height));
    let popup = centered(area, w, h);
    f.render_widget(Clear, popup);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .style(Style::new().bg(bg().unwrap_or(Color::Reset)))
        .border_style(Style::new().fg(alert_end()))
        .title(title_line(
            0,
            if restart { "restart" } else { "kill" },
            '\0',
            vec![],
            alert_end(),
            true,
        ));
    let inner = block.inner(popup);
    f.render_widget(block, popup);
    let tpl = app.lang.tr(if restart {
        "confirm_restart"
    } else {
        "confirm_kill"
    });
    let question = tpl
        .replace("{name}", &name)
        .replace("{pid}", &pid.to_string());
    let lines = vec![
        Line::from(question),
        Line::from(""),
        Line::from(vec![
            Span::styled(" [y] ", bold(hi())),
            Span::styled(format!("{}    ", app.lang.tr("yes")), text()),
            Span::styled("[n] ", bold(hi())),
            Span::styled(app.lang.tr("no").to_string(), text()),
        ]),
    ];
    f.render_widget(Paragraph::new(lines), inner);
}

/// (key, description) rows for the help panel
fn help_rows(lang: Lang) -> Vec<(String, String)> {
    let t = |k: &str| lang.tr(k);
    vec![
        (
            "1-6".into(),
            format!("{} / {}", t("boxes"), t("hint_close_short")),
        ),
        ("Tab".into(), t("layout").to_string()),
        ("enter".into(), t("detail_desc").to_string()),
        ("p / c / l".into(), t("focus_desc").to_string()),
        ("r".into(), t("order_desc").to_string()),
        ("d".into(), t("expand_desc").to_string()),
        ("f".into(), t("filter_desc").to_string()),
        ("o".into(), t("conn_order_desc").to_string()),
        ("e".into(), t("lan_order_desc").to_string()),
        ("n / i".into(), t("net_io_desc").to_string()),
        ("space".into(), t("track_desc").to_string()),
        ("k".into(), t("kill_desc").to_string()),
        ("L".into(), t("log_desc").to_string()),
        ("s".into(), t("sweep_desc").to_string()),
        ("+ / -".into(), t("refresh_desc").to_string()),
        (
            "\u{2191}\u{2193} / wheel".into(),
            t("sel_scroll_desc").to_string(),
        ),
        ("z".into(), t("zoom_desc").to_string()),
        ("q / esc / m".into(), t("quit_menu_desc").to_string()),
    ]
}

/// merged help + about panel
fn draw_help_popup(f: &mut Frame, area: Rect, app: &App) {
    let rows = help_rows(app.lang);
    // 双列键位把总高从 ~33 行压到 ~23 行:旧版 h 估算 (+8) 低估头部开销
    // 且撞终端高度封顶后无滚动,底部(数据口径段/press any)固定被裁。
    // 宽度按内容精确计算(显示宽度,CJK 双宽):双列放不下自动回退单列
    let kw = rows.iter().map(|(k, _)| disp_w(k)).max().unwrap_or(0);
    let desc_w = |rs: &[(String, String)]| rs.iter().map(|(_, d)| disp_w(d)).max().unwrap_or(0);
    let dw_all = desc_w(&rows);
    let col_w = 3 + kw + 2 + dw_all + 3;
    let w_two = (2 + 2 * col_w) as u16;
    let w_one = (2 + 3 + kw + 2 + dw_all + 4) as u16;
    let two_col = area.width >= w_two;
    let half = rows.len().div_ceil(2);
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(
            format!(" ltop v{}", env!("CARGO_PKG_VERSION")),
            bold(accent()),
        ),
        Span::styled("  by uMaj35ty", dim()),
    ]));
    lines.push(Line::from(Span::styled(
        app.lang.tr("about_line").to_string(),
        dim(),
    )));
    lines.push(Line::from(Span::styled(
        format!(
            " theme: {} \u{b7} {}",
            theme_name(),
            app.lang
                .tr(if app.udp.fresh { "udp_on" } else { "udp_off" })
        ),
        dim(),
    )));
    lines.push(Line::from(Span::styled(
        format!(" {}", app.lang.tr("caliber_head")),
        bold(hi()),
    )));
    for k in ["caliber_procs", "caliber_conns", "caliber_keys"] {
        lines.push(Line::from(Span::styled(app.lang.tr(k), dim())));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        app.lang.tr("keys_header"),
        bold(hi()),
    )));
    // 头部(数据口径段)通常是面板里最宽的行:弹窗宽度必须把它算进去,
    // 否则中文下整列被裁(help 面板"信息不全"的根因之一)
    let head_max = lines
        .iter()
        .map(|l| l.spans.iter().map(|s| disp_w(&s.content)).sum::<usize>())
        .max()
        .unwrap_or(0) as u16;
    let col_spans = |(k, d): &(String, String), dw: usize, kw: usize| -> Vec<Span<'static>> {
        let kpad = " ".repeat(kw.saturating_sub(disp_w(k)));
        let dpad = " ".repeat(dw.saturating_sub(disp_w(d)));
        vec![
            Span::styled(format!(" {k}{kpad}  "), bold(hi())),
            Span::styled(format!("{d}{dpad}   "), text()),
        ]
    };
    if two_col {
        let (left, right) = rows.split_at(half);
        let dw_l = desc_w(left);
        let dw_r = desc_w(right);
        for i in 0..half {
            let mut spans = match left.get(i) {
                Some(e) => col_spans(e, dw_l, kw),
                None => vec![Span::raw(" ".repeat(6 + kw + dw_l))],
            };
            spans.push(Span::raw("  "));
            if let Some(e) = right.get(i) {
                spans.extend(col_spans(e, dw_r, kw));
            }
            lines.push(Line::from(spans));
        }
    } else {
        let dw = desc_w(&rows);
        for e in &rows {
            lines.push(Line::from(col_spans(e, dw, kw)));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(app.lang.tr("press_any"), dim())));
    let w = if two_col {
        w_two.max(head_max + 4)
    } else {
        w_one.max(head_max + 4).min(area.width)
    };
    let h = (lines.len() as u16 + 2).min(area.height);
    let popup = centered(area, w, h);
    f.render_widget(Clear, popup);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .style(Style::new().bg(bg().unwrap_or(Color::Reset)))
        .border_style(Style::new().fg(hi()))
        .title(title_line(0, "help", '\0', vec![], hi(), false));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    f.render_widget(Paragraph::new(lines), inner);
}

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    }
}

fn draw_theme_popup(f: &mut Frame, area: Rect, sel: usize, scroll: usize) {
    let count = builtin_count() + 1; // + omarchy
                                     // popup hugs its content: at most THEME_VISIBLE rows, never a half-empty box
    let visible = THEME_VISIBLE.min(area.height.saturating_sub(4).max(1) as usize);
    let height = visible as u16 + 2;
    let width = 30.min(area.width);
    let popup = Rect {
        x: area.x + 1,
        y: area.bottom().saturating_sub(height),
        width,
        height,
    };

    let current = theme_name();
    // clamp the window so the selection stays visible even on short terminals
    let mut lo = scroll.min(count.saturating_sub(visible));
    if sel < lo {
        lo = sel;
    }
    if sel >= lo + visible {
        lo = sel + 1 - visible;
    }
    let mut lines: Vec<Line> = Vec::new();
    for i in lo..count.min(lo + visible) {
        let name = if i == 0 {
            "omarchy (current)".to_string()
        } else {
            builtin_name(i - 1).to_string()
        };
        let is_current = current
            == if i == 0 {
                "omarchy"
            } else {
                builtin_name(i - 1)
            };
        let style = if i == sel {
            Style::new()
                .fg(bg().unwrap_or(Color::Reset))
                .bg(hi())
                .add_modifier(Modifier::BOLD)
        } else if is_current {
            Style::new().fg(hi())
        } else {
            text()
        };
        let marker = if is_current { "● " } else { "  " };
        lines.push(Line::from(Span::styled(format!("{marker}{name}"), style)));
    }

    f.render_widget(Clear, popup);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .style(Style::new().bg(bg().unwrap_or(Color::Reset)))
        .border_style(Style::new().fg(hi()))
        .title(title_line(0, "theme", '\0', vec![], hi(), false));
    let inner = block.inner(popup);
    f.render_widget(block, popup);
    f.render_widget(Paragraph::new(lines), inner);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::BOX_COUNT;
    use ratatui::layout::Constraint;

    /// invisible-selection regression, dark theme: a download gradient
    /// whose start ≈ the background (omarchy-void style) must still yield
    /// a wash that separates from the row bg and reads under span colors
    #[test]
    fn selection_wash_clears_floors_on_dark_and_light() {
        let dark_bg = Color::Rgb(0x28, 0x2a, 0x36);
        let dark_text = Color::Rgb(0xf8, 0xf8, 0xf2);
        // hue indistinguishable from the bg (the old at(0.12) bug)
        let camo = Color::Rgb(0x2b, 0x2d, 0x39);
        let wash = selection_bg_over(camo, Some(dark_bg), dark_text);
        assert!(
            crate::theme::contrast_ratio(wash, dark_bg) >= 1.3,
            "dark: not visible"
        );
        assert!(
            crate::theme::contrast_ratio(wash, dark_text) >= 2.2,
            "dark: span colors unreadable"
        );

        // light theme (solarized-like): dark text, light bg, mid hue
        let light_bg = Color::Rgb(0xee, 0xe8, 0xd5);
        let light_text = Color::Rgb(0x07, 0x36, 0x42);
        let wash = selection_bg_over(Color::Rgb(0x42, 0x8c, 0x4a), Some(light_bg), light_text);
        assert!(
            crate::theme::contrast_ratio(wash, light_bg) >= 1.3,
            "light: not visible"
        );
        assert!(
            crate::theme::contrast_ratio(wash, light_text) >= 2.2,
            "light: span colors unreadable"
        );

        // no measurable bg (transparent): hue passes through
        assert_eq!(selection_bg_over(camo, None, dark_text), camo);
    }

    #[test]
    fn readable_bg_holds_the_text_floor() {
        let text = Color::Rgb(0xf8, 0xf8, 0xf2);
        let base = Color::Rgb(0x28, 0x2a, 0x36);
        // bright gradient end (dracula yellow): white text reads 1.1:1 on it
        let yellow = Color::Rgb(0xf1, 0xfa, 0x8c);
        assert!(crate::theme::contrast_ratio(yellow, text) < 3.0);
        let fixed = readable_bg_over(yellow, Some(base), text);
        assert!(crate::theme::contrast_ratio(fixed, text) >= 3.0);
        // already-compliant colors pass through untouched
        assert_eq!(readable_bg_over(base, Some(base), text), base);
        // transparent bg: no anchor to blend toward
        assert_eq!(readable_bg_over(yellow, None, text), yellow);
    }

    #[test]
    fn readable_fg_picks_the_better_theme_anchor() {
        let text = Color::Rgb(0xf8, 0xf8, 0xf2);
        let dark_anchor = Color::Rgb(0x28, 0x2a, 0x36);
        // bright bg: dark anchor reads far better than white text
        let yellow = Color::Rgb(0xf1, 0xfa, 0x8c);
        assert_eq!(
            readable_fg_between(yellow, text, Some(dark_anchor)),
            dark_anchor
        );
        // near-black bg: text wins
        assert_eq!(
            readable_fg_between(dark_anchor, text, Some(dark_anchor)),
            text
        );
        // no anchor: text
        assert_eq!(readable_fg_between(yellow, text, None), text);
    }

    fn lens(cs: &[Constraint]) -> Vec<Option<u16>> {
        cs.iter()
            .map(|c| match c {
                Constraint::Length(n) => Some(*n),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn fit_columns_uses_fixed_vocabulary_widths() {
        // LAN: bases [IP16, MAC17, flex, IF8]; the hostname column takes the
        // room up to its cap, then the small remaining slack is spread
        let cs = fit_columns(&[16, 17, 0, 8], 2, 8, 30, 80, 1);
        assert_eq!(lens(&cs), vec![Some(18), Some(19), Some(31), Some(9)]);
        // identical call -> identical layout (widths never sample live data)
        let again = fit_columns(&[16, 17, 0, 8], 2, 8, 30, 80, 1);
        assert_eq!(lens(&cs), lens(&again));
        // cramped: the fixed columns stay intact and the right edge clips
        let narrow = fit_columns(&[16, 17, 0, 8], 2, 8, 30, 40, 1);
        assert_eq!(lens(&narrow), vec![Some(16), Some(17), Some(8), Some(8)]);
    }

    #[test]
    fn process_table_width_is_stable_and_capped() {
        let base = [0, 7, 7, 7, 9, 9, 7, 7];
        // 73 wide: no slack, name gets exactly the remainder
        assert_eq!(
            lens(&fit_columns(&base, 0, 8, 24, 73, 1)),
            vec![
                Some(13),
                Some(7),
                Some(7),
                Some(7),
                Some(9),
                Some(9),
                Some(7),
                Some(7)
            ]
        );
        // very wide: the name column is capped at 24, growth is capped too,
        // and the rest stays as trailing blank
        let wide = lens(&fit_columns(&base, 0, 8, 24, 160, 1));
        assert_eq!(wide[0], Some(36));
        assert_eq!(wide.iter().map(|w| w.unwrap()).sum::<u16>(), 112);
    }

    #[test]
    fn rendered_table_keeps_one_space_between_columns() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let headers = [
            "NAME",
            "PID",
            "Σ NET↓",
            "Σ NET↑",
            "↓/s",
            "↑/s",
            "DISK ΣR",
            "DISK ΣW",
        ];
        let data = [vec![
            "zcode-host-local".to_string(),
            "546829".to_string(),
            "0B".to_string(),
            "0B".to_string(),
            "0B/s".to_string(),
            "0B/s".to_string(),
            "20.0KiB".to_string(),
            "0B".to_string(),
        ]];
        let cols = fit_columns(&[0, 7, 7, 7, 9, 9, 7, 7], 0, 8, 24, 73, 1);
        let mut term = Terminal::new(TestBackend::new(73, 4)).unwrap();
        let rows: Vec<Row> = data
            .iter()
            .map(|r| Row::new(r.iter().map(|c| Cell::from(c.clone())).collect::<Vec<_>>()))
            .collect();
        let header = Row::new(headers);
        term.draw(|f| {
            let t = Table::new(rows, cols).header(header).column_spacing(1);
            f.render_widget(t, f.area());
        })
        .unwrap();
        let buf = term.backend().buffer();
        let line: String = (0..73)
            .map(|x| buf[(x, 1)].symbol().to_string())
            .collect::<String>();
        // columns are separated by blanks (never glued), and spare width is
        // spread evenly across them rather than dumped into one column
        let fields: Vec<&str> = line.split_whitespace().collect();
        // the name is clipped to its fixed 13 columns; every numeric column
        // stays intact because its width came from the vocabulary, not from
        // this particular row
        assert_eq!(
            fields,
            vec![
                "zcode-host-lo",
                "546829",
                "0B",
                "0B",
                "0B/s",
                "0B/s",
                "20.0KiB",
                "0B"
            ]
        );
    }

    /// Regression: a real 249x67 terminal panicked with "index outside of
    /// buffer … index is (126, 67)" on the first frame — some box drew one
    /// row past its area. Every draw call must stay inside the buffer.
    #[test]
    fn full_frame_stays_inside_the_terminal() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut app = crate::app::App::new(None);
        app.update();
        std::thread::sleep(std::time::Duration::from_millis(700));
        app.update();
        for (w, h) in [(249u16, 67u16), (80, 24), (120, 30), (200, 50), (60, 20)] {
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            term.draw(|f| draw(f, &app))
                .unwrap_or_else(|e| panic!("{w}x{h}: {e}"));
        }
    }

    /// Regression: selecting the last visible row of the bottom-most list box
    /// drew the 2-line overlay one row past the buffer ("index … (126, 67)" on
    /// a 249x67 terminal). The overlay must be clipped to the box interior.
    #[test]
    fn selected_row_overlay_never_leaves_the_buffer() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut app = crate::app::App::new(None);
        app.update();
        // PROCS bottom-most: hide every other table box. The live ledger only
        // lists processes with I/O in the sampling window, so inject rows.
        for i in [BOX_LAN, BOX_CONNS, BOX_DISKS] {
            app.visible[i] = false;
        }
        app.focus[BOX_PROCS] = true;
        for k in 0..40u32 {
            app.proc.procs.push(crate::collect::proc::ProcRow {
                pid: 1000 + k,
                name: format!("proc{k}"),
                net_recv: 10,
                net_sent: 5,
                net_rr: 1.0,
                net_sr: 0.5,
                udp_recv: 0,
                udp_sent: 0,
                udp_rr: 0.0,
                udp_sr: 0.0,
                disk_r: 0,
                disk_w: 0,
                disk_rr: 0.0,
                disk_wr: 0.0,
                uptime_s: None,
                alive: true,
                cmdline: None,
            });
        }
        let n = crate::ui::procs_display(&app).len();
        // sel sweeps every row plus two past the end: the boundary cases are
        // the last visible row and the first invisible one
        for (w, h) in [(249u16, 67u16), (80, 24), (100, 30), (60, 18)] {
            for sel in 0..n + 2 {
                app.sel[BOX_PROCS] = (sel < n).then_some(sel);
                let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
                term.draw(|f| draw(f, &app))
                    .unwrap_or_else(|e| panic!("{w}x{h} sel {sel}: {e}"));
            }
        }
    }

    /// Regression: the detail popup shows the explicit running/exited status —
    /// "running" for a live row, "exited" + captured cmdline + start hint for a
    /// dead one — and must not try to read /proc for a dead pid.
    #[test]
    fn detail_popup_renders_exited_process() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut app = crate::app::App::new(None);
        app.update();
        let mut row = crate::collect::proc::ProcRow {
            pid: 4242,
            name: "deadproc".into(),
            net_recv: 100,
            net_sent: 50,
            net_rr: 0.0,
            net_sr: 0.0,
            udp_recv: 0,
            udp_sent: 0,
            udp_rr: 0.0,
            udp_sr: 0.0,
            disk_r: 0,
            disk_w: 0,
            disk_rr: 0.0,
            disk_wr: 0.0,
            uptime_s: None,
            alive: false,
            cmdline: Some(vec!["/usr/bin/deadproc".into(), "--flag".into()]),
        };
        app.proc.procs.push(row.clone());
        let buf_text = |term: &Terminal<TestBackend>| {
            let buf = term.backend().buffer();
            (0..buf.area().height)
                .map(|y| {
                    (0..buf.area().width)
                        .map(|x| buf[(x, y)].symbol().to_string())
                        .collect::<String>()
                })
                .collect::<String>()
        };
        app.menu = crate::app::MenuState::Detail(BOX_PROCS, 4242);
        let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();
        let text = buf_text(&term);
        assert!(text.contains("exited"), "status shown");
        assert!(text.contains("/usr/bin/deadproc"), "captured cmdline shown");
        assert!(text.contains("[s] start/restart"), "start hint shown");

        // the same row while alive reads live /proc fields and says running
        row.alive = true;
        row.pid = std::process::id();
        app.proc.procs.push(row);
        app.menu = crate::app::MenuState::Detail(BOX_PROCS, u64::from(std::process::id()));
        let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();
        let text = buf_text(&term);
        assert!(text.contains("running"), "alive row shows running");
        assert!(!text.contains("exited"), "no exited banner for a live row");
        // the s hint is resident for any process with a captured cmdline
        assert!(text.contains("[s] start/restart"), "resident start hint");
    }

    /// Long command lines wrap onto continuation lines, and the key-binding
    /// hint row stays visible (pinned to the popup's bottom line) even when
    /// the fields overflow a small terminal.
    #[test]
    fn detail_panel_wraps_cmd_and_pins_the_hint_row() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut app = crate::app::App::new(None);
        app.update();
        let tail = "and-a-long-tail-marker".to_string();
        let long_cmd = format!(
            "/usr/bin/deadproc --very-long-option={} {}",
            "x".repeat(120),
            tail
        );
        app.proc.procs.push(crate::collect::proc::ProcRow {
            pid: 4242,
            name: "deadproc".into(),
            net_recv: 100,
            net_sent: 50,
            net_rr: 0.0,
            net_sr: 0.0,
            udp_recv: 0,
            udp_sent: 0,
            udp_rr: 0.0,
            udp_sr: 0.0,
            disk_r: 0,
            disk_w: 0,
            disk_rr: 0.0,
            disk_wr: 0.0,
            uptime_s: None,
            alive: false,
            cmdline: Some(vec![long_cmd]),
        });
        app.menu = crate::app::MenuState::Detail(BOX_PROCS, 4242);
        let buf_text = |term: &Terminal<TestBackend>| {
            let buf = term.backend().buffer();
            (0..buf.area().height)
                .map(|y| {
                    (0..buf.area().width)
                        .map(|x| buf[(x, y)].symbol().to_string())
                        .collect::<String>()
                })
                .collect::<String>()
        };
        // roomy terminal: the whole command is readable across wrapped lines
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();
        let text = buf_text(&term);
        assert!(text.contains(&tail), "wrapped cmd tail is rendered");
        assert!(text.contains("[esc] close"), "hints complete");

        // tiny terminal: fields overflow but the hint row must survive
        let mut term = Terminal::new(TestBackend::new(60, 10)).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();
        let text = buf_text(&term);
        assert!(text.contains("[esc] close"), "hints pinned to the bottom");
        assert!(text.contains("[s] start/restart"), "s hint visible");
        assert!(text.contains("[k] kill pid 4242"), "k hint visible");
    }

    /// Regression: with a tracked (pinned-to-top) process, the selection
    /// overlay must follow the *display* order. It used to index the raw
    /// ledger, showing the detail of whichever process sat at that position
    /// before pinning (enter's detail popup was right, the overlay wrong).
    #[test]
    fn procs_overlay_follows_the_pinned_order() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut app = crate::app::App::new(None);
        app.update();
        for (pid, name) in [(100u32, "aaa"), (200u32, "bbb")] {
            app.proc.procs.push(crate::collect::proc::ProcRow {
                pid,
                name: name.into(),
                net_recv: 10,
                net_sent: 0,
                net_rr: 0.0,
                net_sr: 0.0,
                udp_recv: 0,
                udp_sent: 0,
                udp_rr: 0.0,
                udp_sr: 0.0,
                disk_r: 0,
                disk_w: 0,
                disk_rr: 0.0,
                disk_wr: 0.0,
                uptime_s: None,
                alive: true,
                cmdline: None,
            });
        }
        app.tracked.toggle_proc(200); // bbb pinned → display [bbb, aaa]
                                      // PROCS alone in the table column: parallel tests share the global
                                      // height allocator, so a stacked layout could squeeze this box until
                                      // the selected row falls outside the viewport and renders no overlay
        for i in [BOX_LAN, BOX_CONNS, BOX_DISKS] {
            app.visible[i] = false;
        }
        app.focus[BOX_PROCS] = true;
        app.sel[BOX_PROCS] = Some(1); // display row 1 = aaa (pid 100)
        let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();
        let buf = term.backend().buffer();
        let text: String = (0..buf.area().height)
            .map(|y| {
                (0..buf.area().width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect();
        assert!(
            text.contains("(pid 100"),
            "overlay shows the selected row's process"
        );
        assert!(
            !text.contains("(pid 200"),
            "overlay must not show the pre-pin occupant"
        );
    }

    /// The selection follows the entity it names (pid / inode) through
    /// re-sorts; an entity that left the list clears the selection instead of
    /// silently moving onto whatever took its list position.
    #[test]
    fn selection_follows_the_entity_through_resorts() {
        let mut app = crate::app::App::new(None);
        app.update();
        for (pid, name, recv) in [(100u32, "aaa", 10u64), (200u32, "bbb", 1000u64)] {
            app.proc.procs.push(crate::collect::proc::ProcRow {
                pid,
                name: name.into(),
                net_recv: recv,
                net_sent: 0,
                net_rr: 0.0,
                net_sr: 0.0,
                udp_recv: 0,
                udp_sent: 0,
                udp_rr: 0.0,
                udp_sr: 0.0,
                disk_r: 0,
                disk_w: 0,
                disk_rr: 0.0,
                disk_wr: 0.0,
                uptime_s: None,
                alive: true,
                cmdline: None,
            });
        }
        app.sel[BOX_PROCS] = Some(0); // aaa
        let anchor = selected_id(&app, BOX_PROCS);
        assert_eq!(anchor, Some(100));
        // a re-sort puts bbb first; the selection must stay on aaa
        app.proc.procs.swap(0, 1);
        reselect(&mut app, BOX_PROCS, anchor.unwrap());
        assert_eq!(app.sel[BOX_PROCS], Some(1));

        // the entity leaving the list clears the selection
        app.proc.procs.retain(|p| p.pid != 100);
        reselect(&mut app, BOX_PROCS, anchor.unwrap());
        assert_eq!(app.sel[BOX_PROCS], None);

        // conns follow their inode the same way
        let mk = |inode: u64| crate::collect::conn::Conn {
            proto: 6,
            local: ("127.0.0.1".parse().unwrap(), 1000 + inode as u16),
            remote: ("127.0.0.1".parse().unwrap(), 80),
            state: 1,
            uid: 0,
            inode,
            bytes_recv: 0,
            bytes_sent: 0,
            rate_recv: 0.0,
            rate_sent: 0.0,
            pid: None,
            proc: None,
            dead: false,
            first_seen: None,
            dead_at: None,
        };
        app.conn.sort = crate::collect::conn::ConnSort::Name; // 平面行的 inode 跟随
        app.conn.conns = vec![mk(11), mk(22)];
        app.sel[BOX_CONNS] = Some(1); // inode 22
        let anchor = selected_id(&app, BOX_CONNS);
        assert_eq!(anchor, Some(22));
        app.conn.conns.swap(0, 1); // inode 22 moved to position 0
        reselect(&mut app, BOX_CONNS, anchor.unwrap());
        assert_eq!(app.sel[BOX_CONNS], Some(0), "selection follows the inode");
    }

    /// classic 右列中线分组:CONNS 底边与左列 NET 图表底边对齐,
    /// LAN+CONNS 恰好占上半,PROCS 从中线开始。
    /// 断言走 buffer 文本而非 BOX_AREAS——后者是全局态,并行测试会互踩。
    /// 回归:CONNS 超百行时滚到底曾"只剩 1 行且卡死"——off 封顶应是
    /// 最后一页而非最后一行,且渲染裁剪 take(visible) 防止越界半行
    #[test]
    fn conns_bottom_scroll_shows_full_last_page() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut app = crate::app::App::new(None);
        for i in 0..BOX_COUNT {
            app.visible[i] = i == BOX_CONNS;
        }
        let n = 200usize;
        for k in 0..n {
            app.conn.conns.push(crate::collect::conn::Conn {
                proto: crate::collect::conn::IPPROTO_TCP,
                local: ("127.0.0.1".parse().unwrap(), 1000 + k as u16),
                remote: ("127.0.0.1".parse().unwrap(), 443),
                state: 1,
                uid: 0,
                inode: k as u64,
                bytes_recv: 0,
                bytes_sent: 0,
                rate_recv: 0.0,
                rate_sent: 0.0,
                pid: Some(1),
                proc: Some(format!("c{k:04}")),
                dead: false,
                first_seen: None,
                dead_at: None,
            });
        }
        // 滚动值严重越界(旧版会 clamp 到 len-1 只剩最后一行)
        app.scroll[BOX_CONNS] = 5000;
        let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();
        let buf = term.backend().buffer();
        let rows: Vec<String> = (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect())
            .collect();
        let shown: Vec<&String> = rows.iter().filter(|r| r.contains("c019")).collect();
        assert!(!shown.is_empty(), "最后一页应包含尾段连接");
        // 尾页应是多行(旧 bug 只渲染 1 行)
        assert!(
            shown.len() >= 5,
            "尾页行数 {} < 5,像是只剩单行",
            shown.len()
        );
        // 首屏的行不应再出现
        assert!(
            !rows.iter().any(|r| r.contains("c0000")),
            "越界滚动后仍显示首行"
        );
    }

    /// DISKS 滚到底:off 封顶应是最后一页(可视行数)而非最后一行。
    /// 回归:DISKS 曾手写 min(len-1)(LAN/CONNS/PROCS 用 clamped_offset,
    /// 唯它漏网),滚到底后整盒只剩 1 行或空白
    #[test]
    fn disks_bottom_scroll_shows_full_last_page() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut app = crate::app::App::new(None);
        for i in 0..BOX_COUNT {
            app.visible[i] = i == BOX_DISKS;
        }
        // 5 个盘 × 每盘 12 个分区 = 60+ 行树,远超一屏
        for d in 0..5 {
            let root = crate::collect::disk::TreeNode {
                name: format!("sd{d}"),
                pretty: None,
                size: None,
                children: (0..12)
                    .map(|p| crate::collect::disk::TreeNode {
                        name: format!("sd{d}p{}", p + 1),
                        pretty: None,
                        size: None,
                        children: Vec::new(),
                    })
                    .collect(),
            };
            app.disk.tree.push(root);
        }
        app.scroll[BOX_DISKS] = 5000;
        let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();
        let buf = term.backend().buffer();
        let rows: Vec<String> = (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect())
            .collect();
        // 尾段分区(sd4p8..p12)应可见
        assert!(
            rows.iter().any(|r| r.contains("sd4p11")),
            "最后一页应包含尾部节点"
        );
        // 尾页应是多行(旧 bug 只渲染 1 行)
        let tail_rows = rows
            .iter()
            .filter(|r| ["sd4p", "sd4"].iter().any(|t| r.contains(t)))
            .count();
        assert!(tail_rows >= 5, "尾页行数 {tail_rows} < 5,像是只剩单行");
        assert!(
            !rows.iter().any(|r| r.contains("sd0p1")),
            "越界滚动后仍显示首行"
        );
    }

    /// disks-only app with an overflowing tree: `disks` root × 12 partitions
    fn disks_overflow_app(disks: usize) -> crate::app::App {
        let mut app = crate::app::App::new(None);
        for i in 0..BOX_COUNT {
            app.visible[i] = i == BOX_DISKS;
        }
        for d in 0..disks {
            app.disk.tree.push(crate::collect::disk::TreeNode {
                name: format!("sd{d}"),
                pretty: None,
                size: None,
                children: (0..12)
                    .map(|p| crate::collect::disk::TreeNode {
                        name: format!("sd{d}p{}", p + 1),
                        pretty: None,
                        size: None,
                        children: Vec::new(),
                    })
                    .collect(),
            });
        }
        app
    }

    /// render to a line-per-row symbol grid (TestBackend, no shared state)
    fn render_grid(app: &crate::app::App) -> Vec<String> {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
        term.draw(|f| draw(f, app)).unwrap();
        let buf = term.backend().buffer();
        (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect())
            .collect()
    }

    #[test]
    fn disks_scrollbar_shows_arrows_and_thumb_at_offset() {
        // 溢出列表:btop 字形出现,滑块随偏移从轨道顶走到轨道底
        let mut app = disks_overflow_app(3);
        let mut rows = render_grid(&app);
        // 仅 disks 可见,缓冲区里的 ↑ 只能来自滚动条
        let (uy, ux) = rows
            .iter()
            .enumerate()
            .find_map(|(y, r)| r.chars().position(|c| c == '\u{2191}').map(|x| (y, x)))
            .expect("overflowing disks list renders an up arrow");
        let col: Vec<char> = rows.iter().map(|r| r.chars().nth(ux).unwrap()).collect();
        let dy = col
            .iter()
            .position(|&c| c == '\u{2193}')
            .expect("down arrow");
        assert!(dy > uy + 2, "arrows must not be adjacent");
        assert_eq!(
            col[uy + 1..dy].iter().filter(|&&c| c == '\u{2588}').count(),
            1,
            "exactly one thumb on the track"
        );
        assert_eq!(col[uy + 1], '\u{2588}', "off=0 puts the thumb at the top");

        app.scroll[BOX_DISKS] = usize::MAX; // draw clamps to the last page
        rows = render_grid(&app);
        let col: Vec<char> = rows.iter().map(|r| r.chars().nth(ux).unwrap()).collect();
        let dy = col.iter().position(|&c| c == '\u{2193}').unwrap();
        assert_eq!(
            col[dy - 1],
            '\u{2588}',
            "bottom offset puts the thumb at the track end"
        );

        // 不溢出:不渲染滚动条
        let app = disks_overflow_app(1);
        let rows = render_grid(&app);
        assert!(
            !rows.iter().any(|r| r.contains('\u{2191}')),
            "fitting list must not render a scrollbar"
        );
    }

    #[test]
    fn disks_scrollbar_hit_pages_and_thumb() {
        // 纯几何核:内框 40×20 → 滑条列 x=38,数据行 y=1..=18
        let inner = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 20,
        };
        let (len, visible, off) = (100usize, 10usize, 0usize);
        let bar_x = inner.right() - 1;
        assert!(scrollbar_hit_at(inner, bar_x, 1, len, visible, off)
            .is_some_and(|h| matches!(h, SbHit::PageUp)));
        assert!(scrollbar_hit_at(inner, bar_x, 19, len, visible, off)
            .is_some_and(|h| matches!(h, SbHit::PageDown)));
        // off=0 时滑块在首轨行
        assert!(scrollbar_hit_at(inner, bar_x, 2, len, visible, off)
            .is_some_and(|h| matches!(h, SbHit::Thumb { grab: 0 })));
        // 滑块上方翻上页、下方翻下页(off 取中位,滑块落在轨道中部)
        let mid_off = 45usize;
        assert!(scrollbar_hit_at(inner, bar_x, 5, len, visible, mid_off)
            .is_some_and(|h| matches!(h, SbHit::PageUp)));
        assert!(scrollbar_hit_at(inner, bar_x, 15, len, visible, mid_off)
            .is_some_and(|h| matches!(h, SbHit::PageDown)));
        // 滑条列之外与不溢出列表都不是命中
        assert!(scrollbar_hit_at(inner, bar_x - 1, 5, len, visible, off).is_none());
        assert!(scrollbar_hit_at(inner, bar_x, 5, 5, visible, 0).is_none());
    }

    #[test]
    fn disks_scrollbar_drag_maps_track_to_offset() {
        let mut app = crate::app::App::new(None);
        let inner = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 20,
        };
        let (len, visible) = (100usize, 10usize);
        let last_track = 1 + (20 - 1) - 2; // bar.y + bar.height - 2
        scrollbar_drag_at(&mut app, inner, len, visible, last_track, 0);
        assert_eq!(
            app.scroll[BOX_DISKS],
            len - visible,
            "track end = last page"
        );
        scrollbar_drag_at(&mut app, inner, len, visible, 2, 0);
        assert_eq!(app.scroll[BOX_DISKS], 0, "track start = first page");
        scrollbar_drag_at(&mut app, inner, len, visible, (2 + last_track) / 2, 0);
        let mid = app.scroll[BOX_DISKS];
        assert!(
            (40..=50).contains(&mid),
            "middle of the track ≈ half the span, got {mid}"
        );
        // 记录了抓取偏移的拖拽:同一行减去偏移
        app.scroll[BOX_DISKS] = 0;
        scrollbar_drag_at(&mut app, inner, len, visible, 7, 5);
        assert_eq!(app.scroll[BOX_DISKS], 0, "grab-compensated top stays top");
    }

    #[test]
    fn disk_row_shows_fstype_tail_after_name() {
        let mut app = disks_overflow_app(0);
        app.disk.devices.push(crate::collect::disk::DiskDev {
            name: "sda1".into(),
            fstype: "ext4".into(),
            mounts: vec!["/".into()],
            total: 100,
            used: 40,
            ..Default::default()
        });
        app.disk.tree.push(crate::collect::disk::TreeNode {
            name: "sda1".into(),
            pretty: None,
            size: None,
            children: Vec::new(),
        });
        let rows = render_grid(&app);
        let row = rows.iter().find(|r| r.contains("sda1")).expect("row");
        assert!(
            row.contains("· ext4"),
            "dim fstype tail after the name: {row}"
        );
    }

    /// proc 排序:同名进程成组,组首行带 ×N 标识
    /// 选中聚合父行:覆盖层显示组摘要(Σ/成员清单),而非被聚合的单条连接
    #[test]
    fn aggregate_selection_shows_group_summary() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut app = crate::app::App::new(None);
        for i in [BOX_NET, BOX_DISKIO, BOX_DISKS, BOX_LAN, BOX_PROCS] {
            app.visible[i] = false;
        }
        let mk = |inode: u64, recv: u64| crate::collect::conn::Conn {
            proto: crate::collect::conn::IPPROTO_TCP,
            local: ("127.0.0.1".parse().unwrap(), 1000 + inode as u16),
            remote: ("127.0.0.1".parse().unwrap(), 443),
            state: 1,
            uid: 0,
            inode,
            bytes_recv: recv,
            bytes_sent: 0,
            rate_recv: 0.0,
            rate_sent: 0.0,
            pid: Some(1000 + inode as u32),
            proc: Some("firefox".into()),
            dead: false,
            first_seen: None,
            dead_at: None,
        };
        app.conn.conns = vec![mk(1, 100), mk(2, 900), mk(3, 50)];
        app.conn.sort = crate::collect::conn::ConnSort::Proc;
        app.sel[BOX_CONNS] = Some(0); // 聚合父行
        app.focus[BOX_CONNS] = true;
        fn rows_containing(buf: &str, needle: &str, cap: usize) -> bool {
            buf.matches(needle).count() >= cap
        }
        let mut term = Terminal::new(TestBackend::new(120, 16)).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();
        let buf: String = term
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(buf.contains("firefox ×3"), "组摘要缺失");
        assert!(buf.contains("1.0KiB"), "组 Σ 缺失");
        // 单行摘要:不再罗列成员 IP(子行本身就在聚合行下方展开)
        assert!(
            !rows_containing(&buf, "127.0.0.1:1001", 3),
            "成员 IP 不应出现在摘要里"
        );
    }

    /// proc 排序下追踪行不单行置顶:含追踪的组整组提升,组内相邻(尾部
    /// 墓碑并回本组)——单行置顶曾把同名进程撕成多个碎组、死亡追踪行还
    /// 以旧进程名的墓碑身份混进最前
    #[test]
    fn proc_sort_keeps_tracked_rows_inside_their_group() {
        let mut app = crate::app::App::new(None);
        let mk = |inode: u64, name: &str, dead: bool| crate::collect::conn::Conn {
            proto: crate::collect::conn::IPPROTO_TCP,
            local: ("127.0.0.1".parse().unwrap(), 1000 + inode as u16),
            remote: ("127.0.0.1".parse().unwrap(), 443),
            state: 1,
            uid: 0,
            inode,
            bytes_recv: 100,
            bytes_sent: 0,
            rate_recv: 0.0,
            rate_sent: 0.0,
            pid: Some(1000 + inode as u32),
            proc: Some(name.to_string()),
            dead,
            first_seen: None,
            dead_at: None,
        };
        // other 组在前、steam 在后;只追踪 steam 的尾部墓碑:
        // steam 墓碑必须并回 steam 组(而不是在顶部自成一组),
        // 且含追踪的 steam 组整组提升到 other 之前
        app.conn.conns = vec![
            mk(3, "other", false),
            mk(1, "steam", false),
            mk(2, "steam", false),
            mk(4, "steam", true),
        ];
        app.tracked.conns = vec![4];
        app.conn.sort = crate::collect::conn::ConnSort::Proc;
        let keys: Vec<String> = conn_list(&app)
            .iter()
            .map(|c| crate::collect::conn::owner_key(c))
            .collect();
        assert_eq!(
            keys,
            vec!["steam", "steam", "steam", "other"],
            "同名行必须同组相邻(墓碑并回),追踪只提升整组: {keys:?}"
        );
        // 不追踪时保持原有组序(稳定,不打散)
        app.tracked.conns = Vec::new();
        let keys: Vec<String> = conn_list(&app)
            .iter()
            .map(|c| crate::collect::conn::owner_key(c))
            .collect();
        assert_eq!(keys, vec!["other", "steam", "steam", "steam"]);
    }

    #[test]
    fn proc_sort_groups_same_name_with_marker() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut app = crate::app::App::new(None);
        for i in [BOX_NET, BOX_DISKIO, BOX_DISKS, BOX_LAN, BOX_PROCS] {
            app.visible[i] = false;
        }
        let mk = |inode: u64, recv: u64| crate::collect::conn::Conn {
            proto: crate::collect::conn::IPPROTO_TCP,
            local: ("127.0.0.1".parse().unwrap(), 1000 + inode as u16),
            remote: ("127.0.0.1".parse().unwrap(), 443),
            state: 1,
            uid: 0,
            inode,
            bytes_recv: recv,
            bytes_sent: 0,
            rate_recv: 0.0,
            rate_sent: 0.0,
            pid: Some(1000 + inode as u32),
            proc: Some("firefox".into()),
            dead: false,
            first_seen: None,
            dead_at: None,
        };
        app.conn.conns = vec![mk(1, 100), mk(2, 900), mk(3, 50)];
        app.conn.sort = crate::collect::conn::ConnSort::Proc;
        let mut term = Terminal::new(TestBackend::new(120, 16)).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();
        let buf: String = term
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        eprintln!("BUF:\n{}", {
            let b = term.backend().buffer();
            (0..b.area.height)
                .map(|y| {
                    (0..b.area.width)
                        .map(|x| b[(x, y)].symbol())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n")
        });
        assert!(buf.contains("firefox ×3"), "组首 ×N 标识缺失");
        // 默认折叠:聚合行显示组 Σ(1050B → 1.0KiB),子行隐藏
        assert!(buf.contains("1.0KiB"), "聚合 Σ 缺失");
        assert!(!buf.contains("900B"), "折叠时子行不应显示");
        // 展开后子行可见(各自字节)
        app.expanded.insert("firefox".into());
        term.draw(|f| draw(f, &app)).unwrap();
        let buf: String = term
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(
            buf.contains("900B") && buf.contains("100B") && buf.contains("50B"),
            "展开后子行缺失"
        );
        // 组按合计降序:最大字节连接(900)是组首且排最前
        assert!(buf.contains("c"), "应有连接行");
    }

    #[test]
    fn classic_right_column_snaps_to_midline() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut app = crate::app::App::new(None);
        app.layout = crate::app::LayoutMode::Classic;
        for i in 0..BOX_COUNT {
            app.visible[i] = true;
        }
        // LAN 盒高被钉 6 行:标题+表头+3 行设备+边框——3 台应全部可见
        for k in 0..5u32 {
            app.lan.neighbors.insert(
                format!("10.0.0.{}", k + 1).parse().unwrap(),
                crate::collect::lan::Neighbor {
                    ip: format!("10.0.0.{}", k + 1).parse().unwrap(),
                    mac: None,
                    mac_hidden: false,
                    iface: "eth0".into(),
                    complete: true,
                    hostname: None,
                    is_gateway: false,
                    is_self: false,
                    last_seen: std::time::Instant::now(),
                },
            );
        }
        let mut term = Terminal::new(TestBackend::new(140, 40)).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();
        let buf = term.backend().buffer();
        let rows: Vec<String> = (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect())
            .collect();

        // 盒标题自带唯一的上标编号(btop 角标样式),按它定位最稳:
        // 标题行 = 盒子顶边框行;底边框行 = 下一盒标题行 - 1
        let title_row = |tag: &str| -> usize {
            rows.iter()
                .enumerate()
                .find(|(_, r)| r.contains(tag))
                .map(|(y, _)| y)
                .unwrap_or_else(|| panic!("找不到标题 {tag}"))
        };

        let _y_net = title_row("\u{b9}net"); // ¹
        let y_diskio = title_row("\u{b3}disk i/o"); // ³
        let y_lan = title_row("\u{b2}lan"); // ²
        let y_conns = title_row("\u{2075}conns"); // ⁵
        let y_procs = title_row("\u{2076}procs"); // ⁶
        let net_bottom = y_diskio - 1; // NET 底边框 = DISK I/O 标题行的上一行
        let conns_bottom = y_procs - 1;
        assert_eq!(
            conns_bottom, net_bottom,
            "CONNS 底边框(行 {conns_bottom})未对齐 NET 底边框(行 {net_bottom})"
        );
        // 上组相邻:LAN 底边框 = CONNS 顶边框 - 1,且 LAN 至少有标题+一行内容
        assert!(
            y_conns - 1 > y_lan + 1,
            "LAN 与 CONNS 应相邻且 LAN 有内容区"
        );
        // 盒高 6 → 内容 3 行:设备 1-3 可见,4-5 不见(曾因表头重复扣行只剩 2 台)
        assert!(
            rows.iter().any(|r| r.contains("10.0.0.3")),
            "第 3 台设备应可见"
        );
        assert!(
            !rows.iter().any(|r| r.contains("10.0.0.4")),
            "第 4 台设备应被滚动窗口裁掉"
        );
    }

    #[test]
    fn search_filters_procs_and_conns_by_name() {
        use crate::collect::conn::IPPROTO_TCP;
        let mut app = crate::app::App::new(None);
        app.proc.procs.push(crate::collect::proc::ProcRow {
            pid: 1,
            name: "firefox".into(),
            net_recv: 10,
            net_sent: 5,
            net_rr: 0.0,
            net_sr: 0.0,
            udp_recv: 0,
            udp_sent: 0,
            udp_rr: 0.0,
            udp_sr: 0.0,
            disk_r: 0,
            disk_w: 0,
            disk_rr: 0.0,
            disk_wr: 0.0,
            uptime_s: None,
            alive: true,
            cmdline: None,
        });
        app.proc.procs.push(crate::collect::proc::ProcRow {
            pid: 2,
            name: "chrome".into(),
            ..app.proc.procs[0].clone()
        });
        app.conn.conns.push(crate::collect::conn::Conn {
            proto: IPPROTO_TCP,
            local: ("127.0.0.1".parse().unwrap(), 1000),
            remote: ("127.0.0.1".parse().unwrap(), 443),
            state: 1,
            uid: 0,
            inode: 9,
            bytes_recv: 0,
            bytes_sent: 0,
            rate_recv: 0.0,
            rate_sent: 0.0,
            pid: Some(1),
            proc: Some("firefox".into()),
            dead: false,
            first_seen: None,
            dead_at: None,
        });
        app.search = "FIRE".into(); // 大小写不敏感
        app.search_box = BOX_CONNS; // 默认目标:conns
        let names: Vec<_> = procs_display(&app).iter().map(|p| p.name.clone()).collect();
        assert_eq!(names.len(), 2, "目标 conns 时 procs 不过滤");
        let conns = conns_display(&app);
        assert_eq!(conns.len(), 1, "目标 conns 应过滤连接归属");
        app.search_box = BOX_PROCS;
        let names: Vec<_> = procs_display(&app).iter().map(|p| p.name.clone()).collect();
        assert_eq!(names, vec!["firefox"], "目标 procs 应过滤进程");
        assert_eq!(conns_display(&app).len(), 1, "非目标盒 conns 应直通不清空");
        app.search.clear();
        assert_eq!(procs_display(&app).len(), 2, "清空搜索应恢复全部");
    }

    #[test]
    fn conn_filter_cycles_all_lan_tcp_udp() {
        use crate::collect::conn::{ConnFilter, IPPROTO_TCP, IPPROTO_UDP};
        let mut app = crate::app::App::new(None);
        let mk = |inode: u64, proto: u8, remote: std::net::IpAddr| crate::collect::conn::Conn {
            proto,
            local: ("127.0.0.1".parse().unwrap(), 1000 + inode as u16),
            remote: (remote, 443),
            state: 1,
            uid: 0,
            inode,
            bytes_recv: 0,
            bytes_sent: 0,
            rate_recv: 0.0,
            rate_sent: 0.0,
            pid: None,
            proc: None,
            dead: false,
            first_seen: None,
            dead_at: None,
        };
        let lan = std::net::IpAddr::V4("192.168.1.9".parse().unwrap());
        app.conn.conns = vec![
            mk(1, IPPROTO_TCP, "93.184.216.34".parse().unwrap()), // 外网 tcp
            mk(2, IPPROTO_TCP, lan),                              // 局域网 tcp
            mk(3, IPPROTO_UDP, lan),                              // 局域网 udp
        ];
        app.conn.filter = ConnFilter::All;
        assert_eq!(conns_display(&app).len(), 3);
        app.conn.filter = ConnFilter::Lan;
        assert_eq!(conns_display(&app).len(), 2, "lan 应滤掉外网");
        app.conn.filter = ConnFilter::Tcp;
        assert_eq!(conns_display(&app).len(), 2, "tcp 应只剩两条 tcp");
        assert!(conns_display(&app).iter().all(|c| c.is_tcp()));
        app.conn.filter = ConnFilter::Udp;
        assert_eq!(conns_display(&app).len(), 1, "udp 应只剩一条 udp");
        // 循环闭合
        assert_eq!(ConnFilter::Udp.next(), ConnFilter::All);
    }

    /// LAN 列表滚动:off 推进后可见行随之移动,列表不再"超面板高度不可滚"
    #[test]
    fn lan_list_scrolls_with_offset() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut app = crate::app::App::new(None);
        // 盒子开关来自真实 config,测试必须显式设定目标盒
        for i in 0..BOX_COUNT {
            app.visible[i] = i == BOX_LAN;
        }
        for k in 0..30u32 {
            app.lan.neighbors.insert(
                format!("10.0.0.{}", k + 1).parse().unwrap(),
                crate::collect::lan::Neighbor {
                    ip: format!("10.0.0.{}", k + 1).parse().unwrap(),
                    mac: None,
                    mac_hidden: false,
                    iface: "eth0".into(),
                    complete: true,
                    hostname: None,
                    is_gateway: false,
                    is_self: false,
                    last_seen: std::time::Instant::now(),
                },
            );
        }
        let render_buf = |app: &crate::app::App| {
            let mut term = Terminal::new(TestBackend::new(120, 16)).unwrap();
            term.draw(|f| draw(f, app)).unwrap();
            term.backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect::<String>()
        };
        let render_first_ip = |app: &crate::app::App| {
            let mut term = Terminal::new(TestBackend::new(120, 16)).unwrap();
            term.draw(|f| draw(f, app)).unwrap();
            let buf: String = term
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect();
            // 匹配带尾随空格的完整 IP:"10.0.0.1 " 不会被 "10.0.0.19" 满足
            (0..30u32)
                .filter(|k| buf.contains(&format!("10.0.0.{} ", k + 1)))
                .collect::<Vec<_>>()
        };
        app.scroll[BOX_LAN] = 0;
        let at0 = render_first_ip(&app);
        assert!(at0.contains(&0), "首屏应含第 1 台");
        assert!(!at0.contains(&20), "首屏不应含第 21 台");
        // 滚动提示与真实剩余一致(30 台 − 可见 12 行 = +18)
        assert!(at0.contains(&0), "占位");
        let buf0 = render_buf(&app);
        assert!(buf0.contains("+18"), "+N 提示缺失或错误");
        app.scroll[BOX_LAN] = 20;
        let at20 = render_first_ip(&app);
        assert!(at20.contains(&20), "滚动后应见第 21 台");
        assert!(!at20.contains(&0), "滚动后首屏不应再含第 1 台");
    }

    #[test]
    fn conn_rows_carry_protocol_labels_without_glyphs() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut app = crate::app::App::new(None);
        // 过滤/排序/语言来自真实 config:断言前显式钉住
        app.conn.filter = crate::collect::conn::ConnFilter::All;
        for i in [BOX_NET, BOX_DISKIO, BOX_DISKS, BOX_LAN] {
            app.visible[i] = false;
        }
        let mk = |inode: u64, name: &str, proto: u8, state: u8| crate::collect::conn::Conn {
            proto,
            local: ("127.0.0.1".parse().unwrap(), 1000 + inode as u16),
            remote: ("127.0.0.1".parse().unwrap(), 443),
            state,
            uid: 0,
            inode,
            bytes_recv: 0,
            bytes_sent: 0,
            rate_recv: 0.0,
            rate_sent: 0.0,
            pid: Some(7),
            proc: Some(name.to_string()),
            dead: false,
            first_seen: None,
            dead_at: None,
        };
        // 三个不同名字:proc 排序下各自成组(×1),不触发 ×N 标记
        app.conn.conns = vec![
            mk(
                1,
                "demo",
                crate::collect::conn::IPPROTO_TCP,
                crate::collect::conn::TCP_ESTABLISHED,
            ),
            mk(2, "demo2", crate::collect::conn::IPPROTO_UDP, 7),
            mk(
                3,
                "demo3",
                crate::collect::conn::IPPROTO_TCP,
                crate::collect::conn::TCP_LISTEN,
            ),
        ];
        let mut term = Terminal::new(TestBackend::new(160, 30)).unwrap();
        // 表头随排序模式切换:sum 显示 Σ 合计量,rate 显示速率
        app.conn.sort = crate::collect::conn::ConnSort::Rate;
        term.draw(|f| draw(f, &app)).unwrap();
        let buf: String = term
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(buf.contains("RX/S"), "rate 表头缺失");
        assert!(!buf.contains("Σ RX"), "rate 模式不应出现 Σ 表头");
        app.conn.sort = crate::collect::conn::ConnSort::Sum;
        term.draw(|f| draw(f, &app)).unwrap();
        let buf: String = term
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(buf.contains("Σ RX") && buf.contains("Σ TX"), "sum 表头缺失");
        assert!(!buf.contains("RX/S"), "sum 模式不应残留速率表头");
        // proc 档同为合计口径
        app.conn.sort = crate::collect::conn::ConnSort::Proc;
        term.draw(|f| draw(f, &app)).unwrap();
        let buf: String = term
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(buf.contains("Σ RX"), "proc 档应显示合计表头");
        eprintln!(
            "DEMO-HITS: {}",
            [
                "demo tcp",
                "demo2 udp",
                "◦demo3 tcp",
                "demo",
                "demo2",
                "demo3"
            ]
            .iter()
            .map(|t| format!("{t}={}", buf.contains(t)))
            .collect::<Vec<_>>()
            .join(" ")
        );
        eprintln!("FULL: {}", buf.replace("\n", "|"));
        assert!(buf.contains("demo tcp"), "tcp 标签缺失");
        assert!(buf.contains("demo2 udp"), "udp 标签缺失");
        assert!(buf.contains("demo3 \u{25e6}tcp"), "监听标签缺失");
        for gone in ["\u{25b8}", "\u{22c4}"] {
            assert!(!buf.contains(gone), "旧符号 {gone} 仍在");
        }
    }

    #[test]
    fn layout_priority_favours_lists() {
        // classic: lists keep at least half, the graphs give up width first
        assert_eq!(split_horizontal(150, 76, 40), (74, 76));
        assert_eq!(split_horizontal(100, 76, 40), (40, 60)); // graphs at their floor
        assert_eq!(split_horizontal(60, 76, 40), (30, 30)); // both cramped, split evenly
                                                            // wide/tall: lists take their ideal height, graphs absorb the surplus
                                                            // but are capped at half the height — full-screen surplus goes to lists
        assert_eq!(split_vertical(100, 4, 30), (50, 50));
        assert_eq!(split_vertical(40, 4, 30), (10, 30));
        // when short, graphs hit graph_min before the lists shrink
        assert_eq!(split_vertical(20, 8, 30), (8, 12));
        assert_eq!(split_vertical(12, 8, 30), (8, 4));
    }

    /// 全高回归(用户实机截图发现):wide/tall 下两个图表曾吃到 4/5 屏高。
    /// 复现场景 = 列表内容少(ideal 小)时余量全被图表吞掉。渲染级钉住:
    /// 只留 PROCS 一个列表盒,tall 全高时 ⁶procs 标题必须起自下半屏
    #[test]
    fn tall_layout_caps_graphs_at_half_height() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut app = crate::app::App::new(None);
        app.layout = crate::app::LayoutMode::Tall;
        for i in [BOX_LAN, BOX_CONNS, BOX_DISKS] {
            app.visible[i] = false;
        }
        app.update();
        let mut term = Terminal::new(TestBackend::new(200, 60)).unwrap();
        term.draw(|f| crate::ui::draw(f, &app)).unwrap();
        let buf = term.backend().buffer();
        let rows: Vec<String> = (0..buf.area().height)
            .map(|y| {
                (0..buf.area().width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect()
            })
            .collect();
        let title_row = |tag: &str| {
            rows.iter()
                .position(|r| r.contains(tag))
                .unwrap_or_else(|| panic!("找不到标题 {tag}"))
        };
        let y_net = title_row("\u{b9}net"); // ¹net 图表区顶
        let y_procs = title_row("\u{2076}procs"); // ⁶procs 表格区起
        assert!(
            y_procs - y_net <= 32,
            "图表区应 ≤ 半屏:net@{y_net} procs@{y_procs}(总高 60)"
        );
    }

    #[test]
    fn stable_alloc_ignores_blips_and_adopts_real_changes() {
        use std::time::{Duration, Instant};
        let t0 = Instant::now();
        let mut m = StableAlloc::new();
        // first call always applies
        assert_eq!(m.update(vec![10, 20], 30, t0), vec![10, 20]);
        // 2 rows of drift is a blip: ignored even after the settle window
        assert_eq!(
            m.update(vec![12, 18], 30, t0 + Duration::from_secs(5)),
            vec![10, 20]
        );
        // a 4-row change that persists is adopted as a whole
        assert_eq!(
            m.update(vec![14, 16], 30, t0 + Duration::from_secs(10)),
            vec![14, 16]
        );
        // a terminal resize re-applies immediately
        assert_eq!(
            m.update(vec![20, 30], 50, t0 + Duration::from_secs(10)),
            vec![20, 30]
        );
        // an allocation whose sum does not match the space is rejected
        assert_eq!(
            m.update(vec![20, 40], 50, t0 + Duration::from_secs(20)),
            vec![20, 30]
        );
    }

    #[test]
    fn alloc_heights_gives_spare_to_priority_boxes() {
        // ideals [6,12,4,10] (lan,conns,procs,disks), 40 total: spare 8 goes
        // to the process tables (positions 1,2) split evenly
        assert_eq!(
            alloc_heights(&[6, 12, 4, 10], 40, &[1, 2]),
            vec![6, 16, 8, 10]
        );
        // spare smaller than the target count still lands on priority boxes
        assert_eq!(
            alloc_heights(&[6, 12, 4, 10], 39, &[1, 2]),
            vec![6, 16, 7, 10]
        );
        // no spare_to: even spread (old behaviour)
        assert_eq!(alloc_heights(&[6, 8, 4, 5], 40, &[]), vec![11, 12, 8, 9]);
    }

    #[test]
    fn alloc_heights_shrinks_proportionally_when_cramped() {
        // cramped: proportional shrink, every box keeps >= 3 rows
        assert_eq!(alloc_heights(&[10, 10], 10, &[]), vec![5, 5]);
        assert_eq!(alloc_heights(&[10, 10], 6, &[1]), vec![3, 3]);
        // degenerate terminal still yields usable rows
        assert!(alloc_heights(&[8, 8, 8], 5, &[]).iter().all(|&h| h >= 1));
    }
}

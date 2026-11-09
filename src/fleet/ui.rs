//! fleet TUI 渲染层(自包含,不碰主 TUI 的 ui/mod.rs)。
//!
//! 画面语法是"节点为行"的表 + Σ 速率阻尼峰值走线,与六盒布局无关;
//! 复用的只有 palette 语义色、fmt_human、sanitize_text。所有会落屏的
//! 远端字符串(主机名/进程名/错误行)都先 sanitize;选中态用 hi/accent
//! 派生(ui/mod.rs 的 selection 助手是私有的,不为此打开它)。

use std::collections::HashMap;

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::symbols;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Row, Sparkline, Table, Widget};
use ratatui::Frame;

use super::model::NodeSample;
use crate::snapshot::Snapshot;

/// UI 侧交互状态(选中/弹窗/阻尼峰值),事件循环持有
pub struct UiState {
    pub selected: usize,
    pub help: bool,
    /// 打开的详情弹窗(节点 target;None=关闭)
    pub detail: Option<String>,
    pub detail_scroll: u16,
    /// Σrx/Σtx 阻尼峰值(只在轮次推进时更新,见 update_peaks)
    pub peak: (f64, f64),
    last_round: u64,
}

impl UiState {
    pub fn new() -> Self {
        UiState {
            selected: 0,
            help: false,
            detail: None,
            detail_scroll: 0,
            peak: (1.0, 1.0),
            last_round: 0,
        }
    }

    /// 轮次推进才更新阻尼峰值:瞬时跟涨、×0.97/轮缓落(0.2.8 教训:
    /// 取窗口 max 会尖峰压扁全窗,这里跟涨即时、衰减按轮不按帧)
    pub fn update_peaks(&mut self, round: u64, sum_rx: f64, sum_tx: f64) {
        if round != self.last_round {
            self.last_round = round;
            self.peak.0 = damp(self.peak.0, sum_rx);
            self.peak.1 = damp(self.peak.1, sum_tx);
        }
    }
}

impl Default for UiState {
    fn default() -> Self {
        Self::new()
    }
}

/// 事件循环每帧从共享态克隆出的小视图(不持锁绘制)
pub struct UiView {
    pub order: Vec<String>,
    pub samples: HashMap<String, NodeSample>,
    /// 详情弹窗正打开的那个节点的最近一次成功快照
    pub raws: HashMap<String, Snapshot>,
    pub round: u64,
    pub sum_hist: Vec<(f64, f64)>,
}

/// 阻尼峰值(纯函数;1.0 下限防零除)
pub fn damp(old: f64, new: f64) -> f64 {
    if new > old {
        new
    } else {
        (old * 0.97).max(1.0)
    }
}

/// 头行文本(纯函数)
pub fn header_text(nodes: usize, up: usize, round: u64, zh: bool) -> String {
    if zh {
        format!(" fleet — {nodes} 节点 · {up} 在线 · 第 {round} 轮 ")
    } else {
        format!(" fleet — {nodes} nodes · {up} up · round {round} ")
    }
}

/// 尾行键位提示(纯函数)
pub fn footer_text(zh: bool) -> String {
    if zh {
        " [↑↓] 选择 · [enter] 详情 · [h] 帮助 · [q] 退出 ".to_string()
    } else {
        " [↑↓] select · [enter] detail · [h] help · [q] quit ".to_string()
    }
}

/// 帮助弹窗行(纯函数,双语)
pub fn help_lines(zh: bool) -> Vec<String> {
    if zh {
        vec![
            "ltop fleet — SSH 多机聚合".into(),
            "".into(),
            "↑/↓        选择节点".into(),
            "enter      单节点详情(来自最近一次成功快照)".into(),
            "esc        关闭弹窗".into(),
            "h / ?      本帮助".into(),
            "q / ctrl+c 退出".into(),
            "".into(),
            "节点表来自 ~/.config/ltop/fleet.conf 或 --host;".into(),
            "配置新节点: ltop fleet setup <目标>".into(),
            "失败行保留上一轮成功数据(⚠ 为本轮错误)".into(),
        ]
    } else {
        vec![
            "ltop fleet — SSH multi-node aggregation".into(),
            "".into(),
            "↑/↓        select node".into(),
            "enter      node detail (from the last good snapshot)".into(),
            "esc        close popup".into(),
            "h / ?      this help".into(),
            "q / ctrl+c quit".into(),
            "".into(),
            "Nodes come from ~/.config/ltop/fleet.conf or --host;".into(),
            "set up a node: ltop fleet setup <target>".into(),
            "Failed rounds keep the last good row (⚠ marks this round's error)".into(),
        ]
    }
}

/// 单节点详情行(纯函数,全部 sanitize;无 raw 时给空态提示)
pub fn detail_lines(target: &str, snap: Option<&Snapshot>, zh: bool) -> Vec<String> {
    let Some(s) = snap else {
        return vec![crate::pick_s(
            zh,
            "no successful round yet — waiting for the first good snapshot",
            "还没有成功轮次 — 等待第一份快照",
        )];
    };
    let mut v = Vec::new();
    v.push(format!(
        "node      {} ({})",
        crate::sanitize_text(&s.meta.hostname),
        crate::sanitize_text(target)
    ));
    v.push(format!(
        "system    {} · {} · ltop {} · udp_helper {}",
        crate::sanitize_text(&s.meta.os),
        crate::sanitize_text(&s.meta.arch),
        crate::sanitize_text(&s.meta.version),
        if s.meta.udp_helper { "on" } else { "off" },
    ));
    let sel = crate::sanitize_text(&s.net.selected);
    for (i, iface) in s.net.interfaces.iter().enumerate() {
        let name = crate::sanitize_text(&iface.name);
        let tag = if iface.name == sel { " ←" } else { "" };
        if i == 0 {
            v.push(format!(
                "net       {name}  rx {}  tx {}{tag}",
                crate::fmt_human(iface.rx_bps),
                crate::fmt_human(iface.tx_bps),
            ));
        } else {
            v.push(format!(
                "          {name}  rx {}  tx {}{tag}",
                crate::fmt_human(iface.rx_bps),
                crate::fmt_human(iface.tx_bps),
            ));
        }
    }
    v.push(format!(
        "lan       {} subnets · {} gateways · {} neighbours",
        s.lan.subnets.len(),
        s.lan.gateways.len(),
        s.lan.neighbours.len(),
    ));
    v.push(format!(
        "disk      session read {} · write {} · {} devices",
        crate::fmt_human(s.disk.session_read as f64),
        crate::fmt_human(s.disk.session_write as f64),
        s.disk.devices.len(),
    ));
    v.push(format!("conns     {} tracked", s.conns.len()));
    // top 5 进程按 net_bps 收发合计
    let mut procs: Vec<(&String, f64)> = s
        .procs
        .iter()
        .map(|p| (&p.name, p.net_bps_recv + p.net_bps_sent))
        .collect();
    procs.sort_by(|a, b| b.1.total_cmp(&a.1));
    for (i, (name, vbps)) in procs.iter().take(5).enumerate() {
        let head = if i == 0 { "top procs " } else { "          " };
        v.push(format!(
            "{head}{}  {}/s",
            crate::sanitize_text(name),
            crate::fmt_human(*vbps)
        ));
    }
    v
}

/// 弹窗居中矩形
fn popup_rect(area: Rect, pct_x: u16, pct_y: u16) -> Rect {
    let v = Layout::vertical([
        Constraint::Percentage((100 - pct_y) / 2),
        Constraint::Percentage(pct_y),
        Constraint::Percentage((100 - pct_y) / 2),
    ])
    .split(area);
    let h = Layout::horizontal([
        Constraint::Percentage((100 - pct_x) / 2),
        Constraint::Percentage(pct_x),
        Constraint::Percentage((100 - pct_x) / 2),
    ])
    .split(v[1]);
    h[1]
}

/// 表的一行单元格(纯函数,测试钉住 sanitize/错误行布局/选中态)
/// 返回 (单元格文本, dim 样式)
pub fn row_cells(r: &NodeSample, selected: bool, zh: bool) -> (Vec<String>, bool) {
    let _ = zh;
    match &r.error {
        Some(_) => (
            vec![
                r.target.clone(),
                "⚠".into(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                r.error.clone().unwrap_or_default(),
            ],
            true,
        ),
        None => {
            let (tn, _tv) = r
                .top
                .as_ref()
                .map(|(n, v)| (crate::sanitize_text(n), *v))
                .unwrap_or(("-".to_string(), 0.0));
            let host = crate::sanitize_text(&r.hostname);
            (
                vec![
                    if selected {
                        format!("❯ {host}")
                    } else {
                        host
                    },
                    crate::fmt_human(r.rx),
                    crate::fmt_human(r.tx),
                    r.procs.to_string(),
                    r.conns.to_string(),
                    r.latency_ms
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "-".into()),
                    tn,
                ],
                false,
            )
        }
    }
}

/// 每帧绘制。调用方已持主题(palette 全局)。
pub fn draw(f: &mut Frame, view: &UiView, ui: &mut UiState, zh: bool) {
    let area = f.area();
    let pal = crate::theme::palette();
    let text = Style::new().fg(pal.text);
    let dim = crate::theme::dim();
    let title_st = Style::new().fg(pal.title).add_modifier(Modifier::BOLD);
    let accent_st = Style::new().fg(pal.accent).add_modifier(Modifier::BOLD);
    let hi_st = Style::new().fg(pal.hi);

    let up = view
        .order
        .iter()
        .filter(|t| {
            view.samples
                .get(*t)
                .map(|s| s.error.is_none())
                .unwrap_or(false)
        })
        .count();

    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .split(area);

    // 头行
    let header = Line::from(Span::styled(
        header_text(view.order.len(), up, view.round, zh),
        title_st,
    ));
    Paragraph::new(header).render(chunks[0], f.buffer_mut());

    // Σ 两行走线:label + sparkline + 当前值;峰值归一到 0..=100
    ui.update_peaks(
        view.round,
        view.sum_hist.last().map(|s| s.0).unwrap_or(0.0),
        view.sum_hist.last().map(|s| s.1).unwrap_or(0.0),
    );
    let spark_w = (chunks[1].width.saturating_sub(24)).max(8) as usize;
    for (row, idx, grad) in [(1usize, 0usize, pal.download.0[0]), (2, 1, pal.upload.0[2])] {
        let (label, cur) = if idx == 0 {
            ("Σ rx ", view.sum_hist.last().map(|s| s.0).unwrap_or(0.0))
        } else {
            ("Σ tx ", view.sum_hist.last().map(|s| s.1).unwrap_or(0.0))
        };
        let peak = if idx == 0 { ui.peak.0 } else { ui.peak.1 };
        let bars: Vec<u64> = view
            .sum_hist
            .iter()
            .map(|s| if idx == 0 { s.0 } else { s.1 })
            .map(|v| ((v / peak) * 100.0).clamp(0.0, 100.0) as u64)
            .collect();
        Paragraph::new(Line::from(Span::styled(label.to_string(), dim))).render(
            Rect {
                x: chunks[row].x,
                y: chunks[row].y,
                width: 5,
                height: 1,
            },
            f.buffer_mut(),
        );
        // sparkline 画在中段:用内嵌 rect
        let inner = Rect {
            x: chunks[row].x + 5,
            y: chunks[row].y,
            width: spark_w as u16,
            height: 1,
        };
        Sparkline::default()
            .data(&bars)
            .style(Style::new().fg(grad))
            .render(inner, f.buffer_mut());
        let val_x = inner.x + inner.width + 1;
        let val = Span::styled(
            format!("{}/s", crate::fmt_human(cur)),
            if idx == 0 { accent_st } else { hi_st },
        );
        Paragraph::new(Line::from(val)).render(
            Rect {
                x: val_x,
                y: chunks[row].y,
                width: chunks[row]
                    .width
                    .saturating_sub((val_x - chunks[row].x) + 1),
                height: 1,
            },
            f.buffer_mut(),
        );
    }

    // 表
    let header_cells = ["node", "rx", "tx", "procs", "conns", "ms", "top talker"]
        .iter()
        .map(|h| Span::styled((*h).to_string(), dim))
        .collect::<Vec<_>>();
    let widths = [
        Constraint::Min(12),
        Constraint::Length(9),
        Constraint::Length(9),
        Constraint::Length(6),
        Constraint::Length(6),
        Constraint::Length(6),
        Constraint::Min(8),
    ];
    let mut rows: Vec<Row> = Vec::new();
    for (i, target) in view.order.iter().enumerate() {
        let Some(s) = view.samples.get(target) else {
            continue;
        };
        let (cells, is_dim) = row_cells(s, i == ui.selected, zh);
        let style = if is_dim {
            dim
        } else if i == ui.selected {
            accent_st
        } else {
            text
        };
        rows.push(Row::new(cells.into_iter().map(|c| Span::styled(c, style))));
    }
    let table = Table::new(rows, widths)
        .header(Row::new(header_cells))
        .block(Block::default().borders(Borders::TOP));
    table.render(chunks[3], f.buffer_mut());

    // 尾行
    Paragraph::new(Line::from(Span::styled(footer_text(zh), dim)))
        .render(chunks[4], f.buffer_mut());

    // 弹窗(帮助 / 详情)
    if ui.help {
        let rect = popup_rect(area, 60, 60);
        let lines: Vec<Line> = help_lines(zh)
            .into_iter()
            .map(|l| Line::from(Span::styled(l, text)))
            .collect();
        Clear.render(rect, f.buffer_mut());
        Paragraph::new(lines)
            .block(popup_block(&crate::pick_s(zh, "help", "帮助")))
            .render(rect, f.buffer_mut());
    } else if let Some(target) = &ui.detail {
        let rect = popup_rect(area, 70, 70);
        let lines: Vec<Line> = detail_lines(target, view.raws.get(target), zh)
            .into_iter()
            .map(|l| Line::from(Span::styled(l, text)))
            .collect();
        let title = view
            .raws
            .get(target)
            .map(|s| crate::sanitize_text(&s.meta.hostname))
            .unwrap_or_else(|| crate::sanitize_text(target));
        Clear.render(rect, f.buffer_mut());
        Paragraph::new(lines)
            .scroll((ui.detail_scroll, 0))
            .block(popup_block(&title))
            .render(rect, f.buffer_mut());
    }
}

fn popup_block(title: &str) -> Block<'static> {
    let pal = crate::theme::palette();
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(pal.dim))
        .border_set(symbols::border::ROUNDED)
        .title(Span::styled(
            format!(" {title} "),
            Style::new().fg(pal.title).add_modifier(Modifier::BOLD),
        ))
        .style(Style::new().bg(pal.bg.unwrap_or(ratatui::style::Color::Reset)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::*;

    fn snap_with_host(host: &str, proc_name: &str) -> Snapshot {
        let mut s = super::super::model::fixture::snap(host, 100.0, 50.0);
        if let Some(p) = s.procs.first_mut() {
            p.name = proc_name.to_string();
        }
        s
    }

    #[test]
    fn damp_rises_instantly_decays_slowly() {
        let mut p = 10.0;
        p = damp(p, 100.0);
        assert_eq!(p, 100.0, "尖峰立即跟涨");
        for _ in 0..10 {
            p = damp(p, 0.0);
        }
        assert!(p < 100.0 && p > 60.0, "×0.97/轮缓落,10 轮 ≈ 74: {p}");
        let z = damp(0.0, 0.0);
        assert!(z >= 1.0, "下限防零除: {z}");
    }

    #[test]
    fn header_and_footer_bilingual() {
        assert_eq!(
            header_text(3, 2, 7, false),
            " fleet — 3 nodes · 2 up · round 7 "
        );
        assert!(header_text(3, 2, 7, true).contains("3 节点"));
        assert!(footer_text(true).contains("退出"));
        assert!(footer_text(false).contains("quit"));
    }

    #[test]
    fn row_cells_sanitizes_and_shapes_error_rows() {
        let mut s = snap_with_host("\x1b]0;evil\x07node", "\x1b[31msneaky\x1b[0m");
        if let Some(p) = s.procs.first_mut() {
            p.name = "\x1b[31msneaky\x1b[0m".into();
        }
        let n = super::super::model::aggregate("t", &s, 5);
        let (cells, dim) = row_cells(&n, false, false);
        assert!(!dim);
        let joined = cells.join(" ");
        assert!(!joined.contains('\x1b'), "控制符必须剥除: {joined:?}");
        assert!(cells[0].contains("evilnode"));

        let bad = super::super::model::failed("nas", "pull timed out".into());
        let (cells, dim) = row_cells(&bad, false, false);
        assert!(dim, "错误行走 dim");
        assert_eq!(cells[1], "⚠");
        assert_eq!(cells[6], "pull timed out", "错误话术落在 top talker 列");

        let (cells, _) = row_cells(&n, true, false);
        assert!(cells[0].starts_with("❯ "), "选中行加标记");
    }

    #[test]
    fn detail_lines_show_snapshot_facts_sanitized() {
        let mut s = snap_with_host("air", "chrome");
        s.meta.os = "\x1bmacos".into();
        let lines = detail_lines("192.168.1.2", Some(&s), false);
        let all = lines.join("\n");
        assert!(!all.contains('\x1b'));
        assert!(all.contains("air"));
        assert!(all.contains("192.168.1.2"));
        assert!(all.contains("chrome"));
        assert!(all.contains("subnets"));
        assert!(all.contains("session read"));

        let empty = detail_lines("nas", None, true);
        assert!(empty[0].contains("成功轮次") || empty[0].contains("waiting"));
    }

    #[test]
    fn help_lines_cover_keys() {
        for zh in [true, false] {
            let l = help_lines(zh);
            let all = l.join("\n");
            assert!(all.contains('q'));
            assert!(all.contains("fleet setup"));
        }
    }

    #[test]
    fn popup_rect_stays_inside() {
        let area = Rect::new(0, 0, 100, 40);
        let r = popup_rect(area, 70, 70);
        assert!(r.x > 0 && r.width < area.width);
        assert!(r.y > 0 && r.height < area.height);
        assert!(r.right() <= area.right());
    }

    #[test]
    fn ui_state_peaks_advance_only_on_round_change() {
        let mut ui = UiState::new();
        ui.update_peaks(1, 100.0, 50.0);
        assert_eq!(ui.peak, (100.0, 50.0));
        // 同轮重复调用不得继续衰减(每帧绘制不衰减,只按轮)
        ui.update_peaks(1, 100.0, 50.0);
        assert_eq!(ui.peak, (100.0, 50.0));
        ui.update_peaks(2, 10.0, 5.0);
        assert!(ui.peak.0 < 100.0, "新轮次才衰减");
    }
}

use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::text::Span;

use crate::theme::Gradient;

/// Graph render styles
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum GraphMode {
    /// btop's braille area graph (5x5 lookup, two samples per char)
    Solid,
    /// bars growing out from the vertical center (ltop extension)
    Centered,
    /// single-dot trace at the value's height (ltop extension)
    Line,
    /// oscilloscope-style connected trace (ltop extension)
    Heartbeat,
    /// line + soft fill below (ltop extension)
    Hybrid,
    /// seamless half-block bars (ltop extension): 8 levels per character,
    /// immune to the per-glyph gaps braille fonts can show
    Block,
}

impl GraphMode {
    pub fn next(self) -> Self {
        match self {
            Self::Solid => Self::Centered,
            Self::Centered => Self::Line,
            Self::Line => Self::Heartbeat,
            Self::Heartbeat => Self::Hybrid,
            Self::Hybrid => Self::Solid,
            // Block 不参与循环:只能经 graphs 菜单的 next_with_block 选择
            Self::Block => Self::Solid,
        }
    }

    /// graphs 菜单的反向循环
    pub fn prev_with_block(self) -> Self {
        match self {
            Self::Solid => Self::Block,
            Self::Centered => Self::Solid,
            Self::Line => Self::Centered,
            Self::Heartbeat => Self::Line,
            Self::Hybrid => Self::Heartbeat,
            Self::Block => Self::Hybrid,
        }
    }

    /// graphs 菜单的循环:包含 Block(n 键循环不含——block 为显式选择项)
    pub fn next_with_block(self) -> Self {
        match self {
            Self::Hybrid => Self::Block,
            other => other.next(),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Solid => "bar",
            Self::Centered => "center",
            Self::Line => "line",
            Self::Heartbeat => "heart",
            Self::Hybrid => "hybrid",
            Self::Block => "block",
        }
    }
}

/// btop's braille_up table: [left column height 0-4][right column height 0-4]
/// (verbatim from aristocratos/btop src/btop_draw.cpp Symbols::graph_symbols)
const BRAILLE_UP: [[char; 5]; 5] = [
    [' ', '⢀', '⢠', '⢰', '⢸'],
    ['⡀', '⣀', '⣠', '⣰', '⣸'],
    ['⡄', '⣄', '⣤', '⣴', '⣼'],
    ['⡆', '⣆', '⣦', '⣶', '⣾'],
    ['⡇', '⣇', '⣧', '⣷', '⣿'],
];

fn braille_bits(c: char) -> u32 {
    if c == ' ' {
        0
    } else {
        c as u32 - 0x2800
    }
}

/// Horizontal dot-matrix progress bar (one text line tall): each braille
/// char packs two dot columns, a filled column is a full 4-dot strip — same
/// texture as the graphs. `frac` 0..1.
pub fn braille_bar(frac: f64, width: usize, color: Color) -> Span<'static> {
    let frac = frac.clamp(0.0, 1.0);
    let cols = width * 2;
    let filled = (frac * cols as f64).round() as usize;
    const L: u32 = 0x01 | 0x02 | 0x04 | 0x40;
    const R: u32 = 0x08 | 0x10 | 0x20 | 0x80;
    let mut s = String::with_capacity(width);
    for i in 0..width {
        let mut bits = 0u32;
        if filled > 2 * i {
            bits |= L;
        }
        if filled > 2 * i + 1 {
            bits |= R;
        }
        s.push(char::from_u32(0x2800 + bits).unwrap_or(' '));
    }
    Span::styled(s, Style::new().fg(color))
}

/// Braille graph. Solid mode is a faithful port of btop's `Graph::_create`:
/// one braille char per sample encoding the previous and current value as
/// left/right column heights, values normalized to 0-100 and quantized into
/// per-row bands (round + 0.1 jitter for the btop texture), bottom row of a
/// non-zero sample kept at >= 1 dot. Rows are colored along the gradient from
/// top (end) to bottom (start).
///
/// Centered / Line / Heartbeat are ltop extensions rendered on a dot grid.
pub fn braille_graph(
    data: &[f64],
    width: usize,
    height: usize,
    max_floor: f64,
    grad: Gradient,
    mode: GraphMode,
    scale_frac: f64,
) -> Vec<Line<'static>> {
    let width = width.max(1);
    let height = height.max(1);
    let dots_h = height * 4;

    // one sample per column, btop-style sliding window: take the suffix of
    // the history at exactly `width` samples (never resample — bucketing
    // would shift every column each tick and make the graph wobble)
    let mut series: Vec<f64> = if data.len() > width {
        data[data.len() - width..].to_vec()
    } else {
        data.to_vec()
    };
    if series.len() < width {
        let mut padded = vec![0.0; width - series.len()];
        padded.extend_from_slice(&series);
        series = padded;
    }
    // coupled scaling: divide the auto peak by frac (≤1) so the small side
    // of a lopsided link renders at its true fraction of the chart height
    let max = series.iter().copied().fold(0.0f64, f64::max).max(max_floor)
        / scale_frac.max(0.05);
    if mode == GraphMode::Block {
        return block_graph(&series, width, height, max, grad);
    }
    let pct: Vec<f64> = series.iter().map(|v| v.max(0.0) / max * 100.0).collect();
    let fills: Vec<usize> = series
        .iter()
        .map(|v| ((v.max(0.0) / max * dots_h as f64).round() as usize).min(dots_h))
        .collect();

    let rows: Vec<Vec<u32>> = match mode {
        GraphMode::Solid => {
            let mut rows = vec![vec![0u32; width]; height];
            for (l, row) in rows.iter_mut().enumerate() {
                let (cur_high, cur_low) = if height > 1 {
                    (
                        (100.0 * (height - l) as f64 / height as f64).round(),
                        (100.0 * (height - l - 1) as f64 / height as f64).round(),
                    )
                } else {
                    (100.0, 0.0)
                };
                let m: f64 = if height == 1 { 0.3 } else { 0.1 };
                for c in 0..width {
                    let left = if c > 0 { pct[c - 1] } else { 0.0 };
                    let mut res = [0usize; 2];
                    for (ai, &v) in [left, pct[c]].iter().enumerate() {
                        // btop no_zero: floor applies everywhere except the very
                        // first char's left column (the drawing baseline)
                        let clamp_min = if l == height - 1 && !(c == 0 && ai == 0) {
                            1
                        } else {
                            0
                        };
                        res[ai] = if v >= cur_high {
                            4
                        } else if v <= cur_low {
                            clamp_min
                        } else {
                            ((((v - cur_low) * 4.0 / (cur_high - cur_low)) + m).round() as usize)
                                .clamp(clamp_min, 4)
                        };
                    }
                    row[c] = braille_bits(BRAILLE_UP[res[0]][res[1]]);
                }
            }
            rows
        }
        _ => {
            // line-based modes on a 2x sub-column dot grid: sample i lives in
            // the right half of char i, segments interpolate between samples
            let dw = width * 2;
            let mut grid = vec![vec![false; dots_h]; dw];
            let mid = dots_h / 2;
            let trace_y = |f: usize| -> usize {
                ((f as f32 / dots_h as f32) * (dots_h.saturating_sub(1)) as f32).round() as usize
            };
            match mode {
                GraphMode::Centered => {
                    // two-dot zero axis at the middle, always visible
                    let bl0 = mid.saturating_sub(1);
                    let bl1 = mid.min(dots_h - 1);
                    for (c, &f) in fills.iter().enumerate() {
                        let start = bl0.saturating_sub(f / 2);
                        let end = (bl1 + 1 + f.div_ceil(2)).min(dots_h);
                        let (lo, hi) = grid.split_at_mut(2 * c + 1);
                        let (col0, col1) = (&mut lo[2 * c], &mut hi[0]);
                        for (a, b) in col0.iter_mut().zip(col1.iter_mut()).take(end).skip(start) {
                            *a = true;
                            *b = true;
                        }
                    }
                }
                GraphMode::Hybrid => {
                    // line stroke + soft area fill below it
                    let mut line = vec![vec![false; dots_h]; dw];
                    let mut prev: Option<(usize, usize)> = None;
                    let mut last = 0usize;
                    for (i, &f) in fills.iter().enumerate() {
                        let x = 2 * i + 1;
                        let y = trace_y(f).min(dots_h - 1);
                        if let Some((px, py)) = prev {
                            for (xx, col) in line.iter_mut().enumerate().take(x + 1).skip(px + 1) {
                                let t = (xx - px) as f32 / (x - px) as f32;
                                let yy = (py as f32 + (y as f32 - py as f32) * t).round() as usize;
                                for y2 in last.min(yy)..=last.max(yy) {
                                    col[y2.min(dots_h - 1)] = true;
                                }
                                last = yy;
                            }
                        }
                        line[x][y] = true;
                        if y + 1 < dots_h {
                            line[x][y + 1] = true;
                        }
                        prev = Some((x, y));
                    }
                    for dc in 0..dw {
                        // same-color area fill below the line stroke
                        if let Some(top) = (0..dots_h).rev().find(|&y| line[dc][y]) {
                            for cell in grid[dc].iter_mut().take(top + 1) {
                                *cell = true;
                            }
                        }
                    }
                }
                GraphMode::Line | GraphMode::Heartbeat => {
                    // connected strokes, 2 dots thick, no holes on steep slopes
                    fn dot(grid: &mut [Vec<bool>], x: usize, y: usize, dots_h: usize) {
                        let x = x.min(grid.len() - 1);
                        grid[x][y.min(dots_h - 1)] = true;
                        if y + 1 < dots_h {
                            grid[x][y + 1] = true;
                        }
                    }
                    let mut prev: Option<(usize, usize)> = None;
                    for (i, &f) in fills.iter().enumerate() {
                        let x = 2 * i + 1;
                        let y = trace_y(f).min(dots_h - 1);
                        if let Some((px, py)) = prev {
                            let steps = (x - px).max(1);
                            let mut last = py;
                            for s in 1..=steps {
                                let xx = px + s;
                                let t = s as f32 / steps as f32;
                                let yy = (py as f32 + (y as f32 - py as f32) * t).round() as usize;
                                match mode {
                                    GraphMode::Line => {
                                        for y2 in last.min(yy)..=last.max(yy) {
                                            dot(&mut grid, xx, y2, dots_h);
                                        }
                                    }
                                    GraphMode::Heartbeat => {
                                        if s < steps {
                                            dot(&mut grid, xx, py, dots_h);
                                        } else {
                                            for y2 in y.min(py)..=y.max(py) {
                                                dot(&mut grid, xx, y2, dots_h);
                                            }
                                        }
                                    }
                                    _ => {}
                                }
                                last = yy;
                            }
                        }
                        dot(&mut grid, x, y, dots_h);
                        prev = Some((x, y));
                    }
                }
                GraphMode::Solid | GraphMode::Block => unreachable!(),
            }
            // braille dot bits: char c = dot columns 2c (x0) + 2c+1 (x1)
            const X0: [u32; 4] = [0x01, 0x02, 0x04, 0x40];
            const X1: [u32; 4] = [0x08, 0x10, 0x20, 0x80];
            let mut rows = vec![vec![0u32; width]; height];
            for (l, row) in rows.iter_mut().enumerate() {
                for (c, cell) in row.iter_mut().enumerate() {
                    let mut bits = 0u32;
                    for j in 0..4 {
                        let y_from_bottom = dots_h - 1 - (l * 4 + j);
                        if grid[2 * c][y_from_bottom] {
                            bits |= X0[j];
                        }
                        if grid[2 * c + 1][y_from_bottom] {
                            bits |= X1[j];
                        }
                    }
                    *cell = bits;
                }
            }
            rows
        }
    };

    let mut out = Vec::with_capacity(height);
    for (l, row) in rows.iter().enumerate() {
        let t = if height <= 1 {
            0.5
        } else {
            l as f32 / (height - 1) as f32
        };
        let color = grad.at(t);
        let s: String = row
            .iter()
            .map(|&b| char::from_u32(0x2800 + b).unwrap_or(' '))
            .collect();
        out.push(Line::from(Span::styled(s, Style::new().fg(color))));
    }
    out
}

/// Half-block bar graph (GraphMode::Block): one character per sample per
/// row, 8 height levels via the vertical block set. No braille glyphs —
/// immune to the per-glyph gaps that terminal fonts render around braille.
/// coupled net-graph scaling: when one direction is `threshold`× or more
/// smaller than the other, the small side renders at its true fraction of
/// the chart height (clamped to a readable floor that loosens on taller
/// charts) instead of auto-scaling to full height. 1.0 = normal behavior.
pub fn coupled_frac(small: f64, large: f64, rows: u16, threshold: u32) -> f64 {
    if threshold == 0 || !large.is_finite() || !small.is_finite() || large <= 0.0 || small <= 0.0 {
        return 1.0;
    }
    let ratio = small / large;
    if ratio >= 1.0 || ratio * threshold as f64 >= 1.0 {
        return 1.0;
    }
    let floor = if rows < 12 {
        0.5
    } else if rows <= 24 {
        0.4
    } else {
        0.3
    };
    ratio.max(floor)
}

fn block_graph(
    series: &[f64],
    width: usize,
    height: usize,
    max: f64,
    grad: Gradient,
) -> Vec<Line<'static>> {
    const BLOCKS: [char; 9] = [
        ' ', '\u{2581}', '\u{2582}', '\u{2583}', '\u{2584}', '\u{2585}', '\u{2586}', '\u{2587}',
        '\u{2588}',
    ];
    let blocks_h = height * 8;
    let fills: Vec<usize> = series
        .iter()
        .map(|v| ((v.max(0.0) / max * blocks_h as f64).round() as usize).min(blocks_h))
        .collect();
    let mut out = Vec::with_capacity(height);
    for l in 0..height {
        let t = if height <= 1 {
            0.5
        } else {
            l as f32 / (height - 1) as f32
        };
        let color = grad.at(t);
        let k = height - 1 - l; // 第 k 个字符(0 = 最底)
        let s: String = (0..width)
            .map(|c| {
                let fill = fills[c];
                let idx = fill.saturating_sub(k * 8).min(8);
                BLOCKS[idx]
            })
            .collect();
        out.push(Line::from(Span::styled(s, Style::new().fg(color))));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    /// flatten a rendered row across its color spans
    fn flat(l: &Line) -> String {
        l.spans.iter().map(|s| s.content.clone()).collect()
    }

    #[test]
    #[test]
    fn coupled_frac_thresholds_and_floors() {
        // below the trigger threshold: real fraction, clamped to the floor
        assert_eq!(coupled_frac(1.0, 30.0, 8, 5), 0.5); // 1/30 < 1/5 → floor
        assert_eq!(coupled_frac(1.0, 10.0, 30, 5), 0.3); // tall chart → 0.3
        // not below threshold: untouched
        assert_eq!(coupled_frac(1.0, 3.0, 8, 5), 1.0); // 1/3 > 1/5
        assert_eq!(coupled_frac(1.0, 1.0, 8, 5), 1.0); // equal
        assert_eq!(coupled_frac(1.0, 5.0, 8, 5), 1.0); // right at boundary
        // disabled / degenerate inputs
        assert_eq!(coupled_frac(1.0, 30.0, 8, 0), 1.0);
        assert_eq!(coupled_frac(0.0, 30.0, 8, 5), 1.0);
        assert_eq!(coupled_frac(1.0, 0.0, 8, 5), 1.0);
        assert_eq!(coupled_frac(1.0, 7.0, 8, 10), 1.0); // 1/7 ≥ 1/10
        assert_eq!(coupled_frac(1.0, 15.0, 8, 10), 0.5); // 1/15 < 1/10 → floor
    }

    fn solid_full_block_at_max() {
        let data = vec![100.0; 10];
        let lines = braille_graph(
            &data,
            10,
            1,
            1.0,
            Gradient([Color::Green; 3]),
            GraphMode::Solid,
            1.0,
        );
        assert_eq!(lines.len(), 1);
        let text = flat(&lines[0]);
        // first char: left column is the baseline (0) => table[0][4] = ⢸;
        // from the second char on both columns are maxed => table[4][4] = ⣿
        assert_eq!(text.chars().next(), Some('\u{28b8}'));
        assert_eq!(text.chars().nth(1), Some('\u{28ff}'));
        assert_eq!(text.chars().count(), 10);
    }

    #[test]
    fn solid_keeps_nonzero_off_the_floor() {
        // tiny values above the graph floor: bottom row keeps >= 1 dot (no_zero)
        let data = vec![10.0; 6];
        let lines = braille_graph(
            &data,
            6,
            2,
            64.0 * 1024.0,
            Gradient([Color::Green; 3]),
            GraphMode::Solid,
            1.0,
        );
        let bottom = flat(&lines[1]);
        let chars: Vec<char> = bottom.chars().collect();
        assert_eq!(chars[0], '⢀'); // baseline left 0, right quantizes to 1 dot
        assert_eq!(chars[2], '⣀'); // both columns at the 1-dot floor
    }

    #[test]
    fn solid_stable_when_history_exceeds_width() {
        // once the history is longer than the graph, rendering must be a pure
        // suffix window: identical to rendering just the last `width` samples,
        // so the shape never wobbles as the window slides
        let width = 20;
        let data: Vec<f64> = (0..width + 37)
            .map(|i| if i % 17 == 0 { 900_000.0 } else { 1200.0 })
            .collect();
        let grad = Gradient([Color::Green; 3]);
        let full = braille_graph(&data, width, 3, 64.0 * 1024.0, grad, GraphMode::Solid, 1.0);
        let suffix = braille_graph(
            &data[data.len() - width..],
            width,
            3,
            64.0 * 1024.0,
            grad,
            GraphMode::Solid,
            1.0,
        );
        for (a, b) in full.iter().zip(&suffix) {
            assert_eq!(a.spans[0].content, b.spans[0].content);
        }
    }

    #[test]
    fn hybrid_fills_under_the_line() {
        let grad = Gradient([Color::Green; 3]);
        let lines = braille_graph(&[0.0, 100.0], 2, 2, 1.0, grad, GraphMode::Hybrid, 1.0);
        // area under the interpolated line: col3 top half carries the line
        // segment (⣸), the bottom row is fully filled (⣿)
        assert_eq!(flat(&lines[0]).chars().nth(1), Some('\u{28f8}'));
        assert_eq!(flat(&lines[1]).chars().nth(1), Some('\u{28ff}'));
        // zero-data hybrid: a continuous floor line (interpolation keeps it
        // connected through every sub-column)
        let lines = braille_graph(&[0.0; 4], 4, 2, 1.0, grad, GraphMode::Hybrid, 1.0);
        let chars: Vec<char> = flat(&lines[1]).chars().collect();
        assert_eq!(chars[0], '\u{28a0}');
        assert_eq!(chars[1], '\u{28e0}');
        assert!(chars.iter().all(|&c| c != ' '));
    }

    #[test]
    fn braille_bar_partial_fill() {
        let green = Color::Green;
        // 25% of 2 chars = exactly one dot column => left col of char 0 only
        let bar = braille_bar(0.25, 2, green);
        assert_eq!(bar.content, "\u{2847}\u{2800}");
        assert_eq!(braille_bar(1.0, 2, green).content, "\u{28ff}\u{28ff}");
        assert_eq!(braille_bar(0.0, 2, green).content, "\u{2800}\u{2800}");
    }

    #[test]
    fn left_pads_short_data() {
        let lines = braille_graph(
            &[50.0],
            4,
            2,
            1.0,
            Gradient([Color::Green; 3]),
            GraphMode::Solid,
            1.0,
        );
        assert_eq!(lines.len(), 2);
        assert_eq!(flat(&lines[0]).chars().count(), 4);
    }

    #[test]
    fn centered_fill_spans_the_middle() {
        let data = vec![100.0; 4];
        let lines = braille_graph(
            &data,
            4,
            1,
            1.0,
            Gradient([Color::Green; 3]),
            GraphMode::Centered,
            1.0,
        );
        // dots_h=4, mid=2, f=4 => rows 0..4 filled => full block
        assert_eq!(flat(&lines[0]).chars().next(), Some('\u{28ff}'));
    }

    #[test]
    fn line_mode_draws_connected_polyline() {
        let data = vec![0.0, 100.0];
        let lines = braille_graph(
            &data,
            2,
            2,
            1.0,
            Gradient([Color::Green; 3]),
            GraphMode::Line,
            1.0,
        );
        // 2-dot stroke: col1 {0,1}; segment to col3 tops out via col2 {4,5},
        // col3 {4..7} (vertical connect) => continuous, no holes
        let row_bottom = flat(&lines[1]);
        let row_top = flat(&lines[0]);
        assert_eq!(row_bottom.chars().next(), Some('\u{28a0}'));
        assert_eq!(row_top.chars().nth(1), Some('\u{28fc}'));
    }

    #[test]
    fn heartbeat_sweeps_flat_then_jumps() {
        let data = vec![0.0, 100.0];
        let lines = braille_graph(
            &data,
            2,
            2,
            1.0,
            Gradient([Color::Green; 3]),
            GraphMode::Heartbeat,
            1.0,
        );
        // 2-dot flat run at y=0..1 through col2; jump at col3 fills all rows
        let row_top = flat(&lines[0]);
        let row_bottom = flat(&lines[1]);
        assert_eq!(row_top.chars().nth(1), Some('\u{28b8}')); // col3: full right col
        assert_eq!(row_bottom.chars().nth(1), Some('\u{28fc}')); // flat + jump bottom
    }
}

#[cfg(test)]
mod block_tests {
    use super::*;
    use ratatui::style::Color;

    fn flat(l: &Line) -> String {
        l.spans.iter().map(|s| s.content.clone()).collect()
    }

    #[test]
    fn block_graph_height_levels() {
        // 全 100:每列填满所有行,底行全 █
        let data = vec![100.0; 6];
        let lines = block_graph(&data, 6, 2, 100.0, Gradient([Color::Green; 3]));
        assert_eq!(lines.len(), 2);
        assert_eq!(flat(&lines[1]), "██████");
        // 顶行:fill=16(k=1 时 16-8=8 → 全 █)
        assert_eq!(flat(&lines[0]), "██████");
    }

    #[test]
    fn block_graph_partial_levels() {
        // 单行、值 50(4/8):▄
        let data = vec![50.0; 4];
        let lines = block_graph(&data, 4, 1, 100.0, Gradient([Color::Green; 3]));
        assert_eq!(flat(&lines[0]), "▄▄▄▄");
        // 值 0:空
        let zero = vec![0.0; 4];
        let lines = block_graph(&zero, 4, 1, 100.0, Gradient([Color::Green; 3]));
        assert_eq!(flat(&lines[0]), "    ");
    }
}

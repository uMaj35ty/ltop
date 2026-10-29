use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, RwLock};
use std::time::SystemTime;

use ratatui::style::{Color, Modifier, Style};

use crate::theme_data;

/// A three-stop gradient like btop theme's *_start/_mid/_end colors
#[derive(Clone, Copy)]
pub struct Gradient(pub [Color; 3]);

impl Gradient {
    pub fn at(self, t: f32) -> Color {
        let t = t.clamp(0.0, 1.0);
        let [a, b, c] = self.0;
        if t < 0.5 {
            lerp(a, b, t * 2.0)
        } else {
            lerp(b, c, (t - 0.5) * 2.0)
        }
    }
}

fn lerp(a: Color, b: Color, t: f32) -> Color {
    match (a, b) {
        (Color::Rgb(r1, g1, b1), Color::Rgb(r2, g2, b2)) => Color::Rgb(
            (r1 as f32 + (r2 as f32 - r1 as f32) * t) as u8,
            (g1 as f32 + (g2 as f32 - g1 as f32) * t) as u8,
            (b1 as f32 + (b2 as f32 - b1 as f32) * t) as u8,
        ),
        _ => {
            if t < 0.5 {
                a
            } else {
                b
            }
        }
    }
}

/// Semantic color roles, loaded from an Omarchy/btop theme file
pub struct Palette {
    /// None = terminal default (btop convention: empty main_bg = transparent)
    pub bg: Option<Color>,
    pub text: Color,
    pub title: Color,
    /// highlight color for keyboard shortcuts (btop hi_fg)
    pub hi: Color,
    /// inactive/disabled text (btop inactive_fg)
    pub dim: Color,
    /// bright accents: values, IPs, clock (btop graph_text)
    pub accent: Color,
    pub border_net: Color,
    pub border_lan: Color,
    pub border_disk: Color,
    pub download: Gradient,
    pub upload: Gradient,
    /// mem/disk used meter gradient
    pub used: Gradient,
    /// alert scale green->yellow->red role (btop temp gradient)
    pub alert: Gradient,
}

impl Default for Palette {
    fn default() -> Self {
        // built-in Dracula fallback
        const GREEN: Color = Color::Rgb(0x50, 0xfa, 0x7b);
        const YELLOW: Color = Color::Rgb(0xf1, 0xfa, 0x8c);
        const ORANGE: Color = Color::Rgb(0xff, 0xb8, 0x6c);
        const RED: Color = Color::Rgb(0xff, 0x55, 0x55);
        Self {
            bg: Some(Color::Rgb(0x28, 0x2a, 0x36)),
            text: Color::Rgb(0xf8, 0xf8, 0xf2),
            title: Color::Rgb(0xbd, 0x93, 0xf9),
            hi: Color::Rgb(0xbd, 0x93, 0xf9),
            dim: Color::Rgb(0x62, 0x72, 0xa4),
            accent: Color::Rgb(0x8b, 0xe9, 0xfd),
            border_net: Color::Rgb(0x62, 0x72, 0xa4),
            border_lan: Color::Rgb(0x62, 0x72, 0xa4),
            border_disk: Color::Rgb(0x62, 0x72, 0xa4),
            download: Gradient([GREEN, GREEN, YELLOW]),
            upload: Gradient([YELLOW, YELLOW, ORANGE]),
            used: Gradient([GREEN, YELLOW, RED]),
            alert: Gradient([YELLOW, ORANGE, RED]),
        }
    }
}

/// WCAG large-text floor enforced on the dim role. btop's inactive_fg is
/// decorative by convention, but ltop renders load-bearing text with it
/// (table headers, session labels, tips) — 57 of 72 built-in themes ship
/// an inactive_fg below 3:1 against main_bg, three of them literally
/// equal to the background (invisible).
const DIM_CONTRAST_FLOOR: f32 = 3.0;
/// Blending is capped short of the text color so a lifted dim never
/// becomes indistinguishable from regular text (hierarchy is preserved).
const DIM_BLEND_CAP: f32 = 0.8;

fn rel_lum(c: Color) -> f32 {
    let Color::Rgb(r, g, b) = c else {
        return 0.5;
    };
    // WCAG 2.x sRGB linearization; the u8 branch is precomputed to keep
    // this cheap enough for the render path if it ever moves there
    fn lin(v: u8) -> f32 {
        let v = f32::from(v) / 255.0;
        if v <= 0.03928 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
}

pub(crate) fn contrast_ratio(a: Color, b: Color) -> f32 {
    let (la, lb) = (rel_lum(a), rel_lum(b));
    let (hi, lo) = (la.max(lb), la.min(lb));
    (hi + 0.05) / (lo + 0.05)
}

fn mix_rgb(a: Color, b: Color, t: f32) -> Option<Color> {
    let (Color::Rgb(r1, g1, b1), Color::Rgb(r2, g2, b2)) = (a, b) else {
        return None;
    };
    let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Some(Color::Rgb(mix(r1, r2), mix(g1, g2), mix(b1, b2)))
}

/// Lift `dim` toward `text` until it reaches [`DIM_CONTRAST_FLOOR`] against
/// `bg`. Transparent backgrounds (None) can't be measured, and non-Rgb
/// colors (ANSI/named) aren't blended — both pass through unchanged.
pub(crate) fn lift_dim(dim: Color, bg: Option<Color>, text: Color) -> Color {
    let Some(bg) = bg else {
        return dim;
    };
    if contrast_ratio(dim, bg) >= DIM_CONTRAST_FLOOR {
        return dim;
    }
    let Some(first) = mix_rgb(dim, text, 0.05) else {
        return dim;
    };
    let mut step = first;
    let mut t = 0.05;
    while t < DIM_BLEND_CAP {
        if contrast_ratio(step, bg) >= DIM_CONTRAST_FLOOR {
            return step;
        }
        t += 0.05;
        let Some(next) = mix_rgb(dim, text, t) else {
            break;
        };
        step = next;
    }
    step
}

/// omarchy 跟随模式标记(init/apply_omarchy 置位,apply_builtin 清除)
static FOLLOWING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 跟随模式下已加载的主题文件与加载时的 mtime:poll_omarchy_follow 据此
/// 发现 omarchy-theme-set 重写了文件并热重载
static FOLLOWED: Mutex<Option<(PathBuf, SystemTime)>> = Mutex::new(None);

fn record_followed(p: &Path) {
    if let Ok(mt) = std::fs::metadata(p).and_then(|m| m.modified()) {
        if let Ok(mut g) = FOLLOWED.lock() {
            *g = Some((p.to_path_buf(), mt));
        }
    }
}

fn cells() -> &'static (RwLock<Palette>, RwLock<String>) {
    static CELLS: OnceLock<(RwLock<Palette>, RwLock<String>)> = OnceLock::new();
    CELLS.get_or_init(|| {
        (
            RwLock::new(Palette::default()),
            RwLock::new("dracula".into()),
        )
    })
}

fn set(mut pal: Palette, name: &str) {
    // single choke point: builtins, --theme files and omarchy follow-mode
    // hot reloads all land here, so the dim floor covers every source
    pal.dim = lift_dim(pal.dim, pal.bg, pal.text);
    *cells().0.write().unwrap() = pal;
    *cells().1.write().unwrap() = name.to_string();
}

/// --theme <file> 直载也算 omarchy 跟随?不算:显式指定即固定主题
fn set_following(v: bool) {
    FOLLOWING.store(v, std::sync::atomic::Ordering::Relaxed);
}

/// Label of the active theme (shown in the theme menu)
pub fn theme_name() -> String {
    cells().1.read().unwrap().clone()
}

/// Load theme once at startup: --theme <file> > Omarchy state > btop current.theme.
/// Returns a human-readable description of the source.
pub fn init() -> String {
    if let Some(p) = std::env::args()
        .position(|a| a == "--theme")
        .and_then(|i| std::env::args().nth(i + 1))
        .map(PathBuf::from)
    {
        let src = p.display().to_string();
        if load_file(&p) {
            return src;
        }
    }
    for p in omarchy_candidates() {
        if load_file(&p) {
            set_following(true);
            record_followed(&p);
            return p.display().to_string();
        }
    }
    set(Palette::default(), "dracula");
    "built-in (dracula fallback)".to_string()
}

fn omarchy_candidates() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
        let home = PathBuf::from(home);
        if let Ok(state) = std::env::var("XDG_STATE_HOME") {
            v.push(PathBuf::from(state).join("omarchy/current/theme/btop.theme"));
        }
        v.push(home.join(".local/state/omarchy/current/theme/btop.theme"));
        v.push(home.join(".config/btop/themes/current.theme"));
    }
    v
}

fn load_file(p: &Path) -> bool {
    std::fs::read_to_string(p)
        .ok()
        .and_then(|c| parse_btop_str(&c))
        .map(|pal| {
            let name = if p.to_string_lossy().contains("omarchy") {
                "omarchy".to_string()
            } else {
                p.file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "custom".to_string())
            };
            set(pal, &name);
        })
        .is_some()
}

/// Re-load the Omarchy current theme (theme menu entry "omarchy")
/// 当前是否处于 omarchy 跟随模式(而非固定内嵌主题)
pub fn following_omarchy() -> bool {
    FOLLOWING.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn apply_by_name(name: &str) -> bool {
    (0..builtin_count())
        .find(|i| builtin_name(*i).eq_ignore_ascii_case(name))
        .map(apply_builtin)
        .unwrap_or(false)
}

pub fn apply_omarchy() -> bool {
    for p in omarchy_candidates() {
        if load_file(&p) {
            set_following(true);
            record_followed(&p);
            return true;
        }
    }
    false
}

/// 每 tick 调用:跟随模式下检查 Omarchy 主题文件是否被重写(如
/// omarchy-theme-set),变了就热重载并返回 true,下一帧即新主题。
/// 文件暂时消失(切换中间态)不动主题,下 tick 再看
pub fn poll_omarchy_follow() -> bool {
    if !following_omarchy() {
        return false;
    }
    let Some((path, seen)) = FOLLOWED.lock().ok().and_then(|g| g.clone()) else {
        return false;
    };
    match std::fs::metadata(&path).and_then(|m| m.modified()) {
        Ok(mt) if mt == seen => false,
        Ok(_) => {
            let ok = load_file(&path);
            if ok {
                record_followed(&path);
            }
            ok
        }
        Err(_) => false,
    }
}

pub fn builtin_count() -> usize {
    theme_data::BUILTIN_THEMES.len()
}

pub fn builtin_name(idx: usize) -> &'static str {
    theme_data::BUILTIN_THEMES[idx].0
}

pub fn apply_builtin(idx: usize) -> bool {
    let Some((name, content)) = theme_data::BUILTIN_THEMES.get(idx) else {
        return false;
    };
    set_following(false);
    let Some(pal) = parse_btop_str(content) else {
        return false;
    };
    set(pal, name);
    true
}

pub fn palette() -> std::sync::RwLockReadGuard<'static, Palette> {
    cells().0.read().unwrap()
}

pub(crate) fn parse_btop_str(content: &str) -> Option<Palette> {
    let mut map: HashMap<String, Color> = HashMap::new();
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('#') || !line.starts_with("theme[") {
            continue;
        }
        let Some(kb) = line.find('[') else { continue };
        let Some(ke) = line[kb..].find(']').map(|i| i + kb) else {
            continue;
        };
        let key = &line[kb + 1..ke];
        let Some(qs) = line[ke + 1..].find('"').map(|i| i + ke + 1) else {
            continue;
        };
        let rest = &line[qs + 1..];
        let Some(qe) = rest.find('"') else { continue };
        let Some(color) = parse_hex(&rest[..qe]) else {
            continue;
        };
        map.insert(key.to_string(), color);
    }
    if map.is_empty() {
        return None;
    }

    let mut pal = Palette::default();
    if let Some(c) = map.get("main_bg") {
        pal.bg = Some(*c);
    } else if content.contains("theme[main_bg]") {
        // explicitly empty => terminal default / transparent
        pal.bg = None;
    }
    let set = |dst: &mut Color, key: &str| {
        if let Some(c) = map.get(key) {
            *dst = *c;
        }
    };
    let grad = |a: &str, b: &str, c: &str| -> Option<Gradient> {
        Some(Gradient([*map.get(a)?, *map.get(b)?, *map.get(c)?]))
    };

    set(&mut pal.text, "main_fg");
    set(&mut pal.title, "title");
    set(&mut pal.hi, "hi_fg");
    set(&mut pal.dim, "inactive_fg");
    set(&mut pal.accent, "graph_text");
    set(&mut pal.border_net, "net_box");
    set(&mut pal.border_lan, "proc_box");
    set(&mut pal.border_disk, "mem_box");
    if let Some(g) = grad("download_start", "download_mid", "download_end") {
        pal.download = g;
    }
    if let Some(g) = grad("upload_start", "upload_mid", "upload_end") {
        pal.upload = g;
    }
    if let Some(g) = grad("used_start", "used_mid", "used_end") {
        pal.used = g;
    }
    if let Some(g) = grad("temp_start", "temp_mid", "temp_end") {
        pal.alert = g;
    }
    Some(pal)
}

fn parse_hex(s: &str) -> Option<Color> {
    let s = s.trim().trim_start_matches('#');
    // 主题文件是用户可编辑的:非 ASCII 值按字节 len() 匹配后再定界切片
    // 会落在 char boundary 中间直接 panic(启动早期,panic 钩子尚未安装)
    if !s.is_ascii() {
        return None;
    }
    match s.len() {
        6 => {
            let r = u8::from_str_radix(&s[0..2], 16).ok()?;
            let g = u8::from_str_radix(&s[2..4], 16).ok()?;
            let b = u8::from_str_radix(&s[4..6], 16).ok()?;
            Some(Color::Rgb(r, g, b))
        }
        2 => {
            // btop grayscale shorthand: "#BW" => R=G=B
            let v = u8::from_str_radix(s, 16).ok()?;
            Some(Color::Rgb(v, v, v))
        }
        8 => parse_hex(&s[..6]),
        3 => {
            let expanded: String = s.chars().map(|c| c.to_string().repeat(2)).collect();
            parse_hex(&expanded)
        }
        _ => None,
    }
}

// ---- accessors (palette must be initialized via init() before first draw) ----

pub fn bg() -> Option<Color> {
    palette().bg
}

pub fn bold(color: Color) -> Style {
    Style::new().fg(color).add_modifier(Modifier::BOLD)
}

pub fn dim() -> Style {
    Style::new().fg(palette().dim)
}

pub fn text() -> Style {
    Style::new().fg(palette().text)
}

pub fn border_net() -> Color {
    palette().border_net
}

pub fn border_lan() -> Color {
    palette().border_lan
}

pub fn border_disk() -> Color {
    palette().border_disk
}

pub fn accent() -> Color {
    palette().accent
}

pub fn hi() -> Color {
    palette().hi
}

pub fn alert_end() -> Color {
    palette().alert.at(1.0)
}

pub fn download() -> Gradient {
    palette().download
}

pub fn upload() -> Gradient {
    palette().upload
}

pub fn fmt_speed(v: f64) -> String {
    const UNITS: [&str; 5] = ["B/s", "KiB/s", "MiB/s", "GiB/s", "TiB/s"];
    fmt_units(v, &UNITS)
}

pub fn fmt_bytes(v: f64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    fmt_units(v, &UNITS)
}

fn fmt_units(v: f64, units: &[&str]) -> String {
    let mut v = v.max(0.0);
    let mut u = 0;
    while v >= 1024.0 && u < units.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    // Compact and width-bounded: no space before the unit, and decimals only
    // below 10 so a value never exceeds 8 characters ("1023KiB/s"), which lets
    // the table columns use fixed widths instead of sampling live content.
    if u == 0 || v >= 100.0 {
        format!("{v:.0}{}", units[u])
    } else if v < 10.0 {
        format!("{v:.1}{}", units[u])
    } else {
        format!("{v:.0}{}", units[u])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speed_units() {
        assert_eq!(fmt_speed(12.4), "12B/s");
        assert_eq!(fmt_speed(12_745.0), "12KiB/s");
        assert_eq!(fmt_speed(700.0), "700B/s");
        assert_eq!(fmt_bytes(1_572_864.0), "1.5MiB");
        // width is bounded even at the top of every unit range
        assert!(fmt_speed(1023.0 * 1024.0 + 900.0).len() <= 9);
    }

    #[test]
    fn hex_and_gradient() {
        assert_eq!(parse_hex("#ff0000"), Some(Color::Rgb(255, 0, 0)));
        assert_eq!(parse_hex("0af"), Some(Color::Rgb(0, 170, 255)));
        // 非 ASCII 字节值必须拒绝而非 panic(文件里可有任意字节;
        // 第二例走 8 位长递归分支)
        assert_eq!(parse_hex("红fff"), None);
        assert_eq!(parse_hex("红ff000"), None);
        let g = Gradient([
            Color::Rgb(0, 0, 0),
            Color::Rgb(100, 100, 100),
            Color::Rgb(200, 200, 200),
        ]);
        assert_eq!(g.at(0.0), Color::Rgb(0, 0, 0));
        assert_eq!(g.at(0.5), Color::Rgb(100, 100, 100));
        assert_eq!(g.at(1.0), Color::Rgb(200, 200, 200));
    }

    #[test]
    fn all_builtin_themes_parse() {
        for (name, content) in theme_data::BUILTIN_THEMES {
            assert!(
                parse_btop_str(content).is_some(),
                "theme {name} failed to parse"
            );
        }
    }

    #[test]
    fn all_builtin_themes_dim_meets_contrast_floor() {
        for (name, content) in theme_data::BUILTIN_THEMES {
            let Some(mut pal) = parse_btop_str(content) else {
                continue;
            };
            pal.dim = lift_dim(pal.dim, pal.bg, pal.text);
            if let Some(bg) = pal.bg {
                assert!(
                    contrast_ratio(pal.dim, bg) >= DIM_CONTRAST_FLOOR,
                    "theme {name}: dim {:?} still below the floor after lift",
                    pal.dim
                );
            }
        }
    }

    #[test]
    fn dim_lift_keeps_hierarchy_and_idempotence() {
        // invisible dim (equal to bg) must be lifted but never reach text
        let bgc = Color::Rgb(0x28, 0x2a, 0x36);
        let bg = Some(bgc);
        let text = Color::Rgb(0xf8, 0xf8, 0xf2);
        let lifted = lift_dim(Color::Rgb(0x28, 0x2a, 0x36), bg, text);
        assert!(contrast_ratio(lifted, bgc) >= DIM_CONTRAST_FLOOR);
        assert_ne!(lifted, text, "lifted dim collapsed into plain text");

        // compliant dim passes through untouched; lifting again is a no-op
        let fine = Color::Rgb(0xee, 0xee, 0xee);
        assert_eq!(lift_dim(fine, bg, text), fine);
        assert_eq!(lift_dim(lifted, bg, text), lifted);

        // transparent bg and non-Rgb colors are not measurable — unchanged
        assert_eq!(lift_dim(fine, None, text), fine);
        assert_eq!(
            lift_dim(Color::Indexed(8), bg, text),
            Color::Indexed(8)
        );
    }
}

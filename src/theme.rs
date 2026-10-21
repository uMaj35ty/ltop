use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

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
        _ => if t < 0.5 { a } else { b },
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

/// omarchy 跟随模式标记(init/apply_omarchy 置位,apply_builtin 清除)
static FOLLOWING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn cells() -> &'static (RwLock<Palette>, RwLock<String>) {
    static CELLS: OnceLock<(RwLock<Palette>, RwLock<String>)> = OnceLock::new();
    CELLS.get_or_init(|| (RwLock::new(Palette::default()), RwLock::new("dracula".into())))
}

fn set(pal: Palette, name: &str) {
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
    let ok = omarchy_candidates().iter().any(|p| load_file(p));
    if ok {
        set_following(true);
    }
    ok
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
        let Some(ke) = line[kb..].find(']').map(|i| i + kb) else { continue };
        let key = &line[kb + 1..ke];
        let Some(qs) = line[ke + 1..].find('"').map(|i| i + ke + 1) else { continue };
        let rest = &line[qs + 1..];
        let Some(qe) = rest.find('"') else { continue };
        let Some(color) = parse_hex(&rest[..qe]) else { continue };
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
        Some(Gradient([
            *map.get(a)?,
            *map.get(b)?,
            *map.get(c)?,
        ]))
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
        let g = Gradient([Color::Rgb(0, 0, 0), Color::Rgb(100, 100, 100), Color::Rgb(200, 200, 200)]);
        assert_eq!(g.at(0.0), Color::Rgb(0, 0, 0));
        assert_eq!(g.at(0.5), Color::Rgb(100, 100, 100));
        assert_eq!(g.at(1.0), Color::Rgb(200, 200, 200));
    }

    #[test]
    fn all_builtin_themes_parse() {
        for (name, content) in theme_data::BUILTIN_THEMES {
            assert!(parse_btop_str(content).is_some(), "theme {name} failed to parse");
        }
    }
}

//! Persistent user config: `~/.config/ltop/config.conf`, plain `key = value`.
//! Loaded at startup; menu settings changes mutate the app and rewrite the
//! file immediately (no hot file watching — restart-free by construction).

use std::fs;
use std::path::PathBuf;

use crate::app::LayoutMode;
use crate::collect::conn::{ConnFilter, ConnSort};
use crate::collect::lan::LanSort;
use crate::collect::proc::ProcSort;
use crate::ui::graph::GraphMode;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    En,
    Zh,
}

impl Lang {
    pub fn next(self) -> Self {
        match self {
            Self::En => Self::Zh,
            Self::Zh => Self::En,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::Zh => "zh",
        }
    }

    #[cfg(not(test))]
    fn parse(s: &str) -> Option<Self> {
        match s {
            "en" => Some(Self::En),
            "zh" => Some(Self::Zh),
            _ => None,
        }
    }

    /// UI strings for the bottom bar, menus and config panels only — box
    /// titles stay English in every language.
    pub fn tr(self, k: &str) -> &'static str {
        match self {
            Self::En => match k {
                "quit" => "quit",
                "menu" => "menu",
                "graph" => "graph",
                "layout" => "layout",
                "boxes" => "boxes",
                "scroll" => "scroll",
                "themes" => "themes",
                "settings" => "settings",
                "help" => "help",
                "reset" => "reset",
                "graphs" => "graphs",
                "about" => "about",
                "close" => "close",
                "hint" => " \u{2190}/\u{2192} select \u{b7} enter apply \u{b7} esc close ",
                "hint_reset" => " letter executes \u{b7} \u{2191}\u{2193} select \u{b7} esc close ",
                "letters_hint" => " letter cycles \u{b7} esc close",
                "language" => "language",
                "net" => "net graphs",
                "disk" => "disk graphs",
                "refresh" => "refresh",
                "log" => "log",
                "log_interval" => "log interval",
                "log_retention" => "log retention",
                "sweep_interval" => "sweep interval",
                "never" => "never",
                "sweep" => "sweep",
                "tracked_only" => "log tracked only",
                "global_kill" => "global kill key",
                "pause" => "pause",
                "search" => "search",
                "paused" => "paused",
                "search_hint" => " enter filter \u{b7} esc clear ",
                "filter" => "filter",
                "lifetime" => "alive",
                "uptime" => "up",
                "clear" => "clear",
                "udp_on" => " udp per-process accounting: eBPF active (ltop-bpf)",
                "udp_off" => " udp per-process accounting: off \u{2014} shows up once ltop-bpf runs",
                "caliber_head" => " data scopes (mismatch \u{2260} bug)",
                "caliber_procs" => " PROCS = payload bytes (eBPF, session); NET box = NIC bytes (headers, retransmits, kernel traffic)",
                "caliber_conns" => " CONNS: TCP \u{3a3} counts since connection setup (kernel); UDP \u{3a3} is session; proxied traffic counts under the proxy",
                "caliber_keys" => " space pauses sampling \u{b7} / filters procs & conns (enter applies, esc clears)",
                "tombstone" => "tombstones",
                "proc_groups" => "conns proc groups",
                "expanded" => "expanded",
                "collapsed" => "collapsed",
                "on" => "on",
                "off" => "off",
                "press_any" => " press any key to close",
                "reset_net" => "reset net totals",
                "reset_disk" => "reset disk totals",
                "reset_all" => "reset all totals",
                "config_path" => "config file",
                "boxes_close" => "toggle boxes",
                "hint_close_short" => "or hide",
                "set_both" => "set both",
                "detail_desc" => "open item detail",
                "zoom_desc" => "zoom focused box (lan / conns / procs)",
                "focus_desc" => "focus procs / conns / lan (toggle)",
                "order_desc" => "procs sort",
                "expand_desc" => "expand all mounts",
                "filter_desc" => "conns filter all/lan",
                "conn_order_desc" => "conns sort",
                "net_io_desc" => "iface / io mode",
                "lan_order_desc" => "lan sort (ip / hostname / mac / iface)",
                "track_desc" => "track item in detail",
                "kill_desc" => "SIGTERM in detail",
                "log_desc" => "logging on/off",
                "sweep_desc" => "lan sweep on/off",
                "refresh_desc" => "refresh interval",
                "sel_scroll_desc" => "select / scroll lists",
                "quit_menu_desc" => "quit / menu",
                _ => "",
            },
            Self::Zh => match k {
                "quit" => "\u{9000}\u{51fa}",
                "menu" => "\u{83dc}\u{5355}",
                "graph" => "\u{56fe}\u{8868}",
                "layout" => "\u{5e03}\u{5c40}",
                "boxes" => "\u{76d2}\u{5b50}",
                "scroll" => "\u{6eda}\u{52a8}",
                "themes" => "\u{4e3b}\u{9898}",
                "settings" => "\u{8bbe}\u{7f6e}",
                "help" => "\u{5e2e}\u{52a9}",
                "reset" => "\u{91cd}\u{7f6e}",
                "graphs" => "\u{56fe}\u{8868}\u{6837}\u{5f0f}",
                "about" => "\u{5173}\u{4e8e}",
                "close" => "\u{5173}\u{95ed}",
                "hint" => " \u{2190}/\u{2192} \u{9009}\u{62e9} \u{b7} enter \u{786e}\u{8ba4} \u{b7} esc \u{5173}\u{95ed} ",
                "hint_reset" => " \u{5b57}\u{6bcd}\u{76f4}\u{63a5}\u{6267}\u{884c} \u{b7} \u{2191}\u{2193} \u{9009}\u{62e9} \u{b7} esc \u{5173}\u{95ed} ",
                "letters_hint" => " \u{5b57}\u{6bcd}\u{5faa}\u{73af} \u{b7} esc \u{5173}\u{95ed}",
                "language" => "\u{8bed}\u{8a00}",
                "net" => "\u{7f51}\u{7edc}\u{56fe}\u{8868}",
                "disk" => "\u{786c}\u{76d8}\u{56fe}\u{8868}",
                "refresh" => "\u{5237}\u{65b0}",
                "log" => "\u{65e5}\u{5fd7}",
                "log_interval" => "\u{65e5}\u{5fd7}\u{95f4}\u{9694}",
                "log_retention" => "\u{65e5}\u{5fd7}\u{4fdd}\u{7559}",
                "sweep_interval" => "\u{626b}\u{63cf}\u{95f4}\u{9694}",
                "never" => "\u{6c38}\u{4e0d}\u{5220}\u{9664}",
                "sweep" => "\u{626b}\u{63cf}",
                "tracked_only" => "\u{4ec5}\u{8bb0}\u{5f55}\u{8ffd}\u{8e2a}\u{9879}",
                "global_kill" => "\u{5168}\u{5c40} kill \u{952e}",
                "pause" => "\u{6682}\u{505c}",
                "search" => "\u{641c}\u{7d22}",
                "paused" => "\u{5df2}\u{6682}\u{505c}",
                "search_hint" => " enter \u{8fc7}\u{6ee4} \u{b7} esc \u{6e05}\u{9664} ",
                "filter" => "\u{8fc7}\u{6ee4}",
                "lifetime" => "\u{5b58}\u{6d3b}",
                "uptime" => "\u{8fd0}\u{884c}",
                "clear" => "\u{6e05}\u{9664}",
                "udp_on" => " udp \u{6bcf}\u{8fdb}\u{7a0b}\u{8ba1}\u{8d39}: eBPF \u{6d3b}\u{8dc3}(ltop-bpf)",
                "udp_off" => " udp \u{6bcf}\u{8fdb}\u{7a0b}\u{8ba1}\u{8d39}: \u{672a}\u{542f}\u{7528} \u{2014} sudo \u{8fd0}\u{884c} ltop-bpf \u{540e}\u{81ea}\u{52a8}\u{51fa}\u{73b0}",
                "caliber_head" => " \u{6570}\u{636e}\u{53e3}\u{5f84}(\u{5bf9}\u{4e0d}\u{4e0a}\u{4e0d}\u{662f} bug)",
                "caliber_procs" => " PROCS = \u{5e94}\u{7528}\u{5c42}\u{5b57}\u{8282}(eBPF \u{4f1a}\u{8bdd}\u{7d2f}\u{8ba1});NET \u{76d2} = \u{7f51}\u{5361}\u{5b57}\u{8282}(\u{542b}\u{5305}\u{5934}/\u{91cd}\u{4f20}/\u{5185}\u{6838}\u{6d41}\u{91cf})",
                "caliber_conns" => " CONNS:TCP \u{3a3} \u{4e3a}\u{5185}\u{6838}\u{81ea}\u{5efa}\u{8fde}\u{7d2f}\u{8ba1}(\u{542b}\u{542f}\u{52a8}\u{524d});UDP \u{3a3} \u{4e3a}\u{4f1a}\u{8bdd}\u{7d2f}\u{8ba1};\u{4ee3}\u{7406}\u{6d41}\u{91cf}\u{8bb0}\u{5728}\u{4ee3}\u{7406}\u{5934}\u{4e0a}",
                "caliber_keys" => " space \u{6682}\u{505c}\u{91c7}\u{6837} \u{b7} / \u{641c}\u{7d22}\u{8fc7}\u{6ee4}\u{8fdb}\u{7a0b}\u{4e0e}\u{8fde}\u{63a5}(enter \u{5e94}\u{7528} esc \u{6e05}\u{9664})",
                "tombstone" => "\u{5893}\u{7891}\u{884c}",
                "proc_groups" => "conns \u{8fdb}\u{7a0b}\u{5206}\u{7ec4}",
                "expanded" => "\u{5c55}\u{5f00}",
                "collapsed" => "\u{6298}\u{53e0}",
                "on" => "\u{5f00}",
                "off" => "\u{5173}",
                "press_any" => " \u{6309}\u{4efb}\u{610f}\u{952e}\u{5173}\u{95ed}",
                "reset_net" => "\u{6e05}\u{96f6}\u{7f51}\u{7edc}\u{7d2f}\u{8ba1}",
                "reset_disk" => "\u{6e05}\u{96f6}\u{786c}\u{76d8}\u{7d2f}\u{8ba1}",
                "reset_all" => "\u{5168}\u{90e8}\u{6e05}\u{96f6}",
                "config_path" => "\u{914d}\u{7f6e}\u{6587}\u{4ef6}",
                "boxes_close" => "\u{76d2}\u{5b50}\u{5f00}\u{5173}",
                "hint_close_short" => "\u{6216}\u{9690}\u{85cf}",
                "set_both" => "\u{540c}\u{65f6}\u{8bbe}\u{7f6e}",
                "detail_desc" => "\u{6253}\u{5f00}\u{6761}\u{76ee}\u{8be6}\u{60c5}",
                "zoom_desc" => "\u{5168}\u{5c4f}\u{805a}\u{7126}\u{76d2}\u{5b50}（lan / conns / procs）",
                "focus_desc" => "\u{805a}\u{7126} procs / conns / lan（再按取消）",
                "order_desc" => "procs \u{6392}\u{5e8f}\u{4e94}\u{6001}",
                "expand_desc" => "\u{5c55}\u{5f00}/\u{6298}\u{53e0}\u{6302}\u{8f7d}\u{70b9}",
                "filter_desc" => "conns \u{8fc7}\u{6ee4} all/lan",
                "conn_order_desc" => "conns \u{6392}\u{5e8f}\u{4e09}\u{6001}",
                "net_io_desc" => "\u{5207}\u{7f51}\u{5361} / io \u{6a21}\u{5f0f}",
                "lan_order_desc" => "lan \u{6392}\u{5e8f}(ip / \u{4e3b}\u{673a}\u{540d} / mac / \u{63a5}\u{53e3})",
                "track_desc" => "\u{8be6}\u{60c5}\u{4e2d}\u{52fe}\u{9009}\u{8ffd}\u{8e2a}",
                "kill_desc" => "\u{8be6}\u{60c5}\u{4e2d} SIGTERM",
                "log_desc" => "\u{65e5}\u{5fd7}\u{5f55}\u{5236}\u{5f00}/\u{5173}",
                "sweep_desc" => "\u{626b}\u{63cf}\u{5f00}/\u{5173}",
                "refresh_desc" => "\u{5237}\u{65b0}\u{95f4}\u{9694}",
                "sel_scroll_desc" => "\u{9009}\u{4e2d} / \u{6eda}\u{52a8}\u{5217}\u{8868}",
                "quit_menu_desc" => "\u{9000}\u{51fa} / \u{83dc}\u{5355}",
                _ => "",
            },
        }
    }
}

pub struct Config {
    pub lang: Lang,
    pub layout: LayoutMode,
    pub tick_ms: u64,
    pub graph_net: GraphMode,
    pub graph_disk: GraphMode,
    pub boxes: [bool; 6],
    pub log_interval_ms: u64,
    /// 0 = keep logs forever
    pub log_retention_days: u32,
    pub sweep_interval_s: u64,
    pub sweep: bool,
    /// log only processes pinned from the detail panel
    pub log_tracked_only: bool,
    /// expose the global kill key (advanced)
    pub allow_global_kill: bool,
    pub lan_sort: LanSort,
    pub conn_filter: ConnFilter,
    /// CONNS 墓碑行保留时长;0 = 关闭墓碑
    pub tombstone_ttl_ms: u64,
    /// "omarchy" = 跟随 omarchy 当前主题;其余为内嵌 btop 主题名
    pub theme: String,
    /// 运行中实时跟随 omarchy 主题文件重写(omarchy-theme-set);默认关:
    /// 非 omarchy 系统无意义,省一次每 tick 的 stat。CLI --follow-theme 可临时开
    pub theme_live_follow: bool,
    /// proc 分组默认展开(true)或折叠(false)
    pub proc_expand_default: bool,
    pub conn_sort: ConnSort,
    pub proc_sort: ProcSort,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            lang: Lang::En,
            layout: LayoutMode::Classic,
            tick_ms: 1000,
            graph_net: GraphMode::Hybrid,
            graph_disk: GraphMode::Centered,
            boxes: [true; 6],
            log_interval_ms: 60_000,
            log_retention_days: 7,
            sweep_interval_s: 60,
            sweep: true,
            log_tracked_only: false,
            allow_global_kill: false,
            lan_sort: LanSort::Ip,
            conn_filter: ConnFilter::All,
            tombstone_ttl_ms: 5000,
            theme: "omarchy".to_string(),
            theme_live_follow: false,
            proc_expand_default: false,
            conn_sort: ConnSort::Proc,
            proc_sort: ProcSort::NetSum,
        }
    }
}

#[cfg(not(test))]
fn parse_layout(s: &str) -> Option<LayoutMode> {
    match s {
        "classic" => Some(LayoutMode::Classic),
        "wide" => Some(LayoutMode::Wide),
        "tall" => Some(LayoutMode::Tall),
        _ => None,
    }
}

#[cfg(not(test))]
fn parse_graph(s: &str) -> Option<GraphMode> {
    match s {
        "bar" => Some(GraphMode::Solid),
        "center" => Some(GraphMode::Centered),
        "line" => Some(GraphMode::Line),
        "heart" => Some(GraphMode::Heartbeat),
        "hybrid" => Some(GraphMode::Hybrid),
        "block" => Some(GraphMode::Block),
        _ => None,
    }
}

impl Config {
    pub fn config_dir() -> Option<PathBuf> {
        let base = match std::env::var("XDG_CONFIG_HOME") {
            Ok(x) if !x.is_empty() => PathBuf::from(x),
            _ => PathBuf::from(std::env::var("HOME").ok()?).join(".config"),
        };
        Some(base.join("ltop"))
    }

    pub fn path() -> Option<PathBuf> {
        Self::config_dir().map(|d| d.join("config.conf"))
    }

    /// One-time rename of legacy `lanmon` dirs so existing config survives the
    /// project rename.
    pub fn migrate_legacy_dirs() {
        if let Ok(home) = std::env::var("HOME") {
            for (old, new) in [
                // localist-top → ltop(2026-10-21 更名);新的在前:两者都在时新者优先
                (
                    PathBuf::from(&home).join(".config/localist-top"),
                    PathBuf::from(&home).join(".config/ltop"),
                ),
                (
                    PathBuf::from(&home).join(".local/state/localist-top"),
                    PathBuf::from(&home).join(".local/state/ltop"),
                ),
                (
                    PathBuf::from(&home).join(".config/lanmon"),
                    PathBuf::from(&home).join(".config/ltop"),
                ),
                (
                    PathBuf::from(&home).join(".local/state/lanmon"),
                    PathBuf::from(&home).join(".local/state/ltop"),
                ),
            ] {
                if old.exists() && !new.exists() {
                    let _ = fs::rename(&old, &new);
                }
            }
        }
    }

    pub fn load() -> Self {
        // 测试与宿主用户配置双向隔离:App::new 读真实 config 会让渲染类
        // 测试随宿主的 boxes/lang/layout 漂移(实测:宿主 config 里被误
        // hide 的盒子令 Air 上 4 个渲染测试失败);save 同理 no-op
        #[cfg(test)]
        {
            Self::default()
        }
        #[cfg(not(test))]
        {
            Self::load_persisted()
        }
    }

    #[cfg(not(test))]
    fn load_persisted() -> Self {
        let mut cfg = Self::default();
        let Some(path) = Self::path() else {
            return cfg;
        };
        let Ok(content) = fs::read_to_string(path) else {
            return cfg;
        };
        for line in content.lines() {
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let (k, v) = (k.trim(), v.trim());
            match k {
                "lang" => {
                    if let Some(x) = Lang::parse(v) {
                        cfg.lang = x;
                    }
                }
                "layout" => {
                    if let Some(x) = parse_layout(v) {
                        cfg.layout = x;
                    }
                }
                "tick_ms" => {
                    if let Ok(x) = v.parse::<u64>() {
                        cfg.tick_ms = x.clamp(250, 5000);
                    }
                }
                "graph_net" => {
                    if let Some(x) = parse_graph(v) {
                        cfg.graph_net = x;
                    }
                }
                "graph_disk" => {
                    if let Some(x) = parse_graph(v) {
                        cfg.graph_disk = x;
                    }
                }
                "boxes" => {
                    for (i, c) in v.chars().take(6).enumerate() {
                        cfg.boxes[i] = c == '1';
                    }
                    // never allow everything off
                    if !cfg.boxes.iter().any(|&b| b) {
                        cfg.boxes = [true; 6];
                    }
                }
                "log_interval_ms" => {
                    if let Ok(x) = v.parse::<u64>() {
                        cfg.log_interval_ms = x.clamp(1000, 3_600_000);
                    }
                }
                "log_retention_days" => {
                    if let Ok(x) = v.parse::<u32>() {
                        cfg.log_retention_days = x;
                    }
                }
                "sweep_interval_s" => {
                    if let Ok(x) = v.parse::<u64>() {
                        cfg.sweep_interval_s = x.clamp(5, 86_400);
                    }
                }
                "sweep" => cfg.sweep = v == "true",
                "log_tracked_only" => cfg.log_tracked_only = v == "true",
                "allow_global_kill" => cfg.allow_global_kill = v == "true",
                "lan_sort" => {
                    cfg.lan_sort = match v {
                        "ip" => LanSort::Ip,
                        "host" => LanSort::Hostname,
                        "mac" => LanSort::Mac,
                        "iface" => LanSort::Iface,
                        _ => cfg.lan_sort,
                    }
                }
                "tombstone_ttl_ms" => {
                    if let Ok(v) = v.parse() {
                        cfg.tombstone_ttl_ms = v;
                    }
                }
                "proc_expand_default" => cfg.proc_expand_default = v == "true",
                "theme" => {
                    if !v.is_empty() {
                        cfg.theme = v.to_string();
                    }
                }
                "theme_live_follow" => cfg.theme_live_follow = v == "true",
                "conn_filter" => {
                    cfg.conn_filter = match v {
                        "all" => ConnFilter::All,
                        "lan" => ConnFilter::Lan,
                        "tcp" => ConnFilter::Tcp,
                        "udp" => ConnFilter::Udp,
                        _ => cfg.conn_filter,
                    }
                }
                "conn_sort" => {
                    cfg.conn_sort = match v {
                        "name" => ConnSort::Name,
                        "rate" => ConnSort::Rate,
                        "sum" => ConnSort::Sum,
                        _ => cfg.conn_sort,
                    }
                }
                "proc_sort" => {
                    cfg.proc_sort = match v {
                        "name" => ProcSort::Name,
                        "net-sum" => ProcSort::NetSum,
                        "net-rate" => ProcSort::NetRate,
                        "disk-sum" => ProcSort::DiskSum,
                        "disk-rate" => ProcSort::DiskRate,
                        _ => cfg.proc_sort,
                    }
                }
                _ => {}
            }
        }
        cfg
    }

    pub fn content(&self) -> String {
        let boxes: String = self.boxes.iter().map(|&b| if b { '1' } else { '0' }).collect();
        format!(
            "lang = {}\nlayout = {}\ntick_ms = {}\ngraph_net = {}\ngraph_disk = {}\nboxes = {}\nlog_interval_ms = {}\nlog_retention_days = {}\nsweep_interval_s = {}\nsweep = {}\nlog_tracked_only = {}\nallow_global_kill = {}\nlan_sort = {}\nconn_filter = {}\nconn_sort = {}\nproc_sort = {}\ntombstone_ttl_ms = {}\ntheme = {}\ntheme_live_follow = {}\nproc_expand_default = {}\n",
            self.lang.label(),
            match self.layout {
                LayoutMode::Classic => "classic",
                LayoutMode::Wide => "wide",
                LayoutMode::Tall => "tall",
            },
            self.tick_ms,
            self.graph_net.label(),
            self.graph_disk.label(),
            boxes,
            self.log_interval_ms,
            self.log_retention_days,
            self.sweep_interval_s,
            self.sweep,
            self.log_tracked_only,
            self.allow_global_kill,
            self.lan_sort.label(),
            self.conn_filter.label(),
            self.conn_sort.label(),
            self.proc_sort.label(),
            self.tombstone_ttl_ms,
            self.theme,
            self.theme_live_follow,
            self.proc_expand_default,
        )
    }

    #[cfg(not(test))]
    pub fn save(&self) {
        if let Some(dir) = Self::config_dir() {
            if fs::create_dir_all(&dir).is_ok() {
                if let Some(path) = Self::path() {
                    let _ = fs::write(path, self.content());
                }
            }
        }
    }
    // 测试环境 no-op:不写宿主真实 config(与 load 隔离配套)
    #[cfg(test)]
    pub fn save(&self) {}

}

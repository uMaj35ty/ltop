use std::time::Duration;

use crate::collect::conn::ConnCollector;
use crate::collect::disk::DiskCollector;
use crate::collect::lan::LanCollector;
use crate::collect::net::NetCollector;
use crate::collect::proc::ProcCollector;
use crate::collect::udp::UdpCollector;
use crate::config::{Config, Lang};
use crate::log::MonLog;
use crate::ui::graph::GraphMode;

/// Boxes, addressed by number keys 1-6 (btop-style toggle badges)
pub const BOX_NET: usize = 0;
pub const BOX_LAN: usize = 1;
pub const BOX_DISKIO: usize = 2;
pub const BOX_DISKS: usize = 3;
pub const BOX_CONNS: usize = 4;
pub const BOX_PROCS: usize = 5;
pub const BOX_COUNT: usize = 6;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum IoGraphMode {
    Both,
    Read,
    Write,
}

impl IoGraphMode {
    pub fn next(self) -> Self {
        match self {
            Self::Both => Self::Read,
            Self::Read => Self::Write,
            Self::Write => Self::Both,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Both => "both",
            Self::Read => "read",
            Self::Write => "write",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MenuState {
    None,
    /// btop-style bottom menu bar: themes / settings / reset / graphs / about / quit / close
    Main(usize),
    /// settings panel: (selected row)
    Settings(usize),
    /// theme picker list: (selected item, scroll offset); item 0 = "omarchy"
    Theme(usize, usize),
    /// reset popup: 0 net / 1 disk / 2 all
    Reset(usize),
    /// graph style popup (per-scope graph modes)
    Graphs(usize),
    /// help + about popup
    Help,
    /// detailed view of a list item: (box index, stable id)
    Detail(usize, u64),
    /// kill confirmation for a pid
    ConfirmKill(u32),
    /// restart confirmation for a pid: SIGTERM, wait for exit, relaunch
    ConfirmRestart(u32),
    /// `/` 搜索模式(输入缓冲在 App.search_buf)
    Search,
}

/// Items the user pinned from the detail panel. Order decides the row colour
/// (alternating), so tracked rows stay visually distinguishable.
#[derive(Default)]
pub struct Tracked {
    pub procs: Vec<u32>,
    pub conns: Vec<u64>,
}

impl Tracked {
    fn toggle_vec(list: &mut Vec<u64>, id: u64) {
        if let Some(pos) = list.iter().position(|&x| x == id) {
            list.remove(pos);
        } else {
            list.push(id);
        }
    }

    pub fn toggle_proc(&mut self, pid: u32) {
        if let Some(pos) = self.procs.iter().position(|&x| x == pid) {
            self.procs.remove(pos);
        } else {
            self.procs.push(pid);
        }
    }

    pub fn toggle_conn(&mut self, inode: u64) {
        Self::toggle_vec(&mut self.conns, inode);
    }
    /// colour slot (0/1) when tracked, so rows alternate
    pub fn proc_slot(&self, pid: u32) -> Option<usize> {
        self.procs.iter().position(|&p| p == pid).map(|i| i % 2)
    }

    pub fn conn_slot(&self, inode: u64) -> Option<usize> {
        self.conns.iter().position(|&i| i == inode).map(|i| i % 2)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum LayoutMode {
    /// NET | LAN on top, DISK I/O | DISKS below
    #[default]
    Classic,
    /// graphs on top side-by-side (NET | DISK I/O), tables below (LAN | DISKS)
    Wide,
    /// graphs stacked full-width (NET over DISK I/O), tables below (LAN | DISKS)
    Tall,
}

impl LayoutMode {
    pub fn next(self) -> Self {
        match self {
            Self::Classic => Self::Wide,
            Self::Wide => Self::Tall,
            Self::Tall => Self::Classic,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            Self::Classic => Self::Tall,
            Self::Wide => Self::Classic,
            Self::Tall => Self::Wide,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Classic => "classic",
            Self::Wide => "wide",
            Self::Tall => "tall",
        }
    }
}

#[cfg(target_os = "linux")]
fn host_name() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

#[cfg(target_os = "macos")]
fn host_name() -> String {
    use std::ffi::CStr;
    let mut buf = [0 as libc::c_char; 256];
    // SAFETY: buf is a fully-owned 256-byte buffer for the duration of the call
    if unsafe { libc::gethostname(buf.as_mut_ptr(), buf.len()) } == 0 {
        // SAFETY: gethostname NUL-terminates within the buffer on success
        unsafe { CStr::from_ptr(buf.as_ptr()) }
            .to_string_lossy()
            .trim_end_matches('.')
            .to_string()
    } else {
        String::new()
    }
}

pub struct App {
    pub net: NetCollector,
    pub lan: LanCollector,
    pub disk: DiskCollector,
    pub conn: ConnCollector,
    pub proc: ProcCollector,
    /// eBPF UDP deltas via the ltop-bpf daemon snapshot (passive; silent
    /// degrade when the daemon is not running)
    pub udp: UdpCollector,
    /// mac:nettop 轮询(PROCS net + CONNS 流行;Linux 无)
    #[cfg(target_os = "macos")]
    pub nettop: crate::collect::nettop::NettopCollector,
    pub log: MonLog,
    pub lang: Lang,
    pub hostname: String,
    pub ticks: u64,
    pub tick: Duration,
    pub quit: bool,
    /// space 键:冻结采样,画面停格便于读数
    pub paused: bool,
    /// `/` 搜索:对 procs/conns 名字的子串过滤(不区分大小写)
    pub search: String,
    /// 搜索作用的目标盒(Tab 切换):BOX_CONNS / BOX_PROCS
    pub search_box: usize,
    /// proc 分组展开状态:相对 `proc_expand_default` 的反转集合
    pub expanded: std::collections::HashSet<String>,
    /// proc 分组的默认态:false = 折叠(聚合行)、true = 展开
    pub proc_expand_default: bool,
    /// zoom:独占全屏的盒(LAN/CONNS/PROCS;z 进入/退出;None=正常)
    pub zoom: Option<usize>,
    /// 鼠标双击检测:上次左键按下(时间, 列, 行)
    pub last_click: Option<(std::time::Instant, u16, u16)>,
    /// detail 弹窗的字段游标(↑↓ 移动,y 复制选中值)
    pub detail_sel: usize,
    /// 运行中实时跟随 omarchy 主题重写(--follow-theme / theme_live_follow);
    /// 默认关,save_config 时原样保留
    pub theme_live_follow: bool,
    /// 搜索输入缓冲(菜单 Search 模式中)
    pub search_buf: String,
    pub menu: MenuState,
    /// per-list cursor; only procs/conns support selection
    pub sel: [Option<usize>; BOX_COUNT],
    /// the entity each box's selection names (pid / inode) — lets the
    /// highlight follow a process through re-sorts instead of sticking to a
    /// list position
    pub sel_anchor: [Option<u64>; BOX_COUNT],
    /// per-box focus flags (independent; p/c toggle their own box)
    pub focus: [bool; BOX_COUNT],
    /// most recently focused box — arrows/enter/k operate on it
    pub last_focus: usize,
    pub tracked: Tracked,
    pub log_tracked_only: bool,
    pub allow_global_kill: bool,
    pub layout: LayoutMode,
    /// box visibility toggled by number keys (btop 1-4 style)
    pub visible: [bool; BOX_COUNT],
    /// per-box list scroll offset (mouse wheel / arrow keys)
    pub scroll: [usize; BOX_COUNT],
    pub io_graph: IoGraphMode,
    pub disks_expanded: bool,
    /// braille graph style, settable per scope (menu graphs / global `g`)
    pub graph_net: GraphMode,
    pub net_graph_coupled: bool,
    pub net_graph_scale_ratio: u32,
    pub graph_disk: GraphMode,
}

impl App {
    pub fn new(cli_log_interval: Option<Duration>) -> Self {
        let cfg = Config::load();
        let hostname = host_name();
        let log_interval = cli_log_interval.unwrap_or(Duration::from_millis(cfg.log_interval_ms));
        let mut app = Self {
            net_graph_coupled: cfg.net_graph_coupled,
            net_graph_scale_ratio: cfg.net_graph_scale_ratio,
            net: NetCollector::new(),
            lan: LanCollector::new(),
            disk: DiskCollector::new(),
            conn: ConnCollector::new(),
            proc: ProcCollector::new(),
            udp: UdpCollector::new(),
            #[cfg(target_os = "macos")]
            nettop: crate::collect::nettop::NettopCollector::new(),
            log: MonLog::new(log_interval, cfg.log_retention_days, hostname.clone()),
            lang: cfg.lang,
            hostname,
            ticks: 0,
            tick: Duration::from_millis(1000),
            quit: false,
            paused: false,
            search: String::new(),
            search_box: BOX_CONNS,
            expanded: std::collections::HashSet::new(),
            proc_expand_default: false,
            zoom: None,
            last_click: None,
            detail_sel: 0,
            theme_live_follow: false,
            search_buf: String::new(),
            menu: MenuState::None,
            sel: [None; BOX_COUNT],
            sel_anchor: [None; BOX_COUNT],
            focus: [false; BOX_COUNT],
            last_focus: BOX_CONNS,
            tracked: Tracked::default(),
            log_tracked_only: cfg.log_tracked_only,
            allow_global_kill: cfg.allow_global_kill,
            layout: cfg.layout,
            visible: cfg.boxes,
            scroll: [0; BOX_COUNT],
            io_graph: IoGraphMode::Both,
            disks_expanded: false,
            graph_net: cfg.graph_net,
            graph_disk: cfg.graph_disk,
        };
        // config-driven state that lives in the collectors
        app.tick = Duration::from_millis(cfg.tick_ms);
        app.proc.exit_retention = if cfg.proc_exit_retention_min == 0 {
            Duration::MAX
        } else {
            Duration::from_secs(cfg.proc_exit_retention_min * 60)
        };
        app.lan.sweep_interval = Duration::from_secs(cfg.sweep_interval_s);
        app.lan.sweep_enabled = cfg.sweep;
        // retention housekeeping on startup
        let _ = app.log.purge_old();
        app.lan.sort = cfg.lan_sort;
        app.conn.filter = cfg.conn_filter;
        app.conn.sort = cfg.conn_sort;
        app.proc.sort = cfg.proc_sort;
        let old_default = app.proc_expand_default;
        app.conn.tombstone_ttl = std::time::Duration::from_millis(cfg.tombstone_ttl_ms);
        app.proc_expand_default = cfg.proc_expand_default;
        app.theme_live_follow = cfg.theme_live_follow;
        if app.proc_expand_default != old_default {
            app.expanded.clear(); // 换默认态时清空反转集合
        }
        app
    }

    /// Snapshot current user-adjustable state into the config file
    pub fn save_config(&self) {
        Config {
            lang: self.lang,
            layout: self.layout,
            tick_ms: self.tick.as_millis() as u64,
            graph_net: self.graph_net,
            graph_disk: self.graph_disk,
            boxes: self.visible,
            log_interval_ms: self.log.interval.as_millis() as u64,
            log_tracked_only: self.log_tracked_only,
            allow_global_kill: self.allow_global_kill,
            log_retention_days: self.log.retention_days,
            sweep_interval_s: self.lan.sweep_interval.as_secs(),
            sweep: self.lan.sweep_enabled,
            lan_sort: self.lan.sort,
            conn_filter: self.conn.filter,
            conn_sort: self.conn.sort,
            proc_sort: self.proc.sort,
            tombstone_ttl_ms: self.conn.tombstone_ttl.as_millis() as u64,
            theme: if crate::theme::following_omarchy() {
                "omarchy".to_string()
            } else {
                crate::theme::theme_name()
            },
            theme_live_follow: self.theme_live_follow,
            proc_expand_default: self.proc_expand_default,
            proc_exit_retention_min: if self.proc.exit_retention == Duration::MAX {
                0
            } else {
                self.proc.exit_retention.as_secs() / 60
            },
            net_graph_coupled: self.net_graph_coupled,
            net_graph_scale_ratio: self.net_graph_scale_ratio,
        }
        .save();
    }

    pub fn update(&mut self) {
        if self.paused {
            return; // 冻结:采样差分在恢复后一次性补上,字节不丢
        }
        // capture what is selected before the samples re-sort the lists, so
        // the highlight can follow the same process/connection/neighbour afterwards
        self.sel_anchor[crate::app::BOX_PROCS] =
            crate::ui::selected_id(self, crate::app::BOX_PROCS);
        self.sel_anchor[crate::app::BOX_CONNS] =
            crate::ui::selected_id(self, crate::app::BOX_CONNS);
        self.sel_anchor[crate::app::BOX_LAN] = crate::ui::selected_id(self, crate::app::BOX_LAN);
        self.net.sample();
        self.lan.sample();
        self.disk.sample();
        #[cfg(target_os = "linux")]
        {
            self.conn.sample();
            let udp = self.udp.sample();
            self.conn.apply_udp(&udp.inodes);
            self.proc.sample(&udp.net, &udp.udp);
        }
        // mac:一次 nettop 全量喂两盒——流行 → CONNS,进程行差分 → PROCS
        #[cfg(target_os = "macos")]
        {
            self.nettop.sample();
            let flows = std::mem::take(&mut self.nettop.flows);
            self.conn.ingest(flows);
            self.conn.sample();
            let net = std::mem::take(&mut self.nettop.net);
            let udp = std::mem::take(&mut self.nettop.udp);
            self.proc.sample(&net, &udp);
        }
        for idx in [
            crate::app::BOX_PROCS,
            crate::app::BOX_CONNS,
            crate::app::BOX_LAN,
        ] {
            if let Some(id) = self.sel_anchor[idx] {
                crate::ui::reselect(self, idx, id);
            }
        }
        // 快照序列化只在录制时做:from_app 对每条连接/进程做格式化与
        // String 克隆,默认不开录制时每个 tick 白扔一整轮分配
        if self.log.enabled {
            let mut snap = crate::snapshot::from_app(self);
            if self.log_tracked_only {
                // tracked_only:日志行的 procs 仅记录置顶进程
                let tracked: std::collections::HashSet<u32> =
                    self.tracked.procs.iter().copied().collect();
                snap.procs.retain(|p| tracked.contains(&p.pid));
            }
            self.log.maybe_write(&snap);
        }
        self.ticks += 1;
    }

    pub fn reset_net(&mut self) {
        self.net.reset_totals();
        self.conn.reset_totals();
        self.proc.reset_net();
    }

    pub fn reset_disk(&mut self) {
        self.disk.reset_session();
        self.proc.reset_disk();
    }

    pub fn reset_all(&mut self) {
        self.reset_net();
        self.reset_disk();
    }
}

pub fn local_time_str() -> String {
    let t = unsafe { libc::time(std::ptr::null_mut()) };
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&t, &mut tm) };
    format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
}

/// 秒数 → 紧凑时长:3d4h / 2h13m / 5m12s / 42s
pub fn fmt_dur(secs: u64) -> String {
    let d = secs / 86400;
    let h = (secs % 86400) / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if d > 0 {
        format!("{d}d{h}h")
    } else if h > 0 {
        format!("{h}h{m:02}m")
    } else if m > 0 {
        format!("{m}m{s:02}s")
    } else {
        format!("{s}s")
    }
}

pub fn fmt_interval(d: Duration) -> String {
    let ms = d.as_millis();
    if ms < 1000 {
        format!("{ms}ms")
    } else {
        format!("{:.1}s", d.as_secs_f64())
    }
}

#[cfg(test)]
mod pause_tests {
    use super::*;

    /// space 冻结:update 直返,tick 不前进;恢复后一次性补采样
    #[test]
    fn pause_freezes_sampling() {
        let mut app = App::new(None);
        app.paused = true;
        let ticks = app.ticks;
        app.update();
        assert_eq!(app.ticks, ticks, "暂停时不应采样");
        app.paused = false;
        app.update();
        assert_ne!(app.ticks, ticks, "恢复后应恢复采样");
    }
}

//! localist top: btop-styled TUI monitor (net / lan / disk io / conns / procs)

pub mod app;
pub mod collect;
pub mod config;
pub mod monlog;
pub mod replay;
pub mod theme;
pub mod theme_data;
pub mod ui;

use std::io::{self, stdout};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind,
        KeyModifiers, MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};

use app::{App, MenuState};

type Tui = Terminal<CrosstermBackend<io::Stdout>>;

/// `--help`/`-h`、`--version`/`-V` 在任何副作用(目录迁移/主题初始化/开终端)之前短路
fn early_exit(args: &[String]) -> Option<Result<()>> {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        // argv[0] 的文件名即调用名(ltop 或软链别名 loli),取不到回落 ltop
        let prog = args
            .first()
            .and_then(|a| std::path::Path::new(a).file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("ltop");
        println!("{}", usage_text(prog));
        return Some(Ok(()));
    }
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("ltop {}", env!("CARGO_PKG_VERSION"));
        return Some(Ok(()));
    }
    None
}

/// 与 `--dump`/`--replay`/`--log` 的实际处理一一对应;改参数先改这里。
/// USAGE 行按调用名显示:loli 是安装后软链出来的别名(`ln -s ltop loli`),
/// 不随仓库分发;软链指向同一程序,-h 自然跟随调用名
fn usage_text(prog: &str) -> String {
    format!(
        "localist-top v{} — btop-styled TUI monitoring for local network, LAN neighbours and disk I/O

USAGE:
    {prog} [OPTIONS]

    (no options)            run the TUI
    --dump                  one-shot diagnostic dump to stdout, then exit
    --log                   start the TUI with the monitoring log enabled
    --log-interval <MS>     monitoring log interval in ms (with --log; default from settings)
    --follow-theme          live-follow Omarchy theme changes while running (default off)
    --replay [FILE]         replay/analyse the latest session log (or a given JSONL file)
    --top <N>               replay: report the top N processes (default 15)
    --csv <OUT>             replay: also export per-process deltas as CSV
    -h, --help              print this help
    -V, --version           print version

eBPF per-process/per-connection accounting needs the privileged helper:
    sudo systemctl enable --now ltop-bpf
Data scopes differ by design (PROCS = payload, NET box = NIC bytes) — see in-app help (m → help).",
        env!("CARGO_PKG_VERSION")
    )
}

/// 本发行版仅支持 x86_64:eBPF 探针的 PT_REGS 寄存器映射与各数据源都按
/// 探针寄存器映射覆盖 x86_64 与 aarch64;其他架构启动即明确报错,
/// 而不是静默产出错账
pub fn arch_guard() -> Result<()> {
    if !matches!(std::env::consts::ARCH, "x86_64" | "aarch64") {
        anyhow::bail!(
            "ltop v{} supports x86_64/aarch64 Linux only (running on {}); \
             other architectures are not supported — see README",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::ARCH
        );
    }
    Ok(())
}

/// entry point used by the `ltop` binary (and any symlink alias to it)
pub fn cli_main() -> Result<()> {
    // `ltop --dump | head` 这类管道截断:恢复 SIGPIPE 默认行为,静默退出
    // 而非 println! 在 EPIPE 上 panic
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let args: Vec<String> = std::env::args().collect();
    // help/version 最先短路:不依赖 tty,也不受 arch 限制(arm64 上也要能看帮助)
    if let Some(res) = early_exit(&args) {
        return res;
    }
    arch_guard()?;
    config::Config::migrate_legacy_dirs();
    if args.iter().any(|a| a == "--replay") {
        return replay::run_replay(&args);
    }
    let theme_src = theme::init();
    // 恢复上次选定的内嵌主题(非 omarchy 跟随)
    let boot_cfg = config::Config::load();
    if boot_cfg.theme != "omarchy" {
        let _ = theme::apply_by_name(&boot_cfg.theme);
    }
    if args.iter().any(|a| a == "--dump") {
        return dump(&theme_src);
    }
    let auto_log = args.iter().any(|a| a == "--log");
    // 实时跟随:CLI 旗标或配置开启;默认关(非 omarchy 系统无意义,省每 tick 一次 stat)
    let follow_theme =
        args.iter().any(|a| a == "--follow-theme") || boot_cfg.theme_live_follow;
    let log_interval: Option<Duration> = args
        .iter()
        .position(|a| a == "--log-interval")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_millis);

    let mut terminal = setup()?;
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = restore();
        default_hook(info);
    }));
    let res = run(&mut terminal, log_interval, auto_log, follow_theme);
    restore()?;
    res
}

fn setup() -> Result<Tui> {
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen, EnableMouseCapture)?;
    Ok(Terminal::new(CrosstermBackend::new(stdout()))?)
}

fn restore() -> Result<()> {
    disable_raw_mode()?;
    execute!(stdout(), LeaveAlternateScreen, DisableMouseCapture)?;
    Ok(())
}

fn run(
    terminal: &mut Tui,
    log_interval: Option<Duration>,
    auto_log: bool,
    follow_theme: bool,
) -> Result<()> {
    let mut app = App::new(log_interval);
    // CLI --follow-theme 在配置之上再开一档(配置默认关)
    app.theme_live_follow = app.theme_live_follow || follow_theme;
    if auto_log {
        app.monlog.toggle();
    }
    // two warm-up samples so the first frame already has rates
    app.update();
    std::thread::sleep(Duration::from_millis(350));
    app.update();
    let mut last_tick = Instant::now();

    while !app.quit {
        terminal.draw(|f| ui::draw(f, &app))?;

        let timeout = app.tick.saturating_sub(last_tick.elapsed());
        if event::poll(timeout)? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    handle_key(&mut app, key.code, key.modifiers);
                }
                Event::Mouse(m) => handle_mouse(&mut app, &m),
                _ => {}
            }
        }

        if last_tick.elapsed() >= app.tick {
            // omarchy 跟随模式(--follow-theme / theme_live_follow):主题文件
            // 被重写(omarchy-theme-set)即热重载,下一帧 draw 拿到新调色板
            if app.theme_live_follow {
                theme::poll_omarchy_follow();
            }
            app.update();
            last_tick = Instant::now();
        }
    }
    app.monlog.finalize();
    Ok(())
}

fn handle_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    if modifiers.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        app.quit = true;
        return;
    }
    match app.menu {
        MenuState::None => match code {
            KeyCode::Char('q') => app.quit = true,
            KeyCode::Char(' ') => app.paused = !app.paused,
            KeyCode::Char('/') => {
                // 打开时载入当前过滤,可直接编辑;清空后 enter 即重置
                app.search_buf = app.search.clone();
                app.menu = MenuState::Search;
            }
            // Esc backs out of everything (focus + selection); only m opens
            // the menu — reaching for Esc to close a popup must never land
            // you in another popup one press later
            KeyCode::Esc => {
                app.conn.peer_filter = None; // 解除 LAN 联动的对端过滤
                for f in app.focus.iter_mut() {
                    *f = false;
                }
                for s in app.sel.iter_mut() {
                    *s = None;
                }
            }
            KeyCode::Char('m') => app.menu = MenuState::Main(0),
            KeyCode::Char(c @ '1'..='6') => {
                let idx = (c as usize - '1' as usize).min(app::BOX_COUNT - 1);
                let shown = app.visible.iter().filter(|&&v| v).count();
                if !(app.visible[idx] && shown <= 1) {
                    app.visible[idx] = !app.visible[idx];
                }
            }
            KeyCode::Tab => app.layout = app.layout.next(),
            KeyCode::Char('n') => {
                app.net.cycle_selected();
                app.last_focus = app::BOX_NET;
            }
            KeyCode::Char('p') => {
                // 单焦点:点亮一个盒即熄灭其余
                let on = !app.focus[app::BOX_PROCS];
                app.focus[app::BOX_PROCS] = on;
                app.focus[app::BOX_CONNS] = false;
                app.focus[app::BOX_LAN] = false;
                app.last_focus = app::BOX_PROCS;
            }
            KeyCode::Char('c') => {
                let on = !app.focus[app::BOX_CONNS];
                app.focus[app::BOX_CONNS] = on;
                app.focus[app::BOX_PROCS] = false;
                app.focus[app::BOX_LAN] = false;
                app.last_focus = app::BOX_CONNS;
            }
            KeyCode::Char('l') => {
                // 单焦点:点亮 lan 即熄灭 procs/conns
                let on = !app.focus[app::BOX_LAN];
                app.focus[app::BOX_LAN] = on;
                app.focus[app::BOX_PROCS] = false;
                app.focus[app::BOX_CONNS] = false;
                app.last_focus = app::BOX_LAN;
            }
            KeyCode::Char('e') => app.lan.sort = app.lan.sort.next(),
            KeyCode::Char('i') => {
                app.io_graph = app.io_graph.next();
                app.last_focus = app::BOX_DISKIO;
            }
            KeyCode::Char('r') => app.proc.sort = app.proc.sort.next(),
            // `d` matches the [d]isks title letter; `e` now belongs to the
            // lan box (order cycle)
            KeyCode::Char('d') => {
                app.disks_expanded = !app.disks_expanded;
            }
            KeyCode::Char('f') => {
                app.conn.filter = app.conn.filter.next();
                app.conn.peer_filter = None; // 手动切过滤即解除对端联动
            }
            KeyCode::Char('o') => app.conn.sort = app.conn.sort.next(),
            KeyCode::Left | KeyCode::Right => {
                // 聚合父行上:← 折叠、→ 展开;其余场景不占用
                let b = app.last_focus;
                if b == app::BOX_CONNS
                    && app.focus[b]
                    && ui::selected_is_agg(app, b)
                {
                    if let Some(key) = ui::selected_agg_key(app, b) {
                        // ← 折叠、→ 展开(相对设置中的默认态取反亦可)
                        let open = code == KeyCode::Right;
                        ui::set_group_open(app, &key, open);
                    }
                }
            }
            KeyCode::Char('k') => {
                let b = app.last_focus;
                if app.focus[b] {
                    if let Some(pid) = ui::selected_pid(app, b) {
                        kill_pid(pid);
                    }
                }
            }
            KeyCode::Char('L') => {
                app.monlog.toggle();
            }
            KeyCode::Enter => {
                let b = app.last_focus;
                if app.focus[b] {
                    // lan 行上的 enter = 置顶(仅一个)+ 联动 conns 只看该对端;
                    // 再按取消置顶并解除联动
                    if b == app::BOX_LAN {
                        if let Some(id) = ui::selected_id(app, b) {
                            let ip = std::net::Ipv4Addr::from(id as u32);
                            if app.lan.tracked.contains(&ip) {
                                app.lan.tracked.remove(&ip);
                                app.conn.peer_filter = None;
                            } else {
                                app.lan.tracked.clear();
                                app.lan.tracked.insert(ip);
                                // 焦点留在 lan:再按 enter 即取消;
                                // conns 盒的联动过滤结果实时可见
                                app.conn.peer_filter = Some(ip);
                            }
                            ui::reselect(app, app::BOX_LAN, id);
                        }
                    } else if b == app::BOX_CONNS && ui::selected_is_agg(app, b) {
                        if let Some(key) = ui::selected_agg_key(app, b) {
                            ui::toggle_group(app, &key);
                        }
                    } else if let Some(id) = ui::selected_id(app, b) {
                        app.menu = MenuState::Detail(b, id);
                    }
                }
            }
            KeyCode::Up => {
                let b = app.last_focus;
                if app.focus[b] {
                    ui::move_selection(app, b, -1);
                } else {
                    app.scroll[b] = app.scroll[b].saturating_sub(3);
                }
            }
            KeyCode::Down => {
                let b = app.last_focus;
                if app.focus[b] {
                    ui::move_selection(app, b, 1);
                } else {
                    app.scroll[b] = app.scroll[b].saturating_add(3);
                }
            }
            KeyCode::PageUp => {
                app.scroll[app.last_focus] = app.scroll[app.last_focus].saturating_sub(3);
            }
            KeyCode::PageDown => {
                app.scroll[app.last_focus] = app.scroll[app.last_focus].saturating_add(3);
            }
            KeyCode::Char('s') => {
                app.lan.sweep_enabled = !app.lan.sweep_enabled;
                if app.lan.sweep_enabled {
                    app.lan.sweep_now();
                }
            }
            KeyCode::Char('+') | KeyCode::Char('=') => {
                app.tick = (app.tick + Duration::from_millis(250)).min(Duration::from_secs(5));
            }
            KeyCode::Char('-') => {
                app.tick = app
                    .tick
                    .saturating_sub(Duration::from_millis(250))
                    .max(Duration::from_millis(250));
            }
            _ => {}
        },
        MenuState::Main(sel) => {
            const BUTTONS: usize = 7;
            match code {
                KeyCode::Left | KeyCode::Char('h') => {
                    app.menu = MenuState::Main((sel + BUTTONS - 1) % BUTTONS)
                }
                KeyCode::Right | KeyCode::Char('l') => {
                    app.menu = MenuState::Main((sel + 1) % BUTTONS)
                }
                KeyCode::Enter => match sel {
                    0 => app.menu = MenuState::Theme(0, 0),
                    1 => app.menu = MenuState::Settings(0),
                    2 => app.menu = MenuState::Reset(0),
                    3 => app.menu = MenuState::Graphs,
                    4 => app.menu = MenuState::Help,
                    5 => app.quit = true,
                    _ => app.menu = MenuState::None,
                },
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('m') => {
                    app.menu = MenuState::None
                }
                _ => {}
            }
        }
        MenuState::Settings(sel) => {
            const ROWS: usize = 12;
            const TICKS: [u64; 5] = [250, 500, 1000, 2000, 5000];
            const LOGS: [u64; 5] = [10_000, 30_000, 60_000, 300_000, 600_000];
            const RETENTION: [u32; 3] = [7, 30, 0];
            const SWEEPS: [u64; 4] = [30, 60, 120, 300];
            fn next_in(list: &[u64], cur: u64) -> u64 {
                let i = list.iter().position(|&v| v == cur).map_or(0, |i| (i + 1) % list.len());
                list[i]
            }
            fn next_u32(list: &[u32], cur: u32) -> u32 {
                let i = list.iter().position(|&v| v == cur).map_or(0, |i| (i + 1) % list.len());
                list[i]
            }
            const TOMBS: [u64; 4] = [0, 3000, 5000, 8000];
            let changed = match code {
                KeyCode::Up | KeyCode::Char('k') => {
                    app.menu = MenuState::Settings(sel.saturating_sub(1));
                    false
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    app.menu = MenuState::Settings((sel + 1).min(ROWS - 1));
                    false
                }
                KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => match sel {
                    0 => {
                        app.lang = app.lang.next();
                        true
                    }
                    1 => {
                        let ms = next_in(&TICKS, app.tick.as_millis() as u64);
                        app.tick = Duration::from_millis(ms);
                        true
                    }
                    2 => {
                        app.layout = app.layout.next();
                        true
                    }
                    3 => {
                        app.graph_net = app.graph_net.next();
                        true
                    }
                    4 => {
                        app.graph_disk = app.graph_disk.next();
                        true
                    }
                    5 => {
                        let ms = next_in(&LOGS, app.monlog.interval.as_millis() as u64);
                        app.monlog.interval = Duration::from_millis(ms);
                        true
                    }
                    6 => {
                        let d = next_u32(&RETENTION, app.monlog.retention_days);
                        app.monlog.retention_days = d;
                        let _ = app.monlog.purge_old();
                        true
                    }
                    7 => {
                        let s = next_in(&SWEEPS, app.lan.sweep_interval.as_secs());
                        app.lan.sweep_interval = Duration::from_secs(s);
                        true
                    }
                    8 => {
                        app.lan.sweep_enabled = !app.lan.sweep_enabled;
                        true
                    }
                    9 => {
                        app.log_tracked_only = !app.log_tracked_only;
                        true
                    }
                    10 => {
                        app.allow_global_kill = !app.allow_global_kill;
                        true
                    }
                    11 => {
                        let cur = app.conn.tombstone_ttl.as_millis() as u64;
                        let i = TOMBS
                            .iter()
                            .position(|&v| v == cur)
                            .map_or(0, |i| (i + 1) % TOMBS.len());
                        app.conn.tombstone_ttl = std::time::Duration::from_millis(TOMBS[i]);
                        true
                    }
                    _ => false,
                },
                _ => false,
            };
            if changed {
                app.save_config();
            }
            if matches!(code, KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('m')) {
                app.menu = MenuState::None;
            }
        }
        MenuState::Theme(sel, scroll) => match code {
            KeyCode::Up | KeyCode::Char('k') => {
                if sel > 0 {
                    let scroll = if sel - 1 < scroll { scroll - 1 } else { scroll };
                    app.menu = MenuState::Theme(sel - 1, scroll);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let count = theme::builtin_count() + 1;
                if sel + 1 < count {
                    let scroll = if sel + 1 >= scroll + ui::THEME_VISIBLE {
                        scroll + 1
                    } else {
                        scroll
                    };
                    app.menu = MenuState::Theme(sel + 1, scroll);
                }
            }
            KeyCode::Enter => {
                let ok = if sel == 0 {
                    theme::apply_omarchy()
                } else {
                    theme::apply_builtin(sel - 1)
                };
                if ok {
                    // 主题选择持久化(omarchy 跟随记为 "omarchy")
                    app.save_config();
                }
            }
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('m') => {
                app.menu = MenuState::None
            }
            _ => {}
        },
        MenuState::Reset(sel) => match code {
            KeyCode::Up | KeyCode::Char('k') => {
                app.menu = MenuState::Reset(sel.saturating_sub(1))
            }
            KeyCode::Down | KeyCode::Char('j') => {
                app.menu = MenuState::Reset((sel + 1).min(2))
            }
            KeyCode::Enter => {
                match sel {
                    0 => app.reset_net(),
                    1 => app.reset_disk(),
                    _ => app.reset_all(),
                }
                app.menu = MenuState::None;
            }
            KeyCode::Char('n') => {
                app.reset_net();
                app.menu = MenuState::None;
            }
            KeyCode::Char('d') => {
                app.reset_disk();
                app.menu = MenuState::None;
            }
            KeyCode::Char('a') => {
                app.reset_all();
                app.menu = MenuState::None;
            }
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('m') => {
                app.menu = MenuState::None
            }
            _ => {}
        },
        MenuState::Graphs => match code {
            KeyCode::Char('n') => app.graph_net = app.graph_net.next(),
            KeyCode::Char('d') => app.graph_disk = app.graph_disk.next(),
            KeyCode::Char('a') => {
                let s = app.graph_net.next();
                app.graph_net = s;
                app.graph_disk = s;
            }
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('m') => {
                app.menu = MenuState::None
            }
            _ => {}
        },
        MenuState::Search => match code {
            KeyCode::Tab => {
                app.search_box = if app.search_box == app::BOX_CONNS {
                    app::BOX_PROCS
                } else {
                    app::BOX_CONNS
                };
            }
            KeyCode::Char(c) => app.search_buf.push(c),
            KeyCode::Backspace => {
                app.search_buf.pop();
            }
            KeyCode::Enter => {
                app.search = std::mem::take(&mut app.search_buf);
                app.menu = MenuState::None;
            }
            KeyCode::Esc => {
                // esc = 重置:清除已应用的过滤与缓冲
                app.search.clear();
                app.search_buf.clear();
                app.menu = MenuState::None;
            }
            _ => {}
        },
        MenuState::Help => app.menu = MenuState::None,
        MenuState::Detail(b, id) => match code {
            KeyCode::Char(' ') | KeyCode::Char('t') => ui::toggle_track(app, b, id),
            KeyCode::Char('s') => {
                // exited process: start it right away; a running one goes
                // through the restart confirmation (SIGTERM + relaunch)
                if b == app::BOX_PROCS {
                    if let Some(p) = app.proc.procs.iter().find(|p| p.pid as u64 == id) {
                        if let Some(cmd) = &p.cmdline {
                            if p.alive {
                                app.menu = MenuState::ConfirmRestart(p.pid);
                            } else {
                                let _ = start_cmd(cmd);
                            }
                        }
                    }
                }
            }
            KeyCode::Char('k') => {
                let pid = match b {
                    app::BOX_PROCS => Some(id as u32),
                    app::BOX_CONNS => app
                        .conn
                        .conns
                        .iter()
                        .find(|c| c.inode == id)
                        .and_then(|c| c.pid),
                    _ => None,
                };
                if let Some(pid) = pid {
                    app.menu = MenuState::ConfirmKill(pid);
                }
            }
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('m') => {
                app.menu = MenuState::None
            }
            _ => {}
        },
        MenuState::ConfirmKill(pid) => match code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                kill_pid(pid);
                app.menu = MenuState::None;
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc | KeyCode::Char('q') => {
                app.menu = MenuState::None;
            }
            _ => {}
        },
        MenuState::ConfirmRestart(pid) => match code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                let cmd = app
                    .proc
                    .procs
                    .iter()
                    .find(|p| p.pid == pid)
                    .and_then(|p| p.cmdline.clone());
                if let Some(cmd) = cmd {
                    kill_pid(pid);
                    // relaunch from a worker thread: the old instance may take
                    // a moment to exit (releasing ports/singleton locks), and
                    // the TUI must not freeze while waiting for it
                    std::thread::spawn(move || {
                        for _ in 0..40 {
                            if !std::path::Path::new(&format!("/proc/{pid}")).exists() {
                                break;
                            }
                            std::thread::sleep(std::time::Duration::from_millis(50));
                        }
                        let _ = start_cmd(&cmd);
                    });
                }
                app.menu = MenuState::None;
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc | KeyCode::Char('q') => {
                app.menu = MenuState::None;
            }
            _ => {}
        },
    }
}

fn handle_mouse(app: &mut App, m: &crossterm::event::MouseEvent) {
    use crossterm::event::MouseButton;
    match m.kind {
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            if let Some(idx) = ui::hit_box(m.column, m.row) {
                if m.kind == MouseEventKind::ScrollUp {
                    app.scroll[idx] = app.scroll[idx].saturating_sub(3);
                } else {
                    app.scroll[idx] = app.scroll[idx].saturating_add(3);
                }
            }
        }
        MouseEventKind::Down(MouseButton::Left) => {
            // clicking a procs/conns data row selects (and focuses) it;
            // clicking anywhere else cancels every focus
            let hit = ui::hit_box(m.column, m.row)
                .filter(|&idx| {
                    matches!(idx, app::BOX_PROCS | app::BOX_CONNS | app::BOX_LAN)
                })
                .and_then(|idx| ui::box_rect(idx).map(|r| (idx, r)));
            let mut on_row = false;
            if let Some((idx, rect)) = hit {
                let row = m.row as i32 - (rect.y as i32 + 2); // border + header
                if row >= 0 {
                    // 与绘制同源:off 以可见行数封顶(旧版用未封顶的 scroll,
                    // 滚到底后点击错位)
                    let visible = (rect.height as usize).saturating_sub(3).max(1);
                    let abs = ui::clamped_offset(app, idx, visible) + row as usize;
                    if abs < ui::list_len(app, idx) {
                        app.sel[idx] = Some(abs);
                        // 单焦点:点亮一个盒即熄灭其余(与 p/c/l 键同语义)
                        for (b, f) in app.focus.iter_mut().enumerate() {
                            *f = b == idx;
                        }
                        app.last_focus = idx;
                        on_row = true;
                        // 点击聚合父行:选中同时切换展开/折叠
                        if idx == app::BOX_CONNS && ui::selected_is_agg(app, idx) {
                            if let Some(key) = ui::selected_agg_key(app, idx) {
                                ui::toggle_group(app, &key);
                            }
                        }
                    }
                }
            }
            if !on_row {
                // clicked an empty row or outside any list: cancel all focus
                for f in app.focus.iter_mut() {
                    *f = false;
                }
                for s in app.sel.iter_mut() {
                    *s = None;
                }
            }
        }
        _ => {}
    }
}

/// SIGTERM, the polite request; the process can still clean up and exit.
pub(crate) fn kill_pid(pid: u32) {
    unsafe {
        libc::kill(pid as i32, libc::SIGTERM);
    }
}

/// Launch a captured command line detached from the terminal: own session
/// (survives the TUI), null stdio. Used to start/restart a process from its
/// detail panel.
pub(crate) fn start_cmd(cmd: &[String]) -> std::io::Result<u32> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let (prog, args) = cmd
        .split_first()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "empty command"))?;
    // SAFETY: pre_exec runs in the forked child before exec; setsid() takes no
    // arguments, and the closure touches nothing else
    unsafe {
        Command::new(prog)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .pre_exec(|| {
                libc::setsid();
                Ok(())
            })
            .spawn()
            .map(|child| child.id())
    }
}

/// headless dump of one sampled round (debug aid)
pub fn dump(theme_src: &str) -> Result<()> {
    let mut app = App::new(None);
    app.update();
    // long enough for the hostname workers (mDNS + NetBIOS) to finish
    std::thread::sleep(Duration::from_millis(2600));
    app.update();

    println!("== THEME ==\n{theme_src}");
    println!("== NET ==");
    println!("selected: {:?}", app.net.selected);
    for (name, info) in &app.net.ifaces {
        let (rx, tx) = match app.net.rates.get(name) {
            Some(r) => (format!("{:.0} B/s", r.rx), format!("{:.0} B/s", r.tx)),
            None => ("-".into(), "-".into()),
        };
        println!(
            "{:<12} {:<8} speed {:<8} mac {:<18} rx {:<12} tx {:<12} addrs {:?}",
            name, info.operstate, info.speed_mbps.unwrap_or(0), info.mac, rx, tx, info.addrs
        );
    }

    println!("== LAN ==");
    println!(
        "subnets: {:?}  gateways: {:?}  sweep: {}",
        app.lan.subnets, app.lan.gateways, app.lan.sweeping
    );
    for n in app.lan.sorted_neighbors() {
        println!(
            "{:<16} {:<18} {:<10} host {:<24} {}{}",
            n.ip.to_string(),
            n.mac.clone().unwrap_or_default(),
            n.iface,
            n.hostname.clone().unwrap_or_default(),
            if n.is_gateway { "gw " } else { "" },
            if n.is_self { "self" } else { "" },
        );
    }

    println!("== CONNS == total {}", app.conn.conns.len());
    for c in app.conn.conns.iter().take(12) {
        let proc_str = match (&c.proc, c.pid) {
            (Some(n), Some(p)) => format!("{n}({p})"),
            _ => app.conn.user_name(c.uid),
        };
        println!(
            "{:<20} {:<24} -> {:<24} {} r {:.0} w {:.0}",
            proc_str,
            collect::conn::sock_str(&c.local),
            collect::conn::sock_str(&c.remote),
            if c.is_listen() {
                "LISTEN"
            } else if c.is_tcp() {
                "EST"
            } else {
                "UDP"
            },
            c.rate_recv,
            c.rate_sent
        );
    }

    println!("== PROCS ==");
    for p in app.proc.procs.iter().take(12) {
        println!(
            "{:<20} net SUM={}/{} r {}/{} disk SUM={}/{} r {}/{}",
            format!("{}({})", p.name, p.pid),
            fmt_human(p.net_recv as f64),
            fmt_human(p.net_sent as f64),
            fmt_human(p.net_rr),
            fmt_human(p.net_sr),
            fmt_human(p.disk_r as f64),
            fmt_human(p.disk_w as f64),
            fmt_human(p.disk_rr),
            fmt_human(p.disk_wr),
        );
    }

    println!("== UDP (eBPF) ==");
    println!(
        "snapshot {}  fresh {}  pids {}  sockets {}",
        app.udp.path.display(),
        app.udp.fresh,
        app.udp.pids_seen,
        app.udp.sks_seen
    );
    for d in app.udp.last_inodes.iter().take(8) {
        let conn = app
            .conn
            .conns
            .iter()
            .find(|c| c.inode == d.inode)
            .map(|c| format!("{} -> {}", collect::conn::sock_str(&c.local), collect::conn::sock_str(&c.remote)))
            .unwrap_or_else(|| "(socket 已关闭)".into());
        println!(
            "  inode {:<8} {}  delta r {}/ w {}",
            d.inode,
            conn,
            fmt_human(d.recv as f64),
            fmt_human(d.sent as f64),
        );
    }
    for p in app
        .proc
        .procs
        .iter()
        .filter(|p| p.udp_recv + p.udp_sent > 0)
        .take(12)
    {
        println!(
            "{:<20} udp SUM={}/{} r {}/{}",
            format!("{}({})", p.name, p.pid),
            fmt_human(p.udp_recv as f64),
            fmt_human(p.udp_sent as f64),
            fmt_human(p.udp_rr),
            fmt_human(p.udp_sr),
        );
    }

    println!("== DISK ==");
    let mut tree_lines: Vec<String> = Vec::new();
    fn emit(nodes: &[collect::disk::TreeNode], prefix: String, depth: usize, out: &mut Vec<String>) {
        let n = nodes.len();
        for (i, node) in nodes.iter().enumerate() {
            let last = i + 1 == n;
            let glyph = if depth == 0 {
                String::new()
            } else if last {
                "+- ".into()
            } else {
                "|- ".into()
            };
            let child_prefix = if depth == 0 {
                String::new()
            } else {
                format!("{prefix}{}", if last { "  " } else { "| " })
            };
            out.push(format!("{prefix}{glyph}{}", node.name));
            emit(&node.children, child_prefix, depth + 1, out);
        }
    }
    emit(&app.disk.tree, String::new(), 0, &mut tree_lines);
    for l in &tree_lines {
        println!("  {l}");
    }
    for d in &app.disk.devices {
        println!(
            "{:<14} {:<6} mounts {:?} used {}/{} r {:.0} w {:.0}",
            d.name,
            d.fstype,
            d.mounts,
            fmt_human(d.used as f64),
            fmt_human(d.total as f64),
            d.rate_r,
            d.rate_w
        );
    }
    Ok(())
}

pub(crate) fn fmt_human(v: f64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = v;
    let mut u = 0;
    while v >= 1024.0 && u < 4 {
        v /= 1024.0;
        u += 1;
    }
    format!("{v:.1}{}", UNITS[u])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// the build-time eBPF object must parse and carry all four probes plus
    /// the LRU map, so a broken clang output fails tests instead of at runtime
    #[test]
    fn bpf_object_parses_with_probes_and_map() {
        let path = std::path::Path::new(env!("OUT_DIR")).join("ltop_udp_bpf.o");
        let Ok(bytes) = std::fs::read(&path) else {
            eprintln!("skipped: 没有 eBPF 对象(构建机缺 clang)");
            return;
        };
        let obj = aya_obj::Object::parse(&bytes).expect("parse eBPF object");
        for name in [
            "udp_send",
            "udp_recv_entry",
            "udp_recv",
            "udp6_send",
            "udp6_recv_entry",
            "udp6_recv",
            "tcp_send_entry",
            "tcp_send",
            "tcp_recv_entry",
            "tcp_recv",
        ] {
            assert!(obj.programs.contains_key(name), "缺程序 {name}");
        }
        for (name, ksize) in [("net_bytes", 4u32), ("udp_bytes", 4)] {
            let m = obj.maps.get(name).expect("缺 pid 账本 map");
            let aya_obj::maps::Map::Legacy(m) = m else {
                panic!("{name} 应是 legacy map 定义");
            };
            assert_eq!(m.def.map_type, 9, "BPF_MAP_TYPE_LRU_HASH");
            assert_eq!(m.def.key_size, ksize);
            assert_eq!(m.def.value_size, 16);
            assert_eq!(m.def.max_entries, 16384);
        }
        let sk = obj.maps.get("udp_sk").expect("缺 udp_sk map");
        let aya_obj::maps::Map::Legacy(sk) = sk else {
            panic!("udp_sk 应是 legacy map 定义");
        };
        assert_eq!(sk.def.map_type, 9);
        assert_eq!(sk.def.key_size, 8); // struct sock 指针为键
        assert_eq!(sk.def.value_size, 16);
        let stash = obj.maps.get("recv_sk").expect("缺 recv_sk map");
        let aya_obj::maps::Map::Legacy(stash) = stash else {
            panic!("recv_sk 应是 legacy map 定义");
        };
        assert_eq!(stash.def.key_size, 8); // 完整 pid_tgid
        assert_eq!(stash.def.value_size, 8); // 暂存的 sk
        let cache = obj.maps.get("sk_ino").expect("缺 sk_ino map");
        let aya_obj::maps::Map::Legacy(cache) = cache else {
            panic!("sk_ino 应是 legacy map 定义");
        };
        assert_eq!(cache.def.key_size, 8); // sk
        assert_eq!(cache.def.value_size, 8); // inode
    }

    #[test]
    fn enter_on_aggregate_toggles_collapse() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let mut app = App::new(None);
        app.conn.sort = crate::collect::conn::ConnSort::Proc;
        let mk = |inode: u64| crate::collect::conn::Conn {
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
            proc: Some("web".into()),
            dead: false,
            first_seen: None,
            dead_at: None,
        };
        app.conn.conns = vec![mk(1), mk(2)];
        app.sel[app::BOX_CONNS] = Some(0); // 聚合父行
        app.focus[app::BOX_CONNS] = true;
        handle_key(&mut app, KeyCode::Enter, KeyModifiers::empty());
        assert!(
            app.expanded.contains("web"),
            "聚合行 enter 应展开组"
        );
        handle_key(&mut app, KeyCode::Enter, KeyModifiers::empty());
        assert!(!app.expanded.contains("web"), "再按应折叠");
        // ← 折叠 / → 展开
        app.expanded.insert("web".into());
        handle_key(&mut app, KeyCode::Left, KeyModifiers::empty());
        assert!(!app.expanded.contains("web"));
        handle_key(&mut app, KeyCode::Right, KeyModifiers::empty());
        assert!(app.expanded.contains("web"));
    }

    #[test]
    fn enter_on_lan_pins_single_and_filters_conns() {
        use crossterm::event::{KeyCode, KeyModifiers};
        use std::net::Ipv4Addr;
        let mut app = App::new(None);
        for k in 1..=3u32 {
            let ip = Ipv4Addr::new(10, 0, 0, k as u8);
            app.lan.neighbors.insert(ip, crate::collect::lan::Neighbor {
                ip,
                mac: None,
                iface: "eth0".into(),
                complete: true,
                hostname: None,
                is_gateway: false,
                is_self: false,
                last_seen: std::time::Instant::now(),
            });
        }
        // 选中第 2 台(排序后位置 1),聚焦 lan
        app.conn.sort = crate::collect::conn::ConnSort::Name;
        app.focus[app::BOX_LAN] = true;
        app.last_focus = app::BOX_LAN;
        app.sel[app::BOX_LAN] = Some(1);
        let ip2 = Ipv4Addr::new(10, 0, 0, 2);

        handle_key(&mut app, KeyCode::Enter, KeyModifiers::empty());
        assert!(app.lan.tracked.contains(&ip2), "enter 应置顶");
        assert_eq!(app.lan.tracked.len(), 1, "置顶应只有一个");
        assert_eq!(app.conn.peer_filter, Some(ip2), "应联动 conns 对端过滤");
        // 置顶组提前:选中的邻居现在在位置 0
        assert_eq!(
            app.lan.sorted_neighbors()[0].ip, ip2,
            "置顶后应排第一"
        );

        // 再按:取消置顶 + 解除联动
        handle_key(&mut app, KeyCode::Enter, KeyModifiers::empty());
        assert!(app.lan.tracked.is_empty(), "再按应取消置顶");
        assert!(app.conn.peer_filter.is_none(), "联动应解除");
    }

    #[test]
    fn lan_focus_excludes_other_boxes() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let mut app = App::new(None);
        handle_key(&mut app, KeyCode::Char('p'), KeyModifiers::empty());
        handle_key(&mut app, KeyCode::Char('l'), KeyModifiers::empty());
        assert!(app.focus[app::BOX_LAN] && !app.focus[app::BOX_PROCS]);
        handle_key(&mut app, KeyCode::Char('c'), KeyModifiers::empty());
        assert!(app.focus[app::BOX_CONNS] && !app.focus[app::BOX_LAN]);
    }

    #[test]
    fn e_key_cycles_lan_sort() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let mut app = App::new(None);
        let s0 = app.lan.sort;
        handle_key(&mut app, KeyCode::Char('e'), KeyModifiers::empty());
        assert_ne!(app.lan.sort, s0, "e 应循环 lan 排序");
    }

    #[test]
    fn esc_resets_the_applied_search() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let mut app = App::new(None);
        app.search = "fire".into();
        app.search_buf = "fire".into();
        app.menu = MenuState::Search;
        handle_key(
            &mut app,
            KeyCode::Char('x'),
            KeyModifiers::NONE,
        );
        assert_eq!(app.search_buf, "firex");
        assert_eq!(app.search, "fire", "输入中不影响已应用过滤");
        handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.search, "firex", "enter 应用");
        assert!(app.menu == MenuState::None);
        // 再次进入搜索会载入现有过滤;esc 一键重置
        handle_key(&mut app, KeyCode::Char('/'), KeyModifiers::NONE);
        assert_eq!(app.search_buf, "firex");
        handle_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(app.search.is_empty(), "esc 应清除已应用过滤");
        assert!(app.search_buf.is_empty());
        assert!(app.menu == MenuState::None);
    }

    #[test]
    fn kill_pid_terminates_the_process() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep");
        let pid = child.id();
        kill_pid(pid);
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(
            child.try_wait().expect("wait").is_some(),
            "child {pid} survived SIGTERM"
        );
    }

    #[test]
    fn start_cmd_launches_detached() {
        let before = std::fs::read_dir("/proc")
            .expect("read /proc")
            .flatten()
            .filter_map(|e| e.file_name().to_string_lossy().parse::<u32>().ok())
            .collect::<Vec<_>>();
        let new_pid = start_cmd(&["sleep".to_string(), "30".to_string()])
            .expect("spawn detached sleep");
        std::thread::sleep(std::time::Duration::from_millis(150));
        let argv = std::fs::read_to_string(format!("/proc/{new_pid}/cmdline"))
            .expect("detached child must be alive");
        assert!(argv.contains("sleep"), "argv: {argv}");
        kill_pid(new_pid);
        std::thread::sleep(std::time::Duration::from_millis(300));
        // detached child is reaped by init; its pid must not have existed before
        assert!(!before.contains(&new_pid));
    }

    /// p/c focus: first press focuses, second press unfocuses — and unfocusing
    /// must also stop the two-line selection overlay from rendering (that
    /// lingering overlay read as "focus didn't cancel")
    #[test]
    fn p_and_c_second_press_cancels_focus() {
        use crossterm::event::KeyCode;
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut app = App::new(None);
        app.update();
        for (key, idx, other) in [
            (KeyCode::Char('p'), app::BOX_PROCS, app::BOX_CONNS),
            (KeyCode::Char('c'), app::BOX_CONNS, app::BOX_PROCS),
        ] {
            assert!(!app.focus[idx], "starts unfocused");
            handle_key(&mut app, key, KeyModifiers::empty());
            assert!(app.focus[idx], "first press focuses");
            assert!(!app.focus[other], "单焦点:另一盒熄灭");
            handle_key(&mut app, key, KeyModifiers::empty());
            assert!(!app.focus[idx], "second press cancels focus");
        }
        // p 后再 c:唯一焦点切换到 conns
        handle_key(&mut app, KeyCode::Char('p'), KeyModifiers::empty());
        handle_key(&mut app, KeyCode::Char('c'), KeyModifiers::empty());
        assert!(app.focus[app::BOX_CONNS] && !app.focus[app::BOX_PROCS]);
        // with a selection set, the two-line overlay (its "(pid N)" detail
        // line) renders while focused and disappears when focus is cancelled
        handle_key(&mut app, KeyCode::Char('p'), KeyModifiers::empty());
        app.proc.procs.push(crate::collect::proc::ProcRow {
            pid: 4242,
            name: "probe".into(),
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
            uptime_s: None,            alive: true,
            cmdline: None,
        });
        app.proc.procs.shrink_to_fit();
        app.sel[app::BOX_PROCS] = Some(0);
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
        app.focus[app::BOX_PROCS] = true;
        let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
        term.draw(|f| ui::draw(f, &app)).unwrap();
        assert!(buf_text(&term).contains("(pid "), "overlay renders while focused");
        app.focus[app::BOX_PROCS] = false;
        let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
        term.draw(|f| ui::draw(f, &app)).unwrap();
        assert!(!buf_text(&term).contains("(pid "), "overlay gone after unfocus");
    }

    /// Esc on the main page clears all focus/selection; it must NOT open the
    /// menu — closing a popup with Esc must never land in another popup one
    /// press later. `m` keeps opening the menu.
    #[test]
    fn esc_clears_focus_and_m_opens_menu() {
        use crossterm::event::KeyCode;

        let mut app = App::new(None);
        app.focus[app::BOX_PROCS] = true;
        app.focus[app::BOX_CONNS] = true;
        app.sel[app::BOX_PROCS] = Some(2);
        handle_key(&mut app, KeyCode::Esc, KeyModifiers::empty());
        assert!(app.focus.iter().all(|&f| !f), "esc clears all focus");
        assert!(app.sel.iter().all(|&s| s.is_none()), "esc clears selection");
        assert!(app.menu == MenuState::None, "esc does not open the menu");
        handle_key(&mut app, KeyCode::Char('m'), KeyModifiers::empty());
        assert!(matches!(app.menu, MenuState::Main(_)), "m opens the menu");
    }

    /// 点击行点亮一个盒时必须熄灭其余:旧版点击只置位不清除,p 后点
    /// LAN 行会出现双焦点,绕过 p/c/l 键的单焦点不变量
    #[test]
    fn mouse_click_enforces_single_focus() {
        use crossterm::event::{KeyCode, MouseButton, MouseEvent, MouseEventKind};
        use crate::collect::proc::ProcRow;
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut app = App::new(None);
        // 注入合成进程,不依赖测试机的真实采样
        app.proc.procs.push(ProcRow {
            pid: 4242,
            name: "probe".into(),
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
        // 先画一帧,再从 buffer 定位 PROCS 标题上标(⁶)拿点击坐标——
        // 不读 BOX_AREAS(全局态,并行测试互踩,见 layout 测试注释)
        let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
        term.draw(|f| ui::draw(f, &app)).unwrap();
        handle_key(&mut app, KeyCode::Char('l'), KeyModifiers::empty());
        assert!(app.focus[app::BOX_LAN], "l 点亮 lan");
        let buf = term.backend().buffer();
        let procs_title = (0..buf.area().height)
            .flat_map(|y| (0..buf.area().width).map(move |x| (x, y)))
            .find(|(x, y)| buf[(*x, *y)].symbol() == "\u{2076}")
            .expect("⁶procs 标题");
        // 点击标题行下两行 = 首个数据行(跳过边框+表头)。hit_box 内部读
        // BOX_AREAS 全局态,并行测试可能恰好清掉它,重绘重试消除竞态
        let m = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: procs_title.0 + 2,
            row: procs_title.1 + 2,
            modifiers: KeyModifiers::empty(),
        };
        for _ in 0..20 {
            term.draw(|f| ui::draw(f, &app)).unwrap();
            handle_mouse(&mut app, &m);
            if app.focus[app::BOX_PROCS] {
                break;
            }
        }
        assert!(app.focus[app::BOX_PROCS], "点击 procs 行点亮 procs");
        assert!(!app.focus[app::BOX_LAN], "单焦点:lan 熄灭");
        assert!(!app.focus[app::BOX_CONNS], "单焦点:conns 熄灭");
    }

    /// 本发行版仅 x86_64:钉住 arch_guard 的通过路径(测试进程即 x86_64;
    /// 非 x86_64 分支是纯字符串比较,无法在测试内模拟)
    #[test]
    fn arch_guard_passes_on_x86_64() {
        assert!(arch_guard().is_ok());
    }

    /// 非 omarchy 跟随档(固定内嵌主题)时 poll 必须是 no-op:
    /// 固定主题就是用户显式选的,不能被外部文件改写
    #[test]
    fn poll_is_noop_when_not_following() {
        assert!(!theme::poll_omarchy_follow());
    }

    /// --help 文本必须覆盖全部入口参数与助手提示:改参数先改 usage_text,
    /// 这里防的是"加了参数忘了写帮助"。loli 是别名:USAGE 行跟随调用名
    #[test]
    fn usage_covers_all_flags() {
        let u = usage_text("ltop");
        for flag in [
            "--dump",
            "--log",
            "--log-interval",
            "--follow-theme",
            "--replay",
            "--top",
            "--csv",
            "-h, --help",
            "-V, --version",
            "ltop-bpf",
        ] {
            assert!(u.contains(flag), "usage 缺 {flag}");
        }
        assert!(u.contains("ltop [OPTIONS]"), "USAGE 用调用名");
        assert!(usage_text("loli").contains("loli [OPTIONS]"), "loli 别名同版帮助");
    }

    /// update() 必须把 LAN 选中纳入每 tick 锚定:数据驱动的重排只发生
    /// 在 sample() 阶段(捕获在前、采样在后),旧版 sel_anchor[LAN] 永远
    /// 为 None,主机名解析到达等重排会让选中漂移到别的对端
    #[test]
    fn update_anchors_lan_selection() {
        use crate::collect::lan::{LanSort, Neighbor};
        use std::net::Ipv4Addr;
        use std::time::Instant;

        let mut app = App::new(None);
        let mk = |ip: Ipv4Addr, host: &str| Neighbor {
            ip,
            mac: None,
            iface: "eth0".into(),
            complete: true,
            hostname: Some(host.into()),
            is_gateway: false,
            is_self: false,
            last_seen: Instant::now(),
        };
        let a = Ipv4Addr::new(10, 0, 0, 2);
        let b = Ipv4Addr::new(10, 0, 0, 3);
        app.lan.neighbors.insert(a, mk(a, "zzz"));
        app.lan.neighbors.insert(b, mk(b, "aaa"));
        app.lan.sort = LanSort::Ip;
        app.sel[app::BOX_LAN] = Some(0);
        assert_eq!(
            ui::selected_id(&app, app::BOX_LAN),
            Some(u32::from(a) as u64)
        );
        app.update();
        assert_eq!(
            app.sel_anchor[app::BOX_LAN],
            Some(u32::from(a) as u64),
            "update 锚定 LAN 选中(旧版恒为 None)"
        );
        assert_eq!(
            ui::selected_id(&app, app::BOX_LAN),
            Some(u32::from(a) as u64),
            "采样后仍是同一主机"
        );
    }

    #[test]
    fn s_restarts_a_running_process_after_confirmation() {
        use crossterm::event::KeyCode;
        use crate::collect::proc::ProcRow;

        let mut child = std::process::Command::new("sleep")
            .arg("31")
            .spawn()
            .expect("spawn sleep 31");
        let pid = child.id();
        let mut app = App::new(None);
        app.proc.procs.push(ProcRow {
            pid,
            name: "sleep".into(),
            net_recv: 0,
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
            uptime_s: None,            alive: true,
            cmdline: Some(vec!["sleep".into(), "31".into()]),
        });
        app.menu = MenuState::Detail(app::BOX_PROCS, u64::from(pid));
        handle_key(&mut app, KeyCode::Char('s'), KeyModifiers::empty());
        assert!(
            matches!(app.menu, MenuState::ConfirmRestart(_)),
            "s on a running process asks for confirmation"
        );
        // refusing changes nothing
        handle_key(&mut app, KeyCode::Char('n'), KeyModifiers::empty());
        assert!(app.menu == MenuState::None);
        std::thread::sleep(std::time::Duration::from_millis(150));
        assert!(child.try_wait().expect("poll").is_none(), "n must not kill");

        // confirming: SIGTERM the old instance, then relaunch in the worker
        app.menu = MenuState::Detail(app::BOX_PROCS, u64::from(pid));
        handle_key(&mut app, KeyCode::Char('s'), KeyModifiers::empty());
        handle_key(&mut app, KeyCode::Char('y'), KeyModifiers::empty());
        assert!(app.menu == MenuState::None);
        // reap the old instance promptly (the worker waits for /proc/<pid>
        // to vanish; a zombie lingers until the parent reaps it)
        let _ = child.wait();
        // give the worker time to notice and spawn the new one
        std::thread::sleep(std::time::Duration::from_millis(500));
        assert!(
            child.try_wait().expect("poll").is_some(),
            "old instance terminated"
        );
        let new_pid = std::fs::read_dir("/proc")
            .expect("read /proc")
            .flatten()
            .find(|e| {
                std::fs::read_to_string(e.path().join("cmdline"))
                    .map(|c| c == "sleep\x0031\x00")
                    .unwrap_or(false)
            })
            .map(|e| e.file_name().to_string_lossy().parse::<u32>().unwrap());
        let new_pid = new_pid.expect("relaunched sleep 31 must be running");
        kill_pid(new_pid);
    }
}

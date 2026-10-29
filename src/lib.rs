//! ltop: btop-styled TUI monitor (net / lan / disk io / conns / procs)

pub mod app;
pub mod collect;
pub mod config;
pub mod lansetup;
pub mod log;
pub mod replay;
pub mod report;
pub mod snapshot;
pub mod theme;
pub mod theme_data;
pub mod ui;
pub mod watch;

use std::io::{self, stdout, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
        KeyboardEnhancementFlags, MouseEventKind, PopKeyboardEnhancementFlags,
        PushKeyboardEnhancementFlags,
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
        // argv[0] 的文件名即调用名(官方只发 ltop;用户自建软链别名同效),
        // 取不到回落 ltop
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
    // 未知旗标明确报错:静默忽略会让 typo 直接落进 TUI(--json 打错时
    // 用户以为拿到了 JSON)
    let known = [
        "--json",
        "--schema",
        "--dump",
        "--log",
        "--log-interval",
        "--follow-theme",
        "--theme",
        "--replay",
        "--top",
        "--csv",
        "--help",
        "-h",
        "--version",
        "-V",
    ];
    if let Some(bad) = args
        .iter()
        .skip(1)
        .find(|a| a.starts_with("--") && !known.contains(&a.as_str()))
    {
        usage_warn(
            &format!("ltop: unknown option {bad} (try --help)"),
            &format!("ltop: 未知选项 {bad}(试 --help)"),
        );
        return Some(Err(anyhow::anyhow!("unknown option")));
    }
    None
}

/// CLI 用法错误统一口径(0.2.10 约定):`ltop[ 子命令]: <消息>(试 --help)`,
/// 双语、stderr 提示行 + 退出码 1;子命令内与 TUI 旗标路径同款
pub(crate) fn usage_warn(en: &str, zh: &str) {
    eprintln!("{}", crate::pick_s(cli_lang_zh(), en, zh));
}

/// 与 `--dump`/`--replay`/`--log` 的实际处理一一对应;改参数先改这里。
/// USAGE 行按调用名显示:用户可自建软链别名(`ln -s ltop <名>`),
/// 软链指向同一程序,-h 自然跟随调用名
fn usage_text(prog: &str) -> String {
    format!(
        "ltop v{} — btop-styled TUI monitoring for local network, LAN neighbours and disk I/O

USAGE:
    {prog} [OPTIONS]

    (no options)            run the TUI
    watch [OPTIONS]         headless capture to a schema-v1 JSONL file
    report <FILE> [OPTIONS] window summary of a watch/monlog JSONL (--json for AI)
    lan-setup [--remove]    macOS: install/remove the snapshot agent (real MACs
                            behind the macOS 27 privacy gate; see docs)
    --json                  AI-friendly structured snapshot (JSON, schema v1)
    --schema                field reference for --json (machine-readable)
    --dump                  one-shot diagnostic dump to stdout, then exit
    --log                   start the TUI with the monitoring log enabled
    --log-interval <MS>     monitoring log interval in ms (with --log; default from settings)
    --follow-theme          live-follow Omarchy theme changes while running (default off)
    --theme <FILE>          load a btop/Omarchy theme file for this session (fixed, not followed)
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

/// mac:声明用户主动型活动禁用 App Nap。挂机/节能时系统会挂起"空闲"
/// 进程并合并定时器——tick 停摆,唤醒后子进程采样(netstat/nettop)连锁
/// 失效,NET/CONNS 清零僵死。该声明允许系统照常睡眠,但本进程不因空闲
/// 被节流;活动持续整个进程生命周期,无需手动结束
#[cfg(target_os = "macos")]
fn disable_app_nap() {
    use std::ffi::c_void;
    use std::os::raw::c_char;
    #[link(name = "objc", kind = "dylib")]
    extern "C" {
        fn objc_getClass(name: *const c_char) -> *mut c_void;
        fn sel_registerName(name: *const c_char) -> *mut c_void;
        fn objc_msgSend();
    }
    type Id = *mut c_void;
    unsafe {
        // 本crate只链 IOKit(带 CoreFoundation):dlopen 保证 Foundation 在场
        libc::dlopen(
            c"/System/Library/Frameworks/Foundation.framework/Foundation".as_ptr()
                as *const c_char,
            libc::RTLD_LAZY,
        );
        let cls = objc_getClass(c"NSProcessInfo".as_ptr());
        if cls.is_null() {
            return;
        }
        let msg2: unsafe extern "C" fn(Id, Id) -> Id =
            std::mem::transmute(objc_msgSend as *const ());
        let msg3: unsafe extern "C" fn(Id, Id, *const c_char) -> Id =
            std::mem::transmute(objc_msgSend as *const ());
        let msg4: unsafe extern "C" fn(Id, Id, u64, Id) -> u64 =
            std::mem::transmute(objc_msgSend as *const ());
        let pi = msg2(cls, sel_registerName(c"processInfo".as_ptr()));
        if pi.is_null() {
            return;
        }
        let nsstr = objc_getClass(c"NSString".as_ptr());
        if nsstr.is_null() {
            return;
        }
        let reason = msg3(
            nsstr,
            sel_registerName(c"stringWithUTF8String:".as_ptr()),
            c"ltop monitoring tick".as_ptr(),
        );
        if reason.is_null() {
            return;
        }
        // NSActivityUserInitiatedLettingSystemSleepDisabled = 1 << 21
        let _token = msg4(
            pi,
            sel_registerName(c"beginActivityWithOptions:reason:".as_ptr()),
            1 << 21,
            reason,
        );
    }
}

/// eBPF 探针的 PT_REGS 寄存器映射与各数据源都按 x86_64 与 aarch64 双架构
/// 覆盖;其他架构启动即明确报错,而不是静默产出错账
pub fn arch_guard() -> Result<()> {
    let ok = match std::env::consts::OS {
        "linux" => matches!(std::env::consts::ARCH, "x86_64" | "aarch64"),
        "macos" => matches!(std::env::consts::ARCH, "x86_64" | "aarch64"),
        _ => false,
    };
    if !ok {
        anyhow::bail!(
            "ltop v{} supports Linux and macOS on x86_64/aarch64 (running on {} {}); \
             see README",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
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
    #[cfg(target_os = "macos")]
    disable_app_nap();
    let args: Vec<String> = std::env::args().collect();
    // 子命令最先分派:各自的 --help/参数校验在各自模块(help/version
    // 通用短路在下方,只管 TUI 路径的旗标)
    match args.get(1).map(String::as_str) {
        Some("watch") => return watch::run(&args[2..]),
        Some("lan-setup") => return lansetup::run(&args[2..]),
        Some("report") => return report::run(&args[2..]),
        // 子命令打错字不能掉进 TUI(此前 repor 会静默开界面);
        // 旗标(含 --json 等)照旧走原路径
        Some(cmd) if !cmd.starts_with('-') => {
            usage_warn(
                &format!(
                    "ltop: unknown command {cmd:?} (subcommands: watch, report, lan-setup; try --help)"
                ),
                &format!(
                    "ltop: 未知命令 {cmd:?}(可用:watch、report、lan-setup;试 --help)"
                ),
            );
            return Err(anyhow::anyhow!("unknown command"));
        }
        _ => {}
    }
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
    // 恢复上次选定的内嵌主题(非 omarchy 跟随);--theme 直载时固定
    // 用户指定的文件,不被启动恢复覆盖
    let boot_cfg = config::Config::load();
    let theme_fixed_by_cli = args.iter().any(|a| a == "--theme");
    if !theme_fixed_by_cli && boot_cfg.theme != "omarchy" {
        let _ = theme::apply_by_name(&boot_cfg.theme);
    }
    if args.iter().any(|a| a == "--json") {
        return json_snapshot();
    }
    if args.iter().any(|a| a == "--schema") {
        return print_schema();
    }
    if args.iter().any(|a| a == "--dump") {
        return dump(&theme_src);
    }
    let auto_log = args.iter().any(|a| a == "--log");
    // 实时跟随:CLI 旗标或配置开启;默认关(非 omarchy 系统无意义,省每 tick 一次 stat)
    let follow_theme = args.iter().any(|a| a == "--follow-theme") || boot_cfg.theme_live_follow;
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
    // 录制收尾提示在备用屏关闭后打印才会被用户看到
    if let Ok(Some(path)) = &res {
        eprintln!(
            "{}",
            pick_s(
                cli_lang_zh(),
                &format!("ltop: monitoring log saved to {}", path.display()),
                &format!("ltop: 监控日志已保存到 {}", path.display())
            )
        );
    }
    res.map(|_| ())
}

fn setup() -> Result<Tui> {
    enable_raw_mode()?;
    // DISAMBIGUATE_ESCAPE_CODES:esc 编码为完整 CSI 序列,消除裸 0x1B 与
    // 鼠标 SGR 序列的歧义——ssh 分包下 esc 曾被逐字节降级,鼠标事件被
    // 拆成数字字符误触盒子 hide(取证实测)。不支持的终端忽略此请求。
    execute!(
        stdout(),
        EnterAlternateScreen,
        EnableMouseCapture,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    )?;
    Ok(Terminal::new(CrosstermBackend::new(stdout()))?)
}

fn restore() -> Result<()> {
    disable_raw_mode()?;
    execute!(
        stdout(),
        PopKeyboardEnhancementFlags,
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    Ok(())
}

fn run(
    terminal: &mut Tui,
    log_interval: Option<Duration>,
    auto_log: bool,
    follow_theme: bool,
) -> Result<Option<PathBuf>> {
    let mut app = App::new(log_interval);
    // CLI --follow-theme 在配置之上再开一档(配置默认关)
    app.theme_live_follow = app.theme_live_follow || follow_theme;
    if auto_log {
        app.log.toggle();
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
    // 录制收尾:flush+关闭,路径由 cli_main 在备用屏关闭后打印
    // (restore 前打印会被 LeaveAlternateScreen 连屏丢弃)
    let log_path = app.log.finalize();
    Ok(log_path)
}

/// OSC 52:把文本写入终端本地剪贴板(base64;零依赖,ssh 场景直达本地)
fn osc52_copy(text: &str) {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut b64 = String::new();
    for chunk in text.as_bytes().chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        b64.push(T[(n >> 18) as usize & 63] as char);
        b64.push(T[(n >> 12) as usize & 63] as char);
        b64.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        b64.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    let _ = write!(stdout(), "\x1b]52;c;{b64}\x07");
    let _ = stdout().flush();
}

/// 不可信字符串的终端安全过滤:丢弃 C0 控制符(含 ESC/CSI/OSC 载体)。
/// TUI 渲染路径由 ratatui 的 is_control 过滤兜底;所有**绕过 ratatui 的
/// CLI 输出**(--replay/report 的 println、--dump 的名字列)必须先过这里,
/// 否则外部 JSONL 或 LAN 对端提供的进程名/主机名可携带终端转义序列
/// 直接执行(清屏/剪贴板覆写/钓鱼链接)
pub(crate) fn sanitize_text(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect()
}

/// Settings 菜单:循环改第 sel 项的值(forward=下一个值/反向=上一个值;
/// 布尔项两向同效)。返回是否发生变更(需存盘)。行序必须与
/// draw_settings_popup 的 rows 一致,见测试 settings_rows_map_to_their_values
fn cycle_setting(app: &mut App, sel: usize, forward: bool) -> bool {
    const TICKS: [u64; 5] = [250, 500, 1000, 2000, 5000];
    const LOGS: [u64; 5] = [10_000, 30_000, 60_000, 300_000, 600_000];
    const RETENTION: [u32; 3] = [7, 30, 0];
    const SWEEPS: [u64; 4] = [30, 60, 120, 300];
    const TOMBS: [u64; 4] = [0, 3000, 5000, 8000];
    fn step_in(list: &[u64], cur: u64, forward: bool) -> u64 {
        let i = list.iter().position(|&v| v == cur).unwrap_or(0);
        let n = list.len();
        list[if forward {
            (i + 1) % n
        } else {
            (i + n - 1) % n
        }]
    }
    fn step_u32(list: &[u32], cur: u32, forward: bool) -> u32 {
        let i = list.iter().position(|&v| v == cur).unwrap_or(0);
        let n = list.len();
        list[if forward {
            (i + 1) % n
        } else {
            (i + n - 1) % n
        }]
    }
    match sel {
        0 => {
            app.lang = app.lang.next();
            true
        }
        1 => {
            let ms = step_in(&TICKS, app.tick.as_millis() as u64, forward);
            app.tick = Duration::from_millis(ms);
            true
        }
        2 => {
            app.layout = if forward {
                app.layout.next()
            } else {
                app.layout.prev()
            };
            true
        }
        3 => {
            app.graph_net = if forward {
                app.graph_net.next_with_block()
            } else {
                app.graph_net.prev_with_block()
            };
            true
        }
        4 => {
            app.graph_disk = if forward {
                app.graph_disk.next_with_block()
            } else {
                app.graph_disk.prev_with_block()
            };
            true
        }
        5 => {
            // 录制开关(与 L 键同路径):开=建文件写表头,关=flush 收尾
            app.log.toggle();
            true
        }
        6 => {
            // 全局录制热键 L 的暴露开关(默认关,防误触)
            app.allow_global_log = !app.allow_global_log;
            true
        }
        7 => {
            let ms = step_in(&LOGS, app.log.interval.as_millis() as u64, forward);
            app.log.interval = Duration::from_millis(ms);
            true
        }
        8 => {
            let d = step_u32(&RETENTION, app.log.retention_days, forward);
            app.log.retention_days = d;
            let _ = app.log.purge_old();
            true
        }
        9 => {
            app.log_tracked_only = !app.log_tracked_only;
            true
        }
        10 => {
            let s = step_in(&SWEEPS, app.lan.sweep_interval.as_secs(), forward);
            app.lan.sweep_interval = Duration::from_secs(s);
            true
        }
        11 => {
            app.lan.sweep_enabled = !app.lan.sweep_enabled;
            true
        }
        12 => {
            let cur = app.conn.tombstone_ttl.as_millis() as u64;
            let i = TOMBS.iter().position(|&v| v == cur).map_or(0, |i| {
                if forward {
                    (i + 1) % TOMBS.len()
                } else {
                    (i + TOMBS.len() - 1) % TOMBS.len()
                }
            });
            app.conn.tombstone_ttl = std::time::Duration::from_millis(TOMBS[i]);
            true
        }
        13 => {
            app.proc_expand_default = !app.proc_expand_default;
            true
        }
        14 => {
            app.allow_global_kill = !app.allow_global_kill;
            true
        }
        _ => false,
    }
}

fn handle_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    // 诊断开关:LTOP_KEY_LOG=<文件> 追加记录每个按键事件(排查 ssh 下的
    // 按键序列串扰,如 esc 触发盒子 hide)
    if let Ok(path) = std::env::var("LTOP_KEY_LOG") {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(f, "{code:?} {modifiers:?}");
        }
    }
    if modifiers.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        app.quit = true;
        return;
    }
    match app.menu {
        MenuState::None => match code {
            KeyCode::Char('q') => app.quit = true,
            // z:焦点盒(LAN/CONNS/PROCS)全屏 zoom;再按退出。esc 在 zoom
            // 中只取消聚焦(zoom 本体仅 z 可退,见下方 esc 分支)
            KeyCode::Char('z') => {
                if app.zoom.is_some() {
                    app.zoom = None;
                } else if matches!(
                    app.last_focus,
                    crate::app::BOX_LAN | crate::app::BOX_CONNS | crate::app::BOX_PROCS
                ) && app.visible[app.last_focus]
                {
                    app.zoom = Some(app.last_focus);
                }
            }
            // zoom 本体只由 z 退出;esc 在 zoom 内只取消聚焦(留在全屏)——
            // 此前 esc 直接退整个 zoom,想只摘掉聚焦就得连全屏一起退
            KeyCode::Esc if app.zoom.is_some() => {
                for f in app.focus.iter_mut() {
                    *f = false;
                }
            }
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
            // ssh 下 esc 序列可能被解析成带 ALT 修饰的幽灵字符键;盒子的
            // hide/show 只响应无修饰的数字
            KeyCode::Char(c @ '1'..='6')
                if !modifiers.intersects(KeyModifiers::ALT | KeyModifiers::CONTROL) =>
            {
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
                if b == app::BOX_CONNS && app.focus[b] && ui::selected_is_agg(app, b) {
                    if let Some(key) = ui::selected_agg_key(app, b) {
                        // ← 折叠、→ 展开(相对设置中的默认态取反亦可)
                        let open = code == KeyCode::Right;
                        ui::set_group_open(app, &key, open);
                    }
                }
            }
            KeyCode::Char('k') => {
                // 全局即杀键:受设置里的 allow_global_kill(默认关)约束,
                // 关闭时不生效(底栏提示也随开关隐藏);要杀走详情页
                // k → 确认的路径
                if app.allow_global_kill {
                    let b = app.last_focus;
                    if app.focus[b] {
                        if let Some(pid) = ui::selected_pid(app, b) {
                            kill_pid(pid);
                        }
                    }
                }
            }
            KeyCode::Char('L') => {
                // 全局录制热键:受 allow_global_log(默认关)约束防误触;
                // 录制走 --log 或 settings 面板的录制行
                if app.allow_global_log {
                    app.log.toggle();
                }
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
                        app.detail_sel = 0;
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
            // Home/End/g/G:跳顶/跳底(聚焦盒移动选中,未聚焦盒跳滚动;
            // ssh 幽灵序列防御同数字键——排除 ALT/CTRL)
            KeyCode::Home | KeyCode::Char('g')
                if !modifiers.intersects(KeyModifiers::ALT | KeyModifiers::CONTROL) =>
            {
                let b = app.last_focus;
                if app.focus[b] {
                    ui::jump_selection(app, b, true);
                } else {
                    app.scroll[b] = 0;
                }
            }
            KeyCode::End | KeyCode::Char('G')
                if !modifiers.intersects(KeyModifiers::ALT | KeyModifiers::CONTROL) =>
            {
                let b = app.last_focus;
                if app.focus[b] {
                    ui::jump_selection(app, b, false);
                } else {
                    app.scroll[b] = usize::MAX;
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
                    3 => app.menu = MenuState::Graphs(0),
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
            // 与 draw_settings_popup 的分组行、cycle_setting 的分支一一对应:
            // 三处不同步曾把录制行之后全部错位一格(按录制行改的是日志
            // 间隔、按 global_kill 行改的是墓碑时长、末行失效)
            const ROWS: usize = 15;
            let changed = match code {
                KeyCode::Up | KeyCode::Char('k') => {
                    app.menu = MenuState::Settings(sel.saturating_sub(1));
                    false
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    app.menu = MenuState::Settings((sel + 1).min(ROWS - 1));
                    false
                }
                KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                    cycle_setting(app, sel, true)
                }
                KeyCode::Left | KeyCode::Char('h') => cycle_setting(app, sel, false),
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('m') => {
                    app.menu = MenuState::None;
                    false
                }
                _ => false,
            };
            if changed {
                app.save_config();
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
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('m') => app.menu = MenuState::None,
            _ => {}
        },
        MenuState::Reset(sel) => match code {
            KeyCode::Up | KeyCode::Char('k') => app.menu = MenuState::Reset(sel.saturating_sub(1)),
            KeyCode::Down | KeyCode::Char('j') => app.menu = MenuState::Reset((sel + 1).min(2)),
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
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('m') => app.menu = MenuState::None,
            _ => {}
        },
        MenuState::Graphs(sel) => match code {
            KeyCode::Up | KeyCode::Char('k') => app.menu = MenuState::Graphs(sel.saturating_sub(1)),
            KeyCode::Down | KeyCode::Char('j') => app.menu = MenuState::Graphs((sel + 1).min(4)),
            // 行改值:Right/l=下一模式,Left/h=上一模式(Block 仅在此可达)
            KeyCode::Right | KeyCode::Char('l') => match sel {
                0 => app.graph_net = app.graph_net.next_with_block(),
                1 => app.graph_disk = app.graph_disk.next_with_block(),
                2 => {
                    let s = app.graph_net.next_with_block();
                    app.graph_net = s;
                    app.graph_disk = s;
                }
                3 => app.net_graph_coupled = !app.net_graph_coupled,
                4 => app.net_graph_scale_ratio = next_scale_ratio(app.net_graph_scale_ratio),
                _ => {}
            },
            KeyCode::Left | KeyCode::Char('h') => match sel {
                0 => app.graph_net = app.graph_net.prev_with_block(),
                1 => app.graph_disk = app.graph_disk.prev_with_block(),
                2 => {
                    let s = app.graph_net.prev_with_block();
                    app.graph_net = s;
                    app.graph_disk = s;
                }
                3 => app.net_graph_coupled = !app.net_graph_coupled,
                4 => app.net_graph_scale_ratio = prev_scale_ratio(app.net_graph_scale_ratio),
                _ => {}
            },
            KeyCode::Enter => match sel {
                0 => app.graph_net = app.graph_net.next_with_block(),
                1 => app.graph_disk = app.graph_disk.next_with_block(),
                2 => {
                    let s = app.graph_net.next_with_block();
                    app.graph_net = s;
                    app.graph_disk = s;
                }
                3 => app.net_graph_coupled = !app.net_graph_coupled,
                4 => app.net_graph_scale_ratio = next_scale_ratio(app.net_graph_scale_ratio),
                _ => {}
            },
            // n/d/a 直达键保留(下一模式);c/r 跳到耦合缩放行
            KeyCode::Char('n') => app.graph_net = app.graph_net.next_with_block(),
            KeyCode::Char('d') => app.graph_disk = app.graph_disk.next_with_block(),
            KeyCode::Char('a') => {
                let s = app.graph_net.next_with_block();
                app.graph_net = s;
                app.graph_disk = s;
            }
            KeyCode::Char('c') => app.net_graph_coupled = !app.net_graph_coupled,
            KeyCode::Char('r') => {
                app.net_graph_scale_ratio = next_scale_ratio(app.net_graph_scale_ratio)
            }
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('m') => app.menu = MenuState::None,
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
            KeyCode::Up => {
                if app.detail_sel > 0 {
                    app.detail_sel -= 1;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if app.detail_sel + 1 < ui::detail_field_count(app, b, id) {
                    app.detail_sel += 1;
                }
            }
            KeyCode::Char('y') => {
                if let Some(text) = ui::detail_selected_value(app, b, id) {
                    osc52_copy(&text);
                }
            }
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
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('m') => app.menu = MenuState::None,
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
                            // kill(0) 探活(EPERM = 活着但没权限发信号)——
                            // 跨平台,Linux /proc 探测的同义替换
                            let alive = unsafe { libc::kill(pid as libc::pid_t, 0) } == 0
                                || std::io::Error::last_os_error().raw_os_error()
                                    == Some(libc::EPERM);
                            if !alive {
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

const SCALE_STEPS: [u32; 4] = [2, 5, 10, 20];

fn next_scale_ratio(cur: u32) -> u32 {
    SCALE_STEPS
        .iter()
        .find(|&&s| s > cur)
        .copied()
        .unwrap_or(SCALE_STEPS[0])
}

fn prev_scale_ratio(cur: u32) -> u32 {
    SCALE_STEPS
        .iter()
        .rev()
        .find(|&&s| s < cur)
        .copied()
        .unwrap_or(*SCALE_STEPS.last().unwrap())
}

fn handle_mouse(app: &mut App, m: &crossterm::event::MouseEvent) {
    use crossterm::event::MouseButton;
    // detail 弹窗打开时接管鼠标:单击字段行移动游标、双击复制字段值、
    // 滚轮逐字段步进、点击弹窗外关闭面板
    if let crate::app::MenuState::Detail(b, id) = app.menu {
        let dbl = app.last_click.is_some_and(|(t0, c0, r0)| {
            t0.elapsed() < std::time::Duration::from_millis(400)
                && c0 == m.column
                && r0 == m.row
        });
        app.last_click = Some((std::time::Instant::now(), m.column, m.row));
        match m.kind {
            MouseEventKind::ScrollUp => {
                app.detail_sel = app.detail_sel.saturating_sub(1);
            }
            MouseEventKind::ScrollDown => {
                let n = ui::detail_field_count(app, b, id);
                app.detail_sel = (app.detail_sel + 1).min(n.saturating_sub(1));
            }
            MouseEventKind::Down(MouseButton::Left) => {
                match ui::detail_hit_test(app, b, id, m.row) {
                    ui::DetailHit::Field(idx, value) => {
                        app.detail_sel = idx;
                        if dbl {
                            osc52_copy(&value);
                            app.last_click = None;
                        }
                    }
                    ui::DetailHit::Heading => {}
                    ui::DetailHit::Outside => {
                        app.menu = crate::app::MenuState::None;
                        app.last_click = None;
                    }
                }
            }
            _ => {}
        }
        return;
    }
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
            // 双击(同格 400ms 内两次):直接打开该行 detail(procs/conns)
            if let Some((t0, c0, r0)) = app.last_click {
                if t0.elapsed() < std::time::Duration::from_millis(400)
                    && c0 == m.column
                    && r0 == m.row
                {
                    app.last_click = None;
                    let idx = ui::hit_box(m.column, m.row);
                    if let Some(b) = idx {
                        // 仅 procs/conns 的普通行开详情:聚合父行(首击已切换
                        // 展开,id 是 hash,弹详情只会得到 "is gone")与 lan 行
                        // (无 detail 分支,弹空窗)不开;游标归零防残留
                        if matches!(b, app::BOX_PROCS | app::BOX_CONNS)
                            && !ui::selected_is_agg(app, b)
                        {
                            app.detail_sel = 0;
                            if let Some(id) = ui::selected_id(app, b) {
                                app.menu = crate::app::MenuState::Detail(b, id);
                            }
                        }
                    }
                    return;
                }
            }
            app.last_click = Some((std::time::Instant::now(), m.column, m.row));
            // clicking a procs/conns data row selects (and focuses) it;
            // clicking anywhere else cancels every focus
            let hit = ui::hit_box(m.column, m.row)
                .filter(|&idx| matches!(idx, app::BOX_PROCS | app::BOX_CONNS | app::BOX_LAN))
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

/// AI-friendly structured snapshot: two sampling rounds (rates), JSON out
pub fn json_snapshot() -> Result<()> {
    let mut app = App::new(None);
    app.update();
    std::thread::sleep(Duration::from_millis(1200));
    app.update();
    let snap = snapshot::from_app(&app);
    println!("{}", serde_json::to_string_pretty(&snap)?);
    Ok(())
}

/// field reference for `--json` consumers (machine-parseable, versioned)
pub fn print_schema() -> Result<()> {
    let fields: &[(&str, &str, &str)] = &[
        (
            "schema",
            "u32",
            "snapshot schema version (additive-only within a version)",
        ),
        ("meta.hostname", "string", "host name"),
        ("meta.ts", "u64", "unix timestamp (seconds)"),
        ("meta.version", "string", "ltop version"),
        ("meta.os / meta.arch", "string", "platform"),
        (
            "meta.udp_helper",
            "bool",
            "eBPF helper snapshot fresh (Linux only)",
        ),
        ("net.selected", "string", "interface shown in the TUI"),
        (
            "net.interfaces[].name/operstate/mac",
            "string",
            "interface identity",
        ),
        (
            "net.interfaces[].speed_mbps",
            "u64?",
            "link speed (None on Wi-Fi)",
        ),
        ("net.interfaces[].addresses[]", "string", "CIDR addresses"),
        (
            "net.interfaces[].rx_bps/tx_bps",
            "f64",
            "instantaneous bytes/s",
        ),
        (
            "net.interfaces[].session_rx/session_tx",
            "u64",
            "bytes since ltop start",
        ),
        (
            "net.interfaces[].alive_rx/alive_tx",
            "u64",
            "bytes since interface up",
        ),
        ("lan.subnets[]", "string", "observed subnets (CIDR)"),
        ("lan.gateways[]", "string", "default gateways"),
        (
            "lan.neighbours[].ip/mac/hostname/iface",
            "string",
            "neighbour identity; mac is null on macOS (system sanitizes)",
        ),
        (
            "lan.neighbours[].is_gateway/is_self/tracked",
            "bool",
            "role flags",
        ),
        (
            "disk.devices[].name/mounts[]/fstype",
            "string",
            "device identity",
        ),
        ("disk.devices[].total/used", "u64", "capacity (bytes)"),
        (
            "disk.devices[].read_bps/write_bps/iops",
            "f64",
            "throughput",
        ),
        (
            "disk.session_read/session_write",
            "u64",
            "bytes since ltop start",
        ),
        (
            "conns[].proto/local/remote/state",
            "string",
            "connection identity (state: LISTEN/ESTAB/UDP)",
        ),
        (
            "conns[].process/pid/user",
            "string|u32|null",
            "owner (null when not owned by you)",
        ),
        ("conns[].inode", "u64", "stable row key"),
        (
            "conns[].bytes_recv/bytes_sent",
            "u64",
            "cumulative per-socket bytes",
        ),
        ("conns[].bps_recv/bps_sent", "f64", "instantaneous bytes/s"),
        (
            "procs[].pid/name/alive",
            "u32|string|bool",
            "process identity",
        ),
        (
            "procs[].net_recv/net_sent/udp_recv/udp_sent",
            "u64",
            "session Σ bytes",
        ),
        (
            "procs[].net_bps_recv/net_bps_sent",
            "f64",
            "network bytes/s",
        ),
        (
            "procs[].disk_read/disk_written/disk_bps_read/disk_bps_written",
            "u64|f64",
            "disk I/O (own-uid processes)",
        ),
    ];
    let out = serde_json::json!({
        "schema": snapshot::SCHEMA,
        "command": "ltop --json",
        "fields": fields.iter().map(|(p, t, d)| serde_json::json!({
            "path": p, "type": t, "description": d,
        })).collect::<Vec<_>>(),
    });
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
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
            name,
            info.operstate,
            info.speed_mbps.unwrap_or(0),
            info.mac,
            rx,
            tx,
            info.addrs
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
            // mDNS/NBNS 主机名来自 LAN 对端:--dump 裸写终端,剥控制符
            sanitize_text(n.hostname.as_deref().unwrap_or_default()),
            if n.is_gateway { "gw " } else { "" },
            if n.is_self { "self" } else { "" },
        );
    }

    println!("== CONNS == total {}", app.conn.conns.len());
    for c in app.conn.conns.iter().take(12) {
        let proc_str = match (&c.proc, c.pid) {
            (Some(n), Some(p)) => format!("{}({p})", sanitize_text(n)),
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
            format!("{}({})", sanitize_text(&p.name), p.pid),
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
        "snapshot {}  fresh {}  pids {}  sockets {}  daemon schema {}",
        app.udp.path.display(),
        app.udp.fresh,
        app.udp.pids_seen,
        app.udp.sks_seen,
        // None = pre-versioning daemon (≤0.2.11); a value above
        // SNAPSHOT_VERSION means the helper is newer than this build
        app.udp
            .remote_ver
            .map_or("pre-versioning (<0.2.12)".to_string(), |v| v.to_string())
    );
    for d in app.udp.last_inodes.iter().take(8) {
        let conn = app
            .conn
            .conns
            .iter()
            .find(|c| c.inode == d.inode)
            .map(|c| {
                format!(
                    "{} -> {}",
                    collect::conn::sock_str(&c.local),
                    collect::conn::sock_str(&c.remote)
                )
            })
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
    fn emit(
        nodes: &[collect::disk::TreeNode],
        prefix: String,
        depth: usize,
        out: &mut Vec<String>,
    ) {
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

/// CLI 消息语言:默认英文,config.conf 的 lang=zh 切中文(headless 路径
/// 不建 App,轻量直读配置;cfg(test) 下按测试隔离约定返回默认 En)
pub fn cli_lang_zh() -> bool {
    matches!(config::Config::load().lang, config::Lang::Zh)
}

/// EN/ZH 字面量二选一(配合 cli_lang_zh 使用;返回 String 规避闭包寿命)
pub fn pick<'a>(zh: bool, en: &'a str, c: &'a str) -> &'a str {
    if zh {
        c
    } else {
        en
    }
}

pub fn pick_s(zh: bool, en: &str, c: &str) -> String {
    pick(zh, en, c).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detail_popup_smoke_and_hit_test_outside_on_empty_data() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let mut app = App::new(None);
        app.menu = crate::app::MenuState::Detail(app::BOX_PROCS, 0);
        let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
        term.draw(|f| ui::draw(f, &app)).unwrap(); // smoke: empty-data path renders
        assert!(matches!(
            ui::detail_hit_test(&app, app::BOX_PROCS, 0, 39),
            ui::DetailHit::Outside
        ));
    }

    /// the build-time eBPF object must parse and carry all four probes plus
    /// the LRU map, so a broken clang output fails tests instead of at runtime
    /// (aya_obj 依赖仅 Linux;mac 构建跳过本测试)
    #[cfg(target_os = "linux")]
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
        assert!(app.expanded.contains("web"), "聚合行 enter 应展开组");
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
            app.lan.neighbors.insert(
                ip,
                crate::collect::lan::Neighbor {
                    ip,
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
        assert_eq!(app.lan.sorted_neighbors()[0].ip, ip2, "置顶后应排第一");

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
        handle_key(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
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

    /// 当前系统全部 pid(Linux 走 /proc,mac 走 sysctl KERN_PROC_ALL)
    fn all_pids() -> Vec<u32> {
        #[cfg(target_os = "linux")]
        {
            std::fs::read_dir("/proc")
                .expect("read /proc")
                .flatten()
                .filter_map(|e| e.file_name().to_string_lossy().parse::<u32>().ok())
                .collect()
        }
        #[cfg(target_os = "macos")]
        {
            crate::collect::macproc::list_pids()
                .into_iter()
                .map(|m| m.pid)
                .collect()
        }
    }

    /// pid 是否存活且 comm 为 name(comm 在 Linux/mac 都是 15 字符截断名)
    fn pid_runs(pid: u32, name: &str) -> bool {
        #[cfg(target_os = "linux")]
        {
            std::fs::read_to_string(format!("/proc/{pid}/comm"))
                .map(|c| c.trim() == name)
                .unwrap_or(false)
        }
        #[cfg(target_os = "macos")]
        {
            crate::collect::macproc::list_pids()
                .iter()
                .any(|m| m.pid == pid && m.comm == name)
        }
    }

    /// 找一个 comm == name 且非 exclude 的存活 pid
    fn find_running(name: &str, exclude: u32) -> Option<u32> {
        all_pids()
            .into_iter()
            .find(|&p| p != exclude && pid_runs(p, name))
    }

    #[test]
    fn start_cmd_launches_detached() {
        let before = all_pids();
        let new_pid =
            start_cmd(&["sleep".to_string(), "30".to_string()]).expect("spawn detached sleep");
        std::thread::sleep(std::time::Duration::from_millis(150));
        assert!(
            pid_runs(new_pid, "sleep"),
            "detached child must be alive and running sleep"
        );
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
            uptime_s: None,
            alive: true,
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
        assert!(
            buf_text(&term).contains("(pid "),
            "overlay renders while focused"
        );
        app.focus[app::BOX_PROCS] = false;
        let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
        term.draw(|f| ui::draw(f, &app)).unwrap();
        assert!(
            !buf_text(&term).contains("(pid "),
            "overlay gone after unfocus"
        );
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
        use crate::collect::proc::ProcRow;
        use crossterm::event::{KeyCode, MouseButton, MouseEvent, MouseEventKind};
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

    /// 钉住 arch_guard 的通过路径(测试进程即 x86_64;
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
    /// 这里防的是"加了参数忘了写帮助"。USAGE 行跟随调用名(自建软链别名同效)
    #[test]
    fn usage_covers_all_flags() {
        let u = usage_text("ltop");
        for flag in [
            "--dump",
            "--log",
            "--log-interval",
            "--follow-theme",
            "--theme",
            "--replay",
            "--top",
            "--csv",
            "-h, --help",
            "-V, --version",
            "ltop-bpf",
            "watch",
            "report",
        ] {
            assert!(u.contains(flag), "usage 缺 {flag}");
        }
        assert!(u.contains("ltop [OPTIONS]"), "USAGE 用调用名");
        assert!(
            usage_text("myalias").contains("myalias [OPTIONS]"),
            "自建软链别名同版帮助(调用名跟随机制保留)"
        );
    }

    /// zoom 只由 z 退出;esc 在 zoom 内只取消聚焦(留在全屏)——旧语义
    /// esc 直接退整个 zoom,聚焦无法单独摘除
    #[test]
    fn zoom_esc_clears_focus_only() {
        let mut app = App::new(None);
        app.focus[app::BOX_PROCS] = true;
        app.last_focus = app::BOX_PROCS;
        app.visible[app::BOX_PROCS] = true;
        handle_key(&mut app, KeyCode::Char('z'), KeyModifiers::NONE);
        assert_eq!(app.zoom, Some(app::BOX_PROCS), "z 进入 zoom");
        handle_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(!app.focus[app::BOX_PROCS], "esc 只取消聚焦");
        assert_eq!(app.zoom, Some(app::BOX_PROCS), "esc 不得退出 zoom");
        handle_key(&mut app, KeyCode::Char('z'), KeyModifiers::NONE);
        assert!(app.zoom.is_none(), "z 退出 zoom");
    }

    /// --theme 已进白名单:带文件参数不再被 early_exit 拒绝
    /// (0.2.6 及之前旗标直接报 unknown option,theme::init 的直载分支不可达)
    #[test]
    fn theme_flag_passes_early_exit() {
        assert!(early_exit(&[
            "ltop".to_string(),
            "--theme".to_string(),
            "/tmp/x.theme".to_string()
        ])
        .is_none());
        assert!(early_exit(&["ltop".to_string(), "--typo".to_string()]).is_some());
    }

    /// settings 行号 ↔ cycle_setting 分支一一对应:recording 行(5)插入后
    /// 分支没跟着移,曾把 5 号之后全部错位一格——按录制行改的是日志间隔、
    /// 按 global_kill 行改的是墓碑时长、末行 proc_groups 是死行
    #[test]
    fn settings_rows_map_to_their_values() {
        // 录制开关测试不许往真实 state 目录写文件
        let dir = std::env::temp_dir().join(format!("ltop-settings-{}", std::process::id()));
        std::env::set_var("XDG_STATE_HOME", &dir);
        // 全部 15 行都有处理分支(死行在此暴露)
        for sel in 0..15 {
            let mut probe = App::new(None);
            assert!(cycle_setting(&mut probe, sel, true), "第 {sel} 行无分支");
        }
        // 5 号行开关录制本身,不串扰日志间隔
        let mut a = App::new(None);
        let iv = a.log.interval;
        assert!(cycle_setting(&mut a, 5, true));
        assert!(a.log.enabled, "5 号行必须开启录制");
        assert_eq!(a.log.interval, iv, "5 号行不得改动日志间隔(错位)");
        // 6 号行开全局录制热键暴露
        let mut b = App::new(None);
        assert!(cycle_setting(&mut b, 6, true));
        assert!(b.allow_global_log, "6 号行必须开全局录制键");
        // 14 号行开 global kill,不动墓碑时长
        let mut c = App::new(None);
        let tomb = c.conn.tombstone_ttl;
        assert!(cycle_setting(&mut c, 14, true));
        assert!(c.allow_global_kill, "14 号行必须开 global kill");
        assert_eq!(c.conn.tombstone_ttl, tomb, "14 号行不得改动墓碑时长(错位)");
        std::env::remove_var("XDG_STATE_HOME");
        let _ = std::fs::remove_dir_all(dir);
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
            mac_hidden: false,
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
        use crate::collect::proc::ProcRow;
        use crossterm::event::KeyCode;

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
            uptime_s: None,
            alive: true,
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
        // reap the old instance promptly (the worker waits for the pid to
        // vanish via kill(0); a zombie lingers until the parent reaps it)
        let _ = child.wait();
        // give the worker time to notice and spawn the new one
        std::thread::sleep(std::time::Duration::from_millis(500));
        assert!(
            child.try_wait().expect("poll").is_some(),
            "old instance terminated"
        );
        let new_pid = find_running("sleep", pid).expect("relaunched sleep must be running");
        kill_pid(new_pid);
    }
}

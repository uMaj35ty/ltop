//! fleet:SSH 多机聚合(0.3.0-beta)。
//!
//! 管理机通过 SSH 周期拉取各节点的 `ltop --json`(schema v1),聚合为
//! 一张总览表;节点只需已装 ltop(任意 ≥0.2.3 版本),零新增软件。
//! 模块分工:`model` 纯函数(聚合/错误分类)、`poll` ssh 拉取、
//! `cfg` fleet.conf、`setup` 密钥 doctor、`ui` TUI 渲染;本文件是
//! 入口、共享态与两个前端的调度。凭据红线:BatchMode 密钥专用,不存
//! 不问密码(见 SECURITY.md fleet 段)。

mod cfg;
mod model;
mod poll;
mod setup;
mod ui;

use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, Context, Result};

use model::NodeSample;
use snapshot::Snapshot;

use crate::snapshot;

/// 轮询共享态:poller 线程写,前端(CLI 打印/TUI 绘制)读。
/// 失败轮保留上一轮 raw/last_ok(不"瞬时空白"),样本行换成错误行。
struct FleetShared {
    order: Vec<String>,
    samples: HashMap<String, NodeSample>,
    raws: HashMap<String, Snapshot>,
    last_ok: HashMap<String, u64>,
    /// Σ(非错误节点)rx/tx 走线,每轮一个样本(TUI 走线用)
    sum_hist: Vec<(f64, f64)>,
    round: u64,
}

const SUM_HIST_CAP: usize = 240;

impl FleetShared {
    fn new(nodes: &[(String, String)]) -> Self {
        FleetShared {
            order: nodes.iter().map(|(t, _)| t.clone()).collect(),
            samples: HashMap::new(),
            raws: HashMap::new(),
            last_ok: HashMap::new(),
            sum_hist: Vec::new(),
            round: 0,
        }
    }

    fn apply(&mut self, rounds: Vec<poll::NodeRound>, now: u64) {
        let mut sum = (0.0f64, 0.0f64);
        for r in rounds {
            let ok = r.sample.error.is_none();
            if ok {
                sum.0 += r.sample.rx;
                sum.1 += r.sample.tx;
                if let Some(raw) = r.raw {
                    self.raws.insert(r.sample.target.clone(), raw);
                }
                self.last_ok.insert(r.sample.target.clone(), now);
            }
            self.samples.insert(r.sample.target.clone(), r.sample);
        }
        self.round += 1;
        self.sum_hist.push(sum);
        if self.sum_hist.len() > SUM_HIST_CAP {
            self.sum_hist.remove(0);
        }
    }

    /// UI 每帧取的小视图:raws 只带详情弹窗正打开的那个(避免整帧克隆
    /// 全部快照)
    fn view(&self, detail: Option<&str>) -> ui::UiView {
        let mut raws = HashMap::new();
        if let Some(t) = detail {
            if let Some(r) = self.raws.get(t) {
                raws.insert(t.to_string(), r.clone());
            }
        }
        ui::UiView {
            order: self.order.clone(),
            samples: self.samples.clone(),
            raws,
            round: self.round,
            sum_hist: self.sum_hist.clone(),
        }
    }
}

/// 一轮:逐节点并发拉取(每节点一个线程),按节点表顺序返回
fn poll_round(
    nodes: &[(String, String)],
    poll: Duration,
    master: bool,
    zh: bool,
) -> Vec<poll::NodeRound> {
    let mut handles = Vec::new();
    for (t, c) in nodes {
        let (t, c, poll) = (t.clone(), c.clone(), poll);
        handles.push(std::thread::spawn(move || {
            poll::pull(&t, &c, poll, master, zh)
        }));
    }
    handles.into_iter().filter_map(|h| h.join().ok()).collect()
}

/// 后台轮询线程:每轮 apply 进共享态;stop 置位后本轮结束即退出
fn spawn_poller(
    nodes: Vec<(String, String)>,
    interval: u64,
    timeout: u64,
    master: bool,
    zh: bool,
    shared: Arc<Mutex<FleetShared>>,
    stop: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let poll = Duration::from_secs(timeout);
        loop {
            let rounds = poll_round(&nodes, poll, master, zh);
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            if let Ok(mut s) = shared.lock() {
                s.apply(rounds, now);
            }
            // 分片睡眠:stop 一到即响应,不等满 interval
            for _ in 0..interval * 5 {
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    })
}

const FLEET_USAGE: &str = "ltop fleet [OPTIONS]        poll nodes and aggregate
ltop fleet setup <TARGET>   install a key on a node and verify it

OPTIONS:
    --host <TARGET>   node to poll, repeatable (SSH target; key auth
                      required; reads ~/.config/ltop/fleet.conf when
                      omitted)
    --cmd <COMMAND>   remote command for the most recent --host (or the
                      default for later ones). default: ltop --json — on
                      macOS give the full path, non-interactive ssh has no
                      PATH additions
    --save            persist the effective node list to fleet.conf
    --once            one round and exit, non-interactive (default: TUI
                      when on a terminal, else the same one-line-per-round
                      loop)
    --interval <S>    seconds between rounds (default 5, min 1)
    --timeout <S>     per-node pull timeout (default 15)
    -h, --help        this help";

pub struct FleetOpts {
    /// None = 用 fleet.conf 的节点表
    pub hosts: Option<Vec<(String, String)>>,
    pub once: bool,
    pub interval: Option<u64>,
    pub timeout: Option<u64>,
    pub save: bool,
}

/// None = 已打印 help(正常退出路径)
pub fn parse_args(args: &[String]) -> Result<Option<FleetOpts>> {
    let zh = crate::cli_lang_zh();
    let mut hosts: Vec<(String, String)> = Vec::new();
    let mut cmd = cfg::DEFAULT_CMD.to_string();
    let mut once = false;
    let mut interval = None;
    let mut timeout = None;
    let mut save = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                println!("{FLEET_USAGE}");
                return Ok(None);
            }
            "--host" => {
                let v = args.get(i + 1).context(crate::pick_s(
                    zh,
                    "--host needs a target",
                    "--host 需要一个目标",
                ))?;
                hosts.push((v.clone(), cmd.clone()));
                i += 1;
            }
            // --cmd 应用于最近一个 --host(其前则成为后续主机的默认)
            "--cmd" => {
                let v = args
                    .get(i + 1)
                    .context(crate::pick_s(zh, "--cmd needs a value", "--cmd 需要一个值"))?
                    .clone();
                match hosts.last_mut() {
                    Some(h) => h.1 = v,
                    None => cmd = v,
                }
                i += 1;
            }
            "--save" => save = true,
            "--once" => once = true,
            "--interval" => {
                interval = Some(
                    args.get(i + 1)
                        .and_then(|v| v.parse::<u64>().ok())
                        .context(crate::pick_s(
                            zh,
                            "--interval needs a number of seconds",
                            "--interval 需要一个秒数",
                        ))?
                        .max(1),
                );
                i += 1;
            }
            "--timeout" => {
                timeout = Some(
                    args.get(i + 1)
                        .and_then(|v| v.parse::<u64>().ok())
                        .context(crate::pick_s(
                            zh,
                            "--timeout needs a number of seconds",
                            "--timeout 需要一个秒数",
                        ))?
                        .max(1),
                );
                i += 1;
            }
            a if a.starts_with('-') => {
                bail!("ltop fleet: unknown option {a} (try --help)");
            }
            a => bail!(
                "{}",
                crate::pick_s(
                    zh,
                    &format!(
                        "ltop fleet: unexpected argument {a:?} (options only; to set up a node use `ltop fleet setup`)",
                    ),
                    &format!(
                        "ltop fleet: 多余的参数 {a:?}(只收选项;配置节点用 `ltop fleet setup`)"
                    )
                )
            ),
        }
        i += 1;
    }
    let hosts = if hosts.is_empty() { None } else { Some(hosts) };
    Ok(Some(FleetOpts {
        hosts,
        once,
        interval,
        timeout,
        save,
    }))
}

pub fn run(args: &[String]) -> Result<()> {
    // `ltop fleet setup <target>`:密钥 doctor,独立路径,不进轮询
    if args.first().map(String::as_str) == Some("setup") {
        return setup::run(&args[1..]);
    }

    let Some(opts) = parse_args(args)? else {
        return Ok(());
    };
    let zh = crate::cli_lang_zh();

    // 节点表:CLI 覆盖配置;--save 把生效表写回 fleet.conf
    let mut conf = cfg::FleetConf::load();
    if let Some(hosts) = &opts.hosts {
        conf.merge_hosts(hosts);
    }
    if conf.nodes.is_empty() {
        bail!(
            "{}",
            crate::pick_s(
                zh,
                "ltop fleet: no nodes — pass --host or add nodes to ~/.config/ltop/fleet.conf; to set up a node run `ltop fleet setup <target>`",
                "ltop fleet: 没有节点 — 用 --host 或在 ~/.config/ltop/fleet.conf 添加;配置新节点用 `ltop fleet setup <目标>`"
            )
        );
    }
    if opts.save {
        conf.save().map_err(|e| {
            anyhow::anyhow!(
                "{}",
                crate::pick_s(
                    zh,
                    &format!("ltop fleet: cannot write fleet.conf: {e}"),
                    &format!("ltop fleet: 写 fleet.conf 失败:{e}")
                )
            )
        })?;
        println!(
            "{}",
            crate::pick_s(
                zh,
                &format!(
                    "saved {} node(s) to {}",
                    conf.nodes.len(),
                    cfg::FleetConf::path()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default()
                ),
                &format!(
                    "已保存 {} 个节点到 {}",
                    conf.nodes.len(),
                    cfg::FleetConf::path()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default()
                )
            )
        );
    }

    let interval = opts.interval.unwrap_or(conf.interval).max(1);
    let timeout = opts.timeout.unwrap_or(conf.timeout).max(1);
    let master = conf.master;
    let nodes: Vec<(String, String)> = conf
        .nodes
        .iter()
        .map(|n| {
            (
                n.target.clone(),
                n.cmd
                    .clone()
                    .unwrap_or_else(|| cfg::DEFAULT_CMD.to_string()),
            )
        })
        .collect();

    // --once 永远走非交互(脚本/CI 语义);tty 上默认 TUI,否则逐轮打印
    let interactive = !opts.once && std::io::IsTerminal::is_terminal(&std::io::stdout());
    if interactive {
        run_tui(nodes, interval, timeout, master, zh)
    } else {
        run_cli(nodes, interval, timeout, master, zh, opts.once)
    }
}

/// CLI 前端:逐轮打印(非 tty 的形态,也是将来无头 daemon 的雏形)
fn run_cli(
    nodes: Vec<(String, String)>,
    interval: u64,
    timeout: u64,
    master: bool,
    zh: bool,
    once: bool,
) -> Result<()> {
    let poll = Duration::from_secs(timeout);
    let mut state = FleetShared::new(&nodes);
    loop {
        let rounds = poll_round(&nodes, poll, master, zh);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        state.apply(rounds, now);
        print!("\n[t+{now}] round {}\n{}", state.round, render(&state, zh));
        std::io::stdout().flush().ok();
        if once {
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs(interval));
    }
}

/// TUI 前端:poller 后台线程 + 自有事件循环(AlternateScreen)。
/// 键位:↑↓ 选择 / enter 详情 / h 帮助 / esc 关弹窗 / q 退出。
fn run_tui(
    nodes: Vec<(String, String)>,
    interval: u64,
    timeout: u64,
    master: bool,
    zh: bool,
) -> Result<()> {
    use crossterm::event::{
        Event, KeyCode, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags,
        PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    };
    use crossterm::execute;
    use crossterm::terminal::{
        disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
    };
    use ratatui::{backend::CrosstermBackend, Terminal};

    crate::theme::init(); // fleet 路径不经过主 TUI 的初始化,这里补上

    type Tui = Terminal<CrosstermBackend<std::io::Stdout>>;
    fn setup() -> Result<Tui> {
        enable_raw_mode()?;
        // 与主 TUI 同款:DISAMBIGUATE_ESCAPE_CODES 消除 ssh 分包下 esc
        // 与 CSI 序列的歧义;fleet 无鼠标交互,不抓鼠标
        execute!(
            std::io::stdout(),
            EnterAlternateScreen,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )?;
        Ok(Terminal::new(CrosstermBackend::new(std::io::stdout()))?)
    }
    fn restore() {
        let _ = disable_raw_mode();
        let _ = execute!(
            std::io::stdout(),
            PopKeyboardEnhancementFlags,
            LeaveAlternateScreen
        );
    }
    // panic/错误路径也要还终端:守卫兜底恢复
    struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            restore();
        }
    }

    let _guard = Guard;
    let mut terminal = setup()?;

    let shared = Arc::new(Mutex::new(FleetShared::new(&nodes)));
    let stop = Arc::new(AtomicBool::new(false));
    let poller = spawn_poller(
        nodes,
        interval,
        timeout,
        master,
        zh,
        Arc::clone(&shared),
        Arc::clone(&stop),
    );

    let mut ui = ui::UiState::new();
    let res = (|| -> Result<()> {
        loop {
            let detail = ui.detail.clone();
            let view = shared
                .lock()
                .map(|s| s.view(detail.as_deref()))
                .unwrap_or(ui::UiView {
                    order: Vec::new(),
                    samples: HashMap::new(),
                    raws: HashMap::new(),
                    round: 0,
                    sum_hist: Vec::new(),
                });
            terminal.draw(|f| ui::draw(f, &view, &mut ui, zh))?;

            if crossterm::event::poll(Duration::from_millis(250))? {
                let ev = crossterm::event::read()?;
                if let Event::Key(k) = ev {
                    if k.kind != KeyEventKind::Press {
                        continue;
                    }
                    match k.code {
                        KeyCode::Char('q') => return Ok(()),
                        KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                            return Ok(())
                        }
                        KeyCode::Char('h') | KeyCode::Char('?') => ui.help = !ui.help,
                        KeyCode::Esc => {
                            ui.help = false;
                            ui.detail = None;
                            ui.detail_scroll = 0;
                        }
                        KeyCode::Down if ui.help => {}
                        KeyCode::Down if ui.detail.is_some() => {
                            ui.detail_scroll = ui.detail_scroll.saturating_add(1)
                        }
                        KeyCode::Down => {
                            let n = view.order.len().max(1);
                            ui.selected = (ui.selected + 1) % n;
                        }
                        KeyCode::Up if ui.help => {}
                        KeyCode::Up if ui.detail.is_some() => {
                            ui.detail_scroll = ui.detail_scroll.saturating_sub(1)
                        }
                        KeyCode::Up => {
                            let n = view.order.len().max(1);
                            ui.selected = (ui.selected + n - 1) % n;
                        }
                        KeyCode::Enter if !ui.help => {
                            let t = view.order.get(ui.selected).cloned();
                            match &ui.detail {
                                Some(open) if Some(open) == t.as_ref() => {
                                    ui.detail = None;
                                    ui.detail_scroll = 0;
                                }
                                _ => {
                                    ui.detail = t;
                                    ui.detail_scroll = 0;
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    })();

    stop.store(true, Ordering::Relaxed);
    let _ = poller.join();
    drop(terminal);
    restore();
    std::io::stdout().flush().ok();
    res
}

/// CLI 总览表(非 tty 形态)。远端字符串全部 sanitize 后才落表。
fn render(state: &FleetShared, zh: bool) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{:<24} {:>10} {:>10} {:>6} {:>6} {:>6}  {}\n",
        "node", "rx", "tx", "procs", "conns", "ms", "top talker"
    ));
    for target in &state.order {
        let Some(r) = state.samples.get(target) else {
            out.push_str(&format!("{target:<24} …\n"));
            continue;
        };
        match &r.error {
            Some(e) => out.push_str(&format!("{:<24} ⚠ {}\n", r.target, e)),
            None => {
                let (tn, tv) = r
                    .top
                    .as_ref()
                    .map(|(n, v)| (crate::sanitize_text(n), *v))
                    .unwrap_or(("-".to_string(), 0.0));
                let host = crate::sanitize_text(&r.hostname);
                let ms = r
                    .latency_ms
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "-".into());
                out.push_str(&format!(
                    "{:<24} {:>10} {:>10} {:>6} {:>6} {:>6}  {} ({}/s)\n",
                    host,
                    crate::fmt_human(r.rx),
                    crate::fmt_human(r.tx),
                    r.procs,
                    r.conns,
                    ms,
                    tn,
                    crate::fmt_human(tv),
                ));
            }
        }
    }
    let _ = zh;
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arglist(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_host_cmd_pairing_and_flags() {
        let o = parse_args(&arglist(&[
            "--host",
            "air",
            "--cmd",
            "/opt/homebrew/bin/ltop --json",
            "--host",
            "fedora",
            "--interval",
            "3",
            "--timeout",
            "9",
            "--save",
            "--once",
        ]))
        .unwrap()
        .unwrap();
        let hosts = o.hosts.unwrap();
        assert_eq!(
            hosts[0],
            ("air".into(), "/opt/homebrew/bin/ltop --json".into())
        );
        assert_eq!(
            hosts[1],
            ("fedora".into(), "ltop --json".into()),
            "--cmd 只作用于最近一个 --host,后续主机拿默认命令"
        );
        assert_eq!(o.interval, Some(3));
        assert_eq!(o.timeout, Some(9));
        assert!(o.save);
        assert!(o.once);
    }

    #[test]
    fn no_hosts_means_use_config() {
        let o = parse_args(&arglist(&["--once"])).unwrap().unwrap();
        assert!(o.hosts.is_none());
    }

    #[test]
    fn rejects_unknown_options_and_positionals() {
        assert!(parse_args(&arglist(&["--wat"])).is_err());
        assert!(parse_args(&arglist(&["fedora"])).is_err());
        assert!(parse_args(&arglist(&["--interval"])).is_err(), "缺值");
    }

    #[test]
    fn help_short_circuits() {
        let o = parse_args(&arglist(&["-h"])).unwrap();
        assert!(o.is_none(), "help 打印后返回 None");
    }

    #[test]
    fn shared_apply_keeps_last_good_and_sums() {
        let nodes = vec![("a".to_string(), "c".to_string())];
        let mut sh = FleetShared::new(&nodes);
        let ok = poll::NodeRound {
            sample: model::aggregate("a", &model::fixture::snap("host-a", 10.0, 5.0), 7),
            raw: Some(model::fixture::snap("host-a", 10.0, 5.0)),
        };
        sh.apply(vec![ok], 1);
        assert_eq!(sh.round, 1);
        assert_eq!(sh.sum_hist.last(), Some(&(10.0, 5.0)));
        assert_eq!(sh.last_ok.get("a"), Some(&1));

        // 失败轮:样本换错误行,raw/last_ok 保留
        let bad = poll::NodeRound {
            sample: model::failed("a", "pull timed out".into()),
            raw: None,
        };
        sh.apply(vec![bad], 2);
        let view = sh.view(Some("a"));
        assert!(view.samples.get("a").unwrap().error.is_some());
        assert!(view.raws.contains_key("a"), "raw 保留");
        assert_eq!(sh.last_ok.get("a"), Some(&1), "last_ok 不被失败轮覆盖");
        assert_eq!(sh.sum_hist.last(), Some(&(0.0, 0.0)), "错误节点不计入合计");
    }

    #[test]
    fn render_sanitizes_remote_strings() {
        let nodes = vec![("t".to_string(), "c".to_string())];
        let mut s = model::fixture::snap("\x1b]0;evil\x07node", 10.0, 5.0);
        if let Some(p) = s.procs.first_mut() {
            p.name = "\x1b[31msneaky\x1b[0m".into();
        }
        let mut sh = FleetShared::new(&nodes);
        sh.apply(
            vec![poll::NodeRound {
                sample: model::aggregate("t", &s, 5),
                raw: None,
            }],
            1,
        );
        let out = render(&sh, false);
        assert!(!out.contains('\x1b'), "控制符必须被剥掉:{out:?}");
        assert!(out.contains("evilnode"));
    }

    #[test]
    fn render_shows_pending_placeholder_before_first_round() {
        let nodes = vec![("nas".to_string(), "c".to_string())];
        let sh = FleetShared::new(&nodes);
        let out = render(&sh, false);
        assert!(out.contains("nas"), "首帧前节点就要在表上: {out:?}");
    }
}

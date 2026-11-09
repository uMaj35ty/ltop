//! fleet:SSH 多机聚合(0.3.0-beta)。
//!
//! 管理机通过 SSH 周期拉取各节点的 `ltop --json`(schema v1),聚合为
//! 一张总览表;节点只需已装 ltop(任意 ≥0.2.3 版本),零新增软件。
//! 模块分工:`model` 纯函数(聚合/错误分类)、`poll` ssh 拉取、
//! `cfg` fleet.conf、`setup` 密钥 doctor;本文件是入口与 CLI 渲染。
//! 凭据红线:BatchMode 密钥专用,不存不问密码(见 SECURITY.md fleet 段)。

mod cfg;
mod model;
mod poll;
mod setup;

use std::collections::HashMap;
use std::io::Write;
use std::time::Duration;

use anyhow::{bail, Context, Result};

use model::NodeSample;
use snapshot::Snapshot;

use crate::snapshot;

/// 一个节点的常驻状态:最新样本 + 最近一份成功快照(批次② TUI 详情用)
struct NodeState {
    sample: NodeSample,
    /// 最近一次成功的原始快照;失败轮保留上一次(表里以 dim 陈旧呈现)
    raw: Option<Snapshot>,
    /// 最近一次成功的时间戳(秒)
    last_ok: Option<u64>,
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
    --once            one round and exit (default: loop)
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
                    &format!("ltop fleet: unexpected argument {a:?} (options only; to set up a node use `ltop fleet setup`)",),
                    &format!("ltop fleet: 多余的参数 {a:?}(只收选项;配置节点用 `ltop fleet setup`)")
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
    let poll = Duration::from_secs(timeout);

    // 常驻状态:失败轮保留上一轮的 raw/last_ok(不"瞬时空白"),样本行换成错误行
    let mut state: HashMap<String, NodeState> = HashMap::new();
    let mut round: u64 = 0;
    loop {
        round += 1;
        let mut handles = Vec::new();
        for (t, c) in &nodes {
            let (t, c, poll) = (t.clone(), c.clone(), poll);
            handles.push(std::thread::spawn(move || {
                poll::pull(&t, &c, poll, master, zh)
            }));
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        for h in handles {
            let Ok(r) = h.join() else { continue };
            let target = r.sample.target.clone();
            let ok = r.sample.error.is_none();
            use std::collections::hash_map::Entry;
            match state.entry(target) {
                Entry::Vacant(v) => {
                    v.insert(NodeState {
                        sample: r.sample,
                        raw: r.raw,
                        last_ok: if ok { Some(now) } else { None },
                    });
                }
                Entry::Occupied(mut o) => {
                    let e = o.get_mut();
                    e.sample = r.sample;
                    if ok {
                        e.raw = r.raw;
                        e.last_ok = Some(now);
                    }
                }
            }
        }
        print!("\n[{}] round {round}\n{}", now_hhmmss(), render(&state, zh));
        std::io::stdout().flush().ok();
        if opts.once {
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs(interval));
    }
}

fn now_hhmmss() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // 本地时区不引入 chrono:直接展示 UTC 无意义,改用相对轮次即可。
    // 这里保留秒数便于确认轮询确实在走
    format!("t+{secs}")
}

/// CLI 总览表(批次②由 TUI 取代)。远端字符串全部 sanitize 后才落表。
fn render(state: &HashMap<String, NodeState>, zh: bool) -> String {
    let mut rows: Vec<&NodeState> = state.values().collect();
    rows.sort_by(|a, b| a.sample.target.cmp(&b.sample.target));
    let mut out = String::new();
    out.push_str(&format!(
        "{:<24} {:>10} {:>10} {:>6} {:>6} {:>6}  {}\n",
        "node", "rx", "tx", "procs", "conns", "ms", "top talker"
    ));
    for st in rows {
        let r = &st.sample;
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
    fn render_sanitizes_remote_strings() {
        let mut s = model::fixture::snap("\x1b]0;evil\x07node", 10.0, 5.0);
        if let Some(p) = s.procs.first_mut() {
            p.name = "\x1b[31msneaky\x1b[0m".into();
        }
        let n = model::aggregate("t", &s, 5);
        let mut st = HashMap::new();
        st.insert(
            "t".to_string(),
            NodeState {
                sample: n,
                raw: None,
                last_ok: None,
            },
        );
        let out = render(&st, false);
        assert!(!out.contains('\x1b'), "控制符必须被剥掉:{out:?}");
        assert!(out.contains("evilnode"));
    }
}

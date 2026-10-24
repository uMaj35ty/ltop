//! fleet:SSH 多机聚合(0.3.0-beta 验证分支的最小实现)。
//!
//! 管理机通过 SSH 周期拉取各节点的 `ltop --json`(schema v1),聚合为
//! 一张总览表;节点只需已装 ltop(任意 ≥0.2.3 版本),零新增软件。
//! 本阶段目标是**验证多机聚合的可行性**(异构节点、认证、延迟、schema
//! 兼容),不是最终形态:fleet capture/report 的多主机维度在后续迭代。

use std::io::Write;
use std::process::Command;
use std::time::Duration;

use anyhow::{bail, Context, Result};

use crate::snapshot::Snapshot;

/// 一个节点一轮的聚合样本
pub struct NodeSample {
    pub target: String,
    pub hostname: String,
    /// 非 lo 接口的 rx/tx 速率合计(B/s)
    pub rx: f64,
    pub tx: f64,
    pub procs: usize,
    pub conns: usize,
    /// 最活跃进程(按 net_bps 收发合计)
    pub top: Option<(String, f64)>,
    /// 拉取/解析失败的原因(该节点本轮缺席)
    pub error: Option<String>,
}

/// 从一台节点的 schema v1 快照聚合出样本(纯函数,双平台可测)
pub fn aggregate(target: &str, snap: &Snapshot) -> NodeSample {
    let mut rx = 0.0;
    let mut tx = 0.0;
    for i in &snap.net.interfaces {
        if i.name == "lo" {
            continue;
        }
        rx += i.rx_bps;
        tx += i.tx_bps;
    }
    let top = snap
        .procs
        .iter()
        .map(|p| (p.name.clone(), p.net_bps_recv + p.net_bps_sent))
        .max_by(|a, b| a.1.total_cmp(&b.1));
    NodeSample {
        target: target.to_string(),
        hostname: snap.meta.hostname.clone(),
        rx,
        tx,
        procs: snap.procs.len(),
        conns: snap.conns.len(),
        top,
        error: None,
    }
}

fn failed(target: &str, err: String) -> NodeSample {
    NodeSample {
        target: target.to_string(),
        hostname: String::new(),
        rx: 0.0,
        tx: 0.0,
        procs: 0,
        conns: 0,
        top: None,
        error: Some(err),
    }
}

/// SSH 拉取一个节点(并发模型:每节点一个线程,超时即该节点缺席)
fn pull(target: &str, cmd: &str, timeout: Duration) -> NodeSample {
    use std::sync::mpsc;
    let (tx, rx) = mpsc::channel();
    let (t, c) = (target.to_string(), cmd.to_string());
    std::thread::spawn(move || {
        // BatchMode:密钥不通时立刻失败,不挂住等密码输入
        let out = Command::new("ssh")
            .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=5", &t, &c])
            .output();
        let _ = tx.send(match out {
            Ok(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).into_owned()),
            Ok(o) => Err(format!(
                "remote exited {}: {}",
                o.status,
                String::from_utf8_lossy(&o.stderr).trim()
            )),
            Err(e) => Err(e.to_string()),
        });
    });
    let text: String = match rx.recv_timeout(timeout) {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => return failed(target, e),
        Err(_) => return failed(target, format!("timeout after {}s", timeout.as_secs())),
    };
    match serde_json::from_str::<Snapshot>(&text) {
        Ok(snap) => aggregate(target, &snap),
        Err(e) => failed(target, format!("not a schema-v1 snapshot: {e}")),
    }
}

fn render(rows: &[NodeSample]) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{:<24} {:>10} {:>10} {:>6} {:>6}  {}\n",
        "node", "rx", "tx", "procs", "conns", "top talker"
    ));
    for r in rows {
        match &r.error {
            Some(e) => out.push_str(&format!("{:<24} ⚠ {}\n", r.target, e)),
            None => {
                let (tn, tv) = r
                    .top
                    .as_ref()
                    .map(|(n, v)| (n.as_str(), *v))
                    .unwrap_or(("-", 0.0));
                out.push_str(&format!(
                    "{:<24} {:>10} {:>10} {:>6} {:>6}  {} ({}/s)\n",
                    r.hostname,
                    crate::fmt_human(r.rx),
                    crate::fmt_human(r.tx),
                    r.procs,
                    r.conns,
                    tn,
                    crate::fmt_human(tv),
                ));
            }
        }
    }
    out
}

const FLEET_USAGE: &str = "ltop fleet [OPTIONS]

USAGE:
    ltop fleet --host <[user@]host>... [OPTIONS]

    --host <TARGET>   node to poll, repeatable (SSH target; key auth required)
    --cmd <COMMAND>   remote command for the most recent --host (or the
                      default for later ones). default: ltop --json — on
                      macOS give the full path, non-interactive ssh has no
                      PATH additions
    --once            one round and exit (default: loop)
    --interval <S>    seconds between rounds (default 5, min 1)
    --timeout <S>     per-node pull timeout (default 15)
    -h, --help        this help";

/// fleet 的可选项:每主机独立远端命令(mac 无交互 PATH 下需要全路径)
pub struct FleetOpts {
    pub hosts: Vec<(String, String)>,
    pub once: bool,
    pub interval: u64,
    pub timeout: u64,
}

/// None = 已打印 help(正常退出路径)
pub fn parse_args(args: &[String]) -> Result<Option<FleetOpts>> {
    let zh = crate::cli_lang_zh();
    let mut hosts: Vec<(String, String)> = Vec::new();
    let mut cmd = "ltop --json".to_string();
    let mut once = false;
    let mut interval = 5u64;
    let mut timeout = 15u64;
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
            "--once" => once = true,
            "--interval" => {
                interval = args
                    .get(i + 1)
                    .and_then(|v| v.parse::<u64>().ok())
                    .context(crate::pick_s(
                        zh,
                        "--interval needs a number of seconds",
                        "--interval 需要一个秒数",
                    ))?
                    .max(1);
                i += 1;
            }
            "--timeout" => {
                timeout = args
                    .get(i + 1)
                    .and_then(|v| v.parse::<u64>().ok())
                    .context(crate::pick_s(
                        zh,
                        "--timeout needs a number of seconds",
                        "--timeout 需要一个秒数",
                    ))?
                    .max(1);
                i += 1;
            }
            a if a.starts_with("--") => {
                bail!("ltop fleet: unknown option {a} (try --help)");
            }
            a => bail!("ltop fleet: unexpected argument {a:?} (options only, try --help)"),
        }
        i += 1;
    }
    if hosts.is_empty() {
        bail!(crate::pick_s(
            zh,
            "ltop fleet: at least one --host is required (try --help)",
            "ltop fleet: 至少需要一个 --host(见 --help)"
        ));
    }
    Ok(Some(FleetOpts {
        hosts,
        once,
        interval,
        timeout,
    }))
}

pub fn run(args: &[String]) -> Result<()> {
    let Some(opts) = parse_args(args)? else {
        return Ok(());
    };
    let hosts = &opts.hosts;

    let poll = Duration::from_secs(opts.timeout);
    loop {
        let mut samples: Vec<NodeSample> = Vec::new();
        let mut handles = Vec::new();
        for (t, c) in hosts {
            let (t, c, poll) = (t.clone(), c.clone(), poll);
            handles.push(std::thread::spawn(move || pull(&t, &c, poll)));
        }
        for h in handles {
            if let Ok(s) = h.join() {
                samples.push(s);
            }
        }
        print!("\n{}", render(&samples));
        std::io::stdout().flush().ok();
        if opts.once {
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs(opts.interval));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(name: &str, rx: f64, tx: f64) -> Snapshot {
        use crate::snapshot::*;
        Snapshot {
            schema: SCHEMA,
            meta: Meta {
                hostname: name.to_string(),
                ts: 1,
                version: "test".into(),
                os: "linux".into(),
                arch: "aarch64".into(),
                udp_helper: false,
            },
            net: NetSnapshot {
                selected: "en0".into(),
                interfaces: vec![IfaceSnapshot {
                    name: "en0".into(),
                    operstate: "up".into(),
                    speed_mbps: None,
                    duplex: None,
                    mac: "aa:aa".into(),
                    addresses: vec![],
                    rx_bps: rx,
                    tx_bps: tx,
                    session_rx: 0,
                    session_tx: 0,
                    alive_rx: 0,
                    alive_tx: 0,
                }],
            },
            lan: LanSnapshot {
                subnets: vec![],
                gateways: vec![],
                neighbours: vec![],
            },
            disk: DiskSnapshot {
                devices: vec![],
                session_read: 0,
                session_write: 0,
            },
            conns: vec![ConnSnapshot {
                proto: "tcp".into(),
                local: "127.0.0.1:1".into(),
                remote: "127.0.0.1:2".into(),
                state: "ESTAB".into(),
                process: Some("top".into()),
                pid: Some(1),
                user: "u".into(),
                inode: 1,
                bytes_recv: 0,
                bytes_sent: 0,
                bps_recv: rx,
                bps_sent: 0.0,
            }],
            procs: vec![ProcSnapshot {
                pid: 1,
                name: "top".into(),
                net_recv: 0,
                net_sent: 0,
                net_bps_recv: rx,
                net_bps_sent: 0.0,
                udp_recv: 0,
                udp_sent: 0,
                disk_read: 0,
                disk_written: 0,
                disk_bps_read: 0.0,
                disk_bps_written: 0.0,
                alive: true,
            }],
        }
    }

    /// 聚合口径:lo 不计入,rx/tx 合计,最活跃进程取收发速率和最大者
    #[test]
    fn aggregates_rates_and_top_talker() {
        let mut s = snap("node-a", 100.0, 50.0);
        // 追加 lo 行:不得计入合计
        let lo: Snapshot = snap("x", 9999.0, 9999.0);
        let lo_iface = lo.net.interfaces.into_iter().map(|mut i| {
            i.name = "lo".into();
            i
        });
        s.net.interfaces.extend(lo_iface);
        let n = aggregate("a@h", &s);
        assert_eq!(n.hostname, "node-a");
        assert_eq!(n.rx, 100.0, "lo 不得计入");
        assert_eq!(n.tx, 50.0);
        assert_eq!(n.procs, 1);
        assert_eq!(n.top.as_ref().map(|(n, _)| n.as_str()), Some("top"));
    }

    /// 两节点样本互不串扰(聚合是逐快照的)
    #[test]
    fn nodes_stay_independent() {
        let a = aggregate("a", &snap("node-a", 1.0, 1.0));
        let b = aggregate("b", &snap("node-b", 500.0, 500.0));
        assert_eq!(a.rx, 1.0);
        assert_eq!(b.rx, 500.0);
    }
}

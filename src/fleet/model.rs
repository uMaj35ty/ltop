//! fleet 纯函数层:节点样本聚合、ssh 失败分类、版本比较。
//!
//! 这一层的所有函数都不碰网络/文件系统,是 fleet 的测试主力;
//! poll.rs 负责真正的拉取,把结果交给这里聚合并给出可操作的提示。

use crate::snapshot::Snapshot;

/// 一个节点一轮的聚合样本
#[derive(Clone)]
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
    /// 本轮拉取耗时(管理机侧口径,含 ssh 握手)
    pub latency_ms: Option<u64>,
    /// 拉取/解析失败的一行话(已本地化、已 sanitize;该节点本轮缺席)
    pub error: Option<String>,
}

/// ssh 失败的分类:决定给用户哪句可操作提示
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FailKind {
    /// 密钥不通(BatchMode 下立刻 Permission denied)→ 指路 fleet setup
    NeedKey,
    /// 主机指纹不在 known_hosts → 必须人工 ssh 一次确认
    HostKey,
    /// 本机私钥权限过宽(ssh 拒用)
    KeyPerms,
    /// 主机名解析失败
    Resolve,
    /// 网络层不通(refused/timeout/no route)
    Unreachable,
    /// 远端命令跑不起来(PATH/未装/版本过旧)→ 指路 --cmd 全路径与升级
    RemoteCmd,
    /// 远端起来了但输出不是 schema v1 快照
    BadSnapshot,
    /// 超过单节点拉取时限
    Timeout,
    /// 其余(本地 ssh 缺失等)
    Other,
}

/// 从 ssh stderr / 退出码归类失败原因(纯函数;stderr 可能是远端控制的,
/// 只做子串匹配,不回显原文——回显由 fail_text 统一 sanitize+截断)
pub fn classify(stderr: &str, exit: Option<i32>) -> FailKind {
    let s = stderr;
    if s.contains("Permission denied") {
        return FailKind::NeedKey;
    }
    if s.contains("Host key verification failed") {
        return FailKind::HostKey;
    }
    if s.contains("UNPROTECTED PRIVATE KEY FILE") {
        return FailKind::KeyPerms;
    }
    if s.contains("Could not resolve hostname") {
        return FailKind::Resolve;
    }
    if s.contains("Connection refused")
        || s.contains("Connection timed out")
        || s.contains("No route to host")
        || s.contains("Connection reset by peer")
    {
        return FailKind::Unreachable;
    }
    // 节点 ltop 过旧:clap 对未知旗标的使用错误;或远端找不到命令
    if s.contains("unexpected argument")
        || s.contains("Unknown argument")
        || s.contains("invalid option")
        || s.contains("command not found")
        || exit == Some(127)
    {
        return FailKind::RemoteCmd;
    }
    FailKind::Other
}

/// 分类 → 一句可操作的本地化提示。`detail` 是原始 stderr 首行;这里
/// 统一 sanitize(剥控制符)+ 截断,纵深防御不依赖调用方,让懂行的
/// 用户看得到根因又不给控制符留通道。
pub fn fail_text(zh: bool, kind: FailKind, target: &str, detail: &str) -> String {
    let detail = truncate(&crate::sanitize_text(detail), 80);
    let head = match kind {
        FailKind::NeedKey => crate::pick_s(zh, "no SSH key configured", "未配置 SSH 密钥"),
        FailKind::HostKey => {
            crate::pick_s(zh, "host fingerprint not in known_hosts", "主机指纹未入库")
        }
        FailKind::KeyPerms => crate::pick_s(
            zh,
            "local private key permissions too open",
            "本机私钥权限过宽",
        ),
        FailKind::Resolve => crate::pick_s(zh, "cannot resolve hostname", "主机名解析失败"),
        FailKind::Unreachable => crate::pick_s(zh, "network unreachable", "网络不可达"),
        FailKind::RemoteCmd => crate::pick_s(
            zh,
            "remote ltop missing/too old (need >= 0.2.3; try --cmd with full path)",
            "节点 ltop 缺失或过旧(需 >= 0.2.3;试试 --cmd 给全路径)",
        ),
        FailKind::BadSnapshot => {
            crate::pick_s(zh, "not a schema-v1 snapshot", "不是 schema v1 快照")
        }
        FailKind::Timeout => crate::pick_s(zh, "pull timed out", "拉取超时"),
        FailKind::Other => crate::pick_s(zh, "pull failed", "拉取失败"),
    };
    match kind {
        FailKind::NeedKey => format!(
            "{head} — {} `ltop fleet setup {target}` {}",
            crate::pick_s(zh, "run", "运行"),
            crate::pick_s(zh, "to install a key", "配置公钥"),
        ),
        FailKind::HostKey => format!(
            "{head} — {} `ssh {target}` {}",
            crate::pick_s(zh, "connect once manually with", "先手动跑一次"),
            crate::pick_s(zh, "and confirm the fingerprint", "确认指纹"),
        ),
        FailKind::KeyPerms => format!(
            "{head} — {}",
            crate::pick_s(zh, "chmod 600 the private key", "对私钥 chmod 600"),
        ),
        _ if detail.is_empty() => head,
        _ => format!("{head}: {detail}"),
    }
}

/// 失败样本(该节点本轮缺席)
pub fn failed(target: &str, error: String) -> NodeSample {
    NodeSample {
        target: target.to_string(),
        hostname: String::new(),
        rx: 0.0,
        tx: 0.0,
        procs: 0,
        conns: 0,
        top: None,
        latency_ms: None,
        error: Some(error),
    }
}

/// 从一台节点的 schema v1 快照聚合出样本(纯函数,双平台可测)
pub fn aggregate(target: &str, snap: &Snapshot, latency_ms: u64) -> NodeSample {
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
        latency_ms: Some(latency_ms),
        error: None,
    }
}

/// 语义化版本比较:"0.2.9" / "0.3.0-beta" / "0.2" 都可;
/// 解析不了的返回 false(setup doctor 用,不据此硬失败)
pub fn version_at_least(v: &str, major: u32, minor: u32, patch: u32) -> bool {
    let core = v.split(['-', '+']).next().unwrap_or("");
    let mut seg = [0u32; 3];
    for (i, p) in core.split('.').take(3).enumerate() {
        seg[i] = p.parse().unwrap_or(0);
    }
    (seg[0], seg[1], seg[2]) >= (major, minor, patch)
}

/// 定长截断(按字符,不动多字节中间)
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
pub(crate) mod fixture {
    //! 程序化构造 schema v1 快照(测试与将来的 UI 测试共用)
    use crate::snapshot::*;

    pub fn snap(name: &str, rx: f64, tx: f64) -> Snapshot {
        Snapshot {
            schema: SCHEMA,
            meta: Meta {
                hostname: name.to_string(),
                ts: 1,
                version: "0.2.13".into(),
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
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 聚合口径:lo 不计入,rx/tx 合计,最活跃进程取收发速率和最大者
    #[test]
    fn aggregates_rates_and_top_talker() {
        let mut s = fixture::snap("node-a", 100.0, 50.0);
        // 追加 lo 行:不得计入合计
        let lo: Snapshot = fixture::snap("x", 9999.0, 9999.0);
        let lo_iface = lo.net.interfaces.into_iter().map(|mut i| {
            i.name = "lo".into();
            i
        });
        s.net.interfaces.extend(lo_iface);
        let n = aggregate("a@h", &s, 12);
        assert_eq!(n.hostname, "node-a");
        assert_eq!(n.rx, 100.0, "lo 不得计入");
        assert_eq!(n.tx, 50.0);
        assert_eq!(n.procs, 1);
        assert_eq!(n.latency_ms, Some(12));
        assert_eq!(n.top.as_ref().map(|(n, _)| n.as_str()), Some("top"));
    }

    /// 两节点样本互不串扰(聚合是逐快照的)
    #[test]
    fn nodes_stay_independent() {
        let a = aggregate("a", &fixture::snap("node-a", 1.0, 1.0), 1);
        let b = aggregate("b", &fixture::snap("node-b", 500.0, 500.0), 2);
        assert_eq!(a.rx, 1.0);
        assert_eq!(b.rx, 500.0);
    }

    #[test]
    fn classifies_auth_and_network_failures() {
        assert_eq!(
            classify("user@air: Permission denied (publickey).", Some(255)),
            FailKind::NeedKey
        );
        assert_eq!(
            classify("Host key verification failed.", Some(255)),
            FailKind::HostKey
        );
        assert_eq!(
            classify(
                "@@@@@@ WARNING: UNPROTECTED PRIVATE KEY FILE! @@@@@@",
                Some(255)
            ),
            FailKind::KeyPerms
        );
        assert_eq!(
            classify(
                "ssh: Could not resolve hostname nas: Name or service not known",
                Some(255)
            ),
            FailKind::Resolve
        );
        assert_eq!(
            classify(
                "ssh: connect to host 10.0.0.9 port 22: Connection refused",
                Some(255)
            ),
            FailKind::Unreachable
        );
        assert_eq!(
            classify("bash: ltop: command not found", Some(127)),
            FailKind::RemoteCmd
        );
        assert_eq!(
            classify("error: unexpected argument '--json' found", Some(2)),
            FailKind::RemoteCmd
        );
        assert_eq!(classify("", Some(1)), FailKind::Other);
    }

    #[test]
    fn fail_text_is_actionable_not_raw() {
        let t = fail_text(
            true,
            FailKind::NeedKey,
            "air",
            "\x1b[31mevil\x1b[0m noise that is quite long and should be cut off",
        );
        assert!(t.contains("fleet setup air"), "指路 setup: {t}");
        assert!(!t.contains('\x1b'), "不得回显控制符");
        let en = fail_text(false, FailKind::HostKey, "nas", "");
        assert!(en.contains("ssh nas"));
        // 可带 detail 的类别:长文本截断且剥控制符
        let long = fail_text(
            true,
            FailKind::Other,
            "x",
            "\x1b[31mnoise that keeps going and going well past the eighty character truncation boundary for sure",
        );
        assert!(long.contains('…'), "长 detail 截断: {long}");
        assert!(!long.contains('\x1b'));
    }

    #[test]
    fn version_comparison() {
        assert!(version_at_least("0.2.9", 0, 2, 3));
        assert!(version_at_least("0.3.0-beta", 0, 2, 3));
        assert!(!version_at_least("0.2.2", 0, 2, 3));
        assert!(!version_at_least("0.1.9", 0, 2, 3));
        assert!(!version_at_least("unknown", 0, 2, 3));
        assert!(version_at_least("1.0", 0, 2, 3));
    }

    #[test]
    fn truncate_keeps_multibyte_intact() {
        let t = truncate("节点超时了请检查网络", 5);
        assert!(t.ends_with('…'));
        assert_eq!(t.chars().count(), 6);
        assert_eq!(truncate("short", 10), "short");
    }
}

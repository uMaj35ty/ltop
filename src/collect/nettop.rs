//! macOS per-process and per-flow network stats via Apple's `nettop` CLI.
//!
//! One dump (`nettop -L 1 -x -n`) carries both row kinds of the kernel's
//! NTstat tables, so a single spawn feeds the PROCS ledger and the CONNS
//! box; a second dump in `-m udp` mode yields the per-process UDP share:
//!   `<name>.<pid>,iface,state,bytes_in,bytes_out,...`   per-process Σ
//!   `tcp4 l:p<->r:p,iface,state,bytes_in,bytes_out,...` per-socket flows
//!
//! Column layout is shared by all row kinds: field 1 = label, 2 = interface,
//! 3 = state, 4 = bytes_in, 5 = bytes_out (later columns shift per protocol —
//! never parse past field 5).
//!
//! Counters are cumulative per socket and survive nettop restarts, but rows
//! exist only while the socket is open: a transfer that completes within one
//! sampling tick is never counted, and a process row vanishes once its last
//! socket closes (verified against a live socket that was closed while the
//! owner kept running). No eBPF on macOS — this is the documented semantic
//! difference from the Linux data path.
//!
//! Endpoint notation differs by family: IPv4 uses `ip:port`, IPv6 uses
//! `v6%iface.port` (dot before the port, `%` scope id), `*` = any.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::Instant;

use crate::collect::conn::{Conn, IPPROTO_TCP, IPPROTO_UDP, TCP_ESTABLISHED, TCP_LISTEN};
use crate::collect::udp::UdpDelta;

/// Linux 行里 UDP 用内核态 07(CLOSE),这里保持同口径
const UDP_STATE: u8 = 7;

/// 归属不了的行(他人进程的 socket)在 mac 上没有 uid 信息,
/// u32::MAX 让 user_name() 显示为 "n/a"
pub const NO_UID: u32 = u32::MAX;

/// parsed process row: `<name>.<pid>` with its live-socket byte Σ
#[derive(Debug, PartialEq)]
pub struct NetProcRow {
    pub pid: u32,
    pub name: String,
    pub rx: u64,
    pub tx: u64,
}

/// parsed flow row: one live socket
#[derive(Debug, PartialEq)]
pub struct NetFlowRow {
    pub proto: u8,
    pub local: (IpAddr, u16),
    pub remote: (IpAddr, u16),
    pub is_listen: bool,
    pub rx: u64,
    pub tx: u64,
}

/// 纯解析:一份 nettop CSV 输出 → 进程行 + 流行(双平台可测)
pub fn parse_dump(out: &str) -> (Vec<NetProcRow>, Vec<NetFlowRow>) {
    let mut procs = Vec::new();
    let mut flows = Vec::new();
    for line in out.lines() {
        if line.starts_with("time,") {
            continue; // 表头
        }
        let f: Vec<&str> = line.split(',').collect();
        if f.len() < 6 {
            continue;
        }
        let label = f[1];
        let rx = f[4].parse().unwrap_or(0);
        let tx = f[5].parse().unwrap_or(0);
        if let Some((lhs, rhs)) = label.split_once("<->") {
            // 流行:`tcp4 1.2.3.4:5<->6.7.8.9:10`
            let Some((kind, l)) = lhs.split_once(' ') else {
                continue;
            };
            let (proto, v6) = match kind {
                "tcp4" => (IPPROTO_TCP, false),
                "tcp6" => (IPPROTO_TCP, true),
                "udp4" => (IPPROTO_UDP, false),
                "udp6" => (IPPROTO_UDP, true),
                _ => continue,
            };
            let (Some(local), Some(remote)) = (parse_endpoint(l, v6), parse_endpoint(rhs, v6))
            else {
                continue;
            };
            // 只留 Linux 同口径的行:TCP 仅 Established/Listen(其余状态丢弃),
            // UDP(state 列为空)全留
            let state = f[3];
            if proto == IPPROTO_TCP
                && !state.is_empty()
                && state != "Established"
                && state != "Listen"
            {
                continue;
            }
            flows.push(NetFlowRow {
                proto,
                local,
                remote,
                is_listen: state == "Listen",
                rx,
                tx,
            });
        } else if let Some((name, pid)) = label.rsplit_once('.') {
            if let Ok(pid) = pid.parse() {
                procs.push(NetProcRow {
                    pid,
                    name: name.to_string(),
                    rx,
                    tx,
                });
            }
        }
    }
    (procs, flows)
}

/// `1.2.3.4:80`(v4 冒号)/ `fe80::1%en0.59069`(v6 点+scope)/ `*:22`
pub fn parse_endpoint(s: &str, v6: bool) -> Option<(IpAddr, u16)> {
    let (ip, port) = if v6 {
        s.rsplit_once('.')?
    } else {
        s.rsplit_once(':')?
    };
    let port: u16 = match port {
        "*" => 0,
        p => p.parse().ok()?,
    };
    let host = ip.split('%').next()?; // 去 scope id
    let ip = match host {
        "*" => {
            if v6 {
                IpAddr::V6(Ipv6Addr::UNSPECIFIED)
            } else {
                IpAddr::V4(Ipv4Addr::UNSPECIFIED)
            }
        }
        _ => host.parse().ok()?,
    };
    Some((ip, port))
}

/// 稳定行键,顶替 Linux 的 socket inode:下游(墓碑/选中锚/排序稳定性)
/// 只需要一个"同一 socket 跨 tick 不变"的数,不需要真 inode。
/// FNV-1a over (proto, local, remote)。
pub fn flow_inode(proto: u8, local: &(IpAddr, u16), remote: &(IpAddr, u16)) -> u64 {
    let mut key = vec![proto];
    for (ip, port) in [local, remote] {
        match ip {
            IpAddr::V4(v4) => key.extend_from_slice(&v4.octets()),
            IpAddr::V6(v6) => key.extend_from_slice(&v6.octets()),
        }
        key.extend_from_slice(&port.to_be_bytes());
    }
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in key {
        h ^= b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

/// 流行 → Conn 行(与 Linux netlink 行同构)。同键去重(nettop 会为
/// 未绑定地址打出 `udp4 *:*<->*:*` 这类全空行)。
pub fn flows_to_conns(flows: &[NetFlowRow]) -> Vec<Conn> {
    let mut out: Vec<Conn> = Vec::new();
    let mut seen: HashSet<u64> = HashSet::new();
    for f in flows {
        let inode = flow_inode(f.proto, &f.local, &f.remote);
        if !seen.insert(inode) {
            continue;
        }
        let state = if f.proto == IPPROTO_TCP {
            if f.is_listen {
                TCP_LISTEN
            } else {
                TCP_ESTABLISHED
            }
        } else {
            UDP_STATE
        };
        out.push(Conn {
            proto: f.proto,
            local: f.local,
            remote: f.remote,
            state,
            uid: NO_UID,
            inode,
            bytes_recv: f.rx,
            bytes_sent: f.tx,
            rate_recv: 0.0,
            rate_sent: 0.0,
            pid: None,
            proc: None,
            dead: false,
            first_seen: None,
            dead_at: None,
        });
    }
    out
}

/// mac 数据采集器:每 tick 两次 nettop 子进程(全量 + udp 模式),
/// 进程行差分成 per-pid 增量,流行转成 Conn 行暂存给 ConnCollector。
/// Linux 上不编译(纯解析函数除外,供双平台测试)。
#[cfg_attr(target_os = "linux", allow(dead_code))]
pub struct NettopCollector {
    prev_all: HashMap<u32, (u64, u64, Instant)>,
    prev_udp: HashMap<u32, (u64, u64, Instant)>,
    /// 本轮流行(App 取走喂给 ConnCollector::ingest)
    pub flows: Vec<Conn>,
    /// per-pid TCP+UDP 增量(PROCS net 账本)
    pub net: Vec<UdpDelta>,
    /// per-pid UDP 分项(PROCS udp 分解)
    pub udp: Vec<UdpDelta>,
    /// 本轮 nettop 子进程失败(spawn/非零退出):数据已清空,行数与
    /// 速率不可信;CONNS 盒降级徽标数据源
    pub stale: bool,
}

#[cfg_attr(target_os = "linux", allow(dead_code))]
impl NettopCollector {
    pub fn new() -> Self {
        Self {
            prev_all: HashMap::new(),
            prev_udp: HashMap::new(),
            flows: Vec::new(),
            net: Vec::new(),
            udp: Vec::new(),
            stale: false,
        }
    }

    #[cfg(target_os = "macos")]
    pub fn sample(&mut self) {
        let Ok((procs_all, flows)) = dump_mode(&[]) else {
            // 失败即清空,不留上一轮冻结的行(速率归零、行数照旧最误导);
            // stale 亮徽标
            self.flows.clear();
            self.net.clear();
            self.udp.clear();
            self.prev_all.clear();
            self.prev_udp.clear();
            self.stale = true;
            return;
        };
        self.stale = false;
        self.flows = flows_to_conns(&flows);
        self.net = diff_procs(&mut self.prev_all, procs_all, Instant::now());
        self.udp = match dump_mode(&["-m", "udp"]) {
            Ok((procs_udp, _)) => diff_procs(&mut self.prev_udp, procs_udp, Instant::now()),
            Err(_) => Vec::new(),
        };
    }
}

#[cfg_attr(target_os = "linux", allow(dead_code))]
impl Default for NettopCollector {
    fn default() -> Self {
        Self::new()
    }
}

/// 同模式上一轮差分:计数器为活 socket 累计值,行消失即清键,
/// 重现时 saturating_sub 归零重新起算(与 eBPF 快照差分同约定)
#[cfg_attr(target_os = "linux", allow(dead_code))]
fn diff_procs(
    prev: &mut HashMap<u32, (u64, u64, Instant)>,
    cur: Vec<NetProcRow>,
    now: Instant,
) -> Vec<UdpDelta> {
    let old = std::mem::take(prev);
    let mut out = Vec::new();
    let mut new: HashMap<u32, (u64, u64, Instant)> = HashMap::new();
    for r in cur {
        new.insert(r.pid, (r.rx, r.tx, now));
        if let Some(&(pr, pt, t0)) = old.get(&r.pid) {
            let dt = now.duration_since(t0).as_secs_f64().max(1e-6);
            out.push(UdpDelta {
                pid: r.pid,
                recv: r.rx.saturating_sub(pr),
                sent: r.tx.saturating_sub(pt),
                dt,
            });
        }
    }
    *prev = new;
    out
}

#[cfg(target_os = "macos")]
fn dump_mode(extra: &[&str]) -> std::io::Result<(Vec<NetProcRow>, Vec<NetFlowRow>)> {
    let out = std::process::Command::new("nettop")
        .args(["-L", "1", "-x", "-n"])
        .args(extra)
        .output()?;
    if !out.status.success() {
        return Err(std::io::Error::other("nettop failed"));
    }
    Ok(parse_dump(&String::from_utf8_lossy(&out.stdout)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collect::conn::TCP_LISTEN;
    use std::net::Ipv6Addr;

    /// Air 真机采样原样(有流量的 Established 行 + 监听行 + UDP 行 + 进程行)
    const SAMPLE: &str = "\
time,,interface,state,bytes_in,bytes_out,rx_dupe,rx_ooo,re-tx,rtt_avg,rcvsize,tx_win,tc_class,tc_mgt,cc_algo,P,C,R,W,arch,
23:09:55.103078,launchd.1,,,0,0,0,0,0,,,,,,,,,,,,
23:09:55.100019,tcp4 *:22<->*:*,,Listen,,,,,,,,,,-,cubic,-,-,-,-,so,
23:09:55.100000,tcp6 *.22<->*.*,,Listen,,,,,,,,,,-,cubic,-,-,-,-,so,
23:09:55.099965,udp4 *:68<->*:*,,,0,0,,,,,786896,,CTL,,,,,,,so,
23:09:55.103094,remoted.367,,,3978,2558,0,0,0,,,,,,,,,,,,
23:09:55.208376,tcp6 fe80::c48a:a1ff:feae:cd4%anri0.58783<->fe80::c48a:a1ff:feae:c2b%anri0.59067,anri0,Established,3978,2558,0,0,0,1.00 ms,131072,131072,BE,-,cubic,-,-,-,-,so,
23:09:55.207910,tcp4 192.168.121.111:50012<->17.57.145.134:5223,en0,Established,5093128,4927378,55222,89204,46306,117.62 ms,131072,249728,RD,-,cubic,-,-,-,-,ch,
23:09:55.194177,udp4 *:5353<->*:*,en0,,34390403,28503582,,,,,786896,,CTL,,,,,,,so,
23:09:55.207780,udp4 *:*<->*:*,,,,,,,,,,,,,,,,,,so,
23:09:55.101366,tcp6 fe80::c48a:a1ff:feae:c2b%anri0.59069<->*.*,anri0,Listen,,,,,,,,,,-,cubic,-,-,-,-,so,
23:09:55.207943,tcp4 1.2.3.4:5<->6.7.8.9:10,en0,SynSent,0,0,0,0,0,0.00 ms,131072,0,BE,-,cubic,-,1,-,-,so,
";

    #[test]
    fn parses_process_and_flow_rows_from_one_dump() {
        let (procs, flows) = parse_dump(SAMPLE);
        // launchd / remoted 是进程行;SynSent 行被丢弃(Linux 同口径只留 Established/Listen)
        assert!(procs.contains(&NetProcRow {
            pid: 367,
            name: "remoted".into(),
            rx: 3978,
            tx: 2558,
        }));
        assert!(procs.contains(&NetProcRow {
            pid: 1,
            name: "launchd".into(),
            rx: 0,
            tx: 0,
        }));
        assert_eq!(procs.len(), 2);

        // tcp4 Established:bytes 在 4/5 列
        let est = flows
            .iter()
            .find(|f| f.local == ("192.168.121.111".parse().unwrap(), 50012))
            .expect("tcp4 established row");
        assert_eq!(est.proto, IPPROTO_TCP);
        assert_eq!(est.remote, ("17.57.145.134".parse().unwrap(), 5223));
        assert_eq!(est.rx, 5_093_128);
        assert_eq!(est.tx, 4_927_378);
        assert!(!est.is_listen);

        // v6 流行:点分隔端口 + %scope
        let v6 = flows
            .iter()
            .find(|f| f.local.0.is_ipv6() && !f.is_listen)
            .expect("v6 flow");
        assert_eq!(
            v6.local,
            (
                IpAddr::V6("fe80::c48a:a1ff:feae:cd4".parse::<Ipv6Addr>().unwrap()),
                58783
            )
        );
        assert_eq!(v6.rx, 3978);

        // 监听行:tcp4 *:22(bytes 列为空 → 0)
        let listen = flows
            .iter()
            .find(|f| f.is_listen && f.local.0.is_ipv4())
            .expect("v4 listener");
        assert_eq!(listen.local, (IpAddr::V4(Ipv4Addr::UNSPECIFIED), 22));
        assert_eq!(listen.remote, (IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0));
        assert_eq!(listen.rx, 0);

        // v6 监听行 `*.22<->*.*` 的 `*.*` 端点解析为 (::, 0)
        let l6 = flows
            .iter()
            .find(|f| f.is_listen && f.local.0.is_ipv6())
            .expect("v6 listener");
        assert_eq!(l6.local, (IpAddr::V6(Ipv6Addr::UNSPECIFIED), 22));
        assert_eq!(l6.remote, (IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0));

        // UDP 行:state 为空,bytes 在 4/5 列
        let udp = flows
            .iter()
            .find(|f| f.proto == IPPROTO_UDP && f.rx > 0)
            .expect("udp flow");
        assert_eq!(udp.local, (IpAddr::V4(Ipv4Addr::UNSPECIFIED), 5353));
        assert_eq!(udp.rx, 34_390_403);
        assert_eq!(udp.tx, 28_503_582);
    }

    #[test]
    fn flows_to_conns_dedupes_and_synthesizes_stable_inodes() {
        let (_, flows) = parse_dump(SAMPLE);
        let conns = flows_to_conns(&flows);
        // 全空 `udp4 *:*<->*:*` 行与同键行去重
        let inodes: HashSet<u64> = conns.iter().map(|c| c.inode).collect();
        assert_eq!(inodes.len(), conns.len());
        // 键稳定:同元组不同次解析得到同一 inode
        let f = &conns[0];
        assert_eq!(flow_inode(f.proto, &f.local, &f.remote), f.inode);
        // 未归属行带 NO_UID,监听行状态位正确
        assert_eq!(conns.iter().map(|c| c.uid).min(), Some(NO_UID));
        assert!(conns.iter().any(|c| c.is_listen()));
        assert!(conns.iter().any(|c| !c.is_tcp() && c.state == UDP_STATE));
        let listen = conns.iter().find(|c| c.is_listen()).unwrap();
        assert_eq!(listen.state, TCP_LISTEN);
    }

    #[test]
    fn endpoint_notation_by_family() {
        assert_eq!(
            parse_endpoint("192.168.1.5:8080", false),
            Some(("192.168.1.5".parse().unwrap(), 8080))
        );
        assert_eq!(
            parse_endpoint("fe80::1%en0.59069", true),
            Some(("fe80::1".parse().unwrap(), 59069))
        );
        assert_eq!(
            parse_endpoint("*:22", false),
            Some((IpAddr::V4(Ipv4Addr::UNSPECIFIED), 22))
        );
        assert_eq!(
            parse_endpoint("*.*", true),
            Some((IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0))
        );
        assert_eq!(parse_endpoint("garbage", false), None);
    }

    #[test]
    fn diff_procs_baselines_then_deltas_then_rebaselines_on_reset() {
        let mut prev = HashMap::new();
        let t0 = Instant::now();
        // 首轮:基线,无增量
        let d = diff_procs(
            &mut prev,
            vec![NetProcRow {
                pid: 5,
                name: "a".into(),
                rx: 100,
                tx: 40,
            }],
            t0,
        );
        assert!(d.is_empty());
        // 次轮:差分
        let d = diff_procs(
            &mut prev,
            vec![NetProcRow {
                pid: 5,
                name: "a".into(),
                rx: 700,
                tx: 90,
            }],
            t0,
        );
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].recv, 600);
        assert_eq!(d[0].sent, 50);
        // 行消失后重现(计数器更小):saturating 归零,不产生负数
        let d = diff_procs(
            &mut prev,
            vec![NetProcRow {
                pid: 5,
                name: "a".into(),
                rx: 10,
                tx: 0,
            }],
            t0,
        );
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].recv, 0);
        assert_eq!(d[0].sent, 0);
    }

    /// 归属链真机验证(mac):自己进程的 listening socket 必须同时出现在
    /// nettop 流行与 socket_info 投影里,且 flow 键一致 —— 偏移/字节序/
    /// v4-in-16 字节布局任何一处错,这个测试都会当场断言失败
    /// 真机环境集成测试:nettop 快照在 CI runner 上不可控(枚举时机/缓存
    /// 差异),本地复验: cargo test attribution -- --ignored --nocapture
    #[test]
    #[cfg(target_os = "macos")]
    #[ignore = "requires live nettop (CI-flaky); run locally with --ignored"]
    fn attribution_joins_nettop_rows_and_socket_info() {
        use crate::collect::macproc;
        use std::net::TcpListener;

        let l = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let port = l.local_addr().unwrap().port();
        let (_, flows) = dump_mode(&[]).expect("nettop dump");
        let f = flows
            .iter()
            .find(|f| f.local == ("127.0.0.1".parse().unwrap(), port))
            .unwrap_or_else(|| panic!("listener :{port} missing from nettop flows"));
        let want = flow_inode(f.proto, &f.local, &f.remote);

        let map = macproc::socket_map();
        assert_eq!(
            map.get(&want).map(|(pid, _)| *pid),
            Some(std::process::id()),
            "own listener must be attributed via socket_info projection"
        );
    }

    /// 诊断:CONNS 归属全链路(仅 mac、人工排查用)
    #[test]
    #[cfg(target_os = "macos")]
    #[ignore = "diagnostic"]
    fn conns_attribution_full_junction_debug() {
        let mut nt = NettopCollector::new();
        nt.sample();
        let flows = std::mem::take(&mut nt.flows);
        eprintln!("nettop flows: {}", flows.len());
        let mut cc = crate::collect::conn::ConnCollector::new();
        cc.ingest(flows);
        cc.sample();
        let attributed = cc.conns.iter().filter(|c| c.pid.is_some()).count();
        eprintln!("conns: {} attributed: {}", cc.conns.len(), attributed);
        for c in cc.conns.iter().take(10) {
            eprintln!(
                "  {} pid={:?} proc={:?} uid={} ino={:016x}",
                crate::collect::conn::sock_str(&c.local),
                c.pid,
                c.proc,
                c.uid,
                c.inode
            );
        }
    }
}

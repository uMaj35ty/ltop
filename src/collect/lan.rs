use std::collections::{BTreeMap, HashSet};
use std::ffi::CStr;

#[cfg(target_os = "linux")]
use std::fs;
use std::net::{Ipv4Addr, UdpSocket};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use if_addrs::{get_if_addrs, IfAddr};

const KEEP_ALIVE: Duration = Duration::from_secs(600);
pub const DEFAULT_SWEEP_INTERVAL: Duration = Duration::from_secs(60);
const MAX_CONCURRENT_RESOLVES: usize = 16;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LanSort {
    Ip,
    Hostname,
    Mac,
    Iface,
}

impl LanSort {
    pub fn next(self) -> Self {
        match self {
            Self::Ip => Self::Hostname,
            Self::Hostname => Self::Mac,
            Self::Mac => Self::Iface,
            Self::Iface => Self::Ip,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Ip => "ip",
            Self::Hostname => "host",
            Self::Mac => "mac",
            Self::Iface => "iface",
        }
    }
}

#[derive(Clone)]
pub struct Neighbor {
    pub ip: Ipv4Addr,
    pub mac: Option<String>,
    /// mac:系统对非 Apple 签名二进制脱敏邻居 MAC——UI 显示 "—" 而非
    /// "expired"(Linux sweep 猜测语义)
    pub mac_hidden: bool,
    pub iface: String,
    pub complete: bool,
    pub hostname: Option<String>,
    pub is_gateway: bool,
    pub is_self: bool,
    pub last_seen: Instant,
}

pub struct LanCollector {
    pub neighbors: BTreeMap<Ipv4Addr, Neighbor>,
    /// 置顶的邻居(enter 切换):排序时置顶组在前
    pub tracked: HashSet<Ipv4Addr>,
    /// (iface, network address, prefix len), private IPv4 ranges only
    pub subnets: Vec<(String, Ipv4Addr, u8)>,
    pub own_ips: HashSet<Ipv4Addr>,
    pub gateways: HashSet<Ipv4Addr>,
    pub sort: LanSort,
    /// how often the UDP knock sweep runs (configurable)
    pub sweep_interval: Duration,
    pub sweep_enabled: bool,
    pub sweeping: bool,
    pub last_sweep: Option<Instant>,
    pub resolve_failures: HashSet<Ipv4Addr>,
    /// mac:arp 表枚举被系统隐私机制静默拦截(输出 0 行)——UI 提示用户
    /// 给终端授权「本地网络」;Linux 恒 false
    pub arp_blocked: bool,
    /// mac:RTM_GET 子网扫描(邻居表枚举被拦后的手工逐条替代)
    #[cfg(target_os = "macos")]
    rt_scan_done: Option<mpsc::Receiver<Vec<(Ipv4Addr, u16)>>>,
    #[cfg(target_os = "macos")]
    rt_scan_busy: bool,
    #[cfg(target_os = "macos")]
    rt_scan_last: Option<Instant>,
    resolving: HashSet<Ipv4Addr>,
    hostname_tx: Sender<(Ipv4Addr, Option<String>)>,
    hostname_rx: Receiver<(Ipv4Addr, Option<String>)>,
    sweep_done_rx: Option<Receiver<()>>,
}

/// Virtual bridges (docker, libvirt…) are NAT'd side networks, not the LAN
const NON_LAN_IFACE_PREFIXES: [&str; 4] = ["docker", "br-", "veth", "virbr"];

impl LanCollector {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            neighbors: BTreeMap::new(),
            tracked: std::collections::HashSet::new(),
            subnets: Vec::new(),
            own_ips: HashSet::new(),
            gateways: HashSet::new(),
            sort: LanSort::Ip,
            sweep_interval: DEFAULT_SWEEP_INTERVAL,
            sweep_enabled: true,
            sweeping: false,
            last_sweep: None,
            resolve_failures: HashSet::new(),
            arp_blocked: false,
            #[cfg(target_os = "macos")]
            rt_scan_done: None,
            #[cfg(target_os = "macos")]
            rt_scan_busy: false,
            #[cfg(target_os = "macos")]
            rt_scan_last: None,
            resolving: HashSet::new(),
            hostname_tx: tx,
            hostname_rx: rx,
            sweep_done_rx: None,
        }
    }

    pub fn sample(&mut self) {
        let now = Instant::now();
        self.refresh_subnets();
        self.read_gateways();
        self.read_arp(now);
        self.prune(now);
        self.spawn_resolves();
        self.drain_hostnames();
        self.maybe_sweep(now);
        #[cfg(target_os = "macos")]
        {
            self.maybe_rt_scan(now);
            self.drain_rt_scan(now);
        }
    }

    /// mac:邻居表枚举被隐私机制拦死(sysctl 空、RTM 单条 MAC 脱敏),但
    /// 单条查询的「条目是否存在 + 归属接口」是真的——后台逐条探测子网,
    /// 重建无 MAC 的邻居列表(IP/主机名/流量联动可用)
    #[cfg(target_os = "macos")]
    fn maybe_rt_scan(&mut self, now: Instant) {
        // 只作 ARP 被隐私机制拦死时的替代:ARP 正常时无条件空扫整个子网
        // 白白发 RTM_GET,还会把 read_arp 拿到的真实 MAC 周期性清成隐藏
        if !self.arp_blocked || self.rt_scan_busy || self.subnets.is_empty() {
            return;
        }
        if self
            .rt_scan_last
            .is_some_and(|t| now.duration_since(t) < Duration::from_secs(30))
        {
            return;
        }
        let Some((_, net, prefix)) = self.pick_subnet() else {
            return;
        };
        let (tx, rx) = mpsc::channel();
        self.rt_scan_busy = true;
        self.rt_scan_last = Some(now);
        self.rt_scan_done = Some(rx);
        thread::spawn(move || {
            let pid = std::process::id();
            let mut seq = 1u32;
            let mut found = Vec::new();
            for ip in host_addrs(net, prefix) {
                seq = seq.wrapping_add(1);
                if let Some(e) = crate::collect::lan_rtget::probe(ip, seq, pid) {
                    found.push((ip, e.if_index));
                }
            }
            let _ = tx.send(found);
        });
    }

    #[cfg(target_os = "macos")]
    fn drain_rt_scan(&mut self, now: Instant) {
        let Some(rx) = &self.rt_scan_done else {
            return;
        };
        let Ok(found) = rx.try_recv() else {
            return;
        };
        self.rt_scan_busy = false;
        self.rt_scan_done = None;
        for (ip, if_index) in found {
            let Some(iface) = if_index_to_name(if_index) else {
                continue;
            };
            if iface == "lo0" || iface.is_empty() {
                continue;
            }
            let is_gateway = self.gateways.contains(&ip);
            let is_self = self.own_ips.contains(&ip);
            match self.neighbors.get_mut(&ip) {
                Some(n) => {
                    n.iface = iface;
                    if n.mac.is_none() {
                        // 已有真实 ARP MAC 的邻居不清成隐藏:RTM 探测只补
                        // 缺 MAC 的行,否则 MAC 列随扫描周期闪烁
                        n.mac_hidden = true;
                    }
                    n.complete = true;
                    n.is_gateway = is_gateway;
                    n.is_self = is_self;
                    n.last_seen = now;
                }
                None => {
                    self.neighbors.insert(
                        ip,
                        Neighbor {
                            ip,
                            mac: None,
                            mac_hidden: true,
                            iface,
                            complete: true,
                            hostname: None,
                            is_gateway,
                            is_self,
                            last_seen: now,
                        },
                    );
                }
            }
        }
    }

    fn refresh_subnets(&mut self) {
        self.subnets.clear();
        self.own_ips.clear();
        let Ok(list) = get_if_addrs() else {
            return;
        };
        for ifa in list {
            let IfAddr::V4(v4) = ifa.addr else {
                continue;
            };
            self.own_ips.insert(v4.ip);
            if NON_LAN_IFACE_PREFIXES
                .iter()
                .any(|p| ifa.name.starts_with(p))
            {
                continue;
            }
            let mask = u32::from(v4.netmask);
            if !v4.ip.is_private() && !v4.ip.is_link_local() {
                continue;
            }
            let net = Ipv4Addr::from(u32::from(v4.ip) & mask);
            let prefix = mask.count_ones() as u8;
            if !self
                .subnets
                .iter()
                .any(|(_, n, p)| *n == net && *p == prefix)
            {
                self.subnets.push((ifa.name, net, prefix));
            }
        }
        self.subnets.sort();
    }

    fn read_gateways(&mut self) {
        self.gateways.clear();
        #[cfg(target_os = "linux")]
        {
            let Ok(content) = fs::read_to_string("/proc/net/route") else {
                return;
            };
            for line in content.lines().skip(1) {
                let cols: Vec<&str> = line.split_whitespace().collect();
                if cols.len() >= 4 && cols[1] == "00000000" {
                    if let Ok(raw) = u32::from_str_radix(cols[2], 16) {
                        self.gateways.insert(Ipv4Addr::from(u32::from_be(raw)));
                    }
                }
            }
        }
        // mac:默认路由经 route 子进程(与 net.rs default_route_iface 同源)
        #[cfg(target_os = "macos")]
        {
            if let Some(ip) = parse_route_gateway(&route_default_output()) {
                self.gateways.insert(ip);
            }
        }
    }

    fn read_arp(&mut self, now: Instant) {
        #[cfg(target_os = "linux")]
        let entries = {
            let mut entries = Vec::new();
            if let Ok(content) = fs::read_to_string("/proc/net/arp") {
                for line in content.lines().skip(1) {
                    let cols: Vec<&str> = line.split_whitespace().collect();
                    if cols.len() < 6 {
                        continue;
                    }
                    let Ok(ip) = cols[0].parse::<Ipv4Addr>() else {
                        continue;
                    };
                    let flags =
                        u16::from_str_radix(cols[2].trim_start_matches("0x"), 16).unwrap_or(0);
                    let complete = flags & 0x2 != 0;
                    let mac = cols[3].to_lowercase();
                    let mac = if complete && mac != "00:00:00:00:00:00" {
                        Some(mac)
                    } else {
                        None
                    };
                    entries.push((ip, mac, cols[5].to_string()));
                }
            }
            entries
        };
        #[cfg(target_os = "macos")]
        let entries: Vec<(Ipv4Addr, Option<String>, String)> = {
            let raw = arp_an_output();
            // 正常输出至少含 incomplete 条目;0 行 = 被隐私机制静默拦截
            self.arp_blocked = raw.lines().next().is_none();
            parse_arp_an(&raw)
        };
        for (ip, mac, iface) in entries {
            let complete = mac.is_some();
            if iface == "lo" {
                continue;
            }
            let is_gateway = self.gateways.contains(&ip);
            let is_self = self.own_ips.contains(&ip);
            match self.neighbors.get_mut(&ip) {
                Some(n) => {
                    n.iface = iface;
                    n.mac = mac;
                    // 真实 ARP 数据到达(如签名解锁后)时,清除 RTM 扫描
                    // 遗留的脱敏标记
                    n.mac_hidden = false;
                    n.complete = complete;
                    n.is_gateway = is_gateway;
                    n.is_self = is_self;
                    n.last_seen = now;
                }
                // sweep misses linger as incomplete entries; only real answers
                // (ATF_COM set) may create a neighbour
                None if complete => {
                    self.neighbors.insert(
                        ip,
                        Neighbor {
                            ip,
                            mac,
                            mac_hidden: false,
                            iface,
                            complete,
                            hostname: None,
                            is_gateway,
                            is_self,
                            last_seen: now,
                        },
                    );
                }
                None => {}
            }
        }
    }

    fn prune(&mut self, now: Instant) {
        self.neighbors
            .retain(|_, n| now.duration_since(n.last_seen) < KEEP_ALIVE);
    }

    fn spawn_resolves(&mut self) {
        let candidates: Vec<Ipv4Addr> = self
            .neighbors
            .values()
            .filter(|n| {
                n.complete
                    && n.hostname.is_none()
                    && !self.resolving.contains(&n.ip)
                    && !self.resolve_failures.contains(&n.ip)
            })
            .map(|n| n.ip)
            .collect();
        for ip in candidates {
            if self.resolving.len() >= MAX_CONCURRENT_RESOLVES {
                break;
            }
            self.resolving.insert(ip);
            let tx = self.hostname_tx.clone();
            thread::spawn(move || {
                let _ = tx.send((ip, resolve_any(ip)));
            });
        }
    }

    fn drain_hostnames(&mut self) {
        loop {
            match self.hostname_rx.try_recv() {
                Ok((ip, Some(host))) => {
                    if let Some(n) = self.neighbors.get_mut(&ip) {
                        n.hostname = Some(host);
                    }
                    self.resolving.remove(&ip);
                }
                Ok((ip, None)) => {
                    self.resolving.remove(&ip);
                    self.resolve_failures.insert(ip);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }
    }

    fn maybe_sweep(&mut self, now: Instant) {
        if let Some(rx) = &self.sweep_done_rx {
            if rx.try_recv().is_ok() {
                self.sweeping = false;
                self.sweep_done_rx = None;
                self.last_sweep = Some(now);
                // new neighbours may now be resolvable
                self.resolve_failures.clear();
            }
        }
        if !self.sweep_enabled || self.sweeping || self.subnets.is_empty() {
            return;
        }
        let due = self
            .last_sweep
            .is_none_or(|t| now.duration_since(t) >= self.sweep_interval);
        if due {
            self.start_sweep();
        }
    }

    pub fn sweep_now(&mut self) {
        if !self.sweeping {
            self.last_sweep = None;
        }
    }

    fn start_sweep(&mut self) {
        let Some((_, net, prefix)) = self.pick_subnet() else {
            return;
        };
        let (done_tx, done_rx) = mpsc::channel();
        self.sweeping = true;
        self.sweep_done_rx = Some(done_rx);
        thread::spawn(move || {
            for ip in host_addrs(net, prefix) {
                // An empty UDP datagram is enough: to route it the kernel must
                // resolve the destination's MAC, which fills the neighbour table.
                if let Ok(sock) = UdpSocket::bind("0.0.0.0:0") {
                    if sock.connect((ip, 9)).is_ok() {
                        let _ = sock.send(&[]);
                    }
                }
            }
            let _ = done_tx.send(());
        });
    }

    fn pick_subnet(&self) -> Option<(String, Ipv4Addr, u8)> {
        self.subnets
            .iter()
            .find(|(_, net, prefix)| {
                // 掩码位运算判网关归属,不枚举地址:大网段下 host_range
                // 全段 .any() 是千万级迭代
                let mask = if *prefix == 0 {
                    0
                } else {
                    u32::MAX << (32 - *prefix as u32)
                };
                self.gateways
                    .iter()
                    .any(|g| u32::from(*g) & mask == u32::from(*net))
            })
            .or_else(|| self.subnets.first())
            .cloned()
    }

    pub fn sorted_neighbors(&self) -> Vec<&Neighbor> {
        let mut list: Vec<&Neighbor> = self.neighbors.values().collect();
        match self.sort {
            LanSort::Ip => list.sort_by_key(|n| u32::from(n.ip)),
            LanSort::Hostname => list.sort_by(|a, b| {
                a.hostname
                    .as_deref()
                    .unwrap_or("")
                    .cmp(b.hostname.as_deref().unwrap_or(""))
                    .then(u32::from(a.ip).cmp(&u32::from(b.ip)))
            }),
            LanSort::Mac => list.sort_by(|a, b| {
                a.mac
                    .as_deref()
                    .unwrap_or("")
                    .cmp(b.mac.as_deref().unwrap_or(""))
                    .then(u32::from(a.ip).cmp(&u32::from(b.ip)))
            }),
            LanSort::Iface => list.sort_by(|a, b| {
                a.iface
                    .cmp(&b.iface)
                    .then(u32::from(a.ip).cmp(&u32::from(b.ip)))
            }),
        }
        // 置顶组提前(组内保持既有顺序)
        if !self.tracked.is_empty() {
            list.sort_by_key(|n| !self.tracked.contains(&n.ip));
        }
        list
    }
}

impl Default for LanCollector {
    fn default() -> Self {
        Self::new()
    }
}

fn host_addrs(net: Ipv4Addr, prefix: u8) -> Vec<Ipv4Addr> {
    // 大网段(VPN 的 /8-/21)按 /22 粒度截断:全段枚举是千万级地址的 Vec
    // + 每个扫描周期近死循环;网关与常见设备几乎总在本段头部 /22
    host_range(net, prefix.max(22)).collect()
}

#[cfg(target_os = "macos")]
fn route_default_output() -> String {
    std::process::Command::new("route")
        .args(["-n", "get", "default"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// `   gateway: 192.168.121.1` 行 → 网关地址
#[cfg_attr(target_os = "linux", allow(dead_code))]
fn parse_route_gateway(out: &str) -> Option<Ipv4Addr> {
    for line in out.lines() {
        if let Some(i) = line.find("gateway:") {
            return line[i + "gateway:".len()..].trim().parse().ok();
        }
    }
    None
}

#[cfg(target_os = "macos")]
fn arp_an_output() -> String {
    std::process::Command::new("arp")
        .arg("-an")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// `? (192.168.121.1) at c0:4a:9:30:d3:af on en0 ifscope [ethernet]`
/// `? (192.168.121.255) at (incomplete) on en0 ...` → (ip, mac, iface)。
/// mac 的 MAC 段不补零,这里规范成两位(Linux /proc/net/arp 口径);
/// 组播/广播表项是协议噪声,跳过。
/// mac:接口索引 → 接口名(libc::if_indextoname)
#[cfg(target_os = "macos")]
fn if_index_to_name(idx: u16) -> Option<String> {
    let mut buf = [0 as libc::c_char; libc::IF_NAMESIZE];
    // SAFETY: buf 为全有权的 IF_NAMESIZE 缓冲
    let p = unsafe { libc::if_indextoname(idx as libc::c_uint, buf.as_mut_ptr()) };
    if p.is_null() {
        return None;
    }
    // SAFETY: 成功时 p 指向 buf 内的 NUL 结尾字符串
    let s = unsafe { std::ffi::CStr::from_ptr(p) };
    Some(s.to_string_lossy().into_owned())
}

#[cfg_attr(target_os = "linux", allow(dead_code))]
fn parse_arp_an(out: &str) -> Vec<(Ipv4Addr, Option<String>, String)> {
    let mut out_v = Vec::new();
    for line in out.lines() {
        let Some(a) = line.find('(') else { continue };
        let Some(b) = line[a + 1..].find(')') else {
            continue;
        };
        let Ok(ip) = line[a + 1..a + 1 + b].parse::<Ipv4Addr>() else {
            continue;
        };
        // 广播条目按全 f MAC 判:按 IP 末字节==255 判断会在 /23+/8 下
        // 误杀合法主机(.255 可是真实地址)
        if ip.is_multicast() || ip.is_broadcast() {
            continue;
        }
        let rest = &line[a + 1 + b..];
        let Some(at) = rest.find(" at ") else {
            continue;
        };
        let tail = &rest[at + 4..];
        let (mac_raw, complete) = if let Some(end) = tail.find(' ') {
            let tok = &tail[..end];
            if tok == "(incomplete)" {
                (None, false)
            } else {
                (Some(tok), true)
            }
        } else {
            (Some(tail.trim()), true)
        };
        // 子网广播条目按全 f MAC 判:按 IP 末字节==255 判断会在 /23+/8
        // 下误杀合法主机(.255 可以是真实地址)
        if mac_raw.is_some_and(|m| m.eq_ignore_ascii_case("ff:ff:ff:ff:ff:ff")) {
            continue;
        }
        let iface = tail
            .split(" on ")
            .nth(1)
            .and_then(|s| s.split_whitespace().next())
            .unwrap_or("")
            .to_string();
        let mac = mac_raw.and_then(normalize_mac).filter(|_| complete);
        out_v.push((ip, mac, iface));
    }
    out_v
}

/// mac arp 输出的段不补零(c0:4a:9:30:d3:af)→ 统一两位小写
#[cfg_attr(target_os = "linux", allow(dead_code))]
fn normalize_mac(s: &str) -> Option<String> {
    let mut parts = Vec::new();
    for p in s.split(':') {
        parts.push(format!("{:02x}", u8::from_str_radix(p, 16).ok()?));
    }
    (parts.len() == 6).then(|| parts.join(":"))
}

fn host_range(net: Ipv4Addr, prefix: u8) -> impl Iterator<Item = Ipv4Addr> {
    let base = u32::from(net);
    let host_bits = 32 - prefix.min(32) as u32;
    let start = base + 1;
    let end = base | ((1u32 << host_bits) - 1);
    (start..end).map(Ipv4Addr::from)
}

/// mDNS first (the device's real `.local` name), then NetBIOS node status
/// (Windows/Samba/router boxes), then classic reverse DNS.
fn resolve_any(ip: Ipv4Addr) -> Option<String> {
    resolve_mdns(ip)
        .or_else(|| resolve_nbns(ip))
        .or_else(|| resolve_hostname(ip))
}

const MDNS_GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);
const MDNS_PORT: u16 = 5353;

/// 每查询随机化的事务 ID:恒定 ID(mDNS 0x6c74 / NBNS 0x2a2a)可被同链路
/// 抢答者无竞速预测。时间纳秒 ^ pid ^ 原子计数器做混淆即可——防的是
/// "预测后抢先应答",不是密码学 PRNG 场景
fn query_id() -> u16 {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let c = COUNTER.fetch_add(0x9E37_79B9, Ordering::Relaxed);
    let x = nanos
        .wrapping_mul(0x9E37_79B1)
        ^ c
        ^ (c >> 16)
        ^ std::process::id();
    (((x >> 8) ^ x) & 0xFFFF) as u16
}

/// 应答头两字节 == 我们的事务 ID(前 2 字节在 len ≥ 2 时才读)
fn reply_id_matches(buf: &[u8], id: u16) -> bool {
    buf.len() >= 2 && [buf[0], buf[1]] == id.to_be_bytes()
}

/// Ask the LAN's mDNS responders for the PTR of `ip`
/// (`d.c.b.a.in-addr.arpa`); this is how `avahi-resolve -a` works.
/// Sent from an ephemeral port, so it is a *legacy* mDNS query and
/// responders reply unicast — no root, no conflict with the local
/// resolver, no multicast traffic beyond one query packet.
fn resolve_mdns(ip: Ipv4Addr) -> Option<String> {
    let sock = UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.set_read_timeout(Some(Duration::from_millis(600)))
        .ok()?;
    let id = query_id();
    let query = build_ptr_query(ip, id);
    let mut buf = [0u8; 1500];
    for _ in 0..2 {
        sock.send_to(&query, (MDNS_GROUP, MDNS_PORT)).ok()?;
        let until = Instant::now() + Duration::from_millis(600);
        while Instant::now() < until {
            match sock.recv_from(&mut buf) {
                // 欺骗防护三件套:应答必须来自被查设备(legacy 单播 mDNS
                // 的应答者即被查者,同 NBNS 路径)+ 事务 ID 匹配 + PTR 可解析
                Ok((n, src)) if src.ip() == ip && reply_id_matches(&buf[..n], id) => {
                    if let Some(name) = parse_ptr_response(&buf[..n]) {
                        return Some(name);
                    }
                }
                Ok((_, _)) => continue,
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    continue
                }
                Err(_) => return None,
            }
        }
    }
    None
}

/// one-question DNS query: PTR of `<rev(ip)>.in-addr.arpa`
fn build_ptr_query(ip: Ipv4Addr, id: u16) -> Vec<u8> {
    let [a, b, c, d] = ip.octets();
    let mut m = Vec::with_capacity(17 + 4 + 6);
    m.extend_from_slice(&id.to_be_bytes());
    m.extend_from_slice(&[0x00, 0x00]); // flags: standard query
    m.extend_from_slice(&[0x00, 0x01]); // qdcount = 1
    m.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
    for label in [
        d.to_string(),
        c.to_string(),
        b.to_string(),
        a.to_string(),
        "in-addr".into(),
        "arpa".into(),
    ] {
        m.push(label.len() as u8);
        m.extend_from_slice(label.as_bytes());
    }
    m.push(0);
    m.extend_from_slice(&12u16.to_be_bytes()); // QTYPE = PTR
    m.extend_from_slice(&1u16.to_be_bytes()); // QCLASS = IN
    m
}

/// first `.local` PTR record in a DNS response, without the suffix
fn parse_ptr_response(buf: &[u8]) -> Option<String> {
    if buf.len() < 12 {
        return None;
    }
    let flags = u16::from_be_bytes([buf[2], buf[3]]);
    if flags & 0x8000 == 0 {
        return None; // not a response
    }
    let qd = u16::from_be_bytes([buf[4], buf[5]]) as usize;
    let an = u16::from_be_bytes([buf[6], buf[7]]) as usize;
    let mut i = 12usize;
    for _ in 0..qd {
        read_name(buf, &mut i)?;
        i += 4;
    }
    for _ in 0..an {
        read_name(buf, &mut i)?;
        if i + 10 > buf.len() {
            return None;
        }
        let rtype = u16::from_be_bytes([buf[i], buf[i + 1]]);
        let rdlen = u16::from_be_bytes([buf[i + 8], buf[i + 9]]) as usize;
        if rtype == 12 && rdlen > 0 && i + 10 + rdlen <= buf.len() {
            let mut j = i + 10;
            if let Some(name) = read_name(buf, &mut j) {
                let lower = name.to_lowercase();
                if let Some(base) = lower.strip_suffix(".local") {
                    if !base.is_empty() {
                        return Some(base.to_string());
                    }
                }
            }
        }
        i += 10 + rdlen;
    }
    None
}

/// decode a (possibly compression-pointed) domain name; `i` ends just past
/// the name at the original position
fn read_name(buf: &[u8], i: &mut usize) -> Option<String> {
    let mut labels: Vec<String> = Vec::new();
    let mut pos = *i;
    let mut end = *i;
    let mut jumps = 0;
    loop {
        let &len = buf.get(pos)?;
        if len & 0xC0 == 0xC0 {
            let &lo = buf.get(pos + 1)?;
            if end == *i {
                end = pos + 2;
            }
            pos = (((len & 0x3F) as usize) << 8) | lo as usize;
            jumps += 1;
            if jumps > 16 {
                return None;
            }
        } else if len == 0 {
            if end == *i {
                end = pos + 1;
            }
            break;
        } else {
            let l = len as usize;
            let s = pos + 1;
            if s + l > buf.len() {
                return None;
            }
            labels.push(String::from_utf8_lossy(&buf[s..s + l]).into_owned());
            pos = s + l;
        }
    }
    *i = end;
    if labels.is_empty() {
        None
    } else {
        Some(labels.join("."))
    }
}

/// Ask the device itself for its NetBIOS name (NBNS node-status request to
/// UDP 137 — what `nmblookup -A` does). Windows, Samba and many home-routers
/// answer; phones and most Linux boxes do not.
fn resolve_nbns(ip: Ipv4Addr) -> Option<String> {
    let sock = UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.set_read_timeout(Some(Duration::from_millis(400)))
        .ok()?;
    let id = query_id();
    let query = build_nbns_query(id);
    let mut buf = [0u8; 1500];
    for _ in 0..2 {
        sock.send_to(&query, (ip, 137)).ok()?;
        let until = Instant::now() + Duration::from_millis(400);
        while Instant::now() < until {
            match sock.recv_from(&mut buf) {
                Ok((n, src))
                    if src.ip() == ip && reply_id_matches(&buf[..n], id) =>
                {
                    if let Some(name) = parse_nbns_response(&buf[..n]) {
                        let name = name.trim_end_matches('\0').trim().to_lowercase();
                        // a reverse-encoded address or an empty string is not a name
                        if !name.is_empty() && name.parse::<Ipv4Addr>().is_err() {
                            return Some(name);
                        }
                    }
                }
                Ok(_) => continue, // not from the queried device
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    continue
                }
                Err(_) => return None,
            }
        }
    }
    None
}

/// NBNS node-status request for `*` ("who are you"), type NBSTAT
fn build_nbns_query(id: u16) -> Vec<u8> {
    let mut m = Vec::with_capacity(50);
    m.extend_from_slice(&id.to_be_bytes());
    m.extend_from_slice(&[0x00, 0x00]); // flags: standard query
    m.extend_from_slice(&[0x00, 0x01]); // qdcount = 1
    m.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
    m.push(0x20); // the encoded name is one 32-char label
    m.extend_from_slice(b"CKAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"); // "*"
    m.push(0x00);
    m.extend_from_slice(&0x21u16.to_be_bytes()); // NBSTAT
    m.extend_from_slice(&1u16.to_be_bytes()); // IN
    m
}

/// first NetBIOS name of a node-status response
fn parse_nbns_response(buf: &[u8]) -> Option<String> {
    if buf.len() < 12 {
        return None;
    }
    let flags = u16::from_be_bytes([buf[2], buf[3]]);
    if flags & 0x8000 == 0 {
        return None; // not a response
    }
    let mut i = 12usize;
    read_name(buf, &mut i)?; // RR name (encoded `*` label or a pointer)
                             // type(2) class(2) ttl(4) rdlen(2)
    if i + 10 > buf.len() {
        return None;
    }
    let rdlen = u16::from_be_bytes([buf[i + 8], buf[i + 9]]) as usize;
    i += 10;
    // rdata: num_names (1) then per name: 15 name bytes + 1 flags + 4 ip
    if rdlen < 19 || i + rdlen > buf.len() || buf[i] == 0 {
        return None;
    }
    let rec = &buf[i + 1..i + 17];
    let name = String::from_utf8_lossy(&rec[..15])
        .trim_matches(|c: char| c == ' ' || c == '\0')
        .to_string();
    if name.is_empty() || name.parse::<Ipv4Addr>().is_ok() {
        None
    } else {
        Some(name)
    }
}

/// Reverse DNS, best effort. Runs on a worker thread; may take seconds.
fn resolve_hostname(ip: Ipv4Addr) -> Option<String> {
    let mut addr: libc::sockaddr_in = unsafe { std::mem::zeroed() };
    addr.sin_family = libc::AF_INET as libc::sa_family_t;
    addr.sin_addr.s_addr = u32::from(ip).to_be();
    let mut host = [0 as libc::c_char; 256];
    let mut serv = [0 as libc::c_char; 64];
    let rc = unsafe {
        libc::getnameinfo(
            &addr as *const libc::sockaddr_in as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
            host.as_mut_ptr(),
            host.len() as libc::socklen_t,
            serv.as_mut_ptr(),
            serv.len() as libc::socklen_t,
            libc::NI_NAMEREQD,
        )
    };
    if rc != 0 {
        return None;
    }
    let name = unsafe { CStr::from_ptr(host.as_ptr()) }
        .to_string_lossy()
        .trim_end_matches('.')
        .trim_end_matches(".lan")
        .to_lowercase();
    // systemd-resolved's NSS answers reverse lookups of unnamed hosts with
    // the literal address — that is not a name
    if name.is_empty() || name.parse::<Ipv4Addr>().is_ok() {
        None
    } else {
        Some(name)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn tracked_neighbors_sort_first() {
        let mut c = LanCollector::new();
        for k in 1..=3u32 {
            let ip = format!("10.0.0.{k}").parse().unwrap();
            c.neighbors.insert(
                ip,
                Neighbor {
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
        c.tracked.insert("10.0.0.3".parse().unwrap());
        let ips: Vec<_> = c
            .sorted_neighbors()
            .iter()
            .map(|n| n.ip.to_string())
            .collect();
        assert_eq!(ips, vec!["10.0.0.3", "10.0.0.1", "10.0.0.2"]);
    }

    use super::*;

    #[test]
    fn host_range_excludes_network_and_broadcast() {
        let hosts: Vec<Ipv4Addr> = host_range("192.168.1.0".parse().unwrap(), 24).collect();
        assert_eq!(hosts.len(), 254);
        assert_eq!(hosts[0], "192.168.1.1".parse::<Ipv4Addr>().unwrap());
        assert_eq!(hosts[253], "192.168.1.254".parse::<Ipv4Addr>().unwrap());
    }

    #[test]
    fn host_addrs_clamped_on_huge_subnets() {
        // /8 VPN 网段按 /22 截断:不再构造千万级地址的 Vec、扫描不再
        // 近死循环(修复前 = 1670 万地址)
        assert_eq!(host_addrs("10.0.0.0".parse().unwrap(), 8).len(), 1022);
        // 正常 /24 行为不变
        assert_eq!(host_addrs("192.168.1.0".parse().unwrap(), 24).len(), 254);
    }

    #[test]
    fn ptr_query_well_formed() {
        let q = build_ptr_query("192.168.1.7".parse().unwrap(), 0x1234);
        assert_eq!(&q[0..2], &[0x12, 0x34]); // id
        assert_eq!(&q[2..4], &[0, 0]); // query
        assert_eq!(&q[4..6], &[0, 1]); // one question
        let tail = &q[12..];
        let expected: &[&[u8]] = &[b"7", b"1", b"168", b"192", b"in-addr", b"arpa"];
        let mut pos = 0;
        for lbl in expected {
            assert_eq!(tail[pos], lbl.len() as u8);
            assert_eq!(&tail[pos + 1..pos + 1 + lbl.len()], *lbl);
            pos += 1 + lbl.len();
        }
        assert_eq!(tail[pos], 0);
        assert_eq!(&tail[pos + 1..pos + 3], &[0, 12]); // PTR
        assert_eq!(&tail[pos + 3..pos + 5], &[0, 1]); // IN
    }

    fn name_bytes(name: &str) -> Vec<u8> {
        let mut out = Vec::new();
        for lbl in name.split('.') {
            out.push(lbl.len() as u8);
            out.extend_from_slice(lbl.as_bytes());
        }
        out.push(0);
        out
    }

    #[test]
    fn parses_ptr_answer_with_compression() {
        let mut m = Vec::new();
        m.extend_from_slice(&[0x6c, 0x74]); // id
        m.extend_from_slice(&[0x84, 0x00]); // response
        m.extend_from_slice(&[0, 1, 0, 1, 0, 0, 0, 0]); // qd 1, an 1
                                                        // question: 7.1.168.192.in-addr.arpa PTR IN
        m.extend_from_slice(&name_bytes("7.1.168.192.in-addr.arpa"));
        m.extend_from_slice(&[0, 12, 0, 1]);
        // answer: name = pointer to question (0xC00C), PTR "MyPhone.local"
        m.extend_from_slice(&[0xC0, 0x0C]);
        m.extend_from_slice(&[0, 12, 0, 1, 0, 0, 0, 120]); // PTR IN TTL 120
        let rd = name_bytes("MyPhone.local");
        m.extend_from_slice(&(rd.len() as u16).to_be_bytes());
        m.extend_from_slice(&rd);
        assert_eq!(parse_ptr_response(&m).as_deref(), Some("myphone"));
    }

    #[test]
    fn skips_non_ptr_and_non_local_answers() {
        // answers: [A record, PTR "gw.example.com", PTR "MyPhone.local"]
        // → the .local PTR wins
        let answers = |rd: &[u8], rtype: u16| -> Vec<u8> {
            let mut a = Vec::new();
            a.extend_from_slice(&[0xC0, 0x0C]); // name = pointer to question
            a.extend_from_slice(&rtype.to_be_bytes());
            a.extend_from_slice(&[0, 1, 0, 0, 0, 60]); // class IN, TTL
            a.extend_from_slice(&(rd.len() as u16).to_be_bytes());
            a.extend_from_slice(rd);
            a
        };
        let mut m = Vec::new();
        m.extend_from_slice(&[0, 1, 0x84, 0, 0, 1, 0, 3, 0, 0, 0, 0]); // qd 1, an 3
        m.extend_from_slice(&name_bytes("7.1.168.192.in-addr.arpa"));
        m.extend_from_slice(&[0, 12, 0, 1]);
        m.extend(&answers(&[192, 168, 1, 7], 1));
        m.extend(&answers(&name_bytes("gw.example.com"), 12));
        m.extend(&answers(&name_bytes("MyPhone.local"), 12));
        assert_eq!(parse_ptr_response(&m).as_deref(), Some("myphone"));

        // no .local answer at all → None
        let mut m2 = Vec::new();
        m2.extend_from_slice(&[0, 1, 0x84, 0, 0, 1, 0, 1, 0, 0, 0, 0]); // qd 1, an 1
        m2.extend_from_slice(&name_bytes("7.1.168.192.in-addr.arpa"));
        m2.extend_from_slice(&[0, 12, 0, 1]);
        m2.extend(&answers(&name_bytes("gw.example.com"), 12));
        assert_eq!(parse_ptr_response(&m2), None);
    }

    #[test]
    fn rejects_queries_and_garbage() {
        assert_eq!(parse_ptr_response(&[]), None);
        assert_eq!(parse_ptr_response(&[0u8; 11]), None);
        let mut q = build_ptr_query("192.168.1.7".parse().unwrap(), 1);
        q[2] = 0x00; // ensure QR=0
        assert_eq!(parse_ptr_response(&q), None);
    }

    #[test]
    fn read_name_handles_plain_and_pointer() {
        let mut m = Vec::new();
        m.extend_from_slice(&name_bytes("a.local"));
        let ptr_at = m.len();
        m.extend_from_slice(&[0xC0, 0x00]); // pointer to offset 0
        let mut i = ptr_at;
        assert_eq!(read_name(&m, &mut i).as_deref(), Some("a.local"));
        assert_eq!(i, ptr_at + 2); // consumed only the pointer
    }

    /// 事务 ID 随机化:恒定 ID(mDNS 0x6c74 / NBNS 0x2a2a 的旧写法)可被
    /// 同链路攻击者无竞速预测后抢先伪造应答
    #[test]
    fn query_ids_are_randomized() {
        let a = query_id();
        let b = query_id();
        let c = query_id();
        assert!(!(a == b && b == c), "连续三次 ID 不应全同: {a} {b} {c}");
    }

    #[test]
    fn nbns_query_well_formed() {
        let q = build_nbns_query(0x4242);
        assert_eq!(&q[0..2], &[0x42, 0x42]); // id
        assert_eq!(&q[2..4], &[0, 0]); // query
        assert_eq!(&q[4..6], &[0, 1]); // one question
        assert_eq!(q[12], 0x20); // 32-char label
        assert_eq!(&q[13..15], b"CK"); // encoded '*'
        assert_eq!(&q[13 + 30..13 + 32], b"AA");
        assert_eq!(q[45], 0); // label terminator
        assert_eq!(&q[46..48], &[0, 0x21]); // NBSTAT
        assert_eq!(&q[48..50], &[0, 1]); // IN
    }

    #[test]
    fn parses_nbns_node_status_response() {
        let mut m = Vec::new();
        m.extend_from_slice(&[0x2a, 0x2a]); // id
        m.extend_from_slice(&[0x84, 0x00]); // response
        m.extend_from_slice(&[0, 0, 0, 1, 0, 0, 0, 0]); // qd 0, an 1
        m.push(0x20); // encoded name label, echoed back
        m.extend_from_slice(b"CKAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
        m.push(0);
        m.extend_from_slice(&[0, 0x21, 0, 1, 0, 0, 0, 0]); // NBSTAT IN TTL 0
        let names = 1u8;
        let mut rec: Vec<u8> = b"SMBSHARE".to_vec();
        rec.resize(15, b' '); // space-padded name
        rec.push(0x04); // name flags: registered, unique
        rec.extend_from_slice(&[192, 168, 1, 1]);
        m.extend_from_slice(&((1 + rec.len()) as u16).to_be_bytes()); // rdlen
        m.push(names);
        m.extend_from_slice(&rec);
        assert_eq!(parse_nbns_response(&m).as_deref(), Some("SMBSHARE"));
    }

    #[test]
    fn rejects_nbns_queries_and_empty_names() {
        assert_eq!(parse_nbns_response(&[]), None);
        assert_eq!(parse_nbns_response(&[0u8; 20]), None);
        let mut q = build_nbns_query(1);
        q[2] = 0x00; // QR=0: a query, not a response
        assert_eq!(parse_nbns_response(&q), None);
        // zero names in the status record
        let mut m = Vec::new();
        m.extend_from_slice(&[0, 1, 0x84, 0, 0, 0, 0, 1, 0, 0, 0, 0]);
        m.push(0x20);
        m.extend_from_slice(b"CKAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
        m.push(0);
        m.extend_from_slice(&[0, 0x21, 0, 1, 0, 0, 0, 0]);
        m.extend_from_slice(&19u16.to_be_bytes());
        m.push(0); // num_names = 0
        m.extend_from_slice(&[0u8; 18]);
        assert_eq!(parse_nbns_response(&m), None);
    }

    #[test]
    fn parses_route_gateway_line() {
        let out = "   route to: default\n destination: default\n       mask: 0.0.0.0\n    gateway: 192.168.121.1\n  interface: en0\n";
        assert_eq!(
            parse_route_gateway(out),
            Some("192.168.121.1".parse().unwrap())
        );
        assert_eq!(parse_route_gateway("no gateway here"), None);
        assert_eq!(parse_route_gateway("gateway: not-an-ip"), None);
    }

    #[test]
    fn parses_mac_arp_table_like_bsd_output() {
        // Air 真机原样输出:MAC 段不补零;含 incomplete、广播与组播行
        let out = "\
? (192.168.121.1) at c0:4a:9:30:d3:af on en0 ifscope [ethernet]
? (192.168.121.111) at c0:c7:db:f4:1e:8d on en0 ifscope permanent [ethernet]
? (192.168.121.148) at a2:b4:34:4f:e4:6b on en0 ifscope [ethernet]
? (192.168.121.200) at (incomplete) on en0 ifscope [ethernet]
? (192.168.121.255) at ff:ff:ff:ff:ff:ff on en0 ifscope [ethernet]
? (224.0.0.251) at 1:0:5e:0:0:fb on en0 ifscope permanent [ethernet]
";
        let rows = parse_arp_an(out);
        let gw = rows
            .iter()
            .find(|(ip, ..)| *ip == "192.168.121.1".parse::<Ipv4Addr>().unwrap())
            .unwrap();
        // MAC 补零规范成 Linux 口径
        assert_eq!(gw.1.as_deref(), Some("c0:4a:09:30:d3:af"));
        assert_eq!(gw.2, "en0");
        assert!(rows.iter().any(|(ip, mac, _)| *ip
            == "192.168.121.200".parse::<Ipv4Addr>().unwrap()
            && mac.is_none()));
        // 广播/组播行被过滤
        assert!(rows
            .iter()
            .all(|(ip, ..)| !ip.is_multicast() && ip.octets()[3] != 255));
        assert_eq!(rows.len(), 4);
    }

    /// 诊断:RTM 扫描真机跑 40 秒看邻居(仅 mac、人工排查用)
    #[test]
    #[cfg(target_os = "macos")]
    #[ignore = "diagnostic"]
    fn lan_rt_scan_live_debug() {
        let gw: std::net::Ipv4Addr = "192.168.121.1".parse().unwrap();
        eprintln!(
            "single probe gw: {:?}",
            crate::collect::lan_rtget::probe(gw, 1, std::process::id())
        );
        let mut c = LanCollector::new();
        c.sweep_enabled = false;
        let t0 = std::time::Instant::now();
        while t0.elapsed() < std::time::Duration::from_secs(40) {
            c.sample();
            thread::sleep(std::time::Duration::from_millis(500));
        }
        eprintln!("neighbors {} 个", c.neighbors.len());
        for n in c.sorted_neighbors() {
            eprintln!(
                "  {} if={} gw={} self={} hidden={} host={:?}",
                n.ip, n.iface, n.is_gateway, n.is_self, n.mac_hidden, n.hostname
            );
        }
    }

    #[test]
    fn rejects_malformed_arp_lines() {
        assert!(parse_arp_an("garbage").is_empty());
        assert!(parse_arp_an("? (not-an-ip) at ab:cd:ef:01:02:03 on en0").is_empty());
        // MAC 段不是十六进制 → 视为不完整
        let rows = parse_arp_an("? (10.0.0.9) at zzz on en0");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].1, None);
    }
}

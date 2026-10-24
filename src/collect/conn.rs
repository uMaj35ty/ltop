//! Per-connection network stats via netlink INET_DIAG, no root required.
//!
//! `ss -tim` uses the same channel: NETLINK_SOCK_DIAG dumps every socket
//! system-wide (uid included), and INET_DIAG_INFO carries struct tcp_info
//! whose tcpi_bytes_received / tcpi_bytes_acked are cumulative per-socket
//! byte counters — diffed per tick they become per-connection B/s.
//! TCP only: the kernel keeps no per-socket byte counters for UDP.
//! PID attribution scans /proc/<pid>/fd (own uid only).

use std::collections::{HashMap, HashSet};
use std::fs;
use std::net::{IpAddr, Ipv4Addr};
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
use libc::AF_NETLINK;
#[cfg(target_os = "linux")]
use libc::{AF_INET, AF_INET6};

#[cfg(target_os = "linux")]
const NETLINK_SOCK_DIAG: libc::c_int = 4;
#[cfg(target_os = "linux")]
const SOCK_DIAG_BY_FAMILY: u16 = 20;
#[cfg(target_os = "linux")]
const NLMSG_ERROR: u16 = 2;
#[cfg(target_os = "linux")]
const NLMSG_DONE: u16 = 3;
#[cfg(target_os = "linux")]
const NLM_F_REQUEST: u16 = 0x01;
#[cfg(target_os = "linux")]
const NLM_F_DUMP: u16 = 0x300;
#[cfg(target_os = "linux")]
const INET_DIAG_INFO: u16 = 2;
#[cfg(target_os = "linux")]
const INET_DIAG_NOCOOKIE: u32 = 0xffff_ffff;

/// struct tcp_info offsets (UAPI, append-only) of the u64 byte counters
#[cfg(target_os = "linux")]
const TI_BYTES_ACKED: usize = 120;
#[cfg(target_os = "linux")]
const TI_BYTES_RECEIVED: usize = 128;
#[cfg(target_os = "linux")]
const TI_MIN_LEN: usize = 136;

pub const IPPROTO_TCP: u8 = 6;
pub const IPPROTO_UDP: u8 = 17;
pub const TCP_ESTABLISHED: u8 = 1;
pub const TCP_LISTEN: u8 = 10;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConnFilter {
    All,
    Lan,
    Tcp,
    Udp,
}

impl ConnFilter {
    pub fn next(self) -> Self {
        match self {
            Self::All => Self::Lan,
            Self::Lan => Self::Tcp,
            Self::Tcp => Self::Udp,
            Self::Udp => Self::All,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Lan => "lan",
            Self::Tcp => "tcp",
            Self::Udp => "udp",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ConnSort {
    Name,
    Rate,
    Sum,
    /// 按进程分组:同名进程的连接相邻,组按合计流量降序
    Proc,
}

impl ConnSort {
    pub fn next(self) -> Self {
        match self {
            Self::Proc => Self::Sum,
            Self::Sum => Self::Name,
            Self::Name => Self::Rate,
            Self::Rate => Self::Proc,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Rate => "rate",
            Self::Sum => "sum",
            Self::Proc => "proc",
        }
    }
}

/// 进程分组的归属键:有归属用进程名,否则 uid 标签。
/// 键是名字本身——不同 pid 的同名进程(如多 worker)归入同组。
pub(crate) fn owner_key(c: &Conn) -> String {
    c.proc.clone().unwrap_or_else(|| format!("uid:{}", c.uid))
}

/// 同名进程的连接相邻;组按会话合计流量(收+发)降序,组内按合计降序、
/// 再按 inode 稳定排序。
pub(crate) fn sort_by_proc_group(conns: &mut [Conn]) {
    let mut totals: HashMap<String, u64> = HashMap::new();
    for c in conns.iter() {
        *totals.entry(owner_key(c)).or_insert(0) += c.bytes_recv + c.bytes_sent;
    }
    conns.sort_by(|a, b| {
        let (ka, kb) = (owner_key(a), owner_key(b));
        let (ta, tb) = (totals[&ka], totals[&kb]);
        tb.cmp(&ta)
            .then_with(|| ka.cmp(&kb))
            .then_with(|| (b.bytes_recv + b.bytes_sent).cmp(&(a.bytes_recv + a.bytes_sent)))
            .then_with(|| a.inode.cmp(&b.inode))
    });
}

#[derive(Clone)]
pub struct Conn {
    pub proto: u8,
    pub local: (IpAddr, u16),
    pub remote: (IpAddr, u16),
    pub state: u8,
    pub uid: u32,
    pub inode: u64,
    pub bytes_recv: u64,
    pub bytes_sent: u64,
    pub rate_recv: f64,
    pub rate_sent: f64,
    pub pid: Option<u32>,
    pub proc: Option<String>,
    /// 墓碑行:连接已关闭,数值为最后已知状态,展示 TTL 到期后移除
    pub dead: bool,
    /// ltop 首次看到该 socket 的时刻(存活时长的下限)
    pub first_seen: Option<Instant>,
    /// 墓碑的死亡时刻;活连接为 None
    pub dead_at: Option<Instant>,
}

impl Conn {
    pub fn is_tcp(&self) -> bool {
        self.proto == IPPROTO_TCP
    }

    pub fn is_listen(&self) -> bool {
        self.is_tcp() && self.state == TCP_LISTEN
    }

    pub fn total_rate(&self) -> f64 {
        self.rate_recv + self.rate_sent
    }
}

pub struct ConnCollector {
    prev: HashMap<u64, (u64, u64, Instant)>,
    #[cfg(target_os = "linux")]
    pid_map: HashMap<u64, (u32, String)>,
    /// UDP session totals per socket inode, fed by the eBPF per-socket
    /// deltas (the kernel keeps no UDP byte counters of its own)
    udp_totals: HashMap<u64, (u64, u64)>,
    /// 上一轮还活着、这轮从 netlink 消失的连接:灰显保留 tombstone_ttl
    tombstones: Vec<(Conn, Instant)>,
    /// socket 首见时刻(存活时长下限)
    born: HashMap<u64, Instant>,
    last_live: HashMap<u64, Conn>,
    /// 墓碑保留时长
    pub tombstone_ttl: Duration,
    pub conns: Vec<Conn>,
    /// linux:本轮 netlink dump 的失败原因(socket/send/recv/kernel -errno),
    /// None = 正常;mac 恒 None。CONNS 盒降级徽标的数据源
    pub diag_error: Option<String>,
    pub users: HashMap<u32, String>,
    pub filter: ConnFilter,
    pub sort: ConnSort,
    /// LAN 联动:只看该对端(enter from lan row);f 循环或 esc 清除
    pub peer_filter: Option<Ipv4Addr>,
    /// session totals per remote IPv4: (recv bytes, sent bytes)
    remote_totals: HashMap<Ipv4Addr, (u64, u64)>,
    ticks: u64,
    /// mac:本轮 nettop 流行,App 在 sample 前经 ingest 存入
    #[cfg(target_os = "macos")]
    inbox: Vec<Conn>,
}

impl Default for ConnCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl ConnCollector {
    pub fn new() -> Self {
        Self {
            prev: HashMap::new(),
            #[cfg(target_os = "linux")]
            pid_map: HashMap::new(),
            udp_totals: HashMap::new(),
            born: HashMap::new(),
            tombstones: Vec::new(),
            last_live: HashMap::new(),
            tombstone_ttl: Duration::from_secs(5),
            conns: Vec::new(),
            diag_error: None,
            users: HashMap::new(),
            filter: ConnFilter::All,
            sort: ConnSort::Proc,
            peer_filter: None,
            remote_totals: HashMap::new(),
            ticks: 0,
            #[cfg(target_os = "macos")]
            inbox: Vec::new(),
        }
    }

    /// mac:接收本轮 nettop 流行(nettop.rs 解析产物;Linux 无此路径)
    #[cfg(target_os = "macos")]
    pub fn ingest(&mut self, flows: Vec<Conn>) {
        self.inbox = flows;
    }

    pub fn sample(&mut self) {
        let now = Instant::now();
        let mut conns = {
            #[cfg(target_os = "linux")]
            {
                let mut c = Vec::new();
                let mut err: Option<String> = None;
                for &(family, proto) in &[
                    (AF_INET, IPPROTO_TCP),
                    (AF_INET6, IPPROTO_TCP),
                    (AF_INET, IPPROTO_UDP),
                    (AF_INET6, IPPROTO_UDP),
                ] {
                    if let Err(e) = dump_family(family, proto, &mut c) {
                        err.get_or_insert(e);
                    }
                }
                self.diag_error = err;
                c
            }
            #[cfg(target_os = "macos")]
            {
                self.diag_error = None;
                std::mem::take(&mut self.inbox)
            }
        };
        conns.retain(|c| {
            if c.is_tcp() {
                c.state == TCP_ESTABLISHED || c.state == TCP_LISTEN
            } else {
                true
            }
        });

        let old_prev = std::mem::take(&mut self.prev);
        for c in &mut conns {
            if let Some(&(pr, ps, t0)) = old_prev.get(&c.inode) {
                let dt = now.duration_since(t0).as_secs_f64().max(1e-6);
                let dr = c.bytes_recv.saturating_sub(pr);
                let ds = c.bytes_sent.saturating_sub(ps);
                c.rate_recv = dr as f64 / dt;
                c.rate_sent = ds as f64 / dt;
                // session totals per remote (TCP only; UDP has no counters)
                if c.is_tcp() {
                    if let IpAddr::V4(ip) = c.remote.0 {
                        let e = self.remote_totals.entry(ip).or_insert((0, 0));
                        e.0 += dr;
                        e.1 += ds;
                    }
                }
            }
        }
        self.prev = conns
            .iter()
            .filter(|c| c.is_tcp())
            .map(|c| (c.inode, (c.bytes_recv, c.bytes_sent, now)))
            .collect();
        if self.remote_totals.len() > 8192 {
            self.remote_totals.clear();
        }

        #[cfg(target_os = "linux")]
        {
            self.pid_map = scan_pids();
            for c in &mut conns {
                if let Some((pid, name)) = self.pid_map.get(&c.inode) {
                    c.pid = Some(*pid);
                    c.proc = Some(name.clone());
                }
            }
        }
        // mac:libproc 枚举本 uid 的 socket,按 flow 键归属(与 Linux 只能
        // 读自己 uid 的 /proc/<pid>/fd 同语义);他人进程的行无 uid 信息
        #[cfg(target_os = "macos")]
        {
            let map = crate::collect::macproc::socket_map();
            let own = unsafe { libc::getuid() };
            for c in &mut conns {
                if let Some(&(pid, ref name)) = map.get(&c.inode) {
                    c.pid = Some(pid);
                    c.proc = Some(name.clone());
                    c.uid = own;
                }
            }
        }

        match self.sort {
            ConnSort::Name => conns.sort_by(|a, b| {
                a.proc
                    .as_deref()
                    .unwrap_or("")
                    .cmp(b.proc.as_deref().unwrap_or(""))
                    .then_with(|| sock_str(&a.local).cmp(&sock_str(&b.local)))
            }),
            ConnSort::Sum => conns.sort_by(|a, b| {
                (a.bytes_recv + a.bytes_sent)
                    .cmp(&(b.bytes_recv + b.bytes_sent))
                    .reverse()
            }),
            ConnSort::Rate => conns.sort_by(|a, b| {
                b.total_rate()
                    .partial_cmp(&a.total_rate())
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.is_listen().cmp(&b.is_listen()))
            }),
            ConnSort::Proc => sort_by_proc_group(&mut conns),
        }
        for c in &mut conns {
            c.first_seen = Some(*self.born.entry(c.inode).or_insert(now));
        }
        if self.born.len() > 16384 {
            let live: HashSet<u64> = conns.iter().map(|c| c.inode).collect();
            self.born.retain(|ino, _| live.contains(ino));
        }
        self.update_tombstones(&mut conns, now);
        self.conns = conns;

        self.ticks += 1;
        if self.ticks % 60 == 1 {
            self.users = load_users();
        }
    }

    /// 墓碑簿记:这轮消失且有流量的连接转为灰显墓碑,保留 TTL;复活的
    /// (inode 重现)与到期的移除。墓碑追加在活行之后,不参与排序。
    fn update_tombstones(&mut self, live: &mut Vec<Conn>, now: Instant) {
        if self.tombstone_ttl.is_zero() {
            self.tombstones.clear();
            return;
        }
        let live_inodes: HashSet<u64> = live.iter().map(|c| c.inode).collect();
        self.tombstones.retain(|(c, born)| {
            c.dead
                && now.duration_since(*born) < self.tombstone_ttl
                && !live_inodes.contains(&c.inode)
        });
        let prev = std::mem::take(&mut self.last_live);
        for (ino, c) in prev {
            if !live_inodes.contains(&ino) && c.bytes_recv + c.bytes_sent > 0 {
                let mut t = c;
                t.dead = true;
                t.dead_at = Some(now);
                self.tombstones.push((t, now));
            }
        }
        self.last_live = live.iter().map(|c| (c.inode, c.clone())).collect();
        let mut tombs: Vec<Conn> = self.tombstones.iter().map(|(c, _)| c.clone()).collect();
        live.append(&mut tombs);
    }

    pub fn user_name(&self, uid: u32) -> String {
        // mac 他人进程的连接没有 uid 信息(nettop 不提供),显示 n/a
        #[cfg(target_os = "macos")]
        if uid == crate::collect::nettop::NO_UID {
            return "n/a".to_string();
        }
        match self.users.get(&uid) {
            Some(n) => n.clone(),
            None => format!("uid:{uid}"),
        }
    }

    /// fold eBPF per-socket UDP deltas into the freshly rebuilt rows: session
    /// Σ lands in bytes_recv/sent, interval rates in rate_recv/sent (both are
    /// kernel-zero for UDP). Called once per tick right after `sample`. Rows
    /// are rebuilt from netlink every tick, so the Σ must be re-injected on
    /// EVERY tick — also when this round had no deltas (idle socket) — or the
    /// display would flicker back to 0.
    pub fn apply_udp(&mut self, deltas: &[crate::collect::udp::InodeDelta]) {
        let mut rates: HashMap<u64, (f64, f64)> = HashMap::new();
        let mut round_bytes: HashMap<u64, (u64, u64)> = HashMap::new();
        for d in deltas {
            let e = self.udp_totals.entry(d.inode).or_insert((0, 0));
            e.0 += d.recv;
            e.1 += d.sent;
            let r = rates.entry(d.inode).or_insert((0.0, 0.0));
            r.0 += d.recv as f64 / d.dt;
            r.1 += d.sent as f64 / d.dt;
            round_bytes.insert(d.inode, (d.recv, d.sent));
        }
        if self.udp_totals.len() > 8192 {
            self.udp_totals.clear();
        }
        for c in &mut self.conns {
            if c.is_tcp() {
                continue;
            }
            let Some(&(ur, us)) = self.udp_totals.get(&c.inode) else {
                continue;
            };
            c.bytes_recv = ur;
            c.bytes_sent = us;
            if let Some(&(rr, rs)) = rates.get(&c.inode) {
                c.rate_recv += rr;
                c.rate_sent += rs;
            }
            // UDP joins the per-remote session totals (TCP-only before eBPF),
            // so the LAN box Σ covers both transports; only this round's
            // delta goes in — udp_totals is cumulative and would snowball
            if !c.is_listen() {
                if let (IpAddr::V4(ip), Some(&(dr, ds))) = (c.remote.0, round_bytes.get(&c.inode)) {
                    let e = self.remote_totals.entry(ip).or_insert((0, 0));
                    e.0 += dr;
                    e.1 += ds;
                }
            }
        }
    }

    pub fn reset_totals(&mut self) {
        self.remote_totals.clear();
        self.udp_totals.clear();
    }

    /// Aggregate per remote IPv4: (conn count, recv B/s, sent B/s, session
    /// recv bytes, session sent bytes). Used to cross-link LAN neighbours.
    pub fn per_remote_v4(&self) -> HashMap<Ipv4Addr, (usize, f64, f64, u64, u64)> {
        let mut out: HashMap<Ipv4Addr, (usize, f64, f64, u64, u64)> = HashMap::new();
        for c in &self.conns {
            if c.is_listen() {
                continue;
            }
            if let IpAddr::V4(ip) = c.remote.0 {
                let e = out.entry(ip).or_insert((0, 0.0, 0.0, 0, 0));
                e.0 += 1;
                e.1 += c.rate_recv;
                e.2 += c.rate_sent;
                if let Some(&(tr, ts)) = self.remote_totals.get(&ip) {
                    e.3 = tr;
                    e.4 = ts;
                }
            }
        }
        out
    }
}

/// ip:port display helper shared with the UI
pub fn sock_str(s: &(IpAddr, u16)) -> String {
    format!("{}:{}", s.0, s.1)
}

#[cfg(target_os = "linux")]
fn dump_family(family: libc::c_int, proto: u8, out: &mut Vec<Conn>) -> Result<(), String> {
    // SAFETY: raw netlink socket, all buffers sized by the protocol's fixed
    // message layout; fd is closed on every return path below.
    unsafe {
        let fd = libc::socket(
            AF_NETLINK,
            libc::SOCK_RAW | libc::SOCK_CLOEXEC,
            NETLINK_SOCK_DIAG,
        );
        if fd < 0 {
            return Err("socket".into());
        }
        let req = build_req(family as u8, proto);
        let mut sa: libc::sockaddr_nl = std::mem::zeroed();
        sa.nl_family = AF_NETLINK as libc::sa_family_t;
        let rc = libc::sendto(
            fd,
            req.as_ptr() as *const libc::c_void,
            req.len(),
            0,
            &sa as *const libc::sockaddr_nl as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t,
        );
        if rc < 0 {
            libc::close(fd);
            return Err("send".into());
        }
        let mut buf = [0u8; 65536];
        loop {
            let len = libc::recv(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len(), 0);
            if len <= 0 {
                libc::close(fd);
                return Err("recv".into());
            }
            let len = len as usize;
            let mut off = 0usize;
            let mut done = false;
            let mut err: Option<String> = None;
            while off + 16 <= len {
                let mlen = u32from(&buf[off..off + 4]) as usize;
                let mtype = u16from(&buf[off + 4..off + 6]);
                if mlen < 16 || off + mlen > len {
                    done = true;
                    break;
                }
                match mtype {
                    NLMSG_DONE => {
                        done = true;
                        break;
                    }
                    // 内核拒绝 dump(缺 INET_DIAG/UDP_DIAG 编译、SELinux 策略)
                    // 原样吞掉 = CONNS 静默清空,与"没有连接"不可区分:
                    // 带出错误码给降级徽标
                    NLMSG_ERROR => {
                        done = true;
                        err = Some(if mlen >= 20 {
                            format!(
                                "kernel -{}",
                                i32::from_ne_bytes([
                                    buf[off + 16],
                                    buf[off + 17],
                                    buf[off + 18],
                                    buf[off + 19],
                                ])
                            )
                        } else {
                            "kernel".into()
                        });
                        break;
                    }
                    SOCK_DIAG_BY_FAMILY => {
                        if let Some(c) = parse_diag(&buf[off + 16..off + mlen], proto) {
                            out.push(c);
                        }
                    }
                    _ => {}
                }
                off += (mlen + 3) & !3;
            }
            if done {
                libc::close(fd);
                return match err {
                    Some(e) => Err(e),
                    None => Ok(()),
                };
            }
        }
    }
}

/// nlmsghdr + inet_diag_req_v2 (fixed 72-byte message)
#[cfg(target_os = "linux")]
fn build_req(family: u8, proto: u8) -> Vec<u8> {
    const TOTAL: usize = 16 + 56;
    let mut b = vec![0u8; TOTAL];
    b[0..4].copy_from_slice(&(TOTAL as u32).to_ne_bytes());
    b[4..6].copy_from_slice(&SOCK_DIAG_BY_FAMILY.to_ne_bytes());
    b[6..8].copy_from_slice(&(NLM_F_REQUEST | NLM_F_DUMP).to_ne_bytes());
    b[8..12].copy_from_slice(&1u32.to_ne_bytes());
    // inet_diag_req_v2
    b[16] = family;
    b[17] = proto;
    b[18] = 1 << (INET_DIAG_INFO - 1); // idiag_ext: ask for tcp_info
    b[20..24].copy_from_slice(&0xffff_ffffu32.to_ne_bytes()); // all states
                                                              // inet_diag_sockid cookie = NOCOOKIE (dump request)
    b[16 + 48..16 + 52].copy_from_slice(&INET_DIAG_NOCOOKIE.to_ne_bytes());
    b[16 + 52..16 + 56].copy_from_slice(&INET_DIAG_NOCOOKIE.to_ne_bytes());
    b
}

/// inet_diag_msg fixed part = 4 status bytes + sockid(48) + 5 u32 (expires,
/// rqueue, wqueue, uid, inode) = 72, followed by rtattr stream.
#[cfg(target_os = "linux")]
fn parse_diag(p: &[u8], proto: u8) -> Option<Conn> {
    if p.len() < 72 {
        return None;
    }
    let family = p[0];
    let state = p[1];
    let sport = u16::from_be_bytes([p[4], p[5]]);
    let dport = u16::from_be_bytes([p[6], p[7]]);
    let src = normalize(ip_from(&p[8..24], family));
    let dst = normalize(ip_from(&p[24..40], family));
    let uid = u32from(&p[64..68]);
    let inode = u32from(&p[68..72]) as u64;

    let mut bytes_recv = 0u64;
    let mut bytes_sent = 0u64;
    let mut off = 72;
    while off + 4 <= p.len() {
        let rta_len = u16from(&p[off..off + 2]) as usize;
        let rta_type = u16from(&p[off + 2..off + 4]);
        if rta_len < 4 || off + rta_len > p.len() {
            break;
        }
        if rta_type == INET_DIAG_INFO {
            let ti = &p[off + 4..off + rta_len];
            if ti.len() >= TI_MIN_LEN {
                bytes_sent = u64from(&ti[TI_BYTES_ACKED..TI_BYTES_ACKED + 8]);
                bytes_recv = u64from(&ti[TI_BYTES_RECEIVED..TI_BYTES_RECEIVED + 8]);
            }
        }
        off += (rta_len + 3) & !3;
    }

    Some(Conn {
        proto,
        local: (src, sport),
        remote: (dst, dport),
        state,
        uid,
        inode,
        bytes_recv,
        bytes_sent,
        rate_recv: 0.0,
        rate_sent: 0.0,
        pid: None,
        proc: None,
        dead: false,
        first_seen: None,
        dead_at: None,
    })
}

#[cfg(target_os = "linux")]
fn ip_from(b: &[u8], family: u8) -> IpAddr {
    if family as libc::c_int == AF_INET {
        IpAddr::V4(Ipv4Addr::new(b[0], b[1], b[2], b[3]))
    } else {
        let mut octets = [0u8; 16];
        octets.copy_from_slice(b);
        IpAddr::V6(octets.into())
    }
}

/// ::ffff:a.b.c.d dual-stack sockets count as IPv4 for LAN matching
#[cfg(target_os = "linux")]
fn normalize(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(v6)),
        v4 => v4,
    }
}

/// inode -> (pid, comm); only sockets of processes we may read (own uid)
#[cfg(target_os = "linux")]
fn scan_pids() -> HashMap<u64, (u32, String)> {
    let mut out = HashMap::new();
    let Ok(pids) = fs::read_dir("/proc") else {
        return out;
    };
    for p in pids.flatten() {
        let Ok(pid) = p.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let Ok(fds) = fs::read_dir(p.path().join("fd")) else {
            continue;
        };
        let mut comm: Option<String> = None;
        for f in fds.flatten() {
            let Ok(link) = fs::read_link(f.path()) else {
                continue;
            };
            let s = link.to_string_lossy();
            let Some(rest) = s.strip_prefix("socket:[") else {
                continue;
            };
            let Ok(ino) = rest.trim_end_matches(']').parse::<u64>() else {
                continue;
            };
            if comm.is_none() {
                comm = fs::read_to_string(p.path().join("comm"))
                    .ok()
                    .map(|s| s.trim().to_string());
            }
            out.entry(ino)
                .or_insert((pid, comm.clone().unwrap_or_else(|| "?".into())));
        }
    }
    out
}

fn load_users() -> HashMap<u32, String> {
    let mut out = HashMap::new();
    if let Ok(content) = fs::read_to_string("/etc/passwd") {
        for line in content.lines() {
            let f: Vec<&str> = line.split(':').collect();
            if f.len() >= 4 {
                if let Ok(uid) = f[2].parse::<u32>() {
                    out.insert(uid, f[0].to_string());
                }
            }
        }
    }
    out
}

#[cfg(target_os = "linux")]
fn u16from(b: &[u8]) -> u16 {
    u16::from_ne_bytes([b[0], b[1]])
}

#[cfg(target_os = "linux")]
fn u32from(b: &[u8]) -> u32 {
    u32::from_ne_bytes([b[0], b[1], b[2], b[3]])
}

#[cfg(target_os = "linux")]
fn u64from(b: &[u8]) -> u64 {
    u64::from_ne_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collect::udp::InodeDelta;

    fn udp_row(inode: u64) -> Conn {
        Conn {
            proto: IPPROTO_UDP,
            local: ("127.0.0.1".parse().unwrap(), 5353),
            remote: ("0.0.0.0".parse().unwrap(), 0),
            state: 7,
            uid: 0,
            inode,
            bytes_recv: 0,
            bytes_sent: 0,
            rate_recv: 0.0,
            rate_sent: 0.0,
            pid: None,
            proc: None,
            dead: false,
            first_seen: None,
            dead_at: None,
        }
    }

    #[test]
    fn proc_groups_order_by_total_traffic_desc() {
        let mk = |inode: u64, name: &str, recv: u64, sent: u64| Conn {
            proto: IPPROTO_TCP,
            local: ("127.0.0.1".parse().unwrap(), 1000 + inode as u16),
            remote: ("93.184.216.34".parse().unwrap(), 443),
            state: 1,
            uid: 0,
            inode,
            bytes_recv: recv,
            bytes_sent: sent,
            rate_recv: 0.0,
            rate_sent: 0.0,
            pid: Some(100 + inode as u32),
            proc: Some(name.to_string()),
            dead: false,
            first_seen: None,
            dead_at: None,
        };
        // firefox 组合计 1500,chrome 组 3000,散行 500:期望 chrome、firefox×2、散行
        let mut conns = vec![
            mk(1, "firefox", 1000, 500),
            mk(2, "chrome", 3000, 0),
            mk(3, "firefox", 500, 0),
            mk(4, "misc", 500, 0),
        ];
        sort_by_proc_group(&mut conns);
        let names: Vec<&str> = conns.iter().map(|c| c.proc.as_deref().unwrap()).collect();
        assert_eq!(names, vec!["chrome", "firefox", "firefox", "misc"]);
        // 同组内按合计降序:firefox 的 1500 行在 500 行前
        assert_eq!(conns[1].bytes_recv + conns[1].bytes_sent, 1500);
        assert_eq!(conns[2].bytes_recv + conns[2].bytes_sent, 500);
    }

    #[test]
    fn udp_deltas_land_on_the_matching_socket_row() {
        let mut c = ConnCollector::new();
        c.conns = vec![udp_row(1009), udp_row(2000)];
        c.apply_udp(&[
            InodeDelta {
                inode: 1009,
                recv: 600,
                sent: 120,
                dt: 1.0,
            },
            InodeDelta {
                inode: 404, // socket gone between snapshot and now: ignored
                recv: 5,
                sent: 0,
                dt: 1.0,
            },
        ]);
        let hit = &c.conns[0];
        assert_eq!(hit.bytes_recv, 600); // session Σ injected
        assert_eq!(hit.bytes_sent, 120);
        assert!((hit.rate_recv - 600.0).abs() < 1e-9);
        assert!((hit.rate_sent - 120.0).abs() < 1e-9);
        let miss = &c.conns[1];
        assert_eq!(miss.bytes_recv + miss.bytes_sent, 0);
        // an idle round (no deltas) must NOT drop the Σ back to 0: rows are
        // rebuilt every tick, so the injection has to happen every tick too
        c.conns = vec![udp_row(1009)];
        c.apply_udp(&[]);
        assert_eq!(c.conns[0].bytes_recv, 600, "空闲轮 Σ 被清掉了");
        assert_eq!(c.conns[0].rate_recv, 0.0);
        // reset clears the accumulated Σ so counting restarts from zero
        c.reset_totals();
        c.conns = vec![udp_row(1009)];
        c.apply_udp(&[InodeDelta {
            inode: 1009,
            recv: 7,
            sent: 0,
            dt: 1.0,
        }]);
        assert_eq!(c.conns[0].bytes_recv, 7);
    }

    #[test]
    fn udp_joins_the_lan_neighbour_session_totals() {
        let mut c = ConnCollector::new();
        let mut row = udp_row(1009);
        row.remote = ("192.168.1.7".parse().unwrap(), 9999);
        c.conns = vec![row];
        c.apply_udp(&[InodeDelta {
            inode: 1009,
            recv: 500,
            sent: 100,
            dt: 1.0,
        }]);
        let agg = c.per_remote_v4();
        let &(n, rr, _rs, sum_r, sum_w) = agg.get(&"192.168.1.7".parse().unwrap()).unwrap();
        assert_eq!((n, sum_r, sum_w), (1, 500, 100));
        assert!(rr > 0.0);
        // a second round adds only the delta, not the cumulative again
        let mut row2 = udp_row(1009);
        row2.remote = ("192.168.1.7".parse().unwrap(), 9999);
        c.conns = vec![row2];
        c.apply_udp(&[InodeDelta {
            inode: 1009,
            recv: 200,
            sent: 0,
            dt: 1.0,
        }]);
        let (_, _, _, sum_r, _) = c.per_remote_v4()[&"192.168.1.7".parse().unwrap()];
        assert_eq!(sum_r, 700, "remote_totals 被累计值重复计入");
    }

    #[test]
    fn tombstones_keep_closed_connections_then_expire() {
        let mut c = ConnCollector::new();
        c.tombstone_ttl = Duration::from_millis(100);
        let mut row = udp_row(1009);
        row.bytes_recv = 500;
        // 上一轮活着
        c.last_live.insert(1009, row.clone());
        let t0 = Instant::now();
        // 这轮消失:转为墓碑,数值保留,追加在行尾
        let mut live: Vec<Conn> = Vec::new();
        c.update_tombstones(&mut live, t0);
        c.conns = live;
        assert_eq!(c.conns.len(), 1);
        let tomb = &c.conns[0];
        assert!(tomb.dead);
        assert_eq!(tomb.bytes_recv, 500);
        assert_eq!(tomb.inode, 1009);
        // TTL 内重复簿记(sample() 每轮都传全新向量):墓碑仍在,不重复堆积
        let mut live: Vec<Conn> = Vec::new();
        c.update_tombstones(&mut live, t0);
        c.conns = live;
        assert_eq!(c.conns.len(), 1);
        // TTL 过期:移除
        let mut live: Vec<Conn> = Vec::new();
        c.update_tombstones(&mut live, t0 + Duration::from_millis(150));
        c.conns = live;
        assert!(c.conns.is_empty());
    }

    #[test]
    fn revived_inode_drops_the_tombstone() {
        let mut c = ConnCollector::new();
        let mut row = udp_row(1009);
        row.bytes_recv = 500; // 0 字节的行不产生墓碑
        c.last_live.insert(1009, row.clone());
        let t0 = Instant::now();
        let mut live: Vec<Conn> = Vec::new();
        c.update_tombstones(&mut live, t0);
        assert_eq!(live.len(), 1);
        // 同 inode 复活:墓碑让位给活行
        let mut revived = vec![row];
        c.update_tombstones(&mut revived, t0);
        assert_eq!(revived.len(), 1);
        assert!(!revived[0].dead);
        assert_eq!(c.tombstones.len(), 0);
    }

    #[test]
    fn apply_udp_lands_on_tombstones_too() {
        let mut c = ConnCollector::new();
        let mut row = udp_row(1009);
        row.bytes_recv = 500;
        c.last_live.insert(1009, row);
        let t0 = Instant::now();
        let mut live: Vec<Conn> = Vec::new();
        c.update_tombstones(&mut live, t0);
        c.conns = live;
        // 连接关闭后迟到的最后一笔增量也要落到墓碑上
        c.apply_udp(&[InodeDelta {
            inode: 1009,
            recv: 300,
            sent: 0,
            dt: 1.0,
        }]);
        assert_eq!(c.conns.len(), 1, "墓碑行应在 conns 里");
        // Σ 以 udp_totals(会话真值)覆写;测试伪造的 500 不在其中,
        // 只应显示迟到增量的 300
        assert_eq!(c.conns[0].bytes_recv, 300);
    }

    /// the real inet_diag dump must carry UDP sockets with their inodes,
    /// otherwise per-socket UDP deltas have nothing to land on
    #[test]
    #[cfg(target_os = "linux")]
    fn netlink_dump_contains_udp_sockets() {
        let mut c = ConnCollector::new();
        c.sample();
        let udp_rows: Vec<_> = c.conns.iter().filter(|c| !c.is_tcp()).collect();
        assert!(!udp_rows.is_empty(), "inet_diag 没有返回任何 UDP socket");
        assert!(udp_rows.iter().all(|c| c.inode > 0));
    }

    /// full junction: real netlink rows + synthetic snapshot diff through
    /// UdpCollector + apply_udp — Σ AND rate must both land on the row
    #[test]
    #[cfg(target_os = "linux")]
    fn netlink_rows_receive_udp_rates_end_to_end() {
        use crate::collect::udp::UdpCollector;
        let mut c = ConnCollector::new();
        c.sample();
        let inode = {
            let row = c.conns.iter().find(|c| !c.is_tcp()).expect("没有 UDP 行");
            row.inode
        };
        let path = std::env::temp_dir().join(format!("ltop-udp-e2e-{}", std::process::id()));
        let write = |ts: u64, recv: u64| {
            std::fs::write(
                &path,
                format!(
                    "{{\"inst\":\"e2e\",\"ts\":{ts},\"pids\":[],\"sks\":[{{\"ino\":{inode},\"recv\":{recv},\"sent\":0}}]}}"
                ),
            )
            .unwrap();
        };
        write(1000, 100);
        let mut u = UdpCollector::new();
        u.path = path.clone();
        assert!(u.sample().inodes.is_empty()); // 基线
        write(2000, 700);
        let deltas = u.sample();
        std::fs::remove_file(&path).ok();
        assert_eq!(deltas.inodes.len(), 1);
        assert_eq!(deltas.inodes[0].inode, inode);
        c.apply_udp(&deltas.inodes);
        let row = c.conns.iter().find(|c| c.inode == inode).unwrap();
        assert!(row.rate_recv > 0.0, "UDP 行速率未注入: {}", row.rate_recv);
        assert_eq!(row.bytes_recv, 600, "UDP 行 Σ 未注入");
    }
}

//! Per-process session accounting: total network traffic and its UDP share
//! (both from the eBPF deltas fed in by the `ltop-bpf` daemon — the pid
//! ledger rides kprobes on the syscalls, so even connections that close
//! within one sampling tick are counted, which the old netlink per-socket
//! diff missed) and total disk I/O (from /proc/<pid>/io) accumulated over
//! the monitoring session, so abnormal processes stand out.

use std::collections::HashMap;
use std::fs;
use std::time::Instant;

use libc::geteuid;

use crate::collect::udp::UdpDelta;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ProcSort {
    Name,
    NetSum,
    NetRate,
    DiskSum,
    DiskRate,
}

impl ProcSort {
    pub fn next(self) -> Self {
        match self {
            Self::Name => Self::NetSum,
            Self::NetSum => Self::NetRate,
            Self::NetRate => Self::DiskSum,
            Self::DiskSum => Self::DiskRate,
            Self::DiskRate => Self::Name,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::NetSum => "net-sum",
            Self::NetRate => "net-rate",
            Self::DiskSum => "disk-sum",
            Self::DiskRate => "disk-rate",
        }
    }

    fn sort_key(&self, r: &ProcRow) -> (u64, f64) {
        match self {
            Self::Name => (0, 0.0),
            Self::NetSum => (r.net_recv + r.net_sent, 0.0),
            Self::NetRate => (0, r.net_rr + r.net_sr),
            Self::DiskSum => (r.disk_r + r.disk_w, 0.0),
            Self::DiskRate => (0, r.disk_rr + r.disk_wr),
        }
    }
}

#[derive(Clone)]
pub struct ProcRow {
    pub pid: u32,
    pub name: String,
    pub net_recv: u64,
    pub net_sent: u64,
    pub net_rr: f64,
    pub net_sr: f64,
    /// UDP share inside net_* (eBPF; zero when ltop-bpf is not running)
    pub udp_recv: u64,
    pub udp_sent: u64,
    pub udp_rr: f64,
    pub udp_sr: f64,
    pub disk_r: u64,
    pub disk_w: u64,
    pub disk_rr: f64,
    pub disk_wr: f64,
    /// pid live with the same start time as when the ledger first saw it
    /// 进程已运行秒数(首见时捕获)
    pub uptime_s: Option<u64>,
    pub alive: bool,
    /// command line captured while the process was alive — lets the detail
    /// panel show and restart it after exit
    pub cmdline: Option<Vec<String>>,
}

impl ProcRow {
    pub fn total_rate(&self) -> f64 {
        self.net_rr + self.net_sr + self.disk_rr + self.disk_wr
    }

    pub fn total_bytes(&self) -> u64 {
        self.net_recv + self.net_sent + self.disk_r + self.disk_w
    }
}

#[derive(Clone)]
struct ProcState {
    net_recv: u64,
    net_sent: u64,
    udp_recv: u64,
    udp_sent: u64,
    disk_r: u64,
    disk_w: u64,
    /// /proc/<pid>/stat starttime in clock ticks — distinguishes the process
    /// we recorded from a later one that reused the pid
    start_ticks: Option<u64>,
    cmdline: Option<Vec<String>>,
    /// 首见时的进程运行秒数(/proc/uptime − starttime/CLK_TCK)
    uptime_s: Option<u64>,
}

impl ProcState {
    fn zero() -> Self {
        Self {
            net_recv: 0,
            net_sent: 0,
            udp_recv: 0,
            udp_sent: 0,
            disk_r: 0,
            disk_w: 0,
            start_ticks: None,
            cmdline: None,
            uptime_s: None,
        }
    }

    /// first entry into the ledger: snapshot identity while it is still alive
    fn first_sight(pid: u32) -> Self {
        let mut s = Self::zero();
        s.start_ticks = read_pid_start(pid);
        s.cmdline = read_pid_cmdline(pid);
        s.uptime_s = read_pid_uptime_s(pid);
        s
    }
}

pub struct ProcCollector {
    acc: HashMap<(u32, String), ProcState>,
    io_prev: HashMap<u32, (u64, u64)>,
    /// last readable /proc/<pid>/comm per pid, so UDP deltas of a process
    /// that just exited still land under its name
    udp_names: HashMap<u32, String>,
    last_t: Instant,
    pub procs: Vec<ProcRow>,
    pub sort: ProcSort,
    /// euid == 0: /proc/<pid>/fd and /proc/<pid>/io are readable for all users
    pub is_root: bool,
}

impl Default for ProcCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcCollector {
    pub fn new() -> Self {
        Self {
            acc: HashMap::new(),
            io_prev: HashMap::new(),
            udp_names: HashMap::new(),
            last_t: Instant::now(),
            procs: Vec::new(),
            sort: ProcSort::NetSum,
            is_root: unsafe { geteuid() == 0 },
        }
    }

    /// `net` = per-pid deltas TCP+UDP combined (net Σ and rates), `udp` =
    /// the UDP share of the same traffic (the breakdown fields only)
    pub fn sample(&mut self, net: &[UdpDelta], udp: &[UdpDelta]) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_t).as_secs_f64().max(1e-6);
        self.last_t = now;

        // --- network: fold per-pid eBPF deltas into per-process totals ---
        let mut net_rates: HashMap<(u32, String), (f64, f64)> = HashMap::new();
        for d in net {
            let key = (d.pid, self.udp_name(d.pid));
            let st = self
                .acc
                .entry(key.clone())
                .or_insert_with(|| ProcState::first_sight(d.pid));
            st.net_recv += d.recv;
            st.net_sent += d.sent;
            let r = net_rates.entry(key).or_insert((0.0, 0.0));
            r.0 += d.recv as f64 / d.dt;
            r.1 += d.sent as f64 / d.dt;
        }

        // --- disk: /proc/<pid>/io read_bytes/write_bytes deltas; the same
        // pass records which pids are alive right now and their start times ---
        let mut io_rates: HashMap<u32, (f64, f64)> = HashMap::new();
        let mut pids_now: HashMap<u32, u64> = HashMap::new();
        if let Ok(pids) = fs::read_dir("/proc") {
            for p in pids.flatten() {
                let Ok(pid) = p.file_name().to_string_lossy().parse::<u32>() else {
                    continue;
                };
                if let Some(start) = read_pid_start(pid) {
                    pids_now.insert(pid, start);
                }
                let (Some(rb), Some(wb), Some(comm)) = read_pid_io(&p.path()) else {
                    continue;
                };
                let (dr, dw) = match self.io_prev.get(&pid) {
                    Some(&(pr, pw)) => (rb.saturating_sub(pr), wb.saturating_sub(pw)),
                    None => (0, 0),
                };
                self.io_prev.insert(pid, (rb, wb));
                io_rates.insert(pid, (dr as f64 / dt, dw as f64 / dt));
                if dr > 0 || dw > 0 {
                    let key = (pid, comm);
                    let st = self
                        .acc
                        .entry(key)
                        .or_insert_with(|| ProcState::first_sight(pid));
                    st.disk_r += dr;
                    st.disk_w += dw;
                }
            }
        }

        // --- udp: the breakdown of the very same traffic (the net deltas
        // above already carry it), kept separate for the detail panel ---
        let udp_rates: HashMap<u32, (f64, f64)> = if !udp.is_empty() {
            let mut udp_rates: HashMap<u32, (f64, f64)> = HashMap::new();
            for d in udp {
                let name = self.udp_name(d.pid);
                let key = (d.pid, name);
                let st = self
                    .acc
                    .entry(key.clone())
                    .or_insert_with(|| ProcState::first_sight(d.pid));
                st.udp_recv += d.recv;
                st.udp_sent += d.sent;
                let r = udp_rates.entry(d.pid).or_insert((0.0, 0.0));
                r.0 += d.recv as f64 / d.dt;
                r.1 += d.sent as f64 / d.dt;
            }
            udp_rates
        } else {
            HashMap::new()
        };

        // --- rows ---
        self.procs = self
            .acc
            .iter()
            .map(|((pid, name), st)| {
                let (nrr, nsr) = net_rates
                    .get(&(*pid, name.clone()))
                    .copied()
                    .unwrap_or((0.0, 0.0));
                let (urr, usr) = udp_rates.get(pid).copied().unwrap_or((0.0, 0.0));
                let (drr, dwr) = io_rates.get(pid).copied().unwrap_or((0.0, 0.0));
                let alive = row_alive(st.start_ticks, pids_now.get(pid).copied());
                ProcRow {
                    pid: *pid,
                    name: name.clone(),
                    net_recv: st.net_recv,
                    net_sent: st.net_sent,
                    net_rr: nrr,
                    net_sr: nsr,
                    udp_recv: st.udp_recv,
                    udp_sent: st.udp_sent,
                    udp_rr: urr,
                    udp_sr: usr,
                    disk_r: st.disk_r,
                    disk_w: st.disk_w,
                    disk_rr: drr,
                    disk_wr: dwr,
                    uptime_s: st.uptime_s,
                    alive,
                    cmdline: st.cmdline.clone(),
                }
            })
            .filter(|r| r.total_bytes() > 0 || r.total_rate() > 0.0)
            .collect();

        match self.sort {
            ProcSort::Name => self.procs.sort_by(|a, b| a.name.cmp(&b.name)),
            _ => self.procs.sort_by(|a, b| {
                let (a1, a2) = self.sort.sort_key(a);
                let (b1, b2) = self.sort.sort_key(b);
                b1.cmp(&a1)
                    .then_with(|| b2.partial_cmp(&a2).unwrap_or(std::cmp::Ordering::Equal))
            }),
        }
    }

    pub fn reset_net(&mut self) {
        for st in self.acc.values_mut() {
            st.net_recv = 0;
            st.net_sent = 0;
            st.udp_recv = 0;
            st.udp_sent = 0;
        }
    }

    /// name for a UDP-only delta: current comm if readable, else the last one
    /// seen (so a short-lived sender still gets attributed), else "(gone)"
    fn udp_name(&mut self, pid: u32) -> String {
        if let Ok(comm) = fs::read_to_string(format!("/proc/{pid}/comm")) {
            let comm = comm.trim().to_string();
            if !comm.is_empty() {
                if self.udp_names.len() > 8192 {
                    self.udp_names.clear();
                }
                self.udp_names.insert(pid, comm.clone());
                return comm;
            }
        }
        self.udp_names
            .get(&pid)
            .cloned()
            .unwrap_or_else(|| "(gone)".to_string())
    }

    pub fn reset_disk(&mut self) {
        for st in self.acc.values_mut() {
            st.disk_r = 0;
            st.disk_w = 0;
        }
    }
}

/// (read_bytes, write_bytes, comm) if the process lets us read its io stats
fn read_pid_io(path: &std::path::Path) -> (Option<u64>, Option<u64>, Option<String>) {
    let mut r = None;
    let mut w = None;
    if let Ok(content) = fs::read_to_string(path.join("io")) {
        for line in content.lines() {
            if let Some(v) = line.strip_prefix("read_bytes:") {
                r = v.trim().parse().ok();
            } else if let Some(v) = line.strip_prefix("write_bytes:") {
                w = v.trim().parse().ok();
            }
        }
    }
    let comm = fs::read_to_string(path.join("comm"))
        .ok()
        .map(|s| s.trim().to_string());
    (r, w, comm)
}

/// /proc/<pid>/stat starttime (clock ticks since boot). Field 22 overall;
/// after the `(comm)` parenthesis (which may contain spaces) it is the 20th
/// token, so index 19 — this is how a reused pid is told apart.
/// 进程已运行秒数:/proc/uptime − starttime/CLK_TCK
fn read_pid_uptime_s(pid: u32) -> Option<u64> {
    let start_ticks = read_pid_start(pid)?;
    let uptime = fs::read_to_string("/proc/uptime").ok()?;
    let up = uptime.split_whitespace().next()?.parse::<f64>().ok()?;
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as f64;
    let age = up - start_ticks as f64 / hz;
    Some(age.max(0.0) as u64)
}

fn read_pid_start(pid: u32) -> Option<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after = stat.rsplit_once(')')?.1;
    after.split_whitespace().nth(19)?.parse().ok()
}

/// argv as it was invoked; None for kernel threads and zombies (empty cmdline)
fn read_pid_cmdline(pid: u32) -> Option<Vec<String>> {
    let raw = fs::read_to_string(format!("/proc/{pid}/cmdline")).ok()?;
    let parts: Vec<String> = raw
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts)
    }
}

/// alive = a process with the same pid exists and its start time matches the
/// one recorded at first sight; unknown start times fall back to pid existence
fn row_alive(start: Option<u64>, now: Option<u64>) -> bool {
    match (start, now) {
        (Some(s), Some(cur)) => s == cur,
        (Some(_), None) => false,
        (None, known) => known.is_some(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn udp_deltas_land_in_the_ledger_under_the_pid_name() {
        let mut c = ProcCollector::new();
        let pid = std::process::id(); // this test's own /proc/<pid>/comm is readable
        let delta = UdpDelta {
            pid,
            recv: 1000,
            sent: 250,
            dt: 1.0,
        };
        // eBPF 语义:net 是 TCP+UDP 合计,udp 是其中的分项
        c.sample(&[delta], &[delta]);
        let row = c.procs.iter().find(|r| r.pid == pid).expect("udp row");
        assert_eq!(row.udp_recv, 1000);
        assert_eq!(row.udp_sent, 250);
        assert_eq!(row.net_recv, 1000); // net 已含 UDP,不重复折算
        assert_eq!(row.net_sent, 250);
        assert!((row.udp_rr - 1000.0).abs() < 1e-9);
        assert!((row.net_sr - 250.0).abs() < 1e-9);
        assert!(!row.name.is_empty());
    }

    #[test]
    fn reset_net_clears_udp_totals_too() {
        let mut c = ProcCollector::new();
        let pid = std::process::id();
        let d = UdpDelta {
            pid,
            recv: 10,
            sent: 0,
            dt: 1.0,
        };
        c.sample(&[d], &[d]);
        c.reset_net();
        // a fresh delta starts counting from zero, not from 10 (the row may
        // legitimately survive the filter on this test's own disk I/O)
        let d = UdpDelta {
            pid,
            recv: 5,
            sent: 0,
            dt: 1.0,
        };
        c.sample(&[d], &[d]);
        let row = c.procs.iter().find(|r| r.pid == pid).expect("row");
        assert_eq!(row.udp_recv, 5);
    }

    #[test]
    fn row_alive_rules() {
        assert!(row_alive(Some(7), Some(7))); // same pid, same start
        assert!(!row_alive(Some(7), Some(9))); // pid reused by a new process
        assert!(!row_alive(Some(7), None)); // pid gone
        assert!(row_alive(None, Some(7))); // start unknown, pid exists
        assert!(!row_alive(None, None));
    }

    #[test]
    fn pid_start_and_cmdline_read_from_proc() {
        let pid = std::process::id();
        let start = read_pid_start(pid).expect("stat start ticks");
        assert!(start > 0);
        let cmd = read_pid_cmdline(pid).expect("own cmdline");
        assert!(!cmd.is_empty());
        assert!(cmd[0].contains("ltop") || cmd[0].contains("dep"));
    }

    /// full lifecycle: a child that does disk I/O enters the ledger while
    /// alive, and is still listed — but flagged exited — after it dies
    #[test]
    fn exited_process_stays_in_the_ledger_marked_dead() {
        // must live on a real filesystem: /proc/<pid>/io counts storage-bound
        // bytes, writes to tmpfs (e.g. /tmp on Arch) never move the counters
        let tmp = std::env::current_dir()
            .expect("cwd")
            .join(format!("target/ltop-alive-{}", std::process::id()));
        let mut child = std::process::Command::new("sh")
            .arg("-c")
            // builtin echo keeps the write syscall in the sh process itself —
            // a child (dd) would do its own io that sh's counters never see
            .arg(format!(
                "for i in 1 2 3 4 5 6 7 8; do echo aaaaaaaaaaaaaaaaaaaaaaaa > {}; sleep 0.15; done",
                tmp.display()
            ))
            .spawn()
            .expect("spawn writer child");
        let pid = child.id();

        let mut c = ProcCollector::new();
        let mut saw_alive = false;
        for _ in 0..12 {
            c.sample(&[], &[]);
            if let Some(row) = c.procs.iter().find(|r| r.pid == pid) {
                saw_alive = saw_alive || row.alive;
                if row.alive {
                    assert!(row.cmdline.as_ref().is_some_and(|cmd| cmd[0].contains("sh")));
                }
            }
            if child.try_wait().expect("poll").is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        let _ = child.wait();
        std::thread::sleep(std::time::Duration::from_millis(150));
        c.sample(&[], &[]);
        let row = c
            .procs
            .iter()
            .find(|r| r.pid == pid)
            .expect("exited process must stay in the ledger");
        assert!(!row.alive, "row must be flagged exited after death");
        assert!(row.cmdline.is_some());
        assert!(saw_alive, "row must have been seen alive first");
        let _ = fs::remove_file(&tmp);
    }
}

//! AI-friendly structured snapshot (schema v1) — one serialization source
//! shared by `ltop --json` and the monitoring log (v2 JSONL).
//!
//! Compatibility contract: the `schema` field lets consumers branch; fields
//! are additive-only within a schema version (new keys may appear, existing
//! keys never change meaning or disappear).

use serde::Serialize;

use crate::app::App;

pub const SCHEMA: u32 = 1;

#[derive(Serialize)]
pub struct Snapshot {
    pub schema: u32,
    pub meta: Meta,
    pub net: NetSnapshot,
    pub lan: LanSnapshot,
    pub disk: DiskSnapshot,
    pub conns: Vec<ConnSnapshot>,
    pub procs: Vec<ProcSnapshot>,
}

#[derive(Serialize)]
pub struct Meta {
    pub hostname: String,
    pub ts: u64,
    pub version: &'static str,
    pub os: &'static str,
    pub arch: &'static str,
    /// eBPF helper snapshot is fresh (UDP per-process accounting is live)
    pub udp_helper: bool,
}

#[derive(Serialize)]
pub struct IfaceSnapshot {
    pub name: String,
    pub operstate: String,
    pub speed_mbps: Option<u64>,
    pub duplex: Option<String>,
    pub mac: String,
    pub addresses: Vec<String>,
    pub rx_bps: f64,
    pub tx_bps: f64,
    /// session Σ (since ltop start; resettable)
    pub session_rx: u64,
    pub session_tx: u64,
    /// Alive Σ (since the interface came up; not resettable)
    pub alive_rx: u64,
    pub alive_tx: u64,
}

#[derive(Serialize)]
pub struct NetSnapshot {
    pub selected: String,
    pub interfaces: Vec<IfaceSnapshot>,
}

#[derive(Serialize)]
pub struct NeighborSnapshot {
    pub ip: String,
    /// None on macOS (system sanitizes MACs for non-Apple-signed binaries)
    pub mac: Option<String>,
    pub hostname: Option<String>,
    pub iface: String,
    pub is_gateway: bool,
    pub is_self: bool,
    pub tracked: bool,
}

#[derive(Serialize)]
pub struct LanSnapshot {
    pub subnets: Vec<String>,
    pub gateways: Vec<String>,
    pub neighbours: Vec<NeighborSnapshot>,
}

#[derive(Serialize)]
pub struct DiskDevSnapshot {
    pub name: String,
    pub mounts: Vec<String>,
    pub fstype: String,
    pub total: u64,
    pub used: u64,
    pub read_bps: f64,
    pub write_bps: f64,
    pub iops: f64,
}

#[derive(Serialize)]
pub struct DiskSnapshot {
    pub devices: Vec<DiskDevSnapshot>,
    pub session_read: u64,
    pub session_write: u64,
}

#[derive(Serialize)]
pub struct ConnSnapshot {
    pub proto: String,
    pub local: String,
    pub remote: String,
    pub state: String,
    pub process: Option<String>,
    pub pid: Option<u32>,
    pub user: String,
    pub inode: u64,
    pub bytes_recv: u64,
    pub bytes_sent: u64,
    pub bps_recv: f64,
    pub bps_sent: f64,
}

#[derive(Serialize)]
pub struct ProcSnapshot {
    pub pid: u32,
    pub name: String,
    pub net_recv: u64,
    pub net_sent: u64,
    pub net_bps_recv: f64,
    pub net_bps_sent: f64,
    pub udp_recv: u64,
    pub udp_sent: u64,
    pub disk_read: u64,
    pub disk_written: u64,
    pub disk_bps_read: f64,
    pub disk_bps_written: f64,
    pub alive: bool,
}

fn unix_ts() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// build the snapshot from a freshly sampled App (call after `update()`)
pub fn from_app(app: &App) -> Snapshot {
    let name = app.net.selected.clone().unwrap_or_default();
    let interfaces: Vec<IfaceSnapshot> = app
        .net
        .ifaces
        .iter()
        .map(|(iname, info)| {
            let rates = app.net.rates.get(iname);
            let (alive_rx, alive_tx) = app
                .net
                .alive_for(iname)
                .unwrap_or((0, 0));
            IfaceSnapshot {
                name: iname.clone(),
                operstate: info.operstate.clone(),
                speed_mbps: info.speed_mbps,
                duplex: info.duplex.clone(),
                mac: info.mac.clone(),
                addresses: info.addrs.clone(),
                rx_bps: rates.map(|r| r.rx).unwrap_or(0.0),
                tx_bps: rates.map(|r| r.tx).unwrap_or(0.0),
                session_rx: rates.map(|r| r.rx_total).unwrap_or(0),
                session_tx: rates.map(|r| r.tx_total).unwrap_or(0),
                alive_rx,
                alive_tx,
            }
        })
        .collect();

    let neighbours = app
        .lan
        .sorted_neighbors()
        .iter()
        .map(|n| NeighborSnapshot {
            ip: n.ip.to_string(),
            mac: n.mac.clone(),
            hostname: n.hostname.clone(),
            iface: n.iface.clone(),
            is_gateway: n.is_gateway,
            is_self: n.is_self,
            tracked: app.lan.tracked.contains(&n.ip),
        })
        .collect();

    let conns = app
        .conn
        .conns
        .iter()
        .map(|c| ConnSnapshot {
            proto: if c.is_tcp() { "tcp".into() } else { "udp".into() },
            local: crate::collect::conn::sock_str(&c.local),
            remote: crate::collect::conn::sock_str(&c.remote),
            state: if c.is_listen() {
                "LISTEN".into()
            } else if c.is_tcp() {
                "ESTAB".into()
            } else {
                "UDP".into()
            },
            process: c.proc.clone(),
            pid: c.pid,
            user: app.conn.user_name(c.uid),
            inode: c.inode,
            bytes_recv: c.bytes_recv,
            bytes_sent: c.bytes_sent,
            bps_recv: c.rate_recv,
            bps_sent: c.rate_sent,
        })
        .collect();

    let procs = app
        .proc
        .procs
        .iter()
        .map(|p| ProcSnapshot {
            pid: p.pid,
            name: p.name.clone(),
            net_recv: p.net_recv,
            net_sent: p.net_sent,
            net_bps_recv: p.net_rr,
            net_bps_sent: p.net_sr,
            udp_recv: p.udp_recv,
            udp_sent: p.udp_sent,
            disk_read: p.disk_r,
            disk_written: p.disk_w,
            disk_bps_read: p.disk_rr,
            disk_bps_written: p.disk_wr,
            alive: p.alive,
        })
        .collect();

    Snapshot {
        schema: SCHEMA,
        meta: Meta {
            hostname: app.hostname.clone(),
            ts: unix_ts(),
            version: env!("CARGO_PKG_VERSION"),
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
            udp_helper: app.udp.fresh,
        },
        net: NetSnapshot { selected: name, interfaces },
        lan: LanSnapshot {
            subnets: app
                .lan
                .subnets
                .iter()
                .map(|(_, n, p)| format!("{n}/{p}"))
                .collect(),
            gateways: app.lan.gateways.iter().map(|g| g.to_string()).collect(),
            neighbours,
        },
        disk: DiskSnapshot {
            devices: app
                .disk
                .devices
                .iter()
                .map(|d| DiskDevSnapshot {
                    name: d.name.clone(),
                    mounts: d.mounts.clone(),
                    fstype: d.fstype.clone(),
                    total: d.total,
                    used: d.used,
                    read_bps: d.rate_r,
                    write_bps: d.rate_w,
                    iops: d.iops,
                })
                .collect(),
            session_read: app.disk.totals().3,
            session_write: app.disk.totals().4,
        },
        conns,
        procs,
    }
}

//! UDP per-process and per-socket accounting, fed by the privileged
//! `ltop-bpf` companion daemon (kprobes on udp_sendmsg / udp_recvmsg + v6).
//!
//! The daemon writes a snapshot of *cumulative* byte counters to
//! `/run/ltop/udp.json` once per sampling interval, keyed two ways: by pid
//! (for the PROCS ledger) and by socket INODE (for CONNS — the BPF program
//! walks sk→socket→file→inode in-kernel via CO-RE, because /proc/net/udp's
//! pointer column is a per-boot hash on this kernel and cannot be used for
//! correlation). This collector diffs consecutive snapshots it happens to
//! read (counter resets, pid reuse and inode reuse clamp at zero, same
//! convention as `--replay`). Without a fresh snapshot everything silently
//! degrades.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// cumulative byte counters per key, as carried by every snapshot
type Counters = HashMap<u64, (u64, u64)>;

/// one decoded snapshot from the daemon
#[derive(Debug, PartialEq, Clone)]
pub struct Snap {
    pub ts: u64,
    /// writer identity (`<pid>-<start_ms>`); a change means a new kernel map,
    /// i.e. a new counter universe — diffs across instances would be garbage
    pub inst: Option<String>,
    pub pids: Counters,
    /// UDP-only per-pid counters (the breakdown behind ProcRow.udp_*)
    pub upids: Counters,
    /// keyed by socket INODE (resolved in-kernel by the BPF program)
    pub sks: Counters,
}

/// one snapshot-to-snapshot increment for a pid
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UdpDelta {
    pub pid: u32,
    pub recv: u64,
    pub sent: u64,
    /// seconds covered by this delta (the daemon's interval, not ltop's tick)
    pub dt: f64,
}

/// one increment for a socket inode
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InodeDelta {
    pub inode: u64,
    pub recv: u64,
    pub sent: u64,
    pub dt: f64,
}

/// everything one sampling round produced
#[derive(Default)]
pub struct UdpSample {
    /// per-pid deltas, TCP+UDP combined (the PROCS net ledger)
    pub net: Vec<UdpDelta>,
    /// per-pid deltas, UDP only (the PROCS UDP breakdown)
    pub udp: Vec<UdpDelta>,
    /// per-inode deltas, UDP only (CONNS)
    pub inodes: Vec<InodeDelta>,
}

/// per-key diff state; both sides (pid, inode) run the identical engine
#[derive(Default)]
struct DiffSide {
    ts: Option<u64>,
    map: Counters,
}

impl DiffSide {
    /// diff `cur` against the previous snapshot; first sighting baselines,
    /// timestamp regressions re-baseline, resets/reuse clamp at zero
    fn diff(&mut self, ts: u64, cur: Counters) -> Vec<(u64, u64, u64, f64)> {
        let Some(prev_ts) = self.ts else {
            self.ts = Some(ts);
            self.map = cur;
            return Vec::new();
        };
        if prev_ts >= ts {
            self.ts = Some(ts);
            self.map = cur;
            return Vec::new();
        }
        let dt = (ts - prev_ts) as f64 / 1000.0;
        let mut out = Vec::new();
        let mut kept = Counters::new();
        for (&key, &(cr, cs)) in &cur {
            let base = self.map.get(&key).copied().unwrap_or((cr, cs));
            let (dr, ds) = (cr.saturating_sub(base.0), cs.saturating_sub(base.1));
            kept.insert(key, (cr, cs));
            if dr > 0 || ds > 0 {
                out.push((key, dr, ds, dt));
            }
        }
        self.ts = Some(ts);
        self.map = kept;
        out
    }
}

pub struct UdpCollector {
    pids: DiffSide,
    upids: DiffSide,
    sks: DiffSide,
    /// writer identity of the last snapshot; a switch re-baselines both sides
    prev_inst: Option<String>,
    /// snapshot path; writable by the daemon only
    pub path: PathBuf,
    /// pids / sockets present in the most recent snapshot read
    pub pids_seen: usize,
    pub sks_seen: usize,
    /// the last snapshot was seen recently enough to trust (`fresh` badge)
    pub fresh: bool,
    /// snapshot age beyond this counts as stale (badge off, deltas still merged)
    pub max_age_ms: u64,
    /// socket-level deltas of the last round (--dump visibility)
    pub last_inodes: Vec<InodeDelta>,
}

impl Default for UdpCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl UdpCollector {
    pub fn new() -> Self {
        Self {
            pids: DiffSide::default(),
            upids: DiffSide::default(),
            sks: DiffSide::default(),
            prev_inst: None,
            // LANMON_UDP_SNAPSHOT overrides for tests / unusual deployments;
            // the daemon has the matching --out flag
            path: std::env::var("LANMON_UDP_SNAPSHOT")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("/run/ltop/udp.json")),
            pids_seen: 0,
            sks_seen: 0,
            fresh: false,
            max_age_ms: 10_000,
            last_inodes: Vec::new(),
        }
    }

    /// read the daemon snapshot (if any) and diff it against the previous one
    pub fn sample(&mut self) -> UdpSample {
        self.pids_seen = 0;
        self.sks_seen = 0;
        self.fresh = false;
        let Ok(text) = fs::read_to_string(&self.path) else {
            self.pids.ts = None;
            self.upids.ts = None;
            self.sks.ts = None;
            return UdpSample::default();
        };
        match parse_snapshot(&text) {
            Some(snap) => self.ingest(snap),
            None => UdpSample::default(),
        }
    }

    fn ingest(&mut self, snap: Snap) -> UdpSample {
        self.pids_seen = snap.pids.len();
        self.sks_seen = snap.sks.len();
        self.fresh = now_ms().saturating_sub(snap.ts) <= self.max_age_ms;
        if self.prev_inst != snap.inst {
            // a different (or first-seen) writer: its counters are unrelated
            // to whatever we diffed before — baseline, never emit a delta
            self.prev_inst = snap.inst;
            let Snap {
                ts,
                pids,
                upids,
                sks,
                ..
            } = snap;
            self.pids.diff(ts, pids);
            self.upids.diff(ts, upids);
            self.sks.diff(ts, sks);
            return UdpSample::default();
        }
        let Snap {
            ts,
            pids,
            upids,
            sks,
            ..
        } = snap;
        let net_deltas = self.pids.diff(ts, pids);
        let udp_deltas = self.upids.diff(ts, upids);
        let sk_deltas = self.sks.diff(ts, sks);
        self.last_inodes.clear();
        // socket deltas are already inode-keyed by the BPF side
        UdpSample {
            net: pid_deltas_from(net_deltas),
            udp: pid_deltas_from(udp_deltas),
            inodes: {
                let inodes: Vec<InodeDelta> = sk_deltas
                    .into_iter()
                    .map(|(inode, recv, sent, dt)| InodeDelta {
                        inode,
                        recv,
                        sent,
                        dt,
                    })
                    .collect();
                self.last_inodes = inodes.clone();
                inodes
            },
        }
    }
}

fn pid_deltas_from(raw: Vec<(u64, u64, u64, f64)>) -> Vec<UdpDelta> {
    raw.into_iter()
        .map(|(key, recv, sent, dt)| UdpDelta {
            pid: key as u32,
            recv,
            sent,
            dt,
        })
        .collect()
}

/// parse the daemon's snapshot:
/// `{"inst":"<pid>-<ms>","ts":<ms>,"pids":[{"pid":N,...}],"upids":[{"pid":N,...}],"sks":[{"ino":N,...}]}`
/// written by `ltop-bpf` in exactly this key order; tolerant of trailing
/// garbage but not of reordered keys (both ends ship together)
pub fn parse_snapshot(text: &str) -> Option<Snap> {
    let inst = str_after(text, "\"inst\":\"").map(|s| s.to_string());
    let ts = num_after(text, "\"ts\":")?;
    let pids = parse_counters(text, "\"pids\":[", "\"pid\":")?;
    let upids = parse_counters(text, "\"upids\":[", "\"pid\":").unwrap_or_default();
    let sks = parse_counters(text, "\"sks\":[", "\"ino\":").unwrap_or_default();
    Some(Snap {
        ts,
        inst,
        pids,
        upids,
        sks,
    })
}

/// scan a fixed-shape array of `{"<keyfield>":<key>,"recv":R,"sent":S}` objects
fn parse_counters(text: &str, array_key: &str, key_pat: &str) -> Option<Counters> {
    let start = text.find(array_key)? + array_key.len();
    // 扫描必须止于本数组的 ']':数组是扁平对象列表,无嵌套;越过边界会把
    // 后续数组(如 upids)的同名键并进来,把 pids 的合计值覆盖成 UDP 分项
    let end = text[start..].find(']').unwrap_or(text.len() - start);
    let mut rest = &text[start..start + end];
    let mut out = Counters::new();
    while let Some(p) = rest.find(key_pat) {
        rest = &rest[p + key_pat.len()..];
        let Some(key) = num_at(rest) else {
            break;
        };
        let recv = num_after(rest, "\"recv\":");
        let sent = num_after(rest, "\"sent\":");
        match (recv, sent) {
            (Some(recv), Some(sent)) => {
                out.insert(key, (recv, sent));
            }
            _ => break,
        }
    }
    Some(out)
}

/// first number after `key` anywhere in `s`
fn num_after(s: &str, key: &str) -> Option<u64> {
    let p = s.find(key)? + key.len();
    num_at(&s[p..])
}

/// string up to the closing quote after `key`
fn str_after<'a>(s: &'a str, key: &str) -> Option<&'a str> {
    let rest = &s[s.find(key)? + key.len()..];
    let end = rest.find('"')?;
    Some(&rest[..end])
}

/// leading ASCII digits as a number, if any
fn num_at(s: &str) -> Option<u64> {
    let n = num_len(s);
    (n > 0).then(|| s[..n].parse().ok())?
}

fn num_len(s: &str) -> usize {
    s.bytes().take_while(|b| b.is_ascii_digit()).count()
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SNAP: &str = concat!(
        "{\"inst\":\"777-1234\",\"ts\":170000001000,\"pids\":[",
        "{\"pid\":100,\"recv\":1000,\"sent\":200},",
        "{\"pid\":200,\"recv\":0,\"sent\":50}],",
        "\"upids\":[{\"pid\":100,\"recv\":1000,\"sent\":200}],",
        "\"sks\":[{\"ino\":1009,\"recv\":800,\"sent\":100}]}"
    );

    fn map(pairs: &[(u64, u64, u64)]) -> Counters {
        pairs.iter().map(|(k, r, s)| (*k, (*r, *s))).collect()
    }

    fn snap(inst: &str, ts: u64, pids: &[(u64, u64, u64)]) -> Snap {
        Snap {
            ts,
            inst: if inst.is_empty() {
                None
            } else {
                Some(inst.into())
            },
            pids: map(pids),
            upids: Counters::new(),
            sks: Counters::new(),
        }
    }

    #[test]
    fn parses_snapshot_pids_and_sks() {
        let s = parse_snapshot(SNAP).unwrap();
        assert_eq!(s.ts, 170000001000);
        assert_eq!(s.inst.as_deref(), Some("777-1234"));
        assert_eq!(s.pids[&100], (1000, 200));
        assert_eq!(s.pids[&200], (0, 50));
        assert_eq!(s.sks[&1009], (800, 100));
    }

    #[test]
    fn empty_or_malformed_snapshots_yield_none() {
        assert!(parse_snapshot("").is_none());
        assert!(parse_snapshot("{").is_none());
        assert!(
            parse_snapshot("{\"inst\":\"x\",\"ts\":1,\"pids\":[],\"sks\":[]}")
                .unwrap()
                .pids
                .is_empty()
        );
        // truncated tail keeps the complete leading entries
        let s = parse_snapshot(
            "{\"inst\":\"x\",\"ts\":7,\"pids\":[{\"pid\":9,\"recv\":1,\"sent\":2},{\"pid\":",
        )
        .unwrap();
        assert_eq!(s.ts, 7);
        assert_eq!(s.pids.len(), 1);
    }

    #[test]
    fn str_after_reads_up_to_closing_quote() {
        assert_eq!(
            str_after("{\"inst\":\"a-b-1\"}", "\"inst\":\""),
            Some("a-b-1")
        );
        assert_eq!(str_after("{\"ts\":1}", "\"inst\":\""), None);
    }

    /// 回归:pids 与 upids 含同名 pid 时,合计值不得被 UDP 分项覆盖
    /// (旧解析器越过 pids 数组的边界,把 upids 的值并进来)
    #[test]
    fn pids_array_boundary_stops_the_scanner() {
        let text = concat!(
            "{\"inst\":\"x\",\"ts\":1,\"pids\":[{\"pid\":9,\"recv\":900000,\"sent\":100}],",
            "\"upids\":[{\"pid\":9,\"recv\":800,\"sent\":50}],",
            "\"sks\":[]}"
        );
        let s = parse_snapshot(text).unwrap();
        assert_eq!(s.pids[&9], (900000, 100), "合计被 upids 覆盖了");
        assert_eq!(s.upids[&9], (800, 50));
    }

    #[test]
    fn net_and_udp_deltas_arrive_as_separate_channels() {
        let mut c = UdpCollector::new();
        // 组合账本含 TCP+UDP,分项账本只有 UDP:一次 ingest 同时出两路
        let s0 = Snap {
            ts: 1000,
            inst: Some("A".into()),
            pids: map(&[(7, 100, 0)]),
            upids: map(&[(7, 100, 0)]),
            sks: Counters::new(),
        };
        assert!(c.ingest(s0).net.is_empty()); // 基线
        let s1 = Snap {
            ts: 2000,
            inst: Some("A".into()),
            pids: map(&[(7, 1100, 0)]), // TCP+UDP 合计增量 1000
            upids: map(&[(7, 300, 0)]), // 其中 UDP 增量 200
            sks: Counters::new(),
        };
        let sample = c.ingest(s1);
        assert_eq!(sample.net.len(), 1);
        assert_eq!(sample.net[0].recv, 1000, "net 应是 TCP+UDP 合计");
        assert_eq!(sample.udp.len(), 1);
        assert_eq!(sample.udp[0].recv, 200, "udp 应只是分项");
    }

    #[test]
    fn socket_deltas_come_out_inode_keyed() {
        let mut c = UdpCollector::new();
        let s0 = Snap {
            ts: 1000,
            inst: Some("A".into()),
            pids: Counters::new(),
            upids: Counters::new(),
            sks: map(&[(1009, 500, 0)]),
        };
        assert!(c.ingest(s0).inodes.is_empty()); // baseline
        let s1 = Snap {
            ts: 2000,
            inst: Some("A".into()),
            pids: Counters::new(),
            upids: Counters::new(),
            sks: map(&[(1009, 1500, 30)]),
        };
        let sample = c.ingest(s1);
        assert_eq!(
            sample.inodes,
            vec![InodeDelta {
                inode: 1009,
                recv: 1000,
                sent: 30,
                dt: 1.0
            }]
        );
        assert!(sample.net.is_empty());
        // the last round stays visible for --dump
        assert_eq!(c.last_inodes, sample.inodes);
    }

    #[test]
    fn writer_switch_rebaselines_instead_of_diffing() {
        let mut c = UdpCollector::new();
        assert!(c.ingest(snap("A", 1000, &[(7, 100, 0)])).net.is_empty());
        // same writer, ordinary growth
        let d = c.ingest(snap("A", 2000, &[(7, 300, 0)])).net;
        assert_eq!(d[0].recv, 200);
        // a second daemon takes over the file: its counters are unrelated
        // (smaller or larger), the collector must re-baseline, not diff
        assert!(c.ingest(snap("B", 3000, &[(7, 10, 0)])).net.is_empty());
        let d = c.ingest(snap("B", 4000, &[(7, 90, 0)])).net;
        assert_eq!(d[0].recv, 80);
        // switching back and forth never emits a delta at the switch
        assert!(c.ingest(snap("A", 5000, &[(7, 100_000, 0)])).net.is_empty());
    }

    #[test]
    fn missing_inst_field_still_diffs_against_itself() {
        let mut c = UdpCollector::new();
        assert!(c.ingest(snap("", 1000, &[(7, 100, 0)])).net.is_empty());
        let d = c.ingest(snap("", 2000, &[(7, 250, 0)])).net;
        assert_eq!(d[0].recv, 150);
    }

    #[test]
    fn first_snapshot_is_baseline_only() {
        let mut c = UdpCollector::new();
        assert!(c.ingest(snap("A", 1000, &[(7, 500, 300)])).net.is_empty());
        let d = c.ingest(snap("A", 2000, &[(7, 900, 300)])).net;
        assert_eq!(
            d,
            vec![UdpDelta {
                pid: 7,
                recv: 400,
                sent: 0,
                dt: 1.0
            }]
        );
    }

    #[test]
    fn gaps_accumulate_into_one_exact_delta() {
        let mut c = UdpCollector::new();
        c.ingest(snap("A", 1000, &[(7, 100, 0)]));
        // three daemon intervals pass between two ltop reads
        let d = c.ingest(snap("A", 4000, &[(7, 700, 30)])).net;
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].recv, 600);
        assert_eq!(d[0].sent, 30);
        assert!((d[0].dt - 3.0).abs() < 1e-9);
    }

    #[test]
    fn counter_resets_and_pid_reuse_clamp_at_zero() {
        let mut c = UdpCollector::new();
        c.ingest(snap("A", 1000, &[(7, 10_000, 0)]));
        // counters went backwards: LRU eviction + new pid with the same number
        assert!(c.ingest(snap("A", 2000, &[(7, 40, 0)])).net.is_empty());
        let d = c.ingest(snap("A", 3000, &[(7, 140, 0)])).net;
        assert_eq!(d[0].recv, 100);
    }

    #[test]
    fn timestamp_regression_rebaselines() {
        let mut c = UdpCollector::new();
        c.ingest(snap("A", 5000, &[(7, 100, 0)]));
        // older ts than the previous read: re-baseline, no garbage deltas
        assert!(c.ingest(snap("A", 4000, &[(7, 999, 0)])).net.is_empty());
        // and the lower new counters become the baseline (clamped, not negative)
        assert!(c.ingest(snap("A", 5000, &[(7, 199, 0)])).net.is_empty());
        let d = c.ingest(snap("A", 6000, &[(7, 299, 0)])).net;
        assert_eq!(d[0].recv, 100);
    }

    /// the whole ltop-side pipeline against a temp file: read → parse →
    /// diff → inode deltas, exactly what --dump and the TUI exercise
    #[test]
    fn sample_pipeline_yields_inode_deltas() {
        let path = std::env::temp_dir().join(format!("ltop-udp-sample-{}", std::process::id()));
        let write = |ts: u64, recv: u64| {
            std::fs::write(
                &path,
                format!(
                    "{{\"inst\":\"t\",\"ts\":{ts},\"pids\":[],\"sks\":[{{\"ino\":1009,\"recv\":{recv},\"sent\":0}}]}}"
                ),
            )
            .unwrap();
        };
        write(1000, 100);
        let mut c = UdpCollector::new();
        c.path = path.clone();
        assert!(c.sample().inodes.is_empty()); // baseline
        write(3000, 500);
        let out = c.sample();
        std::fs::remove_file(&path).ok();
        assert_eq!(out.inodes.len(), 1);
        assert_eq!(out.inodes[0].inode, 1009);
        assert_eq!(out.inodes[0].recv, 400);
    }
}

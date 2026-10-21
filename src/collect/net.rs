use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fs;
use std::time::Instant;

use if_addrs::{get_if_addrs, IfAddr};

const HIST_CAP: usize = 900;

pub struct IfaceInfo {
    pub operstate: String,
    pub speed_mbps: Option<u64>,
    pub duplex: Option<String>,
    pub mtu: Option<u64>,
    pub mac: String,
    pub addrs: Vec<String>,
}

#[derive(Clone, Copy)]
pub struct IfaceRates {
    pub rx: f64,
    pub tx: f64,
    pub rx_total: u64,
    pub tx_total: u64,
}

pub struct NetCollector {
    last_totals: HashMap<String, (u64, u64, Instant)>,
    base_totals: HashMap<String, (u64, u64)>,
    pub ifaces: BTreeMap<String, IfaceInfo>,
    pub rates: HashMap<String, IfaceRates>,
    pub history_rx: HashMap<String, VecDeque<f64>>,
    pub history_tx: HashMap<String, VecDeque<f64>>,
    pub selected: Option<String>,
}

impl NetCollector {
    pub fn new() -> Self {
        Self {
            last_totals: HashMap::new(),
            base_totals: HashMap::new(),
            ifaces: BTreeMap::new(),
            rates: HashMap::new(),
            history_rx: HashMap::new(),
            history_tx: HashMap::new(),
            selected: None,
        }
    }

    pub fn sample(&mut self) {
        let now = Instant::now();
        let entries = parse_proc_net_dev().unwrap_or_default();

        let mut infos = BTreeMap::new();
        for (name, ..) in &entries {
            infos.insert(name.clone(), read_iface_info(name));
        }
        if let Ok(list) = get_if_addrs() {
            for ifa in list {
                let s = match ifa.addr {
                    IfAddr::V4(v4) => {
                        format!("{}/{}", v4.ip, u32::from(v4.netmask).count_ones())
                    }
                    IfAddr::V6(v6) => {
                        let prefix: u32 = v6.netmask.octets().iter().map(|b| b.count_ones()).sum();
                        format!("{}/{}", v6.ip, prefix)
                    }
                };
                if let Some(info) = infos.get_mut(&ifa.name) {
                    info.addrs.push(s);
                }
            }
        }
        for info in infos.values_mut() {
            info.addrs.sort();
        }
        self.ifaces = infos;

        let mut new_last = HashMap::new();
        for (name, rx, tx, ..) in entries.iter().cloned() {
            new_last.insert(name, (rx, tx, now));
        }

        for (name, (rx, tx, _)) in &new_last {
            self.base_totals.entry(name.clone()).or_insert((*rx, *tx));
            let (brx, btx) = self.base_totals[name];
            if let Some(&(prx, ptx, pt)) = self.last_totals.get(name) {
                let dt = now.duration_since(pt).as_secs_f64().max(1e-6);
                let rxb = rx.saturating_sub(prx) as f64 / dt;
                let txb = tx.saturating_sub(ptx) as f64 / dt;
                push_hist(self.history_rx.entry(name.clone()).or_default(), rxb);
                push_hist(self.history_tx.entry(name.clone()).or_default(), txb);
                self.rates.insert(
                    name.clone(),
                    IfaceRates {
                        rx: rxb,
                        tx: txb,
                        rx_total: rx.saturating_sub(brx),
                        tx_total: tx.saturating_sub(btx),
                    },
                );
            }
        }

        // drop state for interfaces that disappeared
        self.last_totals = new_last;
        let live: Vec<String> = self.last_totals.keys().cloned().collect();
        self.rates.retain(|k, _| live.contains(k));
        self.history_rx.retain(|k, _| live.contains(k));
        self.history_tx.retain(|k, _| live.contains(k));

        self.ensure_selected();
    }

    fn ensure_selected(&mut self) {
        let valid = |s: &str, this: &Self| this.rates.contains_key(s);
        if let Some(sel) = &self.selected {
            if valid(sel, self) {
                return;
            }
        }
        let candidates: Vec<String> = self.rates.keys().filter(|k| k.as_str() != "lo").cloned().collect();
        let pick = candidates
            .into_iter()
            .find(|k| Some(k.as_str()) == default_route_iface().as_deref())
            .or_else(|| self.rates.keys().find(|k| k.as_str() != "lo").cloned())
            .or_else(|| self.rates.keys().next().cloned());
        self.selected = pick;
    }

    pub fn cycle_selected(&mut self) {
        let names: Vec<String> = self.rates.keys().filter(|k| k.as_str() != "lo").cloned().collect();
        if names.is_empty() {
            return;
        }
        let idx = self
            .selected
            .as_ref()
            .and_then(|s| names.iter().position(|n| n == s))
            .map_or(0, |i| (i + 1) % names.len());
        self.selected = Some(names[idx].clone());
    }

    pub fn reset_totals(&mut self) {
        for (k, v) in &self.last_totals {
            self.base_totals.insert(k.clone(), (v.0, v.1));
        }
    }

    pub fn selected_rates(&self) -> Option<IfaceRates> {
        self.selected.as_ref().and_then(|s| self.rates.get(s)).copied()
    }

    pub fn selected_hist(&self) -> (Vec<f64>, Vec<f64>) {
        match &self.selected {
            Some(name) => (
                self.history_rx
                    .get(name)
                    .map(|d| d.iter().copied().collect())
                    .unwrap_or_default(),
                self.history_tx
                    .get(name)
                    .map(|d| d.iter().copied().collect())
                    .unwrap_or_default(),
            ),
            None => (Vec::new(), Vec::new()),
        }
    }
}

impl Default for NetCollector {
    fn default() -> Self {
        Self::new()
    }
}

fn push_hist(dq: &mut VecDeque<f64>, v: f64) {
    dq.push_back(v);
    while dq.len() > HIST_CAP {
        dq.pop_front();
    }
}

fn read_iface_info(name: &str) -> IfaceInfo {
    let sys = |attr: &str| {
        fs::read_to_string(format!("/sys/class/net/{name}/{attr}"))
            .ok()
            .map(|s| s.trim().to_string())
    };
    IfaceInfo {
        operstate: sys("operstate").unwrap_or_else(|| "unknown".into()),
        speed_mbps: sys("speed").and_then(|s| s.parse().ok()),
        duplex: sys("duplex"),
        mtu: sys("mtu").and_then(|s| s.parse().ok()),
        mac: sys("address").unwrap_or_default(),
        addrs: Vec::new(),
    }
}

/// (name, rx_bytes, tx_bytes, rx_drop, tx_drop)
type DevCounters = (String, u64, u64, u64, u64);

fn parse_proc_net_dev() -> std::io::Result<Vec<DevCounters>> {
    let content = fs::read_to_string("/proc/net/dev")?;
    let mut out = Vec::new();
    for line in content.lines().skip(2) {
        let Some((name, rest)) = line.split_once(':') else {
            continue;
        };
        let f: Vec<u64> = rest
            .split_whitespace()
            .filter_map(|x| x.parse().ok())
            .collect();
        if f.len() < 12 {
            continue;
        }
        out.push((name.trim().to_string(), f[0], f[8], f[3], f[11]));
    }
    Ok(out)
}

pub fn default_route_iface() -> Option<String> {
    let content = fs::read_to_string("/proc/net/route").ok()?;
    for line in content.lines().skip(1) {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() >= 8 && cols[1] == "00000000" {
            return Some(cols[0].to_string());
        }
    }
    None
}

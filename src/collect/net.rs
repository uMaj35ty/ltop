use std::collections::{BTreeMap, HashMap, VecDeque};
#[cfg(target_os = "linux")]
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
    /// 接口存活以来的原始计数(首次采样;reset 不动——会话 Σ 才可重置)
    boot_totals: HashMap<String, (u64, u64)>,
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
            boot_totals: HashMap::new(),
            ifaces: BTreeMap::new(),
            rates: HashMap::new(),
            history_rx: HashMap::new(),
            history_tx: HashMap::new(),
            selected: None,
        }
    }

    pub fn sample(&mut self) {
        let now = Instant::now();
        let (entries, mut infos) = sample_entries().unwrap_or_default();

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
            self.boot_totals.entry(name.clone()).or_insert((*rx, *tx));
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

    /// 选中接口的 Alive Σ(接口存活以来的累计;与可重置的会话 Σ 并列)
    pub fn selected_alive(&self) -> Option<(u64, u64)> {
        let sel = self.selected.as_ref()?;
        let cur = self.last_totals.get(sel)?;
        let base = self.boot_totals.get(sel)?;
        Some((cur.0.saturating_sub(base.0), cur.1.saturating_sub(base.1)))
    }

    /// 指定接口的 Alive Σ(--json 全接口用)
    pub fn alive_for(&self, name: &str) -> Option<(u64, u64)> {
        let cur = self.last_totals.get(name)?;
        let base = self.boot_totals.get(name)?;
        Some((cur.0.saturating_sub(base.0), cur.1.saturating_sub(base.1)))
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

/// (counters, per-iface info) — 平台分派
#[cfg(target_os = "linux")]
fn sample_entries() -> std::io::Result<(Vec<DevCounters>, BTreeMap<String, IfaceInfo>)> {
    let entries = parse_proc_net_dev()?;
    let mut infos = BTreeMap::new();
    for (name, ..) in &entries {
        infos.insert(name.clone(), read_iface_info(name));
    }
    Ok((entries, infos))
}

/// mac:接口计数走 `netstat -ib`(Apple 二进制不受隐私门控;getifaddrs 的
/// if_data64 对未授权进程是冻结的假数据,实测 98MB 传输纹丝不动),接口
/// 元信息(MAC/MTU/状态/地址)仍走 getifaddrs(不受限)
#[cfg(target_os = "macos")]
fn sample_entries() -> std::io::Result<(Vec<DevCounters>, BTreeMap<String, IfaceInfo>)> {
    let entries = parse_netstat_ib(&netstat_ib_output());
    let (_, mut infos) = sample_link_info()?;
    for info in infos.values_mut() {
        info.addrs.sort();
    }
    Ok((entries, infos))
}

#[cfg(target_os = "macos")]
fn netstat_ib_output() -> String {
    std::process::Command::new("netstat")
        .args(["-ib"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// `netstat -ib` 的 Link 行 → (name, ibytes, obytes, 0, 0)。
/// 列:Name Mtu Network Address Ipkts Ierrs Ibytes Opkts Oerrs Obytes [Coll];
/// 每接口多条(\<Link#N\> 一条 + 每地址一条),计数只认 Link 行
pub fn parse_netstat_ib(out: &str) -> Vec<DevCounters> {
    let mut out_v = Vec::new();
    for line in out.lines().skip(1) {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 10 || !f[2].starts_with("<Link") {
            continue;
        }
        let (Ok(ib), Ok(ob)) = (f[6].parse::<u64>(), f[9].parse::<u64>()) else {
            continue;
        };
        out_v.push((f[0].to_string(), ib, ob, 0, 0));
    }
    out_v
}

/// getifaddrs 只取链路层信息(AF_LINK 记录:MAC/状态/MTU + 地址列表)
#[cfg(target_os = "macos")]
fn sample_link_info() -> std::io::Result<((), BTreeMap<String, IfaceInfo>)> {
    use std::ffi::CStr;

    unsafe {
        let mut ifap: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut ifap) != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut infos: BTreeMap<String, IfaceInfo> = BTreeMap::new();
        let mut counted: Vec<String> = Vec::new();
        let mut p = ifap;
        while !p.is_null() {
            let ifa = &*p;
            let name = CStr::from_ptr(ifa.ifa_name).to_string_lossy().into_owned();
            let entry = infos.entry(name.clone()).or_insert_with(|| IfaceInfo {
                operstate: "unknown".into(),
                speed_mbps: None,
                duplex: None,
                mtu: None,
                mac: String::new(),
                addrs: Vec::new(),
            });
            // 元信息只认 AF_LINK 记录(MAC/状态/MTU);字节计数已改走
            // netstat -ib(if_data64 对未授权进程是冻结的假数据)
            if let Some(dl) = ifa.ifa_addr.cast::<libc::sockaddr_dl>().as_ref() {
                if dl.sdl_family as i32 != libc::AF_LINK {
                    p = ifa.ifa_next;
                    continue;
                }
                if !counted.contains(&name) {
                    counted.push(name.clone());
                    if !ifa.ifa_data.is_null() {
                        let d = &*(ifa.ifa_data as *const libc::if_data64);
                        entry.operstate = if ifa.ifa_flags & (libc::IFF_UP as u32) != 0 {
                            "up".into()
                        } else {
                            "down".into()
                        };
                        entry.mtu = Some(d.ifi_mtu as u64);
                    }
                }
                if entry.mac.is_empty() {
                    let nlen = dl.sdl_nlen as usize;
                    let alen = dl.sdl_alen as usize;
                    let start = nlen.min(dl.sdl_data.len());
                    let end = (start + alen).min(dl.sdl_data.len());
                    entry.mac = dl.sdl_data[start..end]
                        .iter()
                        .map(|b| format!("{:02x}", *b as u8))
                        .collect::<Vec<_>>()
                        .join(":");
                }
            }
            p = ifa.ifa_next;
        }
        libc::freeifaddrs(ifap);
        Ok(((), infos))
    }
}

#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
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

/// mac:默认路由接口(每秒一次 route 子进程,与 iostat/nettop 同模式)
#[cfg(target_os = "macos")]
pub fn default_route_iface() -> Option<String> {
    let out = std::process::Command::new("route")
        .args(["-n", "get", "default"])
        .output()
        .ok()?;
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        if let Some(i) = line.find("interface:") {
            return Some(line[i + "interface:".len()..].trim().to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Air 真机 netstat -ib 原样输出(截选):每接口一条 <Link#N> 行 +
    /// 每地址一行;计数只认 Link 行
    #[test]
    fn parses_netstat_ib_link_rows() {
        let sample = "\
Name       Mtu   Network       Address            Ipkts Ierrs     Ibytes    Opkts Oerrs     Obytes  Coll
en0        1500  <Link#14>   c0:c7:db:f4:1e:8d  5541796     0 3138218733  3941442     0 2567353867     0
en0        1500  umaj35tydem fe80:e::1432:d53b  5541796     - 3138218733  3941442     - 2567353867     -
en0        1500  192.168.121  umaj35tydem        5541796     - 3138218733  3941442     - 2567353867     -
lo0        16384 <Link#2>    <#2>                480123     0 480123456   480123     0 480123456     0
utun7      1380  <Link#41>   <#41>                     0     0           0        3     0           348     0
";
        let mut rows = parse_netstat_ib(sample);
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(rows.len(), 3, "只认 Link 行");
        let en0 = rows.iter().find(|r| r.0 == "en0").unwrap();
        assert_eq!(en0.1, 3_138_218_733);
        assert_eq!(en0.2, 2_567_353_867);
        let utun = rows.iter().find(|r| r.0 == "utun7").unwrap();
        assert_eq!((utun.1, utun.2), (0, 348));
    }

    #[test]
    fn netstat_parse_tolerates_garbage() {
        assert!(parse_netstat_ib("").is_empty());
        assert!(parse_netstat_ib("Name Mtu Network").is_empty());
        // 数值列非数字的行跳过
        assert!(parse_netstat_ib("en0 1500 <Link#1> x a b c d e f g").is_empty());
    }
}

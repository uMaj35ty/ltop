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
    /// 接口连续缺席轮数(宽限淘汰计数)
    missed_rounds: HashMap<String, u32>,
    /// 图表缩放峰值(阻尼:即时跟涨、每样本慢速回落)
    peak_rx: HashMap<String, f64>,
    peak_tx: HashMap<String, f64>,
    pub ifaces: BTreeMap<String, IfaceInfo>,
    pub rates: HashMap<String, IfaceRates>,
    pub history_rx: HashMap<String, VecDeque<f64>>,
    pub history_tx: HashMap<String, VecDeque<f64>>,
    pub selected: Option<String>,
}

/// 接口从本轮采集中消失后,保留其速率/历史/选中的宽限轮数:
/// mac netstat 预算偶发超时、瞬时采集失败都不再清空图表或切换选中
/// (旧逻辑单轮缺席即整组状态清除——图表无端重置的直接来源)
const IFACE_GRACE_ROUNDS: u32 = 20;

/// 图表缩放峰值的衰减系数:单帧尖峰(重连突发等)会把可见窗口的 max
/// 抬高数倍,旧波形瞬间被压扁贴地、直到尖峰滑出窗口才恢复——峰值即时
/// 跟涨、按样本 ×0.97 慢落,尖峰影响约 23 个样本减半
const PEAK_DECAY: f64 = 0.97;

impl NetCollector {
    pub fn new() -> Self {
        Self {
            last_totals: HashMap::new(),
            base_totals: HashMap::new(),
            missed_rounds: HashMap::new(),
            peak_rx: HashMap::new(),
            peak_tx: HashMap::new(),
            ifaces: BTreeMap::new(),
            rates: HashMap::new(),
            history_rx: HashMap::new(),
            history_tx: HashMap::new(),
            selected: None,
        }
    }

    pub fn sample(&mut self) {
        let now = Instant::now();
        // 整轮失败:历史/速率/选中原样保留——旧行为会被 retain 清空,
        // 图表无端重置、alive Σ 归零
        if let Ok((entries, mut infos)) = sample_entries() {
            if let Ok(list) = get_if_addrs() {
                for ifa in list {
                    let s = match ifa.addr {
                        IfAddr::V4(v4) => {
                            format!("{}/{}", v4.ip, u32::from(v4.netmask).count_ones())
                        }
                        IfAddr::V6(v6) => {
                            let prefix: u32 =
                                v6.netmask.octets().iter().map(|b| b.count_ones()).sum();
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
            self.ingest(entries, now);
        }
    }

    /// 一轮采集计数落地。entries 为空(mac netstat 预算内全员超时)视同
    /// 整轮失败:原样保留,不推历史、不清状态
    fn ingest(&mut self, entries: Vec<DevCounters>, now: Instant) {
        if entries.is_empty() {
            return;
        }

        for (name, rx, tx, ..) in entries.iter().cloned() {
            self.base_totals.entry(name.clone()).or_insert((rx, tx));
            let (brx, btx) = self.base_totals[&name];
            if let Some(&(prx, ptx, pt)) = self.last_totals.get(&name) {
                let dt = now.duration_since(pt).as_secs_f64().max(1e-6);
                let rxb = rx.saturating_sub(prx) as f64 / dt;
                let txb = tx.saturating_sub(ptx) as f64 / dt;
                push_hist(self.history_rx.entry(name.clone()).or_default(), rxb);
                push_hist(self.history_tx.entry(name.clone()).or_default(), txb);
                let pr = self.peak_rx.entry(name.clone()).or_insert(0.0);
                *pr = rxb.max(*pr * PEAK_DECAY);
                let pt = self.peak_tx.entry(name.clone()).or_insert(0.0);
                *pt = txb.max(*pt * PEAK_DECAY);
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
            // 本轮计数写回:下一轮差分的基线(缺席轮不动它,重现为跨
            // 缺席期的真实差分)
            self.last_totals.insert(name, (rx, tx, now));
        }

        // 宽限淘汰:本轮出现的清零缺席计数,缺席的 +1;连续超限才淘汰
        // 状态、历史与 base(宽限内的缺席接口速率/图表原样冻结)
        let present: std::collections::HashSet<String> =
            entries.iter().map(|(n, ..)| n.clone()).collect();
        let mut expired: Vec<String> = Vec::new();
        for name in self.last_totals.keys() {
            if present.contains(name) {
                self.missed_rounds.remove(name);
            } else {
                let m = self.missed_rounds.entry(name.clone()).or_insert(0);
                *m += 1;
                if *m > IFACE_GRACE_ROUNDS {
                    expired.push(name.clone());
                }
            }
        }
        for name in &expired {
            self.last_totals.remove(name);
            self.rates.remove(name);
            self.history_rx.remove(name);
            self.history_tx.remove(name);
            self.base_totals.remove(name);
            self.peak_rx.remove(name);
            self.peak_tx.remove(name);
            self.missed_rounds.remove(name);
        }

        self.ensure_selected();
    }

    fn ensure_selected(&mut self) {
        let valid = |s: &str, this: &Self| this.rates.contains_key(s);
        if let Some(sel) = &self.selected {
            if valid(sel, self) {
                return;
            }
        }
        let candidates: Vec<String> = self
            .rates
            .keys()
            .filter(|k| k.as_str() != "lo")
            .cloned()
            .collect();
        let pick = candidates
            .into_iter()
            .find(|k| Some(k.as_str()) == default_route_iface().as_deref())
            .or_else(|| self.rates.keys().find(|k| k.as_str() != "lo").cloned())
            .or_else(|| self.rates.keys().next().cloned());
        self.selected = pick;
    }

    pub fn cycle_selected(&mut self) {
        let names: Vec<String> = self
            .rates
            .keys()
            .filter(|k| k.as_str() != "lo")
            .cloned()
            .collect();
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

    /// 选中接口的 Alive Σ = 当前原始计数器(接口存活以来的累计,不受
    /// reset 影响——基线只在会话 Σ 上相减)
    pub fn selected_alive(&self) -> Option<(u64, u64)> {
        let sel = self.selected.as_ref()?;
        self.last_totals.get(sel).map(|(rx, tx, _)| (*rx, *tx))
    }

    /// 指定接口的 Alive Σ(--json 全接口用)
    pub fn alive_for(&self, name: &str) -> Option<(u64, u64)> {
        self.last_totals.get(name).map(|(rx, tx, _)| (*rx, *tx))
    }

    pub fn selected_rates(&self) -> Option<IfaceRates> {
        self.selected
            .as_ref()
            .and_then(|s| self.rates.get(s))
            .copied()
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

    /// 选中接口的阻尼缩放峰值(rx, tx):图表用它做比例上限,单帧尖峰
    /// 不再把整个窗口的波形压扁
    pub fn selected_peaks(&self) -> (f64, f64) {
        match &self.selected {
            Some(name) => (
                *self.peak_rx.get(name).unwrap_or(&0.0),
                *self.peak_tx.get(name).unwrap_or(&0.0),
            ),
            None => (0.0, 0.0),
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

/// mac:接口计数走 `netstat`(Apple 二进制不受隐私门控;getifaddrs 的
/// if_data64 对未授权进程是冻结的假数据,实测 98MB 传输纹丝不动),接口
/// 元信息(MAC/MTU/状态/地址)仍走 getifaddrs(不受限)。
/// 查询粒度=逐接口 `netstat -I <if> -b`:macOS 27 实测全接口枚举(`-ib`)
/// 会卡在个别 P2P 链路(awdl0 5-18s)拖死整个 tick,而单接口查询 0.00s。
#[cfg(target_os = "macos")]
fn sample_entries() -> std::io::Result<(Vec<DevCounters>, BTreeMap<String, IfaceInfo>)> {
    let (_, mut infos) = sample_link_info()?;
    for info in infos.values_mut() {
        info.addrs.sort();
    }
    // P2P 侧链整体不进 NET 盒:查询会阻塞(见 NETSTAT_SLOW_IFACES),
    // 留着也只能显示全 0 行造成困惑
    for name in NETSTAT_SLOW_IFACES {
        infos.remove(name);
    }
    // 27 起 getifaddrs AF_LINK 的 MAC 对未签名进程脱敏(恒 02:00:00:00:00:00),
    // launchd 快照(ltop lan-setup 安装)里有 ifconfig(Apple 签名)的真值则覆盖
    if let Some(ifmacs) = crate::collect::lan_snapshot::load_if_macs() {
        for (name, mac) in ifmacs {
            if let Some(info) = infos.get_mut(&name) {
                info.mac = mac;
            }
        }
    }
    let entries = parse_netstat_ib(&netstat_counters_output(infos.keys()));
    Ok((entries, infos))
}

/// 已知会阻塞查询的 P2P 链路(AirDrop/低功耗直连):单接口查询也卡 5s+,
/// 字节计数不入 NET 口径(NET=NIC 与 PROCS=payload 的差异注记已覆盖)
#[cfg(target_os = "macos")]
const NETSTAT_SLOW_IFACES: [&str; 2] = ["awdl0", "llw0"];

/// 逐接口并行 `netstat -I <if> -b`,500ms 总超时:超时/掉队线程的结果
/// 丢弃(该接口本轮无计数,下轮重查),tick 永不被单接口拖住。
/// 输出直接拼接:每段自带表头行,解析器的 `<Link` 过滤会安全跳过。
#[cfg(target_os = "macos")]
fn netstat_counters_output<'a>(ifaces: impl Iterator<Item = &'a String>) -> String {
    use std::sync::mpsc;
    use std::sync::Arc;
    use std::time::Duration;

    // 仍未完成的 netstat 子进程 pid:超时后统一 SIGKILL,否则卡死的接口
    // 每 tick 泄漏一个线程+子进程,长会话无上界
    let pending: Arc<std::sync::Mutex<Vec<i32>>> = Arc::default();
    let (tx, rx) = mpsc::channel();
    let mut n = 0usize;
    for name in ifaces {
        if NETSTAT_SLOW_IFACES.contains(&name.as_str()) {
            continue;
        }
        let tx = tx.clone();
        let name = name.clone();
        let pending = pending.clone();
        std::thread::spawn(move || {
            let Ok(child) = std::process::Command::new("netstat")
                .args(["-I", &name, "-b"])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
            else {
                let _ = tx.send(String::new());
                return;
            };
            let pid = child.id() as i32;
            pending.lock().unwrap().push(pid);
            let out = child
                .wait_with_output()
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                .unwrap_or_default();
            pending.lock().unwrap().retain(|&p| p != pid);
            let _ = tx.send(out);
        });
        n += 1;
    }
    drop(tx);
    let deadline = Instant::now() + Duration::from_millis(500);
    let mut buf = String::new();
    let mut timed_out = false;
    for _ in 0..n {
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(out) => buf.push_str(&out),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                timed_out = true;
                break;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    if timed_out {
        for pid in pending.lock().unwrap().drain(..) {
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
        }
    }
    buf
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

    fn dev(name: &str, rx: u64, tx: u64) -> DevCounters {
        (name.to_string(), rx, tx, 0, 0)
    }

    /// 空轮(mac netstat 预算内全员超时)保留全部状态:不推历史、不清
    /// 速率与选中——旧行为 retain 空 live 会把图表整个清掉
    #[test]
    fn empty_round_keeps_everything() {
        let mut n = NetCollector::new();
        n.ingest(vec![dev("en0", 1000, 2000)], Instant::now());
        n.ingest(vec![dev("en0", 2000, 2500)], Instant::now());
        let len = n.history_rx.get("en0").unwrap().len();
        assert_eq!(len, 1);
        n.ingest(vec![], Instant::now());
        assert_eq!(
            n.history_rx.get("en0").unwrap().len(),
            len,
            "空轮不得推进或清空历史"
        );
        assert!(n.rates.contains_key("en0"), "空轮不得清速率");
        assert_eq!(n.selected.as_deref(), Some("en0"), "空轮不得换选中");
    }

    /// 宽限内的缺席:状态冻结不丢;重现后差分跨缺席期继续(恰好多一行)
    #[test]
    fn transient_miss_freezes_then_resumes() {
        let mut n = NetCollector::new();
        n.ingest(vec![dev("en0", 0, 0)], Instant::now());
        for i in 1..=3u64 {
            n.ingest(vec![dev("en0", i * 1000, 0)], Instant::now());
        }
        assert_eq!(n.history_rx.get("en0").unwrap().len(), 3);
        // en0 缺席 3 轮(只有 en1 在场):宽限内历史/速率/选中全保留
        for _ in 0..3 {
            n.ingest(vec![dev("en1", 0, 0)], Instant::now());
        }
        assert_eq!(n.history_rx.get("en0").unwrap().len(), 3, "缺席不清历史");
        assert!(n.rates.contains_key("en0"), "缺席不清速率");
        assert_eq!(n.selected.as_deref(), Some("en0"), "缺席不换选中");
        // 重现:差分跨缺席期,恰好多一行
        n.ingest(
            vec![dev("en1", 0, 0), dev("en0", 10_000, 0)],
            Instant::now(),
        );
        assert_eq!(n.history_rx.get("en0").unwrap().len(), 4);
        assert_eq!(n.missed_rounds.get("en0"), None, "重现清缺席计数");
    }

    /// 连续缺席超宽限才淘汰:状态、历史与 base_totals 一并清(顺带修
    /// base 从不修剪的旧账)
    #[test]
    fn sustained_absence_expires_after_grace() {
        let mut n = NetCollector::new();
        n.ingest(vec![dev("en0", 0, 0)], Instant::now());
        for i in 1..=3u64 {
            n.ingest(vec![dev("en0", i * 1000, 0)], Instant::now());
        }
        assert!(n.base_totals.contains_key("en0"));
        for _ in 0..(IFACE_GRACE_ROUNDS + 1) {
            n.ingest(vec![dev("en1", 0, 0)], Instant::now());
        }
        assert!(!n.history_rx.contains_key("en0"), "超宽限淘汰历史");
        assert!(!n.rates.contains_key("en0"));
        assert!(!n.last_totals.contains_key("en0"));
        assert!(!n.base_totals.contains_key("en0"), "base 一并修剪");
        assert_eq!(n.selected.as_deref(), Some("en1"), "淘汰后重选在场接口");
    }

    /// 阻尼缩放峰值:尖峰即时跟涨(图表比例立刻让位),安静后按样本衰减
    /// 回落——单帧尖峰不再把整窗波形压扁几十秒
    #[test]
    fn scale_peak_rises_fast_decays_slow() {
        let mut n = NetCollector::new();
        n.ingest(vec![dev("en0", 0, 0)], Instant::now());
        n.ingest(vec![dev("en0", 1_000_000, 0)], Instant::now());
        let spike = n.selected_peaks().0;
        assert!(spike > 0.0, "尖峰即时入峰");
        // 尖峰后 60 轮安静:峰值应显著回落(0.97^60 ≈ 0.16)
        for _ in 0..60 {
            n.ingest(vec![dev("en0", 1_001_000, 0)], Instant::now());
        }
        let p = n.selected_peaks().0;
        assert!(p < spike * 0.3, "峰值应回落: spike={spike} now={p}");
        assert!(p > 0.0);
    }
}

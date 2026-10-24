//! macOS 27 邻居/网卡 MAC 门控的退避数据源。
//!
//! `ltop lan-setup` 安装常驻用户 LaunchAgent(io.github.umaj35ty.ltop.lan),
//! 每 30s 用 Apple 平台签名工具轮转两份快照到 state 目录([`crate::log_dir`]):
//! - lan-arp.txt = `/usr/sbin/arp -a -n`(邻居 IP→MAC)
//! - lan-if.txt  = `/sbin/ifconfig`(本机网卡 ether MAC;27 起 getifaddrs
//!   对未签名进程脱敏成 02:00:00:00:00:00)
//!
//! ltop 只读文件,零特权、零门控。判别器实测(2026-10-24,Air 27.0.1):
//! 门控 = 责任进程归因 × 平台签名双维度 —— 未签名二进制任何上下文都是
//! 空表;Apple 签名 arp 从 app 归因(终端)直跑也被蒙;经 launchd 拉起
//! 则归因回系统域,真值恢复。快照缺失/超龄 → 调用方回退进程内路径。
//!
//! 解析器保持跨平台纯函数:mac 专属代码只有 mac 机器能测,纯逻辑放这里
//! 让 Linux 侧 CI/本地一样跑单测(教训:平台专属测试面越小越好)。

use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime};

/// 快照最大容忍陈旧:4 × 30s agent 周期(睡眠唤醒后要一两拍才续上)
const STALE_AFTER: Duration = Duration::from_secs(120);
/// 防御性解析上限(正常 ARP 表几十至几百行)
const MAX_LINES: usize = 4096;
/// 快照文件大小上限:损坏文件(或被塞垃圾)也不至于整读进内存
const MAX_SNAPSHOT_BYTES: u64 = 1 << 20;

pub const ARP_FILE: &str = "lan-arp.txt";
pub const IF_FILE: &str = "lan-if.txt";

/// 邻居快照条目:(ip, mac, iface);只收完整条目,mac 已归一化小写补零
pub type ArpEntries = Vec<(Ipv4Addr, String, String)>;

/// `arp -a -n` 文本 → 条目列表
pub fn parse_arp_entries(text: &str) -> ArpEntries {
    let mut out = Vec::new();
    for line in text.lines().take(MAX_LINES) {
        if let Some(e) = parse_arp_line(line) {
            out.push(e);
        }
    }
    out
}

/// `? (192.168.121.1) at c0:4a:9:30:d3:af on en0 ifscope [ethernet]`
pub fn parse_arp_line(line: &str) -> Option<(Ipv4Addr, String, String)> {
    let lp = line.find('(')?;
    let rp = line.find(')')?;
    if rp < lp + 1 {
        return None;
    }
    let ip: Ipv4Addr = line[lp + 1..rp].trim().parse().ok()?;
    // 组播/广播条目(IGMP、mDNS 组、255.255.255.255)与进程内路径同滤
    if ip.is_multicast() || ip.is_broadcast() {
        return None;
    }
    let rest = line[rp + 1..]
        .trim_start()
        .strip_prefix("at ")?
        .trim_start();
    let mac_end = rest.find(" on ")?;
    let mac = normalize_mac(rest[..mac_end].trim())?;
    let iface = rest[mac_end + 4..].split_whitespace().next()?.to_string();
    if iface.is_empty() {
        return None;
    }
    Some((ip, mac, iface))
}

/// MAC 字段归一化:1-2 位十六进制 × 6(macOS arp 会缺前导零),
/// 补零小写;全 0 / 全 f(未解析、广播)→ None
fn normalize_mac(raw: &str) -> Option<String> {
    let parts: Vec<&str> = raw.split(':').collect();
    if parts.len() != 6 {
        return None;
    }
    let mut octs = [0u8; 6];
    for (i, p) in parts.iter().enumerate() {
        if p.is_empty() || p.len() > 2 || !p.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        octs[i] = u8::from_str_radix(p, 16).ok()?;
    }
    if octs == [0; 6] || octs == [0xff; 6] {
        return None;
    }
    Some(
        octs.iter()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(":"),
    )
}

/// ifconfig 文本 → 接口名 → ether MAC。
/// 块首=`en0: flags=...`(零缩进行),`ether` 行缩进;`lladdr` 等干扰忽略
pub fn parse_if_macs(text: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut cur: Option<String> = None;
    for line in text.lines().take(MAX_LINES) {
        if !line.starts_with([' ', '\t']) {
            cur = line.split(':').next().map(|s| s.trim().to_string());
            continue;
        }
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix("ether ") {
            let mac = normalize_mac(rest.split_whitespace().next().unwrap_or(""));
            if let (Some(name), Some(mac)) = (cur.as_deref(), mac) {
                out.insert(name.to_string(), mac);
            }
        }
    }
    out
}

// ---------- 载入(mtime 缓存:每秒多调用点共享一次解析) ----------

struct FileCache<T> {
    mtime: SystemTime,
    val: Arc<T>,
}

/// 接口名 → ether MAC
pub type IfMacs = HashMap<String, String>;

static ARP_CACHE: OnceLock<Mutex<Option<FileCache<ArpEntries>>>> = OnceLock::new();
static IF_CACHE: OnceLock<Mutex<Option<FileCache<IfMacs>>>> = OnceLock::new();

fn cached<T: Send + Sync + 'static>(
    cache: &OnceLock<Mutex<Option<FileCache<T>>>>,
    path: &Path,
    parse: fn(&str) -> T,
) -> Option<Arc<T>> {
    let md = std::fs::metadata(path).ok()?;
    if md.len() > MAX_SNAPSHOT_BYTES {
        return None;
    }
    let mtime = md.modified().ok()?;
    if SystemTime::now()
        .duration_since(mtime)
        .ok()?
        .gt(&STALE_AFTER)
    {
        return None;
    }
    let cell = cache.get_or_init(|| Mutex::new(None));
    let mut guard = cell.lock().ok()?;
    if let Some(c) = guard.as_ref() {
        if c.mtime == mtime {
            return Some(Arc::clone(&c.val));
        }
    }
    let val = Arc::new(parse(&std::fs::read_to_string(path).ok()?));
    *guard = Some(FileCache {
        mtime,
        val: Arc::clone(&val),
    });
    Some(val)
}

fn load_arp_from(dir: &Path) -> Option<ArpEntries> {
    let path = dir.join(ARP_FILE);
    cached(&ARP_CACHE, &path, parse_arp_entries).map(|x| (*x).clone())
}

fn load_if_from(dir: &Path) -> Option<IfMacs> {
    let path = dir.join(IF_FILE);
    cached(&IF_CACHE, &path, parse_if_macs).map(|x| (*x).clone())
}

/// 邻居快照(缺失/超龄 → None,调用方回退进程内路径)
pub fn load_arp_entries() -> Option<ArpEntries> {
    load_arp_from(&crate::log::log_dir())
}

/// 本机网卡 MAC 快照(接口名 → MAC)
pub fn load_if_macs() -> Option<IfMacs> {
    load_if_from(&crate::log::log_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arp_lines_parse_and_filter() {
        let e = parse_arp_line("? (192.168.121.1) at c0:4a:9:30:d3:af on en0 ifscope [ethernet]")
            .unwrap();
        assert_eq!(e.0.to_string(), "192.168.121.1");
        assert_eq!(e.1, "c0:4a:09:30:d3:af", "缺前导零要补齐");
        assert_eq!(e.2, "en0");
        let e = parse_arp_line("? (169.254.84.246) at a2:c1:c5:d:37:68 on en6 [ethernet]").unwrap();
        assert_eq!(e.1, "a2:c1:c5:0d:37:68");
        assert_eq!(e.2, "en6", "无 ifscope 变体");
        assert!(
            parse_arp_line("? (192.168.121.2) at (incomplete) on en0 ifscope [ethernet]").is_none()
        );
        assert!(parse_arp_line(
            "? (192.168.121.255) at ff:ff:ff:ff:ff:ff on en0 ifscope [ethernet]"
        )
        .is_none());
        assert!(
            parse_arp_line("? (224.0.0.251) at 1:0:5e:0:0:fb on en0 ifscope [ethernet]").is_none()
        );
        assert!(parse_arp_line("").is_none());
        assert!(parse_arp_line("garbage line").is_none());
        assert!(parse_arp_line("? (not.an.ip) at c0:4a:9:30:d3:af on en0").is_none());
    }

    #[test]
    fn arp_entries_take_cap_and_skip_garbage() {
        let text = "? (10.0.0.1) at a:b:c:d:e:f on en0 ifscope [ethernet]\nnonsense\n";
        let e = parse_arp_entries(text);
        assert_eq!(
            e,
            vec![(
                "10.0.0.1".parse().unwrap(),
                "0a:0b:0c:0d:0e:0f".into(),
                "en0".into()
            )]
        );
    }

    #[test]
    fn ifmacs_parse_blocks() {
        let text = "en0: flags=8863<UP,BROADCAST,SMART,RUNNING,SIMPLEX> mtu 1500\n\
                    \tether c0:c7:db:f4:1e:8d\n\
                    \tinet6 fe80::1%en0 prefixlen 64\n\
                    ap1: flags=8802<BROADCAST,SIMPLEX> mtu 1500\n\
                    \tstatus: inactive\n\
                    en6: flags=2863<UP,SMART> mtu 1500\n\
                    \tlladdr 1c:57:dc:b:83:26\n\
                    \tether 1c:57:dc:b:83:26\n";
        let m = parse_if_macs(text);
        assert_eq!(m.get("en0").map(String::as_str), Some("c0:c7:db:f4:1e:8d"));
        assert!(!m.contains_key("ap1"), "无 ether 行的块不进");
        assert_eq!(m.get("en6").map(String::as_str), Some("1c:57:dc:0b:83:26"));
    }

    fn backdate(p: &Path, secs_ago: u64) {
        use std::fs::FileTimes;
        let f = std::fs::OpenOptions::new().append(true).open(p).unwrap();
        let t = SystemTime::now() - Duration::from_secs(secs_ago);
        f.set_times(FileTimes::new().set_modified(t)).unwrap();
    }

    #[test]
    fn snapshot_staleness_gates_loader() {
        let dir = std::env::temp_dir().join(format!(
            "ltop-lansnap-{}-{:p}",
            std::process::id(),
            &ARP_CACHE
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let arp = dir.join(ARP_FILE);
        std::fs::write(
            &arp,
            "? (10.0.0.1) at a0:b1:c2:d3:e4:5 on en0 ifscope [ethernet]\n\
             ? (10.0.0.2) at (incomplete) on en0 ifscope [ethernet]\n",
        )
        .unwrap();
        backdate(&arp, 0);
        let e = load_arp_from(&dir).unwrap();
        assert_eq!(e.len(), 1, "incomplete 不进");
        assert_eq!(e[0].1, "a0:b1:c2:d3:e4:05");
        backdate(&arp, 200);
        assert!(load_arp_from(&dir).is_none(), "超龄快照必须被拒");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_snapshot_is_none_not_panic() {
        let dir = std::env::temp_dir().join(format!(
            "ltop-lansnap-missing-{}-{:p}",
            std::process::id(),
            &IF_CACHE
        ));
        assert!(load_arp_from(&dir).is_none());
        assert!(load_if_from(&dir).is_none());
    }
}

//! PF_ROUTE RTM_GET probes — the neighbour-table stand-in for macOS.
//!
//! macOS 26+ privacy gates hide the neighbour table from binaries that are
//! not signed by Apple: bulk enumeration (sysctl NET_RT_FLAGS) answers an
//! empty table, and even single-entry queries return the MAC sanitized to
//! the interface's synthetic address (both verified against the same entry
//! in the same second, where an Apple-signed process sees the real MAC).
//! Route-socket RTM_GET queries, however, answer fine — entry existence and
//! the owning interface are real, so probing the subnet entry-by-entry
//! rebuilds a neighbour list without MACs (shown as "—" in the UI).
//!
//! The message construction/parsing is platform-independent and unit-tested
//! on Linux too; only `probe()` itself (a PF_ROUTE socket) is macOS-gated.

#![allow(dead_code)] // 消息构造/解析跨平台测试;probe 仅 mac 数据路径

use std::net::Ipv4Addr;

// struct rt_msghdr offsets (darwin LP64, C-probe pinned)
const RTM_VERSION_OFF: usize = 2;
const RTM_TYPE_OFF: usize = 3;
const RTM_FLAGS_OFF: usize = 8; // int
const RTM_ADDRS_OFF: usize = 12; // int
const RTM_PID_OFF: usize = 16;
const RTM_SEQ_OFF: usize = 20;
const RTM_ERRNO_OFF: usize = 24;
// darwin 的 rt_msghdr 是 92 字节(pid_t=int、紧凑布局;C probe 实测
// write rc=92+16=108),FreeBSD 手册的直觉尺寸 144 不适用
const RTM_HDR_LEN: usize = 92;

const RTM_VERSION: u8 = 5;
const RTM_GET: u8 = 4;
const RTF_UP: i32 = 0x1;
const RTF_GATEWAY: i32 = 0x2;
const RTF_HOST: i32 = 0x4;
const RTF_LLINFO: i32 = 0x400;
const RTA_DST: i32 = 0x1;
const RTA_GATEWAY: i32 = 0x2;
const AF_INET_DARWIN: u8 = 2;
const AF_LINK_DARWIN: u8 = 18;

/// one probe result: the kernel knows this IP's neighbour entry, and the
/// route lives on this interface index
#[derive(Debug, PartialEq)]
pub struct LlEntry {
    pub if_index: u16,
}

/// build an RTM_GET request for `ip` (pure, cross-platform tested)
pub fn build_get(ip: Ipv4Addr, seq: u32, pid: u32) -> Vec<u8> {
    let mut m = vec![0u8; RTM_HDR_LEN + 16];
    let total = m.len() as u16;
    m[0..2].copy_from_slice(&total.to_ne_bytes()); // rtm_msglen
    m[RTM_VERSION_OFF] = RTM_VERSION;
    m[RTM_TYPE_OFF] = RTM_GET;
    m[RTM_FLAGS_OFF..RTM_FLAGS_OFF + 4]
        .copy_from_slice(&(RTF_UP | RTF_HOST | RTF_GATEWAY).to_ne_bytes());
    m[RTM_ADDRS_OFF..RTM_ADDRS_OFF + 4].copy_from_slice(&RTA_DST.to_ne_bytes());
    m[RTM_PID_OFF..RTM_PID_OFF + 4].copy_from_slice(&pid.to_ne_bytes());
    m[RTM_SEQ_OFF..RTM_SEQ_OFF + 4].copy_from_slice(&seq.to_ne_bytes());
    // sockaddr_in @ header end
    m[RTM_HDR_LEN] = 16;
    m[RTM_HDR_LEN + 1] = AF_INET_DARWIN;
    let [a, b, c, d] = ip.octets();
    m[RTM_HDR_LEN + 4..RTM_HDR_LEN + 8].copy_from_slice(&[a, b, c, d]);
    m
}

/// parse an RTM_GET reply (pure, cross-platform tested). Returns the owning
/// interface index and whether the entry carries a link-layer address
/// (sdl_alen == 6). Two traps verified on hardware: RTM_GET *clones* a host
/// route for any in-subnet IP (so RTF_LLINFO is always set and useless as a
/// discriminator), and the MAC itself is sanitized — alen is the only truth.
pub fn parse_reply(buf: &[u8], seq: u32, pid: u32) -> Option<(u16, bool)> {
    if buf.len() < RTM_HDR_LEN || buf[RTM_TYPE_OFF] != RTM_GET {
        return None;
    }
    let rseq = u32_at(buf, RTM_SEQ_OFF)?;
    let rpid = u32_at(buf, RTM_PID_OFF)?;
    if rseq != seq || rpid != pid {
        return None; // 路由 socket 上的广播消息,不是本请求的应答
    }
    let errno = i32_at(buf, RTM_ERRNO_OFF)?;
    if errno != 0 {
        return None;
    }
    let _flags = i32_at(buf, RTM_FLAGS_OFF)?;
    let mut addrs = i32_at(buf, RTM_ADDRS_OFF)?;
    // 步进 sockaddr 数组,RTA_GATEWAY(位 1)= link 层应答,取 sdl_index
    let mut off = RTM_HDR_LEN;
    let mut gw_if: Option<u16> = None;
    let mut alen: u8 = 0;
    for i in 0..8 {
        if addrs & 1 == 0 {
            addrs >>= 1;
            continue;
        }
        addrs >>= 1;
        if off + 4 > buf.len() {
            break;
        }
        let len = buf[off] as usize;
        if len == 0 {
            break;
        }
        if i == 1 && buf[off + 1] == AF_LINK_DARWIN {
            gw_if = Some(u16::from_ne_bytes([buf[off + 2], buf[off + 3]])); // sdl_index
            alen = buf.get(off + 6).copied().unwrap_or(0); // sdl_alen
        }
        off += (len + 3) & !3;
    }
    gw_if.map(|if_index| (if_index, alen == 6))
}

/// 一次 RTM_GET 探测:内核存在该 IP 的邻居条目时返回接口索引
#[cfg(target_os = "macos")]
pub fn probe(ip: Ipv4Addr, seq: u32, pid: u32) -> Option<LlEntry> {
    // SAFETY: 路由 socket 生命周期内所有缓冲区大小固定;fd 全路径关闭
    let fd = unsafe { libc::socket(libc::PF_ROUTE, libc::SOCK_RAW, 0) };
    if fd < 0 {
        return None;
    }
    let result = (|| {
        let tv = libc::timeval {
            tv_sec: 1,
            tv_usec: 0,
        };
        if unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_RCVTIMEO,
                &tv as *const libc::timeval as *const libc::c_void,
                std::mem::size_of::<libc::timeval>() as libc::socklen_t,
            )
        } != 0
        {
            return None;
        }
        let msg = build_get(ip, seq, pid);
        let w = unsafe { libc::write(fd, msg.as_ptr() as *const libc::c_void, msg.len()) };
        if w < 0 {
            return None;
        }
        let mut buf = [0u8; 512];
        // 迭代上限:路由广播风暴下 SO_RCVTIMEO 只在包间生效,不设上限可
        // 让本函数长时间空转
        for _ in 0..64 {
            let n = unsafe { libc::recv(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len(), 0) };
            if n <= 0 {
                return None; // 超时/错误:视为无条目
            }
            if let Some((if_index, has_link_addr)) = parse_reply(&buf[..n as usize], seq, pid) {
                // alen==6 = 内核有真实链路层条目(MAC 值已脱敏);空克隆路由不算
                return has_link_addr.then_some(LlEntry { if_index });
            }
        }
        None
    })();
    unsafe { libc::close(fd) };
    result
}

fn u32_at(b: &[u8], off: usize) -> Option<u32> {
    let s = b.get(off..off + 4)?;
    Some(u32::from_ne_bytes(s.try_into().ok()?))
}

fn i32_at(b: &[u8], off: usize) -> Option<i32> {
    let s = b.get(off..off + 4)?;
    Some(i32::from_ne_bytes(s.try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(seq: u32, pid: u32, errno: i32, flags: i32, with_gw: bool) -> Vec<u8> {
        let mut b = vec![0u8; RTM_HDR_LEN + 16 + 20];
        b[RTM_TYPE_OFF] = RTM_GET;
        b[RTM_FLAGS_OFF..RTM_FLAGS_OFF + 4].copy_from_slice(&flags.to_ne_bytes());
        b[RTM_SEQ_OFF..RTM_SEQ_OFF + 4].copy_from_slice(&seq.to_ne_bytes());
        b[RTM_PID_OFF..RTM_PID_OFF + 4].copy_from_slice(&pid.to_ne_bytes());
        b[RTM_ERRNO_OFF..RTM_ERRNO_OFF + 4].copy_from_slice(&errno.to_ne_bytes());
        let addrs = if with_gw {
            RTA_DST | RTA_GATEWAY
        } else {
            RTA_DST
        };
        b[RTM_ADDRS_OFF..RTM_ADDRS_OFF + 4].copy_from_slice(&addrs.to_ne_bytes());
        // dst sockaddr_in @144(len=16 family=AF_INET)
        b[RTM_HDR_LEN] = 16;
        b[RTM_HDR_LEN + 1] = AF_INET_DARWIN;
        if with_gw {
            // 对齐 16 后,sockaddr_dl @160(len=20 family=18 index=14 nlen=0 alen=6)
            let sdl = RTM_HDR_LEN + 16;
            b[sdl] = 20;
            b[sdl + 1] = AF_LINK_DARWIN;
            b[sdl + 2..sdl + 4].copy_from_slice(&14u16.to_ne_bytes());
            b[sdl + 6] = 6;
        }
        b
    }

    #[test]
    fn build_get_carries_dst_and_header() {
        let m = build_get("192.168.121.1".parse().unwrap(), 7, 42);
        assert_eq!(&m[0..2], &(m.len() as u16).to_ne_bytes());
        assert_eq!(m[RTM_TYPE_OFF], RTM_GET);
        assert_eq!(&m[RTM_SEQ_OFF..RTM_SEQ_OFF + 4], &7u32.to_ne_bytes());
        assert_eq!(&m[RTM_PID_OFF..RTM_PID_OFF + 4], &42u32.to_ne_bytes());
        assert_eq!(m[RTM_HDR_LEN + 1], AF_INET_DARWIN);
        assert_eq!(&m[RTM_HDR_LEN + 4..RTM_HDR_LEN + 8], &[192, 168, 121, 1]);
    }

    #[test]
    fn parse_reply_reads_llinfo_entry_and_interface() {
        let (if_index, llinfo) = parse_reply(
            &reply(7, 42, 0, RTF_UP | RTF_HOST | RTF_LLINFO, true),
            7,
            42,
        )
        .unwrap();
        assert_eq!(if_index, 14);
        assert!(llinfo, "LLINFO 条目才算邻居");
    }

    #[test]
    fn parse_reply_flags_zero_alen_as_no_neighbour() {
        // RTM_GET 会为任意子网 IP 克隆 host 路由(LLINFO 恒置,不能判别);
        // alen==0 = 空克隆(无邻居),alen==6 = 真实链路层条目(真机实测)
        let mut no_addr = reply(7, 42, 0, RTF_UP | RTF_HOST | RTF_LLINFO, true);
        no_addr[92 + 16 + 6] = 0; // sdl_alen = 0
        let (_, has) = parse_reply(&no_addr, 7, 42).unwrap();
        assert!(!has);
    }

    #[test]
    fn parse_reply_rejects_mismatch_and_errors() {
        assert!(
            parse_reply(&reply(8, 42, 0, 0, true), 7, 42).is_none(),
            "seq 不匹配"
        );
        assert!(
            parse_reply(&reply(7, 43, 0, 0, true), 7, 42).is_none(),
            "pid 不匹配"
        );
        assert!(
            parse_reply(&reply(7, 42, 65 /*ESRCH*/, 0, true), 7, 42).is_none(),
            "errno 非零"
        );
        // 无 GATEWAY sockaddr → 无法归属接口
        assert!(parse_reply(&reply(7, 42, 0, RTF_LLINFO, false), 7, 42).is_none());
    }
}

//! ltop-bpf: privileged companion daemon for ltop.
//!
//! Attaches kprobes on udp_sendmsg / udp_recvmsg (+ udpv6 variants), keeps
//! per-pid byte counters in a kernel LRU map and writes a snapshot of
//! cumulative counters to /run/ltop/udp.json once per interval. The
//! unprivileged TUI only reads that file. Needs root (or CAP_BPF +
//! CAP_PERFMON); exit status 1 with a reason when nothing can be attached.
//!
//! usage: ltop-bpf [--interval MS] [--out PATH] [--once]
//!
//! Links are not pinned on bpffs: everything unloads automatically when the
//! daemon exits, so a crashed run can never leave double-attached probes
//! behind. Counter continuity across daemon restarts is sacrificed for that
//! safety (the TUI re-baselines on its next read).

#[cfg(target_os = "linux")]
mod imp {
    use std::fs;
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::io::AsRawFd;
    use std::path::PathBuf;
    use std::process::exit;
    use std::time::Duration;

    use anyhow::{bail, Result};
    use aya::{maps::HashMap, programs::KProbe, Ebpf, Pod};

    include!(concat!(env!("OUT_DIR"), "/bpf_object.rs"));

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct UdpVal {
        recv: u64,
        sent: u64,
    }
    unsafe impl Pod for UdpVal {}

    /// (program name in the object, kernel symbol, is kretprobe)
    const PROBES: [(&str, &str, bool); 10] = [
        ("udp_send", "udp_sendmsg", false),
        ("udp_recv_entry", "udp_recvmsg", false),
        ("udp_recv", "udp_recvmsg", true),
        ("udp6_send", "udpv6_sendmsg", false),
        ("udp6_recv_entry", "udpv6_recvmsg", false),
        ("udp6_recv", "udpv6_recvmsg", true),
        ("tcp_send_entry", "tcp_sendmsg", false),
        ("tcp_send", "tcp_sendmsg", true),
        ("tcp_recv_entry", "tcp_recvmsg", false),
        ("tcp_recv", "tcp_recvmsg", true),
    ];

    pub fn entry() {
        // 同 ltop:管道截断(--once | head)时静默退出而非 panic
        unsafe {
            libc::signal(libc::SIGPIPE, libc::SIG_DFL);
        }
        if let Err(e) = run() {
            eprintln!("ltop-bpf: {e:#}");
            exit(1);
        }
    }

    fn run() -> Result<()> {
        // 探针 C 代码的 PT_REGS 寄存器映射按 x86_64 编写,其他架构直接拒绝
        if !matches!(std::env::consts::ARCH, "x86_64" | "aarch64") {
            anyhow::bail!(
                "ltop-bpf supports x86_64/aarch64 Linux only (running on {})",
                std::env::consts::ARCH
            );
        }
        let mut args = std::env::args().skip(1);
        let mut interval_ms = 1000u64;
        let mut out = PathBuf::from("/run/ltop/udp.json");
        let mut once = false;
        while let Some(a) = args.next() {
            match a.as_str() {
                "--interval" => {
                    interval_ms = args.next().and_then(|v| v.parse().ok()).unwrap_or(1000);
                }
                "--out" => {
                    if let Some(p) = args.next() {
                        out = PathBuf::from(p);
                    }
                }
                "--once" => once = true,
                "--help" | "-h" => {
                    println!(
                        "ltop-bpf v{} — privileged eBPF accounting helper for ltop

USAGE:
    ltop-bpf [OPTIONS]

    --interval <MS>   snapshot interval in ms (default 1000, min 100)
    --out <PATH>      snapshot output path (default /run/ltop/udp.json)
    --once            sample once, print, exit (smoke test)
    -h, --help        print this help

Run as root (systemd unit: deploy/ltop-bpf.service).",
                        env!("CARGO_PKG_VERSION")
                    );
                    return Ok(());
                }
                other => bail!("未知参数 {other}(见 --help)"),
            }
        }
        let interval = Duration::from_millis(interval_ms.max(100));

        if let Some(dir) = out.parent() {
            fs::create_dir_all(dir)?;
        }

        // single-instance guard —— 必须在 load/attach 之前:锁在挂载后的话,
        // 并发启动的第二个实例已经把全部探针挂完才退出(瞬态双 attach)。
        // 锁文件按输出文件名区分:写不同快照文件的实例互不干扰
        let lock_path = out
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .join(format!(
                "{}.lock",
                out.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("ltop-bpf")
            ));
        let lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)?;
        let rc = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc != 0 {
            bail!(
                "另一个 ltop-bpf 实例已在运行(锁 {}),本实例退出",
                lock_path.display()
            );
        }
        // the fd must stay open for the process lifetime or the lock is released
        std::mem::forget(lock);

        if BPF_OBJECT.is_empty() {
            bail!("构建时没有生成 eBPF 对象(需要系统 clang 带 bpf target)");
        }
        let mut ebpf = Ebpf::load(BPF_OBJECT)?;

        let mut attached: Vec<&str> = Vec::new();
        let mut failed: Vec<String> = Vec::new();
        for &(prog, symbol, _kret) in &PROBES {
            let Some(program) = ebpf.program_mut(prog) else {
                failed.push(format!("{symbol}: 程序 {prog} 不在对象里"));
                continue;
            };
            let probe: Result<&mut KProbe, _> = program.try_into();
            let probe = match probe {
                Ok(p) => p,
                Err(e) => {
                    failed.push(format!("{symbol}: {e}"));
                    continue;
                }
            };
            if let Err(e) = probe.load() {
                failed.push(format!("{symbol}: load: {e}"));
                continue;
            }
            match probe.attach(symbol, 0) {
                Ok(_) => attached.push(symbol),
                Err(e) => failed.push(format!("{symbol}: attach: {e}")),
            }
        }
        if attached.is_empty() {
            bail!("没有任何 kprobe 挂载成功: {}", failed.join("; "));
        }
        for f in &failed {
            eprintln!("ltop-bpf: 跳过 {f}");
        }

        // identity of this attach (one kernel map = one counter universe); the
        // TUI re-baselines whenever it changes, so even a daemon restart with a
        // stale snapshot on disk can never be diffed across instances
        let inst = format!("{}-{}", std::process::id(), now_ms());

        // take_map gives owned MapData, so all maps can live side by side
        let Some(map) = ebpf.take_map("net_bytes") else {
            bail!("对象里没有 net_bytes map");
        };
        let mut map: HashMap<_, u32, UdpVal> = HashMap::try_from(map)?;
        let Some(udpmap) = ebpf.take_map("udp_bytes") else {
            bail!("对象里没有 udp_bytes map");
        };
        let mut udpmap: HashMap<_, u32, UdpVal> = HashMap::try_from(udpmap)?;
        let Some(skmap) = ebpf.take_map("udp_sk") else {
            bail!("对象里没有 udp_sk map");
        };
        let mut skmap: HashMap<_, u64, UdpVal> = HashMap::try_from(skmap)?;

        eprintln!(
            "ltop-bpf: 已挂载 {}; 快照 {} 每 {}ms",
            attached.join("+"),
            out.display(),
            interval.as_millis()
        );

        loop {
            let entries = read_map(&mut map)?;
            let udp_entries = read_map(&mut udpmap)?;
            let sk_entries = read_map(&mut skmap)?;
            let ts = now_ms();
            write_snapshot(&out, &inst, ts, &entries, &udp_entries, &sk_entries)?;
            if once {
                let (recv, sent): (u64, u64) = entries
                    .iter()
                    .map(|(_, v)| (v.recv, v.sent))
                    .fold((0, 0), |(a, b), (c, d)| (a + c, b + d));
                println!(
                    "ltop-bpf: {} 个 pid / {} 个 socket,Σrecv {} Σsent {} → {}",
                    entries.len(),
                    sk_entries.len(),
                    recv,
                    sent,
                    out.display()
                );
                return Ok(());
            }
            std::thread::sleep(interval);
        }
    }

    fn read_map<K: aya::Pod, T: std::borrow::BorrowMut<aya::maps::MapData>>(
        map: &mut HashMap<T, K, UdpVal>,
    ) -> Result<Vec<(K, UdpVal)>> {
        let mut out = Vec::new();
        for k in map.keys() {
            let k = k?;
            if let Ok(v) = map.get(&k, 0) {
                out.push((k, v));
            }
        }
        Ok(out)
    }

    /// atomic replace so the TUI never reads a half-written file
    fn write_snapshot(
        path: &std::path::Path,
        inst: &str,
        ts: u64,
        entries: &[(u32, UdpVal)],
        udp_entries: &[(u32, UdpVal)],
        sk_entries: &[(u64, UdpVal)],
    ) -> Result<()> {
        let mut sorted: Vec<(u32, UdpVal)> = entries.to_vec();
        sorted.sort_by(|a, b| {
            (b.1.recv + b.1.sent)
                .cmp(&(a.1.recv + a.1.sent))
                .then_with(|| a.0.cmp(&b.0))
        });
        let mut json = format!("{{\"inst\":\"{inst}\",\"ts\":{ts},\"pids\":[");
        for (i, (pid, v)) in sorted.iter().enumerate() {
            if i > 0 {
                json.push(',');
            }
            json.push_str(&format!(
                "{{\"pid\":{pid},\"recv\":{},\"sent\":{}}}",
                v.recv, v.sent
            ));
        }
        // UDP-only per-pid ledger, feeds the UDP breakdown in the process detail
        json.push_str("],\"upids\":[");
        let mut udps: Vec<(u32, UdpVal)> = udp_entries.to_vec();
        udps.sort_by(|a, b| {
            (b.1.recv + b.1.sent)
                .cmp(&(a.1.recv + a.1.sent))
                .then_with(|| a.0.cmp(&b.0))
        });
        for (i, (pid, v)) in udps.iter().enumerate() {
            if i > 0 {
                json.push(',');
            }
            json.push_str(&format!(
                "{{\"pid\":{pid},\"recv\":{},\"sent\":{}}}",
                v.recv, v.sent
            ));
        }
        // per-socket side ledger keyed by socket INODE (the BPF program walks
        // sk→socket→file→inode via CO-RE, so the key is already what CONNS uses)
        json.push_str("],\"sks\":[");
        let mut sks: Vec<(u64, UdpVal)> = sk_entries.to_vec();
        sks.sort_by(|a, b| {
            (b.1.recv + b.1.sent)
                .cmp(&(a.1.recv + a.1.sent))
                .then_with(|| a.0.cmp(&b.0))
        });
        for (i, (ino, v)) in sks.iter().enumerate() {
            if i > 0 {
                json.push(',');
            }
            json.push_str(&format!(
                "{{\"ino\":{ino},\"recv\":{},\"sent\":{}}}",
                v.recv, v.sent
            ));
        }
        json.push_str("]}");
        let tmp = path.with_extension("json.tmp");
        fs::File::create(&tmp)?
            .write_all(json.as_bytes())
            .and_then(|_| {
                fs::set_permissions(&tmp, fs::Permissions::from_mode(0o644))?;
                fs::rename(&tmp, path)
            })?;
        Ok(())
    }

    fn now_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }
}

#[cfg(target_os = "linux")]
fn main() {
    imp::entry()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("ltop-bpf 仅支持 Linux(当前平台无 eBPF);TUI 主程序不受影响");
    std::process::exit(1);
}

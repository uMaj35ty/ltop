use std::collections::{HashMap, VecDeque};
use std::ffi::CString;
#[cfg(target_os = "linux")]
use std::fs;
use std::time::Instant;

const HIST_CAP: usize = 900;

/// 图表缩放峰值衰减(与 net.rs PEAK_DECAY 同值):即时跟涨、每样本慢落
const PEAK_DECAY: f64 = 0.97;
/// Linux:计数器单位是扇区;mac(IOKit)已是字节
#[cfg(target_os = "linux")]
const SECTOR: f64 = 512.0;
#[cfg(target_os = "macos")]
const SECTOR: f64 = 1.0;

/// Filesystems that are virtual, read-only images, or may block statvfs on stalls
const FS_BLACKLIST: &[&str] = &[
    "proc",
    "sysfs",
    "devtmpfs",
    "devpts",
    "securityfs",
    "cgroup",
    "cgroup2",
    "pstore",
    "bpf",
    "autofs",
    "mqueue",
    "hugetlbfs",
    "debugfs",
    "tracefs",
    "configfs",
    "fusectl",
    "efivarfs",
    "ramfs",
    "nsfs",
    "rpc_pipefs",
    "overlay",
    "squashfs",
    "iso9660",
    "tmpfs",
    "nfs",
    "nfs4",
    "cifs",
    "smbfs",
    "9p",
    "afs",
    // 陈旧网络挂载的 statvfs 可阻塞分钟级,冻结整个 UI tick(采样在
    // UI 路径上)——网络 FS 一律不入盘盒
    "afp",
    "webdav",
    "glusterfs",
    "ceph",
    "cephfs",
    "lustre",
    "ocfs2",
    "devfs",
];

/// Virtual/ephemeral block devices kept out of the device tree
#[cfg(target_os = "linux")]
const SKIP_BLOCK_PREFIXES: &[&str] = &["loop", "ram", "zram", "sr", "fd", "nbd", "md"];

#[derive(Clone, Default)]
pub struct DiskDev {
    /// kernel device name (stats key): dm-0, sdb1, nvme0n1p1
    pub name: String,
    /// /dev/mapper friendly name, e.g. "root" for dm-0
    pub pretty: Option<String>,
    pub fstype: String,
    /// all mountpoints backed by this device (btrfs subvols collapse here)
    pub mounts: Vec<String>,
    pub total: u64,
    pub used: u64,
    pub rate_r: f64,
    pub rate_w: f64,
    pub iops: f64,
}

#[derive(Clone)]
pub struct TreeNode {
    pub name: String,
    pub pretty: Option<String>,
    pub size: Option<u64>,
    pub children: Vec<TreeNode>,
}

pub struct DiskCollector {
    prev: HashMap<String, Prev>,
    /// one entry per backing device (deduped across btrfs subvol mounts)
    pub devices: Vec<DiskDev>,
    /// block device hierarchy from /sys/block
    pub tree: Vec<TreeNode>,
    pub hist_r: VecDeque<f64>,
    pub hist_w: VecDeque<f64>,
    /// 图表缩放峰值(阻尼:即时跟涨、每样本慢速回落,见 net.rs PEAK_DECAY)
    peak_r: f64,
    peak_w: f64,
    last_t: Instant,
    /// session cumulative bytes (independently resettable)
    session_r: f64,
    session_w: f64,
}

impl DiskCollector {
    pub fn new() -> Self {
        Self {
            prev: HashMap::new(),
            devices: Vec::new(),
            tree: Vec::new(),
            hist_r: VecDeque::new(),
            hist_w: VecDeque::new(),
            peak_r: 0.0,
            peak_w: 0.0,
            last_t: Instant::now(),
            session_r: 0.0,
            session_w: 0.0,
        }
    }

    pub fn sample(&mut self) {
        let now = Instant::now();
        let stats = parse_diskstats();
        #[cfg(target_os = "linux")]
        let dm = dm_name_map();
        #[cfg(target_os = "macos")]
        let _dm: HashMap<String, String> = HashMap::new();
        let mounts = parse_mountinfo();

        // group mounts by resolved backing device
        let mut agg: HashMap<String, DiskDev> = HashMap::new();
        #[cfg(target_os = "macos")]
        {
            // APFS 的挂载点在合成卷上(disk3sX,IOKit 统计恒 0),真实 I/O
            // 计数在物理叶子(disk0s*)——反向 join:设备 = 全部带计数的
            // 叶子(速率图的数据面),挂载只作行标注,容量仅挂载行有
            for name in stats.keys() {
                agg.insert(
                    name.clone(),
                    DiskDev {
                        name: name.clone(),
                        ..Default::default()
                    },
                );
            }
            for (_, mount, source, fstype) in mounts {
                let base = source.rsplit('/').next().unwrap_or("").to_string();
                match agg.get_mut(&base) {
                    Some(dev) => {
                        dev.mounts.push(mount.clone());
                        dev.fstype = fstype;
                        if let Some((total, used)) = fs_usage(&mount) {
                            dev.total = total;
                            dev.used = used;
                        }
                    }
                    None => {
                        // 挂载但无计数(网络卷等):容量行,速率恒 0
                        let mut dev = DiskDev {
                            name: base.clone(),
                            fstype,
                            ..Default::default()
                        };
                        if let Some((total, used)) = fs_usage(&mount) {
                            dev.total = total;
                            dev.used = used;
                        }
                        dev.mounts.push(mount);
                        agg.insert(base, dev);
                    }
                }
            }
        }
        #[cfg(target_os = "linux")]
        for (majmin, mount, source, fstype) in mounts {
            let Some((total, used)) = fs_usage(&mount) else {
                continue;
            };
            let base = source.rsplit('/').next().unwrap_or("").to_string();
            let dm_key = dm.get(&base).cloned();
            // prefer kernel name, then mapper alias, then major:minor —
            // the name is what device-tree nodes are keyed by
            let mut key: Option<String> = None;
            for c in [Some(base.clone()), dm_key, Some(majmin.clone())]
                .into_iter()
                .flatten()
            {
                if stats.contains_key(&c) {
                    key = Some(c);
                    break;
                }
            }
            let Some(key) = key else { continue };
            let entry = agg.entry(key.clone()).or_insert_with(|| DiskDev {
                name: key.clone(),
                pretty: dm.get(&base).cloned(),
                fstype,
                total,
                used,
                ..Default::default()
            });
            entry.mounts.push(mount);
        }

        let mut prev = HashMap::new();
        let mut devs: Vec<DiskDev> = Vec::new();
        for (_, mut dev) in agg {
            dev.mounts.sort();
            if let (Some(p), Some(raw)) = (self.prev.get(&dev.name), stats.get(&dev.name)) {
                let dt = now.duration_since(p.t).as_secs_f64().max(1e-6);
                dev.rate_r = raw.sectors_r.saturating_sub(p.sectors_r) as f64 * SECTOR / dt;
                dev.rate_w = raw.sectors_w.saturating_sub(p.sectors_w) as f64 * SECTOR / dt;
                dev.iops = raw.ios.saturating_sub(p.ios) as f64 / dt;
            }
            if let Some(raw) = stats.get(&dev.name) {
                prev.insert(
                    dev.name.clone(),
                    Prev {
                        sectors_r: raw.sectors_r,
                        sectors_w: raw.sectors_w,
                        ios: raw.ios,
                        t: now,
                    },
                );
            }
            devs.push(dev);
        }
        devs.sort_by(|a, b| a.mounts.first().cmp(&b.mounts.first()));

        self.prev = prev;
        self.devices = devs;
        self.tree = build_block_tree();

        let tr: f64 = self.devices.iter().map(|d| d.rate_r).sum();
        let tw: f64 = self.devices.iter().map(|d| d.rate_w).sum();
        let dt_g = now.duration_since(self.last_t).as_secs_f64();
        self.last_t = now;
        self.session_r += tr * dt_g;
        self.session_w += tw * dt_g;
        push_hist(&mut self.hist_r, tr);
        push_hist(&mut self.hist_w, tw);
        // 阻尼峰值:即时跟涨、每样本 ×0.97 慢落(单帧 IOPS 尖峰不再把
        // 整窗波形压扁到贴地)
        self.peak_r = tr.max(self.peak_r * PEAK_DECAY);
        self.peak_w = tw.max(self.peak_w * PEAK_DECAY);
    }

    /// 图表缩放峰值(read, write):draw 侧以 GRAPH_FLOOR.max(peak) 做比例
    pub fn peaks(&self) -> (f64, f64) {
        (self.peak_r, self.peak_w)
    }

    /// (read B/s, write B/s, iops, session read bytes, session write bytes)
    pub fn totals(&self) -> (f64, f64, f64, u64, u64) {
        let tr: f64 = self.devices.iter().map(|d| d.rate_r).sum();
        let tw: f64 = self.devices.iter().map(|d| d.rate_w).sum();
        let ti: f64 = self.devices.iter().map(|d| d.iops).sum();
        (tr, tw, ti, self.session_r as u64, self.session_w as u64)
    }

    pub fn reset_session(&mut self) {
        self.session_r = 0.0;
        self.session_w = 0.0;
    }

    pub fn dev_by_name(&self, name: &str) -> Option<&DiskDev> {
        self.devices.iter().find(|d| d.name == name)
    }

    pub fn hist(&self) -> (Vec<f64>, Vec<f64>) {
        (
            self.hist_r.iter().copied().collect(),
            self.hist_w.iter().copied().collect(),
        )
    }
}

impl Default for DiskCollector {
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

/// (total_bytes, used_bytes)
#[cfg(target_os = "linux")]
fn fs_usage(path: &str) -> Option<(u64, u64)> {
    let c = CString::new(path).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: st is a valid, fully-owned allocation for the duration of the call
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    let fr = st.f_frsize.max(st.f_bsize) as u64;
    let total = st.f_blocks as u64 * fr;
    let used = total.saturating_sub(st.f_bfree as u64 * fr);
    Some((total, used))
}

/// mac:total = statfs f_blocks(APFS 卷报容器级容量,与 df 的 Total 列
/// 同口径);used = getattrlist ATTR_VOL_SPACEUSED 逐卷真实已用——statfs
/// 的 f_bfree 在 APFS 上是容器级共享池,total−free 对容器内每个卷都算出
/// 同一个数(df 正是用 getattrlist 才拿到逐卷 Used 的),失败才回退差值
#[cfg(target_os = "macos")]
fn fs_usage(path: &str) -> Option<(u64, u64)> {
    let c = CString::new(path).ok()?;
    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: st is a valid, fully-owned allocation for the duration of the call
    if unsafe { libc::statfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    let fr = st.f_bsize as u64;
    let total = st.f_blocks as u64 * fr;
    let used = volume_spaceused(&c).unwrap_or_else(|| total.saturating_sub(st.f_bfree as u64 * fr));
    Some((total, used))
}

/// getattrlist(ATTR_VOL_SPACEUSED):逐卷真实已用字节,df 的 Used 列同源
#[cfg(target_os = "macos")]
fn volume_spaceused(c: &CString) -> Option<u64> {
    #[repr(C)]
    struct AttrList {
        bitmapcount: u16,
        reserved: u16,
        commonattr: u32,
        volattr: u32,
        dirattr: u32,
        fileattr: u32,
        forkattr: u32,
    }
    const ATTR_BIT_MAP_COUNT: u16 = 5;
    const ATTR_VOL_INFO: u32 = 0x8000_0000;
    const ATTR_VOL_SPACEUSED: u32 = 0x0080_0000;
    extern "C" {
        fn getattrlist(
            path: *const libc::c_char,
            attrlist: *const AttrList,
            attrbuf: *mut libc::c_void,
            bufsize: libc::size_t,
            options: u32,
        ) -> libc::c_int;
    }
    let attrs = AttrList {
        bitmapcount: ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: 0,
        volattr: ATTR_VOL_INFO | ATTR_VOL_SPACEUSED,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    };
    // 返回布局:u32 长度头(实测=12)后紧跟 u64 值,偏移 4——实测
    // 0x032E2CD000=12.7GiB 与 df 的 Used 一致(无 8 字节自然对齐)
    let mut buf = [0u8; 16];
    // SAFETY: buf is fully owned for the duration of the call; the attribute
    // set requests exactly one u64, which fits the 16-byte buffer
    let r = unsafe { getattrlist(c.as_ptr(), &attrs, buf.as_mut_ptr().cast(), buf.len(), 0) };
    if r != 0 {
        return None;
    }
    Some(u64::from_ne_bytes(buf[4..12].try_into().unwrap()))
}

#[cfg(target_os = "linux")]
fn read_size(sysdir: &std::path::Path) -> Option<u64> {
    fs::read_to_string(sysdir.join("size"))
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(|s| s * 512)
}

/// mac:树已砍。registry 树被合成 APFS 管道层与模拟器镜像淹没(一台
/// 物理盘的机器能列出 30+ 节点),而宿主本就看不见 VM guest 内部的
/// 挂载与速率——物理层级在 mac 上没有信息量。draw 侧对空树回退到
/// 挂载卷平铺;Linux 保留完整物理层级(LUKS/LVM 有意义)。
#[cfg(target_os = "macos")]
fn build_block_tree() -> Vec<TreeNode> {
    Vec::new()
}

/// Build the physical hierarchy: disk -> partition -> dm (LUKS/LVM).
/// dm devices spanning several disks become roots (RAID/VG level).
#[cfg(target_os = "linux")]
fn build_block_tree() -> Vec<TreeNode> {
    let Ok(rd) = fs::read_dir("/sys/block") else {
        return Vec::new();
    };
    type DiskEntry = (String, Option<u64>);
    type DmEntry = (String, Option<String>, Vec<String>, Option<u64>);
    let mut disks: Vec<DiskEntry> = Vec::new();
    let mut dms: Vec<DmEntry> = Vec::new();
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with("dm-") {
            let pretty = fs::read_to_string(e.path().join("dm/name"))
                .ok()
                .map(|s| s.trim().to_string());
            let slaves = fs::read_dir(e.path().join("slaves"))
                .map(|rd| {
                    rd.flatten()
                        .map(|s| s.file_name().to_string_lossy().into_owned())
                        .collect()
                })
                .unwrap_or_default();
            dms.push((name, pretty, slaves, read_size(&e.path())));
        } else if !SKIP_BLOCK_PREFIXES.iter().any(|p| name.starts_with(p)) {
            disks.push((name, read_size(&e.path())));
        }
    }
    disks.sort();

    let mut taken = vec![false; dms.len()];
    let mut roots = Vec::new();
    for (dname, dsize) in &disks {
        let mut children = Vec::new();
        if let Ok(rd) = fs::read_dir(format!("/sys/block/{dname}")) {
            let mut parts: Vec<(String, Option<u64>)> = rd
                .flatten()
                .filter_map(|e| {
                    let n = e.file_name().to_string_lossy().into_owned();
                    (n.starts_with(dname.as_str()) && e.path().join("dev").exists())
                        .then(|| (n, read_size(&e.path())))
                })
                .collect();
            parts.sort();
            for (pname, psize) in parts {
                let mut part = TreeNode {
                    name: pname.clone(),
                    pretty: None,
                    size: psize,
                    children: Vec::new(),
                };
                for (i, (dmname, pretty, slaves, size)) in dms.iter().enumerate() {
                    if !taken[i] && slaves.len() == 1 && slaves[0] == pname {
                        taken[i] = true;
                        part.children.push(TreeNode {
                            name: dmname.clone(),
                            pretty: pretty.clone(),
                            size: *size,
                            children: Vec::new(),
                        });
                    }
                }
                children.push(part);
            }
        }
        for (i, (dmname, pretty, slaves, size)) in dms.iter().enumerate() {
            if !taken[i] && slaves.len() == 1 && &slaves[0] == dname {
                taken[i] = true;
                children.push(TreeNode {
                    name: dmname.clone(),
                    pretty: pretty.clone(),
                    size: *size,
                    children: Vec::new(),
                });
            }
        }
        roots.push(TreeNode {
            name: dname.clone(),
            pretty: None,
            size: *dsize,
            children,
        });
    }
    for (i, (dmname, pretty, _, size)) in dms.iter().enumerate() {
        if !taken[i] {
            roots.push(TreeNode {
                name: dmname.clone(),
                pretty: pretty.clone(),
                size: *size,
                children: Vec::new(),
            });
        }
    }
    roots
}

/// mac:IOKit 叶子 IOMedia 的字节/操作计数(整盘聚合对象已跳过)
#[cfg(target_os = "macos")]
fn parse_diskstats() -> HashMap<String, DiskRaw> {
    let (map, _) = crate::collect::iokit::disk_counters();
    map.into_iter()
        .map(|(k, v)| {
            (
                k,
                DiskRaw {
                    sectors_r: v[0],
                    sectors_w: v[1],
                    ios: v[2] + v[3],
                },
            )
        })
        .collect()
}

#[cfg(target_os = "linux")]
fn parse_diskstats() -> HashMap<String, DiskRaw> {
    let mut out = HashMap::new();
    let Ok(content) = fs::read_to_string("/proc/diskstats") else {
        return out;
    };
    for line in content.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 14 {
            continue;
        }
        let name = f[2].to_string();
        let key = format!("{}:{}", f[0], f[1]);
        let nums: Vec<u64> = f[3..14].iter().filter_map(|x| x.parse().ok()).collect();
        if nums.len() < 11 {
            continue;
        }
        let raw = DiskRaw {
            sectors_r: nums[2],
            sectors_w: nums[6],
            ios: nums[0].saturating_add(nums[4]),
        };
        // index by major:minor AND by kernel device name
        out.insert(key, raw.clone());
        out.insert(name, raw);
    }
    out
}

/// /dev/mapper friendly names -> kernel dm names, e.g. "root" -> "dm-0"
#[cfg(target_os = "linux")]
fn dm_name_map() -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Ok(entries) = fs::read_dir("/sys/block") else {
        return out;
    };
    for entry in entries.flatten() {
        let kernel = entry.file_name().to_string_lossy().into_owned();
        if let Ok(name) = fs::read_to_string(entry.path().join("dm/name")) {
            out.insert(name.trim().to_string(), kernel);
        }
    }
    out
}

#[derive(Clone)]
struct Prev {
    sectors_r: u64,
    sectors_w: u64,
    ios: u64,
    t: Instant,
}

#[derive(Clone)]
struct DiskRaw {
    sectors_r: u64,
    sectors_w: u64,
    ios: u64,
}

type Mount = (String, String, String, String); // (major:minor, mountpoint, source, fstype)

/// mac:getfsstat 挂载表(major:minor 不可得,留空;source 的 basename
/// 恰是 IOKit 计数器的键,如 /dev/disk3s1s1 → disk3s1s1)
#[cfg(target_os = "macos")]
fn parse_mountinfo() -> Vec<Mount> {
    let n = unsafe { libc::getfsstat(std::ptr::null_mut(), 0, libc::MNT_NOWAIT) };
    if n <= 0 {
        return Vec::new();
    }
    // SAFETY: buffer sized by the getfsstat size probe above
    let mut buf: Vec<libc::statfs> = vec![unsafe { std::mem::zeroed() }; n as usize];
    let got = unsafe {
        libc::getfsstat(
            buf.as_mut_ptr(),
            (buf.len() * std::mem::size_of::<libc::statfs>()) as libc::c_int,
            libc::MNT_NOWAIT,
        )
    };
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for e in buf.iter().take(got.max(0) as usize) {
        let fstype = cstr_field(&e.f_fstypename);
        let mount = cstr_field(&e.f_mntonname);
        let source = cstr_field(&e.f_mntfromname);
        if FS_BLACKLIST.iter().any(|b| fstype == *b) {
            continue;
        }
        // APFS 系统卷只留 Data(Preboot/VM/Update 等是噪声);/Volumes 是
        // 外置与网络卷的自然挂载点
        let keep = mount == "/"
            || mount.starts_with("/System/Volumes/Data")
            || mount.starts_with("/Volumes/")
            || mount.starts_with("/mnt")
            || mount.starts_with("/media");
        if !keep {
            continue;
        }
        if !seen.insert((source.clone(), mount.clone())) {
            continue;
        }
        out.push((String::new(), mount, source, fstype));
    }
    out
}

#[cfg_attr(target_os = "linux", allow(dead_code))]
fn cstr_field(b: &[libc::c_char]) -> String {
    let bytes: Vec<u8> = b
        .iter()
        .take_while(|&&c| c != 0)
        // 经 c_int 中转:aarch64 的 c_char 是 u8(x86_64 是 i8),直转 u8
        // 在前者上是同类型 cast
        .map(|&c| c as libc::c_int as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(target_os = "linux")]
fn parse_mountinfo() -> Vec<Mount> {
    let mut out = Vec::new();
    let Ok(content) = fs::read_to_string("/proc/self/mountinfo") else {
        return out;
    };
    let mut seen = std::collections::HashSet::new();
    for line in content.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let Some(sep) = fields.iter().position(|f| *f == "-") else {
            continue;
        };
        if sep < 5 || sep + 2 >= fields.len() {
            continue;
        }
        let majmin = fields[2].to_string();
        let mount = unescape_octal(fields[4]);
        let fstype = fields[sep + 1].to_string();
        let source = unescape_octal(fields[sep + 2]);

        if FS_BLACKLIST.iter().any(|b| fstype == *b)
            || fstype.starts_with("fuse.") && fstype != "fuseblk"
            || !(mount.starts_with("/mnt")
                || mount.starts_with("/media")
                || mount.starts_with("/run/media")
                || mount == "/"
                || (!mount.starts_with("/proc")
                    && !mount.starts_with("/sys")
                    && !mount.starts_with("/dev")
                    && !mount.starts_with("/run")))
        {
            continue;
        }
        if !seen.insert((majmin.clone(), mount.clone())) {
            continue;
        }
        out.push((majmin, mount, source, fstype));
    }
    out
}

#[cfg(target_os = "linux")]
fn unescape_octal(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 3 < bytes.len() {
            let oct = &s[i + 1..i + 4];
            if let Ok(v) = u8::from_str_radix(oct, 8) {
                out.push(v);
                i += 4;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

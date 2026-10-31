# 兼容性

## 要求

源码构建要 Rust stable; 平台为 **x86_64 / aarch64** 的 Linux 或 macOS。
预编译 Linux 二进制基线 **glibc 2.35**（Ubuntu 22.04 LTS）。eBPF 计费仅
Linux，*构建期*需要带 `bpf` target 的 clang; *运行期*没有任何特殊要求——
不要 BTF、不要 nightly、不要内核头。

## 已测环境

每个 release 的 tarball 都在 QEMU/KVM 里逐发行版验证: 开箱二进制、eBPF
探针挂载（含 kprobe）、systemd 单元、无助手时的静默降级。日常开发环境是
Omarchy（Arch），它同时充当参考桌面。

| 环境 | 内核 | eBPF 计费 | 备注 |
|---|---|---|---|
| Arch Linux (rolling) | 7.2.7 | ✓ 完整 | 同时是开发机（Omarchy 桌面） |
| Ubuntu 24.04 LTS | 6.8 | ✓ 完整 | glibc 2.39 |
| Ubuntu 22.04 LTS | 5.15 | ✓ 完整 | **无 BTF 可跑**; glibc 2.35 基线 |
| Ubuntu 24.04 LTS (aarch64) | 6.8 | ✓ 完整 | QEMU aarch64; 树莓派级硬件预期一致 |
| Debian 13 | 6.12 | ✓ 完整 | glibc 2.41 |
| Fedora 44 (Workstation live) | 6.19 | ✓ 完整 | SELinux enforcing: **零拒绝** |
| macOS 27（Apple Silicon，MacBook Air） | darwin 27 | n/a（`nettop` 路径） | 实机验证; 见 [macOS](#macos) |

裁过 kprobes/BTF 的特种内核（如 Ubuntu 的 `-kvm` 云内核）是唯一已知的坑:
换 `generic` 内核。那是内核 flavor 的限制，不是 ltop 的。

## SELinux

Fedora（enforcing）下两个二进制加 systemd 单元零拒绝——助手走标准 bpf
syscall 路径挂 kprobe，只往 `/run/ltop/` 写一份全局可读快照。

## macOS

macOS 没有 eBPF; 同样六个盒子读 Apple 原生源:

- **PROCS 和 CONNS** 轮询 `nettop`（每进程行与每 socket 流，TCP+UDP 一次
  spawn 全拿）; CONNS 归属经 `libproc` 扫调用者自己的 socket——其他用户的
  连接显示 `n/a`。
- **DISK** 读 IOKit `IOMedia` 统计加每进程 rusage。
- **LAN** 解析 `arp -an` 和默认路由——并对平台的邻居表脱敏做了绕行（见下）。
- macOS 构建不含 `ltop-bpf`。

与 Linux 路径的已知差异:

- socket 计数是**活 socket 上的累计值**，靠抽样: 一个采样周期内完成的
  传输不计入。（eBPF 路径在系统调用处计数，不漏。）
- **系统对非 Apple 签名二进制隐藏部分本地网络**——与"本地网络"隐私开关
  无关: 邻居表枚举为空，单条路由查询返回的 MAC 被脱敏成接口合成地址，
  `getifaddrs` 字节计数冻结（全部与 Apple 签名工具同秒交叉验证过）。
  macOS 27 更进一步: 从应用里 shell 出的 Apple 签名 `arp` 也返回空，
  `getifaddrs` 把所有网卡 MAC 报成 `02:00:00:00:00:00`（读取权改由未公开
  的 `com.apple.developer.networking.topology-observation` entitlement 按
  「责任进程归因 × 平台签名」双维度门控）。ltop 的绕法: NET 盒改用
  `netstat -ib` 计数，LAN 盒经路由 socket 逐条探测内核缓存重建邻居列表，
  `ltop lan-setup` 另装一个每用户 launchd 代理，经 Apple 平台签名工具
  轮转 `arp`/`ifconfig` 快照——LAN/NET 盒显示真实 MAC，免 root、免
  entitlement。未启用时 MAC 显示 `—`。
- 裸终端的帧buffer 字体可能缺盲文字形; 走 ssh 或用正经终端模拟器则齐全。

## 分发渠道回顾

细节见[安装](installation.md): Homebrew（macOS 与 Linux）、crates.io
（源码）、GitHub Releases tarball（x86_64/aarch64）、AUR `ltop-bin` 待
注册开放。

## 已知限制

- 其他用户的连接只能归到 uid 层（Linux 的 `/proc/<pid>/fd` 不可读;
  macOS 经 `libproc` 显示 `n/a`）。
- WiFi 网卡内核不给链路 `speed`，字段留空。
- CONNS 的 TCP 总量含整条连接生命周期（内核语义）——见
  [FAQ](faq.md#conns-总量包含-ltop-启动之前的流量)。
- `ltop-bpf` 重启后 eBPF 计数从零开始; UI 经快照实例 ID 自动重新基线。
- 配置文件: 未知键与注释行在保存时保留（0.2.10+），但**已知键**携带本版本
  不认识的值时（如更新版本新增的排序名），会回落默认并在下次保存时被重写。
- macOS 只计活 socket（nettop 语义）: 一个采样周期内完成的传输不计。

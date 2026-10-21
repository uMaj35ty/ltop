# localist top/ltop

[English](../README.md) | 简体中文

[![Crates.io](https://img.shields.io/crates/v/ltop)](https://crates.io/crates/ltop)
[![docs.rs](https://img.shields.io/docsrs/ltop)](https://docs.rs/ltop)
[![CI](https://github.com/uMaj35ty/ltop/actions/workflows/ci.yml/badge.svg)](https://github.com/uMaj35ty/ltop/actions/workflows/ci.yml)

一个终端本地网络监控工具,只盯一台机器:
网络走向(谁在和谁通信)、进程账本(谁在吃流量/磁盘)、局域网拓扑(网上有谁)——
无需 root 跑主程序,可选的 eBPF 助手补齐每进程/每连接的字节级计费。

```
 ▐▌localist top ▏ host omarchy ▏ lan 8 hosts ▏ refresh 1.0s                21:14:52
╭─┐¹net┌──────────────────────────────╮ ╭─┐²lan┌─┐8 hosts─┐ ┌─┐⁵conns┌──────────╮
│ ▲ up  wifi  mtu 1500  192.168.1.4/24 │ │ IP      MAC      HOST │ │ firefox×3 tcp ... │
│ ▼ RX 27.3 MiB/s  Σ 28.0 GiB          │ │ ✦192.168.1.1 …  gw    │ │ steam  udp  …     │
│ ⣿⣿⣿⣤⣿⣿⣿⣿⣿⣿⣀⠀⣿⠀⣿                    │ └───────────────────────┘ └───────────────────┘
╰──────────────────────────────────────╯ ╭─┐⁶procs┌──────────────────────────────────╮
╭─┐³disk i/o┌───────────────────────────╮ │ firefox(312141)  net Σ=… r …/…       │
│ ▼ READ …   MOUNT   DEV   USED   R/S W/S │ │ steam(3190)      net Σ=… r …/…       │
╰─────────────────────────────────────────╯ ╰──────────────────────────────────────╯
[/] 搜索  [space] 暂停  [q] 退出  [m] 菜单  [Tab] 布局  [1-6] 盒子  [enter] 详情
```

## 快速安装

```bash
cargo install ltop
```

AUR:`ltop-bin`——待 AUR 开放注册后提供(打包件已就绪)。

## 截图

Omarchy 上的 classic 布局,主题跟随桌面:

![classic 布局——六盒、实时 LAN 邻居与分组连接](../assets/screenshots/classic-six-boxes.png)

wide 布局 + 详情面板(启动命令、每连接分项、追踪/kill/拉起)与主题菜单:

![wide 布局与详情面板](../assets/screenshots/wide-detail-panel.png)

tall 布局——分组连接带合计,追踪行高亮:

![tall 布局与进程分组](../assets/screenshots/tall-process-groups.png)

实时 Omarchy 主题跟随(`--follow-theme`)——ltop 运行中随桌面切主题换装:

![Omarchy 主题跟随](../assets/screenshots/omarchy-theme-follow.gif)

## 特性

- **六个盒子,各管一摊**
  - **NET** — 每网卡实时上下行速率(braille 面积图)、连接状态/速率/双工/MTU/IP、自启动累计流量
  - **LAN** — 同网段设备自动发现:IP / MAC / 主机名 / 接口,✦ 标记网关,后台扫描刷新;
    每个邻居旁显示与它通信的连接数、速率与累计字节(与 CONNS 联动)
  - **DISK I/O / DISKS** — 读写速率图(按设备去重)+ 物理设备树(`nvme0n1 → dm-0(root)`)、
    容量用量条、实时读写速率
  - **CONNS** — 全系统每条连接:TCP 显示每连接真实 B/s 与累计字节;UDP 连接由 eBPF 计费
    同样有速率与 Σ;协议标签、进程分组、关闭后 5 秒墓碑行、四态过滤(全部/局域网/tcp/udp)
  - **PROCS** — 监控时段内每进程的**总**网络(TCP+UDP,短连接不漏账)与**总**磁盘读写;
    已退出进程置灰保留,`s` 键一键拉起/重启;支持搜索、追踪置顶、四种排序
- **eBPF 网络计费(TCP+UDP,可选)** — netlink 对 UDP 不提供每 socket 字节计数、对 TCP
  有短连接采样洞,eBPF 一并补齐:特权助手 `ltop-bpf` 挂 kprobe 到收发系统调用(含 v6),
  按 pid 与 socket inode 双账本累计,每秒快照到 `/run/ltop/udp.json`;无特权的 ltop
  读快照差分后并入账本。不需要 nightly 工具链(系统 clang 构建期内嵌);没跑助手时
  功能静默缺席,其余不受影响
- **监控日志与回放** — `L` 键录制每进程账本到 JSONL(默认 60s 粒度,保留期可调),
  `--replay` 差分还原录制期增量并输出报表,`--csv` 导出
- **三种布局** — `Tab` 在 classic(左图右表,CONNS 底边与图表分界线对齐)/ wide / tall 间切换,
  盒子开关(1–6)后自动收缩
- **主题与语言** — 内嵌 41 个 btop 官方主题热切换,或跟随 Omarchy 当前主题
  (启动时取当前主题;`--follow-theme` 可开启运行中实时跟随——桌面切主题一个
  tick 内换装,默认关);界面中英双语
- **焦点与追踪** — `p`/`c`/`l` 单焦点切换 procs/conns/lan,↑↓ 选中(选中行跟随进程/连接/邻居本体,
  重排不漂移),`enter` 详情,`space`/`t` 追踪置顶,`k` kill;`enter` 在 lan 行置顶邻居并联动
  CONNS 只看该对端

## 要求与源码构建

要求:Rust stable、Linux,**仅支持 x86_64**——eBPF 探针寄存器映射与各数据源按
x86_64 编写,其他架构启动时明确报错(arm64 为独立里程碑);eBPF 计费另需带
bpf target 的 clang(Arch 的 `clang` 即可)与 root 权限部署助手。

### 已测试环境

发布压缩包逐发行版在 QEMU/KVM 实测(开箱二进制、eBPF 探针挂载含 kprobe、
systemd 单元、无助手静默降级)。ltop 日常开发于 Omarchy(Arch),它同时是
参照桌面环境。

| 环境 | 内核 | eBPF 计费 | 备注 |
|---|---|---|---|
| Arch Linux (rolling) | 7.2.7 | ✓ 完整 | 亦为开发机(Omarchy 桌面) |
| Ubuntu 24.04 LTS | 6.8 | ✓ 完整 | glibc 2.39 |
| Ubuntu 22.04 LTS | 5.15 | ✓ 完整 | **无 BTF 也可用**;glibc 2.35 为二进制基线 |
| Debian 13 | 6.12 | ✓ 完整 | glibc 2.41 |
| Fedora 44(Workstation live) | 6.19 | ✓ 完整 | SELinux enforcing:零拒绝 |

更老的内核在特化变体里裁剪 kprobe/BTF(如 Ubuntu 的 `-kvm` 云内核):那种
环境请用 `generic` 内核——不是 ltop 的限制。

```bash
cargo install ltop                   # 从 crates.io 安装,开箱即用
# 或源码构建:
cargo build --release
./target/release/ltop                # 直接跑,零配置可用
sudo cp target/release/ltop-bpf /usr/local/bin/
sudo cp deploy/ltop-bpf.service /etc/systemd/system/
sudo systemctl enable --now ltop-bpf # 开启 UDP/TCP 每进程计费
```

二进制:`ltop` 与 `ltop-bpf`(特权采集器)。Release 压缩包与 AUR 包内自带
`loli` 软链别名(`-h` 跟随调用名显示);`cargo install` 的用户可自行
`ln -s ltop loli`。

```bash
ltop --theme <btop.theme>   # 指定主题文件
ltop --dump                 # 无界面打印一轮采集结果(调试)
ltop --log                  # 启动即录制监控日志
ltop --replay [--csv OUT]   # 回放/分析会话日志,缺省取最新一份
ltop-bpf --once             # 助手采一轮打印后退出(冒烟)
```

配置文件 `~/.config/ltop/config.conf` 为纯文本 `key = value`,设置面板
即改即存;高阶用法可直接编辑(重启生效):刷新 `tick_ms`、日志粒度/保留期、扫描间隔、
盒子开关、排序默认值、墓碑时长 `tombstone_ttl_ms`、主题 `theme` 等。

### 冒烟与诊断

```bash
sudo bash scripts/udp-smoke.sh     # 发起可对账的 UDP 流量并核对快照
python3 scripts/snap-diff.py       # 连续采样快照,输出每 pid 原始增量/计数回退
```

## 快捷键

| 键 | 作用 |
|---|---|
| `q` / `Ctrl-C` | 退出 |
| `space` | 暂停/恢复采样(画面停格读数,恢复后字节补记不丢) |
| `/` | 搜索:按名字过滤进程/连接(大小写不敏感);`Tab` 切换目标盒,`enter` 应用,`esc` 一键清除 |
| `m` | btop 风格菜单(themes / settings / reset / graphs / help / quit / close) |
| `Esc` | 主页一键清除焦点与选中;在面板内为关闭 |
| `1`–`6` | 开关盒子:1 net、2 lan、3 disk i/o、4 disks、5 conns、6 procs |
| `Tab` | 布局 classic → wide → tall |
| `p` / `c` / `l` | 单焦点切换 procs / conns / lan(再按取消;点亮一盒即熄灭其余) |
| `r` | procs 排序五态(name → net-sum → net-rate → disk-sum → disk-rate) |
| `f` / `o` | conns 过滤四态(all → lan → tcp → udp)/ 排序四态(**proc**: 同名进程相邻、组按合计流量降序、组首行 `名字 ×N` → sum → name → rate) |
| `enter` / `←` `→`(组首行) | 折叠/展开该进程分组(搜索命中自动展开;点击组首行同样切换;默认态可在设置调) |
| `d` | disks 展开/折叠挂载点树 |
| `e` | lan 排序四态(ip → 主机名 → mac → 接口) |
| `enter`(lan 行) | 置顶邻居到列表顶部(红色高亮)**并联动 CONNS 只看该对端**;再按取消并清除;`esc`/`f` 亦可清除 |
| `n` / `i` | 切网卡 / disk i/o 图显模式 |
| `↑↓` / `PgUp` `PgDn` / 滚轮 | 选中行移动 / 列表滚动;鼠标滚轮作用于光标所指列表,点击行选中(与 btop 同款,无可见滚动条) |
| `enter` | 详情面板:进程显示 exited 状态/启动命令/UDP 分项,连接显示协议/归属/Σ 与速率;`space`/`t` 追踪置顶,`k` kill(需确认),`s` 拉起/重启 |
| `k` | 对选中进程直接 SIGTERM(需在设置中开启 global kill) |
| `L` | 开始/停止监控日志 |
| `s` | 局域网扫描开关 |
| `+` / `-` | 刷新间隔 ±250ms |

## 数据口径(对不上不是 bug)

- **PROCS** = 应用层 payload 字节(eBPF 会话累计);**NET 盒** = 网卡字节
  (含 IP/TCP 包头、重传、内核流量)——两者天然差 10–40%
- **CONNS**:TCP Σ 为内核自建连累计(含 ltop 启动前);UDP Σ 为会话累计
- 走代理的流量记在**代理进程**头上(浏览器经代理出网时,外部连接归属代理)

## 工作原理

- **eBPF 计费管线**:kprobe 挂在收发系统调用上,按 pid 与 socket inode(内核内
  CO-RE 走 sk→socket→file→inode 解析)双账本累计 → 每秒原子快照 → ltop 差分合并。
  防脏三件套:retval 值域闸门(>16MiB 丢弃)、flock 单实例、快照实例 ID 换源重基线。
  退出自动卸载 BPF,崩溃不留探针
- **LAN 发现**:被动读 `/proc/net/arp` + 每 60s 对私网子网发 discard 端口 UDP knock
  (内核自动 ARP,无应用层流量);主机名三层尽力而为:mDNS 反查 → NetBIOS 节点状态
  → 反向 DNS
- **数据来源**:netlink `inet_diag`(连接表,TCP 计数)、`/proc/net/dev`(网卡)、
  `/proc/diskstats` + `statvfs`(磁盘)、`/proc/net/arp`(邻居)——详见
  `src/collect/` 各模块头注释

## 已知限制

- 其他用户的连接只能归属到 uid 层(无 `/proc/<pid>/fd` 读权)
- WiFi 网卡内核不提供 `speed`,显示为空
- CONNS 的 TCP Σ 含建连以来全部历史(内核语义),与 PROCS 会话累计口径不同
- ltop-bpf 重启后 eBPF 计数从头累计(ltop 依实例 ID 自动重基线,无需干预)

## 开发

```bash
cargo test        # 93 项:单元/渲染(TestBackend)/真机合流(netlink+快照)
cargo clippy      # 全仓零警告
```

诊断工具:`scripts/udp-smoke.sh`(流量对账冒烟)、`scripts/snap-diff.py`(快照差分)。
目录:`bpf/net_bpf.c`(eBPF 程序,C,构建期 clang 编译内嵌)、`src/collect/`(各采集器)、
`src/bin/ltop_bpf.rs`(特权助手)、`deploy/`(systemd unit)。

许可:主题文件来自 btop(Apache-2.0),其余代码与文档以 MIT 许可发布(见 LICENSE)。

# ltop

[English](../README.md) | 简体中文

[![Crates.io](https://img.shields.io/crates/v/ltop)](https://crates.io/crates/ltop)
[![docs.rs](https://img.shields.io/docsrs/ltop)](https://docs.rs/ltop)
[![CI](https://github.com/uMaj35ty/ltop/actions/workflows/ci.yml/badge.svg)](https://github.com/uMaj35ty/ltop/actions/workflows/ci.yml)

**一块屏幕,直接揪出吃带宽、吃磁盘的进程**——按进程的网络与磁盘 I/O、
逐连接流量、局域网邻居,btop 风格的终端界面。面向软件与游戏开发者、
系统管理员和折腾爱好者。主程序免 root,可选的 eBPF 助手补齐每进程/
每连接的字节级计费。

📖 **文档站(截图与画廊): [umaj35ty.github.io/ltop](https://umaj35ty.github.io/ltop/)**
——本 README 是电梯稿 + 安装与首次启动所需的全部;深层细节在文档站。

![Classic 布局——Steam 下载实况、六盒、实时 LAN 邻居与进程分组连接](../assets/screenshots/main.png)

## 快速安装

**Homebrew**(macOS 与 Linux;预编译二进制):

```bash
brew install uMaj35ty/ltop/ltop
```

**crates.io**(源码编译;需要 Rust 工具链):

```bash
cargo install ltop
```

**GitHub Releases**——`x86_64/aarch64` 的 Linux 与 macOS 预编译包(Linux 侧
glibc 2.35+)。二进制未签名:macOS 下载后需移除隔离属性
(`xattr -c ltop-*-darwin.tar.gz`)。
AUR:`ltop-bin`——打包件已就绪,待 AUR 开放注册后发布。
0.2.4 起不再内置 `loli` 别名,需要就自己 `ln -s` 一个。

## 首次运行

```bash
ltop
```

无需 root、零配置。第一屏最要紧的键:

| 按键 | 作用 |
|---|---|
| `Tab` | 布局循环:classic → wide → tall |
| `/` | 按名字搜索进程/连接 |
| `enter` | 选中行详情面板(完整命令行、逐连接分解、追踪/终止/重启) |
| `p` / `c` / `l` | 单焦点 procs / conns / lan |
| LAN 行上 `enter` | 置顶该邻居并让 CONNS 只看它 |
| `1`–`6` / `z` / `space` / `m` | 开关盒子 / 缩放 / 暂停 / 菜单 |

完整键位表与六盒导览:[文档站 TUI 页](https://umaj35ty.github.io/ltop/docs/tui.html)
(应用内 `m` → help 始终可用)。

**可选(Linux)**——经特权 eBPF 助手解锁精确的每进程/每连接计费(含 UDP):

```bash
sudo cp ltop-bpf /usr/local/bin/            # 与 TUI 同一 release
sudo cp ltop-bpf.service /etc/systemd/system/
sudo systemctl enable --now ltop-bpf
```

升级时助手要随 TUI 一起重新部署——陈旧的 `ltop-bpf` 会让每进程计费静默归零。

## 面向 AI 代理

`ltop --json` 输出结构化快照(schema v1:网络速率、每进程/每连接字节、
磁盘 I/O、LAN 邻居);`ltop --schema` 是随版本更新的字段参考。配套的
agent skill([docs/skills/ltop-observe/SKILL.md](skills/ltop-observe/SKILL.md))
教 agent 何时以及如何使用,并附诊断工作流。

时间窗任务用 **`ltop watch` + `ltop report`**:watch 无头录制 schema v1
JSONL,report 聚合出精确的窗口总和与峰值——不是抽样猜测:

```bash
ltop watch --duration 10m --proc chrome   # 录制(崩溃安全,免 root)
ltop report <watch 打印的路径> --json   # ~2 KB 摘要(单文件)
```

实机实测:10 分钟窗口轮询 `--json` 约 4 万 token,`report --json` 约 1k
——**省 ~97%**;一次性问题用 jq 过滤后的单帧 `--json` 约 1.7k token,
传统命令组合约 2.2 万。详见[文档站 AI 页](https://umaj35ty.github.io/ltop/docs/ai.html)。

## 功能

- **六盒,各司一职**——NET(每网卡盲文图表、链路状态、累计流量)·
  LAN(自动发现的邻居,带每对端连接数/速率/字节)· DISK I/O + DISKS
  (速率图表、设备树、容量条)· CONNS(系统全部 socket,真实逐连接
  B/s 与总量,进程分组,已关闭连接留墓碑行)· PROCS(每进程整场网络
  TCP+UDP 与磁盘总量;退出的进程灰显保留、一键重启)
- **eBPF 计费,可选(Linux)**——kprobe 挂在 send/recv(v4+v6),按 pid 和
  socket inode 累计字节;无特权的 UI 每秒 diff `/run/ltop/udp.json`
  快照。LRU map、单实例锁、退出自动摘除——TUI 免 root,构建不需要
  nightly。没有助手其余一切照常
- **macOS 真 MAC 地址,27 也不例外**——苹果按「归因 × 签名」门控邻居与
  网卡 MAC(27 连应用里跑的 Apple 签名 `arp` 都返回空);`ltop lan-setup`
  安装每用户 launchd 代理,经 Apple 平台签名工具轮转 `arp`/`ifconfig`
  快照——LAN/NET 盒显示真实 MAC,免 root、免 entitlement
- **无头 watch + report**——崩溃安全的 JSONL 采集(`--interval`/
  `--duration`/`--stop` 哨兵/`--proc` 聚焦)与窗口差分摘要;TUI 监控日志
  (`L`/`--log`)共用同一 schema,`--replay`/`--csv` 兼容旧日志
- **三布局、焦点与缩放**——`Tab` 循环 classic/wide/tall;`p`/`c`/`l`
  单焦点;选中行跨重排跟随;`z` 缩放;支持鼠标滚轮/点击
- **72 套内嵌 btop 主题**——任何 btop 主题文件直接可用(`--theme <file>`);
  Omarchy 下跟随桌面主题,`--follow-theme` 让运行中的 ltop 实时变色
  (默认关);界面英文或中文(`~/.config/ltop/config.conf` 里 `lang = zh`)
- **Linux 与 macOS,x86_64 + aarch64**——macOS 读 Apple 原生源
  (nettop、libproc、IOKit、路由 socket);每个 release 在 QEMU/KVM 五个
  发行版(含 SELinux enforcing 与无 BTF 内核)及 Apple Silicon 实机验证
  ——矩阵与平台差异见[兼容性页](https://umaj35ty.github.io/ltop/docs/compat.html)

## 数据口径(对不上不是 bug)

- **PROCS** = 应用载荷字节;**NET 盒** = 网卡字节(含 IP/TCP 头、重传、
  内核流量)——两者差 10–40% 属预期
- **CONNS** 的 TCP Σ = 内核 per-socket 计数器(整条连接生命周期,含
  ltop 启动之前);UDP Σ = 本场会话累计
- **NET Σ**:session Σ(ltop 启动以来,可重置)与 alive Σ(≈网卡启动以来)
- 经代理的流量记在代理进程头上

## 文档

| 页面 | 内容 |
|---|---|
| [快速上手](https://umaj35ty.github.io/ltop/docs/quickstart.html) | 安装 + 最要紧的键 |
| [TUI](https://umaj35ty.github.io/ltop/docs/tui.html) | 六盒、布局、完整键位 |
| [主题](https://umaj35ty.github.io/ltop/docs/themes.html) | 主题文件、Omarchy 跟随、自定义配色 |
| [LAN 发现](https://umaj35ty.github.io/ltop/docs/lan.html) | 被动 ARP + UDP knock、置顶与过滤 |
| [eBPF 计费](https://umaj35ty.github.io/ltop/docs/ebpf.html) | 助手架构、防脏护栏、数据口径 |
| [数据面](https://umaj35ty.github.io/ltop/docs/data.html) | `--json`/`--schema`、监控日志、watch/report、配置项 |
| [AI 代理](https://umaj35ty.github.io/ltop/docs/ai.html) | 快照 + watch/report 工作流、token 实测、agent skill |
| [兼容性](https://umaj35ty.github.io/ltop/docs/compat.html) | 测试矩阵、macOS 说明、已知限制 |
| [横向对比](https://umaj35ty.github.io/ltop/docs/comparison.html) | vs btop / nethogs / iftop / shell 管道 |
| [FAQ](https://umaj35ty.github.io/ltop/docs/faq.html) | 常见误读与坑 |
| [安全](https://umaj35ty.github.io/ltop/docs/zh/security.html) | 权限边界、磁盘数据、LAN 行为、漏洞上报 |
| [参与贡献](https://umaj35ty.github.io/ltop/docs/zh/contributing.html) | 环境准备、工程约定、PR 流程 |

## 源码构建

```bash
cargo build --release     # target/release/{ltop, ltop-bpf}   (ltop-bpf 仅 Linux)
cargo test                # 单元 + 渲染帧 + 实连集成测试
cargo clippy              # 零警告
```

Rust stable;Linux 上需要带 `bpf` target 的 clang 编译 eBPF 对象——没有
clang 构建照样成功,只有 UDP 计费静默降级。诊断脚本在 `scripts/`
(`udp-smoke.sh`、`snap-diff.py`、`soak.sh`);eBPF 程序是 `bpf/net_bpf.c`;
采集器在 `src/collect/`;systemd 单元在 `deploy/`。

## 特别致谢

特别感谢 **Aristocratos**
([btop](https://github.com/aristocratos/btop)) 与 btop 官方主题集的作者
们——ltop 的主题格式就是 btop 格式,内嵌 41 套主题转换自该主题集;同样
感谢 **DHH**([Omarchy](https://github.com/dhh/omarchy))与 Omarchy 内置
主题的创作者们,另外 31 套来自那里。每一套主题都是其作者的成果;详见
[主题 → 致谢](https://umaj35ty.github.io/ltop/docs/zh/themes.html#致谢)。

主题移植自 btop(Apache-2.0);其余代码与文档以 MIT 协议发布
(见 [LICENSE](../LICENSE))。

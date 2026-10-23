# 快速上手

## 1. 安装（一条命令）

```bash
brew install uMaj35ty/ltop/ltop     # macOS 与 Linux —— 预编译二进制
```

或者有 Rust 工具链的话:

```bash
cargo install ltop                  # 源码编译（crates.io）
```

或者从 [GitHub Releases] 下 tarball（`x86_64` / `aarch64`，Linux 与 macOS）。
全部渠道见[安装](installation.md)。

[GitHub Releases]: https://github.com/uMaj35ty/ltop/releases

## 2. 运行

```bash
ltop
```

不要 root，不要配置。落地即 classic 布局: 左边图表（上 NET 下 DISK I/O），
右边表格（LAN、CONNS、PROCS、DISKS）。

## 3. 最要紧的键

下面十个覆盖日常；页脚永远显示当前上下文，完整表在 `m → help`（也见
[TUI 界面](tui.md)）。

| 按键 | 作用 |
|---|---|
| `Tab` | 布局循环: classic → wide → tall |
| `1`–`6` | 开关盒子（1 net、2 lan、3 disk i/o、4 disks、5 conns、6 procs） |
| `/` | 按名字子串搜索进程/连接 |
| `p` / `c` / `l` | 单焦点 PROCS / CONNS / LAN（点亮一个，其余变暗） |
| `↑↓` + `enter` | 选中行 → 详情面板（完整命令行、逐连接分解、运行时长） |
| LAN 行上 `enter` | 置顶该邻居并让 CONNS 只看它——"这台设备到底在干什么" |
| `f` / `o` | CONNS: 过滤循环（all/lan/tcp/udp）/ 排序循环（proc 分组、sum、name、rate） |
| `r` | PROCS: 排序循环（net-sum、net-rate、disk-sum、disk-rate、name） |
| `z` | 焦点盒子全屏缩放（`esc` 退出） |
| `m` | 菜单: 主题、设置、图表、帮助 |
| `space` | 暂停/恢复采样（暂停期间漏掉的字节恢复时补齐） |
| `q` | 退出 |

## 4. 可选: 每进程/每连接字节计费（Linux）

没有这一步 ltop 也能用——CONNS 有 TCP 实时速率，PROCS 有来自 socket 抽样的
网络总量；缺的是*精确*的 UDP 每进程计费和每连接总量。一条命令开启:

```bash
sudo cp target/release/ltop-bpf /usr/local/bin/      # 从 release tarball 或源码构建
sudo cp deploy/ltop-bpf.service /etc/systemd/system/
sudo systemctl enable --now ltop-bpf
```

Homebrew 装的二进制相同；助手要与 TUI 出自同一个 release（见
[eBPF 计费](ebpf.md)和[升级注意事项](faq.md#升级后每进程计费归零)）。

## 5. 可选: AI 工作流

记住这一对命令:

```bash
ltop watch --duration 10m --proc chrome   # 无头录制（免 root，崩溃安全），
                                         # 结束时打印输出路径
ltop report <该路径> --json               # report 只收单个文件
```

`report` 给出精确的窗口总和与峰值，不是抽样猜测——10 分钟窗口 JSON 约 1k
token，轮询 `--json` 要 4 万。见 [AI 代理](ai.md)。

## 6. 调成自己喜欢的样子

- **主题**: `m` → theme 循环 72 套内嵌 btop 主题；Omarchy 下选 `omarchy`
  跟随桌面。`--theme <file>` 加载任意 btop 主题文件；`--follow-theme`
  让运行中的 ltop 跟着桌面变色。详见[主题](themes.md)。
- **语言**: `~/.config/ltop/config.conf` 里 `lang = zh`，所有人类可读输出
  （TUI 和 CLI）切中文；JSON 键恒为英文。
- **设置面板**: `m` → settings 写入 `~/.config/ltop/config.conf`（刷新率、
  日志保留、扫描间隔、排序默认……）。见[数据面](data.md#配置)。

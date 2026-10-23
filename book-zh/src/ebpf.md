# eBPF 计费

## 为什么需要它

常规路子有两个绕不过去的坑:

- **netlink/`/proc` 不给逐 socket 的 UDP 字节计数。** TCP 还有字节队列
  可看，UDP 什么都没有。
- **逐 socket 抽样会漏短命 TCP 连接。** 一个采样周期内完成全程的连接完全
  不可见——而这恰恰常是你想抓的流量（DNS、小请求、话痨型辅助进程）。

`ltop-bpf` 把计数挪到字节发生的地方: 在 send/recv 系统调用（v4 **和** v6）
上挂 kprobe，按 **pid** 和 **socket inode** 在内核 map 里累计载荷字节。
两本账喂给同一个 UI，于是 PROCS 拿到不会漏短命连接的每进程总量，CONNS
也能像 TCP 一样把字节记到单条 UDP 连接上。

## 架构

```
ltop-bpf（root，systemd）                ltop（免特权 TUI）
  kprobe 挂 send/recv v4+v6               每 tick 读 /run/ltop/udp.json
  pid 账本 ─┐                             按实例 ID 重基线后做差
            ├─ /run/ltop/udp.json  ──►    （饱和差分）
  inode 账本┘   每秒原子重写                合并进自己的账本
```

特权采集器和免特权 UI 之间只有一份每秒重写的全局可读快照文件。TUI 永远
不需要特权，助手也不需要知道终端的存在。

## 部署

```bash
sudo cp ltop-bpf /usr/local/bin/
sudo cp ltop-bpf.service /etc/systemd/system/
sudo systemctl enable --now ltop-bpf
```

- **冒烟测试:** `sudo ltop-bpf --once` 采一次打印即退；
  `sudo bash scripts/udp-smoke.sh` 造可计费的 UDP 流量并端到端验证快照；
  `python3 scripts/snap-diff.py` 打印每 pid 的原始差分。
- **升级:** 助手要随 TUI 一起重新部署（为什么见
  [FAQ](faq.md#升级后每进程计费归零)）。
- **源码构建**需要带 `bpf` target 的 clang；预编译 tarball 内含编译好的
  对象。

## 数据干净靠什么

几道护栏保证长时间运行的计数不出脏数据:

1. **LRU map 自清理。** pid 和 socket map 都是 `BPF_MAP_TYPE_LRU_HASH`
   （各 16384 项）: 空闲进程和关闭的 socket 自己被驱逐，几千进程的机器
   也耗不尽 map。
2. **实例 ID + 饱和差分。** 每个助手实例（`flock` 单实例锁守护）给快照
   打唯一 ID；TUI 发现 ID 变了就重新基线，差分用饱和运算——pid/socket
   复用、守护进程带着旧文件重启，都变不出负数或垃圾值。
3. **CO-RE，不做每内核构建。** 唯一和结构体耦合的部分（从 `sk` 走到
   socket inode）用 CO-RE 重定位，加载时对着内核 BTF 解析字段。没有 BTF
   的内核也能跑（Ubuntu 22.04 基线已验证）——字段偏移加载时解。

探针在退出时自动摘除——采集器崩了也不会留下 kprobe；寄存器读取有 16 MiB
值域闸门，野 `len` 污染不了账本。

## 数字怎么读

eBPF 账本计的是**载荷字节**（系统调用处）。NET 盒计的是**接口字节**
（驱动层，每个包都算）。两者范围*本来就不同*——头、重传、没经过你进程
系统调用的 LAN 杂音只存在于接口数字里——所以 PROCS 总量比 NET 盒低
10–40% 是正常形状，不是 bug。精确拆解见
[FAQ](faq.md#procs-总量比-net-盒低)。

## 没有助手时

`ltop` 默认免特权、从不要求助手: 没有它，UDP 每进程列干脆不出现（TCP 靠
socket 抽样照常），`--json` 里 `meta.udp_helper` 读 `false`。macOS 没有
eBPF 也没有助手——Apple 原生计费方式和差异说明见
[兼容性 → macOS](compat.md#macos)。

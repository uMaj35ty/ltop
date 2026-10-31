# 常见问题

## rx 和 tx 两张图形状一样

不是 bug——两个效应叠加。每张图按各自窗口内的峰值归一化（btop 同款，
为了小波动也可读），而下载时上行几乎全是 TCP ACK，其速率与下行包速率
一一对应。你会看到 tx/rx ≈ 1/30——正是 ACK 的理论占比（约每 2 个数据段
1 个 ACK，2900 字节数据对 66 字节确认）；图表把这条微小的波形放大到
满幅，两条曲线自然长得像。两个方向差距悬殊时读头部数字、在
`m` → graphs 里换 Block 图表模式，或开启比例耦合让小方向按真实占比占
图表高度：`m` → graphs → `c`（要持久化设 `net_graph_coupled = true`；
触发倍数 `net_graph_scale_ratio`，默认 5 倍）。

## PROCS 总量比 NET 盒低

所有平台都如此，是设计使然。NET 盒计**接口字节**（驱动层: 每个包都算，
含头、重传和背景杂音）; PROCS 计归到进程头上的**载荷字节**（Linux 在系统
调用处 eBPF 计，macOS 靠活 socket 计），部分平台还只算你自己的进程。差值
就是"是接口流量、但不是你进程载荷"的那部分。精确拆解见
[eBPF 计费 → 数字怎么读](ebpf.md#数字怎么读)。

## CONNS 总量包含 ltop 启动之前的流量

也是设计使然，仅 TCP: 连接的 Σ 来自内核逐 socket 计数器，覆盖连接的完整
生命周期——包括 ltop 启动之前。UDP Σ 由 ltop（助手在线时）从会话开始
累计。这处差异（加上包头）也是 CONNS 和 PROCS 总量互相对不上的原因之一;
经代理的流量记在代理进程头上。

## 升级后每进程计费归零

陈旧的 `ltop-bpf` 还在往旧路径写快照，而新 TUI diff 的是另一个文件——
症状是每进程数字静默读零。修法: 助手永远随 TUI 一起部署:

```bash
sudo cp ltop-bpf /usr/local/bin/          # 与 TUI 同一 release
sudo systemctl restart ltop-bpf
```

## UDP 列缺失/不完整

Linux 上这意味着 eBPF 助手没在跑——查 `ltop --json` 的 `meta.udp_helper`，
然后[部署助手](ebpf.md#部署)（TUI 本身免 root; 没有助手 TCP 列靠 socket
抽样照常）。macOS 的 UDP 每进程数字来自活 socket 抽样，一个采样周期内
完成的传输不计——已记录的平台差异，不是 bug。

## `conns[].process` 有些是 null

那些 socket 属于**其他用户**（Linux）: 归属扫描只扫你自己 uid 的 socket。
"null"是"归不到你头上"，不是"没有进程在用"。

## CONNS 里有个叫 "n/a" 的组（旧版本显示 uid:4294967295）

macOS 上的对应情形: 非特权的 ltop 只能归属**你自己用户**的 socket
（libproc，与 Linux 读自己的 /proc/<pid>/fd 同语义）。其他用户、系统守护
进程和内核的连接看得到流量，但归不到进程，于是聚在 `n/a` 组里——0.2.12
起如此; 旧版本把哨兵值原样打成 `uid:4294967295`，是同一个桶。这是权限
边界，不是数据缺失。想看归属，用 `sudo ltop` 跑一次即可（root 能枚举
全部进程，组随之清空）; 是否以 root 跑监控请自行权衡。

## `--json` 要等约 1.2 秒

背后连采两轮，每个速率字段才有真实差分可填。脚本上，这点延迟换来的是
速率而不只是计数; `--dump` 没这个等待但会截断值——解析请认准 `--json`。

## `watch` 把磁盘写满了

写盘量随 `--interval` 线性: 1 秒 ≈ 8 KB/行 ≈ 28 MB/h; 默认 5 秒约
5–6 MB/h。采集文件默认 `--retention 7` 天（下次 watch 启动时清理;
`0` 永久保留）。长期记录是监控日志的活（`L` / `--log`）——默认 60 秒
粒度、7 天保留。

## `loli` 命令没了

0.2.4 退役——*localist top* 时代的历史遗留，tarball/brew/AUR 都不再附带。
想要就自己软链一个（帮助文本跟着调用名走）:

```bash
ln -s "$(command -v ltop)" ~/.local/bin/loli
```

## AUR 包呢？

`ltop-bin` 打包已就绪，等 AUR 重新开放账号注册。在那之前用 Homebrew、
crates.io 或 GitHub Releases。

## 盲文字形缺（盒子显示成 `?` 或空白）

终端字体缺 Unicode 盲文区。换个字体齐的终端模拟器，或走 ssh——裸
framebuffer 控制台可能缺字形。macOS 自带 Terminal 的裸渲染尤其如此。

## 老内核/云镜像

eBPF 助手运行下限 = glibc 2.35 + 有 kprobes 的内核; TUI 本身只要二进制
能链接就能跑。Ubuntu 的 `-kvm` 云内核裁了 kprobes——换 `generic` 内核。
Ubuntu 22.04 的 5.15 **无 BTF 可跑**（CO-RE 加载时解析）。

## ltop 会不会偷偷联网/狂扫我的网络？

没有遥测，永远没有。唯一的对外动作是 LAN 扫描: 往自己子网发小 UDP 包
（默认每 60 秒，`sweep = false` 可关），借内核做 ARP 解析——不探端口、
不发 raw 包、不出已连接前缀一步。

## 怎么把报告变成 JSON 给 agent/脚本？

```bash
ltop report <file.jsonl> --json
```

……然后读 [AI 代理](ai.md)——包括为什么循环轮询 `--json` 每 10 分钟要
多烧约 4 万 token，而 report 只要约 1k。

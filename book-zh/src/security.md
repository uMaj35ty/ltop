# 安全

本页说明 ltop 会触碰你系统与网络的哪些部分、拒绝做哪些事,以及它读取的
不可信输入如何被处理。运营政策(支持版本、漏洞上报渠道)见仓库的
[SECURITY.md](https://github.com/uMaj35ty/ltop/blob/master/SECURITY.md)。

## 权限

- **TUI 全程免 root 运行。** 所有数据源都是任意进程可读的:`/proc`、
  `getifaddrs`、PF_ROUTE/netlink dump、临时 UDP socket。终止/重启进程
  由内核限制在你自己的 uid 内。
- **`ltop-bpf` 是唯一的特权组件**,且完全可选。它以 systemd 服务运行,
  只持有 `CAP_BPF` + `CAP_PERFMON`(< 5.11 内核另需 `CAP_SYS_RESOURCE`),
  只写 `/run/ltop` 下的一个快照文件,无任何监听端口、不发起网络连接。
  随包的 unit 已做沙箱:`NoNewPrivileges`、`ProtectSystem=strict`(仅
  `/run/ltop` 可写)、`ProtectHome`、`PrivateTmp`、内核接口保护,以及
  不含 `CAP_SYS_ADMIN` 的能力边界集。

## 磁盘上的数据

| 数据 | 位置 | 权限 |
| --- | --- | --- |
| 监控日志(`L` / `--log`) | `~/.local/state/ltop/` | 0600 |
| watch 采集 | `~/.local/state/ltop/` | 0600 |
| 配置 | `~/.config/ltop/config.conf` | 0644(仅界面偏好) |
| eBPF 快照 | `/run/ltop/udp.json` | 0644,root 属主——**设计上全局可读** |

会话日志与采集文件记录连接对端、进程名和主机名,因此一律以 `0600`
创建,不依赖 umask。eBPF 快照全局可读是因为免 root 的 TUI 要读它;在
共享主机上这意味着每个本地用户都能看到各进程的字节收发量——多用户
机器部署助手前请把这一点纳入考量。

ltop **没有任何遥测**:不检查更新、无统计上报,除下面的 LAN 探测外
没有任何数据离开你的机器。

## 网络行为

LAN 盒会主动查询本地网络:

- 逐邻居的单播 **mDNS**、**NetBIOS Node Status** 与**反向 DNS** 查询
  (主机名解析);
- 可配置的 UDP "knock" 扫描——向子网内每主机发一个到 discard 端口 9
  的空数据报,每轮最多 1022 台,默认 60 秒一轮,设置里可关。

所有行为都以你的局域网为边界。应答按不可信输入处理:DNS/NBNS 事务 ID
逐查询随机化并与源地址一同校验;所有解析器是有界检查的安全 Rust;不可
信字符串在进入任何终端输出前会被净化;eBPF 快照读取有文件大小上限。

## 漏洞上报

请使用仓库的 GitHub **Private Vulnerability Reporting**(可利用的问题
不要开公开 issue)。完整政策见
[SECURITY.md](https://github.com/uMaj35ty/ltop/blob/master/SECURITY.md)。

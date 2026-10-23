# AI 代理

ltop 把"这台机器的网络/磁盘/LAN 状态"当数据处理，而不是让你去刮屏幕:
一份 schema 稳定的 JSON 快照喂 TUI、CLI、日志和无头工具。本页是给 agent
用的工作参考; 同样内容在仓库里打包成了现成的 agent skill:
[`docs/skills/ltop-observe/SKILL.md`](https://github.com/uMaj35ty/ltop/blob/master/docs/skills/ltop-observe/SKILL.md)
（含诊断配方）。

## 一次性问题: `--json`

```bash
ltop --json | jq '<查询>'     # 快照; 约 1.2 秒（连采两轮才有速率）
ltop --schema                 # 版本化字段参考，永不漂移
```

单帧过滤后的快照约 1.7k token——传统命令组合（`ss`/`lsof`/`df`/`arp` 输出
再解析）约 2.2 万，不过滤约 8k。一次性问题这就是对的工具。

CLI 面严格: 未知子命令和手滑（`ltop repor`）一律 exit 1 并给明确报错——
没有任何东西会"意外落进交互 TUI"; JSON 键不随 `lang` 变，恒英文。

## 时间窗: `watch` + `report`

"盯 10 分钟再汇报"这类任务**不要轮询** `--json`（5 秒节奏 10 分钟约 4 万
token）。录一次，摘要一次:

```bash
ltop watch --duration 10m --proc chrome    # 无头、崩溃安全、免 root
ltop report <watch 打印的路径> --json    # report 只收单个文件
```

`report --json` 返回约 2 KB 的精确数字——窗口总和（末快照累计 − 首快照
累计，跨 pid 重启分段）、峰值速率、活跃时长——不是抽样猜测。实机实测:
同样信息**省 ~97% token**。

```json
{"schema":1,"file":"…","host":"…",
 "window":{"start":…,"end":…,"snapshots":N,"interval_s":S},
 "totals":{"scope":"nic","nic_rx":…,"nic_tx":…,"peak_ts":…,"peak_rx_bps":…,"peak_tx_bps":…},
 "procs":[{"name":"chrome","net_recv":…,"net_sent":…,"peak_rx_bps":…,"peak_tx_bps":…,
           "udp_recv":…,"udp_sent":…,"peak_udp_bps":…,"disk_read":…,"disk_written":…,
           "peak_disk_bps":…,"active_s":…,"alive":true}],
 "conns":[{"remote":"1.2.3.4:443","proto":"tcp","recv":…,"sent":…,"peak_rx_bps":…,
           "peak_tx_bps":…,"duration_s":…,"state":"ESTAB","process":"chrome"}]}
```

`--top N`（默认 5）、`--proc`（子串或含 `*` 的通配）、`--fields
net,udp,disk,conns` 可以再裁。

## 和 `jq` 的分工

watch 产物和 TUI 监控日志就是普通 JSONL——`report` 不聚合的任何切片都是
一条 `jq` 的事（文件同 schema，`report` 两者都读）。窗口数学给 `report`，
任意查询给 `jq`，当前瞬间给 `--json`。

## 读快照——最容易读错的地方

- `meta.udp_helper: true` 表示 Linux eBPF 助手在跑、UDP 每进程计费完整;
  `false`（或 macOS）时 UDP 数字不完整。
- `conns[].process` 为 `null` 是**其他用户**的 socket（Linux）。别断言
  这些连接"与任何进程无关"——只是归不到你的 uid 头上。
- `procs[].disk_*` 只覆盖当前用户的进程。
- `net.interfaces[].session_rx/tx` = ltop 启动以来（可重置）;
  `alive_rx/tx` = 接口启动以来。
- 进程总量是**载荷**字节，接口总量是**网卡**字节——差 10–40% 是设计使然
  （[为什么](ebpf.md#数字怎么读)）。

## 配方（来自打包的 skill）

- **网速慢？** `net.interfaces[]` 按 `rx_bps+tx_bps` 排序; 查 `operstate`
  和 `speed_mbps`; 再看忙接口上 `bps_recv+bps_sent` 最高的 `conns[]`——
  `conns[].process` 就是吃流量的（限你自己的用户）。
- **磁盘忙？** `disk.devices[]` 看吞吐对容量，再按磁盘速率排 `procs[]`
  找写盘的。
- **LAN 清点？** `lan.neighbours[]`——网关已标记、mDNS 主机名、`tracked`
  是置顶设备。
- **"盯 chrome 10 分钟汇报流量"**——上面那对命令，两条都带
  `--proc chrome`。

## 展望

`ltop mcp` 子命令（手写 stdio JSON-RPC，包 `--json` 和 watch+report，后续
加多机 fleet 聚合）在 0.3 路线图上; 上面的 JSON 接口就是它的底座，schema
v1 今天已稳定。

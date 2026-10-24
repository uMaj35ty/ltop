# 数据面

ltop 的每个显示面背后都是同一份序列化快照——**schema v1**，随二进制一起
版本化，脚本和 agent 可以放心依赖。同样的结构体喂给这些入口:

| 入口 | 命令 | 形态 |
|---|---|---|
| 一次性快照 | `ltop --json` | 单个 JSON 对象（不过滤约 8k token） |
| 字段参考 | `ltop --schema` | 随版本更新的机器可读字段表 |
| 监控日志 | `L` / `--log` | JSONL，每 `log_interval_ms`（默认 60 秒）一行 |
| 无头采集 | `ltop watch` | JSONL，每 `--interval`（默认 5 秒）一行 |
| 窗口摘要 | `ltop report` | 人读表格；`--json` 给 ~2 KB JSON |

```bash
ltop --json      # 结构化快照（约 1.2 秒: 连采两轮，速率字段才有值）
ltop --schema    # 权威字段表——随二进制版本化，永不漂移
```

快照覆盖五个域: `meta`（版本、os、arch、`udp_helper` 标志）、`net`
（每接口速率与累计计数）、`procs`（每进程网络+磁盘计费，带 UDP 分解）、
`conns`（逐连接 socket）、`disk`（容量/吞吐）、`lan`（邻居）。值得知道的
语义直接标在输出里——比如 macOS 的 MAC 脱敏（启用 `ltop lan-setup` 后即为真值）。

## 监控日志（TUI）

TUI 里按 `L`，或启动带 `--log`，快照记到
`~/.local/state/ltop/ltop-log-<ts>.jsonl`:

- `log_interval_ms`（默认 60 000）定节奏; `log_tracked_only = true` 只记
  被追踪的进程。
- `log_retention_days`（默认 7）在启动和开关时清理过期文件。

## 无头 watch

```bash
ltop watch --duration 10m --proc chrome   # 完整语法见 `ltop watch --help`
```

- 停止条件: `--duration`（`10s`/`5m`/`2h`）、`--stop` 哨兵文件（出现即消费
  并停止）、SIGINT——后者是优雅停: 最后一份快照落盘。
- 输出默认 `~/.local/state/ltop/ltop-session-<ts>.jsonl`，与 TUI 监控日志
  分开、不受它的保留期清理；自己的 `--retention`（默认 7 天，`0` = 永久）
  在启动时清理。
- 逐行追加，崩溃安全；免 root。
- `--proc <name>` 聚焦模式只记匹配的进程及其连接——**按名追踪**，重启换了
  pid 时间线也不断。
- 写盘量随间隔线性（1 秒 ≈ 8 KB/行 ≈ 28 MB/h；默认 5 秒约 5–6 MB/h）。
  清理靠 `--retention`。

## report

```bash
ltop report <file.jsonl> [--json]
```

读 watch 产物**或** TUI 监控日志（同一 schema），给出窗口摘要: 每进程窗口
总和（跨 pid 重启的窗口差分）、峰值速率、活跃时长、最后存活/退出状态；
每连接的总和/峰值/生命周期（按 inode 键，消失的连接保留最后已知总量）；
网卡范围的全局总量与最忙时段。

过滤: `--top N`（默认 5，按流量）、`--proc`（子串，含 `*` 时通配）、
`--fields net,udp,disk,conns`、`--json` 机器可读。建在其上的 AI 工作流见
[AI 代理](ai.md)。

## 遗留与调试

- `ltop --dump` —— 人读的单轮采样打到 stdout（调试用；要解析请用
  `--json`，dump 会截断对不齐）。
- `ltop --replay [--csv OUT] [--top N]` —— 从旧版 `session-*` 日志（v1，
  仅 procs）重建会话增量并导出 CSV。新格式文件归 `report`，旧文件会被
  拒收并指路到这里。

## 配置

`~/.config/ltop/config.conf` 是纯 `key = value`，应用内设置面板
（`m` → settings）写入，手改安全（重启生效）:

| 键 | 默认 | 含义 |
|---|---|---|
| `lang` | `en` | 界面/消息语言；`zh` 切中文（JSON 键恒英文） |
| `layout` | classic | 启动布局（classic/wide/tall） |
| `tick_ms` | 1000 | 刷新间隔 |
| `graph_net` / `graph_disk` | hybrid / centered | 默认图表样式 |
| `boxes` | 全开 | 可见盒子 |
| `log_interval_ms` | 60000 | 监控日志节奏 |
| `log_retention_days` | 7 | 监控日志保留天数 |
| `log_tracked_only` | false | 只记被追踪的进程 |
| `sweep` / `sweep_interval_s` | true / 60 | LAN UDP knock 扫描开关与节奏 |
| `tombstone_ttl_ms` | 5000 | 关闭连接在 CONNS 留多久 |
| `theme` | omarchy | 主题名（见[主题](themes.md)） |
| `theme_live_follow` | false | 桌面主题变化时运行中的 ltop 跟着变色（同 `--follow-theme`） |
| `proc_expand_default` | false | CONNS 进程组默认展开 |
| `proc_exit_retention_min` | 30 | 退出进程账本 N 分钟后清除（0 = 永久） |
| `conn_filter` / `conn_sort` / `proc_sort` / `lan_sort` | all / proc / net-sum / ip | 默认列表模式 |
| `allow_global_kill` | false | 允许终止其他用户的进程 |
| `net_graph_coupled` | false | NET 图表比例耦合:上下行悬殊时,小方向图按真实比例占图表高度,不再满幅放大 |
| `net_graph_scale_ratio` | 5 | 上面的触发阈值,单位倍(2–20);低于阈值保持普通自动缩放 |

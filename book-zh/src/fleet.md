# Fleet(多机聚合)

`ltop fleet` 在一个终端里回答"我所有机器现在怎么样"。管理机通过 SSH
逐节点拉取 `ltop --json` 快照(schema v1),聚合成一张总览表。节点只需要
装了 ltop(任意 ≥ 0.2.3 版本)和 SSH 密钥访问——不要守护进程、不要
agent、不开端口、不装新软件。

## 配置一个节点

```console
$ ltop fleet setup air
```

doctor 会找到本机公钥,把一次性授权原样交给 `ssh-copy-id`(密码提示属于
ssh 而非 ltop——ltop 全程不经手凭据),验证节点上 `ltop --json` 可用并
报告版本与延迟,最后打印推荐的 *force-command* `authorized_keys` 行——
把密钥锁死为只执行快照命令(见[安全 → Fleet](security.md#fleetssh-多机聚合030-beta))。

## 添加节点

命令行给一次并用 `--save` 落盘:

```console
$ ltop fleet --host air --cmd '/opt/homebrew/bin/ltop --json' --host fedora --save
已保存 2 个节点到 /home/you/.config/ltop/fleet.conf
```

或直接编辑 `~/.config/ltop/fleet.conf`(手写的行在重写时原样保留):

```text
interval = 5          # 轮询间隔(秒)
timeout  = 15         # 单节点拉取时限(秒)
master   = on         # ControlMaster 连接复用
node = air
node = fedora
cmd fedora = /usr/local/bin/ltop --json
```

`cmd <节点>` 与 node 行顺序无关,macOS 上必须注意:非交互 ssh 会话没有
PATH,要给全路径(Homebrew 安装即 `/opt/homebrew/bin/ltop --json`)。

## 跑起来

在终端里 `ltop fleet` 打开它自己的 TUI(与六盒本地监控互相独立):

- **↑/↓** 选择节点(排序变化时选中跟着节点走)
- **enter** 节点详情——逐接口速率(选中接口带 ← 标记)、LAN/磁盘/连接
  计数、按网络流量排的前 5 进程,从该节点最近一次成功快照实时重建
- **s** 排序循环:声明序 → rx → tx → procs → conns(降序,失败节点沉底)
- **p** 暂停/恢复后台轮询
- **r** 立即拉一轮
- **c** 把选中节点最活跃进程的 `ssh <节点> 'kill <pid>'` 复制进剪贴板
  (OSC 52)——ltop 只负责*复制*,执行在你的终端里
- **h** 帮助,**esc** 关弹窗,**q** 退出

表格上方是 `Σ rx` / `Σ tx` 走线,fleet 总量用阻尼峰值归一(瞬涨缓落),
一次突发不会压扁整条历史。失败轮保留该节点上一轮的好数据并在原位解释
原因("未配置 SSH 密钥 — 运行 `ltop fleet setup …`"),一台节点挂了不会
糊掉整个视图。

非交互运行(`--once`,或 stdout 不是终端)改为逐轮打印一张表——无头部署
(cron/systemd user unit)消费的就是这份输出:

```console
$ ltop fleet --once
[t+1794… ] round 1
node      rx     tx   procs conns  ms   top talker
air    4.5KiB 8.9KiB    4    53  2075  ZCode Helper (2.1KiB/s)
```

## 选项

| 旗标 | 含义 |
|---|---|
| `--host <目标>` | 轮询节点,可重复;本次运行覆盖 `fleet.conf` |
| `--cmd <命令>` | 最近一个 `--host` 的远端命令(默认 `ltop --json`) |
| `--save` | 把生效节点表写入 `fleet.conf` |
| `--once` | 单轮、非交互、退出 |
| `--interval <S>` | 轮询间隔秒数(默认 5,最小 1) |
| `--timeout <S>` | 单节点拉取时限秒数(默认 15) |
| `setup <目标>` | 密钥 doctor:装钥、验证、打印硬化行 |

安全边界——只收密钥、零凭据存储、force-command 建议、控制只到复制为
止——见[安全 → Fleet](security.md#fleetssh-多机聚合030-beta)。

## 兼容性

线上格式是 additive-only 契约的 schema v1 快照:0.3.0-beta 管理机可以
混跑 0.2.3 和 0.2.13 的节点,未来节点加字段旧管理机也会忽略。低于
0.2.3(没有 `--json`)的节点在表里原位报告并给升级提示,不会静默失败。

# Changelog

All notable changes to this project are documented in this file.
The format is based on [Keep a Changelog](https://keepachangelog.com/), and this project adheres to [Semantic Versioning](https://semver.org/).

## [0.2.7] - 2026-10-24

0.2.6 双端(macOS 原生 + Lima Linux)全面测评后的集中修复版:苹果端报告
30 项发现全部复核(26 项实锤、3 项行号漂移、0 项不成立),另由二轮对抗
评审补出 8 项新发现,全部合入。

### Fixed

- **`--replay` 对自家产物全面兼容(同根因三连修)**
  - 裸 `--replay` 现在能发现全部三类会话文件:watch 采集
    (`ltop-session-*`)、TUI monlog(`ltop-log-*`)、旧版裸 `session-*`;
    目录解析复用 `log::log_dir`,删除逐字节复制的 `replay::log_dir`
  - 表头同时识别 `watch_start`(watch)与 `session_start`(monlog):
    watch 产物不再显示 host `—`、起始 1970-01-01、间隔 0s
  - 盘挂键兼容 `disk_read`/`disk_written`(现 schema)与旧
    `disk_r`/`disk_w`:回放/CSV 的 Σ读盘/Σ写盘列不再恒 0
  - 新增以 watch 真实输出为 fixture 的端到端回放测试(旧测试只钉旧格式
    fixture,对此类回归永久失明)
- **`--theme <file>` 死旗标复活**:CLI 白名单收录 `--theme` 并补进
  usage;直载主题优先于启动主题恢复(0.2.6 中该旗标直接报
  `unknown option`,README 宣传的功能不可达)
- **`parse_hex` 拒绝非 ASCII 主题值**:损坏/恶意主题文件不再能在 panic
  钩子安装前触发 char boundary panic
- **全局 kill 键 `k` 受 `allow_global_kill` 约束**(默认关):关闭时不再
  能对选中行无确认 SIGTERM——底栏提示本就随开关隐藏,行为终于一致;
  详情页 `k` → 确认路径不受影响
- **Hybrid 图首段以首样本 y 播种**(对齐 Line 分支):降序首样本不再把
  区域轮廓削出凹口;补降序首样本回归测试(旧测试 `[0,100]` 首点恰为 0,
  完全掩盖)
- **网格余量路由修正**:spare 高度发给 CONNS/PROCS 盒的全局下标(旧代码
  把 pair 内列下标当网格行号用,隐藏 CONNS 后余量错发给 DISKS 行)
- **`cargo clippy --all-targets` 回到零警告**:修复重复/丢失的 `#[test]`
  注册——Solid 模式回归测试此前从未执行;另清两处测试代码警告
- **Linux CONNS 采集失败可见化**:netlink dump 被拒(缺 INET_DIAG/UDP_DIAG
  编译、SELinux 策略)时 CONNS 标题亮 ⚠ 徽标并带内核错误码,不再静默
  清空、与"没有连接"不可区分
- **mac CONNS/PROCS 采集失败可见化**:nettop spawn 失败即清空旧流行并亮
  stale 徽标(此前沿用上一轮数据:连接行冻结、速率归零、无任何信号)
- **watch `--stop` 哨兵按 200ms 切片粒度响应**:`--interval 3600` 时
  "出现即停"不再最多晚 1 小时(帮助文本承诺如实兑现)
- **会话文件名加 `-<pid>` 后缀**:同秒双实例不再互相截断(截断者继续写
  旧偏移会产生 NUL 稀疏文件,report 计为 skipped,静默腐蚀);新旧文件名
  解析/清理/发现全兼容
- **report 活跃时长口径修正**:每快照每名字至多计 1——同名多进程
  (chrome/nginx worker)不再按行把 active_s 倍增进程数倍;active_s =
  (samples−1) × interval(首见快照是基线),不再超过窗口墙钟时长
- **replay 最忙时段改用实际 dt 作分母**:录制中暂停/采集停滞的区间不再
  按名义 interval 成倍虚高,与逐进程 peak_net_bps 同口径
- **mac netstat 泄漏**:500ms 预算超时后 SIGKILL 未完成的子进程(此前每
  tick 每卡死接口泄漏一个线程 + netstat 子进程,长会话无上界)
- **LAN 大网段钳制**:host 扫描枚举按 /22 截断——/8 VPN 场景不再构造
  1670 万地址的 Vec + 每周期近死循环;网关归属判断改掩码位运算
- **mac RTM 子网扫描只在 ARP 表被隐私机制拦截时运行**,且 drain 时不再
  清除已有真实 MAC(MAC 列不再随扫描周期闪烁)
- **arp 广播条目按全 f MAC 判定**:/23+/8 网段下 `.255` 是合法主机,
  不再按 IP 末字节误杀
- **双击打开详情补守卫**:CONNS 聚合父行(首击已切换展开)与 LAN 行不再
  弹出 "connection is gone"/空详情;打开前字段游标归零
- **搜索强制展开 CONNS 分组仅在搜索目标是 CONNS 时**:目标是 procs 时
  conns 未被过滤却照样摊平,白降信息密度
- **FS 黑名单补 afp/webdav/glusterfs/ceph/cephfs/lustre/ocfs2**:陈旧
  网络挂载的 statvfs(分钟级阻塞)不再能冻结整个 UI tick
- **watch `--duration` 单位倍乘 checked**:超大值在 release 下不再回绕成
  错误的短时长静默提前结束
- **ltop-bpf 单实例锁提前到 load/attach 之前**,锁文件按输出文件名区分:
  systemd 与手动实例竞态不再瞬态双挂载;写不同快照文件的合法实例可并存

### Changed

- 未开启录制时不再每 tick 构建完整快照(from_app 的全量格式化与 String
  克隆只在 `--log`/`L` 录制时发生;默认 250ms tick 下省一轮热路径分配)
- eBPF `sk_ino` LRU 的已知局限(SLAB 地址复用后的低概率错账)在源码注明,
  功能修复(失效探针或逐包重验)入路线图
- README/book/skill 文档与实现对齐:quickstart 改单文件示例(report 只收
  单文件,通配示例在 ≥2 个文件时报错);crash-safe 措辞收敛为"进程崩溃
  安全(逐行追加)"——无 fsync,断电不保证;`active_s` 口径描述同步

### Internal

- Tests: 140 passing (0.2.6 为 132),`cargo clippy --all-targets` 零警告

## [0.2.6] - 2026-10-23

### Added

- **detail 面板鼠标交互** — 单击字段行移动字段游标(与 ↑↓ 等价),双击字段行
  复制该字段值(OSC 52,ssh 下直达本地剪贴板),滚轮逐字段步进,点击弹窗外
  关闭面板
- **NET 图表比例耦合**(opt-in,默认关)— `net_graph_coupled = true` 后,
  上下行差距达 `net_graph_scale_ratio` 倍(默认 5,可配 2–20)时,小方向的
  图表按真实比例占图表高度(小窗保底 1/2,高图 1/3),头部显示缩放徽标,
  不再把微小的反向流量放大成与主方向相同的波形
- macOS:CONNS 归属名回退链补 kinfo p_comm——proc_pidpath 对 launchd
  常驻进程(如 brew services 的 syncthing)返回 0 时,该进程的连接分组
  不再显示 "?",正确显示进程名

## [0.2.5] - 2026-10-23

### Added

- **Documentation website** — landing page, gallery, skills page, roadmap and
  an mdBook manual (12 chapters, English with a full Chinese mirror),
  deployed to GitHub Pages; the README is slimmed to pitch + quick start with
  the deep details moved to the site (layout, eBPF internals, data plane,
  compatibility matrix, comparison, FAQ)
- **31 Omarchy themes merged into the built-in library** — each theme's
  effective btop palette, selectable in the theme menu as `omarchy-<name>`;
  builtin themes 41 → **72**
- `ltop watch --retention <DAYS>` — purge old capture files at start
  (default 7 days, `0` = keep forever)
- `proc_exit_retention_min` config — evict exited-process ledgers after
  N minutes (default 30, `0` = keeps the old keep-forever behavior)
- `scripts/soak.sh` — long-run memory soak harness (55 min runs, RSS flat)

### Changed

- **macOS NET box**: per-interface `netstat -I` queries now run in parallel
  under a 500 ms total budget, and `awdl0`/`llw0` are excluded (their kernel
  queries stall). `watch --duration 2s` completes in ~3.3 s where it took
  6–13 s; en0 counters verified against `netstat` ground truth

### Internal

- Resource audit closed: every ledger, cache and history is bounded or
  evicted (proc ledger, io_prev, watch files were the three unbounded ones;
  charts, tombstones, monlog retention, eBPF LRU maps and LAN neighbours
  verified bounded); 55-min soak with flat RSS
- Tests: 132 passing on Linux, clippy clean

## [0.2.4] - 2026-10-23

### Added

- **`ltop watch`** — headless capture to schema-v1 JSONL: reuses the TUI
  sampling layer (`--interval`, default 5 s), stops on `--duration`
  (`10s`/`5m`/`2h`), a `--stop` sentinel file (consumed on sight), or SIGINT
  (graceful: the last snapshot lands in the file). Output defaults to
  `~/.local/state/ltop/ltop-session-<ts>.jsonl`, separate from the TUI
  monitoring log and exempt from its retention purge. `--proc <name>` focus
  mode records only matching processes and their connections (tracked by
  name, so a restart that changes the pid keeps the timeline continuous).
  Line-by-line appends are crash-safe; no root required.
- **`ltop report <file>`** — window summary of a watch capture or a TUI
  monitoring log (same schema): per-process window sums (window
  differencing across pid restarts), peak rates (UDP peaks derived from
  row deltas), active time and last alive/exited state; per-connection
  sums/peaks/lifetimes keyed by inode (macOS synthetic flow hash), vanished
  connections keep their last known totals; NIC-scope global totals and the
  peak interval. Filters: `--top N` (default 5, by traffic), `--proc`
  (substring, or glob when it contains `*`), `--fields
  net,udp,disk,conns`, and `--json` for a ~2 KB machine-readable summary
  (a 10-minute watch costs ~40k tokens to poll via `--json` vs ~1k as a
  report — about 97% fewer). Old `session-*` logs (v1, procs-only) are
  rejected with a pointer to `--replay`; arbitrary queries stay with `jq`.

### Removed

- **`loli` alias retired** — the out-of-the-box `loli` symlink (a leftover
  from the original *localist top* naming) is no longer shipped: release
  tarballs, the Homebrew formula and the AUR package stub all drop it.
  The invoked-name mechanism stays: `ln -s ltop <name>` gives any alias
  version-correct `--help`. Existing installed symlinks keep working
  (brew upgrade cleans the formula-owned one); nothing is uninstalled
  retroactively.

### Changed

- Snapshot structs now derive `Deserialize` (report reads back what
  watch/monlog wrote); `meta.version/os/arch` became `String` (same JSON).
- **CLI messages are bilingual**: English by default; setting `lang = zh`
  in `config.conf` switches usage text, errors, finalize messages and the
  report/replay tables to Chinese — consistently across `watch`, `report`,
  `--replay` and the monitoring-log notice (previously the new subcommands
  printed Chinese-only while the TUI `--help` was English-only). The
  privileged helper `ltop-bpf` (journal-facing) and all JSON output keys
  stay English.

## [0.2.3] - 2026-10-23

### Added

- **AI-friendly interfaces**: `ltop --json` emits a schema-versioned
  structured snapshot (all five domains, rates included, platform semantics
  like macOS MAC sanitization marked); `ltop --schema` is the versioned
  machine-readable field reference; an agent skill
  (`docs/skills/ltop-observe/SKILL.md`) packages diagnostic recipes.
- **Monitoring log v2**: full-domain JSONL snapshots sharing the `--json`
  schema (was PROCS-only); `log_tracked_only` filters the procs array.

### Changed

- Added `serde`/`serde_json` dependencies for the snapshot/log serialization.

- **OSC 52 copy in the detail popup**: ↑↓/jk moves a field cursor, `y`
  copies the selected value to the terminal clipboard (works over SSH, no
  X11/Wayland dependency); hint row shows `[y] copy`.
- **Home/End/g/G jump**: focused lists jump to first/last row (top/bottom
  for scroll-only boxes); guarded against the SSH ghost-modifier sequences.
- **Alive Σ in the NET box**: since-interface-up totals shown beside the
  resettable session Σ (the underlying counters were already collected).
- **GraphMode::Block**: seamless half-block bars as a non-default graph
  style (graphs menu only; the `n` cycle keeps the original five).
- **Renamed** the monitoring log to `ltop-log-<ts>.jsonl` (old
  `session-*` files still read and purged) and the module to `log`.

### Fixed

- mac detail popup showed `-` for exe/cwd: now sourced from
  `proc_pidpath` / `PROC_PIDVNODEPATHINFO`.

## [0.2.2] - 2026-10-22

### Fixed

- Help panel showed a hard-coded "localist top v0.1.0"; the version now
  follows the build (`CARGO_PKG_VERSION`) and the branding is consistent
  ("ltop") across the help panel, `--help`, replay output and doc comments.

## [0.2.1] - 2026-10-22

### Added

- **macOS support (Apple Silicon & Intel)** — all six boxes run natively:
  PROCS and CONNS poll `nettop` (per-process rows and per-socket flows,
  TCP and UDP in one spawn), CONNS attribution scans the caller's own
  sockets via libproc, DISK reads IOKit IOMedia statistics plus per-process
  rusage disk I/O, LAN parses `arp -an` and the default route. Verified on
  hardware (macOS 27, MacBook Air). macOS has no eBPF: counters are
  cumulative per live socket, so traffic completing within one sampling
  tick is not counted; other users' connections are not attributed (`n/a`).
- Release tarballs for `aarch64-darwin` and `x86_64-darwin` (ltop + loli
  symlink; ltop-bpf stays Linux-only), CI runs the full suite on a macOS
  runner.

## [0.2.0] - 2026-10-21

### Added

- **aarch64 (arm64) Linux support** — the eBPF probes gained an arm64 pt_regs
  branch (syscall args in x0-x7, retval in x0, libbpf-style `__TARGET_ARCH_*`
  defines passed by build.rs), release tarballs ship for both
  `x86_64-linux` and `aarch64-linux`, CI runs the full suite natively on an
  arm runner, and the architecture guard now accepts aarch64.
- Verified end-to-end under QEMU aarch64 emulation (Ubuntu 24.04 arm64,
  kernel 6.8): all ten probes attach and per-pid/per-inode accounting matches.

## [0.1.3] - 2026-10-21

### Changed

- Internal paths renamed for consistency: config `~/.config/ltop/`, state &
  logs `~/.local/state/ltop/`, eBPF snapshot `/run/ltop/udp.json` (was
  `localist-top` / `lanmon`). Existing config and monitoring logs migrate
  automatically on first start; the GitHub v0.1.2 release is superseded by
  this one.

## [0.1.2] - 2026-10-21

### Fixed

- wide/tall layouts: the two charts could balloon to 4/5 of the screen height
  on tall terminals when the lists had little content; the graph area is now
  capped at half the screen and surplus rows flow to the lists.
- Release binaries are now built on a glibc 2.35 baseline (ubuntu:22.04) —
  the 0.1.1 binaries required GLIBC_2.39 and failed on older distributions.
- `ltop --dump | head` no longer panics on pipe close (SIGPIPE restored).

### Added

- Live Omarchy theme following, opt-in: `--follow-theme` (or
  `theme_live_follow = true` in the config; off by default). While in the
  `omarchy` follow mode, re-writing the desktop theme (e.g.
  `omarchy-theme-set`) hot-reloads ltop's palette within one tick — verified
  on the real desktop across three theme switches.

### Changed

- The crates.io crate is **`ltop`** (`cargo install ltop`). A one-off
  `localist-top` crate with the same 0.1.2 source was published briefly and is
  yanked; on-disk config/log paths are unchanged.

## [0.1.1] - 2026-10-21

First public release.

### Added

- Six-box btop-style TUI (NET / LAN / DISK I/O / DISKS / CONNS / PROCS) with three layouts (classic / wide / tall), 41 embedded btop themes plus Omarchy theme following, and an English/Chinese bilingual interface.
- eBPF network accounting for TCP+UDP via the privileged `ltop-bpf` helper: per-pid and per-socket-inode ledgers, kprobe-attached send/recv probes (v4+v6), secondly snapshots to `/run/lanmon/udp.json`; the unprivileged TUI diffs the snapshots. Degrades silently when the helper is absent.
- LAN neighbour discovery (passive ARP + UDP knock + mDNS/NetBIOS/reverse-DNS hostname resolution), per-neighbour connection count/rate/Σ byte display, and `enter` to pin a neighbour to the top while filtering CONNS to that peer.
- CONNS process grouping (`proc` sort): same-name processes grouped with `▸/▾` collapse/expand via `enter`/`←`/`→`/mouse, search auto-expands, group state persisted in settings.
- Single-focus model: `p`/`c`/`l` light procs/conns/lan exclusively (keys and mouse clicks alike); selection follows the entity across re-sorts in all three list boxes.
- Monitoring log & replay: `L` records per-pid ledgers to JSONL, `--replay` reports session deltas, `--csv` export; `--dump` for one-shot diagnostics.
- `cargo test` suite: 95 tests (unit, rendered-frame TestBackend, live netlink+snapshot integration); clippy clean across all targets.
- `-h`/`--help` and `-V`/`--version` on the shipped binaries (`ltop`, `ltop-bpf`); help follows the invoked name, so the bundled `loli` symlink (alias, shipped in release tarballs and the AUR package) reports as itself.

### Notes

- x86_64 Linux only; a launch check aborts with a clear message on other architectures.
- Data scopes differ by design (PROCS = payload via eBPF; NET box = NIC bytes incl. headers/retransmits) — see the in-app help panel and README "Data scopes".

[0.1.2]: https://github.com/uMaj35ty/ltop/releases/tag/v0.1.2
[0.1.1]: https://github.com/uMaj35ty/ltop/releases/tag/v0.1.1

# Changelog

All notable changes to this project are documented in this file.
The format is based on [Keep a Changelog](https://keepachangelog.com/), and this project adheres to [Semantic Versioning](https://semver.org/).

## [0.2.7] - 2026-10-24

macOS bug fixes.

### Fixed

- **`--replay` now understands every session log the app itself writes**
  - Bare `--replay` discovers all three session-file prefixes: watch captures
    (`ltop-session-*`), TUI monlog (`ltop-log-*`) and legacy bare `session-*`;
    the state dir now comes from `log::log_dir`, deleting the byte-identical
    `replay::log_dir` copy
  - Headers accept both `watch_start` (watch) and `session_start` (monlog):
    watch captures no longer show host `—`, start 1970-01-01, interval 0s
  - Disk keys accept `disk_read`/`disk_written` (current schema) alongside
    legacy `disk_r`/`disk_w`: the Σ read/Σ written columns no longer stay 0
  - New end-to-end test feeding real `watch` output through `--replay` (the
    old tests pinned legacy-format fixtures only and could never catch this)
- **`--theme <file>` actually works again**: the flag is whitelisted in the
  CLI and documented in usage; a file passed this way takes priority over the
  saved-theme restore (in 0.2.6 the flag exited with `unknown option` while
  the README advertised it)
- **`parse_hex` rejects non-ASCII theme values**: a corrupt or hostile theme
  file can no longer trigger a char-boundary panic before the panic hook is
  installed
- **Global kill key `k` respects `allow_global_kill`** (default off): with
  the toggle off, the key can no longer SIGTERM the selected row without
  confirmation — the footer hint already hid with the toggle, behaviour now
  matches; the detail-panel `k` → confirm path is unaffected
- **Settings panel row misalignment**: inserting the recording row shifted
  every handler below it by one — cycling "recording" changed the log
  interval, cycling "global kill key" changed the tombstone TTL, and the last
  row (conns proc groups) did nothing. All 14 rows now act on their own
  value, recording (same path as the `L` key) and global kill are operable
  from settings, and a regression test pins the mapping
- **Hybrid graph seeds the first segment from the first sample** (matching
  the Line branch): a descending first sample no longer notches the area
  outline; regression test added (the old `[0,100]` fixtures started at 0
  and masked it)
- **Grid spare-height routing fixed**: spare rows go to the CONNS/PROCS boxes
  themselves (the old code used a within-pair column index as a grid row
  index, sending spare height to the wrong box once CONNS was hidden)
- **`cargo clippy --all-targets` back to zero warnings**: a duplicated and a
  missing `#[test]` attribute fixed — the Solid-mode regression test had
  never been running; two more test-code warnings cleaned up
- **Linux CONNS degradation is visible**: a rejected netlink dump (missing
  INET_DIAG/UDP_DIAG config, SELinux policy) lights a ⚠ badge with the
  kernel errno in the CONNS title instead of silently emptying the box
- **mac CONNS/PROCS degradation is visible**: a failed nettop spawn clears
  the stale flows and lights a stale badge (it used to freeze last round's
  rows with zeroed rates and no signal)
- **`watch --stop` sentinel responds at slice granularity**: with
  `--interval 3600`, "stop as soon as it appears" no longer lags up to a full
  hour (help text now holds)
- **Session file names get a `-<pid>` suffix**: two instances started within
  the same second no longer truncate each other's file (the truncated writer
  kept writing at its old offsets, producing NUL-sparse files that report
  counts as skipped); parsers, purge and discovery accept old and new names
- **report active-time caliber corrected**: a name counts once per snapshot —
  same-named multi-process sets (chrome/nginx workers) no longer multiply
  `active_s` by the worker count; `active_s` = (samples − 1) × interval
  (the first sighting is the baseline), so it can no longer exceed the
  window's wall-clock span
- **replay busiest intervals divide by actual dt**: paused/stalled stretches
  no longer inflate the rate by the pause factor, aligning the column with
  the per-process peak (`peak_net_bps`) caliber
- **mac netstat leak fixed**: unfinished `netstat -I` children are SIGKILLed
  when the 500 ms budget expires (previously every stalled interface leaked
  one thread + child per tick, unbounded over long sessions)
- **LAN sweeps clamp huge subnets**: host enumeration truncates to a /22 —
  a /8 VPN no longer builds a 16.7-million-address Vec and near-deadloops the
  sweep thread each interval; gateway membership now uses mask arithmetic
- **mac RTM subnet scan runs only when the ARP table is privacy-blocked**, and
  no longer clears MACs it already has (the MAC column stopped flickering on
  every scan cycle)
- **ARP broadcast entries detected by the all-f MAC**: on /23+/8 subnets
  `.255` is a legal host and is no longer dropped by the last-octet
  heuristic
- **Double-click detail guarded**: aggregate parent rows (already toggled by
  the first click, id is a hash) and LAN rows no longer open a bogus
  "connection is gone" / empty detail popup; the field cursor resets on open
- **Search expands CONNS groups only when CONNS is the search target**: a
  procs-targeted search no longer flattens unfiltered CONNS groups
- **FS blacklist adds afp/webdav/glusterfs/ceph/cephfs/lustre/ocfs2**: a
  stale network mount's minute-long `statvfs` can no longer freeze the whole
  UI tick (disk sampling runs on the UI path)
- **`watch --duration` unit multiplication is checked**: an oversized value
  no longer wraps into a wrong short duration and exits silently (release
  builds have no overflow checks)
- **ltop-bpf single-instance lock moves ahead of load/attach**, keyed by the
  output file name: a systemd/manual race no longer transiently double-attaches
  every probe, and two instances writing different snapshot files can coexist
- **Chinese translations completed** for the confirm kill/restart dialogs,
  help panel intro and keys header, zoom footer, detail panel hints
  (track/copy/close/kill/start) and process state fields (running/exited)

### Changed

- The full snapshot is only built while recording: `from_app`'s per-connection
  formatting and String clones no longer run every tick with `--log` off
  (a wasted full round of allocations at the default 250 ms tick)
- The eBPF `sk_ino` LRU's known limitation (low-probability mis-attribution
  after SLAB address reuse) is documented in source; the functional fix
  (invalidation probe or per-packet revalidation) is tracked in the roadmap
- Docs aligned with the implementation: quickstart uses a single-file report
  example (report takes exactly one file — the glob example errored on its
  own once a directory held two logs); crash-safe wording narrowed to
  process-crash-safe (per-line appends, no fsync — power loss is not
  covered); the `active_s` description matches the new caliber

### Internal

- Tests: 141 passing (132 in 0.2.6), `cargo clippy --all-targets` zero
  warnings

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

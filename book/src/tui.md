# The TUI

Six boxes, one job each. `Tab` cycles three layouts — **classic** (charts
left, tables right), **wide** (charts across the top, tables below), **tall**
(one column) — and boxes toggle with `1`–`6`, collapsing cleanly when hidden.
`z` zooms the focused box full-screen; `esc` restores. `space` freezes the
frame; bytes transferred while paused are back-filled on resume, so pausing
never loses accounting.

![Classic layout](../assets/screenshots/classic-six-boxes-small.webp)

## The six boxes

### 1 — NET

Per-NIC real-time throughput as braille area charts (rx/tx with separate
gradients), plus link state, speed, duplex, MTU and addresses. Cumulative
traffic shows twice: since ltop started (resettable) and since the interface
came up. `n` switches the chart mode; the NIC cycles with a click on the
header.

### 2 — LAN

Same-subnet devices discovered automatically (how: [LAN discovery](lan.md)):
IP, MAC, mDNS hostname, interface; gateways marked. Each neighbour shows its
live connection count, current rate and accumulated bytes — the numbers link
into CONNS, so "how much is going to that box" is one glance, and one `enter`
pins the neighbour and filters CONNS to that peer.

### 3 / 4 — DISK I/O and DISKS

Read/write rate charts (deduplicated across stacked devices — `nvme0n1` and
its `dm-0(root)` child are one sample, not two) beside a physical device tree
with capacity bars and live per-device rates. `d` expands the mount tree;
`i` switches the chart mode.

### 5 — CONNS

Every socket on the system. TCP connections show true per-connection B/s and
totals; UDP "connections" are eBPF-metered (Linux, via the
[eBPF helper](ebpf.md)) and get the same columns. Rows carry protocol tags
(`tcp`/`udp`, `lan` for local-peers), and closed connections linger as grayed
**tombstone rows** for 5 seconds (`tombstone_ttl_ms` configurable) so a
finished transfer is still readable. `f` cycles the filter (all → lan → tcp →
udp), `o` cycles sorts — including **proc** grouping, which folds same-name
processes into an expandable group head (`▸ firefox ×17` with the group Σ;
`enter` on the head collapses/expands, search hits auto-expand).

![Tall layout — grouped connections with group totals and a tracked row](../assets/screenshots/tall-process-groups-small.webp)

### 6 — PROCS

Per-process **session totals** for network (TCP+UDP — short-lived connections
are never missed by sampling, because eBPF counts bytes at the syscall) and
disk I/O, plus live rates. Exited processes stay grayed out with their
captured command line; `s` relaunches one, or `enter` opens the detail panel
for a copy of the exact command. `r` cycles sorts (net-sum → net-rate →
disk-sum → disk-rate → name). Exited ledgers are evicted after 30 minutes by
default (`proc_exit_retention_min`, `0` = keep forever).

## Focus, selection, detail

- `p` / `c` / `l` single-focus PROCS / CONNS / LAN: the chosen box fills the
  screen's attention (the others dim to stubs). `esc` clears.
- Arrow-key selection **follows** its process/connection/neighbour across
  re-sorts and group collapse — the row you watch stays the row you watch.
- `enter` on a conns/procs row opens the **detail panel**: processes show
  state, uptime, full captured command line, exe/cwd, RSS and a UDP
  breakdown; connections show protocol, owner, lifetime, totals and rates.
  Inside the panel: `space`/`t` pin-tracks the row (tracked rows stay on
  top), `k` kills, `s` relaunches, `↑↓` moves a field cursor and `y` copies
  the selected value to the clipboard via OSC 52 (works over SSH, no X11
  dependency).
- `enter` on a LAN row pins the neighbour (red highlight, stays on top) and
  filters CONNS to that peer; repeat to unpin, `esc`/`f` clear.

![Wide layout — detail panel with captured command line, UDP breakdown and the theme menu](../assets/screenshots/wide-detail-panel-small.webp)

## Search

`/` searches by name substring across processes and connections (`Tab`
switches the target box, `enter` applies, `esc` clears). Matches also drive
sorts — a filtered PROCS view is still sorted by your chosen mode.

## Kill-guard rails

`k` kills the selected process (or closes the socket). Killing *other users'*
processes requires `allow_global_kill = true` in the config; the default
denies it.

## Monitoring log

`L` (or start with `--log`) records per-process ledgers to
`~/.local/state/ltop/ltop-log-<ts>.jsonl` at `log_interval_ms` (default 60 s),
sharing the same schema-v1 snapshot format as `watch`; retention
(`log_retention_days`, default 7) purges old files at startup. Replay and
export: `--replay [--csv OUT]`. Details in [The data plane](data.md).

## Full key bindings

| Key | Action |
|---|---|
| `q` / `Ctrl-C` | quit |
| `space` | pause/resume sampling (back-fill on resume) |
| `/` | search (Tab switches box, enter applies, esc clears) |
| `m` | menu: themes / settings / reset / graphs / help / quit |
| `Esc` | clear focus & selection; close panels |
| `1`–`6` | toggle boxes: net, lan, disk i/o, disks, conns, procs |
| `Tab` | layout classic → wide → tall |
| `p` / `c` / `l` | single-focus procs / conns / lan |
| `r` | procs sort cycle |
| `f` / `o` | conns filter cycle / sort cycle |
| `enter`, `←→` on a group head | collapse / expand process group |
| `d` | expand/collapse disk mount tree |
| `e` | lan sort: ip → hostname → mac → iface |
| `n` / `i` | net / disk chart mode |
| `↑↓` `PgUp` `PgDn` / wheel | move selection; wheel scrolls the list under the cursor; click selects |
| `enter` on a lan row | pin neighbour + filter CONNS to that peer |
| `enter` on a conns/procs row | detail panel (track `t`/`space`, kill `k`, relaunch `s`, copy `y`) |
| `z` | zoom the focused box full-screen |
| `Home`/`End`/`g`/`G` | jump to first/last row |
| `L` | toggle the monitoring log |
| `s` | LAN sweep on/off (main screen; in detail panels `s` relaunches) |
| `+`/`-` | refresh interval ±250 ms |

Settings worth knowing (all in `m` → settings, persisted to
`~/.config/ltop/config.conf`): refresh `tick_ms` (default 1000), tombstone
TTL, the recording toggle (same as the `L` key), log interval/retention,
sweep interval, default sort/filter modes, `theme`, `lang`,
`proc_expand_default` (groups expanded by default), `proc_exit_retention_min`,
`allow_global_kill` (exposes the `k` key; default off), `allow_global_log`
(exposes the `L` record hotkey; default off).

# Introduction

**ltop** (localist top) is a terminal monitor for a single machine. It answers
three questions btop, nethogs and iftop each answer only in part:

1. **Where does the traffic go?** — who talks to whom, on which interface, at
   what rate, with true per-connection and per-process byte accounting
   (TCP **and** UDP).
2. **What are the processes doing?** — who eats bandwidth and disk right now,
   and who did over the whole session, including processes that already exited.
3. **What does the LAN look like?** — which devices are on the local network,
   which is the gateway, and what traffic flows to each of them.

![Classic layout — six boxes with live LAN neighbours and grouped connections](../assets/screenshots/classic-six-boxes.png)

## How it works

Six boxes, one job each (NET, LAN, DISK I/O, DISKS, CONNS, PROCS), rendered
with [ratatui] in a btop-style braille-chart aesthetic. The main program runs
**unprivileged** — no root, no setuid, no daemon. On Linux an *optional*
privileged helper (`ltop-bpf`, a tiny eBPF collector) unlocks per-process and
per-connection byte accounting for TCP and UDP; without it everything else
keeps working. On macOS the same six boxes read Apple-native sources (nettop,
libproc, IOKit, the route socket). Every display surface is backed by one
machine-readable data plane: `ltop --json` emits a schema-v1 snapshot,
`ltop watch` records it headlessly, and `ltop report` sums a window of it —
which is what makes ltop usable **by AI agents** (~97 % fewer tokens than
polling, see [AI agents](ai.md)).

[ratatui]: https://ratatui.rs

## Highlights

- **True per-connection accounting** — every TCP connection shows live B/s and
  cumulative totals; UDP connections are eBPF-metered and get the same
  columns. Closed connections linger as 5-second tombstones so short bursts
  stay readable.
- **Per-process totals that survive restarts** — PROCS accumulates network
  (TCP+UDP) and disk I/O per process for the whole session; exited processes
  stay grayed out with their captured command line and can be relaunched with
  one key.
- **LAN neighbours with traffic** — passive ARP plus a periodic UDP-knock
  sweep finds same-subnet devices (IP, MAC, mDNS hostname, interface); each
  neighbour carries its live connection count, rate and accumulated bytes,
  and `enter` pins it and filters CONNS to that peer.
- **Headless watch + report** — record a time window to crash-safe JSONL, then
  get exact window sums, peak rates and active time per process/connection as
  a table or ~2 KB of JSON.
- **72 embedded btop themes** — any btop theme works as-is; on Omarchy the
  palette can follow the desktop theme, optionally re-coloring a *running*
  ltop (`--follow-theme`). UI in English or Chinese (`lang = zh`).
- **Three layouts, one keystroke apart** — `Tab` cycles classic / wide / tall;
  boxes toggle with `1`–`6`; `z` zooms the focused box full-screen.

## Where to go next

- [Quick start](quickstart.md) — install and the keys that matter, in two
  minutes.
- [Installation](installation.md) — all channels, platform requirements and
  the eBPF helper deployment.
- [The TUI](tui.md) — the full surface: boxes, layouts, key bindings.
- [AI agents](ai.md) — the JSON snapshot, watch + report, and the bundled
  agent skill.

## Status

ltop is MIT-licensed, developed daily on Omarchy (Arch Linux), and verified
per release in QEMU/KVM across five Linux distributions plus on macOS
hardware — see [Compatibility](compat.md). Distribution channels: [Homebrew],
[crates.io], and [GitHub Releases] tarballs (x86_64 + aarch64); an AUR
`ltop-bin` package is ready and pending the AUR reopening registration.

[Homebrew]: https://github.com/uMaj35ty/ltop#readme
[crates.io]: https://crates.io/crates/ltop
[GitHub Releases]: https://github.com/uMaj35ty/ltop/releases

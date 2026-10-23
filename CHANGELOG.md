# Changelog

All notable changes to this project are documented in this file.
The format is based on [Keep a Changelog](https://keepachangelog.com/), and this project adheres to [Semantic Versioning](https://semver.org/).

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

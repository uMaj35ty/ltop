# Changelog

All notable changes to this project are documented in this file.
The format is based on [Keep a Changelog](https://keepachangelog.com/), and this project adheres to [Semantic Versioning](https://semver.org/).

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

[0.1.1]: https://github.com/uMaj35ty/ltop/releases/tag/v0.1.1

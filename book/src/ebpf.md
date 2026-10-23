# eBPF accounting

## Why it exists

Two hard limits in the usual sources:

- **netlink/`/proc` offers no per-socket byte counters for UDP.** TCP shows
  byte queues, UDP shows nothing usable per socket.
- **Per-socket TCP sampling loses short-lived connections.** Anything that
  completes within one sampling tick is invisible — which is exactly the
  traffic you often want to catch (DNS, small requests, chatty helpers).

`ltop-bpf` solves both by counting bytes where they happen: kprobes on the
send/recv syscalls (v4 **and** v6) accumulate payload bytes per **pid** and
per **socket inode** in kernel maps. Both ledgers feed the same UI, so
PROCS gets per-process totals that cannot miss short-lived connections, and
CONNS can attribute bytes to individual UDP "connections" like it already
does for TCP.

## Architecture

```
ltop-bpf (root, systemd)                 ltop (unprivileged TUI)
  kprobes send/recv v4+v6                  reads /run/ltop/udp.json
  pid ledger ─┐                            every tick, diffs the snapshot
              ├─ /run/ltop/udp.json  ──►   (saturating per-instance diff)
  inode ledger┘   rewritten every second   merges deltas into its ledgers
```

The privileged collector and the unprivileged UI communicate through one
world-readable snapshot file that is rewritten every second. The TUI never
needs privileges, and the helper never needs to know about terminals.

## Deploying

```bash
sudo cp ltop-bpf /usr/local/bin/
sudo cp ltop-bpf.service /etc/systemd/system/
sudo systemctl enable --now ltop-bpf
```

- **Smoke test:** `sudo ltop-bpf --once` samples once, prints, exits;
  `sudo bash scripts/udp-smoke.sh` generates accountable UDP traffic and
  verifies the snapshot end-to-end; `python3 scripts/snap-diff.py` prints
  per-pid raw deltas.
- **Upgrades:** re-deploy the helper together with the TUI (see the
  [FAQ](faq.md#per-process-accounting-zeroed-after-an-upgrade) for why).
- **Building from source** needs a clang with the `bpf` target; prebuilt
  tarballs carry a compiled-in object.

## Keeping the data clean

Guard rails keep kernel counters honest over a long-running session:

1. **LRU maps with self-eviction.** The pid and socket maps are
   `BPF_MAP_TYPE_LRU_HASH` (16 384 entries each): idle pids and closed
   sockets evict themselves, so a busy multi-thousand-process box cannot
   exhaust the maps.
2. **Per-instance identity + saturating diff.** Every helper instance
   (guarded by a `flock` single-instance lock) stamps its snapshots with a
   unique id; the TUI re-baselines whenever the id changes and diffs with
   saturating arithmetic — pid/socket reuse or a daemon restart with a stale
   file can never produce negative or garbage deltas.
3. **CO-RE, no per-kernel builds.** The single struct-coupled part (walking
   `sk` to the socket inode) uses CO-RE relocations resolved against the
   kernel BTF at load time. Kernels without BTF still work (Ubuntu 22.04's
   baseline is verified) — field offsets simply resolve at load.

Probes detach automatically on exit — a crashed collector never leaves
kprobes behind — and register reads pass a 16 MiB value gate so a wild
`len` cannot poison the ledgers.

## What the numbers mean

The eBPF ledgers count **payload bytes** seen at the syscall. The NET box
counts **interface bytes** (driver-level, every packet). The two scopes
differ *by design* — headers, retransmits, and LAN chatter the process never
sent through a syscall all live only in the interface numbers — so PROCS
totals being 10–40 % below the NET box is the expected shape, not a bug. See
the [FAQ entry](faq.md#procs-totals-are-lower-than-the-net-box) for
the exact breakdown.

## When the helper is absent

`ltop` runs unprivileged by default and never requires the helper: without
it the UDP per-process columns are simply absent (TCP still works from
socket sampling) and `meta.udp_helper` reads `false` in `--json`. On macOS
there is no eBPF and no helper at all — see
[Compatibility → macOS](compat.md#macos) for the Apple-native accounting and
its documented differences.

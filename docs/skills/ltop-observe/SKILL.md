---
name: ltop-observe
description: Observe system state (network, disk, processes, LAN neighbours) via ltop's structured JSON snapshot. Use when diagnosing "why is the network slow", "what is writing the disk", "which process eats bandwidth", or "what devices are on this LAN".
---

# ltop system observation

`ltop` is a btop-styled monitor with an AI-friendly JSON snapshot covering
five domains: per-interface network rates, per-process byte accounting
(network + disk, plus a UDP breakdown), per-connection sockets, disk
capacity/throughput, and LAN neighbours.

## When to use

- Network diagnostics: who/what is transferring, on which interface, at
  what rate — including per-connection sockets.
- Disk diagnostics: which process is doing I/O, device capacity/throughput.
- LAN survey: which devices are on the local network (with mDNS hostnames),
  who is the gateway.
- Any "the system feels slow" triage: rates + cumulative bytes separate
  "busy now" from "historically heavy".

## Commands

```
ltop --json      # structured snapshot (schema v1; takes ~1.2s: two
                 # sampling rounds so rate fields are populated)
ltop --schema    # machine-readable field reference for --json
```

Always prefer `--json` over `--dump` (the human-readable dump truncates and
misaligns values). `--schema` is the authoritative field list — it is
versioned with the binary and never drifts from it.

## Reading the snapshot

- `meta.udp_helper`: when `true`, the Linux eBPF helper is live and UDP
  per-process accounting is complete. When `false` (or on macOS) UDP
  per-process numbers are partial.
- `net.interfaces[].rx_bps/tx_bps`: instantaneous bytes/s;
  `session_rx/session_tx` = since ltop started (resettable);
  `alive_rx/alive_tx` = since the interface came up (not resettable).
- `conns[].process` is null for sockets owned by other users (Linux) —
  do not claim those connections are unrelated to a process, they are just
  not attributable to you.
- `lan.neighbours[].mac` is `null` on macOS: the system sanitizes neighbour
  MACs for non-Apple-signed binaries (independent of any permission toggle).
- `procs[].disk_*` covers only processes owned by the current user.

## Diagnostic recipes

- **Network slow?** `ltop --json` → sort `net.interfaces[]` by
  `rx_bps+tx_bps`; check `operstate` and `speed_mbps`; then look at
  `conns[]` with the highest `bps_recv+bps_sent` on the busy interface —
  `conns[].process` names the eater (your user only).
- **Disk busy?** `disk.devices[]` for device throughput (read_bps/write_bps
  vs capacity), `procs[]` sorted by `disk_bps_read+disk_bps_written` for the
  writer (own-uid processes on macOS).
- **LAN inventory?** `lan.neighbours[]` — gateway is flagged, hostnames are
  mDNS-resolved, `tracked` marks pinned devices.
- **Process accounting caveats**: `procs[].net_*` is payload bytes seen by
  the eBPF helper (Linux) or live-socket accounting (macOS — transfers that
  complete within one sampling tick are missed); `disk_*` is own-uid only.

## Environment notes

- Linux: per-process/per-connection byte accounting requires the privileged
  helper (`sudo systemctl enable --now ltop-bpf`); without it, check
  `meta.udp_helper` — UDP numbers degrade.
- macOS: neighbour MACs are hidden by the system for non-Apple-signed
  binaries regardless of the Local Network toggle; interface byte counters
  come from `netstat` (live) — see docs/custom-theme.md sibling notes.

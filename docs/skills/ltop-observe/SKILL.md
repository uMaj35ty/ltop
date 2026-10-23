---
name: ltop-observe
description: Observe system state (network, disk, processes, LAN neighbours) via ltop's structured JSON snapshot; watch a system over a time window with ltop watch + report. Use when diagnosing "why is the network slow", "what is writing the disk", "which process eats bandwidth", "what devices are on this LAN", or "watch chrome for 10 minutes and report traffic".
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
ltop watch ...   # headless capture to schema-v1 JSONL (see below)
ltop report <f>  # window summary of a capture/monlog file (see below)
```

Always prefer `--json` over `--dump` (the human-readable dump truncates and
misaligns values). `--schema` is the authoritative field list — it is
versioned with the binary and never drifts from it.

Command-surface behavior worth knowing:

- Subcommands and flags validate strictly: unknown options and typos
  (e.g. `ltop repor`) exit 1 with a clear message — nothing ever falls
  into the interactive TUI by accident.
- Human-readable output (help text, errors, the report table) follows the
  config language: English by default, Chinese when `lang = zh` in
  `~/.config/ltop/config.conf`. **JSON output keys are always English** —
  parse `--json` / `report --json` programmatically, never scrape the
  human tables.
- `ltop-bpf` is the separate privileged collector (Linux only, runs as
  root via systemd; `--once` is its smoke test). `--replay` analyses
  legacy v1 session logs only.

## Time-window monitoring (watch + report)

For "watch this for N minutes and report" tasks do **not** poll `--json`
(~40k tokens for 10 minutes at 5 s cadence). Record once, summarize once:

```bash
ltop watch --duration 10m --proc chrome     # record; --out/--stop optional
ltop report <file.jsonl> --json             # ~2 KB window summary
```

- `watch` appends full schema-v1 snapshots as JSONL lines (crash-safe,
  no root); `--interval <s>` sets the cadence, `--duration`/a `--stop`
  sentinel file/SIGINT end it. `--proc <name>` keeps only matching
  processes (by name — survives pid changes) and their connections.
  The capture file starts with a `{"watch_start":…}` header line; skip it.
- `report` prints a human table; `--json` gives this shape:

```json
{"schema":1,"file":"…","host":"…",
 "window":{"start":…,"end":…,"snapshots":N,"interval_s":S},
 "totals":{"scope":"nic","nic_rx":…,"nic_tx":…,"peak_ts":…,"peak_rx_bps":…,"peak_tx_bps":…},
 "procs":[{"name":"chrome","net_recv":…,"net_sent":…,"peak_rx_bps":…,"peak_tx_bps":…,
           "udp_recv":…,"udp_sent":…,"peak_udp_bps":…,"disk_read":…,"disk_written":…,
           "peak_disk_bps":…,"active_s":…,"alive":true}],
 "conns":[{"remote":"1.2.3.4:443","proto":"tcp","recv":…,"sent":…,"peak_rx_bps":…,
           "peak_tx_bps":…,"duration_s":…,"state":"ESTAB","process":"chrome"}]}
```

  Sums are window differencing (last cumulative − first, segmented across
  pid restarts); peaks are the max per-row rate; `active_s` =
  (snapshots-seen − 1) × interval (first snapshot is the baseline; a name
  is counted once per snapshot even when several pids share it). `--top N` (default 5), `--proc` filter
  (substring or `*` glob), `--fields net,udp,disk,conns` select groups.
  Totals are NIC-scope — larger than process payload sums by design.
- Arbitrary queries: the file is plain JSONL, use `jq`. TUI monitoring
  logs (`ltop-log-*.jsonl`) share the schema — `report` reads them too;
  old `session-*` files (v1) belong to `--replay`.

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

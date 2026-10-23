# AI agents

ltop treats "the machine's network/disk/LAN state" as data, not as screen
scraping: one schema-versioned JSON snapshot feeds the TUI, the CLI, the
logs, and the headless tools. This page is the working reference for using
ltop from an agent; the same content ships as a ready-made agent skill in
the repo at [`docs/skills/ltop-observe/SKILL.md`](https://github.com/uMaj35ty/ltop/blob/master/docs/skills/ltop-observe/SKILL.md)
(diagnostic recipes included).

## One-shot questions: `--json`

```bash
ltop --json | jq '<query>'     # snapshot; ~1.2 s (two sampling rounds populate rates)
ltop --schema                  # versioned field reference, drifts never
```

A single filtered snapshot costs ~1.7k tokens — vs ~22k for the traditional
shell-command combination (`ss`/`lsof`/`df`/`arp` output to parse) and ~8k
unfiltered. For one-shot questions this is the right tool.

Strict CLI surface: unknown subcommands and typos (`ltop repor`) exit 1 with
a clear message — nothing falls into the interactive TUI by accident, and
JSON keys are always English regardless of `lang`.

## Time windows: `watch` + `report`

For "watch this for 10 minutes, then report", **do not poll** `--json`
(~40k tokens per 10 minutes at 5 s cadence). Record once, summarize once:

```bash
ltop watch --duration 10m --proc chrome    # headless, crash-safe, no root
ltop report <path watch printed> --json    # report takes a single file
```

`report --json` returns ~2 KB with exact numbers — window sums (last
snapshot cumulative − first, segmented across pid restarts), peak rates,
active time — instead of sampled guesses. Measured on a live desktop:
**~97 % fewer tokens** than polling for the same information.

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

`--top N` (default 5), `--proc` (substring or `*` glob) and `--fields
net,udp,disk,conns` trim the output further.

## Division of labor with `jq`

`watch` output and TUI monitoring logs are plain JSONL — anything `report`
does not aggregate is a `jq` one-liner away (the files share the schema, and
`report` reads both). `report` for window math, `jq` for arbitrary slices,
`--json` for the current instant.

## Reading the snapshot — the parts that get misread

- `meta.udp_helper: true` means the Linux eBPF helper is live and UDP
  per-process accounting is complete; when `false` (or on macOS) UDP numbers
  are partial.
- `conns[].process` is `null` for sockets owned by **other users** (Linux).
  Do not claim those connections are unrelated to any process — they are
  just not attributable to your uid.
- `procs[].disk_*` covers only processes owned by the current user.
- `net.interfaces[].session_rx/tx` = since ltop started (resettable);
  `alive_rx/tx` = since the interface came up.
- Process totals are **payload** bytes; interface totals are **NIC** bytes —
  a 10–40 % gap is by design ([why](ebpf.md#what-the-numbers-mean)).

## Recipes (from the bundled skill)

- **Network slow?** sort `net.interfaces[]` by `rx_bps+tx_bps`; check
  `operstate`/`speed_mbps`; then the top `conns[]` on the busy interface —
  `conns[].process` names the eater (your user only).
- **Disk busy?** `disk.devices[]` for throughput vs capacity, then `procs[]`
  sorted by disk rates for the writer.
- **LAN inventory?** `lan.neighbours[]` — gateways flagged, mDNS hostnames,
  `tracked` marks pinned devices.
- **"Watch chrome for 10 minutes and report traffic"** — the watch+report
  pair above, with `--proc chrome` on both commands.

## Outlook

A `ltop mcp` subcommand (hand-written stdio JSON-RPC wrapping `--json`,
watch+report and — later — multi-host fleet aggregation) is on the 0.3
roadmap; the JSON surfaces above are its intended substrate and are stable
in schema v1 today.

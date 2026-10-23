# The data plane

Every display surface in ltop is backed by one serialized snapshot —
**schema v1**, versioned with the binary so scripts and agents can rely on
it. The same structs serialize into all of these:

| Surface | Command | Shape |
|---|---|---|
| one-shot snapshot | `ltop --json` | single JSON object (~8k tokens unfiltered) |
| field reference | `ltop --schema` | versioned machine-readable field list |
| monitoring log | `L` / `--log` | JSONL, one snapshot per `log_interval_ms` (default 60 s) |
| headless capture | `ltop watch` | JSONL, one snapshot per `--interval` (default 5 s) |
| window summary | `ltop report` | human table, or one ~2 KB JSON object with `--json` |

```bash
ltop --json      # structured snapshot (takes ~1.2 s: two sampling rounds
                 # so rate fields are populated)
ltop --schema    # the authoritative field list — versioned with the binary,
                 # never drifts from it
```

The snapshot covers five domains: `meta` (version, os, arch,
`udp_helper` flag), `net` (per-interface rates and cumulative counters),
`procs` (per-process network + disk accounting with UDP breakdown),
`conns` (per-connection sockets), `disk` (capacity/throughput), and `lan`
(neighbours). Semantics worth knowing are marked right in the output — e.g.
macOS MAC sanitization.

## Monitoring log (TUI)

`L` in the TUI, or start with `--log`, records snapshots to
`~/.local/state/ltop/ltop-log-<ts>.jsonl`:

- `log_interval_ms` (default 60 000) sets the cadence;
  `log_tracked_only = true` records only tracked processes.
- `log_retention_days` (default 7) purges old files at startup and when
  toggled.

## Headless watch

```bash
ltop watch --duration 10m --proc chrome   # full syntax in `ltop watch --help`
```

- Stops on `--duration` (`10s`/`5m`/`2h`), a `--stop` sentinel file (consumed
  on sight), or SIGINT — which is graceful: the last snapshot lands in the
  file.
- Output defaults to `~/.local/state/ltop/ltop-session-<ts>.jsonl`, separate
  from the TUI monitoring log and exempt from its retention purge; its own
  `--retention` (default 7 days, `0` = keep forever) prunes at start.
- Line-by-line appends are crash-safe; no root required.
- `--proc <name>` focus mode records only matching processes and their
  connections — tracked **by name**, so a restart that changes the pid keeps
  the timeline continuous.
- Write volume scales with the interval (1 s ≈ 8 KB/line ≈ 28 MB/h; the 5 s
  default is ~5–6 MB/h). `--retention` is the cleanup story.

## Report

```bash
ltop report <file.jsonl> [--json]
```

Reads a watch capture **or** a TUI monitoring log (same schema) and produces
the window summary: per-process window sums (window differencing across pid
restarts), peak rates, active time, last alive/exited state; per-connection
sums/peaks/lifetimes keyed by inode (vanished connections keep their last
known totals); NIC-scope global totals and the peak interval.

Filters: `--top N` (default 5, by traffic), `--proc` (substring, or glob when
it contains `*`), `--fields net,udp,disk,conns`, and `--json` for the
machine-readable form. The AI workflow built on this lives in
[AI agents](ai.md).

## Legacy and debugging

- `ltop --dump` — one human-readable sampling round to stdout (debugging;
  prefer `--json` for parsing, the dump truncates and misaligns values).
- `ltop --replay [--csv OUT] [--top N]` — rebuilds session deltas from
  legacy `session-*` logs (v1, procs-only) and exports CSV. Current-format
  files belong to `report`; old ones are rejected with a pointer here.

## Configuration

`~/.config/ltop/config.conf` is plain `key = value`, written by the in-app
settings panel (`m` → settings) and safe to hand-edit (applies on restart):

| Key | Default | Meaning |
|---|---|---|
| `lang` | `en` | UI/messages language; `zh` switches all human output to Chinese (JSON keys stay English) |
| `layout` | classic | start layout (classic/wide/tall) |
| `tick_ms` | 1000 | refresh interval |
| `graph_net` / `graph_disk` | hybrid / centered | default chart style |
| `boxes` | all on | visible boxes |
| `log_interval_ms` | 60000 | monitoring log cadence |
| `log_retention_days` | 7 | monitoring log purge age |
| `log_tracked_only` | false | log only tracked processes |
| `sweep` / `sweep_interval_s` | true / 60 | LAN UDP-knock sweep on/off and cadence |
| `tombstone_ttl_ms` | 5000 | how long closed connections linger in CONNS |
| `theme` | omarchy | theme name (see [Themes](themes.md)) |
| `theme_live_follow` | false | re-color a running ltop on Omarchy theme change (same as `--follow-theme`) |
| `proc_expand_default` | false | CONNS process groups start expanded |
| `proc_exit_retention_min` | 30 | evict exited-process ledgers after N minutes (0 = forever) |
| `conn_filter` / `conn_sort` / `proc_sort` / `lan_sort` | all / proc / net-sum / ip | default list modes |
| `allow_global_kill` | false | allow killing other users' processes |
| `net_graph_coupled` | false | coupled NET chart scaling: when one direction dwarfs the other, the small side renders at its true fraction of the chart height instead of auto-scaling to full height |
| `net_graph_scale_ratio` | 5 | trigger threshold for the above, in×(2–20); ratios below it keep normal auto-scaling |

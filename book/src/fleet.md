# Fleet (multi-node aggregation)

`ltop fleet` answers "what is happening on *all* my machines" from one
terminal. The manager polls each node over SSH for its `ltop --json`
snapshot (schema v1) and aggregates the answers into one table. Nodes need
nothing but an installed ltop (any version ≥ 0.2.3) and SSH key access —
no daemon, no agent, no open port, no new software.

## Set up a node

```console
$ ltop fleet setup air
```

The doctor finds a local public key, hands the one-time authorization to
`ssh-copy-id` (any password prompt belongs to ssh, not ltop — ltop never
sees credentials), verifies `ltop --json` works on the node and reports its
version and latency, then prints the recommended *force-command*
`authorized_keys` line that locks the key to exactly the snapshot command
(see [Security → Fleet](security.md#fleet-ssh-aggregation-030-beta)).

## Add nodes

Either pass them on the command line once with `--save`:

```console
$ ltop fleet --host air --cmd '/opt/homebrew/bin/ltop --json' --host fedora --save
saved 2 node(s) to /home/you/.config/ltop/fleet.conf
```

or edit `~/.config/ltop/fleet.conf` directly (hand-written lines are
preserved across rewrites):

```text
interval = 5          # seconds between rounds
timeout  = 15         # per-node pull timeout
master   = on         # ControlMaster connection reuse
node = air
node = fedora
cmd fedora = /usr/local/bin/ltop --json
```

`cmd <node>` is order-independent and matters on macOS: a non-interactive
ssh session has no PATH additions, so give the full path there
(`/opt/homebrew/bin/ltop --json` for Homebrew installs).

## Run it

On a terminal, `ltop fleet` opens its own TUI (independent of the six-box
local monitor):

- **↑/↓** select a node (selection follows the node through re-sorts)
- **enter** node detail — per-interface rates with the selected interface
  marked, LAN/disk/conns counts, top-5 processes by network activity,
  rebuilt live from the node's last good snapshot
- **s** cycle sort: config order → rx → tx → procs → conns (descending,
  failed nodes sink)
- **p** pause / resume background polling
- **r** pull one round now
- **c** copy `ssh <node> 'kill <pid>'` for the selected node's most active
  process to the clipboard (OSC 52) — ltop only *copies* the command, your
  terminal executes it
- **h** help, **esc** close popups, **q** quit

Above the table, `Σ rx` / `Σ tx` sparklines show the fleet total with
damped peaks (instant rise, slow decay) so one burst does not flatten the
history. A failed round keeps the node's last good row and explains itself
in place ("no SSH key configured — run `ltop fleet setup …`"), so one dead
node never blanks the view.

Non-interactive runs (`--once`, or stdout not a terminal) print one table
per round instead — the same output a headless deployment (cron/systemd
user unit) consumes:

```console
$ ltop fleet --once
[t+1794… ] round 1
node      rx     tx   procs conns  ms   top talker
air    4.5KiB 8.9KiB    4    53  2075  ZCode Helper (2.1KiB/s)
```

## Alerts

Threshold rules live in `fleet.conf` and fire **system notifications** on
the machine running fleet (`notify-send` on Linux, `osascript` on macOS;
without a desktop session the state stays visible in the TUI/CLI only):

```text
alert node rx > 50M        # node rx over 50 MB/s
alert node tx > 1G         # node tx over 1 GB/s
alert proc chrome net > 20M   # processes matching "chrome" (case-insensitive,
                              # sums) over 20 MB/s
alert conn ip 203.0.113.7  # a connection to this IP appears
alert new-device           # a LAN neighbour not seen before (instant)
alert node-down            # node unreachable for 3 consecutive rounds
```

Anti-spam is built in: a rule must hold for **2 consecutive rounds**
(hysteresis) before firing, re-notifies at most every 10 minutes while it
stays active, sends a *resolved* notification when it clears, and a
rolling window caps everything at 20 notifications per hour. `alert
new-device` is deliberately instant (first sight seeds the baseline, so
your existing LAN never alarms). The TUI header shows `⚠ N` while alerts
are active; non-tty runs print `⚠ …`/`✓ resolved …` lines. See
[Security → Fleet](security.md#fleet-ssh-aggregation-030-beta) for why
notifications never carry raw remote stderr.

## Options

| Flag | Meaning |
|---|---|
| `--host <TARGET>` | node to poll, repeatable; overrides `fleet.conf` for this run |
| `--cmd <COMMAND>` | remote command for the most recent `--host` (default `ltop --json`) |
| `--save` | persist the effective node list to `fleet.conf` |
| `--once` | one round, non-interactive, exit |
| `--interval <S>` | seconds between rounds (default 5, min 1) |
| `--timeout <S>` | per-node pull timeout (default 15) |
| `setup <TARGET>` | key doctor: install, verify, print hardening line |

Security boundaries — key-only pulls, no credential storage, the
force-command recommendation, and the copy-only control surface — are
described in [Security → Fleet](security.md#fleet-ssh-aggregation-030-beta).

## Compatibility

The wire format is the schema-v1 snapshot with an additive-only contract:
a 0.3.0-beta manager happily mixes nodes running 0.2.3 and 0.2.13, and
future nodes may add fields the old manager ignores. Nodes older than
0.2.3 (no `--json`) are reported in place with an upgrade hint instead of
failing silently.

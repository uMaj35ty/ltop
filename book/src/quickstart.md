# Quick start

## 1. Install (one line)

```bash
brew install uMaj35ty/ltop/ltop     # macOS & Linux — prebuilt binaries
```

or, with a Rust toolchain:

```bash
cargo install ltop                  # compiles from source (crates.io)
```

or grab a tarball from [GitHub Releases] (`x86_64` / `aarch64`, Linux and
macOS). All channels are covered in [Installation](installation.md).

[GitHub Releases]: https://github.com/uMaj35ty/ltop/releases

## 2. Run it

```bash
ltop
```

No configuration, no root. You land on the classic layout: charts on the left
(NET up top, DISK I/O below), tables on the right (LAN, CONNS, PROCS, DISKS).

## 3. The keys that matter

Everything is reachable from these ten; the footer always shows the current
context, and `m → help` has the full table (also in [The TUI](tui.md)).

| Key | Does |
|---|---|
| `Tab` | cycle layout: classic → wide → tall |
| `1`–`6` | toggle a box (1 net, 2 lan, 3 disk i/o, 4 disks, 5 conns, 6 procs) |
| `/` | search processes/connections by name substring |
| `p` / `c` / `l` | single-focus PROCS / CONNS / LAN (one lights up, the rest dim) |
| `↑↓` + `enter` | select a row → detail panel (full command line, per-connection breakdown, uptime) |
| `enter` on a LAN row | pin the neighbour and filter CONNS to that peer — "show me everything talking to this device" |
| `f` / `o` | CONNS: cycle filter (all/lan/tcp/udp) / sort (proc-grouped, sum, name, rate) |
| `r` | PROCS: cycle sort (net-sum, net-rate, disk-sum, disk-rate, name) |
| `z` | zoom the focused box full-screen (`esc` restores) |
| `m` | menu: themes, settings, graphs, help |
| `space` | pause/resume sampling (bytes missed while paused are back-filled) |
| `q` | quit |

## 4. Optional: per-process/per-connection byte accounting (Linux)

Without this, ltop still works — CONNS shows TCP live rates and PROCS shows
network totals from socket sampling; what is missing is *exact* UDP
per-process accounting and per-connection totals. One command enables it:

```bash
sudo cp target/release/ltop-bpf /usr/local/bin/      # from a release tarball or source build
sudo cp deploy/ltop-bpf.service /etc/systemd/system/
sudo systemctl enable --now ltop-bpf
```

A Homebrew install ships the same binaries; deploy the helper from the same
release the TUI came from (see [eBPF accounting](ebpf.md) and the
[upgrade note](faq.md#per-process-accounting-zeroed-after-an-upgrade)).

## 5. Optional: the AI workflow

The one command to remember:

```bash
ltop watch --duration 10m --proc chrome   # record headlessly (no root, crash-safe);
                                         # prints the output path
ltop report <that path> --json            # report takes a single file
```

`report` prints exact window sums and peaks instead of sampled guesses —
a 10-minute window costs ~1k tokens as JSON vs ~40k polled via `--json`.
See [AI agents](ai.md).

## 6. Make it yours

- **Theme**: `m` → theme cycles the 72 embedded btop themes; on Omarchy the
  `omarchy` entry follows the desktop. `--theme <file>` loads any btop theme
  file; `--follow-theme` re-colors a running ltop when the desktop theme
  changes. Details in [Themes](themes.md).
- **Language**: `lang = zh` in `~/.config/ltop/config.conf` switches all human
  output (TUI and CLI) to Chinese; JSON keys stay English.
- **Settings panel**: `m` → settings writes `~/.config/ltop/config.conf`
  (tick rate, log retention, sweep interval, sort defaults, …). See
  [The data plane](data.md#configuration).

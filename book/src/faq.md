# FAQ

## The rx and tx charts have the same shape

Not a bug — two effects stacking. Each chart scales to its own window peak
(btop-style, so small movements stay readable), and during a download your
uplink is almost entirely TCP ACKs, whose rate tracks the downlink's packet
rate one-to-one. The tx/rx ratio you'll see is ~1/30 — exactly the expected
ACK share (one ACK per ~2 segments, ~66 bytes per ~2900 bytes of data); the
graph then blows that tiny waveform up to full height, so the two curves
look like siblings. When the two directions differ this much, read the
header numbers, switch the graph mode to Block (`m` → graphs), or turn on
coupled scaling so the small side renders at its true fraction of the chart
height: `m` → graphs → `c` (persist with `net_graph_coupled = true`; the
trigger ratio is `net_graph_scale_ratio`, default 5×).

## PROCS totals are lower than the NET box

By design, on every platform. The NET box counts **interface bytes**
(driver level: every packet, including headers, retransmits and background
chatter). PROCS counts **payload bytes** attributed to processes (eBPF at
the syscall on Linux; live-socket accounting on macOS), and only for your
own processes on some platforms. The gap is the sum of everything that is
interface traffic but not your processes' payload. See
[eBPF accounting → What the numbers mean](ebpf.md#what-the-numbers-mean).

## CONNS totals include traffic from before ltop started

Also by design, and TCP-only: a TCP connection's Σ comes from the kernel's
per-socket counters, which cover the connection's whole life — including
time before ltop launched. UDP Σ is session-accumulated by ltop from the
moment the helper is live. That difference (plus headers) is part of why
CONNS and PROCS totals don't match each other either; traffic through a
proxy is attributed to the proxy process.

## Per-process accounting zeroed after an upgrade

A stale `ltop-bpf` keeps writing its snapshot to the old path while the new
TUI diffs against a different file — the symptom is per-process numbers
silently reading zero. Fix: always re-deploy the helper with the TUI:

```bash
sudo cp ltop-bpf /usr/local/bin/          # from the same release as the TUI
sudo systemctl restart ltop-bpf
```

## UDP columns are missing / partial

On Linux this means the eBPF helper is not running — check
`meta.udp_helper` in `ltop --json` and
[deploy it](ebpf.md#deploying) (no root needed for the TUI itself; TCP
columns keep working from socket sampling without the helper). On macOS UDP
per-process numbers come from sampled live sockets and transfers that
complete within one tick are not counted — a documented platform
difference, not a bug.

## `conns[].process` is null for some connections

Those sockets belong to **other users** (Linux): attribution scans your own
uid's sockets. "null" means "not attributable to you", not "unrelated to any
process".

## CONNS has a group named "n/a" (uid:4294967295 in older versions)

macOS counterpart of the above: the unprivileged TUI can only attribute
sockets of **your own user** (libproc, same semantics as reading your own
`/proc/<pid>/fd` on Linux). Connections belonging to other users, system
daemons or the kernel are visible as traffic but not attributable to a
process, so they group under `n/a` — 0.2.12 and later; older builds printed
the raw `uid:4294967295` sentinel, which is the same bucket. This is a
permission boundary, not missing data. To see the owners resolved, run
`sudo ltop` once (root can enumerate every process and the group empties
out); only do so if you are comfortable running a monitor as root.

## One-shot `--json` takes ~1.2 s

Two sampling rounds run back-to-back so every rate field is populated from
a real delta. For scripting, that latency buys you rates instead of
counters-only; `--dump` skips it but truncates values — parse `--json`.

## `watch` filled my disk

Write volume scales with `--interval`: 1 s ≈ 8 KB/line ≈ 28 MB/h; the 5 s
default is ~5–6 MB/h. Captures default to `--retention 7` days (purged at
the next watch start; `0` keeps forever). Long-term recording is the
monitoring log's job (`L` / `--log`), which defaults to 60 s granularity and
7-day retention.

## The `loli` command is gone

Retired in 0.2.4 — it was a leftover from the original *localist top*
naming, and tarballs/brew/AUR no longer ship it. Make your own alias if you
liked it (help follows the invoked name):

```bash
ln -s "$(command -v ltop)" ~/.local/bin/loli
```

## AUR package?

`ltop-bin` is built and waiting for the AUR to reopen account registration.
Until then: Homebrew, crates.io or GitHub Releases.

## Missing braille glyphs (boxes render as `?` or blank)

Your terminal font lacks Unicode braille. Use a proper terminal emulator
fonts-and-all, or render over SSH — bare framebuffer consoles may miss the
glyphs. On macOS this affects the built-in Terminal's framebuffer rendering
in particular.

## Old kernels / cloud images

glibc 2.35+ and a kernel with kprobes are the runtime floor for the eBPF
helper; the TUI itself runs on anything the binary links against. Ubuntu's
`-kvm` cloud kernel trims kprobes — use the `generic` kernel. Ubuntu 22.04's
5.15 works **without BTF** (CO-RE resolves at load time).

## Does ltop phone home?

No telemetry, ever. The only outbound activity is the LAN sweep: small UDP
packets to your own subnet (default every 60 s, `sweep = false` to disable)
that make the kernel resolve neighbours via ARP — no port probing, no raw
sockets, no traffic beyond your connected prefixes.

## How do I get the report as JSON for my agent/script?

```bash
ltop report <file.jsonl> --json
```

… and read [AI agents](ai.md) — including why polling `--json` in a loop
wastes ~40k tokens per 10 minutes versus ~1k for a report.

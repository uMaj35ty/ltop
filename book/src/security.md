# Security

This page describes what ltop touches on your system and network, what it
refuses to do, and how the untrusted inputs it reads are handled. The
operational policy (supported versions, how to report a vulnerability) is
the repository's [SECURITY.md](https://github.com/uMaj35ty/ltop/blob/master/SECURITY.md).

## Privileges

- **The TUI runs unprivileged.** Every data source is readable by any
  process: `/proc`, `getifaddrs`, PF_ROUTE/netlink dumps, ephemeral UDP
  sockets. Kill/relaunch actions are bounded by the kernel to your own uid.
- **`ltop-bpf` is the only privileged component** and it is optional. It
  runs as a systemd service with `CAP_BPF` + `CAP_PERFMON` only (plus
  `CAP_SYS_RESOURCE` on kernels < 5.11), writes a single snapshot file
  under `/run/ltop`, holds no listening sockets and makes no network
  connections. The shipped unit applies a sandbox: `NoNewPrivileges`,
  `ProtectSystem=strict` (only `/run/ltop` writable), `ProtectHome`,
  `PrivateTmp`, kernel-interface protections and a capability bounding set
  without `CAP_SYS_ADMIN`.

## Your data on disk

| Data | Location | Mode |
| --- | --- | --- |
| Monitoring log (`L` / `--log`) | `~/.local/state/ltop/` | 0600 |
| Watch capture | `~/.local/state/ltop/` | 0600 |
| Config | `~/.config/ltop/config.conf` | 0644 (preferences only) |
| eBPF snapshot | `/run/ltop/udp.json` | 0644, root-owned — **world-readable by design** |

Session logs and captures hold connection endpoints, process names and
hostnames, so they are created `0600` regardless of umask. The eBPF
snapshot is world-readable because the unprivileged TUI reads it; on a
shared host this means every local user can see per-process byte totals —
factor that in before deploying the helper on multi-user machines.

ltop has **no telemetry**: no update checks, no analytics, nothing leaves
your machine except the LAN probes below.

## Network behavior

The LAN box actively queries your local network:

- per-neighbour unicast **mDNS**, **NetBIOS node-status** and **reverse
  DNS** lookups (hostname resolution);
- a configurable UDP "knock" sweep — one empty datagram to discard port 9
  per subnet host, at most 1022 hosts per round, default every 60 s,
  toggleable in settings.

Everything is scoped to your LAN. Replies are untrusted input and handled
accordingly: DNS/NBNS transaction IDs are randomized per query and
validated together with the source address, all parsers are bounds-checked
pure Rust, untrusted strings are sanitized before they reach any terminal
output, and the eBPF snapshot reader caps file size.

## Fleet (SSH aggregation, 0.3.0-beta)

`ltop fleet` is key-only by construction: every pull runs
`ssh -o BatchMode=yes`, so password auth cannot hang the poller because it
cannot happen at all. ltop never stores, asks for, or forwards passwords or
passphrases — the one-time key installation (`ltop fleet setup <target>`)
hands the terminal to `ssh-copy-id`, and any password prompt belongs to
ssh, not ltop. There is no `password =` field in `fleet.conf`.

- **No credential library**: fleet shells out to the system `ssh`; host-key
  verification is never relaxed (no `StrictHostKeyChecking=no` escape hatch;
  first-contact fingerprint confirmation happens in your own ssh session).
- **The manager key is the blast radius**: it can read metrics from every
  node it is installed on. `fleet setup` prints the recommended
  *force-command* authorized_keys line locked to exactly `ltop --json` with
  forwarding disabled — a leaked key can then only read metrics.
- **Control is copy-only**: the `c` key copies `ssh <node> 'kill <pid>'`
  to the clipboard (OSC 52); ltop never executes anything on a node beyond
  the read-only snapshot command.
- **Remote strings are adversarial**: hostnames, process names and ssh
  stderr are sanitized (control characters stripped) and truncated before
  reaching the terminal; stderr is classified by substring match, never
  echoed raw. `fleet.conf` is written 0600 (it reveals LAN topology);
  ControlMaster sockets live under `XDG_RUNTIME_DIR`.
- Node count is capped at 64.

## Reporting a vulnerability

Please use GitHub **Private Vulnerability Reporting** on the repository
(never a public issue for anything exploitable). See
[SECURITY.md](https://github.com/uMaj35ty/ltop/blob/master/SECURITY.md)
for the full policy.

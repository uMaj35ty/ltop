# Security Policy

ltop is a local-first monitoring tool: the TUI runs unprivileged, reads
local kernel interfaces, and — only when you enable it — uses a privileged
eBPF helper plus active LAN probes. This document describes the supported
versions, how to report vulnerabilities, and the boundaries of the threat
model.

## Supported versions

Only the latest release line receives security fixes:

| Version | Supported |
| --- | --- |
| 0.2.x (latest) | ✅ |
| older | ❌ — please upgrade |

## Reporting a vulnerability

Please use **GitHub Private Vulnerability Reporting**
(Security → Report a vulnerability) — do not open a public issue for
something exploitable. Include the affected target (Linux/macOS ×
x86_64/aarch64), a reproduction path, and your assessment of impact.

You will get a response within 7 days. Fixes land in the next patch/minor
release; you will be credited in the release notes unless you prefer
otherwise.

## Threat model and boundaries

### Process privileges

- **The TUI runs unprivileged.** All kernel data comes from interfaces any
  process may read (`/proc`, `getifaddrs`, PF_ROUTE/netlink dumps, ephemeral
  UDP sockets). Killing/relaunching is limited to your own uid by the
  kernel.
- **`ltop-bpf` is an explicit, root-only helper** (systemd service). It
  attaches kprobes, needs `CAP_BPF` + `CAP_PERFMON` (+ `CAP_SYS_RESOURCE`
  on kernels < 5.11), and writes one snapshot file. The shipped unit
  sandbox it: `NoNewPrivileges`, `ProtectSystem=strict` with only
  `/run/ltop` writable, `ProtectHome`, `PrivateTmp`, kernel-interface
  protections, and a bounding set with no `CAP_SYS_ADMIN`. It holds no
  listening sockets and makes no network connections.

### Data on disk

- Session logs (`~/.local/state/ltop/`) record connection endpoints,
  process names and hostnames and are created **0600**.
- `/run/ltop/udp.json` is **world-readable by design** (the unprivileged
  TUI reads the root-written snapshot). It contains per-process/per-socket
  byte totals only — on multi-user hosts this is visible to every local
  user; keep that in mind before deploying the helper on shared machines.
- No telemetry: ltop never sends data anywhere beyond your own LAN probes.
  There are no update checks, no analytics, no phones-home.

### Network behavior (LAN-active features)

The LAN box queries the local network: unicast mDNS, NetBIOS node-status
and reverse-DNS lookups per neighbour, plus a configurable UDP "knock"
sweep (one empty datagram to discard port 9 per subnet host, ≤ 1022 hosts,
default every 60 s, toggleable). Replies are parsed defensively: transaction
IDs are randomized per query and validated together with the source
address; the parsers are bounds-checked pure Rust with fuzz-resistant
fallbacks. The TUI renders untrusted strings through ratatui (control
characters dropped), and the CLI output paths sanitize them explicitly.

### Known, by-design trade-offs (not vulnerabilities)

- `--out`/`--csv`/`--replay` accept arbitrary paths **within the calling
  user's permissions** — they are features, not privilege boundaries.
- Exported CSVs of process names can contain spreadsheet-hostile content;
  fields are quoted and formula-prefixed (`=`/`+`/`-`/`@` get a leading
  apostrophe), but treat exports as data, not code.
- The eBPF `sk_ino` LRU has a documented, low-probability
  mis-attribution window after SLAB address reuse (see the roadmap).

## Supply chain

Releases are built from tagged commits with `--locked` (Cargo.lock is
committed). All GitHub Actions are pinned to commit SHAs; workflows run
least-privilege (CI is read-only; only the release job holds
`contents: write`, and the tap job's token never persists on the runner).
Dependabot (actions + crates) and secret scanning with push protection are
enabled on the repository.

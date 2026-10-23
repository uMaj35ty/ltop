# LAN discovery

The LAN box answers "what is on my network" without a root, without nmap, and
without leaving the TUI. Discovery is two mechanisms:

1. **Passive** — the kernel's neighbour (ARP/NDP) table is read on every
   refresh, so any device the machine has spoken to appears immediately, with
   whatever freshness the kernel still holds.
2. **Active UDP-knock sweep** — a background thread walks the local subnet
   (derived from the interface address + prefix) and sends small UDP packets
   to candidate addresses. The packets themselves usually go unanswered, but
   the attempt forces the kernel to resolve each address via ARP, which makes
   the *kernel* report the neighbour — ltop never needs raw sockets or root.
   The sweep cadence is `sweep_interval_s` (default 60 s) and can be turned
   off (`sweep = false`) for quiet networks.

The two sources merge per (interface, IP). Hostnames resolve through a
best-effort chain — mDNS reverse lookup → NetBIOS node status → reverse DNS —
gateways are flagged, and macOS's neighbour-table sanitization is
worked around by probing kernel cache entries through the route socket — see
[Compatibility → macOS](compat.md#macos) for what is and is not visible
there.

## What each neighbour shows

| Column | Meaning |
|---|---|
| IP / hostname | resolved address, mDNS name when available |
| MAC | hardware address (— on macOS, system-sanitized) |
| iface | which local NIC the neighbour is reachable through |
| Σ conns | live connection count from CONNS to this peer |
| rate | current rx+tx to this peer |
| Σ bytes | accumulated bytes to this peer since ltop started |

The traffic columns are the point: the LAN box and CONNS share the connection
ledger, so each neighbour carries its own live slice of the traffic.

## Interactions

- `e` cycles the LAN sort (ip → hostname → mac → iface).
- `s` toggles the background sweep on/off for the current session.
- `↑↓` selects; the selection *follows* the neighbour across re-sorts and
  sweeps (a device that briefly vanishes from ARP does not lose your place).
- `enter` **pins** the neighbour — it stays at the top with a red highlight —
  and simultaneously **filters CONNS to that peer**. This is the fast path
  for "what exactly is this device doing": select it, press enter, read the
  per-connection breakdown. Press `enter` again to unpin; `esc`/`f` also
  clear the filter.
- `l` single-focuses the LAN box; `z` zooms it full-screen (useful on dense
  subnets).

## Boundaries by design

- The subnet is derived from your own interface; ltop does not scan beyond
  the connected prefixes.
- Devices that block ARP entirely (some firewalled hosts) appear only if the
  kernel has a cached entry from other traffic.
- The sweep is UDP-to-nothing: no TCP connects, no port probing, no raw
  packets — it looks like ordinary (failed) delivery attempts to the OS.

# Compared to other tools

ltop doesn't try to replace btop or nethogs. It covers the slice they leave
open: who talks to whom on this machine and this LAN, per process and per
connection, with a machine-readable data plane.

## btop — use both

btop owns the system overview: CPU, memory, sensors, every process. ltop
borrows its visual language (braille charts, box grid, even the theme
format) but only watches network, disk I/O and the LAN — and watches those
deeper: per-process session totals that survive restarts, per-connection
bytes for TCP and UDP, LAN neighbours with per-peer traffic, headless
capture for later analysis.

The author runs btop and ltop side by side every day — btop for the
system, ltop for the traffic — and recommends exactly that setup. Any
btop theme you already use works in ltop as-is.

## nethogs — per process, live only

nethogs prints a per-process bytes-per-second list (needs root for most of
it) and forgets everything between refreshes. ltop keeps session ledgers:
exited processes stay on screen with their totals, TCP and UDP are
attributed exactly via eBPF, and you also get per-connection rows, disk
I/O and the LAN dimension. If a live list is all you need and you have
root, nethogs is smaller; the rest is ltop territory.

## iftop — per pair, one interface

iftop shows bandwidth between address pairs on one interface. ltop's CONNS
box covers the same view with totals and rates per connection, adds the
owner process, and links each LAN neighbour to its connections. iftop's
remaining advantage: it is preinstalled in more places.

## The shell pipeline (ss / lsof / arp / df)

The comparison that matters for scripts and agents: five output formats to
parse, root needed for half the answers, no per-socket UDP bytes anywhere,
and no memory of the last hour. `ltop --json` answers in one schema-stable
snapshot (~1.7k tokens filtered), and `watch` + `report` turn "what
happened" into two commands with exact numbers — see [AI agents](ai.md).

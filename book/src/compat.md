# Compatibility

## Requirements

Rust stable for source builds; Linux or macOS on **x86_64 / aarch64**.
Prebuilt Linux binaries baseline **glibc 2.35** (Ubuntu 22.04 LTS). The eBPF
accounting is Linux-only and needs a clang with the `bpf` target at *build*
time; at *runtime* it needs nothing special — no BTF is required, no nightly
toolchain, no kernel headers.

## Tested environments

Release tarballs are verified per distribution in QEMU/KVM: open-box binary,
eBPF probe attach (incl. kprobe), systemd unit, and the silent degrade path
without the helper. ltop is developed daily on Omarchy (Arch), which doubles
as the reference desktop.

| Environment | Kernel | eBPF accounting | Notes |
|---|---|---|---|
| Arch Linux (rolling) | 7.2.7 | ✓ full | also the dev machine (Omarchy desktop) |
| Ubuntu 24.04 LTS | 6.8 | ✓ full | glibc 2.39 |
| Ubuntu 22.04 LTS | 5.15 | ✓ full | works **without BTF**; glibc 2.35 baseline |
| Ubuntu 24.04 LTS (aarch64) | 6.8 | ✓ full | QEMU aarch64; Pi-class hardware expected to match |
| Debian 13 | 6.12 | ✓ full | glibc 2.41 |
| Fedora 44 (Workstation live) | 6.19 | ✓ full | SELinux enforcing: **zero denials** |
| macOS 27 (Apple Silicon, MacBook Air) | darwin 27 | n/a (`nettop` path) | verified on hardware; see [macOS](#macos) |

Specialty kernels that trim kprobes/BTF (e.g. Ubuntu's `-kvm` cloud kernel)
are the one known trap: use the `generic` kernel there. That is a kernel
flavour limitation, not an ltop limitation.

## SELinux

Fedora (enforcing) runs both binaries and the systemd unit with zero
denials — the helper attaches kprobes through the standard bpf syscall path
and writes only its world-readable snapshot to `/run/ltop/`.

## macOS

macOS has no eBPF; the same six boxes read Apple-native sources:

- **PROCS and CONNS** poll `nettop` (per-process rows and per-socket flows,
  TCP and UDP in one spawn); CONNS attribution scans the caller's own
  sockets via `libproc` — other users' connections show as `n/a`.
- **DISK** reads IOKit `IOMedia` statistics plus per-process rusage.
- **LAN** parses `arp -an` and the default route — with a workaround for the
  platform's neighbour-table sanitization (below).
- `ltop-bpf` does not exist on macOS.

Documented differences from the Linux path:

- Socket counters are **cumulative per live socket**, sampled: a transfer
  that completes within one sampling tick is not counted. (The eBPF path
  counts at the syscall and misses nothing.)
- **The system hides parts of the local network from binaries that are not
  signed by Apple** — independent of the Local Network privacy toggle: the
  neighbour table enumerates empty, single-entry route queries answer with
  the MAC sanitized to the interface's synthetic address, and `getifaddrs`
  byte counters read frozen (all cross-checked in the same second against
  Apple-signed tools). macOS 27 went further: even Apple-signed `arp` shelled
  out from an app comes back empty, and `getifaddrs` reports every interface
  MAC as `02:00:00:00:00:00` (access is now gated on the undocumented
  `com.apple.developer.networking.topology-observation` entitlement, by
  responsible-process attribution × platform signature). ltop routes around
  what it can: the NET box counts via `netstat -ib`, the LAN box rebuilds its
  neighbour list by probing the kernel cache entry-by-entry over the route
  socket, and `ltop lan-setup` installs a per-user launchd agent that rotates
  `arp`/`ifconfig` snapshots through Apple platform binaries — real MACs in
  the LAN and NET boxes, no root, no entitlements. Without it, MAC addresses
  show as `—`.
- Bare-terminal framebuffer fonts may miss braille glyphs; rendering over
  SSH or in a proper terminal emulator shows them all.

## Release channels recap

[Installation](installation.md) has the details: Homebrew (macOS & Linux),
crates.io (source), GitHub Releases tarballs (x86_64/aarch64), AUR
`ltop-bin` pending registration reopening.

## Known limitations

- Connections of other users can only be attributed to the uid level
  (`/proc/<pid>/fd` is unreadable for them on Linux; `n/a` via `libproc` on
  macOS).
- WiFi NICs expose no link `speed` from the kernel; the field stays empty.
- CONNS TCP totals include the connection's whole history (kernel
  semantics) — see the [FAQ](faq.md#conns-totals-include-traffic-from-before-ltop-started).
- After an `ltop-bpf` restart the eBPF counters start from zero; the UI
  re-baselines automatically via snapshot instance IDs.
- On macOS only live sockets are counted (nettop semantics): a transfer
  completing within one sampling tick is missed.

# Installation

ltop ships as two binaries:

- **`ltop`** — the TUI and the whole CLI surface (`--json`, `watch`, `report`,
  …). Runs unprivileged.
- **`ltop-bpf`** — the optional privileged eBPF collector (Linux only). Not
  needed for ltop to be useful; see [eBPF accounting](ebpf.md).

Platforms: Linux and macOS on **x86_64 and aarch64**. Prebuilt Linux binaries
target **glibc 2.35+** (Ubuntu 22.04 LTS baseline); static musl builds are not
provided.

## Homebrew (macOS & Linux)

Prebuilt binaries via the project tap:

```bash
brew install uMaj35ty/ltop/ltop
```

Upgrades come from the same formula (`brew upgrade ltop`). The formula no
longer ships the retired `loli` symlink; `brew upgrade` cleans it up
automatically.

## crates.io

```bash
cargo install ltop
```

Compiles from source with the stable Rust toolchain; installs both binaries to
`~/.cargo/bin`. Notes:

- **No clang needed.** The eBPF object is normally built at compile time with
  the system clang — but when clang (or the `bpf` target) is unavailable the
  build *succeeds* with an empty object embedded: the TUI runs normally and
  only UDP per-process accounting is silently absent. `ltop-bpf` refuses to
  start with a clear message in that case.
- To build the eBPF collector from source you need a clang with the `bpf`
  target (Arch's default `clang` has it).

## GitHub Releases

Tarballs for `x86_64` and `aarch64`, Linux and macOS, per release. They contain
both binaries and the systemd unit. Binaries are unsigned — on macOS, drop the
quarantine attribute first:

```bash
xattr -c ltop-*-darwin.tar.gz
```

Linux layout after unpacking:

```bash
sudo cp ltop /usr/local/bin/
sudo cp ltop-bpf /usr/local/bin/          # optional, Linux only
sudo cp ltop-bpf.service /etc/systemd/system/
sudo systemctl enable --now ltop-bpf      # optional
```

## AUR

`ltop-bin` — the package is built and waiting; publication is pending the AUR
reopening account registration. This page will change when it lands.

## Deploying / upgrading the eBPF helper

Re-deploy the helper **together with** the TUI — a stale `ltop-bpf` keeps
writing its snapshot to the old path and per-process accounting silently
zeroes:

```bash
sudo cp target/release/ltop-bpf /usr/local/bin/
sudo systemctl restart ltop-bpf
```

## Verify

```bash
ltop --version          # TUI/CLI version
ltop --json | head -c 300    # a schema-v1 snapshot; "meta.udp_helper": true
                        # means the eBPF helper is live (Linux)
sudo ltop-bpf --once    # collector smoke test: sample once, print, exit
```

## Building from source

```bash
git clone https://github.com/uMaj35ty/ltop
cd ltop
cargo build --release
# artifacts in target/release/: ltop, ltop-bpf (Linux)
```

Requirements: Rust stable; on Linux, clang with the `bpf` target for the eBPF
object (see above for what happens without it). The eBPF probes carry
per-arch `pt_regs` register mappings for x86_64 and aarch64; no nightly
toolchain is involved.

Tested per release in QEMU/KVM (open-box binaries, kprobe attach, systemd
unit, silent degrade without the helper) — the matrix lives in
[Compatibility](compat.md).

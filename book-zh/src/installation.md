# 安装

ltop 是两个二进制:

- **`ltop`** —— TUI 和全部 CLI（`--json`、`watch`、`report`……）。免特权运行。
- **`ltop-bpf`** —— 可选的特权 eBPF 采集器（仅 Linux）。没有它 ltop 照样有用，
  见 [eBPF 计费](ebpf.md)。

平台: Linux 与 macOS，**x86_64 和 aarch64**。预编译 Linux 二进制要求
**glibc 2.35+**（Ubuntu 22.04 LTS 基线）；不提供静态 musl 包。

## Homebrew（macOS 与 Linux）

项目 tap 的预编译二进制:

```bash
brew install uMaj35ty/ltop/ltop
```

升级走同一条 formula（`brew upgrade ltop`）。formula 已不带退役的 `loli`
软链；`brew upgrade` 会自动清掉旧的。

## crates.io

```bash
cargo install ltop
```

stable Rust 工具链源码编译；两个二进制都装进 `~/.cargo/bin`。注意:

- **不需要 clang。** eBPF 对象通常在编译期用系统 clang 构建——但 clang（或
  `bpf` target）不可用时构建照样*成功*，嵌入空对象: TUI 正常运行，只有 UDP
  每进程计费静默缺席。该情况下 `ltop-bpf` 启动时会明确报错拒启。
- 从源码构建 eBPF 采集器需要带 `bpf` target 的 clang（Arch 默认 `clang`
  就有）。

## GitHub Releases

每个 release 提供 `x86_64` 和 `aarch64`、Linux 与 macOS 的 tarball，内含
两个二进制和 systemd 单元。二进制未签名——macOS 先去掉隔离属性:

```bash
xattr -c ltop-*-darwin.tar.gz
```

Linux 侧解包后:

```bash
sudo cp ltop /usr/local/bin/
sudo cp ltop-bpf /usr/local/bin/          # 可选，仅 Linux
sudo cp ltop-bpf.service /etc/systemd/system/
sudo systemctl enable --now ltop-bpf      # 可选
```

## AUR

`ltop-bin`——打包件已就绪，等 AUR 开放账号注册即发布，届时更新本页。

## 部署/升级 eBPF 助手

助手要**随 TUI 一起**重新部署——陈旧的 `ltop-bpf` 还在往旧路径写快照，
每进程计费会静默归零:

```bash
sudo cp target/release/ltop-bpf /usr/local/bin/
sudo systemctl restart ltop-bpf
```

## 验证

```bash
ltop --version          # TUI/CLI 版本
ltop --json | head -c 300    # 一份 schema-v1 快照；"meta.udp_helper": true
                        # 说明 eBPF 助手在跑（Linux）
sudo ltop-bpf --once    # 采集器冒烟测试: 采一次、打印、退出
```

## 源码构建

```bash
git clone https://github.com/uMaj35ty/ltop
cd ltop
cargo build --release
# 产物在 target/release/: ltop、ltop-bpf（Linux）
```

要求: Rust stable；Linux 上构建 eBPF 对象需要带 `bpf` target 的 clang
（没有会怎样见上文）。eBPF 探针内含 x86_64 与 aarch64 两种架构的 `pt_regs`
寄存器映射，全程不需要 nightly。

每个 release 在 QEMU/KVM 里验证（开箱二进制、kprobe 挂载、systemd 单元、
无助手时的静默降级）——矩阵见[兼容性](compat.md)。

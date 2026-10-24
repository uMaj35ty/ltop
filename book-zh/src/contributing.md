# 参与贡献

欢迎 issue 与 PR。本页汇总项目的工程约定,让第一次 PR 就能对齐第十次
PR 的标准。

## 环境准备

- Rust(stable 近期版);Linux 上还需要**带 BPF target 的 clang**——
  eBPF 对象由 `build.rs` 在构建时从 `bpf/net_bpf.c` 编译。macOS 只需要
  Xcode 工具链。
- `cargo test` 必须全过,`cargo clippy --all-targets -- -D warnings`
  必须零警告——**零警告是硬门槛**,CI 在 Linux x86_64、Linux aarch64、
  macOS 三个目标上强制执行。

## 评审与 CI 会坚持的约定

- **平台门控。** macOS 专属的 FFI 声明与函数一律加
  `#[cfg(target_os = "macos")]`——未门控的 extern 声明会把链接错误带进
  Linux 构建,而 Linux 侧 clippy 看不见 macOS 专属代码的 lint,所以平台
  相关改动必须在对应目标上验证(CI 的 `test-macos`/`test-aarch64` 就是
  为这个存在的)。
- **偏移类常量要有出处。** 结构偏移(kinfo_proc、socket_fdinfo、
  rusage_info_v2 等)以 SDK 头文件为准并经真机验证;注释里写明来源,
  复核时读头文件本身,不要转信二手值。
- **不可信输入必须净化。** 来自 LAN(mDNS/NBNS 主机名)、本地进程、
  配置文件与外部 JSONL 的字符串不得裸写终端:TUI 走 ratatui(控制符被
  丢弃),CLI 输出走 `sanitize_text`。新增输出路径必须同样处理。
- **测试用真实 fixture,不互相模拟。** watch 有消费 `ltop watch` 真实
  输出的端到端测试;replay 同时带现行与旧格式 fixture;UI 测试断言渲染
  缓冲。修 bug 必须钉回归:改回旧代码确认测试会挂(突变验证)是修复的
  一部分。
- **界面双语。** 用户可见文案走 `src/config.rs` 的 `tr` 词典——英文与
  中文词条成对添加。盒标题与技术 token 按设计保留英文。
- **文档随代码。** 用户可见改动同步更新 mdBook(双语),定位性改动更新
  README,`CHANGELOG.md` 按既有风格书写。

## PR 流程

- 从 `master` 拉分支;一个 PR 只做一件事。
- 提交信息惯例:一行高密度摘要,讲清做了什么与为什么(见 `git log`)。
- CI 必须绿(三个 CI 目标的 test + clippy)。发版由维护者执行:tag →
  签名 tarball → Homebrew tap → crates.io → 文档站。

## 安全敏感改动

如果改动涉及 LAN 解析器、eBPF 助手、文件权限或[安全](security.md)
页面的任何内容,请在 PR 描述里明确指出。审计基线在仓库历史里(0.2.9);
请把你所保持的性质显式写出来——例如"LAN 提供的字符串永远不会未经
过滤地到达终端"。

# Contributing

Issues and PRs are welcome. This page collects the project's working
conventions so a first PR looks like the tenth one.

## Setup

- Rust (stable, current), plus **clang with the BPF target** on Linux —
  the eBPF object is compiled from `bpf/net_bpf.c` by `build.rs` at build
  time. macOS needs Xcode's toolchain only.
- `cargo test` must pass and `cargo clippy --all-targets -- -D warnings`
  must be clean — **zero warnings is a hard gate**, enforced by CI on
  Linux x86_64, Linux aarch64 and macOS.

## Conventions that CI and review will hold you to

- **Platform-gated code.** macOS-only FFI declarations and functions are
  `#[cfg(target_os = "macos")]` — ungated extern declarations pull link
  errors into the Linux build, and Linux clippy cannot see macOS-only
  lints, so platform-specific changes are verified on the matching target
  (CI's `test-macos`/`test-aarch64` jobs exist for exactly this).
- **Offset-style constants need receipts.** Structure offsets (kinfo_proc,
  socket_fdinfo, rusage_info_v2, …) are pinned against the SDK header and
  verified on real hardware; note the source in a comment, and double-check
  the header rather than trusting second-hand values.
- **Untrusted input is sanitized.** Strings from the LAN (mDNS/NBNS
  hostnames), local processes, config and external JSONL files never reach
  a terminal raw: the TUI goes through ratatui (control characters
  dropped), CLI output goes through `sanitize_text`. New output paths must
  do the same.
- **Tests are fixtures, not mocks of each other.** Watch has end-to-end
  tests that consume real `ltop watch` output; replay ships both current-
  and legacy-format fixtures; UI tests assert on rendered buffers. When you
  fix a bug, pin it: a test that fails against the old code (mutation
  check) is part of the fix.
- **Bilingual UI.** User-facing strings go through the `tr` dictionary in
  `src/config.rs` — add the English and Chinese entries together. Box
  titles and technical tokens stay English by design.
- **Docs follow code.** User-visible changes update the mdBook (both
  languages), the README when the pitch changes, and `CHANGELOG.md` in the
  established style.

## Pull requests

- Branch from `master`; keep PRs focused (one behavior per PR).
- The commit message convention is a dense summary line describing the
  what and the why (see `git log`).
- CI must be green (test + clippy on all three CI targets). The maintainer
  handles releases: tag → signed tarballs → Homebrew tap → crates.io →
  docs site.

## Security-sensitive changes

If your change touches the LAN parsers, the eBPF helper, file permissions
or anything in [Security](security.md), call it out in the PR description.
The audit baseline lives in the repository's history (0.2.9); keep the
property you're preserving stated explicitly — e.g. "LAN-supplied strings
never reach a terminal unfiltered".

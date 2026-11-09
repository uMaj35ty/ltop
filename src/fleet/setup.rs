//! `ltop fleet setup <target>`:密钥配置 doctor。
//!
//! 安全边界(与 SECURITY.md fleet 段一致):ltop 全程不经手密码——
//! 一次性授权交给 ssh-copy-id 在用户自己的终端里完成(TTY 原样交接,
//! 密码提示是 ssh 自己的);ltop 只做编排、验证与收尾建议。
//! host key 校验永不放松:指纹确认发生在 ssh-copy-id 自己的交互里。

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use anyhow::{bail, Result};

use super::cfg::DEFAULT_CMD;
use super::model::{fail_text, version_at_least, FailKind};
use super::poll::{control_path, pull_raw, ssh_args, Fetch};

const SETUP_TIMEOUT: Duration = Duration::from_secs(20);

/// 本机候选公钥,按偏好排序
fn find_pubkey() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let ssh = PathBuf::from(home).join(".ssh");
    let preferred = ["id_ed25519.pub", "id_ecdsa.pub", "id_rsa.pub"];
    for name in preferred {
        let p = ssh.join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    // 其余 *.pub 兜底(如 sk-*.pub)
    let mut others: Vec<PathBuf> = std::fs::read_dir(&ssh)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|x| x == "pub").unwrap_or(false))
        .collect();
    others.sort();
    others.into_iter().next()
}

pub fn run(args: &[String]) -> Result<()> {
    let zh = crate::cli_lang_zh();
    let Some(target) = args.first() else {
        bail!(crate::pick_s(
            zh,
            "usage: ltop fleet setup <ssh-target>",
            "用法:ltop fleet setup <ssh 目标>"
        ));
    };
    let target = target.clone();

    // ① 本机密钥
    let Some(pubkey) = find_pubkey() else {
        bail!(crate::pick_s(
            zh,
            "no public key in ~/.ssh — generate one first:\n  ssh-keygen -t ed25519 -C \"ltop fleet\"\nthen re-run: ltop fleet setup ",
            "~/.ssh 下没有公钥 — 先生成一把:\n  ssh-keygen -t ed25519 -C \"ltop fleet\"\n然后重跑: ltop fleet setup "
        ));
    };
    println!(
        "{}",
        crate::pick_s(
            zh,
            &format!("[1/4] using key {pubkey:?}"),
            &format!("[1/4] 使用密钥 {pubkey:?}")
        )
    );

    // ② 一次性授权(TTY 原样交给 ssh-copy-id,密码只发生在 ssh 自己的提示符里)
    let has_copy_id = Command::new("ssh-copy-id")
        .arg("--help")
        .output()
        .map(|o| o.status.success() || !o.stdout.is_empty() || !o.stderr.is_empty())
        .unwrap_or(false);
    if has_copy_id {
        println!(
            "{}",
            crate::pick_s(
                zh,
                "[2/4] running ssh-copy-id (enter the password at ssh's own prompt, if asked)",
                "[2/4] 执行 ssh-copy-id(如需密码,在 ssh 自己的提示符输入,ltop 看不见它)"
            )
        );
        let st = Command::new("ssh-copy-id").arg(&target).status()?;
        if !st.success() {
            bail!(crate::pick_s(
                zh,
                "ssh-copy-id failed — fix the cause above and re-run setup",
                "ssh-copy-id 失败 — 按上面的原因处理后重跑 setup"
            ));
        }
    } else {
        println!(
            "{}",
            crate::pick_s(
                zh,
                &format!(
                    "[2/4] ssh-copy-id not found — run this yourself, then re-run setup:\n  cat {pubkey:?} | ssh {target} 'mkdir -p ~/.ssh && chmod 700 ~/.ssh && cat >> ~/.ssh/authorized_keys && chmod 600 ~/.ssh/authorized_keys'"
                ),
                &format!(
                    "[2/4] 找不到 ssh-copy-id — 请自行执行后重跑 setup:\n  cat {pubkey:?} | ssh {target} 'mkdir -p ~/.ssh && chmod 700 ~/.ssh && cat >> ~/.ssh/authorized_keys && chmod 600 ~/.ssh/authorized_keys'"
                )
            )
        );
        bail!(crate::pick_s(
            zh,
            "public key not installed yet",
            "公钥尚未安装"
        ));
    }

    // ③ 验证:拉一份快照 + 解析远端 ltop 实际路径(force-command 要全路径)
    println!(
        "{}",
        crate::pick_s(
            zh,
            "[3/4] verifying ltop --json on the node",
            "[3/4] 验证节点 ltop --json"
        )
    );
    let master = true;
    let cpath = control_path(&target);
    let fetch = pull_raw(&target, DEFAULT_CMD, SETUP_TIMEOUT, master);
    let text = match fetch {
        Fetch::Ok { text } => text,
        Fetch::Fail { msg, .. } => bail!("{}", msg),
    };
    let snap: crate::snapshot::Snapshot = match serde_json::from_str(&text) {
        Ok(s) => s,
        Err(e) => bail!(
            "{}",
            fail_text(zh, FailKind::BadSnapshot, &target, &e.to_string())
        ),
    };
    println!(
        "{}",
        crate::pick_s(
            zh,
            &format!(
                "      ok: {} ({}, {}, ltop {}) — key auth works",
                snap.meta.hostname, snap.meta.os, snap.meta.arch, snap.meta.version
            ),
            &format!(
                "      成功:{}({}, {}, ltop {}) — 密钥认证已通",
                snap.meta.hostname, snap.meta.os, snap.meta.arch, snap.meta.version
            )
        )
    );
    if !version_at_least(&snap.meta.version, 0, 2, 3) {
        println!(
            "{}",
            crate::pick_s(
                zh,
                "      WARNING: node ltop is older than 0.2.3 (--json missing) — upgrade the node",
                "      警告:节点 ltop 低于 0.2.3(没有 --json)— 请升级节点"
            )
        );
    }
    // 远端实际二进制路径(非交互 PATH 是通用坑,force-command 必须全路径)
    let remote_path = Command::new("ssh")
        .args(ssh_args(&target, "command -v ltop", master, &cpath))
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty());

    // ④ 收尾:force-command 受限行 + 入表提示
    let pub_text = std::fs::read_to_string(&pubkey).unwrap_or_default();
    let pub_text = pub_text.trim();
    let remote_cmd = match &remote_path {
        Some(p) => format!("{p} --json"),
        None => format!(
            "{} --json {}",
            DEFAULT_CMD,
            crate::pick_s(
                zh,
                "(PATH may not resolve here — replace with the full path)",
                "(此处 PATH 可能解析不到 — 请换成全路径)"
            )
        ),
    };
    println!("\n{}", crate::pick_s(
        zh,
        "[4/4] optional hardening — replace the bare pubkey in the node's ~/.ssh/authorized_keys with a line locked to exactly this command (even a leaked key can then only read metrics):",
        "[4/4] 可选硬化 — 把节点 ~/.ssh/authorized_keys 里刚装的裸公钥换成下面这行(锁死为单条命令,密钥泄漏也只能读指标):"
    ));
    println!("command=\"{remote_cmd}\",no-port-forwarding,no-agent-forwarding,no-X11-forwarding {pub_text}");
    println!(
        "\n{}",
        crate::pick_s(
            zh,
            &format!("done — add the node with: ltop fleet --host {target} --save"),
            &format!("完成 — 用以下命令入表: ltop fleet --host {target} --save")
        )
    );
    Ok(())
}

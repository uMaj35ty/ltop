//! `ltop lan-setup`:macOS 27 邻居/网卡 MAC 门控的零特权退避开关——安装
//! 常驻用户 LaunchAgent,用 Apple 平台签名工具(/usr/sbin/arp、
//! /sbin/ifconfig)每 30s 轮转两份快照到 state 目录,LAN 盒 / NET 盒读
//! 文件补真值。判别器实测与门控模型见 [`crate::collect::lan_snapshot`]。
//!
//! 供应链面刻意最小:plist 只引用 Apple 平台二进制与安装时展开的固定
//! 路径(sh 单引号壳 + 单层 XML 转义),无网络;plist 0644、快照目录 0700、快照文件经
//! umask 077 落成 0600(与 0.2.9 采集文件权限口径一致)。

#[cfg(target_os = "macos")]
use anyhow::Context;
use anyhow::{bail, Result};

#[cfg(target_os = "macos")]
const LABEL: &str = "io.github.umaj35ty.ltop.lan";
#[cfg(target_os = "macos")]
const INTERVAL_SECS: u32 = 30;

pub fn run(args: &[String]) -> Result<()> {
    let zh = crate::cli_lang_zh();
    let p = |en: &str, c: &str| crate::pick_s(zh, en, c);
    let remove = args.iter().any(|a| a == "--remove" || a == "-r");
    if args.iter().any(|a| !(a == "--remove" || a == "-r")) {
        bail!(
            "{}",
            p(
                "usage: ltop lan-setup [--remove]",
                "用法:ltop lan-setup [--remove]"
            )
        );
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = remove;
        bail!(
            "{}",
            p("ltop lan-setup is macOS only", "ltop lan-setup 仅限 macOS")
        );
    }
    #[cfg(target_os = "macos")]
    {
        if remove {
            remove_agent(zh)
        } else {
            install_agent(zh)
        }
    }
}

/// 脚本里要展开的快照目录(与读取方 log_dir() 同源,XDG_STATE_HOME 优先)
#[cfg(target_os = "macos")]
fn agent_script(dir: &std::path::Path) -> String {
    // 原样嵌入 + sh 单引号壳('\''转义,$/反引号/双引号全部失效);
    // XML 转义只在 render_plist 一层做——两层转义会让含 & 的合法路径
    // 在 launchd 解码后残留实体,快照静默写不出来(0.2.10 review #2/#3)
    let d = dir.display().to_string().replace('\'', "'\\''");
    format!(
        "umask 077; /usr/sbin/arp -a -n > '{d}/lan-arp.tmp' \
         && /bin/mv '{d}/lan-arp.tmp' '{d}/lan-arp.txt'; \
         /sbin/ifconfig > '{d}/lan-if.tmp' \
         && /bin/mv '{d}/lan-if.tmp' '{d}/lan-if.txt'"
    )
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn render_plist(label: &str, script: &str, interval_secs: u32) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \x20 <key>Label</key><string>{}</string>\n\
         \x20 <key>ProgramArguments</key>\n\
         \x20 <array>\n\
         \x20   <string>/bin/sh</string>\n\
         \x20   <string>-c</string>\n\
         \x20   <string>{}</string>\n\
         \x20 </array>\n\
         \x20 <key>StartInterval</key><integer>{interval_secs}</integer>\n\
         \x20 <key>RunAtLoad</key><true/>\n\
         </dict>\n\
         </plist>\n",
        xml_escape(label),
        xml_escape(script)
    )
}

#[cfg(target_os = "macos")]
fn launchctl(args: &[&str]) -> Result<std::process::Output> {
    // 全路径:非交互 ssh/launchd 环境无 PATH(fleet 实测的通用坑)
    Command::new("/bin/launchctl")
        .args(args)
        .output()
        .context("launchctl")
}

#[cfg(target_os = "macos")]
fn install_agent(zh: bool) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let p = |en: &str, c: &str| crate::pick_s(zh, en, c);
    let dir = crate::log::log_dir();
    std::fs::create_dir_all(&dir).with_context(|| {
        p(
            &format!("cannot create {}", dir.display()),
            &format!("建不了目录 {}", dir.display()),
        )
    })?;
    let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));

    let home = std::env::var("HOME").with_context(|| {
        p(
            "HOME is not set; cannot locate LaunchAgents",
            "未设置 HOME,定位不到 LaunchAgents",
        )
    })?;
    let plist = std::path::PathBuf::from(home)
        .join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist"));
    std::fs::create_dir_all(plist.parent().expect("plist parent")).with_context(|| {
        p(
            &format!("cannot create {}", plist.parent().unwrap().display()),
            &format!("建不了目录 {}", plist.parent().unwrap().display()),
        )
    })?;
    std::fs::write(
        &plist,
        render_plist(LABEL, &agent_script(&dir), INTERVAL_SECS),
    )
    .with_context(|| {
        p(
            &format!("cannot write {}", plist.display()),
            &format!("写不了 {}", plist.display()),
        )
    })?;

    let uid = unsafe { libc::getuid() };
    // 重装幂等:先 bootout(未加载时报错,忽略)再 bootstrap
    let _ = launchctl(&["bootout", &format!("gui/{uid}/{LABEL}")]);
    let out = launchctl(&[
        "bootstrap",
        &format!("gui/{uid}"),
        plist.to_string_lossy().as_ref(),
    ])?;
    if !out.status.success() {
        bail!(
            "{}",
            p(
                &format!(
                    "launchctl bootstrap failed: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ),
                &format!(
                    "launchctl bootstrap 失败:{}",
                    String::from_utf8_lossy(&out.stderr).trim()
                )
            )
        );
    }
    println!(
        "{}",
        p(
            &format!(
                "installed {LABEL}\n\
                 snapshot dir : {}\n\
                 LAN/NET boxes show real MACs within ~{}s.\n\
                 remove with  : ltop lan-setup --remove",
                dir.display(),
                INTERVAL_SECS
            ),
            &format!(
                "已安装 {LABEL}\n\
                 快照目录:{}\n\
                 LAN/NET 盒最迟 ~{}s 内显示真实 MAC。\n\
                 移除:ltop lan-setup --remove",
                dir.display(),
                INTERVAL_SECS
            )
        )
    );
    Ok(())
}

#[cfg(target_os = "macos")]
fn remove_agent(zh: bool) -> Result<()> {
    let p = |en: &str, c: &str| crate::pick_s(zh, en, c);
    let uid = unsafe { libc::getuid() };
    let _ = launchctl(&["bootout", &format!("gui/{uid}/{LABEL}")]);
    let home = std::env::var("HOME").with_context(|| {
        p(
            "HOME is not set; cannot locate LaunchAgents",
            "未设置 HOME,定位不到 LaunchAgents",
        )
    })?;
    let plist = std::path::PathBuf::from(home)
        .join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist"));
    match std::fs::remove_file(&plist) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(e).with_context(|| {
                p(
                    &format!("cannot remove {}", plist.display()),
                    &format!("删不了 {}", plist.display()),
                )
            })
        }
    }
    let dir = crate::log::log_dir();
    for f in [
        crate::collect::lan_snapshot::ARP_FILE,
        crate::collect::lan_snapshot::IF_FILE,
        "lan-arp.tmp",
        "lan-if.tmp",
    ] {
        let _ = std::fs::remove_file(dir.join(f));
    }
    println!(
        "{}",
        p(
            &format!("removed {LABEL} (snapshot files deleted)"),
            &format!("已移除 {LABEL}(快照文件一并删除)")
        )
    );
    Ok(())
}

#[cfg(target_os = "macos")]
use std::process::Command;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_escapes_xml_sensitive_bytes() {
        let script = "a > b & c < d";
        let plist = render_plist("io.test.label", script, 30);
        assert!(plist.contains("a &gt; b &amp; c &lt; d"));
        assert!(!plist.contains("a > b"));
        assert!(
            !plist.contains("&amp;amp;"),
            "脚本层不得预转义(双层转义=合法路径静默失败)"
        );
        assert!(plist.contains("<integer>30</integer>"));
        assert!(plist.contains("<string>io.test.label</string>"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn script_references_both_rotations() {
        let s = agent_script(std::path::Path::new("/tmp/x"));
        assert!(s.contains("/usr/sbin/arp -a -n"));
        assert!(s.contains("/sbin/ifconfig"));
        assert!(s.contains("'/tmp/x/lan-arp.txt'"), "sh 单引号壳");
        assert!(s.contains("'/tmp/x/lan-if.txt'"));
        assert!(s.contains("umask 077"));
        assert!(s.contains("/bin/mv"));
    }
}

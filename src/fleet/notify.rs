//! fleet 通知传输:系统通知,零依赖 shell-out(herdr 同款思路——
//! Linux `notify-send`,macOS `osascript display notification`)。
//! 发送失败静默放弃:告警状态照样进 TUI/CLI,通知只是旁路;
//! 决不因通知环境残缺拖累轮询。

use std::process::{Command, Stdio};

/// macOS AppleScript 字符串转义(纯函数;正文来自远端数据,必须先过
/// sanitize_text 再到这里——本函数只处理引号与反斜杠)
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn escape_applescript(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            _ => out.push(c),
        }
    }
    out
}

/// notify-send 参数(纯函数,测试钉住 app-name 与参数顺序)
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub fn notify_send_args(title: &str, body: &str) -> Vec<String> {
    vec![
        "--app-name".into(),
        "ltop".into(),
        "--expire-time".into(),
        "8000".into(),
        title.into(),
        body.into(),
    ]
}

/// osascript 脚本(纯函数)
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn osascript_script(title: &str, body: &str) -> String {
    let t = escape_applescript(title);
    let b = escape_applescript(body);
    format!("display notification \"{b}\" with title \"{t}\"")
}

/// 平台缺省发送器:Linux notify-send / macOS osascript,都不在 PATH
/// 就放弃
pub fn platform_send(bin: &str, args: &[String]) -> bool {
    let out = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .output();
    matches!(out, Ok(o) if o.status.success())
}

/// 按当前平台发一条系统通知;true = 已交给平台(或发送成功)
pub fn send(title: &str, body: &str) -> bool {
    let t = crate::sanitize_text(title);
    let b = crate::sanitize_text(body);
    #[cfg(target_os = "macos")]
    {
        platform_send("osascript", &["-e".to_string(), osascript_script(&t, &b)])
    }
    #[cfg(not(target_os = "macos"))]
    {
        platform_send("notify-send", &notify_send_args(&t, &b))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applescript_escapes_quotes_and_backslashes() {
        assert_eq!(
            escape_applescript(r#"he said "hi" \ ok"#),
            r#"he said \"hi\" \\ ok"#
        );
        assert_eq!(escape_applescript("plain"), "plain");
    }

    #[test]
    fn notify_send_args_shape() {
        let a = notify_send_args("t", "b");
        assert_eq!(&a[..2], &["--app-name".to_string(), "ltop".to_string()]);
        assert_eq!(&a[4..], &["t".to_string(), "b".to_string()]);
    }

    #[test]
    fn osascript_script_carries_escaped_text() {
        let s = osascript_script("ltop", "quote \" here");
        // 正文里的引号必须转义,否则脚本被截断
        assert!(s.contains("\\\" here"), "actual: {s}");
        assert!(s.starts_with("display notification "));
        assert!(s.ends_with("with title \"ltop\""));
    }

    #[test]
    fn send_survives_missing_binary() {
        // 一个必不存在的可执行名:发送失败返回 false 而非 panic
        assert!(!platform_send("definitely-not-a-binary-xyz", &["x".into()]));
    }
}

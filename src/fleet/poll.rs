//! fleet 拉取层:每节点一个 ssh 子进程拉 `ltop --json`。
//!
//! 三条硬规矩(0.2.8 nettop 教训在 fleet 尺度的应用):
//! 1. BatchMode:密钥不通立刻失败,绝不挂住等密码;
//! 2. 超时必须 kill 子进程——超时后孤儿 ssh 会越积越多;
//! 3. stdout/stderr 与等待并发读,管道写满不会反过来卡死子进程。
//!
//! ControlMaster(auto + ControlPersist)让后续轮次复用已认证连接,
//! 每轮成本从完整握手降到毫秒级;socket 放 XDG_RUNTIME_DIR(0700)。

use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::model::{aggregate, classify, fail_text, failed, FailKind, NodeSample};
use crate::snapshot::Snapshot;

/// 一轮拉取的结果:成功是 JSON 文本,失败是分类 + 已本地化的一行话
pub enum Fetch {
    Ok {
        text: String,
    },
    /// kind 供批次② UI 上色/图标(当前消费者只用 msg)
    Fail {
        #[allow(dead_code)]
        kind: FailKind,
        msg: String,
    },
}

/// 一轮拉取的完整产物:聚合样本 + 原始快照(失败轮 raw=None)
pub struct NodeRound {
    pub sample: NodeSample,
    pub raw: Option<Snapshot>,
}

/// 拉取并聚合成样本(轮询主路径;延迟 = 管理机侧从发起到收到输出)
pub fn pull(target: &str, cmd: &str, timeout: Duration, master: bool, zh: bool) -> NodeRound {
    let start = Instant::now();
    match run_ssh(target, cmd, timeout, master) {
        Fetch::Ok { text } => {
            let latency_ms = start.elapsed().as_millis() as u64;
            match serde_json::from_str::<Snapshot>(&text) {
                Ok(snap) => NodeRound {
                    sample: aggregate(target, &snap, latency_ms),
                    raw: Some(snap),
                },
                Err(e) => NodeRound {
                    sample: failed(
                        target,
                        fail_text(zh, FailKind::BadSnapshot, target, &e.to_string()),
                    ),
                    raw: None,
                },
            }
        }
        Fetch::Fail { msg, .. } => NodeRound {
            sample: failed(target, msg),
            raw: None,
        },
    }
}

/// 组 ssh 参数(纯函数,测试钉住行为;cmd 是独立 argv,永不过 shell 拼接)
pub fn ssh_args(target: &str, cmd: &str, master: bool, control_path: &str) -> Vec<String> {
    let mut v = vec![
        "-o".into(),
        "BatchMode=yes".into(),
        "-o".into(),
        "ConnectTimeout=5".into(),
    ];
    if master {
        v.extend([
            "-o".into(),
            "ControlMaster=auto".into(),
            "-o".into(),
            format!("ControlPath={control_path}"),
            "-o".into(),
            "ControlPersist=600".into(),
        ]);
    }
    v.push(target.into());
    v.push(cmd.into());
    v
}

/// ControlMaster socket 路径:运行时目录优先,回落 tmp;按 target 哈希隔离
pub fn control_path(target: &str) -> String {
    let base = std::env::var("XDG_RUNTIME_DIR")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let mut h: u64 = 0xcbf29ce484222325;
    for b in target.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    base.join(format!("ltop-fleet-{h:x}")).display().to_string()
}

/// 真正跑一轮 ssh:spawn → 并发读管道 → 带截止时间的等待,超时 kill
fn run_ssh(target: &str, cmd: &str, timeout: Duration, master: bool) -> Fetch {
    let args = ssh_args(target, cmd, master, &control_path(target));
    let child = Command::new("ssh")
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            return Fetch::Fail {
                kind: FailKind::Other,
                msg: fail_text(
                    crate::cli_lang_zh(),
                    FailKind::Other,
                    target,
                    &format!("ssh: {e}"),
                ),
            }
        }
    };
    let (status, timed_out, out, err) = wait_capped(&mut child, timeout);
    let stdout = String::from_utf8_lossy(&out).into_owned();
    let stderr = String::from_utf8_lossy(&err).into_owned();
    let zh = crate::cli_lang_zh();
    if timed_out {
        return Fetch::Fail {
            kind: FailKind::Timeout,
            msg: fail_text(
                zh,
                FailKind::Timeout,
                target,
                &crate::pick_s(
                    zh,
                    &format!("no response within {}s", timeout.as_secs()),
                    &format!("{}s 内无响应", timeout.as_secs()),
                ),
            ),
        };
    }
    match status {
        Some(st) if st.success() => Fetch::Ok { text: stdout },
        Some(st) => {
            let kind = classify(&stderr, st.code());
            let first = stderr.lines().next().unwrap_or("");
            Fetch::Fail {
                kind,
                msg: fail_text(zh, kind, target, first),
            }
        }
        None => Fetch::Fail {
            kind: FailKind::Other,
            msg: fail_text(zh, FailKind::Other, target, "ssh wait failed"),
        },
    }
}

/// 带截止时间的子进程等待:两个读线程吃管道,主线程轮询 try_wait,
/// 到点 kill + wait(子进程死了管道自然 EOF,读线程随即收尾)。
/// 返回 (退出状态, 是否超时, stdout, stderr)。
fn wait_capped(
    child: &mut Child,
    timeout: Duration,
) -> (Option<ExitStatus>, bool, Vec<u8>, Vec<u8>) {
    let (tx_out, rx_out) = mpsc::channel();
    let (tx_err, rx_err) = mpsc::channel();
    if let Some(mut s) = child.stdout.take() {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = s.read_to_end(&mut buf);
            let _ = tx_out.send(buf);
        });
    }
    if let Some(mut s) = child.stderr.take() {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = s.read_to_end(&mut buf);
            let _ = tx_err.send(buf);
        });
    }
    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Some(st),
            Ok(None) if Instant::now() >= deadline => {
                timed_out = true;
                let _ = child.kill();
                break child.wait().ok();
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(30)),
            Err(_) => break None,
        }
    };
    // 子进程已死(或被杀),管道写端关闭,读线程必然很快返回
    let out = rx_out
        .recv_timeout(Duration::from_secs(5))
        .unwrap_or_default();
    let err = rx_err
        .recv_timeout(Duration::from_secs(5))
        .unwrap_or_default();
    (status, timed_out, out, err)
}

/// setup doctor 需要拿一次原始文本(不走聚合)
pub fn pull_raw(target: &str, cmd: &str, timeout: Duration, master: bool) -> Fetch {
    run_ssh(target, cmd, timeout, master)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_args_shape() {
        let plain = ssh_args("air", "ltop --json", false, "/x/sock");
        assert_eq!(
            plain,
            vec![
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=5",
                "air",
                "ltop --json"
            ]
        );
        let m = ssh_args("a b", "ltop --json", true, "/x/s1");
        assert!(m.windows(2).any(|w| w == ["ControlMaster=auto", "-o"]));
        assert!(m.iter().any(|a| a == "ControlPath=/x/s1"));
        assert!(m.iter().any(|a| a == "ControlPersist=600"));
        // target 与 cmd 必须是独立 argv(永不过 shell)
        assert_eq!(*m.last().unwrap(), "ltop --json");
        assert!(m.contains(&"a b".to_string()));
    }

    #[test]
    fn control_path_distinguishes_targets() {
        let a = control_path("air");
        let b = control_path("fedora");
        assert_ne!(a, b);
        assert!(a.contains("ltop-fleet-"));
    }

    #[cfg(unix)]
    #[test]
    fn wait_capped_kills_on_deadline() {
        // sleep 直接作为子进程:kill 打在它自己身上,无孤儿
        let mut c = Command::new("sleep")
            .arg("30")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn sleep");
        let (status, timed_out, _, _) = wait_capped(&mut c, Duration::from_millis(300));
        assert!(timed_out);
        assert!(status.is_some(), "kill 后要收回退出状态,不留僵尸");
    }

    #[cfg(unix)]
    #[test]
    fn wait_capped_returns_output_of_fast_child() {
        let mut c = Command::new("sh")
            .args(["-c", "echo hello"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn sh");
        let (status, timed_out, out, _) = wait_capped(&mut c, Duration::from_secs(5));
        assert!(!timed_out);
        assert_eq!(status.and_then(|s| s.code()), Some(0));
        assert_eq!(String::from_utf8_lossy(&out).trim(), "hello");
    }
}

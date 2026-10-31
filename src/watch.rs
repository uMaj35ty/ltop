//! headless 采集:`ltop watch` — 复用 TUI 采样层(App update / schema v1),
//! JSONL 逐行追加(写到一半的行可跳过,崩溃安全)。输出与 TUI monlog
//! (ltop-log-*)分文件:默认 ltop-session-<ts>.jsonl,不会被 TUI 的保留期
//! 清理(ltop-log-*/session-* 前缀)碰到。

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};

use crate::app::App;

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

pub struct WatchOpts {
    pub interval: Duration,
    pub duration: Option<Duration>,
    pub out: Option<PathBuf>,
    pub stop: Option<PathBuf>,
    /// 聚焦:仅写名称匹配(子串,不区分大小写)的进程及其连接行
    pub proc: Option<String>,
    /// 产物保留天数(启动时清 state 目录里的旧 ltop-session-*.jsonl;
    /// 0 = 永久保留)。防磁盘侧无限累积
    pub retention_days: u32,
}

/// `10s` / `5m` / `2h`;纯数字按秒
pub fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    let (num, unit) = s.split_at(s.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(s.len()));
    let n: u64 = num.parse().ok()?;
    // checked:release 无溢出检查,回绕会把超大时长变成错误的短时长,
    // 进程静默提前结束
    let secs = match unit {
        "" | "s" => n,
        "m" => n.checked_mul(60)?,
        "h" => n.checked_mul(3600)?,
        _ => return None,
    };
    Some(Duration::from_secs(secs))
}

pub fn run(args: &[String]) -> Result<()> {
    let zh = crate::cli_lang_zh();
    let p = |en: &str, c: &str| crate::pick_s(zh, en, c);
    let mut opts = WatchOpts {
        interval: Duration::from_secs(5),
        duration: None,
        out: None,
        stop: None,
        proc: None,
        retention_days: 7,
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                println!("{}", watch_usage(zh));
                return Ok(());
            }
            "--interval" => {
                let v = args
                    .get(i + 1)
                    .context(p("--interval needs a value (seconds)", "--interval 需要一个秒数"))?;
                let n: u64 = v.parse().map_err(|_| {
                    anyhow::anyhow!(p(
                        &format!("--interval {v:?} is not a number"),
                        &format!("--interval {v:?} 不是秒数")
                    ))
                })?;
                opts.interval = Duration::from_secs(n.max(1));
                i += 1;
            }
            "--duration" => {
                let v = args.get(i + 1).context(p(
                    "--duration needs a value (e.g. 30s/5m/2h)",
                    "--duration 需要一个时长(如 30s/5m/2h)",
                ))?;
                opts.duration = Some(parse_duration(v).ok_or_else(|| {
                    anyhow::anyhow!(p(
                        &format!("--duration {v:?} is not parseable"),
                        &format!("--duration {v:?} 无法解析")
                    ))
                })?);
                i += 1;
            }
            "--out" | "--stop" | "--proc" => {
                let v = args.get(i + 1).context(format!(
                    "{},{}",
                    args[i],
                    p("needs a value", "需要一个值")
                ))?;
                match args[i].as_str() {
                    "--out" => opts.out = Some(PathBuf::from(v)),
                    "--stop" => opts.stop = Some(PathBuf::from(v)),
                    _ => opts.proc = Some(v.clone()),
                }
                i += 1;
            }
            "--retention" => {
                let v = args.get(i + 1).context(p(
                    "--retention needs a number of days (0 = keep forever)",
                    "--retention 需要一个天数(0 = 永久保留)",
                ))?;
                opts.retention_days = v.parse().map_err(|_| {
                    anyhow::anyhow!(p(
                        &format!("--retention {v:?} is not a number"),
                        &format!("--retention {v:?} 不是天数")
                    ))
                })?;
                i += 1;
            }
            a if a.starts_with("--") => {
                bail!(
                    "{}",
                    crate::pick_s(
                        crate::cli_lang_zh(),
                        &format!("ltop watch: unknown option {a} (try --help)"),
                        &format!("ltop watch: 未知选项 {a}(试 --help)")
                    )
                )
            }
            a => bail!(
                "{}",
                p(
                    &format!("ltop watch: unexpected argument {a:?} (subcommand takes options only; try --help)"),
                    &format!("ltop watch: 意外参数 {a:?}(子命令只收选项,try --help)")
                )
            ),
        }
        i += 1;
    }
    execute(opts, zh)?;
    Ok(())
}

/// 清理 state 目录里超过保留期的 ltop-session-*.jsonl(0 天 = 不清理)。
/// watch 产物前缀独立于 TUI monlog 清理,自己管自己的生命周期。
pub fn purge_in(dir: &std::path::Path, retention_days: u32) -> usize {
    if retention_days == 0 {
        return 0;
    }
    let cutoff = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
        .saturating_sub(retention_days as u64 * 86_400);
    let Ok(rd) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut removed = 0;
    for e in rd.flatten() {
        let path = e.path();
        // 时间戳可带 -<pid> 后缀(同秒碰撞防护的文件名)
        let Some(ts) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_prefix("ltop-session-"))
            .and_then(|n| n.strip_suffix(".jsonl"))
            .and_then(|n| n.split('-').next())
            .and_then(|n| n.parse::<u64>().ok())
        else {
            continue;
        };
        if ts < cutoff && std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

fn watch_usage(zh: bool) -> String {
    if zh {
        format!(
            "ltop watch v{} — headless 采集,schema v1 JSONL(AI/ltop report 的输入)

USAGE:
    ltop watch [OPTIONS]

    --interval <S>   采样间隔,秒(默认 5)
    --duration <T>   总时长,如 30s / 5m / 2h(缺省 = 采到 --stop 或 Ctrl-C)
    --out <FILE>     输出 JSONL(默认 ~/.local/state/ltop/ltop-session-<ts>.jsonl)
    --stop <FILE>    哨兵文件:文件出现即停止(停止后删除该文件)
    --retention <D>  保留期:启动时清理超过 D 天的旧采集文件(默认 7;0 = 永久)
    --proc <NAME>    聚焦:只写名称匹配(子串)的进程及其连接;按名追踪,
                     进程重启换 pid 不丢线
    -h, --help       本帮助

消费:ltop report <file>(摘要)/ jq(任意查询);TUI monlog(ltop-log-*)
同 schema,report 也能读。",
            env!("CARGO_PKG_VERSION")
        )
    } else {
        format!(
            "ltop watch v{} — headless capture to schema-v1 JSONL (input for ltop report / AI)

USAGE:
    ltop watch [OPTIONS]

    --interval <S>   sampling interval, seconds (default 5)
    --duration <T>   total duration, e.g. 30s / 5m / 2h (default: run until --stop or Ctrl-C)
    --out <FILE>     output JSONL (default ~/.local/state/ltop/ltop-session-<ts>.jsonl)
    --stop <FILE>    sentinel file: stop as soon as it appears (consumed on stop)
    --retention <D>  purge capture files older than D days at start (default 7; 0 = keep forever)
    --proc <NAME>    focus: record only processes matching NAME (substring) and their
                     connections; tracked by name — survives pid changes on restart
    -h, --help       this help

Consume: ltop report <file> (summary) / jq (arbitrary queries); TUI monitoring
logs (ltop-log-*) share the same schema — report reads them too.",
            env!("CARGO_PKG_VERSION")
        )
    }
}

fn default_out() -> PathBuf {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // -<pid> 后缀:同秒双实例不互相截断(截断者继续写旧偏移 = NUL 稀疏文件)
    crate::log::log_dir().join(format!("ltop-session-{secs}-{}.jsonl", std::process::id()))
}

fn proc_matches(filter: &Option<String>, name: &str) -> bool {
    match filter {
        None => true,
        Some(f) => name.to_lowercase().contains(&f.to_lowercase()),
    }
}

/// 采样直到停止条件;返回写出的行数。sleep 切小片以便信号及时生效。
pub(crate) fn execute(opts: WatchOpts, zh: bool) -> Result<usize> {
    let p = |en: &str, c: &str| crate::pick_s(zh, en, c);
    unsafe {
        // SIGINT/SIGTERM 优雅收尾:置位即停,最后一条快照完整落盘
        libc::signal(
            libc::SIGINT,
            on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGTERM,
            on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t,
        );
    }
    let mut app = App::new(None);
    // 两个样本起步:首个无速率,第二个起 rates 就绪(与 TUI / --json 同);
    // 计时从 warm-up 后开始,duration 全程有快照覆盖
    app.update();
    std::thread::sleep(opts.interval.min(Duration::from_millis(1200)));
    app.update();
    let start = Instant::now();

    let path = opts.out.clone().unwrap_or_else(default_out);
    // 产物保留期:启动时清理过期文件(当前写的文件时间戳是 now,永不会命中)
    let purged = purge_in(
        path.parent().unwrap_or(std::path::Path::new("/tmp")),
        opts.retention_days,
    );
    if purged > 0 {
        eprintln!(
            "{}",
            p(
                &format!("ltop watch: purged {purged} capture file(s) past retention"),
                &format!("ltop watch: 已清理 {purged} 个超过保留期的采集文件")
            )
        );
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| {
            p(
                &format!("cannot create output directory {}", dir.display()),
                &format!("建输出目录失败 {}", dir.display()),
            )
        })?;
    }
    let mut w = std::fs::File::create(&path).with_context(|| {
        p(
            &format!("cannot create {}", path.display()),
            &format!("创建 {} 失败", path.display()),
        )
    })?;
    // 采集文件记录全量连接对端/进程名,隐私敏感:显式 0600(同 monlog)
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let proc_js = opts
        .proc
        .as_deref()
        .map(|p| format!("\"{}\"", crate::log::json_str(p)))
        .unwrap_or_else(|| "null".into());
    let dur_js = opts
        .duration
        .map(|d| d.as_secs().to_string())
        .unwrap_or_else(|| "null".into());
    writeln!(
        w,
        "{{\"watch_start\":{secs},\"host\":\"{}\",\"interval_s\":{},\"proc\":{proc_js},\"duration_s\":{dur_js}}}",
        crate::log::json_str(&app.hostname),
        opts.interval.as_secs(),
    )?;

    let mut lines = 0usize;
    let mut reason = p("duration reached", "duration 到点");
    'outer: loop {
        // 先落盘再睡:warm-up 第二个样本即窗口基线;信号/duration 到点时
        // 最后一个快照已在文件里,窗口不缺头尾
        let mut snap = crate::snapshot::from_app(&app);
        if opts.proc.is_some() {
            let f = &opts.proc;
            snap.procs.retain(|p| proc_matches(f, &p.name));
            snap.conns
                .retain(|c| c.process.as_deref().is_some_and(|pn| proc_matches(f, pn)));
        }
        writeln!(w, "{}", serde_json::to_string(&snap).unwrap_or_default())?;
        lines += 1;
        if STOP.load(Ordering::SeqCst) {
            reason = p("SIGINT/SIGTERM received", "收到 SIGINT/SIGTERM");
            break;
        }
        if let Some(sf) = &opts.stop {
            if sf.exists() {
                let _ = std::fs::remove_file(sf);
                reason = p("stop file seen", "哨兵文件出现");
                break;
            }
        }
        if let Some(d) = opts.duration {
            if start.elapsed() >= d {
                break;
            }
        }
        // 本片睡眠 = min(interval, duration 余量);切片睡以便信号及时醒来
        let budget = opts
            .duration
            .map(|d| d.saturating_sub(start.elapsed()))
            .unwrap_or(opts.interval)
            .min(opts.interval);
        let mut left = budget;
        while left > Duration::ZERO && !STOP.load(Ordering::SeqCst) {
            // 哨兵按切片粒度响应:interval 1h 时"出现即停"不再最多晚 1 小时
            if let Some(sf) = &opts.stop {
                if sf.exists() {
                    // 哨兵即消费:删掉避免下次启动立即停
                    let _ = std::fs::remove_file(sf);
                    reason = p("stop file seen", "哨兵文件出现");
                    break 'outer;
                }
            }
            let slice = left.min(Duration::from_millis(200));
            std::thread::sleep(slice);
            left = left.saturating_sub(slice);
        }
        if STOP.load(Ordering::SeqCst) {
            reason = p("SIGINT/SIGTERM received", "收到 SIGINT/SIGTERM");
            break;
        }
        app.update();
    }
    w.flush()?;
    eprintln!(
        "{}",
        p(
            &format!(
                "ltop watch: {} ({reason}), {lines} snapshots → {}",
                crate::app::fmt_dur(start.elapsed().as_secs()),
                path.display()
            ),
            &format!(
                "ltop watch: {}({reason}),{lines} 行快照 → {}",
                crate::app::fmt_dur(start.elapsed().as_secs()),
                path.display()
            )
        )
    );
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_durations() {
        assert_eq!(parse_duration("10s"), Some(Duration::from_secs(10)));
        assert_eq!(parse_duration("90"), Some(Duration::from_secs(90)));
        assert_eq!(parse_duration("5m"), Some(Duration::from_secs(300)));
        assert_eq!(parse_duration("2h"), Some(Duration::from_secs(7200)));
        assert_eq!(parse_duration("abc"), None);
        assert_eq!(parse_duration("5x"), None);
        assert_eq!(parse_duration(""), None);
    }

    /// 哨兵按切片粒度生效:interval 10s 时中途出现的哨兵秒级停止,
    /// 不再等下一个整间隔边界(0.2.6 最多晚一整个 interval)
    #[test]
    fn stop_sentinel_honored_within_slice_granularity() {
        let dir = std::env::temp_dir().join(format!("ltop-watch-stop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("w.jsonl");
        let sf = dir.join("stop.now");
        let sf2 = sf.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(500));
            std::fs::write(&sf2, b"").unwrap();
        });
        let t0 = std::time::Instant::now();
        let lines = execute(
            WatchOpts {
                interval: Duration::from_secs(10),
                duration: None,
                out: Some(out.clone()),
                stop: Some(sf.clone()),
                proc: None,
                retention_days: 0,
            },
            true,
        )
        .unwrap();
        let elapsed = t0.elapsed();
        assert!(
            elapsed < Duration::from_secs(5),
            "哨兵应秒级生效,实测 {elapsed:?}"
        );
        assert_eq!(lines, 1, "写完首快照即停,不等下一个间隔边界");
        assert!(!sf.exists(), "哨兵被消费");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 端到端:短 duration 产出 表头 + ≥2 快照行,末行 schema v1 可反序列化
    #[test]
    fn short_duration_writes_valid_jsonl() {
        let dir = std::env::temp_dir().join(format!("ltop-watch-e2e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("w.jsonl");
        let lines = execute(
            WatchOpts {
                interval: Duration::from_secs(1),
                duration: Some(Duration::from_secs(2)),
                out: Some(out.clone()),
                stop: None,
                proc: None,
                retention_days: 0,
            },
            true,
        )
        .unwrap();
        assert!(lines >= 2, "2s@1s 至少 2 行,实测 {lines}");
        let content = std::fs::read_to_string(&out).unwrap();
        let mut rows = content.lines();
        let header = rows.next().unwrap();
        assert!(header.contains("\"watch_start\""), "首行是表头:{header}");
        let last = content.lines().last().unwrap();
        let v: serde_json::Value = serde_json::from_str(last).unwrap();
        assert_eq!(v["schema"], 1, "末行是 schema v1 快照");
        // 主机名随机器不同(开发机/Air/CI),只断言非空
        assert!(
            v["meta"]["hostname"]
                .as_str()
                .is_some_and(|h| !h.is_empty()),
            "meta.hostname 非空"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 聚焦模式:文件里只留匹配进程的行(测试环境无同名进程时 procs 为空,
    /// 但 schema 行仍在 —— 断言过滤路径不炸、行数正常)
    #[test]
    fn proc_focus_writes_schema_rows() {
        let dir = std::env::temp_dir().join(format!("ltop-watch-focus-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("w.jsonl");
        let lines = execute(
            WatchOpts {
                interval: Duration::from_secs(1),
                duration: Some(Duration::from_secs(2)),
                out: Some(out.clone()),
                stop: None,
                proc: Some("zzz-no-such-proc".into()),
                retention_days: 0,
            },
            true,
        )
        .unwrap();
        assert!(lines >= 2);
        let content = std::fs::read_to_string(&out).unwrap();
        for row in content.lines().skip(1) {
            let v: serde_json::Value = serde_json::from_str(row).unwrap();
            assert!(
                v["procs"].as_array().unwrap().iter().all(|p| {
                    p["name"]
                        .as_str()
                        .unwrap()
                        .to_lowercase()
                        .contains("zzz-no-such-proc")
                }),
                "聚焦模式下不应有未匹配进程"
            );
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

#[cfg(test)]
mod purge_tests {
    use super::*;

    #[test]
    fn purge_removes_only_old_session_files() {
        let dir = std::env::temp_dir().join(format!("ltop-purge-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        // 旧 session 文件(8 天前)、新文件、别的名字、别的后缀
        let old = format!("ltop-session-{}.jsonl", now - 8 * 86_400);
        let new = format!("ltop-session-{}.jsonl", now - 86_400);
        for (name, _) in [
            (old.clone(), now - 8 * 86_400),
            (new.clone(), now - 86_400),
            ("ltop-log-999.jsonl".to_string(), now - 30 * 86_400), // monlog 前缀,不属于 watch
            ("ltop-session-notats.jsonl".to_string(), now - 30 * 86_400),
        ] {
            std::fs::write(dir.join(name), b"{}").unwrap();
        }
        assert_eq!(purge_in(&dir, 7), 1, "只清 8 天前的那个");
        assert!(!dir.join(&old).exists());
        assert!(dir.join(&new).exists());
        assert!(dir.join("ltop-log-999.jsonl").exists(), "monlog 前缀不动");
        assert!(
            dir.join("ltop-session-notats.jsonl").exists(),
            "无时间戳文件名不动"
        );
        // 0 = 不清理
        assert_eq!(purge_in(&dir, 0), 0);
        assert!(dir.join(&new).exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

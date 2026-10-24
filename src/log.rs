//! Session monitoring log: periodic JSONL snapshots of per-process totals
//! (network bytes + disk bytes) for post-hoc analysis of abnormal processes.

use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub struct MonLog {
    pub enabled: bool,
    pub interval: Duration,
    /// days to keep session files; 0 = never delete
    pub retention_days: u32,
    last: Instant,
    file: Option<(PathBuf, File)>,
    host: String,
}

impl MonLog {
    pub fn new(interval: Duration, retention_days: u32, host: String) -> Self {
        Self {
            enabled: false,
            interval,
            retention_days,
            last: Instant::now(),
            file: None,
            host,
        }
    }

    /// Delete session logs older than the retention window. Returns the number
    /// of files removed. retention_days == 0 keeps everything.
    pub fn purge_old(&self) -> usize {
        if self.retention_days == 0 {
            return 0;
        }
        let cutoff = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
            .saturating_sub(self.retention_days as u64 * 86_400);
        let dir = log_dir();
        let Ok(rd) = fs::read_dir(&dir) else {
            return 0;
        };
        let mut removed = 0;
        for e in rd.flatten() {
            let path = e.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let Some(ts) = session_ts(name) else {
                continue;
            };
            if ts < cutoff && fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }
        removed
    }

    /// Start/stop recording; returns the log path when recording starts
    pub fn toggle(&mut self) -> Option<PathBuf> {
        if self.enabled {
            self.finalize();
            return None;
        }
        let dir = log_dir();
        if fs::create_dir_all(&dir).is_err() {
            return None;
        }
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        // -<pid> 后缀:同秒双实例不再互相截断(截断者继续写旧偏移会产出
        // NUL 稀疏文件,report 计为 skipped,静默腐蚀)
        let path = dir.join(format!("ltop-log-{secs}-{}.jsonl", std::process::id()));
        let Ok(f) = File::create(&path) else {
            return None;
        };
        // 会话日志记录全量连接对端/进程名/主机名,隐私敏感:显式 0600,
        // 不依赖 umask(多用户系统的共享 home 下 644 可跨用户读)
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
        }
        let mut file = (path.clone(), f);
        let host = json_str(&self.host);
        let iv = self.interval.as_secs();
        let _ = writeln!(
            file.1,
            "{{\"session_start\":{secs},\"host\":\"{host}\",\"interval_s\":{iv}}}"
        );
        self.file = Some(file);
        self.enabled = true;
        self.last = Instant::now();
        let _ = self.purge_old();
        Some(path)
    }

    /// Write a snapshot if the interval has elapsed (called every tick)
    pub fn maybe_write(&mut self, snap: &crate::snapshot::Snapshot) {
        if !self.enabled || self.last.elapsed() < self.interval {
            return;
        }
        self.last = Instant::now();
        self.write_snapshot_v2(snap);
    }

    /// Flush and close; report the log path on stderr so the user can find it
    pub fn finalize(&mut self) -> Option<PathBuf> {
        self.enabled = false;
        let (path, mut f) = self.file.take()?;
        let _ = f.flush();
        Some(path)
    }

    /// v2:全盒快照行(与 `ltop --json` 同 schema);tracked_only 时 procs
    /// 仅记录置顶进程
    fn write_snapshot_v2(&mut self, snap: &crate::snapshot::Snapshot) {
        let Some((_, f)) = self.file.as_mut() else {
            return;
        };
        let _ = writeln!(f, "{}", serde_json::to_string(snap).unwrap_or_default());
    }
}

pub(crate) fn log_dir() -> PathBuf {
    if let Ok(x) = std::env::var("XDG_STATE_HOME") {
        if !x.is_empty() {
            return PathBuf::from(x).join("ltop");
        }
    }
    match std::env::var("HOME") {
        Ok(h) if !h.is_empty() => PathBuf::from(h).join(".local/state/ltop"),
        _ => PathBuf::from("/tmp/ltop"),
    }
}

/// unix ts from a `<prefix>-<ts>[-<pid>].jsonl` file name(新旧名兼容)。
/// monlog 清理只认 ltop-log-/session-(watch 产物 ltop-session-* 不归它管)
fn session_ts(name: &str) -> Option<u64> {
    parse_prefixed_ts(name, &["ltop-log-", "session-"])
}

/// replay 的会话文件发现:三类前缀都算(TUI monlog、watch 采集、旧版裸名)
pub(crate) fn session_file_ts(name: &str) -> Option<u64> {
    parse_prefixed_ts(name, &["ltop-log-", "ltop-session-", "session-"])
}

fn parse_prefixed_ts(name: &str, prefixes: &[&str]) -> Option<u64> {
    let stem = prefixes
        .iter()
        .find_map(|p| name.strip_prefix(p))?
        .strip_suffix(".jsonl")?;
    // 新文件名带 -<pid> 后缀(同秒碰撞防护);旧名是纯秒
    stem.split('-').next()?.parse::<u64>().ok()
}

pub(crate) fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_session_file_names() {
        assert_eq!(session_ts("session-1792368483.jsonl"), Some(1792368483));
        assert_eq!(session_ts("ltop-log-1792368483.jsonl"), Some(1792368483));
        // -<pid> 后缀(同秒碰撞防护)与纯秒旧名都认
        assert_eq!(session_ts("ltop-log-1792368483-1234.jsonl"), Some(1792368483));
        assert_eq!(session_ts("session-abc.jsonl"), None);
        assert_eq!(session_ts("other.jsonl"), None);
        assert_eq!(session_ts("session-12.txt"), None);
        // monlog 清理不碰 watch 采集产物
        assert_eq!(session_ts("ltop-session-1792368483.jsonl"), None);
        // replay 发现:三类前缀都认
        assert_eq!(
            session_file_ts("ltop-session-1792368483-99.jsonl"),
            Some(1792368483)
        );
        assert_eq!(session_file_ts("ltop-log-1792368483.jsonl"), Some(1792368483));
        assert_eq!(session_file_ts("session-1792368483.jsonl"), Some(1792368483));
        assert_eq!(session_file_ts("other.jsonl"), None);
    }
}

//! Session monitoring log: periodic JSONL snapshots of per-process totals
//! (network bytes + disk bytes) for post-hoc analysis of abnormal processes.

use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::collect::proc::ProcRow;

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
        let path = dir.join(format!("session-{secs}.jsonl"));
        let Ok(f) = File::create(&path) else {
            return None;
        };
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
    pub fn maybe_write(&mut self, procs: &[ProcRow]) {
        if !self.enabled || self.last.elapsed() < self.interval {
            return;
        }
        self.last = Instant::now();
        self.write_snapshot(procs);
    }

    /// Flush and close; report the log path on stderr so the user can find it
    pub fn finalize(&mut self) {
        if let Some((path, mut f)) = self.file.take() {
            let _ = f.flush();
            eprintln!("ltop: 监控日志已保存到 {}", path.display());
        }
        self.enabled = false;
    }

    fn write_snapshot(&mut self, procs: &[ProcRow]) {
        let Some((_, f)) = self.file.as_mut() else {
            return;
        };
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let mut line = format!("{{\"ts\":{secs},\"procs\":[");
        let mut first = true;
        for p in procs {
            if p.total_bytes() == 0 {
                continue;
            }
            if !first {
                line.push(',');
            }
            first = false;
            let name = json_str(&p.name);
            line.push_str(&format!(
                "{{\"pid\":{},\"name\":\"{name}\",\"net_recv\":{},\"net_sent\":{},\"udp_recv\":{},\"udp_sent\":{},\"disk_r\":{},\"disk_w\":{}}}",
                p.pid, p.net_recv, p.net_sent, p.udp_recv, p.udp_sent, p.disk_r, p.disk_w
            ));
        }
        line.push_str("]}");
        let _ = writeln!(f, "{line}");
    }
}

fn log_dir() -> PathBuf {
    if let Ok(x) = std::env::var("XDG_STATE_HOME") {
        if !x.is_empty() {
            return PathBuf::from(x).join("localist-top");
        }
    }
    match std::env::var("HOME") {
        Ok(h) if !h.is_empty() => PathBuf::from(h).join(".local/state/localist-top"),
        _ => PathBuf::from("/tmp/localist-top"),
    }
}

/// unix ts from a `session-<ts>.jsonl` file name
fn session_ts(name: &str) -> Option<u64> {
    name.strip_prefix("session-")
        .and_then(|r| r.strip_suffix(".jsonl"))
        .and_then(|r| r.parse::<u64>().ok())
}

fn json_str(s: &str) -> String {
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
        assert_eq!(session_ts("session-abc.jsonl"), None);
        assert_eq!(session_ts("other.jsonl"), None);
        assert_eq!(session_ts("session-12.txt"), None);
    }
}

//! Replay / analysis of session JSONL logs written by `MonLog`
//! (`~/.local/state/localist-top/session-*.jsonl`).
//!
//! The log stores cumulative per-process counters per snapshot, so replay
//! reconstructs recording-period increments as deltas between consecutive
//! snapshots (counter resets from a restarted pid are clamped at zero).

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};

const DEFAULT_TOP: usize = 15;

#[derive(Debug, Default, Clone)]
pub struct ProcAgg {
    pub pid: u32,
    pub name: String,
    pub first_ts: u64,
    pub last_ts: u64,
    pub samples: u32,
    /// recording-period increments, bytes
    pub net_recv: u64,
    pub net_sent: u64,
    pub udp_recv: u64,
    pub udp_sent: u64,
    pub disk_r: u64,
    pub disk_w: u64,
    /// busiest single interval, net (recv+sent) bytes/sec
    pub peak_net_bps: f64,
}

impl ProcAgg {
    pub fn total(&self) -> u64 {
        self.net_recv + self.net_sent + self.disk_r + self.disk_w
    }
}

#[derive(Debug)]
pub struct Session {
    pub path: PathBuf,
    pub host: String,
    pub start: u64,
    pub interval_s: u64,
    pub samples: usize,
    pub procs: Vec<ProcAgg>,
    /// (ts, total net B/s) busiest intervals, sorted desc
    pub busy: Vec<(u64, f64)>,
}

/// `ltop --replay [FILE] [--top N] [--csv OUT]` — analyze one session log
/// (default: the newest in the state dir) and print a per-process report.
pub fn run_replay(args: &[String]) -> Result<()> {
    let mut file: Option<PathBuf> = None;
    let mut top = DEFAULT_TOP;
    let mut csv: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--replay" => {
                if let Some(n) = args.get(i + 1) {
                    if !n.starts_with('-') {
                        file = Some(PathBuf::from(n));
                        i += 1;
                    }
                }
            }
            "--top" => {
                if let Some(n) = args.get(i + 1).and_then(|v| v.parse::<usize>().ok()) {
                    top = n.max(1);
                    i += 1;
                }
            }
            "--csv" => {
                if let Some(n) = args.get(i + 1) {
                    if !n.starts_with('-') {
                        csv = Some(PathBuf::from(n));
                        i += 1;
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }

    let path = match file {
        Some(p) => p,
        None => newest_in(&log_dir()).context(concat!(
            "state 目录里没有会话日志；先用 `L` 键或 --log 录制，",
            "或指定文件：ltop --replay <session-*.jsonl>"
        ))?,
    };
    let ses = load_file(&path)?;
    print_report(&ses, top);
    if let Some(out) = csv {
        write_csv(&ses, &out)?;
        println!("CSV 已导出到 {}", out.display());
    }
    Ok(())
}

pub fn load_file(path: &std::path::Path) -> Result<Session> {
    let f = std::fs::File::open(path)
        .with_context(|| format!("打不开 {}", path.display()))?;
    let rd = std::io::BufReader::new(f);
    let mut ses = Session {
        path: path.to_path_buf(),
        host: String::new(),
        start: 0,
        interval_s: 0,
        samples: 0,
        procs: Vec::new(),
        busy: Vec::new(),
    };
    // (pid, name) -> previous snapshot counters
    let mut prev: HashMap<(u32, String), [u64; 6]> = HashMap::new();
    let mut prev_ts: Option<u64> = None;
    let mut interval_total: Vec<(u64, u64)> = Vec::new(); // (ts, net bytes in interval)

    for line in rd.lines() {
        let line = line?;
        if line.contains("\"session_start\"") {
            ses.start = num(&line, "session_start").unwrap_or(0);
            if let Some(h) = text(&line, "host") {
                ses.host = h;
            }
            ses.interval_s = num(&line, "interval_s").unwrap_or(0);
            continue;
        }
        let Some(ts) = num(&line, "ts") else {
            continue;
        };
        let procs = parse_procs(&line);
        ses.samples += 1;
        let dt = prev_ts.map_or(0, |t| ts.saturating_sub(t));
        let mut net_sum = 0u64;
        for sp in &procs {
            let key = (sp.pid, sp.name.clone());
            let cur = [sp.net_recv, sp.net_sent, sp.udp_recv, sp.udp_sent, sp.disk_r, sp.disk_w];
            let agg = ses
                .procs
                .iter_mut()
                .find(|a| a.pid == sp.pid && a.name == sp.name);
            let agg = match agg {
                Some(a) => a,
                None => {
                    ses.procs.push(ProcAgg {
                        pid: sp.pid,
                        name: sp.name.clone(),
                        first_ts: ts,
                        ..Default::default()
                    });
                    ses.procs.last_mut().unwrap()
                }
            };
            agg.last_ts = ts;
            agg.samples += 1;
            let base = prev.get(&key).map_or(cur, |p| *p);
            let mut d = [0u64; 6];
            for k in 0..6 {
                d[k] = cur[k].saturating_sub(base[k]);
            }
            agg.net_recv += d[0];
            agg.net_sent += d[1];
            agg.udp_recv += d[2];
            agg.udp_sent += d[3];
            agg.disk_r += d[4];
            agg.disk_w += d[5];
            net_sum += d[0] + d[1];
            if dt > 0 {
                let rate = (d[0] + d[1]) as f64 / dt as f64;
                if rate > agg.peak_net_bps {
                    agg.peak_net_bps = rate;
                }
            }
            prev.insert(key, cur);
        }
        if dt > 0 {
            interval_total.push((ts, net_sum));
        }
        prev_ts = Some(ts);
    }
    interval_total.sort_by_key(|&(_, bytes)| std::cmp::Reverse(bytes));
    ses.busy = interval_total
        .into_iter()
        .take(3)
        .map(|(ts, bytes)| {
            let rate = bytes as f64 / ses.interval_s.max(1) as f64;
            (ts, rate)
        })
        .collect();
    ses.procs.sort_by_key(|a| std::cmp::Reverse(a.total()));
    Ok(ses)
}

pub fn newest_in(dir: &std::path::Path) -> Option<PathBuf> {
    let mut best: Option<(u64, PathBuf)> = None;
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if let Some(ts) = name
            .strip_prefix("session-")
            .and_then(|r| r.strip_suffix(".jsonl"))
            .and_then(|r| r.parse::<u64>().ok())
        {
            if best.as_ref().is_none_or(|(b, _)| ts > *b) {
                best = Some((ts, p));
            }
        }
    }
    best.map(|(_, p)| p)
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

struct SnapProc {
    pid: u32,
    name: String,
    net_recv: u64,
    net_sent: u64,
    udp_recv: u64,
    udp_sent: u64,
    disk_r: u64,
    disk_w: u64,
}

/// Extract the objects of the `"procs":[...]` array from a snapshot line.
fn parse_procs(line: &str) -> Vec<SnapProc> {
    let Some(start) = line.find("\"procs\":") else {
        return Vec::new();
    };
    let bytes = line.as_bytes();
    let Some(mut i) = bytes[start..].iter().position(|&b| b == b'[').map(|p| start + p + 1) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    while i < bytes.len() {
        if bytes[i] == b']' {
            break;
        }
        if bytes[i] != b'{' {
            i += 1;
            continue;
        }
        // find the matching '}' honouring string escapes
        let mut depth = 0usize;
        let mut in_str = false;
        let mut esc = false;
        let mut j = i;
        while j < bytes.len() {
            let b = bytes[j];
            if in_str {
                if esc {
                    esc = false;
                } else if b == b'\\' {
                    esc = true;
                } else if b == b'"' {
                    in_str = false;
                }
            } else {
                match b {
                    b'"' => in_str = true,
                    b'{' => depth += 1,
                    b'}' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
            }
            j += 1;
        }
        if j >= bytes.len() {
            break;
        }
        let obj = &line[i..=j];
        if let Some(p) = decode_proc(obj) {
            out.push(p);
        }
        i = j + 1;
    }
    out
}

fn decode_proc(obj: &str) -> Option<SnapProc> {
    Some(SnapProc {
        pid: num(obj, "pid")? as u32,
        name: text(obj, "name").unwrap_or_default(),
        net_recv: num(obj, "net_recv").unwrap_or(0),
        net_sent: num(obj, "net_sent").unwrap_or(0),
        udp_recv: num(obj, "udp_recv").unwrap_or(0),
        udp_sent: num(obj, "udp_sent").unwrap_or(0),
        disk_r: num(obj, "disk_r").unwrap_or(0),
        disk_w: num(obj, "disk_w").unwrap_or(0),
    })
}

/// value of `"key":<number>` anywhere in a flat JSON object string
fn num(s: &str, key: &str) -> Option<u64> {
    let pat = format!("\"{key}\":");
    let mut from = 0;
    loop {
        let p = s[from..].find(&pat)? + from;
        let rest = &s[p + pat.len()..];
        let digits: String = rest
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if !digits.is_empty() {
            return digits.parse().ok();
        }
        from = p + pat.len();
    }
}

/// value of `"key":"..."` with the escapes written by `json_str` decoded
fn text(s: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\":\"");
    let p = s.find(&pat)? + pat.len();
    let mut out = String::new();
    let mut chars = s[p..].chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => break,
            '\\' => match chars.next()? {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                'u' => {
                    let hex: String = chars.by_ref().take(4).collect();
                    let v = u32::from_str_radix(&hex, 16).ok()?;
                    out.push(char::from_u32(v)?);
                }
                other => out.push(other),
            },
            other => out.push(other),
        }
    }
    Some(out)
}

pub fn print_report(ses: &Session, top: usize) {
    println!("== localist top 回放 ==");
    println!(
        "file  {}\nhost  {}  开始 {}  快照 {} 个  间隔 {}s",
        ses.path.display(),
        if ses.host.is_empty() { "—" } else { &ses.host },
        fmt_ts(ses.start),
        ses.samples,
        ses.interval_s
    );
    if ses.procs.is_empty() {
        println!("（日志里没有进程快照）");
        return;
    }
    println!(
        "\n{:<28} {:>9} {:>9} {:>9} {:>9} {:>10} {:>4}",
        "进程", "Σ收", "Σ发", "Σ读盘", "Σ写盘", "峰值网速", "采样"
    );
    for a in ses.procs.iter().take(top) {
        println!(
            "{:<28} {:>9} {:>9} {:>9} {:>9} {:>10} {:>4}",
            format!("{}({})", a.name, a.pid),
            crate::fmt_human(a.net_recv as f64),
            crate::fmt_human(a.net_sent as f64),
            crate::fmt_human(a.disk_r as f64),
            crate::fmt_human(a.disk_w as f64),
            format!("{}/s", crate::fmt_human(a.peak_net_bps)),
            a.samples
        );
    }
    let shown = ses.procs.len().min(top);
    if ses.procs.len() > shown {
        println!("（其余 {} 个进程未列出，--top N 调整）", ses.procs.len() - shown);
    }
    if !ses.busy.is_empty() {
        println!("\n最忙时段:");
        for (ts, rate) in &ses.busy {
            println!("  {}  {}/s", fmt_ts(*ts), crate::fmt_human(*rate));
        }
    }
}

fn write_csv(ses: &Session, out: &std::path::Path) -> Result<()> {
    let mut w = std::io::BufWriter::new(std::fs::File::create(out)
        .with_context(|| format!("创建 {} 失败", out.display()))?);
    writeln!(
        w,
        "name,pid,first_ts,last_ts,samples,net_recv,net_sent,udp_recv,udp_sent,disk_r,disk_w,peak_net_bps"
    )?;
    for a in &ses.procs {
        writeln!(
            w,
            "{},{},{},{},{},{},{},{},{},{},{},{}",
            csv_field(&a.name),
            a.pid,
            a.first_ts,
            a.last_ts,
            a.samples,
            a.net_recv,
            a.net_sent,
            a.udp_recv,
            a.udp_sent,
            a.disk_r,
            a.disk_w,
            a.peak_net_bps as u64
        )?;
    }
    w.flush()?;
    Ok(())
}

fn csv_field(s: &str) -> String {
    if s.contains('"') || s.contains(',') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

fn fmt_ts(ts: u64) -> String {
    let t = ts as i64 as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe {
        libc::localtime_r(&t, &mut tm);
    }
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = concat!(
        "{\"session_start\":1700000000,\"host\":\"omarchy\",\"interval_s\":60}\n",
        "{\"ts\":1700000060,\"procs\":[",
        "{\"pid\":100,\"name\":\"firefox\",\"net_recv\":1000,\"net_sent\":200,\"disk_r\":0,\"disk_w\":0},",
        "{\"pid\":200,\"name\":\"sync\",\"net_recv\":0,\"net_sent\":0,\"disk_r\":50,\"disk_w\":70}]}\n",
        "{\"ts\":1700000120,\"procs\":[",
        "{\"pid\":100,\"name\":\"firefox\",\"net_recv\":3000,\"net_sent\":600,\"disk_r\":0,\"disk_w\":0},",
        "{\"pid\":300,\"name\":\"new\",\"net_recv\":500,\"net_sent\":0,\"disk_r\":0,\"disk_w\":0}]}\n",
        "{\"ts\":1700000180,\"procs\":[",
        "{\"pid\":100,\"name\":\"firefox\",\"net_recv\":2500,\"net_sent\":600,\"disk_r\":0,\"disk_w\":0},",
        "{\"pid\":200,\"name\":\"sync\",\"net_recv\":0,\"net_sent\":0,\"disk_r\":90,\"disk_w\":70}]}\n",
    );

    fn load_str(tag: &str, s: &str) -> Session {
        let dir = std::env::temp_dir().join(format!("ltop-replay-test-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("session-1700000000.jsonl");
        std::fs::write(&p, s).unwrap();
        let ses = load_file(&p).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        ses
    }

    #[test]
    fn header_and_snapshots_parse() {
        let ses = load_str("header", SAMPLE);
        assert_eq!(ses.host, "omarchy");
        assert_eq!(ses.start, 1700000000);
        assert_eq!(ses.interval_s, 60);
        assert_eq!(ses.samples, 3);
    }

    #[test]
    fn deltas_accumulate_positive_and_clamp_resets() {
        let ses = load_str("deltas", SAMPLE);
        let ff = ses.procs.iter().find(|a| a.name == "firefox").unwrap();
        // 1000->3000 (+2000) then 3000->2500 (reset, clamped)
        assert_eq!(ff.net_recv, 2000);
        assert_eq!(ff.net_sent, 400);
        assert_eq!(ff.disk_r + ff.disk_w, 0);
        // peak: 2000+400=2400 bytes over 60s
        assert!((ff.peak_net_bps - 40.0).abs() < 1e-9);
        assert_eq!(ff.samples, 3);
    }

    #[test]
    fn new_process_baseline_is_first_snapshot() {
        let ses = load_str("baseline", SAMPLE);
        let new = ses.procs.iter().find(|a| a.name == "new").unwrap();
        assert_eq!(new.net_recv, 0); // first sighting records no delta
        assert_eq!(new.samples, 1);
    }

    #[test]
    fn disk_counters_count() {
        let ses = load_str("disk", SAMPLE);
        let sync = ses.procs.iter().find(|a| a.name == "sync").unwrap();
        // 50->90 (+40); write counter unchanged
        assert_eq!(sync.disk_r, 40);
        assert_eq!(sync.disk_w, 0);
        assert_eq!(sync.samples, 2);
    }

    #[test]
    fn busiest_interval_ranked() {
        let ses = load_str("busy", SAMPLE);
        assert_eq!(ses.busy[0].0, 1700000120);
        assert!((ses.busy[0].1 - 2400.0 / 60.0).abs() < 1e-9);
    }

    #[test]
    fn sorts_by_total_desc() {
        let ses = load_str("sort", SAMPLE);
        let totals: Vec<u64> = ses.procs.iter().map(|a| a.total()).collect();
        assert!(totals.windows(2).all(|w| w[0] >= w[1]));
    }

    #[test]
    fn udp_columns_are_parsed_and_exported() {
        // eBPF 落地后快照带 udp_recv/udp_sent;回放按同规则差分出分项增量
        let with_udp = concat!(
            "{\"session_start\":1700000000,\"host\":\"omarchy\",\"interval_s\":60}\n",
            "{\"ts\":1700000060,\"procs\":[",
            "{\"pid\":100,\"name\":\"ff\",\"net_recv\":1000,\"net_sent\":200,",
            "\"udp_recv\":800,\"udp_sent\":100,\"disk_r\":0,\"disk_w\":0}]}\n",
            "{\"ts\":1700000120,\"procs\":[",
            "{\"pid\":100,\"name\":\"ff\",\"net_recv\":2000,\"net_sent\":300,",
            "\"udp_recv\":1600,\"udp_sent\":150,\"disk_r\":0,\"disk_w\":0}]}\n",
        );
        let ses = load_str("udp_keys", with_udp);
        let ff = ses.procs.iter().find(|a| a.name == "ff").unwrap();
        assert_eq!(ff.net_recv, 1000);
        assert_eq!(ff.net_sent, 100);
        assert_eq!(ff.udp_recv, 800, "UDP 分项差分");
        assert_eq!(ff.udp_sent, 50);
        // 旧格式(无 udp 键)不炸,分项为 0
        let ses = load_str("udp_keys", SAMPLE);
        assert!(ses.procs.iter().all(|a| a.udp_recv + a.udp_sent == 0));
    }

    #[test]
    fn text_unescapes_json_strings() {
        assert_eq!(text("{\"name\":\"a\\\"b\\\\c\"}", "name").unwrap(), "a\"b\\c");
        assert_eq!(text("{\"name\":\"中文\"}", "name").unwrap(), "中文");
        assert_eq!(num("{\"ts\":123,\"x\":1}", "ts"), Some(123));
    }

    #[test]
    fn csv_quotes_fields_with_commas() {
        assert_eq!(csv_field("plain"), "plain");
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("say\"hi"), "\"say\"\"hi\"");
    }
}

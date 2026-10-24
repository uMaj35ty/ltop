//! 消费端摘要:`ltop report <file>` — 把 schema v1 JSONL(watch 产物 /
//! TUI monlog v2)聚合为窗口摘要。
//!
//! 窗口语义:「一段时间内的总和」= 末快照累计 − 首快照累计(按 pid/inode
//! 分段差分、clamp;等价于逐行差分求和)。峰值 = 窗口内逐行速率的最大值
//! (UDP 无速率字段,由 report 侧按逐行差分/dt 推出)。进程按名聚合
//! (重启换 pid 分段续算),连接按 inode(mac 为合成 flow hash)。
//! 旧 monlog v1(session-*,仅 PROCS、无 schema 字段)不进 report,走 --replay。

use std::collections::{HashMap, HashSet};
use std::io::BufRead;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use serde_json::json;

use crate::snapshot::{self, Snapshot};

const DEFAULT_TOP: usize = 5;

// ---------- 聚合结果 ----------

#[derive(Debug, Default)]
pub struct ProcAgg {
    pub name: String,
    pub net_recv: u64,
    pub net_sent: u64,
    pub peak_rx: f64,
    pub peak_tx: f64,
    pub udp_recv: u64,
    pub udp_sent: u64,
    pub peak_udp_rx: f64,
    pub peak_udp_tx: f64,
    pub disk_read: u64,
    pub disk_written: u64,
    pub peak_disk_r: f64,
    pub peak_disk_w: f64,
    /// 出现过的快照数(每快照每名字至多计 1;活跃时长 =
    /// (samples-1) × interval:首见快照是基线,尚未经历完整区间)
    pub samples: u32,
    /// 上一次计入 samples 的快照时间戳(同名多进程按行去重用)
    samples_since: u64,
    pub last_alive: bool,
}

impl ProcAgg {
    /// 排序键:网络流量(net+udp,收+发)。disk 不计入「流量」榜。
    fn traffic(&self) -> u64 {
        self.net_recv + self.net_sent + self.udp_recv + self.udp_sent
    }
}

#[derive(Debug)]
pub struct ConnAgg {
    pub inode: u64,
    pub proto: String,
    pub remote: String,
    pub process: Option<String>,
    pub state: String,
    pub recv: u64,
    pub sent: u64,
    pub peak_rx: f64,
    pub peak_tx: f64,
    pub first_ts: u64,
    pub last_ts: u64,
}

impl ConnAgg {
    fn traffic(&self) -> u64 {
        self.recv + self.sent
    }
}

#[derive(Debug)]
pub struct Report {
    pub file: PathBuf,
    pub host: String,
    pub start: u64,
    pub end: u64,
    pub snapshots: usize,
    /// 推断的采样间隔(快照间 dt 中位数);0 = 推断不出
    pub interval_s: u64,
    /// 跳过的行(表头 / 旧格式 / 半行)
    pub skipped: usize,
    /// 窗口 NIC 总流量(接口层口径,≠ 进程 payload Σ)
    pub nic_rx: u64,
    pub nic_tx: u64,
    pub peak_ts: u64,
    pub peak_rx_bps: f64,
    pub peak_tx_bps: f64,
    pub procs: Vec<ProcAgg>,
    pub conns: Vec<ConnAgg>,
}

// ---------- 过滤 ----------

/// `--proc`:子串(不区分大小写);含 `*` 时走简单通配(`chrome*`、`*render*`)
pub fn name_match(filter: &str, name: &str) -> bool {
    if filter.contains('*') {
        glob_match(&filter.to_lowercase(), &name.to_lowercase())
    } else {
        name.to_lowercase().contains(&filter.to_lowercase())
    }
}

/// 简单通配:`*` 任意段;首段前缀、末段后缀、中段顺序出现
fn glob_match(pat: &str, s: &str) -> bool {
    let parts: Vec<&str> = pat.split('*').collect();
    if parts.len() == 1 {
        return pat == s;
    }
    let Some(mut rest) = s.strip_prefix(parts[0]) else {
        return false;
    };
    for mid in &parts[1..parts.len() - 1] {
        match rest.find(mid) {
            Some(p) => rest = &rest[p + mid.len()..],
            None => return false,
        }
    }
    rest.ends_with(*parts.last().unwrap_or(&""))
}

/// `--fields net,udp,disk,conns` 组级枚举(默认全)
#[derive(Clone, Copy, Debug)]
pub struct Fields {
    pub net: bool,
    pub udp: bool,
    pub disk: bool,
    pub conns: bool,
}

impl Fields {
    pub const ALL: Self = Self {
        net: true,
        udp: true,
        disk: true,
        conns: true,
    };

    fn parse(s: &str, zh: bool) -> Result<Self> {
        let mut f = Self {
            net: false,
            udp: false,
            disk: false,
            conns: false,
        };
        for g in s.split(',') {
            match g.trim() {
                "net" => f.net = true,
                "udp" => f.udp = true,
                "disk" => f.disk = true,
                "conns" => f.conns = true,
                other => bail!(
                    "{}",
                    crate::pick_s(
                        zh,
                        &format!("unknown group {other:?} (available: net,udp,disk,conns)"),
                        &format!("未知组 {other:?}(可用:net,udp,disk,conns)")
                    )
                ),
            }
        }
        Ok(f)
    }
}

// ---------- 载入 ----------

/// 逐行读 JSONL,只收 schema v1 快照行;表头(watch_start/session_start)、
/// 旧 v1 行(无 schema 字段)、写到一半的行都计入 skipped
pub fn load(path: &std::path::Path, zh: bool) -> Result<(Vec<Snapshot>, usize)> {
    let f = std::fs::File::open(path).with_context(|| {
        crate::pick_s(
            zh,
            &format!("cannot open {}", path.display()),
            &format!("打不开 {}", path.display()),
        )
    })?;
    let mut out = Vec::new();
    let mut skipped = 0usize;
    for line in std::io::BufReader::new(f).lines() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            skipped += 1;
            continue;
        };
        if v.get("schema").and_then(|s| s.as_u64()) != Some(snapshot::SCHEMA as u64) {
            skipped += 1;
            continue;
        }
        match serde_json::from_value::<Snapshot>(v) {
            Ok(mut s) => {
                // 外部文件:名字/主机名进人读表与 --json 前剥控制符
                // (serde 会把 \u001b 解码为真实 ESC,print 路径裸写终端)
                s.meta.hostname = crate::sanitize_text(&s.meta.hostname);
                for p in &mut s.procs {
                    p.name = crate::sanitize_text(&p.name);
                }
                for c in &mut s.conns {
                    if let Some(pn) = &mut c.process {
                        *pn = crate::sanitize_text(pn);
                    }
                    c.state = crate::sanitize_text(&c.state);
                }
                out.push(s);
            }
            Err(_) => skipped += 1,
        }
    }
    Ok((out, skipped))
}

// ---------- 聚合 ----------

/// 纯聚合(输入 = 有效快照序列),供 run 与单测共用
pub fn build(snaps: Vec<Snapshot>, skipped: usize, file: PathBuf) -> Report {
    let host = snaps
        .first()
        .map(|s| s.meta.hostname.clone())
        .unwrap_or_default();
    let start = snaps.first().map(|s| s.meta.ts).unwrap_or(0);
    let end = snaps.last().map(|s| s.meta.ts).unwrap_or(0);

    let mut procs: HashMap<String, ProcAgg> = HashMap::new();
    // (name, pid) → 上一快照累计;分段差分,clamp 处理计数器重置
    let mut proc_prev: HashMap<(String, u32), [u64; 6]> = HashMap::new();
    // inode → (聚合, 上一快照在场, 上一累计);中途消失不计其后的窗口
    let mut conns: HashMap<u64, (ConnAgg, bool, (u64, u64))> = HashMap::new();
    let mut if_first: HashMap<String, (u64, u64)> = HashMap::new();
    let mut if_last: HashMap<String, (u64, u64)> = HashMap::new();
    let mut peak: Option<(u64, f64, f64)> = None;
    let mut dts: Vec<u64> = Vec::new();
    let mut prev_ts: Option<u64> = None;

    for s in &snaps {
        let ts = s.meta.ts;
        if let Some(pt) = prev_ts {
            if ts > pt {
                dts.push(ts - pt);
            }
        }

        for p in &s.procs {
            let cur = [
                p.net_recv,
                p.net_sent,
                p.udp_recv,
                p.udp_sent,
                p.disk_read,
                p.disk_written,
            ];
            let agg = procs.entry(p.name.clone()).or_insert_with(|| ProcAgg {
                name: p.name.clone(),
                ..Default::default()
            });
            let base = proc_prev
                .get(&(p.name.clone(), p.pid))
                .copied()
                .unwrap_or(cur);
            let d = [
                cur[0].saturating_sub(base[0]),
                cur[1].saturating_sub(base[1]),
                cur[2].saturating_sub(base[2]),
                cur[3].saturating_sub(base[3]),
                cur[4].saturating_sub(base[4]),
                cur[5].saturating_sub(base[5]),
            ];
            agg.net_recv += d[0];
            agg.net_sent += d[1];
            agg.udp_recv += d[2];
            agg.udp_sent += d[3];
            agg.disk_read += d[4];
            agg.disk_written += d[5];
            if p.net_bps_recv > agg.peak_rx {
                agg.peak_rx = p.net_bps_recv;
            }
            if p.net_bps_sent > agg.peak_tx {
                agg.peak_tx = p.net_bps_sent;
            }
            if p.disk_bps_read > agg.peak_disk_r {
                agg.peak_disk_r = p.disk_bps_read;
            }
            if p.disk_bps_written > agg.peak_disk_w {
                agg.peak_disk_w = p.disk_bps_written;
            }
            if let Some(pt) = prev_ts {
                if ts > pt {
                    let dt = (ts - pt) as f64;
                    if dt > 0.0 {
                        let ur = d[2] as f64 / dt;
                        let ut = d[3] as f64 / dt;
                        if ur > agg.peak_udp_rx {
                            agg.peak_udp_rx = ur;
                        }
                        if ut > agg.peak_udp_tx {
                            agg.peak_udp_tx = ut;
                        }
                    }
                }
            }
            // 每快照每名字至多计 1:同名多进程(chrome/nginx worker)每
            // 快照占多行,按行计数会把活跃时长虚增进程数倍、甚至超过
            // 窗口墙钟时长
            if agg.samples_since != ts {
                agg.samples_since = ts;
                agg.samples += 1;
            }
            agg.last_alive = p.alive;
            proc_prev.insert((p.name.clone(), p.pid), cur);
        }

        let mut seen: HashSet<u64> = HashSet::new();
        for c in &s.conns {
            let e = conns.entry(c.inode).or_insert_with(|| {
                (
                    ConnAgg {
                        inode: c.inode,
                        proto: c.proto.clone(),
                        remote: c.remote.clone(),
                        process: c.process.clone(),
                        state: c.state.clone(),
                        recv: 0,
                        sent: 0,
                        peak_rx: 0.0,
                        peak_tx: 0.0,
                        first_ts: ts,
                        last_ts: ts,
                    },
                    true,
                    (c.bytes_recv, c.bytes_sent),
                )
            });
            let (agg, present, prev) = e;
            if *present {
                agg.recv += c.bytes_recv.saturating_sub(prev.0);
                agg.sent += c.bytes_sent.saturating_sub(prev.1);
            }
            *present = true;
            *prev = (c.bytes_recv, c.bytes_sent);
            if c.bps_recv > agg.peak_rx {
                agg.peak_rx = c.bps_recv;
            }
            if c.bps_sent > agg.peak_tx {
                agg.peak_tx = c.bps_sent;
            }
            agg.last_ts = ts;
            agg.state = c.state.clone();
            seen.insert(c.inode);
        }
        for (inode, (_, present, _)) in conns.iter_mut() {
            if !seen.contains(inode) {
                *present = false;
            }
        }

        for i in &s.net.interfaces {
            if_first
                .entry(i.name.clone())
                .or_insert((i.session_rx, i.session_tx));
            if_last.insert(i.name.clone(), (i.session_rx, i.session_tx));
        }
        let (rxs, txs) = s
            .net
            .interfaces
            .iter()
            .fold((0.0_f64, 0.0_f64), |a, i| (a.0 + i.rx_bps, a.1 + i.tx_bps));
        if peak.is_none_or(|(_, pr, pt)| rxs + txs > pr + pt) {
            peak = Some((ts, rxs, txs));
        }

        prev_ts = Some(ts);
    }

    let (nic_rx, nic_tx) = if_last
        .iter()
        .filter_map(|(k, last)| {
            let first = if_first.get(k)?;
            Some((
                last.0.saturating_sub(first.0),
                last.1.saturating_sub(first.1),
            ))
        })
        .fold((0, 0), |a, x| (a.0 + x.0, a.1 + x.1));

    dts.sort_unstable();
    let interval_s = dts.get(dts.len() / 2).copied().unwrap_or(0);

    let mut procs: Vec<ProcAgg> = procs.into_values().collect();
    procs.sort_by_key(|p| std::cmp::Reverse(p.traffic()));
    let mut conns: Vec<ConnAgg> = conns.into_values().map(|(c, _, _)| c).collect();
    conns.sort_by_key(|c| std::cmp::Reverse(c.traffic()));

    Report {
        file,
        host,
        start,
        end,
        snapshots: snaps.len(),
        interval_s,
        skipped,
        nic_rx,
        nic_tx,
        peak_ts: peak.map(|(t, _, _)| t).unwrap_or(0),
        peak_rx_bps: peak.map(|(_, r, _)| r).unwrap_or(0.0),
        peak_tx_bps: peak.map(|(_, _, t)| t).unwrap_or(0.0),
        procs,
        conns,
    }
}

// ---------- CLI ----------

pub fn run(args: &[String]) -> Result<()> {
    let zh = crate::cli_lang_zh();
    let p = |en: &str, c: &str| crate::pick_s(zh, en, c);
    let mut file: Option<PathBuf> = None;
    let mut top = DEFAULT_TOP;
    let mut proc: Option<String> = None;
    let mut fields = Fields::ALL;
    let mut as_json = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                println!("{}", report_usage(zh));
                return Ok(());
            }
            "--top" => {
                let v = args
                    .get(i + 1)
                    .context(p("--top needs a number", "--top 需要一个数字"))?;
                top = v
                    .parse::<usize>()
                    .map_err(|_| {
                        anyhow::anyhow!(p(
                            &format!("--top {v:?} is not a number"),
                            &format!("--top {v:?} 不是数字")
                        ))
                    })?
                    .max(1);
                i += 1;
            }
            "--proc" => {
                proc = Some(
                    args.get(i + 1)
                        .context(p("--proc needs a name", "--proc 需要一个名称"))?
                        .clone(),
                );
                i += 1;
            }
            "--fields" => {
                let v = args.get(i + 1).context(p(
                    "--fields needs a group list (e.g. net,conns)",
                    "--fields 需要组列表(如 net,conns)",
                ))?;
                fields = Fields::parse(v, zh)?;
                i += 1;
            }
            "--json" => as_json = true,
            a if a.starts_with('-') && a != "-" => {
                bail!("ltop report: unknown option {a} (try --help)")
            }
            a => {
                if file.replace(PathBuf::from(a)).is_some() {
                    bail!(
                        "{}",
                        p(
                            &format!("ltop report: takes exactly one file (got {a:?})"),
                            &format!("ltop report: 只收一个文件(得到 {a:?})")
                        )
                    );
                }
            }
        }
        i += 1;
    }
    let Some(path) = file else {
        bail!(
            "{}",
            p(
                "usage: ltop report <file.jsonl> [--top N] [--proc <name>] [--fields net,udp,disk,conns] [--json]",
                "用法:ltop report <file.jsonl> [--top N] [--proc <name>] [--fields net,udp,disk,conns] [--json]"
            )
        )
    };
    let (mut snaps, skipped) = load(&path, zh)?;
    if snaps.is_empty() {
        let tail = if skipped > 0 {
            p(
                &format!("skipped {skipped} lines"),
                &format!("跳过 {skipped} 行"),
            )
        } else {
            p("empty file", "文件为空").to_string()
        };
        bail!(
            "{}",
            p(
                &format!(
                    "no schema-v1 snapshot lines in {} (legacy session-* logs belong to --replay; {tail})",
                    path.display()
                ),
                &format!(
                    "{} 里没有 schema v1 快照行(旧 monlog session-* 走 --replay;{tail})",
                    path.display()
                )
            )
        );
    }
    if let Some(f) = &proc {
        for s in &mut snaps {
            s.procs.retain(|p| name_match(f, &p.name));
            s.conns
                .retain(|c| c.process.as_deref().is_some_and(|pn| name_match(f, pn)));
        }
    }
    let r = build(snaps, skipped, path.clone());
    if as_json {
        println!("{}", json_summary(&r, top, fields));
    } else {
        print_human(&r, top, fields);
    }
    Ok(())
}

fn report_usage(zh: bool) -> String {
    if zh {
        format!(
            "ltop report v{} — schema v1 JSONL 的窗口摘要(人读表;--json 给 AI)

USAGE:
    ltop report <FILE.jsonl> [OPTIONS]

    --top <N>        按流量前 N(默认 {DEFAULT_TOP})
    --proc <NAME>    过滤:子串;含 * 走通配(如 chrome*)
    --fields <LIST>  组级枚举 net,udp,disk,conns(默认全部)
    --json           结构化输出(单行,~1-2KB)
    -h, --help       本帮助

输入:ltop watch 产物 / TUI monlog(ltop-log-*.jsonl,同 schema)。
窗口总和 = 末快照累计 − 首快照累计;峰值 = 窗口内逐行速率最大值。
旧 session-* 日志(v1,仅 PROCS)不进 report,用 --replay。
任意查询:文件本身就是标准 JSON,直接 jq。",
            env!("CARGO_PKG_VERSION")
        )
    } else {
        format!(
            "ltop report v{} — window summary of schema-v1 JSONL (human table; --json for AI)

USAGE:
    ltop report <FILE.jsonl> [OPTIONS]

    --top <N>        top N by traffic (default {DEFAULT_TOP})
    --proc <NAME>    filter: substring, or glob when it contains * (e.g. chrome*)
    --fields <LIST>  group list net,udp,disk,conns (default: all)
    --json           structured single-line output (~1-2 KB)
    -h, --help       this help

Input: ltop watch captures / TUI monitoring logs (ltop-log-*.jsonl, same schema).
Window sums = last cumulative − first; peaks = max per-row rate in window.
Legacy session-* logs (v1, procs only) go to --replay. Arbitrary queries: jq.",
            env!("CARGO_PKG_VERSION")
        )
    }
}

// ---------- 输出 ----------

/// 一行 cells:首列左对齐,其余右对齐,单空格分隔
fn row(cells: &[String], widths: &[usize]) -> String {
    cells
        .iter()
        .enumerate()
        .map(|(i, c)| {
            if i == 0 {
                format!("{c:<width$}", width = widths[i])
            } else {
                format!("{c:>width$}", width = widths[i])
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn trunc(s: &str, w: usize) -> String {
    if s.chars().count() <= w {
        s.to_string()
    } else {
        s.chars().take(w - 1).collect::<String>() + "…"
    }
}

fn print_human(r: &Report, top: usize, f: Fields) {
    let zh = crate::cli_lang_zh();
    let p = |en: &str, c: &str| crate::pick_s(zh, en, c);
    println!("== ltop report ==");
    println!("file  {}", r.file.display());
    println!(
        "{}",
        p(
            &format!(
                "host  {}  window {} → {}  {} snapshots  ~{}s interval{}",
                if r.host.is_empty() { "—" } else { &r.host },
                crate::replay::fmt_ts(r.start),
                crate::replay::fmt_ts(r.end),
                r.snapshots,
                r.interval_s,
                if r.skipped > 0 {
                    format!("  ({} lines skipped: header/legacy/torn)", r.skipped)
                } else {
                    String::new()
                }
            ),
            &format!(
                "host  {}  窗口 {} → {}  快照 {} 个  间隔 ~{}s{}",
                if r.host.is_empty() { "—" } else { &r.host },
                crate::replay::fmt_ts(r.start),
                crate::replay::fmt_ts(r.end),
                r.snapshots,
                r.interval_s,
                if r.skipped > 0 {
                    format!("  (跳过 {} 行:表头/旧格式/半行)", r.skipped)
                } else {
                    String::new()
                }
            )
        )
    );
    println!();
    println!(
        "{}",
        p(
            "Global (NIC interface scope, ≠ process payload Σ):",
            "全局(NIC 接口层口径,≠ 进程 payload Σ):"
        )
    );
    println!(
        "  {} {}  {} {}  {} ↓{}/s ↑{}/s @ {}",
        p("Σrx", "Σ收"),
        crate::fmt_human(r.nic_rx as f64),
        p("Σtx", "Σ发"),
        crate::fmt_human(r.nic_tx as f64),
        p("peak", "峰值"),
        crate::fmt_human(r.peak_rx_bps),
        crate::fmt_human(r.peak_tx_bps),
        crate::replay::fmt_ts(r.peak_ts)
    );

    let procs: Vec<&ProcAgg> = r.procs.iter().take(top).collect();
    if procs.is_empty() {
        println!();
        println!(
            "{}",
            p(
                "Processes: none (no data in window, or filtered out by --proc)",
                "进程:无(窗口内无进程数据,或被 --proc 过滤光)"
            )
        );
    } else {
        println!();
        println!(
            "{}",
            p(
                &format!(
                    "Processes (top {} by traffic, {} total):",
                    top.min(r.procs.len()),
                    r.procs.len()
                ),
                &format!(
                    "进程(按流量前 {},共 {}):",
                    top.min(r.procs.len()),
                    r.procs.len()
                )
            )
        );
        let mut hdr = vec![p("Process", "进程")];
        let mut w = vec![24usize];
        if f.net {
            hdr.extend([
                p("Σrx", "Σ收"),
                p("Σtx", "Σ发"),
                p("pk rx/s", "峰收/s"),
                p("pk tx/s", "峰发/s"),
            ]);
            w.extend([9, 9, 9, 9]);
        }
        if f.udp {
            hdr.extend([p("u rx", "u收"), p("u tx", "u发"), p("pk u/s", "u峰/s")]);
            w.extend([9, 9, 9]);
        }
        if f.disk {
            hdr.extend([p("d rd", "d读"), p("d wr", "d写"), p("pk d/s", "d峰/s")]);
            w.extend([9, 9, 9]);
        }
        hdr.extend([p("active", "活跃"), p("state", "状态")]);
        w.extend([6, if zh { 4 } else { 6 }]);
        println!("{}", row(&hdr, &w));
        for p in &procs {
            let mut cells = vec![trunc(&p.name, 24)];
            if f.net {
                cells.extend([
                    crate::fmt_human(p.net_recv as f64),
                    crate::fmt_human(p.net_sent as f64),
                    crate::fmt_human(p.peak_rx),
                    crate::fmt_human(p.peak_tx),
                ]);
            }
            if f.udp {
                cells.extend([
                    crate::fmt_human(p.udp_recv as f64),
                    crate::fmt_human(p.udp_sent as f64),
                    crate::fmt_human(p.peak_udp_rx.max(p.peak_udp_tx)),
                ]);
            }
            if f.disk {
                cells.extend([
                    crate::fmt_human(p.disk_read as f64),
                    crate::fmt_human(p.disk_written as f64),
                    crate::fmt_human(p.peak_disk_r.max(p.peak_disk_w)),
                ]);
            }
            cells.push(crate::app::fmt_dur(
                p.samples.saturating_sub(1) as u64 * r.interval_s.max(1),
            ));
            cells.push(
                if zh {
                    if p.last_alive {
                        "活"
                    } else {
                        "退"
                    }
                } else if p.last_alive {
                    "alive"
                } else {
                    "exited"
                }
                .into(),
            );
            println!("{}", row(&cells, &w));
        }
        if r.procs.len() > procs.len() {
            println!(
                "{}",
                p(
                    &format!(
                        "({} more processes not shown; adjust --top N)",
                        r.procs.len() - procs.len()
                    ),
                    &format!(
                        "(其余 {} 个进程未列出,--top N 调整)",
                        r.procs.len() - procs.len()
                    )
                )
            );
        }
    }

    if f.conns {
        if r.conns.is_empty() {
            println!();
            println!(
                "{}",
                p(
                    "Connections: none (no data in window, or filtered out by --proc)",
                    "连接:无(窗口内无连接数据,或被 --proc 过滤光)"
                )
            );
        } else {
            println!();
            println!(
                "{}",
                p(
                    &format!(
                        "Connections (top {} by traffic, {} total):",
                        top.min(r.conns.len()),
                        r.conns.len()
                    ),
                    &format!(
                        "连接(按流量前 {},共 {}):",
                        top.min(r.conns.len()),
                        r.conns.len()
                    )
                )
            );
            let mut hdr = vec![p("Peer", "对端"), p("proto", "协议")];
            let mut w = vec![30usize, if zh { 4 } else { 5 }];
            hdr.extend([
                p("Σrx", "Σ收"),
                p("Σtx", "Σ发"),
                p("pk rx/s", "峰收/s"),
                p("pk tx/s", "峰发/s"),
                p("dur", "持续"),
                p("state", "状态"),
                p("proc", "进程"),
            ]);
            w.extend([9, 9, 9, 9, 7, 6, 14]);
            println!("{}", row(&hdr, &w));
            for c in r.conns.iter().take(top) {
                let cells = vec![
                    trunc(&c.remote, 30),
                    c.proto.clone(),
                    crate::fmt_human(c.recv as f64),
                    crate::fmt_human(c.sent as f64),
                    crate::fmt_human(c.peak_rx),
                    crate::fmt_human(c.peak_tx),
                    crate::app::fmt_dur(c.last_ts.saturating_sub(c.first_ts).max(1)),
                    trunc(&c.state, 6),
                    trunc(c.process.as_deref().unwrap_or("—"), 14),
                ];
                println!("{}", row(&cells, &w));
            }
        }
    }
}

/// 结构化摘要(单行紧凑);供 run 输出与体积单测共用
fn json_summary(r: &Report, top: usize, f: Fields) -> String {
    // AI 读摘要:速率取整到 B/s,控制体积(~1-2KB);精确速率回 watch JSONL 查
    let mut totals = serde_json::Map::new();
    totals.insert("scope".into(), json!("nic"));
    totals.insert("nic_rx".into(), json!(r.nic_rx));
    totals.insert("nic_tx".into(), json!(r.nic_tx));
    totals.insert("peak_ts".into(), json!(r.peak_ts));
    totals.insert("peak_rx_bps".into(), json!(r.peak_rx_bps as u64));
    totals.insert("peak_tx_bps".into(), json!(r.peak_tx_bps as u64));

    let procs: Vec<serde_json::Value> = r
        .procs
        .iter()
        .take(top)
        .map(|p| {
            let mut m = serde_json::Map::new();
            m.insert("name".into(), json!(p.name));
            if f.net {
                m.insert("net_recv".into(), json!(p.net_recv));
                m.insert("net_sent".into(), json!(p.net_sent));
                m.insert("peak_rx_bps".into(), json!(p.peak_rx as u64));
                m.insert("peak_tx_bps".into(), json!(p.peak_tx as u64));
            }
            if f.udp {
                m.insert("udp_recv".into(), json!(p.udp_recv));
                m.insert("udp_sent".into(), json!(p.udp_sent));
                m.insert(
                    "peak_udp_bps".into(),
                    json!(p.peak_udp_rx.max(p.peak_udp_tx) as u64),
                );
            }
            if f.disk {
                m.insert("disk_read".into(), json!(p.disk_read));
                m.insert("disk_written".into(), json!(p.disk_written));
                m.insert(
                    "peak_disk_bps".into(),
                    json!(p.peak_disk_r.max(p.peak_disk_w) as u64),
                );
            }
            m.insert(
                "active_s".into(),
                json!(p.samples.saturating_sub(1) as u64 * r.interval_s.max(1)),
            );
            m.insert("alive".into(), json!(p.last_alive));
            serde_json::Value::Object(m)
        })
        .collect();

    let conns: Vec<serde_json::Value> = r
        .conns
        .iter()
        .take(top)
        .map(|c| {
            json!({
                "remote": c.remote,
                "proto": c.proto,
                "recv": c.recv,
                "sent": c.sent,
                "peak_rx_bps": c.peak_rx as u64,
                "peak_tx_bps": c.peak_tx as u64,
                "duration_s": c.last_ts.saturating_sub(c.first_ts),
                "state": c.state,
                "process": c.process,
            })
        })
        .collect();

    let mut out = json!({
        "schema": snapshot::SCHEMA,
        "file": r.file.display().to_string(),
        "host": r.host,
        "window": {
            "start": r.start,
            "end": r.end,
            "snapshots": r.snapshots,
            "interval_s": r.interval_s,
        },
        "totals": serde_json::Value::Object(totals),
        "procs": procs,
        "conns": if f.conns { json!(conns) } else { json!([]) },
    });
    if r.skipped > 0 {
        out["skipped_lines"] = json!(r.skipped);
    }
    serde_json::to_string(&out).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 3 个 schema v1 快照 + 1 表头 + 1 旧 v1 行 + 1 半行。
    /// 剧情:chrome pid10 → pid11 重启;app 出现后退出;conn#1 中途消失;
    /// conn#2 后段才出现。时间 100/110/120,间隔 10s。
    const SAMPLE: &str = concat!(
        "{\"watch_start\":99,\"host\":\"h\",\"interval_s\":10}\n",
        "{\"ts\":1,\"procs\":[{\"pid\":9,\"name\":\"old\",\"net_recv\":1}]}\n", // 旧 v1:无 schema
        "{\"schema\":1,\"meta\":{\"hostname\":\"h\",\"ts\":100,\"version\":\"t\",\"os\":\"linux\",\"arch\":\"x86_64\",\"udp_helper\":true},",
        "\"net\":{\"selected\":\"eth0\",\"interfaces\":[{\"name\":\"eth0\",\"operstate\":\"up\",\"speed_mbps\":null,\"duplex\":null,\"mac\":\"\",\"addresses\":[],\"rx_bps\":100.0,\"tx_bps\":10.0,\"session_rx\":1000,\"session_tx\":500,\"alive_rx\":9000,\"alive_tx\":4000}]},",
        "\"lan\":{\"subnets\":[],\"gateways\":[],\"neighbours\":[]},",
        "\"disk\":{\"devices\":[],\"session_read\":0,\"session_write\":0},",
        "\"conns\":[{\"proto\":\"tcp\",\"local\":\"l\",\"remote\":\"1.1.1.1:443\",\"state\":\"ESTAB\",\"process\":\"chrome\",\"pid\":10,\"user\":\"u\",\"inode\":1,\"bytes_recv\":80,\"bytes_sent\":40,\"bps_recv\":80.0,\"bps_sent\":40.0}],",
        "\"procs\":[{\"pid\":10,\"name\":\"chrome\",\"net_recv\":100,\"net_sent\":50,\"net_bps_recv\":100.0,\"net_bps_sent\":50.0,\"udp_recv\":10,\"udp_sent\":5,\"disk_read\":7,\"disk_written\":3,\"disk_bps_read\":7.0,\"disk_bps_written\":3.0,\"alive\":true},",
        "{\"pid\":20,\"name\":\"app\",\"net_recv\":200,\"net_sent\":0,\"net_bps_recv\":0.0,\"net_bps_sent\":0.0,\"udp_recv\":0,\"udp_sent\":0,\"disk_read\":0,\"disk_written\":0,\"disk_bps_read\":0.0,\"disk_bps_written\":0.0,\"alive\":true}]}\n",
        "{\"schema\":1,\"meta\":{\"hostname\":\"h\",\"ts\":110,\"version\":\"t\",\"os\":\"linux\",\"arch\":\"x86_64\",\"udp_helper\":true},",
        "\"net\":{\"selected\":\"eth0\",\"interfaces\":[{\"name\":\"eth0\",\"operstate\":\"up\",\"speed_mbps\":null,\"duplex\":null,\"mac\":\"\",\"addresses\":[],\"rx_bps\":300.0,\"tx_bps\":20.0,\"session_rx\":3000,\"session_tx\":1500,\"alive_rx\":11000,\"alive_tx\":5500}]},",
        "\"lan\":{\"subnets\":[],\"gateways\":[],\"neighbours\":[]},",
        "\"disk\":{\"devices\":[],\"session_read\":0,\"session_write\":0},",
        "\"conns\":[{\"proto\":\"tcp\",\"local\":\"l\",\"remote\":\"1.1.1.1:443\",\"state\":\"ESTAB\",\"process\":\"chrome\",\"pid\":10,\"user\":\"u\",\"inode\":1,\"bytes_recv\":480,\"bytes_sent\":90,\"bps_recv\":300.0,\"bps_sent\":90.0},",
        "{\"proto\":\"udp\",\"local\":\"l\",\"remote\":\"2.2.2.2:53\",\"state\":\"UDP\",\"process\":\"app\",\"pid\":20,\"user\":\"u\",\"inode\":2,\"bytes_recv\":10,\"bytes_sent\":0,\"bps_recv\":1.0,\"bps_sent\":0.0}],",
        "\"procs\":[{\"pid\":10,\"name\":\"chrome\",\"net_recv\":500,\"net_sent\":150,\"net_bps_recv\":300.0,\"net_bps_sent\":90.0,\"udp_recv\":30,\"udp_sent\":5,\"disk_read\":9,\"disk_written\":3,\"disk_bps_read\":9.0,\"disk_bps_written\":3.0,\"alive\":true},",
        "{\"pid\":11,\"name\":\"chrome\",\"net_recv\":50,\"net_sent\":10,\"net_bps_recv\":5.0,\"net_bps_sent\":1.0,\"udp_recv\":10,\"udp_sent\":5,\"disk_read\":9,\"disk_written\":3,\"disk_bps_read\":9.0,\"disk_bps_written\":3.0,\"alive\":true},",
        "{\"pid\":20,\"name\":\"app\",\"net_recv\":200,\"net_sent\":0,\"net_bps_recv\":0.0,\"net_bps_sent\":0.0,\"udp_recv\":0,\"udp_sent\":0,\"disk_read\":0,\"disk_written\":0,\"disk_bps_read\":0.0,\"disk_bps_written\":0.0,\"alive\":false}]}\n",
        "{\"schema\":1,\"meta\":{\"hostname\":\"h\",\"ts\":120,\"version\":\"t\",\"os\":\"linux\",\"arch\":\"x86_64\",\"udp_helper\":true},",
        "\"net\":{\"selected\":\"eth0\",\"interfaces\":[{\"name\":\"eth0\",\"operstate\":\"up\",\"speed_mbps\":null,\"duplex\":null,\"mac\":\"\",\"addresses\":[],\"rx_bps\":200.0,\"tx_bps\":50.0,\"session_rx\":5000,\"session_tx\":2500,\"alive_rx\":13000,\"alive_tx\":6500}]},",
        "\"lan\":{\"subnets\":[],\"gateways\":[],\"neighbours\":[]},",
        "\"disk\":{\"devices\":[],\"session_read\":0,\"session_write\":0},",
        "\"conns\":[{\"proto\":\"udp\",\"local\":\"l\",\"remote\":\"2.2.2.2:53\",\"state\":\"UDP\",\"process\":\"app\",\"pid\":20,\"user\":\"u\",\"inode\":2,\"bytes_recv\":30,\"bytes_sent\":5,\"bps_recv\":2.0,\"bps_sent\":1.0}],",
        "\"procs\":[{\"pid\":11,\"name\":\"chrome\",\"net_recv\":250,\"net_sent\":60,\"net_bps_recv\":500.0,\"net_bps_sent\":200.0,\"udp_recv\":30,\"udp_sent\":5,\"disk_read\":9,\"disk_written\":3,\"disk_bps_read\":9.0,\"disk_bps_written\":3.0,\"alive\":true}]}\n",
        "{\"schema\":1,\"meta\":{\"hostname\":\"h\",\"ts\":130,\"version\":\"t\",\"os\":\"linux\",\"arch\":\"x86_64\",\"udp_helper\":true},", // 之后截断的半行
    );

    /// 每次调用独占一个临时目录:测试并发跑 sample() 不能共享文件
    fn load_str(s: &str) -> (Vec<Snapshot>, usize) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let n = SEQ.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("ltop-report-test-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("ltop-session-1.jsonl");
        std::fs::write(&p, s).unwrap();
        let r = load(&p, false).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        r
    }

    fn sample() -> Report {
        // 半行:截断的最后一个快照(解析失败 → skipped)
        let (snaps, skipped) = load_str(SAMPLE);
        assert_eq!(snaps.len(), 3);
        assert_eq!(skipped, 3, "表头 + 旧 v1 + 半行都应跳过");
        build(snaps, skipped, PathBuf::from("t.jsonl"))
    }

    #[test]
    fn skips_header_old_format_and_torn_lines() {
        sample(); // 断言在 sample() 内
    }

    #[test]
    fn proc_window_sum_spans_pid_restart() {
        let r = sample();
        let chrome = r.procs.iter().find(|p| p.name == "chrome").unwrap();
        // pid10: recv 100→500 (400), pid11: 50→250 (200)
        assert_eq!(chrome.net_recv, 600);
        // pid10: sent 50→150 (100), pid11: 10→60 (50)
        assert_eq!(chrome.net_sent, 150);
        // t100 pid10 + t110 pid10/pid11 + t120 pid11:每快照每名字至多
        // 计 1(旧口径按行计 4,同名双进程把活跃时长虚增一倍)
        assert_eq!(chrome.samples, 3);
        assert!(chrome.last_alive);
        // 峰值取逐行速率最大(500 来自 t=120 行)
        assert_eq!(chrome.peak_rx, 500.0);
    }

    #[test]
    fn proc_udp_peak_derived_per_interval() {
        let r = sample();
        let chrome = r.procs.iter().find(|p| p.name == "chrome").unwrap();
        // udp_recv:pid10 10→30 (Δ20) + pid11 10→30 (Δ20),10s 内 → 峰 2 B/s
        assert!((chrome.peak_udp_rx - 2.0).abs() < 1e-9);
        assert_eq!(chrome.udp_recv, 40);
    }

    #[test]
    fn exited_proc_keeps_last_state_and_no_fiction() {
        let r = sample();
        let app = r.procs.iter().find(|p| p.name == "app").unwrap();
        // t=110 行 alive=false 是最后出现 → 退;累计无新增
        assert!(!app.last_alive);
        assert_eq!(app.net_recv, 0);
        assert_eq!(app.samples, 2);
    }

    #[test]
    fn conn_keeps_last_known_totals_after_vanish() {
        let r = sample();
        let c1 = r.conns.iter().find(|c| c.inode == 1).unwrap();
        // t=110 后消失:窗口 Σ 只算到场段(480-80, 90-40),消失后不计
        assert_eq!(c1.recv, 400);
        assert_eq!(c1.sent, 50);
        assert_eq!(c1.peak_rx, 300.0);
        assert_eq!(c1.last_ts - c1.first_ts, 10);
        assert_eq!(c1.state, "ESTAB");
        let c2 = r.conns.iter().find(|c| c.inode == 2).unwrap();
        // t=110 首现即基线,t=120 起计
        assert_eq!(c2.recv, 20);
    }

    #[test]
    fn nic_window_sum_and_global_peak() {
        let r = sample();
        assert_eq!(r.nic_rx, 4000);
        assert_eq!(r.nic_tx, 2000);
        // 峰值区间:t=110(rx_bps 300 为最大)
        assert_eq!(r.peak_ts, 110);
        assert_eq!(r.peak_rx_bps, 300.0);
        assert_eq!(r.snapshots, 3);
        assert_eq!(r.interval_s, 10);
        assert_eq!(r.start, 100);
        assert_eq!(r.end, 120);
        assert_eq!(r.skipped, 3);
    }

    #[test]
    fn json_summary_stays_small() {
        let r = sample();
        let s = json_summary(&r, DEFAULT_TOP, Fields::ALL);
        assert!(s.len() < 2048, "默认摘要应 <2KB,实测 {}B", s.len());
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["window"]["snapshots"], 3);
        assert_eq!(v["procs"].as_array().unwrap().len(), 2);
        assert_eq!(v["conns"].as_array().unwrap().len(), 2);
        // skipped>0 才带 skipped_lines
        assert_eq!(v["skipped_lines"], 3);
        let (snaps, _) = load_str("{\"watch_start\":1,\"host\":\"h\"}\n");
        let r0 = build(snaps, 0, PathBuf::from("t"));
        // 空数据要能安全出摘要(build 前提是非空,这里防御式验证字段)
        assert_eq!(r0.snapshots, 0);
    }

    #[test]
    fn fields_groups_gate_output() {
        let r = sample();
        let f = Fields::parse("conns", false).unwrap();
        let s = json_summary(&r, DEFAULT_TOP, f);
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert!(v["procs"][0].get("net_recv").is_none(), "net 组被排除");
        assert_eq!(v["conns"].as_array().unwrap().len(), 2);
        assert!(Fields::parse("net,bogus", false).is_err());
    }

    #[test]
    fn name_match_substring_and_glob() {
        assert!(name_match("chrom", "chrome"));
        assert!(name_match("CHROME", "chrome-helper"));
        assert!(!name_match("chrom", "firefox"));
        assert!(name_match("chrome*", "chrome"));
        assert!(name_match("chrome*", "chrome-helper"));
        assert!(!name_match("chrome*", "xchrome"));
        assert!(name_match("*render*", "chrome:renderer"));
        assert!(name_match("a*b*c", "a-x-b-y-c"));
        assert!(!name_match("a*b*c", "a-b-c-d"));
    }

    #[test]
    fn load_rejects_missing_file() {
        let err = load(std::path::Path::new("/nonexistent/x.jsonl"), false);
        assert!(err.is_err());
    }
}

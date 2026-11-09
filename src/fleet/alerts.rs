//! fleet 告警引擎(纯函数,零 IO):规则解析、迟滞评估、通知决策。
//!
//! 设计要点:
//! - **规则按节点实例化**:同一条 `alert node rx > 50M` 对每个节点有
//!   独立状态,状态键 = `{规则原文}@{target}`;
//! - **迟滞用轮次不用秒**:连续 `hysteresis` 轮成立才触发(默认 2),
//!   回落也连续 `hysteresis` 轮才解除——防单轮抖动,且与 poll 模型同构
//!   可测;node-down 例外:连续失败本身就是 miss 计数,阈值独立(默认 3);
//! - **触发/重提/解除三事件**:触发立即通知,持续期间每 `cooldown_s`
//!   重提一次(默认 10min),回落发解除通知;
//! - **每小时上限**:全局滚动窗口,默认 20 条——告警风暴时宁可漏报
//!   也不轰炸系统通知中心。

use std::collections::HashMap;

/// 一条告警规则(fleet.conf 的 `alert ...` 行)
#[derive(Clone, Debug, PartialEq)]
pub enum AlertRule {
    /// 节点总收包速率红线(B/s)
    NodeRx(f64),
    /// 节点总发包速率红线(B/s)
    NodeTx(f64),
    /// 按名字子串匹配(大小写不敏感)的进程网络速率红线
    ProcNet(String, f64),
    /// 出现到指定 IP 的连接(remote 含该地址)
    ConnIp(String),
    /// LAN 出现基线之外的新邻居
    NewDevice,
    /// 节点连续多轮拉取失败
    NodeDown,
}

impl AlertRule {
    /// 规则原文(fleet.conf 行等价形式,作状态键与展示)
    pub fn source(&self) -> String {
        match self {
            AlertRule::NodeRx(v) => format!("alert node rx > {}", rate_str(*v)),
            AlertRule::NodeTx(v) => format!("alert node tx > {}", rate_str(*v)),
            AlertRule::ProcNet(n, v) => format!("alert proc {n} net > {}", rate_str(*v)),
            AlertRule::ConnIp(ip) => format!("alert conn ip {ip}"),
            AlertRule::NewDevice => "alert new-device".to_string(),
            AlertRule::NodeDown => "alert node-down".to_string(),
        }
    }
}

/// 带宽字面量 → B/s:支持 50 / 50K / 50M / 50G / 50T / 50KiB…
/// (K/M/G/T 十进制与 Ki/Mi 二进制都收,大小写不敏感,B 后缀可选)
pub fn parse_rate(s: &str) -> Option<f64> {
    let t = s.trim().to_ascii_uppercase();
    let t = t.strip_suffix('B').unwrap_or(&t);
    let t = t.trim_end();
    let (num, mult) = if let Some(p) = t.find("KI") {
        (&t[..p], 1024.0)
    } else if let Some(p) = t.find("MI") {
        (&t[..p], 1024.0 * 1024.0)
    } else if let Some(p) = t.find("GI") {
        (&t[..p], 1024.0 * 1024.0 * 1024.0)
    } else {
        let c = t.chars().last()?;
        let m = match c {
            '0'..='9' | '.' => 1.0,
            'K' => 1_000.0,
            'M' => 1_000_000.0,
            'G' => 1_000_000_000.0,
            'T' => 1_000_000_000_000.0,
            _ => return None,
        };
        (&t[..t.len() - if m == 1.0 { 0 } else { 1 }], m)
    };
    num.trim().parse::<f64>().ok().map(|v| v * mult)
}

/// 阈值的规范展示(自动挑单位,整数商不带小数;规则原文回写用)
pub fn rate_str(v: f64) -> String {
    const U: [(&str, f64); 4] = [("T", 1e12), ("G", 1e9), ("M", 1e6), ("K", 1e3)];
    for (s, m) in U {
        if v >= m {
            let x = v / m;
            if (x - x.round()).abs() < 1e-9 {
                return format!("{}{}", x.round() as i64, s);
            }
            return format!("{x}{s}");
        }
    }
    if (v - v.round()).abs() < 1e-9 {
        return format!("{}", v.round() as i64);
    }
    format!("{v}")
}

/// fleet.conf 的 `alert ...` 行 → 规则;语法不认识返回 None(cfg 侧
/// 会把原行保留在 extra 并启动警告)
pub fn parse_alert_line(line: &str) -> Option<AlertRule> {
    let t = line.trim();
    let rest = t.strip_prefix("alert")?.trim();
    let mut it = rest.split_whitespace();
    let head = it.next()?;
    match head {
        "node" => {
            let metric = it.next()?;
            let dir = it.next()?;
            if dir != ">" {
                return None;
            }
            let v = parse_rate(it.next()?)?;
            match metric {
                "rx" => Some(AlertRule::NodeRx(v)),
                "tx" => Some(AlertRule::NodeTx(v)),
                _ => None,
            }
        }
        "proc" => {
            let name = it.next()?;
            let metric = it.next()?;
            let dir = it.next()?;
            if dir != ">" || metric != "net" {
                return None;
            }
            let v = parse_rate(it.next()?)?;
            Some(AlertRule::ProcNet(name.to_string(), v))
        }
        "conn" => {
            if it.next()? != "ip" {
                return None;
            }
            let ip = it.next()?;
            Some(AlertRule::ConnIp(ip.to_string()))
        }
        "new-device" => Some(AlertRule::NewDevice),
        "node-down" => Some(AlertRule::NodeDown),
        _ => None,
    }
}

/// 一次评估喂进来的每节点数据
pub struct NodeEval<'a> {
    pub target: &'a str,
    /// None = 该节点尚无成功样本(不计入速率类规则)
    pub sample: Option<&'a crate::fleet::model::NodeSample>,
    /// 最近成功快照(proc/conn/new-device 规则用)
    pub snap: Option<&'a crate::snapshot::Snapshot>,
    /// 连续失败轮数(node-down 规则用;成功轮归零)
    pub miss: u32,
}

/// 引擎事件(评估产出;发送由调用方做)
#[derive(Clone, Debug, PartialEq)]
pub enum AlertEvent {
    /// 首次触发(立即通知)
    Fire(String, String),
    /// 持续期间冷却到期重提
    ReFire(String, String),
    /// 回落解除
    Clear(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Verdict {
    /// 本轮规则条件成立?
    Hold(bool),
    /// 无数据(节点还没成功样本/快照),不计 hit 也不计 miss
    NoData,
}

#[derive(Clone)]
struct RuleState {
    /// 连续成立/不成立轮数(迟滞计数)
    hits: u32,
    misses: u32,
    /// Some(ts) = 已激活(迟滞跨过)
    active_since: Option<u64>,
    last_sent: Option<u64>,
}

/// 告警引擎:规则集 + 每状态键的状态
pub struct AlertEngine {
    pub rules: Vec<AlertRule>,
    states: HashMap<String, RuleState>,
    /// LAN 基线:target → 已见设备标识集(首轮全部入基线,不告警)
    known: HashMap<String, std::collections::HashSet<String>>,
    pub hysteresis: u32,
    pub cooldown_s: u64,
    pub node_down_rounds: u32,
    pub max_per_hour: u32,
    sent_win: std::collections::VecDeque<u64>,
    zh: bool,
}

impl AlertEngine {
    pub fn new(rules: Vec<AlertRule>, zh: bool) -> Self {
        AlertEngine {
            rules,
            states: HashMap::new(),
            known: HashMap::new(),
            hysteresis: 2,
            cooldown_s: 600,
            node_down_rounds: 3,
            max_per_hour: 20,
            sent_win: std::collections::VecDeque::new(),
            zh,
        }
    }

    /// 规则对单节点的本轮判定(纯函数)
    fn verdict(rule: &AlertRule, n: &NodeEval) -> Verdict {
        match rule {
            AlertRule::NodeRx(v) => match n.sample {
                Some(s) => Verdict::Hold(s.rx > *v),
                None => Verdict::NoData,
            },
            AlertRule::NodeTx(v) => match n.sample {
                Some(s) => Verdict::Hold(s.tx > *v),
                None => Verdict::NoData,
            },
            AlertRule::ProcNet(name, v) => match n.snap {
                Some(s) => {
                    let hit = s
                        .procs
                        .iter()
                        .filter(|p| p.name.to_lowercase().contains(&name.to_lowercase()))
                        .map(|p| p.net_bps_recv + p.net_bps_sent)
                        .sum::<f64>()
                        > *v;
                    Verdict::Hold(hit)
                }
                None => Verdict::NoData,
            },
            AlertRule::ConnIp(ip) => match n.snap {
                Some(s) => Verdict::Hold(s.conns.iter().any(|c| c.remote.contains(ip))),
                None => Verdict::NoData,
            },
            AlertRule::NewDevice => {
                // 由引擎在 evaluate 里走基线逻辑,这里占位不成立
                let _ = n;
                Verdict::Hold(false)
            }
            AlertRule::NodeDown => Verdict::Hold(n.miss >= 3),
        }
    }

    /// 邻居身份标识:mac 优先,缺(macOS 脱敏)回落 ip+iface
    fn dev_id(nb: &crate::snapshot::NeighborSnapshot) -> String {
        match &nb.mac {
            Some(m) if !m.is_empty() => format!("m:{m}"),
            _ => format!("i:{}@{}", nb.ip, nb.iface),
        }
    }

    /// 一轮评估:返回要发的事件;自身推进状态。`now` 秒。
    pub fn evaluate(&mut self, nodes: &[NodeEval], now: u64) -> Vec<AlertEvent> {
        // 滚动窗口裁剪
        while let Some(&t) = self.sent_win.front() {
            if now.saturating_sub(t) >= 3600 {
                self.sent_win.pop_front();
            } else {
                break;
            }
        }
        let mut events = Vec::new();
        for rule in &self.rules {
            for n in nodes {
                let key = format!("{}@{}", rule.source(), n.target);
                // new-device 特例:基线增量判定
                let v = if matches!(rule, AlertRule::NewDevice) {
                    match n.snap {
                        Some(s) => {
                            let entry = self.known.entry(n.target.to_string()).or_default();
                            let mut fresh = 0;
                            for nb in &s.lan.neighbours {
                                let id = Self::dev_id(nb);
                                if !entry.contains(&id) {
                                    entry.insert(id);
                                    fresh += 1;
                                }
                            }
                            if entry.len() as u32 == fresh as u32 && fresh > 0 {
                                // 首轮全量入基线,不告警
                                Verdict::Hold(false)
                            } else {
                                Verdict::Hold(fresh > 0)
                            }
                        }
                        None => Verdict::NoData,
                    }
                } else if matches!(rule, AlertRule::NodeDown) {
                    Verdict::Hold(n.miss >= self.node_down_rounds)
                } else {
                    Self::verdict(rule, n)
                };
                let st = self.states.entry(key.clone()).or_insert(RuleState {
                    hits: 0,
                    misses: 0,
                    active_since: None,
                    last_sent: None,
                });
                match v {
                    Verdict::NoData => {}
                    Verdict::Hold(true) => {
                        st.hits += 1;
                        st.misses = 0;
                    }
                    Verdict::Hold(false) => {
                        st.misses += 1;
                        st.hits = 0;
                    }
                }
                // 迟滞触发(new-device 是瞬时事件:看见即触发,下轮
                // 不再出现即解除——用迟滞会把一次性事件吃掉)
                let hyst = if matches!(rule, AlertRule::NewDevice) {
                    1
                } else {
                    self.hysteresis.max(1)
                };
                if st.active_since.is_none() && st.hits >= hyst {
                    st.active_since = Some(now);
                    st.last_sent = None;
                }
                // 迟滞解除
                if st.active_since.is_some() && st.misses >= hyst {
                    st.active_since = None;
                    events.push(AlertEvent::Clear(key.clone()));
                    continue;
                }
                // 发送决策:激活中 + (未发过 | 冷却到期) + 小时上限。
                // 先取出判定所需字段结束 st 借用,再调 self.describe
                let (since, last_sent, can_send) = {
                    let Some(since) = st.active_since else {
                        continue;
                    };
                    let due = match st.last_sent {
                        None => true,
                        Some(t) => now.saturating_sub(t) >= self.cooldown_s,
                    };
                    let cap = (self.sent_win.len() as u32) < self.max_per_hour;
                    (since, st.last_sent, due && cap)
                };
                if can_send {
                    let msg = self.describe(rule, n, since);
                    let ev = if last_sent.is_none() {
                        AlertEvent::Fire(key.clone(), msg)
                    } else {
                        AlertEvent::ReFire(key.clone(), msg)
                    };
                    let st = self.states.get_mut(&key).expect("state exists");
                    st.last_sent = Some(now);
                    self.sent_win.push_back(now);
                    events.push(ev);
                }
            }
        }
        events
    }

    /// 事件话术(双语,已 sanitize——数据源是远端字符串)
    fn describe(&self, rule: &AlertRule, n: &NodeEval, _since: u64) -> String {
        let node = crate::sanitize_text(n.target);
        let zh = self.zh;
        let (head, tail) = match rule {
            AlertRule::NodeRx(v) => (
                crate::pick_s(zh, "rx over threshold", "收包超阈值"),
                crate::pick_s(
                    zh,
                    &format!(
                        "rx {} > {}",
                        crate::fmt_human(n.sample.map(|s| s.rx).unwrap_or(0.0)),
                        rate_str(*v)
                    ),
                    &format!(
                        "rx {} > {}",
                        crate::fmt_human(n.sample.map(|s| s.rx).unwrap_or(0.0)),
                        rate_str(*v)
                    ),
                ),
            ),
            AlertRule::NodeTx(v) => (
                crate::pick_s(zh, "tx over threshold", "发包超阈值"),
                crate::pick_s(
                    zh,
                    &format!(
                        "tx {} > {}",
                        crate::fmt_human(n.sample.map(|s| s.tx).unwrap_or(0.0)),
                        rate_str(*v)
                    ),
                    &format!(
                        "tx {} > {}",
                        crate::fmt_human(n.sample.map(|s| s.tx).unwrap_or(0.0)),
                        rate_str(*v)
                    ),
                ),
            ),
            AlertRule::ProcNet(name, v) => (
                crate::pick_s(zh, "process network burst", "进程流量超阈值"),
                format!("{name} > {}", rate_str(*v)),
            ),
            AlertRule::ConnIp(ip) => (
                crate::pick_s(zh, "connection to watched IP", "出现到关注 IP 的连接"),
                ip.clone(),
            ),
            AlertRule::NewDevice => (
                crate::pick_s(zh, "new LAN device", "LAN 出现新设备"),
                String::new(),
            ),
            AlertRule::NodeDown => (
                crate::pick_s(zh, "node unreachable", "节点持续失联"),
                String::new(),
            ),
        };
        let prefix = crate::pick_s(zh, "fleet", "fleet");
        if tail.is_empty() {
            format!("{prefix}: {head} — {node}")
        } else {
            format!("{prefix}: {head} — {node} ({tail})")
        }
    }

    /// 当前活跃告警(UI/CLI 展示用):键与话术
    pub fn active(&self) -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = self
            .states
            .iter()
            .filter(|(_, s)| s.active_since.is_some())
            .map(|(k, _)| {
                let rule_txt = k.split('@').next().unwrap_or("").to_string();
                (k.clone(), rule_txt)
            })
            .collect();
        v.sort();
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::model::{self, NodeSample};
    use crate::snapshot::Snapshot;

    fn sample(rx: f64, tx: f64) -> NodeSample {
        let mut s = model::fixture::snap("host", rx, tx);
        if let Some(p) = s.procs.first_mut() {
            p.net_bps_recv = rx;
            p.net_bps_sent = 0.0;
        }
        model::aggregate("t", &s, 1)
    }

    #[test]
    fn parses_rate_literals() {
        assert_eq!(parse_rate("50"), Some(50.0));
        assert_eq!(parse_rate("50M"), Some(50_000_000.0));
        assert_eq!(parse_rate("2MiB"), Some(2.0 * 1024.0 * 1024.0));
        assert_eq!(parse_rate("1g"), Some(1e9));
        assert_eq!(parse_rate("10 KB"), Some(10_000.0));
        assert_eq!(parse_rate("abc"), None);
        assert_eq!(parse_rate(""), None);
        assert_eq!(rate_str(50_000_000.0), "50M");
        assert_eq!(rate_str(1_000_000.0), "1M");
        assert_eq!(rate_str(1500.0), "1.5K");
        // 1MiB 的规范展示是十进制忠实值(parse→source 往返不丢真值)
        assert_eq!(rate_str(1024.0 * 1024.0), "1.048576M");
    }

    #[test]
    fn parses_all_rule_kinds_and_rejects_garbage() {
        assert_eq!(
            parse_alert_line("alert node rx > 50M"),
            Some(AlertRule::NodeRx(5e7))
        );
        assert_eq!(
            parse_alert_line("alert node tx > 1G"),
            Some(AlertRule::NodeTx(1e9))
        );
        assert_eq!(
            parse_alert_line("alert proc chrome net > 20M"),
            Some(AlertRule::ProcNet("chrome".into(), 2e7))
        );
        assert_eq!(
            parse_alert_line("alert conn ip 203.0.113.7"),
            Some(AlertRule::ConnIp("203.0.113.7".into()))
        );
        assert_eq!(
            parse_alert_line("alert new-device"),
            Some(AlertRule::NewDevice)
        );
        assert_eq!(
            parse_alert_line("alert node-down"),
            Some(AlertRule::NodeDown)
        );
        assert_eq!(parse_alert_line("alert node rx >= 5"), None, "只收 >");
        assert_eq!(parse_alert_line("alert node zz > 5"), None);
        assert_eq!(parse_alert_line("alert wat"), None);
        assert_eq!(parse_alert_line("node rx > 5"), None, "必须有 alert 前缀");
        // source() 回写规范形
        assert_eq!(
            parse_alert_line("alert node rx > 50.0M").unwrap().source(),
            "alert node rx > 50M"
        );
    }

    fn node_eval<'a>(
        target: &'a str,
        s: Option<&'a NodeSample>,
        snap: Option<&'a Snapshot>,
        miss: u32,
    ) -> NodeEval<'a> {
        NodeEval {
            target,
            sample: s,
            snap,
            miss,
        }
    }

    #[test]
    fn hysteresis_requires_consecutive_rounds() {
        let mut eng = AlertEngine::new(vec![AlertRule::NodeRx(100.0)], false);
        let s = sample(200.0, 0.0);
        let nodes = [node_eval("a", Some(&s), None, 0)];
        // 1 轮成立:不触发(迟滞 2)
        assert!(eng.evaluate(&nodes, 0).is_empty());
        // 2 轮成立:触发
        let ev = eng.evaluate(&nodes, 5);
        assert!(matches!(ev.first(), Some(AlertEvent::Fire(_, _))));
        // 持续:不重发(冷却未到)
        assert!(eng.evaluate(&nodes, 10).is_empty());
        // 回落 1 轮:未解除
        let s2 = sample(10.0, 0.0);
        let nodes2 = [node_eval("a", Some(&s2), None, 0)];
        assert!(eng.evaluate(&nodes2, 15).is_empty());
        // 回落 2 轮:解除
        let ev = eng.evaluate(&nodes2, 20);
        assert!(matches!(ev.first(), Some(AlertEvent::Clear(_))));
    }

    #[test]
    fn cooldown_refires_then_hour_cap() {
        let mut eng = AlertEngine::new(vec![AlertRule::NodeTx(100.0)], false);
        eng.cooldown_s = 60;
        let s = sample(0.0, 500.0);
        let nodes = [node_eval("a", Some(&s), None, 0)];
        let _ = eng.evaluate(&nodes, 0); // hit 1
        let _ = eng.evaluate(&nodes, 5); // fire
                                         // 冷却未到:静默
        assert!(eng.evaluate(&nodes, 30).is_empty());
        // 冷却到:重提
        assert!(matches!(
            eng.evaluate(&nodes, 70).first(),
            Some(AlertEvent::ReFire(_, _))
        ));
        // 上限:一小时内塞满窗口(cooldown 60,每 100s 重提一次,
        // 全部落在同一小时窗口),之后不再发送
        for i in 2..30u64 {
            let _ = eng.evaluate(&nodes, 100 * i);
        }
        assert!(
            (eng.sent_win.len() as u32) <= eng.max_per_hour,
            "窗口按上限截断"
        );
        let no = eng.evaluate(&nodes, 100 * 31);
        assert!(
            no.iter()
                .all(|e| !matches!(e, AlertEvent::Fire(_, _) | AlertEvent::ReFire(_, _))),
            "小时内达上限后不再发送: {no:?}"
        );
    }

    #[test]
    fn node_down_needs_three_misses() {
        let mut eng = AlertEngine::new(vec![AlertRule::NodeDown], false);
        let bad = model::failed("a", "timeout".into());
        let n = [node_eval("a", Some(&bad), None, 1)];
        assert!(eng.evaluate(&n, 0).is_empty());
        let n = [node_eval("a", Some(&bad), None, 3)];
        assert!(eng.evaluate(&n, 2).is_empty(), "迟滞第 1 轮");
        assert!(matches!(
            eng.evaluate(&n, 5).first(),
            Some(AlertEvent::Fire(_, _))
        ));
    }

    #[test]
    fn proc_net_sums_matching_names_case_insensitive() {
        let mut eng = AlertEngine::new(vec![AlertRule::ProcNet("Chrome".into(), 100.0)], false);
        let mut snap = model::fixture::snap("h", 0.0, 0.0);
        let mk = |pid: u32, name: &str, bps: f64| crate::snapshot::ProcSnapshot {
            pid,
            name: name.to_string(),
            net_recv: 0,
            net_sent: 0,
            net_bps_recv: bps,
            net_bps_sent: 0.0,
            udp_recv: 0,
            udp_sent: 0,
            disk_read: 0,
            disk_written: 0,
            disk_bps_read: 0.0,
            disk_bps_written: 0.0,
            alive: true,
        };
        snap.procs.clear();
        snap.procs.push(mk(1, "chrome", 80.0));
        snap.procs.push(mk(2, "Chrome Helper", 80.0));
        snap.procs.push(mk(3, "other", 500.0));
        let s = model::aggregate("a", &snap, 1);
        let nodes = [node_eval("a", Some(&s), Some(&snap), 0)];
        assert!(eng.evaluate(&nodes, 0).is_empty(), "160 合计,迟滞中");
        assert!(
            matches!(
                eng.evaluate(&nodes, 5).first(),
                Some(AlertEvent::Fire(_, _))
            ),
            "两轮 160>100 触发(只合计名字匹配的)"
        );
    }

    #[test]
    fn new_device_first_round_is_baseline() {
        let mut eng = AlertEngine::new(vec![AlertRule::NewDevice], false);
        let mut snap = model::fixture::snap("h", 0.0, 0.0);
        snap.lan.neighbours.push(crate::snapshot::NeighborSnapshot {
            ip: "192.168.1.9".into(),
            mac: Some("aa:bb".into()),
            hostname: None,
            iface: "en0".into(),
            is_gateway: false,
            is_self: false,
            tracked: false,
        });
        {
            let nodes = [node_eval("a", None, Some(&snap), 0)];
            assert!(eng.evaluate(&nodes, 0).is_empty(), "首轮全量入基线");
        }
        // 新增一台:触发
        snap.lan.neighbours.push(crate::snapshot::NeighborSnapshot {
            ip: "192.168.1.10".into(),
            mac: None,
            hostname: None,
            iface: "en0".into(),
            is_gateway: false,
            is_self: false,
            tracked: false,
        });
        {
            let nodes = [node_eval("a", None, Some(&snap), 0)];
            assert!(
                matches!(
                    eng.evaluate(&nodes, 5).first(),
                    Some(AlertEvent::Fire(_, _))
                ),
                "新设备瞬时触发"
            );
            // 同样的设备:解除(瞬时事件的天然收尾)
            assert!(matches!(
                eng.evaluate(&nodes, 10).first(),
                Some(AlertEvent::Clear(_))
            ));
        }
    }

    #[test]
    fn per_node_states_are_independent() {
        let mut eng = AlertEngine::new(vec![AlertRule::NodeRx(100.0)], false);
        let sa = sample(200.0, 0.0);
        let sb = sample(10.0, 0.0);
        let nodes = [
            node_eval("a", Some(&sa), None, 0),
            node_eval("b", Some(&sb), None, 0),
        ];
        let _ = eng.evaluate(&nodes, 0);
        let ev = eng.evaluate(&nodes, 5);
        assert_eq!(ev.len(), 1, "只有 a 触发");
        let AlertEvent::Fire(k, _) = &ev[0] else {
            panic!("应为 Fire: {ev:?}")
        };
        assert!(k.ends_with("@a"));
    }

    #[test]
    fn active_lists_only_fired() {
        let mut eng = AlertEngine::new(vec![AlertRule::NodeRx(100.0)], false);
        let s = sample(200.0, 0.0);
        let nodes = [node_eval("a", Some(&s), None, 0)];
        let _ = eng.evaluate(&nodes, 0);
        assert!(eng.active().is_empty(), "迟滞未过不列活跃");
        let _ = eng.evaluate(&nodes, 5);
        assert_eq!(eng.active().len(), 1);
    }
}

//! fleet 配置:`~/.config/ltop/fleet.conf`(独立于主 config)。
//!
//! 刻意分家的理由:主 config 连着 TUI 设置面板的 rows 三处同步约束,
//! 而节点表是"机器相关"而非"应用偏好";分家后 hand-edit 与 `--save`
//! 互不干扰。纪律与主 config 一致:行式 `key = value`、未知行原样
//! 保留、原子写(tmp+rename);节点表是 mildly sensitive(暴露内网
//! 拓扑),新文件显式 0600。
//!
//! 语法:
//! ```text
//! interval = 5          # 轮询间隔(秒)
//! timeout  = 15         # 单节点拉取时限(秒)
//! master   = on         # ControlMaster 连接复用(on/off)
//! node = uMaj@air       # 节点,可重复
//! node = fedora
//! cmd fedora = /usr/local/bin/ltop --json   # 按节点名给远端命令
//! ```
//! `cmd <target>` 与 node 行顺序无关(mac 无交互 PATH 需要 brew 全路径)。

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use crate::config::Config;

use super::alerts::{parse_alert_line, AlertRule};

pub const DEFAULT_CMD: &str = "ltop --json";
/// 节点数硬顶:配置手滑也不许变成 fork bomb
pub const MAX_NODES: usize = 64;

#[derive(Clone, Debug, PartialEq)]
pub struct NodeCfg {
    pub target: String,
    /// None = 用 DEFAULT_CMD
    pub cmd: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FleetConf {
    pub interval: u64,
    pub timeout: u64,
    pub master: bool,
    pub nodes: Vec<NodeCfg>,
    /// 告警规则(`alert ...` 行)
    pub alerts: Vec<AlertRule>,
    /// 语法不认识的 alert 行(启动时警告一次,原行进 extra)
    pub bad_alerts: Vec<String>,
    /// 未认识的行(含注释),保存时原样回写
    pub extra: Vec<String>,
}

impl Default for FleetConf {
    fn default() -> Self {
        FleetConf {
            interval: 5,
            timeout: 15,
            master: true,
            nodes: Vec::new(),
            alerts: Vec::new(),
            bad_alerts: Vec::new(),
            extra: Vec::new(),
        }
    }
}

impl FleetConf {
    pub fn path() -> Option<PathBuf> {
        Config::config_dir().map(|d| d.join("fleet.conf"))
    }

    /// 读盘;文件不存在/不可读 → 全默认
    pub fn load() -> FleetConf {
        match Self::path().and_then(|p| fs::read_to_string(p).ok()) {
            Some(text) => Self::parse(&text),
            None => FleetConf::default(),
        }
    }

    pub fn parse(text: &str) -> FleetConf {
        let mut conf = FleetConf::default();
        let mut cmd_for: HashMap<String, String> = HashMap::new();
        for line in text.lines() {
            conf.apply_line(line, &mut cmd_for);
        }
        // cmd 行与 node 行顺序无关:收尾统一挂接
        for n in &mut conf.nodes {
            if let Some(c) = cmd_for.get(&n.target) {
                n.cmd = Some(c.clone());
            }
        }
        conf
    }

    /// 单行消化;`cmd_for` 暂存 cmd 行,解析结束再按 target 挂接
    fn apply_line(&mut self, line: &str, cmd_for: &mut HashMap<String, String>) {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') || t.starts_with(';') {
            self.extra.push(line.to_string());
            return;
        }
        // alert 行是谓词不是 key=value,必须在 split_once 之前拦下
        // (否则整行落进 extra,告警静默失效——冒烟实测抓到)
        if t.starts_with("alert ") || t == "alert" {
            match parse_alert_line(t) {
                Some(r) => self.alerts.push(r),
                None => {
                    self.bad_alerts.push(t.to_string());
                    self.extra.push(line.to_string());
                }
            }
            return;
        }
        let Some((key, value)) = t.split_once('=') else {
            self.extra.push(line.to_string());
            return;
        };
        let key = key.trim();
        let value = strip_inline_comment(value).trim().to_string();
        match key {
            "interval" => {
                if let Ok(n) = value.parse::<u64>() {
                    self.interval = n.max(1);
                } else {
                    self.extra.push(line.to_string());
                }
            }
            "timeout" => {
                if let Ok(n) = value.parse::<u64>() {
                    self.timeout = n.max(1);
                } else {
                    self.extra.push(line.to_string());
                }
            }
            "master" => match value.as_str() {
                "on" | "true" => self.master = true,
                "off" | "false" => self.master = false,
                _ => self.extra.push(line.to_string()),
            },
            "node" => {
                if value.is_empty() {
                    self.extra.push(line.to_string());
                } else if self.nodes.iter().any(|n| n.target == value) {
                    // 重复节点按首见为准(dedupe,不重复轮询)
                } else if self.nodes.len() < MAX_NODES {
                    self.nodes.push(NodeCfg {
                        target: value,
                        cmd: None,
                    });
                }
            }
            k if k.starts_with("cmd ") => {
                let target = k[4..].trim().to_string();
                if !target.is_empty() && !value.is_empty() {
                    cmd_for.insert(target, value);
                } else {
                    self.extra.push(line.to_string());
                }
            }
            "alert" => match parse_alert_line(t) {
                Some(r) => self.alerts.push(r),
                None => {
                    self.bad_alerts.push(t.to_string());
                    self.extra.push(line.to_string());
                }
            },
            _ => self.extra.push(line.to_string()),
        }
    }

    /// 回写内容:已知键在前,未知行原样殿后
    pub fn content(&self) -> String {
        let mut out = String::new();
        out.push_str("# ltop fleet: nodes & options (hand-editable; `ltop fleet --host ... --save` writes here too)\n");
        out.push_str(&format!("interval = {}\n", self.interval));
        out.push_str(&format!("timeout = {}\n", self.timeout));
        out.push_str(&format!(
            "master = {}\n",
            if self.master { "on" } else { "off" }
        ));
        for n in &self.nodes {
            out.push_str(&format!("node = {}\n", n.target));
        }
        for n in &self.nodes {
            if let Some(c) = &n.cmd {
                out.push_str(&format!("cmd {} = {}\n", n.target, c));
            }
        }
        for a in &self.alerts {
            out.push_str(&format!("{}\n", a.source()));
        }
        for l in &self.extra {
            out.push_str(l);
            out.push('\n');
        }
        out
    }

    /// 原子保存;测试环境 no-op,不写宿主真实 fleet.conf(与主 config 同款纪律)
    #[cfg(not(test))]
    pub fn save(&self) -> std::io::Result<()> {
        let Some(path) = Self::path() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no config dir",
            ));
        };
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        write_atomic_0600(&path, &self.content())
    }

    #[cfg(test)]
    pub fn save(&self) -> std::io::Result<()> {
        Ok(())
    }

    /// 与 CLI --host 合并:CLI 条目追加在后、按 target 去重、CLI 的 cmd 优先
    pub fn merge_hosts(&mut self, hosts: &[(String, String)]) {
        for (target, cmd) in hosts {
            let def_cmd = cmd != DEFAULT_CMD;
            match self.nodes.iter_mut().find(|n| &n.target == target) {
                Some(n) => {
                    if def_cmd {
                        n.cmd = Some(cmd.clone());
                    }
                }
                None => {
                    if self.nodes.len() < MAX_NODES {
                        self.nodes.push(NodeCfg {
                            target: target.clone(),
                            cmd: if def_cmd { Some(cmd.clone()) } else { None },
                        });
                    }
                }
            }
        }
    }
}

/// 值内联注释剥离:`5  # 注释` → `5`(只在已知键的值上调用)
fn strip_inline_comment(v: &str) -> &str {
    match v.find(" #") {
        Some(i) => &v[..i],
        None => v,
    }
}

/// 原子写;新文件显式 0600(节点表是内网拓扑),既有文件权限存续
#[cfg_attr(test, allow(dead_code))]
fn write_atomic_0600(path: &std::path::Path, content: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    let existing = fs::metadata(path).ok().map(|m| m.permissions());
    {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        f.write_all(content.as_bytes())?;
    }
    match existing {
        Some(p) => {
            let _ = fs::set_permissions(&tmp, p);
        }
        None => {
            let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600));
        }
    }
    match fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_keys_and_nodes() {
        let c = FleetConf::parse(
            "interval = 3\ntimeout = 20\nmaster = off\nnode = air\nnode = fedora\ncmd fedora = /usr/local/bin/ltop --json\n",
        );
        assert_eq!(c.interval, 3);
        assert_eq!(c.timeout, 20);
        assert!(!c.master);
        assert_eq!(c.nodes.len(), 2);
        assert_eq!(c.nodes[0].target, "air");
        assert_eq!(c.nodes[0].cmd, None);
        // cmd 行写在 node 前面也能挂上(顺序无关)
        let reversed = FleetConf::parse("cmd fedora = /x/ltop --json\nnode = fedora\n");
        assert_eq!(reversed.nodes[0].cmd.as_deref(), Some("/x/ltop --json"));
    }

    #[test]
    fn unknown_lines_survive_roundtrip() {
        let text = "# my note\ninterval = 7\ncustom_thing = 42\nnode = air\n";
        let c = FleetConf::parse(text);
        assert_eq!(c.interval, 7);
        assert_eq!(c.extra, vec!["# my note", "custom_thing = 42"]);
        let out = c.content();
        assert!(out.contains("# my note"));
        assert!(out.contains("custom_thing = 42"));
        assert!(out.contains("node = air"));
    }

    #[test]
    fn bad_values_fall_back_to_extra() {
        let c = FleetConf::parse("interval = abc\nnode =\nmaster = maybe\n");
        assert_eq!(c.interval, 5, "坏值回落默认且保留原行");
        assert!(c.master);
        assert!(c.extra.iter().any(|l| l.contains("interval = abc")));
        assert!(c.extra.iter().any(|l| l.contains("master = maybe")));
        assert_eq!(c.nodes.len(), 0, "空 node 行不入表");
    }

    #[test]
    fn duplicate_nodes_dedupe() {
        let c = FleetConf::parse("node = air\nnode = air\nnode = fedora\n");
        assert_eq!(c.nodes.len(), 2);
        assert_eq!(c.nodes[0].target, "air");
    }

    #[test]
    fn inline_comments_stripped_on_known_keys() {
        let c = FleetConf::parse("interval = 9 # faster\nnode = air # my laptop\n");
        assert_eq!(c.interval, 9);
        assert_eq!(c.nodes[0].target, "air");
    }

    #[test]
    fn merge_hosts_dedupes_and_prefers_cli_cmd() {
        let mut c = FleetConf::parse("node = air\nnode = fedora\ncmd fedora = /old/ltop --json\n");
        c.merge_hosts(&[
            ("air".to_string(), DEFAULT_CMD.to_string()),
            ("nas".to_string(), "/opt/ltop --json".to_string()),
            ("fedora".to_string(), "/new/ltop --json".to_string()),
        ]);
        assert_eq!(c.nodes.len(), 3);
        assert_eq!(c.nodes[0].target, "air");
        assert_eq!(c.nodes[0].cmd, None, "默认 cmd 不覆盖 None");
        assert_eq!(
            c.nodes[1].cmd.as_deref(),
            Some("/new/ltop --json"),
            "CLI cmd 优先"
        );
        assert_eq!(c.nodes[2].cmd.as_deref(), Some("/opt/ltop --json"));
    }

    #[test]
    fn node_cap_enforced() {
        let mut text = String::new();
        for i in 0..(MAX_NODES + 10) {
            text.push_str(&format!("node = h{i}\n"));
        }
        let c = FleetConf::parse(&text);
        assert_eq!(c.nodes.len(), MAX_NODES);
    }
}

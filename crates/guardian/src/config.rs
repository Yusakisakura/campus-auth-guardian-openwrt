//! UCI 配置模型与解析。
//!
//! # 为什么是 UCI 而不是继续用 INI
//!
//! 换成 UCI 的真正收益不是「更 OpenWrt」，而是**省掉一整个 LuCI 配置后端**：
//! 配置既然是 UCI，LuCI 就能用内置的 `uci` RPC 直接读写，配置页不需要写任何自定义
//! ubus 方法。自定义 rpcd 后端因此只剩「看状态 / 看日志 / 点立即认证」三件事。
//!
//! 上游的旧版 INI 兼容逻辑（`user_account = ,0,学号@运营商`）已丢弃 —— 新仓库没有存量用户。

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// UCI 配置文件路径。
pub const UCI_PATH: &str = "/etc/config/campus-auth-guardian";

/// 运营商类型。`user_account` 里的 `@后缀` 与之对应。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Operator {
    /// 校园网自有网络
    #[default]
    Campus,
    /// 中国移动
    Cmcc,
    /// 中国联通
    Unicom,
    /// 中国电信
    Telecom,
}

impl Operator {
    pub fn as_str(self) -> &'static str {
        match self {
            Operator::Campus => "campus",
            Operator::Cmcc => "cmcc",
            Operator::Unicom => "unicom",
            Operator::Telecom => "telecom",
        }
    }

    /// 全部运营商，供 UI 下拉框使用。
    pub const ALL: [Operator; 4] = [
        Operator::Campus,
        Operator::Cmcc,
        Operator::Unicom,
        Operator::Telecom,
    ];

    pub fn display(self) -> &'static str {
        match self {
            Operator::Campus => "校园网",
            Operator::Cmcc => "中国移动",
            Operator::Unicom => "中国联通",
            Operator::Telecom => "中国电信",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "campus" => Some(Operator::Campus),
            "cmcc" => Some(Operator::Cmcc),
            "unicom" => Some(Operator::Unicom),
            "telecom" => Some(Operator::Telecom),
            _ => None,
        }
    }
}

impl fmt::Display for Operator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 应用配置。
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// 守护总开关。关闭时进程仍运行并检测连通性，但不发起认证。
    pub enabled: bool,
    pub auth_url: String,
    pub check_url: String,
    pub check_interval: Duration,
    /// WAN 接口名。仅用于 [`crate::ipdetect::detect_local_ip`] 兜底；主路径是 UDP 探测。
    pub wan_iface: Option<String>,
    /// 固定 IP；空 = 自动探测
    pub fixed_ip: Option<String>,
    pub student_id: String,
    pub operator: Operator,
    pub password: String,
    pub retry_interval: Duration,
    pub max_retries: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            // 路由器上默认开启守护 —— 装完就该干活，而不是等用户去点开关
            enabled: true,
            // 留空而非填占位地址。上游默认值是某校真实门户地址，照抄等于公开自己的学校；
            // 而随便填个假地址又会让未配置的机器拿它去认证。留空 + validate() 报错最诚实。
            auth_url: String::new(),
            check_url: "http://www.baidu.com".into(),
            check_interval: Duration::from_secs(30),
            wan_iface: Some("wan".into()),
            fixed_ip: None,
            student_id: String::new(),
            operator: Operator::default(),
            password: String::new(),
            retry_interval: Duration::from_secs(10),
            max_retries: 3,
        }
    }
}

impl Config {
    /// 归一化认证地址：允许只填服务器（`http://10.0.0.1/` 或裸 `10.0.0.1`），
    /// 自动补全登录端点；已含 `/eportal` 的完整地址原样保留；未指定端口默认 `:801`。
    pub fn normalize_auth_url(raw: &str) -> String {
        let s = raw.trim();
        if s.is_empty() || s.contains("/eportal") {
            return s.into();
        }
        let mut s = s.to_string();
        if !s.contains("://") {
            s = format!("http://{s}");
        }
        let no_query = s.split(['#', '?']).next().unwrap_or(&s);
        let base = no_query.trim_end_matches('/');
        let after_scheme = base.split("://").nth(1).unwrap_or(base);
        let authority = after_scheme.split('/').next().unwrap_or(after_scheme);
        // IPv6 中括号段含冒号，需排除后再判端口
        let host_part = authority.rsplit(']').next().unwrap_or(authority);
        let with_port = if host_part.contains(':') {
            base.to_string()
        } else {
            format!("{base}:801")
        };
        format!("{with_port}/eportal/portal/login")
    }

    /// 认证服务器基础地址（scheme+host+port），如 `http://10.0.0.1:801`。
    pub fn portal_base(&self) -> &str {
        self.auth_url
            .split("/eportal")
            .next()
            .unwrap_or(&self.auth_url)
            .trim_end_matches('/')
    }

    /// ePortal 登录页地址，用于抓取 PHPSESSID。
    pub fn login_page_url(&self) -> String {
        format!("{}/srun_portal_pc.php?ac_id=1&", self.portal_base())
    }

    /// 解析 UCI 文本。缺失字段保留默认值。
    pub fn parse(text: &str) -> Self {
        let sections = parse_uci(text);
        let mut cfg = Config::default();

        // 取名为 main 的节；没有具名节时退而取第一个同类型节
        let Some(sec) = sections
            .iter()
            .find(|s| s.name.as_deref() == Some("main"))
            .or_else(|| sections.first())
        else {
            return cfg;
        };

        if let Some(v) = sec.get("enabled") {
            if let Some(b) = parse_bool(v) {
                cfg.enabled = b;
            }
        }
        // 注意：这里没有「空值就保留默认」的短路 —— 默认本来就是空的，
        // 而显式写空串应当就是「未配置」，由 validate() 报出来。
        if let Some(v) = sec.get("auth_url") {
            cfg.auth_url = Config::normalize_auth_url(v);
        }
        if let Some(v) = sec.get("check_url") {
            if !v.trim().is_empty() {
                cfg.check_url = v.trim().to_string();
            }
        }
        if let Some(v) = sec.get("check_interval").and_then(|v| parse_bounded_u64(v, 1, 3600)) {
            cfg.check_interval = Duration::from_secs(v);
        }
        if let Some(v) = sec.get("wan_iface") {
            cfg.wan_iface = non_empty(v);
        }
        if let Some(v) = sec.get("fixed_ip") {
            cfg.fixed_ip = non_empty(v);
        }
        if let Some(v) = sec.get("student_id") {
            cfg.student_id = v.trim().to_string();
        }
        if let Some(op) = sec.get("operator").and_then(Operator::parse) {
            cfg.operator = op;
        }
        if let Some(v) = sec.get("password") {
            cfg.password = v.to_string();
        }
        if let Some(v) = sec.get("retry_interval").and_then(|v| parse_bounded_u64(v, 1, 3600)) {
            cfg.retry_interval = Duration::from_secs(v);
        }
        if let Some(v) = sec.get("max_retries").and_then(|v| parse_bounded_u64(v, 1, 100)) {
            cfg.max_retries = v as u32;
        }

        cfg
    }

    /// 从文件读取配置。文件不存在时返回默认配置（不写文件 —— 默认配置由 ipk 提供）。
    pub fn load(path: &Path) -> std::io::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => Ok(Config::parse(&text)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(e),
        }
    }

    /// 默认配置路径。
    pub fn default_path() -> PathBuf {
        PathBuf::from(UCI_PATH)
    }

    /// 校验配置，返回问题列表（空 = 没问题）。
    ///
    /// 供 `campus-auth-guardian validate` 与 LuCI 的即时反馈使用。
    pub fn validate(&self) -> Vec<String> {
        let mut problems = Vec::new();

        if self.student_id.trim().is_empty() {
            problems.push("学号未填写（student_id）".into());
        }
        if self.password.is_empty() {
            problems.push("密码未填写（password）".into());
        }
        if self.auth_url.trim().is_empty() {
            problems.push("认证服务器地址未填写（auth_url）".into());
        } else if crate::http::parse_url(&self.auth_url).is_none() {
            problems.push(format!("认证服务器地址无法解析：{}", self.auth_url));
        }
        if crate::http::parse_url(&self.check_url).is_none() {
            problems.push(format!("连通性检测地址无法解析：{}", self.check_url));
        }
        if let Some(ip) = self.fixed_ip.as_deref() {
            if !crate::ipdetect::is_usable_ipv4(ip) {
                problems.push(format!("fixed_ip 不是可用的 IPv4 地址：{ip}"));
            }
        }

        problems
    }
}

fn non_empty(v: &str) -> Option<String> {
    let t = v.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

fn parse_bool(v: &str) -> Option<bool> {
    match v.trim() {
        "1" | "true" | "yes" | "on" | "enabled" => Some(true),
        "0" | "false" | "no" | "off" | "disabled" => Some(false),
        _ => None,
    }
}

/// 解析整数并做区间校验；越界返回 None（调用方保留默认值）。
fn parse_bounded_u64(v: &str, lo: u64, hi: u64) -> Option<u64> {
    let n: u64 = v.trim().parse().ok()?;
    (lo..=hi).contains(&n).then_some(n)
}

// ---------------------------------------------------------------------------
// UCI 解析
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct Section {
    #[allow(dead_code)]
    kind: String,
    name: Option<String>,
    options: Vec<(String, String)>,
}

impl Section {
    fn get(&self, key: &str) -> Option<&str> {
        self.options
            .iter()
            .rev() // 后写的覆盖先写的
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v.as_str())
    }
}

/// 解析 UCI 文本为节列表。只关心 `option`，`list` 一并按普通键值处理。
fn parse_uci(text: &str) -> Vec<Section> {
    let mut sections: Vec<Section> = Vec::new();

    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let mut parts = line.splitn(2, char::is_whitespace);
        let keyword = parts.next().unwrap_or("");
        let rest = parts.next().unwrap_or("").trim();

        match keyword {
            "config" => {
                let mut it = rest.splitn(2, char::is_whitespace);
                let kind = it.next().unwrap_or("").to_string();
                let name = it
                    .next()
                    .map(|s| parse_uci_value(s.trim()))
                    .filter(|s| !s.is_empty());
                sections.push(Section {
                    kind,
                    name,
                    options: Vec::new(),
                });
            }
            "option" | "list" => {
                let Some(sec) = sections.last_mut() else {
                    continue;
                };
                if let Some((k, v)) = split_key_value(rest) {
                    sec.options.push((k, v));
                }
            }
            _ => {}
        }
    }

    sections
}

/// 把 `key 'value'` 拆成 (key, value)。
fn split_key_value(rest: &str) -> Option<(String, String)> {
    let rest = rest.trim_start();
    let idx = rest.find(char::is_whitespace)?;
    let (k, v) = rest.split_at(idx);
    if k.is_empty() {
        return None;
    }
    Some((k.to_string(), parse_uci_value(v.trim())))
}

/// 去掉 UCI 值的引号并处理转义。支持单引号、双引号和裸值。
fn parse_uci_value(raw: &str) -> String {
    let raw = raw.trim();

    if raw.len() >= 2 && raw.starts_with('\'') && raw.ends_with('\'') {
        let inner = &raw[1..raw.len() - 1];
        let mut out = String::with_capacity(inner.len());
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.next() {
                    Some('\'') => out.push('\''),
                    Some('\\') => out.push('\\'),
                    Some(other) => {
                        out.push('\\');
                        out.push(other);
                    }
                    None => out.push('\\'),
                }
            } else {
                out.push(c);
            }
        }
        return out;
    }

    if raw.len() >= 2 && raw.starts_with('"') && raw.ends_with('"') {
        return raw[1..raw.len() - 1].to_string();
    }

    raw.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
config campus-auth-guardian 'main'
	option enabled '1'
	option auth_url 'http://10.0.0.1:801/eportal/portal/login'
	option check_url 'http://www.baidu.com'
	option check_interval '15'
	option wan_iface 'wan'
	option fixed_ip ''
	option operator 'unicom'
	option student_id '12345678'
	option password 'TestPass123'
	option retry_interval '5'
	option max_retries '7'
"#;

    #[test]
    fn parses_full_config() {
        let cfg = Config::parse(SAMPLE);
        assert!(cfg.enabled);
        assert_eq!(
            cfg.auth_url,
            "http://10.0.0.1:801/eportal/portal/login"
        );
        assert_eq!(cfg.check_interval, Duration::from_secs(15));
        assert_eq!(cfg.wan_iface.as_deref(), Some("wan"));
        assert_eq!(cfg.fixed_ip, None, "空值应归一为 None");
        assert_eq!(cfg.operator, Operator::Unicom);
        assert_eq!(cfg.student_id, "12345678");
        assert_eq!(cfg.password, "TestPass123");
        assert_eq!(cfg.retry_interval, Duration::from_secs(5));
        assert_eq!(cfg.max_retries, 7);
    }

    #[test]
    fn empty_text_gives_defaults() {
        assert_eq!(Config::parse(""), Config::default());
        assert_eq!(Config::parse("\n# 只有注释\n   \n"), Config::default());
    }

    #[test]
    fn defaults_enabled_on_router() {
        // 路由器上默认应开启守护：装完即工作
        assert!(Config::default().enabled);
    }

    #[test]
    fn interval_bounds_clamped_to_default() {
        let cfg = Config::parse(
            "config campus-auth-guardian 'main'\n\toption check_interval '99999'\n\toption retry_interval '0'\n\toption max_retries '9999'\n",
        );
        assert_eq!(cfg.check_interval, Duration::from_secs(30));
        assert_eq!(cfg.retry_interval, Duration::from_secs(10));
        assert_eq!(cfg.max_retries, 3);
    }

    #[test]
    fn interval_bounds_edges_accepted() {
        let lo = Config::parse(
            "config x 'main'\n\toption check_interval '1'\n\toption retry_interval '1'\n\toption max_retries '1'\n",
        );
        assert_eq!(lo.check_interval, Duration::from_secs(1));
        assert_eq!(lo.max_retries, 1);

        let hi = Config::parse(
            "config x 'main'\n\toption check_interval '3600'\n\toption retry_interval '3600'\n\toption max_retries '100'\n",
        );
        assert_eq!(hi.check_interval, Duration::from_secs(3600));
        assert_eq!(hi.max_retries, 100);
    }

    #[test]
    fn value_quoting_and_escapes() {
        assert_eq!(parse_uci_value("'abc'"), "abc");
        assert_eq!(parse_uci_value("\"abc\""), "abc");
        assert_eq!(parse_uci_value("abc"), "abc");
        assert_eq!(parse_uci_value("'a b c'"), "a b c");
        assert_eq!(parse_uci_value(r"'it\'s'"), "it's");
        assert_eq!(parse_uci_value(r"'back\\slash'"), r"back\slash");
        assert_eq!(parse_uci_value("''"), "");
    }

    #[test]
    fn value_with_equals_and_spaces() {
        let cfg = Config::parse(
            "config x 'main'\n\toption password 'a=b c=d'\n\toption student_id 'x y'\n",
        );
        assert_eq!(cfg.password, "a=b c=d");
        assert_eq!(cfg.student_id, "x y");
    }

    #[test]
    fn anonymous_section_still_parsed() {
        // 没有具名节时退回第一个节，避免手写配置漏了 'main' 就整个失效
        let cfg = Config::parse("config campus-auth-guardian\n\toption student_id 'abc'\n");
        assert_eq!(cfg.student_id, "abc");
    }

    #[test]
    fn later_option_wins() {
        let cfg = Config::parse("config x 'main'\n\toption student_id 'a'\n\toption student_id 'b'\n");
        assert_eq!(cfg.student_id, "b");
    }

    #[test]
    fn unicode_values_survive() {
        let cfg = Config::parse("config x 'main'\n\toption student_id '学号123🎯'\n");
        assert_eq!(cfg.student_id, "学号123🎯");
    }

    #[test]
    fn normalize_auth_url_cases() {
        assert_eq!(
            Config::normalize_auth_url("http://10.0.0.1/"),
            "http://10.0.0.1:801/eportal/portal/login"
        );
        assert_eq!(
            Config::normalize_auth_url("10.0.0.1"),
            "http://10.0.0.1:801/eportal/portal/login"
        );
        assert_eq!(
            Config::normalize_auth_url("http://10.0.0.1:8080/"),
            "http://10.0.0.1:8080/eportal/portal/login"
        );
        // 完整地址原样保留
        assert_eq!(
            Config::normalize_auth_url("http://10.0.0.1:801/eportal/portal/login"),
            "http://10.0.0.1:801/eportal/portal/login"
        );
        assert_eq!(Config::normalize_auth_url(""), "");
    }

    #[test]
    fn portal_base_and_login_page() {
        let mut cfg = Config::default();
        cfg.auth_url = "http://10.0.0.1:801/eportal/portal/login".into();
        assert_eq!(cfg.portal_base(), "http://10.0.0.1:801");
        assert_eq!(
            cfg.login_page_url(),
            "http://10.0.0.1:801/srun_portal_pc.php?ac_id=1&"
        );
    }

    #[test]
    fn validate_reports_missing_credentials() {
        let cfg = Config::default();
        let problems = cfg.validate();
        assert!(problems.iter().any(|p| p.contains("学号")));
        assert!(problems.iter().any(|p| p.contains("密码")));
    }

    #[test]
    fn empty_auth_url_stays_empty_and_is_reported() {
        // 回归：ipk 里装的是空模板，此时绝不能退回到某个占位地址去认证
        let cfg = Config::parse("config x 'main'\n\toption auth_url ''\n");
        assert_eq!(cfg.auth_url, "");
        assert!(cfg.validate().iter().any(|p| p.contains("认证服务器地址")));
    }

    #[test]
    fn default_auth_url_is_empty() {
        assert_eq!(Config::default().auth_url, "");
    }

    #[test]
    fn validate_accepts_complete_config() {
        let cfg = Config::parse(SAMPLE);
        assert_eq!(cfg.validate(), Vec::<String>::new());
    }

    #[test]
    fn validate_rejects_bad_fixed_ip() {
        let cfg = Config::parse(
            "config x 'main'\n\toption student_id '1'\n\toption password 'p'\n\toption fixed_ip '169.254.1.1'\n",
        );
        assert!(cfg.validate().iter().any(|p| p.contains("fixed_ip")));
    }

    #[test]
    fn operator_parse_and_display() {
        assert_eq!(Operator::parse("UNICOM"), Some(Operator::Unicom));
        assert_eq!(Operator::parse(" cmcc "), Some(Operator::Cmcc));
        assert_eq!(Operator::parse("bogus"), None);
        assert_eq!(Operator::Campus.display(), "校园网");
        assert_eq!(Operator::Telecom.as_str(), "telecom");
        assert_eq!(Operator::ALL.len(), 4);
    }
}

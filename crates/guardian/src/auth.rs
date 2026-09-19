//! ePortal JSONP 认证协议实现。
//!
//! 协议要点（从现网抓包/日志还原）：
//! - 请求：GET `{base}/eportal/portal/login?callback=dr1005&login_method=1&...`
//! - `user_account` 需 URL 编码：`,0,{学号}@{运营商}` → `%2C0%2C...%40unicom`
//! - 响应：JSONP `dr1005({"result":1,"msg":"...","ret_code":0})`
//! - `result=1` 成功；`result=0` 且 `ret_code=2` 已在线（视为成功）；其余失败
//!
//! 相对上游的两处改动：候选 IP 的语义（见 [`candidate_ips`]），以及日志脱敏
//! （见 [`redact_password`]）。

use std::time::Duration;

use serde_json::Value;

use crate::config::Config;
use crate::http;
use crate::ipdetect;
use crate::{log_info, log_warn};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthOutcome {
    Success,
    AlreadyOnline,
    Failed { msg: String },
    NetworkError { msg: String },
}

impl AuthOutcome {
    pub fn is_ok(&self) -> bool {
        matches!(self, AuthOutcome::Success | AuthOutcome::AlreadyOnline)
    }

    pub fn describe(&self) -> String {
        match self {
            AuthOutcome::Success => "成功".into(),
            AuthOutcome::AlreadyOnline => "已在线".into(),
            AuthOutcome::Failed { msg } => format!("失败({msg})"),
            AuthOutcome::NetworkError { msg } => format!("网络错误({msg})"),
        }
    }
}

/// URL 百分号编码（RFC 3986 非保留字符外的全部转义）。
pub fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// 构造认证 URL。`ac` 为 (wlan_ac_ip, wlan_ac_name)，从 captive portal 重定向提取；未知传空。
pub fn build_login_url(cfg: &Config, ip: &str, callback: &str, ac: Option<(&str, &str)>) -> String {
    let account = match cfg.operator {
        crate::config::Operator::Campus => format!(",0,{}", cfg.student_id),
        _ => format!(",0,{}@{}", cfg.student_id, cfg.operator.as_str()),
    };
    let (ac_ip, ac_name) = ac.unwrap_or(("", ""));
    format!(
        "{}?callback={cb}&login_method=1&user_account={acc}&user_password={pw}\
         &wlan_user_ip={ip}&wlan_user_ipv6=&wlan_user_mac=000000000000\
         &wlan_ac_ip={acip}&wlan_ac_name={acname}&jsVersion=4.1.3&terminal_type=1&lang=zh-cn&v=3015&lang=zh",
        cfg.auth_url,
        cb = urlencode(callback),
        acc = urlencode(&account),
        pw = urlencode(&cfg.password),
        ip = urlencode(ip),
        acip = urlencode(ac_ip),
        acname = urlencode(ac_name),
    )
}

/// 抹掉 URL 里 `user_password` 的值，供日志使用。
///
/// 上游把完整 URL（含明文密码）直接打进日志。在 Windows 桌面版上那只落在本机文件里；
/// 在路由器上它会进 syslog、被 `logread` 看到、被用户贴进 issue。所以这里必须脱敏。
pub fn redact_password(url: &str) -> String {
    const KEY: &str = "user_password=";
    let Some(start) = url.find(KEY) else {
        return url.to_string();
    };
    let val_start = start + KEY.len();
    let val_end = url[val_start..]
        .find('&')
        .map(|i| val_start + i)
        .unwrap_or(url.len());
    format!("{}***{}", &url[..val_start], &url[val_end..])
}

/// 解析 JSONP `dr1005({...})` 提取 result/ret_code/msg。
fn parse_jsonp(text: &str) -> Option<(i64, Option<i64>, String)> {
    let start = text.find('(')?;
    let end = text.rfind(')')?;
    if start >= end {
        return None;
    }
    let v: Value = serde_json::from_str(&text[start + 1..end]).ok()?;
    let result = v.get("result")?.as_i64()?;
    let ret_code = v.get("ret_code").and_then(|r| r.as_i64());
    let msg = v
        .get("msg")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();
    Some((result, ret_code, msg))
}

/// 根据 JSONP 响应判定认证结果。
pub fn interpret(jsonp: &str) -> AuthOutcome {
    match parse_jsonp(jsonp) {
        Some((1, _msg, _)) => AuthOutcome::Success,
        Some((0, Some(2), _msg)) => AuthOutcome::AlreadyOnline,
        Some((0, _, msg)) => AuthOutcome::Failed { msg },
        Some((code, _, _)) => AuthOutcome::Failed {
            msg: format!("未知 result={code}"),
        },
        None => AuthOutcome::Failed {
            msg: "响应非 JSONP".into(),
        },
    }
}

/// 执行一次认证。
pub fn authenticate(cfg: &Config) -> AuthOutcome {
    authenticate_inner(cfg, None)
}

/// 带门户 AC 参数的认证（captive portal 检测到后调用）。
/// `ac` = (wlanacip, wlanacname, wlanuserip)。
pub fn authenticate_with_ac(cfg: &Config, ac: Option<(&str, &str, &str)>) -> AuthOutcome {
    authenticate_inner(cfg, ac)
}

fn authenticate_inner(cfg: &Config, ac: Option<(&str, &str, &str)>) -> AuthOutcome {
    let (ac_ip, ac_name) = ac.map(|(i, n, _)| (i, n)).unwrap_or(("", ""));
    let portal_ip = ac.map(|(_, _, u)| u);

    let candidates = candidate_ips(cfg, portal_ip);
    if candidates.is_empty() {
        return AuthOutcome::NetworkError {
            msg: "未探测到可用于认证的本机 IP".into(),
        };
    }
    log_info!("认证候选 IP: {candidates:?}");

    let mut last: Option<AuthOutcome> = None;
    for (i, ip) in candidates.iter().enumerate() {
        let outcome = authenticate_with_ip(cfg, ip, (ac_ip, ac_name));
        log_info!(
            "候选 {}/{} IP {ip} 结果: {}",
            i + 1,
            candidates.len(),
            outcome.describe()
        );
        if outcome.is_ok() {
            return outcome; // 成功/已在线立即返回
        }
        last = Some(outcome);
    }
    last.unwrap_or_else(|| AuthOutcome::NetworkError {
        msg: "全部候选 IP 认证失败".into(),
    })
}

/// 按优先级收集候选 IP。
///
/// 上游是「枚举本机全部网卡按评分排序」—— 那台 PC 本身就是校园网客户端，合理。
/// 路由器上不行：`br-lan`、docker 网桥、Tailscale 的地址都会被拿去认证。所以这里改成：
///
/// 1. `fixed_ip` —— 用户显式指定，永远最高优先级
/// 2. **内核选出的、通往门户的源 IP** —— 路由器上这就是门户看到的那个地址，最权威
/// 3. 门户在重定向里报的会话 IP —— 交叉验证；多 WAN 或门户会话过期时可能才是对的
/// 4. 兜底：按接口名 / 网段打分枚举（[`ipdetect::detect_local_ip`]）
///
/// 顺序与上游不同（上游把门户报的 IP 放第一）。路由器上内核的答案总是最新的，
/// 而门户报的 IP 可能来自 DHCP 换址之前的旧会话，所以内核优先。
fn candidate_ips(cfg: &Config, portal_ip: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();

    if let Some(f) = cfg.fixed_ip.as_deref() {
        push_unique(&mut out, f);
    }

    // 用门户地址做一次路由查找，问内核「从哪个地址出去」
    let kernel_ip = http::parse_url(&cfg.auth_url)
        .and_then(|(_, host, port, _)| ipdetect::source_ip_for(&host, port));
    if let Some(ip) = kernel_ip.as_deref() {
        push_unique(&mut out, ip);
    }

    if let Some(ip) = portal_ip {
        push_unique(&mut out, ip);
    }

    if let Some(ip) = ipdetect::detect_local_ip(cfg.wan_iface.as_deref()) {
        push_unique(&mut out, &ip);
    }

    // 两者不一致值得记一条：多半是 DHCP 刚换过地址，或存在多 WAN。
    // 没有这条日志的话，认证失败时很难判断到底该信谁。
    if let (Some(k), Some(p)) = (kernel_ip.as_deref(), portal_ip) {
        let p = p.trim();
        if !p.is_empty() && k != p {
            log_warn!("源 IP 与门户报告的会话 IP 不一致：内核={k} 门户={p}（DHCP 刚变更或多 WAN？）");
        }
    }

    out
}

fn push_unique(out: &mut Vec<String>, ip: &str) {
    let ip = ip.trim();
    if !ip.is_empty() && !out.iter().any(|x| x == ip) {
        out.push(ip.to_string());
    }
}

fn authenticate_with_ip(cfg: &Config, ip: &str, ac: (&str, &str)) -> AuthOutcome {
    /// 连门户通常是内网一跳，5s 足够
    const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
    /// 门户偶有慢响应，读放宽到 10s
    const READ_TIMEOUT: Duration = Duration::from_secs(10);

    // 先 GET 一次登录页，与上游行为一致。注意 http.rs 没有 cookie jar，所以这次请求
    // 带不上 PHPSESSID —— 上游用 ureq 时也没开 cookies feature，行为相同。
    // 保留它是因为部分门户会在登录页被访问时登记客户端；失败不影响后续认证。
    let _ = http::get(&cfg.login_page_url(), CONNECT_TIMEOUT);

    let url = build_login_url(cfg, ip, "dr1005", Some(ac));
    log_info!("认证请求: {}", redact_password(&url));

    match http::get(&url, READ_TIMEOUT) {
        Ok(resp) => {
            log_info!("HTTP {}", resp.status);
            if !resp.body.is_empty() {
                log_info!("响应: {}", truncate(&resp.body, 300));
            }
            match interpret(&resp.body) {
                // 非 2xx 且正文不是 JSONP：多半是被网关挡了，报网络错误比「响应非 JSONP」有指向性
                AuthOutcome::Failed { msg } if !(200..300).contains(&resp.status) => {
                    AuthOutcome::NetworkError {
                        msg: format!("HTTP {} - {msg}", resp.status),
                    }
                }
                other => other,
            }
        }
        Err(e) => AuthOutcome::NetworkError { msg: e.to_string() },
    }
}

fn truncate(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Operator;

    fn cfg() -> Config {
        let mut c = Config::default();
        c.auth_url = "http://10.0.0.1:801/eportal/portal/login".into();
        c.student_id = "12345678".into();
        c.operator = Operator::Unicom;
        c.password = "TestPass123".into();
        c
    }

    #[test]
    fn url_encode_account() {
        assert_eq!(urlencode(",0,12345678@unicom"), "%2C0%2C12345678%40unicom");
        assert_eq!(urlencode("pw/1+2"), "pw%2F1%2B2");
    }

    #[test]
    fn urlencode_edge_empty_and_special() {
        assert_eq!(urlencode(""), "");
        assert_eq!(urlencode("=&%#+"), "%3D%26%25%23%2B");
        assert_eq!(urlencode("中文"), "%E4%B8%AD%E6%96%87");
        assert_eq!(urlencode("a b"), "a%20b");
        assert_eq!(urlencode("-_.~"), "-_.~"); // 非保留字符
    }

    #[test]
    fn login_url_shape() {
        let url = build_login_url(&cfg(), "10.20.30.40", "dr1005", None);
        assert!(url.starts_with("http://10.0.0.1:801/eportal/portal/login?callback=dr1005&"));
        assert!(url.contains("user_account=%2C0%2C12345678%40unicom"));
        assert!(url.contains("user_password=TestPass123"));
        assert!(url.contains("wlan_user_ip=10.20.30.40"));
        assert!(url.contains("jsVersion=4.1.3"));
    }

    #[test]
    fn interpret_success() {
        let o = interpret(r#"dr1005({"result":1,"msg":"Portal协议认证成功！"});"#);
        assert_eq!(o, AuthOutcome::Success);
    }

    #[test]
    fn interpret_already_online() {
        let o = interpret(r#"dr1005({"result":0,"msg":"IP: 10.20.30.41 已经在线！","ret_code":2});"#);
        assert_eq!(o, AuthOutcome::AlreadyOnline);
    }

    #[test]
    fn interpret_ac_fail() {
        let o = interpret(r#"dr1005({"result":0,"msg":"AC认证失败","ret_code":1});"#);
        assert_eq!(
            o,
            AuthOutcome::Failed {
                msg: "AC认证失败".into()
            }
        );
    }

    #[test]
    fn interpret_garbage() {
        assert!(matches!(interpret("not jsonp"), AuthOutcome::Failed { .. }));
    }

    #[test]
    fn interpret_success_has_no_ret_code() {
        // result 为字符串时按失败处理（服务端不会这样发；防御性）
        let o = interpret(r#"dr1005({"result":"1","msg":"ok"});"#);
        assert!(matches!(o, AuthOutcome::Failed { .. }));
    }

    #[test]
    fn parse_jsonp_msg_with_brackets() {
        // msg 内含括号：rfind(')') 应正确取到最外层
        let o = interpret(r#"dr1005({"result":0,"msg":"失败(原因:AC(x))","ret_code":1});"#);
        assert_eq!(
            o,
            AuthOutcome::Failed {
                msg: "失败(原因:AC(x))".into()
            }
        );
    }

    #[test]
    fn interpret_unknown_result_code() {
        let o = interpret(r#"dr1005({"result":3,"msg":"奇怪"});"#);
        assert!(matches!(o, AuthOutcome::Failed { .. }));
    }

    #[test]
    fn parse_jsonp_no_parens() {
        assert!(matches!(interpret("dr1005"), AuthOutcome::Failed { .. }));
    }

    #[test]
    fn outcome_describe_covers_all() {
        assert_eq!(AuthOutcome::Success.describe(), "成功");
        assert_eq!(AuthOutcome::AlreadyOnline.describe(), "已在线");
        assert!(AuthOutcome::Success.is_ok());
        assert!(!AuthOutcome::Failed { msg: "x".into() }.is_ok());
    }

    // --- 脱敏 ---

    #[test]
    fn redact_hides_password() {
        let url = build_login_url(&cfg(), "10.20.30.40", "dr1005", None);
        let safe = redact_password(&url);
        assert!(!safe.contains("TestPass123"), "密码不该出现在日志里");
        assert!(safe.contains("user_password=***"));
        // 其余部分必须原样保留，否则日志就没用了
        assert!(safe.contains("wlan_user_ip=10.20.30.40"));
        assert!(safe.contains("user_account=%2C0%2C12345678%40unicom"));
    }

    #[test]
    fn redact_password_in_middle_and_at_end() {
        assert_eq!(
            redact_password("http://x/?a=1&user_password=pw&b=2"),
            "http://x/?a=1&user_password=***&b=2"
        );
        assert_eq!(
            redact_password("http://x/?user_password=pw"),
            "http://x/?user_password=***"
        );
    }

    #[test]
    fn redact_no_password_is_noop() {
        assert_eq!(redact_password("http://x/?a=1"), "http://x/?a=1");
    }

    #[test]
    fn redact_handles_urlencoded_password() {
        // 密码含特殊字符时会被编码，脱敏按 `&` 边界切，仍应整体抹掉
        let mut c = cfg();
        c.password = "p&w=1".into();
        let url = build_login_url(&c, "10.20.30.40", "dr1005", None);
        let safe = redact_password(&url);
        assert!(!safe.contains("p%26w%3D1"));
        assert!(safe.contains("user_password=***&wlan_user_ip="));
    }

    // --- 候选 IP 顺序 ---

    #[test]
    fn fixed_ip_comes_first() {
        let mut c = cfg();
        c.fixed_ip = Some("10.9.9.9".into());
        let got = candidate_ips(&c, Some("10.8.8.8"));
        assert_eq!(got[0], "10.9.9.9");
    }

    #[test]
    fn portal_ip_is_a_candidate() {
        let mut c = cfg();
        c.fixed_ip = None;
        let got = candidate_ips(&c, Some("10.8.8.8"));
        assert!(got.contains(&"10.8.8.8".to_string()));
    }

    #[test]
    fn candidates_are_deduplicated() {
        let mut c = cfg();
        c.fixed_ip = Some("10.8.8.8".into());
        let got = candidate_ips(&c, Some("10.8.8.8"));
        assert_eq!(got.iter().filter(|x| *x == "10.8.8.8").count(), 1);
    }

    #[test]
    fn blank_portal_ip_ignored() {
        let mut c = cfg();
        c.fixed_ip = None;
        let got = candidate_ips(&c, Some("   "));
        assert!(!got.iter().any(|x| x.trim().is_empty()));
    }

    #[test]
    fn candidates_never_empty_when_fixed_ip_set() {
        // 即使门户地址无法解析、枚举也拿不到地址，fixed_ip 也必须在
        let mut c = cfg();
        c.auth_url = "http://this-host-does-not-exist.invalid/eportal/portal/login".into();
        c.fixed_ip = Some("10.1.2.3".into());
        let got = candidate_ips(&c, None);
        assert!(got.contains(&"10.1.2.3".to_string()));
    }
}

//! 网络连通性检测与 captive portal 识别。
//!
//! 上游这里自己手写了一份 TcpStream HTTP GET，本仓库已把它抽成 [`crate::http`]，
//! 本模块直接复用 —— 顺带把「状态码 + Location 头」的解析从字符串切分换成了结构化字段。

use std::time::Duration;

use crate::http::{self, Error, Scheme};
use crate::log_warn;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetStatus {
    /// 直连成功
    Connected,
    /// 被劫持到认证门户。`redirect` 是重定向 URL；被 200 拦截时可能为空串
    /// （那时拿不到 `wlanuserip`，认证会退回自行探测 IP）。
    CaptivePortal { redirect: String },
    /// DNS 暂不可用（认证刚成功后常见，DHCP/DNS 未生效）；非硬失败
    DnsPending,
    /// 无法联网
    Disconnected { reason: String },
}

/// 从 captive portal 重定向 URL 提取 AC 参数（wlanacip / wlanacname / wlanuserip）。
pub fn extract_ac_params(redirect: &str) -> (String, String, String) {
    let get = |key: &str| -> String {
        redirect
            .split('?')
            .nth(1)
            .unwrap_or("")
            .split('&')
            .find_map(|kv| {
                let (k, v) = kv.split_once('=')?;
                (k.eq_ignore_ascii_case(key)).then(|| v.to_string())
            })
            .unwrap_or_default()
    };
    (get("wlanacip"), get("wlanacname"), get("wlanuserip"))
}

/// 响应体里是否带门户特征。有些学校不重定向，直接 200 返回一个登录页，
/// 此时没有 Location 可看，只能靠正文认出来。
fn looks_like_portal(body: &str) -> bool {
    const MARKERS: [&str; 4] = ["wlanuserip=", "srun_portal", "eportal", "ac_id="];
    MARKERS.iter().any(|m| body.contains(m))
}

/// 检测网络状态。
///
/// 语义与上游一致：**DNS 失败单独成一类**（`DnsPending`），因为刚认证成功、
/// DHCP 还没推 DNS 的时候它会出现，把它当断网会导致立刻重复认证。
pub fn check(url: &str, timeout: Duration) -> NetStatus {
    match http::get(url, timeout) {
        Ok(resp) => {
            // 3xx 带 Location = 被重定向到门户
            if (300..400).contains(&resp.status) {
                if let Some(loc) = resp.header("location") {
                    return NetStatus::CaptivePortal {
                        redirect: loc.to_string(),
                    };
                }
            }
            // 200/204 但正文是登录页 = 被 200 拦截
            if resp.status == 200 && looks_like_portal(&resp.body) {
                return NetStatus::CaptivePortal {
                    redirect: String::new(),
                };
            }
            if resp.status == 0 {
                return NetStatus::Disconnected {
                    reason: "无有效 HTTP 响应".into(),
                };
            }
            NetStatus::Connected
        }
        Err(Error::Dns(e)) => {
            log_warn!("DNS 解析 {url} 失败: {e}（可能认证刚生效，DNS 暂未就绪）");
            NetStatus::DnsPending
        }
        Err(e) => NetStatus::Disconnected {
            reason: e.to_string(),
        },
    }
}

/// 检测地址是否可用。供 `validate` 子命令提前发现「填了 https 但没编 TLS」这类配置错误。
pub fn check_url_supported(url: &str) -> Result<(), String> {
    match http::parse_url(url) {
        None => Err(format!("无法解析: {url}")),
        Some((Scheme::Https, ..)) => {
            #[cfg(feature = "tls")]
            {
                Ok(())
            }
            #[cfg(not(feature = "tls"))]
            {
                Err(format!("{url} 是 https，但当前二进制未编译 TLS 支持"))
            }
        }
        Some((Scheme::Http, ..)) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_ac_params_full() {
        let (ip, name, user) = extract_ac_params(
            "http://10.0.0.1/a79.htm?wlanuserip=10.20.30.42&wlanacname=&wlanacip=10.0.0.2",
        );
        assert_eq!(ip, "10.0.0.2");
        assert_eq!(name, "");
        assert_eq!(user, "10.20.30.42");
    }

    #[test]
    fn extract_ac_params_missing_and_case() {
        let (ip, name, user) = extract_ac_params("http://x/a.htm?WLANACIP=1.2.3.4&WlanAcName=AC1");
        assert_eq!(ip, "1.2.3.4");
        assert_eq!(name, "AC1");
        assert_eq!(user, "");
    }

    #[test]
    fn extract_ac_params_no_query() {
        let (ip, name, user) = extract_ac_params("http://x/a.htm");
        assert_eq!((ip.as_str(), name.as_str(), user.as_str()), ("", "", ""));
    }

    #[test]
    fn portal_markers() {
        assert!(looks_like_portal("<script>location.href='a79.htm?wlanuserip=1.2.3.4'</script>"));
        assert!(looks_like_portal("<form action='/srun_portal_pc.php'>"));
        assert!(!looks_like_portal("<html><body>hello</body></html>"));
    }

    #[test]
    fn dns_pending_is_not_disconnected() {
        let s = NetStatus::DnsPending;
        assert!(matches!(s, NetStatus::DnsPending));
        assert_ne!(
            s,
            NetStatus::Disconnected {
                reason: "x".into()
            }
        );
    }

    #[test]
    fn unreachable_host_is_disconnected_not_dns_pending() {
        // 保留地址段，能解析但连不上 → 应是 Disconnected
        let s = check("http://192.0.2.1/", Duration::from_millis(300));
        assert!(
            matches!(s, NetStatus::Disconnected { .. }),
            "期望 Disconnected，实得 {s:?}"
        );
    }

    #[test]
    fn check_url_supported_accepts_http() {
        assert!(check_url_supported("http://www.baidu.com").is_ok());
        assert!(check_url_supported("not a url").is_err());
    }

    #[test]
    fn check_url_supported_https_depends_on_feature() {
        let r = check_url_supported("https://example.com");
        #[cfg(feature = "tls")]
        assert!(r.is_ok());
        #[cfg(not(feature = "tls"))]
        assert!(r.is_err(), "未开 tls feature 时应提前报错而不是运行期才发现");
    }
}

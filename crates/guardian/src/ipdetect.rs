//! 本机 IPv4 探测（Linux）。
//!
//! # 为什么和上游 Windows 版逻辑完全不同
//!
//! 上游在 Windows 上跑，那台 PC **本身就是**校园网客户端，所以「枚举本机网卡 → 按 10.x
//! 优先打分 → 逐个拿去认证」是合理的。
//!
//! 在 OpenWrt 软路由上这个前提不成立：路由器有 `br-lan`、docker 网桥、可能还有 Tailscale，
//! 「本机 IP」是一堆东西，照搬会把 `192.168.1.1` 这类内网地址也拿去门户认证。
//!
//! 路由器上唯一正确的语义是：**内核选出来用于访问门户的那个源 IP**。
//! [`source_ip_for`] 用一个 UDP socket `connect()` 就能问出来 —— `connect()` 对 UDP
//! 只做路由查找、不发任何包，所以即使门户不可达（那正是需要认证的时刻）也照样能用。
//!
//! 这样还顺带绕开了查 WAN 设备名、解析 UCI 网络配置、读 netlink 一堆麻烦事。
//! [`list_adapters`] 只作为兜底和 `detect-ip` 诊断命令使用。

use std::net::{ToSocketAddrs, UdpSocket};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterIp {
    pub name: String,
    pub ip: String,
    pub mac: Option<String>,
}

/// 询问内核：要访问 `host:port`，源地址会是哪个？
///
/// 这是本模块的核心。返回的就是门户将要看到的 `wlan_user_ip`。
pub fn source_ip_for(host: &str, port: u16) -> Option<String> {
    let addrs: Vec<_> = (host, port).to_socket_addrs().ok()?.collect();

    for sa in addrs {
        let bind_addr = if sa.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" };
        let Ok(sock) = UdpSocket::bind(bind_addr) else {
            continue;
        };
        // UDP connect 不发包，只做一次路由查找并锁定对端
        if sock.connect(sa).is_err() {
            continue;
        }
        let Ok(local) = sock.local_addr() else {
            continue;
        };
        let ip = local.ip();
        if ip.is_loopback() || ip.is_unspecified() {
            continue;
        }
        return Some(ip.to_string());
    }

    None
}

/// 这个 IPv4 是否适合用来认证。
///
/// 排除回环、链路本地（169.254，DHCP 失败时的 APIPA）、以及 CGNAT 段（100.64/10，
/// Tailscale 等 VPN 常用，门户不可能认）。
pub fn is_usable_ipv4(ip: &str) -> bool {
    let octets: Vec<u32> = ip.split('.').filter_map(|o| o.parse().ok()).collect();
    if octets.len() != 4 {
        return false;
    }
    let (a, b) = (octets[0], octets[1]);
    if a == 127 {
        return false; // 回环
    }
    if a == 169 && b == 254 {
        return false; // 链路本地 / APIPA
    }
    if a == 100 && (64..=127).contains(&b) {
        return false; // CGNAT（Tailscale 等）
    }
    if a == 0 {
        return false;
    }
    true
}

/// 打分：校园网段优先。仅在 [`source_ip_for`] 失败时用于兜底挑选。
pub fn ip_score(ip: &str) -> u8 {
    let octets: Vec<u32> = ip.split('.').filter_map(|o| o.parse().ok()).collect();
    if octets.len() != 4 {
        return 0;
    }
    let (a, b, c) = (octets[0], octets[1], octets[2]);
    if a == 10 {
        5 // 校园网/企业内网（ePortal 的目标网段）
    } else if a == 192 && b == 168 && c != 1 {
        4 // 宿主网段（非 .1 网关自身）
    } else if a == 192 && b == 168 {
        3 // 192.168.x.1 类（很可能是网关/虚拟网卡）
    } else if a == 172 && (16..=31).contains(&b) {
        2 // docker / Hyper-V 常见段
    } else {
        1
    }
}

/// 兜底路径：枚举网卡挑一个最像样的地址。
///
/// `wan_iface` 非空时优先取该接口的地址（这是路由器上最可靠的兜底）。
pub fn detect_local_ip(wan_iface: Option<&str>) -> Option<String> {
    let adapters = list_adapters();

    if let Some(name) = wan_iface {
        if let Some(a) = adapters
            .iter()
            .find(|a| a.name == name && is_usable_ipv4(&a.ip))
        {
            return Some(a.ip.clone());
        }
    }

    adapters
        .into_iter()
        .filter(|a| is_usable_ipv4(&a.ip))
        .max_by_key(|a| ip_score(&a.ip))
        .map(|a| a.ip)
}

// ---------------------------------------------------------------------------
// getifaddrs
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
pub fn list_adapters() -> Vec<AdapterIp> {
    use std::collections::HashMap;

    let mut macs: HashMap<String, String> = HashMap::new();
    let mut out: Vec<AdapterIp> = Vec::new();

    unsafe {
        let mut ifap: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut ifap) != 0 {
            return out;
        }

        // 第一遍：收集 MAC（AF_PACKET 条目与 AF_INET 条目共用 ifa_name）
        let mut cur = ifap;
        while !cur.is_null() {
            let a = &*cur;
            if !a.ifa_addr.is_null() && (*a.ifa_addr).sa_family as i32 == libc::AF_PACKET {
                if let Some(mac) = sockaddr_to_mac(a.ifa_addr) {
                    macs.insert(cstr_to_string(a.ifa_name), mac);
                }
            }
            cur = a.ifa_next;
        }

        // 第二遍：收集 IPv4
        let mut cur = ifap;
        while !cur.is_null() {
            let a = &*cur;
            if !a.ifa_addr.is_null() && (*a.ifa_addr).sa_family as i32 == libc::AF_INET {
                if let Some(ip) = sockaddr_to_ipv4(a.ifa_addr) {
                    let name = cstr_to_string(a.ifa_name);
                    let mac = macs.get(&name).cloned();
                    out.push(AdapterIp { name, ip, mac });
                }
            }
            cur = a.ifa_next;
        }

        libc::freeifaddrs(ifap);
    }

    out
}

#[cfg(not(target_os = "linux"))]
pub fn list_adapters() -> Vec<AdapterIp> {
    Vec::new()
}

#[cfg(target_os = "linux")]
unsafe fn sockaddr_to_ipv4(sa: *const libc::sockaddr) -> Option<String> {
    let sin = sa as *const libc::sockaddr_in;
    if sin.is_null() {
        return None;
    }
    // s_addr 是网络字节序；from_be 后按大端解释即为点分四段的数值
    let raw = u32::from_be((*sin).sin_addr.s_addr);
    Some(std::net::Ipv4Addr::from(raw).to_string())
}

#[cfg(target_os = "linux")]
unsafe fn sockaddr_to_mac(sa: *const libc::sockaddr) -> Option<String> {
    let sll = sa as *const libc::sockaddr_ll;
    if sll.is_null() {
        return None;
    }
    let len = (*sll).sll_halen as usize;
    if len == 0 || len > 8 {
        return None;
    }
    let addr = &(*sll).sll_addr;
    Some(
        (0..len)
            .map(|i| format!("{:02X}", addr[i]))
            .collect::<Vec<_>>()
            .join(":"),
    )
}

#[cfg(target_os = "linux")]
unsafe fn cstr_to_string(p: *const libc::c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_loopback_and_link_local() {
        assert!(!is_usable_ipv4("127.0.0.1"));
        assert!(!is_usable_ipv4("169.254.1.1"));
        assert!(!is_usable_ipv4("100.64.0.1")); // Tailscale / CGNAT
        assert!(!is_usable_ipv4("100.123.202.1"));
        assert!(!is_usable_ipv4("0.0.0.0"));
        assert!(!is_usable_ipv4("not-an-ip"));
        assert!(is_usable_ipv4("10.20.30.41"));
        assert!(is_usable_ipv4("192.168.1.1"));
    }

    #[test]
    fn score_prefers_campus_net() {
        assert!(ip_score("10.20.30.41") > ip_score("192.168.100.100"));
        assert!(ip_score("192.168.100.100") > ip_score("192.168.1.1"));
        assert!(ip_score("192.168.1.1") > ip_score("172.29.144.1"));
        assert!(ip_score("172.29.144.1") > ip_score("100.123.202.1"));
        assert_eq!(ip_score("not-an-ip"), 0);
    }

    #[test]
    fn source_ip_is_routable_and_usable() {
        // 对公网地址做一次路由查找，应当能拿到本机出接口地址。
        // 无外网的环境下可能返回 None，所以只断言「拿到的话必须可用」。
        if let Some(ip) = source_ip_for("223.5.5.5", 53) {
            assert!(is_usable_ipv4(&ip), "拿到不可用的源地址: {ip}");
            assert_ne!(ip, "0.0.0.0");
        }
    }

    #[test]
    fn source_ip_none_for_unresolvable_host() {
        assert_eq!(source_ip_for("this-host-does-not-exist.invalid", 80), None);
    }

    #[test]
    fn list_adapters_does_not_panic() {
        let adapters = list_adapters();
        for a in &adapters {
            assert!(!a.name.is_empty(), "网卡名不应为空");
        }
    }

    #[test]
    fn detect_local_ip_prefers_named_interface() {
        // 不假设具体网卡存在，只要求不 panic 且结果（若有）可用
        let r = detect_local_ip(Some("definitely-not-a-real-iface"));
        if let Some(ip) = r {
            assert!(is_usable_ipv4(&ip));
        }
        let r = detect_local_ip(None);
        if let Some(ip) = r {
            assert!(is_usable_ipv4(&ip));
        }
    }
}

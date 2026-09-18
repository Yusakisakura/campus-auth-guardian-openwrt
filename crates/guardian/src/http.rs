//! 极简 HTTP/1.1 客户端：只为 ePortal 认证和连通性探测服务。
//!
//! 为什么不用 ureq：上游用 ureq 只发了两个 GET，却因此拖进 rustls + ring，
//! 而 ring 含手写汇编与 C 代码、交叉编译到 aarch64-musl 时必须装 C 交叉工具链。
//! 自己实现这几十行之后，**默认构建里一个 C 依赖都没有**，`rustup target add` 即可交叉编译。
//!
//! 只支持 `http://`。需要 https 时开启 `tls` feature（会重新引入 ureq + rustls）。
//!
//! 语义约定：**只要请求完整走完，无论 HTTP 状态码是多少都返回 `Ok`**，
//! 由调用方自己看 `resp.status`。`Err` 只表示传输层失败（DNS / 连接 / IO）。

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

/// 响应体上限。ePortal 返回的是几十字节的 JSONP，64KB 已经非常宽裕。
const MAX_BODY: usize = 64 * 1024;

/// 读取响应头时的上限，防止对端不发 `\r\n\r\n` 导致内存无界增长。
const MAX_HEAD: usize = 32 * 1024;

#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Response {
    /// 按名取响应头，大小写不敏感（部分门户返回小写 `location`）。
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Debug)]
pub enum Error {
    /// URL 无法解析
    BadUrl(String),
    /// 配置了 https 但构建时没开 `tls` feature
    TlsNotBuilt,
    /// 域名解析失败（认证刚生效时常见，DHCP/DNS 还没就绪）
    Dns(String),
    /// TCP 连接失败
    Connect(String),
    /// 收发过程中出错
    Io(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::BadUrl(u) => write!(f, "非法地址: {u}"),
            Error::TlsNotBuilt => write!(
                f,
                "配置了 https，但当前二进制未编译 TLS 支持（需 --features tls 重新构建）"
            ),
            Error::Dns(e) => write!(f, "DNS 解析失败: {e}"),
            Error::Connect(e) => write!(f, "连接失败: {e}"),
            Error::Io(e) => write!(f, "收发失败: {e}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheme {
    Http,
    Https,
}

/// 解析 `scheme://host[:port][/path]`，返回 (scheme, host, port, path)。
///
/// 不做 IPv6 字面量支持 —— 校园网门户都是 IPv4，用不上，加了反而徒增分支。
pub fn parse_url(url: &str) -> Option<(Scheme, String, u16, String)> {
    let (scheme, rest) = if let Some(r) = url.strip_prefix("http://") {
        (Scheme::Http, r)
    } else if let Some(r) = url.strip_prefix("https://") {
        (Scheme::Https, r)
    } else {
        return None;
    };

    let (hostport, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };

    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => {
            let port: u16 = p.parse().ok()?;
            if port == 0 {
                return None; // 端口 0 无意义
            }
            (h, port)
        }
        None => (hostport, if scheme == Scheme::Https { 443 } else { 80 }),
    };

    if host.is_empty() {
        return None;
    }

    Some((scheme, host.to_string(), port, path.to_string()))
}

/// 发一个 GET 并读回响应。
pub fn get(url: &str, timeout: Duration) -> Result<Response, Error> {
    let (scheme, host, port, path) = parse_url(url).ok_or_else(|| Error::BadUrl(url.to_string()))?;

    if scheme == Scheme::Https {
        return get_https(url, timeout);
    }

    let addr = format!("{host}:{port}");

    // 先解析再 connect_timeout：对端不可达时避免 SYN 重试挂 20s+
    let sockaddrs: Vec<_> = addr
        .to_socket_addrs()
        .map_err(|e| Error::Dns(e.to_string()))?
        .collect();
    if sockaddrs.is_empty() {
        return Err(Error::Dns(format!("{addr} 无可用地址")));
    }

    let mut stream = None;
    let mut last_err = None;
    for sa in &sockaddrs {
        match TcpStream::connect_timeout(sa, timeout) {
            Ok(s) => {
                stream = Some(s);
                break;
            }
            Err(e) => last_err = Some(e),
        }
    }
    let mut stream = stream.ok_or_else(|| {
        Error::Connect(format!(
            "{addr}: {}",
            last_err
                .map(|e| e.to_string())
                .unwrap_or_else(|| "无可用地址".into())
        ))
    })?;

    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));

    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: {}\r\nAccept: */*\r\nConnection: close\r\n\r\n",
        user_agent()
    );
    stream
        .write_all(req.as_bytes())
        .map_err(|e| Error::Io(format!("发送请求失败: {e}")))?;

    read_response(&mut stream)
}

/// 把 socket 里剩下的数据读进来，切成响应头和响应体。
///
/// 停止条件（按优先级）：体收满 `Content-Length` / 体超 [`MAX_BODY`] / 对端关闭 /
/// 头部读完后读超时。最后一条是必需的：我们发的是 `Connection: close`，但对端未必遵守，
/// 那时若死等关闭就会白白耗掉一整个超时，把一次成功的探测误判成断网。
fn read_response(stream: &mut TcpStream) -> Result<Response, Error> {
    let mut buf: Vec<u8> = Vec::with_capacity(4096);
    let mut chunk = [0u8; 2048];
    let mut head_end: Option<usize> = None;
    let mut content_length: Option<usize> = None;

    loop {
        // 头已读完，体要么收满 Content-Length，要么到上限，都该收手了
        if let Some(p) = head_end {
            let got = buf.len() - p;
            if got >= MAX_BODY || content_length.is_some_and(|cl| got >= cl) {
                break;
            }
        }

        match stream.read(&mut chunk) {
            Ok(0) => break, // 对端关闭
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if head_end.is_none() {
                    if let Some(p) = find_head_end(&buf) {
                        head_end = Some(p);
                        content_length = parse_content_length(&buf[..p]);
                    } else if buf.len() >= MAX_HEAD {
                        break; // 对端迟迟不发 \r\n\r\n，放弃
                    }
                }
            }
            Err(e)
                if head_end.is_some()
                    && matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
            {
                break // 头已经拿到了，体收多少算多少
            }
            Err(e) => return Err(Error::Io(format!("读取响应失败: {e}"))),
        }
    }

    let head_end = head_end.ok_or_else(|| Error::Io("响应头不完整".into()))?;
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let body = String::from_utf8_lossy(&buf[head_end..]).into_owned();

    let mut lines = head.lines();
    let status_line = lines.next().unwrap_or("");
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .ok_or_else(|| Error::Io(format!("无法解析状态行: {status_line}")))?;

    let headers = lines
        .filter_map(|l| {
            let (k, v) = l.split_once(':')?;
            Some((
                k.trim().to_string(),
                v.trim().trim_end_matches('\r').to_string(),
            ))
        })
        .collect();

    Ok(Response {
        status,
        headers,
        body,
    })
}

/// 找 `\r\n\r\n`，返回**响应体起始**下标。
fn find_head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

/// 从响应头里取 `Content-Length`。`Transfer-Encoding: chunked` 没有这个头，返回 None，
/// 此时只能读到对端关闭为止（有读超时兜底）。
fn parse_content_length(head: &[u8]) -> Option<usize> {
    String::from_utf8_lossy(head).lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim()
            .eq_ignore_ascii_case("content-length")
            .then(|| v.trim().parse().ok())?
    })
}

fn user_agent() -> String {
    format!("CampusAuthGuardian/{}", env!("CARGO_PKG_VERSION"))
}

#[cfg(feature = "tls")]
fn get_https(url: &str, timeout: Duration) -> Result<Response, Error> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(timeout)
        .timeout(timeout)
        .build();
    match agent.get(url).call() {
        Ok(resp) => {
            let status = resp.status();
            let headers = resp
                .headers_names()
                .into_iter()
                .filter_map(|n| resp.header(&n).map(|v| (n.clone(), v.to_string())))
                .collect();
            let body = resp.into_string().unwrap_or_default();
            Ok(Response {
                status,
                headers,
                body,
            })
        }
        Err(ureq::Error::Status(code, resp)) => {
            let headers = resp
                .headers_names()
                .into_iter()
                .filter_map(|n| resp.header(&n).map(|v| (n.clone(), v.to_string())))
                .collect();
            let body = resp.into_string().unwrap_or_default();
            Ok(Response {
                status: code,
                headers,
                body,
            })
        }
        Err(e) => Err(Error::Io(e.to_string())),
    }
}

#[cfg(not(feature = "tls"))]
fn get_https(_url: &str, _timeout: Duration) -> Result<Response, Error> {
    Err(Error::TlsNotBuilt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_parse_basic() {
        assert_eq!(
            parse_url("http://www.baidu.com"),
            Some((Scheme::Http, "www.baidu.com".into(), 80, "/".into()))
        );
        assert_eq!(
            parse_url("http://10.0.0.1:801/a79.htm?x=1"),
            Some((
                Scheme::Http,
                "10.0.0.1".into(),
                801,
                "/a79.htm?x=1".into()
            ))
        );
    }

    #[test]
    fn url_parse_https_defaults_to_443() {
        assert_eq!(
            parse_url("https://example.com/x"),
            Some((Scheme::Https, "example.com".into(), 443, "/x".into()))
        );
    }

    #[test]
    fn url_parse_rejects_non_http_scheme() {
        assert_eq!(parse_url("ftp://x"), None);
        assert_eq!(parse_url("10.0.0.1"), None);
    }

    #[test]
    fn url_parse_edge_ports() {
        assert_eq!(
            parse_url("http://h:65535/"),
            Some((Scheme::Http, "h".into(), 65535, "/".into()))
        );
        assert_eq!(parse_url("http://h:0/"), None, "端口 0 无效");
        assert_eq!(parse_url("http://h:abc/"), None, "非数字端口");
        assert_eq!(parse_url("http:///x"), None, "空主机");
    }

    #[test]
    fn url_parse_keeps_query_in_path() {
        assert_eq!(
            parse_url("http://h/a?b=c&d=e"),
            Some((Scheme::Http, "h".into(), 80, "/a?b=c&d=e".into()))
        );
    }

    #[test]
    fn head_end_finds_body_start() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhi";
        assert_eq!(find_head_end(raw), Some(38));
        assert_eq!(find_head_end(b"HTTP/1.1 200 OK\r\n"), None);
    }

    #[test]
    fn header_lookup_is_case_insensitive() {
        let r = Response {
            status: 302,
            headers: vec![("location".into(), "http://x/".into())],
            body: String::new(),
        };
        assert_eq!(r.header("Location"), Some("http://x/"));
        assert_eq!(r.header("LOCATION"), Some("http://x/"));
        assert_eq!(r.header("X-Nope"), None);
    }

    #[test]
    fn content_length_parsing() {
        assert_eq!(
            parse_content_length(b"HTTP/1.1 200 OK\r\nContent-Length: 42\r\n"),
            Some(42)
        );
        assert_eq!(
            parse_content_length(b"HTTP/1.1 200 OK\r\ncontent-length:  7 \r\n"),
            Some(7)
        );
        assert_eq!(
            parse_content_length(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n"),
            None
        );
        assert_eq!(parse_content_length(b"HTTP/1.1 200 OK\r\n"), None);
    }

    /// 起一个只会应答一次的假服务器，返回端口。`reply` 写完后连接**保持不关**。
    fn spawn_sticky_server(reply: &'static [u8]) -> u16 {
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut s, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = s.read(&mut buf); // 读掉请求行
                let _ = s.write_all(reply);
                let _ = s.flush();
                std::thread::sleep(Duration::from_secs(5)); // 故意不关连接
            }
        });
        port
    }

    #[test]
    fn stops_at_content_length_without_waiting_for_close() {
        // 回归：早期实现只在对端关闭时收手，遇到不遵守 Connection: close 的对端
        // 会白等一整个超时，把成功的探测误判成断网。
        let port = spawn_sticky_server(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhi");
        let start = std::time::Instant::now();
        let resp = get(&format!("http://127.0.0.1:{port}/"), Duration::from_secs(3)).unwrap();
        let elapsed = start.elapsed();

        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, "hi");
        assert!(elapsed < Duration::from_secs(1), "不该等满超时: {elapsed:?}");
    }

    #[test]
    fn incomplete_body_after_timeout_is_still_ok() {
        // 长度未知（chunked）且对端不关连接：头已拿到就算成功，体收多少算多少
        let port = spawn_sticky_server(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nhi\r\n",
        );
        let start = std::time::Instant::now();
        let resp = get(
            &format!("http://127.0.0.1:{port}/"),
            Duration::from_millis(300),
        )
        .unwrap();

        assert_eq!(resp.status, 200);
        assert!(resp.body.contains("hi"));
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn https_without_tls_feature_is_a_clear_error() {
        // 默认构建下 https 应给出可操作的错误，而不是静默失败
        #[cfg(not(feature = "tls"))]
        assert!(matches!(
            get("https://example.com", Duration::from_secs(1)),
            Err(Error::TlsNotBuilt)
        ));
    }
}

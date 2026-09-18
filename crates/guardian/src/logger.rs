//! 日志：格式化成一行带时间戳的文本，写 stderr，由 procd 转交 logd。
//!
//! # 相对上游删掉了什么
//!
//! 上游会自己写 `campus_auth.log` 并做 512KB 轮转，另外维护一个内存环形缓冲供 C# UI
//! 读取。这两样在路由器上都是多余的：
//!
//! - **文件轮转**：OpenWrt 的日志统一归 logd 管，自己再写一份既费 flash 又和 logd
//!   的轮转策略打架。procd 会把守护进程的 stderr 转进 syslog，用户用
//!   `logread -e campus-auth-guardian` 查看。
//! - **内存环形缓冲**：它只能被同一个进程读到。唯一的消费者 LuCI 跑在别的进程里，
//!   读的是 logd 的缓冲，读不到我们的。
//!
//! 于是本模块只剩「格式化 + 写 stderr」。

use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
    Error,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Info => "INFO",
            Level::Warn => "WARN",
            Level::Error => "ERROR",
        }
    }
}

/// 记录一条日志。
///
/// 刻意不用 `eprintln!` —— 它在写失败时会 panic。守护进程的 stderr 是通往 logd 的管道，
/// logd 一旦重启这条管道就可能断，那时因为一句日志把整个守护进程带崩是不划算的。
pub fn log(level: Level, text: impl AsRef<str>) {
    let line = format_line(now_secs(), level, text.as_ref());
    let _ = writeln!(std::io::stderr(), "{line}");
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 格式化为 `[2026-09-01 08:00:00] [INFO] 正文`。
fn format_line(unix: u64, level: Level, text: &str) -> String {
    let (y, mo, d, h, mi, s) = format_cn_local(unix);
    format!(
        "[{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}] [{}] {text}",
        level.as_str()
    )
}

/// Unix 秒 → 东八区 (y,mo,d,h,mi,s)。纯整数算法，无外部依赖。
///
/// 固定 UTC+8：目标场景是国内校园网，路由器时区通常也是 CST。为此引入 tz 依赖不值得。
fn format_cn_local(unix: u64) -> (u64, u64, u64, u64, u64, u64) {
    let total = unix + 8 * 3600;
    let days = total / 86400;
    let rem = total % 86400;
    let (h, mi, s) = (rem / 3600, rem % 3600 / 60, rem % 60);
    // Howard Hinnant civil_from_days
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as u64, m as u64, d as u64, h, mi, s)
}

/// 便捷宏：info 级别。
#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => { $crate::logger::log($crate::logger::Level::Info, format!($($arg)*)) };
}

/// 便捷宏：warn 级别。
#[macro_export]
macro_rules! log_warn {
    ($($arg:tt)*) => { $crate::logger::log($crate::logger::Level::Warn, format!($($arg)*)) };
}

/// 便捷宏：error 级别。
#[macro_export]
macro_rules! log_error {
    ($($arg:tt)*) => { $crate::logger::log($crate::logger::Level::Error, format!($($arg)*)) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cn_local_time_conversion() {
        // 2026-09-01 00:00:00 UTC = 08:00 CST
        let (y, mo, d, h, mi, s) = format_cn_local(1788220800);
        assert_eq!((y, mo, d, h, mi, s), (2026, 9, 1, 8, 0, 0));
    }

    #[test]
    fn cn_local_handles_epoch_and_leap_day() {
        // 1970-01-01 00:00:00 UTC = 1970-01-01 08:00 CST
        assert_eq!(format_cn_local(0), (1970, 1, 1, 8, 0, 0));
        // 2024-02-29 00:00:00 UTC = 2024-02-29 08:00 CST（闰日）
        assert_eq!(format_cn_local(1709164800), (2024, 2, 29, 8, 0, 0));
    }

    #[test]
    fn log_format() {
        assert_eq!(
            format_line(1788220800, Level::Info, "hello"),
            "[2026-09-01 08:00:00] [INFO] hello"
        );
        assert_eq!(
            format_line(1788220800, Level::Error, "boom"),
            "[2026-09-01 08:00:00] [ERROR] boom"
        );
    }

    #[test]
    fn level_strings_are_stable() {
        assert_eq!(Level::Info.as_str(), "INFO");
        assert_eq!(Level::Warn.as_str(), "WARN");
        assert_eq!(Level::Error.as_str(), "ERROR");
    }

    #[test]
    fn logging_does_not_panic() {
        log(Level::Info, "测试");
        log_info!("格式化 {} {}", 1, "abc");
        log_warn!("warn");
        log_error!("error");
    }
}

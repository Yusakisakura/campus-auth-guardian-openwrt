//! 校园网 ePortal 认证守护内核（OpenWrt 版）。
//!
//! 本 crate 同时产出 lib 与 bin：`lib` 供单测和将来的复用，`bin` 是路由器上跑的守护进程。
//!
//! 与上游 Windows 版的差异：
//! - `ipdetect` 改为 Linux 实现，且语义从「枚举本机网卡打分」改为「取通往门户的源 IP」
//! - `config` 改为 UCI
//! - `http` 取代 ureq（默认构建不含任何 C 依赖）
//! - 去掉了 `ffi`（C ABI 层，只服务于已移除的 C# 壳）

pub mod auth;
pub mod config;
pub mod guardian;
pub mod http;
pub mod ipdetect;
pub mod logger;
pub mod netcheck;

pub use config::{Config, Operator};
pub use guardian::{Guardian, GuardianState};

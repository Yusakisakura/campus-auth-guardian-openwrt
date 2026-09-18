//! 守护循环：周期检测网络，断网时指数退避重试认证。
//!
//! # 相对上游的结构改动
//!
//! 上游把事件通过 `crossbeam_channel` 推给 C# 壳。C# 壳已经没了，唯一的消费者是
//! LuCI —— 而 LuCI 跑在另一个进程里，没法接收 channel。所以改成**状态快照 + 文件**：
//! 循环每轮把当前状态写进 `status.json`，rpcd 后端直接读文件。channel 依赖因此去掉。
//!
//! 外部控制走标准 Unix 信号（由 procd / rpcd 发）：
//!
//! | 信号 | 含义 |
//! |---|---|
//! | `SIGHUP` | 重新读取 UCI 配置 |
//! | `SIGUSR1` | 立刻认证一次（不看退避计时） |
//! | `SIGTERM` / `SIGINT` | 退出 |

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::auth::{self, AuthOutcome};
use crate::config::Config;
use crate::netcheck::{self, NetStatus};
use crate::{log_error, log_info, log_warn};

/// 状态文件所在目录。`/var/run` 是 tmpfs，重启即清空，正合适。
pub const RUN_DIR: &str = "/var/run/campus-auth-guardian";
/// 状态文件路径。内容不含凭据（只有状态、IP、门户消息），故 0644 可读。
pub const STATUS_FILE: &str = "/var/run/campus-auth-guardian/status.json";

/// 单次连通性检测的超时。
const CHECK_TIMEOUT: Duration = Duration::from_secs(8);

/// 守护循环运行状态（对 UI 展示）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardianState {
    Stopped,
    Monitoring,
    Authenticating,
}

impl GuardianState {
    pub fn as_str(self) -> &'static str {
        match self {
            GuardianState::Stopped => "stopped",
            GuardianState::Monitoring => "monitoring",
            GuardianState::Authenticating => "authenticating",
        }
    }
}

/// 循环产生的运行时快照。与配置一起构成 `status.json`。
#[derive(Default)]
struct Snapshot {
    net: Option<NetStatus>,
    last_auth: Option<AuthOutcome>,
    last_auth_ts: Option<u64>,
    last_net_ts: Option<u64>,
    consecutive_failures: u32,
    next_check_secs: u64,
}

struct Inner {
    /// 守护是否生效。跟随配置里的 `enabled`。非信号驱动，无需 Arc。
    running: AtomicBool,
    /// 以下三个由信号处理函数写入，因此必须是 `Arc<AtomicBool>`
    /// （`signal_hook::flag::register` 的签名要求）。
    manual_kick: Arc<AtomicBool>,
    reload: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
    state: Mutex<GuardianState>,
    cfg: Mutex<Config>,
    cfg_path: Mutex<PathBuf>,
    snapshot: Mutex<Snapshot>,
}

impl Inner {
    fn cfg(&self) -> Config {
        self.cfg.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn set_state(&self, s: GuardianState) {
        *self.state.lock().unwrap_or_else(|e| e.into_inner()) = s;
    }

    fn snapshot(&self) -> Snapshot {
        let s = self.snapshot.lock().unwrap_or_else(|e| e.into_inner());
        Snapshot {
            net: s.net.clone(),
            last_auth: s.last_auth.clone(),
            last_auth_ts: s.last_auth_ts,
            last_net_ts: s.last_net_ts,
            consecutive_failures: s.consecutive_failures,
            next_check_secs: s.next_check_secs,
        }
    }

    /// 组装 `status.json` 的内容。
    fn status_json(&self) -> Value {
        let cfg = self.cfg();
        let snap = self.snapshot();
        let state = *self.state.lock().unwrap_or_else(|e| e.into_inner());

        json!({
            "version": env!("CARGO_PKG_VERSION"),
            "state": state.as_str(),
            "enabled": self.running.load(Ordering::SeqCst),
            "net": snap.net.as_ref().map(net_status_json),
            "last_auth": snap.last_auth.as_ref().map(auth_outcome_json),
            "last_auth_ts": snap.last_auth_ts,
            "last_net_ts": snap.last_net_ts,
            "consecutive_failures": snap.consecutive_failures,
            "next_check_secs": snap.next_check_secs,
            "config_problems": cfg.validate(),
        })
    }

    fn write_status(&self) {
        let path = Path::new(STATUS_FILE);
        let body = self.status_json().to_string();
        if let Err(e) = write_atomic(path, &body) {
            log_warn!("写状态文件失败: {e}");
        }
    }
}

fn net_status_json(s: &NetStatus) -> Value {
    match s {
        NetStatus::Connected => json!({"kind": "connected"}),
        NetStatus::CaptivePortal { redirect } => {
            json!({"kind": "captive_portal", "redirect": redirect})
        }
        NetStatus::DnsPending => json!({"kind": "dns_pending"}),
        NetStatus::Disconnected { reason } => {
            json!({"kind": "disconnected", "reason": reason})
        }
    }
}

fn auth_outcome_json(o: &AuthOutcome) -> Value {
    match o {
        AuthOutcome::Success => json!({"kind": "success", "msg": ""}),
        AuthOutcome::AlreadyOnline => json!({"kind": "already_online", "msg": ""}),
        AuthOutcome::Failed { msg } => json!({"kind": "failed", "msg": msg}),
        AuthOutcome::NetworkError { msg } => json!({"kind": "network_error", "msg": msg}),
    }
}

/// 先写临时文件再 rename，避免 rpcd 读到写了一半的 JSON。
fn write_atomic(path: &Path, data: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(data.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Guardian
// ---------------------------------------------------------------------------

pub struct Guardian {
    inner: Arc<Inner>,
    handle: Mutex<Option<JoinHandle<()>>>,
}

impl Guardian {
    /// 创建守护器并启动工作线程。
    pub fn start(cfg: Config, cfg_path: PathBuf) -> std::io::Result<Self> {
        let inner = Arc::new(Inner {
            running: AtomicBool::new(cfg.enabled),
            manual_kick: Arc::new(AtomicBool::new(false)),
            reload: Arc::new(AtomicBool::new(false)),
            shutdown: Arc::new(AtomicBool::new(false)),
            state: Mutex::new(GuardianState::Monitoring),
            cfg: Mutex::new(cfg),
            cfg_path: Mutex::new(cfg_path),
            snapshot: Mutex::new(Snapshot::default()),
        });

        register_signals(&inner)?;

        let handle = {
            let inner = Arc::clone(&inner);
            std::thread::Builder::new()
                .name("guardian-loop".into())
                .spawn(move || run_loop(inner))?
        };

        Ok(Self {
            inner,
            handle: Mutex::new(Some(handle)),
        })
    }

    /// 立刻认证一次（等价于 `kill -USR1`）。
    pub fn kick(&self) {
        self.inner.manual_kick.store(true, Ordering::SeqCst);
    }

    /// 请求重新读取配置（等价于 `kill -HUP`）。实际重载发生在循环下一轮。
    pub fn request_reload(&self) {
        self.inner.reload.store(true, Ordering::SeqCst);
    }

    pub fn is_running(&self) -> bool {
        self.inner.running.load(Ordering::SeqCst)
    }

    pub fn state(&self) -> GuardianState {
        *self.inner.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn config(&self) -> Config {
        self.inner.cfg()
    }

    /// 当前状态快照（与写进 `status.json` 的内容一致）。
    pub fn status_json(&self) -> Value {
        self.inner.status_json()
    }

    /// 阻塞直到循环退出（收到 `SIGTERM` / `SIGINT`）。
    pub fn wait(&self) {
        if let Some(h) = self.handle.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = h.join();
        }
    }

    /// 请求退出并等待线程结束。
    pub fn stop(&self) {
        self.inner.shutdown.store(true, Ordering::SeqCst);
        self.wait();
    }
}

impl Drop for Guardian {
    fn drop(&mut self) {
        self.stop();
    }
}

fn register_signals(inner: &Arc<Inner>) -> std::io::Result<()> {
    use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM, SIGUSR1};
    use signal_hook::flag;

    // 用旗标而不是回调：回调里做重活（读文件、认证）会违反信号安全约束
    flag::register(SIGHUP, Arc::clone(&inner.reload))?;
    flag::register(SIGUSR1, Arc::clone(&inner.manual_kick))?;
    flag::register(SIGTERM, Arc::clone(&inner.shutdown))?;
    flag::register(SIGINT, Arc::clone(&inner.shutdown))?;
    Ok(())
}

/// 从磁盘重读配置并应用。`enabled` 的变化会立刻生效（不必重启进程）。
fn reload_from_disk(inner: &Arc<Inner>) -> std::io::Result<()> {
    let path = inner
        .cfg_path
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let cfg = Config::load(&path)?;

    let enabled = cfg.enabled;
    *inner.cfg.lock().unwrap_or_else(|e| e.into_inner()) = cfg;
    inner.running.store(enabled, Ordering::SeqCst);

    log_info!(
        "配置已重载（守护{}）",
        if enabled { "开启" } else { "关闭" }
    );
    Ok(())
}

fn run_loop(inner: Arc<Inner>) {
    // 以 500ms 粒度轮询旗标，避免长 sleep 期间无法响应退出 / 手动触发
    let mut next_check = Instant::now();
    let mut failures: u32 = 0;

    while !inner.shutdown.load(Ordering::SeqCst) {
        // --- 重载 ---
        if inner.reload.swap(false, Ordering::SeqCst) {
            if let Err(e) = reload_from_disk(&inner) {
                log_error!("重载配置失败: {e}");
            }
            next_check = Instant::now(); // 配置可能改了检测地址，立刻复检
        }

        // --- 手动触发 ---
        if inner.manual_kick.swap(false, Ordering::SeqCst) {
            log_info!("收到手动认证请求");
            let outcome = do_auth(&inner, None);
            failures = if outcome.is_ok() { 0 } else { failures + 1 };
            {
                let mut s = inner.snapshot.lock().unwrap_or_else(|e| e.into_inner());
                s.last_auth = Some(outcome);
                s.last_auth_ts = Some(now_secs());
                s.consecutive_failures = failures;
                s.next_check_secs = inner.cfg().check_interval.as_secs();
            }
            next_check = Instant::now() + inner.cfg().check_interval;
            inner.write_status();
        }

        // --- 周期检测 ---
        if Instant::now() >= next_check {
            let cfg = inner.cfg();

            if !inner.running.load(Ordering::SeqCst) {
                // 守护关闭：不发认证，但仍检测网络以驱动 UI 状态卡
                let status = netcheck::check(&cfg.check_url, CHECK_TIMEOUT);
                let mut s = inner.snapshot.lock().unwrap_or_else(|e| e.into_inner());
                s.net = Some(status);
                s.last_net_ts = Some(now_secs());
                s.next_check_secs = cfg.check_interval.as_secs();
                drop(s);
                inner.set_state(GuardianState::Stopped);
                inner.write_status();
                next_check = Instant::now() + cfg.check_interval;
                sleep_interruptible(&inner, Duration::from_millis(500));
                continue;
            }

            let status = netcheck::check(&cfg.check_url, CHECK_TIMEOUT);
            let backoff;

            match &status {
                NetStatus::Connected => {
                    failures = 0;
                    backoff = cfg.check_interval;
                }
                NetStatus::DnsPending => {
                    // 认证刚生效时 DNS 可能还没推下来，短延迟快速复检，不计失败
                    log_info!("DNS 暂未就绪，5s 后复检");
                    backoff = Duration::from_secs(5);
                }
                NetStatus::CaptivePortal { redirect } => {
                    log_warn!("检测到 captive portal: {redirect}");
                    let (ac_ip, ac_name, portal_ip) = netcheck::extract_ac_params(redirect);
                    log_info!("AC 参数: ip={ac_ip} name={ac_name} user_ip={portal_ip}");
                    failures = run_retry_burst(
                        &inner,
                        Some((ac_ip.as_str(), ac_name.as_str(), portal_ip.as_str())),
                    );
                    backoff = backoff_delay(&cfg, failures);
                }
                NetStatus::Disconnected { reason } => {
                    log_warn!("网络不可达: {reason}");
                    failures += 1;
                    backoff = backoff_delay(&cfg, failures);
                }
            }

            {
                let mut s = inner.snapshot.lock().unwrap_or_else(|e| e.into_inner());
                s.net = Some(status);
                s.last_net_ts = Some(now_secs());
                s.consecutive_failures = failures;
                s.next_check_secs = backoff.as_secs();
            }
            inner.write_status();
            next_check = Instant::now() + backoff;
        }

        sleep_interruptible(&inner, Duration::from_millis(500));
    }

    inner.set_state(GuardianState::Stopped);
    inner.write_status();
    log_info!("守护进程退出");
}

/// 分片睡眠，好让信号旗标能被及时看到。返回时若已收到退出信号则立即返回。
fn sleep_interruptible(inner: &Arc<Inner>, total: Duration) {
    const SLICE: Duration = Duration::from_millis(100);
    let deadline = Instant::now() + total;
    while Instant::now() < deadline {
        if inner.shutdown.load(Ordering::SeqCst)
            || inner.reload.load(Ordering::SeqCst)
            || inner.manual_kick.load(Ordering::SeqCst)
        {
            return;
        }
        std::thread::sleep(SLICE.min(deadline.saturating_duration_since(Instant::now())));
    }
}

/// 指数退避：`retry_interval * 2^(failures-1)`，封顶 600s。
fn backoff_delay(cfg: &Config, failures: u32) -> Duration {
    if failures == 0 {
        return cfg.check_interval;
    }
    let exp = failures.saturating_sub(1).min(10);
    let secs = cfg
        .retry_interval
        .as_secs()
        .saturating_mul(1u64 << exp)
        .min(600);
    Duration::from_secs(secs)
}

/// 一轮认证爆发：最多 `cfg.max_retries` 次，每次间隔 `retry_interval`。
/// 返回仍剩余的连续失败数（0 = 成功）。
fn run_retry_burst(inner: &Arc<Inner>, ac: Option<(&str, &str, &str)>) -> u32 {
    let cfg = inner.cfg();
    for attempt in 1..=cfg.max_retries {
        if !inner.running.load(Ordering::SeqCst) || inner.shutdown.load(Ordering::SeqCst) {
            return 0;
        }
        let outcome = do_auth(inner, ac);
        if outcome.is_ok() {
            return 0;
        }
        log_warn!("第 {attempt}/{} 次认证失败", cfg.max_retries);
        if attempt < cfg.max_retries {
            sleep_interruptible(inner, cfg.retry_interval);
        }
    }
    log_error!("连续 {} 次认证失败，进入退避等待", cfg.max_retries);
    cfg.max_retries
}

fn do_auth(inner: &Arc<Inner>, ac: Option<(&str, &str, &str)>) -> AuthOutcome {
    let cfg = inner.cfg();
    inner.set_state(GuardianState::Authenticating);
    let outcome = auth::authenticate_with_ac(&cfg, ac);
    inner.set_state(GuardianState::Monitoring);

    match &outcome {
        AuthOutcome::Success => log_info!("认证成功"),
        AuthOutcome::AlreadyOnline => log_info!("已在线，无需认证"),
        AuthOutcome::Failed { msg } => log_error!("认证失败: {msg}"),
        AuthOutcome::NetworkError { msg } => log_error!("认证网络错误: {msg}"),
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Operator;

    #[test]
    fn backoff_grows_and_caps() {
        let mut cfg = Config::default();
        cfg.retry_interval = Duration::from_secs(10);
        assert_eq!(backoff_delay(&cfg, 0), Duration::from_secs(30)); // = check_interval
        assert_eq!(backoff_delay(&cfg, 1), Duration::from_secs(10));
        assert_eq!(backoff_delay(&cfg, 2), Duration::from_secs(20));
        assert_eq!(backoff_delay(&cfg, 3), Duration::from_secs(40));
        assert_eq!(backoff_delay(&cfg, 8), Duration::from_secs(600)); // 封顶
    }

    #[test]
    fn backoff_does_not_overflow() {
        let mut cfg = Config::default();
        cfg.retry_interval = Duration::from_secs(3600);
        // 极大失败数不应 panic（饱和运算 + 指数封顶）
        assert_eq!(backoff_delay(&cfg, u32::MAX), Duration::from_secs(600));
    }

    #[test]
    fn state_strings_are_stable() {
        // LuCI 前端按这些字符串判断，改动即为破坏性变更
        assert_eq!(GuardianState::Stopped.as_str(), "stopped");
        assert_eq!(GuardianState::Monitoring.as_str(), "monitoring");
        assert_eq!(GuardianState::Authenticating.as_str(), "authenticating");
    }

    fn test_inner(cfg: Config) -> Inner {
        Inner {
            running: AtomicBool::new(cfg.enabled),
            manual_kick: Arc::new(AtomicBool::new(false)),
            reload: Arc::new(AtomicBool::new(false)),
            shutdown: Arc::new(AtomicBool::new(false)),
            state: Mutex::new(GuardianState::Monitoring),
            cfg: Mutex::new(cfg),
            cfg_path: Mutex::new(PathBuf::from("/nonexistent")),
            snapshot: Mutex::new(Snapshot::default()),
        }
    }

    #[test]
    fn status_json_shape() {
        let mut cfg = Config::default();
        cfg.auth_url = "http://10.0.0.1:801/eportal/portal/login".into();
        cfg.student_id = "12345678".into();
        cfg.password = "TestPass123".into();
        cfg.operator = Operator::Unicom;
        let inner = test_inner(cfg);

        let v = inner.status_json();
        assert_eq!(v["state"], "monitoring");
        assert_eq!(v["enabled"], true);
        assert_eq!(v["consecutive_failures"], 0);
        assert!(v["net"].is_null());
        assert!(v["last_auth"].is_null());
        assert_eq!(v["config_problems"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn status_json_reports_unconfigured_auth_url() {
        // 装完是空模板，首页应当提示去填认证地址，而不是默默拿默认值去认证
        let mut cfg = Config::default();
        cfg.student_id = "12345678".into();
        cfg.password = "TestPass123".into();
        let problems = test_inner(cfg).status_json()["config_problems"]
            .as_array()
            .unwrap()
            .len();
        assert_eq!(problems, 1, "只差认证地址一项");
    }

    #[test]
    fn status_json_never_contains_password() {
        // 回归：状态文件是给 rpcd / LuCI 读的，绝不能把凭据带出去
        let mut cfg = Config::default();
        cfg.student_id = "12345678".into();
        cfg.password = "SuperSecret123".into();
        let inner = test_inner(cfg);
        let text = inner.status_json().to_string();
        assert!(!text.contains("SuperSecret123"), "状态文件泄漏了密码: {text}");
        assert!(!text.contains("12345678"), "状态文件泄漏了学号: {text}");
    }

    #[test]
    fn status_json_reports_config_problems() {
        // 默认配置没有学号密码 → 应报告出来，供 LuCI 首页提示
        let inner = test_inner(Config::default());
        let problems = inner.status_json()["config_problems"]
            .as_array()
            .unwrap()
            .len();
        assert!(problems >= 2, "默认配置应报出学号/密码缺失");
    }

    #[test]
    fn status_json_serializes_net_and_auth() {
        let inner = test_inner(Config::default());
        {
            let mut s = inner.snapshot.lock().unwrap();
            s.net = Some(NetStatus::CaptivePortal {
                redirect: "http://x/a79.htm?wlanuserip=10.20.30.41".into(),
            });
            s.last_auth = Some(AuthOutcome::Failed {
                msg: "AC认证失败".into(),
            });
            s.last_auth_ts = Some(1788220800);
            s.consecutive_failures = 3;
            s.next_check_secs = 40;
        }
        let v = inner.status_json();
        assert_eq!(v["net"]["kind"], "captive_portal");
        assert!(v["net"]["redirect"].as_str().unwrap().contains("wlanuserip"));
        assert_eq!(v["last_auth"]["kind"], "failed");
        assert_eq!(v["last_auth"]["msg"], "AC认证失败");
        assert_eq!(v["last_auth_ts"], 1788220800);
        assert_eq!(v["consecutive_failures"], 3);
        assert_eq!(v["next_check_secs"], 40);
    }

    #[test]
    fn atomic_write_creates_dirs_and_replaces() {
        let dir = std::env::temp_dir().join(format!("cag-test-{}", std::process::id()));
        let path = dir.join("nested").join("status.json");
        let _ = std::fs::remove_dir_all(&dir);

        write_atomic(&path, r#"{"a":1}"#).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), r#"{"a":1}"#);

        // 覆盖写：不应残留临时文件，也不应读到半截内容
        write_atomic(&path, r#"{"a":2}"#).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), r#"{"a":2}"#);
        assert!(!path.with_extension("json.tmp").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reload_from_disk_missing_file_keeps_defaults() {
        let inner = Arc::new(test_inner(Config::default()));
        // 路径不存在时 Config::load 返回默认配置而非报错
        reload_from_disk(&inner).unwrap();
        assert!(inner.running.load(Ordering::SeqCst));
    }

    #[test]
    fn reload_applies_enabled_flag() {
        let dir = std::env::temp_dir().join(format!("cag-reload-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cfg");
        let write = |v: &str| {
            std::fs::write(
                &path,
                format!("config campus-auth-guardian 'main'\n\toption enabled '{v}'\n"),
            )
            .unwrap()
        };

        write("0");
        let mut cfg = Config::default();
        cfg.enabled = true;
        let mut inner = test_inner(cfg);
        inner.cfg_path = Mutex::new(path.clone());
        let inner = Arc::new(inner);

        assert!(inner.running.load(Ordering::SeqCst));

        // enabled=0 → 守护关闭，但进程继续跑（仍检测网络）
        reload_from_disk(&inner).unwrap();
        assert!(!inner.running.load(Ordering::SeqCst), "enabled=0 应关闭守护");
        assert!(!inner.cfg().enabled);

        // 改回 1 → 重新开启，无需重启进程
        write("1");
        reload_from_disk(&inner).unwrap();
        assert!(inner.running.load(Ordering::SeqCst), "enabled=1 应重新开启");

        let _ = std::fs::remove_dir_all(&dir);
    }
}

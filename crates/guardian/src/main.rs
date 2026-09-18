//! 命令行入口。
//!
//! 不引入 clap：一共六个子命令，手写解析几十行就够，省下的二进制体积对路由器有意义。
//!
//! 退出码（供脚本与 LuCI 后端判断）：
//!
//! | 码 | 含义 |
//! |---|---|
//! | 0 | 成功 / 已在线 / 校验通过 |
//! | 1 | 认证失败，或配置有问题 |
//! | 2 | 用法错误，或网络层失败 |

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use guardian::auth::{self, AuthOutcome};
use guardian::config::Config;
use guardian::guardian::{Guardian, STATUS_FILE};
use guardian::netcheck::{self, NetStatus};
use guardian::{http, ipdetect};
use serde_json::json;

const EXIT_OK: u8 = 0;
const EXIT_FAIL: u8 = 1;
const EXIT_USAGE: u8 = 2;

fn main() -> ExitCode {
    ExitCode::from(run())
}

fn run() -> u8 {
    let args = match Args::parse(std::env::args().skip(1)) {
        Ok(Some(args)) => args,
        Ok(None) => return EXIT_OK, // --help / --version 已输出
        Err(msg) => {
            eprintln!("campus-auth-guardian: {msg}");
            eprintln!("用 --help 查看用法。");
            return EXIT_USAGE;
        }
    };

    match args.cmd.as_str() {
        "run" => cmd_run(&args),
        "auth" => cmd_auth(&args),
        "status" => cmd_status(&args),
        "check" => cmd_check(&args),
        "detect-ip" => cmd_detect_ip(&args),
        "validate" => cmd_validate(&args),
        other => {
            eprintln!("campus-auth-guardian: 未知命令 '{other}'");
            eprintln!("用 --help 查看用法。");
            EXIT_USAGE
        }
    }
}

// ---------------------------------------------------------------------------
// 子命令
// ---------------------------------------------------------------------------

/// 前台运行守护进程。procd 会以这个模式拉起它。
fn cmd_run(args: &Args) -> u8 {
    let cfg = match load_config(args) {
        Ok(c) => c,
        Err(code) => return code,
    };

    for p in cfg.validate() {
        guardian::log_warn!("配置有问题: {p}");
    }
    guardian::log_info!(
        "启动守护进程 v{}，配置 {}，守护{}",
        env!("CARGO_PKG_VERSION"),
        args.config.display(),
        if cfg.enabled { "开启" } else { "关闭" }
    );

    let g = match Guardian::start(cfg, args.config.clone()) {
        Ok(g) => g,
        Err(e) => {
            guardian::log_error!("启动失败: {e}");
            return EXIT_FAIL;
        }
    };

    g.wait(); // 阻塞至收到 SIGTERM / SIGINT
    EXIT_OK
}

/// 立刻认证一次。
fn cmd_auth(args: &Args) -> u8 {
    let cfg = match load_config(args) {
        Ok(c) => c,
        Err(code) => return code,
    };

    let outcome = auth::authenticate(&cfg);

    if args.json {
        println!("{}", outcome_json(&outcome));
    } else {
        match &outcome {
            AuthOutcome::Success => println!("认证成功"),
            AuthOutcome::AlreadyOnline => println!("已在线，无需认证"),
            AuthOutcome::Failed { msg } => println!("认证失败: {msg}"),
            AuthOutcome::NetworkError { msg } => println!("网络错误: {msg}"),
        }
    }

    match outcome {
        AuthOutcome::Success | AuthOutcome::AlreadyOnline => EXIT_OK,
        AuthOutcome::Failed { .. } => EXIT_FAIL,
        AuthOutcome::NetworkError { .. } => EXIT_USAGE,
    }
}

/// 打印守护进程写下的状态快照。
fn cmd_status(args: &Args) -> u8 {
    let text = match std::fs::read_to_string(STATUS_FILE) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if args.json {
                println!("{}", json!({"running": false}));
            } else {
                println!("守护进程未在运行（{STATUS_FILE} 不存在）");
            }
            return EXIT_FAIL;
        }
        Err(e) => {
            eprintln!("读取 {STATUS_FILE} 失败: {e}");
            return EXIT_USAGE;
        }
    };

    if args.json {
        // 原样透传 —— 内容本来就是 JSON，重新解析只会丢掉未来新增的字段
        print!("{text}");
        if !text.ends_with('\n') {
            println!();
        }
        return EXIT_OK;
    }

    let v: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{STATUS_FILE} 内容不是合法 JSON: {e}");
            return EXIT_USAGE;
        }
    };

    println!("版本:     {}", v["version"].as_str().unwrap_or("?"));
    println!(
        "守护:     {}",
        if v["enabled"].as_bool().unwrap_or(false) {
            "开启"
        } else {
            "关闭"
        }
    );
    println!("状态:     {}", v["state"].as_str().unwrap_or("?"));
    println!("网络:     {}", describe_net(&v["net"]));
    println!("上次认证: {}", describe_auth(&v["last_auth"]));
    if let Some(ts) = v["last_auth_ts"].as_u64() {
        println!("认证时间: {ts}");
    }
    println!("连续失败: {}", v["consecutive_failures"]);
    println!("下次检测: {} 秒后", v["next_check_secs"]);

    if let Some(problems) = v["config_problems"].as_array() {
        if !problems.is_empty() {
            println!("配置问题:");
            for p in problems {
                println!("  - {}", p.as_str().unwrap_or("?"));
            }
        }
    }
    EXIT_OK
}

/// 做一次连通性检测。
fn cmd_check(args: &Args) -> u8 {
    let cfg = match load_config(args) {
        Ok(c) => c,
        Err(code) => return code,
    };

    let status = netcheck::check(&cfg.check_url, Duration::from_secs(8));

    if args.json {
        println!("{}", net_json(&status));
    } else {
        println!("检测地址: {}", cfg.check_url);
        match &status {
            NetStatus::Connected => println!("结果: 已连通"),
            NetStatus::CaptivePortal { redirect } if redirect.is_empty() => {
                println!("结果: 被劫持到认证门户（无重定向地址，疑似 200 拦截）");
            }
            NetStatus::CaptivePortal { redirect } => {
                println!("结果: 被劫持到认证门户");
                println!("重定向: {redirect}");
                let (ac_ip, ac_name, user_ip) = netcheck::extract_ac_params(redirect);
                println!("AC IP:   {ac_ip}");
                println!("AC 名称: {ac_name}");
                println!("会话 IP: {user_ip}");
            }
            NetStatus::DnsPending => println!("结果: DNS 暂不可用（认证刚生效时常见）"),
            NetStatus::Disconnected { reason } => println!("结果: 无法联网 —— {reason}"),
        }
    }

    match status {
        NetStatus::Connected => EXIT_OK,
        NetStatus::DnsPending => EXIT_FAIL,
        NetStatus::CaptivePortal { .. } => EXIT_FAIL,
        NetStatus::Disconnected { .. } => EXIT_USAGE,
    }
}

/// IP 探测诊断。认证出问题时第一个该跑的命令。
fn cmd_detect_ip(args: &Args) -> u8 {
    let cfg = match load_config(args) {
        Ok(c) => c,
        Err(code) => return code,
    };

    let portal = http::parse_url(&cfg.auth_url);
    let kernel_ip = portal
        .as_ref()
        .and_then(|(_, host, port, _)| ipdetect::source_ip_for(host, *port));
    let adapters = ipdetect::list_adapters();
    let fallback = ipdetect::detect_local_ip(cfg.wan_iface.as_deref());

    if args.json {
        println!(
            "{}",
            json!({
                "auth_url": cfg.auth_url,
                "portal_base": cfg.portal_base(),
                "kernel_source_ip": kernel_ip,
                "wan_iface": cfg.wan_iface,
                "fallback_ip": fallback,
                "adapters": adapters.iter().map(|a| json!({
                    "name": a.name,
                    "ip": a.ip,
                    "mac": a.mac,
                    "usable": ipdetect::is_usable_ipv4(&a.ip),
                    "score": ipdetect::ip_score(&a.ip),
                })).collect::<Vec<_>>(),
            })
        );
        return EXIT_OK;
    }

    println!("配置文件:   {}", args.config.display());
    println!("认证地址:   {}", cfg.auth_url);
    println!("门户根地址: {}", cfg.portal_base());
    println!();
    println!("内核选定的源 IP（通往门户）: {}", kernel_ip.as_deref().unwrap_or("（探测失败）"));
    println!("兜底探测结果:               {}", fallback.as_deref().unwrap_or("（无）"));
    println!();
    println!("全部 IPv4 地址:");
    if adapters.is_empty() {
        println!("  （一个都没枚举到）");
    }
    for a in &adapters {
        println!(
            "  {:<16} {:<16} {:<8} 评分={}{}",
            a.name,
            a.ip,
            a.mac.as_deref().unwrap_or("-"),
            ipdetect::ip_score(&a.ip),
            if ipdetect::is_usable_ipv4(&a.ip) {
                ""
            } else {
                "  [不可用于认证]"
            }
        );
    }
    println!();
    println!("认证时实际使用的候选 IP 依次为：fixed_ip → 上面的「源 IP」→ 门户报告的会话 IP → 兜底结果");

    if kernel_ip.is_none() && fallback.is_none() && cfg.fixed_ip.is_none() {
        return EXIT_FAIL;
    }
    EXIT_OK
}

/// 校验配置。
fn cmd_validate(args: &Args) -> u8 {
    let cfg = match load_config(args) {
        Ok(c) => c,
        Err(code) => return code,
    };

    let mut problems = cfg.validate();
    // 额外的环境检查：填了 https 但没编 TLS 时，现在就该报出来
    if let Err(e) = netcheck::check_url_supported(&cfg.check_url) {
        problems.push(format!("检测地址不可用：{e}"));
    }
    if let Err(e) = netcheck::check_url_supported(&cfg.auth_url) {
        problems.push(format!("认证地址不可用：{e}"));
    }

    if args.json {
        println!("{}", json!({"ok": problems.is_empty(), "problems": problems}));
    } else if problems.is_empty() {
        println!("配置正常。");
    } else {
        println!("发现 {} 个问题：", problems.len());
        for p in &problems {
            println!("  - {p}");
        }
    }

    if problems.is_empty() {
        EXIT_OK
    } else {
        EXIT_FAIL
    }
}

// ---------------------------------------------------------------------------
// 辅助
// ---------------------------------------------------------------------------

fn load_config(args: &Args) -> Result<Config, u8> {
    match Config::load(&args.config) {
        Ok(c) => Ok(c),
        Err(e) => {
            eprintln!("读取配置 {} 失败: {e}", args.config.display());
            Err(EXIT_USAGE)
        }
    }
}

fn outcome_json(o: &AuthOutcome) -> serde_json::Value {
    match o {
        AuthOutcome::Success => json!({"kind": "success", "msg": ""}),
        AuthOutcome::AlreadyOnline => json!({"kind": "already_online", "msg": ""}),
        AuthOutcome::Failed { msg } => json!({"kind": "failed", "msg": msg}),
        AuthOutcome::NetworkError { msg } => json!({"kind": "network_error", "msg": msg}),
    }
}

fn net_json(s: &NetStatus) -> serde_json::Value {
    match s {
        NetStatus::Connected => json!({"kind": "connected"}),
        NetStatus::CaptivePortal { redirect } => {
            json!({"kind": "captive_portal", "redirect": redirect})
        }
        NetStatus::DnsPending => json!({"kind": "dns_pending"}),
        NetStatus::Disconnected { reason } => json!({"kind": "disconnected", "reason": reason}),
    }
}

fn describe_net(v: &serde_json::Value) -> String {
    match v["kind"].as_str() {
        Some("connected") => "已连通".into(),
        Some("captive_portal") => match v["redirect"].as_str() {
            Some(r) if !r.is_empty() => format!("被劫持到认证门户（{r}）"),
            _ => "被劫持到认证门户".into(),
        },
        Some("dns_pending") => "DNS 暂不可用".into(),
        Some("disconnected") => format!("无法联网 —— {}", v["reason"].as_str().unwrap_or("?")),
        _ => "未知".into(),
    }
}

fn describe_auth(v: &serde_json::Value) -> String {
    match v["kind"].as_str() {
        Some("success") => "成功".into(),
        Some("already_online") => "已在线".into(),
        Some("failed") => format!("失败 —— {}", v["msg"].as_str().unwrap_or("?")),
        Some("network_error") => format!("网络错误 —— {}", v["msg"].as_str().unwrap_or("?")),
        _ => "（本次启动后尚未认证）".into(),
    }
}

// ---------------------------------------------------------------------------
// 参数解析
// ---------------------------------------------------------------------------

struct Args {
    cmd: String,
    config: PathBuf,
    json: bool,
}

impl Args {
    /// `Ok(None)` 表示已经处理完（`--help` / `--version`），调用方应直接退出。
    fn parse(argv: impl Iterator<Item = String>) -> Result<Option<Self>, String> {
        let mut cmd: Option<String> = None;
        let mut config = Config::default_path();
        let mut json = false;
        let mut argv = argv;

        while let Some(a) = argv.next() {
            match a.as_str() {
                "-h" | "--help" => {
                    print_help();
                    return Ok(None);
                }
                "-V" | "--version" => {
                    println!("campus-auth-guardian {}", env!("CARGO_PKG_VERSION"));
                    return Ok(None);
                }
                "--json" => json = true,
                "-c" | "--config" => {
                    let p = argv.next().ok_or("--config 需要一个路径参数")?;
                    config = PathBuf::from(p);
                }
                s if s.starts_with("--config=") => {
                    config = PathBuf::from(&s["--config=".len()..]);
                }
                s if s.starts_with('-') && s.len() > 1 => {
                    return Err(format!("未知选项 '{s}'"));
                }
                s => {
                    if cmd.is_some() {
                        return Err(format!("多余的命令 '{s}'"));
                    }
                    cmd = Some(s.to_string());
                }
            }
        }

        Ok(Some(Self {
            cmd: cmd.unwrap_or_else(|| "run".into()),
            config,
            json,
        }))
    }
}

fn print_help() {
    println!(
        "\
campus-auth-guardian {ver} —— 校园网 ePortal 认证守护进程（OpenWrt）

用法: campus-auth-guardian [命令] [选项]

命令:
  run           前台运行守护进程（默认；由 procd 拉起）
  auth          立刻认证一次
  status        打印守护进程的当前状态
  check         做一次连通性检测，识别是否被劫持到门户
  detect-ip     打印 IP 探测结果（认证排错先跑这个）
  validate      校验配置文件

选项:
  -c, --config <路径>   配置文件路径（默认 {path}）
      --json            以 JSON 输出，便于脚本处理
  -h, --help            显示本帮助
  -V, --version         显示版本

示例:
  campus-auth-guardian detect-ip          # 看内核认为的源 IP 对不对
  campus-auth-guardian auth               # 手动认证一次
  campus-auth-guardian status --json      # 给脚本读的状态

守护进程的信号:
  SIGHUP    重新读取配置
  SIGUSR1   立刻认证一次
  SIGTERM   退出",
        ver = env!("CARGO_PKG_VERSION"),
        path = guardian::config::UCI_PATH,
    );
}

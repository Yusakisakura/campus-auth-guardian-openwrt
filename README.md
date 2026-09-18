# Campus Auth Guardian (OpenWrt)

校园网 ePortal 认证守护进程 —— **Rust 内核 + LuCI 界面**，跑在 OpenWrt 路由器上。

特别感谢 @NekoMirra

> 衍生自 [NekoMirra/campus-auth-guardian](https://github.com/NekoMirra/campus-auth-guardian)（MIT）。
> 上游是 Windows 桌面程序，本仓库移植到 OpenWrt，重做了 IP 探测、配置和界面。

24小时自动登录。

## 功能特性

- **自动守护** — 周期检测网络，断线/被踢立刻重认证，指数退避（10s → 20s → … → 10min）
- **WAN 上线即认证** — DHCP 拿到地址后立刻动手，不必等退避计时
- **LuCI 界面** — 状态 / 设置 / 日志 / 手动操作，装完就能用
- **多架构支持** — aarch64 / x86_64 / armv7，覆盖主流路由器和软路由
- **双格式发布** — `.ipk`（opkg，OpenWrt 21.02 ~ 23.05）和 `.apk`（apk，OpenWrt 24.10+）
- **纯静态 musl 二进制**（~500KB）— 不依赖固件 libc，跨版本兼容
- **零 C 依赖** — 默认构建不需要 C 交叉工具链，`rustup target add` 即可

## 已验证环境

| 项目 | 值 |
|---|---|
| 设备 | CMCC RAX3000M（MT7981B / Filogic 820，双核 Cortex-A53） |
| 固件 | ImmortalWrt 24.10.5（OpenWrt 24.10 分支） |
| 包架构 | `aarch64_cortex-a53` |
| Rust target | `aarch64-unknown-linux-musl` |

## 快速开始

从 [Releases](https://github.com/Yusakisakura/campus-auth-guardian-openwrt/releases) 下载对应架构的包，传到路由器安装：

```sh
# OpenWrt 21.02 ~ 23.05（opkg）
scp campus-auth-guardian_*.ipk root@192.168.1.1:/tmp/
ssh root@192.168.1.1 "opkg install /tmp/campus-auth-guardian_*.ipk"

# OpenWrt 24.10+（apk）
scp campus-auth-guardian_*.apk root@192.168.1.1:/tmp/
ssh root@192.168.1.1 "apk add --allow-untrusted /tmp/campus-auth-guardian_*.apk"
```
或使用软件包管理直接安装

装完自动启用，去 **LuCI → 服务 → 校园网认证 → 设置** 填三项必填配置即可。

## 支持的架构

| OpenWrt 架构 | Rust target | 典型设备 |
|---|---|---|
| `aarch64_cortex-a53` | `aarch64-unknown-linux-musl` | RAX3000M、R2S/R4S、树莓派 4 |
| `aarch64_cortex-a72` | `aarch64-unknown-linux-musl` | R5S/R6S、树莓派 5 |
| `x86_64` | `x86_64-unknown-linux-musl` | 软路由、虚拟机 |
| `arm_cortex-a7_neon-vfpv4` | `armv7-unknown-linux-musleabihf` | IPQ40xx 等 ARMv7 路由器 |

> **MIPS 设备**：Rust 已不提供 MIPS 预编译 target，无法交叉编译。

## 配置

**必填三项**：`auth_url`（认证地址）、`student_id`（学号）、`password`（密码）

可通过 LuCI 设置页填写，或命令行：

```sh
uci set campus-auth-guardian.main.auth_url='10.0.0.1'
uci set campus-auth-guardian.main.student_id='2023123456'
uci set campus-auth-guardian.main.password='你的密码'
uci set campus-auth-guardian.main.operator='unicom'   # campus / cmcc / unicom / telecom
uci commit campus-auth-guardian
/etc/init.d/campus-auth-guardian reload
```

`auth_url` 会自动补全，以下三种写法等价：`10.0.0.1` / `http://10.0.0.1/` / `http://10.0.0.1:801/eportal/portal/login`

### 怎么找认证地址

连着校园网，用浏览器打开任意 **http**（不是 https）网站，看它跳转到的地址，填主机名即可。
注：本软件不一定适配所有学校的校园网认证系统，具体是否可用还要看学校系统

## 命令行

```sh
campus-auth-guardian auth          # 手动认证一次
campus-auth-guardian check         # 检测连通性 / 是否被劫持到门户
campus-auth-guardian detect-ip     # IP 探测诊断（认证出问题先跑这个）
campus-auth-guardian status        # 当前状态
campus-auth-guardian validate      # 配置校验
```

## 排错

```sh
# 服务状态
/etc/init.d/campus-auth-guardian status

# 配置校验
campus-auth-guardian validate

# 看日志
logread | grep campus-auth | tail -30
```

认证失败时先跑 `campus-auth-guardian detect-ip`，确认「内核选定的源 IP」是 WAN 口的校园网地址，不是 `192.168.x.x`。

## 自己编译

```sh
# WSL / Linux（需要 rustup）
sh scripts/build-all.sh              # 编译所有架构 + ipk/apk 双格式
sh scripts/build-all.sh --no-build   # 跳过编译，用已有二进制打包
sh scripts/build-all.sh --arch x86_64  # 只打一个架构
```

不需要 OpenWrt SDK，也不需要 C 交叉工具链。

## License

MIT，见 [LICENSE](LICENSE)。版权行同时保留上游署名。

---

**免责声明**：本工具仅用于让你自己的设备接入你已有权限使用的校园网。请遵守所在学校的网络使用规定。
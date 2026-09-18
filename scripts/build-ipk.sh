#!/bin/sh
#
# 交叉编译并打包成单个 .ipk。
#
# 打包步骤在 WSL 内执行（原生 ar/tar/chmod），因为：
# - 现代 GNU ar 的扩展格式跟 opkg 不兼容
# - Windows 9p 挂载上 chmod 是空操作
# - WSL /tmp 是原生 Linux 文件系统，所有工具正常工作
#
# 用法：
#   scripts/build-ipk.sh              # 编译 + 打包
#   scripts/build-ipk.sh --no-build   # 跳过编译，用已有二进制打包

set -eu

ROOT=$(cd "$(dirname "$0")/.." && pwd)
TARGET=aarch64-unknown-linux-musl
BIN="$ROOT/target/$TARGET/release/campus-auth-guardian"

DO_BUILD=1
[ "${1:-}" = "--no-build" ] && DO_BUILD=0

# --- 1. 编译 ---
if [ "$DO_BUILD" = "1" ]; then
    echo ">>> 交叉编译 ($TARGET)"
    (cd "$ROOT" && cargo build --release --target "$TARGET")
fi

if [ ! -f "$BIN" ]; then
    echo "找不到二进制：$BIN" >&2
    echo "先跑一次不带 --no-build 的构建。" >&2
    exit 1
fi

# --- 2. 打包（在 WSL 内执行）---
echo ">>> 通过 WSL 打包 ipk"
# wsl -l 输出 UTF-16LE，先转成 UTF-8 再解析
WSL_DISTRO=$(wsl -l -v 2>/dev/null | iconv -f UTF-16LE -t UTF-8 2>/dev/null | grep -oP 'Ubuntu[^ *]*' | head -1)
if [ -z "$WSL_DISTRO" ]; then
    echo "错误: 找不到 WSL Ubuntu 发行版" >&2
    echo "请先安装: wsl --install Ubuntu" >&2
    exit 1
fi
echo "  WSL 发行版: $WSL_DISTRO"

# 把 Windows 路径转成 WSL 格式 (E:\xyw\... → /mnt/e/xyw/...)
WSL_ROOT=$(wsl -d "$WSL_DISTRO" -e wslpath -a "$(cygpath -w "$ROOT")")

# MSYS_NO_PATHCONV=1 阻止 Git Bash 把 /mnt/e/... 自动转成 C:/Program Files/Git/mnt/e/...
MSYS_NO_PATHCONV=1 wsl -d "$WSL_DISTRO" -e bash "$WSL_ROOT/scripts/build-ipk-wsl.sh"

echo
echo ">>> 完成"
ls -lh "$ROOT/dist/"*.ipk

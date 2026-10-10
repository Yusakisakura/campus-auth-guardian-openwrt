#!/bin/bash
# 多架构 + 双格式（ipk + apk）打包脚本
# 用法：
#   scripts/build-all.sh              # 编译所有架构并打包
#   scripts/build-all.sh --no-build   # 跳过编译，用已有二进制打包
#   scripts/build-all.sh --arch aarch64_cortex-a53   # 只打一个架构
set -eu

REPO="$(cd "$(dirname "$0")/.." && pwd)"
PKG_NAME=campus-auth-guardian
PKG_RELEASE=1
DIST="$REPO/dist"

DO_BUILD=1
FILTER_ARCH=""

while [ $# -gt 0 ]; do
    case "$1" in
        --no-build) DO_BUILD=0 ;;
        --arch)     FILTER_ARCH="$2"; shift ;;
        *)          echo "未知参数: $1" >&2; exit 1 ;;
    esac
    shift
done

# ── apk 打包工具（Alpine 官方 apk-tools-static，内含 mkpkg）──────────
# OpenWrt 25.12+ 的 .apk 是 ADB(v3) 格式，必须用官方 mkpkg 生成，
# 手搓 tar 包会报 "v2 package format error"。
APK_TOOLS_VER=3.0.8-r0
APK_TOOL_DIR="$REPO/build-tools"
APK_BIN="$APK_TOOL_DIR/sbin/apk.static"

ensure_apk_tool() {
    [ -x "$APK_BIN" ] && return 0
    echo ">>> 下载 apk-tools-static $APK_TOOLS_VER ..."
    mkdir -p "$APK_TOOL_DIR"
    local tmp="$APK_TOOL_DIR/apk-tools-static.apk"
    local url="https://dl-cdn.alpinelinux.org/alpine/edge/main/x86_64/apk-tools-static-$APK_TOOLS_VER.apk"
    if ! curl -fsSL -o "$tmp" "$url"; then
        # 固定版本下架时从 APKINDEX 解析当前版本
        local ver
        ver=$(curl -fsSL "https://dl-cdn.alpinelinux.org/alpine/edge/main/x86_64/APKINDEX.tar.gz" \
            | tar xzO APKINDEX 2>/dev/null \
            | awk -v RS='' '/\nP:apk-tools-static\n/ {for(i=1;i<=NF;i++) if($i ~ /^V:/) {print substr($i,3); exit}}')
        [ -n "$ver" ] || { echo "错误: 无法确定 apk-tools-static 版本" >&2; return 1; }
        echo ">>> 回退到 apk-tools-static $ver"
        curl -fsSL -o "$tmp" \
            "https://dl-cdn.alpinelinux.org/alpine/edge/main/x86_64/apk-tools-static-$ver.apk" \
            || { echo "错误: 下载 apk-tools-static 失败" >&2; return 1; }
    fi
    tar xzf "$tmp" -C "$APK_TOOL_DIR" || { echo "错误: 解包 apk-tools-static 失败" >&2; return 1; }
    rm -f "$tmp"
    chmod +x "$APK_BIN"
    [ -x "$APK_BIN" ] || { echo "错误: 找不到 $APK_BIN" >&2; return 1; }
}

# 版本号
PKG_VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' "$REPO/Cargo.toml" | head -1)
[ -z "$PKG_VERSION" ] && { echo "错误: 无法从 Cargo.toml 读取版本号" >&2; exit 1; }
echo ">>> 版本: $PKG_VERSION"

# 架构映射：OpenWrt架构 → Rust target
declare -A ARCH_MAP=(
    [aarch64_cortex-a53]=aarch64-unknown-linux-musl
    [aarch64_cortex-a72]=aarch64-unknown-linux-musl
    [x86_64]=x86_64-unknown-linux-musl
    [arm_cortex-a7_neon-vfpv4]=armv7-unknown-linux-musleabihf
)

mkdir -p "$DIST"

# ── 编译（只对不同的 Rust target 各编一次）────────────
if [ "$DO_BUILD" = "1" ]; then
    declare -A BUILT_TARGETS=()
    for arch in "${!ARCH_MAP[@]}"; do
        [ -n "$FILTER_ARCH" ] && [ "$arch" != "$FILTER_ARCH" ] && continue
        rust_target="${ARCH_MAP[$arch]}"
        if [ -z "${BUILT_TARGETS[$rust_target]:-}" ]; then
            echo ">>> 编译 $rust_target ..."
            (cd "$REPO" && cargo build --release --target "$rust_target") || { echo "编译失败: $rust_target" >&2; exit 1; }
            BUILT_TARGETS[$rust_target]=1
        fi
    done
fi

# ── 打包 ─────────────────────────────────────────────
build_one() {
    local arch="$1"
    local rust_target="${ARCH_MAP[$arch]}"
    local bin="$REPO/target/$rust_target/release/$PKG_NAME"

    if [ ! -f "$bin" ]; then
        echo "  ✗ 跳过 $arch: 找不到 $bin"
        return
    fi

    echo ">>> 打包 $arch (Rust: $rust_target)"

    local stage
    stage=$(mktemp -d)
    local data="$stage/data"
    local ctrl="$stage/control"
    local inner="$stage/inner"
    mkdir -p "$data" "$ctrl" "$inner"

    # 复制文件
    mkdir -p "$data/usr/bin"
    cp "$bin" "$data/usr/bin/$PKG_NAME"
    cp -R "$REPO/openwrt/files/." "$data/"

    # 权限
    find "$data" -type d -exec chmod 0755 {} +
    find "$data" -type f -exec chmod 0644 {} +
    chmod 0755 "$data/usr/bin/$PKG_NAME"
    chmod 0755 "$data/etc/init.d/$PKG_NAME"
    chmod 0755 "$data/etc/hotplug.d/iface/90-$PKG_NAME"
    chmod 0755 "$data/usr/libexec/rpcd/$PKG_NAME"
    chmod 0600 "$data/etc/config/$PKG_NAME"

    # ── ipk（tar.gz 容器，内含 control.tar.gz + data.tar.gz）──
    local size
    size=$(du -sk "$data" | cut -f1)
    sed -e "s|@VERSION@|${PKG_VERSION}-${PKG_RELEASE}|" \
        -e "s|@ARCH@|${arch}|" \
        -e "s|@SIZE@|${size}|" \
        -e "s|@MAINTAINER@|Yusaki_Sakura|" \
        "$REPO/openwrt/control" > "$ctrl/control"
    cp "$REPO/openwrt/preinst"  "$ctrl/preinst"
    cp "$REPO/openwrt/postinst" "$ctrl/postinst"
    cp "$REPO/openwrt/prerm"    "$ctrl/prerm"
    chmod 0755 "$ctrl/preinst" "$ctrl/postinst" "$ctrl/prerm"

    printf '2.0\n' > "$inner/debian-binary"
    (cd "$ctrl" && tar --owner=0 --group=0 --numeric-owner -czf "$inner/control.tar.gz" .)
    (cd "$data" && tar --owner=0 --group=0 --numeric-owner -czf "$inner/data.tar.gz" .)

    local ipk="$DIST/${PKG_NAME}_${PKG_VERSION}-${PKG_RELEASE}_${arch}.ipk"
    (cd "$inner" && tar --owner=0 --group=0 --numeric-owner -czf "$ipk" .)
    echo "  ✓ ipk: $(ls -lh "$ipk" | awk '{print $5}') $ipk"

    # ── apk（ADB v3 格式，用官方 apk mkpkg 生成，兼容 OpenWrt 25.12+）──
    ensure_apk_tool

    # mkpkg 会把文件属主写进包；非 root 构建装到路由器会变 nobody
    if [ "$(id -u)" = 0 ]; then
        chown -R 0:0 "$data"
    elif command -v sudo >/dev/null 2>&1 && sudo -n chown -R 0:0 "$data" 2>/dev/null; then
        :
    else
        echo "  警告: 无法以 root 改属主，包内属主将是构建用户（装后为 nobody）" >&2
    fi

    local apk="$DIST/${PKG_NAME}_${PKG_VERSION}-r${PKG_RELEASE}_${arch}.apk"
    "$APK_BIN" mkpkg \
        --files "$data" \
        --output "$apk" \
        --info name:"$PKG_NAME" \
        --info version:"${PKG_VERSION}-r${PKG_RELEASE}" \
        --info arch:"$arch" \
        --info description:"校园网 ePortal 认证守护进程（含 LuCI 界面）" \
        --info url:"https://github.com/Yusakisakura/campus-auth-guardian-openwrt" \
        --info maintainer:"Yusaki_Sakura" \
        --info license:"MIT" \
        --info origin:"$PKG_NAME" \
        --info "depends:libc luci-base" \
        --script "pre-install:$REPO/openwrt/preinst" \
        --script "pre-upgrade:$REPO/openwrt/preinst" \
        --script "post-install:$REPO/openwrt/postinst" \
        --script "post-upgrade:$REPO/openwrt/postinst" \
        --script "pre-deinstall:$REPO/openwrt/prerm"
    echo "  ✓ apk: $(ls -lh "$apk" | awk '{print $5}') $apk"

    rm -rf "$stage"
}

for arch in "${!ARCH_MAP[@]}"; do
    [ -n "$FILTER_ARCH" ] && [ "$arch" != "$FILTER_ARCH" ] && continue
    build_one "$arch"
done

echo
echo "=== 完成 ==="
ls -lh "$DIST"/${PKG_NAME}_${PKG_VERSION}-* 2>/dev/null
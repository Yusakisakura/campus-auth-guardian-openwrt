#!/bin/bash
# 在 WSL 内部运行的打包脚本。
#
# OpenWrt 24.10+ 的 ipk 格式：一个 gzip 压缩的 tar 包，内含：
#   ./debian-binary     ("2.0\n")
#   ./control.tar.gz    (包元数据)
#   ./data.tar.gz       (实际文件)
#
# 旧版（OpenWrt 21 及之前）用 ar 归档，24.10 改成了 tar.gz。
set -eu

REPO=/mnt/e/xyw/campus-auth-guardian-openwrt
PKG_NAME=campus-auth-guardian
PKG_RELEASE=1
PKG_ARCH=aarch64_cortex-a53

# 版本号从 Cargo.toml 自动读取，改一处全生效
PKG_VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' "$REPO/Cargo.toml" | head -1)
if [ -z "$PKG_VERSION" ]; then
    echo "错误: 无法从 Cargo.toml 读取版本号" >&2
    exit 1
fi
echo ">>> 版本: $PKG_VERSION"

# 临时目录放在 WSL 原生文件系统 /tmp 上（不是 /mnt/e 的 9p 挂载）
STAGE=$(mktemp -d)
trap "rm -rf $STAGE" EXIT
DATA="$STAGE/data"
CTRL="$STAGE/control"
INNER="$STAGE/inner"
DIST="$REPO/dist"

mkdir -p "$DATA" "$CTRL" "$INNER" "$DIST"

# --- 1. 二进制 ---
BIN="$REPO/target/aarch64-unknown-linux-musl/release/$PKG_NAME"
if [ ! -f "$BIN" ]; then
    echo "错误: 找不到 $BIN" >&2
    exit 1
fi

mkdir -p "$DATA/usr/bin"
cp "$BIN" "$DATA/usr/bin/$PKG_NAME"

# --- 2. openwrt/files/ 整棵树 ---
cp -R "$REPO/openwrt/files/." "$DATA/"

# --- 3. 权限 ---
find "$DATA" -type d -exec chmod 0755 {} +
find "$DATA" -type f -exec chmod 0644 {} +
chmod 0755 "$DATA/usr/bin/$PKG_NAME"
chmod 0755 "$DATA/etc/init.d/$PKG_NAME"
chmod 0755 "$DATA/etc/hotplug.d/iface/90-$PKG_NAME"
chmod 0755 "$DATA/usr/libexec/rpcd/$PKG_NAME"
chmod 0600 "$DATA/etc/config/$PKG_NAME"

# 验证权限
for f in "usr/bin/$PKG_NAME" "etc/init.d/$PKG_NAME" "etc/hotplug.d/iface/90-$PKG_NAME" "usr/libexec/rpcd/$PKG_NAME"; do
    mode=$(stat -c "%a" "$DATA/$f")
    if [ "$mode" != "755" ]; then
        echo "权限错误: $f = $mode" >&2; exit 1
    fi
done
echo ">>> 权限验证通过"

# --- 4. control 元数据 ---
SIZE=$(du -sk "$DATA" | cut -f1)
sed -e "s|@VERSION@|${PKG_VERSION}-${PKG_RELEASE}|" \
    -e "s|@ARCH@|${PKG_ARCH}|" \
    -e "s|@SIZE@|${SIZE}|" \
    -e "s|@MAINTAINER@|campus-auth-guardian|" \
    "$REPO/openwrt/control" > "$CTRL/control"

cp "$REPO/openwrt/preinst"  "$CTRL/preinst"
cp "$REPO/openwrt/postinst" "$CTRL/postinst"
cp "$REPO/openwrt/prerm"    "$CTRL/prerm"
chmod 0755 "$CTRL/preinst" "$CTRL/postinst" "$CTRL/prerm"

# --- 5. 打内层 tar 包 ---
printf '2.0\n' > "$INNER/debian-binary"
(cd "$CTRL" && tar --owner=0 --group=0 --numeric-owner -czf "$INNER/control.tar.gz" .)
(cd "$DATA" && tar --owner=0 --group=0 --numeric-owner -czf "$INNER/data.tar.gz" .)

echo ">>> 内层 tar 打包完成"
echo "  control.tar.gz: $(stat -c%s "$INNER/control.tar.gz") bytes"
echo "  data.tar.gz:    $(stat -c%s "$INNER/data.tar.gz") bytes"

# --- 6. 打外层 tar.gz = 最终 ipk ---
IPK="$DIST/${PKG_NAME}_${PKG_VERSION}-${PKG_RELEASE}_${PKG_ARCH}.ipk"
rm -f "$IPK"
(cd "$INNER" && tar --owner=0 --group=0 --numeric-owner -czf "$IPK" .)

echo
echo ">>> 构建完成: $IPK"
ls -lh "$IPK"

# 验证
echo
echo "=== 验证 ipk 结构 ==="
file "$IPK"
echo "内容:"
tar -tzf "$IPK"

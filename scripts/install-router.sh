#!/bin/sh
#
# 把打好的 ipk 传到路由器并安装。
#
# 用法:
#   scripts/install-router.sh                 # 默认 root@192.168.1.1
#   scripts/install-router.sh root@10.0.0.1

set -eu

HOST="${1:-root@192.168.1.1}"
ROOT=$(cd "$(dirname "$0")/.." && pwd)

IPK=$(ls -1t "$ROOT"/dist/*.ipk 2>/dev/null | head -n1 || true)
if [ -z "$IPK" ]; then
	echo "dist/ 里没有 .ipk。先跑 scripts/build-ipk.sh" >&2
	exit 1
fi
NAME=$(basename "$IPK")

echo ">>> 上传 $NAME 到 $HOST"
scp "$IPK" "$HOST:/tmp/$NAME"

echo ">>> 安装"
# --force-reinstall：开发时反复装同一个版本号，不加这个 opkg 会直接跳过
ssh "$HOST" "opkg install --force-reinstall /tmp/$NAME && rm -f /tmp/$NAME"

echo ">>> 等待服务起来"
sleep 3

echo ">>> 服务状态"
ssh "$HOST" "/etc/init.d/campus-auth-guardian status 2>&1 || true"

echo
echo ">>> 状态"
ssh "$HOST" "/usr/bin/campus-auth-guardian status 2>&1 || true"

echo
echo ">>> IP 探测（确认认证的是 WAN 口那个地址）"
ssh "$HOST" "/usr/bin/campus-auth-guardian detect-ip 2>&1 || true"

echo
echo ">>> 配置校验"
ssh "$HOST" "/usr/bin/campus-auth-guardian validate 2>&1 || true"

cat <<'EOF'

如果「配置校验」报了学号/密码未填写，去 LuCI 填：
  网络 → 校园网认证 → 设置

看实时日志：
  ssh <路由器> 'logread -f | grep campus-auth'
EOF

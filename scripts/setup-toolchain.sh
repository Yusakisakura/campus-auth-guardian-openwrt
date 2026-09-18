#!/bin/sh
#
# 准备交叉编译环境。在 WSL / Linux 上跑一次即可。
#
# 不需要 C 交叉工具链 —— 默认构建（不带 --features tls）的依赖全是纯 Rust，
# rustup 自带 aarch64-musl 的静态 libc 和 rust-lld，装个 target 就能编。

set -eu

echo ">>> 检查基础工具"
missing=''
for tool in ar tar file sed; do
	command -v "$tool" >/dev/null 2>&1 || missing="$missing $tool"
done
if [ -n "$missing" ]; then
	echo "缺少工具:$missing" >&2
	echo "Debian/Ubuntu: apt-get install -y binutils tar file sed" >&2
	exit 1
fi

echo ">>> 安装 rustup（走 rsproxy 镜像）"
if command -v rustup >/dev/null 2>&1; then
	echo "已装，跳过：$(rustup --version 2>/dev/null | head -n1)"
else
	export RUSTUP_DIST_SERVER=https://rsproxy.cn
	export RUSTUP_UPDATE_ROOT=https://rsproxy.cn/rustup
	curl --proto '=https' --tlsv1.2 -sSf https://rsproxy.cn/rustup-init.sh |
		sh -s -- -y --profile minimal
fi

# shellcheck disable=SC1091
. "$HOME/.cargo/env"

echo ">>> 配置 crates.io 镜像"
mkdir -p "$HOME/.cargo"
if ! grep -q rsproxy "$HOME/.cargo/config.toml" 2>/dev/null; then
	cat >> "$HOME/.cargo/config.toml" <<'EOF'

[source.crates-io]
replace-with = 'rsproxy-sparse'

[source.rsproxy-sparse]
registry = "sparse+https://rsproxy.cn/index/"

[registries.rsproxy]
index = "https://rsproxy.cn/crates.io-index"

[net]
git-fetch-with-cli = true
EOF
	echo "已写入 ~/.cargo/config.toml"
else
	echo "已配置，跳过"
fi

echo ">>> 添加 aarch64-unknown-linux-musl target"
rustup target add aarch64-unknown-linux-musl

# 仓库的 .cargo/config.toml 里把链接器写成裸名 "rust-lld"，但 rust-lld 不在 PATH 上。
# 这个包装脚本负责从 rustc 的 sysroot 里找到它 —— 这样仓库里就不用写绝对路径，
# 免得把本机用户名带进公开仓库。
echo ">>> 安装 rust-lld 包装脚本"
cat > "$HOME/.cargo/bin/rust-lld" <<'EOF'
#!/bin/sh
exec "$(rustc --print sysroot)/lib/rustlib/$(rustc -vV | sed -n 's/^host: //p')/bin/rust-lld" "$@"
EOF
chmod 0755 "$HOME/.cargo/bin/rust-lld"

echo
echo ">>> 验证"
rustc --version
rustup target list --installed | grep musl || true
echo
echo "接下来：scripts/build-ipk.sh"

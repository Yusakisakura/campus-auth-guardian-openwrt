'use strict';
'require view';
'require rpc';

var callStatus = rpc.declare({
	object: 'campus-auth-guardian',
	method: 'status',
	expect: { }
});

return view.extend({
	load: function() {
		return callStatus();
	},

	render: function(st) {
		st = st || {};
		var version = st.version || 'unknown';
		var running = st.running === true;

		var css = '' +
			'.about-wrap{max-width:800px;margin:0 auto}' +
			'.about-hero{text-align:center;padding:32px 0 24px;border-bottom:1px solid #e0e0e0;margin-bottom:24px}' +
			'.about-hero h2{margin:12px 0 4px;font-size:24px}' +
			'.about-hero .ver{color:#666;font-size:14px}' +
			'.about-hero .badge{display:inline-block;padding:3px 12px;border-radius:4px;font-size:13px;color:#fff;margin-top:8px}' +
			'.about-section{margin-bottom:24px}' +
			'.about-section h3{margin:0 0 10px;padding-bottom:6px;border-bottom:1px solid #eee;font-size:16px}' +
			'.about-section p{margin:6px 0;line-height:1.7;color:#333}' +
			'.about-table{width:100%;border-collapse:collapse;margin:8px 0}' +
			'.about-table td{padding:6px 10px;border-bottom:1px solid #f0f0f0;vertical-align:top}' +
			'.about-table td:first-child{width:140px;font-weight:600;color:#555;white-space:nowrap}' +
			'.about-box{padding:14px 18px;border-radius:6px;margin:12px 0;line-height:1.7}' +
			'.about-box.blue{background:#e3f2fd;border-left:4px solid #2196f3}' +
			'.about-box.yellow{background:#fff8e1;border-left:4px solid #ffc107}' +
			'.about-link{color:#1976d2;text-decoration:none}.about-link:hover{text-decoration:underline}' +
			'.about-code{background:#f5f5f5;padding:2px 6px;border-radius:3px;font-size:13px}' +
			'.about-features{margin:8px 0;padding-left:20px;line-height:1.9}' +
			'.about-credits{margin-top:8px}' +
			'.about-credits p{margin:4px 0}' +
			'.about-footer{text-align:center;color:#999;font-size:12px;padding:16px 0;border-top:1px solid #eee;margin-top:16px}';

		var badgeBg = running ? '#4caf50' : '#9e9e9e';
		var badgeText = running ? '运行中' : '未运行';

		return E('div', { 'class': 'cbi-map about-wrap' }, [
			E('style', {}, [ css ]),

			/* ── 头部 ── */
			E('div', { 'class': 'about-hero' }, [
				E('h2', {}, [ 'Campus Auth Guardian' ]),
				E('div', { 'class': 'ver' }, [ 'OpenWrt 版  ·  v' + version ]),
				E('div', { 'class': 'badge', 'style': 'background:' + badgeBg }, [ badgeText ])
			]),

			/* ── 简介 ── */
			E('div', { 'class': 'about-section' }, [
				E('h3', {}, [ '项目简介' ]),
				E('p', {}, [
					'校园网 ePortal 认证守护进程，跑在 OpenWrt 路由器上。路由器 WAN 口完成一次认证，NAT 之后',
					E('strong', {}, [ '所有内网设备共享这条已认证的链路' ]),
					'，不必各跑一个客户端。'
				]),
				E('p', { 'class': 'about-credits' }, [
					E('strong', {}, [ '作者：' ]), 'Yusaki_Sakura', E('br'),
					E('strong', {}, [ '特别感谢：' ]), 'NekoMirra — 提供上游 Windows 版本及 ePortal 协议实现'
				])
			]),

			/* ── 功能特性 ── */
			E('div', { 'class': 'about-section' }, [
				E('h3', {}, [ '功能特性' ]),
				E('ul', { 'class': 'about-features' }, [
					E('li', {}, [ E('strong', {}, [ '自动守护' ]), ' — 周期检测，断线/被踢立刻重认证，指数退避（10s → 20s → … → 10min）' ]),
					E('li', {}, [ E('strong', {}, [ 'WAN 上线即认证' ]), ' — DHCP 拿到地址后立刻动手' ]),
					E('li', {}, [ E('strong', {}, [ 'LuCI 界面' ]), ' — 状态 / 设置 / 日志 / 手动操作，装完就能用' ]),
					E('li', {}, [ E('strong', {}, [ '单个 ipk' ]), ' — 传一个文件，装一次，卸载无残留' ]),
					E('li', {}, [ E('strong', {}, [ '纯静态 musl' ]), ' — ~511KB，不依赖固件 libc，OpenWrt 21.02 ~ 24.10 都能跑' ]),
					E('li', {}, [ E('strong', {}, [ '零 C 依赖' ]), ' — 交叉编译不需要 C 工具链，rustup target add 即可' ])
				])
			]),

			/* ── 技术栈 ── */
			E('div', { 'class': 'about-section' }, [
				E('h3', {}, [ '技术栈' ]),
				E('table', { 'class': 'about-table' }, [
					E('tr', {}, [ E('td', {}, [ '语言' ]), E('td', {}, [ 'Rust（内核）+ JavaScript（LuCI 界面）' ]) ]),
					E('tr', {}, [ E('td', {}, [ '认证协议' ]), E('td', {}, [ 'ePortal JSONP' ]) ]),
					E('tr', {}, [ E('td', {}, [ '进程管理' ]), E('td', {}, [ 'procd' ]) ]),
					E('tr', {}, [ E('td', {}, [ 'Web 界面' ]), E('td', {}, [ 'LuCI JS API' ]) ]),
					E('tr', {}, [ E('td', {}, [ 'IPC' ]), E('td', {}, [ 'rpcd / ubus' ]) ]),
					E('tr', {}, [ E('td', {}, [ 'Rust target' ]), E('td', {}, [ 'aarch64-unknown-linux-musl（静态链接）' ]) ])
				])
			]),

			/* ── 关联项目 ── */
			E('div', { 'class': 'about-section' }, [
				E('h3', {}, [ '关联项目' ]),
				E('table', { 'class': 'about-table' }, [
					E('tr', {}, [
						E('td', {}, [ '本项目（OpenWrt）' ]),
						E('td', {}, [
							E('a', { 'href': 'https://github.com/Yusakisakura', 'target': '_blank', 'style': 'text-decoration:none;margin-right:8px;vertical-align:middle' }, [
								E('img', { 'src': 'https://github.com/Yusakisakura.png', 'style': 'width:28px;height:28px;border-radius:50%;vertical-align:middle' })
							]),
							E('a', { 'class': 'about-link', 'href': 'https://github.com/Yusakisakura/campus-auth-guardian-openwrt', 'target': '_blank' }, [ 'campus-auth-guardian-openwrt' ])
						])
					]),
					E('tr', {}, [
						E('td', {}, [ 'Android 版' ]),
						E('td', {}, [
							E('a', { 'href': 'https://github.com/YusakiSakura', 'target': '_blank', 'style': 'text-decoration:none;margin-right:8px;vertical-align:middle' }, [
								E('img', { 'src': 'https://github.com/YusakiSakura.png', 'style': 'width:28px;height:28px;border-radius:50%;vertical-align:middle' })
							]),
							E('a', { 'class': 'about-link', 'href': 'https://github.com/YusakiSakura/campus-auth-guardian-android', 'target': '_blank' }, [ 'campus-auth-guardian-android' ])
						])
					]),
					E('tr', {}, [
						E('td', {}, [ 'Windows 版（上游）' ]),
						E('td', {}, [
							E('a', { 'href': 'https://github.com/NekoMirra', 'target': '_blank', 'style': 'text-decoration:none;margin-right:8px;vertical-align:middle' }, [
								E('img', { 'src': 'https://github.com/NekoMirra.png', 'style': 'width:28px;height:28px;border-radius:50%;vertical-align:middle' })
							]),
							E('a', { 'class': 'about-link', 'href': 'https://github.com/NekoMirra/campus-auth-guardian', 'target': '_blank' }, [ 'NekoMirra/campus-auth-guardian' ])
						])
					])
				])
			]),

			/* ── 安全说明 ── */
			E('div', { 'class': 'about-section' }, [
				E('h3', {}, [ '安全说明' ]),
				E('div', { 'class': 'about-box blue' }, [
					E('p', {}, [ '配置文件 ', E('code', { 'class': 'about-code' }, [ '/etc/config/campus-auth-guardian' ]), ' 权限为 0600（仅 root 可读）。' ]),
					E('p', {}, [ '日志中的密码已脱敏（', E('code', { 'class': 'about-code' }, [ 'user_password=***' ]), '），状态文件不含凭据。' ]),
					E('p', {}, [ '本程序只做认证，不改动防火墙、路由或 DNS 设置。' ])
				]),
				E('div', { 'class': 'about-box yellow' }, [
					E('p', {}, [ E('strong', {}, [ '免责声明：' ]), '本工具仅用于让你自己的设备接入你已有权限使用的校园网。请遵守所在学校的网络使用规定。' ])
				])
			]),

			/* ── 许可 ── */
			E('div', { 'class': 'about-section' }, [
				E('h3', {}, [ '许可' ]),
				E('p', {}, [ 'MIT License — 版权行同时保留上游署名（MIT 的硬性要求）。' ])
			]),

			E('div', { 'class': 'about-footer' }, [ 'Campus Auth Guardian for OpenWrt · MIT License' ])
		]);
	},

	handleSaveApply: null,
	handleSave: null,
	handleReset: null
});

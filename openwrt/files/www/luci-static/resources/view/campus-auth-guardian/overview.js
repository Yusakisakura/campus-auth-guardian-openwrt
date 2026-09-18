'use strict';
'require view';
'require poll';
'require rpc';
'require uci';
'require ui';
'require dom';

var callStatus = rpc.declare({
	object: 'campus-auth-guardian',
	method: 'status',
	expect: { }
});

var callAuth = rpc.declare({
	object: 'campus-auth-guardian',
	method: 'auth',
	expect: { }
});

var callDetectIp = rpc.declare({
	object: 'campus-auth-guardian',
	method: 'detect_ip',
	expect: { }
});

var NET_TEXT = {
	connected:      [ '%s: 已连通'.format(_('WAN')), 'success'  ],
	captive_portal: [ '被劫持到认证门户',             'warning' ],
	dns_pending:    [ 'DNS 暂不可用',                 'warning' ],
	disconnected:   [ '无法联网',                     'danger'  ]
};

var AUTH_TEXT = {
	success:        [ '认证成功',           'success' ],
	already_online: [ '已在线，无需认证',   'success' ],
	failed:         [ '认证失败',           'danger'  ],
	network_error:  [ '网络错误',           'danger'  ]
};

var STATE_TEXT = {
	monitoring:     [ '监控中',   'success' ],
	authenticating: [ '认证中…',  'warning' ],
	stopped:        [ '已停止',   'danger'  ]
};

/* ── helpers ─────────────────────────────────────────────── */

function badge(text, kind) {
	var bg = kind === 'success' ? '#4caf50' :
	         kind === 'warning' ? '#ff9800' :
	         kind === 'danger'  ? '#f44336' : '#9e9e9e';
	return E('span', {
		'class': 'ifacebadge',
		'style': 'background:%s;color:#fff;padding:3px 10px;border-radius:4px;font-size:13px'.format(bg)
	}, [ text ]);
}

function infoTable(rows) {
	/* rows = [ [label, value], ... ] */
	return E('table', { 'class': 'table', 'style': 'margin:0' }, rows.map(function(r) {
		return E('tr', { 'class': 'tr' }, [
			E('td', { 'class': 'td', 'style': 'width:120px;font-weight:600;color:#555;white-space:nowrap;padding:8px 12px' }, [ r[0] ]),
			E('td', { 'class': 'td', 'style': 'padding:8px 12px' }, [ typeof r[1] === 'string' ? r[1] : r[1] ])
		]);
	}));
}

function sectionCard(title, contentNodes) {
	return E('div', { 'class': 'cbi-section', 'style': 'margin-bottom:16px;padding:16px 20px' }, [
		E('h3', { 'style': 'margin:0 0 12px 0' }, [ title ]),
		E('div', { 'style': 'padding:0' }, contentNodes)
	]);
}

function tsToString(ts) {
	if (!ts) return '—';
	var d = new Date(ts * 1000);
	var p = function(n) { return (n < 10 ? '0' : '') + n; };
	return [ d.getFullYear(), p(d.getMonth() + 1), p(d.getDate()) ].join('-') +
		' ' + [ p(d.getHours()), p(d.getMinutes()), p(d.getSeconds()) ].join(':');
}

/* ── view ────────────────────────────────────────────────── */

return view.extend({
	load: function() {
		return Promise.all([
			callStatus(),
			uci.load('campus-auth-guardian')
		]);
	},

	render: function(data) {
		var st = data[0] || {};
		var self = this;

		this.daemonNode  = E('div');
		this.netNode     = E('div');
		this.authNode    = E('div');
		this.problemNode = E('div');

		this.update(st);

		poll.add(function() {
			return callStatus().then(function(s) { self.update(s || {}); });
		}, 5);

		return E('div', { 'class': 'cbi-map' }, [
			E('h2', { 'name': 'content' }, [ '校园网认证' ]),
			E('div', { 'class': 'cbi-map-descr' }, [
				'在路由器 WAN 口完成校园网 ePortal 认证，NAT 之后所有内网设备共享已认证的链路。'
			]),

			sectionCard('守护进程', [ this.daemonNode ]),
			sectionCard('网络与认证', [ this.netNode, this.authNode ]),

			this.problemNode,

			sectionCard('手动操作', [
				E('div', { 'style': 'display:flex;gap:12px;align-items:center;flex-wrap:wrap;margin-bottom:12px' }, [
					E('button', {
						'class': 'btn cbi-button cbi-button-apply',
						'style': 'padding:6px 20px',
						'click': ui.createHandlerFn(this, 'handleAuth')
					}, [ '立即认证' ]),
					E('button', {
						'class': 'btn cbi-button',
						'style': 'padding:6px 20px',
						'click': ui.createHandlerFn(this, 'handleDetectIp')
					}, [ 'IP 探测诊断' ])
				]),
				E('div', { 'class': 'cbi-section-descr', 'style': 'margin:0;color:#666' }, [
					'「立即认证」让守护进程马上认证一次；「IP 探测诊断」显示内核认为的源 IP，认证出问题时先看它。'
				])
			])
		]);
	},

	update: function(st) {
		var running = st.running === true;
		var enabled = st.enabled === true;
		var state   = st.state || (running ? 'monitoring' : 'stopped');
		var stLabel = STATE_TEXT[state] || [ state, 'warning' ];

		/* ── 守护进程 ── */
		var daemonRows = [
			[ '进程状态', running ? badge('运行中', 'success') : badge('未运行', 'danger') ],
			[ '自启动',   enabled ? badge('已启用', 'success') : badge('已停用', 'warning') ],
			[ '当前状态', badge(stLabel[0], stLabel[1]) ],
			[ '下次检测', '%d 秒后'.format(st.next_check_secs || 0) ],
			[ '连续失败', String(st.consecutive_failures || 0) ],
			[ '版本',     st.version || '—' ]
		];
		dom.content(this.daemonNode, [ infoTable(daemonRows) ]);

		/* ── 网络 ── */
		var net = st.net;
		if (!net) {
			dom.content(this.netNode, [
				E('p', { 'style': 'color:#999;font-style:italic;padding:8px 0' }, [ '尚无检测结果' ])
			]);
		} else {
			var nt = NET_TEXT[net.kind] || [ net.kind, 'warning', '?' ];
			var netRows = [
				[ '连通性',   badge(nt[0], nt[1]) ],
				[ 'WAN IP',   st.wan_ip || '—' ],
				[ '检测时间', tsToString(st.last_net_ts) ]
			];
			if (net.kind === 'captive_portal' && net.redirect)
				netRows.push([ '重定向到', E('code', {}, [ net.redirect ]) ]);
			if (net.kind === 'disconnected' && net.reason)
				netRows.push([ '断线原因', net.reason ]);
			dom.content(this.netNode, [ infoTable(netRows) ]);
		}

		/* ── 认证 ── */
		var auth = st.last_auth;
		if (!auth) {
			dom.content(this.authNode, [
				E('p', { 'style': 'color:#999;font-style:italic;padding:8px 0' }, [ '本次启动后尚未认证' ])
			]);
		} else {
			var at = AUTH_TEXT[auth.kind] || [ auth.kind, 'warning', '?' ];
			var authRows = [
				[ '上次结果', badge(at[0], at[1]) ],
				[ '认证时间', tsToString(st.last_auth_ts) ]
			];
			if (auth.msg)
				authRows.push([ '详情', auth.msg ]);
			dom.content(this.authNode, [ infoTable(authRows) ]);
		}

		/* ── 配置问题 ── */
		var problems = st.config_problems || [];
		if (problems.length) {
			dom.content(this.problemNode, [
				sectionCard('配置待完善', [
					E('div', { 'class': 'alert-message warning' }, [
						E('ul', { 'style': 'margin:4px 0' }, problems.map(function(p) {
							return E('li', {}, [ p ]);
						})),
						E('p', { 'style': 'margin:8px 0 0 0' }, [
							'请到「设置」页补全，保存后会自动重启服务。'
						])
					])
				])
			]);
		} else {
			dom.content(this.problemNode, []);
		}
	},

	handleAuth: function() {
		var self = this;
		return callAuth().then(function(res) {
			ui.addNotification(null, E('p', {}, [ (res && res.msg) || '已触发认证' ]));
			return new Promise(function(resolve) { setTimeout(resolve, 4000); });
		}).then(function() {
			return callStatus();
		}).then(function(s) {
			self.update(s || {});
		});
	},

	handleDetectIp: function() {
		return callDetectIp().then(function(r) {
			if (!r || r.error) {
				ui.addNotification(null, E('p', {}, [
					(r && r.error) || '探测失败'
				]), 'error');
				return;
			}

			var adapters = (r.adapters || []).map(function(a) {
				return E('tr', { 'class': 'tr' }, [
					E('td', { 'class': 'td' }, [ E('code', {}, [ a.name ]) ]),
					E('td', { 'class': 'td' }, [ E('code', {}, [ a.ip ]) ]),
					E('td', { 'class': 'td' }, [ a.mac || '—' ]),
					E('td', { 'class': 'td', 'style': 'text-align:center' }, [ String(a.score) ]),
					E('td', { 'class': 'td', 'style': 'text-align:center' }, [
						a.usable ? badge('可用', 'success') : badge('不可用', 'danger')
					])
				]);
			});

			ui.showModal('IP 探测结果', [
				E('div', { 'style': 'margin-bottom:12px' }, [
					E('p', {}, [
						'内核选定的源 IP（通往门户）：',
						E('strong', { 'style': 'font-size:15px' }, [ r.kernel_source_ip || '（探测失败）' ])
					]),
					E('p', {}, [
						'认证地址：', E('code', {}, [ r.auth_url || '—' ])
					])
				]),
				E('table', { 'class': 'table' }, [
					E('tr', { 'class': 'tr table-titles' }, [
						E('th', { 'class': 'th' }, [ '接口' ]),
						E('th', { 'class': 'th' }, [ 'IPv4' ]),
						E('th', { 'class': 'th' }, [ 'MAC' ]),
						E('th', { 'class': 'th', 'style': 'text-align:center' }, [ '评分' ]),
						E('th', { 'class': 'th', 'style': 'text-align:center' }, [ '可用于认证' ])
					])
				].concat(adapters)),
				E('div', { 'class': 'right', 'style': 'margin-top:12px' }, [
					E('button', {
						'class': 'btn',
						'click': ui.hideModal
					}, [ '关闭' ])
				])
			]);
		});
	},

	handleSaveApply: null,
	handleSave: null,
	handleReset: null
});

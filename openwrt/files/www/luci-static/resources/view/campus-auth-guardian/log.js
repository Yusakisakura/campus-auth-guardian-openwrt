'use strict';
'require view';
'require poll';
'require rpc';
'require dom';
'require ui';

var callLogTail = rpc.declare({
	object: 'campus-auth-guardian',
	method: 'log_tail',
	params: [ 'lines' ],
	expect: { log: [] }
});

var LINES = 200;

return view.extend({
	load: function() {
		return callLogTail(LINES);
	},

	render: function(lines) {
		var self = this;

		this.logNode = E('pre', {
			'id': 'cag-log',
			'style': 'max-height:65vh;overflow:auto;white-space:pre-wrap;' +
			         'word-break:break-all;background:#1e1e1e;color:#d4d4d4;' +
			         'padding:12px 16px;border-radius:6px;font-size:12px;' +
			         'font-family:Consolas,"Courier New",monospace;' +
			         'line-height:1.5;margin:0;min-height:200px'
		});

		this.setLines(lines);

		poll.add(function() {
			return callLogTail(LINES).then(function(l) { self.setLines(l); });
		}, 10);

		return E('div', { 'class': 'cbi-map' }, [
			E('h2', { 'name': 'content' }, [ '校园网认证 · 日志' ]),
			E('div', { 'class': 'cbi-map-descr' }, [
				'来自 logd 的 syslog，每 10 秒自动刷新。'
			]),

			E('div', { 'class': 'cbi-section', 'style': 'padding:16px 20px' }, [
				E('div', { 'style': 'display:flex;justify-content:flex-end;align-items:center;margin-bottom:12px' }, [
					E('div', { 'style': 'display:flex;gap:8px' }, [
						E('button', {
							'class': 'btn cbi-button',
							'style': 'padding:6px 16px',
							'click': ui.createHandlerFn(this, 'handleRefresh')
						}, [ '🔄 刷新' ]),
						E('button', {
							'class': 'btn cbi-button',
							'style': 'padding:6px 16px',
							'click': ui.createHandlerFn(this, 'handleClear')
						}, [ '🗑️ 清空' ])
					])
				]),
				this.logNode
			]),

			E('div', { 'class': 'cbi-section-descr', 'style': 'margin-top:8px' }, [
				'查看完整日志：', E('code', {}, [ 'logread -f | grep campus-auth' ])
			])
		]);
	},

	setLines: function(lines) {
		if (!lines || !lines.length) {
			dom.content(this.logNode, [
				E('span', { 'style': 'color:#6a9955;font-style:italic' }, [ '（暂无日志）' ])
			]);
			return;
		}

		var formatted = lines.map(function(line) {
			// 给时间戳和日志级别上色
			return line
				.replace(/(\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2})/, '\x1b[36m$1\x1b[0m')
				.replace(/(\[(INFO|WARN|ERROR)\])/, function(m, full, level) {
					var color = level === 'ERROR' ? '\x1b[31m' :
					            level === 'WARN'  ? '\x1b[33m' : '\x1b[32m';
					return color + full + '\x1b[0m';
				});
		});

		// 简单的 ANSI → HTML 转换
		var html = lines.map(function(line) {
			var esc = line.replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;');
			// 高亮时间戳
			esc = esc.replace(/(\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2})/,
				'<span style="color:#569cd6">$1</span>');
			// 高亮日志级别
			esc = esc.replace(/(\[(INFO|WARN|ERROR)\])/g, function(m, full, level) {
				var color = level === 'ERROR' ? '#f44747' :
				            level === 'WARN'  ? '#d7ba7d' : '#6a9955';
				return '<span style="color:' + color + ';font-weight:600">' + full + '</span>';
			});
			return esc;
		}).join('\n');

		this.logNode.innerHTML = html;

		var el = document.getElementById('cag-log');
		if (el) el.scrollTop = el.scrollHeight;
	},

	handleRefresh: function() {
		var self = this;
		return callLogTail(LINES).then(function(l) {
			self.setLines(l);
		});
	},

	handleClear: function() {
		dom.content(this.logNode, [
			E('span', { 'style': 'color:#6a9955;font-style:italic' }, [ '（已清空，等待刷新…）' ])
		]);
	},

	handleSaveApply: null,
	handleSave: null,
	handleReset: null
});

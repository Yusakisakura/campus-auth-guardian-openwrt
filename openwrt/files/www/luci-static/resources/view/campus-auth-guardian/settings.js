'use strict';
'require form';
'require uci';
'require view';

// 这个页面完全走 LuCI 内置的 uci RPC —— 配置本来就是 UCI 格式，
// 不需要任何自定义 ubus 方法。保存后 procd 的 reload trigger 会自动重启服务。

return view.extend({
	load: function() {
		return uci.load('campus-auth-guardian');
	},

	render: function() {
		var m, s, o;

		m = new form.Map('campus-auth-guardian', _('校园网认证'),
			_('认证路由器 WAN 口拿到的校园网 IP。保存后服务会自动重启。'));

		// ---- 基本 ----
		s = m.section(form.NamedSection, 'main', 'campus-auth-guardian', _('基本'));
		s.anonymous = true;

		o = s.option(form.Flag, 'enabled', _('启用守护'),
			_('关闭后进程仍在运行并检测网络，但不会发起认证。'));
		o.rmempty = false;
		o.default = '1';

		o = s.option(form.Value, 'auth_url', _('认证服务器地址'),
			_('可以只填主机名，会自动补全为 http://<主机>:801/eportal/portal/login。<br />' +
			  '怎么找：连上校园网后用浏览器打开任意 http 网站，看它跳转到的认证页地址。'));
		o.placeholder = '10.0.0.1';
		o.rmempty = false;

		o = s.option(form.Value, 'check_url', _('连通性检测地址'),
			_('用来判断网络是否已连通。建议填一个稳定、响应小的地址。'));
		o.placeholder = 'http://www.baidu.com';
		o.rmempty = false;

		o = s.option(form.Value, 'check_interval', _('检测间隔'),
			_('单位：秒。越小发现断线越快，但请求也越频繁。'));
		o.datatype = 'range(1,3600)';
		o.placeholder = '30';

		// ---- 账号 ----
		s = m.section(form.NamedSection, 'main', 'campus-auth-guardian', _('账号'));
		s.anonymous = true;

		o = s.option(form.Value, 'student_id', _('学号'));
		o.rmempty = false;

		o = s.option(form.ListValue, 'operator', _('运营商'));
		o.value('campus',  _('校园网'));
		o.value('cmcc',    _('中国移动'));
		o.value('unicom',  _('中国联通'));
		o.value('telecom', _('中国电信'));
		o.default = 'campus';
		o.rmempty = false;

		o = s.option(form.Value, 'password', _('密码'));
		o.password = true;
		o.rmempty = false;
		o.description = _('密码在路由器上以明文存储（配置文件权限 0600，仅 root 可读）。');

		// ---- 高级 ----
		s = m.section(form.NamedSection, 'main', 'campus-auth-guardian', _('高级'),
			_('不确定就不要改。'));
		s.anonymous = true;

		o = s.option(form.Value, 'wan_iface', _('WAN 接口名'),
			_('仅用于 IP 探测的兜底路径和「WAN 上线触发重认证」。' +
			  '主路径是询问内核通往门户的源 IP，不依赖这个值。'));
		o.placeholder = 'wan';

		o = s.option(form.Value, 'fixed_ip', _('固定认证 IP'),
			_('留空 = 自动探测（推荐）。只有在自动探测出错时才手动指定。'));
		o.datatype = 'ip4addr';
		o.placeholder = _('留空');

		o = s.option(form.Value, 'retry_interval', _('重试间隔'),
			_('单位：秒。认证失败后的重试间隔。'));
		o.datatype = 'range(1,3600)';
		o.placeholder = '10';

		o = s.option(form.Value, 'max_retries', _('单轮重试次数'),
			_('一轮里最多重试几次。全部失败后进入指数退避，最长等 10 分钟。'));
		o.datatype = 'range(1,100)';
		o.placeholder = '3';

		return m.render();
	}
});

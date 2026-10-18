// 在页面环境中运行：页面读取配置后会删除 #temp-script，扩展的隔离环境无法读取
// window.__config__，因此把登录用户写到 DOM 属性上供 content.js 读取。
(() => {
  const c = window.__config__;
  if (!c) return;
  const u = c.user;
  const name = u ? String(u.member_name || u.username || u.name || u.member_id || '已登录') : null;
  document.documentElement.dataset.nsautoUser = JSON.stringify(name);

  // 签到请求必须在页面环境中发出，让站点 Service Worker 处理网络和安全状态。
  // 站点另有跨域 fog 脚本负责行为采集和 PoW，并向 document.cookie 写入 fog。
  // Service Worker 的虚拟 Cookie 与 fog 检查是两套机制，不能混为一谈。
  document.addEventListener('nsauto-attendance-request', async () => {
    const request = JSON.parse(document.documentElement.dataset.nsautoAttendanceRequest || 'null');
    if (!request || typeof request.id !== 'string' || typeof request.random !== 'boolean') return;
    let result;
    try {
      const response = await fetch(`/api/attendance?random=${request.random}`, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        credentials: 'same-origin',
      });
      result = { id: request.id, status: response.status, body: await response.text() };
    } catch (e) {
      result = { id: request.id, error: String(e) };
    }
    document.documentElement.dataset.nsautoAttendanceResponse = JSON.stringify(result);
  });
})();

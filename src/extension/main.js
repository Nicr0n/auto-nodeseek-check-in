// 在页面环境中运行：页面读取配置后会删除 #temp-script，扩展的隔离环境无法读取
// window.__config__，因此把登录用户写到 DOM 属性上供 content.js 读取。
(() => {
  const c = window.__config__;
  if (!c) return;
  const u = c.user;
  const name = u ? String(u.member_name || u.username || u.name || u.member_id || '已登录') : null;
  document.documentElement.dataset.nsautoUser = JSON.stringify(name);
})();

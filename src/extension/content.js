// 在签到服务启动的浏览器中运行：读取当前任务，汇报登录状态，执行签到或自动登录。
// 不使用调试端口（CDP），否则 Cloudflare 人机验证会失败。

const send = (type, data) => chrome.runtime.sendMessage({ type, data });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// 登录用户由 main.js 从页面配置中读出并写到 <html data-nsauto-user>。
// 返回：undefined 表示不是 NodeSeek 正常页面（如 Cloudflare 验证页），null 表示未登录。
async function readUser() {
  const deadline = Date.now() + 5000;
  while (Date.now() < deadline) {
    const v = document.documentElement.dataset.nsautoUser;
    if (v !== undefined) return JSON.parse(v);
    await sleep(200);
  }
  return undefined;
}

// 登录表单由异步加载的 Vue 组件渲染，需要等待其出现
async function waitFor(selector, ms = 30_000) {
  const deadline = Date.now() + ms;
  while (!document.querySelector(selector)) {
    if (Date.now() > deadline) return false;
    await sleep(500);
  }
  return true;
}

function setInput(selector, value) {
  const el = document.querySelector(selector);
  if (!el) return false;
  el.value = value;
  el.dispatchEvent(new Event('input', { bubbles: true }));
  return true;
}

async function autoLogin(task, fail) {
  if (!(await waitFor('#stacked-password'))) return fail('登录页加载超时');
  if (!setInput('#stacked-email', task.username) || !setInput('#stacked-password', task.password)) {
    return fail('找不到登录表单');
  }

  const deadline = Date.now() + 90_000;
  while (!document.querySelector('input[name="cf-turnstile-response"]')?.value) {
    if (Date.now() > deadline) return fail('人机验证未能自动通过');
    await sleep(1000);
  }

  const button = document.querySelector(
    '.login-btn-group button[type="submit"], .login-btn-group button',
  );
  if (!button) return fail('找不到登录按钮');
  button.click();

  // 登录成功会跳转页面，本脚本随之结束；仍停留在本页说明登录未成功
  const end = Date.now() + 30_000;
  while (Date.now() < end) {
    await sleep(1000);
    if (document.getElementById('2fa')) return fail('账号开启了两步验证，需要手动登录');
  }
  fail('登录未成功（账号密码错误或登录被拒绝）');
}

// Cloudflare 的整页验证会加载 /cdn-cgi/challenge-platform/ 下的脚本
function onChallengePage() {
  return !!document.querySelector('script[src*="/cdn-cgi/challenge-platform/"]');
}

// NodeSeek 自己的整页安全检查（标题为 "NodeSeek | security check"），会自动完成计算并刷新页面
function onSecurityCheckPage() {
  return /security check/i.test(document.title);
}

async function requestAttendance(task) {
  const root = document.documentElement;
  delete root.dataset.nsautoAttendanceResponse;
  root.dataset.nsautoAttendanceRequest = JSON.stringify({ id: task.id, random: task.random });
  document.dispatchEvent(new Event('nsauto-attendance-request'));
  const deadline = Date.now() + 30_000;
  while (Date.now() < deadline) {
    const value = root.dataset.nsautoAttendanceResponse;
    if (value) {
      const response = JSON.parse(value);
      if (response.id === task.id) {
        if (response.error) throw new Error(response.error);
        return response;
      }
    }
    await sleep(200);
  }
  throw new Error('页面签到请求超时');
}

async function main(task) {
  const report = (data) => send('report', { id: task.id, ...data });
  const user = await readUser();
  if (task.mode === 'browse' && user === undefined) {
    await report({ kind: 'result', ok: false, message: '页面被验证拦截，跳过本次日间浏览' });
    return;
  }
  if (user === undefined) {
    // 签到时停在需要人工点击的验证上：通知服务端，等待用户通过 VNC 完成。
    // 验证通过后页面会重新加载，本脚本随之重新执行。
    // 无需点击的验证通常几秒内自动通过并跳转（本脚本随之结束），因此稍等再判断。
    if (task.mode === 'checkin' && (onChallengePage() || onSecurityCheckPage())) {
      const security = onSecurityCheckPage();
      await sleep(security ? 60_000 : 10_000);
      await report({ kind: 'challenge', challenge: security ? 'security_check' : 'cloudflare' });
    }
    return;
  }

  await report({ kind: 'state', user });
  const onSignIn = location.pathname.includes('signIn');

  if (task.mode === 'browse') {
    if (!user) {
      await report({ kind: 'result', ok: false, message: '登录已失效，跳过本次日间浏览' });
      return;
    }
    await browsePosts(task, report);
    return;
  }

  if (task.mode === 'login') {
    // 手动登录：只预填账号密码，由用户完成人机验证并点击登录
    if (onSignIn && task.username && (await waitFor('#stacked-password'))) {
      setInput('#stacked-email', task.username);
      setInput('#stacked-password', task.password);
    }
    return;
  }

  // 签到
  if (user) {
    if (onSignIn) {
      location.href = '/board';
      return;
    }
    // 安全脚本会延迟启动；给系统鼠标交互和 Service Worker 留出完成时间。
    await report({ kind: 'prepare_security_check' });
    await sleep(20_000);
    try {
      let r = await requestAttendance(task);
      let body = r.body;
      // 旧的安全状态可能失效：刷新页面重新检查，每个任务只重试一次。
      const retryKey = `nsauto-attendance-retry-${task.id}`;
      if (r.status === 403 && /security check/i.test(body) && !sessionStorage.getItem(retryKey)) {
        sessionStorage.setItem(retryKey, '1');
        location.reload();
        return;
      }
      if (r.status === 403 && /security check/i.test(body)) {
        await report({ kind: 'challenge', challenge: 'security_check' });
        // 用户操作页面后定期重试；浏览器关闭时脚本随之结束。
        do {
          await sleep(15_000);
          r = await requestAttendance(task);
          body = r.body;
        } while (r.status === 403 && /security check/i.test(body));
      }
      await report({ kind: 'attendance', status: r.status, body });
    } catch (e) {
      await report({ kind: 'result', ok: false, message: `签到请求失败：${e}` });
    }
  } else if (!task.username || !task.password) {
    await report({
      kind: 'result',
      ok: false,
      need_login: true,
      message: '登录已失效，请在管理页面手动登录',
    });
  } else if (!onSignIn) {
    location.href = '/signIn.html';
  } else {
    await autoLogin(task, (msg) =>
      report({
        kind: 'result',
        ok: false,
        need_login: true,
        message: `登录已失效，自动登录失败（${msg}），请在管理页面手动登录`,
      }),
    );
  }
}

(async () => {
  const task = await send('task');
  if (!task) return;
  try {
    await main(task);
  } catch (e) {
    await send('report', { id: task.id, kind: 'result', ok: false, message: `扩展脚本出错：${e}` });
  }
})();

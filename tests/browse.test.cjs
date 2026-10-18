const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { test } = require('node:test');
const vm = require('node:vm');

const browseSource = readFileSync(`${__dirname}/../src/extension/browse.js`, 'utf8');
const contentSource = readFileSync(`${__dirname}/../src/extension/content.js`, 'utf8');

function session({ duration = 120_000, loggedIn = true, challenge = false, links = [
  '/post-1-1', '/post-2-1', 'https://outside.example/post-3-1', '/api/account/signOut',
] } = {}) {
  let now = 0;
  const storage = new Map();
  const reports = [];
  const navigation = [];
  const clicks = [];
  const pauses = [];
  const task = { id: 'reading', mode: 'browse', browse_until_ms: duration };
  async function page(path = '/') {
    let next = null;
    const location = {
      href: `https://www.nodeseek.com${path}`, origin: 'https://www.nodeseek.com', pathname: path,
      assign(url) { next = url; navigation.push(url); },
    };
    const anchors = links.map((href) => ({
      href,
      scrollIntoView() {},
      click() { assert.equal(this.target, '_self'); clicks.push(href); next = href; navigation.push(href); },
    }));
    const document = {
      title: challenge ? 'Just a moment' : 'NodeSeek',
      documentElement: { dataset: challenge ? {} : { nsautoUser: JSON.stringify(loggedIn ? 'member' : null) } },
      querySelector: () => null,
      querySelectorAll: () => anchors,
    };
    const math = Object.create(Math);
    math.random = () => 0;
    const context = vm.createContext({
      document, location, URL, Math: math,
      Date: { now: () => now },
      setTimeout(callback, ms) { assert.ok(ms >= 0); pauses.push(ms); now += ms; callback(); },
      sessionStorage: { getItem: (key) => storage.get(key), setItem: (key, value) => storage.set(key, value) },
      chrome: { runtime: { async sendMessage(msg) {
        if (msg.type === 'task') return task;
        reports.push(msg.data);
        return { ok: true };
      } } },
      fetch() { throw new Error('日间浏览不应调用签到或其他业务接口'); },
    });
    vm.runInContext(browseSource, context);
    await vm.runInContext(contentSource, context);
    return next;
  }
  return { page, task, storage, reports, navigation, clicks, pauses, now: () => now };
}

test('跨页面浏览保持同一截止时间，只打开帖子，时间到后结束', async () => {
  const s = session();
  let path = '/';
  for (let i = 0; i < 20; i++) {
    const next = await s.page(path);
    if (!next) break;
    path = next;
  }
  assert.equal(s.now(), 120_000);
  assert.equal(s.reports.at(-1).kind, 'result');
  assert.equal(s.reports.at(-1).ok, true);
  assert.ok(s.reports.some((r) => r.kind === 'browse_progress' && r.path.startsWith('/post-')));
  assert.ok(s.clicks.length >= 2);
  assert.deepEqual(s.clicks.slice(0, 2), ['/post-1-1', '/post-2-1']);
  assert.ok(s.navigation.every((p) => p === '/' || /^\/post-\d+-\d+$/.test(p)));
  assert.equal(s.reports.some((r) => r.kind === 'attendance' || r.kind === 'prepare_security_check'), false);
});

test('180 秒会话也不会因页面跳转延长', async () => {
  const s = session({ duration: 180_000 });
  let path = '/';
  for (let i = 0; i < 30; i++) {
    const next = await s.page(path);
    if (!next) break;
    path = next;
  }
  assert.equal(s.now(), 180_000);
  assert.equal(s.reports.at(-1).ok, true);
});

test('截止时间临近时不再点击新帖子', async () => {
  const s = session({ duration: 5000 });
  await s.page();
  assert.equal(s.now(), 5000);
  assert.deepEqual(s.navigation, []);
});

test('首页没有安全帖子链接时停止，不点击其他操作', async () => {
  const s = session({ links: ['https://outside.example/post-1-1', '/api/attendance', '/signIn.html',
    'javascript:alert(1)', '/post-1-1?action=delete', '/post-1-1#reply'] });
  await s.page();
  assert.deepEqual(s.clicks, []);
  assert.equal(s.reports.at(-1).ok, false);
  assert.match(s.reports.at(-1).message, /未找到帖子/);
});

test('浏览遇到登录失效或验证时立即退出，不进行自动登录或签到', async () => {
  for (const options of [{ loggedIn: false }, { challenge: true }]) {
    const s = session(options);
    await s.page();
    assert.deepEqual(s.navigation, []);
    assert.equal(s.reports.at(-1).ok, false);
    assert.equal(s.reports.some((r) => r.kind === 'challenge'), false);
  }
});

test('缺少截止时间时不导航', async () => {
  const s = session();
  delete s.task.browse_until_ms;
  await s.page();
  assert.deepEqual(s.navigation, []);
  assert.equal(s.reports.at(-1).ok, false);
});

const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { test } = require('node:test');
const vm = require('node:vm');

const contentSource = readFileSync(`${__dirname}/../src/extension/content.js`, 'utf8');
const mainSource = readFileSync(`${__dirname}/../src/extension/main.js`, 'utf8');

async function run({ responses = [{ status: 200, body: '{"success":true}' }], challenge = false,
  title = '', mode = 'checkin', retried = false, bridge = true, networkError = false } = {}) {
  let now = 0;
  let reloads = 0;
  const reports = [];
  const requests = [];
  const listeners = new Map();
  const storage = new Map(retried ? [['nsauto-attendance-retry-task', '1']] : []);
  const document = {
    title,
    cookie: '', // Cookie 尚未生成时也不应无限等待。
    documentElement: { dataset: {} },
    querySelector: () => challenge ? {} : null,
    addEventListener: (type, listener) => listeners.set(type, listener),
    dispatchEvent(event) { listeners.get(event.type)?.(event); },
  };
  const config = challenge || title ? undefined : { user: { member_name: 'member' } };
  if (bridge) {
    vm.runInNewContext(mainSource, {
      document,
      window: { __config__: config },
      async fetch(url, options) {
        requests.push({ url, options, at: now });
        if (networkError) throw new Error('network unavailable');
        const r = responses[Math.min(requests.length - 1, responses.length - 1)];
        return { status: r.status, text: async () => r.body };
      },
    });
  } else if (config) {
    document.documentElement.dataset.nsautoUser = '"member"';
  }
  await vm.runInNewContext(contentSource, {
    document,
    Event: class { constructor(type) { this.type = type; } },
    chrome: { runtime: { async sendMessage(msg) {
      if (msg.type === 'task') return { id: 'task', mode, random: true };
      reports.push(msg.data);
      return { ok: true };
    } } },
    Date: { now: () => now },
    setTimeout(callback, ms) { now += ms; callback(); },
    location: { pathname: '/board', reload() { reloads++; } },
    sessionStorage: { getItem: (key) => storage.get(key), setItem: (key, value) => storage.set(key, value) },
    fetch() { throw new Error('签到必须使用页面环境的 fetch'); },
  });
  return { reports, requests, reloads, storage };
}

test('没有可见 fog Cookie 时，通过桌面交互和页面环境正常签到', async () => {
  const { reports, requests } = await run();
  assert.deepEqual(reports.map((r) => r.kind), ['state', 'prepare_security_check', 'attendance']);
  assert.equal(requests.length, 1);
  assert.equal(requests[0].url, '/api/attendance?random=true');
  assert.equal(requests[0].options.method, 'POST');
  assert.equal(requests[0].options.credentials, 'same-origin');
  assert.ok(requests[0].at >= 20_000);
  assert.equal(reports.at(-1).status, 200);
});

test('安全检查拦截时刷新页面，每个任务只自动刷新一次', async () => {
  const { reports, reloads, storage } = await run({
    responses: [{ status: 403, body: 'NodeSeek | security check' }],
  });
  assert.equal(reloads, 1);
  assert.equal(storage.get('nsauto-attendance-retry-task'), '1');
  assert.equal(reports.some((r) => r.kind === 'attendance'), false);
});

test('刷新后仍被拦截则提供人工入口，之后自动重试成功', async () => {
  const { reports, requests, reloads } = await run({ retried: true, responses: [
    { status: 403, body: 'NodeSeek | security check' },
    { status: 200, body: '{"success":true}' },
  ] });
  assert.deepEqual(reports.map((r) => r.kind), ['state', 'prepare_security_check', 'challenge', 'attendance']);
  assert.equal(reports[2].challenge, 'security_check');
  assert.equal(requests.length, 2);
  assert.ok(requests[1].at - requests[0].at >= 15_000);
  assert.equal(reloads, 0);
  assert.equal(reports.at(-1).status, 200);
});

test('页面请求网络错误能回传失败结果', async () => {
  const { reports } = await run({ networkError: true });
  assert.equal(reports.at(-1).kind, 'result');
  assert.equal(reports.at(-1).ok, false);
  assert.match(reports.at(-1).message, /network unavailable/);
});

test('页面桥接脚本未加载时有超时结果', async () => {
  const { reports, requests } = await run({ bridge: false });
  assert.equal(requests.length, 0);
  assert.equal(reports.at(-1).ok, false);
  assert.match(reports.at(-1).message, /页面签到请求超时/);
});

test('Cloudflare 验证页不请求桌面交互或签到', async () => {
  const { reports, requests } = await run({ challenge: true });
  assert.deepEqual(reports.map((r) => r.kind), ['challenge']);
  assert.equal(reports[0].challenge, 'cloudflare');
  assert.equal(requests.length, 0);
});

test('仅凭标题也能识别 NodeSeek 整页检查并提供人工入口', async () => {
  const { reports, requests } = await run({ title: 'NodeSeek | security check' });
  assert.equal(reports[0].challenge, 'security_check');
  assert.equal(requests.length, 0);
});

test('手动登录模式不请求桌面交互或签到', async () => {
  const { reports, requests } = await run({ mode: 'login' });
  assert.deepEqual(reports.map((r) => r.kind), ['state']);
  assert.equal(requests.length, 0);
});

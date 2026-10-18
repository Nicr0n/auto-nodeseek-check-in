// 日间阅读仅在首页和帖子之间导航，不操作回复、点赞或签到按钮。
const readingDelay = (min, max) => min + Math.floor(Math.random() * (max - min + 1));

function readingPostLink(link) {
  try {
    const url = new URL(link.href, location.href);
    return url.origin === location.origin && !url.search && !url.hash
      && /^\/post-\d+-\d+(?:\.html)?$/.test(url.pathname);
  } catch {
    return false;
  }
}

async function browsePosts(task, report) {
  const remaining = () => task.browse_until_ms - Date.now();
  const pause = (ms) => sleep(Math.max(0, Math.min(ms, remaining())));
  const finish = () => report({ kind: 'result', ok: true, message: '日间浏览时间已到' });
  if (!Number.isFinite(task.browse_until_ms)) {
    return report({ kind: 'result', ok: false, message: '日间浏览缺少截止时间' });
  }
  if (remaining() <= 0) return finish();
  await report({ kind: 'browse_progress', path: location.pathname });

  const onPost = /^\/post-\d+-\d+(?:\.html)?$/.test(location.pathname);
  if (onPost) {
    // 在帖子上停留阅读，系统滚轮由后端驱动；页面切换不重置总时长。
    await pause(readingDelay(35_000, 55_000));
    if (remaining() <= 0) return finish();
    location.assign('/');
    return;
  }

  await pause(readingDelay(8_000, 15_000));
  if (remaining() <= 0) return finish();
  const deadline = Math.min(task.browse_until_ms, Date.now() + 10_000);
  let links;
  do {
    links = Array.from(document.querySelectorAll('a[href]')).filter(readingPostLink);
    if (links.length || Date.now() >= deadline) break;
    await pause(500);
  } while (remaining() > 0);
  if (remaining() <= 0) return finish();
  if (!links.length) {
    return report({ kind: 'result', ok: false, message: '首页未找到帖子链接，结束本次浏览' });
  }

  const key = `nsauto-browse-visited-${task.id}`;
  let visited;
  try { visited = JSON.parse(sessionStorage.getItem(key) || '[]'); } catch { visited = []; }
  if (!Array.isArray(visited)) visited = [];
  const unread = links.filter((link) => !visited.includes(new URL(link.href, location.href).pathname));
  const candidates = unread.length ? unread : links;
  const link = candidates[readingDelay(0, candidates.length - 1)];
  const path = new URL(link.href, location.href).pathname;
  visited.push(path);
  sessionStorage.setItem(key, JSON.stringify(visited));
  link.scrollIntoView({ block: 'center' });
  await pause(readingDelay(800, 1800));
  if (remaining() <= 0) return finish();
  link.target = '_self';
  link.click();
}

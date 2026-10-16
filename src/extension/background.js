// 内容脚本受页面的跨域限制，与本地服务的通信统一经由这里转发。
const API = '__API__';

chrome.runtime.onMessage.addListener((msg, _sender, sendResponse) => {
  const req =
    msg.type === 'task'
      ? fetch(`${API}/internal/task`)
      : fetch(`${API}/internal/report`, {
          method: 'POST',
          headers: { 'content-type': 'application/json' },
          body: JSON.stringify(msg.data),
        });
  req
    .then((r) => r.json())
    .then(sendResponse, () => sendResponse(null));
  return true;
});

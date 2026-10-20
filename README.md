# NodeSeek 自动签到

Docker 容器中运行真实的 Chromium 浏览器：通过网页上的远程浏览器窗口（noVNC）手动完成一次人机验证和登录，之后登录状态保存在 `/data`，每天自动签到。支持多账号。

## 启动

```bash
docker compose pull      # 使用 CI 发布的镜像 ghcr.io/nicr0n/auto-nodeseek-check-in
docker compose up -d     # 或 docker compose up -d --build 在本地构建
```

打开 `http://<服务器>:8080`。管理页面本身没有认证，请通过 Cloudflare Tunnel + Access 等方式控制访问，不要直接暴露到公网。

## CI/CD

- `.github/workflows/ci.yml`：每次推送和 PR 都会检查格式、运行 clippy、构建镜像。推送到 `main` 时发布 `latest` 和 `sha-<提交>` 标签，推送 `v1.2.3` 这样的标签时另外发布 `1.2.3` 和 `1.2`。
- `deploy/`：服务器上的自动更新。systemd 定时器每 5 分钟执行一次 `update.sh`，拉取到新镜像就重建容器；浏览器正在签到或登录时跳过，下一轮再试。

在服务器上安装（在仓库目录中以 root 执行，部署目录为 `/opt/nodeseek-checkin`）：

```bash
mkdir -p /opt/nodeseek-checkin
cp docker-compose.yml deploy/update.sh /opt/nodeseek-checkin/
cp deploy/nodeseek-checkin-update.service deploy/nodeseek-checkin-update.timer /etc/systemd/system/
systemctl daemon-reload
systemctl enable --now nodeseek-checkin-update.timer
```

## 使用

1. 点击「添加账号」。账号密码可选，填写后会在登录页自动预填，并在登录失效时尝试自动重新登录。
2. 点击账号的「登录」，页面上会出现远程浏览器窗口：完成 Cloudflare 验证、登录（如有两步验证也在这里输入）。
3. 登录成功后点击「完成并保存」。
4. 之后每天在 `CHECKIN_TIME` 后随机延迟 0~`CHECKIN_JITTER_MINUTES` 分钟依次为所有启用的账号签到。也可以手动点击「签到」或「全部签到」。

登录失效且自动登录失败时，账号会显示「需要登录」，重新执行第 2、3 步即可。

## 日间浏览

每个启用账号每天在 `TZ` 时区的 09:00–21:00 内随机安排一次浏览，持续 120–180 秒。程序使用账号已有的浏览器配置打开首页，随机选择帖子阅读，在帖子上停留约 35–55 秒，再返回首页选择其他帖子；阅读期间会移动系统鼠标并间歇滚动。

浏览任务与签到、手动登录共用浏览器锁，浏览器繁忙时等待；剩余时间不足以在 21:00 前完成则跳过。登录失效或遇到验证拦截时结束本次浏览。任务只打开帖子，不回复、点赞或触发签到。

每日计划保存在 `/data/reading-schedule.json`，重启后保留原定时间，每个账号每天最多尝试一次；管理页面显示下次浏览时间和进行中的浏览状态。接近 21:00 才启动服务或启用账号时，可能来不及安排当天浏览。

## 人工验证与通知

签到时如果 Cloudflare 要求点击「Verify you are human」，程序不会自动点击，而是：

1. 保持浏览器打开最多 30 分钟，管理页面上显示远程浏览器窗口；
2. 推送 Webhook 通知（在管理页面的「通知」中填写地址）。

在管理页面完成验证后，签到会自动继续。签到失败（包括登录失效）时也会推送通知。

NodeSeek 的跨域 fog 脚本会采集鼠标或触摸轨迹，页面打开约 10 秒后通过 `/edge-cgi/fog` WebSocket 获取计算题，完成 PoW 后向 `document.cookie` 写入有效期 1 天的 `fog` Cookie。这与站点 Service Worker 的虚拟 Cookie 是不同机制。签到时程序会通过 `xdotool` 在浏览器内容区域移动系统指针，等待 20 秒后由页面环境调用签到接口，保留站点原有的请求处理链路。遇到安全检查拦截时先刷新重试一次；仍未通过则显示远程浏览器窗口并发送通知，手动完成检查后程序会定期重试签到。Cloudflare 要求点击的验证仍需人工完成。

Docker 镜像已包含 `xdotool`；直接运行二进制时，需要额外安装它，并确保 `DISPLAY` 指向 Chromium 使用的 X11 显示器。

Webhook 以 JSON POST 发送：

```json
{
  "event": "need_verification",
  "title": "NodeSeek 签到需要人工验证",
  "text": "完整的通知文本，可直接转发",
  "account": "账号名称",
  "message": "详细说明",
  "url": "管理页面访问地址"
}
```

`event` 为 `need_verification`（需要人工验证）、`checkin_failed`（签到失败）或 `test`（测试）。

## 配置

| 环境变量 | 默认值 | 说明 |
| --- | --- | --- |
| `CHECKIN_TIME` | `00:10` | 每日签到时间（`TZ` 时区） |
| `CHECKIN_JITTER_MINUTES` | `30` | 随机延迟的最大分钟数 |
| `TZ` | `Asia/Shanghai` | 时区 |

## 数据

`/data/accounts.json` 保存账号设置（密码为明文），`/data/profiles/<id>/` 是各账号的浏览器配置目录（包含登录状态）。请妥善保管该目录。

## 注意

- 管理页面没有内置认证，访问控制交给 Cloudflare Access（或其他反向代理）。
- 同一时间只运行一个浏览器：登录窗口打开期间签到会排队，登录窗口 20 分钟未完成会自动关闭。

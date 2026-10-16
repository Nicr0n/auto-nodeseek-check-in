# NodeSeek 自动签到

Docker 容器中运行真实的 Chromium 浏览器：通过网页上的远程浏览器窗口（noVNC）手动完成一次人机验证和登录，之后登录状态保存在 `/data`，每天自动签到。支持多账号。

## 启动

```bash
# 设置管理页面密码（.env 不会提交到 Git）
echo 'ADMIN_PASSWORD=你的密码' > .env
docker compose up -d --build
```

打开 `http://<服务器>:8080`，用户名任意，密码为 `ADMIN_PASSWORD`。

## 使用

1. 点击「添加账号」。账号密码可选，填写后会在登录页自动预填，并在登录失效时尝试自动重新登录。
2. 点击账号的「登录」，页面上会出现远程浏览器窗口：完成 Cloudflare 验证、登录（如有两步验证也在这里输入）。
3. 登录成功后点击「完成并保存」。
4. 之后每天在 `CHECKIN_TIME` 后随机延迟 0~`CHECKIN_JITTER_MINUTES` 分钟依次为所有启用的账号签到。也可以手动点击「签到」或「全部签到」。

登录失效且自动登录失败时，账号会显示「需要登录」，重新执行第 2、3 步即可。

## 人工验证与通知

签到时如果 Cloudflare 要求点击「Verify you are human」，程序不会自动点击，而是：

1. 保持浏览器打开最多 30 分钟，管理页面上显示远程浏览器窗口；
2. 推送 Webhook 通知（在管理页面的「通知」中填写地址）。

在管理页面完成验证后，签到会自动继续。签到失败（包括登录失效）时也会推送通知。

NodeSeek 自己的安全检查（页面打开约 10 秒后在后台完成，通过后才能调用签到接口）如果 30 秒内没有通过，也按上面的方式处理：在远程浏览器窗口中的 NodeSeek 页面上移动几下鼠标即可。程序不会伪造鼠标操作。

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
| `ADMIN_PASSWORD` | 无 | 管理页面密码。不设置则无需密码，**不要这样暴露到公网** |
| `CHECKIN_TIME` | `00:10` | 每日签到时间（`TZ` 时区） |
| `CHECKIN_JITTER_MINUTES` | `30` | 随机延迟的最大分钟数 |
| `TZ` | `Asia/Shanghai` | 时区 |

## 数据

`/data/accounts.json` 保存账号设置（密码为明文），`/data/profiles/<id>/` 是各账号的浏览器配置目录（包含登录状态）。请妥善保管该目录。

## 注意

- 管理页面使用 HTTP Basic 认证，暴露到公网时请放在 HTTPS 反向代理之后。
- 同一时间只运行一个浏览器：登录窗口打开期间签到会排队，登录窗口 20 分钟未完成会自动关闭。

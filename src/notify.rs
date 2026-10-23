//! 通过 Discord Webhook 推送签到结果和需要手动处理的事件。
//!
//! 每条通知是一个 embed：标题和颜色由事件决定，账号、鸡腿等信息放在字段中。

use std::time::Duration;

use anyhow::{Result, bail};
use chrono::{SecondsFormat, Utc};
use reqwest::{StatusCode, Url};
use serde_json::{Value, json};

use crate::store::Settings;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// 签到成功，获得了鸡腿
    CheckinSuccess,
    /// 今天已经签到过
    AlreadyCheckedIn,
    /// 签到失败
    CheckinFailed,
    /// 登录失效且无法自动登录
    LoginExpired,
    /// 签到时遇到需要人工处理的验证
    NeedVerification,
    /// 日间浏览出错
    BrowseFailed,
    /// 后台任务出错
    ServiceError,
    /// 设置页面的测试消息
    Test,
}

impl Event {
    fn title(self) -> &'static str {
        match self {
            Self::CheckinSuccess => "NodeSeek 签到成功",
            Self::AlreadyCheckedIn => "NodeSeek 今日已签到",
            Self::CheckinFailed => "NodeSeek 签到失败",
            Self::LoginExpired => "NodeSeek 登录已失效",
            Self::NeedVerification => "NodeSeek 签到需要人工验证",
            Self::BrowseFailed => "NodeSeek 日间浏览失败",
            Self::ServiceError => "NodeSeek 签到服务出错",
            Self::Test => "NodeSeek 签到通知测试",
        }
    }

    fn color(self) -> u32 {
        match self {
            Self::CheckinSuccess => 0x57f287,
            Self::AlreadyCheckedIn => 0x99aab5,
            Self::CheckinFailed | Self::LoginExpired | Self::ServiceError => 0xed4245,
            Self::NeedVerification => 0xfee75c,
            Self::BrowseFailed => 0xe67e22,
            Self::Test => 0x5865f2,
        }
    }

    /// 需要用户打开管理页面处理
    fn needs_action(self) -> bool {
        matches!(self, Self::LoginExpired | Self::NeedVerification)
    }
}

pub struct Notification {
    pub event: Event,
    /// 正文，可以为空
    pub message: String,
    /// embed 字段：(名称, 值)
    pub fields: Vec<(&'static str, String)>,
}

impl Notification {
    pub fn new(event: Event, message: impl Into<String>) -> Self {
        Self {
            event,
            message: message.into(),
            fields: Vec::new(),
        }
    }

    /// 添加字段；值为空时忽略（Discord 不接受空字段）。
    pub fn field(mut self, name: &'static str, value: impl Into<String>) -> Self {
        let value = value.into();
        if !value.trim().is_empty() {
            self.fields.push((name, value));
        }
        self
    }
}

/// 检查是否为 Discord Webhook 地址。
pub fn check_webhook_url(url: &str) -> Result<()> {
    let ok = Url::parse(url).is_ok_and(|u| {
        u.scheme() == "https"
            && matches!(
                u.host_str(),
                Some("discord.com" | "discordapp.com" | "ptb.discord.com" | "canary.discord.com")
            )
            && u.path().starts_with("/api/webhooks/")
    });
    if !ok {
        bail!("请填写 Discord Webhook 地址（https://discord.com/api/webhooks/...）");
    }
    Ok(())
}

/// 按字符截断，避免超出 Discord 的长度限制导致整条消息被拒绝。
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max - 1).collect();
    out.push('…');
    out
}

fn payload(n: &Notification, public_url: Option<&str>) -> Value {
    let mut description = n.message.clone();
    if let Some(url) = public_url.filter(|_| n.event.needs_action()) {
        if !description.is_empty() {
            description.push_str("\n\n");
        }
        description.push_str(&format!("[打开管理页面]({url})"));
    }
    let fields: Vec<_> = n
        .fields
        .iter()
        .map(
            |(name, value)| json!({ "name": name, "value": truncate(value, 1024), "inline": true }),
        )
        .collect();

    let mut embed = json!({
        "title": n.event.title(),
        "color": n.event.color(),
        "fields": fields,
        "footer": { "text": "NodeSeek 自动签到" },
        "timestamp": Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
    });
    if !description.is_empty() {
        embed["description"] = truncate(&description, 4096).into();
    }
    if let Some(url) = public_url {
        embed["url"] = url.into();
    }
    // 消息内容来自外部（如接口报错），禁止触发任何 @ 提及
    json!({ "embeds": [embed], "allowed_mentions": { "parse": [] } })
}

pub struct Notifier {
    client: reqwest::Client,
}

impl Notifier {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .expect("创建 HTTP 客户端失败");
        Self { client }
    }

    /// 发送通知。未配置 Webhook 时什么也不做。
    pub async fn send(&self, settings: &Settings, n: &Notification) -> Result<()> {
        let webhook = settings.discord_webhook_url.trim();
        if webhook.is_empty() {
            return Ok(());
        }
        let public_url = Some(settings.public_url.trim()).filter(|u| !u.is_empty());
        let payload = payload(n, public_url);

        let mut retried = false;
        loop {
            let resp = self.client.post(webhook).json(&payload).send().await?;
            let status = resp.status();
            if status.is_success() {
                return Ok(());
            }
            let body = resp.text().await.unwrap_or_default();
            // 被限流时按 Discord 给出的 retry_after（秒）等待后重试一次
            if status == StatusCode::TOO_MANY_REQUESTS && !retried {
                retried = true;
                let wait = serde_json::from_str::<Value>(&body)
                    .ok()
                    .and_then(|v| v["retry_after"].as_f64())
                    .unwrap_or(1.0);
                tokio::time::sleep(Duration::from_secs_f64(wait.clamp(0.5, 30.0))).await;
                continue;
            }
            let body: String = body.chars().take(200).collect();
            bail!("Discord Webhook 返回 HTTP {status}：{body}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_discord_webhooks() {
        for url in [
            "https://discord.com/api/webhooks/123/abc",
            "https://discordapp.com/api/webhooks/123/abc",
            "https://canary.discord.com/api/webhooks/123/abc",
        ] {
            assert!(check_webhook_url(url).is_ok(), "{url}");
        }
        for url in [
            "http://discord.com/api/webhooks/123/abc",
            "https://discord.com/channels/123",
            "https://evil.com/api/webhooks/123/abc",
            "https://discord.com.evil.com/api/webhooks/123/abc",
            "discord.com/api/webhooks/123/abc",
        ] {
            assert!(check_webhook_url(url).is_err(), "{url}");
        }
    }

    #[test]
    fn payload_skips_empty_fields_and_links_admin_page() {
        let n = Notification::new(Event::NeedVerification, "请完成验证")
            .field("账号", "main")
            .field("NodeSeek 用户", "");
        let p = payload(&n, Some("https://checkin.example.com"));
        let embed = &p["embeds"][0];
        assert_eq!(embed["title"], "NodeSeek 签到需要人工验证");
        assert_eq!(embed["url"], "https://checkin.example.com");
        assert_eq!(
            embed["description"],
            "请完成验证\n\n[打开管理页面](https://checkin.example.com)"
        );
        assert_eq!(embed["fields"].as_array().unwrap().len(), 1);
        assert_eq!(p["allowed_mentions"]["parse"], json!([]));

        let n = Notification::new(Event::CheckinSuccess, "").field("获得鸡腿", "5");
        let embed = &payload(&n, None)["embeds"][0];
        assert!(embed.get("description").is_none());
        assert!(embed.get("url").is_none());
    }

    #[test]
    fn long_text_is_truncated() {
        let n = Notification::new(Event::CheckinFailed, "错".repeat(5000))
            .field("原因", "x".repeat(2000));
        let embed = &payload(&n, None)["embeds"][0];
        assert_eq!(embed["description"].as_str().unwrap().chars().count(), 4096);
        assert_eq!(
            embed["fields"][0]["value"]
                .as_str()
                .unwrap()
                .chars()
                .count(),
            1024
        );
    }
}

//! 通过 Webhook 推送需要手动处理的事件。
//!
//! 以 JSON POST 到用户配置的地址，同时提供结构化字段和可直接展示的 `text`，
//! 方便各种机器人按需取用。

use std::time::Duration;

use anyhow::{Result, bail};
use serde::Serialize;

use crate::store::Settings;

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Event {
    /// 签到时遇到需要人工点击的 Cloudflare 验证
    NeedVerification,
    /// 签到失败（包括登录失效）
    CheckinFailed,
    /// 设置页面的测试消息
    Test,
}

#[derive(Serialize)]
struct Payload<'a> {
    event: Event,
    title: &'a str,
    text: String,
    account: Option<&'a str>,
    message: &'a str,
    url: Option<&'a str>,
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
    pub async fn send(
        &self,
        settings: &Settings,
        event: Event,
        title: &str,
        account: Option<&str>,
        message: &str,
    ) -> Result<()> {
        let webhook = settings.webhook_url.trim();
        if webhook.is_empty() {
            return Ok(());
        }
        let url = Some(settings.public_url.trim()).filter(|u| !u.is_empty());

        let mut text = String::from(title);
        if let Some(account) = account {
            text.push_str(&format!("\n账号：{account}"));
        }
        text.push_str(&format!("\n{message}"));
        if let Some(url) = url {
            text.push_str(&format!("\n管理页面：{url}"));
        }

        let payload = Payload { event, title, text, account, message, url };
        let resp = self.client.post(webhook).json(&payload).send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body: String = resp.text().await.unwrap_or_default().chars().take(200).collect();
            bail!("Webhook 返回 HTTP {status}：{body}");
        }
        Ok(())
    }
}

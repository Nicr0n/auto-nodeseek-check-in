//! 启动和关闭 Chromium。
//!
//! 不开启调试端口（`--remote-debugging-port` 会导致 Cloudflare 人机验证失败），
//! 页面上的操作全部由加载的扩展完成，见 `extension/`。

use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use anyhow::{Context, Result};
use tokio::{process::Child, time::timeout};

pub const EXTENSION_FILES: [(&str, &str); 5] = [
    ("manifest.json", include_str!("extension/manifest.json")),
    ("main.js", include_str!("extension/main.js")),
    ("background.js", include_str!("extension/background.js")),
    ("content.js", include_str!("extension/content.js")),
    ("browse.js", include_str!("extension/browse.js")),
];

#[derive(Clone, Debug)]
pub struct ChromeConfig {
    pub binary: String,
    pub display: String,
    pub extension_dir: PathBuf,
}

/// 把内置扩展写入 `dir`，`api` 为扩展回报结果的本地服务地址。
pub async fn install_extension(dir: &Path, api: &str) -> Result<()> {
    tokio::fs::create_dir_all(dir).await?;
    for (name, content) in EXTENSION_FILES {
        tokio::fs::write(dir.join(name), content.replace("__API__", api)).await?;
    }
    Ok(())
}

pub struct Chrome {
    child: Child,
}

impl Chrome {
    pub async fn launch(cfg: &ChromeConfig, profile: &Path, url: &str) -> Result<Self> {
        tokio::fs::create_dir_all(profile).await?;
        // 容器异常退出后残留的锁文件会让 Chrome 拒绝使用该配置目录
        for name in ["SingletonLock", "SingletonSocket", "SingletonCookie"] {
            let _ = tokio::fs::remove_file(profile.join(name)).await;
        }

        let child = tokio::process::Command::new(&cfg.binary)
            .arg(format!("--user-data-dir={}", profile.display()))
            .arg(format!("--load-extension={}", cfg.extension_dir.display()))
            .args([
                "--no-first-run",
                "--no-default-browser-check",
                "--hide-crash-restore-bubble",
                "--no-sandbox",
                "--disable-dev-shm-usage",
                "--password-store=basic",
                "--lang=zh-CN",
                "--window-position=0,0",
                "--window-size=1280,800",
            ])
            .arg(url)
            .env("DISPLAY", &cfg.display)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("无法启动浏览器 {}", cfg.binary))?;
        Ok(Self { child })
    }

    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// 在浏览器内容区域移动系统指针，让页面的交互检查能够运行。
    /// 不通过 DOM 派发事件，也不点击页面或 Cloudflare 验证控件。
    pub async fn prepare_security_check(&self, cfg: &ChromeConfig) -> Result<()> {
        for (x, y) in [(420, 300), (460, 320), (510, 350), (570, 370), (630, 360)] {
            let status = timeout(
                Duration::from_secs(3),
                tokio::process::Command::new("xdotool")
                    .args(["mousemove", &x.to_string(), &y.to_string()])
                    .env("DISPLAY", &cfg.display)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .kill_on_drop(true)
                    .status(),
            )
            .await
            .context("移动浏览器鼠标超时")?
            .context("无法运行 xdotool，请确认已安装")?;
            anyhow::ensure!(status.success(), "移动浏览器鼠标失败：{status}");
            // 检查脚本可能延迟加载，交互覆盖页面打开后的前十余秒。
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
        Ok(())
    }

    /// 阅读时在内容区域移动指针并滚动，按钮 4/5 是滚轮，不点击页面控件。
    pub async fn reading_interaction(&self, cfg: &ChromeConfig) -> Result<()> {
        let x = rand::random_range(350..=900).to_string();
        let y = rand::random_range(250..=650).to_string();
        let steps = rand::random_range(1..=3).to_string();
        let wheel = if rand::random_range(0..5) == 0 {
            "4"
        } else {
            "5"
        };
        let status = timeout(
            Duration::from_secs(3),
            tokio::process::Command::new("xdotool")
                .args([
                    "mousemove",
                    &x,
                    &y,
                    "click",
                    "--repeat",
                    &steps,
                    "--delay",
                    "150",
                    wheel,
                ])
                .env("DISPLAY", &cfg.display)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .status(),
        )
        .await
        .context("阅读时鼠标操作超时")?
        .context("无法运行 xdotool")?;
        anyhow::ensure!(status.success(), "阅读时鼠标操作失败：{status}");
        Ok(())
    }

    /// 发送 SIGTERM 让浏览器正常退出，确保 Cookie 等状态写回磁盘。
    pub async fn close(mut self) {
        if let Some(pid) = self.child.id() {
            let _ = tokio::process::Command::new("kill")
                .args(["-TERM", &pid.to_string()])
                .status()
                .await;
        }
        if timeout(Duration::from_secs(15), self.child.wait())
            .await
            .is_err()
        {
            tracing::warn!("浏览器未能正常退出，强制结束");
            let _ = self.child.kill().await;
        }
    }
}

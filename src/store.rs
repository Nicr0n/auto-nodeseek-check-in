//! 账号数据，持久化到 `<data>/accounts.json`。

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoginStatus {
    #[default]
    Unknown,
    LoggedIn,
    NeedLogin,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CheckinRecord {
    pub at: String,
    pub ok: bool,
    pub message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Account {
    pub id: String,
    pub name: String,
    /// 可选：用于登录失效时自动重新登录
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
    /// true 为“试试手气”，false 为固定 5 个鸡腿
    #[serde(default = "default_true")]
    pub random: bool,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub status: LoginStatus,
    #[serde(default)]
    pub nodeseek_user: Option<String>,
    #[serde(default)]
    pub last_checkin: Option<CheckinRecord>,
}

fn default_true() -> bool {
    true
}

/// 返回给前端的账号信息，不包含密码。
#[derive(Serialize)]
pub struct AccountView {
    pub id: String,
    pub name: String,
    pub username: String,
    pub has_password: bool,
    pub random: bool,
    pub enabled: bool,
    pub status: LoginStatus,
    pub nodeseek_user: Option<String>,
    pub last_checkin: Option<CheckinRecord>,
}

impl From<&Account> for AccountView {
    fn from(a: &Account) -> Self {
        Self {
            id: a.id.clone(),
            name: a.name.clone(),
            username: a.username.clone(),
            has_password: !a.password.is_empty(),
            random: a.random,
            enabled: a.enabled,
            status: a.status,
            nodeseek_user: a.nodeseek_user.clone(),
            last_checkin: a.last_checkin.clone(),
        }
    }
}

/// 全局设置，持久化到 `<data>/settings.json`。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Settings {
    /// 需要手动操作或签到失败时推送通知的 Webhook 地址
    #[serde(default)]
    pub webhook_url: String,
    /// 管理页面的访问地址，用于通知中的链接
    #[serde(default)]
    pub public_url: String,
}

pub struct Store {
    dir: PathBuf,
    accounts: Mutex<Vec<Account>>,
    settings: Mutex<Settings>,
}

impl Store {
    pub async fn open(dir: &Path) -> Result<Self> {
        tokio::fs::create_dir_all(dir.join("profiles")).await?;
        let file = dir.join("accounts.json");
        let accounts = match tokio::fs::read(&file).await {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e.into()),
        };
        let settings = match tokio::fs::read(dir.join("settings.json")).await {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Settings::default(),
            Err(e) => return Err(e.into()),
        };
        Ok(Self {
            dir: dir.to_path_buf(),
            accounts: Mutex::new(accounts),
            settings: Mutex::new(settings),
        })
    }

    pub async fn settings(&self) -> Settings {
        self.settings.lock().await.clone()
    }

    pub async fn set_settings(&self, new: Settings) -> Result<()> {
        let mut settings = self.settings.lock().await;
        write_atomic(&self.dir.join("settings.json"), &serde_json::to_vec_pretty(&new)?).await?;
        *settings = new;
        Ok(())
    }

    pub fn profile_dir(&self, id: &str) -> PathBuf {
        self.dir.join("profiles").join(id)
    }

    pub async fn list(&self) -> Vec<Account> {
        self.accounts.lock().await.clone()
    }

    pub async fn get(&self, id: &str) -> Option<Account> {
        self.accounts
            .lock()
            .await
            .iter()
            .find(|a| a.id == id)
            .cloned()
    }

    pub async fn insert(&self, account: Account) -> Result<()> {
        let mut accounts = self.accounts.lock().await;
        accounts.push(account);
        self.save(&accounts).await
    }

    /// 修改账号并保存。账号不存在时返回错误。
    pub async fn update(&self, id: &str, f: impl FnOnce(&mut Account)) -> Result<()> {
        let mut accounts = self.accounts.lock().await;
        let Some(a) = accounts.iter_mut().find(|a| a.id == id) else {
            bail!("账号不存在");
        };
        f(a);
        self.save(&accounts).await
    }

    pub async fn remove(&self, id: &str) -> Result<()> {
        let mut accounts = self.accounts.lock().await;
        let before = accounts.len();
        accounts.retain(|a| a.id != id);
        if accounts.len() == before {
            bail!("账号不存在");
        }
        self.save(&accounts).await?;
        drop(accounts);
        let _ = tokio::fs::remove_dir_all(self.profile_dir(id)).await;
        Ok(())
    }

    async fn save(&self, accounts: &[Account]) -> Result<()> {
        write_atomic(&self.dir.join("accounts.json"), &serde_json::to_vec_pretty(accounts)?).await
    }
}

async fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    tokio::fs::write(&tmp, data).await?;
    tokio::fs::rename(&tmp, path).await?;
    Ok(())
}

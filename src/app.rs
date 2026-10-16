//! 应用状态：账号签到、手动登录会话，以及浏览器的独占使用。
//!
//! 浏览器中的扩展通过 `/internal/task` 获取当前任务，通过 `/internal/report` 汇报
//! 登录状态和签到结果。同一时间只运行一个浏览器，因此同一时间只有一个任务。

use std::{
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use tokio::{
    sync::{Mutex, OwnedMutexGuard},
    time::{Instant, sleep},
};

use crate::{
    browser::{Chrome, ChromeConfig},
    notify::{Event, Notifier},
    store::{Account, CheckinRecord, LoginStatus, Store},
};

const BASE: &str = "https://www.nodeseek.com";
/// 手动登录窗口最长保持时间
const LOGIN_SESSION_TIMEOUT: Duration = Duration::from_secs(20 * 60);
/// 单个账号签到（含可能的自动登录）最长时间
const CHECKIN_TIMEOUT: Duration = Duration::from_secs(4 * 60);
/// 遇到需要人工点击的 Cloudflare 验证时，等待人工处理的最长时间
const VERIFICATION_TIMEOUT: Duration = Duration::from_secs(30 * 60);

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskMode {
    /// 签到；登录失效且保存了密码时尝试自动登录
    Checkin,
    /// 手动登录：只预填账号密码
    Login,
}

/// 下发给浏览器扩展的任务
#[derive(Clone, Debug, Serialize)]
pub struct Task {
    id: String,
    #[serde(skip)]
    account_id: String,
    mode: TaskMode,
    random: bool,
    username: String,
    password: String,
}

/// 扩展汇报的内容
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Report {
    State {
        id: String,
        user: Option<String>,
    },
    Attendance {
        id: String,
        status: u16,
        body: String,
    },
    Result {
        id: String,
        ok: bool,
        message: String,
        #[serde(default)]
        need_login: bool,
    },
    /// 页面停在需要人工处理的验证上
    Challenge {
        id: String,
        #[serde(default)]
        challenge: ChallengeKind,
    },
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChallengeKind {
    /// Cloudflare 的「Verify you are human」
    #[default]
    Cloudflare,
    /// NodeSeek 自己的安全检查（fog Cookie）没能自动通过
    SecurityCheck,
}

impl ChallengeKind {
    /// 遇到的验证和需要用户做的操作
    fn describe(self) -> (&'static str, &'static str) {
        match self {
            Self::Cloudflare => ("签到时遇到了 Cloudflare 人机验证", "完成验证"),
            Self::SecurityCheck => (
                "签到时 NodeSeek 安全检查未能自动通过",
                "在 NodeSeek 页面上移动几下鼠标",
            ),
        }
    }
}

#[derive(Clone, Debug)]
struct Outcome {
    ok: bool,
    message: String,
    need_login: bool,
}

struct Running {
    task: Task,
    /// 最近一次汇报的登录用户：None 为尚未汇报，Some(None) 为未登录
    user: Option<Option<String>>,
    outcome: Option<Outcome>,
    /// 签到时首次遇到需要人工处理的验证的时间
    challenge_since: Option<Instant>,
    /// 最近一次汇报的验证类型
    challenge: ChallengeKind,
}

pub struct App {
    pub store: Store,
    pub chrome: ChromeConfig,
    notifier: Notifier,
    /// 同一时间只运行一个浏览器（共用一个虚拟显示器）
    browser_lock: Arc<Mutex<()>>,
    running: StdMutex<Option<Running>>,
    login: Mutex<Option<LoginSession>>,
    activity: StdMutex<Option<String>>,
    pub next_run: StdMutex<Option<DateTime<Local>>>,
}

struct LoginSession {
    account_id: String,
    chrome: Chrome,
    started: Instant,
    _guard: OwnedMutexGuard<()>,
}

/// 签到被验证拦住、等待人工处理
#[derive(Serialize)]
pub struct VerificationView {
    pub account_id: String,
    pub remaining_secs: u64,
    pub message: String,
}

#[derive(Serialize)]
pub struct LoginView {
    pub account_id: String,
    /// 检测到已登录时为 NodeSeek 用户名
    pub logged_in_as: Option<String>,
    pub remaining_secs: u64,
}

impl App {
    pub fn new(store: Store, chrome: ChromeConfig) -> Self {
        Self {
            store,
            chrome,
            notifier: Notifier::new(),
            browser_lock: Arc::new(Mutex::new(())),
            running: StdMutex::new(None),
            login: Mutex::new(None),
            activity: StdMutex::new(None),
            next_run: StdMutex::new(None),
        }
    }

    pub fn activity(&self) -> Option<String> {
        self.activity.lock().unwrap().clone()
    }

    fn set_activity(&self, s: Option<String>) {
        *self.activity.lock().unwrap() = s;
    }

    pub fn is_busy(&self) -> bool {
        self.browser_lock.try_lock().is_err()
    }

    fn try_lock_browser(&self) -> Result<OwnedMutexGuard<()>> {
        self.browser_lock
            .clone()
            .try_lock_owned()
            .map_err(|_| anyhow!("浏览器正在使用中（签到进行中或有未关闭的登录窗口）"))
    }

    // ---------- 与浏览器扩展的交互 ----------

    fn begin_task(&self, account: &Account, mode: TaskMode) {
        let task = Task {
            id: format!("{:016x}", rand::random::<u64>()),
            account_id: account.id.clone(),
            mode,
            random: account.random,
            username: account.username.clone(),
            password: account.password.clone(),
        };
        *self.running.lock().unwrap() = Some(Running {
            task,
            user: None,
            outcome: None,
            challenge_since: None,
            challenge: ChallengeKind::default(),
        });
    }

    fn end_task(&self) -> Option<Running> {
        self.running.lock().unwrap().take()
    }

    /// 当前未完成的任务。
    pub fn current_task(&self) -> Option<Task> {
        let running = self.running.lock().unwrap();
        running
            .as_ref()
            .filter(|r| r.outcome.is_none())
            .map(|r| r.task.clone())
    }

    pub fn handle_report(&self, report: Report) {
        let mut running = self.running.lock().unwrap();
        let Some(r) = running.as_mut() else { return };
        let id = match &report {
            Report::State { id, .. }
            | Report::Attendance { id, .. }
            | Report::Result { id, .. }
            | Report::Challenge { id, .. } => id,
        };
        if *id != r.task.id || r.outcome.is_some() {
            return;
        }
        match report {
            Report::State { user, .. } => r.user = Some(user),
            Report::Challenge { challenge, .. } => {
                r.challenge = challenge;
                if r.challenge_since.is_none() {
                    r.challenge_since = Some(Instant::now());
                }
            }
            Report::Attendance { status, body, .. } => {
                let (ok, message) = match parse_attendance(status, &body) {
                    Ok(m) => (true, m),
                    Err(m) => (false, m),
                };
                r.outcome = Some(Outcome {
                    ok,
                    message,
                    need_login: false,
                });
            }
            Report::Result {
                ok,
                message,
                need_login,
                ..
            } => {
                r.outcome = Some(Outcome {
                    ok,
                    message,
                    need_login,
                });
            }
        }
    }

    fn reported_user(&self) -> Option<String> {
        self.running
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|r| r.user.clone().flatten())
    }

    // ---------- 签到 ----------

    /// 在后台立即为单个账号签到（浏览器被占用时直接报错）。
    pub fn spawn_check_in(self: &Arc<Self>, id: String) -> Result<()> {
        let guard = self.try_lock_browser()?;
        let app = self.clone();
        tokio::spawn(async move {
            let _guard = guard;
            app.check_in_locked(&id).await;
        });
        Ok(())
    }

    /// 在后台为所有启用的账号签到。
    pub fn spawn_check_in_all(self: &Arc<Self>) -> Result<()> {
        if self.is_busy() {
            bail!("浏览器正在使用中（签到进行中或有未关闭的登录窗口）");
        }
        let app = self.clone();
        tokio::spawn(async move { app.check_in_all().await });
        Ok(())
    }

    /// 为所有启用的账号依次签到（会等待浏览器空闲）。
    pub async fn check_in_all(&self) {
        let _guard = self.browser_lock.lock().await;
        let accounts: Vec<_> = self
            .store
            .list()
            .await
            .into_iter()
            .filter(|a| a.enabled)
            .collect();
        for (i, a) in accounts.iter().enumerate() {
            if i > 0 {
                sleep(Duration::from_secs(rand::random_range(5..20))).await;
            }
            self.check_in_locked(&a.id).await;
        }
    }

    async fn check_in_locked(&self, id: &str) {
        let Some(account) = self.store.get(id).await else {
            return;
        };
        self.set_activity(Some(format!("正在签到：{}", account.name)));
        let outcome = match self.run_check_in(&account).await {
            Ok(o) => o,
            Err(e) => Outcome {
                ok: false,
                message: format!("{e:#}"),
                need_login: false,
            },
        };
        self.set_activity(None);

        if outcome.ok {
            tracing::info!("[{}] {}", account.name, outcome.message);
        } else {
            tracing::warn!("[{}] {}", account.name, outcome.message);
        }
        if !outcome.ok {
            self.notify(
                Event::CheckinFailed,
                "NodeSeek 签到失败",
                Some(&account.name),
                &outcome.message,
            )
            .await;
        }
        let record = CheckinRecord {
            at: Local::now().to_rfc3339(),
            ok: outcome.ok,
            message: outcome.message,
        };
        let _ = self
            .store
            .update(id, |a| a.last_checkin = Some(record))
            .await;
    }

    async fn run_check_in(&self, account: &Account) -> Result<Outcome> {
        self.begin_task(account, TaskMode::Checkin);
        let profile = self.store.profile_dir(&account.id);
        let mut chrome =
            match Chrome::launch(&self.chrome, &profile, &format!("{BASE}/board")).await {
                Ok(c) => c,
                Err(e) => {
                    self.end_task();
                    return Err(e);
                }
            };

        let started = Instant::now();
        let mut notified = false;
        let outcome = loop {
            let (outcome, challenge_since, challenge) = {
                let running = self.running.lock().unwrap();
                let r = running.as_ref();
                (
                    r.and_then(|r| r.outcome.clone()),
                    r.and_then(|r| r.challenge_since),
                    r.map(|r| r.challenge).unwrap_or_default(),
                )
            };
            if let Some(o) = outcome {
                break Ok(o);
            }
            if !chrome.is_running() {
                break Err(anyhow!("浏览器意外退出"));
            }

            // 遇到需要人工处理的验证：通知用户，并延长等待时间
            let deadline = match challenge_since {
                Some(since) => {
                    if !notified {
                        notified = true;
                        self.set_activity(Some(format!("等待人工验证：{}", account.name)));
                        let (what, action) = challenge.describe();
                        self.notify(
                            Event::NeedVerification,
                            "NodeSeek 签到需要人工验证",
                            Some(&account.name),
                            &format!(
                                "{what}，请在 {} 分钟内打开管理页面，在浏览器窗口中{action}，通过后签到会自动继续。",
                                VERIFICATION_TIMEOUT.as_secs() / 60
                            ),
                        )
                        .await;
                    }
                    since + VERIFICATION_TIMEOUT
                }
                None => started + CHECKIN_TIMEOUT,
            };
            if Instant::now() > deadline {
                break Err(if challenge_since.is_some() {
                    anyhow!("等待人工验证超时")
                } else {
                    anyhow!("签到超时")
                });
            }
            sleep(Duration::from_millis(500)).await;
        };

        chrome.close().await;
        let running = self.end_task();
        let user = running.and_then(|r| r.user);

        match &outcome {
            Ok(o) if o.need_login => {
                self.set_status(&account.id, LoginStatus::NeedLogin, None)
                    .await;
            }
            _ => match user {
                Some(Some(name)) => {
                    self.set_status(&account.id, LoginStatus::LoggedIn, Some(name))
                        .await;
                }
                Some(None) => {
                    self.set_status(&account.id, LoginStatus::NeedLogin, None)
                        .await
                }
                None => {}
            },
        }
        outcome
    }

    /// 签到被验证拦住、等待人工处理时返回其信息。
    pub fn verification_view(&self) -> Option<VerificationView> {
        let running = self.running.lock().unwrap();
        let r = running.as_ref()?;
        if !matches!(r.task.mode, TaskMode::Checkin) || r.outcome.is_some() {
            return None;
        }
        let since = r.challenge_since?;
        let (what, action) = r.challenge.describe();
        Some(VerificationView {
            account_id: r.task.account_id.clone(),
            remaining_secs: VERIFICATION_TIMEOUT
                .saturating_sub(since.elapsed())
                .as_secs(),
            message: format!("{what}。请在下方浏览器窗口中{action}，通过后签到会自动继续。"),
        })
    }

    /// 放弃当前等待人工验证的签到。
    pub fn abort_verification(&self) -> Result<()> {
        let mut running = self.running.lock().unwrap();
        match running.as_mut() {
            Some(r) if r.challenge_since.is_some() && r.outcome.is_none() => {
                r.outcome = Some(Outcome {
                    ok: false,
                    message: "未完成人工验证，已放弃本次签到".into(),
                    need_login: false,
                });
                Ok(())
            }
            _ => bail!("当前没有等待人工验证的签到"),
        }
    }

    async fn notify(&self, event: Event, title: &str, account: Option<&str>, message: &str) {
        let settings = self.store.settings().await;
        if let Err(e) = self
            .notifier
            .send(&settings, event, title, account, message)
            .await
        {
            tracing::warn!("发送通知失败: {e:#}");
        }
    }

    /// 发送测试通知，返回错误以便在页面上显示。
    pub async fn notify_test(&self) -> Result<()> {
        let settings = self.store.settings().await;
        if settings.webhook_url.trim().is_empty() {
            bail!("请先填写 Webhook 地址");
        }
        self.notifier
            .send(
                &settings,
                Event::Test,
                "NodeSeek 签到通知测试",
                None,
                "收到这条消息说明 Webhook 配置正确。",
            )
            .await
    }

    async fn set_status(&self, id: &str, status: LoginStatus, user: Option<String>) {
        let _ = self
            .store
            .update(id, |a| {
                a.status = status;
                if user.is_some() {
                    a.nodeseek_user = user;
                }
            })
            .await;
    }

    // ---------- 手动登录 ----------

    /// 为账号打开登录页，用户通过 VNC 完成人机验证和登录。
    pub async fn start_login(&self, id: &str) -> Result<()> {
        let account = self.store.get(id).await.context("账号不存在")?;
        let guard = self.try_lock_browser()?;
        self.begin_task(&account, TaskMode::Login);
        let profile = self.store.profile_dir(id);
        let chrome =
            match Chrome::launch(&self.chrome, &profile, &format!("{BASE}/signIn.html")).await {
                Ok(c) => c,
                Err(e) => {
                    self.end_task();
                    return Err(e);
                }
            };

        self.set_activity(Some(format!("等待手动登录：{}", account.name)));
        *self.login.lock().await = Some(LoginSession {
            account_id: id.to_string(),
            chrome,
            started: Instant::now(),
            _guard: guard,
        });
        Ok(())
    }

    pub async fn login_view(&self) -> Option<LoginView> {
        let login = self.login.lock().await;
        let session = login.as_ref()?;
        Some(LoginView {
            account_id: session.account_id.clone(),
            logged_in_as: self.reported_user(),
            remaining_secs: LOGIN_SESSION_TIMEOUT
                .saturating_sub(session.started.elapsed())
                .as_secs(),
        })
    }

    /// 确认登录完成：保存登录状态并关闭浏览器。
    pub async fn finish_login(&self) -> Result<String> {
        let mut login = self.login.lock().await;
        if login.is_none() {
            bail!("没有进行中的登录");
        }
        let Some(user) = self.reported_user() else {
            bail!("还没有检测到登录成功，请先在浏览器中完成登录（登录后页面会自动跳转）");
        };
        let session = login.take().unwrap();
        drop(login);

        session.chrome.close().await;
        self.end_task();
        self.set_status(
            &session.account_id,
            LoginStatus::LoggedIn,
            Some(user.clone()),
        )
        .await;
        self.set_activity(None);
        Ok(user)
    }

    pub async fn cancel_login(&self) {
        let session = self.login.lock().await.take();
        if let Some(session) = session {
            session.chrome.close().await;
            self.end_task();
            self.set_activity(None);
        }
    }

    /// 关闭超时的登录窗口，避免一直占用浏览器导致定时签到无法执行。
    pub async fn reap_login_session(&self) {
        let expired = self
            .login
            .lock()
            .await
            .as_ref()
            .is_some_and(|s| s.started.elapsed() > LOGIN_SESSION_TIMEOUT);
        if expired {
            tracing::info!("登录窗口超时，已自动关闭");
            self.cancel_login().await;
        }
    }
}

/// 解析签到接口的响应。
fn parse_attendance(status: u16, body: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Resp {
        #[serde(default)]
        success: bool,
        #[serde(default)]
        message: String,
        gain: Option<i64>,
        current: Option<i64>,
    }

    let Ok(resp) = serde_json::from_str::<Resp>(body) else {
        if body.contains("security check") {
            return Err(format!("签到请求被 NodeSeek 安全检查拦截（HTTP {status}）"));
        }
        let snippet: String = body.chars().take(200).collect();
        return Err(format!("签到接口返回异常（HTTP {status}）：{snippet}"));
    };
    if resp.success {
        let n = |v: Option<i64>| v.map_or_else(|| "?".into(), |v| v.to_string());
        Ok(format!(
            "签到成功，获得 {} 个鸡腿，当前共 {} 个",
            n(resp.gain),
            n(resp.current)
        ))
    } else if resp.message.contains("已完成签到") || resp.message.contains("重复") {
        Ok(format!("今日已签到：{}", resp.message))
    } else {
        Err(format!("签到失败（HTTP {status}）：{}", resp.message))
    }
}

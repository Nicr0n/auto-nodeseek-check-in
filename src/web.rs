//! 管理页面、HTTP API 和 VNC 的 WebSocket 代理。

use std::{net::SocketAddr, sync::Arc};

use anyhow::anyhow;
use axum::{
    Json, Router,
    body::Bytes,
    extract::{
        ConnectInfo, Path, Request, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::StatusCode,
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};
use tower_http::services::ServeDir;

use crate::{
    app::{App, Report},
    store::{Account, AccountView, LoginStatus, Settings},
};

pub struct WebConfig {
    pub vnc_addr: String,
    pub novnc_dir: String,
    pub checkin_time: String,
}

#[derive(Clone)]
struct Ctx {
    app: Arc<App>,
    cfg: Arc<WebConfig>,
}

pub fn router(app: Arc<App>, cfg: WebConfig) -> Router {
    let novnc = ServeDir::new(&cfg.novnc_dir);
    let ctx = Ctx {
        app,
        cfg: Arc::new(cfg),
    };
    Router::new()
        .route("/", get(index))
        .route("/api/state", get(state))
        .route("/internal/task", get(internal_task))
        .route("/internal/report", post(internal_report))
        .route("/api/accounts", post(create_account))
        .route(
            "/api/accounts/{id}",
            axum::routing::put(update_account).delete(delete_account),
        )
        .route("/api/accounts/{id}/checkin", post(check_in))
        .route("/api/accounts/{id}/login", post(start_login))
        .route("/api/checkin", post(check_in_all))
        .route("/api/login/finish", post(finish_login))
        .route("/api/login/cancel", post(cancel_login))
        .route("/api/verification/abort", post(abort_verification))
        .route("/api/settings", axum::routing::put(update_settings))
        .route("/api/settings/test", post(test_settings))
        // noVNC 的不同版本会把 path 参数解析为相对根目录或相对 vnc.html 所在目录
        .route("/vnc-ws", get(vnc_ws))
        .route("/novnc/vnc-ws", get(vnc_ws))
        .nest_service("/novnc", novnc)
        .layer(middleware::from_fn(internal_only))
        .with_state(ctx)
}

struct ApiError(anyhow::Error);

impl<E: Into<anyhow::Error>> From<E> for ApiError {
    fn from(e: E) -> Self {
        Self(e.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": format!("{:#}", self.0) })),
        )
            .into_response()
    }
}

type ApiResult<T = Json<Value>> = Result<T, ApiError>;

fn ok() -> ApiResult {
    Ok(Json(json!({ "ok": true })))
}

/// 浏览器扩展使用的内部接口只允许容器内部访问；管理页面的访问控制交给前面的 cloudflared
async fn internal_only(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
    next: Next,
) -> Response {
    if req.uri().path().starts_with("/internal/") && !peer.ip().is_loopback() {
        return StatusCode::FORBIDDEN.into_response();
    }
    next.run(req).await
}

async fn index() -> Html<&'static str> {
    Html(include_str!("index.html"))
}

async fn state(State(ctx): State<Ctx>) -> ApiResult {
    let accounts: Vec<AccountView> = ctx.app.store.list().await.iter().map(Into::into).collect();
    let next_run = ctx.app.next_run.lock().unwrap().map(|t| t.to_rfc3339());
    let next_browse = ctx.app.next_browse.lock().unwrap().map(|t| t.to_rfc3339());
    Ok(Json(json!({
        "accounts": accounts,
        "login": ctx.app.login_view().await,
        "verification": ctx.app.verification_view(),
        "settings": ctx.app.store.settings().await,
        "activity": ctx.app.activity(),
        "busy": ctx.app.is_busy(),
        "next_run": next_run,
        "next_browse": next_browse,
        "checkin_time": ctx.cfg.checkin_time,
    })))
}

async fn internal_task(State(ctx): State<Ctx>) -> Json<Value> {
    let task = ctx.app.current_task();
    tracing::debug!("扩展获取任务: {}", task.is_some());
    Json(json!(task))
}

async fn internal_report(State(ctx): State<Ctx>, Json(report): Json<Report>) -> ApiResult {
    tracing::debug!("扩展汇报: {report:?}");
    ctx.app.handle_report(report);
    ok()
}

#[derive(Deserialize)]
struct AccountInput {
    name: Option<String>,
    username: Option<String>,
    /// None 表示不修改；空字符串表示清除
    password: Option<String>,
    random: Option<bool>,
    enabled: Option<bool>,
}

async fn create_account(State(ctx): State<Ctx>, Json(input): Json<AccountInput>) -> ApiResult {
    let name = input.name.unwrap_or_default().trim().to_string();
    if name.is_empty() {
        return Err(anyhow!("名称不能为空").into());
    }
    let id = format!("{:08x}", rand::random::<u32>());
    ctx.app
        .store
        .insert(Account {
            id: id.clone(),
            name,
            username: input.username.unwrap_or_default().trim().to_string(),
            password: input.password.unwrap_or_default(),
            random: input.random.unwrap_or(true),
            enabled: input.enabled.unwrap_or(true),
            status: LoginStatus::NeedLogin,
            nodeseek_user: None,
            last_checkin: None,
        })
        .await?;
    Ok(Json(json!({ "id": id })))
}

async fn update_account(
    State(ctx): State<Ctx>,
    Path(id): Path<String>,
    Json(input): Json<AccountInput>,
) -> ApiResult {
    ctx.app
        .store
        .update(&id, |a| {
            if let Some(v) = input
                .name
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
            {
                a.name = v;
            }
            if let Some(v) = input.username {
                a.username = v.trim().to_string();
            }
            if let Some(v) = input.password {
                a.password = v;
            }
            if let Some(v) = input.random {
                a.random = v;
            }
            if let Some(v) = input.enabled {
                a.enabled = v;
            }
        })
        .await?;
    ok()
}

async fn delete_account(State(ctx): State<Ctx>, Path(id): Path<String>) -> ApiResult {
    if ctx.app.is_busy() {
        return Err(anyhow!("浏览器正在使用中，请稍后再删除").into());
    }
    ctx.app.store.remove(&id).await?;
    ok()
}

async fn check_in(State(ctx): State<Ctx>, Path(id): Path<String>) -> ApiResult {
    ctx.app.spawn_check_in(id)?;
    ok()
}

async fn check_in_all(State(ctx): State<Ctx>) -> ApiResult {
    ctx.app.spawn_check_in_all()?;
    ok()
}

async fn start_login(State(ctx): State<Ctx>, Path(id): Path<String>) -> ApiResult {
    ctx.app.start_login(&id).await?;
    ok()
}

async fn finish_login(State(ctx): State<Ctx>) -> ApiResult {
    let user = ctx.app.finish_login().await?;
    Ok(Json(json!({ "user": user })))
}

async fn abort_verification(State(ctx): State<Ctx>) -> ApiResult {
    ctx.app.abort_verification()?;
    ok()
}

async fn update_settings(State(ctx): State<Ctx>, Json(input): Json<Settings>) -> ApiResult {
    let webhook = input.webhook_url.trim();
    if !webhook.is_empty() && !webhook.starts_with("http://") && !webhook.starts_with("https://") {
        return Err(anyhow!("Webhook 地址需要以 http:// 或 https:// 开头").into());
    }
    ctx.app
        .store
        .set_settings(Settings {
            webhook_url: webhook.to_string(),
            public_url: input.public_url.trim().to_string(),
        })
        .await?;
    ok()
}

async fn test_settings(State(ctx): State<Ctx>) -> ApiResult {
    ctx.app.notify_test().await?;
    ok()
}

async fn cancel_login(State(ctx): State<Ctx>) -> ApiResult {
    ctx.app.cancel_login().await;
    ok()
}

/// noVNC 的 WebSocket 与 x11vnc 的 TCP 端口之间的转发（相当于 websockify）。
async fn vnc_ws(State(ctx): State<Ctx>, ws: WebSocketUpgrade) -> Response {
    let addr = ctx.cfg.vnc_addr.clone();
    ws.protocols(["binary"])
        .on_upgrade(move |socket| async move {
            if let Err(e) = proxy_vnc(socket, &addr).await {
                tracing::debug!("VNC 连接结束: {e:#}");
            }
        })
}

async fn proxy_vnc(socket: WebSocket, addr: &str) -> anyhow::Result<()> {
    let tcp = TcpStream::connect(addr).await?;
    let (mut tcp_rx, mut tcp_tx) = tcp.into_split();
    let (mut ws_tx, mut ws_rx) = socket.split();

    let upstream = async {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = tcp_rx.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            ws_tx
                .send(Message::Binary(Bytes::copy_from_slice(&buf[..n])))
                .await?;
        }
        anyhow::Ok(())
    };
    let downstream = async {
        while let Some(msg) = ws_rx.next().await {
            match msg? {
                Message::Binary(data) => tcp_tx.write_all(&data).await?,
                Message::Text(text) => tcp_tx.write_all(text.as_bytes()).await?,
                Message::Close(_) => break,
                _ => {}
            }
        }
        anyhow::Ok(())
    };

    tokio::select! {
        r = upstream => r,
        r = downstream => r,
    }
}

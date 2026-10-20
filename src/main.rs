mod app;
mod browser;
mod notify;
mod reading;
mod store;
mod web;

use std::{env, net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

use anyhow::{Context, Result};
use chrono::{DateTime, Local, NaiveTime, TimeDelta};

use crate::{app::App, browser::ChromeConfig, store::Store, web::WebConfig};

fn env_or(key: &str, default: &str) -> String {
    env::var(key)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let data_dir = PathBuf::from(env_or("DATA_DIR", "/data"));
    let listen = env_or("LISTEN", "0.0.0.0:8080");
    let checkin_time = env_or("CHECKIN_TIME", "00:10");
    let time =
        NaiveTime::parse_from_str(&checkin_time, "%H:%M").context("CHECKIN_TIME 格式应为 HH:MM")?;
    let jitter_minutes: u32 = env_or("CHECKIN_JITTER_MINUTES", "30")
        .parse()
        .context("CHECKIN_JITTER_MINUTES 应为整数")?;

    let listener = tokio::net::TcpListener::bind(&listen).await?;
    let port = listener.local_addr()?.port();

    let store = Store::open(&data_dir).await?;
    let extension_dir = data_dir.join("extension");
    browser::install_extension(&extension_dir, &format!("http://127.0.0.1:{port}")).await?;
    let chrome = ChromeConfig {
        binary: env_or("CHROME_BIN", "chromium"),
        display: env_or("DISPLAY", ":99"),
        extension_dir,
    };
    let app = Arc::new(App::new(store, chrome));

    tokio::spawn(scheduler(app.clone(), time, jitter_minutes));
    tokio::spawn({
        let app = app.clone();
        let path = data_dir.join("reading-schedule.json");
        async move {
            if let Err(e) = reading::run(app, &path).await {
                tracing::error!("日间阅读调度停止：{e:#}");
            }
        }
    });
    tokio::spawn({
        let app = app.clone();
        async move {
            loop {
                tokio::time::sleep(Duration::from_secs(30)).await;
                app.reap_login_session().await;
            }
        }
    });

    let router = web::router(
        app,
        WebConfig {
            vnc_addr: env_or("VNC_ADDR", "127.0.0.1:5900"),
            novnc_dir: env_or("NOVNC_DIR", "/opt/novnc"),
            checkin_time,
        },
    );
    tracing::info!("管理页面: http://{listen}");
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}

async fn scheduler(app: Arc<App>, time: NaiveTime, jitter_minutes: u32) {
    loop {
        let next = next_run(time, jitter_minutes);
        tracing::info!("下次签到时间: {}", next.format("%Y-%m-%d %H:%M:%S"));
        *app.next_run.lock().unwrap() = Some(next);

        let wait = (next - Local::now()).to_std().unwrap_or_default();
        tokio::time::sleep(wait).await;
        app.check_in_all().await;
    }
}

/// 下一个 `time` 时刻（严格晚于现在），再加上随机延迟。
fn next_run(time: NaiveTime, jitter_minutes: u32) -> DateTime<Local> {
    let now = Local::now();
    let mut date = now.date_naive();
    let base = loop {
        if let Some(t) = date.and_time(time).and_local_timezone(Local).earliest()
            && t > now
        {
            break t;
        }
        date = date.succ_opt().expect("日期溢出");
    };
    let jitter = rand::random_range(0..=i64::from(jitter_minutes) * 60);
    base + TimeDelta::seconds(jitter)
}

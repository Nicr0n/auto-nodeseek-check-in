//! 每个启用账号每天安排一次日间阅读；计划落盘，服务重启后不重复执行。

use std::{path::Path, sync::Arc, time::Duration};

use anyhow::{Context, Result};
use chrono::{DateTime, Local, NaiveTime};
use serde::{Deserialize, Serialize};

use crate::{app::App, store::write_atomic};

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ReadingEvent {
    account_id: String,
    at: i64,
    duration_secs: u64,
    attempted: bool,
}

#[derive(Default, Deserialize, Serialize)]
struct ReadingPlan {
    date: String,
    events: Vec<ReadingEvent>,
}

fn window(now: DateTime<Local>) -> (DateTime<Local>, DateTime<Local>) {
    let date = now.date_naive();
    let at = |hour| {
        date.and_time(NaiveTime::from_hms_opt(hour, 0, 0).unwrap())
            .and_local_timezone(Local)
            .earliest()
            .expect("本地时区缺少日间阅读时间")
    };
    (at(9), at(21))
}

impl ReadingPlan {
    fn refresh(&mut self, now: DateTime<Local>, enabled: &[String]) -> bool {
        let date = now.date_naive().to_string();
        let mut changed = false;
        if self.date != date {
            self.date = date;
            self.events.clear();
            changed = true;
        }
        let (start, end) = window(now);
        let earliest = now.timestamp().max(start.timestamp());
        // 即使随机到最长的三分钟，也能在 21 点前结束阅读。
        let latest = end.timestamp() - 180;
        if earliest > latest {
            return changed;
        }
        for id in enabled {
            if self.events.iter().any(|e| e.account_id == *id) {
                continue;
            }
            self.events.push(ReadingEvent {
                account_id: id.clone(),
                at: rand::random_range(earliest..=latest),
                duration_secs: rand::random_range(120..=180),
                attempted: false,
            });
            changed = true;
        }
        changed
    }

    fn next(&self, enabled: &[String]) -> Option<usize> {
        self.events
            .iter()
            .enumerate()
            .filter(|(_, e)| !e.attempted && enabled.contains(&e.account_id))
            .min_by_key(|(_, e)| e.at)
            .map(|(i, _)| i)
    }

    fn next_time(&self, enabled: &[String]) -> Option<DateTime<Local>> {
        self.next(enabled)
            .and_then(|i| DateTime::from_timestamp(self.events[i].at, 0))
            .map(|t| t.with_timezone(&Local))
    }

    async fn save(&self, path: &Path) -> Result<()> {
        write_atomic(path, &serde_json::to_vec_pretty(self)?).await
    }
}

pub async fn run(app: Arc<App>, path: &Path) -> Result<()> {
    let mut plan: ReadingPlan = match tokio::fs::read(path).await {
        Ok(bytes) => serde_json::from_slice(&bytes).context("日间阅读计划格式错误")?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => ReadingPlan::default(),
        Err(e) => return Err(e.into()),
    };
    loop {
        let now = Local::now();
        let enabled: Vec<_> = app
            .store
            .list()
            .await
            .into_iter()
            .filter(|a| a.enabled)
            .map(|a| a.id)
            .collect();
        if plan.refresh(now, &enabled) {
            plan.save(path).await?;
        }
        let next = plan.next(&enabled);
        *app.next_browse.lock().unwrap() = plan.next_time(&enabled);
        if let Some(i) = next
            && plan.events[i].at <= now.timestamp()
        {
            // 先落盘再运行，避免容器重启后重复打开浏览器。
            plan.events[i].attempted = true;
            plan.save(path).await?;
            *app.next_browse.lock().unwrap() = plan.next_time(&enabled);
            let event = plan.events[i].clone();
            tracing::info!(
                "开始日间浏览：账号 {}，计划时长 {} 秒",
                event.account_id,
                event.duration_secs
            );
            app.browse_account(
                &event.account_id,
                Duration::from_secs(event.duration_secs),
                window(now).1,
            )
            .await;
            continue;
        }
        tokio::time::sleep(Duration::from_secs(15)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, TimeZone};

    fn at(hour: u32, minute: u32) -> DateTime<Local> {
        Local
            .from_local_datetime(
                &NaiveDate::from_ymd_opt(2026, 10, 19)
                    .unwrap()
                    .and_hms_opt(hour, minute, 0)
                    .unwrap(),
            )
            .unwrap()
    }

    #[test]
    fn events_stay_in_window_and_duration_bounds() {
        for now in [at(1, 0), at(9, 0), at(15, 0), at(20, 56)] {
            let mut plan = ReadingPlan::default();
            let ids: Vec<_> = (0..100).map(|i| i.to_string()).collect();
            assert!(plan.refresh(now, &ids));
            assert_eq!(plan.events.len(), ids.len());
            for e in plan.events {
                assert!(e.at >= now.timestamp().max(at(9, 0).timestamp()));
                assert!(e.at + e.duration_secs as i64 <= at(21, 0).timestamp());
                assert!((120..=180).contains(&e.duration_secs));
            }
        }
        let mut plan = ReadingPlan::default();
        plan.refresh(at(20, 58), &["one".into()]);
        assert!(plan.events.is_empty());
    }

    #[test]
    fn restart_does_not_reschedule_or_repeat_attempted_events() {
        let ids = vec!["one".into()];
        let mut plan = ReadingPlan::default();
        plan.refresh(at(8, 0), &ids);
        let time = plan.events[0].at;
        let saved = serde_json::to_vec(&plan).unwrap();
        let mut restored: ReadingPlan = serde_json::from_slice(&saved).unwrap();
        assert!(!restored.refresh(at(10, 0), &ids));
        assert_eq!(restored.events[0].at, time);
        restored.events[0].attempted = true;
        let saved = serde_json::to_vec(&restored).unwrap();
        let mut restored: ReadingPlan = serde_json::from_slice(&saved).unwrap();
        assert!(!restored.refresh(at(11, 0), &ids));
        assert_eq!(restored.next(&ids), None);
        assert!(restored.next_time(&ids).is_none());
        let tomorrow = at(8, 0) + chrono::TimeDelta::days(1);
        restored.refresh(tomorrow, &ids);
        assert!(!restored.events[0].attempted);
    }

    #[test]
    fn disabled_accounts_are_not_run_and_new_accounts_get_a_plan() {
        let mut plan = ReadingPlan::default();
        plan.refresh(at(8, 0), &["one".into()]);
        assert_eq!(plan.next(&[]), None);
        let ids = vec!["one".into(), "two".into()];
        assert!(plan.refresh(at(12, 0), &ids));
        assert_eq!(plan.events.len(), 2);
        assert!(plan.events[1].at >= at(12, 0).timestamp());
        assert!(!plan.refresh(at(12, 0), &ids));
    }
}

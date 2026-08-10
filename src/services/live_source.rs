use crate::models::live_source;
use anyhow::{bail, Result};
use chrono::{Datelike, Local, NaiveTime, Timelike};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder, Set,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const WEEKDAYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CaptureMode {
    Off,
    #[default]
    Standard,
    Full,
}

impl CaptureMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Standard => "standard",
            Self::Full => "full",
        }
    }
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "off" => Ok(Self::Off),
            "standard" => Ok(Self::Standard),
            "full" => Ok(Self::Full),
            _ => bail!("互动采集模式必须是 off、standard 或 full"),
        }
    }
}

pub type WeeklySchedule = BTreeMap<String, Vec<String>>;

pub fn normalize_schedule(value: Option<WeeklySchedule>) -> Result<Option<String>> {
    let Some(value) = value else { return Ok(None) };
    let mut normalized = WeeklySchedule::new();
    for day in WEEKDAYS {
        let windows = value.get(day).cloned().unwrap_or_default();
        if windows.len() > 6 {
            bail!("每天最多配置 6 个录制时段");
        }
        let mut clean = Vec::with_capacity(windows.len());
        for window in windows {
            let (start, end) = parse_window(&window)?;
            clean.push(format!("{}-{}", start.format("%H:%M"), end.format("%H:%M")));
        }
        normalized.insert(day.to_owned(), clean);
    }
    Ok(Some(serde_json::to_string(&normalized)?))
}

pub fn schedule_from_json(value: Option<&str>) -> WeeklySchedule {
    value
        .and_then(|raw| serde_json::from_str(raw).ok())
        .unwrap_or_default()
}

pub fn schedule_is_active(schedule: Option<&str>, now: chrono::DateTime<Local>) -> bool {
    let Some(raw) = schedule else { return true };
    let schedule = schedule_from_json(Some(raw));
    let weekday = now.weekday().num_days_from_monday() as usize;
    let today = WEEKDAYS[weekday];
    let previous = WEEKDAYS[(weekday + 6) % 7];
    let current = NaiveTime::from_hms_opt(now.hour(), now.minute(), now.second()).unwrap();
    schedule.get(today).into_iter().flatten().any(|item| {
        parse_window(item)
            .map(|(s, e)| s <= e && current >= s && current < e)
            .unwrap_or(false)
    }) || schedule.get(previous).into_iter().flatten().any(|item| {
        parse_window(item)
            .map(|(s, e)| s > e && current < e)
            .unwrap_or(false)
    }) || schedule.get(today).into_iter().flatten().any(|item| {
        parse_window(item)
            .map(|(s, e)| s > e && current >= s)
            .unwrap_or(false)
    })
}

fn parse_window(value: &str) -> Result<(NaiveTime, NaiveTime)> {
    let (start, end) = value
        .trim()
        .split_once('-')
        .ok_or_else(|| anyhow::anyhow!("时段格式应为 HH:MM-HH:MM"))?;
    let start = NaiveTime::parse_from_str(start.trim(), "%H:%M")?;
    let end = NaiveTime::parse_from_str(end.trim(), "%H:%M")?;
    if start == end {
        bail!("时段的开始和结束时间不能相同");
    }
    Ok((start, end))
}

#[derive(Clone, Debug, Deserialize)]
pub struct NewLiveSource {
    pub room_id: i64,
    pub short_id: i64,
    pub uid: i64,
    pub anchor_name: String,
    #[serde(default)]
    pub face: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub cover: String,
    #[serde(default = "default_true")]
    pub auto_record_enabled: bool,
    pub weekly_schedule: Option<WeeklySchedule>,
    #[serde(default)]
    pub capture_mode: CaptureMode,
}

#[derive(Clone, Debug, Deserialize)]
pub struct UpdateLiveSource {
    pub room_id: i64,
    pub auto_record_enabled: Option<bool>,
    pub weekly_schedule: Option<WeeklySchedule>,
    pub clear_schedule: Option<bool>,
    pub capture_mode: Option<CaptureMode>,
}

fn default_true() -> bool {
    true
}

#[derive(Clone)]
pub struct LiveSourceService {
    db: DatabaseConnection,
}

impl LiveSourceService {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }
    pub async fn list(&self) -> Result<Vec<live_source::Model>> {
        Ok(live_source::Entity::find()
            .order_by_asc(live_source::Column::Id)
            .all(&self.db)
            .await?)
    }
    pub async fn find(&self, room_id: i64) -> Result<Option<live_source::Model>> {
        Ok(live_source::Entity::find()
            .filter(live_source::Column::RoomId.eq(room_id))
            .one(&self.db)
            .await?)
    }
    pub async fn add(&self, value: NewLiveSource) -> Result<live_source::Model> {
        if value.room_id <= 0 {
            bail!("直播间号必须为正整数");
        }
        if self.find(value.room_id).await?.is_some() {
            bail!("该直播源已存在");
        }
        let now = Local::now().to_rfc3339();
        let schedule = normalize_schedule(value.weekly_schedule)?;
        Ok(live_source::ActiveModel {
            room_id: Set(value.room_id),
            short_id: Set(value.short_id),
            uid: Set(value.uid),
            anchor_name: Set(value.anchor_name),
            face: Set(value.face),
            title: Set(value.title),
            cover: Set(value.cover),
            auto_record_enabled: Set(value.auto_record_enabled),
            weekly_schedule: Set(schedule),
            capture_mode: Set(value.capture_mode.as_str().to_owned()),
            manual_stop_latched: Set(false),
            created_at: Set(now.clone()),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(&self.db)
        .await?)
    }
    pub async fn update(&self, value: UpdateLiveSource) -> Result<live_source::Model> {
        let current = self
            .find(value.room_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("直播源不存在"))?;
        let mut model: live_source::ActiveModel = current.into();
        if let Some(enabled) = value.auto_record_enabled {
            model.auto_record_enabled = Set(enabled);
        }
        if value.clear_schedule.unwrap_or(false) {
            model.weekly_schedule = Set(None);
        } else if value.weekly_schedule.is_some() {
            model.weekly_schedule = Set(normalize_schedule(value.weekly_schedule)?);
        }
        if let Some(mode) = value.capture_mode {
            model.capture_mode = Set(mode.as_str().to_owned());
        }
        model.updated_at = Set(Local::now().to_rfc3339());
        Ok(model.update(&self.db).await?)
    }
    pub async fn delete(&self, room_id: i64) -> Result<bool> {
        Ok(live_source::Entity::delete_many()
            .filter(live_source::Column::RoomId.eq(room_id))
            .exec(&self.db)
            .await?
            .rows_affected
            > 0)
    }
    pub async fn set_manual_latch(&self, room_id: i64, value: bool) -> Result<()> {
        if let Some(current) = self.find(room_id).await? {
            let mut model: live_source::ActiveModel = current.into();
            model.manual_stop_latched = Set(value);
            model.updated_at = Set(Local::now().to_rfc3339());
            model.update(&self.db).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    #[test]
    fn schedule_supports_cross_midnight() {
        let mut schedule = WeeklySchedule::new();
        schedule.insert("mon".into(), vec!["23:00-02:00".into()]);
        let raw = normalize_schedule(Some(schedule)).unwrap().unwrap();
        assert!(schedule_is_active(
            Some(&raw),
            Local.with_ymd_and_hms(2026, 8, 10, 23, 30, 0).unwrap()
        ));
        assert!(schedule_is_active(
            Some(&raw),
            Local.with_ymd_and_hms(2026, 8, 11, 1, 30, 0).unwrap()
        ));
    }
    #[test]
    fn absent_schedule_means_all_day() {
        assert!(schedule_is_active(None, Local::now()));
    }
}

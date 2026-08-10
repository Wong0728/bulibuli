use chrono::{DateTime, Local};
use sea_orm::entity::prelude::*;
use serde::Serialize;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize)]
#[sea_orm(table_name = "logs")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub level: String,
    pub message: String,
    pub uid: Option<String>,
    /// 视频 BV 号（可选；用于抽屉"日志"区按 bvid 查询）
    #[sea_orm(nullable)]
    pub bvid: Option<String>,
    pub created_at: Option<DateTime<Local>>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

impl Model {
    pub fn to_api(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "level": self.level,
            "msg": self.message,
            "uid": self.uid,
            "bvid": self.bvid,
            "time": self.created_at.map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string()),
            "timestamp": self.created_at.map(|t| t.timestamp()).unwrap_or(0),
        })
    }
}

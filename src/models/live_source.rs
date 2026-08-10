use sea_orm::entity::prelude::*;
use serde::Serialize;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize)]
#[sea_orm(table_name = "live_sources")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    #[sea_orm(unique)]
    pub room_id: i64,
    pub short_id: i64,
    pub uid: i64,
    pub anchor_name: String,
    pub face: String,
    pub title: String,
    pub cover: String,
    pub auto_record_enabled: bool,
    #[sea_orm(column_type = "Text", nullable)]
    pub weekly_schedule: Option<String>,
    pub capture_mode: String,
    pub manual_stop_latched: bool,
    pub manual_stop_session_key: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

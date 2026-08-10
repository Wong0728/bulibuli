use anyhow::Result;
use sea_orm::DatabaseConnection;

use crate::config::Config;

pub async fn init_sources(_config: &Config, _conn: &DatabaseConnection) -> Result<(), anyhow::Error> {
    // 视频源现在通过Web API管理，无需初始化
    Ok(())
}

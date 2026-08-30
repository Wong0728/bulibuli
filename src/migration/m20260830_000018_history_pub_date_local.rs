use sea_orm::ConnectionTrait;
use sea_orm_migration::prelude::*;

/// 把 `history.pub_date` 回填为本机时区的日期。
///
/// 背景：写入侧此前用 `chrono::DateTime::from_timestamp(ts, 0)` 再 `format("%Y-%m-%d")`，
/// 而该表达式解析到的是 `DateTime<Utc>`（`DateTime` 的默认类型参数在这里不生效）。
/// 北京时间 00:00–07:59 投稿的记录因此少一天，看板按日期分组会把夜投视频归到前一天。
/// 写入侧已收敛到 `services::time_format::pub_date`，这里校正历史行。
///
/// `date(ts, 'unixepoch', 'localtime')` 与 Rust 侧同源于进程时区，两者一致性由
/// `db::migrations::tests` 里的对拍测试钉住。
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        // 只改写确实不一致的行：重复执行无副作用，也不会碰 version（乐观锁语义不变）。
        conn.execute_unprepared(
            "UPDATE history
             SET pub_date = date(pub_timestamp, 'unixepoch', 'localtime')
             WHERE pub_timestamp IS NOT NULL
               AND pub_timestamp > 0
               AND date(pub_timestamp, 'unixepoch', 'localtime') IS NOT NULL
               AND (pub_date IS NULL
                    OR pub_date <> date(pub_timestamp, 'unixepoch', 'localtime'))",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Migration(
            "pub_date 时区回填不可逆：原始 UTC 口径未保留".to_string(),
        ))
    }
}

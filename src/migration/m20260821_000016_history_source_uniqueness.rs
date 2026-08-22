use sea_orm::ConnectionTrait;
use sea_orm_migration::prelude::*;

/// 修复 history 唯一索引：去掉 source，改为 (bvid, COALESCE(cid, -1))。
///
/// 背景：add_to_history 的 source 晋升规则（auto 优先）意味着同一 (bvid, cid)
/// 只应有一条记录；旧索引包含 source 时，.one() 返回任意一条记录，
/// 更新 source 可能触发唯一冲突，且 video/audio 并发完成时会出现插入竞态。
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();

        // 1. 对现有重复记录按 (bvid, cid) 去重：
        //    - 优先保留已完成的（download_time IS NOT NULL）
        //    - 其次优先 source = 'auto' 的
        //    - 再优先 download_time 最新的、id 最大的
        // 2. 如果同组内存在 source = 'auto' 的记录，把保留记录的 source 提升为 'auto'。
        // 使用 CTE 而不是临时表，避免连接池/事务导致临时表不可见。
        conn.execute_unprepared(
            "WITH keep AS (
                 SELECT h.id,
                        (SELECT MAX(CASE WHEN h3.source = 'auto' THEN 1 ELSE 0 END)
                         FROM history h3
                         WHERE h3.bvid = h.bvid
                           AND COALESCE(h3.cid, -1) = COALESCE(h.cid, -1)) AS has_auto
                 FROM history h
                 WHERE h.id = (
                     SELECT h2.id
                     FROM history h2
                     WHERE h2.bvid = h.bvid
                       AND COALESCE(h2.cid, -1) = COALESCE(h.cid, -1)
                     ORDER BY
                       CASE WHEN h2.download_time IS NOT NULL THEN 0 ELSE 1 END,
                       CASE WHEN h2.source = 'auto' THEN 0 ELSE 1 END,
                       h2.download_time DESC,
                       h2.id DESC
                     LIMIT 1
                 )
             )
             DELETE FROM history
             WHERE id NOT IN (SELECT id FROM keep)",
        )
        .await?;

        conn.execute_unprepared(
            "WITH keep AS (
                 SELECT h.id,
                        (SELECT MAX(CASE WHEN h3.source = 'auto' THEN 1 ELSE 0 END)
                         FROM history h3
                         WHERE h3.bvid = h.bvid
                           AND COALESCE(h3.cid, -1) = COALESCE(h.cid, -1)) AS has_auto
                 FROM history h
                 WHERE h.id = (
                     SELECT h2.id
                     FROM history h2
                     WHERE h2.bvid = h.bvid
                       AND COALESCE(h2.cid, -1) = COALESCE(h.cid, -1)
                     ORDER BY
                       CASE WHEN h2.download_time IS NOT NULL THEN 0 ELSE 1 END,
                       CASE WHEN h2.source = 'auto' THEN 0 ELSE 1 END,
                       h2.download_time DESC,
                       h2.id DESC
                     LIMIT 1
                 )
             )
             UPDATE history
             SET source = 'auto'
             WHERE id IN (SELECT id FROM keep WHERE has_auto = 1)",
        )
        .await?;

        // 3. 替换唯一索引：去掉 source，确保同一 (bvid, cid) 只有一行。
        conn.execute_unprepared("DROP INDEX IF EXISTS uix_history_bvid_cid_source")
            .await?;
        conn.execute_unprepared(
            "CREATE UNIQUE INDEX IF NOT EXISTS uix_history_bvid_cid \
             ON history(bvid, COALESCE(cid, -1))",
        )
        .await?;

        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Migration(
            "history source uniqueness migration cannot be rolled back".to_string(),
        ))
    }
}

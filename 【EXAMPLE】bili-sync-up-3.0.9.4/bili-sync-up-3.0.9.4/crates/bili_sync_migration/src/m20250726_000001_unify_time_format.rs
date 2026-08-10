use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // 1. 修改所有 DateTime 类型字段为 String 类型
        // 注意：SQLite 不支持直接修改列类型，需要重建表

        // 1.1 重建 video_source 表
        manager
            .create_table(
                Table::create()
                    .table(VideoSourceNew::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(VideoSourceNew::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(VideoSourceNew::Name).string().not_null())
                    .col(ColumnDef::new(VideoSourceNew::Path).string().not_null())
                    .col(ColumnDef::new(VideoSourceNew::Type).integer().not_null())
                    .col(ColumnDef::new(VideoSourceNew::LatestRowAt).string().not_null())
                    .col(ColumnDef::new(VideoSourceNew::CreatedAt).string().not_null())
                    .col(ColumnDef::new(VideoSourceNew::SeasonId).string())
                    .col(ColumnDef::new(VideoSourceNew::MediaId).string())
                    .col(ColumnDef::new(VideoSourceNew::EpId).string())
                    .col(ColumnDef::new(VideoSourceNew::DownloadAllSeasons).boolean())
                    .col(ColumnDef::new(VideoSourceNew::VideoNameTemplate).string())
                    .col(ColumnDef::new(VideoSourceNew::PageNameTemplate).string())
                    .col(ColumnDef::new(VideoSourceNew::SelectedSeasons).string())
                    .col(
                        ColumnDef::new(VideoSourceNew::Enabled)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(
                        ColumnDef::new(VideoSourceNew::ScanDeletedVideos)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(ColumnDef::new(VideoSourceNew::CachedEpisodes).string())
                    .col(ColumnDef::new(VideoSourceNew::CacheUpdatedAt).string())
                    .to_owned(),
            )
            .await?;

        // 复制数据并转换时间格式
        let db = manager.get_connection();

        // 复制数据，同时转换时间格式
        db.execute_unprepared(
            r#"
            INSERT INTO video_source_new (
                id, name, path, type, latest_row_at, created_at, season_id, media_id, ep_id,
                download_all_seasons, video_name_template, page_name_template, selected_seasons,
                enabled, scan_deleted_videos, cached_episodes, cache_updated_at
            )
            SELECT 
                id, name, path, type,
                strftime('%Y-%m-%d %H:%M:%S', latest_row_at),
                created_at,
                season_id, media_id, ep_id,
                download_all_seasons, video_name_template, page_name_template, selected_seasons,
                enabled, scan_deleted_videos, cached_episodes,
                CASE 
                    WHEN cache_updated_at IS NOT NULL 
                    THEN strftime('%Y-%m-%d %H:%M:%S', cache_updated_at)
                    ELSE NULL
                END
            FROM video_source
            "#,
        )
        .await?;

        // 删除旧表并重命名新表
        manager
            .drop_table(Table::drop().table(VideoSource::Table).to_owned())
            .await?;
        db.execute_unprepared("ALTER TABLE video_source_new RENAME TO video_source")
            .await?;

        // 1.2 重建 submission 表
        manager
            .create_table(
                Table::create()
                    .table(SubmissionNew::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(SubmissionNew::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(SubmissionNew::UpperId)
                            .big_integer()
                            .not_null()
                            .unique_key(),
                    )
                    .col(ColumnDef::new(SubmissionNew::UpperName).string().not_null())
                    .col(ColumnDef::new(SubmissionNew::Path).string().not_null())
                    .col(ColumnDef::new(SubmissionNew::CreatedAt).string().not_null())
                    .col(ColumnDef::new(SubmissionNew::LatestRowAt).string().not_null())
                    .col(ColumnDef::new(SubmissionNew::Enabled).boolean().not_null())
                    .col(ColumnDef::new(SubmissionNew::ScanDeletedVideos).boolean().not_null())
                    .col(ColumnDef::new(SubmissionNew::SelectedVideos).string())
                    .to_owned(),
            )
            .await?;

        db.execute_unprepared(
            r#"
            INSERT INTO submission_new 
            SELECT 
                id, upper_id, upper_name, path, created_at,
                strftime('%Y-%m-%d %H:%M:%S', latest_row_at),
                enabled, scan_deleted_videos, selected_videos
            FROM submission
            "#,
        )
        .await?;

        manager
            .drop_table(Table::drop().table(Submission::Table).to_owned())
            .await?;
        db.execute_unprepared("ALTER TABLE submission_new RENAME TO submission")
            .await?;

        // 1.3 重建 collection 表
        manager
            .create_table(
                Table::create()
                    .table(CollectionNew::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(CollectionNew::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(CollectionNew::SId).big_integer().not_null())
                    .col(ColumnDef::new(CollectionNew::MId).big_integer().not_null())
                    .col(ColumnDef::new(CollectionNew::Name).string().not_null())
                    .col(ColumnDef::new(CollectionNew::Type).integer().not_null())
                    .col(ColumnDef::new(CollectionNew::Path).string().not_null())
                    .col(ColumnDef::new(CollectionNew::CreatedAt).string().not_null())
                    .col(ColumnDef::new(CollectionNew::LatestRowAt).string().not_null())
                    .col(ColumnDef::new(CollectionNew::Enabled).boolean().not_null())
                    .col(ColumnDef::new(CollectionNew::ScanDeletedVideos).boolean().not_null())
                    .to_owned(),
            )
            .await?;

        db.execute_unprepared(
            r#"
            INSERT INTO collection_new 
            SELECT 
                id, s_id, m_id, name, type, path, created_at,
                strftime('%Y-%m-%d %H:%M:%S', latest_row_at),
                enabled, scan_deleted_videos
            FROM collection
            "#,
        )
        .await?;

        manager
            .drop_table(Table::drop().table(Collection::Table).to_owned())
            .await?;
        db.execute_unprepared("ALTER TABLE collection_new RENAME TO collection")
            .await?;

        // 为 collection 表添加组合唯一索引
        manager
            .create_index(
                Index::create()
                    .name("idx_collection_composite_unique")
                    .table(Collection::Table)
                    .col(CollectionNew::SId)
                    .col(CollectionNew::MId)
                    .col(CollectionNew::Type)
                    .unique()
                    .to_owned(),
            )
            .await?;

        // 1.4 重建 favorite 表
        manager
            .create_table(
                Table::create()
                    .table(FavoriteNew::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(FavoriteNew::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(FavoriteNew::FId).big_integer().not_null().unique_key())
                    .col(ColumnDef::new(FavoriteNew::Name).string().not_null())
                    .col(ColumnDef::new(FavoriteNew::Path).string().not_null())
                    .col(ColumnDef::new(FavoriteNew::CreatedAt).string().not_null())
                    .col(ColumnDef::new(FavoriteNew::LatestRowAt).string().not_null())
                    .col(ColumnDef::new(FavoriteNew::Enabled).boolean().not_null())
                    .col(ColumnDef::new(FavoriteNew::ScanDeletedVideos).boolean().not_null())
                    .to_owned(),
            )
            .await?;

        db.execute_unprepared(
            r#"
            INSERT INTO favorite_new 
            SELECT 
                id, f_id, name, path, created_at,
                strftime('%Y-%m-%d %H:%M:%S', latest_row_at),
                enabled, scan_deleted_videos
            FROM favorite
            "#,
        )
        .await?;

        manager
            .drop_table(Table::drop().table(Favorite::Table).to_owned())
            .await?;
        db.execute_unprepared("ALTER TABLE favorite_new RENAME TO favorite")
            .await?;

        // 1.5 重建 watch_later 表
        manager
            .create_table(
                Table::create()
                    .table(WatchLaterNew::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(WatchLaterNew::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(WatchLaterNew::Path).string().not_null())
                    .col(ColumnDef::new(WatchLaterNew::CreatedAt).string().not_null())
                    .col(ColumnDef::new(WatchLaterNew::LatestRowAt).string().not_null())
                    .col(ColumnDef::new(WatchLaterNew::Enabled).boolean().not_null())
                    .col(ColumnDef::new(WatchLaterNew::ScanDeletedVideos).boolean().not_null())
                    .to_owned(),
            )
            .await?;

        db.execute_unprepared(
            r#"
            INSERT INTO watch_later_new 
            SELECT 
                id, path, created_at,
                strftime('%Y-%m-%d %H:%M:%S', latest_row_at),
                enabled, scan_deleted_videos
            FROM watch_later
            "#,
        )
        .await?;

        manager
            .drop_table(Table::drop().table(WatchLater::Table).to_owned())
            .await?;
        db.execute_unprepared("ALTER TABLE watch_later_new RENAME TO watch_later")
            .await?;

        // 1.6 重建 task_queue 表
        manager
            .create_table(
                Table::create()
                    .table(TaskQueueNew::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(TaskQueueNew::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(TaskQueueNew::TaskType).string().not_null())
                    .col(ColumnDef::new(TaskQueueNew::TaskData).string().not_null())
                    .col(ColumnDef::new(TaskQueueNew::Status).string().not_null())
                    .col(ColumnDef::new(TaskQueueNew::RetryCount).integer().not_null())
                    .col(ColumnDef::new(TaskQueueNew::CreatedAt).string().not_null())
                    .col(ColumnDef::new(TaskQueueNew::UpdatedAt).string().not_null())
                    .to_owned(),
            )
            .await?;

        db.execute_unprepared(
            r#"
            INSERT INTO task_queue_new 
            SELECT 
                id, task_type, task_data, status, retry_count,
                strftime('%Y-%m-%d %H:%M:%S', substr(created_at, 1, 19)),
                strftime('%Y-%m-%d %H:%M:%S', substr(updated_at, 1, 19))
            FROM task_queue
            "#,
        )
        .await?;

        manager
            .drop_table(Table::drop().table(TaskQueue::Table).to_owned())
            .await?;
        db.execute_unprepared("ALTER TABLE task_queue_new RENAME TO task_queue")
            .await?;

        // 2. 修复时区问题 - 将UTC时间转换为北京时间
        // 对于看起来是UTC时间的记录（时间比当前时间早8小时以上），添加8小时

        db.execute_unprepared(
            r#"
            UPDATE video_source 
            SET latest_row_at = strftime('%Y-%m-%d %H:%M:%S', datetime(latest_row_at, '+8 hours'))
            WHERE datetime(latest_row_at) < datetime('now', '-7 hours')
            "#,
        )
        .await?;

        db.execute_unprepared(
            r#"
            UPDATE submission 
            SET latest_row_at = strftime('%Y-%m-%d %H:%M:%S', datetime(latest_row_at, '+8 hours'))
            WHERE datetime(latest_row_at) < datetime('now', '-7 hours')
            "#,
        )
        .await?;

        db.execute_unprepared(
            r#"
            UPDATE collection 
            SET latest_row_at = strftime('%Y-%m-%d %H:%M:%S', datetime(latest_row_at, '+8 hours'))
            WHERE datetime(latest_row_at) < datetime('now', '-7 hours')
            "#,
        )
        .await?;

        db.execute_unprepared(
            r#"
            UPDATE favorite 
            SET latest_row_at = strftime('%Y-%m-%d %H:%M:%S', datetime(latest_row_at, '+8 hours'))
            WHERE datetime(latest_row_at) < datetime('now', '-7 hours')
            "#,
        )
        .await?;

        db.execute_unprepared(
            r#"
            UPDATE watch_later 
            SET latest_row_at = strftime('%Y-%m-%d %H:%M:%S', datetime(latest_row_at, '+8 hours'))
            WHERE datetime(latest_row_at) < datetime('now', '-7 hours')
            "#,
        )
        .await?;

        // 2.4 重建 config_items 表
        manager
            .create_table(
                Table::create()
                    .table(ConfigItemsNew::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(ConfigItemsNew::KeyName)
                            .string()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(ConfigItemsNew::ValueJson).text().not_null())
                    .col(ColumnDef::new(ConfigItemsNew::UpdatedAt).string().not_null())
                    .to_owned(),
            )
            .await?;

        // 复制数据并转换时间格式为北京时间
        db.execute_unprepared(
            r#"
            INSERT INTO config_items_new (key_name, value_json, updated_at)
            SELECT 
                key_name, 
                value_json,
                strftime('%Y-%m-%d %H:%M:%S', datetime(updated_at, '+8 hours'))
            FROM config_items
            "#,
        )
        .await?;

        manager
            .drop_table(Table::drop().table(ConfigItems::Table).to_owned())
            .await?;
        db.execute_unprepared("ALTER TABLE config_items_new RENAME TO config_items")
            .await?;

        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // 回滚操作：将String类型改回DateTime类型
        // 注意：这会丢失精度（恢复微秒）

        // 暂不实现回滚，因为从String转回DateTime可能会有数据丢失
        Ok(())
    }
}

// 定义表和列的枚举

#[derive(Iden)]
enum VideoSource {
    Table,
}

#[derive(Iden)]
enum VideoSourceNew {
    Table,
    Id,
    Name,
    Path,
    Type,
    LatestRowAt,
    CreatedAt,
    SeasonId,
    MediaId,
    EpId,
    DownloadAllSeasons,
    VideoNameTemplate,
    PageNameTemplate,
    SelectedSeasons,
    Enabled,
    ScanDeletedVideos,
    CachedEpisodes,
    CacheUpdatedAt,
}

#[derive(Iden)]
enum Submission {
    Table,
}

#[derive(Iden)]
enum SubmissionNew {
    Table,
    Id,
    UpperId,
    UpperName,
    Path,
    CreatedAt,
    LatestRowAt,
    Enabled,
    ScanDeletedVideos,
    SelectedVideos,
}

#[derive(Iden)]
enum Collection {
    Table,
}

#[derive(Iden)]
enum CollectionNew {
    Table,
    Id,
    SId,
    MId,
    Name,
    Type,
    Path,
    CreatedAt,
    LatestRowAt,
    Enabled,
    ScanDeletedVideos,
}

#[derive(Iden)]
enum Favorite {
    Table,
}

#[derive(Iden)]
enum FavoriteNew {
    Table,
    Id,
    FId,
    Name,
    Path,
    CreatedAt,
    LatestRowAt,
    Enabled,
    ScanDeletedVideos,
}

#[derive(Iden)]
enum WatchLater {
    Table,
}

#[derive(Iden)]
enum WatchLaterNew {
    Table,
    Id,
    Path,
    CreatedAt,
    LatestRowAt,
    Enabled,
    ScanDeletedVideos,
}

#[derive(Iden)]
enum TaskQueue {
    Table,
}

#[derive(Iden)]
enum TaskQueueNew {
    Table,
    Id,
    TaskType,
    TaskData,
    Status,
    RetryCount,
    CreatedAt,
    UpdatedAt,
}

#[derive(Iden)]
enum ConfigItems {
    Table,
}

#[derive(Iden)]
enum ConfigItemsNew {
    Table,
    KeyName,
    ValueJson,
    UpdatedAt,
}

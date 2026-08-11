use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"UPDATE live_merge_jobs
                   SET status='failed',
                       error_msg=COALESCE(error_msg, 'duplicate active merge job')
                 WHERE status IN ('queued', 'running', 'cancelling')
                   AND EXISTS (
                       SELECT 1
                         FROM live_merge_jobs keeper
                        WHERE keeper.recording_id = live_merge_jobs.recording_id
                          AND keeper.status IN ('queued', 'running', 'cancelling')
                          AND (keeper.created_at < live_merge_jobs.created_at
                               OR (keeper.created_at = live_merge_jobs.created_at
                                   AND keeper.id < live_merge_jobs.id))
                   )"#,
            )
            .await?;
        manager
            .get_connection()
            .execute_unprepared(
                r#"CREATE UNIQUE INDEX IF NOT EXISTS ux_live_merge_jobs_active_recording
                   ON live_merge_jobs(recording_id)
                   WHERE status IN ('queued', 'running', 'cancelling')"#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP INDEX IF EXISTS ux_live_merge_jobs_active_recording")
            .await?;
        Ok(())
    }
}

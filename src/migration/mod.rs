use sea_orm_migration::prelude::*;

mod m20260801_000001_initial_schema;
mod m20260801_000002_add_multi_page;
mod m20260803_000003_add_version_and_operation_log;
mod m20260807_000004_create_live_recordings;
mod m20260809_000005_add_session_roles;
mod m20260810_000006_live_sources_and_interactions;
mod m20260810_000007_live_recording_reliability;
mod m20260810_000008_live_merge_jobs;
mod m20260810_000009_live_source_quality;
mod m20260810_000010_live_merge_job_guards;
mod m20260813_000011_history_multi_page;
mod m20260813_000012_history_fts_rebuild;
mod m20260816_000013_history_sha256;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20260801_000001_initial_schema::Migration),
            Box::new(m20260801_000002_add_multi_page::Migration),
            Box::new(m20260803_000003_add_version_and_operation_log::Migration),
            Box::new(m20260807_000004_create_live_recordings::Migration),
            Box::new(m20260809_000005_add_session_roles::Migration),
            Box::new(m20260810_000006_live_sources_and_interactions::Migration),
            Box::new(m20260810_000007_live_recording_reliability::Migration),
            Box::new(m20260810_000008_live_merge_jobs::Migration),
            Box::new(m20260810_000009_live_source_quality::Migration),
            Box::new(m20260810_000010_live_merge_job_guards::Migration),
            Box::new(m20260813_000011_history_multi_page::Migration),
            Box::new(m20260813_000012_history_fts_rebuild::Migration),
            Box::new(m20260816_000013_history_sha256::Migration),
        ]
    }
}

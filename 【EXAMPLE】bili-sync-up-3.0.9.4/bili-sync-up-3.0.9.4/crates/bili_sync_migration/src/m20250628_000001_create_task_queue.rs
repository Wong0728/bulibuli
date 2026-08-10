use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // 创建任务队列表
        manager
            .create_table(
                Table::create()
                    .table(TaskQueue::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(TaskQueue::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(TaskQueue::TaskType).string_len(50).not_null())
                    .col(ColumnDef::new(TaskQueue::TaskData).text().not_null())
                    .col(
                        ColumnDef::new(TaskQueue::Status)
                            .string_len(20)
                            .not_null()
                            .default("pending"),
                    )
                    .col(ColumnDef::new(TaskQueue::RetryCount).integer().not_null().default(0))
                    .col(
                        ColumnDef::new(TaskQueue::CreatedAt)
                            .timestamp()
                            .default(Expr::current_timestamp())
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(TaskQueue::UpdatedAt)
                            .timestamp()
                            .default(Expr::current_timestamp())
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        // 创建索引以提高查询性能
        manager
            .create_index(
                Index::create()
                    .name("idx_task_queue_status")
                    .table(TaskQueue::Table)
                    .col(TaskQueue::Status)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_task_queue_type_status")
                    .table(TaskQueue::Table)
                    .col(TaskQueue::TaskType)
                    .col(TaskQueue::Status)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_task_queue_created_at")
                    .table(TaskQueue::Table)
                    .col(TaskQueue::CreatedAt)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // 删除索引
        manager
            .drop_index(
                Index::drop()
                    .name("idx_task_queue_status")
                    .table(TaskQueue::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name("idx_task_queue_type_status")
                    .table(TaskQueue::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name("idx_task_queue_created_at")
                    .table(TaskQueue::Table)
                    .to_owned(),
            )
            .await?;

        // 删除表
        manager
            .drop_table(Table::drop().table(TaskQueue::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum TaskQueue {
    Table,
    Id,
    TaskType,
    TaskData,
    Status,
    RetryCount,
    CreatedAt,
    UpdatedAt,
}

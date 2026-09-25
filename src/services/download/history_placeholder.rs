//! 手动入队的 history 占位记录：入队即建 state=pending 行，
//! 驱动看板「下载中」Tab 在整个传输期间有卡片可显。

use crate::models::history;
use anyhow::Result;
use sea_orm::{ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set};
use std::path::Path;
use tracing::warn;

use super::DownloadManager;

/// 手动入队写 history 占位记录所需的信息（见 [`DownloadManager::ensure_history_placeholder`]）。
pub(super) struct HistoryPlaceholder<'a> {
    pub bvid: &'a str,
    pub title: &'a str,
    pub uid: Option<&'a str>,
    pub cid: Option<i64>,
    pub page: Option<i32>,
    pub part_title: Option<&'a str>,
    /// 任务下载目录：封面直接落到该目录，避免完成后二次下载到兜底目录。
    pub cover_dir: Option<&'a Path>,
}

/// 占位记录的落库部分：查询同 (bvid, cid) 既有记录，不存在则插入 state=pending 行。
/// `owner` 为 (owner_name, owner_face, pic) 快照。返回 `Ok(None)` 表示已有记录，
/// 无需再写；插入撞唯一索引（与 video/audio 并发完成或并发入队竞态）时返回 Err。
/// 独立于 DownloadManager 抽出，便于直接对内存 SQLite 做单元测试。
async fn insert_history_placeholder(
    db: &DatabaseConnection,
    info: &HistoryPlaceholder<'_>,
    owner: (Option<String>, Option<String>, Option<String>),
) -> Result<Option<history::Model>> {
    let HistoryPlaceholder {
        bvid,
        title,
        uid,
        cid,
        page,
        part_title,
        ..
    } = *info;
    let mut query = history::Entity::find().filter(history::Column::Bvid.eq(bvid));
    query = match cid {
        Some(cid) => query.filter(history::Column::Cid.eq(cid)),
        None => query.filter(history::Column::Cid.is_null()),
    };
    if query.one(db).await?.is_some() {
        return Ok(None);
    }
    let placeholder = history::ActiveModel {
        uid: Set(uid.map(str::to_string)),
        bvid: Set(bvid.to_string()),
        cid: Set(cid),
        page: Set(page),
        part_title: Set(part_title.map(str::to_string)),
        title: Set(Some(title.to_string())),
        source: Set("manual".to_string()),
        state: Set(Some("pending".to_string())),
        owner_name: Set(owner.0),
        owner_face: Set(owner.1),
        pic: Set(owner.2),
        ..Default::default()
    };
    Ok(Some(placeholder.insert(db).await?))
}

impl DownloadManager {
    /// 手动下载入队时写入占位 history 记录（state=pending）。
    /// 看板「下载中」Tab 由 history 表驱动（board_page 按 bvid ∈ 活跃任务过滤
    /// history 行），入队时不落行则整个传输过程看板无卡片可显，直到首个
    /// 子任务完成经 add_to_history 建行才"突然出现"。完成后由 add_to_history
    /// 的 UPSERT 补齐元数据（uid/pic/download_time 等被 excluded 覆盖）。
    ///
    /// `cover_dir` 为任务下载目录：占位记录建立后立即把封面下载到该目录并回写
    /// cover_local_path。否则前端看板卡片请求 /api/cover 时（uid 可能为空）
    /// 会把封面落到 manual/{日期} 兜底目录，完成后 add_to_history 再下一份
    /// 到任务目录，同一封面存两份且路径不一致。
    pub(super) async fn ensure_history_placeholder(&self, info: HistoryPlaceholder<'_>) {
        let bvid = info.bvid;
        // 未监控博主没有 bloggers 行可兜底，看板分组名只能靠 owner 快照；
        // get_video_info 有会话级缓存，手动入队前 start_download 刚查过，通常零开销。
        let owner = match self.bili_api.get_video_info(bvid, "").await {
            Ok(info) => (
                Some(info.owner.name).filter(|s| !s.is_empty()),
                Some(info.owner.face).filter(|s| !s.is_empty()).map(|s| {
                    if s.starts_with("http") {
                        s
                    } else {
                        format!("https:{s}")
                    }
                }),
                Some(info.pic).filter(|s| !s.is_empty()),
            ),
            Err(e) => {
                warn!("[ensure_history_placeholder] 获取 {bvid} 视频信息失败: {e}");
                (None, None, None)
            }
        };
        let inserted = match insert_history_placeholder(&self.db, &info, owner).await {
            Ok(Some(model)) => model,
            Ok(None) => return,
            Err(e) => {
                warn!("[ensure_history_placeholder] {bvid} 写入占位记录失败: {e}");
                return;
            }
        };
        let Some(dir) = info.cover_dir else { return };
        match self.ensure_cover_local_to(bvid, info.uid, Some(dir)).await {
            Ok(Some(path)) => {
                let mut model: history::ActiveModel = inserted.into();
                model.cover_local_path = Set(Some(path.to_string_lossy().to_string()));
                if let Err(e) = model.update(&self.db).await {
                    warn!("[ensure_history_placeholder] {bvid} 回写封面路径失败: {e}");
                }
            }
            // 目录内已有封面时返回该路径；None 理论上不出现（下载失败走 Err）
            Ok(None) => {}
            Err(e) => {
                // 封面落盘失败不阻塞下载：留空即"可重试标记"，完成路径会再补
                warn!("[ensure_history_placeholder] {bvid} 下载封面失败: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::insert_history_placeholder;
    use crate::models::history;
    use sea_orm::{Database, EntityTrait};
    use sea_orm_migration::MigratorTrait;

    fn placeholder<'a>(
        bvid: &'a str,
        cid: Option<i64>,
        page: Option<i32>,
    ) -> super::HistoryPlaceholder<'a> {
        super::HistoryPlaceholder {
            bvid,
            title: "测试视频",
            uid: Some("9469"),
            cid,
            page,
            part_title: Some("第二P"),
            cover_dir: None,
        }
    }

    #[tokio::test]
    async fn history_placeholder_creates_pending_row_once_per_bvid_cid() {
        let db = Database::connect("sqlite::memory:")
            .await
            .expect("connect in-memory sqlite");
        crate::migration::Migrator::up(&db, None)
            .await
            .expect("run migrations");

        let info = placeholder("BV1xx411c7mD", Some(42), Some(2));
        // 首次插入：创建 state=pending 的手动占位记录，owner 快照落库
        let first = insert_history_placeholder(&db, &info, (Some("UP主".into()), None, None))
            .await
            .expect("首次插入成功")
            .expect("应创建占位记录");
        assert_eq!(first.state.as_deref(), Some("pending"));
        assert_eq!(first.source, "manual");
        assert_eq!(first.owner_name.as_deref(), Some("UP主"));

        // 同 (bvid, cid) 重复调用：已有记录，不重复建行
        let second = insert_history_placeholder(&db, &info, (None, None, None))
            .await
            .expect("二次查询成功");
        assert!(second.is_none());

        // 不同 cid（其他分P）：各自独立建行
        let other = placeholder("BV1xx411c7mD", Some(43), Some(3));
        assert!(insert_history_placeholder(&db, &other, (None, None, None))
            .await
            .expect("分P插入成功")
            .is_some());

        let rows = history::Entity::find().all(&db).await.expect("query rows");
        assert_eq!(rows.len(), 2);
    }
}

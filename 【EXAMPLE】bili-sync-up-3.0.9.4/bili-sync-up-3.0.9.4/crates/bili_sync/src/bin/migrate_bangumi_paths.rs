use anyhow::Result;
use bili_sync_entity::*;
use reqwest::Client;
use sea_orm::*;
use std::collections::HashMap;
use tracing::{error, info, warn};

/// 番剧路径迁移工具
/// 修复数据库中存储的番剧路径，从错误的版本路径更新为正确的番剧文件夹路径
#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日志
    tracing_subscriber::fmt::init();

    info!("开始番剧路径迁移工具");

    // 直接连接数据库
    let database_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite://./config/data.db".to_string());
    let db = Database::connect(&database_url).await?;

    // 创建HTTP客户端
    let client = Client::new();

    // 查找所有番剧视频
    info!("查询数据库中的番剧视频...");
    let bangumi_videos = video::Entity::find()
        .filter(video::Column::SourceType.eq(1)) // 番剧类型
        .filter(video::Column::SeasonId.is_not_null()) // 有season_id
        .all(&db)
        .await?;

    info!("找到 {} 个番剧视频记录", bangumi_videos.len());

    // 按season_id分组
    let mut videos_by_season: HashMap<String, Vec<video::Model>> = HashMap::new();
    for video in bangumi_videos {
        if let Some(ref season_id) = video.season_id {
            videos_by_season.entry(season_id.clone()).or_default().push(video);
        }
    }

    info!("发现 {} 个不同的番剧季度", videos_by_season.len());

    // 处理每个季度
    let mut updated_count = 0;
    for (season_id, videos) in videos_by_season {
        info!("处理番剧季度: {}", season_id);

        // 从API获取真实的番剧标题
        let season_title = get_season_title_from_api(&client, &season_id).await;

        if let Some(title) = season_title {
            info!("获取到番剧标题: \"{}\"", title);

            // 构造正确的番剧文件夹路径
            let first_video = &videos[0];
            let old_path = &first_video.path;

            // 从旧路径中提取根目录（去掉最后的部分）
            let root_path = if let Some(parent_parts) = extract_root_path(old_path) {
                parent_parts
            } else {
                warn!("无法解析路径: {}", old_path);
                continue;
            };

            // 构造新的番剧文件夹路径
            let new_bangumi_path = format!("{}/{}", root_path, title);

            info!("准备更新路径: {} -> {}", old_path, new_bangumi_path);

            // 更新该季度的所有视频
            for video in videos {
                let video_id = video.id;
                let mut video_active: video::ActiveModel = video.into();
                video_active.path = Set(new_bangumi_path.clone());

                match video_active.update(&db).await {
                    Ok(_) => {
                        updated_count += 1;
                    }
                    Err(e) => {
                        error!("更新视频 {} 失败: {}", video_id, e);
                    }
                }
            }
        } else {
            warn!("无法获取季度 {} 的标题，跳过", season_id);
        }
    }

    info!("路径迁移完成，共更新 {} 个视频记录", updated_count);
    Ok(())
}

/// 从API获取番剧季标题
async fn get_season_title_from_api(client: &Client, season_id: &str) -> Option<String> {
    let url = format!("https://api.bilibili.com/pgc/view/web/season?season_id={}", season_id);

    match client.get(&url).send().await {
        Ok(response) => {
            if response.status().is_success() {
                match response.json::<serde_json::Value>().await {
                    Ok(json) => {
                        if json["code"].as_i64().unwrap_or(-1) == 0 {
                            if let Some(title) = json["result"]["title"].as_str() {
                                return Some(title.to_string());
                            }
                        } else {
                            warn!("API返回错误: {}", json["message"].as_str().unwrap_or("未知错误"));
                        }
                    }
                    Err(e) => {
                        error!("解析API响应失败: {}", e);
                    }
                }
            } else {
                warn!("HTTP请求失败，状态码: {}", response.status());
            }
        }
        Err(e) => {
            error!("发送API请求失败: {}", e);
        }
    }

    None
}

/// 从旧路径中提取根目录路径
/// 例如: "D:/Downloads/番剧\中文\Season 1" -> "D:/Downloads/番剧"
fn extract_root_path(path: &str) -> Option<String> {
    let path_parts: Vec<&str> = path.split(['/', '\\']).collect();

    // 找到"番剧"这个目录
    if let Some(bangumi_index) = path_parts.iter().position(|&part| part.contains("番剧")) {
        // 包含番剧目录在内的路径部分
        let root_parts = &path_parts[..=bangumi_index];
        return Some(root_parts.join("/"));
    }

    // 如果没找到"番剧"目录，尝试从路径结构推断
    if path_parts.len() >= 2 {
        // 去掉最后两个部分（版本文件夹和Season文件夹）
        let root_parts = &path_parts[..path_parts.len() - 2];
        return Some(root_parts.join("/"));
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_root_path() {
        assert_eq!(
            extract_root_path("D:/Downloads/番剧\\中文\\Season 1"),
            Some("D:/Downloads/番剧".to_string())
        );

        assert_eq!(
            extract_root_path("D:/Downloads/番剧\\日配\\Season 1"),
            Some("D:/Downloads/番剧".to_string())
        );

        assert_eq!(
            extract_root_path("D:/Downloads/番剧/测试/Season 1"),
            Some("D:/Downloads/番剧".to_string())
        );
    }
}

use crate::models::history;
use anyhow::Result;
use chrono::{Duration, Local};
use sea_orm::{ActiveModelTrait, EntityTrait, Set};

pub(super) fn auto_burn_retry_at(attempts: i32) -> chrono::DateTime<Local> {
    let exponent = attempts.clamp(0, 6) as u32;
    let delay_seconds = 30_i64.saturating_mul(2_i64.pow(exponent)).min(3600);
    Local::now() + Duration::seconds(delay_seconds)
}

pub(super) fn sidecar_retry_at(attempts: i32) -> chrono::DateTime<Local> {
    let exponent = attempts.clamp(0, 7) as u32;
    let delay_seconds = 60_i64.saturating_mul(2_i64.pow(exponent)).min(7200);
    Local::now() + Duration::seconds(delay_seconds)
}

pub(super) async fn missing_burn_materials(
    video_path: &std::path::Path,
    bvid: &str,
    want_danmaku: bool,
    want_subtitle: bool,
) -> Vec<&'static str> {
    let Some(parent) = video_path.parent() else {
        return vec!["视频目录"];
    };
    let stem = video_path
        .file_stem()
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_else(|| bvid.to_string());
    let mut missing = Vec::new();
    if want_danmaku {
        let candidates = [
            parent.join(format!("{bvid}_danmaku.xml")),
            parent.join(format!("{bvid}_danmaku.json")),
        ];
        if !any_path_exists(&candidates).await {
            missing.push("弹幕");
        }
    }
    if want_subtitle {
        let subtitle_dir = parent.join("subtitle");
        let candidates = [
            subtitle_dir.join(format!("{stem}.ass")),
            subtitle_dir.join(format!("{stem}.srt")),
            parent.join(format!("{stem}.ass")),
            parent.join(format!("{stem}.srt")),
        ];
        if !any_path_exists(&candidates).await {
            missing.push("字幕");
        }
    }
    missing
}

async fn any_path_exists(paths: &[std::path::PathBuf]) -> bool {
    for path in paths {
        if tokio::fs::try_exists(path).await.unwrap_or(false) {
            return true;
        }
    }
    false
}

pub(super) async fn update_auto_burn_state(
    db: &sea_orm::DatabaseConnection,
    history_id: i32,
    status: &str,
    next_retry_at: Option<chrono::DateTime<Local>>,
) -> Result<()> {
    let history = history::Entity::find_by_id(history_id).one(db).await?;
    let Some(history) = history else {
        return Ok(());
    };
    let mut model: history::ActiveModel = history.into();
    model.auto_burn_status = Set(Some(status.to_string()));
    model.auto_burn_next_retry_at = Set(next_retry_at);
    model.update(db).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_windows_are_bounded() {
        let now = Local::now();
        let burn = auto_burn_retry_at(99)
            .signed_duration_since(now)
            .num_seconds();
        let sidecar = sidecar_retry_at(99)
            .signed_duration_since(now)
            .num_seconds();
        assert!((1918..=1921).contains(&burn));
        assert!((7198..=7201).contains(&sidecar));
    }

    #[tokio::test]
    async fn missing_burn_materials_accepts_any_supported_sidecar() {
        let dir = tempfile::tempdir().expect("temp dir");
        let video = dir.path().join("BV1xx411c7mD.mp4");
        tokio::fs::write(&video, b"video").await.expect("video");

        let missing = missing_burn_materials(&video, "BV1xx411c7mD", true, true).await;
        assert_eq!(missing, vec!["弹幕", "字幕"]);

        tokio::fs::write(dir.path().join("BV1xx411c7mD_danmaku.json"), b"{}")
            .await
            .expect("danmaku");
        tokio::fs::create_dir_all(dir.path().join("subtitle"))
            .await
            .expect("subtitle dir");
        tokio::fs::write(dir.path().join("subtitle/BV1xx411c7mD.srt"), b"1")
            .await
            .expect("subtitle");
        assert!(missing_burn_materials(&video, "BV1xx411c7mD", true, true)
            .await
            .is_empty());
    }
}

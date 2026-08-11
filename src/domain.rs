use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadStatus {
    Pending,
    Downloading,
    Paused,
    Retrying,
    Merging,
    Completed,
    Failed,
    Cancelled,
}

impl DownloadStatus {
    pub fn can_transition_to(&self, next: &Self) -> bool {
        use DownloadStatus::*;
        matches!(
            (self, next),
            (Pending, Downloading | Paused | Cancelled | Failed)
                | (
                    Downloading,
                    Paused | Retrying | Merging | Completed | Failed | Cancelled
                )
                | (Paused, Downloading | Retrying | Cancelled | Failed)
                | (Retrying, Downloading | Failed | Cancelled)
                | (Merging, Completed | Failed | Cancelled)
                | (Failed, Retrying | Cancelled)
        ) || self == next
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

impl fmt::Display for DownloadStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Pending => "pending",
            Self::Downloading => "downloading",
            Self::Paused => "paused",
            Self::Retrying => "retrying",
            Self::Merging => "merging",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        })
    }
}

impl FromStr for DownloadStatus {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let normalized = match value {
            "waiting" => "pending",
            "merge_failed" => "failed",
            other => other,
        };
        serde_json::from_value(serde_json::Value::String(normalized.to_string()))
            .map_err(|_| format!("未知下载状态: {value}"))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadStage {
    Queued,
    Resolving,
    Transferring,
    Muxing,
    Finalizing,
    Done,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    Video,
    Audio,
    Danmaku,
    Comments,
    Cover,
}

impl fmt::Display for TaskKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Video => "video",
            Self::Audio => "audio",
            Self::Danmaku => "danmaku",
            Self::Comments => "comments",
            Self::Cover => "cover",
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskSource {
    Auto,
    Manual,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TaskKey {
    pub bvid: String,
    pub kind: TaskKind,
    /// 分P序号：单P为 None（与存量进度合并行为一致），多P为具体分P号，
    /// 用于区分同 bvid 同类型不同分P的进度快照，避免合并冲突。
    pub page: Option<i32>,
}

#[derive(Clone, Debug, Serialize)]
pub struct BasicInfo {
    pub bvid: String,
    pub title: String,
    pub source: TaskSource,
    pub owner_uid: Option<String>,
    pub owner_name: Option<String>,
    pub cover: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct FileInfo {
    pub path: Option<String>,
    pub filename: Option<String>,
    pub total_size: i64,
    pub format: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct DownloadInfo {
    pub status: DownloadStatus,
    pub stage: DownloadStage,
    pub progress_percent: i32,
    pub downloaded_size: i64,
    pub speed: i64,
    pub generation: i64,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct TaskInfo {
    pub basic: BasicInfo,
    pub file: FileInfo,
    pub download: DownloadInfo,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transitions_are_restricted() {
        assert!(DownloadStatus::Pending.can_transition_to(&DownloadStatus::Downloading));
        assert!(DownloadStatus::Downloading.can_transition_to(&DownloadStatus::Merging));
        assert!(DownloadStatus::Merging.can_transition_to(&DownloadStatus::Completed));
        assert!(!DownloadStatus::Completed.can_transition_to(&DownloadStatus::Downloading));
    }

    #[test]
    fn legacy_statuses_are_normalized() {
        assert_eq!(
            DownloadStatus::from_str("waiting").expect("legacy status is known"),
            DownloadStatus::Pending
        );
        assert_eq!(
            DownloadStatus::from_str("merge_failed").expect("legacy status is known"),
            DownloadStatus::Failed
        );
    }

    #[test]
    fn display_values_are_stable() {
        assert_eq!(DownloadStatus::Retrying.to_string(), "retrying");
        assert_eq!(TaskKind::Danmaku.to_string(), "danmaku");
    }
}

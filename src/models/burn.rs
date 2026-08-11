use serde::Serialize;

/// 字幕或弹幕烧录接口返回的可序列化状态。
#[derive(Clone, Serialize)]
pub struct BurnTask {
    pub bvid: String,
    pub status: String,
    pub message: String,
    pub output_path: Option<String>,
    #[serde(skip_serializing)]
    pub created_at: i64,
    #[serde(skip_serializing)]
    pub updated_at: i64,
}

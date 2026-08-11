use serde::Serialize;

/// Serializable status returned by the subtitle/danmaku burn endpoints.
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

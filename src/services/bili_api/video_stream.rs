//! 视频信息与音视频流解析：get_video_info / playurl / 流选择（全程强类型）。
//!
//! 按职责拆分为兄弟模块：
//! - [`error`]：流不可下载的业务性错误类型（充电专属/付费/多分段）
//! - [`info`]：视频详情（WBI 签名 + 会话级缓存）与 CC 字幕列表
//! - [`playurl`]：playurl 请求与视频流集合构建（DASH / durl 直链）
//! - [`audio`]：音频下载流（m4a / 杜比 / Hi-Res）选择
//! - [`selection`]：画质优先、编码偏好、最低画质下限的稳定选流算法

#[path = "video_stream/audio.rs"]
mod audio;
#[path = "video_stream/error.rs"]
mod error;
#[path = "video_stream/info.rs"]
mod info;
#[path = "video_stream/playurl.rs"]
mod playurl;
#[path = "video_stream/selection.rs"]
mod selection;

pub use error::StreamUnavailableError;
pub(crate) use selection::choose_video_stream;

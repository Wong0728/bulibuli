//! 视频解析与取流接口。
//!
//! 按职责拆分的子模块：
//! - `resolve`：媒体链接解析与番剧/课程季分集信息；
//! - `list`：UP 主投稿列表分页查询；
//! - `urls`：视频/音频流获取、视频信息与下载权限门禁。

mod list;
mod resolve;
mod urls;

pub(crate) use list::get_videos;
pub(crate) use resolve::resolve_video;
pub(crate) use urls::{gate_download, get_audio_url, get_video_info, get_video_urls};

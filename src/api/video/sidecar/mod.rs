//! 视频附件（侧车文件）接口。
//!
//! 按职责拆分的子模块：
//! - `fetch`：触发弹幕/评论下载并落盘到视频所在目录；
//! - `view`：读取已下载的弹幕/评论文件供前端展示。

mod fetch;
mod view;

pub(crate) use fetch::{download_comments, download_danmaku};
pub(crate) use view::{get_comments, get_danmaku};

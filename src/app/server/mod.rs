//! 主 Web 服务器：axum 路由、请求安全策略、端口绑定与优雅关停。
//!
//! 模块划分：
//! - [`lifecycle`]：serve 主循环与关停信号（Ctrl+C / SIGTERM / cancellation + 宽限期）
//! - [`bind`]：监听器绑定（端口回退、双栈 LAN）与用户可访问 URL 推导
//! - [`router`]：路由组装、静态资源、限流器与请求日志
//! - [`request_filter`]：请求安全中间件（来源 IP、Host、会话、CSRF/Origin）
//! - [`rbac`]：服务端 RBAC 边界（Owner/Operator/Viewer）
//! - [`net_headers`]：网络信任策略（proxy XFF）、Host/Origin 校验与安全响应头

pub mod bind;
pub mod lifecycle;
pub mod net_headers;
pub mod rbac;
pub mod request_filter;
pub mod router;

pub use bind::{accessible_main_urls, bind_main_listener, main_url};
pub use lifecycle::serve;

pub(crate) use router::build_router;

# 第三方组件与许可声明（NOTICE）

本应用捆绑或使用了以下第三方组件，各组件以其自身的许可证为准，本声明不替代上游许可证文本。

## 前端（`web/`，Vue 3 工程）

| 组件 | 用途 | 许可证 |
| --- | --- | --- |
| Vue 3 | 前端 UI 框架 | MIT |
| Vite | 构建工具 | MIT |
| Pinia | 状态管理 | MIT |
| vue-tsc / TypeScript | 类型检查与编译 | Apache-2.0 |
| Socket.IO Client | 实时通信客户端 | MIT |
| Font Awesome Free | 图标 | CC BY 4.0 / SIL OFL 1.1 / MIT（按文件适用） |
| JetBrains Mono | 等宽字体（代码/日志展示） | SIL OFL 1.1 |
| qrcode.js | 二维码渲染 | MIT |

完整依赖清单见 [`web/package.json`](web/package.json) 及其传递依赖的各自许可声明。

## 后端（Rust，主要 crate）

后端以 MIT 许可发布，依赖的关键 crate 包括：

| Crate | 用途 | 许可证 |
| --- | --- | --- |
| tokio / tokio-util | 异步运行时 | MIT |
| axum / tower-http | Web 框架与中间件 | MIT |
| sea-orm / sea-orm-migration / sqlx | SQLite ORM 与迁移 | MIT OR Apache-2.0 |
| reqwest / rustls | HTTP 客户端与 TLS（rustls，避免系统 OpenSSL） | MIT OR Apache-2.0 |
| socketioxide / tokio-tungstenite | Socket.IO 与 WebSocket | MIT |
| serde / serde_json | 序列化 | MIT OR Apache-2.0 |
| aes-gcm / sha2 / hmac / subtle | 加密、哈希与常量时间比较 | MIT OR Apache-2.0 等，见各 crate |
| tracing / tracing-subscriber / tracing-appender | 日志 | MIT |
| chrono / anyhow / thiserror | 时间与错误处理 | MIT OR Apache-2.0 |
| maxminddb | GeoIP 数据库读取 | Apache-2.0 |
| ratatui / crossterm | 终端界面 | MIT |
| prost | Protobuf 解码（弹幕） | MIT |

完整依赖树及各 crate 的准确许可证以 `Cargo.lock` 锁定版本和上游声明为准；可用 `cargo license` 或 `cargo deny check` 复核（仓库含 `deny.toml` 配置）。

## 捆绑的二进制运行时

### FFmpeg（自编译精简构建，GPL-2.0-or-later）

Windows 包内的 `ffmpeg.exe` / `ffprobe.exe` 为**本仓库 GitHub Actions 工作流**（[`.github/workflows/build-ffmpeg.yml`](.github/workflows/build-ffmpeg.yml)）基于上游 [FFmpeg n8.1.2 源码标签](https://github.com/FFmpeg/FFmpeg/tree/n8.1.2) 交叉编译的精简构建（仅含直播录制、合并/remux、字幕烧录所需能力，静态链接 mingw-w64）。因链接 libx264（GPL-2.0-or-later），**整体以 GPL-2.0-or-later 许可发布**。据此：

- 上游源码：<https://github.com/FFmpeg/FFmpeg/tree/n8.1.2>；构建配置、外部依赖版本（x264、libass、FreeType 等）与校验和记录见 [`resources/README.md`](resources/README.md) 及产物内 `BUILD_INFO.txt`。
- 任何分发包含该 FFmpeg 二进制的完整包时，须遵守 GPL-2.0-or-later 的源码提供义务与许可文本要求（本仓库工作流即为对应源码与构建脚本的提供途径）。
- FFmpeg 属高频披露安全漏洞的组件，本项目会定期跟进上游更新；更新时同步调整工作流中的 `ffmpeg_ref` 并重新构建。

### aria2

| 组件 | 用途 | 来源 | 许可证 |
| --- | --- | --- | --- |
| aria2 1.37.0 | RPC 多线程下载引擎 | <https://github.com/aria2/aria2/releases> | GPL-2.0-or-later |

随包分发的 aria2 二进制同样受其 GPL 许可约束，源码可从上述官方 Releases 页面获取。

## GeoIP 数据库

`resources/geo/GeoLite2-Country.mmdb` 由 **[DB-IP.com](https://db-ip.com)** 提供（DB-IP IP to Country Lite 免费版，每月更新），以 **CC BY 4.0** 许可使用。下载页：<https://db-ip.com/db/download/ip-to-country-lite>。

> 注：文件名中的 "GeoLite2" 是历史遗留命名（沿用 MaxMind 格式兼容命名），实际数据来源为 DB-IP，与 MaxMind 公司无关。依据 CC BY 4.0，面向最终用户的界面中需保留 DB-IP 来源说明。

## 其他

Exact bundled versions and SHA-256 values are listed in [`resources/README.md`](resources/README.md). The Unix runtime binaries are copied from the official CI runner packages at release-build time and receive per-package `.sha256` manifests.

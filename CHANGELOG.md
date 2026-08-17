# Changelog

## v2.0.0-alpha.8

- 完成全量代码审查整改（37 项）：修复直播录制会话条目泄漏、活跃 WebSocket 阻塞优雅关机、多 P 音频重试取错分 P、断点续传恢复失效、aria2 任务进度不更新、弹幕采集鉴权重连无退避等缺陷。
- 加固安全与健壮性：认证限流内存上限与近似 LRU 淘汰、CSRF 恒定时间比较、私网 IP 识别补全（IPv4-mapped/CGNAT）、安装器拒绝 manifest 外可执行文件、发布流程补齐 `windows-build` 依赖。
- 工程卫生：MSRV 声明对齐 1.97.1、发布构建启用 `--locked`、前端轮询守卫与属性转义补齐；`clippy -D warnings` 零警告，261 项测试与前端/冒烟测试全部通过。

## v2.0.0-alpha.7

- Termux 默认始终使用 GitHub Actions 云端预编译包；只有显式设置 `BULIBULI_SOURCE_BUILD=1` 才会从源码构建。
- 修正 Termux 远程安装后的后台启动路径，并同步 README、Release 说明和 Hero 页面。
- 修复 Hero 窄屏横向溢出，保证手机浏览器可正常浏览和操作。

## v2.0.0-alpha.5

- Termux Android arm64 改为 GitHub Actions 云端编译并发布预编译包；安装器默认下载、校验并复用该包，不再要求本机编译 Rust。
- Windows 安装器继续独立于便携包，并优先发现用户放在当前目录、源码 checkout 或缓存目录中的已校验归档。

## v2.0.0-alpha.4

- 修复原生下载兜底直接写入最终文件的问题：现在使用临时文件，完成后复用 SHA-256 去重和安全归位流程，失败或取消会清理半成品。
- 约束 Aria2/历史任务文件名和下载目录，阻止绝对路径、路径穿越以及目录符号链接把产物写到下载根目录外。
- 补充下载进度、完成文件名、监控调度、视频处理和 FFmpeg 探测的边界测试。
- 发布工作流继续生成 Windows/Linux `core`、各平台 `portable`、SHA-256 清单和 `latest.json`。

## v2.0.0-alpha.3

- GitHub Release 的 `portable` 包继续提供完整自包含的 aria2c、FFmpeg 和必要动态库；ffprobe 可选，缺失时由 FFmpeg 回退探测。
- Linux 一键安装优先复用本机运行时，依赖齐全时下载更小的 `core` 包；缺少依赖时回退完整包。
- 完善包内运行时优先级和 `ffprobe` 可选探测回退。

## v2.0.0-alpha.2

- Linux/macOS portable packages carry platform-native aria2c and FFmpeg binaries plus SHA-256 manifests.
- Linux control IPC falls back from `XDG_RUNTIME_DIR` to short safe paths when a data directory is too deep for Unix socket limits.
- First-run pairing codes are written with private permissions and removed after successful pairing.
- Linux aria2 uses system DNS and receives a parent-death signal; systemd services kill the complete child process group.
- Headless startup no longer emits browser-open errors.
- Linux installer follows the latest Release by default and supports explicit version pinning.
- Added release metadata, contributor/security guidance and third-party notices.

## v2.0.0-alpha.1

- First Rust v2 Alpha Release.

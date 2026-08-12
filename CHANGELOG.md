# Changelog

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

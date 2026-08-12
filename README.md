# 补哩补哩 bulibuli

> 下架之前，先下为敬。

<p align="center">
  <img src="docs/hero/bulibuli_hero_preview.svg" alt="补哩补哩 bulibuli Hero 矢量预览" width="100%">
</p>

补哩补哩是一个基于 Rust/Axum 的 B 站视频监控、补档下载、直播监控与直播录制工具。当前主线是 Rust v2 Alpha，支持 Windows、Linux、macOS Intel、macOS Apple Silicon，以及单独的 Termux 源码安装方式。

## 从 Releases 安装

打开 [GitHub Releases](https://github.com/Wong0728/bulibuli/releases)，下载与你的系统和 CPU 架构对应的 `portable` 归档，并下载同名 `.sha256` 文件。先校验 SHA-256，再解压到一个有写权限的目录。

| 平台 | 归档 | 运行方式 |
| --- | --- | --- |
| Windows x86_64 | `bulibuli-windows-x86_64-portable-*.zip` | 解压后运行 `bulibuli.exe` |
| Linux x86_64 | `bulibuli-linux-x86_64-portable-*.tar.gz` | 解压后运行 `./bulibuli`，或执行 `./install.sh run` |
| macOS Intel | `bulibuli-macos-x86_64-portable-*.tar.gz` | 解压后运行 `./bulibuli` |
| macOS Apple Silicon | `bulibuli-macos-arm64-portable-*.tar.gz` | 解压后运行 `./bulibuli` |

便携包包含程序、前端、GeoIP 数据、对应平台的 aria2c/FFmpeg 运行时及其必要的非系统动态库，不要求用户另行安装 Rust、Node.js、aria2 或 FFmpeg。Linux/macOS 如果系统阻止执行新文件，请先执行：

```bash
chmod +x bulibuli resources/aria2c resources/ffmpeg
```

首次启动会显示本地地址；有图形桌面时尝试自动打开浏览器，没有图形桌面时直接复制终端中的地址即可。默认只监听 `127.0.0.1`。

### Linux 一键安装

安装器默认查询最新 GitHub Release，并校验归档和包内 aria2c/FFmpeg 的 SHA-256：

```bash
curl -fsSL https://raw.githubusercontent.com/Wong0728/bulibuli/main/deploy/linux/install.sh | bash
```

安装到用户目录后，前台运行：

```bash
~/.local/share/bulibuli/install.sh run
```

如需固定版本，避免自动升级：

```bash
curl -fsSL https://raw.githubusercontent.com/Wong0728/bulibuli/main/deploy/linux/install.sh \
  | BULIBULI_VERSION=v2.0.0-alpha.2 bash
```

安装器支持 `install`、`run`、`service`、`unservice` 和 `status`。`service` 会优先使用用户级 systemd；root 安装会创建独立服务用户。自定义数据目录时使用绝对路径，例如：

```bash
BULIBULI_DATA_DIR=/srv/bulibuli-data ~/.local/share/bulibuli/install.sh service
```

没有 sudo、systemd 或图形桌面也不影响便携包前台运行；只有缺少包内运行时且需要系统包管理器补齐时才需要安装权限。

### Termux

Termux 使用源码构建并通过 `pkg` 安装 Rust、aria2 和 FFmpeg：

```bash
curl -fsSL https://raw.githubusercontent.com/Wong0728/bulibuli/main/deploy/termux/install.sh | bash
```

首次构建需要网络和一定时间；后台运行使用 `bash install.sh start`，开机自启还需要 Termux:Boot。

## 首次设置与常用操作

1. 启动程序并打开终端打印的本地 URL。
2. 首次设备配对码会同时显示在终端，并写入 `data/pair-code.txt`；文件权限为当前用户可读，配对成功后自动删除。
3. 在网页中完成设备配对和安全设置，再按需扫码登录 B 站。
4. 下载任务使用内置 aria2c，视频合并、字幕烧录和直播录制使用内置 FFmpeg。

常用本机控制命令：

```text
bulibuli ctl sys status
bulibuli ctl dl status
bulibuli ctl cred qrcode
bulibuli ctl sys ffmpeg-test
```

Linux/macOS 将 `bulibuli.exe` 替换为 `./bulibuli`。完整命令清单见 [`docs/skill.md`](docs/skill.md)。

## 数据、配置和日志

- 默认数据目录：可执行文件旁的 `data/`。
- 自定义数据目录：环境变量 `BILI__DATA_DIR=/absolute/path`。
- 数据目录包含 SQLite 数据库、下载目录、`security.toml`、日志、迁移备份和运行状态；升级前请先停止程序并备份整个目录。
- 日志按天滚动。日志、数据库、Cookie、session 和配对码不要上传到 issue 或公开工单。
- Unix 控制 socket 优先使用 `XDG_RUNTIME_DIR`，深层数据目录不会再触发 Linux socket 路径过长；Windows 使用仅本机可访问的命名管道。

## 故障排查

- `/api/health` 返回服务存活状态，`/api/ready` 会同时检查数据库和 aria2。
- 下载失败先运行 `sys status`，确认 aria2 可用；再运行 `sys aria2-restart`。
- FFmpeg 检查使用 `sys ffmpeg-test`。
- 无桌面、SSH、Docker 或 nohup 环境不会强行调用浏览器；直接打开终端输出的 URL。
- 如果安装器找不到最新版本，可用 `BULIBULI_VERSION=vX.Y.Z` 固定到 Releases 中明确存在的版本；也可以直接下载 portable 归档，不需要安装器。
- Linux 便携包中的运行时二进制带有独立 `.sha256` 文件；归档或运行时校验失败时停止安装，不要使用被替换的文件。

## 从源码开发

要求：Rust 1.97.1、Python 3.11+、Node.js 22+。Rust 版本由 `rust-toolchain.toml` 固定；前端依赖位于 `static/js`。

```bash
cargo test --all-targets
cd static/js
npm ci --ignore-scripts
npm run build
cd ../..
python build.py --check
python build.py --portable
```

`python build.py --portable` 会拒绝组装缺少 aria2c、FFmpeg 或必要动态库的 Unix 包，避免生成用户下载后无法完成核心任务的 Release。每个 Release 由 tag 触发 GitHub Actions 自动生成，归档与校验文件会一起上传。

## 文档与贡献

- [文档索引](docs/README.md)
- [代码规范](代码规范.md)
- [贡献指南](CONTRIBUTING.md)
- [行为准则](CODE_OF_CONDUCT.md)
- [安全策略](SECURITY.md)
- [第三方资源与许可](NOTICE.md)
- [变更记录](CHANGELOG.md)
- [部署安全说明](deploy/SECURITY.md)
- [内置资源清单与校验](resources/README.md)

提交 issue 前请附上版本、平台/架构、启动方式、脱敏后的错误信息和复现步骤；不要附带 Cookie、token、完整绝对路径或整个 `data/` 目录。

## 项目状态

`v2.0.0-alpha.*` 是预发布版本，接口和数据结构仍可能变化。升级或迁移前请备份 `data/`，不要让新旧版本同时写同一数据库。旧 Python v1 仅保留在 `v1-python` 分支和历史标签中，不属于当前 Rust v2 产品。

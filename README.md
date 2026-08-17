# 补哩补哩 bulibuli

> 下架之前，先下为敬。

<p align="center">
  <img src="docs/hero/bulibuli_hero_preview.svg" alt="补哩补哩 bulibuli Hero 矢量预览" width="100%">
</p>

补哩补哩是一个基于 Rust/Axum 的 B 站视频监控、补档下载、直播监控与直播录制工具。当前主线是 Rust v2 Alpha，支持 Windows、Linux、macOS Intel、macOS Apple Silicon，以及 Android arm64/Termux 云端预编译包。

## 从 Releases 安装

打开 [GitHub Releases](https://github.com/Wong0728/bulibuli/releases)，下载与你的系统和 CPU 架构对应的 `portable` 归档，并下载同名 `.sha256` 文件。先校验 SHA-256，再解压到一个有写权限的目录。

| 平台 | 归档 | 运行方式 |
| --- | --- | --- |
| Windows x86_64 | `bulibuli-windows-x86_64-portable-*.zip` | 解压后运行 `bulibuli.exe` |
| Windows x86_64（已有运行时） | `bulibuli-windows-x86_64-core-*.zip` | 不含 aria2c/FFmpeg；也可用 Windows 安装器自动选择包类型 |
| Linux x86_64 | `bulibuli-linux-x86_64-portable-*.tar.gz` | 解压后运行 `./bulibuli`，或执行 `./install.sh run` |
| macOS Intel | `bulibuli-macos-x86_64-portable-*.tar.gz` | 解压后运行 `./bulibuli` |
| macOS Apple Silicon | `bulibuli-macos-arm64-portable-*.tar.gz` | 解压后运行 `./bulibuli` |
| Termux Android arm64 | `bulibuli-termux-arm64-portable-*.tar.gz` | 解压后运行 `bash install.sh`；依赖由 `pkg` 提供 |

桌面平台的 `portable` 包是完整自包含包：包含程序、前端、GeoIP 数据、对应平台的 aria2c、FFmpeg 及必要的非系统动态库，不要求用户另行安装 Rust、Node.js、aria2 或 FFmpeg。若系统另有可用的 `ffprobe`，程序也会优先使用它；没有时会用包内 FFmpeg 完成媒体探测。Termux 包只包含 Android/Termux 的 bulibuli 和前端，aria2/FFmpeg 由 `pkg` 提供。Linux/macOS 如果系统阻止执行新文件，请先执行：

```bash
chmod +x bulibuli resources/aria2c resources/ffmpeg
```

首次启动会显示本地地址；有图形桌面时尝试自动打开浏览器，没有图形桌面时直接复制终端中的地址即可。默认只监听 `127.0.0.1`。

### Windows 一键安装

`install.ps1` 是独立的 PowerShell 安装器，不嵌入 Windows 发布包。先下载 Windows `portable` 归档和同名 `.sha256`，放到当前目录、安装器目录、源码 checkout 目录，或 `BULIBULI_CACHE_DIR` 指定的缓存目录；安装器会优先发现并校验本地包，找不到时才访问 GitHub Release：

```powershell
Invoke-WebRequest https://raw.githubusercontent.com/Wong0728/bulibuli/main/deploy/windows/install.ps1 -OutFile .\install.ps1
powershell -ExecutionPolicy Bypass -File .\install.ps1
```

也可以显式指定本地归档：

```powershell
powershell -ExecutionPolicy Bypass -File .\install.ps1 `
  -PackagePath .\bulibuli-windows-x86_64-portable-vX.Y.Z.zip `
  -InstallDir "$env:LOCALAPPDATA\bulibuli" -Variant Auto
```

归档必须保留同名 `.sha256`；包内的 `bulibuli.package.json` 由安装器继续校验。需要指定本地缓存目录时设置 `BULIBULI_CACHE_DIR`，例如：

```powershell
$env:BULIBULI_CACHE_DIR = 'D:\GitHub\bulibuli'
powershell -ExecutionPolicy Bypass -File .\install.ps1
```

不传 `-PackagePath` 时，本地包会优先于 Release；没有本地包时读取 Release 的 `latest.json`，没有稳定版本时回退读取包含 Alpha 的 Releases 清单。PATH 修改为用户级设置，完成后请重新打开终端。

### Linux 一键安装

一键安装器会先检查包内运行时（完整包优先），再检查 `ARIA2C_PATH`、`FFMPEG_PATH`/`FFMPEG`/`FFMPEG_HOME`/`FFMPEG_DIR`、`FFPROBE_PATH` 和系统 `PATH`。依赖齐全时下载体积更小的 `core` 包；缺少 aria2c 或 FFmpeg 时才尝试系统包管理器，仍不可用才回退完整 `portable` 包。两种归档都会校验 SHA-256：

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
  | BULIBULI_VERSION=vX.Y.Z bash
```

安装器支持 `install`、`run`、`service`、`unservice` 和 `status`。`service` 会优先使用用户级 systemd；root 安装会创建独立服务用户。自定义数据目录时使用绝对路径，例如：

```bash
BULIBULI_DATA_DIR=/srv/bulibuli-data ~/.local/share/bulibuli/install.sh service
```

没有 sudo、systemd 或图形桌面也不影响便携包前台运行；只有缺少包内运行时且需要系统包管理器补齐时才需要安装权限。

### Termux Android arm64

Termux 现在优先下载 GitHub Actions 云端编译好的预编译包，再通过 `pkg` 安装 curl、Python、aria2 和 FFmpeg：

```bash
curl -fsSL https://raw.githubusercontent.com/Wong0728/bulibuli/main/deploy/termux/install.sh | bash
```

脚本会校验 Release 的 SHA-256 和包内文件清单，不需要 Rust、Node.js 或本机编译。远程一键安装完成后，后台运行使用 `bash "$PREFIX/opt/bulibuli/install.sh" start`；如果是手动解压包，则使用解压目录中的 `bash install.sh start`。开机自启还需要 Termux:Boot。只有需要源码构建时才设置 `BULIBULI_SOURCE_BUILD=1`。

## 首次设置与常用操作

1. 启动程序并打开终端打印的本地 URL。
2. 首次设备配对码会同时显示在终端，并写入 `data/pair-code.txt`；文件权限为当前用户可读，配对成功后自动删除。
3. 在网页中完成设备配对和安全设置，再按需扫码登录 B 站。
4. 完整包优先使用包内 aria2c 和 FFmpeg；轻量 `core` 包没有包内运行时时，按环境变量再到系统 `PATH` 查找，媒体探测会继续寻找 ffprobe 或回退 FFmpeg。

常用本机控制命令：

```text
bulibuli ctl sys status
bulibuli ctl dl status
bulibuli ctl cred qrcode
bulibuli ctl sys ffmpeg-test
```

查看版本和帮助不会加载配置、创建数据目录、启动数据库或启动服务：

```text
bulibuli --version
bulibuli --help
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
npm run test:smoke
cd ../..
python build.py --check
python build.py --portable
```

`python build.py --portable` 会生成桌面平台完整自包含包，或生成不内置媒体运行时的 Termux Android arm64 包；桌面 Unix 包缺少 aria2c、FFmpeg 或必要动态库时会拒绝组装。`python build.py --core` 生成不含媒体运行时的轻量命令包。每个包都会生成 `bulibuli.package.json`，记录版本、平台、架构、包类型和文件 SHA-256。推送 `v*` tag 后由独立的 Release 工作流构建并上传 Windows/Linux/macOS `portable` 包、Termux arm64 包、Windows/Linux `core` 包及 `latest.json`；已有 tag 也可以从 Actions 手动补发。详见 [云端发布架构](docs/user/RELEASES.md)。

## 文档与贡献

- [用户文档索引](docs/user/README.md)
- [贡献指南](CONTRIBUTING.md)
- [行为准则](CODE_OF_CONDUCT.md)
- [安全策略](SECURITY.md)
- [第三方资源与许可](NOTICE.md)
- [变更记录](CHANGELOG.md)
- [部署安全说明](deploy/SECURITY.md)
- [内置资源清单与校验](resources/README.md)

提交 issue 前请附上版本、平台/架构、启动方式、脱敏后的错误信息和复现步骤；不要附带 Cookie、token、完整绝对路径或整个 `data/` 目录。

## 项目状态

`v2.0.0-alpha.*` 是预发布版本，接口和数据结构仍可能变化。升级前请备份 `data/`，不要让新旧版本同时写同一数据库。当前公开主线为 Rust v2。

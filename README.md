# 补哩补哩 bulibuli

> 下架之前，先下为敬。

<p align="center">
  <img src="docs/hero/bulibuli_hero_preview.svg" alt="补哩补哩 bulibuli Hero 矢量预览" width="100%">
</p>

补哩补哩是一个基于 Rust/Axum 的 B 站视频监控、补档下载、直播监控与直播录制工具。当前主线是 Rust v2 正式版，支持 Windows、Linux、macOS Intel、macOS Apple Silicon，以及 Android arm64/Termux 云端预编译包。

> 隐私说明：本工具收集的 B 站 Cookie 仅存储在本地并加密，数据不回传任何第三方服务器，详见 [隐私说明（PRIVACY.md）](PRIVACY.md)。

**前端架构**：主界面是唯一的 Vue 3 + Vite + Pinia + Vue Router 工程（位于 `web/`，构建产物落到 `static/app/`）；根路由 `/` 直接提供该界面。新版构建步骤见「从源码开发」一节。

## 快速开始

1. 从 [Releases](https://github.com/Wong0728/bulibuli/releases) 下载对应平台的 `portable` 包和同名 `.sha256`，校验后解压到任意有写权限的目录。
2. 运行程序：Windows 双击或执行 `bulibuli.exe`（启动器会通过 PowerShell 调用 `bulibuli-core.exe`）；Linux/macOS 执行 `./bulibuli`（首次请先 `chmod +x bulibuli resources/aria2c resources/ffmpeg`）。
3. 打开终端打印的本地地址（默认 `http://127.0.0.1:5000`），输入终端横幅中的**首次设备配对码**完成配对。配对码 10 分钟有效且只能用一次；过期或丢失可执行 `bulibuli ctl pair` 重新生成，无需重启。
4. 在网页中按需扫码登录 B 站，然后添加要监控的视频或直播间。

## 最低系统要求

| 平台 | 要求 |
| --- | --- |
| Windows x86_64 | Windows 10 / Server 2016 及更新 |
| Linux x86_64 | **glibc ≥ 2.35**（Ubuntu 22.04 LTS、Debian 12、RHEL/Rocky/Alma 9 及更新的发行版）；更低版本的发行版无法运行 portable 包 |
| macOS | 以 Release 页说明为准（CI 使用 macos-14 / macos-15 构建机，未显式降低部署目标） |
| Termux Android arm64 | Termux 应用环境，aria2/FFmpeg 由 `pkg` 提供 |

## 从 Releases 安装

打开 [GitHub Releases](https://github.com/Wong0728/bulibuli/releases)，下载与你的系统和 CPU 架构对应的 `portable` 归档，并下载同名 `.sha256` 文件。先校验 SHA-256，再解压到一个有写权限的目录。

| 平台 | 归档 | 运行方式 |
| --- | --- | --- |
| Windows x86_64 | `bulibuli-windows-x86_64-portable-*.zip` | 解压后运行 `bulibuli.exe`（PowerShell 启动器） |
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

不传 `-PackagePath` 时，本地包会优先于 Release；没有本地包时读取 Release 的 `latest.json`（仅指向正式版）。`latest.json` 拉取失败时的回退也只会在 Releases 清单中选取正式版（排除 draft 与 prerelease）：若当前只有 alpha 预发布版本，一键安装会终止并提示用 `BULIBULI_VERSION` 固定版本或直接下载归档。PATH 修改为用户级设置，完成后请重新打开终端。

**升级已有安装**：安装目录已存在时安装器会报错，重跑安装器升级需显式加 `-Force`：

```powershell
powershell -ExecutionPolicy Bypass -File .\install.ps1 -Force
```

`-Force` 为合并覆盖：`data/` 目录（数据库、下载、会话、配对状态）始终保留，但旧版本独有的文件可能残留；如遇异常建议手动删除旧安装目录（保留 `data/`）后全新安装。安装器中途失败不会自动回滚到旧版本，已复制的文件会留在安装目录；此时按上述方式删除后重装即可。

### Linux 一键安装

一键安装器会先检查包内运行时（完整包优先），再检查 `ARIA2C_PATH`、`FFMPEG_PATH`/`FFMPEG`/`FF_PATH`/`FFMPEG_HOME`/`FFMPEG_DIR`、`FFPROBE_PATH` 和系统 `PATH`（程序运行时探测 FFmpeg 也接受同一组环境变量）。依赖齐全时下载体积更小的 `core` 包；缺少 aria2c 或 FFmpeg 时才尝试系统包管理器，仍不可用才回退完整 `portable` 包。两种归档都会校验 SHA-256：

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

安装器支持 `install`、`run`、`service`、`unservice` 和 `status`。`service` 会优先使用用户级 systemd；root 安装会创建独立服务用户，`run` 与 `service` 统一使用 `/var/lib/bulibuli` 作为数据目录（可用 `BULIBULI_DATA_DIR` 覆盖），避免一台机器出现两套数据库。自定义数据目录时使用绝对路径，例如：

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

### 各平台升级方式

| 平台 | 升级方法 |
| --- | --- |
| Windows | 重跑安装器并加 `-Force`（见上方 Windows 一节；`data/` 保留，旧版独有文件可能残留） |
| Linux | 重跑远程一键命令 `curl -fsSL ... deploy/linux/install.sh \| bash`；已安装目录内的 `install.sh` 不自动升级（检测到旧版本会有提示）。升级保留 `data/`，若服务正在运行会提示先停止 |
| Termux | 重跑远程一键命令 `curl -fsSL ... deploy/termux/install.sh \| bash`；已安装目录内的 `install.sh` 不自动升级（检测到旧版本会有提示） |

任何升级入口都不会删除或覆盖 `data/`（数据库、下载、会话、配对状态）。

## 首次设置与常用操作

1. 启动程序并打开终端打印的本地 URL。
2. 首次设备配对码以醒目横幅在终端打印（请确保终端可见；以 nohup/systemd/Docker 方式运行看不到输出时，可执行 `bulibuli ctl pair` 重新生成，无需重启）。
3. 在网页中完成设备配对和安全设置，再按需扫码登录 B 站。
4. 完整包优先使用包内 aria2c 和 FFmpeg；轻量 `core` 包没有包内运行时时，按环境变量再到系统 `PATH` 查找，媒体探测会继续寻找 ffprobe 或回退 FFmpeg。

常用本机控制命令（需服务已运行；AI Skill 未启用时仍放行只读 `sys status`，其他高级命令需先执行 `ai on`）：

```text
bulibuli ctl sys status
bulibuli ctl dl status
bulibuli ctl cred qrcode
bulibuli ctl sys ffmpeg-test
```

AI 模式（`bulibuli ctl ai on`）开启后，AI 助手可执行与人工相同的全部操作，包括监听模式、IP 访问规则、GeoIP、信任外部 aria2/FFmpeg、设备配对等；命令需服务已运行。

查看版本和帮助不会加载配置、创建数据目录、启动数据库或启动服务：

```text
bulibuli --version
bulibuli --help
```

Linux/macOS 将 `bulibuli.exe` 替换为 `./bulibuli`。完整命令清单见 [`docs/skill.md`](docs/skill.md)。

## 数据、配置和日志

- 默认数据目录：可执行文件旁的 `data/`。
- 自定义数据目录：环境变量 `BILI__DATA_DIR=/absolute/path`。
- 配置真相源和 active/configured 生效契约见 [`docs/user/CONFIGURATION.md`](docs/user/CONFIGURATION.md)：`BILI__*` 环境变量负责启动入口，`data/security.toml` 负责网络与安全，SQLite `runtime_config` 负责可热更新业务设置，`startup_state.json` 负责 onboarding/AI/终端状态。网络模式写入后仍需重启；用 `bulibuli ctl sys status` 同时查看 active/configured/restart_required，不要只看文件是否落盘。
- 数据目录包含 SQLite 数据库、下载目录、`security.toml`、日志、迁移备份和运行状态；升级前请先停止程序并备份整个目录。设置页/API 的 `/api/backup` 只生成数据库快照，不包含密钥、设置文件或下载目录；需要完整恢复时使用 `/api/backup/full`，它生成带 `BACKUP-MANIFEST.json` 的完整恢复目录。恢复前必须停止程序，并同时恢复数据库、`security.toml`、`startup_state.json`、`.secret-store.key`（或对应系统密钥环/`BILI__MASTER_KEY`）和下载目录。
- 日志按天滚动。日志、数据库、Cookie、session 和配对码不要上传到 issue 或公开工单。
- Unix 控制 socket 优先使用 `XDG_RUNTIME_DIR`，深层数据目录不会再触发 Linux socket 路径过长；Windows 使用仅本机可访问的命名管道。
- 应用内更新：设置页可切换更新策略（仅提示 / 自动下载暂存 / 关闭）并手动"立即检查更新""立即更新"。自动更新只替换程序文件（Windows 启动器、`bulibuli-core.exe`、static、resources），不触碰 `data/`；更新完成后需重启程序生效，Windows 运行中更新会在退出程序后自动完成 Core 替换。更新全程先在临时目录完成下载与校验（SHA-256），替换失败时保留原版本可执行文件继续运行（Windows 上可能残留 `bulibuli-core.old.exe`，可在退出程序后手动删除），不会出现半新半旧的程序文件；下载/校验失败则原样保留当前版本。
- 内置 aria2c 启动时使用 `--stop-with-process`，Windows 另有 Job Object，Linux 另有父进程死亡信号；正常退出会先走 RPC 优雅关停，超时才强制回收。macOS/Termux 仍应在发布前实测强制结束和恢复场景，不能把静态回收逻辑当成多平台运行证明。

## 故障排查

- `/api/health` 返回服务存活状态，`/api/ready` 会同时检查数据库和 aria2。
- 下载失败先运行 `sys status`，确认 aria2 可用；再运行 `sys aria2-restart`。
- FFmpeg 检查使用 `sys ffmpeg-test`（读取设置里的 FFmpeg 模式与自定义路径，与设置页测试一致）。
- 无桌面、SSH、Docker 或 nohup 环境不会强行调用浏览器；直接打开终端输出的 URL。
- 如果安装器找不到最新版本，可用 `BULIBULI_VERSION=vX.Y.Z` 固定到 Releases 中明确存在的版本；也可以直接下载 portable 归档，不需要安装器。
- Linux 便携包中的运行时二进制带有独立 `.sha256` 文件；归档或运行时校验失败时停止安装，不要使用被替换的文件。

## 从源码开发

要求：Rust 1.97.1、Python 3.11+、Node.js 22+。Rust 版本由 `rust-toolchain.toml` 固定。

- **Vue 3 前端**（`web/` → `static/app/`，`/` 主路由）：`cd web && npm ci --ignore-scripts && npm run build`；CI 与 `python build.py` 会自动跑这一步。

常用命令：

```bash
cargo test --all-targets
cd web && npm ci --ignore-scripts && npm run build && cd ..
python build.py --check
python build.py --portable
```

跳过某个前端的写法：

```bash
# 复用已有 Vue 3 产物
python build.py --skip-frontend --portable
```

`python build.py --portable` 会生成桌面平台完整自包含包，或生成不内置媒体运行时的 Termux Android arm64 包；桌面 Unix 包缺少 aria2c、FFmpeg 或必要动态库时会拒绝组装。`python build.py --core` 生成不含媒体运行时的轻量命令包。每个包都会生成 `bulibuli.package.json`，记录版本、平台、架构、包类型和文件 SHA-256。推送 `v*` tag 后由独立的 Release 工作流构建并上传 Windows/Linux/macOS `portable` 包、Termux arm64 包、Windows/Linux `core` 包及 `latest.json`；已有 tag 也可以从 Actions 手动补发。详见 [云端发布架构](docs/user/RELEASES.md)。

## 文档与贡献

- [用户文档索引](docs/user/README.md)
- [贡献指南](CONTRIBUTING.md)
- [行为准则](CODE_OF_CONDUCT.md)
- [安全策略](SECURITY.md)
- [隐私说明](PRIVACY.md)
- [第三方资源与许可](NOTICE.md)
- [变更记录](CHANGELOG.md)
- [部署安全说明](deploy/SECURITY.md)
- [内置资源清单与校验](resources/README.md)

提交 issue 前请附上版本、平台/架构、启动方式、脱敏后的错误信息和复现步骤；不要附带 Cookie、token、完整绝对路径或整个 `data/` 目录。[提交 Issue](https://github.com/Wong0728/bulibuli/issues/new)

## 项目状态

`v2.0.0` 是当前正式版。升级前请备份 `data/`，不要让新旧版本同时写同一数据库。后续版本仍可能调整接口和数据结构。

## 关于未签名二进制

发布产物未做代码签名与公证（无 Windows Authenticode / macOS notarization）：Windows SmartScreen 与 macOS Gatekeeper 首次运行时可能提示"未知发布者"——请先按本页方法核对 SHA-256 再选择"仍要运行"（macOS 需在"系统设置 → 隐私与安全性"中点"仍要打开"，或对解压出的二进制执行 `xattr -d com.apple.quarantine`）。这也是推荐用 `install.ps1` / `install.sh` 安装的原因之一：安装器会先完成校验再落盘。

> **⚠️ Pre-release** — 这是 Alpha 预发布版本，可能存在未发现的问题。生产环境建议等待正式版。

## 变更摘要

- 完成全量代码审查整改（37 项）：修复直播录制会话条目泄漏、活跃 WebSocket 阻塞优雅关机、多 P 音频重试取错分 P、断点续传恢复失效、aria2 任务进度不更新、弹幕采集鉴权重连无退避等缺陷。
- 加固安全与健壮性：认证限流内存上限与近似 LRU 淘汰、CSRF 恒定时间比较、私网 IP 识别补全（IPv4-mapped/CGNAT）、安装器拒绝 manifest 外可执行文件、发布流程补齐依赖。
- 工程卫生：MSRV 声明对齐 1.97.1、发布构建启用 `--locked`、前端轮询守卫与属性转义补齐；`clippy -D warnings` 零警告，261 项测试与前端/冒烟测试全部通过。

完整变更记录见 [CHANGELOG.md](https://github.com/Wong0728/bulibuli/blob/__TAG__/CHANGELOG.md)。

---

## 下载选择

| 平台 / 场景 | 推荐下载 | 用途与启动方式 |
| --- | --- | --- |
| Windows x86_64 | [portable 完整包](https://github.com/Wong0728/bulibuli/releases/download/__TAG__/bulibuli-windows-x86_64-portable-__TAG__.zip) · [SHA-256](https://github.com/Wong0728/bulibuli/releases/download/__TAG__/bulibuli-windows-x86_64-portable-__TAG__.zip.sha256) | 解压后运行 `bulibuli.exe`；不需要另外安装 Rust、Node.js、aria2 或 FFmpeg。安装器从仓库单独下载，支持优先使用本地归档。 |
| Windows x86_64（已有运行时） | [core 轻量包](https://github.com/Wong0728/bulibuli/releases/download/__TAG__/bulibuli-windows-x86_64-core-__TAG__.zip) · [SHA-256](https://github.com/Wong0728/bulibuli/releases/download/__TAG__/bulibuli-windows-x86_64-core-__TAG__.zip.sha256) | 不含 aria2c/FFmpeg；适合本机已有并通过自检的运行时，或使用 Windows 一键安装器。 |
| Linux x86_64（完整包） | [portable 完整包](https://github.com/Wong0728/bulibuli/releases/download/__TAG__/bulibuli-linux-x86_64-portable-__TAG__.tar.gz) · [SHA-256](https://github.com/Wong0728/bulibuli/releases/download/__TAG__/bulibuli-linux-x86_64-portable-__TAG__.tar.gz.sha256) | 解压后运行 `./bulibuli`，或运行 `./install.sh run`；适合本机没有 aria2c/FFmpeg 的用户。 |
| Linux x86_64（已有运行时） | [core 轻量包](https://github.com/Wong0728/bulibuli/releases/download/__TAG__/bulibuli-linux-x86_64-core-__TAG__.tar.gz) · [SHA-256](https://github.com/Wong0728/bulibuli/releases/download/__TAG__/bulibuli-linux-x86_64-core-__TAG__.tar.gz.sha256) | 不含 aria2c/FFmpeg；适合本机已有这两个程序，或使用下面的一键安装器。 |
| macOS Intel | [portable 完整包](https://github.com/Wong0728/bulibuli/releases/download/__TAG__/bulibuli-macos-x86_64-portable-__TAG__.tar.gz) · [SHA-256](https://github.com/Wong0728/bulibuli/releases/download/__TAG__/bulibuli-macos-x86_64-portable-__TAG__.tar.gz.sha256) | Apple Intel Mac 解压后运行 `./bulibuli`。 |
| macOS Apple Silicon | [portable 完整包](https://github.com/Wong0728/bulibuli/releases/download/__TAG__/bulibuli-macos-arm64-portable-__TAG__.tar.gz) · [SHA-256](https://github.com/Wong0728/bulibuli/releases/download/__TAG__/bulibuli-macos-arm64-portable-__TAG__.tar.gz.sha256) | M1/M2/M3/M4 等 Apple Silicon Mac 解压后运行 `./bulibuli`。 |
| Termux Android arm64 | [预编译包](https://github.com/Wong0728/bulibuli/releases/download/__TAG__/bulibuli-termux-arm64-portable-__TAG__.tar.gz) · [SHA-256](https://github.com/Wong0728/bulibuli/releases/download/__TAG__/bulibuli-termux-arm64-portable-__TAG__.tar.gz.sha256) | 解压后运行 `bash install.sh`；只需通过 `pkg` 安装 curl、Python、aria2 和 FFmpeg，不需要 Rust、Node.js 或本机编译。 |

Windows 安装器：[下载 `install.ps1`](https://raw.githubusercontent.com/Wong0728/bulibuli/main/deploy/windows/install.ps1)。它独立于发布包，运行时会优先使用当前目录、源码 checkout 目录或本地缓存中的已校验归档。

## 怎么使用

### portable 完整包

完整包已经包含对应平台的程序、前端、GeoIP 数据、aria2c、FFmpeg 及必要动态库。下载归档和同名 `.sha256` 后，先校验 SHA-256，再解压并启动：

```bash
# Linux/macOS
sha256sum -c bulibuli-*-portable-__TAG__.tar.gz.sha256  # macOS 可使用 shasum -a 256 -c
tar -xzf bulibuli-*-portable-__TAG__.tar.gz
cd bulibuli-*-portable-__TAG__
chmod +x bulibuli resources/aria2c resources/ffmpeg
./bulibuli
```

Windows 解压 `.zip` 后直接运行 `bulibuli.exe`。首次启动后，程序会在终端显示本地地址和设备配对码；在浏览器打开该地址完成首次设置。

### Termux 预编译包

Termux Release 包针对 Android arm64 构建，下载并校验后解压：

```bash
sha256sum -c bulibuli-termux-arm64-portable-__TAG__.tar.gz.sha256
tar -xzf bulibuli-termux-arm64-portable-__TAG__.tar.gz
cd bulibuli-termux-arm64-portable-__TAG__
bash install.sh
```

`install.sh` 会通过 Termux `pkg` 安装 curl、Python、aria2 和 FFmpeg，然后直接运行云端编译好的 bulibuli。需要自动下载当前版本时，也可以执行下面的命令；安装完成后，使用 `$PREFIX/opt/bulibuli/install.sh` 管理后台进程：

```bash
curl -fsSL https://raw.githubusercontent.com/Wong0728/bulibuli/main/deploy/termux/install.sh | bash
```

```bash
bash "$PREFIX/opt/bulibuli/install.sh" start
```

### Linux 一键安装

安装器会优先检查包内运行时，再检查环境变量和系统 `PATH` 中的 aria2c/FFmpeg；本机依赖齐全时下载更小的 `core` 包，缺少依赖时自动回退完整 `portable` 包：

```bash
curl -fsSL https://raw.githubusercontent.com/Wong0728/bulibuli/main/deploy/linux/install.sh | bash
~/.local/share/bulibuli/install.sh run
```

`core` 包直接运行时必须由环境变量或系统 `PATH` 提供 aria2c 和 FFmpeg；不确定时请下载 Linux `portable` 完整包，或使用上面的一键安装器。

## 校验与版本

- 每个归档旁边的 `.sha256` 文件用于校验下载完整性。
- 桌面平台的 `portable` 是 GitHub 页面上推荐的完整自包含包；Termux 预编译包只包含 Android/Termux 的 bulibuli 和前端，aria2/FFmpeg 由 Termux `pkg` 提供。
- `core` 提供 Windows/Linux 轻量命令包，用于复用本机运行时；目标平台没有 core 资产时，安装器会回退到完整包。
- 安装器默认读取 Release 的 `latest.json`，当前只有 Alpha 时会回退读取预发布清单；需要离线或可复现安装时，请显式指定版本。
- 详细变更记录见 [CHANGELOG.md](https://github.com/Wong0728/bulibuli/blob/__TAG__/CHANGELOG.md)。

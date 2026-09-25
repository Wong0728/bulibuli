> **补丁版本** — 累计上一正式版之后的修复，发布包经过 Actions 质量门禁、清单校验与 SHA-256 校验。本版含多项安全加固，已在用请升级。

## 本版要点

- **安全**：数据目录与备份目录权限收紧到 `0700`（完整备份会把主密钥文件带进受 umask 影响的宽松目录）；风控、登录过期、磁盘满等系统事件改为按角色投递，不再广播给所有已连接设备；反代（Proxy）部署不再把代理的回环地址当作本机客户端，远程设备不能再让服务端弹出资源管理器；配置了按 IP 的访问限制却拿不到真实客户端地址时直接 403 并给出可执行的修复指引，不再静默放行；父域可种的 Cookie 不再遮蔽合法会话；会话凭据的落盘与外发面同步收窄。
- **B 站链路稳定性**：博主扫描、看板刷新等批量任务改用独立的后台限流桶，跑批期间前台搜索与刷新不再排队到超时；被风控或登录态失效时明确弹窗提示（同类事件 60 秒冷却），不再"什么都拉不到却毫无提示"；连点刷新不再叠出多轮全量扫描。
- **数据正确性**：投稿超过 50 个的 UP 主，其已下载视频不再被监控误判为"已下架"（分页大小前后不一致留下的扫描空洞）；投稿日期与评论/弹幕导出的时间戳改按本机时区，深夜投稿不再归到前一天，历史数据由迁移自动回填；深分页越界时不再谎报"还有下一页"。
- **下载与直播可靠性（本版重点）**：永久失败的视频任务不再每 30 秒无限重试请求 B 站接口（自动重试计数在重新派发时被错误清零，导致"最多 3 次"上限形同虚设）；一次网络卡顿不再毁掉整场录像（被强杀的 FLV 分段时长为 0，此前被当作分段损坏，直接判整场合并失败并**删除已合并好的成品**）；主程序被强杀不再留下 FFmpeg 孤儿进程继续写满磁盘（Windows 绑 Job Object，Linux/Android 随父进程退出）；优雅关机不再砍断收尾合并，中断的合并会在下次启动自动补做；看板"失败"页签此前没有任何数据源、永远空白，现在能看到失败任务。
- **首次启动与配对**：配对失败给出真实原因而不是一个 401 和误导文案；Setup 端口在向导完成前先进向导；配对码支持整段粘贴；Windows 上 `ctl` 命令不再每条卡满 30 秒才返回。
- **界面**：修复扫码登录框里 B 站图标显示成"方框打叉"及两处图标空白；系统偏好为深色时，在应用内手选"浅色"现在真正生效（此前被深色媒体查询覆盖，浅色开关形同虚设）；系统深色主题不再被一次实时推送打回浅色；看板历史翻页数千条不再卡顿；单次网络超时不再被判成"已离线"并锁住所有操作按钮；实时消息去重、图标字体预加载与若干键盘可达性一并补齐。
- **风控特征与边界一致性**：字幕请求的 UA 不再与同一会话的设备指纹上报相矛盾（一份会话出现两种客户端特征本身就是风控信号）；直播轮询的风控退避改按错误类型判定，不再漏掉两类风控码；playurl 的 MCDN 兜底候选不再被自家 URL 策略必然拒绝。侧车（弹幕/评论）写盘目录解析的一条分支补回下载根目录校验，被污染的历史记录不再能把文件写到下载根之外。重试中的任务纳入"全部暂停"与单个暂停的可操作集合。
- **直播录制不再谎报成品**：合并失败的记录此前会把第一个分段当成完整成品落库，而烧录入口只看"文件在不在"——你以为整场都录下来了，实际只能烧到第一个分段。现在这类记录不再承诺成品并提示先重试合并（已录的历史记录同样受保护）。另外修掉两处真正会丢内容的缺陷：合并成功后先删分段再写库，写库失败即成品无人认领且重试永远报"无分段"；以及没有音频轨的直播间（纯画面/静音场次）合并必然判失败——卡点是产物校验硬性要求同时存在音视频轨，不是 ffmpeg 参数问题。四处无超时的外部探测一并补上，避免任务永久停在"启动中/收尾中"。
- **命令行 `ctl` 不再假成功**：服务端零字节关闭连接时，此前会打印一个空行并 `exit 0`，脚本与 AI 按退出码判断时会以为命令生效了。
- **录制不再被误停、不再留打不开的归档**：磁盘空间探测失败（权限、临时 IO 错误、网络盘）此前被当作"空间为 0"而直接停录一场本来能正常录的直播；弹幕归档在收尾超时时会留下语法不完整、事后打不开的半截 JSON/XML，现在最终文件名要么完整要么不存在。更新包解压前会先校验归档内部条目名，不再把"程序文件不被写到别处"寄托在用户机器上装的是哪个 tar/PowerShell 上。
- **博主监控不再吃掉你的改动**：扫描结束时整行回写快照，会覆盖你在扫描期间做的停止监控、改检查间隔、改活跃时段等操作，并且连配置版本号一起退回旧值、绕过乐观锁。四处已改为只推进需要更新的列。
- **门禁**：`cargo clippy --all-targets -- -D warnings` 零警告，399 项 Rust 测试与前端构建/冒烟测试全部通过（测试数以发布当天 `cargo test --all-targets` 为准，改文案时别照抄）。

## 暂不支持（请按这些边界理解能力，避免与其它工具的习惯对比）

- **画质**：下载画质白名单上限为 `125`（HDR 真彩）。8K（`127`）与杜比视界（`126`）只有 B 站在线观看可用，下载链路不支持；设置里选择后会按可用画质降级。
- **链接类型**：支持 BV / AV / 番剧 `ep` / 赛季 `ss` / 课程 `fp` 以及 `b23.tv` 短链。收藏夹、稍后再看、合集·链式、动态、音乐 `au`/`am`、每周必看、互动视频这类链接**暂不解析**，粘贴后会返回"不支持的B站链接"；这类内容请改用具体视频链接，或按 UP 主 UID 载入其投稿列表再挑。
- **更新清单信任根**：`latest.json` 提供同名 `.sha256` 用于校验下载完整性，但清单本身还没有独立签名，信任根等同 GitHub Release 本身。

完整变更记录见 [CHANGELOG.md](https://github.com/Wong0728/bulibuli/blob/__TAG__/CHANGELOG.md)。

---

## 下载选择

| 平台 / 场景 | 推荐下载 | 用途与启动方式 |
| --- | --- | --- |
| Windows x86_64 | [portable 完整包](https://github.com/Wong0728/bulibuli/releases/download/__TAG__/bulibuli-windows-x86_64-portable-__TAG__.zip) · [SHA-256](https://github.com/Wong0728/bulibuli/releases/download/__TAG__/bulibuli-windows-x86_64-portable-__TAG__.zip.sha256) | 解压后运行 `bulibuli.exe`（PowerShell 启动器）；不需要另外安装 Rust、Node.js、aria2 或 FFmpeg。安装器从仓库单独下载，支持优先使用本地归档。 |
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

- 每个归档旁边的 `.sha256` 文件用于校验下载完整性；更新清单 `latest.json` 同样附带 `latest.json.sha256`（核对发布产物内部自洽，不是独立签名）。
- 桌面平台的 `portable` 是 GitHub 页面上推荐的完整自包含包；Termux 预编译包只包含 Android/Termux 的 bulibuli 和前端，aria2/FFmpeg 由 Termux `pkg` 提供。
- `core` 提供 Windows/Linux 轻量命令包，用于复用本机运行时；目标平台没有 core 资产时，安装器会回退到完整包。
- 取包口径按平台不同：Windows 安装器读 Release 的 `latest.json`（`releases/latest` 只解析最新正式版），Linux/Termux 安装器直接查 Releases 列表、默认取最新一条（含预发布，`BULIBULI_STABLE_ONLY=1` 可只在正式版中解析）。需要离线或可复现安装时显式指定版本（`-Version vX.Y.Z` 或 `BULIBULI_VERSION=vX.Y.Z`）。
- 详细变更记录见 [CHANGELOG.md](https://github.com/Wong0728/bulibuli/blob/__TAG__/CHANGELOG.md)。

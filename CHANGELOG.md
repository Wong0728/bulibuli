# Changelog

## v2.0.0

- Rust v2 正式版：汇总 Alpha 阶段的功能、稳定性、安全与跨平台打包修复，Windows、Linux、macOS 和 Termux 发布包统一经过质量门禁与 SHA-256 校验。
- 启动与生命周期修复：后台服务在主端口绑定前完成启动，Setup 端口可独立提供首次配置，aria2 失联恢复、任务进度落库、更新替换和 Windows 子进程回收更加可靠。
- 发布包改进：Windows portable/core 包通过 PowerShell 启动器分离启动层与 Core 程序，所有正式版资产和 `latest.json` 均由 Release 工作流生成并校验。

## v2.0.0-alpha.11

- 首次启动修复：后台服务（monitor/download_manager 等）改为在绑定主端口之前完成启动，消除"端口已监听但 HTTP 尚未 accept"导致首次启动 5000 端口无响应、重启后才正常的竞态窗口。
- 配对页修复：`/api/auth/state` 轮询失败（含触发限流 429）时前端自动放慢到 15 秒退避、恢复后回到 5 秒，不再与探测限流互相续费造成死锁；配对页提示补充"执行 `bulibuli ctl pair` 可查看/重新生成配对码"。
- Setup 端口（5001）修复：向导端口不再 307 重定向到主端口，改为直接提供向导页面与所需 API（静态页面 / health / auth / setup），首次启动主端口尚未就绪时向导依然可用。
- 打包修复：portable/core 包随包分发 `docs/` 目录——`/api/foundation/status` 与 `/api/setup/status` 返回的 `ai_skill_path` 指向 `<app_root>/docs/skill.md`，此前缺失导致 AI Skill 路径失效。
- `/api/download/add` 的 `title` 改为可选字段：缺省时自动通过视频信息接口补全标题，直接 `POST {"bvid": ...}` 不再报 missing field title。
- ctl 权限边界：默认（AI Skill 关闭）额外放行只读诊断命令 `sys status`，与 `--help` 推荐的新手命令保持一致。
- 体验修正：无图形桌面环境（无头服务器）启动时不再输出"正在自动打开浏览器..."，改为提示手动访问管理地址；配对后未登录 B 站账号的横幅文案改为明确的"下一步必做"引导。
- 稳定性加固（发版前审查修复轮）：aria2 失联判定加 30 秒宽限窗口（覆盖应用重启抖动，不再把下载中任务批量误标 failed）；进度落库 writer 任务死亡后 submit 侧丢弃快照并降级，防止内存无界增长；更新器写盘改走 tokio::fs 异步接口，Windows 更新替换失败时回滚旧 exe 文名；更新清单 `download_url` 增加信任域白名单（仅 GitHub Release 资产域）；关闭 TLS 校验时健康接口返回用户可见风险警告。
- 打包安全加固：`build.py` 组包前强制校验 `resources/` 哈希（被替换的运行时二进制拒绝入包）、打包前清扫 dist/ 旧产物、Windows 运行时不回退 PATH 中的同名二进制；Linux 安装器内嵌校验显式拒绝非 x86_64 架构包。
- 依赖卫生：升级 vite 5→6 及若干依赖后移除 deny.toml 中已失效的 RUSTSEC 豁免（paste/anyhow/rust_decimal-rkyv 三条）。
- 发版流程改进：Release workflow 支持 `workflow_dispatch` dry-run（`checkout_ref` 指定分支、`publish=false` 不建 Release），打 tag 前先在 main 上干跑验证，避免 tag 反复重移；自建 Linux FFmpeg 导出 ldd 动态依赖清单，打包容器按清单复检缺失库并点名具体库名。
- 说明：Linux portable 包体积自 alpha.9 起明显下降（约 110MB → 41MB）属预期——aria2c/FFmpeg 改为动态链接 + wrapper 启动，仅复制非系统共享库，功能不受影响。

## v2.0.0-alpha.10

- Linux 兼容性修复：Release 构建迁移到 ubuntu:22.04 容器（产物 glibc ≤ 2.35，修复在 Ubuntu 22.04 LTS / Debian 12 / RHEL 9 等主流发行版上因 `GLIBC_2.39 not found` 完全无法运行的问题）；README 新增 Quick Start 与最低系统要求。
- 安装器修复（Linux/Termux）：版本解析改为直接走 Releases API 并默认跟随最新 Release（含预发布）——此前纯 alpha 阶段"只认正式版"的过滤会导致一键安装直接失败；`BULIBULI_STABLE_ONLY=1` 可恢复旧行为；系统依赖自动安装失败不再透出 sudo 原始报错，统一给出中文提示后回退 portable 包。
- 打包修复：`build.py` 不再对共享 `lib/` 目录里上一轮生成的 `.sha256` 重复写校验和，消除 `.sha256.sha256`/`.sha256.sha256.sha256` 嵌套文件。
- 控制通道修复（Unix）：候选目录先归权（0700）再校验属主/模式——此前 umask 产生的 0755 目录会被自己拒绝，导致容器等环境下 ctl 命令不可用；新增回归单测。
- Linux 包体积优化：FFmpeg/ffprobe 换为 Actions 自编译精简构建（ubuntu:22.04 容器内原生编译，glibc 2.35 基线，能力集与 Windows 版一致），替代发行版完整 FFmpeg 及其递归拖入的全部动态库；`build.py` 支持 `BULIBULI_RUNTIME_DIR` 预置自建运行时。
- 使用体验：首次设备配对码改为醒目横幅输出并提示 `bulibuli ctl pair` 可重新生成（README 同步更正"只能重启重新生成"的过时说明）；setup server 日志说明其用途与生命周期；aria2 启动竞态期的 RPC 重试日志降为 debug（重试耗尽才 WARN）。

## v2.0.0-alpha.9

- 运行时替换：Windows 包内 FFmpeg/ffprobe 换为本仓库 Actions 自编译的精简构建（FFmpeg n8.1.2，GPL-2.0+，含弹幕烧录所需 libass/libx264 全能力，替换原 gyan.dev 5.1.6 通用构建）；NOTICE 与 resources 清单同步更正许可声明，x264 固定 commit、依赖 tarball 增加 SHA-256 校验。
- 前端产物目录统一：Vue 构建产物固定为 `static/app/`（根路由唯一入口），移除旧版 `static/index.html`、`static/js/`、`static/css/` 与 `settings.html`/`setup.html` 独立页面；三平台安装器与 build.py 校验路径同步。
- 发版前审查修复（高危项）：clippy `-D warnings` 门禁恢复零警告；release profile 改回 `panic=unwind` 使后台任务 panic 兜底（catch_unwind 状态回滚）真正生效；`ctl audit --since` 多字节输入不再 panic；Linux/Termux 安装器内嵌校验改用 `static/app/index.html`（修复全新安装无声失败）；`static/app/` 全量 hash 资产随源码入库；aria2 不再将含 Cookie 的下载项明文写入 session 文件（重启恢复改由应用重新解析 URL）。

- 数据安全：Linux 安装器重装/升级保留 `data/`（与 Windows/Termux 对齐），运行中先提示停止服务再升级；root 下 `run` 与 `service` 统一数据目录 `/var/lib/bulibuli`；三平台重跑脚本均有明确的升级/最新/需 `-Force` 提示。
- 行为修复：网络恢复重试失败任务正确携带 `since`（不再静默重试全部历史失败任务）；删除 `ai assist` 短时授权层——AI 模式开启后 AI 与人工权限一致（README/`--help`/设置向导已写明）；默认画质统一为 1080P（80），用户修改设置后抽屉与链接下载均按设置值；抽屉移除 8K/杜比视界下载项（后端白名单最高 125），设置页相应加注。
- 修复：`sys logs` 与 TUI 退出摘要使用实际日志目录；`refresh video` 查不到记录返回 404；迁移 002 补列存在性守卫；`/settings.html` 直链 302 回主界面（片段改走 `/_fragments/settings.html`）；`sys ffmpeg-test` 与设置页测试一致；网页退出登录补调设备会话注销；`storage.history_limit` 接入历史清理（设置值优先、环境变量兜底），移除 `uid_history_limit`/`prefer_audio_quality`/`danmaku_download_all` 死配置项；Operator 设置页只读并提示仅 Owner 可修改；博主载入配置按 UID 取真实自动任务配置；`blg add` 重复返回友好 Conflict；`audit list` 非法 `--source/--since` 报错；ctl 移除半实现的 `dl burn` 入口。
- 功能上线：下载优先级 −/＋ 控件（Web 与 `ctl dl priority` 共用后端）、下载管理队列摘要（各状态任务数/等待重试）、设置页全局日志（跨博主、15 秒轮询）、全部暂停/全部恢复按钮；删除设置页"测试下载"调试入口。
- 新功能：应用内更新机制（设置页策略：仅提示/自动下载暂存/关闭 + 立即检查 + 立即更新），只替换程序文件、不触碰 `data/`；Windows 运行中更新在退出程序后自动完成替换。
- 云端与 CI：Hero 页补 Windows 一键安装命令；Quality gates 四个 job 全部加超时上限防止卡死；CHANGELOG 补齐 alpha.6 段并标注 alpha.5/6/7 为补发。

## v2.0.0-alpha.8

- 完成全量代码审查整改（37 项）：修复直播录制会话条目泄漏、活跃 WebSocket 阻塞优雅关机、多 P 音频重试取错分 P、断点续传恢复失效、aria2 任务进度不更新、弹幕采集鉴权重连无退避等缺陷。
- 加固安全与健壮性：认证限流内存上限与近似 LRU 淘汰、CSRF 恒定时间比较、私网 IP 识别补全（IPv4-mapped/CGNAT）、安装器拒绝 manifest 外可执行文件、发布流程补齐 `windows-build` 依赖。
- 工程卫生：MSRV 声明对齐 1.97.1、发布构建启用 `--locked`、前端轮询守卫与属性转义补齐；`clippy -D warnings` 零警告，261 项测试与前端/冒烟测试全部通过。

## v2.0.0-alpha.7

> ⚠️ 本版本于 2026-08-17 补发（发布时间晚于 alpha.8，内容与 alpha.8 相同），请优先使用 alpha.8。

- Termux 默认始终使用 GitHub Actions 云端预编译包；只有显式设置 `BULIBULI_SOURCE_BUILD=1` 才会从源码构建。
- 修正 Termux 远程安装后的后台启动路径，并同步 README、Release 说明和 Hero 页面。
- 修复 Hero 窄屏横向溢出，保证手机浏览器可正常浏览和操作。

## v2.0.0-alpha.6

> ⚠️ 本版本于 2026-08-17 补发。Termux 预编译包发布流程调整，与 alpha.5 一并完成；变更内容与 alpha.8 相同，请优先使用 alpha.8。

## v2.0.0-alpha.5

> ⚠️ 本版本于 2026-08-17 补发（发布时间晚于 alpha.8，内容与 alpha.8 相同），请优先使用 alpha.8。

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

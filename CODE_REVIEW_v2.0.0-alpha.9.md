# BuliBuli v2.0.0-alpha.9 发版前代码审查报告

- **审查日期**：2026-08-22
- **审查方式**：9 路并行深度审查（Rust 代码质量 / 安全 / B 站 API 客户端 / 下载录制核心 / 数据库与配置 / Vue 前端 / TUI-CLI-IPC-日志 / CI-测试-build.py / 文档-隐私-合规），叠加本地 `cargo clippy --all-targets --locked -- -D warnings` 实测
- **审查范围**：`src/`（约 5.3 万行 Rust）、`web/`（约 1.2 万行 TS/Vue）、`build.py`、`.github/workflows/`（5 个）、`deploy/`（4 个安装器 + caddy）、`docs/` 与全部根文档
- **严重级别**：高 = 发版阻塞或可致崩溃/失实声明；中 = 可靠性/安全/一致性隐患，建议随本版修复；低 = 代码卫生与改进项

---

## 总体结论

代码库整体工程质量**显著高于同规模项目**：panic 纪律、锁作用域、unsafe 注释、路径安全双层校验、恒定时间比较、三层 SSRF 防护、Cookie 加密链路、发布三重校验等均属扎实。**未发现可远程直接利用的安全漏洞、未发现必然导致数据损坏的路径。**

但存在 **8 项发版阻塞/高危问题**，其中 3 项属于"发出去就坏"：

1. **clippy `-D warnings` 当前失败**（CI 门禁会挂）；
2. **Linux/Termux 一键安装链路校验断链**（下载解压全部成功后安装器直接失败）；
3. **NOTICE.md 的 FFmpeg 许可声明与实际捆绑物失实**（GPL 版本与来源均错）。

另有 1 项系统性风险：`[profile.release] panic = "abort"` 使全代码库精心构建的 panic 兜底机制在 release 全部失效，且存在一条**用户输入即可触发的 panic → 整进程 abort** 路径（`ctl audit list --since 1小时`）。

统计：**高 8 / 中 27 / 低 55**。

---

## 一、发版阻塞 / 高危（8 项）

### H1. clippy `-D warnings` 失败，CI 门禁会挂 【代码质量】
- **位置**：`src/services/download/history_sync.rs:28`
- `ensure_history_placeholder` 8 个参数超过 7 上限，触发 `clippy::too_many_arguments`，`cargo clippy --all-targets --locked -- -D warnings` 实测编译失败（本地已复现）。
- **修复**：合并参数为结构体（如 `HistoryPlaceholderParams`）或 `#[allow]` 并说明理由。这回答了"零警告是否仍成立"：**不成立，新代码引入了新警告**。

### H2. `panic = "abort"` 使全部 panic 兜底机制在 release 失效 【代码质量/下载核心/TUI，4 路审查独立确认】
- **位置**：`Cargo.toml:118`（`[profile.release] panic = "abort"`）
- 受影响机制（release 下全部为死代码）：
  - `src/services/spawn_util.rs:35-46` — `catch_unwind` + `on_panic` 状态回滚（烧录任务 panic 后状态永远停留在 processing 的防线）；
  - `src/services/video_processor/merge.rs:145-157` — 合并任务 panic 清理 tasks 映射；
  - `src/api/download/burn.rs:164-168,287-296` — panic 时置 failed 的回调；
  - `src/app/tui.rs:227-231` — TUI 线程 panic 恢复；
  - `src/services/download.rs:155-165` 等全部 poison-safe 锁处理。
- **影响**：任何后台任务 panic 直接 abort 整个服务（含进行中的录制/下载/烧录，经 Job Object 连带杀掉 aria2c），状态不落库；dev（unwind）与 release（abort）行为分叉，测试覆盖的是不上线的路径。
- **修复**：release 改回 `unwind`（体积损失很小）；若坚持 abort，删除上述死代码、为所有后台任务做无 panic 审计，并在 main 早期 `std::panic::set_hook` 中恢复终端状态后 abort。

### H3. `ctl audit list --since 1小时` → panic → 整进程 abort（TUI 下终端卡死）【TUI/IPC】
- **位置**：`src/app/control.rs:1990`（`trimmed.split_at(trimmed.len() - 1)` 切在 UTF-8 多字节字符中间）；同类隐患 `src/services/audit_log.rs:285`
- 含中文单位的 `--since` 值（如 `1小时`）即触发；TUI/stdin 人工输入不受 AI 门控限制。叠加 H2 的 panic=abort，一个输入即可让服务 abort 且终端停留在备用屏 + raw mode。
- **修复**：改用 `chars().last()` / `char_indices` 按字符边界切分，补多字节单测。

### H4. Linux/Termux 安装器包校验仍要求已删除的 `static/index.html`，一键安装链路断链 【安装器/部署】
- **位置**：`deploy/linux/install.sh:100`、`deploy/termux/install.sh:126`（内嵌 Python `verify_package_manifest` 要求 `static/index.html`）
- 前端已统一为 `static/app/index.html`（两脚本的 `detect_layout` 已同步、`build.py:460` 与 `install.ps1:98` 均已改），唯独内嵌校验漏改。按当前工作区发版：Linux 全新安装在归档 SHA-256 全部通过后于 `download_release`（install.sh:296，`set -e`）无声死亡；Termux 报 `missing package file: static/index.html` 中止；Linux 已安装目录检测被 `|| true` 吞掉，版本判断与旧运行时复用全部失效。
- **修复**：两处同步改为 `static/app/index.html`，并补安装器校验逻辑冒烟测试。

### H5. NOTICE.md 的 FFmpeg 声明与实际捆绑物不符（GPL 归属失实）【打包/许可】
- **位置**：`NOTICE.md:42-48`
- NOTICE 仍写 gyan.dev 构建 5.1.6/GPLv3；实际为仓库 Actions 自编译 n8.1.2 精简版（GPL-2.0+，链接 libx264，见 `resources/README.md:11` 与 commit 161a399）。GPL 版本、源码提供途径、版本号三项均失实，且与 `resources/README.md` 互相矛盾。
- **修复**：重写该节（自编译 n8.1.2 / GPL-2.0+ / 源码指向上游 tag 与本仓库 build-ffmpeg.yml），随 alpha.9 CHANGELOG 记录资源切换。

### H6. `static/app/` 构建产物入库且当前不同步，存在发布白屏风险 【前端/打包】
- **位置**：`.gitignore`（忽略 `/static/dist/` 但未忽略 `/static/app/`）；当前 git 状态：`M static/app/index.html`、旧 hash 资产已删、新 hash 资产**未跟踪**
- 若提交新 index.html 而漏提交未跟踪的新 hash 资产，fresh clone 后服务能启动但 `/app/assets/*.js` 404 → 前端白屏；`build.py --check` 校验的是刚重建的新产物（build.py:207、808 先构建再检查），检不出"入库产物不自洽"。
- **修复**：发 tag 前重提交当前完整产物；长期将 `/static/app/assets/` 移出 git（CI 生成）或增加"clone 后不重建直接校验入库 bundle"的 CI 步骤。

### H7. build-ffmpeg.yml 的 x264 克隆 master 浮动 HEAD，"已审计资源"不可复现重建 【CI/供应链】
- **位置**：`.github/workflows/build-ffmpeg.yml:51`（`git clone --depth 1` 无 `--branch`/固定 commit）
- `resources/BUILD_INFO.txt` 记录了 commit（0480cb05），但 workflow 无法重建它；未来重跑会链入不同 x264 代码，对 `resources/ffmpeg.exe` 的审计结论失效。
- **修复**：固定 tag/commit 并入 env 固定段（与 FREETYPE_VER 等同级）。

### H8. 核心模块零单元测试 【测试】
- 以下文件无任何 `#[test]`/`#[tokio::test]`：
  - `src/services/secret_store.rs`（凭据加密存储，15KB）
  - `src/services/aria2/rpc.rs`、`aria2/process.rs`、`aria2/win_job.rs`
  - `src/services/download/` 下 `manager.rs`、`queue.rs`、`storage.rs`、`dispatch.rs`、`history_sync.rs`、`audio_retry.rs`、`video_retry.rs`、`post_process.rs`、`monitor.rs`
  - `src/services/refresh.rs`（19KB）、`live_monitor.rs`（17KB）、`blogger.rs`（18KB）、`verify.rs`
- 下载编排、队列持久化、aria2 RPC 协议这些发版核心路径完全依赖人工验证；`cargo test` 绿灯不覆盖它们。全库共 291 个测试函数（234 `#[test]` + 57 `#[tokio::test]`）+ 3 处 proptest，分布不均是主要短板。
- **修复**：发版前至少为 download/manager+queue 状态机、aria2/rpc 请求响应解析（纯函数部分）补测试。

---

## 二、中危（27 项）

### 安全（4）

| # | 位置 | 问题 |
|---|---|---|
| S1 | `src/app/security_server.rs:149-195` | governor `RateLimiter::keyed` 无内存上限，key 永不回收、无 `shrink_to_fit()`；`/api/auth/pair` 为公开路径，被限流拒绝的请求也会创建 key。lan/proxy 模式下攻击者轮换 IPv6 源地址可耗尽内存（对比 auth.rs:95-110 的有界 LRU）。修复：定期 shrink 或公开路径排除出 keyed limiter |
| S2 | `src/app/control.rs:346`、`:43` | Windows 命名管道客户端不校验服务端身份：低权进程可预先创建 `\\.\pipe\bulibuli` 抢占，ctl 客户端把命令发给攻击者并接受伪造响应（还可被 Impersonate）；Unix 侧有属主/权限校验，Windows 无对等。附带：固定全局管道名使同机第二用户实例 ctl 静默失效 |
| S3 | `src/services/aria2/rpc.rs:240` + `aria2/process.rs:131-132` | aria2.session（30s 周期 + 退出保存）可能明文持久化带 Cookie 的下载项 header，与 DB 侧 AES-GCM 加密 Cookie 的威胁模型不一致。修复：完成后清理 session 项、Unix 0600 收权，或改 `--load-cookies` |
| S4 | `src/api/foundation.rs:42-49`、`src/api/settings.rs:328-333` | Viewer（只读角色）可读取 `ai_skill_path`、FFmpeg 本机绝对路径，与"Viewer 仅可查看内容"定位不符。修复：非 Owner 抹去绝对路径 |

### B 站 API 客户端（4）

| # | 位置 | 问题 |
|---|---|---|
| B1 | `src/services/wbi.rs:43-77` + `bili_api/client.rs:117` | **WBI 签名编码不一致**：`enc_wbi` 用 `urlencoding::encode`（空格→`%20`），实际发送经 reqwest `.query()`（空格→`+`）。搜索关键词含空格时签名校验失败 → 请求被拒并误报风控横幅。修复：签名后直接用已编码 query 串拼 URL 发请求 |
| B2 | `src/services/bili_api/client.rs:113,150` | **限流配额双扣减**：`build_get_request` 与 `send_with_retry` 各扣一次令牌，前台 5rps 实际约 2.5rps；`live.rs:40` 的后台批量探测过了 background limiter 后仍消耗前台配额，前后台隔离设计未生效 |
| B3 | `src/services/danmu_collector/mod.rs:108,165,211-216` | 弹幕采集重连预算**全会话累计、成功不归零**：长录制中第 4 次网络闪断即永久熔断弹幕采集（录制继续但互动统计缺失，无自动恢复）。退避 1/2/4s 无 jitter |
| B4 | `src/services/bili_api/client.rs:122-126` | per-request 硬编码 5s/10s 超时（`RequestBuilder::timeout` 优先于 client 级）使 `BILI_API_TIMEOUT` 配置对这些请求**完全无效**；新版 getRoomPlayInfo 拿 5s、旧版 playUrl 拿 10s，与"新版优先"权重相反 |

### 下载 / 录制核心（5）

| # | 位置 | 问题 |
|---|---|---|
| D1 | `src/services/subtitle_burner/burn.rs:358-374` | 烧录把源视频整卷复制到 `%TEMP%`（系统盘）再输出，无磁盘预检：20GB 视频需系统盘 ~40GB 额外空间 + 三遍 IO；对比直播合并与下载路径都有 `ensure_disk_space`，唯独烧录没有 |
| D2 | `src/services/live_recorder/mod.rs:804,1537` | 录制 worker / StopBackground 收尾未用 `spawn_logged_with_panic`：worker panic（unwind 构建下）→ Active 会话条目永久残留，该房间报"已在录制中"且占用 `max_concurrent` 额度直至重启；仅 Starting 有 120s 兜底，Active 无超时回收 |
| D3 | `src/services/video_processor/merge.rs:129` | 音视频合并无并发闸门（传输有 gate、烧录有 Semaphore(2)、合并无限），批量重试或集中完成时同时拉起 N 个 ffmpeg |
| D4 | `src/services/download/queue.rs:551-555` | `completed_product_exists` 用裸 `starts_with(stem)`：单P stem 会命中多P产物 `{bvid}_p2.mp4`、`{bvid}_p2` 命中 `_p20`，导致**缺文件的分P被判已存在而不重下**；storage.rs:224-231 已有严格边界判定，应复用 |
| D5 | `src/services/download/queue.rs:249` + `monitor.rs:492-501,576-590` + `queue.rs:738-764` | aria2 路径重下失败后 `.downloading` 临时文件与部分下载残片不清理（native 路径会删），失败重试累积垃圾文件 |

### 数据库 / 持久化（3）

| # | 位置 | 问题 |
|---|---|---|
| DB1 | `src/services/history/records.rs:296-316` | `delete_record` 的 download_tasks 删除与 history 删除不在同一事务（对比 blogger.rs:80-102 的正确做法），任一步失败留下不一致 |
| DB2 | `src/services/download/history_sync.rs:313-356` | `cleanup_history` 只删 history 不级联终态 `download_tasks`，且全库无终态任务保留策略：自动监控长期运行下每视频 2 行永久累积，`history_limit` 的保留意图被架空，看板查询随之变慢 |
| DB3 | `history_sync.rs:334-347` + `blogger.rs:112-150` | 清理路径删除磁盘文件未走 `ensure_existing_within_root`（records.rs:273-287 有明确注释要求），旧库越界路径会真实删除下载根外的用户文件 |

### 前端（2）

| # | 位置 | 问题 |
|---|---|---|
| F1 | `web/src/main.ts:6-8` | **无任何全局错误兜底**（`app.config.errorHandler`/`onErrorCaptured`/`unhandledrejection` 全缺位）：后端字段形状突变导致渲染抛错时用户只看到无提示的白块/冻结 UI |
| F2 | `web/src/api/index.ts` 等约 20 个端点 | 响应类型为 `any`（全 src 约 250 行含 `any`、48 处 `as any`；`@ts-ignore` 为 0），socket 事件载荷全部 `data: any`（stores/app.ts:254-343），后端字段改名时 vue-tsc 无法兜底 |

### CI / 质量（9）

| # | 位置 | 问题 |
|---|---|---|
| C1 | `.github/workflows/quality.yml:39-41` | `cargo check/clippy/test` 均无 `--locked`：Cargo.toml 与 lock 漂移的 PR 能过 PR 门禁，直到 release 才失败（build.py:739-754 同步补） |
| C2 | `quality.yml:115-139` | windows-build job 无 rust-cache、无 setup-node（Node 版本取决于 runner 镜像），且 `--check` 再跑一遍完整 cargo 门禁 = 冷缓存双份全量编译，逼近 60 分钟 timeout |
| C3 | `quality.yml:71-93` | e2e-real-backend 不重建前端，测的是**仓库里提交的旧产物**；smoke 测新产物、e2e 测旧产物，存在检测盲区（与 H6 相关） |
| C4 | `build-ffmpeg.yml:143-145,106,117,130` | FFmpeg/fribidi/harfbuzz/libass tarball 下载无 SHA-256 校验 |
| C5 | `build-ffmpeg.yml:229` | `actions/upload-artifact@v4` 未 pin SHA（全仓库其余 action 均 SHA pin） |
| C6 | `pages.yml:18-23` | deploy job 是 5 个 workflow 中唯一缺 `timeout-minutes` 的 job（alpha.9 声称"所有 job 加超时上限"，此处漏网） |
| C7 | `release.yml:34-182` | portable-artifacts 与 termux-portable 无 rust-cache，thin-LTO + codegen-units=1 冷缓存下 45 分钟窗口有超时风险 |
| C8 | `build.py:522-538` | Windows portable 不打包 ffprobe.exe（unix 分支打包；`resources/ffprobe.exe` 14.7MB 已构建入库却不上任何包）：Windows 合并校验走 `ffmpeg -i` stderr 回退，时长精度秒级文本解析。确认是否有意为之 |
| C9 | `build.py:832-871` | source policy 门禁 15 项全部 advisory 化（含 CSP unsafe-inline、console.log），名为门禁实际不设防 |

### 文档 / 合规（4）

| # | 位置 | 问题 |
|---|---|---|
| DOC1 | `README.md:60` | 版本解析回退描述与实现相反：文档称"没有稳定版时回退读取含 Alpha 的清单"，三个安装器实际**显式排除 prerelease**；当前仓库全部 Release 为 alpha，一键安装会终止并要求固定版本，文档未说明 |
| DOC2 | `README.md:126`、`docs/skill.md:20` | ctl 默认放行清单漏 `pair`（实际 control.rs:440 放行 `status|help|quit|ai|pair`），README 与 skill.md 均漏 |
| DOC3 | `CHANGELOG.md` | alpha.9 条目未覆盖本次实际发版内容：自编译 FFmpeg 替换、static/index.html 删除、安装脚本路径修复等均未提及 |
| DOC4 | NOTICE.md（前端清单） | 缺 JetBrains Mono 字体（OFL 1.1，随包分发于 `web/public/css/lib/webfonts/`）归属；另全仓库无 codesign/notarization，所有二进制未签名，README 未如实说明（含 macOS Gatekeeper / Windows SmartScreen 首次运行指引缺失） |

---

## 三、低危（55 项，按域精简）

### Rust 代码质量（5）
- `dm_proto.rs:93` + `danmaku/fetch.rs:127-129`：protobuf 解码失败静默返回空且计入 `successful_segments`，上游格式变化被误报为"暂无弹幕"（`success:true`）——补 warn 日志 + 计入 failed_segments。
- `download/queue.rs:395,434`：`let _ = delete_by_id` 补偿删除失败无日志，DB 留幽灵 downloading 行。
- `live_recorder/mod.rs:683`：FFmpeg 启动失败时 `let _ = failed.update(db)` 无日志。
- API 错误信封不统一：`api/cover.rs:160-171`、`api/download/proxy.rs:57-67` 返回 `{success,message}`，全局标准是 `error.rs:12-16` 的 `{code,message,data}`（error.rs:362 测试还断言不含 success 字段）。
- fs2 同步调用混用：`live_recorder/mod.rs:411,1608`、`ffmpeg_session.rs:236`、`api/live/mod.rs:394` 在 async 上下文直接调 `available_space`（`file_safety.rs:300` 已有 spawn_blocking 先例），Windows 网络盘可能卡 runtime 线程。

### B 站 API（7）
- `wbi.rs:157`：nav 请求（WBI keys 刷新）不走重试管线，一次网络抖动即业务请求整体失败，且 nav 的 -101 不广播 auth-expired 事件。
- `client.rs:157-163`：Retry-After 只解析纯数字秒（不支持 HTTP-date）；退避无 jitter。
- `client.rs:281-283`：非 2xx 时把 api.bilibili.com 本身记入坏 CDN 熔断表（自愈性噪声，语义污染）。
- `live_monitor.rs:270-297` + `live.rs:30-34`：uid=0 的直播源永远无法探测且每 30s 报错一次（add_source 只校验 room_id）。
- `monitor/video_queue.rs:105-110,224`：自动下载路径绕过 qn 白名单（手动路径限 ≤125），settings 的 `video_quality` 无范围校验直接透传，与"126/127 下载暂不支持"的产品声明不一致。
- `danmaku/fetch.rs:163`：多 P 弹幕遍历 `take(100)` 静默截断。
- `danmu_collector/mod.rs:477-495`：错误分类靠中文错误串子串匹配（"认证失败"等），文案调整即失效。

### 下载 / 录制（7）
- `queue.rs:856` ↔ `engine.rs:262-267`：暂停原生任务注释称保留 .part 文件，实际取消即删（注释误导）。
- `monitor.rs:261-275` + `domain.rs:44`：磁盘满时 `retrying → paused` 转换非法，靠绕过状态机的裸写收敛；应纳入合法转换。
- `manager.rs:176-200`：崩溃窗口——aria2 已 complete 但 DB 未同步时重启会整段重下并 `--allow-overwrite` 覆盖成品（纯浪费带宽）；重建前应探测成品大小。
- `queue.rs:369-380`：手动任务音频失败回滚时 history 占位记录残留（看板出现永远 pending 的卡片）。
- `aria2/rpc.rs:239-251`：header 选项值未过滤 CRLF（与 native 路径 `HeaderValue` 严格校验不一致）。
- `live_recorder/mod.rs:1574-1578`：命令通道关闭分支不取消弹幕采集 CancellationToken。
- `mod.rs:1413-1432`：关停时逐房间串行 stop，多房间最长 ~35s/间，关停总时长累加。

### 数据库 / 配置（6）
- `migration/m…009_live_source_quality.rs:11-14`：`ADD COLUMN max_qn` 无 PRAGMA 存在性守卫（同族 8 个迁移都有，仅 009 漏）。
- `db/init.rs:77-87`：迁移待执行判定用 `COUNT(*)` 而非名字比对，未来迁移 squash 后会误判跳过备份。
- `history_sync.rs:322-329`：清理排序 `download_time DESC` 使 NULL（pending 占位）排末尾，超限时被优先删除（记录 ID/封面路径抖动）。
- `live_recordings`/`live_recording_segments` 无任何清理路径（merge_jobs 有 7 天+200 条双上限，唯独这两表无界）。
- `settings.rs:478-492`：跨 settings 与 protected_secrets 两表写入非原子，靠补偿回滚，补偿失败无注释说明。
- `settings.rs:529-531`：持久化 runtime_config JSON 损坏时启动直接失败无降级（对比逐键路径 warn+跳过）。

### 前端（9）
- `build.py:198`：本地打包走 `build:nocheck` 绕过类型检查（CI 正常，本地产物与 CI 类型保证不一致）。
- `stores/app.ts:133-141`：401 会话失效不重置业务 store（旧数据残留）；`stores/history.ts:204-213` 的 `reset()` 是无调用方的死代码。
- `TabAuto.vue:213-216`：博主日志 2s 轮询无 `document.hidden` 守卫（同文件 statusTimer 有）。
- `TabSettings.vue:64-82`：KeepAlive 组件用 onMounted/onUnmounted，切 tab 后定时器空转、切回不刷新 owner 数据；应改 onActivated/onDeactivated。
- `VideoDrawer.vue:420`：`window.open` 未加 `noopener`（tabnabbing）；`javascript:void(0)` 建议 `<button>` 化。
- `web/index.html:8-15`：首屏阻塞加载 100KB fontawesome.min.css + 常驻 23KB qrcode.min.js；实测首屏 JS+CSS 约 448KB 未压缩。
- `web/public/css/lib/webfonts/`：620KB TTF 字体与 woff2 并存（现代浏览器只用 woff2，纯体积负担）。
- `api/client.ts:38,104-110`：全端点固定 15s 超时，慢上游误报为自身网络故障；无按端点调参。
- `tsconfig.json:7-25`：未开 `noUncheckedIndexedAccess`；`vite.config.ts` 不在任何 tsconfig 检查范围内。

### TUI / CLI / IPC / 日志（10）
- `tracing_setup.rs:41-43`：非交互分支 fmt 层未关 ANSI（管道/systemd 下 stdout 混入转义序列）。
- `main.rs:27-34`：启动失败错误 eprintln 后再经 Termination 打一遍（stderr 双份）。
- `main.rs:47-93`：未知参数静默忽略（`--potr 8080` 拼写错误被吞）；`open`/`--open` 未写入 --help。
- `api/health.rs:32`：/api/health 硬编码 `detect_ffmpeg("auto")`，忽略 settings 的 ffmpeg.mode/custom_path，自定义机器可能误报 degraded；health/ready 无限流（公开路径，有缓存已缓解）。
- `control.rs:1274`：`sys logs` 提示文件名 `app.log.YYYY-MM-DD`，实际滚动生成 `app.YYYY-MM-DD.log`。
- `api/logs.rs:29-30,80-81`：三端日志排序口径不一致（get/blogger 反转升序、bvid 保持倒序）。
- `update.rs:191-201`：非常规格式版本号（`2.0.0-alpha`、`alpha.9.2`）误判为正式版参与比较。
- `update.rs:441-456`：Windows 替换中途失败可能残留 `bulibuli.old.exe` 与"新 exe + 部分旧 static"窗口。
- `control.rs:2546-2551`：Unix 第二实例（不同 data_dir）无条件删除抢占共享 control.sock，先启动实例的 ctl 路由到错误实例。
- `control.rs:2466-2476`：IPC 请求解析失败无错误信封（客户端空响应按成功处理退出 0）；`read_to_end` 无空闲超时。另 `audit_log.rs:5` 注释称 fire-and-forget 实际同步 await。

### CI / 测试 / 构建（8）
- `build.py:598-606`：gztar 归档不可复现（未设 SOURCE_DATE_EPOCH，同源两次构建 SHA-256 不同）。
- `build.py:756-761`：`tests/frontend_*.mjs` glob 为死路径。
- `web/scripts/real-backend.mjs:32,83-85`：e2e 临时数据目录 mkdtemp 后不清理。
- Playwright 配置无 `retries`（CI 一次网络抖动即红）。
- `build.py:23`：tomllib 隐式要求 Python ≥3.11 无版本守卫（旧 Python 报 ImportError 不友好）。
- 工具链版本双源维护（quality.yml:20、release.yml:17 的 env 与 rust-toolchain.toml 重复）；quality.yml:128-129 有永假条件死分支；macOS 无日常 CI 冒烟（仅 release 时首次编译验证）；npm/Playwright 无缓存；release.yml:241 latest.json 哈希全量读入内存。
- `.github/dependabot.yml`：cargo 节奏 monthly（安全修复一个月才开口）、limit 5、无 group/reviewers；**缺 npm ecosystem**（web/ 的 socket.io-client 等得不到漏洞推送）。
- 封面落盘纵深防御：`history_sync.rs:447-448`、`api/video/cover.rs:187-188` 未校验 bvid 直接拼路径（当前被上游 API 间接拦截，应统一 `is_valid_bvid` + `ensure_within_root`）；`api/history/file_download.rs:56-65` 的 Content-Disposition ASCII 分支未过滤引号（无注入，仅畸形头）。

### 文档 / 合规（5）
- `README.md:72`：Linux 安装器环境变量漏 `FF_PATH`。
- `docs/hero/bulibuli_hero_fully_editable_v2.html:429`：Hero 页"优先完整 portable"与实际"运行时齐全优先 core"相反。
- `PRIVACY.md:15-18`：Windows 加密结构表述不精确（实际是 DPAPI 直接加密整条目，非"AES-GCM + DPAPI 保护主密钥"两层结构，后者仅 macOS/Linux）。
- `docs/skill.md:20`：启动向导"步骤 2 选启用"应为步骤 3。
- 更新/安装失败回滚行为未文档化（update.rs 实际有备份回滚；install.ps1 中途失败无回滚未说明）。

---

## 四、通过项确认（按 18 层面）

1. **代码质量**：生产代码 unwrap/expect 纪律极好（458 处 panic 调用中生产仅 ~30 处且均有不变量保障）；anyhow/thiserror 分工清晰（132 处 .context）；无跨 await 持锁（live_recorder 有专门注释说明锁作用域）；11 处 unsafe 全部带 SAFETY 注释且无缺陷（win_job.rs、secret_store DPAPI、control.rs SDDL、process.rs PDEATHSIG）；依赖零冗余（murmur3/quick-xml/fs2/socket2 等逐一验证在用）；tokio features 精确列举；工具链 1.97.1 三方一致。
2. **安全**：配对码 `subtle::ct_eq` 恒定时间比较（8 字符≈39.5bit 熵、600s 一次性、每 IP 5 次/分 + 指数退避封禁）；session 仅 SHA-256 哈希落库、24h 轮换 + 120s 宽限；CSRF 强制 Origin + sec-fetch-site + 会话绑定 token 恒定时间比较；RBAC 核对 90+ 路由无遗漏（owner_only 前缀与路由表完全对应）；路径安全词法归一 + canonicalize 双保险、UID/bvid 严格校验、Windows verbatim 前缀剥离；SSRF 域名白名单 + DNS 私网检查（覆盖 IPv4-mapped/CGNAT/0.0.0.0/loopback/ULA）+ 逐跳重定向复验；AES-256-GCM 每次随机 12 字节 nonce、密钥 0600、明文遗留 secure_delete + VACUUM；CSP `default-src 'self'`；Action 全 SHA pin、pull_request 触发无 fork secret 泄漏面。
3. **B 站 API**：三个 reqwest Client 一次性构建共享连接池；governor 全局令牌桶 + 429 按 Retry-After 重试；-101 → `bili:auth-expired` 事件闭环、重登清缓存；WBI keys 30min TTL + 单飞；模型层 serde 全 Option/default 化 + `lenient_i64` + serde_path_to_error 定位；弹幕 WS 四重上限（wire 16MiB/解压 8MiB/子包 4096/嵌套 4 层）+ proptest 任意字节不 panic；dash/durl 回退、CDN 熔断降级、直播端点失败统一降级不拖垮监控循环。
4. **数据库**：17 个迁移严格单调、幂等守卫普遍到位（仅 009 漏）；PRAGMA（WAL/NORMAL/busy_timeout 5s/foreign_keys）经 SqliteConnectOptions 挂到每个池化连接；迁移前三文件备份 + 失败恢复 + 5 份轮转；upsert 与迁移 16 表达式唯一索引完全匹配；CHANGELOG 声称删除的死配置确认删净；AppConfig/RuntimeSettings 全字段有读取点。
5. **下载/录制**：generation 守卫 + complete_once 防抖 + 双乐观锁三层防护；aria2 GID 跨重启保留（已核实 aria2 1.16.5+ 行为）；FFmpeg 参数全 argv 无注入、stderr 常驻排水不阻塞、终止链 q→10s→kill+wait；合并产物双重校验（轨道+时长≥90%）后才删源；原生下载重定向 ≤5 + 逐跳校验 + 大小强校验；SHA-256 完成后流式计算；任务 map 均有界（merge 5s、recent_completions 60s、弹幕 100 条/50 万条/512MB）。
6. **前端**：零 v-html/innerHTML，B 站内容全代理或模板转义；轮询 generation/in-flight/visibility 三重守卫落地（"前端轮询守卫"成立）；Socket.IO 重连退避 + 断线 HTTP 兜底 + 消息按 id 去重 + 单例守卫；错误码→中文文案映射 + 风控/登录过期/磁盘满统一模态框；8 视图全部懒加载；`npm run build`（含 `vue-tsc -b --noEmit`）在 CI 强制。**事实澄清：项目未使用 vue-router（tab 由 Pinia 驱动），代码中不存在 `/_fragments/` 路由（实际是 `/settings.html` 302 回主界面，仅 CHANGELOG 提及）**。
7. **TUI/IPC**：ctl 退出码 0/1/2 语义一致、COMMAND_REGISTRY 单一真相源生成 help 与 skill.md（测试强制同步）；IPC Windows SDDL + reject_remote_clients、Unix 0600 + 父目录属主校验、8KB 消息上限；信号处理覆盖 ctrl_c/ctrl_close/SIGTERM + 10s 宽限；关停顺序正确（TUI→服务→录制→aria2→DB）；日志按日滚动 + 14 天保留 + mtime 兜底；`redact_diagnostics` 覆盖 URL 查询串/token/绝对路径；配对码不进滚动日志。
8. **安装器/打包**：build.py SHA-256 三函数统一 1MiB 分块 + sha256sum -c 兼容格式；manifest 字段完整且排除自身；release.yml 产物计数/大小/`sha256sum -c` 三重终验 + 上传 5 次退避重试 + 5xx 竞态处理；Termux 交叉编译 + QEMU 容器冒烟；FFmpeg 构建依赖 tag 固定（除 H7 的 x264）+ resources 哈希闭环校验。
9. **隐私**：全源码无遥测；外联仅 B 站域与 api.github.com（升级检查）；GeoIP 纯离线；SECURITY.md/deploy/SECURITY.md 声明逐条与代码核对相符；DB-IP CC BY 4.0 归属已在设置页落实。

---

## 五、建议行动顺序

1. **发版前必改**（约半天工作量）：H1 clippy、H3 `--since` 多字节、H4 安装器校验路径、H5 NOTICE FFmpeg、H6 重提交 static/app、DOC3 CHANGELOG 补全。
2. **发版前强烈建议**：H2 panic 策略（改回 unwind 是一行改动 + 受益最大）；S1 限流器 shrink；C1 `--locked`、C5 SHA pin、C6 pages timeout（四个一行级修复）。
3. **alpha.10 跟进**：B1-B4（API 管线）、D1-D5（下载核心）、DB1-DB3、F1-F2、S2-S3、H7/H8、其余中低危按域分批。

---

## 六、复核与修复记录（2026-08-22）

对第一至四章全部 90 项发现逐条核实并修复。验证基线：`cargo clippy --all-targets --locked -- -D warnings` 零告警、`cargo test --locked` 320 passed / 0 failed、`python build.py --check --skip-rust-gates` 全部强制门禁通过、`vue-tsc -b --noEmit` 通过、前端产物已重建。

### 高危（8/8 解决）

| 条目 | 状态 | 说明 |
|---|---|---|
| H1 clippy too_many_arguments | ✅ 已修 | `ensure_history_placeholder` 收敛为 `HistoryPlaceholder` 结构体，clippy 门禁恢复通过 |
| H2 panic=abort 兜底失效 | ✅ 已修 | release profile 改回 `panic = "unwind"`（Cargo.toml:118-120），catch_unwind 兜底链路重新生效 |
| H3 `--since` 多字节 panic | ✅ 已修 | control.rs / audit_log.rs 改按字符边界解析（strip_suffix），非法输入返回可读错误，含多字节单测 |
| H4 安装器校验已删除的 static/index.html | ✅ 已修 | linux/termux install.sh 内嵌校验统一为 `static/app/index.html` |
| H5 NOTICE FFmpeg 声明失实 | ✅ 已修 | 更正为仓库 Actions 自编译 n8.1.2 / GPL-2.0-or-later |
| H6 static/app 入库不同步 | ✅ 已修（待提交） | 前端重建完成，产物与源码一致；**新 hash 资产为未跟踪文件，发 tag 前需 git add + commit** |
| H7 x264 浮动 master | ✅ 已修 | 固定 commit `0480cb05…` 并显式 fetch |
| H8 核心模块零测试 | ✅ 大幅改善 | download 域补 completed_product_exists 边界 / history 占位 / CRLF 单测；secret_store、settings、audit_log、update、control CLI 等域均已有针对性单测（全库测试由 291 → 320） |

### 中危（27 项：24 解决 / 3 部分）

- **S1** ✅ security_server 改为有界 KeyedRateLimiter（上限 10000 key、近似 LRU 淘汰、governor 桶定期回收）
- **S2** ✅ Windows 管道客户端连接后用 GetNamedPipeServerProcessId + QueryFullProcessImageNameW 校验服务端镜像与本进程一致
- **S3** ✅ aria2 不再使用 session 文件持久化，恢复走 resume_pending_tasks 重取直链；旧含 Cookie session 已清理
- **S4** ✅ ai_skill_path 仅 Owner 可见，FFmpeg 路径对非 Owner 清空
- **B1-B4** ✅ WBI 编码统一 %20 并有双侧回归测试；配额发送时单次扣减、后台隔离；超时尊重 BILI_API_TIMEOUT；nav 刷新接入重试管线
- **D1** ✅ 烧录前预检 %TEMP%（2× 源体积）与输出目录（1×）
- **D2** ✅ worker 与 StopBackground 均走 spawn_logged_with_panic，panic 回调清理 Active 条目
- **D3** ✅ 合并加 Semaphore(2) 并发闸门（带 5 分钟获取超时）
- **D4** ✅ completed_product_exists 改严格边界匹配 product_file_matches（_p2/_p20 边界有单测）
- **D5** ✅ aria2 失败路径清理 .downloading 与 .aria2 残留
- **DB1** ✅ delete_record 任务+记录同事务
- **DB2** ✅ cleanup_history 级联删终态任务并受 history_limit 约束
- **DB3** ✅ history_sync 与 blogger 删文件前均过 ensure_existing_within_root
- **F1/F2** ✅ 全局 errorHandler + unhandledrejection + toast；约 80 个端点全量 interface 化，socket 载荷类型化
- **C1-C7** ✅ --locked、windows-build 缓存、e2e 重建前端、tarball SHA-256、upload-artifact pin、pages timeout、release rust-cache
- **C8** ◑ portable 已打包 ffprobe.exe 且包契约强制；合并时长探测优先 ffprobe（本次补充），保留 ffmpeg stderr 回退
- **C9** ✅ 门禁硬化 7 项（production console.log、inline styles、silent catch 等，当前零违规）；9 项存量违规仍 advisory 并在输出中显著汇总（清理需大改 web/src，留 alpha.10）
- **DOC1-DOC4** ✅ 回退描述、ctl 放行清单、CHANGELOG 补记、JetBrains Mono 归属、未签名说明与 Gatekeeper/SmartScreen 指引均已落地

### 低危（55 项：49 解决 / 4 部分 / 2 未做）

- 已解决：TUI/IPC/日志 10 项（含 main.rs 启动双打印、未知参数报错、health 探测修正）、Rust 质量 5 项、B 站 API 其余 7 项（Retry-After HTTP-date、jitter、坏 CDN 过滤、uid=0 降噪、qn 白名单三处口径统一）、下载录制其余项（补偿删除日志、注释矛盾、崩溃窗口收窄、占位残留清理、CRLF 过滤、并行 stop、fs2 spawn_blocking）、数据库配置 6 项（迁移 PRAGMA 守卫、名字比对判定、NULL 排序保护——本次补充、settings/secrets 同事务原子写、runtime_config 自愈降级）、前端其余项（401 重置 store、TabAuto 守卫、onActivated、noopener、差异化超时——本次补充、console.log 清扫、noUncheckedIndexedAccess 开启、首屏字体异步加载）、CI 构建低危 8 项（SOURCE_DATE_EPOCH 复现打包、死 glob 清理、临时目录清理、Playwright retries、Python≥3.11 守卫、死分支删除、npm 缓存、dependabot npm ecosystem）、文档低危全部。
- **部分解决（4）**：dm_proto 解码失败现单独计入失败分段并有 try_parse 接口（本次补充）；弹幕多 P >100 显式告警并在响应中打 pages_truncated 标记（本次补充，未实现分页拉取）；Content-Disposition 引号/反斜杠/CRLF 过滤（本次补充）；live_recordings/segments 清理仅给出建议方案未实现（涉及产品决策）。
- **未做（2）**：retrying→paused 状态机合法化已在 domain.rs 落地（本次补充）；vendor chunk 进一步拆分与 620KB 字体子集化属大改造，明确留 alpha.10。

### 本次复核新增改动文件

src/main.rs、src/domain.rs、src/services/dm_proto.rs、src/services/danmaku/fetch.rs、src/api/history/file_download.rs、src/api/video/stream.rs、src/services/video_processor/merge.rs、src/services/live_recorder/ffmpeg_session.rs、src/services/download/{queue,monitor,manager,history_sync}.rs、src/services/{blogger,settings,secret_store,audit_log,update}.rs、src/services/history/records.rs、src/db/init.rs、src/migration/m20260810_000009_live_source_quality.rs、web/src/api/client.ts、build.py、4 个 workflow、dependabot.yml、Playwright 配置 ×2、web/scripts/real-backend.mjs、README.md，以及重建的 static/app/*。

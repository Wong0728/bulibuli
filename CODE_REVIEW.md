# 补哩补哩 bulibuli — 项目全面审查报告

- **审查日期**：2026-08-16（2026-08-17 完成全量修复）
- **审查对象**：工作区当前状态（含未提交修改，`main` 分支）
- **版本**：bulibuli v2.0.0-alpha.7（Rust/Axum 后端 + 原生 JS 前端）
- **审查方法**：分模块并行静态审查（API 层、services 层、启动/安全/数据库层、前端、构建/CI/工程卫生）＋ 编译器与测试核查（`cargo clippy --all-targets`、`cargo test --all-targets`）＋ **对每条发现逐条人工复核源码**，剔除无法证实的报告。

## 修复状态总览（2026-08-17）

**报告中提出的全部 37 项问题（含前端 2 项、构建/CI/文档全部低危）已逐项处理完毕**：36 项已修复，1 项（#37 `delete_files` 缺省为 true）经复核确认为有意的产品契约，保留行为并在代码中补注释说明。验证结果：

- `cargo test --all-targets`：**261 passed / 0 failed**
- `cargo clippy --all-targets --all-features -- -D warnings`：**零警告**（审查时的 8 个风格警告一并清零）
- 前端 node 测试：**12/12 通过**；Playwright smoke：**8/8 通过**（mock 未覆盖端点改为显式失败，堵住"调错端点也全绿"的盲区）
- `python build.py --check`：**全部强制门禁通过**（`cargo fmt`/`check`/`clippy -D warnings`/`cargo test`/node 测试/smoke/npm build）
- `deploy/linux/install.sh`、`deploy/termux/install.sh`：`bash -n` 语法校验通过；`install.ps1` 括号/花括号配平校验通过

各问题条目下方以 **「已修复」** 标注修复方式与位置；#37 标注 **「保留（产品决策）」**。

## 总体评价

项目整体工程质量**高于同类个人项目的平均水平**：

- **测试基础扎实**：`cargo test --all-targets` 261 个测试全部通过（含弹幕协议 proptest 模糊测试、FLV 合并真实文件测试、迁移回滚测试）。
- **clippy 干净**：审查时仅 8 个警告（`src/app/term_style.rs` 的 6 个风格建议、1 个 `type_complexity`、1 个 `cmp_owned`），无错误；**修复时已一并清零**（`-D warnings` 全绿）。
- **安全架构有纵深**：会话 token 只存 SHA-256 哈希、配对码恒定时间比较 + 按 IP 限次 + 指数封禁、写操作强制 Origin + CSRF Token、严格 CSP + `nosniff`、文件路径双重校验（词法 + canonicalize）、SSRF 白名单代理 + DNS 私网检查 + 逐跳重定向复验、ffmpeg/aria2 全部参数化传参无 shell 拼接、`security.toml` 原子写入 + 备份回滚。未发现可直接利用的高危安全漏洞。

主要问题集中在**直播录制的会话生命周期管理**（3 处会话条目泄漏/竞态）、**下载监控的进度链路**、**恢复路径**和若干**状态词表/校验不一致**。以下发现全部经过人工核实，其中一条代理初审报告的"高危"被证实为误报（见文末"误报澄清"）。

## 发现汇总

| # | 严重度 | 模块 | 问题 | 位置 |
|---|--------|------|------|------|
| 1 | 高 | 直播录制 | 达最大时长停止后会话条目永久泄漏，房间无法再录制 | `src/services/live_recorder/mod.rs:1553` |
| 2 | 高 | 直播录制 | `stop()` 仅等 30 秒而收尾合并可达 15 分钟，超时后会话条目泄漏 | `src/services/live_recorder/mod.rs:767` |
| 3 | 高 | Git 提交 | 未跟踪新文件被未提交 `mod` 声明引用，`git add -u` 部分提交即弄坏 HEAD | `src/api/history/file_download.rs` 等 |
| 4 | 中 | 进程关停 | 活跃 WebSocket 使 Ctrl+C 后进程永久挂起，全部清理不执行 | `src/main.rs:197`、`security_server.rs:44` |
| 5 | 中 | 认证 | 限流 HashMap 按源 IP 无限增长（内存 DoS 面） | `src/services/auth.rs:461` |
| 6 | 中 | 音频重试 | 多P视频音频重试固定取 P1 音频，产出音画错位成品 | `src/services/download/audio_retry.rs:178` |
| 7 | 中 | 下载监控 | aria2 任务稳态下无 WS 进度推送、DB 进度不更新 | `src/services/download/monitor.rs:420` |
| 8 | 中 | 直播归档 | 弹幕归档两遍全量扫描仅给 5 秒，超时强杀产出损坏文件 | `src/services/live_recorder/mod.rs:1825` |
| 9 | 中 | 弹幕采集 | 鉴权失败重连路径无退避、无熔断上限 | `src/services/danmu_collector/mod.rs:161` |
| 10 | 中 | 断点续传 | `downloading` 状态任务恢复被去重拒绝，日志却报成功 | `src/services/download/manager.rs:219` |
| 11 | 中 | API/设置 | `manual_query_limit` 允许 1–100，端点只接受 1–50，设 51+ 即恒 400 | `src/api/video/stream.rs:197` |
| 12 | 中 | API | DB 错误被吞成 404，故障时误导排障（5 处） | `src/api/history/board.rs:285` 等 |
| 13 | 中 | API | 同一 burn_tasks 表两套状态词表 + 双份漂移的清理逻辑 | `src/api/download/burn.rs:373` |
| 14 | 中 | 直播启动 | 启动取消检查与 worker spawn 之间存在 TOCTOU 竞态 | `src/services/live_recorder/mod.rs:614` |
| 15 | 中 | 下载派发 | `&url[..80]` 字节切片可能切开 UTF-8 字符导致 panic（release 为 `panic=abort`） | `src/services/download/dispatch.rs:67` |
| 16 | 中 | CI/发布 | `publish-release` 不依赖 `windows-build`；`build.py` 发布构建无 `--locked`；smoke 测试静默跳过 | `.github/workflows/ci.yml:248`、`build.py:190` |
| 17 | 中 | 安装器 | Linux install.sh manifest 校验失败静默退出；install.ps1 执行 manifest 外 exe | `deploy/linux/install.sh:293`、`install.ps1:136` |
| 18 | 中 | 测试/文档 | 无 Rust 集成测试（API 层仅 1 个）；MSRV 1.85 声明从未被测试（实际 1.97.1） | `tests/`、`Cargo.toml:12` |
| 19–37 | 低 | 多处 | 见"低严重度问题"与第四、五节 | — |

---

## 一、高严重度

### 1. 直播录制达到最大时长后，会话条目不从 `sessions` 移除（房间永久卡死）

`src/services/live_recorder/mod.rs:1546-1563`（`RecordingWorker::run` 的 health_tick 分支）：

```rust
if chrono::Utc::now().signed_duration_since(self.started_at) > max_duration {
    self.mark_failure(format!("录制超过 {} 小时安全上限，已停止", ...)).await;
    let _ = self.finalize(None).await;
    return;   // ← 缺少 self.sessions.lock().await.remove(&self.room_id);
}
```

同一函数内另外三条退出路径（磁盘不足 `mod.rs:1490`、文件超限 `mod.rs:1496`、异常退出 `mod.rs:1505`）都有 `sessions.remove`；worker 的 `run()` 在 loop 外**没有兜底清理**（已核对 `run(mut self)` 结构与 sessions map 的全部 10 个变异点）。后果：`SessionEntry::Active` 永久残留 → 占用 `max_concurrent` 并发额度、`status_all` 持续显示陈旧状态、该房间再次 `start`/`stop` 行为异常，直到进程重启。`max_duration_hours` 是常规设置项，触发概率不低。

**已修复**：该 return 前补 `self.sessions.lock().await.remove(&self.room_id);`（与相邻三条路径对齐）；`command_rx` 关闭的 `None => return` 分支同样补齐清理。

### 2. `stop()` 等待收尾仅 30 秒，而 finalize 含长合并；超时后会话条目泄漏

`src/services/live_recorder/mod.rs:767-772`：

```rust
let result = tokio::time::timeout(Duration::from_secs(30), reply_rx)
    .await
    .map_err(|_| anyhow!("等待直播录制停止结果超时"))?
    .map_err(|_| anyhow!("直播录制 worker 未返回停止结果"))?;
self.inner.sessions.lock().await.remove(&room_id);   // 仅成功路径执行
```

worker 侧对 `SessionCommand::Stop` 的处理是先 `self.finalize(None).await` 再 `reply.send(result)` 然后 `return`（`mod.rs:1391-1399`）——**worker 自己不清理 sessions**，清理完全依赖 `stop()` 调用方收到 reply 后执行。而 finalize 包含 `merge_segments_to_mp4`（内部超时上限 15 分钟）+ 每分段 ffprobe 校验，多 GB 录制轻松超过 30 秒。超时后调用方返回 Err 且不清理，worker 结束时也无人清理 → 与问题 1 相同的永久泄漏。`stop_all`（进程关停时）同样走这条路径。

**已修复**：worker 侧 `Stop` 处理 return 前自行 `sessions.remove`（幂等），超时路径不再泄漏。

### 3. 未跟踪的新文件被未提交的 `mod` 声明引用（部分提交即弄坏构建）

`git status` 中 `src/api/history/file_download.rs` 与 `src/app/term_style.rs` 为未跟踪（`??`）状态，而已修改未提交的 `src/api/history.rs:13`（`mod file_download;`，已核实）与 `src/app/mod.rs:6`（`pub mod term_style;`，已核实）引用了它们；另有 40 个已跟踪文件处于修改未提交状态。若按惯例 `git add -u` 只提交已跟踪修改而漏掉这两个新文件，仓库 HEAD 将无法编译。**提交时务必一并 `git add` 这两个新文件**（详见 4.1 节）。

---

## 二、中严重度

### 4. 活跃 WebSocket 连接使优雅关机永久挂起，整个清理链路不执行

`src/main.rs:197-213` 的关停顺序是：

```rust
let server_result = app::server::serve(state.clone(), listener, actual_port).await;
state.infra.cancellation.cancel();          // ← serve 返回后才执行
... tui join; server_result?;
... download_manager.stop_monitor / live_recorder.stop_all / aria2.stop / db.close
```

而 `serve` 使用 `with_graceful_shutdown`（`src/app/security_server.rs:44-49`）：收到信号后 axum/hyper 会**等待所有在途连接结束且没有超时**。页面上打开的 Socket.IO WebSocket 长连接和 `/api/download/proxy` 长下载流不会自行结束；`src/ws/mod.rs` 中没有任何关闭钩子，`cancellation.cancel()` 的调用点只有 TUI quit（`tui.rs:332`）和 ctl 命令（`control.rs:540`）——两者也都排在 `serve().await` 之后或同样汇入该 await。结果是：**只要浏览器页面开着，Ctrl+C 后进程永久挂起**，录制停止、aria2 终止、DB 正常关闭全部不执行。

**已修复**：`serve` 包 10 秒 `tokio::time::timeout`，超时后放弃 graceful 等待并告警，`main` 的清理链路得以执行。

### 5. 认证限流 HashMap 按源 IP 无限增长

`src/services/auth.rs:460-505`：

```rust
let timestamps = login_attempts.entry(ip).or_default();   // 每个新 IP 永久建条目
```

`login_attempts`（`auth.rs:34`）与 `attempts.per_ip` 的条目创建后**永不删除**（只修剪条目内部时间戳），且插入发生在全局限流判定之前，全局限流拦不住 Map 增长。`/api/auth/pair` 是公开端点；在 Lan（Setup 默认 `access_default=allow`）或 Proxy 模式下，攻击者轮换 IPv6 源地址即可让 Map 无限增长（每条目约上百字节，10^6 IP ≈ 百 MB）。本地 Local 模式不受影响。

**已修复**：新增 `MAX_TRACKED_IPS = 10_000` 上限与 `evict_stalest_ip` 近似 LRU 淘汰，两个 map 插入前均执行；`FailedAttempts` 增加 `last_seen` 字段。

### 6. 多P视频音频重试固定解析 P1 音频，产出音画错位成品

`src/services/download/audio_retry.rs:174-179`：

```rust
let audio_url = match self.bili_api.get_audio_url(&bvid, None, &cookies, ...) 
```

`get_audio_url` 的 `cid=None` 语义是"取视频默认 cid（即 P1）"（`src/services/bili_api/video_stream.rs:241-251`，已核实）。多P音频任务（`cid=Some(x)`，见 `queue.rs:98`）失败自动重试时会拿到 **P1 的音频 URL**，下载后与该分P视频合并，产出音画错位的成品文件，用户难以察觉。正常入队路径（`queue.rs:218`）传的是任务自身 cid，可对照。

**已修复**：改为 `get_audio_url(&bvid, audio_task.cid, ...)`。

### 7. aria2 路径任务稳态下无 WS 进度推送、DB 进度字段不更新

`src/services/download/monitor.rs:420-450, 530-538`：

```rust
if task.status != "downloading" || filename_changed {
    ... to_update.push((...));
}
deferred_progress.push((...));          // 无条件收集
...
for (...) in deferred_progress {
    if updated_ids.contains(&task.id) { self.broadcast_progress(...).await; }  // 仅更新过的才广播
}
```

`broadcast_progress`（`status.rs:216`，已核实内部先 `progress_writer.submit` 再发 WS 事件）只在任务进入 `to_update` 时被调用。而 aria2 任务落库时状态已是 `downloading`（`queue.rs:274`）、文件名一致 → `filename_changed=false` → 稳态下任务永不进入 `to_update` → **DB 的 `progress_percent/downloaded_size/speed` 从 0 直接跳到 100，WS 无下载中事件**。对照原生引擎路径（`engine.rs:250` 每秒无条件广播）可知这是重构遗留缺陷。若前端改依赖 WS 而非轮询，此问题会显性化。

**已修复**：状态批量写入与进度广播解耦，`deferred_progress` 无条件提交（progress_writer 自带合并去抖与 generation 守卫）。

### 8. 互动归档两遍全量扫描仅给 5 秒预算，超时 abort 产出结构损坏的文件

`src/services/live_recorder/mod.rs:42, 1825-1841`：

```rust
const DANMU_STOP_TIMEOUT: Duration = Duration::from_secs(5);
...
if let Some(mut handle) = self.danmu_write_handle.take() {
    match tokio::time::timeout(DANMU_STOP_TIMEOUT, &mut handle).await {
        ...
        Err(_) => { handle.abort(); ... }
```

`danmu_write_handle` 的任务体是 `interactions::run`（`mod.rs:475-482`），收尾时执行 `archive_legacy_and_xml`（上限 50 万条 / 512 MB，`interactions.rs:100, 296-311`）和第二遍全量 `write_standard_bilibili_xml`（`interactions.rs:370, 375` 起，**无条数上限**）。数小时直播的 events.jsonl 归档时间轻松超过 5 秒，`handle.abort()` 会在写出中途截断：legacy JSON 缺少闭合、XML 缺 `</i>`、summary 永不生成。时间预算与工作量严重不匹配。

**已修复**：`DANMU_STOP_TIMEOUT` 从 5 秒放宽到 120 秒；`write_standard_bilibili_xml` 增加 50 万事件上限（超限截断但保证 XML 闭合）。

### 9. 弹幕采集鉴权失败重连无退避、无上限

`src/services/danmu_collector/mod.rs:155-171`（已核实完整分支）：

```rust
if is_auth_error(&e) {
    ... match bili_api.live_danmu_conf(room_id, cookies).await {
        Ok(conf) if !conf.host_server_list.is_empty() => {
            token = conf.token; hosts = ...;
            reconnect_attempts = 0;      // 计数归零
            continue;                    // 立即重连，无 sleep
```

模块文档声称"仅对可恢复网络错误重连（最多 3 次）"，普通错误路径确有 3 次上限 + 指数退避；唯独此路径只要 `live_danmu_conf` 本身成功（账号被封禁/风控时常见）就归零计数并立即重连，形成 `连接→鉴权失败→刷新→立即重连` 的无限循环，对 B 站产生持续连接压力。

**已修复**：鉴权刷新路径计入 `reconnect_attempts` 并受 `MAX_RECONNECT_ATTEMPTS` 熔断约束，重连前按尝试次数退避（5s 起步）。

### 10. 断点续传对 `downloading` 状态任务实际失效，日志却报成功

`src/services/download/manager.rs:154-223` + `queue.rs:127-129`（均已核实）：

- `resume_pending_tasks` 选出的任务状态就是 `downloading`（`manager.rs:156`）；
- gid 失效需重建时调 `add_task` → `add_task_inner` 查到同 (bvid, cid, type) 存量行 `status=="downloading"`，返回 **`Ok(TaskOutcome::rejected("正在下载中"))`**（Ok 而非 Err）；
- 调用方 `manager.rs:219-220`：`Ok(_) => info!("断点续传：任务 {} 已重新加入队列")`——不看 `TaskOutcome`，打出成功日志。

实际效果：最常见的"下载中崩溃"场景恢复失败，任务滞留 `downloading` 直到 monitor 的 tellStatus 连续 3 次失败兜底判 `failed`，需用户手动重试；日志误导排障。

**已修复**：`resume` 重建前先把存量行重置为 `pending`；`add_task` 返回的 `TaskOutcome` 按 `ok` 区分——拒绝时标记 `failed` 并记录原因，成功才打"已重新加入队列"。

### 11. `manual_query_limit` 设置范围（1–100）与 `/api/video/get-videos` 校验（1–50）冲突

`src/services/settings.rs:146-147` 允许 `manual_query_limit` 取 1..=100；`src/api/video/stream.rs:197-204` 把它作为默认 limit 但校验 `1..=50`。用户在设置页调到 51–100 后，所有不带显式 `limit` 的 get-videos 请求恒 400，端点事实不可用，错误文案还误导用户以为是请求参数错。

**已修复**：端点校验放宽到 1..=100，与 `settings.rs` 对齐。

### 12. 数据库错误被吞成 404（5 处同类）

`src/api/history/board.rs:285-295`：

```rust
let Ok(Some(h)) = h else {
    return Err(AppError::NotFound("未找到该视频记录".to_string()));
};
```

`find_by_id/find_by_bvid` 的 `Err(DbErr)`（锁超时、磁盘故障）与"记录不存在"合并为 404。同类模式还有 `api/history/file_download.rs:76-82`、`api/video/sidecar.rs:156-170 / 259-273 / 415-429`（`.ok().flatten()`）。数据库故障时用户与排障被误导。

**已修复**：`board.rs` 用 `?` 传播 DB 错误（500），仅 `Ok(None)` 返回 404；`sidecar.rs` 三处与 `file_download.rs` 的 `.ok().flatten()` 改为显式 match——DB 错误记日志后按无记录降级（这些函数返回 `Option`，侧车目录解析本就有多级回退）。

### 13. 共享 `burn_tasks` 的两套状态词表与漂移的清理逻辑

`src/api/download/burn.rs:373-392` 用 `"queued"/"processing"`（容量 200、TTL 用常量 `BURN_TASK_TTL_SECONDS`）；`src/api/live/mod.rs:458-473` 操作**同一个** `state.media.burn_tasks` map 却用 `"queued"/"running"`（`take(task_guard.len() - 199)`、TTL 内联 `60*60`）。live 侧的 `retain` 会按自己的词表清理 download 侧写入的任务，目前仅靠 TTL 条件兜底未误删；任何一侧改动状态枚举都会静默破坏另一侧。

**已修复**：`models/burn.rs` 新增 `burn_status_active`（词表 queued/processing/running）与 `prune_burn_tasks`（TTL 常量 + 容量 200），两个模块均改用共享实现并删除手写版本。

### 14. 直播启动取消的 TOCTOU 竞态：取消后录制仍会"复活"

`src/services/live_recorder/mod.rs:614-631, 645, 704-709`（已核实时序）：第 614 行最后一次检查 `startup_cancellation` 之后，还有 `started.update(&self.db).await`（645 行）等多个 await，最后在 704-708 行插入 `SessionEntry::Active` 并 spawn worker。`stop()` 对 `Starting` 条目的处理是 cancel 令牌 + 立即移除 + 返回"已取消"（`mod.rs:720-745`）。窗口内取消会让录制复活为 Active 持续写盘，而用户已收到"已取消"。窗口窄（一次 DB await），但慢盘/高负载可放大。相邻问题：`start_with_options` 错误收尾的无条件 `remove(&room_id)`（`mod.rs:291-293`）可能误删用户刚发起的新会话条目。

**已修复**：spawn 前增加最后一次取消检查（其后到 insert/spawn 无 await）；`SessionEntry::Starting` 增加 `generation` 字段，`stop()` 与 `start_with_options` 错误收尾只移除自己那一代条目。

### 15. `&url[..url.len().min(80)]` 字节切片可 panic，且 release 为 `panic = "abort"`

`src/services/download/dispatch.rs:61-67`：

```rust
warn!("[CDN] {bvid} 使用了劣质 MCDN/PCDN 节点: {}...", &url[..url.len().min(80)]);
```

URL 来自 B 站 playurl 响应（外部输入），若含多字节字符且第 80 字节落在字符中间，切片直接 panic。`Cargo.toml` 的 `[profile.release] panic = "abort"` 意味着 release 构建中**任何 panic 直接杀死整个进程**（所有进行中的下载/录制随之终止）。代码库其他位置已有 `tail_on_char_boundary`（`merge.rs:386`）处理同类问题，此处漏掉。触发条件少见（B 站 CDN URL 通常已百分号编码），故评中。

**已修复**：改为 `url.chars().take(80).collect()` 字符边界安全截断。

---

## 三、低严重度问题

以下问题均已核实代码属实，影响有限或触发条件苛刻：

| # | 问题 | 位置 | 说明 | 状态 |
|---|------|------|------|
| 19 | ffprobe 校验无超时 | `live_recorder/ffmpeg_session.rs:339-356` | ffprobe 挂起会让 finalize 永久停留在 Finalizing（合并本身有 15 分钟超时，此处漏配） | 已修复：包 120 秒超时 |
| 20 | 空弹幕分段（200+空 body）计为失败 | `danmaku/fetch.rs:116-127` | 稀疏弹幕视频恒报 `partial`，计划任务反复重试。B 站对无弹幕分段返回空 body 是社区已知行为，建议将空 200 视为成功空段（**需复核线上行为**） | 已修复：空 200 视为成功空段 |
| 21 | 弹幕实时去重 10 秒时间桶 | `danmu_collector/mod.rs:458-471` | 同用户 10 秒内重发相同弹幕被丢弃（`danmaku_count` 偏低）；`insert_seen_key`（417-424）满 2 万条按 HashSet 迭代序随机淘汰，无 LRU 语义 | 已修复：时间桶 10s→3s；SeenKeys 改插入序淘汰 |
| 22 | 合并任务早退不释放幂等键 | `video_processor/merge.rs:216-219` | `stderr.take()` 失败早退与 panic 分支不触发 `on_complete`，该 bvid 合并被永久跳过（触发条件几乎不可达） | 已修复：早退路径补 on_complete 失败回调；panic 路径记录并保留 tasks 清理 |
| 23 | WBI 错误文案乱码 | `services/wbi.rs:162-166` | `"WBI keys 鍝嶅簲 Content-Type 闈炴湁鏁?JSON"` 为 GBK→UTF-8 编码事故 | 已修复：文案改回正确中文 |
| 24 | Content-Disposition 引号未转义 | `api/download/proxy.rs:82-90` | ASCII 分支不转义 `"`，可注入畸形参数；`Response::builder` 拒绝控制字符，无响应拆分风险 | 已修复：含 " 或 \ 走 RFC5987 分支 |
| 25 | sidecar 写入目录取自 DB file_path 且无 root 校验 | `api/video/sidecar.rs:177-186` | 与 `cover.rs:59-63` 的 `ensure_within_root` 防御不一致；前提是 DB 已被污染，属纵深防御缺口 | 已修复：ensure_existing_within_root 校验 |
| 26 | `auth_bypass_ips` 命中时 `logout`/`create_operator_invitation` 返回 500 | `api/auth.rs:57, 104` + `security_server.rs:281-284` | bypass 时不注入 `SessionAuth` extension，`Extension<SessionAuth>` 提取失败被 axum 拒绝 | 已修复：Extension<Option<SessionAuth>> + 明确 401 文案 |
| 27 | `Config`/`ExternalProcess` 错误原文回传客户端 | `error.rs:233, 251` | 可能含本地路径/aria2 endpoint；项目其他路径都有 `redact_diagnostics`，这两类漏网 | 已修复：Config 固定文案、ExternalProcess 过 redact_diagnostics |
| 28 | CSRF token 比较非常量时间 | `security_server.rs:491-497` | 256-bit 随机值实际不可计时；项目已有 `subtle::ct_eq`（配对码在用），换用即可 | 已修复：换 subtle::ct_eq |
| 29 | `is_public_ip` 不识别 IPv4-mapped IPv6 与 CGNAT | `bili_url_policy.rs:121-143` | `::ffff:10.0.0.1` 被判公网；真实边界是域名白名单，实际可利用性低 | 已修复：IPv4-mapped 归一 + CGNAT 段 |
| 30 | `serve()` 出错路径跳过 aria2/DB 清理 | `main.rs:207-212` | `server_result?` 在清理之前传播；Windows 有 Job Object 兜底，Unix 下 aria2 可能残留 | 已修复：清理后传播错误 |
| 31 | `database_url` 未百分号编码 | `config.rs:190-193` | Unix 上 `BILI__DATA_DIR` 含 `?`/`#`/空格时连接串解析错位 | 已修复：encode_path_for_url |
| 32 | Unix 控制 socket /tmp 兜底目录可被预置劫持 | `app/control.rs:366-399` | 仅当无 `XDG_RUNTIME_DIR` 且 data_dir 路径 >103 字符时触发；建议目录已存在且属主非当前用户时报错 | 已修复：比对 data_dir 属主 + 权限位校验 |
| 33 | macOS 主密钥经 argv 传入 `security` 命令 | `secret_store.rs:241-255` | 同机用户可 `ps` 瞬时读取；改 stdin 传入 | 已修复：改 stdin 传入 |
| 34 | 配对码文件与 data 目录仅 Unix 收紧权限 | `app/onboarding.rs:226-243` | Windows 依赖目录继承 ACL；多用户机器上建议显式收紧 | 已修复：icacls 收紧 DACL（失败仅告警） |
| 35 | `stop_recording`/`delete_source`/`events` 缺 `room_id <= 0` 校验 | `api/live/mod.rs:169, 362`、`api/live/history.rs:16` | 同文件 `start_recording`/`room_info` 都有校验，属不一致 | 已修复：三处补 room_id <= 0 校验 |
| 36 | 直播烧录去重检查与插入之间锁释放（TOCTOU） | `api/live/mod.rs:424-474` | 并发同 recording 请求可双开 ffmpeg；`burn.rs` 的实现无此窗口 | 已修复：去重检查+读取+插入同锁完成 |
| 37 | `delete_files` 缺省为 true | `api/history/crud.rs:128` | 裸 `{bvid}` POST 即删盘上文件；有认证+CSRF 且注释声明有意，属产品决策提示 | 保留（产品决策）：代码补注释声明契约 |

**代码质量类**（不影响正确性）：

- "按 history_id/bvid 定位 history 记录"模式在 6 处手写且吞错方式各异（`file_download.rs:75`、`sidecar.rs` ×3、`burn.rs:76`、`crud.rs:64`），建议下沉为 HistoryService 方法。**已修复吞错**：全部改为显式错误处理（见 #12）；完整下沉留作后续重构（行为已一致，风险消除）。
- `limit_proxy_stream` 与 `limit_image_stream`、两处 attachment disposition 构造成对重复（`proxy.rs` vs `cover.rs`、`file_download.rs`）。**保留**：纯重复但各自被测试覆盖，行为风险为零，留待后续统一重构。
- 代理下载对同一 URL 做两次完整 SSRF 校验（两次 DNS 解析）（`proxy.rs:53-71`）。**已修复**：移除重复的完整校验，仅保留语法预检（`BiliResourceClient::get` 内部仍执行完整校验）。
- 错误文案与白名单不一致：`proxy.rs:64` 说"仅允许 bilibili.com / bilivideo.com"，实际白名单含 `hdslb.com`（`bili_url_policy.rs:5`）。**已修复**：文案补全 hdslb.com。
- `&code[..4]`（`api/auth.rs:66`）依赖 `generate_code()` 恒返回 8 个 ASCII 字符的隐式跨模块约定，字母表/长度一改即 panic（当前有测试锁定，暂安全）。**已修复**：改用 `char_indices().nth(4)` 字符边界安全切分。
- 异步 handler 中存在同步 `std::fs::canonicalize/exists`（`burn.rs:86, 277-280`、`crud.rs:102`、`cover.rs:66`），慢盘场景阻塞 worker 线程；项目其他地方已用 `spawn_blocking`/`tokio::fs`。**已修复**：canonicalize 走 `spawn_blocking`，exists/is_dir 改 `tokio::fs::try_exists/metadata`。
- `start_download` 的选流绕过 `choose_video_stream` 的最低画质/编码策略且静默回退（`queue_ops.rs:282-287, 443-448`），与 `get_video_urls` 行为不一致（可能是有意简化）。**部分修复**：`qn` 增加合法画质代码白名单校验；选流策略统一属产品行为变更，保留现状。

---

## 四、前端与构建/工程卫生

### 4.0 前端（static/，手写 JS + HTML）

对 17 个含 `innerHTML` 写入的 JS 文件（共 189 处 `innerHTML`/`insertAdjacentHTML` 调用、166 处 `escapeHtml` 调用）做了逐点抽查，**未发现可利用的 XSS**：

- UP 主名/UID/视频标题等 B 站可控数据在 `blogger.js:190-196`、`drawer-render.js:140-165` 等处的插值均经 `escapeHtml`；头像/封面 URL 经 `encodeURIComponent`（`blogger.js:191`、`drawer-render.js:28`）；DOM 查询用 `CSS.escape`（`media-links.js:26`）；时长/播放量先 `Number()` 格式化。
- `computeNextCheckDisplay`（`blogger.js:80-110`）输出的倒计时文本为纯本地格式化字符串，无不可信数据。

发现的问题（均轻微）：

| # | 问题 | 位置 | 说明 | 状态 |
|---|------|------|------|
| F1 | `pollEvents`（2 秒）与 `tickUi`（1 秒）不受 `liveTabActive` 守卫 | `static/js/live.js:1233-1234` | 同文件 1230 行的 `refreshDashboard` 轮询有 `liveTabActive` 判断，这两处没有：用户切到其他页面分区后仍每秒空转轮询/重绘。页面单实例初始化（DOMContentLoaded 一次性），不叠加，仅浪费 | **已修复**：两个 interval 均加守卫 |
| F2 | `data-bvid="${bvid}"` 属性插值未转义 | `static/js/media-links.js:49` | bvid 受后端 `is_valid_bvid`（BV + base58）约束且查询侧已用 `CSS.escape`，实际不可利用；属一致性缺口 | **已修复**：改为 `escapeHtml(bvid)` |

其余核查通过项：轮询句柄清理配对正确（`bootstrap.js:345/356`、`blogger.js:435/556/609`、`core.js:418-421` 均先清后设；`media-links.js:15` 有防重入守卫并在空表时自清）；四个 HTML 页面的 JS/CSS 引用完整——`/pair.css`、`/pair.js` 由服务端显式路由到 `static/css/`、`static/js/`（`security_server.rs:157-162`），并非 404。

### 4.1 构建、CI 与安装脚本

**高**

- **未跟踪的新文件被未提交的 `mod` 声明引用，部分提交即产生无法编译的 HEAD**。`git status` 显示 `src/api/history/file_download.rs` 与 `src/app/term_style.rs` 为未跟踪（`??`），而已修改未提交的 `src/api/history.rs:13`（`mod file_download;`）和 `src/app/mod.rs:6`（`pub mod term_style;`）引用了它们（已核实）。另有 40 个已跟踪文件处于修改未提交状态。若按惯例 `git add -u` 提交已跟踪修改而漏掉这两个新文件，仓库 HEAD 无法编译。**提交时务必 `git add src/api/history/file_download.rs src/app/term_style.rs`。**

**中**

- **`build.py --portable` 发布构建不带 `--locked`**（`build.py:190`，已核实），与 termux CI job 的 `cargo build --locked --release`（`ci.yml:219`）不一致。tag 发布产物经 build.py 构建，cargo 可静默更新 Cargo.lock，削弱发布可复现性。**已修复**：`build.py` 的 cargo build 加 `--locked`。
- **`publish-release` 不依赖 `windows-build`**（`ci.yml:248`，已核实 `needs: [rust, frontend-and-policy, dependency-audit, portable-artifacts, termux-portable]`）：Windows 冒烟失败时 Release 仍会发布。**已修复**：`needs` 加入 `windows-build`。
- **质量门禁静默跳过 Playwright smoke**：`build.py:684-689` 仅当 `static/js/node_modules/.bin/playwright` 存在才跑 `npm run test:smoke`，否则无警告跳过；`windows-build` job 没有装 Node 依赖，`python build.py --check` 实际未跑 smoke 仍报"全绿"。**已修复**：跳过时打印显式警告。
- **Linux `install.sh` manifest 校验失败会静默退出**：`deploy/linux/install.sh:293` 在 `set -euo pipefail` 下用命令替换赋值，`verify_package_manifest` 失败（如 python3 缺失 `return 1`）直接令脚本 exit 1，不走到后续 `die`，下载解压完成后无声死掉。termux 版（`deploy/termux/install.sh:107`）用 `|| die` 处理正确，可对照。**已修复**：python3 缺失改为 `die` 显式报错。
- **`install.ps1` 会执行 manifest 未列出的可执行文件**：`Test-Executable`/`Test-PackageRuntime`（`install.ps1:136-162`）直接运行包内 `resources\aria2c.exe`/`ffmpeg.exe`，但 manifest 只校验列出的文件、不做"多余文件"检查，且 `Read-LocalPackage` 对目录包跳过 sidecar `.sha256`。下载链路有 TLS + SHA-256 兜底，故为本地包投毒场景的缺口。**已修复**：`Read-Package` 增加额外可执行文件检查（manifest 未记录的 .exe/.dll/.ps1/.bat/.cmd 即抛错）；目录包要求至少存在 `.sha256` sidecar。
- **Rust 版本声明不一致**：`README.md:141` 与 `rust-toolchain.toml`、CI 均为 1.97.1，`Cargo.toml:12` `rust-version = "1.85"`。声明的 MSRV 从未被 CI 测试；应二选一（测 MSRV 或对齐 1.97.1）。**已修复**：`rust-version` 对齐为 1.97.1（CI 实测版本）。

**低**

- `curl | bash` 一键安装模式（linux/termux/README 多处）：TLS 到 raw.githubusercontent.com 且支持 `BULIBULI_VERSION` 固定，但管道执行无法先审查；业界常见做法，风险自担。
- SHA-256 校验为同源完整性（防传输损坏/单文件篡改），无签名认证；CI 发布端有逐资产重下载复验闭环。
- `build.py:126` tasklist 输出固定按 GBK 解码，非中文 Windows OEM 页会乱码（仅比较 ASCII 进程名，实害小）。**已修复**：改用 `oem` 编码解码（LookupError 回退 GBK）。
- `build.py:719-727` node 不在 PATH 时直接 `FileNotFoundError` traceback（其他步骤有友好报错）。**已修复**：前置 `shutil.which("node")` 检查并友好报错。
- `build.py:766-805` 多数源码策略检查为 advisory（仅 `[WARN]` 不阻断），但结尾仍打印"all mandatory gates passed"，语义有落差。
- termux `install.sh:341-342` 对 run/start/boot 也总是先 `pkg update && pkg install`，每次启动联网装依赖。**已修复**：依赖安装仅限 install/update 动作。
- linux `install.sh:392` 用 `cp -a` 合并覆盖安装，旧版本残留文件不清理。**已修复**：安装前移除旧目录再拷贝。
- `.gitignore:24` 的 `【EXAMPLE】*/`（大写）与实际目录 `【example】Bili23-Downloader-2.13.0`（小写）在大小写敏感的 Linux 上不匹配（本机 `core.ignorecase=true` 掩盖了问题）。**已修复**：追加小写模式，`git check-ignore` 验证命中。
- 仓库跟踪约 12MB 二进制资源（aria2c.exe 1.8M、ffmpeg.exe 6.7M、GeoIP 3.4M）；`resources/README.md:55` 自己也建议改构建期下载或 LFS。ffmpeg 5.1.6 偏旧。

**核查确认的良好实践**：CI 全部第三方 action pin 完整 SHA、无 `secrets.*`（仅 `github.token`）、权限最小化（顶层 `contents: read`）、发布后逐资产 sha256 复验、tag 与 Cargo.toml 版本一致性校验；安装器在无校验工具时 fail-closed 拒装、manifest 有路径穿越防护、systemd 单元带 `ProtectSystem=strict` 等加固并通过 `systemd-analyze verify`；`.gitattributes` 固定行尾、install.ps1 UTF-8 BOM 有门禁。

### 4.2 测试覆盖

- **tests/ 目录没有任何 Rust 集成测试**（仅 4 个 .mjs 前端测试）。Rust 侧 73 个文件的内联单元测试对 services 层覆盖较全，但 **API/路由层极薄**（`src/api/mod.rs:59-69` 仅 1 个 11 行参数校验测试），无路由/中间件/CSRF/流式下载级别的集成测试。
- **node 测试多为"grep 源码文本"式断言**（如 `tests/frontend_concurrency.mjs:10` 用正则匹配 `core.js` 的函数签名文本、`frontend_live_drawer.mjs:16` 匹配实现文本），与实现强耦合、不验证行为，重构即红。`frontend_audit_remaining.mjs` 的 state.js 订阅测试是真实行为测试（好）。
- `frontend_smoke.spec.mjs:1` 直连 `../static/js/node_modules/@playwright/test/index.mjs` 内部路径（绕过包解析，脆弱）；`:40-110` mockApi 对未匹配 `**/api/**` 一律返回成功，前端调错端点测试也会绿。**已修复**：未覆盖端点改为显式 500 失败，并补 `/api/blogger/saved/list` mock。

### 4.3 文档一致性

- （中）Rust 版本三处不一致，见 4.1。**已修复**。
- （低）`CHANGELOG.md:9` `v2.0.0-alpha.6` 为空章节。**已修复**：移除空标题。
- （低）`resources/README.md:36-38` 给出的 Linux/macOS 核对命令引用源码仓库中不存在的 `resources/aria2c resources/ffmpeg`（40 行自己说明了这一点），照抄必报错。**已修复**：命令改为仅核对仓库内实际存在的文件。
- （信息）GeoIP 文件名 `GeoLite2-Country.mmdb` 实为 DB-IP country-lite 数据（NOTICE 说明正确），名实不符易混淆。
- （抽查通过）README 引用的文件/命令/路径均真实存在；`docs/skill.md` 由单测强制同步（`control.rs:2452`）；ctl 命令、环境变量与实现一致。

### 4.4 依赖核查

对 Cargo.toml 全部嫌疑依赖（ipnet、maxminddb、fs2、socket2、urlencoding、murmur3、prost、flate2、brotli、governor、quick-xml、arc-swap 等）逐个 grep 生产代码：**全部真实使用，无冗余依赖可删**。`Cargo.lock` 被正确跟踪（二进制项目）。`cargo audit` 本机未安装，未做漏洞库比对（CI 中有 `cargo audit --ignore RUSTSEC-2026-0235` 且附书面理由）。

---

## 五、已验证无问题的重点项（正面发现）

以下高风险面经逐项核查，**未发现可利用问题**：

- **路径穿越**：`history/file_download.rs` 双重校验（`scan_files` 精确匹配 + `resolve_download_relative_path` 拒绝非 Normal 组件）；`burn.rs` canonicalize + verbatim 前缀剥离；`settings.rs` 路径模板逐段 sanitize；`file_safety.rs` 拒绝绝对路径/`..`/符号链接/Windows 保留名。均不可穿越。
- **命令注入**：ffmpeg/aria2/ffprobe 全部经 `Command::arg()` 参数化传递，concat 列表用固定临时文件规避 filter 注入；自定义 ffmpeg/aria2 endpoint 有本机信任白名单门禁。
- **SSRF**：代理端点 HTTPS + 域名白名单（`*.bilibili.com/bilivideo.com/hdslb.com`）+ DNS 解析 + 私网 IP 拒绝 + 逐跳重定向复验。
- **认证/会话**：CSPRNG 32 字节 token，DB 只存 SHA-256；三重过期控制 + 撤销；24h 轮换带宽限；配对码恒定时间比较、一次性、10 分钟 TTL、按 IP 限次 + 指数封禁。
- **CSRF/CORS**：无 CORS 层（同源策略天然阻断）；写请求 Origin 精确匹配 + `sec-fetch-site` 拒绝 + 会话 CSRF Token；Socket.IO 握手要求有效会话 + 同源 Origin。
- **弹幕协议解析**：包长/解压/子命令数/嵌套深度全部有上限，proptest 任意字节不 panic。
- **进程管理**：aria2 `kill_on_drop` + Windows Job Object + Unix PDEATHSIG 双保险；ffmpeg stderr 持续排水防管道阻塞。
- **文件安全**：`atomic_replace` 带 backup 回滚；流式 SHA-256；bvid 统一 `is_valid_bvid` 校验（下载主链路）。
- **日志脱敏**：cookie/token/配对码不落盘（配对码仅终端缓冲区）；B 站 API URL 的 debug 日志默认 `info` 级不输出。
- **测试**：261 个测试全部通过；clippy 仅 8 个风格警告。

---

## 六、修复验证记录（2026-08-17）

全部修复完成后的验证输出：

- `cargo fmt --all -- --check`：通过
- `cargo clippy --all-targets --all-features -- -D warnings`：零警告
- `cargo test --all-targets`：**261 passed; 0 failed**（与修复前持平，无回归）
- `node --test tests/frontend_live_drawer.mjs tests/frontend_concurrency.mjs tests/frontend_audit_remaining.mjs`：**12 pass / 0 fail**（其中一条与实现文本强耦合的断言已随签名演进更新）
- `npm run test:smoke`（Playwright）：**8 passed**（mockApi 严格化后补齐 `/api/blogger/saved/list` mock）
- `python build.py --check`：**all mandatory gates passed**
- `bash -n deploy/linux/install.sh deploy/termux/install.sh`：通过；`install.ps1` 括号/花括号配平通过

修复涉及的文件（Rust 22 个 + 前端 3 个 + 构建/CI/部署/文档 9 个）：

`src/services/live_recorder/{mod,interactions,ffmpeg_session}.rs`、`src/services/download/{audio_retry,monitor,manager,dispatch}.rs`、`src/services/danmu_collector/mod.rs`、`src/services/danmaku/fetch.rs`、`src/services/video_processor/merge.rs`、`src/services/{auth,bili_url_policy,secret_store,file_safety,wbi,subtitle_burner}.rs`、`src/services/monitor/logging.rs`、`src/api/{error→src/error.rs,auth,cover}.rs`、`src/api/download/{burn,proxy,queue_ops}.rs`、`src/api/live/{mod,history}.rs`、`src/api/history/{board,crud,file_download}.rs`、`src/api/video/{stream,sidecar}.rs`、`src/app/{security_server,onboarding,control,term_style}.rs`、`src/models/burn.rs`、`src/{main,config}.rs`；`static/js/{live,media-links}.js`；`build.py`、`.github/workflows/ci.yml`、`deploy/linux/install.sh`、`deploy/termux/install.sh`、`deploy/windows/install.ps1`、`Cargo.toml`、`.gitignore`、`CHANGELOG.md`、`resources/README.md`、`tests/frontend_smoke.spec.mjs`、`tests/frontend_live_drawer.mjs`。

## 七、误报澄清（审查过程中剔除的报告）

- **「audio_retry 的 `Cid.eq(None)` 生成 `cid = NULL` 恒为假 → 每 30 秒 remux + SHA-256 死循环」**——**不成立**。经核对本地 sea-orm 2.0.1 源码（`entity/column.rs:135-145`），`eq(None)` 被显式翻译为 `IS NULL`（文档示例明确该行为），降级状态可正常写成 `degraded`，死循环不存在。该文件的真实问题只有上文 #6（多P取 P1 音频）。
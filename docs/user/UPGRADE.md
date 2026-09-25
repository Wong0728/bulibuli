# 版本升级说明

本文记录跨版本升级时的注意事项：哪些改动需要停机、备份或调整部署脚本。各平台的日常升级命令以根 README 的「各平台升级方式」为准，本文不重复。

## 升级前必做

1. 先停止程序，确认没有旧进程仍在写 `data/`。
2. 备份**整个** `data/` 目录（数据库、下载目录、`security.toml`、`startup_state.json`、`logs`、迁移备份和运行状态）。设置页与 `/api/backup` 只生成数据库快照，不含密钥、设置文件和下载目录，不能当作升级前的完整回滚点；需要可完整恢复时使用 `/api/backup/full`。
3. 不要让新旧版本同时写同一数据库。不要在同一数据目录上并行跑两个版本，也不要把两个版本指向同一 `BILI__DATA_DIR` / `BULIBULI_DATA_DIR`。
4. 主密钥来源要与原环境一致（`data/.secret-store.key`、系统密钥环或 `BILI__MASTER_KEY`），否则升级后数据库里的 Cookie 等密文无法解密。
5. 升级后执行 `bulibuli ctl sys status`，确认 active/configured 模式、下载队列和直播恢复列表都符合预期。

任何升级入口都不会删除或覆盖 `data/`。

## 从 v2.0.0 升级到 v2.0.1

v2.0.1 是 v2.0.0 正式版之后的补丁版本，按补丁版规则走：接口和数据结构不做破坏性调整，升级动作与 README 描述一致。以下四项是这一轮里需要知道的实际变化。

### 迁移 018：`pub_date` 按本地时区回填

首次以 v2.0.1 启动时会自动执行迁移 `m20260830_000018_history_pub_date_local`：把 `history.pub_date` 回填为**进程本地时区**的日期，修正此前按 UTC 口径写入导致北京时间 00:00–07:59 投稿少一天、看板按日期分组错归前一天的问题。

- 迁移只改写确实不一致的行，重复执行无副作用，也不会改动 `version` 乐观锁字段。
- **该回填不可逆**：降级迁移会直接报错，因为原始 UTC 口径没有保留。需要回滚到 v2.0.0，只能用升级前备份的 `data/` 整体恢复，不能只换可执行文件。
- 回填结果取决于进程时区，与 Rust 侧写入同源。容器化部署请确认容器的时区设置与预期一致，否则日期分组会跟随容器时区。

### `ai assist` 短时授权层已删除

`ai assist` 这层短时授权能力已经移除，不存在对应的 `ctl` 子命令。AI 模式只有 `ai on` / `ai off` 开关，**开启后 AI 助手可执行与人工相同的全部操作**。如果自动化脚本里调用过 `ai assist`，需要改为 `ai on` 并在任务结束后 `ai off`；升级到本版本前先检查这类脚本。

### `static/app/` 是唯一前端产物目录

Vue 3 构建产物固定落在 `static/app/`，由根路由 `/` 提供。旧版的 `static/index.html`、`static/js/`、`static/css/` 以及独立的 `settings.html` / `setup.html` 页面文件都已移除：

- 直接静态托管 `static/` 的反代规则、书签或监控探针需要改指到 `/`。
- 旧的 `/settings.html` 直链不再提供页面，会被重定向回主界面。
- 自行拼装发布目录时，`static/app/index.html` 是安装器与 `build.py` 校验包完整性的必需文件，缺失会被判为无效包。

### 三项死配置已移除

`uid_history_limit`、`prefer_audio_quality`、`danmaku_download_all` 已从设置结构中删除，程序不再读取它们。历史清理数量改由 `storage.history_limit` 统一控制（设置值优先、环境变量兜底）。旧数据库 `runtime_config` 记录里若残留这三个键，会被直接忽略，下次保存设置时随序列化一并消失，不需要手工清理。

### 回滚

v2.0.1 回滚到 v2.0.0 的唯一安全路径是：停止程序 → 用升级前备份的 `data/` 整体恢复（含数据库、`security.toml`、`startup_state.json`、密钥材料和下载目录）→ 换回 v2.0.0 程序文件 → 启动后执行 `bulibuli ctl sys status` 复核。只换程序文件不回滚数据库，会因为 `pub_date` 已按本地时区回填而出现新旧口径混合。

## 从 alpha 版本直接升级

仍在运行 `v2.0.0-alpha.*` 的环境升级到正式版线时，除上文四项之外，还需要注意 alpha 阶段已经落地的变化同样生效：启动顺序改为后台服务在主端口绑定前完成启动，Setup 端口（默认 5001）独立提供首次配置页面，首次配对码可用 `bulibuli ctl pair` 重新生成而无需重启，`bulibuli --version` / `--help` 不再创建数据目录或启动服务，包内 `docs/` 目录承载 AI Skill 文档路径。Windows 上运行中更新的 Core 替换在退出程序后完成，可能残留 `bulibuli-core.old.exe`，可在退出后手动删除。

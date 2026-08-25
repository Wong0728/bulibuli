# 配置、状态与恢复

本文面向需要安装、部署、排障或让自动化代理操作 bulibuli 的使用者。先记住一个原则：不同存储各自负责不同类型的状态，不能只备份数据库，也不能把“已写入磁盘”理解成“当前监听器已经切换”。

## 从哪个入口开始

| 使用主体 | 先读什么 | 适合解决的问题 |
| --- | --- | --- |
| 普通用户 | [项目主页与安装说明](../../README.md) | 下载、首次配对、登录和日常使用 |
| 部署/运维 | [部署安全说明](../../deploy/SECURITY.md) 与本文 | 端口、LAN/proxy、数据目录、备份和恢复 |
| AI 或自动化代理 | [Skill 命令契约](../skill.md) 与本文的“ctl 检查” | 命令门控、状态判断、重启判断 |
| 开发者/审查者 | [贡献指南](../../CONTRIBUTING.md)、[安全策略](../../SECURITY.md)、[生命周期审查报告](../../启动与生命周期-静态审查报告.md) | 源码、验证边界和风险追踪 |

## 四个配置真相源

| 来源 | 保存内容 | 什么时候读取/生效 | 不负责什么 |
| --- | --- | --- | --- |
| `BILI__*` 环境变量 | 普通启动参数，如端口、数据目录、超时、TLS 校验；另有 `BILI__MASTER_KEY`、`BILI__SETUP_PORT_ENABLED` 等运行/密钥入口 | 启动时读取；改动通常需要重启 | 不写入 `runtime_config`，也不替代 `security.toml` 的业务结构 |
| `data/security.toml` | 监听模式、代理域名、访问默认策略/IP 规则、GeoIP、受信任 aria2/FFmpeg 等安全配置 | 启动时决定 listener；访问规则等非监听字段保存后可被当前进程读取；`local/lan/proxy` 模式切换需要重启 | 不保存业务设置、onboarding 或 AI 开关 |
| SQLite `runtime_config` | 网页设置中的业务配置，如下载、直播、烧录和存储策略 | 保存后热更新 | 不决定网络监听模式，也不保存启动向导状态 |
| `data/startup_state.json` | onboarding 完成、AI Skill 开关、终端模式、已知 B 站 UID 等启动状态 | 启动时读取；相关开关按各自 API 写入 | 不保存安全规则、Cookie 密文或业务设置 |

密钥另有独立边界：加密密文在数据库中，主密钥可能来自 `data/.secret-store.key`、系统密钥环或外部 `BILI__MASTER_KEY`。恢复时必须使用与原环境相同的主密钥来源，否则数据库中的 Cookie 等密文无法解密。

### active、configured 与重启

网络模式分成两个状态：

- `active_mode`：当前已经绑定 listener、实际用于请求安全链的模式。
- `configured_mode`：`security.toml` 中保存的下次启动目标模式。

执行 `bulibuli ctl mode lan` 或在 Setup 中保存模式后，若两者不同，返回 `restart_required: true`。重启前不要用当前页面地址推断新模式已经可用。可以先执行：

```text
bulibuli ctl sys status
```

输出中的 `active_mode`、`configured_mode` 和 `restart_required` 应一起判断；`mode` 字段保留为当前 active 模式以兼容旧脚本。

## 备份与恢复

### 数据库快照

`POST /api/backup` 只生成 SQLite 一致性快照。它适合在修改业务数据前留一份数据库回滚点，不是完整用户恢复包，不包含密钥来源、`security.toml`、`startup_state.json` 或下载文件。

### 完整恢复目录

Owner 使用 `POST /api/backup/full` 生成带 `BACKUP-MANIFEST.json` 的恢复目录。目录会尽量包含数据库快照、设置状态、密钥文件、日志和下载目录；下载文件按复制时状态保存，不是事务性文件系统快照。

恢复步骤：

1. 停止 bulibuli，确认没有旧进程继续写 `data/`。
2. 保护备份目录权限，不把它上传到 issue、网盘公开链接或日志系统。
3. 按 manifest 同时恢复数据库、`security.toml`、`startup_state.json`、密钥材料和下载目录。
4. 如果原来使用系统密钥环或 `BILI__MASTER_KEY`，在新环境重新配置同一主密钥来源。
5. 启动后执行 `bulibuli ctl sys status`，检查 active/configured 模式、下载队列和直播恢复列表。

完整备份仍需要人工恢复，没有自动覆盖当前运行目录的恢复按钮；这是为了避免正在运行的 SQLite、下载文件和密钥被半恢复状态覆盖。

## ctl 检查

AI Skill 未启用时，以下命令仍可用：`status`、`help`、`quit`、`ai`、`pair` 和只读诊断 `sys status`。其他高级命令先执行 `bulibuli ctl ai on`。

模式变更后使用 `sys status` 判断是否需要重启，不要只看 `security.toml` 是否已经写入。完整命令参数和返回错误码见 [Skill 命令契约](../skill.md)。

## 验证边界

源码检查、`cargo check`、单元测试和 CI 只能证明对应代码路径或构建门禁；它们不能替代真实浏览器跨端口交接、局域网另一台设备访问、完整备份恢复、强杀进程和各平台密钥环验证。发布或部署时应把这些运行验收单独记录。

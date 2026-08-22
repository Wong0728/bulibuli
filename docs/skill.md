# 补哩补哩 Skill

> 本文档供本机 AI 调用 `ctl` 命令使用，由 `COMMAND_REGISTRY` 自动生成，请勿手改。
> 命令清单与代码同步：修改命令后运行 `cargo test test_skill_doc_in_sync` 更新文档。

## 调用方式

所有命令通过本机 IPC 调用（无网络端口暴露）：

```
bulibuli.exe ctl <command> [args...]
```

返回 JSON 信封：
- 成功：`{"ok": true, "data": {...}}`
- 失败：`{"ok": false, "error": "...", "code": "..."}`

## 前置条件

1. **AI Skill 模式已启用**：网页 Setup 向导步骤 3 选启用，或运行 `ai on`。未启用时仅 `status` / `help` / `quit` / `ai` / `pair` 可用，其他命令返回 `AI_SKILL_DISABLED`。
启用后 AI 助手拥有与人工相同的全部操作权限（含 `mode` / `access` / `geo` / `trust` / `pair` 等基础配置命令），无需任何临时授权；所有 ctl 命令都要求服务已在运行。
2. **B 站已登录**：涉及 B 站 API 的命令（download / blogger / cookies / refresh）需先扫码登录，未登录返回 `BILI_NOT_LOGGED_IN`。
3. **本机权限**：命名管道仅本机进程可连接（SDDL 限制为系统/管理员/所有者）。

## 命令清单

### 策略 - 监听模式、访问规则、AI 开关

| 命令 | 说明 | 示例 |
|---|---|---|
| `mode local|lan|proxy <domain>` | 切换监听模式（重启后生效） | `mode lan` |
| `access default|allow|deny|remove|list` | 管理 IP 访问规则 | `access allow 192.168.1.0/24 --minutes 60` |
| `ai on|off` | 切换 AI Skill 模式（ctl 命令门控） | `ai on` |
| `geo cn on|off / geo db <path|remove>` | 大陆 IP 限制 / GeoIP 数据库 | `geo cn on` |
| `trust aria2|ffmpeg <value|remove>` | 信任外部 aria2 / FFmpeg | `trust ffmpeg /usr/bin/ffmpeg` |

### 下载 - 入队、暂停、重试、烧录

| 命令 | 说明 | 示例 |
|---|---|---|
| `dl status` | 查看队列状态（任务数、健康度） | `dl status` |
| `dl add <BV>` | 入队下载（仅支持 BV 号；AV/ep/ss/fp 暂未支持） | `dl add BV1xx411c7mD` |
| `dl pause <task_id|all>` | 暂停任务（all 暂停全部） | `dl pause 123` |
| `dl resume <task_id|all>` | 恢复任务（all 恢复全部） | `dl resume all` |
| `dl retry <bvid> [video|audio] | all-failed` | 重试任务（all-failed 重试全部失败） | `dl retry BV1xx411c7mD video` |
| `dl remove <bvid> [video|audio]` | 移除任务（默认 video） | `dl remove BV1xx411c7mD` |
| `dl priority <bvid> <level> [video|audio]` | 调整优先级（1..=300，默认 video） | `dl priority BV1xx411c7mD 200` |

### 博主 - 搜索、收藏、自动任务

| 命令 | 说明 | 示例 |
|---|---|---|
| `blg search <keyword>` | 按名字搜索 UP 主（B 站搜索） | `blg search 老番茄` |
| `blg add <uid>` | 添加博主为监控任务（自动拉取资料） | `blg add 12345` |
| `blg list [monitor|saved]` | 列出监控 / 收藏博主（默认全部） | `blg list monitor` |
| `blg del <uid>` | 删除博主（先试监控，再试收藏） | `blg del 12345` |
| `blg monitor on|off <uid>` | 启停博主监控 | `blg monitor on 12345` |

### 系统 - 状态、配置、日志、关停

| 命令 | 说明 | 示例 |
|---|---|---|
| `sys status` | 完整系统状态（运行时长、模式、aria2） | `sys status` |
| `sys config` | 查看安全配置 | `sys config` |
| `sys aria2-restart` | 重启 Aria2 引擎 | `sys aria2-restart` |
| `sys ffmpeg-test` | 探测并测试 FFmpeg 可用性 | `sys ffmpeg-test` |
| `sys logs` | 查看日志文件路径 | `sys logs` |
| `sys refresh board|blogger|video <bvid>` | 触发刷新（board / blogger / 单视频） | `sys refresh video BV1xx411c7mD` |
| `quit` | 优雅关停程序 | `quit` |
| `audit list [--source <s>] [--since <1h|24h|7d>] [--limit N]` | 查询审计日志（按来源/时间过滤） | `audit list --source ai_skill --since 1h` |
| `audit by-target <task|blogger|cookie|session> <id>` | 按目标资源查操作历史 | `audit by-target task 42` |
| `events [--watch] [--limit N]` | 查看最近事件（--watch 流式订阅） | `events --watch` |

### 凭证 - B 站登录、会话、配对

| 命令 | 说明 | 示例 |
|---|---|---|
| `cred qrcode` | 取扫码登录二维码 URL | `cred qrcode` |
| `cred qrcode-poll <qrcode_key>` | 轮询扫码状态（code=0 成功） | `cred qrcode-poll abc123` |
| `cred status` | 查看 B 站登录状态 | `cred status` |
| `pair [close]` | 服务器终端开启 / 关闭配对模式 | `pair` |
| `sessions` | 列出已配对会话 | `sessions` |
| `revoke <id|all>` | 撤销指定会话或全部会话 | `revoke all` |

## 乐观并发控制

状态变更类操作支持乐观锁，调用方传 `--expected-version N`：

```
dl pause <task_id> --expected-version 42
```

- 当前 version 匹配时执行 + version += 1，返回 `{"ok": true, "data": {"new_version": 43}}`
- 不匹配时返回 `CONFLICT` 错误 + 当前状态，调用方可重新读状态后重试
- 不传 `--expected-version` 时按「最后写入胜出」语义

## 实时事件订阅

```
bulibuli.exe ctl events --watch
```

流式输出 JSON Lines（每行一条审计事件），30 秒无事件发空行保活。
敏感操作（cookie 保存、pair code 生成）不广播，仅在审计日志中可查。

## 端到端流程示例

### 场景 1：下载某 UP 主最新视频

```
blg search <name>       # 搜索 UP 主拿 uid
blg add <uid>           # 添加监控
dl add <BV1xx411c7mD>   # 入队下载
dl status               # 轮询队列直到 completed
```

### 场景 2：扫码登录 B 站

```
cred qrcode              # 取二维码 URL + qrcode_key
# 提示用户用 B 站 App 扫码
cred qrcode-poll <key>   # 每 2 秒轮询，code=0 表示成功
cred status              # 确认登录状态
```

## 错误码

| 错误码 | 含义 |
|---|---|
| `AI_SKILL_DISABLED` | AI Skill 模式未启用，先执行 `ai on` |
| `BILI_NOT_LOGGED_IN` | B 站未登录，先执行 `cred qrcode` 扫码 |
| `CONFLICT` | 乐观锁冲突，重新读状态后重试 |
| `BAD_REQUEST` | 参数错误 |
| `NOT_FOUND` | 资源不存在 |
| `RISK_CONTROL` | 触发 B 站风控 |
| `INTERNAL` | 内部错误 |

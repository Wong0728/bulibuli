//! 本机控制通道：提供 IPC 服务并分发两套命令入口。
//!
//! 命令体系（P1 重构后）：
//! - **专家模式**：扁平命令，`bulibuli.exe ctl <command> [args...]` 直接调用，AI 脚本化友好
//! - **引导模式**：TUI 内的菜单包装，最终翻译为扁平命令调用 `execute()`（UI 在 `tui.rs`）
//! - **`>` 前缀**：TUI/stdin 输入时以 `>` 开头表示专家模式直输，由 `submit` 层剥离后调用 `execute()`
//!
//! 命令分为 5 个主题，另保留旧名 alias：
//! - `ai` / `dl` / `blg` / `sys` / `cred` —— 主题前缀，二级分发到各主题命令
//! - `status` / `pair` / `sessions` / `revoke` / `access` / `mode` / `geo` / `trust` / `config` / `quit` / `help`
//!   —— 旧名 alias 保留，避免破坏存量脚本
//!
//! AI 模式门控（P3）：未启用时仅放行 `status` / `help` / `quit` / `ai on` / `pair`。
//! 门控在 `execute()` 入口检查 `ai_skill_enabled` 标志，拒绝时返回 `AI_SKILL_DISABLED`。

#[path = "control_origin.rs"]
mod control_origin;

use crate::app::onboarding::StartupState;
use crate::error::{AppError, AppResult};
use crate::models::operation_log::{OperationOutcome, OperationSource, OperationTarget};
use crate::services::audit_log::AuditContext;
use crate::services::blogger::NewBlogger;
use crate::services::conflict_guard::ConflictGuard;
use crate::services::credential::Credential;
use crate::services::download::TaskOutcome;
use crate::services::security_config::{AccessAction, AccessMode};
use crate::services::url_parser::{parse_media_input, ResolvedMedia};
use crate::state::SharedState;
use ipnet::IpNet;
use serde_json::{json, Value};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;

const MAX_COMMAND_BYTES: usize = 8 * 1024;
#[cfg(windows)]
const PIPE_NAME: &str = r"\\.\pipe\bulibuli";

// --- 命令清单（自动生成 help；由测试断言与 docs/skill.md 同步） ---

/// 命令分类。`help` 输出按此分组；后续 `docs/skill.md` 也按此分节。
#[derive(Clone, Copy, PartialEq, Eq)]
enum CommandCategory {
    Policy,
    Download,
    Blogger,
    System,
    Credential,
}

impl CommandCategory {
    fn key(self) -> &'static str {
        match self {
            CommandCategory::Policy => "policy",
            CommandCategory::Download => "download",
            CommandCategory::Blogger => "blogger",
            CommandCategory::System => "system",
            CommandCategory::Credential => "credential",
        }
    }

    fn title(self) -> &'static str {
        match self {
            CommandCategory::Policy => "策略 - 监听模式、访问规则、AI 开关",
            CommandCategory::Download => "下载 - 入队、暂停、重试、烧录",
            CommandCategory::Blogger => "博主 - 搜索、收藏、自动任务",
            CommandCategory::System => "系统 - 状态、配置、日志、关停",
            CommandCategory::Credential => "凭证 - B 站登录、会话、配对",
        }
    }
}

/// 单条命令的元数据：名称、分类、一句话说明、调用示例。
/// `help` 自动遍历 `COMMAND_REGISTRY` 生成分组清单；P3 的 `docs/skill.md` 同步测试也以此为准。
struct CommandSpec {
    name: &'static str,
    category: CommandCategory,
    desc: &'static str,
    example: &'static str,
}

pub(crate) use control_origin::CommandOrigin;

const COMMAND_REGISTRY: &[CommandSpec] = &[
    // 策略
    CommandSpec {
        name: "mode local|lan|proxy <domain>",
        category: CommandCategory::Policy,
        desc: "切换监听模式（重启后生效）",
        example: "mode lan",
    },
    CommandSpec {
        name: "access default|allow|deny|remove|list",
        category: CommandCategory::Policy,
        desc: "管理 IP 访问规则",
        example: "access allow 192.168.1.0/24 --minutes 60",
    },
    CommandSpec {
        name: "ai on|off",
        category: CommandCategory::Policy,
        desc: "切换 AI Skill 模式（ctl 命令门控）",
        example: "ai on",
    },
    CommandSpec {
        name: "geo cn on|off / geo db <path|remove>",
        category: CommandCategory::Policy,
        desc: "大陆 IP 限制 / GeoIP 数据库",
        example: "geo cn on",
    },
    CommandSpec {
        name: "trust aria2|ffmpeg <value|remove>",
        category: CommandCategory::Policy,
        desc: "信任外部 aria2 / FFmpeg",
        example: "trust ffmpeg /usr/bin/ffmpeg",
    },
    // 下载
    CommandSpec {
        name: "dl status",
        category: CommandCategory::Download,
        desc: "查看队列状态（任务数、健康度）",
        example: "dl status",
    },
    CommandSpec {
        name: "dl add <BV>",
        category: CommandCategory::Download,
        desc: "入队下载（仅支持 BV 号；AV/ep/ss/fp 暂未支持）",
        example: "dl add BV1xx411c7mD",
    },
    CommandSpec {
        name: "dl pause <task_id|all>",
        category: CommandCategory::Download,
        desc: "暂停任务（all 暂停全部）",
        example: "dl pause 123",
    },
    CommandSpec {
        name: "dl resume <task_id|all>",
        category: CommandCategory::Download,
        desc: "恢复任务（all 恢复全部）",
        example: "dl resume all",
    },
    CommandSpec {
        name: "dl retry <bvid> [video|audio] | all-failed",
        category: CommandCategory::Download,
        desc: "重试任务（all-failed 重试全部失败）",
        example: "dl retry BV1xx411c7mD video",
    },
    CommandSpec {
        name: "dl remove <bvid> [video|audio]",
        category: CommandCategory::Download,
        desc: "移除任务（默认 video）",
        example: "dl remove BV1xx411c7mD",
    },
    CommandSpec {
        name: "dl priority <bvid> <level> [video|audio]",
        category: CommandCategory::Download,
        desc: "调整优先级（1..=300，默认 video）",
        example: "dl priority BV1xx411c7mD 200",
    },
    // 博主
    CommandSpec {
        name: "blg search <keyword>",
        category: CommandCategory::Blogger,
        desc: "按名字搜索 UP 主（B 站搜索）",
        example: "blg search 老番茄",
    },
    CommandSpec {
        name: "blg add <uid>",
        category: CommandCategory::Blogger,
        desc: "添加博主为监控任务（自动拉取资料）",
        example: "blg add 12345",
    },
    CommandSpec {
        name: "blg list [monitor|saved]",
        category: CommandCategory::Blogger,
        desc: "列出监控 / 收藏博主（默认全部）",
        example: "blg list monitor",
    },
    CommandSpec {
        name: "blg del <uid>",
        category: CommandCategory::Blogger,
        desc: "删除博主（先试监控，再试收藏）",
        example: "blg del 12345",
    },
    CommandSpec {
        name: "blg monitor on|off <uid>",
        category: CommandCategory::Blogger,
        desc: "启停博主监控",
        example: "blg monitor on 12345",
    },
    // 系统
    CommandSpec {
        name: "sys status",
        category: CommandCategory::System,
        desc: "完整系统状态（运行时长、模式、aria2）",
        example: "sys status",
    },
    CommandSpec {
        name: "sys config",
        category: CommandCategory::System,
        desc: "查看安全配置",
        example: "sys config",
    },
    CommandSpec {
        name: "sys aria2-restart",
        category: CommandCategory::System,
        desc: "重启 Aria2 引擎",
        example: "sys aria2-restart",
    },
    CommandSpec {
        name: "sys ffmpeg-test",
        category: CommandCategory::System,
        desc: "探测并测试 FFmpeg 可用性",
        example: "sys ffmpeg-test",
    },
    CommandSpec {
        name: "sys logs",
        category: CommandCategory::System,
        desc: "查看日志文件路径",
        example: "sys logs",
    },
    CommandSpec {
        name: "sys refresh board|blogger|video <bvid>",
        category: CommandCategory::System,
        desc: "触发刷新（board / blogger / 单视频）",
        example: "sys refresh video BV1xx411c7mD",
    },
    CommandSpec {
        name: "quit",
        category: CommandCategory::System,
        desc: "优雅关停程序",
        example: "quit",
    },
    // 凭证
    CommandSpec {
        name: "cred qrcode",
        category: CommandCategory::Credential,
        desc: "取扫码登录二维码 URL",
        example: "cred qrcode",
    },
    CommandSpec {
        name: "cred qrcode-poll <qrcode_key>",
        category: CommandCategory::Credential,
        desc: "轮询扫码状态（code=0 成功）",
        example: "cred qrcode-poll abc123",
    },
    CommandSpec {
        name: "cred status",
        category: CommandCategory::Credential,
        desc: "查看 B 站登录状态",
        example: "cred status",
    },
    CommandSpec {
        name: "pair [close]",
        category: CommandCategory::Credential,
        desc: "服务器终端开启 / 关闭配对模式",
        example: "pair",
    },
    CommandSpec {
        name: "sessions",
        category: CommandCategory::Credential,
        desc: "列出已配对会话",
        example: "sessions",
    },
    CommandSpec {
        name: "revoke <id|all>",
        category: CommandCategory::Credential,
        desc: "撤销指定会话或全部会话",
        example: "revoke all",
    },
    // 审计与事件（P2）
    CommandSpec {
        name: "audit list [--source <s>] [--since <1h|24h|7d>] [--limit N]",
        category: CommandCategory::System,
        desc: "查询审计日志（按来源/时间过滤）",
        example: "audit list --source ai_skill --since 1h",
    },
    CommandSpec {
        name: "audit by-target <task|blogger|cookie|session> <id>",
        category: CommandCategory::System,
        desc: "按目标资源查操作历史",
        example: "audit by-target task 42",
    },
    CommandSpec {
        name: "events [--watch] [--limit N]",
        category: CommandCategory::System,
        desc: "查看最近事件（--watch 流式订阅）",
        example: "events --watch",
    },
];

// --- IPC 服务器入口（保留原实现） ---

pub fn start_server(state: SharedState) {
    tokio::spawn(async move {
        if let Err(error) = serve_ipc(state).await {
            tracing::error!(%error, "本机控制通道退出");
        }
    });
}

/// 终端界面未启用时的回退：从 stdin 逐行读命令，错误只回显不中断服务。
pub fn start_stdin_loop(state: SharedState) {
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        return;
    }
    tokio::spawn(async move {
        use tokio::io::{AsyncBufReadExt, BufReader};
        let mut lines = BufReader::new(tokio::io::stdin()).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            // `>` 前缀 = 专家模式直输；剥离后走扁平命令路径
            let trimmed = strip_expert_prefix(&line);
            let args = split_command(trimmed);
            match execute_from(&state, &args, CommandOrigin::HumanTerminal).await {
                Ok(value) => println!("{}", format_response(&value)),
                Err(error) => eprintln!("控制命令失败：{error}"),
            }
        }
    });
}

pub async fn run_client(data_dir: &Path, args: &[String]) -> AppResult<String> {
    #[cfg(windows)]
    let _data_dir = data_dir;
    let request = serde_json::to_vec(args)?;
    if request.len() > MAX_COMMAND_BYTES {
        return Err(AppError::BadRequest("控制命令过长".to_string()));
    }
    #[cfg(unix)]
    {
        let mut stream = connect_unix_control(data_dir).await?;
        stream.write_all(&request).await?;
        stream.shutdown().await?;
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await?;
        String::from_utf8(response)
            .map_err(|error| AppError::Internal(format!("控制响应不是 UTF-8: {error}")))
    }
    #[cfg(windows)]
    {
        let mut stream = tokio::net::windows::named_pipe::ClientOptions::new().open(PIPE_NAME)?;
        // 连接后先校验服务端身份（S2）：任何本机进程都能创建同名管道冒充服务端。
        verify_pipe_server_identity(&stream)?;
        stream.write_all(&request).await?;
        stream.shutdown().await?;
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await?;
        String::from_utf8(response)
            .map_err(|error| AppError::Internal(format!("控制响应不是 UTF-8: {error}")))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _unused = data_dir;
        Err(AppError::Config("当前平台不支持本机控制通道".to_string()))
    }
}

#[cfg(unix)]
// 采用 macOS 更严格的 104 字节上限（含结尾 NUL），Linux 也可安全使用。
const UNIX_SOCKET_MAX_PATH_BYTES: usize = 103;

/// 校验命名管道服务端身份（S2）。
///
/// 威胁模型：管道名 `\\.\pipe\bulibuli` 是本机固定名字，任何本机进程都可以
/// 先创建同名管道冒充服务端，骗取 ctl 客户端发来的命令（配对码、扫码落盘
/// Cookie 等敏感操作）。管道的 SACL 只约束"谁能连"，不约束"谁在监听"。
///
/// 校验方式：连接成功后通过 GetNamedPipeServerProcessId 取服务端 PID，
/// 再以 PROCESS_QUERY_LIMITED_INFORMATION（同用户进程即可查询，无需高权限）
/// 打开并 QueryFullProcessImageNameW 取服务端进程的完整镜像路径，要求与
/// 客户端自身镜像路径一致（大小写不敏感，Windows 路径不区分大小写）——
/// 即服务端必须是同一份 bulibuli 可执行文件。PID 复用竞态由镜像路径比对
/// 兜底：复用 PID 指向的其他进程路径必然不同。
///
/// 局限（有意接受）：不校验服务端是否被注入/调试——攻击者若能让同一镜像
/// 路径的进程执行任意代码，等于可执行文件本身已被攻破，超出本机信任边界。
/// 校验失败时立即断开（drop 连接）并返回错误，不发送任何命令。
#[cfg(windows)]
fn verify_pipe_server_identity<Client: std::os::windows::io::AsRawHandle>(
    client: &Client,
) -> AppResult<()> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    let fail =
        |message: String| AppError::Internal(format!("控制通道服务端身份校验失败：{message}"));

    unsafe {
        let mut server_pid: u32 = 0;
        if GetNamedPipeServerProcessId(client.as_raw_handle(), &mut server_pid) == 0 {
            return Err(fail("无法获取服务端进程 ID".to_string()));
        }
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, server_pid);
        if process.is_null() {
            return Err(fail(format!(
                "无法打开服务端进程（PID {server_pid}，可能已在退出中）"
            )));
        }
        let mut buffer = [0u16; 1024];
        let mut length = buffer.len() as u32;
        let queried = QueryFullProcessImageNameW(process, 0, buffer.as_mut_ptr(), &mut length);
        CloseHandle(process);
        if queried == 0 {
            return Err(fail("无法查询服务端进程镜像路径".to_string()));
        }
        let server_image = String::from_utf16_lossy(&buffer[..length as usize]);
        let own_image = std::env::current_exe()
            .map_err(|error| fail(format!("无法获取本进程路径: {error}")))?;
        if !server_image.eq_ignore_ascii_case(&own_image.to_string_lossy()) {
            // 拒绝并断开：连接随 client 被 drop 关闭，不发送命令。
            return Err(fail(format!(
                "服务端镜像 {} 与本进程 {} 不一致，疑似同名管道冒充，已断开",
                server_image,
                own_image.display()
            )));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn unix_control_socket_candidates(data_dir: &Path) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        let runtime_dir = PathBuf::from(runtime_dir);
        if runtime_dir.is_absolute() {
            candidates.push(runtime_dir.join("bulibuli").join("control.sock"));
        }
    }
    candidates.push(data_dir.join("control.sock"));

    // 长数据目录在没有 XDG_RUNTIME_DIR 的最小容器中仍然需要一个稳定的短路径。
    // 用户名只用于隔离临时目录名，不参与任何权限判断。
    let username = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "user".to_string());
    let safe_username: String = username
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
        .take(32)
        .collect();
    let safe_username = if safe_username.is_empty() {
        "user"
    } else {
        safe_username.as_str()
    };
    candidates.push(
        std::env::temp_dir()
            .join(format!("bulibuli-{safe_username}"))
            .join("control.sock"),
    );

    candidates.retain(|path| path.as_os_str().as_bytes().len() <= UNIX_SOCKET_MAX_PATH_BYTES);
    candidates.dedup();
    candidates
}

#[cfg(unix)]
async fn connect_unix_control(data_dir: &Path) -> std::io::Result<tokio::net::UnixStream> {
    let mut last_error = None;
    for path in unix_control_socket_candidates(data_dir) {
        match tokio::net::UnixStream::connect(&path).await {
            Ok(stream) => return Ok(stream),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "没有可用的 bulibuli Unix 控制通道路径",
        )
    }))
}

// --- 命令分发 ---

/// 执行通过仅供 AI 使用的 `ctl` IPC 到达的命令。
pub async fn execute(state: &SharedState, args: &[String]) -> AppResult<Value> {
    execute_from(state, args, CommandOrigin::AiCtl).await
}

/// 按可信的本机来源执行命令。只有 TUI/stdin 可以传入 `HumanTerminal`；
/// IPC 调用方始终使用上面的 `execute` 包装函数。
pub(crate) async fn execute_from(
    state: &SharedState,
    args: &[String],
    origin: CommandOrigin,
) -> AppResult<Value> {
    let Some(command) = args.first().map(String::as_str) else {
        return Ok(help());
    };
    // AI Skill 模式门控：仅在 AI IPC 来源时生效，人工终端（TUI/stdin）无条件放行。
    // 未启用时仅放行 status / help / quit / ai（用于重新启用）/ pair，
    // 以及只读诊断 `sys status`（--help 推荐新用户首先执行的命令）。
    let sys_status_probe = command == "sys" && args.get(1).map(String::as_str) == Some("status");
    if matches!(origin, CommandOrigin::AiCtl)
        && !state.infra.ai_skill_enabled.load(Ordering::Relaxed)
        && !matches!(command, "status" | "help" | "quit" | "ai" | "pair")
        && !sys_status_probe
    {
        return Err(AppError::AiSkillDisabled(format!(
            "AI Skill 模式未启用，ctl 仅放行 status/help/quit/ai/pair/sys status；当前命令 `{command}` 被拒绝。使用 `ai on` 启用"
        )));
    }
    match command {
        // 旧名 alias（保留以兼容现有脚本）
        "help" => Ok(help()),
        "status" => Ok(legacy_status(state).await),
        "pair" => pair_command(state, args).await,
        "sessions" => Ok(serde_json::to_value(
            state.bili.auth.list_sessions().await?,
        )?),
        "revoke" => revoke_command(state, args).await,
        "access" => access_command(state, args).await,
        "mode" => mode_command(state, args).await,
        "geo" => geo_command(state, args).await,
        "trust" => trust_command(state, args).await,
        "config" => Ok(sys_config_value(state)),
        "quit" => quit_command(state).await,
        // 新主题分发
        "ai" => ai_command(state, args).await,
        "dl" => dl_command(state, args).await,
        "blg" => blg_command(state, args).await,
        "sys" => sys_command(state, args).await,
        "cred" => cred_command(state, args).await,
        // P2 审计与事件
        "audit" => audit_command(state, args).await,
        "events" => events_command(state, args).await,
        _ => Err(AppError::BadRequest(format!(
            "未知控制命令 `{command}`；使用 help 查看"
        ))),
    }
}

/// `pair [close]`：开启/关闭配对模式。
/// 开启配对会生成配对码，这是敏感操作（一次性访问凭证）；审计走 record_silent，不广播事件。
/// AI 模式开启后 AI 与人工拥有相同权限，配对始终允许（不再需要短时授权层）。
async fn pair_command(state: &SharedState, args: &[String]) -> AppResult<Value> {
    // 只接受缺省（开启）或 close：未知参数报错，避免 `pair xxx` 被静默当作开配对。
    if let Some(unknown) = args.get(1).filter(|value| value.as_str() != "close") {
        return Err(AppError::BadRequest(format!(
            "pair 不支持参数 `{unknown}`；用法：pair [close]"
        )));
    }
    let is_close = args.get(1).is_some_and(|value| value == "close");
    let ctx = ctl_audit_ctx(
        &args.join(" "),
        OperationTarget::Session,
        None,
        if is_close { "pair_close" } else { "pair_open" },
        None,
    );
    let auth = state.bili.auth.clone();
    let result: AppResult<Value> = if is_close {
        auth.close_pairing().await;
        Ok(json!({"pairing_open": false}))
    } else {
        let code = auth.open_pairing().await;
        Ok(json!({"pairing_code": format!("{}-{}", &code[..4], &code[4..])}))
    };
    match result {
        Ok(value) => {
            state
                .infra
                .audit_log
                .record_silent(&ctx, OperationOutcome::Success, None, None, None)
                .await;
            Ok(value)
        }
        Err(error) => {
            let code = error_code_for(&error);
            state
                .infra
                .audit_log
                .record_silent(
                    &ctx,
                    OperationOutcome::Error,
                    None,
                    Some(code),
                    Some(json!({"error": error.to_string()})),
                )
                .await;
            Err(error)
        }
    }
}

/// `quit`：优雅关停。审计后取消 cancellation token。
async fn quit_command(state: &SharedState) -> AppResult<Value> {
    let ctx = ctl_audit_ctx("quit", OperationTarget::Session, None, "quit", None);
    state
        .infra
        .audit_log
        .record(&ctx, OperationOutcome::Success, None, None, None)
        .await;
    state.infra.cancellation.cancel();
    Ok(json!({"shutting_down": true}))
}

// --- 主题 1：策略（ai 命令；mode/access/geo/trust 保留旧实现） ---

async fn ai_command(state: &SharedState, args: &[String]) -> AppResult<Value> {
    let Some(value) = args.get(1).map(String::as_str) else {
        return Ok(json!({
            "ai_skill_enabled": state.infra.ai_skill_enabled.load(Ordering::Relaxed),
        }));
    };
    let enabled = match value {
        "on" => true,
        "off" => false,
        _ => return Err(AppError::BadRequest("用法：ai on|off".to_string())),
    };
    audit_only_op(
        state,
        &format!("ai {value}"),
        OperationTarget::Settings,
        None,
        "ai_toggle",
        || async move {
            // 1. 更新内存门控标志（ctl 命令门控以此为准）
            state
                .infra
                .ai_skill_enabled
                .store(enabled, Ordering::Relaxed);
            // 2. 持久化到 startup_state.json（下次启动沿用）
            StartupState::save_ai_flag(&state.infra.paths.data_dir, enabled)
                .map_err(|error| AppError::Internal(format!("写入 AI 开关失败: {error}")))?;
            Ok(json!({
                "ai_skill_enabled": enabled,
                "note": if enabled {
                    "AI Skill 模式已启用，AI 助手可执行与人工相同的全部 ctl 命令（含 mode/access/geo/trust/pair）"
                } else {
                    "AI Skill 模式已关闭，ctl 仅放行 status/help/quit/ai/pair"
                },
            }))
        },
    )
    .await
}

// --- 主题 2：下载（dl 命令） ---

async fn dl_command(state: &SharedState, args: &[String]) -> AppResult<Value> {
    let Some(sub) = args.get(1).map(String::as_str) else {
        return Err(AppError::BadRequest(
            "用法：dl status|add|pause|resume|retry|remove|priority".to_string(),
        ));
    };
    match sub {
        "status" => Ok(json!({
            "queue": state.media.download_manager.queue_metrics().await?,
            "health": state.media.download_manager.get_health().await,
        })),
        "add" => dl_add(state, args).await,
        "pause" => dl_pause_resume(state, args, "pause").await,
        "resume" => dl_pause_resume(state, args, "resume").await,
        "retry" => dl_retry(state, args).await,
        "remove" => dl_remove(state, args).await,
        "priority" => dl_priority(state, args).await,
        // 字幕烧录编排仅 Web 支持（前端 /api/download/burn），ctl 不提供半实现入口，
        // 避免"帮助可见、调用必错"的假命令。
        _ => Err(AppError::BadRequest(format!(
            "未知 dl 子命令 `{sub}`；用法：dl status|add|pause|resume|retry|remove|priority（烧录请用前端网页）"
        ))),
    }
}

/// `dl add <BV>`：取流 + 入队（source=manual）。
/// 仅支持 BV；AV/ep/ss/fp 在 P1 暂未支持，明确返回错误。
///
/// 审计：target=Task, target_id=bvid, action=add。幂等入队（同 bvid 返回同一 task_id），
/// 不要求 expected_version；outcome 由 add_task 返回值决定（accepted/rejected/done）。
async fn dl_add(state: &SharedState, args: &[String]) -> AppResult<Value> {
    let input = required(args, 2, "dl add 需要 BV 号")?;
    let media =
        parse_media_input(input).map_err(|error| AppError::BadRequest(error.to_string()))?;
    let bvid = match media {
        ResolvedMedia::VideoBv(b) => b,
        ResolvedMedia::VideoAv(_)
        | ResolvedMedia::Episode(_)
        | ResolvedMedia::Season(_)
        | ResolvedMedia::Course(_) => {
            return Err(AppError::BadRequest(
                "AV/ep/ss/fp 暂未支持，请用 BV 号（dl add BV1xx411c7mD）".to_string(),
            ))
        }
    };

    let cookies = require_bili_login(state).await?;
    let settings = state.infra.settings_service.current();
    let qn = settings.query.video_quality;
    let fnval = settings.query.video_format;

    // 获取标题
    let info = state
        .bili
        .bili_api
        .get_video_info(&bvid, &cookies)
        .await
        .map_err(|e| AppError::Internal(format!("获取视频信息失败: {e}")))?;
    let title = if info.title.is_empty() {
        bvid.clone()
    } else {
        info.title.clone()
    };

    // 获取单 P 流（cid=None 使用默认分 P）
    let streams = state
        .bili
        .bili_api
        .get_video_urls(&bvid, &cookies, fnval, Some(qn), None)
        .await
        .map_err(|e| AppError::Internal(format!("获取视频流失败: {e}")))?;
    let selected = streams
        .qualities
        .iter()
        .filter(|q| q.quality <= qn)
        .max_by_key(|q| q.quality)
        .or_else(|| streams.qualities.first())
        .ok_or_else(|| AppError::NotFound("该视频无可用的视频流".to_string()))?;
    if selected.url.is_empty() {
        return Err(AppError::NotFound("获取视频下载链接失败".to_string()));
    }

    let ctx = ctl_audit_ctx(
        &format!("dl add {bvid}"),
        OperationTarget::Task,
        Some(bvid.clone()),
        "add",
        None,
    );
    let dm = state.media.download_manager.clone();
    let url = selected.url.clone();
    let quality = selected.quality;
    let bvid_for_closure = bvid.clone();
    let title_for_closure = title.clone();
    let outcome = with_guarded_op(
        state,
        ctx,
        OperationTarget::Task,
        Some(&bvid),
        None,
        move |_guard| async move {
            dm.add_task(
                &bvid_for_closure,
                &title_for_closure,
                &url,
                &cookies,
                quality,
                "video",
                None,
                "manual",
                None,
                None,
            )
            .await
            .map_err(AppError::from)
        },
    )
    .await?;
    Ok(outcome_to_value(outcome))
}

/// `dl pause <task_id|all> [--expected-version N]` / `dl resume <task_id|all> [--expected-version N]`
///
/// 乐观锁：`task_id` 为具体数字且传 `--expected-version` 时启用；`all` 跳过乐观锁（审计 only）。
async fn dl_pause_resume(state: &SharedState, args: &[String], action: &str) -> AppResult<Value> {
    let (args, expected_version) = extract_expected_version(args)?;
    let target = required(&args, 2, &format!("dl {action} 需要 task_id 或 all"))?;
    let task_id = parse_task_id_or_all(target)?;
    // all 模式为批量操作，不使用乐观锁（影响多行，version 无意义）
    let (target_id_for_guard, version_for_ctx) = match task_id {
        Some(id) => (Some(id.to_string()), expected_version),
        None => (None, None),
    };
    let ctx = ctl_audit_ctx(
        &format!("dl {action} {target}"),
        OperationTarget::Task,
        target_id_for_guard.clone(),
        action,
        version_for_ctx,
    );
    let dm = state.media.download_manager.clone();
    let outcome = with_guarded_op(
        state,
        ctx,
        OperationTarget::Task,
        target_id_for_guard.as_deref(),
        expected_version,
        move |_guard| async move {
            if action == "pause" {
                dm.pause_task(task_id).await.map_err(AppError::from)
            } else {
                dm.resume_task(task_id).await.map_err(AppError::from)
            }
        },
    )
    .await?;
    Ok(outcome_to_value(outcome))
}

/// `dl retry <bvid> [video|audio] | all-failed`
///
/// 审计：target=Task, target_id=bvid 或 "all-failed", action=retry。
/// 入参是 bvid（非 task_id），不传 expected_version；按"最后写入胜出"语义。
async fn dl_retry(state: &SharedState, args: &[String]) -> AppResult<Value> {
    let target = required(args, 2, "dl retry 需要 bvid 或 all-failed")?;
    if target == "all-failed" {
        let ctx = ctl_audit_ctx(
            "dl retry all-failed",
            OperationTarget::Task,
            None,
            "retry_all_failed",
            None,
        );
        let dm = state.media.download_manager.clone();
        let outcome = with_guarded_op(
            state,
            ctx,
            OperationTarget::Task,
            None,
            None,
            move |_guard| async move { dm.retry_all_failed(None).await.map_err(AppError::from) },
        )
        .await?;
        return Ok(outcome_to_value(outcome));
    }
    let task_type = args.get(3).map(String::as_str).unwrap_or("video");
    if !matches!(task_type, "video" | "audio") {
        return Err(AppError::BadRequest(
            "task_type 必须是 video 或 audio".to_string(),
        ));
    }
    let ctx = ctl_audit_ctx(
        &format!("dl retry {target} {task_type}"),
        OperationTarget::Task,
        Some(target.to_string()),
        "retry",
        None,
    );
    let dm = state.media.download_manager.clone();
    let bvid = target.to_string();
    let outcome = with_guarded_op(
        state,
        ctx,
        OperationTarget::Task,
        Some(target),
        None,
        move |_guard| async move {
            dm.retry_task(&bvid, task_type)
                .await
                .map_err(AppError::from)
        },
    )
    .await?;
    Ok(outcome_to_value(outcome))
}

/// `dl remove <bvid> [video|audio]`
///
/// 审计：target=Task, target_id=bvid, action=remove。
async fn dl_remove(state: &SharedState, args: &[String]) -> AppResult<Value> {
    let bvid = required(args, 2, "dl remove 需要 bvid")?;
    let task_type = args.get(3).map(String::as_str).unwrap_or("video");
    if !matches!(task_type, "video" | "audio") {
        return Err(AppError::BadRequest(
            "task_type 必须是 video 或 audio".to_string(),
        ));
    }
    let ctx = ctl_audit_ctx(
        &format!("dl remove {bvid} {task_type}"),
        OperationTarget::Task,
        Some(bvid.to_string()),
        "remove",
        None,
    );
    let dm = state.media.download_manager.clone();
    let bvid_owned = bvid.to_string();
    let outcome = with_guarded_op(
        state,
        ctx,
        OperationTarget::Task,
        Some(bvid),
        None,
        move |_guard| async move {
            dm.remove_task(&bvid_owned, task_type)
                .await
                .map_err(AppError::from)
        },
    )
    .await?;
    Ok(outcome_to_value(outcome))
}

/// `dl priority <bvid> <level> [video|audio]`
///
/// 审计：target=Task, target_id=bvid, action=priority。
async fn dl_priority(state: &SharedState, args: &[String]) -> AppResult<Value> {
    let bvid = required(args, 2, "dl priority 需要 bvid")?;
    let level_str = required(args, 3, "dl priority 需要 level（1..=300）")?;
    let level: i32 = level_str
        .parse()
        .map_err(|_| AppError::BadRequest("level 必须是 1..=300 的整数".to_string()))?;
    if !(1..=300).contains(&level) {
        return Err(AppError::BadRequest(
            "level 必须在 1..=300 范围内".to_string(),
        ));
    }
    let task_type = args.get(4).map(String::as_str).unwrap_or("video");
    if !matches!(task_type, "video" | "audio") {
        return Err(AppError::BadRequest(
            "task_type 必须是 video 或 audio".to_string(),
        ));
    }
    let ctx = ctl_audit_ctx(
        &format!("dl priority {bvid} {level} {task_type}"),
        OperationTarget::Task,
        Some(bvid.to_string()),
        "priority",
        None,
    );
    let dm = state.media.download_manager.clone();
    let bvid_owned = bvid.to_string();
    let result = with_guarded_op(
        state,
        ctx,
        OperationTarget::Task,
        Some(bvid),
        None,
        move |_guard| async move {
            dm.set_priority(&bvid_owned, task_type, level)
                .await
                .map_err(|e| AppError::BadRequest(e.to_string()))
        },
    )
    .await?;
    Ok(result)
}

// --- 主题 3：博主（blg 命令） ---

async fn blg_command(state: &SharedState, args: &[String]) -> AppResult<Value> {
    let Some(sub) = args.get(1).map(String::as_str) else {
        return Err(AppError::BadRequest(
            "用法：blg search|add|list|del|monitor".to_string(),
        ));
    };
    match sub {
        "search" => blg_search(state, args).await,
        "add" => blg_add(state, args).await,
        "list" => blg_list(state, args).await,
        "del" => blg_del(state, args).await,
        "monitor" => blg_monitor(state, args).await,
        _ => Err(AppError::BadRequest(format!(
            "未知 blg 子命令 `{sub}`；用法：blg search|add|list|del|monitor"
        ))),
    }
}

/// `blg search <keyword>`：B 站用户搜索（page=1, page_size=10）
async fn blg_search(state: &SharedState, args: &[String]) -> AppResult<Value> {
    let keyword = required(args, 2, "blg search 需要 keyword")?;
    let cookies = require_bili_login(state).await?;
    let page = state
        .bili
        .bili_api
        .search_users(keyword, &cookies, 1, 10)
        .await
        .map_err(|e| AppError::Internal(format!("搜索 UP 主失败: {e}")))?;
    Ok(json!({
        "total": page.total,
        "users": page.users,
    }))
}

/// `blg add <uid>`：加为监控任务（has_auto_task=true, monitor_enabled=true）
///
/// 审计：target=Blogger, target_id=uid, action=add。
async fn blg_add(state: &SharedState, args: &[String]) -> AppResult<Value> {
    let uid = required(args, 2, "blg add 需要 uid")?;
    let uid_i64: i64 = uid
        .parse()
        .map_err(|_| AppError::BadRequest("uid 必须是整数".to_string()))?;
    let cookies = require_bili_login(state).await?;

    // 预查重复：与 Web（manage.rs）一致返回友好 Conflict，而不是数据库唯一键错误
    // 被包成 INTERNAL "添加博主失败"。
    if let Some(existing) = state
        .business
        .blogger_service
        .find_by_uid(uid)
        .await
        .map_err(|e| AppError::Internal(format!("查询博主失败: {e}")))?
    {
        if existing.has_auto_task {
            return Err(AppError::Conflict("该博主已有自动任务".to_string()));
        }
    }

    // 拉取资料（失败时回退到空资料，不阻断添加）
    let (name, face, sign, level, fans) =
        match state.bili.bili_api.get_user_info(uid_i64, &cookies).await {
            Ok(profile) => (
                Some(profile.name).filter(|s| !s.is_empty()),
                Some(profile.face).filter(|s| !s.is_empty()),
                Some(profile.sign).filter(|s| !s.is_empty()),
                Some(profile.level as i32),
                Some(profile.fans),
            ),
            Err(error) => {
                tracing::warn!(uid, %error, "blg add 拉取资料失败，使用兜底信息");
                (None, None, None, None, None)
            }
        };

    let ctx = ctl_audit_ctx(
        &format!("blg add {uid}"),
        OperationTarget::Blogger,
        Some(uid.to_string()),
        "add",
        None,
    );
    let svc = state.business.blogger_service.clone();
    let uid_for_closure = uid.to_string();
    let blogger = with_guarded_op(
        state,
        ctx,
        OperationTarget::Blogger,
        Some(uid),
        None,
        move |_guard| async move {
            svc.add_blogger(NewBlogger {
                uid: uid_for_closure,
                name,
                min_interval: 60,
                max_interval: 300,
                face,
                sign,
                level,
                fans,
                download_video: true,
                download_danmaku: true,
                download_comments: true,
                download_cover: true,
                burn_danmaku: false,
                burn_subtitle: false,
                series_filter_regex: None,
                active_windows: None,
                monitor_enabled: true,
                is_saved: false,
                has_auto_task: true,
            })
            .await
            .map_err(|e| AppError::Internal(format!("添加博主失败: {e}")))
        },
    )
    .await?;
    Ok(json!({
        "blogger": blogger,
        "message": "博主已添加为监控任务",
    }))
}

/// `blg list [monitor|saved]`
async fn blg_list(state: &SharedState, args: &[String]) -> AppResult<Value> {
    let filter = args.get(2).map(String::as_str);
    let mut result = json!({});
    if matches!(filter, None | Some("monitor")) {
        let monitor = state
            .business
            .blogger_service
            .list_auto_tasks()
            .await
            .map_err(|e| AppError::Internal(format!("列出监控博主失败: {e}")))?;
        result["monitor"] = serde_json::to_value(monitor)?;
    }
    if matches!(filter, None | Some("saved")) {
        let saved = state
            .business
            .blogger_service
            .list_saved()
            .await
            .map_err(|e| AppError::Internal(format!("列出收藏博主失败: {e}")))?;
        result["saved"] = serde_json::to_value(saved)?;
    }
    if let Some(other) = filter.filter(|f| !matches!(*f, "monitor" | "saved")) {
        return Err(AppError::BadRequest(format!(
            "blg list 参数必须是 monitor|saved，收到 `{other}`"
        )));
    }
    Ok(result)
}

/// `blg del <uid> [--expected-version N]`：先试监控，再试收藏
///
/// 乐观锁：传 `--expected-version` 时校验 blogger.version；不匹配返回 CONFLICT。
async fn blg_del(state: &SharedState, args: &[String]) -> AppResult<Value> {
    let (args, expected_version) = extract_expected_version(args)?;
    let uid = required(&args, 2, "blg del 需要 uid")?;
    // find_by_uid 拿 id（监控和收藏共用同一张表，uid 唯一）
    let blogger = state
        .business
        .blogger_service
        .find_by_uid(uid)
        .await
        .map_err(|e| AppError::Internal(format!("查询博主失败: {e}")))?
        .ok_or_else(|| AppError::NotFound("未找到该博主".to_string()))?;
    let id = blogger.id;
    let ctx = ctl_audit_ctx(
        &format!("blg del {uid}"),
        OperationTarget::Blogger,
        Some(id.to_string()),
        "delete",
        expected_version,
    );
    let svc = state.business.blogger_service.clone();
    let result = with_guarded_op(
        state,
        ctx,
        OperationTarget::Blogger,
        Some(&id.to_string()),
        expected_version,
        move |_guard| async move {
            if blogger.has_auto_task {
                let removed_uid = svc
                    .remove_auto_task(id)
                    .await
                    .map_err(|e| AppError::Internal(format!("删除监控博主失败: {e}")))?
                    .ok_or_else(|| AppError::NotFound("未找到监控博主".to_string()))?;
                Ok(json!({"removed": removed_uid, "kind": "monitor"}))
            } else if blogger.is_saved {
                let removed_uid = svc
                    .remove_saved(id)
                    .await
                    .map_err(|e| AppError::Internal(format!("删除收藏博主失败: {e}")))?
                    .ok_or_else(|| AppError::NotFound("未找到收藏博主".to_string()))?;
                Ok(json!({"removed": removed_uid, "kind": "saved"}))
            } else {
                Err(AppError::NotFound(
                    "该博主既不是监控也不是收藏，无法删除".to_string(),
                ))
            }
        },
    )
    .await?;
    Ok(result)
}

/// `blg monitor on|off <uid> [--expected-version N]`
async fn blg_monitor(state: &SharedState, args: &[String]) -> AppResult<Value> {
    let (args, expected_version) = extract_expected_version(args)?;
    let action = required(&args, 2, "blg monitor 需要 on|off")?;
    let uid = required(&args, 3, "blg monitor 需要 uid")?;
    let running = match action {
        "on" => true,
        "off" => false,
        _ => {
            return Err(AppError::BadRequest(
                "blg monitor 第二个参数必须是 on|off".to_string(),
            ))
        }
    };
    // 解析 UID → ID 用于乐观锁（find_by_uid 内部缓存，开销小）。
    let blogger = state
        .business
        .blogger_service
        .find_by_uid(uid)
        .await
        .map_err(|e| AppError::Internal(format!("查询博主失败: {e}")))?
        .ok_or_else(|| AppError::NotFound("未找到该博主".to_string()))?;
    let id_str = blogger.id.to_string();
    let ctx = ctl_audit_ctx(
        &format!("blg monitor {action} {uid}"),
        OperationTarget::Blogger,
        Some(id_str.clone()),
        if running { "monitor_on" } else { "monitor_off" },
        expected_version,
    );
    let svc = state.business.blogger_service.clone();
    let result = with_guarded_op(
        state,
        ctx,
        OperationTarget::Blogger,
        Some(&id_str),
        expected_version,
        move |_guard| async move {
            let toggle = svc
                .set_monitor_running(uid, running)
                .await
                .map_err(|e| AppError::Internal(format!("切换监控状态失败: {e}")))?;
            let message = match toggle {
                crate::services::blogger::MonitorToggle::NotFound => {
                    return Err(AppError::NotFound("未找到该博主".to_string()))
                }
                crate::services::blogger::MonitorToggle::AlreadyInState => {
                    if running {
                        "博主监控已是运行中状态"
                    } else {
                        "博主监控已是停止状态"
                    }
                }
                crate::services::blogger::MonitorToggle::Updated => {
                    if running {
                        "博主监控已启动"
                    } else {
                        "博主监控已停止"
                    }
                }
            };
            Ok(json!({
                "monitor_running": running,
                "message": message,
            }))
        },
    )
    .await?;
    Ok(result)
}

// --- 主题 4：系统（sys 命令） ---

async fn sys_command(state: &SharedState, args: &[String]) -> AppResult<Value> {
    let Some(sub) = args.get(1).map(String::as_str) else {
        return Err(AppError::BadRequest(
            "用法：sys status|config|aria2-restart|ffmpeg-test|logs|refresh".to_string(),
        ));
    };
    match sub {
        "status" => Ok(sys_status_value(state).await),
        "config" => Ok(sys_config_value(state)),
        "aria2-restart" => sys_aria2_restart(state).await,
        "ffmpeg-test" => sys_ffmpeg_test(state).await,
        "logs" => Ok(sys_logs_value(state)),
        "refresh" => sys_refresh(state, args).await,
        _ => Err(AppError::BadRequest(format!(
            "未知 sys 子命令 `{sub}`；用法：sys status|config|aria2-restart|ffmpeg-test|logs|refresh"
        ))),
    }
}

async fn sys_status_value(state: &SharedState) -> Value {
    let mode = state.bili.security.current().mode;
    let pairing = state.bili.auth.pairing_state().await;
    let sessions = state
        .bili
        .auth
        .list_sessions()
        .await
        .map(|s| s.len())
        .unwrap_or(0);
    let uptime_secs = state.infra.started_at.elapsed().as_secs();
    let aria2_status = state.media.aria2.status().await;
    let ai_enabled = state.infra.ai_skill_enabled.load(Ordering::Relaxed);
    json!({
        "mode": mode,
        "pairing": pairing,
        "sessions": sessions,
        "ai_skill_enabled": ai_enabled,
        "uptime_secs": uptime_secs,
        "aria2_status": aria2_status,
    })
}

fn sys_config_value(state: &SharedState) -> Value {
    let mut value =
        serde_json::to_value(state.bili.security.current()).unwrap_or_else(|_| json!({}));
    if let Some(obj) = value.as_object_mut() {
        obj.insert(
            "effective_geo_db".to_string(),
            serde_json::to_value(state.bili.security.effective_geo_db()).unwrap_or(Value::Null),
        );
        obj.insert(
            "ai_skill_enabled".to_string(),
            json!(state.infra.ai_skill_enabled.load(Ordering::Relaxed)),
        );
    }
    value
}

async fn sys_aria2_restart(state: &SharedState) -> AppResult<Value> {
    audit_only_op(
        state,
        "sys aria2-restart",
        OperationTarget::Settings,
        None,
        "aria2_restart",
        || async move {
            let settings = state.infra.settings_service.current();
            // init 内部已先 stop 再 start，等价于 restart
            state
                .media
                .aria2
                .init(&settings)
                .await
                .map_err(|e| AppError::Internal(format!("重启 Aria2 失败: {e}")))?;
            let status = state.media.aria2.status().await;
            Ok(json!({
                "restarted": true,
                "aria2_status": status,
            }))
        },
    )
    .await
}

async fn sys_ffmpeg_test(state: &SharedState) -> AppResult<Value> {
    // 与 Web /api/settings/ffmpeg-test 保持一致：读取设置里的 ffmpeg.mode + custom_path，
    // 而不是固定 "auto" 探测——否则设置页配好自定义 FFmpeg 后 ctl 仍报"未找到 FFmpeg"。
    let settings = state.infra.settings_service.current();
    let mode = settings.ffmpeg.mode.clone();
    let custom_path = (!settings.ffmpeg.custom_path.trim().is_empty())
        .then(|| settings.ffmpeg.custom_path.clone());
    let (path, source) = state
        .media
        .video_processor
        .detect_ffmpeg(&mode, custom_path.as_deref())
        .await;
    let Some(path) = path else {
        return Ok(json!({
            "available": false,
            "path": null,
            "source": source,
            "version": null,
            "message": "未找到 FFmpeg",
        }));
    };
    let (ok, version) = state.media.video_processor.check_ffmpeg(&path).await;
    Ok(json!({
        "available": ok,
        "path": path.file_name().and_then(|name| name.to_str()).unwrap_or("ffmpeg"),
        "source": source,
        "version": version,
    }))
}

fn sys_logs_value(state: &SharedState) -> Value {
    json!({
        "logs_dir": state.infra.paths.data_dir.join("logs"),
        // 与 tracing_appender 实际产物一致：filename_prefix("app") + 日期 + suffix("log")
        // 生成 app.YYYY-MM-DD.log（不是 app.log.YYYY-MM-DD）。
        "hint": "日志按天滚动，文件名形如 app.YYYY-MM-DD.log",
    })
}

async fn sys_refresh(state: &SharedState, args: &[String]) -> AppResult<Value> {
    let scope = required(args, 2, "sys refresh 需要 scope（board|blogger|video）")?;
    match scope {
        "board" => {
            let ctx = ctl_audit_ctx(
                "sys refresh board",
                OperationTarget::Blogger,
                None,
                "refresh_board",
                None,
            );
            let refresh = state.business.refresh_service.clone();
            let outcome = with_guarded_op(
                state,
                ctx,
                OperationTarget::Blogger,
                None,
                None,
                move |_| async move {
                    let n = refresh
                        .trigger_l1()
                        .await
                        .map_err(|e| AppError::Internal(format!("触发 board 刷新失败: {e}")))?;
                    Ok(json!({"scope": "board", "triggered": n}))
                },
            )
            .await?;
            Ok(outcome)
        }
        "blogger" => {
            let ctx = ctl_audit_ctx(
                "sys refresh blogger",
                OperationTarget::Blogger,
                None,
                "refresh_blogger",
                None,
            );
            let refresh = state.business.refresh_service.clone();
            let outcome = with_guarded_op(
                state,
                ctx,
                OperationTarget::Blogger,
                None,
                None,
                move |_| async move {
                    let n = refresh
                        .trigger_l2()
                        .await
                        .map_err(|e| AppError::Internal(format!("触发 blogger 刷新失败: {e}")))?;
                    Ok(json!({"scope": "blogger", "triggered": n}))
                },
            )
            .await?;
            Ok(outcome)
        }
        "video" => {
            let bvid = required(args, 3, "sys refresh video 需要 bvid")?;
            // 单视频刷新走 B 站 API，需登录前置
            require_bili_login(state).await?;
            let ctx = ctl_audit_ctx(
                &format!("sys refresh video {bvid}"),
                OperationTarget::Task,
                Some(bvid.to_string()),
                "refresh_video",
                None,
            );
            let refresh = state.business.refresh_service.clone();
            let bvid_owned = bvid.to_string();
            let outcome = with_guarded_op(
                state,
                ctx,
                OperationTarget::Task,
                Some(bvid),
                None,
                move |_| async move {
                    // 直接透传 AppError（查不到记录时返回 NOT_FOUND，与 Web 一致）
                    refresh.trigger_video(&bvid_owned).await?;
                    Ok(json!({"scope": "video", "bvid": bvid_owned, "triggered": 1}))
                },
            )
            .await?;
            Ok(outcome)
        }
        "verify" => Err(AppError::BadRequest(
            "verify 是独立 worker，无法通过 ctl 触发".to_string(),
        )),
        _ => Err(AppError::BadRequest(format!(
            "未知 refresh scope `{scope}`；用法：sys refresh board|blogger|video <bvid>"
        ))),
    }
}

// --- 主题 5：凭证（cred 命令） ---

async fn cred_command(state: &SharedState, args: &[String]) -> AppResult<Value> {
    let Some(sub) = args.get(1).map(String::as_str) else {
        return Err(AppError::BadRequest(
            "用法：cred qrcode|qrcode-poll|status".to_string(),
        ));
    };
    match sub {
        "qrcode" => {
            // 生成扫码二维码是凭证操作链起点，审计走 record_silent 不广播
            let ctx = ctl_audit_ctx(
                "cred qrcode",
                OperationTarget::Cookie,
                None,
                "qrcode_generate",
                None,
            );
            let bili_api = state.bili.bili_api.clone();
            let result: AppResult<serde_json::Value> = async {
                let qrcode = bili_api
                    .get_qrcode_url()
                    .await
                    .map_err(|e| AppError::Internal(format!("获取二维码失败: {e}")))?;
                Ok(json!({
                    "qrcode_url": qrcode.url,
                    "qrcode_key": qrcode.qrcode_key,
                }))
            }
            .await;
            match result {
                Ok(value) => {
                    state
                        .infra
                        .audit_log
                        .record_silent(&ctx, OperationOutcome::Success, None, None, None)
                        .await;
                    Ok(value)
                }
                Err(error) => {
                    let code = error_code_for(&error);
                    state
                        .infra
                        .audit_log
                        .record_silent(
                            &ctx,
                            OperationOutcome::Error,
                            None,
                            Some(code),
                            Some(json!({"error": error.to_string()})),
                        )
                        .await;
                    Err(error)
                }
            }
        }
        "qrcode-poll" => {
            let key = required(args, 2, "cred qrcode-poll 需要 qrcode_key")?;
            // 扫码登录是敏感操作（Cookie 落盘）：审计走 record_silent，不广播事件。
            let ctx = ctl_audit_ctx(
                &format!("cred qrcode-poll {key}"),
                OperationTarget::Cookie,
                None,
                "qrcode_poll",
                None,
            );
            let bili_api = state.bili.bili_api.clone();
            let settings = state.infra.settings_service.clone();
            let result: AppResult<serde_json::Value> = async {
                let poll = bili_api
                    .check_qrcode_status(key)
                    .await
                    .map_err(|e| AppError::Internal(format!("轮询扫码状态失败: {e}")))?;
                // code=0 成功时落盘 Cookie。
                if poll.code == 0 {
                    if let Some(cookies) = poll.cookies.as_deref().filter(|c| !c.trim().is_empty())
                    {
                        let nav = bili_api.get_nav_info(cookies).await.map_err(|e| {
                            AppError::Unauthorized(format!("扫码凭证校验失败: {e}"))
                        })?;
                        if !nav.is_login {
                            return Err(AppError::Unauthorized(
                                "扫码凭证未通过 B 站登录校验".to_string(),
                            ));
                        }
                        settings.save_cookie_header(cookies).await?;
                        bili_api.invalidate_session_caches().await;
                    }
                }
                Ok(json!({
                    "code": poll.code,
                    "message": poll.message,
                    "logged_in": poll.code == 0,
                }))
            }
            .await;
            match result {
                Ok(value) => {
                    state
                        .infra
                        .audit_log
                        .record_silent(&ctx, OperationOutcome::Success, None, None, None)
                        .await;
                    Ok(value)
                }
                Err(error) => {
                    let code = error_code_for(&error);
                    state
                        .infra
                        .audit_log
                        .record_silent(
                            &ctx,
                            OperationOutcome::Error,
                            None,
                            Some(code),
                            Some(json!({"error": error.to_string()})),
                        )
                        .await;
                    Err(error)
                }
            }
        }
        "status" => cred_status_value(state).await,
        _ => Err(AppError::BadRequest(format!(
            "未知 cred 子命令 `{sub}`；用法：cred qrcode|qrcode-poll|status"
        ))),
    }
}

async fn cred_status_value(state: &SharedState) -> AppResult<Value> {
    let cookies = state
        .infra
        .settings_service
        .cookie_header()
        .await
        .unwrap_or_default();
    let logged_in = Credential::from_cookie_header(&cookies).is_logged_in();
    if !logged_in {
        return Ok(json!({
            "logged_in": false,
            "uid": null,
            "hint": "未登录，使用 cred qrcode 扫码登录",
        }));
    }
    let nav = state.bili.bili_api.get_nav_info(&cookies).await;
    let (uid, is_login) = match nav {
        Ok(n) => (Some(n.mid), n.is_login),
        Err(_) => (None, false),
    };
    Ok(json!({
        "logged_in": is_login,
        "uid": uid,
    }))
}

// --- 旧名 alias 实现（保留 access/mode/geo/trust/revoke；简化 status） ---

async fn legacy_status(state: &SharedState) -> Value {
    json!({
        "mode": state.bili.security.current().mode,
        "pairing": state.bili.auth.pairing_state().await,
        "sessions": state.bili.auth.list_sessions().await.map(|s| s.len()).unwrap_or(0),
        "ai_skill_enabled": state.infra.ai_skill_enabled.load(Ordering::Relaxed),
    })
}

async fn revoke_command(state: &SharedState, args: &[String]) -> AppResult<Value> {
    let id = required(args, 1, "revoke 需要 session-id 或 all")?;
    let ctx = ctl_audit_ctx(
        &format!("revoke {id}"),
        OperationTarget::Session,
        Some(id.to_string()),
        "revoke",
        None,
    );
    let auth = state.bili.auth.clone();
    let ws = state.infra.ws.clone();
    let id_owned = id.to_string();
    let revoked = with_guarded_op(
        state,
        ctx,
        OperationTarget::Session,
        Some(id),
        None,
        move |_guard| async move {
            let revoked = auth.revoke(&id_owned).await?;
            ws.disconnect_session(&id_owned)
                .await
                .map_err(|error| AppError::Internal(error.to_string()))?;
            Ok::<_, AppError>(json!({"revoked": revoked}))
        },
    )
    .await?;
    Ok(revoked)
}

async fn access_command(state: &SharedState, args: &[String]) -> AppResult<Value> {
    // list 仅查询，不写入审计日志
    if args.get(1).map(String::as_str) == Some("list") {
        return Ok(serde_json::to_value(
            state.bili.security.current().access_rules,
        )?);
    }
    let cmd_str = args.join(" ");
    let args_owned: Vec<String> = args.to_vec();
    audit_only_op(
        state,
        &cmd_str,
        OperationTarget::Settings,
        None,
        "access",
        || async move {
            match args_owned.get(1).map(String::as_str) {
                Some("default") => {
                    let action = parse_action(required(&args_owned, 2, "缺少 allow/deny")?)?;
                    state
                        .bili
                        .security
                        .update(|config| {
                            config.access_default = action;
                            Ok(())
                        })
                        .await?;
                    Ok(json!({"default": action}))
                }
                Some("allow" | "deny") => {
                    let action = parse_action(args_owned[1].as_str())?;
                    let network = parse_network(required(&args_owned, 2, "缺少 IP 或 CIDR")?)?;
                    let minutes = parse_minutes(&args_owned)?;
                    let id = state
                        .bili
                        .security
                        .add_rule(action, network, minutes)
                        .await?;
                    Ok(json!({"rule_id": id}))
                }
                Some("remove") => {
                    let id = required(&args_owned, 2, "缺少规则 ID")?;
                    Ok(json!({"removed": state.bili.security.remove_rule(id).await?}))
                }
                _ => Err(AppError::BadRequest(
                    "用法：access default|allow|deny|remove|list".to_string(),
                )),
            }
        },
    )
    .await
}

async fn mode_command(state: &SharedState, args: &[String]) -> AppResult<Value> {
    let Some(mode) = args.get(1).map(String::as_str) else {
        return Ok(json!({"mode": state.bili.security.current().mode}));
    };
    let cmd_str = args.join(" ");
    let args_owned: Vec<String> = args.to_vec();
    let mode_owned = mode.to_string();
    audit_only_op(
        state,
        &cmd_str,
        OperationTarget::Settings,
        None,
        "mode",
        || async move {
            state
                .bili
                .security
                .update(|config| {
                    match mode_owned.as_str() {
                        "local" => {
                            config.mode = AccessMode::Local;
                            config.proxy_domain = None;
                        }
                        "lan" => {
                            config.mode = AccessMode::Lan;
                            config.proxy_domain = None;
                        }
                        "proxy" => {
                            let domain =
                                required(&args_owned, 2, "proxy 模式缺少域名")?.to_ascii_lowercase();
                            config.mode = AccessMode::Proxy;
                            config.proxy_domain = Some(domain);
                            config.geo_cn = true;
                        }
                        _ => {
                            return Err(AppError::BadRequest(
                                "模式必须是 local/lan/proxy".to_string(),
                            ))
                        }
                    }
                    Ok(())
                })
                .await?;
            // lan 模式下默认放行 + 无任何规则时，局域网内任意 IP 均可达（HTTP 明文），
            // 返回警告提醒先配置允许网段或将默认策略改为 deny。
            let current = state.bili.security.current();
            if mode_owned == "lan"
                && current.access_default == AccessAction::Allow
                && current.access_rules.is_empty()
            {
                return Ok(json!({
                    "mode": mode_owned,
                    "restart_required": true,
                    "warning": "lan 模式当前为默认放行且无任何访问规则，建议先执行：access default deny 并 access allow <可信网段>",
                }));
            }
            Ok(json!({"mode": mode_owned, "restart_required": true}))
        },
    )
    .await
}

async fn geo_command(state: &SharedState, args: &[String]) -> AppResult<Value> {
    let cmd_str = args.join(" ");
    let args_owned: Vec<String> = args.to_vec();
    audit_only_op(
        state,
        &cmd_str,
        OperationTarget::Settings,
        None,
        "geo",
        || async move {
            match (
                args_owned.get(1).map(String::as_str),
                args_owned.get(2).map(String::as_str),
            ) {
                (Some("cn"), Some(value @ ("on" | "off"))) => {
                    state
                        .bili
                        .security
                        .update(|config| {
                            config.geo_cn = value == "on";
                            Ok(())
                        })
                        .await?;
                    Ok(json!({"geo_cn": value == "on"}))
                }
                (Some("db"), Some("remove")) => {
                    state
                        .bili
                        .security
                        .update(|config| {
                            config.geo_db = None;
                            Ok(())
                        })
                        .await?;
                    Ok(json!({
                        "geo_db": null,
                        "effective_geo_db": state.bili.security.effective_geo_db(),
                    }))
                }
                (Some("db"), Some(path)) => {
                    let path = PathBuf::from(path);
                    if !path.is_absolute() || !path.is_file() {
                        return Err(AppError::BadRequest(
                            "GeoIP 数据库必须是存在的绝对文件路径".to_string(),
                        ));
                    }
                    state
                        .bili
                        .security
                        .update(|config| {
                            config.geo_db = Some(path.clone());
                            Ok(())
                        })
                        .await?;
                    Ok(json!({"geo_db_updated": true, "effective_geo_db": path}))
                }
                _ => Err(AppError::BadRequest(
                    "用法：geo cn on|off 或 geo db <absolute-path>|remove".to_string(),
                )),
            }
        },
    )
    .await
}

async fn trust_command(state: &SharedState, args: &[String]) -> AppResult<Value> {
    let cmd_str = args.join(" ");
    let args_owned: Vec<String> = args.to_vec();
    audit_only_op(
        state,
        &cmd_str,
        OperationTarget::Settings,
        None,
        "trust",
        || async move {
            match (
                args_owned.get(1).map(String::as_str),
                args_owned.get(2).map(String::as_str),
            ) {
                (Some("aria2"), Some("remove")) => {
                    state
                        .bili
                        .security
                        .update(|config| {
                            config.trusted_aria2_endpoint = None;
                            Ok(())
                        })
                        .await?;
                    Ok(json!({"aria2_trust": null}))
                }
                (Some("aria2"), Some(endpoint)) => {
                    let parsed = url::Url::parse(endpoint).map_err(|_| {
                        AppError::BadRequest("aria2 endpoint 必须是 URL".to_string())
                    })?;
                    if !matches!(parsed.scheme(), "http" | "https") {
                        return Err(AppError::BadRequest(
                            "aria2 endpoint 仅支持 HTTP(S)".to_string(),
                        ));
                    }
                    state
                        .bili
                        .security
                        .update(|config| {
                            config.trusted_aria2_endpoint = Some(endpoint.to_string());
                            Ok(())
                        })
                        .await?;
                    Ok(json!({"aria2_trust": endpoint}))
                }
                (Some("ffmpeg"), Some("remove")) => {
                    state
                        .bili
                        .security
                        .update(|config| {
                            config.trusted_ffmpeg_paths.clear();
                            Ok(())
                        })
                        .await?;
                    Ok(json!({"ffmpeg_trust": []}))
                }
                (Some("ffmpeg"), Some(path)) => {
                    let path = PathBuf::from(path);
                    if !path.is_absolute() || !path.is_file() {
                        return Err(AppError::BadRequest(
                            "FFmpeg 必须是存在的绝对文件路径".to_string(),
                        ));
                    }
                    state
                        .bili
                        .security
                        .update(|config| {
                            if !config.trusted_ffmpeg_paths.contains(&path) {
                                config.trusted_ffmpeg_paths.push(path);
                            }
                            Ok(())
                        })
                        .await?;
                    Ok(json!({"ffmpeg_trust_updated": true}))
                }
                _ => Err(AppError::BadRequest(
                    "用法：trust aria2|ffmpeg <value|remove>".to_string(),
                )),
            }
        },
    )
    .await
}

// --- 主题 6：审计与事件（audit/events 命令） ---

/// `audit list [--source <s>] [--since <1h|24h|7d>] [--limit N]`
/// `audit by-target <task|blogger|cookie|session> <id>`
async fn audit_command(state: &SharedState, args: &[String]) -> AppResult<Value> {
    let Some(sub) = args.get(1).map(String::as_str) else {
        return Err(AppError::BadRequest(
            "用法：audit list|by-target".to_string(),
        ));
    };
    match sub {
        "list" => {
            let source = parse_source_flag(args)?;
            let since = parse_since_flag(args)?;
            let limit = parse_limit_flag(args, 100)?;
            let rows = state
                .infra
                .audit_log
                .list(source, since.as_deref(), limit)
                .await?;
            Ok(json!({
                "count": rows.len(),
                "logs": rows.iter().map(|r| r.to_api()).collect::<Vec<_>>(),
            }))
        }
        "by-target" => {
            let target_str = required(args, 2, "audit by-target 需要 target_type")?;
            let target_id = required(args, 3, "audit by-target 需要 target_id")?;
            let target = parse_target_type(target_str)?;
            let limit = parse_limit_flag(args, 100)?;
            let rows = state
                .infra
                .audit_log
                .by_target(target, target_id, limit)
                .await?;
            Ok(json!({
                "count": rows.len(),
                "logs": rows.iter().map(|r| r.to_api()).collect::<Vec<_>>(),
            }))
        }
        _ => Err(AppError::BadRequest(format!(
            "未知 audit 子命令 `{sub}`；用法：audit list|by-target"
        ))),
    }
}

/// `events [--watch] [--limit N]`
/// - 无 `--watch`：返回最近 N 条审计事件（非流式，IPC 友好）
/// - `--watch`：流式订阅（仅 IPC 连接支持，stdin loop 走 println）
async fn events_command(state: &SharedState, args: &[String]) -> AppResult<Value> {
    // 只接受 --watch/--limit：未知 flag 报错而非静默忽略，避免拼错参数后
    // 拿到看似成功的结果。
    if let Some(unknown) = args
        .iter()
        .skip(1)
        .find(|arg| arg.starts_with("--") && !matches!(arg.as_str(), "--watch" | "--limit"))
    {
        return Err(AppError::BadRequest(format!(
            "events 不支持参数 `{unknown}`；用法：events [--watch] [--limit N]"
        )));
    }
    let watch = args.iter().any(|a| a == "--watch");
    let limit = parse_limit_flag(args, 50)?;
    if watch {
        // 流式模式由 handle_streaming 拦截，execute 不应被调到这里
        return Err(AppError::BadRequest(
            "events --watch 需通过 IPC 流式连接调用，stdin/TUI 暂不支持".to_string(),
        ));
    }
    let rows = state.infra.audit_log.list(None, None, limit).await?;
    Ok(json!({
        "count": rows.len(),
        "events": rows.iter().map(|r| r.to_api()).collect::<Vec<_>>(),
    }))
}

/// 判断是否为流式命令（events --watch），供 handle_stream 路由到流式处理。
fn is_streaming_command(args: &[String]) -> bool {
    args.first().is_some_and(|c| c == "events") && args.iter().any(|a| a == "--watch")
}

/// 流式处理 `events --watch`：订阅 AuditEventSender，逐条写 JSON Lines 到客户端。
/// 30 秒无事件时发空行保活；客户端断开时退出。
async fn handle_streaming<S>(mut stream: S, state: SharedState) -> AppResult<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    use tokio::sync::broadcast::error::RecvError;
    let mut rx = state.infra.audit_log.subscribe();
    // 先发送 hello，让客户端确认流已建立
    let hello = json!({
        "type": "stream_opened",
        "message": "audit event stream connected",
    });
    stream.write_all(format!("{}\n", hello).as_bytes()).await?;
    stream.flush().await?;
    loop {
        match tokio::time::timeout(Duration::from_secs(30), rx.recv()).await {
            Ok(Ok(event)) => {
                let line = serde_json::to_string(&event).unwrap_or_default() + "\n";
                if stream.write_all(line.as_bytes()).await.is_err() {
                    break; // 客户端已断开
                }
                if stream.flush().await.is_err() {
                    break;
                }
            }
            Ok(Err(RecvError::Closed)) => break,
            Ok(Err(RecvError::Lagged(_))) => {
                // 客户端跟不上节奏导致事件丢失，发送一条 warning 说明情况
                let warn = json!({"type": "lagged", "message": "部分事件被丢弃"});
                let _ = stream.write_all(format!("{}\n", warn).as_bytes()).await;
                let _ = stream.flush().await;
            }
            Err(_) => {
                // 30 秒超时：发送空行保活，供客户端判断连接仍存活
                if stream.write_all(b"\n").await.is_err() {
                    break;
                }
                let _ = stream.flush().await;
            }
        }
    }
    let _ = stream.shutdown().await;
    Ok(())
}

// --- 辅助函数 ---

/// 从参数中提取 `--expected-version N`，返回 (剩余参数, expected_version)。
/// 调用方据此决定是否启用乐观锁。
/// flag 存在但值缺失/非法时报 BAD_REQUEST 而非静默忽略——否则拼错的乐观锁
/// 参数会让写操作悄悄退化成"最后写入胜出"，破坏调用方的并发假设。
fn extract_expected_version(args: &[String]) -> AppResult<(Vec<String>, Option<i32>)> {
    let Some(pos) = args.iter().position(|a| a == "--expected-version") else {
        return Ok((args.to_vec(), None));
    };
    let mut remaining: Vec<String> = Vec::with_capacity(args.len().saturating_sub(2));
    remaining.extend_from_slice(&args[..pos]);
    let value = args.get(pos + 1).ok_or_else(|| {
        AppError::BadRequest("--expected-version 缺少数值（如 --expected-version 42）".to_string())
    })?;
    let parsed = value.parse::<i32>().map_err(|_| {
        AppError::BadRequest(format!("--expected-version 非法值 `{value}`，需要整数"))
    })?;
    remaining.extend_from_slice(&args[pos + 2..]);
    Ok((remaining, Some(parsed)))
}

/// 解析 `--source <src>` flag 为 OperationSource。非法值报 BAD_REQUEST 而非静默忽略，
/// 避免拼错参数后静默得到全量审计结果。
fn parse_source_flag(args: &[String]) -> AppResult<Option<OperationSource>> {
    let Some(val) = parse_flag_value(args, "--source") else {
        return Ok(None);
    };
    val.parse::<OperationSource>()
        .map(Some)
        .map_err(|_| AppError::BadRequest(format!("--source 非法值 `{val}`")))
}

/// 解析 `--since <duration>`：仅接受 Nd/Nh/Nm（如 1h / 24h / 7d）。非法值报 BAD_REQUEST，
/// 与 --source 一致，避免静默忽略过滤条件。
fn parse_since_flag(args: &[String]) -> AppResult<Option<String>> {
    let Some(val) = parse_flag_value(args, "--since") else {
        return Ok(None);
    };
    validate_since_duration(val)?;
    Ok(Some(val.to_string()))
}

fn validate_since_duration(value: &str) -> AppResult<()> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    // strip_suffix 按字符边界切分：多字节输入（如 `1小时`）不会像 split_at 那样 panic。
    let valid = ["h", "d", "m"].iter().any(|unit| {
        trimmed
            .strip_suffix(unit)
            .is_some_and(|num| num.parse::<i64>().is_ok())
    });
    if valid {
        Ok(())
    } else {
        Err(AppError::BadRequest(format!(
            "--since 非法值 `{value}`，支持格式如 1h / 24h / 7d"
        )))
    }
}

/// 解析 `--target-type` 字符串为 OperationTarget。
fn parse_target_type(s: &str) -> AppResult<OperationTarget> {
    match s {
        "task" => Ok(OperationTarget::Task),
        "blogger" => Ok(OperationTarget::Blogger),
        "history" => Ok(OperationTarget::History),
        "settings" => Ok(OperationTarget::Settings),
        "cookie" => Ok(OperationTarget::Cookie),
        "session" => Ok(OperationTarget::Session),
        other => Err(AppError::BadRequest(format!(
            "target_type 必须是 task|blogger|history|settings|cookie|session，收到 `{other}`"
        ))),
    }
}

/// 取 `--flag value` 的 value 部分。
fn parse_flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    let pos = args.iter().position(|a| a == flag)?;
    args.get(pos + 1).map(String::as_str)
}

/// 取 `--limit N`（默认值由调用方给）。值缺失或非正整数时报 BAD_REQUEST，
/// 不静默回退默认值——拼错 limit 会悄悄拿到全量/空结果，掩盖调用方错误。
fn parse_limit_flag(args: &[String], default: u64) -> AppResult<u64> {
    let Some(value) = parse_flag_value(args, "--limit") else {
        return Ok(default);
    };
    let parsed = value
        .parse::<u64>()
        .map_err(|_| AppError::BadRequest(format!("--limit 非法值 `{value}`，需要正整数")))?;
    if parsed == 0 {
        return Err(AppError::BadRequest("--limit 必须大于 0".to_string()));
    }
    Ok(parsed)
}

/// 为本机 ctl 调用构造审计上下文（复用 AuditContext::for_ctl，简化调用）。
/// request_id 自动生成 UUID，便于跨服务追踪同一次调用。
fn ctl_audit_ctx(
    command: &str,
    target: OperationTarget,
    target_id: Option<String>,
    action: &str,
    expected_version: Option<i32>,
) -> AuditContext {
    let request_id = uuid::Uuid::new_v4().to_string();
    AuditContext::for_ctl(
        command,
        target,
        target_id,
        action,
        expected_version,
        &request_id,
    )
}

/// 写操作失败时统一记审计 + 回滚 guard（如果持有）。
/// 错误码：CONFLICT / AI_SKILL_DISABLED / BILI_NOT_LOGGED_IN / INTERNAL / BAD_REQUEST。
fn error_code_for(error: &AppError) -> &'static str {
    match error {
        AppError::Conflict(_) => "CONFLICT",
        AppError::Unauthorized(_) => "UNAUTHORIZED",
        AppError::AiSkillDisabled(_) => "AI_SKILL_DISABLED",
        AppError::BiliNotLoggedIn(_) => "BILI_NOT_LOGGED_IN",
        AppError::NotFound(_) => "NOT_FOUND",
        AppError::BadRequest(_) => "BAD_REQUEST",
        AppError::RiskControl(_) => "RISK_CONTROL",
        _ => "INTERNAL",
    }
}

/// 把 ConflictGuard 包到写操作里：成功 commit + 审计，失败 rollback + 审计。
/// `op` 收到 `Option<&ConflictGuard>`（None=未启用乐观锁），返回操作结果。
async fn with_guarded_op<T, F, Fut>(
    state: &SharedState,
    ctx: AuditContext,
    target: OperationTarget,
    target_id: Option<&str>,
    expected_version: Option<i32>,
    op: F,
) -> AppResult<T>
where
    F: FnOnce(Option<&ConflictGuard>) -> Fut,
    Fut: std::future::Future<Output = AppResult<T>>,
{
    // 1. 乐观锁校验（仅在 expected_version 和 target_id 都存在时启用）
    let guard: Option<ConflictGuard> = if let (Some(ev), Some(id)) = (expected_version, target_id) {
        match state
            .infra
            .conflict_guard
            .check_and_bump(target, id, Some(ev))
            .await
        {
            Ok(g) => Some(g),
            Err(error) => {
                // 冲突：审计 + 返回错误
                state
                    .infra
                    .audit_log
                    .record(
                        &ctx,
                        OperationOutcome::Conflict,
                        None,
                        Some(error_code_for(&error)),
                        None,
                    )
                    .await;
                return Err(error);
            }
        }
    } else {
        None
    };

    // 2. 执行操作
    let result = op(guard.as_ref()).await;

    match result {
        Ok(value) => {
            // 成功：commit guard + 审计
            if let Some(g) = guard {
                let new_v = g.new_version();
                g.commit();
                state
                    .infra
                    .audit_log
                    .record(&ctx, OperationOutcome::Success, Some(new_v), None, None)
                    .await;
            } else {
                state
                    .infra
                    .audit_log
                    .record(&ctx, OperationOutcome::Success, None, None, None)
                    .await;
            }
            Ok(value)
        }
        Err(error) => {
            let code = error_code_for(&error);
            // 失败：rollback guard + 审计
            if let Some(g) = guard {
                g.rollback().await.ok();
            }
            state
                .infra
                .audit_log
                .record(
                    &ctx,
                    OperationOutcome::Error,
                    None,
                    Some(code),
                    Some(json!({"error": error.to_string()})),
                )
                .await;
            Err(error)
        }
    }
}

/// 仅审计（不走乐观锁）：成功记 Success，失败记 Error。
/// 用于 settings/access/mode/geo/trust/ai 等不涉及 version 的写操作。
/// `op` 不接收 guard 参数（这些操作永远不启用乐观锁）。
async fn audit_only_op<T, F, Fut>(
    state: &SharedState,
    command: &str,
    target: OperationTarget,
    target_id: Option<String>,
    action: &str,
    op: F,
) -> AppResult<T>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = AppResult<T>>,
{
    let ctx = ctl_audit_ctx(command, target, target_id, action, None);
    let result = op().await;
    match result {
        Ok(value) => {
            state
                .infra
                .audit_log
                .record(&ctx, OperationOutcome::Success, None, None, None)
                .await;
            Ok(value)
        }
        Err(error) => {
            let code = error_code_for(&error);
            state
                .infra
                .audit_log
                .record(
                    &ctx,
                    OperationOutcome::Error,
                    None,
                    Some(code),
                    Some(json!({"error": error.to_string()})),
                )
                .await;
            Err(error)
        }
    }
}

fn required<'a>(args: &'a [String], index: usize, message: &str) -> AppResult<&'a str> {
    args.get(index)
        .map(String::as_str)
        .ok_or_else(|| AppError::BadRequest(message.to_string()))
}

fn parse_action(value: &str) -> AppResult<AccessAction> {
    match value {
        "allow" => Ok(AccessAction::Allow),
        "deny" => Ok(AccessAction::Deny),
        _ => Err(AppError::BadRequest(
            "访问动作必须是 allow/deny".to_string(),
        )),
    }
}

fn parse_network(value: &str) -> AppResult<IpNet> {
    value
        .parse::<IpNet>()
        .or_else(|_| value.parse::<IpAddr>().map(IpNet::from))
        .map_err(|_| AppError::BadRequest("IP 或 CIDR 格式无效".to_string()))
}

fn parse_minutes(args: &[String]) -> AppResult<Option<u64>> {
    let Some(position) = args.iter().position(|value| value == "--minutes") else {
        return Ok(None);
    };
    let value = required(args, position + 1, "--minutes 缺少数值")?;
    value
        .parse::<u64>()
        .map(Some)
        .map_err(|_| AppError::BadRequest("minutes 必须是正整数".to_string()))
}

// 高影响操作（网络暴露、GeoIP 和可信可执行文件）在 AI 模式开启后与人工同等可用。
// AI 模式门控由 `execute_from` 入口统一检查（未开启时整批拒绝），无需再叠加短时授权层。
// （原 `ai assist` 短时授权层已按 2026-08-18 审计整改删除。）

/// `dl pause/resume <task_id|all>`：`all` → None（全局），数字 → Some(id)
fn parse_task_id_or_all(target: &str) -> AppResult<Option<i32>> {
    if target == "all" {
        return Ok(None);
    }
    target
        .parse::<i32>()
        .map(Some)
        .map_err(|_| AppError::BadRequest("task_id 必须是整数或 all".to_string()))
}

/// 把 TaskOutcome 转成 ctl 返回的 JSON 信封。
fn outcome_to_value(outcome: TaskOutcome) -> Value {
    json!({
        "ok": outcome.ok,
        "message": outcome.message,
        "download_id": outcome.download_id,
    })
}

/// 涉及 B 站 API 的命令前置：取 Cookie 并校验已登录，未登录返回结构化错误。
async fn require_bili_login(state: &SharedState) -> AppResult<String> {
    let cookies = state.infra.settings_service.cookie_header().await?;
    if !Credential::from_cookie_header(&cookies).is_logged_in() {
        return Err(AppError::BiliNotLoggedIn(
            "B 站未登录，请先执行 cred qrcode 扫码登录".to_string(),
        ));
    }
    Ok(cookies)
}

pub(crate) fn split_command(line: &str) -> Vec<String> {
    line.split_whitespace().map(str::to_string).collect()
}

/// `>` 前缀专家模式：TUI/stdin 输入 `> dl status` 时剥离 `>` 走扁平命令路径。
/// IPC 客户端 `ctl dl status` 不带 `>`，直接走 `execute()`。
pub(crate) fn strip_expert_prefix(line: &str) -> &str {
    let trimmed = line.trim_start();
    trimmed
        .strip_prefix('>')
        .map(|rest| rest.trim_start())
        .unwrap_or(trimmed)
}

fn help() -> Value {
    // 按 category 分组，保持 COMMAND_REGISTRY 中的声明顺序
    let mut themes: Vec<Value> = Vec::new();
    for category in [
        CommandCategory::Policy,
        CommandCategory::Download,
        CommandCategory::Blogger,
        CommandCategory::System,
        CommandCategory::Credential,
    ] {
        let commands: Vec<Value> = COMMAND_REGISTRY
            .iter()
            .filter(|spec| spec.category == category)
            .map(|spec| {
                json!({
                    "name": spec.name,
                    "desc": spec.desc,
                    "example": spec.example,
                })
            })
            .collect();
        themes.push(json!({
            "name": category.key(),
            "title": category.title(),
            "commands": commands,
        }));
    }
    json!({
        "themes": themes,
        "expert_mode_hint": "TUI 输入 `> <command>` 直接执行扁平命令；IPC 客户端 `ctl <command>` 即扁平命令",
        "ai_mode_note": "未启用 AI Skill 模式时，ctl 仅放行 status/help/quit/ai/pair",
    })
}

pub(crate) fn format_response(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

/// 从 `COMMAND_REGISTRY` 生成 `docs/skill.md` 的完整内容。
///
/// 这是 AI Skill 文档的唯一真实来源：命令清单、示例、错误码均从此函数生成。
/// 单测 `test_skill_doc_in_sync` 断言 `docs/skill.md` 内容 == 此函数输出，
/// 文档漂移时测试失败，强制保持同步。
#[cfg(test)]
fn generate_skill_markdown() -> String {
    let mut out = String::new();
    out.push_str("# 补哩补哩 Skill\n\n");
    out.push_str(
        "> 本文档供本机 AI 调用 `ctl` 命令使用，由 `COMMAND_REGISTRY` 自动生成，请勿手改。\n",
    );
    out.push_str(
        "> 命令清单与代码同步：修改命令后运行 `cargo test test_skill_doc_in_sync` 更新文档。\n\n",
    );

    // 调用方式
    out.push_str("## 调用方式\n\n");
    out.push_str("所有命令通过本机 IPC 调用（无网络端口暴露）：\n\n");
    out.push_str("```\nbulibuli.exe ctl <command> [args...]\n```\n\n");
    out.push_str("返回 JSON 信封：\n");
    out.push_str("- 成功：`{\"ok\": true, \"data\": {...}}`\n");
    out.push_str("- 失败：`{\"ok\": false, \"error\": \"...\", \"code\": \"...\"}`\n\n");

    // 前置条件
    out.push_str("## 前置条件\n\n");
    out.push_str("1. **AI Skill 模式已启用**：网页 Setup 向导步骤 3 选启用，或运行 `ai on`。");
    out.push_str(
        "未启用时仅 `status` / `help` / `quit` / `ai` / `pair` 可用，其他命令返回 `AI_SKILL_DISABLED`。\n",
    );
    out.push_str(
        "启用后 AI 助手拥有与人工相同的全部操作权限（含 `mode` / `access` / `geo` / `trust` / `pair` 等基础配置命令），无需任何临时授权；所有 ctl 命令都要求服务已在运行。\n",
    );
    out.push_str(
        "2. **B 站已登录**：涉及 B 站 API 的命令（download / blogger / cookies / refresh）",
    );
    out.push_str("需先扫码登录，未登录返回 `BILI_NOT_LOGGED_IN`。\n");
    out.push_str(
        "3. **本机权限**：命名管道仅本机进程可连接（SDDL 限制为系统/管理员/所有者）。\n\n",
    );

    // 命令清单
    out.push_str("## 命令清单\n\n");
    for category in [
        CommandCategory::Policy,
        CommandCategory::Download,
        CommandCategory::Blogger,
        CommandCategory::System,
        CommandCategory::Credential,
    ] {
        out.push_str(&format!("### {}\n\n", category.title()));
        out.push_str("| 命令 | 说明 | 示例 |\n");
        out.push_str("|---|---|---|\n");
        for spec in COMMAND_REGISTRY.iter().filter(|s| s.category == category) {
            out.push_str(&format!(
                "| `{}` | {} | `{}` |\n",
                spec.name, spec.desc, spec.example
            ));
        }
        out.push('\n');
    }

    // 乐观锁
    out.push_str("## 乐观并发控制\n\n");
    out.push_str("状态变更类操作支持乐观锁，调用方传 `--expected-version N`：\n\n");
    out.push_str("```\ndl pause <task_id> --expected-version 42\n```\n\n");
    out.push_str("- 当前 version 匹配时执行 + version += 1，返回 `{\"ok\": true, \"data\": {\"new_version\": 43}}`\n");
    out.push_str("- 不匹配时返回 `CONFLICT` 错误 + 当前状态，调用方可重新读状态后重试\n");
    out.push_str("- 不传 `--expected-version` 时按「最后写入胜出」语义\n\n");

    // 事件流
    out.push_str("## 实时事件订阅\n\n");
    out.push_str("```\nbulibuli.exe ctl events --watch\n```\n\n");
    out.push_str("流式输出 JSON Lines（每行一条审计事件），30 秒无事件发空行保活。\n");
    out.push_str("敏感操作（cookie 保存、pair code 生成）不广播，仅在审计日志中可查。\n\n");

    // 端到端流程
    out.push_str("## 端到端流程示例\n\n");
    out.push_str("### 场景 1：下载某 UP 主最新视频\n\n");
    out.push_str("```\nblg search <name>       # 搜索 UP 主拿 uid\nblg add <uid>           # 添加监控\ndl add <BV1xx411c7mD>   # 入队下载\ndl status               # 轮询队列直到 completed\n```\n\n");
    out.push_str("### 场景 2：扫码登录 B 站\n\n");
    out.push_str("```\ncred qrcode              # 取二维码 URL + qrcode_key\n# 提示用户用 B 站 App 扫码\ncred qrcode-poll <key>   # 每 2 秒轮询，code=0 表示成功\ncred status              # 确认登录状态\n```\n\n");

    // 错误码
    out.push_str("## 错误码\n\n");
    out.push_str("| 错误码 | 含义 |\n");
    out.push_str("|---|---|\n");
    out.push_str("| `AI_SKILL_DISABLED` | AI Skill 模式未启用，先执行 `ai on` |\n");
    out.push_str("| `BILI_NOT_LOGGED_IN` | B 站未登录，先执行 `cred qrcode` 扫码 |\n");
    out.push_str("| `CONFLICT` | 乐观锁冲突，重新读状态后重试 |\n");
    out.push_str("| `BAD_REQUEST` | 参数错误 |\n");
    out.push_str("| `NOT_FOUND` | 资源不存在 |\n");
    out.push_str("| `RISK_CONTROL` | 触发 B 站风控 |\n");
    out.push_str("| `INTERNAL` | 内部错误 |\n");

    out
}

#[cfg(test)]
mod skill_doc_tests {
    use super::generate_skill_markdown;
    use std::path::PathBuf;

    /// 断言 `docs/skill.md` 与 `generate_skill_markdown()` 输出一致。
    /// 修改命令清单后运行 `cargo test test_skill_doc_in_sync` —— 失败时把
    /// `generate_skill_markdown()` 输出覆写回 `docs/skill.md` 即可。
    #[test]
    fn test_skill_doc_in_sync() {
        let expected = generate_skill_markdown();
        let skill_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("docs")
            .join("skill.md");
        let actual = std::fs::read_to_string(&skill_path)
            .map(|content| content.replace("\r\n", "\n"))
            .unwrap_or_else(|error| {
            panic!(
                "读取 {} 失败: {error}\n请运行测试前先创建该文件（内容 = generate_skill_markdown() 输出）",
                skill_path.display()
            )
        });
        assert_eq!(
            expected,
            actual,
            "docs/skill.md 与 COMMAND_REGISTRY 不同步。\n\
             修改命令后请用 generate_skill_markdown() 的输出更新 docs/skill.md。\n\
             路径: {}",
            skill_path.display()
        );
    }

    /// 生成 `docs/skill.md`：仅在文件缺失时写入，用于首次初始化。
    /// 后续命令变更后请手动删除 docs/skill.md 再跑此测试，或直接用
    /// `generate_skill_markdown()` 输出覆写。
    #[test]
    fn write_skill_doc_if_missing() {
        let skill_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("docs")
            .join("skill.md");
        if !skill_path.exists() {
            std::fs::create_dir_all(skill_path.parent().unwrap()).unwrap();
            std::fs::write(&skill_path, generate_skill_markdown()).unwrap();
        }
    }
}

/// IPC 请求读取的空闲超时：客户端连接后长时间不发送（或不关写端）时
/// 断开连接，防止单个慢速/挂起客户端长期占用服务端任务。
const IPC_READ_TIMEOUT: Duration = Duration::from_secs(30);

async fn handle_stream<S>(mut stream: S, state: SharedState) -> AppResult<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut buffer = Vec::new();
    // 读满上限 +1 字节即可判断超长；整体包在空闲超时里，慢速客户端 30s 后被断开。
    let mut limited = (&mut stream).take((MAX_COMMAND_BYTES + 1) as u64);
    let read = limited.read_to_end(&mut buffer);
    match tokio::time::timeout(IPC_READ_TIMEOUT, read).await {
        Ok(result) => result?,
        Err(_) => {
            // 超时也走统一错误信封，客户端能区分"连上了但被服务端断开"和"连接失败"。
            let body = json!({
                "ok": false,
                "error": format!("控制命令读取超时（{} 秒无完整请求）", IPC_READ_TIMEOUT.as_secs()),
                "code": "IPC_TIMEOUT",
            });
            stream.write_all(format_response(&body).as_bytes()).await?;
            stream.shutdown().await?;
            return Ok(());
        }
    };
    if buffer.len() > MAX_COMMAND_BYTES {
        let body = json!({"ok": false, "error": "控制命令过长", "code": "BAD_REQUEST"});
        stream.write_all(format_response(&body).as_bytes()).await?;
        stream.shutdown().await?;
        return Ok(());
    }
    // 解析失败返回统一错误信封（而非直接断开让客户端看到空响应/UTF-8 错误），
    // 与 execute 层的错误信封格式保持一致。
    let args: Vec<String> = match serde_json::from_slice(&buffer) {
        Ok(args) => args,
        Err(error) => {
            let body = json!({
                "ok": false,
                "error": format!("控制命令不是合法的 JSON 参数数组: {error}"),
                "code": "BAD_REQUEST",
            });
            stream.write_all(format_response(&body).as_bytes()).await?;
            stream.shutdown().await?;
            return Ok(());
        }
    };
    // 流式命令（events --watch）：路由到 handle_streaming，保持长连接推 JSON Lines
    // 但先走 AI 模式门控：未启用时拒绝订阅事件流（避免泄露审计信息）
    if is_streaming_command(&args) {
        if !state.infra.ai_skill_enabled.load(Ordering::Relaxed) {
            let body = json!({
                "ok": false,
                "error": "AI Skill 模式未启用，events --watch 不可用",
                "code": "AI_SKILL_DISABLED"
            });
            stream.write_all(format_response(&body).as_bytes()).await?;
            stream.shutdown().await?;
            return Ok(());
        }
        return handle_streaming(stream, state).await;
    }
    let response = execute(&state, &args).await;
    let body = match response {
        Ok(value) => json!({"ok": true, "data": value}),
        Err(error) => {
            let code = error_code_for(&error);
            json!({"ok": false, "error": error.to_string(), "code": code})
        }
    };
    stream.write_all(format_response(&body).as_bytes()).await?;
    stream.shutdown().await?;
    Ok(())
}

/// 探测 Unix socket 是否有活服务在监听：connect 成功即视为活。
/// 连接本身不发送任何数据，服务端会因读超时/EOF 自行清理。
#[cfg(unix)]
async fn unix_socket_is_alive(path: &Path) -> bool {
    matches!(
        tokio::time::timeout(
            Duration::from_secs(2),
            tokio::net::UnixStream::connect(path)
        )
        .await,
        Ok(Ok(_))
    )
}

#[cfg(unix)]
fn ensure_control_socket_parent(parent: &Path, data_dir: &Path) -> Result<(), String> {
    // 控制通道只允许当前用户访问。先归权再校验：create_dir_all 受 umask 影响
    // （常见得到 0755），若先校验会被自己刚创建的目录拒绝，导致 XDG 候选必然
    // 落空。归权对他人属主的目录会失败（保留原权限），随后的属主/模式校验仍会
    // 拒绝，防止攻击者预置 /tmp 目录替换 socket 劫持 ctl 命令。
    let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
    let (Ok(metadata), Ok(data_metadata)) =
        (std::fs::metadata(parent), std::fs::metadata(data_dir))
    else {
        // 元数据不可读时不在这一步下结论，交给后续 bind 报具体错误。
        return Ok(());
    };
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let mode = metadata.permissions().mode();
    if metadata.uid() != data_metadata.uid() || (mode & 0o077) != 0 {
        return Err(format!(
            "目录属主/权限异常（uid={}, mode={mode:o}），拒绝使用",
            metadata.uid()
        ));
    }
    Ok(())
}

#[cfg(unix)]
async fn serve_ipc(state: SharedState) -> AppResult<()> {
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};

    let mut listener = None;
    let mut bind_errors = Vec::new();
    for (index, path) in unix_control_socket_candidates(&state.infra.paths.data_dir)
        .into_iter()
        .enumerate()
    {
        let label = match index {
            0 => "XDG_RUNTIME_DIR/control.sock",
            1 => "data/control.sock",
            _ => "temporary/control.sock",
        };
        if let Some(parent) = path.parent() {
            if let Err(error) = std::fs::create_dir_all(parent) {
                bind_errors.push(format!("{label}: {error}"));
                continue;
            }
            if let Err(reason) = ensure_control_socket_parent(parent, &state.infra.paths.data_dir) {
                bind_errors.push(format!("{label}: {reason}"));
                continue;
            }
        }
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_socket() => {
                // 已有 socket 文件：先探测是否有活服务在监听（connect 成功即活）。
                // 活服务在监听时拒绝抢占——remove+bind 会让本实例劫持 ctl 客户端，
                // 后续 ctl 命令（含 quit/pair 等敏感操作）全部打到错误的服务端。
                // 只有连接失败（ECONNREFUSED 等，上次异常退出的残留文件）才清理。
                if unix_socket_is_alive(&path).await {
                    return Err(AppError::Config(format!(
                        "本机控制通道 {} 已被另一个运行中的 bulibuli 实例占用（{}），拒绝抢占；如确认无其他实例请手动删除该文件后重启",
                        label,
                        path.display()
                    )));
                }
                if let Err(error) = std::fs::remove_file(&path) {
                    bind_errors.push(format!("{label}: {error}"));
                    continue;
                }
            }
            Ok(_) => {
                bind_errors.push(format!("{label}: path exists and is not a Unix socket"));
                continue;
            }
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                bind_errors.push(format!("{label}: {error}"));
                continue;
            }
            Err(_) => {}
        }
        match tokio::net::UnixListener::bind(&path) {
            Ok(value) => {
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
                tracing::info!("本机控制通道已启用：{label}");
                listener = Some(value);
                break;
            }
            Err(error) => bind_errors.push(format!("{label}: {error}")),
        }
    }
    let listener = listener.ok_or_else(|| {
        AppError::Config(format!(
            "无法创建本机 Unix 控制通道；已尝试 XDG_RUNTIME_DIR、数据目录和临时目录：{}",
            bind_errors.join("; ")
        ))
    })?;
    loop {
        let (stream, _) = listener.accept().await?;
        let client_state = state.clone();
        tokio::spawn(async move {
            if let Err(error) = handle_stream(stream, client_state).await {
                tracing::warn!(%error, "处理本机控制请求失败");
            }
        });
    }
}

#[cfg(windows)]
async fn serve_ipc(state: SharedState) -> AppResult<()> {
    use tokio::net::windows::named_pipe::ServerOptions;
    loop {
        let server = {
            let (mut attributes, descriptor) = pipe_security_attributes()?;
            let server = unsafe {
                ServerOptions::new()
                    .reject_remote_clients(true)
                    .create_with_security_attributes_raw(
                        PIPE_NAME,
                        (&mut attributes as *mut windows_sys::Win32::Security::SECURITY_ATTRIBUTES)
                            .cast(),
                    )?
            };
            unsafe {
                windows_sys::Win32::Foundation::LocalFree(descriptor.cast());
            }
            server
        };
        server.connect().await?;
        let client_state = state.clone();
        tokio::spawn(async move {
            if let Err(error) = handle_stream(server, client_state).await {
                tracing::warn!(%error, "处理本机控制请求失败");
            }
        });
    }
}

#[cfg(windows)]
fn pipe_security_attributes() -> AppResult<(
    windows_sys::Win32::Security::SECURITY_ATTRIBUTES,
    windows_sys::Win32::Security::PSECURITY_DESCRIPTOR,
)> {
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    let sddl: Vec<u16> = "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;OW)"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut descriptor = std::ptr::null_mut();
    let ok = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok((
        windows_sys::Win32::Security::SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<windows_sys::Win32::Security::SECURITY_ATTRIBUTES>()
                as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        },
        descriptor,
    ))
}

#[cfg(not(any(unix, windows)))]
async fn serve_ipc(_state: SharedState) -> AppResult<()> {
    Err(AppError::Config("当前平台不支持本机控制通道".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_groups_all_categories() {
        let value = help();
        let themes = value
            .get("themes")
            .and_then(|v| v.as_array())
            .expect("help 应含 themes 数组");
        assert_eq!(themes.len(), 5, "应有 5 个主题分组");
        for theme in themes {
            let commands = theme
                .get("commands")
                .and_then(|v| v.as_array())
                .expect("每个主题应有 commands 数组");
            assert!(!commands.is_empty(), "每个主题至少有 1 条命令");
        }
    }

    #[test]
    fn command_registry_has_no_duplicate_names() {
        let mut names: Vec<&str> = COMMAND_REGISTRY.iter().map(|s| s.name).collect();
        names.sort_unstable();
        let initial = names.len();
        names.dedup();
        assert_eq!(names.len(), initial, "COMMAND_REGISTRY 中命令名不应重复");
    }

    #[test]
    fn strip_expert_prefix_handles_gt_prefix() {
        assert_eq!(strip_expert_prefix("> dl status"), "dl status");
        assert_eq!(strip_expert_prefix(">dl status"), "dl status");
        assert_eq!(strip_expert_prefix("  > dl status"), "dl status");
        assert_eq!(strip_expert_prefix("dl status"), "dl status");
        assert_eq!(strip_expert_prefix(""), "");
    }

    #[test]
    fn validate_since_duration_multibyte_does_not_panic() {
        // 尾字符为多字节时 split_at 字节切分会 panic；非法值应返回 BadRequest。
        assert!(validate_since_duration("1小时").is_err());
        assert!(validate_since_duration("小时").is_err());
        assert!(validate_since_duration("24小时").is_err());
        // 合法值不受影响
        assert!(validate_since_duration("1h").is_ok());
        assert!(validate_since_duration("24h").is_ok());
        assert!(validate_since_duration("7d").is_ok());
        assert!(validate_since_duration("30m").is_ok());
    }

    #[test]
    fn parse_task_id_or_all_works() {
        assert_eq!(parse_task_id_or_all("all").unwrap(), None);
        assert_eq!(parse_task_id_or_all("123").unwrap(), Some(123));
        assert!(parse_task_id_or_all("abc").is_err());
    }

    #[test]
    fn outcome_to_value_preserves_fields() {
        let accepted = TaskOutcome::accepted("已入队", 42);
        let value = outcome_to_value(accepted);
        assert_eq!(value["ok"], json!(true));
        assert_eq!(value["message"], json!("已入队"));
        assert_eq!(value["download_id"], json!(42));

        let rejected = TaskOutcome::rejected("重复任务");
        let value = outcome_to_value(rejected);
        assert_eq!(value["ok"], json!(false));
        assert_eq!(value["download_id"], json!(null));
    }

    #[test]
    fn pair_command_has_no_temporary_authorization_requirement() {
        // AI 模式开启后 AI 与人工权限一致（审计 2026-08-18 B2：删除 ai assist 短时授权层），
        // 帮助清单里 pair 不应再出现"临时授权"字样。
        let pair = COMMAND_REGISTRY
            .iter()
            .find(|spec| spec.name.starts_with("pair "))
            .expect("pair 应在 COMMAND_REGISTRY 中");
        assert!(!pair.desc.contains("授权"));
    }

    #[cfg(unix)]
    #[test]
    fn ensure_control_socket_parent_reclaims_umask_mode() {
        // 回归：create_dir_all 受 umask 影响常得 0755，旧逻辑"先校验后归权"
        // 会拒绝自己刚创建的目录，导致 XDG 候选必然失败。归权必须先于校验。
        use std::os::unix::fs::PermissionsExt;
        let base = std::env::temp_dir().join(format!(
            "bulibuli-test-parent-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let parent = base.join("bulibuli");
        let data_dir = base.join("data");
        std::fs::create_dir_all(&parent).expect("创建候选目录");
        std::fs::create_dir_all(&data_dir).expect("创建数据目录");
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755))
            .expect("设置 0755 模拟 umask 产物");
        ensure_control_socket_parent(&parent, &data_dir).expect("自建 0755 目录应被归权放行");
        let mode = std::fs::metadata(&parent)
            .expect("读取元数据")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700, "候选目录应已收敛为 0700");
        std::fs::remove_dir_all(&base).ok();
    }

    #[cfg(unix)]
    #[test]
    fn unix_control_socket_candidates_respect_kernel_path_limit() {
        let deep_data_dir = PathBuf::from("/").join("x".repeat(180));
        let candidates = unix_control_socket_candidates(&deep_data_dir);
        assert!(!candidates.is_empty());
        assert!(candidates
            .iter()
            .all(|path| path.as_os_str().as_bytes().len() <= UNIX_SOCKET_MAX_PATH_BYTES));
        assert!(candidates.iter().any(|path| {
            path.file_name().is_some_and(|name| name == "control.sock")
                && path.to_string_lossy().contains("bulibuli-")
        }));
    }
}

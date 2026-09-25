//! IPC 传输层：客户端连接、服务端监听循环与平台传输安全。
//!
//! - Windows：命名管道 `\\.\pipe\bulibuli`，SDDL 限制本机进程，连接后校验服务端镜像身份（S2）
//! - Unix：优先 `XDG_RUNTIME_DIR`，回退数据目录与临时目录，socket 文件权限收紧到 0600

use crate::error::{AppError, AppResult};
use crate::state::SharedState;
use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;
use tokio::io::AsyncWriteExt;
use tokio::task::{JoinHandle, JoinSet};
use tokio_util::sync::CancellationToken;

use super::stream::{
    handle_stream, is_streaming_command, read_control_response, MAX_COMMAND_BYTES,
};

use super::helpers::{format_response, split_command, strip_expert_prefix};
use super::CommandOrigin;

#[cfg(windows)]
const PIPE_NAME: &str = r"\\.\pipe\bulibuli";

#[cfg(unix)]
// 采用 macOS 更严格的 104 字节上限（含结尾 NUL），Linux 也可安全使用。
const UNIX_SOCKET_MAX_PATH_BYTES: usize = 103;

pub fn start_server(state: SharedState, cancellation: CancellationToken) -> JoinHandle<()> {
    tokio::spawn(async move {
        if let Err(error) = serve_ipc(state, cancellation).await {
            tracing::error!(%error, "本机控制通道退出");
        }
    })
}

/// 终端界面未启用时的回退：从 stdin 逐行读命令，错误只回显不中断服务。
pub fn start_stdin_loop(
    state: SharedState,
    cancellation: CancellationToken,
) -> Option<JoinHandle<()>> {
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        return None;
    }
    Some(tokio::spawn(async move {
        use tokio::io::{AsyncBufReadExt, BufReader};
        let mut lines = BufReader::new(tokio::io::stdin()).lines();
        loop {
            let line = tokio::select! {
                _ = cancellation.cancelled() => break,
                result = lines.next_line() => result,
            };
            let Ok(Some(line)) = line else { break };
            // `>` 前缀 = 专家模式直输；剥离后走扁平命令路径
            let trimmed = strip_expert_prefix(&line);
            let args = split_command(trimmed);
            match super::execute_from(&state, &args, CommandOrigin::HumanTerminal).await {
                Ok(value) => println!("{}", format_response(&value)),
                Err(error) => eprintln!("控制命令失败：{error}"),
            }
        }
    }))
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
        stream.write_all(b"\n").await?;
        stream.shutdown().await?;
        let response = read_control_response(&mut stream, is_streaming_command(args)).await?;
        String::from_utf8(response)
            .map_err(|error| AppError::Internal(format!("控制响应不是 UTF-8: {error}")))
    }
    #[cfg(windows)]
    {
        let mut stream = tokio::net::windows::named_pipe::ClientOptions::new().open(PIPE_NAME)?;
        // 连接后先校验服务端身份（S2）：任何本机进程都能创建同名管道冒充服务端。
        verify_pipe_server_identity(&stream)?;
        stream.write_all(&request).await?;
        // 换行收尾：管道客户端的 shutdown() 不产生服务端可见的 EOF。
        stream.write_all(b"\n").await?;
        stream.shutdown().await?;
        let response = read_control_response(&mut stream, is_streaming_command(args)).await?;
        String::from_utf8(response)
            .map_err(|error| AppError::Internal(format!("控制响应不是 UTF-8: {error}")))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _unused = data_dir;
        Err(AppError::Config("当前平台不支持本机控制通道".to_string()))
    }
}

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
    use std::os::unix::ffi::OsStrExt;

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

/// 探测 Unix socket 是否有活服务在监听：connect 成功即视为活。
/// 连接本身不发送任何数据，服务端会因读超时/EOF 自行清理。
#[cfg(unix)]
async fn unix_socket_is_alive(path: &Path) -> bool {
    matches!(
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
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
    // 归权失败不在此处报错：目录可能是他人的，后续属主/模式校验会拒绝。
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).ok();
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
async fn serve_ipc(state: SharedState, cancellation: CancellationToken) -> AppResult<()> {
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
    let mut clients = JoinSet::new();
    loop {
        tokio::select! {
            _ = cancellation.cancelled() => break,
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                let client_state = state.clone();
                clients.spawn(async move {
                    if let Err(error) = handle_stream(stream, client_state).await {
                        tracing::warn!(%error, "处理本机控制请求失败");
                    }
                });
            }
        }
    }
    clients.shutdown().await;
    Ok(())
}

#[cfg(windows)]
async fn serve_ipc(state: SharedState, cancellation: CancellationToken) -> AppResult<()> {
    use tokio::net::windows::named_pipe::ServerOptions;
    let mut clients = JoinSet::new();
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
        tokio::select! {
            _ = cancellation.cancelled() => break,
            result = server.connect() => result?,
        }
        let client_state = state.clone();
        clients.spawn(async move {
            if let Err(error) = handle_stream(server, client_state).await {
                tracing::warn!(%error, "处理本机控制请求失败");
            }
        });
    }
    clients.shutdown().await;
    Ok(())
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
async fn serve_ipc(_state: SharedState, _cancellation: CancellationToken) -> AppResult<()> {
    Err(AppError::Config("当前平台不支持本机控制通道".to_string()))
}

/// Unix 控制通道路径与权限相关测试；Windows 下无对应实现，整模块不参与编译。
#[cfg(unix)]
mod tests {
    use super::*;

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

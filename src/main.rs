#![recursion_limit = "256"]

mod api;
mod app;
mod config;
mod db;
mod domain;
mod error;
mod migration;
mod models;
mod services;
mod state;
mod ws;

use crate::config::load_config;
use crate::db::init_database;
use crate::state::AppState;
use tokio::task::JoinHandle;
use tracing::{error, info};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if let Some(result) = handle_early_cli(&args) {
        return result;
    }
    install_crypto_provider()?;
    if let Err(e) = run(args).await {
        // 在 tracing 初始化之前用 eprintln 确保错误可见；随后直接 exit(1)，
        // 避免错误再经 Termination 的 Debug 输出打印第二遍（stderr 双份）。
        eprintln!(
            "{}",
            app::term_style::error(&format!("应用启动失败: {e:#}"))
        );
        std::process::exit(1);
    }
    Ok(())
}

fn install_crypto_provider() -> anyhow::Result<()> {
    if rustls::crypto::CryptoProvider::get_default().is_some() {
        return Ok(());
    }
    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|_| anyhow::anyhow!("Rustls CryptoProvider 已被其他组件初始化"))
}

async fn run(args: Vec<String>) -> anyhow::Result<()> {
    validate_cli_args(&args)?;
    let (config, paths) = load_config()?;
    // 上次运行暂存的更新在启动早期完成替换（只换程序文件、不碰 data/）。
    // tracing 尚未初始化，错误用 eprintln 保证可见但不阻塞启动。
    if let Err(error) = services::update::startup_apply_staged(&paths) {
        eprintln!(
            "{}",
            app::term_style::error(&format!("应用暂存更新失败: {error:#}"))
        );
    }
    if args.first().is_some_and(|value| value == "ctl") {
        // 退出码约定：0 成功 / 1 命令被拒绝或执行失败 / 2 本地连接失败。
        // 响应本来就是 JSON 信封，按 ok 字段判定结果供 AI/脚本靠 exit code 决策。
        return match app::control::run_client(&paths.data_dir, &args[1..]).await {
            Ok(response) => {
                let envelope: Result<serde_json::Value, _> = serde_json::from_str(&response);
                match envelope {
                    Ok(value) if value.get("ok") == Some(&serde_json::Value::Bool(false)) => {
                        let error = value
                            .get("error")
                            .and_then(|v| v.as_str())
                            .unwrap_or("未知错误");
                        let code = value.get("code").and_then(|v| v.as_str()).unwrap_or("-");
                        eprintln!("ctl 命令失败 [{code}]: {error}");
                        std::process::exit(1);
                    }
                    _ => {
                        println!("{response}");
                        Ok(())
                    }
                }
            }
            Err(e) => {
                eprintln!("ctl 连接失败（服务未运行？）: {e:#}");
                std::process::exit(2);
            }
        };
    }

    // `--open` / `open`：仅打开浏览器到网页管理界面，不启动服务。
    if args.first().is_some_and(|v| v == "--open" || v == "open") {
        let port = read_actual_port(&paths).unwrap_or(config.port);
        let url = format!("http://127.0.0.1:{port}");
        println!("正在打开浏览器: {url}");
        app::onboarding::open_browser_safe(&url);
        return Ok(());
    }

    // 保存配置端口，供后续 onboarding 使用（config 会被移入 AppState）
    let config_port = config.port;

    // 交互终端下启用终端界面（日志分区 + 命令输入），控制台日志改经缓冲区展示
    let interactive = std::io::IsTerminal::is_terminal(&std::io::stdin())
        && std::io::IsTerminal::is_terminal(&std::io::stdout());

    let console_writer = app::tui::init(interactive);
    app::tracing_setup::init_tracing(&paths.data_dir.join("logs"), console_writer)?;

    // 防双实例：对 data/bulibuli.lock 加排他文件锁，句柄在进程生命周期内持有。
    // 拿不到锁说明已有实例在运行，直接报错退出。
    // 错误链已含"实例锁被占用（可能已有实例在运行）"上下文，由 main 的
    // eprintln 统一打印一次即可；这里不再先 error! 一遍避免双份输出。
    let _instance_lock = acquire_instance_lock(&paths.data_dir)?;

    info!("starting {} v{}", config.app_name, config.app_version);
    let db = init_database(&paths, &config).await?;
    let state = AppState::new(config, paths, db, false).await?;

    // BiliApi 就绪后：同步 B 站登录 UID 到 startup_state.json。
    app::onboarding::sync_bili_uid(&state, &state.infra.paths).await;

    let control_handle =
        app::control::start_server(state.clone(), state.infra.cancellation.clone());

    // 更新策略启动检查：manual 仅记录最新版本，auto 额外下载暂存，off 不发任何请求。
    let startup_update_handle = {
        let state = state.clone();
        let cancellation = state.infra.cancellation.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = cancellation.cancelled() => {}
                result = services::update::startup_check(&state) => {
                    if let Err(error) = result {
                        tracing::warn!(%error, "启动更新检查失败");
                    }
                }
            }
        })
    };

    // 启动 Setup 独立端口服务（始终 localhost，供首次设置向导和重新配置使用）
    // 必须先于 onboarding 启动，这样 onboarding 打开浏览器时服务器已就绪。
    let (actual_setup_port, setup_handle) =
        match app::setup_server::start_setup_server(state.clone()).await {
            Ok((port, handle)) if port != 0 => {
                info!("setup server started on port {port}");
                (port, handle)
            }
            // 0 = 未启动：onboarding 已完成（默认模式）或 BILI__SETUP_PORT_ENABLED=false。
            Ok((_, handle)) => {
                info!("setup server 未启动");
                (0, handle)
            }
            Err(e) => {
                // 不再回退到"假端口"（port+1 上并无服务监听，浏览器会指向连接拒绝地址）。
                // 需要该端口却绑定失败时明确报错退出，让用户看到真实原因。
                state.infra.cancellation.cancel();
                wait_background_task("IPC server", Some(control_handle)).await;
                wait_background_task("startup update check", Some(startup_update_handle)).await;
                state.infra.background_tasks.shutdown().await;
                state.infra.db.clone().close().await?;
                return Err(anyhow::anyhow!(
                    "Setup 端口绑定失败（首次配置向导依赖该端口，请检查端口 {} 是否被占用）: {e}",
                    config_port + 1
                ));
            }
        };

    // Onboarding：首次启动打印 Setup URL + 自动打开浏览器；后续启动显示状态摘要 + 自动打开浏览器。
    // 此时 setup server 已就绪，浏览器打开后可立即访问。
    let startup_state = match app::onboarding::run(
        &state.infra.paths,
        interactive,
        actual_setup_port,
        config_port,
    )
    .await
    {
        Ok(value) => value,
        Err(error) => {
            state.infra.cancellation.cancel();
            app::setup_server::shutdown_setup_server();
            wait_background_task("Setup server", setup_handle).await;
            wait_background_task("IPC server", Some(control_handle)).await;
            wait_background_task("startup update check", Some(startup_update_handle)).await;
            state.infra.background_tasks.shutdown().await;
            state.infra.db.clone().close().await?;
            return Err(error);
        }
    };
    let first_launch = !startup_state.onboarding_completed;

    // 同步 AI Skill 状态（onboarding 可能在 Web 向导中修改了此值）
    state.infra.ai_skill_enabled.store(
        startup_state.ai_skill_enabled,
        std::sync::atomic::Ordering::Relaxed,
    );

    // 审计事件桥接：把审计事件流转发给 WebSocket 客户端，前端可实时感知多端写操作。
    let audit_bridge_handle = ws::start_audit_event_bridge(
        state.infra.ws.clone(),
        state.infra.audit_log.subscribe(),
        state.infra.cancellation.clone(),
    );

    // 审计日志 30 天自动清理：每 24 小时清理一次 30 天前的记录。
    let audit_cleanup_handle = state
        .infra
        .audit_log
        .clone()
        .start_cleanup_task(state.infra.cancellation.clone());

    // 后台服务必须在绑定主端口之前完成启动：一旦端口进入内核监听队列，
    // 连接会立即成功但请求要等到 axum::serve 开始 accept 才有响应；
    // 若 monitor/download_manager 启动耗时，用户会看到"端口在听但 curl 无响应"的假死窗口。
    state.media.download_manager.start_monitor().await;
    state.business.monitor_service.start().await;
    state.business.refresh_service.start().await;
    state.business.live_monitor.start().await;
    state.bili.verify_service.start().await;

    // 服务就绪后再绑定端口并立即 serve，确保浏览器打开时服务器已可响应。
    // 失败原因已包含在错误链里，交由 main 的 eprintln 打印一次（避免双份输出）。
    let (listener, actual_port) = match app::server::bind_main_listener(&state).await {
        Ok(bound) => bound,
        Err(error) => {
            state.infra.cancellation.cancel();
            state.media.download_manager.stop_monitor().await;
            state.business.monitor_service.stop().await;
            state.business.refresh_service.stop().await;
            state.business.live_monitor.stop().await;
            state.bili.verify_service.stop().await;
            state.media.live_recorder.stop_all().await;
            let _ = state.media.aria2.stop().await;
            app::setup_server::shutdown_setup_server();
            wait_background_task("Setup server", setup_handle).await;
            wait_background_task("IPC server", Some(control_handle)).await;
            wait_background_task("startup update check", Some(startup_update_handle)).await;
            wait_background_task("audit event bridge", Some(audit_bridge_handle)).await;
            wait_background_task("audit cleanup", Some(audit_cleanup_handle)).await;
            state.infra.background_tasks.shutdown().await;
            state.infra.db.clone().close().await?;
            return Err(error);
        }
    };

    // 首次/后续启动统一走 TUI：banner 与摘要经 console_line 进日志缓冲，TUI 接管后首屏可见。
    // 状态栏已含 URL/模式/AI，这里只补充状态栏没有的增量信息。
    let bili_text = match startup_state.bili_logged_in_uid {
        Some(uid) => format!("B站登录: 已登录({uid})"),
        None => "B站登录: 未登录".to_string(),
    };
    app::tui::console_line(bili_text);
    let main_url = app::server::main_url(&state);
    app::tui::console_line(format!("网页管理: {main_url}"));
    if first_launch {
        app::tui::console_line(
            "首次设置窗口已打开；完成设置后将使用主端口网页管理界面".to_string(),
        );
    } else if app::onboarding::browser_available() {
        app::tui::console_line("正在自动打开浏览器...".to_string());
    } else {
        // 无桌面环境（如无头服务器）：明确告知需手动访问，而非输出一句无效的"正在打开"。
        app::tui::console_line(
            "未检测到图形桌面环境，跳过自动打开浏览器；请在本地电脑浏览器中访问上面的地址"
                .to_string(),
        );
    }
    if !first_launch {
        app::onboarding::open_browser_safe(&main_url);
    }

    let mut tui_handle = None;
    let mut stdin_handle = None;
    match app::tui::start(state.clone()) {
        Some(handle) => tui_handle = Some(handle),
        None => {
            stdin_handle =
                app::control::start_stdin_loop(state.clone(), state.infra.cancellation.clone());
        }
    }

    let server_result = app::server::serve(state.clone(), listener, actual_port).await;
    state.infra.cancellation.cancel();
    if let Some(handle) = tui_handle.take() {
        handle.join();
    }
    state.media.download_manager.stop_monitor().await;
    state.business.monitor_service.stop().await;
    state.business.refresh_service.stop().await;
    state.business.live_monitor.stop().await;
    state.bili.verify_service.stop().await;
    state.media.live_recorder.stop_all().await;
    if let Err(error) = state.media.aria2.stop().await {
        error!("stopping aria2 failed: {error}");
    }
    app::setup_server::shutdown_setup_server();
    wait_background_task("Setup server", setup_handle).await;
    wait_background_task("IPC server", Some(control_handle)).await;
    wait_background_task("stdin loop", stdin_handle).await;
    wait_background_task("startup update check", Some(startup_update_handle)).await;
    wait_background_task("audit event bridge", Some(audit_bridge_handle)).await;
    wait_background_task("audit cleanup", Some(audit_cleanup_handle)).await;
    state.infra.background_tasks.shutdown().await;
    state.infra.db.clone().close().await?;
    info!("shutdown complete");
    // 清理完成后再传播 serve 的错误：出错路径同样需要停止 aria2/录制并正常关闭数据库。
    server_result?;
    Ok(())
}

async fn wait_background_task(name: &'static str, handle: Option<JoinHandle<()>>) {
    let Some(handle) = handle else { return };
    services::spawn_util::wait_join_handle(name, handle, std::time::Duration::from_secs(10)).await;
}

fn handle_early_cli(args: &[String]) -> Option<anyhow::Result<()>> {
    match args.first().map(String::as_str) {
        Some("--version") => {
            println!("bulibuli {}", env!("CARGO_PKG_VERSION"));
            Some(Ok(()))
        }
        Some("--help") | Some("-h") => {
            println!(
                "补哩补哩 bulibuli {}\n\n用法:\n  bulibuli                 启动服务\n  bulibuli open            打开浏览器到网页管理界面后退出\n  bulibuli --version       输出版本并退出\n  bulibuli --help          显示帮助并退出\n  bulibuli ctl <command>   执行高级控制命令\n\n常用控制命令（需服务已运行）:\n  bulibuli ctl sys status\n  bulibuli ctl sys ffmpeg-test\n  bulibuli ctl sys aria2-restart\n  bulibuli ctl dl status\n\n提示：ctl 命令默认仅放行 status/help/quit/ai/pair/sys status，先执行 `bulibuli ctl ai on` 启用 AI Skill 模式后，AI 可执行与人工相同的全部命令（含 mode/access/geo/trust）。",
                env!("CARGO_PKG_VERSION")
            );
            Some(Ok(()))
        }
        _ => None,
    }
}

/// 校验剩余命令行参数：合法入口仅 `ctl <command...>` / `open` / `--open`，
/// 其余（含拼写错误的 flag，如 `--potr`）直接报错而非静默忽略——否则用户
/// 以为参数生效了。`ctl` 的子命令合法性由 control 层自行校验。
fn validate_cli_args(args: &[String]) -> anyhow::Result<()> {
    match args.first().map(String::as_str) {
        Some("ctl") => Ok(()),
        Some("open") | Some("--open") | None => {
            if let Some(extra) = args.get(1) {
                return Err(anyhow::anyhow!(
                    "未知命令行参数 `{extra}`；用法见 `bulibuli --help`"
                ));
            }
            Ok(())
        }
        Some(other) => Err(anyhow::anyhow!(
            "未知命令行参数 `{other}`；用法见 `bulibuli --help`"
        )),
    }
}

/// 读取 `actual_port.txt` 获取上次运行的实际端口。
fn read_actual_port(paths: &crate::config::AppPaths) -> Option<u16> {
    std::fs::read_to_string(paths.data_dir.join("actual_port.txt"))
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

/// 对 `data/bulibuli.lock` 加排他文件锁防止双实例。
/// 返回的文件句柄必须由调用方持有到进程结束，drop 后锁自动释放。
fn acquire_instance_lock(data_dir: &std::path::Path) -> anyhow::Result<std::fs::File> {
    use anyhow::Context;
    use fs2::FileExt;
    std::fs::create_dir_all(data_dir).context("创建数据目录失败")?;
    let lock_path = data_dir.join("bulibuli.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("打开实例锁文件失败: {}", lock_path.display()))?;
    file.try_lock_exclusive().with_context(|| {
        format!(
            "实例锁被占用（可能已有实例在运行）: {}",
            lock_path.display()
        )
    })?;
    Ok(file)
}

#[cfg(test)]
mod tls_tests {
    #[test]
    fn early_cli_flags_do_not_enter_runtime_bootstrap() {
        assert!(super::handle_early_cli(&["--version".to_string()]).is_some());
        assert!(super::handle_early_cli(&["--help".to_string()]).is_some());
        assert!(super::handle_early_cli(&["ctl".to_string()]).is_none());
    }

    #[test]
    fn unknown_cli_args_are_rejected() {
        // 拼写错误的 flag 必须报错而非静默忽略。
        assert!(super::validate_cli_args(&["--potr".to_string(), "8080".into()]).is_err());
        assert!(super::validate_cli_args(&["sreve".to_string()]).is_err());
        // open 不接受多余参数。
        assert!(super::validate_cli_args(&["open".to_string(), "extra".into()]).is_err());
        // 合法入口放行。
        assert!(super::validate_cli_args(&[]).is_ok());
        assert!(super::validate_cli_args(&["ctl".to_string(), "sys status".into()]).is_ok());
        assert!(super::validate_cli_args(&["--open".to_string()]).is_ok());
    }

    #[test]
    fn ring_provider_builds_client_config() {
        super::install_crypto_provider().expect("install ring provider");
        let _config = rustls::ClientConfig::builder()
            .with_root_certificates(rustls::RootCertStore::empty())
            .with_no_client_auth();
    }
}

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
use tracing::{error, info};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    install_crypto_provider()?;
    if let Err(e) = run().await {
        // 在 tracing 初始化之前用 eprintln 确保错误可见
        eprintln!("应用启动失败: {:#}", e);
        return Err(e);
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

async fn run() -> anyhow::Result<()> {
    let (config, paths) = load_config()?;
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.first().is_some_and(|value| value == "ctl") {
        let response = app::control::run_client(&paths.data_dir, &args[1..]).await?;
        println!("{response}");
        return Ok(());
    }

    // `--open` / `open`：仅打开浏览器到网页管理界面，不启动服务。
    if args.first().is_some_and(|v| v == "--open" || v == "open") {
        let port = read_actual_port(&paths).unwrap_or(config.port);
        let url = format!("http://127.0.0.1:{port}");
        println!("正在打开浏览器: {url}");
        if let Err(e) = open::that(&url) {
            eprintln!("打开浏览器失败: {e}");
            eprintln!("请手动复制上面的地址到浏览器");
        }
        return Ok(());
    }

    // 保存配置端口，供后续 onboarding 使用（config 会被移入 AppState）
    let config_port = config.port;

    // 交互终端下启用终端界面（日志分区 + 命令输入），控制台日志改经缓冲区展示
    let interactive = std::io::IsTerminal::is_terminal(&std::io::stdin())
        && std::io::IsTerminal::is_terminal(&std::io::stdout());

    let console_writer = app::tui::init(interactive);
    app::tracing_setup::init_tracing(&paths.data_dir.join("logs"), console_writer)?;

    info!("starting {} v{}", config.app_name, config.app_version);
    let db = init_database(&paths, &config).await?;
    let state = AppState::new(config, paths, db, false).await?;

    // BiliApi 就绪后：同步 B 站登录 UID 到 startup_state.json。
    app::onboarding::sync_bili_uid(&state, &state.infra.paths).await;

    app::control::start_server(state.clone());

    // 启动 Setup 独立端口服务（始终 localhost，供首次设置向导和重新配置使用）
    // 必须先于 onboarding 启动，这样 onboarding 打开浏览器时服务器已就绪。
    let actual_setup_port = match app::setup_server::start_setup_server(state.clone()).await {
        Ok(port) => {
            info!("setup server started on port {port}");
            port
        }
        Err(e) => {
            tracing::warn!(%e, "setup server 启动失败，使用默认端口");
            config_port + 1
        }
    };

    // Onboarding：首次启动打印 Setup URL + 自动打开浏览器；后续启动显示状态摘要 + 自动打开浏览器。
    // 此时 setup server 已就绪，浏览器打开后可立即访问。
    let startup_state = app::onboarding::run(
        &state.infra.paths,
        interactive,
        actual_setup_port,
        config_port,
    )
    .await?;

    // 同步 AI Skill 状态（onboarding 可能在 Web 向导中修改了此值）
    state.infra.ai_skill_enabled.store(
        startup_state.ai_skill_enabled,
        std::sync::atomic::Ordering::Relaxed,
    );

    // BiliApi 就绪后：若向导选了“现在扫码”则执行扫码登录。
    if startup_state.ai_skill_enabled && startup_state.scan_now_requested {
        app::onboarding::run_qr_login(&state, &state.infra.paths).await;
    }

    // 审计事件桥接：把审计事件流转发给 WebSocket 客户端，前端可实时感知多端写操作。
    ws::start_audit_event_bridge(state.infra.ws.clone(), state.infra.audit_log.subscribe());

    // 审计日志 30 天自动清理：每 24 小时清理一次 30 天前的记录。
    state
        .infra
        .audit_log
        .clone()
        .start_cleanup_task(state.infra.cancellation.clone());

    // 先绑定端口再启动 TUI/浏览器，确保浏览器打开时服务器已就绪。
    let (listener, actual_port) = match app::server::bind_main_listener(&state).await {
        Ok(result) => result,
        Err(e) => {
            error!("主服务器端口绑定失败: {e}");
            return Err(e);
        }
    };

    let is_first_launch = !startup_state.onboarding_completed;
    let mut tui_handle = None;
    if is_first_launch {
        info!("首次启动，请在浏览器中完成设置向导");
        app::control::start_stdin_loop(state.clone());
    } else {
        // 后续启动：显示状态摘要（TUI 启动前用 println，用户可见）
        let mode = crate::services::security_config::SecurityConfigService::load(
            &state.infra.paths.data_dir,
            &state.infra.paths.app_root,
        )
        .map(|s| s.current().mode)
        .unwrap_or(crate::services::security_config::AccessMode::Local);
        let mode_text = match mode {
            crate::services::security_config::AccessMode::Local => "本机",
            crate::services::security_config::AccessMode::Lan => "局域网",
            crate::services::security_config::AccessMode::Proxy => "代理",
        };
        let ai_text = if startup_state.ai_skill_enabled {
            "已启用"
        } else {
            "未启用"
        };
        let bili_text = match startup_state.bili_logged_in_uid {
            Some(uid) => format!("已登录({uid})"),
            None => "未登录".to_string(),
        };
        println!("═══════════════════════════════════════════════════════");
        println!("  补哩补哩 bulibuli v{}", env!("CARGO_PKG_VERSION"));
        let main_url = app::server::main_url(&state);
        println!("  网页管理: {main_url}");
        println!("  监听模式: {mode_text} | AI 模式: {ai_text} | B站: {bili_text}");
        println!();
        println!("  正在自动打开浏览器...");
        println!("  Ctrl+C 停止服务");
        println!("═══════════════════════════════════════════════════════");

        app::onboarding::open_browser_safe(&main_url);

        match app::tui::start(state.clone()) {
            Some(handle) => tui_handle = Some(handle),
            None => app::control::start_stdin_loop(state.clone()),
        }
    }

    state.media.download_manager.start_monitor().await;
    state.business.monitor_service.start().await;
    state.business.refresh_service.start().await;
    state.business.live_monitor.start().await;
    state.bili.verify_service.start().await;

    let server_result = app::server::serve(state.clone(), listener, actual_port).await;
    state.infra.cancellation.cancel();
    if let Some(handle) = tui_handle.take() {
        handle.join();
    }
    server_result?;
    state.media.download_manager.stop_monitor().await;
    state.business.monitor_service.stop().await;
    state.business.refresh_service.stop().await;
    state.business.live_monitor.stop().await;
    state.bili.verify_service.stop().await;
    state.media.live_recorder.stop_all().await;
    if let Err(error) = state.media.aria2.stop().await {
        error!("stopping aria2 failed: {error}");
    }
    state.infra.db.clone().close().await?;
    info!("shutdown complete");
    Ok(())
}

/// 读取 `actual_port.txt` 获取上次运行的实际端口。
fn read_actual_port(paths: &crate::config::AppPaths) -> Option<u16> {
    std::fs::read_to_string(paths.data_dir.join("actual_port.txt"))
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

#[cfg(test)]
mod tls_tests {
    #[test]
    fn ring_provider_builds_client_config() {
        super::install_crypto_provider().expect("install ring provider");
        let _config = rustls::ClientConfig::builder()
            .with_root_certificates(rustls::RootCertStore::empty())
            .with_no_client_auth();
    }
}

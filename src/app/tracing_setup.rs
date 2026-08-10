//! 日志初始化：文件滚动 + 可选的终端界面控制台缓冲输出。
//!
//! 从 `security_server` 拆分而来：日志装配与 HTTP 服务无关，独立成模块便于维护。

/// 初始化 tracing：始终写按日滚动的文件日志；交互终端下额外接入控制台缓冲写入器。
pub fn init_tracing(
    log_dir: &std::path::Path,
    console_writer: Option<crate::app::tui::ConsoleWriter>,
) -> anyhow::Result<()> {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    std::fs::create_dir_all(log_dir)?;
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let file = tracing_subscriber::fmt::layer()
        .with_writer(tracing_appender::rolling::daily(log_dir, "app.log"))
        .with_ansi(false)
        .with_target(false);
    let registry = tracing_subscriber::registry().with(filter).with(file);
    // 交互终端：控制台日志经缓冲区供终端界面展示（无 ANSI，避免控制序列污染画面）
    match console_writer {
        Some(writer) => registry
            .with(
                tracing_subscriber::fmt::layer()
                    .with_target(false)
                    .with_ansi(false)
                    .with_writer(writer),
            )
            .try_init(),
        None => registry
            .with(tracing_subscriber::fmt::layer().with_target(false))
            .try_init(),
    }
    .map_err(|error| anyhow::anyhow!("initialize tracing: {error}"))?;
    Ok(())
}

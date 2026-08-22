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
    // 按天滚动并限制保留文件数（tracing-appender 0.2 支持 max_log_files），
    // 避免日志目录无限增长；aria2.log 等无轮转文件由 cleanup_old_logs 兜底清理。
    let file_appender = tracing_appender::rolling::RollingFileAppender::builder()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("app")
        .filename_suffix("log")
        .max_log_files(14)
        .build(log_dir)
        .map_err(|error| anyhow::anyhow!("build rolling log appender: {error}"))?;
    cleanup_old_logs(log_dir, 14);
    let file = tracing_subscriber::fmt::layer()
        .with_writer(file_appender)
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
            .with(
                tracing_subscriber::fmt::layer()
                    .with_target(false)
                    // 非 TTY 分支（服务/管道/重定向）：tracing-subscriber 默认
                    // 开 ANSI（只看 NO_COLOR，不做终端探测），管道和日志采集器
                    // 会收到转义序列。仅当 stdout 确为终端时才保留颜色，
                    // 与 term_style 的 colored() 判定口径一致。
                    .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stdout())),
            )
            .try_init(),
    }
    .map_err(|error| anyhow::anyhow!("initialize tracing: {error}"))?;
    Ok(())
}

/// 启动时清理日志目录中超过保留期的旧日志文件（含无轮转的 aria2.log 等）。
fn cleanup_old_logs(log_dir: &std::path::Path, keep_days: u64) {
    let cutoff =
        std::time::SystemTime::now() - std::time::Duration::from_secs(keep_days * 24 * 3600);
    let Ok(entries) = std::fs::read_dir(log_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let expired = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .map(|modified| modified < cutoff)
            .unwrap_or(false);
        if expired {
            if let Err(error) = std::fs::remove_file(&path) {
                eprintln!("清理旧日志失败 {}: {error}", path.display());
            }
        }
    }
}

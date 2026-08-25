//! 服务器终端交互界面：双模式布局。
//!
//! - **Web 模式**（默认）：上方日志滚动区 + 底部状态栏（URL + 模式提示）+ 输入框。
//!   输入框始终可用，直接输入视为专家命令（`>` 前缀可选）。`mode` 切换到终端模式。
//! - **终端模式**：上方日志区 + 中间命令帮助面板 + 底部输入框。
//!   所有命令直接输入，不需要 `>` 前缀。`mode` 切回 Web 模式。
//!
//! 仅在 stdin/stdout 均为终端时启用；非交互环境（管道、服务）自动回退纯日志输出。
//! 命令始终异步执行且错误只回显不中断，保证输错命令不会卡死或影响服务运行。

use crate::app::control;
use crate::app::onboarding::{StartupState, TerminalMode};
use crate::state::SharedState;
use ratatui::crossterm::{
    self,
    event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind},
};
use ratatui::layout::{Constraint, Layout, Position};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

const MAX_LOG_LINES: usize = 1000;
const MAX_IMPORTANT_LOG_LINES: usize = 200;

/// 终端模式下的精简命令帮助（按类别分组）。
const TERMINAL_HELP_ITEMS: &[(&str, &str)] = &[
    ("status", "查看运行状态"),
    ("dl ...", "下载管理: status/add/pause/resume/retry/remove"),
    ("blg ...", "博主管理: search/add/list/del/monitor"),
    (
        "sys ...",
        "系统操作: status/config/aria2-restart/ffmpeg-test/logs",
    ),
    ("cred ...", "凭证管理: qrcode/qrcode-poll/status"),
    ("ai on|off", "切换 AI Skill 模式"),
    ("mode", "切回 Web 模式"),
    ("help", "完整命令清单"),
    ("quit", "优雅关停程序"),
];

/// 界面是否已接管屏幕：接管期间日志只进缓冲区，退出后恢复直写 stdout。
static TUI_ACTIVE: AtomicBool = AtomicBool::new(false);
static CONSOLE: OnceLock<LogBuffer> = OnceLock::new();

#[derive(Clone, Default)]
pub struct LogBuffer {
    inner: Arc<Mutex<LogInner>>,
}

#[derive(Default)]
struct LogInner {
    lines: VecDeque<String>,
    important_lines: VecDeque<String>,
    pending_line: String,
    pending_utf8: Vec<u8>,
}

impl LogBuffer {
    fn push_line(&self, line: String) {
        let mut inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        Self::store_line(&mut inner, line);
    }

    /// 接收 tracing writer 的字节流，先按完整 UTF-8 序列解码，再按行切分。
    ///
    /// `Write::write` 不保证一次写入一个完整字符；中文的三个 UTF-8 字节
    /// 可能被分到三次写入，不能对每个字节块单独调用 `from_utf8_lossy`。
    fn push_chunk(&self, chunk: &[u8]) {
        let mut inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        inner.pending_utf8.extend_from_slice(chunk);
        let decoded = Self::decode_pending_utf8(&mut inner.pending_utf8);
        inner.pending_line.push_str(&decoded);
        while let Some(position) = inner.pending_line.find('\n') {
            let line: String = inner.pending_line.drain(..=position).collect();
            Self::store_line(&mut inner, line.trim_end_matches(['\r', '\n']).to_string());
        }
    }

    /// 解码所有完整 UTF-8 字符，保留末尾尚不完整的字节序列。
    fn decode_pending_utf8(bytes: &mut Vec<u8>) -> String {
        let mut decoded = String::new();
        let mut cursor = 0;
        while cursor < bytes.len() {
            match std::str::from_utf8(&bytes[cursor..]) {
                Ok(text) => {
                    decoded.push_str(text);
                    cursor = bytes.len();
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    if valid > 0 {
                        decoded.push_str(
                            std::str::from_utf8(&bytes[cursor..cursor + valid])
                                .expect("valid UTF-8 prefix"),
                        );
                        cursor += valid;
                    }
                    match error.error_len() {
                        Some(length) => {
                            decoded.push('\u{FFFD}');
                            cursor = cursor.saturating_add(length);
                        }
                        None => break,
                    }
                }
            }
        }
        if cursor > 0 {
            bytes.drain(..cursor);
        }
        decoded
    }

    fn store_line(inner: &mut LogInner, line: String) {
        if Self::is_important_line(&line) {
            Self::push_bounded(
                &mut inner.important_lines,
                line.clone(),
                MAX_IMPORTANT_LOG_LINES,
            );
        }
        Self::push_bounded(&mut inner.lines, line, MAX_LOG_LINES);
    }

    fn is_important_line(line: &str) -> bool {
        line.contains(" WARN ")
            || line.contains(" ERROR ")
            || line.starts_with("WARN ")
            || line.starts_with("ERROR ")
            || line.contains("命令失败：")
    }

    fn push_bounded(lines: &mut VecDeque<String>, line: String, limit: usize) {
        if lines.len() >= limit {
            lines.pop_front();
        }
        lines.push_back(line);
    }

    fn important_lines(&self) -> Vec<String> {
        self.inner
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .important_lines
            .iter()
            .cloned()
            .collect()
    }

    fn exit_summary(&self, log_dir: &Path) -> String {
        let important = self.important_lines();
        let mut summary = String::new();
        if !important.is_empty() {
            summary.push_str("\n=== 本次运行的警告与错误（最多最近 200 条）===\n");
            for line in important {
                summary.push_str(&line);
                summary.push('\n');
            }
        }
        // 日志目录以实际 data_dir 为准（BILI__DATA_DIR 可重定向），不再硬编码 data/logs。
        summary.push_str(&format!("完整日志目录: {}\n", log_dir.display()));
        summary
    }

    /// 取一窗口日志：`offset` 为距底部的行数（0 = 最新），返回窗口内容与总行数。
    fn view(&self, offset: usize, count: usize) -> (Vec<String>, usize) {
        let inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        let total = inner.lines.len();
        let end = total.saturating_sub(offset);
        let start = end.saturating_sub(count);
        (
            inner
                .lines
                .iter()
                .skip(start)
                .take(end - start)
                .cloned()
                .collect(),
            total,
        )
    }

    /// 日志行级别样式：错误红色加粗、警告黄色、配对码等凭证青色加粗、命令回显弱化。
    fn line_style(line: &str) -> Option<Style> {
        if line.contains(" ERROR ") || line.starts_with("ERROR ") || line.contains("命令失败：")
        {
            Some(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD))
        } else if line.contains(" WARN ") || line.starts_with("WARN ") {
            Some(Style::default().fg(Color::Yellow))
        } else if line.contains("配对码") {
            Some(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
        } else if line.starts_with("> ") {
            Some(Style::default().fg(Color::DarkGray))
        } else {
            None
        }
    }
}

/// tracing 控制台层的 writer：日志始终进缓冲区；界面未接管时同步写 stdout。
#[derive(Clone)]
pub struct ConsoleWriter {
    buffer: LogBuffer,
}

impl std::io::Write for ConsoleWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.buffer.push_chunk(buf);
        if !TUI_ACTIVE.load(Ordering::Relaxed) {
            std::io::stdout().lock().write_all(buf)?;
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if !TUI_ACTIVE.load(Ordering::Relaxed) {
            std::io::stdout().flush()?;
        }
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for ConsoleWriter {
    type Writer = ConsoleWriter;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// 交互终端下初始化日志缓冲并返回控制台 writer；非交互环境返回 None。
pub fn init(interactive: bool) -> Option<ConsoleWriter> {
    if !interactive {
        return None;
    }
    let buffer = LogBuffer::default();
    CONSOLE.set(buffer.clone()).ok();
    Some(ConsoleWriter { buffer })
}

/// 控制台专用输出（如首次配对码）：进入界面日志区但不落盘；无界面时退化为 println。
pub fn console_line(message: String) {
    if let Some(buffer) = CONSOLE.get() {
        buffer.push_line(message.clone());
    }
    if !TUI_ACTIVE.load(Ordering::Relaxed) {
        // 直写 stdout 时高亮配对码等重点行；TUI 内由 render_log_area 按级别着色。
        println!("{}", crate::app::term_style::code_line(&message));
    }
}

pub struct TuiHandle {
    thread: Option<std::thread::JoinHandle<()>>,
}

impl TuiHandle {
    pub fn join(mut self) {
        let Some(thread) = self.thread.take() else {
            return;
        };
        if thread.join().is_err() {
            TUI_ACTIVE.store(false, Ordering::Relaxed);
            tracing::warn!("终端界面线程 panic，终端状态可能未完整恢复");
        }
    }
}

/// 启动终端界面线程；返回 None 表示未启用（调用方应回退到普通 stdin 命令循环）。
pub fn start(state: SharedState) -> Option<TuiHandle> {
    let buffer = CONSOLE.get().cloned()?;
    let initial_mode = StartupState::load(&state.infra.paths.data_dir).terminal_mode;
    let log_dir = state.infra.paths.data_dir.join("logs");
    let handle = tokio::runtime::Handle::current();
    let spawned = std::thread::Builder::new()
        .name("terminal-ui".to_string())
        .spawn(move || {
            TUI_ACTIVE.store(true, Ordering::Relaxed);
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run(&state, &buffer, &handle, initial_mode, log_dir)
            }));
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    tracing::warn!(
                        raw_os_error = ?error.raw_os_error(),
                        error_kind = ?error.kind(),
                        %error,
                        "终端界面异常退出，正在停止服务以释放实例锁"
                    );
                    state.infra.cancellation.cancel();
                }
                Err(panic) => {
                    let message = panic
                        .downcast_ref::<&str>()
                        .copied()
                        .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
                        .unwrap_or("非字符串 panic payload");
                    tracing::error!(
                        panic = message,
                        "终端界面线程 panic，正在停止服务以释放实例锁"
                    );
                    state.infra.cancellation.cancel();
                }
            }
            TUI_ACTIVE.store(false, Ordering::Relaxed);
        });
    spawned.ok().map(|thread| TuiHandle {
        thread: Some(thread),
    })
}

/// 无论正常退出还是 panic，都恢复终端状态，避免用户 shell 卡在原始模式。
struct TerminalGuard {
    buffer: LogBuffer,
    log_dir: PathBuf,
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        // Drop 中无法向上传播错误，终端恢复失败只能忽略（进程即将退出，终端可能已关闭）
        crossterm::execute!(
            std::io::stdout(),
            crossterm::event::DisableMouseCapture,
            crossterm::terminal::LeaveAlternateScreen,
            crossterm::cursor::Show
        )
        .ok();
        crossterm::terminal::disable_raw_mode().ok();
        TUI_ACTIVE.store(false, Ordering::Relaxed);
        let summary = self.buffer.exit_summary(&self.log_dir);
        let mut stdout = std::io::stdout().lock();
        // 退出摘要中的警告/错误按级别着色，退出后仍能一眼定位重点。
        let styled = summary
            .lines()
            .map(crate::app::term_style::log_line)
            .collect::<Vec<_>>()
            .join("\n");
        std::io::Write::write_all(&mut stdout, styled.as_bytes()).ok();
        std::io::Write::write_all(&mut stdout, b"\n").ok();
        std::io::Write::flush(&mut stdout).ok();
    }
}

/// Win10 传统控制台可能仍使用系统代码页（例如 936），而 ratatui 输出的是 UTF-8。
/// 只在 TUI 生命周期内切换到 UTF-8，退出时恢复用户原来的控制台设置。
struct ConsoleCodePageGuard {
    previous_output_code_page: Option<u32>,
}

impl ConsoleCodePageGuard {
    fn new() -> Self {
        Self {
            previous_output_code_page: set_utf8_output_code_page(),
        }
    }
}

impl Drop for ConsoleCodePageGuard {
    fn drop(&mut self) {
        restore_output_code_page(self.previous_output_code_page);
    }
}

fn set_utf8_output_code_page() -> Option<u32> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Console::{GetConsoleOutputCP, SetConsoleOutputCP};

        // A redirected stdout has no console code page; leave it untouched.
        let previous = unsafe { GetConsoleOutputCP() };
        if previous == 0 || previous == 65001 {
            return None;
        }
        let changed = unsafe { SetConsoleOutputCP(65001) } != 0;
        changed.then_some(previous)
    }
    #[cfg(not(windows))]
    {
        None
    }
}

fn restore_output_code_page(previous: Option<u32>) {
    #[cfg(windows)]
    if let Some(code_page) = previous {
        use windows_sys::Win32::System::Console::SetConsoleOutputCP;
        unsafe {
            let _ = SetConsoleOutputCP(code_page);
        }
    }
}

fn annotate_io_error(stage: &'static str, error: std::io::Error) -> std::io::Error {
    let raw = error
        .raw_os_error()
        .map_or_else(|| "none".to_string(), |value| value.to_string());
    std::io::Error::new(
        error.kind(),
        format!("TUI {stage}失败: {error} (raw_os_error={raw})"),
    )
}

fn run(
    state: &SharedState,
    buffer: &LogBuffer,
    handle: &tokio::runtime::Handle,
    initial_mode: TerminalMode,
    log_dir: PathBuf,
) -> std::io::Result<()> {
    let _console_code_page = ConsoleCodePageGuard::new();
    crossterm::terminal::enable_raw_mode()
        .map_err(|error| annotate_io_error("启用 raw mode", error))?;
    let _guard = TerminalGuard {
        buffer: buffer.clone(),
        log_dir,
    };
    // 备用屏下终端原生滚动失效，捕获鼠标以支持滚轮翻阅历史日志
    crossterm::execute!(
        std::io::stdout(),
        crossterm::terminal::EnterAlternateScreen,
        crossterm::event::EnableMouseCapture
    )
    .map_err(|error| annotate_io_error("进入备用屏", error))?;
    let backend = ratatui::backend::CrosstermBackend::new(std::io::stdout());
    let mut terminal = ratatui::Terminal::new(backend)
        .map_err(|error| annotate_io_error("创建终端后端", error))?;

    let mut input = String::new();
    let mut history: Vec<String> = Vec::new();
    let mut history_index: Option<usize> = None;
    // 日志区上翻偏移（距底部行数），绘制时按总行数钳位
    let mut scroll: usize = 0;
    let mut current_mode = initial_mode;

    while !state.infra.cancellation.is_cancelled() {
        terminal
            .draw(|frame| draw(frame, buffer, &input, &mut scroll, current_mode, state))
            .map_err(|error| annotate_io_error("绘制界面", error))?;
        if !crossterm::event::poll(Duration::from_millis(150))
            .map_err(|error| annotate_io_error("轮询输入", error))?
        {
            continue;
        }
        let event =
            crossterm::event::read().map_err(|error| annotate_io_error("读取输入", error))?;
        if let Event::Mouse(mouse) = &event {
            match mouse.kind {
                MouseEventKind::ScrollUp => scroll = scroll.saturating_add(3),
                MouseEventKind::ScrollDown => scroll = scroll.saturating_sub(3),
                _ => {}
            }
            continue;
        }
        let Event::Key(key) = event else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if input.is_empty() {
                    // 空输入 Ctrl+C = 退出服务；有内容时先清空输入，防误触关停。
                    state.infra.cancellation.cancel();
                } else {
                    input.clear();
                    history_index = None;
                }
            }
            KeyCode::PageUp => scroll = scroll.saturating_add(10),
            KeyCode::PageDown => scroll = scroll.saturating_sub(10),
            KeyCode::End => scroll = 0,
            KeyCode::Enter => {
                let line = std::mem::take(&mut input);
                history_index = None;
                scroll = 0;
                if !line.trim().is_empty() {
                    history.push(line.trim().to_string());
                }
                // 检查是否为 mode 切换命令
                let trimmed = line.trim();
                if trimmed == "mode" {
                    current_mode = match current_mode {
                        TerminalMode::Web => TerminalMode::Terminal,
                        TerminalMode::Terminal => TerminalMode::Web,
                    };
                    // 持久化模式选择
                    let _ =
                        StartupState::save_terminal_mode(&state.infra.paths.data_dir, current_mode);
                    let mode_text = match current_mode {
                        TerminalMode::Web => "Web 模式",
                        TerminalMode::Terminal => "终端模式",
                    };
                    buffer.push_line(format!("已切换到{mode_text}"));
                } else {
                    submit(state, buffer, handle, &line, current_mode);
                }
            }
            KeyCode::Backspace => {
                input.pop();
            }
            KeyCode::Esc => {
                input.clear();
                history_index = None;
            }
            KeyCode::Up if !history.is_empty() => {
                let next = history_index.map_or(history.len() - 1, |i| i.saturating_sub(1));
                history_index = Some(next);
                input = history[next].clone();
            }
            KeyCode::Down => {
                if let Some(index) = history_index {
                    if index + 1 < history.len() {
                        history_index = Some(index + 1);
                        input = history[index + 1].clone();
                    } else {
                        history_index = None;
                        input.clear();
                    }
                }
            }
            KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                input.push(ch);
            }
            _ => {}
        }
    }
    Ok(())
}

/// 命令异步执行。
///
/// - **Web 模式**：直接输入视为专家命令，剥离 `>` 前缀后以人工来源执行。
/// - **终端模式**：所有命令直接输入，并以人工来源执行。
fn submit(
    state: &SharedState,
    buffer: &LogBuffer,
    handle: &tokio::runtime::Handle,
    line: &str,
    mode: TerminalMode,
) {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return;
    }
    // Web 模式下，`>` 前缀可选——直接输入也视为专家命令
    let stripped = match mode {
        TerminalMode::Web => control::strip_expert_prefix(trimmed),
        TerminalMode::Terminal => trimmed,
    };
    buffer.push_line(format!("> {stripped}"));
    let args = control::split_command(stripped);
    let state = state.clone();
    let buffer = buffer.clone();
    handle.spawn(async move {
        match control::execute_from(&state, &args, control::CommandOrigin::HumanTerminal).await {
            Ok(value) => {
                for line in control::format_response(&value).lines() {
                    buffer.push_line(line.to_string());
                }
            }
            Err(error) => buffer.push_line(format!("命令失败：{error}")),
        }
    });
}

fn draw(
    frame: &mut ratatui::Frame,
    buffer: &LogBuffer,
    input: &str,
    scroll: &mut usize,
    mode: TerminalMode,
    state: &SharedState,
) {
    match mode {
        TerminalMode::Web => draw_web_mode(frame, buffer, input, scroll, state),
        TerminalMode::Terminal => draw_terminal_mode(frame, buffer, input, scroll),
    }
}

/// Web 模式：日志区 + 状态栏 + 输入框（无帮助面板）。
fn draw_web_mode(
    frame: &mut ratatui::Frame,
    buffer: &LogBuffer,
    input: &str,
    scroll: &mut usize,
    state: &SharedState,
) {
    let [log_area, status_area, input_area] = Layout::vertical([
        Constraint::Min(3),
        Constraint::Length(3),
        Constraint::Length(3),
    ])
    .areas(frame.area());

    // 日志区
    render_log_area(frame, buffer, log_area, scroll);

    // 状态栏：URL + 模式提示
    let main_url = crate::app::server::main_url(state);
    let mode_text = match state.bili.security.current().mode {
        crate::services::security_config::AccessMode::Local => "本机",
        crate::services::security_config::AccessMode::Lan => "局域网",
        crate::services::security_config::AccessMode::Proxy => "代理",
    };
    let ai_text = if state
        .infra
        .ai_skill_enabled
        .load(std::sync::atomic::Ordering::Relaxed)
    {
        "AI: 已启用"
    } else {
        "AI: 未启用"
    };
    let status_line = Line::from(vec![
        Span::styled(
            format!(" 网页管理: {main_url} "),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(" | {mode_text} | {ai_text} | ")),
        Span::styled(
            "输入命令或 mode 切换终端模式",
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(status_line)
            .block(Block::default().borders(Borders::ALL).title(" 补哩补哩 ")),
        status_area,
    );

    // 输入框
    render_input_area(
        frame,
        input,
        input_area,
        "Web 模式 · 直接输入命令 · Enter 执行 · mode 切换终端模式",
    );
}

/// 终端模式：日志区 + 帮助面板 + 输入框。
fn draw_terminal_mode(
    frame: &mut ratatui::Frame,
    buffer: &LogBuffer,
    input: &str,
    scroll: &mut usize,
) {
    let help_height = (TERMINAL_HELP_ITEMS.len() as u16 + 2)
        .min(frame.area().height.saturating_sub(6))
        .max(3);
    let [log_area, help_area, input_area] = Layout::vertical([
        Constraint::Min(3),
        Constraint::Length(help_height),
        Constraint::Length(3),
    ])
    .areas(frame.area());

    // 日志区
    render_log_area(frame, buffer, log_area, scroll);

    // 命令帮助面板
    let help_lines: Vec<Line> = TERMINAL_HELP_ITEMS
        .iter()
        .map(|(command, effect)| {
            Line::from(vec![
                Span::styled(format!(" {command:<20}"), Style::default().fg(Color::Cyan)),
                Span::raw(*effect),
            ])
        })
        .collect();
    frame.render_widget(
        Paragraph::new(help_lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" 可用命令 · mode 切回 Web 模式 "),
        ),
        help_area,
    );

    // 输入框
    render_input_area(
        frame,
        input,
        input_area,
        "终端模式 · Enter 执行 · Esc 清空 · ↑↓ 历史 · mode 切回 Web 模式",
    );
}

/// 渲染日志区域。
fn render_log_area(
    frame: &mut ratatui::Frame,
    buffer: &LogBuffer,
    area: ratatui::layout::Rect,
    scroll: &mut usize,
) {
    let visible = area.height as usize;
    let total = {
        let (_, total) = buffer.view(0, 0);
        total
    };
    *scroll = (*scroll).min(total.saturating_sub(visible));
    let (log_lines, _) = buffer.view(*scroll, visible);
    let mut logs: Vec<Line> = log_lines
        .iter()
        .map(|line| match LogBuffer::line_style(line) {
            Some(style) => Line::styled(line.as_str(), style),
            None => Line::raw(line.as_str()),
        })
        .collect();
    if *scroll > 0 {
        logs.push(Line::styled(
            format!(" -- 已上翻 {scroll} 行，End/滚轮下滑回到最新 -- "),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }
    frame.render_widget(Paragraph::new(logs), area);
}

/// 渲染输入区域。
fn render_input_area(
    frame: &mut ratatui::Frame,
    input: &str,
    area: ratatui::layout::Rect,
    title: &str,
) {
    let prompt = Line::from(vec![
        Span::styled(
            "> ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(input),
    ]);
    let cursor_x = area.x + 1 + prompt.width() as u16;
    frame.render_widget(
        Paragraph::new(prompt).block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" {title} ")),
        ),
        area,
    );
    frame.set_cursor_position(Position::new(
        cursor_x.min(area.x + area.width.saturating_sub(2)),
        area.y + 1,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn important_log_detection_covers_tracing_and_command_errors() {
        assert!(LogBuffer::is_important_line(
            "2026-08-10T00:00:00Z  WARN request failed"
        ));
        assert!(LogBuffer::is_important_line(
            "2026-08-10T00:00:00Z ERROR parse failed"
        ));
        assert!(LogBuffer::is_important_line("命令失败：参数无效"));
        assert!(!LogBuffer::is_important_line(
            "2026-08-10T00:00:00Z  INFO request complete"
        ));
    }

    #[test]
    fn log_line_styles_match_severity_and_credentials() {
        let styled = |line: &str| LogBuffer::line_style(line).is_some();
        assert!(styled("2026-08-10T00:00:00Z ERROR parse failed"));
        assert!(styled("2026-08-10T00:00:00Z  WARN request failed"));
        assert!(styled("命令失败：参数无效"));
        assert!(styled("首次设备配对码：ABCD-1234（10 分钟内有效）"));
        assert!(styled("> status"));
        assert!(!styled("2026-08-10T00:00:00Z  INFO request complete"));
    }

    #[test]
    fn important_log_replay_is_bounded_and_in_order() {
        let buffer = LogBuffer::default();
        buffer.push_line("2026-08-10T00:00:00Z INFO ignored".to_string());
        for index in 0..205 {
            buffer.push_line(format!("2026-08-10T00:00:00Z WARN warning-{index}"));
        }

        let important = buffer.important_lines();
        assert_eq!(important.len(), MAX_IMPORTANT_LOG_LINES);
        assert!(important
            .first()
            .is_some_and(|line| line.ends_with("warning-5")));
        assert!(important
            .last()
            .is_some_and(|line| line.ends_with("warning-204")));

        let summary = buffer.exit_summary(Path::new("data/logs"));
        assert!(!summary.contains("INFO ignored"));
        assert!(summary.contains("warning-5"));
        assert!(summary.contains("warning-204"));
        assert!(summary.contains("完整日志目录: data/logs"));
    }

    #[test]
    fn split_utf8_character_is_not_rendered_as_three_replacement_chars() {
        for split in 1..=3 {
            let buffer = LogBuffer::default();
            let bytes = "中\n".as_bytes();
            for chunk in bytes.chunks(split) {
                buffer.push_chunk(chunk);
            }
            let (lines, _) = buffer.view(0, 1);
            assert_eq!(lines, vec!["中"]);
        }
    }
}

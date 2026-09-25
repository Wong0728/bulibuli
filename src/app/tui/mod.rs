//! 服务器终端交互界面：双模式布局。
//!
//! - **Web 模式**（默认）：上方日志滚动区 + 底部状态栏（URL + 模式提示）+ 输入框。
//!   输入框始终可用，直接输入视为专家命令（`>` 前缀可选）。`mode` 切换到终端模式。
//! - **终端模式**：上方日志区 + 中间命令帮助面板 + 底部输入框。
//!   所有命令直接输入，不需要 `>` 前缀。`mode` 切回 Web 模式。
//!
//! 仅在 stdin/stdout 均为终端时启用；非交互环境（管道、服务）自动回退纯日志输出。
//! 命令始终异步执行且错误只回显不中断，保证输错命令不会卡死或影响服务运行。
//!
//! 模块划分：
//! - [`log_buffer`]：日志缓冲与控制台 writer（tracing 输出接入口）
//! - [`render`]：双模式界面绘制
//! - [`terminal`]：终端接管/恢复守卫与控制台代码页切换

mod log_buffer;
mod render;
mod terminal;

use crate::app::control;
use crate::app::onboarding::{StartupState, TerminalMode};
use crate::state::SharedState;
use ratatui::crossterm::{
    self,
    event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind},
};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::Duration;
use tracing::info;

pub use log_buffer::ConsoleWriter;
use log_buffer::LogBuffer;
use terminal::{annotate_io_error, ConsoleCodePageGuard, TerminalGuard};

/// 界面是否已接管屏幕：接管期间日志只进缓冲区，退出后恢复直写 stdout。
static TUI_ACTIVE: AtomicBool = AtomicBool::new(false);
static CONSOLE: OnceLock<LogBuffer> = OnceLock::new();

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
            .draw(|frame| render::draw(frame, buffer, &input, &mut scroll, current_mode, state))
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
                    info!("终端 Ctrl+C 触发服务关停");
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
                    // 持久化模式选择：失败可忽略，仅影响下次启动的默认界面模式。
                    StartupState::save_terminal_mode(&state.infra.paths.data_dir, current_mode)
                        .ok();
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

//! 终端接管与恢复守卫：无论正常退出还是 panic，都恢复终端状态；
//! Win10 传统控制台在 TUI 生命周期内切换 UTF-8 代码页，退出时还原。

use ratatui::crossterm;
use std::path::PathBuf;

use super::log_buffer::LogBuffer;
use super::TUI_ACTIVE;
use std::sync::atomic::Ordering;

/// 无论正常退出还是 panic，都恢复终端状态，避免用户 shell 卡在原始模式。
pub(super) struct TerminalGuard {
    pub(super) buffer: LogBuffer,
    pub(super) log_dir: PathBuf,
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
pub(super) struct ConsoleCodePageGuard {
    previous_output_code_page: Option<u32>,
}

impl ConsoleCodePageGuard {
    pub(super) fn new() -> Self {
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
            // 恢复失败可忽略：仅影响退出后新写入文本的显示编码，进程随即结束。
            SetConsoleOutputCP(code_page);
        }
    }
    #[cfg(not(windows))]
    {
        // 非 Windows 平台无控制台代码页概念，参数仅用于签名对称。
        let _previous = previous;
    }
}

pub(super) fn annotate_io_error(stage: &'static str, error: std::io::Error) -> std::io::Error {
    let raw = error
        .raw_os_error()
        .map_or_else(|| "none".to_string(), |value| value.to_string());
    std::io::Error::new(
        error.kind(),
        format!("TUI {stage}失败: {error} (raw_os_error={raw})"),
    )
}

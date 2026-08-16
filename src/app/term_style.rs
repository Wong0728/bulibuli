//! 终端纯文本输出的重点着色：让 URL、配对码、模式结果和错误警告更醒目。
//!
//! 仅在 stdout 为交互终端时启用颜色；管道、服务和重定向输出保持纯文本，
//! 避免日志文件混入转义序列。NO_COLOR 与 Windows 传统控制台兼容由 crossterm 兜底
//! （Windows 下 Display 路径自动探测/启用 VT，失败时退回 WinAPI 着色）。

use crossterm::style::{Attribute, Color, Stylize};
use std::io::IsTerminal;

fn colored() -> bool {
    std::io::stdout().is_terminal()
}

/// 重点 URL / 入口地址：青色加粗。
pub fn url(text: &str) -> String {
    if colored() {
        format!("{}", text.with(Color::Cyan).attribute(Attribute::Bold))
    } else {
        text.to_string()
    }
}

/// 配对码等一次性凭证：黄色加粗。
pub fn code(text: &str) -> String {
    if colored() {
        format!("{}", text.with(Color::Yellow).attribute(Attribute::Bold))
    } else {
        text.to_string()
    }
}

/// 正常状态 / 成功信息：绿色。
pub fn ok(text: &str) -> String {
    if colored() {
        format!("{}", text.with(Color::Green))
    } else {
        text.to_string()
    }
}

/// 警告：黄色。
pub fn warn(text: &str) -> String {
    if colored() {
        format!("{}", text.with(Color::Yellow))
    } else {
        text.to_string()
    }
}

/// 错误：红色加粗。
pub fn error(text: &str) -> String {
    if colored() {
        format!("{}", text.with(Color::Red).attribute(Attribute::Bold))
    } else {
        text.to_string()
    }
}

/// 次要提示：灰色。
pub fn dim(text: &str) -> String {
    if colored() {
        format!("{}", text.with(Color::DarkGrey))
    } else {
        text.to_string()
    }
}

/// 按级别着色单条日志行（退出摘要用）：ERROR 红、WARN 黄，其余原样。
pub fn log_line(line: &str) -> String {
    if !colored() {
        return line.to_string();
    }
    if line.contains(" ERROR ") || line.starts_with("ERROR ") {
        return format!("{}", line.with(Color::Red).attribute(Attribute::Bold));
    }
    if line.contains(" WARN ") || line.starts_with("WARN ") {
        return format!("{}", line.with(Color::Yellow));
    }
    line.to_string()
}

/// 含“配对码”的整行消息高亮（非 TUI 的 console_line 直写路径）。
pub fn code_line(line: &str) -> String {
    if !colored() || !line.contains("配对码") {
        return line.to_string();
    }
    code(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers_fall_back_to_plain_text_when_not_a_terminal() {
        // 测试进程的 stdout 通常被捕获（非终端），此时所有助手都应原样返回。
        if !std::io::stdout().is_terminal() {
            assert_eq!(url("http://x"), "http://x");
            assert_eq!(code("ABCD-1234"), "ABCD-1234");
            assert_eq!(error("boom"), "boom");
            assert_eq!(log_line("2026 ERROR boom"), "2026 ERROR boom");
        }
    }

    #[test]
    fn log_line_detection_matches_severity_markers() {
        // 仅验证判定逻辑；颜色是否启用取决于运行环境。
        let plain = |line: &str| !colored() || log_line(line) != line;
        assert!(plain("2026-08-16 ERROR something failed") || !colored());
        assert!(!log_line("2026-08-16  INFO all good").contains("\u{1b}[") || !colored());
    }
}

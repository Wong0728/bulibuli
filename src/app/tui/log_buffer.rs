//! 日志缓冲与控制台 writer：tracing 输出进环形缓冲，TUI 接管期间不直写 stdout。
//!
//! `ConsoleWriter` 同时实现 `MakeWriter`，供 `tracing_setup` 的 fmt 层使用；
//! 重要行（WARN/ERROR/命令失败）单独留档，退出时输出摘要。

use ratatui::style::{Color, Modifier, Style};
use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};

use super::TUI_ACTIVE;
use std::sync::atomic::Ordering;

pub(super) const MAX_LOG_LINES: usize = 1000;
pub(super) const MAX_IMPORTANT_LOG_LINES: usize = 200;

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
    pub(super) fn push_line(&self, line: String) {
        let mut inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        Self::store_line(&mut inner, line);
    }

    /// 接收 tracing writer 的字节流，先按完整 UTF-8 序列解码，再按行切分。
    ///
    /// `Write::write` 不保证一次写入一个完整字符；中文的三个 UTF-8 字节
    /// 可能被分到三次写入，不能对每个字节块单独调用 `from_utf8_lossy`。
    pub(super) fn push_chunk(&self, chunk: &[u8]) {
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

    pub(super) fn exit_summary(&self, log_dir: &Path) -> String {
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
    pub(super) fn view(&self, offset: usize, count: usize) -> (Vec<String>, usize) {
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
    pub(super) fn line_style(line: &str) -> Option<Style> {
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
    // 仅 tui::init 构造；字段对父模块私有可见，外部只能拿到 writer 句柄。
    pub(super) buffer: LogBuffer,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

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

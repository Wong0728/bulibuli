//! 界面绘制：Web 模式（日志区 + 状态栏 + 输入框）与终端模式（日志区 + 帮助面板 + 输入框）。

use crate::app::onboarding::TerminalMode;
use crate::state::SharedState;
use ratatui::layout::{Constraint, Layout, Position};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use super::log_buffer::LogBuffer;

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

pub(super) fn draw(
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

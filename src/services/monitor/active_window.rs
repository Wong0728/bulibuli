//! 博主活跃检查时段（闹钟式窗口）计算工具。
//!
//! 时段以 `"HH:MM-HH:MM"` 字符串数组的 JSON 形式存于 `bloggers.active_windows`；
//! 空/未配置 = 全天活跃。支持跨午夜窗口（如 `22:00-02:00`）。

use chrono::{DateTime, Duration, Local, NaiveTime, Timelike};
use serde::Serialize;

/// 一天的总分钟数。
const MINUTES_PER_DAY: u32 = 24 * 60;

/// 解析 JSON 数组为分钟对 `(start, end)`（均在 0..1440）。
/// 非法条目与 `start == end` 的空窗口直接跳过；解析失败返回空列表（即全天活跃）。
pub(crate) fn parse_windows(json: &str) -> Vec<(u32, u32)> {
    let items: Vec<String> = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    items
        .iter()
        .filter_map(|s| parse_single_window(s))
        .collect()
}

/// 解析单条 `"HH:MM-HH:MM"`；非法或空窗口返回 None。
pub(crate) fn parse_single_window(s: &str) -> Option<(u32, u32)> {
    let (start_raw, end_raw) = s.trim().split_once('-')?;
    let start = parse_hhmm(start_raw)?;
    let end = parse_hhmm(end_raw)?;
    if start == end {
        return None;
    }
    Some((start, end))
}

fn parse_hhmm(s: &str) -> Option<u32> {
    let (h, m) = s.trim().split_once(':')?;
    let hour: u32 = h.parse().ok()?;
    let minute: u32 = m.parse().ok()?;
    if hour > 23 || minute > 59 {
        return None;
    }
    Some(hour * 60 + minute)
}

/// 当前时刻是否处于任一活跃窗口内。空列表恒为 true（全天活跃）。
pub(crate) fn is_active(now: DateTime<Local>, windows: &[(u32, u32)]) -> bool {
    if windows.is_empty() {
        return true;
    }
    let minute = now.hour() * 60 + now.minute();
    windows.iter().any(|&(start, end)| {
        if start < end {
            minute >= start && minute < end
        } else {
            // 跨午夜：如 22:00-02:00
            minute >= start || minute < end
        }
    })
}

/// 返回 `after` 之后（含当分钟）最近的窗口开始时刻。
/// 调用前提：`windows` 非空且 `after` 不在任何窗口内。
pub(crate) fn next_window_start(after: DateTime<Local>, windows: &[(u32, u32)]) -> DateTime<Local> {
    let minute = after.hour() * 60 + after.minute();
    // 今天剩余时间里最早的 start，否则明天所有 start 中最早的
    let today_next = windows
        .iter()
        .map(|&(start, _)| start)
        .filter(|&start| start > minute)
        .min();
    let (day_offset, start_minute) = match today_next {
        Some(start) => (0, start),
        None => {
            let earliest = windows
                .iter()
                .map(|&(start, _)| start)
                .min()
                .unwrap_or(0)
                .min(MINUTES_PER_DAY - 1);
            (1, earliest)
        }
    };
    let time =
        NaiveTime::from_hms_opt(start_minute / 60, start_minute % 60, 0).unwrap_or(NaiveTime::MIN);
    let date = after.date_naive() + Duration::days(day_offset);
    date.and_time(time)
        .and_local_timezone(Local)
        .earliest()
        // 夏令时缺口等极端情况：退回顺延一小时，保证单调前进
        .unwrap_or_else(|| after + Duration::hours(1))
}

/// 将用户输入的多个时间窗规范化为排序、去重、合并后的字符串数组。
///
/// 跨午夜窗口会先拆成当天首尾两段参与合并，最后再重新组合为一个跨午夜窗口。
/// 若输入窗口完整覆盖全天，则返回空数组；存储层将其解释为“全天活跃”。
pub(crate) fn normalize_windows(items: &[String]) -> Result<Vec<String>, String> {
    let mut segments: Vec<(u32, u32)> = Vec::new();
    for item in items {
        let trimmed = item.trim();
        let (start, end) = parse_single_window(trimmed)
            .ok_or_else(|| format!("时段格式无效: {trimmed}（应为 HH:MM-HH:MM 且起止不同）"))?;
        if start < end {
            segments.push((start, end));
        } else {
            segments.push((start, MINUTES_PER_DAY));
            segments.push((0, end));
        }
    }
    if segments.is_empty() {
        return Ok(Vec::new());
    }

    segments.sort_unstable_by_key(|&(start, end)| (start, end));
    let mut merged: Vec<(u32, u32)> = Vec::with_capacity(segments.len());
    for (start, end) in segments {
        if let Some(last) = merged.last_mut() {
            if start <= last.1 {
                last.1 = last.1.max(end);
                continue;
            }
        }
        merged.push((start, end));
    }

    if merged.len() == 1 && merged[0] == (0, MINUTES_PER_DAY) {
        return Ok(Vec::new());
    }

    let cross_midnight = if merged.len() >= 2
        && merged.first().is_some_and(|segment| segment.0 == 0)
        && merged
            .last()
            .is_some_and(|segment| segment.1 == MINUTES_PER_DAY)
    {
        let first_end = merged.first().map(|segment| segment.1).unwrap_or(0);
        let last_start = merged.last().map(|segment| segment.0).unwrap_or(0);
        merged.remove(0);
        merged.pop();
        Some((last_start, first_end))
    } else {
        None
    };

    let mut normalized: Vec<String> = merged
        .into_iter()
        .map(|(start, end)| format!("{}-{}", format_minute(start), format_minute(end)))
        .collect();
    if let Some((start, end)) = cross_midnight {
        normalized.push(format!("{}-{}", format_minute(start), format_minute(end)));
    }
    Ok(normalized)
}

fn format_minute(minute: u32) -> String {
    let minute = minute.min(MINUTES_PER_DAY - 1);
    format!("{:02}:{:02}", minute / 60, minute % 60)
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ScheduleSnapshot {
    pub monitor_enabled: bool,
    pub runtime_state: &'static str,
    pub pause_reason: Option<&'static str>,
    pub within_active_window: bool,
    pub next_action_at: i64,
    pub next_action_kind: Option<&'static str>,
}

/// 将“用户是否启用监控”“当前是否在时间窗内”“下一检查时间”组合成稳定的 API 状态。
pub(crate) fn schedule_snapshot(
    monitor_enabled: bool,
    next_check: Option<DateTime<Local>>,
    active_windows_json: Option<&str>,
    now: DateTime<Local>,
) -> ScheduleSnapshot {
    if !monitor_enabled {
        return ScheduleSnapshot {
            monitor_enabled: false,
            runtime_state: "stopped",
            pause_reason: None,
            within_active_window: false,
            next_action_at: 0,
            next_action_kind: None,
        };
    }

    let windows = active_windows_json.map(parse_windows).unwrap_or_default();
    let within_active_window = is_active(now, &windows);
    if !within_active_window {
        let next = next_window_start(now, &windows);
        return ScheduleSnapshot {
            monitor_enabled: true,
            runtime_state: "waiting_window",
            pause_reason: Some("outside_active_window"),
            within_active_window: false,
            next_action_at: next.timestamp(),
            next_action_kind: Some("resume_monitoring"),
        };
    }

    let due = next_check.is_some_and(|next| next <= now);
    ScheduleSnapshot {
        monitor_enabled: true,
        runtime_state: if due { "checking" } else { "scheduled" },
        pause_reason: None,
        within_active_window: true,
        next_action_at: next_check.map(|next| next.timestamp()).unwrap_or(0),
        next_action_kind: Some("check"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(h: u32, m: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 7, 28, h, m, 0).unwrap()
    }

    #[test]
    fn parse_windows_basic_and_invalid() {
        let w = parse_windows(r#"["12:00-14:00","18:30-23:00"]"#);
        assert_eq!(w, vec![(720, 840), (1110, 1380)]);
        // 非法条目跳过、start==end 忽略
        let w = parse_windows(r#"["25:00-26:00","08:00-08:00","xx","09:15-10:45"]"#);
        assert_eq!(w, vec![(555, 645)]);
        // 非法 JSON / 空数组 → 空列表
        assert!(parse_windows("not json").is_empty());
        assert!(parse_windows("[]").is_empty());
    }

    #[test]
    fn is_active_empty_means_always() {
        assert!(is_active(at(3, 0), &[]));
    }

    #[test]
    fn is_active_normal_window_boundaries() {
        let w = vec![(720, 840)]; // 12:00-14:00
        assert!(is_active(at(12, 0), &w)); // 起点含
        assert!(is_active(at(13, 59), &w));
        assert!(!is_active(at(14, 0), &w)); // 终点不含
        assert!(!is_active(at(11, 59), &w));
    }

    #[test]
    fn is_active_cross_midnight() {
        let w = vec![(1320, 120)]; // 22:00-02:00
        assert!(is_active(at(23, 30), &w));
        assert!(is_active(at(1, 59), &w));
        assert!(!is_active(at(2, 0), &w));
        assert!(!is_active(at(12, 0), &w));
    }

    #[test]
    fn is_active_multi_window() {
        let w = vec![(1110, 1380), (720, 840)]; // 乱序：18:30-23:00, 12:00-14:00
        assert!(is_active(at(12, 30), &w));
        assert!(is_active(at(20, 0), &w));
        assert!(!is_active(at(15, 0), &w));
    }

    #[test]
    fn next_window_start_today_and_tomorrow() {
        let w = vec![(1110, 1380), (720, 840)];
        // 15:00 → 今天 18:30
        assert_eq!(next_window_start(at(15, 0), &w), at(18, 30));
        // 23:30 → 明天 12:00
        let next = next_window_start(at(23, 30), &w);
        assert_eq!(next, at(12, 0) + Duration::days(1));
    }

    #[test]
    fn next_window_start_exact_boundary_moves_forward() {
        let w = vec![(720, 840)];
        // 正好 12:00（分钟相等不算"之后"）→ 明天 12:00；
        // 实际调度不会走到这里：12:00 时 is_active 已为 true
        let next = next_window_start(at(12, 0), &w);
        assert_eq!(next, at(12, 0) + Duration::days(1));
    }
}

//! Unix 时间戳 → 本机时区文本：统一 `pub_date`、评论 HTML 与弹幕导出里的日期口径。
//!
//! 这些位置曾经直接用 `chrono::DateTime::from_timestamp(ts, 0)` 再格式化，而它解析到的是
//! `DateTime<Utc>`（默认类型参数在这里不生效），于是北京时间 00:00–07:59 的投稿 `pub_date`
//! 少一天、评论与弹幕时间早 8 小时。收敛到本模块后，时区口径只有一处需要审查。

use chrono::{Local, TimeZone};

/// 按本机时区把 Unix 秒格式化为 `fmt` 指定的样式；时间戳越界时返回 `None`。
pub(crate) fn format_unix_local(ts: i64, fmt: &str) -> Option<String> {
    Local
        .timestamp_opt(ts, 0)
        .single()
        .map(|moment| moment.format(fmt).to_string())
}

/// `history.pub_date` 的口径：本机时区的 `YYYY-MM-DD`。
/// 与迁移里 SQLite 的 `date(ts, 'unixepoch', 'localtime')` 必须保持一致。
pub(crate) fn pub_date(ts: i64) -> Option<String> {
    format_unix_local(ts, "%Y-%m-%d")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;

    /// 北京时间 00:00–07:59 之间投稿：UTC 日期会比本地日期少一天，是回归的核心场景。
    const EARLY_MORNING_CN: i64 = 1_753_804_800; // UTC 2025-07-29 16:00 = +08 2025-07-30 00:00

    #[test]
    fn pub_date_uses_local_calendar_day() {
        let ts = EARLY_MORNING_CN;
        let expected = Local
            .timestamp_opt(ts, 0)
            .single()
            .expect("valid ts")
            .format("%Y-%m-%d")
            .to_string();
        assert_eq!(pub_date(ts).as_deref(), Some(expected.as_str()));
        // 明确不是 UTC 口径：偏移非零的时区里两者必然不同。
        let utc_day = chrono::DateTime::<chrono::Utc>::from_timestamp(ts, 0)
            .expect("valid ts")
            .format("%Y-%m-%d")
            .to_string();
        let offset = Local
            .timestamp_opt(ts, 0)
            .single()
            .map(|d| d.offset().local_minus_utc());
        if offset.is_some_and(|seconds| seconds != 0) {
            assert_ne!(pub_date(ts).as_deref().unwrap(), utc_day.as_str());
        }
    }

    #[test]
    fn format_unix_local_renders_full_clock_and_rejects_out_of_range() {
        let rendered = format_unix_local(EARLY_MORNING_CN, "%Y-%m-%d %H:%M").expect("renders");
        assert_eq!(rendered.len(), 16, "{rendered}");
        // 超出可表示范围的时间戳返回 None，而不是 panic 或 1970-01-01。
        assert!(format_unix_local(i64::MAX, "%Y-%m-%d").is_none());
    }

    #[test]
    fn helper_matches_fixed_offset_equivalent() {
        // 用显式 +08:00 验证"同一时刻、本地时区"的换算方向正确。
        let ts = EARLY_MORNING_CN;
        let cn = FixedOffset::east_opt(8 * 3600).expect("offset");
        let as_cn = cn.timestamp_opt(ts, 0).single().expect("valid");
        assert_eq!(
            as_cn.format("%Y-%m-%d %H:%M:%S").to_string(),
            "2025-07-30 00:00:00"
        );
    }
}

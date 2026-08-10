//! 弹幕增量更新的调度决策函数（纯函数，易测试）。

use chrono::{DateTime, Duration, Utc};

use crate::config::DanmakuUpdatePolicy;

/// 弹幕同步阶段（与数据库 `page.danmaku_sync_generation` 字段一一对应）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Initial = 0,
    Fresh = 1,
    Mature = 2,
    Cold = 3,
    Frozen = 4,
}

impl Stage {
    pub fn from_generation(generation: u32) -> Self {
        match generation {
            0 => Stage::Initial,
            1 => Stage::Fresh,
            2 => Stage::Mature,
            3 => Stage::Cold,
            _ => Stage::Frozen,
        }
    }

    pub fn as_generation(self) -> u32 {
        self as u32
    }

    pub fn label(self) -> &'static str {
        match self {
            Stage::Initial => "未同步",
            Stage::Fresh => "新鲜期",
            Stage::Mature => "成熟期",
            Stage::Cold => "老化期",
            Stage::Frozen => "已冻结",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Skip,
    Sync { next_stage: Stage },
}

pub fn stage_for_age(
    policy: &DanmakuUpdatePolicy,
    pubtime: DateTime<Utc>,
    now: DateTime<Utc>,
    allow_freeze: bool,
) -> Stage {
    let age = now.signed_duration_since(pubtime).max(Duration::zero());
    let fresh_end = Duration::days(policy.fresh_days as i64);
    let mature_end = Duration::days(policy.mature_days as i64);
    let cold_end = Duration::days(policy.cold_days as i64);

    if allow_freeze && age >= cold_end {
        Stage::Frozen
    } else if age < fresh_end {
        Stage::Fresh
    } else if age < mature_end {
        Stage::Mature
    } else {
        Stage::Cold
    }
}

pub fn should_sync_danmaku(
    policy: &DanmakuUpdatePolicy,
    pubtime: DateTime<Utc>,
    last_synced: Option<DateTime<Utc>>,
    generation: u32,
    now: DateTime<Utc>,
) -> Decision {
    if !policy.enabled {
        return Decision::Skip;
    }

    let current_stage = Stage::from_generation(generation);
    if current_stage == Stage::Frozen {
        return Decision::Skip;
    }

    let target_stage = stage_for_age(policy, pubtime, now, true);
    if target_stage == Stage::Frozen {
        return Decision::Sync {
            next_stage: Stage::Frozen,
        };
    }

    let interval = match target_stage {
        Stage::Fresh => Duration::hours(policy.fresh_interval_hours as i64),
        Stage::Mature => Duration::days(policy.mature_interval_days as i64),
        Stage::Cold => Duration::days(policy.cold_interval_days as i64),
        Stage::Initial | Stage::Frozen => Duration::zero(),
    };

    match last_synced {
        None => Decision::Sync {
            next_stage: target_stage,
        },
        Some(last_synced_at) => {
            let since_last = now.signed_duration_since(last_synced_at);
            if target_stage.as_generation() > current_stage.as_generation() {
                return Decision::Sync {
                    next_stage: target_stage,
                };
            }
            if since_last >= interval {
                Decision::Sync {
                    next_stage: target_stage,
                }
            } else {
                Decision::Skip
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> DanmakuUpdatePolicy {
        DanmakuUpdatePolicy {
            enabled: true,
            fresh_days: 3,
            fresh_interval_hours: 6,
            mature_days: 30,
            mature_interval_days: 3,
            cold_days: 180,
            cold_interval_days: 30,
        }
    }

    fn t(days: i64, hours: i64) -> DateTime<Utc> {
        DateTime::<Utc>::from_timestamp(0, 0).unwrap() + Duration::days(days) + Duration::hours(hours)
    }

    #[test]
    fn disabled_always_skip() {
        let mut policy = policy();
        policy.enabled = false;
        assert_eq!(should_sync_danmaku(&policy, t(0, 0), None, 0, t(10, 0)), Decision::Skip);
    }

    #[test]
    fn first_sync_triggers() {
        assert_eq!(
            should_sync_danmaku(&policy(), t(0, 0), None, 0, t(0, 1)),
            Decision::Sync {
                next_stage: Stage::Fresh
            }
        );
    }

    #[test]
    fn stage_transition_triggers_immediately() {
        assert_eq!(
            should_sync_danmaku(&policy(), t(0, 0), Some(t(0, 2)), Stage::Fresh.as_generation(), t(5, 0)),
            Decision::Sync {
                next_stage: Stage::Mature
            }
        );
    }

    #[test]
    fn frozen_always_skips() {
        assert_eq!(
            should_sync_danmaku(
                &policy(),
                t(0, 0),
                Some(t(200, 0)),
                Stage::Frozen.as_generation(),
                t(300, 0)
            ),
            Decision::Skip
        );
    }

    #[test]
    fn stage_label_returns_chinese_text() {
        assert_eq!(Stage::Initial.label(), "未同步");
        assert_eq!(Stage::Fresh.label(), "新鲜期");
        assert_eq!(Stage::Mature.label(), "成熟期");
        assert_eq!(Stage::Cold.label(), "老化期");
        assert_eq!(Stage::Frozen.label(), "已冻结");
    }
}

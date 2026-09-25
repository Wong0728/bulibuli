//! 事件去重：有界的 `SeenKeys` 集合与历史回填 / 实时弹幕共用的去重 key 构造。

use std::collections::{HashSet, VecDeque};

const MAX_SEEN_EVENT_KEYS: usize = 20_000;

/// 去重集合：HashSet 判重 + VecDeque 记录插入顺序。
/// 满容量时按最旧插入淘汰——此前按 HashSet 迭代序随机淘汰，
/// 可能淘汰刚插入的 key 使去重立即失效。
#[derive(Default)]
pub(super) struct SeenKeys {
    set: HashSet<String>,
    order: VecDeque<String>,
}

impl SeenKeys {
    /// 插入并返回是否为新 key。容量超限时先淘汰最旧条目。
    pub(super) fn insert(&mut self, key: String) -> bool {
        if self.set.len() >= MAX_SEEN_EVENT_KEYS {
            while let Some(oldest) = self.order.pop_front() {
                self.set.remove(&oldest);
                if self.set.len() < MAX_SEEN_EVENT_KEYS {
                    break;
                }
            }
        }
        // 只有新 key 才进淘汰队列：重复 key 不使 set.len() 增长，却会让 order 无界
        // 追加克隆字符串——刷屏场次下队列永远不会触发淘汰，等于无界内存增长。
        if !self.set.insert(key.clone()) {
            return false;
        }
        self.order.push_back(key);
        true
    }
}

/// 历史回填 / 实时弹幕共用的去重 key：优先使用服务端 id，缺失时退回
/// 「用户 + 文本 + 3 秒时间桶」的近似 key。
pub(super) fn history_key(value: &serde_json::Value, uid: i64, text: &str) -> String {
    if let Some(id) = value
        .get("id_str")
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            value
                .pointer("/data/id_str")
                .and_then(serde_json::Value::as_str)
        })
    {
        return format!("id:{id}");
    }
    // 无服务端 id 时的近似去重：加宽时间桶到 3 秒，降低同用户短时间重发相同
    // 弹幕被误丢的概率（此前 10 秒桶在刷屏场景明显丢计数）。
    format!("msg:{uid}:{text}:{}", chrono::Utc::now().timestamp() / 3)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_keys_do_not_grow_the_eviction_queue() {
        // 刷屏场次里同一条弹幕会反复到达：重复 key 不进 set，若仍追加 order，
        // set.len() 永不触及上限、淘汰永不触发，order 便无界增长。
        let mut seen = SeenKeys::default();
        assert!(seen.insert("danmaku-duplicate".to_string()));
        for _ in 0..(MAX_SEEN_EVENT_KEYS * 3) {
            assert!(!seen.insert("danmaku-duplicate".to_string()));
        }
        assert_eq!(seen.set.len(), 1);
        assert_eq!(seen.order.len(), 1);
    }

    #[test]
    fn distinct_keys_stay_bounded_and_report_newness() {
        let mut seen = SeenKeys::default();
        assert!(seen.insert("first".to_string()));
        assert!(!seen.insert("first".to_string()));
        for index in 0..(MAX_SEEN_EVENT_KEYS * 2) {
            seen.insert(format!("danmaku-{index}"));
        }
        assert!(
            seen.set.len() <= MAX_SEEN_EVENT_KEYS,
            "集合越界: {}",
            seen.set.len()
        );
        assert!(
            seen.order.len() <= MAX_SEEN_EVENT_KEYS,
            "淘汰队列越界: {}",
            seen.order.len()
        );
    }
}

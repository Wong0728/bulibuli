use std::sync::{Arc, Mutex, MutexGuard};
use tokio::sync::Notify;

struct GateState {
    active: usize,
    limit: usize,
}

#[derive(Clone)]
pub struct ConcurrencyGate {
    state: Arc<Mutex<GateState>>,
    notify: Arc<Notify>,
}

impl ConcurrencyGate {
    pub fn new(limit: usize) -> Self {
        Self {
            state: Arc::new(Mutex::new(GateState {
                active: 0,
                limit: limit.max(1),
            })),
            notify: Arc::new(Notify::new()),
        }
    }

    /// 调整并发上限。返回 `true` 表示上限确实发生变化（调用方据此把新值同步给 aria2）。
    pub async fn set_limit(&self, limit: usize) -> bool {
        let limit = limit.max(1);
        let changed = {
            let mut state = self.lock_state();
            let changed = state.limit != limit;
            state.limit = limit;
            changed
        };
        if changed {
            self.notify.notify_waiters();
        }
        changed
    }

    pub async fn acquire(&self) -> ConcurrencyPermit {
        loop {
            // 在持锁期间创建 Notified 未来，避免释放锁到 await 之间丢失唤醒；
            // 临界区仅整型比较/自增，无 await，故用 std::sync::Mutex 即可。
            let notified = {
                let mut state = self.lock_state();
                if state.active < state.limit {
                    state.active += 1;
                    return ConcurrencyPermit { gate: self.clone() };
                }
                self.notify.notified()
            };
            notified.await;
        }
    }

    pub async fn acquire_timeout(&self, timeout: std::time::Duration) -> Option<ConcurrencyPermit> {
        tokio::time::timeout(timeout, self.acquire()).await.ok()
    }

    /// 同步释放许可，保证 `Drop` 不依赖仍在运行的 Tokio runtime。
    fn release(&self) {
        {
            let mut state = self.lock_state();
            state.active = state.active.saturating_sub(1);
        }
        self.notify.notify_one();
    }

    /// 获取状态锁并自动处理毒化：临界区仅整型读写不会 panic，即便毒化也取回内部数据继续。
    fn lock_state(&self) -> MutexGuard<'_, GateState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[cfg(test)]
    fn counts(&self) -> (usize, usize) {
        let state = self.lock_state();
        (state.active, state.limit)
    }
}

pub struct ConcurrencyPermit {
    gate: ConcurrencyGate,
}

impl Drop for ConcurrencyPermit {
    fn drop(&mut self) {
        self.gate.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn adjusts_limit_without_losing_active_permits() {
        let gate = ConcurrencyGate::new(2);
        let first = gate.acquire().await;
        let second = gate.acquire().await;
        assert_eq!(gate.counts(), (2, 2));
        gate.set_limit(1).await;
        assert_eq!(gate.counts(), (2, 1));
        drop(first);
        drop(second);
        tokio::task::yield_now().await;
        let third = gate.acquire().await;
        assert_eq!(gate.counts(), (1, 1));
        drop(third);
    }
}

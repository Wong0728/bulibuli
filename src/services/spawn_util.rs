//! fire-and-forget 任务 spawn 工具：统一捕获并记录任务 panic。
//!
//! 背景：`tokio::spawn` 的任务 panic 只会在 JoinHandle 被 await 时暴露；
//! fire-and-forget 调用点从不 await，panic 被静默吞掉（如烧录任务 panic 后
//! 状态永远停留在 processing）。所有 fire-and-forget spawn 应走本模块。

use futures::FutureExt;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::task::JoinHandle;
use tracing::error;

/// 包装 `tokio::spawn`：任务 panic 时 `tracing::error!` 记录任务名与 panic 内容，
/// 不向外传播 panic（fire-and-forget 语义）。返回外层守护任务的句柄。
pub fn spawn_logged<F>(name: &'static str, fut: F) -> JoinHandle<()>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    spawn_logged_with_panic(name, fut, || async {})
}

/// 同 [`spawn_logged`]，额外在任务 panic 时执行一次异步清理回调
/// （例如把持久化状态置为 failed，避免永久停留在 processing）。
pub fn spawn_logged_with_panic<F, P, G>(name: &'static str, fut: F, on_panic: G) -> JoinHandle<()>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
    G: FnOnce() -> P + Send + 'static,
    P: Future<Output = ()> + Send + 'static,
{
    tokio::spawn(async move {
        // fut 内部持有 MutexGuard 等非 UnwindSafe 类型时由调用方保证 unwind 安全；
        // 这里断言仅用于跨 catch_unwind 边界，panic 后任务即结束、不复用任何状态。
        match AssertUnwindSafe(fut).catch_unwind().await {
            Ok(_) => {}
            Err(panic_payload) => {
                let message = panic_payload
                    .downcast_ref::<&str>()
                    .map(|s| (*s).to_string())
                    .or_else(|| panic_payload.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "非字符串 panic payload".to_string());
                error!(task = name, "后台任务 panic: {message}");
                on_panic().await;
            }
        }
    })
}

/// 进程级 detached task 登记器。
///
/// 这些任务通常由请求回调或后台 worker 内部创建，不能交给调用方直接 await；
/// 登记后由主流程在关闭数据库前统一等待。完成的句柄会在下一次登记时回收，
/// 避免长期运行时只因保存 JoinHandle 而增长。
#[derive(Clone, Default)]
pub struct TaskRegistry {
    state: Arc<Mutex<TaskRegistryState>>,
}

struct TaskRegistryState {
    accepting: bool,
    handles: Vec<(&'static str, JoinHandle<()>)>,
}

impl Default for TaskRegistryState {
    fn default() -> Self {
        Self {
            accepting: true,
            handles: Vec::new(),
        }
    }
}

impl TaskRegistry {
    /// 登记并启动一个进程级后台任务。
    ///
    /// 关闭流程一旦开始，后续任务不会再脱离登记器运行；返回 false 时调用方
    /// 应把刚创建的业务任务标记为失败/取消。默认值保持 accepting=true，避免
    /// 在构造阶段为每个实例再写一套初始化代码。
    pub fn spawn<F>(&self, name: &'static str, fut: F) -> bool
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.spawn_with_panic(name, fut, || async {})
    }

    /// 同 [`spawn`]，允许 panic 时执行一次业务清理回调。
    pub fn spawn_with_panic<F, P, G>(&self, name: &'static str, fut: F, on_panic: G) -> bool
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
        G: FnOnce() -> P + Send + 'static,
        P: Future<Output = ()> + Send + 'static,
    {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if !state.accepting {
            return false;
        }
        state.handles.retain(|(_, handle)| !handle.is_finished());
        state
            .handles
            .push((name, spawn_logged_with_panic(name, fut, on_panic)));
        true
    }

    /// 关闭登记器并等待所有已登记任务。
    ///
    /// 先在同一把锁下停止接收新任务，再取出句柄，消除“shutdown 已开始但另一个
    /// 请求刚好登记成功”的竞态。所有任务共享一个 10 秒截止时间；超时后逐个
    /// abort 并 await，避免把仍运行的任务带过数据库关闭。
    pub async fn shutdown(&self) {
        let handles = {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            state.accepting = false;
            std::mem::take(&mut state.handles)
        };
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        for (name, mut handle) in handles {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                error!(task = name, "登记后台任务超过统一退出截止时间，已 abort");
                handle.abort();
                let _ = handle.await;
                continue;
            }
            match tokio::time::timeout(remaining, &mut handle).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => error!(task = name, "登记后台任务退出异常: {error}"),
                Err(_) => {
                    error!(task = name, "登记后台任务未在统一截止时间内退出，已 abort");
                    handle.abort();
                    let _ = handle.await;
                }
            }
        }
    }
}

/// 等待一个由调用方持有的后台任务；超时后必须 abort 并再次 await。
/// 统一所有服务的 shutdown 语义，避免 timeout 只丢弃 JoinHandle、任务继续访问 DB。
pub async fn wait_join_handle<T>(name: &'static str, mut handle: JoinHandle<T>, timeout: Duration) {
    match tokio::time::timeout(timeout, &mut handle).await {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => error!(task = name, "后台任务退出异常: {error}"),
        Err(_) => {
            error!(task = name, "后台任务超时，已 abort");
            handle.abort();
            let _ = handle.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn normal_completion_passes_through() {
        let handle = spawn_logged("test_ok", async { 42 });
        handle.await.expect("join");
    }

    #[tokio::test]
    async fn panic_triggers_callback() {
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let handle =
            spawn_logged_with_panic("test_panic", async { panic!("boom") }, || async move {
                let _ = tx.send(());
            });
        handle.await.expect("join");
        assert!(rx.await.is_ok(), "panic 回调应被执行");
    }

    #[tokio::test]
    async fn panic_does_not_poison_caller() {
        let first = spawn_logged("test_panic_silent", async { panic!("first") });
        first.await.expect("join 应正常返回而非 Err");
        let second = spawn_logged("test_after", async { 1 });
        second.await.expect("后续任务不受影响");
    }

    #[tokio::test]
    async fn registry_closes_and_rejects_late_tasks() {
        let registry = TaskRegistry::default();
        assert!(registry.spawn("registry_task", async {}));
        registry.shutdown().await;
        assert!(!registry.spawn("late_task", async {}));
    }
}

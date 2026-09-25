//! fire-and-forget 任务 spawn 工具：统一捕获并记录任务 panic。
//!
//! 背景：`tokio::spawn` 的任务 panic 只会在 JoinHandle 被 await 时暴露；
//! fire-and-forget 调用点从不 await，panic 被静默吞掉（如烧录任务 panic 后
//! 状态永远停留在 processing）。所有 fire-and-forget spawn 应走本模块。
//!
//! 子模块：
//! - [`child_lifetime`]：外部子进程（FFmpeg/ffprobe）生命周期防护

use futures::FutureExt;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::task::JoinHandle;
use tracing::error;

pub mod child_lifetime;

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

/// 普通后台任务的统一退出预算。
pub const DEFAULT_SHUTDOWN_BUDGET: Duration = Duration::from_secs(10);

struct RegisteredTask {
    name: &'static str,
    handle: JoinHandle<()>,
    budget: Duration,
}

struct TaskRegistryState {
    accepting: bool,
    handles: Vec<RegisteredTask>,
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

    /// 同 [`spawn`]，但为任务单独声明关机预算。
    ///
    /// 用于"收尾"类任务（如直播分段合并）：普通后台任务只给 10 秒，会砍断
    /// 仍在写盘的合并；这类任务需要更长预算才能自然完成。
    pub fn spawn_with_budget<F>(&self, name: &'static str, fut: F, budget: Duration) -> bool
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.spawn_with_panic_and_budget(name, fut, || async {}, budget)
    }

    /// 同 [`spawn`]，允许 panic 时执行一次业务清理回调。
    pub fn spawn_with_panic<F, P, G>(&self, name: &'static str, fut: F, on_panic: G) -> bool
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
        G: FnOnce() -> P + Send + 'static,
        P: Future<Output = ()> + Send + 'static,
    {
        self.spawn_with_panic_and_budget(name, fut, on_panic, DEFAULT_SHUTDOWN_BUDGET)
    }

    /// 同 [`spawn_with_panic`]，额外声明关机预算。
    pub fn spawn_with_panic_and_budget<F, P, G>(
        &self,
        name: &'static str,
        fut: F,
        on_panic: G,
        budget: Duration,
    ) -> bool
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
        G: FnOnce() -> P + Send + 'static,
        P: Future<Output = ()> + Send + 'static,
    {
        let budget = if budget.is_zero() {
            DEFAULT_SHUTDOWN_BUDGET
        } else {
            budget
        };
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if !state.accepting {
            return false;
        }
        state.handles.retain(|task| !task.handle.is_finished());
        state.handles.push(RegisteredTask {
            name,
            handle: spawn_logged_with_panic(name, fut, on_panic),
            budget,
        });
        true
    }

    /// 关闭登记器并等待所有已登记任务。
    ///
    /// 先在同一把锁下停止接收新任务，再取出句柄，消除“shutdown 已开始但另一个
    /// 请求刚好登记成功”的竞态。所有任务共享一个截止时间，取登记任务里最大的
    /// 关机预算（普通任务 10 秒，收尾任务更长）；超时后逐个 abort 并 await，
    /// 避免把仍运行的任务带过数据库关闭。
    pub async fn shutdown(&self) {
        let handles = {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            state.accepting = false;
            std::mem::take(&mut state.handles)
        };
        let budget = shutdown_budget(&handles.iter().map(|task| task.budget).collect::<Vec<_>>());
        let deadline = tokio::time::Instant::now() + budget;
        for RegisteredTask {
            name,
            mut handle,
            budget: _,
        } in handles
        {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                error!(task = name, "登记后台任务超过统一退出截止时间，已 abort");
                handle.abort();
                // 任务已被 abort，Err(JoinError) 属预期结果，仅回收任务句柄
                handle.await.ok();
                continue;
            }
            match tokio::time::timeout(remaining, &mut handle).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => error!(task = name, "登记后台任务退出异常: {error}"),
                Err(_) => {
                    error!(task = name, "登记后台任务未在统一截止时间内退出，已 abort");
                    handle.abort();
                    // 任务已被 abort，Err(JoinError) 属预期结果，仅回收任务句柄
                    handle.await.ok();
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
            // 任务已被 abort，Err(JoinError) 属预期结果，仅回收任务句柄
            handle.await.ok();
        }
    }
}

/// 关机统一截止时间：取所有登记任务里最大的预算，没有任务时用默认值。
/// 单个任务不会因预算小于最大值而被提前 abort——共享截止时间是刻意设计，
/// 避免关机总时长随任务数量线性累加。
fn shutdown_budget(budgets: &[Duration]) -> Duration {
    budgets
        .iter()
        .copied()
        .max()
        .unwrap_or(DEFAULT_SHUTDOWN_BUDGET)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

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
                // 接收端同测试内必然在等待，发送失败无从处理也无需处理
                tx.send(()).ok();
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

    #[test]
    fn shutdown_budget_takes_largest_registered_budget() {
        assert_eq!(shutdown_budget(&[]), DEFAULT_SHUTDOWN_BUDGET);
        assert_eq!(
            shutdown_budget(&[Duration::from_secs(3)]),
            Duration::from_secs(3)
        );
        assert_eq!(
            shutdown_budget(&[DEFAULT_SHUTDOWN_BUDGET, Duration::from_secs(120)]),
            Duration::from_secs(120)
        );
    }

    #[tokio::test]
    async fn finalize_budget_task_is_not_aborted_early() {
        let registry = TaskRegistry::default();
        let finished = Arc::new(AtomicBool::new(false));
        let flag = finished.clone();
        assert!(registry.spawn_with_budget(
            "slow_finalize",
            async move {
                tokio::time::sleep(Duration::from_millis(300)).await;
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
            },
            Duration::from_secs(5),
        ));
        registry.shutdown().await;
        assert!(
            finished.load(std::sync::atomic::Ordering::SeqCst),
            "预算内的收尾任务不应被 abort"
        );
    }

    #[tokio::test]
    async fn task_over_budget_is_aborted_and_cleaned_up() {
        struct DropGuard(Arc<std::sync::atomic::AtomicBool>);
        impl Drop for DropGuard {
            fn drop(&mut self) {
                self.0.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }
        let dropped = Arc::new(AtomicBool::new(false));
        let guard = DropGuard(dropped.clone());
        let registry = TaskRegistry::default();
        assert!(registry.spawn_with_budget(
            "stuck_forever",
            async move {
                let _guard = guard;
                loop {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            },
            Duration::from_millis(80),
        ));
        registry.shutdown().await;
        assert!(
            dropped.load(std::sync::atomic::Ordering::SeqCst),
            "超过预算的任务必须被 abort 并释放其资源"
        );
    }

    #[tokio::test]
    async fn zero_budget_falls_back_to_default() {
        let registry = TaskRegistry::default();
        assert!(registry.spawn_with_budget("zero_budget", async {}, Duration::ZERO));
        let state = registry
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        assert_eq!(state.handles[0].budget, DEFAULT_SHUTDOWN_BUDGET);
    }
}

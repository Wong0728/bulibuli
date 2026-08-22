//! fire-and-forget 任务 spawn 工具：统一捕获并记录任务 panic。
//!
//! 背景：`tokio::spawn` 的任务 panic 只会在 JoinHandle 被 await 时暴露；
//! fire-and-forget 调用点从不 await，panic 被静默吞掉（如烧录任务 panic 后
//! 状态永远停留在 processing）。所有 fire-and-forget spawn 应走本模块。

use futures::FutureExt;
use std::future::Future;
use std::panic::AssertUnwindSafe;
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
}

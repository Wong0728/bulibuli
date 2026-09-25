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

/// 外部子进程（FFmpeg/ffprobe）生命周期防护：
/// 主进程被强杀时子进程不得变成孤儿继续写盘。
///
/// - Windows：kill-on-close Job Object（本进程句柄表在进程退出时由系统关闭，
///   Job 随之关闭即连带结束其中所有进程）+ `CREATE_NO_WINDOW`
/// - Linux/Android：`prctl(PR_SET_PDEATHSIG, SIGTERM)`，父进程被 SIGKILL 时子进程随之退出
/// - macOS：两种机制都没有，保持调用方原有的 `kill_on_drop`
pub mod child_lifetime {
    use tokio::process::{Child, Command};
    use tracing::warn;

    /// Windows：不为其分配控制台窗口（FFmpeg 系都是控制台程序）。
    pub fn configure(command: &mut Command) {
        #[cfg(windows)]
        {
            command.creation_flags(CREATE_NO_WINDOW);
        }
        // std 的 unix CommandExt 没有 pdeathsig（本工具链已无此方法），只能沿用
        // aria2 侧的 prctl 写法。SIGTERM 而非 SIGKILL：FFmpeg 收到后会写完文件尾，
        // 不再留下需要靠 #3 容错的截断分段。macOS 没有对应机制，保持 kill_on_drop。
        #[cfg(any(target_os = "android", target_os = "linux"))]
        {
            // SAFETY: 闭包只做一次 prctl 系统调用，不取锁、不分配、不阻塞，
            // 满足 fork/exec 之间的 async-signal-safety 要求。
            unsafe {
                use std::os::unix::process::CommandExt;
                command.as_std_mut().pre_exec(|| {
                    unsafe extern "C" {
                        fn prctl(option: i32, ...) -> i32;
                    }
                    const PR_SET_PDEATHSIG: i32 = 1;
                    const SIGTERM: i32 = 15;
                    if prctl(PR_SET_PDEATHSIG, SIGTERM) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
    }

    /// 设置生命周期后 spawn，并在 Windows 立即绑定 Job Object。
    pub fn spawn(command: &mut Command) -> std::io::Result<Child> {
        configure(command);
        let child = command.spawn()?;
        if !bind_to_process_job(&child) {
            warn!("子进程未绑定到 Job Object，将依赖优雅退出清理");
        }
        Ok(child)
    }

    /// Windows：把已启动的子进程绑进进程级 kill-on-close Job Object。
    /// 子进程可能已经退出，因此绑定失败只降级为告警，不算错误。
    pub fn bind_to_process_job(child: &Child) -> bool {
        #[cfg(windows)]
        {
            match child.raw_handle() {
                Some(handle) => job_object::assign(handle),
                None => true,
            }
        }
        #[cfg(not(windows))]
        {
            let _ = child;
            true
        }
    }

    /// 仅供测试：确认子进程确实落在本进程的 Job Object 里。
    #[cfg(all(test, windows))]
    pub fn is_bound_to_process_job(child: &Child) -> bool {
        child.raw_handle().is_some_and(job_object::is_member)
    }

    #[cfg(windows)]
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    #[cfg(windows)]
    mod job_object {
        //! 进程级共享的 kill-on-close Job Object。
        //! 与 `services::aria2::win_job` 同机制；那份是 aria2 私有模块，这里给
        //! FFmpeg 录制/合并/烧录复用，句柄只在创建它的进程内有效。

        use std::sync::OnceLock;
        use tracing::warn;
        use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
            SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };

        struct Job(isize);
        // SAFETY: 句柄仅用于 assign/close；close 只在 Drop 里发生一次，且
        // Win32 Job Object 句柄本身可跨线程使用。
        unsafe impl Send for Job {}
        unsafe impl Sync for Job {}

        impl Drop for Job {
            fn drop(&mut self) {
                if self.0 != 0 {
                    // SAFETY: `self.0` 由 `CreateJobObjectW` 创建且只在此处关闭一次。
                    unsafe {
                        if CloseHandle(self.0 as HANDLE) == 0 {
                            warn!("CloseHandle(Job) 失败");
                        }
                    }
                }
            }
        }

        static JOB: OnceLock<Option<Job>> = OnceLock::new();

        fn create() -> Option<Job> {
            // SAFETY: 传给 Win32 API 的指针要么按要求为空，要么指向本次调用所需的
            // 已初始化值；失败路径显式关闭已创建的句柄。
            unsafe {
                let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if job.is_null() {
                    warn!("CreateJobObjectW 返回空句柄");
                    return None;
                }
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                if SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const core::ffi::c_void,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                ) == 0
                {
                    if CloseHandle(job) == 0 {
                        warn!("清理失败的 Job Object 句柄失败");
                    }
                    warn!("SetInformationJobObject 失败");
                    return None;
                }
                Some(Job(job as isize))
            }
        }

        /// 惰性创建并绑定；Job 创建失败时记住 None，不再反复重试。
        pub fn assign(process_handle: *mut core::ffi::c_void) -> bool {
            let job = JOB.get_or_init(create);
            match job {
                Some(job) => {
                    // SAFETY: `job.0` 是有效 Job Object 句柄，`process_handle`
                    // 来自刚启动的子进程。
                    unsafe {
                        AssignProcessToJobObject(job.0 as HANDLE, process_handle as HANDLE) != 0
                    }
                }
                None => false,
            }
        }

        /// 仅供测试：进程是否属于本模块创建的那个 Job。
        #[cfg(test)]
        pub fn is_member(process_handle: *mut core::ffi::c_void) -> bool {
            use windows_sys::Win32::System::JobObjects::IsProcessInJob;
            let Some(job) = JOB.get().and_then(|job| job.as_ref()) else {
                return false;
            };
            let mut result = 0_i32;
            // SAFETY: 两个句柄分别来自已启动的子进程与本模块创建的 Job；
            // `result` 是可写的 i32 输出参数。
            unsafe {
                IsProcessInJob(process_handle as HANDLE, job.0 as HANDLE, &mut result) != 0
                    && result != 0
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    #[cfg(windows)]
    use tokio::process::Command;

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

    /// Windows 专属：真实 spawn 一个子进程，确认它确实进了 kill-on-close Job。
    #[cfg(windows)]
    #[tokio::test]
    async fn spawned_child_is_bound_to_kill_on_close_job() {
        use std::process::Stdio;
        let mut command = Command::new("cmd.exe");
        command
            .args(["/C", "ping 127.0.0.1 -n 6 > nul"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let child = child_lifetime::spawn(&mut command).expect("spawn child");
        let bound = child_lifetime::is_bound_to_process_job(&child);
        drop(child);
        assert!(bound, "spawn 出来的子进程必须已绑定到 Job Object");
    }
}

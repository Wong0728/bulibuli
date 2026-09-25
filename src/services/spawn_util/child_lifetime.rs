//! 外部子进程（FFmpeg/ffprobe）生命周期防护：
//! 主进程被强杀时子进程不得变成孤儿继续写盘。
//!
//! - Windows：kill-on-close Job Object（本进程句柄表在进程退出时由系统关闭，
//!   Job 随之关闭即连带结束其中所有进程）+ `CREATE_NO_WINDOW`
//! - Linux/Android：`prctl(PR_SET_PDEATHSIG, SIGTERM)`，父进程被 SIGKILL 时子进程随之退出
//! - macOS：两种机制都没有，保持调用方原有的 `kill_on_drop`

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
        let _child = child;
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
                unsafe { AssignProcessToJobObject(job.0 as HANDLE, process_handle as HANDLE) != 0 }
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

#[cfg(all(test, windows))]
mod tests {
    use std::process::Stdio;
    use tokio::process::Command;

    /// Windows 专属：真实 spawn 一个子进程，确认它确实进了 kill-on-close Job。
    #[tokio::test]
    async fn spawned_child_is_bound_to_kill_on_close_job() {
        let mut command = Command::new("cmd.exe");
        command
            .args(["/C", "ping 127.0.0.1 -n 6 > nul"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let child = super::spawn(&mut command).expect("spawn child");
        let bound = super::is_bound_to_process_job(&child);
        drop(child);
        assert!(bound, "spawn 出来的子进程必须已绑定到 Job Object");
    }
}

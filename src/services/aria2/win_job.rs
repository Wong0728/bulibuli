//! Windows Job Object 绑定：父进程退出（含被强杀）时连带结束 aria2c 子进程。

use tracing::warn;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};

/// 持有 Job Object 句柄。以 isize 存储以保证 Send（HANDLE 是裸指针）。
/// Drop 时关闭句柄；因设了 KILL_ON_JOB_CLOSE，本进程退出（句柄被 OS 回收）即结束 Job 内所有进程。
pub struct Job(isize);
// SAFETY: 句柄仅用于 assign/close，跨线程传递安全。
unsafe impl Send for Job {}

impl Drop for Job {
    fn drop(&mut self) {
        if self.0 != 0 {
            // SAFETY: `self.0` is a live Job Object handle created by
            // `CreateJobObjectW`; this guard owns it and closes it once.
            unsafe {
                if CloseHandle(self.0 as HANDLE) == 0 {
                    warn!("CloseHandle(Job) 失败");
                }
            }
        }
    }
}

/// 创建一个 kill-on-close 的 Job Object。
pub fn create() -> Option<Job> {
    // SAFETY: All pointers passed to the Win32 API are either null as
    // explicitly permitted or point to initialized values for the call.
    // Every non-null handle is transferred to `Job` or closed on failure.
    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            warn!("CreateJobObjectW 返回空句柄");
            return None;
        }
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let ok = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const core::ffi::c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );
        if ok == 0 {
            if CloseHandle(job) == 0 {
                warn!("清理失败的 Job Object 句柄失败");
            }
            warn!("SetInformationJobObject 失败");
            return None;
        }
        Some(Job(job as isize))
    }
}

/// 把指定进程句柄分配到 Job。返回是否成功。
pub fn assign(job: &Job, process_handle: *mut core::ffi::c_void) -> bool {
    // SAFETY: `job` owns a live Job Object handle and `process_handle`
    // comes from the spawned child's platform-specific process handle.
    unsafe { AssignProcessToJobObject(job.0 as HANDLE, process_handle as HANDLE) != 0 }
}

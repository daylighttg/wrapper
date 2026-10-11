//! OS-specific process hygiene.

/// Makes sure child processes (QEMU, the native agent) die with us even if
/// we are killed abruptly. On Linux children use `PR_SET_PDEATHSIG`; on
/// Windows we put ourselves in a job object that kills its members when its
/// last handle (ours) closes.
pub fn kill_children_on_exit() {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
            SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };
        use windows_sys::Win32::System::Threading::GetCurrentProcess;

        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return;
        }
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const _,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );
        // Fails harmlessly when we are already in a job that forbids nesting.
        AssignProcessToJobObject(job, GetCurrentProcess());
        // The handle is intentionally leaked: it must live as long as we do.
    }
}

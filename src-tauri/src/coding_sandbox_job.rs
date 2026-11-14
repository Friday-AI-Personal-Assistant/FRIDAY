//! Windows Job Object resource containment primitives for the future guest runner.
use std::{ffi::c_void, mem::{size_of, zeroed}, ptr};
use windows_sys::Win32::{
    Foundation::{CloseHandle, GetLastError, HANDLE, INVALID_HANDLE_VALUE},
    System::{
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, QueryInformationJobObject,
            SetInformationJobObject, JOBOBJECT_BASIC_LIMIT_INFORMATION,
            JOBOBJECT_CPU_RATE_CONTROL_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JobObjectCpuRateControlInformation, JobObjectExtendedLimitInformation,
            JOB_OBJECT_CPU_RATE_CONTROL_ENABLE, JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP,
            JOB_OBJECT_LIMIT_ACTIVE_PROCESS, JOB_OBJECT_LIMIT_JOB_MEMORY,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_PROCESS_MEMORY,
        },
        Threading::IsProcessInJob,
    },
};

/// Fixed resource policy. These values are never read from a guest request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GuestJobPolicy {
    pub(crate) active_process_limit: u32,
    pub(crate) per_process_memory_bytes: usize,
    pub(crate) job_memory_bytes: usize,
    /// CPU hard cap in 1/100th-percent units (1000 = 10%).
    pub(crate) cpu_rate: u32,
}
pub(crate) const GUEST_JOB_POLICY: GuestJobPolicy = GuestJobPolicy {
    active_process_limit: 16,
    per_process_memory_bytes: 256 * 1024 * 1024,
    job_memory_bytes: 512 * 1024 * 1024,
    cpu_rate: 2_000,
};

#[derive(Debug)]
pub(crate) struct OwnedGuestJob { handle: HANDLE }

impl OwnedGuestJob {
    /// Create an unnamed job, apply all mandatory limits, and verify kernel state.
    pub(crate) fn create() -> Result<Self, String> {
        let handle = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return Err(last_error("create the private guest Job Object"));
        }
        let job = Self { handle };
        job.configure_and_verify()?;
        Ok(job)
    }

    fn configure_and_verify(&self) -> Result<(), String> {
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
        limits.BasicLimitInformation = JOBOBJECT_BASIC_LIMIT_INFORMATION {
            LimitFlags: JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
                | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
                | JOB_OBJECT_LIMIT_PROCESS_MEMORY
                | JOB_OBJECT_LIMIT_JOB_MEMORY,
            ..unsafe { zeroed() }
        };
        limits.BasicLimitInformation.ActiveProcessLimit = GUEST_JOB_POLICY.active_process_limit;
        limits.ProcessMemoryLimit = GUEST_JOB_POLICY.per_process_memory_bytes;
        limits.JobMemoryLimit = GUEST_JOB_POLICY.job_memory_bytes;
        set_info(self.handle, JobObjectExtendedLimitInformation,
            &limits as *const _ as *const c_void,
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>(),
            "apply process-count and memory limits")?;

        let mut cpu: JOBOBJECT_CPU_RATE_CONTROL_INFORMATION = unsafe { zeroed() };
        cpu.ControlFlags = JOB_OBJECT_CPU_RATE_CONTROL_ENABLE | JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP;
        unsafe { cpu.Anonymous.CpuRate = GUEST_JOB_POLICY.cpu_rate; }
        set_info(self.handle, JobObjectCpuRateControlInformation,
            &cpu as *const _ as *const c_void,
            size_of::<JOBOBJECT_CPU_RATE_CONTROL_INFORMATION>(),
            "apply the CPU hard cap")?;

        let mut actual: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
        query_info(self.handle, JobObjectExtendedLimitInformation,
            &mut actual as *mut _ as *mut c_void,
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>(),
            "verify process-count and memory limits")?;
        let required = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
            | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
            | JOB_OBJECT_LIMIT_PROCESS_MEMORY
            | JOB_OBJECT_LIMIT_JOB_MEMORY;
        if actual.BasicLimitInformation.LimitFlags & required != required
            || actual.BasicLimitInformation.ActiveProcessLimit != GUEST_JOB_POLICY.active_process_limit
            || actual.ProcessMemoryLimit != GUEST_JOB_POLICY.per_process_memory_bytes
            || actual.JobMemoryLimit != GUEST_JOB_POLICY.job_memory_bytes {
            return Err("Windows did not retain every required guest process and memory limit; execution is refused.".into());
        }

        let mut actual_cpu: JOBOBJECT_CPU_RATE_CONTROL_INFORMATION = unsafe { zeroed() };
        query_info(self.handle, JobObjectCpuRateControlInformation,
            &mut actual_cpu as *mut _ as *mut c_void,
            size_of::<JOBOBJECT_CPU_RATE_CONTROL_INFORMATION>(),
            "verify the CPU hard cap")?;
        let cpu_flags = JOB_OBJECT_CPU_RATE_CONTROL_ENABLE | JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP;
        if actual_cpu.ControlFlags & cpu_flags != cpu_flags
            || unsafe { actual_cpu.Anonymous.CpuRate } != GUEST_JOB_POLICY.cpu_rate {
            return Err("Windows did not retain the required guest CPU hard cap; execution is refused.".into());
        }
        Ok(())
    }

    /// Assign a FRIDAY-created process and verify its membership. Never resumes it.
    ///
    /// # Safety
    /// process must be a valid handle to a FRIDAY-created process that is still
    /// suspended. The caller must not resume its primary thread unless this succeeds.
    pub(crate) unsafe fn assign_suspended_process(&self, process: HANDLE) -> Result<(), String> {
        if process.is_null() || process == INVALID_HANDLE_VALUE {
            return Err("The suspended guest process handle is invalid.".into());
        }
        if AssignProcessToJobObject(self.handle, process) == 0 {
            return Err(last_error("assign the suspended guest process to its Job Object"));
        }
        let mut member = 0;
        if IsProcessInJob(process, self.handle, &mut member) == 0 {
            return Err(last_error("verify suspended guest process Job Object membership"));
        }
        if member == 0 {
            return Err("Windows did not confirm guest process Job Object membership; execution is refused.".into());
        }
        Ok(())
    }
}

impl Drop for OwnedGuestJob {
    fn drop(&mut self) {
        if !self.handle.is_null() && self.handle != INVALID_HANDLE_VALUE {
            unsafe { CloseHandle(self.handle); }
            self.handle = ptr::null_mut();
        }
    }
}

fn set_info(handle: HANDLE, class: windows_sys::Win32::System::JobObjects::JOBOBJECTINFOCLASS,
    value: *const c_void, size: usize, operation: &str) -> Result<(), String> {
    if unsafe { SetInformationJobObject(handle, class, value, size as u32) } == 0 {
        Err(last_error(operation))
    } else { Ok(()) }
}
fn query_info(handle: HANDLE, class: windows_sys::Win32::System::JobObjects::JOBOBJECTINFOCLASS,
    value: *mut c_void, size: usize, operation: &str) -> Result<(), String> {
    if unsafe { QueryInformationJobObject(handle, class, value, size as u32, ptr::null_mut()) } == 0 {
        Err(last_error(operation))
    } else { Ok(()) }
}
fn last_error(operation: &str) -> String {
    let code = unsafe { GetLastError() };
    format!("FRIDAY could not {operation} (Windows error {code}); guest execution is refused.")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn guest_job_policy_is_fixed_and_bounded() {
        assert_eq!(GUEST_JOB_POLICY.active_process_limit, 16);
        assert_eq!(GUEST_JOB_POLICY.per_process_memory_bytes, 256 * 1024 * 1024);
        assert_eq!(GUEST_JOB_POLICY.job_memory_bytes, 512 * 1024 * 1024);
        assert_eq!(GUEST_JOB_POLICY.cpu_rate, 2_000);
        assert!(GUEST_JOB_POLICY.per_process_memory_bytes <= GUEST_JOB_POLICY.job_memory_bytes);
        assert!(GUEST_JOB_POLICY.active_process_limit > 0);
        assert!(GUEST_JOB_POLICY.cpu_rate > 0 && GUEST_JOB_POLICY.cpu_rate <= 10_000);
    }

    #[test]
    fn windows_job_object_applies_and_reads_back_all_required_limits() {
        let job = OwnedGuestJob::create().expect("Windows must support the mandatory Job Object policy");
        let mut actual: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
        query_info(job.handle, JobObjectExtendedLimitInformation,
            &mut actual as *mut _ as *mut c_void,
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>(), "read back test limits").unwrap();
        let flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
            | JOB_OBJECT_LIMIT_PROCESS_MEMORY | JOB_OBJECT_LIMIT_JOB_MEMORY;
        assert_eq!(actual.BasicLimitInformation.LimitFlags & flags, flags);
        assert_eq!(actual.BasicLimitInformation.ActiveProcessLimit, GUEST_JOB_POLICY.active_process_limit);
        assert_eq!(actual.ProcessMemoryLimit, GUEST_JOB_POLICY.per_process_memory_bytes);
        assert_eq!(actual.JobMemoryLimit, GUEST_JOB_POLICY.job_memory_bytes);
        let mut cpu: JOBOBJECT_CPU_RATE_CONTROL_INFORMATION = unsafe { zeroed() };
        query_info(job.handle, JobObjectCpuRateControlInformation,
            &mut cpu as *mut _ as *mut c_void,
            size_of::<JOBOBJECT_CPU_RATE_CONTROL_INFORMATION>(), "read back test CPU limits").unwrap();
        let expected = JOB_OBJECT_CPU_RATE_CONTROL_ENABLE | JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP;
        assert_eq!(cpu.ControlFlags & expected, expected);
        assert_eq!(unsafe { cpu.Anonymous.CpuRate }, GUEST_JOB_POLICY.cpu_rate);
    }
}

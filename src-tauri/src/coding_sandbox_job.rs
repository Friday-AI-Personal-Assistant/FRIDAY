//! Windows Job Object resource containment primitives for the future guest runner.
use std::{ffi::c_void, mem::{size_of, zeroed}, ptr};
use windows_sys::Win32::{
    Foundation::{CloseHandle, GetLastError, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT, STILL_ACTIVE},
    System::{
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob, QueryInformationJobObject,
            SetInformationJobObject, JOBOBJECT_BASIC_LIMIT_INFORMATION,
            JOBOBJECT_CPU_RATE_CONTROL_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JobObjectCpuRateControlInformation, JobObjectExtendedLimitInformation,
            JOB_OBJECT_CPU_RATE_CONTROL_ENABLE, JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP,
            JOB_OBJECT_LIMIT_ACTIVE_PROCESS, JOB_OBJECT_LIMIT_JOB_MEMORY,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_PROCESS_MEMORY,
            JOB_OBJECT_LIMIT_BREAKAWAY_OK, JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK,
        },
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
        let forbidden = JOB_OBJECT_LIMIT_BREAKAWAY_OK | JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK;
        if actual.BasicLimitInformation.LimitFlags & required != required
            || actual.BasicLimitInformation.LimitFlags & forbidden != 0
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

    /// Read the kernel's current count of processes still owned by this job.
    /// A zero count is the condition required before joining stdout/stderr readers:
    /// descendants may inherit the pipe writers after the root process exits.
    pub(crate) fn active_process_count(&self) -> Result<u32, String> {
        let mut accounting: windows_sys::Win32::System::JobObjects::JOBOBJECT_BASIC_ACCOUNTING_INFORMATION =
            unsafe { zeroed() };
        query_info(
            self.handle,
            windows_sys::Win32::System::JobObjects::JobObjectBasicAccountingInformation,
            &mut accounting as *mut _ as *mut c_void,
            size_of::<windows_sys::Win32::System::JobObjects::JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>(),
            "query the FRIDAY-owned guest process-tree count",
        )?;
        Ok(accounting.ActiveProcesses)
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



/// RAII wrapper for a Windows handle owned by the current FRIDAY process.
#[cfg(windows)]
struct OwnedWinHandle(HANDLE);

#[cfg(windows)]
impl OwnedWinHandle {
    fn new(handle: HANDLE, operation: &str) -> Result<Self, String> {
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            Err(last_error(operation))
        } else {
            Ok(Self(handle))
        }
    }

    fn raw(&self) -> HANDLE { self.0 }

    fn into_raw(mut self) -> HANDLE {
        let handle = self.0;
        self.0 = ptr::null_mut();
        handle
    }
}

#[cfg(windows)]
impl Drop for OwnedWinHandle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            unsafe { CloseHandle(self.0); }
            self.0 = ptr::null_mut();
        }
    }
}

/// Attribute-list storage for CreateProcessW's explicit inherited-handle list.
#[cfg(windows)]
struct ProcThreadAttributeList {
    storage: Vec<usize>,
    initialized: bool,
}

#[cfg(windows)]
impl ProcThreadAttributeList {
    fn new(inherited: &[HANDLE]) -> Result<Self, String> {
        use windows_sys::Win32::System::Threading::{
            InitializeProcThreadAttributeList, UpdateProcThreadAttribute,
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST, LPPROC_THREAD_ATTRIBUTE_LIST,
        };
        let mut required = 0usize;
        unsafe {
            InitializeProcThreadAttributeList(ptr::null_mut(), 1, 0, &mut required);
        }
        if required == 0 {
            return Err(last_error("size the guest process handle allowlist"));
        }
        let words = required.saturating_add(size_of::<usize>() - 1) / size_of::<usize>();
        let mut result = Self { storage: vec![0usize; words], initialized: false };
        let list = result.storage.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST;
        if unsafe { InitializeProcThreadAttributeList(list, 1, 0, &mut required) } == 0 {
            return Err(last_error("initialize the guest process handle allowlist"));
        }
        result.initialized = true;
        if unsafe {
            UpdateProcThreadAttribute(
                list,
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                inherited.as_ptr() as *mut c_void,
                inherited.len() * size_of::<HANDLE>(),
                ptr::null_mut(),
                ptr::null_mut(),
            )
        } == 0 {
            return Err(last_error("apply the guest process handle allowlist"));
        }
        Ok(result)
    }

    fn as_mut_ptr(&mut self) -> windows_sys::Win32::System::Threading::LPPROC_THREAD_ATTRIBUTE_LIST {
        self.storage.as_mut_ptr() as windows_sys::Win32::System::Threading::LPPROC_THREAD_ATTRIBUTE_LIST
    }
}

#[cfg(windows)]
impl Drop for ProcThreadAttributeList {
    fn drop(&mut self) {
        if self.initialized {
            unsafe {
                windows_sys::Win32::System::Threading::DeleteProcThreadAttributeList(self.as_mut_ptr());
            }
            self.initialized = false;
        }
    }
}

/// Creates anonymous stdout/stderr pipes. Only the child write ends are
/// inheritable; parent read ends are explicitly non-inheritable.
#[cfg(windows)]
struct GuestPipeHandles {
    stdout_read: Option<OwnedWinHandle>,
    stdout_write: OwnedWinHandle,
    stderr_read: Option<OwnedWinHandle>,
    stderr_write: OwnedWinHandle,
    stdin_null: OwnedWinHandle,
}

#[cfg(windows)]
impl GuestPipeHandles {
    fn create() -> Result<Self, String> {
        use windows_sys::Win32::Foundation::{
            SetHandleInformation, HANDLE_FLAG_INHERIT, GENERIC_READ,
        };
        use windows_sys::Win32::System::Pipes::CreatePipe;
        use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
        use windows_sys::Win32::Storage::FileSystem::{
            CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE,
            OPEN_EXISTING,
        };

        let mut security: SECURITY_ATTRIBUTES = unsafe { zeroed() };
        security.nLength = size_of::<SECURITY_ATTRIBUTES>() as u32;
        security.bInheritHandle = 1;

        let mut stdout_read = ptr::null_mut();
        let mut stdout_write = ptr::null_mut();
        if unsafe { CreatePipe(&mut stdout_read, &mut stdout_write, &security, 0) } == 0 {
            return Err(last_error("create the guest stdout pipe"));
        }
        let stdout_read = OwnedWinHandle::new(stdout_read, "own the guest stdout read handle")?;
        let stdout_write = OwnedWinHandle::new(stdout_write, "own the guest stdout write handle")?;
        if unsafe { SetHandleInformation(stdout_read.raw(), HANDLE_FLAG_INHERIT, 0) } == 0 {
            return Err(last_error("make the guest stdout read handle non-inheritable"));
        }

        let mut stderr_read = ptr::null_mut();
        let mut stderr_write = ptr::null_mut();
        if unsafe { CreatePipe(&mut stderr_read, &mut stderr_write, &security, 0) } == 0 {
            return Err(last_error("create the guest stderr pipe"));
        }
        let stderr_read = OwnedWinHandle::new(stderr_read, "own the guest stderr read handle")?;
        let stderr_write = OwnedWinHandle::new(stderr_write, "own the guest stderr write handle")?;
        if unsafe { SetHandleInformation(stderr_read.raw(), HANDLE_FLAG_INHERIT, 0) } == 0 {
            return Err(last_error("make the guest stderr read handle non-inheritable"));
        }

        let nul: Vec<u16> = "NUL".encode_utf16().chain(std::iter::once(0)).collect();
        let stdin = unsafe {
            CreateFileW(
                nul.as_ptr(),
                GENERIC_READ,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                &security,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                ptr::null_mut(),
            )
        };
        let stdin_null = OwnedWinHandle::new(stdin, "open the null guest stdin handle")?;
        Ok(Self {
            stdout_read: Some(stdout_read),
            stdout_write,
            stderr_read: Some(stderr_read),
            stderr_write,
            stdin_null,
        })
    }

    fn inherited_handles(&self) -> [HANDLE; 3] {
        [self.stdout_write.raw(), self.stderr_write.raw(), self.stdin_null.raw()]
    }

    fn take_readers(&mut self) -> Result<GuestOutputCapture, String> {
        use std::os::windows::io::FromRawHandle;
        let stdout = self.stdout_read.take()
            .ok_or_else(|| "Guest stdout read handle was already transferred.".to_string())?;
        let stderr = self.stderr_read.take()
            .ok_or_else(|| "Guest stderr read handle was already transferred.".to_string())?;
        let stdout_file = unsafe { std::fs::File::from_raw_handle(stdout.into_raw() as _) };
        let stderr_file = unsafe { std::fs::File::from_raw_handle(stderr.into_raw() as _) };
        Ok(GuestOutputCapture::start(stdout_file, stderr_file))
    }
}

/// A FRIDAY-owned process created suspended. The process is terminated on drop
/// unless the complete containment transaction successfully resumes it.
#[cfg(windows)]
pub(crate) struct OwnedGuestProcess {
    process: HANDLE,
    thread: HANDLE,
    resumed: bool,
    output: Option<GuestOutputCapture>,
}

#[cfg(windows)]
impl OwnedGuestProcess {
    /// Finish the captured streams only after Windows confirms every process in
    /// this FRIDAY-owned job has exited. Root-process exit alone is insufficient:
    /// descendants may still hold inherited stdout/stderr writer handles.
    pub(crate) fn finish_output_after_job_exit(
        &mut self,
        job: &OwnedGuestJob,
        timeout: std::time::Duration,
    ) -> Result<GuestCapturedOutput, String> {
        use std::time::{Instant, Duration};
        let deadline = Instant::now().checked_add(timeout)
            .ok_or_else(|| "The guest output-drain deadline is outside the supported range.".to_string())?;
        loop {
            if job.active_process_count()? == 0 {
                break;
            }
            let now = Instant::now();
            if now >= deadline {
                return Err("The FRIDAY-owned guest process tree did not become empty before the output-drain deadline; captured output is incomplete.".into());
            }
            std::thread::sleep(Duration::from_millis(25).min(deadline.saturating_duration_since(now)));
        }
        let capture = self.output.take()
            .ok_or_else(|| "Guest stdout/stderr output was already collected or was not attached.".to_string())?;
        capture.finish()
    }

    fn terminate_if_suspended(&mut self) {
        if !self.resumed && !self.process.is_null() && self.process != INVALID_HANDLE_VALUE {
            unsafe { windows_sys::Win32::System::Threading::TerminateProcess(self.process, 1); }
        }
    }
}

#[cfg(windows)]
impl Drop for OwnedGuestProcess {
    fn drop(&mut self) {
        self.terminate_if_suspended();
        unsafe {
            if !self.thread.is_null() && self.thread != INVALID_HANDLE_VALUE {
                CloseHandle(self.thread);
            }
            if !self.process.is_null() && self.process != INVALID_HANDLE_VALUE {
                CloseHandle(self.process);
            }
        }
        self.thread = ptr::null_mut();
        self.process = ptr::null_mut();
    }
}

/// Inputs are constructed only by trusted FRIDAY code. There is deliberately no
/// argument-vector or environment field: this first launch primitive starts only
/// the pinned runner image, with no shell and no inherited handles.
#[cfg(windows)]
pub(crate) struct GuestRunnerLaunchSpec {
    pub(crate) executable: std::path::PathBuf,
    pub(crate) working_directory: std::path::PathBuf,
}

#[cfg(windows)]
trait LaunchBackend {
    type Process;
    fn create_suspended(&mut self) -> Result<Self::Process, String>;
    fn assign_and_verify(&mut self, process: &mut Self::Process) -> Result<(), String>;
    fn resume(&mut self, process: &mut Self::Process) -> Result<(), String>;
    fn terminate(&mut self, process: &mut Self::Process);
}

/// Shared transaction orchestrator. Its ordering is tested with an injected
/// backend so an assignment/verification failure can never reach resume.
#[cfg(windows)]
fn run_launch_transaction<B: LaunchBackend>(backend: &mut B) -> Result<B::Process, String> {
    let mut process = backend.create_suspended()?;
    if let Err(error) = backend.assign_and_verify(&mut process) {
        backend.terminate(&mut process);
        return Err(error);
    }
    if let Err(error) = backend.resume(&mut process) {
        backend.terminate(&mut process);
        return Err(error);
    }
    Ok(process)
}

#[cfg(windows)]
struct WindowsLaunchBackend<'a> {
    job: &'a OwnedGuestJob,
    spec: &'a GuestRunnerLaunchSpec,
}

#[cfg(windows)]
impl LaunchBackend for WindowsLaunchBackend<'_> {
    type Process = OwnedGuestProcess;

    fn create_suspended(&mut self) -> Result<Self::Process, String> {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::System::Threading::{
            CreateProcessW, CREATE_NO_WINDOW, CREATE_SUSPENDED, EXTENDED_STARTUPINFO_PRESENT,
            STARTF_USESTDHANDLES, STARTUPINFOEXW, PROCESS_INFORMATION,
        };

        // The runner and working directory are trusted FRIDAY paths, never
        // supplied by the project, model, or guest request.
        if !self.spec.executable.is_absolute() || !self.spec.working_directory.is_absolute() {
            return Err("The guest runner executable and working directory must be absolute trusted paths.".into());
        }
        let executable_meta = std::fs::symlink_metadata(&self.spec.executable)
            .map_err(|_| "The pinned guest runner executable is unavailable; execution is refused.".to_string())?;
        if !executable_meta.file_type().is_file() || executable_meta.file_type().is_symlink() {
            return Err("The pinned guest runner path is not an ordinary file; execution is refused.".into());
        }
        let working_meta = std::fs::symlink_metadata(&self.spec.working_directory)
            .map_err(|_| "The guest working directory is unavailable; execution is refused.".to_string())?;
        if !working_meta.file_type().is_dir() || working_meta.file_type().is_symlink() {
            return Err("The guest working directory is not an ordinary directory; execution is refused.".into());
        }

        let mut application: Vec<u16> = self.spec.executable.as_os_str().encode_wide().collect();
        if application.contains(&0) {
            return Err("The pinned guest runner path contains an invalid NUL character.".into());
        }
        application.push(0);

        // No caller-controlled arguments are accepted. The quoted executable is
        // argv[0]; lpApplicationName independently pins the image being started.
        let mut command_line_wide = quote_windows_argument_wide(&application[..application.len() - 1]);
        command_line_wide.push(0);

        let mut working_directory: Vec<u16> = self.spec.working_directory.as_os_str().encode_wide().collect();
        if working_directory.contains(&0) {
            return Err("The guest working directory contains an invalid NUL character.".into());
        }
        working_directory.push(0);

        let mut pipes = GuestPipeHandles::create()?;
        let inherited = pipes.inherited_handles();
        let mut attributes = ProcThreadAttributeList::new(&inherited)?;
        let mut startup: STARTUPINFOEXW = unsafe { zeroed() };
        startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
        startup.StartupInfo.dwFlags |= STARTF_USESTDHANDLES;
        startup.StartupInfo.hStdInput = pipes.stdin_null.raw();
        startup.StartupInfo.hStdOutput = pipes.stdout_write.raw();
        startup.StartupInfo.hStdError = pipes.stderr_write.raw();
        startup.lpAttributeList = attributes.as_mut_ptr();
        let mut info: PROCESS_INFORMATION = unsafe { zeroed() };
        let environment = minimal_runner_environment()?;
        let flags = CREATE_SUSPENDED | CREATE_NO_WINDOW | EXTENDED_STARTUPINFO_PRESENT
            | windows_sys::Win32::System::Threading::CREATE_UNICODE_ENVIRONMENT;

        let created = unsafe {
            CreateProcessW(
                application.as_ptr(),
                command_line_wide.as_mut_ptr(),
                ptr::null(),
                ptr::null(),
                1, // Only the explicit PROC_THREAD_ATTRIBUTE_HANDLE_LIST is inherited.
                flags,
                environment.as_ptr() as *const c_void,
                working_directory.as_ptr(),
                &startup.StartupInfo,
                &mut info,
            )
        };
        if created == 0 {
            return Err(last_error("create the pinned guest runner in a suspended state"));
        }
        if info.hProcess.is_null() || info.hThread.is_null() {
            if !info.hProcess.is_null() {
                unsafe { windows_sys::Win32::System::Threading::TerminateProcess(info.hProcess, 1); }
                unsafe { CloseHandle(info.hProcess); }
            }
            if !info.hThread.is_null() {
                unsafe { CloseHandle(info.hThread); }
            }
            return Err("Windows returned incomplete suspended-process handles; execution is refused.".into());
        }
        // Close the parent's copies of every child-only write/stdin handle
        // before starting readers. The child owns its allowlisted duplicates.
        let output = pipes.take_readers()?;
        drop(pipes.stdout_write);
        drop(pipes.stderr_write);
        drop(pipes.stdin_null);
        Ok(OwnedGuestProcess {
            process: info.hProcess,
            thread: info.hThread,
            resumed: false,
            output: Some(output),
        })
    }

    fn assign_and_verify(&mut self, process: &mut Self::Process) -> Result<(), String> {
        unsafe { self.job.assign_suspended_process(process.process) }
    }

    fn resume(&mut self, process: &mut Self::Process) -> Result<(), String> {
        use windows_sys::Win32::System::Threading::ResumeThread;
        let previous_suspend_count = unsafe { ResumeThread(process.thread) };
        if previous_suspend_count == u32::MAX {
            return Err(last_error("resume the contained guest runner"));
        }
        if previous_suspend_count != 1 {
            // A newly created primary thread must have exactly one suspension
            // from CREATE_SUSPENDED. Unexpected state is treated as unsafe.
            return Err("The suspended guest runner had an unexpected thread suspend count; execution is refused.".into());
        }
        process.resumed = true;
        unsafe { CloseHandle(process.thread); }
        process.thread = ptr::null_mut();
        Ok(())
    }

    fn terminate(&mut self, process: &mut Self::Process) {
        process.terminate_if_suspended();
    }
}

#[cfg(windows)]
fn quote_windows_argument_wide(value: &[u16]) -> Vec<u16> {
    const BACKSLASH: u16 = b'\\' as u16;
    const QUOTE: u16 = b'"' as u16;
    let mut quoted = vec![QUOTE];
    let mut backslashes = 0usize;
    for &unit in value {
        if unit == BACKSLASH {
            backslashes += 1;
        } else if unit == QUOTE {
            quoted.extend(std::iter::repeat(BACKSLASH).take(backslashes * 2 + 1));
            quoted.push(QUOTE);
            backslashes = 0;
        } else {
            quoted.extend(std::iter::repeat(BACKSLASH).take(backslashes));
            backslashes = 0;
            quoted.push(unit);
        }
    }
    quoted.extend(std::iter::repeat(BACKSLASH).take(backslashes * 2));
    quoted.push(QUOTE);
    quoted
}

#[cfg(windows)]
fn minimal_runner_environment() -> Result<Vec<u16>, String> {
    use std::{ffi::OsString, os::windows::ffi::{OsStrExt, OsStringExt}, path::PathBuf};
    use windows_sys::Win32::System::SystemInformation::GetWindowsDirectoryW;

    let mut root_buffer = vec![0u16; 32_768];
    let root_len = unsafe { GetWindowsDirectoryW(root_buffer.as_mut_ptr(), root_buffer.len() as u32) };
    if root_len == 0 || root_len as usize >= root_buffer.len() {
        return Err(last_error("resolve the Windows system directory for the minimal runner environment"));
    }
    root_buffer.truncate(root_len as usize);
    let windows_root = PathBuf::from(OsString::from_wide(&root_buffer));
    if !windows_root.is_absolute() {
        return Err("Windows returned a non-absolute system root; guest execution is refused.".into());
    }
    let system32 = windows_root.join("System32");
    let mut entries = vec![
        ("PATH", system32.as_os_str()),
        ("SystemRoot", windows_root.as_os_str()),
        ("WINDIR", windows_root.as_os_str()),
    ];
    entries.sort_by(|a, b| a.0.to_ascii_lowercase().cmp(&b.0.to_ascii_lowercase()));

    let mut block = Vec::new();
    for (key, value) in entries {
        block.extend(key.encode_utf16());
        block.push(b'=' as u16);
        block.extend(value.encode_wide());
        block.push(0);
    }
    block.push(0);
    Ok(block)
}

/// Create, contain, verify, then resume the pinned runner. This remains unused
/// until a reviewed runner image, fixed environment, guest session and storage
/// controls are implemented and physically verified.
#[cfg(windows)]
#[allow(dead_code)]
pub(crate) fn launch_contained_guest_runner(
    job: &OwnedGuestJob,
    spec: &GuestRunnerLaunchSpec,
) -> Result<OwnedGuestProcess, String> {
    run_launch_transaction(&mut WindowsLaunchBackend { job, spec })
}

#[cfg(windows)]
#[cfg(test)]
mod launch_transaction_tests {
    use super::*;

    #[derive(Default)]
    struct FakeBackend {
        events: Vec<&'static str>,
        fail_assignment: bool,
        fail_resume: bool,
    }

    impl LaunchBackend for FakeBackend {
        type Process = ();

        fn create_suspended(&mut self) -> Result<Self::Process, String> {
            self.events.push("create_suspended");
            Ok(())
        }
        fn assign_and_verify(&mut self, _: &mut Self::Process) -> Result<(), String> {
            self.events.push("assign_and_verify");
            if self.fail_assignment { Err("assignment rejected".into()) } else { Ok(()) }
        }
        fn resume(&mut self, _: &mut Self::Process) -> Result<(), String> {
            self.events.push("resume");
            if self.fail_resume { Err("resume rejected".into()) } else { Ok(()) }
        }
        fn terminate(&mut self, _: &mut Self::Process) {
            self.events.push("terminate");
        }
    }

    #[test]
    fn launch_transaction_assigns_and_verifies_before_resume() {
        let mut backend = FakeBackend::default();
        run_launch_transaction(&mut backend).unwrap();
        assert_eq!(backend.events, ["create_suspended", "assign_and_verify", "resume"]);
    }

    #[test]
    fn assignment_failure_terminates_without_resuming() {
        let mut backend = FakeBackend { fail_assignment: true, ..Default::default() };
        assert!(run_launch_transaction(&mut backend).is_err());
        assert_eq!(backend.events, ["create_suspended", "assign_and_verify", "terminate"]);
    }

    #[test]
    fn resume_failure_terminates_the_still_suspended_process() {
        let mut backend = FakeBackend { fail_resume: true, ..Default::default() };
        assert!(run_launch_transaction(&mut backend).is_err());
        assert_eq!(backend.events, ["create_suspended", "assign_and_verify", "resume", "terminate"]);
    }

    #[test]
    fn windows_argument_quoting_preserves_spaces_and_trailing_backslashes() {
        let input: Vec<u16> = r"C:\Program Files\Friday\runner.exe".encode_utf16().collect();
        let output = String::from_utf16(&quote_windows_argument_wide(&input)).unwrap();
        assert_eq!(output, r#""C:\Program Files\Friday\runner.exe""#);

        let trailing: Vec<u16> = r"C:\Friday Folder\".encode_utf16().collect();
        let output = String::from_utf16(&quote_windows_argument_wide(&trailing)).unwrap();
        assert_eq!(output, r#""C:\Friday Folder\\""#);
    }

    #[test]
    fn windows_argument_quoting_preserves_non_bmp_unicode_code_units() {
        let input = vec![b'C' as u16, b':' as u16, b'\\' as u16, 0xD83D, 0xDE80];
        let output = quote_windows_argument_wide(&input);
        assert_eq!(&output[1..output.len() - 1], input.as_slice());
    }
}


/// Fixed retained-output ceiling for each guest stream. Readers must continue
/// draining the OS pipe after this limit is reached; excess bytes are discarded.
pub(crate) const GUEST_STDOUT_RETAIN_LIMIT: usize = 256 * 1024;
pub(crate) const GUEST_STDERR_RETAIN_LIMIT: usize = 256 * 1024;

/// Bounded byte retention for one output stream. This type deliberately does
/// not own a pipe: the reader must keep reading and call push() for every
/// chunk, even after is_truncated() becomes true, to prevent pipe deadlocks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BoundedGuestOutput {
    bytes: Vec<u8>,
    limit: usize,
    truncated: bool,
}

impl BoundedGuestOutput {
    pub(crate) fn new(limit: usize) -> Self {
        Self { bytes: Vec::with_capacity(limit.min(8 * 1024)), limit, truncated: false }
    }

    /// Retain only the prefix that fits. The caller must continue draining its
    /// source after this method returns, including when the buffer is full.
    pub(crate) fn push(&mut self, chunk: &[u8]) {
        let remaining = self.limit.saturating_sub(self.bytes.len());
        let retained = remaining.min(chunk.len());
        self.bytes.extend_from_slice(&chunk[..retained]);
        if retained < chunk.len() {
            self.truncated = true;
        }
    }

    pub(crate) fn as_bytes(&self) -> &[u8] { &self.bytes }
    pub(crate) fn as_lossy_text(&self) -> String { String::from_utf8_lossy(&self.bytes).into_owned() }
    pub(crate) fn is_truncated(&self) -> bool { self.truncated }
    pub(crate) fn retained_len(&self) -> usize { self.bytes.len() }
}

/// Per-stream output result. Truncation is explicit so callers never mistake
/// a retained prefix for complete compiler/runner output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GuestCapturedOutput {
    pub(crate) stdout: BoundedGuestOutput,
    pub(crate) stderr: BoundedGuestOutput,
}

impl GuestCapturedOutput {
    pub(crate) fn new() -> Self {
        Self {
            stdout: BoundedGuestOutput::new(GUEST_STDOUT_RETAIN_LIMIT),
            stderr: BoundedGuestOutput::new(GUEST_STDERR_RETAIN_LIMIT),
        }
    }

    pub(crate) fn is_truncated(&self) -> bool {
        self.stdout.is_truncated() || self.stderr.is_truncated()
    }
}

#[cfg(test)]
mod bounded_output_tests {
    use super::*;

    #[test]
    fn output_retention_never_exceeds_the_configured_limit() {
        let mut output = BoundedGuestOutput::new(8);
        output.push(b"123456");
        output.push(b"7890");
        assert_eq!(output.as_bytes(), b"12345678");
        assert_eq!(output.retained_len(), 8);
        assert!(output.is_truncated());
    }

    #[test]
    fn output_under_limit_is_preserved_without_truncation() {
        let mut output = BoundedGuestOutput::new(32);
        output.push(b"compiler ");
        output.push(b"finished");
        assert_eq!(output.as_lossy_text(), "compiler finished");
        assert!(!output.is_truncated());
    }

    #[test]
    fn zero_limit_discards_all_bytes_and_marks_truncation() {
        let mut output = BoundedGuestOutput::new(0);
        output.push(b"discard");
        assert!(output.as_bytes().is_empty());
        assert!(output.is_truncated());
    }

    #[test]
    fn stdout_and_stderr_have_independent_fixed_budgets() {
        let output = GuestCapturedOutput::new();
        assert_eq!(output.stdout.limit, GUEST_STDOUT_RETAIN_LIMIT);
        assert_eq!(output.stderr.limit, GUEST_STDERR_RETAIN_LIMIT);
        assert_eq!(GUEST_STDOUT_RETAIN_LIMIT + GUEST_STDERR_RETAIN_LIMIT, 512 * 1024);
    }

    #[test]
    fn additional_chunks_after_truncation_are_discarded_safely() {
        let mut output = BoundedGuestOutput::new(4);
        output.push(b"123456");
        output.push(b"7890");
        assert_eq!(output.as_bytes(), b"1234");
        assert!(output.is_truncated());
    }
}


/// A single stream reader that drains to EOF on a dedicated thread while
/// retaining only a bounded prefix. The reader continues consuming bytes
/// after the retention limit is reached, preventing a full child pipe from
/// blocking the process.
pub(crate) struct GuestOutputReader {
    worker: Option<std::thread::JoinHandle<Result<BoundedGuestOutput, String>>>,
}

impl GuestOutputReader {
    pub(crate) fn start<R>(mut reader: R, limit: usize) -> Self
    where
        R: std::io::Read + Send + 'static,
    {
        let worker = std::thread::spawn(move || {
            let mut output = BoundedGuestOutput::new(limit);
            let mut chunk = [0u8; 8 * 1024];
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) => return Ok(output),
                    Ok(count) => output.push(&chunk[..count]),
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(format!("Guest output stream could not be drained: {error}")),
                }
            }
        });
        Self { worker: Some(worker) }
    }

    /// Join only after the owned process tree has exited and all writer handles
    /// are closed. A panic or read failure is surfaced rather than hidden.
    pub(crate) fn finish(&mut self) -> Result<BoundedGuestOutput, String> {
        let worker = self.worker.take()
            .ok_or_else(|| "Guest output reader was already joined.".to_string())?;
        worker.join()
            .map_err(|_| "Guest output reader thread terminated unexpectedly.".to_string())?
    }
}

/// Concurrent stdout/stderr drain coordinator. Both readers must be started
/// before waiting for process completion so neither stream can fill its pipe.
pub(crate) struct GuestOutputCapture {
    stdout: GuestOutputReader,
    stderr: GuestOutputReader,
}

impl GuestOutputCapture {
    pub(crate) fn start<R1, R2>(stdout: R1, stderr: R2) -> Self
    where
        R1: std::io::Read + Send + 'static,
        R2: std::io::Read + Send + 'static,
    {
        Self {
            stdout: GuestOutputReader::start(stdout, GUEST_STDOUT_RETAIN_LIMIT),
            stderr: GuestOutputReader::start(stderr, GUEST_STDERR_RETAIN_LIMIT),
        }
    }

    /// Call after the contained process tree is confirmed stopped and both
    /// pipe writers have closed. Each reader has drained through EOF.
    pub(crate) fn finish(mut self) -> Result<GuestCapturedOutput, String> {
        let stdout = self.stdout.finish()?;
        let stderr = self.stderr.finish()?;
        Ok(GuestCapturedOutput { stdout, stderr })
    }
}

#[cfg(test)]
mod output_reader_tests {
    use super::*;
    use std::io::{Cursor, Read};

    #[test]
    fn reader_drains_entire_stream_after_retention_limit() {
        let bytes = vec![b'x'; 128 * 1024];
        let reader = GuestOutputReader::start(Cursor::new(bytes.clone()), 1024);
        let mut reader = reader;
        let output = reader.finish().unwrap();
        assert_eq!(output.retained_len(), 1024);
        assert!(output.is_truncated());
        assert_eq!(output.as_bytes(), &bytes[..1024]);
    }

    #[test]
    fn stdout_and_stderr_are_drained_concurrently_with_independent_limits() {
        let stdout = vec![b'o'; GUEST_STDOUT_RETAIN_LIMIT + 4096];
        let stderr = vec![b'e'; GUEST_STDERR_RETAIN_LIMIT + 8192];
        let capture = GuestOutputCapture::start(Cursor::new(stdout), Cursor::new(stderr));
        let result = capture.finish().unwrap();
        assert_eq!(result.stdout.retained_len(), GUEST_STDOUT_RETAIN_LIMIT);
        assert_eq!(result.stderr.retained_len(), GUEST_STDERR_RETAIN_LIMIT);
        assert!(result.stdout.is_truncated());
        assert!(result.stderr.is_truncated());
        assert!(result.is_truncated());
    }

    struct InterruptThenData {
        interrupted: bool,
        data: Cursor<Vec<u8>>,
    }

    impl Read for InterruptThenData {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            if !self.interrupted {
                self.interrupted = true;
                return Err(std::io::ErrorKind::Interrupted.into());
            }
            self.data.read(buffer)
        }
    }

    #[test]
    fn reader_retries_interrupted_reads_and_preserves_complete_output() {
        let source = InterruptThenData {
            interrupted: false,
            data: Cursor::new(b"compiler finished".to_vec()),
        };
        let mut reader = GuestOutputReader::start(source, 64);
        let output = reader.finish().unwrap();
        assert_eq!(output.as_bytes(), b"compiler finished");
        assert!(!output.is_truncated());
    }

    struct FailingReader;
    impl Read for FailingReader {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "simulated pipe failure"))
        }
    }

    #[test]
    fn reader_surfaces_stream_errors() {
        let mut reader = GuestOutputReader::start(FailingReader, 64);
        assert!(reader.finish().unwrap_err().contains("could not be drained"));
    }

    #[test]
    fn reader_cannot_be_joined_twice() {
        let mut reader = GuestOutputReader::start(Cursor::new(b"done".to_vec()), 64);
        assert_eq!(reader.finish().unwrap().as_bytes(), b"done");
        assert!(reader.finish().unwrap_err().contains("already joined"));
    }
}

/// Result of supervising a contained guest process.
#[cfg(windows)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GuestProcessOutcome {
    Exited { exit_code: u32 },
    TimedOut,
    Cancelled,
}

#[cfg(windows)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProcessWaitState {
    Exited,
    StillRunning,
}

#[cfg(windows)]
trait ProcessSupervisorBackend {
    fn wait(&mut self, timeout_ms: u32) -> Result<ProcessWaitState, String>;
    fn terminate_owned_job(&mut self) -> Result<(), String>;
    fn exit_code(&mut self) -> Result<u32, String>;
}

/// Supervise only the process already created by this launch transaction and
/// the Job Object that owns its descendants. Cancellation and timeout both
/// terminate the whole owned job, then allow only a bounded shutdown grace.
#[cfg(windows)]
fn supervise_process<B: ProcessSupervisorBackend>(
    backend: &mut B,
    timeout: std::time::Duration,
    cancellation: &std::sync::atomic::AtomicBool,
    shutdown_grace: std::time::Duration,
) -> Result<GuestProcessOutcome, String> {
    use std::sync::atomic::Ordering;
    use std::time::Instant;

    let deadline = Instant::now().checked_add(timeout)
        .ok_or_else(|| "The guest timeout value is outside the supported range.".to_string())?;

    loop {
        if cancellation.load(Ordering::Acquire) {
            backend.terminate_owned_job()?;
            return match backend.wait(shutdown_grace.as_millis().min(u32::MAX as u128) as u32)? {
                ProcessWaitState::Exited => Ok(GuestProcessOutcome::Cancelled),
                ProcessWaitState::StillRunning => Err(
                    "The cancelled guest process tree did not stop within the shutdown grace period.".into()
                ),
            };
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            backend.terminate_owned_job()?;
            return match backend.wait(shutdown_grace.as_millis().min(u32::MAX as u128) as u32)? {
                ProcessWaitState::Exited => Ok(GuestProcessOutcome::TimedOut),
                ProcessWaitState::StillRunning => Err(
                    "The timed-out guest process tree did not stop within the shutdown grace period.".into()
                ),
            };
        }

        let poll = remaining.min(std::time::Duration::from_millis(50));
        match backend.wait(poll.as_millis().max(1).min(u32::MAX as u128) as u32)? {
            ProcessWaitState::Exited => {
                return Ok(GuestProcessOutcome::Exited { exit_code: backend.exit_code()? });
            }
            ProcessWaitState::StillRunning => {}
        }
    }
}

#[cfg(windows)]
struct WindowsProcessSupervisor<'a> {
    job: &'a OwnedGuestJob,
    process: HANDLE,
}

#[cfg(windows)]
impl ProcessSupervisorBackend for WindowsProcessSupervisor<'_> {
    fn wait(&mut self, timeout_ms: u32) -> Result<ProcessWaitState, String> {
        use windows_sys::Win32::System::Threading::WaitForSingleObject;
        match unsafe { WaitForSingleObject(self.process, timeout_ms) } {
            WAIT_OBJECT_0 => Ok(ProcessWaitState::Exited),
            WAIT_TIMEOUT => Ok(ProcessWaitState::StillRunning),
            _ => Err(last_error("wait for the contained guest process")),
        }
    }

    fn terminate_owned_job(&mut self) -> Result<(), String> {
        if unsafe { windows_sys::Win32::System::JobObjects::TerminateJobObject(self.job.handle, 1) } == 0 {
            Err(last_error("terminate the FRIDAY-owned guest Job Object"))
        } else {
            Ok(())
        }
    }

    fn exit_code(&mut self) -> Result<u32, String> {
        let mut code = 0u32;
        if unsafe { windows_sys::Win32::System::Threading::GetExitCodeProcess(self.process, &mut code) } == 0 {
            return Err(last_error("read the contained guest process exit code"));
        }
        if code == STILL_ACTIVE as u32 {
            return Err("Windows reported the guest process as still active after signaling it exited.".into());
        }
        Ok(code)
    }
}

/// Wait for a launched process with a fixed caller-selected deadline and
/// cancellation signal. The job remains owned by the caller for the full run.
#[cfg(windows)]
#[allow(dead_code)]
pub(crate) fn supervise_contained_guest_runner(
    job: &OwnedGuestJob,
    process: &OwnedGuestProcess,
    timeout: std::time::Duration,
    cancellation: &std::sync::atomic::AtomicBool,
) -> Result<GuestProcessOutcome, String> {
    let mut backend = WindowsProcessSupervisor { job, process: process.process };
    supervise_process(&mut backend, timeout, cancellation, std::time::Duration::from_secs(2))
}

#[cfg(windows)]
#[cfg(test)]
mod job_tree_output_tests {
    use super::*;

    #[test]
    fn job_accounting_reports_zero_only_after_all_descendants_exit() {
        // Exercise the kernel-backed accounting query against a real empty job.
        let job = OwnedGuestJob::create().expect("create test job");
        assert_eq!(job.active_process_count().unwrap(), 0);
    }

    #[test]
    fn output_finish_is_single_use_and_requires_an_attached_capture() {
        // This regression guards the ownership contract without launching a
        // process: absence of an attached capture must be reported, not hidden.
        let mut process = OwnedGuestProcess {
            process: ptr::null_mut(),
            thread: ptr::null_mut(),
            resumed: true,
            output: None,
        };
        let job = OwnedGuestJob::create().expect("create test job");
        let result = process.finish_output_after_job_exit(&job, std::time::Duration::from_millis(100));
        assert!(result.unwrap_err().contains("already collected"));
    }
}

#[cfg(windows)]
#[cfg(test)]
mod process_supervision_tests {
    use super::*;
    use std::{collections::VecDeque, sync::atomic::AtomicBool, time::Duration};

    struct FakeSupervisor {
        waits: VecDeque<Result<ProcessWaitState, String>>,
        terminated: bool,
        code: u32,
    }

    impl ProcessSupervisorBackend for FakeSupervisor {
        fn wait(&mut self, _: u32) -> Result<ProcessWaitState, String> {
            self.waits.pop_front().unwrap_or(Ok(ProcessWaitState::StillRunning))
        }
        fn terminate_owned_job(&mut self) -> Result<(), String> {
            self.terminated = true;
            Ok(())
        }
        fn exit_code(&mut self) -> Result<u32, String> { Ok(self.code) }
    }

    #[test]
    fn supervision_returns_exit_code_when_process_finishes() {
        let mut fake = FakeSupervisor {
            waits: VecDeque::from([Ok(ProcessWaitState::Exited)]),
            terminated: false,
            code: 23,
        };
        let result = supervise_process(&mut fake, Duration::from_secs(5), &AtomicBool::new(false), Duration::from_secs(1)).unwrap();
        assert_eq!(result, GuestProcessOutcome::Exited { exit_code: 23 });
        assert!(!fake.terminated);
    }

    #[test]
    fn cancellation_terminates_owned_job_and_waits_for_shutdown() {
        let mut fake = FakeSupervisor {
            waits: VecDeque::from([Ok(ProcessWaitState::Exited)]),
            terminated: false,
            code: 0,
        };
        let result = supervise_process(&mut fake, Duration::from_secs(5), &AtomicBool::new(true), Duration::from_secs(1)).unwrap();
        assert_eq!(result, GuestProcessOutcome::Cancelled);
        assert!(fake.terminated);
    }

    #[test]
    fn timeout_terminates_owned_job_and_waits_for_shutdown() {
        let mut fake = FakeSupervisor {
            waits: VecDeque::from([Ok(ProcessWaitState::Exited)]),
            terminated: false,
            code: 0,
        };
        let result = supervise_process(&mut fake, Duration::ZERO, &AtomicBool::new(false), Duration::from_secs(1)).unwrap();
        assert_eq!(result, GuestProcessOutcome::TimedOut);
        assert!(fake.terminated);
    }

    #[test]
    fn cancellation_fails_closed_if_job_does_not_stop_in_grace_period() {
        let mut fake = FakeSupervisor {
            waits: VecDeque::from([Ok(ProcessWaitState::StillRunning)]),
            terminated: false,
            code: 0,
        };
        let result = supervise_process(&mut fake, Duration::from_secs(5), &AtomicBool::new(true), Duration::from_millis(10));
        assert!(result.is_err());
        assert!(fake.terminated);
    }
}

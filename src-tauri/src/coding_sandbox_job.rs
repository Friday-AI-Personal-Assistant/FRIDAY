//! Windows Job Object resource containment primitives for the future guest runner.
use std::{ffi::c_void, mem::{size_of, zeroed}, ptr};
use windows_sys::Win32::{
    Foundation::{CloseHandle, GetLastError, HANDLE, INVALID_HANDLE_VALUE},
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


/// A FRIDAY-owned process created suspended. The process is terminated on drop
/// unless the complete containment transaction successfully resumes it.
#[cfg(windows)]
pub(crate) struct OwnedGuestProcess {
    process: HANDLE,
    thread: HANDLE,
    resumed: bool,
}

#[cfg(windows)]
impl OwnedGuestProcess {
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
            CreateProcessW, CREATE_NO_WINDOW, CREATE_SUSPENDED, STARTUPINFOW, PROCESS_INFORMATION,
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

        let mut startup: STARTUPINFOW = unsafe { zeroed() };
        startup.cb = size_of::<STARTUPINFOW>() as u32;
        let mut info: PROCESS_INFORMATION = unsafe { zeroed() };
        let environment = minimal_runner_environment()?;
        let flags = CREATE_SUSPENDED | CREATE_NO_WINDOW
            | windows_sys::Win32::System::Threading::CREATE_UNICODE_ENVIRONMENT;

        let created = unsafe {
            CreateProcessW(
                application.as_ptr(),
                command_line_wide.as_mut_ptr(),
                ptr::null(),
                ptr::null(),
                0, // No inherited handles.
                flags,
                environment.as_ptr() as *const c_void,
                working_directory.as_ptr(),
                &startup,
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
        Ok(OwnedGuestProcess { process: info.hProcess, thread: info.hThread, resumed: false })
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
        use windows_sys::Win32::System::Threading::{WaitForSingleObject, WAIT_OBJECT_0, WAIT_TIMEOUT};
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
        if code == windows_sys::Win32::System::Threading::STILL_ACTIVE {
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

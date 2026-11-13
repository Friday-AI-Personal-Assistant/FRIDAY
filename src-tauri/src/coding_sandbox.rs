use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    env,
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::Read,
    path::{Component, Path, PathBuf},
    sync::Mutex,
    process::{Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};
use rand::RngCore;
use tauri::{AppHandle, Manager};

const MAX_SNAPSHOT_DEPTH: usize = 12;
const MAX_SNAPSHOT_FILES: usize = 2_000;
const MAX_SNAPSHOT_FILE_BYTES: u64 = 1_048_576;
const MAX_SNAPSHOT_TOTAL_BYTES: u64 = 32 * 1024 * 1024;
const SNAPSHOT_OWNER_MARKER: &str = ".friday-snapshot-owner.json";
const SNAPSHOT_OWNER_SCHEMA: u32 = 1;

const MAX_GUEST_RESULT_BYTES: usize = 1024 * 1024;
const MAX_GUEST_RESULT_DURATION_MS: u64 = 120_000;

const MAX_GUEST_REQUEST_DURATION_MS: u64 = 120_000;
const MAX_GUEST_REQUEST_OUTPUT_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GuestRunRequest {
    schema_version: u32,
    run_id: String,
    snapshot_id: String,
    operation: GuestRunOperation,
    max_duration_ms: u64,
    max_output_bytes: u64,
}

fn validate_guest_run_request(request: GuestRunRequest) -> Result<GuestRunRequest, String> {
    if request.schema_version != 1
        || !valid_snapshot_id(&request.run_id)
        || !valid_snapshot_id(&request.snapshot_id)
        || request.max_duration_ms == 0
        || request.max_duration_ms > MAX_GUEST_REQUEST_DURATION_MS
        || request.max_output_bytes == 0
        || request.max_output_bytes > MAX_GUEST_REQUEST_OUTPUT_BYTES
    {
        return Err("The guest run request is invalid or exceeds FRIDAY's fixed policy limits.".into());
    }
    Ok(request)
}

fn parse_guest_run_request(bytes: &[u8]) -> Result<GuestRunRequest, String> {
    if bytes.is_empty() || bytes.len() > 4096 {
        return Err("The guest run request is empty or exceeds its size limit.".into());
    }
    let request: GuestRunRequest = serde_json::from_slice(bytes)
        .map_err(|_| "The guest run request is malformed or contains unsupported fields.".to_string())?;
    validate_guest_run_request(request)
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum GuestRunOperation {
    RustCargoTest,
    NodeNpmTest,
    PythonUnittest,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum GuestRunStatus {
    Passed,
    Failed,
    TimedOut,
    Cancelled,
    Unsupported,
    RunnerError,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GuestRunResult {
    schema_version: u32,
    run_id: String,
    operation: GuestRunOperation,
    status: GuestRunStatus,
    exit_code: Option<i32>,
    stdout: Option<String>,
    stderr: Option<String>,
    truncated: bool,
    duration_ms: u64,
}

fn validate_guest_result_json(
    bytes: &[u8],
    expected_run_id: &str,
    expected_operation: GuestRunOperation,
) -> Result<GuestRunResult, String> {
    validate_guest_result_json_with_limit(
        bytes,
        expected_run_id,
        expected_operation,
        MAX_GUEST_RESULT_BYTES as u64,
    )
}


const GUEST_RESULT_FILE_NAME: &str = "result.json";

fn validate_guest_result_directory_entries(names: &[String]) -> Result<(), String> {
    if names.len() != 1 || names[0] != GUEST_RESULT_FILE_NAME {
        return Err("The guest result folder must contain exactly one file named result.json; unexpected or missing entries are rejected.".into());
    }
    Ok(())
}

fn read_verified_guest_result_file(
    result_directory: &Path,
    expected_run_id: &str,
    expected_operation: GuestRunOperation,
    max_output_bytes: u64,
) -> Result<GuestRunResult, String> {
    let directory_metadata = fs::symlink_metadata(result_directory)
        .map_err(|_| "The guest result directory is unavailable.".to_string())?;
    if !directory_metadata.is_dir() || is_reparse_or_symlink(&directory_metadata) {
        return Err("The guest result directory is not a normal directory.".into());
    }
    let canonical_directory = fs::canonicalize(result_directory)
        .map_err(|_| "FRIDAY could not verify the guest result directory.".to_string())?;

    let entries = fs::read_dir(&canonical_directory)
        .map_err(|_| "FRIDAY could not inspect the guest result directory.".to_string())?;
    let mut entry_names = Vec::new();
    for entry in entries {
        let entry = entry
            .map_err(|_| "FRIDAY could not inspect a guest result entry.".to_string())?;
        let name = entry.file_name().into_string()
            .map_err(|_| "The guest result directory contains a non-text filename.".to_string())?;
        entry_names.push(name);
    }
    validate_guest_result_directory_entries(&entry_names)?;

    let result_path = canonical_directory.join(GUEST_RESULT_FILE_NAME);
    let path_metadata = fs::symlink_metadata(&result_path)
        .map_err(|_| "The guest result file is unavailable.".to_string())?;
    if !path_metadata.is_file() || is_reparse_or_symlink(&path_metadata)
        || path_metadata.len() == 0 || path_metadata.len() > MAX_GUEST_RESULT_BYTES as u64
    {
        return Err("The guest result must be a bounded regular file, not a link or reparse point.".into());
    }
    let canonical_result = fs::canonicalize(&result_path)
        .map_err(|_| "FRIDAY could not verify the guest result file path.".to_string())?;
    if canonical_result.parent() != Some(canonical_directory.as_path())
        || canonical_result.file_name() != Some(OsStr::new(GUEST_RESULT_FILE_NAME))
    {
        return Err("The guest result file failed its directory-boundary check.".into());
    }

    let mut file = open_source_file(&result_path)
        .map_err(|_| "The guest result file could not be opened without following links.".to_string())?;
    let opened_metadata = file.metadata()
        .map_err(|_| "FRIDAY could not verify the opened guest result file.".to_string())?;
    if !opened_metadata.is_file() || is_reparse_or_symlink(&opened_metadata)
        || opened_metadata.len() != path_metadata.len()
        || opened_metadata.len() > MAX_GUEST_RESULT_BYTES as u64
    {
        return Err("The guest result file changed or failed its size and file-type checks.".into());
    }
    let mut bytes = Vec::with_capacity(opened_metadata.len() as usize);
    file.by_ref().take(MAX_GUEST_RESULT_BYTES as u64 + 1).read_to_end(&mut bytes)
        .map_err(|_| "FRIDAY could not read the guest result file safely.".to_string())?;
    if bytes.is_empty() || bytes.len() > MAX_GUEST_RESULT_BYTES
        || bytes.len() as u64 != opened_metadata.len()
    {
        return Err("The guest result file is empty, oversized, or changed while being read.".into());
    }
    validate_guest_result_json_with_limit(
        &bytes, expected_run_id, expected_operation, max_output_bytes,
    )
}

fn validate_guest_result_json_with_limit(
    bytes: &[u8],
    expected_run_id: &str,
    expected_operation: GuestRunOperation,
    max_output_bytes: u64,
) -> Result<GuestRunResult, String> {
    if max_output_bytes == 0 || max_output_bytes > MAX_GUEST_REQUEST_OUTPUT_BYTES {
        return Err("The expected guest output limit is outside FRIDAY's policy.".into());
    }
    if bytes.is_empty() || bytes.len() > MAX_GUEST_RESULT_BYTES {
        return Err("The guest result is empty or exceeds FRIDAY's size limit.".into());
    }
    if !valid_snapshot_id(expected_run_id) {
        return Err("The expected guest run identifier is invalid.".into());
    }
    let result: GuestRunResult = serde_json::from_slice(bytes)
        .map_err(|_| "The guest result is malformed, contains duplicate or unknown fields, or is not valid UTF-8 JSON.".to_string())?;
    if result.schema_version != 1
        || result.run_id != expected_run_id
        || result.operation != expected_operation
        || result.duration_ms > MAX_GUEST_RESULT_DURATION_MS
    {
        return Err("The guest result does not match FRIDAY's run contract.".into());
    }
    let output_bytes = result.stdout.as_ref().map_or(0, |value| value.len())
        .saturating_add(result.stderr.as_ref().map_or(0, |value| value.len()));
    if output_bytes as u64 > max_output_bytes {
        return Err("The guest result diagnostics exceed the limit assigned to this run.".into());
    }
    match (result.status, result.exit_code) {
        (GuestRunStatus::Passed, Some(0)) => {}
        (GuestRunStatus::Failed, Some(code)) if code != 0 => {}
        (GuestRunStatus::TimedOut | GuestRunStatus::Cancelled | GuestRunStatus::Unsupported | GuestRunStatus::RunnerError, None) => {}
        _ => return Err("The guest result has an inconsistent status and exit code.".into()),
    }
    Ok(result)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodingSandboxPreflight {
    platform_supported: bool,
    runtime_detected: bool,
    execution_enabled: bool,
    network_isolation_enforced: bool,
    windows_edition: String,
    windows_build: String,
    sandbox_feature_state: String,
    wsb_cli_available: bool,
    wsb_cli_version: String,
    virtualization_firmware_enabled: Option<bool>,
    sandbox_process_detected: bool,
    virtual_disk_api_available: bool,
    manage_volume_privilege_enabled: bool,
    hyperv_cmdlets_available: bool,
    mapped_folders_policy: String,
    mapped_folder_write_policy: String,
    result_storage_ready: bool,
    message: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodingSandboxSnapshot {
    snapshot_id: String,
    files_copied: usize,
    bytes_copied: u64,
    truncated: bool,
    message: String,
}


#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SnapshotOwnershipMarker {
    schema_version: u32,
    snapshot_id: String,
    owner: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodingSandboxRecovery {
    removed_snapshots: usize,
    preserved_unverified_entries: usize,
    message: String,
}

static OWNED_SNAPSHOTS: Mutex<BTreeMap<String, PathBuf>> = Mutex::new(BTreeMap::new());

fn valid_snapshot_id(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn write_snapshot_ownership_marker(snapshot_path: &Path, snapshot_id: &str) -> Result<(), String> {
    let marker_path = snapshot_path.join(SNAPSHOT_OWNER_MARKER);
    let marker = SnapshotOwnershipMarker {
        schema_version: SNAPSHOT_OWNER_SCHEMA,
        snapshot_id: snapshot_id.to_string(),
        owner: "FRIDAY-CodingAssistant".to_string(),
    };
    let bytes = serde_json::to_vec(&marker)
        .map_err(|_| "FRIDAY could not prepare snapshot ownership metadata.".to_string())?;
    let mut file = OpenOptions::new().write(true).create_new(true).open(&marker_path)
        .map_err(|_| "FRIDAY could not safely create snapshot ownership metadata.".to_string())?;
    use std::io::Write;
    file.write_all(&bytes)
        .map_err(|_| "FRIDAY could not write snapshot ownership metadata.".to_string())?;
    file.sync_all()
        .map_err(|_| "FRIDAY could not persist snapshot ownership metadata.".to_string())?;
    Ok(())
}

fn verified_owned_snapshot_path(workspace_root: &Path, snapshot_id: &str) -> Result<PathBuf, String> {
    if !valid_snapshot_id(snapshot_id) {
        return Err("The snapshot identifier is invalid.".into());
    }
    let root_meta = fs::symlink_metadata(workspace_root)
        .map_err(|_| "The private snapshot workspace is unavailable.".to_string())?;
    if !root_meta.is_dir() || is_reparse_or_symlink(&root_meta) {
        return Err("The private snapshot workspace is not a normal directory.".into());
    }
    let canonical_root = fs::canonicalize(workspace_root)
        .map_err(|_| "FRIDAY could not verify the private snapshot workspace.".to_string())?;
    let candidate = workspace_root.join(snapshot_id);
    let candidate_meta = fs::symlink_metadata(&candidate)
        .map_err(|_| "The snapshot folder is unavailable.".to_string())?;
    if !candidate_meta.is_dir() || is_reparse_or_symlink(&candidate_meta) {
        return Err("The snapshot folder is not a normal directory.".into());
    }
    let canonical_candidate = fs::canonicalize(&candidate)
        .map_err(|_| "FRIDAY could not verify the snapshot folder.".to_string())?;
    // Windows canonicalization may add an extended-length prefix (\\?\) that is
    // not present in the original candidate path. Verify the canonical parent and
    // exact child name instead of comparing differently normalized path strings.
    if canonical_candidate.parent() != Some(canonical_root.as_path())
        || canonical_candidate.file_name().and_then(OsStr::to_str) != Some(snapshot_id)
    {
        return Err("The snapshot folder failed its ownership-path check.".into());
    }
    let marker_path = canonical_candidate.join(SNAPSHOT_OWNER_MARKER);
    let marker_meta = fs::symlink_metadata(&marker_path)
        .map_err(|_| "Snapshot ownership evidence is missing; cleanup was refused.".to_string())?;
    if !marker_meta.is_file() || is_reparse_or_symlink(&marker_meta) || marker_meta.len() > 1024 {
        return Err("Snapshot ownership evidence is not a small regular file; cleanup was refused.".into());
    }
    let mut marker_file = open_source_file(&marker_path)
        .map_err(|_| "Snapshot ownership evidence could not be opened without following links.".to_string())?;
    let opened_marker_meta = marker_file.metadata()
        .map_err(|_| "Snapshot ownership evidence could not be verified.".to_string())?;
    if !opened_marker_meta.is_file() || is_reparse_or_symlink(&opened_marker_meta) || opened_marker_meta.len() > 1024 {
        return Err("Snapshot ownership evidence changed or exceeded its size limit; cleanup was refused.".into());
    }
    let mut bytes = Vec::new();
    marker_file.take(1025).read_to_end(&mut bytes)
        .map_err(|_| "Snapshot ownership evidence could not be read; cleanup was refused.".to_string())?;
    if bytes.len() > 1024 {
        return Err("Snapshot ownership evidence exceeded its read limit; cleanup was refused.".into());
    }
    let marker: SnapshotOwnershipMarker = serde_json::from_slice(&bytes)
        .map_err(|_| "Snapshot ownership evidence is malformed; cleanup was refused.".to_string())?;
    if marker.schema_version != SNAPSHOT_OWNER_SCHEMA
        || marker.snapshot_id != snapshot_id
        || marker.owner != "FRIDAY-CodingAssistant"
    {
        return Err("Snapshot ownership evidence does not match this FRIDAY workspace; cleanup was refused.".into());
    }
    Ok(canonical_candidate)
}

fn remove_verified_owned_snapshot(workspace_root: &Path, snapshot_id: &str) -> Result<(), String> {
    let verified = verified_owned_snapshot_path(workspace_root, snapshot_id)?;
    fs::remove_dir_all(&verified)
        .map_err(|_| "FRIDAY could not completely remove the verified snapshot folder.".to_string())
}


fn detect_windows_sandbox_at(system_root: Option<&OsStr>) -> bool {
    let Some(system_root) = system_root else { return false; };
    let executable = PathBuf::from(system_root).join("System32").join("WindowsSandbox.exe");
    fs::symlink_metadata(executable)
        .map(|metadata| metadata.is_file() && !is_reparse_or_symlink(&metadata))
        .unwrap_or(false)
}

fn detect_windows_sandbox() -> bool {
    detect_windows_sandbox_at(env::var_os("SystemRoot").as_deref())
}

#[cfg(windows)]
pub(crate) fn is_reparse_or_symlink(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_type().is_symlink() || (metadata.file_attributes() & 0x400 != 0)
}

#[cfg(not(windows))]
fn is_reparse_or_symlink(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(windows)]
const WINDOWS_PREFLIGHT_TIMEOUT: Duration = Duration::from_secs(15);
#[cfg(windows)]
const MAX_PREFLIGHT_OUTPUT_BYTES: u64 = 64 * 1024;

#[cfg(windows)]
fn run_bounded_probe(mut command: Command) -> Result<(ExitStatus, Vec<u8>), String> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| "The Windows Sandbox capability probe could not be started.".to_string())?;

    let stdout = child.stdout.take()
        .ok_or_else(|| "The Windows Sandbox capability probe output was unavailable.".to_string())?;
    let stderr = child.stderr.take()
        .ok_or_else(|| "The Windows Sandbox capability probe diagnostics were unavailable.".to_string())?;

    let stdout_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.take(MAX_PREFLIGHT_OUTPUT_BYTES).read_to_end(&mut bytes);
        bytes
    });
    let stderr_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stderr.take(MAX_PREFLIGHT_OUTPUT_BYTES).read_to_end(&mut bytes);
        bytes
    });

    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < WINDOWS_PREFLIGHT_TIMEOUT => {
                thread::sleep(Duration::from_millis(50));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err("The Windows Sandbox capability probe timed out; readiness is unverified.".into());
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err("The Windows Sandbox capability probe could not be monitored; readiness is unverified.".into());
            }
        }
    };

    let stdout = stdout_reader.join()
        .map_err(|_| "The Windows Sandbox capability probe output could not be collected.".to_string())?;
    let _ = stderr_reader.join();
    Ok((status, stdout))
}

#[cfg(windows)]
fn detect_windows_sandbox_capabilities() -> (String, String, String, bool, String, Option<bool>, bool, bool, bool, bool, String, String) {
    // Fixed, read-only PowerShell probes. No user or model supplied command text is accepted.
    let script = r#"$ErrorActionPreference = 'SilentlyContinue'
$cv = Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion'
$feature = Get-WindowsOptionalFeature -Online -FeatureName Containers-DisposableClientVM
$cpu = Get-CimInstance Win32_Processor | Select-Object -First 1 -ExpandProperty VirtualizationFirmwareEnabled
$active = [bool](Get-Process -Name WindowsSandbox -ErrorAction SilentlyContinue)
$wsb = Get-Command wsb -ErrorAction SilentlyContinue
$version = ''
if ($wsb -and $wsb.Source) {
  $version = [string](Get-Item -LiteralPath $wsb.Source -ErrorAction SilentlyContinue).VersionInfo.FileVersion
}
$privileges = (& whoami.exe /priv 2>$null | Out-String)
$virtualDiskApi = Test-Path -LiteralPath (Join-Path $env:SystemRoot 'System32\virtdisk.dll')
$hypervCmdlets = [bool](Get-Command New-VHD -ErrorAction SilentlyContinue) -and [bool](Get-Command Mount-VHD -ErrorAction SilentlyContinue)
$sandboxPolicy = $null
$policyReadFailed = $false
try {
  $sandboxPolicy = Get-ItemProperty -LiteralPath 'HKLM:\SOFTWARE\Policies\Microsoft\Windows\Sandbox' -ErrorAction Stop
} catch [System.Management.Automation.ItemNotFoundException] {
  $sandboxPolicy = $null
} catch {
  $policyReadFailed = $true
}
$mappedFoldersPolicy = if ($policyReadFailed) { -1 } elseif ($null -eq $sandboxPolicy -or $null -eq $sandboxPolicy.AllowMappedFolders) { $null } else { try { [int]$sandboxPolicy.AllowMappedFolders } catch { -1 } }
$mappedFolderWritePolicy = if ($policyReadFailed) { -1 } elseif ($null -eq $sandboxPolicy -or $null -eq $sandboxPolicy.AllowWriteToMappedFolders) { $null } else { try { [int]$sandboxPolicy.AllowWriteToMappedFolders } catch { -1 } }
[pscustomobject]@{
  edition = [string]$cv.EditionID
  build = [string]$cv.CurrentBuildNumber
  feature = [string]$feature.State
  wsbAvailable = [bool]$wsb
  wsbVersion = [string]$version
  virtualizationFirmwareEnabled = if ($null -eq $cpu) { $null } else { [bool]$cpu }
  sandboxProcessDetected = $active
  privileges = [string]$privileges
  virtualDiskApiAvailable = [bool]$virtualDiskApi
  hypervCmdletsAvailable = [bool]$hypervCmdlets
  mappedFoldersPolicy = $mappedFoldersPolicy
  mappedFolderWritePolicy = $mappedFolderWritePolicy
} | ConvertTo-Json -Compress"#;
    let mut command = Command::new("powershell.exe");
    command.args(["-NoProfile", "-NonInteractive", "-Command", script]);
    let Ok((status, stdout)) = run_bounded_probe(command) else {
        return ("Unknown".into(), "Unknown".into(), "Unknown".into(), false, "Unavailable".into(), None, false, false, false, false, "Unknown".into(), "Unknown".into());
    };
    if !status.success() {
        return ("Unknown".into(), "Unknown".into(), "Unknown".into(), false, "Unavailable".into(), None, false, false, false, false, "Unknown".into(), "Unknown".into());
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&stdout) else {
        return ("Unknown".into(), "Unknown".into(), "Unknown".into(), false, "Unavailable".into(), None, false, false, false, false, "Unknown".into(), "Unknown".into());
    };
    let string_field = |key: &str| value.get(key).and_then(|v| v.as_str()).filter(|s| !s.is_empty()).unwrap_or("Unknown").to_string();
    let firmware = value.get("virtualizationFirmwareEnabled").and_then(serde_json::Value::as_bool);
    (
        string_field("edition"),
        string_field("build"),
        string_field("feature"),
        value.get("wsbAvailable").and_then(serde_json::Value::as_bool).unwrap_or(false),
        string_field("wsbVersion"),
        firmware,
        value.get("sandboxProcessDetected").and_then(serde_json::Value::as_bool).unwrap_or(false),
        value.get("virtualDiskApiAvailable").and_then(serde_json::Value::as_bool).unwrap_or(false),
        privilege_output_has_enabled(&value.get("privileges").and_then(serde_json::Value::as_str).unwrap_or(""), "SeManageVolumePrivilege"),
        value.get("hypervCmdletsAvailable").and_then(serde_json::Value::as_bool).unwrap_or(false),
        sandbox_policy_state(value.get("mappedFoldersPolicy")),
        sandbox_policy_state(value.get("mappedFolderWritePolicy")),
    )
}

fn sandbox_policy_state(value: Option<&serde_json::Value>) -> String {
    match value {
        None | Some(serde_json::Value::Null) => "Not configured".into(),
        Some(value) if value.as_i64() == Some(0) => "Blocked".into(),
        Some(value) if value.as_i64() == Some(1) => "Allowed".into(),
        _ => "Unknown".into(),
    }
}

fn privilege_output_has_enabled(output: &str, privilege: &str) -> bool {
    output.lines().any(|line| {
        let mut fields = line.split_whitespace();
        fields.next() == Some(privilege) && fields.last() == Some("Enabled")
    })
}

#[tauri::command]
pub async fn coding_sandbox_preflight() -> Result<CodingSandboxPreflight, String> {
    let _project_root = crate::coding_assistant::selected_project_root_for_sandbox()?;
    #[cfg(windows)]
    {
        let detected = detect_windows_sandbox();
        let (edition, build, feature, wsb_available, wsb_version, virtualization, active,
            virtual_disk_api, manage_volume_privilege, hyperv_cmdlets,
            mapped_folders_policy, mapped_folder_write_policy) = detect_windows_sandbox_capabilities();
        let edition_supported = ["Professional", "Enterprise", "Education"]
            .iter().any(|supported| edition.eq_ignore_ascii_case(supported));
        let runtime_ready = detected && edition_supported
            && feature.eq_ignore_ascii_case("Enabled") && wsb_available
            && virtualization == Some(true) && !active;
        let message = if active {
            "A Windows Sandbox process is already running. FRIDAY will not attach to or stop a session it did not create. Execution remains disabled.".to_string()
        } else if runtime_ready {
            "Windows Sandbox prerequisites appear present. This is detection only, not a launch test. Result-storage readiness is not established; execution remains disabled until storage, command restrictions, resource enforcement, cancellation and result validation are verified.".to_string()
        } else {
            "Windows Sandbox is not yet confirmed ready. Review the detected edition, feature, CLI, virtualization and result-storage capability details. Unknown values are treated as unverified. FRIDAY will not elevate or fall back to host execution.".to_string()
        };
        Ok(CodingSandboxPreflight {
            platform_supported: true, runtime_detected: detected,
            execution_enabled: false, network_isolation_enforced: false,
            windows_edition: edition, windows_build: build, sandbox_feature_state: feature,
            wsb_cli_available: wsb_available, wsb_cli_version: wsb_version,
            virtualization_firmware_enabled: virtualization, sandbox_process_detected: active,
            virtual_disk_api_available: virtual_disk_api,
            manage_volume_privilege_enabled: manage_volume_privilege,
            hyperv_cmdlets_available: hyperv_cmdlets,
            mapped_folders_policy,
            mapped_folder_write_policy,
            result_storage_ready: false,
            message,
        })
    }
    #[cfg(not(windows))]
    {
        Ok(CodingSandboxPreflight {
            platform_supported: false, runtime_detected: false,
            execution_enabled: false, network_isolation_enforced: false,
            windows_edition: "Not applicable".into(), windows_build: "Not applicable".into(),
            sandbox_feature_state: "Not applicable".into(), wsb_cli_available: false,
            wsb_cli_version: "Not applicable".into(), virtualization_firmware_enabled: None,
            sandbox_process_detected: false,
            virtual_disk_api_available: false, manage_volume_privilege_enabled: false,
            hyperv_cmdlets_available: false,
            mapped_folders_policy: "Not applicable".into(),
            mapped_folder_write_policy: "Not applicable".into(),
            result_storage_ready: false,
            message: "The Coding Assistant execution sandbox is designed for Windows Sandbox. No project code has been executed and no host-execution fallback is available.".to_string(),
        })
    }
}

fn safe_project_file_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if lower.is_empty()
        || lower.starts_with('.')
        || ["secret", "credential", "password", "token", "private", "id_rsa", ".env"]
            .iter().any(|needle| lower.contains(needle))
    {
        return false;
    }
    matches!(
        Path::new(name).extension().and_then(OsStr::to_str).map(str::to_ascii_lowercase).as_deref(),
        Some("rs" | "toml" | "json" | "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs"
            | "py" | "pyi" | "go" | "mod" | "sum" | "java" | "kt" | "kts" | "cs"
            | "fs" | "fsx" | "vb" | "cpp" | "c" | "h" | "hpp" | "cc" | "html"
            | "css" | "scss" | "sass" | "md" | "mdx" | "yaml" | "yml" | "xml"
            | "sql" | "sh" | "bat" | "ps1" | "txt" | "lock" | "gradle" | "properties")
    ) || matches!(lower.as_str(), "makefile" | "dockerfile" | "cmakelists.txt")
}

fn excluded_directory(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    name.starts_with('.') || matches!(
        lower.as_str(),
        "node_modules" | "target" | "dist" | "build" | "out" | "coverage"
            | "vendor" | ".git" | ".svn" | ".hg" | ".idea" | ".vscode"
            | ".friday-code-backups" | "venv" | ".venv" | "__pycache__"
    )
}

struct SnapshotBudget {
    files: usize,
    bytes: u64,
    truncated: bool,
}

fn safe_relative_path(path: &Path) -> bool {
    path.components().all(|component| matches!(component, Component::Normal(_)))
}

#[cfg(windows)]
fn open_source_file(path: &Path) -> Result<File, String> {
    use std::os::windows::fs::OpenOptionsExt;
    // FILE_FLAG_OPEN_REPARSE_POINT: inspect the link itself instead of following it.
    OpenOptions::new().read(true).custom_flags(0x0020_0000).open(path)
        .map_err(|_| "A project file could not be opened safely.".to_string())
}

#[cfg(unix)]
fn open_source_file(path: &Path) -> Result<File, String> {
    use std::os::unix::fs::OpenOptionsExt;
    // O_NOFOLLOW closes the final-component symlink race between lstat-style
    // validation and open. Parent-directory identity is still checked by callers.
    OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(path)
        .map_err(|_| "A project file could not be opened safely.".to_string())
}

#[cfg(not(any(windows, unix)))]
fn open_source_file(path: &Path) -> Result<File, String> {
    File::open(path).map_err(|_| "A project file could not be opened safely.".to_string())
}

fn copy_snapshot_tree(
    source_root: &Path,
    current: &Path,
    destination_root: &Path,
    depth: usize,
    budget: &mut SnapshotBudget,
) -> Result<(), String> {
    if depth > MAX_SNAPSHOT_DEPTH {
        budget.truncated = true;
        return Ok(());
    }
    let entries = fs::read_dir(current)
        .map_err(|_| "FRIDAY could not safely read part of the selected project.".to_string())?;
    for entry in entries {
        if budget.files >= MAX_SNAPSHOT_FILES || budget.bytes >= MAX_SNAPSHOT_TOTAL_BYTES {
            budget.truncated = true;
            break;
        }
        let entry = match entry { Ok(value) => value, Err(_) => { budget.truncated = true; continue; } };
        let name = entry.file_name();
        let Some(name_text) = name.to_str() else { budget.truncated = true; continue; };
        let source = entry.path();
        let metadata = match fs::symlink_metadata(&source) {
            Ok(value) => value,
            Err(_) => { budget.truncated = true; continue; }
        };
        if is_reparse_or_symlink(&metadata) { continue; }
        if metadata.is_dir() {
            if excluded_directory(name_text) { continue; }
            let relative = source.strip_prefix(source_root).map_err(|_| "Project path escaped its selected root.".to_string())?;
            if !safe_relative_path(relative) { continue; }
            let target_dir = destination_root.join(relative);
            fs::create_dir_all(&target_dir).map_err(|_| "FRIDAY could not prepare the isolated project copy.".to_string())?;
            copy_snapshot_tree(source_root, &source, destination_root, depth + 1, budget)?;
            if budget.truncated { break; }
            continue;
        }
        if !metadata.is_file() || !safe_project_file_name(name_text)
            || metadata.len() == 0 || metadata.len() > MAX_SNAPSHOT_FILE_BYTES
        {
            continue;
        }
        let relative = source.strip_prefix(source_root).map_err(|_| "Project path escaped its selected root.".to_string())?;
        if !safe_relative_path(relative) { continue; }
        let canonical = match fs::canonicalize(&source) { Ok(value) => value, Err(_) => continue };
        if !canonical.starts_with(source_root) { continue; }

        let mut file = match open_source_file(&source) {
            Ok(value) => value,
            Err(_) => { budget.truncated = true; continue; }
        };
        let opened_metadata = file.metadata().map_err(|_| "FRIDAY could not verify a project file.".to_string())?;
        if is_reparse_or_symlink(&opened_metadata) || !opened_metadata.is_file()
            || opened_metadata.len() != metadata.len()
        { continue; }
        let remaining = MAX_SNAPSHOT_TOTAL_BYTES.saturating_sub(budget.bytes);
        let limit = MAX_SNAPSHOT_FILE_BYTES.min(remaining);
        let mut contents = Vec::with_capacity(opened_metadata.len() as usize);
        file.take(limit.saturating_add(1)).read_to_end(&mut contents)
            .map_err(|_| "FRIDAY could not read a project file safely.".to_string())?;
        if contents.is_empty() || contents.len() as u64 > limit {
            budget.truncated = true;
            continue;
        }
        let after = fs::symlink_metadata(&source).map_err(|_| "A project file changed during snapshot preparation.".to_string())?;
        if is_reparse_or_symlink(&after) || after.len() != opened_metadata.len() { continue; }

        let target = destination_root.join(relative);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|_| "FRIDAY could not prepare the isolated project copy.".to_string())?;
        }
        let mut output = OpenOptions::new().write(true).create_new(true).open(&target)
            .map_err(|_| "FRIDAY could not create a new isolated snapshot file.".to_string())?;
        use std::io::Write;
        output.write_all(&contents).map_err(|_| "FRIDAY could not finish writing the isolated project copy.".to_string())?;
        output.sync_all().map_err(|_| "FRIDAY could not verify the isolated project copy was written.".to_string())?;
        budget.files += 1;
        budget.bytes = budget.bytes.saturating_add(contents.len() as u64);
    }
    Ok(())
}

#[tauri::command]
pub async fn coding_sandbox_prepare_snapshot(app: AppHandle) -> Result<CodingSandboxSnapshot, String> {
    let source_root = crate::coding_assistant::selected_project_root_for_sandbox()?;
    let root_metadata = fs::symlink_metadata(&source_root)
        .map_err(|_| "The selected project folder is no longer available.".to_string())?;
    if !root_metadata.is_dir() || is_reparse_or_symlink(&root_metadata) {
        return Err("The selected project folder is not a safe normal directory.".into());
    }

    let app_data = app.path().app_local_data_dir()
        .map_err(|_| "FRIDAY could not locate its private application data folder.".to_string())?;
    let workspace_root = app_data.join("coding-sandbox-workspaces");
    fs::create_dir_all(&workspace_root)
        .map_err(|_| "FRIDAY could not create private sandbox workspace storage.".to_string())?;
    let workspace_meta = fs::symlink_metadata(&workspace_root)
        .map_err(|_| "FRIDAY could not verify private sandbox workspace storage.".to_string())?;
    if !workspace_meta.is_dir() || is_reparse_or_symlink(&workspace_meta) {
        return Err("Private sandbox workspace storage is not a safe normal directory.".into());
    }

    let mut random = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut random);
    let snapshot_id = random.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    let snapshot_path = workspace_root.join(&snapshot_id);
    fs::create_dir(&snapshot_path)
        .map_err(|_| "FRIDAY could not create a unique isolated snapshot folder.".to_string())?;
    if let Err(error) = write_snapshot_ownership_marker(&snapshot_path, &snapshot_id) {
        let _ = fs::remove_dir(&snapshot_path);
        return Err(error);
    }

    let mut budget = SnapshotBudget { files: 0, bytes: 0, truncated: false };
    let result = copy_snapshot_tree(&source_root, &source_root, &snapshot_path, 0, &mut budget);
    if let Err(error) = result {
        let _ = remove_verified_owned_snapshot(&workspace_root, &snapshot_id);
        return Err(error);
    }
    if budget.files == 0 {
        let _ = remove_verified_owned_snapshot(&workspace_root, &snapshot_id);
        return Err("No eligible source or project-manifest files were found to snapshot.".into());
    }

    OWNED_SNAPSHOTS.lock()
        .map_err(|_| "Snapshot ownership tracking is temporarily unavailable.".to_string())?
        .insert(snapshot_id.clone(), snapshot_path);
    Ok(CodingSandboxSnapshot {
        snapshot_id,
        files_copied: budget.files,
        bytes_copied: budget.bytes,
        truncated: budget.truncated,
        message: if budget.truncated {
            "A bounded partial copy was prepared. Some files or folders were skipped or exceeded safety limits; the original project was not changed. Execution remains disabled.".into()
        } else {
            "A bounded isolated project copy was prepared. The original project was not changed. Execution remains disabled.".into()
        },
    })
}

fn xml_escape(value: &str) -> String {
    value.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn build_sandbox_config(snapshot_path: &Path) -> Result<String, String> {
    let metadata = fs::symlink_metadata(snapshot_path)
        .map_err(|_| "The isolated snapshot is unavailable.".to_string())?;
    if !metadata.is_dir() || is_reparse_or_symlink(&metadata) {
        return Err("The snapshot is not a safe normal directory.".into());
    }
    let canonical = fs::canonicalize(snapshot_path)
        .map_err(|_| "FRIDAY could not verify the snapshot path.".to_string())?;
    if canonical.as_path() != snapshot_path {
        return Err("The snapshot path changed during validation.".into());
    }
    let host_folder = canonical.to_str()
        .ok_or_else(|| "The snapshot path cannot be represented safely in Windows Sandbox configuration.".to_string())?;
    if !canonical.is_absolute() || canonical.components().any(|part| matches!(part, Component::ParentDir)) {
        return Err("The snapshot path is not a safe absolute path.".into());
    }
    let escaped = xml_escape(host_folder);
    Ok(format!(r#"<Configuration>
  <VGpu>Disable</VGpu>
  <Networking>Disable</Networking>
  <AudioInput>Disable</AudioInput>
  <VideoInput>Disable</VideoInput>
  <ClipboardRedirection>Disable</ClipboardRedirection>
  <PrinterRedirection>Disable</PrinterRedirection>
  <ProtectedClient>Enable</ProtectedClient>
  <MappedFolders>
    <MappedFolder>
      <HostFolder>{escaped}</HostFolder>
      <SandboxFolder>C:\FRIDAY\Project</SandboxFolder>
      <ReadOnly>true</ReadOnly>
    </MappedFolder>
  </MappedFolders>
</Configuration>
"#))
}

#[tauri::command]
pub async fn coding_sandbox_recover_workspaces(app: AppHandle) -> Result<CodingSandboxRecovery, String> {
    let app_data = app.path().app_local_data_dir()
        .map_err(|_| "FRIDAY could not locate its private application data folder.".to_string())?;
    let workspace_root = app_data.join("coding-sandbox-workspaces");
    let root_meta = match fs::symlink_metadata(&workspace_root) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(CodingSandboxRecovery {
                removed_snapshots: 0,
                preserved_unverified_entries: 0,
                message: "No previous Coding Assistant sandbox workspaces were found.".into(),
            });
        }
        Err(_) => return Err("FRIDAY could not inspect its private sandbox workspace.".into()),
    };
    if !root_meta.is_dir() || is_reparse_or_symlink(&root_meta) {
        return Err("The private sandbox workspace is not a normal directory; recovery was refused.".into());
    }
    let entries = fs::read_dir(&workspace_root)
        .map_err(|_| "FRIDAY could not list its private sandbox workspace.".to_string())?;
    let mut removed = 0usize;
    let mut preserved = 0usize;
    for entry in entries {
        let Ok(entry) = entry else { preserved += 1; continue; };
        let entry_name = entry.file_name();
        let Some(name) = entry_name.to_str().map(str::to_string) else { preserved += 1; continue; };
        if !valid_snapshot_id(&name) {
            preserved += 1;
            continue;
        }
        // A component remount must not delete a snapshot still owned by this process.
        let current_session_owns_it = OWNED_SNAPSHOTS.lock()
            .map(|owned| owned.contains_key(&name))
            .unwrap_or(true);
        if current_session_owns_it {
            preserved += 1;
            continue;
        }
        match remove_verified_owned_snapshot(&workspace_root, &name) {
            Ok(()) => {
                removed += 1;
                if let Ok(mut owned) = OWNED_SNAPSHOTS.lock() { owned.remove(&name); }
            }
            Err(_) => preserved += 1,
        }
    }
    Ok(CodingSandboxRecovery {
        removed_snapshots: removed,
        preserved_unverified_entries: preserved,
        message: format!("Recovery removed {removed} verified FRIDAY-owned snapshot(s) and preserved {preserved} unverified or unknown entrie(s). Original project folders are never scanned or deleted."),
    })
}

#[tauri::command]
pub async fn coding_sandbox_config_preview(snapshot_id: String) -> Result<String, String> {
    if !valid_snapshot_id(&snapshot_id) {
        return Err("The snapshot identifier is invalid.".into());
    }
    let owned = OWNED_SNAPSHOTS.lock()
        .map_err(|_| "Snapshot ownership tracking is temporarily unavailable.".to_string())?;
    let path = owned.get(&snapshot_id)
        .ok_or_else(|| "This snapshot is not owned by the current FRIDAY session.".to_string())?;
    build_sandbox_config(path)
}

#[tauri::command]
pub async fn coding_sandbox_discard_snapshot(snapshot_id: String) -> Result<String, String> {
    if !valid_snapshot_id(&snapshot_id) {
        return Err("The snapshot identifier is invalid.".into());
    }
    let mut owned = OWNED_SNAPSHOTS.lock()
        .map_err(|_| "Snapshot ownership tracking is temporarily unavailable.".to_string())?;
    let path = owned.get(&snapshot_id).cloned()
        .ok_or_else(|| "This snapshot is not owned by the current FRIDAY session.".to_string())?;
    let parent = path.parent()
        .ok_or_else(|| "The snapshot folder has no verified parent.".to_string())?;
    remove_verified_owned_snapshot(parent, &snapshot_id)?;
    owned.remove(&snapshot_id);
    Ok("The isolated snapshot copy was removed. The original project was not changed.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_directory(label: &str) -> PathBuf {
        let mut random = [0u8; 12];
        rand::thread_rng().fill_bytes(&mut random);
        let suffix = random.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
        let path = env::temp_dir().join(format!("friday-sandbox-{label}-{suffix}"));
        fs::create_dir_all(&path).expect("test directory should be created");
        path
    }


    fn guest_result_json(status: &str, exit_code: &str) -> Vec<u8> {
        format!(
            r#"{{"schemaVersion":1,"runId":"0123456789abcdef0123456789abcdef","operation":"rust-cargo-test","status":"{status}"{exit_code},"stdout":"ok","stderr":"","truncated":false,"durationMs":25}}"#
        ).into_bytes()
    }


    fn valid_guest_request_json() -> Vec<u8> {
        br#"{"schemaVersion":1,"runId":"0123456789abcdef0123456789abcdef","snapshotId":"abcdef0123456789abcdef0123456789","operation":"rust-cargo-test","maxDurationMs":120000,"maxOutputBytes":1048576}"#.to_vec()
    }

    #[test]
    fn guest_request_accepts_only_fixed_allowlisted_bounded_contract() {
        let request = parse_guest_run_request(&valid_guest_request_json()).unwrap();
        assert_eq!(request.operation, GuestRunOperation::RustCargoTest);
        assert_eq!(request.max_duration_ms, MAX_GUEST_REQUEST_DURATION_MS);
        assert_eq!(request.max_output_bytes, MAX_GUEST_REQUEST_OUTPUT_BYTES);
    }

    #[test]
    fn guest_request_rejects_unknown_fields_and_duplicate_keys() {
        let unknown = br#"{"schemaVersion":1,"runId":"0123456789abcdef0123456789abcdef","snapshotId":"abcdef0123456789abcdef0123456789","operation":"rust-cargo-test","maxDurationMs":1000,"maxOutputBytes":1000,"command":"cmd.exe"}"#;
        assert!(parse_guest_run_request(unknown).is_err());
        let duplicate = br#"{"schemaVersion":1,"schemaVersion":1,"runId":"0123456789abcdef0123456789abcdef","snapshotId":"abcdef0123456789abcdef0123456789","operation":"rust-cargo-test","maxDurationMs":1000,"maxOutputBytes":1000}"#;
        assert!(parse_guest_run_request(duplicate).is_err());
    }

    #[test]
    fn guest_request_rejects_invalid_ids_versions_and_resource_limits() {
        let mut value: serde_json::Value = serde_json::from_slice(&valid_guest_request_json()).unwrap();
        value["schemaVersion"] = serde_json::json!(2);
        assert!(parse_guest_run_request(&serde_json::to_vec(&value).unwrap()).is_err());
        value["schemaVersion"] = serde_json::json!(1);
        value["runId"] = serde_json::json!("../outside");
        assert!(parse_guest_run_request(&serde_json::to_vec(&value).unwrap()).is_err());
        value["runId"] = serde_json::json!("0123456789abcdef0123456789abcdef");
        value["snapshotId"] = serde_json::json!("../../project");
        assert!(parse_guest_run_request(&serde_json::to_vec(&value).unwrap()).is_err());
        value["snapshotId"] = serde_json::json!("abcdef0123456789abcdef0123456789");
        value["maxDurationMs"] = serde_json::json!(MAX_GUEST_REQUEST_DURATION_MS + 1);
        assert!(parse_guest_run_request(&serde_json::to_vec(&value).unwrap()).is_err());
        value["maxDurationMs"] = serde_json::json!(0);
        assert!(parse_guest_run_request(&serde_json::to_vec(&value).unwrap()).is_err());
        value["maxDurationMs"] = serde_json::json!(1000);
        value["maxOutputBytes"] = serde_json::json!(MAX_GUEST_REQUEST_OUTPUT_BYTES + 1);
        assert!(parse_guest_run_request(&serde_json::to_vec(&value).unwrap()).is_err());
        value["maxOutputBytes"] = serde_json::json!(0);
        assert!(parse_guest_run_request(&serde_json::to_vec(&value).unwrap()).is_err());
    }

    #[test]
    fn guest_request_rejects_oversized_documents() {
        assert!(parse_guest_run_request(&vec![b' '; 4097]).is_err());
    }


    #[test]
    fn guest_result_directory_requires_exactly_one_expected_file() {
        assert!(validate_guest_result_directory_entries(&["result.json".into()]).is_ok());
        assert!(validate_guest_result_directory_entries(&[]).is_err());
        assert!(validate_guest_result_directory_entries(&["other.json".into()]).is_err());
        assert!(validate_guest_result_directory_entries(&["result.json".into(), "extra.txt".into()]).is_err());
        assert!(validate_guest_result_directory_entries(&["RESULT.JSON".into()]).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn open_source_file_opens_windows_symlink_itself_not_its_target() {
        use std::os::windows::fs::symlink_file;
        let directory = make_test_directory("windows-no-follow-open");
        let target = directory.join("target.txt");
        let link = directory.join("link.txt");
        fs::write(&target, b"trusted target").unwrap();
        symlink_file(&target, &link)
            .expect("Windows CI must permit creating a file symlink for the no-follow regression test");
        let opened = open_source_file(&link).expect("opening the reparse point itself should succeed");
        let metadata = opened.metadata().unwrap();
        assert!(is_reparse_or_symlink(&metadata), "opened handle must identify the reparse point itself");
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn open_source_file_rejects_symlinks_without_following_them() {
        let directory = make_test_directory("no-follow-open");
        let target = directory.join("target.txt");
        let link = directory.join("link.txt");
        fs::write(&target, b"trusted target").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(open_source_file(&link).is_err());
        let mut opened = open_source_file(&target).unwrap();
        let mut contents = Vec::new();
        opened.read_to_end(&mut contents).unwrap();
        assert_eq!(contents, b"trusted target");
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn guest_result_file_reader_accepts_only_bounded_regular_result_file() {
        let directory = make_test_directory("guest-result");
        let run_id = "0123456789abcdef0123456789abcdef";
        fs::write(directory.join(GUEST_RESULT_FILE_NAME), guest_result_json("passed", r#","exitCode":0"#)).unwrap();
        let result = read_verified_guest_result_file(
            &directory, run_id, GuestRunOperation::RustCargoTest, MAX_GUEST_RESULT_BYTES as u64,
        ).unwrap();
        assert_eq!(result.status, GuestRunStatus::Passed);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn guest_result_file_reader_rejects_extra_missing_oversized_and_linked_entries() {
        let run_id = "0123456789abcdef0123456789abcdef";
        let directory = make_test_directory("guest-result-invalid");
        assert!(read_verified_guest_result_file(
            &directory, run_id, GuestRunOperation::RustCargoTest, MAX_GUEST_RESULT_BYTES as u64,
        ).is_err());
        fs::write(directory.join(GUEST_RESULT_FILE_NAME), guest_result_json("passed", r#","exitCode":0"#)).unwrap();
        fs::write(directory.join("unexpected.txt"), b"extra").unwrap();
        assert!(read_verified_guest_result_file(
            &directory, run_id, GuestRunOperation::RustCargoTest, MAX_GUEST_RESULT_BYTES as u64,
        ).is_err());
        fs::remove_file(directory.join("unexpected.txt")).unwrap();
        fs::write(directory.join(GUEST_RESULT_FILE_NAME), vec![b'x'; MAX_GUEST_RESULT_BYTES + 1]).unwrap();
        assert!(read_verified_guest_result_file(
            &directory, run_id, GuestRunOperation::RustCargoTest, MAX_GUEST_RESULT_BYTES as u64,
        ).is_err());
        fs::remove_file(directory.join(GUEST_RESULT_FILE_NAME)).unwrap();
        #[cfg(unix)]
        {
            let outside = directory.join("outside.json");
            fs::write(&outside, guest_result_json("passed", r#","exitCode":0"#)).unwrap();
            std::os::unix::fs::symlink(&outside, directory.join(GUEST_RESULT_FILE_NAME)).unwrap();
            assert!(read_verified_guest_result_file(
                &directory, run_id, GuestRunOperation::RustCargoTest, MAX_GUEST_RESULT_BYTES as u64,
            ).is_err());
        }
        fs::remove_dir_all(directory).unwrap();
    }


    #[cfg(unix)]
    #[test]
    fn guest_result_file_reader_rejects_non_text_entry_names() {
        use std::os::unix::ffi::OsStringExt;
        let directory = make_test_directory("guest-result-non-text-name");
        let invalid_name = std::ffi::OsString::from_vec(vec![b'n', b'o', b'n', 0xff]);
        fs::write(directory.join(invalid_name), b"unexpected").unwrap();
        assert!(read_verified_guest_result_file(
            &directory,
            "0123456789abcdef0123456789abcdef",
            GuestRunOperation::RustCargoTest,
            MAX_GUEST_RESULT_BYTES as u64,
        ).is_err());
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn guest_result_file_reader_rejects_symlinked_result_directory() {
        let parent = make_test_directory("guest-result-directory-link");
        let real_directory = parent.join("real");
        fs::create_dir(&real_directory).unwrap();
        let linked_directory = parent.join("linked");
        std::os::unix::fs::symlink(&real_directory, &linked_directory).unwrap();
        assert!(read_verified_guest_result_file(
            &linked_directory,
            "0123456789abcdef0123456789abcdef",
            GuestRunOperation::RustCargoTest,
            MAX_GUEST_RESULT_BYTES as u64,
        ).is_err());
        fs::remove_file(linked_directory).unwrap();
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn guest_result_accepts_only_matching_bounded_success_contract() {
        let bytes = guest_result_json("passed", r#","exitCode":0"#);
        let result = validate_guest_result_json(
            &bytes,
            "0123456789abcdef0123456789abcdef",
            GuestRunOperation::RustCargoTest,
        ).unwrap();
        assert_eq!(result.status, GuestRunStatus::Passed);
        assert_eq!(result.exit_code, Some(0));
    }

    #[test]
    fn guest_result_rejects_malformed_unknown_duplicate_and_mismatched_data() {
        let run_id = "0123456789abcdef0123456789abcdef";
        let operation = GuestRunOperation::RustCargoTest;
        assert!(validate_guest_result_json(b"{", run_id, operation).is_err());
        let unknown = br#"{"schemaVersion":1,"runId":"0123456789abcdef0123456789abcdef","operation":"rust-cargo-test","status":"passed","exitCode":0,"truncated":false,"durationMs":1,"extra":true}"#;
        assert!(validate_guest_result_json(unknown, run_id, operation).is_err());
        let duplicate = br#"{"schemaVersion":1,"schemaVersion":1,"runId":"0123456789abcdef0123456789abcdef","operation":"rust-cargo-test","status":"passed","exitCode":0,"truncated":false,"durationMs":1}"#;
        assert!(validate_guest_result_json(duplicate, run_id, operation).is_err());
        let bytes = guest_result_json("passed", r#","exitCode":0"#);
        assert!(validate_guest_result_json(&bytes, "ffffffffffffffffffffffffffffffff", operation).is_err());
        assert!(validate_guest_result_json(&bytes, run_id, GuestRunOperation::PythonUnittest).is_err());
    }

    #[test]
    fn guest_result_rejects_inconsistent_status_exit_and_excessive_limits() {
        let run_id = "0123456789abcdef0123456789abcdef";
        let operation = GuestRunOperation::RustCargoTest;
        assert!(validate_guest_result_json(&guest_result_json("passed", r#","exitCode":1"#), run_id, operation).is_err());
        assert!(validate_guest_result_json(&guest_result_json("failed", r#","exitCode":0"#), run_id, operation).is_err());
        assert!(validate_guest_result_json(&guest_result_json("timed_out", r#","exitCode":124"#), run_id, operation).is_err());
        let long_output = "x".repeat(MAX_GUEST_RESULT_BYTES + 1);
        let oversized = serde_json::json!({
            "schemaVersion": 1, "runId": run_id, "operation": "rust-cargo-test",
            "status": "passed", "exitCode": 0, "stdout": long_output,
            "stderr": "", "truncated": true, "durationMs": 1
        });
        assert!(validate_guest_result_json(&serde_json::to_vec(&oversized).unwrap(), run_id, operation).is_err());
        let long_duration = br#"{"schemaVersion":1,"runId":"0123456789abcdef0123456789abcdef","operation":"rust-cargo-test","status":"passed","exitCode":0,"truncated":false,"durationMs":120001}"#;
        assert!(validate_guest_result_json(long_duration, run_id, operation).is_err());
    }

    #[test]
    fn guest_result_enforces_the_exact_per_run_output_budget() {
        let run_id = "0123456789abcdef0123456789abcdef";
        let operation = GuestRunOperation::RustCargoTest;
        let value = serde_json::json!({
            "schemaVersion": 1,
            "runId": run_id,
            "operation": "rust-cargo-test",
            "status": "passed",
            "exitCode": 0,
            "stdout": "123456",
            "stderr": "7890",
            "truncated": false,
            "durationMs": 1
        });
        let bytes = serde_json::to_vec(&value).unwrap();
        assert!(validate_guest_result_json_with_limit(&bytes, run_id, operation, 10).is_ok());
        assert!(validate_guest_result_json_with_limit(&bytes, run_id, operation, 9).is_err());
        assert!(validate_guest_result_json_with_limit(&bytes, run_id, operation, 0).is_err());
        assert!(validate_guest_result_json_with_limit(
            &bytes, run_id, operation, MAX_GUEST_REQUEST_OUTPUT_BYTES + 1
        ).is_err());
    }

    #[test]
    fn guest_result_rejects_oversized_or_invalid_expected_identifiers() {
        let bytes = guest_result_json("passed", r#","exitCode":0"#);
        assert!(validate_guest_result_json(&vec![b'x'; MAX_GUEST_RESULT_BYTES + 1], "0123456789abcdef0123456789abcdef", GuestRunOperation::RustCargoTest).is_err());
        assert!(validate_guest_result_json(&bytes, "../bad", GuestRunOperation::RustCargoTest).is_err());
    }

    #[test]
    fn ownership_marker_allows_cleanup_only_for_matching_snapshot() {
        let workspace = make_test_directory("owned-marker");
        let id = "0123456789abcdef0123456789abcdef";
        let snapshot = workspace.join(id);
        fs::create_dir(&snapshot).unwrap();
        write_snapshot_ownership_marker(&snapshot, id).unwrap();
        assert_eq!(verified_owned_snapshot_path(&workspace, id).unwrap(), fs::canonicalize(&snapshot).unwrap());
        remove_verified_owned_snapshot(&workspace, id).unwrap();
        assert!(!snapshot.exists());
        fs::remove_dir_all(workspace).unwrap();
    }

    #[test]
    fn ownership_recovery_refuses_missing_malformed_or_mismatched_markers() {
        let workspace = make_test_directory("bad-marker");
        let id = "0123456789abcdef0123456789abcdef";
        let snapshot = workspace.join(id);
        fs::create_dir(&snapshot).unwrap();
        assert!(verified_owned_snapshot_path(&workspace, id).is_err());
        fs::write(snapshot.join(SNAPSHOT_OWNER_MARKER), b"{malformed").unwrap();
        assert!(verified_owned_snapshot_path(&workspace, id).is_err());
        fs::write(snapshot.join(SNAPSHOT_OWNER_MARKER), serde_json::to_vec(&SnapshotOwnershipMarker {
            schema_version: SNAPSHOT_OWNER_SCHEMA,
            snapshot_id: "ffffffffffffffffffffffffffffffff".into(),
            owner: "FRIDAY-CodingAssistant".into(),
        }).unwrap()).unwrap();
        assert!(verified_owned_snapshot_path(&workspace, id).is_err());
        assert!(snapshot.exists(), "unverified folders must be preserved");
        fs::remove_dir_all(workspace).unwrap();
    }

    #[test]
    fn ownership_recovery_refuses_oversized_marker_files() {
        let workspace = make_test_directory("oversized-marker");
        let id = "0123456789abcdef0123456789abcdef";
        let snapshot = workspace.join(id);
        fs::create_dir(&snapshot).unwrap();
        fs::write(snapshot.join(SNAPSHOT_OWNER_MARKER), vec![b'x'; 1025]).unwrap();
        assert!(verified_owned_snapshot_path(&workspace, id).is_err());
        assert!(snapshot.exists(), "oversized ownership evidence must not authorise deletion");
        fs::remove_dir_all(workspace).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn ownership_recovery_refuses_symlinked_snapshot_directories() {
        let workspace = make_test_directory("linked-marker");
        let id = "0123456789abcdef0123456789abcdef";
        let real = workspace.join("real");
        fs::create_dir(&real).unwrap();
        write_snapshot_ownership_marker(&real, id).unwrap();
        std::os::unix::fs::symlink(&real, workspace.join(id)).unwrap();
        assert!(verified_owned_snapshot_path(&workspace, id).is_err());
        assert!(real.exists(), "the target of a redirected path must never be removed");
        fs::remove_dir_all(workspace).unwrap();
    }

    #[test]
    fn snapshot_copy_only_copies_eligible_regular_source_files() {
        let base = make_test_directory("allowlist");
        let source = base.join("project");
        let destination = base.join("snapshot");
        fs::create_dir_all(source.join("src")).unwrap();
        fs::create_dir_all(source.join("node_modules")).unwrap();
        fs::create_dir_all(source.join(".git")).unwrap();
        fs::create_dir(&destination).unwrap();
        fs::write(source.join("src/main.rs"), "fn main() {}").unwrap();
        fs::write(source.join(".env"), "API_KEY=not-for-copy").unwrap();
        fs::write(source.join("node_modules/ignored.rs"), "ignored").unwrap();
        fs::write(source.join(".git/config"), "ignored").unwrap();
        fs::write(source.join("picture.png"), [0u8, 1, 2, 3]).unwrap();
        fs::write(source.join("oversized.rs"), vec![b'x'; (MAX_SNAPSHOT_FILE_BYTES + 1) as usize]).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(source.join("src/main.rs"), source.join("src/linked.rs")).unwrap();

        let canonical_source = fs::canonicalize(&source).unwrap();
        let mut budget = SnapshotBudget { files: 0, bytes: 0, truncated: false };
        copy_snapshot_tree(&canonical_source, &canonical_source, &destination, 0, &mut budget).unwrap();

        assert_eq!(budget.files, 1);
        assert_eq!(fs::read_to_string(destination.join("src/main.rs")).unwrap(), "fn main() {}");
        assert!(!destination.join(".env").exists());
        assert!(!destination.join("node_modules").exists());
        assert!(!destination.join(".git").exists());
        assert!(!destination.join("picture.png").exists());
        assert!(!destination.join("oversized.rs").exists());
        #[cfg(unix)]
        assert!(!destination.join("src/linked.rs").exists());
        assert_eq!(fs::read_to_string(source.join("src/main.rs")).unwrap(), "fn main() {}");
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn snapshot_copy_enforces_the_file_count_cap() {
        let base = make_test_directory("file-cap");
        let source = base.join("project");
        let destination = base.join("snapshot");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&destination).unwrap();
        for index in 0..=MAX_SNAPSHOT_FILES {
            fs::write(source.join(format!("file-{index:04}.rs")), "x").unwrap();
        }
        let canonical_source = fs::canonicalize(&source).unwrap();
        let mut budget = SnapshotBudget { files: 0, bytes: 0, truncated: false };
        copy_snapshot_tree(&canonical_source, &canonical_source, &destination, 0, &mut budget).unwrap();
        assert_eq!(budget.files, MAX_SNAPSHOT_FILES);
        assert!(budget.truncated);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn sandbox_config_disables_networking_and_shares_snapshot_read_only() {
        let base = make_test_directory("wsb-config");
        let snapshot = base.join("snapshot");
        fs::create_dir(&snapshot).unwrap();
        let canonical = fs::canonicalize(&snapshot).unwrap();
        let config = build_sandbox_config(&canonical).unwrap();
        assert!(config.contains("<Networking>Disable</Networking>"));
        assert!(config.contains("<VGpu>Disable</VGpu>"));
        assert!(config.contains("<ClipboardRedirection>Disable</ClipboardRedirection>"));
        assert!(config.contains("<ReadOnly>true</ReadOnly>"));
        assert!(config.contains("<SandboxFolder>C:\\FRIDAY\\Project</SandboxFolder>"));
        assert!(config.contains(canonical.to_str().unwrap()));
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn sandbox_config_escapes_xml_path_characters() {
        assert_eq!(xml_escape(r#"C:\\Work & <Test> "Folder""#),
            "C:\\\\Work &amp; &lt;Test&gt; &quot;Folder&quot;");
    }

    #[test]
    fn sandbox_config_refuses_missing_or_non_directory_paths() {
        let base = make_test_directory("wsb-invalid");
        let file = base.join("not-a-folder.rs");
        fs::write(&file, "x").unwrap();
        assert!(build_sandbox_config(&file).is_err());
        assert!(build_sandbox_config(&base.join("missing")).is_err());
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn sandbox_preflight_never_advertises_execution_or_network_isolation_early() {
        let policy = CodingSandboxPreflight {
            platform_supported: true,
            runtime_detected: true,
            execution_enabled: false,
            network_isolation_enforced: false,
            windows_edition: "Professional".into(),
            windows_build: "26100".into(),
            sandbox_feature_state: "Enabled".into(),
            wsb_cli_available: true,
            wsb_cli_version: "1.0.0".into(),
            virtualization_firmware_enabled: Some(true),
            sandbox_process_detected: false,
            virtual_disk_api_available: true,
            manage_volume_privilege_enabled: true,
            hyperv_cmdlets_available: true,
            mapped_folders_policy: "Not configured".into(),
            mapped_folder_write_policy: "Not configured".into(),
            result_storage_ready: false,
            message: "Preflight only".into(),
        };
        assert!(!policy.execution_enabled);
        assert!(!policy.network_isolation_enforced);
    }

    #[test]
    fn sandbox_policy_probe_distinguishes_blocked_allowed_and_unconfigured() {
        assert_eq!(sandbox_policy_state(None), "Not configured");
        assert_eq!(sandbox_policy_state(Some(&serde_json::Value::Null)), "Not configured");
        assert_eq!(sandbox_policy_state(Some(&serde_json::json!(0))), "Blocked");
        assert_eq!(sandbox_policy_state(Some(&serde_json::json!(1))), "Allowed");
        assert_eq!(sandbox_policy_state(Some(&serde_json::json!(2))), "Unknown");
        assert_eq!(sandbox_policy_state(Some(&serde_json::json!("1"))), "Unknown");
    }

    #[test]
    fn volume_management_privilege_probe_requires_exact_enabled_privilege() {
        let enabled = "Privilege Name Description State\nSeManageVolumePrivilege Perform volume maintenance tasks Enabled\n";
        let disabled = "SeManageVolumePrivilege Perform volume maintenance tasks Disabled\n";
        assert!(privilege_output_has_enabled(enabled, "SeManageVolumePrivilege"));
        assert!(!privilege_output_has_enabled(disabled, "SeManageVolumePrivilege"));
        assert!(!privilege_output_has_enabled(enabled, "SeBackupPrivilege"));
        assert!(!privilege_output_has_enabled("", "SeManageVolumePrivilege"));
    }

    #[test]
    fn storage_preflight_never_marks_unverified_storage_ready() {
        let policy = CodingSandboxPreflight {
            platform_supported: true, runtime_detected: true, execution_enabled: false,
            network_isolation_enforced: false, windows_edition: "Professional".into(),
            windows_build: "26100".into(), sandbox_feature_state: "Enabled".into(),
            wsb_cli_available: true, wsb_cli_version: "1.0.0".into(),
            virtualization_firmware_enabled: Some(true), sandbox_process_detected: false,
            virtual_disk_api_available: true, manage_volume_privilege_enabled: true,
            hyperv_cmdlets_available: true,
            mapped_folders_policy: "Not configured".into(),
            mapped_folder_write_policy: "Not configured".into(),
            result_storage_ready: false,
            message: "Detection only.".into(),
        };
        assert!(!policy.result_storage_ready);
        assert!(!policy.execution_enabled);
    }

    #[test]
    fn windows_sandbox_detection_never_treats_a_missing_root_as_available() {
        assert!(!detect_windows_sandbox_at(None));
    }

    #[test]
    fn snapshot_allowlist_excludes_secrets_and_binary_assets() {
        assert!(safe_project_file_name("main.rs"));
        assert!(safe_project_file_name("package.json"));
        assert!(!safe_project_file_name(".env"));
        assert!(!safe_project_file_name("client-secret.json"));
        assert!(!safe_project_file_name("id_rsa"));
        assert!(!safe_project_file_name("image.png"));
        assert!(!safe_project_file_name("application.exe"));
    }

    #[test]
    fn snapshot_skips_hidden_and_dependency_directories() {
        for name in [".git", ".vscode", "node_modules", "target", "dist", "vendor"] {
            assert!(excluded_directory(name));
        }
        assert!(!excluded_directory("src"));
    }

    #[test]
    fn snapshot_paths_must_be_relative_normal_components() {
        assert!(safe_relative_path(Path::new("src/main.rs")));
        assert!(!safe_relative_path(Path::new("../outside.rs")));
        assert!(!safe_relative_path(Path::new("/absolute/path")));
    }
}

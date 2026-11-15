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

fn read_verified_guest_result_file(
    result_directory: &Path,
    expected_run_id: &str,
    expected_operation: GuestRunOperation,
    max_output_bytes: u64,
) -> Result<GuestRunResult, String> {
    let bytes = crate::coding_sandbox_fs::read_single_bounded_file_from_directory(
        result_directory,
        OsStr::new(GUEST_RESULT_FILE_NAME),
        MAX_GUEST_RESULT_BYTES as u64,
    ).map_err(|_| "The guest result directory or file could not be safely verified and read.".to_string())?;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum GuestResultStorageState {
    Planned, Created, Attached, GptWriteInProgress, Partitioned, Mounted, Shared, GuestStopped,
    ShareRevoked, Detached, Removed, RecoveryRequired,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum GuestResultStorageEvent {
    CreateVerified, AttachVerified, BeginGptInitialization, CompleteGptInitialization,
    MountVerified, GrantShare, ConfirmGuestStopped, RevokeShare, DetachVerified,
    DeleteVerified, PreserveForRecovery,
}

/// Pure lifecycle gate for a future fixed-capacity result volume.
/// This records allowed ordering only; it performs no disk or Sandbox I/O.
fn advance_guest_result_storage_state(
    state: GuestResultStorageState,
    event: GuestResultStorageEvent,
    ownership_verified: bool,
    guest_stopped: bool,
) -> Result<GuestResultStorageState, String> {
    if event == GuestResultStorageEvent::PreserveForRecovery {
        return if state == GuestResultStorageState::Removed {
            Err("Removed result storage cannot be moved back into recovery.".into())
        } else {
            Ok(GuestResultStorageState::RecoveryRequired)
        };
    }
    if state == GuestResultStorageState::RecoveryRequired {
        return Err("Result storage is preserved for recovery; automatic lifecycle actions are blocked.".into());
    }
    if !ownership_verified {
        return Err("Result-storage ownership could not be verified; the operation was refused.".into());
    }

    match (state, event) {
        (GuestResultStorageState::Planned, GuestResultStorageEvent::CreateVerified) => Ok(GuestResultStorageState::Created),
        (GuestResultStorageState::Created, GuestResultStorageEvent::AttachVerified) => Ok(GuestResultStorageState::Attached),
        (GuestResultStorageState::Attached, GuestResultStorageEvent::BeginGptInitialization) => Ok(GuestResultStorageState::GptWriteInProgress),
        (GuestResultStorageState::GptWriteInProgress, GuestResultStorageEvent::CompleteGptInitialization) => Ok(GuestResultStorageState::Partitioned),
        (GuestResultStorageState::Partitioned, GuestResultStorageEvent::MountVerified) => Ok(GuestResultStorageState::Mounted),
        (GuestResultStorageState::Mounted, GuestResultStorageEvent::GrantShare) => Ok(GuestResultStorageState::Shared),
        (GuestResultStorageState::Shared, GuestResultStorageEvent::ConfirmGuestStopped) if guest_stopped => Ok(GuestResultStorageState::GuestStopped),
        (GuestResultStorageState::GuestStopped, GuestResultStorageEvent::RevokeShare) => Ok(GuestResultStorageState::ShareRevoked),
        (GuestResultStorageState::ShareRevoked, GuestResultStorageEvent::DetachVerified) => Ok(GuestResultStorageState::Detached),
        (GuestResultStorageState::Detached, GuestResultStorageEvent::DeleteVerified) => Ok(GuestResultStorageState::Removed),
        _ => Err("The result-storage lifecycle transition is out of order or lacks required evidence.".into()),
    }
}


const GUEST_RESULT_STORAGE_JOURNAL_SCHEMA: u32 = 1;
const MAX_GUEST_RESULT_STORAGE_JOURNAL_BYTES: usize = 16 * 1024;
const MAX_GUEST_RESULT_STORAGE_JOURNAL_ENTRIES: usize = 16;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GuestResultStorageJournalEntry {
    sequence: u32,
    previous_state: GuestResultStorageState,
    event: GuestResultStorageEvent,
    next_state: GuestResultStorageState,
    ownership_verified: bool,
    guest_stopped: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GuestResultStorageJournal {
    schema_version: u32,
    run_id: String,
    vhdx_identifier: String,
    entries: Vec<GuestResultStorageJournalEntry>,
}

fn validate_guest_result_storage_journal(
    journal: &GuestResultStorageJournal,
) -> Result<GuestResultStorageState, String> {
    if journal.schema_version != GUEST_RESULT_STORAGE_JOURNAL_SCHEMA
        || !valid_snapshot_id(&journal.run_id)
        || !valid_guest_vhdx_identifier(&journal.vhdx_identifier)
        || journal.entries.len() > MAX_GUEST_RESULT_STORAGE_JOURNAL_ENTRIES
    {
        return Err("The result-storage recovery journal header or bounds are invalid.".into());
    }
    let mut state = GuestResultStorageState::Planned;
    for (index, entry) in journal.entries.iter().enumerate() {
        if entry.sequence != index as u32 + 1 || entry.previous_state != state {
            return Err("The result-storage recovery journal is out of sequence or has a broken state chain.".into());
        }
        state = advance_guest_result_storage_state(
            state, entry.event, entry.ownership_verified, entry.guest_stopped,
        )?;
        if entry.next_state != state {
            return Err("The result-storage recovery journal contains an invalid transition result.".into());
        }
    }
    Ok(state)
}

fn encode_guest_result_storage_journal(
    journal: &GuestResultStorageJournal,
) -> Result<Vec<u8>, String> {
    validate_guest_result_storage_journal(journal)?;
    let bytes = serde_json::to_vec(journal)
        .map_err(|_| "The result-storage recovery journal could not be encoded.".to_string())?;
    if bytes.is_empty() || bytes.len() > MAX_GUEST_RESULT_STORAGE_JOURNAL_BYTES {
        return Err("The result-storage recovery journal exceeds its fixed size limit.".into());
    }
    Ok(bytes)
}

fn parse_guest_result_storage_journal(bytes: &[u8]) -> Result<GuestResultStorageJournal, String> {
    if bytes.is_empty() || bytes.len() > MAX_GUEST_RESULT_STORAGE_JOURNAL_BYTES {
        return Err("The result-storage recovery journal is empty or oversized.".into());
    }
    let journal: GuestResultStorageJournal = serde_json::from_slice(bytes)
        .map_err(|_| "The result-storage recovery journal is malformed or contains unsupported fields.".to_string())?;
    if journal.entries.is_empty() {
        return Err("The result-storage recovery journal contains no recorded transitions.".into());
    }
    validate_guest_result_storage_journal(&journal)?;
    Ok(journal)
}

fn append_guest_result_storage_journal_entry(
    journal: &GuestResultStorageJournal,
    event: GuestResultStorageEvent,
    ownership_verified: bool,
    guest_stopped: bool,
) -> Result<GuestResultStorageJournal, String> {
    let state = validate_guest_result_storage_journal(journal)?;
    if journal.entries.len() >= MAX_GUEST_RESULT_STORAGE_JOURNAL_ENTRIES {
        return Err("The result-storage recovery journal reached its fixed transition limit.".into());
    }
    let next_state = advance_guest_result_storage_state(
        state, event, ownership_verified, guest_stopped,
    )?;
    let mut next = journal.clone();
    next.entries.push(GuestResultStorageJournalEntry {
        sequence: next.entries.len() as u32 + 1,
        previous_state: state,
        event,
        next_state,
        ownership_verified,
        guest_stopped,
    });
    encode_guest_result_storage_journal(&next)?;
    Ok(next)
}

const GUEST_RESULT_STORAGE_JOURNAL_DIR_PREFIX: &str = ".friday-result-journal-";
const GUEST_RESULT_STORAGE_JOURNAL_FILE: &str = "state.json";

fn guest_result_storage_journal_generation_path(root: &Path, generation: usize) -> PathBuf {
    root.join(format!("{GUEST_RESULT_STORAGE_JOURNAL_DIR_PREFIX}{generation:02}"))
}

fn read_guest_result_storage_journal_generation(
    root: &Path,
    generation: usize,
) -> Result<GuestResultStorageJournal, String> {
    if generation == 0 || generation > MAX_GUEST_RESULT_STORAGE_JOURNAL_ENTRIES {
        return Err("The result-storage journal generation is outside its fixed limit.".into());
    }
    let directory = guest_result_storage_journal_generation_path(root, generation);
    let metadata = fs::symlink_metadata(&directory)
        .map_err(|_| "A result-storage journal generation is missing or unavailable.".to_string())?;
    if !metadata.is_dir() || is_reparse_or_symlink(&metadata) {
        return Err("A result-storage journal generation is not an ordinary directory.".into());
    }
    let bytes = crate::coding_sandbox_fs::read_single_bounded_file_from_directory(
        &directory, OsStr::new(GUEST_RESULT_STORAGE_JOURNAL_FILE),
        MAX_GUEST_RESULT_STORAGE_JOURNAL_BYTES as u64,
    ).map_err(|_| "A result-storage journal generation is incomplete or unsafe.".to_string())?;
    let journal = parse_guest_result_storage_journal(&bytes)?;
    if journal.entries.len() != generation {
        return Err("A result-storage journal generation does not match its sequence.".into());
    }
    Ok(journal)
}

/// Persist an immutable, create-new journal generation. Each generation is a
/// separate private directory containing exactly one bounded, flushed file.
/// Existing generations are never overwritten; an interrupted generation is
/// retained and forces manual recovery instead of being silently repaired.
fn persist_guest_result_storage_journal(
    root: &Path,
    journal: &GuestResultStorageJournal,
) -> Result<(), String> {
    let state = validate_guest_result_storage_journal(journal)?;
    let generation = journal.entries.len();
    if generation == 0 || generation > MAX_GUEST_RESULT_STORAGE_JOURNAL_ENTRIES {
        return Err("Only a non-empty bounded result-storage journal can be persisted.".into());
    }
    let root_metadata = fs::symlink_metadata(root)
        .map_err(|_| "The result-storage journal root is unavailable.".to_string())?;
    if !root_metadata.is_dir() || is_reparse_or_symlink(&root_metadata) {
        return Err("The result-storage journal root must be an ordinary directory.".into());
    }
    if generation > 1 {
        let previous = read_guest_result_storage_journal_generation(root, generation - 1)?;
        if previous.run_id != journal.run_id
            || previous.vhdx_identifier != journal.vhdx_identifier
            || previous.entries != journal.entries[..generation - 1]
        {
            return Err("The next result-storage journal generation does not extend the persisted history.".into());
        }
    } else {
        let first = guest_result_storage_journal_generation_path(root, 1);
        if fs::symlink_metadata(first).is_ok() {
            return Err("The initial result-storage journal generation already exists.".into());
        }
    }
    let directory = guest_result_storage_journal_generation_path(root, generation);
    fs::create_dir(&directory)
        .map_err(|_| "FRIDAY could not exclusively create the next result-storage journal generation.".to_string())?;
    secure_private_workspace_directory(&directory)
        .map_err(|_| "FRIDAY could not secure the new journal generation; it was preserved for recovery.".to_string())?;
    let bytes = encode_guest_result_storage_journal(journal)?;
    let file_path = directory.join(GUEST_RESULT_STORAGE_JOURNAL_FILE);
    let mut file = OpenOptions::new().write(true).create_new(true).open(&file_path)
        .map_err(|_| "FRIDAY could not exclusively create the journal generation file; it was preserved.".to_string())?;
    use std::io::Write;
    file.write_all(&bytes)
        .map_err(|_| "FRIDAY could not write the journal generation; it was preserved.".to_string())?;
    file.sync_all()
        .map_err(|_| "FRIDAY could not flush the journal generation; it was preserved.".to_string())?;
    drop(file);
    let verified = read_guest_result_storage_journal_generation(root, generation)?;
    if verified != *journal || validate_guest_result_storage_journal(&verified)? != state {
        return Err("The persisted result-storage journal did not pass read-back verification.".into());
    }
    Ok(())
}

/// Load the longest contiguous sequence of immutable journal generations.
/// Gaps, malformed files, unexpected generation directories, and identity
/// changes fail closed; callers must preserve the VHDX for manual recovery.
fn load_guest_result_storage_journal(
    root: &Path,
    expected_run_id: &str,
    expected_vhdx_identifier: &str,
) -> Result<GuestResultStorageJournal, String> {
    if !valid_snapshot_id(expected_run_id) || !valid_guest_vhdx_identifier(expected_vhdx_identifier) {
        return Err("The expected result-storage journal identity is invalid.".into());
    }
    let root_metadata = fs::symlink_metadata(root)
        .map_err(|_| "The result-storage journal root is unavailable.".to_string())?;
    if !root_metadata.is_dir() || is_reparse_or_symlink(&root_metadata) {
        return Err("The result-storage journal root must be an ordinary directory.".into());
    }
    let mut latest: Option<GuestResultStorageJournal> = None;
    let mut gap_seen = false;
    for generation in 1..=MAX_GUEST_RESULT_STORAGE_JOURNAL_ENTRIES {
        let path = guest_result_storage_journal_generation_path(root, generation);
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                gap_seen = true;
            }
            Err(_) => return Err("A result-storage journal generation could not be inspected.".into()),
            Ok(_) if gap_seen => return Err("The result-storage journal contains a generation gap.".into()),
            Ok(_) => {
                let current = read_guest_result_storage_journal_generation(root, generation)?;
                if current.run_id != expected_run_id || current.vhdx_identifier != expected_vhdx_identifier {
                    return Err("The result-storage journal belongs to a different run or VHDX.".into());
                }
                if let Some(previous) = &latest {
                    if current.entries[..generation - 1] != previous.entries {
                        return Err("The result-storage journal history is not append-only.".into());
                    }
                }
                latest = Some(current);
            }
        }
    }
    latest.ok_or_else(|| "No complete result-storage journal generation is available.".into())
}




/// Append one verified lifecycle transition through the durable journal boundary.
/// A non-empty caller snapshot must exactly match the latest persisted generation;
/// competing writers are serialized by exclusive generation-directory creation.
fn append_and_persist_guest_result_storage_transition(
    root: &Path,
    current: &GuestResultStorageJournal,
    event: GuestResultStorageEvent,
    ownership_verified: bool,
    guest_stopped: bool,
) -> Result<GuestResultStorageJournal, String> {
    validate_guest_result_storage_journal(current)?;
    if current.entries.is_empty() {
        for generation in 1..=MAX_GUEST_RESULT_STORAGE_JOURNAL_ENTRIES {
            let path = guest_result_storage_journal_generation_path(root, generation);
            match fs::symlink_metadata(path) {
                Ok(_) => return Err("An initial result-storage journal cannot be created over existing recovery evidence.".into()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err("Existing result-storage journal evidence could not be inspected.".into()),
            }
        }
    } else {
        let latest = load_guest_result_storage_journal(
            root, &current.run_id, &current.vhdx_identifier,
        )?;
        if latest != *current {
            return Err("The supplied result-storage journal is stale or differs from persisted recovery history.".into());
        }
    }

    let next = append_guest_result_storage_journal_entry(
        current, event, ownership_verified, guest_stopped,
    )?;
    persist_guest_result_storage_journal(root, &next)?;
    let verified = load_guest_result_storage_journal(
        root, &next.run_id, &next.vhdx_identifier,
    )?;
    if verified != next {
        return Err("The newly appended result-storage transition failed full recovery verification.".into());
    }
    Ok(next)
}

const GUEST_RESULT_VOLUME_CAPACITY_BYTES: u64 = 64 * 1024 * 1024;
const GUEST_RESULT_VOLUME_GUEST_MOUNT: &str = r"C:\FRIDAY\Results";

#[derive(Clone, Debug, PartialEq, Eq)]
struct GuestResultVolumePlan {
    run_id: String,
    storage_root: PathBuf,
    run_directory: PathBuf,
    vhdx_path: PathBuf,
    guest_mount_path: String,
    capacity_bytes: u64,
    max_result_bytes: u64,
}

/// Builds an immutable, policy-owned plan for one bounded result volume.
/// This does not create, attach, format, mount, or share a VHDX.
fn build_guest_result_volume_plan(
    storage_root: &Path,
    run_id: &str,
) -> Result<GuestResultVolumePlan, String> {
    if !valid_snapshot_id(run_id) {
        return Err("The result-volume run identifier is invalid.".into());
    }
    if !storage_root.is_absolute() {
        return Err("The result-volume storage root must be absolute.".into());
    }
    let root_metadata = fs::symlink_metadata(storage_root)
        .map_err(|_| "The private result-volume storage root is unavailable.".to_string())?;
    if !root_metadata.is_dir() || is_reparse_or_symlink(&root_metadata) {
        return Err("The result-volume storage root must be an ordinary directory.".into());
    }
    let canonical_root = fs::canonicalize(storage_root)
        .map_err(|_| "The private result-volume storage root could not be verified.".to_string())?;
    let canonical_metadata = fs::symlink_metadata(&canonical_root)
        .map_err(|_| "The canonical result-volume storage root could not be verified.".to_string())?;
    if !canonical_metadata.is_dir() || is_reparse_or_symlink(&canonical_metadata) {
        return Err("The canonical result-volume storage root is not an ordinary directory.".into());
    }

    let run_directory = canonical_root.join(run_id);
    match fs::symlink_metadata(&run_directory) {
        Ok(_) => return Err("A result-volume run directory already exists; reuse is refused.".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err("The result-volume run directory could not be safely inspected.".into()),
    }
    let vhdx_path = run_directory.join("result.vhdx");
    if !vhdx_path.starts_with(&canonical_root)
        || run_directory.file_name().and_then(OsStr::to_str) != Some(run_id)
    {
        return Err("The result-volume path failed its containment check.".into());
    }

    Ok(GuestResultVolumePlan {
        run_id: run_id.to_string(),
        storage_root: canonical_root,
        run_directory,
        vhdx_path,
        guest_mount_path: GUEST_RESULT_VOLUME_GUEST_MOUNT.to_string(),
        capacity_bytes: GUEST_RESULT_VOLUME_CAPACITY_BYTES,
        max_result_bytes: MAX_GUEST_RESULT_BYTES as u64,
    })
}

/// Re-derives the plan from trusted host inputs and compares every field.
/// A guest request can never choose a path, mount point, capacity, or result cap.
fn validate_guest_result_volume_plan(
    plan: &GuestResultVolumePlan,
    trusted_storage_root: &Path,
    expected_run_id: &str,
) -> Result<(), String> {
    if !valid_snapshot_id(expected_run_id) {
        return Err("The result-volume run identifier is invalid.".into());
    }
    if !trusted_storage_root.is_absolute() {
        return Err("The result-volume storage root must be absolute.".into());
    }
    let root_metadata = fs::symlink_metadata(trusted_storage_root)
        .map_err(|_| "The private result-volume storage root is unavailable.".to_string())?;
    if !root_metadata.is_dir() || is_reparse_or_symlink(&root_metadata) {
        return Err("The result-volume storage root must be an ordinary directory.".into());
    }
    let canonical_root = fs::canonicalize(trusted_storage_root)
        .map_err(|_| "The private result-volume storage root could not be verified.".to_string())?;
    let canonical_metadata = fs::symlink_metadata(&canonical_root)
        .map_err(|_| "The canonical result-volume storage root could not be verified.".to_string())?;
    if !canonical_metadata.is_dir() || is_reparse_or_symlink(&canonical_metadata) {
        return Err("The canonical result-volume storage root is not an ordinary directory.".into());
    }

    // Validation intentionally does not require the run directory to be absent:
    // reservation creates it before ownership evidence can be verified.
    let expected_run_directory = canonical_root.join(expected_run_id);
    let expected = GuestResultVolumePlan {
        run_id: expected_run_id.to_string(),
        storage_root: canonical_root,
        run_directory: expected_run_directory.clone(),
        vhdx_path: expected_run_directory.join("result.vhdx"),
        guest_mount_path: GUEST_RESULT_VOLUME_GUEST_MOUNT.to_string(),
        capacity_bytes: GUEST_RESULT_VOLUME_CAPACITY_BYTES,
        max_result_bytes: MAX_GUEST_RESULT_BYTES as u64,
    };
    if plan != &expected {
        return Err("The result-volume plan differs from FRIDAY's fixed per-run policy.".into());
    }
    if plan.capacity_bytes < plan.max_result_bytes
        || plan.max_result_bytes != MAX_GUEST_RESULT_BYTES as u64
        || plan.guest_mount_path != GUEST_RESULT_VOLUME_GUEST_MOUNT
    {
        return Err("The result-volume capacity or guest mount point violates FRIDAY's fixed policy.".into());
    }
    Ok(())
}


const GUEST_RESULT_VOLUME_OWNER_MARKER: &str = ".friday-result-volume-owner.json";
const GUEST_RESULT_VOLUME_OWNER_SCHEMA: u32 = 1;

const GUEST_RESULT_VHDX_OWNER_MARKER: &str = ".friday-result-vhdx-owner.json";
const GUEST_RESULT_VHDX_OWNER_SCHEMA: u32 = 1;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GuestResultVhdxOwnershipMarker {
    schema_version: u32,
    run_id: String,
    owner: String,
    identifier: String,
    capacity_bytes: u64,
}

fn validate_guest_result_vhdx_ownership_marker(
    marker: &GuestResultVhdxOwnershipMarker,
    expected_run_id: &str,
    expected_identifier: Option<&str>,
) -> Result<(), String> {
    if !valid_snapshot_id(expected_run_id)
        || marker.schema_version != GUEST_RESULT_VHDX_OWNER_SCHEMA
        || marker.run_id != expected_run_id
        || marker.owner != "FRIDAY-CodingAssistant-ResultVhdx"
        || marker.capacity_bytes != GUEST_RESULT_VOLUME_CAPACITY_BYTES
        || !valid_guest_vhdx_identifier(&marker.identifier)
        || expected_identifier.is_some_and(|expected| expected != marker.identifier)
    {
        return Err("The result VHDX ownership record does not match FRIDAY's exact run and disk identity.".into());
    }
    Ok(())
}

/// Persists the VHDX's stable on-disk identifier in a separate exclusive marker.
/// This marker is evidence for later verification, not authorization to attach,
/// mount, detach, or delete the virtual disk.
fn write_guest_result_vhdx_ownership_marker(
    plan: &GuestResultVolumePlan,
    trusted_storage_root: &Path,
    identifier: &str,
) -> Result<(), String> {
    verify_guest_result_volume_directory(plan, trusted_storage_root)?;
    if !valid_guest_vhdx_identifier(identifier) {
        return Err("The result VHDX identifier is invalid; ownership was not recorded.".into());
    }
    let marker = GuestResultVhdxOwnershipMarker {
        schema_version: GUEST_RESULT_VHDX_OWNER_SCHEMA,
        run_id: plan.run_id.clone(),
        owner: "FRIDAY-CodingAssistant-ResultVhdx".to_string(),
        identifier: identifier.to_string(),
        capacity_bytes: GUEST_RESULT_VOLUME_CAPACITY_BYTES,
    };
    let bytes = serde_json::to_vec(&marker)
        .map_err(|_| "FRIDAY could not prepare VHDX ownership metadata.".to_string())?;
    let marker_path = plan.run_directory.join(GUEST_RESULT_VHDX_OWNER_MARKER);
    let mut file = OpenOptions::new().write(true).create_new(true).open(&marker_path)
        .map_err(|_| "FRIDAY could not exclusively create VHDX ownership metadata.".to_string())?;
    use std::io::Write;
    file.write_all(&bytes)
        .map_err(|_| "FRIDAY could not write VHDX ownership metadata.".to_string())?;
    file.sync_all()
        .map_err(|_| "FRIDAY could not persist VHDX ownership metadata.".to_string())?;
    verify_guest_result_vhdx_ownership_marker(plan, trusted_storage_root, Some(identifier))?;
    Ok(())
}

fn verify_guest_result_vhdx_ownership_marker(
    plan: &GuestResultVolumePlan,
    trusted_storage_root: &Path,
    expected_identifier: Option<&str>,
) -> Result<String, String> {
    verify_guest_result_volume_directory(plan, trusted_storage_root)?;
    let marker_path = plan.run_directory.join(GUEST_RESULT_VHDX_OWNER_MARKER);
    let metadata = fs::symlink_metadata(&marker_path)
        .map_err(|_| "VHDX ownership evidence is missing; the resource was preserved.".to_string())?;
    if !metadata.is_file() || is_reparse_or_symlink(&metadata) || metadata.len() == 0 || metadata.len() > 1024 {
        return Err("VHDX ownership evidence is not a bounded ordinary file.".into());
    }
    let mut file = open_source_file(&marker_path)
        .map_err(|_| "VHDX ownership evidence could not be opened without following links.".to_string())?;
    let opened = file.metadata()
        .map_err(|_| "VHDX ownership evidence could not be verified.".to_string())?;
    if !opened.is_file() || is_reparse_or_symlink(&opened) || opened.len() != metadata.len() || opened.len() > 1024 {
        return Err("VHDX ownership evidence changed while being opened.".into());
    }
    let mut bytes = Vec::new();
    file.take(1025).read_to_end(&mut bytes)
        .map_err(|_| "VHDX ownership evidence could not be read.".to_string())?;
    if bytes.is_empty() || bytes.len() > 1024 {
        return Err("VHDX ownership evidence exceeded its read limit.".into());
    }
    let marker: GuestResultVhdxOwnershipMarker = serde_json::from_slice(&bytes)
        .map_err(|_| "VHDX ownership evidence is malformed or contains unsupported fields.".to_string())?;
    validate_guest_result_vhdx_ownership_marker(&marker, &plan.run_id, expected_identifier)?;
    Ok(marker.identifier)
}




/// Verifies that the result VHDX is the exact ordinary file directly inside
/// FRIDAY's verified per-run directory. This is a read-only path/handle check;
/// it does not authorize attachment, mounting, sharing, or deletion.
fn verify_guest_result_vhdx_file_path(
    plan: &GuestResultVolumePlan,
    trusted_storage_root: &Path,
) -> Result<PathBuf, String> {
    let verified_directory = verify_guest_result_volume_directory(plan, trusted_storage_root)?;
    if plan.vhdx_path.parent() != Some(plan.run_directory.as_path())
        || plan.vhdx_path.file_name() != Some(OsStr::new("result.vhdx"))
    {
        return Err("The result VHDX path is not FRIDAY's exact per-run disk path.".into());
    }
    let metadata = fs::symlink_metadata(&plan.vhdx_path)
        .map_err(|_| "The result VHDX is unavailable; it was preserved for recovery.".to_string())?;
    if !metadata.is_file() || is_reparse_or_symlink(&metadata) || metadata.len() < 8 {
        return Err("The result VHDX is not an ordinary non-empty file.".into());
    }
    let canonical = fs::canonicalize(&plan.vhdx_path)
        .map_err(|_| "The result VHDX path could not be verified.".to_string())?;
    if canonical.parent() != Some(verified_directory.as_path())
        || canonical.file_name() != Some(OsStr::new("result.vhdx"))
    {
        return Err("The result VHDX is not the exact direct child of its owned run directory.".into());
    }
    let mut file = open_source_file(&plan.vhdx_path)
        .map_err(|_| "The result VHDX could not be opened without following links.".to_string())?;
    let opened = file.metadata()
        .map_err(|_| "The opened result VHDX handle could not be verified.".to_string())?;
    if !opened.is_file() || is_reparse_or_symlink(&opened) || opened.len() != metadata.len() {
        return Err("The result VHDX changed while its file handle was opened.".into());
    }
    let mut signature = [0u8; 8];
    file.read_exact(&mut signature)
        .map_err(|_| "The result VHDX header could not be read.".to_string())?;
    if &signature != b"vhdxfile" {
        return Err("The result file does not contain the VHDX signature.".into());
    }
    Ok(canonical)
}

#[cfg(windows)]
fn verify_guest_result_vhdx_identity(
    plan: &GuestResultVolumePlan,
    trusted_storage_root: &Path,
) -> Result<String, String> {
    let recorded_identifier =
        verify_guest_result_vhdx_ownership_marker(plan, trusted_storage_root, None)?;
    let verified_path = verify_guest_result_vhdx_file_path(plan, trusted_storage_root)?;
    let observed_identifier =
        verify_guest_result_vhdx_capacity(&verified_path, GUEST_RESULT_VOLUME_CAPACITY_BYTES)?;
    // Recheck the directory and file after the path-based Virtual Disk API query.
    // A changed path or replaced file is never accepted as ownership evidence.
    let verified_again = verify_guest_result_vhdx_file_path(plan, trusted_storage_root)?;
    if verified_again != verified_path || observed_identifier != recorded_identifier {
        return Err("The result VHDX identity changed or does not match its FRIDAY ownership record.".into());
    }
    Ok(observed_identifier)
}

#[cfg(windows)]
const FRIDAY_GET_VIRTUAL_DISK_INFO_VIRTUAL_DISK_ID: u32 = 14;

#[cfg(windows)]
#[repr(C)]
#[derive(Clone, Copy)]
struct FridayWindowsGuid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

#[cfg(windows)]
#[repr(C)]
#[derive(Clone, Copy)]
struct FridayVirtualStorageType {
    device_id: u32,
    vendor_id: FridayWindowsGuid,
}

#[cfg(windows)]
#[repr(C)]
#[derive(Clone, Copy)]
struct FridayCreateVirtualDiskVersion2 {
    unique_id: FridayWindowsGuid,
    maximum_size: u64,
    block_size_in_bytes: u32,
    sector_size_in_bytes: u32,
    physical_sector_size_in_bytes: u32,
    parent_path: *const u16,
    source_path: *const u16,
    open_flags: u32,
    parent_virtual_storage_type: FridayVirtualStorageType,
    source_virtual_storage_type: FridayVirtualStorageType,
    resiliency_guid: FridayWindowsGuid,
}

#[cfg(windows)]
#[repr(C)]
union FridayCreateVirtualDiskParametersUnion {
    version2: std::mem::ManuallyDrop<FridayCreateVirtualDiskVersion2>,
    alignment: [u64; 16],
}

#[cfg(windows)]
#[repr(C)]
struct FridayCreateVirtualDiskParameters {
    version: u32,
    parameters: FridayCreateVirtualDiskParametersUnion,
}

#[cfg(windows)]
#[repr(C)]
#[derive(Clone, Copy)]
struct FridayVirtualDiskSizeInfo {
    virtual_size: u64,
    physical_size: u64,
    block_size: u32,
    sector_size: u32,
}

#[cfg(windows)]
#[repr(C)]
union FridayGetVirtualDiskInfoData {
    size: FridayVirtualDiskSizeInfo,
    identifier: FridayWindowsGuid,
    provider_subtype: u32,
    alignment: [u64; 3],
}

#[cfg(windows)]
#[repr(C)]
struct FridayGetVirtualDiskInfo {
    version: u32,
    data: FridayGetVirtualDiskInfoData,
}

#[cfg(windows)]
#[repr(C)]
#[derive(Clone, Copy)]
struct FridayOpenVirtualDiskVersion1 {
    read_write_depth: u32,
}

#[cfg(windows)]
#[repr(C)]
union FridayOpenVirtualDiskParametersData {
    version1: FridayOpenVirtualDiskVersion1,
    alignment: [u64; 3],
}

#[cfg(windows)]
#[repr(C)]
#[derive(Clone, Copy)]
struct FridayAttachVirtualDiskVersion1 {
    reserved: u32,
}

#[cfg(windows)]
#[repr(C)]
struct FridayAttachVirtualDiskParameters {
    version: u32,
    version1: FridayAttachVirtualDiskVersion1,
}

#[cfg(windows)]
#[repr(C)]
struct FridayOpenVirtualDiskParameters {
    version: u32,
    parameters: FridayOpenVirtualDiskParametersData,
}

#[cfg(windows)]
#[link(name = "virtdisk")]
extern "system" {
    fn OpenVirtualDisk(
        virtual_storage_type: *const FridayVirtualStorageType,
        path: *const u16,
        virtual_disk_access_mask: u32,
        flags: u32,
        parameters: *const FridayOpenVirtualDiskParameters,
        handle: *mut *mut std::ffi::c_void,
    ) -> u32;
    fn AttachVirtualDisk(
        virtual_disk_handle: *mut std::ffi::c_void,
        security_descriptor: *const std::ffi::c_void,
        flags: u32,
        provider_specific_flags: u32,
        parameters: *const FridayAttachVirtualDiskParameters,
        overlapped: *mut std::ffi::c_void,
    ) -> u32;
    fn DetachVirtualDisk(
        virtual_disk_handle: *mut std::ffi::c_void,
        flags: u32,
        provider_specific_flags: u32,
    ) -> u32;
    fn GetVirtualDiskPhysicalPath(
        virtual_disk_handle: *mut std::ffi::c_void,
        disk_path_size_in_bytes: *mut u32,
        disk_path: *mut u16,
    ) -> u32;
    fn GetVirtualDiskInformation(
        virtual_disk_handle: *mut std::ffi::c_void,
        virtual_disk_info_size: *mut u32,
        virtual_disk_info: *mut FridayGetVirtualDiskInfo,
        size_used: *mut u32,
    ) -> u32;
    fn CreateVirtualDisk(
        virtual_storage_type: *const FridayVirtualStorageType,
        path: *const u16,
        virtual_disk_access_mask: u32,
        security_descriptor: *const std::ffi::c_void,
        flags: u32,
        provider_specific_flags: u32,
        parameters: *const FridayCreateVirtualDiskParameters,
        overlapped: *mut std::ffi::c_void,
        handle: *mut *mut std::ffi::c_void,
    ) -> u32;
}

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
}

#[cfg(windows)]
struct OwnedVirtualDiskHandle(*mut std::ffi::c_void);

#[cfg(windows)]
impl OwnedVirtualDiskHandle {
    fn close(mut self) -> Result<(), String> {
        let handle = std::mem::replace(&mut self.0, std::ptr::null_mut());
        if handle.is_null() || unsafe { CloseHandle(handle) } == 0 {
            return Err("Windows could not close the newly created virtual-disk handle; the resource was preserved for recovery.".into());
        }
        Ok(())
    }
}

#[cfg(windows)]
impl Drop for OwnedVirtualDiskHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CloseHandle(self.0); }
            self.0 = std::ptr::null_mut();
        }
    }
}


/// Owns a single FRIDAY-created VHDX attachment. The handle is deliberately
/// non-permanent: process termination closes it and Windows releases the attach.
#[cfg(windows)]
struct OwnedAttachedGuestVhdx {
    handle: OwnedVirtualDiskHandle,
    physical_path: String,
}

#[cfg(windows)]
impl OwnedAttachedGuestVhdx {
    fn physical_path(&self) -> &str {
        &self.physical_path
    }

    fn detach(self) -> Result<(), String> {
        let status = unsafe { DetachVirtualDisk(self.handle.0, 0, 0) };
        if status != 0 {
            return Err(format!("Windows could not detach FRIDAY's owned result VHDX (error {status}); the attachment handle will be closed for automatic cleanup."));
        }
        if guest_vhdx_physical_path(self.handle.0).is_ok() {
            return Err("Windows reported detachment but the result VHDX still has an attached physical-device path.".into());
        }
        self.handle.close()
    }
}

#[cfg(windows)]
fn guest_vhdx_physical_path(handle: *mut std::ffi::c_void) -> Result<String, String> {
    let mut buffer = [0u16; 1024];
    let mut size_bytes = (buffer.len() * std::mem::size_of::<u16>()) as u32;
    let status = unsafe { GetVirtualDiskPhysicalPath(handle, &mut size_bytes, buffer.as_mut_ptr()) };
    if status != 0 {
        return Err(format!("Windows could not confirm the attached result VHDX device path (error {status})."));
    }
    if size_bytes < 2 || size_bytes as usize > buffer.len() * 2 || size_bytes % 2 != 0 {
        return Err("Windows returned an invalid result VHDX device-path length.".into());
    }
    let end = buffer.iter().position(|unit| *unit == 0)
        .ok_or_else(|| "Windows returned an unterminated result VHDX device path.".to_string())?;
    let path = String::from_utf16(&buffer[..end])
        .map_err(|_| "Windows returned a malformed result VHDX device path.".to_string())?;
    let prefix = r"\\.\PhysicalDrive";
    if !path.starts_with(prefix)
        || path[prefix.len()..].is_empty()
        || !path[prefix.len()..].bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err("Windows returned a physical-device path outside the expected disk namespace.".into());
    }
    Ok(path)
}

/// Attaches only the exact FRIDAY-owned fixed VHDX, read/write, without drive
/// letters or permanent lifetime. No partition is created or mounted here.
#[cfg(windows)]
fn attach_guest_result_vhdx(
    plan: &GuestResultVolumePlan,
    trusted_storage_root: &Path,
) -> Result<OwnedAttachedGuestVhdx, String> {
    let expected_identifier = verify_guest_result_vhdx_identity(plan, trusted_storage_root)?;
    use std::os::windows::ffi::OsStrExt;
    let path_wide: Vec<u16> = plan.vhdx_path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    if path_wide.len() <= 1 {
        return Err("The result VHDX path is invalid for attachment.".into());
    }
    let storage_type = FridayVirtualStorageType {
        device_id: 3,
        vendor_id: FridayWindowsGuid {
            data1: 0xec984aec,
            data2: 0xa0f9,
            data3: 0x47e9,
            data4: [0x90, 0x1f, 0x71, 0x41, 0x5a, 0x66, 0x34, 0x5b],
        },
    };
    let open_parameters = FridayOpenVirtualDiskParameters {
        version: 1,
        parameters: FridayOpenVirtualDiskParametersData {
            version1: FridayOpenVirtualDiskVersion1 { read_write_depth: 1 },
        },
    };
    let mut raw_handle = std::ptr::null_mut();
    // ATTACH_RW | DETACH | GET_INFO. The handle is scoped to this owner.
    let open_status = unsafe {
        OpenVirtualDisk(&storage_type, path_wide.as_ptr(), 0x0002_0000 | 0x0004_0000 | 0x0008_0000,
            0, &open_parameters, &mut raw_handle)
    };
    if open_status != 0 || raw_handle.is_null() {
        return Err(format!("Windows could not open the verified result VHDX for controlled attachment (error {open_status})."));
    }
    let handle = OwnedVirtualDiskHandle(raw_handle);
    let parameters = FridayAttachVirtualDiskParameters {
        version: 1,
        version1: FridayAttachVirtualDiskVersion1 { reserved: 0 },
    };
    // NO_DRIVE_LETTER. PERMANENT_LIFETIME is intentionally never set.
    let attach_status = unsafe {
        AttachVirtualDisk(handle.0, std::ptr::null(), 0x2, 0, &parameters, std::ptr::null_mut())
    };
    if attach_status != 0 {
        return Err(format!("Windows refused to attach the private result VHDX (error {attach_status}); no volume was exposed."));
    }
    let physical_path = guest_vhdx_physical_path(handle.0)
        .map_err(|error| format!("{error} The attachment handle will be closed for automatic cleanup."))?;
    let observed_identifier = verify_attached_guest_result_vhdx_identity(
        &handle, &physical_path, plan, trusted_storage_root,
    ).map_err(|error| format!("The attached result VHDX failed identity revalidation ({error}); the attachment handle will be closed."))?;
    if observed_identifier != expected_identifier {
        return Err("The result VHDX identity changed during attachment; the attachment handle will be closed.".into());
    }
    Ok(OwnedAttachedGuestVhdx { handle, physical_path })
}

/// Revalidates an attached disk through the exact FRIDAY-owned handle rather
/// than reopening its backing path while Windows has it attached.
#[cfg(windows)]
fn verify_attached_guest_result_vhdx_identity(
    handle: &OwnedVirtualDiskHandle,
    expected_physical_path: &str,
    plan: &GuestResultVolumePlan,
    trusted_storage_root: &Path,
) -> Result<String, String> {
    let recorded_identifier =
        verify_guest_result_vhdx_ownership_marker(plan, trusted_storage_root, None)?;
    let verified_path = verify_guest_result_vhdx_file_path(plan, trusted_storage_root)?;
    let physical_path = guest_vhdx_physical_path(handle.0)?;
    if physical_path != expected_physical_path {
        return Err("The attached physical-device path changed during identity verification.".into());
    }
    let mut identifier_info = FridayGetVirtualDiskInfo {
        version: FRIDAY_GET_VIRTUAL_DISK_INFO_VIRTUAL_DISK_ID, // VHDX GET_VIRTUAL_DISK_INFO_VIRTUAL_DISK_ID
        data: FridayGetVirtualDiskInfoData { alignment: [0; 3] },
    };
    let mut identifier_bytes = std::mem::size_of::<FridayGetVirtualDiskInfo>() as u32;
    let status = unsafe {
        GetVirtualDiskInformation(handle.0, &mut identifier_bytes, &mut identifier_info, std::ptr::null_mut())
    };
    if status != 0 {
        return Err(format!("Windows could not read the persistent identifier from FRIDAY's attached VHDX handle (error {status})."));
    }
    let observed_identifier = format_guest_vhdx_identifier(unsafe { identifier_info.data.identifier })?;
    let verified_again = verify_guest_result_vhdx_file_path(plan, trusted_storage_root)?;
    if verified_again != verified_path {
        return Err("The result VHDX backing-file path changed during attached identity verification.".into());
    }
    if observed_identifier != recorded_identifier {
        return Err(format!(
            "The attached VHDX persistent identifier does not match its FRIDAY ownership record (recorded={}, observed={}).",
            recorded_identifier, observed_identifier
        ));
    }
    Ok(observed_identifier)
}

#[cfg(windows)]
fn query_guest_result_vhdx_info(path: &Path) -> Result<(FridayVirtualDiskSizeInfo, u32, String), String> {
    use std::os::windows::ffi::OsStrExt;
    let path_wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    if path_wide.len() <= 1 {
        return Err("The result VHDX path is invalid for metadata verification.".into());
    }
    let storage_type = FridayVirtualStorageType {
        device_id: 3,
        vendor_id: FridayWindowsGuid {
            data1: 0xec984aec,
            data2: 0xa0f9,
            data3: 0x47e9,
            data4: [0x90, 0x1f, 0x71, 0x41, 0x5a, 0x66, 0x34, 0x5b],
        },
    };
    let open_parameters = FridayOpenVirtualDiskParameters {
        version: 1,
        parameters: FridayOpenVirtualDiskParametersData {
            version1: FridayOpenVirtualDiskVersion1 { read_write_depth: 0 },
        },
    };
    let mut raw_handle: *mut std::ffi::c_void = std::ptr::null_mut();
    // GET_INFO only: this handle cannot attach, modify, or write the disk.
    let open_status = unsafe {
        OpenVirtualDisk(&storage_type, path_wide.as_ptr(), 0x0008_0000, 0, &open_parameters, &mut raw_handle)
    };
    if open_status != 0 || raw_handle.is_null() {
        return Err(format!("Windows could not open the result VHDX for read-only metadata verification (error {open_status})."));
    }
    let handle = OwnedVirtualDiskHandle(raw_handle);

    let mut size_info = FridayGetVirtualDiskInfo {
        version: 1, // GET_VIRTUAL_DISK_INFO_SIZE
        data: FridayGetVirtualDiskInfoData { alignment: [0; 3] },
    };
    let mut size_bytes = std::mem::size_of::<FridayGetVirtualDiskInfo>() as u32;
    let size_status = unsafe {
        GetVirtualDiskInformation(handle.0, &mut size_bytes, &mut size_info, std::ptr::null_mut())
    };
    if size_status != 0 {
        return Err(format!("Windows could not read the result VHDX capacity (error {size_status})."));
    }
    let size = unsafe { size_info.data.size };

    let mut subtype_info = FridayGetVirtualDiskInfo {
        version: 7, // GET_VIRTUAL_DISK_INFO_PROVIDER_SUBTYPE
        data: FridayGetVirtualDiskInfoData { alignment: [0; 3] },
    };
    let mut subtype_bytes = std::mem::size_of::<FridayGetVirtualDiskInfo>() as u32;
    let subtype_status = unsafe {
        GetVirtualDiskInformation(handle.0, &mut subtype_bytes, &mut subtype_info, std::ptr::null_mut())
    };
    if subtype_status != 0 {
        return Err(format!("Windows could not verify the result VHDX allocation type (error {subtype_status})."));
    }
    let subtype = unsafe { subtype_info.data.provider_subtype };

    let mut identifier_info = FridayGetVirtualDiskInfo {
        version: 2, // GET_VIRTUAL_DISK_INFO_IDENTIFIER
        data: FridayGetVirtualDiskInfoData { alignment: [0; 3] },
    };
    let mut identifier_bytes = std::mem::size_of::<FridayGetVirtualDiskInfo>() as u32;
    let identifier_status = unsafe {
        GetVirtualDiskInformation(handle.0, &mut identifier_bytes, &mut identifier_info, std::ptr::null_mut())
    };
    if identifier_status != 0 {
        return Err(format!("Windows could not read the persistent result VHDX identifier (error {identifier_status})."));
    }
    let identifier = unsafe { identifier_info.data.identifier };
    let identifier = format_guest_vhdx_identifier(identifier)?;
    handle.close()?;
    Ok((size, subtype, identifier))
}

fn valid_guest_vhdx_identifier(value: &str) -> bool {
    value.len() == 36
        && [8, 13, 18, 23].iter().all(|index| value.as_bytes().get(*index) == Some(&b'-'))
        && value.bytes().enumerate().all(|(index, byte)| {
            [8, 13, 18, 23].contains(&index) || byte.is_ascii_hexdigit()
        })
        && value.bytes().any(|byte| byte != b'0' && byte != b'-')
        && value == value.to_ascii_lowercase()
}

#[cfg(windows)]
fn format_guest_vhdx_identifier(value: FridayWindowsGuid) -> Result<String, String> {
    if value.data1 == 0 && value.data2 == 0 && value.data3 == 0 && value.data4.iter().all(|byte| *byte == 0) {
        return Err("Windows returned an empty result VHDX identifier.".into());
    }
    let formatted = format!(
        "{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        value.data1, value.data2, value.data3,
        value.data4[0], value.data4[1], value.data4[2], value.data4[3],
        value.data4[4], value.data4[5], value.data4[6], value.data4[7]
    );
    if !valid_guest_vhdx_identifier(&formatted) {
        return Err("Windows returned a malformed result VHDX identifier.".into());
    }
    Ok(formatted)
}

#[cfg(windows)]
fn validate_guest_result_vhdx_readback(
    size: FridayVirtualDiskSizeInfo,
    provider_subtype: u32,
    identifier: &str,
    expected_capacity: u64,
) -> Result<(), String> {
    if expected_capacity != GUEST_RESULT_VOLUME_CAPACITY_BYTES
        || size.virtual_size != expected_capacity
        || size.physical_size < expected_capacity
        || size.block_size != 0 // Windows reports the default block-size sentinel for fixed VHDX disks
        || size.sector_size != 512
        || provider_subtype != 2 // VIRTUAL_DISK_TYPE_FIXED
        || !valid_guest_vhdx_identifier(identifier)
    {
        return Err(format!(
            "The result VHDX metadata does not match FRIDAY's fixed-capacity disk policy (expected_capacity={}, virtual_size={}, physical_size={}, block_size={}, sector_size={}, provider_subtype={}, identifier_valid={}).",
            expected_capacity, size.virtual_size, size.physical_size, size.block_size, size.sector_size, provider_subtype, valid_guest_vhdx_identifier(identifier)
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn verify_guest_result_vhdx_capacity(path: &Path, expected_capacity: u64) -> Result<String, String> {
    let (size, provider_subtype, identifier) = query_guest_result_vhdx_info(path)?;
    validate_guest_result_vhdx_readback(size, provider_subtype, &identifier, expected_capacity)?;
    Ok(identifier)
}

#[cfg(windows)]
fn create_guest_result_vhdx(
    plan: &GuestResultVolumePlan,
    trusted_storage_root: &Path,
) -> Result<PathBuf, String> {
    validate_guest_result_volume_plan(plan, trusted_storage_root, &plan.run_id)?;
    let verified_directory = verify_guest_result_volume_directory(plan, trusted_storage_root)?;
    if verified_directory != plan.run_directory {
        return Err("The result-volume directory identity changed; VHDX creation was refused.".into());
    }
    match fs::symlink_metadata(&plan.vhdx_path) {
        Ok(_) => return Err("A result VHDX already exists; replacement is refused.".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err("The result VHDX path could not be safely inspected.".into()),
    }
    if plan.capacity_bytes != GUEST_RESULT_VOLUME_CAPACITY_BYTES
        || plan.capacity_bytes < 3 * 1024 * 1024
        || plan.capacity_bytes % 512 != 0
    {
        return Err("The result VHDX capacity is outside FRIDAY's fixed creation policy.".into());
    }

    use std::os::windows::ffi::OsStrExt;
    let path_wide: Vec<u16> = plan.vhdx_path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    if path_wide.len() <= 1 {
        return Err("The result VHDX path is invalid.".into());
    }

    // Microsoft VHDX provider. Version 2 is required for the VHDX 1 MiB
    // block-size and 4 KiB physical-sector settings. A zero GUID requests a
    // system-generated unique disk identifier; no parent/source disk is used.
    let storage_type = FridayVirtualStorageType {
        device_id: 3, // VIRTUAL_STORAGE_TYPE_DEVICE_VHDX
        vendor_id: FridayWindowsGuid {
            data1: 0xec984aec,
            data2: 0xa0f9,
            data3: 0x47e9,
            data4: [0x90, 0x1f, 0x71, 0x41, 0x5a, 0x66, 0x34, 0x5b],
        },
    };
    let zero_guid = FridayWindowsGuid {
        data1: 0, data2: 0, data3: 0, data4: [0; 8],
    };
    let version2 = FridayCreateVirtualDiskVersion2 {
        unique_id: zero_guid,
        maximum_size: plan.capacity_bytes,
        block_size_in_bytes: 0, // Fixed VHDX uses the Windows default block-size sentinel.
        sector_size_in_bytes: 512,
        physical_sector_size_in_bytes: 4096,
        parent_path: std::ptr::null(),
        source_path: std::ptr::null(),
        open_flags: 0,
        parent_virtual_storage_type: FridayVirtualStorageType {
            device_id: 0, vendor_id: zero_guid,
        },
        source_virtual_storage_type: FridayVirtualStorageType {
            device_id: 0, vendor_id: zero_guid,
        },
        resiliency_guid: zero_guid,
    };
    let parameters = FridayCreateVirtualDiskParameters {
        version: 2, // CREATE_VIRTUAL_DISK_VERSION_2
        parameters: FridayCreateVirtualDiskParametersUnion {
            version2: std::mem::ManuallyDrop::new(version2),
        },
    };

    let mut raw_handle: *mut std::ffi::c_void = std::ptr::null_mut();
    // VIRTUAL_DISK_ACCESS_NONE is required for Version 2. FULL_PHYSICAL_ALLOCATION
    // requests a fixed-size VHDX instead of a dynamically expanding backing file.
    let status = unsafe {
        CreateVirtualDisk(
            &storage_type,
            path_wide.as_ptr(),
            0,
            std::ptr::null(),
            0x1, // CREATE_VIRTUAL_DISK_FLAG_FULL_PHYSICAL_ALLOCATION
            0,
            &parameters,
            std::ptr::null_mut(),
            &mut raw_handle,
        )
    };
    if status != 0 {
        return Err(format!("Windows refused to create the private result VHDX (error {status}); the run directory was preserved."));
    }
    if raw_handle.is_null() {
        return Err("Windows reported VHDX creation without returning an owned handle; the run directory was preserved.".into());
    }
    OwnedVirtualDiskHandle(raw_handle).close()?;

    // Verify the resulting object through a no-follow file open. Never delete
    // an unexpected or partially-created file automatically; recovery owns it.
    let metadata = fs::symlink_metadata(&plan.vhdx_path)
        .map_err(|_| "Windows created a VHDX that FRIDAY could not safely inspect; it was preserved for recovery.".to_string())?;
    if !metadata.is_file() || is_reparse_or_symlink(&metadata) || metadata.len() < 8 {
        return Err("The created result VHDX is not an ordinary non-empty file; it was preserved for recovery.".into());
    }
    let mut file = open_source_file(&plan.vhdx_path)
        .map_err(|_| "The created result VHDX could not be opened without following links; it was preserved.".to_string())?;
    let opened = file.metadata()
        .map_err(|_| "The created result VHDX handle could not be verified; it was preserved.".to_string())?;
    if !opened.is_file() || is_reparse_or_symlink(&opened) || opened.len() != metadata.len() {
        return Err("The result VHDX changed during verification; it was preserved for recovery.".into());
    }
    let mut signature = [0u8; 8];
    file.read_exact(&mut signature)
        .map_err(|_| "The result VHDX header could not be read; it was preserved for recovery.".to_string())?;
    if &signature != b"vhdxfile" {
        return Err("Windows returned a file without the expected VHDX signature; it was preserved for recovery.".into());
    }
    let identifier = verify_guest_result_vhdx_capacity(&plan.vhdx_path, plan.capacity_bytes)
        .map_err(|error| format!("The created VHDX failed Windows metadata verification ({error}); it was preserved for recovery."))?;
    write_guest_result_vhdx_ownership_marker(plan, trusted_storage_root, &identifier)
        .map_err(|error| format!("The created VHDX identity could not be bound to its FRIDAY run ({error}); it was preserved for recovery."))?;
    verify_guest_result_vhdx_identity(plan, trusted_storage_root)
        .map_err(|error| format!("The created VHDX failed its final read-only ownership check ({error}); it was preserved for recovery."))?;
    Ok(plan.vhdx_path.clone())
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GuestResultVolumeOwnershipMarker {
    schema_version: u32,
    run_id: String,
    owner: String,
}

/// Atomically reserves a new per-run directory and writes its ownership record.
/// If marker creation or persistence fails, the incomplete directory is left
/// untouched for recovery rather than being recursively deleted.
fn reserve_guest_result_volume_directory(
    plan: &GuestResultVolumePlan,
    trusted_storage_root: &Path,
) -> Result<PathBuf, String> {
    validate_guest_result_volume_plan(plan, trusted_storage_root, &plan.run_id)?;
    fs::create_dir(&plan.run_directory)
        .map_err(|_| "FRIDAY could not exclusively reserve this result-volume run directory.".to_string())?;
    // This is private host state, never a general guest share. Apply the same
    // fail-closed owner/SYSTEM/Administrators boundary as the snapshot workspace
    // before publishing any ownership metadata.
    secure_private_workspace_directory(&plan.run_directory)
        .map_err(|_| "FRIDAY could not restrict the reserved result-volume directory; it was preserved for recovery.".to_string())?;

    let marker = GuestResultVolumeOwnershipMarker {
        schema_version: GUEST_RESULT_VOLUME_OWNER_SCHEMA,
        run_id: plan.run_id.clone(),
        owner: "FRIDAY-CodingAssistant-ResultVolume".to_string(),
    };
    let bytes = serde_json::to_vec(&marker)
        .map_err(|_| "FRIDAY could not prepare result-volume ownership metadata.".to_string())?;
    let marker_path = plan.run_directory.join(GUEST_RESULT_VOLUME_OWNER_MARKER);
    let mut file = OpenOptions::new().write(true).create_new(true).open(&marker_path)
        .map_err(|_| "FRIDAY could not exclusively create result-volume ownership metadata; the directory was preserved.".to_string())?;
    use std::io::Write;
    file.write_all(&bytes)
        .map_err(|_| "FRIDAY could not write result-volume ownership metadata; the directory was preserved.".to_string())?;
    file.sync_all()
        .map_err(|_| "FRIDAY could not persist result-volume ownership metadata; the directory was preserved.".to_string())?;
    verify_guest_result_volume_directory(plan, trusted_storage_root)
}

/// Verifies the exact per-run child directory and its no-follow ownership marker.
/// This does not authorize disk detach or recursive deletion.
fn verify_guest_result_volume_directory(
    plan: &GuestResultVolumePlan,
    trusted_storage_root: &Path,
) -> Result<PathBuf, String> {
    validate_guest_result_volume_plan(plan, trusted_storage_root, &plan.run_id)?;
    let root = fs::canonicalize(trusted_storage_root)
        .map_err(|_| "The trusted result-volume root could not be reverified.".to_string())?;
    let metadata = fs::symlink_metadata(&plan.run_directory)
        .map_err(|_| "The result-volume run directory is unavailable.".to_string())?;
    if !metadata.is_dir() || is_reparse_or_symlink(&metadata) {
        return Err("The result-volume run directory is not an ordinary directory.".into());
    }
    let canonical = fs::canonicalize(&plan.run_directory)
        .map_err(|_| "The result-volume run directory could not be canonicalized.".to_string())?;
    if canonical.parent() != Some(root.as_path())
        || canonical.file_name().and_then(OsStr::to_str) != Some(plan.run_id.as_str())
    {
        return Err("The result-volume run directory failed its exact-child ownership check.".into());
    }

    let marker_path = canonical.join(GUEST_RESULT_VOLUME_OWNER_MARKER);
    let marker_metadata = fs::symlink_metadata(&marker_path)
        .map_err(|_| "Result-volume ownership evidence is missing; the directory was preserved.".to_string())?;
    if !marker_metadata.is_file() || is_reparse_or_symlink(&marker_metadata) || marker_metadata.len() > 1024 {
        return Err("Result-volume ownership evidence is not a small regular file.".into());
    }
    let mut marker_file = open_source_file(&marker_path)
        .map_err(|_| "Result-volume ownership evidence could not be opened without following links.".to_string())?;
    let opened_metadata = marker_file.metadata()
        .map_err(|_| "Result-volume ownership evidence could not be verified.".to_string())?;
    if !opened_metadata.is_file() || is_reparse_or_symlink(&opened_metadata) || opened_metadata.len() > 1024 {
        return Err("Result-volume ownership evidence changed or exceeded its size limit.".into());
    }
    let mut bytes = Vec::new();
    marker_file.take(1025).read_to_end(&mut bytes)
        .map_err(|_| "Result-volume ownership evidence could not be read.".to_string())?;
    if bytes.len() > 1024 {
        return Err("Result-volume ownership evidence exceeded its read limit.".into());
    }
    let marker: GuestResultVolumeOwnershipMarker = serde_json::from_slice(&bytes)
        .map_err(|_| "Result-volume ownership evidence is malformed.".to_string())?;
    if marker.schema_version != GUEST_RESULT_VOLUME_OWNER_SCHEMA
        || marker.run_id != plan.run_id
        || marker.owner != "FRIDAY-CodingAssistant-ResultVolume"
    {
        return Err("Result-volume ownership evidence does not match this FRIDAY run.".into());
    }
    Ok(canonical)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SandboxSessionState {
    Planned,
    LaunchRequested,
    ProcessObserved,
    SessionConfirmed,
    Running,
    StopRequested,
    Stopped,
    Removed,
    RecoveryRequired,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SandboxSessionEvent {
    RequestLaunch,
    ObserveProcess,
    ConfirmSession,
    ConfirmRunning,
    RequestStop,
    ConfirmStopped,
    RemoveOwnershipRecord,
    PreserveForRecovery,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SandboxProcessIdentity {
    session_id: String,
    process_id: u32,
    process_creation_time: u64,
    image_path: String,
}

fn valid_windows_sandbox_image_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    let drive_absolute = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/');
    let unc_absolute = value.starts_with(r"\\") || value.starts_with(r"\\?\\");
    let normalized = value.replace('/', "\\");
    let has_parent_component = normalized.split('\\').any(|part| part == "..");
    let file_name = normalized.rsplit('\\').next().unwrap_or("");
    (drive_absolute || unc_absolute)
        && !has_parent_component
        && file_name.eq_ignore_ascii_case("WindowsSandbox.exe")
}

fn validate_sandbox_process_identity(identity: &SandboxProcessIdentity) -> Result<(), String> {
    if !valid_snapshot_id(&identity.session_id)
        || identity.process_id <= 4
        || identity.process_creation_time == 0
        || !valid_windows_sandbox_image_path(&identity.image_path)
    {
        return Err("The Windows Sandbox process identity is incomplete or invalid.".into());
    }
    Ok(())
}

/// Pure ownership gate. A stop or cleanup is authorized only for the exact
/// process identity FRIDAY recorded and re-verified; PID alone is never enough.
fn advance_sandbox_session(
    state: SandboxSessionState,
    event: SandboxSessionEvent,
    identity: Option<&SandboxProcessIdentity>,
    observed_identity: Option<&SandboxProcessIdentity>,
    ownership_verified: bool,
    process_confirmed_stopped: bool,
) -> Result<SandboxSessionState, String> {
    if event == SandboxSessionEvent::PreserveForRecovery {
        return if state == SandboxSessionState::Removed {
            Err("A removed Sandbox session record cannot be restored to recovery.".into())
        } else {
            Ok(SandboxSessionState::RecoveryRequired)
        };
    }
    if state == SandboxSessionState::RecoveryRequired {
        return Err("Sandbox session ownership is ambiguous; automatic actions are blocked.".into());
    }
    match event {
        SandboxSessionEvent::RequestLaunch if state == SandboxSessionState::Planned => {
            Ok(SandboxSessionState::LaunchRequested)
        }
        SandboxSessionEvent::ObserveProcess if state == SandboxSessionState::LaunchRequested => {
            let observed = observed_identity.ok_or_else(|| "No Sandbox process identity was observed.".to_string())?;
            validate_sandbox_process_identity(observed)?;
            Ok(SandboxSessionState::ProcessObserved)
        }
        SandboxSessionEvent::ConfirmSession if state == SandboxSessionState::ProcessObserved => {
            let expected = identity.ok_or_else(|| "FRIDAY's recorded Sandbox process identity is missing.".to_string())?;
            let observed = observed_identity.ok_or_else(|| "The observed Sandbox process identity is missing.".to_string())?;
            validate_sandbox_process_identity(expected)?;
            validate_sandbox_process_identity(observed)?;
            if expected != observed || !ownership_verified {
                return Err("The observed Sandbox process does not match FRIDAY's exact owned session.".into());
            }
            Ok(SandboxSessionState::SessionConfirmed)
        }
        SandboxSessionEvent::ConfirmRunning if state == SandboxSessionState::SessionConfirmed => {
            if !ownership_verified {
                return Err("Sandbox ownership was not re-verified before marking the session running.".into());
            }
            Ok(SandboxSessionState::Running)
        }
        SandboxSessionEvent::RequestStop if state == SandboxSessionState::Running => {
            let expected = identity.ok_or_else(|| "FRIDAY's recorded Sandbox process identity is missing.".to_string())?;
            let observed = observed_identity.ok_or_else(|| "The current Sandbox process identity is missing.".to_string())?;
            validate_sandbox_process_identity(expected)?;
            validate_sandbox_process_identity(observed)?;
            if expected != observed || !ownership_verified {
                return Err("Stopping was refused because the Sandbox process identity or ownership changed.".into());
            }
            Ok(SandboxSessionState::StopRequested)
        }
        SandboxSessionEvent::ConfirmStopped if state == SandboxSessionState::StopRequested => {
            let expected = identity.ok_or_else(|| "FRIDAY's recorded Sandbox process identity is missing.".to_string())?;
            let observed = observed_identity.ok_or_else(|| "The final Sandbox process identity is missing.".to_string())?;
            validate_sandbox_process_identity(expected)?;
            validate_sandbox_process_identity(observed)?;
            if expected != observed || !ownership_verified || !process_confirmed_stopped {
                return Err("The exact owned Sandbox process was not positively verified as stopped.".into());
            }
            Ok(SandboxSessionState::Stopped)
        }
        SandboxSessionEvent::RemoveOwnershipRecord if state == SandboxSessionState::Stopped => {
            let expected = identity.ok_or_else(|| "FRIDAY's recorded Sandbox process identity is missing.".to_string())?;
            validate_sandbox_process_identity(expected)?;
            if !ownership_verified {
                return Err("Sandbox ownership evidence is required before removing its session record.".into());
            }
            Ok(SandboxSessionState::Removed)
        }
        _ => Err("The Sandbox session transition is out of order or lacks exact ownership evidence.".into()),
    }
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


/// Restrict the private snapshot workspace to the current user and the
/// operating-system principals required for administration and recovery.
fn secure_private_workspace_directory(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| "FRIDAY could not inspect its private workspace directory.".to_string())?;
    if !metadata.is_dir() || is_reparse_or_symlink(&metadata) {
        return Err("The private workspace is not an ordinary directory.".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::{fs::OpenOptionsExt, io::AsRawFd};
        let directory = OpenOptions::new().read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
            .map_err(|_| "FRIDAY could not securely open its private workspace directory.".to_string())?;
        let opened = directory.metadata().map_err(|_| "FRIDAY could not verify its private workspace directory.".to_string())?;
        if !opened.is_dir() || is_reparse_or_symlink(&opened) {
            return Err("The private workspace changed type during permission setup.".into());
        }
        if unsafe { libc::fchmod(directory.as_raw_fd(), 0o700) } != 0 {
            return Err("FRIDAY could not restrict private workspace permissions.".into());
        }
        let verified = directory.metadata().map_err(|_| "FRIDAY could not verify private workspace permissions.".to_string())?;
        use std::os::unix::fs::PermissionsExt;
        if verified.permissions().mode() & 0o077 != 0 {
            return Err("Private workspace permissions are broader than owner-only.".into());
        }
    }
    #[cfg(windows)]
    {
        let system_root = env::var_os("SystemRoot")
            .ok_or_else(|| "Windows system directory is unavailable; private workspace ACL setup was refused.".to_string())?;
        let system32 = PathBuf::from(system_root).join("System32");
        let identity = Command::new(system32.join("whoami.exe"))
            .args(["/user", "/fo", "csv", "/nh"]).output()
            .map_err(|_| "FRIDAY could not identify the current Windows account for workspace ACL setup.".to_string())?;
        if !identity.status.success() {
            return Err("Windows account identity lookup failed; private workspace ACL setup was refused.".into());
        }
        let identity_text = String::from_utf8(identity.stdout)
            .map_err(|_| "Windows account identity output was not valid UTF-8.".to_string())?;
        let sid = identity_text.lines().filter_map(|line| line.split(',').last())
            .map(|value| value.trim().trim_matches('"'))
            .find(|value| value.starts_with("S-1-") && value.len() <= 184)
            .ok_or_else(|| "FRIDAY could not validate the current Windows account SID.".to_string())?;
        let script = r#"$ErrorActionPreference = 'Stop'
$path = [System.Environment]::GetEnvironmentVariable('FRIDAY_WORKSPACE_PATH')
$currentSid = [System.Environment]::GetEnvironmentVariable('FRIDAY_WORKSPACE_SID')
if ([string]::IsNullOrWhiteSpace($path) -or [string]::IsNullOrWhiteSpace($currentSid)) {
  throw 'ACL_INPUT_MISSING'
}
$allowed = @($currentSid, 'S-1-5-18', 'S-1-5-32-544')
try {
  # Use the .NET filesystem ACL APIs directly. Keep stage-specific diagnostics
  # so Windows CI identifies the exact failing ACL operation.
  # A protected SDDL descriptor avoids ambiguous rule-merging and purge behavior.
  $sddl = "D:P(A;OICI;FA;;;$currentSid)(A;OICI;FA;;;S-1-5-18)(A;OICI;FA;;;S-1-5-32-544)"
  # Windows PowerShell 5.1 exposes ACL methods on DirectoryInfo instances.
  $directory = [System.IO.DirectoryInfo]::new($path)
  $stage = 'read-acl'
  $acl = $directory.GetAccessControl()
  $stage = 'apply-sddl'
  $acl.SetSecurityDescriptorSddlForm($sddl)
  $stage = 'write-acl'
  $directory.SetAccessControl($acl)
  $stage = 'verify-acl'
  $check = $directory.GetAccessControl()
  if (-not $check.AreAccessRulesProtected) { throw 'ACL_INHERITANCE' }
  $rules = @($check.GetAccessRules($true, $true, [System.Security.Principal.SecurityIdentifier]))
  if ($rules.Count -ne 3) { throw 'ACL_RULE_COUNT' }
  foreach ($rule in $rules) {
    if ($rule.IsInherited -or $rule.AccessControlType -ne [System.Security.AccessControl.AccessControlType]::Allow -or $rule.FileSystemRights -ne [System.Security.AccessControl.FileSystemRights]::FullControl -or $allowed -notcontains $rule.IdentityReference.Value) {
      throw 'ACL_RULE_CONTENT'
    }
  }
} catch {
  $message = ($_.Exception.Message -replace '[\r\n]+', ' ').Trim()
  [Console]::Error.WriteLine('PRIVATE_WORKSPACE_ACL_FAILED:' + $stage + ':' + $_.Exception.GetType().FullName + ':' + $message + ':' + $_.FullyQualifiedErrorId)
  exit 1
}
"#;
        let powershell = system32.join("WindowsPowerShell").join("v1.0").join("powershell.exe");
        let applied = Command::new(powershell)
            .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", script])
            // Pass values through the child process environment. PowerShell's
            // -Command argument parsing does not reliably populate $args for
            // this invocation shape, and paths may contain spaces or Unicode.
            .env("FRIDAY_WORKSPACE_PATH", path.as_os_str())
            .env("FRIDAY_WORKSPACE_SID", sid)
            .output()
            .map_err(|_| "FRIDAY could not apply restrictive Windows workspace permissions.".to_string())?;
        if !applied.status.success() {
            let diagnostic = String::from_utf8_lossy(&applied.stderr);
            let diagnostic = diagnostic.lines().find(|line| line.contains("PRIVATE_WORKSPACE_ACL_FAILED:"))
                .unwrap_or("PRIVATE_WORKSPACE_ACL_FAILED:UNKNOWN");
            return Err(format!("Windows workspace ACL could not be rebuilt and verified; snapshot operation was stopped ({diagnostic})."));
        }
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
    secure_private_workspace_directory(&workspace_root)?;
    let workspace_meta = fs::symlink_metadata(&workspace_root)
        .map_err(|_| "FRIDAY could not verify private sandbox workspace storage.".to_string())?;
    if !workspace_meta.is_dir() || is_reparse_or_symlink(&workspace_meta) {
        return Err("Private sandbox workspace storage is not a safe normal directory.".into());
    }

    let mut random = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut random);
    let snapshot_id = random.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    let snapshot_path = workspace_root.join(&snapshot_id);

    // Production snapshots are collected and written through held directory
    // handles. The schema-2 inventory is created last, only after every bounded
    // payload file has been written and flushed successfully.
    let (files_copied, bytes_copied) =
        crate::coding_sandbox_fs::prepare_snapshot_from_paths(
            &source_root,
            &workspace_root,
            &snapshot_id,
            (MAX_SNAPSHOT_DEPTH, MAX_SNAPSHOT_FILES, MAX_SNAPSHOT_FILE_BYTES, MAX_SNAPSHOT_TOTAL_BYTES),
            safe_project_file_name,
            excluded_directory,
        ).map_err(|_| "FRIDAY could not safely create a complete isolated project snapshot. Any incomplete, unmarked data is preserved for manual review.".to_string())?;
    if files_copied == 0 {
        return Err("No eligible source or project-manifest files were found to snapshot.".into());
    }

    OWNED_SNAPSHOTS.lock()
        .map_err(|_| "Snapshot ownership tracking is temporarily unavailable.".to_string())?
        .insert(snapshot_id.clone(), snapshot_path);
    Ok(CodingSandboxSnapshot {
        snapshot_id,
        files_copied,
        bytes_copied,
        truncated: false,
        message: "A complete bounded schema-2 snapshot was prepared through verified directory handles. The original project was not changed. Execution remains disabled.".into(),
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
    let config = format!(r#"<Configuration>
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
"#);
    validate_sandbox_config_policy(&config)?;
    Ok(config)
}

/// Strict validator for FRIDAY's one generated Windows Sandbox template.
/// This intentionally accepts only the exact template grammar, not arbitrary XML.
/// It does not prove that the Windows host enforces the settings.
fn validate_sandbox_config_policy(config: &str) -> Result<(), String> {
    const PREFIX: &str = r#"<Configuration>
  <VGpu>Disable</VGpu>
  <Networking>Disable</Networking>
  <AudioInput>Disable</AudioInput>
  <VideoInput>Disable</VideoInput>
  <ClipboardRedirection>Disable</ClipboardRedirection>
  <PrinterRedirection>Disable</PrinterRedirection>
  <ProtectedClient>Enable</ProtectedClient>
  <MappedFolders>
    <MappedFolder>
      <HostFolder>"#;
    const SUFFIX: &str = r#"</HostFolder>
      <SandboxFolder>C:\FRIDAY\Project</SandboxFolder>
      <ReadOnly>true</ReadOnly>
    </MappedFolder>
  </MappedFolders>
</Configuration>
"#;

    let Some(encoded_host_path) = config.strip_prefix(PREFIX)
        .and_then(|rest| rest.strip_suffix(SUFFIX))
    else {
        return Err("The Windows Sandbox configuration does not match FRIDAY's exact isolation template; launch is refused.".into());
    };
    if encoded_host_path.is_empty() {
        return Err("The Windows Sandbox host folder is empty; launch is refused.".into());
    }

    // Only the five named entities emitted by xml_escape are accepted. Decode
    // and re-encode to reject malformed, unknown, or raw entity syntax.
    let decoded = decode_xml_text(encoded_host_path)
        .ok_or_else(|| "The Windows Sandbox host folder contains invalid XML text; launch is refused.".to_string())?;
    if decoded.is_empty() || decoded.chars().any(char::is_control)
        || xml_escape(&decoded) != encoded_host_path
    {
        return Err("The Windows Sandbox host folder is not safely XML-escaped; launch is refused.".into());
    }
    Ok(())
}

fn decode_xml_text(value: &str) -> Option<String> {
    let mut decoded = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(index) = rest.find('&') {
        decoded.push_str(&rest[..index]);
        rest = &rest[index..];
        let (entity, replacement) = if rest.starts_with("&amp;") {
            ("&amp;", "&")
        } else if rest.starts_with("&lt;") {
            ("&lt;", "<")
        } else if rest.starts_with("&gt;") {
            ("&gt;", ">")
        } else if rest.starts_with("&quot;") {
            ("&quot;", "\"")
        } else if rest.starts_with("&apos;") {
            ("&apos;", "'")
        } else {
            return None;
        };
        decoded.push_str(replacement);
        rest = &rest[entity.len()..];
    }
    decoded.push_str(rest);
    Some(decoded)
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
    // Re-assert the private ACL before enumerating old workspace entries.
    secure_private_workspace_directory(&workspace_root)?;
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
        match crate::coding_sandbox_fs::cleanup_snapshot_from_workspace_path(
            &workspace_root,
            &name,
            (MAX_SNAPSHOT_DEPTH, MAX_SNAPSHOT_FILES, MAX_SNAPSHOT_FILE_BYTES, MAX_SNAPSHOT_TOTAL_BYTES),
        ) {
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
    crate::coding_sandbox_fs::cleanup_snapshot_from_workspace_path(
        parent,
        &snapshot_id,
        (MAX_SNAPSHOT_DEPTH, MAX_SNAPSHOT_FILES, MAX_SNAPSHOT_FILE_BYTES, MAX_SNAPSHOT_TOTAL_BYTES),
    ).map_err(|_| "The snapshot could not be safely verified and removed. Its data has been preserved.".to_string())?;
    owned.remove(&snapshot_id);
    Ok("The isolated snapshot copy was removed. The original project was not changed.".into())
}


const GUEST_RESULT_GPT_SECTOR_BYTES: u64 = 512;
const GUEST_RESULT_GPT_ALIGNMENT_SECTORS: u64 = 2048;
const GUEST_RESULT_GPT_METADATA_SECTORS: u64 = 34;
const GUEST_RESULT_PARTITION_TYPE_GUID: &str = "EBD0A0A2-B9E5-4433-87C0-68B6B72699C7";
const GUEST_RESULT_FILESYSTEM_LABEL: &str = "FRIDAY_RESULTS";

#[derive(Clone, Debug, PartialEq, Eq)]
struct GuestResultPartitionPlan {
    run_id: String,
    disk_capacity_bytes: u64,
    logical_sector_bytes: u32,
    partition_number: u32,
    first_lba: u64,
    last_lba: u64,
    partition_type_guid: String,
    partition_unique_guid: String,
    filesystem: String,
    filesystem_label: String,
}

fn build_guest_result_partition_plan(
    run_id: &str,
    disk_capacity_bytes: u64,
    logical_sector_bytes: u32,
) -> Result<GuestResultPartitionPlan, String> {
    if !valid_snapshot_id(run_id)
        || disk_capacity_bytes != GUEST_RESULT_VOLUME_CAPACITY_BYTES
        || logical_sector_bytes as u64 != GUEST_RESULT_GPT_SECTOR_BYTES
        || disk_capacity_bytes % GUEST_RESULT_GPT_SECTOR_BYTES != 0
    {
        return Err("The attached result disk does not match FRIDAY's fixed GPT layout policy.".into());
    }
    let total_sectors = disk_capacity_bytes / GUEST_RESULT_GPT_SECTOR_BYTES;
    if total_sectors <= GUEST_RESULT_GPT_METADATA_SECTORS * 2 {
        return Err("The result disk is too small for a safe GPT layout.".into());
    }
    let first_lba = GUEST_RESULT_GPT_ALIGNMENT_SECTORS;
    let last_lba = total_sectors - GUEST_RESULT_GPT_METADATA_SECTORS - 1;
    if first_lba > last_lba {
        return Err("The result disk has no usable aligned GPT partition range.".into());
    }
    // Bind the unique partition GUID to the already-generated per-run identifier.
    let compact = run_id.to_ascii_lowercase();
    let partition_unique_guid = format!(
        "{}-{}-{}-{}-{}",
        &compact[0..8], &compact[8..12], &compact[12..16],
        &compact[16..20], &compact[20..32]
    ).to_ascii_uppercase();
    Ok(GuestResultPartitionPlan {
        run_id: run_id.to_string(),
        disk_capacity_bytes,
        logical_sector_bytes,
        partition_number: 1,
        first_lba,
        last_lba,
        partition_type_guid: GUEST_RESULT_PARTITION_TYPE_GUID.into(),
        partition_unique_guid,
        filesystem: "NTFS".into(),
        filesystem_label: GUEST_RESULT_FILESYSTEM_LABEL.into(),
    })
}

fn validate_guest_result_partition_plan(
    plan: &GuestResultPartitionPlan,
    expected_run_id: &str,
) -> Result<(), String> {
    let expected = build_guest_result_partition_plan(
        expected_run_id,
        GUEST_RESULT_VOLUME_CAPACITY_BYTES,
        GUEST_RESULT_GPT_SECTOR_BYTES as u32,
    )?;
    if plan != &expected {
        return Err("The result partition plan differs from FRIDAY's fixed per-run GPT and NTFS policy.".into());
    }
    Ok(())
}


const GUEST_RESULT_GPT_HEADER_BYTES: usize = 92;
const GUEST_RESULT_GPT_ENTRY_COUNT: usize = 128;
const GUEST_RESULT_GPT_ENTRY_BYTES: usize = 128;
const GUEST_RESULT_GPT_ARRAY_BYTES: usize = GUEST_RESULT_GPT_ENTRY_COUNT * GUEST_RESULT_GPT_ENTRY_BYTES;

#[derive(Clone, Debug, PartialEq, Eq)]
struct GuestResultGptMetadata {
    protective_mbr: Vec<u8>,
    primary_header: Vec<u8>,
    primary_entries: Vec<u8>,
    backup_entries: Vec<u8>,
    backup_header: Vec<u8>,
    disk_guid: String,
}

fn parse_canonical_guid_bytes(value: &str) -> Result<[u8; 16], String> {
    let compact = value.replace('-', "");
    if compact.len() != 32 || !compact.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("A GPT identifier is not a canonical hexadecimal GUID.".into());
    }
    let mut raw = [0u8; 16];
    for (index, byte) in raw.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&compact[index * 2..index * 2 + 2], 16)
            .map_err(|_| "A GPT identifier contains invalid hexadecimal data.".to_string())?;
    }
    // GPT stores GUID fields using the Windows/UEFI mixed-endian GUID layout.
    raw[..4].reverse();
    raw[4..6].reverse();
    raw[6..8].reverse();
    Ok(raw)
}

fn gpt_crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for byte in bytes {
        crc ^= *byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

fn write_gpt_header(
    sector: &mut [u8],
    current_lba: u64,
    backup_lba: u64,
    first_usable_lba: u64,
    last_usable_lba: u64,
    disk_guid: &[u8; 16],
    entries_lba: u64,
    entries_crc: u32,
) -> Result<(), String> {
    if sector.len() != GUEST_RESULT_GPT_SECTOR_BYTES as usize {
        return Err("The GPT header buffer does not match the fixed logical sector size.".into());
    }
    sector.fill(0);
    sector[0..8].copy_from_slice(b"EFI PART");
    sector[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
    sector[12..16].copy_from_slice(&(GUEST_RESULT_GPT_HEADER_BYTES as u32).to_le_bytes());
    sector[24..32].copy_from_slice(&current_lba.to_le_bytes());
    sector[32..40].copy_from_slice(&backup_lba.to_le_bytes());
    sector[40..48].copy_from_slice(&first_usable_lba.to_le_bytes());
    sector[48..56].copy_from_slice(&last_usable_lba.to_le_bytes());
    sector[56..72].copy_from_slice(disk_guid);
    sector[72..80].copy_from_slice(&entries_lba.to_le_bytes());
    sector[80..84].copy_from_slice(&(GUEST_RESULT_GPT_ENTRY_COUNT as u32).to_le_bytes());
    sector[84..88].copy_from_slice(&(GUEST_RESULT_GPT_ENTRY_BYTES as u32).to_le_bytes());
    sector[88..92].copy_from_slice(&entries_crc.to_le_bytes());
    let header_crc = gpt_crc32(&sector[..GUEST_RESULT_GPT_HEADER_BYTES]);
    sector[16..20].copy_from_slice(&header_crc.to_le_bytes());
    Ok(())
}

fn build_guest_result_gpt_metadata(
    plan: &GuestResultPartitionPlan,
) -> Result<GuestResultGptMetadata, String> {
    validate_guest_result_partition_plan(plan, &plan.run_id)?;
    let total_sectors = plan.disk_capacity_bytes / GUEST_RESULT_GPT_SECTOR_BYTES;
    let backup_header_lba = total_sectors - 1;
    let backup_entries_lba = backup_header_lba - (GUEST_RESULT_GPT_ARRAY_BYTES as u64 / GUEST_RESULT_GPT_SECTOR_BYTES);
    if plan.first_lba < GUEST_RESULT_GPT_ALIGNMENT_SECTORS
        || plan.last_lba >= backup_entries_lba
        || backup_entries_lba <= plan.last_lba
    {
        return Err("The planned partition overlaps reserved GPT metadata.".into());
    }

    let partition_type = parse_canonical_guid_bytes(&plan.partition_type_guid)?;
    let partition_id = parse_canonical_guid_bytes(&plan.partition_unique_guid)?;
    let compact_run = plan.run_id.to_ascii_lowercase();
    let mut disk_compact = compact_run.clone().into_bytes();
    disk_compact[0] = if disk_compact[0] == b'0' { b'1' } else { b'0' };
    let disk_guid_text = format!(
        "{}-{}-{}-{}-{}",
        String::from_utf8_lossy(&disk_compact[0..8]),
        String::from_utf8_lossy(&disk_compact[8..12]),
        String::from_utf8_lossy(&disk_compact[12..16]),
        String::from_utf8_lossy(&disk_compact[16..20]),
        String::from_utf8_lossy(&disk_compact[20..32]),
    ).to_ascii_uppercase();
    let disk_guid = parse_canonical_guid_bytes(&disk_guid_text)?;
    if disk_guid == partition_id || disk_guid.iter().all(|byte| *byte == 0) {
        return Err("The generated GPT disk identifier is not unique.".into());
    }

    let mut entries = vec![0u8; GUEST_RESULT_GPT_ARRAY_BYTES];
    let entry = &mut entries[..GUEST_RESULT_GPT_ENTRY_BYTES];
    entry[0..16].copy_from_slice(&partition_type);
    entry[16..32].copy_from_slice(&partition_id);
    entry[32..40].copy_from_slice(&plan.first_lba.to_le_bytes());
    entry[40..48].copy_from_slice(&plan.last_lba.to_le_bytes());
    let name: Vec<u16> = "FRIDAY_RESULTS".encode_utf16().collect();
    for (index, unit) in name.iter().enumerate() {
        let offset = 56 + index * 2;
        entry[offset..offset + 2].copy_from_slice(&unit.to_le_bytes());
    }
    let entries_crc = gpt_crc32(&entries);
    let mut primary_header = vec![0u8; GUEST_RESULT_GPT_SECTOR_BYTES as usize];
    let mut backup_header = vec![0u8; GUEST_RESULT_GPT_SECTOR_BYTES as usize];
    write_gpt_header(
        &mut primary_header, 1, backup_header_lba, 34, total_sectors - 34 - 1,
        &disk_guid, 2, entries_crc,
    )?;
    write_gpt_header(
        &mut backup_header, backup_header_lba, 1, 34, total_sectors - 34 - 1,
        &disk_guid, backup_entries_lba, entries_crc,
    )?;

    let mut protective_mbr = vec![0u8; GUEST_RESULT_GPT_SECTOR_BYTES as usize];
    let mbr_entry = &mut protective_mbr[446..462];
    mbr_entry[1..4].copy_from_slice(&[0x00, 0x02, 0x00]);
    mbr_entry[4] = 0xee;
    mbr_entry[5..8].copy_from_slice(&[0xff, 0xff, 0xff]);
    mbr_entry[8..12].copy_from_slice(&1u32.to_le_bytes());
    let protective_sectors = (total_sectors - 1).min(u32::MAX as u64) as u32;
    mbr_entry[12..16].copy_from_slice(&protective_sectors.to_le_bytes());
    protective_mbr[510] = 0x55;
    protective_mbr[511] = 0xaa;

    Ok(GuestResultGptMetadata {
        protective_mbr,
        primary_header,
        primary_entries: entries.clone(),
        backup_entries: entries,
        backup_header,
        disk_guid: disk_guid_text,
    })
}


#[cfg(windows)]
#[repr(C)]
struct FridayDiskGeometry {
    cylinders: i64,
    media_type: u32,
    tracks_per_cylinder: u32,
    sectors_per_track: u32,
    bytes_per_sector: u32,
}

#[cfg(windows)]
#[repr(C)]
struct FridayDiskGeometryEx {
    geometry: FridayDiskGeometry,
    disk_size: i64,
    data: [u8; 1],
}

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn CreateFileW(
        file_name: *const u16,
        desired_access: u32,
        share_mode: u32,
        security_attributes: *const std::ffi::c_void,
        creation_disposition: u32,
        flags_and_attributes: u32,
        template_file: *mut std::ffi::c_void,
    ) -> *mut std::ffi::c_void;
    fn DeviceIoControl(
        device: *mut std::ffi::c_void,
        control_code: u32,
        input_buffer: *const std::ffi::c_void,
        input_buffer_size: u32,
        output_buffer: *mut std::ffi::c_void,
        output_buffer_size: u32,
        bytes_returned: *mut u32,
        overlapped: *mut std::ffi::c_void,
    ) -> i32;
    fn ReadFile(
        file: *mut std::ffi::c_void,
        buffer: *mut u8,
        bytes_to_read: u32,
        bytes_read: *mut u32,
        overlapped: *mut std::ffi::c_void,
    ) -> i32;
    fn WriteFile(
        file: *mut std::ffi::c_void,
        buffer: *const u8,
        bytes_to_write: u32,
        bytes_written: *mut u32,
        overlapped: *mut std::ffi::c_void,
    ) -> i32;
    fn SetFilePointerEx(
        file: *mut std::ffi::c_void,
        distance: i64,
        new_position: *mut i64,
        move_method: u32,
    ) -> i32;
    fn FlushFileBuffers(file: *mut std::ffi::c_void) -> i32;
}

#[cfg(windows)]
struct OwnedRawGuestDisk(*mut std::ffi::c_void);

#[cfg(windows)]
impl Drop for OwnedRawGuestDisk {
    fn drop(&mut self) {
        const INVALID_HANDLE_VALUE: *mut std::ffi::c_void = -1isize as *mut std::ffi::c_void;
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            unsafe { CloseHandle(self.0); }
            self.0 = std::ptr::null_mut();
        }
    }
}

#[cfg(windows)]
impl OwnedRawGuestDisk {
    fn open_attached_device(attached: &OwnedAttachedGuestVhdx) -> Result<Self, String> {
        use std::os::windows::ffi::OsStrExt;
        let current_path = guest_vhdx_physical_path(attached.handle.0)?;
        if current_path != attached.physical_path {
            return Err("The attached VHDX physical-device identity changed; raw disk access was refused.".into());
        }
        let wide: Vec<u16> = std::ffi::OsStr::new(&current_path)
            .encode_wide().chain(std::iter::once(0)).collect();
        const GENERIC_READ: u32 = 0x8000_0000;
        const GENERIC_WRITE: u32 = 0x4000_0000;
        const FILE_SHARE_READ: u32 = 0x0000_0001;
        const FILE_SHARE_WRITE: u32 = 0x0000_0002;
        const OPEN_EXISTING: u32 = 3;
        const FILE_ATTRIBUTE_NORMAL: u32 = 0x0000_0080;
        const FILE_FLAG_WRITE_THROUGH: u32 = 0x8000_0000;
        const INVALID_HANDLE_VALUE: *mut std::ffi::c_void = -1isize as *mut std::ffi::c_void;
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(), GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE, std::ptr::null(),
                OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL | FILE_FLAG_WRITE_THROUGH,
                std::ptr::null_mut(),
            )
        };
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return Err("Windows refused raw access to FRIDAY's exact attached result disk.".into());
        }
        let owned = Self(handle);
        owned.verify_fixed_geometry()?;
        Ok(owned)
    }

    fn verify_windows_partition_layout(
        &self,
        plan: &GuestResultPartitionPlan,
        metadata: &GuestResultGptMetadata,
    ) -> Result<(), String> {
        const IOCTL_DISK_UPDATE_PROPERTIES: u32 = 0x0007_0140;
        const IOCTL_DISK_GET_DRIVE_LAYOUT_EX: u32 = 0x0007_0050;
        // Raw GPT writes bypass the partition manager. Ask Windows to discard
        // any cached layout before requesting its independent interpretation.
        let mut update_returned = 0u32;
        let refreshed = unsafe {
            DeviceIoControl(
                self.0,
                IOCTL_DISK_UPDATE_PROPERTIES,
                std::ptr::null(),
                0,
                std::ptr::null_mut(),
                0,
                &mut update_returned,
                std::ptr::null_mut(),
            )
        };
        if refreshed == 0 {
            return Err("Windows could not refresh the attached result disk's cached partition properties.".into());
        }
        let mut output = vec![0u64; 8192];
        let mut returned = 0u32;
        let ok = unsafe {
            DeviceIoControl(
                self.0,
                IOCTL_DISK_GET_DRIVE_LAYOUT_EX,
                std::ptr::null(),
                0,
                output.as_mut_ptr() as *mut std::ffi::c_void,
                (output.len() * std::mem::size_of::<u64>()) as u32,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err("Windows could not independently enumerate the result disk's partition layout.".into());
        }
        let returned = returned as usize;
        let output_bytes = unsafe {
            std::slice::from_raw_parts(output.as_ptr() as *const u8, output.len() * 8)
        };
        verify_guest_result_windows_partition_layout_bytes(output_bytes, returned, plan, metadata)
    }

    fn verify_fixed_geometry(&self) -> Result<(), String> {
        const IOCTL_DISK_GET_DRIVE_GEOMETRY_EX: u32 = 0x0007_00A0;
        let mut output = [0u64; 8];
        let mut returned = 0u32;
        let ok = unsafe {
            DeviceIoControl(
                self.0, IOCTL_DISK_GET_DRIVE_GEOMETRY_EX,
                std::ptr::null(), 0, output.as_mut_ptr() as *mut std::ffi::c_void,
                std::mem::size_of_val(&output) as u32, &mut returned, std::ptr::null_mut(),
            )
        };
        if ok == 0 || returned < 32 {
            return Err("Windows could not verify the attached result disk geometry.".into());
        }
        let geometry = unsafe { &*(output.as_ptr() as *const FridayDiskGeometryEx) };
        if geometry.disk_size < 0
            || geometry.disk_size as u64 != GUEST_RESULT_VOLUME_CAPACITY_BYTES
            || geometry.geometry.bytes_per_sector as u64 != GUEST_RESULT_GPT_SECTOR_BYTES
        {
            return Err("The attached physical device does not match FRIDAY's exact 64 MiB / 512-byte disk policy.".into());
        }
        Ok(())
    }

    fn read_exact_at(&self, offset: u64, buffer: &mut [u8]) -> Result<(), String> {
        if buffer.is_empty() || buffer.len() > u32::MAX as usize || offset > i64::MAX as u64 {
            return Err("A raw result-disk read exceeded its fixed bounds.".into());
        }
        if unsafe { SetFilePointerEx(self.0, offset as i64, std::ptr::null_mut(), 0) } == 0 {
            return Err("Windows could not position the result disk for verification.".into());
        }
        let mut read = 0u32;
        if unsafe { ReadFile(self.0, buffer.as_mut_ptr(), buffer.len() as u32, &mut read, std::ptr::null_mut()) } == 0
            || read as usize != buffer.len()
        {
            return Err("Windows returned an incomplete raw result-disk read.".into());
        }
        Ok(())
    }

    fn write_all_at(&self, offset: u64, buffer: &[u8]) -> Result<(), String> {
        if buffer.is_empty() || buffer.len() > u32::MAX as usize || offset > i64::MAX as u64 {
            return Err("A raw result-disk write exceeded its fixed bounds.".into());
        }
        if unsafe { SetFilePointerEx(self.0, offset as i64, std::ptr::null_mut(), 0) } == 0 {
            return Err("Windows could not position the result disk for a bounded write.".into());
        }
        let mut written = 0u32;
        if unsafe { WriteFile(self.0, buffer.as_ptr(), buffer.len() as u32, &mut written, std::ptr::null_mut()) } == 0
            || written as usize != buffer.len()
        {
            return Err("Windows returned an incomplete raw result-disk write; the disk must be preserved for recovery.".into());
        }
        Ok(())
    }

    fn flush(&self) -> Result<(), String> {
        if unsafe { FlushFileBuffers(self.0) } == 0 {
            return Err("Windows could not flush result-disk metadata; the disk must be preserved for recovery.".into());
        }
        Ok(())
    }
}

#[cfg(windows)]
fn verify_zero_region(disk: &OwnedRawGuestDisk, first_lba: u64, sector_count: u64) -> Result<(), String> {
    let total_bytes = sector_count.checked_mul(GUEST_RESULT_GPT_SECTOR_BYTES)
        .ok_or_else(|| "The GPT blank-region size overflowed.".to_string())?;
    if total_bytes > GUEST_RESULT_VOLUME_CAPACITY_BYTES || total_bytes > 64 * 1024 {
        return Err("The GPT blank-region check exceeded its fixed bounded read size.".into());
    }
    let mut bytes = vec![0u8; total_bytes as usize];
    disk.read_exact_at(first_lba * GUEST_RESULT_GPT_SECTOR_BYTES, &mut bytes)?;
    if bytes.iter().any(|byte| *byte != 0) {
        return Err("The result disk already contains partition metadata or data; initialization was refused.".into());
    }
    Ok(())
}


const FRIDAY_DRIVE_LAYOUT_EX_HEADER_BYTES: usize = 48;
const FRIDAY_PARTITION_INFORMATION_EX_BYTES: usize = 144;

/// Validates the layout reported by Windows' storage stack, independently of
/// the raw GPT-sector comparison. The byte offsets follow the documented
/// DRIVE_LAYOUT_INFORMATION_EX / PARTITION_INFORMATION_EX ABI.
fn verify_guest_result_windows_partition_layout_bytes(
    bytes: &[u8],
    returned_bytes: usize,
    plan: &GuestResultPartitionPlan,
    metadata: &GuestResultGptMetadata,
) -> Result<(), String> {
    let required = FRIDAY_DRIVE_LAYOUT_EX_HEADER_BYTES
        .checked_add(FRIDAY_PARTITION_INFORMATION_EX_BYTES)
        .ok_or_else(|| "The Windows partition-layout size overflowed.".to_string())?;
    if returned_bytes > bytes.len() || returned_bytes < required {
        return Err("Windows returned a truncated drive-layout structure.".into());
    }
    let data = &bytes[..returned_bytes];
    let read_u32 = |offset: usize| -> Result<u32, String> {
        let raw: [u8; 4] = data.get(offset..offset + 4)
            .ok_or_else(|| "Windows returned a truncated partition-layout field.".to_string())?
            .try_into().map_err(|_| "Windows returned an invalid partition-layout field.".to_string())?;
        Ok(u32::from_le_bytes(raw))
    };
    let read_u64 = |offset: usize| -> Result<u64, String> {
        let raw: [u8; 8] = data.get(offset..offset + 8)
            .ok_or_else(|| "Windows returned a truncated partition-layout field.".to_string())?
            .try_into().map_err(|_| "Windows returned an invalid partition-layout field.".to_string())?;
        Ok(u64::from_le_bytes(raw))
    };
    if read_u32(0)? != 1 || read_u32(4)? != 1 {
        return Err("Windows does not recognize exactly one GPT partition on the result disk.".into());
    }
    if data[8..24] != metadata.primary_header[56..72] {
        return Err("Windows reported a GPT disk identifier different from FRIDAY's recorded layout.".into());
    }
    let expected_usable_start = 34u64.checked_mul(GUEST_RESULT_GPT_SECTOR_BYTES)
        .ok_or_else(|| "The GPT usable-range calculation overflowed.".to_string())?;
    let total_sectors = plan.disk_capacity_bytes / GUEST_RESULT_GPT_SECTOR_BYTES;
    let expected_usable_length = (total_sectors - 68)
        .checked_mul(GUEST_RESULT_GPT_SECTOR_BYTES)
        .ok_or_else(|| "The GPT usable-range calculation overflowed.".to_string())?;
    if read_u64(24)? != expected_usable_start || read_u64(32)? != expected_usable_length || read_u32(40)? < 1 {
        return Err("Windows reported an unexpected GPT usable range or partition capacity.".into());
    }

    let entry = FRIDAY_DRIVE_LAYOUT_EX_HEADER_BYTES;
    if read_u32(entry)? != 1
        || read_u64(entry + 8)? != plan.first_lba * GUEST_RESULT_GPT_SECTOR_BYTES
        || read_u64(entry + 16)? != (plan.last_lba - plan.first_lba + 1) * GUEST_RESULT_GPT_SECTOR_BYTES
        || read_u32(entry + 24)? != 1
        || data[entry + 28] != 0
        || data[entry + 32..entry + 48] != metadata.primary_entries[0..16]
        || data[entry + 48..entry + 64] != metadata.primary_entries[16..32]
        || read_u64(entry + 64)? != 0
    {
        return Err("Windows' partition entry differs from FRIDAY's exact partition type, identity, bounds, or attributes.".into());
    }
    let mut expected_name = [0u8; 72];
    for (index, unit) in "FRIDAY_RESULTS".encode_utf16().enumerate() {
        expected_name[index * 2..index * 2 + 2].copy_from_slice(&unit.to_le_bytes());
    }
    if data[entry + 72..entry + 144] != expected_name {
        return Err("Windows reported an unexpected result-partition label.".into());
    }
    Ok(())
}

fn verify_gpt_header_crc(header: &[u8]) -> bool {
    if header.len() < GUEST_RESULT_GPT_HEADER_BYTES {
        return false;
    }
    let stored = u32::from_le_bytes(match header[16..20].try_into() {
        Ok(value) => value,
        Err(_) => return false,
    });
    let mut crc_input = header[..GUEST_RESULT_GPT_HEADER_BYTES].to_vec();
    crc_input[16..20].fill(0);
    gpt_crc32(&crc_input) == stored
}

#[cfg(windows)]
fn verify_guest_result_gpt_readback(
    disk: &OwnedRawGuestDisk,
    plan: &GuestResultPartitionPlan,
    expected: &GuestResultGptMetadata,
) -> Result<(), String> {
    let total_sectors = plan.disk_capacity_bytes / GUEST_RESULT_GPT_SECTOR_BYTES;
    let backup_entries_lba = total_sectors - 1 - (GUEST_RESULT_GPT_ARRAY_BYTES as u64 / GUEST_RESULT_GPT_SECTOR_BYTES);
    let mut mbr = vec![0u8; 512];
    let mut primary_header = vec![0u8; 512];
    let mut primary_entries = vec![0u8; GUEST_RESULT_GPT_ARRAY_BYTES];
    let mut backup_entries = vec![0u8; GUEST_RESULT_GPT_ARRAY_BYTES];
    let mut backup_header = vec![0u8; 512];
    disk.read_exact_at(0, &mut mbr)?;
    disk.read_exact_at(512, &mut primary_header)?;
    disk.read_exact_at(2 * 512, &mut primary_entries)?;
    disk.read_exact_at(backup_entries_lba * 512, &mut backup_entries)?;
    disk.read_exact_at((total_sectors - 1) * 512, &mut backup_header)?;
    if mbr != expected.protective_mbr
        || primary_header != expected.primary_header
        || primary_entries != expected.primary_entries
        || backup_entries != expected.backup_entries
        || backup_header != expected.backup_header
        || gpt_crc32(&primary_entries) != u32::from_le_bytes(primary_header[88..92].try_into().unwrap())
        || gpt_crc32(&backup_entries) != u32::from_le_bytes(backup_header[88..92].try_into().unwrap())
        || !verify_gpt_header_crc(&primary_header)
        || !verify_gpt_header_crc(&backup_header)
    {
        return Err("The GPT metadata read-back differs from FRIDAY's exact layout or CRC policy.".into());
    }
    Ok(())
}

/// Initializes only the verified FRIDAY-owned attached VHDX. It refuses any
/// nonblank metadata region, journals intent before the first write, writes
/// backup structures before primary structures, flushes each phase, and
/// independently reads back every GPT structure before recording success.
#[cfg(windows)]
fn initialize_guest_result_gpt(
    attached: &OwnedAttachedGuestVhdx,
    plan: &GuestResultVolumePlan,
    trusted_storage_root: &Path,
    journal_root: &Path,
    current_journal: &GuestResultStorageJournal,
) -> Result<GuestResultStorageJournal, String> {
    validate_guest_result_volume_plan(plan, trusted_storage_root, &plan.run_id)?;
    let partition_plan = build_guest_result_partition_plan(
        &plan.run_id, plan.capacity_bytes, GUEST_RESULT_GPT_SECTOR_BYTES as u32,
    )?;
    let metadata = build_guest_result_gpt_metadata(&partition_plan)?;
    let identifier = verify_guest_result_vhdx_identity(plan, trusted_storage_root)?;
    if current_journal.run_id != plan.run_id || current_journal.vhdx_identifier != identifier
        || validate_guest_result_storage_journal(current_journal)? != GuestResultStorageState::Attached
    {
        return Err("The GPT initialization journal is not bound to this attached FRIDAY result disk.".into());
    }
    let disk = OwnedRawGuestDisk::open_attached_device(attached)?;
    verify_zero_region(&disk, 0, 34)?;
    let total_sectors = plan.capacity_bytes / GUEST_RESULT_GPT_SECTOR_BYTES;
    let backup_entries_lba = total_sectors - 1 - (GUEST_RESULT_GPT_ARRAY_BYTES as u64 / GUEST_RESULT_GPT_SECTOR_BYTES);
    verify_zero_region(&disk, backup_entries_lba, 33)?;

    let in_progress = append_and_persist_guest_result_storage_transition(
        journal_root, current_journal, GuestResultStorageEvent::BeginGptInitialization, true, false,
    )?;
    let write_result = (|| -> Result<(), String> {
        disk.write_all_at(backup_entries_lba * 512, &metadata.backup_entries)?;
        disk.write_all_at((total_sectors - 1) * 512, &metadata.backup_header)?;
        disk.flush()?;
        disk.write_all_at(2 * 512, &metadata.primary_entries)?;
        disk.write_all_at(512, &metadata.primary_header)?;
        disk.flush()?;
        disk.write_all_at(0, &metadata.protective_mbr)?;
        disk.flush()?;
        verify_guest_result_gpt_readback(&disk, &partition_plan, &metadata)?;
        disk.verify_fixed_geometry()?;
        disk.verify_windows_partition_layout(&partition_plan, &metadata)?;
        if guest_vhdx_physical_path(attached.handle.0)? != attached.physical_path {
            return Err("The attached physical-device identity changed after GPT initialization.".into());
        }
        if verify_attached_guest_result_vhdx_identity(
            &attached.handle, &attached.physical_path, plan, trusted_storage_root,
        )? != identifier {
            return Err("The attached VHDX persistent identity changed after GPT initialization.".into());
        }
        Ok(())
    })();
    if let Err(error) = write_result {
        let _ = append_and_persist_guest_result_storage_transition(
            journal_root, &in_progress, GuestResultStorageEvent::PreserveForRecovery, false, false,
        );
        return Err(format!("{error} The result disk has been preserved for recovery; no automatic retry will occur."));
    }
    append_and_persist_guest_result_storage_transition(
        journal_root, &in_progress, GuestResultStorageEvent::CompleteGptInitialization, true, false,
    )
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
    fn gpt_metadata_contains_matching_primary_backup_tables_and_crc_values() {
        let run_id = "0123456789abcdef0123456789abcdef";
        let plan = build_guest_result_partition_plan(
            run_id, GUEST_RESULT_VOLUME_CAPACITY_BYTES, 512,
        ).unwrap();
        let metadata = build_guest_result_gpt_metadata(&plan).unwrap();
        assert_eq!(&metadata.protective_mbr[510..512], &[0x55, 0xaa]);
        assert_eq!(&metadata.primary_header[0..8], b"EFI PART");
        assert_eq!(&metadata.backup_header[0..8], b"EFI PART");
        assert_eq!(metadata.primary_entries, metadata.backup_entries);
        assert_eq!(
            gpt_crc32(&metadata.primary_entries),
            u32::from_le_bytes(metadata.primary_header[88..92].try_into().unwrap()),
        );
        assert_eq!(
            gpt_crc32(&metadata.backup_entries),
            u32::from_le_bytes(metadata.backup_header[88..92].try_into().unwrap()),
        );
        assert_eq!(
            u32::from_le_bytes(metadata.primary_header[16..20].try_into().unwrap()),
            gpt_crc32(&{
                let mut header = metadata.primary_header[..GUEST_RESULT_GPT_HEADER_BYTES].to_vec();
                header[16..20].fill(0);
                header
            }),
        );
        assert_eq!(
            u32::from_le_bytes(metadata.backup_header[16..20].try_into().unwrap()),
            gpt_crc32(&{
                let mut header = metadata.backup_header[..GUEST_RESULT_GPT_HEADER_BYTES].to_vec();
                header[16..20].fill(0);
                header
            }),
        );
        assert_ne!(metadata.disk_guid, plan.partition_unique_guid);
        assert_eq!(
            u64::from_le_bytes(metadata.primary_header[24..32].try_into().unwrap()), 1,
        );
        assert_eq!(
            u64::from_le_bytes(metadata.backup_header[24..32].try_into().unwrap()),
            GUEST_RESULT_VOLUME_CAPACITY_BYTES / 512 - 1,
        );
    }

    #[test]
    fn windows_partition_layout_parser_accepts_only_the_exact_single_gpt_partition() {
        let run_id = "0123456789abcdef0123456789abcdef";
        let plan = build_guest_result_partition_plan(run_id, GUEST_RESULT_VOLUME_CAPACITY_BYTES, 512).unwrap();
        let metadata = build_guest_result_gpt_metadata(&plan).unwrap();
        let mut bytes = vec![0u8; FRIDAY_DRIVE_LAYOUT_EX_HEADER_BYTES + FRIDAY_PARTITION_INFORMATION_EX_BYTES];
        bytes[0..4].copy_from_slice(&1u32.to_le_bytes());
        bytes[4..8].copy_from_slice(&1u32.to_le_bytes());
        bytes[8..24].copy_from_slice(&metadata.primary_header[56..72]);
        bytes[24..32].copy_from_slice(&(34u64 * 512).to_le_bytes());
        let total_sectors = plan.disk_capacity_bytes / 512;
        bytes[32..40].copy_from_slice(&((total_sectors - 68) * 512).to_le_bytes());
        bytes[40..44].copy_from_slice(&128u32.to_le_bytes());
        let entry = FRIDAY_DRIVE_LAYOUT_EX_HEADER_BYTES;
        bytes[entry..entry + 4].copy_from_slice(&1u32.to_le_bytes());
        bytes[entry + 8..entry + 16].copy_from_slice(&(plan.first_lba * 512).to_le_bytes());
        bytes[entry + 16..entry + 24].copy_from_slice(&((plan.last_lba - plan.first_lba + 1) * 512).to_le_bytes());
        bytes[entry + 24..entry + 28].copy_from_slice(&1u32.to_le_bytes());
        bytes[entry + 32..entry + 48].copy_from_slice(&metadata.primary_entries[0..16]);
        bytes[entry + 48..entry + 64].copy_from_slice(&metadata.primary_entries[16..32]);
        for (index, unit) in "FRIDAY_RESULTS".encode_utf16().enumerate() {
            bytes[entry + 72 + index * 2..entry + 74 + index * 2].copy_from_slice(&unit.to_le_bytes());
        }
        assert!(verify_guest_result_windows_partition_layout_bytes(&bytes, bytes.len(), &plan, &metadata).is_ok());

        let mut altered = bytes.clone();
        altered[4..8].copy_from_slice(&2u32.to_le_bytes());
        assert!(verify_guest_result_windows_partition_layout_bytes(&altered, altered.len(), &plan, &metadata).is_err());
        let mut altered = bytes.clone();
        altered[entry + 8..entry + 16].copy_from_slice(&0u64.to_le_bytes());
        assert!(verify_guest_result_windows_partition_layout_bytes(&altered, altered.len(), &plan, &metadata).is_err());
        let mut altered = bytes.clone();
        altered[entry + 96] ^= 0x80;
        assert!(verify_guest_result_windows_partition_layout_bytes(&altered, altered.len(), &plan, &metadata).is_err());
        assert!(verify_guest_result_windows_partition_layout_bytes(&bytes, 80, &plan, &metadata).is_err());
    }

    #[test]
    fn gpt_lifecycle_requires_journaled_intent_before_completion() {
        use GuestResultStorageEvent as E;
        use GuestResultStorageState as S;
        assert!(advance_guest_result_storage_state(S::Attached, E::CompleteGptInitialization, true, false).is_err());
        assert_eq!(
            advance_guest_result_storage_state(S::Attached, E::BeginGptInitialization, true, false).unwrap(),
            S::GptWriteInProgress,
        );
        assert_eq!(
            advance_guest_result_storage_state(S::GptWriteInProgress, E::CompleteGptInitialization, true, false).unwrap(),
            S::Partitioned,
        );
        assert!(advance_guest_result_storage_state(S::GptWriteInProgress, E::MountVerified, true, false).is_err());
        assert_eq!(
            advance_guest_result_storage_state(S::GptWriteInProgress, E::PreserveForRecovery, false, false).unwrap(),
            S::RecoveryRequired,
        );
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
    fn private_workspace_permissions_are_restricted_before_use() {
        let workspace = make_test_directory("private-workspace-permissions");
        secure_private_workspace_directory(&workspace).unwrap();
        let metadata = fs::symlink_metadata(&workspace).unwrap();
        assert!(metadata.is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(metadata.permissions().mode() & 0o077, 0);
        }
        fs::remove_dir(&workspace).unwrap();
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
        assert!(config.contains("<AudioInput>Disable</AudioInput>"));
        assert!(config.contains("<VideoInput>Disable</VideoInput>"));
        assert!(config.contains("<ClipboardRedirection>Disable</ClipboardRedirection>"));
        assert!(config.contains("<PrinterRedirection>Disable</PrinterRedirection>"));
        assert!(config.contains("<ProtectedClient>Enable</ProtectedClient>"));
        assert!(config.contains("<ReadOnly>true</ReadOnly>"));
        validate_sandbox_config_policy(&config).unwrap();
        assert!(config.contains(r"<SandboxFolder>C:\FRIDAY\Project</SandboxFolder>"));
        assert!(config.contains(canonical.to_str().unwrap()));
        fs::remove_dir_all(base).unwrap();
    }

    fn safe_sandbox_config_fixture(host_folder: &str) -> String {
        format!(r#"<Configuration>
  <VGpu>Disable</VGpu>
  <Networking>Disable</Networking>
  <AudioInput>Disable</AudioInput>
  <VideoInput>Disable</VideoInput>
  <ClipboardRedirection>Disable</ClipboardRedirection>
  <PrinterRedirection>Disable</PrinterRedirection>
  <ProtectedClient>Enable</ProtectedClient>
  <MappedFolders>
    <MappedFolder>
      <HostFolder>{}</HostFolder>
      <SandboxFolder>C:\FRIDAY\Project</SandboxFolder>
      <ReadOnly>true</ReadOnly>
    </MappedFolder>
  </MappedFolders>
</Configuration>
"#, xml_escape(host_folder))
    }

    #[test]
    fn sandbox_config_policy_rejects_network_or_writable_share_regressions() {
        let safe = safe_sandbox_config_fixture(r"C:\FRIDAY\Snapshot");
        validate_sandbox_config_policy(&safe).unwrap();
        for (from, to) in [
            ("<Networking>Disable</Networking>", "<Networking>Enable</Networking>"),
            ("<ReadOnly>true</ReadOnly>", "<ReadOnly>false</ReadOnly>"),
            ("<ProtectedClient>Enable</ProtectedClient>", "<ProtectedClient>Disable</ProtectedClient>"),
        ] {
            assert!(validate_sandbox_config_policy(&safe.replace(from, to)).is_err());
        }
        assert!(validate_sandbox_config_policy(&safe.replace(
            "</MappedFolders>", "<MappedFolder></MappedFolder></MappedFolders>"
        )).is_err());
    }

    #[test]
    fn sandbox_config_policy_rejects_any_non_template_xml_structure() {
        let safe = safe_sandbox_config_fixture(r"C:\FRIDAY\Snapshot");
        for mutation in [
            safe.replace("<Networking>", "<Networking enabled='true'>"),
            safe.replace("</Configuration>", "<VGpu>Enable</VGpu></Configuration>"),
            safe.replace("</Configuration>", "<LogonCommand/></Configuration>"),
            safe.replace("</Configuration>", "<!-- injected --></Configuration>"),
            safe.replace("<HostFolder>", "<HostFolder><![CDATA["),
            safe.replace("</HostFolder>", "]]></HostFolder>"),
            safe.replace("<ReadOnly>true</ReadOnly>", "<ReadOnly>true</ReadOnly><ReadOnly>false</ReadOnly>"),
            safe.replace("<VGpu>Disable</VGpu>", "<Unknown>value</Unknown><VGpu>Disable</VGpu>"),
        ] {
            assert!(validate_sandbox_config_policy(&mutation).is_err(),
                "non-template XML structure was accepted");
        }
    }

    #[test]
    fn sandbox_config_policy_accepts_escaped_path_text_and_rejects_bad_entities() {
        let safe = safe_sandbox_config_fixture(r#"C:\FRIDAY\Work & <Finance> "Q1""#);
        validate_sandbox_config_policy(&safe).unwrap();
        let bad_entity = safe.replace("&amp;", "&bogus;");
        assert!(validate_sandbox_config_policy(&bad_entity).is_err());
        let raw_ampersand = safe.replace("&amp;", "&");
        assert!(validate_sandbox_config_policy(&raw_ampersand).is_err());
        let raw_tag = safe.replace("&lt;Finance&gt;", "<Networking>Enable</Networking>");
        assert!(validate_sandbox_config_policy(&raw_tag).is_err());
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


    fn valid_result_storage_journal() -> GuestResultStorageJournal {
        let journal = GuestResultStorageJournal {
            schema_version: GUEST_RESULT_STORAGE_JOURNAL_SCHEMA,
            run_id: "0123456789abcdef0123456789abcdef".into(),
            vhdx_identifier: "12345678-1234-5678-9abc-def012345678".into(),
            entries: Vec::new(),
        };
        append_guest_result_storage_journal_entry(
            &journal, GuestResultStorageEvent::CreateVerified, true, false,
        ).unwrap()
    }

    #[test]
    fn result_storage_journal_round_trips_and_preserves_recovery_state() {
        let journal = append_guest_result_storage_journal_entry(
            &valid_result_storage_journal(),
            GuestResultStorageEvent::PreserveForRecovery, true, false,
        ).unwrap();
        let bytes = encode_guest_result_storage_journal(&journal).unwrap();
        let parsed = parse_guest_result_storage_journal(&bytes).unwrap();
        assert_eq!(parsed, journal);
        assert_eq!(validate_guest_result_storage_journal(&parsed).unwrap(), GuestResultStorageState::RecoveryRequired);
        assert!(append_guest_result_storage_journal_entry(
            &parsed, GuestResultStorageEvent::DeleteVerified, true, true,
        ).is_err());
    }

    #[test]
    fn result_storage_journal_rejects_reordering_and_forged_transitions() {
        let mut journal = valid_result_storage_journal();
        journal.entries[0].sequence = 9;
        assert!(validate_guest_result_storage_journal(&journal).is_err());
        let mut journal = valid_result_storage_journal();
        journal.entries[0].next_state = GuestResultStorageState::Removed;
        assert!(validate_guest_result_storage_journal(&journal).is_err());
        let mut journal = valid_result_storage_journal();
        journal.run_id = "../foreign-run".into();
        assert!(validate_guest_result_storage_journal(&journal).is_err());
    }

    #[test]
    fn result_storage_journal_persists_immutable_generations_and_recovers_latest() {
        let root = make_test_directory("result-journal-durable");
        let first = valid_result_storage_journal();
        persist_guest_result_storage_journal(&root, &first).unwrap();
        let second = append_guest_result_storage_journal_entry(
            &first, GuestResultStorageEvent::PreserveForRecovery, true, false,
        ).unwrap();
        persist_guest_result_storage_journal(&root, &second).unwrap();
        let loaded = load_guest_result_storage_journal(
            &root, &first.run_id, &first.vhdx_identifier,
        ).unwrap();
        assert_eq!(loaded, second);
        assert!(persist_guest_result_storage_journal(&root, &second).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn result_storage_journal_preserves_interrupted_generation_for_recovery() {
        let root = make_test_directory("result-journal-interrupted");
        let first = valid_result_storage_journal();
        persist_guest_result_storage_journal(&root, &first).unwrap();
        let interrupted = guest_result_storage_journal_generation_path(&root, 2);
        fs::create_dir(&interrupted).unwrap();
        secure_private_workspace_directory(&interrupted).unwrap();
        assert!(load_guest_result_storage_journal(
            &root, &first.run_id, &first.vhdx_identifier,
        ).is_err());
        assert!(interrupted.exists(), "incomplete journal evidence must be preserved");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn result_storage_journal_rejects_generation_gaps_and_identity_substitution() {
        let root = make_test_directory("result-journal-gap");
        let first = valid_result_storage_journal();
        persist_guest_result_storage_journal(&root, &first).unwrap();
        let later = guest_result_storage_journal_generation_path(&root, 3);
        fs::create_dir(&later).unwrap();
        secure_private_workspace_directory(&later).unwrap();
        assert!(load_guest_result_storage_journal(
            &root, &first.run_id, &first.vhdx_identifier,
        ).is_err());
        assert!(load_guest_result_storage_journal(
            &root, "fedcba9876543210fedcba9876543210", &first.vhdx_identifier,
        ).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn result_storage_journal_appends_only_from_latest_persisted_state() {
        let root = make_test_directory("result-journal-append-gateway");
        let empty = GuestResultStorageJournal {
            schema_version: GUEST_RESULT_STORAGE_JOURNAL_SCHEMA,
            run_id: "0123456789abcdef0123456789abcdef".into(),
            vhdx_identifier: "12345678-1234-5678-9abc-def012345678".into(),
            entries: Vec::new(),
        };
        let created = append_and_persist_guest_result_storage_transition(
            &root, &empty, GuestResultStorageEvent::CreateVerified, true, false,
        ).unwrap();
        assert_eq!(validate_guest_result_storage_journal(&created).unwrap(), GuestResultStorageState::Created);
        let attached = append_and_persist_guest_result_storage_transition(
            &root, &created, GuestResultStorageEvent::AttachVerified, true, false,
        ).unwrap();
        assert_eq!(validate_guest_result_storage_journal(&attached).unwrap(), GuestResultStorageState::Attached);
        assert_eq!(load_guest_result_storage_journal(
            &root, &attached.run_id, &attached.vhdx_identifier,
        ).unwrap(), attached);
        assert!(append_and_persist_guest_result_storage_transition(
            &root, &created, GuestResultStorageEvent::MountVerified, true, false,
        ).is_err(), "a stale branch must not append over a newer persisted generation");
        assert!(append_and_persist_guest_result_storage_transition(
            &root, &attached, GuestResultStorageEvent::GrantShare, false, false,
        ).is_err(), "unverified ownership must not be journaled as a successful transition");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn result_storage_journal_append_gateway_refuses_preexisting_incomplete_generation() {
        let root = make_test_directory("result-journal-append-incomplete");
        let empty = GuestResultStorageJournal {
            schema_version: GUEST_RESULT_STORAGE_JOURNAL_SCHEMA,
            run_id: "0123456789abcdef0123456789abcdef".into(),
            vhdx_identifier: "12345678-1234-5678-9abc-def012345678".into(),
            entries: Vec::new(),
        };
        let interrupted = guest_result_storage_journal_generation_path(&root, 1);
        fs::create_dir(&interrupted).unwrap();
        secure_private_workspace_directory(&interrupted).unwrap();
        assert!(append_and_persist_guest_result_storage_transition(
            &root, &empty, GuestResultStorageEvent::CreateVerified, true, false,
        ).is_err());
        assert!(interrupted.exists(), "uncertain recovery evidence must be preserved");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn result_storage_journal_rejects_oversized_and_unknown_field_documents() {
        assert!(parse_guest_result_storage_journal(
            &vec![b'x'; MAX_GUEST_RESULT_STORAGE_JOURNAL_BYTES + 1]
        ).is_err());
        let mut bytes = encode_guest_result_storage_journal(&valid_result_storage_journal()).unwrap();
        bytes.pop();
        bytes.extend_from_slice(br#","unexpected":true}"#);
        assert!(parse_guest_result_storage_journal(&bytes).is_err());
    }

    #[test]
    fn result_storage_lifecycle_accepts_only_verified_ordered_happy_path() {
        use GuestResultStorageEvent as E;
        use GuestResultStorageState as S;
        let mut state = S::Planned;
        for event in [E::CreateVerified, E::AttachVerified, E::BeginGptInitialization, E::CompleteGptInitialization, E::MountVerified, E::GrantShare,
            E::ConfirmGuestStopped, E::RevokeShare, E::DetachVerified, E::DeleteVerified] {
            state = advance_guest_result_storage_state(state, event, true, event == E::ConfirmGuestStopped)
                .expect("valid lifecycle transition should succeed");
        }
        assert_eq!(state, S::Removed);
    }

    #[test]
    fn result_storage_lifecycle_refuses_cleanup_before_guest_stop_and_share_revocation() {
        use GuestResultStorageEvent as E;
        use GuestResultStorageState as S;
        assert!(advance_guest_result_storage_state(S::Shared, E::RevokeShare, true, true).is_err());
        assert!(advance_guest_result_storage_state(S::Shared, E::DetachVerified, true, true).is_err());
        assert!(advance_guest_result_storage_state(S::Mounted, E::DeleteVerified, true, true).is_err());
    }

    #[test]
    fn result_storage_lifecycle_requires_verified_ownership_for_every_action() {
        use GuestResultStorageEvent as E;
        use GuestResultStorageState as S;
        assert!(advance_guest_result_storage_state(S::Planned, E::CreateVerified, false, false).is_err());
        assert!(advance_guest_result_storage_state(S::Detached, E::DeleteVerified, false, true).is_err());
    }

    #[test]
    fn result_storage_lifecycle_requires_positive_guest_stop_evidence() {
        use GuestResultStorageEvent as E;
        use GuestResultStorageState as S;
        assert!(advance_guest_result_storage_state(S::Shared, E::ConfirmGuestStopped, true, false).is_err());
        assert_eq!(advance_guest_result_storage_state(S::Shared, E::ConfirmGuestStopped, true, true).unwrap(), S::GuestStopped);
    }

    #[test]
    fn result_storage_recovery_state_preserves_resources_and_blocks_automatic_actions() {
        use GuestResultStorageEvent as E;
        use GuestResultStorageState as S;
        assert_eq!(advance_guest_result_storage_state(S::Shared, E::PreserveForRecovery, false, false).unwrap(), S::RecoveryRequired);
        assert!(advance_guest_result_storage_state(S::RecoveryRequired, E::RevokeShare, true, true).is_err());
        assert!(advance_guest_result_storage_state(S::Removed, E::PreserveForRecovery, true, true).is_err());
    }

    fn valid_test_sandbox_identity() -> SandboxProcessIdentity {
        SandboxProcessIdentity {
            session_id: "0123456789abcdef0123456789abcdef".into(),
            process_id: 4321,
            process_creation_time: 987654321,
            image_path: r"C:\\Windows\\System32\\WindowsSandbox.exe".into(),
        }
    }

    #[test]
    fn sandbox_session_accepts_only_exact_owned_lifecycle() {
        use SandboxSessionEvent as E;
        use SandboxSessionState as S;
        let identity = valid_test_sandbox_identity();
        let mut state = S::Planned;
        state = advance_sandbox_session(state, E::RequestLaunch, None, None, false, false).unwrap();
        state = advance_sandbox_session(state, E::ObserveProcess, None, Some(&identity), false, false).unwrap();
        state = advance_sandbox_session(state, E::ConfirmSession, Some(&identity), Some(&identity), true, false).unwrap();
        state = advance_sandbox_session(state, E::ConfirmRunning, Some(&identity), Some(&identity), true, false).unwrap();
        state = advance_sandbox_session(state, E::RequestStop, Some(&identity), Some(&identity), true, false).unwrap();
        state = advance_sandbox_session(state, E::ConfirmStopped, Some(&identity), Some(&identity), true, true).unwrap();
        state = advance_sandbox_session(state, E::RemoveOwnershipRecord, Some(&identity), None, true, true).unwrap();
        assert_eq!(state, S::Removed);
    }

    #[test]
    fn sandbox_session_rejects_invalid_process_identity_and_image_path() {
        let mut identity = valid_test_sandbox_identity();
        assert!(validate_sandbox_process_identity(&identity).is_ok());
        identity.process_id = 0;
        assert!(validate_sandbox_process_identity(&identity).is_err());
        identity.process_id = 4321;
        identity.process_creation_time = 0;
        assert!(validate_sandbox_process_identity(&identity).is_err());
        identity.process_creation_time = 1;
        identity.image_path = r"C:\\Windows\\System32\\cmd.exe".into();
        assert!(validate_sandbox_process_identity(&identity).is_err());
        identity.image_path = r"C:\\Windows\\..\\Other\\WindowsSandbox.exe".into();
        assert!(validate_sandbox_process_identity(&identity).is_err());
    }

    #[test]
    fn sandbox_session_rejects_pid_reuse_and_foreign_process_substitution() {
        use SandboxSessionEvent as E;
        use SandboxSessionState as S;
        let identity = valid_test_sandbox_identity();
        let mut replacement = identity.clone();
        replacement.process_creation_time += 1;
        assert!(advance_sandbox_session(S::ProcessObserved, E::ConfirmSession,
            Some(&identity), Some(&replacement), true, false).is_err());
        assert!(advance_sandbox_session(S::Running, E::RequestStop,
            Some(&identity), Some(&replacement), true, false).is_err());
        replacement = identity.clone();
        replacement.session_id = "fedcba9876543210fedcba9876543210".into();
        assert!(advance_sandbox_session(S::Running, E::RequestStop,
            Some(&identity), Some(&replacement), true, false).is_err());
    }

    #[test]
    fn sandbox_session_refuses_stop_without_ownership_or_exit_evidence() {
        use SandboxSessionEvent as E;
        use SandboxSessionState as S;
        let identity = valid_test_sandbox_identity();
        assert!(advance_sandbox_session(S::Running, E::RequestStop,
            Some(&identity), Some(&identity), false, false).is_err());
        assert!(advance_sandbox_session(S::StopRequested, E::ConfirmStopped,
            Some(&identity), Some(&identity), true, false).is_err());
        assert!(advance_sandbox_session(S::StopRequested, E::ConfirmStopped,
            Some(&identity), None, true, true).is_err());
    }

    #[test]
    fn sandbox_session_recovery_state_blocks_all_automatic_actions() {
        use SandboxSessionEvent as E;
        use SandboxSessionState as S;
        let identity = valid_test_sandbox_identity();
        assert_eq!(advance_sandbox_session(S::Running, E::PreserveForRecovery,
            Some(&identity), Some(&identity), false, false).unwrap(), S::RecoveryRequired);
        assert!(advance_sandbox_session(S::RecoveryRequired, E::RequestStop,
            Some(&identity), Some(&identity), true, true).is_err());
        assert!(advance_sandbox_session(S::Removed, E::PreserveForRecovery,
            Some(&identity), Some(&identity), true, true).is_err());
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

    #[test]
    fn result_volume_plan_uses_fixed_capacity_and_guest_mount() {
        let root = make_test_directory("result-volume-plan");
        let run_id = "0123456789abcdef0123456789abcdef";
        let plan = build_guest_result_volume_plan(&root, run_id).unwrap();
        assert_eq!(plan.run_id, run_id);
        assert_eq!(plan.storage_root, fs::canonicalize(&root).unwrap());
        assert_eq!(plan.run_directory, plan.storage_root.join(run_id));
        assert_eq!(plan.vhdx_path, plan.run_directory.join("result.vhdx"));
        assert_eq!(plan.capacity_bytes, 64 * 1024 * 1024);
        assert_eq!(plan.max_result_bytes, MAX_GUEST_RESULT_BYTES as u64);
        assert_eq!(plan.guest_mount_path, r"C:\FRIDAY\Results");
        validate_guest_result_volume_plan(&plan, &root, run_id).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn result_volume_plan_rejects_untrusted_paths_and_reused_run_directories() {
        let root = make_test_directory("result-volume-invalid");
        assert!(build_guest_result_volume_plan(&root, "../outside").is_err());
        assert!(build_guest_result_volume_plan(Path::new("relative"), "0123456789abcdef0123456789abcdef").is_err());
        let run_id = "0123456789abcdef0123456789abcdef";
        fs::create_dir(root.join(run_id)).unwrap();
        assert!(build_guest_result_volume_plan(&root, run_id).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn result_volume_plan_rejects_mutated_capacity_paths_and_guest_mount() {
        let root = make_test_directory("result-volume-tamper");
        let run_id = "0123456789abcdef0123456789abcdef";
        let plan = build_guest_result_volume_plan(&root, run_id).unwrap();
        let mut changed = plan.clone();
        changed.capacity_bytes *= 2;
        assert!(validate_guest_result_volume_plan(&changed, &root, run_id).is_err());
        changed = plan.clone();
        changed.vhdx_path = root.join("outside.vhdx");
        assert!(validate_guest_result_volume_plan(&changed, &root, run_id).is_err());
        changed = plan.clone();
        changed.guest_mount_path = r"C:\FRIDAY\Project".into();
        assert!(validate_guest_result_volume_plan(&changed, &root, run_id).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn result_volume_plan_rejects_symlinked_storage_root() {
        let base = make_test_directory("result-volume-link");
        let real = base.join("real");
        fs::create_dir(&real).unwrap();
        let link = base.join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(build_guest_result_volume_plan(&link, "0123456789abcdef0123456789abcdef").is_err());
        fs::remove_dir_all(base).unwrap();
    }


    #[test]
    fn result_volume_directory_is_exclusively_reserved_and_verified() {
        let root = make_test_directory("result-volume-reserve");
        let run_id = "0123456789abcdef0123456789abcdef";
        let plan = build_guest_result_volume_plan(&root, run_id).unwrap();
        let reserved = reserve_guest_result_volume_directory(&plan, &root).unwrap();
        assert_eq!(reserved, plan.run_directory);
        let directory_metadata = fs::symlink_metadata(&reserved).unwrap();
        assert!(directory_metadata.is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(directory_metadata.permissions().mode() & 0o077, 0);
        }
        assert!(reserved.join(GUEST_RESULT_VOLUME_OWNER_MARKER).is_file());
        assert_eq!(verify_guest_result_volume_directory(&plan, &root).unwrap(), reserved);
        assert!(reserve_guest_result_volume_directory(&plan, &root).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn native_result_vhdx_creation_produces_fixed_private_vhdx_and_refuses_reuse() {
        let root = make_test_directory("native-result-vhdx");
        let run_id = "0123456789abcdef0123456789abcdef";
        let plan = build_guest_result_volume_plan(&root, run_id).unwrap();
        reserve_guest_result_volume_directory(&plan, &root).unwrap();
        let created = create_guest_result_vhdx(&plan, &root).unwrap();
        assert_eq!(created, plan.vhdx_path);
        let metadata = fs::symlink_metadata(&created).unwrap();
        assert!(metadata.is_file());
        assert!(!is_reparse_or_symlink(&metadata));
        assert!(metadata.len() >= 8);
        let mut file = open_source_file(&created).unwrap();
        let mut signature = [0u8; 8];
        file.read_exact(&mut signature).unwrap();
        assert_eq!(&signature, b"vhdxfile");
        let identifier = verify_guest_result_vhdx_capacity(&created, GUEST_RESULT_VOLUME_CAPACITY_BYTES).unwrap();
        assert!(valid_guest_vhdx_identifier(&identifier));
        assert_eq!(verify_guest_result_vhdx_ownership_marker(&plan, &root, Some(&identifier)).unwrap(), identifier);
        assert_eq!(verify_guest_result_vhdx_identity(&plan, &root).unwrap(), identifier);
        let attached = attach_guest_result_vhdx(&plan, &root).unwrap();
        assert!(attached.physical_path().starts_with(r"\\.\PhysicalDrive"));
        let journal_root = root.join("journal");
        fs::create_dir(&journal_root).unwrap();
        secure_private_workspace_directory(&journal_root).unwrap();
        let empty_journal = GuestResultStorageJournal {
            schema_version: GUEST_RESULT_STORAGE_JOURNAL_SCHEMA,
            run_id: run_id.into(),
            vhdx_identifier: identifier.clone(),
            entries: Vec::new(),
        };
        let created_journal = append_and_persist_guest_result_storage_transition(
            &journal_root, &empty_journal, GuestResultStorageEvent::CreateVerified, true, false,
        ).unwrap();
        let attached_journal = append_and_persist_guest_result_storage_transition(
            &journal_root, &created_journal, GuestResultStorageEvent::AttachVerified, true, false,
        ).unwrap();
        let partitioned_journal = initialize_guest_result_gpt(
            &attached, &plan, &root, &journal_root, &attached_journal,
        ).unwrap();
        assert_eq!(
            validate_guest_result_storage_journal(&partitioned_journal).unwrap(),
            GuestResultStorageState::Partitioned,
        );
        attached.detach().unwrap();

        assert!(create_guest_result_vhdx(&plan, &root).is_err(),
            "a pre-existing result VHDX must never be replaced");
        drop(file);
        fs::remove_file(created).unwrap();
        fs::remove_dir_all(root).unwrap();
    }


    #[test]
    fn result_vhdx_file_path_requires_exact_owned_regular_vhdx_file() {
        let root = make_test_directory("result-vhdx-file-path");
        let run_id = "0123456789abcdef0123456789abcdef";
        let plan = build_guest_result_volume_plan(&root, run_id).unwrap();
        reserve_guest_result_volume_directory(&plan, &root).unwrap();
        fs::write(&plan.vhdx_path, b"vhdxfile-test").unwrap();
        assert_eq!(
            verify_guest_result_vhdx_file_path(&plan, &root).unwrap(),
            fs::canonicalize(&plan.vhdx_path).unwrap()
        );
        fs::remove_file(&plan.vhdx_path).unwrap();
        fs::write(plan.run_directory.join("unexpected.vhdx"), b"vhdxfile-test").unwrap();
        assert!(verify_guest_result_vhdx_file_path(&plan, &root).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn result_vhdx_file_path_rejects_symlink_substitution() {
        let root = make_test_directory("result-vhdx-file-link");
        let run_id = "0123456789abcdef0123456789abcdef";
        let plan = build_guest_result_volume_plan(&root, run_id).unwrap();
        reserve_guest_result_volume_directory(&plan, &root).unwrap();
        let outside = root.join("outside-result.vhdx");
        fs::write(&outside, b"vhdxfile-test").unwrap();
        std::os::unix::fs::symlink(&outside, &plan.vhdx_path).unwrap();
        assert!(verify_guest_result_vhdx_file_path(&plan, &root).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn result_vhdx_identity_marker_binds_exact_run_capacity_and_identifier() {
        let marker = GuestResultVhdxOwnershipMarker {
            schema_version: GUEST_RESULT_VHDX_OWNER_SCHEMA,
            run_id: "0123456789abcdef0123456789abcdef".into(),
            owner: "FRIDAY-CodingAssistant-ResultVhdx".into(),
            identifier: "12345678-1234-5678-9abc-def012345678".into(),
            capacity_bytes: GUEST_RESULT_VOLUME_CAPACITY_BYTES,
        };
        assert!(validate_guest_result_vhdx_ownership_marker(
            &marker, "0123456789abcdef0123456789abcdef", Some("12345678-1234-5678-9abc-def012345678")
        ).is_ok());
        assert!(validate_guest_result_vhdx_ownership_marker(
            &marker, "ffffffffffffffffffffffffffffffff", None
        ).is_err());
        assert!(validate_guest_result_vhdx_ownership_marker(
            &marker, "0123456789abcdef0123456789abcdef", Some("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa")
        ).is_err());
        let mut wrong_capacity = marker;
        wrong_capacity.capacity_bytes += 512;
        assert!(validate_guest_result_vhdx_ownership_marker(
            &wrong_capacity, "0123456789abcdef0123456789abcdef", None
        ).is_err());
    }

    #[test]
    fn result_vhdx_identity_marker_is_exclusive_and_rejects_tampering() {
        let root = make_test_directory("result-vhdx-identity");
        let run_id = "0123456789abcdef0123456789abcdef";
        let plan = build_guest_result_volume_plan(&root, run_id).unwrap();
        reserve_guest_result_volume_directory(&plan, &root).unwrap();
        let identifier = "12345678-1234-5678-9abc-def012345678";
        write_guest_result_vhdx_ownership_marker(&plan, &root, identifier).unwrap();
        assert_eq!(verify_guest_result_vhdx_ownership_marker(&plan, &root, Some(identifier)).unwrap(), identifier);
        assert!(write_guest_result_vhdx_ownership_marker(&plan, &root, identifier).is_err());
        assert!(verify_guest_result_vhdx_ownership_marker(&plan, &root, Some("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa")).is_err());
        fs::remove_file(plan.run_directory.join(GUEST_RESULT_VHDX_OWNER_MARKER)).unwrap();
        fs::write(plan.run_directory.join(GUEST_RESULT_VHDX_OWNER_MARKER),
            br#"{"schemaVersion":1,"runId":"ffffffffffffffffffffffffffffffff","owner":"FRIDAY-CodingAssistant-ResultVhdx","identifier":"12345678-1234-5678-9abc-def012345678","capacityBytes":67108864}"#).unwrap();
        assert!(verify_guest_result_vhdx_ownership_marker(&plan, &root, None).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn result_vhdx_identity_marker_rejects_symlink() {
        let root = make_test_directory("result-vhdx-identity-link");
        let run_id = "0123456789abcdef0123456789abcdef";
        let plan = build_guest_result_volume_plan(&root, run_id).unwrap();
        reserve_guest_result_volume_directory(&plan, &root).unwrap();
        let outside = root.join("outside-vhdx-marker");
        fs::write(&outside, b"not ownership evidence").unwrap();
        std::os::unix::fs::symlink(&outside, plan.run_directory.join(GUEST_RESULT_VHDX_OWNER_MARKER)).unwrap();
        assert!(verify_guest_result_vhdx_ownership_marker(&plan, &root, None).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn result_volume_directory_refuses_missing_or_tampered_ownership_evidence() {
        let root = make_test_directory("result-volume-marker");
        let run_id = "0123456789abcdef0123456789abcdef";
        let plan = build_guest_result_volume_plan(&root, run_id).unwrap();
        fs::create_dir(&plan.run_directory).unwrap();
        assert!(verify_guest_result_volume_directory(&plan, &root).is_err());
        fs::write(plan.run_directory.join(GUEST_RESULT_VOLUME_OWNER_MARKER),
            br#"{"schemaVersion":1,"runId":"ffffffffffffffffffffffffffffffff","owner":"FRIDAY-CodingAssistant-ResultVolume"}"#).unwrap();
        assert!(verify_guest_result_volume_directory(&plan, &root).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn result_volume_directory_refuses_symlinked_ownership_marker() {
        let root = make_test_directory("result-volume-marker-link");
        let run_id = "0123456789abcdef0123456789abcdef";
        let plan = build_guest_result_volume_plan(&root, run_id).unwrap();
        fs::create_dir(&plan.run_directory).unwrap();
        let outside = root.join("outside-marker");
        fs::write(&outside, b"not ownership evidence").unwrap();
        std::os::unix::fs::symlink(&outside, plan.run_directory.join(GUEST_RESULT_VOLUME_OWNER_MARKER)).unwrap();
        assert!(verify_guest_result_volume_directory(&plan, &root).is_err());
        fs::remove_dir_all(root).unwrap();
    }


    #[test]
    fn result_partition_plan_is_single_aligned_fixed_ntfs_partition() {
        let run_id = "0123456789abcdef0123456789abcdef";
        let plan = build_guest_result_partition_plan(
            run_id, GUEST_RESULT_VOLUME_CAPACITY_BYTES, 512
        ).unwrap();
        assert_eq!(plan.partition_number, 1);
        assert_eq!(plan.first_lba, 2048);
        assert_eq!(plan.last_lba, (GUEST_RESULT_VOLUME_CAPACITY_BYTES / 512) - 35);
        assert_eq!(plan.partition_type_guid, GUEST_RESULT_PARTITION_TYPE_GUID);
        assert_eq!(plan.partition_unique_guid, "01234567-89AB-CDEF-0123-456789ABCDEF");
        assert_eq!(plan.filesystem, "NTFS");
        assert_eq!(plan.filesystem_label, GUEST_RESULT_FILESYSTEM_LABEL);
        assert_eq!(plan.first_lba % GUEST_RESULT_GPT_ALIGNMENT_SECTORS, 0);
        validate_guest_result_partition_plan(&plan, run_id).unwrap();
    }

    #[test]
    fn result_partition_plan_rejects_capacity_sector_and_identifier_substitution() {
        let run_id = "0123456789abcdef0123456789abcdef";
        assert!(build_guest_result_partition_plan(run_id, 128 * 1024 * 1024, 512).is_err());
        assert!(build_guest_result_partition_plan(run_id, GUEST_RESULT_VOLUME_CAPACITY_BYTES, 4096).is_err());
        assert!(build_guest_result_partition_plan("../outside", GUEST_RESULT_VOLUME_CAPACITY_BYTES, 512).is_err());
        let mut plan = build_guest_result_partition_plan(
            run_id, GUEST_RESULT_VOLUME_CAPACITY_BYTES, 512
        ).unwrap();
        plan.last_lba -= 1;
        assert!(validate_guest_result_partition_plan(&plan, run_id).is_err());
        plan = build_guest_result_partition_plan(
            run_id, GUEST_RESULT_VOLUME_CAPACITY_BYTES, 512
        ).unwrap();
        plan.partition_unique_guid = "AAAAAAAA-AAAA-AAAA-AAAA-AAAAAAAAAAAA".into();
        assert!(validate_guest_result_partition_plan(&plan, run_id).is_err());
    }

}

use serde::Serialize;
use std::{
    collections::BTreeMap,
    env,
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::Read,
    path::{Component, Path, PathBuf},
    sync::Mutex,
    process::Command,
};
use rand::RngCore;
use tauri::{AppHandle, Manager};

const MAX_SNAPSHOT_DEPTH: usize = 12;
const MAX_SNAPSHOT_FILES: usize = 2_000;
const MAX_SNAPSHOT_FILE_BYTES: u64 = 1_048_576;
const MAX_SNAPSHOT_TOTAL_BYTES: u64 = 32 * 1024 * 1024;

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

static OWNED_SNAPSHOTS: Mutex<BTreeMap<String, PathBuf>> = Mutex::new(BTreeMap::new());

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
fn is_reparse_or_symlink(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_type().is_symlink() || (metadata.file_attributes() & 0x400 != 0)
}

#[cfg(not(windows))]
fn is_reparse_or_symlink(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(windows)]
fn detect_windows_sandbox_capabilities() -> (String, String, String, bool, String, Option<bool>, bool) {
    // Fixed, read-only PowerShell probes. No user or model supplied command text is accepted.
    let script = r#"$ErrorActionPreference = 'SilentlyContinue'
$cv = Get-ItemProperty 'HKLM:\\SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion'
$feature = Get-WindowsOptionalFeature -Online -FeatureName Containers-DisposableClientVM
$cpu = Get-CimInstance Win32_Processor | Select-Object -First 1 -ExpandProperty VirtualizationFirmwareEnabled
$active = [bool](Get-Process -Name WindowsSandbox -ErrorAction SilentlyContinue)
$wsb = Get-Command wsb -ErrorAction SilentlyContinue
$version = ''
if ($wsb) { $version = (& $wsb.Source --version 2>&1 | Out-String).Trim() }
[pscustomobject]@{
  edition = [string]$cv.EditionID
  build = [string]$cv.CurrentBuildNumber
  feature = [string]$feature.State
  wsbAvailable = [bool]$wsb
  wsbVersion = [string]$version
  virtualizationFirmwareEnabled = if ($null -eq $cpu) { $null } else { [bool]$cpu }
  sandboxProcessDetected = $active
} | ConvertTo-Json -Compress"#;
    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", script])
        .output();
    let Ok(output) = output else {
        return ("Unknown".into(), "Unknown".into(), "Unknown".into(), false, "Unavailable".into(), None, false);
    };
    if !output.status.success() {
        return ("Unknown".into(), "Unknown".into(), "Unknown".into(), false, "Unavailable".into(), None, false);
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&output.stdout) else {
        return ("Unknown".into(), "Unknown".into(), "Unknown".into(), false, "Unavailable".into(), None, false);
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
    )
}

#[tauri::command]
pub async fn coding_sandbox_preflight() -> Result<CodingSandboxPreflight, String> {
    let _project_root = crate::coding_assistant::selected_project_root_for_sandbox()?;
    #[cfg(windows)]
    {
        let detected = detect_windows_sandbox();
        let (edition, build, feature, wsb_available, wsb_version, virtualization, active) =
            detect_windows_sandbox_capabilities();
        let edition_supported = ["Professional", "Enterprise", "Education"]
            .iter().any(|supported| edition.eq_ignore_ascii_case(supported));
        let runtime_ready = detected && edition_supported
            && feature.eq_ignore_ascii_case("Enabled") && wsb_available
            && virtualization == Some(true) && !active;
        let message = if active {
            "A Windows Sandbox process is already running. FRIDAY will not attach to or stop a session it did not create. Execution remains disabled.".to_string()
        } else if runtime_ready {
            "Windows Sandbox prerequisites appear present. This is detection only, not a launch test. Execution remains disabled until command restrictions, resource enforcement, cancellation and result validation are complete.".to_string()
        } else {
            "Windows Sandbox is not yet confirmed ready. Review the detected edition, feature, CLI and virtualization details. Unknown values are treated as unverified. FRIDAY will not fall back to host execution.".to_string()
        };
        Ok(CodingSandboxPreflight {
            platform_supported: true, runtime_detected: detected,
            execution_enabled: false, network_isolation_enforced: false,
            windows_edition: edition, windows_build: build, sandbox_feature_state: feature,
            wsb_cli_available: wsb_available, wsb_cli_version: wsb_version,
            virtualization_firmware_enabled: virtualization, sandbox_process_detected: active,
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

#[cfg(not(windows))]
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

    let mut budget = SnapshotBudget { files: 0, bytes: 0, truncated: false };
    let result = copy_snapshot_tree(&source_root, &source_root, &snapshot_path, 0, &mut budget);
    if let Err(error) = result {
        let _ = fs::remove_dir_all(&snapshot_path);
        return Err(error);
    }
    if budget.files == 0 {
        let _ = fs::remove_dir_all(&snapshot_path);
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
pub async fn coding_sandbox_config_preview(snapshot_id: String) -> Result<String, String> {
    if snapshot_id.len() != 32 || !snapshot_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
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
    if snapshot_id.len() != 32 || !snapshot_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("The snapshot identifier is invalid.".into());
    }
    let mut owned = OWNED_SNAPSHOTS.lock()
        .map_err(|_| "Snapshot ownership tracking is temporarily unavailable.".to_string())?;
    let path = owned.get(&snapshot_id).cloned()
        .ok_or_else(|| "This snapshot is not owned by the current FRIDAY session.".to_string())?;
    let metadata = fs::symlink_metadata(&path)
        .map_err(|_| "The snapshot folder is already unavailable; FRIDAY will not guess what to delete.".to_string())?;
    if !metadata.is_dir() || is_reparse_or_symlink(&metadata) {
        return Err("The snapshot path is no longer a normal directory; FRIDAY refused cleanup.".into());
    }
    let parent = path.parent().ok_or_else(|| "The snapshot folder has no verified parent.".to_string())?;
    let parent_metadata = fs::symlink_metadata(parent)
        .map_err(|_| "FRIDAY could not verify the private snapshot workspace.".to_string())?;
    if !parent_metadata.is_dir() || is_reparse_or_symlink(&parent_metadata) {
        return Err("The private snapshot workspace is no longer a normal directory; cleanup was refused.".into());
    }
    let canonical_parent = fs::canonicalize(parent)
        .map_err(|_| "FRIDAY could not verify the private snapshot workspace path.".to_string())?;
    let canonical = fs::canonicalize(&path)
        .map_err(|_| "FRIDAY could not verify the snapshot folder before cleanup.".to_string())?;
    if canonical != path || canonical.parent() != Some(canonical_parent.as_path())
        || path.file_name().and_then(OsStr::to_str) != Some(snapshot_id.as_str())
    {
        return Err("The snapshot folder failed its ownership-path check; cleanup was refused.".into());
    }
    fs::remove_dir_all(&canonical)
        .map_err(|_| "FRIDAY could not completely remove the isolated snapshot folder.".to_string())?;
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
            platform_supported: true, runtime_detected: true,
            execution_enabled: false, network_isolation_enforced: false,
            message: "Preflight only".into(),
        };
        assert!(!policy.execution_enabled);
        assert!(!policy.network_isolation_enforced);
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

use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{OsStr, OsString},
    fs,
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};
use sha2::{Digest, Sha256};
use tauri::AppHandle;
use tauri_plugin_dialog::DialogExt;

const MAX_PROJECT_DEPTH: usize = 4;
const MAX_SCANNED_ENTRIES: usize = 3_000;
const MAX_PROJECT_CANDIDATES: usize = 300;
const MAX_SELECTED_FILES: usize = 25;
const MAX_FILE_BYTES: u64 = 128 * 1024;
const MAX_TOTAL_BYTES: usize = 180 * 1024;

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CodingFileCandidate {
    id: String,
    name: String,
    relative_path: String,
    size_bytes: u64,
    language: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodingProjectScan {
    project_name: String,
    files: Vec<CodingFileCandidate>,
    scanned_entries: usize,
    truncated: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodingProjectFile {
    name: String,
    relative_path: String,
    language: String,
    content: String,
    size_bytes: usize,
}

#[derive(Clone)]
struct ProjectGrant {
    root: PathBuf,
    path: PathBuf,
    root_identity: (u64, u64),
    file_identity: crate::coding_sandbox_fs::RuntimeTreeIdentity,
}

static PROJECT_GRANTS: Mutex<BTreeMap<String, ProjectGrant>> = Mutex::new(BTreeMap::new());
static SELECTED_PROJECT_ROOT: Mutex<Option<PathBuf>> = Mutex::new(None);

const MAX_PROPOSED_FILE_BYTES: usize = 128 * 1024;
const CODE_PREVIEW_TTL_SECONDS: u64 = 120;
const MAX_CODE_BACKUP_FILES: usize = 500;
const MAX_CODE_BACKUP_BYTES: u64 = 100 * 1024 * 1024;

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CodingChangePreview {
    preview_id: String,
    file_name: String,
    relative_path: String,
    original_sha256: String,
    proposed_sha256: String,
    original_size_bytes: usize,
    proposed_size_bytes: usize,
    unified_diff: String,
    expires_in_seconds: u64,
}

struct StoredCodePreview {
    file_id: String,
    root: PathBuf,
    path: PathBuf,
    original_sha256: String,
    proposed_sha256: String,
    proposed_content: String,
    expires_at: u64,
    approval_token: Option<String>,
}

static CODE_PREVIEWS: Mutex<BTreeMap<String, StoredCodePreview>> = Mutex::new(BTreeMap::new());

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodingApplyResult {
    file_name: String,
    relative_path: String,
    backup_relative_path: String,
    bytes_written: usize,
    original_sha256: String,
    resulting_sha256: String,
    undo_record_id: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodingChangeHistoryItem {
    id: String,
    relative_path: String,
    backup_relative_path: String,
    created_at: u64,
    status: String,
    undo_record_id: String,
    undo_available: bool,
    unavailable_reason: Option<String>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CodingUndoRecord {
    id: String,
    relative_path: String,
    backup_relative_path: String,
    original_sha256: String,
    resulting_sha256: String,
    created_at: u64,
    #[serde(default)]
    original_unix_mode: Option<u32>,
    // Self-relative Windows security descriptor containing the original DACL,
    // hex-encoded so older records remain readable and no new crate is needed.
    #[serde(default)]
    original_windows_dacl: Option<String>,
}

#[cfg(windows)]
#[link(name = "Advapi32")]
extern "system" {
    fn GetFileSecurityW(
        file_name: *const u16,
        requested_information: u32,
        security_descriptor: *mut std::ffi::c_void,
        descriptor_length: u32,
        length_needed: *mut u32,
    ) -> i32;
    fn SetFileSecurityW(
        file_name: *const u16,
        security_information: u32,
        security_descriptor: *mut std::ffi::c_void,
    ) -> i32;
    fn GetSecurityDescriptorControl(
        security_descriptor: *mut std::ffi::c_void,
        control: *mut u16,
        revision: *mut u32,
    ) -> i32;
}

#[cfg(windows)]
fn windows_dacl_descriptor(path: &Path) -> Result<Vec<u8>, String> {
    use std::os::windows::ffi::OsStrExt;
    const DACL_SECURITY_INFORMATION: u32 = 0x0000_0004;
    let wide = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let mut needed = 0u32;
    let first = unsafe {
        GetFileSecurityW(wide.as_ptr(), DACL_SECURITY_INFORMATION, std::ptr::null_mut(), 0, &mut needed)
    };
    if first != 0 || needed == 0 {
        return Err("Windows could not determine the original file DACL size.".into());
    }
    let mut descriptor = vec![0u8; needed as usize];
    let succeeded = unsafe {
        GetFileSecurityW(
            wide.as_ptr(), DACL_SECURITY_INFORMATION,
            descriptor.as_mut_ptr() as *mut std::ffi::c_void,
            needed, &mut needed,
        )
    };
    if succeeded == 0 || needed as usize > descriptor.len() {
        return Err(format!("Windows could not capture the original file DACL (error {}).", unsafe { GetLastError() }));
    }
    descriptor.truncate(needed as usize);
    Ok(descriptor)
}

#[cfg(windows)]
fn original_windows_dacl(path: &Path) -> Result<Option<String>, String> {
    let descriptor = windows_dacl_descriptor(path)?;
    let mut encoded = String::with_capacity(descriptor.len() * 2);
    for byte in descriptor { use std::fmt::Write as _; let _ = write!(&mut encoded, "{byte:02x}"); }
    Ok(Some(encoded))
}

#[cfg(not(windows))]
fn original_windows_dacl(_path: &Path) -> Result<Option<String>, String> { Ok(None) }

#[cfg(windows)]
fn restore_windows_dacl(path: &Path, encoded: &str) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    const DACL_SECURITY_INFORMATION: u32 = 0x0000_0004;
    if encoded.is_empty() || encoded.len() % 2 != 0 || encoded.len() > 128 * 1024 {
        return Err("The saved Windows DACL metadata is malformed or outside its size limit.".into());
    }
    let mut descriptor = Vec::with_capacity(encoded.len() / 2);
    for pair in encoded.as_bytes().chunks_exact(2) {
        let digit = |b: u8| -> Option<u8> {
            match b { b'0'..=b'9' => Some(b - b'0'), b'a'..=b'f' => Some(b - b'a' + 10), b'A'..=b'F' => Some(b - b'A' + 10), _ => None }
        };
        let high = digit(pair[0]).ok_or("The saved Windows DACL metadata contains invalid hexadecimal data.")?;
        let low = digit(pair[1]).ok_or("The saved Windows DACL metadata contains invalid hexadecimal data.")?;
        descriptor.push((high << 4) | low);
    }
    if descriptor.len() < 20 {
        return Err("The saved Windows security descriptor is too short.".into());
    }
    const PROTECTED_DACL_SECURITY_INFORMATION: u32 = 0x8000_0000;
    const UNPROTECTED_DACL_SECURITY_INFORMATION: u32 = 0x2000_0000;
    const SE_DACL_PROTECTED: u16 = 0x1000;
    let mut control = 0u16;
    let mut revision = 0u32;
    let control_read = unsafe {
        GetSecurityDescriptorControl(
            descriptor.as_mut_ptr() as *mut std::ffi::c_void,
            &mut control,
            &mut revision,
        )
    };
    if control_read == 0 {
        return Err(format!("Windows could not inspect the saved DACL inheritance state (error {}).", unsafe { GetLastError() }));
    }
    let inheritance = if control & SE_DACL_PROTECTED != 0 {
        PROTECTED_DACL_SECURITY_INFORMATION
    } else {
        UNPROTECTED_DACL_SECURITY_INFORMATION
    };
    let wide = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let succeeded = unsafe {
        SetFileSecurityW(
            wide.as_ptr(), DACL_SECURITY_INFORMATION | inheritance,
            descriptor.as_mut_ptr() as *mut std::ffi::c_void,
        )
    };
    if succeeded == 0 {
        return Err(format!("Windows could not restore the original file DACL (error {}).", unsafe { GetLastError() }));
    }
    Ok(())
}

#[cfg(windows)]
#[link(name = "Kernel32")]
extern "system" {
    fn ReplaceFileW(
        replaced_file_name: *const u16,
        replacement_file_name: *const u16,
        backup_file_name: *const u16,
        replace_flags: u32,
        exclude: *mut std::ffi::c_void,
        reserved: *mut std::ffi::c_void,
    ) -> i32;
    fn GetLastError() -> u32;
}

#[cfg(windows)]
fn replace_file_with_backup(target: &Path, replacement: &Path, displaced: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    let wide = |path: &Path| path.as_os_str().encode_wide().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let target_wide = wide(target);
    let replacement_wide = wide(replacement);
    let displaced_wide = wide(displaced);
    // Flags are deliberately zero: in particular, never ignore ACL merge failures.
    let succeeded = unsafe {
        ReplaceFileW(
            target_wide.as_ptr(),
            replacement_wide.as_ptr(),
            displaced_wide.as_ptr(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if succeeded == 0 {
        let code = unsafe { GetLastError() };
        return Err(format!("Windows ReplaceFileW failed with error {code}"));
    }
    Ok(())
}

fn ensure_code_backup_dir(root: &Path) -> Result<PathBuf, String> {
    let dir = root.join(".friday-code-backups");
    match fs::symlink_metadata(&dir) {
        Ok(meta) if is_redirected(&meta) || !meta.is_dir() =>
            return Err("FRIDAY's code backup folder is not a safe normal directory.".into()),
        Ok(_) => {},
        Err(error) if error.kind() == std::io::ErrorKind::NotFound =>
            fs::create_dir(&dir).map_err(|e| format!("Could not create the code backup folder: {e}"))?,
        Err(_) => return Err("Could not inspect FRIDAY's code backup folder.".into()),
    }
    let meta = fs::symlink_metadata(&dir).map_err(|_| "Could not verify the code backup folder.".to_string())?;
    if is_redirected(&meta) || !meta.is_dir() { return Err("FRIDAY's code backup folder is not safe.".into()); }
    let canonical = fs::canonicalize(&dir).map_err(|_| "Could not verify FRIDAY's code backup folder.".to_string())?;
    if !canonical.starts_with(root) { return Err("FRIDAY's code backup folder is outside the selected project.".into()); }
    Ok(canonical)
}

fn check_code_backup_capacity(dir: &Path, additional_bytes: u64) -> Result<(), String> {
    let mut count = 0usize;
    let mut bytes = 0u64;
    for entry in fs::read_dir(dir).map_err(|_| "Could not inspect code backup storage.".to_string())? {
        let entry = entry.map_err(|_| "Could not inspect a code backup item.".to_string())?;
        let meta = fs::symlink_metadata(entry.path()).map_err(|_| "Could not inspect a code backup item.".to_string())?;
        if is_redirected(&meta) || !meta.is_file() { return Err("Code backup storage contains an unexpected item. Review it before continuing.".into()); }
        count = count.saturating_add(1);
        bytes = bytes.saturating_add(meta.len());
    }
    if count.saturating_add(3) > MAX_CODE_BACKUP_FILES || bytes.saturating_add(additional_bytes) > MAX_CODE_BACKUP_BYTES {
        return Err("FRIDAY's code backup limit (500 files or 100 MB) would be exceeded. Existing backups will not be deleted.".into());
    }
    Ok(())
}

#[cfg(any(unix, windows))]
fn open_verified_code_parent(
    root: &Path,
    current: &Path,
) -> Result<(File, OsString, (u64, u64)), String> {
    let relative = current.strip_prefix(root)
        .map_err(|_| "The selected project file is outside the approved folder.".to_string())?;
    let components = relative.components().collect::<Vec<_>>();
    if components.is_empty() || components.iter().any(|part| !matches!(part, std::path::Component::Normal(_))) {
        return Err("The selected project path is not normalized.".into());
    }
    let mut parent = crate::coding_sandbox_fs::open_verified_root_directory(root)
        .map_err(|_| "Could not safely open the approved project folder.".to_string())?;
    for component in &components[..components.len() - 1] {
        let std::path::Component::Normal(name) = component else { unreachable!() };
        parent = crate::coding_sandbox_fs::open_child_directory_nofollow(&parent, name)
            .map_err(|_| "A project parent folder changed or was redirected.".to_string())?;
    }
    let std::path::Component::Normal(file_name) = components[components.len() - 1] else { unreachable!() };
    let parent_identity = crate::coding_sandbox_fs::verified_directory_handle_identity(&parent)
        .map_err(|_| "Could not verify the selected project file's parent folder.".to_string())?;
    Ok((parent, file_name.to_os_string(), parent_identity))
}

#[cfg(any(unix, windows))]
fn verified_code_child_identity(parent: &File, name: &OsStr) -> Result<crate::coding_sandbox_fs::RuntimeTreeIdentity, String> {
    let entries = crate::coding_sandbox_fs::enumerate_runtime_tree_entries(parent, MAX_SCANNED_ENTRIES)
        .map_err(|_| "Could not safely inspect the selected project file's parent folder.".to_string())?;
    let entry = entries.into_iter().find(|entry| entry.name == name)
        .ok_or("The selected project file disappeared before installation.")?;
    if !entry.is_file || entry.is_reparse {
        return Err("The selected project entry is no longer an ordinary regular file.".into());
    }
    let opened = crate::coding_sandbox_fs::open_file_nofollow(parent, name)
        .map_err(|_| "The selected project entry could not be safely reopened.".to_string())?;
    if !crate::coding_sandbox_fs::runtime_tree_handle_matches(entry.identity, &opened)
        .map_err(|_| "Could not verify the selected project file identity.".to_string())?
    {
        return Err("The selected project file changed identity before installation.".into());
    }
    Ok(entry.identity)
}


#[cfg(any(unix, windows))]
fn verify_installed_code_file(
    parent: &File,
    name: &OsStr,
    expected_identity: crate::coding_sandbox_fs::RuntimeTreeIdentity,
    expected_sha256: &str,
) -> Result<(), String> {
    let mut file = crate::coding_sandbox_fs::open_file_nofollow(parent, name)
        .map_err(|_| "The installed project file could not be safely reopened relative to its verified parent.".to_string())?;
    let metadata = file.metadata()
        .map_err(|_| "Could not inspect the installed project file.".to_string())?;
    if !metadata.is_file() || is_redirected(&metadata)
        || metadata.len() == 0 || metadata.len() > MAX_PROPOSED_FILE_BYTES as u64
        || !crate::coding_sandbox_fs::runtime_tree_handle_matches(expected_identity, &file)
            .map_err(|_| "Could not verify the installed project file identity.".to_string())?
    {
        return Err("The installed project file does not match the verified replacement identity.".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_PROPOSED_FILE_BYTES as u64 + 1).read_to_end(&mut bytes)
        .map_err(|_| "Could not read the installed project file through its verified handle.".to_string())?;
    if bytes.len() as u64 != metadata.len() || sha256_hex(&bytes) != expected_sha256 {
        return Err("The installed project file failed its identity-bound content verification.".into());
    }
    Ok(())
}

fn apply_code_preview_to_disk(
    root: &Path, current: &Path, proposed: &[u8], expected_sha256: &str, token: &str, now: u64,
) -> Result<(PathBuf, CodingUndoRecord), String> {
    apply_code_preview_to_disk_bound(root, current, proposed, expected_sha256, token, now, None)
}

fn apply_code_preview_to_disk_bound(
    root: &Path, current: &Path, proposed: &[u8], expected_sha256: &str, token: &str, now: u64,
    expected_identity: Option<((u64, u64), crate::coding_sandbox_fs::RuntimeTreeIdentity)>,
) -> Result<(PathBuf, CodingUndoRecord), String>
{
    if !current.starts_with(root) || proposed.is_empty() || proposed.len() > MAX_PROPOSED_FILE_BYTES || proposed.contains(&0) {
        return Err("The approved code change failed its project or content bounds.".into());
    }
    let meta = fs::symlink_metadata(current).map_err(|_| "The selected project file is no longer available.".to_string())?;
    if is_redirected(&meta) || !meta.is_file() || meta.len() == 0 || meta.len() > MAX_FILE_BYTES {
        return Err("The selected project file is no longer an allowed regular file.".into());
    }
    if meta.permissions().readonly() { return Err("The selected project file is read-only.".into()); }
    if !supported_project_file(current) { return Err("This file type is not allowed for safe code editing.".into()); }
    let canonical = fs::canonicalize(current).map_err(|_| "Could not revalidate the selected project file.".to_string())?;
    if canonical != current || !canonical.starts_with(root) { return Err("The selected project file moved or was redirected.".into()); }
    let original = if let Some((expected_root_identity, expected_file_identity)) = expected_identity {
        let root_handle = crate::coding_sandbox_fs::open_verified_root_directory(root)
            .map_err(|_| "Could not safely reopen the approved project folder.".to_string())?;
        let actual_root_identity = crate::coding_sandbox_fs::verified_directory_identity(root, &root_handle)
            .map_err(|_| "Could not verify the approved project folder.".to_string())?;
        if actual_root_identity != expected_root_identity {
            return Err("The approved project folder changed after scanning. No changes were made.".into());
        }
        let relative = current.strip_prefix(root)
            .map_err(|_| "The selected project file is outside the approved folder.".to_string())?;
        crate::coding_sandbox_fs::read_verified_project_file(
            &root_handle, relative, expected_file_identity, MAX_FILE_BYTES,
        ).map_err(|_| "The selected project file no longer matches the identity captured during scanning. Create a fresh preview.".to_string())?
    } else {
        fs::read(&canonical).map_err(|_| "Could not re-read the selected project file.".to_string())?
    };
    if original.len() as u64 != meta.len() || original.len() as u64 > MAX_FILE_BYTES || original.contains(&0)
        || std::str::from_utf8(&original).is_err() {
        return Err("The selected project file changed or is no longer bounded UTF-8 text.".into());
    }
    let original_hash = sha256_hex(&original);
    let original_windows_dacl = original_windows_dacl(&canonical)?;
    if original_hash != expected_sha256 { return Err("The file changed after preview. No changes were made; create a fresh preview.".into()); }
    let resulting_hash = sha256_hex(proposed);
    if resulting_hash == original_hash { return Err("The proposed content matches the original; there is nothing to apply.".into()); }

    let backup_dir = ensure_code_backup_dir(root)?;
    check_code_backup_capacity(&backup_dir, (original.len() as u64).saturating_add(16_384))?;
    let backup_directory_handle = {
        let root_handle = crate::coding_sandbox_fs::open_verified_root_directory(root)
            .map_err(|_| "Could not safely reopen the approved project folder for backup storage.".to_string())?;
        crate::coding_sandbox_fs::open_child_directory_nofollow(&root_handle, OsStr::new(".friday-code-backups"))
            .map_err(|_| "The code backup folder changed or was redirected.".to_string())?
    };
    let backup = backup_dir.join(format!("{token}.bak"));
    let mut backup_file = crate::coding_sandbox_fs::create_destination_entry(
        &backup_directory_handle,
        backup.file_name().ok_or("The backup filename is invalid.")?,
        false,
    ).map_err(|e| format!("Could not create a new handle-relative original-file backup: {e}"))?;
    if let Err(e) = backup_file.write_all(&original).and_then(|_| backup_file.sync_all()) {
        drop(backup_file);
        return Err(format!("Could not write the original-file backup: {e}. Any partial recovery item is retained for inspection."));
    }
    drop(backup_file);
    let verified_backup = crate::coding_sandbox_fs::read_bounded_file_from_directory_handle(
        &backup_directory_handle,
        backup.file_name().ok_or("The backup filename is invalid.")?,
        MAX_FILE_BYTES,
        MAX_SCANNED_ENTRIES,
    ).map_err(|_| "Could not verify the original-file backup through its verified directory handle.".to_string())?;
    if sha256_hex(&verified_backup) != original_hash {
        return Err("The original-file backup failed SHA-256 verification. The project file was not changed.".into());
    }

    let relative = relative_path(root, current).ok_or("The project file is outside the selected folder.")?;
    let record = CodingUndoRecord {
        id: token.to_string(), relative_path: relative.clone(),
        backup_relative_path: format!(".friday-code-backups/{token}.bak"),
        original_sha256: original_hash.clone(), resulting_sha256: resulting_hash.clone(), created_at: now,
        original_unix_mode: original_unix_mode(&meta),
        original_windows_dacl,
    };
    let journal_path = backup_dir.join(format!("{token}.txn.json"));
    let journal_bytes = serde_json::to_vec(&record).map_err(|_| "Could not prepare the code-change recovery journal.".to_string())?;
    let mut journal = crate::coding_sandbox_fs::create_destination_entry(
        &backup_directory_handle,
        journal_path.file_name().ok_or("The journal filename is invalid.")?,
        false,
    ).map_err(|e| format!("Could not create the handle-relative code-change recovery journal: {e}"))?;
    journal.write_all(&journal_bytes).and_then(|_| journal.sync_all())
        .map_err(|e| format!("Could not flush the code-change recovery journal: {e}"))?;
    drop(journal);

    let parent = current.parent().ok_or("The selected project file has no parent folder.")?;
    #[cfg(any(unix, windows))]
    let (parent_handle, current_name, parent_identity) = open_verified_code_parent(root, current)?;
    let temporary = parent.join(format!(".friday-code-edit-{token}.tmp"));
    let mut temp_file = crate::coding_sandbox_fs::create_destination_entry(
        &parent_handle,
        temporary.file_name().ok_or("The replacement filename is invalid.")?,
        false,
    ).map_err(|e| format!("Could not prepare the handle-relative replacement file: {e}"))?;
    if let Err(e) = temp_file.set_permissions(meta.permissions()).and_then(|_| temp_file.write_all(proposed)).and_then(|_| temp_file.sync_all()) {
        drop(temp_file); let _ = fs::remove_file(&temporary);
        return Err(format!("Could not prepare the replacement contents: {e}"));
    }
    drop(temp_file);
    let temp_bytes = crate::coding_sandbox_fs::read_bounded_file_from_directory_handle(
        &parent_handle,
        temporary.file_name().ok_or("The replacement filename is invalid.")?,
        MAX_PROPOSED_FILE_BYTES as u64,
        MAX_SCANNED_ENTRIES,
    ).map_err(|_| "Could not verify the replacement through its verified parent directory handle.".to_string())?;
    if sha256_hex(&temp_bytes) != resulting_hash {
        let _ = fs::remove_file(&temporary);
        return Err("The replacement file failed SHA-256 verification. The original was not changed.".into());
    }

    // Re-check the scanned identity after preparing recovery data and immediately
    // before handle-relative, no-replace installation on both supported platforms.
    if let Some((expected_root_identity, expected_file_identity)) = expected_identity {
        let root_handle = crate::coding_sandbox_fs::open_verified_root_directory(root)
            .map_err(|_| "The approved project folder changed before installation.".to_string())?;
        let actual_root_identity = crate::coding_sandbox_fs::verified_directory_identity(root, &root_handle)
            .map_err(|_| "Could not reverify the approved project folder before installation.".to_string())?;
        if actual_root_identity != expected_root_identity {
            return Err("The approved project folder changed before installation. Recovery files were retained.".into());
        }
        let relative = current.strip_prefix(root)
            .map_err(|_| "The selected project file moved outside the approved folder.".to_string())?;
        let latest = crate::coding_sandbox_fs::read_verified_project_file(
            &root_handle, relative, expected_file_identity, MAX_FILE_BYTES,
        ).map_err(|_| "The selected project file changed identity before installation. Recovery files were retained.".to_string())?;
        if sha256_hex(&latest) != original_hash {
            return Err("The selected project file changed before installation. Recovery files were retained.".into());
        }
    }

    let displaced = parent.join(format!(".friday-code-original-{token}.tmp"));
    #[cfg(any(unix, windows))]
    let original_identity = verified_code_child_identity(&parent_handle, &current_name)?;
    #[cfg(any(unix, windows))]
    let temporary_identity = verified_code_child_identity(
        &parent_handle, temporary.file_name().ok_or("The replacement filename is invalid.")?,
    )?;
    #[cfg(any(unix, windows))]
    let displaced_name = displaced.file_name().ok_or("The displaced-file name is invalid.")?;
    #[cfg(any(unix, windows))]
    {
        let temporary_name = temporary.file_name()
            .ok_or("The replacement-file name is invalid.")?;
        if let Some((expected_root_identity, expected_file_identity)) = expected_identity {
            let actual_root = crate::coding_sandbox_fs::open_verified_root_directory(root)
                .and_then(|handle| crate::coding_sandbox_fs::verified_directory_identity(root, &handle))
                .map_err(|_| "Could not reverify the approved project folder before installation.".to_string())?;
            if actual_root != expected_root_identity || original_identity != expected_file_identity {
                return Err("The approved project or source-file identity changed before installation. Recovery files were retained.".into());
            }
        }
        let latest_original = crate::coding_sandbox_fs::open_file_nofollow(&parent_handle, &current_name)
            .map_err(|_| "The selected project file changed before installation.".to_string())?;
        let mut latest_bytes = Vec::new();
        use std::io::Read as _;
        latest_original.take(MAX_FILE_BYTES + 1).read_to_end(&mut latest_bytes)
            .map_err(|_| "Could not re-read the selected project file before installation.".to_string())?;
        if sha256_hex(&latest_bytes) != original_hash {
            return Err("The selected project file changed before installation. Recovery files were retained.".into());
        }
        crate::coding_sandbox_fs::rename_file_noreplace(
            &parent_handle, &current_name, displaced_name, parent_identity, original_identity,
        ).map_err(|error| format!(
            "Could not stage the verified original without replacing another file: {error}. Recovery files were retained."
        ))?;
        if let Err(error) = crate::coding_sandbox_fs::rename_file_noreplace(
            &parent_handle, temporary_name, current_name.as_os_str(), parent_identity, temporary_identity,
        ) {
            let restored = crate::coding_sandbox_fs::rename_file_noreplace(
                &parent_handle, displaced_name, &current_name, parent_identity, original_identity,
            ).is_ok();
            return Err(if restored {
                format!("Could not install the approved change; the original file was restored: {error}. Recovery files were retained.")
            } else {
                format!("Could not install the change or restore the original automatically. The original remains at {} and recovery files were retained: {error}", displaced.display())
            });
        }
    }
    #[cfg(any(unix, windows))]
    let installed_verification = verify_installed_code_file(
        &parent_handle, &current_name, temporary_identity, &resulting_hash,
    );
    if let Err(error) = installed_verification {
        #[cfg(any(unix, windows))]
        {
            let failed_name = format!(".friday-code-failed-{token}.tmp");
            let failed_move = verified_code_child_identity(&parent_handle, &current_name)
                .ok()
                .filter(|identity| *identity == temporary_identity)
                .and_then(|identity| crate::coding_sandbox_fs::rename_file_noreplace(
                    &parent_handle, &current_name, OsStr::new(&failed_name), parent_identity, identity,
                ).ok())
                .is_some();
            let restored = crate::coding_sandbox_fs::rename_file_noreplace(
                &parent_handle, displaced_name, &current_name, parent_identity, original_identity,
            ).is_ok();
            return Err(if restored {
                format!("The installed file failed verification ({error}); FRIDAY restored the original using verified handle-relative operations.")
            } else {
                format!("The installed file failed verification ({error}) and automatic restore could not be proven safe. The original remains at {}. The failed replacement was preserved: {}.", displaced.display(), if failed_move { failed_name } else { "current project entry left untouched".into() })
            });
        }
    }
    let undo_path = backup_dir.join(format!("{token}.undo.json"));
    let undo_bytes = serde_json::to_vec(&record).map_err(|_| "Could not prepare the guarded undo record.".to_string())?;
    let mut undo_file = crate::coding_sandbox_fs::create_destination_entry(
        &backup_directory_handle,
        undo_path.file_name().ok_or("The Undo filename is invalid.")?,
        false,
    ).map_err(|e| format!("The change was installed and verified, but its handle-relative Undo record could not be created: {e}. The original backup remains at {}.", backup.display()))?;
    if let Err(e) = undo_file.write_all(&undo_bytes).and_then(|_| undo_file.sync_all()) {
        drop(undo_file);
        return Err(format!("The change was installed and verified, but its undo record could not be saved: {e}. The original backup remains at {}.", backup.display()));
    }
    drop(undo_file);
    if let Err(e) = fs::remove_file(&displaced) {
        return Err(format!("The change was verified and its undo record saved, but FRIDAY could not remove its temporary displaced copy: {e}. The verified original backup remains."));
    }
    fs::remove_file(&journal_path)
        .map_err(|e| format!("The change was applied and its Undo record saved, but FRIDAY could not clean up the resolved transaction journal: {e}. Recovery will reconcile it before the next edit."))?;
    Ok((backup, record))
}

fn validate_undo_record(record: &CodingUndoRecord) -> Result<(), String> {
    if record.id.len() != 48 || !record.id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("The undo record identifier is invalid.".into());
    }
    let relative = Path::new(&record.relative_path);
    if relative.as_os_str().is_empty() || relative.is_absolute()
        || relative.components().any(|part| !matches!(part, std::path::Component::Normal(_))) {
        return Err("The undo record contains an unsafe project path.".into());
    }
    let expected_backup = format!(".friday-code-backups/{}.bak", record.id);
    if record.backup_relative_path != expected_backup || record.original_sha256.len() != 64
        || record.resulting_sha256.len() != 64
        || !record.original_sha256.bytes().all(|b| b.is_ascii_hexdigit())
        || !record.resulting_sha256.bytes().all(|b| b.is_ascii_hexdigit())
        || record.original_sha256 == record.resulting_sha256 {
        return Err("The undo record failed its integrity checks.".into());
    }
    if let Some(ref dacl) = record.original_windows_dacl {
        if dacl.len() < 40 || dacl.len() > 128 * 1024 || dacl.len() % 2 != 0
            || !dacl.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("The saved Windows DACL metadata is malformed or outside its size limit.".into());
        }
    }
    Ok(())
}

fn read_recovery_file(path: &Path, limit: u64) -> Result<Option<Vec<u8>>, String> {
    let parent = path.parent()
        .ok_or("A recovery file has no containing directory.")?;
    let name = path.file_name()
        .ok_or("A recovery file has no valid filename.")?;
    // Resolve the containing directory once, then perform the entire child
    // lookup/read through that verified directory capability. This central
    // wrapper covers journals, temporary copies, displaced files and backups.
    let directory = crate::coding_sandbox_fs::open_verified_root_directory(parent)
        .map_err(|_| "FRIDAY could not safely open a recovery folder.".to_string())?;
    match crate::coding_sandbox_fs::read_bounded_file_from_directory_handle(
        &directory, name, limit, MAX_SCANNED_ENTRIES,
    ) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err("A recovery item changed, was redirected, or exceeded its safety limits. Recovery evidence was preserved.".into()),
    }
}

fn safe_recovery_target(root: &Path, record: &CodingUndoRecord) -> Result<PathBuf, String> {
    validate_undo_record(record)?;
    let canonical_root = validate_granted_root(root)?;
    let target = canonical_root.join(&record.relative_path);
    if !supported_project_file(&target) {
        return Err("A recovery journal points to a file type outside the Coding Assistant allowlist.".into());
    }
    let parent = target.parent().ok_or("A recovery target has no parent folder.")?;
    let parent_meta = fs::symlink_metadata(parent)
        .map_err(|_| "The recovery target folder is missing. Recovery copies were preserved.".to_string())?;
    if is_redirected(&parent_meta) || !parent_meta.is_dir() {
        return Err("A recovery target folder is redirected or is not a normal folder.".into());
    }
    let canonical_parent = fs::canonicalize(parent)
        .map_err(|_| "FRIDAY could not validate a recovery target folder.".to_string())?;
    if !canonical_parent.starts_with(&canonical_root) {
        return Err("A recovery target folder resolves outside the selected project.".into());
    }
    match fs::symlink_metadata(&target) {
        Ok(meta) if is_redirected(&meta) || !meta.is_file() || meta.len() == 0 || meta.len() > MAX_FILE_BYTES =>
            return Err("A recovery target is not a safe bounded regular file. Recovery copies were preserved.".into()),
        Ok(_) => {},
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
        Err(_) => return Err("FRIDAY could not inspect the recovery target. Recovery copies were preserved.".into()),
    }
    Ok(target)
}

fn recovery_hash(path: &Path, expected_limit: u64) -> Result<Option<String>, String> {
    Ok(read_recovery_file(path, expected_limit)?.map(|bytes| sha256_hex(&bytes)))
}

fn remove_recovery_copy(path: &Path, expected_hash: &str) -> Result<(), String> {
    match recovery_hash(path, MAX_FILE_BYTES.max(MAX_PROPOSED_FILE_BYTES as u64))? {
        None => Ok(()),
        Some(actual) if actual == expected_hash => fs::remove_file(path)
            .map_err(|_| "FRIDAY could not safely clean up a verified recovery copy.".to_string()),
        Some(_) => Err("A temporary recovery copy has unexpected contents. FRIDAY preserved it for manual review.".into()),
    }
}

#[cfg(unix)]
fn original_unix_mode(metadata: &fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    Some(metadata.permissions().mode() & 0o777)
}

#[cfg(not(unix))]
fn original_unix_mode(_metadata: &fs::Metadata) -> Option<u32> {
    None
}

fn apply_recovery_permissions(path: &Path, record: &CodingUndoRecord) -> Result<(), String> {
    #[cfg(unix)]
    if let Some(mode) = record.original_unix_mode {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode & 0o777))
            .map_err(|_| "FRIDAY could not restore the original file permissions.".to_string())?;
    }
    #[cfg(windows)]
    if let Some(ref dacl) = record.original_windows_dacl {
        restore_windows_dacl(path, dacl)?;
    }
    #[cfg(not(any(unix, windows)))]
    let _ = (path, record);
    Ok(())
}

fn write_recovery_copy(path: &Path, bytes: &[u8], expected_hash: &str, record: &CodingUndoRecord) -> Result<(), String> {
    if bytes.is_empty() || bytes.len() as u64 > MAX_FILE_BYTES || sha256_hex(bytes) != expected_hash {
        return Err("The verified recovery source failed its integrity check.".into());
    }
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)
        .map_err(|_| "FRIDAY could not create a new recovery temporary file.".to_string())?;
    file.write_all(bytes)
        .map_err(|_| "FRIDAY could not write a recovery temporary file.".to_string())?;
    drop(file);
    apply_recovery_permissions(path, record)?;
    let file = OpenOptions::new().write(true).open(path)
        .map_err(|_| "FRIDAY could not reopen a recovery temporary file for flushing.".to_string())?;
    file.sync_all().map_err(|_| "FRIDAY could not flush a recovery temporary file.".to_string())?;
    drop(file);
    let installed = read_recovery_file(path, MAX_FILE_BYTES)?
        .ok_or("The recovery temporary file disappeared.")?;
    if sha256_hex(&installed) != expected_hash {
        return Err("The recovery temporary file failed SHA-256 verification.".into());
    }
    Ok(())
}

fn inspect_recovery_undo_state(backup_dir: &Path, expected: &CodingUndoRecord) -> Result<Option<bool>, String> {
    let active = backup_dir.join(format!("{}.undo.json", expected.id));
    let consumed = backup_dir.join(format!("{}.undone.json", expected.id));
    let active_exists = match fs::symlink_metadata(&active) {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => return Err("FRIDAY could not inspect the active Undo record. Recovery evidence was preserved.".into()),
    };
    let consumed_exists = match fs::symlink_metadata(&consumed) {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => return Err("FRIDAY could not inspect the consumed Undo record. Recovery evidence was preserved.".into()),
    };
    if active_exists && consumed_exists {
        return Err("Both active and consumed Undo records exist. Manual review is required.".into());
    }
    let selected = if active_exists { &active } else if consumed_exists { &consumed } else { return Ok(None); };
    let bytes = read_recovery_file(selected, 16 * 1024)?
        .ok_or("An Undo state record disappeared during recovery.")?;
    let saved: CodingUndoRecord = serde_json::from_slice(&bytes)
        .map_err(|_| "An Undo state record is malformed. Recovery evidence was preserved.".to_string())?;
    validate_undo_record(&saved)?;
    if saved.id != expected.id
        || saved.relative_path != expected.relative_path
        || saved.backup_relative_path != expected.backup_relative_path
        || saved.original_sha256 != expected.original_sha256
        || saved.resulting_sha256 != expected.resulting_sha256
        || saved.created_at != expected.created_at
        || saved.original_windows_dacl != expected.original_windows_dacl {
        return Err("An Undo state record does not match its transaction journal. Recovery evidence was preserved.".into());
    }
    Ok(Some(active_exists))
}

fn recover_undo_transaction(root: &Path, backup_dir: &Path, record: &CodingUndoRecord) -> Result<(), String> {
    validate_undo_record(record)?;
    let canonical_root = validate_granted_root(root)?;
    let target = safe_recovery_target(&canonical_root, record)?;
    let (project_handle, target_name, project_identity) = open_verified_code_parent(&canonical_root, &target)?;
    let backup_handle = crate::coding_sandbox_fs::open_verified_root_directory(backup_dir)
        .map_err(|_| "FRIDAY could not safely open the recovery folder. Recovery evidence was preserved.".to_string())?;
    let backup_identity = crate::coding_sandbox_fs::verified_directory_handle_identity(&backup_handle)
        .map_err(|_| "FRIDAY could not verify the recovery folder identity.".to_string())?;

    let backup_name = OsString::from(format!("{}.bak", record.id));
    let backup_bytes = crate::coding_sandbox_fs::read_bounded_file_from_directory_handle(
        &backup_handle, &backup_name, MAX_FILE_BYTES, MAX_CODE_BACKUP_FILES,
    ).map_err(|_| "The original backup for an interrupted Undo is missing or unsafe. Recovery files were preserved.".to_string())?;
    if sha256_hex(&backup_bytes) != record.original_sha256 {
        return Err("The original backup for an interrupted Undo failed SHA-256 verification. Recovery files were preserved.".into());
    }

    let displaced_name = OsString::from(format!(".friday-code-undone-{}.tmp", record.id));
    let temporary_name = OsString::from(format!(".friday-code-undo-{}.tmp", record.id));
    let journal_name = OsString::from(format!("{}.undo-txn.json", record.id));
    let recovered_journal_name = OsString::from(format!("{}.undo-recovered.json", record.id));

    let read_optional = |parent: &File, name: &OsStr, limit: u64| -> Result<Option<Vec<u8>>, String> {
        match crate::coding_sandbox_fs::read_bounded_file_from_directory_handle(
            parent, name, limit, MAX_SCANNED_ENTRIES,
        ) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err("A recovery item changed identity, was redirected, or exceeded its safety limits. Recovery evidence was preserved.".into()),
        }
    };
    let target_bytes = read_optional(&project_handle, &target_name, MAX_FILE_BYTES)?;
    let displaced_bytes = read_optional(&project_handle, &displaced_name, MAX_FILE_BYTES)?;
    let temporary_bytes = read_optional(&project_handle, &temporary_name, MAX_FILE_BYTES)?;
    let target_hash = target_bytes.as_ref().map(|bytes| sha256_hex(bytes));
    let displaced_hash = displaced_bytes.as_ref().map(|bytes| sha256_hex(bytes));
    let temporary_hash = temporary_bytes.as_ref().map(|bytes| sha256_hex(bytes));

    let active_name = OsString::from(format!("{}.undo.json", record.id));
    let consumed_name = OsString::from(format!("{}.undone.json", record.id));
    let consume_active_record = || -> Result<(), String> {
        let identity = verified_code_child_identity(&backup_handle, &active_name)?;
        crate::coding_sandbox_fs::rename_file_noreplace(
            &backup_handle, &active_name, &consumed_name, backup_identity, identity,
        ).map_err(|_| "FRIDAY could not safely mark the recovered Undo as consumed. Recovery evidence was preserved.".to_string())
    };

    match target_hash.as_deref() {
        Some(hash) if hash == record.original_sha256 => {
            // Windows ReplaceFileW can retain the replaced file's DACL. Reapply
            // the recorded permissions only after verifying the target content.
            apply_recovery_permissions(&target, record)?;
            if let Some(hash) = displaced_hash.as_deref() {
                if hash != record.resulting_sha256 { return Err("An interrupted Undo left an unexpected displaced file. Nothing was removed.".into()); }
            }
            if let Some(hash) = temporary_hash.as_deref() {
                if hash != record.original_sha256 { return Err("An interrupted Undo left an unexpected temporary file. Nothing was removed.".into()); }
            }
            match inspect_recovery_undo_state(backup_dir, record)? {
                Some(true) => consume_active_record()?,
                Some(false) => {},
                None => return Err("The interrupted Undo has no valid active or consumed Undo record. Recovery evidence was preserved.".into()),
            }
        }
        Some(hash) if hash == record.resulting_sha256 => {
            if inspect_recovery_undo_state(backup_dir, record)? != Some(true) {
                return Err("The edited target does not have one valid active Undo record. Recovery evidence was preserved.".into());
            }
            if displaced_hash.is_some() {
                return Err("An interrupted Undo has both the edited target and a displaced copy. FRIDAY preserved all files for manual review.".into());
            }
            if let Some(hash) = temporary_hash.as_deref() {
                if hash != record.original_sha256 { return Err("An interrupted Undo temporary file has unexpected contents. Recovery evidence was preserved.".into()); }
            }
        }
        None => {
            if let Some(hash) = displaced_hash.as_deref() {
                if inspect_recovery_undo_state(backup_dir, record)? != Some(true) {
                    return Err("The displaced edited file does not have one valid active Undo record. Recovery evidence was preserved.".into());
                }
                if hash != record.resulting_sha256 { return Err("The displaced file from an interrupted Undo failed verification.".into()); }
                let identity = verified_code_child_identity(&project_handle, &displaced_name)?;
                crate::coding_sandbox_fs::rename_file_noreplace(
                    &project_handle, &displaced_name, &target_name, project_identity, identity,
                ).map_err(|_| "FRIDAY could not restore the edited file without replacing an unexpected target. Recovery copies were preserved.".to_string())?;
            } else if temporary_hash.as_deref() == Some(record.original_sha256.as_str()) {
                let undo_state = inspect_recovery_undo_state(backup_dir, record)?
                    .ok_or("The interrupted Undo has no valid Undo state record. Recovery evidence was preserved.")?;
                let identity = verified_code_child_identity(&project_handle, &temporary_name)?;
                crate::coding_sandbox_fs::rename_file_noreplace(
                    &project_handle, &temporary_name, &target_name, project_identity, identity,
                ).map_err(|_| "FRIDAY could not finish restoring the verified original without replacing an unexpected target.".to_string())?;
                if undo_state { consume_active_record()?; }
            } else {
                return Err("The target and displaced copy are missing after an interrupted Undo. FRIDAY preserved the verified backup; manual recovery is required.".into());
            }
        }
        Some(_) => return Err("The project file matches neither side of the interrupted Undo. FRIDAY preserved every recovery copy.".into()),
    }

    // Recovery copies are intentionally retained as evidence. The journal is
    // archived by a no-replace handle-relative rename instead of being unlinked.
    let journal_identity = verified_code_child_identity(&backup_handle, &journal_name)?;
    crate::coding_sandbox_fs::rename_file_noreplace(
        &backup_handle, &journal_name, &recovered_journal_name, backup_identity, journal_identity,
    ).map_err(|_| "FRIDAY reconciled the Undo state but could not safely archive its recovery journal. All recovery evidence was preserved.".to_string())?;
    Ok(())
}

fn recover_apply_transaction(root: &Path, backup_dir: &Path, record: &CodingUndoRecord) -> Result<(), String> {
    validate_undo_record(record)?;
    let target = safe_recovery_target(root, record)?;
    let backup = backup_dir.join(format!("{}.bak", record.id));
    let original = read_recovery_file(&backup, MAX_FILE_BYTES)?
        .ok_or("The original backup for an interrupted Apply is missing. Recovery files were preserved.")?;
    if sha256_hex(&original) != record.original_sha256 {
        return Err("The original backup for an interrupted Apply failed SHA-256 verification. Recovery files were preserved.".into());
    }
    let target_hash = recovery_hash(&target, MAX_FILE_BYTES)?;
    let temporary = target.parent().unwrap().join(format!(".friday-code-edit-{}.tmp", record.id));
    let displaced = target.parent().unwrap().join(format!(".friday-code-original-{}.tmp", record.id));
    let temporary_hash = recovery_hash(&temporary, MAX_PROPOSED_FILE_BYTES as u64)?;
    let displaced_hash = recovery_hash(&displaced, MAX_FILE_BYTES)?;

    match target_hash.as_deref() {
        Some(hash) if hash == record.resulting_sha256 => {
            if let Some(hash) = displaced_hash.as_deref() {
                if hash != record.original_sha256 { return Err("The displaced original from an interrupted Apply failed verification.".into()); }
            }
            if let Some(hash) = temporary_hash.as_deref() {
                if hash != record.resulting_sha256 { return Err("The replacement temporary file from an interrupted Apply has unexpected contents.".into()); }
            }
            let undo_path = backup_dir.join(format!("{}.undo.json", record.id));
            match inspect_recovery_undo_state(backup_dir, record)? {
                Some(true) => {},
                Some(false) => return Err("A completed Apply has already-consumed Undo state. Recovery evidence was preserved.".into()),
                None => {
                    let bytes = serde_json::to_vec(record).map_err(|_| "Could not prepare a recovered Undo record.".to_string())?;
                    let mut file = OpenOptions::new().write(true).create_new(true).open(&undo_path)
                        .map_err(|_| "FRIDAY could not create the missing Undo record after Apply recovery.".to_string())?;
                    file.write_all(&bytes).and_then(|_| file.sync_all())
                        .map_err(|_| "FRIDAY could not flush the recovered Undo record.".to_string())?;
                }
            }
            if displaced_hash.is_some() { remove_recovery_copy(&displaced, &record.original_sha256)?; }
            if temporary_hash.is_some() { remove_recovery_copy(&temporary, &record.resulting_sha256)?; }
        }
        Some(hash) if hash == record.original_sha256 => {
            if let Some(hash) = displaced_hash.as_deref() {
                if hash != record.original_sha256 { return Err("An interrupted Apply left an unexpected displaced file. Recovery copies were preserved.".into()); }
                remove_recovery_copy(&displaced, &record.original_sha256)?;
            }
            if let Some(hash) = temporary_hash.as_deref() {
                if hash != record.resulting_sha256 { return Err("An interrupted Apply left an unexpected replacement temporary file.".into()); }
                remove_recovery_copy(&temporary, &record.resulting_sha256)?;
            }
        }
        None => {
            if displaced_hash.as_deref() == Some(record.original_sha256.as_str()) {
                fs::rename(&displaced, &target)
                    .map_err(|_| "FRIDAY could not restore the displaced original after interrupted Apply. The backup remains available.".to_string())?;
                if temporary_hash.is_some() { remove_recovery_copy(&temporary, &record.resulting_sha256)?; }
            } else if displaced_hash.is_none() {
                let restore_temp = target.parent().unwrap().join(format!(".friday-code-recovery-{}.tmp", record.id));
                write_recovery_copy(&restore_temp, &original, &record.original_sha256, record)?;
                fs::rename(&restore_temp, &target)
                    .map_err(|_| "FRIDAY could not reinstall the verified original backup. The backup remains available.".to_string())?;
                if temporary_hash.is_some() { remove_recovery_copy(&temporary, &record.resulting_sha256)?; }
            } else {
                return Err("The displaced original from interrupted Apply failed verification. All copies were preserved.".into());
            }
        }
        Some(_) => return Err("The project file has contents that match neither side of an interrupted Apply. FRIDAY preserved every recovery copy.".into()),
    }
    fs::remove_file(backup_dir.join(format!("{}.txn.json", record.id)))
        .map_err(|_| "FRIDAY resolved the Apply transaction but could not remove its journal.".to_string())?;
    Ok(())
}

fn recover_project_code_transactions(root: &Path) -> Result<usize, String> {
    let backup_dir = root.join(".friday-code-backups");
    let meta = match fs::symlink_metadata(&backup_dir) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(_) => return Err("FRIDAY could not inspect the project recovery folder.".into()),
    };
    if is_redirected(&meta) || !meta.is_dir() {
        return Err("The project recovery folder is redirected or unsafe. No automatic recovery was attempted.".into());
    }
    let canonical = fs::canonicalize(&backup_dir)
        .map_err(|_| "FRIDAY could not validate the project recovery folder.".to_string())?;
    if !canonical.starts_with(root) { return Err("The project recovery folder resolves outside the selected project.".into()); }
    let mut undo_journals = Vec::new();
    let mut apply_journals = Vec::new();
    for entry in fs::read_dir(&canonical).map_err(|_| "FRIDAY could not list project recovery records.".to_string())? {
        let entry = entry.map_err(|_| "FRIDAY could not inspect a project recovery record.".to_string())?;
        let name = entry.file_name().to_string_lossy().to_string();
        if name.ends_with(".undo-txn.json") { undo_journals.push(entry.path()); }
        else if name.ends_with(".txn.json") { apply_journals.push(entry.path()); }
        if undo_journals.len().saturating_add(apply_journals.len()) > MAX_CODE_BACKUP_FILES {
            return Err("The project contains too many recovery journals for automatic processing. Manual review is required.".into());
        }
    }
    undo_journals.sort();
    apply_journals.sort();
    let mut recovered = 0usize;
    for (paths, is_undo) in [(undo_journals, true), (apply_journals, false)] {
        for journal in paths {
            let bytes = read_recovery_file(&journal, 16 * 1024)?
                .ok_or("A recovery journal disappeared during recovery.")?;
            let record: CodingUndoRecord = serde_json::from_slice(&bytes)
                .map_err(|_| "A recovery journal is invalid or damaged. Recovery files were preserved.".to_string())?;
            validate_undo_record(&record)?;
            let expected_name = if is_undo { format!("{}.undo-txn.json", record.id) } else { format!("{}.txn.json", record.id) };
            if journal.file_name().and_then(|v| v.to_str()) != Some(expected_name.as_str()) {
                return Err("A recovery journal filename does not match its transaction identifier.".into());
            }
            if is_undo { recover_undo_transaction(root, &canonical, &record)?; }
            else { recover_apply_transaction(root, &canonical, &record)?; }
            recovered = recovered.saturating_add(1);
        }
    }
    Ok(recovered)
}

fn undo_code_change_on_disk(root: &Path, record: &CodingUndoRecord) -> Result<(PathBuf, PathBuf), String> {
    validate_undo_record(record)?;
    let canonical_root = validate_granted_root(root)?;
    let target = canonical_root.join(Path::new(&record.relative_path));
    let metadata = fs::symlink_metadata(&target)
        .map_err(|_| "The edited project file is no longer available. No undo was performed.".to_string())?;
    if is_redirected(&metadata) || !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_FILE_BYTES {
        return Err("Undo is allowed only for the original bounded regular project file.".into());
    }
    if metadata.permissions().readonly() || !supported_project_file(&target) {
        return Err("The project file is read-only or is not an allowed code file. Undo was refused.".into());
    }
    let canonical_target = fs::canonicalize(&target)
        .map_err(|_| "FRIDAY could not revalidate the edited project file.".to_string())?;
    if canonical_target != target || !canonical_target.starts_with(&canonical_root) {
        return Err("The edited project file moved or was redirected. Undo was refused.".into());
    }

    // Pin the target's containing directory before reading or changing any child.
    // All installation and rollback renames below are relative to this handle.
    let parent = canonical_target.parent().ok_or("The edited file has no parent directory.")?;
    let (parent_handle, current_name, parent_identity) =
        open_verified_code_parent(&canonical_root, &canonical_target)?;
    let original_identity = verified_code_child_identity(&parent_handle, &current_name)?;
    let mut current_file = crate::coding_sandbox_fs::open_file_nofollow(&parent_handle, &current_name)
        .map_err(|_| "FRIDAY could not safely open the current edited file.".to_string())?;
    if !crate::coding_sandbox_fs::runtime_tree_handle_matches(original_identity, &current_file)
        .map_err(|_| "Could not verify the current edited file identity.".to_string())? {
        return Err("The edited project file changed identity. Undo was refused.".into());
    }
    let current_meta = current_file.metadata()
        .map_err(|_| "Could not inspect the current edited file.".to_string())?;
    if !current_meta.is_file() || current_meta.len() != metadata.len()
        || current_meta.len() == 0 || current_meta.len() > MAX_FILE_BYTES {
        return Err("The current edited file is no longer a bounded regular file. Undo was refused.".into());
    }
    let mut current = Vec::new();
    current_file.take(MAX_FILE_BYTES + 1).read_to_end(&mut current)
        .map_err(|_| "FRIDAY could not read the current edited file through its verified handle.".to_string())?;
    if current.len() as u64 != current_meta.len() || current.contains(&0)
        || std::str::from_utf8(&current).is_err() || sha256_hex(&current) != record.resulting_sha256 {
        return Err("This file has changed since FRIDAY edited it. Undo will not overwrite your newer changes.".into());
    }

    let backup_dir = ensure_code_backup_dir(&canonical_root)?;
    let backup = backup_dir.join(format!("{}.bak", record.id));
    let backup_name = format!("{}.bak", record.id);
    let backup_handle = crate::coding_sandbox_fs::open_verified_root_directory(&backup_dir)
        .map_err(|_| "FRIDAY could not safely open its code backup folder.".to_string())?;
    let original = crate::coding_sandbox_fs::read_bounded_file_from_directory_handle(
        &backup_handle, OsStr::new(&backup_name), MAX_FILE_BYTES, MAX_CODE_BACKUP_FILES,
    ).map_err(|_| "FRIDAY could not safely read the original backup. Undo was refused.".to_string())?;
    if original.contains(&0) || std::str::from_utf8(&original).is_err()
        || sha256_hex(&original) != record.original_sha256 {
        return Err("The original backup failed integrity verification. Undo was refused.".into());
    }

    let journal_path = backup_dir.join(format!("{}.undo-txn.json", record.id));
    let journal_bytes = serde_json::to_vec(record)
        .map_err(|_| "FRIDAY could not prepare the undo recovery journal.".to_string())?;
    let mut journal = crate::coding_sandbox_fs::create_destination_entry(
        &backup_handle,
        journal_path.file_name().ok_or("The Undo journal filename is invalid.")?,
        false,
    ).map_err(|e| format!("Could not create the handle-relative Undo recovery journal: {e}"))?;
    journal.write_all(&journal_bytes).and_then(|_| journal.sync_all())
        .map_err(|e| format!("Could not flush the undo recovery journal: {e}"))?;
    drop(journal);

    let temporary = parent.join(format!(".friday-code-undo-{}.tmp", record.id));
    let mut temp_file = crate::coding_sandbox_fs::create_destination_entry(
        &parent_handle,
        temporary.file_name().ok_or("The Undo temporary filename is invalid.")?,
        false,
    ).map_err(|e| format!("Could not prepare the handle-relative original-file restoration: {e}"))?;
    if let Err(error) = temp_file.set_permissions(metadata.permissions())
        .and_then(|_| temp_file.write_all(&original))
        .and_then(|_| temp_file.sync_all()) {
        drop(temp_file);
        let _ = fs::remove_file(&temporary);
        return Err(format!("Could not prepare the original-file restoration: {error}"));
    }
    drop(temp_file);
    // Restore the original DACL/mode on the prepared object before it becomes
    // the project entry. The subsequent rename does not inherit target ACLs.
    apply_recovery_permissions(&temporary, record)?;
    let temporary_name = temporary.file_name().ok_or("The Undo temporary filename is invalid.")?;
    let prepared = crate::coding_sandbox_fs::read_bounded_file_from_directory_handle(
        &parent_handle, temporary_name, MAX_FILE_BYTES, MAX_SCANNED_ENTRIES,
    ).map_err(|_| "Could not verify the prepared original-file restoration through its verified parent directory.".to_string())?;
    if sha256_hex(&prepared) != record.original_sha256 {
        return Err("The prepared original-file restoration failed SHA-256 verification.".into());
    }
    let temporary_identity = verified_code_child_identity(&parent_handle, temporary_name)?;

    let displaced = parent.join(format!(".friday-code-undone-{}.tmp", record.id));
    let displaced_name = displaced.file_name().ok_or("The displaced-file name is invalid.")?;

    // Use the same no-replace, identity-checked rename protocol on Windows and
    // Unix. Never let an unexpected destination be overwritten during Undo.
    crate::coding_sandbox_fs::rename_file_noreplace(
        &parent_handle, &current_name, displaced_name, parent_identity, original_identity,
    ).map_err(|error| format!(
        "Could not stage the verified edited file without replacing another entry: {error}. The Undo journal and backup were retained."
    ))?;

    if let Err(error) = crate::coding_sandbox_fs::rename_file_noreplace(
        &parent_handle, temporary_name, current_name.as_os_str(), parent_identity, temporary_identity,
    ) {
        let restored = crate::coding_sandbox_fs::rename_file_noreplace(
            &parent_handle, displaced_name, &current_name, parent_identity, original_identity,
        ).is_ok();
        return Err(if restored {
            format!("Could not install the original file; the edited file was restored: {error}. Recovery files were retained.")
        } else {
            format!("Undo installation and automatic rollback both failed. The edited file remains at {} and the original backup remains at {}: {error}",
                displaced.display(), backup.display())
        });
    }

    if let Err(error) = verify_installed_code_file(
        &parent_handle, &current_name, temporary_identity, &record.original_sha256,
    ) {
        let failed_name = format!(".friday-code-undo-failed-{}.tmp", record.id);
        let failed_preserved = crate::coding_sandbox_fs::rename_file_noreplace(
            &parent_handle, &current_name, OsStr::new(&failed_name), parent_identity, temporary_identity,
        ).is_ok();
        let rolled_back = crate::coding_sandbox_fs::rename_file_noreplace(
            &parent_handle, displaced_name, &current_name, parent_identity, original_identity,
        ).is_ok();
        return Err(if rolled_back {
            format!("The restored file failed identity/content verification ({error}); FRIDAY put the edited file back.")
        } else {
            format!("The restored file failed verification and rollback could not be proven safe. The edited file remains at {} and the failed restoration was {}. The original backup remains at {}.",
                displaced.display(), if failed_preserved { failed_name } else { "the project entry".into() }, backup.display())
        });
    }

    // Mark the Undo as consumed with a no-replace rename below the pinned
    // backup-directory handle, so a competing record cannot be overwritten.
    let backup_identity = crate::coding_sandbox_fs::verified_directory_handle_identity(&backup_handle)
        .map_err(|_| "Could not verify the code backup folder before recording Undo completion.".to_string())?;
    let undo_name = OsStr::new(&format!("{}.undo.json", record.id)).to_os_string();
    let consumed_name = OsStr::new(&format!("{}.undone.json", record.id)).to_os_string();
    let undo_identity = verified_code_child_identity(&backup_handle, &undo_name)?;
    crate::coding_sandbox_fs::rename_file_noreplace(
        &backup_handle, &undo_name, &consumed_name, backup_identity, undo_identity,
    ).map_err(|e| format!("The original was restored and verified, but FRIDAY could not safely mark Undo as consumed: {e}. The recovery journal and copies were retained."))?;

    // Cleanup remains deliberately conservative in this stage: retain the
    // displaced edited copy as recovery evidence rather than unlinking a name
    // after its identity check. A later cleanup primitive will own safe removal.
    fs::remove_file(&journal_path)
        .map_err(|e| format!("The original was restored and Undo was consumed, but FRIDAY could not clean up the resolved transaction journal: {e}. Recovery will reconcile it before the next edit."))?;
    Ok((canonical_target, backup))
}

fn take_approved_preview(
    previews: &mut BTreeMap<String, StoredCodePreview>,
    preview_id: &str,
    approval_token: &str,
    now: u64,
) -> Result<StoredCodePreview, String> {
    let preview = previews.remove(preview_id)
        .ok_or("This code preview has expired or was already used. Create it again.")?;
    if preview.expires_at <= now {
        return Err("This code preview has expired. Create a fresh preview.".into());
    }
    if preview.approval_token.as_deref() != Some(approval_token) || approval_token.is_empty() {
        return Err("The exact code preview was not approved with a valid token.".into());
    }
    Ok(preview)
}

fn current_epoch_seconds() -> Result<u64, String> {
    SystemTime::now().duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| "The system clock is invalid; FRIDAY cannot create a safe preview.".to_string())
}

fn sha256_hex(content: &[u8]) -> String {
    let digest = Sha256::digest(content);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

// A bounded single-hunk unified diff with up to three context lines on each side.
// This preview function never writes to disk.
fn unified_diff(original: &str, proposed: &str) -> String {
    let old_lines: Vec<&str> = original.split('\n').collect();
    let new_lines: Vec<&str> = proposed.split('\n').collect();
    let mut prefix = 0usize;
    while prefix < old_lines.len().min(new_lines.len()) && old_lines[prefix] == new_lines[prefix] { prefix += 1; }
    let mut suffix = 0usize;
    while suffix < old_lines.len().saturating_sub(prefix).min(new_lines.len().saturating_sub(prefix))
        && old_lines[old_lines.len() - 1 - suffix] == new_lines[new_lines.len() - 1 - suffix] { suffix += 1; }

    let old_end = old_lines.len().saturating_sub(suffix);
    let new_end = new_lines.len().saturating_sub(suffix);
    let prefix_context = prefix.min(3);
    let suffix_context = suffix.min(3);
    let old_count = prefix_context + old_end.saturating_sub(prefix) + suffix_context;
    let new_count = prefix_context + new_end.saturating_sub(prefix) + suffix_context;
    let start_line = prefix - prefix_context + 1;
    let mut output = format!("@@ -{},{} +{},{} @@\n", start_line, old_count, start_line, new_count);

    for line in &old_lines[prefix - prefix_context..prefix] {
        output.push(' ');
        output.push_str(line);
        output.push('\n');
    }
    for line in &old_lines[prefix..old_end] {
        output.push('-');
        output.push_str(line);
        output.push('\n');
    }
    for line in &new_lines[prefix..new_end] {
        output.push('+');
        output.push_str(line);
        output.push('\n');
    }
    for line in &old_lines[old_end..old_end + suffix_context] {
        output.push(' ');
        output.push_str(line);
        output.push('\n');
    }
    output
}

fn make_token() -> String {
    let mut bytes = [0u8; 24];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn is_redirected(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
        return metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0;
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn ignored_directory(name: &str) -> bool {
    name.starts_with('.') || matches!(
        name.to_ascii_lowercase().as_str(),
        "node_modules" | "target" | "dist" | "build" | "out" | "vendor"
            | "venv" | ".venv" | "__pycache__" | ".next" | ".cache"
            | "coverage" | "bin" | "obj" | "packages" | "secrets" | "credentials"
            | "private" | "private-keys" | "certs" | "certificates"
    )
}

fn sensitive_file_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower == ".env"
        || lower.starts_with(".env.")
        || lower.contains("secret")
        || lower.contains("credential")
        || lower.starts_with("secret.")
        || lower.starts_with("token.")
        || lower.starts_with("auth.")
        || lower.starts_with("service-account.")
        || lower.ends_with(".pem")
        || lower.ends_with(".key")
        || lower.ends_with(".p12")
        || lower.ends_with(".pfx")
        || lower.ends_with(".jks")
        || lower.ends_with(".keystore")
        || lower.ends_with(".asc")
        || lower.ends_with(".crt")
        || lower.ends_with(".cer")
        || lower == "id_rsa"
        || lower == "id_ed25519"
}

fn supported_project_file(path: &Path) -> bool {
    let name = path.file_name().and_then(|value| value.to_str()).unwrap_or("");
    if sensitive_file_name(name) {
        return false;
    }
    const SPECIAL_NAMES: &[&str] = &[
        "cargo.toml", "cargo.lock", "package.json", "tsconfig.json",
        "vite.config.ts", "vite.config.js", "pyproject.toml", "requirements.txt",
        "go.mod", "go.sum", "pom.xml", "build.gradle", "build.gradle.kts",
        "makefile", "dockerfile", "readme", "readme.md", "readme.txt",
    ];
    if SPECIAL_NAMES.contains(&name.to_ascii_lowercase().as_str()) {
        return true;
    }
    const EXTENSIONS: &[&str] = &[
        "rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "py", "pyi", "go",
        "java", "kt", "kts", "c", "h", "cc", "cpp", "hpp", "cs", "fs",
        "php", "rb", "swift", "sh", "ps1", "bat", "cmd", "html", "htm",
        "css", "scss", "sass", "less", "sql", "json", "jsonc", "yaml",
        "yml", "toml", "xml", "md", "mdx", "txt", "ini", "cfg", "gradle",
        "vue", "svelte", "astro", "graphql", "gql", "proto",
    ];
    path.extension().and_then(|extension| extension.to_str())
        .is_some_and(|extension| EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str()))
}

fn language_for(path: &Path) -> String {
    let extension = path.extension().and_then(|value| value.to_str()).unwrap_or("").to_ascii_lowercase();
    match extension.as_str() {
        "rs" => "Rust", "ts" | "tsx" => "TypeScript", "js" | "jsx" | "mjs" | "cjs" => "JavaScript",
        "py" | "pyi" => "Python", "go" => "Go", "java" => "Java", "kt" | "kts" => "Kotlin",
        "c" | "h" | "cc" | "cpp" | "hpp" => "C/C++", "cs" => "C#", "php" => "PHP",
        "rb" => "Ruby", "swift" => "Swift", "html" | "htm" => "HTML", "css" | "scss" | "sass" | "less" => "CSS",
        "sql" => "SQL", "json" | "jsonc" => "JSON", "yaml" | "yml" => "YAML", "toml" => "TOML",
        "md" | "mdx" => "Markdown", "sh" | "ps1" | "bat" | "cmd" => "Shell", "xml" => "XML",
        "vue" | "svelte" | "astro" => "Web component", _ => "Text/config",
    }.to_string()
}

fn relative_path(root: &Path, path: &Path) -> Option<String> {
    path.strip_prefix(root).ok().map(|relative| {
        relative.components().map(|part| part.as_os_str().to_string_lossy().to_string()).collect::<Vec<_>>().join("/")
    })
}

fn scan_folder(
    current: &std::fs::File,
    relative_directory: &Path,
    root: &Path,
    root_identity: (u64, u64),
    depth: usize,
    scanned: &mut usize,
    candidates: &mut Vec<CodingFileCandidate>,
    grants: &mut BTreeMap<String, ProjectGrant>,
) -> Result<(), String> {
    scan_folder_with_hook(
        current, relative_directory, root, root_identity, depth, scanned, candidates, grants, &mut |_| Ok(()),
    )
}

fn scan_folder_with_hook<H>(
    current: &std::fs::File,
    relative_directory: &Path,
    root: &Path,
    root_identity: (u64, u64),
    depth: usize,
    scanned: &mut usize,
    candidates: &mut Vec<CodingFileCandidate>,
    grants: &mut BTreeMap<String, ProjectGrant>,
    before_entry_open: &mut H,
) -> Result<(), String>
where
    H: FnMut(&Path) -> Result<(), String>,
{
    if depth > MAX_PROJECT_DEPTH || candidates.len() >= MAX_PROJECT_CANDIDATES || *scanned >= MAX_SCANNED_ENTRIES {
        return Ok(());
    }
    let entries = match crate::coding_sandbox_fs::enumerate_runtime_tree_entries(current, MAX_SCANNED_ENTRIES) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
            *scanned = MAX_SCANNED_ENTRIES;
            return Ok(());
        }
        Err(_) => return Err("FRIDAY could not safely inspect part of the selected project folder.".into()),
    };
    for entry in entries {
        if *scanned >= MAX_SCANNED_ENTRIES || candidates.len() >= MAX_PROJECT_CANDIDATES { break; }
        *scanned = scanned.saturating_add(1);
        let name = entry.name;
        let Some(name_text) = name.to_str() else { continue; };
        if entry.is_reparse { continue; }
        let relative = relative_directory.join(&name);
        if entry.is_directory {
            if depth >= MAX_PROJECT_DEPTH || ignored_directory(name_text) { continue; }
            before_entry_open(&relative)?;
            let child = crate::coding_sandbox_fs::open_child_directory_nofollow(current, &name)
                .map_err(|_| "A project folder changed while FRIDAY was scanning it; scan again.".to_string())?;
            let metadata = child.metadata()
                .map_err(|_| "FRIDAY could not verify a project subfolder.".to_string())?;
            if !metadata.is_dir() || is_redirected(&metadata)
                || !crate::coding_sandbox_fs::runtime_tree_handle_matches(entry.identity, &child)
                    .map_err(|_| "FRIDAY could not verify a project subfolder.".to_string())?
            {
                return Err("A project folder changed while FRIDAY was scanning it; scan again.".into());
            }
            scan_folder_with_hook(&child, &relative, root, root_identity, depth + 1, scanned, candidates, grants, before_entry_open)?;
            continue;
        }
        if !entry.is_file || !supported_project_file(&relative) { continue; }
        before_entry_open(&relative)?;
        let file = crate::coding_sandbox_fs::open_file_nofollow(current, &name)
            .map_err(|_| "A project file changed while FRIDAY was scanning it; scan again.".to_string())?;
        let metadata = file.metadata()
            .map_err(|_| "FRIDAY could not verify a project file.".to_string())?;
        if !metadata.is_file() || is_redirected(&metadata)
            || !crate::coding_sandbox_fs::runtime_tree_handle_matches(entry.identity, &file)
                .map_err(|_| "FRIDAY could not verify a project file.".to_string())?
            || !crate::coding_sandbox_fs::has_single_hard_link(&file)
        {
            return Err("A project file changed or has an unsafe filesystem link; scan again.".into());
        }
        if metadata.len() == 0 || metadata.len() > MAX_FILE_BYTES { continue; }
        let candidate_path = root.join(&relative);
        let relative_text = relative.components().map(|part| part.as_os_str().to_string_lossy().to_string()).collect::<Vec<_>>().join("/");
        let id = make_token();
        candidates.push(CodingFileCandidate {
            id: id.clone(), name: name_text.to_string(), relative_path: relative_text,
            size_bytes: metadata.len(), language: language_for(&relative),
        });
        grants.insert(id, ProjectGrant { root: root.to_path_buf(), path: candidate_path, root_identity, file_identity: entry.identity });
    }
    Ok(())
}

pub(crate) fn selected_project_root_for_sandbox() -> Result<PathBuf, String> {
    ensure_skill_loaded()?;
    let root = SELECTED_PROJECT_ROOT.lock()
        .map_err(|_| "Coding Assistant project access is temporarily unavailable.".to_string())?
        .clone()
        .ok_or_else(|| "Select a project folder before checking sandbox readiness.".to_string())?;
    validate_granted_root(&root)
}

fn ensure_skill_loaded() -> Result<(), String> {
    if crate::skills::is_skill_loaded("coding-assistant") {
        Ok(())
    } else {
        Err("Load the Coding Assistant skill before selecting or reading project files.".into())
    }
}

fn validate_granted_root(root: &Path) -> Result<PathBuf, String> {
    let metadata = fs::symlink_metadata(root)
        .map_err(|_| "The selected project folder is no longer available. Select it again.".to_string())?;
    if is_redirected(&metadata) || !metadata.is_dir() {
        return Err("The selected project folder is no longer a normal folder.".into());
    }
    let canonical = fs::canonicalize(root)
        .map_err(|_| "FRIDAY could not revalidate the selected project folder.".to_string())?;
    if canonical != root {
        return Err("The selected project folder changed after approval. Select it again.".into());
    }
    Ok(canonical)
}

pub fn revoke_session_access() -> Result<(), String> {
    *SELECTED_PROJECT_ROOT.lock()
        .map_err(|_| "Coding Assistant project access is temporarily unavailable.".to_string())? = None;
    PROJECT_GRANTS.lock()
        .map_err(|_| "Coding Assistant project access is temporarily unavailable.".to_string())?
        .clear();
    CODE_PREVIEWS.lock()
        .map_err(|_| "Coding Assistant preview storage is temporarily unavailable.".to_string())?
        .clear();
    Ok(())
}

#[tauri::command]
pub async fn pick_project_folder_and_list_files(app: AppHandle) -> Result<CodingProjectScan, String> {
    crate::permissions::authorize_command("coding.pick_project_folder_and_list_files", "coding.project.select")?;
    ensure_skill_loaded()?;
    revoke_session_access()?;
    let selected = app.dialog().file().blocking_pick_folder();
    let Some(selected) = selected else { return Err("Project folder selection was cancelled.".into()); };
    let path = selected.into_path().map_err(|_| "FRIDAY could not access the selected project folder.".to_string())?;
    let selected_meta = fs::symlink_metadata(&path).map_err(|_| "FRIDAY could not inspect the selected project folder.".to_string())?;
    if !selected_meta.is_dir() || is_redirected(&selected_meta) {
        return Err("Please select a normal project folder, not a symbolic link or redirected folder.".into());
    }
    let root = fs::canonicalize(&path).map_err(|_| "FRIDAY could not open the selected project folder.".to_string())?;
    validate_granted_root(&root)?;
    let recovered_transactions = recover_project_code_transactions(&root)?;
    let mut candidates = Vec::new();
    let mut grants = BTreeMap::new();
    let root_handle = crate::coding_sandbox_fs::open_verified_root_directory(&root)
        .map_err(|_| "FRIDAY could not safely open the selected project folder.".to_string())?;
    let root_identity = crate::coding_sandbox_fs::verified_directory_identity(&root, &root_handle)
        .map_err(|_| "The selected project folder changed while FRIDAY was opening it.".to_string())?;
    let mut scanned = 0usize;
    scan_folder(&root_handle, Path::new(""), &root, root_identity, 0, &mut scanned, &mut candidates, &mut grants)?;
    let final_root_identity = crate::coding_sandbox_fs::verified_directory_identity(&root, &root_handle)
        .map_err(|_| "The selected project folder changed during scanning; scan again.".to_string())?;
    if final_root_identity != root_identity {
        return Err("The selected project folder changed during scanning; scan again.".into());
    }
    candidates.sort_by(|a, b| a.relative_path.to_lowercase().cmp(&b.relative_path.to_lowercase()));
    let truncated = scanned >= MAX_SCANNED_ENTRIES || candidates.len() >= MAX_PROJECT_CANDIDATES;
    *PROJECT_GRANTS.lock().map_err(|_| "Coding Assistant project access is temporarily unavailable.".to_string())? = grants;
    *SELECTED_PROJECT_ROOT.lock().map_err(|_| "Coding Assistant project access is temporarily unavailable.".to_string())? = Some(root.clone());
    let project_name = root.file_name().and_then(|value| value.to_str()).unwrap_or("Selected project").chars().take(160).collect();
    let _ = crate::tools::record(&app, "coding.list_project", &format!("Listed {} bounded source/configuration files from a folder explicitly selected by the user; recovered {} interrupted code transaction(s).", candidates.len(), recovered_transactions), true);
    Ok(CodingProjectScan { project_name, files: candidates, scanned_entries: scanned, truncated })
}


fn list_code_change_history_for_root(root: &Path) -> Result<Vec<CodingChangeHistoryItem>, String> {
    let root = validate_granted_root(root)?;
    recover_project_code_transactions(&root)?;
    let backup_dir = root.join(".friday-code-backups");
    let metadata = match fs::symlink_metadata(&backup_dir) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err("FRIDAY could not inspect the project's code history folder.".into()),
    };
    if is_redirected(&metadata) || !metadata.is_dir() {
        return Err("The project's code history folder is redirected or unsafe. No history was changed.".into());
    }
    let canonical = fs::canonicalize(&backup_dir)
        .map_err(|_| "FRIDAY could not validate the project's code history folder.".to_string())?;
    if !canonical.starts_with(&root) {
        return Err("The project's code history folder resolves outside the selected project.".into());
    }
    let mut entries = Vec::new();
    for entry in fs::read_dir(&canonical).map_err(|_| "FRIDAY could not read the project's code history.".to_string())? {
        let entry = entry.map_err(|_| "FRIDAY could not inspect a code history item.".to_string())?;
        let name = entry.file_name().to_string_lossy().to_string();
        if name.ends_with(".undo.json") || name.ends_with(".undone.json") {
            let meta = fs::symlink_metadata(entry.path())
                .map_err(|_| "FRIDAY could not inspect a saved code history record.".to_string())?;
            if is_redirected(&meta) || !meta.is_file() || meta.len() == 0 || meta.len() > 16 * 1024 {
                return Err("A code history record is unsafe or outside its size limit. All recovery evidence was preserved.".into());
            }
            entries.push((name, entry.path()));
            if entries.len() > MAX_CODE_BACKUP_FILES {
                return Err("This project contains more than 500 code history records. Manual review is required.".into());
            }
        }
    }
    let mut history = Vec::new();
    for (name, path) in entries {
        let bytes = read_recovery_file(&path, 16 * 1024)?
            .ok_or("A code history record disappeared while it was being read.")?;
        let record: CodingUndoRecord = serde_json::from_slice(&bytes)
            .map_err(|_| "A saved code history record is malformed. FRIDAY preserved it and stopped loading history.".to_string())?;
        validate_undo_record(&record)?;
        let active_name = format!("{}.undo.json", record.id);
        let consumed_name = format!("{}.undone.json", record.id);
        if name != active_name && name != consumed_name {
            return Err("A code history filename does not match its record identifier. No history was changed.".into());
        }
        let active_path = canonical.join(&active_name);
        let consumed_path = canonical.join(&consumed_name);
        if fs::symlink_metadata(&active_path).is_ok() && fs::symlink_metadata(&consumed_path).is_ok() {
            return Err("Both active and consumed history records exist for one change. Manual review is required.".into());
        }
        let is_undone = name == consumed_name;
        let mut undo_available = false;
        let mut unavailable_reason = None;
        if !is_undone {
            let target = match safe_recovery_target(&root, &record) {
                Ok(value) => Some(value),
                Err(_) => {
                    unavailable_reason = Some("The original project file is missing, redirected, or no longer safe.".to_string());
                    None
                }
            };
            if let Some(target) = target {
                let target_hash = recovery_hash(&target, MAX_FILE_BYTES)?;
                let backup = canonical.join(format!("{}.bak", record.id));
                let backup_bytes = read_recovery_file(&backup, MAX_FILE_BYTES)?;
                match (target_hash, backup_bytes) {
                    (Some(current), Some(original)) if current == record.resulting_sha256 && sha256_hex(&original) == record.original_sha256 => {
                        undo_available = true;
                    }
                    (Some(current), _) if current != record.resulting_sha256 =>
                        unavailable_reason = Some("The project file has changed since FRIDAY edited it. Undo is disabled to protect newer changes.".into()),
                    (None, _) =>
                        unavailable_reason = Some("The original project file is missing. Undo is disabled.".into()),
                    (_, None) =>
                        unavailable_reason = Some("The original backup is missing. Undo is disabled.".into()),
                    (_, Some(_)) =>
                        unavailable_reason = Some("The original backup failed integrity verification. Undo is disabled.".into()),
                }
            }
        }
        history.push(CodingChangeHistoryItem {
            id: record.id.clone(),
            relative_path: record.relative_path,
            backup_relative_path: record.backup_relative_path,
            created_at: record.created_at,
            status: if is_undone { "undone".into() } else if undo_available { "applied".into() } else { "blocked".into() },
            undo_record_id: record.id,
            undo_available,
            unavailable_reason,
        });
    }
    history.sort_by(|a, b| b.created_at.cmp(&a.created_at).then_with(|| a.id.cmp(&b.id)));
    Ok(history)
}

#[tauri::command]
pub async fn list_code_change_history() -> Result<Vec<CodingChangeHistoryItem>, String> {
    crate::permissions::authorize_command("coding.list_code_change_history", "coding.history.read")?;
    ensure_skill_loaded()?;
    let root = SELECTED_PROJECT_ROOT.lock()
        .map_err(|_| "Coding Assistant project access is temporarily unavailable.".to_string())?
        .clone()
        .ok_or("Choose the project folder again before viewing its Coding Assistant history.")?;
    list_code_change_history_for_root(&root)
}

#[tauri::command]
pub async fn preview_code_change(file_id: String, proposed_content: String) -> Result<CodingChangePreview, String> {
    crate::permissions::authorize_command("coding.preview_code_change", "coding.change.preview")?;
    ensure_skill_loaded()?;
    if file_id.len() != 48 || !file_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("Select a project file again before creating a change preview.".into());
    }
    if proposed_content.is_empty() || proposed_content.len() > MAX_PROPOSED_FILE_BYTES || proposed_content.as_bytes().contains(&0) {
        return Err("The proposed file must be non-empty UTF-8 text no larger than 128 KB, with no binary NUL bytes.".into());
    }
    let now = current_epoch_seconds()?;
    let grant = {
        let grants = PROJECT_GRANTS.lock().map_err(|_| "Coding Assistant project access is temporarily unavailable.".to_string())?;
        grants.get(&file_id).cloned().ok_or("This project file grant has expired. Select the project folder again.")?
    };
    let root = validate_granted_root(&grant.root)?;
    if !supported_project_file(&grant.path) {
        return Err("This file type is not permitted for a Coding Assistant preview.".into());
    }
    let relative_file_path = grant.path.strip_prefix(&root)
        .map_err(|_| "The selected file is outside the approved project folder.".to_string())?;
    let root_handle = crate::coding_sandbox_fs::open_verified_root_directory(&root)
        .map_err(|_| "FRIDAY could not safely reopen the selected project folder.".to_string())?;
    let opened_root_identity = crate::coding_sandbox_fs::verified_directory_identity(&root, &root_handle)
        .map_err(|_| "FRIDAY could not verify the selected project folder.".to_string())?;
    if opened_root_identity != grant.root_identity {
        return Err("The selected project folder changed after scanning. Select it again.".into());
    }
    let original_bytes = crate::coding_sandbox_fs::read_verified_project_file(
        &root_handle, relative_file_path, grant.file_identity, MAX_FILE_BYTES,
    ).map_err(|_| "The selected project file changed identity or could not be read safely. Scan the project again.".to_string())?;
    if original_bytes.contains(&0) {
        return Err("The current file changed or is no longer eligible for a safe preview.".into());
    }
    let original_content = String::from_utf8(original_bytes)
        .map_err(|_| "The current file is not valid UTF-8 text.".to_string())?;
    if original_content == proposed_content {
        return Err("The proposed content is identical to the current file; there is nothing to preview.".into());
    }
    let original_sha256 = sha256_hex(original_content.as_bytes());
    let proposed_sha256 = sha256_hex(proposed_content.as_bytes());
    let proposed_size_bytes = proposed_content.len();
    let preview_id = make_token();
    let relative = relative_path(&root, &grant.path).ok_or("The selected file is outside the approved project folder.")?;
    let file_name = grant.path.file_name().and_then(|value| value.to_str()).unwrap_or("Project file").chars().take(180).collect();
    let diff = unified_diff(&original_content, &proposed_content);
    let mut previews = CODE_PREVIEWS.lock().map_err(|_| "Coding Assistant preview storage is temporarily unavailable.".to_string())?;
    previews.retain(|_, preview| preview.expires_at > now);
    if previews.len() >= 10 {
        return Err("There are already 10 pending code previews. Wait for them to expire or select the project again.".into());
    }
    previews.insert(preview_id.clone(), StoredCodePreview {
        file_id, root, path: grant.path, original_sha256: original_sha256.clone(),
        proposed_sha256: proposed_sha256.clone(), proposed_content,
        expires_at: now.saturating_add(CODE_PREVIEW_TTL_SECONDS), approval_token: None,
    });
    Ok(CodingChangePreview {
        preview_id, file_name, relative_path: relative, original_sha256, proposed_sha256,
        original_size_bytes: original_content.len(), proposed_size_bytes,
        unified_diff: diff, expires_in_seconds: CODE_PREVIEW_TTL_SECONDS,
    })
}


/// Issues a short-lived, one-use token for the exact preview currently shown to the user.
/// This command does not modify project files; a later apply operation must consume the token.
#[tauri::command]
pub fn issue_code_change_approval(preview_id: String) -> Result<String, String> {
    crate::permissions::authorize_command("coding.issue_code_change_approval", "coding.change.approval")?;
    ensure_skill_loaded()?;
    if preview_id.len() != 48 || !preview_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("Review the code preview again before approving it.".into());
    }
    let now = current_epoch_seconds()?;
    let mut previews = CODE_PREVIEWS.lock()
        .map_err(|_| "Coding Assistant preview storage is temporarily unavailable.".to_string())?;
    let preview = previews.get_mut(&preview_id)
        .ok_or("This code preview has expired or was already cancelled. Create it again.")?;
    if preview.expires_at <= now {
        previews.remove(&preview_id);
        return Err("This code preview has expired. Create a fresh preview before approving.".into());
    }
    // Re-approval rotates the token, immediately invalidating any previously issued token.
    let token = make_token();
    preview.approval_token = Some(token.clone());
    Ok(token)
}

/// Applies one exact, freshly approved preview. The command is deliberately not exposed in
/// the UI until guarded undo is implemented and verified.
#[tauri::command]
pub fn apply_code_change(app: AppHandle, preview_id: String, approval_token: String) -> Result<CodingApplyResult, String> {
    crate::permissions::authorize_command("coding.apply_code_change", "coding.change.apply")?;
    ensure_skill_loaded()?;
    if preview_id.len() != 48 || !preview_id.bytes().all(|b| b.is_ascii_hexdigit())
        || approval_token.len() != 48 || !approval_token.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("The code approval is invalid. Review and approve the preview again.".into());
    }
    let now = current_epoch_seconds()?;
    let preview = {
        let mut previews = CODE_PREVIEWS.lock().map_err(|_| "Coding Assistant preview storage is temporarily unavailable.".to_string())?;
        take_approved_preview(&mut previews, &preview_id, &approval_token, now)?
    };
    validate_proposed_content(&preview.proposed_content)?;
    if sha256_hex(preview.proposed_content.as_bytes()) != preview.proposed_sha256 {
        return Err("The approved proposal failed its integrity check. Create a fresh preview.".into());
    }
    let grants = PROJECT_GRANTS.lock().map_err(|_| "Coding Assistant project access is temporarily unavailable.".to_string())?;
    let grant = grants.get(&preview.file_id).ok_or("The project permission expired. Select the project folder again.")?;
    let root = validate_granted_root(&grant.root)?;
    if root != preview.root || grant.path != preview.path {
        return Err("The project permission changed after preview. Select the project folder again.".into());
    }
    // Reconcile any interrupted transaction before trusting a fresh approval.
    // If recovery changes the file, the preview hash check below will reject it.
    recover_project_code_transactions(&root)?;
    let current = grant.path.clone();
    if current != preview.path || !current.starts_with(&root) {
        return Err("The selected project file moved or was redirected. No changes were made.".into());
    }
    let root_handle = crate::coding_sandbox_fs::open_verified_root_directory(&root)
        .map_err(|_| "Could not safely reopen the approved project folder.".to_string())?;
    let actual_root_identity = crate::coding_sandbox_fs::verified_directory_identity(&root, &root_handle)
        .map_err(|_| "Could not verify the approved project folder.".to_string())?;
    if actual_root_identity != grant.root_identity {
        return Err("The approved project folder changed after scanning. No changes were made.".into());
    }
    let relative_file = current.strip_prefix(&root)
        .map_err(|_| "The selected project file is outside the approved folder.".to_string())?;
    let original = crate::coding_sandbox_fs::read_verified_project_file(
        &root_handle, relative_file, grant.file_identity, MAX_FILE_BYTES,
    ).map_err(|_| "The selected project file changed identity after preview. Create a fresh preview.".to_string())?;
    if sha256_hex(&original) != preview.original_sha256 {
        return Err("The file changed after preview. No changes were made; create a fresh preview.".into());
    }
    let token = make_token();
    let (backup, record) = apply_code_preview_to_disk_bound(&root, &current, preview.proposed_content.as_bytes(),
        &preview.original_sha256, &token, now, Some((grant.root_identity, grant.file_identity)))?;
    let relative = relative_path(&root, &current).ok_or("The changed file is outside the project folder.")?;
    let backup_relative = backup.strip_prefix(&root).unwrap_or(&backup).to_string_lossy().replace('\\', "/");
    let file_name = current.file_name().and_then(|v| v.to_str()).unwrap_or("Project file").to_string();
    let _ = crate::tools::record(&app, "coding.apply_code_change",
        &format!("Applied one explicitly approved file change: {relative}; original SHA-256 {}; resulting SHA-256 {}; backup {}.",
            record.original_sha256, record.resulting_sha256, backup_relative), true);
    Ok(CodingApplyResult {
        file_name, relative_path: relative, backup_relative_path: backup_relative,
        bytes_written: preview.proposed_content.len(), original_sha256: record.original_sha256,
        resulting_sha256: record.resulting_sha256, undo_record_id: record.id,
    })
}

/// Restores a previously applied change only while the target still matches FRIDAY's exact post-edit hash.
#[tauri::command]
pub fn undo_code_change(app: AppHandle, undo_record_id: String) -> Result<String, String> {
    crate::permissions::authorize_command("coding.undo_code_change", "coding.change.undo")?;
    ensure_skill_loaded()?;
    if undo_record_id.len() != 48 || !undo_record_id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("The undo request is invalid.".into());
    }
    let grants = PROJECT_GRANTS.lock()
        .map_err(|_| "Coding Assistant project access is temporarily unavailable.".to_string())?;
    let mut selected: Option<(PathBuf, PathBuf, CodingUndoRecord)> = None;
    for grant in grants.values() {
        let root = validate_granted_root(&grant.root)?;
        // Resolve interrupted Apply/Undo journals before looking up an active Undo record.
        recover_project_code_transactions(&root)?;
        let backup_dir = ensure_code_backup_dir(&root)?;
        let record_name = format!("{}.undo.json", undo_record_id);
        let backup_handle = crate::coding_sandbox_fs::open_verified_root_directory(&backup_dir)
            .map_err(|_| "FRIDAY could not safely open its code backup folder.".to_string())?;
        match crate::coding_sandbox_fs::read_bounded_file_from_directory_handle(
            &backup_handle, OsStr::new(&record_name), 16 * 1024, MAX_CODE_BACKUP_FILES,
        ) {
            Ok(bytes) => {
                let record: CodingUndoRecord = serde_json::from_slice(&bytes)
                    .map_err(|_| "The guarded undo record is invalid or damaged.".to_string())?;
                validate_undo_record(&record)?;
                let target = root.join(&record.relative_path);
                if target == grant.path {
                    selected = Some((root, target, record));
                    break;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("FRIDAY could not safely read the guarded undo record.".into()),
        }
    }
    drop(grants);
    let (root, target, record) = selected.ok_or("No unused undo record for this file exists in the currently approved project. Select the project folder again if needed.")?;
    if root.join(&record.relative_path) != target || record.id != undo_record_id {
        return Err("The undo record no longer matches the approved project file.".into());
    }
    let (restored, backup) = undo_code_change_on_disk(&root, &record)?;
    let relative = relative_path(&root, &restored).unwrap_or_else(|| record.relative_path.clone());
    let backup_relative = backup.strip_prefix(&root).unwrap_or(&backup).to_string_lossy().replace('\\', "/");
    let _ = crate::tools::record(&app, "coding.undo_code_change",
        &format!("Undid one approved file change: {relative}; restored SHA-256 {}; retained backup {}.",
            record.original_sha256, backup_relative), true);
    Ok(format!("Restored {relative} to its verified original contents. The original backup was retained at {backup_relative}."))
}


const MAX_MULTI_FILE_PROPOSAL_JSON_BYTES: usize = 600 * 1024;
const MAX_MULTI_FILE_PROPOSALS: usize = 8;
const MAX_MULTI_FILE_PROPOSAL_TOTAL_BYTES: usize = 512 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MultiFileProposalEnvelope { files: Vec<MultiFileProposalEntry> }

#[derive(Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MultiFileProposalEntry { path: String, content: String }

fn valid_multi_file_relative_path(path: &str) -> bool {
    if path.is_empty()
        || path.starts_with('/')
        || path.contains('\\')
        || path.contains(':')
        || path.chars().any(char::is_control)
    {
        return false;
    }
    let components: Vec<&str> = path.split('/').collect();
    !components.is_empty()
        && components.iter().all(|part| !part.is_empty() && *part != "." && *part != "..")
}

fn validate_multi_file_proposal(proposal_json: &str, allowed_paths: &[String]) -> Result<Vec<MultiFileProposalEntry>, String> {
    if proposal_json.is_empty() || proposal_json.len() > MAX_MULTI_FILE_PROPOSAL_JSON_BYTES || allowed_paths.is_empty() || allowed_paths.len() > MAX_SELECTED_FILES {
        return Err("The multi-file proposal or inspected-file list is outside FRIDAY's safety limits.".into());
    }
    let mut allowed = BTreeSet::new();
    for path in allowed_paths {
        let parsed = Path::new(path);
        if !valid_multi_file_relative_path(path) || parsed.is_absolute() || parsed.components().any(|part| !matches!(part, std::path::Component::Normal(_))) || !allowed.insert(path.to_ascii_lowercase()) {
            return Err("The inspected project-file list contains an invalid or duplicate path.".into());
        }
    }
    let envelope: MultiFileProposalEnvelope = serde_json::from_str(proposal_json).map_err(|_| "The local model did not return the required strict JSON multi-file proposal.".to_string())?;
    if envelope.files.is_empty() || envelope.files.len() > MAX_MULTI_FILE_PROPOSALS {
        return Err("A multi-file proposal must contain between 1 and 8 changed files.".into());
    }
    let mut seen = BTreeSet::new();
    let mut total = 0usize;
    for entry in &envelope.files {
        let parsed = Path::new(&entry.path);
        let normalized = entry.path.to_ascii_lowercase();
        if !valid_multi_file_relative_path(&entry.path) || parsed.is_absolute() || parsed.components().any(|part| !matches!(part, std::path::Component::Normal(_))) || !allowed.contains(&normalized) || !seen.insert(normalized) {
            return Err("The proposal contains an uninspected, unsafe, or duplicate project path. No files were changed.".into());
        }
        validate_proposed_content(&entry.content)?;
        total = total.saturating_add(entry.content.len());
        if total > MAX_MULTI_FILE_PROPOSAL_TOTAL_BYTES { return Err("The combined multi-file proposal exceeds FRIDAY's 512 KB limit.".into()); }
    }
    Ok(envelope.files)
}

#[tauri::command]
pub fn validate_multi_file_coding_proposal(proposal_json: String, allowed_paths: Vec<String>) -> Result<Vec<MultiFileProposalEntry>, String> {
    crate::permissions::authorize_command("coding.validate_multi_file_coding_proposal", "coding.proposal.validate")?;
    ensure_skill_loaded()?;
    validate_multi_file_proposal(&proposal_json, &allowed_paths)
}

fn validate_proposed_content(content: &str) -> Result<(), String> {
    if content.is_empty() || content.len() > MAX_PROPOSED_FILE_BYTES || content.as_bytes().contains(&0) {
        return Err("The approved proposal is empty, too large, or contains binary NUL bytes.".into());
    }
    Ok(())
}

#[tauri::command]
pub fn cancel_code_change_preview(preview_id: String) -> Result<(), String> {
    crate::permissions::authorize_command("coding.cancel_code_change_preview", "coding.change.cancel")?;
    ensure_skill_loaded()?;
    if preview_id.len() != 48 || !preview_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("The code preview identifier is invalid.".into());
    }
    CODE_PREVIEWS.lock()
        .map_err(|_| "Coding Assistant preview storage is temporarily unavailable.".to_string())?
        .remove(&preview_id);
    Ok(())
}

#[tauri::command]
pub async fn read_project_files(app: AppHandle, file_ids: Vec<String>) -> Result<Vec<CodingProjectFile>, String> {
    crate::permissions::authorize_command("coding.read_project_files", "coding.project.read")?;
    ensure_skill_loaded()?;
    if file_ids.is_empty() || file_ids.len() > MAX_SELECTED_FILES {
        return Err("Select between 1 and 25 project files to inspect.".into());
    }
    let mut unique = BTreeSet::new();
    if file_ids.iter().any(|id| id.len() != 48 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) || !unique.insert(id.clone())) {
        return Err("A project file selection is invalid or duplicated. Select the files again.".into());
    }
    let grants = PROJECT_GRANTS.lock().map_err(|_| "Coding Assistant project access is temporarily unavailable.".to_string())?;
    let mut files = Vec::with_capacity(file_ids.len());
    let mut total_bytes = 0usize;
    for id in &file_ids {
        let grant = grants.get(id).ok_or("This project selection has expired. Select the project folder again.")?;
        let root = validate_granted_root(&grant.root)?;
        if !supported_project_file(&grant.path) {
            return Err("A selected project file is no longer an allowed source/configuration file.".into());
        }
        let relative_file_path = grant.path.strip_prefix(&root)
            .map_err(|_| "A selected project file is outside the approved folder.".to_string())?;
        let root_handle = crate::coding_sandbox_fs::open_verified_root_directory(&root)
            .map_err(|_| "FRIDAY could not safely reopen the selected project folder.".to_string())?;
        let opened_root_identity = crate::coding_sandbox_fs::verified_directory_identity(&root, &root_handle)
            .map_err(|_| "FRIDAY could not verify the selected project folder.".to_string())?;
        if opened_root_identity != grant.root_identity {
            return Err("The selected project folder changed after scanning. Select it again.".into());
        }
        let remaining = MAX_TOTAL_BYTES.saturating_sub(total_bytes);
        let bytes = crate::coding_sandbox_fs::read_verified_project_file(
            &root_handle, relative_file_path, grant.file_identity, MAX_FILE_BYTES.min(remaining as u64),
        ).map_err(|_| "A selected project file changed identity or could not be read safely. Scan the project again.".to_string())?;
        if bytes.len() > MAX_FILE_BYTES as usize || bytes.len() > remaining {
            return Err("The selected files exceed the 180 KB combined project-reading limit. Select fewer files.".into());
        }
        if bytes.contains(&0) {
            return Err("A selected project file appears to contain binary data and was not read.".into());
        }
        let content = String::from_utf8(bytes).map_err(|_| "A selected project file is not valid UTF-8 text and was not read.".to_string())?;
        let name = grant.path.file_name().and_then(|value| value.to_str()).unwrap_or("Project file").chars().take(180).collect();
        let relative = relative_path(&root, &grant.path).ok_or("A selected project file is outside the approved folder.")?;
        total_bytes = total_bytes.saturating_add(content.len());
        files.push(CodingProjectFile {
            name,
            relative_path: relative,
            language: language_for(&grant.path),
            size_bytes: content.len(),
            content,
        });
    }
    drop(grants);
    let _ = crate::tools::record(&app, "coding.read_project_files", &format!("Read {} explicitly selected project files ({} bytes) for local code analysis.", files.len(), total_bytes), true);
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;


    #[test]
    fn multi_file_proposal_accepts_only_bounded_inspected_targets() {
        let json = r#"{"files":[{"path":"src/main.rs","content":"fn main() {}"},{"path":"src/lib.rs","content":"pub fn ready() {}"}]}"#;
        let result = validate_multi_file_proposal(json, &["src/main.rs".into(), "src/lib.rs".into()]).unwrap();
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn multi_file_proposal_rejects_uninspected_duplicate_and_unsafe_paths() {
        let cases = [
            r#"{"files":[{"path":"src/other.rs","content":"changed"}]}"#,
            r#"{"files":[{"path":"src/main.rs","content":"one"},{"path":"src/main.rs","content":"two"}]}"#,
            r#"{"files":[{"path":"../outside.rs","content":"changed"}]}"#,
            r#"{"files":[{"path":"src/main.rs","content":"changed","extra":true}]}"#,
        ];
        for json in cases { assert!(validate_multi_file_proposal(json, &["src/main.rs".into()]).is_err()); }
    }

    #[test]
    fn multi_file_proposal_enforces_count_and_content_limits() {
        let json = serde_json::json!({"files": (0..9).map(|i| serde_json::json!({"path":format!("src/{i}.rs"),"content":"x"})).collect::<Vec<_>>()});
        let paths = (0..9).map(|i| format!("src/{i}.rs")).collect::<Vec<_>>();
        assert!(validate_multi_file_proposal(&json.to_string(), &paths).is_err());
        let huge = serde_json::json!({"files":[{"path":"src/main.rs","content":"x".repeat(MAX_PROPOSED_FILE_BYTES + 1)}]});
        assert!(validate_multi_file_proposal(&huge.to_string(), &["src/main.rs".into()]).is_err());
    }

    #[test]
    fn multi_file_proposal_rejects_windows_and_ambiguous_path_forms_on_every_host() {
        for path in ["C:/outside.rs", "/absolute.rs", "src//main.rs", "src/./main.rs", "src/../outside.rs", "src\\\\main.rs", "src/file:name.rs"] {
            let json = serde_json::json!({"files":[{"path":path,"content":"changed"}]});
            assert!(validate_multi_file_proposal(&json.to_string(), &[path.into()]).is_err(), "accepted {path}");
        }
        for path in ["C:/outside.rs", "/absolute.rs", "src//main.rs", "src/./main.rs", "src/../outside.rs", "src\\\\main.rs", "src/file:name.rs"] {
            assert!(validate_multi_file_proposal(
                r#"{"files":[{"path":"src/main.rs","content":"changed"}]}"#,
                &[path.into()],
            ).is_err(), "accepted unsafe allowlist path {path}");
        }
    }

    #[test]
    fn multi_file_proposal_enforces_aggregate_content_limit() {
        let paths = (0..5).map(|index| format!("src/{index}.rs")).collect::<Vec<_>>();
        let files = paths.iter().map(|path| serde_json::json!({
            "path": path,
            "content": "x".repeat(110 * 1024)
        })).collect::<Vec<_>>();
        let json = serde_json::json!({"files":files}).to_string();
        assert!(json.len() <= MAX_MULTI_FILE_PROPOSAL_JSON_BYTES);
        assert!(validate_multi_file_proposal(&json, &paths).is_err());
    }

    #[test]
    fn project_file_allowlist_includes_common_languages_and_manifests() {
        for name in ["src/main.rs", "src/App.tsx", "main.py", "go.mod", "Cargo.toml", "package.json", "README.md"] {
            assert!(supported_project_file(Path::new(name)), "{name}");
        }
    }

    #[test]
    fn project_file_allowlist_excludes_secrets_and_unknown_files() {
        for name in [".env", ".env.local", "server.pem", "private.key", "credentials.json", "id_rsa", "token.json", "service-account.json", "client.p12", "signing.jks", "image.png", "archive.zip"] {
            assert!(!supported_project_file(Path::new(name)), "{name}");
        }
    }

    #[test]
    fn ignored_directories_exclude_dependencies_builds_and_hidden_data() {
        for name in [".git", ".vscode", "node_modules", "target", "dist", "coverage", "vendor", "secrets", "credentials", "private-keys", "certificates"] {
            assert!(ignored_directory(name), "{name}");
        }
        assert!(!ignored_directory("src"));
    }

    #[test]
    fn language_detection_is_stable_for_common_project_files() {
        assert_eq!(language_for(Path::new("src/main.rs")), "Rust");
        assert_eq!(language_for(Path::new("src/App.tsx")), "TypeScript");
        assert_eq!(language_for(Path::new("main.py")), "Python");
        assert_eq!(language_for(Path::new("package.json")), "JSON");
    }

    #[test]
    fn project_scan_stays_inside_the_selected_root_and_skips_secrets_and_dependencies() {
        let temp = std::env::temp_dir().join(format!("friday-coding-scan-{}", make_token()));
        fs::create_dir_all(temp.join("src")).unwrap();
        fs::create_dir_all(temp.join("node_modules/pkg")).unwrap();
        fs::create_dir_all(temp.join("secrets")).unwrap();
        fs::write(temp.join("src/main.rs"), "fn main() {}").unwrap();
        fs::write(temp.join("package.json"), "{ }").unwrap();
        fs::write(temp.join(".env"), "API_KEY=not-for-reading").unwrap();
        fs::write(temp.join("secrets/token.txt"), "not-for-reading").unwrap();
        fs::write(temp.join("node_modules/pkg/index.js"), "ignored").unwrap();
        let root = fs::canonicalize(&temp).unwrap();
        let root_handle = crate::coding_sandbox_fs::open_verified_root_directory(&root).unwrap();
        let root_identity = crate::coding_sandbox_fs::verified_directory_identity(&root, &root_handle).unwrap();
        let mut candidates = Vec::new();
        let mut grants = BTreeMap::new();
        let mut scanned = 0usize;
        scan_folder(&root_handle, Path::new(""), &root, root_identity, 0, &mut scanned, &mut candidates, &mut grants).unwrap();
        let paths: BTreeSet<String> = candidates.iter().map(|item| item.relative_path.clone()).collect();
        assert!(paths.contains("src/main.rs"));
        assert!(paths.contains("package.json"));
        assert!(!paths.iter().any(|path| path.contains("node_modules") || path.contains("secrets") || path == ".env"));
        assert!(candidates.len() <= MAX_PROJECT_CANDIDATES);
        assert_eq!(grants.len(), candidates.len());
        drop(root_handle);
        fs::remove_dir_all(&temp).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn project_scan_rejects_ordinary_directory_replacement_after_enumeration() {
        let temp = std::env::temp_dir().join(format!("friday-coding-scan-race-{}", make_token()));
        let displaced = std::env::temp_dir().join(format!("friday-coding-scan-displaced-{}", make_token()));
        fs::create_dir_all(temp.join("replace-me")).unwrap();
        let root = fs::canonicalize(&temp).unwrap();
        let root_handle = crate::coding_sandbox_fs::open_verified_root_directory(&root).unwrap();
        let root_identity = crate::coding_sandbox_fs::verified_directory_identity(&root, &root_handle).unwrap();
        let mut candidates = Vec::new();
        let mut grants = BTreeMap::new();
        let mut scanned = 0usize;
        let mut replaced = false;
        let mut hook = |relative: &Path| {
            if relative == Path::new("replace-me") && !replaced {
                fs::rename(root.join("replace-me"), &displaced).map_err(|_| "test directory replacement failed".to_string())?;
                fs::create_dir(root.join("replace-me")).map_err(|_| "test replacement directory creation failed".to_string())?;
                fs::write(root.join("replace-me").join("secret.rs"), b"must not be scanned").map_err(|_| "test replacement file creation failed".to_string())?;
                replaced = true;
            }
            Ok(())
        };
        let result = scan_folder_with_hook(&root_handle, Path::new(""), &root, root_identity, 0, &mut scanned, &mut candidates, &mut grants, &mut hook);
        assert!(replaced, "the test must replace the ordinary directory after enumeration");
        assert!(result.is_err(), "the replacement directory must fail identity validation");
        assert!(candidates.is_empty(), "replacement contents must never become project candidates");
        assert!(grants.is_empty(), "replacement contents must never receive project grants");
        drop(root_handle);
        fs::remove_dir_all(&temp).unwrap();
        fs::remove_dir_all(&displaced).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn project_scan_rejects_ordinary_file_replacement_after_enumeration() {
        let temp = std::env::temp_dir().join(format!("friday-coding-file-race-{}", make_token()));
        let displaced = std::env::temp_dir().join(format!("friday-coding-file-displaced-{}", make_token()));
        fs::create_dir_all(&temp).unwrap();
        fs::write(temp.join("replace-me.rs"), b"original").unwrap();
        let root = fs::canonicalize(&temp).unwrap();
        let root_handle = crate::coding_sandbox_fs::open_verified_root_directory(&root).unwrap();
        let root_identity = crate::coding_sandbox_fs::verified_directory_identity(&root, &root_handle).unwrap();
        let mut candidates = Vec::new();
        let mut grants = BTreeMap::new();
        let mut scanned = 0usize;
        let mut replaced = false;
        let mut hook = |relative: &Path| {
            if relative == Path::new("replace-me.rs") && !replaced {
                fs::rename(root.join("replace-me.rs"), &displaced).map_err(|_| "test file replacement failed".to_string())?;
                fs::write(root.join("replace-me.rs"), b"replacement").map_err(|_| "test replacement file creation failed".to_string())?;
                replaced = true;
            }
            Ok(())
        };
        let result = scan_folder_with_hook(&root_handle, Path::new(""), &root, root_identity, 0, &mut scanned, &mut candidates, &mut grants, &mut hook);
        assert!(replaced, "the test must replace the regular file after enumeration");
        assert!(result.is_err(), "the replacement file must fail identity validation");
        assert!(candidates.is_empty(), "replacement contents must never become project candidates");
        assert!(grants.is_empty(), "replacement contents must never receive project grants");
        drop(root_handle);
        fs::remove_dir_all(&temp).unwrap();
        fs::remove_file(&displaced).unwrap();
    }

    #[test]
    fn preview_diff_shows_changed_middle_with_context() {
        let diff = unified_diff("alpha\nbeta\ngamma", "alpha\nBETA\ngamma");
        assert!(diff.contains(" alpha"));
        assert!(diff.contains("-beta"));
        assert!(diff.contains("+BETA"));
        assert!(diff.contains(" gamma"));
    }

    #[test]
    fn code_preview_approval_is_exact_expiring_and_one_use() {
        let mut previews = BTreeMap::new();
        let preview = StoredCodePreview {
            file_id: "file".into(),
            root: PathBuf::from("/project"),
            path: PathBuf::from("/project/src/main.rs"),
            original_sha256: "original".into(),
            proposed_sha256: "proposed".into(),
            proposed_content: "fn main() {}".into(),
            expires_at: 120,
            approval_token: Some("approved-token".into()),
        };
        previews.insert("preview-id".into(), preview);

        assert!(take_approved_preview(&mut previews, "preview-id", "wrong-token", 100).is_err());
        assert!(!previews.contains_key("preview-id"), "failed attempts must consume the pending approval");
        
        let expired = StoredCodePreview {
            file_id: "file".into(),
            root: PathBuf::from("/project"),
            path: PathBuf::from("/project/src/main.rs"),
            original_sha256: "original".into(),
            proposed_sha256: "proposed".into(),
            proposed_content: "fn main() {}".into(),
            expires_at: 120,
            approval_token: Some("approved-token".into()),
        };
        previews.insert("expired-preview".into(), expired);
        assert!(take_approved_preview(&mut previews, "expired-preview", "approved-token", 120).is_err());
        assert!(!previews.contains_key("expired-preview"));

        let valid = StoredCodePreview {
            file_id: "file".into(),
            root: PathBuf::from("/project"),
            path: PathBuf::from("/project/src/main.rs"),
            original_sha256: "original".into(),
            proposed_sha256: "proposed".into(),
            proposed_content: "fn main() {}".into(),
            expires_at: 121,
            approval_token: Some("approved-token".into()),
        };
        previews.insert("valid-preview".into(), valid);
        assert!(take_approved_preview(&mut previews, "valid-preview", "approved-token", 120).is_ok());
        assert!(take_approved_preview(&mut previews, "valid-preview", "approved-token", 120).is_err());
    }

    #[test]
    fn guarded_code_apply_creates_verified_backup_and_undo_record() {
        let root = std::env::temp_dir().join(format!("friday-code-apply-{}", make_token()));
        fs::create_dir_all(&root).unwrap();
        let root = fs::canonicalize(&root).unwrap();
        let file = root.join("main.rs");
        fs::write(&file, "fn main() {}").unwrap();
        let original_hash = sha256_hex(b"fn main() {}");
        let token = make_token();
        let (backup, record) = apply_code_preview_to_disk(&root, &file, b"fn main() { println!(\"hi\"); }",
            &original_hash, &token, 123).unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"fn main() { println!(\"hi\"); }");
        assert_eq!(sha256_hex(&fs::read(&backup).unwrap()), original_hash);
        assert_eq!(record.original_sha256, original_hash);
        assert_eq!(record.resulting_sha256, sha256_hex(b"fn main() { println!(\"hi\"); }"));
        assert!(root.join(".friday-code-backups").join(format!("{token}.undo.json")).is_file());
        assert!(!root.join(".friday-code-backups").join(format!("{token}.txn.json")).exists(),
            "a completed Apply must remove its resolved transaction journal");
        fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn guarded_apply_never_overwrites_an_existing_displaced_destination() {
        let root = std::env::temp_dir().join(format!("friday-code-apply-collision-{}", make_token()));
        fs::create_dir_all(&root).unwrap();
        let root = fs::canonicalize(&root).unwrap();
        let file = root.join("main.rs");
        let original = b"original source";
        fs::write(&file, original).unwrap();
        let token = make_token();
        let displaced = root.join(format!(".friday-code-original-{token}.tmp"));
        fs::write(&displaced, b"pre-existing unrelated file").unwrap();

        let result = apply_code_preview_to_disk(
            &root, &file, b"proposed source", &sha256_hex(original), &token, 123,
        );
        assert!(result.is_err(), "Apply must fail rather than replace a pre-existing displaced destination");
        assert_eq!(fs::read(&file).unwrap(), original, "the approved source must remain unchanged");
        assert_eq!(fs::read(&displaced).unwrap(), b"pre-existing unrelated file",
            "the unrelated destination must never be overwritten");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn guarded_undo_restores_verified_original_and_prevents_replay() {
        let temp = std::env::temp_dir().join(format!("friday-code-undo-{}", make_token()));
        fs::create_dir_all(&temp).unwrap();
        let root = fs::canonicalize(&temp).unwrap();
        let file = root.join("main.rs");
        let original = b"fn main() {}";
        let proposed = b"fn main() { println!(\"hi\"); }";
        fs::write(&file, original).unwrap();
        let token = make_token();
        let (_, record) = apply_code_preview_to_disk(&root, &file, proposed, &sha256_hex(original), &token, 123).unwrap();
        let (restored, backup) = undo_code_change_on_disk(&root, &record).unwrap();
        assert_eq!(restored, file);
        assert_eq!(fs::read(&file).unwrap(), original);
        assert_eq!(fs::read(&backup).unwrap(), original);
        assert!(root.join(".friday-code-backups").join(format!("{token}.undone.json")).is_file());
        assert!(!root.join(".friday-code-backups").join(format!("{token}.undo.json")).exists());
        assert!(undo_code_change_on_disk(&root, &record).is_err(), "a consumed undo must not replay");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn guarded_undo_refuses_intervening_user_changes() {
        let temp = std::env::temp_dir().join(format!("friday-code-undo-stale-{}", make_token()));
        fs::create_dir_all(&temp).unwrap();
        let root = fs::canonicalize(&temp).unwrap();
        let file = root.join("main.rs");
        fs::write(&file, "original").unwrap();
        let token = make_token();
        let (_, record) = apply_code_preview_to_disk(&root, &file, b"friday edit", &sha256_hex(b"original"), &token, 123).unwrap();
        fs::write(&file, "user's newer edit").unwrap();
        assert!(undo_code_change_on_disk(&root, &record).is_err());
        assert_eq!(fs::read_to_string(&file).unwrap(), "user's newer edit");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn guarded_undo_refuses_corrupted_backup() {
        let temp = std::env::temp_dir().join(format!("friday-code-undo-backup-{}", make_token()));
        fs::create_dir_all(&temp).unwrap();
        let root = fs::canonicalize(&temp).unwrap();
        let file = root.join("main.rs");
        fs::write(&file, "original").unwrap();
        let token = make_token();
        let (backup, record) = apply_code_preview_to_disk(&root, &file, b"friday edit", &sha256_hex(b"original"), &token, 123).unwrap();
        fs::write(&backup, "corrupted").unwrap();
        assert!(undo_code_change_on_disk(&root, &record).is_err());
        assert_eq!(fs::read_to_string(&file).unwrap(), "friday edit");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn recovery_restores_original_when_apply_crashes_between_renames() {
        let temp = std::env::temp_dir().join(format!("friday-code-recovery-apply-{}", make_token()));
        fs::create_dir_all(&temp).unwrap();
        let root = fs::canonicalize(&temp).unwrap();
        let file = root.join("main.rs");
        let original = b"original source";
        let proposed = b"proposed source";
        fs::write(&file, original).unwrap();
        let token = make_token();
        let backup_dir = ensure_code_backup_dir(&root).unwrap();
        fs::write(backup_dir.join(format!("{token}.bak")), original).unwrap();
        let record = CodingUndoRecord {
            id: token.clone(), relative_path: "main.rs".into(),
            backup_relative_path: format!(".friday-code-backups/{token}.bak"),
            original_sha256: sha256_hex(original), resulting_sha256: sha256_hex(proposed), created_at: 123,
            original_unix_mode: None,
            original_windows_dacl: None,
        };
        fs::write(backup_dir.join(format!("{token}.txn.json")), serde_json::to_vec(&record).unwrap()).unwrap();
        fs::write(root.join(format!(".friday-code-original-{token}.tmp")), original).unwrap();
        fs::write(root.join(format!(".friday-code-edit-{token}.tmp")), proposed).unwrap();
        fs::remove_file(&file).unwrap();
        assert_eq!(recover_project_code_transactions(&root).unwrap(), 1);
        assert_eq!(fs::read(&file).unwrap(), original);
        assert!(!root.join(format!(".friday-code-edit-{token}.tmp")).exists());
        assert!(!backup_dir.join(format!("{token}.txn.json")).exists());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn recovery_finishes_undo_when_original_is_already_installed() {
        let temp = std::env::temp_dir().join(format!("friday-code-recovery-undo-{}", make_token()));
        fs::create_dir_all(&temp).unwrap();
        let root = fs::canonicalize(&temp).unwrap();
        let file = root.join("main.rs");
        let original = b"original source";
        let proposed = b"proposed source";
        fs::write(&file, original).unwrap();
        let token = make_token();
        let backup_dir = ensure_code_backup_dir(&root).unwrap();
        fs::write(backup_dir.join(format!("{token}.bak")), original).unwrap();
        let record = CodingUndoRecord {
            id: token.clone(), relative_path: "main.rs".into(),
            backup_relative_path: format!(".friday-code-backups/{token}.bak"),
            original_sha256: sha256_hex(original), resulting_sha256: sha256_hex(proposed), created_at: 123,
            original_unix_mode: None,
            original_windows_dacl: None,
        };
        fs::write(backup_dir.join(format!("{token}.undo.json")), serde_json::to_vec(&record).unwrap()).unwrap();
        fs::write(backup_dir.join(format!("{token}.undo-txn.json")), serde_json::to_vec(&record).unwrap()).unwrap();
        fs::write(root.join(format!(".friday-code-undone-{token}.tmp")), proposed).unwrap();
        assert_eq!(recover_project_code_transactions(&root).unwrap(), 1);
        assert!(backup_dir.join(format!("{token}.undone.json")).is_file());
        assert!(!backup_dir.join(format!("{token}.undo.json")).exists());
        assert!(!backup_dir.join(format!("{token}.undo-txn.json")).exists());
        assert_eq!(fs::read(&file).unwrap(), original);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn recovery_preserves_unknown_target_contents_and_journal() {
        let temp = std::env::temp_dir().join(format!("friday-code-recovery-unknown-{}", make_token()));
        fs::create_dir_all(&temp).unwrap();
        let root = fs::canonicalize(&temp).unwrap();
        let file = root.join("main.rs");
        let token = make_token();
        let backup_dir = ensure_code_backup_dir(&root).unwrap();
        let original = b"original source";
        let proposed = b"proposed source";
        fs::write(&file, b"user changed this").unwrap();
        fs::write(backup_dir.join(format!("{token}.bak")), original).unwrap();
        let record = CodingUndoRecord {
            id: token.clone(), relative_path: "main.rs".into(),
            backup_relative_path: format!(".friday-code-backups/{token}.bak"),
            original_sha256: sha256_hex(original), resulting_sha256: sha256_hex(proposed), created_at: 123,
            original_unix_mode: None,
        original_windows_dacl: None,
        };
        fs::write(backup_dir.join(format!("{token}.txn.json")), serde_json::to_vec(&record).unwrap()).unwrap();
        assert!(recover_project_code_transactions(&root).is_err());
        assert_eq!(fs::read(&file).unwrap(), b"user changed this");
        assert!(backup_dir.join(format!("{token}.txn.json")).exists());
        fs::remove_dir_all(&root).unwrap();
    }


    fn recovery_edge_fixture(label: &str, target_bytes: &[u8]) -> (PathBuf, PathBuf, PathBuf, CodingUndoRecord, Vec<u8>, Vec<u8>) {
        let temp = std::env::temp_dir().join(format!("friday-code-edge-{label}-{}", make_token()));
        fs::create_dir_all(&temp).unwrap();
        let root = fs::canonicalize(&temp).unwrap();
        let file = root.join("main.rs");
        let original = b"edge original source".to_vec();
        let proposed = b"edge proposed source".to_vec();
        fs::write(&file, target_bytes).unwrap();
        let token = make_token();
        let backup_dir = ensure_code_backup_dir(&root).unwrap();
        fs::write(backup_dir.join(format!("{token}.bak")), &original).unwrap();
        let record = CodingUndoRecord {
            id: token.clone(),
            relative_path: "main.rs".into(),
            backup_relative_path: format!(".friday-code-backups/{token}.bak"),
            original_sha256: sha256_hex(&original),
            resulting_sha256: sha256_hex(&proposed),
            created_at: 456,
            original_unix_mode: None,
        original_windows_dacl: None,
        };
        (root, file, backup_dir, record, original, proposed)
    }

    #[test]
    fn recovery_refuses_directory_in_place_of_active_undo_record() {
        let (root, file, backup_dir, record, original, _) = recovery_edge_fixture("directory-active-undo", b"edge original source");
        let journal = backup_dir.join(format!("{}.undo-txn.json", record.id));
        fs::write(&journal, serde_json::to_vec(&record).unwrap()).unwrap();
        let active = backup_dir.join(format!("{}.undo.json", record.id));
        fs::create_dir(&active).unwrap();
        assert!(recover_project_code_transactions(&root).is_err());
        assert_eq!(fs::read(&file).unwrap(), original);
        assert!(journal.is_file());
        assert!(active.is_dir());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_reparse_point_junction_is_rejected_as_recovery_folder() {
        use std::process::Command;
        let temp = std::env::temp_dir().join(format!("friday-code-junction-{}", make_token()));
        let root = temp.join("project");
        let outside = temp.join("outside");
        let link = root.join(".friday-code-backups");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("keep.txt"), b"outside user data").unwrap();

        // Use PowerShell environment variables rather than constructing a quoted
        // cmd.exe command string; this safely handles spaces and Windows path syntax.
        let result = Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "New-Item -ItemType Junction -Path $env:FRIDAY_JUNCTION_PATH -Target $env:FRIDAY_JUNCTION_TARGET | Out-Null",
            ])
            .env("FRIDAY_JUNCTION_PATH", &link)
            .env("FRIDAY_JUNCTION_TARGET", &outside)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "Could not create a Windows directory junction for the test: {}",
            String::from_utf8_lossy(&result.stderr)
        );

        let meta = fs::symlink_metadata(&link).unwrap();
        assert!(is_redirected(&meta), "Windows junction must be recognized as a reparse point");
        assert!(recover_project_code_transactions(&root).is_err());
        assert_eq!(fs::read(outside.join("keep.txt")).unwrap(), b"outside user data");

        fs::remove_dir(&link).unwrap();
        fs::remove_dir_all(&root).unwrap();
        fs::remove_dir_all(&outside).unwrap();
        fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn recovery_refuses_malformed_active_undo_record_and_preserves_journal() {
        let (root, file, backup_dir, record, original, _) = recovery_edge_fixture("malformed-active-undo", b"edge original source");
        let journal = backup_dir.join(format!("{}.undo-txn.json", record.id));
        fs::write(&journal, serde_json::to_vec(&record).unwrap()).unwrap();
        let active = backup_dir.join(format!("{}.undo.json", record.id));
        fs::write(&active, b"{ not valid undo json").unwrap();
        assert!(recover_project_code_transactions(&root).is_err());
        assert_eq!(fs::read(&file).unwrap(), original);
        assert!(journal.is_file());
        assert!(active.is_file());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recovery_refuses_mismatched_consumed_undo_record() {
        let (root, file, backup_dir, record, original, _) = recovery_edge_fixture("mismatched-consumed-undo", b"edge original source");
        let journal = backup_dir.join(format!("{}.undo-txn.json", record.id));
        fs::write(&journal, serde_json::to_vec(&record).unwrap()).unwrap();
        let mut wrong = record.clone();
        wrong.relative_path = "other.rs".into();
        let consumed = backup_dir.join(format!("{}.undone.json", record.id));
        fs::write(&consumed, serde_json::to_vec(&wrong).unwrap()).unwrap();
        assert!(recover_project_code_transactions(&root).is_err());
        assert_eq!(fs::read(&file).unwrap(), original);
        assert!(journal.is_file());
        assert!(consumed.is_file());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recovery_refuses_unexpected_displaced_apply_copy() {
        let (root, file, backup_dir, record, original, _) = recovery_edge_fixture("unexpected-displaced-apply", b"edge original source");
        let journal = backup_dir.join(format!("{}.txn.json", record.id));
        let displaced = root.join(format!(".friday-code-original-{}.tmp", record.id));
        fs::write(&journal, serde_json::to_vec(&record).unwrap()).unwrap();
        fs::write(&displaced, b"unexpected displaced user data").unwrap();
        assert!(recover_project_code_transactions(&root).is_err());
        assert_eq!(fs::read(&file).unwrap(), original);
        assert_eq!(fs::read(&displaced).unwrap(), b"unexpected displaced user data");
        assert!(journal.is_file());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recovery_refuses_unexpected_displaced_undo_copy() {
        let (root, file, backup_dir, record, _, proposed) = recovery_edge_fixture("unexpected-displaced-undo", b"edge proposed source");
        let journal = backup_dir.join(format!("{}.undo-txn.json", record.id));
        let displaced = root.join(format!(".friday-code-undone-{}.tmp", record.id));
        fs::write(&backup_dir.join(format!("{}.undo.json", record.id)), serde_json::to_vec(&record).unwrap()).unwrap();
        fs::write(&journal, serde_json::to_vec(&record).unwrap()).unwrap();
        fs::write(&displaced, b"unexpected displaced user data").unwrap();
        assert!(recover_project_code_transactions(&root).is_err());
        assert_eq!(fs::read(&file).unwrap(), proposed);
        assert_eq!(fs::read(&displaced).unwrap(), b"unexpected displaced user data");
        assert!(journal.is_file());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recovery_refuses_unexpected_undo_temporary_copy() {
        let (root, file, backup_dir, record, _, proposed) = recovery_edge_fixture("unexpected-undo-temp", b"edge proposed source");
        let journal = backup_dir.join(format!("{}.undo-txn.json", record.id));
        let temporary = root.join(format!(".friday-code-undo-{}.tmp", record.id));
        fs::write(&backup_dir.join(format!("{}.undo.json", record.id)), serde_json::to_vec(&record).unwrap()).unwrap();
        fs::write(&journal, serde_json::to_vec(&record).unwrap()).unwrap();
        fs::write(&temporary, b"unexpected temporary user data").unwrap();
        assert!(recover_project_code_transactions(&root).is_err());
        assert_eq!(fs::read(&file).unwrap(), proposed);
        assert_eq!(fs::read(&temporary).unwrap(), b"unexpected temporary user data");
        assert!(journal.is_file());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn recovery_refuses_symlink_target_without_touching_outside_file() {
        use std::os::unix::fs::symlink;
        let (root, file, backup_dir, record, _, _) = recovery_edge_fixture("symlink-target", b"edge original source");
        let journal = backup_dir.join(format!("{}.txn.json", record.id));
        let outside = root.parent().unwrap().join(format!("friday-code-target-outside-{}", make_token()));
        fs::write(&outside, b"outside user data").unwrap();
        fs::remove_file(&file).unwrap();
        symlink(&outside, &file).unwrap();
        fs::write(&journal, serde_json::to_vec(&record).unwrap()).unwrap();
        assert!(recover_project_code_transactions(&root).is_err());
        assert_eq!(fs::read(&outside).unwrap(), b"outside user data");
        assert!(fs::symlink_metadata(&file).unwrap().file_type().is_symlink());
        assert!(journal.is_file());
        fs::remove_dir_all(&root).unwrap();
        fs::remove_file(outside).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn recovery_refuses_symlink_displaced_and_temporary_files() {
        use std::os::unix::fs::symlink;
        for mode in ["apply-displaced", "apply-temporary", "undo-displaced", "undo-temporary"] {
            let target_bytes = if mode.starts_with("undo") { b"edge proposed source".as_slice() } else { b"edge original source".as_slice() };
            let (root, file, backup_dir, record, _, _) = recovery_edge_fixture(mode, target_bytes);
            let outside = root.parent().unwrap().join(format!("friday-code-copy-outside-{}", make_token()));
            fs::write(&outside, b"outside user data").unwrap();
            let (journal, copy) = match mode {
                "apply-displaced" => (backup_dir.join(format!("{}.txn.json", record.id)), root.join(format!(".friday-code-original-{}.tmp", record.id))),
                "apply-temporary" => (backup_dir.join(format!("{}.txn.json", record.id)), root.join(format!(".friday-code-edit-{}.tmp", record.id))),
                "undo-displaced" => (backup_dir.join(format!("{}.undo-txn.json", record.id)), root.join(format!(".friday-code-undone-{}.tmp", record.id))),
                _ => (backup_dir.join(format!("{}.undo-txn.json", record.id)), root.join(format!(".friday-code-undo-{}.tmp", record.id))),
            };
            if mode.starts_with("undo") {
                fs::write(backup_dir.join(format!("{}.undo.json", record.id)), serde_json::to_vec(&record).unwrap()).unwrap();
            }
            fs::write(&journal, serde_json::to_vec(&record).unwrap()).unwrap();
            symlink(&outside, &copy).unwrap();
            assert!(recover_project_code_transactions(&root).is_err(), "mode {mode}");
            assert_eq!(fs::read(&outside).unwrap(), b"outside user data", "mode {mode}");
            assert!(fs::symlink_metadata(&copy).unwrap().file_type().is_symlink(), "mode {mode}");
            assert!(journal.is_file(), "mode {mode}");
            fs::remove_dir_all(&root).unwrap();
            fs::remove_file(outside).unwrap();
        }
    }

    #[test]
    fn recovery_refuses_missing_original_backup_and_preserves_journal() {
        let (root, file, backup_dir, record, original, _) = recovery_edge_fixture("missing-backup", b"edge original source");
        let journal = backup_dir.join(format!("{}.txn.json", record.id));
        fs::write(&journal, serde_json::to_vec(&record).unwrap()).unwrap();
        fs::remove_file(backup_dir.join(format!("{}.bak", record.id))).unwrap();
        assert!(recover_project_code_transactions(&root).is_err());
        assert_eq!(fs::read(&file).unwrap(), original);
        assert!(journal.is_file());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recovery_refuses_corrupted_original_backup_and_preserves_evidence() {
        let (root, file, backup_dir, record, _, _) = recovery_edge_fixture("corrupt-backup", b"edge original source");
        let journal = backup_dir.join(format!("{}.txn.json", record.id));
        fs::write(&journal, serde_json::to_vec(&record).unwrap()).unwrap();
        fs::write(backup_dir.join(format!("{}.bak", record.id)), b"corrupted backup").unwrap();
        assert!(recover_project_code_transactions(&root).is_err());
        assert_eq!(fs::read(&file).unwrap(), b"edge original source");
        assert!(journal.is_file());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recovery_refuses_malformed_journal_without_cleanup() {
        let (root, file, backup_dir, record, _, _) = recovery_edge_fixture("malformed-journal", b"edge original source");
        let journal = backup_dir.join(format!("{}.txn.json", record.id));
        fs::write(&journal, b"{ definitely not valid json").unwrap();
        assert!(recover_project_code_transactions(&root).is_err());
        assert_eq!(fs::read(&file).unwrap(), b"edge original source");
        assert!(journal.is_file());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recovery_refuses_journal_filename_identifier_mismatch() {
        let (root, file, backup_dir, record, _, _) = recovery_edge_fixture("journal-name-mismatch", b"edge original source");
        let wrong_name = backup_dir.join(format!("{}.txn.json", make_token()));
        fs::write(&wrong_name, serde_json::to_vec(&record).unwrap()).unwrap();
        assert!(recover_project_code_transactions(&root).is_err());
        assert_eq!(fs::read(&file).unwrap(), b"edge original source");
        assert!(wrong_name.is_file());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recovery_refuses_path_traversal_journal() {
        let (root, file, backup_dir, mut record, _, _) = recovery_edge_fixture("path-traversal", b"edge original source");
        record.relative_path = "../outside.rs".into();
        let journal = backup_dir.join(format!("{}.txn.json", record.id));
        fs::write(&journal, serde_json::to_vec(&record).unwrap()).unwrap();
        assert!(recover_project_code_transactions(&root).is_err());
        assert_eq!(fs::read(&file).unwrap(), b"edge original source");
        assert!(journal.is_file());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recovery_refuses_unexpected_temporary_contents_and_preserves_them() {
        let (root, file, backup_dir, record, _, _) = recovery_edge_fixture("unexpected-temp", b"edge original source");
        let journal = backup_dir.join(format!("{}.txn.json", record.id));
        let temporary = root.join(format!(".friday-code-edit-{}.tmp", record.id));
        fs::write(&journal, serde_json::to_vec(&record).unwrap()).unwrap();
        fs::write(&temporary, b"user-owned unexpected contents").unwrap();
        assert!(recover_project_code_transactions(&root).is_err());
        assert_eq!(fs::read(&file).unwrap(), b"edge original source");
        assert_eq!(fs::read(&temporary).unwrap(), b"user-owned unexpected contents");
        assert!(journal.is_file());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recovery_refuses_mismatched_undo_record_for_completed_apply() {
        let (root, file, backup_dir, record, _, proposed) = recovery_edge_fixture("mismatched-undo", b"edge proposed source");
        let journal = backup_dir.join(format!("{}.txn.json", record.id));
        fs::write(&journal, serde_json::to_vec(&record).unwrap()).unwrap();
        let mut wrong = record.clone();
        wrong.resulting_sha256 = "a".repeat(64);
        fs::write(backup_dir.join(format!("{}.undo.json", record.id)), serde_json::to_vec(&wrong).unwrap()).unwrap();
        assert!(recover_project_code_transactions(&root).is_err());
        assert_eq!(fs::read(&file).unwrap(), proposed);
        assert!(journal.is_file());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recovery_refuses_both_active_and_consumed_undo_records() {
        let (root, file, backup_dir, record, original, _) = recovery_edge_fixture("duplicate-undo-state", b"edge original source");
        let journal = backup_dir.join(format!("{}.undo-txn.json", record.id));
        fs::write(&journal, serde_json::to_vec(&record).unwrap()).unwrap();
        fs::write(backup_dir.join(format!("{}.undo.json", record.id)), serde_json::to_vec(&record).unwrap()).unwrap();
        fs::write(backup_dir.join(format!("{}.undone.json", record.id)), serde_json::to_vec(&record).unwrap()).unwrap();
        assert!(recover_project_code_transactions(&root).is_err());
        assert_eq!(fs::read(&file).unwrap(), original);
        assert!(journal.is_file());
        assert!(backup_dir.join(format!("{}.undo.json", record.id)).is_file());
        assert!(backup_dir.join(format!("{}.undone.json", record.id)).is_file());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn recovery_refuses_symlink_journal_and_preserves_link() {
        use std::os::unix::fs::symlink;
        let (root, file, backup_dir, record, _, _) = recovery_edge_fixture("symlink-journal", b"edge original source");
        let real = backup_dir.join("journal-source.json");
        fs::write(&real, serde_json::to_vec(&record).unwrap()).unwrap();
        let journal = backup_dir.join(format!("{}.txn.json", record.id));
        symlink(&real, &journal).unwrap();
        assert!(recover_project_code_transactions(&root).is_err());
        assert_eq!(fs::read(&file).unwrap(), b"edge original source");
        assert!(fs::symlink_metadata(&journal).unwrap().file_type().is_symlink());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn recovery_refuses_symlink_backup_and_target_without_touching_outside_file() {
        use std::os::unix::fs::symlink;
        let (root, file, backup_dir, record, _, _) = recovery_edge_fixture("symlink-backup-target", b"edge original source");
        let journal = backup_dir.join(format!("{}.txn.json", record.id));
        fs::write(&journal, serde_json::to_vec(&record).unwrap()).unwrap();
        let outside = root.parent().unwrap().join(format!("friday-code-outside-{}", make_token()));
        fs::write(&outside, b"outside user data").unwrap();
        fs::remove_file(backup_dir.join(format!("{}.bak", record.id))).unwrap();
        symlink(&outside, backup_dir.join(format!("{}.bak", record.id))).unwrap();
        assert!(recover_project_code_transactions(&root).is_err());
        assert_eq!(fs::read(&outside).unwrap(), b"outside user data");
        assert!(journal.is_file());
        fs::remove_dir_all(&root).unwrap();
        fs::remove_file(outside).unwrap();
        let _ = file;
    }
    
    fn recovery_fixture(label: &str) -> (PathBuf, PathBuf, CodingUndoRecord, Vec<u8>, Vec<u8>) {
        let temp = std::env::temp_dir().join(format!("friday-code-fault-{label}-{}", make_token()));
        fs::create_dir_all(&temp).unwrap();
        let root = fs::canonicalize(&temp).unwrap();
        let file = root.join("main.rs");
        let original = b"original source".to_vec();
        let proposed = b"proposed source".to_vec();
        let token = make_token();
        let backup_dir = ensure_code_backup_dir(&root).unwrap();
        fs::write(backup_dir.join(format!("{token}.bak")), &original).unwrap();
        let record = CodingUndoRecord {
            id: token.clone(),
            relative_path: "main.rs".into(),
            backup_relative_path: format!(".friday-code-backups/{token}.bak"),
            original_sha256: sha256_hex(&original),
            resulting_sha256: sha256_hex(&proposed),
            created_at: 123,
            original_unix_mode: None,
        original_windows_dacl: None,
        };
        (root, file, record, original, proposed)
    }

    #[test]
    fn successful_apply_and_undo_remove_resolved_transaction_journals() {
        let root = std::env::temp_dir().join(format!("friday-code-journal-cleanup-{}", make_token()));
        fs::create_dir_all(&root).unwrap();
        let root = fs::canonicalize(&root).unwrap();
        let file = root.join("main.rs");
        let original = b"original source";
        let proposed = b"first approved edit";
        fs::write(&file, original).unwrap();

        let first_token = make_token();
        let (backup, first_record) = apply_code_preview_to_disk(
            &root, &file, proposed, &sha256_hex(original), &first_token, 123,
        ).unwrap();
        let backup_dir = root.join(".friday-code-backups");
        assert!(!backup_dir.join(format!("{}.txn.json", first_record.id)).exists());
        assert!(backup.is_file());
        assert!(backup_dir.join(format!("{}.undo.json", first_record.id)).is_file());

        let (restored, _) = undo_code_change_on_disk(&root, &first_record).unwrap();
        assert_eq!(restored, file);
        assert_eq!(fs::read(&file).unwrap(), original);
        assert!(!backup_dir.join(format!("{}.undo-txn.json", first_record.id)).exists());
        assert!(backup_dir.join(format!("{}.undone.json", first_record.id)).is_file());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn sequential_edits_can_be_undone_in_reverse_order_without_stale_journals() {
        let root = std::env::temp_dir().join(format!("friday-code-sequential-{}", make_token()));
        fs::create_dir_all(&root).unwrap();
        let root = fs::canonicalize(&root).unwrap();
        let file = root.join("main.rs");
        let original = b"original source";
        let first = b"first edit";
        let second = b"second edit";
        fs::write(&file, original).unwrap();

        let (_, first_record) = apply_code_preview_to_disk(
            &root, &file, first, &sha256_hex(original), &make_token(), 123,
        ).unwrap();
        let (_, second_record) = apply_code_preview_to_disk(
            &root, &file, second, &sha256_hex(first), &make_token(), 124,
        ).unwrap();
        let backup_dir = root.join(".friday-code-backups");
        assert!(!backup_dir.join(format!("{}.txn.json", first_record.id)).exists());
        assert!(!backup_dir.join(format!("{}.txn.json", second_record.id)).exists());

        undo_code_change_on_disk(&root, &second_record).unwrap();
        assert_eq!(fs::read(&file).unwrap(), first);
        undo_code_change_on_disk(&root, &first_record).unwrap();
        assert_eq!(fs::read(&file).unwrap(), original);
        assert!(!backup_dir.join(format!("{}.undo-txn.json", first_record.id)).exists());
        assert!(!backup_dir.join(format!("{}.undo-txn.json", second_record.id)).exists());
        fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn recovery_restores_original_unix_permissions_when_rebuilding_missing_target() {
        use std::os::unix::fs::PermissionsExt;
        let (root, file, record, original, _proposed) = recovery_fixture("unix-mode");
        let mut record = record;
        record.original_unix_mode = Some(0o751);
        let backup_dir = root.join(".friday-code-backups");
        fs::write(backup_dir.join(format!("{}.txn.json", record.id)), serde_json::to_vec(&record).unwrap()).unwrap();

        // Model a crash after the target was displaced, followed by loss of that
        // staged copy: recovery must rebuild only from the verified original backup.
        assert!(!file.exists());
        assert_eq!(recover_project_code_transactions(&root).unwrap(), 1);
        assert_eq!(fs::read(&file).unwrap(), original);
        assert_eq!(fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o751);
        assert!(!backup_dir.join(format!("{}.txn.json", record.id)).exists());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn legacy_undo_record_without_unix_mode_remains_readable() {
        let json = r#"{"id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","relativePath":"main.rs","backupRelativePath":".friday-code-backups/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.bak","originalSha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","resultingSha256":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","createdAt":123}"#;
        let record: CodingUndoRecord = serde_json::from_str(json).unwrap();
        assert_eq!(record.original_unix_mode, None);
        assert!(validate_undo_record(&record).is_ok());
    }

    #[test]
    fn undo_record_rejects_malformed_windows_dacl_metadata_but_accepts_legacy_records() {
        let (_, _, mut record, _, _) = recovery_fixture("dacl-metadata-validation");
        assert!(validate_undo_record(&record).is_ok(), "legacy records without DACL metadata remain readable");

        for malformed in ["", "abc", "zz".repeat(20).as_str(), "aa".repeat(65_537).as_str(), "aa".repeat(19).as_str()] {
            record.original_windows_dacl = Some(malformed.to_string());
            assert!(validate_undo_record(&record).is_err(), "malformed DACL metadata must be rejected");
        }

        record.original_windows_dacl = Some("ab".repeat(20));
        assert!(validate_undo_record(&record).is_ok(), "bounded hexadecimal DACL metadata is structurally accepted");
    }

    #[test]
    fn replace_file_failure_snapshots_reconcile_all_documented_windows_outcomes() {
        // ReplaceFileW documents 1175/1176 as leaving the target and replacement
        // names intact, and 1177 as moving the replaced file to the supplied
        // backup name while the replacement remains at its temporary name.
        // These snapshots exercise FRIDAY's recovery decisions, not API fault injection.
        for error_code in [1175u32, 1176, 1177] {
            let (root, file, record, original, proposed) = recovery_fixture(&format!("replace-apply-{error_code}"));
            let backup_dir = root.join(".friday-code-backups");
            let journal = backup_dir.join(format!("{}.txn.json", record.id));
            let temporary = root.join(format!(".friday-code-edit-{}.tmp", record.id));
            let displaced = root.join(format!(".friday-code-original-{}.tmp", record.id));
            fs::write(&journal, serde_json::to_vec(&record).unwrap()).unwrap();
            fs::write(&temporary, &proposed).unwrap();
            if error_code == 1177 {
                fs::write(&displaced, &original).unwrap();
            } else {
                fs::write(&file, &original).unwrap();
            }
            assert_eq!(recover_project_code_transactions(&root).unwrap(), 1, "Apply error {error_code}");
            assert_eq!(fs::read(&file).unwrap(), original, "Apply error {error_code}");
            assert!(!temporary.exists(), "Apply temporary for error {error_code}");
            assert!(!displaced.exists(), "Apply displaced file for error {error_code}");
            assert!(!journal.exists(), "Apply journal for error {error_code}");
            fs::remove_dir_all(&root).unwrap();
        }

        for error_code in [1175u32, 1176, 1177] {
            let (root, file, record, original, proposed) = recovery_fixture(&format!("replace-undo-{error_code}"));
            let backup_dir = root.join(".friday-code-backups");
            let journal = backup_dir.join(format!("{}.undo-txn.json", record.id));
            let active = backup_dir.join(format!("{}.undo.json", record.id));
            let temporary = root.join(format!(".friday-code-undo-{}.tmp", record.id));
            let displaced = root.join(format!(".friday-code-undone-{}.tmp", record.id));
            fs::write(&active, serde_json::to_vec(&record).unwrap()).unwrap();
            fs::write(&journal, serde_json::to_vec(&record).unwrap()).unwrap();
            fs::write(&temporary, &original).unwrap();
            if error_code == 1177 {
                fs::write(&displaced, &proposed).unwrap();
            } else {
                fs::write(&file, &proposed).unwrap();
            }
            assert_eq!(recover_project_code_transactions(&root).unwrap(), 1, "Undo error {error_code}");
            assert_eq!(fs::read(&file).unwrap(), proposed, "Undo error {error_code}");
            assert!(active.is_file(), "Undo must remain available after error {error_code}");
            assert!(!backup_dir.join(format!("{}.undone.json", record.id)).exists());
            assert!(!temporary.exists(), "Undo temporary for error {error_code}");
            assert!(!displaced.exists(), "Undo displaced file for error {error_code}");
            assert!(!journal.exists(), "Undo journal for error {error_code}");
            fs::remove_dir_all(&root).unwrap();
        }
    }

    #[test]
    fn recovery_handles_apply_crash_at_each_transaction_boundary() {
        // Snapshots model abrupt process termination after the journal, temp write,
        // first rename, second rename, and post-install verification/undo persistence.
        for stage in ["journal", "temporary", "first-rename", "second-rename", "verified"] {
            let (root, file, record, original, proposed) = recovery_fixture(stage);
            let backup_dir = root.join(".friday-code-backups");
            fs::write(backup_dir.join(format!("{}.txn.json", record.id)), serde_json::to_vec(&record).unwrap()).unwrap();
            match stage {
                "journal" => fs::write(&file, &original).unwrap(),
                "temporary" => {
                    fs::write(&file, &original).unwrap();
                    fs::write(root.join(format!(".friday-code-edit-{}.tmp", record.id)), &proposed).unwrap();
                }
                "first-rename" => {
                    fs::write(root.join(format!(".friday-code-original-{}.tmp", record.id)), &original).unwrap();
                    fs::write(root.join(format!(".friday-code-edit-{}.tmp", record.id)), &proposed).unwrap();
                }
                "second-rename" | "verified" => {
                    fs::write(&file, &proposed).unwrap();
                    fs::write(root.join(format!(".friday-code-original-{}.tmp", record.id)), &original).unwrap();
                }
                _ => unreachable!(),
            }
            assert_eq!(recover_project_code_transactions(&root).unwrap(), 1, "stage {stage}");
            let installed = fs::read(&file).unwrap();
            if stage == "second-rename" || stage == "verified" {
                assert_eq!(installed, proposed, "stage {stage}");
                let undo = read_recovery_file(&backup_dir.join(format!("{}.undo.json", record.id)), 16 * 1024).unwrap().unwrap();
                let saved: CodingUndoRecord = serde_json::from_slice(&undo).unwrap();
                assert_eq!(saved.resulting_sha256, record.resulting_sha256);
            } else {
                assert_eq!(installed, original, "stage {stage}");
                assert!(!backup_dir.join(format!("{}.undo.json", record.id)).exists());
            }
            assert!(!backup_dir.join(format!("{}.txn.json", record.id)).exists(), "stage {stage}");
            assert!(!root.join(format!(".friday-code-edit-{}.tmp", record.id)).exists(), "stage {stage}");
            assert!(!root.join(format!(".friday-code-original-{}.tmp", record.id)).exists(), "stage {stage}");
            fs::remove_dir_all(&root).unwrap();
        }
    }

    #[test]
    fn recovery_handles_undo_crash_at_each_transaction_boundary() {
        // Snapshots model abrupt termination after the journal, temp write,
        // first rename, second rename, and before/after consuming the Undo record.
        for stage in ["journal", "temporary", "first-rename", "second-rename", "verified"] {
            let (root, file, record, original, proposed) = recovery_fixture(stage);
            let backup_dir = root.join(".friday-code-backups");
            fs::write(backup_dir.join(format!("{}.undo.json", record.id)), serde_json::to_vec(&record).unwrap()).unwrap();
            fs::write(backup_dir.join(format!("{}.undo-txn.json", record.id)), serde_json::to_vec(&record).unwrap()).unwrap();
            match stage {
                "journal" => fs::write(&file, &proposed).unwrap(),
                "temporary" => {
                    fs::write(&file, &proposed).unwrap();
                    fs::write(root.join(format!(".friday-code-undo-{}.tmp", record.id)), &original).unwrap();
                }
                "first-rename" => {
                    fs::write(root.join(format!(".friday-code-undone-{}.tmp", record.id)), &proposed).unwrap();
                    fs::write(root.join(format!(".friday-code-undo-{}.tmp", record.id)), &original).unwrap();
                }
                "second-rename" | "verified" => {
                    fs::write(&file, &original).unwrap();
                    fs::write(root.join(format!(".friday-code-undone-{}.tmp", record.id)), &proposed).unwrap();
                }
                _ => unreachable!(),
            }
            assert_eq!(recover_project_code_transactions(&root).unwrap(), 1, "stage {stage}");
            if stage == "second-rename" || stage == "verified" {
                assert_eq!(fs::read(&file).unwrap(), original, "stage {stage}");
                assert!(backup_dir.join(format!("{}.undone.json", record.id)).is_file(), "stage {stage}");
                assert!(!backup_dir.join(format!("{}.undo.json", record.id)).exists(), "stage {stage}");
            } else {
                assert_eq!(fs::read(&file).unwrap(), proposed, "stage {stage}");
                assert!(backup_dir.join(format!("{}.undo.json", record.id)).is_file(), "stage {stage}");
                assert!(!backup_dir.join(format!("{}.undone.json", record.id)).exists(), "stage {stage}");
            }
            assert!(!backup_dir.join(format!("{}.undo-txn.json", record.id)).exists(), "stage {stage}");
            assert!(backup_dir.join(format!("{}.undo-recovered.json", record.id)).is_file(), "stage {stage}");
            // Recovery artifacts are retained as evidence; cleanup is a separate, fail-closed operation.
            if stage == "temporary" || stage == "first-rename" { assert!(root.join(format!(".friday-code-undo-{}.tmp", record.id)).exists(), "retained Undo temporary for {stage}"); }
            if stage == "second-rename" || stage == "verified" { assert!(root.join(format!(".friday-code-undone-{}.tmp", record.id)).exists(), "retained displaced copy for {stage}"); }
            fs::remove_dir_all(&root).unwrap();
        }
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn installed_code_verification_binds_content_to_the_expected_file_identity() {
        let root = std::env::temp_dir().join(format!("friday-installed-identity-{}", make_token()));
        fs::create_dir_all(&root).unwrap();
        let parent = crate::coding_sandbox_fs::open_verified_root_directory(&root).unwrap();
        let installed = root.join("installed.tmp");
        fs::write(&installed, b"verified replacement").unwrap();
        let identity = verified_code_child_identity(&parent, OsStr::new("installed.tmp")).unwrap();
        assert!(verify_installed_code_file(&parent, OsStr::new("installed.tmp"), identity, &sha256_hex(b"verified replacement")).is_ok());
        assert!(verify_installed_code_file(&parent, OsStr::new("installed.tmp"), identity, &sha256_hex(b"other")).is_err());
        drop(parent);
        fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn installed_code_verification_rejects_symlink_substitution_without_reading_target() {
        use std::os::unix::fs::symlink;
        let root = std::env::temp_dir().join(format!("friday-installed-link-{}", make_token()));
        let outside = std::env::temp_dir().join(format!("friday-installed-outside-{}", make_token()));
        fs::create_dir_all(&root).unwrap();
        let parent = crate::coding_sandbox_fs::open_verified_root_directory(&root).unwrap();
        let installed = root.join("installed.tmp");
        fs::write(&installed, b"verified replacement").unwrap();
        let identity = verified_code_child_identity(&parent, OsStr::new("installed.tmp")).unwrap();
        fs::write(&outside, b"outside private data").unwrap();
        fs::remove_file(&installed).unwrap();
        symlink(&outside, &installed).unwrap();
        assert!(verify_installed_code_file(&parent, OsStr::new("installed.tmp"), identity, &sha256_hex(b"outside private data")).is_err());
        assert_eq!(fs::read(&outside).unwrap(), b"outside private data");
        fs::remove_dir_all(&root).unwrap();
        fs::remove_file(&outside).unwrap();
    }

    #[test]
    fn guarded_code_apply_refuses_stale_original_without_modifying_it() {
        let root = std::env::temp_dir().join(format!("friday-code-stale-{}", make_token()));
        fs::create_dir_all(&root).unwrap();
        let file = root.join("main.rs");
        fs::write(&file, "externally changed").unwrap();
        let result = apply_code_preview_to_disk(&root, &file, b"proposed", &sha256_hex(b"old"), &make_token(), 123);
        assert!(result.is_err());
        assert_eq!(fs::read_to_string(&file).unwrap(), "externally changed");
        assert!(!root.join(".friday-code-backups").exists());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn guarded_code_apply_refuses_symlink_targets() {
        let root = std::env::temp_dir().join(format!("friday-code-link-{}", make_token()));
        let outside = std::env::temp_dir().join(format!("friday-code-outside-{}", make_token()));
        fs::create_dir_all(&root).unwrap();
        fs::write(&outside, "outside").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, root.join("linked.rs")).unwrap();
        #[cfg(windows)]
        { let _ = fs::remove_dir_all(&root); let _ = fs::remove_file(&outside); return; }
        #[cfg(unix)]
        {
            let result = apply_code_preview_to_disk(&root, &root.join("linked.rs"), b"changed", &sha256_hex(b"outside"), &make_token(), 123);
            assert!(result.is_err());
            assert_eq!(fs::read_to_string(&outside).unwrap(), "outside");
        }
        fs::remove_dir_all(&root).unwrap();
        fs::remove_file(&outside).unwrap();
    }

    #[test]
    fn sha256_hash_is_stable_and_content_sensitive() {
        assert_eq!(sha256_hex(b"hello"), sha256_hex(b"hello"));
        assert_ne!(sha256_hex(b"hello"), sha256_hex(b"world"));
    }

    #[test]
    fn relative_paths_are_project_scoped() {
        let root = Path::new("/workspace/project");
        assert_eq!(relative_path(root, Path::new("/workspace/project/src/main.rs")).as_deref(), Some("src/main.rs"));
        assert!(relative_path(root, Path::new("/workspace/other/main.rs")).is_none());
    }
 
    fn write_history_fixture(root: &Path, record: &CodingUndoRecord, original: &[u8]) -> PathBuf {
        let backup_dir = ensure_code_backup_dir(root).unwrap();
        fs::write(backup_dir.join(format!("{}.bak", record.id)), original).unwrap();
        backup_dir
    }

    #[test]
    fn persistent_history_lists_valid_active_record_and_enables_undo() {
        let (root, file, backup_dir, record, original, proposed) =
            recovery_edge_fixture("history-active", b"edge proposed source");
        write_history_fixture(&root, &record, &original);
        fs::write(&file, &proposed).unwrap();
        fs::write(backup_dir.join(format!("{}.undo.json", record.id)), serde_json::to_vec(&record).unwrap()).unwrap();
        let history = list_code_change_history_for_root(&root).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].status, "applied");
        assert!(history[0].undo_available);
        assert_eq!(history[0].undo_record_id, record.id);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn persistent_history_blocks_undo_when_project_file_was_modified() {
        let (root, file, backup_dir, record, original, _) =
            recovery_edge_fixture("history-modified", b"edge proposed source");
        write_history_fixture(&root, &record, &original);
        fs::write(&file, b"user's newer content").unwrap();
        fs::write(backup_dir.join(format!("{}.undo.json", record.id)), serde_json::to_vec(&record).unwrap()).unwrap();
        let history = list_code_change_history_for_root(&root).unwrap();
        assert_eq!(history[0].status, "blocked");
        assert!(!history[0].undo_available);
        assert!(history[0].unavailable_reason.as_deref().unwrap().contains("changed"));
        assert_eq!(fs::read(&file).unwrap(), b"user's newer content");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn persistent_history_blocks_undo_when_original_backup_is_corrupted() {
        let (root, file, backup_dir, record, _, proposed) =
            recovery_edge_fixture("history-corrupt-backup", b"edge proposed source");
        fs::write(&file, &proposed).unwrap();
        fs::write(backup_dir.join(format!("{}.bak", record.id)), b"corrupted backup").unwrap();
        fs::write(backup_dir.join(format!("{}.undo.json", record.id)), serde_json::to_vec(&record).unwrap()).unwrap();
        let history = list_code_change_history_for_root(&root).unwrap();
        assert_eq!(history[0].status, "blocked");
        assert!(!history[0].undo_available);
        assert!(history[0].unavailable_reason.as_deref().unwrap().contains("integrity"));
        assert_eq!(fs::read(&file).unwrap(), proposed);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn persistent_history_blocks_undo_when_original_backup_is_missing() {
        let (root, file, backup_dir, record, _, proposed) =
            recovery_edge_fixture("history-missing-backup", b"edge proposed source");
        fs::remove_file(backup_dir.join(format!("{}.bak", record.id))).unwrap();
        fs::write(&file, &proposed).unwrap();
        fs::write(backup_dir.join(format!("{}.undo.json", record.id)), serde_json::to_vec(&record).unwrap()).unwrap();
        let history = list_code_change_history_for_root(&root).unwrap();
        assert_eq!(history[0].status, "blocked");
        assert!(!history[0].undo_available);
        assert!(history[0].unavailable_reason.as_deref().unwrap().contains("backup is missing"));
        assert_eq!(fs::read(&file).unwrap(), proposed);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn persistent_history_rejects_conflicting_active_and_consumed_records() {
        let (root, _, backup_dir, record, _, _) =
            recovery_edge_fixture("history-conflicting-states", b"edge original source");
        let bytes = serde_json::to_vec(&record).unwrap();
        fs::write(backup_dir.join(format!("{}.undo.json", record.id)), &bytes).unwrap();
        fs::write(backup_dir.join(format!("{}.undone.json", record.id)), &bytes).unwrap();
        assert!(list_code_change_history_for_root(&root).is_err());
        assert!(backup_dir.join(format!("{}.undo.json", record.id)).is_file());
        assert!(backup_dir.join(format!("{}.undone.json", record.id)).is_file());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn persistent_history_lists_consumed_records_as_undone() {
        let (root, file, backup_dir, record, original, _) =
            recovery_edge_fixture("history-undone", b"edge original source");
        write_history_fixture(&root, &record, &original);
        fs::write(backup_dir.join(format!("{}.undone.json", record.id)), serde_json::to_vec(&record).unwrap()).unwrap();
        let history = list_code_change_history_for_root(&root).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].status, "undone");
        assert!(!history[0].undo_available);
        assert_eq!(fs::read(&file).unwrap(), original);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn persistent_history_refuses_malformed_record_and_preserves_evidence() {
        let (root, _, backup_dir, record, _, _) =
            recovery_edge_fixture("history-malformed", b"edge proposed source");
        let active = backup_dir.join(format!("{}.undo.json", record.id));
        fs::write(&active, b"{ malformed history").unwrap();
        assert!(list_code_change_history_for_root(&root).is_err());
        assert!(active.is_file());
        assert_eq!(fs::read(&active).unwrap(), b"{ malformed history");
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn persistent_history_rejects_symlinked_record_without_following_it() {
        let (root, _, backup_dir, record, _, _) =
            recovery_edge_fixture("history-symlink", b"edge proposed source");
        let outside = root.parent().unwrap().join(format!("friday-history-outside-{}", make_token()));
        fs::write(&outside, b"outside user data").unwrap();
        let active = backup_dir.join(format!("{}.undo.json", record.id));
        std::os::unix::fs::symlink(&outside, &active).unwrap();
        assert!(list_code_change_history_for_root(&root).is_err());
        assert_eq!(fs::read(&outside).unwrap(), b"outside user data");
        fs::remove_dir_all(root).unwrap();
        fs::remove_file(outside).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_backup_recovery_restores_the_original_custom_file_dacl() {
        use std::process::Command;
        let (root, file, record, original, _) = recovery_fixture("windows-dacl-recovery");
        fs::write(&file, &original).unwrap();
        let whoami = Command::new("whoami").output().expect("whoami must be available");
        assert!(whoami.status.success());
        let account = String::from_utf8_lossy(&whoami.stdout).trim().to_string();
        let grant = format!("{account}:(F)");
        let set_acl = Command::new("icacls")
            .arg(&file)
            .args(["/inheritance:r", "/grant:r", &grant])
            .output().expect("icacls must be available");
        assert!(set_acl.status.success(), "{}", String::from_utf8_lossy(&set_acl.stderr));
        let snapshot_acl = |path: &Path| -> String {
            let output = Command::new("icacls").arg(path).output().expect("icacls must be available");
            assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
            String::from_utf8_lossy(&output.stdout).lines()
                .filter(|line| !line.trim().is_empty()).collect::<Vec<_>>().join("\n")
        };
        let mut record = record;
        record.original_windows_dacl = original_windows_dacl(&file).unwrap();
        let expected_acl = snapshot_acl(&file);
        fs::write(root.join(".friday-code-backups").join(format!("{}.txn.json", record.id)),
            serde_json::to_vec(&record).unwrap()).unwrap();
        fs::remove_file(&file).unwrap();
        assert_eq!(recover_project_code_transactions(&root).unwrap(), 1);
        assert_eq!(fs::read(&file).unwrap(), original);
        assert_eq!(snapshot_acl(&file), expected_acl, "backup recovery must restore the original DACL");
        fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_apply_and_undo_preserve_a_custom_file_dacl() {
        use std::process::Command;
        let root = std::env::temp_dir().join(format!("friday-code-windows-acl-{}", make_token()));
        fs::create_dir_all(&root).unwrap();
        let root = fs::canonicalize(&root).unwrap();
        let file = root.join("main.rs");
        let original = b"original source";
        let proposed = b"approved replacement";
        fs::write(&file, original).unwrap();

        let whoami = Command::new("whoami").output().expect("whoami must be available");
        assert!(whoami.status.success());
        let account = String::from_utf8_lossy(&whoami.stdout).trim().to_string();
        let grant = format!("{account}:(F)");
        let set_acl = Command::new("icacls")
            .arg(&file)
            .args(["/inheritance:r", "/grant:r", &grant])
            .output()
            .expect("icacls must be available on Windows");
        assert!(set_acl.status.success(), "{}", String::from_utf8_lossy(&set_acl.stderr));

        let snapshot_acl = |path: &Path| -> String {
            let output = Command::new("icacls").arg(path).output().expect("icacls must be available");
            assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter(|line| !line.trim().is_empty())
                .collect::<Vec<_>>()
                .join("\n")
        };
        let original_acl = snapshot_acl(&file);
        let (_, record) = apply_code_preview_to_disk(
            &root, &file, proposed, &sha256_hex(original), &make_token(), 123,
        ).unwrap();
        assert_eq!(snapshot_acl(&file), original_acl, "Apply must preserve the original DACL");

        undo_code_change_on_disk(&root, &record).unwrap();
        assert_eq!(fs::read(&file).unwrap(), original);
        assert_eq!(snapshot_acl(&file), original_acl, "Undo must preserve the original DACL");
        fs::remove_dir_all(&root).unwrap();
    }


}

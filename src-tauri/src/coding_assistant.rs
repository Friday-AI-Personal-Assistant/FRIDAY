use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    fs::OpenOptions,
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
}

static PROJECT_GRANTS: Mutex<BTreeMap<String, ProjectGrant>> = Mutex::new(BTreeMap::new());

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

fn apply_code_preview_to_disk(root: &Path, current: &Path, proposed: &[u8], expected_sha256: &str, token: &str, now: u64)
    -> Result<(PathBuf, CodingUndoRecord), String>
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
    let original = fs::read(&canonical).map_err(|_| "Could not re-read the selected project file.".to_string())?;
    if original.len() as u64 != meta.len() || original.len() as u64 > MAX_FILE_BYTES || original.contains(&0)
        || std::str::from_utf8(&original).is_err() {
        return Err("The selected project file changed or is no longer bounded UTF-8 text.".into());
    }
    let original_hash = sha256_hex(&original);
    if original_hash != expected_sha256 { return Err("The file changed after preview. No changes were made; create a fresh preview.".into()); }
    let resulting_hash = sha256_hex(proposed);
    if resulting_hash == original_hash { return Err("The proposed content matches the original; there is nothing to apply.".into()); }

    let backup_dir = ensure_code_backup_dir(root)?;
    check_code_backup_capacity(&backup_dir, (original.len() as u64).saturating_add(16_384))?;
    let backup = backup_dir.join(format!("{token}.bak"));
    let mut backup_file = OpenOptions::new().write(true).create_new(true).open(&backup)
        .map_err(|e| format!("Could not create a new original-file backup: {e}"))?;
    if let Err(e) = backup_file.write_all(&original).and_then(|_| backup_file.sync_all()) {
        drop(backup_file); let _ = fs::remove_file(&backup);
        return Err(format!("Could not write the original-file backup: {e}"));
    }
    drop(backup_file);
    let verified_backup = fs::read(&backup).map_err(|_| "Could not verify the original-file backup.".to_string())?;
    if sha256_hex(&verified_backup) != original_hash {
        return Err("The original-file backup failed SHA-256 verification. The project file was not changed.".into());
    }

    let relative = relative_path(root, current).ok_or("The project file is outside the selected folder.")?;
    let record = CodingUndoRecord {
        id: token.to_string(), relative_path: relative.clone(),
        backup_relative_path: format!(".friday-code-backups/{token}.bak"),
        original_sha256: original_hash.clone(), resulting_sha256: resulting_hash.clone(), created_at: now,
        original_unix_mode: original_unix_mode(&meta),
    };
    let journal_path = backup_dir.join(format!("{token}.txn.json"));
    let journal_bytes = serde_json::to_vec(&record).map_err(|_| "Could not prepare the code-change recovery journal.".to_string())?;
    let mut journal = OpenOptions::new().write(true).create_new(true).open(&journal_path)
        .map_err(|e| format!("Could not create the code-change recovery journal: {e}"))?;
    journal.write_all(&journal_bytes).and_then(|_| journal.sync_all())
        .map_err(|e| format!("Could not flush the code-change recovery journal: {e}"))?;
    drop(journal);

    let parent = current.parent().ok_or("The selected project file has no parent folder.")?;
    let temporary = parent.join(format!(".friday-code-edit-{token}.tmp"));
    let mut temp_file = OpenOptions::new().write(true).create_new(true).open(&temporary)
        .map_err(|e| format!("Could not prepare the replacement file: {e}"))?;
    if let Err(e) = temp_file.set_permissions(meta.permissions()).and_then(|_| temp_file.write_all(proposed)).and_then(|_| temp_file.sync_all()) {
        drop(temp_file); let _ = fs::remove_file(&temporary);
        return Err(format!("Could not prepare the replacement contents: {e}"));
    }
    drop(temp_file);
    let temp_bytes = fs::read(&temporary).map_err(|_| "Could not verify the replacement file.".to_string())?;
    if sha256_hex(&temp_bytes) != resulting_hash {
        let _ = fs::remove_file(&temporary);
        return Err("The replacement file failed SHA-256 verification. The original was not changed.".into());
    }

    let displaced = parent.join(format!(".friday-code-original-{token}.tmp"));
    if let Err(e) = fs::rename(current, &displaced) {
        let _ = fs::remove_file(&temporary);
        return Err(format!("Could not safely stage the original file: {e}"));
    }
    if let Err(e) = fs::rename(&temporary, current) {
        let restored = fs::rename(&displaced, current).is_ok();
        let _ = fs::remove_file(&temporary);
        return Err(if restored { format!("Could not install the approved change; the original file was restored: {e}") }
            else { format!("Could not install the change or restore the original automatically. The original remains at {}: {e}", displaced.display()) });
    }
    let installed = fs::read(current).map_err(|_| format!("The replacement was installed but could not be verified. The original backup remains at {}.", backup.display()))?;
    if sha256_hex(&installed) != resulting_hash {
        let failed = parent.join(format!(".friday-code-failed-{token}.tmp"));
        let _ = fs::rename(current, &failed);
        let restored = fs::rename(&displaced, current).is_ok();
        let _ = fs::remove_file(&failed);
        return Err(if restored { "The installed file failed verification; FRIDAY restored the original.".into() }
            else { format!("The installed file failed verification and automatic restore failed. The original backup remains at {}.", backup.display()) });
    }
    let undo_path = backup_dir.join(format!("{token}.undo.json"));
    let undo_bytes = serde_json::to_vec(&record).map_err(|_| "Could not prepare the guarded undo record.".to_string())?;
    let mut undo_file = OpenOptions::new().write(true).create_new(true).open(&undo_path)
        .map_err(|e| format!("The change was installed and verified, but its undo record could not be created: {e}. The original backup remains at {}.", backup.display()))?;
    if let Err(e) = undo_file.write_all(&undo_bytes).and_then(|_| undo_file.sync_all()) {
        drop(undo_file);
        return Err(format!("The change was installed and verified, but its undo record could not be saved: {e}. The original backup remains at {}.", backup.display()));
    }
    drop(undo_file);
    if let Err(e) = fs::remove_file(&displaced) {
        return Err(format!("The change was verified and its undo record saved, but FRIDAY could not remove its temporary displaced copy: {e}. The verified original backup remains."));
    }
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
    Ok(())
}

fn read_recovery_file(path: &Path, limit: u64) -> Result<Option<Vec<u8>>, String> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("FRIDAY could not inspect a recovery file.".into()),
    };
    if is_redirected(&meta) || !meta.is_file() || meta.len() == 0 || meta.len() > limit {
        return Err("A recovery item is not a safe bounded regular file. No automatic cleanup was attempted.".into());
    }
    let bytes = fs::read(path).map_err(|_| "FRIDAY could not read a recovery file.".to_string())?;
    if bytes.len() as u64 != meta.len() {
        return Err("A recovery file changed while FRIDAY was reading it.".into());
    }
    Ok(Some(bytes))
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
    #[cfg(not(unix))]
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
        || saved.created_at != expected.created_at {
        return Err("An Undo state record does not match its transaction journal. Recovery evidence was preserved.".into());
    }
    Ok(Some(active_exists))
}

fn recover_undo_transaction(root: &Path, backup_dir: &Path, record: &CodingUndoRecord) -> Result<(), String> {
    validate_undo_record(record)?;
    let target = safe_recovery_target(root, record)?;
    let backup = backup_dir.join(format!("{}.bak", record.id));
    let original = read_recovery_file(&backup, MAX_FILE_BYTES)?
        .ok_or("The original backup for an interrupted Undo is missing. Recovery files were preserved.")?;
    if sha256_hex(&original) != record.original_sha256 {
        return Err("The original backup for an interrupted Undo failed SHA-256 verification. Recovery files were preserved.".into());
    }
    let target_hash = recovery_hash(&target, MAX_FILE_BYTES)?;
    let displaced = target.parent().unwrap().join(format!(".friday-code-undone-{}.tmp", record.id));
    let temporary = target.parent().unwrap().join(format!(".friday-code-undo-{}.tmp", record.id));
    let displaced_hash = recovery_hash(&displaced, MAX_FILE_BYTES)?;
    let temporary_hash = recovery_hash(&temporary, MAX_FILE_BYTES)?;

    match target_hash.as_deref() {
        Some(hash) if hash == record.original_sha256 => {
            if let Some(hash) = displaced_hash.as_deref() { if hash != record.resulting_sha256 { return Err("An interrupted Undo left an unexpected displaced file. Nothing was deleted.".into()); } }
            if let Some(hash) = temporary_hash.as_deref() { if hash != record.original_sha256 { return Err("An interrupted Undo left an unexpected temporary file. Nothing was deleted.".into()); } }
            match inspect_recovery_undo_state(backup_dir, record)? {
                Some(true) => {
                    let undo_path = backup_dir.join(format!("{}.undo.json", record.id));
                    let consumed = backup_dir.join(format!("{}.undone.json", record.id));
                    fs::rename(&undo_path, &consumed)
                        .map_err(|_| "FRIDAY could not mark the recovered Undo as consumed.".to_string())?;
                }
                Some(false) => {},
                None => return Err("The interrupted Undo has no valid active or consumed Undo record. Recovery evidence was preserved.".into()),
            }
            if displaced_hash.is_some() { remove_recovery_copy(&displaced, &record.resulting_sha256)?; }
            if temporary_hash.is_some() { remove_recovery_copy(&temporary, &record.original_sha256)?; }
        }
        Some(hash) if hash == record.resulting_sha256 => {
            if inspect_recovery_undo_state(backup_dir, record)? != Some(true) {
                return Err("The edited target does not have one valid active Undo record. Recovery evidence was preserved.".into());
            }
            if displaced_hash.is_some() {
                return Err("An interrupted Undo has both the edited target and a displaced copy. FRIDAY preserved all files for manual review.".into());
            }
            if let Some(hash) = temporary_hash.as_deref() {
                if hash != record.original_sha256 { return Err("An interrupted Undo temporary file has unexpected contents.".into()); }
                remove_recovery_copy(&temporary, &record.original_sha256)?;
            }
        }
        None => {
            if let Some(hash) = displaced_hash.as_deref() {
                if inspect_recovery_undo_state(backup_dir, record)? != Some(true) {
                    return Err("The displaced edited file does not have one valid active Undo record. Recovery evidence was preserved.".into());
                }
                if hash != record.resulting_sha256 { return Err("The displaced file from an interrupted Undo failed verification.".into()); }
                fs::rename(&displaced, &target)
                    .map_err(|_| "FRIDAY could not restore the edited file after an interrupted Undo. Recovery copies were preserved.".to_string())?;
                if temporary_hash.is_some() { remove_recovery_copy(&temporary, &record.original_sha256)?; }
            } else if temporary_hash.as_deref() == Some(record.original_sha256.as_str()) {
                let undo_state = inspect_recovery_undo_state(backup_dir, record)?
                    .ok_or("The interrupted Undo has no valid Undo state record. Recovery evidence was preserved.")?;
                fs::rename(&temporary, &target)
                    .map_err(|_| "FRIDAY could not finish restoring the verified original after an interrupted Undo.".to_string())?;
                if undo_state {
                    let undo_path = backup_dir.join(format!("{}.undo.json", record.id));
                    let consumed = backup_dir.join(format!("{}.undone.json", record.id));
                    fs::rename(&undo_path, &consumed)
                        .map_err(|_| "FRIDAY restored the original but could not mark Undo as consumed.".to_string())?;
                }
            } else {
                return Err("The target and displaced copy are missing after an interrupted Undo. FRIDAY preserved the verified backup; manual recovery is required.".into());
            }
        }
        Some(_) => return Err("The project file has contents that match neither side of an interrupted Undo. FRIDAY preserved every recovery copy.".into()),
    }
    let journal = backup_dir.join(format!("{}.undo-txn.json", record.id));
    fs::remove_file(journal).map_err(|_| "FRIDAY completed recovery but could not remove the resolved Undo journal.".to_string())?;
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
    let current = fs::read(&canonical_target)
        .map_err(|_| "FRIDAY could not verify the current edited file.".to_string())?;
    if current.len() as u64 != metadata.len() || current.len() as u64 > MAX_FILE_BYTES
        || current.contains(&0) || std::str::from_utf8(&current).is_err() {
        return Err("The current edited file is no longer bounded UTF-8 text. Undo was refused.".into());
    }
    if sha256_hex(&current) != record.resulting_sha256 {
        return Err("This file has changed since FRIDAY edited it. Undo will not overwrite your newer changes.".into());
    }

    let backup_dir = ensure_code_backup_dir(&canonical_root)?;
    let backup = backup_dir.join(format!("{}.bak", record.id));
    let backup_meta = fs::symlink_metadata(&backup)
        .map_err(|_| "The verified original backup is missing. Undo was refused.".to_string())?;
    if is_redirected(&backup_meta) || !backup_meta.is_file() || backup_meta.len() == 0
        || backup_meta.len() > MAX_FILE_BYTES {
        return Err("The original backup is not a safe regular file. Undo was refused.".into());
    }
    let original = fs::read(&backup)
        .map_err(|_| "FRIDAY could not read the original backup.".to_string())?;
    if original.len() as u64 != backup_meta.len() || original.contains(&0)
        || std::str::from_utf8(&original).is_err() || sha256_hex(&original) != record.original_sha256 {
        return Err("The original backup failed integrity verification. Undo was refused.".into());
    }

    let journal_path = backup_dir.join(format!("{}.undo-txn.json", record.id));
    let journal_bytes = serde_json::to_vec(record)
        .map_err(|_| "FRIDAY could not prepare the undo recovery journal.".to_string())?;
    let mut journal = OpenOptions::new().write(true).create_new(true).open(&journal_path)
        .map_err(|e| format!("Could not create the undo recovery journal: {e}"))?;
    journal.write_all(&journal_bytes).and_then(|_| journal.sync_all())
        .map_err(|e| format!("Could not flush the undo recovery journal: {e}"))?;
    drop(journal);

    let parent = canonical_target.parent().ok_or("The edited file has no parent directory.")?;
    let temporary = parent.join(format!(".friday-code-undo-{}.tmp", record.id));
    let mut temp_file = OpenOptions::new().write(true).create_new(true).open(&temporary)
        .map_err(|e| format!("Could not prepare the original-file restoration: {e}"))?;
    if let Err(error) = temp_file.set_permissions(metadata.permissions())
        .and_then(|_| temp_file.write_all(&original))
        .and_then(|_| temp_file.sync_all()) {
        drop(temp_file);
        let _ = fs::remove_file(&temporary);
        return Err(format!("Could not prepare the original-file restoration: {error}"));
    }
    drop(temp_file);
    let prepared = fs::read(&temporary)
        .map_err(|_| "Could not verify the prepared original-file restoration.".to_string())?;
    if sha256_hex(&prepared) != record.original_sha256 {
        let _ = fs::remove_file(&temporary);
        return Err("The prepared original-file restoration failed SHA-256 verification.".into());
    }

    let displaced = parent.join(format!(".friday-code-undone-{}.tmp", record.id));
    fs::rename(&canonical_target, &displaced)
        .map_err(|e| format!("Could not safely stage the current edited file: {e}"))?;
    if let Err(error) = fs::rename(&temporary, &canonical_target) {
        let restored = fs::rename(&displaced, &canonical_target).is_ok();
        let _ = fs::remove_file(&temporary);
        return Err(if restored {
            format!("Could not restore the original file; the edited file was put back: {error}")
        } else {
            format!("Undo installation and automatic rollback both failed. The edited file remains at {} and the original backup remains at {}.",
                displaced.display(), backup.display())
        });
    }
    let restored_bytes = fs::read(&canonical_target)
        .map_err(|_| format!("The original was installed but could not be verified. The backup remains at {}.", backup.display()))?;
    if sha256_hex(&restored_bytes) != record.original_sha256 {
        let failed = parent.join(format!(".friday-code-undo-failed-{}.tmp", record.id));
        let _ = fs::rename(&canonical_target, &failed);
        let rolled_back = fs::rename(&displaced, &canonical_target).is_ok();
        let _ = fs::remove_file(&failed);
        return Err(if rolled_back {
            "The restored file failed verification; FRIDAY put the edited version back.".into()
        } else {
            format!("The restored file failed verification and rollback failed. Recovery copies remain in {}.", backup.display())
        });
    }

    let undo_path = backup_dir.join(format!("{}.undo.json", record.id));
    let consumed_path = backup_dir.join(format!("{}.undone.json", record.id));
    fs::rename(&undo_path, &consumed_path)
        .map_err(|e| format!("The original was restored and verified, but FRIDAY could not mark this undo as consumed: {e}. A repeat undo will still be blocked by the file hash check."))?;
    fs::remove_file(&displaced)
        .map_err(|e| format!("The original was restored and verified, but FRIDAY could not clean up its staged copy: {e}"))?;
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
    current: &Path,
    root: &Path,
    depth: usize,
    scanned: &mut usize,
    candidates: &mut Vec<CodingFileCandidate>,
    grants: &mut BTreeMap<String, ProjectGrant>,
) -> Result<(), String> {
    if depth > MAX_PROJECT_DEPTH || candidates.len() >= MAX_PROJECT_CANDIDATES || *scanned >= MAX_SCANNED_ENTRIES {
        return Ok(());
    }
    let entries = fs::read_dir(current).map_err(|_| "FRIDAY could not read part of the selected project folder.".to_string())?;
    for entry in entries {
        if *scanned >= MAX_SCANNED_ENTRIES || candidates.len() >= MAX_PROJECT_CANDIDATES { break; }
        *scanned += 1;
        let entry = match entry { Ok(value) => value, Err(_) => continue };
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let metadata = match fs::symlink_metadata(&path) { Ok(value) => value, Err(_) => continue };
        if is_redirected(&metadata) { continue; }
        if metadata.is_dir() {
            if depth < MAX_PROJECT_DEPTH && !ignored_directory(&name) {
                let canonical = match fs::canonicalize(&path) { Ok(value) => value, Err(_) => continue };
                if canonical.starts_with(root) {
                    scan_folder(&canonical, root, depth + 1, scanned, candidates, grants)?;
                }
            }
            continue;
        }
        if !metadata.is_file() || !supported_project_file(&path) || metadata.len() == 0 || metadata.len() > MAX_FILE_BYTES {
            continue;
        }
        let canonical = match fs::canonicalize(&path) { Ok(value) => value, Err(_) => continue };
        if !canonical.starts_with(root) { continue; }
        let relative = match relative_path(root, &canonical) { Some(value) if !value.is_empty() => value, _ => continue };
        let id = make_token();
        candidates.push(CodingFileCandidate {
            id: id.clone(),
            name,
            relative_path: relative,
            size_bytes: metadata.len(),
            language: language_for(&canonical),
        });
        grants.insert(id, ProjectGrant { root: root.to_path_buf(), path: canonical });
    }
    Ok(())
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
    let mut scanned = 0usize;
    scan_folder(&root, &root, 0, &mut scanned, &mut candidates, &mut grants)?;
    candidates.sort_by(|a, b| a.relative_path.to_lowercase().cmp(&b.relative_path.to_lowercase()));
    let truncated = scanned >= MAX_SCANNED_ENTRIES || candidates.len() >= MAX_PROJECT_CANDIDATES;
    *PROJECT_GRANTS.lock().map_err(|_| "Coding Assistant project access is temporarily unavailable.".to_string())? = grants;
    let project_name = root.file_name().and_then(|value| value.to_str()).unwrap_or("Selected project").chars().take(160).collect();
    let _ = crate::tools::record(&app, "coding.list_project", &format!("Listed {} bounded source/configuration files from a folder explicitly selected by the user; recovered {} interrupted code transaction(s).", candidates.len(), recovered_transactions), true);
    Ok(CodingProjectScan { project_name, files: candidates, scanned_entries: scanned, truncated })
}


#[tauri::command]
pub async fn preview_code_change(file_id: String, proposed_content: String) -> Result<CodingChangePreview, String> {
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
    let metadata = fs::symlink_metadata(&grant.path)
        .map_err(|_| "The selected project file is no longer available. Select it again.".to_string())?;
    if is_redirected(&metadata) || !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_FILE_BYTES {
        return Err("The selected project file is no longer an allowed regular file.".into());
    }
    if !supported_project_file(&grant.path) {
        return Err("This file type is not permitted for a Coding Assistant preview.".into());
    }
    let canonical = fs::canonicalize(&grant.path)
        .map_err(|_| "FRIDAY could not revalidate the selected project file.".to_string())?;
    if canonical != grant.path || !canonical.starts_with(&root) {
        return Err("The selected file moved or was redirected outside the approved project folder.".into());
    }
    let original_bytes = fs::read(&canonical)
        .map_err(|_| "FRIDAY could not read the current file for preview.".to_string())?;
    if original_bytes.len() as u64 > MAX_FILE_BYTES || original_bytes.len() as u64 != metadata.len() || original_bytes.contains(&0) {
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
    let relative = relative_path(&root, &canonical).ok_or("The selected file is outside the approved project folder.")?;
    let file_name = canonical.file_name().and_then(|value| value.to_str()).unwrap_or("Project file").chars().take(180).collect();
    let diff = unified_diff(&original_content, &proposed_content);
    let mut previews = CODE_PREVIEWS.lock().map_err(|_| "Coding Assistant preview storage is temporarily unavailable.".to_string())?;
    previews.retain(|_, preview| preview.expires_at > now);
    if previews.len() >= 10 {
        return Err("There are already 10 pending code previews. Wait for them to expire or select the project again.".into());
    }
    previews.insert(preview_id.clone(), StoredCodePreview {
        file_id, root, path: canonical, original_sha256: original_sha256.clone(),
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
    let current = fs::canonicalize(&grant.path).map_err(|_| "The selected project file is no longer available.".to_string())?;
    if current != preview.path || !current.starts_with(&root) {
        return Err("The selected project file moved or was redirected. No changes were made.".into());
    }
    let original = fs::read(&current).map_err(|_| "Could not re-read the selected project file.".to_string())?;
    if sha256_hex(&original) != preview.original_sha256 {
        return Err("The file changed after preview. No changes were made; create a fresh preview.".into());
    }
    let token = make_token();
    let (backup, record) = apply_code_preview_to_disk(&root, &current, preview.proposed_content.as_bytes(),
        &preview.original_sha256, &token, now)?;
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
    ensure_skill_loaded()?;
    if undo_record_id.len() != 48 || !undo_record_id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("The undo request is invalid.".into());
    }
    let grants = PROJECT_GRANTS.lock()
        .map_err(|_| "Coding Assistant project access is temporarily unavailable.".to_string())?;
    let mut selected: Option<(PathBuf, PathBuf, CodingUndoRecord)> = None;
    for grant in grants.values() {
        let root = validate_granted_root(&grant.root)?;
        let backup_dir = ensure_code_backup_dir(&root)?;
        let record_path = backup_dir.join(format!("{}.undo.json", undo_record_id));
        match fs::symlink_metadata(&record_path) {
            Ok(meta) => {
                if is_redirected(&meta) || !meta.is_file() || meta.len() == 0 || meta.len() > 16 * 1024 {
                    return Err("The guarded undo record is not a safe bounded regular file.".into());
                }
                let bytes = fs::read(&record_path)
                    .map_err(|_| "FRIDAY could not read the guarded undo record.".to_string())?;
                if bytes.len() as u64 != meta.len() {
                    return Err("The guarded undo record changed while FRIDAY was reading it.".into());
                }
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
            Err(_) => return Err("FRIDAY could not inspect the guarded undo record.".into()),
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

fn validate_proposed_content(content: &str) -> Result<(), String> {
    if content.is_empty() || content.len() > MAX_PROPOSED_FILE_BYTES || content.as_bytes().contains(&0) {
        return Err("The approved proposal is empty, too large, or contains binary NUL bytes.".into());
    }
    Ok(())
}

#[tauri::command]
pub fn cancel_code_change_preview(preview_id: String) -> Result<(), String> {
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
        let metadata = fs::symlink_metadata(&grant.path).map_err(|_| "A selected project file is no longer available.".to_string())?;
        if is_redirected(&metadata) || !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_FILE_BYTES {
            return Err("A selected project file is no longer a supported regular file.".into());
        }
        if !supported_project_file(&grant.path) {
            return Err("A selected project file is no longer an allowed source/configuration file.".into());
        }
        let canonical = fs::canonicalize(&grant.path).map_err(|_| "A selected project file is no longer available.".to_string())?;
        if canonical != grant.path || !canonical.starts_with(&root) {
            return Err("A selected project file moved outside the approved project folder.".into());
        }
        let remaining = MAX_TOTAL_BYTES.saturating_sub(total_bytes);
        if metadata.len() as usize > remaining {
            return Err("The selected files exceed the 180 KB combined project-reading limit. Select fewer files.".into());
        }
        let bytes = fs::read(&canonical).map_err(|_| "FRIDAY could not read one of the selected project files.".to_string())?;
        if bytes.len() > MAX_FILE_BYTES as usize || bytes.len() > remaining || bytes.len() as u64 != metadata.len() {
            return Err("A project file changed size while FRIDAY was reading it. Select the project files again.".into());
        }
        if bytes.contains(&0) {
            return Err("A selected project file appears to contain binary data and was not read.".into());
        }
        let content = String::from_utf8(bytes).map_err(|_| "A selected project file is not valid UTF-8 text and was not read.".to_string())?;
        let name = grant.path.file_name().and_then(|value| value.to_str()).unwrap_or("Project file").chars().take(180).collect();
        let relative = relative_path(&root, &canonical).ok_or("A selected project file is outside the approved folder.")?;
        total_bytes = total_bytes.saturating_add(content.len());
        files.push(CodingProjectFile {
            name,
            relative_path: relative,
            language: language_for(&canonical),
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
        let mut candidates = Vec::new();
        let mut grants = BTreeMap::new();
        let mut scanned = 0usize;
        scan_folder(&root, &root, 0, &mut scanned, &mut candidates, &mut grants).unwrap();
        let paths: BTreeSet<String> = candidates.iter().map(|item| item.relative_path.clone()).collect();
        assert!(paths.contains("src/main.rs"));
        assert!(paths.contains("package.json"));
        assert!(!paths.iter().any(|path| path.contains("node_modules") || path.contains("secrets") || path == ".env"));
        assert!(candidates.len() <= MAX_PROJECT_CANDIDATES);
        assert_eq!(grants.len(), candidates.len());
        fs::remove_dir_all(&temp).unwrap();
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
        assert!(root.join(".friday-code-backups").join(format!("{token}.txn.json")).is_file());
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
        };
        (root, file, record, original, proposed)
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
            assert!(!root.join(format!(".friday-code-undo-{}.tmp", record.id)).exists(), "stage {stage}");
            assert!(!root.join(format!(".friday-code-undone-{}.tmp", record.id)).exists(), "stage {stage}");
            fs::remove_dir_all(&root).unwrap();
        }
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
}

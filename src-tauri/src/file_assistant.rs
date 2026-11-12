use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};
use tauri::AppHandle;
use tauri_plugin_dialog::DialogExt;

const MAX_SELECTED_FILES: usize = 8;
const MAX_FILE_BYTES: u64 = 64 * 1024;
const MAX_BACKUP_STORAGE_BYTES: u64 = 100 * 1024 * 1024;
const MAX_BACKUP_STORAGE_FILES: usize = 500;
const MAX_TOTAL_BYTES: usize = 180 * 1024;
const MAX_FOLDER_DEPTH: usize = 3;
const MAX_SCANNED_ENTRIES: usize = 2_000;
const MAX_FOLDER_CANDIDATES: usize = 200;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectedTextFile {
    pub name: String,
    pub content: String,
    pub size_bytes: usize,
    pub grant_id: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderScanResult {
    pub folder_id: String,
    pub files: Vec<FileCandidate>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct FileCandidate {
    id: String,
    name: String,
    relative_path: String,
    size_bytes: u64,
}

struct GrantedFile {
    root: PathBuf,
    path: PathBuf,
}

struct PendingFileMove {
    grant_id: String,
    destination_relative: String,
    original_sha256: String,
    expires_at: u64,
    approval_token: Option<String>,
}

struct PendingFileEdit {
    grant_id: String,
    original_sha256: String,
    proposed_content: String,
    expires_at: u64,
    approval_token: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileEditPreview {
    pub preview_id: String,
    pub file_name: String,
    pub relative_path: String,
    pub original_content: String,
    pub proposed_content: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileMovePreview {
    pub preview_id: String,
    pub source_relative_path: String,
    pub destination_relative_path: String,
    pub file_name: String,
    pub size_bytes: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileMoveResult {
    pub file_name: String,
    pub destination_relative_path: String,
    pub backup_path: String,
    pub undo_available: bool,
    pub warning: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileEditResult {
    pub file_name: String,
    pub backup_path: String,
    pub bytes_written: usize,
    pub undo_available: bool,
    pub warning: Option<String>,
}


#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct UndoRecord {
    id: String,
    kind: String,
    original_relative_path: String,
    current_relative_path: String,
    backup_relative_path: String,
    expected_current_sha256: String,
    #[serde(default)]
    backup_sha256: Option<String>,
    created_at: u64,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct FileTransactionJournal {
    id: String,
    kind: String,
    original_relative_path: String,
    current_relative_path: String,
    backup_relative_path: String,
    expected_original_sha256: String,
    expected_current_sha256: String,
    created_at: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileRecoveryItem {
    pub id: String,
    pub kind: String,
    pub original_relative_path: String,
    pub current_relative_path: String,
    pub status: String,
    pub created_at: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileUndoOperation {
    pub id: String,
    pub kind: String,
    pub original_relative_path: String,
    pub current_relative_path: String,
    pub created_at: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileUndoPreview {
    pub preview_id: String,
    pub kind: String,
    pub original_relative_path: String,
    pub current_relative_path: String,
}

struct PendingFileUndo {
    grant_id: String,
    record: UndoRecord,
    expires_at: u64,
    approval_token: Option<String>,
}

static PENDING_FILE_UNDOS: Mutex<BTreeMap<String, PendingFileUndo>> = Mutex::new(BTreeMap::new());
static FOLDER_GRANTS: Mutex<BTreeMap<String, GrantedFile>> = Mutex::new(BTreeMap::new());
static PENDING_FILE_EDITS: Mutex<BTreeMap<String, PendingFileEdit>> = Mutex::new(BTreeMap::new());
static PENDING_FILE_MOVES: Mutex<BTreeMap<String, PendingFileMove>> = Mutex::new(BTreeMap::new());

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

fn supported_text_file(path: &Path) -> bool {
    const EXTENSIONS: &[&str] = &[
        "txt", "md", "markdown", "csv", "tsv", "json", "jsonl", "yaml", "yml",
        "toml", "ini", "cfg", "log", "rs", "ts", "tsx", "js", "jsx", "mjs",
        "cjs", "py", "html", "htm", "css", "scss", "xml", "sql", "go", "java",
        "c", "h", "cc", "cpp", "hpp", "cs", "php", "rb", "swift", "kt", "sh",
        "ps1", "bat", "cmd", "env", "gitignore",
    ];
    path.extension().and_then(|extension| extension.to_str())
        .is_some_and(|extension| EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str()))
}

fn read_selected_text_file(path: &Path, remaining_bytes: usize) -> Result<SelectedTextFile, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| "FRIDAY could not inspect one of the selected files.".to_string())?;
    if is_redirected(&metadata) || !metadata.is_file() {
        return Err("For safety, FRIDAY can read regular files only, not folders or symbolic links.".into());
    }
    if !supported_text_file(path) {
        return Err("The selected file type is not supported for text reading.".into());
    }
    if metadata.len() == 0 {
        return Err("One of the selected files is empty.".into());
    }
    if metadata.len() > MAX_FILE_BYTES {
        return Err("Each selected text file must be 64 KB or smaller.".into());
    }
    if metadata.len() as usize > remaining_bytes {
        return Err("The selected files exceed FRIDAY's 180 KB combined reading limit.".into());
    }

    let bytes = fs::read(path).map_err(|_| "FRIDAY could not read one of the selected files.".to_string())?;
    if bytes.len() > MAX_FILE_BYTES as usize || bytes.len() > remaining_bytes {
        return Err("A selected file changed size or exceeded the reading limit while being read.".into());
    }
    let content = String::from_utf8(bytes)
        .map_err(|_| "One of the selected files is not valid UTF-8 text. Binary and unsupported encodings are not read.".to_string())?;
    if content.contains('\0') {
        return Err("One of the selected files appears to contain binary data, not plain text.".into());
    }
    let name = path.file_name().and_then(|value| value.to_str())
        .unwrap_or("Selected text file").chars().take(180).collect();
    let size_bytes = content.len();
    Ok(SelectedTextFile { name, content, size_bytes, grant_id: None })
}

fn ignored_folder(name: &str) -> bool {
    name.starts_with('.') || matches!(
        name.to_ascii_lowercase().as_str(),
        "node_modules" | "target" | "dist" | "build" | "out" | "vendor"
            | ".venv" | "venv" | "__pycache__" | ".next" | ".cache"
    )
}

fn edit_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

fn content_hash(content: &[u8]) -> String {
    format!("{:x}", Sha256::digest(content))
}

fn approval_is_valid(expires_at: u64, expected: Option<&str>, provided: &str, now: u64) -> bool {
    expires_at >= now && !provided.is_empty() && expected.is_some_and(|value| value == provided)
}

fn validate_edit_content(content: &str) -> Result<(), String> {
    if content.len() > MAX_FILE_BYTES as usize {
        return Err("An edited file must remain 64 KB or smaller.".into());
    }
    if content.contains('\0') {
        return Err("FRIDAY cannot save text containing binary null characters.".into());
    }
    Ok(())
}

fn replace_file_with_backup(root: &Path, current: &Path, proposed: &[u8], expected_sha256: &str, token: &str) -> Result<PathBuf, String> {
    if !current.starts_with(root) {
        return Err("The file is no longer inside the approved folder.".into());
    }
    let metadata = fs::symlink_metadata(current)
        .map_err(|_| "FRIDAY could not inspect the file before saving.".to_string())?;
    if is_redirected(&metadata) || !metadata.is_file() {
        return Err("For safety, FRIDAY can edit regular files only, not symbolic links.".into());
    }
    if metadata.len() > MAX_FILE_BYTES {
        return Err("The original file is now larger than the 64 KB editing limit.".into());
    }
    if metadata.permissions().readonly() {
        return Err("This file is read-only. FRIDAY will not change its permissions to edit it.".into());
    }
    let original_bytes = fs::read(current)
        .map_err(|_| "FRIDAY could not re-check the original file before replacement.".to_string())?;
    if content_hash(&original_bytes) != expected_sha256 {
        return Err("The original file changed immediately before saving. No changes were made.".into());
    }
    let original_permissions = metadata.permissions();
    ensure_backup_capacity(root, (original_bytes.len() as u64).saturating_add(proposed.len() as u64).saturating_add(16_384), 8)?;
    let backup_dir = ensure_backup_dir(root, false)?;
    let backup_path = backup_dir.join(format!("{token}.bak"));
    let relative_current = current.strip_prefix(root).map_err(|_| "The file is outside the approved folder.")?
        .to_string_lossy().replace('\\', "/");
    begin_file_transaction(root, &FileTransactionJournal {
        id: token.to_string(), kind: "edit".into(),
        original_relative_path: relative_current.clone(), current_relative_path: relative_current,
        backup_relative_path: format!(".friday-backups/{token}.bak"),
        expected_original_sha256: content_hash(&original_bytes),
        expected_current_sha256: content_hash(proposed), created_at: edit_now(),
    })?;
    let parent = current.parent().ok_or("The selected file has no parent folder.")?;
    let temporary = parent.join(format!(".friday-edit-{token}.tmp"));
    let mut temp_file = OpenOptions::new().write(true).create_new(true).open(&temporary)
        .map_err(|e| format!("Could not prepare the replacement file: {e}"))?;
    if let Err(error) = temp_file.set_permissions(original_permissions).and_then(|_| temp_file.write_all(proposed)).and_then(|_| temp_file.sync_all()) {
        let _ = fs::remove_file(&temporary);
        return Err(format!("Could not write the replacement file: {error}"));
    }
    drop(temp_file);
    if let Err(error) = fs::rename(current, &backup_path) {
        let _ = fs::remove_file(&temporary);
        let _ = mark_file_transaction_phase(root, token, "aborted");
        return Err(format!("Could not preserve the original file as a backup: {error}"));
    }
    mark_file_transaction_phase(root, token, "backup_created")?;
    if let Err(error) = fs::rename(&temporary, current) {
        let restored = fs::rename(&backup_path, current).is_ok();
        let _ = fs::remove_file(&temporary);
        if restored { let _ = mark_file_transaction_phase(root, token, "aborted"); }
        return Err(if restored {
            format!("Could not install the edited file; the original was restored: {error}")
        } else {
            format!("Could not install the edited file or restore it automatically. Your original is preserved at {}: {error}", backup_path.display())
        });
    }
    mark_file_transaction_phase(root, token, "file_installed")?;
    Ok(backup_path)
}


fn validate_granted_root(root: &Path) -> Result<PathBuf, String> {
    let metadata = fs::symlink_metadata(root)
        .map_err(|_| "The selected folder is no longer available.".to_string())?;
    if is_redirected(&metadata) || !metadata.is_dir() {
        return Err("The selected folder is no longer a normal, approved directory.".into());
    }
    let canonical = fs::canonicalize(root)
        .map_err(|_| "Could not verify the selected folder.".to_string())?;
    if canonical.as_path() != root {
        return Err("The selected folder's location changed after it was approved. Select it again.".into());
    }
    Ok(canonical)
}

fn safe_relative_file(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let path = Path::new(relative);
    if relative.is_empty() || relative.len() > 512 || path.is_absolute()
        || path.components().any(|part| !matches!(part, std::path::Component::Normal(_)))
        || !supported_text_file(path) {
        return Err("The undo record contains an unsafe file path.".into());
    }
    let full = root.join(path);
    let parent = full.parent().ok_or("The undo file has no parent folder.")?;
    let parent_meta = fs::symlink_metadata(parent)
        .map_err(|_| "The undo file's folder is no longer available.".to_string())?;
    if is_redirected(&parent_meta) || !parent_meta.is_dir()
        || !fs::canonicalize(parent).map_err(|_| "Could not verify the undo folder.".to_string())?.starts_with(root) {
        return Err("The undo file's folder is not safe or is outside the selected folder.".into());
    }
    Ok(full)
}

fn ensure_backup_dir(root: &Path, create: bool) -> Result<PathBuf, String> {
    let dir = root.join(".friday-backups");
    match fs::symlink_metadata(&dir) {
        Ok(meta) if is_redirected(&meta) || !meta.is_dir() => {
            return Err("The FRIDAY backup location is not a normal folder.".into());
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && create => {
            match fs::create_dir(&dir) {
                Ok(()) => {}
                Err(create_error) if create_error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(create_error) => return Err(format!("Could not create the FRIDAY backup folder: {create_error}")),
            }
        }
        Err(_) => return Err("The FRIDAY backup folder is no longer available.".into()),
    }
    let meta = fs::symlink_metadata(&dir)
        .map_err(|_| "The FRIDAY backup folder is no longer available.".to_string())?;
    if is_redirected(&meta) || !meta.is_dir() {
        return Err("The FRIDAY backup location is not a normal folder.".into());
    }
    let canonical = fs::canonicalize(&dir)
        .map_err(|_| "Could not verify FRIDAY's backup folder.".to_string())?;
    if canonical.as_path() != dir.as_path() || !canonical.starts_with(root) {
        return Err("The FRIDAY backup folder was redirected or is outside the selected folder.".into());
    }
    Ok(dir)
}

fn undo_backup_dir(root: &Path) -> Result<PathBuf, String> {
    ensure_backup_dir(root, false)
}

// Never automatically delete recovery data. Refuse new changes at a bounded
// storage ceiling so users can review/export backups without losing recovery points.
fn ensure_backup_capacity(root: &Path, additional_bytes: u64, additional_files: usize) -> Result<(), String> {
    let dir = ensure_backup_dir(root, true)?;
    let mut bytes = 0u64;
    let mut files = 0usize;
    for entry in fs::read_dir(&dir).map_err(|_| "Could not inspect FRIDAY backup storage.".to_string())? {
        let entry = entry.map_err(|_| "Could not inspect a FRIDAY backup item.".to_string())?;
        let meta = fs::symlink_metadata(entry.path()).map_err(|_| "Could not inspect a FRIDAY backup item.".to_string())?;
        if is_redirected(&meta) || !meta.is_file() {
            return Err("FRIDAY backup storage contains an unexpected item. Review it before continuing.".into());
        }
        files = files.saturating_add(1);
        bytes = bytes.saturating_add(meta.len());
    }
    if files.saturating_add(additional_files) > MAX_BACKUP_STORAGE_FILES || bytes.saturating_add(additional_bytes) > MAX_BACKUP_STORAGE_BYTES {
        return Err("This operation would exceed FRIDAY's backup safety limit (500 files or 100 MB including reserved recovery metadata). Existing backups were preserved; review or export them before making more changes.".into());
    }
    Ok(())
}

fn valid_transaction_id(id: &str) -> bool {
    id.len() == 48 && id.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn transaction_journal_path(root: &Path, id: &str) -> Result<PathBuf, String> {
    if !valid_transaction_id(id) {
        return Err("The transaction identifier is invalid.".into());
    }
    Ok(undo_backup_dir(root)?.join(format!("{id}.txn.json")))
}

fn begin_file_transaction(root: &Path, journal: &FileTransactionJournal) -> Result<(), String> {
    let path = transaction_journal_path(root, &journal.id)?;
    let bytes = serde_json::to_vec(journal).map_err(|_| "Could not prepare the file-operation recovery record.".to_string())?;
    let mut file = OpenOptions::new().write(true).create_new(true).open(&path)
        .map_err(|e| format!("Could not start a durable file-operation recovery record: {e}"))?;
    if let Err(error) = file.write_all(&bytes).and_then(|_| file.sync_all()) {
        drop(file);
        let _ = fs::remove_file(&path);
        return Err(format!("Could not persist the file-operation recovery record: {error}"));
    }
    Ok(())
}

// Each phase is a separate create-new, flushed marker. This avoids relying on
// replacing an existing journal file, which behaves differently on Windows.
fn mark_file_transaction_phase(root: &Path, id: &str, phase: &str) -> Result<(), String> {
    const PHASES: &[&str] = &["backup_created", "destination_created", "source_removed", "file_installed", "history_saved", "complete", "aborted"];
    if !PHASES.contains(&phase) {
        return Err("The file-operation recovery phase is invalid.".into());
    }
    let marker = undo_backup_dir(root)?.join(format!("{id}.txn-{phase}"));
    let mut file = match OpenOptions::new().write(true).create_new(true).open(&marker) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(&marker)
                .map_err(|_| "Could not inspect an existing recovery phase marker.".to_string())?;
            if is_redirected(&metadata) || !metadata.is_file() || metadata.len() > 64 {
                return Err("An existing recovery phase marker is not a safe regular file.".into());
            }
            let contents = fs::read_to_string(&marker)
                .map_err(|_| "An existing recovery phase marker could not be validated.".to_string())?;
            if contents != phase {
                return Err("An existing recovery phase marker is incomplete or inconsistent.".into());
            }
            return Ok(());
        }
        Err(error) => return Err(format!("The file change may have completed, but FRIDAY could not persist its recovery phase: {error}")),
    };
    file.write_all(phase.as_bytes()).and_then(|_| file.sync_all())
        .map_err(|e| format!("FRIDAY could not flush its recovery phase marker: {e}"))
}

fn transaction_phase_exists(root: &Path, id: &str, phase: &str) -> bool {
    let Ok(dir) = undo_backup_dir(root) else { return false; };
    let path = dir.join(format!("{id}.txn-{phase}"));
    match fs::symlink_metadata(&path) {
        Ok(metadata) if !is_redirected(&metadata) && metadata.is_file() && metadata.len() <= 64 => {
            fs::read_to_string(path).map(|contents| contents == phase).unwrap_or(false)
        }
        _ => false,
    }
}

fn bounded_regular_file_hash(path: &Path) -> Option<String> {
    let metadata = fs::symlink_metadata(path).ok()?;
    if is_redirected(&metadata) || !metadata.is_file() || metadata.len() > MAX_FILE_BYTES {
        return None;
    }
    let bytes = fs::read(path).ok()?;
    if bytes.len() as u64 > MAX_FILE_BYTES || bytes.contains(&0) || std::str::from_utf8(&bytes).is_err() {
        return None;
    }
    Some(content_hash(&bytes))
}

fn validate_transaction_journal(journal: &FileTransactionJournal, id: &str) -> Result<(), String> {
    if journal.id != id || id.len() != 48 || !id.bytes().all(|byte| byte.is_ascii_hexdigit())
        || !is_sha256(&journal.expected_original_sha256) || !is_sha256(&journal.expected_current_sha256) {
        return Err("The file-operation recovery record failed integrity validation.".into());
    }
    let expected_backup = match journal.kind.as_str() {
        "edit" => format!(".friday-backups/{id}.bak"),
        "move" => format!(".friday-backups/{id}.move.bak"),
        _ => return Err("The file-operation recovery type is not supported.".into()),
    };
    if journal.backup_relative_path != expected_backup {
        return Err("The file-operation recovery backup path is invalid.".into());
    }
    for relative in [&journal.original_relative_path, &journal.current_relative_path] {
        let path = Path::new(relative);
        if relative.is_empty() || relative.len() > 512 || path.is_absolute()
            || path.components().any(|part| !matches!(part, std::path::Component::Normal(_)))
            || !supported_text_file(path) {
            return Err("The file-operation recovery path is invalid.".into());
        }
    }
    match journal.kind.as_str() {
        "edit" if journal.original_relative_path != journal.current_relative_path => {
            return Err("An edit recovery record must refer to the same original and current path.".into());
        }
        "move" if journal.original_relative_path == journal.current_relative_path
            || journal.expected_original_sha256 != journal.expected_current_sha256 => {
            return Err("A move recovery record must refer to two distinct paths with unchanged file contents.".into());
        }
        _ => {}
    }
    Ok(())
}

fn classify_file_transaction(root: &Path, journal: &FileTransactionJournal) -> String {
    let backup = root.join(&journal.backup_relative_path);
    let backup_hash = bounded_regular_file_hash(&backup);
    let original = match safe_relative_file(root, &journal.original_relative_path) {
        Ok(path) => path, Err(_) => return "manual_review".into(),
    };
    let current = match safe_relative_file(root, &journal.current_relative_path) {
        Ok(path) => path, Err(_) => return "manual_review".into(),
    };
    let original_hash = bounded_regular_file_hash(&original);
    let current_hash = bounded_regular_file_hash(&current);
    let original_missing = matches!(fs::symlink_metadata(&original), Err(error) if error.kind() == std::io::ErrorKind::NotFound);
    let current_missing = matches!(fs::symlink_metadata(&current), Err(error) if error.kind() == std::io::ErrorKind::NotFound);

    match journal.kind.as_str() {
        "edit" if backup_hash.as_deref() == Some(journal.expected_original_sha256.as_str())
            && current_hash.as_deref() == Some(journal.expected_current_sha256.as_str()) => "finish_undo_history".into(),
        "edit" if backup_hash.as_deref() == Some(journal.expected_original_sha256.as_str()) && current_missing => "restore_original".into(),
        // If the original bytes are present at the original path, the edit
        // either never began or its interrupted rollback already restored it.
        // In both cases it is safe to close the journal without changing files.
        "edit" if original_hash.as_deref() == Some(journal.expected_original_sha256.as_str()) => "operation_not_started".into(),
        "move" if backup_hash.as_deref() == Some(journal.expected_original_sha256.as_str())
            && current_hash.as_deref() == Some(journal.expected_current_sha256.as_str()) && original_missing => "finish_undo_history".into(),
        "move" if backup_hash.as_deref() == Some(journal.expected_original_sha256.as_str()) && current_missing && original_missing => "restore_original".into(),
        "move" if original_hash.as_deref() == Some(journal.expected_original_sha256.as_str()) && current_missing => "operation_not_started".into(),
        _ => "manual_review".into(),
    }
}

fn list_file_transactions(root: &Path) -> Result<Vec<FileRecoveryItem>, String> {
    match fs::symlink_metadata(root.join(".friday-backups")) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err("Could not safely inspect FRIDAY's backup folder.".into()),
        Ok(metadata) if is_redirected(&metadata) || !metadata.is_dir() => {
            return Err("FRIDAY's backup folder is not a safe normal directory.".into());
        }
        Ok(_) => {}
    }
    let dir = undo_backup_dir(root)?;
    let mut items = Vec::new();
    for entry in fs::read_dir(&dir).map_err(|_| "Could not inspect file-operation recovery records.".to_string())? {
        let entry = entry.map_err(|_| "Could not inspect a file-operation recovery record.".to_string())?;
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".txn.json") { continue; }
        let id = name.trim_end_matches(".txn.json");
        if transaction_phase_exists(root, id, "complete") || transaction_phase_exists(root, id, "aborted") { continue; }
        let metadata = fs::symlink_metadata(entry.path()).map_err(|_| "Could not inspect a recovery record.".to_string())?;
        if is_redirected(&metadata) || !metadata.is_file() || metadata.len() > 16_384 { continue; }
        let journal: FileTransactionJournal = match fs::read(entry.path()).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()) {
            Some(value) => value,
            None => continue,
        };
        if validate_transaction_journal(&journal, id).is_err() { continue; }
        items.push(FileRecoveryItem {
            id: journal.id.clone(), kind: journal.kind.clone(),
            original_relative_path: journal.original_relative_path.clone(),
            current_relative_path: journal.current_relative_path.clone(),
            status: classify_file_transaction(root, &journal),
            created_at: journal.created_at,
        });
    }
    for entry in fs::read_dir(&dir).map_err(|_| "Could not inspect undo recovery markers.".to_string())? {
        let entry = entry.map_err(|_| "Could not inspect an undo recovery marker.".to_string())?;
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".undoing") { continue; }
        let id = name.trim_end_matches(".undoing");
        if dir.join(format!("{id}.undone")).exists() { continue; }
        let metadata = fs::symlink_metadata(entry.path()).map_err(|_| "Could not inspect an undo recovery marker.".to_string())?;
        if is_redirected(&metadata) || !metadata.is_file() || metadata.len() > 64
            || fs::read_to_string(entry.path()).ok().as_deref() != Some("undoing") { continue; }
        let record = match load_undo_record(root, id) { Ok(value) => value, Err(_) => continue };
        items.push(FileRecoveryItem {
            id: record.id.clone(), kind: format!("undo-{}", record.kind),
            original_relative_path: record.original_relative_path.clone(),
            current_relative_path: record.current_relative_path.clone(),
            status: undo_recovery_status(root, &record), created_at: record.created_at,
        });
    }
    items.sort_by(|a,b| b.created_at.cmp(&a.created_at));
    items.truncate(50);
    Ok(items)
}

fn save_undo_record(root: &Path, record: &UndoRecord) -> Result<(), String> {
    let dir = undo_backup_dir(root)?;
    let path = dir.join(format!("{}.undo.json", record.id));
    let bytes = serde_json::to_vec(record).map_err(|_| "Could not prepare the undo history record.".to_string())?;
    let mut file = OpenOptions::new().write(true).create_new(true).open(&path)
        .map_err(|e| format!("The file operation completed, but FRIDAY could not save its undo history: {e}"))?;
    if let Err(error) = file.write_all(&bytes).and_then(|_| file.sync_all()) {
        drop(file);
        let _ = fs::remove_file(&path);
        return Err(format!("The file operation completed and its original backup remains, but undo history could not be saved: {error}"));
    }
    Ok(())
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn validate_undo_backup(record: &UndoRecord, bytes: &[u8]) -> Result<(), String> {
    if bytes.len() > MAX_FILE_BYTES as usize || bytes.contains(&0) || std::str::from_utf8(bytes).is_err() {
        return Err("The original backup is not valid, bounded UTF-8 text.".into());
    }
    if let Some(expected) = record.backup_sha256.as_deref() {
        if !is_sha256(expected) || content_hash(bytes) != expected {
            return Err("The original backup failed its integrity check. Undo is unavailable.".into());
        }
    }
    Ok(())
}

fn load_undo_record(root: &Path, id: &str) -> Result<UndoRecord, String> {
    if id.len() != 48 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("This undo item is invalid.".into());
    }
    let dir = undo_backup_dir(root)?;
    let path = dir.join(format!("{id}.undo.json"));
    let meta = fs::symlink_metadata(&path).map_err(|_| "This undo item is no longer available.".to_string())?;
    if is_redirected(&meta) || !meta.is_file() || meta.len() > 16_384 {
        return Err("The undo history record is not a safe file.".into());
    }
    let record: UndoRecord = serde_json::from_slice(&fs::read(path)
        .map_err(|_| "Could not read the undo history record.".to_string())?)
        .map_err(|_| "The undo history record is invalid.".to_string())?;
    let expected_backup = match record.kind.as_str() {
        "edit" => format!(".friday-backups/{id}.bak"),
        "move" => format!(".friday-backups/{id}.move.bak"),
        _ => return Err("The undo history record failed validation.".into()),
    };
    if record.id != id || record.backup_relative_path != expected_backup
        || !is_sha256(&record.expected_current_sha256)
        || record.backup_sha256.as_deref().is_some_and(|value| !is_sha256(value)) {
        return Err("The undo history record failed validation.".into());
    }
    Ok(record)
}

fn undo_is_already_applied(root: &Path, record: &UndoRecord) -> bool {
    let backup = root.join(&record.backup_relative_path);
    let backup_meta = match fs::symlink_metadata(&backup) {
        Ok(meta) if meta.is_file() && !is_redirected(&meta) => meta,
        _ => return false,
    };
    if backup_meta.len() > MAX_FILE_BYTES { return false; }
    let backup_bytes = match fs::read(&backup) { Ok(bytes) => bytes, Err(_) => return false };
    if validate_undo_backup(record, &backup_bytes).is_err() { return false; }
    match record.kind.as_str() {
        "edit" => {
            let current = match safe_relative_file(root, &record.current_relative_path) {
                Ok(path) => path, Err(_) => return false,
            };
            let meta = match fs::symlink_metadata(&current) {
                Ok(meta) if meta.is_file() && !is_redirected(&meta) => meta,
                _ => return false,
            };
            if meta.len() > MAX_FILE_BYTES { return false; }
            fs::read(current).map(|bytes| content_hash(&bytes) == content_hash(&backup_bytes)).unwrap_or(false)
        }
        "move" => {
            let original = match safe_relative_file(root, &record.original_relative_path) {
                Ok(path) => path, Err(_) => return false,
            };
            let current = match safe_relative_file(root, &record.current_relative_path) {
                Ok(path) => path, Err(_) => return false,
            };
            let original_meta = match fs::symlink_metadata(&original) {
                Ok(meta) if !is_redirected(&meta) && meta.is_file() && meta.len() <= MAX_FILE_BYTES => meta,
                _ => return false,
            };
            let original_bytes = match fs::read(&original) { Ok(bytes) => bytes, Err(_) => return false };
            if original_bytes.len() as u64 != original_meta.len() { return false; }
            let current_missing = matches!(fs::symlink_metadata(&current), Err(error) if error.kind() == std::io::ErrorKind::NotFound);
            current_missing
                && content_hash(&original_bytes) == content_hash(&backup_bytes)
                && content_hash(&original_bytes) == record.expected_current_sha256
        }
        _ => false,
    }
}

fn list_undo_records(root: &Path) -> Result<Vec<FileUndoOperation>, String> {
    let dir = match undo_backup_dir(root) {
        Ok(value) => value,
        Err(_) => return Ok(Vec::new()),
    };
    let mut items = Vec::new();
    for entry in fs::read_dir(&dir).map_err(|_| "Could not read FRIDAY's undo history.".to_string())? {
        let entry = match entry { Ok(value) => value, Err(_) => continue };
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".undo.json") { continue; }
        let id = name.trim_end_matches(".undo.json");
        let record = match load_undo_record(root, id) { Ok(value) => value, Err(_) => continue };
        if dir.join(format!("{id}.undone")).exists() || undo_is_already_applied(root, &record) { continue; }
        items.push(FileUndoOperation {
            id: record.id,
            kind: record.kind,
            original_relative_path: record.original_relative_path,
            current_relative_path: record.current_relative_path,
            created_at: record.created_at,
        });
    }
    items.sort_by(|a,b| b.created_at.cmp(&a.created_at));
    items.truncate(50);
    Ok(items)
}

fn write_undone_marker(root: &Path, id: &str) -> Result<(), String> {
    let dir = undo_backup_dir(root)?;
    let marker = dir.join(format!("{id}.undone"));
    let file = OpenOptions::new().write(true).create_new(true).open(marker)
        .map_err(|e| format!("The file was restored, but FRIDAY could not update its undo history: {e}"))?;
    file.sync_all().map_err(|e| format!("The file was restored, but its undo-history marker could not be saved: {e}"))
}

fn restore_missing_file_without_overwrite(current: &Path, bytes: &[u8]) -> Result<(), String> {
    match fs::symlink_metadata(current) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Ok(_) => return Err("The original location is occupied. FRIDAY will not overwrite it.".into()),
        Err(_) => return Err("Could not safely inspect the original file location.".into()),
    }
    let parent = current.parent().ok_or("The original file has no parent folder.")?;
    let temp = parent.join(format!(".friday-recovery-{}.tmp", make_token()));
    let mut file = OpenOptions::new().write(true).create_new(true).open(&temp)
        .map_err(|e| format!("Could not prepare the recovered file: {e}"))?;
    if let Err(error) = file.write_all(bytes).and_then(|_| file.sync_all()) {
        drop(file);
        let _ = fs::remove_file(&temp);
        return Err(format!("Could not write the recovered file: {error}"));
    }
    drop(file);
    // Hard-link creation is atomic and refuses to replace a file created after
    // the initial vacancy check. Both paths are in the same parent directory.
    if let Err(error) = fs::hard_link(&temp, current) {
        let _ = fs::remove_file(&temp);
        return Err(format!("The original backup is safe, but FRIDAY could not restore without overwriting: {error}"));
    }
    let _ = fs::remove_file(&temp);
    Ok(())
}

fn write_undo_intent_marker(root: &Path, id: &str) -> Result<(), String> {
    if id.len() != 48 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("The undo transaction identifier is invalid.".into());
    }
    let marker = undo_backup_dir(root)?.join(format!("{id}.undoing"));
    let mut file = match OpenOptions::new().write(true).create_new(true).open(&marker) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(&marker)
                .map_err(|_| "Could not inspect the existing undo recovery marker.".to_string())?;
            if is_redirected(&metadata) || !metadata.is_file() || metadata.len() > 64 {
                return Err("The undo recovery marker is not a safe regular file.".into());
            }
            let contents = fs::read_to_string(&marker)
                .map_err(|_| "The existing undo recovery marker could not be validated.".to_string())?;
            if contents != "undoing" {
                return Err("The existing undo recovery marker is incomplete or inconsistent.".into());
            }
            return Ok(());
        }
        Err(error) => return Err(format!("Could not start the undo recovery record: {error}")),
    };
    file.write_all(b"undoing").and_then(|_| file.sync_all())
        .map_err(|e| format!("Could not flush the undo recovery record: {e}"))
}

fn undo_recovery_status(root: &Path, record: &UndoRecord) -> String {
    if undo_is_already_applied(root, record) { return "finish_undo_history".into(); }
    let backup = root.join(&record.backup_relative_path);
    let backup_hash = bounded_regular_file_hash(&backup);
    let current = match safe_relative_file(root, &record.current_relative_path) {
        Ok(path) => path, Err(_) => return "manual_review".into(),
    };
    let original = match safe_relative_file(root, &record.original_relative_path) {
        Ok(path) => path, Err(_) => return "manual_review".into(),
    };
    let current_hash = bounded_regular_file_hash(&current);
    let original_hash = bounded_regular_file_hash(&original);
    let current_missing = matches!(fs::symlink_metadata(&current), Err(error) if error.kind() == std::io::ErrorKind::NotFound);
    let original_missing = matches!(fs::symlink_metadata(&original), Err(error) if error.kind() == std::io::ErrorKind::NotFound);
    match record.kind.as_str() {
        "edit" if backup_hash.is_some() && current_missing => "restore_original".into(),
        "edit" if backup_hash.is_some() && current_hash.as_deref() == Some(record.expected_current_sha256.as_str()) => "operation_not_started".into(),
        "move" if backup_hash.as_deref() == Some(record.expected_current_sha256.as_str()) && original_hash.as_deref() == Some(record.expected_current_sha256.as_str()) && current_missing => "finish_undo_history".into(),
        "move" if backup_hash.as_deref() == Some(record.expected_current_sha256.as_str()) && current_missing && original_missing => "restore_original".into(),
        "move" if current_hash.as_deref() == Some(record.expected_current_sha256.as_str()) && original_missing => "operation_not_started".into(),
        _ => "manual_review".into(),
    }
}

fn recover_undo_intent(root: &Path, id: &str) -> Result<String, String> {
    let record = load_undo_record(root, id)?;
    let status = undo_recovery_status(root, &record);
    match status.as_str() {
        "finish_undo_history" => {
            if !undo_backup_dir(root)?.join(format!("{id}.undone")).exists() {
                write_undone_marker(root, id)?;
            }
            Ok("Completed recovery of the interrupted Undo operation. The original file is restored.".into())
        }
        "restore_original" => {
            let backup = root.join(&record.backup_relative_path);
            let bytes = fs::read(&backup).map_err(|_| "Could not read the original backup.".to_string())?;
            validate_undo_backup(&record, &bytes)?;
            let target = if record.kind == "move" {
                safe_relative_file(root, &record.original_relative_path)?
            } else {
                safe_relative_file(root, &record.current_relative_path)?
            };
            restore_missing_file_without_overwrite(&target, &bytes)?;
            write_undone_marker(root, id)?;
            Ok("Restored the original file from its verified backup and completed Undo recovery.".into())
        }
        "operation_not_started" => {
            let marker = undo_backup_dir(root)?.join(format!("{id}.undoing"));
            fs::remove_file(marker).map_err(|e| format!("Undo was not applied, but FRIDAY could not clear its recovery marker: {e}"))?;
            Ok("Confirmed that Undo had not changed the file. The Undo option remains available.".into())
        }
        _ => Err("The interrupted Undo state is ambiguous. FRIDAY made no changes; inspect the file and backup manually.".into()),
    }
}

fn make_token() -> String {
    let mut bytes = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn scan_folder(
    current: &Path,
    root: &Path,
    depth: usize,
    scanned: &mut usize,
    candidates: &mut Vec<FileCandidate>,
    grants: &mut BTreeMap<String, GrantedFile>,
) -> Result<(), String> {
    if depth > MAX_FOLDER_DEPTH || candidates.len() >= MAX_FOLDER_CANDIDATES
        || *scanned >= MAX_SCANNED_ENTRIES {
        return Ok(());
    }

    let entries = fs::read_dir(current)
        .map_err(|_| "FRIDAY could not read part of the selected folder.".to_string())?;
    for entry in entries {
        if *scanned >= MAX_SCANNED_ENTRIES || candidates.len() >= MAX_FOLDER_CANDIDATES {
            break;
        }
        *scanned += 1;
        let entry = match entry { Ok(value) => value, Err(_) => continue };
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let metadata = match fs::symlink_metadata(&path) { Ok(value) => value, Err(_) => continue };
        if is_redirected(&metadata) { continue; }

        if metadata.is_dir() {
            if depth < MAX_FOLDER_DEPTH && !ignored_folder(&name) {
                let canonical = match fs::canonicalize(&path) { Ok(value) => value, Err(_) => continue };
                if canonical.starts_with(root) {
                    scan_folder(&canonical, root, depth + 1, scanned, candidates, grants)?;
                }
            }
            continue;
        }
        if !metadata.is_file() || !supported_text_file(&path)
            || metadata.len() == 0 || metadata.len() > MAX_FILE_BYTES {
            continue;
        }
        let canonical = match fs::canonicalize(&path) { Ok(value) => value, Err(_) => continue };
        if !canonical.starts_with(root) { continue; }
        let relative_path = match canonical.strip_prefix(root) {
            Ok(value) => value.components()
                .map(|part| part.as_os_str().to_string_lossy().to_string())
                .collect::<Vec<_>>().join("/"),
            Err(_) => continue,
        };
        let id = make_token();
        candidates.push(FileCandidate {
            id: id.clone(),
            name,
            relative_path,
            size_bytes: metadata.len(),
        });
        grants.insert(id, GrantedFile { root: root.to_path_buf(), path: canonical });
    }
    Ok(())
}

pub fn revoke_session_access() -> Result<(), String> {
    FOLDER_GRANTS.lock().map_err(|_| "File access grants are temporarily unavailable.".to_string())?.clear();
    PENDING_FILE_EDITS.lock().map_err(|_| "File edit previews are temporarily unavailable.".to_string())?.clear();
    PENDING_FILE_MOVES.lock().map_err(|_| "File move previews are temporarily unavailable.".to_string())?.clear();
    PENDING_FILE_UNDOS.lock().map_err(|_| "File undo previews are temporarily unavailable.".to_string())?.clear();
    Ok(())
}

fn ensure_skill_loaded() -> Result<(), String> {
    if crate::skills::is_skill_loaded("file-assistant") {
        Ok(())
    } else {
        Err("Load the File Assistant skill before using its file access.".into())
    }
}

#[tauri::command]
pub async fn pick_folder_and_list_text_files(app: AppHandle) -> Result<FolderScanResult, String> {
    ensure_skill_loaded()?;
    revoke_session_access()?;
    let selected = app.dialog().file().blocking_pick_folder();
    let Some(selected) = selected else {
        return Err("Folder selection was cancelled.".into());
    };
    let path = selected.into_path()
        .map_err(|_| "FRIDAY could not access the selected folder.".to_string())?;
    let selected_meta = fs::symlink_metadata(&path)
        .map_err(|_| "FRIDAY could not inspect the selected folder.".to_string())?;
    if !selected_meta.is_dir() || is_redirected(&selected_meta) {
        return Err("Please select a normal folder, not a symbolic link or redirected folder.".into());
    }
    let root = fs::canonicalize(&path)
        .map_err(|_| "FRIDAY could not open the selected folder.".to_string())?;
    let root_meta = fs::symlink_metadata(&root)
        .map_err(|_| "FRIDAY could not inspect the selected folder.".to_string())?;
    if !root_meta.is_dir() || is_redirected(&root_meta) {
        return Err("Please select a normal folder, not a symbolic link or redirected folder.".into());
    }

    let mut candidates = Vec::new();
    let mut grants = BTreeMap::new();
    let mut scanned = 0usize;
    scan_folder(&root, &root, 0, &mut scanned, &mut candidates, &mut grants)?;
    let folder_id = make_token();
    grants.insert(folder_id.clone(), GrantedFile { root: root.clone(), path: root });
    *FOLDER_GRANTS.lock().map_err(|_| "File Assistant is busy.".to_string())? = grants;
    let _ = crate::tools::record(
        &app,
        "file.list_selected_folder",
        &format!("Listed up to {} supported text files in a folder explicitly selected by the user.", candidates.len()),
        true,
    );
    Ok(FolderScanResult { folder_id, files: candidates })
}

#[tauri::command]
pub async fn read_granted_text_files(
    app: AppHandle,
    file_ids: Vec<String>,
) -> Result<Vec<SelectedTextFile>, String> {
    ensure_skill_loaded()?;
    if file_ids.is_empty() || file_ids.len() > MAX_SELECTED_FILES {
        return Err("Select between 1 and 8 files to read.".into());
    }
    let mut unique = std::collections::BTreeSet::new();
    if file_ids.iter().any(|id| id.len() != 48 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) || !unique.insert(id.clone())) {
        return Err("A file was selected more than once.".into());
    }

    let grants = FOLDER_GRANTS.lock().map_err(|_| "File Assistant is busy.".to_string())?;
    let mut files = Vec::with_capacity(file_ids.len());
    let mut total_bytes = 0usize;
    for id in &file_ids {
        let grant = grants.get(id).ok_or("This folder selection has expired. Select the folder again.")?;
        let root = validate_granted_root(&grant.root)?;
        let path_meta = fs::symlink_metadata(&grant.path)
            .map_err(|_| "A selected file is no longer available.".to_string())?;
        if is_redirected(&path_meta) || !path_meta.is_file() {
            return Err("A selected file is no longer a regular file. Symbolic links are not read.".into());
        }
        let current = fs::canonicalize(&grant.path)
            .map_err(|_| "A selected file is no longer available.".to_string())?;
        if current != grant.path || !current.starts_with(&root) {
            return Err("A selected file is no longer inside the approved folder.".into());
        }
        let mut file = read_selected_text_file(&current, MAX_TOTAL_BYTES.saturating_sub(total_bytes))?;
        file.grant_id = Some(id.clone());
        total_bytes = total_bytes.saturating_add(file.size_bytes);
        files.push(file);
    }
    drop(grants);

    let _ = crate::tools::record(
        &app,
        "file.read_folder_selection",
        &format!("Read {} user-selected files from a previously selected folder ({} bytes).", files.len(), total_bytes),
        true,
    );
    Ok(files)
}


#[tauri::command]
pub fn prepare_file_edit(file_id: String, proposed_content: String) -> Result<FileEditPreview, String> {
    ensure_skill_loaded()?;
    if file_id.len() != 48 || !file_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("This file selection is invalid. Choose the folder and file again.".into());
    }
    validate_edit_content(&proposed_content)?;
    let grants = FOLDER_GRANTS.lock().map_err(|_| "File Assistant is busy.".to_string())?;
    let grant = grants.get(&file_id).ok_or("This file selection has expired. Select the folder again.")?;
    let root = validate_granted_root(&grant.root)?;
    let path_meta = fs::symlink_metadata(&grant.path)
        .map_err(|_| "The selected file is no longer available.".to_string())?;
    if is_redirected(&path_meta) || !path_meta.is_file() {
        return Err("For safety, FRIDAY can edit regular files only, not symbolic links.".into());
    }
    let current = fs::canonicalize(&grant.path)
        .map_err(|_| "The selected file is no longer available.".to_string())?;
    if current != grant.path || !current.starts_with(&root) {
        return Err("The selected file has changed location. Select it again before editing.".into());
    }
    if !supported_text_file(&current) || path_meta.len() > MAX_FILE_BYTES {
        return Err("Only supported text files up to 64 KB can be edited.".into());
    }
    let original_bytes = fs::read(&current)
        .map_err(|_| "FRIDAY could not read the original file for its preview.".to_string())?;
    if original_bytes.len() > MAX_FILE_BYTES as usize {
        return Err("The selected file exceeded the 64 KB limit while being read.".into());
    }
    let original_content = String::from_utf8(original_bytes.clone())
        .map_err(|_| "Only valid UTF-8 text files can be edited.".to_string())?;
    if original_content.contains('\0') {
        return Err("This file appears to contain binary data and cannot be edited.".into());
    }
    let relative_path = current.strip_prefix(&root)
        .map_err(|_| "The selected file is outside the approved folder.".to_string())?
        .to_string_lossy().replace('\\', "/");
    let file_name = current.file_name().and_then(|name| name.to_str()).unwrap_or("Selected file").to_string();
    let preview_id = make_token();
    let mut pending = PENDING_FILE_EDITS.lock().map_err(|_| "File editing is temporarily unavailable.".to_string())?;
    pending.retain(|_, edit| edit.expires_at >= edit_now());
    if pending.len() >= 32 {
        return Err("Too many pending file previews. Close an existing preview and try again.".into());
    }
    pending.insert(preview_id.clone(), PendingFileEdit {
        grant_id: file_id,
        original_sha256: content_hash(&original_bytes),
        proposed_content: proposed_content.clone(),
        expires_at: edit_now() + 300,
        approval_token: None,
    });
    Ok(FileEditPreview { preview_id, file_name, relative_path, original_content, proposed_content })
}

#[tauri::command]
pub fn issue_file_edit_approval(preview_id: String) -> Result<String, String> {
    ensure_skill_loaded()?;
    let mut pending = PENDING_FILE_EDITS.lock().map_err(|_| "File editing is temporarily unavailable.".to_string())?;
    let expired = pending.get(&preview_id)
        .ok_or("This preview has expired. Prepare the edit again.")?
        .expires_at < edit_now();
    if expired {
        pending.remove(&preview_id);
        return Err("This preview has expired. Prepare the edit again.".into());
    }
    let token = make_token();
    let edit = pending.get_mut(&preview_id).ok_or("This preview has expired. Prepare the edit again.")?;
    edit.approval_token = Some(token.clone());
    Ok(token)
}


#[tauri::command]
pub fn cancel_file_edit_preview(preview_id: String) -> Result<(), String> {
    ensure_skill_loaded()?;
    let mut pending = PENDING_FILE_EDITS.lock().map_err(|_| "File editing is temporarily unavailable.".to_string())?;
    pending.remove(&preview_id);
    Ok(())
}

#[tauri::command]
pub fn apply_file_edit(
    app: AppHandle,
    preview_id: String,
    approval_token: String,
) -> Result<FileEditResult, String> {
    ensure_skill_loaded()?;
    let edit = {
        let mut pending = PENDING_FILE_EDITS.lock().map_err(|_| "File editing is temporarily unavailable.".to_string())?;
        pending.remove(&preview_id).ok_or("This preview has expired or was already used. Prepare the edit again.")?
    };
    if !approval_is_valid(edit.expires_at, edit.approval_token.as_deref(), &approval_token, edit_now()) {
        return Err("The edit was not approved with a valid, fresh confirmation. Review it again.".into());
    }
    validate_edit_content(&edit.proposed_content)?;
    let grants = FOLDER_GRANTS.lock().map_err(|_| "File Assistant is busy.".to_string())?;
    let grant = grants.get(&edit.grant_id).ok_or("This folder selection has expired. Select the folder again.")?;
    let root = validate_granted_root(&grant.root)?;
    let metadata = fs::symlink_metadata(&grant.path)
        .map_err(|_| "The selected file is no longer available.".to_string())?;
    if is_redirected(&metadata) || !metadata.is_file() {
        return Err("The selected file is no longer a regular file. No changes were made.".into());
    }
    let current = fs::canonicalize(&grant.path)
        .map_err(|_| "The selected file is no longer available.".to_string())?;
    if current != grant.path || !current.starts_with(&root) {
        return Err("The selected file has changed location. No changes were made.".into());
    }
    let original = fs::read(&current)
        .map_err(|_| "FRIDAY could not re-read the original file. No changes were made.".to_string())?;
    if content_hash(&original) != edit.original_sha256 {
        return Err("This file changed after the preview was prepared. No changes were made; prepare a fresh preview.".into());
    }
    let backup_token = make_token();
    let backup = replace_file_with_backup(&root, &current, edit.proposed_content.as_bytes(), &edit.original_sha256, &backup_token)?;
    let relative_backup = backup.strip_prefix(&root).unwrap_or(&backup).to_string_lossy().replace('\\', "/");
    let relative_current = current.strip_prefix(&root).unwrap_or(&current).to_string_lossy().replace('\\', "/");
    let undo_warning = save_undo_record(&root, &UndoRecord {
        id: backup_token.clone(), kind: "edit".into(),
        original_relative_path: relative_current.clone(), current_relative_path: relative_current,
        backup_relative_path: relative_backup.clone(),
        expected_current_sha256: content_hash(edit.proposed_content.as_bytes()),
        backup_sha256: Some(content_hash(&original)),
        created_at: edit_now(),
    }).err();
    if undo_warning.is_none() {
        mark_file_transaction_phase(&root, &backup_token, "history_saved")?;
        mark_file_transaction_phase(&root, &backup_token, "complete")?;
    }
    let file_name = current.file_name().and_then(|name| name.to_str()).unwrap_or("Selected file").to_string();
    let bytes_written = edit.proposed_content.len();
    let _ = crate::tools::record(
        &app,
        "file.edit_selected_file",
        &format!("Updated {file_name}; original preserved at {relative_backup}."),
        true,
    );
    Ok(FileEditResult {
        file_name, backup_path: relative_backup, bytes_written,
        undo_available: undo_warning.is_none(), warning: undo_warning,
    })
}


fn validate_relative_destination(value: &str) -> Result<PathBuf, String> {
    if value.is_empty() || value.len() > 512 || value.contains('\0') {
        return Err("Enter a valid destination path inside the selected folder.".into());
    }
    let normalized = value.replace('\\', "/");
    let path = Path::new(&normalized);
    if path.is_absolute() || path.components().any(|part| !matches!(part, std::path::Component::Normal(_))) {
        return Err("Destination must be a relative path inside the selected folder; parent and absolute paths are not allowed.".into());
    }
    if !supported_text_file(path) {
        return Err("The destination must keep a supported text-file extension.".into());
    }
    Ok(path.to_path_buf())
}

#[tauri::command]
pub fn prepare_file_move(file_id: String, destination_relative_path: String) -> Result<FileMovePreview, String> {
    ensure_skill_loaded()?;
    if file_id.len() != 48 || !file_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("This file selection is invalid. Choose the folder and file again.".into());
    }
    let destination = validate_relative_destination(&destination_relative_path)?;
    let grants = FOLDER_GRANTS.lock().map_err(|_| "File Assistant is busy.".to_string())?;
    let grant = grants.get(&file_id).ok_or("This file selection has expired. Select the folder again.")?;
    let root = validate_granted_root(&grant.root)?;
    let source_meta = fs::symlink_metadata(&grant.path).map_err(|_| "The selected file is no longer available.".to_string())?;
    if is_redirected(&source_meta) || !source_meta.is_file() || source_meta.len() == 0 || source_meta.len() > MAX_FILE_BYTES {
        return Err("Only regular, non-empty supported text files up to 64 KB can be moved.".into());
    }
    let source = fs::canonicalize(&grant.path).map_err(|_| "The selected file is no longer available.".to_string())?;
    if source != grant.path || !source.starts_with(&root) || !supported_text_file(&source) {
        return Err("The selected file is no longer inside the approved folder or is not supported.".into());
    }
    let destination_path = root.join(&destination);
    let parent = destination_path.parent().ok_or("Choose a destination inside an existing subfolder.")?;
    let parent_meta = fs::symlink_metadata(parent).map_err(|_| "The destination folder must already exist inside the selected folder.".to_string())?;
    if is_redirected(&parent_meta) || !parent_meta.is_dir() {
        return Err("The destination must be a normal existing folder, not a symbolic link.".into());
    }
    let canonical_parent = fs::canonicalize(parent).map_err(|_| "FRIDAY could not verify the destination folder.".to_string())?;
    if !canonical_parent.starts_with(&root) { return Err("The destination is outside the selected folder.".into()); }
    if destination_path == source { return Err("The source and destination are the same.".into()); }
    match fs::symlink_metadata(&destination_path) {
        Ok(_) => return Err("A file or folder already exists at the destination. Choose another name.".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
        Err(_) => return Err("FRIDAY could not safely inspect the destination. No changes were made.".into()),
    }
    let original = fs::read(&source).map_err(|_| "FRIDAY could not verify the selected file.".to_string())?;
    if original.len() as u64 != source_meta.len() || original.contains(&0) {
        return Err("The selected file changed or is not plain text. No changes were made.".into());
    }
    let source_relative_path = source.strip_prefix(&root).map_err(|_| "The selected file is outside the approved folder.")?
        .to_string_lossy().replace('\\', "/");
    let preview_id = make_token();
    let mut pending = PENDING_FILE_MOVES.lock().map_err(|_| "File operations are temporarily unavailable.".to_string())?;
    pending.retain(|_, item| item.expires_at >= edit_now());
    if pending.len() >= 32 { return Err("Too many pending file previews. Close one and try again.".into()); }
    pending.insert(preview_id.clone(), PendingFileMove {
        grant_id: file_id, destination_relative: destination.to_string_lossy().replace('\\', "/"),
        original_sha256: content_hash(&original), expires_at: edit_now() + 300, approval_token: None,
    });
    Ok(FileMovePreview {
        preview_id, source_relative_path, destination_relative_path: destination_path.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/"),
        file_name: source.file_name().and_then(|name| name.to_str()).unwrap_or("Selected file").to_string(),
        size_bytes: source_meta.len(),
    })
}

#[tauri::command]
pub fn issue_file_move_approval(preview_id: String) -> Result<String, String> {
    ensure_skill_loaded()?;
    let mut pending = PENDING_FILE_MOVES.lock().map_err(|_| "File operations are temporarily unavailable.".to_string())?;
    let expired = pending.get(&preview_id).ok_or("This preview has expired. Prepare the move again.")?.expires_at < edit_now();
    if expired { pending.remove(&preview_id); return Err("This preview has expired. Prepare the move again.".into()); }
    let token = make_token();
    pending.get_mut(&preview_id).ok_or("This preview has expired.")?.approval_token = Some(token.clone());
    Ok(token)
}

#[tauri::command]
pub fn cancel_file_move_preview(preview_id: String) -> Result<(), String> {
    ensure_skill_loaded()?;
    PENDING_FILE_MOVES.lock().map_err(|_| "File operations are temporarily unavailable.".to_string())?.remove(&preview_id);
    Ok(())
}

#[tauri::command]
pub fn apply_file_move(app: AppHandle, preview_id: String, approval_token: String) -> Result<FileMoveResult, String> {
    ensure_skill_loaded()?;
    let operation = PENDING_FILE_MOVES.lock().map_err(|_| "File operations are temporarily unavailable.".to_string())?
        .remove(&preview_id).ok_or("This preview has expired or was already used. Prepare it again.")?;
    if !approval_is_valid(operation.expires_at, operation.approval_token.as_deref(), &approval_token, edit_now()) {
        return Err("The move was not approved with a valid, fresh confirmation. Review it again.".into());
    }
    let grants = FOLDER_GRANTS.lock().map_err(|_| "File Assistant is busy.".to_string())?;
    let grant = grants.get(&operation.grant_id).ok_or("This folder selection has expired. Select it again.")?;
    let root = validate_granted_root(&grant.root)?;
    let source_meta = fs::symlink_metadata(&grant.path).map_err(|_| "The selected file is no longer available.".to_string())?;
    if is_redirected(&source_meta) || !source_meta.is_file() || source_meta.len() > MAX_FILE_BYTES {
        return Err("The source is no longer a safe regular text file. No changes were made.".into());
    }
    let source = fs::canonicalize(&grant.path).map_err(|_| "The selected file is no longer available.".to_string())?;
    if source != grant.path || !source.starts_with(&root) { return Err("The source is no longer inside the selected folder. No changes were made.".into()); }
    let bytes = fs::read(&source).map_err(|_| "FRIDAY could not re-check the source file.".to_string())?;
    if content_hash(&bytes) != operation.original_sha256 { return Err("The source changed after the preview. No changes were made; prepare a fresh preview.".into()); }
    let relative = validate_relative_destination(&operation.destination_relative)?;
    let destination = root.join(relative);
    let parent = destination.parent().ok_or("Invalid destination folder.")?;
    let parent_meta = fs::symlink_metadata(parent).map_err(|_| "The destination folder is no longer available.".to_string())?;
    if is_redirected(&parent_meta) || !parent_meta.is_dir() { return Err("The destination folder is no longer safe. No changes were made.".into()); }
    let canonical_parent = fs::canonicalize(parent).map_err(|_| "Could not verify the destination folder.".to_string())?;
    if !canonical_parent.starts_with(&root) {
        return Err("The destination is outside the selected folder. No changes were made.".into());
    }
    match fs::symlink_metadata(&destination) {
        Ok(_) => return Err("The destination is no longer empty. No changes were made.".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
        Err(_) => return Err("FRIDAY could not safely inspect the destination. No changes were made.".into()),
    }
    ensure_backup_capacity(&root, (bytes.len() as u64).saturating_mul(2).saturating_add(16_384), 8)?;
    let backup_dir = ensure_backup_dir(&root, false)?;
    let token = make_token();
    let source_relative = source.strip_prefix(&root).map_err(|_| "The source is outside the approved folder.")?
        .to_string_lossy().replace('\\', "/");
    let destination_relative_for_journal = destination.strip_prefix(&root).map_err(|_| "The destination is outside the approved folder.")?
        .to_string_lossy().replace('\\', "/");
    begin_file_transaction(&root, &FileTransactionJournal {
        id: token.clone(), kind: "move".into(),
        original_relative_path: source_relative.clone(), current_relative_path: destination_relative_for_journal.clone(),
        backup_relative_path: format!(".friday-backups/{token}.move.bak"),
        expected_original_sha256: content_hash(&bytes), expected_current_sha256: content_hash(&bytes),
        created_at: edit_now(),
    })?;
    let backup = backup_dir.join(format!("{token}.move.bak"));
    let mut backup_file = OpenOptions::new().write(true).create_new(true).open(&backup)
        .map_err(|e| format!("Could not create a new backup without overwriting anything: {e}"))?;
    if let Err(error) = backup_file.write_all(&bytes).and_then(|_| backup_file.sync_all()) {
        drop(backup_file);
        let _ = fs::remove_file(&backup);
        return Err(format!("Could not safely write the original backup: {error}"));
    }
    drop(backup_file);
    mark_file_transaction_phase(&root, &token, "backup_created")?;
    let latest_bytes = fs::read(&source).map_err(|_| "FRIDAY could not re-check the source immediately before moving.".to_string())?;
    if content_hash(&latest_bytes) != operation.original_sha256 {
        let _ = fs::remove_file(&backup);
        let _ = mark_file_transaction_phase(&root, &token, "aborted");
        return Err("The source changed immediately before moving. No changes were made.".into());
    }
    // A hard-link creation is atomic and fails rather than replacing an existing destination.
    if let Err(error) = fs::hard_link(&source, &destination) {
        let _ = fs::remove_file(&backup);
        let _ = mark_file_transaction_phase(&root, &token, "aborted");
        return Err(format!("Could not safely create the destination without overwriting anything: {error}"));
    }
    mark_file_transaction_phase(&root, &token, "destination_created")?;
    if let Err(error) = fs::remove_file(&source) {
        let _ = fs::remove_file(&destination);
        let _ = fs::remove_file(&backup);
        let _ = mark_file_transaction_phase(&root, &token, "aborted");
        return Err(format!("Could not finish moving the file. The original remains in place: {error}"));
    }
    mark_file_transaction_phase(&root, &token, "source_removed")?;
    let backup_relative = backup.strip_prefix(&root).unwrap_or(&backup).to_string_lossy().replace('\\', "/");
    let source_relative = source.strip_prefix(&root).unwrap_or(&source).to_string_lossy().replace('\\', "/");
    let destination_relative = destination.strip_prefix(&root).unwrap_or(&destination).to_string_lossy().replace('\\', "/");
    let undo_warning = save_undo_record(&root, &UndoRecord {
        id: token.clone(), kind: "move".into(),
        original_relative_path: source_relative, current_relative_path: destination_relative.clone(),
        backup_relative_path: backup_relative.clone(),
        expected_current_sha256: content_hash(&bytes),
        backup_sha256: Some(content_hash(&bytes)),
        created_at: edit_now(),
    }).err();
    if undo_warning.is_none() {
        mark_file_transaction_phase(&root, &token, "history_saved")?;
        mark_file_transaction_phase(&root, &token, "complete")?;
    }
    let file_name = destination.file_name().and_then(|name| name.to_str()).unwrap_or("Selected file").to_string();
    let _ = crate::tools::record(&app, "file.move_selected_file",
        &format!("Moved a selected file to {destination_relative}; backup preserved at {backup_relative}."), true);
    Ok(FileMoveResult {
        file_name, destination_relative_path: destination_relative, backup_path: backup_relative,
        undo_available: undo_warning.is_none(), warning: undo_warning,
    })
}


#[tauri::command]
pub fn list_file_undo_operations(file_id: String) -> Result<Vec<FileUndoOperation>, String> {
    ensure_skill_loaded()?;
    let grants = FOLDER_GRANTS.lock().map_err(|_| "File Assistant is busy.".to_string())?;
    let grant = grants.get(&file_id).ok_or("Select the folder again to view its undo history.")?;
    let root = validate_granted_root(&grant.root)?;
    list_undo_records(&root)
}

#[tauri::command]
pub fn recover_file_transaction(file_id: String, transaction_id: String) -> Result<String, String> {
    ensure_skill_loaded()?;
    if transaction_id.len() != 48 || !transaction_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("The recovery transaction identifier is invalid.".into());
    }
    let grants = FOLDER_GRANTS.lock().map_err(|_| "File Assistant is busy.".to_string())?;
    let grant = grants.get(&file_id).ok_or("Select the folder again before recovering files.")?;
    let root = validate_granted_root(&grant.root)?;
    if transaction_phase_exists(&root, &transaction_id, "complete")
        || transaction_phase_exists(&root, &transaction_id, "aborted") {
        return Err("This file operation is already closed.".into());
    }
    let undo_intent = undo_backup_dir(&root)?.join(format!("{transaction_id}.undoing"));
    if matches!(fs::symlink_metadata(&undo_intent), Ok(metadata) if !is_redirected(&metadata) && metadata.is_file() && metadata.len() <= 64)
        && fs::read_to_string(&undo_intent).ok().as_deref() == Some("undoing") {
        return recover_undo_intent(&root, &transaction_id);
    }
    let journal_path = transaction_journal_path(&root, &transaction_id)?;
    let metadata = fs::symlink_metadata(&journal_path)
        .map_err(|_| "The file-operation recovery record is no longer available.".to_string())?;
    if is_redirected(&metadata) || !metadata.is_file() || metadata.len() > 16_384 {
        return Err("The file-operation recovery record is not a safe regular file.".into());
    }
    let journal: FileTransactionJournal = serde_json::from_slice(&fs::read(&journal_path)
        .map_err(|_| "Could not read the file-operation recovery record.".to_string())?)
        .map_err(|_| "The file-operation recovery record is invalid.".to_string())?;
    validate_transaction_journal(&journal, &transaction_id)?;
    let status = classify_file_transaction(&root, &journal);
    let backup = root.join(&journal.backup_relative_path);
    let original_bytes = if status == "restore_original" || status == "finish_undo_history" {
        let backup_meta = fs::symlink_metadata(&backup)
            .map_err(|_| "The original backup is missing; FRIDAY made no recovery changes.".to_string())?;
        if is_redirected(&backup_meta) || !backup_meta.is_file() || backup_meta.len() > MAX_FILE_BYTES {
            return Err("The original backup is not a safe regular file; FRIDAY made no recovery changes.".into());
        }
        let bytes = fs::read(&backup).map_err(|_| "Could not read the original backup; no recovery changes were made.".to_string())?;
        if content_hash(&bytes) != journal.expected_original_sha256
            || bytes.contains(&0) || std::str::from_utf8(&bytes).is_err() {
            return Err("The original backup failed its integrity check; FRIDAY made no recovery changes.".into());
        }
        Some(bytes)
    } else { None };

    match status.as_str() {
        "finish_undo_history" => {
            let backup_bytes = original_bytes.as_ref().ok_or("The original backup is unavailable.")?;
            let record_path = undo_backup_dir(&root)?.join(format!("{}.undo.json", journal.id));
            match fs::symlink_metadata(&record_path) {
                Ok(_) => {
                    let existing = load_undo_record(&root, &journal.id)?;
                    let expected_backup_hash = content_hash(backup_bytes);
                    if existing.kind != journal.kind
                        || existing.original_relative_path != journal.original_relative_path
                        || existing.current_relative_path != journal.current_relative_path
                        || existing.backup_relative_path != journal.backup_relative_path
                        || existing.expected_current_sha256 != journal.expected_current_sha256
                        || existing.backup_sha256.as_deref() != Some(expected_backup_hash.as_str()) {
                        return Err("The existing Undo history does not match the recovery journal. FRIDAY made no changes.".into());
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    save_undo_record(&root, &UndoRecord {
                        id: journal.id.clone(), kind: journal.kind.clone(),
                        original_relative_path: journal.original_relative_path.clone(),
                        current_relative_path: journal.current_relative_path.clone(),
                        backup_relative_path: journal.backup_relative_path.clone(),
                        expected_current_sha256: journal.expected_current_sha256.clone(),
                        backup_sha256: Some(content_hash(backup_bytes)), created_at: journal.created_at,
                    })?;
                }
                Err(_) => return Err("Could not safely inspect the undo-history record.".into()),
            }
            mark_file_transaction_phase(&root, &journal.id, "history_saved")?;
            mark_file_transaction_phase(&root, &journal.id, "complete")?;
            Ok("Recovered the completed file operation and restored its Undo history. No user files were overwritten.".into())
        }
        "restore_original" => {
            let bytes = original_bytes.as_ref().ok_or("The original backup is unavailable.")?;
            let target = safe_relative_file(&root, &journal.original_relative_path)?;
            restore_missing_file_without_overwrite(&target, bytes)?;
            mark_file_transaction_phase(&root, &journal.id, "complete")?;
            Ok(format!("Restored the original file at {} from its verified backup. No existing file was overwritten.", journal.original_relative_path))
        }
        "operation_not_started" => {
            mark_file_transaction_phase(&root, &journal.id, "aborted")?;
            Ok("Confirmed that the file operation had not changed the original file. No user files were changed.".into())
        }
        _ => Err("FRIDAY found an ambiguous recovery state. It made no changes; review the files and backup manually.".into()),
    }
}

#[tauri::command]
pub fn list_file_recovery_items(file_id: String) -> Result<Vec<FileRecoveryItem>, String> {
    ensure_skill_loaded()?;
    let grants = FOLDER_GRANTS.lock().map_err(|_| "File Assistant is busy.".to_string())?;
    let grant = grants.get(&file_id).ok_or("Select the folder again to inspect recovery items.")?;
    let root = validate_granted_root(&grant.root)?;
    list_file_transactions(&root)
}

#[tauri::command]
pub fn prepare_file_undo(file_id: String, operation_id: String) -> Result<FileUndoPreview, String> {
    ensure_skill_loaded()?;
    let grants = FOLDER_GRANTS.lock().map_err(|_| "File Assistant is busy.".to_string())?;
    let grant = grants.get(&file_id).ok_or("Select the folder again to undo this operation.")?;
    let root = validate_granted_root(&grant.root)?;
    let record = load_undo_record(&root, &operation_id)?;
    if undo_backup_dir(&root)?.join(format!("{}.undone", record.id)).exists()
        || undo_is_already_applied(&root, &record) {
        return Err("This operation has already been undone.".into());
    }
    let current = safe_relative_file(&root, &record.current_relative_path)?;
    let current_bytes = match fs::symlink_metadata(&current) {
        Ok(meta) => {
            if is_redirected(&meta) || !meta.is_file() || meta.len() > MAX_FILE_BYTES {
                return Err("The current file is no longer a safe regular text file.".into());
            }
            let bytes = fs::read(&current).map_err(|_| "Could not read the current file.".to_string())?;
            if content_hash(&bytes) != record.expected_current_sha256 {
                return Err("This file has changed since FRIDAY's operation. To protect your newer work, undo is unavailable.".into());
            }
            Some(bytes)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => return Err("Could not safely inspect the current file.".into()),
    };
    let backup = root.join(&record.backup_relative_path);
    if !backup.starts_with(undo_backup_dir(&root)?) {
        return Err("The backup path failed safety validation.".into());
    }
    let backup_meta = fs::symlink_metadata(&backup).map_err(|_| "The original backup is missing.".to_string())?;
    if is_redirected(&backup_meta) || !backup_meta.is_file() || backup_meta.len() > MAX_FILE_BYTES {
        return Err("The original backup is not a safe regular text file.".into());
    }
    let backup_bytes = fs::read(&backup).map_err(|_| "Could not read the original backup.".to_string())?;
    validate_undo_backup(&record, &backup_bytes)?;
    let original = safe_relative_file(&root, &record.original_relative_path)?;
    if record.kind == "move" {
        match fs::symlink_metadata(&original) {
            Ok(_) => return Err("The original location is occupied. Move or rename that item first; FRIDAY will not overwrite it.".into()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
            Err(_) => return Err("Could not safely inspect the original location.".into()),
        }
        if content_hash(&backup_bytes) != record.expected_current_sha256 {
            return Err("The move backup does not match the moved file. Undo is unavailable.".into());
        }
    } else if record.kind != "edit" || original != current {
        return Err("The undo record is not a supported file edit.".into());
    }
    let preview_id = make_token();
    let mut pending = PENDING_FILE_UNDOS.lock().map_err(|_| "File undo is temporarily unavailable.".to_string())?;
    pending.retain(|_, item| item.expires_at >= edit_now());
    if pending.len() >= 32 { return Err("Too many pending undo previews. Close one and try again.".into()); }
    pending.insert(preview_id.clone(), PendingFileUndo {
        grant_id: file_id, record: record.clone(), expires_at: edit_now() + 300, approval_token: None,
    });
    Ok(FileUndoPreview { preview_id, kind: record.kind, original_relative_path: record.original_relative_path, current_relative_path: record.current_relative_path })
}

#[tauri::command]
pub fn issue_file_undo_approval(preview_id: String) -> Result<String, String> {
    ensure_skill_loaded()?;
    let mut pending = PENDING_FILE_UNDOS.lock().map_err(|_| "File undo is temporarily unavailable.".to_string())?;
    let expired = pending.get(&preview_id).ok_or("This undo preview has expired.")?.expires_at < edit_now();
    if expired { pending.remove(&preview_id); return Err("This undo preview has expired. Prepare it again.".into()); }
    let token = make_token();
    pending.get_mut(&preview_id).ok_or("This undo preview has expired.")?.approval_token = Some(token.clone());
    Ok(token)
}

#[tauri::command]
pub fn cancel_file_undo_preview(preview_id: String) -> Result<(), String> {
    ensure_skill_loaded()?;
    PENDING_FILE_UNDOS.lock().map_err(|_| "File undo is temporarily unavailable.".to_string())?.remove(&preview_id);
    Ok(())
}

#[tauri::command]
pub fn apply_file_undo(app: AppHandle, preview_id: String, approval_token: String) -> Result<String, String> {
    ensure_skill_loaded()?;
    let pending = PENDING_FILE_UNDOS.lock().map_err(|_| "File undo is temporarily unavailable.".to_string())?
        .remove(&preview_id).ok_or("This undo preview has expired or was already used.")?;
    if !approval_is_valid(pending.expires_at, pending.approval_token.as_deref(), &approval_token, edit_now()) {
        return Err("Undo requires a fresh, explicit approval.".into());
    }
    let grants = FOLDER_GRANTS.lock().map_err(|_| "File Assistant is busy.".to_string())?;
    let grant = grants.get(&pending.grant_id).ok_or("Select the folder again before undoing.")?;
    let root = validate_granted_root(&grant.root)?;
    let record = load_undo_record(&root, &pending.record.id)?;
    if undo_backup_dir(&root)?.join(format!("{}.undone", record.id)).exists()
        || undo_is_already_applied(&root, &record) {
        return Err("This operation has already been undone.".into());
    }
    let current = safe_relative_file(&root, &record.current_relative_path)?;
    let (current_meta, current_bytes) = match fs::symlink_metadata(&current) {
        Ok(meta) => {
            if is_redirected(&meta) || !meta.is_file() || meta.len() > MAX_FILE_BYTES {
                return Err("The current file is no longer safe to restore.".into());
            }
            let bytes = fs::read(&current).map_err(|_| "Could not re-check the current file.".to_string())?;
            if content_hash(&bytes) != record.expected_current_sha256 {
                return Err("The file changed after the undo preview. No changes were made.".into());
            }
            (Some(meta), Some(bytes))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (None, None),
        Err(_) => return Err("Could not safely inspect the current file.".into()),
    };
    let backup = root.join(&record.backup_relative_path);
    let backup_dir = undo_backup_dir(&root)?;
    if !backup.starts_with(&backup_dir) { return Err("The backup path failed safety validation.".into()); }
    let backup_meta = fs::symlink_metadata(&backup).map_err(|_| "The original backup is missing.".to_string())?;
    if is_redirected(&backup_meta) || !backup_meta.is_file() || backup_meta.len() > MAX_FILE_BYTES {
        return Err("The original backup is not a safe regular file.".into());
    }
    let original_bytes = fs::read(&backup).map_err(|_| "Could not read the original backup.".to_string())?;
    validate_undo_backup(&record, &original_bytes)?;
    let original = safe_relative_file(&root, &record.original_relative_path)?;
    if record.kind == "move" {
        match fs::symlink_metadata(&original) {
            Ok(_) => return Err("The original location is occupied. Nothing was overwritten.".into()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
            Err(_) => return Err("Could not safely inspect the original location.".into()),
        }
        if content_hash(&original_bytes) != record.expected_current_sha256 {
            return Err("The original backup failed its integrity check.".into());
        }
        ensure_backup_capacity(&root, 0, 2)?;
        write_undo_intent_marker(&root, &record.id)?;
        fs::hard_link(&backup, &original).map_err(|e| format!("Could not safely restore the original location: {e}"))?;
        if current_bytes.is_some() {
            if let Err(error) = fs::remove_file(&current) {
                let _ = fs::remove_file(&original);
                return Err(format!("Could not finish undoing the move; the moved file remains in place: {error}"));
            }
        }
    } else if record.kind == "edit" && original == current {
        if current_bytes.as_ref().is_some_and(|bytes| content_hash(&original_bytes) == content_hash(bytes)) {
            return Err("The backup is identical to the current file; undo is unnecessary.".into());
        }
        // Reserve space for the durable intent/completion markers and the
        // preserved post-edit version (when one still exists).
        ensure_backup_capacity(&root, current_bytes.as_ref().map(|bytes| bytes.len() as u64).unwrap_or(0),
            if current_bytes.is_some() { 3 } else { 2 })?;
        write_undo_intent_marker(&root, &record.id)?;
        let parent = current.parent().ok_or("The file has no parent folder.")?;
        let temp = parent.join(format!(".friday-undo-{}.tmp", make_token()));
        let mut temp_file = OpenOptions::new().write(true).create_new(true).open(&temp)
            .map_err(|e| format!("Could not prepare the restored file: {e}"))?;
        if let Some(meta) = current_meta.as_ref() {
            if let Err(error) = temp_file.set_permissions(meta.permissions()) {
                drop(temp_file); let _ = fs::remove_file(&temp);
                return Err(format!("Could not preserve the current file permissions: {error}"));
            }
        }
        if let Err(error) = temp_file.write_all(&original_bytes).and_then(|_| temp_file.sync_all()) {
            drop(temp_file); let _ = fs::remove_file(&temp);
            return Err(format!("Could not write the restored file: {error}"));
        }
        drop(temp_file);
        if let Some(current_bytes) = current_bytes.as_ref() {
            let redo = backup_dir.join(format!("{}.redo.bak", record.id));
            let mut redo_file = OpenOptions::new().write(true).create_new(true).open(&redo)
                .map_err(|e| { let _ = fs::remove_file(&temp); format!("Could not preserve the current version: {e}") })?;
            if let Err(error) = redo_file.write_all(current_bytes).and_then(|_| redo_file.sync_all()) {
                drop(redo_file); let _ = fs::remove_file(&redo); let _ = fs::remove_file(&temp);
                return Err(format!("Could not preserve the current version: {error}"));
            }
            drop(redo_file);
            let displaced = parent.join(format!(".friday-undo-current-{}.tmp", make_token()));
            if let Err(error) = fs::rename(&current, &displaced) {
                let _ = fs::remove_file(&temp); let _ = fs::remove_file(&redo);
                return Err(format!("Could not safely stage the current file: {error}"));
            }
            if let Err(error) = fs::rename(&temp, &current) {
                let restored = fs::rename(&displaced, &current).is_ok();
                let _ = fs::remove_file(&temp);
                return Err(if restored { format!("Could not install the original version; the current file was restored: {error}") }
                    else { format!("Could not install the original version. The current version remains at {}: {error}", displaced.display()) });
            }
            let _ = fs::remove_file(&displaced);
        } else {
            let _ = fs::remove_file(&temp);
            restore_missing_file_without_overwrite(&current, &original_bytes)?;
        }
    } else {
        return Err("This undo operation is not supported.".into());
    }
    let history_warning = write_undone_marker(&root, &record.id).err();
    let _ = crate::tools::record(&app, "file.undo_operation",
        &format!("Undid a {} operation for {}.", record.kind, record.original_relative_path), true);
    Ok(match history_warning {
        Some(warning) => format!("Restored {}. The original backup remains, but FRIDAY could not save the undo-history marker: {warning}", record.original_relative_path),
        None => format!("Restored {}.", record.original_relative_path),
    })
}

#[tauri::command]
pub async fn pick_and_read_text_files(app: AppHandle) -> Result<Vec<SelectedTextFile>, String> {
    ensure_skill_loaded()?;
    revoke_session_access()?;
    let selected = app.dialog().file()
        .add_filter("Supported text and code files", &[
            "txt", "md", "csv", "tsv", "json", "jsonl", "yaml", "yml", "toml",
            "ini", "cfg", "log", "rs", "ts", "tsx", "js", "jsx", "py", "html",
            "css", "xml", "sql", "go", "java", "c", "h", "cpp", "hpp", "cs",
            "php", "rb", "swift", "kt", "sh", "ps1",
        ])
        .blocking_pick_files();
    let Some(paths) = selected else {
        return Err("File selection was cancelled.".into());
    };
    if paths.is_empty() || paths.len() > MAX_SELECTED_FILES {
        return Err("Select between 1 and 8 text files at a time.".into());
    }

    let mut files = Vec::with_capacity(paths.len());
    let mut total_bytes = 0usize;
    for selected_path in paths {
        let path = selected_path.into_path()
            .map_err(|_| "FRIDAY could not access one of the selected file paths.".to_string())?;
        let file = read_selected_text_file(&path, MAX_TOTAL_BYTES.saturating_sub(total_bytes))?;
        total_bytes = total_bytes.saturating_add(file.size_bytes);
        files.push(file);
    }

    let _ = crate::tools::record(
        &app,
        "file.read_selected",
        &format!("Read {} explicitly selected text files ({} bytes) into the current session.", files.len(), total_bytes),
        true,
    );
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_file_allowlist_is_case_insensitive_and_excludes_binary_formats() {
        assert!(supported_text_file(Path::new("notes.TXT")));
        assert!(supported_text_file(Path::new("src/main.rs")));
        assert!(supported_text_file(Path::new("data.json")));
        assert!(!supported_text_file(Path::new("photo.png")));
        assert!(!supported_text_file(Path::new("document.pdf")));
        assert!(!supported_text_file(Path::new("archive.zip")));
        assert!(!supported_text_file(Path::new("README")));
    }

    #[test]
    fn folder_scan_stays_within_selected_root_and_skips_common_build_folders() {
        let root = std::env::temp_dir().join(format!("friday-file-assistant-{}", make_token()));
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("node_modules")).unwrap();
        fs::create_dir_all(root.join(".private")).unwrap();
        fs::write(root.join("README.md"), "hello").unwrap();
        fs::write(root.join("src").join("main.rs"), "fn main() {}").unwrap();
        fs::write(root.join("node_modules").join("package.json"), "{}").unwrap();
        fs::write(root.join(".private").join("secret.txt"), "hidden").unwrap();
        fs::write(root.join("image.png"), [0u8, 1, 2]).unwrap();

        let canonical_root = fs::canonicalize(&root).unwrap();
        let mut candidates = Vec::new();
        let mut grants = BTreeMap::new();
        let mut scanned = 0usize;
        scan_folder(&canonical_root, &canonical_root, 0, &mut scanned, &mut candidates, &mut grants).unwrap();

        let paths: Vec<String> = candidates.iter().map(|item| item.relative_path.clone()).collect();
        assert!(paths.contains(&"README.md".to_string()));
        assert!(paths.contains(&"src/main.rs".to_string()));
        assert!(!paths.iter().any(|path| path.contains("node_modules")));
        assert!(!paths.iter().any(|path| path.contains(".private")));
        assert!(!paths.iter().any(|path| path.ends_with(".png")));
        assert_eq!(grants.len(), candidates.len());

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn folder_scan_limits_are_bounded() {
        assert_eq!(MAX_FOLDER_DEPTH, 3);
        assert_eq!(MAX_SCANNED_ENTRIES, 2_000);
        assert_eq!(MAX_FOLDER_CANDIDATES, 200);
        assert!(ignored_folder("node_modules"));
        assert!(ignored_folder(".git"));
        assert!(!ignored_folder("src"));
    }

    #[test]
    fn file_assistant_limits_are_small_and_bounded() {
        assert_eq!(MAX_SELECTED_FILES, 8);
        assert_eq!(MAX_FILE_BYTES, 64 * 1024);
        assert_eq!(MAX_TOTAL_BYTES, 180 * 1024);
    }

    #[test]
    fn recovery_transaction_ids_must_be_fixed_length_hex_tokens() {
        assert!(valid_transaction_id(&"a".repeat(48)));
        assert!(!valid_transaction_id("../outside"));
        assert!(!valid_transaction_id(&"g".repeat(48)));
        assert!(!valid_transaction_id(&"a".repeat(47)));
    }

    #[test]
    fn approval_tokens_reject_expiry_and_mismatches() {
        assert!(approval_is_valid(100, Some("one-use-token"), "one-use-token", 100));
        assert!(!approval_is_valid(99, Some("one-use-token"), "one-use-token", 100));
        assert!(!approval_is_valid(100, Some("one-use-token"), "different-token", 100));
        assert!(!approval_is_valid(100, None, "one-use-token", 100));
        assert!(!approval_is_valid(100, Some("one-use-token"), "", 100));
    }

    #[test]
    fn pending_edit_approval_can_only_be_consumed_once() {
        let id = make_token();
        PENDING_FILE_EDITS.lock().unwrap().insert(id.clone(), PendingFileEdit {
            grant_id: make_token(),
            original_sha256: content_hash(b"before"),
            proposed_content: "after".into(),
            expires_at: edit_now() + 60,
            approval_token: Some(make_token()),
        });
        assert!(PENDING_FILE_EDITS.lock().unwrap().remove(&id).is_some());
        assert!(PENDING_FILE_EDITS.lock().unwrap().remove(&id).is_none());
    }

    #[test]
    fn edit_content_is_bounded_text_without_null_bytes() {
        assert!(validate_edit_content("hello\nworld").is_ok());
        assert!(validate_edit_content(&"x".repeat(MAX_FILE_BYTES as usize)).is_ok());
        assert!(validate_edit_content(&"x".repeat(MAX_FILE_BYTES as usize + 1)).is_err());
        assert!(validate_edit_content("hello\0world").is_err());
    }

    #[test]
    fn replacement_preserves_original_backup_and_updates_selected_file() {
        let root_path = std::env::temp_dir().join(format!("friday-file-edit-{}", make_token()));
        fs::create_dir_all(root_path.join("src")).unwrap();
        let root = fs::canonicalize(&root_path).unwrap();
        let file = root.join("src").join("notes.txt");
        fs::write(&file, "before").unwrap();
        let backup = replace_file_with_backup(&root, &file, b"after", &content_hash(b"before"), &make_token()).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "after");
        assert_eq!(fs::read_to_string(&backup).unwrap(), "before");
        assert!(backup.starts_with(root.join(".friday-backups")));
        fs::remove_dir_all(&root).unwrap();
    }


    #[test]
    fn undo_history_records_are_persisted_and_backup_paths_are_bound_to_ids() {
        let root_path = std::env::temp_dir().join(format!("friday-undo-history-{}", make_token()));
        fs::create_dir_all(root_path.join(".friday-backups")).unwrap();
        let root = fs::canonicalize(&root_path).unwrap();
        let record = UndoRecord {
            id: make_token(), kind: "edit".into(),
            original_relative_path: "notes.txt".into(), current_relative_path: "notes.txt".into(),
            backup_relative_path: String::new(), expected_current_sha256: content_hash(b"after"),
            backup_sha256: Some(content_hash(b"before")), created_at: edit_now(),
        };
        let mut record = record;
        record.backup_relative_path = format!(".friday-backups/{}.bak", record.id);
        fs::write(root.join(&record.backup_relative_path), b"before").unwrap();
        save_undo_record(&root, &record).unwrap();
        let loaded = load_undo_record(&root, &record.id).unwrap();
        assert_eq!(loaded.backup_relative_path, record.backup_relative_path);
        assert_eq!(list_undo_records(&root).unwrap().len(), 1);
        write_undone_marker(&root, &record.id).unwrap();
        assert!(list_undo_records(&root).unwrap().is_empty());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn backup_directory_is_created_when_missing_and_symlink_redirection_is_rejected() {
        let root_path = std::env::temp_dir().join(format!("friday-backup-root-{}", make_token()));
        let outside_path = std::env::temp_dir().join(format!("friday-backup-outside-{}", make_token()));
        fs::create_dir_all(&root_path).unwrap();
        fs::create_dir_all(&outside_path).unwrap();
        let root = fs::canonicalize(&root_path).unwrap();
        let outside = fs::canonicalize(&outside_path).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            symlink(&outside, root.join(".friday-backups")).unwrap();
            assert!(ensure_backup_dir(&root, false).is_err());
        }

        #[cfg(windows)]
        {
            use std::os::windows::fs::symlink_dir;
            let link = root.join(".friday-backups");
            if symlink_dir(&outside, &link).is_ok() {
                assert!(ensure_backup_dir(&root, false).is_err());
            } else {
                assert!(ensure_backup_dir(&root, true).is_ok());
            }
        }

        #[cfg(not(any(unix, windows)))]
        {
            assert!(ensure_backup_dir(&root, true).is_ok());
        }

        fs::remove_dir_all(&root).unwrap();
        fs::remove_dir_all(&outside).unwrap();
    }

    #[test]
    fn backup_storage_capacity_refuses_growth_without_deleting_existing_backups() {
        let root_path = std::env::temp_dir().join(format!("friday-backup-cap-{}", make_token()));
        fs::create_dir_all(root_path.join(".friday-backups")).unwrap();
        let root = fs::canonicalize(&root_path).unwrap();
        for index in 0..MAX_BACKUP_STORAGE_FILES {
            fs::write(root.join(".friday-backups").join(format!("item-{index}.bak")), b"keep").unwrap();
        }
        assert!(ensure_backup_capacity(&root, 1, 1).is_err());
        assert_eq!(fs::read_dir(root.join(".friday-backups")).unwrap().count(), MAX_BACKUP_STORAGE_FILES);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn backup_capacity_reserves_space_for_journal_and_undo_metadata() {
        let root_path = std::env::temp_dir().join(format!("friday-backup-reserve-{}", make_token()));
        fs::create_dir_all(root_path.join(".friday-backups")).unwrap();
        let root = fs::canonicalize(&root_path).unwrap();
        for index in 0..(MAX_BACKUP_STORAGE_FILES - 7) {
            fs::write(root.join(".friday-backups").join(format!("item-{index}.bak")), b"x").unwrap();
        }
        assert!(ensure_backup_capacity(&root, 1, 8).is_err());
        assert_eq!(fs::read_dir(root.join(".friday-backups")).unwrap().count(), MAX_BACKUP_STORAGE_FILES - 7);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn granted_root_must_remain_the_same_real_directory() {
        let root = std::env::temp_dir().join(format!("friday-granted-root-{}", make_token()));
        fs::create_dir_all(&root).unwrap();
        let canonical_root = fs::canonicalize(&root).unwrap();
        assert_eq!(validate_granted_root(&canonical_root).unwrap(), canonical_root);

        let regular_file = root.join("not-a-folder.txt");
        fs::write(&regular_file, "text").unwrap();
        assert!(validate_granted_root(&regular_file).is_err());

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let alias = root.with_file_name(format!("friday-granted-root-link-{}", make_token()));
            symlink(&root, &alias).unwrap();
            assert!(validate_granted_root(&alias).is_err());
            fs::remove_file(alias).unwrap();
        }

        #[cfg(windows)]
        {
            use std::os::windows::fs::symlink_dir;
            let alias = root.with_file_name(format!("friday-granted-root-link-{}", make_token()));
            if symlink_dir(&root, &alias).is_ok() {
                assert!(validate_granted_root(&alias).is_err());
                fs::remove_dir(alias).unwrap();
            }
        }

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn recovery_completes_interrupted_undo_when_the_working_file_is_missing() {
        let root_path = std::env::temp_dir().join(format!("friday-undo-recover-missing-{}", make_token()));
        fs::create_dir_all(root_path.join(".friday-backups")).unwrap();
        let root = fs::canonicalize(&root_path).unwrap();
        let id = make_token();
        let backup_relative = format!(".friday-backups/{id}.bak");
        fs::write(root.join(&backup_relative), b"original contents").unwrap();
        let record = UndoRecord {
            id: id.clone(), kind: "edit".into(), original_relative_path: "notes.txt".into(),
            current_relative_path: "notes.txt".into(), backup_relative_path: backup_relative,
            expected_current_sha256: content_hash(b"edited contents"),
            backup_sha256: Some(content_hash(b"original contents")), created_at: edit_now(),
        };
        save_undo_record(&root, &record).unwrap();
        write_undo_intent_marker(&root, &id).unwrap();
        let result = recover_undo_intent(&root, &id).unwrap();
        assert!(result.contains("Restored the original file"));
        assert_eq!(fs::read(root.join("notes.txt")).unwrap(), b"original contents");
        assert!(root.join(".friday-backups").join(format!("{id}.undone")).is_file());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn recovery_does_not_change_a_file_when_undo_never_started() {
        let root_path = std::env::temp_dir().join(format!("friday-undo-recover-not-started-{}", make_token()));
        fs::create_dir_all(root_path.join(".friday-backups")).unwrap();
        let root = fs::canonicalize(&root_path).unwrap();
        let id = make_token();
        let backup_relative = format!(".friday-backups/{id}.bak");
        fs::write(root.join(&backup_relative), b"original").unwrap();
        fs::write(root.join("notes.txt"), b"edited").unwrap();
        let record = UndoRecord {
            id: id.clone(), kind: "edit".into(), original_relative_path: "notes.txt".into(),
            current_relative_path: "notes.txt".into(), backup_relative_path: backup_relative,
            expected_current_sha256: content_hash(b"edited"),
            backup_sha256: Some(content_hash(b"original")), created_at: edit_now(),
        };
        save_undo_record(&root, &record).unwrap();
        write_undo_intent_marker(&root, &id).unwrap();
        assert!(recover_undo_intent(&root, &id).unwrap().contains("Undo option remains available"));
        assert_eq!(fs::read(root.join("notes.txt")).unwrap(), b"edited");
        assert!(!root.join(".friday-backups").join(format!("{id}.undoing")).exists());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn undo_intent_detects_interrupted_restore_and_preserves_ambiguous_files() {
        let root_path = std::env::temp_dir().join(format!("friday-undo-intent-{}", make_token()));
        fs::create_dir_all(root_path.join(".friday-backups")).unwrap();
        let root = fs::canonicalize(&root_path).unwrap();
        let id = make_token();
        fs::write(root.join(".friday-backups").join(format!("{id}.bak")), b"before").unwrap();
        let record = UndoRecord {
            id: id.clone(), kind: "edit".into(), original_relative_path: "notes.txt".into(),
            current_relative_path: "notes.txt".into(), backup_relative_path: format!(".friday-backups/{id}.bak"),
            expected_current_sha256: content_hash(b"after"), backup_sha256: Some(content_hash(b"before")),
            created_at: edit_now(),
        };
        save_undo_record(&root, &record).unwrap();
        write_undo_intent_marker(&root, &id).unwrap();
        assert_eq!(undo_recovery_status(&root, &record), "restore_original");
        fs::write(root.join("notes.txt"), b"unexpected").unwrap();
        assert_eq!(undo_recovery_status(&root, &record), "manual_review");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn transaction_journal_classifies_interrupted_edit_and_move_states() {
        let root_path = std::env::temp_dir().join(format!("friday-transaction-state-{}", make_token()));
        fs::create_dir_all(root_path.join(".friday-backups")).unwrap();
        let root = fs::canonicalize(&root_path).unwrap();

        let edit_id = make_token();
        let edit = FileTransactionJournal {
            id: edit_id.clone(), kind: "edit".into(),
            original_relative_path: "notes.txt".into(), current_relative_path: "notes.txt".into(),
            backup_relative_path: format!(".friday-backups/{edit_id}.bak"),
            expected_original_sha256: content_hash(b"before"),
            expected_current_sha256: content_hash(b"after"), created_at: edit_now(),
        };
        fs::write(root.join(".friday-backups").join(format!("{edit_id}.bak")), b"before").unwrap();
        assert_eq!(classify_file_transaction(&root, &edit), "restore_original");
        fs::write(root.join("notes.txt"), b"after").unwrap();
        assert_eq!(classify_file_transaction(&root, &edit), "finish_undo_history");

        let move_id = make_token();
        let moved = FileTransactionJournal {
            id: move_id.clone(), kind: "move".into(),
            original_relative_path: "old.txt".into(), current_relative_path: "new.txt".into(),
            backup_relative_path: format!(".friday-backups/{move_id}.move.bak"),
            expected_original_sha256: content_hash(b"moved"),
            expected_current_sha256: content_hash(b"moved"), created_at: edit_now(),
        };
        fs::write(root.join(".friday-backups").join(format!("{move_id}.move.bak")), b"moved").unwrap();
        assert_eq!(classify_file_transaction(&root, &moved), "restore_original");
        fs::write(root.join("new.txt"), b"moved").unwrap();
        assert_eq!(classify_file_transaction(&root, &moved), "finish_undo_history");

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn recovery_scan_is_empty_when_no_backup_folder_exists() {
        let root_path = std::env::temp_dir().join(format!("friday-no-recovery-folder-{}", make_token()));
        fs::create_dir_all(&root_path).unwrap();
        let root = fs::canonicalize(&root_path).unwrap();
        assert!(list_file_transactions(&root).unwrap().is_empty());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn restored_edit_with_backup_still_present_is_classified_as_no_change_needed() {
        let root_path = std::env::temp_dir().join(format!("friday-edit-rollback-recovery-{}", make_token()));
        fs::create_dir_all(root_path.join(".friday-backups")).unwrap();
        let root = fs::canonicalize(&root_path).unwrap();
        let id = make_token();
        let backup_relative = format!(".friday-backups/{id}.bak");
        fs::write(root.join(&backup_relative), b"original").unwrap();
        fs::write(root.join("notes.txt"), b"original").unwrap();
        let journal = FileTransactionJournal {
            id: id.clone(), kind: "edit".into(), original_relative_path: "notes.txt".into(),
            current_relative_path: "notes.txt".into(), backup_relative_path: backup_relative,
            expected_original_sha256: content_hash(b"original"),
            expected_current_sha256: content_hash(b"edited"), created_at: edit_now(),
        };
        assert_eq!(classify_file_transaction(&root, &journal), "operation_not_started");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn transaction_journal_rejects_traversal_and_unexpected_backup_paths() {
        let id = make_token();
        let mut journal = FileTransactionJournal {
            id: id.clone(), kind: "edit".into(),
            original_relative_path: "notes.txt".into(), current_relative_path: "notes.txt".into(),
            backup_relative_path: format!(".friday-backups/{id}.bak"),
            expected_original_sha256: content_hash(b"before"),
            expected_current_sha256: content_hash(b"after"), created_at: edit_now(),
        };
        assert!(validate_transaction_journal(&journal, &id).is_ok());
        journal.current_relative_path = "../outside.txt".into();
        assert!(validate_transaction_journal(&journal, &id).is_err());
        journal.current_relative_path = "notes.txt".into();
        journal.backup_relative_path = ".friday-backups/other.bak".into();
        assert!(validate_transaction_journal(&journal, &id).is_err());
    }

    #[test]
    fn transaction_journal_rejects_inconsistent_edit_and_move_semantics() {
        let id = make_token();
        let mut journal = FileTransactionJournal {
            id: id.clone(), kind: "edit".into(), original_relative_path: "notes.txt".into(),
            current_relative_path: "notes.txt".into(), backup_relative_path: format!(".friday-backups/{id}.bak"),
            expected_original_sha256: content_hash(b"before"),
            expected_current_sha256: content_hash(b"after"), created_at: edit_now(),
        };
        assert!(validate_transaction_journal(&journal, &id).is_ok());
        journal.current_relative_path = "other.txt".into();
        assert!(validate_transaction_journal(&journal, &id).is_err());
        journal.kind = "move".into();
        journal.backup_relative_path = format!(".friday-backups/{id}.move.bak");
        assert!(validate_transaction_journal(&journal, &id).is_err());
        journal.current_relative_path = "notes.txt".into();
        journal.expected_current_sha256 = journal.expected_original_sha256.clone();
        assert!(validate_transaction_journal(&journal, &id).is_err());
        journal.current_relative_path = "other.txt".into();
        assert!(validate_transaction_journal(&journal, &id).is_ok());
    }

    #[test]
    fn interrupted_move_with_both_paths_present_requires_manual_review() {
        let root_path = std::env::temp_dir().join(format!("friday-move-manual-review-{}", make_token()));
        fs::create_dir_all(root_path.join(".friday-backups")).unwrap();
        let root = fs::canonicalize(&root_path).unwrap();
        let id = make_token();
        fs::write(root.join(".friday-backups").join(format!("{id}.move.bak")), b"same bytes").unwrap();
        fs::write(root.join("old.txt"), b"same bytes").unwrap();
        fs::write(root.join("new.txt"), b"same bytes").unwrap();
        let journal = FileTransactionJournal {
            id: id.clone(), kind: "move".into(), original_relative_path: "old.txt".into(),
            current_relative_path: "new.txt".into(),
            backup_relative_path: format!(".friday-backups/{id}.move.bak"),
            expected_original_sha256: content_hash(b"same bytes"),
            expected_current_sha256: content_hash(b"same bytes"), created_at: edit_now(),
        };
        assert!(validate_transaction_journal(&journal, &journal.id).is_ok());
        assert_eq!(classify_file_transaction(&root, &journal), "manual_review");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn transaction_journal_markers_are_create_new_and_durable() {
        let root_path = std::env::temp_dir().join(format!("friday-transaction-markers-{}", make_token()));
        fs::create_dir_all(root_path.join(".friday-backups")).unwrap();
        let root = fs::canonicalize(&root_path).unwrap();
        let id = make_token();
        let journal = FileTransactionJournal {
            id: id.clone(), kind: "edit".into(),
            original_relative_path: "notes.txt".into(), current_relative_path: "notes.txt".into(),
            backup_relative_path: format!(".friday-backups/{id}.bak"),
            expected_original_sha256: content_hash(b"before"),
            expected_current_sha256: content_hash(b"after"), created_at: edit_now(),
        };
        begin_file_transaction(&root, &journal).unwrap();
        mark_file_transaction_phase(&root, &id, "backup_created").unwrap();
        assert!(transaction_phase_exists(&root, &id, "backup_created"));
        mark_file_transaction_phase(&root, &id, "backup_created").unwrap();
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn missing_file_restore_uses_validated_bytes_and_never_overwrites() {
        let root = std::env::temp_dir().join(format!("friday-missing-restore-{}", make_token()));
        fs::create_dir_all(&root).unwrap();
        let target = root.join("notes.txt");

        restore_missing_file_without_overwrite(&target, b"restored original").unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"restored original");
        assert!(restore_missing_file_without_overwrite(&target, b"must not replace").is_err());
        assert_eq!(fs::read(&target).unwrap(), b"restored original");

        let leftovers = fs::read_dir(&root).unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(".friday-recovery-"))
            .count();
        assert_eq!(leftovers, 0);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn undo_backup_integrity_is_checked_for_edits_and_moves() {
        let edit_record = UndoRecord {
            id: make_token(), kind: "edit".into(),
            original_relative_path: "notes.txt".into(), current_relative_path: "notes.txt".into(),
            backup_relative_path: ".friday-backups/test.bak".into(),
            expected_current_sha256: content_hash(b"after"),
            backup_sha256: Some(content_hash(b"before")), created_at: edit_now(),
        };
        assert!(validate_undo_backup(&edit_record, b"before").is_ok());
        assert!(validate_undo_backup(&edit_record, b"tampered").is_err());
        assert!(validate_undo_backup(&edit_record, b"bad\xffutf8").is_err());
    }

    #[test]
    fn undo_completion_is_detected_even_if_history_marker_was_not_written() {
        let root_path = std::env::temp_dir().join(format!("friday-undo-recovery-{}", make_token()));
        fs::create_dir_all(root_path.join(".friday-backups")).unwrap();
        let root = fs::canonicalize(&root_path).unwrap();

        let edit_id = make_token();
        fs::write(root.join("edited.txt"), "before").unwrap();
        fs::write(root.join(".friday-backups").join(format!("{edit_id}.bak")), "before").unwrap();
        let edit_record = UndoRecord {
            id: edit_id.clone(), kind: "edit".into(),
            original_relative_path: "edited.txt".into(), current_relative_path: "edited.txt".into(),
            backup_relative_path: format!(".friday-backups/{edit_id}.bak"),
            expected_current_sha256: content_hash(b"after"),
            backup_sha256: Some(content_hash(b"before")), created_at: edit_now(),
        };
        assert!(undo_is_already_applied(&root, &edit_record));

        let move_id = make_token();
        fs::write(root.join("original.txt"), "moved bytes").unwrap();
        fs::write(root.join(".friday-backups").join(format!("{move_id}.move.bak")), "moved bytes").unwrap();
        let move_record = UndoRecord {
            id: move_id.clone(), kind: "move".into(),
            original_relative_path: "original.txt".into(), current_relative_path: "renamed.txt".into(),
            backup_relative_path: format!(".friday-backups/{move_id}.move.bak"),
            expected_current_sha256: content_hash(b"moved bytes"),
            backup_sha256: Some(content_hash(b"moved bytes")), created_at: edit_now(),
        };
        assert!(undo_is_already_applied(&root, &move_record));

        fs::write(root.join("renamed.txt"), "still here").unwrap();
        assert!(!undo_is_already_applied(&root, &move_record));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn undo_paths_reject_parent_traversal_and_unsupported_files() {
        let root = std::env::temp_dir().join(format!("friday-undo-path-{}", make_token()));
        fs::create_dir_all(&root).unwrap();
        assert!(safe_relative_file(&root, "../outside.txt").is_err());
        assert!(safe_relative_file(&root, "nested/../../outside.txt").is_err());
        assert!(safe_relative_file(&root, "photo.png").is_err());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn move_destination_must_be_safe_relative_supported_path() {
        assert!(validate_relative_destination("notes/renamed.md").is_ok());
        assert!(validate_relative_destination("renamed.TXT").is_ok());
        assert!(validate_relative_destination("../outside.txt").is_err());
        assert!(validate_relative_destination("notes/../../outside.txt").is_err());
        assert!(validate_relative_destination("/outside.txt").is_err());
        assert!(validate_relative_destination("notes/photo.png").is_err());
        assert!(validate_relative_destination("").is_err());
    }

}

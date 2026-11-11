use rand::RngCore;
use serde::Serialize;
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
pub struct FileEditResult {
    pub file_name: String,
    pub backup_path: String,
    pub bytes_written: usize,
}

static FOLDER_GRANTS: Mutex<BTreeMap<String, GrantedFile>> = Mutex::new(BTreeMap::new());
static PENDING_FILE_EDITS: Mutex<BTreeMap<String, PendingFileEdit>> = Mutex::new(BTreeMap::new());

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
    if metadata.file_type().is_symlink() || !metadata.is_file() {
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

fn validate_edit_content(content: &str) -> Result<(), String> {
    if content.len() > MAX_FILE_BYTES as usize {
        return Err("An edited file must remain 64 KB or smaller.".into());
    }
    if content.contains('\\0') {
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
    if metadata.file_type().is_symlink() || !metadata.is_file() {
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
    let backup_dir = root.join(".friday-backups");
    match fs::symlink_metadata(&backup_dir) {
        Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() =>
            return Err("The FRIDAY backup location is not a safe folder.".into()),
        Ok(_) => {},
        Err(_) => fs::create_dir(&backup_dir)
            .map_err(|e| format!("Could not create the backup folder: {e}"))?,
    }
    let backup_path = backup_dir.join(format!("{token}.bak"));
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
        return Err(format!("Could not preserve the original file as a backup: {error}"));
    }
    if let Err(error) = fs::rename(&temporary, current) {
        let restored = fs::rename(&backup_path, current).is_ok();
        let _ = fs::remove_file(&temporary);
        return Err(if restored {
            format!("Could not install the edited file; the original was restored: {error}")
        } else {
            format!("Could not install the edited file or restore it automatically. Your original is preserved at {}: {error}", backup_path.display())
        });
    }
    Ok(backup_path)
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
        if metadata.file_type().is_symlink() { continue; }

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

#[tauri::command]
pub async fn pick_folder_and_list_text_files(app: AppHandle) -> Result<Vec<FileCandidate>, String> {
    let selected = app.dialog().file().blocking_pick_folder();
    let Some(selected) = selected else {
        return Err("Folder selection was cancelled.".into());
    };
    let path = selected.into_path()
        .map_err(|_| "FRIDAY could not access the selected folder.".to_string())?;
    let root = fs::canonicalize(&path)
        .map_err(|_| "FRIDAY could not open the selected folder.".to_string())?;
    let root_meta = fs::symlink_metadata(&root)
        .map_err(|_| "FRIDAY could not inspect the selected folder.".to_string())?;
    if !root_meta.is_dir() || root_meta.file_type().is_symlink() {
        return Err("Please select a normal folder, not a symbolic link.".into());
    }

    let mut candidates = Vec::new();
    let mut grants = BTreeMap::new();
    let mut scanned = 0usize;
    scan_folder(&root, &root, 0, &mut scanned, &mut candidates, &mut grants)?;
    *FOLDER_GRANTS.lock().map_err(|_| "File Assistant is busy.".to_string())? = grants;
    let _ = crate::tools::record(
        &app,
        "file.list_selected_folder",
        &format!("Listed up to {} supported text files in a folder explicitly selected by the user.", candidates.len()),
        true,
    );
    Ok(candidates)
}

#[tauri::command]
pub async fn read_granted_text_files(
    app: AppHandle,
    file_ids: Vec<String>,
) -> Result<Vec<SelectedTextFile>, String> {
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
        let root = fs::canonicalize(&grant.root)
            .map_err(|_| "The selected folder is no longer available.".to_string())?;
        let current = fs::canonicalize(&grant.path)
            .map_err(|_| "A selected file is no longer available.".to_string())?;
        if !current.starts_with(&root) {
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
    if file_id.len() != 48 || !file_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("This file selection is invalid. Choose the folder and file again.".into());
    }
    validate_edit_content(&proposed_content)?;
    let grants = FOLDER_GRANTS.lock().map_err(|_| "File Assistant is busy.".to_string())?;
    let grant = grants.get(&file_id).ok_or("This file selection has expired. Select the folder again.")?;
    let root = fs::canonicalize(&grant.root)
        .map_err(|_| "The selected folder is no longer available.".to_string())?;
    let path_meta = fs::symlink_metadata(&grant.path)
        .map_err(|_| "The selected file is no longer available.".to_string())?;
    if path_meta.file_type().is_symlink() || !path_meta.is_file() {
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
    if original_content.contains('\\0') {
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
    let mut pending = PENDING_FILE_EDITS.lock().map_err(|_| "File editing is temporarily unavailable.".to_string())?;
    let edit = pending.get_mut(&preview_id).ok_or("This preview has expired. Prepare the edit again.")?;
    if edit.expires_at < edit_now() {
        pending.remove(&preview_id);
        return Err("This preview has expired. Prepare the edit again.".into());
    }
    let token = make_token();
    edit.approval_token = Some(token.clone());
    Ok(token)
}


#[tauri::command]
pub fn cancel_file_edit_preview(preview_id: String) -> Result<(), String> {
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
    let edit = {
        let mut pending = PENDING_FILE_EDITS.lock().map_err(|_| "File editing is temporarily unavailable.".to_string())?;
        pending.remove(&preview_id).ok_or("This preview has expired or was already used. Prepare the edit again.")?
    };
    if edit.expires_at < edit_now() || edit.approval_token.as_deref() != Some(approval_token.as_str()) {
        return Err("The edit was not approved with a valid, fresh confirmation. Review it again.".into());
    }
    validate_edit_content(&edit.proposed_content)?;
    let grants = FOLDER_GRANTS.lock().map_err(|_| "File Assistant is busy.".to_string())?;
    let grant = grants.get(&edit.grant_id).ok_or("This folder selection has expired. Select the folder again.")?;
    let root = fs::canonicalize(&grant.root)
        .map_err(|_| "The selected folder is no longer available.".to_string())?;
    let metadata = fs::symlink_metadata(&grant.path)
        .map_err(|_| "The selected file is no longer available.".to_string())?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
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
    let file_name = current.file_name().and_then(|name| name.to_str()).unwrap_or("Selected file").to_string();
    let bytes_written = edit.proposed_content.len();
    let _ = crate::tools::record(
        &app,
        "file.edit_selected_file",
        &format!("Updated {file_name}; original preserved at {relative_backup}."),
        true,
    );
    Ok(FileEditResult { file_name, backup_path: relative_backup, bytes_written })
}

#[tauri::command]
pub async fn pick_and_read_text_files(app: AppHandle) -> Result<Vec<SelectedTextFile>, String> {
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
    fn edit_content_is_bounded_text_without_null_bytes() {
        assert!(validate_edit_content("hello\nworld").is_ok());
        assert!(validate_edit_content(&"x".repeat(MAX_FILE_BYTES as usize)).is_ok());
        assert!(validate_edit_content(&"x".repeat(MAX_FILE_BYTES as usize + 1)).is_err());
        assert!(validate_edit_content("hello\\0world").is_err());
    }

    #[test]
    fn replacement_preserves_original_backup_and_updates_selected_file() {
        let root = std::env::temp_dir().join(format!("friday-file-edit-{}", make_token()));
        fs::create_dir_all(root.join("src")).unwrap();
        let file = root.join("src").join("notes.txt");
        fs::write(&file, "before").unwrap();
        let backup = replace_file_with_backup(&root, &file, b"after", &content_hash(b"before"), &make_token()).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "after");
        assert_eq!(fs::read_to_string(&backup).unwrap(), "before");
        assert!(backup.starts_with(root.join(".friday-backups")));
        fs::remove_dir_all(&root).unwrap();
    }

}

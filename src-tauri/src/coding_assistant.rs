use serde::Serialize;
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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CodingUndoRecord {
    id: String,
    relative_path: String,
    backup_relative_path: String,
    original_sha256: String,
    resulting_sha256: String,
    created_at: u64,
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
    let mut candidates = Vec::new();
    let mut grants = BTreeMap::new();
    let mut scanned = 0usize;
    scan_folder(&root, &root, 0, &mut scanned, &mut candidates, &mut grants)?;
    candidates.sort_by(|a, b| a.relative_path.to_lowercase().cmp(&b.relative_path.to_lowercase()));
    let truncated = scanned >= MAX_SCANNED_ENTRIES || candidates.len() >= MAX_PROJECT_CANDIDATES;
    *PROJECT_GRANTS.lock().map_err(|_| "Coding Assistant project access is temporarily unavailable.".to_string())? = grants;
    let project_name = root.file_name().and_then(|value| value.to_str()).unwrap_or("Selected project").chars().take(160).collect();
    let _ = crate::tools::record(&app, "coding.list_project", &format!("Listed {} bounded source/configuration files from a folder explicitly selected by the user.", candidates.len()), true);
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

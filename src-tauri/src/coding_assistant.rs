use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
};
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

struct ProjectGrant {
    root: PathBuf,
    path: PathBuf,
}

static PROJECT_GRANTS: Mutex<BTreeMap<String, ProjectGrant>> = Mutex::new(BTreeMap::new());

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
    fn relative_paths_are_project_scoped() {
        let root = Path::new("/workspace/project");
        assert_eq!(relative_path(root, Path::new("/workspace/project/src/main.rs")).as_deref(), Some("src/main.rs"));
        assert!(relative_path(root, Path::new("/workspace/other/main.rs")).is_none());
    }
}

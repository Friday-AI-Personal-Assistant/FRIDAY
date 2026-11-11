use serde::Serialize;
use std::{fs, path::Path};
use tauri::AppHandle;
use tauri_plugin_dialog::DialogExt;

const MAX_SELECTED_FILES: usize = 8;
const MAX_FILE_BYTES: u64 = 64 * 1024;
const MAX_TOTAL_BYTES: usize = 180 * 1024;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectedTextFile {
    pub name: String,
    pub content: String,
    pub size_bytes: usize,
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
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("For safety, FRIDAY can read regular files only, not folders or symbolic links.".into());
    }
    if !supported_text_file(path) {
        return Err(format!(
            "The selected file type is not supported for text reading: {}",
            path.extension().and_then(|extension| extension.to_str()).unwrap_or("unknown")
        ));
    }
    if metadata.len() == 0 {
        return Err("One of the selected files is empty.".into());
    }
    if metadata.len() > MAX_FILE_BYTES {
        return Err("Each selected text file must be 64 KB or smaller in this first version.".into());
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
    Ok(SelectedTextFile { name, content, size_bytes })
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
    fn file_assistant_limits_are_small_and_bounded() {
        assert_eq!(MAX_SELECTED_FILES, 8);
        assert_eq!(MAX_FILE_BYTES, 64 * 1024);
        assert_eq!(MAX_TOTAL_BYTES, 180 * 1024);
    }
}

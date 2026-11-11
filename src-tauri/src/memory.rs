use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf, sync::Mutex, time::{SystemTime, UNIX_EPOCH}};
use tauri::{AppHandle, Manager};

use crate::workspace::{decrypt_bytes, encrypt_bytes, encryption_key};

const STORE_FILE: &str = "personal-memory.enc";
const MAX_MEMORIES: usize = 500;
const MAX_CATEGORY_BYTES: usize = 60;
const MAX_CONTENT_BYTES: usize = 2_000;
static MEMORY_LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub id: String,
    pub category: String,
    pub content: String,
    pub enabled: bool,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct MemoryStore {
    entries: Vec<MemoryEntry>,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

fn validate(category: &str, content: &str) -> Result<(), String> {
    let category = category.trim();
    let content = content.trim();
    if category.is_empty() || category.len() > MAX_CATEGORY_BYTES || !category.chars().all(|c| c.is_alphanumeric() || c == ' ' || c == '-' || c == '_') {
        return Err("Choose a short category using letters, numbers, spaces, hyphens or underscores.".into());
    }
    if content.is_empty() || content.len() > MAX_CONTENT_BYTES {
        return Err("Memory must contain between 1 and 2,000 bytes.".into());
    }
    Ok(())
}

fn store_path(app: &AppHandle) -> Result<PathBuf, String> {
    let base = app.path().app_data_dir().map_err(|e| format!("Cannot locate FRIDAY's local data folder: {e}"))?;
    let dir = base.join("workspace");
    fs::create_dir_all(&dir).map_err(|e| format!("Cannot create the local memory folder: {e}"))?;
    Ok(dir.join(STORE_FILE))
}

fn read_store(app: &AppHandle) -> Result<MemoryStore, String> {
    let path = store_path(app)?;
    if !path.exists() { return Ok(MemoryStore::default()); }
    let payload = fs::read(path).map_err(|e| format!("Could not read encrypted personal memory: {e}"))?;
    let key = encryption_key()?;
    let plaintext = decrypt_bytes(&key, &payload)?;
    serde_json::from_slice(&plaintext).map_err(|_| "Personal memory could not be decoded. Its encrypted file may be damaged.".to_string())
}

fn write_store(app: &AppHandle, store: &MemoryStore) -> Result<(), String> {
    if store.entries.len() > MAX_MEMORIES { return Err("FRIDAY can store up to 500 memory entries.".into()); }
    let json = serde_json::to_vec(store).map_err(|e| format!("Could not prepare personal memory: {e}"))?;
    if json.len() > 2_000_000 { return Err("Personal memory store is too large.".into()); }
    let key = encryption_key()?;
    let encrypted = encrypt_bytes(&key, &json)?;
    let path = store_path(app)?;
    let temp = path.with_extension("enc.tmp");
    fs::write(&temp, encrypted).map_err(|e| format!("Could not write encrypted personal memory: {e}"))?;
    if path.exists() { fs::remove_file(&path).map_err(|e| format!("Could not replace personal memory: {e}"))?; }
    fs::rename(&temp, &path).map_err(|e| format!("Could not finish saving personal memory: {e}"))?;
    Ok(())
}

#[tauri::command]
pub fn list_memories(app: AppHandle) -> Result<Vec<MemoryEntry>, String> {
    let _guard = MEMORY_LOCK.lock().map_err(|_| "Personal memory is busy.".to_string())?;
    let mut entries = read_store(&app)?.entries;
    entries.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Ok(entries)
}

#[tauri::command]
pub fn add_memory(app: AppHandle, id: String, category: String, content: String) -> Result<MemoryEntry, String> {
    if !valid_id(&id) { return Err("Invalid memory ID.".into()); }
    validate(&category, &content)?;
    let _guard = MEMORY_LOCK.lock().map_err(|_| "Personal memory is busy.".to_string())?;
    let mut store = read_store(&app)?;
    if store.entries.iter().any(|entry| entry.id == id) { return Err("That memory already exists.".into()); }
    if store.entries.len() >= MAX_MEMORIES { return Err("FRIDAY has reached its 500-memory limit. Delete an old entry first.".into()); }
    let timestamp = now();
    let entry = MemoryEntry { id, category: category.trim().to_string(), content: content.trim().to_string(), enabled: true, created_at: timestamp, updated_at: timestamp };
    store.entries.push(entry.clone());
    write_store(&app, &store)?;
    Ok(entry)
}

#[tauri::command]
pub fn update_memory(app: AppHandle, id: String, category: String, content: String) -> Result<MemoryEntry, String> {
    if !valid_id(&id) { return Err("Invalid memory ID.".into()); }
    validate(&category, &content)?;
    let _guard = MEMORY_LOCK.lock().map_err(|_| "Personal memory is busy.".to_string())?;
    let mut store = read_store(&app)?;
    let entry = store.entries.iter_mut().find(|entry| entry.id == id).ok_or_else(|| "That memory could not be found.".to_string())?;
    entry.category = category.trim().to_string();
    entry.content = content.trim().to_string();
    entry.updated_at = now();
    let updated = entry.clone();
    write_store(&app, &store)?;
    Ok(updated)
}

#[tauri::command]
pub fn set_memory_enabled(app: AppHandle, id: String, enabled: bool) -> Result<(), String> {
    if !valid_id(&id) { return Err("Invalid memory ID.".into()); }
    let _guard = MEMORY_LOCK.lock().map_err(|_| "Personal memory is busy.".to_string())?;
    let mut store = read_store(&app)?;
    let entry = store.entries.iter_mut().find(|entry| entry.id == id).ok_or_else(|| "That memory could not be found.".to_string())?;
    entry.enabled = enabled;
    entry.updated_at = now();
    write_store(&app, &store)
}

#[tauri::command]
pub fn delete_memory(app: AppHandle, id: String) -> Result<(), String> {
    if !valid_id(&id) { return Err("Invalid memory ID.".into()); }
    let _guard = MEMORY_LOCK.lock().map_err(|_| "Personal memory is busy.".to_string())?;
    let mut store = read_store(&app)?;
    let before = store.entries.len();
    store.entries.retain(|entry| entry.id != id);
    if store.entries.len() == before { return Err("That memory could not be found.".into()); }
    write_store(&app, &store)
}

#[cfg(test)]
mod tests {
    use super::{valid_id, validate};

    #[test]
    fn validates_memory_content_and_categories() {
        assert!(validate("Preferences", "Prefers concise answers").is_ok());
        assert!(validate("", "A fact").is_err());
        assert!(validate("Preferences", "").is_err());
        assert!(validate("Preferences", &"x".repeat(2_001)).is_err());
        assert!(validate("Bad/category", "A fact").is_err());
    }

    #[test]
    fn validates_memory_ids() {
        assert!(valid_id("c2a1b7f0-1234"));
        assert!(!valid_id("../unsafe"));
        assert!(!valid_id(""));
    }
}

use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf, sync::Mutex, time::{SystemTime, UNIX_EPOCH}};
use tauri::{AppHandle, Manager};

use crate::workspace::{decrypt_bytes, encrypt_bytes, encryption_key};

const MAX_CONVERSATIONS: usize = 500;
const MAX_MESSAGES: usize = 2_000;
const MAX_MESSAGE_BYTES: usize = 20_000;
const MAX_TOTAL_BYTES: usize = 2_000_000;
const STORE_FILE: &str = "conversations.enc";
static STORE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone, Serialize, Deserialize)]
pub struct ConversationMessage {
    pub role: String,
    pub content: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Conversation {
    pub id: String,
    pub title: String,
    pub created_at: u64,
    pub updated_at: u64,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub archived: bool,
    pub messages: Vec<ConversationMessage>,
}

#[derive(Serialize)]
pub struct ConversationSummary {
    pub id: String,
    pub title: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub message_count: usize,
    pub preview: String,
    pub pinned: bool,
    pub archived: bool,
}

#[derive(Default, Serialize, Deserialize)]
struct ConversationStore {
    conversations: Vec<Conversation>,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

fn store_path(app: &AppHandle) -> Result<PathBuf, String> {
    let base = app.path().app_data_dir()
        .map_err(|e| format!("Cannot locate FRIDAY's local data folder: {e}"))?;
    let dir = base.join("workspace");
    fs::create_dir_all(&dir).map_err(|e| format!("Cannot create the local chat folder: {e}"))?;
    Ok(dir.join(STORE_FILE))
}

fn read_store(app: &AppHandle) -> Result<ConversationStore, String> {
    let path = store_path(app)?;
    if !path.exists() { return Ok(ConversationStore::default()); }
    let payload = fs::read(path).map_err(|e| format!("Could not read encrypted chat history: {e}"))?;
    let key = encryption_key()?;
    let plaintext = decrypt_bytes(&key, &payload)?;
    serde_json::from_slice(&plaintext).map_err(|_| "Chat history could not be decoded. The encrypted file may be damaged.".to_string())
}

fn write_store(app: &AppHandle, store: &ConversationStore) -> Result<(), String> {
    if store.conversations.len() > MAX_CONVERSATIONS {
        return Err("FRIDAY can store up to 500 conversations. Delete an old conversation first.".into());
    }
    let json = serde_json::to_vec(store).map_err(|e| format!("Could not prepare chat history: {e}"))?;
    if json.len() > 50_000_000 { return Err("Chat history is too large to save.".into()); }
    let key = encryption_key()?;
    let encrypted = encrypt_bytes(&key, &json)?;
    let path = store_path(app)?;
    crate::atomic_file::replace(&path, &encrypted)
        .map_err(|e| format!("Could not safely save encrypted chat history: {e}"))?;
    Ok(())
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

#[tauri::command]
pub fn list_conversations(app: AppHandle) -> Result<Vec<ConversationSummary>, String> {
    crate::permissions::authorize_command("conversations.list", "conversations.list")?;
    let _guard = STORE_LOCK.lock().map_err(|_| "Chat history is busy.".to_string())?;
    let store = read_store(&app)?;
    let mut items: Vec<_> = store.conversations.iter().map(|c| conversation_summary(
        c, c.messages.last().map(|message| message_preview(&message.content)).unwrap_or_default()
    )).collect();
    items.sort_by(|a,b| b.pinned.cmp(&a.pinned).then_with(|| b.updated_at.cmp(&a.updated_at)));
    Ok(items)
}

fn conversation_summary(c: &Conversation, preview: String) -> ConversationSummary {
    ConversationSummary {
        id: c.id.clone(), title: c.title.clone(), created_at: c.created_at, updated_at: c.updated_at,
        message_count: c.messages.len(), preview, pinned: c.pinned, archived: c.archived,
    }
}

fn message_preview(content: &str) -> String {
    content.chars().take(180).collect()
}

fn matches_query(conversation: &Conversation, query: &str) -> bool {
    let needle = query.to_lowercase();
    conversation.title.to_lowercase().contains(&needle)
        || conversation.messages.iter().any(|message| message.content.to_lowercase().contains(&needle))
}

#[tauri::command]
pub fn search_conversations(app: AppHandle, query: String, archived: bool) -> Result<Vec<ConversationSummary>, String> {
    crate::permissions::authorize_command("conversations.search", "conversations.search")?;
    let query = query.trim();
    if query.is_empty() || query.len() > 200 || query.chars().any(char::is_control) {
        return Err("Enter a search phrase of 1–200 characters.".into());
    }
    let _guard = STORE_LOCK.lock().map_err(|_| "Chat history is busy.".to_string())?;
    let store = read_store(&app)?;
    let mut items: Vec<_> = store.conversations.iter()
        .filter(|conversation| conversation.archived == archived && matches_query(conversation, query))
        .map(|conversation| {
            let matching_message = conversation.messages.iter().rev()
                .find(|message| message.content.to_lowercase().contains(&query.to_lowercase()));
            let preview = matching_message.map(|message| message_preview(&message.content))
                .or_else(|| conversation.messages.last().map(|message| message_preview(&message.content)))
                .unwrap_or_default();
            conversation_summary(conversation, preview)
        }).collect();
    items.sort_by(|a, b| b.pinned.cmp(&a.pinned).then_with(|| b.updated_at.cmp(&a.updated_at)));
    items.truncate(100);
    Ok(items)
}

#[tauri::command]
pub fn load_conversation(app: AppHandle, id: String) -> Result<Conversation, String> {
    crate::permissions::authorize_command("conversations.read", "conversations.read")?;
    if !valid_id(&id) { return Err("Invalid conversation ID.".into()); }
    let _guard = STORE_LOCK.lock().map_err(|_| "Chat history is busy.".to_string())?;
    read_store(&app)?.conversations.into_iter().find(|c| c.id == id)
        .ok_or_else(|| "That conversation could not be found.".to_string())
}

#[tauri::command]
pub fn save_conversation(app: AppHandle, id: String, title: String, messages: Vec<ConversationMessage>) -> Result<(), String> {
    crate::permissions::authorize_command("conversations.write", "conversations.write")?;
    if !valid_id(&id) || title.trim().is_empty() || title.len() > 160 || messages.len() > MAX_MESSAGES {
        return Err("The conversation details are invalid or exceed the allowed limits.".into());
    }
    let mut total = 0usize;
    for message in &messages {
        if !matches!(message.role.as_str(), "user" | "assistant") || message.content.len() > MAX_MESSAGE_BYTES {
            return Err("A chat message has an unsupported role or is too long.".into());
        }
        total = total.saturating_add(message.content.len());
    }
    if total > MAX_TOTAL_BYTES { return Err("This conversation is too large to save. Start a new chat.".into()); }

    let _guard = STORE_LOCK.lock().map_err(|_| "Chat history is busy.".to_string())?;
    let mut store = read_store(&app)?;
    let timestamp = now();
    if let Some(existing) = store.conversations.iter_mut().find(|c| c.id == id) {
        existing.title = title.trim().to_string();
        existing.messages = messages;
        existing.updated_at = timestamp;
    } else {
        if store.conversations.len() >= MAX_CONVERSATIONS {
            return Err("FRIDAY has reached its 500-conversation limit. Delete an old conversation first.".into());
        }
        store.conversations.push(Conversation { id, title: title.trim().to_string(), created_at: timestamp, updated_at: timestamp, pinned: false, archived: false, messages });
    }
    write_store(&app, &store)
}

#[tauri::command]
pub fn delete_conversation(app: AppHandle, id: String) -> Result<(), String> {
    crate::permissions::authorize_command("conversations.delete", "conversations.delete")?;
    if !valid_id(&id) { return Err("Invalid conversation ID.".into()); }
    let _guard = STORE_LOCK.lock().map_err(|_| "Chat history is busy.".to_string())?;
    let mut store = read_store(&app)?;
    let before = store.conversations.len();
    store.conversations.retain(|c| c.id != id);
    if store.conversations.len() == before { return Err("That conversation could not be found.".into()); }
    write_store(&app, &store)
}

#[tauri::command]
pub fn set_conversation_pinned(app: AppHandle, id: String, pinned: bool) -> Result<(), String> {
    crate::permissions::authorize_command("conversations.pin", "conversations.pin")?;
    if !valid_id(&id) { return Err("Invalid conversation ID.".into()); }
    let _guard = STORE_LOCK.lock().map_err(|_| "Chat history is busy.".to_string())?;
    let mut store = read_store(&app)?;
    let conversation = store.conversations.iter_mut().find(|conversation| conversation.id == id)
        .ok_or_else(|| "That conversation could not be found.".to_string())?;
    conversation.pinned = pinned;
    write_store(&app, &store)
}

#[tauri::command]
pub fn set_conversation_archived(app: AppHandle, id: String, archived: bool) -> Result<(), String> {
    crate::permissions::authorize_command("conversations.archive", "conversations.archive")?;
    if !valid_id(&id) { return Err("Invalid conversation ID.".into()); }
    let _guard = STORE_LOCK.lock().map_err(|_| "Chat history is busy.".to_string())?;
    let mut store = read_store(&app)?;
    let conversation = store.conversations.iter_mut().find(|conversation| conversation.id == id)
        .ok_or_else(|| "That conversation could not be found.".to_string())?;
    conversation.archived = archived;
    write_store(&app, &store)
}

#[cfg(test)]
mod tests {
    use super::{valid_id, Conversation};

    #[test]
    fn legacy_conversations_default_to_unpinned_and_unarchived() {
        let legacy = r#"{"id":"legacy-chat","title":"Old chat","created_at":1,"updated_at":2,"messages":[]}"#;
        let conversation: Conversation = serde_json::from_str(legacy).expect("legacy history remains readable");
        assert!(!conversation.pinned);
        assert!(!conversation.archived);
    }

    #[test]
    fn search_matches_titles_and_older_messages_case_insensitively() {
        let conversation = Conversation {
            id: "chat-1".into(), title: "Project discussion".into(), created_at: 1, updated_at: 2,
            pinned: false, archived: false,
            messages: vec![
                super::ConversationMessage { role: "user".into(), content: "Remember the midnight azure palette".into() },
                super::ConversationMessage { role: "assistant".into(), content: "We can continue tomorrow".into() },
            ],
        };
        assert!(super::matches_query(&conversation, "MIDNIGHT AZURE"));
        assert!(super::matches_query(&conversation, "project discussion"));
        assert!(!super::matches_query(&conversation, "not present"));
    }

    #[test]
    fn accepts_generated_style_ids() {
        assert!(valid_id("a1b2c3d4-e5f6-7890"));
    }

    #[test]
    fn rejects_empty_or_unsafe_ids() {
        assert!(!valid_id(""));
        assert!(!valid_id("../private"));
        assert!(!valid_id("contains spaces"));
        assert!(!valid_id(&"a".repeat(65)));
    }
}

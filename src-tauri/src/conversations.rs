use serde::{Deserialize, Serialize};
use std::{collections::HashSet, fs, path::PathBuf, sync::Mutex, time::{SystemTime, UNIX_EPOCH}};
use tauri::{AppHandle, Manager};

use crate::workspace::{decrypt_bytes, encrypt_bytes, encryption_key};

const MAX_CONVERSATIONS: usize = 500;
const MAX_MESSAGES: usize = 2_000;
const MAX_MESSAGE_BYTES: usize = 20_000;
const MAX_TOTAL_BYTES: usize = 2_000_000;
const MAX_IMPORT_BYTES: usize = 45_000_000;
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

#[derive(Deserialize)]
pub struct ConversationExport {
    pub format: String,
    pub version: u32,
    pub exported_at: String,
    pub conversations: Vec<Conversation>,
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

fn matching_message_preview(content: &str, query: &str) -> String {
    const PREVIEW_CHARS: usize = 180;
    const CONTEXT_BEFORE: usize = 60;

    let normalized_content = content.to_lowercase();
    let normalized_query = query.to_lowercase();
    let Some(match_byte) = normalized_content.find(&normalized_query) else {
        return message_preview(content);
    };

    // Convert the normalized byte offset to a character offset, then slice the
    // original text by characters so multi-byte UTF-8 content is never split.
    let match_start = normalized_content[..match_byte].chars().count();
    let match_length = normalized_query.chars().count();
    let chars: Vec<char> = content.chars().collect();
    let mut start = match_start.saturating_sub(CONTEXT_BEFORE);
    let mut end = (start + PREVIEW_CHARS).min(chars.len());

    if end - start < PREVIEW_CHARS && start > 0 {
        start = end.saturating_sub(PREVIEW_CHARS);
    }
    if match_start + match_length > end {
        end = (match_start + match_length).min(chars.len());
        start = end.saturating_sub(PREVIEW_CHARS);
    }

    let mut preview = String::new();
    if start > 0 { preview.push('…'); }
    preview.extend(chars[start..end].iter());
    if end < chars.len() { preview.push('…'); }
    preview
}

fn matches_query(conversation: &Conversation, query: &str) -> bool {
    // Keep this helper case-insensitive for every caller, including tests and future call sites.
    let needle = query.to_lowercase();
    conversation.title.to_lowercase().contains(&needle)
        || conversation.messages.iter().any(|message| message.content.to_lowercase().contains(&needle))
}

#[tauri::command]
pub fn search_conversations(app: AppHandle, query: String, archived: bool) -> Result<Vec<ConversationSummary>, String> {
    crate::permissions::authorize_command("conversations.search", "conversations.search")?;
    let query = query.trim();
    if query.is_empty() || query.chars().count() > 200 || query.chars().any(char::is_control) {
        return Err("Enter a search phrase of 1–200 characters.".into());
    }
    let needle = query.to_lowercase();
    let _guard = STORE_LOCK.lock().map_err(|_| "Chat history is busy.".to_string())?;
    let store = read_store(&app)?;
    let mut items: Vec<_> = store.conversations.iter()
        .filter(|conversation| conversation.archived == archived && matches_query(conversation, &needle))
        .map(|conversation| {
            let matching_message = conversation.messages.iter().rev()
                .find(|message| message.content.to_lowercase().contains(&needle));
            let preview = matching_message.map(|message| matching_message_preview(&message.content, &needle))
                .or_else(|| conversation.messages.last().map(|message| message_preview(&message.content)))
                .unwrap_or_default();
            conversation_summary(conversation, preview)
        }).collect();
    items.sort_by(|a, b| b.pinned.cmp(&a.pinned).then_with(|| b.updated_at.cmp(&a.updated_at)));
    items.truncate(100);
    Ok(items)
}

#[tauri::command]
pub fn export_all_conversations(app: AppHandle) -> Result<Vec<Conversation>, String> {
    crate::permissions::authorize_command("conversations.export", "conversations.export")?;
    let _guard = STORE_LOCK.lock().map_err(|_| "Chat history is busy.".to_string())?;
    Ok(read_store(&app)?.conversations)
}


fn validate_conversation_export(backup: &ConversationExport) -> Result<(), String> {
    if backup.format != "friday-conversation-export" || backup.version != 1
        || backup.exported_at.is_empty() || backup.exported_at.len() > 64 {
        return Err("Unsupported or invalid FRIDAY conversation export.".into());
    }
    if backup.conversations.is_empty() || backup.conversations.len() > MAX_CONVERSATIONS {
        return Err("A backup must contain between 1 and 500 conversations.".into());
    }
    let mut ids = HashSet::new();
    let mut aggregate_bytes = 0usize;
    let latest_allowed = now().saturating_add(86_400);
    for conversation in &backup.conversations {
        if !valid_id(&conversation.id) || !ids.insert(conversation.id.as_str()) {
            return Err("The backup contains an invalid or duplicate conversation ID.".into());
        }
        if conversation.title.trim().is_empty() || conversation.title.len() > 160
            || conversation.title.chars().any(char::is_control)
            || conversation.created_at == 0
            || conversation.updated_at < conversation.created_at
            || conversation.updated_at > latest_allowed {
            return Err("The backup contains invalid conversation details or timestamps.".into());
        }
        if conversation.messages.len() > MAX_MESSAGES {
            return Err("A conversation in the backup exceeds the 2,000-message limit.".into());
        }
        let mut conversation_bytes = conversation.title.len();
        for message in &conversation.messages {
            if !matches!(message.role.as_str(), "user" | "assistant")
                || message.content.len() > MAX_MESSAGE_BYTES {
                return Err("The backup contains an unsupported message role or an oversized message.".into());
            }
            conversation_bytes = conversation_bytes.saturating_add(message.content.len());
        }
        if conversation_bytes > MAX_TOTAL_BYTES {
            return Err("A conversation in the backup exceeds the 2 MB content limit.".into());
        }
        aggregate_bytes = aggregate_bytes.saturating_add(conversation_bytes);
        if aggregate_bytes > MAX_IMPORT_BYTES {
            return Err("The backup exceeds FRIDAY's 45 MB import-content limit.".into());
        }
    }
    Ok(())
}

#[tauri::command]
pub fn import_conversations(app: AppHandle, backup: ConversationExport) -> Result<usize, String> {
    crate::permissions::authorize_command("conversations.import", "conversations.import")?;
    validate_conversation_export(&backup)?;
    let _guard = STORE_LOCK.lock().map_err(|_| "Chat history is busy.".to_string())?;
    let mut store = read_store(&app)?;
    if store.conversations.len().saturating_add(backup.conversations.len()) > MAX_CONVERSATIONS {
        return Err("Import would exceed FRIDAY's 500-conversation limit. Delete old conversations first.".into());
    }

    let mut used_ids: HashSet<String> = store.conversations.iter().map(|item| item.id.clone()).collect();
    let mut imported = backup.conversations;
    let imported_count = imported.len();
    for conversation in &mut imported {
        if used_ids.contains(&conversation.id) {
            let mut suffix = 0u64;
            loop {
                let replacement = format!("import-{}-{}", now(), suffix);
                suffix = suffix.saturating_add(1);
                if !used_ids.contains(&replacement) {
                    conversation.id = replacement;
                    break;
                }
            }
        }
        used_ids.insert(conversation.id.clone());
    }
    store.conversations.extend(imported);
    write_store(&app, &store)?;
    Ok(imported_count)
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
    use super::{valid_id, validate_conversation_export, Conversation, ConversationExport};


    fn valid_export() -> ConversationExport {
        ConversationExport {
            format: "friday-conversation-export".into(),
            version: 1,
            exported_at: "2026-11-21T00:00:00.000Z".into(),
            conversations: vec![Conversation {
                id: "backup-chat-1".into(), title: "Imported chat".into(),
                created_at: 1, updated_at: 2, pinned: true, archived: true,
                messages: vec![super::ConversationMessage { role: "user".into(), content: "Hello".into() }],
            }],
        }
    }

    #[test]
    fn accepts_valid_version_one_export_and_preserves_archive_metadata() {
        let backup = valid_export();
        assert!(validate_conversation_export(&backup).is_ok());
        assert!(backup.conversations[0].pinned);
        assert!(backup.conversations[0].archived);
    }

    #[test]
    fn rejects_unknown_export_versions_and_duplicate_ids() {
        let mut backup = valid_export();
        backup.version = 2;
        assert!(validate_conversation_export(&backup).is_err());
        let mut backup = valid_export();
        let mut duplicate = backup.conversations[0].clone();
        duplicate.title = "Duplicate".into();
        backup.conversations.push(duplicate);
        assert!(validate_conversation_export(&backup).is_err());
    }

    #[test]
    fn rejects_unsupported_roles_and_oversized_messages() {
        let mut backup = valid_export();
        backup.conversations[0].messages[0].role = "system".into();
        assert!(validate_conversation_export(&backup).is_err());
        let mut backup = valid_export();
        backup.conversations[0].messages[0].content = "x".repeat(super::MAX_MESSAGE_BYTES + 1);
        assert!(validate_conversation_export(&backup).is_err());
    }

    #[test]
    fn rejects_invalid_timestamps_and_empty_exports() {
        let mut backup = valid_export();
        backup.conversations[0].updated_at = 0;
        assert!(validate_conversation_export(&backup).is_err());
        let mut backup = valid_export();
        backup.conversations.clear();
        assert!(validate_conversation_export(&backup).is_err());
    }

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
    fn search_preview_shows_context_around_a_late_match() {
        let content = format!("{}important phrase{}", "opening context ".repeat(20), " closing context".repeat(20));
        let preview = super::matching_message_preview(&content, "IMPORTANT PHRASE");
        assert!(preview.contains("important phrase"));
        assert!(preview.starts_with('…'));
        assert!(preview.ends_with('…'));
        assert!(preview.chars().count() <= 182);
    }

    #[test]
    fn search_preview_handles_unicode_case_insensitively_without_splitting_characters() {
        let content = format!("{}Café résumé{}", "前置内容".repeat(30), "尾部内容".repeat(30));
        let preview = super::matching_message_preview(&content, "CAFÉ RÉSUMÉ");
        assert!(preview.contains("Café résumé"));
        assert!(preview.starts_with('…'));
        assert!(preview.ends_with('…'));
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

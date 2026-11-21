//! Persistent, explicit user grants. Static command policy remains separate.
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Manager};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

const STORE_VERSION: u32 = 1;
const MAX_STORE_BYTES: u64 = 131_072;
const MAX_GRANTS: usize = 256;
const MAX_AUDIT_EVENTS: usize = 512;
const MAX_RESOURCE_BYTES: usize = 512;
const MAX_GRANT_SECONDS: u64 = 31_536_000;
static STORE_LOCK: Mutex<()> = Mutex::new(());

const GRANTABLE_CAPABILITIES: &[&str] = &[
    "files.read.selected",
    "files.write.approved",
    "web.research",
    "desktop.launch",
    "ai.remote.inference",
    "memory.read",
    "memory.write",
];

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PermissionGrant {
    pub id: String,
    pub capability: String,
    pub resource: String,
    pub granted_at: u64,
    pub expires_at: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PermissionAuditEvent {
    pub grant_id: String,
    pub action: String,
    pub capability: String,
    pub resource: String,
    pub occurred_at: u64,
    pub expires_at: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GrantStore {
    version: u32,
    grants: Vec<PermissionGrant>,
    audit: Vec<PermissionAuditEvent>,
}

impl Default for GrantStore {
    fn default() -> Self {
        Self { version: STORE_VERSION, grants: Vec::new(), audit: Vec::new() }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionSnapshot {
    pub grants: Vec<PermissionGrant>,
    pub audit: Vec<PermissionAuditEvent>,
}

fn now_unix() -> Result<u64, String> {
    SystemTime::now().duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs()).map_err(|_| "System clock is before the Unix epoch.".into())
}

fn store_path(app: &AppHandle) -> Result<PathBuf, String> {
    let base = app.path().app_data_dir()
        .map_err(|e| format!("Cannot locate FRIDAY's application data folder: {e}"))?;
    Ok(base.join("permissions").join("grants.json"))
}

fn valid_resource(resource: &str) -> bool {
    !resource.is_empty()
        && resource.len() <= MAX_RESOURCE_BYTES
        && resource.trim() == resource
        && !resource.chars().any(char::is_control)
}

fn valid_grant_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

fn validate_store(store: &GrantStore) -> Result<(), String> {
    if store.version != STORE_VERSION || store.grants.len() > MAX_GRANTS
        || store.audit.len() > MAX_AUDIT_EVENTS {
        return Err("Permission store version or size is unsupported; access is denied.".into());
    }
    let mut ids = std::collections::BTreeSet::new();
    for grant in &store.grants {
        if !valid_grant_id(&grant.id) || !ids.insert(grant.id.as_str())
            || !GRANTABLE_CAPABILITIES.contains(&grant.capability.as_str())
            || !valid_resource(&grant.resource)
            || grant.expires_at.is_some_and(|expiry| expiry <= grant.granted_at
                || expiry.saturating_sub(grant.granted_at) > MAX_GRANT_SECONDS) {
            return Err("Permission store contains an invalid grant; access is denied.".into());
        }
    }
    for event in &store.audit {
        if !valid_grant_id(&event.grant_id)
            || !GRANTABLE_CAPABILITIES.contains(&event.capability.as_str())
            || !valid_resource(&event.resource)
            || !matches!(event.action.as_str(), "granted" | "revoked" | "expired") {
            return Err("Permission audit history is invalid; access is denied.".into());
        }
    }
    Ok(())
}

fn read_store(path: &Path) -> Result<GrantStore, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(GrantStore::default()),
        Err(error) => return Err(format!("Could not inspect permission store: {error}")),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > MAX_STORE_BYTES {
        return Err("Permission store is not a safe regular file or exceeds its size limit; access is denied.".into());
    }
    let bytes = fs::read(path).map_err(|e| format!("Could not read permission store: {e}"))?;
    let store: GrantStore = serde_json::from_slice(&bytes)
        .map_err(|e| format!("Permission store is invalid; access is denied: {e}"))?;
    validate_store(&store)?;
    Ok(store)
}

fn write_store(path: &Path, store: &GrantStore) -> Result<(), String> {
    validate_store(store)?;
    let parent = path.parent().ok_or("Invalid permission store path.")?;
    fs::create_dir_all(parent).map_err(|e| format!("Could not create permission storage: {e}"))?;
    let bytes = serde_json::to_vec_pretty(store).map_err(|e| format!("Could not encode permissions: {e}"))?;
    if bytes.len() as u64 > MAX_STORE_BYTES {
        return Err("Permission history reached its safe storage limit.".into());
    }
    crate::atomic_file::replace(path, &bytes)
        .map_err(|e| format!("Could not safely save permissions: {e}"))
}

fn append_event(store: &mut GrantStore, event: PermissionAuditEvent) {
    store.audit.push(event);
    if store.audit.len() > MAX_AUDIT_EVENTS {
        let excess = store.audit.len() - MAX_AUDIT_EVENTS;
        store.audit.drain(0..excess);
    }
}

fn new_grant_id() -> Result<String, String> {
    let mut bytes = [0_u8; 16];
    OsRng.try_fill_bytes(&mut bytes)
        .map_err(|_| "Secure random generator is unavailable; no permission was granted.".to_string())?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn grant_matches(grant: &PermissionGrant, capability: &str, resource: &str, now: u64) -> bool {
    grant.capability == capability && grant.resource == resource
        && grant.expires_at.is_none_or(|expiry| now < expiry)
}

pub(crate) fn has_grant(path: &Path, capability: &str, resource: &str) -> Result<bool, String> {
    if !GRANTABLE_CAPABILITIES.contains(&capability) || !valid_resource(resource) {
        return Ok(false);
    }
    let _guard = STORE_LOCK.lock().map_err(|_| "Permission store is busy.".to_string())?;
    let store = read_store(path)?;
    let now = now_unix()?;
    Ok(store.grants.iter().any(|g| grant_matches(g, capability, resource, now)))
}

#[tauri::command]
pub fn list_permission_grants(app: AppHandle) -> Result<PermissionSnapshot, String> {
    crate::permissions::authorize_command("permissions.list", "permissions.list")?;
    let _guard = STORE_LOCK.lock().map_err(|_| "Permission store is busy.".to_string())?;
    let store = read_store(&store_path(&app)?)?;
    Ok(PermissionSnapshot { grants: store.grants, audit: store.audit })
}

#[tauri::command]
pub async fn grant_permission(
    app: AppHandle,
    capability: String,
    resource: String,
    expires_in_seconds: Option<u64>,
) -> Result<PermissionGrant, String> {
    crate::permissions::authorize_command("permissions.grant", "permissions.grant")?;
    if !GRANTABLE_CAPABILITIES.contains(&capability.as_str()) {
        return Err("This permission is not in FRIDAY's approved grant catalogue.".into());
    }
    if !valid_resource(&resource) {
        return Err("Permission resource must be a clear, non-empty identifier of at most 512 bytes.".into());
    }
    if expires_in_seconds.is_some_and(|seconds| seconds == 0 || seconds > MAX_GRANT_SECONDS) {
        return Err("Permission expiry must be between one second and one year.".into());
    }
    let now = now_unix()?;
    let expires_at = expires_in_seconds.map(|seconds| now.saturating_add(seconds));
    let expiry_text = expires_at.map(|t| format!("Unix time {t}")).unwrap_or_else(|| "no automatic expiry".into());
    let message = format!(
        "FRIDAY is requesting permission:\n\nCapability: {capability}\nResource: {resource}\nExpiry: {expiry_text}\n\nGranting this permission does not bypass separate per-action approvals or safety checks. Do you explicitly approve this grant?"
    );
    let approved = app.dialog().message(message)
        .title("FRIDAY — Permission request")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom("Grant permission".into(), "Cancel".into()))
        .blocking_show();
    if !approved {
        return Err("Permission request was cancelled; nothing was granted.".into());
    }

    let _guard = STORE_LOCK.lock().map_err(|_| "Permission store is busy.".to_string())?;
    let path = store_path(&app)?;
    let mut store = read_store(&path)?;
    let now = now_unix()?;
    if store.grants.iter().any(|g| grant_matches(g, &capability, &resource, now)) {
        return Err("An active grant already exists for this exact capability and resource.".into());
    }
    let grant = PermissionGrant {
        id: new_grant_id()?,
        capability: capability.clone(),
        resource: resource.clone(),
        granted_at: now,
        expires_at,
    };
    store.grants.push(grant.clone());
    append_event(&mut store, PermissionAuditEvent {
        grant_id: grant.id.clone(), action: "granted".into(),
        capability, resource, occurred_at: now, expires_at,
    });
    write_store(&path, &store)?;
    Ok(grant)
}

#[tauri::command]
pub fn revoke_permission(app: AppHandle, grant_id: String) -> Result<String, String> {
    crate::permissions::authorize_command("permissions.revoke", "permissions.revoke")?;
    if !valid_grant_id(&grant_id) {
        return Err("Invalid permission grant identifier.".into());
    }
    let _guard = STORE_LOCK.lock().map_err(|_| "Permission store is busy.".to_string())?;
    let path = store_path(&app)?;
    let mut store = read_store(&path)?;
    let index = store.grants.iter().position(|g| g.id == grant_id)
        .ok_or("Permission grant was not found or was already revoked.")?;
    let grant = store.grants.remove(index);
    let now = now_unix()?;
    append_event(&mut store, PermissionAuditEvent {
        grant_id, action: "revoked".into(), capability: grant.capability,
        resource: grant.resource, occurred_at: now, expires_at: grant.expires_at,
    });
    write_store(&path, &store)?;
    Ok("Permission revoked. Future checks will deny this grant.".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, sync::atomic::{AtomicU64, Ordering}};
    static NEXT_TEST: AtomicU64 = AtomicU64::new(1);

    fn test_path() -> PathBuf {
        let n = NEXT_TEST.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("friday-permissions-{}-{n}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir.join("grants.json")
    }

    #[test]
    fn grant_requires_exact_capability_and_resource_and_expiry() {
        let path = test_path();
        let mut store = GrantStore::default();
        store.grants.push(PermissionGrant {
            id: "0123456789abcdef0123456789abcdef".into(),
            capability: "files.read.selected".into(),
            resource: "folder:projects/demo".into(),
            granted_at: 100,
            expires_at: Some(200),
        });
        write_store(&path, &store).unwrap();
        assert!(grant_matches(&read_store(&path).unwrap().grants[0], "files.read.selected", "folder:projects/demo", 199));
        assert!(!grant_matches(&store.grants[0], "files.read.selected", "folder:projects/other", 199));
        assert!(!grant_matches(&store.grants[0], "files.write.approved", "folder:projects/demo", 199));
        assert!(!grant_matches(&store.grants[0], "files.read.selected", "folder:projects/demo", 200));
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn corrupt_or_unsupported_store_fails_closed() {
        let path = test_path();
        fs::write(&path, b"{broken").unwrap();
        assert!(read_store(&path).is_err());
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn duplicate_ids_and_unknown_capabilities_are_rejected() {
        let id = "0123456789abcdef0123456789abcdef";
        let grant = PermissionGrant { id: id.into(), capability: "files.read.selected".into(),
            resource: "folder:demo".into(), granted_at: 10, expires_at: None };
        let mut store = GrantStore::default();
        store.grants = vec![grant.clone(), grant];
        assert!(validate_store(&store).is_err());
        let mut unknown = GrantStore::default();
        unknown.grants.push(PermissionGrant { id: id.into(), capability: "arbitrary.shell".into(),
            resource: "all".into(), granted_at: 10, expires_at: None });
        assert!(validate_store(&unknown).is_err());
    }

    #[test]
    fn invalid_resource_and_overlong_expiry_are_rejected() {
        assert!(!valid_resource(""));
        assert!(!valid_resource("  resource"));
        assert!(!valid_resource("resource\nforged"));
        let mut store = GrantStore::default();
        store.grants.push(PermissionGrant { id: "0123456789abcdef0123456789abcdef".into(),
            capability: "files.read.selected".into(), resource: "folder:demo".into(),
            granted_at: 1, expires_at: Some(1 + MAX_GRANT_SECONDS + 1) });
        assert!(validate_store(&store).is_err());
    }
}

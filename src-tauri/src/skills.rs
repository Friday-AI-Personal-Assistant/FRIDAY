use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, path::PathBuf, sync::Mutex};
use tauri::{AppHandle, Manager};

const SKILLS: &[(&str, &str, &str, &[&str])] = &[
    ("web-research", "Web Research", "Research topics and prepare sourced summaries. The web-search tool will be connected in a later build.", &["Internet access"]),
    ("pdf-reader", "PDF Reader", "Read and summarise PDF documents you choose. The document-reading tool will be connected in a later build.", &["Read selected PDF files"]),
    ("file-assistant", "File Assistant", "Help organise files and folders, always asking before making changes. File tools will be connected in a later build.", &["Read selected files", "Write files after approval"]),
    ("coding-assistant", "Coding Assistant", "Help explain, write and review code. Project access and sandboxed execution will be connected in later builds.", &["Read selected project files"]),
];

static SKILLS_LOCK: Mutex<()> = Mutex::new(());

#[derive(Default, Serialize, Deserialize)]
struct SkillRegistry {
    installed: BTreeMap<String, bool>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillView {
    id: String,
    name: String,
    description: String,
    permissions: Vec<String>,
    installed: bool,
    enabled: bool,
}

fn registry_path(app: &AppHandle) -> Result<PathBuf, String> {
    let base = app.path().app_data_dir()
        .map_err(|e| format!("Cannot locate FRIDAY's application data folder: {e}"))?;
    Ok(base.join("skills").join("registry.json"))
}

fn read_registry(path: &PathBuf) -> Result<SkillRegistry, String> {
    if !path.exists() { return Ok(SkillRegistry::default()); }
    let contents = fs::read_to_string(path)
        .map_err(|e| format!("Could not read the local skills registry: {e}"))?;
    serde_json::from_str(&contents)
        .map_err(|e| format!("The local skills registry is invalid: {e}"))
}

fn save_registry(path: &PathBuf, registry: &SkillRegistry) -> Result<(), String> {
    let parent = path.parent().ok_or("Invalid skills registry path.")?;
    fs::create_dir_all(parent)
        .map_err(|e| format!("Could not create the local skills folder: {e}"))?;
    let contents = serde_json::to_vec_pretty(registry)
        .map_err(|e| format!("Could not prepare the skills registry: {e}"))?;
    fs::write(path, contents)
        .map_err(|e| format!("Could not save the skills registry: {e}"))
}

fn known_skill(skill_id: &str) -> bool {
    SKILLS.iter().any(|(id, _, _, _)| *id == skill_id.as_str())
}

#[tauri::command]
pub fn skills_catalog(app: AppHandle) -> Result<Vec<SkillView>, String> {
    let _guard = SKILLS_LOCK.lock().map_err(|_| "Skills manager is busy.".to_string())?;
    let path = registry_path(&app)?;
    let registry = read_registry(&path)?;
    Ok(SKILLS.iter().map(|(id, name, description, permissions)| {
        let enabled = registry.installed.get(*id).copied();
        SkillView {
            id: (*id).to_string(),
            name: (*name).to_string(),
            description: (*description).to_string(),
            permissions: permissions.iter().map(|p| (*p).to_string()).collect(),
            installed: enabled.is_some(),
            enabled: enabled.unwrap_or(false),
        }
    }).collect())
}

#[tauri::command]
pub fn install_skill(app: AppHandle, skill_id: String) -> Result<String, String> {
    let _guard = SKILLS_LOCK.lock().map_err(|_| "Skills manager is busy.".to_string())?;
    if !known_skill(&skill_id) { return Err("This skill is not in FRIDAY's built-in catalogue.".into()); }
    let path = registry_path(&app)?;
    let mut registry = read_registry(&path)?;
    if registry.installed.contains_key(&skill_id) { return Ok("This skill is already installed.".into()); }
    registry.installed.insert(skill_id, false);
    save_registry(&path, &registry)?;
    Ok("Skill package installed. Enable it when you are ready.".into())
}

#[tauri::command]
pub fn set_skill_enabled(app: AppHandle, skill_id: String, enabled: bool) -> Result<String, String> {
    let _guard = SKILLS_LOCK.lock().map_err(|_| "Skills manager is busy.".to_string())?;
    if !known_skill(&skill_id) { return Err("This skill is not in FRIDAY's built-in catalogue.".into()); }
    let path = registry_path(&app)?;
    let mut registry = read_registry(&path)?;
    let state = registry.installed.get_mut(&skill_id)
        .ok_or("Install this skill before enabling it.")?;
    *state = enabled;
    save_registry(&path, &registry)?;
    Ok(if enabled { "Skill enabled. Its tools will become available as they are implemented." } else { "Skill disabled." }.into())
}

#[tauri::command]
pub fn uninstall_skill(app: AppHandle, skill_id: String) -> Result<String, String> {
    let _guard = SKILLS_LOCK.lock().map_err(|_| "Skills manager is busy.".to_string())?;
    if !known_skill(&skill_id) { return Err("This skill is not in FRIDAY's built-in catalogue.".into()); }
    let path = registry_path(&app)?;
    let mut registry = read_registry(&path)?;
    registry.installed.remove(&skill_id);
    save_registry(&path, &registry)?;
    Ok("Skill package removed from this device.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogue_has_unique_ids_and_declared_permissions() {
        let mut ids = std::collections::BTreeSet::new();
        for (id, _, _, permissions) in SKILLS {
            assert!(ids.insert(*id));
            assert!(!permissions.is_empty());
        }
    }

    #[test]
    fn registry_round_trips_install_and_enable_state() {
        let mut registry = SkillRegistry::default();
        registry.installed.insert("pdf-reader".into(), false);
        registry.installed.insert("web-research".into(), true);
        let json = serde_json::to_string(&registry).unwrap();
        let restored: SkillRegistry = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.installed.get("pdf-reader"), Some(&false));
        assert_eq!(restored.installed.get("web-research"), Some(&true));
    }
}

mod workspace;
mod assistant;
mod skills;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            workspace::initialize_workspace,
            workspace::save_private_note,
            workspace::read_private_note,
            assistant::ollama_models,
            assistant::ollama_chat,
            assistant::ollama_chat_stream,
            assistant::desktop_action,
            skills::skills_catalog,
            skills::install_skill,
            skills::set_skill_enabled,
            skills::uninstall_skill
        ])
        .run(tauri::generate_context!())
        .expect("error while running FRIDAY");
}
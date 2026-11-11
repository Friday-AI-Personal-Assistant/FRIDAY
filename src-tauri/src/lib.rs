mod workspace;
mod assistant;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            workspace::initialize_workspace,
            workspace::save_private_note,
            workspace::read_private_note,
            assistant::ollama_models,
            assistant::ollama_chat,
            assistant::desktop_action
        ])
        .run(tauri::generate_context!())
        .expect("error while running FRIDAY");
}
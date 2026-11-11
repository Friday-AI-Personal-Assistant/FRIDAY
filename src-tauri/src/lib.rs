mod workspace;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            workspace::initialize_workspace,
            workspace::save_private_note,
            workspace::read_private_note
        ])
        .run(tauri::generate_context!())
        .expect("error while running FRIDAY");
}
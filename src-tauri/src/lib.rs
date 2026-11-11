mod workspace;
mod agent;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(agent::AgentState::default())
        .invoke_handler(tauri::generate_handler![
            agent::local_operation,
            agent::stop_operation,
            workspace::save_agent_state,
            workspace::read_agent_state,
            workspace::initialize_workspace,
            workspace::save_private_note,
            workspace::read_private_note
        ])
        .run(tauri::generate_context!())
        .expect("error while running FRIDAY");
}
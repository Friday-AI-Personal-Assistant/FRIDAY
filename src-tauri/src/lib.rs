mod workspace;
mod assistant;
mod skills;
mod conversations;
mod memory;
mod tools;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            workspace::initialize_workspace,
            workspace::save_private_note,
            workspace::read_private_note,
            conversations::list_conversations,
            conversations::load_conversation,
            conversations::save_conversation,
            conversations::delete_conversation,
            memory::list_memories,
            memory::add_memory,
            memory::update_memory,
            memory::set_memory_enabled,
            memory::delete_memory,
            assistant::ollama_models,
            assistant::ollama_model_details,
            assistant::ollama_pull_model,
            assistant::ollama_delete_model,
            assistant::ollama_chat,
            assistant::ollama_chat_stream,
            tools::list_tools,
            tools::execute_tool,
            tools::issue_tool_approval,
            tools::tool_history,
            tools::clear_tool_history,
            skills::skills_catalog,
            skills::install_skill,
            skills::set_skill_enabled,
            skills::load_skill,
            skills::unload_skill,
            skills::loaded_skill_instructions,
            skills::uninstall_skill
        ])
        .run(tauri::generate_context!())
        .expect("error while running FRIDAY");
}
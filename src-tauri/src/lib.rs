mod workspace;
mod assistant;
mod skills;
mod conversations;
mod memory;
mod tools;
mod pdf_reader;
mod web_research;
mod file_assistant;
mod coding_assistant;
mod coding_intelligence;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
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
            pdf_reader::pick_and_read_pdf,
            web_research::web_search,
            web_research::web_read_page,
            web_research::web_read_pages,
            coding_assistant::pick_project_folder_and_list_files,
            coding_assistant::read_project_files,
            coding_assistant::preview_code_change,
            coding_assistant::issue_code_change_approval,
            coding_assistant::cancel_code_change_preview,
            coding_assistant::apply_code_change,
            coding_assistant::undo_code_change,
            coding_assistant::list_code_change_history,
            coding_intelligence::analyze_project_intelligence,
            file_assistant::pick_and_read_text_files,
            file_assistant::pick_folder_and_list_text_files,
            file_assistant::read_granted_text_files,
            file_assistant::prepare_file_edit,
            file_assistant::issue_file_edit_approval,
            file_assistant::cancel_file_edit_preview,
            file_assistant::apply_file_edit,
            file_assistant::prepare_file_move,
            file_assistant::issue_file_move_approval,
            file_assistant::cancel_file_move_preview,
            file_assistant::apply_file_move,
            file_assistant::list_file_undo_operations,
            file_assistant::list_file_recovery_items,
            file_assistant::recover_file_transaction,
            file_assistant::prepare_file_undo,
            file_assistant::issue_file_undo_approval,
            file_assistant::cancel_file_undo_preview,
            file_assistant::apply_file_undo,
            skills::uninstall_skill
        ])
        .run(tauri::generate_context!())
        .expect("error while running FRIDAY");
}
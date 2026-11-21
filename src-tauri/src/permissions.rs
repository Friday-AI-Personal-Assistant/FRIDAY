//! Central, deny-by-default policy for the currently registered built-in tools.
//!
//! This is deliberately a small policy gateway, not yet a persistent user-grant
//! manager. Consequential actions continue to require the existing exact-action,
//! one-use approval token. Unknown tools and mismatched declarations are denied.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ToolPolicy {
    tool_id: &'static str,
    capability: &'static str,
    approval_required: bool,
}

const POLICIES: &[ToolPolicy] = &[
    ToolPolicy {
        tool_id: "calculator.evaluate",
        capability: "local.compute",
        approval_required: false,
    },
    ToolPolicy {
        tool_id: "desktop.open_target",
        capability: "desktop.launch",
        approval_required: true,
    },
];

/// Validate the tool's declared capability against the trusted backend policy.
/// Returns whether the action requires a fresh explicit approval.
pub(crate) fn authorize_tool(
    tool_id: &str,
    capability: &str,
    declared_approval_required: bool,
) -> Result<bool, String> {
    let policy = POLICIES
        .iter()
        .find(|policy| policy.tool_id == tool_id)
        .ok_or_else(|| "No capability policy exists for this tool; access is denied.".to_string())?;

    if policy.capability != capability {
        return Err("The tool's capability declaration does not match FRIDAY's policy.".into());
    }
    if policy.approval_required != declared_approval_required {
        return Err("The tool's approval declaration does not match FRIDAY's policy.".into());
    }

    Ok(policy.approval_required)
}


/// Backend-owned allowlist for file and Coding Assistant IPC commands.
/// Each command is mapped independently so an IPC caller cannot turn a
/// read/preview capability into a write or recovery capability.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CommandPolicy {
    command_id: &'static str,
    capability: &'static str,
}

const COMMAND_POLICIES: &[CommandPolicy] = &[
    CommandPolicy { command_id: "file.pick_folder_and_list_text_files", capability: "files.folder.select" },
    CommandPolicy { command_id: "file.read_granted_text_files", capability: "files.read.granted" },
    CommandPolicy { command_id: "file.prepare_file_edit", capability: "files.edit.preview" },
    CommandPolicy { command_id: "file.issue_file_edit_approval", capability: "files.edit.approval" },
    CommandPolicy { command_id: "file.cancel_file_edit_preview", capability: "files.edit.cancel" },
    CommandPolicy { command_id: "file.apply_file_edit", capability: "files.edit.apply" },
    CommandPolicy { command_id: "file.prepare_file_move", capability: "files.move.preview" },
    CommandPolicy { command_id: "file.issue_file_move_approval", capability: "files.move.approval" },
    CommandPolicy { command_id: "file.cancel_file_move_preview", capability: "files.move.cancel" },
    CommandPolicy { command_id: "file.apply_file_move", capability: "files.move.apply" },
    CommandPolicy { command_id: "file.list_file_undo_operations", capability: "files.recovery.history" },
    CommandPolicy { command_id: "file.recover_file_transaction", capability: "files.recovery.reconcile" },
    CommandPolicy { command_id: "file.list_file_recovery_items", capability: "files.recovery.inspect" },
    CommandPolicy { command_id: "file.prepare_file_undo", capability: "files.undo.preview" },
    CommandPolicy { command_id: "file.issue_file_undo_approval", capability: "files.undo.approval" },
    CommandPolicy { command_id: "file.cancel_file_undo_preview", capability: "files.undo.cancel" },
    CommandPolicy { command_id: "file.apply_file_undo", capability: "files.undo.apply" },
    CommandPolicy { command_id: "file.pick_and_read_text_files", capability: "files.read.picker" },
    CommandPolicy { command_id: "coding.pick_project_folder_and_list_files", capability: "coding.project.select" },
    CommandPolicy { command_id: "coding.list_code_change_history", capability: "coding.history.read" },
    CommandPolicy { command_id: "coding.preview_code_change", capability: "coding.change.preview" },
    CommandPolicy { command_id: "coding.issue_code_change_approval", capability: "coding.change.approval" },
    CommandPolicy { command_id: "coding.apply_code_change", capability: "coding.change.apply" },
    CommandPolicy { command_id: "coding.undo_code_change", capability: "coding.change.undo" },
    CommandPolicy { command_id: "coding.validate_multi_file_coding_proposal", capability: "coding.proposal.validate" },
    CommandPolicy { command_id: "coding.cancel_code_change_preview", capability: "coding.change.cancel" },
    CommandPolicy { command_id: "coding.read_project_files", capability: "coding.project.read" },
    CommandPolicy { command_id: "coding.analyze_project_intelligence", capability: "coding.analysis.local" },
    CommandPolicy { command_id: "coding_sandbox_preflight", capability: "coding.sandbox.preflight" },
    CommandPolicy { command_id: "coding_sandbox_prepare_snapshot", capability: "coding.sandbox.snapshot.prepare" },
    CommandPolicy { command_id: "coding_sandbox_recover_workspaces", capability: "coding.sandbox.snapshot.recover" },
    CommandPolicy { command_id: "coding_sandbox_config_preview", capability: "coding.sandbox.config.preview" },
    CommandPolicy { command_id: "coding_sandbox_discard_snapshot", capability: "coding.sandbox.snapshot.discard" },

    // Local workspace, encrypted conversation history and personal-memory operations.
    CommandPolicy { command_id: "workspace.initialize", capability: "workspace.initialize" },
    CommandPolicy { command_id: "workspace.note.write", capability: "workspace.note.write" },
    CommandPolicy { command_id: "workspace.note.read", capability: "workspace.note.read" },
    CommandPolicy { command_id: "conversations.list", capability: "conversations.list" },
    CommandPolicy { command_id: "conversations.search", capability: "conversations.search" },
    CommandPolicy { command_id: "conversations.export", capability: "conversations.export" },
    CommandPolicy { command_id: "conversations.import", capability: "conversations.import" },
    CommandPolicy { command_id: "conversations.read", capability: "conversations.read" },
    CommandPolicy { command_id: "conversations.write", capability: "conversations.write" },
    CommandPolicy { command_id: "conversations.delete", capability: "conversations.delete" },
    CommandPolicy { command_id: "conversations.pin", capability: "conversations.pin" },
    CommandPolicy { command_id: "conversations.archive", capability: "conversations.archive" },
    CommandPolicy { command_id: "memory.list", capability: "memory.list" },
    CommandPolicy { command_id: "memory.add", capability: "memory.add" },
    CommandPolicy { command_id: "memory.update", capability: "memory.update" },
    CommandPolicy { command_id: "memory.toggle", capability: "memory.toggle" },
    CommandPolicy { command_id: "memory.delete", capability: "memory.delete" },
    // Skill catalogue and local declarative-skill lifecycle. These do not grant tool permissions.
    CommandPolicy { command_id: "skills.catalog.read", capability: "skills.catalog.read" },
    CommandPolicy { command_id: "skills.import.inspect", capability: "skills.import.inspect" },
    CommandPolicy { command_id: "skills.quarantine.reinspect", capability: "skills.quarantine.reinspect" },
    CommandPolicy { command_id: "skills.quarantine.inventory", capability: "skills.quarantine.inventory" },
    CommandPolicy { command_id: "skills.install", capability: "skills.install" },
    CommandPolicy { command_id: "skills.enable", capability: "skills.enable" },
    CommandPolicy { command_id: "skills.load", capability: "skills.load" },
    CommandPolicy { command_id: "skills.instructions.read", capability: "skills.instructions.read" },
    CommandPolicy { command_id: "skills.unload", capability: "skills.unload" },
    CommandPolicy { command_id: "skills.uninstall", capability: "skills.uninstall" },

    // These commands manage one-use approval receipts only; none launches a guest process.
    CommandPolicy { command_id: "issue_guest_run_execution_approval", capability: "coding.guest.execution.approval" },
    CommandPolicy { command_id: "consume_guest_run_execution_approval", capability: "coding.guest.execution.consume" },
    CommandPolicy { command_id: "revoke_guest_run_execution_approval", capability: "coding.guest.execution.revoke" },
    CommandPolicy { command_id: "ollama_models", capability: "ai.models.list" },
    CommandPolicy { command_id: "ollama_model_details", capability: "ai.models.details" },
    CommandPolicy { command_id: "ollama_pull_model", capability: "ai.models.install" },
    CommandPolicy { command_id: "ollama_delete_model", capability: "ai.models.remove" },
    CommandPolicy { command_id: "ollama_chat_stream", capability: "ai.chat.stream" },
    CommandPolicy { command_id: "ollama_coding_chat", capability: "ai.coding.chat" },
    CommandPolicy { command_id: "ollama_chat", capability: "ai.chat.complete" },
    CommandPolicy { command_id: "pick_and_read_pdf", capability: "pdf.read.selected" },
    CommandPolicy { command_id: "web_search", capability: "web.search" },
    CommandPolicy { command_id: "web_read_page", capability: "web.read.page" },
    CommandPolicy { command_id: "web_read_pages", capability: "web.read.pages" },
    CommandPolicy { command_id: "list_tools", capability: "tools.catalog.read" },
    CommandPolicy { command_id: "tool_history", capability: "tools.history.read" },
    CommandPolicy { command_id: "clear_tool_history", capability: "tools.history.clear" },
    CommandPolicy { command_id: "issue_tool_approval", capability: "tools.approval.issue" },
    CommandPolicy { command_id: "execute_tool", capability: "tools.execute" },
    CommandPolicy { command_id: "cancel_ollama_chat", capability: "ai.chat.cancel" },
    CommandPolicy { command_id: "permissions.list", capability: "permissions.list" },
    CommandPolicy { command_id: "permissions.grant", capability: "permissions.grant" },
    CommandPolicy { command_id: "permissions.revoke", capability: "permissions.revoke" },
];

/// Authorize a specific IPC command against its trusted backend capability.
/// Folder/project selection grants and per-operation preview tokens remain
/// independently validated by their owning feature module.
pub(crate) fn authorize_command(command_id: &str, capability: &str) -> Result<(), String> {
    let policy = COMMAND_POLICIES
        .iter()
        .find(|policy| policy.command_id == command_id)
        .ok_or_else(|| "No backend capability policy exists for this operation; access is denied.".to_string())?;
    if policy.capability != capability {
        return Err("The operation's capability does not match FRIDAY's backend policy.".into());
    }
    Ok(())
}


/// Auditable inventory of every command exposed through Tauri IPC.
/// Tests enforce exact parity with lib.rs and each handler's own policy call.
#[derive(Clone, Copy, Debug)]
struct IpcCommand { handler: &'static str, command_id: &'static str, capability: &'static str, module: &'static str }
const REGISTERED_IPC_COMMANDS: &[IpcCommand] = &[
    IpcCommand { handler: "workspace::initialize_workspace", command_id: "workspace.initialize", capability: "workspace.initialize", module: "workspace" },
    IpcCommand { handler: "workspace::save_private_note", command_id: "workspace.note.write", capability: "workspace.note.write", module: "workspace" },
    IpcCommand { handler: "workspace::read_private_note", command_id: "workspace.note.read", capability: "workspace.note.read", module: "workspace" },
    IpcCommand { handler: "conversations::list_conversations", command_id: "conversations.list", capability: "conversations.list", module: "conversations" },
    IpcCommand { handler: "conversations::search_conversations", command_id: "conversations.search", capability: "conversations.search", module: "conversations" },
    IpcCommand { handler: "conversations::export_all_conversations", command_id: "conversations.export", capability: "conversations.export", module: "conversations" },
    IpcCommand { handler: "conversations::import_conversations", command_id: "conversations.import", capability: "conversations.import", module: "conversations" },
    IpcCommand { handler: "conversations::load_conversation", command_id: "conversations.read", capability: "conversations.read", module: "conversations" },
    IpcCommand { handler: "conversations::save_conversation", command_id: "conversations.write", capability: "conversations.write", module: "conversations" },
    IpcCommand { handler: "conversations::delete_conversation", command_id: "conversations.delete", capability: "conversations.delete", module: "conversations" },
    IpcCommand { handler: "conversations::set_conversation_pinned", command_id: "conversations.pin", capability: "conversations.pin", module: "conversations" },
    IpcCommand { handler: "conversations::set_conversation_archived", command_id: "conversations.archive", capability: "conversations.archive", module: "conversations" },
    IpcCommand { handler: "memory::list_memories", command_id: "memory.list", capability: "memory.list", module: "memory" },
    IpcCommand { handler: "memory::add_memory", command_id: "memory.add", capability: "memory.add", module: "memory" },
    IpcCommand { handler: "memory::update_memory", command_id: "memory.update", capability: "memory.update", module: "memory" },
    IpcCommand { handler: "memory::set_memory_enabled", command_id: "memory.toggle", capability: "memory.toggle", module: "memory" },
    IpcCommand { handler: "memory::delete_memory", command_id: "memory.delete", capability: "memory.delete", module: "memory" },
    IpcCommand { handler: "assistant::ollama_models", command_id: "ollama_models", capability: "ai.models.list", module: "assistant" },
    IpcCommand { handler: "assistant::ollama_model_details", command_id: "ollama_model_details", capability: "ai.models.details", module: "assistant" },
    IpcCommand { handler: "assistant::ollama_pull_model", command_id: "ollama_pull_model", capability: "ai.models.install", module: "assistant" },
    IpcCommand { handler: "assistant::ollama_delete_model", command_id: "ollama_delete_model", capability: "ai.models.remove", module: "assistant" },
    IpcCommand { handler: "assistant::ollama_chat", command_id: "ollama_chat", capability: "ai.chat.complete", module: "assistant" },
    IpcCommand { handler: "assistant::ollama_coding_chat", command_id: "ollama_coding_chat", capability: "ai.coding.chat", module: "assistant" },
    IpcCommand { handler: "assistant::ollama_chat_stream", command_id: "ollama_chat_stream", capability: "ai.chat.stream", module: "assistant" },
    IpcCommand { handler: "assistant::cancel_ollama_chat", command_id: "cancel_ollama_chat", capability: "ai.chat.cancel", module: "assistant" },
    IpcCommand { handler: "tools::list_tools", command_id: "list_tools", capability: "tools.catalog.read", module: "tools" },
    IpcCommand { handler: "tools::execute_tool", command_id: "execute_tool", capability: "tools.execute", module: "tools" },
    IpcCommand { handler: "tools::issue_tool_approval", command_id: "issue_tool_approval", capability: "tools.approval.issue", module: "tools" },
    IpcCommand { handler: "tools::tool_history", command_id: "tool_history", capability: "tools.history.read", module: "tools" },
    IpcCommand { handler: "tools::clear_tool_history", command_id: "clear_tool_history", capability: "tools.history.clear", module: "tools" },
    IpcCommand { handler: "skills::skills_catalog", command_id: "skills.catalog.read", capability: "skills.catalog.read", module: "skills" },
    IpcCommand { handler: "skills::inspect_and_quarantine_skill", command_id: "skills.import.inspect", capability: "skills.import.inspect", module: "skills" },
    IpcCommand { handler: "skills::reinspect_community_skill", command_id: "skills.quarantine.reinspect", capability: "skills.quarantine.reinspect", module: "skills" },
    IpcCommand { handler: "skills::list_community_skills_quarantine", command_id: "skills.quarantine.inventory", capability: "skills.quarantine.inventory", module: "skills" },
    IpcCommand { handler: "skills::install_skill", command_id: "skills.install", capability: "skills.install", module: "skills" },
    IpcCommand { handler: "skills::set_skill_enabled", command_id: "skills.enable", capability: "skills.enable", module: "skills" },
    IpcCommand { handler: "skills::load_skill", command_id: "skills.load", capability: "skills.load", module: "skills" },
    IpcCommand { handler: "skills::unload_skill", command_id: "skills.unload", capability: "skills.unload", module: "skills" },
    IpcCommand { handler: "skills::loaded_skill_instructions", command_id: "skills.instructions.read", capability: "skills.instructions.read", module: "skills" },
    IpcCommand { handler: "pdf_reader::pick_and_read_pdf", command_id: "pick_and_read_pdf", capability: "pdf.read.selected", module: "pdf_reader" },
    IpcCommand { handler: "web_research::web_search", command_id: "web_search", capability: "web.search", module: "web_research" },
    IpcCommand { handler: "web_research::web_read_page", command_id: "web_read_page", capability: "web.read.page", module: "web_research" },
    IpcCommand { handler: "web_research::web_read_pages", command_id: "web_read_pages", capability: "web.read.pages", module: "web_research" },
    IpcCommand { handler: "coding_assistant::pick_project_folder_and_list_files", command_id: "coding.pick_project_folder_and_list_files", capability: "coding.project.select", module: "coding_assistant" },
    IpcCommand { handler: "coding_assistant::read_project_files", command_id: "coding.read_project_files", capability: "coding.project.read", module: "coding_assistant" },
    IpcCommand { handler: "coding_assistant::preview_code_change", command_id: "coding.preview_code_change", capability: "coding.change.preview", module: "coding_assistant" },
    IpcCommand { handler: "coding_assistant::validate_multi_file_coding_proposal", command_id: "coding.validate_multi_file_coding_proposal", capability: "coding.proposal.validate", module: "coding_assistant" },
    IpcCommand { handler: "coding_assistant::issue_code_change_approval", command_id: "coding.issue_code_change_approval", capability: "coding.change.approval", module: "coding_assistant" },
    IpcCommand { handler: "coding_assistant::cancel_code_change_preview", command_id: "coding.cancel_code_change_preview", capability: "coding.change.cancel", module: "coding_assistant" },
    IpcCommand { handler: "coding_assistant::apply_code_change", command_id: "coding.apply_code_change", capability: "coding.change.apply", module: "coding_assistant" },
    IpcCommand { handler: "coding_assistant::undo_code_change", command_id: "coding.undo_code_change", capability: "coding.change.undo", module: "coding_assistant" },
    IpcCommand { handler: "coding_assistant::list_code_change_history", command_id: "coding.list_code_change_history", capability: "coding.history.read", module: "coding_assistant" },
    IpcCommand { handler: "coding_intelligence::analyze_project_intelligence", command_id: "coding.analyze_project_intelligence", capability: "coding.analysis.local", module: "coding_intelligence" },
    IpcCommand { handler: "coding_sandbox::coding_sandbox_preflight", command_id: "coding_sandbox_preflight", capability: "coding.sandbox.preflight", module: "coding_sandbox" },
    IpcCommand { handler: "coding_sandbox::coding_sandbox_recover_workspaces", command_id: "coding_sandbox_recover_workspaces", capability: "coding.sandbox.snapshot.recover", module: "coding_sandbox" },
    IpcCommand { handler: "coding_sandbox::coding_sandbox_prepare_snapshot", command_id: "coding_sandbox_prepare_snapshot", capability: "coding.sandbox.snapshot.prepare", module: "coding_sandbox" },
    IpcCommand { handler: "coding_sandbox::coding_sandbox_config_preview", command_id: "coding_sandbox_config_preview", capability: "coding.sandbox.config.preview", module: "coding_sandbox" },
    IpcCommand { handler: "coding_sandbox::coding_sandbox_discard_snapshot", command_id: "coding_sandbox_discard_snapshot", capability: "coding.sandbox.snapshot.discard", module: "coding_sandbox" },
    IpcCommand { handler: "coding_sandbox::issue_guest_run_execution_approval", command_id: "issue_guest_run_execution_approval", capability: "coding.guest.execution.approval", module: "coding_sandbox" },
    IpcCommand { handler: "coding_sandbox::consume_guest_run_execution_approval", command_id: "consume_guest_run_execution_approval", capability: "coding.guest.execution.consume", module: "coding_sandbox" },
    IpcCommand { handler: "coding_sandbox::revoke_guest_run_execution_approval", command_id: "revoke_guest_run_execution_approval", capability: "coding.guest.execution.revoke", module: "coding_sandbox" },
    IpcCommand { handler: "file_assistant::pick_and_read_text_files", command_id: "file.pick_and_read_text_files", capability: "files.read.picker", module: "file_assistant" },
    IpcCommand { handler: "file_assistant::pick_folder_and_list_text_files", command_id: "file.pick_folder_and_list_text_files", capability: "files.folder.select", module: "file_assistant" },
    IpcCommand { handler: "file_assistant::read_granted_text_files", command_id: "file.read_granted_text_files", capability: "files.read.granted", module: "file_assistant" },
    IpcCommand { handler: "file_assistant::prepare_file_edit", command_id: "file.prepare_file_edit", capability: "files.edit.preview", module: "file_assistant" },
    IpcCommand { handler: "file_assistant::issue_file_edit_approval", command_id: "file.issue_file_edit_approval", capability: "files.edit.approval", module: "file_assistant" },
    IpcCommand { handler: "file_assistant::cancel_file_edit_preview", command_id: "file.cancel_file_edit_preview", capability: "files.edit.cancel", module: "file_assistant" },
    IpcCommand { handler: "file_assistant::apply_file_edit", command_id: "file.apply_file_edit", capability: "files.edit.apply", module: "file_assistant" },
    IpcCommand { handler: "file_assistant::prepare_file_move", command_id: "file.prepare_file_move", capability: "files.move.preview", module: "file_assistant" },
    IpcCommand { handler: "file_assistant::issue_file_move_approval", command_id: "file.issue_file_move_approval", capability: "files.move.approval", module: "file_assistant" },
    IpcCommand { handler: "file_assistant::cancel_file_move_preview", command_id: "file.cancel_file_move_preview", capability: "files.move.cancel", module: "file_assistant" },
    IpcCommand { handler: "file_assistant::apply_file_move", command_id: "file.apply_file_move", capability: "files.move.apply", module: "file_assistant" },
    IpcCommand { handler: "file_assistant::list_file_undo_operations", command_id: "file.list_file_undo_operations", capability: "files.recovery.history", module: "file_assistant" },
    IpcCommand { handler: "file_assistant::list_file_recovery_items", command_id: "file.list_file_recovery_items", capability: "files.recovery.inspect", module: "file_assistant" },
    IpcCommand { handler: "file_assistant::recover_file_transaction", command_id: "file.recover_file_transaction", capability: "files.recovery.reconcile", module: "file_assistant" },
    IpcCommand { handler: "file_assistant::prepare_file_undo", command_id: "file.prepare_file_undo", capability: "files.undo.preview", module: "file_assistant" },
    IpcCommand { handler: "file_assistant::issue_file_undo_approval", command_id: "file.issue_file_undo_approval", capability: "files.undo.approval", module: "file_assistant" },
    IpcCommand { handler: "file_assistant::cancel_file_undo_preview", command_id: "file.cancel_file_undo_preview", capability: "files.undo.cancel", module: "file_assistant" },
    IpcCommand { handler: "file_assistant::apply_file_undo", command_id: "file.apply_file_undo", capability: "files.undo.apply", module: "file_assistant" },
    IpcCommand { handler: "skills::uninstall_skill", command_id: "skills.uninstall", capability: "skills.uninstall", module: "skills" },
    IpcCommand { handler: "user_permissions::list_permission_grants", command_id: "permissions.list", capability: "permissions.list", module: "user_permissions" },
    IpcCommand { handler: "user_permissions::grant_permission", command_id: "permissions.grant", capability: "permissions.grant", module: "user_permissions" },
    IpcCommand { handler: "user_permissions::revoke_permission", command_id: "permissions.revoke", capability: "permissions.revoke", module: "user_permissions" },
];
fn ipc_module_source(module: &str) -> &'static str {
    match module {
        "assistant" => include_str!("assistant.rs"),
        "coding_assistant" => include_str!("coding_assistant.rs"),
        "coding_intelligence" => include_str!("coding_intelligence.rs"),
        "coding_sandbox" => include_str!("coding_sandbox.rs"),
        "conversations" => include_str!("conversations.rs"),
        "file_assistant" => include_str!("file_assistant.rs"),
        "memory" => include_str!("memory.rs"),
        "pdf_reader" => include_str!("pdf_reader.rs"),
        "skills" => include_str!("skills.rs"),
        "tools" => include_str!("tools.rs"),
        "web_research" => include_str!("web_research.rs"),
        "workspace" => include_str!("workspace.rs"),
        "user_permissions" => include_str!("user_permissions.rs"),
        _ => panic!("Unknown IPC module in command manifest"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_and_coding_commands_have_explicit_backend_policies() {
        assert!(authorize_command("file.pick_folder_and_list_text_files", "files.folder.select").is_ok());
        assert!(authorize_command("file.read_granted_text_files", "files.read.granted").is_ok());
        assert!(authorize_command("file.prepare_file_edit", "files.edit.preview").is_ok());
        assert!(authorize_command("file.issue_file_edit_approval", "files.edit.approval").is_ok());
        assert!(authorize_command("file.cancel_file_edit_preview", "files.edit.cancel").is_ok());
        assert!(authorize_command("file.apply_file_edit", "files.edit.apply").is_ok());
        assert!(authorize_command("file.prepare_file_move", "files.move.preview").is_ok());
        assert!(authorize_command("file.issue_file_move_approval", "files.move.approval").is_ok());
        assert!(authorize_command("file.cancel_file_move_preview", "files.move.cancel").is_ok());
        assert!(authorize_command("file.apply_file_move", "files.move.apply").is_ok());
        assert!(authorize_command("file.list_file_undo_operations", "files.recovery.history").is_ok());
        assert!(authorize_command("file.recover_file_transaction", "files.recovery.reconcile").is_ok());
        assert!(authorize_command("file.list_file_recovery_items", "files.recovery.inspect").is_ok());
        assert!(authorize_command("file.prepare_file_undo", "files.undo.preview").is_ok());
        assert!(authorize_command("file.issue_file_undo_approval", "files.undo.approval").is_ok());
        assert!(authorize_command("file.cancel_file_undo_preview", "files.undo.cancel").is_ok());
        assert!(authorize_command("file.apply_file_undo", "files.undo.apply").is_ok());
        assert!(authorize_command("file.pick_and_read_text_files", "files.read.picker").is_ok());
        assert!(authorize_command("coding.pick_project_folder_and_list_files", "coding.project.select").is_ok());
        assert!(authorize_command("coding.list_code_change_history", "coding.history.read").is_ok());
        assert!(authorize_command("coding.preview_code_change", "coding.change.preview").is_ok());
        assert!(authorize_command("coding.issue_code_change_approval", "coding.change.approval").is_ok());
        assert!(authorize_command("coding.apply_code_change", "coding.change.apply").is_ok());
        assert!(authorize_command("coding.undo_code_change", "coding.change.undo").is_ok());
        assert!(authorize_command("coding.validate_multi_file_coding_proposal", "coding.proposal.validate").is_ok());
        assert!(authorize_command("coding.cancel_code_change_preview", "coding.change.cancel").is_ok());
        assert!(authorize_command("coding.read_project_files", "coding.project.read").is_ok());
        assert!(authorize_command("coding.analyze_project_intelligence", "coding.analysis.local").is_ok());
    }

    #[test]
    fn sandbox_lifecycle_commands_have_distinct_backend_capabilities() {
        assert!(authorize_command("coding_sandbox_preflight", "coding.sandbox.preflight").is_ok());
        assert!(authorize_command("coding_sandbox_prepare_snapshot", "coding.sandbox.snapshot.prepare").is_ok());
        assert!(authorize_command("coding_sandbox_recover_workspaces", "coding.sandbox.snapshot.recover").is_ok());
        assert!(authorize_command("coding_sandbox_config_preview", "coding.sandbox.config.preview").is_ok());
        assert!(authorize_command("coding_sandbox_discard_snapshot", "coding.sandbox.snapshot.discard").is_ok());
    }

    #[test]
    fn conversation_import_has_a_dedicated_backend_capability() {
        assert!(authorize_command("conversations.import", "conversations.import").is_ok());
        assert!(authorize_command("conversations.import", "conversations.export").is_err());
        assert!(authorize_command("unknown.import", "conversations.import").is_err());
    }

    #[test]
    fn guest_approval_receipt_commands_have_narrow_backend_capabilities() {
        assert!(authorize_command("issue_guest_run_execution_approval", "coding.guest.execution.approval").is_ok());
        assert!(authorize_command("consume_guest_run_execution_approval", "coding.guest.execution.consume").is_ok());
        assert!(authorize_command("revoke_guest_run_execution_approval", "coding.guest.execution.revoke").is_ok());
        assert!(authorize_command("issue_guest_run_execution_approval", "coding.guest.execution.consume").is_err());
        assert!(authorize_command("consume_guest_run_execution_approval", "coding.guest.execution.approval").is_err());
        assert!(authorize_command("revoke_guest_run_execution_approval", "coding.guest.execution.approval").is_err());
    }

    #[test]
    fn sandbox_capabilities_cannot_be_substituted() {
        assert!(authorize_command("coding_sandbox_discard_snapshot", "coding.sandbox.snapshot.recover").is_err());
        assert!(authorize_command("coding_sandbox_prepare_snapshot", "coding.sandbox.config.preview").is_err());
    }

    #[test]
    fn unknown_file_or_coding_operations_are_denied_by_default() {
        assert!(authorize_command("file.unknown", "files.read.granted").is_err());
        assert!(authorize_command("coding.unknown", "coding.project.read").is_err());
    }

    #[test]
    fn file_and_coding_capabilities_cannot_be_substituted() {
        assert!(authorize_command("file.apply_file_edit", "files.read.granted").is_err());
        assert!(authorize_command("coding.apply_code_change", "coding.project.read").is_err());
        assert!(authorize_command("coding.read_project_files", "coding.change.apply").is_err());
    }

    
    #[test]
    fn workspace_conversation_memory_and_skill_commands_have_narrow_capabilities() {
        let cases = [
            ("workspace.initialize", "workspace.initialize"),
            ("workspace.note.write", "workspace.note.write"),
            ("workspace.note.read", "workspace.note.read"),
            ("conversations.list", "conversations.list"),
            ("conversations.search", "conversations.search"),
            ("conversations.export", "conversations.export"),
            ("conversations.read", "conversations.read"),
            ("conversations.write", "conversations.write"),
            ("conversations.delete", "conversations.delete"),
            ("conversations.pin", "conversations.pin"),
            ("conversations.archive", "conversations.archive"),
            ("memory.list", "memory.list"),
            ("memory.add", "memory.add"),
            ("memory.update", "memory.update"),
            ("memory.toggle", "memory.toggle"),
            ("memory.delete", "memory.delete"),
            ("skills.catalog.read", "skills.catalog.read"),
            ("skills.import.inspect", "skills.import.inspect"),
            ("skills.install", "skills.install"),
            ("skills.enable", "skills.enable"),
            ("skills.load", "skills.load"),
            ("skills.instructions.read", "skills.instructions.read"),
            ("skills.unload", "skills.unload"),
            ("skills.uninstall", "skills.uninstall"),
        ];
        for (command, capability) in cases {
            assert!(authorize_command(command, capability).is_ok(), "{command}");
            assert!(authorize_command(command, "unrelated.capability").is_err(), "{command}");
        }
        assert!(authorize_command("skills.install", "skills.enable").is_err());
        assert!(authorize_command("memory.delete", "memory.list").is_err());
        assert!(authorize_command("conversations.delete", "conversations.read").is_err());
        assert!(authorize_command("conversations.search", "conversations.delete").is_err());
        assert!(authorize_command("conversations.pin", "conversations.archive").is_err());
        assert!(authorize_command("conversations.archive", "conversations.pin").is_err());
    }

    #[test]
    fn ai_provider_research_pdf_and_tool_commands_have_narrow_backend_capabilities() {
        let cases = [
            ("ollama_models", "ai.models.list"),
            ("ollama_model_details", "ai.models.details"),
            ("ollama_pull_model", "ai.models.install"),
            ("ollama_delete_model", "ai.models.remove"),
            ("ollama_chat_stream", "ai.chat.stream"),
            ("ollama_coding_chat", "ai.coding.chat"),
            ("ollama_chat", "ai.chat.complete"),
            ("pick_and_read_pdf", "pdf.read.selected"),
            ("web_search", "web.search"),
            ("web_read_page", "web.read.page"),
            ("web_read_pages", "web.read.pages"),
            ("list_tools", "tools.catalog.read"),
            ("tool_history", "tools.history.read"),
            ("clear_tool_history", "tools.history.clear"),
            ("issue_tool_approval", "tools.approval.issue"),
            ("execute_tool", "tools.execute"),
        ];
        for (command, capability) in cases {
            assert!(authorize_command(command, capability).is_ok(), "{command}");
            assert!(authorize_command(command, "unrelated.capability").is_err(), "{command}");
        }
        assert!(authorize_command("ollama_delete_model", "ai.models.list").is_err());
        assert!(authorize_command("ollama_pull_model", "ai.models.remove").is_err());
        assert!(authorize_command("web_read_pages", "web.search").is_err());
        assert!(authorize_command("clear_tool_history", "tools.history.read").is_err());
        assert!(authorize_command("execute_tool", "tools.approval.issue").is_err());
    }

    #[test]
    fn chat_cancellation_has_its_own_backend_capability() {
        assert!(authorize_command("cancel_ollama_chat", "ai.chat.cancel").is_ok());
        assert!(authorize_command("cancel_ollama_chat", "ai.chat.stream").is_err());
        assert!(authorize_command("cancel_ollama_chat", "ai.chat.complete").is_err());
        assert!(authorize_command("cancel_ollama_chat", "unrelated.capability").is_err());
    }

    #[test]
    fn known_local_computation_is_allowed_without_approval() {
        assert_eq!(authorize_tool("calculator.evaluate", "local.compute", false), Ok(false));
    }

    #[test]
    fn consequential_desktop_launch_requires_approval() {
        assert_eq!(authorize_tool("desktop.open_target", "desktop.launch", true), Ok(true));
    }

    #[test]
    fn unknown_tools_are_denied_by_default() {
        assert!(authorize_tool("unknown.tool", "local.compute", false).is_err());
    }

    #[test]
    fn capability_and_approval_declarations_cannot_weaken_policy() {
        assert!(authorize_tool("desktop.open_target", "local.compute", true).is_err());
        assert!(authorize_tool("desktop.open_target", "desktop.launch", false).is_err());
        assert!(authorize_tool("calculator.evaluate", "local.compute", true).is_err());
    }
    #[test]
    fn registered_ipc_manifest_matches_tauri_handler_exactly() {
        use std::collections::BTreeSet;
        let handler_block = include_str!("lib.rs").split("tauri::generate_handler![")
            .nth(1).expect("Tauri invoke handler must exist").split("])")
            .next().expect("Tauri invoke handler must close");
        let actual: BTreeSet<String> = handler_block.lines().filter_map(|line| {
            let entry = line.trim().trim_end_matches(',');
            if entry.contains("::") { Some(entry.to_string()) } else { None }
        }).collect();
        let expected: BTreeSet<String> = REGISTERED_IPC_COMMANDS.iter()
            .map(|entry| entry.handler.to_string()).collect();
        assert_eq!(actual, expected, "Every exposed IPC command must be inventoried exactly once");
        assert_eq!(actual.len(), REGISTERED_IPC_COMMANDS.len(), "Duplicate manifest entries are forbidden");
    }

    #[test]
    fn every_registered_ipc_handler_enforces_its_exact_policy() {
        for entry in REGISTERED_IPC_COMMANDS {
            assert!(authorize_command(entry.command_id, entry.capability).is_ok(), "Missing policy: {}", entry.handler);
            let source = ipc_module_source(entry.module);
            let function = entry.handler.split("::").nth(1).unwrap();
            let start = source.find(&format!("fn {function}(")).unwrap_or_else(|| panic!("Handler missing: {}", entry.handler));
            let remainder = &source[start..];
            let end = remainder.find("#[tauri::command]").unwrap_or(remainder.len());
            let handler_source = &remainder[..end];
            let call = format!("authorize_command(\"{}\", \"{}\")", entry.command_id, entry.capability);
            assert!(handler_source.contains(&call), "{} must call its exact backend policy", entry.handler);
        }
    }

}

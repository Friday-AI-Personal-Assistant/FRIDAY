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
    CommandPolicy { command_id: "conversations.read", capability: "conversations.read" },
    CommandPolicy { command_id: "conversations.write", capability: "conversations.write" },
    CommandPolicy { command_id: "conversations.delete", capability: "conversations.delete" },
    CommandPolicy { command_id: "memory.list", capability: "memory.list" },
    CommandPolicy { command_id: "memory.add", capability: "memory.add" },
    CommandPolicy { command_id: "memory.update", capability: "memory.update" },
    CommandPolicy { command_id: "memory.toggle", capability: "memory.toggle" },
    CommandPolicy { command_id: "memory.delete", capability: "memory.delete" },
    // Skill catalogue and local declarative-skill lifecycle. These do not grant tool permissions.
    CommandPolicy { command_id: "skills.catalog.read", capability: "skills.catalog.read" },
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
            ("conversations.read", "conversations.read"),
            ("conversations.write", "conversations.write"),
            ("conversations.delete", "conversations.delete"),
            ("memory.list", "memory.list"),
            ("memory.add", "memory.add"),
            ("memory.update", "memory.update"),
            ("memory.toggle", "memory.toggle"),
            ("memory.delete", "memory.delete"),
            ("skills.catalog.read", "skills.catalog.read"),
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
}

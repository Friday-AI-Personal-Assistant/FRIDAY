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

#[cfg(test)]
mod tests {
    use super::*;

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

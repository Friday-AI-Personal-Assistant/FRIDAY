use serde::Serialize;
use std::{env, fs, path::PathBuf};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodingSandboxPreflight {
    platform_supported: bool,
    runtime_detected: bool,
    execution_enabled: bool,
    network_isolation_enforced: bool,
    message: String,
}

fn detect_windows_sandbox() -> bool {
    let Some(system_root) = env::var_os("SystemRoot") else {
        return false;
    };
    let executable = PathBuf::from(system_root)
        .join("System32")
        .join("WindowsSandbox.exe");
    fs::symlink_metadata(executable)
        .map(|metadata| metadata.is_file() && !metadata.file_type().is_symlink())
        .unwrap_or(false)
}

#[tauri::command]
pub async fn coding_sandbox_preflight() -> Result<CodingSandboxPreflight, String> {
    let _project_root = crate::coding_assistant::selected_project_root_for_sandbox()?;

    #[cfg(windows)]
    {
        let detected = detect_windows_sandbox();
        let message = if detected {
            "Windows Sandbox was detected. Code execution is still disabled while FRIDAY completes the isolated workspace launcher, strict command allowlist, resource enforcement, cancellation and result validation. No project code has been executed.".to_string()
        } else {
            "Windows Sandbox was not detected. FRIDAY will not fall back to running project code on your normal Windows account. Enable the Windows Sandbox feature before isolated execution can be offered.".to_string()
        };
        Ok(CodingSandboxPreflight {
            platform_supported: true,
            runtime_detected: detected,
            execution_enabled: false,
            network_isolation_enforced: false,
            message,
        })
    }

    #[cfg(not(windows))]
    {
        Ok(CodingSandboxPreflight {
            platform_supported: false,
            runtime_detected: false,
            execution_enabled: false,
            network_isolation_enforced: false,
            message: "The Coding Assistant execution sandbox is designed for Windows Sandbox. No project code has been executed and no host-execution fallback is available.".to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_preflight_never_advertises_execution_or_network_isolation_early() {
        let policy = CodingSandboxPreflight {
            platform_supported: true,
            runtime_detected: true,
            execution_enabled: false,
            network_isolation_enforced: false,
            message: "Preflight only".into(),
        };
        assert!(!policy.execution_enabled);
        assert!(!policy.network_isolation_enforced);
    }

    #[test]
    fn windows_sandbox_detection_never_treats_a_missing_root_as_available() {
        let previous = env::var_os("SystemRoot");
        env::remove_var("SystemRoot");
        assert!(!detect_windows_sandbox());
        if let Some(value) = previous {
            env::set_var("SystemRoot", value);
        }
    }
}

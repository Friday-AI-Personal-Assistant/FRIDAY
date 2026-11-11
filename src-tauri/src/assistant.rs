use serde::{Deserialize, Serialize};
use std::{process::Command, time::Duration};

const OLLAMA: &str = "http://127.0.0.1:11434";

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Message { pub role: String, pub content: String }

#[derive(Serialize)]
struct Request<'a> { model: &'a str, messages: &'a [Message], stream: bool }
#[derive(Deserialize)]
struct Response { message: Message }
#[derive(Deserialize)]
struct Models { models: Vec<Model> }
#[derive(Deserialize)]
struct Model { name: String }

#[tauri::command]
pub async fn ollama_models() -> Result<Vec<String>, String> {
    let client = reqwest::Client::builder().timeout(Duration::from_secs(4)).build().map_err(|e| e.to_string())?;
    let response = client.get(format!("{OLLAMA}/api/tags")).send().await
        .map_err(|_| "Ollama is offline. Start Ollama on this computer.".to_string())?;
    let models: Models = response.error_for_status().map_err(|e| e.to_string())?
        .json().await.map_err(|e| e.to_string())?;
    Ok(models.models.into_iter().map(|m| m.name).collect())
}

#[tauri::command]
pub async fn ollama_chat(messages: Vec<Message>, model: String) -> Result<String, String> {
    if model.trim().is_empty() || messages.is_empty() || messages.len() > 50 ||
       messages.iter().any(|m| !matches!(m.role.as_str(), "user" | "assistant" | "system") || m.content.len() > 20000) {
        return Err("Invalid chat request.".into());
    }
    let client = reqwest::Client::builder().timeout(Duration::from_secs(180)).build().map_err(|e| e.to_string())?;
    let response = client.post(format!("{OLLAMA}/api/chat"))
        .json(&Request { model: &model, messages: &messages, stream: false })
        .send().await.map_err(|_| "Cannot reach Ollama. Make sure Ollama is running.".to_string())?;
    let response = response.error_for_status().map_err(|e| format!("Ollama returned an error: {e}"))?;
    let body: Response = response.json().await.map_err(|e| format!("Invalid Ollama response: {e}"))?;
    Ok(body.message.content)
}

// Explicit allowlist: no shell commands, arbitrary programs, or arbitrary URLs.
#[tauri::command]
pub fn desktop_action(action: String) -> Result<String, String> {
    let (program, args, success): (&str, Vec<&str>, &str) = match action.as_str() {
        "notepad" => ("notepad.exe", vec![], "Opened Notepad."),
        "downloads" => ("explorer.exe", vec!["shell:Downloads"], "Opened Downloads."),
        "documents" => ("explorer.exe", vec!["shell:Personal"], "Opened Documents."),
        "desktop" => ("explorer.exe", vec!["shell:Desktop"], "Opened Desktop."),
        "browser" => ("explorer.exe", vec!["https://www.google.com"], "Opened your default browser."),
        "youtube" => ("explorer.exe", vec!["https://www.youtube.com"], "Opened YouTube."),
        "email" => ("explorer.exe", vec!["mailto:"], "Opened your default email composer. Review and send manually."),
        _ => return Err("This desktop action is not permitted.".into()),
    };
    Command::new(program).args(args).spawn().map_err(|e| format!("Could not launch action: {e}"))?;
    Ok(success.into())
}

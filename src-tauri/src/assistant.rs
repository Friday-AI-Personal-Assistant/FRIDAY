use serde::{Deserialize, Serialize};
use std::{process::Command, time::Duration};

const OLLAMA: &str = "http://127.0.0.1:11434";
const MAX_MODEL_NAME_BYTES: usize = 200;
const MAX_MESSAGE_BYTES: usize = 20_000;
const MAX_TOTAL_MESSAGE_BYTES: usize = 250_000;
const MAX_MESSAGES: usize = 50;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Message {
    pub role: String,
    pub content: String,
}

#[derive(Serialize)]
struct Request<'a> {
    model: &'a str,
    messages: &'a [Message],
    stream: bool,
}

#[derive(Deserialize)]
struct Response {
    message: Message,
}

#[derive(Deserialize)]
struct Models {
    #[serde(default)]
    models: Vec<Model>,
}

#[derive(Deserialize)]
struct Model {
    name: String,
}

fn validate_chat_request(messages: &[Message], model: &str) -> Result<(), String> {
    if model.trim().is_empty()
        || model.len() > MAX_MODEL_NAME_BYTES
        || model.chars().any(char::is_control)
        || messages.is_empty()
        || messages.len() > MAX_MESSAGES
    {
        return Err("Invalid chat request. Check the selected model and message count.".into());
    }

    let mut total_bytes = 0_usize;
    for message in messages {
        if !matches!(message.role.as_str(), "user" | "assistant" | "system")
            || message.content.len() > MAX_MESSAGE_BYTES
        {
            return Err("Invalid chat request. A message role or message size is not allowed.".into());
        }

        total_bytes = total_bytes.saturating_add(message.content.len());
        if total_bytes > MAX_TOTAL_MESSAGE_BYTES {
            return Err("This conversation is too large to send to Ollama. Start a new chat.".into());
        }
    }

    Ok(())
}

fn upstream_error(status: reqwest::StatusCode, body: String) -> String {
    let detail = body.trim();
    if detail.is_empty() {
        format!("Ollama returned HTTP {status}. Check that Ollama is running correctly.")
    } else {
        let short_detail: String = detail.chars().take(400).collect();
        format!("Ollama returned HTTP {status}: {short_detail}")
    }
}

#[tauri::command]
pub async fn ollama_models() -> Result<Vec<String>, String> {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(8))
        .build()
        .map_err(|e| format!("Could not prepare the Ollama connection: {e}"))?;

    let response = client
        .get(format!("{OLLAMA}/api/tags"))
        .send()
        .await
        .map_err(|_| "Cannot connect to Ollama at 127.0.0.1:11434. Start Ollama and try Refresh again.".to_string())?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(upstream_error(status, body));
    }

    let models: Models = response
        .json()
        .await
        .map_err(|_| "Ollama responded, but its installed-model list could not be read.".to_string())?;

    let mut names: Vec<String> = models
        .models
        .into_iter()
        .map(|model| model.name.trim().to_string())
        .filter(|name| !name.is_empty())
        .collect();
    names.sort_unstable();
    names.dedup();

    Ok(names)
}

#[tauri::command]
pub async fn ollama_chat(messages: Vec<Message>, model: String) -> Result<String, String> {
    validate_chat_request(&messages, &model)?;

    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(180))
        .build()
        .map_err(|e| format!("Could not prepare the Ollama chat connection: {e}"))?;

    let response = client
        .post(format!("{OLLAMA}/api/chat"))
        .json(&Request {
            model: &model,
            messages: &messages,
            stream: false,
        })
        .send()
        .await
        .map_err(|_| "Cannot reach Ollama. Check that it is running, then try again.".to_string())?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(upstream_error(status, body));
    }

    let body: Response = response
        .json()
        .await
        .map_err(|_| "Ollama returned an unreadable chat response. Please try again.".to_string())?;

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
    Command::new(program)
        .args(args)
        .spawn()
        .map_err(|e| format!("Could not launch action: {e}"))?;
    Ok(success.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_a_normal_local_chat_request() {
        let messages = vec![Message {
            role: "user".into(),
            content: "Hello FRIDAY".into(),
        }];
        assert!(validate_chat_request(&messages, "llama3.2:1b").is_ok());
    }

    #[test]
    fn rejects_unknown_message_roles() {
        let messages = vec![Message {
            role: "tool".into(),
            content: "run this".into(),
        }];
        assert!(validate_chat_request(&messages, "llama3.2:1b").is_err());
    }

    #[test]
    fn rejects_oversized_messages_and_model_names() {
        let messages = vec![Message {
            role: "user".into(),
            content: "x".repeat(MAX_MESSAGE_BYTES + 1),
        }];
        assert!(validate_chat_request(&messages, "llama3.2:1b").is_err());
        assert!(validate_chat_request(&[], &"m".repeat(MAX_MODEL_NAME_BYTES + 1)).is_err());
    }
}

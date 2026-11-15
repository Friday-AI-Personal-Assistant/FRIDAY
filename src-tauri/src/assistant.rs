use serde::{Deserialize, Serialize};
use std::{collections::HashMap, process::Command, sync::{Mutex, OnceLock}, time::Duration};
use tokio::sync::watch;
use tauri::ipc::Channel;

const OLLAMA: &str = "http://127.0.0.1:11434";
const MAX_MODEL_NAME_BYTES: usize = 200;
const MAX_MESSAGE_BYTES: usize = 20_000;
const MAX_TOTAL_MESSAGE_BYTES: usize = 250_000;
const MAX_MESSAGES: usize = 50;
const MAX_CODING_RESPONSE_BODY_BYTES: usize = 256 * 1024;
const MAX_CODING_OUTPUT_BYTES: usize = 128 * 1024;
const MAX_CODING_MESSAGES: usize = 50;
const MAX_CODING_TOTAL_MESSAGE_BYTES: usize = 230_000;
const MAX_CODING_SYSTEM_MESSAGE_BYTES: usize = 8_000;

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
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<serde_json::Value>,
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


fn validate_coding_chat_request(messages: &[Message], model: &str) -> Result<(), String> {
    validate_chat_request(messages, model)?;
    if messages.len() < 2
        || messages.len() > MAX_CODING_MESSAGES
        || messages.first().map_or(true, |message| message.role != "system")
        || messages.last().map_or(true, |message| message.role != "user")
        || messages.iter().skip(1).any(|message| message.role != "user")
        || messages.first().is_some_and(|message| message.content.len() > MAX_CODING_SYSTEM_MESSAGE_BYTES)
    {
        return Err("The coding request must contain one bounded system instruction followed by project context and a final user task.".into());
    }
    let total = messages.iter().fold(0usize, |sum, message| sum.saturating_add(message.content.len()));
    if total > MAX_CODING_TOTAL_MESSAGE_BYTES {
        return Err("The selected project context and coding task exceed FRIDAY's dedicated coding-request size limit. Select fewer or smaller files.".into());
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


#[derive(Serialize, Clone)]
pub struct ModelInfo { pub name: String, pub size: u64, pub modified_at: String }
#[derive(Deserialize)]
struct ModelTags { #[serde(default)] models: Vec<ModelTag> }
#[derive(Deserialize)]
struct ModelTag {
    name: String,
    #[serde(default)] size: u64,
    #[serde(default)] modified_at: String,
}
fn validate_model_name(name: &str) -> Result<(), String> {
    let value = name.trim();
    if value.is_empty() || value.len() > MAX_MODEL_NAME_BYTES
        || !value.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':' | '/'))
        || value.starts_with('/') || value.ends_with('/') || value.contains("//")
    { return Err("Enter a valid Ollama model name, for example llama3.2:1b.".into()); }
    Ok(())
}
#[derive(Serialize, Clone)]
pub struct ModelPullEvent { pub status: String, pub completed: Option<u64>, pub total: Option<u64>, pub done: bool }
#[derive(Deserialize)]
struct PullResponse {
    #[serde(default)] status: String,
    #[serde(default)] completed: Option<u64>,
    #[serde(default)] total: Option<u64>,
    #[serde(default)] error: Option<String>,
}
#[tauri::command]
pub async fn ollama_model_details() -> Result<Vec<ModelInfo>, String> {
    let client = reqwest::Client::builder().connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(12)).build()
        .map_err(|e| format!("Could not prepare the Ollama connection: {e}"))?;
    let response = client.get(format!("{OLLAMA}/api/tags")).send().await
        .map_err(|_| "Cannot connect to Ollama at 127.0.0.1:11434. Start Ollama and refresh.".to_string())?;
    let status = response.status();
    if !status.is_success() { return Err(upstream_error(status, response.text().await.unwrap_or_default())); }
    let tags: ModelTags = response.json().await
        .map_err(|_| "Ollama returned an unreadable installed-model list.".to_string())?;
    let mut models: Vec<ModelInfo> = tags.models.into_iter().map(|m| ModelInfo {
        name: m.name, size: m.size, modified_at: m.modified_at,
    }).collect();
    models.sort_by(|a,b| a.name.cmp(&b.name));
    Ok(models)
}
#[tauri::command]
pub async fn ollama_pull_model(name: String, on_event: Channel<ModelPullEvent>) -> Result<(), String> {
    validate_model_name(&name)?;
    let client = reqwest::Client::builder().connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(7200)).build()
        .map_err(|e| format!("Could not prepare the model download: {e}"))?;
    let mut response = client.post(format!("{OLLAMA}/api/pull"))
        .json(&serde_json::json!({"name": name, "stream": true})).send().await
        .map_err(|_| "Cannot reach Ollama. Start Ollama and try the download again.".to_string())?;
    let status = response.status();
    if !status.is_success() { return Err(upstream_error(status, response.text().await.unwrap_or_default())); }
    let mut pending = Vec::<u8>::new();
    while let Some(bytes) = response.chunk().await
        .map_err(|_| "The model download connection was interrupted. Check Ollama and refresh installed models.".to_string())?
    {
        pending.extend_from_slice(&bytes);
        if pending.len() > 1_000_000 && !pending.contains(&b'\n') {
            return Err("Ollama sent an unexpectedly large download status.".into());
        }
        while let Some(newline) = pending.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = pending.drain(..=newline).collect();
            if line.iter().all(u8::is_ascii_whitespace) { continue; }
            let progress: PullResponse = serde_json::from_slice(&line)
                .map_err(|_| "Ollama sent an unreadable model-download status.".to_string())?;
            if let Some(error) = progress.error {
                return Err(format!("Ollama could not download this model: {}", error.chars().take(400).collect::<String>()));
            }
            let done = progress.status.eq_ignore_ascii_case("success");
            on_event.send(ModelPullEvent { status: progress.status, completed: progress.completed, total: progress.total, done })
                .map_err(|e| format!("Could not update download progress in FRIDAY: {e}"))?;
            if done { return Ok(()); }
        }
    }
    if !pending.iter().all(u8::is_ascii_whitespace) {
        let progress: PullResponse = serde_json::from_slice(&pending)
            .map_err(|_| "Ollama sent an unreadable final download status.".to_string())?;
        if let Some(error) = progress.error { return Err(error); }
        let done = progress.status.eq_ignore_ascii_case("success");
        on_event.send(ModelPullEvent { status: progress.status, completed: progress.completed, total: progress.total, done })
            .map_err(|e| format!("Could not update download progress in FRIDAY: {e}"))?;
        if done { return Ok(()); }
    }
    Err("Ollama closed the download stream before confirming success. Refresh the model list before retrying.".into())
}
#[tauri::command]
pub async fn ollama_delete_model(name: String) -> Result<(), String> {
    validate_model_name(&name)?;
    let client = reqwest::Client::builder().connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(30)).build()
        .map_err(|e| format!("Could not prepare the model removal request: {e}"))?;
    let response = client.delete(format!("{OLLAMA}/api/delete"))
        .json(&serde_json::json!({"name": name})).send().await
        .map_err(|_| "Cannot reach Ollama. Start Ollama and try again.".to_string())?;
    let status = response.status();
    if !status.is_success() { return Err(upstream_error(status, response.text().await.unwrap_or_default())); }
    Ok(())
}


#[derive(Serialize, Clone)]
pub struct StreamEvent {
    pub event: String,
    pub content: String,
}

#[derive(Deserialize)]
struct Response {
    message: Message,
}

#[derive(Deserialize)]
struct StreamToolCall {
    function: StreamToolFunction,
}
#[derive(Deserialize)]
struct StreamToolFunction {
    name: String,
    arguments: serde_json::Value,
}
#[derive(Deserialize)]
struct StreamMessage {
    #[serde(default)]
    content: String,
    #[serde(default)]
    tool_calls: Vec<StreamToolCall>,
}
#[derive(Deserialize)]
struct StreamResponse {
    #[serde(default)]
    message: Option<StreamMessage>,
    #[serde(default)]
    done: bool,
    #[serde(default)]
    error: Option<String>,
}

fn forward_stream_line(line: &[u8], channel: &Channel<StreamEvent>) -> Result<bool, String> {
    if line.iter().all(u8::is_ascii_whitespace) {
        return Ok(false);
    }
    let chunk: StreamResponse = serde_json::from_slice(line)
        .map_err(|_| "Ollama sent an unreadable streaming response.".to_string())?;
    if let Some(error) = chunk.error {
        return Err(format!("Ollama streaming error: {}", error.chars().take(400).collect::<String>()));
    }
    if let Some(message) = chunk.message {
        if !message.content.is_empty() {
            channel.send(StreamEvent { event: "token".into(), content: message.content })
                .map_err(|e| format!("Could not deliver the response to FRIDAY: {e}"))?;
        }
        for call in message.tool_calls.into_iter().take(1) {
            if call.function.name.len() > 100 {
                return Err("Ollama proposed a tool name that exceeds FRIDAY's safety limit.".into());
            }
            let proposal = serde_json::json!({
                "tool_id": call.function.name,
                "arguments": call.function.arguments
            });
            let encoded = proposal.to_string();
            if encoded.len() > 8_192 {
                return Err("Ollama proposed tool arguments that exceed FRIDAY's 8 KB safety limit.".into());
            }
            channel.send(StreamEvent {
                event: "tool_proposal".into(),
                content: encoded,
            }).map_err(|e| format!("Could not deliver a tool proposal to FRIDAY: {e}"))?;
        }
    }
    Ok(chunk.done)
}

static ACTIVE_STREAMS: OnceLock<Mutex<HashMap<String, watch::Sender<bool>>>> = OnceLock::new();

fn active_streams() -> &'static Mutex<HashMap<String, watch::Sender<bool>>> {
    ACTIVE_STREAMS.get_or_init(|| Mutex::new(HashMap::new()))
}

async fn run_ollama_stream(
    messages: Vec<Message>,
    model: String,
    on_event: Channel<StreamEvent>,
    cancel_rx: &mut watch::Receiver<bool>,
) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(180))
        .build()
        .map_err(|e| format!("Could not prepare the Ollama chat connection: {e}"))?;

    let request = client
        .post(format!("{OLLAMA}/api/chat"))
        .json(&Request {
            model: &model,
            messages: &messages,
            stream: true,
            tools: Some(serde_json::json!([
                {
                    "type": "function",
                    "function": {
                        "name": "calculator.evaluate",
                        "description": "Evaluate a basic arithmetic expression locally. Use only for arithmetic.",
                        "parameters": {
                            "type": "object",
                            "properties": { "expression": { "type": "string", "description": "Arithmetic expression using numbers, +, -, *, / and parentheses." } },
                            "required": ["expression"]
                        }
                    }
                },
                {
                    "type": "function",
                    "function": {
                        "name": "desktop.open_target",
                        "description": "Open one fixed Windows destination. Requires explicit user approval in FRIDAY.",
                        "parameters": {
                            "type": "object",
                            "properties": { "target": { "type": "string", "enum": ["notepad", "downloads", "documents", "desktop", "browser", "youtube", "email"] } },
                            "required": ["target"]
                        }
                    }
                }
            ]))
        });

    let send_result = tokio::select! {
        _ = cancel_rx.changed() => {
            let _ = on_event.send(StreamEvent { event: "stopped".into(), content: String::new() });
            return Ok(());
        }
        result = request.send() => result,
    };
    let mut response = send_result
        .map_err(|_| "Cannot reach Ollama. Check that it is running, then try again.".to_string())?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(upstream_error(status, body));
    }

    let mut pending = Vec::<u8>::new();
    let mut finished = false;
    while !finished {
        let chunk_result = tokio::select! {
            _ = cancel_rx.changed() => {
                let _ = on_event.send(StreamEvent { event: "stopped".into(), content: String::new() });
                return Ok(());
            }
            result = response.chunk() => result,
        };
        let Some(bytes) = chunk_result
            .map_err(|_| "The connection to Ollama was interrupted while generating the answer.".to_string())?
        else { break };

        pending.extend_from_slice(&bytes);
        if pending.len() > 1_000_000 && !pending.contains(&b'\n') {
            return Err("Ollama sent an unexpectedly large streaming response chunk.".into());
        }
        while let Some(newline) = pending.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = pending.drain(..=newline).collect();
            finished = forward_stream_line(&line, &on_event)?;
            if finished { break; }
        }
    }

    if !pending.iter().all(u8::is_ascii_whitespace) {
        forward_stream_line(&pending, &on_event)?;
    }
    on_event.send(StreamEvent { event: "done".into(), content: String::new() })
        .map_err(|e| format!("Could not finish the response in FRIDAY: {e}"))?;
    Ok(())
}

#[tauri::command]
pub async fn ollama_chat_stream(
    messages: Vec<Message>,
    model: String,
    request_id: String,
    on_event: Channel<StreamEvent>,
) -> Result<(), String> {
    validate_chat_request(&messages, &model)?;
    if request_id.is_empty() || request_id.len() > 80
        || !request_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("Invalid response request identifier.".into());
    }

    let (cancel_tx, mut cancel_rx) = watch::channel(false);
    {
        let mut streams = active_streams().lock()
            .map_err(|_| "FRIDAY could not access its active response list.".to_string())?;
        if streams.contains_key(&request_id) {
            return Err("This response is already active.".into());
        }
        streams.insert(request_id.clone(), cancel_tx);
    }

    let result = run_ollama_stream(messages, model, on_event, &mut cancel_rx).await;
    if let Ok(mut streams) = active_streams().lock() {
        streams.remove(&request_id);
    }
    result
}

#[tauri::command]
pub fn cancel_ollama_chat(request_id: String) -> Result<bool, String> {
    let streams = active_streams().lock()
        .map_err(|_| "FRIDAY could not access its active response list.".to_string())?;
    match streams.get(&request_id) {
        Some(cancel_tx) => {
            cancel_tx.send(true)
                .map_err(|_| "The response has already finished.".to_string())?;
            Ok(true)
        }
        None => Ok(false),
    }
}

fn parse_bounded_coding_response(body: &[u8]) -> Result<String, String> {
    if body.is_empty() || body.len() > MAX_CODING_RESPONSE_BODY_BYTES {
        return Err("Ollama's coding response was empty or exceeded FRIDAY's response-size limit.".into());
    }
    let parsed: Response = serde_json::from_slice(body)
        .map_err(|_| "Ollama returned an unreadable coding response.".to_string())?;
    if parsed.message.content.trim().is_empty() {
        return Err("The local model returned an empty coding proposal.".into());
    }
    if parsed.message.content.len() > MAX_CODING_OUTPUT_BYTES {
        return Err("The generated file exceeds FRIDAY's 128 KB coding proposal limit.".into());
    }
    Ok(parsed.message.content)
}

async fn read_bounded_ollama_body(
    mut response: reqwest::Response,
    max_bytes: usize,
) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await
        .map_err(|_| "The connection to Ollama was interrupted while receiving the coding response.".to_string())?
    {
        if body.len().saturating_add(chunk.len()) > max_bytes {
            return Err("Ollama's response exceeded FRIDAY's fixed response-size limit.".into());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[tauri::command]
pub async fn ollama_coding_chat(messages: Vec<Message>, model: String) -> Result<String, String> {
    validate_coding_chat_request(&messages, &model)?;
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(240))
        .build()
        .map_err(|e| format!("Could not prepare the local coding connection: {e}"))?;
    let response = client
        .post(format!("{OLLAMA}/api/chat"))
        .json(&Request {
            model: &model,
            messages: &messages,
            stream: false,
            tools: None,
        })
        .send()
        .await
        .map_err(|_| "Cannot reach Ollama. Check that it is running, then try the coding task again.".to_string())?;
    let status = response.status();
    let body = read_bounded_ollama_body(response, MAX_CODING_RESPONSE_BODY_BYTES).await?;
    if !status.is_success() {
        return Err(upstream_error(status, String::from_utf8_lossy(&body).into_owned()));
    }
    parse_bounded_coding_response(&body)
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
            tools: None,
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


    #[test]
    fn coding_chat_requires_system_context_and_a_final_user_task() {
        let valid = vec![
            Message { role: "system".into(), content: "coding instructions".into() },
            Message { role: "user".into(), content: "selected source".into() },
            Message { role: "user".into(), content: "implement a feature".into() },
        ];
        assert!(validate_coding_chat_request(&valid, "llama3.2:1b").is_ok());
        let missing_system = vec![Message { role: "user".into(), content: "task".into() }];
        assert!(validate_coding_chat_request(&missing_system, "llama3.2:1b").is_err());
        let wrong_last_role = vec![
            Message { role: "system".into(), content: "instructions".into() },
            Message { role: "assistant".into(), content: "not accepted".into() },
        ];
        assert!(validate_coding_chat_request(&wrong_last_role, "llama3.2:1b").is_err());
    }

    #[test]
    fn coding_chat_rejects_excessive_system_and_total_context() {
        let too_large_system = vec![
            Message { role: "system".into(), content: "x".repeat(MAX_CODING_SYSTEM_MESSAGE_BYTES + 1) },
            Message { role: "user".into(), content: "task".into() },
        ];
        assert!(validate_coding_chat_request(&too_large_system, "llama3.2:1b").is_err());
        let too_large_total = vec![
            Message { role: "system".into(), content: "instructions".into() },
            Message { role: "user".into(), content: "x".repeat(MAX_CODING_TOTAL_MESSAGE_BYTES) },
        ];
        assert!(validate_coding_chat_request(&too_large_total, "llama3.2:1b").is_err());
    }

    #[test]
    fn bounded_coding_response_accepts_valid_content_at_the_limit() {
        let content = "x".repeat(MAX_CODING_OUTPUT_BYTES);
        let body = serde_json::json!({"message":{"role":"assistant","content":content}})
            .to_string();
        assert_eq!(parse_bounded_coding_response(body.as_bytes()).unwrap().len(), MAX_CODING_OUTPUT_BYTES);
    }

    #[test]
    fn bounded_coding_response_rejects_oversized_content_and_body() {
        let content = "x".repeat(MAX_CODING_OUTPUT_BYTES + 1);
        let body = serde_json::json!({"message":{"role":"assistant","content":content}})
            .to_string();
        assert!(parse_bounded_coding_response(body.as_bytes()).is_err());
        assert!(parse_bounded_coding_response(&vec![b' '; MAX_CODING_RESPONSE_BODY_BYTES + 1]).is_err());
    }

    #[test]
    fn bounded_coding_response_rejects_empty_or_malformed_content() {
        assert!(parse_bounded_coding_response(br#"{"message":{"role":"assistant","content":""}}"#).is_err());
        assert!(parse_bounded_coding_response(b"not-json").is_err());
    }

    #[test]
    fn parses_a_structured_ollama_tool_proposal() {
        let raw = br#"{"message":{"role":"assistant","content":"","tool_calls":[{"function":{"name":"calculator.evaluate","arguments":{"expression":"2 + 2"}}}]},"done":true}"#;
        let parsed: StreamResponse = serde_json::from_slice(raw).unwrap();
        let call = &parsed.message.unwrap().tool_calls[0];
        assert_eq!(call.function.name, "calculator.evaluate");
        assert_eq!(call.function.arguments["expression"], "2 + 2");
    }
}

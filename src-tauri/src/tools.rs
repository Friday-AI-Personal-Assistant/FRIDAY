use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::{HashMap, VecDeque}, fs, path::PathBuf, process::Command, sync::{LazyLock, Mutex}, time::{SystemTime, UNIX_EPOCH}};
use tauri::{AppHandle, Manager};

#[derive(Clone, Serialize)]
pub struct ToolDefinition {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub permission: &'static str,
    pub approval_required: bool,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct ToolEvent {
    pub id: u64,
    pub tool_id: String,
    pub label: String,
    pub outcome: String,
    pub timestamp: u64,
    pub approved: bool,
}
#[derive(Clone)]
struct PendingApproval {
    tool_id: String,
    arguments: Value,
    expires_at: u64,
}
static PENDING_APPROVALS: LazyLock<Mutex<HashMap<String, PendingApproval>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static HISTORY_LOCK: Mutex<()> = Mutex::new(());

const HISTORY_FILE: &str = "tool-activity.enc";
const MAX_HISTORY: usize = 500;
const MAX_HISTORY_BYTES: u64 = 512 * 1024;
const TOOLS: &[ToolDefinition] = &[
    ToolDefinition { id: "calculator.evaluate", name: "Calculator", description: "Evaluate a basic arithmetic expression locally.", permission: "No access (local computation)", approval_required: false },
    ToolDefinition { id: "desktop.open_target", name: "Open desktop item", description: "Open one of FRIDAY's fixed, approved Windows destinations.", permission: "External application launch", approval_required: true },
];

fn workspace_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let base = app.path().app_data_dir().map_err(|e| format!("Cannot locate application data: {e}"))?;
    Ok(base.join("workspace"))
}
fn now() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() }
fn valid_target(arguments: &Value) -> Result<&str, String> {
    let target = arguments.get("target").and_then(Value::as_str)
        .ok_or("A permitted desktop target is required.")?;
    if !matches!(target, "notepad" | "downloads" | "documents" | "desktop" | "browser" | "youtube" | "email") {
        return Err("This desktop destination is not in FRIDAY's allowlist.".into());
    }
    Ok(target)
}
fn read_history_locked(app: &AppHandle) -> Result<VecDeque<ToolEvent>, String> {
    let path = workspace_dir(app)?.join(HISTORY_FILE);
    if !path.exists() { return Ok(VecDeque::new()); }
    let metadata = fs::metadata(&path).map_err(|e| format!("Could not inspect tool history: {e}"))?;
    if metadata.len() > MAX_HISTORY_BYTES { return Err("Encrypted tool history exceeds its safety limit.".into()); }
    let payload = fs::read(path).map_err(|e| format!("Could not read encrypted tool history: {e}"))?;
    let key = crate::workspace::encryption_key()?;
    let plain = crate::workspace::decrypt_bytes(&key, &payload)?;
    let items: Vec<ToolEvent> = serde_json::from_slice(&plain).map_err(|_| "Saved tool history is unreadable.".to_string())?;
    if items.len() > MAX_HISTORY { return Err("Saved tool history contains too many entries.".into()); }
    Ok(items.into())
}
fn write_history_locked(app: &AppHandle, items: &VecDeque<ToolEvent>) -> Result<(), String> {
    let directory = workspace_dir(app)?;
    fs::create_dir_all(&directory).map_err(|e| format!("Could not create the local workspace: {e}"))?;
    let plain = serde_json::to_vec(items).map_err(|e| format!("Could not prepare tool history: {e}"))?;
    let key = crate::workspace::encryption_key()?;
    let encrypted = crate::workspace::encrypt_bytes(&key, &plain)?;
    let destination = directory.join(HISTORY_FILE);
    let temporary = directory.join("tool-activity.enc.tmp");
    fs::write(&temporary, encrypted).map_err(|e| format!("Could not save encrypted tool history: {e}"))?;
    if destination.exists() { fs::remove_file(&destination).map_err(|e| format!("Could not replace tool history: {e}"))?; }
    fs::rename(&temporary, &destination).map_err(|e| format!("Could not finish saving tool history: {e}"))?;
    Ok(())
}
fn record(app: &AppHandle, tool_id: &str, outcome: &str, approved: bool) -> Result<(), String> {
    let _guard = HISTORY_LOCK.lock().map_err(|_| "Tool history is temporarily unavailable.".to_string())?;
    let mut history = read_history_locked(app)?;
    let id = history.back().map(|item| item.id.saturating_add(1)).unwrap_or(1);
    history.push_back(ToolEvent {
        id, tool_id: tool_id.chars().take(100).collect(), label: tool_id.chars().take(120).collect(),
        outcome: outcome.chars().take(240).collect(), timestamp: now(), approved,
    });
    while history.len() > MAX_HISTORY { history.pop_front(); }
    write_history_locked(app, &history)
}

#[tauri::command]
pub fn list_tools() -> Vec<ToolDefinition> { TOOLS.to_vec() }

#[tauri::command]
pub fn tool_history(app: AppHandle) -> Result<Vec<ToolEvent>, String> {
    let _guard = HISTORY_LOCK.lock().map_err(|_| "Tool history is temporarily unavailable.".to_string())?;
    Ok(read_history_locked(&app)?.iter().rev().cloned().collect())
}

#[tauri::command]
pub fn clear_tool_history(app: AppHandle) -> Result<(), String> {
    let _guard = HISTORY_LOCK.lock().map_err(|_| "Tool history is temporarily unavailable.".to_string())?;
    let path = workspace_dir(&app)?.join(HISTORY_FILE);
    if path.exists() { fs::remove_file(path).map_err(|e| format!("Could not clear tool history: {e}"))?; }
    Ok(())
}

#[tauri::command]
pub fn issue_tool_approval(tool_id: String, arguments: Value) -> Result<String, String> {
    let definition = TOOLS.iter().find(|tool| tool.id == tool_id).ok_or("This tool is not registered in FRIDAY.")?;
    if !definition.approval_required { return Err("This tool does not use an approval token.".into()); }
    if tool_id == "desktop.open_target" { valid_target(&arguments)?; }
    let mut bytes = [0_u8; 32];
    OsRng.fill_bytes(&mut bytes);
    let token = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
    let mut approvals = PENDING_APPROVALS.lock().map_err(|_| "Approval service is temporarily unavailable.".to_string())?;
    approvals.retain(|_, item| item.expires_at >= now());
    if approvals.len() >= 128 { return Err("Too many pending approvals. Finish or cancel an existing request.".into()); }
    approvals.insert(token.clone(), PendingApproval { tool_id, arguments, expires_at: now() + 120 });
    Ok(token)
}

fn consume_approval(token: Option<String>, tool_id: &str, arguments: &Value) -> Result<(), String> {
    let token = token.ok_or("This action requires a fresh, one-use approval.")?;
    let mut approvals = PENDING_APPROVALS.lock().map_err(|_| "Approval service is temporarily unavailable.".to_string())?;
    let approval = approvals.remove(&token).ok_or("This approval is invalid, expired, or already used.")?;
    if approval.expires_at < now() || approval.tool_id != tool_id || approval.arguments != *arguments {
        return Err("Approval did not match this exact action. Please review and approve again.".into());
    }
    Ok(())
}

#[tauri::command]
pub fn execute_tool(app: AppHandle, tool_id: String, arguments: Value, approval_token: Option<String>) -> Result<String, String> {
    let result = match tool_id.as_str() {
        "calculator.evaluate" => {
            let expression = arguments.get("expression").and_then(Value::as_str).ok_or("Calculator requires an expression string.")?;
            if expression.len() > 256 { return Err("Calculator expression is too long.".into()); }
            evaluate(expression).map(format_number)?
        }
        "desktop.open_target" => {
            let target = valid_target(&arguments)?;
            consume_approval(approval_token, &tool_id, &arguments)?;
            let (program, args, label) = match target {
                "notepad" => ("notepad.exe", vec![], "Opened Notepad."),
                "downloads" => ("explorer.exe", vec!["shell:Downloads"], "Opened Downloads."),
                "documents" => ("explorer.exe", vec!["shell:Personal"], "Opened Documents."),
                "desktop" => ("explorer.exe", vec!["shell:Desktop"], "Opened Desktop."),
                "browser" => ("explorer.exe", vec!["https://www.google.com"], "Opened your default browser."),
                "youtube" => ("explorer.exe", vec!["https://www.youtube.com"], "Opened YouTube."),
                "email" => ("explorer.exe", vec!["mailto:"], "Opened your default email composer. Review and send manually."),
                _ => return Err("This desktop destination is not in FRIDAY's allowlist.".into()),
            };
            Command::new(program).args(args).spawn().map(|_| label.to_string())
                .map_err(|e| format!("Could not launch the approved destination: {e}"))?
        }
        _ => return Err("This tool is not registered in FRIDAY.".into()),
    };
    if let Err(error) = record(&app, &tool_id, &result, tool_id == "desktop.open_target") {
        return Ok(format!("{result} Tool activity could not be saved: {error}"));
    }
    Ok(result)
}

fn format_number(value: f64) -> String {
    if value == 0.0 { return "0".into(); }
    let mut out = format!("{value:.10}");
    while out.contains('.') && out.ends_with('0') { out.pop(); }
    if out.ends_with('.') { out.pop(); }
    out
}
struct Parser<'a> { chars: Vec<char>, pos: usize, _source: &'a str }
impl<'a> Parser<'a> {
    fn new(source: &'a str) -> Self { Self { chars: source.chars().collect(), pos: 0, _source: source } }
    fn spaces(&mut self) { while self.pos < self.chars.len() && self.chars[self.pos].is_whitespace() { self.pos += 1; } }
    fn eat(&mut self, c: char) -> bool { self.spaces(); if self.chars.get(self.pos) == Some(&c) { self.pos += 1; true } else { false } }
    fn expression(&mut self) -> Result<f64, String> {
        let mut v = self.term()?;
        loop { if self.eat('+') { v += self.term()?; } else if self.eat('-') { v -= self.term()?; } else { break; } }
        Ok(v)
    }
    fn term(&mut self) -> Result<f64, String> {
        let mut v = self.factor()?;
        loop {
            if self.eat('*') { v *= self.factor()?; }
            else if self.eat('/') { let d = self.factor()?; if d == 0.0 { return Err("Division by zero is not allowed.".into()); } v /= d; }
            else { break; }
            if !v.is_finite() { return Err("The result is outside the supported numeric range.".into()); }
        }
        Ok(v)
    }
    fn factor(&mut self) -> Result<f64, String> {
        if self.eat('+') { return self.factor(); }
        if self.eat('-') { return Ok(-self.factor()?); }
        if self.eat('(') { let v = self.expression()?; if !self.eat(')') { return Err("Missing closing parenthesis.".into()); } return Ok(v); }
        self.spaces();
        let start = self.pos;
        while self.pos < self.chars.len() && (self.chars[self.pos].is_ascii_digit() || self.chars[self.pos] == '.') { self.pos += 1; }
        if start == self.pos { return Err("Use numbers, +, -, *, / and parentheses only.".into()); }
        let token: String = self.chars[start..self.pos].iter().collect();
        let value: f64 = token.parse().map_err(|_| "Invalid number in expression.".to_string())?;
        if !value.is_finite() { return Err("Number is outside the supported range.".into()); }
        Ok(value)
    }
}
fn evaluate(source: &str) -> Result<f64, String> {
    if source.trim().is_empty() { return Err("Enter an arithmetic expression.".into()); }
    let mut parser = Parser::new(source);
    let value = parser.expression()?;
    parser.spaces();
    if parser.pos != parser.chars.len() { return Err("Unsupported characters. Only basic arithmetic is allowed.".into()); }
    if !value.is_finite() { return Err("The result is outside the supported numeric range.".into()); }
    Ok(value)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn calculator_obeys_precedence_and_parentheses() {
        assert_eq!(evaluate("2 + 3 * (4 - 1)").unwrap(), 11.0);
        assert_eq!(evaluate("-2.5 * 4").unwrap(), -10.0);
    }
    #[test] fn calculator_rejects_division_by_zero_and_code() {
        assert!(evaluate("5 / 0").is_err());
        assert!(evaluate("1 + std::process::exit(0)").is_err());
    }
    #[test] fn registry_contains_only_explicit_tools() {
        assert_eq!(TOOLS.len(), 2);
        assert!(TOOLS.iter().any(|tool| tool.id == "desktop.open_target" && tool.approval_required));
    }
    #[test] fn approval_tokens_are_bound_to_arguments_and_one_use() {
        let token = "test-token".to_string();
        let args = serde_json::json!({"target":"notepad"});
        PENDING_APPROVALS.lock().unwrap().insert(token.clone(), PendingApproval {
            tool_id: "desktop.open_target".into(), arguments: args.clone(), expires_at: now() + 60
        });
        assert!(consume_approval(Some(token.clone()), "desktop.open_target", &serde_json::json!({"target":"downloads"})).is_err());
        assert!(consume_approval(Some(token.clone()), "desktop.open_target", &args).is_err());
        let valid_token = "valid-once".to_string();
        PENDING_APPROVALS.lock().unwrap().insert(valid_token.clone(), PendingApproval {
            tool_id: "desktop.open_target".into(), arguments: args.clone(), expires_at: now() + 60
        });
        assert!(consume_approval(Some(valid_token.clone()), "desktop.open_target", &args).is_ok());
        assert!(consume_approval(Some(valid_token), "desktop.open_target", &args).is_err());
    }
}

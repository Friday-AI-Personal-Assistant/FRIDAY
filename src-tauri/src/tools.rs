use serde::Serialize;
use serde_json::Value;
use std::{collections::VecDeque, process::Command, sync::Mutex, time::{SystemTime, UNIX_EPOCH}};

#[derive(Clone, Serialize)]
pub struct ToolDefinition {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub permission: &'static str,
    pub approval_required: bool,
}
#[derive(Clone, Serialize)]
pub struct ToolEvent {
    pub id: u64,
    pub tool_id: String,
    pub label: String,
    pub outcome: String,
    pub timestamp: u64,
    pub approved: bool,
}
static HISTORY: Mutex<VecDeque<ToolEvent>> = Mutex::new(VecDeque::new());
static NEXT_ID: Mutex<u64> = Mutex::new(1);

const TOOLS: &[ToolDefinition] = &[
    ToolDefinition { id: "calculator.evaluate", name: "Calculator", description: "Evaluate a basic arithmetic expression locally.", permission: "No access (local computation)", approval_required: false },
    ToolDefinition { id: "desktop.open_target", name: "Open desktop item", description: "Open one of FRIDAY's fixed, approved Windows destinations.", permission: "External application launch", approval_required: true },
];

#[tauri::command]
pub fn list_tools() -> Vec<ToolDefinition> { TOOLS.to_vec() }

#[tauri::command]
pub fn tool_history() -> Result<Vec<ToolEvent>, String> {
    let history = HISTORY.lock().map_err(|_| "Tool history is temporarily unavailable.".to_string())?;
    Ok(history.iter().rev().cloned().collect())
}

fn record(tool_id: &str, label: &str, outcome: &str, approved: bool) {
    if let (Ok(mut history), Ok(mut next)) = (HISTORY.lock(), NEXT_ID.lock()) {
        let id = *next;
        *next = next.saturating_add(1);
        history.push_back(ToolEvent {
            id, tool_id: tool_id.into(), label: label.into(),
            outcome: outcome.chars().take(240).collect(),
            timestamp: SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs(),
            approved,
        });
        while history.len() > 100 { history.pop_front(); }
    }
}

#[tauri::command]
pub fn execute_tool(tool_id: String, arguments: Value, approved: bool) -> Result<String, String> {
    let result = match tool_id.as_str() {
        "calculator.evaluate" => {
            let expression = arguments.get("expression").and_then(Value::as_str)
                .ok_or("Calculator requires an expression string.")?;
            if expression.len() > 256 { return Err("Calculator expression is too long.".into()); }
            evaluate(expression).map(|number| format_number(number))
        }
        "desktop.open_target" => {
            if !approved {
                record(&tool_id, "Open desktop item", "Blocked: user approval required", false);
                return Err("This action requires your explicit approval.".into());
            }
            let target = arguments.get("target").and_then(Value::as_str)
                .ok_or("A permitted desktop target is required.")?;
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
            Command::new(program).args(args).spawn()
                .map(|_| label.to_string())
                .map_err(|e| format!("Could not launch the approved destination: {e}"))?
        }
        _ => return Err("This tool is not registered in FRIDAY.".into()),
    };
    record(&tool_id, &tool_id, &result, approved);
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
}

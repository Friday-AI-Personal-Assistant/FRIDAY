use serde_json::{json, Value};
use std::{io::{Read, Write}, process::{Child, Command, Stdio}, sync::{Arc, Mutex}, time::{Duration, Instant}};
use tauri::{AppHandle, Manager, State};

#[derive(Default)]
pub struct AgentState {
    child: Arc<Mutex<Option<Child>>>,
}

fn run_worker(app: AppHandle, state: Arc<Mutex<Option<Child>>>, mut request: Value) -> Result<Value, String> {
    if !request.is_object() { return Err("Invalid request".into()); }
    let operation = request["operation"].as_str().ok_or("Missing operation")?.to_owned();
    if !["models", "capabilities", "chat", "tools", "execute", "speak", "listen", "diagnostics"].contains(&operation.as_str()) {
        return Err("Unknown operation".into());
    }
    let root = app.path().app_data_dir().map_err(|e| e.to_string())?.join("documents");
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    request["root"] = json!(root);
    let mut command = if cfg!(debug_assertions) {
        let mut cmd = Command::new(if cfg!(windows) { "python" } else { "python3" });
        cmd.arg(concat!(env!("CARGO_MANIFEST_DIR"), "/../worker/friday_worker.py"));
        cmd
    } else {
        let path = app.path().resource_dir().map_err(|e| e.to_string())?.join("worker/friday-worker.exe");
        if !path.exists() { return Err("Bundled local worker is missing. Rebuild the Windows installer.".into()); }
        Command::new(path)
    };
    #[cfg(windows)] {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    command.env("PYTHONUTF8", "1").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    let mut slot = state.lock().map_err(|_| "Worker lock unavailable")?;
    if slot.is_some() { return Err("A local operation is already running".into()); }
    let mut child = command.spawn().map_err(|e| format!("Cannot start local worker: {e}"))?;
    let stdout = child.stdout.take().ok_or("No worker output")?;
    let mut input = child.stdin.take().ok_or("No worker input")?;
    let mut bytes = serde_json::to_vec(&request).map_err(|e| e.to_string())?;
    bytes.push(b'\n');
    // Drain stdout concurrently so large accessibility trees cannot fill the pipe.
    let reader = std::thread::spawn(move || {
        let mut output = Vec::new();
        stdout.take(10 * 1024 * 1024 + 1).read_to_end(&mut output).map(|_| output)
    });
    *slot = Some(child);
    drop(slot);
    if let Err(e) = input.write_all(&bytes) {
        if let Ok(mut slot) = state.lock() { if let Some(child) = slot.as_mut() { let _ = child.kill(); } }
        return Err(e.to_string());
    }
    drop(input);
    let start = Instant::now();
    loop {
        let mut slot = state.lock().map_err(|_| "Worker lock unavailable")?;
        let child = slot.as_mut().ok_or("Operation cancelled")?;
        let finished = child.try_wait().map_err(|e| e.to_string())?.is_some();
        if finished { slot.take(); break; }
        if start.elapsed() > Duration::from_secs(150) {
            let _ = child.kill(); let _ = child.wait(); slot.take();
            return Err("Operation timed out after 150 seconds".into());
        }
        drop(slot);
        std::thread::sleep(Duration::from_millis(50));
    }
    let output = reader.join().map_err(|_| "Worker output reader failed")?.map_err(|e| e.to_string())?;
    if output.len() > 10 * 1024 * 1024 { return Err("Worker response exceeds limit".into()); }
    let value: Value = serde_json::from_slice(&output).map_err(|e| format!("Invalid worker response: {e}"))?;
    if value["ok"] != true { return Err(value["error"].as_str().unwrap_or("Local worker failed").to_owned()); }
    Ok(value["result"].clone())
}

#[tauri::command]
pub async fn local_operation(app: AppHandle, state: State<'_, AgentState>, request: Value) -> Result<Value, String> {
    let state = state.child.clone();
    tauri::async_runtime::spawn_blocking(move || run_worker(app, state, request)).await.map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn stop_operation(state: State<'_, AgentState>) -> Result<(), String> {
    let mut slot = state.child.lock().map_err(|_| "Worker lock unavailable")?;
    if let Some(child) = slot.as_mut() {
        child.kill().map_err(|e| e.to_string())?;
        let _ = child.wait();
    }
    // Keep the child slot until the polling owner observes its exit, preventing a new
    // request from racing with cancellation of the preceding request.
    Ok(())
}

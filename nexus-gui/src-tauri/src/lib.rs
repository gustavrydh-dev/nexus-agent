use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use reqwest::blocking::Client;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::env;
use std::io::{Read, Write};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex,
};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{Emitter, State};

const MODEL: &str = "gpt-5.6-luna";
const NEXUS_CLIENT_TOKEN: &str = env!("NEXUS_CLIENT_TOKEN");
const MAX_AGENT_ROUNDS: usize = 100;
const MAX_TERMINAL_OUTPUT: usize = 200_000;

static COMMAND_COUNTER: AtomicU64 = AtomicU64::new(1);

struct TerminalSession {
    id: String,
    name: Mutex<String>,
    writer: Mutex<Option<Box<dyn Write + Send>>>,
    master: Mutex<Option<Box<dyn portable_pty::MasterPty + Send>>>,
    alive: Arc<AtomicBool>,
    output: Arc<Mutex<String>>,
    created_at: u64,
    updated_at: AtomicU64,
}

struct SessionManager {
    sessions: Mutex<HashMap<String, Arc<TerminalSession>>>,
    active_id: Mutex<Option<String>>,
}

impl SessionManager {
    fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            active_id: Mutex::new(None),
        }
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn shell_command() -> CommandBuilder {
    #[cfg(target_os = "windows")]
    {
        let mut cmd = CommandBuilder::new("cmd.exe");
        cmd.arg("/Q");
        cmd
    }

    #[cfg(not(target_os = "windows"))]
    {
        #[cfg(target_os = "macos")]
        let shell = "/bin/zsh";

        #[cfg(target_os = "linux")]
        let shell = "/bin/bash";

        let mut cmd = CommandBuilder::new(shell);
        cmd.arg("-i");
        cmd
    }
}

fn new_session_id() -> String {
    let n = COMMAND_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("session-{}-{}", now_secs(), n)
}

fn start_session_pty(
    session: &Arc<TerminalSession>,
    app: &tauri::AppHandle,
) -> Result<(), String> {
    if session.alive.load(Ordering::SeqCst) {
        return Ok(());
    }

    let pty = native_pty_system();

    let pair = pty
        .openpty(PtySize {
            rows: 40,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("Could not open PTY: {e}"))?;

    let _child = pair
        .slave
        .spawn_command(shell_command())
        .map_err(|e| format!("Could not start shell: {e}"))?;

    let writer = pair
        .master
        .take_writer()
        .map_err(|e| format!("Could not create PTY writer: {e}"))?;

    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| format!("Could not create PTY reader: {e}"))?;

    *session
        .writer
        .lock()
        .map_err(|_| "Writer lock poisoned".to_string())? = Some(writer);

    *session
        .master
        .lock()
        .map_err(|_| "Master lock poisoned".to_string())? = Some(pair.master);

    session.alive.store(true, Ordering::SeqCst);
    session.updated_at.store(now_secs(), Ordering::SeqCst);

    let alive = Arc::clone(&session.alive);
    let output = Arc::clone(&session.output);
    let session_id = session.id.clone();
    let app = app.clone();

    thread::spawn(move || {
        let mut buffer = [0u8; 8192];

        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,

                Ok(n) => {
                    let text = String::from_utf8_lossy(&buffer[..n]).to_string();

                    if let Ok(mut stored) = output.lock() {
                        stored.push_str(&text);

                        if stored.len() > MAX_TERMINAL_OUTPUT {
                            let keep = stored.len() - MAX_TERMINAL_OUTPUT;
                            *stored = stored[keep..].to_string();
                        }
                    }

                    let _ = app.emit(
                        "terminal-output",
                        json!({
                            "session_id": session_id,
                            "text": text
                        }),
                    );
                }

                Err(_) => break,
            }
        }

        alive.store(false, Ordering::SeqCst);

        let _ = app.emit(
            "terminal-exited",
            json!({
                "session_id": session_id
            }),
        );
    });

    Ok(())
}

fn create_session_internal(
    manager: &SessionManager,
    app: &tauri::AppHandle,
    name: String,
) -> Result<Arc<TerminalSession>, String> {
    let id = new_session_id();

    let session = Arc::new(TerminalSession {
        id: id.clone(),

        name: Mutex::new(if name.trim().is_empty() {
            "New task".to_string()
        } else {
            name
        }),

        writer: Mutex::new(None),
        master: Mutex::new(None),

        alive: Arc::new(AtomicBool::new(false)),

        output: Arc::new(Mutex::new(String::new())),

        created_at: now_secs(),
        updated_at: AtomicU64::new(now_secs()),
    });

    start_session_pty(&session, app)?;

    manager
        .sessions
        .lock()
        .map_err(|_| "Sessions lock poisoned".to_string())?
        .insert(id.clone(), Arc::clone(&session));

    *manager
        .active_id
        .lock()
        .map_err(|_| "Active session lock poisoned".to_string())? =
        Some(id);

    Ok(session)
}

fn active_session(
    manager: &SessionManager,
    app: &tauri::AppHandle,
) -> Result<Arc<TerminalSession>, String> {
    let id = manager
        .active_id
        .lock()
        .map_err(|_| "Active session lock poisoned".to_string())?
        .clone();

    if let Some(id) = id {
        if let Some(session) = manager
            .sessions
            .lock()
            .map_err(|_| "Sessions lock poisoned".to_string())?
            .get(&id)
            .cloned()
        {
            if !session.alive.load(Ordering::SeqCst) {
                let _ = start_session_pty(&session, app);
            }

            return Ok(session);
        }
    }

    create_session_internal(manager, app, "New task".to_string())
}

fn write_terminal(
    session: &Arc<TerminalSession>,
    text: &str,
) -> Result<(), String> {
    let mut guard = session
        .writer
        .lock()
        .map_err(|_| "Writer lock poisoned".to_string())?;

    let writer = guard
        .as_mut()
        .ok_or_else(|| "Terminal is not running".to_string())?;

    writer
        .write_all(text.as_bytes())
        .map_err(|e| format!("PTY write error: {e}"))?;

    writer
        .flush()
        .map_err(|e| format!("PTY flush error: {e}"))?;

    session.updated_at.store(now_secs(), Ordering::SeqCst);

    Ok(())
}

fn snapshot(session: &Arc<TerminalSession>) -> String {
    session
        .output
        .lock()
        .map(|v| v.clone())
        .unwrap_or_default()
}

fn clean_terminal_output(text: &str) -> String {
    let mut result = String::with_capacity(text.len());

    let bytes = text.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == 0x1b {
            if i + 1 < bytes.len() && bytes[i + 1] == b'[' {
                i += 2;

                while i < bytes.len() {
                    let c = bytes[i] as char;
                    i += 1;

                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }

                continue;
            }

            if i + 1 < bytes.len() && bytes[i + 1] == b']' {
                i += 2;

                while i < bytes.len() {
                    if bytes[i] == 0x07 {
                        i += 1;
                        break;
                    }

                    if bytes[i] == 0x1b
                        && i + 1 < bytes.len()
                        && bytes[i + 1] == b'\\'
                    {
                        i += 2;
                        break;
                    }

                    i += 1;
                }

                continue;
            }

            i += 1;
            continue;
        }

        if bytes[i] == b'\r' {
            i += 1;

            if i < bytes.len() && bytes[i] == b'\n' {
                i += 1;
            }

            result.push('\n');
            continue;
        }

        if bytes[i] == 8 {
            if !result.is_empty() {
                result.pop();
            }

            i += 1;
            continue;
        }

        result.push(bytes[i] as char);
        i += 1;
    }

    result
}

fn run_terminal_for_ai(
    session: &Arc<TerminalSession>,
    app: &tauri::AppHandle,
    command: &str,
    input: &str,
    wait_ms: u64,
) -> Result<String, String> {
    if !session.alive.load(Ordering::SeqCst) {
        start_session_pty(session, app)?;
    }

    let marker_id = COMMAND_COUNTER.fetch_add(1, Ordering::SeqCst);

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();

    let marker = format!(
        "__NEXUS_FINISHED_{}_{}_{}__",
        std::process::id(),
        marker_id,
        timestamp
    );

    let before = snapshot(session);
    let before_len = before.len();

    let wrapped = format!(
        "\n{}; printf '\\n{}\\n'\n",
        command, marker
    );

    write_terminal(session, &wrapped)?;

    if !input.is_empty() {
        thread::sleep(Duration::from_millis(100));
        write_terminal(session, input)?;
    }

    let started = SystemTime::now();

    loop {
        thread::sleep(Duration::from_millis(50));

        let current = snapshot(session);

        let new_output = if current.len() >= before_len {
            current[before_len..].to_string()
        } else {
            current.clone()
        };

        if new_output.contains(&marker) {
            let cleaned = new_output
                .replace(&marker, "")
                .trim()
                .to_string();

            return Ok(clean_terminal_output(&cleaned));
        }

        if !session.alive.load(Ordering::SeqCst) {
            return Ok(
                "[NEXUS: TERMINAL SESSION ENDED. A NEW SESSION WILL BE AVAILABLE FOR THE NEXT STEP.]"
                    .to_string(),
            );
        }

        let elapsed = started.elapsed().unwrap_or_default();

        if elapsed > Duration::from_secs(600) {
            return Err("Terminal operation exceeded 10 minutes".to_string());
        }

        if elapsed > Duration::from_millis(wait_ms.max(1000)) {
            return Ok(format!(
                "[NEXUS: COMMAND TIMEOUT]\n{}",
                clean_terminal_output(&new_output)
            ));
        }
    }
}

#[tauri::command]
fn start_terminal(
    manager: State<'_, SessionManager>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let _ = active_session(&manager, &app)?;
    Ok(())
}

fn is_admin_command(command: &str) -> bool {
    let command = command.trim();

    command.starts_with("sudo ")
        || command.starts_with("sudo\t")
        || command == "sudo"
        || command.starts_with("su ")
        || command.starts_with("su\t")
        || command == "su"
        || command.starts_with("doas ")
        || command.starts_with("doas\t")
        || command == "doas"
}

#[tauri::command]
fn terminal_write(
    manager: State<'_, SessionManager>,
    app: tauri::AppHandle,
    text: String,
) -> Result<(), String> {
    let session = active_session(&manager, &app)?;
    write_terminal(&session, &text)
}

#[tauri::command]
fn terminal_ctrl_c(
    manager: State<'_, SessionManager>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let session = active_session(&manager, &app)?;
    write_terminal(&session, "\u{3}")
}

#[tauri::command]
fn terminal_resize(
    manager: State<'_, SessionManager>,
    app: tauri::AppHandle,
    rows: u16,
    cols: u16,
) -> Result<(), String> {
    let session = active_session(&manager, &app)?;

    let guard = session
        .master
        .lock()
        .map_err(|_| "Master lock poisoned".to_string())?;

    if let Some(master) = guard.as_ref() {
        master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("PTY resize error: {e}"))?;
    }

    Ok(())
}

#[tauri::command]
fn terminal_status(
    manager: State<'_, SessionManager>,
    app: tauri::AppHandle,
) -> bool {
    active_session(&manager, &app)
        .map(|s| s.alive.load(Ordering::SeqCst))
        .unwrap_or(false)
}

#[tauri::command]
fn create_terminal_session(
    manager: State<'_, SessionManager>,
    app: tauri::AppHandle,
    name: String,
) -> Result<String, String> {
    let session = create_session_internal(&manager, &app, name)?;

    let _ = app.emit(
        "session-switched",
        json!({
            "session_id": session.id,
            "output": snapshot(&session),
            "name": session.name.lock().map(|n| n.clone()).unwrap_or_default()
        }),
    );

    Ok(session.id.clone())
}

#[tauri::command]
fn list_terminal_sessions(
    manager: State<'_, SessionManager>,
) -> Result<Vec<Value>, String> {
    let active = manager
        .active_id
        .lock()
        .map_err(|_| "Active session lock poisoned".to_string())?
        .clone();

    let sessions = manager
        .sessions
        .lock()
        .map_err(|_| "Sessions lock poisoned".to_string())?;

    let mut list = Vec::new();

    for session in sessions.values() {
        list.push(json!({
            "id": session.id,
            "name": session.name.lock().map(|n| n.clone()).unwrap_or_else(|_| "Session".to_string()),
            "created_at": session.created_at,
            "updated_at": session.updated_at.load(Ordering::SeqCst),
            "active": active.as_deref() == Some(&session.id),
            "alive": session.alive.load(Ordering::SeqCst)
        }));
    }

    list.sort_by(|a, b| {
        b.get("updated_at")
            .and_then(Value::as_u64)
            .cmp(&a.get("updated_at").and_then(Value::as_u64))
    });

    Ok(list)
}

#[tauri::command]
fn switch_terminal_session(
    manager: State<'_, SessionManager>,
    app: tauri::AppHandle,
    session_id: String,
) -> Result<String, String> {
    let session = manager
        .sessions
        .lock()
        .map_err(|_| "Sessions lock poisoned".to_string())?
        .get(&session_id)
        .cloned()
        .ok_or_else(|| "Session not found".to_string())?;

    if !session.alive.load(Ordering::SeqCst) {
        start_session_pty(&session, &app)?;
    }

    *manager
        .active_id
        .lock()
        .map_err(|_| "Active session lock poisoned".to_string())? =
        Some(session_id.clone());

    let output = snapshot(&session);

    let name = session
        .name
        .lock()
        .map(|n| n.clone())
        .unwrap_or_else(|_| "Session".to_string());

    let _ = app.emit(
        "session-switched",
        json!({
            "session_id": session_id,
            "output": output,
            "name": name
        }),
    );

    Ok(output)
}

#[tauri::command]
fn rename_terminal_session(
    manager: State<'_, SessionManager>,
    session_id: String,
    name: String,
) -> Result<(), String> {
    let sessions = manager
        .sessions
        .lock()
        .map_err(|_| "Sessions lock poisoned".to_string())?;

    let session = sessions
        .get(&session_id)
        .ok_or_else(|| "Session not found".to_string())?;

    *session
        .name
        .lock()
        .map_err(|_| "Name lock poisoned".to_string())? =
        if name.trim().is_empty() {
            "New task".to_string()
        } else {
            name
        };

    session.updated_at.store(now_secs(), Ordering::SeqCst);

    Ok(())
}

#[tauri::command]
fn delete_terminal_session(
    manager: State<'_, SessionManager>,
    app: tauri::AppHandle,
    session_id: String,
) -> Result<(), String> {
    let was_active = manager
        .active_id
        .lock()
        .map_err(|_| "Active session lock poisoned".to_string())?
        .as_deref()
        == Some(&session_id);

    {
        let mut sessions = manager
            .sessions
            .lock()
            .map_err(|_| "Sessions lock poisoned".to_string())?;

        if sessions.len() <= 1 {
            return Err("You must keep at least one session.".to_string());
        }

        sessions
            .remove(&session_id)
            .ok_or_else(|| "Session not found".to_string())?;
    }

    if was_active {
        let next = manager
            .sessions
            .lock()
            .map_err(|_| "Sessions lock poisoned".to_string())?
            .values()
            .next()
            .cloned()
            .ok_or_else(|| "No sessions left".to_string())?;

        *manager
            .active_id
            .lock()
            .map_err(|_| "Active session lock poisoned".to_string())? =
            Some(next.id.clone());

        if !next.alive.load(Ordering::SeqCst) {
            start_session_pty(&next, &app)?;
        }

        let _ = app.emit(
            "session-switched",
            json!({
                "session_id": next.id,
                "output": snapshot(&next),
                "name": next.name.lock().map(|n| n.clone()).unwrap_or_default()
            }),
        );
    }

    Ok(())
}

fn call_nexus_backend(
    input: &Vec<Value>,
) -> Result<Value, String> {
    let client = Client::new();

    let body = json!({
        "model": MODEL,
        "input": input,
        "tools": [
            {
                "type": "function",
                "name": "terminal",
                "description": "Execute arbitrary commands in the user's active persistent terminal session. Use this to inspect files, create or modify files, run programs, install packages, use git, interact with command line programs, and verify results.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": {
                            "type": "string",
                            "description": "The shell command to execute."
                        },
                        "input": {
                            "type": "string",
                            "description": "Optional text to send to an interactive terminal program after starting the command."
                        },
                        "wait_ms": {
                            "type": "integer",
                            "description": "How long to wait for the command to finish before returning a timeout."
                        }
                    },
                    "required": ["command", "input", "wait_ms"],
                    "additionalProperties": false
                }
            }
        ]
    });

    let response = client
        .post("https://nexus-api.nexus-agent-api.workers.dev/v1/responses")
        .header("X-Nexus-Client", NEXUS_CLIENT_TOKEN)
        .json(&body)
        .send()
        .map_err(|e| format!("Nexus backend request failed: {e}"))?;

    let status = response.status();

    let text = response
        .text()
        .map_err(|e| format!("Could not read OpenAI response: {e}"))?;

    if !status.is_success() {
        return Err(format!(
            "OpenAI API error {}: {}",
            status, text
        ));
    }

    serde_json::from_str(&text)
        .map_err(|e| format!("Invalid OpenAI response: {e}"))
}

fn extract_output_text(response: &Value) -> String {
    if let Some(text) = response.get("output_text").and_then(Value::as_str) {
        return text.to_string();
    }

    let mut result = String::new();

    if let Some(items) = response.get("output").and_then(Value::as_array) {
        for item in items {
            if item.get("type").and_then(Value::as_str) == Some("message") {
                if let Some(content) =
                    item.get("content").and_then(Value::as_array)
                {
                    for part in content {
                        if let Some(text) =
                            part.get("text").and_then(Value::as_str)
                        {
                            result.push_str(text);
                        }
                    }
                }
            }
        }
    }

    result
}

#[tauri::command]
fn run_nexus_task(
    manager: State<'_, SessionManager>,
    app: tauri::AppHandle,
    task: String,
    admin_commands: bool,
    dangerous_confirmation: bool,
) -> Result<String, String> {
    let session = active_session(&manager, &app)?;

    let system_prompt = r#"
You are NEXUS, an autonomous computer and terminal agent.

ACT on the user's computer, not merely explain.

You have one universal tool called terminal operating on the user's active persistent terminal session.

Use any shell command or installed program.

For every task:

1. Understand the requested result.
2. Inspect when necessary.
3. Execute commands.
4. Diagnose and fix failures.
5. Verify the result.
6. Respond concisely.

You may:

- create files
- read files
- modify files
- delete files
- create folders
- run Python
- run Node
- run Rust
- use git
- install packages
- inspect the filesystem
- execute pipelines
- interact with programs that request input

Do not just give commands to the user.

Do not claim completion without verification.
"#;

    let mut input = vec![
        json!({
            "role": "system",
            "content": system_prompt
        }),
        json!({
            "role": "user",
            "content": task
        }),
    ];

    for _ in 0..MAX_AGENT_ROUNDS {
        let response = call_nexus_backend(&input)?;

        let output = response
            .get("output")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        input.extend(output.clone());

        let calls: Vec<Value> = output
            .into_iter()
            .filter(|x| {
                x.get("type").and_then(Value::as_str)
                    == Some("function_call")
            })
            .collect();

        if calls.is_empty() {
            let text = extract_output_text(&response);

            return Ok(if text.trim().is_empty() {
                "Nexus slutförde uppgiften.".to_string()
            } else {
                text
            });
        }

        for call in calls {
            let call_id = call
                .get("call_id")
                .and_then(Value::as_str)
                .ok_or_else(|| "Missing function call ID".to_string())?;

            let name = call
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("");

            if name != "terminal" {
                input.push(json!({
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": "Unknown tool"
                }));

                continue;
            }

            let args: Value = serde_json::from_str(
                call
                    .get("arguments")
                    .and_then(Value::as_str)
                    .unwrap_or("{}"),
            )
            .map_err(|e| format!("Invalid terminal arguments: {e}"))?;

            let command = args
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or("");
	    if !admin_commands && is_admin_command(command) {
    		input.push(json!({
        	    "type": "function_call_output",
        	    "call_id": call_id,
        	    "output": "BLOCKED: Administrator commands are disabled in Nexus Settings."
   		 }));

    		continue;
	    }
            let terminal_input = args
                .get("input")
                .and_then(Value::as_str)
                .unwrap_or("");

            let wait_ms = args
                .get("wait_ms")
                .and_then(Value::as_u64)
                .unwrap_or(30_000);

            let result = run_terminal_for_ai(
                &session,
                &app,
                command,
                terminal_input,
                wait_ms,
            )?;

            input.push(json!({
                "type": "function_call_output",
                "call_id": call_id,
                "output": result
            }));
        }
    }

    Err("Nexus reached the maximum number of agent steps.".to_string())
}

#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(SessionManager::new())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            greet,
            start_terminal,
            terminal_write,
            terminal_ctrl_c,
            terminal_resize,
            terminal_status,
            create_terminal_session,
            list_terminal_sessions,
            switch_terminal_session,
            rename_terminal_session,
            delete_terminal_session,
            run_nexus_task
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

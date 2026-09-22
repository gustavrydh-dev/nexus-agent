#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::io::Write;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

#[tauri::command]
fn run_nexus_task(task: String) -> Result<String, String> {
    if task.trim().is_empty() {
        return Err("Task cannot be empty".to_string());
    }

    let agent_path = std::env::var("NEXUS_AGENT_PATH")
        .unwrap_or_else(|_| {
            format!(
                "{}/nexus-agent/target/debug/nexus-agent",
                std::env::var("HOME").unwrap_or_default()
            )
        });

    let mut child = Command::new(&agent_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .envs(std::env::vars())
        .spawn()
        .map_err(|e| format!("Could not start Nexus agent: {}", e))?;

    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| "Could not access Nexus agent stdin".to_string())?;

        writeln!(stdin, "{}", task)
            .map_err(|e| format!("Could not send task to Nexus agent: {}", e))?;
    }

    thread::sleep(Duration::from_millis(100));

    let output = child
        .wait_with_output()
        .map_err(|e| format!("Nexus agent failed: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if output.status.success() {
        Ok(stdout.to_string())
    } else {
        Err(format!(
            "Nexus agent exited with an error.\n\n{}\n{}",
            stdout, stderr
        ))
    }
}

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![run_nexus_task])
        .run(tauri::generate_context!())
        .expect("error while running Nexus");
}

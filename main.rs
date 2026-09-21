use portable_pty::{
    native_pty_system,
    CommandBuilder,
    PtySize,
};

use reqwest::blocking::Client;
use serde_json::{json, Value};

use std::env;
use std::io::{self, Read, Write};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
    Mutex,
};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const MODEL: &str = "gpt-5.6-luna";

const MAX_AGENT_ROUNDS: usize = 100;
const MAX_TERMINAL_OUTPUT: usize = 200_000;

static COMMAND_COUNTER: AtomicU64 = AtomicU64::new(1);

struct Session {
    writer: Box<dyn Write + Send>,
    alive: Arc<AtomicBool>,
}

struct Terminal {
    session: Mutex<Option<Session>>,
    output: Arc<Mutex<String>>,
}

impl Terminal {
    fn new() -> Result<Self, String> {
        let terminal = Self {
            session: Mutex::new(None),
            output: Arc::new(Mutex::new(String::new())),
        };

        terminal.start_session()?;

        Ok(terminal)
    }

    fn start_session(&self) -> Result<(), String> {
        let pty_system = native_pty_system();

        let pair = pty_system
            .openpty(PtySize {
                rows: 60,
                cols: 200,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| {
                format!("Kunde inte skapa PTY: {}", e)
            })?;

        let shell = if cfg!(target_os = "windows") {
            "cmd.exe"
        } else if cfg!(target_os = "macos") {
            "/bin/zsh"
        } else {
            "/bin/bash"
        };

        let mut command = CommandBuilder::new(shell);

        if cfg!(target_os = "windows") {
            command.arg("/Q");
        } else {
            command.arg("-i");

            command.env(
                "TERM",
                "xterm-256color",
            );

            command.env(
                "NO_COLOR",
                "1",
            );

            command.env(
                "CLICOLOR",
                "0",
            );
        }

        let mut child = pair
            .slave
            .spawn_command(command)
            .map_err(|e| {
                format!(
                    "Kunde inte starta shell: {}",
                    e
                )
            })?;

        drop(pair.slave);

        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| {
                format!(
                    "Kunde inte skapa PTY-reader: {}",
                    e
                )
            })?;

        let alive = Arc::new(
            AtomicBool::new(true)
        );

        let alive_reader =
            Arc::clone(&alive);

        let output =
            Arc::clone(&self.output);

        thread::spawn(move || {
            let mut buffer =
                [0u8; 16_384];

            loop {
                match reader.read(
                    &mut buffer
                ) {
                    Ok(0) => {
                        alive_reader.store(
                            false,
                            Ordering::SeqCst,
                        );

                        break;
                    }

                    Ok(n) => {
                        let text =
                            String::from_utf8_lossy(
                                &buffer[..n]
                            );

                        if let Ok(mut out) =
                            output.lock()
                        {
                            out.push_str(&text);

                            if out.len()
                                > MAX_TERMINAL_OUTPUT
                            {
                                let keep_from =
                                    out.len()
                                        .saturating_sub(
                                            MAX_TERMINAL_OUTPUT
                                        );

                                *out =
                                    out[keep_from..]
                                        .to_string();
                            }
                        }
                    }

                    Err(_) => {
                        alive_reader.store(
                            false,
                            Ordering::SeqCst,
                        );

                        break;
                    }
                }
            }
        });

        thread::sleep(
            Duration::from_millis(150)
        );

        let writer = pair
            .master
            .take_writer()
            .map_err(|e| {
                format!(
                    "Kunde inte skapa PTY-writer: {}",
                    e
                )
            })?;

        let master =
            pair.master;

        let alive_child =
            Arc::clone(&alive);

        thread::spawn(move || {
            let _master = master;

            let _ = child.wait();

            alive_child.store(
                false,
                Ordering::SeqCst,
            );
        });

        let mut session =
            self.session
                .lock()
                .map_err(|e| e.to_string())?;

        *session = Some(Session {
            writer,
            alive,
        });

        Ok(())
    }

    fn restart_session(
        &self,
    ) -> Result<(), String> {
        {
            let mut session =
                self.session
                    .lock()
                    .map_err(|e| e.to_string())?;

            *session = None;
        }

        thread::sleep(
            Duration::from_millis(100)
        );

        self.start_session()
    }

    fn snapshot(&self) -> String {
        self.output
            .lock()
            .map(|x| x.clone())
            .unwrap_or_default()
    }

    fn write_once(
        &self,
        data: &str,
    ) -> Result<(), String> {
        let mut session =
            self.session
                .lock()
                .map_err(|e| e.to_string())?;

        let current =
            match session.as_mut() {
                Some(session) => session,

                None => {
                    return Err(
                        "Ingen aktiv terminalsession."
                            .to_string()
                    );
                }
            };

        if !current
            .alive
            .load(Ordering::SeqCst)
        {
            return Err(
                "Terminalsessionen är inte längre aktiv."
                    .to_string()
            );
        }

        current
            .writer
            .write_all(
                data.as_bytes()
            )
            .map_err(|e| e.to_string())?;

        current
            .writer
            .flush()
            .map_err(|e| e.to_string())?;

        Ok(())
    }

    fn write_with_recovery(
        &self,
        data: &str,
    ) -> Result<(), String> {
        match self.write_once(data) {
            Ok(()) => Ok(()),

            Err(first_error) => {
                eprintln!(
                    "[NEXUS] Terminalsessionen dog: {}",
                    first_error
                );

                self.restart_session()?;

                thread::sleep(
                    Duration::from_millis(150)
                );

                self.write_once(data)
                    .map_err(|second_error| {
                        format!(
                            "{}; återanslutning misslyckades: {}",
                            first_error,
                            second_error
                        )
                    })
            }
        }
    }

    fn run(
        &self,
        command: &str,
        input: &str,
        wait_ms: u64,
    ) -> String {
        let before =
            self.snapshot();

        let counter =
            COMMAND_COUNTER.fetch_add(
                1,
                Ordering::SeqCst,
            );

        let now =
            SystemTime::now()
                .duration_since(
                    UNIX_EPOCH
                )
                .unwrap_or_default()
                .as_nanos();

        let marker = format!(
            "__NEXUS_FINISHED_{}_{}_{}__",
            std::process::id(),
            counter,
            now
        );

        if !command
            .trim()
            .is_empty()
        {
            let wrapped =
                if cfg!(target_os = "windows") {
                    format!(
                        "\r\n{}\r\n\
                         set __NEXUS_CODE=%ERRORLEVEL%\r\n\
                         echo {}:%__NEXUS_CODE%\r\n",
                        command,
                        marker
                    )
                } else {
                    format!(
                        "\n{}\n\
                         __nexus_code=$?; \
                         printf '\\n{}:%s\\n' \
                         \"$__nexus_code\"\n",
                        command,
                        marker
                    )
                };

            if let Err(error) =
                self.write_with_recovery(
                    &wrapped
                )
            {
                return format!(
                    "TERMINAL WRITE ERROR: {}",
                    error
                );
            }
        }

        if !input.is_empty() {
            if let Err(error) =
                self.write_with_recovery(
                    input
                )
            {
                return format!(
                    "TERMINAL INPUT ERROR: {}",
                    error
                );
            }
        }

        let timeout =
            Duration::from_millis(
                wait_ms.clamp(
                    250,
                    600_000
                )
            );

        let started =
            std::time::Instant::now();

        loop {
            let current =
                self.snapshot();

            let new_output =
                extract_new_output(
                    &before,
                    &current
                );

            if new_output
                .contains(&marker)
            {
                return clean_terminal_output(
                    &new_output,
                    Some(&marker)
                );
            }

            let terminal_dead = {
                match self.session.lock() {
                    Ok(session) => {
                        match session.as_ref() {
                            Some(session) => {
                                !session
                                    .alive
                                    .load(Ordering::SeqCst)
                            }

                            None => true,
                        }
                    }

                    Err(_) => true,
                }
            };

            if terminal_dead {
                return format!(
                    "{}\n\n\
                     [NEXUS: TERMINAL SESSION \
                     ENDED BEFORE THE COMMAND \
                     COMPLETED. A NEW SESSION WILL \
                     BE AVAILABLE FOR THE NEXT STEP.]",
                    clean_terminal_output(
                        &new_output,
                        Some(&marker)
                    )
                );
            }

            if started.elapsed()
                >= timeout
            {
                return format!(
                    "{}\n\n\
                     [NEXUS: PROCESS STILL RUNNING, \
                     WAITING FOR INPUT, OR EXCEEDED \
                     WAIT TIME]",
                    clean_terminal_output(
                        &new_output,
                        Some(&marker)
                    )
                );
            }

            thread::sleep(
                Duration::from_millis(50)
            );
        }
    }
}

fn extract_new_output(
    before: &str,
    current: &str,
) -> String {
    if current.len()
        >= before.len()
        && current.starts_with(before)
    {
        current[
            before.len()..
        ]
        .to_string()
    } else {
        current.to_string()
    }
}

fn clean_terminal_output(
    text: &str,
    marker: Option<&str>,
) -> String {
    let mut result =
        text.to_string();

    if let Some(marker) =
        marker
    {
        if let Some(position) =
            result.find(marker)
        {
            result.truncate(
                position
                    + marker.len()
            );
        }
    }

    let bytes =
        result.as_bytes();

    let mut cleaned =
        String::with_capacity(
            result.len()
        );

    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == 0x1b {
            i += 1;

            if i < bytes.len()
                && bytes[i] == b'['
            {
                i += 1;

                while i < bytes.len() {
                    let c =
                        bytes[i];

                    if (0x40..=0x7e)
                        .contains(&c)
                    {
                        i += 1;
                        break;
                    }

                    i += 1;
                }

                continue;
            }

            continue;
        }

        if bytes[i] == 0x08 {
            if !cleaned.is_empty() {
                cleaned.pop();
            }

            i += 1;
            continue;
        }

        cleaned.push(
            bytes[i] as char
        );

        i += 1;
    }

    cleaned
        .replace(
            "\r\n",
            "\n"
        )
        .replace(
            '\r',
            "\n"
        )
        .replace(
            "\0",
            ""
        )
}

fn get_text(
    output: &[Value]
) -> Option<String> {
    let mut result =
        String::new();

    for item in output {
        if item["type"]
            != "message"
        {
            continue;
        }

        if let Some(content) =
            item["content"]
                .as_array()
        {
            for part in content {
                if let Some(text) =
                    part["text"]
                        .as_str()
                {
                    result
                        .push_str(text);
                }
            }
        }
    }

    if result
        .trim()
        .is_empty()
    {
        None
    } else {
        Some(result)
    }
}

fn terminal_tool_definition()
    -> Value
{
    json!({
        "type": "function",

        "name": "terminal",

        "description": r#"
You have access to the user's REAL persistent terminal.

This is your universal computer execution capability.

There is deliberately NO fixed whitelist of commands.

You may use ANY executable, shell builtin, script,
interpreter, compiler, package manager, system command,
network CLI, development tool, process, or program that is
actually available on the user's computer.

The terminal supports normal shell behavior including:

- commands
- pipelines
- redirects
- environment variables
- scripts
- interactive programs
- REPLs
- long-running processes
- background processes
- SSH
- Git
- package managers
- programming languages
- compilers
- build tools
- testing tools
- file operations
- system configuration
- networking
- databases
- archives
- arbitrary installed software

Do not restrict yourself to examples.

If the user asks you to perform something that can be done
through the terminal, ACTUALLY DO IT.

Do not merely explain commands.

The terminal session is persistent between calls.

If a process asks for input, use the input field.

If a process needs Ctrl-C, send \u0003.

If the terminal session dies, continue the task using the
new terminal session automatically.

Always inspect results.

If something fails, diagnose it, correct it and retry.

Never claim completion without verification.
"#,

        "strict": true,

        "parameters": {
            "type": "object",

            "properties": {
                "command": {
                    "type": "string",

                    "description":
                        "Any shell command or shell script. Empty if only sending input to the currently running process."
                },

                "input": {
                    "type": "string",

                    "description":
                        "Raw input for the currently running terminal process. May contain normal text, newlines, or control characters such as \\u0003."
                },

                "wait_ms": {
                    "type": "integer",

                    "description":
                        "Maximum time to wait. Normal commands around 30000. Builds/installations 120000 or more. Interactive commands usually 1000-5000."
                }
            },

            "required": [
                "command",
                "input",
                "wait_ms"
            ],

            "additionalProperties": false
        }
    })
}

fn system_prompt()
    -> &'static str
{
    r#"
You are Nexus.

You are an autonomous computer agent.

Your job is to ACTUALLY COMPLETE tasks on the user's computer.

You have a universal real terminal.

============================================================
THE FUNDAMENTAL RULE
============================================================

DO THE TASK.

Do not merely explain how the user could do it.

If the task can be performed using the terminal,
perform it yourself.

============================================================
NO ARTIFICIAL COMMAND LIMIT
============================================================

You are not restricted to a predefined list of commands.

If a program or command exists on the user's computer,
you may use it.

You can use:

shell builtins
executables
scripts
Python
Node
npm
pnpm
yarn
Rust
Cargo
Go
Java
Git
GitHub CLI
Docker
Homebrew
APT
SSH
SCP
curl
wget
rsync
databases
compilers
package managers
system tools
network tools
file tools
archive tools
development tools
testing tools
configuration tools
and arbitrary installed software.

Use pipes, redirects, variables, scripts and command chains
when appropriate.

============================================================
AUTONOMOUS EXECUTION
============================================================

For every user request:

1. Understand the objective.
2. Inspect the current state.
3. Decide what needs to be done.
4. Execute it.
5. Inspect the result.
6. Fix problems.
7. Continue.
8. Verify the final result.

Do not stop after the first successful command if the user's
actual objective requires more work.

============================================================
INTERACTIVE TERMINAL
============================================================

The terminal is persistent.

Programs can ask questions.

When they do:

1. Read the output.
2. Understand what input is required.
3. Send the input.
4. Continue.
5. Verify the result.

Ctrl-C:
\u0003

Ctrl-D:
\u0004

============================================================
TERMINAL FAILURE
============================================================

A terminal session may occasionally terminate.

If the terminal reports that its session died:

DO NOT GIVE UP.

Continue the user's task using the newly available terminal
session.

Re-check the state before continuing.

============================================================
ERROR RECOVERY
============================================================

When something fails:

Do not blindly repeat the exact same command.

Instead:

1. Read the error.
2. Identify the cause.
3. Inspect relevant state.
4. Change the approach.
5. Retry.
6. Verify.

============================================================
FILES AND PROJECTS
============================================================

Before modifying an existing project:

inspect it.

Understand its structure.

Make the required changes.

Then build, test or run it.

Fix compilation/runtime errors.

Verify the final result.

============================================================
LONG-RUNNING PROCESSES
============================================================

A command that does not immediately finish may be:

- a server
- a development process
- an interactive program
- a REPL
- a build
- a download
- another legitimate long-running task

Do not automatically assume failure.

Inspect its output and state.

If the user wants it running, leave it running.

============================================================
UNKNOWN REQUESTS
============================================================

The user can ask for tasks you were never explicitly
programmed to recognize.

That is normal.

Do not say:

"I don't have a tool for that."

First determine whether the task can be solved through
the terminal.

If it can, figure out how and do it.

============================================================
VERIFICATION
============================================================

Never say something is complete merely because a command
returned.

Verify the actual result.

Examples:

If you created a file:
check that it exists and contains the expected content.

If you built software:
run the relevant tests or executable.

If you installed something:
verify the installation.

If you started a server:
verify that it is actually running.

============================================================
CONTEXT
============================================================

Remember relevant conversation context.

Understand references such as:

"that file"
"the project"
"there"
"the folder"
"the server"
"the program"
"what we just created"

Use actual computer state to resolve ambiguity whenever
possible.

============================================================
COMPLETION
============================================================

Only report completion after the requested result has been
verified.

Be concise in the final response.
"#
}

fn run_ai(
    client: &Client,
    api_key: &str,
    terminal: &Terminal,
    history: &mut Vec<Value>,
    user_message: &str,
) -> String {
    history.push(json!({
        "role": "user",
        "content": user_message
    }));

    let tools = json!([
        terminal_tool_definition()
    ]);

    let mut input =
        vec![
            json!({
                "role": "system",
                "content": system_prompt()
            })
        ];

    input.extend(
        history.clone()
    );

    for _ in 0..MAX_AGENT_ROUNDS {
        let response =
            match client
                .post(
                    "https://api.openai.com/v1/responses"
                )
                .bearer_auth(api_key)
                .json(&json!({
                    "model": MODEL,
                    "input": input,
                    "tools": tools
                }))
                .send()
            {
                Ok(response) =>
                    response,

                Err(error) => {
                    return format!(
                        "API-fel: {}",
                        error
                    );
                }
            };

        let status =
            response.status();

        let body: Value =
            match response.json()
            {
                Ok(body) =>
                    body,

                Err(error) => {
                    return format!(
                        "Kunde inte läsa API-svaret: {}",
                        error
                    );
                }
            };

        if !status.is_success() {
            return format!(
                "OpenAI API-fel:\n{}",
                body
            );
        }

        let output =
            body["output"]
                .as_array()
                .cloned()
                .unwrap_or_default();

        input.extend(
            output.clone()
        );

        let mut tool_called =
            false;

        for item in &output {
            if item["type"]
                != "function_call"
            {
                continue;
            }

            if item["name"]
                != "terminal"
            {
                continue;
            }

            tool_called =
                true;

            let call_id =
                item["call_id"]
                    .as_str()
                    .unwrap_or("");

            let arguments_string =
                item["arguments"]
                    .as_str()
                    .unwrap_or("{}");

            let arguments: Value =
                serde_json::from_str(
                    arguments_string
                )
                .unwrap_or_else(
                    |_| json!({})
                );

            let command =
                arguments["command"]
                    .as_str()
                    .unwrap_or("");

            let terminal_input =
                arguments["input"]
                    .as_str()
                    .unwrap_or("");

            let wait_ms =
                arguments["wait_ms"]
                    .as_u64()
                    .unwrap_or(30_000);

            println!();
            println!(
                "╭─ NEXUS TERMINAL ─────────────────────────────"
            );

            if !command.is_empty() {
                println!(
                    "│ $ {}",
                    command
                );
            }

            if !terminal_input.is_empty() {
                println!(
                    "│ [interactive input]"
                );
            }

            let result =
                terminal.run(
                    command,
                    terminal_input,
                    wait_ms
                );

            println!("│");

            println!(
                "{}",
                indent_output(&result)
            );

            println!(
                "╰──────────────────────────────────────────────"
            );

            println!();

            input.push(
                json!({
                    "type":
                        "function_call_output",

                    "call_id":
                        call_id,

                    "output":
                        result
                })
            );
        }

        if !tool_called {
            if let Some(answer) =
                get_text(&output)
            {
                history.push(
                    json!({
                        "role":
                            "assistant",

                        "content":
                            answer
                    })
                );

                if history.len() > 40 {
                    let remove =
                        history.len()
                            - 40;

                    history.drain(
                        0..remove
                    );
                }

                return answer;
            }

            return "Klart."
                .to_string();
        }
    }

    let answer =
        "Nexus nådde maxgränsen för agentsteg. \
         Uppgiften kan behöva delas upp."
            .to_string();

    history.push(
        json!({
            "role":
                "assistant",

            "content":
                answer
        })
    );

    answer
}

fn indent_output(
    text: &str
) -> String {
    text.lines()
        .map(|line| {
            format!("│ {}", line)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn main() {
    let api_key =
        match env::var(
            "OPENAI_API_KEY"
        ) {
            Ok(key)
                if !key.trim().is_empty()
            => key,

            _ => {
                println!();
                println!(
                    "OPENAI_API_KEY saknas."
                );
                return;
            }
        };

    println!();
    println!(
        "╔══════════════════════════════════════════════╗"
    );
    println!(
        "║              NEXUS AI AGENT                 ║"
    );
    println!(
        "║       Autonomous Computer Terminal          ║"
    );
    println!(
        "╚══════════════════════════════════════════════╝"
    );
    println!();

    println!(
        "Startar persistent terminal..."
    );

    let terminal =
        match Terminal::new()
        {
            Ok(terminal) =>
                terminal,

            Err(error) => {
                println!(
                    "Terminalfel: {}",
                    error
                );
                return;
            }
        };

    println!(
        "Terminal ansluten."
    );

    println!(
        "Nexus är redo."
    );

    println!(
        "Terminal recovery: AKTIV"
    );

    println!(
        "Skriv 'exit' för att avsluta."
    );

    println!(
        "Skriv 'clear' för att rensa AI-minnet."
    );

    println!();

    let client =
        match Client::builder()
            .timeout(
                Duration::from_secs(900)
            )
            .build()
        {
            Ok(client) =>
                client,

            Err(error) => {
                println!(
                    "HTTP-klientfel: {}",
                    error
                );
                return;
            }
        };

    let mut history:
        Vec<Value> =
            Vec::new();

    loop {
        print!("nexus> ");

        if io::stdout()
            .flush()
            .is_err()
        {
            break;
        }

        let mut user_input =
            String::new();

        if io::stdin()
            .read_line(
                &mut user_input
            )
            .is_err()
        {
            break;
        }

        let user_input =
            user_input.trim();

        if user_input.is_empty() {
            continue;
        }

        if user_input
            .eq_ignore_ascii_case(
                "exit"
            )
        {
            println!();
            println!(
                "Nexus avslutas."
            );
            break;
        }

        if user_input
            .eq_ignore_ascii_case(
                "clear"
            )
        {
            history.clear();

            println!();
            println!(
                "AI-minnet är nollställt."
            );
            println!();

            continue;
        }

        let answer =
            run_ai(
                &client,
                &api_key,
                &terminal,
                &mut history,
                user_input
            );

        println!();
        println!(
            "Nexus: {}",
            answer
        );
        println!();
    }
}

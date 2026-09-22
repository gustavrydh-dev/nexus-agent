import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import "./styles.css";

type Session = {
  id: string;
  name: string;
  created_at: number;
  updated_at: number;
  active: boolean;
  alive: boolean;
};

const app = document.querySelector<HTMLDivElement>("#app")!;

let terminal: Terminal;
let fitAddon: FitAddon;

let nexusEnabled = false;
let currentSessionId = "";
let currentSessionName = "New task";
let currentView = "home";

let aiBusy = false;

let sessions: Session[] = [];

type NexusSettings = {
  startup: boolean;
  defaultNexus: boolean;
  dangerousConfirmation: boolean;
  adminCommands: boolean;
};

function getSettings(): NexusSettings {
  return {
    startup:
      localStorage.getItem("nexus-startup") === "true",

    defaultNexus:
      localStorage.getItem("nexus-default-mode") === "true",

    dangerousConfirmation:
      localStorage.getItem("nexus-danger-confirmation") !== "false",

    adminCommands:
      localStorage.getItem("nexus-admin-commands") === "true",
  };
}


type ModalResult = {
  confirmed: boolean;
  value?: string;
};

function showModal(
  title: string,
  message: string,
  options: {
    input?: boolean;
    defaultValue?: string;
    confirmText?: string;
    cancelText?: string;
    danger?: boolean;
  } = {},
): Promise<ModalResult> {
  return new Promise((resolve) => {
    const existing = document.querySelector("#nexus-modal");
    existing?.remove();

    const modal = document.createElement("div");
    modal.id = "nexus-modal";
    modal.innerHTML = `
      <div class="nexus-modal-backdrop">
        <div class="nexus-modal">
          <div class="nexus-modal-title">
            ${escapeHtml(title)}
          </div>

          <div class="nexus-modal-message">
            ${escapeHtml(message)}
          </div>

          ${
            options.input
              ? `
                <input
                  id="nexus-modal-input"
                  class="nexus-modal-input"
                  value="${escapeHtml(options.defaultValue || "")}"
                  autocomplete="off"
                />
              `
              : ""
          }

          <div class="nexus-modal-actions">
            <button id="nexus-modal-cancel" class="secondary-button">
              ${escapeHtml(options.cancelText || "Avbryt")}
            </button>

            <button
              id="nexus-modal-confirm"
              class="${options.danger ? "danger-button" : "primary-button"}"
            >
              ${escapeHtml(options.confirmText || "Fortsätt")}
            </button>
          </div>
        </div>
      </div>
    `;

    document.body.appendChild(modal);

    const input =
      modal.querySelector<HTMLInputElement>("#nexus-modal-input");

    if (input) {
      input.focus();
      input.select();

      input.addEventListener("keydown", (event) => {
        if (event.key === "Enter") {
          event.preventDefault();

          const value = input.value;

          modal.remove();

          resolve({
            confirmed: true,
            value,
          });
        }

        if (event.key === "Escape") {
          event.preventDefault();

          modal.remove();

          resolve({
            confirmed: false,
          });
        }
      });
    }

    modal
      .querySelector<HTMLButtonElement>("#nexus-modal-cancel")
      ?.addEventListener("click", () => {
        modal.remove();

        resolve({
          confirmed: false,
        });
      });

    modal
      .querySelector<HTMLButtonElement>("#nexus-modal-confirm")
      ?.addEventListener("click", () => {
        const value = input?.value;

        modal.remove();

        resolve({
          confirmed: true,
          value,
        });
      });
  });
}

function escapeHtml(value: string) {
  return value
   .replace(/&/g, "&amp;")
   .replace(/</g, "&lt;")
   .replace(/>/g, "&gt;")
   .replace(/"/g, "&quot;")
   .replace(/'/g, "&#039;");
}

function render() {
  app.innerHTML = `
    <div class="nexus-app">
      <aside class="sidebar">
        <div class="brand">
          <div class="brand-title">NEXUS</div>
          <div class="brand-subtitle">AI COMPUTER AGENT</div>
        </div>

        <button id="new-task" class="new-task">
          <span>＋</span>
          <span>New task</span>
        </button>

        <nav class="nav">
          <button class="nav-item ${currentView === "home" ? "active" : ""}" data-view="home">
            <span>⌂</span>
            <span>Home</span>
          </button>

          <button class="nav-item ${currentView === "history" ? "active" : ""}" data-view="history">
            <span>◷</span>
            <span>History</span>
          </button>

          <button class="nav-item ${currentView === "settings" ? "active" : ""}" data-view="settings">
            <span>⚙</span>
            <span>Settings</span>
          </button>
        </nav>

        <div class="sidebar-bottom">
          <div class="status-dot"></div>
          <span>System ready</span>
        </div>
      </aside>

      <main class="main">
        <header class="topbar">
          <div>
            <div class="top-title">NEXUS AI</div>
            <div class="top-subtitle">
              ${escapeHtml(currentSessionName)}
            </div>
          </div>

          <div class="top-actions">
            <div class="online">
              <span class="online-dot"></span>
              Online
            </div>

            <button id="nexus-toggle" class="nexus-toggle ${nexusEnabled ? "on" : ""}">
              <span class="toggle-dot"></span>
              NEXUS ${nexusEnabled ? "ON" : "OFF"}
            </button>
          </div>
        </header>

        <section class="content">
          ${renderView()}
        </section>
      </main>
    </div>
  `;

  attachEvents();
}

function renderView() {
  if (currentView === "history") {
    return renderHistory();
  }

  if (currentView === "settings") {
    return renderSettings();
  }

  return renderHome();
}

function renderHome() {
  return `
    <div class="home">
      <div class="hero">
        <div class="eyebrow">COMPUTER CONTROL</div>

        <h1>
          Your computer,<br />
          controlled by AI.
        </h1>

        <p>
          Give Nexus a task in natural language and let it inspect,
          execute and verify the work directly on your computer.
        </p>
      </div>

      <div class="terminal-card">
        <div class="terminal-header">
          <div class="terminal-title">
            <span class="terminal-status"></span>
            ${escapeHtml(currentSessionName)}
          </div>

          <div class="terminal-actions">
            <button id="clear-terminal">Clear</button>
            <button id="ctrl-c">Ctrl+C</button>
          </div>
        </div>

        <div id="terminal-container"></div>

        ${
          nexusEnabled
            ? `
              <div class="ai-input-wrapper">
                <div class="ai-input-label">
                  <span class="ai-input-dot"></span>
                  NEXUS AI
                </div>

                <textarea
                  id="nexus-input"
                  class="nexus-input"
                  placeholder="Describe what you want Nexus to do..."
                  rows="3"
                  autocomplete="off"
                  spellcheck="false"
                ></textarea>

                <div class="ai-input-footer">
                  <span>Press Enter to run · Shift + Enter for new line</span>
                  <button id="nexus-send" class="nexus-send">
                    Run task
                    <span>↵</span>
                  </button>
                </div>
              </div>
            `
            : `
              <div class="terminal-input-area">
                <div class="input-mode">TERMINAL</div>
                <div class="input-hint">
                  Type a terminal command...
                </div>
              </div>
            `
        }
      </div>
    </div>
  `;
}

function renderHistory() {
  return `
    <div class="page">
      <div class="page-header">
        <div>
          <div class="eyebrow">SESSIONS</div>
          <h1>History</h1>
          <p>Continue working in any previous terminal session.</p>
        </div>

        <button id="history-new" class="primary-button">
          ＋ New task
        </button>
      </div>

      <div class="session-list">
        ${
          sessions.length === 0
            ? `
              <div class="empty-state">
                <div class="empty-icon">◷</div>
                <h2>No sessions yet</h2>
                <p>Create a new task to start a terminal session.</p>
              </div>
            `
            : sessions
                .map(
                  (session) => `
                    <div class="session-card ${
                      session.active ? "selected" : ""
                    }" data-session-id="${escapeHtml(session.id)}">

                      <div class="session-main">
                        <div class="session-icon">⌘</div>

                        <div>
                          <div class="session-name">
                            ${escapeHtml(session.name)}
                          </div>

                          <div class="session-meta">
                            ${
                              session.active
                                ? "Active session"
                                : "Saved session"
                            }
                            · ${formatDate(session.updated_at)}
                          </div>
                        </div>
                      </div>

                      <div class="session-actions">
                        <button
                          class="session-open"
                          data-session-id="${escapeHtml(session.id)}"
                        >
                          Open
                        </button>

                        <button
                          class="session-rename"
                          data-session-id="${escapeHtml(session.id)}"
                        >
                          Rename
                        </button>

                        <button
                          class="session-delete danger"
                          data-session-id="${escapeHtml(session.id)}"
                        >
                          Delete
                        </button>
                      </div>
                    </div>
                  `,
                )
                .join("")
        }
      </div>
    </div>
  `;
}

function renderSettings() {  

  return `
    <div class="page">
      <div class="page-header">
        <div>
          <div class="eyebrow">CONFIGURATION</div>
          <h1>Settings</h1>
          <p>Configure how Nexus behaves on this computer.</p>
        </div>
      </div>

      <div class="settings-grid">

        <section class="settings-card">
          <div class="settings-card-title">General</div>

          <div class="setting-row">
            <div>
              <strong>Start Nexus automatically</strong>
              <span>Launch Nexus when you start your computer.</span>
            </div>

            <label class="switch">
	      <input type="checkbox" id="startup-toggle" ${
  	        getSettings().startup ? "checked" : ""
	      }>


              <span></span>
            </label>
          </div>

          <div class="setting-row">
            <div>
              <strong>Default Nexus mode</strong>
              <span>Start new sessions with AI control enabled.</span>
            </div>

            <label class="switch">
              <input type="checkbox" id="default-nexus-toggle" ${
                nexusEnabled ? "checked" : ""
              }>
              <span></span>
            </label>
          </div>
        </section>

        <section class="settings-card">
          <div class="settings-card-title">AI & Permissions</div>

          <div class="setting-row">
            <div>
              <strong>Ask before dangerous actions</strong>
              <span>
                Ask before deletion, system changes and administrator actions.
              </span>
            </div>

            <label class="switch">

	      <input type="checkbox" id="danger-toggle" ${
                getSettings().dangerousConfirmation ? "checked" : ""
              }>
              <span></span>
            </label>
          </div>

          <div class="setting-row">
            <div>
              <strong>Allow administrator commands</strong>
              <span>
                Allow Nexus to use commands requiring elevated privileges.
              </span>
            </div>

            <label class="switch">

	      <input type="checkbox" id="admin-toggle" ${
  		getSettings().adminCommands ? "checked" : ""
              }>
              <span></span>
            </label>
          </div>
        </section>

        <section class="settings-card">
          <div class="settings-card-title">Privacy</div>

          <div class="setting-row column">
            <div>
              <strong>Protected folders</strong>
              <span>
                Folders that Nexus should not access.
              </span>
            </div>

            <button id="protected-folders" class="secondary-button">
              Manage protected folders
            </button>
          </div>

          <div class="privacy-note">
            Nexus currently runs through the operating system terminal.
            Protected-folder enforcement will use OS-level permissions before
            this feature is considered a security boundary.
          </div>
        </section>

        <section class="settings-card">
          <div class="settings-card-title">Data</div>

          <div class="setting-row">
            <div>
              <strong>Delete all session history</strong>
              <span>Remove saved Nexus sessions from this installation.</span>
            </div>

            <button id="delete-history" class="danger-button">
              Delete history
            </button>
          </div>

          <div class="setting-row">
            <div>
              <strong>Clear current terminal</strong>
              <span>Clear the visible terminal output.</span>
            </div>

            <button id="clear-current" class="secondary-button">
              Clear
            </button>
          </div>
        </section>

        <section class="settings-card uninstall-card">
          <div class="settings-card-title">Uninstall</div>

          <div class="setting-row column">
            <div>
              <strong>Remove Nexus</strong>
              <span>
                Uninstalling Nexus removes the application and its local
                configuration. Your personal files are not deleted.
              </span>
            </div>

            <button id="uninstall" class="danger-button">
              Uninstall Nexus
            </button>
          </div>
        </section>

      </div>
    </div>
  `;
}

function formatDate(timestamp: number) {
  if (!timestamp) {
    return "Unknown date";
  }

  return new Date(timestamp * 1000).toLocaleString("sv-SE", {
    dateStyle: "short",
    timeStyle: "short",
  });
}

async function loadSessions() {
  try {
    sessions = await invoke<Session[]>("list_terminal_sessions");
  } catch (error) {
    console.error("Could not load sessions:", error);
    sessions = [];
  }
}

async function createNewTask() {
  const result = await showModal(
    "New task",
    "Ge den nya terminalsessionen ett namn.",
    {
      input: true,
      defaultValue: `Task ${sessions.length + 1}`,
      confirmText: "Create",
    },
  );

  if (!result.confirmed) {
    return;
  }

  const name =
    result.value?.trim() || `Task ${sessions.length + 1}`;

  try {
    const id = await invoke<string>("create_terminal_session", {
      name,
    });

    currentSessionId = id;
    currentSessionName = name;
    currentView = "home";

    await loadSessions();

    render();

    await startTerminal();
  } catch (error) {
    console.error("Could not create session:", error);

    await showModal(
      "Could not create session",
      String(error),
      {
        confirmText: "OK",
        cancelText: "",
      },
    );
  }
}

async function openSession(sessionId: string) {
  try {
    await invoke("switch_terminal_session", {
      sessionId,
    });

    currentSessionId = sessionId;

    const selected = sessions.find((s) => s.id === sessionId);

    if (selected) {
      currentSessionName = selected.name;
    }

    await loadSessions();

    currentView = "home";

    render();

    await startTerminal();
  } catch (error) {
    alert(`Kunde inte öppna sessionen:\n${error}`);
  }
}

async function renameSession(sessionId: string) {
  const session = sessions.find((s) => s.id === sessionId);

  if (!session) {
    return;
  }

  const result = await showModal(
    "Rename session",
    "Choose a new name for this terminal session.",
    {
      input: true,
      defaultValue: session.name,
      confirmText: "Rename",
    },
  );

  if (!result.confirmed) {
    return;
  }

  const name = result.value?.trim();

  if (!name) {
    return;
  }

  try {
    await invoke("rename_terminal_session", {
      sessionId,
      name,
    });

    if (sessionId === currentSessionId) {
      currentSessionName = name;
    }

    await loadSessions();

    render();

    if (currentView === "home") {
      await startTerminal();
    }
  } catch (error) {
    console.error("Could not rename session:", error);

    await showModal(
      "Could not rename session",
      String(error),
      {
        confirmText: "OK",
        cancelText: "",
      },
    );
  }
}
async function deleteSession(sessionId: string) {
  const session = sessions.find((s) => s.id === sessionId);

  if (!session) {
    return;
  }

  const result = await showModal(
    "Delete session",
    `Are you sure you want to delete "${session.name}"?`,
    {
      confirmText: "Delete",
      danger: true,
    },
  );

  if (!result.confirmed) {
    return;
  }

  try {
    await invoke("delete_terminal_session", {
      sessionId,
    });

    await loadSessions();

    const active = sessions.find((s) => s.active) || sessions[0];

    if (active) {
      currentSessionId = active.id;
      currentSessionName = active.name;
    }

    currentView = "history";

    render();
  } catch (error) {
    console.error("Could not delete session:", error);

    await showModal(
      "Could not delete session",
      String(error),
      {
        confirmText: "OK",
        cancelText: "",
      },
    );
  }
}
async function startTerminal() {
  if (currentView !== "home") {
    return;
  }

  const container =
    document.querySelector<HTMLDivElement>("#terminal-container");

  if (!container) {
    return;
  }

  container.innerHTML = "";

  terminal = new Terminal({
    cursorBlink: true,
    fontSize: 14,
    fontFamily:
      "SFMono-Regular, Menlo, Monaco, Consolas, 'Liberation Mono', monospace",
    theme: {
      background: "#0a0d12",
      foreground: "#e7ebf2",
      cursor: "#ffffff",
    },
    scrollback: 10000,
  });

  fitAddon = new FitAddon();

  terminal.loadAddon(fitAddon);
  terminal.open(container);

  fitAddon.fit();

  terminal.onData(async (data) => {
    if (nexusEnabled) {
      return;
    }

    try {
      await invoke("terminal_write", {
        text: data,
      });
    } catch (error) {
      console.error(error);
    }
  });

  terminal.attachCustomKeyEventHandler((event) => {
    if (
      event.type === "keydown" &&
      event.ctrlKey &&
      event.key.toLowerCase() === "c"
    ) {
      if (!nexusEnabled) {
        invoke("terminal_ctrl_c").catch(console.error);
      }

      return false;
    }

    return true;
  });

  try {
    await invoke("start_terminal");

    const output = await invoke<string>("switch_terminal_session", {
      sessionId: currentSessionId,
    });

    if (output) {
      terminal.write(output);
    }
  } catch (error) {
    console.error("Terminal startup error:", error);
  }

  try {
    await invoke("terminal_resize", {
      rows: terminal.rows,
      cols: terminal.cols,
    });
  } catch (error) {
    console.error("Resize error:", error);
  }

  window.addEventListener("resize", resizeTerminal);
}

async function resizeTerminal() {
  if (!terminal || !fitAddon) {
    return;
  }

  try {
    fitAddon.fit();

    await invoke("terminal_resize", {
      rows: terminal.rows,
      cols: terminal.cols,
    });
  } catch {
    // Ignore resize races.
  }
}

async function runNexus(task: string) {
  if (aiBusy) {
    return;
  }

  aiBusy = true;

  terminal.write(
    `\r\n\x1b[90m[NEXUS] ${task}\x1b[0m\r\n\r\n`,
  );

  try {

   const settings = getSettings();

   const result = await invoke<string>("run_nexus_task", {
     task,
     adminCommands: settings.adminCommands,
     dangerousConfirmation: settings.dangerousConfirmation,
   });
    terminal.write(
      `\x1b[36m[NEXUS] ${result}\x1b[0m\r\n`,
    );

    await loadSessions();
  } catch (error) {
    terminal.write(
      `\r\n\x1b[31m[NEXUS ERROR] ${String(error)}\x1b[0m\r\n`,
    );
  } finally {
    aiBusy = false;
  }
}

function attachEvents() {
  const nexusInput =
    document.querySelector<HTMLTextAreaElement>("#nexus-input");

  const nexusSend =
    document.querySelector<HTMLButtonElement>("#nexus-send");

  if (nexusInput && nexusSend) {
    const submitNexusTask = async () => {
      const task = nexusInput.value.trim();

      if (!task || aiBusy) {
        return;
      }

      nexusInput.value = "";
      await runNexus(task);
    };

    nexusSend.addEventListener("click", submitNexusTask);

    nexusInput.addEventListener("keydown", async (event) => {
      if (
        event.key === "Enter" &&
        !event.shiftKey
      ) {
        event.preventDefault();
        await submitNexusTask();
      }
    });
  }
  document.querySelectorAll<HTMLButtonElement>("[data-view]").forEach(
    (button) => {
      button.addEventListener("click", async () => {
        currentView = button.dataset.view || "home";

        await loadSessions();

        const active = sessions.find((s) => s.active);

        if (active) {
          currentSessionId = active.id;
          currentSessionName = active.name;
        }

        render();

        if (currentView === "home") {
          await startTerminal();
        }
      });
    },
  );

  document
    .querySelector<HTMLButtonElement>("#new-task")
    ?.addEventListener("click", createNewTask);

  document
    .querySelector<HTMLButtonElement>("#history-new")
    ?.addEventListener("click", createNewTask);

  document
    .querySelector<HTMLButtonElement>("#nexus-toggle")
    ?.addEventListener("click", () => {
      nexusEnabled = !nexusEnabled;
      render();

      if (currentView === "home") {
        startTerminal();
      }
    });

  document
    .querySelector<HTMLButtonElement>("#ctrl-c")
    ?.addEventListener("click", async () => {
      try {
        await invoke("terminal_ctrl_c");
      } catch (error) {
        console.error(error);
      }
    });

  document
    .querySelector<HTMLButtonElement>("#clear-terminal")
    ?.addEventListener("click", () => {
      terminal?.clear();
    });

  document
    .querySelectorAll<HTMLButtonElement>(".session-open")
    .forEach((button) => {
      button.addEventListener("click", () => {
        const id = button.dataset.sessionId;

        if (id) {
          openSession(id);
        }
      });
    });

  document
    .querySelectorAll<HTMLButtonElement>(".session-rename")
    .forEach((button) => {
      button.addEventListener("click", () => {
        const id = button.dataset.sessionId;

        if (id) {
          renameSession(id);
        }
      });
    });

  document
    .querySelectorAll<HTMLButtonElement>(".session-delete")
    .forEach((button) => {
      button.addEventListener("click", () => {
        const id = button.dataset.sessionId;

        if (id) {
          deleteSession(id);
        }
      });
    });

document
  .querySelector<HTMLInputElement>("#startup-toggle")
  ?.addEventListener("change", (event) => {
    const enabled = (event.target as HTMLInputElement).checked;

    localStorage.setItem(
      "nexus-startup",
      enabled ? "true" : "false",
    );

    console.log("Startup Nexus:", enabled);
  });

document
  .querySelector<HTMLInputElement>("#default-nexus-toggle")
  ?.addEventListener("change", (event) => {
    nexusEnabled = (event.target as HTMLInputElement).checked;

    localStorage.setItem(
      "nexus-default-mode",
      nexusEnabled ? "true" : "false",
    );
  });

document
  .querySelector<HTMLInputElement>("#danger-toggle")
  ?.addEventListener("change", (event) => {
    const enabled = (event.target as HTMLInputElement).checked;

    localStorage.setItem(
      "nexus-danger-confirmation",
      enabled ? "true" : "false",
    );
  });

 document
  .querySelector<HTMLInputElement>("#admin-toggle")
  ?.addEventListener("change", (event) => {
    const enabled = (event.target as HTMLInputElement).checked;

    localStorage.setItem(
      "nexus-admin-commands",
      enabled ? "true" : "false",
    );
  });
  document
    .querySelector<HTMLButtonElement>("#clear-current")
    ?.addEventListener("click", () => {
      terminal?.clear();
    });
document
  .querySelector<HTMLButtonElement>("#delete-history")
  ?.addEventListener("click", async () => {
    const result = await showModal(
      "Delete session history",
      "This will delete all removable terminal sessions.",
      {
        confirmText: "Delete all",
        danger: true,
      },
    );

    if (!result.confirmed) {
      return;
    }

    const removableSessions = [...sessions].filter(
      (session) => session.id !== currentSessionId,
    );

    for (const session of removableSessions) {
      try {
        await invoke("delete_terminal_session", {
          sessionId: session.id,
        });
      } catch (error) {
        console.error(
          "Could not delete session:",
          session.id,
          error,
        );
      }
    }

    await loadSessions();

    const active =
      sessions.find((session) => session.active) ||
      sessions[0];

    if (active) {
      currentSessionId = active.id;
      currentSessionName = active.name;
    }

    render();

    await showModal(
      "History cleared",
      "All removable sessions have been deleted.",
      {
        confirmText: "OK",
        cancelText: "",
      },
    );
  });

 document
  .querySelector<HTMLButtonElement>("#protected-folders")
  ?.addEventListener("click", async () => {
    await showModal(
      "Protected folders",
      "OS-level protected-folder permissions will be connected here. Nexus will not use a fake text-based security restriction.",
      {
        confirmText: "OK",
        cancelText: "",
      },
    );
  });

document
  .querySelector<HTMLButtonElement>("#uninstall")
  ?.addEventListener("click", async () => {
    const result = await showModal(
      "Uninstall Nexus",
      "The actual uninstall action will be connected to the operating system installer.",
      {
        confirmText: "OK",
        cancelText: "",
      },
    );

    if (result.confirmed) {
      console.log("Uninstall requested");
    }
  });

}
async function initialize() {

  const settings = getSettings();

  nexusEnabled = settings.defaultNexus;

  await loadSessions();
  }  await loadSessions();

  if (sessions.length === 0) {
    try {
      const id = await invoke<string>("create_terminal_session", {
        name: "New task",
      });

      currentSessionId = id;
    } catch (error) {
      console.error("Initial session error:", error);
    }

    await loadSessions();
  }

  const active = sessions.find((s) => s.active) || sessions[0];

  if (active) {
    currentSessionId = active.id;
    currentSessionName = active.name;
  }

  render();

  await startTerminal();

  listen<{ session_id: string; text: string }>(
    "terminal-output",
    (event) => {
      if (!terminal) {
        return;
      }

      if (event.payload.session_id === currentSessionId) {
        terminal.write(event.payload.text);
      }
    },
  );

  listen<{ session_id: string }>(
    "terminal-exited",
    async (event) => {
      if (event.payload.session_id === currentSessionId) {
        await loadSessions();
      }
    },
  );

  listen<{
    session_id: string;
    output: string;
    name: string;
  }>("session-switched", (event) => {
    currentSessionId = event.payload.session_id;
    currentSessionName = event.payload.name;

    if (terminal) {
      terminal.clear();

      if (event.payload.output) {
        terminal.write(event.payload.output);
      }
    }
  });

initialize().catch(console.error);

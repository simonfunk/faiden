//! Faiden native host.
//!
//! The command surface below is the whole authority boundary. Web content can
//! ask for a shell in a directory the user already bound; it can never choose
//! the program that runs, and terminal bytes never come back as commands.

pub mod agent;
pub mod clock;
pub mod environment;
pub mod error;
pub mod store;
pub mod terminal;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, RunEvent, State};

use agent::locate::{self, Lookup};
use agent::runtime::{
    AgentDiagnostics, AgentEvent, AgentEvents, AgentManager, AgentRunInfo, AgentSnapshot,
    AgentSpec, AgentTimeouts, AUTHORITY_DISCLOSURE,
};
use environment::DirectoryContext;
use error::AppError;
use store::{
    AgentMessage, AgentRun, HandoffDraft, NewSession, ReviewItem, Session, Store, Thread,
    UnseenReviewCount,
};
use terminal::{
    ExitInfo, TerminalEvent, TerminalEvents, TerminalInfo, TerminalLifecycle, TerminalManager,
    TerminalSnapshot, TerminalSpec,
};

/// Single channel for all terminal traffic. The payload carries `terminalId`,
/// `epoch` and `seq` so a view can reject anything that is not its own.
pub const TERMINAL_EVENT: &str = "faiden://terminal";
/// Single channel for all agent traffic, with the same epoch/seq discipline.
pub const AGENT_EVENT: &str = "faiden://agent";

pub struct AppState {
    pub store: Arc<Store>,
    pub terminals: TerminalManager,
    pub agents: AgentManager,
}

/// Resolves where a terminal for `session_id` must start.
///
/// This is the authority boundary for starting a shell. The renderer names a
/// session and nothing else: the directory comes from the thread that session
/// belongs to, so a shell can only ever start somewhere the user already bound.
/// Unknown sessions and sessions that have already ended are refused, which
/// makes an unowned terminal unrepresentable.
pub fn resolve_terminal_cwd(store: &Store, session_id: &str) -> Result<Option<String>, AppError> {
    let session = store.get_session(session_id)?;
    if session.ended_at.is_some() {
        return Err(AppError::conflict(
            "SESSION_ENDED",
            format!("session {session_id} has already ended; start a new session"),
        ));
    }
    Ok(store.get_thread(&session.thread_id)?.workdir)
}

/// Where an agent session will be started, and why there.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentCwd {
    pub path: PathBuf,
    /// Provenance, stated plainly. This is the directory the child is started
    /// in and the one `session/new` is told — never an observed tool cwd.
    pub label: String,
}

/// Resolves where an agent for `session_id` must start.
///
/// The authority boundary: the renderer names a session and nothing else. The
/// directory comes from the thread that session belongs to, so an agent can
/// only ever start somewhere the user already bound — or, said explicitly, in
/// the home directory. Unknown and already-ended sessions are refused.
pub fn resolve_agent_cwd(
    store: &Store,
    session_id: &str,
    home: Option<&Path>,
) -> Result<AgentCwd, AppError> {
    let session = store.get_session(session_id)?;
    if session.ended_at.is_some() {
        return Err(AppError::conflict(
            "SESSION_ENDED",
            format!("session {session_id} has already ended; start a new session"),
        ));
    }
    resolve_agent_cwd_for_thread(store, &session.thread_id, home)
}

/// The same resolution, before any session exists, so the UI can show where an
/// agent *would* start without creating anything.
pub fn resolve_agent_cwd_for_thread(
    store: &Store,
    thread_id: &str,
    home: Option<&Path>,
) -> Result<AgentCwd, AppError> {
    let thread = store.get_thread(thread_id)?;

    let (path, label) = match thread.workdir {
        Some(dir) => (
            PathBuf::from(dir.clone()),
            format!(
                "{dir} — the directory bound to this thread. The agent is started here; \
                 Faiden does not observe where its tools actually run."
            ),
        ),
        None => {
            let Some(home) = home else {
                return Err(AppError::validation(
                    "cwd",
                    "no directory is bound to this thread and no home directory is available; \
                     bind a directory before starting an agent",
                ));
            };
            (
                home.to_path_buf(),
                format!(
                    "{} — your home directory. No directory is bound to this thread, so the \
                     agent starts here. Bind a directory to give it a project.",
                    home.display()
                ),
            )
        }
    };

    if !path.is_dir() {
        return Err(AppError::validation(
            "cwd",
            format!("{} is not an existing directory", path.display()),
        ));
    }
    Ok(AgentCwd { path, label })
}

/// Whether an installed Hermes can be found, and under what authority it would
/// run. Reported honestly: Faiden never installs, configures or authenticates.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentAvailability {
    pub installed: bool,
    pub program: Option<String>,
    pub source: Option<String>,
    pub message: String,
    pub disclosure: String,
}

impl AgentAvailability {
    pub fn probe(lookup: &Lookup) -> AgentAvailability {
        match locate::locate_hermes(lookup) {
            Ok(install) => AgentAvailability {
                installed: true,
                program: Some(install.program.to_string_lossy().to_string()),
                source: Some(install.source.clone()),
                message: format!(
                    "Hermes found at {} ({}). Faiden starts it as `hermes acp`.",
                    install.program.display(),
                    install.source
                ),
                disclosure: AUTHORITY_DISCLOSURE.to_string(),
            },
            Err(e) => AgentAvailability {
                installed: false,
                program: None,
                source: None,
                message: e.message(),
                disclosure: AUTHORITY_DISCLOSURE.to_string(),
            },
        }
    }
}

/// One line per agent run, with the thread it belongs to, so the sidebar can
/// show real state rather than guessing from process existence.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentOverview {
    pub thread_id: String,
    pub session_id: String,
    pub run_id: String,
    pub status: agent::runtime::AgentStatus,
    /// A pending permission request is genuine, actionable status: the agent is
    /// blocked until a human answers.
    pub awaiting_permission: bool,
    pub ended_at: Option<i64>,
}

/// Attributes live runs to their threads. A run whose session no longer exists
/// is dropped rather than attached to the wrong thread.
pub fn agent_overviews(store: &Store, runs: Vec<AgentRunInfo>) -> Vec<AgentOverview> {
    runs.into_iter()
        .filter_map(|run| {
            let session = store.get_session(&run.session_id).ok()?;
            Some(AgentOverview {
                thread_id: session.thread_id,
                session_id: run.session_id,
                run_id: run.run_id,
                status: run.status,
                awaiting_permission: run.status == agent::runtime::AgentStatus::AwaitingPermission,
                ended_at: run.ended_at,
            })
        })
        .collect()
}

/// Persists terminal runs against the sessions that own them, so a terminal's
/// end survives the process that observed it.
pub struct StoreTerminalLifecycle {
    store: Arc<Store>,
}

impl StoreTerminalLifecycle {
    pub fn new(store: Arc<Store>) -> StoreTerminalLifecycle {
        StoreTerminalLifecycle { store }
    }
}

impl TerminalLifecycle for StoreTerminalLifecycle {
    fn started(&self, terminal_id: &str, session_id: &str) -> Result<(), AppError> {
        self.store.start_terminal_run(terminal_id, session_id)
    }

    fn ended(&self, terminal_id: &str, outcome: &str) {
        // Called from the waiter thread and from the quit path; the store keeps
        // whichever outcome was recorded first. A failure here must not stop a
        // terminal from being cleaned up (the child is already killed/reaped by
        // the time this runs), so it is not propagated — but it is not silently
        // swallowed either: the "durable first" claim would otherwise be false
        // with no trace of why.
        if let Err(e) = self.store.end_terminal_run(terminal_id, outcome) {
            eprintln!(
                "faiden: failed to durably record terminal {terminal_id} end ({outcome}): {e}"
            );
        }
    }
}

struct TauriSink {
    app: AppHandle,
}

impl TerminalEvents for TauriSink {
    fn emit(&self, event: TerminalEvent) {
        // A dropped window is not an error worth surfacing; the snapshot on
        // re-attach is what makes the view whole again.
        let _ = self.app.emit(TERMINAL_EVENT, event);
    }
}

struct TauriAgentSink {
    app: AppHandle,
}

impl AgentEvents for TauriAgentSink {
    fn emit(&self, event: AgentEvent) {
        let _ = self.app.emit(AGENT_EVENT, event);
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub version: String,
    pub platform: String,
    pub data_dir: String,
    /// Stated plainly in the UI: this build has no background service.
    pub lifecycle_note: String,
    pub retained_output_chars: usize,
}

// --- app --------------------------------------------------------------------

#[tauri::command]
fn app_info(app: AppHandle) -> Result<AppInfo, AppError> {
    let data_dir = app
        .path()
        .app_data_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "(unavailable)".to_string());
    Ok(AppInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        platform: std::env::consts::OS.to_string(),
        data_dir,
        lifecycle_note: "Terminals are owned by this application. Switching threads keeps them running; quitting Faiden ends every terminal. There is no background service in this build.".to_string(),
        retained_output_chars: terminal::RETAINED_OUTPUT_CHARS,
    })
}

// --- threads ----------------------------------------------------------------

#[tauri::command]
fn thread_list(state: State<'_, AppState>) -> Result<Vec<Thread>, AppError> {
    state.store.list_threads()
}

#[tauri::command]
fn thread_create(state: State<'_, AppState>, title: String) -> Result<Thread, AppError> {
    state.store.create_thread(&title)
}

#[tauri::command]
fn thread_rename(
    state: State<'_, AppState>,
    id: String,
    title: String,
) -> Result<Thread, AppError> {
    state.store.rename_thread(&id, &title)
}

#[tauri::command]
fn thread_set_notes(
    state: State<'_, AppState>,
    id: String,
    notes: String,
) -> Result<Thread, AppError> {
    state.store.set_thread_notes(&id, &notes)
}

/// Binding a directory is checked against the real filesystem first, so a
/// thread never displays a path that was never there.
#[tauri::command]
fn thread_set_workdir(
    state: State<'_, AppState>,
    id: String,
    path: Option<String>,
) -> Result<Thread, AppError> {
    if let Some(p) = path.as_deref() {
        let ctx = environment::inspect_directory(p)?;
        if !ctx.is_directory {
            return Err(AppError::validation(
                "workdir",
                format!("{p} is not an existing directory"),
            ));
        }
    }
    state.store.set_thread_workdir(&id, path.as_deref())
}

#[tauri::command]
fn thread_mark_seen(state: State<'_, AppState>, id: String) -> Result<Thread, AppError> {
    state.store.mark_thread_seen(&id)
}

#[tauri::command]
fn thread_mark_reviewed(state: State<'_, AppState>, id: String) -> Result<Thread, AppError> {
    state.store.mark_thread_reviewed(&id)
}

// --- sessions ---------------------------------------------------------------

#[tauri::command]
fn session_list(state: State<'_, AppState>, thread_id: String) -> Result<Vec<Session>, AppError> {
    state.store.list_sessions(&thread_id)
}

#[tauri::command]
fn session_create(
    state: State<'_, AppState>,
    thread_id: String,
    session: NewSession,
) -> Result<Session, AppError> {
    state.store.create_session(&thread_id, session)
}

#[tauri::command]
fn session_end(
    state: State<'_, AppState>,
    id: String,
    outcome: String,
) -> Result<Session, AppError> {
    state.store.end_session(&id, &outcome)
}

#[tauri::command]
fn handoff_draft(
    state: State<'_, AppState>,
    thread_id: String,
    predecessor_id: Option<String>,
) -> Result<HandoffDraft, AppError> {
    state
        .store
        .build_handoff_draft(&thread_id, predecessor_id.as_deref())
}

// --- review inbox -----------------------------------------------------------

#[tauri::command]
fn review_list(state: State<'_, AppState>, thread_id: String) -> Result<Vec<ReviewItem>, AppError> {
    state.store.list_review_items(&thread_id)
}

#[tauri::command]
fn review_add(
    state: State<'_, AppState>,
    thread_id: String,
    session_id: Option<String>,
    kind: String,
    summary: String,
) -> Result<ReviewItem, AppError> {
    state
        .store
        .add_review_item(&thread_id, session_id.as_deref(), &kind, &summary)
}

/// Results waiting to be looked at, per thread. This is what keeps a thread
/// actionable after an agent produces something new: the thread's own
/// seen/reviewed timestamps record what a human did and are never rewritten by
/// an agent, so the unread signal comes from the items themselves.
#[tauri::command]
fn review_unseen_counts(state: State<'_, AppState>) -> Result<Vec<UnseenReviewCount>, AppError> {
    state.store.unseen_review_counts()
}

#[tauri::command]
fn review_mark_reviewed(state: State<'_, AppState>, id: String) -> Result<ReviewItem, AppError> {
    state.store.mark_review_item_reviewed(&id)
}

// --- environment ------------------------------------------------------------

#[tauri::command]
fn environment_inspect(path: String) -> Result<DirectoryContext, AppError> {
    environment::inspect_directory(&path)
}

/// Native folder picker. Returns `None` when the user cancels.
#[tauri::command]
fn environment_pick_directory(app: AppHandle) -> Result<Option<String>, AppError> {
    use tauri_plugin_dialog::DialogExt;
    let picked = app.dialog().file().blocking_pick_folder();
    Ok(picked
        .and_then(|p| p.into_path().ok())
        .map(|p| p.to_string_lossy().to_string()))
}

// --- terminals --------------------------------------------------------------

/// Started only from an explicit user action in the UI.
///
/// The webview supplies a session id and a terminal size — nothing else. The
/// program is chosen natively (`command: None`) and the working directory is
/// derived from the session's thread binding, so web content can neither pick
/// what runs nor where it runs.
#[tauri::command]
fn terminal_start(
    state: State<'_, AppState>,
    session_id: String,
    cols: u16,
    rows: u16,
) -> Result<TerminalInfo, AppError> {
    let cwd = resolve_terminal_cwd(&state.store, &session_id)?;
    state.terminals.start(TerminalSpec {
        session_id,
        cwd,
        command: None,
        cols,
        rows,
    })
}

#[tauri::command]
fn terminal_write(
    state: State<'_, AppState>,
    terminal_id: String,
    data: String,
) -> Result<(), AppError> {
    state.terminals.write(&terminal_id, &data)
}

#[tauri::command]
fn terminal_resize(
    state: State<'_, AppState>,
    terminal_id: String,
    cols: u16,
    rows: u16,
) -> Result<(), AppError> {
    state.terminals.resize(&terminal_id, cols, rows)
}

#[tauri::command]
fn terminal_snapshot(
    state: State<'_, AppState>,
    terminal_id: String,
) -> Result<TerminalSnapshot, AppError> {
    state.terminals.snapshot(&terminal_id)
}

#[tauri::command]
fn terminal_list(state: State<'_, AppState>) -> Result<Vec<TerminalInfo>, AppError> {
    Ok(state.terminals.list())
}

#[tauri::command]
fn terminal_close(state: State<'_, AppState>, terminal_id: String) -> Result<ExitInfo, AppError> {
    state.terminals.close(&terminal_id)
}

// --- agents -----------------------------------------------------------------

/// Read-only probe. Says whether an installed Hermes was found and under whose
/// authority it would run; it never installs or configures anything.
#[tauri::command]
fn agent_availability() -> AgentAvailability {
    AgentAvailability::probe(&Lookup::from_env())
}

/// Started only from an explicit user action.
///
/// The webview supplies a session id. Everything else — which executable runs,
/// its argv, its directory and its environment — is decided here, so web
/// content can neither choose the program nor where it runs.
#[tauri::command]
fn agent_start(state: State<'_, AppState>, session_id: String) -> Result<AgentRunInfo, AppError> {
    let lookup = Lookup::from_env();
    let install = locate::locate_hermes(&lookup)?;
    let cwd = resolve_agent_cwd(&state.store, &session_id, lookup.home.as_deref())?;
    state.agents.start(AgentSpec {
        session_id,
        program: install.program,
        // Fixed argv. Nothing from the UI reaches it, and there is no `--yolo`,
        // no `--accept-hooks` and no auto-approval flag anywhere in it.
        args: vec!["acp".to_string()],
        cwd: cwd.path,
        env: Vec::new(),
        source: install.source,
        timeouts: AgentTimeouts::default(),
    })
}

/// Where an agent for this session would start, and why. Read-only, so the UI
/// can show the directory and its provenance before anything is spawned.
#[tauri::command]
fn agent_planned_cwd(state: State<'_, AppState>, thread_id: String) -> Result<AgentCwd, AppError> {
    resolve_agent_cwd_for_thread(&state.store, &thread_id, Lookup::from_env().home.as_deref())
}

#[tauri::command]
fn agent_list(state: State<'_, AppState>) -> Result<Vec<AgentRunInfo>, AppError> {
    Ok(state.agents.list())
}

#[tauri::command]
fn agent_overview(state: State<'_, AppState>) -> Result<Vec<AgentOverview>, AppError> {
    Ok(agent_overviews(&state.store, state.agents.list()))
}

#[tauri::command]
fn agent_snapshot(state: State<'_, AppState>, run_id: String) -> Result<AgentSnapshot, AppError> {
    state.agents.snapshot(&run_id)
}

/// Sends one message. Nothing is ever sent without this call, which exists only
/// because the user pressed Submit.
#[tauri::command]
fn agent_prompt(
    state: State<'_, AppState>,
    run_id: String,
    text: String,
) -> Result<AgentMessage, AppError> {
    state.agents.prompt(&run_id, &text)
}

/// The user's answer to the pending permission request. `option_id` of `None`
/// means "not allowed", which is what every other path also falls back to.
#[tauri::command]
fn agent_answer_permission(
    state: State<'_, AppState>,
    run_id: String,
    request_id: String,
    option_id: Option<String>,
) -> Result<(), AppError> {
    state
        .agents
        .answer_permission(&run_id, &request_id, option_id.as_deref())
}

#[tauri::command]
fn agent_cancel(state: State<'_, AppState>, run_id: String) -> Result<AgentRunInfo, AppError> {
    state.agents.cancel(&run_id)
}

#[tauri::command]
fn agent_stop(state: State<'_, AppState>, run_id: String) -> Result<AgentRunInfo, AppError> {
    state.agents.stop(&run_id)
}

#[tauri::command]
fn agent_diagnostics(
    state: State<'_, AppState>,
    run_id: String,
) -> Result<AgentDiagnostics, AppError> {
    state.agents.diagnostics(&run_id)
}

/// Durable history for a session, including runs whose process is long gone.
/// These are records, never reattached sessions.
#[tauri::command]
fn agent_history(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<Vec<AgentRun>, AppError> {
    state.store.list_agent_runs(&session_id)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentTranscript {
    pub messages: Vec<AgentMessage>,
    /// True when older entries exist that this reply does not include.
    pub truncated: bool,
}

#[tauri::command]
fn agent_transcript(
    state: State<'_, AppState>,
    run_id: String,
) -> Result<AgentTranscript, AppError> {
    let (messages, truncated) = state
        .store
        .list_agent_messages(&run_id, agent::runtime::MAX_TRANSCRIPT_ENTRIES)?;
    Ok(AgentTranscript {
        messages,
        truncated,
    })
}

pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            let store = Arc::new(Store::open(&data_dir.join("faiden.sqlite3"))?);
            // Terminals do not survive this application's exit, so any run
            // still marked open belongs to a process that is gone. Settle it as
            // unknown before anything can present it as live.
            store.reconcile_interrupted_runs()?;
            // Same reasoning for agents: an open agent run at startup belongs to
            // a child that is gone. It is settled as disconnected, and nothing
            // is reattached or replayed.
            store.reconcile_interrupted_agent_runs()?;
            let sink = Arc::new(TauriSink {
                app: app.handle().clone(),
            });
            let agent_sink = Arc::new(TauriAgentSink {
                app: app.handle().clone(),
            });
            let lifecycle = Arc::new(StoreTerminalLifecycle::new(store.clone()));
            app.manage(AppState {
                terminals: TerminalManager::with_lifecycle(sink, lifecycle),
                agents: AgentManager::new(agent_sink, store.clone()),
                store,
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            app_info,
            thread_list,
            thread_create,
            thread_rename,
            thread_set_notes,
            thread_set_workdir,
            thread_mark_seen,
            thread_mark_reviewed,
            session_list,
            session_create,
            session_end,
            handoff_draft,
            review_list,
            review_add,
            review_mark_reviewed,
            review_unseen_counts,
            environment_inspect,
            environment_pick_directory,
            terminal_start,
            terminal_write,
            terminal_resize,
            terminal_snapshot,
            terminal_list,
            terminal_close,
            agent_availability,
            agent_planned_cwd,
            agent_start,
            agent_list,
            agent_overview,
            agent_snapshot,
            agent_prompt,
            agent_answer_permission,
            agent_cancel,
            agent_stop,
            agent_diagnostics,
            agent_history,
            agent_transcript,
        ])
        .build(tauri::generate_context!())
        .expect("failed to build Faiden");

    app.run(|handle, event| {
        // There is no background service in this build, so quitting really does
        // end every terminal. Do it deliberately rather than orphaning children.
        if matches!(event, RunEvent::ExitRequested { .. } | RunEvent::Exit) {
            if let Some(state) = handle.try_state::<AppState>() {
                state.terminals.shutdown_all();
                // Agents are owned children too. Quitting ends them; it is
                // disclosed in the UI rather than hidden behind a promise of
                // background work.
                state.agents.shutdown_all();
            }
        }
    });
}

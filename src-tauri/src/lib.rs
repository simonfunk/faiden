//! Faiden native host.
//!
//! The command surface below is the whole authority boundary. Web content can
//! ask for a shell in a directory the user already bound; it can never choose
//! the program that runs, and terminal bytes never come back as commands.

pub mod clock;
pub mod environment;
pub mod error;
pub mod store;
pub mod terminal;

use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, RunEvent, State};

use environment::DirectoryContext;
use error::AppError;
use store::{HandoffDraft, NewSession, ReviewItem, Session, Store, Thread};
use terminal::{
    ExitInfo, TerminalEvent, TerminalEvents, TerminalInfo, TerminalLifecycle, TerminalManager,
    TerminalSnapshot, TerminalSpec,
};

/// Single channel for all terminal traffic. The payload carries `terminalId`,
/// `epoch` and `seq` so a view can reject anything that is not its own.
pub const TERMINAL_EVENT: &str = "faiden://terminal";

pub struct AppState {
    pub store: Arc<Store>,
    pub terminals: TerminalManager,
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
            let sink = Arc::new(TauriSink {
                app: app.handle().clone(),
            });
            let lifecycle = Arc::new(StoreTerminalLifecycle::new(store.clone()));
            app.manage(AppState {
                terminals: TerminalManager::with_lifecycle(sink, lifecycle),
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
            environment_inspect,
            environment_pick_directory,
            terminal_start,
            terminal_write,
            terminal_resize,
            terminal_snapshot,
            terminal_list,
            terminal_close,
        ])
        .build(tauri::generate_context!())
        .expect("failed to build Faiden");

    app.run(|handle, event| {
        // There is no background service in this build, so quitting really does
        // end every terminal. Do it deliberately rather than orphaning children.
        if matches!(event, RunEvent::ExitRequested { .. } | RunEvent::Exit) {
            if let Some(state) = handle.try_state::<AppState>() {
                state.terminals.shutdown_all();
            }
        }
    });
}

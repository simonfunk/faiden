//! The authority boundary around starting a terminal, and the durable session
//! lifecycle that a terminal's end must leave behind.
//!
//! These exercise the composition the Tauri command layer uses — a real
//! `Store` over a temporary SQLite file wired to a real `TerminalManager` — so
//! they cover the production path rather than a parallel arrangement.

mod support;

use std::sync::Arc;
use std::time::Duration;

use faiden_lib::error::AppError;
use faiden_lib::store::{NewSession, Session, Store};
use faiden_lib::terminal::{TerminalManager, TerminalSpec};
use faiden_lib::{resolve_terminal_cwd, StoreTerminalLifecycle};
use support::{has_exit, process_gone, text_of, wait_until, CollectingSink};
use tempfile::TempDir;

const T: Duration = Duration::from_secs(20);

struct Fixture {
    _tmp: TempDir,
    root: std::path::PathBuf,
    db: std::path::PathBuf,
    store: Arc<Store>,
}

fn fixture() -> Fixture {
    let tmp = TempDir::new().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let db = root.join("faiden.sqlite3");
    let store = Arc::new(Store::open(&db).unwrap());
    Fixture {
        _tmp: tmp,
        root,
        db,
        store,
    }
}

fn manager(store: &Arc<Store>) -> (Arc<CollectingSink>, TerminalManager) {
    let sink = CollectingSink::new();
    let mgr = TerminalManager::with_lifecycle(
        sink.clone(),
        Arc::new(StoreTerminalLifecycle::new(store.clone())),
    );
    (sink, mgr)
}

fn shell_session(store: &Store, thread_id: &str) -> Session {
    store
        .create_session(
            thread_id,
            NewSession {
                label: "Shell session".into(),
                kind: "shell".into(),
                predecessor_id: None,
                briefing: String::new(),
            },
        )
        .unwrap()
}

// --- 1. Authority: the renderer names a session, never a directory ----------

#[test]
fn a_forged_session_id_can_never_start_a_terminal() {
    let f = fixture();
    match resolve_terminal_cwd(&f.store, "ses_invented_by_the_webview") {
        Err(AppError::NotFound { entity, .. }) => assert_eq!(entity, "session"),
        other => panic!("an unknown session must fail closed, got {other:?}"),
    }
}

#[test]
fn a_session_that_has_already_ended_cannot_start_another_terminal() {
    let f = fixture();
    let thread = f.store.create_thread("Ended").unwrap();
    let s = shell_session(&f.store, &thread.id);
    f.store
        .end_session(&s.id, "terminal process exited with code 0")
        .unwrap();

    match resolve_terminal_cwd(&f.store, &s.id) {
        Err(AppError::Conflict { code, .. }) => assert_eq!(code, "SESSION_ENDED"),
        other => panic!("an ended session must not accept a new terminal, got {other:?}"),
    }
}

#[test]
fn the_working_directory_comes_from_the_thread_binding_not_from_the_caller() {
    let f = fixture();
    let bound = f.root.join("bound");
    let elsewhere = f.root.join("elsewhere");
    std::fs::create_dir(&bound).unwrap();
    std::fs::create_dir(&elsewhere).unwrap();

    let thread = f.store.create_thread("Bound").unwrap();
    let s = shell_session(&f.store, &thread.id);

    // An unbound thread yields no directory at all, rather than a guess.
    assert_eq!(resolve_terminal_cwd(&f.store, &s.id).unwrap(), None);

    f.store
        .set_thread_workdir(&thread.id, Some(bound.to_str().unwrap()))
        .unwrap();
    let resolved = resolve_terminal_cwd(&f.store, &s.id).unwrap();
    assert_eq!(
        resolved.as_deref(),
        bound.to_str(),
        "the directory is derived from the thread's recorded binding"
    );
    assert_ne!(
        resolved.as_deref(),
        elsewhere.to_str(),
        "no other existing directory can be substituted for it"
    );

    // And the shell really does start there.
    let (sink, mgr) = manager(&f.store);
    let info = mgr
        .start(TerminalSpec {
            session_id: s.id.clone(),
            cwd: resolved,
            command: Some(vec!["/bin/sh".into(), "-c".into(), "pwd -P".into()]),
            cols: 80,
            rows: 24,
        })
        .unwrap();
    let id = info.terminal_id.clone();
    let events = sink.wait_for(T, |e| has_exit(e, &id));
    assert!(text_of(&events, &id).contains(bound.to_str().unwrap()));
}

#[test]
fn a_binding_that_no_longer_exists_is_rejected_before_anything_spawns() {
    let f = fixture();
    let dir = f.root.join("gone-later");
    std::fs::create_dir(&dir).unwrap();
    let thread = f.store.create_thread("Stale").unwrap();
    f.store
        .set_thread_workdir(&thread.id, Some(dir.to_str().unwrap()))
        .unwrap();
    let s = shell_session(&f.store, &thread.id);
    std::fs::remove_dir(&dir).unwrap();

    let resolved = resolve_terminal_cwd(&f.store, &s.id).unwrap();
    let (_sink, mgr) = manager(&f.store);
    match mgr.start(TerminalSpec {
        session_id: s.id.clone(),
        cwd: resolved,
        command: None,
        cols: 80,
        rows: 24,
    }) {
        Err(AppError::Validation { field, .. }) => assert_eq!(field, "cwd"),
        other => panic!("a vanished binding must be rejected, got {other:?}"),
    }
    assert!(mgr.list().is_empty(), "nothing was spawned");
}

// --- 2. A terminal's end is durable ----------------------------------------

#[test]
fn a_natural_exit_ends_the_session_with_a_factual_outcome() {
    let f = fixture();
    let thread = f.store.create_thread("Natural exit").unwrap();
    let s = shell_session(&f.store, &thread.id);
    let (sink, mgr) = manager(&f.store);

    let info = mgr
        .start(TerminalSpec {
            session_id: s.id.clone(),
            cwd: None,
            command: Some(vec!["/bin/sh".into(), "-c".into(), "exit 3".into()]),
            cols: 80,
            rows: 24,
        })
        .unwrap();
    let id = info.terminal_id.clone();
    sink.wait_for(T, |e| has_exit(e, &id));

    // The exit is durable by the time anything can observe it.
    let ended = wait_until(Duration::from_secs(5), || {
        f.store.get_session(&s.id).unwrap().ended_at.is_some()
    });
    assert!(ended, "a terminal exiting ends the session it belonged to");

    let outcome = f.store.get_session(&s.id).unwrap().outcome.unwrap();
    assert!(
        outcome.contains("code 3"),
        "the recorded outcome states the real exit code, got: {outcome}"
    );
    assert!(
        !outcome.to_lowercase().contains("success"),
        "a lifecycle record never claims the work succeeded, got: {outcome}"
    );
}

#[test]
fn closing_a_terminal_ends_its_session_before_close_returns() {
    let f = fixture();
    let thread = f.store.create_thread("Close").unwrap();
    let s = shell_session(&f.store, &thread.id);
    let (_sink, mgr) = manager(&f.store);

    let info = mgr
        .start(TerminalSpec {
            session_id: s.id.clone(),
            cwd: None,
            command: Some(vec!["/bin/sh".into(), "-c".into(), "sleep 300".into()]),
            cols: 80,
            rows: 24,
        })
        .unwrap();
    assert_eq!(f.store.get_session(&s.id).unwrap().ended_at, None);

    mgr.close(&info.terminal_id).expect("close");
    let after = f.store.get_session(&s.id).unwrap();
    assert!(
        after.ended_at.is_some(),
        "close does not return before the end is recorded"
    );
    assert!(after.outcome.is_some());
}

#[test]
fn quitting_records_why_a_live_terminal_ended_and_leaves_a_real_exit_alone() {
    let f = fixture();
    let thread = f.store.create_thread("Quit").unwrap();
    let live = shell_session(&f.store, &thread.id);
    let finished = shell_session(&f.store, &thread.id);
    let (sink, mgr) = manager(&f.store);

    // One terminal that exits on its own first.
    let done = mgr
        .start(TerminalSpec {
            session_id: finished.id.clone(),
            cwd: None,
            command: Some(vec!["/bin/sh".into(), "-c".into(), "exit 0".into()]),
            cols: 80,
            rows: 24,
        })
        .unwrap();
    let done_id = done.terminal_id.clone();
    sink.wait_for(T, |e| has_exit(e, &done_id));

    // And one that is still running when the application quits.
    let running = mgr
        .start(TerminalSpec {
            session_id: live.id.clone(),
            cwd: None,
            command: Some(vec!["/bin/sh".into(), "-c".into(), "sleep 300".into()]),
            cols: 80,
            rows: 24,
        })
        .unwrap();
    let pid = running.pid.unwrap();

    mgr.shutdown_all();
    assert!(wait_until(Duration::from_secs(10), || process_gone(pid)));

    let live_outcome = f.store.get_session(&live.id).unwrap().outcome.unwrap();
    assert!(
        live_outcome.to_lowercase().contains("quit"),
        "the record says the terminal ended because Faiden quit, not that it \
         merely died: {live_outcome}"
    );

    let finished_outcome = f.store.get_session(&finished.id).unwrap().outcome.unwrap();
    assert!(
        finished_outcome.contains("code 0"),
        "a terminal that had already exited keeps its real exit code across the \
         quit path, got: {finished_outcome}"
    );
    assert!(
        !finished_outcome.to_lowercase().contains("quit"),
        "quitting must not rewrite an outcome that was already observed, got: \
         {finished_outcome}"
    );
}

#[test]
fn a_session_interrupted_by_an_abnormal_exit_is_unknown_after_restart_not_live() {
    let f = fixture();
    let thread = f.store.create_thread("Crash").unwrap();
    let s = shell_session(&f.store, &thread.id);
    let pid;
    {
        let (_sink, mgr) = manager(&f.store);
        let info = mgr
            .start(TerminalSpec {
                session_id: s.id.clone(),
                cwd: None,
                command: Some(vec!["/bin/sh".into(), "-c".into(), "sleep 300".into()]),
                cols: 80,
                rows: 24,
            })
            .unwrap();
        pid = info.pid.unwrap();
        // Deliberately no close and no shutdown_all: this stands in for a
        // process that died without recording anything. Dropping the manager
        // does not touch the child, which the reader and waiter threads keep.
    }

    // Restart over the same database.
    let store = Store::open(&f.db).unwrap();
    assert_eq!(
        store.get_session(&s.id).unwrap().ended_at,
        None,
        "before reconciliation the record is still the stale one"
    );
    assert_eq!(store.reconcile_interrupted_runs().unwrap(), 1);

    let after = store.get_session(&s.id).unwrap();
    assert!(
        after.ended_at.is_some(),
        "an interrupted session is not presented as still open after a restart"
    );
    let outcome = after.outcome.unwrap();
    assert!(
        outcome.to_lowercase().contains("unknown"),
        "the outcome is honest about not knowing, got: {outcome}"
    );

    // Housekeeping for the leaked child.
    unsafe { libc::kill(pid as i32, libc::SIGKILL) };
}

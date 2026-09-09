//! Durable persistence for Hermes ACP agent runs.
//!
//! Every test uses a `TempDir` database. Nothing here touches the real Faiden
//! database, `~/.hermes`, or any external service.

use faiden_lib::store::{
    AgentMessageDraft, AgentPermissionResolution, NewSession, Store, AGENT_OUTCOME_INTERRUPTED,
    LIMIT_AGENT_BODY_CHARS, LIMIT_AGENT_DETAIL_CHARS, LIMIT_AGENT_KEY_CHARS, LIMIT_AGENT_LIST_MAX,
};
use tempfile::TempDir;

fn store(dir: &TempDir) -> Store {
    Store::open(&dir.path().join("faiden.sqlite3")).expect("open store")
}

/// Thread + agent-kind session, returned as `(thread_id, session_id)`.
fn thread_with_session(store: &Store) -> (String, String) {
    let thread = store.create_thread("ACP thread").expect("thread");
    let session = store
        .create_session(
            &thread.id,
            NewSession {
                label: "Hermes session".to_string(),
                kind: "hermes-acp".to_string(),
                predecessor_id: None,
                briefing: String::new(),
            },
        )
        .expect("session");
    (thread.id, session.id)
}

#[test]
fn an_agent_run_is_durable_before_anything_streams() {
    let dir = TempDir::new().unwrap();
    let store = store(&dir);
    let (_thread, session) = thread_with_session(&store);

    let run = store
        .start_agent_run("run_a", &session, "/usr/local/bin/hermes", Some("/tmp"))
        .expect("start run");

    assert_eq!(run.run_id, "run_a");
    assert_eq!(run.session_id, session);
    assert_eq!(run.program, "/usr/local/bin/hermes");
    assert_eq!(run.cwd.as_deref(), Some("/tmp"));
    assert_eq!(run.acp_session_id, None, "no session id before session/new");
    assert_eq!(run.ended_at, None);

    store
        .record_acp_session_id("run_a", "sess-123")
        .expect("record acp session id");
    let reloaded = store.get_agent_run("run_a").expect("reload");
    assert_eq!(reloaded.acp_session_id.as_deref(), Some("sess-123"));
}

#[test]
fn starting_a_run_for_an_unknown_session_fails_closed() {
    let dir = TempDir::new().unwrap();
    let store = store(&dir);
    let err = store
        .start_agent_run("run_x", "ses_missing", "hermes", None)
        .expect_err("unknown session must be refused");
    assert_eq!(err.code(), "NOT_FOUND");
}

#[test]
fn an_ended_run_keeps_the_first_outcome_that_was_observed() {
    let dir = TempDir::new().unwrap();
    let store = store(&dir);
    let (_thread, session) = thread_with_session(&store);
    store
        .start_agent_run("run_a", &session, "hermes", None)
        .unwrap();

    store
        .end_agent_run("run_a", "agent process exited")
        .unwrap();
    // The quit path racing the reader thread must not rewrite the fact.
    store
        .end_agent_run("run_a", "terminated because Faiden quit")
        .unwrap();

    let run = store.get_agent_run("run_a").unwrap();
    assert_eq!(run.outcome.as_deref(), Some("agent process exited"));
    assert!(run.ended_at.is_some());

    // The owning Faiden session is settled with the same factual outcome.
    let settled = store.get_session(&session).unwrap();
    assert_eq!(settled.outcome.as_deref(), Some("agent process exited"));
}

#[test]
fn interrupted_runs_reconcile_as_disconnected_never_as_live() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("faiden.sqlite3");
    let session = {
        let store = Store::open(&path).unwrap();
        let (_thread, session) = thread_with_session(&store);
        store
            .start_agent_run("run_a", &session, "hermes", None)
            .unwrap();
        session
    };

    // Restart: a run still marked open belongs to a process that is gone.
    let store = Store::open(&path).unwrap();
    assert_eq!(store.reconcile_interrupted_agent_runs().unwrap(), 1);
    let run = store.get_agent_run("run_a").unwrap();
    assert_eq!(run.outcome.as_deref(), Some(AGENT_OUTCOME_INTERRUPTED));
    assert!(!AGENT_OUTCOME_INTERRUPTED.to_lowercase().contains("success"));
    assert_eq!(
        store.get_session(&session).unwrap().outcome.as_deref(),
        Some(AGENT_OUTCOME_INTERRUPTED)
    );
    // Idempotent across a second restart.
    assert_eq!(store.reconcile_interrupted_agent_runs().unwrap(), 0);
}

#[test]
fn streamed_chunks_upsert_one_message_per_key() {
    let dir = TempDir::new().unwrap();
    let store = store(&dir);
    let (_thread, session) = thread_with_session(&store);
    store
        .start_agent_run("run_a", &session, "hermes", None)
        .unwrap();

    let first = store
        .upsert_agent_message(
            "run_a",
            AgentMessageDraft {
                key: "agent:1",
                role: "agent",
                turn: 1,
                body: "Hel",
                detail: None,
                status: Some("streaming"),
            },
        )
        .unwrap();
    let second = store
        .upsert_agent_message(
            "run_a",
            AgentMessageDraft {
                key: "agent:1",
                role: "agent",
                turn: 1,
                body: "Hello there",
                detail: None,
                status: Some("complete"),
            },
        )
        .unwrap();

    assert_eq!(first.id, second.id, "the same key is one message row");
    assert_eq!(second.body, "Hello there");
    assert_eq!(second.status.as_deref(), Some("complete"));

    let (messages, truncated) = store.list_agent_messages("run_a", 100).unwrap();
    assert_eq!(messages.len(), 1);
    assert!(!truncated);
}

#[test]
fn a_transcript_is_bounded_and_reports_truncation_rather_than_pretending() {
    let dir = TempDir::new().unwrap();
    let store = store(&dir);
    let (_thread, session) = thread_with_session(&store);
    store
        .start_agent_run("run_a", &session, "hermes", None)
        .unwrap();

    for i in 0..12 {
        store
            .upsert_agent_message(
                "run_a",
                AgentMessageDraft {
                    key: &format!("agent:{i}"),
                    role: "agent",
                    turn: i,
                    body: &format!("chunk {i}"),
                    detail: None,
                    status: None,
                },
            )
            .unwrap();
    }

    let (messages, truncated) = store.list_agent_messages("run_a", 5).unwrap();
    assert_eq!(messages.len(), 5, "bounded to the newest 5");
    assert!(truncated, "truncation is reported, not hidden");
    assert_eq!(messages.first().unwrap().body, "chunk 7");
    assert_eq!(messages.last().unwrap().body, "chunk 11");
}

#[test]
fn an_oversized_message_body_is_refused_not_silently_stored() {
    let dir = TempDir::new().unwrap();
    let store = store(&dir);
    let (_thread, session) = thread_with_session(&store);
    store
        .start_agent_run("run_a", &session, "hermes", None)
        .unwrap();

    let huge = "x".repeat(LIMIT_AGENT_BODY_CHARS + 1);
    let err = store
        .upsert_agent_message(
            "run_a",
            AgentMessageDraft {
                key: "agent:1",
                role: "agent",
                turn: 1,
                body: &huge,
                detail: None,
                status: None,
            },
        )
        .expect_err("oversized body must be refused");
    assert_eq!(err.code(), "VALIDATION");
}

#[test]
fn a_permission_request_records_its_offer_and_its_single_resolution() {
    let dir = TempDir::new().unwrap();
    let store = store(&dir);
    let (_thread, session) = thread_with_session(&store);
    store
        .start_agent_run("run_a", &session, "hermes", None)
        .unwrap();

    let request = store
        .record_permission_request(
            "perm_1",
            "run_a",
            "Run a command: rm -rf build",
            "$ rm -rf build",
            r#"[{"optionId":"allow_once","name":"Allow once","kind":"allow_once"}]"#,
        )
        .unwrap();
    assert_eq!(request.resolved_at, None);
    assert_eq!(request.resolution, None);

    store
        .resolve_permission_request(
            "perm_1",
            AgentPermissionResolution::Selected {
                option_id: "allow_once".to_string(),
            },
        )
        .unwrap();
    // A second answer cannot overwrite what the user actually chose.
    store
        .resolve_permission_request("perm_1", AgentPermissionResolution::Cancelled)
        .unwrap();

    let stored = store.get_permission_request("perm_1").unwrap();
    assert_eq!(stored.resolution.as_deref(), Some("selected:allow_once"));
    assert!(stored.resolved_at.is_some());
}

#[test]
fn the_agent_schema_is_additive_and_preserves_rows_written_by_the_old_build() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("faiden.sqlite3");

    // A database exactly as the foundation build left it: no agent tables and
    // no user_version stamp.
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE threads (
                seq INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT NOT NULL UNIQUE,
                title TEXT NOT NULL, notes TEXT NOT NULL DEFAULT '', workdir TEXT,
                created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
                seen_at INTEGER, reviewed_at INTEGER);
            INSERT INTO threads (id, title, notes, created_at, updated_at)
                VALUES ('thr_old', 'Older thread', 'notes that must survive', 1, 1);
            "#,
        )
        .unwrap();
        assert_eq!(
            conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    let store = Store::open(&path).unwrap();
    let old = store.get_thread("thr_old").expect("old row survives");
    assert_eq!(old.title, "Older thread");
    assert_eq!(old.notes, "notes that must survive");

    // The new tables exist and are usable against that same database.
    let session = store
        .create_session(
            "thr_old",
            NewSession {
                label: "Hermes session".to_string(),
                kind: "hermes-acp".to_string(),
                predecessor_id: None,
                briefing: String::new(),
            },
        )
        .unwrap();
    store
        .start_agent_run("run_new", &session.id, "hermes", None)
        .expect("agent tables exist after migration");
}

// --- review findings: lifecycle invariants at the persistence boundary -------

#[test]
fn a_session_that_has_already_ended_cannot_own_another_agent_run() {
    // Store-early-review blocker 1. The store, not just the command layer, is
    // the authority: an ended session is closed for good.
    let dir = TempDir::new().unwrap();
    let store = store(&dir);
    let (_thread, session) = thread_with_session(&store);
    store.end_session(&session, "closed by the user").unwrap();

    let err = store
        .start_agent_run("run_a", &session, "hermes", None)
        .expect_err("an ended session must not gain a new owned run");
    assert_eq!(err.code(), "CONFLICT");
}

#[test]
fn a_reconciled_session_cannot_own_another_agent_run_either() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("faiden.sqlite3");
    let session = {
        let store = Store::open(&path).unwrap();
        let (_thread, session) = thread_with_session(&store);
        store
            .start_agent_run("run_a", &session, "hermes", None)
            .unwrap();
        session
    };
    let store = Store::open(&path).unwrap();
    store.reconcile_interrupted_agent_runs().unwrap();

    let err = store
        .start_agent_run("run_b", &session, "hermes", None)
        .expect_err("reconciliation ended the session; it stays ended");
    assert_eq!(err.code(), "CONFLICT");
}

#[test]
fn one_session_can_never_hold_two_open_agent_runs() {
    // Store-early-review blocker 2, and the persistence half of the parent's
    // reproduced start race: the invariant is enforced by the database, so two
    // independent connections cannot both win.
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("faiden.sqlite3");
    let store_a = Store::open(&path).unwrap();
    let (_thread, session) = thread_with_session(&store_a);
    store_a
        .start_agent_run("run_a", &session, "hermes", None)
        .unwrap();

    let err = store_a
        .start_agent_run("run_b", &session, "hermes", None)
        .expect_err("the same session must not own two live runs");
    assert_eq!(err.code(), "CONFLICT");

    let store_b = Store::open(&path).unwrap();
    let err = store_b
        .start_agent_run("run_c", &session, "hermes", None)
        .expect_err("a second connection must lose too");
    assert_eq!(err.code(), "CONFLICT");
}

#[test]
fn concurrent_starts_for_one_session_leave_exactly_one_winner() {
    use std::sync::{Arc, Barrier};

    let dir = TempDir::new().unwrap();
    let store = Arc::new(store(&dir));
    let (_thread, session) = thread_with_session(&store);

    let barrier = Arc::new(Barrier::new(8));
    let mut handles = Vec::new();
    for i in 0..8 {
        let store = store.clone();
        let session = session.clone();
        let barrier = barrier.clone();
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            store
                .start_agent_run(&format!("run_{i}"), &session, "hermes", None)
                .is_ok()
        }));
    }
    let winners = handles
        .into_iter()
        .filter(|_| true)
        .map(|h| h.join().unwrap())
        .filter(|ok| *ok)
        .count();
    assert_eq!(
        winners, 1,
        "exactly one concurrent start may own the session"
    );
}

#[test]
fn a_session_may_own_a_new_run_once_the_previous_one_has_ended() {
    // The invariant is "one *open* run", not "one run ever" — but the session
    // ends with its run, so a genuinely new session is what comes next.
    let dir = TempDir::new().unwrap();
    let store = store(&dir);
    let thread = store.create_thread("ACP thread").unwrap();
    let first = store
        .create_session(
            &thread.id,
            NewSession {
                label: "first".to_string(),
                kind: "hermes-acp".to_string(),
                predecessor_id: None,
                briefing: String::new(),
            },
        )
        .unwrap();
    store
        .start_agent_run("run_a", &first.id, "hermes", None)
        .unwrap();
    store
        .end_agent_run("run_a", "the agent process exited")
        .unwrap();

    let second = store
        .create_session(
            &thread.id,
            NewSession {
                label: "second".to_string(),
                kind: "hermes-acp".to_string(),
                predecessor_id: Some(first.id.clone()),
                briefing: String::new(),
            },
        )
        .unwrap();
    store
        .start_agent_run("run_b", &second.id, "hermes", None)
        .expect("a fresh session may start a fresh agent");
}

#[test]
fn a_transcript_request_is_clamped_by_the_store_not_by_its_caller() {
    // Store-early-review blocker 4: `limit as i64` made a huge request negative,
    // and SQLite reads a negative LIMIT as "no limit".
    let dir = TempDir::new().unwrap();
    let store = store(&dir);
    let (_thread, session) = thread_with_session(&store);
    store
        .start_agent_run("run_a", &session, "hermes", None)
        .unwrap();
    for i in 0..5 {
        store
            .upsert_agent_message(
                "run_a",
                AgentMessageDraft {
                    key: &format!("agent:{i}"),
                    role: "agent",
                    turn: i,
                    body: &format!("chunk {i}"),
                    detail: None,
                    status: None,
                },
            )
            .unwrap();
    }

    let (all, truncated) = store.list_agent_messages("run_a", usize::MAX).unwrap();
    assert_eq!(
        all.len(),
        5,
        "an overflowing limit must not become unlimited"
    );
    assert!(!truncated);

    let (none, truncated) = store.list_agent_messages("run_a", 0).unwrap();
    assert!(none.is_empty());
    assert!(
        truncated,
        "asking for nothing still reports that there is more"
    );

    // The ceiling is the store's, not the caller's: even an unbounded request
    // can never return more than this.
    assert_eq!(
        store
            .list_agent_messages("run_a", usize::MAX)
            .unwrap()
            .0
            .len(),
        5usize.min(LIMIT_AGENT_LIST_MAX)
    );
}

#[test]
fn transcript_field_bounds_accept_the_maximum_and_refuse_one_more() {
    let dir = TempDir::new().unwrap();
    let store = store(&dir);
    let (_thread, session) = thread_with_session(&store);
    store
        .start_agent_run("run_a", &session, "hermes", None)
        .unwrap();

    fn at_limit<'a>(key: &'a str, body: &'a str, detail: Option<&'a str>) -> AgentMessageDraft<'a> {
        AgentMessageDraft {
            key,
            role: "agent",
            turn: 1,
            body,
            detail,
            status: None,
        }
    }

    let max_key = "k".repeat(LIMIT_AGENT_KEY_CHARS);
    store
        .upsert_agent_message("run_a", at_limit(&max_key, "body", None))
        .expect("a key of exactly the maximum length is accepted");
    let over_key = "k".repeat(LIMIT_AGENT_KEY_CHARS + 1);
    assert_eq!(
        store
            .upsert_agent_message("run_a", at_limit(&over_key, "body", None))
            .expect_err("one character too long")
            .code(),
        "VALIDATION"
    );

    let max_body = "b".repeat(LIMIT_AGENT_BODY_CHARS);
    store
        .upsert_agent_message("run_a", at_limit("body-key", &max_body, None))
        .expect("a body of exactly the maximum length is accepted");

    let max_detail = "d".repeat(LIMIT_AGENT_DETAIL_CHARS);
    store
        .upsert_agent_message("run_a", at_limit("detail-key", "body", Some(&max_detail)))
        .expect("a detail of exactly the maximum length is accepted");
    let over_detail = "d".repeat(LIMIT_AGENT_DETAIL_CHARS + 1);
    assert_eq!(
        store
            .upsert_agent_message("run_a", at_limit("detail-key", "body", Some(&over_detail)))
            .expect_err("one character too long")
            .code(),
        "VALIDATION"
    );
}

#[test]
fn migration_preserves_every_foundation_table_and_is_idempotent_on_reopen() {
    // The foundation schema exactly as commit a5a3681 wrote it, with a row in
    // every table, so a regression in any baseline table or index is caught.
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("faiden.sqlite3");
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(FOUNDATION_SCHEMA).unwrap();
        conn.execute_batch(
            r#"
            INSERT INTO threads (id, title, notes, workdir, created_at, updated_at, seen_at)
                VALUES ('thr_old', 'Older thread', 'notes that must survive', NULL, 1, 2, 3);
            INSERT INTO sessions (id, thread_id, label, kind, briefing, created_at)
                VALUES ('ses_old', 'thr_old', 'Shell session', 'shell', 'briefing', 4);
            INSERT INTO review_items (id, thread_id, session_id, kind, summary, created_at)
                VALUES ('rev_old', 'thr_old', 'ses_old', 'handoff', 'read me', 5);
            INSERT INTO terminal_runs (terminal_id, session_id, started_at, ended_at, outcome)
                VALUES ('term_old', 'ses_old', 6, 7, 'exited');
            "#,
        )
        .unwrap();
    }

    let store = Store::open(&path).unwrap();
    assert_eq!(
        store.get_thread("thr_old").unwrap().notes,
        "notes that must survive"
    );
    assert_eq!(store.get_session("ses_old").unwrap().label, "Shell session");
    let items = store.list_review_items("thr_old").unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].summary, "read me");
    drop(store);

    let conn = rusqlite::Connection::open(&path).unwrap();
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert!(version >= 3, "the stamp advances to the current schema");
    let terminal_outcome: String = conn
        .query_row(
            "SELECT outcome FROM terminal_runs WHERE terminal_id = 'term_old'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(terminal_outcome, "exited");
    drop(conn);

    // Reopening is a no-op, not a second migration.
    let reopened = Store::open(&path).unwrap();
    assert_eq!(
        reopened.get_thread("thr_old").unwrap().title,
        "Older thread"
    );
    let conn = rusqlite::Connection::open(&path).unwrap();
    let after: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(after, version, "reopening does not re-stamp or re-migrate");
}

/// Verbatim foundation schema (commit a5a3681), used to prove the agent
/// migration is additive against a database this build never created.
const FOUNDATION_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS threads (
    seq         INTEGER PRIMARY KEY AUTOINCREMENT,
    id          TEXT NOT NULL UNIQUE,
    title       TEXT NOT NULL,
    notes       TEXT NOT NULL DEFAULT '',
    workdir     TEXT,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL,
    seen_at     INTEGER,
    reviewed_at INTEGER
);

CREATE TABLE IF NOT EXISTS sessions (
    seq            INTEGER PRIMARY KEY AUTOINCREMENT,
    id             TEXT NOT NULL UNIQUE,
    thread_id      TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    label          TEXT NOT NULL,
    kind           TEXT NOT NULL,
    predecessor_id TEXT REFERENCES sessions(id) ON DELETE SET NULL,
    briefing       TEXT NOT NULL DEFAULT '',
    created_at     INTEGER NOT NULL,
    ended_at       INTEGER,
    outcome        TEXT
);
CREATE INDEX IF NOT EXISTS sessions_by_thread ON sessions(thread_id);

CREATE TABLE IF NOT EXISTS review_items (
    seq         INTEGER PRIMARY KEY AUTOINCREMENT,
    id          TEXT NOT NULL UNIQUE,
    thread_id   TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    session_id  TEXT REFERENCES sessions(id) ON DELETE SET NULL,
    kind        TEXT NOT NULL,
    summary     TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    seen_at     INTEGER,
    reviewed_at INTEGER
);
CREATE INDEX IF NOT EXISTS review_items_by_thread ON review_items(thread_id);

CREATE TABLE IF NOT EXISTS terminal_runs (
    seq         INTEGER PRIMARY KEY AUTOINCREMENT,
    terminal_id TEXT NOT NULL UNIQUE,
    session_id  TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    started_at  INTEGER NOT NULL,
    ended_at    INTEGER,
    outcome     TEXT
);
CREATE INDEX IF NOT EXISTS terminal_runs_open ON terminal_runs(ended_at);
"#;

#[test]
fn a_new_result_makes_a_seen_thread_actionable_again_without_unreviewing_it() {
    // Store-early-review blocker 3, assessed against the finished runtime: the
    // agent files unseen review items, and the thread's own history of being
    // seen/reviewed is a fact that must not be rewritten.
    let dir = TempDir::new().unwrap();
    let store = store(&dir);
    let (thread, session) = thread_with_session(&store);
    store.mark_thread_seen(&thread).unwrap();
    let reviewed = store.mark_thread_reviewed(&thread).unwrap();
    assert!(reviewed.reviewed_at.is_some());

    store
        .add_review_item(
            &thread,
            Some(&session),
            "agent-turn",
            "Hermes turn ended (end_turn).",
        )
        .unwrap();

    let counts = store.unseen_review_counts().unwrap();
    assert_eq!(counts.len(), 1);
    assert_eq!(counts[0].thread_id, thread);
    assert_eq!(
        counts[0].unseen, 1,
        "a new result is unseen on a seen thread"
    );

    // The human acts recorded earlier are still true.
    let after = store.get_thread(&thread).unwrap();
    assert!(after.seen_at.is_some());
    assert!(after.reviewed_at.is_some());

    // Looking at the thread clears the unseen count, and only that.
    store.mark_thread_seen(&thread).unwrap();
    assert!(store.unseen_review_counts().unwrap().is_empty());
    let items = store.list_review_items(&thread).unwrap();
    assert!(items[0].seen_at.is_some());
    assert_eq!(items[0].reviewed_at, None, "seen is not reviewed");
}

//! Thread-first SQLite persistence.
//!
//! A thread is the durable unit: it needs no repository, no worktree and no
//! terminal. Sessions hang underneath a thread and may point at a predecessor.
//! Human review state (`seen` / `reviewed`) is deliberately separate from any
//! execution state.

use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::clock::{format_utc, MonotonicClock};
use crate::error::AppError;

pub const LIMIT_TITLE_CHARS: usize = 200;
pub const LIMIT_NOTES_CHARS: usize = 20_000;
pub const LIMIT_BRIEFING_CHARS: usize = 40_000;
pub const LIMIT_LABEL_CHARS: usize = 200;
pub const LIMIT_KIND_CHARS: usize = 64;
pub const LIMIT_SUMMARY_CHARS: usize = 1_000;
pub const LIMIT_PATH_CHARS: usize = 4_096;
/// Upper bound on one persisted agent transcript entry. A chunk stream that
/// exceeds it is refused rather than silently trimmed by SQLite.
pub const LIMIT_AGENT_BODY_CHARS: usize = 200_000;
pub const LIMIT_AGENT_DETAIL_CHARS: usize = 40_000;
pub const LIMIT_AGENT_KEY_CHARS: usize = 200;
/// Hard ceiling on how many transcript rows one read may return. The store owns
/// this bound, not its caller: a `usize` above `i64::MAX` used to cast negative,
/// and SQLite reads a negative `LIMIT` as "no limit at all".
pub const LIMIT_AGENT_LIST_MAX: usize = 2_000;

/// Stated verbatim on every generated handoff draft. This slice has no model
/// integration at all, and the UI must never imply otherwise.
/// Recorded for a run whose end was never observed, because the process that
/// owned it exited without saying how. It is deliberately not a success claim.
pub const OUTCOME_INTERRUPTED: &str =
    "unknown — Faiden exited without recording how this terminal ended";

pub const HANDOFF_DISCLOSURE: &str = "This is a human-authored handoff draft. It does not resume an AI context: no model session was replayed, continued or contacted.";

/// Recorded for an agent run that was still open when Faiden exited. Agents are
/// owned by this application, so such a run belongs to a process that is gone.
/// It is a lifecycle fact, never a claim that the work finished.
pub const AGENT_OUTCOME_INTERRUPTED: &str =
    "disconnected — Faiden exited while this agent session was running";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Thread {
    pub id: String,
    pub title: String,
    pub notes: String,
    pub workdir: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub seen_at: Option<i64>,
    pub reviewed_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub id: String,
    pub thread_id: String,
    pub label: String,
    pub kind: String,
    pub predecessor_id: Option<String>,
    pub briefing: String,
    pub created_at: i64,
    pub ended_at: Option<i64>,
    pub outcome: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewSession {
    pub label: String,
    pub kind: String,
    pub predecessor_id: Option<String>,
    pub briefing: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReviewItem {
    pub id: String,
    pub thread_id: String,
    pub session_id: Option<String>,
    pub kind: String,
    pub summary: String,
    pub created_at: i64,
    pub seen_at: Option<i64>,
    pub reviewed_at: Option<i64>,
}

/// One owned `hermes acp` child process, bound to the Faiden session it serves.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentRun {
    pub run_id: String,
    pub session_id: String,
    pub program: String,
    /// The directory the child was started in. Not an observed tool cwd.
    pub cwd: Option<String>,
    /// The agent's own session id, known only once `session/new` answered.
    pub acp_session_id: Option<String>,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub outcome: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentMessage {
    pub id: String,
    pub run_id: String,
    /// Streaming identity: repeated chunks for one turn share a key.
    pub key: String,
    /// `user` | `agent` | `thought` | `tool` | `system`.
    pub role: String,
    pub turn: i64,
    pub body: String,
    /// JSON side-channel for tool calls (kind, raw input); never rendered raw.
    pub detail: Option<String>,
    pub status: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// The fields of a transcript entry, passed as one value so streaming callers
/// read as data rather than as a long positional argument list.
#[derive(Debug, Clone, Copy)]
pub struct AgentMessageDraft<'a> {
    pub key: &'a str,
    pub role: &'a str,
    pub turn: i64,
    pub body: &'a str,
    pub detail: Option<&'a str>,
    pub status: Option<&'a str>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentPermissionRecord {
    pub id: String,
    pub run_id: String,
    pub title: String,
    pub detail: String,
    /// The offered options, verbatim JSON as the agent sent them.
    pub options: String,
    pub created_at: i64,
    pub resolved_at: Option<i64>,
    pub resolution: Option<String>,
}

/// Results waiting to be looked at on one thread. Independent of the thread's
/// own seen/reviewed state, which records what a human did rather than what an
/// agent produced afterwards.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UnseenReviewCount {
    pub thread_id: String,
    pub unseen: i64,
}

/// How a permission request ended. `Cancelled` is ACP's spelling for "not
/// allowed" and is what every fail-closed path records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentPermissionResolution {
    Selected { option_id: String },
    Cancelled,
}

impl AgentPermissionResolution {
    pub fn as_record(&self) -> String {
        match self {
            AgentPermissionResolution::Selected { option_id } => format!("selected:{option_id}"),
            AgentPermissionResolution::Cancelled => "cancelled".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HandoffDraft {
    pub thread_id: String,
    pub predecessor_session_id: Option<String>,
    pub text: String,
    pub disclosure: String,
    pub generated_at: i64,
}

pub struct Store {
    conn: Mutex<Connection>,
    clock: MonotonicClock,
}

const SCHEMA: &str = r#"
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

-- One row per PTY run, so a terminal's end can be attributed to the session it
-- belonged to even when the process that started it died without saying so.
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

/// Migration 2. Purely additive: it creates tables the foundation build never
/// had and touches nothing that already exists, so an older database keeps
/// every row it had.
const SCHEMA_AGENT: &str = r#"
-- One row per owned `hermes acp` child process.
CREATE TABLE IF NOT EXISTS agent_runs (
    seq            INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id         TEXT NOT NULL UNIQUE,
    session_id     TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    program        TEXT NOT NULL,
    cwd            TEXT,
    acp_session_id TEXT,
    started_at     INTEGER NOT NULL,
    ended_at       INTEGER,
    outcome        TEXT
);
CREATE INDEX IF NOT EXISTS agent_runs_open ON agent_runs(ended_at);
CREATE INDEX IF NOT EXISTS agent_runs_by_session ON agent_runs(session_id);

-- Transcript. `key` is the streaming identity of an entry (an assistant turn,
-- a tool call id), so repeated chunks update one row instead of appending
-- thousands.
CREATE TABLE IF NOT EXISTS agent_messages (
    seq        INTEGER PRIMARY KEY AUTOINCREMENT,
    id         TEXT NOT NULL UNIQUE,
    run_id     TEXT NOT NULL REFERENCES agent_runs(run_id) ON DELETE CASCADE,
    key        TEXT NOT NULL,
    role       TEXT NOT NULL,
    turn       INTEGER NOT NULL,
    body       TEXT NOT NULL,
    detail     TEXT,
    status     TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    UNIQUE(run_id, key)
);
CREATE INDEX IF NOT EXISTS agent_messages_by_run ON agent_messages(run_id, seq);

-- What the agent asked permission for, and the single answer it received.
CREATE TABLE IF NOT EXISTS agent_permission_requests (
    seq         INTEGER PRIMARY KEY AUTOINCREMENT,
    id          TEXT NOT NULL UNIQUE,
    run_id      TEXT NOT NULL REFERENCES agent_runs(run_id) ON DELETE CASCADE,
    title       TEXT NOT NULL,
    detail      TEXT NOT NULL,
    options     TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    resolved_at INTEGER,
    resolution  TEXT
);
CREATE INDEX IF NOT EXISTS agent_permissions_by_run ON agent_permission_requests(run_id);
"#;

/// Migration 3. Makes "one live agent per session" a database invariant rather
/// than a check some caller remembers to perform.
///
/// Two `AgentManager::start` calls for one session could both pass an
/// in-memory registry check and then both insert, so both children ran and
/// either could end the shared owning session underneath the other. A partial
/// unique index is the only place that race can be decided once.
///
/// Any run still open when this runs belongs to a process that is gone — agents
/// do not survive the application's exit — so settling them first is both
/// truthful and what lets the index be created on an existing file.
const SCHEMA_AGENT_SINGLE_OWNER: &str = r#"
UPDATE agent_runs
   SET ended_at = COALESCE(ended_at, strftime('%s','now') * 1000),
       outcome  = COALESCE(outcome, 'disconnected — Faiden exited while this agent session was running')
 WHERE ended_at IS NULL;

CREATE UNIQUE INDEX IF NOT EXISTS agent_runs_one_open_per_session
    ON agent_runs(session_id) WHERE ended_at IS NULL;
"#;

/// Applied in order to whatever `PRAGMA user_version` the file reports. A
/// foundation database is stamped 0 and already holds migration 1's tables;
/// every statement is `IF NOT EXISTS`, so replaying it is a no-op rather than
/// a failure.
const MIGRATIONS: &[&str] = &[SCHEMA, SCHEMA_AGENT, SCHEMA_AGENT_SINGLE_OWNER];

const THREAD_COLS: &str = "id, title, notes, workdir, created_at, updated_at, seen_at, reviewed_at";
const SESSION_COLS: &str =
    "id, thread_id, label, kind, predecessor_id, briefing, created_at, ended_at, outcome";
const REVIEW_COLS: &str =
    "id, thread_id, session_id, kind, summary, created_at, seen_at, reviewed_at";
const AGENT_RUN_COLS: &str =
    "run_id, session_id, program, cwd, acp_session_id, started_at, ended_at, outcome";
const AGENT_MESSAGE_COLS: &str =
    "id, run_id, key, role, turn, body, detail, status, created_at, updated_at";
const AGENT_PERMISSION_COLS: &str =
    "id, run_id, title, detail, options, created_at, resolved_at, resolution";

/// Brings a database file up to the current schema, whatever version it is at.
/// Every step is additive, so an older Faiden database keeps all of its rows.
fn migrate(conn: &Connection) -> Result<(), AppError> {
    let current: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if current < 0 || current as usize > MIGRATIONS.len() {
        return Err(AppError::internal(format!(
            "database reports schema version {current}, but this build only knows {}; \
             refusing to touch it rather than risk destroying newer data",
            MIGRATIONS.len()
        )));
    }
    for (index, statements) in MIGRATIONS.iter().enumerate().skip(current as usize) {
        conn.execute_batch(statements)?;
        conn.pragma_update(None, "user_version", (index + 1) as i64)?;
    }
    Ok(())
}

impl Store {
    pub fn open(path: &Path) -> Result<Store, AppError> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let conn = Connection::open(path)?;
        // Pragmas stay outside the migration transaction: `journal_mode` cannot
        // be changed from inside one.
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON;")?;
        migrate(&conn)?;
        Ok(Store {
            conn: Mutex::new(conn),
            clock: MonotonicClock::new(),
        })
    }

    fn lock(&self) -> Result<MutexGuard<'_, Connection>, AppError> {
        self.conn
            .lock()
            .map_err(|_| AppError::internal("store mutex poisoned by an earlier panic"))
    }

    // --- threads ------------------------------------------------------------

    pub fn create_thread(&self, title: &str) -> Result<Thread, AppError> {
        let title = validate_text("title", title, 1, LIMIT_TITLE_CHARS, true)?;
        let now = self.clock.now_ms();
        let id = new_id("thr");
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO threads (id, title, notes, workdir, created_at, updated_at) \
             VALUES (?1, ?2, '', NULL, ?3, ?3)",
            params![id, title, now],
        )?;
        read_thread(&conn, &id)
    }

    pub fn list_threads(&self) -> Result<Vec<Thread>, AppError> {
        let conn = self.lock()?;
        let sql = format!("SELECT {THREAD_COLS} FROM threads ORDER BY updated_at DESC, seq DESC");
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], map_thread)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn get_thread(&self, id: &str) -> Result<Thread, AppError> {
        let conn = self.lock()?;
        read_thread(&conn, id)
    }

    pub fn rename_thread(&self, id: &str, title: &str) -> Result<Thread, AppError> {
        let title = validate_text("title", title, 1, LIMIT_TITLE_CHARS, true)?;
        let now = self.clock.now_ms();
        let conn = self.lock()?;
        require_thread(&conn, id)?;
        conn.execute(
            "UPDATE threads SET title = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, title, now],
        )?;
        read_thread(&conn, id)
    }

    pub fn set_thread_notes(&self, id: &str, notes: &str) -> Result<Thread, AppError> {
        // Notes keep their whitespace: they are free-form brainstorming text.
        let notes = validate_text("notes", notes, 0, LIMIT_NOTES_CHARS, false)?;
        let now = self.clock.now_ms();
        let conn = self.lock()?;
        require_thread(&conn, id)?;
        conn.execute(
            "UPDATE threads SET notes = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, notes, now],
        )?;
        read_thread(&conn, id)
    }

    pub fn set_thread_workdir(&self, id: &str, workdir: Option<&str>) -> Result<Thread, AppError> {
        let workdir = match workdir {
            None => None,
            Some(raw) => {
                let value = validate_text("workdir", raw, 1, LIMIT_PATH_CHARS, true)?;
                let path = Path::new(&value);
                if !path.is_absolute() {
                    return Err(AppError::validation(
                        "workdir",
                        "a bound directory must be an absolute path",
                    ));
                }
                // The store is the authority for this invariant, not only the
                // command layer: a thread must never display a directory that
                // was not there when it was bound.
                if !path.is_dir() {
                    return Err(AppError::validation(
                        "workdir",
                        format!("{value} is not an existing directory"),
                    ));
                }
                Some(value)
            }
        };
        let now = self.clock.now_ms();
        let conn = self.lock()?;
        require_thread(&conn, id)?;
        conn.execute(
            "UPDATE threads SET workdir = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, workdir, now],
        )?;
        read_thread(&conn, id)
    }

    /// Records that a human looked at the thread. Deliberately does NOT set
    /// `reviewed_at` on the thread or on any of its inbox items.
    pub fn mark_thread_seen(&self, id: &str) -> Result<Thread, AppError> {
        let now = self.clock.now_ms();
        let mut conn = self.lock()?;
        require_thread(&conn, id)?;
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE threads SET seen_at = ?2 WHERE id = ?1 AND seen_at IS NULL",
            params![id, now],
        )?;
        tx.execute(
            "UPDATE review_items SET seen_at = ?2 WHERE thread_id = ?1 AND seen_at IS NULL",
            params![id, now],
        )?;
        tx.commit()?;
        read_thread(&conn, id)
    }

    /// An explicit human act, never implied by opening or by a process exiting.
    pub fn mark_thread_reviewed(&self, id: &str) -> Result<Thread, AppError> {
        let now = self.clock.now_ms();
        let conn = self.lock()?;
        require_thread(&conn, id)?;
        conn.execute(
            "UPDATE threads SET reviewed_at = ?2, seen_at = COALESCE(seen_at, ?2) WHERE id = ?1",
            params![id, now],
        )?;
        read_thread(&conn, id)
    }

    // --- sessions -----------------------------------------------------------

    pub fn create_session(&self, thread_id: &str, new: NewSession) -> Result<Session, AppError> {
        let label = validate_text("label", &new.label, 1, LIMIT_LABEL_CHARS, true)?;
        let kind = validate_text("kind", &new.kind, 1, LIMIT_KIND_CHARS, true)?;
        let briefing = validate_text("briefing", &new.briefing, 0, LIMIT_BRIEFING_CHARS, false)?;
        let now = self.clock.now_ms();
        let id = new_id("ses");

        let conn = self.lock()?;
        require_thread(&conn, thread_id)?;
        if let Some(pred) = new.predecessor_id.as_deref() {
            require_session_in_thread(&conn, pred, thread_id)?;
        }
        conn.execute(
            "INSERT INTO sessions (id, thread_id, label, kind, predecessor_id, briefing, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![id, thread_id, label, kind, new.predecessor_id, briefing, now],
        )?;
        read_session(&conn, &id)
    }

    pub fn list_sessions(&self, thread_id: &str) -> Result<Vec<Session>, AppError> {
        let conn = self.lock()?;
        require_thread(&conn, thread_id)?;
        let sql =
            format!("SELECT {SESSION_COLS} FROM sessions WHERE thread_id = ?1 ORDER BY seq ASC");
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![thread_id], map_session)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn get_session(&self, id: &str) -> Result<Session, AppError> {
        let conn = self.lock()?;
        require_session(&conn, id)
    }

    /// Records how a session ended. This is a lifecycle fact only; it makes no
    /// claim about whether the work succeeded or was reviewed.
    pub fn end_session(&self, id: &str, outcome: &str) -> Result<Session, AppError> {
        let outcome = validate_text("outcome", outcome, 0, LIMIT_SUMMARY_CHARS, true)?;
        let now = self.clock.now_ms();
        let conn = self.lock()?;
        require_session(&conn, id)?;
        end_session_row(&conn, id, now, &outcome)?;
        read_session(&conn, id)
    }

    // --- terminal runs ------------------------------------------------------

    /// Binds a PTY run to the session it belongs to. Called before the child is
    /// spawned, so a terminal can never run without a durable owner.
    pub fn start_terminal_run(&self, terminal_id: &str, session_id: &str) -> Result<(), AppError> {
        let terminal_id = validate_text("terminal_id", terminal_id, 1, LIMIT_LABEL_CHARS, true)?;
        let now = self.clock.now_ms();
        let conn = self.lock()?;
        require_session(&conn, session_id)?;
        conn.execute(
            "INSERT INTO terminal_runs (terminal_id, session_id, started_at) VALUES (?1, ?2, ?3)",
            params![terminal_id, session_id, now],
        )?;
        Ok(())
    }

    /// Records how a run ended and ends its session with the same factual
    /// outcome. Both are first-writer-wins, so calling this twice — a natural
    /// exit racing the quit path — cannot rewrite what was already observed.
    pub fn end_terminal_run(&self, terminal_id: &str, outcome: &str) -> Result<(), AppError> {
        let outcome = validate_text("outcome", outcome, 1, LIMIT_SUMMARY_CHARS, true)?;
        let now = self.clock.now_ms();
        let mut conn = self.lock()?;
        let session_id: Option<String> = conn
            .query_row(
                "SELECT session_id FROM terminal_runs WHERE terminal_id = ?1",
                params![terminal_id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(session_id) = session_id else {
            return Err(AppError::not_found("terminal_run", terminal_id));
        };
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE terminal_runs SET ended_at = COALESCE(ended_at, ?2), \
             outcome = COALESCE(outcome, ?3) WHERE terminal_id = ?1",
            params![terminal_id, now, outcome],
        )?;
        end_session_row(&tx, &session_id, now, &outcome)?;
        tx.commit()?;
        Ok(())
    }

    /// Ends every run still marked open. Terminals do not survive this
    /// application's exit, so any open run at startup belongs to a process that
    /// is gone: it is reported as unknown, never as still live.
    pub fn reconcile_interrupted_runs(&self) -> Result<usize, AppError> {
        let now = self.clock.now_ms();
        let mut conn = self.lock()?;
        let open: Vec<(String, String)> = {
            let mut stmt = conn.prepare(
                "SELECT terminal_id, session_id FROM terminal_runs WHERE ended_at IS NULL",
            )?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            let mut v = Vec::new();
            for row in rows {
                v.push(row?);
            }
            v
        };
        let tx = conn.transaction()?;
        for (terminal_id, session_id) in &open {
            tx.execute(
                "UPDATE terminal_runs SET ended_at = ?2, outcome = COALESCE(outcome, ?3) \
                 WHERE terminal_id = ?1",
                params![terminal_id, now, OUTCOME_INTERRUPTED],
            )?;
            end_session_row(&tx, session_id, now, OUTCOME_INTERRUPTED)?;
        }
        tx.commit()?;
        Ok(open.len())
    }

    /// Composes an editable handoff draft. Nothing is persisted: the user
    /// reviews and edits the text, then explicitly creates a session from it.
    pub fn build_handoff_draft(
        &self,
        thread_id: &str,
        predecessor_id: Option<&str>,
    ) -> Result<HandoffDraft, AppError> {
        let conn = self.lock()?;
        let thread = read_thread(&conn, thread_id)?;
        let predecessor = match predecessor_id {
            None => None,
            Some(pid) => Some(require_session_in_thread(&conn, pid, thread_id)?),
        };
        drop(conn);

        let generated_at = self.clock.now_ms();
        let text = compose_handoff_text(&thread, predecessor.as_ref(), generated_at);
        Ok(HandoffDraft {
            thread_id: thread.id,
            predecessor_session_id: predecessor.map(|s| s.id),
            text,
            disclosure: HANDOFF_DISCLOSURE.to_string(),
            generated_at,
        })
    }

    // --- agent runs ---------------------------------------------------------

    /// Binds an agent run to the session it belongs to. Called *before* the
    /// child is spawned, so an agent can never run without a durable owner.
    pub fn start_agent_run(
        &self,
        run_id: &str,
        session_id: &str,
        program: &str,
        cwd: Option<&str>,
    ) -> Result<AgentRun, AppError> {
        let run_id = validate_text("run_id", run_id, 1, LIMIT_LABEL_CHARS, true)?;
        let program = validate_text("program", program, 1, LIMIT_PATH_CHARS, true)?;
        let cwd = match cwd {
            None => None,
            Some(c) => Some(validate_text("cwd", c, 1, LIMIT_PATH_CHARS, true)?),
        };
        let now = self.clock.now_ms();
        let conn = self.lock()?;
        let session = require_session(&conn, session_id)?;
        // A session ends once, for good. Reconciliation and a natural exit both
        // end it, and neither may be undone by starting another agent under it.
        if session.ended_at.is_some() {
            return Err(AppError::conflict(
                "SESSION_ENDED",
                format!("session {session_id} has already ended; start a new session"),
            ));
        }
        // The insert is the serialization point for "one live agent per
        // session": `agent_runs_one_open_per_session` rejects the loser of a
        // race here, across independent connections as well as threads.
        conn.execute(
            "INSERT INTO agent_runs (run_id, session_id, program, cwd, started_at) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![run_id, session_id, program, cwd, now],
        )
        .map_err(|e| match e {
            rusqlite::Error::SqliteFailure(inner, _)
                if inner.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                AppError::conflict(
                    "AGENT_ALREADY_RUNNING",
                    format!("session {session_id} already owns a running agent"),
                )
            }
            other => AppError::from(other),
        })?;
        read_agent_run(&conn, &run_id)
    }

    /// Records the id the agent chose for its own session. Written once
    /// `session/new` has actually answered, never before.
    pub fn record_acp_session_id(
        &self,
        run_id: &str,
        acp_session_id: &str,
    ) -> Result<(), AppError> {
        let value = validate_text("acp_session_id", acp_session_id, 1, LIMIT_LABEL_CHARS, true)?;
        let conn = self.lock()?;
        let changed = conn.execute(
            "UPDATE agent_runs SET acp_session_id = ?2 WHERE run_id = ?1",
            params![run_id, value],
        )?;
        if changed == 0 {
            return Err(AppError::not_found("agent_run", run_id));
        }
        Ok(())
    }

    pub fn get_agent_run(&self, run_id: &str) -> Result<AgentRun, AppError> {
        let conn = self.lock()?;
        read_agent_run(&conn, run_id)
    }

    pub fn list_agent_runs(&self, session_id: &str) -> Result<Vec<AgentRun>, AppError> {
        let conn = self.lock()?;
        let sql = format!(
            "SELECT {AGENT_RUN_COLS} FROM agent_runs WHERE session_id = ?1 ORDER BY seq ASC"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![session_id], map_agent_run)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// First-writer-wins, exactly like a terminal run: a natural exit racing
    /// the quit path cannot rewrite the outcome that was actually observed.
    pub fn end_agent_run(&self, run_id: &str, outcome: &str) -> Result<(), AppError> {
        let outcome = validate_text("outcome", outcome, 1, LIMIT_SUMMARY_CHARS, true)?;
        let now = self.clock.now_ms();
        let mut conn = self.lock()?;
        let session_id: Option<String> = conn
            .query_row(
                "SELECT session_id FROM agent_runs WHERE run_id = ?1",
                params![run_id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(session_id) = session_id else {
            return Err(AppError::not_found("agent_run", run_id));
        };
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE agent_runs SET ended_at = COALESCE(ended_at, ?2), \
             outcome = COALESCE(outcome, ?3) WHERE run_id = ?1",
            params![run_id, now, outcome],
        )?;
        end_session_row(&tx, &session_id, now, &outcome)?;
        tx.commit()?;
        Ok(())
    }

    /// Settles every agent run still marked open. Agents do not survive this
    /// application's exit, so an open run at startup is a process that is gone.
    /// Nothing is reattached and no prompt is replayed.
    pub fn reconcile_interrupted_agent_runs(&self) -> Result<usize, AppError> {
        let now = self.clock.now_ms();
        let mut conn = self.lock()?;
        let open: Vec<(String, String)> = {
            let mut stmt =
                conn.prepare("SELECT run_id, session_id FROM agent_runs WHERE ended_at IS NULL")?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            let mut v = Vec::new();
            for row in rows {
                v.push(row?);
            }
            v
        };
        let tx = conn.transaction()?;
        for (run_id, session_id) in &open {
            tx.execute(
                "UPDATE agent_runs SET ended_at = ?2, outcome = COALESCE(outcome, ?3) \
                 WHERE run_id = ?1",
                params![run_id, now, AGENT_OUTCOME_INTERRUPTED],
            )?;
            // A permission request nobody could answer is settled closed, never
            // as an allow.
            tx.execute(
                "UPDATE agent_permission_requests SET resolved_at = ?2, resolution = 'cancelled' \
                 WHERE run_id = ?1 AND resolved_at IS NULL",
                params![run_id, now],
            )?;
            end_session_row(&tx, session_id, now, AGENT_OUTCOME_INTERRUPTED)?;
        }
        tx.commit()?;
        Ok(open.len())
    }

    // --- agent transcript ---------------------------------------------------

    /// Inserts or updates the entry identified by `draft.key` within `run_id`.
    pub fn upsert_agent_message(
        &self,
        run_id: &str,
        draft: AgentMessageDraft<'_>,
    ) -> Result<AgentMessage, AppError> {
        let key = validate_text("key", draft.key, 1, LIMIT_AGENT_KEY_CHARS, true)?;
        let role = validate_text("role", draft.role, 1, LIMIT_KIND_CHARS, true)?;
        // Bodies keep their whitespace: they are verbatim model and tool output.
        let body = validate_text("body", draft.body, 0, LIMIT_AGENT_BODY_CHARS, false)?;
        let detail = match draft.detail {
            None => None,
            Some(d) => Some(validate_text(
                "detail",
                d,
                0,
                LIMIT_AGENT_DETAIL_CHARS,
                false,
            )?),
        };
        let status = match draft.status {
            None => None,
            Some(s) => Some(validate_text("status", s, 0, LIMIT_KIND_CHARS, true)?),
        };
        let now = self.clock.now_ms();
        let id = new_id("msg");
        let conn = self.lock()?;
        require_agent_run(&conn, run_id)?;
        conn.execute(
            "INSERT INTO agent_messages \
                 (id, run_id, key, role, turn, body, detail, status, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9) \
             ON CONFLICT(run_id, key) DO UPDATE SET \
                 role = excluded.role, turn = excluded.turn, body = excluded.body, \
                 detail = excluded.detail, status = excluded.status, \
                 updated_at = excluded.updated_at",
            params![id, run_id, key, role, draft.turn, body, detail, status, now],
        )?;
        let sql = format!(
            "SELECT {AGENT_MESSAGE_COLS} FROM agent_messages WHERE run_id = ?1 AND key = ?2"
        );
        conn.query_row(&sql, params![run_id, key], map_agent_message)
            .optional()?
            .ok_or_else(|| AppError::not_found("agent_message", key))
    }

    /// The newest `limit` entries in chronological order, plus whether older
    /// entries exist. Truncation is reported, never disguised as completeness.
    ///
    /// `limit` is clamped to [`LIMIT_AGENT_LIST_MAX`] here rather than trusted:
    /// a caller-supplied `usize` above `i64::MAX` would otherwise cast negative,
    /// and SQLite treats a negative `LIMIT` as unbounded.
    pub fn list_agent_messages(
        &self,
        run_id: &str,
        limit: usize,
    ) -> Result<(Vec<AgentMessage>, bool), AppError> {
        let limit = limit.min(LIMIT_AGENT_LIST_MAX);
        let conn = self.lock()?;
        require_agent_run(&conn, run_id)?;
        let total: i64 = conn.query_row(
            "SELECT COUNT(*) FROM agent_messages WHERE run_id = ?1",
            params![run_id],
            |r| r.get(0),
        )?;
        let sql = format!(
            "SELECT {AGENT_MESSAGE_COLS} FROM agent_messages WHERE run_id = ?1 \
             ORDER BY seq DESC LIMIT ?2"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![run_id, limit as i64], map_agent_message)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        out.reverse();
        Ok((out, total > limit as i64))
    }

    // --- agent permission requests ------------------------------------------

    pub fn record_permission_request(
        &self,
        id: &str,
        run_id: &str,
        title: &str,
        detail: &str,
        options: &str,
    ) -> Result<AgentPermissionRecord, AppError> {
        let id = validate_text("id", id, 1, LIMIT_LABEL_CHARS, true)?;
        let title = validate_text("title", title, 0, LIMIT_SUMMARY_CHARS, false)?;
        let detail = validate_text("detail", detail, 0, LIMIT_AGENT_DETAIL_CHARS, false)?;
        let options = validate_text("options", options, 1, LIMIT_AGENT_DETAIL_CHARS, false)?;
        let now = self.clock.now_ms();
        let conn = self.lock()?;
        require_agent_run(&conn, run_id)?;
        conn.execute(
            "INSERT INTO agent_permission_requests \
                 (id, run_id, title, detail, options, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id, run_id, title, detail, options, now],
        )?;
        read_permission_request(&conn, &id)
    }

    /// First answer wins. A late cancel cannot overwrite what the user chose,
    /// and an allow can never overwrite a recorded denial.
    pub fn resolve_permission_request(
        &self,
        id: &str,
        resolution: AgentPermissionResolution,
    ) -> Result<AgentPermissionRecord, AppError> {
        let now = self.clock.now_ms();
        let conn = self.lock()?;
        let changed = conn.execute(
            "UPDATE agent_permission_requests SET resolved_at = COALESCE(resolved_at, ?2), \
             resolution = COALESCE(resolution, ?3) WHERE id = ?1",
            params![id, now, resolution.as_record()],
        )?;
        if changed == 0 {
            return Err(AppError::not_found("agent_permission_request", id));
        }
        read_permission_request(&conn, id)
    }

    pub fn get_permission_request(&self, id: &str) -> Result<AgentPermissionRecord, AppError> {
        let conn = self.lock()?;
        read_permission_request(&conn, id)
    }

    // --- review inbox -------------------------------------------------------

    pub fn add_review_item(
        &self,
        thread_id: &str,
        session_id: Option<&str>,
        kind: &str,
        summary: &str,
    ) -> Result<ReviewItem, AppError> {
        let kind = validate_text("kind", kind, 1, LIMIT_KIND_CHARS, true)?;
        let summary = validate_text("summary", summary, 1, LIMIT_SUMMARY_CHARS, true)?;
        let now = self.clock.now_ms();
        let id = new_id("rev");

        let conn = self.lock()?;
        require_thread(&conn, thread_id)?;
        if let Some(sid) = session_id {
            require_session_in_thread(&conn, sid, thread_id)?;
        }
        conn.execute(
            "INSERT INTO review_items (id, thread_id, session_id, kind, summary, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id, thread_id, session_id, kind, summary, now],
        )?;
        read_review_item(&conn, &id)
    }

    pub fn list_review_items(&self, thread_id: &str) -> Result<Vec<ReviewItem>, AppError> {
        let conn = self.lock()?;
        require_thread(&conn, thread_id)?;
        let sql =
            format!("SELECT {REVIEW_COLS} FROM review_items WHERE thread_id = ?1 ORDER BY seq ASC");
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![thread_id], map_review_item)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// How many results are waiting to be looked at, per thread.
    ///
    /// This is what keeps a thread actionable after an agent produces something
    /// new: the thread's own `seen_at`/`reviewed_at` record what a human did and
    /// when, and are never rewritten by an agent, so the unread signal has to
    /// come from the items themselves.
    pub fn unseen_review_counts(&self) -> Result<Vec<UnseenReviewCount>, AppError> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT thread_id, COUNT(*) FROM review_items WHERE seen_at IS NULL \
             GROUP BY thread_id ORDER BY thread_id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(UnseenReviewCount {
                thread_id: r.get(0)?,
                unseen: r.get(1)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn mark_review_item_reviewed(&self, id: &str) -> Result<ReviewItem, AppError> {
        let now = self.clock.now_ms();
        let conn = self.lock()?;
        let exists: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM review_items WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .optional()?;
        if exists.is_none() {
            return Err(AppError::not_found("review_item", id));
        }
        conn.execute(
            "UPDATE review_items SET reviewed_at = ?2, seen_at = COALESCE(seen_at, ?2) WHERE id = ?1",
            params![id, now],
        )?;
        read_review_item(&conn, id)
    }
}

// --- helpers ----------------------------------------------------------------

fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", Uuid::new_v4().simple())
}

/// Character-counted (not byte-counted) bounds, so multi-byte titles are not
/// silently rejected.
fn validate_text(
    field: &str,
    value: &str,
    min_chars: usize,
    max_chars: usize,
    trim: bool,
) -> Result<String, AppError> {
    let value = if trim { value.trim() } else { value };
    let len = value.chars().count();
    if len < min_chars {
        return Err(AppError::validation(
            field,
            if min_chars == 1 {
                "must not be blank".to_string()
            } else {
                format!("must be at least {min_chars} characters")
            },
        ));
    }
    if len > max_chars {
        return Err(AppError::validation(
            field,
            format!("must be at most {max_chars} characters (got {len})"),
        ));
    }
    Ok(value.to_string())
}

/// First-writer-wins end. A session records how it ended once; a later,
/// contradictory outcome is dropped rather than overwriting the observed fact.
fn end_session_row(
    conn: &Connection,
    session_id: &str,
    now: i64,
    outcome: &str,
) -> Result<(), AppError> {
    conn.execute(
        "UPDATE sessions SET ended_at = COALESCE(ended_at, ?2), \
         outcome = COALESCE(outcome, ?3) WHERE id = ?1",
        params![session_id, now, outcome],
    )?;
    Ok(())
}

fn require_thread(conn: &Connection, id: &str) -> Result<(), AppError> {
    let found: Option<i64> = conn
        .query_row("SELECT 1 FROM threads WHERE id = ?1", params![id], |r| {
            r.get(0)
        })
        .optional()?;
    found
        .map(|_| ())
        .ok_or_else(|| AppError::not_found("thread", id))
}

fn require_session(conn: &Connection, id: &str) -> Result<Session, AppError> {
    let sql = format!("SELECT {SESSION_COLS} FROM sessions WHERE id = ?1");
    conn.query_row(&sql, params![id], map_session)
        .optional()?
        .ok_or_else(|| AppError::not_found("session", id))
}

/// Fails closed on both unknown ids and ids belonging to a different thread, so
/// lineage can never cross threads.
fn require_session_in_thread(
    conn: &Connection,
    id: &str,
    thread_id: &str,
) -> Result<Session, AppError> {
    let session = require_session(conn, id)?;
    if session.thread_id != thread_id {
        return Err(AppError::validation(
            "predecessor_id",
            format!("session {id} belongs to a different thread"),
        ));
    }
    Ok(session)
}

fn read_thread(conn: &Connection, id: &str) -> Result<Thread, AppError> {
    let sql = format!("SELECT {THREAD_COLS} FROM threads WHERE id = ?1");
    conn.query_row(&sql, params![id], map_thread)
        .optional()?
        .ok_or_else(|| AppError::not_found("thread", id))
}

fn read_session(conn: &Connection, id: &str) -> Result<Session, AppError> {
    require_session(conn, id)
}

fn read_review_item(conn: &Connection, id: &str) -> Result<ReviewItem, AppError> {
    let sql = format!("SELECT {REVIEW_COLS} FROM review_items WHERE id = ?1");
    conn.query_row(&sql, params![id], map_review_item)
        .optional()?
        .ok_or_else(|| AppError::not_found("review_item", id))
}

fn require_agent_run(conn: &Connection, run_id: &str) -> Result<(), AppError> {
    let found: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM agent_runs WHERE run_id = ?1",
            params![run_id],
            |r| r.get(0),
        )
        .optional()?;
    found
        .map(|_| ())
        .ok_or_else(|| AppError::not_found("agent_run", run_id))
}

fn read_agent_run(conn: &Connection, run_id: &str) -> Result<AgentRun, AppError> {
    let sql = format!("SELECT {AGENT_RUN_COLS} FROM agent_runs WHERE run_id = ?1");
    conn.query_row(&sql, params![run_id], map_agent_run)
        .optional()?
        .ok_or_else(|| AppError::not_found("agent_run", run_id))
}

fn read_permission_request(conn: &Connection, id: &str) -> Result<AgentPermissionRecord, AppError> {
    let sql =
        format!("SELECT {AGENT_PERMISSION_COLS} FROM agent_permission_requests WHERE id = ?1");
    conn.query_row(&sql, params![id], map_permission_request)
        .optional()?
        .ok_or_else(|| AppError::not_found("agent_permission_request", id))
}

fn map_agent_run(row: &Row<'_>) -> rusqlite::Result<AgentRun> {
    Ok(AgentRun {
        run_id: row.get(0)?,
        session_id: row.get(1)?,
        program: row.get(2)?,
        cwd: row.get(3)?,
        acp_session_id: row.get(4)?,
        started_at: row.get(5)?,
        ended_at: row.get(6)?,
        outcome: row.get(7)?,
    })
}

fn map_agent_message(row: &Row<'_>) -> rusqlite::Result<AgentMessage> {
    Ok(AgentMessage {
        id: row.get(0)?,
        run_id: row.get(1)?,
        key: row.get(2)?,
        role: row.get(3)?,
        turn: row.get(4)?,
        body: row.get(5)?,
        detail: row.get(6)?,
        status: row.get(7)?,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
    })
}

fn map_permission_request(row: &Row<'_>) -> rusqlite::Result<AgentPermissionRecord> {
    Ok(AgentPermissionRecord {
        id: row.get(0)?,
        run_id: row.get(1)?,
        title: row.get(2)?,
        detail: row.get(3)?,
        options: row.get(4)?,
        created_at: row.get(5)?,
        resolved_at: row.get(6)?,
        resolution: row.get(7)?,
    })
}

fn map_thread(row: &Row<'_>) -> rusqlite::Result<Thread> {
    Ok(Thread {
        id: row.get(0)?,
        title: row.get(1)?,
        notes: row.get(2)?,
        workdir: row.get(3)?,
        created_at: row.get(4)?,
        updated_at: row.get(5)?,
        seen_at: row.get(6)?,
        reviewed_at: row.get(7)?,
    })
}

fn map_session(row: &Row<'_>) -> rusqlite::Result<Session> {
    Ok(Session {
        id: row.get(0)?,
        thread_id: row.get(1)?,
        label: row.get(2)?,
        kind: row.get(3)?,
        predecessor_id: row.get(4)?,
        briefing: row.get(5)?,
        created_at: row.get(6)?,
        ended_at: row.get(7)?,
        outcome: row.get(8)?,
    })
}

fn map_review_item(row: &Row<'_>) -> rusqlite::Result<ReviewItem> {
    Ok(ReviewItem {
        id: row.get(0)?,
        thread_id: row.get(1)?,
        session_id: row.get(2)?,
        kind: row.get(3)?,
        summary: row.get(4)?,
        created_at: row.get(5)?,
        seen_at: row.get(6)?,
        reviewed_at: row.get(7)?,
    })
}

fn compose_handoff_text(thread: &Thread, predecessor: Option<&Session>, now: i64) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Handoff draft — {}\n", thread.title));
    out.push_str(&format!("Generated {} (UTC)\n\n", format_utc(now)));
    out.push_str(HANDOFF_DISCLOSURE);
    out.push_str("\n\n## Thread notes\n");
    out.push_str(if thread.notes.trim().is_empty() {
        "(no notes recorded)"
    } else {
        &thread.notes
    });
    out.push_str("\n\n## Predecessor session\n");
    match predecessor {
        Some(s) => {
            out.push_str(&format!(
                "{} ({}), started {} (UTC), id {}\n\n",
                s.label,
                s.kind,
                format_utc(s.created_at),
                s.id
            ));
            out.push_str(if s.briefing.trim().is_empty() {
                "(no briefing recorded on the predecessor)"
            } else {
                &s.briefing
            });
        }
        None => out.push_str("(none — this draft starts a new line of work)"),
    }
    out.push_str("\n\n## Environment\n");
    match &thread.workdir {
        Some(dir) => out.push_str(&format!(
            "Directory selected for this thread: {dir}\n\
             Provenance: user selection recorded in Faiden. Not observed from any running process.",
        )),
        None => out.push_str("No directory bound to this thread."),
    }
    out.push_str(
        "\n\n## Carry forward (edit before use)\n\
         - Goal:\n\
         - Decisions made:\n\
         - Approaches rejected:\n\
         - Open tasks:\n\
         - Verified evidence (link artifacts, do not assert success):\n\
         - Active work still running:\n",
    );
    out
}

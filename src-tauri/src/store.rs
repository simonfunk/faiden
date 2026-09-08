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

/// Stated verbatim on every generated handoff draft. This slice has no model
/// integration at all, and the UI must never imply otherwise.
/// Recorded for a run whose end was never observed, because the process that
/// owned it exited without saying how. It is deliberately not a success claim.
pub const OUTCOME_INTERRUPTED: &str =
    "unknown — Faiden exited without recording how this terminal ended";

pub const HANDOFF_DISCLOSURE: &str = "This is a human-authored handoff draft. It does not resume an AI context: no model session was replayed, continued or contacted.";

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
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

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

const THREAD_COLS: &str = "id, title, notes, workdir, created_at, updated_at, seen_at, reviewed_at";
const SESSION_COLS: &str =
    "id, thread_id, label, kind, predecessor_id, briefing, created_at, ended_at, outcome";
const REVIEW_COLS: &str =
    "id, thread_id, session_id, kind, summary, created_at, seen_at, reviewed_at";

impl Store {
    pub fn open(path: &Path) -> Result<Store, AppError> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let conn = Connection::open(path)?;
        conn.execute_batch(SCHEMA)?;
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

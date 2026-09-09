//! One owned `hermes acp` child process per agent session, and the observable
//! state derived from what it actually says.
//!
//! Design notes the tests pin down:
//!
//! * **One subprocess per session.** Hermes binds cwd, session key and the
//!   approval callback inside one process, and several of its caches are
//!   process-global. A private child is the only way two Faiden threads cannot
//!   contaminate each other's safety state.
//! * **Status is derived from protocol events, never from liveness.** A running
//!   child that has not completed `initialize` *and* `session/new` is
//!   `connecting`, not `ready`.
//! * **Every event carries `epoch` and `seq`.** A view subscribes, then
//!   snapshots, and discards anything below the snapshot's `next_seq` — the same
//!   discipline `terminal.rs` uses.
//! * **Permissions fail closed.** Timeout, cancel, disconnect and quit all
//!   answer `{"outcome":"cancelled"}`. No path turns silence into an allow.
//! * **`turn_complete` is not success.** It records the literal `stopReason`.
//!
//! # Lock order
//!
//! `runs` → `state` → `pending`/`permission` → `writer`, and `state` → the
//! store. The registry lock is taken only to clone an `Arc<AgentRun>` and is
//! released before any IO or wait. Nothing blocks on a pipe while holding a
//! lock another thread needs to make progress. `state` is the lock every
//! observable mutation is made under, which is what lets `snapshot` take one
//! coherent cut of transcript, permission and cursor.

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::agent::locate;
use crate::agent::protocol::{self, Incoming, PermissionDecision, ProtocolError, StopReason};
use crate::clock::MonotonicClock;
use crate::error::AppError;
use crate::store::{
    AgentMessage, AgentMessageDraft, AgentPermissionResolution, Store, LIMIT_AGENT_BODY_CHARS,
    LIMIT_SUMMARY_CHARS,
};

/// The one reason a permission resolution is *not* worth filing for review:
/// the user answered it themselves and already knows.
const ANSWERED_BY_USER: &str = "answered by you";

/// Transcript entries kept for one run. Older entries stay in SQLite; the view
/// is told it is looking at a truncated tail rather than a whole history.
pub const MAX_TRANSCRIPT_ENTRIES: usize = 500;
/// Bounded stderr diagnostics. stdout is protocol-only; stderr is where the
/// adapter logs, and it must never grow without limit.
pub const MAX_STDERR_LINES: usize = 200;
pub const MAX_STDERR_LINE_CHARS: usize = 2_000;
/// Backstop for a single turn. Turns are legitimately long, so this is only a
/// last resort against a waiter thread parked forever; EOF and `stop` are the
/// normal ways a wait ends.
pub const TURN_BACKSTOP: Duration = Duration::from_secs(6 * 60 * 60);
/// Stated in the UI. Faiden hosts the process; it does not contain it.
pub const AUTHORITY_DISCLOSURE: &str = "This agent runs the Hermes you installed, with your user's authority, your Hermes configuration and your Hermes tools, including any MCP servers you configured globally. It is not a sandbox. Faiden removes inherited approval-bypass variables before starting it, but a bypass configured inside your own Hermes config or .env is loaded by Hermes itself and is outside Faiden's control.";

/// How long a Faiden-side operation may wait. Hermes auto-denies a permission
/// request after 60 s of its own, so `permission` is deliberately shorter: the
/// answer the user sees must be the answer that lands.
#[derive(Debug, Clone, Copy)]
pub struct AgentTimeouts {
    pub handshake: Duration,
    pub permission: Duration,
    pub shutdown: Duration,
}

impl Default for AgentTimeouts {
    fn default() -> Self {
        AgentTimeouts {
            handshake: Duration::from_secs(45),
            permission: Duration::from_secs(45),
            shutdown: Duration::from_secs(5),
        }
    }
}

/// What to start. The Tauri command layer never forwards a caller-supplied
/// `program` or `args`: it resolves the executable itself, exactly as
/// `terminal.rs` refuses to let web content choose a command.
#[derive(Debug, Clone)]
pub struct AgentSpec {
    /// The Faiden session this run belongs to.
    pub session_id: String,
    pub program: PathBuf,
    pub args: Vec<String>,
    /// Absolute. `session/new` is told this same directory.
    pub cwd: PathBuf,
    /// Extra variables merged over the inherited environment. Approval-bypass
    /// names are stripped from the *result*, so nothing here can reintroduce
    /// one.
    pub env: Vec<(String, String)>,
    /// Where the executable was found. Provenance for display.
    pub source: String,
    pub timeouts: AgentTimeouts,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AgentStatus {
    /// The child is alive but the handshake has not finished. Never "ready".
    Connecting,
    Ready,
    Responding,
    AwaitingPermission,
    /// A cancel has been sent; the turn has not returned yet.
    Cancelling,
    /// A turn returned a `stopReason`. Not a claim that anything succeeded.
    TurnComplete,
    Disconnected,
    Error,
}

impl AgentStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentStatus::Connecting => "connecting",
            AgentStatus::Ready => "ready",
            AgentStatus::Responding => "responding",
            AgentStatus::AwaitingPermission => "awaitingPermission",
            AgentStatus::Cancelling => "cancelling",
            AgentStatus::TurnComplete => "turnComplete",
            AgentStatus::Disconnected => "disconnected",
            AgentStatus::Error => "error",
        }
    }
}

/// A permission request awaiting a human answer. `request_id` is Faiden's own
/// id: the webview never learns, and never supplies, a JSON-RPC id.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PermissionPrompt {
    pub request_id: String,
    pub run_id: String,
    pub tool_call_id: String,
    pub title: String,
    pub detail: String,
    pub options: Vec<protocol::PermissionOption>,
    pub created_at: i64,
    /// When Faiden will answer `cancelled` on the user's behalf.
    pub expires_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentRunInfo {
    pub run_id: String,
    pub session_id: String,
    pub epoch: u64,
    pub pid: Option<u32>,
    pub program: String,
    /// Where the executable was found.
    pub source: String,
    /// The directory the child was started in and that `session/new` was told.
    /// Not an observed tool cwd.
    pub cwd: String,
    pub acp_session_id: Option<String>,
    pub status: AgentStatus,
    /// The agent's own words for the last thing that happened.
    pub detail: Option<String>,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub prompt_in_flight: bool,
    pub disclosure: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSnapshot {
    pub info: AgentRunInfo,
    pub messages: Vec<AgentMessage>,
    /// True when older entries exist that this snapshot does not include.
    pub truncated: bool,
    pub pending_permission: Option<PermissionPrompt>,
    pub next_seq: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentDiagnostics {
    pub run_id: String,
    /// Replies whose id we had not issued, or had already settled.
    pub unmatched_replies: u64,
    /// Lines that were not usable protocol: malformed, alien or oversized.
    pub protocol_faults: u64,
    /// Requests refused fail-closed: a session this run does not own, no turn
    /// in flight, or a run that has already ended.
    pub refused_requests: u64,
    /// `session/update` notifications that belonged to no live turn and were
    /// therefore not written to any transcript entry.
    pub unattributed_updates: u64,
    /// Bounded tail of the child's stderr. Diagnostics, not protocol.
    pub stderr_tail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum AgentEvent {
    #[serde(rename_all = "camelCase")]
    Status {
        run_id: String,
        epoch: u64,
        seq: u64,
        status: AgentStatus,
        detail: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Message {
        run_id: String,
        epoch: u64,
        seq: u64,
        message: AgentMessage,
    },
    #[serde(rename_all = "camelCase")]
    Permission {
        run_id: String,
        epoch: u64,
        seq: u64,
        request: PermissionPrompt,
    },
    #[serde(rename_all = "camelCase")]
    PermissionResolved {
        run_id: String,
        epoch: u64,
        seq: u64,
        request_id: String,
        resolution: String,
    },
    #[serde(rename_all = "camelCase")]
    Ended {
        run_id: String,
        epoch: u64,
        seq: u64,
        outcome: String,
    },
}

impl AgentEvent {
    pub fn run_id(&self) -> &str {
        match self {
            AgentEvent::Status { run_id, .. }
            | AgentEvent::Message { run_id, .. }
            | AgentEvent::Permission { run_id, .. }
            | AgentEvent::PermissionResolved { run_id, .. }
            | AgentEvent::Ended { run_id, .. } => run_id,
        }
    }

    pub fn seq(&self) -> u64 {
        match self {
            AgentEvent::Status { seq, .. }
            | AgentEvent::Message { seq, .. }
            | AgentEvent::Permission { seq, .. }
            | AgentEvent::PermissionResolved { seq, .. }
            | AgentEvent::Ended { seq, .. } => *seq,
        }
    }
}

pub trait AgentEvents: Send + Sync + 'static {
    fn emit(&self, event: AgentEvent);
}

/// Used where events are not wanted.
pub struct NoAgentEvents;

impl AgentEvents for NoAgentEvents {
    fn emit(&self, _event: AgentEvent) {}
}

// --- internals --------------------------------------------------------------

/// One JSON-RPC request awaiting its reply.
type Slot = Arc<(Mutex<Option<Result<Value, String>>>, Condvar)>;

#[derive(Debug, Default, Clone)]
struct ToolEntry {
    title: Option<String>,
    kind: Option<String>,
    status: Option<String>,
    content: String,
}

impl ToolEntry {
    fn body(&self) -> String {
        let title = self
            .title
            .clone()
            .unwrap_or_else(|| "Tool call".to_string());
        let head = match &self.kind {
            Some(kind) => format!("{title} [{kind}]"),
            None => title,
        };
        if self.content.trim().is_empty() {
            head
        } else {
            format!("{head}\n{}", self.content)
        }
    }
}

struct PendingPermission {
    prompt: PermissionPrompt,
    /// The agent's JSON-RPC id, kept native-side only.
    rpc_id: Value,
}

struct RunState {
    status: AgentStatus,
    detail: Option<String>,
    acp_session_id: Option<String>,
    prompt_in_flight: bool,
    /// Set the moment a shutdown begins, before the child is torn down.
    /// `ended_at` cannot serve this purpose: it is `settle`'s first-writer
    /// guard, and settling happens only once the child is actually gone. Frames
    /// already buffered in the pipe keep arriving in between, and none of them
    /// may become something the user is asked to approve.
    closing: bool,
    /// The terminal path that got to claim settlement first. This is separate
    /// from `ended_at`: claiming happens before a claimant tears a child down,
    /// while `ended_at` is only written after its durable outcome is ready.
    settlement_claimed: bool,
    ended_at: Option<i64>,
    next_seq: u64,
    next_request_id: u64,
    turn: i64,
    agent_body: String,
    thought_body: String,
    tools: HashMap<String, ToolEntry>,
    unmatched_replies: u64,
    protocol_faults: u64,
    refused_requests: u64,
    unattributed_updates: u64,
    stderr_tail: VecDeque<String>,
}

struct AgentRun {
    run_id: String,
    session_id: String,
    epoch: u64,
    pid: Option<u32>,
    program: String,
    source: String,
    cwd: String,
    started_at: i64,
    timeouts: AgentTimeouts,
    state: Mutex<RunState>,
    pending: Mutex<HashMap<u64, Slot>>,
    permission: Mutex<Option<PendingPermission>>,
    writer: Mutex<Option<ChildStdin>>,
    child: Mutex<Option<Child>>,
    events: Arc<dyn AgentEvents>,
    store: Arc<Store>,
    clock: Arc<MonotonicClock>,
}

impl AgentRun {
    fn info_locked(&self, state: &RunState) -> AgentRunInfo {
        AgentRunInfo {
            run_id: self.run_id.clone(),
            session_id: self.session_id.clone(),
            epoch: self.epoch,
            pid: self.pid,
            program: self.program.clone(),
            source: self.source.clone(),
            cwd: self.cwd.clone(),
            acp_session_id: state.acp_session_id.clone(),
            status: state.status,
            detail: state.detail.clone(),
            started_at: self.started_at,
            ended_at: state.ended_at,
            prompt_in_flight: state.prompt_in_flight,
            disclosure: AUTHORITY_DISCLOSURE.to_string(),
        }
    }

    fn info(&self) -> AgentRunInfo {
        let state = self.state.lock().expect("agent state mutex");
        self.info_locked(&state)
    }

    /// Assigns a sequence number and publishes the event under one held lock,
    /// so a snapshot can never report a cursor that skips an event.
    fn emit(&self, state: &mut RunState, make: impl FnOnce(u64) -> AgentEvent) {
        let seq = state.next_seq;
        state.next_seq += 1;
        self.events.emit(make(seq));
    }

    /// A settled run's status is frozen: a turn waiter waking up after the
    /// child died must not repaint the run as though it were still working.
    fn set_status(&self, state: &mut RunState, status: AgentStatus, detail: Option<String>) {
        if state.ended_at.is_some() {
            return;
        }
        self.force_status(state, status, detail)
    }

    fn force_status(&self, state: &mut RunState, status: AgentStatus, detail: Option<String>) {
        state.status = status;
        if detail.is_some() {
            state.detail = detail.clone();
        }
        let run_id = self.run_id.clone();
        let epoch = self.epoch;
        self.emit(state, move |seq| AgentEvent::Status {
            run_id,
            epoch,
            seq,
            status,
            detail,
        });
    }

    /// Writes one newline-terminated frame. Serialized: two threads can be
    /// answering a permission and sending a cancel at the same moment.
    fn send(&self, frame: &str) -> Result<(), AppError> {
        let mut guard = self.writer.lock().expect("agent writer mutex");
        let Some(stdin) = guard.as_mut() else {
            return Err(AppError::conflict(
                "AGENT_DISCONNECTED",
                "the agent process is no longer accepting input",
            ));
        };
        stdin
            .write_all(frame.as_bytes())
            .and_then(|_| stdin.write_all(b"\n"))
            .and_then(|_| stdin.flush())
            .map_err(|e| {
                AppError::conflict(
                    "AGENT_DISCONNECTED",
                    format!("writing to the agent failed: {e}"),
                )
            })
    }

    fn next_request_id(&self) -> u64 {
        let mut state = self.state.lock().expect("agent state mutex");
        let id = state.next_request_id;
        state.next_request_id += 1;
        id
    }

    /// Sends a request and waits for its reply. The pending slot is registered
    /// *before* the frame goes out, so a reply cannot arrive unmatched.
    fn request(&self, id: u64, frame: String, timeout: Duration) -> Result<Value, AppError> {
        let slot: Slot = Arc::new((Mutex::new(None), Condvar::new()));
        self.pending
            .lock()
            .expect("agent pending mutex")
            .insert(id, slot.clone());

        if let Err(e) = self.send(&frame) {
            self.pending
                .lock()
                .expect("agent pending mutex")
                .remove(&id);
            return Err(e);
        }

        let deadline = Instant::now() + timeout;
        let (lock, condvar) = &*slot;
        let mut answer = lock.lock().expect("agent slot mutex");
        while answer.is_none() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                drop(answer);
                self.pending
                    .lock()
                    .expect("agent pending mutex")
                    .remove(&id);
                return Err(AppError::conflict(
                    "AGENT_TIMEOUT",
                    format!("the agent did not answer request {id} within {timeout:?}"),
                ));
            }
            let (guard, _) = condvar
                .wait_timeout(answer, remaining)
                .expect("agent slot condvar");
            answer = guard;
        }
        match answer.take().expect("settled slot") {
            Ok(result) => Ok(result),
            Err(message) => Err(AppError::conflict("AGENT_REJECTED", message)),
        }
    }

    fn settle_slot(&self, id: u64, outcome: Result<Value, String>) -> bool {
        let slot = self
            .pending
            .lock()
            .expect("agent pending mutex")
            .remove(&id);
        let Some(slot) = slot else {
            return false;
        };
        let (lock, condvar) = &*slot;
        *lock.lock().expect("agent slot mutex") = Some(outcome);
        condvar.notify_all();
        true
    }

    fn fail_all_pending(&self, message: &str) {
        let pending: Vec<Slot> = self
            .pending
            .lock()
            .expect("agent pending mutex")
            .drain()
            .map(|(_, slot)| slot)
            .collect();
        for slot in pending {
            let (lock, condvar) = &*slot;
            *lock.lock().expect("agent slot mutex") = Some(Err(message.to_string()));
            condvar.notify_all();
        }
    }

    /// Records a transcript entry and publishes it. Bodies are bounded before
    /// they reach the store; the bound is a hard limit, not a silent trim.
    fn record(&self, state: &mut RunState, draft: AgentMessageDraft<'_>) {
        let bounded = bound_body(draft.body);
        let draft = AgentMessageDraft {
            body: &bounded,
            ..draft
        };
        match self.store.upsert_agent_message(&self.run_id, draft) {
            Ok(message) => {
                let run_id = self.run_id.clone();
                let epoch = self.epoch;
                self.emit(state, move |seq| AgentEvent::Message {
                    run_id,
                    epoch,
                    seq,
                    message,
                });
            }
            Err(e) => {
                // Losing a transcript entry must be visible, not silent: the
                // "durable history" claim would otherwise be false with no trace.
                eprintln!(
                    "faiden: failed to persist agent transcript for {}: {e}",
                    self.run_id
                );
            }
        }
    }

    /// Files something the user should read into the thread's review inbox.
    /// New items are unseen and unreviewed: opening a thread is not reviewing
    /// it, and neither is an agent finishing a turn.
    fn record_result(&self, kind: &str, summary: &str) {
        let Ok(session) = self.store.get_session(&self.session_id) else {
            return;
        };
        let bounded: String = summary.chars().take(LIMIT_SUMMARY_CHARS).collect();
        if let Err(e) =
            self.store
                .add_review_item(&session.thread_id, Some(&self.session_id), kind, &bounded)
        {
            eprintln!("faiden: failed to record an agent result for review: {e}");
        }
    }

    /// Answers a pending permission with `decision`, whatever the reason. Every
    /// fail-closed path — timeout, cancel, stop, disconnect — funnels here.
    ///
    /// Lock order is state -> permission -> writer. Keeping the state lock until
    /// the response is sent makes a selected permission and a terminal claim one
    /// serialized decision: shutdown cannot begin between an allow's validation
    /// and its wire response.
    fn resolve_permission(
        &self,
        decision: PermissionDecision,
        why: &str,
    ) -> Result<Option<String>, AppError> {
        let mut state = self.state.lock().expect("agent state mutex");
        let taken = self
            .permission
            .lock()
            .expect("agent permission mutex")
            .take();
        let Some(pending) = taken else {
            return Ok(None);
        };
        self.resolve_taken_permission(&mut state, pending, decision, why)
    }

    /// Resolves a pending request whose ownership was removed while holding the
    /// state mutex. Callers must retain that mutex until this returns.
    fn resolve_taken_permission(
        &self,
        state: &mut RunState,
        pending: PendingPermission,
        decision: PermissionDecision,
        why: &str,
    ) -> Result<Option<String>, AppError> {
        let stored = match decision {
            PermissionDecision::Selected { ref option_id } => AgentPermissionResolution::Selected {
                option_id: option_id.clone(),
            },
            PermissionDecision::Cancelled => AgentPermissionResolution::Cancelled,
        };
        let mut decision = decision;
        let mut why = why.to_string();
        let mut audit_failure: Option<AppError> = None;

        if let Err(e) = self
            .store
            .resolve_permission_request(&pending.prompt.request_id, stored.clone())
        {
            if matches!(decision, PermissionDecision::Selected { .. }) {
                decision = PermissionDecision::Cancelled;
                why = format!("the approval could not be recorded: {}", e.message());
                audit_failure = Some(AppError::conflict(
                    "PERMISSION_NOT_RECORDED",
                    format!(
                        "the approval could not be recorded, so the agent was told \
                         \"not allowed\" instead: {}",
                        e.message()
                    ),
                ));
            } else {
                eprintln!(
                    "faiden: failed to record a denied permission for {}: {e}",
                    pending.prompt.request_id
                );
            }
        }
        let recorded = match decision {
            PermissionDecision::Selected { .. } => stored.as_record(),
            PermissionDecision::Cancelled => AgentPermissionResolution::Cancelled.as_record(),
        };

        let frame = protocol::permission_response(&pending.rpc_id, decision.clone());
        // Best effort on the wire: if the child is already gone the record still
        // has to be settled, and it is settled closed.
        let _ = self.send(&frame);
        let why = why.as_str();

        // A request Faiden had to answer *for* the user is something they need
        // to know about: the agent asked, and it was not allowed.
        if matches!(decision, PermissionDecision::Cancelled) && why != ANSWERED_BY_USER {
            self.record_result(
                "agent-permission",
                &format!(
                    "Answered \"not allowed\" without you ({why}): {}",
                    pending.prompt.title
                ),
            );
        }

        let run_id = self.run_id.clone();
        let epoch = self.epoch;
        let request_id = pending.prompt.request_id.clone();
        let resolution = format!("{recorded} ({why})");
        self.emit(state, move |seq| AgentEvent::PermissionResolved {
            run_id,
            epoch,
            seq,
            request_id,
            resolution,
        });
        if state.status == AgentStatus::AwaitingPermission {
            let next = if state.prompt_in_flight {
                AgentStatus::Responding
            } else {
                AgentStatus::Ready
            };
            self.set_status(state, next, None);
        }
        match audit_failure {
            Some(e) => Err(e),
            None => Ok(Some(pending.prompt.request_id)),
        }
    }

    /// Claims the terminal outcome before doing teardown or durable work. This
    /// keeps first-claimant-wins semantics even while the winner reaps a child.
    fn claim_settlement(&self) -> bool {
        let mut state = self.state.lock().expect("agent state mutex");
        if state.settlement_claimed || state.ended_at.is_some() {
            return false;
        }
        state.settlement_claimed = true;
        true
    }

    /// Completes a settlement whose ownership was already claimed.
    fn settle_claimed(&self, outcome: String, final_status: AgentStatus) {
        // The permission is denied before `ended_at` is set, so the resolution
        // still publishes its event and the user sees why it closed.
        let _ = self.resolve_permission(
            PermissionDecision::Cancelled,
            "the agent session ended before it was answered",
        );
        {
            let mut state = self.state.lock().expect("agent state mutex");
            debug_assert!(state.settlement_claimed);
            state.ended_at = Some(self.clock.now_ms());
            state.prompt_in_flight = false;
        }
        self.fail_all_pending(&outcome);
        let _ = self.store.end_agent_run(&self.run_id, &outcome);

        let mut state = self.state.lock().expect("agent state mutex");
        self.force_status(&mut state, final_status, Some(outcome.clone()));
        let run_id = self.run_id.clone();
        let epoch = self.epoch;
        self.emit(&mut state, move |seq| AgentEvent::Ended {
            run_id,
            epoch,
            seq,
            outcome,
        });
    }

    /// Claims terminal ownership and marks the run closing in one critical
    /// section. Approval resolution also holds this lock through its wire send.
    fn claim_settlement_and_begin_closing(&self) -> bool {
        let mut state = self.state.lock().expect("agent state mutex");
        if state.settlement_claimed || state.ended_at.is_some() {
            return false;
        }
        state.settlement_claimed = true;
        state.closing = true;
        true
    }

    /// Closes stdin, waits a bounded grace period, then kills and reaps.
    fn terminate_child(&self) -> String {
        *self.writer.lock().expect("agent writer mutex") = None;
        let mut guard = self.child.lock().expect("agent child mutex");
        let Some(child) = guard.as_mut() else {
            return "the agent process was already gone".to_string();
        };
        let deadline = Instant::now() + self.timeouts.shutdown;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    let text = format!("the agent process exited ({status})");
                    *guard = None;
                    return text;
                }
                Ok(None) => {}
                Err(e) => return format!("could not wait for the agent process: {e}"),
            }
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = child.kill();
        let status = child.wait();
        *guard = None;
        match status {
            Ok(status) => format!("the agent process was forcibly closed ({status})"),
            Err(e) => format!("the agent process could not be reaped: {e}"),
        }
    }
}

fn bound_body(body: &str) -> String {
    if body.chars().count() <= LIMIT_AGENT_BODY_CHARS {
        return body.to_string();
    }
    // Keep the newest text: that is what a reader is watching.
    let skip = body.chars().count() - LIMIT_AGENT_BODY_CHARS + 200;
    let tail: String = body.chars().skip(skip).collect();
    format!(
        "[earlier output dropped: this entry exceeded {LIMIT_AGENT_BODY_CHARS} characters]\n{tail}"
    )
}

// --- manager ----------------------------------------------------------------

pub struct AgentManager {
    runs: Mutex<HashMap<String, Arc<AgentRun>>>,
    events: Arc<dyn AgentEvents>,
    store: Arc<Store>,
    clock: Arc<MonotonicClock>,
    epochs: AtomicU64,
}

impl AgentManager {
    pub fn new(events: Arc<dyn AgentEvents>, store: Arc<Store>) -> AgentManager {
        AgentManager {
            runs: Mutex::new(HashMap::new()),
            events,
            store,
            clock: Arc::new(MonotonicClock::new()),
            epochs: AtomicU64::new(0),
        }
    }

    fn get(&self, run_id: &str) -> Result<Arc<AgentRun>, AppError> {
        self.runs
            .lock()
            .expect("agent registry mutex")
            .get(run_id)
            .cloned()
            .ok_or_else(|| AppError::not_found("agent_run", run_id))
    }

    /// Spawns the child and returns immediately with status `connecting`. The
    /// handshake runs on its own thread, so the caller never blocks on IO and
    /// the UI can honestly show a session that is still starting.
    pub fn start(&self, spec: AgentSpec) -> Result<AgentRunInfo, AppError> {
        if !spec.cwd.is_absolute() {
            return Err(AppError::validation(
                "cwd",
                "an agent session needs an absolute working directory",
            ));
        }
        if !spec.cwd.is_dir() {
            return Err(AppError::validation(
                "cwd",
                format!("{} is not an existing directory", spec.cwd.display()),
            ));
        }
        // One live run per session, exactly as one session owns one terminal.
        {
            let runs = self.runs.lock().expect("agent registry mutex");
            if runs.values().any(|r| {
                r.session_id == spec.session_id
                    && r.state
                        .lock()
                        .expect("agent state mutex")
                        .ended_at
                        .is_none()
            }) {
                return Err(AppError::conflict(
                    "AGENT_ALREADY_RUNNING",
                    "this session already owns a running agent",
                ));
            }
        }

        let run_id = format!("agr_{}", Uuid::new_v4().simple());
        let program = spec.program.to_string_lossy().to_string();
        let cwd = spec.cwd.to_string_lossy().to_string();
        // Durable before the child exists, so an agent can never run unowned.
        self.store
            .start_agent_run(&run_id, &spec.session_id, &program, Some(&cwd))?;

        let mut child = match self.spawn(&spec) {
            Ok(child) => child,
            Err(e) => {
                // A start that never began is settled now, not left open until
                // the next restart.
                let _ = self
                    .store
                    .end_agent_run(&run_id, &format!("the agent process could not start: {e}"));
                return Err(e);
            }
        };
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let pid = Some(child.id());
        let epoch = self.epochs.fetch_add(1, Ordering::SeqCst) + 1;

        let run = Arc::new(AgentRun {
            run_id: run_id.clone(),
            session_id: spec.session_id.clone(),
            epoch,
            pid,
            program,
            source: spec.source.clone(),
            cwd: cwd.clone(),
            started_at: self.clock.now_ms(),
            timeouts: spec.timeouts,
            state: Mutex::new(RunState {
                status: AgentStatus::Connecting,
                detail: None,
                acp_session_id: None,
                prompt_in_flight: false,
                closing: false,
                settlement_claimed: false,
                ended_at: None,
                next_seq: 1,
                next_request_id: 1,
                turn: 0,
                agent_body: String::new(),
                thought_body: String::new(),
                tools: HashMap::new(),
                unmatched_replies: 0,
                protocol_faults: 0,
                refused_requests: 0,
                unattributed_updates: 0,
                stderr_tail: VecDeque::new(),
            }),
            pending: Mutex::new(HashMap::new()),
            permission: Mutex::new(None),
            writer: Mutex::new(stdin),
            child: Mutex::new(Some(child)),
            events: self.events.clone(),
            store: self.store.clone(),
            clock: self.clock.clone(),
        });

        self.runs
            .lock()
            .expect("agent registry mutex")
            .insert(run_id.clone(), run.clone());

        {
            let mut state = run.state.lock().expect("agent state mutex");
            run.set_status(&mut state, AgentStatus::Connecting, None);
        }

        // A worker that cannot be created leaves a live child with nobody
        // reading it. Every failure below therefore kills and reaps the child,
        // settles the durable row with a truthful outcome and drops the run from
        // the registry, rather than propagating an error over an orphan.
        if let Some(stdout) = stdout {
            let reader = run.clone();
            if let Err(e) = spawn_worker(format!("faiden-agent-read-{run_id}"), move || {
                read_loop(reader, stdout)
            }) {
                return Err(self.abandon_start(&run, "reader", e));
            }
        }
        if let Some(stderr) = stderr {
            let drain = run.clone();
            // stderr is diagnostics only: losing it costs a log tail, not the
            // session, so it is not worth tearing a working agent down for.
            let _ = spawn_worker(format!("faiden-agent-err-{run_id}"), move || {
                stderr_loop(drain, stderr)
            });
        }

        let handshaking = run.clone();
        let session_cwd = cwd;
        if let Err(e) = spawn_worker(format!("faiden-agent-init-{run_id}"), move || {
            handshake(handshaking, session_cwd)
        }) {
            return Err(self.abandon_start(&run, "handshake", e));
        }

        Ok(run.info())
    }

    /// Tears down a run whose worker threads could not be created. The child is
    /// killed and reaped, the run is settled `error`, and the registry forgets
    /// it, so nothing survives a start that never became a session.
    fn abandon_start(&self, run: &Arc<AgentRun>, worker: &str, cause: std::io::Error) -> AppError {
        if run.claim_settlement() {
            let detail = run.terminate_child();
            run.settle_claimed(
                format!("the agent {worker} thread could not be started: {cause} — {detail}"),
                AgentStatus::Error,
            );
        }
        self.runs
            .lock()
            .expect("agent registry mutex")
            .remove(&run.run_id);
        AppError::internal(format!("could not start the agent {worker}: {cause}"))
    }

    fn spawn(&self, spec: &AgentSpec) -> Result<Child, AppError> {
        let lookup = locate::Lookup::from_env();
        // Inherited environment, then the caller's additions, then the bypass
        // scrub over the whole result: nothing added here can reintroduce one.
        let mut merged: Vec<(String, String)> = std::env::vars().collect();
        for (name, value) in &spec.env {
            merged.retain(|(existing, _)| existing != name);
            merged.push((name.clone(), value.clone()));
        }
        let mut env = locate::scrubbed_env(merged);
        env.retain(|(name, _)| name != "PATH");
        env.push((
            "PATH".to_string(),
            locate::augmented_path(lookup.path_var.as_deref(), lookup.home.as_deref()),
        ));

        let mut command = Command::new(&spec.program);
        // Explicit argv, never a shell string: nothing here is concatenated or
        // interpreted by a shell.
        command.args(&spec.args);
        command.current_dir(&spec.cwd);
        command.env_clear();
        command.envs(env);
        command.stdin(Stdio::piped());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
        command.spawn().map_err(|e| {
            AppError::conflict(
                "AGENT_START_FAILED",
                format!("could not start {}: {e}", spec.program.display()),
            )
        })
    }

    pub fn info(&self, run_id: &str) -> Result<AgentRunInfo, AppError> {
        Ok(self.get(run_id)?.info())
    }

    pub fn list(&self) -> Vec<AgentRunInfo> {
        let runs: Vec<Arc<AgentRun>> = self
            .runs
            .lock()
            .expect("agent registry mutex")
            .values()
            .cloned()
            .collect();
        runs.iter().map(|r| r.info()).collect()
    }

    /// A coherent cut: the durable transcript, the pending permission and the
    /// cursor a subscriber should resume from, all read under one lock.
    ///
    /// `state` is taken **first** and held across all three reads, because it is
    /// the lock every observable mutation is made under: `record` writes a
    /// transcript row and numbers its `Message` event while holding it, and the
    /// inbound path installs a pending permission and numbers its `Permission`
    /// event while holding it. Reading the store or the permission first would
    /// let either land in between and publish a cursor that is already past
    /// content this snapshot does not carry — content the subscriber then
    /// discards as "already included".
    ///
    /// Reading the store while `state` is held keeps the established
    /// `state -> store` order that `record` and the inbound permission path
    /// already use; no store call reaches back into a run.
    pub fn snapshot(&self, run_id: &str) -> Result<AgentSnapshot, AppError> {
        let run = self.get(run_id)?;
        let state = run.state.lock().expect("agent state mutex");
        let (messages, truncated) = self
            .store
            .list_agent_messages(run_id, MAX_TRANSCRIPT_ENTRIES)?;
        let pending = run
            .permission
            .lock()
            .expect("agent permission mutex")
            .as_ref()
            .map(|p| p.prompt.clone());
        pause_inside_snapshot_cut(run_id);
        Ok(AgentSnapshot {
            info: run.info_locked(&state),
            messages,
            truncated,
            pending_permission: pending,
            next_seq: state.next_seq,
        })
    }

    pub fn diagnostics(&self, run_id: &str) -> Result<AgentDiagnostics, AppError> {
        let run = self.get(run_id)?;
        let state = run.state.lock().expect("agent state mutex");
        Ok(AgentDiagnostics {
            run_id: run_id.to_string(),
            unmatched_replies: state.unmatched_replies,
            protocol_faults: state.protocol_faults,
            refused_requests: state.refused_requests,
            unattributed_updates: state.unattributed_updates,
            stderr_tail: state
                .stderr_tail
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join("\n"),
        })
    }

    /// Sends one prompt. Nothing is ever sent without this call, and a turn
    /// already in flight is refused rather than queued or duplicated.
    pub fn prompt(&self, run_id: &str, text: &str) -> Result<AgentMessage, AppError> {
        let run = self.get(run_id)?;
        let (session_id, turn, request_id) = {
            let mut state = run.state.lock().expect("agent state mutex");
            let Some(session_id) = state.acp_session_id.clone() else {
                return Err(AppError::conflict(
                    "AGENT_NOT_READY",
                    "this agent session has not finished starting; nothing was sent",
                ));
            };
            if state.closing || state.ended_at.is_some() {
                return Err(AppError::conflict(
                    "AGENT_DISCONNECTED",
                    "this agent session has ended; start a new one",
                ));
            }
            if state.prompt_in_flight {
                return Err(AppError::conflict(
                    "AGENT_BUSY",
                    "this agent is still working on the previous message; nothing was sent",
                ));
            }
            state.prompt_in_flight = true;
            state.turn += 1;
            state.agent_body.clear();
            state.thought_body.clear();
            let request_id = state.next_request_id;
            state.next_request_id += 1;
            (session_id, state.turn, request_id)
        };

        // Recorded before the frame goes out, and marked failed if it does not.
        // A message never appears as sent when it was not.
        let key = format!("user:{turn}");
        let recorded = self.store.upsert_agent_message(
            run_id,
            AgentMessageDraft {
                key: &key,
                role: "user",
                turn,
                body: text,
                detail: None,
                status: Some("sending"),
            },
        );
        let recorded = match recorded {
            Ok(message) => message,
            Err(e) => {
                run.state
                    .lock()
                    .expect("agent state mutex")
                    .prompt_in_flight = false;
                return Err(e);
            }
        };

        let slot: Slot = Arc::new((Mutex::new(None), Condvar::new()));
        run.pending
            .lock()
            .expect("agent pending mutex")
            .insert(request_id, slot.clone());
        let frame = protocol::session_prompt_request(request_id, &session_id, text);

        if let Err(e) = run.send(&frame) {
            run.pending
                .lock()
                .expect("agent pending mutex")
                .remove(&request_id);
            let mut state = run.state.lock().expect("agent state mutex");
            state.prompt_in_flight = false;
            let failed = format!("not sent: {}", e.message());
            run.record(
                &mut state,
                AgentMessageDraft {
                    key: &key,
                    role: "user",
                    turn,
                    body: text,
                    detail: None,
                    status: Some(&failed),
                },
            );
            return Err(e);
        }

        let sent = {
            let mut state = run.state.lock().expect("agent state mutex");
            run.record(
                &mut state,
                AgentMessageDraft {
                    key: &key,
                    role: "user",
                    turn,
                    body: text,
                    detail: None,
                    status: Some("sent"),
                },
            );
            run.set_status(&mut state, AgentStatus::Responding, None);
            recorded
        };

        // The turn is awaited off the caller's thread: a turn is legitimately
        // long, and a Tauri command must not hold on to it.
        let waiting = run.clone();
        let _ = std::thread::Builder::new()
            .name(format!("faiden-agent-turn-{run_id}"))
            .spawn(move || await_turn(waiting, slot, request_id, turn));

        Ok(sent)
    }

    /// The user's answer to a pending permission request. Only the request that
    /// is actually pending can be answered, and only with an option the agent
    /// offered.
    pub fn answer_permission(
        &self,
        run_id: &str,
        request_id: &str,
        option_id: Option<&str>,
    ) -> Result<(), AppError> {
        let run = self.get(run_id)?;
        // State is the terminal-ownership lock and must precede permission. The
        // decision is audited and written while it remains held, so a queued UI
        // approval cannot cross a shutdown boundary after it has been checked.
        let mut state = run.state.lock().expect("agent state mutex");
        let mut guard = run.permission.lock().expect("agent permission mutex");
        let Some(pending) = guard.as_ref() else {
            return Err(AppError::conflict(
                "PERMISSION_NOT_PENDING",
                "that permission request is no longer waiting for an answer",
            ));
        };
        if pending.prompt.request_id != request_id {
            return Err(AppError::conflict(
                "PERMISSION_NOT_PENDING",
                "that permission request is no longer waiting for an answer",
            ));
        }
        let decision = if state.closing || state.settlement_claimed || state.ended_at.is_some() {
            // A stale allow is deliberately converted to an ACP cancellation,
            // not merely rejected at method entry. This owns and settles the
            // pending request before a terminal path can release its claim.
            PermissionDecision::Cancelled
        } else {
            match option_id {
                None => PermissionDecision::Cancelled,
                Some(option_id) => {
                    if !pending
                        .prompt
                        .options
                        .iter()
                        .any(|o| o.option_id == option_id)
                    {
                        return Err(AppError::validation(
                            "optionId",
                            format!("the agent did not offer an option called {option_id}"),
                        ));
                    }
                    PermissionDecision::Selected {
                        option_id: option_id.to_string(),
                    }
                }
            }
        };
        let pending = guard.take().expect("pending permission checked above");
        drop(guard);
        let why = if matches!(decision, PermissionDecision::Cancelled)
            && (state.closing || state.settlement_claimed || state.ended_at.is_some())
        {
            "the agent session was ending"
        } else {
            ANSWERED_BY_USER
        };
        run.resolve_taken_permission(&mut state, pending, decision, why)?;
        Ok(())
    }

    /// Sends `session/cancel`. Sending it is not the same as the turn being
    /// over, and it never grants a pending permission.
    pub fn cancel(&self, run_id: &str) -> Result<AgentRunInfo, AppError> {
        let run = self.get(run_id)?;
        let _ = run.resolve_permission(
            PermissionDecision::Cancelled,
            "the turn was cancelled before you answered",
        );
        let session_id = run
            .state
            .lock()
            .expect("agent state mutex")
            .acp_session_id
            .clone();
        let Some(session_id) = session_id else {
            return Err(AppError::conflict(
                "AGENT_NOT_READY",
                "this agent session has not started; there is nothing to cancel",
            ));
        };
        run.send(&protocol::session_cancel_notification(&session_id))?;

        let mut state = run.state.lock().expect("agent state mutex");
        if state.prompt_in_flight {
            run.set_status(
                &mut state,
                AgentStatus::Cancelling,
                Some("Cancel sent. The agent has not reported the turn as over yet.".to_string()),
            );
        }
        Ok(run.info_locked(&state))
    }

    /// Ends this run and its child. Recorded factually: "stopped" is a
    /// lifecycle fact, not a verdict on the work.
    pub fn stop(&self, run_id: &str) -> Result<AgentRunInfo, AppError> {
        let run = self.get(run_id)?;
        self.terminate(&run, "agent session stopped by you");
        Ok(run.info())
    }

    /// Called on application quit. Faiden owns these children; quitting really
    /// does end them, and that is disclosed rather than hidden.
    pub fn shutdown_all(&self) {
        let runs: Vec<Arc<AgentRun>> = self
            .runs
            .lock()
            .expect("agent registry mutex")
            .values()
            .cloned()
            .collect();
        for run in runs {
            self.terminate(&run, "agent session ended because Faiden quit");
        }
    }

    fn terminate(&self, run: &Arc<AgentRun>, reason: &str) {
        // Claim before closing stdin: its resulting EOF is a consequence of
        // this requested termination, not a competing terminal outcome.
        if !run.claim_settlement_and_begin_closing() {
            return;
        }
        // Deny anything pending before the child can lose the chance to hear it.
        let _ = run.resolve_permission(
            PermissionDecision::Cancelled,
            "the agent session was ending",
        );
        let session_id = run
            .state
            .lock()
            .expect("agent state mutex")
            .acp_session_id
            .clone();
        if let Some(session_id) = session_id {
            let _ = run.send(&protocol::session_cancel_notification(&session_id));
        }
        let detail = run.terminate_child();
        run.settle_claimed(format!("{reason} — {detail}"), AgentStatus::Disconnected);
    }
}

// --- debug-only test seams --------------------------------------------------
//
// Each gate pauses one *named point of a real production path* so a test can
// force an interleaving without sleeps or timing guesses. They compile only
// under `debug_assertions` and are inert until a test arms one.

/// The shared pause primitive: a production thread reaching an armed gate
/// announces itself and blocks until the test releases it.
#[cfg(debug_assertions)]
#[derive(Clone)]
struct PauseGate(Arc<(Mutex<PauseGateState>, Condvar)>);

#[cfg(debug_assertions)]
#[derive(Default)]
struct PauseGateState {
    reached: bool,
    released: bool,
}

#[cfg(debug_assertions)]
impl PauseGate {
    fn new() -> PauseGate {
        PauseGate(Arc::new((
            Mutex::new(PauseGateState::default()),
            Condvar::new(),
        )))
    }

    fn pause(&self) {
        let (lock, wake) = &*self.0;
        let mut state = lock.lock().expect("pause gate state");
        state.reached = true;
        wake.notify_all();
        while !state.released {
            state = wake.wait(state).expect("pause gate wait");
        }
    }

    fn wait_until_reached(&self, timeout: Duration, what: &str) {
        let (lock, wake) = &*self.0;
        let guard = lock.lock().expect("pause gate state");
        let (guard, result) = wake
            .wait_timeout_while(guard, timeout, |state| !state.reached)
            .expect("pause gate wait");
        assert!(
            !result.timed_out() || guard.reached,
            "{what} was not reached"
        );
    }

    fn release(&self) {
        let (lock, wake) = &*self.0;
        lock.lock().expect("pause gate state").released = true;
        wake.notify_all();
    }
}

#[cfg(debug_assertions)]
#[derive(Clone)]
pub struct InboundPermissionGateForTest(PauseGate);

/// Arms the debug-test-only pause immediately after an inbound permission's
/// optimistic liveness observation. The real decision is rechecked under the
/// state -> permission gate after this pause; integration tests use it to make
/// shutdown win that exact boundary without timing guesses.
#[cfg(debug_assertions)]
pub fn arm_inbound_permission_gate_for_test() -> InboundPermissionGateForTest {
    let gate = InboundPermissionGateForTest(PauseGate::new());
    *INBOUND_PERMISSION_GATE
        .get_or_init(|| Mutex::new(None))
        .lock()
        .expect("inbound permission gate mutex") = Some(gate.clone());
    gate
}

#[cfg(debug_assertions)]
impl InboundPermissionGateForTest {
    pub fn wait_until_checked(&self, timeout: Duration) {
        self.0.wait_until_reached(timeout, "permission gate");
    }

    pub fn release(&self) {
        self.0.release();
        *INBOUND_PERMISSION_GATE
            .get_or_init(|| Mutex::new(None))
            .lock()
            .expect("inbound permission gate mutex") = None;
    }
}

#[cfg(debug_assertions)]
static INBOUND_PERMISSION_GATE: std::sync::OnceLock<Mutex<Option<InboundPermissionGateForTest>>> =
    std::sync::OnceLock::new();

#[cfg(debug_assertions)]
fn pause_after_inbound_permission_liveness_check() {
    let gate = INBOUND_PERMISSION_GATE
        .get()
        .and_then(|slot| slot.lock().expect("inbound permission gate mutex").clone());
    if let Some(gate) = gate {
        gate.0.pause();
    }
}

#[cfg(not(debug_assertions))]
fn pause_after_inbound_permission_liveness_check() {}

/// Pauses one run's `snapshot` after it has read that run's durable transcript
/// and pending permission, and before it reads the cursor it will publish.
/// Tests use it to run a concurrent transcript or permission write across that
/// exact boundary.
#[cfg(debug_assertions)]
pub struct SnapshotCutGateForTest {
    gate: PauseGate,
    run_id: String,
}

/// Arms the snapshot pause for one run only: several tests snapshot different
/// runs at once, and none of them may block on another test's gate.
#[cfg(debug_assertions)]
pub fn arm_snapshot_cut_gate_for_test(run_id: &str) -> SnapshotCutGateForTest {
    let gate = PauseGate::new();
    SNAPSHOT_CUT_GATES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .expect("snapshot cut gate mutex")
        .insert(run_id.to_string(), gate.clone());
    SnapshotCutGateForTest {
        gate,
        run_id: run_id.to_string(),
    }
}

#[cfg(debug_assertions)]
impl SnapshotCutGateForTest {
    pub fn wait_until_reached(&self, timeout: Duration) {
        self.gate.wait_until_reached(timeout, "snapshot cut gate");
    }

    pub fn release(&self) {
        self.gate.release();
        if let Some(gates) = SNAPSHOT_CUT_GATES.get() {
            gates
                .lock()
                .expect("snapshot cut gate mutex")
                .remove(&self.run_id);
        }
    }
}

#[cfg(debug_assertions)]
static SNAPSHOT_CUT_GATES: std::sync::OnceLock<Mutex<HashMap<String, PauseGate>>> =
    std::sync::OnceLock::new();

#[cfg(debug_assertions)]
fn pause_inside_snapshot_cut(run_id: &str) {
    let gate = SNAPSHOT_CUT_GATES.get().and_then(|gates| {
        gates
            .lock()
            .expect("snapshot cut gate mutex")
            .get(run_id)
            .cloned()
    });
    if let Some(gate) = gate {
        gate.pause();
    }
}

#[cfg(not(debug_assertions))]
fn pause_inside_snapshot_cut(_run_id: &str) {}

// --- worker threads ---------------------------------------------------------

/// Lets tests deterministically fail a worker-thread spawn that happens after a
/// real child exists, without adding anything to the production path (the
/// injection module compiles away entirely outside `cfg(test)`). Mirrors the
/// same seam in `terminal.rs`.
#[cfg(test)]
mod fault_injection {
    use std::cell::Cell;

    thread_local! {
        static FAIL_NEXT_SPAWN: Cell<Option<&'static str>> = const { Cell::new(None) };
    }

    /// Fails the next worker spawn whose name contains `tag`, once.
    pub(super) fn arm(tag: &'static str) {
        FAIL_NEXT_SPAWN.with(|c| c.set(Some(tag)));
    }

    pub(super) fn should_fail(name: &str) -> bool {
        FAIL_NEXT_SPAWN.with(|c| {
            let armed = c.get().is_some_and(|tag| name.contains(tag));
            if armed {
                c.set(None);
            }
            armed
        })
    }
}

/// Thin wrapper over `thread::Builder::spawn`; identical to the real call in
/// production.
fn spawn_worker<F>(name: String, f: F) -> std::io::Result<std::thread::JoinHandle<()>>
where
    F: FnOnce() + Send + 'static,
{
    #[cfg(test)]
    if fault_injection::should_fail(&name) {
        return Err(std::io::Error::other(format!(
            "fault-injected spawn failure for {name}"
        )));
    }
    std::thread::Builder::new().name(name).spawn(f)
}

fn handshake(run: Arc<AgentRun>, cwd: String) {
    let timeout = run.timeouts.handshake;

    let id = run.next_request_id();
    let initialized = match run.request(id, protocol::initialize_request(id), timeout) {
        Ok(result) => result,
        Err(e) => {
            fail_startup(&run, format!("initialize failed: {}", e.message()));
            return;
        }
    };
    // The agent answers with the version it will actually speak. Anything other
    // than the one this build implements means no version was agreed, and every
    // later frame would be guesswork.
    match initialized.get("protocolVersion").and_then(Value::as_u64) {
        Some(version) if version == u64::from(protocol::PROTOCOL_VERSION) => {}
        Some(version) => {
            fail_startup(
                &run,
                format!(
                    "the agent answered initialize with protocol version {version}; Faiden \
                     implements version {}, so no version was agreed",
                    protocol::PROTOCOL_VERSION
                ),
            );
            return;
        }
        None => {
            fail_startup(
                &run,
                "the agent answered initialize without a protocolVersion, so no version was \
                 agreed"
                    .to_string(),
            );
            return;
        }
    }

    let id = run.next_request_id();
    let result = match run.request(id, protocol::session_new_request(id, &cwd), timeout) {
        Ok(result) => result,
        Err(e) => {
            fail_startup(&run, format!("session/new failed: {}", e.message()));
            return;
        }
    };
    let Some(session_id) = result.get("sessionId").and_then(Value::as_str) else {
        fail_startup(
            &run,
            "session/new answered without a sessionId, so no session exists".to_string(),
        );
        return;
    };

    // Durable before observable. A run whose row does not carry its ACP session
    // id could not be identified after a restart, so it must not be presented as
    // a ready session that the user can prompt.
    if let Err(e) = run.store.record_acp_session_id(&run.run_id, session_id) {
        fail_startup(
            &run,
            format!(
                "session/new answered, but its session id could not be recorded: {}",
                e.message()
            ),
        );
        return;
    }
    let mut state = run.state.lock().expect("agent state mutex");
    state.acp_session_id = Some(session_id.to_string());
    run.set_status(&mut state, AgentStatus::Ready, None);
}

/// A startup that failed settles the run. It never lingers as "connecting",
/// and it is never allowed to look like a session that exists.
fn fail_startup(run: &Arc<AgentRun>, detail: String) {
    // The protocol rejection is known before closing stdin can make the peer
    // exit and its reader observe EOF. Reserve it before teardown so EOF cannot
    // replace this useful startup error with a generic disconnection.
    if !run.claim_settlement_and_begin_closing() {
        return;
    }
    let child_detail = run.terminate_child();
    // Settled as `error`, not `disconnected`: what happened is that startup
    // failed, and the run must never be mistaken for a session that existed.
    run.settle_claimed(format!("{detail} — {child_detail}"), AgentStatus::Error);
}

fn await_turn(run: Arc<AgentRun>, slot: Slot, request_id: u64, turn: i64) {
    let deadline = Instant::now() + TURN_BACKSTOP;
    let (lock, condvar) = &*slot;
    let mut answer = lock.lock().expect("agent slot mutex");
    while answer.is_none() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let (guard, _) = condvar
            .wait_timeout(answer, remaining)
            .expect("agent slot condvar");
        answer = guard;
    }
    let outcome = answer.take();
    drop(answer);
    run.pending
        .lock()
        .expect("agent pending mutex")
        .remove(&request_id);

    let mut state = run.state.lock().expect("agent state mutex");
    state.prompt_in_flight = false;
    // Finalise the streamed entries for this turn, so nothing stays "streaming"
    // after the turn is over.
    finalise_turn(&run, &mut state, turn);

    match outcome {
        Some(Ok(result)) => {
            let stop = StopReason::parse(
                result
                    .get("stopReason")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown"),
            );
            run.set_status(&mut state, AgentStatus::TurnComplete, Some(stop.describe()));
            drop(state);
            // A finished turn is a result to read, not a verdict. It lands in
            // the inbox unseen: opening a thread is not reviewing it.
            run.record_result(
                "agent-turn",
                &format!(
                    "Hermes turn ended ({}). Read the transcript — Faiden makes no claim about \
                     whether the work is correct.",
                    stop.as_str()
                ),
            );
        }
        Some(Err(message)) => {
            run.set_status(
                &mut state,
                AgentStatus::Error,
                Some(format!("the turn failed: {message}")),
            );
        }
        None => {
            run.set_status(
                &mut state,
                AgentStatus::Error,
                Some(format!(
                    "the agent never answered this turn within {TURN_BACKSTOP:?}"
                )),
            );
        }
    }
}

fn finalise_turn(run: &Arc<AgentRun>, state: &mut RunState, turn: i64) {
    if !state.agent_body.is_empty() {
        let body = state.agent_body.clone();
        run.record(
            state,
            AgentMessageDraft {
                key: &format!("agent:{turn}"),
                role: "agent",
                turn,
                body: &body,
                detail: None,
                status: Some("complete"),
            },
        );
    }
    if !state.thought_body.is_empty() {
        let body = state.thought_body.clone();
        run.record(
            state,
            AgentMessageDraft {
                key: &format!("thought:{turn}"),
                role: "thought",
                turn,
                body: &body,
                detail: None,
                status: Some("complete"),
            },
        );
    }
}

fn read_loop(run: Arc<AgentRun>, stdout: std::process::ChildStdout) {
    let mut reader = BufReader::new(stdout);
    loop {
        match read_bounded_line(&mut reader, protocol::MAX_LINE_BYTES) {
            Ok(None) => break,
            Ok(Some((bytes, oversized))) => {
                if oversized {
                    count_fault(
                        &run,
                        &ProtocolError::Oversized {
                            bytes: protocol::MAX_LINE_BYTES,
                        },
                    );
                    continue;
                }
                if bytes.iter().all(u8::is_ascii_whitespace) {
                    continue;
                }
                let text = String::from_utf8_lossy(&bytes).to_string();
                match protocol::parse_line(&text) {
                    Ok(message) => dispatch(&run, message),
                    Err(fault) => count_fault(&run, &fault),
                }
            }
            Err(_) => break,
        }
    }
    // EOF: the child closed stdout or died. Claim before reaping so a concurrent
    // requested stop that closes stdin cannot overwrite the EOF outcome.
    if !run.claim_settlement_and_begin_closing() {
        return;
    }
    let detail = run.terminate_child();
    run.settle_claimed(
        format!("the agent session ended — {detail}"),
        AgentStatus::Disconnected,
    );
}

/// Reads one newline-terminated line without ever buffering more than `limit`
/// bytes. An oversized line is consumed and reported, not accumulated.
fn read_bounded_line<R: BufRead>(
    reader: &mut R,
    limit: usize,
) -> std::io::Result<Option<(Vec<u8>, bool)>> {
    let mut out: Vec<u8> = Vec::new();
    let mut oversized = false;
    loop {
        let available = match reader.fill_buf() {
            Ok(buf) => buf,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if available.is_empty() {
            if out.is_empty() && !oversized {
                return Ok(None);
            }
            return Ok(Some((out, oversized)));
        }
        match available.iter().position(|&b| b == b'\n') {
            Some(index) => {
                if !oversized && out.len() + index <= limit {
                    out.extend_from_slice(&available[..index]);
                } else {
                    oversized = true;
                    out.clear();
                }
                reader.consume(index + 1);
                return Ok(Some((out, oversized)));
            }
            None => {
                let len = available.len();
                if !oversized && out.len() + len <= limit {
                    out.extend_from_slice(available);
                } else {
                    oversized = true;
                    out.clear();
                }
                reader.consume(len);
            }
        }
    }
}

fn count_fault(run: &Arc<AgentRun>, fault: &ProtocolError) {
    let mut state = run.state.lock().expect("agent state mutex");
    state.protocol_faults += 1;
    push_stderr(&mut state, &format!("protocol fault: {fault}"));
}

fn dispatch(run: &Arc<AgentRun>, message: Incoming) {
    match message {
        Incoming::Response { id, result } => settle_reply(run, &id, Ok(result)),
        Incoming::ErrorResponse { id, code, message } => settle_reply(
            run,
            &id,
            Err(format!(
                "the agent reported an error: {message} (code {code})"
            )),
        ),
        Incoming::Request { id, method, params } => handle_request(run, id, &method, &params),
        Incoming::Notification { method, params } => {
            if method == "session/update" {
                apply_update(run, &params);
            }
            // Other notifications are not errors; there is simply nothing this
            // build does with them.
        }
    }
}

fn settle_reply(run: &Arc<AgentRun>, id: &Value, outcome: Result<Value, String>) {
    let matched = id
        .as_u64()
        .map(|numeric| run.settle_slot(numeric, outcome))
        .unwrap_or(false);
    if !matched {
        // A reply to an id we never issued, or already settled. Counted, never
        // applied: a late answer must not land on the current turn.
        let mut state = run.state.lock().expect("agent state mutex");
        state.unmatched_replies += 1;
    }
}

/// Why a permission request must be refused without asking the user, or `None`
/// when it is genuinely answerable.
fn refusal_reason(state: &RunState, claimed_session: &str) -> Option<String> {
    if state.closing || state.settlement_claimed || state.ended_at.is_some() {
        return Some("the agent session is ending or has already ended".to_string());
    }
    match state.acp_session_id.as_deref() {
        None => Some("no ACP session has been created for this run".to_string()),
        Some(ours) if ours != claimed_session => Some(format!(
            "it names ACP session {claimed_session}, which this run does not own"
        )),
        Some(_) => {
            if state.prompt_in_flight {
                None
            } else {
                Some("no turn is in flight, so there is nothing to approve".to_string())
            }
        }
    }
}

fn handle_request(run: &Arc<AgentRun>, id: Value, method: &str, params: &Value) {
    if method != "session/request_permission" {
        // We advertised no filesystem or terminal capability, so this is the
        // only honest answer.
        let _ = run.send(&protocol::method_not_found_response(&id, method));
        return;
    }
    let Some(parsed) = protocol::parse_permission_request(params) else {
        let _ = run.send(&protocol::permission_response(
            &id,
            PermissionDecision::Cancelled,
        ));
        count_fault(
            run,
            &ProtocolError::Unrecognised(
                "a permission request arrived with no option to choose".to_string(),
            ),
        );
        return;
    };

    // The optimistic check exists only to expose the interleaving seam in debug
    // tests. The decision below is made again while state -> permission is held;
    // that second check-and-install is the linearization point.
    let optimistically_live = {
        let state = run.state.lock().expect("agent state mutex");
        refusal_reason(&state, &parsed.session_id).is_none()
    };
    if optimistically_live {
        pause_after_inbound_permission_liveness_check();
    }

    let request_id = format!("perm_{}", Uuid::new_v4().simple());
    let created_at = run.clock.now_ms();
    let options_json = serde_json::to_string(&parsed.options).unwrap_or_else(|_| "[]".to_string());
    let prompt = PermissionPrompt {
        request_id: request_id.clone(),
        run_id: run.run_id.clone(),
        tool_call_id: parsed.tool_call_id,
        title: parsed.title,
        detail: parsed.detail,
        options: parsed.options,
        created_at,
        expires_at: created_at + run.timeouts.permission.as_millis() as i64,
    };

    // State is the terminal-ownership lock. Holding it through durable record,
    // pending installation, and emission makes inbound permission actionable at
    // one point relative to shutdown: state -> permission -> writer.
    let mut state = run.state.lock().expect("agent state mutex");
    let mut permissions = run.permission.lock().expect("agent permission mutex");
    if let Some(reason) = refusal_reason(&state, &parsed.session_id) {
        // A terminal claimant won after the optimistic check. Leave no pending
        // prompt, but retain a cancelled audit row whenever it can be recorded.
        if (state.closing || state.settlement_claimed || state.ended_at.is_some())
            && run
                .store
                .record_permission_request(
                    &request_id,
                    &run.run_id,
                    &prompt.title,
                    &prompt.detail,
                    &options_json,
                )
                .is_ok()
        {
            let _ = run
                .store
                .resolve_permission_request(&request_id, AgentPermissionResolution::Cancelled);
            run.record_result(
                "agent-permission",
                &format!(
                    "Answered \"not allowed\" without you (the agent session was ending): {}",
                    prompt.title
                ),
            );
        }
        let _ = run.send(&protocol::permission_response(
            &id,
            PermissionDecision::Cancelled,
        ));
        state.refused_requests += 1;
        push_stderr(
            &mut state,
            &format!("refused a permission request: {reason}"),
        );
        return;
    }

    if let Err(e) = run.store.record_permission_request(
        &request_id,
        &run.run_id,
        &prompt.title,
        &prompt.detail,
        &options_json,
    ) {
        // If it cannot be recorded it cannot be answered accountably: fail
        // closed rather than prompt for something we cannot audit.
        eprintln!("faiden: refusing an unrecordable permission request: {e}");
        let _ = run.send(&protocol::permission_response(
            &id,
            PermissionDecision::Cancelled,
        ));
        return;
    }

    // Replace-and-deny: a second request while one is pending must not silently
    // drop the first, which would leave the agent waiting forever.
    let previous = permissions.replace(PendingPermission {
        prompt: prompt.clone(),
        rpc_id: id,
    });
    if let Some(previous) = previous {
        let _ = run.send(&protocol::permission_response(
            &previous.rpc_id,
            PermissionDecision::Cancelled,
        ));
        let _ = run.store.resolve_permission_request(
            &previous.prompt.request_id,
            AgentPermissionResolution::Cancelled,
        );
    }

    let run_id = run.run_id.clone();
    let epoch = run.epoch;
    let request = prompt.clone();
    run.emit(&mut state, move |seq| AgentEvent::Permission {
        run_id,
        epoch,
        seq,
        request,
    });
    run.set_status(&mut state, AgentStatus::AwaitingPermission, None);
    drop(permissions);
    drop(state);

    // Faiden's own deadline, deliberately shorter than Hermes's 60 s auto-deny,
    // so the answer the user is shown is the answer that actually lands.
    let expiring = run.clone();
    let deadline = run.timeouts.permission;
    let _ = std::thread::Builder::new()
        .name(format!("faiden-agent-perm-{request_id}"))
        .spawn(move || {
            std::thread::sleep(deadline);
            let still_pending = expiring
                .permission
                .lock()
                .expect("agent permission mutex")
                .as_ref()
                .map(|p| p.prompt.request_id == request_id)
                .unwrap_or(false);
            if still_pending {
                let _ = expiring.resolve_permission(
                    PermissionDecision::Cancelled,
                    "no answer within Faiden's permission deadline",
                );
            }
        });
}

fn apply_update(run: &Arc<AgentRun>, params: &Value) {
    let Some(update) = protocol::parse_session_update(params) else {
        count_fault(
            run,
            &ProtocolError::Unrecognised("unreadable session/update".to_string()),
        );
        return;
    };
    let mut state = run.state.lock().expect("agent state mutex");
    // A session id that is not ours is not applied: it belongs to nothing this
    // run owns.
    if state
        .acp_session_id
        .as_deref()
        .is_some_and(|ours| ours != update.session_id)
    {
        state.unmatched_replies += 1;
        return;
    }
    // No live turn means no entry this update can belong to. Appending it would
    // either repaint a turn the user has already been told is over, or attribute
    // one turn's output to the next one. Counted instead, so the drop is
    // visible rather than silent.
    if !state.prompt_in_flight {
        state.unattributed_updates += 1;
        push_stderr(
            &mut state,
            "dropped a session/update that arrived with no turn in flight",
        );
        return;
    }
    let turn = state.turn;

    match update.delta {
        protocol::TranscriptDelta::AgentMessage { text } => {
            state.agent_body.push_str(&text);
            let body = state.agent_body.clone();
            run.record(
                &mut state,
                AgentMessageDraft {
                    key: &format!("agent:{turn}"),
                    role: "agent",
                    turn,
                    body: &body,
                    detail: None,
                    status: Some("streaming"),
                },
            );
        }
        protocol::TranscriptDelta::Thought { text } => {
            state.thought_body.push_str(&text);
            let body = state.thought_body.clone();
            run.record(
                &mut state,
                AgentMessageDraft {
                    key: &format!("thought:{turn}"),
                    role: "thought",
                    turn,
                    body: &body,
                    detail: None,
                    status: Some("streaming"),
                },
            );
        }
        protocol::TranscriptDelta::ToolCall {
            tool_call_id,
            title,
            kind,
            status,
            content,
        } => {
            let entry = state.tools.entry(tool_call_id.clone()).or_default();
            if title.is_some() {
                entry.title = title;
            }
            if kind.is_some() {
                entry.kind = kind;
            }
            if status.is_some() {
                entry.status.clone_from(&status);
            }
            entry.content.push_str(&content);
            let body = entry.body();
            let entry_status = entry.status.clone();
            run.record(
                &mut state,
                AgentMessageDraft {
                    key: &format!("tool:{tool_call_id}"),
                    role: "tool",
                    turn,
                    body: &body,
                    detail: None,
                    status: entry_status.as_deref(),
                },
            );
        }
        protocol::TranscriptDelta::Other { .. } => {}
    }
}

fn stderr_loop(run: Arc<AgentRun>, stderr: std::process::ChildStderr) {
    let reader = BufReader::new(stderr);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        let mut state = run.state.lock().expect("agent state mutex");
        push_stderr(&mut state, &line);
    }
}

fn push_stderr(state: &mut RunState, line: &str) {
    let bounded: String = line.chars().take(MAX_STDERR_LINE_CHARS).collect();
    state.stderr_tail.push_back(bounded);
    while state.stderr_tail.len() > MAX_STDERR_LINES {
        state.stderr_tail.pop_front();
    }
}

#[cfg(test)]
mod worker_failure_tests {
    use super::*;
    use crate::store::NewSession;

    /// A child that ignores stdin and outlives the test unless it is killed, so
    /// a leak is observable rather than hidden by a cooperative exit.
    fn sleeping_spec(session_id: String, dir: &std::path::Path) -> AgentSpec {
        AgentSpec {
            session_id,
            program: PathBuf::from("/bin/sleep"),
            args: vec!["30".to_string()],
            cwd: dir.to_path_buf(),
            env: Vec::new(),
            source: "test fixture (not Hermes)".to_string(),
            timeouts: AgentTimeouts {
                handshake: Duration::from_millis(200),
                permission: Duration::from_millis(200),
                shutdown: Duration::from_secs(2),
            },
        }
    }

    fn harness() -> (tempfile::TempDir, Arc<Store>, AgentManager, String) {
        let dir = tempfile::TempDir::new().unwrap();
        let store = Arc::new(Store::open(&dir.path().join("faiden.sqlite3")).unwrap());
        let thread = store.create_thread("worker failure").unwrap();
        let session = store
            .create_session(
                &thread.id,
                NewSession {
                    label: "agent".to_string(),
                    kind: "hermes-acp".to_string(),
                    predecessor_id: None,
                    briefing: String::new(),
                },
            )
            .unwrap();
        let manager = AgentManager::new(Arc::new(NoAgentEvents), store.clone());
        (dir, store, manager, session.id)
    }

    fn process_gone(pid: u32) -> bool {
        let out = std::process::Command::new("/bin/ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .output()
            .expect("run ps");
        String::from_utf8_lossy(&out.stdout).trim().is_empty()
    }

    /// The child pid of the only run the manager holds, before the failure path
    /// removes it. Read by listing `/bin/ps` for our own sleep children.
    fn sleep_children() -> Vec<u32> {
        let out = std::process::Command::new("/bin/ps")
            .args(["-o", "pid=,ppid=,comm="])
            .output()
            .expect("run ps");
        let me = std::process::id();
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|line| {
                let mut parts = line.split_whitespace();
                let pid: u32 = parts.next()?.parse().ok()?;
                let ppid: u32 = parts.next()?.parse().ok()?;
                let comm = parts.next()?;
                (ppid == me && comm.ends_with("sleep")).then_some(pid)
            })
            .collect()
    }

    #[test]
    fn a_reader_thread_that_cannot_start_leaves_no_child_and_settles_the_run() {
        let (dir, store, manager, session) = harness();
        let before = sleep_children();

        fault_injection::arm("agent-read");
        let err = manager
            .start(sleeping_spec(session.clone(), dir.path()))
            .expect_err("the reader could not be started");
        assert_eq!(err.code(), "INTERNAL");

        // No child survives, and the registry does not hold a phantom run.
        for pid in sleep_children() {
            assert!(
                before.contains(&pid),
                "agent child {pid} was left running after a failed start"
            );
        }
        assert!(manager.list().is_empty());

        // The durable row is terminal and truthful, so the session is not left
        // open until the next restart.
        let runs = store.list_agent_runs(&session).unwrap();
        assert_eq!(runs.len(), 1);
        assert!(runs[0].ended_at.is_some());
        let outcome = runs[0].outcome.clone().unwrap();
        assert!(outcome.contains("reader"), "got {outcome}");
        assert!(!outcome.to_lowercase().contains("success"));
        assert!(store.get_session(&session).unwrap().ended_at.is_some());
    }

    #[test]
    fn a_handshake_thread_that_cannot_start_is_cleaned_up_the_same_way() {
        let (dir, store, manager, session) = harness();

        fault_injection::arm("agent-init");
        let err = manager
            .start(sleeping_spec(session.clone(), dir.path()))
            .expect_err("the handshake could not be started");
        assert_eq!(err.code(), "INTERNAL");
        assert!(manager.list().is_empty());

        let runs = store.list_agent_runs(&session).unwrap();
        assert!(runs[0].ended_at.is_some());
        assert!(runs[0].outcome.clone().unwrap().contains("handshake"));

        // And no child of ours is left behind.
        for pid in sleep_children() {
            assert!(
                process_gone(pid),
                "agent child {pid} outlived a failed start"
            );
        }
    }
}

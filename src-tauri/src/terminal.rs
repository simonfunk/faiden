//! Real PTY-backed terminals.
//!
//! Design notes that the tests pin down:
//!
//! * A terminal is owned by the Rust side and keyed by a fresh `terminal_id`
//!   per run. Switching views in the UI does nothing to it.
//! * Every event carries `epoch` and `seq`. A view that re-attaches calls
//!   [`TerminalManager::snapshot`] and discards any event whose `seq` is below
//!   the returned `next_seq`, so the subscribe/snapshot race cannot duplicate
//!   or lose output.
//! * Bytes are decoded with a streaming UTF-8 decoder, so a codepoint split
//!   across two `read()` calls is never turned into replacement characters.
//! * Retained scrollback is capped; the cap being hit is reported, not hidden.
//! * The child is killed *and* reaped: a dedicated waiter thread owns
//!   `child.wait()`, so no zombie survives a close. Closing terminates the
//!   whole session the shell leads, not just the shell, so a backgrounded job
//!   is never left orphaned (see [`reap`]).
//! * Terminal liveness says nothing about whether an agent is doing anything
//!   useful. This module never claims otherwise.
//!
//! # Lock order
//!
//! `terminals` → `output` → `life` → `writer`/`master`/`killer`. Nothing ever
//! takes `output` while holding `life`. That is what lets [`TerminalManager::
//! snapshot`] return a coherent cut of lifecycle *and* cursor: an exit reserves
//! its sequence number and publishes its lifecycle change under one held
//! `output` lock, so a snapshot can never report `running: true` alongside a
//! cursor that already supersedes the exit event.

use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{native_pty_system, Child, ChildKiller, CommandBuilder, MasterPty, PtySize};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::clock::MonotonicClock;
use crate::error::AppError;

/// Scrollback retained per terminal, in characters.
pub const RETAINED_OUTPUT_CHARS: usize = 200_000;
/// Upper bound on a single output event, so one burst cannot flood the IPC channel.
pub const MAX_EVENT_CHARS: usize = 16_384;
/// Upper bound on a single write from the UI into a pty.
pub const MAX_WRITE_CHARS: usize = 65_536;

const READ_BUF: usize = 8 * 1024;
const MIN_DIM: u16 = 1;
const MAX_DIM: u16 = 1_000;
/// How long a waiter thread lets the reader drain before announcing the exit.
const DRAIN_GRACE: Duration = Duration::from_secs(2);
/// How long `close` waits for the child to actually die and be reaped.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);
/// How long everything in a terminal's session gets to handle SIGHUP/SIGTERM
/// before SIGKILL. Bounded so closing stays responsive.
const KILL_GRACE: Duration = Duration::from_millis(300);

/// Terminating a terminal means terminating everything that terminal started.
///
/// `portable-pty` calls `setsid()` in the child before `exec`, so the shell is
/// the leader of a brand new session and *every* process it goes on to
/// start — a foreground job, a background job that job control put in its own
/// process group, a worker a tool forked — carries that session id. The session
/// id is therefore the exact boundary of "this terminal": killing by it reaches
/// descendants that reparenting to `launchd` would otherwise hide, and by
/// construction cannot reach a process Faiden did not start.
#[cfg(unix)]
mod reap {
    use std::time::{Duration, Instant};

    #[cfg(target_os = "macos")]
    const PID_SIZE: usize = std::mem::size_of::<libc::c_int>();

    /// `proc_listallpids` reports and later fills in a PID *count*, not a byte
    /// count, on both the sizing call and the real one; only the `buffersize`
    /// it takes as input is in bytes. Capacity is sized in PIDs, with headroom
    /// because processes can appear between the two calls. Dividing the hint
    /// by `sizeof(pid_t)` here (as if it were bytes) would quarter the true
    /// capacity on a 4-byte `pid_t` and silently drop most of the session.
    #[cfg(target_os = "macos")]
    fn capacity_for_hint(hint: libc::c_int) -> usize {
        hint.max(0) as usize + 128
    }

    /// How many of the `cap` allocated slots hold a PID the kernel actually
    /// wrote, given the count `proc_listallpids` returned from the real call.
    /// That count is a PID count, never a byte count, and is clamped to what
    /// was actually allocated.
    #[cfg(target_os = "macos")]
    fn returned_len(count: libc::c_int, cap: usize) -> usize {
        (count.max(0) as usize).min(cap)
    }

    /// Every live process in session `sid`, excluding this process and pid 1.
    #[cfg(target_os = "macos")]
    fn members(sid: libc::pid_t) -> Vec<libc::pid_t> {
        // A first call with a null buffer reports the number of PIDs
        // currently active.
        let hint = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
        if hint <= 0 {
            return Vec::new();
        }
        let cap = capacity_for_hint(hint);
        let mut buf = vec![0 as libc::c_int; cap];
        let count = unsafe {
            libc::proc_listallpids(buf.as_mut_ptr().cast(), (cap * PID_SIZE) as libc::c_int)
        };
        if count <= 0 {
            return Vec::new();
        }
        let me = unsafe { libc::getpid() };
        buf[..returned_len(count, cap)]
            .iter()
            .map(|&p| p as libc::pid_t)
            .filter(|&p| p > 1 && p != me && unsafe { libc::getsid(p) } == sid)
            .collect()
    }

    #[cfg(all(test, target_os = "macos"))]
    mod macos_pid_count_tests {
        use super::*;

        #[test]
        fn capacity_treats_the_hint_as_a_pid_count_not_a_byte_count() {
            // The bug divided an already-PID-counted hint by sizeof(pid_t)
            // again. On a 4-byte pid_t, a hint of 1,000 PIDs must still yield
            // room for roughly 1,000 PIDs, not the ~378 the old byte-based
            // arithmetic produced.
            assert_eq!(capacity_for_hint(1_000), 1_128);
        }

        #[test]
        fn returned_len_keeps_every_pid_the_kernel_wrote_beyond_the_old_quarter() {
            // Mirrors the reported scenario exactly: a capacity of 378 PIDs,
            // all filled in by the kernel. The old code computed
            // `378 / 4 == 94`, silently dropping about three quarters of a
            // fully populated session. The fix must see all 378.
            assert_eq!(returned_len(378, 378), 378);
        }

        #[test]
        fn returned_len_never_exceeds_the_allocated_capacity() {
            // A process count that grew between the sizing call and the real
            // one must never index past what was actually allocated.
            assert_eq!(returned_len(5_000, 378), 378);
        }

        #[test]
        fn returned_len_treats_a_failed_call_as_empty() {
            assert_eq!(returned_len(-1, 378), 0);
            assert_eq!(returned_len(0, 378), 0);
        }
    }

    /// Without a session enumeration this falls back to the process group,
    /// which is narrower but never wrong.
    #[cfg(not(target_os = "macos"))]
    fn members(sid: libc::pid_t) -> Vec<libc::pid_t> {
        let me = unsafe { libc::getpid() };
        if sid > 1 && sid != me && unsafe { libc::getsid(sid) } == sid {
            vec![sid]
        } else {
            Vec::new()
        }
    }

    fn signal_all(sid: libc::pid_t, sig: libc::c_int) {
        for pid in members(sid) {
            unsafe { libc::kill(pid, sig) };
        }
        // The leader's own process group covers anything that changed session
        // membership between enumeration and signalling.
        unsafe { libc::killpg(sid, sig) };
    }

    /// Signals, waits a bounded grace period, then forces. Returns the pids
    /// still present afterwards, so the caller can report the truth rather than
    /// assume success.
    pub fn terminate_session(leader: u32, grace: Duration) -> Vec<u32> {
        let sid = leader as libc::pid_t;
        // Fail safe: a terminal always has its own session. If that invariant
        // ever broke we must not signal Faiden's own session.
        if sid <= 1 || sid == unsafe { libc::getsid(0) } {
            return Vec::new();
        }
        let observed = unsafe { libc::getsid(sid) };
        if observed > 0 && observed != sid {
            // Not the session leader we expect: stay inside the process group.
            unsafe {
                libc::killpg(sid, libc::SIGTERM);
                libc::kill(sid, libc::SIGTERM);
            }
            std::thread::sleep(grace);
            unsafe {
                libc::killpg(sid, libc::SIGKILL);
                libc::kill(sid, libc::SIGKILL);
            }
            return Vec::new();
        }

        signal_all(sid, libc::SIGHUP);
        signal_all(sid, libc::SIGTERM);
        let deadline = Instant::now() + grace;
        while Instant::now() < deadline {
            if members(sid).is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        signal_all(sid, libc::SIGKILL);
        members(sid).into_iter().map(|p| p as u32).collect()
    }
}

/// Kills everything the terminal started. On platforms without a session
/// implementation the caller's `ChildKiller` remains the whole mechanism.
fn kill_process_tree(pid: Option<u32>) {
    #[cfg(unix)]
    if let Some(pid) = pid {
        let _ = reap::terminate_session(pid, KILL_GRACE);
    }
    #[cfg(not(unix))]
    let _ = pid;
}

/// A child shared between the waiter thread that normally owns `wait()` and
/// [`StartGuard`], which must be able to reap it itself if that thread never
/// comes into being.
type SharedChild = Arc<Mutex<Box<dyn Child + Send + Sync>>>;

/// Lets tests deterministically fail the reader/waiter thread spawn that
/// happens after a real child exists, without touching the production path
/// (this module compiles away entirely outside `cfg(test)`).
#[cfg(test)]
mod fault_injection {
    use std::cell::Cell;

    thread_local! {
        static FAIL_NEXT_SPAWN: Cell<Option<&'static str>> = const { Cell::new(None) };
    }

    /// Fails the next thread spawn whose name contains `tag`, once.
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

/// Thin wrapper over `thread::Builder::spawn` so failure can be injected
/// deterministically in tests; identical to the real call in production.
fn spawn_named<F>(name: String, f: F) -> std::io::Result<std::thread::JoinHandle<()>>
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

/// Ensures that once a run is durably marked `started`, every failure path in
/// [`TerminalManager::start`] settles it with a factual outcome instead of
/// leaving it open until the next app restart — and that once a real child
/// exists, a failure occurring before the waiter thread takes ownership of
/// `wait()` still kills and synchronously reaps it, rather than leaving an
/// unreaped zombie or a phantom "running" terminal behind. `disarm` is the
/// only way to skip this; every other exit from `start` runs it via `Drop`.
struct StartGuard {
    lifecycle: Arc<dyn TerminalLifecycle>,
    terminal_id: String,
    pid: Option<u32>,
    child: Option<SharedChild>,
    armed: bool,
}

impl StartGuard {
    fn new(lifecycle: Arc<dyn TerminalLifecycle>, terminal_id: String) -> StartGuard {
        StartGuard {
            lifecycle,
            terminal_id,
            pid: None,
            child: None,
            armed: true,
        }
    }

    /// Called once a real child exists, so a later failure can kill and reap
    /// it rather than merely settling the durable record.
    fn attach_child(&mut self, pid: Option<u32>, child: SharedChild) {
        self.pid = pid;
        self.child = Some(child);
    }

    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for StartGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        kill_process_tree(self.pid);
        if let Some(child) = &self.child {
            // No waiter thread exists to own `wait()`: reap synchronously
            // here instead of leaving a zombie behind.
            let _ = child.lock().expect("child mutex").wait();
        }
        self.lifecycle.ended(
            &self.terminal_id,
            "terminal failed to start before it began running",
        );
    }
}

/// Durable bookkeeping for a terminal run.
///
/// The manager knows nothing about SQLite; the host wires this to the store so
/// that a terminal's start and its end are recorded against the session that
/// owns it. `ended` may be called more than once for one run — a natural exit
/// racing the quit path — and the implementation is required to keep the first
/// outcome rather than overwrite it.
pub trait TerminalLifecycle: Send + Sync + 'static {
    /// Called before the child is spawned. An error aborts the start, so a
    /// terminal never runs without a durable owner.
    fn started(&self, terminal_id: &str, session_id: &str) -> Result<(), AppError>;
    fn ended(&self, terminal_id: &str, outcome: &str);
}

/// Used by tests and by any caller that does not persist runs.
pub struct NoLifecycle;

impl TerminalLifecycle for NoLifecycle {
    fn started(&self, _terminal_id: &str, _session_id: &str) -> Result<(), AppError> {
        Ok(())
    }
    fn ended(&self, _terminal_id: &str, _outcome: &str) {}
}

/// Recorded when the application deliberately ends a terminal on quit. It
/// states the cause; it is not a claim that the work finished.
pub const OUTCOME_QUIT: &str = "terminated because Faiden quit";

/// The factual end of a run. Never phrased as success.
pub fn exit_outcome(exit: &ExitInfo) -> String {
    format!(
        "terminal process exited with code {} ({})",
        exit.exit_code, exit.description
    )
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ExitInfo {
    pub exit_code: u32,
    pub success: bool,
    /// Verbatim from the OS (e.g. `Terminated by Killed`), never a judgement
    /// about whether the work succeeded.
    pub description: String,
    pub at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum TerminalEvent {
    #[serde(rename_all = "camelCase")]
    Output {
        terminal_id: String,
        epoch: u64,
        seq: u64,
        chunk: String,
    },
    #[serde(rename_all = "camelCase")]
    Exit {
        terminal_id: String,
        epoch: u64,
        seq: u64,
        exit_code: u32,
        success: bool,
    },
}

pub trait TerminalEvents: Send + Sync + 'static {
    fn emit(&self, event: TerminalEvent);
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSpec {
    pub session_id: String,
    pub cwd: Option<String>,
    /// Explicit argv. The Tauri command layer never forwards a caller-supplied
    /// value here: web content must not be able to choose the program.
    pub command: Option<Vec<String>>,
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalInfo {
    pub terminal_id: String,
    pub session_id: String,
    pub epoch: u64,
    pub pid: Option<u32>,
    /// The directory the child was actually started in. It is the *initial*
    /// cwd only: the running shell can `cd` and we do not observe that.
    pub cwd: Option<String>,
    pub command_label: String,
    pub started_at: i64,
    pub running: bool,
    pub exit: Option<ExitInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSnapshot {
    pub info: TerminalInfo,
    pub data: String,
    pub next_seq: u64,
    pub truncated: bool,
}

// --- streaming UTF-8 --------------------------------------------------------

/// Decodes a byte stream incrementally, holding back an incomplete trailing
/// sequence until the next read completes it.
#[derive(Debug, Default)]
pub struct Utf8Decoder {
    pending: Vec<u8>,
}

impl Utf8Decoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, bytes: &[u8]) -> String {
        self.pending.extend_from_slice(bytes);
        let mut out = String::new();
        loop {
            match std::str::from_utf8(&self.pending) {
                Ok(s) => {
                    out.push_str(s);
                    self.pending.clear();
                    return out;
                }
                Err(e) => {
                    let valid = e.valid_up_to();
                    if valid > 0 {
                        // Safe by construction: `valid_up_to` is a UTF-8 boundary.
                        out.push_str(std::str::from_utf8(&self.pending[..valid]).unwrap_or(""));
                    }
                    match e.error_len() {
                        // Truncated sequence: keep it for the next read.
                        None => {
                            self.pending.drain(..valid);
                            return out;
                        }
                        // Genuinely invalid bytes: substitute and continue.
                        Some(len) => {
                            out.push('\u{FFFD}');
                            self.pending.drain(..valid + len);
                        }
                    }
                }
            }
        }
    }
}

// --- bounded scrollback -----------------------------------------------------

#[derive(Debug)]
struct OutputState {
    chunks: VecDeque<String>,
    total_chars: usize,
    truncated: bool,
    next_seq: u64,
}

impl OutputState {
    fn new() -> Self {
        Self {
            chunks: VecDeque::new(),
            total_chars: 0,
            truncated: false,
            next_seq: 1,
        }
    }

    fn take_seq(&mut self) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        seq
    }

    fn push(&mut self, chunk: &str) {
        let len = chunk.chars().count();
        if len == 0 {
            return;
        }
        self.chunks.push_back(chunk.to_string());
        self.total_chars += len;
        while self.total_chars > RETAINED_OUTPUT_CHARS {
            let excess = self.total_chars - RETAINED_OUTPUT_CHARS;
            let Some(front) = self.chunks.front_mut() else {
                break;
            };
            let front_len = front.chars().count();
            self.truncated = true;
            if front_len <= excess {
                self.total_chars -= front_len;
                self.chunks.pop_front();
            } else {
                // Split on a character boundary; never mid-codepoint.
                let cut = front
                    .char_indices()
                    .nth(excess)
                    .map(|(i, _)| i)
                    .unwrap_or(front.len());
                front.drain(..cut);
                self.total_chars -= excess;
            }
        }
    }

    fn text(&self) -> String {
        let mut s = String::with_capacity(self.total_chars);
        for c in &self.chunks {
            s.push_str(c);
        }
        s
    }
}

#[derive(Debug)]
struct Lifecycle {
    running: bool,
    reader_done: bool,
    exit_emitted: bool,
    /// Set once the terminal's session has been signalled, so the bounded kill
    /// grace is paid once per terminal rather than once per caller.
    killed: bool,
    exit: Option<ExitInfo>,
}

struct TerminalHandle {
    terminal_id: String,
    session_id: String,
    epoch: u64,
    pid: Option<u32>,
    cwd: Option<String>,
    command_label: String,
    started_at: i64,
    output: Mutex<OutputState>,
    life: Mutex<Lifecycle>,
    signal: Condvar,
    writer: Mutex<Option<Box<dyn Write + Send>>>,
    master: Mutex<Option<Box<dyn MasterPty + Send>>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
}

impl TerminalHandle {
    fn info(&self) -> TerminalInfo {
        let life = self.life.lock().expect("lifecycle mutex");
        TerminalInfo {
            terminal_id: self.terminal_id.clone(),
            session_id: self.session_id.clone(),
            epoch: self.epoch,
            pid: self.pid,
            cwd: self.cwd.clone(),
            command_label: self.command_label.clone(),
            started_at: self.started_at,
            running: life.running,
            exit: life.exit.clone(),
        }
    }

    /// A coherent cut of lifecycle and output cursor.
    ///
    /// Holding `output` across the lifecycle read is what makes the pair
    /// consistent: the waiter reserves the exit's sequence number and marks the
    /// terminal exited under that same lock, so this can never return a cursor
    /// that supersedes an exit missing from `info`.
    fn cut(&self) -> TerminalSnapshot {
        let out = self.output.lock().expect("output mutex");
        let info = self.info();
        TerminalSnapshot {
            info,
            data: out.text(),
            next_seq: out.next_seq,
            truncated: out.truncated,
        }
    }

    /// Reserves the exit's sequence number and publishes the lifecycle change
    /// atomically with respect to [`TerminalHandle::cut`]. Returns the sequence
    /// number the Exit event must carry.
    fn publish_exit(&self, exit: ExitInfo) -> u64 {
        let mut out = self.output.lock().expect("output mutex");
        let seq = out.take_seq();
        let mut life = self.life.lock().expect("lifecycle mutex");
        life.running = false;
        life.exit = Some(exit);
        life.exit_emitted = true;
        self.signal.notify_all();
        seq
    }
}

pub struct TerminalManager {
    sink: Arc<dyn TerminalEvents>,
    lifecycle: Arc<dyn TerminalLifecycle>,
    terminals: Mutex<HashMap<String, Arc<TerminalHandle>>>,
    epochs: AtomicU64,
    clock: Arc<MonotonicClock>,
}

impl TerminalManager {
    pub fn new(sink: Arc<dyn TerminalEvents>) -> TerminalManager {
        TerminalManager::with_lifecycle(sink, Arc::new(NoLifecycle))
    }

    pub fn with_lifecycle(
        sink: Arc<dyn TerminalEvents>,
        lifecycle: Arc<dyn TerminalLifecycle>,
    ) -> TerminalManager {
        TerminalManager {
            sink,
            lifecycle,
            terminals: Mutex::new(HashMap::new()),
            epochs: AtomicU64::new(0),
            clock: Arc::new(MonotonicClock::new()),
        }
    }

    fn map(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, HashMap<String, Arc<TerminalHandle>>>, AppError> {
        self.terminals
            .lock()
            .map_err(|_| AppError::internal("terminal registry poisoned by an earlier panic"))
    }

    fn handle(&self, terminal_id: &str) -> Result<Arc<TerminalHandle>, AppError> {
        self.map()?
            .get(terminal_id)
            .cloned()
            .ok_or_else(|| AppError::not_found("terminal", terminal_id))
    }

    pub fn start(&self, spec: TerminalSpec) -> Result<TerminalInfo, AppError> {
        if spec.session_id.trim().is_empty() {
            return Err(AppError::validation("session_id", "must not be blank"));
        }
        validate_dims(spec.cols, spec.rows)?;

        let cwd = match spec.cwd.as_deref() {
            None => None,
            Some(raw) => {
                let p = Path::new(raw);
                if !p.is_absolute() {
                    return Err(AppError::validation("cwd", "must be an absolute path"));
                }
                if !p.is_dir() {
                    return Err(AppError::validation(
                        "cwd",
                        format!("{raw} is not an existing directory"),
                    ));
                }
                Some(raw.to_string())
            }
        };

        let argv = match spec.command {
            Some(v) if v.is_empty() => {
                return Err(AppError::validation("command", "must not be empty"))
            }
            Some(v) => v,
            None => default_shell_argv(),
        };
        let command_label = argv.join(" ");

        // Reject a second live terminal for the same session; evict a finished
        // one so a session shows exactly one current terminal.
        {
            let mut map = self.map()?;
            if let Some(existing) = map
                .values()
                .find(|h| h.session_id == spec.session_id && h.info().running)
            {
                return Err(AppError::conflict(
                    "TERMINAL_ALREADY_RUNNING",
                    format!(
                        "session {} already has a running terminal ({})",
                        spec.session_id, existing.terminal_id
                    ),
                ));
            }
            map.retain(|_, h| h.session_id != spec.session_id);
        }

        // The id is minted, and the run recorded, before anything is spawned:
        // a terminal must never run without a durable owner to attribute its
        // end to. From here on `guard` settles that record — and kills and
        // reaps any child that comes to exist — on every path out of this
        // function except the final success, which disarms it.
        let terminal_id = format!("term_{}", Uuid::new_v4().simple());
        self.lifecycle.started(&terminal_id, &spec.session_id)?;
        let mut guard = StartGuard::new(self.lifecycle.clone(), terminal_id.clone());

        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize {
                rows: spec.rows,
                cols: spec.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| AppError::internal(format!("openpty failed: {e}")))?;

        let mut cmd = CommandBuilder::new(&argv[0]);
        for a in &argv[1..] {
            cmd.arg(a);
        }
        if let Some(dir) = &cwd {
            cmd.cwd(dir);
        }
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| AppError::internal(format!("failed to spawn {command_label}: {e}")))?;
        // Drop our slave handle so the reader sees EOF once the child is gone.
        drop(pair.slave);

        let killer = child.clone_killer();
        let pid = child.process_id();
        let child: SharedChild = Arc::new(Mutex::new(child));
        // A real child exists now: every failure path below must kill and
        // synchronously reap it, since no waiter thread owns that yet.
        guard.attach_child(pid, child.clone());

        let reader = match pair.master.try_clone_reader() {
            Ok(r) => r,
            Err(e) => return Err(AppError::internal(format!("pty reader unavailable: {e}"))),
        };
        let writer = match pair.master.take_writer() {
            Ok(w) => w,
            Err(e) => return Err(AppError::internal(format!("pty writer unavailable: {e}"))),
        };

        let handle = Arc::new(TerminalHandle {
            terminal_id: terminal_id.clone(),
            session_id: spec.session_id.clone(),
            epoch: self.epochs.fetch_add(1, Ordering::SeqCst) + 1,
            pid,
            cwd,
            command_label,
            started_at: self.clock.now_ms(),
            output: Mutex::new(OutputState::new()),
            life: Mutex::new(Lifecycle {
                running: true,
                reader_done: false,
                exit_emitted: false,
                killed: false,
                exit: None,
            }),
            signal: Condvar::new(),
            writer: Mutex::new(Some(writer)),
            master: Mutex::new(Some(pair.master)),
            killer: Mutex::new(killer),
        });

        // Reader thread: decode and publish output.
        {
            let handle = handle.clone();
            let sink = self.sink.clone();
            let mut reader = reader;
            let spawned = spawn_named(
                format!("faiden-pty-read-{}", handle.terminal_id),
                move || {
                    let mut decoder = Utf8Decoder::new();
                    let mut buf = [0u8; READ_BUF];
                    loop {
                        match reader.read(&mut buf) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                let text = decoder.push(&buf[..n]);
                                for piece in split_bounded(&text, MAX_EVENT_CHARS) {
                                    let seq = {
                                        let mut out = handle.output.lock().expect("output mutex");
                                        out.push(&piece);
                                        out.take_seq()
                                    };
                                    sink.emit(TerminalEvent::Output {
                                        terminal_id: handle.terminal_id.clone(),
                                        epoch: handle.epoch,
                                        seq,
                                        chunk: piece,
                                    });
                                }
                            }
                        }
                    }
                    let mut life = handle.life.lock().expect("lifecycle mutex");
                    life.reader_done = true;
                    handle.signal.notify_all();
                },
            );
            if let Err(e) = spawned {
                return Err(AppError::internal(format!("reader thread: {e}")));
            }
        }

        // Waiter thread: owns `wait()`, so the child is reaped exactly once.
        {
            let handle = handle.clone();
            let sink = self.sink.clone();
            let clock = self.clock.clone();
            let lifecycle = self.lifecycle.clone();
            let child = child.clone();
            let spawned = spawn_named(
                format!("faiden-pty-wait-{}", handle.terminal_id),
                move || {
                    let status = child.lock().expect("child mutex").wait();
                    let (exit_code, success, description) = match status {
                        Ok(s) => (s.exit_code(), s.success(), s.to_string()),
                        Err(e) => (1, false, format!("exit status unavailable: {e}")),
                    };

                    // Let the reader drain what the child wrote before dying,
                    // but never block forever on a grandchild holding the pty.
                    {
                        let mut life = handle.life.lock().expect("lifecycle mutex");
                        let deadline = Instant::now() + DRAIN_GRACE;
                        while !life.reader_done {
                            let remaining = deadline.saturating_duration_since(Instant::now());
                            if remaining.is_zero() {
                                break;
                            }
                            let (g, _) = handle
                                .signal
                                .wait_timeout(life, remaining)
                                .expect("lifecycle mutex");
                            life = g;
                        }
                    }

                    let exit = ExitInfo {
                        exit_code,
                        success,
                        description,
                        at: clock.now_ms(),
                    };
                    // Durable first, then observable: by the time `close` or a
                    // subscriber can see the exit, the session already records
                    // how this run ended.
                    lifecycle.ended(&handle.terminal_id, &exit_outcome(&exit));
                    let seq = handle.publish_exit(exit);
                    sink.emit(TerminalEvent::Exit {
                        terminal_id: handle.terminal_id.clone(),
                        epoch: handle.epoch,
                        seq,
                        exit_code,
                        success,
                    });
                },
            );
            if let Err(e) = spawned {
                return Err(AppError::internal(format!("waiter thread: {e}")));
            }
        }

        self.map()?
            .insert(handle.terminal_id.clone(), handle.clone());
        guard.disarm();
        Ok(handle.info())
    }

    pub fn write(&self, terminal_id: &str, data: &str) -> Result<(), AppError> {
        if data.chars().count() > MAX_WRITE_CHARS {
            return Err(AppError::validation(
                "data",
                format!("a single write is limited to {MAX_WRITE_CHARS} characters"),
            ));
        }
        let handle = self.handle(terminal_id)?;
        if !handle.life.lock().expect("lifecycle mutex").running {
            return Err(AppError::conflict(
                "TERMINAL_EXITED",
                format!("terminal {terminal_id} has already exited"),
            ));
        }
        let mut guard = handle.writer.lock().expect("writer mutex");
        let writer = guard.as_mut().ok_or_else(|| {
            AppError::conflict(
                "TERMINAL_EXITED",
                format!("terminal {terminal_id} is closed"),
            )
        })?;
        writer
            .write_all(data.as_bytes())
            .and_then(|_| writer.flush())
            .map_err(|e| AppError::internal(format!("pty write failed: {e}")))
    }

    pub fn resize(&self, terminal_id: &str, cols: u16, rows: u16) -> Result<(), AppError> {
        validate_dims(cols, rows)?;
        let handle = self.handle(terminal_id)?;
        let guard = handle.master.lock().expect("master mutex");
        let Some(master) = guard.as_ref() else {
            return Err(AppError::conflict(
                "TERMINAL_EXITED",
                format!("terminal {terminal_id} is closed"),
            ));
        };
        master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| AppError::internal(format!("resize failed: {e}")))
    }

    pub fn snapshot(&self, terminal_id: &str) -> Result<TerminalSnapshot, AppError> {
        Ok(self.handle(terminal_id)?.cut())
    }

    pub fn info(&self, terminal_id: &str) -> Result<TerminalInfo, AppError> {
        Ok(self.handle(terminal_id)?.info())
    }

    pub fn list(&self) -> Vec<TerminalInfo> {
        match self.map() {
            Ok(map) => {
                let mut v: Vec<TerminalInfo> = map.values().map(|h| h.info()).collect();
                v.sort_by_key(|i| i.started_at);
                v
            }
            Err(_) => Vec::new(),
        }
    }

    /// Kills the child, waits for it to be reaped, then forgets the id.
    pub fn close(&self, terminal_id: &str) -> Result<ExitInfo, AppError> {
        let handle = self.handle(terminal_id)?;
        let exit = self.terminate(&handle);
        self.map()?.remove(terminal_id);
        // Release the pty ends now that nothing can address this terminal.
        *handle.writer.lock().expect("writer mutex") = None;
        *handle.master.lock().expect("master mutex") = None;
        exit.ok_or_else(|| {
            AppError::conflict(
                "TERMINAL_KILL_TIMEOUT",
                format!(
                    "terminal {terminal_id} was signalled but its exit was not observed within {}s",
                    CLOSE_TIMEOUT.as_secs()
                ),
            )
        })
    }

    /// Best-effort shutdown of every child. Called when the app quits: this
    /// build has no background service, so quitting really does end terminals.
    ///
    /// The quit outcome is recorded *before* anything is signalled, so the
    /// durable record says why the terminal ended rather than reporting the
    /// SIGKILL that the exit path would otherwise attribute it to. Recording is
    /// first-writer-wins, so a terminal that had already exited on its own keeps
    /// its real exit code.
    pub fn shutdown_all(&self) {
        let handles: Vec<Arc<TerminalHandle>> = match self.map() {
            Ok(map) => map.values().cloned().collect(),
            Err(_) => return,
        };
        for h in &handles {
            if h.life.lock().expect("lifecycle mutex").running {
                self.lifecycle.ended(&h.terminal_id, OUTCOME_QUIT);
            }
        }
        // Signal every terminal before waiting on any of them, so the bounded
        // grace periods overlap instead of adding up.
        for h in &handles {
            self.kill(h);
        }
        for h in &handles {
            let _ = self.terminate(h);
            *h.writer.lock().expect("writer mutex") = None;
            *h.master.lock().expect("master mutex") = None;
        }
        if let Ok(mut map) = self.map() {
            map.clear();
        }
    }

    /// Terminates the terminal's whole session, once. Reaping the leader stays
    /// with the waiter thread, which owns `wait()`.
    fn kill(&self, handle: &Arc<TerminalHandle>) {
        {
            let mut life = handle.life.lock().expect("lifecycle mutex");
            if life.exit_emitted || life.killed {
                return;
            }
            life.killed = true;
        }
        kill_process_tree(handle.pid);
        // Also ask portable-pty directly: on a platform with no session
        // implementation this is the entire mechanism, and it is harmless on a
        // process that is already dead but not yet reaped.
        let _ = handle.killer.lock().expect("killer mutex").kill();
    }

    fn terminate(&self, handle: &Arc<TerminalHandle>) -> Option<ExitInfo> {
        self.kill(handle);
        let mut life = handle.life.lock().expect("lifecycle mutex");
        let deadline = Instant::now() + CLOSE_TIMEOUT;
        while !life.exit_emitted {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            let (g, _) = handle
                .signal
                .wait_timeout(life, remaining)
                .expect("lifecycle mutex");
            life = g;
        }
        life.exit.clone()
    }
}

fn validate_dims(cols: u16, rows: u16) -> Result<(), AppError> {
    if !(MIN_DIM..=MAX_DIM).contains(&cols) {
        return Err(AppError::validation(
            "cols",
            format!("must be between {MIN_DIM} and {MAX_DIM}"),
        ));
    }
    if !(MIN_DIM..=MAX_DIM).contains(&rows) {
        return Err(AppError::validation(
            "rows",
            format!("must be between {MIN_DIM} and {MAX_DIM}"),
        ));
    }
    Ok(())
}

fn split_bounded(text: &str, max_chars: usize) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }
    if text.chars().count() <= max_chars {
        return vec![text.to_string()];
    }
    let mut out = Vec::new();
    let mut current = String::new();
    let mut count = 0usize;
    for ch in text.chars() {
        current.push(ch);
        count += 1;
        if count == max_chars {
            out.push(std::mem::take(&mut current));
            count = 0;
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// The user's login shell when it is a real absolute path, otherwise a
/// conservative fallback. Never taken from web content.
fn default_shell_argv() -> Vec<String> {
    let candidate = std::env::var("SHELL").ok().filter(|s| {
        let p = Path::new(s);
        p.is_absolute() && p.exists()
    });
    let shell = candidate.unwrap_or_else(|| {
        for fallback in ["/bin/zsh", "/bin/bash", "/bin/sh"] {
            if Path::new(fallback).exists() {
                return fallback.to_string();
            }
        }
        "/bin/sh".to_string()
    });
    vec![shell, "-l".to_string()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoder_holds_back_a_split_codepoint() {
        let mut d = Utf8Decoder::new();
        let bytes = "🧵".as_bytes(); // f0 9f a7 b5
        assert_eq!(
            d.push(&bytes[..2]),
            "",
            "an incomplete sequence emits nothing"
        );
        assert_eq!(d.push(&bytes[2..3]), "");
        assert_eq!(d.push(&bytes[3..]), "🧵", "completed on the next read");
    }

    #[test]
    fn decoder_emits_the_valid_prefix_immediately() {
        let mut d = Utf8Decoder::new();
        let mut bytes = b"ok:".to_vec();
        bytes.extend_from_slice(&"é".as_bytes()[..1]);
        assert_eq!(d.push(&bytes), "ok:");
        assert_eq!(d.push(&"é".as_bytes()[1..]), "é");
    }

    #[test]
    fn decoder_substitutes_genuinely_invalid_bytes() {
        let mut d = Utf8Decoder::new();
        assert_eq!(d.push(&[b'a', 0xC0, 0xC0, b'b']), "a\u{FFFD}\u{FFFD}b");
    }

    #[test]
    fn ring_buffer_drops_from_the_front_on_character_boundaries() {
        let mut s = OutputState::new();
        s.push(&"🧵".repeat(RETAINED_OUTPUT_CHARS));
        assert!(!s.truncated);
        s.push("tail");
        assert!(s.truncated);
        let text = s.text();
        assert_eq!(text.chars().count(), RETAINED_OUTPUT_CHARS);
        assert!(text.ends_with("tail"));
        assert!(
            !text.contains('\u{FFFD}'),
            "never split a codepoint when trimming"
        );
    }

    #[test]
    fn sequence_numbers_start_at_one_and_never_repeat() {
        let mut s = OutputState::new();
        assert_eq!(s.take_seq(), 1);
        assert_eq!(s.take_seq(), 2);
        assert_eq!(s.next_seq, 3);
    }

    #[test]
    fn split_bounded_respects_the_event_cap() {
        let pieces = split_bounded(&"x".repeat(MAX_EVENT_CHARS * 2 + 5), MAX_EVENT_CHARS);
        assert_eq!(pieces.len(), 3);
        assert_eq!(pieces[0].chars().count(), MAX_EVENT_CHARS);
        assert_eq!(pieces[2].chars().count(), 5);
        assert!(split_bounded("", MAX_EVENT_CHARS).is_empty());
    }

    // --- Startup fault injection: post-`started()` failures must settle the
    // durable run and never leave a real child unkilled/unreaped. ------------

    struct RecordingLifecycle {
        events: Mutex<Vec<(String, String)>>,
    }

    impl RecordingLifecycle {
        fn new() -> Arc<RecordingLifecycle> {
            Arc::new(RecordingLifecycle {
                events: Mutex::new(Vec::new()),
            })
        }
    }

    impl TerminalLifecycle for RecordingLifecycle {
        fn started(&self, _terminal_id: &str, _session_id: &str) -> Result<(), AppError> {
            Ok(())
        }
        fn ended(&self, terminal_id: &str, outcome: &str) {
            self.events
                .lock()
                .expect("events mutex")
                .push((terminal_id.to_string(), outcome.to_string()));
        }
    }

    struct NullSink;
    impl TerminalEvents for NullSink {
        fn emit(&self, _event: TerminalEvent) {}
    }

    /// True if any process on the machine has `needle` on its command line.
    /// Used with an improbable marker instead of a real pid so these tests
    /// never need the `TerminalInfo` that a failed `start` does not return.
    fn any_process_matching(needle: &str) -> bool {
        let out = std::process::Command::new("ps")
            .args(["-eo", "command"])
            .output()
            .expect("run ps");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .any(|l| l.contains(needle) && !l.contains("grep"))
    }

    fn fault_spec(session: &str, marker: &str) -> TerminalSpec {
        TerminalSpec {
            session_id: session.to_string(),
            cwd: None,
            command: Some(vec!["/bin/sleep".into(), marker.into()]),
            cols: 80,
            rows: 24,
        }
    }

    #[test]
    fn reader_thread_spawn_failure_kills_and_reaps_the_child_and_settles_the_run() {
        let marker = "398217"; // improbable real sleep duration; identifies our child only
        let lifecycle = RecordingLifecycle::new();
        let mgr = TerminalManager::with_lifecycle(Arc::new(NullSink), lifecycle.clone());
        fault_injection::arm("faiden-pty-read-");

        match mgr.start(fault_spec("s-reader-fault", marker)) {
            Err(AppError::Internal { .. }) => {}
            other => panic!("a failed reader spawn must surface as an error, got {other:?}"),
        }
        assert!(
            mgr.list().is_empty(),
            "a terminal that never finished starting must not be exposed"
        );

        let deadline = Instant::now() + Duration::from_secs(10);
        while any_process_matching(marker) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            !any_process_matching(marker),
            "the child must be killed and reaped, not left running with no waiter"
        );

        let recorded = lifecycle.events.lock().expect("events mutex");
        assert_eq!(
            recorded.len(),
            1,
            "the durable run must be settled rather than left open until a future restart"
        );
        assert!(
            !recorded[0].1.to_lowercase().contains("success"),
            "a setup failure must never be phrased as success: {:?}",
            recorded[0].1
        );
    }

    #[test]
    fn waiter_thread_spawn_failure_kills_and_reaps_the_child_and_settles_the_run() {
        let marker = "398218";
        let lifecycle = RecordingLifecycle::new();
        let mgr = TerminalManager::with_lifecycle(Arc::new(NullSink), lifecycle.clone());
        fault_injection::arm("faiden-pty-wait-");

        match mgr.start(fault_spec("s-waiter-fault", marker)) {
            Err(AppError::Internal { .. }) => {}
            other => panic!("a failed waiter spawn must surface as an error, got {other:?}"),
        }
        assert!(
            mgr.list().is_empty(),
            "a terminal that never finished starting must not be exposed"
        );

        let deadline = Instant::now() + Duration::from_secs(10);
        while any_process_matching(marker) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            !any_process_matching(marker),
            "with no waiter thread to own wait(), the guard must still kill and reap the child"
        );

        let recorded = lifecycle.events.lock().expect("events mutex");
        assert_eq!(
            recorded.len(),
            1,
            "the durable run must be settled rather than left open until a future restart"
        );
        assert!(
            !recorded[0].1.to_lowercase().contains("success"),
            "a setup failure must never be phrased as success: {:?}",
            recorded[0].1
        );
    }
}

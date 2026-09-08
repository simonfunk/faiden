//! Real PTY behaviour. These tests spawn actual `/bin/sh` children in
//! temporary directories and assert on real bytes, real exits and real reaping.

mod support;

use std::time::Duration;

use faiden_lib::error::AppError;
use faiden_lib::terminal::{
    TerminalEvent, TerminalManager, TerminalSpec, MAX_EVENT_CHARS, RETAINED_OUTPUT_CHARS,
};
use support::{
    exit_of, exit_seq_of, has_exit, pgid_of, process_alive, process_gone, text_of, wait_until,
    CollectingSink,
};
use tempfile::TempDir;

const T: Duration = Duration::from_secs(20);

fn spec(session: &str, script: &str) -> TerminalSpec {
    TerminalSpec {
        session_id: session.to_string(),
        cwd: None,
        command: Some(vec!["/bin/sh".into(), "-c".into(), script.into()]),
        cols: 100,
        rows: 30,
    }
}

#[test]
fn runs_a_real_command_and_reports_its_output_and_exit_status() {
    let sink = CollectingSink::new();
    let mgr = TerminalManager::new(sink.clone());

    let info = mgr
        .start(spec("s1", "printf 'faiden-live'; exit 7"))
        .expect("start");
    assert!(info.running, "a freshly started terminal is running");
    assert!(info.pid.is_some(), "a real child process has a pid");

    let id = info.terminal_id.clone();
    sink.wait_for(T, |e| has_exit(e, &id));

    assert!(
        sink.output_text(&id).contains("faiden-live"),
        "real stdout reaches the sink"
    );
    assert_eq!(
        exit_of(&sink.events(), &id),
        Some((7, false)),
        "the real exit code is reported"
    );

    let after = mgr
        .info(&id)
        .expect("terminal record is retained after exit");
    assert!(!after.running);
    assert_eq!(after.exit.as_ref().map(|x| x.exit_code), Some(7));
    assert!(
        sink.output_text(&id).contains("faiden-live"),
        "output retained alongside the exit status"
    );
}

#[test]
fn snapshot_replays_scrollback_and_tells_the_subscriber_where_to_resume() {
    let sink = CollectingSink::new();
    let mgr = TerminalManager::new(sink.clone());
    let info = mgr.start(spec("s1", "printf 'abc'; exit 0")).unwrap();
    let id = info.terminal_id.clone();
    sink.wait_for(T, |e| has_exit(e, &id));

    let snap = mgr.snapshot(&id).unwrap();
    assert!(snap.data.contains("abc"));
    assert!(!snap.truncated, "short output is not truncated");

    // Sequence numbers start at 1, never repeat, and next_seq is one past the last.
    let seqs: Vec<u64> = sink
        .events()
        .iter()
        .map(|e| match e {
            TerminalEvent::Output { seq, .. } | TerminalEvent::Exit { seq, .. } => *seq,
        })
        .collect();
    assert_eq!(seqs.first(), Some(&1));
    for w in seqs.windows(2) {
        assert!(
            w[1] > w[0],
            "sequence numbers are strictly increasing: {seqs:?}"
        );
    }
    assert_eq!(snap.next_seq, seqs.last().unwrap() + 1);

    // A late-subscribing view can rebuild exactly the retained scrollback.
    let replayed: String = sink
        .events()
        .iter()
        .filter_map(|e| match e {
            TerminalEvent::Output { seq, chunk, .. } if *seq < snap.next_seq => {
                Some(chunk.as_str())
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        replayed, snap.data,
        "snapshot equals the events it supersedes"
    );
}

#[test]
fn multibyte_output_survives_read_boundaries_without_replacement_characters() {
    let sink = CollectingSink::new();
    let mgr = TerminalManager::new(sink.clone());
    // 4000 x 4-byte characters = 16000 bytes, far beyond one read buffer, so a
    // codepoint is guaranteed to straddle a chunk boundary.
    let info = mgr
        .start(spec(
            "s1",
            "i=0; while [ $i -lt 4000 ]; do printf '\\360\\237\\247\\265'; i=$((i+1)); done",
        ))
        .unwrap();
    let id = info.terminal_id.clone();
    sink.wait_for(T, |e| has_exit(e, &id));

    let text = sink.output_text(&id);
    assert!(
        !text.contains('\u{FFFD}'),
        "no replacement characters: a codepoint was split across reads"
    );
    assert_eq!(
        text.matches('\u{1F9F5}').count(),
        4000,
        "every character arrives exactly once"
    );

    for e in sink.events() {
        if let TerminalEvent::Output { chunk, .. } = e {
            assert!(
                chunk.chars().count() <= MAX_EVENT_CHARS,
                "each IPC payload stays bounded ({} chars)",
                chunk.chars().count()
            );
        }
    }
}

#[test]
fn retained_output_is_bounded_and_reports_truncation() {
    let sink = CollectingSink::new();
    let mgr = TerminalManager::new(sink.clone());
    let bytes = RETAINED_OUTPUT_CHARS + 50_000;
    let info = mgr
        .start(spec(
            "s1",
            &format!("head -c {bytes} /dev/zero | tr '\\0' 'a'"),
        ))
        .unwrap();
    let id = info.terminal_id.clone();
    sink.wait_for(T, |e| has_exit(e, &id));

    let snap = mgr.snapshot(&id).unwrap();
    assert!(
        snap.data.chars().count() <= RETAINED_OUTPUT_CHARS,
        "retained scrollback is capped, got {} chars",
        snap.data.chars().count()
    );
    assert!(
        snap.truncated,
        "the user is told that earlier output was dropped"
    );
}

#[test]
fn a_terminal_keeps_running_while_no_view_is_attached() {
    let sink = CollectingSink::new();
    let mgr = TerminalManager::new(sink.clone());
    let info = mgr.start(spec("s1", "printf 'ready\\n'; cat")).unwrap();
    let id = info.terminal_id.clone();
    let pid = info.pid.unwrap();

    sink.wait_for(T, |e| text_of(e, &id).contains("ready"));

    // Simulate the user switching to another thread and back: the manager is
    // untouched, the child is still alive, and the scrollback is still there.
    assert!(mgr.info(&id).unwrap().running);
    assert!(
        !process_gone(pid),
        "switching views must not kill the child"
    );

    mgr.write(&id, "echo still-here\n")
        .expect("write to a live terminal");
    sink.wait_for(T, |e| text_of(e, &id).contains("still-here"));

    let snap = mgr.snapshot(&id).unwrap();
    assert!(
        snap.data.contains("ready"),
        "earlier output is replayable after switching back"
    );
    assert!(snap.info.running);

    mgr.close(&id).unwrap();
    assert!(wait_until(T, || process_gone(pid)), "close reaps the child");
}

#[test]
fn closing_kills_and_reaps_the_child_process() {
    let sink = CollectingSink::new();
    let mgr = TerminalManager::new(sink.clone());
    let info = mgr.start(spec("s1", "sleep 120")).unwrap();
    let id = info.terminal_id.clone();
    let pid = info.pid.unwrap();
    assert!(!process_gone(pid));

    mgr.close(&id).expect("close");

    assert!(
        wait_until(T, || process_gone(pid)),
        "the child must be killed and reaped, not left as a zombie"
    );
    sink.wait_for(T, |e| has_exit(e, &id));

    // After close the id is gone entirely.
    match mgr.info(&id) {
        Err(AppError::NotFound { entity, .. }) => assert_eq!(entity, "terminal"),
        other => panic!("closed terminal id must not resolve, got {other:?}"),
    }
    assert!(mgr.list().is_empty());
}

#[test]
fn writing_to_unknown_exited_or_closed_terminals_fails_closed() {
    let sink = CollectingSink::new();
    let mgr = TerminalManager::new(sink.clone());

    match mgr.write("term_does_not_exist", "x") {
        Err(AppError::NotFound { entity, .. }) => assert_eq!(entity, "terminal"),
        other => panic!("unknown terminal must fail closed, got {other:?}"),
    }

    let info = mgr.start(spec("s1", "exit 0")).unwrap();
    let id = info.terminal_id.clone();
    sink.wait_for(T, |e| has_exit(e, &id));

    match mgr.write(&id, "x") {
        Err(AppError::Conflict { code, .. }) => assert_eq!(code, "TERMINAL_EXITED"),
        other => panic!("writing to an exited terminal must fail closed, got {other:?}"),
    }

    mgr.close(&id).unwrap();
    match mgr.write(&id, "x") {
        Err(AppError::NotFound { entity, .. }) => assert_eq!(entity, "terminal"),
        other => panic!("closed terminal must fail closed, got {other:?}"),
    }
    // Closing twice is a no-op error, not a panic.
    assert!(mgr.close(&id).is_err());
}

#[test]
fn a_session_cannot_own_two_live_terminals_but_may_start_a_new_one_after_close() {
    let sink = CollectingSink::new();
    let mgr = TerminalManager::new(sink.clone());

    let first = mgr.start(spec("session-abc", "cat")).unwrap();
    match mgr.start(spec("session-abc", "cat")) {
        Err(AppError::Conflict { code, .. }) => assert_eq!(code, "TERMINAL_ALREADY_RUNNING"),
        other => panic!("duplicate start must be rejected, got {other:?}"),
    }
    assert_eq!(
        mgr.list().len(),
        1,
        "the rejected start left nothing behind"
    );

    mgr.close(&first.terminal_id).unwrap();
    let second = mgr.start(spec("session-abc", "cat")).unwrap();

    assert_ne!(
        second.terminal_id, first.terminal_id,
        "a new run gets a fresh id"
    );
    assert!(
        second.epoch > first.epoch,
        "epochs increase so late events cannot be misattributed"
    );
    mgr.close(&second.terminal_id).unwrap();
}

#[test]
fn resize_enforces_bounds_and_never_touches_an_unknown_terminal() {
    let sink = CollectingSink::new();
    let mgr = TerminalManager::new(sink.clone());
    let info = mgr.start(spec("s1", "cat")).unwrap();
    let id = info.terminal_id.clone();

    mgr.resize(&id, 120, 40).expect("a sane resize is accepted");

    for (cols, rows) in [(0u16, 40u16), (120, 0), (5000, 40), (120, 5000)] {
        match mgr.resize(&id, cols, rows) {
            Err(AppError::Validation { field, .. }) => {
                assert!(
                    field == "cols" || field == "rows",
                    "unexpected field {field}"
                )
            }
            other => panic!("resize {cols}x{rows} must be rejected, got {other:?}"),
        }
    }

    match mgr.resize("term_nope", 80, 24) {
        Err(AppError::NotFound { entity, .. }) => assert_eq!(entity, "terminal"),
        other => panic!("unknown terminal resize must fail closed, got {other:?}"),
    }
    mgr.close(&id).unwrap();
}

#[test]
fn the_child_actually_runs_in_the_requested_directory() {
    let dir = TempDir::new().unwrap();
    // macOS temp dirs live under a symlinked /var; compare against the resolved path.
    let resolved = std::fs::canonicalize(dir.path()).unwrap();
    let marker = resolved.join("faiden-marker.txt");
    std::fs::write(&marker, "x").unwrap();

    let sink = CollectingSink::new();
    let mgr = TerminalManager::new(sink.clone());
    let mut s = spec("s1", "ls faiden-marker.txt; exit 0");
    s.cwd = Some(resolved.to_string_lossy().to_string());
    let info = mgr.start(s).unwrap();
    let id = info.terminal_id.clone();
    sink.wait_for(T, |e| has_exit(e, &id));

    assert!(sink.output_text(&id).contains("faiden-marker.txt"));
    assert_eq!(exit_of(&sink.events(), &id), Some((0, true)));
    assert_eq!(
        info.cwd.as_deref(),
        Some(resolved.to_string_lossy().as_ref()),
        "the terminal records the directory it was actually started in"
    );
}

#[test]
fn a_missing_working_directory_is_rejected_before_anything_spawns() {
    let dir = TempDir::new().unwrap();
    let missing = dir.path().join("not-created");

    let sink = CollectingSink::new();
    let mgr = TerminalManager::new(sink.clone());
    let mut s = spec("s1", "true");
    s.cwd = Some(missing.to_string_lossy().to_string());

    match mgr.start(s) {
        Err(AppError::Validation { field, .. }) => assert_eq!(field, "cwd"),
        other => panic!("a missing cwd must be rejected, got {other:?}"),
    }
    assert!(mgr.list().is_empty(), "nothing was spawned");
    assert!(sink.events().is_empty());
}

struct RecordingLifecycle(std::sync::Mutex<Vec<(String, String)>>);

impl RecordingLifecycle {
    fn new() -> std::sync::Arc<RecordingLifecycle> {
        std::sync::Arc::new(RecordingLifecycle(std::sync::Mutex::new(Vec::new())))
    }
}

impl faiden_lib::terminal::TerminalLifecycle for RecordingLifecycle {
    fn started(&self, _terminal_id: &str, _session_id: &str) -> Result<(), AppError> {
        Ok(())
    }
    fn ended(&self, terminal_id: &str, outcome: &str) {
        self.0
            .lock()
            .unwrap()
            .push((terminal_id.to_string(), outcome.to_string()));
    }
}

#[test]
fn a_command_that_cannot_be_spawned_settles_the_durable_run_and_leaves_nothing_behind() {
    let sink = CollectingSink::new();
    let lifecycle = RecordingLifecycle::new();
    let mgr = TerminalManager::with_lifecycle(sink.clone(), lifecycle.clone());

    let mut s = spec("s1", "unused");
    s.command = Some(vec!["/nonexistent/faiden-fault-injection-binary".into()]);

    match mgr.start(s) {
        Err(AppError::Internal { .. }) => {}
        other => panic!("spawning a nonexistent binary must fail closed, got {other:?}"),
    }

    assert!(
        mgr.list().is_empty(),
        "a terminal that never got a child must not be exposed"
    );
    assert!(sink.events().is_empty());

    let recorded = lifecycle.0.lock().unwrap();
    assert_eq!(
        recorded.len(),
        1,
        "the durable run must be settled instead of left open for reconciliation to find later"
    );
    assert!(
        !recorded[0].1.to_lowercase().contains("success"),
        "a setup failure must never be phrased as success: {:?}",
        recorded[0].1
    );
}

#[test]
fn oversized_writes_are_rejected_rather_than_forwarded() {
    let sink = CollectingSink::new();
    let mgr = TerminalManager::new(sink.clone());
    let info = mgr.start(spec("s1", "cat")).unwrap();
    let id = info.terminal_id.clone();

    let huge = "z".repeat(faiden_lib::terminal::MAX_WRITE_CHARS + 1);
    match mgr.write(&id, &huge) {
        Err(AppError::Validation { field, .. }) => assert_eq!(field, "data"),
        other => panic!("oversized write must be rejected, got {other:?}"),
    }
    mgr.close(&id).unwrap();
}

#[test]
fn shutdown_all_terminates_every_child() {
    let sink = CollectingSink::new();
    let mgr = TerminalManager::new(sink.clone());
    let a = mgr.start(spec("s1", "sleep 120")).unwrap();
    let b = mgr.start(spec("s2", "sleep 120")).unwrap();
    let (pa, pb) = (a.pid.unwrap(), b.pid.unwrap());

    mgr.shutdown_all();

    assert!(
        wait_until(T, || process_gone(pa) && process_gone(pb)),
        "quitting kills every child"
    );
    assert!(mgr.list().is_empty());
}

// --- Descendant cleanup ------------------------------------------------------

/// An interactive shell with job control puts a background job in its *own*
/// process group. Killing the shell alone therefore leaves that job running and
/// orphaned, which is exactly what this asserts must not happen.
const BACKGROUND_JOB: &str =
    "set -m; sleep 300 & printf 'BG=%s\\n' \"$!\"; printf 'READY\\n'; sleep 300";

fn background_pid(sink: &CollectingSink, id: &str) -> u32 {
    let events = sink.wait_for(T, |e| text_of(e, id).contains("READY"));
    let text = text_of(&events, id);
    let line = text
        .lines()
        .find(|l| l.trim_start().starts_with("BG="))
        .unwrap_or_else(|| panic!("no BG= line in terminal output: {text:?}"));
    line.trim()
        .trim_start_matches("BG=")
        .trim()
        .parse()
        .unwrap_or_else(|e| panic!("unparsable background pid in {line:?}: {e}"))
}

/// A process started by the test harness, in the harness's own session. Nothing
/// Faiden does may touch it.
struct Bystander(std::process::Child);

impl Bystander {
    fn spawn() -> Bystander {
        Bystander(
            std::process::Command::new("/bin/sleep")
                .arg("300")
                .spawn()
                .expect("spawn bystander"),
        )
    }
    fn pid(&self) -> u32 {
        self.0.id()
    }
}

impl Drop for Bystander {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn closing_kills_background_descendants_of_the_shell_not_only_the_shell() {
    let sink = CollectingSink::new();
    let mgr = TerminalManager::new(sink.clone());
    let bystander = Bystander::spawn();

    let info = mgr
        .start(spec("s-descendant", BACKGROUND_JOB))
        .expect("start");
    let id = info.terminal_id.clone();
    let leader = info.pid.expect("a real child process");
    let background = background_pid(&sink, &id);

    assert!(
        process_alive(background),
        "the background job is running before close"
    );
    assert_ne!(
        pgid_of(background),
        pgid_of(leader),
        "with job control the background job really is in its own process group, \
         so killing the shell's group alone would not reach it"
    );

    mgr.close(&id).expect("close reports a real exit");

    assert!(
        wait_until(Duration::from_secs(10), || process_gone(leader)),
        "the shell itself is killed and reaped"
    );
    assert!(
        wait_until(Duration::from_secs(10), || process_gone(background)),
        "the background job the shell started is killed too, not orphaned"
    );
    assert!(
        process_alive(bystander.pid()),
        "cleanup is scoped to this terminal's own session and never touches \
         unrelated processes"
    );
}

#[test]
fn shutdown_all_kills_background_descendants_on_the_quit_path_too() {
    let sink = CollectingSink::new();
    let mgr = TerminalManager::new(sink.clone());
    let bystander = Bystander::spawn();

    let info = mgr.start(spec("s-quit", BACKGROUND_JOB)).expect("start");
    let id = info.terminal_id.clone();
    let leader = info.pid.expect("a real child process");
    let background = background_pid(&sink, &id);
    assert!(process_alive(background));

    mgr.shutdown_all();

    assert!(
        wait_until(Duration::from_secs(10), || process_gone(leader)),
        "quitting reaps the shell"
    );
    assert!(
        wait_until(Duration::from_secs(10), || process_gone(background)),
        "quitting leaves no orphaned background job behind"
    );
    assert!(process_alive(bystander.pid()));
    assert!(mgr.list().is_empty());
}

// --- Snapshot / exit coherence ----------------------------------------------

#[test]
fn a_snapshot_never_publishes_a_cursor_that_supersedes_an_exit_it_omits() {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use std::time::Instant;

    let overall = Instant::now();
    for attempt in 0..60 {
        if overall.elapsed() > Duration::from_secs(25) {
            break;
        }
        let sink = CollectingSink::new();
        let mgr = Arc::new(TerminalManager::new(sink.clone()));
        let info = mgr
            .start(spec(&format!("s-race-{attempt}"), "printf 'x'; exit 5"))
            .expect("start");
        let id = info.terminal_id.clone();

        // Hammer the snapshot from several views at once, exactly as
        // re-attaching during a thread switch does, and remember the highest
        // cursor any snapshot published while still claiming to be running.
        let highest_running_cursor = Arc::new(AtomicU64::new(0));
        let mut spinners = Vec::new();
        for _ in 0..4 {
            let mgr = mgr.clone();
            let id = id.clone();
            let highest = highest_running_cursor.clone();
            spinners.push(std::thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(5);
                loop {
                    match mgr.snapshot(&id) {
                        Ok(s) if s.info.running => {
                            highest.fetch_max(s.next_seq, Ordering::SeqCst);
                        }
                        _ => return,
                    }
                    if Instant::now() > deadline {
                        return;
                    }
                }
            }));
        }
        for s in spinners {
            s.join().unwrap();
        }

        let events = sink.wait_for(T, |e| has_exit(e, &id));
        let exit_seq = exit_seq_of(&events, &id).expect("an exit event was emitted");
        let cursor = highest_running_cursor.load(Ordering::SeqCst);
        assert!(
            exit_seq >= cursor,
            "attempt {attempt}: a snapshot reported running=true with cursor {cursor}, which \
             already supersedes exit event seq {exit_seq}. A view resuming from that cursor \
             discards the exit and stays falsely 'Running'."
        );
    }
}

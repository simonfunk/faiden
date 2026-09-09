//! The ACP runtime, driven against a real subprocess over real pipes.
//!
//! The peer is a **fixture**, not Hermes (see `support/agent_fixture.rs`).
//! Nothing here contacts a provider, the network or `~/.hermes`; every database
//! is a `TempDir` and every working directory is temporary.

mod support;

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use faiden_lib::agent::runtime::{
    arm_inbound_permission_gate_for_test, arm_snapshot_cut_gate_for_test, AgentEvent, AgentManager,
    AgentSnapshot, AgentSpec, AgentStatus, AgentTimeouts,
};
use faiden_lib::store::{NewSession, Store};
use tempfile::TempDir;

use support::agent_fixture::{
    agent_text, fixture_peer, has_status, notes_dir, read_note, status_trail, CollectingAgentSink,
};

const WAIT: Duration = Duration::from_secs(10);

struct Harness {
    _temp: TempDir,
    dir: std::path::PathBuf,
    store: Arc<Store>,
    sink: Arc<CollectingAgentSink>,
    manager: Arc<AgentManager>,
}

fn harness() -> Harness {
    let temp = TempDir::new().unwrap();
    let dir = temp.path().to_path_buf();
    let store = Arc::new(Store::open(&dir.join("faiden.sqlite3")).unwrap());
    let sink = CollectingAgentSink::new();
    let manager = Arc::new(AgentManager::new(sink.clone(), store.clone()));
    Harness {
        _temp: temp,
        dir,
        store,
        sink,
        manager,
    }
}

impl Harness {
    /// A thread plus an agent-kind session to own the run.
    fn session(&self) -> String {
        let thread = self.store.create_thread("ACP thread").unwrap();
        self.store
            .create_session(
                &thread.id,
                NewSession {
                    label: "Hermes session".to_string(),
                    kind: "hermes-acp".to_string(),
                    predecessor_id: None,
                    briefing: String::new(),
                },
            )
            .unwrap()
            .id
    }

    fn spec(&self, program: &Path, cwd: &Path) -> AgentSpec {
        self.spec_for(&self.session(), program, cwd)
    }

    /// A spec for an already-persisted session, so two starts can contend for
    /// the same owner.
    fn spec_for(&self, session_id: &str, program: &Path, cwd: &Path) -> AgentSpec {
        AgentSpec {
            session_id: session_id.to_string(),
            program: program.to_path_buf(),
            args: vec!["acp".to_string()],
            cwd: cwd.to_path_buf(),
            env: vec![(
                "FIXTURE_NOTES".to_string(),
                notes_dir(&self.dir).to_string_lossy().to_string(),
            )],
            source: "fixture peer (not Hermes)".to_string(),
            timeouts: AgentTimeouts {
                handshake: Duration::from_secs(10),
                permission: Duration::from_secs(10),
                shutdown: Duration::from_secs(5),
            },
        }
    }

    fn notes(&self) -> std::path::PathBuf {
        notes_dir(&self.dir)
    }
}

/// The everyday fixture: handshake, then one streamed turn per prompt.
const ECHO_PEER: &str = r#"
sid = "fixture-session"
handshake(sid)
while True:
    msg = recv()
    if msg is None:
        break
    if msg.get("method") == "session/prompt":
        text = msg["params"]["prompt"][0]["text"]
        text_chunk(sid, "You said: ")
        text_chunk(sid, text)
        reply(msg, {"stopReason": "end_turn"})
"#;

#[test]
fn a_live_process_is_only_connecting_until_initialize_and_session_new_both_answer() {
    let h = harness();
    // The peer answers `initialize` but stalls on `session/new`.
    let peer = fixture_peer(
        &h.dir,
        "stalling",
        r#"
msg = recv()
reply(msg, {"protocolVersion": 1})
note("initialized", "yes")
time.sleep(30)
"#,
    );
    let run = h.manager.start(h.spec(&peer, &h.dir)).unwrap();

    read_note(&h.notes(), "initialized", WAIT);
    // The child is alive and has answered one request. That is not "ready".
    std::thread::sleep(Duration::from_millis(150));
    let info = h.manager.info(&run.run_id).unwrap();
    assert_eq!(info.status, AgentStatus::Connecting);
    assert_eq!(info.acp_session_id, None);
    assert!(info.pid.is_some(), "a child really is running");
    assert!(
        !has_status(&h.sink.events(), &run.run_id, "ready"),
        "a running process is never reported as ready on its own"
    );

    h.manager.stop(&run.run_id).unwrap();
}

#[test]
fn the_handshake_reaches_ready_and_records_the_agents_own_session_id() {
    let h = harness();
    let peer = fixture_peer(&h.dir, "echo", ECHO_PEER);
    let run = h.manager.start(h.spec(&peer, &h.dir)).unwrap();
    assert_eq!(run.status, AgentStatus::Connecting);

    h.sink
        .wait_for(WAIT, "ready", |e| has_status(e, &run.run_id, "ready"));
    let info = h.manager.info(&run.run_id).unwrap();
    assert_eq!(info.status, AgentStatus::Ready);
    assert_eq!(info.acp_session_id.as_deref(), Some("fixture-session"));
    assert_eq!(
        status_trail(&h.sink.events(), &run.run_id),
        vec!["connecting", "ready"]
    );

    // Durable, not merely in memory.
    let stored = h.store.get_agent_run(&run.run_id).unwrap();
    assert_eq!(stored.acp_session_id.as_deref(), Some("fixture-session"));

    h.manager.stop(&run.run_id).unwrap();
}

#[test]
fn the_child_gets_argv_acp_a_real_cwd_and_no_inherited_approval_bypass() {
    let h = harness();
    let workdir = h.dir.join("work");
    std::fs::create_dir_all(&workdir).unwrap();
    let peer = fixture_peer(
        &h.dir,
        "introspect",
        r#"
note("argv", sys.argv[1:])
note("cwd", os.getcwd())
note("yolo", os.environ.get("HERMES_YOLO_MODE", "<unset>"))
note("session_new_cwd", "")
handshake()
time.sleep(30)
"#,
    );

    let mut spec = h.spec(&peer, &workdir);
    // Simulate a parent shell that exported a bypass. It must not survive.
    spec.env
        .push(("HERMES_YOLO_MODE".to_string(), "1".to_string()));
    let run = h.manager.start(spec).unwrap();

    assert_eq!(read_note(&h.notes(), "argv", WAIT), r#"["acp"]"#);
    let cwd = read_note(&h.notes(), "cwd", WAIT);
    assert!(
        Path::new(&cwd).canonicalize().unwrap() == workdir.canonicalize().unwrap(),
        "child started in {cwd}, expected {workdir:?}"
    );
    assert_eq!(read_note(&h.notes(), "yolo", WAIT), "<unset>");

    let params = read_note(&h.notes(), "session_new_params", WAIT);
    let params: serde_json::Value = serde_json::from_str(&params).unwrap();
    assert_eq!(
        Path::new(params["cwd"].as_str().unwrap())
            .canonicalize()
            .unwrap(),
        workdir.canonicalize().unwrap(),
        "session/new names the selected directory"
    );
    assert_eq!(params["mcpServers"], serde_json::json!([]));

    h.manager.stop(&run.run_id).unwrap();
}

#[test]
fn a_failed_initialize_settles_the_run_as_error_rather_than_masquerading_as_ready() {
    let h = harness();
    let peer = fixture_peer(
        &h.dir,
        "refuses",
        r#"
msg = recv()
fail(msg, -32000, "no provider configured")
time.sleep(30)
"#,
    );
    let run = h.manager.start(h.spec(&peer, &h.dir)).unwrap();

    h.sink
        .wait_for(WAIT, "error", |e| has_status(e, &run.run_id, "error"));
    let info = h.manager.info(&run.run_id).unwrap();
    assert_eq!(info.status, AgentStatus::Error);
    assert!(
        info.detail
            .as_deref()
            .unwrap_or("")
            .contains("no provider configured"),
        "the agent's own words are shown: {:?}",
        info.detail
    );
    assert!(!has_status(&h.sink.events(), &run.run_id, "ready"));

    // A failed startup still settles the durable session; it does not linger open.
    let stored = h.store.get_agent_run(&run.run_id).unwrap();
    assert!(stored.ended_at.is_some());
    assert!(stored.outcome.unwrap().contains("no provider configured"));
}

#[test]
fn a_failed_session_new_settles_the_run_as_error() {
    let h = harness();
    let peer = fixture_peer(
        &h.dir,
        "refuses_session",
        r#"
msg = recv()
reply(msg, {"protocolVersion": 1})
msg = recv()
fail(msg, -32001, "cwd is not readable")
time.sleep(30)
"#,
    );
    let run = h.manager.start(h.spec(&peer, &h.dir)).unwrap();
    h.sink
        .wait_for(WAIT, "error", |e| has_status(e, &run.run_id, "error"));
    assert!(h
        .manager
        .info(&run.run_id)
        .unwrap()
        .detail
        .unwrap()
        .contains("cwd is not readable"));
}

#[test]
fn streamed_chunks_and_tool_updates_land_in_the_durable_transcript() {
    let h = harness();
    let peer = fixture_peer(
        &h.dir,
        "tools",
        r#"
sid = "fixture-session"
handshake(sid)
msg = recv()
text_chunk(sid, "Reading. ")
update(sid, {"sessionUpdate": "tool_call", "toolCallId": "call-1",
             "title": "Read README.md", "kind": "read", "status": "pending"})
update(sid, {"sessionUpdate": "tool_call_update", "toolCallId": "call-1",
             "status": "completed",
             "content": [{"type": "content", "content": {"type": "text", "text": "12 lines"}}]})
update(sid, {"sessionUpdate": "agent_thought_chunk",
             "content": {"type": "text", "text": "internal"}})
text_chunk(sid, "Done.")
reply(msg, {"stopReason": "end_turn"})
time.sleep(30)
"#,
    );
    let run = h.manager.start(h.spec(&peer, &h.dir)).unwrap();
    h.sink
        .wait_for(WAIT, "ready", |e| has_status(e, &run.run_id, "ready"));
    h.manager.prompt(&run.run_id, "read the readme").unwrap();

    h.sink.wait_for(WAIT, "turn complete", |e| {
        has_status(e, &run.run_id, "turnComplete")
    });

    let snapshot = h.manager.snapshot(&run.run_id).unwrap();
    let roles: Vec<&str> = snapshot.messages.iter().map(|m| m.role.as_str()).collect();
    assert_eq!(roles, vec!["user", "agent", "tool", "thought"]);

    let agent = snapshot
        .messages
        .iter()
        .find(|m| m.role == "agent")
        .unwrap();
    assert_eq!(
        agent.body, "Reading. Done.",
        "chunks accumulate into one entry"
    );

    let tool = snapshot.messages.iter().find(|m| m.role == "tool").unwrap();
    assert_eq!(tool.status.as_deref(), Some("completed"));
    assert!(tool.body.contains("Read README.md"), "got {}", tool.body);
    assert!(tool.body.contains("12 lines"), "got {}", tool.body);

    // The same transcript survives a restart of the store.
    let (durable, _) = h.store.list_agent_messages(&run.run_id, 100).unwrap();
    assert_eq!(durable.len(), 4);

    h.manager.stop(&run.run_id).unwrap();
}

#[test]
fn a_finished_turn_reports_its_literal_stop_reason_and_claims_nothing_more() {
    let h = harness();
    let peer = fixture_peer(
        &h.dir,
        "refusal",
        r#"
sid = "fixture-session"
handshake(sid)
msg = recv()
reply(msg, {"stopReason": "refusal"})
time.sleep(30)
"#,
    );
    let run = h.manager.start(h.spec(&peer, &h.dir)).unwrap();
    h.sink
        .wait_for(WAIT, "ready", |e| has_status(e, &run.run_id, "ready"));
    h.manager.prompt(&run.run_id, "do something").unwrap();
    h.sink.wait_for(WAIT, "turn complete", |e| {
        has_status(e, &run.run_id, "turnComplete")
    });

    let detail = h.manager.info(&run.run_id).unwrap().detail.unwrap();
    assert!(detail.contains("refusal"), "got {detail}");
    assert!(
        !detail.to_lowercase().contains("success"),
        "a stop reason is not a success claim: {detail}"
    );
    h.manager.stop(&run.run_id).unwrap();
}

#[test]
fn two_sessions_run_at_once_without_mixing_their_transcripts_or_processes() {
    let h = harness();
    let one = fixture_peer(
        &h.dir,
        "peer_one",
        r#"
sid = "session-one"
handshake(sid)
while True:
    msg = recv()
    if msg is None: break
    if msg.get("method") == "session/prompt":
        text_chunk(sid, "ONE:" + msg["params"]["prompt"][0]["text"])
        reply(msg, {"stopReason": "end_turn"})
"#,
    );
    let two = fixture_peer(
        &h.dir,
        "peer_two",
        r#"
sid = "session-two"
handshake(sid)
while True:
    msg = recv()
    if msg is None: break
    if msg.get("method") == "session/prompt":
        time.sleep(0.2)
        text_chunk(sid, "TWO:" + msg["params"]["prompt"][0]["text"])
        reply(msg, {"stopReason": "end_turn"})
"#,
    );

    let a = h.manager.start(h.spec(&one, &h.dir)).unwrap();
    let b = h.manager.start(h.spec(&two, &h.dir)).unwrap();
    h.sink.wait_for(WAIT, "both ready", |e| {
        has_status(e, &a.run_id, "ready") && has_status(e, &b.run_id, "ready")
    });

    assert_ne!(a.run_id, b.run_id);
    assert_ne!(
        h.manager.info(&a.run_id).unwrap().pid,
        h.manager.info(&b.run_id).unwrap().pid,
        "each owned session is its own isolated process"
    );

    h.manager.prompt(&a.run_id, "alpha").unwrap();
    h.manager.prompt(&b.run_id, "beta").unwrap();
    h.sink.wait_for(WAIT, "both turns complete", |e| {
        has_status(e, &a.run_id, "turnComplete") && has_status(e, &b.run_id, "turnComplete")
    });

    let events = h.sink.events();
    assert_eq!(agent_text(&events, &a.run_id), "ONE:alpha");
    assert_eq!(agent_text(&events, &b.run_id), "TWO:beta");

    h.manager.shutdown_all();
}

#[test]
fn replies_are_matched_by_id_and_a_reply_to_an_unknown_id_is_dropped() {
    let h = harness();
    let peer = fixture_peer(
        &h.dir,
        "noisy_ids",
        r#"
sid = "fixture-session"
handshake(sid)
msg = recv()
# A reply to an id we never issued, and a duplicate of a settled one.
send({"jsonrpc": "2.0", "id": 99999, "result": {"stopReason": "end_turn"}})
send({"jsonrpc": "2.0", "id": 1, "result": {"protocolVersion": 1}})
text_chunk(sid, "still here")
reply(msg, {"stopReason": "end_turn"})
time.sleep(30)
"#,
    );
    let run = h.manager.start(h.spec(&peer, &h.dir)).unwrap();
    h.sink
        .wait_for(WAIT, "ready", |e| has_status(e, &run.run_id, "ready"));
    h.manager.prompt(&run.run_id, "hi").unwrap();
    h.sink.wait_for(WAIT, "turn complete", |e| {
        has_status(e, &run.run_id, "turnComplete")
    });

    assert_eq!(agent_text(&h.sink.events(), &run.run_id), "still here");
    let diagnostics = h.manager.diagnostics(&run.run_id).unwrap();
    assert_eq!(
        diagnostics.unmatched_replies, 2,
        "stale replies are counted, never applied"
    );
    h.manager.stop(&run.run_id).unwrap();
}

#[test]
fn malformed_oversized_and_fragmented_lines_do_not_break_the_session() {
    let h = harness();
    let peer = fixture_peer(
        &h.dir,
        "hostile",
        r#"
sid = "fixture-session"
handshake(sid)
msg = recv()
send_raw("this is not json at all\n")
send_raw('{"hello":"world"}' + "\n")
send_raw("x" * (5 * 1024 * 1024) + "\n")
# A frame delivered in pieces, with the newline arriving last.
send_raw('{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"' + sid + '",')
time.sleep(0.15)
send_raw('"update":{"sessionUpdate":"agent_message_chunk","content":')
time.sleep(0.15)
send_raw('{"type":"text","text":"reassembled"}}}}' + "\n")
reply(msg, {"stopReason": "end_turn"})
time.sleep(30)
"#,
    );
    let run = h.manager.start(h.spec(&peer, &h.dir)).unwrap();
    h.sink
        .wait_for(WAIT, "ready", |e| has_status(e, &run.run_id, "ready"));
    h.manager.prompt(&run.run_id, "go").unwrap();
    h.sink.wait_for(WAIT, "turn complete", |e| {
        has_status(e, &run.run_id, "turnComplete")
    });

    assert_eq!(
        agent_text(&h.sink.events(), &run.run_id),
        "reassembled",
        "a frame split across writes is reassembled, not lost"
    );
    let diagnostics = h.manager.diagnostics(&run.run_id).unwrap();
    assert!(diagnostics.protocol_faults >= 3, "got {diagnostics:?}");
    assert_eq!(
        h.manager.info(&run.run_id).unwrap().status,
        AgentStatus::TurnComplete
    );
    h.manager.stop(&run.run_id).unwrap();
}

/// Asks permission, then reports back what the client answered.
const PERMISSION_PEER: &str = r#"
sid = "fixture-session"
handshake(sid)
msg = recv()
send({"jsonrpc": "2.0", "id": 500, "method": "session/request_permission",
      "params": {"sessionId": sid,
                 "toolCall": {"toolCallId": "perm-check-1",
                              "title": "Run a command: rm -rf build",
                              "kind": "execute",
                              "content": [{"type": "content",
                                           "content": {"type": "text", "text": "$ rm -rf build"}}]},
                 "options": [
                     {"optionId": "allow_once", "name": "Allow once", "kind": "allow_once"},
                     {"optionId": "allow_always", "name": "Allow always", "kind": "allow_always"},
                     {"optionId": "deny", "name": "Deny", "kind": "reject_once"}]}})
answer = recv()
note("permission_answer", answer)
reply(msg, {"stopReason": "end_turn"})
time.sleep(30)
"#;

fn pending_prompt(events: &[AgentEvent], run_id: &str) -> Option<(String, Vec<String>)> {
    events.iter().rev().find_map(|e| match e {
        AgentEvent::Permission {
            run_id: id,
            request,
            ..
        } if id == run_id => Some((
            request.request_id.clone(),
            request
                .options
                .iter()
                .map(|o| o.option_id.clone())
                .collect(),
        )),
        _ => None,
    })
}

#[test]
fn a_permission_request_is_surfaced_verbatim_and_only_an_offered_option_can_be_chosen() {
    let h = harness();
    let peer = fixture_peer(&h.dir, "permission", PERMISSION_PEER);
    let run = h.manager.start(h.spec(&peer, &h.dir)).unwrap();
    h.sink
        .wait_for(WAIT, "ready", |e| has_status(e, &run.run_id, "ready"));
    h.manager.prompt(&run.run_id, "clean the build").unwrap();

    let events = h.sink.wait_for(WAIT, "permission request", |e| {
        pending_prompt(e, &run.run_id).is_some()
    });
    assert!(has_status(&events, &run.run_id, "awaitingPermission"));
    let (request_id, options) = pending_prompt(&events, &run.run_id).unwrap();
    assert_eq!(options, vec!["allow_once", "allow_always", "deny"]);

    let snapshot = h.manager.snapshot(&run.run_id).unwrap();
    let pending = snapshot.pending_permission.expect("pending permission");
    assert_eq!(pending.title, "Run a command: rm -rf build");
    assert!(pending.detail.contains("rm -rf build"));

    // An option the agent never offered cannot be sent, whatever the UI asks.
    let err = h
        .manager
        .answer_permission(&run.run_id, &request_id, Some("allow_everything_forever"))
        .expect_err("an unoffered option must be refused");
    assert_eq!(err.code(), "VALIDATION");

    h.manager
        .answer_permission(&run.run_id, &request_id, Some("allow_once"))
        .unwrap();

    let answer = read_note(&h.notes(), "permission_answer", WAIT);
    let answer: serde_json::Value = serde_json::from_str(&answer).unwrap();
    assert_eq!(answer["id"], 500, "answered on the agent's own request id");
    assert_eq!(
        answer["result"]["outcome"],
        serde_json::json!({"outcome": "selected", "optionId": "allow_once"})
    );

    // Answering twice is refused: the request is no longer pending.
    let err = h
        .manager
        .answer_permission(&run.run_id, &request_id, Some("allow_once"))
        .expect_err("a stale answer must be refused");
    assert_eq!(err.code(), "CONFLICT");

    h.sink.wait_for(WAIT, "turn complete", |e| {
        has_status(e, &run.run_id, "turnComplete")
    });
    h.manager.stop(&run.run_id).unwrap();
}

#[test]
fn a_stale_allow_after_shutdown_began_is_cancelled_on_wire_and_in_audit() {
    let h = harness();
    let peer = fixture_peer(&h.dir, "permission_shutdown", PERMISSION_PEER);
    let run = h.manager.start(h.spec(&peer, &h.dir)).unwrap();
    h.sink
        .wait_for(WAIT, "ready", |e| has_status(e, &run.run_id, "ready"));
    h.manager.prompt(&run.run_id, "clean the build").unwrap();
    let events = h.sink.wait_for(WAIT, "permission request", |e| {
        pending_prompt(e, &run.run_id).is_some()
    });
    let (request_id, _) = pending_prompt(&events, &run.run_id).unwrap();

    // `stop` claims terminal ownership and sets `closing` before it can take
    // the pending permission. The queued UI approval below is therefore stale,
    // not an attempt to race a live interaction.
    h.manager.stop(&run.run_id).unwrap();
    let err = h
        .manager
        .answer_permission(&run.run_id, &request_id, Some("allow_once"))
        .expect_err("a permission cannot be selected after shutdown begins");
    assert_eq!(err.code(), "CONFLICT");

    let answer: serde_json::Value =
        serde_json::from_str(&read_note(&h.notes(), "permission_answer", WAIT)).unwrap();
    assert_eq!(
        answer["result"]["outcome"],
        serde_json::json!({"outcome": "cancelled"})
    );
    assert_eq!(
        h.store
            .get_permission_request(&request_id)
            .unwrap()
            .resolution
            .as_deref(),
        Some("cancelled"),
        "a stale allow is never durable as selected"
    );
}

#[test]
fn an_inbound_permission_paused_after_liveness_check_is_cancelled_if_shutdown_claims_first() {
    let h = harness();
    let peer = fixture_peer(&h.dir, "permission_inbound_shutdown", PERMISSION_PEER);
    let mut spec = h.spec(&peer, &h.dir);
    spec.timeouts.shutdown = Duration::from_millis(20);
    let gate = arm_inbound_permission_gate_for_test();
    let run = h.manager.start(spec).unwrap();
    h.sink
        .wait_for(WAIT, "ready", |e| has_status(e, &run.run_id, "ready"));
    h.manager.prompt(&run.run_id, "clean the build").unwrap();

    gate.wait_until_checked(WAIT);
    h.manager.stop(&run.run_id).unwrap();
    gate.release();
    std::thread::sleep(Duration::from_millis(50));

    assert!(
        !h.sink
            .events()
            .iter()
            .any(|e| matches!(e, AgentEvent::Permission { .. })),
        "shutdown's terminal claim must prevent the paused request becoming actionable"
    );
    assert!(h
        .manager
        .snapshot(&run.run_id)
        .unwrap()
        .pending_permission
        .is_none());
    let session = h.store.get_session(&run.session_id).unwrap();
    assert!(h
        .store
        .list_review_items(&session.thread_id)
        .unwrap()
        .iter()
        .any(|item| { item.kind == "agent-permission" && item.summary.contains("not allowed") }));
}

#[test]
fn a_denial_is_sent_as_acps_cancelled_outcome_and_recorded_as_such() {
    let h = harness();
    let peer = fixture_peer(&h.dir, "permission_deny", PERMISSION_PEER);
    let run = h.manager.start(h.spec(&peer, &h.dir)).unwrap();
    h.sink
        .wait_for(WAIT, "ready", |e| has_status(e, &run.run_id, "ready"));
    h.manager.prompt(&run.run_id, "clean the build").unwrap();
    let events = h.sink.wait_for(WAIT, "permission request", |e| {
        pending_prompt(e, &run.run_id).is_some()
    });
    let (request_id, _) = pending_prompt(&events, &run.run_id).unwrap();

    h.manager
        .answer_permission(&run.run_id, &request_id, None)
        .unwrap();

    let answer: serde_json::Value =
        serde_json::from_str(&read_note(&h.notes(), "permission_answer", WAIT)).unwrap();
    assert_eq!(
        answer["result"]["outcome"],
        serde_json::json!({"outcome": "cancelled"})
    );
    assert_eq!(
        h.store
            .get_permission_request(&request_id)
            .unwrap()
            .resolution
            .as_deref(),
        Some("cancelled")
    );
    h.manager.stop(&run.run_id).unwrap();
}

#[test]
fn an_unanswered_permission_request_fails_closed_on_our_own_deadline() {
    let h = harness();
    let peer = fixture_peer(&h.dir, "permission_timeout", PERMISSION_PEER);
    let mut spec = h.spec(&peer, &h.dir);
    // Deliberately far below Hermes's own 60 s auto-deny, so Faiden's answer is
    // the one that lands and the user sees why.
    spec.timeouts.permission = Duration::from_millis(300);
    let run = h.manager.start(spec).unwrap();
    h.sink
        .wait_for(WAIT, "ready", |e| has_status(e, &run.run_id, "ready"));
    h.manager.prompt(&run.run_id, "clean the build").unwrap();

    let answer: serde_json::Value =
        serde_json::from_str(&read_note(&h.notes(), "permission_answer", WAIT)).unwrap();
    assert_eq!(
        answer["result"]["outcome"],
        serde_json::json!({"outcome": "cancelled"}),
        "an unanswered request is never escalated into an allow"
    );

    let events = h.sink.wait_for(WAIT, "resolution", |e| {
        e.iter()
            .any(|x| matches!(x, AgentEvent::PermissionResolved { .. }))
    });
    let resolved = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::PermissionResolved { resolution, .. } => Some(resolution.clone()),
            _ => None,
        })
        .unwrap();
    assert!(resolution_is_closed(&resolved), "got {resolved}");
    h.manager.stop(&run.run_id).unwrap();
}

fn resolution_is_closed(resolution: &str) -> bool {
    resolution.contains("cancelled") && !resolution.contains("selected")
}

#[test]
fn cancelling_denies_a_pending_permission_instead_of_granting_it() {
    let h = harness();
    let peer = fixture_peer(&h.dir, "permission_cancel", PERMISSION_PEER);
    let run = h.manager.start(h.spec(&peer, &h.dir)).unwrap();
    h.sink
        .wait_for(WAIT, "ready", |e| has_status(e, &run.run_id, "ready"));
    h.manager.prompt(&run.run_id, "clean the build").unwrap();
    h.sink.wait_for(WAIT, "permission request", |e| {
        pending_prompt(e, &run.run_id).is_some()
    });

    h.manager.cancel(&run.run_id).unwrap();

    let answer: serde_json::Value =
        serde_json::from_str(&read_note(&h.notes(), "permission_answer", WAIT)).unwrap();
    assert_eq!(
        answer["result"]["outcome"],
        serde_json::json!({"outcome": "cancelled"}),
        "cancelling must never escalate privilege"
    );
    h.manager.stop(&run.run_id).unwrap();
}

#[test]
fn cancel_sends_the_notification_and_does_not_pretend_the_turn_finished() {
    let h = harness();
    let peer = fixture_peer(
        &h.dir,
        "cancellable",
        r#"
sid = "fixture-session"
handshake(sid)
msg = recv()
nxt = recv()
note("cancel_frame", nxt)
time.sleep(0.3)
reply(msg, {"stopReason": "cancelled"})
time.sleep(30)
"#,
    );
    let run = h.manager.start(h.spec(&peer, &h.dir)).unwrap();
    h.sink
        .wait_for(WAIT, "ready", |e| has_status(e, &run.run_id, "ready"));
    h.manager.prompt(&run.run_id, "long job").unwrap();
    h.sink.wait_for(WAIT, "responding", |e| {
        has_status(e, &run.run_id, "responding")
    });

    let after_cancel = h.manager.cancel(&run.run_id).unwrap();
    assert_eq!(
        after_cancel.status,
        AgentStatus::Cancelling,
        "sending a cancel is not the same as the turn being over"
    );

    let frame: serde_json::Value =
        serde_json::from_str(&read_note(&h.notes(), "cancel_frame", WAIT)).unwrap();
    assert_eq!(frame["method"], "session/cancel");
    assert_eq!(frame["params"]["sessionId"], "fixture-session");
    assert!(frame.get("id").is_none(), "cancel is a notification");

    h.sink.wait_for(WAIT, "turn complete", |e| {
        has_status(e, &run.run_id, "turnComplete")
    });
    assert!(h
        .manager
        .info(&run.run_id)
        .unwrap()
        .detail
        .unwrap()
        .contains("cancelled"));
    h.manager.stop(&run.run_id).unwrap();
}

#[test]
fn a_client_request_for_a_capability_we_never_advertised_is_refused() {
    let h = harness();
    let peer = fixture_peer(
        &h.dir,
        "asks_for_fs",
        r#"
sid = "fixture-session"
handshake(sid)
send({"jsonrpc": "2.0", "id": 77, "method": "fs/read_text_file",
      "params": {"sessionId": sid, "path": "/etc/passwd"}})
note("fs_answer", recv())
time.sleep(30)
"#,
    );
    let run = h.manager.start(h.spec(&peer, &h.dir)).unwrap();
    let answer: serde_json::Value =
        serde_json::from_str(&read_note(&h.notes(), "fs_answer", WAIT)).unwrap();
    assert_eq!(answer["id"], 77);
    assert_eq!(answer["error"]["code"], -32601);
    assert!(answer.get("result").is_none());
    h.manager.stop(&run.run_id).unwrap();
}

#[test]
fn only_one_prompt_at_a_time_so_a_slow_turn_cannot_be_double_submitted() {
    let h = harness();
    let peer = fixture_peer(
        &h.dir,
        "slow",
        r#"
sid = "fixture-session"
handshake(sid)
msg = recv()
time.sleep(1.0)
reply(msg, {"stopReason": "end_turn"})
time.sleep(30)
"#,
    );
    let run = h.manager.start(h.spec(&peer, &h.dir)).unwrap();
    h.sink
        .wait_for(WAIT, "ready", |e| has_status(e, &run.run_id, "ready"));

    h.manager.prompt(&run.run_id, "first").unwrap();
    let err = h
        .manager
        .prompt(&run.run_id, "second")
        .expect_err("a second prompt during a turn must be refused");
    assert_eq!(err.code(), "CONFLICT");

    let (messages, _) = h.store.list_agent_messages(&run.run_id, 100).unwrap();
    let user: Vec<&str> = messages
        .iter()
        .filter(|m| m.role == "user")
        .map(|m| m.body.as_str())
        .collect();
    assert_eq!(
        user,
        vec!["first"],
        "the refused prompt was never recorded as sent"
    );

    h.sink.wait_for(WAIT, "turn complete", |e| {
        has_status(e, &run.run_id, "turnComplete")
    });
    // Once the turn is over the next prompt is allowed.
    h.manager.prompt(&run.run_id, "second").unwrap();
    h.manager.stop(&run.run_id).unwrap();
}

#[test]
fn a_prompt_before_the_session_exists_is_refused_rather_than_queued() {
    let h = harness();
    let peer = fixture_peer(&h.dir, "stall2", "time.sleep(30)");
    let run = h.manager.start(h.spec(&peer, &h.dir)).unwrap();
    let err = h
        .manager
        .prompt(&run.run_id, "too early")
        .expect_err("no session yet");
    assert_eq!(err.code(), "CONFLICT");
    assert!(h
        .store
        .list_agent_messages(&run.run_id, 10)
        .unwrap()
        .0
        .is_empty());
    h.manager.stop(&run.run_id).unwrap();
}

#[test]
fn the_child_dying_settles_the_run_as_disconnected_and_fails_the_open_turn() {
    let h = harness();
    let peer = fixture_peer(
        &h.dir,
        "dies",
        r#"
sid = "fixture-session"
handshake(sid)
recv()
sys.exit(3)
"#,
    );
    let run = h.manager.start(h.spec(&peer, &h.dir)).unwrap();
    h.sink
        .wait_for(WAIT, "ready", |e| has_status(e, &run.run_id, "ready"));
    h.manager.prompt(&run.run_id, "go").unwrap();

    h.sink.wait_for(WAIT, "disconnected", |e| {
        has_status(e, &run.run_id, "disconnected")
    });
    let info = h.manager.info(&run.run_id).unwrap();
    assert_eq!(info.status, AgentStatus::Disconnected);
    assert!(!info.prompt_in_flight, "the open turn is not left hanging");

    let stored = h.store.get_agent_run(&run.run_id).unwrap();
    let outcome = stored.outcome.unwrap();
    assert!(outcome.to_lowercase().contains("exited"), "got {outcome}");
    assert!(!outcome.to_lowercase().contains("success"));
}

#[test]
fn stopping_closes_the_owned_child_and_records_why() {
    let h = harness();
    let peer = fixture_peer(&h.dir, "echo_stop", ECHO_PEER);
    let run = h.manager.start(h.spec(&peer, &h.dir)).unwrap();
    h.sink
        .wait_for(WAIT, "ready", |e| has_status(e, &run.run_id, "ready"));
    let pid = h.manager.info(&run.run_id).unwrap().pid.unwrap();

    let stopped = h.manager.stop(&run.run_id).unwrap();
    assert_eq!(stopped.status, AgentStatus::Disconnected);
    assert!(support::wait_until(WAIT, || support::process_gone(pid)));

    let outcome = h.store.get_agent_run(&run.run_id).unwrap().outcome.unwrap();
    assert!(outcome.contains("stopped"), "got {outcome}");
}

#[test]
fn quitting_leaves_no_orphaned_agent_children() {
    let h = harness();
    let peer = fixture_peer(&h.dir, "echo_quit", ECHO_PEER);
    let a = h.manager.start(h.spec(&peer, &h.dir)).unwrap();
    let b = h.manager.start(h.spec(&peer, &h.dir)).unwrap();
    h.sink.wait_for(WAIT, "both ready", |e| {
        has_status(e, &a.run_id, "ready") && has_status(e, &b.run_id, "ready")
    });
    let pids = [
        h.manager.info(&a.run_id).unwrap().pid.unwrap(),
        h.manager.info(&b.run_id).unwrap().pid.unwrap(),
    ];

    h.manager.shutdown_all();

    for pid in pids {
        assert!(
            support::wait_until(WAIT, || support::process_gone(pid)),
            "agent child {pid} survived application quit"
        );
    }
    for run in [&a.run_id, &b.run_id] {
        let outcome = h.store.get_agent_run(run).unwrap().outcome.unwrap();
        assert!(outcome.contains("Faiden quit"), "got {outcome}");
    }
}

#[test]
fn a_snapshot_tells_a_subscriber_exactly_where_to_resume() {
    let h = harness();
    let peer = fixture_peer(&h.dir, "echo_snap", ECHO_PEER);
    let run = h.manager.start(h.spec(&peer, &h.dir)).unwrap();
    h.sink
        .wait_for(WAIT, "ready", |e| has_status(e, &run.run_id, "ready"));
    h.manager.prompt(&run.run_id, "hello").unwrap();
    h.sink.wait_for(WAIT, "turn complete", |e| {
        has_status(e, &run.run_id, "turnComplete")
    });

    let snapshot = h.manager.snapshot(&run.run_id).unwrap();
    let highest = h
        .sink
        .events()
        .iter()
        .filter(|e| e.run_id() == run.run_id)
        .map(|e| e.seq())
        .max()
        .unwrap();
    assert_eq!(
        snapshot.next_seq,
        highest + 1,
        "everything already emitted is inside the snapshot"
    );
    assert_eq!(snapshot.info.epoch, run.epoch);
    h.manager.stop(&run.run_id).unwrap();
}

#[test]
fn a_finished_turn_becomes_an_unseen_result_in_the_review_inbox() {
    let h = harness();
    let peer = fixture_peer(&h.dir, "echo_review", ECHO_PEER);
    let run = h.manager.start(h.spec(&peer, &h.dir)).unwrap();
    h.sink
        .wait_for(WAIT, "ready", |e| has_status(e, &run.run_id, "ready"));
    h.manager.prompt(&run.run_id, "summarise the plan").unwrap();
    h.sink.wait_for(WAIT, "turn complete", |e| {
        has_status(e, &run.run_id, "turnComplete")
    });

    let thread_id = h.store.get_session(&run.session_id).unwrap().thread_id;
    let items = support::wait_for_items(&h.store, &thread_id, 1);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].kind, "agent-turn");
    assert_eq!(
        items[0].session_id.as_deref(),
        Some(run.session_id.as_str())
    );
    assert!(
        items[0].summary.contains("end_turn"),
        "the literal stop reason is what is recorded: {}",
        items[0].summary
    );
    assert!(
        !items[0].summary.to_lowercase().contains("success"),
        "a result is something to read, not a verdict: {}",
        items[0].summary
    );
    assert_eq!(items[0].seen_at, None, "a new result is unseen");
    assert_eq!(items[0].reviewed_at, None, "and certainly not reviewed");

    h.manager.stop(&run.run_id).unwrap();
}

#[test]
fn a_permission_that_faiden_had_to_deny_is_also_recorded_as_something_to_read() {
    let h = harness();
    let peer = fixture_peer(&h.dir, "permission_review", PERMISSION_PEER);
    let mut spec = h.spec(&peer, &h.dir);
    spec.timeouts.permission = Duration::from_millis(200);
    let run = h.manager.start(spec).unwrap();
    h.sink
        .wait_for(WAIT, "ready", |e| has_status(e, &run.run_id, "ready"));
    let thread_id = h.store.get_session(&run.session_id).unwrap().thread_id;
    h.manager.prompt(&run.run_id, "clean the build").unwrap();

    let items = support::wait_for_items(&h.store, &thread_id, 1);
    let denial = items
        .iter()
        .find(|i| i.kind == "agent-permission")
        .expect("the denial is in the inbox");
    assert!(
        denial.summary.contains("not allowed"),
        "got {}",
        denial.summary
    );
    assert_eq!(denial.seen_at, None);

    h.manager.stop(&run.run_id).unwrap();
}

// --- parent-reproduced REDs and review findings ------------------------------

#[test]
fn two_concurrent_starts_for_one_session_leave_exactly_one_live_agent() {
    // Reproduces the parent's `race_probe`: two barrier-synchronised starts for
    // one persisted session. Both used to win, so two children ran under one
    // owning session and either could end it underneath the other.
    use std::sync::Barrier;

    let h = harness();
    let peer = fixture_peer(&h.dir, "echo_race", ECHO_PEER);
    let session = h.session();

    let manager = h.manager.clone();
    let barrier = Arc::new(Barrier::new(2));
    let mut jobs = Vec::new();
    for _ in 0..2 {
        let manager = manager.clone();
        let barrier = barrier.clone();
        let mut spec = h.spec_for(&session, &peer, &h.dir);
        spec.timeouts.shutdown = Duration::from_secs(2);
        jobs.push(std::thread::spawn(move || {
            barrier.wait();
            manager.start(spec)
        }));
    }
    let results: Vec<_> = jobs.into_iter().map(|j| j.join().unwrap()).collect();

    let winners: Vec<_> = results.iter().filter_map(|r| r.as_ref().ok()).collect();
    assert_eq!(winners.len(), 1, "exactly one start may own a session");
    let loser = results
        .iter()
        .find_map(|r| r.as_ref().err())
        .expect("the other start is refused");
    assert_eq!(loser.code(), "CONFLICT");

    // The loser left nothing behind: one live run, one durable open row.
    assert_eq!(
        manager
            .list()
            .iter()
            .filter(|r| r.ended_at.is_none())
            .count(),
        1
    );
    let open = h
        .store
        .list_agent_runs(&session)
        .unwrap()
        .into_iter()
        .filter(|r| r.ended_at.is_none())
        .count();
    assert_eq!(open, 1, "only one durable open run exists");

    manager.shutdown_all();
}

/// Sends a permission request carrying a session id this run does not own,
/// while a real prompt is in flight, then reports what the client answered.
const FORGED_SESSION_PEER: &str = r#"
sid = "fixture-session"
handshake(sid)
msg = recv()
send({"jsonrpc": "2.0", "id": 900, "method": "session/request_permission",
      "params": {"sessionId": "FORGED-OTHER-ACP-SESSION",
                 "toolCall": {"toolCallId": "forged-tool",
                              "title": "Forged session permission",
                              "content": [{"type": "content",
                                           "content": {"type": "text",
                                                       "text": "must never reach the user"}}]},
                 "options": [{"optionId": "allow_once", "name": "Allow once",
                              "kind": "allow_once"}]}})
note("forged_answer", recv())
reply(msg, {"stopReason": "end_turn"})
time.sleep(30)
"#;

#[test]
fn a_permission_for_a_session_this_run_does_not_own_never_reaches_the_user() {
    // Reproduces the parent's `permission_probe`: a request naming another ACP
    // session used to be surfaced as a real approval prompt.
    let h = harness();
    let peer = fixture_peer(&h.dir, "forged_session", FORGED_SESSION_PEER);
    let run = h.manager.start(h.spec(&peer, &h.dir)).unwrap();
    h.sink
        .wait_for(WAIT, "ready", |e| has_status(e, &run.run_id, "ready"));
    h.manager.prompt(&run.run_id, "do something").unwrap();

    let answer: serde_json::Value =
        serde_json::from_str(&read_note(&h.notes(), "forged_answer", WAIT)).unwrap();
    assert_eq!(answer["id"], 900);
    assert_eq!(
        answer["result"]["outcome"],
        serde_json::json!({"outcome": "cancelled"}),
        "a request for a session we do not own is refused immediately"
    );

    assert!(
        !h.sink
            .events()
            .iter()
            .any(|e| matches!(e, AgentEvent::Permission { .. })),
        "no approval prompt may be shown for a session this run does not own"
    );
    assert!(h
        .manager
        .snapshot(&run.run_id)
        .unwrap()
        .pending_permission
        .is_none());
    assert!(h.manager.diagnostics(&run.run_id).unwrap().refused_requests >= 1);

    h.manager.stop(&run.run_id).unwrap();
}

/// Asks for permission before any prompt is in flight.
const UNPROMPTED_PERMISSION_PEER: &str = r#"
sid = "fixture-session"
handshake(sid)
send({"jsonrpc": "2.0", "id": 700, "method": "session/request_permission",
      "params": {"sessionId": sid,
                 "toolCall": {"toolCallId": "unprompted", "title": "Unprompted"},
                 "options": [{"optionId": "allow_once", "name": "Allow once",
                              "kind": "allow_once"}]}})
note("unprompted_answer", recv())
time.sleep(30)
"#;

#[test]
fn a_permission_with_no_turn_in_flight_is_refused_rather_than_shown() {
    let h = harness();
    let peer = fixture_peer(&h.dir, "unprompted", UNPROMPTED_PERMISSION_PEER);
    let run = h.manager.start(h.spec(&peer, &h.dir)).unwrap();

    let answer: serde_json::Value =
        serde_json::from_str(&read_note(&h.notes(), "unprompted_answer", WAIT)).unwrap();
    assert_eq!(
        answer["result"]["outcome"],
        serde_json::json!({"outcome": "cancelled"}),
        "nothing the user asked for is running, so nothing may be approved"
    );
    assert!(!h
        .sink
        .events()
        .iter()
        .any(|e| matches!(e, AgentEvent::Permission { .. })));

    h.manager.stop(&run.run_id).unwrap();
}

#[test]
fn a_permission_that_arrives_after_the_run_settled_is_never_surfaced() {
    // A frame already buffered in the pipe when `stop` runs must not become a
    // live approval prompt for a session that no longer exists.
    let h = harness();
    let peer = fixture_peer(
        &h.dir,
        "late_permission",
        r#"
sid = "fixture-session"
handshake(sid)
msg = recv()
note("prompted", "yes")
time.sleep(0.6)
send({"jsonrpc": "2.0", "id": 800, "method": "session/request_permission",
      "params": {"sessionId": sid,
                 "toolCall": {"toolCallId": "late", "title": "Late request"},
                 "options": [{"optionId": "allow_once", "name": "Allow once",
                              "kind": "allow_once"}]}})
time.sleep(30)
"#,
    );
    let run = h.manager.start(h.spec(&peer, &h.dir)).unwrap();
    h.sink
        .wait_for(WAIT, "ready", |e| has_status(e, &run.run_id, "ready"));
    h.manager.prompt(&run.run_id, "go").unwrap();
    read_note(&h.notes(), "prompted", WAIT);

    h.manager.stop(&run.run_id).unwrap();
    std::thread::sleep(Duration::from_millis(700));

    assert!(
        !h.sink
            .events()
            .iter()
            .any(|e| matches!(e, AgentEvent::Permission { .. })),
        "a settled run cannot ask the user for anything"
    );
    let (_messages, _) = h.store.list_agent_messages(&run.run_id, 100).unwrap();
    let err = h
        .manager
        .answer_permission(&run.run_id, "perm_anything", Some("allow_once"))
        .expect_err("there is nothing to answer");
    assert_eq!(err.code(), "CONFLICT");
}

#[test]
fn an_allow_is_never_sent_unless_its_audit_row_was_committed() {
    // The durable record is what makes an approval accountable. If it cannot be
    // written, the agent must not be told the user allowed anything.
    let h = harness();
    let peer = fixture_peer(&h.dir, "permission_audit", PERMISSION_PEER);
    let run = h.manager.start(h.spec(&peer, &h.dir)).unwrap();
    h.sink
        .wait_for(WAIT, "ready", |e| has_status(e, &run.run_id, "ready"));
    h.manager.prompt(&run.run_id, "clean the build").unwrap();
    let events = h.sink.wait_for(WAIT, "permission request", |e| {
        e.iter().any(|x| matches!(x, AgentEvent::Permission { .. }))
    });
    let request_id = events
        .iter()
        .rev()
        .find_map(|e| match e {
            AgentEvent::Permission { request, .. } => Some(request.request_id.clone()),
            _ => None,
        })
        .unwrap();

    // Make the audit write fail for real, with no production seam: a second
    // connection removes the row the resolution must update, so
    // `resolve_permission_request` reports NOT_FOUND.
    rusqlite::Connection::open(h.dir.join("faiden.sqlite3"))
        .unwrap()
        .execute(
            "DELETE FROM agent_permission_requests WHERE id = ?1",
            rusqlite::params![request_id],
        )
        .unwrap();

    let err = h
        .manager
        .answer_permission(&run.run_id, &request_id, Some("allow_once"))
        .expect_err("an unrecordable allow must not be sent");
    assert_eq!(err.code(), "CONFLICT");

    let answer: serde_json::Value =
        serde_json::from_str(&read_note(&h.notes(), "permission_answer", WAIT)).unwrap();
    assert_eq!(
        answer["result"]["outcome"],
        serde_json::json!({"outcome": "cancelled"}),
        "the agent is told 'not allowed', never 'allowed', when the audit failed"
    );

    h.manager.stop(&run.run_id).unwrap();
}

#[test]
fn a_chunk_that_arrives_after_the_turn_ended_is_not_attributed_to_it() {
    let h = harness();
    let peer = fixture_peer(
        &h.dir,
        "late_chunk",
        r#"
sid = "fixture-session"
handshake(sid)
msg = recv()
text_chunk(sid, "during the turn")
reply(msg, {"stopReason": "end_turn"})
time.sleep(0.4)
text_chunk(sid, " AFTER THE TURN")
time.sleep(30)
"#,
    );
    let run = h.manager.start(h.spec(&peer, &h.dir)).unwrap();
    h.sink
        .wait_for(WAIT, "ready", |e| has_status(e, &run.run_id, "ready"));
    h.manager.prompt(&run.run_id, "go").unwrap();
    h.sink.wait_for(WAIT, "turn complete", |e| {
        has_status(e, &run.run_id, "turnComplete")
    });

    support::wait_until(WAIT, || {
        h.manager
            .diagnostics(&run.run_id)
            .map(|d| d.unattributed_updates >= 1)
            .unwrap_or(false)
    });

    let (messages, _) = h.store.list_agent_messages(&run.run_id, 100).unwrap();
    let agent = messages.iter().find(|m| m.role == "agent").unwrap();
    assert_eq!(
        agent.body, "during the turn",
        "a completed turn is not repainted"
    );
    assert_eq!(agent.status.as_deref(), Some("complete"));
    assert_eq!(
        h.manager.info(&run.run_id).unwrap().status,
        AgentStatus::TurnComplete
    );

    h.manager.stop(&run.run_id).unwrap();
}

#[test]
fn an_agent_speaking_another_protocol_version_is_refused_rather_than_used() {
    let h = harness();
    let peer = fixture_peer(
        &h.dir,
        "wrong_version",
        r#"
msg = recv()
reply(msg, {"protocolVersion": 999})
# `fail_startup` closes stdin. Exiting immediately after that close makes reader
# EOF race the startup teardown that already knows why this run must be an error.
assert recv() is None
note("stdin_closed_after_rejection", "yes")
"#,
    );
    let run = h.manager.start(h.spec(&peer, &h.dir)).unwrap();
    h.sink
        .wait_for(WAIT, "error", |e| has_status(e, &run.run_id, "error"));
    assert_eq!(
        read_note(&h.notes(), "stdin_closed_after_rejection", WAIT),
        "yes",
        "the peer really exited on fail_startup's stdin close"
    );

    let info = h.manager.info(&run.run_id).unwrap();
    assert_eq!(info.status, AgentStatus::Error);
    assert_eq!(info.acp_session_id, None);
    assert!(
        info.detail.as_deref().unwrap_or("").contains("999"),
        "the version it offered is named: {:?}",
        info.detail
    );
    assert!(!has_status(&h.sink.events(), &run.run_id, "ready"));
    assert!(h
        .store
        .get_agent_run(&run.run_id)
        .unwrap()
        .ended_at
        .is_some());
}

#[test]
fn a_session_id_that_cannot_be_recorded_does_not_become_a_ready_session() {
    // `session/new` answered, but with an id the durable record refuses. The run
    // must not be usable: after a restart nothing could say what it was.
    let h = harness();
    let peer = fixture_peer(
        &h.dir,
        "blank_session_id",
        r#"
msg = recv()
reply(msg, {"protocolVersion": 1})
msg = recv()
reply(msg, {"sessionId": "   "})
time.sleep(30)
"#,
    );
    let run = h.manager.start(h.spec(&peer, &h.dir)).unwrap();
    h.sink
        .wait_for(WAIT, "error", |e| has_status(e, &run.run_id, "error"));

    assert!(!has_status(&h.sink.events(), &run.run_id, "ready"));
    assert_eq!(h.manager.info(&run.run_id).unwrap().acp_session_id, None);
    assert_eq!(
        h.store.get_agent_run(&run.run_id).unwrap().acp_session_id,
        None
    );
    let err = h
        .manager
        .prompt(&run.run_id, "anything")
        .expect_err("a run that never got a session cannot be prompted");
    assert_eq!(err.code(), "CONFLICT");
}

// --- the snapshot cut -------------------------------------------------------
//
// A view subscribes, then snapshots, and discards every event numbered below
// the snapshot's `next_seq` as "already included". That contract is only true
// if the cursor a snapshot publishes cannot be ahead of the transcript and the
// pending permission it publishes with it. Both tests below pause a real
// snapshot at that boundary and let a real writer run across it.

/// A bounded scheduling allowance, never the assertion: the tests below assert
/// the invariant for whichever way the race actually went.
fn settle_within(bound: Duration, ready: impl Fn() -> bool) {
    let deadline = Instant::now() + bound;
    while !ready() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Transcript entries the snapshot's cursor claims to cover but did not carry.
/// A subscriber drops those events, so these entries are ones the view can
/// never draw.
fn numbered_but_absent(
    snapshot: &AgentSnapshot,
    events: &[AgentEvent],
    run_id: &str,
) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Message {
                run_id: id,
                seq,
                message,
                ..
            } if id == run_id && *seq < snapshot.next_seq => Some(message.key.clone()),
            _ => None,
        })
        .filter(|key| !snapshot.messages.iter().any(|m| &m.key == key))
        .collect()
}

#[test]
fn a_snapshot_never_publishes_a_cursor_past_a_transcript_entry_it_left_out() {
    let h = harness();
    let peer = fixture_peer(&h.dir, "echo_cut", ECHO_PEER);
    let run = h.manager.start(h.spec(&peer, &h.dir)).unwrap();
    h.sink
        .wait_for(WAIT, "ready", |e| has_status(e, &run.run_id, "ready"));

    let gate = arm_snapshot_cut_gate_for_test(&run.run_id);
    let snapping = {
        let manager = h.manager.clone();
        let run_id = run.run_id.clone();
        std::thread::spawn(move || manager.snapshot(&run_id).unwrap())
    };
    gate.wait_until_reached(WAIT);

    // A prompt records a durable entry and numbers its `Message` event. If the
    // snapshot is mid-cut without owning the run's state, that entry lands
    // after the transcript was read and before the cursor is.
    let prompting = {
        let manager = h.manager.clone();
        let run_id = run.run_id.clone();
        std::thread::spawn(move || manager.prompt(&run_id, "hello"))
    };
    settle_within(Duration::from_secs(1), || prompting.is_finished());
    gate.release();

    let snapshot = snapping.join().unwrap();
    prompting.join().unwrap().unwrap();

    let absent = numbered_but_absent(&snapshot, &h.sink.events(), &run.run_id);
    assert!(
        absent.is_empty(),
        "the snapshot published cursor {} while leaving out {absent:?}; a subscriber \
         discards those events as already included, so those entries are lost",
        snapshot.next_seq
    );
    h.manager.stop(&run.run_id).unwrap();
}

#[test]
fn a_snapshot_that_already_numbered_a_permission_carries_the_prompt_to_answer() {
    let h = harness();
    let peer = fixture_peer(&h.dir, "permission_cut", PERMISSION_PEER);
    let mut spec = h.spec(&peer, &h.dir);
    // The fixture peer sleeps after answering; a short grace keeps the teardown
    // at the end of this test from idling for the default five seconds.
    spec.timeouts.shutdown = Duration::from_millis(20);
    let run = h.manager.start(spec).unwrap();
    h.sink
        .wait_for(WAIT, "ready", |e| has_status(e, &run.run_id, "ready"));

    let gate = arm_snapshot_cut_gate_for_test(&run.run_id);
    let snapping = {
        let manager = h.manager.clone();
        let run_id = run.run_id.clone();
        std::thread::spawn(move || manager.snapshot(&run_id).unwrap())
    };
    gate.wait_until_reached(WAIT);

    // The inbound permission is installed, numbered and shown as
    // `awaitingPermission` while the snapshot sits mid-cut.
    let prompting = {
        let manager = h.manager.clone();
        let run_id = run.run_id.clone();
        std::thread::spawn(move || manager.prompt(&run_id, "clean the build"))
    };
    settle_within(Duration::from_secs(1), || {
        pending_prompt(&h.sink.events(), &run.run_id).is_some()
    });
    gate.release();

    let snapshot = snapping.join().unwrap();
    prompting.join().unwrap().unwrap();

    // Either the request is above the cursor — the subscriber will receive its
    // event — or it is inside the snapshot. Numbered but absent leaves the user
    // told an answer is wanted with nothing to answer.
    let numbered = h.sink.events().iter().find_map(|e| match e {
        AgentEvent::Permission {
            run_id: id,
            seq,
            request,
            ..
        } if id == &run.run_id && *seq < snapshot.next_seq => Some(request.request_id.clone()),
        _ => None,
    });
    if let Some(request_id) = numbered {
        assert_eq!(
            snapshot
                .pending_permission
                .as_ref()
                .map(|p| p.request_id.clone()),
            Some(request_id),
            "a permission numbered below the cursor must be answerable from the snapshot"
        );
    }
    assert_eq!(
        snapshot.info.status == AgentStatus::AwaitingPermission,
        snapshot.pending_permission.is_some(),
        "a snapshot that says an answer is wanted must carry the request to answer"
    );
    h.manager.stop(&run.run_id).unwrap();
}

#[test]
fn a_snapshot_mid_cut_and_a_concurrent_stop_both_complete_and_agree() {
    let h = harness();
    let peer = fixture_peer(&h.dir, "echo_cut_stop", ECHO_PEER);
    let mut spec = h.spec(&peer, &h.dir);
    spec.timeouts.shutdown = Duration::from_millis(20);
    let run = h.manager.start(spec).unwrap();
    h.sink
        .wait_for(WAIT, "ready", |e| has_status(e, &run.run_id, "ready"));

    let gate = arm_snapshot_cut_gate_for_test(&run.run_id);
    let snapping = {
        let manager = h.manager.clone();
        let run_id = run.run_id.clone();
        std::thread::spawn(move || manager.snapshot(&run_id).unwrap())
    };
    gate.wait_until_reached(WAIT);

    // `stop` claims settlement, denies anything pending and settles the run —
    // all under the same state lock the cut holds. The two must serialize, not
    // deadlock, whichever order the scheduler picks.
    let stopping = {
        let manager = h.manager.clone();
        let run_id = run.run_id.clone();
        std::thread::spawn(move || manager.stop(&run_id).unwrap())
    };
    settle_within(Duration::from_secs(1), || stopping.is_finished());
    gate.release();

    settle_within(WAIT, || stopping.is_finished() && snapping.is_finished());
    assert!(
        stopping.is_finished() && snapping.is_finished(),
        "a snapshot mid-cut and a concurrent stop must both make progress"
    );
    let snapshot = snapping.join().unwrap();
    let stopped = stopping.join().unwrap();

    assert!(stopped.ended_at.is_some(), "stop settles the run");
    assert!(
        numbered_but_absent(&snapshot, &h.sink.events(), &run.run_id).is_empty(),
        "the cut must not publish a cursor past entries it left out"
    );
    // The snapshot's status, `ended_at` and cursor come from one observation of
    // the same state, so a run the snapshot still calls live cannot already have
    // been told it ended below that cursor.
    if snapshot.info.ended_at.is_none() {
        let ended = h.sink.events().iter().any(|e| {
            matches!(e, AgentEvent::Ended { run_id: id, seq, .. }
                if id == &run.run_id && *seq < snapshot.next_seq)
        });
        assert!(!ended, "a live snapshot cannot have numbered the run's end");
    }
}

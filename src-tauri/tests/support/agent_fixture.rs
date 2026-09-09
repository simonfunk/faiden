//! A **fixture peer**: a small Python program that speaks the same
//! newline-delimited JSON-RPC over stdio that Hermes's ACP adapter speaks.
//!
//! It is not Hermes and is never presented as evidence about Hermes. It exists
//! so the runtime's framing, correlation, permission and lifecycle behaviour can
//! be exercised deterministically against a **real subprocess and real pipes**,
//! with no provider call, no network and no `~/.hermes` access. Production code
//! cannot reach it: the Tauri command layer resolves the program itself and
//! never accepts one from the webview.

#![allow(dead_code)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use faiden_lib::agent::runtime::{AgentEvent, AgentEvents};

/// Shared by every fixture: the wire helpers, so each scenario stays readable.
const PREAMBLE: &str = r#"#!/usr/bin/env python3
import json, os, sys, time

def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()

def send_raw(text):
    sys.stdout.write(text)
    sys.stdout.flush()

def recv():
    line = sys.stdin.readline()
    if not line:
        return None
    line = line.strip()
    if not line:
        return recv()
    return json.loads(line)

def reply(msg, result):
    send({"jsonrpc": "2.0", "id": msg["id"], "result": result})

def fail(msg, code, message):
    send({"jsonrpc": "2.0", "id": msg["id"], "error": {"code": code, "message": message}})

def update(session_id, upd):
    send({"jsonrpc": "2.0", "method": "session/update",
          "params": {"sessionId": session_id, "update": upd}})

def text_chunk(session_id, text):
    update(session_id, {"sessionUpdate": "agent_message_chunk",
                        "content": {"type": "text", "text": text}})

def note(name, value):
    # Written to a temporary name and renamed, so a reader never observes a
    # half-written (or empty) note.
    target = os.path.join(os.environ["FIXTURE_NOTES"], name)
    # Unique per process: two fixture peers may write the same note at once.
    staging = target + ".%d.partial" % os.getpid()
    with open(staging, "w") as f:
        f.write(value if isinstance(value, str) else json.dumps(value))
    os.replace(staging, target)

def handshake(session_id="fixture-session"):
    """Answer initialize and session/new, returning the session/new message."""
    while True:
        msg = recv()
        if msg is None:
            sys.exit(0)
        if msg.get("method") == "initialize":
            reply(msg, {"protocolVersion": 1,
                        "agentInfo": {"name": "fixture-peer", "version": "0"},
                        "agentCapabilities": {"loadSession": True}})
        elif msg.get("method") == "session/new":
            note("session_new_params", msg["params"])
            reply(msg, {"sessionId": session_id})
            return msg
"#;

/// Writes an executable fixture peer and returns its path. `body` is Python
/// appended to the shared preamble.
pub fn fixture_peer(dir: &Path, name: &str, body: &str) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let path = dir.join(format!("{name}.py"));
    fs::write(&path, format!("{PREAMBLE}\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// Where fixtures drop observations for the test to read back.
pub fn notes_dir(dir: &Path) -> PathBuf {
    let notes = dir.join("notes");
    fs::create_dir_all(&notes).unwrap();
    notes
}

pub fn read_note(notes: &Path, name: &str, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(text) = fs::read_to_string(notes.join(name)) {
            if !text.is_empty() {
                return text;
            }
        }
        if Instant::now() >= deadline {
            panic!("fixture never wrote note {name} within {timeout:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

// --- event collection -------------------------------------------------------

#[derive(Default)]
struct Inner {
    events: Vec<AgentEvent>,
}

pub struct CollectingAgentSink {
    inner: Mutex<Inner>,
    signal: Condvar,
}

impl CollectingAgentSink {
    pub fn new() -> Arc<CollectingAgentSink> {
        Arc::new(CollectingAgentSink {
            inner: Mutex::new(Inner::default()),
            signal: Condvar::new(),
        })
    }

    pub fn events(&self) -> Vec<AgentEvent> {
        self.inner.lock().unwrap().events.clone()
    }

    pub fn wait_for<F>(&self, timeout: Duration, what: &str, pred: F) -> Vec<AgentEvent>
    where
        F: Fn(&[AgentEvent]) -> bool,
    {
        let deadline = Instant::now() + timeout;
        let mut guard = self.inner.lock().unwrap();
        loop {
            if pred(&guard.events) {
                return guard.events.clone();
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                panic!(
                    "timed out after {timeout:?} waiting for {what}; saw: {:#?}",
                    guard.events
                );
            }
            let (g, _) = self.signal.wait_timeout(guard, remaining).unwrap();
            guard = g;
        }
    }
}

impl AgentEvents for CollectingAgentSink {
    fn emit(&self, event: AgentEvent) {
        let mut guard = self.inner.lock().unwrap();
        guard.events.push(event);
        self.signal.notify_all();
    }
}

/// The statuses seen for one run, in order, deduplicated when repeated.
pub fn status_trail(events: &[AgentEvent], run_id: &str) -> Vec<String> {
    let mut trail: Vec<String> = Vec::new();
    for event in events {
        if let AgentEvent::Status {
            run_id: id, status, ..
        } = event
        {
            if id == run_id && trail.last().map(String::as_str) != Some(status.as_str()) {
                trail.push(status.as_str().to_string());
            }
        }
    }
    trail
}

pub fn has_status(events: &[AgentEvent], run_id: &str, status: &str) -> bool {
    status_trail(events, run_id).iter().any(|s| s == status)
}

/// Concatenated agent-role transcript text emitted for one run.
pub fn agent_text(events: &[AgentEvent], run_id: &str) -> String {
    events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Message {
                run_id: id,
                message,
                ..
            } if id == run_id && message.role == "agent" => Some(message.body.clone()),
            _ => None,
        })
        .next_back()
        .unwrap_or_default()
}
